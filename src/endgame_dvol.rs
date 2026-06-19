use crate::endgame_sweep::{
    normal_cdf, side_pricing_from_probability, EndgameSidePricing, PolymarketFeeModel,
};
use crate::event_log::log_event;
use crate::strategy::{Direction, Timeframe, STRATEGY_ID_ENDGAME_SWEEP_V1};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::Duration;
use tokio::time::timeout;

#[derive(Debug, Clone, Copy)]
pub struct EndgameDvolConfig {
    pub enabled: bool,
    pub refresh_ms: u64,
    pub timeout_ms: u64,
    pub stale_ms: i64,
    pub btc_multiplier: f64,
    pub eth_multiplier: f64,
    pub bnb_eth_multiplier: f64,
    pub sol_eth_multiplier: f64,
    pub xrp_eth_multiplier: f64,
    pub doge_eth_multiplier: f64,
    pub hype_eth_multiplier: f64,
    pub rv_synthetic_enabled: bool,
    pub rv_synthetic_refresh_ms: u64,
    pub rv_synthetic_timeout_ms: u64,
    pub rv_synthetic_stale_ms: i64,
    pub rv_synthetic_window_days: i64,
    pub rv_synthetic_min_samples: usize,
    pub rv_synthetic_min_multiplier: f64,
    pub rv_synthetic_max_multiplier: f64,
}

