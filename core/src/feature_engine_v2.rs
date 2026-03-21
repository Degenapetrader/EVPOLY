use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::FeatureEngineV2Config;
use crate::signal_state::{SignalSourceAgesMs, SignalState};
use crate::strategy::Timeframe;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeatureRuntimeContext {
    pub active_positions_long: u32,
    pub active_positions_short: u32,
    pub remaining_cap_usd: Option<f64>,
    pub remaining_slots: Option<u32>,
    pub pending_order_pressure: Option<f64>,
    pub settled_win_rate_tf: Option<f64>,
    pub settled_avg_pnl_pct_tf: Option<f64>,
    pub decision_streak: Option<i32>,
    pub regime_fit_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HlL4FeatureGroup {
    pub windows_seconds: Vec<u32>,
    pub signed_notional_sum_by_window: BTreeMap<String, f64>,
    pub cvd_slope: Option<f64>,
    pub cvd_acceleration: Option<f64>,
    pub burst_zscore: Option<f64>,
    pub liquidation_impulse: Option<f64>,
    pub large_trade_concentration: Option<f64>,
    pub side_imbalance: Option<f64>,
    pub flow_persistence: Option<f64>,
    pub reversal_risk_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrendStructureFeatureGroup {
    pub adx: Option<f64>,
    pub di_plus: Option<f64>,
    pub di_minus: Option<f64>,
    pub trend_score: Option<f64>,
    pub support: Option<f64>,
    pub resistance: Option<f64>,
    pub poc: Option<f64>,
    pub vah: Option<f64>,
    pub val: Option<f64>,
    pub trend_alignment_score: Option<f64>,
    pub adx_strength_norm: Option<f64>,
    pub support_distance_bps: Option<f64>,
    pub resistance_distance_bps: Option<f64>,
    pub poc_distance_bps: Option<f64>,
    pub structure_compression_score: Option<f64>,
    pub breakout_proximity_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExternalFeatureGroup {
    pub volume_ratio: Option<f64>,
    pub signed_notional_sum_by_window: BTreeMap<String, f64>,
    pub signed_notional_anchor: Option<f64>,
    pub whale_buys: u32,
    pub whale_sells: u32,
    pub whale_net_usd: f64,
    pub side_imbalance: Option<f64>,
    pub flow_persistence: Option<f64>,
    pub burst_zscore: Option<f64>,
    pub abnormal_flow_max_abs_z: Option<f64>,
    pub microflow_consistency_vs_hl: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeRiskFeatureGroup {
    pub active_positions_long: u32,
    pub active_positions_short: u32,
    pub remaining_cap_usd: Option<f64>,
    pub remaining_slots: Option<u32>,
    pub pending_order_pressure: Option<f64>,
    pub settled_win_rate_tf: Option<f64>,
    pub settled_avg_pnl_pct_tf: Option<f64>,
    pub decision_streak: Option<i32>,
    pub regime_fit_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FreshnessFeatureGroup {
    pub newest_source_update_ms: i64,
    pub source_ages_ms: SignalSourceAgesMs,
    pub stale_flags: BTreeMap<String, bool>,
    pub validity_flags: BTreeMap<String, bool>,
    pub quality_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ComponentScores {
    pub hl_l4_weight: f64,
    pub trend_structure_weight: f64,
    pub external_weight: f64,
    pub hl_l4_score: f64,
    pub trend_structure_score: f64,
    pub external_score: f64,
    pub composite_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturePackCore {
    pub timeframe: Timeframe,
    pub feature_pack_version: String,
    pub hl_l4: HlL4FeatureGroup,
    pub trend_structure: TrendStructureFeatureGroup,
    pub external: ExternalFeatureGroup,
    pub runtime_risk: RuntimeRiskFeatureGroup,
    pub freshness: FreshnessFeatureGroup,
    pub component_scores: ComponentScores,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturePack5m {
    #[serde(flatten)]
    pub core: FeaturePackCore,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturePack15m {
    #[serde(flatten)]
    pub core: FeaturePackCore,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturePack1h {
    #[serde(flatten)]
    pub core: FeaturePackCore,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturePack4h {
    #[serde(flatten)]
    pub core: FeaturePackCore,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "pack", content = "data")]
pub enum FeaturePack {
    M5(FeaturePack5m),
    M15(FeaturePack15m),
    H1(FeaturePack1h),
    H4(FeaturePack4h),
}

#[derive(Debug, Clone)]
pub struct FeatureEngineV2 {
    cfg: FeatureEngineV2Config,
}

impl FeatureEngineV2 {
    pub fn from_env() -> Self {
        Self {
            cfg: FeatureEngineV2Config::from_env(),
        }
    }

    pub fn new(cfg: FeatureEngineV2Config) -> Self {
        Self { cfg }
    }

    pub fn config(&self) -> &FeatureEngineV2Config {
        &self.cfg
    }

    pub fn build_feature_pack(
        &self,
        timeframe: Timeframe,
        snapshot: &SignalState,
        runtime: &FeatureRuntimeContext,
        now_ms: i64,
    ) -> FeaturePack {
        let core = self.build_core(timeframe, snapshot, runtime, now_ms);
        match timeframe {
            Timeframe::M5 => FeaturePack::M5(FeaturePack5m { core }),
            Timeframe::M15 => FeaturePack::M15(FeaturePack15m { core }),
            Timeframe::H1 => FeaturePack::H1(FeaturePack1h { core }),
            Timeframe::H4 => FeaturePack::H4(FeaturePack4h { core }),
            Timeframe::D1 => FeaturePack::H4(FeaturePack4h { core }),
        }
    }

    pub fn build_feature_pack_5m(
        &self,
        snapshot: &SignalState,
        runtime: &FeatureRuntimeContext,
        now_ms: i64,
    ) -> FeaturePack5m {
        FeaturePack5m {
            core: self.build_core(Timeframe::M5, snapshot, runtime, now_ms),
        }
    }

    pub fn build_feature_pack_15m(
        &self,
        snapshot: &SignalState,
        runtime: &FeatureRuntimeContext,
        now_ms: i64,
    ) -> FeaturePack15m {
        FeaturePack15m {
            core: self.build_core(Timeframe::M15, snapshot, runtime, now_ms),
        }
    }

    pub fn build_feature_pack_1h(
        &self,
        snapshot: &SignalState,
        runtime: &FeatureRuntimeContext,
        now_ms: i64,
    ) -> FeaturePack1h {
        FeaturePack1h {
            core: self.build_core(Timeframe::H1, snapshot, runtime, now_ms),
        }
    }

    fn build_core(
        &self,
        timeframe: Timeframe,
        snapshot: &SignalState,
        runtime: &FeatureRuntimeContext,
        now_ms: i64,
    ) -> FeaturePackCore {
        let price = snapshot
            .hl_flow
            .last_price
            .or(snapshot.binance_flow.last_price)
            .unwrap_or(0.0);
        let windows = windows_for_timeframe(timeframe);

        let mut signed_notional_sum_by_window: BTreeMap<String, f64> = BTreeMap::new();
        for window in windows.iter().copied() {
            let value = hl_signed_notional_for_window(snapshot, window)
                .or(snapshot.hl_flow.cvd_5m)
                .unwrap_or(0.0);
            signed_notional_sum_by_window.insert(window.to_string(), value);
        }

        let source_ages_ms = snapshot.source_ages_ms(now_ms);
        let freshness = build_freshness(snapshot, source_ages_ms.clone(), timeframe);
        let trend_structure = build_trend_structure(snapshot, price);
        let external = build_external(snapshot, timeframe);
        let runtime_risk = RuntimeRiskFeatureGroup {
            active_positions_long: runtime.active_positions_long,
            active_positions_short: runtime.active_positions_short,
            remaining_cap_usd: runtime.remaining_cap_usd,
            remaining_slots: runtime.remaining_slots,
            pending_order_pressure: runtime.pending_order_pressure,
            settled_win_rate_tf: runtime.settled_win_rate_tf,
            settled_avg_pnl_pct_tf: runtime.settled_avg_pnl_pct_tf,
            decision_streak: runtime.decision_streak,
            regime_fit_score: runtime.regime_fit_score,
        };

        let hl_l4 = HlL4FeatureGroup {
            windows_seconds: windows,
            signed_notional_sum_by_window,
            cvd_slope: snapshot.hl_flow.cvd_slope,
            cvd_acceleration: snapshot.hl_flow.cvd_acceleration,
            burst_zscore: snapshot
                .hl_flow
                .burst_zscore
                .or_else(|| snapshot.abnormal_events.last().map(|ev| ev.max_abs_z)),
            liquidation_impulse: snapshot
                .hl_flow
                .liquidation_impulse
                .or(snapshot.hl_flow.liquidation_pressure_5m),
            large_trade_concentration: snapshot.hl_flow.large_trade_concentration,
            side_imbalance: snapshot.hl_flow.side_imbalance,
            flow_persistence: snapshot.hl_flow.flow_persistence,
            reversal_risk_score: snapshot.hl_flow.reversal_risk_score,
        };

        let component_scores =
            build_component_scores(&self.cfg, timeframe, &hl_l4, &trend_structure, &external);

        FeaturePackCore {
            timeframe,
            feature_pack_version: self.cfg.version.clone(),
            hl_l4,
            trend_structure,
            external,
            runtime_risk,
            freshness,
            component_scores,
        }
    }
}

fn build_trend_structure(snapshot: &SignalState, price: f64) -> TrendStructureFeatureGroup {
    let support_distance_bps = basis_points_distance(price, snapshot.sr_levels.support);
    let resistance_distance_bps = basis_points_distance(price, snapshot.sr_levels.resistance);
    let poc_distance_bps = basis_points_distance(price, snapshot.volume_profile.poc);
    let trend_alignment_score = match (snapshot.trend.di_plus, snapshot.trend.di_minus) {
        (Some(plus), Some(minus)) => Some((plus - minus) / plus.max(minus).max(1.0)),
        _ => None,
    };
    let adx_strength_norm = snapshot.trend.adx.map(|adx| (adx / 50.0).clamp(0.0, 1.0));
    let structure_compression_score = match (support_distance_bps, resistance_distance_bps) {
        (Some(s), Some(r)) => Some((s + r).max(1.0).recip()),
        _ => None,
    };
    let breakout_proximity_score = match (support_distance_bps, resistance_distance_bps) {
        (Some(s), Some(r)) => Some(1.0 / (s.min(r).max(1.0))),
        _ => None,
    };

    TrendStructureFeatureGroup {
        adx: snapshot.trend.adx,
        di_plus: snapshot.trend.di_plus,
        di_minus: snapshot.trend.di_minus,
        trend_score: snapshot.trend.trend_score,
        support: snapshot.sr_levels.support,
        resistance: snapshot.sr_levels.resistance,
        poc: snapshot.volume_profile.poc,
        vah: snapshot.volume_profile.vah,
        val: snapshot.volume_profile.val,
        trend_alignment_score,
        adx_strength_norm,
        support_distance_bps,
        resistance_distance_bps,
        poc_distance_bps,
        structure_compression_score,
        breakout_proximity_score,
    }
}

fn build_external(snapshot: &SignalState, timeframe: Timeframe) -> ExternalFeatureGroup {
    let windows = windows_for_timeframe(timeframe);
    let abnormal_flow_max_abs_z = snapshot.abnormal_events.last().map(|event| event.max_abs_z);
    let mut signed_notional_sum_by_window = BTreeMap::new();
    let mut consistency_sum = 0.0_f64;
    let mut consistency_n = 0_u32;
    let mut anchor_signed = None;
    for (idx, window) in windows.iter().copied().enumerate() {
        let ext_signed = binance_signed_notional_for_window(snapshot, window);
        let hl_signed = hl_signed_notional_for_window(snapshot, window);
        if idx + 1 == windows.len() {
            anchor_signed = ext_signed;
        }
        signed_notional_sum_by_window.insert(window.to_string(), ext_signed.unwrap_or(0.0));
        if let (Some(ext), Some(hl)) = (ext_signed, hl_signed) {
            if ext.abs() > f64::EPSILON && hl.abs() > f64::EPSILON {
                let aligned = if ext.signum() == hl.signum() {
                    1.0
                } else {
                    -1.0
                };
                consistency_sum += aligned;
                consistency_n = consistency_n.saturating_add(1);
            }
        }
    }
    let microflow_consistency_vs_hl = if consistency_n > 0 {
        Some((consistency_sum / consistency_n as f64).clamp(-1.0, 1.0))
    } else {
        None
    };

    ExternalFeatureGroup {
        volume_ratio: snapshot.binance_flow.volume_ratio_1m,
        signed_notional_sum_by_window,
        signed_notional_anchor: anchor_signed,
        whale_buys: snapshot.binance_flow.whale_buys_5m,
        whale_sells: snapshot.binance_flow.whale_sells_5m,
        whale_net_usd: snapshot.binance_flow.whale_net_usd_5m,
        side_imbalance: snapshot.binance_flow.side_imbalance_5m,
        flow_persistence: snapshot.binance_flow.flow_persistence,
        burst_zscore: snapshot.binance_flow.burst_zscore,
        abnormal_flow_max_abs_z,
        microflow_consistency_vs_hl,
    }
}

fn build_freshness(
    snapshot: &SignalState,
    source_ages_ms: SignalSourceAgesMs,
    timeframe: Timeframe,
) -> FreshnessFeatureGroup {
    // Trend should stay relatively strict while SR/VP can refresh slower than tick-level flow.
    let (trend_stale_ms, srvp_stale_ms, flow_stale_ms) = match timeframe {
        Timeframe::M5 => (60_000_i64, 240_000_i64, 60_000_i64),
        Timeframe::M15 => (180_000_i64, 480_000_i64, 90_000_i64),
        Timeframe::H1 => (300_000_i64, 900_000_i64, 120_000_i64),
        Timeframe::H4 => (300_000_i64, 900_000_i64, 120_000_i64),
        Timeframe::D1 => (300_000_i64, 900_000_i64, 120_000_i64),
    };
    let mut stale_flags = BTreeMap::new();
    stale_flags.insert(
        "trend".to_string(),
        source_ages_ms
            .trend_ms
            .map(|v| v > trend_stale_ms)
            .unwrap_or(true),
    );
    stale_flags.insert(
        "sr_levels".to_string(),
        source_ages_ms
            .sr_levels_ms
            .map(|v| v > srvp_stale_ms)
            .unwrap_or(true),
    );
    stale_flags.insert(
        "volume_profile".to_string(),
        source_ages_ms
            .volume_profile_ms
            .map(|v| v > srvp_stale_ms)
            .unwrap_or(true),
    );
    stale_flags.insert(
        "hl_flow".to_string(),
        source_ages_ms
            .hl_flow_ms
            .map(|v| v > flow_stale_ms)
            .unwrap_or(true),
    );
    stale_flags.insert(
        "binance_flow".to_string(),
        source_ages_ms
            .binance_flow_ms
            .map(|v| v > flow_stale_ms)
            .unwrap_or(true),
    );
    stale_flags.insert(
        "abnormal".to_string(),
        source_ages_ms
            .abnormal_ms
            .map(|v| v > flow_stale_ms)
            .unwrap_or(true),
    );
    stale_flags.insert(
        "whale".to_string(),
        source_ages_ms
            .whale_ms
            .map(|v| v > flow_stale_ms)
            .unwrap_or(true),
    );

    let mut validity_flags = BTreeMap::new();
    validity_flags.insert("adx".to_string(), snapshot.trend.adx.is_some());
    validity_flags.insert("support".to_string(), snapshot.sr_levels.support.is_some());
    validity_flags.insert(
        "resistance".to_string(),
        snapshot.sr_levels.resistance.is_some(),
    );
    validity_flags.insert("poc".to_string(), snapshot.volume_profile.poc.is_some());
    validity_flags.insert("cvd_5m".to_string(), snapshot.hl_flow.cvd_5m.is_some());

    let stale_count = stale_flags.values().filter(|is_stale| **is_stale).count() as f64;
    let invalid_count = validity_flags
        .values()
        .filter(|is_valid| !**is_valid)
        .count() as f64;
    let denom = (stale_flags.len() + validity_flags.len()) as f64;
    let quality_score = if denom <= 0.0 {
        0.0
    } else {
        (1.0 - ((stale_count + invalid_count) / denom)).clamp(0.0, 1.0)
    };

    FreshnessFeatureGroup {
        newest_source_update_ms: snapshot.newest_source_update_ms(),
        source_ages_ms,
        stale_flags,
        validity_flags,
        quality_score,
    }
}

fn build_component_scores(
    cfg: &FeatureEngineV2Config,
    timeframe: Timeframe,
    hl_l4: &HlL4FeatureGroup,
    trend_structure: &TrendStructureFeatureGroup,
    external: &ExternalFeatureGroup,
) -> ComponentScores {
    let (hl_l4_weight, trend_structure_weight, external_weight) = match timeframe {
        Timeframe::M5 => (cfg.hl_l4_weight_5m, 0.20, 0.15),
        Timeframe::M15 => (cfg.hl_l4_weight_15m, 0.30, 0.20),
        Timeframe::H1 => (cfg.hl_l4_weight_1h, 0.40, 0.25),
        Timeframe::H4 => (cfg.hl_l4_weight_1h, 0.40, 0.25),
        Timeframe::D1 => (cfg.hl_l4_weight_1h, 0.40, 0.25),
    };
    let hl_l4_score = score_hl_l4(hl_l4);
    let trend_structure_score = score_trend_structure(trend_structure);
    let external_score = score_external(external);
    let composite_score = (hl_l4_score * hl_l4_weight)
        + (trend_structure_score * trend_structure_weight)
        + (external_score * external_weight);

    ComponentScores {
        hl_l4_weight,
        trend_structure_weight,
        external_weight,
        hl_l4_score,
        trend_structure_score,
        external_score,
        composite_score,
    }
}

fn windows_for_timeframe(timeframe: Timeframe) -> Vec<u32> {
    match timeframe {
        Timeframe::M5 => vec![15, 30, 60, 180, 300],
        Timeframe::M15 => vec![60, 180, 300, 600, 900],
        Timeframe::H1 => vec![300, 900, 1800, 3600],
        Timeframe::H4 => vec![300, 900, 1800, 3600],
        Timeframe::D1 => vec![300, 900, 1800, 3600],
    }
}

fn hl_signed_notional_for_window(snapshot: &SignalState, window_seconds: u32) -> Option<f64> {
    match window_seconds {
        15 => snapshot.hl_flow.signed_notional_15s,
        30 => snapshot.hl_flow.signed_notional_30s,
        60 => snapshot.hl_flow.signed_notional_60s,
        180 => snapshot.hl_flow.signed_notional_180s,
        300 => snapshot.hl_flow.signed_notional_300s,
        600 => snapshot.hl_flow.signed_notional_600s,
        900 => snapshot.hl_flow.signed_notional_900s,
        1800 => snapshot.hl_flow.signed_notional_1800s,
        3600 => snapshot.hl_flow.signed_notional_3600s,
        _ => None,
    }
}

fn binance_signed_notional_for_window(snapshot: &SignalState, window_seconds: u32) -> Option<f64> {
    match window_seconds {
        15 => snapshot.binance_flow.signed_notional_15s,
        30 => snapshot.binance_flow.signed_notional_30s,
        60 => snapshot.binance_flow.signed_notional_60s,
        180 => snapshot.binance_flow.signed_notional_180s,
        300 => snapshot.binance_flow.signed_notional_300s,
        600 => snapshot.binance_flow.signed_notional_600s,
        900 => snapshot.binance_flow.signed_notional_900s,
        1800 => snapshot.binance_flow.signed_notional_1800s,
        3600 => snapshot.binance_flow.signed_notional_3600s,
        _ => None,
    }
}

fn score_hl_l4(hl_l4: &HlL4FeatureGroup) -> f64 {
    let slope = hl_l4
        .cvd_slope
        .map(|v| (v.abs() / 20_000.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let accel = hl_l4
        .cvd_acceleration
        .map(|v| (v.abs() / 20_000.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let burst = hl_l4
        .burst_zscore
        .map(|v| (v.abs() / 3.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let liq = hl_l4
        .liquidation_impulse
        .map(|v| (v.abs() / 50_000.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let concentration = hl_l4
        .large_trade_concentration
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let imbalance = hl_l4
        .side_imbalance
        .map(|v| v.abs())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let persistence = hl_l4
        .flow_persistence
        .map(|v| v.abs())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let reversal_penalty = hl_l4.reversal_risk_score.unwrap_or(0.0).clamp(0.0, 1.0);
    ((0.18 * slope)
        + (0.12 * accel)
        + (0.16 * burst)
        + (0.14 * liq)
        + (0.10 * concentration)
        + (0.12 * imbalance)
        + (0.18 * persistence)
        - (0.15 * reversal_penalty))
        .clamp(0.0, 1.0)
}

fn score_trend_structure(trend: &TrendStructureFeatureGroup) -> f64 {
    let adx = trend.adx_strength_norm.unwrap_or(0.0).clamp(0.0, 1.0);
    let alignment = trend
        .trend_alignment_score
        .map(|v| v.abs())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let trend_score = trend
        .trend_score
        .map(|v| (v.abs() / 100.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let breakout = trend
        .breakout_proximity_score
        .map(|v| (v * 100.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let compression = trend
        .structure_compression_score
        .map(|v| (v * 100.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    ((0.35 * adx)
        + (0.25 * alignment)
        + (0.2 * trend_score)
        + (0.1 * breakout)
        + (0.1 * compression))
        .clamp(0.0, 1.0)
}

fn score_external(external: &ExternalFeatureGroup) -> f64 {
    let volume = external
        .volume_ratio
        .map(|ratio| ((ratio - 1.0) / 2.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let whale_net = (external.whale_net_usd.abs() / 500_000.0).clamp(0.0, 1.0);
    let whale_activity =
        ((external.whale_buys + external.whale_sells) as f64 / 8.0).clamp(0.0, 1.0);
    let imbalance = external
        .side_imbalance
        .map(|v| v.abs())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let persistence = external
        .flow_persistence
        .map(|v| v.abs())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let burst = external
        .burst_zscore
        .or(external.abnormal_flow_max_abs_z)
        .map(|v| (v.abs() / 3.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    let consistency = external
        .microflow_consistency_vs_hl
        .map(|v| ((v + 1.0) / 2.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    ((0.20 * volume)
        + (0.15 * whale_net)
        + (0.10 * whale_activity)
        + (0.15 * imbalance)
        + (0.15 * persistence)
        + (0.15 * burst)
        + (0.10 * consistency))
        .clamp(0.0, 1.0)
}

fn basis_points_distance(price: f64, level: Option<f64>) -> Option<f64> {
    if price <= 0.0 {
        return None;
    }
    level.map(|v| ((v - price).abs() / price) * 10_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal_state::SignalState;

    fn seeded_snapshot(now_ms: i64) -> SignalState {
        let mut snapshot = SignalState::default();
        snapshot.trend.adx = Some(24.0);
        snapshot.trend.di_plus = Some(20.0);
        snapshot.trend.di_minus = Some(12.0);
        snapshot.trend.trend_score = Some(55.0);
        snapshot.sr_levels.support = Some(67_550.0);
        snapshot.sr_levels.resistance = Some(68_420.0);
        snapshot.volume_profile.poc = Some(68_040.0);
        snapshot.volume_profile.vah = Some(68_190.0);
        snapshot.volume_profile.val = Some(67_890.0);

        snapshot.hl_flow.last_price = Some(68_020.0);
        snapshot.hl_flow.cvd_5m = Some(120_000.0);
        snapshot.hl_flow.liquidation_pressure_5m = Some(-0.2);
        snapshot.hl_flow.signed_notional_15s = Some(1_500.0);
        snapshot.hl_flow.signed_notional_30s = Some(3_100.0);
        snapshot.hl_flow.signed_notional_60s = Some(6_200.0);
        snapshot.hl_flow.signed_notional_180s = Some(18_300.0);
        snapshot.hl_flow.signed_notional_300s = Some(30_500.0);
        snapshot.hl_flow.signed_notional_600s = Some(61_000.0);
        snapshot.hl_flow.signed_notional_900s = Some(92_000.0);
        snapshot.hl_flow.signed_notional_1800s = Some(185_000.0);
        snapshot.hl_flow.signed_notional_3600s = Some(371_000.0);
        snapshot.hl_flow.cvd_slope = Some(0.18);
        snapshot.hl_flow.cvd_acceleration = Some(0.05);
        snapshot.hl_flow.burst_zscore = Some(2.4);
        snapshot.hl_flow.liquidation_impulse = Some(-0.15);
        snapshot.hl_flow.large_trade_concentration = Some(0.42);
        snapshot.hl_flow.side_imbalance = Some(0.27);
        snapshot.hl_flow.flow_persistence = Some(0.63);
        snapshot.hl_flow.reversal_risk_score = Some(0.12);

        snapshot.binance_flow.last_price = Some(68_018.0);
        snapshot.binance_flow.last_trade_ts_ms = Some(now_ms - 900);
        snapshot.binance_flow.volume_ratio_1m = Some(1.35);
        snapshot.binance_flow.signed_notional_15s = Some(1_200.0);
        snapshot.binance_flow.signed_notional_30s = Some(2_800.0);
        snapshot.binance_flow.signed_notional_60s = Some(5_500.0);
        snapshot.binance_flow.signed_notional_180s = Some(17_700.0);
        snapshot.binance_flow.signed_notional_300s = Some(29_000.0);
        snapshot.binance_flow.signed_notional_600s = Some(58_000.0);
        snapshot.binance_flow.signed_notional_900s = Some(86_000.0);
        snapshot.binance_flow.signed_notional_1800s = Some(171_000.0);
        snapshot.binance_flow.signed_notional_3600s = Some(355_000.0);
        snapshot.binance_flow.side_imbalance_5m = Some(0.24);
        snapshot.binance_flow.flow_persistence = Some(0.58);
        snapshot.binance_flow.burst_zscore = Some(1.9);
        snapshot.binance_flow.whale_buys_5m = 2;
        snapshot.binance_flow.whale_sells_5m = 1;
        snapshot.binance_flow.whale_net_usd_5m = 270_000.0;

        snapshot.trend_updated_at_ms = now_ms - 1_000;
        snapshot.sr_levels_updated_at_ms = now_ms - 1_000;
        snapshot.volume_profile_updated_at_ms = now_ms - 1_000;
        snapshot.hl_flow_updated_at_ms = now_ms - 1_000;
        snapshot.binance_flow_updated_at_ms = now_ms - 1_000;
        snapshot.abnormal_updated_at_ms = now_ms - 1_000;
        snapshot.whale_updated_at_ms = now_ms - 1_000;
        snapshot.updated_at_ms = now_ms - 1_000;
        snapshot
    }

    fn core(pack: &FeaturePack) -> &FeaturePackCore {
        match pack {
            FeaturePack::M5(p) => &p.core,
            FeaturePack::M15(p) => &p.core,
            FeaturePack::H1(p) => &p.core,
            FeaturePack::H4(p) => &p.core,
        }
    }

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9
    }

    #[test]
    fn windows_match_timeframe_contract() {
        assert_eq!(
            windows_for_timeframe(Timeframe::M5),
            vec![15, 30, 60, 180, 300]
        );
        assert_eq!(
            windows_for_timeframe(Timeframe::M15),
            vec![60, 180, 300, 600, 900]
        );
        assert_eq!(
            windows_for_timeframe(Timeframe::H1),
            vec![300, 900, 1800, 3600]
        );
        assert_eq!(
            windows_for_timeframe(Timeframe::H4),
            vec![300, 900, 1800, 3600]
        );
    }

    #[test]
    fn builder_returns_timeframe_specific_pack() {
        let engine = FeatureEngineV2::new(FeatureEngineV2Config {
            enabled: true,
            version: "test-v2".to_string(),
            hl_l4_weight_5m: 0.65,
            hl_l4_weight_15m: 0.50,
            hl_l4_weight_1h: 0.35,
        });
        let snapshot = SignalState::default();
        let runtime = FeatureRuntimeContext::default();

        let pack = engine.build_feature_pack(Timeframe::M15, &snapshot, &runtime, 1000);
        match pack {
            FeaturePack::M15(p) => {
                assert_eq!(p.core.timeframe, Timeframe::M15);
                assert_eq!(p.core.feature_pack_version, "test-v2");
                assert_eq!(p.core.hl_l4.windows_seconds, vec![60, 180, 300, 600, 900]);
            }
            _ => panic!("unexpected pack variant"),
        }
    }

    #[test]
    fn feature_pack_contains_required_fields_and_freshness_flags() {
        let now_ms = 1_771_426_000_000_i64;
        let engine = FeatureEngineV2::new(FeatureEngineV2Config {
            enabled: true,
            version: "test-v2".to_string(),
            hl_l4_weight_5m: 0.65,
            hl_l4_weight_15m: 0.50,
            hl_l4_weight_1h: 0.35,
        });
        let snapshot = seeded_snapshot(now_ms);
        let runtime = FeatureRuntimeContext::default();

        for timeframe in Timeframe::all() {
            let pack = engine.build_feature_pack(timeframe, &snapshot, &runtime, now_ms);
            let core = core(&pack);
            assert_eq!(core.feature_pack_version, "test-v2");
            assert!(core.freshness.quality_score > 0.0);

            for required in ["adx", "support", "resistance", "poc", "cvd_5m"] {
                assert_eq!(core.freshness.validity_flags.get(required), Some(&true));
            }
            for source in [
                "trend",
                "sr_levels",
                "volume_profile",
                "hl_flow",
                "binance_flow",
            ] {
                assert_eq!(core.freshness.stale_flags.get(source), Some(&false));
            }

            assert_eq!(core.hl_l4.windows_seconds, windows_for_timeframe(timeframe));
            assert_eq!(
                core.hl_l4.signed_notional_sum_by_window.len(),
                core.hl_l4.windows_seconds.len()
            );
            assert_eq!(
                core.external.signed_notional_sum_by_window.len(),
                core.hl_l4.windows_seconds.len()
            );
        }
    }

    #[test]
    fn timeframe_context_separation_uses_distinct_windows_and_anchors() {
        let now_ms = 1_771_426_000_000_i64;
        let engine = FeatureEngineV2::new(FeatureEngineV2Config {
            enabled: true,
            version: "test-v2".to_string(),
            hl_l4_weight_5m: 0.65,
            hl_l4_weight_15m: 0.50,
            hl_l4_weight_1h: 0.35,
        });
        let snapshot = seeded_snapshot(now_ms);
        let runtime = FeatureRuntimeContext::default();

        let p5 = engine.build_feature_pack_5m(&snapshot, &runtime, now_ms);
        let p15 = engine.build_feature_pack_15m(&snapshot, &runtime, now_ms);
        let p1h = engine.build_feature_pack_1h(&snapshot, &runtime, now_ms);

        assert_eq!(p5.core.hl_l4.windows_seconds, vec![15, 30, 60, 180, 300]);
        assert_eq!(p15.core.hl_l4.windows_seconds, vec![60, 180, 300, 600, 900]);
        assert_eq!(p1h.core.hl_l4.windows_seconds, vec![300, 900, 1800, 3600]);
        assert_ne!(
            p5.core.hl_l4.signed_notional_sum_by_window,
            p15.core.hl_l4.signed_notional_sum_by_window
        );
        assert_ne!(
            p15.core.hl_l4.signed_notional_sum_by_window,
            p1h.core.hl_l4.signed_notional_sum_by_window
        );

        assert_eq!(
            p5.core.external.signed_notional_anchor,
            snapshot.binance_flow.signed_notional_300s
        );
        assert_eq!(
            p15.core.external.signed_notional_anchor,
            snapshot.binance_flow.signed_notional_900s
        );
        assert_eq!(
            p1h.core.external.signed_notional_anchor,
            snapshot.binance_flow.signed_notional_3600s
        );
        assert!(p1h
            .core
            .external
            .signed_notional_sum_by_window
            .get("15")
            .is_none());
    }

    #[test]
    fn component_scores_follow_weights_and_stay_bounded() {
        let now_ms = 1_771_426_000_000_i64;
        let cfg = FeatureEngineV2Config {
            enabled: true,
            version: "test-v2".to_string(),
            hl_l4_weight_5m: 0.65,
            hl_l4_weight_15m: 0.50,
            hl_l4_weight_1h: 0.35,
        };
        let engine = FeatureEngineV2::new(cfg.clone());
        let snapshot = seeded_snapshot(now_ms);
        let runtime = FeatureRuntimeContext::default();

        let expected_weights = [
            (Timeframe::M5, cfg.hl_l4_weight_5m, 0.20, 0.15),
            (Timeframe::M15, cfg.hl_l4_weight_15m, 0.30, 0.20),
            (Timeframe::H1, cfg.hl_l4_weight_1h, 0.40, 0.25),
        ];

        for (timeframe, hl_w, trend_w, ext_w) in expected_weights {
            let pack = engine.build_feature_pack(timeframe, &snapshot, &runtime, now_ms);
            let scores = &core(&pack).component_scores;

            assert!(approx_eq(scores.hl_l4_weight, hl_w));
            assert!(approx_eq(scores.trend_structure_weight, trend_w));
            assert!(approx_eq(scores.external_weight, ext_w));
            assert!((0.0..=1.0).contains(&scores.hl_l4_score));
            assert!((0.0..=1.0).contains(&scores.trend_structure_score));
            assert!((0.0..=1.0).contains(&scores.external_score));
            assert!((0.0..=1.0).contains(&scores.composite_score));

            let recomposed = (scores.hl_l4_score * scores.hl_l4_weight)
                + (scores.trend_structure_score * scores.trend_structure_weight)
                + (scores.external_score * scores.external_weight);
            assert!(approx_eq(scores.composite_score, recomposed));
        }
    }

    #[test]
    fn freshness_thresholds_are_timeframe_aware() {
        let now_ms = 1_771_426_000_000_i64;
        let mut snapshot = seeded_snapshot(now_ms);
        // Structural sources are 70s old: trend stale for 5m, SR/VP fresh for all TFs.
        snapshot.trend_updated_at_ms = now_ms - 70_000;
        snapshot.sr_levels_updated_at_ms = now_ms - 70_000;
        snapshot.volume_profile_updated_at_ms = now_ms - 70_000;
        // Flow sources are 80s old: stale for 5m, fresh for 15m/1h.
        snapshot.hl_flow_updated_at_ms = now_ms - 80_000;
        snapshot.binance_flow_updated_at_ms = now_ms - 80_000;
        snapshot.abnormal_updated_at_ms = now_ms - 80_000;
        snapshot.whale_updated_at_ms = now_ms - 80_000;
        snapshot.updated_at_ms = now_ms - 70_000;

        let runtime = FeatureRuntimeContext::default();
        let engine = FeatureEngineV2::new(FeatureEngineV2Config {
            enabled: true,
            version: "test-v2".to_string(),
            hl_l4_weight_5m: 0.65,
            hl_l4_weight_15m: 0.50,
            hl_l4_weight_1h: 0.35,
        });

        let p5 = engine.build_feature_pack(Timeframe::M5, &snapshot, &runtime, now_ms);
        let p15 = engine.build_feature_pack(Timeframe::M15, &snapshot, &runtime, now_ms);
        let p1h = engine.build_feature_pack(Timeframe::H1, &snapshot, &runtime, now_ms);

        let c5 = core(&p5);
        let c15 = core(&p15);
        let c1h = core(&p1h);

        assert_eq!(c5.freshness.stale_flags.get("trend"), Some(&true));
        assert_eq!(c5.freshness.stale_flags.get("sr_levels"), Some(&false));
        assert_eq!(c5.freshness.stale_flags.get("volume_profile"), Some(&false));
        assert_eq!(c5.freshness.stale_flags.get("hl_flow"), Some(&true));
        assert_eq!(c15.freshness.stale_flags.get("trend"), Some(&false));
        assert_eq!(c15.freshness.stale_flags.get("sr_levels"), Some(&false));
        assert_eq!(
            c15.freshness.stale_flags.get("volume_profile"),
            Some(&false)
        );
        assert_eq!(c15.freshness.stale_flags.get("hl_flow"), Some(&false));
        assert_eq!(c1h.freshness.stale_flags.get("trend"), Some(&false));
        assert_eq!(c1h.freshness.stale_flags.get("sr_levels"), Some(&false));
        assert_eq!(
            c1h.freshness.stale_flags.get("volume_profile"),
            Some(&false)
        );
        assert_eq!(c1h.freshness.stale_flags.get("hl_flow"), Some(&false));
    }
}
