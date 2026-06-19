use crate::strategy::Direction;
use futures_util::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::Duration;

const DEFAULT_TICK_BANDS_MS: &[i64] = &[2_000, 1_000, 100];
const BUCKETS_BPS: &[u16] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 35, 40, 45, 50, 55, 60, 65, 70, 75, 80, 85, 90, 95, 100, 110, 120, 130, 140,
    150, 160, 170, 180, 190, 200,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndgameCexVenue {
    BinanceSpot,
    BinanceFutures,
}

impl EndgameCexVenue {
    pub fn as_str(self) -> &'static str {
        match self {
            EndgameCexVenue::BinanceSpot => "binance_spot",
            EndgameCexVenue::BinanceFutures => "binance_futures",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EndgameCexDepthLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone)]
pub struct EndgameCexDepthSnapshot {
    pub symbol: String,
    pub venue: EndgameCexVenue,
    pub best_bid: f64,
    pub best_ask: f64,
    pub bids: Vec<EndgameCexDepthLevel>,
    pub asks: Vec<EndgameCexDepthLevel>,
    pub updated_ms: i64,
}

impl EndgameCexDepthSnapshot {
    pub fn mid(&self) -> f64 {
        (self.best_bid + self.best_ask) / 2.0
    }

    pub fn age_ms(&self, now_ms: i64) -> i64 {
        now_ms.saturating_sub(self.updated_ms).max(0)
    }
}

#[derive(Debug, Clone)]
pub struct EndgameCexDepthDecision {
    pub enabled: bool,
    pub fail_open: bool,
    pub reason: String,
    pub venue: Option<&'static str>,
    pub tick_band_ms: i64,
    pub max_age_ms: i64,
    pub snapshot_age_ms: Option<i64>,
    pub cex_mid: Option<f64>,
    pub boundary_price: Option<f64>,
    pub distance_bps: Option<f64>,
    pub bucket_bps: Option<u16>,
    pub cost_to_boundary_usd: Option<f64>,
    pub threshold_usd: Option<f64>,
    pub multiplier: f64,
}

impl EndgameCexDepthDecision {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            fail_open: true,
            reason: "disabled".to_string(),
            venue: None,
            tick_band_ms: 0,
            max_age_ms: 0,
            snapshot_age_ms: None,
            cex_mid: None,
            boundary_price: None,
            distance_bps: None,
            bucket_bps: None,
            cost_to_boundary_usd: None,
            threshold_usd: None,
            multiplier: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EndgameCexDepthConfig {
    pub enabled: bool,
    pub size_increase_enabled: bool,
    pub max_reduction_pct: f64,
    pub tick_bands_ms: Vec<i64>,
    pub t100_max_age_ms: i64,
    pub t1000_max_age_ms: i64,
    pub t2000_max_age_ms: i64,
    pub t100_threshold_usd: f64,
    pub t1000_threshold_usd: f64,
    pub t2000_threshold_usd: f64,
    pub poll_ms: u64,
    pub fetch_timeout_ms: u64,
    pub binance_limit: u16,
}

impl EndgameCexDepthConfig {
    pub fn from_env() -> Self {
        Self {
            enabled: env_bool("EVPOLY_ENDGAME_CEX_DEPTH_ENABLE", true),
            size_increase_enabled: env_bool("EVPOLY_ENDGAME_CEX_DEPTH_SIZE_INCREASE_ENABLE", false),
            max_reduction_pct: env_f64("EVPOLY_ENDGAME_CEX_DEPTH_MAX_REDUCTION_PCT", 50.0)
                .clamp(0.0, 100.0),
            tick_bands_ms: parse_tick_bands_ms(
                std::env::var("EVPOLY_ENDGAME_CEX_DEPTH_TICK_BANDS_MS")
                    .unwrap_or_else(|_| "2000,1000,100".to_string())
                    .as_str(),
            ),
            t100_max_age_ms: env_i64("EVPOLY_ENDGAME_CEX_DEPTH_T100_MAX_AGE_MS", 250)
                .clamp(25, 5_000),
            t1000_max_age_ms: env_i64("EVPOLY_ENDGAME_CEX_DEPTH_T1000_MAX_AGE_MS", 500)
                .clamp(25, 5_000),
            t2000_max_age_ms: env_i64("EVPOLY_ENDGAME_CEX_DEPTH_T2000_MAX_AGE_MS", 750)
                .clamp(25, 5_000),
            t100_threshold_usd: env_f64("EVPOLY_ENDGAME_CEX_DEPTH_T100_THRESHOLD_USD", 50_000.0)
                .max(0.0),
            t1000_threshold_usd: env_f64("EVPOLY_ENDGAME_CEX_DEPTH_T1000_THRESHOLD_USD", 25_000.0)
                .max(0.0),
            t2000_threshold_usd: env_f64("EVPOLY_ENDGAME_CEX_DEPTH_T2000_THRESHOLD_USD", 10_000.0)
                .max(0.0),
            poll_ms: env_u64("EVPOLY_ENDGAME_CEX_DEPTH_POLL_MS", 100).clamp(25, 10_000),
            fetch_timeout_ms: env_u64("EVPOLY_ENDGAME_CEX_DEPTH_FETCH_TIMEOUT_MS", 350)
                .clamp(25, 5_000),
            binance_limit: env_u64("EVPOLY_ENDGAME_CEX_DEPTH_BINANCE_LIMIT", 100).clamp(20, 1000)
                as u16,
        }
    }