impl EndgameDvolConfig {
    pub fn from_env() -> Self {
        Self {
            enabled: env_bool("EVPOLY_ENDGAME_DVOL_ENABLE", true),
            refresh_ms: env_u64(
                "EVPOLY_ENDGAME_DVOL_REFRESH_MS",
                60_000,
                10_000,
                10 * 60_000,
            ),
            timeout_ms: env_u64("EVPOLY_ENDGAME_DVOL_TIMEOUT_MS", 1_500, 250, 10_000),
            stale_ms: env_i64(
                "EVPOLY_ENDGAME_DVOL_STALE_MS",
                3 * 60_000,
                30_000,
                30 * 60_000,
            ),
            btc_multiplier: env_f64("EVPOLY_ENDGAME_DVOL_BTC_MULT", 1.0, 0.0, 10.0),
            eth_multiplier: env_f64("EVPOLY_ENDGAME_DVOL_ETH_MULT", 1.0, 0.0, 10.0),
            bnb_eth_multiplier: env_f64("EVPOLY_ENDGAME_DVOL_BNB_ETH_MULT", 1.0, 0.0, 10.0),
            sol_eth_multiplier: env_f64("EVPOLY_ENDGAME_DVOL_SOL_ETH_MULT", 1.12, 0.0, 10.0),
            xrp_eth_multiplier: env_f64("EVPOLY_ENDGAME_DVOL_XRP_ETH_MULT", 1.12, 0.0, 10.0),
            doge_eth_multiplier: env_f64("EVPOLY_ENDGAME_DVOL_DOGE_ETH_MULT", 1.25, 0.0, 10.0),
            hype_eth_multiplier: env_f64("EVPOLY_ENDGAME_DVOL_HYPE_ETH_MULT", 2.0, 0.0, 10.0),
            rv_synthetic_enabled: env_bool("EVPOLY_ENDGAME_DVOL_RV_SYNTHETIC_ENABLE", true),
            rv_synthetic_refresh_ms: env_u64(
                "EVPOLY_ENDGAME_DVOL_RV_SYNTHETIC_REFRESH_MS",
                3_600_000,
                60_000,
                24 * 3_600_000,
            ),
            rv_synthetic_timeout_ms: env_u64(
                "EVPOLY_ENDGAME_DVOL_RV_SYNTHETIC_TIMEOUT_MS",
                5_000,
                500,
                30_000,
            ),
            rv_synthetic_stale_ms: env_i64(
                "EVPOLY_ENDGAME_DVOL_RV_SYNTHETIC_STALE_MS",
                6 * 3_600_000,
                60_000,
                7 * 24 * 3_600_000,
            ),
            rv_synthetic_window_days: env_i64(
                "EVPOLY_ENDGAME_DVOL_RV_SYNTHETIC_WINDOW_DAYS",
                30,
                7,
                90,
            ),
            rv_synthetic_min_samples: env_usize(
                "EVPOLY_ENDGAME_DVOL_RV_SYNTHETIC_MIN_SAMPLES",
                500,
                24,
                2_200,
            ),
            rv_synthetic_min_multiplier: env_f64(
                "EVPOLY_ENDGAME_DVOL_RV_SYNTHETIC_MIN_MULT",
                0.75,
                0.0,
                10.0,
            ),
            rv_synthetic_max_multiplier: env_f64(
                "EVPOLY_ENDGAME_DVOL_RV_SYNTHETIC_MAX_MULT",
                2.5,
                0.0,
                10.0,
            ),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EndgameDvolSnapshot {
    pub dvol_pct: f64,
    pub source_ts_ms: i64,
    pub fetched_ms: i64,
}

pub type EndgameDvolCache = Arc<StdRwLock<HashMap<String, EndgameDvolSnapshot>>>;

#[derive(Debug, Clone)]
pub struct EndgameDvolRvMultiplierSnapshot {
    pub multiplier: f64,
    pub symbol_rv_pct: f64,
    pub eth_rv_pct: f64,
    pub symbol_samples: usize,
    pub eth_samples: usize,
    pub symbol_source: &'static str,
    pub fetched_ms: i64,
    pub window_days: i64,
}

pub type EndgameDvolRvMultiplierCache =
    Arc<StdRwLock<HashMap<String, EndgameDvolRvMultiplierSnapshot>>>;

#[derive(Debug, Clone)]
pub struct EndgameDvolDecision {
    pub enabled: bool,
    pub pass: bool,
    pub status: &'static str,
    pub fair_probability: Option<f64>,
    pub dvol_required_bps: Option<f64>,
    pub actual_distance_bps: Option<f64>,
    pub edge_bps_at_price: Option<f64>,
    pub pricing: Option<EndgameSidePricing>,
    pub payload: Value,
}

impl EndgameDvolDecision {
    pub fn disabled(pricing: EndgameSidePricing) -> Self {
        Self {
            enabled: false,
            pass: true,
            status: "disabled",
            fair_probability: Some(pricing.fair_probability),
            dvol_required_bps: None,
            actual_distance_bps: None,
            edge_bps_at_price: Some(pricing.edge_bps),
            pricing: Some(pricing),
            payload: json!({"enabled": false, "status": "disabled"}),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EndgameDvolInput {
    pub symbol: String,
    pub timeframe: Timeframe,
    pub direction: Direction,
    pub tau_sec: i64,
    pub base_mid: f64,
    pub current_mid: f64,
    pub submit_price: f64,
    pub required_probability: f64,
    pub cex_depth_multiplier: f64,
    pub uncertainty_penalty: f64,
    pub buffer_prob: f64,
    pub edge_floor_prob: f64,
    pub bias_multiplier: f64,
    pub fee_model: PolymarketFeeModel,
}

#[derive(Debug, Clone)]
struct EndgameDvolSourceInfo {
    currency: &'static str,
    source: &'static str,
    multiplier: f64,
    payload: Value,
}

#[derive(Debug, Clone, Copy)]
struct EndgameRvSnapshot {
    rv_pct: f64,
    samples: usize,
    source: &'static str,
}

pub fn new_dvol_cache() -> EndgameDvolCache {
    Arc::new(StdRwLock::new(HashMap::new()))
}

pub fn new_rv_multiplier_cache() -> EndgameDvolRvMultiplierCache {
    Arc::new(StdRwLock::new(HashMap::new()))
}

pub fn spawn_dvol_refresh(cache: EndgameDvolCache, cfg: EndgameDvolConfig) {
    if !cfg.enabled {
        log_event(
            "endgame_dvol_refresh_started",
            json!({
                "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                "enabled": false
            }),
        );
        return;
    }
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        log_event(
            "endgame_dvol_refresh_started",
            json!({
                "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                "enabled": true,
                "currencies": ["BTC", "ETH"],
                "refresh_ms": cfg.refresh_ms,
                "timeout_ms": cfg.timeout_ms,
                "stale_ms": cfg.stale_ms,
                "decision_path": "book99_dvol_port"
            }),
        );
        loop {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let mut refreshed = 0usize;
            let mut failed = 0usize;
            for currency in ["BTC", "ETH"] {
                match fetch_deribit_dvol_snapshot(&client, currency, cfg, now_ms).await {
                    Ok(snapshot) => {
                        if let Ok(mut guard) = cache.write() {
                            guard.insert(currency.to_string(), snapshot);
                        }
                        refreshed += 1;
                    }
                    Err(err) => {
                        failed += 1;
                        log_event(
                            "endgame_dvol_refresh_failed",
                            json!({
                                "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                                "currency": currency,
                                "error": err.to_string(),
                                "cache_invalidated": false
                            }),
                        );
                    }
                }
            }
            log_event(
                "endgame_dvol_refresh_complete",
                json!({
                    "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                    "refreshed": refreshed,
                    "failed": failed,
                    "refresh_ms": cfg.refresh_ms,
                    "decision_path": "book99_dvol_port"
                }),
            );
            tokio::time::sleep(Duration::from_millis(cfg.refresh_ms)).await;
        }
    });
}

pub fn spawn_rv_multiplier_refresh(cache: EndgameDvolRvMultiplierCache, cfg: EndgameDvolConfig) {
    if !cfg.enabled || !cfg.rv_synthetic_enabled {
        log_event(
            "endgame_dvol_rv_multiplier_refresh_started",
            json!({
                "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                "enabled": false,
                "dvol_enabled": cfg.enabled,
                "rv_synthetic_enabled": cfg.rv_synthetic_enabled,
                "decision_path": "book99_dvol_port"
            }),
        );
        return;
    }
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        log_event(
            "endgame_dvol_rv_multiplier_refresh_started",
            json!({
                "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                "enabled": true,
                "base_symbol": "ETH",
                "symbols": ["BNB", "SOL", "XRP", "DOGE", "HYPE"],
                "window_days": cfg.rv_synthetic_window_days,
                "refresh_ms": cfg.rv_synthetic_refresh_ms,
                "timeout_ms": cfg.rv_synthetic_timeout_ms,
                "stale_ms": cfg.rv_synthetic_stale_ms,
                "min_samples": cfg.rv_synthetic_min_samples,
                "min_multiplier": cfg.rv_synthetic_min_multiplier,
                "max_multiplier": cfg.rv_synthetic_max_multiplier,
                "fallback": "fixed_env_eth_multipliers",
                "decision_path": "book99_dvol_port"
            }),
        );
        loop {
            let now_ms = chrono::Utc::now().timestamp_millis();
            match fetch_annualized_rv_snapshot(&client, "ETH", cfg, now_ms).await {
                Ok(eth) => {
                    let mut refreshed = 0usize;
                    let mut failed = 0usize;
                    let mut refresh_payloads = Vec::new();
                    for symbol in ["BNB", "SOL", "XRP", "DOGE", "HYPE"] {
                        match fetch_annualized_rv_snapshot(&client, symbol, cfg, now_ms).await {
                            Ok(symbol_rv) => {
                                let raw_multiplier = symbol_rv.rv_pct / eth.rv_pct;
                                let multiplier = raw_multiplier.clamp(
                                    cfg.rv_synthetic_min_multiplier,
                                    cfg.rv_synthetic_max_multiplier,
                                );
                                let snapshot = EndgameDvolRvMultiplierSnapshot {
                                    multiplier,
                                    symbol_rv_pct: symbol_rv.rv_pct,
                                    eth_rv_pct: eth.rv_pct,
                                    symbol_samples: symbol_rv.samples,
                                    eth_samples: eth.samples,
                                    symbol_source: symbol_rv.source,
                                    fetched_ms: now_ms,
                                    window_days: cfg.rv_synthetic_window_days,
                                };
                                if let Ok(mut guard) = cache.write() {
                                    guard.insert(symbol.to_string(), snapshot.clone());
                                }
                                refreshed += 1;
                                refresh_payloads.push(json!({
                                    "symbol": symbol,
                                    "multiplier": multiplier,
                                    "raw_multiplier": raw_multiplier,
                                    "symbol_rv_pct": symbol_rv.rv_pct,
                                    "eth_rv_pct": eth.rv_pct,
                                    "symbol_samples": symbol_rv.samples,
                                    "eth_samples": eth.samples,
                                    "symbol_source": symbol_rv.source,
                                    "base_source": eth.source,
                                    "clamped": (multiplier - raw_multiplier).abs() > 0.000_001
                                }));
                            }
                            Err(err) => {
                                failed += 1;
                                log_event(
                                    "endgame_dvol_rv_multiplier_refresh_failed",
                                    json!({
                                        "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                                        "symbol": symbol,
                                        "error": err.to_string(),
                                        "cache_invalidated": false
                                    }),
                                );
                            }
                        }
                    }
                    log_event(
                        "endgame_dvol_rv_multiplier_refresh_complete",
                        json!({
                            "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                            "refreshed": refreshed,
                            "failed": failed,
                            "base_symbol": "ETH",
                            "eth_rv_pct": eth.rv_pct,
                            "eth_samples": eth.samples,
                            "window_days": cfg.rv_synthetic_window_days,
                            "refresh_ms": cfg.rv_synthetic_refresh_ms,
                            "multipliers": refresh_payloads,
                            "decision_path": "book99_dvol_port"
                        }),
                    );
                }
                Err(err) => {
                    log_event(
                        "endgame_dvol_rv_multiplier_refresh_failed",
                        json!({
                            "strategy_id": STRATEGY_ID_ENDGAME_SWEEP_V1,
                            "symbol": "ETH",
                            "error": err.to_string(),
                            "cache_invalidated": false
                        }),
                    );
                }
            }
            tokio::time::sleep(Duration::from_millis(cfg.rv_synthetic_refresh_ms)).await;
        }
    });
}

pub fn evaluate_dvol_fair(
    cfg: &EndgameDvolConfig,
    cache: &EndgameDvolCache,
    rv_cache: &EndgameDvolRvMultiplierCache,
    input: EndgameDvolInput,
    now_ms: i64,
) -> EndgameDvolDecision {
    let legacy_pricing = side_pricing_from_probability(
        input.direction,
        0.5,
        input.uncertainty_penalty,
        input.buffer_prob,
        input.edge_floor_prob,
        input.bias_multiplier,
        input.fee_model,
    );
    if !cfg.enabled {
        return EndgameDvolDecision::disabled(legacy_pricing);
    }
    let normalized_symbol = normalize_symbol(input.symbol.as_str());
    if !matches!(
        normalized_symbol.as_str(),
        "BTC" | "ETH" | "BNB" | "SOL" | "XRP" | "DOGE" | "HYPE"
    ) {
        return EndgameDvolDecision {
            enabled: true,
            pass: false,
            status: "unsupported_symbol",
            fair_probability: None,
            dvol_required_bps: None,
            actual_distance_bps: None,
            edge_bps_at_price: None,
            pricing: None,
            payload: json!({
                "enabled": true,
                "status": "unsupported_symbol",
                "symbol": normalized_symbol,
                "decision_path": "book99_dvol_port"
            }),
        };
    }
    let source_info = dvol_source_for_symbol(input.symbol.as_str(), *cfg, rv_cache, now_ms);
    let snapshot = cache
        .read()
        .ok()
        .and_then(|guard| guard.get(source_info.currency).copied());
    let Some(snapshot) = snapshot else {
        return EndgameDvolDecision {
            enabled: true,
            pass: false,
            status: "missing",
            fair_probability: None,
            dvol_required_bps: None,
            actual_distance_bps: None,
            edge_bps_at_price: None,
            pricing: None,
            payload: json!({
                "enabled": true,
                "status": "missing",
                "source": source_info.source,
                "currency": source_info.currency,
                "symbol_multiplier": source_info.multiplier,
                "symbol_multiplier_context": source_info.payload,
                "decision_path": "book99_dvol_port"
            }),
        };
    };
    let dvol_age_ms = now_ms.saturating_sub(snapshot.fetched_ms).max(0);
    let dvol_source_age_ms = now_ms.saturating_sub(snapshot.source_ts_ms).max(0);
    let stale = dvol_age_ms > cfg.stale_ms || dvol_source_age_ms > cfg.stale_ms;
    let bps_per_second = dvol_bps_per_second(snapshot.dvol_pct);
    let actual_distance_bps =
        signed_distance_bps(input.direction, input.base_mid, input.current_mid);
    let positive_distance_bps = actual_distance_bps.max(0.0);
    let required_probability = input.required_probability.clamp(0.001, 0.999);
    let (z_score, z_score_source) = dvol_z_for_context(
        input.symbol.as_str(),
        input.timeframe,
        price_mode_for_probability(required_probability),
        required_probability,
        input.tau_sec,
    );
    let base_z_score = dvol_z_for_price(required_probability);
    let cex_depth_required_multiplier = if input.cex_depth_multiplier.is_finite()
        && input.cex_depth_multiplier > 0.0
        && input.cex_depth_multiplier < 1.0
    {
        (1.0 / input.cex_depth_multiplier).clamp(1.0, 10.0)
    } else {
        1.0
    };
    let raw_dvol_required_bps = bps_per_second
        .map(|bps| bps * (input.tau_sec.max(1) as f64).sqrt() * z_score * source_info.multiplier);
    let dvol_required_bps =
        raw_dvol_required_bps.map(|required| required * cex_depth_required_multiplier);
    let sigma_bps = bps_per_second
        .map(|bps| (bps * (input.tau_sec.max(1) as f64).sqrt() * source_info.multiplier).max(1e-9));
    let fair_probability = sigma_bps.map(|sigma| {
        let z = (actual_distance_bps / sigma).clamp(-8.0, 8.0);
        normal_cdf(z).clamp(0.001, 0.999)
    });
    let pricing = fair_probability.map(|fair| {
        side_pricing_from_probability(
            input.direction,
            fair,
            input.uncertainty_penalty,
            input.buffer_prob,
            input.edge_floor_prob,
            input.bias_multiplier,
            input.fee_model,
        )
    });
    let fee_at_submit = crate::endgame_sweep::polymarket_taker_fee_rate(
        input.submit_price.clamp(0.001, 0.999),
        input.fee_model,
    );
    let edge_bps_at_price =
        fair_probability.map(|fair| (fair - input.submit_price - fee_at_submit) * 10_000.0);
    let dvol_pass = !stale
        && dvol_required_bps
            .map(|required| positive_distance_bps + 1e-9 >= required)
            .unwrap_or(false);
    let status = if stale {
        if dvol_source_age_ms > cfg.stale_ms {
            "source_stale"
        } else {
            "stale"
        }
    } else if bps_per_second.is_none() || dvol_required_bps.is_none() || fair_probability.is_none()
    {
        "invalid"
    } else if dvol_pass {
        "pass"
    } else {
        "failed"
    };
    EndgameDvolDecision {
        enabled: true,
        pass: dvol_pass,
        status,
        fair_probability,
        dvol_required_bps,
        actual_distance_bps: Some(positive_distance_bps),
        edge_bps_at_price,
        pricing,
        payload: json!({
            "enabled": true,
            "status": status,
            "source": source_info.source,
            "currency": source_info.currency,
            "dvol_pct": snapshot.dvol_pct,
            "dvol_source_ts_ms": snapshot.source_ts_ms,
            "dvol_fetched_ms": snapshot.fetched_ms,
            "dvol_age_ms": dvol_age_ms,
            "dvol_source_age_ms": dvol_source_age_ms,
            "dvol_stale_ms": cfg.stale_ms,
            "dvol_stale": stale,
            "symbol_multiplier": source_info.multiplier,
            "symbol_multiplier_context": source_info.payload,
            "bps_per_second": bps_per_second,
            "tau_sec": input.tau_sec,
            "submit_price": input.submit_price,
            "required_probability": required_probability,
            "z_score": z_score,
            "base_z_score": base_z_score,
            "z_score_source": z_score_source,
            "base_mid": input.base_mid,
            "current_mid": input.current_mid,
            "direction": match input.direction { Direction::Up => "UP", Direction::Down => "DOWN" },
            "actual_distance_bps": positive_distance_bps,
            "signed_distance_bps": actual_distance_bps,
            "raw_dvol_required_bps": raw_dvol_required_bps,
            "cex_depth_multiplier": input.cex_depth_multiplier,
            "cex_depth_required_multiplier": cex_depth_required_multiplier,
            "dvol_required_bps": dvol_required_bps,
            "fair_probability": fair_probability,
            "submit_fee_rate": fee_at_submit,
            "edge_bps_at_submit_price": edge_bps_at_price,
            "dvol_pass": dvol_pass,
            "decision_path": "book99_dvol_port"
        }),
    }
}

async fn fetch_deribit_dvol_snapshot(
    client: &reqwest::Client,
    currency: &str,
    cfg: EndgameDvolConfig,
    now_ms: i64,
) -> Result<EndgameDvolSnapshot> {
    let start_ms = now_ms.saturating_sub(10 * 60_000);
    let url = format!(
        "https://www.deribit.com/api/v2/public/get_volatility_index_data?currency={currency}&start_timestamp={start_ms}&end_timestamp={now_ms}&resolution=60"
    );
    let payload: Value = timeout(Duration::from_millis(cfg.timeout_ms), async {
        let response = client
            .get(url)
            .send()
            .await
            .context("deribit_dvol_request")?;
        response.json().await.context("deribit_dvol_json_decode")
    })
    .await
    .context("deribit_dvol_timeout")??;
    let data = payload
        .get("result")
        .and_then(|result| result.get("data"))
        .and_then(Value::as_array)
        .context("deribit_dvol_missing_data")?;
    data.iter()
        .rev()
        .find_map(|row| {
            let row = row.as_array()?;
            let source_ts_ms = row
                .first()?
                .as_i64()
                .or_else(|| row.first()?.as_f64().map(|v| v as i64))?;
            let dvol_pct = row
                .get(4)
                .and_then(Value::as_f64)
                .or_else(|| row.get(1).and_then(Value::as_f64))?;
            (dvol_pct.is_finite() && dvol_pct > 0.0).then_some(EndgameDvolSnapshot {
                dvol_pct,
                source_ts_ms,
                fetched_ms: now_ms,
            })
        })
        .context("deribit_dvol_no_valid_rows")
}

async fn fetch_annualized_rv_snapshot(
    client: &reqwest::Client,
    symbol: &str,
    cfg: EndgameDvolConfig,
    now_ms: i64,
) -> Result<EndgameRvSnapshot> {
    let window_ms = cfg
        .rv_synthetic_window_days
        .saturating_add(1)
        .saturating_mul(24 * 3_600_000);
    let start_ms = now_ms.saturating_sub(window_ms);
    let closes = if normalize_symbol(symbol) == "HYPE" {
        fetch_hyperliquid_hourly_closes(client, symbol, start_ms, now_ms, cfg).await?
    } else {
        fetch_binance_hourly_closes(client, symbol, start_ms, now_ms, cfg).await?
    };
    let current_hour_ms = now_ms.saturating_div(3_600_000).saturating_mul(3_600_000);
    let rv_window_start_ms =
        current_hour_ms.saturating_sub(cfg.rv_synthetic_window_days.saturating_mul(24 * 3_600_000));
    let first_return_base_ms = rv_window_start_ms.saturating_sub(3_600_000);
    let mut completed = closes
        .into_iter()
        .filter(|(ts_ms, price)| {
            *ts_ms >= first_return_base_ms
                && *ts_ms < current_hour_ms
                && price.is_finite()
                && *price > 0.0
        })
        .collect::<Vec<_>>();
    completed.sort_by_key(|(ts_ms, _)| *ts_ms);
    let source = if normalize_symbol(symbol) == "HYPE" {
        "hyperliquid_1h_close"
    } else {
        "binance_1h_close"
    };
    let rv_pct =
        annualized_rv_pct_from_hourly_closes(completed.as_slice(), cfg.rv_synthetic_min_samples)?;
    Ok(EndgameRvSnapshot {
        rv_pct,
        samples: completed.len().saturating_sub(1),
        source,
    })
}

async fn fetch_binance_hourly_closes(
    client: &reqwest::Client,
    symbol: &str,
    start_ms: i64,
    now_ms: i64,
    cfg: EndgameDvolConfig,
) -> Result<Vec<(i64, f64)>> {
    let normalized = normalize_symbol(symbol);
    let url = format!(
        "https://api.binance.com/api/v3/klines?symbol={normalized}USDT&interval=1h&startTime={start_ms}&endTime={now_ms}&limit=1000"
    );
    let payload: Value = timeout(Duration::from_millis(cfg.rv_synthetic_timeout_ms), async {
        let response = client
            .get(url)
            .send()
            .await
            .context("binance_rv_klines_request")?;
        response.json().await.context("binance_rv_klines_json")
    })
    .await
    .context("binance_rv_klines_timeout")??;
    let rows = payload
        .as_array()
        .context("binance_rv_klines_expected_array")?;
    let mut closes = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(row) = row.as_array() else {
            continue;
        };
        let Some(ts_ms) = row.first().and_then(Value::as_i64) else {
            continue;
        };
        let close = row
            .get(4)
            .and_then(Value::as_str)
            .and_then(|raw| raw.parse::<f64>().ok())
            .or_else(|| row.get(4).and_then(Value::as_f64));
        if let Some(close) = close.filter(|v| v.is_finite() && *v > 0.0) {
            closes.push((ts_ms, close));
        }
    }
    Ok(closes)
}

async fn fetch_hyperliquid_hourly_closes(
    client: &reqwest::Client,
    symbol: &str,
    start_ms: i64,
    now_ms: i64,
    cfg: EndgameDvolConfig,
) -> Result<Vec<(i64, f64)>> {
    let normalized = normalize_symbol(symbol);
    let payload = json!({
        "type": "candleSnapshot",
        "req": {
            "coin": normalized,
            "interval": "1h",
            "startTime": start_ms,
            "endTime": now_ms
        }
    });
    let response_value: Value =
        timeout(Duration::from_millis(cfg.rv_synthetic_timeout_ms), async {
            let response = client
                .post("https://api.hyperliquid.xyz/info")
                .json(&payload)
                .send()
                .await
                .context("hyperliquid_rv_candles_request")?;
            response.json().await.context("hyperliquid_rv_candles_json")
        })
        .await
        .context("hyperliquid_rv_candles_timeout")??;
    let rows = response_value
        .as_array()
        .context("hyperliquid_rv_candles_expected_array")?;
    let mut closes = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(ts_ms) = row.get("t").and_then(Value::as_i64) else {
            continue;
        };
        let close = row
            .get("c")
            .and_then(Value::as_str)
            .and_then(|raw| raw.parse::<f64>().ok())
            .or_else(|| row.get("c").and_then(Value::as_f64));
        if let Some(close) = close.filter(|v| v.is_finite() && *v > 0.0) {
            closes.push((ts_ms, close));
        }
    }
    Ok(closes)
}

