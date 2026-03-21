use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::DateTime;
use futures_util::stream::{self, StreamExt};
use tokio::time::{timeout, Duration};

use crate::api::{PolymarketApi, RewardsMarketEntry};
use crate::models::{Market, MarketDetails, OrderBook, Token};
use crate::strategy::Timeframe;

use super::{MmMode, MmRewardsConfig};

#[derive(Debug, Clone, Default)]
pub struct MarketRewardSnapshot {
    pub reward_daily_rate: f64,
    pub midpoint: Option<f64>,
    pub max_spread: f64,
    pub min_size: f64,
}

#[derive(Debug, Clone)]
pub struct AutoMarketCandidate {
    pub market: Market,
    pub symbol: String,
    pub timeframe: Timeframe,
    pub timeframe_inferred: bool,
    pub reward_snapshot: MarketRewardSnapshot,
    pub best_bid_up: Option<f64>,
    pub best_ask_up: Option<f64>,
    pub best_bid_down: Option<f64>,
    pub best_ask_down: Option<f64>,
    pub tick_size_est: f64,
    pub feasible_levels_up_est: usize,
    pub feasible_levels_down_est: usize,
    pub feasible_pair_levels_est: usize,
    pub avg_spread: Option<f64>,
    pub rv_short_bps: Option<f64>,
    pub rv_mid_bps: Option<f64>,
    pub rv_long_bps: Option<f64>,
    pub inventory_risk: f64,
    pub competition_raw_api: Option<f64>,
    pub competition_score_api: Option<f64>,
    pub competition_level_api: String,
    pub competition_source_api: String,
    pub competition_qmin_est: f64,
    pub our_qmin_est: f64,
    pub our_share_est: f64,
    pub expected_reward_day_est: f64,
    pub reward_efficiency_est: f64,
    pub capital_locked_est: f64,
    pub score: f64,
}

