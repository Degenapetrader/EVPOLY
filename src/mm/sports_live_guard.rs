use crate::event_log::log_event;
use crate::models::{GammaEventSummary, Market};
use crate::strategy::STRATEGY_ID_MM_SPORT_V1;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub type SharedSportsLiveState = Arc<RwLock<HashMap<i64, SportsLiveSnapshot>>>;

#[derive(Debug, Clone, Default)]
pub struct SportsLiveSnapshot {
    pub game_id: Option<i64>,
    pub event_slug: Option<String>,
    pub live: Option<bool>,
    pub period: Option<String>,
    pub status: Option<String>,
    pub score: Option<String>,
    pub source: &'static str,
    pub observed_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct SportsLiveGuardDecision {
    pub snapshot: SportsLiveSnapshot,
    pub reason: &'static str,
}

pub fn new_shared_sports_live_state() -> SharedSportsLiveState {
    Arc::new(RwLock::new(HashMap::new()))
}

pub fn snapshot_from_gamma_market(
    market: &Market,
    observed_at_ms: i64,
) -> Option<SportsLiveSnapshot> {
    market
        .events
        .as_ref()?
        .iter()
        .find_map(|event| snapshot_from_gamma_event(event, observed_at_ms))
}

pub fn snapshot_from_gamma_event(
    event: &GammaEventSummary,
    observed_at_ms: i64,
) -> Option<SportsLiveSnapshot> {
    let live = event
        .live
        .or_else(|| event.event_state.as_ref().and_then(|state| state.live));
    let period = event.period.clone().or_else(|| {
        event
            .event_state
            .as_ref()
            .and_then(|state| state.period.clone())
    });
    let status = event.status.clone().or_else(|| {
        event
            .event_state
            .as_ref()
            .and_then(|state| state.status.clone())
    });
    let score = event.score.clone().or_else(|| {
        event
            .event_state
            .as_ref()
            .and_then(|state| state.score.clone())
    });
    if event.game_id.is_none()
        && event.slug.as_deref().unwrap_or_default().trim().is_empty()
        && live.is_none()
        && period.as_deref().unwrap_or_default().trim().is_empty()
        && status.as_deref().unwrap_or_default().trim().is_empty()
    {
        return None;
    }
    Some(SportsLiveSnapshot {
        game_id: event.game_id,
        event_slug: event
            .slug
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        live,
        period: normalize_optional_string(period),
        status: normalize_optional_string(status),
        score: normalize_optional_string(score),
        source: "gamma_market_event",
        observed_at_ms,
    })
}

pub fn snapshot_from_ws_value(value: &Value, observed_at_ms: i64) -> Option<SportsLiveSnapshot> {
    let game_id = value
        .get("gameId")
        .and_then(Value::as_i64)
        .or_else(|| value.get("game_id").and_then(Value::as_i64));
    let event_state = value.get("eventState");
    let live = value.get("live").and_then(Value::as_bool).or_else(|| {
        event_state
            .and_then(|state| state.get("live"))
            .and_then(Value::as_bool)
    });
    let period = value.get("period").and_then(Value::as_str).or_else(|| {
        event_state
            .and_then(|state| state.get("period"))
            .and_then(Value::as_str)
    });
    let status = value.get("status").and_then(Value::as_str).or_else(|| {
        event_state
            .and_then(|state| state.get("status"))
            .and_then(Value::as_str)
    });
    let score = value.get("score").and_then(Value::as_str).or_else(|| {
        event_state
            .and_then(|state| state.get("score"))
            .and_then(Value::as_str)
    });
    if game_id.is_none()
        && live.is_none()
        && period.unwrap_or_default().trim().is_empty()
        && status.unwrap_or_default().trim().is_empty()
    {
        return None;
    }
    Some(SportsLiveSnapshot {
        game_id,
        event_slug: value
            .get("slug")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        live,
        period: normalize_optional_string(period.map(str::to_string)),
        status: normalize_optional_string(status.map(str::to_string)),
        score: normalize_optional_string(score.map(str::to_string)),
        source: "polymarket_sports_ws",
        observed_at_ms,
    })
}

pub fn sports_live_guard_decision(
    gamma_snapshot: Option<&SportsLiveSnapshot>,
    ws_snapshot: Option<&SportsLiveSnapshot>,
    now_ms: i64,
    ws_stale_ms: i64,
) -> Option<SportsLiveGuardDecision> {
    if let Some(snapshot) = ws_snapshot.filter(|snapshot| {
        snapshot.game_id.is_some()
            && snapshot.observed_at_ms > 0
            && now_ms.saturating_sub(snapshot.observed_at_ms) <= ws_stale_ms
    }) {
        if let Some(reason) = sports_live_guard_reason(snapshot) {
            return Some(SportsLiveGuardDecision {
                snapshot: snapshot.clone(),
                reason,
            });
        }
    }
    if let Some(snapshot) = gamma_snapshot {
        if let Some(reason) = sports_live_guard_reason(snapshot) {
            return Some(SportsLiveGuardDecision {
                snapshot: snapshot.clone(),
                reason,
            });
        }
    }
    None
}

pub async fn run_polymarket_sports_live_ws_loop(shared_state: SharedSportsLiveState, url: String) {
    let mut reconnect_delay_ms = 5_000_u64;
    loop {
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        match connect_async(url.as_str()).await {
            Ok((ws_stream, _)) => {
                reconnect_delay_ms = 5_000;
                log_event(
                    "mm_sport_polymarket_sports_ws_connected",
                    json!({
                        "strategy_id": STRATEGY_ID_MM_SPORT_V1,
                        "url": url
                    }),
                );
                let (mut write, mut read) = ws_stream.split();
                let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
                loop {
                    tokio::select! {
                        _ = heartbeat.tick() => {
                            if write.send(Message::Ping(Vec::new())).await.is_err() {
                                break;
                            }
                        }
                        message = read.next() => {
                            let Some(message) = message else {
                                break;
                            };
                            let message = match message {
                                Ok(message) => message,
                                Err(err) => {
                                    log_event(
                                        "mm_sport_polymarket_sports_ws_error",
                                        json!({
                                            "strategy_id": STRATEGY_ID_MM_SPORT_V1,
                                            "error": err.to_string()
                                        }),
                                    );
                                    break;
                                }
                            };
                            let text = match message {
                                Message::Text(text) => text,
                                Message::Binary(bytes) => match String::from_utf8(bytes) {
                                    Ok(text) => text,
                                    Err(_) => continue,
                                },
                                Message::Ping(bytes) => {
                                    let _ = write.send(Message::Pong(bytes)).await;
                                    continue;
                                }
                                Message::Pong(_) => continue,
                                Message::Close(_) => break,
                                Message::Frame(_) => continue,
                            };
                            let now_ms = chrono::Utc::now().timestamp_millis();
                            ingest_ws_text(&shared_state, text.as_str(), now_ms).await;
                        }
                    }
                }
            }
            Err(err) => {
                log_event(
                    "mm_sport_polymarket_sports_ws_connect_failed",
                    json!({
                        "strategy_id": STRATEGY_ID_MM_SPORT_V1,
                        "url": url,
                        "error": err.to_string()
                    }),
                );
            }
        }

        log_event(
            "mm_sport_polymarket_sports_ws_disconnected",
            json!({
                "strategy_id": STRATEGY_ID_MM_SPORT_V1,
                "url": url,
                "connected_ms": chrono::Utc::now().timestamp_millis().saturating_sub(started_at_ms),
                "reconnect_delay_ms": reconnect_delay_ms
            }),
        );
        sleep(Duration::from_millis(reconnect_delay_ms)).await;
        reconnect_delay_ms = reconnect_delay_ms.saturating_mul(2).clamp(5_000, 60_000);
    }
}

async fn ingest_ws_text(shared_state: &SharedSportsLiveState, text: &str, observed_at_ms: i64) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if let Some(items) = value.as_array() {
        for item in items {
            ingest_ws_value(shared_state, item, observed_at_ms).await;
        }
    } else {
        ingest_ws_value(shared_state, &value, observed_at_ms).await;
    }
}

