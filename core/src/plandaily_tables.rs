use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{Datelike, Utc};
use chrono_tz::America::New_York;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlanDailyCell {
    pub flips: u32,
    pub n: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeadBinRange {
    pub low_pct: f64,
    pub high_pct: f64,
}

#[derive(Debug, Clone)]
pub struct PlanDailyLookupResult {
    pub flips: u32,
    pub n: u32,
    pub p_flip: f64,
    pub p_hold: f64,
    pub lead_bin_idx: usize,
    pub tau_low_sec: i64,
    pub tau_high_sec: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PlanDailyKey {
    symbol: String,
    group_key: String,
    tau_bucket_sec: i64,
    lead_bin_idx: usize,
}

#[derive(Debug, Clone)]
pub struct PlanDailyTables {
    cells: HashMap<PlanDailyKey, PlanDailyCell>,
    bin_ranges: HashMap<String, Vec<LeadBinRange>>,
    checkpoints_by_symbol: HashMap<String, Vec<i64>>,
}

#[derive(Debug, Clone)]
struct ParseContext {
    symbol: String,
    tau_sec: i64,
}

impl PlanDailyTables {
    pub fn load_default() -> Result<Self> {
        Self::load_from_path(Path::new("docs/plandaily.md"))
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::from_markdown(text.as_str())
    }

    pub fn from_markdown(text: &str) -> Result<Self> {
        let lines = text.lines().collect::<Vec<_>>();
        let mut i = 0usize;
        let mut ctx: Option<ParseContext> = None;
        let mut cells: HashMap<PlanDailyKey, PlanDailyCell> = HashMap::new();
        let mut bin_ranges: HashMap<String, Vec<LeadBinRange>> = HashMap::new();
        let mut checkpoints_by_symbol: HashMap<String, Vec<i64>> = HashMap::new();

        while i < lines.len() {
            let line = lines[i].trim();
            if let Some(parsed) = parse_heading_context(line) {
                ctx = Some(parsed);
                i += 1;
                continue;
            }

            if line.starts_with('|') {
                if let Some(context) = ctx.clone() {
                    if let Ok((headers, rows, next_idx)) = parse_markdown_table(&lines, i) {
                        let is_daily_window_table = headers
                            .first()
                            .map(|h| h.trim().eq_ignore_ascii_case("window"))
                            .unwrap_or(false);

                        if is_daily_window_table {
                            let symbol_key = normalize_symbol(context.symbol.as_str())
                                .unwrap_or_else(|| context.symbol.to_ascii_uppercase());

                            let bins = headers
                                .iter()
                                .skip(1)
                                .filter_map(|label| parse_bin_range(label))
                                .collect::<Vec<_>>();
                            if !bins.is_empty() {
                                bin_ranges.insert(symbol_key.clone(), bins);
                            }

                            checkpoints_by_symbol
                                .entry(symbol_key.clone())
                                .or_default()
                                .push(context.tau_sec);

                            for row in rows {
                                if row.len() < 2 {
                                    continue;
                                }
                                let Some(group_key) = canonical_group_key(row[0].as_str()) else {
                                    continue;
                                };
                                for (bin_idx, cell) in row.iter().skip(1).enumerate() {
                                    if let Some(parsed) = parse_flips_n(cell) {
                                        cells.insert(
                                            PlanDailyKey {
                                                symbol: symbol_key.clone(),
                                                group_key: group_key.clone(),
                                                tau_bucket_sec: context.tau_sec,
                                                lead_bin_idx: bin_idx,
                                            },
                                            parsed,
                                        );
                                    }
                                }
                            }
                            ctx = None;
                            i = next_idx;
                            continue;
                        }

                        ctx = None;
                        i = next_idx;
                        continue;
                    }
                }
            }

            i += 1;
        }

        if cells.is_empty() {
            return Err(anyhow!("no plandaily cells parsed"));
        }

        for checkpoints in checkpoints_by_symbol.values_mut() {
            checkpoints.sort_unstable();
            checkpoints.dedup();
        }

        Ok(Self {
            cells,
            bin_ranges,
            checkpoints_by_symbol,
        })
    }

    pub fn checkpoints_for_symbol(&self, symbol: &str) -> Vec<i64> {
        let symbol = normalize_symbol(symbol).unwrap_or_else(|| symbol.to_ascii_uppercase());
        self.checkpoints_by_symbol
            .get(symbol.as_str())
            .cloned()
            .unwrap_or_default()
    }

    pub fn bin_index_for_lead(&self, symbol: &str, lead_pct: f64) -> Option<usize> {
        let symbol = normalize_symbol(symbol)?;
        let bins = self.bin_ranges.get(symbol.as_str())?;
        if bins.is_empty() {
            return None;
        }
        let lead = lead_pct.max(0.0);
        for (idx, range) in bins.iter().enumerate() {
            let last = idx + 1 == bins.len();
            if lead >= range.low_pct && (lead < range.high_pct || (last && lead <= range.high_pct))
            {
                return Some(idx);
            }
        }
        None
    }

    pub fn lookup(
        &self,
        symbol: &str,
        group_key: &str,
        tau_sec: i64,
        lead_pct: f64,
    ) -> Option<PlanDailyLookupResult> {
        let symbol = normalize_symbol(symbol)?;
        let group_key = canonical_group_key(group_key)?;
        let bin_idx = self.bin_index_for_lead(symbol.as_str(), lead_pct)?;

        let checkpoints = self.checkpoints_by_symbol.get(symbol.as_str())?;
        let (tau_low, tau_high, weight_high) = interpolation_bounds(checkpoints, tau_sec);

        let low = self
            .cells
            .get(&PlanDailyKey {
                symbol: symbol.clone(),
                group_key: group_key.clone(),
                tau_bucket_sec: tau_low,
                lead_bin_idx: bin_idx,
            })
            .copied();
        let high = self
            .cells
            .get(&PlanDailyKey {
                symbol: symbol.clone(),
                group_key: group_key.clone(),
                tau_bucket_sec: tau_high,
                lead_bin_idx: bin_idx,
            })
            .copied();

        let (p_flip, n, flips) = match (low, high) {
            (Some(a), Some(b)) if tau_low != tau_high => {
                let p_a = a.flips as f64 / a.n as f64;
                let p_b = b.flips as f64 / b.n as f64;
                let p = p_a + (p_b - p_a) * weight_high;
                let n_interp =
                    ((a.n as f64) + ((b.n as f64) - (a.n as f64)) * weight_high).round() as u32;
                let n = n_interp.max(1);
                let flips = (p * n as f64).round() as u32;
                (p.clamp(0.0, 1.0), n, flips)
            }
            (Some(a), _) => {
                let p = a.flips as f64 / a.n as f64;
                (p.clamp(0.0, 1.0), a.n, a.flips)
            }
            (_, Some(b)) => {
                let p = b.flips as f64 / b.n as f64;
                (p.clamp(0.0, 1.0), b.n, b.flips)
            }
            (None, None) => return None,
        };

        Some(PlanDailyLookupResult {
            flips,
            n,
            p_flip,
            p_hold: (1.0 - p_flip).clamp(0.0, 1.0),
            lead_bin_idx: bin_idx,
            tau_low_sec: tau_low,
            tau_high_sec: tau_high,
        })
    }

    pub fn group_key_for_period_open(period_open_ts: i64) -> Option<String> {
        let dt_utc = chrono::DateTime::<Utc>::from_timestamp(period_open_ts, 0)?;
        let dt_et = dt_utc.with_timezone(&New_York);
        let close_et = dt_et + chrono::Duration::days(1);
        if close_et.weekday().number_from_monday() <= 5 {
            Some("weekday".to_string())
        } else {
            Some("weekend".to_string())
        }
    }
}

fn interpolation_bounds(checkpoints: &[i64], tau_sec: i64) -> (i64, i64, f64) {
    if checkpoints.is_empty() {
        return (tau_sec.max(0), tau_sec.max(0), 0.0);
    }
    let mut sorted = checkpoints.to_vec();
    sorted.sort_unstable();

    let tau = tau_sec.max(0);
    if tau <= sorted[0] {
        return (sorted[0], sorted[0], 0.0);
    }
    if tau >= *sorted.last().unwrap_or(&sorted[0]) {
        let t = *sorted.last().unwrap_or(&sorted[0]);
        return (t, t, 0.0);
    }

    for window in sorted.windows(2) {
        let low = window[0];
        let high = window[1];
        if tau >= low && tau <= high {
            if high == low {
                return (low, high, 0.0);
            }
            let weight_high = (tau - low) as f64 / (high - low) as f64;
            return (low, high, weight_high.clamp(0.0, 1.0));
        }
    }

    let t = *sorted.last().unwrap_or(&sorted[0]);
    (t, t, 0.0)
}

fn parse_heading_context(line: &str) -> Option<ParseContext> {
    let heading = line.strip_prefix("### ")?.trim();
    let (left, right) = heading.split_once(" — ")?;
    let symbol = normalize_symbol(left.trim())?;
    let tau_sec = tau_label_to_sec(right.trim())?;
    Some(ParseContext { symbol, tau_sec })
}

fn parse_markdown_table(
    lines: &[&str],
    start_idx: usize,
) -> Result<(Vec<String>, Vec<Vec<String>>, usize)> {
    if start_idx + 1 >= lines.len() {
        return Err(anyhow!("incomplete table"));
    }
    let header_line = lines[start_idx].trim();
    let align_line = lines[start_idx + 1].trim();
    if !header_line.starts_with('|') || !align_line.starts_with('|') {
        return Err(anyhow!("not a markdown table"));
    }

    let headers = split_table_cells(header_line);
    if headers.is_empty() {
        return Err(anyhow!("empty table header"));
    }

    let mut rows = Vec::new();
    let mut idx = start_idx + 2;
    while idx < lines.len() {
        let line = lines[idx].trim();
        if !line.starts_with('|') {
            break;
        }
        let cells = split_table_cells(line);
        if !cells.is_empty() {
            rows.push(cells);
        }
        idx += 1;
    }

    Ok((headers, rows, idx))
}

fn split_table_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect::<Vec<_>>()
}

fn normalize_symbol(raw: &str) -> Option<String> {
    let normalized = raw.trim().to_ascii_uppercase();
    match normalized.as_str() {
        "BTC" => Some("BTC".to_string()),
        "ETH" => Some("ETH".to_string()),
        "SOL" | "SOLANA" => Some("SOL".to_string()),
        "XRP" => Some("XRP".to_string()),
        _ => None,
    }
}

fn canonical_group_key(raw: &str) -> Option<String> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.starts_with("weekday") {
        Some("weekday".to_string())
    } else if lower.starts_with("weekend") {
        Some("weekend".to_string())
    } else {
        None
    }
}

