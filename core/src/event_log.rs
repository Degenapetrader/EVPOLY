use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::OnceLock;

use log::warn;
use serde_json::{json, Value};

static EVENT_TX: OnceLock<SyncSender<String>> = OnceLock::new();
static EVENT_PATH: OnceLock<String> = OnceLock::new();
static EVENT_WRITE_ERROR_COUNT: AtomicU64 = AtomicU64::new(0);
static EVENT_WRITE_WARN_LAST_MS: AtomicI64 = AtomicI64::new(0);
static EVENT_QUEUE_DROP_COUNT: AtomicU64 = AtomicU64::new(0);
static EVENT_QUEUE_WARN_LAST_MS: AtomicI64 = AtomicI64::new(0);

pub fn init_event_log(path: &str) -> anyhow::Result<()> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let _ = EVENT_PATH.set(path.to_string());

    let queue_cap = std::env::var("EVPOLY_EVENT_LOG_QUEUE_CAP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(16_384)
        .max(256);
    let batch_size = std::env::var("EVPOLY_EVENT_LOG_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(512)
        .max(1);
    let flush_ms = std::env::var("EVPOLY_EVENT_LOG_FLUSH_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(200)
        .max(10);

    let (tx, rx) = sync_channel::<String>(queue_cap);
    let _ = EVENT_TX.set(tx);

    std::thread::Builder::new()
        .name("evpoly-eventlog".to_string())
        .spawn(move || writer_loop(file, rx, batch_size, flush_ms))
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("failed to spawn event logger thread: {}", e))
}

fn writer_loop(mut file: File, rx: Receiver<String>, batch_size: usize, flush_ms: u64) {
    let timeout = std::time::Duration::from_millis(flush_ms);
    let mut batch: Vec<String> = Vec::with_capacity(batch_size);
    loop {
        match rx.recv_timeout(timeout) {
            Ok(line) => {
                batch.push(line);
                while batch.len() < batch_size {
                    match rx.try_recv() {
                        Ok(next_line) => batch.push(next_line),
                        Err(_) => break,
                    }
                }
                write_batch(&mut file, &batch);
                batch.clear();
            }
            Err(RecvTimeoutError::Timeout) => {
                if !batch.is_empty() {
                    write_batch(&mut file, &batch);
                    batch.clear();
                }
                let _ = file.flush();
            }
            Err(RecvTimeoutError::Disconnected) => {
                if !batch.is_empty() {
                    write_batch(&mut file, &batch);
                    batch.clear();
                }
                let _ = file.flush();
                break;
            }
        }
    }
}

fn write_batch(file: &mut File, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    rotate_if_needed(file);
    for line in lines {
        if let Err(e) = writeln!(file, "{}", line) {
            warn_event_write_error("write", &e);
            break;
        }
    }
}

fn rotate_if_needed(file: &mut File) {
    let Some(path) = EVENT_PATH.get() else {
        return;
    };
    let max_bytes = std::env::var("EVPOLY_EVENTS_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(50 * 1024 * 1024);
    let keep = std::env::var("EVPOLY_EVENTS_ROTATE_KEEP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5)
        .max(1);
    let cur_len = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    if cur_len < max_bytes {
        return;
    }

    let stamp = chrono::Utc::now().timestamp();
    let rotated = format!("{path}.{stamp}");
    let _ = file.flush();
    if std::fs::rename(path, &rotated).is_err() {
        return;
    }
    if let Ok(new_file) = OpenOptions::new().create(true).append(true).open(path) {
        *file = new_file;
    }

    let path_obj = std::path::Path::new(path);
    let parent = path_obj
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let base = path_obj
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("events.jsonl")
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

pub fn log_event(kind: &str, payload: Value) {
    let ts_ms = chrono::Utc::now().timestamp_millis();
    let line = json!({
        "ts_ms": ts_ms,
        "kind": kind,
        "payload": payload
    })
    .to_string();

    if let Some(tx) = EVENT_TX.get() {
        match tx.try_send(line) {
            Ok(_) => {}
            Err(TrySendError::Full(_)) => warn_event_queue_drop("queue_full"),
            Err(TrySendError::Disconnected(_)) => warn_event_queue_drop("queue_disconnected"),
        }
    }
}

fn warn_event_queue_drop(reason: &str) {
    let drops = EVENT_QUEUE_DROP_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let cooldown_ms = std::env::var("EVPOLY_EVENT_QUEUE_WARN_COOLDOWN_MS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(30_000)
        .max(1_000);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let last_ms = EVENT_QUEUE_WARN_LAST_MS.load(Ordering::Relaxed);
    if now_ms.saturating_sub(last_ms) < cooldown_ms {
        return;
    }
    if EVENT_QUEUE_WARN_LAST_MS
        .compare_exchange(last_ms, now_ms, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        warn!(
            "event log queue drop reason={} count={} (cooldown_ms={})",
            reason, drops, cooldown_ms
        );
    }
}

fn warn_event_write_error(context: &str, err: &std::io::Error) {
    let failures = EVENT_WRITE_ERROR_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let cooldown_ms = std::env::var("EVPOLY_EVENT_WRITE_WARN_COOLDOWN_MS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(60_000)
        .max(1_000);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let last_ms = EVENT_WRITE_WARN_LAST_MS.load(Ordering::Relaxed);
    if now_ms.saturating_sub(last_ms) < cooldown_ms {
        return;
    }
    if EVENT_WRITE_WARN_LAST_MS
        .compare_exchange(last_ms, now_ms, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        warn!(
            "event log {} failure (count={}): {}",
            context, failures, err
        );
    }
}
