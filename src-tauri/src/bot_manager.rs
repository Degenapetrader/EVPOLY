#![allow(dead_code)]

use crate::config_io;
use crate::log_stream::{append_file_log_line, LogBuffer};
use chrono::Utc;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
#[cfg(target_os = "windows")]
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(not(target_os = "windows"))]
use sysinfo::System;
use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(target_os = "windows")]
const BOT_PROCESS_NAMES: [&str; 2] = ["evpoly-bot.exe", "evpoly-bot-real.exe"];
#[cfg(not(target_os = "windows"))]
const BOT_PROCESS_NAMES: [&str; 5] = [
    "evpoly-bot",
    "evpoly-bot-real",
    "polymarket-arbitrage-bot",
    "evpoly-bot-x86_64-unknown-linux-gnu",
    "evpoly-bot-aarch64-unknown-linux-gnu",
];

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
    simulation: bool,
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
                simulation: false,
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
        ensure_bot_runtime_dirs(&self.data_dir)?;

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
        inner.simulation = simulation;
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
                            inner.simulation = false;
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
        let cancel_context = {
            let inner = self.inner.lock().map_err(|e| e.to_string())?;
            if inner.status == BotStatus::Running && !inner.simulation {
                inner.env_path.clone()
            } else {
                None
            }
        };

        if let Some(env_path) = cancel_context.as_ref() {
            self.try_cancel_all_before_stop(env_path);
        }

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

    pub fn simulation_mode(&self) -> Option<bool> {
        self.inner.lock().ok().and_then(|inner| match inner.status {
            BotStatus::Stopped => None,
            _ => Some(inner.simulation),
        })
    }

    pub fn last_activity_at(&self) -> Option<String> {
        self.log_buffer
            .lock()
            .ok()
            .and_then(|buffer| buffer.latest_line())
            .map(|line| line.timestamp)
    }

    pub fn request_context(&self) -> Result<BotRequestContext, String> {
        let inner = self.inner.lock().map_err(|e| e.to_string())?;
        if inner.status != BotStatus::Running {
            return Err("bot is not running".to_string());
        }
        let env_path = inner.env_path.clone().ok_or("bot env path missing")?;
        drop(inner);

        let env_vars = read_env_file_pairs(&env_path)?;
        let admin_enabled = env_vars
            .get("EVPOLY_ADMIN_API_ENABLE")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
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
        let config_marker = runtime_config_marker(&self.data_dir);

        #[cfg(target_os = "windows")]
        {
            let current_status = match self.inner.lock() {
                Ok(inner) => inner.status.clone(),
                Err(_) => return,
            };
            if current_status != BotStatus::Stopping {
                return;
            }

            let Some(processes_running) =
                windows_processes_running(&BOT_PROCESS_NAMES, &config_marker)
            else {
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
                if let Some(false) = windows_processes_running(&BOT_PROCESS_NAMES, &config_marker) {
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
            let current_status = match self.inner.lock() {
                Ok(inner) => inner.status.clone(),
                Err(_) => return,
            };
            if current_status != BotStatus::Stopping {
                return;
            }

            let Some(processes_running) =
                unix_processes_running(&BOT_PROCESS_NAMES, &config_marker)
            else {
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
                if let Some(false) = unix_processes_running(&BOT_PROCESS_NAMES, &config_marker) {
                    if let Ok(mut inner) = self.inner.lock() {
                        finalize_stop(&mut inner);
                    }
                }
            } else {
                finalize_stop(&mut inner);
            }
        }
    }

    fn force_cleanup_orphan_processes(&self) {
        #[cfg(target_os = "windows")]
        {
            let config_marker = runtime_config_marker(&self.data_dir);
            if windows_stop_processes(&BOT_PROCESS_NAMES, &config_marker) {
                append_debug_line(
                    &self.data_dir.join("evpoly-debug.log.txt"),
                    "SYSTEM",
                    "forced bot orphan cleanup via process scan",
                );
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            let config_marker = runtime_config_marker(&self.data_dir);
            if unix_stop_processes(&BOT_PROCESS_NAMES, &config_marker) {
                append_debug_line(
                    &self.data_dir.join("evpoly-debug.log.txt"),
                    "SYSTEM",
                    "forced bot orphan cleanup via process scan",
                );
            }
        }
    }

    fn try_cancel_all_before_stop(&self, env_path: &PathBuf) {
        let debug_log = self.data_dir.join("evpoly-debug.log.txt");
        match shutdown_request_context(env_path) {
            Ok(Some(ctx)) => {
                append_debug_line(
                    &debug_log,
                    "SYSTEM",
                    "stop requested: attempting admin cancel-all before process shutdown",
                );
                match send_shutdown_cancel_all(ctx) {
                    Ok(summary) => append_debug_line(&debug_log, "SYSTEM", &summary),
                    Err(err) => append_debug_line(
                        &debug_log,
                        "SYSTEM",
                        format!("stop requested: admin cancel-all failed before shutdown: {err}")
                            .as_str(),
                    ),
                }
            }
            Ok(None) => append_debug_line(
                &debug_log,
                "SYSTEM",
                "stop requested: admin cancel-all skipped before shutdown (admin api unavailable)",
            ),
            Err(err) => append_debug_line(
                &debug_log,
                "SYSTEM",
                format!("stop requested: unable to prepare admin cancel-all: {err}").as_str(),
            ),
        }
    }
}

fn finalize_stop(inner: &mut BotInner) {
    if let Some(path) = inner.env_path.take() {
        config_io::cleanup_env_file(&path);
    }
    inner.child = None;
    inner.simulation = false;
    inner.status = BotStatus::Stopped;
}

fn ensure_bot_runtime_dirs(data_dir: &PathBuf) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("prepare bot data directory: {e}"))?;
    std::fs::create_dir_all(data_dir.join("history"))
        .map_err(|e| format!("prepare bot history directory: {e}"))?;
    Ok(())
}