    pub fn band_for_tau_ms(&self, tau_ms: i64) -> i64 {
        band_for_tau_ms_with_bands(tau_ms, self.tick_bands_ms.as_slice())
    }

    pub fn max_age_ms_for_band(&self, band_ms: i64) -> i64 {
        if band_ms <= 100 {
            self.t100_max_age_ms
        } else if band_ms <= 1_000 {
            self.t1000_max_age_ms
        } else {
            self.t2000_max_age_ms
        }
    }

    pub fn threshold_usd_for_band(&self, band_ms: i64) -> f64 {
        if band_ms <= 100 {
            self.t100_threshold_usd
        } else if band_ms <= 1_000 {
            self.t1000_threshold_usd
        } else {
            self.t2000_threshold_usd
        }
    }

    pub fn max_reduction_for_band(&self, band_ms: i64) -> f64 {
        let cap = (self.max_reduction_pct / 100.0).clamp(0.0, 1.0);
        let band_scale = if band_ms <= 100 {
            1.0
        } else if band_ms <= 1_000 {
            0.7
        } else {
            0.4
        };
        (cap * band_scale).clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct EndgameCexDepthCache {
    inner: Arc<StdRwLock<HashMap<String, EndgameCexDepthSnapshot>>>,
}

impl EndgameCexDepthCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&self, snapshot: EndgameCexDepthSnapshot) {
        if snapshot.symbol.trim().is_empty()
            || !snapshot.best_bid.is_finite()
            || !snapshot.best_ask.is_finite()
            || snapshot.best_bid <= 0.0
            || snapshot.best_ask <= snapshot.best_bid
        {
            return;
        }
        if let Ok(mut inner) = self.inner.write() {
            inner.insert(normalize_symbol(snapshot.symbol.as_str()), snapshot);
        }
    }

