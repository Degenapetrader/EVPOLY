use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::MmSportMarket;

const DATA_API_BASE_URL: &str = "https://data-api.polymarket.com";
pub const HOLDER_SNAPSHOT_REFRESH_MS: i64 = 10 * 60 * 1_000;
pub const HOLDER_SNAPSHOT_RETRY_MS: i64 = 60 * 1_000;
pub const WALLET_PROFILE_REFRESH_MS: i64 = 12 * 60 * 60 * 1_000;
pub const WALLET_PROFILE_RETRY_MS: i64 = 15 * 60 * 1_000;
pub const HOLDER_REFRESH_BUDGET_PER_LOOP: usize = 2;
pub const WALLET_PROFILE_REFRESH_BUDGET_PER_LOOP: usize = 4;
const HOLDER_SELECTION_MIN_COVERAGE: f64 = 0.70;
const HOLDER_SELECTION_MAX_PER_SIDE: usize = 10;
const PROFILE_PAGE_SIZE: usize = 100;
const PROFILE_PAGE_LIMIT: usize = 5;
const PRE_HALT_WINDOW_HOURS: f64 = 6.0;
const EXIT_WINDOW_HOURS: f64 = 1.0;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HolderSnapshotEntry {
    pub wallet_address: String,
    pub display_name: Option<String>,
    pub token_id: String,
    pub outcome: String,
    pub outcome_index: i64,
    pub shares: f64,
    pub side_share_pct: f64,
    pub holds_both_sides: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MarketHolderSnapshot {
    pub condition_id: String,
    pub market_slug: String,
    pub sport_key: String,
    pub fetched_at_ms: i64,
    pub up_total_shares: f64,
    pub down_total_shares: f64,
    pub up_selected_shares: f64,
    pub down_selected_shares: f64,
    pub entries: Vec<HolderSnapshotEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WalletSportProfile {
    pub wallet_address: String,
    pub sport_key: String,
    pub label: String,
    pub sample_count_7d: usize,
    pub sample_count_30d: usize,
    pub sports_market_count_7d: usize,
    pub sports_market_count_30d: usize,
    pub buy_count_7d: usize,
    pub sell_count_7d: usize,
    pub buy_count_30d: usize,
    pub sell_count_30d: usize,
    pub hold_to_settle_rate: f64,
    pub pre_halt_sell_rate: f64,
    pub exit_window_sell_rate: f64,
    pub same_event_both_sides_rate: f64,
    pub confidence: f64,
    pub last_profiled_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SideRiskAdjustment {
    pub entry_band_depth_shares: f64,
    pub expected_pre_halt_shares: f64,
    pub expected_exit_window_shares: f64,
    pub pre_halt_pressure: f64,
    pub exit_window_crowding: f64,
    pub top3_share_pct: f64,
    pub two_sided_share_pct: f64,
    pub profiled_share_pct: f64,
    pub profiled_holder_count: usize,
    pub selected_holder_count: usize,
    pub extra_entry_ticks: u32,
    pub size_multiplier: f64,
}

impl Default for SideRiskAdjustment {
    fn default() -> Self {
        Self {
            entry_band_depth_shares: 0.0,
            expected_pre_halt_shares: 0.0,
            expected_exit_window_shares: 0.0,
            pre_halt_pressure: 0.0,
            exit_window_crowding: 0.0,
            top3_share_pct: 0.0,
            two_sided_share_pct: 0.0,
            profiled_share_pct: 0.0,
            profiled_holder_count: 0,
            selected_holder_count: 0,
            extra_entry_ticks: 0,
            size_multiplier: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MarketRiskSnapshot {
    pub condition_id: String,
    pub market_slug: String,
    pub sport_key: String,
    pub computed_at_ms: i64,
    pub up: SideRiskAdjustment,
    pub down: SideRiskAdjustment,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct MarketPositionsByToken {
    #[serde(default)]
    token: String,
    #[serde(default)]
    positions: Vec<MarketPositionEntry>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct MarketPositionEntry {
    #[serde(rename = "proxyWallet", default)]
    proxy_wallet: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    size: f64,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(rename = "outcomeIndex", default)]
    outcome_index: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct DataTradeEntry {
    #[serde(default)]
    side: String,
    #[serde(rename = "conditionId", default)]
    condition_id: String,
    #[serde(default)]
    timestamp: i64,
    #[serde(default)]
    slug: Option<String>,
    #[serde(rename = "eventSlug", default)]
    event_slug: Option<String>,
    #[serde(rename = "outcomeIndex", default)]
    outcome_index: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ClosedPositionEntry {
    #[serde(rename = "conditionId", default)]
    condition_id: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(rename = "eventSlug", default)]
    event_slug: Option<String>,
    #[serde(rename = "endDate", default)]
    end_date: Option<String>,
    #[serde(default)]
    timestamp: i64,
}

#[derive(Debug, Clone, Default)]
struct SideSelection {
    entries: Vec<HolderSnapshotEntry>,
    total_shares: f64,
    selected_shares: f64,
}

#[derive(Debug, Clone, Default)]
struct ProfileTradeStats {
    unique_conditions_7d: HashSet<String>,
    unique_conditions_30d: HashSet<String>,
    buy_count_7d: usize,
    sell_count_7d: usize,
    buy_count_30d: usize,
    sell_count_30d: usize,
    two_sided_conditions_30d: usize,
}

fn data_api_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

async fn fetch_json<T>(path: &str, params: &[(&str, String)]) -> Result<T>
where
    T: DeserializeOwned + Default,
{
    let url = format!("{}{}", DATA_API_BASE_URL, path);
    let client = data_api_http_client();
    let response = client
        .get(url.as_str())
        .query(params)
        .send()
        .await
        .with_context(|| format!("holder intel request failed for {}", path))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status == StatusCode::NOT_FOUND {
        return Ok(T::default());
    }
    if !status.is_success() {
        anyhow::bail!(
            "holder intel request {} failed status={} body={}",
            path,
            status,
            body.chars().take(240).collect::<String>()
        );
    }
    serde_json::from_str::<T>(body.as_str())
        .with_context(|| format!("holder intel failed to parse {}", path))
}

pub fn sport_key_from_slug(slug: &str) -> String {
    slug.trim()
        .split('-')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

pub fn wallet_profile_cache_key(wallet_address: &str, sport_key: &str) -> String {
    format!(
        "{}:{}",
        wallet_address.trim().to_ascii_lowercase(),
        sport_key.trim().to_ascii_lowercase()
    )
}

fn side_entries_for_token(
    groups: &[MarketPositionsByToken],
    token_id: &str,
) -> Option<Vec<MarketPositionEntry>> {
    let token_id = token_id.trim();
    groups
        .iter()
        .find(|group| group.token.trim().eq_ignore_ascii_case(token_id))
        .map(|group| group.positions.clone())
}

fn select_side_entries(token_id: &str, entries: &[MarketPositionEntry]) -> SideSelection {
    let mut sorted = entries
        .iter()
        .filter(|row| row.size.is_finite() && row.size > 0.0)
        .cloned()
        .collect::<Vec<_>>();
    sorted.sort_by(|a, b| {
        b.size
            .partial_cmp(&a.size)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let total_shares = sorted
        .iter()
        .map(|row| row.size.max(0.0))
        .fold(0.0, |acc, v| acc + v);
    let mut selected = Vec::new();
    let mut selected_shares = 0.0;
    for row in sorted {
        if selected.len() >= HOLDER_SELECTION_MAX_PER_SIDE
            && (selected_shares / total_shares.max(1.0)) >= HOLDER_SELECTION_MIN_COVERAGE
        {
            break;
        }
        let shares = row.size.max(0.0);
        if shares <= 0.0 {
            continue;
        }
        selected_shares += shares;
        selected.push(HolderSnapshotEntry {
            wallet_address: row.proxy_wallet.trim().to_ascii_lowercase(),
            display_name: row
                .name
                .as_ref()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            token_id: token_id.to_string(),
            outcome: row.outcome.unwrap_or_default(),
            outcome_index: row.outcome_index.unwrap_or_default(),
            shares,
            side_share_pct: if total_shares > 0.0 {
                shares / total_shares
            } else {
                0.0
            },
            holds_both_sides: false,
        });
    }
    SideSelection {
        entries: selected,
        total_shares,
        selected_shares,
    }
}

pub async fn fetch_holder_snapshot_for_market(
    market: &MmSportMarket,
    now_ms: i64,
) -> Result<Option<MarketHolderSnapshot>> {
    let params = vec![
        ("market", market.condition_id.clone()),
        ("status", "OPEN".to_string()),
        ("sortBy", "TOKENS".to_string()),
        ("sortDirection", "DESC".to_string()),
        ("limit", "500".to_string()),
        ("offset", "0".to_string()),
    ];
    let groups: Vec<MarketPositionsByToken> =
        fetch_json("/v1/market-positions", params.as_slice()).await?;
    if groups.is_empty() {
        return Ok(None);
    }
    let Some(up_rows) = side_entries_for_token(groups.as_slice(), market.up_token_id.as_str())
    else {
        return Ok(None);
    };
    let Some(down_rows) = side_entries_for_token(groups.as_slice(), market.down_token_id.as_str())
    else {
        return Ok(None);
    };
    let mut up = select_side_entries(market.up_token_id.as_str(), up_rows.as_slice());
    let mut down = select_side_entries(market.down_token_id.as_str(), down_rows.as_slice());
    let up_wallets = up
        .entries
        .iter()
        .map(|entry| entry.wallet_address.clone())
        .collect::<HashSet<_>>();
    let down_wallets = down
        .entries
        .iter()
        .map(|entry| entry.wallet_address.clone())
        .collect::<HashSet<_>>();
    let both_sides = up_wallets
        .intersection(&down_wallets)
        .cloned()
        .collect::<HashSet<_>>();
    for entry in &mut up.entries {
        entry.holds_both_sides = both_sides.contains(entry.wallet_address.as_str());
    }
    for entry in &mut down.entries {
        entry.holds_both_sides = both_sides.contains(entry.wallet_address.as_str());
    }
    let mut entries = up.entries;
    entries.extend(down.entries);
    Ok(Some(MarketHolderSnapshot {
        condition_id: market.condition_id.clone(),
        market_slug: market.market_slug.clone(),
        sport_key: sport_key_from_slug(market.market_slug.as_str()),
        fetched_at_ms: now_ms,
        up_total_shares: up.total_shares,
        down_total_shares: down.total_shares,
        up_selected_shares: up.selected_shares,
        down_selected_shares: down.selected_shares,
        entries,
    }))
}

fn event_slug_for_trade(trade: &DataTradeEntry) -> String {
    trade
        .event_slug
        .as_ref()
        .or(trade.slug.as_ref())
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

fn event_slug_for_closed_position(position: &ClosedPositionEntry) -> String {
    position
        .event_slug
        .as_ref()
        .or(position.slug.as_ref())
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

fn belongs_to_sport(slug: &str, sport_key: &str) -> bool {
    let row_sport = sport_key_from_slug(slug);
    !row_sport.is_empty() && row_sport == sport_key
}

async fn fetch_wallet_trades(wallet_address: &str) -> Result<Vec<DataTradeEntry>> {
    let mut out = Vec::new();
    let now_ms = Utc::now().timestamp_millis();
    let cutoff_ms = now_ms.saturating_sub(30 * 24 * 60 * 60 * 1_000);
    for page in 0..PROFILE_PAGE_LIMIT {
        let offset = page * PROFILE_PAGE_SIZE;
        let params = vec![
            ("user", wallet_address.to_string()),
            ("limit", PROFILE_PAGE_SIZE.to_string()),
            ("offset", offset.to_string()),
            ("takerOnly", "false".to_string()),
        ];
        let batch: Vec<DataTradeEntry> = fetch_json("/trades", params.as_slice()).await?;
        if batch.is_empty() {
            break;
        }
        let mut reached_cutoff = false;
        for row in batch.iter().cloned() {
            let ts_ms = row.timestamp.saturating_mul(1_000);
            if ts_ms > 0 && ts_ms < cutoff_ms {
                reached_cutoff = true;
                break;
            }
            out.push(row);
        }
        if batch.len() < PROFILE_PAGE_SIZE || reached_cutoff {
            break;
        }
    }
    Ok(out)
}

async fn fetch_wallet_closed_positions(wallet_address: &str) -> Result<Vec<ClosedPositionEntry>> {
    let mut out = Vec::new();
    let now_ms = Utc::now().timestamp_millis();
    let cutoff_ms = now_ms.saturating_sub(30 * 24 * 60 * 60 * 1_000);
    for page in 0..PROFILE_PAGE_LIMIT {
        let offset = page * PROFILE_PAGE_SIZE;
        let params = vec![
            ("user", wallet_address.to_string()),
            ("limit", PROFILE_PAGE_SIZE.to_string()),
            ("offset", offset.to_string()),
        ];
        let batch: Vec<ClosedPositionEntry> =
            fetch_json("/closed-positions", params.as_slice()).await?;
        if batch.is_empty() {
            break;
        }
        let mut reached_cutoff = false;
        for row in batch.iter().cloned() {
            let ts_ms = row.timestamp.saturating_mul(1_000);
            if ts_ms > 0 && ts_ms < cutoff_ms {
                reached_cutoff = true;
                break;
            }
            out.push(row);
        }
        if batch.len() < PROFILE_PAGE_SIZE || reached_cutoff {
            break;
        }
    }
    Ok(out)
}

fn closed_position_offset_hours(row: &ClosedPositionEntry) -> Option<f64> {
    let close_ts_ms = row.timestamp.saturating_mul(1_000);
    if close_ts_ms <= 0 {
        return None;
    }
    let end_iso = row.end_date.as_ref()?.trim();
    if end_iso.is_empty() {
        return None;
    }
    let end_dt = DateTime::parse_from_rfc3339(end_iso).ok()?;
    let end_ts_ms = end_dt.with_timezone(&Utc).timestamp_millis();
    Some((end_ts_ms.saturating_sub(close_ts_ms) as f64) / 3_600_000.0)
}

fn classify_wallet_label(
    sample_count_30d: usize,
    sports_market_count_30d: usize,
    hold_to_settle_rate: f64,
    pre_halt_sell_rate: f64,
    exit_window_sell_rate: f64,
    same_event_both_sides_rate: f64,
) -> String {
    if sample_count_30d < 3 && sports_market_count_30d < 6 {
        return "unknown".to_string();
    }
    if same_event_both_sides_rate >= 0.30 {
        return "two_sided_farmer".to_string();
    }
    if pre_halt_sell_rate >= 0.35 || exit_window_sell_rate >= 0.20 {
        return "prestart_seller".to_string();
    }
    if hold_to_settle_rate >= 0.60 && pre_halt_sell_rate < 0.20 && exit_window_sell_rate < 0.10 {
        return "settle_holder".to_string();
    }
    "mixed".to_string()
}

fn build_trade_stats(
    trades: &[DataTradeEntry],
    sport_key: &str,
    cutoff_7d_ms: i64,
) -> ProfileTradeStats {
    let mut stats = ProfileTradeStats::default();
    let mut outcomes_by_condition: HashMap<String, HashSet<i64>> = HashMap::new();
    for row in trades {
        let slug = event_slug_for_trade(row);
        if slug.is_empty() || !belongs_to_sport(slug.as_str(), sport_key) {
            continue;
        }
        let condition_id = row.condition_id.trim().to_ascii_lowercase();
        if condition_id.is_empty() {
            continue;
        }
        let ts_ms = row.timestamp.saturating_mul(1_000);
        let side = row.side.trim().to_ascii_uppercase();
        if ts_ms >= cutoff_7d_ms {
            stats.unique_conditions_7d.insert(condition_id.clone());
            if side == "SELL" {
                stats.sell_count_7d = stats.sell_count_7d.saturating_add(1);
            } else if side == "BUY" {
                stats.buy_count_7d = stats.buy_count_7d.saturating_add(1);
            }
        }
        stats.unique_conditions_30d.insert(condition_id.clone());
        if side == "SELL" {
            stats.sell_count_30d = stats.sell_count_30d.saturating_add(1);
        } else if side == "BUY" {
            stats.buy_count_30d = stats.buy_count_30d.saturating_add(1);
        }
        if let Some(outcome_index) = row.outcome_index {
            outcomes_by_condition
                .entry(condition_id)
                .or_default()
                .insert(outcome_index);
        }
    }
    stats.two_sided_conditions_30d = outcomes_by_condition
        .values()
        .filter(|outcomes| outcomes.len() >= 2)
        .count();
    stats
}

pub async fn fetch_wallet_profile(
    wallet_address: &str,
    sport_key: &str,
    now_ms: i64,
) -> Result<Option<WalletSportProfile>> {
    if wallet_address.trim().is_empty() || sport_key.trim().is_empty() {
        return Ok(None);
    }
    let wallet_address = wallet_address.trim().to_ascii_lowercase();
    let sport_key = sport_key.trim().to_ascii_lowercase();
    let trades = fetch_wallet_trades(wallet_address.as_str()).await?;
    let closed_positions = fetch_wallet_closed_positions(wallet_address.as_str()).await?;
    let cutoff_7d_ms = now_ms.saturating_sub(7 * 24 * 60 * 60 * 1_000);
    let cutoff_30d_ms = now_ms.saturating_sub(30 * 24 * 60 * 60 * 1_000);
    let trade_stats = build_trade_stats(trades.as_slice(), sport_key.as_str(), cutoff_7d_ms);

    let mut sample_count_7d = 0usize;
    let mut sample_count_30d = 0usize;
    let mut settle_count = 0usize;
    let mut pre_halt_count = 0usize;
    let mut exit_window_count = 0usize;
    let mut sports_closed_7d: HashSet<String> = HashSet::new();
    let mut sports_closed_30d: HashSet<String> = HashSet::new();

    for row in &closed_positions {
        let slug = event_slug_for_closed_position(row);
        if slug.is_empty() || !belongs_to_sport(slug.as_str(), sport_key.as_str()) {
            continue;
        }
        let condition_id = row.condition_id.trim().to_ascii_lowercase();
        if condition_id.is_empty() {
            continue;
        }
        let close_ts_ms = row.timestamp.saturating_mul(1_000);
        if close_ts_ms < cutoff_30d_ms {
            continue;
        }
        sample_count_30d = sample_count_30d.saturating_add(1);
        sports_closed_30d.insert(condition_id.clone());
        if close_ts_ms >= cutoff_7d_ms {
            sample_count_7d = sample_count_7d.saturating_add(1);
            sports_closed_7d.insert(condition_id.clone());
        }
        if let Some(offset_hours) = closed_position_offset_hours(row) {
            if offset_hours <= 0.25 {
                settle_count = settle_count.saturating_add(1);
            } else if offset_hours > 0.0 && offset_hours <= EXIT_WINDOW_HOURS {
                exit_window_count = exit_window_count.saturating_add(1);
            } else if offset_hours > EXIT_WINDOW_HOURS && offset_hours <= PRE_HALT_WINDOW_HOURS {
                pre_halt_count = pre_halt_count.saturating_add(1);
            }
        }
    }

    let sample_count_base = sample_count_30d.max(1) as f64;
    let sell_trade_rate_30d = if trade_stats.buy_count_30d + trade_stats.sell_count_30d > 0 {
        trade_stats.sell_count_30d as f64
            / (trade_stats.buy_count_30d + trade_stats.sell_count_30d) as f64
    } else {
        0.0
    };
    let hold_to_settle_rate = if sample_count_30d > 0 {
        settle_count as f64 / sample_count_base
    } else {
        0.0
    };
    let mut pre_halt_sell_rate = if sample_count_30d > 0 {
        pre_halt_count as f64 / sample_count_base
    } else {
        0.0
    };
    let mut exit_window_sell_rate = if sample_count_30d > 0 {
        exit_window_count as f64 / sample_count_base
    } else {
        0.0
    };
    if sample_count_30d == 0 && (trade_stats.buy_count_30d + trade_stats.sell_count_30d) >= 20 {
        pre_halt_sell_rate = (sell_trade_rate_30d * 0.25).clamp(0.0, 0.35);
        exit_window_sell_rate = (sell_trade_rate_30d * 0.15).clamp(0.0, 0.20);
    }
    let same_event_both_sides_rate = if !trade_stats.unique_conditions_30d.is_empty() {
        trade_stats.two_sided_conditions_30d as f64 / trade_stats.unique_conditions_30d.len() as f64
    } else {
        0.0
    };
    let confidence = if sample_count_30d > 0 {
        (sample_count_30d as f64 / 8.0).clamp(0.25, 1.0)
    } else if !trade_stats.unique_conditions_30d.is_empty() {
        (trade_stats.unique_conditions_30d.len() as f64 / 12.0).clamp(0.15, 0.6)
    } else {
        0.0
    };
    let sports_market_count_7d = sports_closed_7d
        .union(&trade_stats.unique_conditions_7d)
        .count();
    let sports_market_count_30d = sports_closed_30d
        .union(&trade_stats.unique_conditions_30d)
        .count();
    let label = classify_wallet_label(
        sample_count_30d,
        sports_market_count_30d,
        hold_to_settle_rate,
        pre_halt_sell_rate,
        exit_window_sell_rate,
        same_event_both_sides_rate,
    );

    Ok(Some(WalletSportProfile {
        wallet_address,
        sport_key,
        label,
        sample_count_7d,
        sample_count_30d,
        sports_market_count_7d,
        sports_market_count_30d,
        buy_count_7d: trade_stats.buy_count_7d,
        sell_count_7d: trade_stats.sell_count_7d,
        buy_count_30d: trade_stats.buy_count_30d,
        sell_count_30d: trade_stats.sell_count_30d,
        hold_to_settle_rate,
        pre_halt_sell_rate,
        exit_window_sell_rate,
        same_event_both_sides_rate,
        confidence,
        last_profiled_at_ms: now_ms,
    }))
}

fn expected_sell_probability(profile: &WalletSportProfile, holds_both_sides: bool) -> (f64, f64) {
    let base_pre = profile.pre_halt_sell_rate.clamp(0.0, 1.0);
    let base_exit = profile.exit_window_sell_rate.clamp(0.0, 1.0);
    let two_sided_boost = if holds_both_sides { 0.15 } else { 0.0 };
    let profile_boost = (profile.same_event_both_sides_rate * 0.20).clamp(0.0, 0.20);
    let settle_discount = (1.0 - (profile.hold_to_settle_rate * 0.35)).clamp(0.55, 1.0);
    let confidence = profile.confidence.clamp(0.0, 1.0);
    let pre = ((base_pre + two_sided_boost + profile_boost) * settle_discount * confidence)
        .clamp(0.0, 1.0);
    let exit =
        ((base_exit + (two_sided_boost * 0.75) + profile_boost) * settle_discount * confidence)
            .clamp(0.0, 1.0);
    (pre, exit)
}

fn map_pressure_to_quote_adjustment(pressure: f64) -> (u32, f64) {
    if !pressure.is_finite() || pressure < 0.5 {
        (0, 1.0)
    } else if pressure < 1.0 {
        (1, 0.85)
    } else if pressure < 2.0 {
        (2, 0.65)
    } else {
        (4, 0.40)
    }
}

fn compute_side_risk(
    market: &MmSportMarket,
    entries: &[HolderSnapshotEntry],
    side_total_shares: f64,
    side_selected_shares: f64,
    entry_band_depth_shares: f64,
    now_ms: i64,
    profile_by_key: &HashMap<String, WalletSportProfile>,
) -> SideRiskAdjustment {
    let mut ordered = entries.to_vec();
    ordered.sort_by(|a, b| {
        b.shares
            .partial_cmp(&a.shares)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top3_shares = ordered
        .iter()
        .take(3)
        .map(|entry| entry.shares)
        .sum::<f64>();
    let two_sided_shares = ordered
        .iter()
        .filter(|entry| entry.holds_both_sides)
        .map(|entry| entry.shares)
        .sum::<f64>();
    let selected_holder_count = ordered.len();
    let quote_halt_ts_ms = market
        .game_start_ts_ms
        .saturating_sub(EXIT_WINDOW_HOURS as i64 * 3_600_000);
    let before_quote_halt = quote_halt_ts_ms > now_ms;
    let sport_key = sport_key_from_slug(market.market_slug.as_str());

    let mut expected_pre_halt_shares = 0.0;
    let mut expected_exit_window_shares = 0.0;
    let mut profiled_shares = 0.0;
    let mut profiled_holder_count = 0usize;

    for entry in &ordered {
        let profile_key =
            wallet_profile_cache_key(entry.wallet_address.as_str(), sport_key.as_str());
        let Some(profile) = profile_by_key.get(profile_key.as_str()) else {
            continue;
        };
        let (pre_prob, exit_prob) = expected_sell_probability(profile, entry.holds_both_sides);
        profiled_holder_count = profiled_holder_count.saturating_add(1);
        profiled_shares += entry.shares;
        if before_quote_halt {
            expected_pre_halt_shares += entry.shares * pre_prob;
        }
        expected_exit_window_shares += entry.shares * exit_prob;
    }

    let profiled_share_pct = if side_total_shares > 0.0 {
        profiled_shares / side_total_shares
    } else {
        0.0
    };
    let selected_coverage_pct = if side_total_shares > 0.0 {
        side_selected_shares / side_total_shares
    } else {
        0.0
    };
    let profile_coverage_factor =
        (0.25 + (profiled_shares / side_selected_shares.max(1.0)) * 0.75).clamp(0.25, 1.0);
    let selected_coverage_factor = selected_coverage_pct.clamp(0.30, 1.0);
    let concentration_boost = if side_total_shares > 0.0 && (top3_shares / side_total_shares) > 0.45
    {
        1.10
    } else {
        1.0
    };
    let two_sided_boost =
        if side_total_shares > 0.0 && (two_sided_shares / side_total_shares) > 0.15 {
            1.10
        } else {
            1.0
        };
    let depth_base = entry_band_depth_shares.max(1.0);
    let pre_halt_pressure = if before_quote_halt {
        (expected_pre_halt_shares / depth_base)
            * profile_coverage_factor
            * selected_coverage_factor
            * concentration_boost
            * two_sided_boost
    } else {
        0.0
    };
    let exit_window_crowding = (expected_exit_window_shares / depth_base)
        * profile_coverage_factor
        * selected_coverage_factor
        * concentration_boost
        * two_sided_boost;
    let (extra_entry_ticks, size_multiplier) = map_pressure_to_quote_adjustment(pre_halt_pressure);

    SideRiskAdjustment {
        entry_band_depth_shares,
        expected_pre_halt_shares,
        expected_exit_window_shares,
        pre_halt_pressure,
        exit_window_crowding,
        top3_share_pct: if side_total_shares > 0.0 {
            top3_shares / side_total_shares
        } else {
            0.0
        },
        two_sided_share_pct: if side_total_shares > 0.0 {
            two_sided_shares / side_total_shares
        } else {
            0.0
        },
        profiled_share_pct,
        profiled_holder_count,
        selected_holder_count,
        extra_entry_ticks,
        size_multiplier,
    }
}

pub fn compute_market_risk(
    market: &MmSportMarket,
    snapshot: &MarketHolderSnapshot,
    profile_by_key: &HashMap<String, WalletSportProfile>,
    up_entry_band_depth_shares: f64,
    down_entry_band_depth_shares: f64,
    now_ms: i64,
) -> MarketRiskSnapshot {
    let up_entries = snapshot
        .entries
        .iter()
        .filter(|entry| {
            entry
                .token_id
                .eq_ignore_ascii_case(market.up_token_id.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    let down_entries = snapshot
        .entries
        .iter()
        .filter(|entry| {
            entry
                .token_id
                .eq_ignore_ascii_case(market.down_token_id.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    MarketRiskSnapshot {
        condition_id: market.condition_id.clone(),
        market_slug: market.market_slug.clone(),
        sport_key: snapshot.sport_key.clone(),
        computed_at_ms: now_ms,
        up: compute_side_risk(
            market,
            up_entries.as_slice(),
            snapshot.up_total_shares,
            snapshot.up_selected_shares,
            up_entry_band_depth_shares,
            now_ms,
            profile_by_key,
        ),
        down: compute_side_risk(
            market,
            down_entries.as_slice(),
            snapshot.down_total_shares,
            snapshot.down_selected_shares,
            down_entry_band_depth_shares,
            now_ms,
            profile_by_key,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_market() -> MmSportMarket {
        MmSportMarket {
            condition_id: "0xcond".to_string(),
            market_slug: "nba-test-game-2026-04-07".to_string(),
            is_sports_market: true,
            polymarket_sports_game_id: None,
            polymarket_sports_event_slug: None,
            polymarket_sports_event_state: None,
            reward_rate_per_day: 500.0,
            reward_min_size_shares: 1000.0,
            reward_max_spread: 0.03,
            minimum_tick_size: 0.01,
            minimum_order_size_usd: 1.0,
            game_start_ts_ms: Utc::now().timestamp_millis() + (4 * 3_600_000),
            period_timestamp: 0,
            up_token_id: "up".to_string(),
            down_token_id: "down".to_string(),
        }
    }

    #[test]
    fn sport_key_uses_slug_prefix() {
        assert_eq!(sport_key_from_slug("nba-okc-lal-2026-04-07"), "nba");
    }

    #[test]
    fn pressure_mapping_scales_quotes() {
        assert_eq!(map_pressure_to_quote_adjustment(0.4), (0, 1.0));
        assert_eq!(map_pressure_to_quote_adjustment(0.8), (1, 0.85));
        assert_eq!(map_pressure_to_quote_adjustment(1.5), (2, 0.65));
        assert_eq!(map_pressure_to_quote_adjustment(3.0), (4, 0.40));
    }

    #[test]
    fn default_side_risk_is_neutral() {
        let risk = SideRiskAdjustment::default();
        assert_eq!(risk.extra_entry_ticks, 0);
        assert_eq!(risk.size_multiplier, 1.0);
    }

    #[test]
    fn market_risk_uses_profiled_holder_behavior() {
        let market = test_market();
        let snapshot = MarketHolderSnapshot {
            condition_id: market.condition_id.clone(),
            market_slug: market.market_slug.clone(),
            sport_key: "nba".to_string(),
            fetched_at_ms: Utc::now().timestamp_millis(),
            up_total_shares: 10_000.0,
            down_total_shares: 8_000.0,
            up_selected_shares: 9_000.0,
            down_selected_shares: 7_000.0,
            entries: vec![HolderSnapshotEntry {
                wallet_address: "0xwallet".to_string(),
                display_name: Some("farmer".to_string()),
                token_id: market.up_token_id.clone(),
                outcome: "UP".to_string(),
                outcome_index: 0,
                shares: 4_000.0,
                side_share_pct: 0.4,
                holds_both_sides: true,
            }],
        };
        let mut profiles = HashMap::new();
        profiles.insert(
            wallet_profile_cache_key("0xwallet", "nba"),
            WalletSportProfile {
                wallet_address: "0xwallet".to_string(),
                sport_key: "nba".to_string(),
                label: "two_sided_farmer".to_string(),
                sample_count_7d: 5,
                sample_count_30d: 8,
                sports_market_count_7d: 5,
                sports_market_count_30d: 8,
                buy_count_7d: 20,
                sell_count_7d: 10,
                buy_count_30d: 40,
                sell_count_30d: 20,
                hold_to_settle_rate: 0.1,
                pre_halt_sell_rate: 0.5,
                exit_window_sell_rate: 0.3,
                same_event_both_sides_rate: 0.5,
                confidence: 1.0,
                last_profiled_at_ms: Utc::now().timestamp_millis(),
            },
        );
        let risk = compute_market_risk(
            &market,
            &snapshot,
            &profiles,
            1_000.0,
            1_000.0,
            Utc::now().timestamp_millis(),
        );
        assert!(risk.up.pre_halt_pressure > 0.5);
        assert!(risk.up.extra_entry_ticks >= 1);
        assert!(risk.up.size_multiplier < 1.0);
    }
}
