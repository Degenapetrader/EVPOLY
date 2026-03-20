#![allow(dead_code)]

use crate::config_io;
use crate::log_stream::LogBuffer;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum BotStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error(String),
}

#[derive(Serialize, Deserialize)]
struct LastState {
    was_running: bool,
    simulation: bool,
}

struct BotInner {
    status: BotStatus,
    child: Option<CommandChild>,
    env_path: Option<PathBuf>,
}

pub struct BotManager {
    data_dir: PathBuf,
    inner: Arc<Mutex<BotInner>>,
    log_buffer: Arc<Mutex<LogBuffer>>,
}

impl BotManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            inner: Arc::new(Mutex::new(BotInner {
                status: BotStatus::Stopped,
                child: None,
                env_path: None,
            })),
            log_buffer: Arc::new(Mutex::new(LogBuffer::new())),
        }
    }

    pub fn start(
        &self,
        app_handle: &AppHandle,
        env_path: PathBuf,
        config_path: PathBuf,
        simulation: bool,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().map_err(|e| e.to_string())?;
        if inner.status != BotStatus::Stopped && !matches!(inner.status, BotStatus::Error(_)) {
            return Err(format!("bot is busy ({:?})", inner.status));
        }
        inner.status = BotStatus::Starting;

        let mut args = vec![
            "--config".to_string(),
            config_path.to_string_lossy().to_string(),
        ];
        if simulation {
            args.push("--simulation".to_string());
        } else {
            args.push("--no-simulation".to_string());
        }

        let debug_log_path = self.data_dir.join("evpoly-debug.log.txt");
        append_debug_session_start(&debug_log_path, simulation, &args);

        let env_vars = read_env_file_pairs(&env_path)?;

        let (mut rx, child) = app_handle
            .shell()
            .sidecar("evpoly-bot")
            .map_err(|e| format!("sidecar init: {e}"))?
            .args(&args)
            .envs(env_vars)
            .current_dir(&self.data_dir)
            .spawn()
            .map_err(|e| format!("spawn: {e}"))?;

        inner.child = Some(child);
        inner.env_path = Some(env_path.clone());
        drop(inner);

        let log_buf = self.log_buffer.clone();
        let inner_ref = self.inner.clone();
        let debug_log = debug_log_path.clone();

        tauri::async_runtime::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(bytes) => {
                        if let Ok(text) = String::from_utf8(bytes) {
                            if let Ok(mut buf) = log_buf.lock() {
                                for line in text.lines() {
                                    buf.push(line.to_string());
                                    append_debug_line(&debug_log, "STDOUT", line);
                                }
                            }
                            if let Ok(mut inner) = inner_ref.lock() {
                                if inner.status == BotStatus::Starting {
                                    inner.status = BotStatus::Running;
                                }
                            }
                        }
                    }
                    CommandEvent::Stderr(bytes) => {
                        if let Ok(text) = String::from_utf8(bytes) {
                            if let Ok(mut buf) = log_buf.lock() {
                                for line in text.lines() {
                                    buf.push(line.to_string());
                                    append_debug_line(&debug_log, "STDERR", line);
                                }
                            }
                            if let Ok(mut inner) = inner_ref.lock() {
                                if inner.status == BotStatus::Starting {
                                    inner.status = BotStatus::Running;
                                }
                            }
                        }
                    }
                    CommandEvent::Error(err) => {
                        let line = format!("Bot process event error: {err}");
                        if let Ok(mut buf) = log_buf.lock() {
                            buf.push(line.clone());
                        }
                        append_debug_line(&debug_log, "SYSTEM", &line);
                        if let Ok(mut inner) = inner_ref.lock() {
                            inner.status = BotStatus::Error(line);
                        }
                    }
                    CommandEvent::Terminated(payload) => {
                        let exit_line = format!(
                            "Bot process terminated (code={:?}, signal={:?})",
                            payload.code, payload.signal
                        );
                        if let Ok(mut buf) = log_buf.lock() {
                            buf.push(exit_line.clone());
                        }
                        append_debug_line(&debug_log, "SYSTEM", &exit_line);
                        if let Ok(mut inner) = inner_ref.lock() {
                            let was_stopping = inner.status == BotStatus::Stopping;
                            if let Some(path) = inner.env_path.take() {
                                config_io::cleanup_env_file(&path);
                            }
                            inner.child = None;
                            if was_stopping || payload.code == Some(0) {
                                inner.status = BotStatus::Stopped;
                            } else {
                                inner.status = BotStatus::Error(exit_line);
                            }
                        }
                    }
                    _ => {}
                }
            }
        });

        self.save_last_state(true, simulation);
        Ok(())
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut inner = self.inner.lock().map_err(|e| e.to_string())?;
        if let Some(child) = inner.child.take() {
            inner.status = BotStatus::Stopping;
            child.kill().map_err(|e| format!("kill: {e}"))?;
        } else if let Some(path) = inner.env_path.take() {
            config_io::cleanup_env_file(&path);
            inner.status = BotStatus::Stopped;
        }
        drop(inner);
        self.save_last_state(false, false);
        Ok(())
    }

    pub fn restart(
        &self,
        app_handle: &AppHandle,
        env_path: PathBuf,
        config_path: PathBuf,
        simulation: bool,
    ) -> Result<(), String> {
        self.stop()?;
        self.start(app_handle, env_path, config_path, simulation)
    }

    pub fn is_running(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.status == BotStatus::Running)
            .unwrap_or(false)
    }

    pub fn get_status(&self) -> BotStatus {
        self.inner
            .lock()
            .map(|inner| inner.status.clone())
            .unwrap_or(BotStatus::Error("lock failed".into()))
    }

    pub fn get_log_buffer(&self) -> Arc<Mutex<LogBuffer>> {
        self.log_buffer.clone()
    }

    fn save_last_state(&self, was_running: bool, simulation: bool) {
        let state = LastState {
            was_running,
            simulation,
        };
        let path = self.data_dir.join("last_state.json");
        let _ = std::fs::write(path, serde_json::to_string(&state).unwrap_or_default());
    }

    pub fn load_last_state(&self) -> Option<(bool, bool)> {
        let path = self.data_dir.join("last_state.json");
        let json = std::fs::read_to_string(path).ok()?;
        let state: LastState = serde_json::from_str(&json).ok()?;
        Some((state.was_running, state.simulation))
    }
}

fn read_env_file_pairs(path: &PathBuf) -> Result<HashMap<String, String>, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read env file: {e}"))?;
    let mut vars = HashMap::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            let k = key.trim();
            if k.is_empty() {
                continue;
            }
            vars.insert(k.to_string(), value.trim().to_string());
        }
    }

    Ok(vars)
}

fn append_debug_session_start(path: &PathBuf, simulation: bool, args: &[String]) {
    let mode = if simulation { "simulation" } else { "live" };
    let line = format!(
        "==================== session start {} mode={} args={} ====================",
        Utc::now().to_rfc3339(),
        mode,
        args.join(" ")
    );
    append_debug_line(path, "SYSTEM", &line);
}

fn append_debug_line(path: &PathBuf, source: &str, line: &str) {
    let ts = Utc::now().to_rfc3339();
    let content = format!("[{ts}] [{source}] {line}\n");
    write_debug_line(path, &content);
    if let Some(parent) = path.parent() {
        let full_log = parent.join("evpoly-full-debug.log.txt");
        write_debug_line(&full_log, &content);
    }
}

fn write_debug_line(path: &PathBuf, content: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        let _ = file.write_all(content.as_bytes());
    }
}