fn annualized_rv_pct_from_hourly_closes(closes: &[(i64, f64)], min_samples: usize) -> Result<f64> {
    let mut log_returns = Vec::with_capacity(closes.len().saturating_sub(1));
    for window in closes.windows(2) {
        let prev = window[0].1;
        let next = window[1].1;
        if prev.is_finite() && next.is_finite() && prev > 0.0 && next > 0.0 {
            log_returns.push((next / prev).ln());
        }
    }
    if log_returns.len() < min_samples {
        anyhow::bail!(
            "rv_insufficient_samples: {} < {}",
            log_returns.len(),
            min_samples
        );
    }
    let mean = log_returns.iter().sum::<f64>() / (log_returns.len() as f64);
    let variance = log_returns
        .iter()
        .map(|value| {
            let diff = *value - mean;
            diff * diff
        })
        .sum::<f64>()
        / ((log_returns.len().saturating_sub(1)) as f64).max(1.0);
    let rv_pct = variance.sqrt() * (365.0_f64 * 24.0).sqrt() * 100.0;
    if !rv_pct.is_finite() || rv_pct <= 0.0 {
        anyhow::bail!("rv_invalid_result");
    }
    Ok(rv_pct)
}

fn dvol_source_for_symbol(
    symbol: &str,
    cfg: EndgameDvolConfig,
    rv_cache: &EndgameDvolRvMultiplierCache,
    now_ms: i64,
) -> EndgameDvolSourceInfo {
    let normalized = normalize_symbol(symbol);
    let (currency, fixed_source, fixed_multiplier) =
        dvol_fixed_multiplier_for_symbol(normalized.as_str(), cfg);
    if matches!(normalized.as_str(), "BTC" | "ETH" | "BNB") {
        return EndgameDvolSourceInfo {
            currency,
            source: fixed_source,
            multiplier: fixed_multiplier,
            payload: json!({
                "mode": if normalized == "BNB" { "fixed_eth_dvol_multiplier" } else { "native_dvol" },
                "symbol": normalized,
                "currency": currency,
                "multiplier": fixed_multiplier
            }),
        };
    }
    if cfg.rv_synthetic_enabled {
        let snapshot = rv_cache
            .read()
            .ok()
            .and_then(|guard| guard.get(normalized.as_str()).cloned());
        if let Some(snapshot) = snapshot {
            let age_ms = now_ms.saturating_sub(snapshot.fetched_ms).max(0);
            if age_ms <= cfg.rv_synthetic_stale_ms {
                return EndgameDvolSourceInfo {
                    currency: "ETH",
                    source: "eth_dvol_synthetic_rv30",
                    multiplier: snapshot.multiplier,
                    payload: json!({
                        "mode": "rv30_eth_ratio",
                        "symbol": normalized,
                        "currency": "ETH",
                        "multiplier": snapshot.multiplier,
                        "symbol_rv_pct": snapshot.symbol_rv_pct,
                        "eth_rv_pct": snapshot.eth_rv_pct,
                        "symbol_samples": snapshot.symbol_samples,
                        "eth_samples": snapshot.eth_samples,
                        "symbol_source": snapshot.symbol_source,
                        "window_days": snapshot.window_days,
                        "fetched_ms": snapshot.fetched_ms,
                        "age_ms": age_ms,
                        "stale_ms": cfg.rv_synthetic_stale_ms
                    }),
                };
            }
            return EndgameDvolSourceInfo {
                currency,
                source: fixed_source,
                multiplier: fixed_multiplier,
                payload: json!({
                    "mode": "fixed_fallback_rv30_stale",
                    "symbol": normalized,
                    "currency": currency,
                    "multiplier": fixed_multiplier,
                    "cached_multiplier": snapshot.multiplier,
                    "cached_fetched_ms": snapshot.fetched_ms,
                    "cached_age_ms": age_ms,
                    "stale_ms": cfg.rv_synthetic_stale_ms
                }),
            };
        }
    }
    EndgameDvolSourceInfo {
        currency,
        source: fixed_source,
        multiplier: fixed_multiplier,
        payload: json!({
            "mode": if cfg.rv_synthetic_enabled { "fixed_fallback_rv30_missing" } else { "fixed_env" },
            "symbol": normalized,
            "currency": currency,
            "multiplier": fixed_multiplier,
            "rv_synthetic_enabled": cfg.rv_synthetic_enabled
        }),
    }
}

