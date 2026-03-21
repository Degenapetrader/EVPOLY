use crate::monitor::MarketSnapshot;
use crate::signal_state::SignalState;
use crate::strategy::{Direction, StrategyAction, StrategyDecision};
use log::debug;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Reset state for a token type after a buy-sell cycle
#[derive(Debug, Clone, PartialEq)]
enum ResetState {
    /// Ready to buy - no reset needed or reset completed
    Ready,
    /// Needs reset - price must drop below trigger_price before allowing another buy
    NeedsReset,
}

#[cfg(test)]
mod tests {
    use super::PriceDetector;

    #[test]
    fn entry_cross_spread_is_enabled_by_default() {
        assert!(PriceDetector::entry_allow_cross_spread());
    }

    #[test]
    fn entry_confidence_hard_floor_is_bounded() {
        let floor = PriceDetector::entry_confidence_hard_floor();
        assert!((0.0..=0.99).contains(&floor));
    }
}

/// Detector for momentum-based trading strategy
/// Strategy: Buy BTC/ETH tokens (BTC/ETH Up/Down) when price reaches trigger_price (0.9) after min_elapsed_minutes (10 minutes)
/// Assumption: If price reaches 0.9 after 10 minutes, it's very likely to reach 1.0 by market close
///
/// Reset mechanism: After a successful buy-sell cycle, require price to drop below trigger_price
/// before allowing another buy. This prevents buying immediately after selling when price only dips slightly.
pub struct PriceDetector {
    trigger_price: f64,       // Minimum price threshold to trigger buy (e.g., 0.9)
    max_buy_price: f64,       // Maximum price to buy at (e.g., 0.95) - don't buy if price > this
    min_elapsed_minutes: u64, // Minimum minutes that must have elapsed (e.g., 10 minutes)
    min_time_remaining_seconds: u64, // Minimum seconds that must remain (e.g., 30 seconds) - don't buy if less time remains
    enable_eth_trading: bool,        // Whether ETH trading is enabled
    enable_solana_trading: bool,     // Whether Solana trading is enabled
    enable_xrp_trading: bool,        // Whether XRP trading is enabled
    // Track which tokens we've bought in this period (key: token_id)
    current_period_bought: Arc<Mutex<std::collections::HashSet<String>>>,
    // Track last logged period to detect new markets
    last_logged_period: Arc<tokio::sync::Mutex<Option<u64>>>,
    // Track reset state per token type after successful buy-sell cycles
    // Key: TokenType, Value: ResetState
    reset_states: Arc<Mutex<HashMap<TokenType, ResetState>>>,
}

#[derive(Debug, Clone)]
pub struct BuyOpportunity {
    pub condition_id: String,    // Market condition ID (BTC or ETH)
    pub token_id: String,        // Token ID for the token we're buying
    pub token_type: TokenType,   // Type of token (BTC Up, BTC Down, ETH Up, ETH Down)
    pub bid_price: f64,          // BID price for the token we're buying
    pub expected_edge_bps: f64,  // Expected edge after fees/buffers
    pub expected_fill_prob: f64, // Estimated fill probability at quoted limit
    pub period_timestamp: u64,
    pub time_remaining_seconds: u64,
    pub time_elapsed_seconds: u64, // How many seconds have elapsed in this period
    pub use_market_order: bool,    // If true, use market order; if false, use limit order
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TokenType {
    BtcUp,
    BtcDown,
    EthUp,
    EthDown,
    SolanaUp,
    SolanaDown,
    XrpUp,
    XrpDown,
}

impl TokenType {
    pub fn display_name(&self) -> &str {
        match self {
            TokenType::BtcUp => "BTC Up",
            TokenType::BtcDown => "BTC Down",
            TokenType::EthUp => "ETH Up",
            TokenType::EthDown => "ETH Down",
            TokenType::SolanaUp => "SOL Up",
            TokenType::SolanaDown => "SOL Down",
            TokenType::XrpUp => "XRP Up",
            TokenType::XrpDown => "XRP Down",
        }
    }

