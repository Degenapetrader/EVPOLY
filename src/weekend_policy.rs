use chrono::{DateTime, Datelike, Utc, Weekday};

use crate::strategy::{
    STRATEGY_ID_ENDGAME_SWEEP_V1, STRATEGY_ID_EVCURVE_V1, STRATEGY_ID_PREMARKET_V1,
    STRATEGY_ID_SESSIONBAND_V1,
};

pub const WEEKEND_POLICY_ENV_KEY: &str = "EVPOLY_WEEKEND_POLICY";
pub const DEV_FORCE_WEEKEND_STATE_ENV_KEY: &str = "EVPOLY_DEV_FORCE_WEEKEND_STATE";
pub const WEEKEND_POLICY_OFF: &str = "off";
pub const WEEKEND_POLICY_PAUSE: &str = "pause";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeekendPolicy {
    Off,
    Pause,
}

impl WeekendPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => WEEKEND_POLICY_OFF,
            Self::Pause => WEEKEND_POLICY_PAUSE,
        }
    }
}

fn parse_weekend_policy(value: Option<&str>) -> WeekendPolicy {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) if raw.eq_ignore_ascii_case(WEEKEND_POLICY_PAUSE) => WeekendPolicy::Pause,
        _ => WeekendPolicy::Off,
    }
}

fn parse_forced_weekend_state(value: Option<&str>) -> Option<bool> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) if raw.eq_ignore_ascii_case("weekend") => Some(true),
        Some(raw) if raw.eq_ignore_ascii_case("weekday") => Some(false),
        _ => None,
    }
}

fn is_weekend_utc(now: DateTime<Utc>, forced_state: Option<bool>) -> bool {
    forced_state.unwrap_or_else(|| matches!(now.weekday(), Weekday::Sat | Weekday::Sun))
}

fn is_weekend_pause_active_with(
    policy: WeekendPolicy,
    now: DateTime<Utc>,
    forced_state: Option<bool>,
) -> bool {
    policy == WeekendPolicy::Pause && is_weekend_utc(now, forced_state)
}

pub fn weekend_policy_from_env() -> WeekendPolicy {
    parse_weekend_policy(std::env::var(WEEKEND_POLICY_ENV_KEY).ok().as_deref())
}

pub fn strategy_uses_weekend_pause(strategy_id: &str) -> bool {
    matches!(
        strategy_id,
        STRATEGY_ID_PREMARKET_V1
            | STRATEGY_ID_ENDGAME_SWEEP_V1
            | STRATEGY_ID_EVCURVE_V1
            | STRATEGY_ID_SESSIONBAND_V1
    )
}

pub fn is_weekend_pause_active_at(now: DateTime<Utc>) -> bool {
    let policy = weekend_policy_from_env();
    let forced_state = parse_forced_weekend_state(
        std::env::var(DEV_FORCE_WEEKEND_STATE_ENV_KEY)
            .ok()
            .as_deref(),
    );
    is_weekend_pause_active_with(policy, now, forced_state)
}

pub fn is_weekend_pause_active_now() -> bool {
    is_weekend_pause_active_at(Utc::now())
}

pub fn should_pause_new_entries_at(strategy_id: &str, now: DateTime<Utc>) -> bool {
    strategy_uses_weekend_pause(strategy_id) && is_weekend_pause_active_at(now)
}

pub fn should_pause_new_entries_now(strategy_id: &str) -> bool {
    should_pause_new_entries_at(strategy_id, Utc::now())
}

#[cfg(test)]
mod tests {
    use super::{
        is_weekend_pause_active_with, is_weekend_utc, parse_forced_weekend_state,
        parse_weekend_policy, strategy_uses_weekend_pause, WeekendPolicy,
    };
    use crate::strategy::{
        STRATEGY_ID_ENDGAME_SWEEP_V1, STRATEGY_ID_EVCURVE_V1, STRATEGY_ID_EVSNIPE_V1,
        STRATEGY_ID_MM_SPORT_V1, STRATEGY_ID_PREMARKET_V1,
    };
    use chrono::{TimeZone, Utc};

    #[test]
    fn weekend_boundaries_are_utc_exact() {
        assert!(!is_weekend_utc(
            Utc.with_ymd_and_hms(2026, 4, 3, 23, 59, 59)
                .single()
                .unwrap(),
            None
        ));
        assert!(is_weekend_utc(
            Utc.with_ymd_and_hms(2026, 4, 4, 0, 0, 0).single().unwrap(),
            None
        ));
        assert!(is_weekend_utc(
            Utc.with_ymd_and_hms(2026, 4, 5, 12, 0, 0).single().unwrap(),
            None
        ));
        assert!(!is_weekend_utc(
            Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 0).single().unwrap(),
            None
        ));
    }

    #[test]
    fn dev_override_can_force_weekend_state() {
        let friday = Utc.with_ymd_and_hms(2026, 4, 3, 12, 0, 0).single().unwrap();
        assert_eq!(parse_forced_weekend_state(Some("weekend")), Some(true));
        assert_eq!(parse_forced_weekend_state(Some("weekday")), Some(false));
        assert!(is_weekend_utc(friday, Some(true)));
        assert!(!is_weekend_utc(friday, Some(false)));
    }

    #[test]
    fn strategy_filter_only_targets_core_entry_strategies() {
        assert!(strategy_uses_weekend_pause(STRATEGY_ID_PREMARKET_V1));
        assert!(strategy_uses_weekend_pause(STRATEGY_ID_ENDGAME_SWEEP_V1));
        assert!(strategy_uses_weekend_pause(STRATEGY_ID_EVCURVE_V1));
        assert!(!strategy_uses_weekend_pause(STRATEGY_ID_EVSNIPE_V1));
        assert!(!strategy_uses_weekend_pause(STRATEGY_ID_MM_SPORT_V1));
    }

    #[test]
    fn policy_parse_defaults_to_off() {
        assert_eq!(parse_weekend_policy(None), WeekendPolicy::Off);
        assert_eq!(parse_weekend_policy(Some("")), WeekendPolicy::Off);
        assert_eq!(parse_weekend_policy(Some("pause")), WeekendPolicy::Pause);
    }

    #[test]
    fn should_pause_depends_on_policy_and_strategy() {
        let saturday = Utc.with_ymd_and_hms(2026, 4, 4, 0, 0, 0).single().unwrap();
        assert!(!is_weekend_pause_active_with(
            parse_weekend_policy(Some("off")),
            saturday,
            None
        ));
        assert!(is_weekend_pause_active_with(
            parse_weekend_policy(Some("pause")),
            saturday,
            None
        ));
        assert!(strategy_uses_weekend_pause(STRATEGY_ID_PREMARKET_V1));
        assert!(!strategy_uses_weekend_pause(STRATEGY_ID_EVSNIPE_V1));
    }
}
