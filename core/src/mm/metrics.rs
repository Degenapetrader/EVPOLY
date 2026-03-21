use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Default)]
pub struct MmMetrics {
    pub quote_submits: u64,
    pub quote_reprices: u64,
    pub quote_cancels: u64,
    pub skip_count: u64,
    pub stale_pauses: u64,
}

impl MmMetrics {
    pub fn on_submit(&mut self) {
        self.quote_submits = self.quote_submits.saturating_add(1);
    }

    pub fn on_reprice(&mut self) {
        self.quote_reprices = self.quote_reprices.saturating_add(1);
    }

    pub fn on_cancel(&mut self) {
        self.quote_cancels = self.quote_cancels.saturating_add(1);
    }

    pub fn on_skip(&mut self) {
        self.skip_count = self.skip_count.saturating_add(1);
    }

    pub fn on_stale_transition(&mut self) {
        self.stale_pauses = self.stale_pauses.saturating_add(1);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BboSnapshot {
    pub ts_ms: i64,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub bid_size: Option<f64>,
    pub ask_size: Option<f64>,
}

impl BboSnapshot {
    pub fn reference_mid(self) -> Option<f64> {
        let midpoint = match (self.bid, self.ask) {
            (Some(bid), Some(ask))
                if bid.is_finite() && ask.is_finite() && bid > 0.0 && ask > 0.0 =>
            {
                let bid_sz = self.bid_size.unwrap_or(0.0);
                let ask_sz = self.ask_size.unwrap_or(0.0);
                if bid_sz.is_finite() && ask_sz.is_finite() && bid_sz > 0.0 && ask_sz > 0.0 {
                    let denom = bid_sz + ask_sz;
                    if denom > 0.0 {
                        let micro = ((ask * bid_sz) + (bid * ask_sz)) / denom;
                        if micro.is_finite() && micro > 0.0 {
                            return Some(micro);
                        }
                    }
                }
                Some((bid + ask) * 0.5)
            }
            (Some(bid), None) if bid.is_finite() && bid > 0.0 => Some(bid),
            (None, Some(ask)) if ask.is_finite() && ask > 0.0 => Some(ask),
            _ => None,
        }?;
        (midpoint.is_finite() && midpoint > 0.0).then_some(midpoint)
    }
}

#[derive(Debug, Clone, Default)]
pub struct MmPriceHistory {
    snapshots_by_token: HashMap<String, VecDeque<BboSnapshot>>,
}

impl MmPriceHistory {
    pub fn record(
        &mut self,
        token_id: &str,
        ts_ms: i64,
        bid: Option<f64>,
        ask: Option<f64>,
        bid_size: Option<f64>,
        ask_size: Option<f64>,
        retain_ms: i64,
    ) {
        if token_id.trim().is_empty() || ts_ms <= 0 {
            return;
        }
        let keep_after = ts_ms.saturating_sub(retain_ms.max(1_000));
        let entry = self
            .snapshots_by_token
            .entry(token_id.to_ascii_lowercase())
            .or_default();
        entry.push_back(BboSnapshot {
            ts_ms,
            bid,
            ask,
            bid_size,
            ask_size,
        });
        while entry
            .front()
            .map(|snapshot| snapshot.ts_ms < keep_after)
            .unwrap_or(false)
        {
            entry.pop_front();
        }
        if entry.len() > 8_192 {
            let to_drop = entry.len().saturating_sub(8_192);
            for _ in 0..to_drop {
                entry.pop_front();
            }
        }
    }

    pub fn reference_mid_at(
        &self,
        token_id: &str,
        target_ts_ms: i64,
        max_age_ms: i64,
    ) -> Option<f64> {
        let history = self
            .snapshots_by_token
            .get(&token_id.to_ascii_lowercase())?;
        let max_age = max_age_ms.max(1);
        for snapshot in history.iter().rev() {
            if snapshot.ts_ms <= target_ts_ms {
                let age = target_ts_ms.saturating_sub(snapshot.ts_ms);
                if age <= max_age {
                    return snapshot.reference_mid();
                }
                return None;
            }
        }
        None
    }

    pub fn latest_reference_mid(
        &self,
        token_id: &str,
        now_ms: i64,
        max_age_ms: i64,
    ) -> Option<f64> {
        self.reference_mid_at(token_id, now_ms, max_age_ms)
    }

    pub fn recent_snapshot_count(&self, token_id: &str, since_ms: i64) -> usize {
        self.snapshots_by_token
            .get(&token_id.to_ascii_lowercase())
            .map(|history| history.iter().filter(|s| s.ts_ms >= since_ms).count())
            .unwrap_or(0)
    }

    pub fn latest_top_depth_notional(
        &self,
        token_id: &str,
        now_ms: i64,
        max_age_ms: i64,
    ) -> Option<f64> {
        let history = self
            .snapshots_by_token
            .get(&token_id.to_ascii_lowercase())?;
        let max_age = max_age_ms.max(1);
        let snapshot = history.iter().rev().find(|snapshot| {
            now_ms.saturating_sub(snapshot.ts_ms) <= max_age
                && snapshot.bid.is_some()
                && snapshot.ask.is_some()
                && snapshot.bid_size.is_some()
                && snapshot.ask_size.is_some()
        })?;
        let bid = snapshot.bid?;
        let ask = snapshot.ask?;
        let bid_size = snapshot.bid_size?;
        let ask_size = snapshot.ask_size?;
        if !(bid.is_finite()
            && ask.is_finite()
            && bid_size.is_finite()
            && ask_size.is_finite()
            && bid > 0.0
            && ask > 0.0
            && bid_size > 0.0
            && ask_size > 0.0)
        {
            return None;
        }
        Some((bid * bid_size + ask * ask_size).max(0.0))
    }

    pub fn top_depth_notional_percentile(
        &self,
        token_id: &str,
        now_ms: i64,
        lookback_ms: i64,
        percentile: f64,
    ) -> Option<f64> {
        let history = self
            .snapshots_by_token
            .get(&token_id.to_ascii_lowercase())?;
        let keep_after = now_ms.saturating_sub(lookback_ms.max(1_000));
        let mut samples = history
            .iter()
            .filter(|snapshot| snapshot.ts_ms >= keep_after)
            .filter_map(|snapshot| {
                let bid = snapshot.bid?;
                let ask = snapshot.ask?;
                let bid_size = snapshot.bid_size?;
                let ask_size = snapshot.ask_size?;
                if !(bid.is_finite()
                    && ask.is_finite()
                    && bid_size.is_finite()
                    && ask_size.is_finite()
                    && bid > 0.0
                    && ask > 0.0
                    && bid_size > 0.0
                    && ask_size > 0.0)
                {
                    return None;
                }
                Some((bid * bid_size + ask * ask_size).max(0.0))
            })
            .collect::<Vec<_>>();
        if samples.is_empty() {
            return None;
        }
        samples.sort_by(|a, b| a.total_cmp(b));
        quantile_sorted(&samples, percentile.clamp(0.0, 1.0))
    }
}

#[derive(Debug, Clone)]
pub struct MmFillEvent {
    pub event_key: String,
    pub ts_ms: i64,
    pub condition_id: String,
    pub token_id: String,
    pub side: String,
    pub price: f64,
    pub notional_usd: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct MmSubmitAnchor {
    pub ts_ms: i64,
    pub price: f64,
    pub mid_submit: f64,
}

#[derive(Debug, Clone)]
pub struct MmMarkoutSample {
    pub event_key: String,
    pub condition_id: String,
    pub token_id: String,
    pub side: String,
    pub horizon_ms: i64,
    pub fill_ts_ms: i64,
    pub fill_price: f64,
    pub fill_notional_usd: f64,
    pub mid_fill: f64,
    pub mid_horizon: f64,
    pub markout_mid_bps: f64,
    pub markout_fill_bps: f64,
    pub prefill_drift_bps: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct MmMarkoutConditionStats {
    pub sample_count_1s: u64,
    pub sample_notional_1s: f64,
    pub ewma_markout_mid_1s_bps: Option<f64>,
    pub ewma_pickoff_bps: Option<f64>,
    pub ewma_prefill_drift_bps: Option<f64>,
    pub window_1s_bps: VecDeque<f64>,
    pub last_update_ms: i64,
}

impl MmMarkoutConditionStats {
    pub fn dynamic_thresholds(&self, k_gate: f64, k_recover: f64) -> Option<(f64, f64)> {
        if self.window_1s_bps.len() < 10 {
            return None;
        }
        let mut values = self.window_1s_bps.iter().copied().collect::<Vec<_>>();
        values.sort_by(|a, b| a.total_cmp(b));
        let median = quantile_sorted(&values, 0.5)?;
        let mut abs_dev = values
            .iter()
            .map(|value| (value - median).abs())
            .collect::<Vec<_>>();
        abs_dev.sort_by(|a, b| a.total_cmp(b));
        let mad = quantile_sorted(&abs_dev, 0.5)?;
        let sigma = 1.4826 * mad;
        let gate = median - (k_gate.max(0.01) * sigma);
        let recover = median - (k_recover.max(0.01) * sigma);
        Some((gate, recover))
    }
}

#[derive(Debug, Clone, Default)]
pub struct MmConserveState {
    pub active: bool,
    pub severity: f64,
    pub active_since_ms: i64,
}

#[derive(Debug, Clone)]
struct PendingMarkoutFill {
    event_key: String,
    condition_id: String,
    token_id: String,
    side: String,
    side_mult: f64,
    fill_ts_ms: i64,
    fill_price: f64,
    fill_notional_usd: f64,
    mid_fill: f64,
    prefill_drift_bps: Option<f64>,
    horizons_done: [bool; 3],
}

#[derive(Debug, Clone, Default)]
pub struct MmMarkoutTracker {
    processed_event_keys: HashSet<String>,
    pending_fills: HashMap<String, PendingMarkoutFill>,
    submit_anchor_by_order: HashMap<String, MmSubmitAnchor>,
    stats_by_condition: HashMap<String, MmMarkoutConditionStats>,
}

impl MmMarkoutTracker {
    pub fn remember_submit_anchor(
        &mut self,
        order_id: &str,
        ts_ms: i64,
        order_price: f64,
        mid_submit: f64,
    ) {
        if order_id.trim().is_empty() || ts_ms <= 0 {
            return;
        }
        let key = order_id.to_ascii_lowercase();
        self.submit_anchor_by_order
            .entry(key)
            .or_insert(MmSubmitAnchor {
                ts_ms,
                price: order_price,
                mid_submit,
            });
        if self.submit_anchor_by_order.len() > 50_000 {
            let mut rows = self
                .submit_anchor_by_order
                .iter()
                .map(|(order_id, anchor)| (order_id.clone(), anchor.ts_ms))
                .collect::<Vec<_>>();
            rows.sort_by_key(|(_, ts_ms)| *ts_ms);
            let drop_count = rows.len().saturating_sub(25_000);
            for (order_id, _) in rows.into_iter().take(drop_count) {
                self.submit_anchor_by_order.remove(order_id.as_str());
            }
        }
    }

    pub fn ingest_fill_event(
        &mut self,
        fill: MmFillEvent,
        price_history: &MmPriceHistory,
        max_book_age_ms: i64,
    ) -> Result<(), &'static str> {
        if self.processed_event_keys.contains(fill.event_key.as_str()) {
            return Ok(());
        }
        if fill.token_id.trim().is_empty()
            || fill.condition_id.trim().is_empty()
            || fill.ts_ms <= 0
            || !fill.price.is_finite()
            || fill.price <= 0.0
        {
            self.processed_event_keys.insert(fill.event_key);
            return Err("invalid_fill");
        }
        let Some(mid_fill) =
            price_history.reference_mid_at(fill.token_id.as_str(), fill.ts_ms, max_book_age_ms)
        else {
            self.processed_event_keys.insert(fill.event_key);
            return Err("stale_book");
        };
        let side_mult = side_multiplier(fill.side.as_str());
        let order_id = order_id_from_event_key(fill.event_key.as_str());
        let prefill_drift_bps = order_id
            .as_deref()
            .and_then(|order_id| self.submit_anchor_by_order.get(order_id))
            .and_then(|anchor| {
                if anchor.mid_submit.is_finite() && anchor.mid_submit > 0.0 {
                    Some(
                        side_mult * ((mid_fill - anchor.mid_submit) / anchor.mid_submit) * 10_000.0,
                    )
                } else {
                    None
                }
            });
        let pending = PendingMarkoutFill {
            event_key: fill.event_key.clone(),
            condition_id: fill.condition_id,
            token_id: fill.token_id,
            side: fill.side,
            side_mult,
            fill_ts_ms: fill.ts_ms,
            fill_price: fill.price,
            fill_notional_usd: fill.notional_usd.max(0.0),
            mid_fill,
            prefill_drift_bps,
            horizons_done: [false, false, false],
        };
        self.processed_event_keys.insert(fill.event_key.clone());
        self.pending_fills.insert(fill.event_key, pending);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn poll_due_samples(
        &mut self,
        now_ms: i64,
        price_history: &MmPriceHistory,
        max_book_age_ms: i64,
        winsor_bps: f64,
        ewma_halflife_fills: f64,
        dynamic_window_limit: usize,
        use_pickoff: bool,
        use_prefill_drift: bool,
    ) -> Vec<MmMarkoutSample> {
        let mut out = Vec::new();
        let mut finished_keys: Vec<String> = Vec::new();
        for (event_key, pending) in self.pending_fills.iter_mut() {
            for (idx, horizon_ms) in MARKOUT_HORIZONS_MS.iter().enumerate() {
                if pending.horizons_done[idx] {
                    continue;
                }
                let due_ts = pending.fill_ts_ms.saturating_add(*horizon_ms);
                if now_ms < due_ts {
                    continue;
                }
                let Some(mid_horizon) = price_history.reference_mid_at(
                    pending.token_id.as_str(),
                    now_ms,
                    max_book_age_ms,
                ) else {
                    continue;
                };
                let markout_mid_raw = pending.side_mult
                    * ((mid_horizon - pending.mid_fill) / pending.mid_fill)
                    * 10_000.0;
                let markout_fill_raw = pending.side_mult
                    * ((mid_horizon - pending.fill_price) / pending.fill_price)
                    * 10_000.0;
                let markout_mid_bps = winsor(markout_mid_raw, winsor_bps);
                let markout_fill_bps = winsor(markout_fill_raw, winsor_bps);
                let sample = MmMarkoutSample {
                    event_key: pending.event_key.clone(),
                    condition_id: pending.condition_id.clone(),
                    token_id: pending.token_id.clone(),
                    side: pending.side.clone(),
                    horizon_ms: *horizon_ms,
                    fill_ts_ms: pending.fill_ts_ms,
                    fill_price: pending.fill_price,
                    fill_notional_usd: pending.fill_notional_usd,
                    mid_fill: pending.mid_fill,
                    mid_horizon,
                    markout_mid_bps,
                    markout_fill_bps,
                    prefill_drift_bps: pending.prefill_drift_bps,
                };
                if *horizon_ms == 1_000 {
                    let stats = self
                        .stats_by_condition
                        .entry(pending.condition_id.clone())
                        .or_default();
                    stats.sample_count_1s = stats.sample_count_1s.saturating_add(1);
                    stats.sample_notional_1s += pending.fill_notional_usd.max(0.0);
                    stats.last_update_ms = now_ms;
                    let ewma_weight = pending.fill_notional_usd.max(1.0);
                    stats.ewma_markout_mid_1s_bps = ewma_update(
                        stats.ewma_markout_mid_1s_bps,
                        markout_mid_bps,
                        ewma_halflife_fills,
                        ewma_weight,
                    );
                    if use_pickoff {
                        if let Some(pickoff_bps) = pending.prefill_drift_bps {
                            stats.ewma_pickoff_bps = ewma_update(
                                stats.ewma_pickoff_bps,
                                winsor(pickoff_bps, winsor_bps),
                                ewma_halflife_fills,
                                ewma_weight,
                            );
                        }
                    }
                    if use_prefill_drift {
                        if let Some(prefill_drift_bps) = pending.prefill_drift_bps {
                            stats.ewma_prefill_drift_bps = ewma_update(
                                stats.ewma_prefill_drift_bps,
                                winsor(prefill_drift_bps, winsor_bps),
                                ewma_halflife_fills,
                                ewma_weight,
                            );
                        }
                    }
                    stats.window_1s_bps.push_back(markout_mid_bps);
                    while stats.window_1s_bps.len() > dynamic_window_limit.max(10) {
                        stats.window_1s_bps.pop_front();
                    }
                }
                pending.horizons_done[idx] = true;
                out.push(sample);
            }

            let all_done = pending.horizons_done.iter().all(|done| *done);
            let too_old = now_ms.saturating_sub(pending.fill_ts_ms) >= 60_000;
            if all_done || too_old {
                finished_keys.push(event_key.clone());
            }
        }
        for event_key in finished_keys {
            self.pending_fills.remove(event_key.as_str());
        }
        if self.processed_event_keys.len() > 200_000 {
            self.processed_event_keys.clear();
        }
        out
    }

    pub fn condition_stats(&self, condition_id: &str) -> Option<&MmMarkoutConditionStats> {
        self.stats_by_condition.get(condition_id)
    }

    pub fn condition_count(&self) -> usize {
        self.stats_by_condition.len()
    }

    pub fn pending_fill_count(&self) -> usize {
        self.pending_fills.len()
    }
}

pub const MARKOUT_HORIZONS_MS: [i64; 3] = [100, 1_000, 5_000];

fn side_multiplier(side: &str) -> f64 {
    if side.eq_ignore_ascii_case("SELL") {
        -1.0
    } else {
        1.0
    }
}

fn order_id_from_event_key(event_key: &str) -> Option<String> {
    for prefix in ["entry_fill_order:", "entry_fill_reconciled:"] {
        if let Some(order_id) = event_key.strip_prefix(prefix) {
            let trimmed = order_id.trim().to_ascii_lowercase();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

fn winsor(value: f64, cap_abs: f64) -> f64 {
    let cap = cap_abs.abs().max(1.0);
    value.clamp(-cap, cap)
}

fn ewma_update(previous: Option<f64>, value: f64, halflife_fills: f64, weight: f64) -> Option<f64> {
    if !value.is_finite() {
        return previous;
    }
    let half = halflife_fills.max(1.0);
    let base_alpha = 1.0 - 0.5_f64.powf(1.0 / half);
    let weight_mult = weight.max(1.0).ln_1p().clamp(0.25, 4.0);
    let alpha = (base_alpha * weight_mult).clamp(0.001, 0.95);
    match previous {
        Some(prev) if prev.is_finite() => Some((alpha * value) + ((1.0 - alpha) * prev)),
        _ => Some(value),
    }
}

fn quantile_sorted(values: &[f64], q: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let clamped_q = q.clamp(0.0, 1.0);
    let last = values.len().saturating_sub(1);
    if last == 0 {
        return values.first().copied();
    }
    let pos = (last as f64) * clamped_q;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        values.get(lo).copied()
    } else {
        let lo_v = values.get(lo).copied()?;
        let hi_v = values.get(hi).copied()?;
        let w = pos - (lo as f64);
        Some((lo_v * (1.0 - w)) + (hi_v * w))
    }
}
