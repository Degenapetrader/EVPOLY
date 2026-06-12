pub mod sports_live_guard;

const MM_SPORT_RATIO_PAUSE_SEC_HARDCODED: u64 = 180;
const MM_SPORT_FRESH_SCAN_MARKETS_PER_TICK_HARDCODED: usize = 200;
const MM_SPORT_ORDER_SUBMIT_CONCURRENCY_HARDCODED: usize = 4;
const MM_SPORT_ACTIVE_SPORT_MARKET_CAP_HARDCODED: usize = 600;
const MM_SPORT_DISCOVERY_FULL_REFRESH_SEC_HARDCODED: u64 = 3_600;
const MM_SPORT_DISCOVERY_DELTA_REFRESH_SEC_HARDCODED: u64 = 600;
const MM_SPORT_DISCOVERY_DELTA_PAGE_BUDGET_HARDCODED: u32 = 8;
const MM_SPORT_DISCOVERY_DETAIL_CAP_HARDCODED: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmSportQuoteSizeMode {
    Multiple,
    DepthRatio,
}

impl MmSportQuoteSizeMode {
    fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "depth_ratio".to_string())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmSportEntryPriceMode {
    Passive,
    BestBid,
}

impl MmSportEntryPriceMode {
    fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "best_bid".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "best_bid" | "best-bid" | "bestbid" | "top_bid" | "top-bid" => Self::BestBid,
            _ => Self::Passive,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passive => "passive",
            Self::BestBid => "best_bid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmSportDiscoveryRoute {
    Sports,
    NonSports,
    Dual,
}

impl MmSportDiscoveryRoute {
    fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "sports".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "nonsports" | "non_sports" | "non-sports" | "non_sport" | "non-sport" => {
                Self::NonSports
            }
            "dual" | "both" => Self::Dual,
            _ => Self::Sports,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sports => "sports",
            Self::NonSports => "nonsports",
            Self::Dual => "dual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmSportExitMode {
    Normal,
    Aggressive,
    NoExit,
}