fn dvol_fixed_multiplier_for_symbol(
    symbol: &str,
    cfg: EndgameDvolConfig,
) -> (&'static str, &'static str, f64) {
    match normalize_symbol(symbol).as_str() {
        "BTC" => ("BTC", "btc_dvol", cfg.btc_multiplier),
        "ETH" => ("ETH", "eth_dvol", cfg.eth_multiplier),
        "BNB" => ("ETH", "eth_dvol_synthetic_bnb", cfg.bnb_eth_multiplier),
        "SOL" => ("ETH", "eth_dvol_synthetic_sol", cfg.sol_eth_multiplier),
        "XRP" => ("ETH", "eth_dvol_synthetic_xrp", cfg.xrp_eth_multiplier),
        "DOGE" => ("ETH", "eth_dvol_synthetic_doge", cfg.doge_eth_multiplier),
        "HYPE" => ("ETH", "eth_dvol_synthetic_hype", cfg.hype_eth_multiplier),
        _ => ("ETH", "eth_dvol_synthetic_other", cfg.doge_eth_multiplier),
    }
}

fn signed_distance_bps(direction: Direction, base_mid: f64, current_mid: f64) -> f64 {
    if !base_mid.is_finite() || !current_mid.is_finite() || base_mid <= 0.0 || current_mid <= 0.0 {
        return 0.0;
    }
    match direction {
        Direction::Up => ((current_mid - base_mid) / current_mid) * 10_000.0,
        Direction::Down => ((base_mid - current_mid) / current_mid) * 10_000.0,
    }
}

