#[derive(Debug, Clone, Copy)]
pub struct RiskCaps {
    pub per_order_usd: f64,
    pub per_market_usd: f64,
    pub per_mode_usd: f64,
    pub global_usd: f64,
}

impl Default for RiskCaps {
    fn default() -> Self {
        Self {
            per_order_usd: 5.0,
            per_market_usd: 25.0,
            per_mode_usd: 50.0,
            global_usd: 100.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskDecision {
    Allow,
    RejectOrderCap,
    RejectMarketCap,
    RejectModeCap,
    RejectGlobalCap,
}

pub fn check_caps(
    caps: RiskCaps,
    target_order_usd: f64,
    used_market_usd: f64,
    used_mode_usd: f64,
    used_global_usd: f64,
) -> RiskDecision {
    if target_order_usd > caps.per_order_usd {
        return RiskDecision::RejectOrderCap;
    }
    if used_market_usd + target_order_usd > caps.per_market_usd {
        return RiskDecision::RejectMarketCap;
    }
    if used_mode_usd + target_order_usd > caps.per_mode_usd {
        return RiskDecision::RejectModeCap;
    }
    if used_global_usd + target_order_usd > caps.global_usd {
        return RiskDecision::RejectGlobalCap;
    }
    RiskDecision::Allow
}
