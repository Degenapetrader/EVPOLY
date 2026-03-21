use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrendStateSnapshot {
    pub adx: Option<f64>,
    pub di_plus: Option<f64>,
    pub di_minus: Option<f64>,
    pub trend_score: Option<f64>,
    pub regime: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SrLevelsSnapshot {
    pub support: Option<f64>,
    pub resistance: Option<f64>,
    pub z_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VolumeProfileSnapshot {
    pub poc: Option<f64>,
    pub vah: Option<f64>,
    pub val: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HlFlowSnapshot {
    pub last_price: Option<f64>,
    pub cvd_5m: Option<f64>,
    pub liquidation_pressure_5m: Option<f64>,
    pub last_large_trade_usd: Option<f64>,
    pub last_fill_ts_ms: Option<i64>,
    // Timeframe-aware signed notional windows used by feature_engine_v2.
    pub signed_notional_15s: Option<f64>,
    pub signed_notional_30s: Option<f64>,
    pub signed_notional_60s: Option<f64>,
    pub signed_notional_180s: Option<f64>,
    pub signed_notional_300s: Option<f64>,
    pub signed_notional_600s: Option<f64>,
    pub signed_notional_900s: Option<f64>,
    pub signed_notional_1800s: Option<f64>,
    pub signed_notional_3600s: Option<f64>,
    // Derived primitives for HL-L4 prioritization.
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
pub struct BinanceFlowSnapshot {
    pub last_price: Option<f64>,
    pub last_trade_ts_ms: Option<i64>,
    pub volume_ratio_1m: Option<f64>,
    pub signed_notional_15s: Option<f64>,
    pub signed_notional_30s: Option<f64>,
    pub signed_notional_60s: Option<f64>,
    pub signed_notional_180s: Option<f64>,
    pub signed_notional_300s: Option<f64>,
    pub signed_notional_600s: Option<f64>,
    pub signed_notional_900s: Option<f64>,
    pub signed_notional_1800s: Option<f64>,
    pub signed_notional_3600s: Option<f64>,
    pub side_imbalance_5m: Option<f64>,
    pub flow_persistence: Option<f64>,
    pub burst_zscore: Option<f64>,
    pub whale_buys_5m: u32,
    pub whale_sells_5m: u32,
    pub whale_net_usd_5m: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowZScore {
    pub window_seconds: u32,
    pub z: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbnormalFlowEvent {
    pub source: String,
    pub side: String,
    pub timestamp_ms: i64,
    pub net_notional_1s: f64,
    pub max_abs_z: f64,
    pub windows: Vec<WindowZScore>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhaleTradeEvent {
    pub source: String,
    pub side: String,
    pub timestamp_ms: i64,
    pub notional_usd: f64,
    pub high_severity: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SignalState {
    pub trend: TrendStateSnapshot,
    pub sr_levels: SrLevelsSnapshot,
    pub volume_profile: VolumeProfileSnapshot,
    pub hl_flow: HlFlowSnapshot,
    pub binance_flow: BinanceFlowSnapshot,
    pub abnormal_events: Vec<AbnormalFlowEvent>,
    pub whale_events: Vec<WhaleTradeEvent>,
    pub trend_updated_at_ms: i64,
    pub sr_levels_updated_at_ms: i64,
    pub volume_profile_updated_at_ms: i64,
    pub hl_flow_updated_at_ms: i64,
    pub binance_flow_updated_at_ms: i64,
    pub abnormal_updated_at_ms: i64,
    pub whale_updated_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SignalSourceAgesMs {
    pub trend_ms: Option<i64>,
    pub sr_levels_ms: Option<i64>,
    pub volume_profile_ms: Option<i64>,
    pub hl_flow_ms: Option<i64>,
    pub binance_flow_ms: Option<i64>,
    pub abnormal_ms: Option<i64>,
    pub whale_ms: Option<i64>,
}

pub type SharedSignalState = Arc<RwLock<SignalState>>;

pub fn new_shared_signal_state() -> SharedSignalState {
    Arc::new(RwLock::new(SignalState::default()))
}

impl SignalState {
    fn age_from_ts(now_ms: i64, ts_ms: i64) -> Option<i64> {
        if ts_ms <= 0 {
            None
        } else {
            Some(now_ms.saturating_sub(ts_ms))
        }
    }

    fn is_fresh(ts_ms: i64, now_ms: i64, max_age_ms: i64) -> bool {
        ts_ms > 0 && now_ms.saturating_sub(ts_ms) <= max_age_ms
    }

    pub fn source_ages_ms(&self, now_ms: i64) -> SignalSourceAgesMs {
        SignalSourceAgesMs {
            trend_ms: Self::age_from_ts(now_ms, self.trend_updated_at_ms),
            sr_levels_ms: Self::age_from_ts(now_ms, self.sr_levels_updated_at_ms),
            volume_profile_ms: Self::age_from_ts(now_ms, self.volume_profile_updated_at_ms),
            hl_flow_ms: Self::age_from_ts(now_ms, self.hl_flow_updated_at_ms),
            binance_flow_ms: Self::age_from_ts(now_ms, self.binance_flow_updated_at_ms),
            abnormal_ms: Self::age_from_ts(now_ms, self.abnormal_updated_at_ms),
            whale_ms: Self::age_from_ts(now_ms, self.whale_updated_at_ms),
        }
    }

    pub fn newest_source_update_ms(&self) -> i64 {
        [
            self.updated_at_ms,
            self.trend_updated_at_ms,
            self.sr_levels_updated_at_ms,
            self.volume_profile_updated_at_ms,
            self.hl_flow_updated_at_ms,
            self.binance_flow_updated_at_ms,
            self.abnormal_updated_at_ms,
            self.whale_updated_at_ms,
        ]
        .into_iter()
        .max()
        .unwrap_or(0)
    }

    pub fn has_any_fresh_source(&self, now_ms: i64, max_age_ms: i64) -> bool {
        Self::is_fresh(self.trend_updated_at_ms, now_ms, max_age_ms)
            || Self::is_fresh(self.sr_levels_updated_at_ms, now_ms, max_age_ms)
            || Self::is_fresh(self.volume_profile_updated_at_ms, now_ms, max_age_ms)
            || Self::is_fresh(self.hl_flow_updated_at_ms, now_ms, max_age_ms)
            || Self::is_fresh(self.binance_flow_updated_at_ms, now_ms, max_age_ms)
            || Self::is_fresh(self.abnormal_updated_at_ms, now_ms, max_age_ms)
            || Self::is_fresh(self.whale_updated_at_ms, now_ms, max_age_ms)
    }

    pub fn sanitize_for_max_age(&self, now_ms: i64, max_age_ms: i64) -> Self {
        let mut out = self.clone();

        if !Self::is_fresh(out.trend_updated_at_ms, now_ms, max_age_ms) {
            out.trend = TrendStateSnapshot::default();
        }
        if !Self::is_fresh(out.sr_levels_updated_at_ms, now_ms, max_age_ms) {
            out.sr_levels = SrLevelsSnapshot::default();
        }
        if !Self::is_fresh(out.volume_profile_updated_at_ms, now_ms, max_age_ms) {
            out.volume_profile = VolumeProfileSnapshot::default();
        }
        if !Self::is_fresh(out.hl_flow_updated_at_ms, now_ms, max_age_ms) {
            out.hl_flow = HlFlowSnapshot::default();
        }
        if !Self::is_fresh(out.binance_flow_updated_at_ms, now_ms, max_age_ms) {
            out.binance_flow = BinanceFlowSnapshot::default();
        }
        if !Self::is_fresh(out.abnormal_updated_at_ms, now_ms, max_age_ms) {
            out.abnormal_events.clear();
        }
        if !Self::is_fresh(out.whale_updated_at_ms, now_ms, max_age_ms) {
            out.whale_events.clear();
        }

        out.updated_at_ms = out.newest_source_update_ms();
        out
    }
}

pub fn trim_vec<T>(items: &mut Vec<T>, max_len: usize) {
    if items.len() > max_len {
        let drop_n = items.len() - max_len;
        items.drain(0..drop_n);
    }
}
