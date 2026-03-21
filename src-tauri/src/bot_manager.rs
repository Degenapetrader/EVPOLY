#![allow(dead_code)]

use crate::config_io;
use crate::log_stream::LogBuffer;
use chrono::Utc;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::Write;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

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

#[derive(Clone)]
pub struct BotRequestContext {
    pub base_url: String,
    pub auth_token: Option<String>,
}

struct BotInner {
    status: BotStatus,
    child: Option<CommandChild>,
    env_path: Option<PathBuf>,
    stop_requested: bool,
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
                stop_requested: false,
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
        inner.stop_requested = false;

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
                            let stop_requested = inner.stop_requested;
                            if let Some(path) = inner.env_path.take() {
                                config_io::cleanup_env_file(&path);
                            }
                            inner.child = None;
                            inner.stop_requested = false;
                            if was_stopping || stop_requested || payload.code == Some(0) {
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
        let should_force_cleanup = {
            let mut inner = self.inner.lock().map_err(|e| e.to_string())?;
            inner.stop_requested = true;
            if let Some(child) = inner.child.take() {
                inner.status = BotStatus::Stopping;
                child.kill().map_err(|e| format!("kill: {e}"))?;
                true
            } else {
                inner.status = BotStatus::Stopping;
                true
            }
        };

        if should_force_cleanup {
            self.force_cleanup_orphan_processes();
        }
        self.reconcile_runtime_state();
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
        self.reconcile_runtime_state();
        self.inner
            .lock()
            .map(|inner| inner.status.clone())
            .unwrap_or(BotStatus::Error("lock failed".into()))
    }

    pub fn get_log_buffer(&self) -> Arc<Mutex<LogBuffer>> {
        self.log_buffer.clone()
    }

    pub fn request_context(&self) -> Result<BotRequestContext, String> {
        let inner = self.inner.lock().map_err(|e| e.to_string())?;
        if inner.status != BotStatus::Running {
            return Err("bot is not running".to_string());
        }
        let env_path = inner
            .env_path
            .clone()
            .ok_or("bot env path missing")?;
        drop(inner);

        let env_vars = read_env_file_pairs(&env_path)?;
        let admin_enabled = env_vars
            .get("EVPOLY_ADMIN_API_ENABLE")
            .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        if !admin_enabled {
            return Err("bot admin api is not enabled".to_string());
        }

        let bind = env_vars
            .get("EVPOLY_ADMIN_API_BIND")
            .or_else(|| env_vars.get("EVPOLY_ADMIN_BIND"))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "127.0.0.1:8787".to_string());
        let base_url = if bind.starts_with("http://") || bind.starts_with("https://") {
            bind
        } else {
            format!("http://{bind}")
        };

        Ok(BotRequestContext {
            base_url,
            auth_token: env_vars
                .get("EVPOLY_ADMIN_API_TOKEN")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        })
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

    fn reconcile_runtime_state(&self) {
        #[cfg(target_os = "windows")]
        {
            let current_status = match self.inner.lock() {
                Ok(inner) => inner.status.clone(),
                Err(_) => return,
            };
            if current_status != BotStatus::Stopping {
                return;
            }

            let config_marker = self
                .data_dir
                .join("runtime.config.json")
                .to_string_lossy()
                .to_string();
            let Some(processes_running) = windows_processes_running(
                &["evpoly-bot.exe", "evpoly-bot-real.exe"],
                &config_marker,
            ) else {
                return;
            };

            let mut inner = match self.inner.lock() {
                Ok(inner) => inner,
                Err(_) => return,
            };

            if inner.status != BotStatus::Stopping {
                return;
            }

            if processes_running {
                drop(inner);
                self.force_cleanup_orphan_processes();
                if let Some(false) = windows_processes_running(
                    &["evpoly-bot.exe", "evpoly-bot-real.exe"],
                    &config_marker,
                ) {
                    if let Ok(mut inner) = self.inner.lock() {
                        finalize_stop(&mut inner);
                    }
                }
            } else {
                finalize_stop(&mut inner);
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            let mut inner = match self.inner.lock() {
                Ok(inner) => inner,
                Err(_) => return,
            };
            if inner.status == BotStatus::Stopping && inner.child.is_none() {
                finalize_stop(&mut inner);
            }
        }
    }

    fn force_cleanup_orphan_processes(&self) {
        #[cfg(target_os = "windows")]
        {
            let config_marker = self
                .data_dir
                .join("runtime.config.json")
                .to_string_lossy()
                .to_string();
            if windows_stop_processes(&["evpoly-bot.exe", "evpoly-bot-real.exe"], &config_marker) {
                append_debug_line(
                    &self.data_dir.join("evpoly-debug.log.txt"),
                    "SYSTEM",
                    "forced bot orphan cleanup via process scan",
                );
            }
        }
    }
}

fn finalize_stop(inner: &mut BotInner) {
    if let Some(path) = inner.env_path.take() {
        config_io::cleanup_env_file(&path);
    }
    inner.child = None;
    inner.status = BotStatus::Stopped;
}

#[cfg(target_os = "windows")]
fn escape_powershell_literal(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(target_os = "windows")]
fn hidden_powershell(script: &str) -> Command {
    let mut command = Command::new("powershell");
    command.creation_flags(CREATE_NO_WINDOW);
    command.args(["-NoProfile", "-Command", script]);
    command
}

#[cfg(target_os = "windows")]
fn windows_processes_running(image_names: &[&str], command_marker: &str) -> Option<bool> {
    let names = image_names
        .iter()
        .map(|name| format!("'{}'", escape_powershell_literal(name)))
        .collect::<Vec<_>>()
        .join(", ");
    let marker = escape_powershell_literal(command_marker);
    let script = format!(
        "$names = @({names}); \
         $marker = '{marker}'; \
         $count = @(Get-CimInstance Win32_Process | Where-Object {{ $names -contains $_.Name -and $_.CommandLine -like ('*' + $marker + '*') }}).Count; \
         [Console]::Out.Write($count)"
    );

    hidden_powershell(&script)
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|stdout| stdout.trim().parse::<usize>().ok())
        .map(|count| count > 0)
}