    pub fn latest(&self, symbol: &str) -> Option<EndgameCexDepthSnapshot> {
        self.inner
            .read()
            .ok()
            .and_then(|inner| inner.get(normalize_symbol(symbol).as_str()).cloned())
    }
}

pub fn evaluate_cex_depth(
    cfg: &EndgameCexDepthConfig,
    cache: &EndgameCexDepthCache,
    symbol: &str,
    direction: Direction,
    boundary_price: f64,
    tau_ms: i64,
    now_ms: i64,
) -> EndgameCexDepthDecision {
    if !cfg.enabled {
        return EndgameCexDepthDecision::disabled();
    }
    let band_ms = cfg.band_for_tau_ms(tau_ms);
    let max_age_ms = cfg.max_age_ms_for_band(band_ms);
    let threshold_usd = cfg.threshold_usd_for_band(band_ms);
    if !boundary_price.is_finite() || boundary_price <= 0.0 {
        return EndgameCexDepthDecision {
            enabled: true,
            fail_open: true,
            reason: "boundary_missing".to_string(),
            venue: None,
            tick_band_ms: band_ms,
            max_age_ms,
            snapshot_age_ms: None,
            cex_mid: None,
            boundary_price: None,
            distance_bps: None,
            bucket_bps: None,
            cost_to_boundary_usd: None,
            threshold_usd: Some(threshold_usd),
            multiplier: 1.0,
        };
    }
    let Some(snapshot) = cache.latest(symbol) else {
        return EndgameCexDepthDecision {
            enabled: true,
            fail_open: true,
            reason: "missing_snapshot".to_string(),
            venue: None,
            tick_band_ms: band_ms,
            max_age_ms,
            snapshot_age_ms: None,
            cex_mid: None,
            boundary_price: Some(boundary_price),
            distance_bps: None,
            bucket_bps: None,
            cost_to_boundary_usd: None,
            threshold_usd: Some(threshold_usd),
            multiplier: 1.0,
        };
    };
    let age_ms = snapshot.age_ms(now_ms);
    let mid = snapshot.mid();
    if age_ms > max_age_ms {
        return EndgameCexDepthDecision {
            enabled: true,
            fail_open: true,
            reason: "stale_snapshot".to_string(),
            venue: Some(snapshot.venue.as_str()),
            tick_band_ms: band_ms,
            max_age_ms,
            snapshot_age_ms: Some(age_ms),
            cex_mid: Some(mid),
            boundary_price: Some(boundary_price),
            distance_bps: None,
            bucket_bps: None,
            cost_to_boundary_usd: None,
            threshold_usd: Some(threshold_usd),
            multiplier: 1.0,
        };
    }

    let ((cost, book_reached), boundary_crossed, distance_bps) = match direction {
        Direction::Up => (
            cost_to_sell_down_to(snapshot.bids.as_slice(), boundary_price),
            boundary_price < mid,
            ((mid / boundary_price) - 1.0).max(0.0) * 10_000.0,
        ),
        Direction::Down => (
            cost_to_buy_up_to(snapshot.asks.as_slice(), boundary_price),
            boundary_price > mid,
            ((boundary_price / mid) - 1.0).max(0.0) * 10_000.0,
        ),
    };
    let reached = boundary_crossed && book_reached;
    let bucket_bps = bucket_for_actual_bps(distance_bps);
    if !reached || !cost.is_finite() {
        return EndgameCexDepthDecision {
            enabled: true,
            fail_open: true,
            reason: "flip_cost_unavailable".to_string(),
            venue: Some(snapshot.venue.as_str()),
            tick_band_ms: band_ms,
            max_age_ms,
            snapshot_age_ms: Some(age_ms),
            cex_mid: Some(mid),
            boundary_price: Some(boundary_price),
            distance_bps: Some(distance_bps),
            bucket_bps: Some(bucket_bps),
            cost_to_boundary_usd: None,
            threshold_usd: Some(threshold_usd),
            multiplier: 1.0,
        };
    }

    let reduction = reduction_for_cost(cost, threshold_usd, cfg.max_reduction_for_band(band_ms));
    EndgameCexDepthDecision {
        enabled: true,
        fail_open: false,
        reason: if reduction > 0.0 {
            "triggered_size_reduce".to_string()
        } else {
            "pass".to_string()
        },
        venue: Some(snapshot.venue.as_str()),
        tick_band_ms: band_ms,
        max_age_ms,
        snapshot_age_ms: Some(age_ms),
        cex_mid: Some(mid),
        boundary_price: Some(boundary_price),
        distance_bps: Some(distance_bps),
        bucket_bps: Some(bucket_bps),
        cost_to_boundary_usd: Some(cost),
        threshold_usd: Some(threshold_usd),
        multiplier: (1.0 - reduction).clamp(0.0, 1.0),
    }
}

pub fn spawn_cex_depth_hub(
    cache: EndgameCexDepthCache,
    cfg: EndgameCexDepthConfig,
    symbols: Vec<String>,
) {
    if !cfg.enabled {
        return;
    }
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(cfg.fetch_timeout_ms))
            .pool_idle_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(8)
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let mut interval = tokio::time::interval(Duration::from_millis(cfg.poll_ms));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let symbols = symbols
            .into_iter()
            .filter_map(|symbol| {
                let normalized = normalize_symbol(symbol.as_str());
                binance_symbol(normalized.as_str())
                    .map(|(venue, venue_symbol)| (normalized, venue, venue_symbol))
            })
            .collect::<Vec<_>>();
        loop {
            interval.tick().await;
            if symbols.is_empty() {
                continue;
            }
            futures_util::stream::iter(symbols.iter().cloned())
                .for_each_concurrent(symbols.len().max(1), |(symbol, venue, venue_symbol)| {
                    let client = client.clone();
                    let cache = cache.clone();
                    let binance_limit = cfg.binance_limit;
                    async move {
                        match fetch_binance_depth(
                            &client,
                            venue,
                            venue_symbol.as_str(),
                            binance_limit,
                        )
                        .await
                        {
                            Ok(mut snapshot) => {
                                snapshot.symbol = symbol.clone();
                                cache.upsert(snapshot);
                            }
                            Err(error) => {
                                log::debug!(
                                    "endgame CEX depth fetch failed symbol={} venue={} venue_symbol={} error={}",
                                    symbol,
                                    venue.as_str(),
                                    venue_symbol,
                                    error
                                );
                            }
                        }
                    }
                })
                .await;
        }
    });
}

