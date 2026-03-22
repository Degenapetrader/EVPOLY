#![allow(dead_code)]

use chrono::Utc;
use serde::Serialize;
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAX_LINES: usize = 500;
pub const DEFAULT_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const DEFAULT_LOG_ROTATIONS: usize = 3;

#[derive(Clone, Serialize)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Serialize)]
pub struct LogLine {
    pub timestamp: String,
    pub content: String,
    pub level: LogLevel,
}

pub struct LogBuffer {
    lines: VecDeque<LogLine>,
}

#[derive(Clone, Serialize)]
pub struct LogTailBatch {
    pub next_cursor: u64,
    pub reset: bool,
    pub lines: Vec<LogLine>,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl LogBuffer {
    pub fn new() -> Self {
        Self {
            lines: VecDeque::with_capacity(MAX_LINES),
        }
    }

    pub fn push(&mut self, content: String) {
        let level = detect_level(&content);
        let line = LogLine {
            timestamp: Utc::now().to_rfc3339(),
            content,
            level,
        };
        if self.lines.len() >= MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn get_lines(&self, last_n: usize) -> Vec<LogLine> {
        let skip = self.lines.len().saturating_sub(last_n);
        self.lines.iter().skip(skip).cloned().collect()
    }

    pub fn latest_line(&self) -> Option<LogLine> {
        self.lines.back().cloned()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

fn detect_level(content: &str) -> LogLevel {
    let upper = content.to_ascii_uppercase();

    // Prefer explicit runtime level tags first (e.g. "[... WARN ...]").
    if upper.contains(" WARN ") || upper.starts_with("WARN ") || upper.contains("\tWARN ") {
        return LogLevel::Warn;
    }
    if upper.contains(" ERROR ")
        || upper.starts_with("ERROR ")
        || upper.contains("\tERROR ")
        || upper.contains(" PANIC")
        || upper.contains(" FATAL")
    {
        return LogLevel::Error;
    }

    let lower = content.to_ascii_lowercase();
    if lower.contains("panic") || lower.contains("fatal") {
        LogLevel::Error
    } else if lower.contains("warn") {
        LogLevel::Warn
    } else {
        LogLevel::Info
    }
}

pub fn append_file_log_line(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    rotate_log_if_needed(path, content.len() as u64)?;
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    file.write_all(content.as_bytes())
}

pub fn read_log_tail(
    path: &Path,
    cursor: Option<u64>,
    limit: usize,
) -> std::io::Result<LogTailBatch> {
    let requested_cursor = cursor.unwrap_or(0);
    if !path.exists() {
        return Ok(LogTailBatch {
            next_cursor: 0,
            reset: requested_cursor > 0,
            lines: Vec::new(),
        });
    }

    let metadata = fs::metadata(path)?;
    let file_len = metadata.len();
    let reset = requested_cursor > file_len;
    let start = if reset { 0 } else { requested_cursor };

    let mut file = OpenOptions::new().read(true).open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut buffer = String::new();
    file.read_to_string(&mut buffer)?;
    let next_cursor = file.stream_position()?;
    let lines = buffer
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (timestamp, content) = parse_embedded_log_line(line)
                .unwrap_or_else(|| (Utc::now().to_rfc3339(), line.to_string()));
            LogLine {
                timestamp,
                level: detect_level(&content),
                content,
            }
        })
        .collect::<Vec<_>>();
    let keep_from = lines.len().saturating_sub(limit.max(1));

    Ok(LogTailBatch {
        next_cursor,
        reset,
        lines: lines.into_iter().skip(keep_from).collect(),
    })
}

fn rotate_log_if_needed(path: &Path, incoming_len: u64) -> std::io::Result<()> {
    let current_len = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if current_len.saturating_add(incoming_len) <= DEFAULT_LOG_MAX_BYTES {
        return Ok(());
    }

    for index in (1..=DEFAULT_LOG_ROTATIONS).rev() {
        let rotated = rotated_log_path(path, index);
        if index == DEFAULT_LOG_ROTATIONS {
            if rotated.exists() {
                let _ = fs::remove_file(rotated);
            }
            continue;
        }

        if rotated.exists() {
            let next = rotated_log_path(path, index + 1);
            if next.exists() {
                let _ = fs::remove_file(&next);
            }
            fs::rename(rotated, next)?;
        }
    }

    if path.exists() {
        let first_rotated = rotated_log_path(path, 1);
        if first_rotated.exists() {
            let _ = fs::remove_file(&first_rotated);
        }
        fs::rename(path, first_rotated)?;
    }

    Ok(())
}

fn rotated_log_path(path: &Path, index: usize) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("{name}.{index}"))
        .unwrap_or_else(|| format!("log.{index}"));
    path.with_file_name(file_name)
}

fn parse_embedded_log_line(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix('[')?;
    let ts_end = rest.find(']')?;
    let timestamp = rest[..ts_end].to_string();
    let content = rest.get(ts_end + 1..)?.trim().to_string();
    Some((timestamp, content))
}