pub async fn scan_rank_auto_candidates(
    api: &PolymarketApi,
    cfg: &MmRewardsConfig,
    now_ms: i64,
    midpoint_history: &mut HashMap<String, Vec<(i64, f64)>>,
) -> Result<Vec<AutoMarketCandidate>> {
    let rewards_page_budget_fast = std::env::var("EVPOLY_MM_REWARDS_PAGE_BUDGET_FAST")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(1)
        .clamp(1, 200);
    let gamma_fallback_enable = std::env::var("EVPOLY_MM_REWARDS_GAMMA_FALLBACK_ENABLE")
        .ok()
        .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(false);
    let min_candidate_pool = std::env::var("EVPOLY_MM_MIN_CANDIDATE_POOL")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(160)
        .clamp(16, 2_000);
    let detail_scan_cap = std::env::var("EVPOLY_MM_REWARDS_DETAIL_SCAN_CAP")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(360)
        .clamp(40, 4_000);
    let detail_timeout_ms = std::env::var("EVPOLY_MM_REWARDS_DETAIL_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(1_500)
        .clamp(300, 5_000);
    let detail_concurrency = std::env::var("EVPOLY_MM_REWARDS_DETAIL_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(12)
        .clamp(1, 64);

    let mut reward_feed_rows = api
        .get_rewards_markets_api(rewards_page_budget_fast, None, None, true)
        .await
        .ok()
        .unwrap_or_default();
    if reward_feed_rows.is_empty() {
        reward_feed_rows = api
            .get_rewards_markets_page()
            .await
            .ok()
            .unwrap_or_default();
    }
    if gamma_fallback_enable {
        let gamma_reward_rows = api
            .get_rewards_markets_gamma(500, cfg.auto_scan_limit.max(20))
            .await
            .ok()
            .unwrap_or_default();
        reward_feed_rows.extend(gamma_reward_rows);
    }

    let mut reward_feed_by_condition: HashMap<String, RewardsMarketEntry> = HashMap::new();
    let mut reward_feed_by_slug: HashMap<String, RewardsMarketEntry> = HashMap::new();
    for row in reward_feed_rows {
        merge_reward_feed_row(&mut reward_feed_by_condition, &mut reward_feed_by_slug, row);
    }
    let mut reward_feed_rows_sorted = reward_feed_by_condition
        .values()
        .cloned()
        .collect::<Vec<_>>();
    reward_feed_rows_sorted.sort_by(|a, b| b.reward_rate_hint().total_cmp(&a.reward_rate_hint()));

    let target_pool = cfg
        .auto_top_n
        .saturating_mul(2)
        .max(min_candidate_pool)
        .clamp(16, 2_000);
    let reward_feed_take = reward_feed_rows_sorted
        .len()
        .min(target_pool.saturating_mul(3).max(120))
        .min(detail_scan_cap);

    let detail_rows = stream::iter(
        reward_feed_rows_sorted
            .iter()
            .take(reward_feed_take)
            .cloned()
            .map(|row| async move {
                let details = match timeout(
                    Duration::from_millis(detail_timeout_ms),
                    api.get_market_via_proxy_pool(row.condition_id.as_str()),
                )
                .await
                {
                    Ok(Ok(v)) => v,
                    _ => return None,
                };
                Some((details, row))
            }),
    )
    .buffer_unordered(detail_concurrency)
    .collect::<Vec<_>>()
    .await;

    let mut market_rows: Vec<(
        Market,
        MarketDetails,
        MarketRewardSnapshot,
        Option<RewardsMarketEntry>,
    )> = Vec::new();
    let mut existing_conditions: HashSet<String> = HashSet::new();
    for item in detail_rows.into_iter().flatten() {
        let (details, row) = item;
        if !details.active || details.closed {
            continue;
        }
        if details.condition_id.trim().is_empty() || details.market_slug.trim().is_empty() {
            continue;
        }
        let condition_key = details.condition_id.to_ascii_lowercase();
        if condition_key.is_empty() || !existing_conditions.insert(condition_key) {
            continue;
        }
        if is_sports_market_details(
            details.tags.as_slice(),
            details.market_slug.as_str(),
            row.market_slug.as_str(),
        ) {
            continue;
        }
        if is_market_expired(&details, now_ms)
            || is_market_inside_exit_window(&details, now_ms, cfg.near_expiry_exit_window_sec)
        {
            continue;
        }
        let rewards_feed_row = reward_feed_match(
            &reward_feed_by_condition,
            &reward_feed_by_slug,
            details.condition_id.as_str(),
            details.market_slug.as_str(),
        )
        .or_else(|| Some(row.clone()));
        let mut reward_snapshot = snapshot_from_market_details(&details, now_ms);
        if let Some(row) = rewards_feed_row.as_ref() {
            let reward_daily = row.reward_rate_hint();
            if reward_daily.is_finite() && reward_daily > 0.0 {
                reward_snapshot.reward_daily_rate =
                    reward_snapshot.reward_daily_rate.max(reward_daily);
            }
            if let Some(spread) = row.rewards_max_spread {
                if spread.is_finite() && spread > 0.0 {
                    reward_snapshot.max_spread = spread;
                }
            }
            if let Some(min_size) = row.rewards_min_size {
                if min_size.is_finite() && min_size > 0.0 {
                    reward_snapshot.min_size = min_size;
                }
            }
        }
        if reward_snapshot.reward_daily_rate < cfg.min_reward_rate_hint {
            continue;
        }
        let market = market_from_details(&details);
        market_rows.push((market, details, reward_snapshot, rewards_feed_row));
    }
    if market_rows.is_empty() {
        return Ok(Vec::new());
    }
    market_rows.sort_by(|a, b| b.2.reward_daily_rate.total_cmp(&a.2.reward_daily_rate));
    let take_n = (cfg.auto_scan_limit as usize)
        .max(target_pool)
        .max(cfg.auto_top_n.saturating_mul(3))
        .min(market_rows.len());
    market_rows.truncate(take_n);

    let history_keep_ms = i64::try_from(cfg.rv_window_long_sec)
        .ok()
        .unwrap_or(86_400)
        .saturating_mul(2_000)
        .max(60_000);
    let mut ranked = Vec::new();

    for (market, details, reward_snapshot, rewards_feed_row) in market_rows {
        let Some(up_token_id) = select_token_id_from_details(&details, true) else {
            continue;
        };
        let Some(down_token_id) = select_token_id_from_details(&details, false) else {
            continue;
        };

        let (up_book_res, down_book_res) = tokio::join!(
            timeout(
                Duration::from_millis(1_000),
                api.get_orderbook(up_token_id.as_str())
            ),
            timeout(
                Duration::from_millis(1_000),
                api.get_orderbook(down_token_id.as_str())
            )
        );
        let up_book = match up_book_res {
            Ok(Ok(v)) => v,
            _ => continue,
        };
        let down_book = match down_book_res {
            Ok(Ok(v)) => v,
            _ => continue,
        };

        let (best_bid_up, best_ask_up) = top_bid_ask(&up_book);
        let (best_bid_down, best_ask_down) = top_bid_ask(&down_book);
        let up_price_hint = token_price_hint_from_details(&details, true);
        let down_price_hint = token_price_hint_from_details(&details, false);
        let avg_spread =
            avg_spread_from_books(best_bid_up, best_ask_up, best_bid_down, best_ask_down);
        let midpoint = midpoint_from_books(best_bid_up, best_ask_up, best_bid_down, best_ask_down)
            .or(reward_snapshot.midpoint);
        let Some(midpoint) = midpoint.filter(|v| v.is_finite() && *v > 0.0 && *v < 1.0) else {
            continue;
        };

        let history = midpoint_history
            .entry(market.condition_id.clone())
            .or_default();
        update_midpoint_history(history, now_ms, midpoint, history_keep_ms);
        let rv_short_bps = realized_vol_bps(history.as_slice(), now_ms, cfg.rv_window_short_sec);
        let rv_mid_bps = realized_vol_bps(history.as_slice(), now_ms, cfg.rv_window_mid_sec);
        let rv_long_bps = realized_vol_bps(history.as_slice(), now_ms, cfg.rv_window_long_sec);
        if cfg.rv_enable
            && rv_short_bps
                .map(|rv| rv > cfg.rv_block_threshold)
                .unwrap_or(false)
        {
            continue;
        }

        let tick_size_est = infer_tick_size_from_books(&up_book, &down_book).max(cfg.min_tick_move);
        let est_bid_up = reward_est_bid(best_bid_up, best_ask_up, up_price_hint);
        let est_bid_down = reward_est_bid(best_bid_down, best_ask_down, down_price_hint);
        let up_ladder_est = estimate_bid_ladder_prices(
            est_bid_up,
            cfg.ladder_levels,
            cfg.min_price,
            cfg.max_price,
            tick_size_est,
        );
        let down_ladder_est = estimate_bid_ladder_prices(
            est_bid_down,
            cfg.ladder_levels,
            cfg.min_price,
            cfg.max_price,
            tick_size_est,
        );
        let feasible_levels_up_est = count_scoring_ladder_levels(
            midpoint,
            reward_snapshot.max_spread,
            cfg.scoring_spread_extend_cents,
            up_ladder_est.as_slice(),
            true,
            tick_size_est,
        );
        let feasible_levels_down_est = count_scoring_ladder_levels(
            midpoint,
            reward_snapshot.max_spread,
            cfg.scoring_spread_extend_cents,
            down_ladder_est.as_slice(),
            false,
            tick_size_est,
        );
        let feasible_pair_levels_est = feasible_levels_up_est.min(feasible_levels_down_est);
        if feasible_pair_levels_est < cfg.auto_min_feasible_levels.max(1) {
            continue;
        }

        let (competition_raw_api, competition_source_api) = if let Some(raw) = rewards_feed_row
            .as_ref()
            .and_then(|row| row.market_competitiveness)
            .filter(|v| v.is_finite() && *v >= 0.0)
        {
            (Some(raw), "rewards_market_competitiveness".to_string())
        } else if let Some(raw) = market.competitive.filter(|v| v.is_finite() && *v >= 0.0) {
            (Some(raw), "gamma_market_competitive".to_string())
        } else {
            (None, "none".to_string())
        };
        let competition_score_api = normalize_competition_score(competition_raw_api);
        let competition_level_api =
            competition_level_from_api(competition_raw_api, competition_score_api).to_string();
        let inventory_risk =
            compute_inventory_risk(midpoint, avg_spread, rv_short_bps, competition_score_api);
        let reward_floor_shares = match cfg.reward_min_size_unit {
            super::MmRewardMinSizeUnit::Shares => reward_snapshot.min_size.max(0.0),
            super::MmRewardMinSizeUnit::Usd => {
                let reference_price = best_bid_up
                    .and_then(|up| best_bid_down.map(|down| (up + down) * 0.5))
                    .unwrap_or(midpoint)
                    .clamp(0.000001, 1.0);
                (reward_snapshot.min_size.max(0.0) / reference_price).max(0.0)
            }
        } * cfg.reward_min_target_mult.max(0.0);
        let budget_price_sum = best_bid_up.unwrap_or(0.0) + best_bid_down.unwrap_or(0.0);
        let budget_based_shares = if budget_price_sum > 0.0 {
            (cfg.auto_rank_budget_usd / budget_price_sum).max(0.0)
        } else {
            cfg.quote_shares_per_side.max(0.0)
        };
        let our_size_shares = budget_based_shares.max(reward_floor_shares).max(1.0);
        let competition_min_size_shares = reward_floor_shares.max(1.0);
        let competition_q = estimate_competition_qmin(
            &up_book,
            &down_book,
            midpoint,
            reward_snapshot.max_spread,
            competition_min_size_shares,
            12,
            3.0,
        );
        let our_estimate = estimate_our_reward_profile(
            reward_snapshot.reward_daily_rate,
            reward_snapshot.max_spread,
            midpoint,
            Some(est_bid_up),
            Some(est_bid_down),
            our_size_shares,
            competition_q,
            3.0,
        );
        let depth_ratio =
            (feasible_pair_levels_est as f64 / cfg.ladder_levels.max(1) as f64).clamp(0.0, 1.0);
        let depth_mult = (0.5 + depth_ratio).clamp(0.5, 1.5);
        let score = (((cfg.rank_alpha_reward * our_estimate.expected_reward_day_est.max(0.0))
            - (cfg.rank_beta_risk * inventory_risk.max(0.0)))
        .max(0.0))
            * depth_mult;
        let inferred_tf = infer_timeframe_from_slug(market.slug.as_str());
        let timeframe = inferred_tf.unwrap_or(Timeframe::D1);
        ranked.push(AutoMarketCandidate {
            symbol: infer_symbol(&market),
            market,
            timeframe,
            timeframe_inferred: inferred_tf.is_some(),
            reward_snapshot,
            best_bid_up,
            best_ask_up,
            best_bid_down,
            best_ask_down,
            tick_size_est,
            feasible_levels_up_est,
            feasible_levels_down_est,
            feasible_pair_levels_est,
            avg_spread,
            rv_short_bps,
            rv_mid_bps,
            rv_long_bps,
            inventory_risk,
            competition_raw_api,
            competition_score_api,
            competition_level_api,
            competition_source_api,
            competition_qmin_est: competition_q,
            our_qmin_est: our_estimate.our_qmin_est,
            our_share_est: our_estimate.our_share_est,
            expected_reward_day_est: our_estimate.expected_reward_day_est,
            reward_efficiency_est: our_estimate.reward_efficiency_est,
            capital_locked_est: our_estimate.capital_locked_est,
            score,
        });
    }

    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| {
                b.expected_reward_day_est
                    .total_cmp(&a.expected_reward_day_est)
            })
            .then_with(|| b.reward_efficiency_est.total_cmp(&a.reward_efficiency_est))
            .then_with(|| a.market.slug.cmp(&b.market.slug))
    });
    Ok(ranked)
}