pub fn dvol_bps_per_second(dvol_pct: f64) -> Option<f64> {
    if !dvol_pct.is_finite() || dvol_pct <= 0.0 {
        return None;
    }
    Some((dvol_pct / 100.0) * 10_000.0 / (365.0_f64 * 24.0 * 3_600.0).sqrt())
}

pub fn dvol_z_for_price(price: f64) -> f64 {
    if !price.is_finite() {
        return 2.326_347_874;
    }
    let p = price.clamp(0.001, 0.999);
    if (p - 0.999).abs() <= 0.000_5 {
        3.090_232_306
    } else if (p - 0.99).abs() <= 0.000_5 {
        2.326_347_874
    } else if (p - 0.97).abs() <= 0.000_5 {
        1.880_793_608
    } else if (p - 0.96).abs() <= 0.000_5 {
        1.750_686_071
    } else if (p - 0.95).abs() <= 0.000_5 {
        1.644_853_627
    } else {
        inverse_normal_cdf(p).unwrap_or(2.326_347_874)
    }
}

pub fn dvol_z_for_context(
    _symbol: &str,
    timeframe: Timeframe,
    price_mode: &str,
    submit_price: f64,
    tau_sec: i64,
) -> (f64, &'static str) {
    let base_z = dvol_z_for_price(submit_price);
    if !matches!(timeframe, Timeframe::M5 | Timeframe::M15 | Timeframe::H1) {
        return (base_z, "price_normal_cdf");
    }
    if price_mode == "cent_99" && (submit_price - 0.99).abs() <= 0.000_5 {
        return match tau_sec {
            -5..=0 => (1.5, "cent99_tau_neg5_0_fixed_1_5"),
            1..=5 => (2.0, "cent99_tau_1_5_fixed_2_0"),
            6..=15 => (2.1, "cent99_tau_6_15_fixed_2_1"),
            16..=30 => (2.2, "cent99_tau_16_30_fixed_2_2"),
            31..=60 => (base_z, "cent99_tau_31_60_price_normal_cdf"),
            61..=90 => (2.45, "cent99_tau_61_90_fixed_2_45"),
            91..=180 => (2.6, "cent99_tau_91_180_fixed_2_6"),
            _ => (base_z, "cent99_tau_outside_schedule_price_normal_cdf"),
        };
    }
    if price_mode == "subcent_999" && (submit_price - 0.999).abs() <= 0.000_5 {
        return match tau_sec {
            -5..=0 => (2.0, "subcent999_tau_neg5_0_fixed_2_0"),
            1..=5 => (base_z, "subcent999_tau_1_5_price_normal_cdf"),
            _ => (base_z, "price_normal_cdf"),
        };
    }
    (base_z, "price_normal_cdf")
}