impl MmSportExitMode {
    fn from_env(raw: Option<String>) -> Self {
        match raw
            .unwrap_or_else(|| "normal".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "aggressive" => Self::Aggressive,
            "no_exit" | "no-exit" | "hold" => Self::NoExit,
            _ => Self::Normal,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Aggressive => "aggressive",
            Self::NoExit => "no_exit",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MmSportSizingProfile {
    pub quote_size_mode: MmSportQuoteSizeMode,
    pub quote_size_mult: f64,
    pub multiple_collateral_cap_mult: f64,
    pub depth_ratio_collateral_cap_mult: f64,
    pub max_share_ratio: f64,
    pub max_quote_shares: f64,
    pub min_top_depth_usd: f64,
}

#[derive(Debug, Clone)]
pub struct MmSportConfig {
    pub enable: bool,
    pub hard_disable: bool,
    pub poll_ms: u64,
    pub event_driven_enable: bool,
    pub event_fallback_poll_ms: u64,
    pub event_min_scan_ms: u64,
    pub ws_stale_ms: i64,
    pub fifo_ws_gap_cancel_ms: i64,
    pub discovery_refresh_sec: u64,
    pub discovery_delta_refresh_sec: u64,
    pub discovery_delta_page_budget: u32,
    pub discovery_detail_cap: usize,
    pub rewards_page_budget: u32,
    pub min_reward_rate_per_day: f64,
    pub discovery_route: MmSportDiscoveryRoute,
    pub quote_size_mode: MmSportQuoteSizeMode,
    pub nonsport_quote_size_mode: MmSportQuoteSizeMode,
    pub entry_price_mode: MmSportEntryPriceMode,
    pub exit_mode: MmSportExitMode,
    pub quote_size_mult: f64,
    pub nonsport_quote_size_mult: f64,
    pub multiple_collateral_cap_mult: f64,
    pub nonsport_multiple_collateral_cap_mult: f64,
    pub depth_ratio_collateral_cap_mult: f64,
    pub nonsport_depth_ratio_collateral_cap_mult: f64,
    pub market_allowlist_keywords: Vec<String>,
    pub market_blacklist_keywords: Vec<String>,
    pub allowed_sport_league_codes: Vec<String>,
    pub blocked_sport_league_codes: Vec<String>,
    pub blocked_competition_levels: Vec<String>,
    pub reward_min_shares_cap: f64,
    pub max_share_ratio: f64,
    pub nonsport_max_share_ratio: f64,
    pub max_quote_shares: f64,
    pub nonsport_max_quote_shares: f64,
    pub fifo_max_share_ratio: f64,
    pub min_top_depth_usd: f64,
    pub nonsport_min_top_depth_usd: f64,
    pub nonsport_end_exit_start_sec: u64,
    pub nonsport_entry_schedule_enabled: bool,
    pub nonsport_entry_schedule_days_utc: Vec<u8>,
    pub nonsport_entry_schedule_start_minute_utc: u16,
    pub nonsport_entry_schedule_end_minute_utc: u16,
    pub min_entry_top_bid_price: f64,
    pub allow_sponsored_rewards: bool,
    pub sponsored_reward_min_share: f64,
    pub inventory_exit_start_sec: u64,
    pub inventory_exit_max_loss_cents: f64,
    pub pause_after_fill_sec: u64,
    pub no_exit_side_pause_sec: u64,
    pub bust_window_ms: i64,
    pub bust_shares_1s: f64,
    pub bust_pause_min_sec: u64,
    pub bust_pause_max_sec: u64,
    pub ratio_breach_cancel_cooldown_ms: i64,
    pub ratio_pause_sec: u64,
    pub reprice_min_interval_ms: i64,
    pub quote_expiry_min_sec: u64,
    pub quote_expiry_max_sec: u64,
    pub quote_cooldown_min_sec: u64,
    pub quote_cooldown_max_sec: u64,
    pub quote_hold_min_sec: u64,
    pub quote_hold_max_sec: u64,
    pub size_requote_delta_pct: f64,
    pub fresh_scan_markets_per_tick: usize,
    pub order_submit_concurrency: usize,
    pub verbose_budget_events: bool,
    pub allowance_refresh_sec: u64,
    pub require_reward_eligible: bool,
    pub pregame_only: bool,
    pub match_only: bool,
    pub post_only: bool,
    pub max_markets: usize,
    pub active_sport_market_cap: usize,
    pub active_nonsport_market_cap: usize,
    pub polymarket_live_guard_enable: bool,
    pub polymarket_live_guard_ws_enable: bool,
    pub polymarket_live_guard_ws_stale_ms: i64,
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
            env_u64("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC", 65).clamp(61, 3_600);
        let quote_expiry_max_sec =
            env_u64("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC", 185).clamp(quote_expiry_min_sec, 7_200);
        let quote_size_mode =
            MmSportQuoteSizeMode::from_env(std::env::var("EVPOLY_MM_SPORT_QUOTE_SIZE_MODE").ok());
        let entry_price_mode =
            MmSportEntryPriceMode::from_env(std::env::var("EVPOLY_MM_SPORT_ENTRY_PRICE_MODE").ok());
        let quote_size_mult = env_f64("EVPOLY_MM_SPORT_QUOTE_SIZE_MULT", 1.2).clamp(0.1, 20.0);
        let multiple_collateral_cap_mult = 0.45;
        let depth_ratio_collateral_cap_mult = 0.45;
        let max_share_ratio = env_f64("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", 0.20).clamp(0.01, 0.99);
        let nonsport_max_share_ratio =
            env_f64("EVPOLY_MM_SPORT_NONSPORT_MAX_SHARE_RATIO", max_share_ratio).clamp(0.01, 0.99);
        let max_quote_shares = env_f64("EVPOLY_MM_SPORT_MAX_QUOTE_SHARES", 1_000.0).max(0.0);
        let nonsport_max_quote_shares =
            env_f64("EVPOLY_MM_SPORT_NONSPORT_MAX_QUOTE_SHARES", 200.0).max(0.0);
        let fifo_max_share_ratio =
            env_f64("EVPOLY_MM_SPORT_FIFO_MAX_SHARE_RATIO", 0.5).clamp(0.01, 0.99);
        let nonsport_entry_schedule_start_minute_utc = env_u64(
            "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_START_MINUTE_UTC",
            13 * 60,
        )
        .min(1_439) as u16;
        let nonsport_entry_schedule_end_minute_utc = env_u64(
            "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_END_MINUTE_UTC",
            4 * 60,
        )
        .min(1_439) as u16;
        let quote_cooldown_min_sec =
            env_u64("EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MIN_SEC", 10).clamp(0, 3_600);
        let quote_cooldown_max_sec = env_u64("EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MAX_SEC", 60)
            .clamp(quote_cooldown_min_sec, 7_200);
        let quote_hold_min_sec = env_u64("EVPOLY_MM_SPORT_QUOTE_HOLD_MIN_SEC", 20).clamp(0, 3_600);
        let quote_hold_max_sec =
            env_u64("EVPOLY_MM_SPORT_QUOTE_HOLD_MAX_SEC", 45).clamp(quote_hold_min_sec, 7_200);
        let discovery_route =
            MmSportDiscoveryRoute::from_env(std::env::var("EVPOLY_MM_SPORT_DISCOVERY_ROUTE").ok());
        let default_active_nonsport_cap = match discovery_route {
            MmSportDiscoveryRoute::Sports => 0,
            MmSportDiscoveryRoute::NonSports => 100,
            MmSportDiscoveryRoute::Dual => 50,
        };
        let min_top_depth_usd = env_f64("EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD", 1_100.0).max(0.0);
        Self {
            enable: env_bool("EVPOLY_STRATEGY_MM_SPORT_ENABLE", false),
            hard_disable: env_bool("EVPOLY_MM_SPORT_HARD_DISABLE", false),
            poll_ms: env_u64("EVPOLY_MM_SPORT_POLL_MS", 250).clamp(50, 30_000),
            event_driven_enable: env_bool("EVPOLY_MM_SPORT_EVENT_DRIVEN_ENABLE", true),
            event_fallback_poll_ms: 1_000,
            event_min_scan_ms: env_u64("EVPOLY_MM_SPORT_EVENT_MIN_SCAN_MS", 1_000)
                .clamp(250, 30_000),
            ws_stale_ms: env_u64("EVPOLY_MM_SPORT_WS_STALE_MS", 2_500).clamp(250, 30_000) as i64,
            fifo_ws_gap_cancel_ms: env_u64("EVPOLY_MM_SPORT_FIFO_WS_GAP_CANCEL_MS", 5_000)
                .clamp(500, 60_000) as i64,
            discovery_refresh_sec: MM_SPORT_DISCOVERY_FULL_REFRESH_SEC_HARDCODED,
            discovery_delta_refresh_sec: MM_SPORT_DISCOVERY_DELTA_REFRESH_SEC_HARDCODED,
            discovery_delta_page_budget: MM_SPORT_DISCOVERY_DELTA_PAGE_BUDGET_HARDCODED,
            discovery_detail_cap: MM_SPORT_DISCOVERY_DETAIL_CAP_HARDCODED,
            rewards_page_budget: env_u32("EVPOLY_MM_SPORT_REWARDS_PAGE_BUDGET", 8).clamp(1, 200),
            min_reward_rate_per_day: env_f64("EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY", 5.0)
                .max(0.0),
            discovery_route,
            quote_size_mode,
            nonsport_quote_size_mode: MmSportQuoteSizeMode::from_env(
                std::env::var("EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MODE")
                    .ok()
                    .or_else(|| Some(quote_size_mode.as_str().to_string())),
            ),
            entry_price_mode,
            exit_mode: MmSportExitMode::from_env(std::env::var("EVPOLY_MM_SPORT_EXIT_MODE").ok()),
            quote_size_mult,
            nonsport_quote_size_mult: env_f64(
                "EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MULT",
                quote_size_mult,
            )
            .clamp(0.1, 20.0),
            multiple_collateral_cap_mult,
            nonsport_multiple_collateral_cap_mult: multiple_collateral_cap_mult,
            depth_ratio_collateral_cap_mult,
            nonsport_depth_ratio_collateral_cap_mult: depth_ratio_collateral_cap_mult,
            market_allowlist_keywords: parse_string_list_env(
                "EVPOLY_MM_SPORT_MARKET_ALLOWLIST_KEYWORDS",
            ),
            market_blacklist_keywords: parse_string_list_env(
                "EVPOLY_MM_SPORT_MARKET_BLACKLIST_KEYWORDS",
            ),
            allowed_sport_league_codes: parse_mm_sport_sport_league_codes_env(
                "EVPOLY_MM_SPORT_ALLOWED_SPORT_LEAGUE_CODES",
            ),
            blocked_sport_league_codes: parse_mm_sport_sport_league_codes_env(
                "EVPOLY_MM_SPORT_BLOCKED_SPORT_LEAGUE_CODES",
            ),
            blocked_competition_levels: parse_mm_sport_blocked_competition_levels_env(
                "EVPOLY_MM_SPORT_BLOCKED_COMPETITION_LEVELS",
            ),
            reward_min_shares_cap: env_f64("EVPOLY_MM_SPORT_REWARD_MIN_SHARES_CAP", 0.0).max(0.0),
            max_share_ratio,
            nonsport_max_share_ratio,
            max_quote_shares,
            nonsport_max_quote_shares,
            fifo_max_share_ratio,
            min_top_depth_usd,
            nonsport_min_top_depth_usd: env_f64(
                "EVPOLY_MM_SPORT_NONSPORT_MIN_TOP_DEPTH_USD",
                min_top_depth_usd,
            )
            .max(0.0),
            nonsport_end_exit_start_sec: env_u64(
                "EVPOLY_MM_SPORT_NONSPORT_END_EXIT_START_SEC",
                172_800,
            )
            .clamp(0, 2_592_000),
            nonsport_entry_schedule_enabled: env_bool(
                "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_ENABLE",
                false,
            ),
            nonsport_entry_schedule_days_utc: parse_mm_sport_weekdays_env(
                "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_DAYS_UTC",
            ),
            nonsport_entry_schedule_start_minute_utc,
            nonsport_entry_schedule_end_minute_utc,
            min_entry_top_bid_price: env_f64("EVPOLY_MM_SPORT_MIN_ENTRY_TOP_BID_PRICE", 0.05)
                .clamp(0.0, 0.99),
            allow_sponsored_rewards: env_bool("EVPOLY_MM_SPORT_ALLOW_SPONSORED_REWARDS", true),
            sponsored_reward_min_share: env_f64("EVPOLY_MM_SPORT_SPONSORED_REWARD_MIN_SHARE", 0.50)
                .clamp(0.0, 1.0),
            inventory_exit_start_sec: env_u64("EVPOLY_MM_SPORT_INVENTORY_EXIT_START_SEC", 3_600)
                .clamp(300, 172_800),
            inventory_exit_max_loss_cents: env_f64(
                "EVPOLY_MM_SPORT_INVENTORY_EXIT_MAX_LOSS_CENTS",
                10.0,
            )
            .clamp(0.0, 99.0),
            pause_after_fill_sec: env_u64("EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC", 3_600)
                .clamp(60, 86_400),
            no_exit_side_pause_sec: env_u64("EVPOLY_MM_SPORT_NO_EXIT_SIDE_PAUSE_SEC", 3_600)
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
            ratio_pause_sec: MM_SPORT_RATIO_PAUSE_SEC_HARDCODED,
            reprice_min_interval_ms: env_u64("EVPOLY_MM_SPORT_REPRICE_MIN_INTERVAL_MS", 600)
                .clamp(50, 60_000) as i64,
            quote_expiry_min_sec,
            quote_expiry_max_sec,
            quote_cooldown_min_sec,
            quote_cooldown_max_sec,
            quote_hold_min_sec,
            quote_hold_max_sec,
            size_requote_delta_pct: env_f64("EVPOLY_MM_SPORT_SIZE_REQUOTE_DELTA_PCT", 0.20)
                .clamp(0.0, 1.0),
            fresh_scan_markets_per_tick: MM_SPORT_FRESH_SCAN_MARKETS_PER_TICK_HARDCODED,
            order_submit_concurrency: MM_SPORT_ORDER_SUBMIT_CONCURRENCY_HARDCODED,
            verbose_budget_events: env_bool("EVPOLY_MM_SPORT_VERBOSE_BUDGET_EVENTS", false),
            allowance_refresh_sec: env_u64("EVPOLY_MM_SPORT_ALLOWANCE_REFRESH_SEC", 60)
                .clamp(15, 3_600),
            require_reward_eligible: env_bool("EVPOLY_MM_SPORT_REQUIRE_REWARD_ELIGIBLE", true),
            pregame_only: env_bool("EVPOLY_MM_SPORT_PREGAME_ONLY", true),
            match_only: env_bool("EVPOLY_MM_SPORT_MATCH_ONLY", true),
            post_only: env_bool("EVPOLY_MM_SPORT_POST_ONLY", true),
            max_markets: env_usize("EVPOLY_MM_SPORT_MAX_MARKETS", 0),
            active_sport_market_cap: MM_SPORT_ACTIVE_SPORT_MARKET_CAP_HARDCODED,
            active_nonsport_market_cap: env_usize(
                "EVPOLY_MM_SPORT_ACTIVE_NONSPORT_MARKET_CAP",
                default_active_nonsport_cap,
            )
            .min(1_000),
            polymarket_live_guard_enable: env_bool(
                "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_ENABLE",
                true,
            ),
            polymarket_live_guard_ws_enable: env_bool(
                "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_ENABLE",
                true,
            ),
            polymarket_live_guard_ws_stale_ms: env_u64(
                "EVPOLY_MM_SPORT_POLYMARKET_LIVE_GUARD_WS_STALE_MS",
                600_000,
            )
            .clamp(30_000, 3_600_000) as i64,
        }
    }

    pub fn sizing_for_market(&self, is_sports_market: bool) -> MmSportSizingProfile {
        if is_sports_market {
            MmSportSizingProfile {
                quote_size_mode: self.quote_size_mode,
                quote_size_mult: self.quote_size_mult,
                multiple_collateral_cap_mult: self.multiple_collateral_cap_mult,
                depth_ratio_collateral_cap_mult: self.depth_ratio_collateral_cap_mult,
                max_share_ratio: self.max_share_ratio,
                max_quote_shares: self.max_quote_shares,
                min_top_depth_usd: self.min_top_depth_usd,
            }
        } else {
            MmSportSizingProfile {
                quote_size_mode: self.nonsport_quote_size_mode,
                quote_size_mult: self.nonsport_quote_size_mult,
                multiple_collateral_cap_mult: self.nonsport_multiple_collateral_cap_mult,
                depth_ratio_collateral_cap_mult: self.nonsport_depth_ratio_collateral_cap_mult,
                max_share_ratio: self.nonsport_max_share_ratio,
                max_quote_shares: self.nonsport_max_quote_shares,
                min_top_depth_usd: self.nonsport_min_top_depth_usd,
            }
        }
    }

    pub fn fifo_ratio_floor(&self) -> f64 {
        0.01
    }

    pub fn effective_fifo_max_share_ratio(&self) -> f64 {
        self.fifo_max_share_ratio.clamp(0.01, 0.99)
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

fn parse_string_list_env(key: &str) -> Vec<String> {
    let Some(raw) = std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let normalized = value.to_ascii_lowercase();
        if out.iter().any(|existing| existing == &normalized) {
            continue;
        }
        out.push(normalized);
    }
    out
}

fn parse_string_list_from_allowed_env(key: &str, allowed: &[&str]) -> Vec<String> {
    let parsed = parse_string_list_env(key);
    if parsed.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for allowed_value in allowed {
        if parsed.iter().any(|value| value == allowed_value) {
            out.push((*allowed_value).to_string());
        }
    }
    out
}

fn parse_mm_sport_sport_league_codes_env(key: &str) -> Vec<String> {
    const ALLOWED: [&str; 25] = [
        "american_football",
        "basketball",
        "baseball",
        "hockey",
        "soccer",
        "tennis",
        "golf",
        "mma",
        "motorsport",
        "nfl",
        "ncaafb",
        "nba",
        "wnba",
        "ncaamb",
        "ncaawb",
        "mlb",
        "nhl",
        "epl",
        "uefa_champions_league",
        "mls",
        "atp",
        "wta",
        "pga_tour",
        "ufc",
        "f1",
    ];
    parse_string_list_from_allowed_env(key, &ALLOWED)
}

fn parse_mm_sport_blocked_competition_levels_env(key: &str) -> Vec<String> {
    const ALLOWED: [&str; 4] = ["low", "medium", "high", "unknown"];
    parse_string_list_from_allowed_env(key, &ALLOWED)
}

fn parse_mm_sport_weekdays_env(key: &str) -> Vec<u8> {
    let Some(raw) = std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return vec![1, 2, 3, 4, 5];
    };
    let mut out = Vec::new();
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let weekday = match value.to_ascii_lowercase().as_str() {
            "none" => return Vec::new(),
            "all" | "*" => return vec![1, 2, 3, 4, 5, 6, 7],
            "mon" | "monday" | "1" => 1,
            "tue" | "tues" | "tuesday" | "2" => 2,
            "wed" | "wednesday" | "3" => 3,
            "thu" | "thur" | "thurs" | "thursday" | "4" => 4,
            "fri" | "friday" | "5" => 5,
            "sat" | "saturday" | "6" => 6,
            "sun" | "sunday" | "0" | "7" => 7,
            _ => continue,
        };
        if !out.contains(&weekday) {
            out.push(weekday);
        }
    }
    out
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| parse_nonnegative_integerish(v.trim()))
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    env_u64(key, u64::from(default)).min(u64::from(u32::MAX)) as u32
}