pub async fn seed_auto_candidates_from_rewards_feed(
    api: &PolymarketApi,
    cfg: &MmRewardsConfig,
    rows: &[RewardsMarketEntry],
    now_ms: i64,
) -> Result<Vec<AutoMarketCandidate>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let take_n = cfg
        .auto_top_n
        .max(8)
        .saturating_mul(4)
        .min(cfg.auto_scan_limit as usize)
        .min(64)
        .max(8);
    let candidates = stream::iter(rows.iter().take(take_n).cloned().map(|row| async move {
        let details = match timeout(
            Duration::from_millis(1_500),
            api.get_market_via_proxy_pool(row.condition_id.as_str()),
        )
        .await
        {
            Ok(Ok(v)) => v,
            _ => return None,
        };
        if !details.active
            || details.closed
            || details.condition_id.trim().is_empty()
            || details.market_slug.trim().is_empty()
            || is_sports_market_details(
                details.tags.as_slice(),
                details.market_slug.as_str(),
                row.market_slug.as_str(),
            )
            || is_market_expired(&details, now_ms)
            || is_market_inside_exit_window(&details, now_ms, cfg.near_expiry_exit_window_sec)
        {
            return None;
        }
        let mut reward_snapshot = snapshot_from_market_details(&details, now_ms);
        let reward_daily = row.reward_rate_hint();
        if reward_daily.is_finite() && reward_daily > 0.0 {
            reward_snapshot.reward_daily_rate = reward_snapshot.reward_daily_rate.max(reward_daily);
        }
        if let Some(spread) = row.rewards_max_spread.filter(|v| v.is_finite() && *v > 0.0) {
            reward_snapshot.max_spread = spread;
        }
        if let Some(min_size) = row.rewards_min_size.filter(|v| v.is_finite() && *v > 0.0) {
            reward_snapshot.min_size = min_size;
        }
        if reward_snapshot.reward_daily_rate < cfg.min_reward_rate_hint {
            return None;
        }
        let market = market_from_details(&details);
        let symbol = infer_symbol(&market);
        let inferred_tf = infer_timeframe_from_slug(market.slug.as_str());
        let timeframe = inferred_tf.unwrap_or(Timeframe::D1);
        let competition_raw_api = row
            .market_competitiveness
            .filter(|v| v.is_finite() && *v >= 0.0)
            .or(market.competitive.filter(|v| v.is_finite() && *v >= 0.0));
        let competition_score_api = normalize_competition_score(competition_raw_api);
        let competition_level_api =
            competition_level_from_api(competition_raw_api, competition_score_api).to_string();
        let competition_source_api = if row.market_competitiveness.is_some() {
            "rewards_market_competitiveness".to_string()
        } else if market.competitive.is_some() {
            "gamma_market_competitive".to_string()
        } else {
            "none".to_string()
        };
        let midpoint = reward_snapshot.midpoint.unwrap_or(0.5);
        let inventory_risk = compute_inventory_risk(midpoint, None, None, competition_score_api);
        let capital_locked_est = cfg.auto_rank_budget_usd.max(1.0);
        let expected_reward_day_est = reward_snapshot.reward_daily_rate.max(0.0);
        let reward_efficiency_est = expected_reward_day_est / capital_locked_est;
        let score =
            (expected_reward_day_est * (1.0 - competition_score_api.unwrap_or(0.0) * 0.5)).max(0.0);
        Some(AutoMarketCandidate {
            market,
            symbol,
            timeframe,
            timeframe_inferred: inferred_tf.is_some(),
            reward_snapshot,
            best_bid_up: None,
            best_ask_up: None,
            best_bid_down: None,
            best_ask_down: None,
            tick_size_est: f64::try_from(details.minimum_tick_size)
                .ok()
                .unwrap_or(cfg.min_tick_move)
                .max(cfg.min_tick_move),
            feasible_levels_up_est: cfg.auto_min_feasible_levels.max(1),
            feasible_levels_down_est: cfg.auto_min_feasible_levels.max(1),
            feasible_pair_levels_est: cfg.auto_min_feasible_levels.max(1),
            avg_spread: None,
            rv_short_bps: None,
            rv_mid_bps: None,
            rv_long_bps: None,
            inventory_risk,
            competition_raw_api,
            competition_score_api,
            competition_level_api,
            competition_source_api,
            competition_qmin_est: 0.0,
            our_qmin_est: 0.0,
            our_share_est: 0.0,
            expected_reward_day_est,
            reward_efficiency_est,
            capital_locked_est,
            score,
        })
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    let mut out = candidates.into_iter().flatten().collect::<Vec<_>>();
    out.sort_by(|a, b| {
        b.reward_snapshot
            .reward_daily_rate
            .total_cmp(&a.reward_snapshot.reward_daily_rate)
            .then_with(|| b.score.total_cmp(&a.score))
            .then_with(|| a.market.slug.cmp(&b.market.slug))
    });
    Ok(out)
}

pub fn select_top_candidates_per_mode(
    ranked: &[AutoMarketCandidate],
    enabled_modes: &[MmMode],
    per_mode: usize,
) -> HashMap<MmMode, Vec<AutoMarketCandidate>> {
    let mut selected: HashMap<MmMode, Vec<AutoMarketCandidate>> = HashMap::new();
    if ranked.is_empty() || enabled_modes.is_empty() {
        return selected;
    }
    let per_mode = per_mode.max(1);
    let reward_p75 = percentile(
        ranked
            .iter()
            .map(|candidate| candidate.expected_reward_day_est)
            .filter(|value| value.is_finite() && *value > 0.0)
            .collect::<Vec<_>>()
            .as_slice(),
        0.75,
    )
    .unwrap_or(0.0);
    let mut used_conditions: HashSet<String> = HashSet::new();
    for mode in enabled_modes.iter().copied() {
        selected.insert(mode, Vec::new());
    }

    // Fair slot assignment: interleave modes per slot to avoid mode-order starvation.
    for _slot in 0..per_mode {
        for mode in enabled_modes.iter().copied() {
            let mode_selected = selected.entry(mode).or_default();
            if mode_selected.len() >= per_mode {
                continue;
            }
            let mut best_idx: Option<usize> = None;
            let mut best_score = f64::NEG_INFINITY;
            for (idx, candidate) in ranked.iter().enumerate() {
                if used_conditions.contains(candidate.market.condition_id.as_str()) {
                    continue;
                }
                let fit_score = mode_fit_score(mode, candidate, reward_p75);
                if fit_score.is_finite() && fit_score > best_score {
                    best_score = fit_score;
                    best_idx = Some(idx);
                }
            }
            if best_idx.is_none() {
                best_idx = ranked.iter().enumerate().find_map(|(idx, candidate)| {
                    (!used_conditions.contains(candidate.market.condition_id.as_str()))
                        .then_some(idx)
                });
            }
            let Some(idx) = best_idx else {
                continue;
            };
            let candidate = ranked[idx].clone();
            used_conditions.insert(candidate.market.condition_id.clone());
            mode_selected.push(candidate);
        }
    }
    selected.retain(|_, rows| !rows.is_empty());
    selected
}

pub fn select_top_candidates_by_reward(
    ranked: &[AutoMarketCandidate],
    enabled_modes: &[MmMode],
    total_limit: usize,
) -> HashMap<MmMode, Vec<AutoMarketCandidate>> {
    let mut selected: HashMap<MmMode, Vec<AutoMarketCandidate>> = HashMap::new();
    if ranked.is_empty() || enabled_modes.is_empty() || total_limit == 0 {
        return selected;
    }
    let reward_p75 = reward_p75_estimate(ranked);
    let mut used_conditions: HashSet<String> = HashSet::new();
    let mut ordered = ranked.to_vec();
    ordered.sort_by(|a, b| {
        b.reward_snapshot
            .reward_daily_rate
            .total_cmp(&a.reward_snapshot.reward_daily_rate)
            .then_with(|| b.score.total_cmp(&a.score))
            .then_with(|| {
                b.expected_reward_day_est
                    .total_cmp(&a.expected_reward_day_est)
            })
            .then_with(|| a.market.slug.cmp(&b.market.slug))
    });
    for candidate in ordered {
        if used_conditions.contains(candidate.market.condition_id.as_str()) {
            continue;
        }
        let Some(mode) = preferred_mode_for_candidate(&candidate, enabled_modes, reward_p75) else {
            continue;
        };
        used_conditions.insert(candidate.market.condition_id.clone());
        selected.entry(mode).or_default().push(candidate);
        if used_conditions.len() >= total_limit {
            break;
        }
    }
    selected
}

pub fn preferred_mode_for_candidate(
    candidate: &AutoMarketCandidate,
    enabled_modes: &[MmMode],
    reward_p75: f64,
) -> Option<MmMode> {
    let mut best_mode: Option<MmMode> = None;
    let mut best_score = f64::NEG_INFINITY;
    for mode in enabled_modes.iter().copied() {
        let fit = mode_fit_score(mode, candidate, reward_p75);
        if fit.is_finite() && fit > best_score {
            best_score = fit;
            best_mode = Some(mode);
        }
    }
    best_mode
}

pub fn reward_p75_estimate(ranked: &[AutoMarketCandidate]) -> f64 {
    percentile(
        ranked
            .iter()
            .map(|candidate| candidate.expected_reward_day_est)
            .filter(|value| value.is_finite() && *value > 0.0)
            .collect::<Vec<_>>()
            .as_slice(),
        0.75,
    )
    .unwrap_or(0.0)
}

fn merge_reward_feed_row(
    by_condition: &mut HashMap<String, RewardsMarketEntry>,
    by_slug: &mut HashMap<String, RewardsMarketEntry>,
    row: RewardsMarketEntry,
) {
    let condition_key = row.condition_id.trim().to_ascii_lowercase();
    if !condition_key.is_empty() {
        let existing = by_condition.get(condition_key.as_str()).cloned();
        let merged = if let Some(existing) = existing {
            if row.reward_rate_hint() > existing.reward_rate_hint() {
                row.clone()
            } else {
                existing
            }
        } else {
            row.clone()
        };
        by_condition.insert(condition_key, merged.clone());
        if !merged.market_slug.trim().is_empty() {
            by_slug.insert(merged.market_slug.trim().to_ascii_lowercase(), merged);
        }
    } else if !row.market_slug.trim().is_empty() {
        by_slug.insert(row.market_slug.trim().to_ascii_lowercase(), row);
    }
}

fn reward_feed_match(
    by_condition: &HashMap<String, RewardsMarketEntry>,
    by_slug: &HashMap<String, RewardsMarketEntry>,
    condition_id: &str,
    market_slug: &str,
) -> Option<RewardsMarketEntry> {
    let condition_key = condition_id.trim().to_ascii_lowercase();
    if !condition_key.is_empty() {
        if let Some(row) = by_condition.get(condition_key.as_str()) {
            return Some(row.clone());
        }
    }
    let slug_key = market_slug.trim().to_ascii_lowercase();
    if !slug_key.is_empty() {
        return by_slug.get(slug_key.as_str()).cloned();
    }
    None
}

pub fn snapshot_from_market_details(details: &MarketDetails, _now_ms: i64) -> MarketRewardSnapshot {
    let reward_daily_rate = extract_max_numeric(details.rewards.rates.as_ref()).unwrap_or(0.0);
    MarketRewardSnapshot {
        reward_daily_rate,
        midpoint: infer_midpoint(details),
        max_spread: f64::try_from(details.rewards.max_spread)
            .ok()
            .unwrap_or(0.0),
        min_size: f64::try_from(details.rewards.min_size).ok().unwrap_or(0.0),
    }
}

fn market_end_ts_ms(details: &MarketDetails) -> Option<i64> {
    let end_iso = details.end_date_iso.trim();
    if end_iso.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(end_iso) {
        return Some(dt.timestamp_millis());
    }
    chrono::NaiveDate::parse_from_str(end_iso, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|dt| {
            chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc)
                .timestamp_millis()
        })
}

pub fn infer_midpoint(details: &MarketDetails) -> Option<f64> {
    let up_price = details.tokens.iter().find_map(|token| {
        let outcome = token.outcome.trim().to_ascii_uppercase();
        let is_up =
            outcome.contains("UP") || outcome == "YES" || outcome == "TRUE" || outcome == "1";
        if !is_up {
            return None;
        }
        let value = f64::try_from(token.price).ok()?;
        (value.is_finite() && value > 0.0 && value < 1.0).then_some(value)
    });
    if up_price.is_some() {
        return up_price;
    }

    let down_price = details.tokens.iter().find_map(|token| {
        let outcome = token.outcome.trim().to_ascii_uppercase();
        let is_down =
            outcome.contains("DOWN") || outcome == "NO" || outcome == "FALSE" || outcome == "0";
        if !is_down {
            return None;
        }
        let value = f64::try_from(token.price).ok()?;
        (value.is_finite() && value > 0.0 && value < 1.0).then_some(value)
    });
    if let Some(down) = down_price {
        return Some((1.0 - down).clamp(0.0, 1.0));
    }

    let mut values = details
        .tokens
        .iter()
        .filter_map(|token| f64::try_from(token.price).ok())
        .filter(|value| value.is_finite() && *value >= 0.0 && *value <= 1.0)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if values.len() == 1 {
        return values.first().copied();
    }
    let first = values.first().copied().unwrap_or(0.5);
    let last = values.last().copied().unwrap_or(0.5);
    if (first + last - 1.0).abs() <= 0.10 {
        // Fallback for complementary YES/NO prices when outcomes are unlabeled.
        return Some(first.clamp(0.0, 1.0));
    }
    Some(((first + last) / 2.0).clamp(0.0, 1.0))
}

fn select_token_id_from_details(details: &MarketDetails, up_side: bool) -> Option<String> {
    details.tokens.iter().find_map(|token| {
        let outcome = token.outcome.trim().to_ascii_uppercase();
        let matched = if up_side {
            outcome.contains("UP") || outcome == "YES" || outcome == "TRUE" || outcome == "1"
        } else {
            outcome.contains("DOWN") || outcome == "NO" || outcome == "FALSE" || outcome == "0"
        };
        matched.then(|| token.token_id.clone())
    })
}

fn token_price_hint_from_details(details: &MarketDetails, up_side: bool) -> Option<f64> {
    details.tokens.iter().find_map(|token| {
        let outcome = token.outcome.trim().to_ascii_uppercase();
        let matched = if up_side {
            outcome.contains("UP") || outcome == "YES" || outcome == "TRUE" || outcome == "1"
        } else {
            outcome.contains("DOWN") || outcome == "NO" || outcome == "FALSE" || outcome == "0"
        };
        if !matched {
            return None;
        }
        let price = f64::try_from(token.price).ok()?;
        (price.is_finite() && price > 0.0 && price < 1.0).then_some(price)
    })
}

fn reward_book_is_stub(best_bid: Option<f64>, best_ask: Option<f64>) -> bool {
    let (Some(bid), Some(ask)) = (best_bid, best_ask) else {
        return false;
    };
    if !(bid.is_finite() && ask.is_finite()) {
        return false;
    }
    (bid <= 0.01 && ask >= 0.99) || (ask - bid >= 0.80)
}

fn reward_est_bid(best_bid: Option<f64>, best_ask: Option<f64>, price_hint: Option<f64>) -> f64 {
    if reward_book_is_stub(best_bid, best_ask) {
        if let Some(hint) = price_hint.filter(|v| v.is_finite() && *v > 0.0 && *v < 1.0) {
            return hint;
        }
    }
    best_bid.unwrap_or(0.0)
}

fn market_from_details(details: &MarketDetails) -> Market {
    let tokens = if details.tokens.is_empty() {
        None
    } else {
        Some(
            details
                .tokens
                .iter()
                .map(|token| Token {
                    token_id: token.token_id.clone(),
                    outcome: token.outcome.clone(),
                    price: Some(token.price),
                })
                .collect::<Vec<_>>(),
        )
    };
    Market {
        condition_id: details.condition_id.clone(),
        market_id: None,
        question: details.question.clone(),
        slug: details.market_slug.clone(),
        description: Some(details.description.clone()),
        resolution_source: None,
        end_date: None,
        end_date_iso: Some(details.end_date_iso.clone()),
        end_date_iso_alt: None,
        game_start_time: details.game_start_time.clone(),
        start_date: None,
        active: details.active,
        closed: details.closed,
        tokens,
        clob_token_ids: None,
        outcomes: None,
        competitive: None,
    }
}

fn is_market_expired(details: &MarketDetails, now_ms: i64) -> bool {
    market_end_ts_ms(details)
        .map(|end_ts_ms| end_ts_ms <= now_ms)
        .unwrap_or(false)
}

fn is_market_inside_exit_window(details: &MarketDetails, now_ms: i64, window_sec: u64) -> bool {
    let window_ms = i64::try_from(window_sec)
        .ok()
        .unwrap_or(24 * 3_600)
        .saturating_mul(1_000)
        .max(1_000);
    market_end_ts_ms(details)
        .map(|end_ts_ms| end_ts_ms > now_ms && end_ts_ms.saturating_sub(now_ms) <= window_ms)
        .unwrap_or(false)
}

fn top_bid_ask(book: &OrderBook) -> (Option<f64>, Option<f64>) {
    let best_bid = book
        .bids
        .iter()
        .filter_map(|entry| f64::try_from(entry.price).ok())
        .filter(|price| price.is_finite() && *price > 0.0)
        .max_by(|a, b| a.total_cmp(b));
    let best_ask = book
        .asks
        .iter()
        .filter_map(|entry| f64::try_from(entry.price).ok())
        .filter(|price| price.is_finite() && *price > 0.0)
        .min_by(|a, b| a.total_cmp(b));
    (best_bid, best_ask)
}

fn infer_tick_size_from_books(up_book: &OrderBook, down_book: &OrderBook) -> f64 {
    let mut prices = up_book
        .bids
        .iter()
        .chain(up_book.asks.iter())
        .chain(down_book.bids.iter())
        .chain(down_book.asks.iter())
        .filter_map(|entry| f64::try_from(entry.price).ok())
        .filter(|price| price.is_finite() && *price > 0.0)
        .collect::<Vec<_>>();
    prices.sort_by(|a, b| a.total_cmp(b));
    prices.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let mut best = f64::INFINITY;
    for window in prices.windows(2) {
        if let [a, b] = window {
            let delta = (b - a).abs();
            if delta.is_finite() && delta > 1e-9 && delta < best {
                best = delta;
            }
        }
    }
    if best.is_finite() && best > 0.0 {
        best.clamp(0.001, 0.05)
    } else {
        0.01
    }
}

fn estimate_bid_ladder_prices(
    anchor_price: f64,
    levels: usize,
    min_price: f64,
    max_price: f64,
    tick_size: f64,
) -> Vec<f64> {
    if !anchor_price.is_finite() || anchor_price <= 0.0 {
        return Vec::new();
    }
    let tick = tick_size.max(0.001);
    let mut ladder: Vec<f64> = Vec::new();
    for idx in 0..levels.max(1) {
        let candidate = (anchor_price - (idx as f64 * tick)).clamp(min_price, max_price);
        let rounded = ((candidate / tick).floor() * tick).clamp(min_price, max_price);
        if rounded.is_finite() && rounded > 0.0 {
            if ladder
                .last()
                .map(|last| (*last - rounded).abs() < 1e-9)
                .unwrap_or(false)
            {
                continue;
            }
            ladder.push(rounded);
        }
    }
    ladder
}

fn scoring_spread_cap(max_spread: f64) -> f64 {
    if max_spread > 1.0 {
        (max_spread / 100.0).max(0.0)
    } else {
        max_spread.max(0.0)
    }
}

fn scoring_spread_with_extend(max_spread: f64, extend_cents: f64) -> f64 {
    let ext = extend_cents.max(0.0);
    if max_spread > 1.0 {
        max_spread + ext
    } else {
        max_spread + (ext / 100.0)
    }
}

fn count_scoring_ladder_levels(
    midpoint: f64,
    max_spread: f64,
    spread_extend_cents: f64,
    ladder_prices: &[f64],
    up_side: bool,
    _tick_size: f64,
) -> usize {
    if !midpoint.is_finite() || !(0.0..=1.0).contains(&midpoint) {
        return 0;
    }
    let spread_cap =
        scoring_spread_cap(scoring_spread_with_extend(max_spread, spread_extend_cents));
    if spread_cap <= 0.0 {
        return 0;
    }
    let mut count = 0usize;
    for price in ladder_prices {
        if !price.is_finite() || *price <= 0.0 {
            break;
        }
        let yes_equiv = if up_side {
            *price
        } else {
            (1.0 - *price).clamp(0.0, 1.0)
        };
        let ok = (midpoint - yes_equiv).abs() <= (spread_cap + 1e-9);
        if ok {
            count = count.saturating_add(1);
        } else if count > 0 {
            break;
        }
    }
    count
}

fn midpoint_from_quotes_local(bid: Option<f64>, ask: Option<f64>) -> Option<f64> {
    match (bid, ask) {
        (Some(b), Some(a)) if b.is_finite() && a.is_finite() && b > 0.0 && a > 0.0 => {
            Some((a + b) * 0.5)
        }
        (Some(b), None) if b.is_finite() && b > 0.0 => Some(b),
        (None, Some(a)) if a.is_finite() && a > 0.0 => Some(a),
        _ => None,
    }
}

fn candidate_midpoint_hint(candidate: &AutoMarketCandidate) -> f64 {
    if let Some(mid) = candidate
        .reward_snapshot
        .midpoint
        .filter(|value| value.is_finite() && *value > 0.0 && *value < 1.0)
    {
        return mid;
    }
    let up_mid = midpoint_from_quotes_local(candidate.best_bid_up, candidate.best_ask_up);
    let down_mid = midpoint_from_quotes_local(candidate.best_bid_down, candidate.best_ask_down);
    match (up_mid, down_mid) {
        (Some(up), Some(down)) => ((up + (1.0 - down)) * 0.5).clamp(0.0, 1.0),
        (Some(up), None) => up.clamp(0.0, 1.0),
        (None, Some(down)) => (1.0 - down).clamp(0.0, 1.0),
        _ => 0.5,
    }
}

fn is_sports_tag(tag: &str) -> bool {
    let normalized = tag.trim().to_ascii_lowercase();
    normalized == "sports"
        || normalized.starts_with("sport")
        || normalized.contains("sports")
        || normalized.contains("basketball")
        || normalized.contains("football")
        || normalized.contains("baseball")
        || normalized.contains("hockey")
        || normalized.contains("soccer")
        || normalized.contains("tennis")
        || normalized.contains("mma")
        || normalized.contains("golf")
        || normalized.contains("ufc")
}

pub fn is_sports_market_slug(slug: &str) -> bool {
    let normalized = slug.trim().to_ascii_lowercase();
    normalized.starts_with("sports/")
        || normalized.starts_with("sports-")
        || normalized.contains("/sports/")
        || normalized.contains("/sports-")
        || normalized.starts_with("cbb-")
        || normalized.starts_with("nba-")
        || normalized.starts_with("wnba-")
        || normalized.starts_with("nfl-")
        || normalized.starts_with("mlb-")
        || normalized.starts_with("nhl-")
        || normalized.starts_with("ncaab-")
        || normalized.starts_with("ncaaf-")
        || normalized.starts_with("epl-")
        || normalized.starts_with("ufc-")
        || normalized.contains("champions-league")
        || normalized.contains("world-cup")
        || normalized.contains("super-bowl")
        || normalized.contains("-playoff")
        || normalized.contains("-vs-")
        || normalized.contains("/vs-")
}

pub fn is_sports_market_details(tags: &[String], primary_slug: &str, fallback_slug: &str) -> bool {
    tags.iter().any(|tag| is_sports_tag(tag.as_str()))
        || is_sports_market_slug(primary_slug)
        || is_sports_market_slug(fallback_slug)
}

pub fn is_sports_market_runtime(slug: &str, question: &str) -> bool {
    if is_sports_market_slug(slug) {
        return true;
    }
    let question_lc = question.trim().to_ascii_lowercase();
    question_lc.contains(" vs. ")
        || question_lc.contains(" vs ")
        || question_lc.contains(" v. ")
        || question_lc.contains(" v ")
}

fn mode_fit_score(mode: MmMode, candidate: &AutoMarketCandidate, reward_p75: f64) -> f64 {
    let midpoint = candidate_midpoint_hint(candidate);
    let is_tail = midpoint <= 0.15 || midpoint >= 0.85;
    let is_sports = is_sports_market_slug(candidate.market.slug.as_str());
    let rv_short = candidate.rv_short_bps.unwrap_or(0.0).max(0.0);
    let reward_est = candidate.expected_reward_day_est.max(0.0);
    let competition_score = candidate.competition_score_api.unwrap_or(0.0).max(0.0);
    let mut score = candidate.score.max(0.0);
    match mode {
        MmMode::QuietStack => {
            if is_sports {
                score *= 0.05;
            }
            if is_tail {
                score *= 0.75;
            } else {
                score *= 1.05;
            }
            if rv_short > 10.0 {
                score *= 0.8;
            } else {
                score *= 1.05;
            }
        }
        MmMode::TailBiased => {
            let tail_distance = (midpoint - 0.5).abs();
            if is_tail {
                score *= 1.5;
            } else {
                score *= 0.35;
            }
            if is_sports {
                score *= 0.8;
            }
            score += tail_distance * 50.0;
        }
        MmMode::Spike => {
            if reward_p75 > 0.0 && reward_est >= reward_p75 {
                score *= 1.4;
            } else {
                score *= 0.7;
            }
            if competition_score >= 0.6 {
                score *= 1.1;
            }
            if is_sports {
                score *= 0.9;
            }
        }
        MmMode::SportsPregame => {
            if is_sports {
                score *= 2.2;
            } else {
                score *= 0.01;
            }
        }
    }
    score
}

fn percentile(values: &[f64], quantile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(|a, b| a.total_cmp(b));
    let q = quantile.clamp(0.0, 1.0);
    let idx = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted.get(idx).copied()
}

fn avg_spread_from_books(
    up_bid: Option<f64>,
    up_ask: Option<f64>,
    down_bid: Option<f64>,
    down_ask: Option<f64>,
) -> Option<f64> {
    let mut spreads = Vec::new();
    if let (Some(bid), Some(ask)) = (up_bid, up_ask) {
        if ask > bid {
            spreads.push(ask - bid);
        }
    }
    if let (Some(bid), Some(ask)) = (down_bid, down_ask) {
        if ask > bid {
            spreads.push(ask - bid);
        }
    }
    if spreads.is_empty() {
        None
    } else {
        Some(spreads.iter().sum::<f64>() / spreads.len() as f64)
    }
}

fn midpoint_from_books(
    up_bid: Option<f64>,
    up_ask: Option<f64>,
    down_bid: Option<f64>,
    down_ask: Option<f64>,
) -> Option<f64> {
    let up_mid = match (up_bid, up_ask) {
        (Some(bid), Some(ask)) if ask > bid => Some((bid + ask) / 2.0),
        (Some(bid), None) => Some(bid),
        (None, Some(ask)) => Some(ask),
        _ => None,
    };
    let down_mid = match (down_bid, down_ask) {
        (Some(bid), Some(ask)) if ask > bid => Some((bid + ask) / 2.0),
        (Some(bid), None) => Some(bid),
        (None, Some(ask)) => Some(ask),
        _ => None,
    };
    match (up_mid, down_mid) {
        (Some(up), Some(down)) => Some(((up + (1.0 - down)) / 2.0).clamp(0.0, 1.0)),
        (Some(up), None) => Some(up.clamp(0.0, 1.0)),
        (None, Some(down)) => Some((1.0 - down).clamp(0.0, 1.0)),
        _ => None,
    }
}

fn update_midpoint_history(
    history: &mut Vec<(i64, f64)>,
    now_ms: i64,
    midpoint: f64,
    keep_ms: i64,
) {
    if !midpoint.is_finite() || midpoint <= 0.0 || midpoint >= 1.0 {
        return;
    }
    if history
        .last()
        .map(|(ts, mid)| now_ms.saturating_sub(*ts) < 500 && (midpoint - *mid).abs() < f64::EPSILON)
        .unwrap_or(false)
    {
        return;
    }
    history.push((now_ms, midpoint));
    let min_keep_ts = now_ms.saturating_sub(keep_ms.max(10_000));
    if history.len() > 4_096 {
        history.retain(|(ts, _)| *ts >= min_keep_ts);
    } else {
        while history
            .first()
            .map(|(ts, _)| *ts < min_keep_ts)
            .unwrap_or(false)
        {
            history.remove(0);
        }
    }
}

fn realized_vol_bps(history: &[(i64, f64)], now_ms: i64, window_sec: u64) -> Option<f64> {
    if history.len() < 3 {
        return None;
    }
    let window_ms = i64::try_from(window_sec)
        .ok()
        .unwrap_or(3_600)
        .saturating_mul(1_000)
        .max(1_000);
    let min_ts = now_ms.saturating_sub(window_ms);
    let mut sum_sq = 0.0_f64;
    let mut n = 0_u64;
    let mut prev: Option<(i64, f64)> = None;
    for (ts, mid) in history.iter().copied() {
        if ts < min_ts {
            continue;
        }
        if let Some((_, prev_mid)) = prev {
            if prev_mid > 0.0 && mid > 0.0 {
                let ret_bps = ((mid / prev_mid) - 1.0) * 10_000.0;
                sum_sq += ret_bps * ret_bps;
                n = n.saturating_add(1);
            }
        }
        prev = Some((ts, mid));
    }
    if n < 2 {
        return None;
    }
    Some((sum_sq / n as f64).sqrt())
}

fn compute_inventory_risk(
    midpoint: f64,
    avg_spread: Option<f64>,
    rv_short_bps: Option<f64>,
    competition_score_api: Option<f64>,
) -> f64 {
    let spread_risk = avg_spread.unwrap_or(0.02).max(0.0) * 100.0;
    let rv_risk = rv_short_bps.unwrap_or(0.0).max(0.0) / 50.0;
    let tail_risk = if midpoint < 0.10 || midpoint > 0.90 {
        0.8
    } else {
        0.2
    };
    let competition_risk = competition_score_api.unwrap_or(0.0).max(0.0) * 2.0;
    1.0 + spread_risk + rv_risk + tail_risk + competition_risk
}

fn normalize_competition_score(raw: Option<f64>) -> Option<f64> {
    let raw = raw?;
    if !raw.is_finite() || raw < 0.0 {
        return None;
    }
    if raw <= 1.0 {
        return Some(raw.clamp(0.0, 1.0));
    }
    // rewards feed `market_competitiveness` is unbounded; log-normalize into [0,1].
    Some((raw.ln_1p() / 14.0).clamp(0.0, 1.0))
}

fn competition_level_from_api(raw: Option<f64>, score: Option<f64>) -> &'static str {
    let Some(raw) = raw else {
        return "unknown";
    };
    if raw <= 1.0 {
        let v = score.unwrap_or(raw.clamp(0.0, 1.0));
        if v < 0.33 {
            return "low";
        }
        if v < 0.66 {
            return "medium";
        }
        return "high";
    }
    // Buckets for rewards feed `market_competitiveness` magnitude.
    if raw < 1_000.0 {
        "low"
    } else if raw < 100_000.0 {
        "medium"
    } else {
        "high"
    }
}

