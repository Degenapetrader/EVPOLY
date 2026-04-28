pub mod sports_live_guard;

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
    pub discovery_route: MmSportDiscoveryRoute,
    pub quote_size_mode: MmSportQuoteSizeMode,
    pub exit_mode: MmSportExitMode,
    pub quote_size_mult: f64,
    pub pair_baseline_quote_size_mult: f64,
    pub multiple_collateral_cap_mult: f64,
    pub depth_ratio_collateral_cap_mult: f64,
    pub market_allowlist_keywords: Vec<String>,
    pub market_blacklist_keywords: Vec<String>,
    pub allowed_sport_league_codes: Vec<String>,
    pub blocked_sport_league_codes: Vec<String>,
    pub blocked_competition_levels: Vec<String>,
    pub reward_min_shares_cap: f64,
    pub max_share_ratio: f64,
    pub min_top_depth_usd: f64,
    pub inventory_exit_start_sec: u64,
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
    pub size_requote_delta_pct: f64,
    pub allowance_refresh_sec: u64,
    pub require_reward_eligible: bool,
    pub pregame_only: bool,
    pub match_only: bool,
    pub post_only: bool,
    pub max_markets: usize,
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
            env_u64("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC", 180).clamp(61, 3_600);
        let quote_expiry_max_sec =
            env_u64("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC", 300).clamp(quote_expiry_min_sec, 7_200);
        Self {
            enable: env_bool("EVPOLY_STRATEGY_MM_SPORT_ENABLE", false),
            hard_disable: env_bool("EVPOLY_MM_SPORT_HARD_DISABLE", false),
            poll_ms: env_u64("EVPOLY_MM_SPORT_POLL_MS", 250).clamp(50, 30_000),
            event_driven_enable: env_bool("EVPOLY_MM_SPORT_EVENT_DRIVEN_ENABLE", true),
            event_fallback_poll_ms: 1_000,
            ws_stale_ms: env_u64("EVPOLY_MM_SPORT_WS_STALE_MS", 2_500).clamp(250, 30_000) as i64,
            discovery_refresh_sec: 300,
            rewards_page_budget: env_u32("EVPOLY_MM_SPORT_REWARDS_PAGE_BUDGET", 8).clamp(1, 200),
            min_reward_rate_per_day: env_f64("EVPOLY_MM_SPORT_MIN_REWARD_RATE_PER_DAY", 300.0)
                .max(0.0),
            discovery_route: MmSportDiscoveryRoute::from_env(
                std::env::var("EVPOLY_MM_SPORT_DISCOVERY_ROUTE").ok(),
            ),
            quote_size_mode: MmSportQuoteSizeMode::from_env(
                std::env::var("EVPOLY_MM_SPORT_QUOTE_SIZE_MODE").ok(),
            ),
            exit_mode: MmSportExitMode::from_env(std::env::var("EVPOLY_MM_SPORT_EXIT_MODE").ok()),
            quote_size_mult: env_f64("EVPOLY_MM_SPORT_QUOTE_SIZE_MULT", 1.2).clamp(0.1, 20.0),
            pair_baseline_quote_size_mult: env_f64(
                "EVPOLY_MM_SPORT_PAIR_BASELINE_QUOTE_SIZE_MULT",
                1.2,
            )
            .clamp(0.1, 20.0),
            multiple_collateral_cap_mult: env_f64_with_alias(
                "EVPOLY_MM_SPORT_MULTIPLE_COLLATERAL_CAP_MULT",
                "EVPOLY_MM_SPORT_MULTIPLE_USDC_CAP_MULT",
                0.45,
            )
            .clamp(0.0, 1.0),
            depth_ratio_collateral_cap_mult: env_f64_with_alias(
                "EVPOLY_MM_SPORT_DEPTH_RATIO_COLLATERAL_CAP_MULT",
                "EVPOLY_MM_SPORT_DEPTH_RATIO_USDC_CAP_MULT",
                0.90,
            )
            .clamp(0.0, 1.0),
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
            max_share_ratio: env_f64("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", 0.05).clamp(0.01, 0.99),
            min_top_depth_usd: env_f64("EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD", 1_100.0).max(0.0),
            inventory_exit_start_sec: env_u64("EVPOLY_MM_SPORT_INVENTORY_EXIT_START_SEC", 28_800)
                .clamp(300, 172_800),
            pause_after_fill_sec: env_u64("EVPOLY_MM_SPORT_PAUSE_AFTER_FILL_SEC", 7_200)
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
            match_only: env_bool("EVPOLY_MM_SPORT_MATCH_ONLY", false),
            post_only: env_bool("EVPOLY_MM_SPORT_POST_ONLY", true),
            max_markets: env_usize("EVPOLY_MM_SPORT_MAX_MARKETS", 0),
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

fn env_f64_with_alias(key: &str, alias: &str, default: f64) -> f64 {
    if std::env::var(key).is_ok() {
        return env_f64(key, default);
    }
    if std::env::var(alias).is_ok() {
        return env_f64(alias, default);
    }
    default
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
    fn mm_sport_config_defaults_to_normal_exit_mode_and_one_hour_side_pause() {
        with_mm_env(
            &[
                ("EVPOLY_MM_SPORT_EXIT_MODE", None),
                ("EVPOLY_MM_SPORT_NO_EXIT_SIDE_PAUSE_SEC", None),
                ("EVPOLY_MM_SPORT_MATCH_ONLY", None),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                assert_eq!(cfg.exit_mode, MmSportExitMode::Normal);
                assert_eq!(cfg.no_exit_side_pause_sec, 3_600);
                assert!(!cfg.match_only);
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
                ("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MIN_SEC", Some("90.0")),
                ("EVPOLY_MM_SPORT_QUOTE_EXPIRY_MAX_SEC", Some("180.0")),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                assert_eq!(cfg.pause_after_fill_sec, 600);
                assert_eq!(cfg.quote_expiry_min_sec, 90);
                assert_eq!(cfg.quote_expiry_max_sec, 180);
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
                ("EVPOLY_MM_SPORT_MULTIPLE_COLLATERAL_CAP_MULT", Some("0.40")),
                (
                    "EVPOLY_MM_SPORT_DEPTH_RATIO_COLLATERAL_CAP_MULT",
                    Some("0.85"),
                ),
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
                assert!((cfg.multiple_collateral_cap_mult - 0.40).abs() < f64::EPSILON);
                assert!((cfg.depth_ratio_collateral_cap_mult - 0.85).abs() < f64::EPSILON);
                assert_eq!(cfg.allowed_sport_league_codes, vec!["nba", "mlb"]);
                assert_eq!(cfg.blocked_competition_levels, vec!["high", "unknown"]);
                assert_eq!(cfg.market_blacklist_keywords, vec!["spread", "total"]);
                assert_eq!(cfg.reward_min_shares_cap, 500.0);
            },
        );
    }

    #[test]
    fn mm_sport_config_accepts_legacy_usdc_cap_aliases() {
        with_mm_env(
            &[
                ("EVPOLY_MM_SPORT_MULTIPLE_COLLATERAL_CAP_MULT", None),
                ("EVPOLY_MM_SPORT_DEPTH_RATIO_COLLATERAL_CAP_MULT", None),
                ("EVPOLY_MM_SPORT_MULTIPLE_USDC_CAP_MULT", Some("0.35")),
                ("EVPOLY_MM_SPORT_DEPTH_RATIO_USDC_CAP_MULT", Some("0.75")),
            ],
            || {
                let cfg = MmSportConfig::from_env();
                assert!((cfg.multiple_collateral_cap_mult - 0.35).abs() < f64::EPSILON);
                assert!((cfg.depth_ratio_collateral_cap_mult - 0.75).abs() < f64::EPSILON);
            },
        );
    }
}