fn price_mode_for_probability(probability: f64) -> &'static str {
    if (probability - 0.999).abs() <= 0.000_5 {
        "subcent_999"
    } else if (probability - 0.99).abs() <= 0.000_5 {
        "cent_99"
    } else {
        "endgame_live"
    }
}

fn inverse_normal_cdf(p: f64) -> Option<f64> {
    if !(0.0..=1.0).contains(&p) || p <= 0.0 || p >= 1.0 {
        return None;
    }
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    let plow = 0.02425;
    let phigh = 1.0 - plow;
    if p < plow {
        let q = (-2.0 * p.ln()).sqrt();
        return Some(
            (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
                / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0),
        );
    }
    if p <= phigh {
        let q = p - 0.5;
        let r = q * q;
        return Some(
            (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
                / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0),
        );
    }
    let q = (-2.0 * (1.0 - p).ln()).sqrt();
    Some(
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0),
    )
}

fn normalize_symbol(raw: &str) -> String {
    match raw.trim().to_ascii_uppercase().as_str() {
        "SOLANA" => "SOL".to_string(),
        other => other.to_string(),
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64, min: u64, max: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn env_i64(name: &str, default: i64, min: i64, max: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn env_usize(name: &str, default: usize, min: usize, max: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn env_f64(name: &str, default: f64, min: f64, max: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(default)
        .clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dvol_z_context_matches_book99_tau_schedule() {
        let cent99_base = dvol_z_for_price(0.99);
        assert_eq!(
            dvol_z_for_context("BTC", Timeframe::M5, "cent_99", 0.99, -5),
            (1.5, "cent99_tau_neg5_0_fixed_1_5")
        );
        assert_eq!(
            dvol_z_for_context("BTC", Timeframe::M5, "cent_99", 0.99, 5),
            (2.0, "cent99_tau_1_5_fixed_2_0")
        );
        assert_eq!(
            dvol_z_for_context("BTC", Timeframe::M15, "cent_99", 0.99, 15),
            (2.1, "cent99_tau_6_15_fixed_2_1")
        );
        assert_eq!(
            dvol_z_for_context("BTC", Timeframe::H1, "cent_99", 0.99, 30),
            (2.2, "cent99_tau_16_30_fixed_2_2")
        );
        assert_eq!(
            dvol_z_for_context("BTC", Timeframe::M5, "cent_99", 0.99, 60),
            (cent99_base, "cent99_tau_31_60_price_normal_cdf")
        );
        assert_eq!(
            dvol_z_for_context("BTC", Timeframe::M5, "cent_99", 0.99, 90),
            (2.45, "cent99_tau_61_90_fixed_2_45")
        );
        assert_eq!(
            dvol_z_for_context("BTC", Timeframe::M5, "cent_99", 0.99, 180),
            (2.6, "cent99_tau_91_180_fixed_2_6")
        );
        let subcent_base = dvol_z_for_price(0.999);
        assert_eq!(
            dvol_z_for_context("BTC", Timeframe::M5, "subcent_999", 0.999, 0),
            (2.0, "subcent999_tau_neg5_0_fixed_2_0")
        );
        assert_eq!(
            dvol_z_for_context("BTC", Timeframe::M5, "subcent_999", 0.999, 5),
            (subcent_base, "subcent999_tau_1_5_price_normal_cdf")
        );
        assert_eq!(
            dvol_z_for_context("BTC", Timeframe::H4, "cent_99", 0.99, 5),
            (cent99_base, "price_normal_cdf")
        );
    }

    #[test]
    fn dvol_fair_probability_uses_remaining_tau() {
        let cfg = EndgameDvolConfig::from_env();
        let cache: EndgameDvolCache = Arc::new(StdRwLock::new(HashMap::from([(
            "BTC".to_string(),
            EndgameDvolSnapshot {
                dvol_pct: 60.0,
                source_ts_ms: 1_000,
                fetched_ms: 1_000,
            },
        )])));
        let rv_cache = new_rv_multiplier_cache();
        let decision = evaluate_dvol_fair(
            &cfg,
            &cache,
            &rv_cache,
            EndgameDvolInput {
                symbol: "BTC".to_string(),
                timeframe: Timeframe::M5,
                direction: Direction::Up,
                tau_sec: 8,
                base_mid: 100.0,
                current_mid: 100.10,
                submit_price: 0.92,
                required_probability: 0.92,
                cex_depth_multiplier: 1.0,
                uncertainty_penalty: 0.0,
                buffer_prob: 0.0,
                edge_floor_prob: 0.0,
                bias_multiplier: 1.0,
                fee_model: PolymarketFeeModel::default(),
            },
            1_500,
        );
        assert!(decision.pass, "{:?}", decision.payload);
        assert!(
            decision.fair_probability.unwrap_or_default() > 0.92,
            "{:?}",
            decision.payload
        );
    }
}