pub fn competition_level_from_raw_api(raw: Option<f64>) -> &'static str {
    let score = normalize_competition_score(raw);
    competition_level_from_api(raw, score)
}

#[derive(Debug, Clone, Copy)]
struct OurRewardEstimate {
    our_qmin_est: f64,
    our_share_est: f64,
    expected_reward_day_est: f64,
    reward_efficiency_est: f64,
    capital_locked_est: f64,
}

fn estimate_our_reward_profile(
    reward_rate_hint: f64,
    max_spread_cents: f64,
    midpoint: f64,
    best_bid_up: Option<f64>,
    best_bid_down: Option<f64>,
    our_size_shares: f64,
    competition_qmin: f64,
    single_side_penalty_c: f64,
) -> OurRewardEstimate {
    let up_bid = best_bid_up.unwrap_or(0.0);
    let down_bid = best_bid_down.unwrap_or(0.0);
    let score_yes = reward_level_score(max_spread_cents, (up_bid - midpoint).abs() * 100.0);
    let score_no = reward_level_score(
        max_spread_cents,
        ((1.0 - down_bid) - midpoint).abs() * 100.0,
    );
    let q_yes = score_yes * our_size_shares.max(0.0);
    let q_no = score_no * our_size_shares.max(0.0);
    let our_qmin = qmin_from_components(q_yes, q_no, midpoint, single_side_penalty_c.max(1.0));
    let share = if competition_qmin + our_qmin > 0.0 {
        (our_qmin / (competition_qmin + our_qmin)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let expected_reward_day = reward_rate_hint.max(0.0) * share;
    let capital_locked = (our_size_shares.max(0.0) * up_bid.max(0.0))
        + (our_size_shares.max(0.0) * down_bid.max(0.0));
    let reward_efficiency = if capital_locked > 0.0 {
        expected_reward_day / capital_locked
    } else {
        0.0
    };
    OurRewardEstimate {
        our_qmin_est: our_qmin,
        our_share_est: share,
        expected_reward_day_est: expected_reward_day,
        reward_efficiency_est: reward_efficiency,
        capital_locked_est: capital_locked,
    }
}

fn estimate_competition_qmin(
    up_book: &OrderBook,
    down_book: &OrderBook,
    midpoint: f64,
    max_spread_cents: f64,
    min_size_shares: f64,
    max_levels: usize,
    single_side_penalty_c: f64,
) -> f64 {
    let q_yes = sum_book_q_component(
        up_book.bids.as_slice(),
        midpoint,
        max_spread_cents,
        min_size_shares,
        max_levels,
        true,
    );
    let q_no = sum_book_q_component(
        down_book.bids.as_slice(),
        midpoint,
        max_spread_cents,
        min_size_shares,
        max_levels,
        false,
    );
    qmin_from_components(q_yes, q_no, midpoint, single_side_penalty_c.max(1.0))
}

fn qmin_from_components(q_yes: f64, q_no: f64, midpoint: f64, single_side_penalty_c: f64) -> f64 {
    let q_yes = q_yes.max(0.0);
    let q_no = q_no.max(0.0);
    if midpoint < 0.10 || midpoint > 0.90 {
        q_yes.min(q_no)
    } else {
        q_yes
            .min(q_no)
            .max(q_yes / single_side_penalty_c)
            .max(q_no / single_side_penalty_c)
    }
}

fn sum_book_q_component(
    levels: &[crate::models::OrderBookEntry],
    midpoint: f64,
    max_spread_cents: f64,
    min_size_shares: f64,
    max_levels: usize,
    is_yes_side: bool,
) -> f64 {
    let mut parsed = levels
        .iter()
        .filter_map(|entry| {
            let price = f64::try_from(entry.price).ok()?;
            let size = f64::try_from(entry.size).ok()?;
            (price.is_finite() && size.is_finite() && price > 0.0 && size > 0.0)
                .then_some((price, size))
        })
        .collect::<Vec<_>>();
    parsed.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut total_q = 0.0_f64;
    for (price, size) in parsed.into_iter().take(max_levels.max(1)) {
        if size + 1e-9 < min_size_shares.max(0.0) {
            continue;
        }
        let yes_equiv = if is_yes_side { price } else { 1.0 - price };
        let spread_cents = (yes_equiv - midpoint).abs() * 100.0;
        let level_score = reward_level_score(max_spread_cents, spread_cents);
        if level_score <= 0.0 {
            continue;
        }
        total_q += level_score * size;
    }
    total_q.max(0.0)
}

fn reward_level_score(max_spread_cents: f64, spread_cents: f64) -> f64 {
    let v = max_spread_cents.max(0.0);
    let s = spread_cents.max(0.0);
    if v <= 0.0 || s > v {
        return 0.0;
    }
    let x = (v - s) / v;
    (x * x).clamp(0.0, 1.0)
}

fn infer_timeframe_from_slug(slug: &str) -> Option<Timeframe> {
    let value = slug.trim().to_ascii_lowercase();
    if value.contains("-5m-") {
        Some(Timeframe::M5)
    } else if value.contains("-15m-") {
        Some(Timeframe::M15)
    } else if value.contains("-1h-") {
        Some(Timeframe::H1)
    } else if value.contains("-4h-") {
        Some(Timeframe::H4)
    } else if value.contains("-up-or-down-on-")
        || value.contains("-updown-day-")
        || value.contains("-up-down-on-")
    {
        Some(Timeframe::D1)
    } else {
        None
    }
}

fn infer_symbol(market: &Market) -> String {
    let slug = market.slug.to_ascii_lowercase();
    let question = market.question.to_ascii_lowercase();
    if slug.contains("btc") || question.contains("bitcoin") || question.contains(" btc ") {
        "BTC".to_string()
    } else if slug.contains("eth") || question.contains("ethereum") || question.contains(" eth ") {
        "ETH".to_string()
    } else if slug.contains("sol") || question.contains("solana") || question.contains(" sol ") {
        "SOL".to_string()
    } else if slug.contains("xrp") || question.contains("ripple") || question.contains(" xrp ") {
        "XRP".to_string()
    } else {
        "MISC".to_string()
    }
}

fn extract_max_numeric(root: Option<&serde_json::Value>) -> Option<f64> {
    fn walk(node: &serde_json::Value, out: &mut f64) {
        match node {
            serde_json::Value::Number(n) => {
                if let Some(value) = n.as_f64() {
                    *out = out.max(value);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            serde_json::Value::Object(map) => {
                for value in map.values() {
                    walk(value, out);
                }
            }
            _ => {}
        }
    }

    let mut max_seen = 0.0_f64;
    let value = root?;
    walk(value, &mut max_seen);
    if max_seen > 0.0 {
        Some(max_seen)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sports_market_details_detection_uses_tags_and_slugs() {
        let tags = vec!["College Basketball".to_string()];
        assert!(is_sports_market_details(
            tags.as_slice(),
            "random-slug",
            "fallback-slug"
        ));
        assert!(is_sports_market_details(
            &[],
            "nba-lakers-vs-celtics",
            "fallback-slug"
        ));
        assert!(is_sports_market_details(
            &[],
            "random-slug",
            "sports/cbb/games"
        ));
        assert!(!is_sports_market_details(
            &[],
            "bitcoin-up-or-down-march-18-9pm-et",
            "bitcoin-up-or-down-march-18-9pm-et"
        ));
    }

    #[test]
    fn sports_runtime_detection_checks_slug_or_vs_question() {
        assert!(is_sports_market_runtime(
            "nfl-bills-vs-jets",
            "Who will win?"
        ));
        assert!(is_sports_market_runtime(
            "generic-slug",
            "Howard Bison vs. UMBC Retrievers"
        ));
        assert!(!is_sports_market_runtime(
            "bitcoin-up-or-down-march-18-9pm-et",
            "Will Bitcoin close above $85,000?"
        ));
    }
}