#[cfg(target_os = "windows")]
fn windows_stop_processes(image_names: &[&str], command_marker: &str) -> bool {
    let names = image_names
        .iter()
        .map(|name| format!("'{}'", escape_powershell_literal(name)))
        .collect::<Vec<_>>()
        .join(", ");
    let marker = escape_powershell_literal(command_marker);
    let script = format!(
        "$names = @({names}); \
         $marker = '{marker}'; \
         $procs = Get-CimInstance Win32_Process | Where-Object {{ $names -contains $_.Name -and $_.CommandLine -like ('*' + $marker + '*') }}; \
         if ($procs) {{ $procs | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}; exit 0 }} else {{ exit 1 }}"
    );

    hidden_powershell(&script)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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

pub async fn send_bot_request(
    ctx: BotRequestContext,
    method: String,
    path: String,
    query: Option<Value>,
    body: Option<Value>,
) -> Result<Value, String> {
    let method =
        Method::from_bytes(method.trim().as_bytes()).map_err(|e| format!("invalid method: {e}"))?;
    let clean_path = if path.trim().starts_with('/') {
        path.trim().to_string()
    } else {
        format!("/{}", path.trim())
    };
    let url = format!("{}{}", ctx.base_url, clean_path);

    let client = reqwest::Client::new();
    let mut request = client.request(method.clone(), &url);
    if let Some(token) = ctx.auth_token {
        if !token.trim().is_empty() {
            request = request.header("x-evpoly-admin-token", token);
        }
    }
    if let Some(query_value) = query {
        if let Some(query_obj) = query_value.as_object() {
            let query_pairs = query_obj
                .iter()
                .filter_map(|(k, v)| json_value_to_query_pair(k, v))
                .collect::<Vec<_>>();
            if !query_pairs.is_empty() {
                request = request.query(&query_pairs);
            }
        }
    }
    if method != Method::GET {
        if let Some(body_value) = body {
            request = request.json(&body_value);
        }
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("bot request failed: {e}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| format!("bot response read failed: {e}"))?;

    let payload = if text.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str::<Value>(&text).unwrap_or_else(|_| {
            let mut m = Map::new();
            m.insert("raw".to_string(), Value::String(text));
            Value::Object(m)
        })
    };

    if !status.is_success() {
        return Err(format!(
            "bot api {} {} -> {}: {}",
            method, clean_path, status, payload
        ));
    }
    Ok(payload)
}

fn json_value_to_query_pair(key: &str, value: &Value) -> Option<(String, String)> {
    if value.is_null() {
        return None;
    }
    if let Some(s) = value.as_str() {
        return Some((key.to_string(), s.to_string()));
    }
    if let Some(b) = value.as_bool() {
        return Some((key.to_string(), b.to_string()));
    }
    if let Some(n) = value.as_i64() {
        return Some((key.to_string(), n.to_string()));
    }
    if let Some(n) = value.as_u64() {
        return Some((key.to_string(), n.to_string()));
    }
    if let Some(n) = value.as_f64() {
        return Some((key.to_string(), n.to_string()));
    }
    if let Some(arr) = value.as_array() {
        let joined = arr
            .iter()
            .filter_map(|item| match item {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(",");
        if joined.is_empty() {
            return None;
        }
        return Some((key.to_string(), joined));
    }
    Some((key.to_string(), value.to_string()))
}
