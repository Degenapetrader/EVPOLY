pub mod adaptive_chase;
pub mod api;
pub mod arbiter;
pub mod binance_wss;
pub mod bot_admin;
pub mod coinbase_ws;
pub mod config;
pub mod detector;
pub mod endgame_sweep;
pub mod entry_idempotency;
pub mod evcurve;
pub mod event_log;
pub mod evsnipe;
pub mod execution_pricing;
pub mod feature_engine_v2;
pub mod hl_signals;
pub mod hyperliquid_wss;
pub mod kelly;
pub mod market_discovery;
pub mod merge;
pub mod microflow;
pub mod mm;
pub mod models;
pub mod monitor;
pub mod plan3_tables;
pub mod plan4b_tables;
pub mod plandaily_tables;
pub mod polymarket_ws;
pub mod sessionband;
pub mod signal_state;
pub mod simulation;
pub mod size_policy;
pub mod strategy;
pub mod strategy_decider;
pub mod symbol_ownership;
pub mod tracking_db;
pub mod trader;
pub mod ui_contracts;

// Re-export commonly used types
pub use api::PolymarketApi;
pub use config::Config;
pub use models::TokenPrice;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

struct HistorySink {
    file: File,
    path: PathBuf,
    writes_since_flush: u64,
}

static HISTORY_FILE: OnceLock<Mutex<HistorySink>> = OnceLock::new();

fn history_flush_every() -> u64 {
    std::env::var("EVPOLY_HISTORY_FLUSH_EVERY")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(32)
        .max(1)
}

fn history_max_bytes() -> u64 {
    std::env::var("EVPOLY_HISTORY_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(100 * 1024 * 1024)
        .max(1024 * 1024)
}

fn history_rotate_keep() -> usize {
    std::env::var("EVPOLY_HISTORY_ROTATE_KEEP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(7)
        .max(1)
}

fn rotate_history_if_needed(sink: &mut HistorySink) {
    let cur_len = sink.file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    if cur_len < history_max_bytes() {
        return;
    }

    let stamp = chrono::Utc::now().timestamp();
    let rotated = PathBuf::from(format!("{}.{}", sink.path.display(), stamp));
    let _ = sink.file.flush();
    if std::fs::rename(&sink.path, &rotated).is_err() {
        return;
    }
    if let Ok(new_file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&sink.path)
    {
        sink.file = new_file;
        sink.writes_since_flush = 0;
    }

    let keep = history_rotate_keep();
    let parent = sink
        .path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base = sink
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("history.toml")
        .to_string();
    let mut rotated_files = Vec::new();
    if let Ok(rd) = std::fs::read_dir(parent) {
        for ent in rd.flatten() {
            if let Some(name) = ent.file_name().to_str() {
                if name.starts_with(&format!("{base}.")) {
                    rotated_files.push(ent.path());
                }
            }
        }
    }
    rotated_files.sort();
    if rotated_files.len() > keep {
        for old in rotated_files
            .iter()
            .take(rotated_files.len().saturating_sub(keep))
        {
            let _ = std::fs::remove_file(old);
        }
    }
}

pub fn init_history_file(file: File) {
    init_history_file_with_path(file, "history.toml");
}

pub fn init_history_file_with_path(file: File, path: &str) {
    HISTORY_FILE
        .set(Mutex::new(HistorySink {
            file,
            path: PathBuf::from(path),
            writes_since_flush: 0,
        }))
        .unwrap_or_else(|_| panic!("History file already initialized"));
}

pub fn append_history_bytes(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    if let Some(file_mutex) = HISTORY_FILE.get() {
        if let Ok(mut sink) = file_mutex.lock() {
            rotate_history_if_needed(&mut sink);
            if sink.file.write_all(bytes).is_ok() {
                sink.writes_since_flush = sink.writes_since_flush.saturating_add(1);
                if sink.writes_since_flush >= history_flush_every() {
                    let _ = sink.file.flush();
                    sink.writes_since_flush = 0;
                }
            }
        }
    }
}

pub fn flush_history_file() {
    if let Some(file_mutex) = HISTORY_FILE.get() {
        if let Ok(mut sink) = file_mutex.lock() {
            let _ = sink.file.flush();
            sink.writes_since_flush = 0;
        }
    }
}

pub fn log_to_history(message: &str) {
    eprint!("{}", message);
    append_history_bytes(message.as_bytes());
}

pub fn log_trading_event(event: &str) {
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    log_to_history(&format!("[{}] {}\n", timestamp, event));
}

// Macro for logging - modules use crate::log_println!
#[macro_export]
macro_rules! log_println {
    ($($arg:tt)*) => {
        {
            let message = format!($($arg)*);
            $crate::log_to_history(&format!("{}\n", message));
        }
    };
}
