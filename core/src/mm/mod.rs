use crate::strategy::Timeframe;

pub mod fair_mid;
pub mod inventory_engine;
pub mod metrics;
pub mod quote_manager;
pub mod reward_scanner;
pub mod risk_supervisor;
pub mod scoring_guard;
pub mod ws_router;

pub const STRATEGY_ID_MM_REWARDS_V1: &str = "mm_rewards_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MmMode {
    QuietStack,
    TailBiased,
    Spike,
    SportsPregame,
}

impl MmMode {
    pub fn all() -> [MmMode; 4] {
        [
            MmMode::QuietStack,
            MmMode::TailBiased,
            MmMode::Spike,
            MmMode::SportsPregame,
        ]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MmMode::QuietStack => "quiet_stack",
            MmMode::TailBiased => "tail_biased",
            MmMode::Spike => "spike",
            MmMode::SportsPregame => "sports_pregame",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmRuntimeMode {
    Shadow,
    Live,
}

impl MmRuntimeMode {
    pub fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "shadow".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "live" => Self::Live,
            _ => Self::Shadow,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Live => "live",
        }
    }

    pub fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmMarketMode {
    Auto,
    Hybrid,
}

impl MmMarketMode {
    pub fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "auto".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "target" | "auto" => Self::Auto,
            "hybrid" => Self::Hybrid,
            _ => Self::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Hybrid => "hybrid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmEnqueueMode {
    Await,
    Bounded,
    TrySend,
}

impl MmEnqueueMode {
    fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "await".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "bounded" => Self::Bounded,
            "try_send" | "trysend" | "try" => Self::TrySend,
            _ => Self::Await,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Await => "await",
            Self::Bounded => "bounded",
            Self::TrySend => "try_send",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmOffTargetCancelMode {
    Disabled,
    Safe,
    Aggressive,
}

impl MmOffTargetCancelMode {
    fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "disabled".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "safe" => Self::Safe,
            "aggressive" | "force" => Self::Aggressive,
            _ => Self::Disabled,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Safe => "safe",
            Self::Aggressive => "aggressive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmToxicReentryCancelScope {
    ThreatenedSide,
    BothSides,
}

impl MmToxicReentryCancelScope {
    fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "threatened_side".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "both" | "both_sides" | "market" => Self::BothSides,
            _ => Self::ThreatenedSide,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ThreatenedSide => "threatened_side",
            Self::BothSides => "both_sides",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmFavorSide {
    Up,
    Down,
    None,
}

impl MmFavorSide {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmRewardMinSizeUnit {
    Shares,
    Usd,
}

impl MmRewardMinSizeUnit {
    fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "shares".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "usd" | "notional" => Self::Usd,
            _ => Self::Shares,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shares => "shares",
            Self::Usd => "usd",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmRewardMinFloorMode {
    PerRung,
    ReferencePrice,
    Hybrid,
}

impl MmRewardMinFloorMode {
    fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "per_rung".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "reference" | "reference_price" => Self::ReferencePrice,
            "hybrid" => Self::Hybrid,
            _ => Self::PerRung,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerRung => "per_rung",
            Self::ReferencePrice => "reference_price",
            Self::Hybrid => "hybrid",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MmModeTarget {
    pub symbol: String,
    pub timeframe: Timeframe,
}

#[derive(Debug, Clone)]
pub struct MmSingleMarketSelector {
    pub lookup_slug: String,
    pub match_slug: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmSportQuoteSizeMode {
    Multiple,
    DepthRatio,
}

impl MmSportQuoteSizeMode {
    fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "multiple".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "depth_ratio" | "depth-ratio" | "ratio" => Self::DepthRatio,
            _ => Self::Multiple,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Multiple => "multiple",
            Self::DepthRatio => "depth_ratio",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MmSportConfig {
    pub enable: bool,
    pub hard_disable: bool,
    pub poll_ms: u64,
    pub event_driven_enable: bool,
    pub event_fallback_poll_ms: u64,
    pub ws_stale_ms: i64,
    pub discovery_refresh_sec: u64,
    pub rewards_page_budget: u32,
    pub min_reward_rate_per_day: f64,
    pub quote_size_mode: MmSportQuoteSizeMode,
    pub quote_size_mult: f64,
    pub pair_baseline_quote_size_mult: f64,
    pub max_share_ratio: f64,
    pub min_top_depth_usd: f64,
    pub pause_after_fill_sec: u64,
    pub bust_window_ms: i64,
    pub bust_shares_1s: f64,
    pub bust_pause_min_sec: u64,
    pub bust_pause_max_sec: u64,
    pub ratio_breach_cancel_cooldown_ms: i64,
    pub ratio_pause_sec: u64,
    pub reprice_min_interval_ms: i64,
    pub quote_expiry_min_sec: u64,
    pub quote_expiry_max_sec: u64,
    pub size_requote_delta_pct: f64,
    pub allowance_refresh_sec: u64,
    pub require_reward_eligible: bool,
    pub pregame_only: bool,
    pub match_only: bool,
    pub post_only: bool,
    pub max_markets: usize,
}

impl Default for MmSportConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl MmSportConfig {
    pub fn from_env() -> Self {
        let bust_pause_min_sec = env_u64("EVPOLY_MM_SPORT_BUST_PAUSE_MIN_SEC", 60).clamp(1, 3_600);
        let bust_pause_max_sec =
            env_u64("EVPOLY_MM_SPORT_BUST_PAUSE_MAX_SEC", 300).clamp(bust_pause_min_sec, 7_200);
        let quote_expiry_min_sec =
            env_u64("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC", 300).clamp(61, 3_600);
        let quote_expiry_max_sec =
            env_u64("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC", 600).clamp(quote_expiry_min_sec, 7_200);
        Self {
            enable: env_bool("EVPOLY_STRATEGY_MM_SPORT_ENABLE", false),
            hard_disable: env_bool("EVPOLY_MM_SPORT_HARD_DISABLE", false),
            poll_ms: env_u64("EVPOLY_MM_SPORT_POLL_MS", 250).clamp(50, 30_000),
            event_driven_enable: env_bool("EVPOLY_MM_SPORT_EVENT_DRIVEN_ENABLE", true),
            event_fallback_poll_ms: env_u64("EVPOLY_MM_SPORT_EVENT_FALLBACK_POLL_MS", 200)
                .clamp(50, 30_000),
            ws_stale_ms: env_u64("EVPOLY_MM_SPORT_WS_STALE_MS", 2_500).clamp(250, 30_000) as i64,
            discovery_refresh_sec: 300,
            rewards_page_budget: env_u32("EVPOLY_MM_SPORT_REWARDS_PAGE_BUDGET", 8).clamp(1, 200),
            min_reward_rate_per_day: env_f64("EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY", 300.0)
                .max(0.0),
            quote_size_mode: MmSportQuoteSizeMode::from_env(
                std::env::var("EVPOLY_MM_SPORT_QUOTE_SIZE_MODE").ok(),
            ),
            quote_size_mult: env_f64("EVPOLY_MM_SPORT_QUOTE_SIZE_MULT", 1.2).clamp(0.1, 20.0),
            pair_baseline_quote_size_mult: env_f64(
                "EVPOLY_MM_SPORT_PAIR_BASELINE_QUOTE_SIZE_MULT",
                1.2,
            )
            .clamp(0.1, 20.0),
            max_share_ratio: env_f64("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", 0.05).clamp(0.01, 0.99),
            min_top_depth_usd: env_f64("EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD", 100_000.0).max(0.0),
            pause_after_fill_sec: env_u64("EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC", 7_200)
                .clamp(60, 86_400),
            bust_window_ms: env_u64("EVPOLY_MM_SPORT_BUST_WINDOW_MS", 1_000).clamp(250, 5_000)
                as i64,
            bust_shares_1s: env_f64("EVPOLY_MM_SPORT_BUST_SHARES_1S", 10_000.0).max(1.0),
            bust_pause_min_sec,
            bust_pause_max_sec,
            ratio_breach_cancel_cooldown_ms: env_u64(
                "EVPOLY_MM_SPORT_RATIO_BREACH_CANCEL_COOLDOWN_MS",
                200,
            )
            .clamp(50, 60_000) as i64,
            ratio_pause_sec: env_u64("EVPOLY_MM_SPORT_RATIO_PAUSE_SEC", 900).clamp(60, 86_400),
            reprice_min_interval_ms: env_u64("EVPOLY_MM_SPORT_REPRICE_MIN_INTERVAL_MS", 600)
                .clamp(50, 60_000) as i64,
            quote_expiry_min_sec,
            quote_expiry_max_sec,
            size_requote_delta_pct: env_f64("EVPOLY_MM_SPORT_SIZE_REQUOTE_DELTA_PCT", 0.03)
                .clamp(0.0, 1.0),
            allowance_refresh_sec: env_u64("EVPOLY_MM_SPORT_ALLOWANCE_REFRESH_SEC", 60)
                .clamp(15, 3_600),
            require_reward_eligible: env_bool("EVPOLY_MM_SPORT_REQUIRE_REWARD_ELIGIBLE", true),
            pregame_only: env_bool("EVPOLY_MM_SPORT_PREGAME_ONLY", true),
            match_only: env_bool("EVPOLY_MM_SPORT_MATCH_ONLY", true),
            // MM Sport is maker-only by design.
            post_only: true,
            max_markets: env_usize("EVPOLY_MM_SPORT_MAX_MARKETS", 0),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MmRewardsConfig {
    pub enable: bool,
    pub hard_disable: bool,
    pub runtime_mode: MmRuntimeMode,
    pub market_mode: MmMarketMode,
    pub auto_require_rewards_feed: bool,
    pub auto_top_n: usize,
    pub auto_refresh_sec: u64,
    pub auto_scan_limit: u32,
    pub auto_min_feasible_levels: usize,
    pub auto_min_stay_sec: u64,
    pub auto_rotation_budget_per_refresh: usize,
    pub auto_force_top_reward: bool,
    pub auto_force_include_reward_min: f64,
    pub auto_rank_budget_usd: f64,
    pub min_reward_rate_hint: f64,
    pub min_total_reward_quote: f64,
    pub max_reward_min_size: f64,
    pub near_expiry_exit_window_sec: u64,
    pub rank_alpha_reward: f64,
    pub rank_beta_risk: f64,
    pub rv_enable: bool,
    pub rv_window_short_sec: u64,
    pub rv_window_mid_sec: u64,
    pub rv_window_long_sec: u64,
    pub rv_block_threshold: f64,
    pub event_driven_enable: bool,
    pub event_scope_filter_enable: bool,
    pub event_fallback_poll_ms: u64,
    pub poll_ms: u64,
    pub quote_ttl_ms: u64,
    pub reprice_min_interval_ms: u64,
    pub requote_min_price_delta_ticks: u32,
    pub requote_anchor_levels: usize,
    pub requote_min_size_delta_pct: f64,
    pub enqueue_mode: MmEnqueueMode,
    pub enqueue_timeout_ms: u64,
    pub queue_pressure_telemetry: bool,
    pub constraints_cache_ms: u64,
    pub quote_shares_per_side: f64,
    pub refill_shares_per_fill: f64,
    pub max_shares_per_order: f64,
    pub ladder_levels: usize,
    pub min_price: f64,
    pub max_price: f64,
    pub min_tick_move: f64,
    pub bbo_hit_cooldown_ms: i64,
    pub bbo1_max_size_fraction: f64,
    pub adverse_tick_trigger_ticks: u32,
    pub normal_safe_buffer_enable: bool,
    pub normal_safe_buffer_ticks: usize,
    pub normal_safe_buffer_ticks_low: usize,
    pub normal_safe_buffer_ticks_medium: usize,
    pub normal_safe_buffer_ticks_high: usize,
    pub fair_mid_enable: bool,
    pub fair_mid_up_step_bps: f64,
    pub fair_mid_down_step_bps: f64,
    pub fair_mid_cap_offset_bps: f64,
    pub normal_top_reprice_min_rest_ms: i64,
    pub normal_top_reprice_cooldown_ms: i64,
    pub favor_side: MmFavorSide,
    pub max_favor_inventory_shares: f64,
    pub max_weak_inventory_shares: f64,
    pub weak_exit_enable: bool,
    pub weak_exit_profit_min: f64,
    pub weak_exit_require_entry_basis: bool,
    pub weak_exit_entry_basis_ttl_ms: i64,
    pub weak_exit_allow_loss_on_imbalance: bool,
    pub weak_exit_imbalance_max_loss_pct: f64,
    pub weak_exit_imbalance_force_touch_ratio: f64,
    pub weak_exit_max_order_shares: f64,
    pub weak_exit_dynamic_imbalance_enable: bool,
    pub weak_exit_imbalance_ladder_levels: usize,
    pub weak_exit_imbalance_safety_buffer_shares: f64,
    pub weak_exit_imbalance_max_pct_of_bbo_ask_size: f64,
    pub weak_exit_band_hard_ratio: f64,
    pub weak_exit_band_emergency_ratio: f64,
    pub weak_exit_band_hard_time_ms: i64,
    pub weak_exit_band_emergency_time_ms: i64,
    pub weak_exit_band_markout_hard_bps: f64,
    pub weak_exit_band_markout_emergency_bps: f64,
    pub weak_exit_band_soft_loss_pct: f64,
    pub weak_exit_band_hard_loss_pct: f64,
    pub weak_exit_band_emergency_loss_pct: f64,
    pub weak_exit_band_hard_extra_deepen_ticks: usize,
    pub weak_exit_band_emergency_extra_deepen_ticks: usize,
    pub weak_exit_band_hard_reprice_mult: f64,
    pub weak_exit_band_emergency_reprice_mult: f64,
    pub weak_exit_settle_grace_ms: i64,
    pub weak_exit_no_inventory_confirm_loops: u32,
    pub weak_exit_max_replaces_per_min: u32,
    pub weak_exit_stale_fallback_enable: bool,
    pub weak_exit_stale_fallback_max_age_ms: i64,
    pub weak_exit_stale_fallback_haircut: f64,
    pub weak_exit_stale_fallback_cap_shares: f64,
    pub weak_exit_force_min_active_rungs: usize,
    pub weak_exit_spread_capture_enable: bool,
    pub forced_exit_deep_bid_inside_offset_cents: f64,
    pub force_buy_min_price: f64,
    pub replacement_cooldown_sec: u64,
    pub skew_ratio_soft: f64,
    pub skew_ratio_hard: f64,
    pub inventory_imbalance_trigger_shares: f64,
    pub skew_size_mult_min: f64,
    pub skew_deepen_levels_max: usize,
    pub competition_high_freeze_enable: bool,
    pub competition_level_allowlist: Vec<String>,
    pub market_blacklist_keywords: Vec<String>,
    pub spike_reward_rate_min: f64,
    pub mode_enable_quiet_stack: bool,
    pub mode_enable_tail_biased: bool,
    pub mode_enable_spike: bool,
    pub mode_enable_sports_pregame: bool,
    pub single_market_selectors: Vec<MmSingleMarketSelector>,
    pub single_market_slug: Option<String>,
    pub single_market_match_slug: Option<String>,
    pub single_market_max_entry_fills: u32,
    pub markout_enable: bool,
    pub markout_gate_bps: f64,
    pub markout_recover_bps: f64,
    pub markout_min_samples: u64,
    pub markout_max_book_age_ms: i64,
    pub markout_winsor_bps: f64,
    pub markout_ewma_halflife_fills: f64,
    pub prefill_drift_enable: bool,
    pub prefill_drift_gate_bps: f64,
    pub prefill_drift_recover_bps: f64,
    pub prefill_drift_min_samples: u64,
    pub prefill_drift_bbo1_size_fraction_mult_min: f64,
    pub prefill_drift_first_level_deepen_ticks_max: usize,
    pub pickoff_enable: bool,
    pub reward_min_size_enforce: bool,
    pub reward_min_size_unit: MmRewardMinSizeUnit,
    pub reward_min_target_mult: f64,
    pub reward_min_shares_cap: f64,
    pub reward_min_floor_mode: MmRewardMinFloorMode,
    pub reward_min_floor_max_mult: f64,
    pub require_reward_eligible: bool,
    pub allow_unwind_only_when_not_reward_eligible: bool,
    pub scoring_guard_enable: bool,
    pub scoring_guard_require_qmin: bool,
    pub scoring_spread_extend_cents: f64,
    pub stale_reconcile_interval_ms: u64,
    pub stale_reconcile_max_lookups: usize,
    pub stale_reconcile_concurrency: usize,
    pub stale_reconcile_api_budget_per_min: usize,
    pub stale_periodic_full_sweep_sec: u64,
    pub stale_periodic_full_sweep_max_lookups: usize,
    pub stale_strike_threshold: u32,
    pub stale_recovery_required: u32,
    pub stale_pause_hold_ms: i64,
    pub off_target_cancel_mode: MmOffTargetCancelMode,
    pub off_target_cancel_max_per_pass: usize,
    pub stale_market_event_ms: i64,
    pub entry_basis_period_scoped: bool,
    pub toxicity_enable: bool,
    pub toxicity_pause_ms: i64,
    pub toxicity_return_250ms_bps: f64,
    pub toxicity_return_1s_bps: f64,
    pub toxicity_micro_skew: f64,
    pub toxicity_burst_updates_1s: usize,
    pub toxicity_depth_thin_enable: bool,
    pub toxicity_depth_lookback_ms: i64,
    pub toxicity_depth_thin_ratio: f64,
    pub toxic_reentry_enable: bool,
    pub toxic_reentry_hold_ms: i64,
    pub toxic_reentry_recover_bps: f64,
    pub toxic_reentry_recover_updates: usize,
    pub toxic_reentry_depth_ticks_min: usize,
    pub toxic_reentry_depth_ticks_max: usize,
    pub toxic_reentry_drop_fraction: f64,
    pub toxic_reentry_cancel_scope: MmToxicReentryCancelScope,
    pub reward_stub_enable: bool,
    pub reward_stub_levels: usize,
    pub reward_stub_size_mult: f64,
    pub reward_stub_min_net_edge_bps: f64,
    pub fee_enabled_dynamic_check: bool,
    pub action_budget_enable: bool,
    pub action_window_sec: u64,
    pub action_per_min_conserve: f64,
    pub action_per_min_survival: f64,
    pub action_cancel_fill_ratio_conserve: f64,
    pub action_cancel_fill_ratio_survival: f64,
    pub action_stale_strikes_survival: u32,
    pub pending_cancel_hold_ms: i64,
    pub requote_anchor_min_rest_ms: i64,
    pub requote_outer_min_rest_ms: i64,
    pub conserve_actions_enable: bool,
    pub conserve_max_levels: usize,
    pub conserve_spread_mult_max: f64,
    pub conserve_size_mult_min: f64,
    pub conserve_hold_ms: i64,
    pub unwind_actions_enable: bool,
    pub unwind_require_markout: bool,
    pub unwind_markout_bps: f64,
    pub unwind_min_inventory_ratio: f64,
    pub markout_dynamic_enable: bool,
    pub markout_dynamic_k_gate: f64,
    pub markout_dynamic_k_recover: f64,
}

impl Default for MmRewardsConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl MmRewardsConfig {
    pub fn from_env() -> Self {
        let runtime_mode = std::env::var("EVPOLY_MM_RUNTIME_MODE")
            .ok()
            .map(|raw| MmRuntimeMode::from_env(Some(raw)))
            .unwrap_or(MmRuntimeMode::Live);
        let market_mode = MmMarketMode::from_env(std::env::var("EVPOLY_MM_MARKET_MODE").ok());
        let mut single_market_selectors =
            parse_single_market_selector_list_env("EVPOLY_MM_SINGLE_MARKET_SLUGS");
        if single_market_selectors.is_empty() {
            if let Some((lookup_slug, exact_market_slug)) =
                env_string("EVPOLY_MM_SINGLE_MARKET_SLUG")
                    .as_deref()
                    .and_then(parse_single_market_selector)
            {
                single_market_selectors.push(MmSingleMarketSelector {
                    lookup_slug,
                    match_slug: exact_market_slug,
                });
            }
        }
        let (single_market_slug, single_market_match_slug) = single_market_selectors
            .first()
            .map(|selector| {
                (
                    Some(selector.lookup_slug.clone()),
                    selector.match_slug.clone(),
                )
            })
            .unwrap_or((None, None));

        Self {
            enable: env_bool("EVPOLY_STRATEGY_MM_REWARDS_ENABLE", false),
            hard_disable: env_bool("EVPOLY_MM_HARD_DISABLE", false),
            runtime_mode,
            market_mode,
            auto_require_rewards_feed: env_bool("EVPOLY_MM_AUTO_REQUIRE_REWARDS_FEED", true),
            auto_top_n: 80,
            auto_refresh_sec: env_u64("EVPOLY_MM_AUTO_REFRESH_SEC", 900).clamp(10, 900),
            auto_scan_limit: 1_000,
            auto_min_feasible_levels: 1,
            auto_min_stay_sec: env_u64("EVPOLY_MM_AUTO_MIN_STAY_SEC", 7_200)
                .clamp(60, 7 * 24 * 3_600),
            auto_rotation_budget_per_refresh: env_usize(
                "EVPOLY_MM_AUTO_ROTATION_BUDGET_PER_REFRESH",
                1,
            )
            .clamp(0, 16),
            auto_force_top_reward: env_bool("EVPOLY_MM_AUTO_FORCE_TOP_REWARD", true),
            auto_force_include_reward_min: env_f64(
                "EVPOLY_MM_AUTO_FORCE_INCLUDE_REWARD_MIN",
                300.0,
            )
            .max(0.0),
            auto_rank_budget_usd: 2_000.0,
            min_reward_rate_hint: env_f64("EVPOLY_MM_MIN_REWARD_RATE_HINT", 15.0).max(0.0),
            min_total_reward_quote: env_f64("EVPOLY_MM_MIN_TOTAL_REWARD_QUOTE", 15.0).max(0.0),
            max_reward_min_size: env_f64("EVPOLY_MM_MAX_REWARD_MIN_SIZE", 0.0).max(0.0),
            near_expiry_exit_window_sec: env_u64(
                "EVPOLY_MM_NEAR_EXPIRY_EXIT_WINDOW_SEC",
                24 * 3_600,
            )
            .clamp(60, 30 * 24 * 3_600),
            rank_alpha_reward: env_f64("EVPOLY_MM_RANK_ALPHA_REWARD", 1.0).clamp(0.01, 100.0),
            rank_beta_risk: env_f64("EVPOLY_MM_RANK_BETA_RISK", 1.0).clamp(0.01, 100.0),
            rv_enable: env_bool("EVPOLY_MM_RV_ENABLE", true),
            rv_window_short_sec: env_u64("EVPOLY_MM_RV_WINDOW_SHORT_SEC", 3_600)
                .clamp(60, 7 * 24 * 3_600),
            rv_window_mid_sec: env_u64("EVPOLY_MM_RV_WINDOW_MID_SEC", 10_800)
                .clamp(300, 14 * 24 * 3_600),
            rv_window_long_sec: env_u64("EVPOLY_MM_RV_WINDOW_LONG_SEC", 86_400)
                .clamp(600, 30 * 24 * 3_600),
            rv_block_threshold: env_f64("EVPOLY_MM_RV_BLOCK_THRESHOLD", 250.0).max(0.0),
            event_driven_enable: env_bool("EVPOLY_MM_EVENT_DRIVEN_ENABLE", true),
            event_scope_filter_enable: env_bool("EVPOLY_MM_EVENT_SCOPE_FILTER_ENABLE", true),
            event_fallback_poll_ms: env_u64("EVPOLY_MM_EVENT_FALLBACK_POLL_MS", 1_000)
                .clamp(50, 30_000),
            poll_ms: env_u64("EVPOLY_MM_POLL_MS", 200).clamp(50, 30_000),
            quote_ttl_ms: env_u64("EVPOLY_MM_QUOTE_TTL_MS", 20_000).clamp(1_000, 300_000),
            reprice_min_interval_ms: env_u64("EVPOLY_MM_REPRICE_MIN_INTERVAL_MS", 120)
                .clamp(50, 60_000),
            requote_min_price_delta_ticks: env_u32("EVPOLY_MM_REQUOTE_MIN_PRICE_DELTA_TICKS", 1)
                .clamp(1, 20),
            requote_anchor_levels: env_usize("EVPOLY_MM_REQUOTE_ANCHOR_LEVELS", 2).clamp(1, 10),
            requote_min_size_delta_pct: env_f64("EVPOLY_MM_REQUOTE_MIN_SIZE_DELTA_PCT", 0.10)
                .clamp(0.0, 1.0),
            enqueue_mode: MmEnqueueMode::from_env(std::env::var("EVPOLY_MM_ENQUEUE_MODE").ok()),
            enqueue_timeout_ms: env_u64("EVPOLY_MM_ENQUEUE_TIMEOUT_MS", 0).clamp(0, 30_000),
            queue_pressure_telemetry: env_bool("EVPOLY_MM_QUEUE_PRESSURE_TELEMETRY", true),
            constraints_cache_ms: 30_000,
            quote_shares_per_side: env_f64("EVPOLY_MM_QUOTE_SHARES_PER_SIDE", 5.0).max(1.0),
            refill_shares_per_fill: 1.0,
            max_shares_per_order: env_f64("EVPOLY_MM_MAX_SHARES_PER_ORDER", 5_000.0).max(1.0),
            ladder_levels: env_usize("EVPOLY_MM_LADDER_LEVELS", 5).clamp(1, 10),
            min_price: env_f64("EVPOLY_MM_MIN_PRICE", 0.01).clamp(0.01, 0.98),
            max_price: env_f64("EVPOLY_MM_MAX_PRICE", 0.99).clamp(0.02, 0.99),
            min_tick_move: env_f64("EVPOLY_MM_MIN_TICK_MOVE", 0.01).clamp(0.001, 0.05),
            bbo_hit_cooldown_ms: (env_u64("EVPOLY_MM_BBO_HIT_COOLDOWN_SEC", 10).clamp(1, 120)
                * 1_000) as i64,
            bbo1_max_size_fraction: 0.40,
            adverse_tick_trigger_ticks: env_u32("EVPOLY_MM_ADVERSE_TICK_TRIGGER_TICKS", 1)
                .clamp(1, 5),
            normal_safe_buffer_enable: env_bool("EVPOLY_MM_NORMAL_SAFE_BUFFER_ENABLE", true),
            normal_safe_buffer_ticks: env_usize("EVPOLY_MM_NORMAL_SAFE_BUFFER_TICKS", 1)
                .clamp(1, 10),
            normal_safe_buffer_ticks_low: env_usize("EVPOLY_MM_NORMAL_SAFE_BUFFER_TICKS_LOW", 1)
                .clamp(1, 10),
            normal_safe_buffer_ticks_medium: env_usize(
                "EVPOLY_MM_NORMAL_SAFE_BUFFER_TICKS_MEDIUM",
                2,
            )
            .clamp(1, 10),
            normal_safe_buffer_ticks_high: env_usize("EVPOLY_MM_NORMAL_SAFE_BUFFER_TICKS_HIGH", 3)
                .clamp(1, 10),
            fair_mid_enable: env_bool("EVPOLY_MM_FAIR_MID_ENABLE", true),
            fair_mid_up_step_bps: env_f64("EVPOLY_MM_FAIR_MID_UP_STEP_BPS", 20.0)
                .clamp(0.1, 5_000.0),
            fair_mid_down_step_bps: env_f64("EVPOLY_MM_FAIR_MID_DOWN_STEP_BPS", 120.0)
                .clamp(0.1, 5_000.0),
            fair_mid_cap_offset_bps: env_f64("EVPOLY_MM_FAIR_MID_CAP_OFFSET_BPS", 50.0)
                .clamp(0.0, 5_000.0),
            normal_top_reprice_min_rest_ms: env_u64("EVPOLY_MM_NORMAL_TOP_REPRICE_MIN_REST_MS", 700)
                .clamp(50, 60_000) as i64,
            normal_top_reprice_cooldown_ms: env_u64(
                "EVPOLY_MM_NORMAL_TOP_REPRICE_COOLDOWN_MS",
                1_500,
            )
            .clamp(100, 120_000) as i64,
            favor_side: MmFavorSide::None,
            max_favor_inventory_shares: 10_000.0,
            max_weak_inventory_shares: 10_000.0,
            weak_exit_enable: env_bool("EVPOLY_MM_WEAK_EXIT_ENABLE", true),
            weak_exit_profit_min: env_f64("EVPOLY_MM_WEAK_EXIT_PROFIT_MIN", 0.01).clamp(0.0, 0.20),
            weak_exit_require_entry_basis: env_bool(
                "EVPOLY_MM_WEAK_EXIT_REQUIRE_ENTRY_BASIS",
                false,
            ),
            weak_exit_entry_basis_ttl_ms: env_u64("EVPOLY_MM_WEAK_EXIT_ENTRY_BASIS_TTL_SEC", 7_200)
                .clamp(60, 604_800) as i64
                * 1_000,
            weak_exit_allow_loss_on_imbalance: env_bool(
                "EVPOLY_MM_WEAK_EXIT_ALLOW_LOSS_ON_IMBALANCE",
                true,
            ),
            weak_exit_imbalance_max_loss_pct: env_f64(
                "EVPOLY_MM_WEAK_EXIT_IMBALANCE_MAX_LOSS_PCT",
                0.10,
            )
            .clamp(0.0, 0.99),
            weak_exit_imbalance_force_touch_ratio: env_f64(
                "EVPOLY_MM_WEAK_EXIT_IMBALANCE_FORCE_TOUCH_RATIO",
                2.0,
            )
            .clamp(1.0, 20.0),
            weak_exit_max_order_shares: env_f64("EVPOLY_MM_WEAK_EXIT_MAX_ORDER_SHARES", 200.0)
                .max(1.0),
            weak_exit_dynamic_imbalance_enable: env_bool(
                "EVPOLY_MM_WEAK_EXIT_DYNAMIC_IMBALANCE_ENABLE",
                true,
            ),
            weak_exit_imbalance_ladder_levels: env_usize(
                "EVPOLY_MM_WEAK_EXIT_IMBALANCE_LADDER_LEVELS",
                5,
            )
            .clamp(1, 10),
            weak_exit_imbalance_safety_buffer_shares: env_f64(
                "EVPOLY_MM_WEAK_EXIT_IMBALANCE_SAFETY_BUFFER_SHARES",
                0.0,
            )
            .max(0.0),
            weak_exit_imbalance_max_pct_of_bbo_ask_size: env_f64(
                "EVPOLY_MM_WEAK_EXIT_IMBALANCE_MAX_PCT_OF_BBO_ASK_SIZE",
                3.0,
            )
            .clamp(0.1, 20.0),
            weak_exit_band_hard_ratio: env_f64("EVPOLY_MM_WEAK_EXIT_BAND_HARD_RATIO", 2.0)
                .clamp(1.0, 50.0),
            weak_exit_band_emergency_ratio: env_f64(
                "EVPOLY_MM_WEAK_EXIT_BAND_EMERGENCY_RATIO",
                5.0,
            )
            .clamp(1.0, 100.0),
            weak_exit_band_hard_time_ms: env_u64("EVPOLY_MM_WEAK_EXIT_BAND_HARD_TIME_SEC", 120)
                .clamp(1, 86_400) as i64
                * 1_000,
            weak_exit_band_emergency_time_ms: env_u64(
                "EVPOLY_MM_WEAK_EXIT_BAND_EMERGENCY_TIME_SEC",
                600,
            )
            .clamp(1, 172_800) as i64
                * 1_000,
            weak_exit_band_markout_hard_bps: env_f64(
                "EVPOLY_MM_WEAK_EXIT_BAND_MARKOUT_HARD_BPS",
                -2.0,
            )
            .clamp(-2_000.0, 2_000.0),
            weak_exit_band_markout_emergency_bps: env_f64(
                "EVPOLY_MM_WEAK_EXIT_BAND_MARKOUT_EMERGENCY_BPS",
                -4.0,
            )
            .clamp(-2_000.0, 2_000.0),
            weak_exit_band_soft_loss_pct: env_f64("EVPOLY_MM_WEAK_EXIT_BAND_SOFT_LOSS_PCT", 0.03)
                .clamp(0.0, 0.99),
            weak_exit_band_hard_loss_pct: env_f64("EVPOLY_MM_WEAK_EXIT_BAND_HARD_LOSS_PCT", 0.07)
                .clamp(0.0, 0.99),
            weak_exit_band_emergency_loss_pct: env_f64(
                "EVPOLY_MM_WEAK_EXIT_BAND_EMERGENCY_LOSS_PCT",
                0.10,
            )
            .clamp(0.0, 0.99),
            weak_exit_band_hard_extra_deepen_ticks: env_usize(
                "EVPOLY_MM_WEAK_EXIT_BAND_HARD_EXTRA_DEEPEN_TICKS",
                1,
            )
            .clamp(0, 20),
            weak_exit_band_emergency_extra_deepen_ticks: env_usize(
                "EVPOLY_MM_WEAK_EXIT_BAND_EMERGENCY_EXTRA_DEEPEN_TICKS",
                2,
            )
            .clamp(0, 20),
            weak_exit_band_hard_reprice_mult: env_f64(
                "EVPOLY_MM_WEAK_EXIT_BAND_HARD_REPRICE_MULT",
                0.5,
            )
            .clamp(0.05, 1.0),
            weak_exit_band_emergency_reprice_mult: env_f64(
                "EVPOLY_MM_WEAK_EXIT_BAND_EMERGENCY_REPRICE_MULT",
                0.25,
            )
            .clamp(0.05, 1.0),
            weak_exit_settle_grace_ms: env_u64("EVPOLY_MM_WEAK_EXIT_SETTLE_GRACE_MS", 3_000)
                .clamp(500, 60_000) as i64,
            weak_exit_no_inventory_confirm_loops: env_u32(
                "EVPOLY_MM_WEAK_EXIT_NO_INVENTORY_CONFIRM_LOOPS",
                3,
            )
            .clamp(1, 20),
            weak_exit_max_replaces_per_min: env_u32("EVPOLY_MM_WEAK_EXIT_MAX_REPLACES_PER_MIN", 20)
                .clamp(1, 600),
            weak_exit_stale_fallback_enable: env_bool(
                "EVPOLY_MM_WEAK_EXIT_STALE_FALLBACK_ENABLE",
                false,
            ),
            weak_exit_stale_fallback_max_age_ms: env_u64(
                "EVPOLY_MM_WEAK_EXIT_STALE_FALLBACK_MAX_AGE_SEC",
                120,
            )
            .clamp(1, 3_600) as i64
                * 1_000,
            weak_exit_stale_fallback_haircut: env_f64(
                "EVPOLY_MM_WEAK_EXIT_STALE_FALLBACK_HAIRCUT",
                0.60,
            )
            .clamp(0.05, 1.0),
            weak_exit_stale_fallback_cap_shares: env_f64(
                "EVPOLY_MM_WEAK_EXIT_STALE_FALLBACK_CAP_SHARES",
                200.0,
            )
            .max(1.0),
            weak_exit_force_min_active_rungs: env_usize(
                "EVPOLY_MM_WEAK_EXIT_FORCE_MIN_ACTIVE_RUNGS",
                1,
            )
            .clamp(1, 3),
            weak_exit_spread_capture_enable: env_bool(
                "EVPOLY_MM_WEAK_EXIT_SPREAD_CAPTURE_ENABLE",
                true,
            ),
            forced_exit_deep_bid_inside_offset_cents: env_f64(
                "EVPOLY_MM_FORCED_EXIT_DEEP_BID_INSIDE_OFFSET_CENTS",
                1.0,
            )
            .clamp(0.0, 10.0),
            force_buy_min_price: env_f64("EVPOLY_MM_FORCE_BUY_MIN_PRICE", 0.10).clamp(0.01, 0.99),
            replacement_cooldown_sec: env_u64("EVPOLY_MM_REPLACEMENT_COOLDOWN_SEC", 180)
                .clamp(30, 3_600),
            skew_ratio_soft: env_f64("EVPOLY_MM_SKEW_RATIO_SOFT", 1.5).clamp(1.0, 10.0),
            skew_ratio_hard: env_f64("EVPOLY_MM_SKEW_RATIO_HARD", 2.0).clamp(1.0, 20.0),
            inventory_imbalance_trigger_shares: env_f64(
                "EVPOLY_MM_INVENTORY_IMBALANCE_TRIGGER_SHARES",
                200.0,
            )
            .clamp(1.0, 1_000_000.0),
            skew_size_mult_min: env_f64("EVPOLY_MM_SKEW_SIZE_MULT_MIN", 0.35).clamp(0.05, 1.0),
            skew_deepen_levels_max: env_usize("EVPOLY_MM_SKEW_DEEPEN_LEVELS_MAX", 3).clamp(0, 8),
            competition_high_freeze_enable: env_bool(
                "EVPOLY_MM_COMPETITION_HIGH_FREEZE_ENABLE",
                false,
            ),
            competition_level_allowlist: parse_competition_level_allowlist_env(
                "EVPOLY_MM_COMPETITION_LEVEL_ALLOWLIST",
            ),
            market_blacklist_keywords: parse_string_list_env("EVPOLY_MM_MARKET_BLACKLIST_KEYWORDS"),
            spike_reward_rate_min: env_f64("EVPOLY_MM_SPIKE_REWARD_RATE_MIN", 50.0).max(0.0),
            mode_enable_quiet_stack: true,
            mode_enable_tail_biased: true,
            mode_enable_spike: true,
            mode_enable_sports_pregame: false,
            single_market_selectors,
            single_market_slug,
            single_market_match_slug,
            single_market_max_entry_fills: env_u32("EVPOLY_MM_SINGLE_MARKET_MAX_ENTRY_FILLS", 0),
            markout_enable: env_bool("EVPOLY_MM_MARKOUT_ENABLE", true),
            markout_gate_bps: env_f64("EVPOLY_MM_MARKOUT_GATE_BPS", -4.0).clamp(-500.0, 500.0),
            markout_recover_bps: env_f64("EVPOLY_MM_MARKOUT_RECOVER_BPS", -1.0)
                .clamp(-500.0, 500.0),
            markout_min_samples: env_u64("EVPOLY_MM_MARKOUT_MIN_SAMPLES", 20).clamp(1, 10_000),
            markout_max_book_age_ms: env_u64("EVPOLY_MM_MARKOUT_MAX_BOOK_AGE_MS", 250)
                .clamp(50, 10_000) as i64,
            markout_winsor_bps: env_f64("EVPOLY_MM_MARKOUT_WINSOR_BPS", 40.0).clamp(1.0, 1_000.0),
            markout_ewma_halflife_fills: env_f64("EVPOLY_MM_MARKOUT_EWMA_HALFLIFE_FILLS", 60.0)
                .clamp(1.0, 10_000.0),
            prefill_drift_enable: env_bool("EVPOLY_MM_PREFILL_DRIFT_ENABLE", true),
            prefill_drift_gate_bps: env_f64("EVPOLY_MM_PREFILL_DRIFT_GATE_BPS", -3.0)
                .clamp(-500.0, 500.0),
            prefill_drift_recover_bps: env_f64("EVPOLY_MM_PREFILL_DRIFT_RECOVER_BPS", -1.0)
                .clamp(-500.0, 500.0),
            prefill_drift_min_samples: env_u64("EVPOLY_MM_PREFILL_DRIFT_MIN_SAMPLES", 20)
                .clamp(1, 10_000),
            prefill_drift_bbo1_size_fraction_mult_min: env_f64(
                "EVPOLY_MM_PREFILL_DRIFT_BBO1_SIZE_FRACTION_MULT_MIN",
                0.50,
            )
            .clamp(0.05, 1.0),
            prefill_drift_first_level_deepen_ticks_max: env_usize(
                "EVPOLY_MM_PREFILL_DRIFT_FIRST_LEVEL_DEEPEN_TICKS_MAX",
                3,
            )
            .clamp(0, 8),
            pickoff_enable: env_bool("EVPOLY_MM_PICKOFF_ENABLE", true),
            reward_min_size_enforce: env_bool("EVPOLY_MM_REWARD_MIN_SIZE_ENFORCE", true),
            reward_min_size_unit: MmRewardMinSizeUnit::from_env(
                std::env::var("EVPOLY_MM_REWARD_MIN_SIZE_UNIT").ok(),
            ),
            reward_min_target_mult: env_f64("EVPOLY_MM_REWARD_MIN_TARGET_MULT", 1.0)
                .clamp(0.0, 20.0),
            reward_min_shares_cap: env_f64("EVPOLY_MM_REWARD_MIN_SHARES_CAP", 0.0).max(0.0),
            reward_min_floor_mode: MmRewardMinFloorMode::from_env(
                std::env::var("EVPOLY_MM_REWARD_MIN_FLOOR_MODE").ok(),
            ),
            reward_min_floor_max_mult: env_f64("EVPOLY_MM_REWARD_MIN_FLOOR_MAX_MULT", 1.0)
                .clamp(1.0, 20.0),
            require_reward_eligible: env_bool("EVPOLY_MM_REQUIRE_REWARD_ELIGIBLE", true),
            allow_unwind_only_when_not_reward_eligible: env_bool(
                "EVPOLY_MM_ALLOW_UNWIND_ONLY_WHEN_NOT_REWARD_ELIGIBLE",
                true,
            ),
            scoring_guard_enable: env_bool("EVPOLY_MM_SCORING_GUARD_ENABLE", true),
            scoring_guard_require_qmin: env_bool("EVPOLY_MM_SCORING_GUARD_REQUIRE_QMIN", true),
            scoring_spread_extend_cents: env_f64("EVPOLY_MM_SCORING_SPREAD_EXTEND_CENTS", 2.0)
                .max(0.0),
            stale_reconcile_interval_ms: env_u64("EVPOLY_MM_STALE_RECONCILE_INTERVAL_MS", 2_000)
                .clamp(200, 60_000),
            stale_reconcile_max_lookups: env_usize("EVPOLY_MM_STALE_RECONCILE_MAX_LOOKUPS", 8)
                .clamp(1, 200),
            stale_reconcile_concurrency: env_usize("EVPOLY_MM_STALE_RECONCILE_CONCURRENCY", 8)
                .clamp(1, 64),
            stale_reconcile_api_budget_per_min: env_usize(
                "EVPOLY_MM_STALE_RECONCILE_API_BUDGET_PER_MIN",
                2_000,
            )
            .clamp(0, 200_000),
            stale_periodic_full_sweep_sec: env_u64("EVPOLY_MM_STALE_PERIODIC_FULL_SWEEP_SEC", 60)
                .clamp(0, 86_400),
            stale_periodic_full_sweep_max_lookups: env_usize(
                "EVPOLY_MM_STALE_PERIODIC_FULL_SWEEP_MAX_LOOKUPS",
                2_000,
            )
            .clamp(1, 10_000),
            stale_strike_threshold: env_u32("EVPOLY_MM_STALE_STRIKE_THRESHOLD", 2).clamp(1, 20),
            stale_recovery_required: env_u32("EVPOLY_MM_STALE_RECOVERY_REQUIRED", 2).clamp(1, 20),
            stale_pause_hold_ms: env_u64("EVPOLY_MM_STALE_PAUSE_HOLD_SEC", 60).clamp(1, 3_600)
                as i64
                * 1_000,
            off_target_cancel_mode: MmOffTargetCancelMode::from_env(
                std::env::var("EVPOLY_MM_OFF_TARGET_CANCEL_MODE").ok(),
            ),
            off_target_cancel_max_per_pass: env_usize(
                "EVPOLY_MM_OFF_TARGET_CANCEL_MAX_PER_PASS",
                0,
            )
            .clamp(0, 1_000),
            stale_market_event_ms: env_u64("EVPOLY_MM_STALE_MARKET_EVENT_MS", 3_000)
                .clamp(200, 120_000) as i64,
            entry_basis_period_scoped: env_bool("EVPOLY_MM_ENTRY_BASIS_PERIOD_SCOPED", false),
            toxicity_enable: env_bool("EVPOLY_MM_TOXICITY_ENABLE", true),
            toxicity_pause_ms: env_u64("EVPOLY_MM_TOXICITY_PAUSE_MS", 1_500).clamp(100, 60_000)
                as i64,
            toxicity_return_250ms_bps: env_f64("EVPOLY_MM_TOXICITY_RET_250MS_BPS", -2.0)
                .clamp(-5_000.0, 5_000.0),
            toxicity_return_1s_bps: env_f64("EVPOLY_MM_TOXICITY_RET_1S_BPS", -4.0)
                .clamp(-5_000.0, 5_000.0),
            toxicity_micro_skew: env_f64("EVPOLY_MM_TOXICITY_MICRO_SKEW", 0.25).clamp(0.0, 1.0),
            toxicity_burst_updates_1s: env_usize("EVPOLY_MM_TOXICITY_BURST_UPDATES_1S", 8)
                .clamp(1, 200),
            toxicity_depth_thin_enable: env_bool("EVPOLY_MM_TOXICITY_DEPTH_THIN_ENABLE", true),
            toxicity_depth_lookback_ms: env_u64("EVPOLY_MM_TOXICITY_DEPTH_LOOKBACK_SEC", 300)
                .clamp(10, 3_600) as i64
                * 1_000,
            toxicity_depth_thin_ratio: env_f64("EVPOLY_MM_TOXICITY_DEPTH_THIN_RATIO", 1.0)
                .clamp(0.1, 10.0),
            toxic_reentry_enable: env_bool("EVPOLY_MM_TOXIC_REENTRY_ENABLE", true),
            toxic_reentry_hold_ms: env_u64("EVPOLY_MM_TOXIC_REENTRY_HOLD_MS", 4_000)
                .clamp(250, 300_000) as i64,
            toxic_reentry_recover_bps: env_f64("EVPOLY_MM_TOXIC_REENTRY_RECOVER_BPS", -0.5)
                .clamp(-5_000.0, 5_000.0),
            toxic_reentry_recover_updates: env_usize("EVPOLY_MM_TOXIC_REENTRY_RECOVER_UPDATES", 3)
                .clamp(1, 200),
            toxic_reentry_depth_ticks_min: env_usize("EVPOLY_MM_TOXIC_REENTRY_DEPTH_TICKS_MIN", 2)
                .clamp(0, 20),
            toxic_reentry_depth_ticks_max: env_usize("EVPOLY_MM_TOXIC_REENTRY_DEPTH_TICKS_MAX", 8)
                .clamp(1, 50),
            toxic_reentry_drop_fraction: env_f64("EVPOLY_MM_TOXIC_REENTRY_DROP_FRACTION", 0.45)
                .clamp(0.0, 0.95),
            toxic_reentry_cancel_scope: MmToxicReentryCancelScope::from_env(
                std::env::var("EVPOLY_MM_TOXIC_REENTRY_CANCEL_SCOPE").ok(),
            ),
            reward_stub_enable: env_bool("EVPOLY_MM_REWARD_STUB_ENABLE", true),
            reward_stub_levels: env_usize("EVPOLY_MM_REWARD_STUB_LEVELS", 2).clamp(0, 6),
            reward_stub_size_mult: env_f64("EVPOLY_MM_REWARD_STUB_SIZE_MULT", 0.35)
                .clamp(0.05, 1.0),
            reward_stub_min_net_edge_bps: env_f64("EVPOLY_MM_REWARD_STUB_MIN_NET_EDGE_BPS", -4.0)
                .clamp(-500.0, 500.0),
            fee_enabled_dynamic_check: env_bool("EVPOLY_MM_FEE_ENABLED_DYNAMIC_CHECK", true),
            action_budget_enable: env_bool("EVPOLY_MM_ACTION_BUDGET_ENABLE", true),
            action_window_sec: env_u64("EVPOLY_MM_ACTION_WINDOW_SEC", 60).clamp(10, 900),
            action_per_min_conserve: env_f64("EVPOLY_MM_ACTION_PER_MIN_CONSERVE", 30.0)
                .clamp(1.0, 10_000.0),
            action_per_min_survival: env_f64("EVPOLY_MM_ACTION_PER_MIN_SURVIVAL", 60.0)
                .clamp(1.0, 20_000.0),
            action_cancel_fill_ratio_conserve: env_f64(
                "EVPOLY_MM_ACTION_CANCEL_FILL_RATIO_CONSERVE",
                3.0,
            )
            .clamp(0.1, 10_000.0),
            action_cancel_fill_ratio_survival: env_f64(
                "EVPOLY_MM_ACTION_CANCEL_FILL_RATIO_SURVIVAL",
                6.0,
            )
            .clamp(0.1, 10_000.0),
            action_stale_strikes_survival: env_u32("EVPOLY_MM_ACTION_STALE_STRIKES_SURVIVAL", 3)
                .clamp(1, 100),
            pending_cancel_hold_ms: env_u64("EVPOLY_MM_PENDING_CANCEL_HOLD_MS", 400)
                .clamp(50, 30_000) as i64,
            requote_anchor_min_rest_ms: env_u64("EVPOLY_MM_REQUOTE_ANCHOR_MIN_REST_MS", 500)
                .clamp(50, 60_000) as i64,
            requote_outer_min_rest_ms: env_u64("EVPOLY_MM_REQUOTE_OUTER_MIN_REST_MS", 200)
                .clamp(50, 60_000) as i64,
            conserve_actions_enable: env_bool("EVPOLY_MM_CONSERVE_ACTIONS_ENABLE", true),
            conserve_max_levels: env_usize("EVPOLY_MM_CONSERVE_MAX_LEVELS", 1).clamp(1, 10),
            conserve_spread_mult_max: env_f64("EVPOLY_MM_CONSERVE_SPREAD_MULT_MAX", 1.35)
                .clamp(1.0, 10.0),
            conserve_size_mult_min: env_f64("EVPOLY_MM_CONSERVE_SIZE_MULT_MIN", 0.35)
                .clamp(0.05, 1.0),
            conserve_hold_ms: env_u64("EVPOLY_MM_CONSERVE_HOLD_MS", 10_000).clamp(500, 300_000)
                as i64,
            unwind_actions_enable: env_bool("EVPOLY_MM_UNWIND_ACTIONS_ENABLE", true),
            unwind_require_markout: env_bool("EVPOLY_MM_UNWIND_REQUIRE_MARKOUT", true),
            unwind_markout_bps: env_f64("EVPOLY_MM_UNWIND_MARKOUT_BPS", -2.0).clamp(-500.0, 500.0),
            unwind_min_inventory_ratio: env_f64("EVPOLY_MM_UNWIND_MIN_INVENTORY_RATIO", 0.80)
                .clamp(0.05, 1.0),
            markout_dynamic_enable: env_bool_with_alias(
                "EVPOLY_MM_MARKOUT_DYNAMIC_ENABLE",
                "EVPOLY_MM_MARKOUT_DYNAMIC_GATE_ENABLE",
                false,
            ),
            markout_dynamic_k_gate: env_f64_with_alias(
                "EVPOLY_MM_MARKOUT_DYNAMIC_K_GATE",
                "EVPOLY_MM_MARKOUT_DYNAMIC_GATE_K",
                1.5,
            )
            .clamp(0.1, 20.0),
            markout_dynamic_k_recover: env_f64("EVPOLY_MM_MARKOUT_DYNAMIC_K_RECOVER", 0.8)
                .clamp(0.05, 20.0),
        }
    }

    pub fn mode_enabled(&self, mode: MmMode) -> bool {
        match mode {
            MmMode::QuietStack => self.mode_enable_quiet_stack,
            MmMode::TailBiased => self.mode_enable_tail_biased,
            MmMode::Spike => self.mode_enable_spike,
            MmMode::SportsPregame => self.mode_enable_sports_pregame,
        }
    }
}

impl MmModeTarget {
    pub fn new(symbol: &str, timeframe: Timeframe) -> Self {
        Self {
            symbol: symbol.trim().to_ascii_uppercase(),
            timeframe,
        }
    }
}

fn parse_single_market_selector(raw: &str) -> Option<(String, Option<String>)> {
    let mut value = raw.trim().trim_matches('/').to_string();
    if value.is_empty() {
        return None;
    }
    if let Some((_, suffix)) = value.split_once("polymarket.com/event/") {
        value = suffix.trim().trim_matches('/').to_string();
    } else if let Some((_, suffix)) = value.split_once("/event/") {
        value = suffix.trim().trim_matches('/').to_string();
    }
    if let Some(suffix) = value.strip_prefix("event/") {
        value = suffix.trim().trim_matches('/').to_string();
    }
    let parts = value
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [single] => Some((single.to_string(), None)),
        [event_slug, market_slug, ..] => {
            Some((event_slug.to_string(), Some(market_slug.to_string())))
        }
        _ => None,
    }
}

fn parse_single_market_selector_list_env(name: &str) -> Vec<MmSingleMarketSelector> {
    let Some(raw) = env_string(name) else {
        return Vec::new();
    };
    raw.split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .filter_map(parse_single_market_selector)
        .map(|(lookup_slug, match_slug)| MmSingleMarketSelector {
            lookup_slug,
            match_slug,
        })
        .collect()
}

fn parse_string_list_env(name: &str) -> Vec<String> {
    let Some(raw) = env_string(name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for value in raw.split(',').map(str::trim).filter(|v| !v.is_empty()) {
        let normalized = value.to_ascii_lowercase();
        if out.iter().any(|existing| existing == &normalized) {
            continue;
        }
        out.push(normalized);
    }
    out
}

fn parse_competition_level_allowlist_env(name: &str) -> Vec<String> {
    const ALL_LEVELS: [&str; 4] = ["low", "medium", "high", "unknown"];
    const DEFAULT_LEVELS: [&str; 2] = ["low", "medium"];
    let parsed = parse_string_list_env(name);
    if parsed.is_empty() {
        return DEFAULT_LEVELS.iter().map(|v| (*v).to_string()).collect();
    }

    let mut out = Vec::new();
    for value in parsed {
        match value.as_str() {
            "all" | "*" => {
                return ALL_LEVELS.iter().map(|v| (*v).to_string()).collect();
            }
            "low" | "medium" | "high" | "unknown" => {
                if out.iter().any(|existing| existing == &value) {
                    continue;
                }
                out.push(value);
            }
            _ => {}
        }
    }

    if out.is_empty() {
        DEFAULT_LEVELS.iter().map(|v| (*v).to_string()).collect()
    } else {
        out
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

fn env_bool_with_alias(name: &str, alias: &str, default: bool) -> bool {
    if std::env::var(name).is_ok() {
        return env_bool(name, default);
    }
    if std::env::var(alias).is_ok() {
        return env_bool(alias, default);
    }
    default
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(default)
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

fn env_f64_with_alias(name: &str, alias: &str, default: f64) -> f64 {
    if std::env::var(name).is_ok() {
        return env_f64(name, default);
    }
    if std::env::var(alias).is_ok() {
        return env_f64(alias, default);
    }
    default
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_market_mode_from_env_value() {
        assert_eq!(MmMarketMode::from_env(None), MmMarketMode::Auto);
        assert_eq!(
            MmMarketMode::from_env(Some("target".to_string())),
            MmMarketMode::Auto
        );
        assert_eq!(
            MmMarketMode::from_env(Some("auto".to_string())),
            MmMarketMode::Auto
        );
        assert_eq!(
            MmMarketMode::from_env(Some("hybrid".to_string())),
            MmMarketMode::Hybrid
        );
        assert_eq!(
            MmMarketMode::from_env(Some("unknown".to_string())),
            MmMarketMode::Auto
        );
    }

    #[test]
    fn parse_enqueue_mode_from_env_value() {
        assert_eq!(
            MmEnqueueMode::from_env(Some("await".to_string())),
            MmEnqueueMode::Await
        );
        assert_eq!(
            MmEnqueueMode::from_env(Some("bounded".to_string())),
            MmEnqueueMode::Bounded
        );
        assert_eq!(
            MmEnqueueMode::from_env(Some("try_send".to_string())),
            MmEnqueueMode::TrySend
        );
        assert_eq!(
            MmEnqueueMode::from_env(Some("try".to_string())),
            MmEnqueueMode::TrySend
        );
        assert_eq!(
            MmEnqueueMode::from_env(Some("unknown".to_string())),
            MmEnqueueMode::Await
        );
    }

    #[test]
    fn parse_off_target_cancel_mode_from_env_value() {
        assert_eq!(
            MmOffTargetCancelMode::from_env(Some("disabled".to_string())),
            MmOffTargetCancelMode::Disabled
        );
        assert_eq!(
            MmOffTargetCancelMode::from_env(Some("safe".to_string())),
            MmOffTargetCancelMode::Safe
        );
        assert_eq!(
            MmOffTargetCancelMode::from_env(Some("aggressive".to_string())),
            MmOffTargetCancelMode::Aggressive
        );
        assert_eq!(
            MmOffTargetCancelMode::from_env(Some("unknown".to_string())),
            MmOffTargetCancelMode::Disabled
        );
    }

    #[test]
    fn parse_toxic_reentry_cancel_scope_from_env_value() {
        assert_eq!(
            MmToxicReentryCancelScope::from_env(Some("threatened_side".to_string())),
            MmToxicReentryCancelScope::ThreatenedSide
        );
        assert_eq!(
            MmToxicReentryCancelScope::from_env(Some("both_sides".to_string())),
            MmToxicReentryCancelScope::BothSides
        );
        assert_eq!(
            MmToxicReentryCancelScope::from_env(Some("market".to_string())),
            MmToxicReentryCancelScope::BothSides
        );
        assert_eq!(
            MmToxicReentryCancelScope::from_env(Some("unknown".to_string())),
            MmToxicReentryCancelScope::ThreatenedSide
        );
    }
}
