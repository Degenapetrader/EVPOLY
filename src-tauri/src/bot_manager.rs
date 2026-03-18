#![allow(dead_code)]

use crate::config_io;
use crate::log_stream::LogBuffer;
use serde::{Deserialize, Serialize};
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
        if inner.status == BotStatus::Running {
            return Err("bot already running".into());
        }
        inner.status = BotStatus::Starting;

        let mut args = vec![
            "--env-file".to_string(),
            env_path.to_string_lossy().to_string(),
            "--config".to_string(),
            config_path.to_string_lossy().to_string(),
        ];
        if simulation {
            args.push("--simulation".to_string());
        }

        let (mut rx, child) = app_handle
            .shell()
            .sidecar("evpoly-bot")
            .map_err(|e| format!("sidecar init: {e}"))?
            .args(&args)
            .spawn()
            .map_err(|e| format!("spawn: {e}"))?;

        inner.child = Some(child);
        inner.env_path = Some(env_path.clone());
        inner.status = BotStatus::Running;
        drop(inner);

        let log_buf = self.log_buffer.clone();
        let inner_ref = self.inner.clone();

        tauri::async_runtime::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(bytes) => {
                        if let Ok(text) = String::from_utf8(bytes) {
                            if let Ok(mut buf) = log_buf.lock() {
                                for line in text.lines() {
                                    buf.push(line.to_string());
                                }
                            }
                        }
                    }
                    CommandEvent::Stderr(bytes) => {
                        if let Ok(text) = String::from_utf8(bytes) {
                            if let Ok(mut buf) = log_buf.lock() {
                                for line in text.lines() {
                                    buf.push(line.to_string());
                                }
                            }
                        }
                    }
                    CommandEvent::Terminated(_) => {
                        if let Ok(mut inner) = inner_ref.lock() {
                            if let Some(path) = inner.env_path.take() {
                                config_io::cleanup_env_file(&path);
                            }
                            inner.child = None;
                            inner.status = BotStatus::Stopped;
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
            if let Some(path) = inner.env_path.take() {
                config_io::cleanup_env_file(&path);
            }
            inner.status = BotStatus::Stopped;
        } else if let Some(path) = inner.env_path.take() {
            config_io::cleanup_env_file(&path);
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