async fn ingest_ws_value(shared_state: &SharedSportsLiveState, value: &Value, observed_at_ms: i64) {
    let Some(snapshot) = snapshot_from_ws_value(value, observed_at_ms) else {
        return;
    };
    let Some(game_id) = snapshot.game_id else {
        return;
    };
    let mut state = shared_state.write().await;
    state.insert(game_id, snapshot);
}

fn sports_live_guard_reason(snapshot: &SportsLiveSnapshot) -> Option<&'static str> {
    if snapshot.live.unwrap_or(false) {
        return Some("live_true");
    }
    if status_indicates_live(snapshot.status.as_deref()) {
        return Some("status_in_progress");
    }
    if period_indicates_in_game(snapshot.period.as_deref()) {
        return Some("period_in_game");
    }
    None
}

fn status_indicates_live(status: Option<&str>) -> bool {
    let Some(status) = status
        .map(normalize_state_text)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    matches!(
        status.as_str(),
        "live" | "inprogress" | "inplay" | "halftime" | "halftimebreak" | "running"
    )
}

fn period_indicates_in_game(period: Option<&str>) -> bool {
    let Some(period) = period
        .map(normalize_state_text)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    !matches!(
        period.as_str(),
        "ns" | "notstarted" | "scheduled" | "pre" | "pregame"
    )
}

fn normalize_state_text(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn normalize_optional_string(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_live_true_blocks() {
        let snapshot = SportsLiveSnapshot {
            live: Some(true),
            source: "gamma_market_event",
            ..Default::default()
        };
        assert_eq!(sports_live_guard_reason(&snapshot), Some("live_true"));
    }

    #[test]
    fn pregame_period_does_not_block() {
        let snapshot = SportsLiveSnapshot {
            live: Some(false),
            period: Some("NS".to_string()),
            source: "gamma_market_event",
            ..Default::default()
        };
        assert_eq!(sports_live_guard_reason(&snapshot), None);
    }

    #[test]
    fn in_game_period_blocks() {
        let snapshot = SportsLiveSnapshot {
            live: Some(false),
            period: Some("Bot 9th".to_string()),
            source: "gamma_market_event",
            ..Default::default()
        };
        assert_eq!(sports_live_guard_reason(&snapshot), Some("period_in_game"));
    }

    #[test]
    fn ended_only_payload_does_not_block() {
        let value = json!({
            "gameId": 10077700,
            "ended": true
        });
        let snapshot = snapshot_from_ws_value(&value, 1_000).expect("snapshot");
        assert_eq!(sports_live_guard_reason(&snapshot), None);
    }

    #[test]
    fn stale_ws_snapshot_is_ignored() {
        let ws_snapshot = SportsLiveSnapshot {
            game_id: Some(1),
            live: Some(true),
            observed_at_ms: 1_000,
            source: "polymarket_sports_ws",
            ..Default::default()
        };
        assert!(sports_live_guard_decision(None, Some(&ws_snapshot), 20_000, 1_000).is_none());
    }
}
