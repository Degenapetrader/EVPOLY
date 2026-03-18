#![allow(dead_code)]

use chrono::Utc;
use serde::Serialize;
use std::collections::VecDeque;

const MAX_LINES: usize = 500;

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

    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

fn detect_level(content: &str) -> LogLevel {
    let lower = content.to_lowercase();
    if lower.contains("error") || lower.contains("panic") || lower.contains("fatal") {
        LogLevel::Error
    } else if lower.contains("warn") {
        LogLevel::Warn
    } else {
        LogLevel::Info
    }
}
