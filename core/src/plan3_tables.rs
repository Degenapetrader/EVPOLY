use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{Timelike, Utc};
use chrono_tz::America::New_York;

use crate::strategy::Timeframe;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan3Cell {
    pub flips: u32,
    pub n: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeadBinRange {
    pub low_pct: f64,
    pub high_pct: f64,
}

#[derive(Debug, Clone)]
pub struct Plan3LookupResult {
    pub flips: u32,
    pub n: u32,
    pub p_flip: f64,
    pub p_hold: f64,
    pub lead_bin_idx: usize,
    pub tau_low_sec: i64,
    pub tau_high_sec: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Plan3Key {
    timeframe: Timeframe,
    symbol: String,
    group_key: String,
    tau_bucket_sec: i64,
    lead_bin_idx: usize,
}

#[derive(Debug, Clone)]
pub struct Plan3Tables {
    cells: HashMap<Plan3Key, Plan3Cell>,
    bin_ranges: HashMap<(Timeframe, String), Vec<LeadBinRange>>,
}

#[derive(Debug, Clone)]
enum ParseContext {
    M5 { symbol: String },
    M15 { symbol: String, group: String },
    H1 { symbol: String, tau_sec: i64 },
    H4 { symbol: String, group: String },
}

impl Plan3Tables {
    pub fn empty_for_remote() -> Self {
        Self {
            cells: HashMap::new(),
            bin_ranges: HashMap::new(),
        }
    }

    pub fn load_default() -> Result<Self> {
        Self::load_from_path(Path::new("docs/plan3.md"))
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
        let mut cells: HashMap<Plan3Key, Plan3Cell> = HashMap::new();
        let mut bin_ranges: HashMap<(Timeframe, String), Vec<LeadBinRange>> = HashMap::new();

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
                        let mut consumed = false;
                        match context {
                            ParseContext::M5 { symbol } => {
                                if headers
                                    .first()
                                    .map(|h| h.to_ascii_lowercase().contains("4h block"))
                                    .unwrap_or(false)
                                {
                                    let symbol_key = normalize_symbol(symbol.as_str())
                                        .unwrap_or_else(|| symbol.to_ascii_uppercase());
                                    let bins = headers
                                        .iter()
                                        .skip(1)
                                        .filter_map(|label| parse_bin_range(label))
                                        .collect::<Vec<_>>();
                                    if !bins.is_empty() {
                                        bin_ranges
                                            .insert((Timeframe::M5, symbol_key.clone()), bins);
                                    }
                                    for row in rows {
                                        if row.len() < 2 {
                                            continue;
                                        }
                                        let Some(group_key) =
                                            canonical_group_key(Timeframe::M5, row[0].as_str())
                                        else {
                                            continue;
                                        };
                                        for (bin_idx, cell) in row.iter().skip(1).enumerate() {
                                            if let Some(parsed) = parse_flips_n(cell) {
                                                cells.insert(
                                                    Plan3Key {
                                                        timeframe: Timeframe::M5,
                                                        symbol: symbol_key.clone(),
                                                        group_key: group_key.clone(),
                                                        tau_bucket_sec: 60,
                                                        lead_bin_idx: bin_idx,
                                                    },
                                                    parsed,
                                                );
                                            }
                                        }
                                    }
                                    consumed = true;
                                }
                            }
                            ParseContext::M15 { symbol, group } => {
                                if headers
                                    .first()
                                    .map(|h| h.to_ascii_lowercase().contains("lead"))
                                    .unwrap_or(false)
                                    && headers.iter().any(|h| h.trim().eq_ignore_ascii_case("T-5"))
                                {
                                    let symbol_key = normalize_symbol(symbol.as_str())
                                        .unwrap_or_else(|| symbol.to_ascii_uppercase());
                                    let mut bins = Vec::new();
                                    for row in &rows {
                                        if let Some(bin) =
                                            row.first().and_then(|label| parse_bin_range(label))
                                        {
                                            bins.push(bin);
                                        }
                                    }
                                    if !bins.is_empty() {
                                        bin_ranges
                                            .insert((Timeframe::M15, symbol_key.clone()), bins);
                                    }

                                    let tau_cols = headers
                                        .iter()
                                        .skip(1)
                                        .map(|h| tau_label_to_sec(h))
                                        .collect::<Vec<_>>();
                                    for (bin_idx, row) in rows.iter().enumerate() {
                                        for (col_idx, tau) in tau_cols.iter().enumerate() {
                                            let Some(tau_sec) = *tau else {
                                                continue;
                                            };
                                            let Some(cell) = row.get(col_idx + 1) else {
                                                continue;
                                            };
                                            if let Some(parsed) = parse_flips_n(cell) {
                                                cells.insert(
                                                    Plan3Key {
                                                        timeframe: Timeframe::M15,
                                                        symbol: symbol_key.clone(),
                                                        group_key: group.clone(),
                                                        tau_bucket_sec: tau_sec,
                                                        lead_bin_idx: bin_idx,
                                                    },
                                                    parsed,
                                                );
                                            }
                                        }
                                    }
                                    consumed = true;
                                }
                            }
                            ParseContext::H1 { symbol, tau_sec } => {
                                if headers
                                    .first()
                                    .map(|h| h.to_ascii_lowercase().contains("hour"))
                                    .unwrap_or(false)
                                {
                                    let symbol_key = normalize_symbol(symbol.as_str())
                                        .unwrap_or_else(|| symbol.to_ascii_uppercase());
                                    let bins = headers
                                        .iter()
                                        .skip(1)
                                        .filter_map(|label| parse_bin_range(label))
                                        .collect::<Vec<_>>();
                                    if !bins.is_empty() {
                                        bin_ranges
                                            .insert((Timeframe::H1, symbol_key.clone()), bins);
                                    }
                                    for row in rows {
                                        if row.len() < 2 {
                                            continue;
                                        }
                                        let Some(group_key) =
                                            canonical_group_key(Timeframe::H1, row[0].as_str())
                                        else {
                                            continue;
                                        };
                                        for (bin_idx, cell) in row.iter().skip(1).enumerate() {
                                            if let Some(parsed) = parse_flips_n(cell) {
                                                cells.insert(
                                                    Plan3Key {
                                                        timeframe: Timeframe::H1,
                                                        symbol: symbol_key.clone(),
                                                        group_key: group_key.clone(),
                                                        tau_bucket_sec: tau_sec,
                                                        lead_bin_idx: bin_idx,
                                                    },
                                                    parsed,
                                                );
                                            }
                                        }
                                    }
                                    consumed = true;
                                }
                            }
                            ParseContext::H4 { symbol, group } => {
                                if headers
                                    .first()
                                    .map(|h| h.to_ascii_lowercase().contains("lead"))
                                    .unwrap_or(false)
                                    && headers
                                        .iter()
                                        .any(|h| h.trim().eq_ignore_ascii_case("T-120"))
                                {
                                    let symbol_key = normalize_symbol(symbol.as_str())
                                        .unwrap_or_else(|| symbol.to_ascii_uppercase());
                                    let mut bins = Vec::new();
                                    for row in &rows {
                                        if let Some(bin) =
                                            row.first().and_then(|label| parse_bin_range(label))
                                        {
                                            bins.push(bin);
                                        }
                                    }
                                    if !bins.is_empty() {
                                        bin_ranges
                                            .insert((Timeframe::H4, symbol_key.clone()), bins);
                                    }
                                    let tau_cols = headers
                                        .iter()
                                        .skip(1)
                                        .map(|h| tau_label_to_sec(h))
                                        .collect::<Vec<_>>();
                                    for (bin_idx, row) in rows.iter().enumerate() {
                                        for (col_idx, tau) in tau_cols.iter().enumerate() {
                                            let Some(tau_sec) = *tau else {
                                                continue;
                                            };
                                            let Some(cell) = row.get(col_idx + 1) else {
                                                continue;
                                            };
                                            if let Some(parsed) = parse_flips_n(cell) {
                                                cells.insert(
                                                    Plan3Key {
                                                        timeframe: Timeframe::H4,
                                                        symbol: symbol_key.clone(),
                                                        group_key: group.clone(),
                                                        tau_bucket_sec: tau_sec,
                                                        lead_bin_idx: bin_idx,
                                                    },
                                                    parsed,
                                                );
                                            }
                                        }
                                    }
                                    consumed = true;
                                }
                            }
                        }

                        if consumed {
                            ctx = None;
                            i = next_idx;
                            continue;
                        } else {
                            // This heading/table shape is not part of the runtime lookup set.
                            // Skip the whole table to avoid repeatedly reparsing each row.
                            ctx = None;
                            i = next_idx;
                            continue;
                        }
                    }
                }
            }

            i += 1;
        }

        if cells.is_empty() {
            return Err(anyhow!("no plan3 cells parsed"));
        }

        Ok(Self { cells, bin_ranges })
    }

    pub fn exact_cell(
        &self,
        timeframe: Timeframe,
        symbol: &str,
        group_key: &str,
        tau_bucket_sec: i64,
        lead_bin_idx: usize,
    ) -> Option<Plan3Cell> {
        let symbol = normalize_symbol(symbol)?;
        let group_key = canonical_group_key(timeframe, group_key)?;
        self.cells
            .get(&Plan3Key {
                timeframe,
                symbol,
                group_key,
                tau_bucket_sec,
                lead_bin_idx,
            })
            .copied()
    }

    pub fn checkpoints_for_timeframe(timeframe: Timeframe) -> &'static [i64] {
        match timeframe {
            Timeframe::M5 => &[60],
            Timeframe::M15 => &[60, 120, 180, 240, 300],
            Timeframe::H1 => &[60, 300, 600, 900],
            Timeframe::H4 => &[900, 1800, 2700, 3600, 4500, 5400, 6300, 7200],
            Timeframe::D1 => &[3600],
        }
    }

    pub fn bin_index_for_lead(
        &self,
        timeframe: Timeframe,
        symbol: &str,
        lead_pct: f64,
    ) -> Option<usize> {
        let symbol = normalize_symbol(symbol)?;
        let bins = self.bin_ranges.get(&(timeframe, symbol))?;
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
        timeframe: Timeframe,
        symbol: &str,
        group_key: &str,
        tau_sec: i64,
        lead_pct: f64,
    ) -> Option<Plan3LookupResult> {
        let symbol = normalize_symbol(symbol)?;
        let group_key = canonical_group_key(timeframe, group_key)?;
        let bin_idx = self.bin_index_for_lead(timeframe, symbol.as_str(), lead_pct)?;
        let checkpoints = Self::checkpoints_for_timeframe(timeframe);
        let (tau_low, tau_high, weight_high) = interpolation_bounds(checkpoints, tau_sec);

        let low = self
            .cells
            .get(&Plan3Key {
                timeframe,
                symbol: symbol.clone(),
                group_key: group_key.clone(),
                tau_bucket_sec: tau_low,
                lead_bin_idx: bin_idx,
            })
            .copied();
        let high = self
            .cells
            .get(&Plan3Key {
                timeframe,
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

        Some(Plan3LookupResult {
            flips,
            n,
            p_flip,
            p_hold: (1.0 - p_flip).clamp(0.0, 1.0),
            lead_bin_idx: bin_idx,
            tau_low_sec: tau_low,
            tau_high_sec: tau_high,
        })
    }

    pub fn group_key_for_period_open(timeframe: Timeframe, period_open_ts: i64) -> Option<String> {
        let dt_utc = chrono::DateTime::<Utc>::from_timestamp(period_open_ts, 0)?;
        let dt_et = dt_utc.with_timezone(&New_York);
        match timeframe {
            Timeframe::M5 | Timeframe::H4 => {
                let start = (dt_et.hour() / 4) * 4;
                let end = start + 4;
                Some(format!("{:02}-{:02}", start, end))
            }
            Timeframe::M15 => {
                let minute = dt_et.minute();
                let group = match minute {
                    0 => "opening",
                    15 | 30 => "middle",
                    45 => "closing",
                    _ if minute < 15 => "opening",
                    _ if minute < 45 => "middle",
                    _ => "closing",
                };
                Some(group.to_string())
            }
            Timeframe::H1 => Some(dt_et.hour().to_string()),
            Timeframe::D1 => None,
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
    if let Some((left, right)) = heading.split_once(" — ") {
        let symbol = normalize_symbol(left.trim())?;
        let rhs = right.trim();

        if rhs.starts_with("T-") {
            let tau_sec = tau_label_to_sec(rhs)?;
            return Some(ParseContext::H1 { symbol, tau_sec });
        }

        let rhs_lower = rhs.to_ascii_lowercase();
        if rhs_lower.starts_with("opening")
            || rhs_lower.starts_with("middle")
            || rhs_lower.starts_with("closing")
        {
            let group = if rhs_lower.starts_with("opening") {
                "opening"
            } else if rhs_lower.starts_with("middle") {
                "middle"
            } else {
                "closing"
            }
            .to_string();
            return Some(ParseContext::M15 { symbol, group });
        }

        let session_label = rhs.split('(').next().map(str::trim).unwrap_or(rhs);
        if session_label.contains("AM") || session_label.contains("PM") {
            let group = canonical_group_key(Timeframe::H4, session_label)?;
            return Some(ParseContext::H4 { symbol, group });
        }
    } else {
        // 5m section headers are shaped like: "### BTC (208,704 windows)".
        let symbol = heading
            .split_whitespace()
            .next()
            .and_then(normalize_symbol)?;
        if heading.contains('(') {
            return Some(ParseContext::M5 { symbol });
        }
    }

    None
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

fn canonical_group_key(timeframe: Timeframe, raw: &str) -> Option<String> {
    match timeframe {
        Timeframe::M5 => normalize_block_group(raw),
        Timeframe::M15 => {
            let lower = raw.trim().to_ascii_lowercase();
            if lower.starts_with("opening") {
                Some("opening".to_string())
            } else if lower.starts_with("middle") {
                Some("middle".to_string())
            } else if lower.starts_with("closing") {
                Some("closing".to_string())
            } else {
                None
            }
        }
        Timeframe::H1 => {
            let compact = raw.trim().replace(' ', "");
            if let Ok(hour_num) = compact.parse::<u32>() {
                if hour_num <= 23 {
                    return Some(hour_num.to_string());
                }
            }
            parse_et_hour_label(compact.as_str()).map(|hour| hour.to_string())
        }
        Timeframe::H4 => {
            if let Some(block) = normalize_block_group(raw) {
                return Some(block);
            }
            session_label_to_block(raw)
        }
        Timeframe::D1 => None,
    }
}

fn normalize_block_group(raw: &str) -> Option<String> {
    let compact = raw.trim().replace(' ', "");
    let (left, right) = compact.split_once('-')?;
    let start = left.parse::<u32>().ok()?;
    let end = right.parse::<u32>().ok()?;
    if start > 23 || !(1..=24).contains(&end) || end <= start {
        return None;
    }
    Some(format!("{:02}-{:02}", start, end))
}

fn session_label_to_block(raw: &str) -> Option<String> {
    let label = raw
        .split('(')
        .next()
        .map(str::trim)
        .unwrap_or(raw)
        .replace(' ', "");
    let (start_label, end_label) = label.split_once('-')?;
    let start = parse_et_hour_label(start_label)?;
    let mut end = parse_et_hour_label(end_label)?;
    if end == 0 {
        end = 24;
    }
    Some(format!("{:02}-{:02}", start, end))
}

fn parse_et_hour_label(label: &str) -> Option<u32> {
    let compact = label.trim().replace(' ', "").to_ascii_uppercase();
    if compact.is_empty() {
        return None;
    }

    let (num_str, suffix) = if let Some(v) = compact.strip_suffix("AM") {
        (v, "AM")
    } else if let Some(v) = compact.strip_suffix("PM") {
        (v, "PM")
    } else {
        return None;
    };

    let raw_hour = num_str.parse::<u32>().ok()?;
    if !(1..=12).contains(&raw_hour) {
        return None;
    }

    let hour = match suffix {
        "AM" => {
            if raw_hour == 12 {
                0
            } else {
                raw_hour
            }
        }
        "PM" => {
            if raw_hour == 12 {
                12
            } else {
                raw_hour + 12
            }
        }
        _ => return None,
    };

    Some(hour)
}

fn parse_flips_n(raw: &str) -> Option<Plan3Cell> {
    let value = raw.trim();
    if value.is_empty() || value == "—" || value == "-" {
        return None;
    }

    let (flips_raw, n_raw) = value.split_once('/')?;
    let flips = flips_raw.trim().replace(',', "").parse::<u32>().ok()?;
    let n = n_raw.trim().replace(',', "").parse::<u32>().ok()?;
    if n == 0 {
        return None;
    }

    Some(Plan3Cell { flips, n })
}

fn parse_bin_range(raw: &str) -> Option<LeadBinRange> {
    let cleaned = raw
        .trim()
        .trim_end_matches('%')
        .replace(' ', "")
        .replace('\u{2212}', "-");

    if let Some(rest) = cleaned.strip_prefix('<') {
        let high = rest.parse::<f64>().ok()?;
        return Some(LeadBinRange {
            low_pct: 0.0,
            high_pct: high,
        });
    }

    if let Some(rest) = cleaned.strip_prefix('>') {
        let low = rest.parse::<f64>().ok()?;
        return Some(LeadBinRange {
            low_pct: low,
            high_pct: low,
        });
    }

    let (low_raw, high_raw) = cleaned.split_once('-')?;
    let low = low_raw.parse::<f64>().ok()?;
    let high = high_raw.parse::<f64>().ok()?;
    if !low.is_finite() || !high.is_finite() {
        return None;
    }

    Some(LeadBinRange {
        low_pct: low,
        high_pct: high,
    })
}

fn tau_label_to_sec(raw: &str) -> Option<i64> {
    let label = raw.trim().to_ascii_uppercase();
    let minutes = label.strip_prefix("T-")?.trim().parse::<i64>().ok()?;
    Some(minutes.max(0) * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_anchor_cells_match_plan3_reference() {
        let fixture = r#"
### BTC — 8AM-12PM (Session 3)
| Lead % | T-120 | T-30 | T-15 |
|---|---|---|---|
| 0-1 | 10/100 | 1/100 | 1/100 |
| 1-2 | 11/110 | 18/159 | 2/136 |
"#;
        let tables = Plan3Tables::from_markdown(fixture).expect("parse inline plan3 fixture");

        let t30 = tables
            .exact_cell(Timeframe::H4, "BTC", "08-12", 1_800, 1)
            .expect("btc h4 t30 cell");
        assert_eq!(t30.flips, 18);
        assert_eq!(t30.n, 159);

        let t15 = tables
            .exact_cell(Timeframe::H4, "BTC", "08-12", 900, 1)
            .expect("btc h4 t15 cell");
        assert_eq!(t15.flips, 2);
        assert_eq!(t15.n, 136);
    }

    #[test]
    fn group_key_mapping_is_dst_safe_shape() {
        // 2026-02-24 13:00:00 UTC => 08:00:00 ET (EST)
        let ts = 1_771_938_800_i64;
        let block = Plan3Tables::group_key_for_period_open(Timeframe::H4, ts).expect("block");
        assert_eq!(block, "08-12");
        let hour = Plan3Tables::group_key_for_period_open(Timeframe::H1, ts).expect("hour");
        assert_eq!(hour, "8");
    }
}