fn parse_tick_bands_ms(raw: &str) -> Vec<i64> {
    let mut out = raw
        .split(',')
        .filter_map(|part| part.trim().parse::<i64>().ok())
        .filter(|value| *value > 0)
        .collect::<Vec<_>>();
    if out.is_empty() {
        out = DEFAULT_TICK_BANDS_MS.to_vec();
    }
    out.sort_by(|a, b| b.cmp(a));
    out.dedup();
    out
}

fn band_for_tau_ms_with_bands(tau_ms: i64, bands: &[i64]) -> i64 {
    let tau_ms = tau_ms.max(0);
    let mut sorted = bands
        .iter()
        .copied()
        .filter(|value| *value > 0)
        .collect::<Vec<_>>();
    if sorted.is_empty() {
        sorted = DEFAULT_TICK_BANDS_MS.to_vec();
    }
    sorted.sort_by(|a, b| b.cmp(a));
    sorted.dedup();
    sorted
        .into_iter()
        .min_by_key(|band| (tau_ms - *band).abs())
        .unwrap_or(100)
}

pub fn cost_to_buy_up_to(asks: &[EndgameCexDepthLevel], target_price: f64) -> (f64, bool) {
    if !target_price.is_finite() || target_price <= 0.0 {
        return (f64::INFINITY, false);
    }
    let mut cost = 0.0;
    let mut reached = false;
    let mut asks = asks.to_vec();
    asks.sort_by(|left, right| {
        left.price
            .partial_cmp(&right.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for level in asks {
        if !level.price.is_finite()
            || !level.size.is_finite()
            || level.price <= 0.0
            || level.size <= 0.0
        {
            continue;
        }
        cost += level.price * level.size;
        if level.price >= target_price {
            reached = true;
            break;
        }
    }
    (cost, reached)
}

pub fn cost_to_sell_down_to(bids: &[EndgameCexDepthLevel], target_price: f64) -> (f64, bool) {
    if !target_price.is_finite() || target_price <= 0.0 {
        return (f64::INFINITY, false);
    }
    let mut cost = 0.0;
    let mut reached = false;
    let mut bids = bids.to_vec();
    bids.sort_by(|left, right| {
        right
            .price
            .partial_cmp(&left.price)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for level in bids {
        if !level.price.is_finite()
            || !level.size.is_finite()
            || level.price <= 0.0
            || level.size <= 0.0
        {
            continue;
        }
        cost += level.price * level.size;
        if level.price <= target_price {
            reached = true;
            break;
        }
    }
    (cost, reached)
}

fn bucket_for_actual_bps(actual_bps: f64) -> u16 {
    if !actual_bps.is_finite() || actual_bps <= 0.0 {
        return 1;
    }
    let rounded = actual_bps.ceil() as u16;
    BUCKETS_BPS
        .iter()
        .copied()
        .find(|bucket| *bucket >= rounded)
        .unwrap_or(200)
}

fn reduction_for_cost(cost: f64, threshold: f64, max_reduction: f64) -> f64 {
    if !cost.is_finite() || !threshold.is_finite() || threshold <= 0.0 || max_reduction <= 0.0 {
        return 0.0;
    }
    if cost >= threshold {
        return 0.0;
    }
    ((threshold - cost) / threshold * max_reduction).clamp(0.0, max_reduction)
}

fn normalize_symbol(symbol: &str) -> String {
    symbol.trim().to_ascii_uppercase()
}

fn binance_symbol(symbol: &str) -> Option<(EndgameCexVenue, String)> {
    match normalize_symbol(symbol).as_str() {
        "BTC" => Some((EndgameCexVenue::BinanceSpot, "BTCUSDT".to_string())),
        "ETH" => Some((EndgameCexVenue::BinanceSpot, "ETHUSDT".to_string())),
        "SOL" => Some((EndgameCexVenue::BinanceSpot, "SOLUSDT".to_string())),
        "XRP" => Some((EndgameCexVenue::BinanceSpot, "XRPUSDT".to_string())),
        "DOGE" => Some((EndgameCexVenue::BinanceSpot, "DOGEUSDT".to_string())),
        "BNB" => Some((EndgameCexVenue::BinanceSpot, "BNBUSDT".to_string())),
        "HYPE" => Some((EndgameCexVenue::BinanceFutures, "HYPEUSDT".to_string())),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct BinanceDepthResponse {
    bids: Vec<[String; 2]>,
    asks: Vec<[String; 2]>,
}

async fn fetch_binance_depth(
    client: &reqwest::Client,
    venue: EndgameCexVenue,
    venue_symbol: &str,
    limit: u16,
) -> anyhow::Result<EndgameCexDepthSnapshot> {
    let base_url = match venue {
        EndgameCexVenue::BinanceSpot => "https://api.binance.com/api/v3/depth",
        EndgameCexVenue::BinanceFutures => "https://fapi.binance.com/fapi/v1/depth",
    };
    let url = format!("{}?symbol={}&limit={}", base_url, venue_symbol, limit);
    let response = client.get(url).send().await?.error_for_status()?;
    let payload = response.json::<BinanceDepthResponse>().await?;
    let bids = parse_levels(payload.bids);
    let asks = parse_levels(payload.asks);
    let best_bid = bids
        .iter()
        .map(|level| level.price)
        .filter(|price| price.is_finite())
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0.0);
    let best_ask = asks
        .iter()
        .map(|level| level.price)
        .filter(|price| price.is_finite())
        .min_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0.0);
    Ok(EndgameCexDepthSnapshot {
        symbol: venue_symbol.trim_end_matches("USDT").to_string(),
        venue,
        best_bid,
        best_ask,
        bids,
        asks,
        updated_ms: chrono::Utc::now().timestamp_millis(),
    })
}

fn parse_levels(raw: Vec<[String; 2]>) -> Vec<EndgameCexDepthLevel> {
    raw.into_iter()
        .filter_map(|[price, size]| {
            let price = price.parse::<f64>().ok()?;
            let size = size.parse::<f64>().ok()?;
            (price.is_finite() && size.is_finite() && price > 0.0 && size > 0.0)
                .then_some(EndgameCexDepthLevel { price, size })
        })
        .collect()
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(price: f64, size: f64) -> EndgameCexDepthLevel {
        EndgameCexDepthLevel { price, size }
    }

    #[test]
    fn t_minus_band_selects_nearest_endgame_offset() {
        let cfg = EndgameCexDepthConfig::from_env();
        assert_eq!(cfg.band_for_tau_ms(1_950), 2_000);
        assert_eq!(cfg.band_for_tau_ms(1_050), 1_000);
        assert_eq!(cfg.band_for_tau_ms(125), 100);
    }

    #[test]
    fn buy_up_uses_sell_down_cost_to_boundary() {
        let bids = vec![level(100.0, 1.0), level(99.0, 2.0), level(98.0, 3.0)];
        let (cost, reached) = cost_to_sell_down_to(&bids, 99.0);
        assert!(reached);
        assert!((cost - 298.0).abs() < 1e-9);
    }

    #[test]
    fn buy_down_uses_buy_up_cost_to_boundary() {
        let asks = vec![level(101.0, 1.0), level(102.0, 2.0), level(103.0, 3.0)];
        let (cost, reached) = cost_to_buy_up_to(&asks, 102.0);
        assert!(reached);
        assert!((cost - 305.0).abs() < 1e-9);
    }

    #[test]
    fn evaluator_fails_open_on_stale_snapshot() {
        let cfg = EndgameCexDepthConfig::from_env();
        let cache = EndgameCexDepthCache::new();
        cache.upsert(EndgameCexDepthSnapshot {
            symbol: "BTC".to_string(),
            venue: EndgameCexVenue::BinanceSpot,
            best_bid: 100.0,
            best_ask: 101.0,
            bids: vec![level(100.0, 1.0)],
            asks: vec![level(101.0, 1.0)],
            updated_ms: 1,
        });
        let decision = evaluate_cex_depth(&cfg, &cache, "BTC", Direction::Up, 99.0, 100, 10_000);
        assert!(decision.fail_open);
        assert_eq!(decision.multiplier, 1.0);
    }

    #[test]
    fn evaluator_reduces_size_only_when_cost_is_below_threshold() {
        let mut cfg = EndgameCexDepthConfig::from_env();
        cfg.t100_threshold_usd = 1_000.0;
        cfg.max_reduction_pct = 50.0;
        let cache = EndgameCexDepthCache::new();
        cache.upsert(EndgameCexDepthSnapshot {
            symbol: "BTC".to_string(),
            venue: EndgameCexVenue::BinanceSpot,
            best_bid: 101.0,
            best_ask: 102.0,
            bids: vec![level(101.0, 1.0), level(99.0, 1.0)],
            asks: vec![level(102.0, 1.0)],
            updated_ms: 10_000,
        });
        let decision = evaluate_cex_depth(&cfg, &cache, "BTC", Direction::Up, 100.0, 100, 10_050);
        assert!(!decision.fail_open);
        assert!(decision.multiplier < 1.0);
        assert!(decision.multiplier >= 0.5);
    }
}