    /// Get the opposite token type (Up <-> Down)
    pub fn opposite(&self) -> TokenType {
        match self {
            TokenType::BtcUp => TokenType::BtcDown,
            TokenType::BtcDown => TokenType::BtcUp,
            TokenType::EthUp => TokenType::EthDown,
            TokenType::EthDown => TokenType::EthUp,
            TokenType::SolanaUp => TokenType::SolanaDown,
            TokenType::SolanaDown => TokenType::SolanaUp,
            TokenType::XrpUp => TokenType::XrpDown,
            TokenType::XrpDown => TokenType::XrpUp,
        }
    }
}

impl PriceDetector {
    pub fn new(
        trigger_price: f64,
        max_buy_price: f64,
        min_elapsed_minutes: u64,
        min_time_remaining_seconds: u64,
        enable_eth_trading: bool,
        enable_solana_trading: bool,
        enable_xrp_trading: bool,
    ) -> Self {
        Self {
            trigger_price,
            max_buy_price,
            min_elapsed_minutes,
            min_time_remaining_seconds,
            enable_eth_trading,
            enable_solana_trading,
            enable_xrp_trading,
            current_period_bought: Arc::new(Mutex::new(std::collections::HashSet::new())),
            last_logged_period: Arc::new(tokio::sync::Mutex::new(None)),
            reset_states: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    fn urgency_spread(urgency: &str) -> f64 {
        match urgency.to_ascii_uppercase().as_str() {
            "HIGH" | "AGGRESSIVE" | "FAST" => 0.015,
            "MEDIUM" | "NORMAL" => 0.008,
            _ => 0.005,
        }
    }

    fn entry_window_pct(_strategy_id: &str) -> f64 {
        std::env::var("EVPOLY_ENTRY_WINDOW_PCT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.30)
            .clamp(0.05, 0.95)
    }

    fn entry_ev_gate_enabled() -> bool {
        std::env::var("EVPOLY_ENTRY_ENABLE_EV_GATE")
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(true)
    }

    fn entry_allow_cross_spread() -> bool {
        std::env::var("EVPOLY_ENTRY_ALLOW_CROSS_SPREAD")
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(true)
    }

    fn entry_aggressive_fill_target() -> f64 {
        std::env::var("EVPOLY_ENTRY_AGGRESSIVE_FILL_TARGET")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(0.85)
            .clamp(0.30, 1.0)
    }

    fn entry_confidence_hard_floor() -> f64 {
        std::env::var("EVPOLY_ENTRY_CONFIDENCE_HARD_FLOOR")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(0.20)
            .clamp(0.0, 0.99)
    }

    fn entry_ev_floor_bps_default(_strategy_id: &str) -> f64 {
        std::env::var("EVPOLY_ENTRY_EV_FLOOR_BPS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(10.0)
            .max(0.0)
    }

    fn entry_fee_buffer_bps(_strategy_id: &str) -> f64 {
        std::env::var("EVPOLY_ENTRY_FEE_BUFFER_BPS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(8.0)
            .max(0.0)
    }

    fn entry_model_buffer_bps(_strategy_id: &str) -> f64 {
        std::env::var("EVPOLY_ENTRY_MODEL_BUFFER_BPS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(6.0)
            .max(0.0)
    }

    fn entry_default_uncertainty(_strategy_id: &str) -> f64 {
        std::env::var("EVPOLY_ENTRY_DEFAULT_UNCERTAINTY_BPS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(12.0)
            .max(0.0)
            / 10_000.0
    }

    fn entry_high_edge_bps(_strategy_id: &str) -> f64 {
        std::env::var("EVPOLY_ENTRY_HIGH_EDGE_BPS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(25.0)
            .max(0.0)
    }

    fn estimate_fill_probability(limit_price: f64, bid: f64, ask: f64) -> f64 {
        if ask <= bid {
            return 0.95;
        }
        let normalized = ((limit_price - bid) / (ask - bid)).clamp(0.0, 1.0);
        (0.30 + normalized * 0.70).clamp(0.05, 1.0)
    }

    fn relaxed_entry_filters_enabled() -> bool {
        std::env::var("EVPOLY_RELAXED_ENTRY_FILTERS")
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }

    fn direction_to_side(direction: &Direction) -> &'static str {
        match direction {
            Direction::Up => "BUY",
            Direction::Down => "SELL",
        }
    }

    fn signal_boost(signals: Option<&SignalState>, desired_side: &str) -> f64 {
        let Some(signals) = signals else {
            return 0.0;
        };

        let mut score = 0.0_f64;
        if let Some(trend) = signals.trend.trend_score {
            let trend_mag = (trend.abs() / 100.0).clamp(0.0, 1.0);
            let trend_side = if trend >= 0.0 { "BUY" } else { "SELL" };
            if trend_side == desired_side {
                score += trend_mag * 0.10;
            } else {
                score -= trend_mag * 0.10;
            }
        }
        if let Some(vr) = signals.binance_flow.volume_ratio_1m {
            if vr >= 1.5 {
                score += 0.08;
            } else if vr < 0.8 {
                score -= 0.08;
            }
        }
        if let Some(last_abnormal) = signals.abnormal_events.last() {
            if last_abnormal.side == desired_side {
                score += 0.12;
            } else {
                score -= 0.12;
            }
        }
        if let Some(last_whale) = signals.whale_events.last() {
            if last_whale.side == desired_side {
                score += 0.08;
            } else {
                score -= 0.10;
            }
        }
        score
    }

    fn build_strategy_opportunity(
        &self,
        strategy_id: &str,
        token: &crate::models::TokenPrice,
        token_type: TokenType,
        condition_id: &str,
        snapshot: &MarketSnapshot,
        time_elapsed_seconds: u64,
        max_price: f64,
        urgency: &str,
        fair_probability: f64,
        uncertainty: f64,
        ev_floor_bps: f64,
    ) -> Option<BuyOpportunity> {
        let bid = token
            .bid
            .map(decimal_to_f64)
            .or_else(|| token.ask.map(decimal_to_f64))?;
        let ask = token
            .ask
            .map(decimal_to_f64)
            .or_else(|| token.bid.map(decimal_to_f64))?;
        if ask <= 0.0 || bid <= 0.0 {
            return None;
        }
        let (best_bid, best_ask) = if ask >= bid { (bid, ask) } else { (ask, bid) };
        if best_bid <= 0.0 || best_ask <= 0.0 {
            return None;
        }

        let total_buffer_bps = Self::entry_fee_buffer_bps(strategy_id)
            + Self::entry_model_buffer_bps(strategy_id)
            + (uncertainty * 10_000.0);
        let urgency_boost = Self::urgency_spread(urgency).clamp(0.0, 0.03) * 0.15;
        let base_target = (Self::entry_aggressive_fill_target() + urgency_boost).clamp(0.0, 1.0);
        let spread = (best_ask - best_bid).max(0.0);
        let mut limit_price = (best_bid + spread * base_target)
            .clamp(0.01, best_ask)
            .min(max_price);
        let mut cross_spread_used = false;
        let crossing_edge_bps =
            ((fair_probability - best_ask.min(max_price)) * 10_000.0) - total_buffer_bps;
        if Self::entry_allow_cross_spread()
            && best_ask <= max_price
            && crossing_edge_bps >= ev_floor_bps.max(Self::entry_high_edge_bps(strategy_id))
        {
            limit_price = best_ask.min(max_price);
            cross_spread_used = true;
        }
        if limit_price <= 0.0 {
            return None;
        }

        let expected_edge_bps = ((fair_probability - limit_price) * 10_000.0) - total_buffer_bps;
        if Self::entry_ev_gate_enabled() && expected_edge_bps < ev_floor_bps {
            return None;
        }
        let expected_fill_prob = Self::estimate_fill_probability(limit_price, best_bid, best_ask);

        Some(BuyOpportunity {
            condition_id: condition_id.to_string(),
            token_id: token.token_id.clone(),
            token_type,
            bid_price: limit_price,
            expected_edge_bps,
            expected_fill_prob,
            period_timestamp: snapshot.period_timestamp,
            time_remaining_seconds: snapshot.time_remaining_seconds,
            time_elapsed_seconds,
            // "true" means marketable-limit (cross-spread) mode; still LIMIT order type.
            use_market_order: cross_spread_used,
        })
    }

    pub async fn detect_strategy_opportunities(
        &self,
        snapshot: &MarketSnapshot,
        strategy: Option<StrategyDecision>,
        signals: Option<&SignalState>,
    ) -> Vec<BuyOpportunity> {
        if snapshot.time_remaining_seconds == 0 {
            return Vec::new();
        }

        let Some(strategy) = strategy else {
            debug!(
                "No strategy decision available for period {}, skipping entries",
                snapshot.period_timestamp
            );
            return Vec::new();
        };

        match strategy.action {
            StrategyAction::Skip => {
                debug!(
                    "Strategy action=SKIP for timeframe {:?}, period {}",
                    strategy.timeframe, snapshot.period_timestamp
                );
                return Vec::new();
            }
            StrategyAction::Trade => {}
        }

        let Some(direction) = strategy.direction.clone() else {
            return Vec::new();
        };
        let Some(accumulation) = strategy.accumulation.as_ref() else {
            return Vec::new();
        };

        // Use timeframe-specific period duration for phase calculations.
        let period_duration = strategy.timeframe.duration_seconds() as u64;
        let bounded_remaining = snapshot.time_remaining_seconds.min(period_duration);
        let time_elapsed_seconds = period_duration.saturating_sub(bounded_remaining);
        if period_duration == 0 {
            return Vec::new();
        }
        let elapsed_pct = time_elapsed_seconds as f64 / period_duration as f64;
        let entry_window_pct = Self::entry_window_pct(strategy.strategy_id.as_str());
        if elapsed_pct > entry_window_pct {
            return Vec::new();
        }
        if snapshot.time_remaining_seconds < self.min_time_remaining_seconds {
            return Vec::new();
        }

        let desired_side = Self::direction_to_side(&direction);
        let confidence =
            (strategy.confidence + Self::signal_boost(signals, desired_side)).clamp(0.0, 1.0);
        let relaxed_entry_filters = Self::relaxed_entry_filters_enabled();
        let base_confidence_floor = if relaxed_entry_filters {
            0.20_f64
        } else {
            0.30_f64
        };
        let confidence_floor = base_confidence_floor.max(Self::entry_confidence_hard_floor());
        if confidence < confidence_floor {
            debug!(
                "Strategy confidence too low after signal-weighting: {:.3} < {:.3}",
                confidence, confidence_floor
            );
            return Vec::new();
        }

        let fair_probability = accumulation
            .fair_probability
            .unwrap_or(accumulation.max_price)
            .clamp(0.01, 0.99);
        let uncertainty = accumulation
            .uncertainty
            .unwrap_or_else(|| Self::entry_default_uncertainty(strategy.strategy_id.as_str()))
            .clamp(0.0, 0.25);
        let ev_floor_bps = accumulation
            .ev_floor_bps
            .unwrap_or_else(|| Self::entry_ev_floor_bps_default(strategy.strategy_id.as_str()))
            .max(0.0);
        let max_price = if relaxed_entry_filters {
            // Stress mode: loosen max-price cap slightly to increase throughput.
            let relaxed_accumulation_max = (accumulation.max_price + 0.20).clamp(0.01, 0.99);
            let relaxed_config_max = (self.max_buy_price + 0.10).clamp(0.01, 0.99);
            let stress_floor = (self.trigger_price + 0.05).min(relaxed_config_max);
            relaxed_accumulation_max
                .max(stress_floor)
                .min(relaxed_config_max)
                .max(0.01)
        } else {
            // Realistic mode: strictly respect configured max-price constraints.
            accumulation
                .max_price
                .clamp(0.01, 0.99)
                .min(self.max_buy_price.clamp(0.01, 0.99))
                .max(0.01)
        };
        let max_price = max_price.min(fair_probability).clamp(0.01, 0.99);
        let mut opportunities = Vec::new();

        match direction {
            Direction::Up => {
                if let Some(btc_up) = snapshot.btc_market.up_token.as_ref() {
                    if let Some(opp) = self.build_strategy_opportunity(
                        strategy.strategy_id.as_str(),
                        btc_up,
                        TokenType::BtcUp,
                        &snapshot.btc_market.condition_id,
                        snapshot,
                        time_elapsed_seconds,
                        max_price,
                        accumulation.urgency.as_str(),
                        fair_probability,
                        uncertainty,
                        ev_floor_bps,
                    ) {
                        opportunities.push(opp);
                    }
                }
                if self.enable_eth_trading {
                    if let Some(eth_up) = snapshot.eth_market.up_token.as_ref() {
                        if let Some(opp) = self.build_strategy_opportunity(
                            strategy.strategy_id.as_str(),
                            eth_up,
                            TokenType::EthUp,
                            &snapshot.eth_market.condition_id,
                            snapshot,
                            time_elapsed_seconds,
                            max_price,
                            accumulation.urgency.as_str(),
                            fair_probability,
                            uncertainty,
                            ev_floor_bps,
                        ) {
                            opportunities.push(opp);
                        }
                    }
                }
                if self.enable_solana_trading {
                    if let Some(sol_up) = snapshot.solana_market.up_token.as_ref() {
                        if let Some(opp) = self.build_strategy_opportunity(
                            strategy.strategy_id.as_str(),
                            sol_up,
                            TokenType::SolanaUp,
                            &snapshot.solana_market.condition_id,
                            snapshot,
                            time_elapsed_seconds,
                            max_price,
                            accumulation.urgency.as_str(),
                            fair_probability,
                            uncertainty,
                            ev_floor_bps,
                        ) {
                            opportunities.push(opp);
                        }
                    }
                }
                if self.enable_xrp_trading {
                    if let Some(xrp_up) = snapshot.xrp_market.up_token.as_ref() {
                        if let Some(opp) = self.build_strategy_opportunity(
                            strategy.strategy_id.as_str(),
                            xrp_up,
                            TokenType::XrpUp,
                            &snapshot.xrp_market.condition_id,
                            snapshot,
                            time_elapsed_seconds,
                            max_price,
                            accumulation.urgency.as_str(),
                            fair_probability,
                            uncertainty,
                            ev_floor_bps,
                        ) {
                            opportunities.push(opp);
                        }
                    }
                }
            }
            Direction::Down => {
                if let Some(btc_down) = snapshot.btc_market.down_token.as_ref() {
                    if let Some(opp) = self.build_strategy_opportunity(
                        strategy.strategy_id.as_str(),
                        btc_down,
                        TokenType::BtcDown,
                        &snapshot.btc_market.condition_id,
                        snapshot,
                        time_elapsed_seconds,
                        max_price,
                        accumulation.urgency.as_str(),
                        fair_probability,
                        uncertainty,
                        ev_floor_bps,
                    ) {
                        opportunities.push(opp);
                    }
                }
                if self.enable_eth_trading {
                    if let Some(eth_down) = snapshot.eth_market.down_token.as_ref() {
                        if let Some(opp) = self.build_strategy_opportunity(
                            strategy.strategy_id.as_str(),
                            eth_down,
                            TokenType::EthDown,
                            &snapshot.eth_market.condition_id,
                            snapshot,
                            time_elapsed_seconds,
                            max_price,
                            accumulation.urgency.as_str(),
                            fair_probability,
                            uncertainty,
                            ev_floor_bps,
                        ) {
                            opportunities.push(opp);
                        }
                    }
                }
                if self.enable_solana_trading {
                    if let Some(sol_down) = snapshot.solana_market.down_token.as_ref() {
                        if let Some(opp) = self.build_strategy_opportunity(
                            strategy.strategy_id.as_str(),
                            sol_down,
                            TokenType::SolanaDown,
                            &snapshot.solana_market.condition_id,
                            snapshot,
                            time_elapsed_seconds,
                            max_price,
                            accumulation.urgency.as_str(),
                            fair_probability,
                            uncertainty,
                            ev_floor_bps,
                        ) {
                            opportunities.push(opp);
                        }
                    }
                }
                if self.enable_xrp_trading {
                    if let Some(xrp_down) = snapshot.xrp_market.down_token.as_ref() {
                        if let Some(opp) = self.build_strategy_opportunity(
                            strategy.strategy_id.as_str(),
                            xrp_down,
                            TokenType::XrpDown,
                            &snapshot.xrp_market.condition_id,
                            snapshot,
                            time_elapsed_seconds,
                            max_price,
                            accumulation.urgency.as_str(),
                            fair_probability,
                            uncertainty,
                            ev_floor_bps,
                        ) {
                            opportunities.push(opp);
                        }
                    }
                }
            }
        }

        if !opportunities.is_empty() {
            crate::log_println!(
                "🧠 Strategy accumulation opportunities: tf={:?} conf={:.2} max_price=${:.4} fair_prob=${:.4} window={:.2}% count={}",
                strategy.timeframe,
                confidence,
                max_price,
                fair_probability,
                entry_window_pct * 100.0,
                opportunities.len()
            );
        }

        opportunities
    }

    /// Check a single token for opportunity
    async fn check_token(
        &self,
        token: &crate::models::TokenPrice,
        token_type: TokenType,
        condition_id: &str,
        snapshot: &MarketSnapshot,
        time_elapsed_seconds: u64,
        min_elapsed_seconds: u64,
    ) -> Option<BuyOpportunity> {
        // Use BID price (what we pay to buy) - return None if bid price is missing
        let bid_price = match token.bid {
            Some(bid) => decimal_to_f64(bid),
            None => {
                if time_elapsed_seconds >= min_elapsed_seconds - 60 {
                    crate::log_println!(
                        "⚠️  {}: No BID price available, skipping",
                        token_type.display_name()
                    );
                }
                return None;
            }
        };

        let time_elapsed_minutes = time_elapsed_seconds / 60;
        let time_remaining_minutes = snapshot.time_remaining_seconds / 60;

        // Check reset state: after a successful buy-sell cycle, require price to drop below trigger_price
        // before allowing another buy. This prevents buying immediately after selling when price only dips slightly.
        let mut reset_states = self.reset_states.lock().await;
        let reset_state = reset_states
            .get(&token_type)
            .cloned()
            .unwrap_or(ResetState::Ready);

        match reset_state {
            ResetState::NeedsReset => {
                // After a successful sell, we need price to drop below trigger_price to reset
                if bid_price < self.trigger_price {
                    // Price dropped below trigger - reset completed, allow buying again
                    reset_states.insert(token_type.clone(), ResetState::Ready);
                    drop(reset_states);
                    if time_elapsed_seconds >= min_elapsed_seconds {
                        eprintln!("✅ {}: Reset completed - BID=${:.6} < trigger=${:.6}, ready for next buy",
                            token_type.display_name(), bid_price, self.trigger_price);
                    }
                    // Still return None here - we need price to go back up >= trigger_price to buy
                    return None;
                } else {
                    // Price hasn't dropped below trigger yet - still needs reset
                    drop(reset_states);
                    if time_elapsed_seconds >= min_elapsed_seconds {
                        eprintln!("⏸️  {}: Needs reset - BID=${:.6} >= trigger=${:.6}, waiting for price to drop below trigger first",
                            token_type.display_name(), bid_price, self.trigger_price);
                    }
                    return None;
                }
            }
            ResetState::Ready => {
                // Ready to buy - no reset needed
                drop(reset_states);
            }
        }

        // Log when price is close to trigger (within 0.05) or past buy window - helps debug why buys aren't triggering
        let price_diff = bid_price - self.trigger_price;
        if time_elapsed_seconds >= min_elapsed_seconds.saturating_sub(60) || price_diff.abs() < 0.05
        {
            eprintln!("🔍 {}: BID=${:.6} (trigger=${:.2}, diff=${:.3}), range: ${:.2}-${:.2}, elapsed={}m{}s (need {}m), remaining={}m{}s",
                token_type.display_name(), bid_price, self.trigger_price, price_diff,
                self.trigger_price, self.max_buy_price,
                time_elapsed_minutes, time_elapsed_seconds % 60, self.min_elapsed_minutes,
                time_remaining_minutes, snapshot.time_remaining_seconds % 60);
        }

        // Check if enough time has elapsed (at least min_elapsed_minutes)
        if time_elapsed_seconds < min_elapsed_seconds {
            // Log when close to buy window or when price is near trigger - helps debug why buys aren't triggering
            let time_remaining_until_window = min_elapsed_seconds - time_elapsed_seconds;
            let price_diff = bid_price - self.trigger_price;
            if time_elapsed_seconds >= min_elapsed_seconds - 60 || price_diff.abs() < 0.05 {
                eprintln!("⏸️  {}: Time not elapsed yet: {}m{}s elapsed < {}m required (need {}s more) | BID=${:.6}",
                    token_type.display_name(), time_elapsed_minutes, time_elapsed_seconds % 60,
                    self.min_elapsed_minutes, time_remaining_until_window, bid_price);
            }
            return None;
        }

        // Buy when price is between trigger_price (min) and max_buy_price (max)
        // Example: buy when 0.87 <= bid_price <= 0.95
        if bid_price < self.trigger_price {
            // Log when close to trigger or past buy window - helps debug why buys aren't triggering
            let price_diff = self.trigger_price - bid_price;
            if time_elapsed_seconds >= min_elapsed_seconds || price_diff < 0.05 {
                eprintln!(
                    "⏸️  {}: Price too low: BID=${:.6} < ${:.6} (trigger) - need ${:.3} more",
                    token_type.display_name(),
                    bid_price,
                    self.trigger_price,
                    price_diff
                );
            }
            return None; // Price too low, wait for it to reach trigger_price (0.87)
        }

        if bid_price > self.max_buy_price {
            // Only log when close to buy window (reduce noise)
            if time_elapsed_seconds >= min_elapsed_seconds {
                debug!(
                    "{}: Price too high: ${:.6} > ${:.6} (max)",
                    token_type.display_name(),
                    bid_price,
                    self.max_buy_price
                );
            }
            return None; // Price too high (> 0.95), skip buying and wait for price to drop
        }

        // Check if there's enough time remaining (at least min_time_remaining_seconds)
        // Don't buy if market is closing soon - too risky
        if snapshot.time_remaining_seconds < self.min_time_remaining_seconds {
            // Log prominently when skipping buy due to insufficient time remaining
            if time_elapsed_seconds >= min_elapsed_seconds {
                eprintln!("⏸️  {}: SKIPPING BUY - insufficient time remaining: {}s < {}s (minimum required)",
                    token_type.display_name(), snapshot.time_remaining_seconds, self.min_time_remaining_seconds);
                eprintln!(
                    "   💡 Price conditions met, but market closing too soon - too risky to buy"
                );
            }
            return None; // Too little time remaining, skip buying
        }

        // Price is in valid range! (trigger_price <= bid_price <= max_buy_price)
        // And there's enough time remaining (>= 30 seconds)
        // This should trigger a buy
        let expected_profit_at_1_0 = 1.0 - bid_price; // Profit if token reaches $1.0

        // Log momentum opportunity in compact one-line format
        eprintln!(
            "🎯 {} BUY: BID=${:.3} | Elapsed: {}m | Remaining: {}s | Profit@$1.0: ${:.3}",
            token_type.display_name(),
            bid_price,
            time_elapsed_minutes,
            snapshot.time_remaining_seconds,
            expected_profit_at_1_0
        );

        Some(BuyOpportunity {
            condition_id: condition_id.to_string(),
            token_id: token.token_id.clone(),
            token_type,
            bid_price,
            expected_edge_bps: 0.0,
            expected_fill_prob: 1.0,
            period_timestamp: snapshot.period_timestamp,
            time_remaining_seconds: snapshot.time_remaining_seconds,
            time_elapsed_seconds,
            use_market_order: false, // Regular detect_opportunities always uses market orders
        })
    }

    /// Detect momentum opportunities: Check BTC/ETH/SOL/XRP Up/Down tokens for price >= trigger_price after min_elapsed_minutes.
    /// This path is still used by the legacy `polymarket-arbitrage-bot-limit` binary.
    /// Strategy: Buy BTC/ETH token when price reaches 0.9 after 10 minutes have elapsed
    /// Returns all matching opportunities so we can buy both ETH Down and BTC Down (and Up) when multiple qualify
    pub async fn detect_opportunities(&self, snapshot: &MarketSnapshot) -> Vec<BuyOpportunity> {
        let mut opportunities = Vec::new();

        // Reject expired markets (time_remaining_seconds <= 0)
        if snapshot.time_remaining_seconds == 0 {
            debug!("Market expired (time_remaining=0), skipping opportunity detection");
            return opportunities;
        }

        // Calculate time elapsed (15 minutes = 900 seconds total)
        const PERIOD_DURATION: u64 = 900; // 15 minutes in seconds
        let time_elapsed_seconds = PERIOD_DURATION - snapshot.time_remaining_seconds;
        let min_elapsed_seconds = self.min_elapsed_minutes * 60;

        // Log when we detect a new market period (to show we're monitoring each market)
        let time_elapsed_minutes = time_elapsed_seconds / 60;
        let time_remaining_minutes = snapshot.time_remaining_seconds / 60;

        {
            let mut last_period = self.last_logged_period.lock().await;
            if last_period.is_none() || last_period.unwrap() != snapshot.period_timestamp {
                eprintln!("🔄 New market (period: {}): {}m elapsed, {}m remaining (buy window: after {}m)",
                    snapshot.period_timestamp, time_elapsed_minutes, time_remaining_minutes, self.min_elapsed_minutes);
                *last_period = Some(snapshot.period_timestamp);
            }
        }

        if time_elapsed_seconds < min_elapsed_seconds
            && time_elapsed_minutes > 0
            && time_elapsed_minutes % 2 == 0
            && time_elapsed_seconds % 60 < 2
        {
            eprintln!(
                "⏱️  Monitoring (period: {}): {}m elapsed, {}m remaining (buy window: after {}m)",
                snapshot.period_timestamp,
                time_elapsed_minutes,
                time_remaining_minutes,
                self.min_elapsed_minutes
            );
        }

        debug!(
            "detect_opportunity: time_elapsed={}s ({}m), remaining={}s, min_required={}s ({}m)",
            time_elapsed_seconds,
            time_elapsed_seconds / 60,
            snapshot.time_remaining_seconds,
            min_elapsed_seconds,
            self.min_elapsed_minutes
        );

        // Check BTC Up
        if let Some(btc_up) = snapshot.btc_market.up_token.as_ref() {
            if let Some(opp) = self
                .check_token(
                    btc_up,
                    TokenType::BtcUp,
                    &snapshot.btc_market.condition_id,
                    snapshot,
                    time_elapsed_seconds,
                    min_elapsed_seconds,
                )
                .await
            {
                opportunities.push(opp);
            }
        }
        // Check BTC Down
        if let Some(btc_down) = snapshot.btc_market.down_token.as_ref() {
            if let Some(opp) = self
                .check_token(
                    btc_down,
                    TokenType::BtcDown,
                    &snapshot.btc_market.condition_id,
                    snapshot,
                    time_elapsed_seconds,
                    min_elapsed_seconds,
                )
                .await
            {
                opportunities.push(opp);
            }
        }
        // Check ETH Up (only if ETH trading is enabled)
        if self.enable_eth_trading {
            if let Some(eth_up) = snapshot.eth_market.up_token.as_ref() {
                if let Some(opp) = self
                    .check_token(
                        eth_up,
                        TokenType::EthUp,
                        &snapshot.eth_market.condition_id,
                        snapshot,
                        time_elapsed_seconds,
                        min_elapsed_seconds,
                    )
                    .await
                {
                    opportunities.push(opp);
                }
            }
            // Check ETH Down
            if let Some(eth_down) = snapshot.eth_market.down_token.as_ref() {
                if let Some(opp) = self
                    .check_token(
                        eth_down,
                        TokenType::EthDown,
                        &snapshot.eth_market.condition_id,
                        snapshot,
                        time_elapsed_seconds,
                        min_elapsed_seconds,
                    )
                    .await
                {
                    opportunities.push(opp);
                }
            }
        }
        // Check Solana Up (only if Solana trading is enabled)
        if self.enable_solana_trading {
            if let Some(solana_up) = snapshot.solana_market.up_token.as_ref() {
                if let Some(opp) = self
                    .check_token(
                        solana_up,
                        TokenType::SolanaUp,
                        &snapshot.solana_market.condition_id,
                        snapshot,
                        time_elapsed_seconds,
                        min_elapsed_seconds,
                    )
                    .await
                {
                    opportunities.push(opp);
                }
            }
            // Check Solana Down
            if let Some(solana_down) = snapshot.solana_market.down_token.as_ref() {
                if let Some(opp) = self
                    .check_token(
                        solana_down,
                        TokenType::SolanaDown,
                        &snapshot.solana_market.condition_id,
                        snapshot,
                        time_elapsed_seconds,
                        min_elapsed_seconds,
                    )
                    .await
                {
                    opportunities.push(opp);
                }
            }
        }
        // Check XRP Up/Down (if enabled)
        if self.enable_xrp_trading {
            if let Some(xrp_up) = snapshot.xrp_market.up_token.as_ref() {
                if let Some(opp) = self
                    .check_token(
                        xrp_up,
                        TokenType::XrpUp,
                        &snapshot.xrp_market.condition_id,
                        snapshot,
                        time_elapsed_seconds,
                        min_elapsed_seconds,
                    )
                    .await
                {
                    opportunities.push(opp);
                }
            }
            if let Some(xrp_down) = snapshot.xrp_market.down_token.as_ref() {
                if let Some(opp) = self
                    .check_token(
                        xrp_down,
                        TokenType::XrpDown,
                        &snapshot.xrp_market.condition_id,
                        snapshot,
                        time_elapsed_seconds,
                        min_elapsed_seconds,
                    )
                    .await
                {
                    opportunities.push(opp);
                }
            }
        }

        opportunities
    }

    /// Mark that we bought a specific token in this period
    pub async fn mark_token_bought(&self, token_id: String) {
        let mut bought = self.current_period_bought.lock().await;
        bought.insert(token_id);
    }

    /// Reset when new period starts
    pub async fn reset_period(&self) {
        let mut bought = self.current_period_bought.lock().await;
        bought.clear();
        // Also reset reset states for new period
        let mut reset_states = self.reset_states.lock().await;
        reset_states.clear();
    }

    /// Mark that a buy-sell cycle completed for a token type
    /// After this, the price must drop below trigger_price before allowing another buy
    pub async fn mark_cycle_completed(&self, token_type: TokenType) {
        let mut reset_states = self.reset_states.lock().await;
        reset_states.insert(token_type.clone(), ResetState::NeedsReset);
        crate::log_println!("🔄 {}: Buy-sell cycle completed. Will require price to drop below ${:.6} before next buy",
            token_type.display_name(), self.trigger_price);
    }
}

// Helper function for Decimal to f64 conversion
fn decimal_to_f64(d: Decimal) -> f64 {
    d.to_string().parse().unwrap_or(0.0)
}