fn runtime_config_marker(data_dir: &PathBuf) -> String {
    data_dir
        .join("runtime.config.json")
        .to_string_lossy()
        .to_string()
}

fn shutdown_request_context(env_path: &PathBuf) -> Result<Option<BotRequestContext>, String> {
    let env_vars = read_env_file_pairs(env_path)?;
    let admin_enabled = env_vars
        .get("EVPOLY_ADMIN_API_ENABLE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    if !admin_enabled {
        return Ok(None);
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

    Ok(Some(BotRequestContext {
        base_url,
        auth_token: env_vars
            .get("EVPOLY_ADMIN_API_TOKEN")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
    }))
}

fn send_shutdown_cancel_all(ctx: BotRequestContext) -> Result<String, String> {
    let url = format!(
        "{}/admin/orders/cancel-all",
        ctx.base_url.trim_end_matches('/')
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| format!("build shutdown client: {e}"))?;
    let mut request = client.post(&url);
    if let Some(token) = ctx.auth_token {
        if !token.trim().is_empty() {
            request = request.header("x-evpoly-admin-token", token);
        }
    }

    let response = request
        .send()
        .map_err(|e| format!("shutdown cancel-all request failed: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|e| format!("shutdown cancel-all response read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("shutdown cancel-all returned {status}: {body}"));
    }

    let payload = serde_json::from_str::<Value>(&body).unwrap_or(Value::String(body));
    let canceled = payload
        .get("canceled_orders")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let not_canceled = payload
        .get("not_canceled_orders")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(format!(
        "stop requested: admin cancel-all finished before shutdown (canceled_orders={canceled}, not_canceled_orders={not_canceled})"
    ))
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

fn process_name_matches(process_name: &str, image_names: &[&str]) -> bool {
    let normalized = process_name.trim().to_ascii_lowercase();
    image_names.iter().any(|name| {
        let image = name.trim().to_ascii_lowercase();
        normalized == image || normalized.starts_with(&format!("{image}-"))
    })
}

#[cfg(not(target_os = "windows"))]
fn unix_process_matches(
    process_name: &str,
    command_line: &[String],
    image_names: &[&str],
    command_marker: &str,
) -> bool {
    process_name_matches(process_name, image_names)
        && command_line
            .iter()
            .any(|arg| arg.to_string_lossy().contains(command_marker))
}

#[cfg(not(target_os = "windows"))]
fn unix_matching_bot_pids(
    system: &System,
    image_names: &[&str],
    command_marker: &str,
) -> Vec<sysinfo::Pid> {
    system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            let process_name = process.name().to_string();
            unix_process_matches(&process_name, process.cmd(), image_names, command_marker)
                .then_some(*pid)
        })
        .collect()
}

#[cfg(not(target_os = "windows"))]
fn unix_processes_running(image_names: &[&str], command_marker: &str) -> Option<bool> {
    let system = System::new_all();
    Some(!unix_matching_bot_pids(&system, image_names, command_marker).is_empty())
}

#[cfg(not(target_os = "windows"))]
fn unix_stop_processes(image_names: &[&str], command_marker: &str) -> bool {
    let system = System::new_all();
    let pids = unix_matching_bot_pids(&system, image_names, command_marker);
    let mut stopped = false;

    for pid in pids {
        if let Some(process) = system.process(pid) {
            stopped |= process.kill();
        }
    }

    stopped
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

fn append_debug_session_start(path: &std::path::Path, simulation: bool, args: &[String]) {
    let mode = if simulation { "simulation" } else { "live" };
    let line = format!(
        "==================== session start {} mode={} args={} ====================",
        Utc::now().to_rfc3339(),
        mode,
        args.join(" ")
    );
    append_debug_line(path, "SYSTEM", &line);
}

fn append_debug_line(path: &std::path::Path, source: &str, line: &str) {
    let ts = Utc::now().to_rfc3339();
    let content = format!("[{ts}] [{source}] {line}\n");
    write_debug_line(path, &content);
    if let Some(parent) = path.parent() {
        let full_log = parent.join("evpoly-full-debug.log.txt");
        write_debug_line(&full_log, &content);
    }
}

fn write_debug_line(path: &std::path::Path, content: &str) {
    let _ = append_file_log_line(path, content);
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

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| format!("build bot request client: {e}"))?;
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

#[cfg(test)]
mod tests {
    use super::{ensure_bot_runtime_dirs, process_name_matches};

    #[test]
    fn ensure_bot_runtime_dirs_creates_history_subdir() {
        let temp_dir = std::env::temp_dir().join(format!(
            "evpoly-bot-manager-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));

        ensure_bot_runtime_dirs(&temp_dir).expect("create bot runtime dirs");

        assert!(temp_dir.exists());
        assert!(temp_dir.join("history").exists());

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn process_name_matches_accepts_linux_sidecar_suffixes() {
        assert!(process_name_matches(
            "evpoly-bot-x86_64-unknown-linux-gnu",
            &["evpoly-bot", "polymarket-arbitrage-bot"]
        ));
        assert!(process_name_matches(
            "polymarket-arbitrage-bot",
            &["evpoly-bot", "polymarket-arbitrage-bot"]
        ));
        assert!(!process_name_matches(
            "random-helper",
            &["evpoly-bot", "polymarket-arbitrage-bot"]
        ));
    }
}