fn env_usize(key: &str, default: usize) -> usize {
    env_u64(key, default as u64).min(usize::MAX as u64) as usize
}

fn parse_nonnegative_integerish(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().or_else(|| {
        let parsed = value.parse::<f64>().ok()?;
        if parsed.is_finite() && parsed >= 0.0 {
            Some(parsed.round() as u64)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn mm_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_mm_env<F: FnOnce()>(updates: &[(&str, Option<&str>)], f: F) {
        let _guard = mm_env_lock().lock().expect("mm env lock poisoned");
        let mut previous: Vec<(&str, Option<String>)> = Vec::with_capacity(updates.len());
        for (name, value) in updates {
            previous.push((*name, std::env::var(name).ok()));
            unsafe {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
        f();
        for (name, value) in previous {
            unsafe {
                match value {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn parse_mm_sport_discovery_route_from_env_value() {
        assert_eq!(
            MmSportDiscoveryRoute::from_env(None),
            MmSportDiscoveryRoute::Sports
        );
        assert_eq!(
            MmSportDiscoveryRoute::from_env(Some("nonsports".to_string())),
            MmSportDiscoveryRoute::NonSports
        );
        assert_eq!(
            MmSportDiscoveryRoute::from_env(Some("non-sport".to_string())),
            MmSportDiscoveryRoute::NonSports
        );
        assert_eq!(
            MmSportDiscoveryRoute::from_env(Some("dual".to_string())),
            MmSportDiscoveryRoute::Dual
        );
        assert_eq!(
            MmSportDiscoveryRoute::from_env(Some("unknown".to_string())),
            MmSportDiscoveryRoute::Sports
        );
    }

    #[test]
    fn parse_mm_sport_exit_mode_from_env_value() {
        assert_eq!(MmSportExitMode::from_env(None), MmSportExitMode::Normal);
        assert_eq!(
            MmSportExitMode::from_env(Some("normal".to_string())),
            MmSportExitMode::Normal
        );
        assert_eq!(
            MmSportExitMode::from_env(Some("aggressive".to_string())),
            MmSportExitMode::Aggressive
        );
        assert_eq!(
            MmSportExitMode::from_env(Some("no_exit".to_string())),
            MmSportExitMode::NoExit
        );
        assert_eq!(
            MmSportExitMode::from_env(Some("unknown".to_string())),
            MmSportExitMode::Normal
        );
    }

    #[test]
    fn parse_mm_sport_entry_price_mode_from_env_value() {
        assert_eq!(
            MmSportEntryPriceMode::from_env(None),
            MmSportEntryPriceMode::BestBid
        );
        assert_eq!(
            MmSportEntryPriceMode::from_env(Some("passive".to_string())),
            MmSportEntryPriceMode::Passive
        );
        assert_eq!(
            MmSportEntryPriceMode::from_env(Some("best_bid".to_string())),
            MmSportEntryPriceMode::BestBid
        );
        assert_eq!(
            MmSportEntryPriceMode::from_env(Some("top-bid".to_string())),
            MmSportEntryPriceMode::BestBid
        );
        assert_eq!(
            MmSportEntryPriceMode::from_env(Some("unknown".to_string())),
            MmSportEntryPriceMode::Passive
        );
    }

    #[test]
    fn mm_sport_config_defaults_to_current_runtime_controls() {
        with_mm_env(
            &[
                ("EVPOLY_MM_SPORT_ENTRY_PRICE_MODE", None),
                ("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", None),
                ("EVPOLY_MM_SPORT_NONSPORT_MAX_SHARE_RATIO", None),
                ("EVPOLY_MM_SPORT_MAX_QUOTE_SHARES", None),
                ("EVPOLY_MM_SPORT_NONSPORT_MAX_QUOTE_SHARES", None),
                ("EVPOLY_MM_SPORT_FIFO_MAX_SHARE_RATIO", None),
                ("EVPOLY_MM_SPORT_MIN_ENTRY_TOP_BID_PRICE", None),
                ("EVPOLY_MM_SPORT_INVENTORY_EXIT_START_SEC", None),
                ("EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC", None),
                ("EVPOLY_MM_SPORT_EXIT_MODE", None),
                ("EVPOLY_MM_SPORT_NO_EXIT_SIDE_PAUSE_SEC", None),
                ("EVPOLY_MM_SPORT_MATCH_ONLY", None),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                assert_eq!(cfg.entry_price_mode, MmSportEntryPriceMode::BestBid);
                assert!((cfg.max_share_ratio - 0.20).abs() < 1e-9);
                assert!((cfg.nonsport_max_share_ratio - 0.20).abs() < 1e-9);
                assert_eq!(cfg.max_quote_shares, 1_000.0);
                assert_eq!(cfg.nonsport_max_quote_shares, 200.0);
                assert!((cfg.fifo_max_share_ratio - 0.50).abs() < 1e-9);
                assert!((cfg.min_entry_top_bid_price - 0.05).abs() < 1e-9);
                assert_eq!(cfg.inventory_exit_start_sec, 3_600);
                assert_eq!(cfg.pause_after_fill_sec, 3_600);
                assert_eq!(cfg.exit_mode, MmSportExitMode::Normal);
                assert_eq!(cfg.no_exit_side_pause_sec, 3_600);
                assert!(cfg.match_only);
            },
        );
    }

    #[test]
    fn mm_sport_config_reads_exit_mode_and_side_pause_override() {
        with_mm_env(
            &[
                ("EVPOLY_MM_SPORT_EXIT_MODE", Some("no_exit")),
                ("EVPOLY_MM_SPORT_NO_EXIT_SIDE_PAUSE_SEC", Some("5400")),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                assert_eq!(cfg.exit_mode, MmSportExitMode::NoExit);
                assert_eq!(cfg.no_exit_side_pause_sec, 5_400);
            },
        );
    }

    #[test]
    fn mm_sport_config_accepts_whole_number_float_env_values() {
        with_mm_env(
            &[
                ("EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC", Some("600.0")),
                (
                    "EVPOLY_MM_SPORT_INVENTORY_EXIT_MAX_LOSS_CENTS",
                    Some("12.5"),
                ),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                assert_eq!(cfg.pause_after_fill_sec, 600);
                assert_eq!(cfg.quote_expiry_min_sec, 65);
                assert_eq!(cfg.quote_expiry_max_sec, 185);
                assert!((cfg.inventory_exit_max_loss_cents - 12.5).abs() < 1e-9);
            },
        );
    }

    #[test]
    fn mm_sport_config_reads_inventory_exit_start_override() {
        with_mm_env(
            &[("EVPOLY_MM_SPORT_INVENTORY_EXIT_START_SEC", Some("21600.0"))],
            || {
                let cfg = MmSportConfig::from_env();
                assert_eq!(cfg.inventory_exit_start_sec, 21_600);
            },
        );
    }

    #[test]
    fn mm_sport_config_reads_nonsport_entry_guards() {
        with_mm_env(
            &[
                (
                    "EVPOLY_MM_SPORT_NONSPORT_END_EXIT_START_SEC",
                    Some("86400.0"),
                ),
                (
                    "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_ENABLE",
                    Some("true"),
                ),
                (
                    "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_DAYS_UTC",
                    Some("mon,wed,7"),
                ),
                (
                    "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_START_MINUTE_UTC",
                    Some("780"),
                ),
                (
                    "EVPOLY_MM_SPORT_NONSPORT_ENTRY_SCHEDULE_END_MINUTE_UTC",
                    Some("240"),
                ),
                ("EVPOLY_MM_SPORT_MIN_ENTRY_TOP_BID_PRICE", Some("0.15")),
                ("EVPOLY_MM_SPORT_ALLOW_SPONSORED_REWARDS", Some("false")),
                ("EVPOLY_MM_SPORT_SPONSORED_REWARD_MIN_SHARE", Some("0.65")),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                assert_eq!(cfg.nonsport_end_exit_start_sec, 86_400);
                assert!(cfg.nonsport_entry_schedule_enabled);
                assert_eq!(cfg.nonsport_entry_schedule_days_utc, vec![1, 3, 7]);
                assert_eq!(cfg.nonsport_entry_schedule_start_minute_utc, 780);
                assert_eq!(cfg.nonsport_entry_schedule_end_minute_utc, 240);
                assert!((cfg.min_entry_top_bid_price - 0.15).abs() < 1e-9);
                assert!(!cfg.allow_sponsored_rewards);
                assert!((cfg.sponsored_reward_min_share - 0.65).abs() < 1e-9);
            },
        );
    }

    #[test]
    fn mm_sport_config_uses_hardcoded_runtime_caps_and_clamps_fifo_ratio() {
        with_mm_env(
            &[
                ("EVPOLY_MM_SPORT_DISCOVERY_ROUTE", Some("dual")),
                ("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", Some("0.10")),
                ("EVPOLY_MM_SPORT_NONSPORT_MAX_SHARE_RATIO", Some("0.05")),
                ("EVPOLY_MM_SPORT_MAX_QUOTE_SHARES", Some("1000")),
                ("EVPOLY_MM_SPORT_NONSPORT_MAX_QUOTE_SHARES", Some("250")),
                ("EVPOLY_MM_SPORT_FIFO_MAX_SHARE_RATIO", Some("0.09")),
                ("EVPOLY_MM_SPORT_ACTIVE_SPORT_MARKET_CAP", Some("44")),
                ("EVPOLY_MM_SPORT_ACTIVE_NONSPORT_MARKET_CAP", Some("33")),
                ("EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MIN_SEC", Some("12")),
                ("EVPOLY_MM_SPORT_QUOTE_COOLDOWN_MAX_SEC", Some("45")),
                ("EVPOLY_MM_SPORT_QUOTE_HOLD_MIN_SEC", Some("25")),
                ("EVPOLY_MM_SPORT_QUOTE_HOLD_MAX_SEC", Some("50")),
                ("EVPOLY_MM_SPORT_EVENT_MIN_SCAN_MS", Some("1500")),
                ("EVPOLY_MM_SPORT_SIZE_REQUOTE_DELTA_PCT", Some("0.2")),
                ("EVPOLY_MM_SPORT_FRESH_SCAN_MARKETS_PER_TICK", Some("24")),
                ("EVPOLY_MM_SPORT_ORDER_SUBMIT_CONCURRENCY", Some("1")),
                ("EVPOLY_MM_SPORT_RATIO_PAUSE_SEC", Some("900")),
                ("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC", Some("90")),
                ("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC", Some("180")),
                ("EVPOLY_MM_SPORT_VERBOSE_BUDGET_EVENTS", Some("true")),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                assert_eq!(cfg.active_sport_market_cap, 600);
                assert_eq!(cfg.active_nonsport_market_cap, 33);
                assert_eq!(cfg.discovery_refresh_sec, 3_600);
                assert_eq!(cfg.discovery_delta_refresh_sec, 600);
                assert_eq!(cfg.discovery_delta_page_budget, 8);
                assert_eq!(cfg.discovery_detail_cap, 1_000);
                assert_eq!(cfg.max_quote_shares, 1000.0);
                assert_eq!(cfg.nonsport_max_quote_shares, 250.0);
                assert_eq!(cfg.sizing_for_market(true).max_quote_shares, 1000.0);
                assert_eq!(cfg.sizing_for_market(false).max_quote_shares, 250.0);
                assert_eq!(cfg.quote_cooldown_min_sec, 12);
                assert_eq!(cfg.quote_cooldown_max_sec, 45);
                assert_eq!(cfg.quote_hold_min_sec, 25);
                assert_eq!(cfg.quote_hold_max_sec, 50);
                assert_eq!(cfg.event_min_scan_ms, 1_500);
                assert!((cfg.size_requote_delta_pct - 0.20).abs() < 1e-9);
                assert_eq!(cfg.fresh_scan_markets_per_tick, 200);
                assert_eq!(cfg.order_submit_concurrency, 4);
                assert_eq!(cfg.ratio_pause_sec, 180);
                assert_eq!(cfg.quote_expiry_min_sec, 90);
                assert_eq!(cfg.quote_expiry_max_sec, 180);
                assert!(cfg.verbose_budget_events);
                assert!((cfg.fifo_ratio_floor() - 0.01).abs() < 1e-9);
                assert!((cfg.effective_fifo_max_share_ratio() - 0.09).abs() < 1e-9);
            },
        );
    }

    #[test]
    fn mm_sport_fifo_ratio_is_independent_from_quote_ratio() {
        with_mm_env(
            &[
                ("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", Some("0.60")),
                ("EVPOLY_MM_SPORT_NONSPORT_MAX_SHARE_RATIO", Some("0.60")),
                ("EVPOLY_MM_SPORT_FIFO_MAX_SHARE_RATIO", Some("0.20")),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                assert!((cfg.fifo_max_share_ratio - 0.20).abs() < 1e-9);
                assert!((cfg.fifo_ratio_floor() - 0.01).abs() < 1e-9);
                assert!((cfg.effective_fifo_max_share_ratio() - 0.20).abs() < 1e-9);
            },
        );
    }

    #[test]
    fn mm_sport_sizing_profile_falls_back_and_overrides_nonsport() {
        with_mm_env(
            &[
                ("EVPOLY_MM_SPORT_QUOTE_SIZE_MODE", Some("multiple")),
                ("EVPOLY_MM_SPORT_QUOTE_SIZE_MULT", Some("2.0")),
                ("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", Some("0.10")),
                ("EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD", Some("1200")),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                let sport = cfg.sizing_for_market(true);
                let nonsport = cfg.sizing_for_market(false);
                assert_eq!(sport.quote_size_mode, MmSportQuoteSizeMode::Multiple);
                assert!((nonsport.quote_size_mult - sport.quote_size_mult).abs() < 1e-9);
                assert!(
                    (nonsport.multiple_collateral_cap_mult - sport.multiple_collateral_cap_mult)
                        .abs()
                        < 1e-9
                );
                assert!((sport.multiple_collateral_cap_mult - 0.45).abs() < 1e-9);
                assert!((nonsport.multiple_collateral_cap_mult - 0.45).abs() < 1e-9);
                assert!((sport.depth_ratio_collateral_cap_mult - 0.45).abs() < 1e-9);
                assert!((nonsport.depth_ratio_collateral_cap_mult - 0.45).abs() < 1e-9);
                assert!((nonsport.max_share_ratio - sport.max_share_ratio).abs() < 1e-9);
                assert!((nonsport.min_top_depth_usd - sport.min_top_depth_usd).abs() < 1e-9);
            },
        );

        with_mm_env(
            &[
                ("EVPOLY_MM_SPORT_QUOTE_SIZE_MODE", Some("multiple")),
                ("EVPOLY_MM_SPORT_QUOTE_SIZE_MULT", Some("2.0")),
                ("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", Some("0.10")),
                ("EVPOLY_MM_SPORT_MAX_QUOTE_SHARES", Some("1000")),
                (
                    "EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MODE",
                    Some("depth_ratio"),
                ),
                ("EVPOLY_MM_SPORT_NONSPORT_QUOTE_SIZE_MULT", Some("0.7")),
                ("EVPOLY_MM_SPORT_NONSPORT_MAX_SHARE_RATIO", Some("0.05")),
                ("EVPOLY_MM_SPORT_NONSPORT_MAX_QUOTE_SHARES", Some("250")),
                ("EVPOLY_MM_SPORT_NONSPORT_MIN_TOP_DEPTH_USD", Some("900")),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                let sport = cfg.sizing_for_market(true);
                let nonsport = cfg.sizing_for_market(false);
                assert_eq!(sport.quote_size_mode, MmSportQuoteSizeMode::Multiple);
                assert_eq!(nonsport.quote_size_mode, MmSportQuoteSizeMode::DepthRatio);
                assert!((nonsport.quote_size_mult - 0.7).abs() < 1e-9);
                assert!((nonsport.multiple_collateral_cap_mult - 0.45).abs() < 1e-9);
                assert!((nonsport.depth_ratio_collateral_cap_mult - 0.45).abs() < 1e-9);
                assert!((nonsport.max_share_ratio - 0.05).abs() < 1e-9);
                assert_eq!(sport.max_quote_shares, 1000.0);
                assert_eq!(nonsport.max_quote_shares, 250.0);
                assert!((nonsport.min_top_depth_usd - 900.0).abs() < 1e-9);
            },
        );
    }

    #[test]
    fn mm_sport_config_reads_post_only_override() {
        with_mm_env(&[("EVPOLY_MM_SPORT_POST_ONLY", Some("false"))], || {
            let cfg = MmSportConfig::from_env();
            assert!(!cfg.post_only);
        });
    }

    #[test]
    fn mm_sport_config_reads_route_filters_and_collateral_caps() {
        with_mm_env(
            &[
                ("EVPOLY_MM_SPORT_DISCOVERY_ROUTE", Some("dual")),
                (
                    "EVPOLY_MM_SPORT_ALLOWED_SPORT_LEAGUE_CODES",
                    Some("nba,mlb,bad"),
                ),
                (
                    "EVPOLY_MM_SPORT_BLOCKED_COMPETITION_LEVELS",
                    Some("high,unknown,bad"),
                ),
                (
                    "EVPOLY_MM_SPORT_MARKET_BLACKLIST_KEYWORDS",
                    Some("spread,total,spread"),
                ),
                ("EVPOLY_MM_SPORT_REWARD_MIN_SHARES_CAP", Some("500")),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                assert_eq!(cfg.discovery_route, MmSportDiscoveryRoute::Dual);
                assert!((cfg.multiple_collateral_cap_mult - 0.45).abs() < f64::EPSILON);
                assert!((cfg.depth_ratio_collateral_cap_mult - 0.45).abs() < f64::EPSILON);
                assert_eq!(cfg.allowed_sport_league_codes, vec!["nba", "mlb"]);
                assert_eq!(cfg.blocked_competition_levels, vec!["high", "unknown"]);
                assert_eq!(cfg.market_blacklist_keywords, vec!["spread", "total"]);
                assert_eq!(cfg.reward_min_shares_cap, 500.0);
            },
        );
    }

    #[test]
    fn mm_sport_collateral_caps_are_fixed() {
        with_mm_env(&[], || {
            let cfg = MmSportConfig::from_env();
            assert!((cfg.multiple_collateral_cap_mult - 0.45).abs() < f64::EPSILON);
            assert!((cfg.nonsport_multiple_collateral_cap_mult - 0.45).abs() < f64::EPSILON);
            assert!((cfg.depth_ratio_collateral_cap_mult - 0.45).abs() < f64::EPSILON);
            assert!((cfg.nonsport_depth_ratio_collateral_cap_mult - 0.45).abs() < f64::EPSILON);
        });
    }
}
