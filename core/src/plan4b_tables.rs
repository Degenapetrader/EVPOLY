use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{Timelike, Utc};

use crate::strategy::Timeframe;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SessionWatchKey {
    pub symbol: String,
    pub timeframe: Timeframe,
    pub session_index: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct SessionWatchStart {
    pub tau_trigger_sec: i64,
    pub watch_start_sec: i64,
    pub trigger_rate_pct: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Plan4bTables {
    rates: HashMap<(String, Timeframe, u8, i64), f64>,
}

impl Plan4bTables {
    pub fn load_default() -> Result<Self> {
        Self::load_from_path("docs/plan4b.md")
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path_ref = path.as_ref();
        let content = fs::read_to_string(path_ref)
            .with_context(|| format!("failed to read {}", path_ref.display()))?;
        let mut rates = HashMap::new();
        let mut current_symbol = "BTC".to_string();
        let mut current_tf: Option<Timeframe> = None;
        let mut current_session: Option<u8> = None;

        for raw_line in content.lines() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }

            if line.starts_with("## ") && line.contains("(Sessionized)") {
                if let Some(symbol) = parse_symbol_header(line) {
                    current_symbol = normalize_symbol(symbol.as_str());
                    current_tf = None;
                    current_session = None;
                }
                continue;
            }

            if line.starts_with("### ") {
                if let Some((timeframe, session_index)) = parse_tf_session_header(line) {
                    current_tf = Some(timeframe);
                    current_session = Some(session_index);
                } else {
                    current_tf = None;
                    current_session = None;
                }
                continue;
            }

            let (Some(timeframe), Some(session_index)) = (current_tf, current_session) else {
                continue;
            };
            if !line.starts_with('|') {
                continue;
            }
            let mut cells = line
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim())
                .collect::<Vec<_>>();
            if cells.len() < 4 {
                continue;
            }
            let tau_sec = match parse_i64(cells[0]) {
                Some(v) if v > 0 => v,
                _ => continue,
            };
            let rate_pct = match parse_f64(cells[3]) {
                Some(v) if v.is_finite() && v >= 0.0 => v,
                _ => continue,
            };
            let key = (
                current_symbol.clone(),
                timeframe,
                session_index.clamp(1, 6),
                tau_sec,
            );
            rates.insert(key, rate_pct);
            cells.clear();
        }

        if rates.is_empty() {
            return Err(anyhow!(
                "no sessionized rows parsed from {}",
                path_ref.display()
            ));
        }
        Ok(Self { rates })
    }

    pub fn derive_watch_starts(
        &self,
        symbols: &[String],
        timeframes: &[Timeframe],
        threshold_pct: f64,
        prewatch_sec: i64,
    ) -> HashMap<SessionWatchKey, SessionWatchStart> {
        let mut out = HashMap::new();
        let threshold = threshold_pct.max(0.0);
        let prewatch = prewatch_sec.max(0);

        for symbol in symbols.iter().map(|s| normalize_symbol(s.as_str())) {
            for timeframe in timeframes.iter().copied() {
                for session_index in 1_u8..=6_u8 {
                    let mut rows = self
                        .rates
                        .iter()
                        .filter_map(|((row_symbol, row_tf, row_session, tau), rate)| {
                            if row_symbol.eq_ignore_ascii_case(symbol.as_str())
                                && *row_tf == timeframe
                                && *row_session == session_index
                            {
                                Some((*tau, *rate))
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                    if rows.is_empty() {
                        continue;
                    }
                    rows.sort_by_key(|(tau, _)| *tau);
                    let Some((tau_trigger_sec, trigger_rate_pct)) =
                        rows.into_iter().find(|(_, rate)| *rate >= threshold)
                    else {
                        continue;
                    };
                    out.insert(
                        SessionWatchKey {
                            symbol: symbol.clone(),
                            timeframe,
                            session_index,
                        },
                        SessionWatchStart {
                            tau_trigger_sec,
                            watch_start_sec: (tau_trigger_sec - prewatch).max(1),
                            trigger_rate_pct,
                        },
                    );
                }
            }
        }

        out
    }

    pub fn session_index_for_period_open_utc(period_open_ts: i64) -> Option<u8> {
        let dt = chrono::DateTime::<Utc>::from_timestamp(period_open_ts, 0)?;
        Some(
            (dt.hour().div_euclid(4) as u8)
                .saturating_add(1)
                .clamp(1, 6),
        )
    }

    pub fn session_label(session_index: u8) -> &'static str {
        match session_index {
            1 => "00-04",
            2 => "04-08",
            3 => "08-12",
            4 => "12-16",
            5 => "16-20",
            _ => "20-24",
        }
    }
}

fn parse_symbol_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("## ")?;
    let (symbol, _) = rest.split_once(' ')?;
    let symbol = symbol.trim();
    if symbol.is_empty() {
        return None;
    }
    if !symbol.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(symbol.to_ascii_uppercase())
}

fn parse_tf_session_header(line: &str) -> Option<(Timeframe, u8)> {
    let rest = line.strip_prefix("### ")?;
    let mut parts = rest.split_whitespace();
    let tf = parse_timeframe(parts.next()?)?;
    let session_raw = parts.next()?;
    let session_num = session_raw.trim_start_matches('S').parse::<u8>().ok()?;
    if !(1..=6).contains(&session_num) {
        return None;
    }
    Some((tf, session_num))
}

fn parse_timeframe(value: &str) -> Option<Timeframe> {
    match value.trim().to_ascii_lowercase().as_str() {
        "5m" => Some(Timeframe::M5),
        "15m" => Some(Timeframe::M15),
        "1h" | "60m" => Some(Timeframe::H1),
        "4h" | "240m" => Some(Timeframe::H4),
        _ => None,
    }
}

fn parse_i64(value: &str) -> Option<i64> {
    value.trim().replace(',', "").parse::<i64>().ok()
}

fn parse_f64(value: &str) -> Option<f64> {
    value
        .trim()
        .trim_end_matches('%')
        .replace(',', "")
        .parse::<f64>()
        .ok()
}

fn normalize_symbol(value: &str) -> String {
    match value.trim().to_ascii_uppercase().as_str() {
        "SOLANA" => "SOL".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_btc_h4_section() {
        let fixture = r#"
## BTC (Sessionized)
### 4h S6
| tau_sec | windows | flips | rate_pct |
|---|---|---|---|
| 251 | 100 | 1 | 1.9 |
| 252 | 100 | 2 | 2.1 |
"#;
        let temp_path = std::env::temp_dir().join(format!(
            "plan4b_test_{}_{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&temp_path, fixture).expect("write temp plan4b fixture");
        let tables = Plan4bTables::load_from_path(&temp_path).expect("load plan4b fixture");
        let _ = std::fs::remove_file(&temp_path);
        let watch = tables.derive_watch_starts(&[String::from("BTC")], &[Timeframe::H4], 2.0, 1);
        let s6 = watch
            .get(&SessionWatchKey {
                symbol: "BTC".to_string(),
                timeframe: Timeframe::H4,
                session_index: 6,
            })
            .expect("btc h4 s6 watch");
        assert_eq!(s6.watch_start_sec, 251);
    }
}
