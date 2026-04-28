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
    pub quote_size_mode: MmSportQuoteSizeMode,
    pub exit_mode: MmSportExitMode,
    pub quote_size_mult: f64,
    pub pair_baseline_quote_size_mult: f64,
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
            max_share_ratio: env_f64("EVPOLY_MM_SPORT_MAX_SHARE_RATIO", 0.05).clamp(0.01, 0.99),
            min_top_depth_usd: env_f64("EVPOLY_MM_SPORT_MIN_TOP_DEPTH_USD", 100_000.0).max(0.0),
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
            match_only: env_bool("EVPOLY_MM_SPORT_MATCH_ONLY", true),
            post_only: env_bool("EVPOLY_MM_SPORT_POST_ONLY", true),
            max_markets: env_usize("EVPOLY_MM_SPORT_MAX_MARKETS", 0),
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
            ],
            || {
                let cfg = MmSportConfig::from_env();
                assert_eq!(cfg.exit_mode, MmSportExitMode::Normal);
                assert_eq!(cfg.no_exit_side_pause_sec, 3_600);
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
}