fn tau_label_to_sec(raw: &str) -> Option<i64> {
    let compact = raw.trim().to_ascii_lowercase().replace(' ', "");
    let t_start = compact.find("t-")?;
    let rest = &compact[t_start + 2..];
    let digits = rest
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    let value = digits.parse::<i64>().ok()?;
    if rest.contains('h') {
        Some(value.saturating_mul(3600))
    } else if rest.contains('m') {
        Some(value.saturating_mul(60))
    } else {
        Some(value)
    }
}

fn parse_bin_range(raw: &str) -> Option<LeadBinRange> {
    let cleaned = raw
        .trim()
        .trim_end_matches('%')
        .replace(',', "")
        .replace(' ', "");
    let (low_raw, high_raw) = cleaned.split_once('-')?;
    let low_pct = low_raw.parse::<f64>().ok()?;
    let high_pct = high_raw.parse::<f64>().ok()?;
    if !low_pct.is_finite() || !high_pct.is_finite() || high_pct < low_pct {
        return None;
    }
    Some(LeadBinRange { low_pct, high_pct })
}

fn parse_flips_n(raw: &str) -> Option<PlanDailyCell> {
    let value = raw
        .trim()
        .trim_matches('`')
        .replace(',', "")
        .replace(' ', "");
    if value.is_empty() || value == "—" || value == "-" {
        return None;
    }
    let (flips_raw, n_raw) = value.split_once('/')?;
    let flips = flips_raw.trim().replace(',', "").parse::<u32>().ok()?;
    let n = n_raw.trim().replace(',', "").parse::<u32>().ok()?;
    if n == 0 {
        return None;
    }
    Some(PlanDailyCell { flips, n })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_daily_table() {
        let text = r#"
### BTC — T-16h (1815 windows)

| Window | 0.0-0.25% | 0.25-0.5% |
|---|---|---|
| Weekday | 1/10 | 2/20 |
| Weekend | 0/5 | — |
"#;
        let tables = PlanDailyTables::from_markdown(text).expect("tables");
        let lookup = tables
            .lookup("BTC", "weekday", 57_600, 0.10)
            .expect("lookup");
        assert_eq!(lookup.flips, 1);
        assert_eq!(lookup.n, 10);
        assert!((lookup.p_flip - 0.1).abs() <= 1e-9);
    }

    #[test]
    fn group_key_uses_close_day() {
        // 2026-02-25 17:00:00 UTC == 12:00 ET open; close is next day (Thursday => weekday)
        let period_open_ts = 1_772_040_000_i64;
        assert_eq!(
            PlanDailyTables::group_key_for_period_open(period_open_ts).as_deref(),
            Some("weekday")
        );
    }
}
