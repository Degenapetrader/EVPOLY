use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::event_log::log_event;
use crate::signal_state::SharedSignalState;

static HYPERLIQUID_PARSE_ERROR_COUNT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct HyperliquidWssConfig {
    pub stream_url: String,
    pub coin: String,
    pub reconnect_min_sec: u64,
    pub reconnect_max_sec: u64,
    pub inactivity_timeout_sec: u64,
}

impl Default for HyperliquidWssConfig {
    fn default() -> Self {
        Self {
            stream_url: "wss://api.hyperliquid.xyz/ws".to_string(),
            coin: "@107".to_string(), // HYPE/USDC spot
            reconnect_min_sec: 1,
            reconnect_max_sec: 20,
            inactivity_timeout_sec: 30,
        }
    }
}

pub fn spawn_hyperliquid_trade_feed(
    cfg: HyperliquidWssConfig,
    state: SharedSignalState,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = cfg.reconnect_min_sec.max(1);
        let mut consecutive_failures: u32 = 0;

        loop {
            match connect_async(&cfg.stream_url).await {
                Ok((mut ws_stream, _)) => {
                    consecutive_failures = 0;
                    crate::log_println!("📡 Hyperliquid WSS connected: {}", cfg.stream_url);
                    log_event(
                        "hyperliquid_connected",
                        json!({
                            "stream_url": cfg.stream_url,
                            "coin": cfg.coin,
                            "backoff_sec": backoff
                        }),
                    );
                    backoff = cfg.reconnect_min_sec.max(1);
                    let connected_at_ms = chrono::Utc::now().timestamp_millis();
                    let subscribe = json!({
                        "method": "subscribe",
                        "subscription": {
                            "type": "trades",
                            "coin": cfg.coin
                        }
                    });
                    if let Err(err) = ws_stream.send(Message::Text(subscribe.to_string())).await {
                        log_event(
                            "hyperliquid_subscribe_failed",
                            json!({
                                "coin": cfg.coin,
                                "error": err.to_string()
                            }),
                        );
                        let lived_ms = chrono::Utc::now()
                            .timestamp_millis()
                            .saturating_sub(connected_at_ms);
                        log_event(
                            "hyperliquid_disconnected",
                            json!({
                                "coin": cfg.coin,
                                "connected_lifetime_ms": lived_ms
                            }),
                        );
                        sleep(Duration::from_secs(backoff)).await;
                        backoff = (backoff * 2).min(cfg.reconnect_max_sec.max(backoff));
                        continue;
                    }

                    loop {
                        let next_msg = tokio::time::timeout(
                            Duration::from_secs(cfg.inactivity_timeout_sec.max(5)),
                            ws_stream.next(),
                        )
                        .await;
                        let Some(msg) = (match next_msg {
                            Ok(v) => v,
                            Err(_) => {
                                log_event(
                                    "hyperliquid_inactivity_timeout",
                                    json!({
                                        "coin": cfg.coin,
                                        "timeout_sec": cfg.inactivity_timeout_sec.max(5)
                                    }),
                                );
                                break;
                            }
                        }) else {
                            break;
                        };

                        match msg {
                            Ok(Message::Text(text)) => {
                                if !process_trade_message(&state, text.as_str()).await {
                                    let c = HYPERLIQUID_PARSE_ERROR_COUNT
                                        .fetch_add(1, Ordering::Relaxed)
                                        + 1;
                                    if c == 1 || c % 100 == 0 {
                                        log_event(
                                            "hyperliquid_parse_error",
                                            json!({
                                                "coin": cfg.coin,
                                                "count": c
                                            }),
                                        );
                                    }
                                }
                            }
                            Ok(Message::Ping(_))
                            | Ok(Message::Pong(_))
                            | Ok(Message::Binary(_)) => {}
                            Ok(Message::Close(_)) => break,
                            Ok(_) => {}
                            Err(err) => {
                                log_event(
                                    "hyperliquid_read_error",
                                    json!({
                                        "coin": cfg.coin,
                                        "error": err.to_string()
                                    }),
                                );
                                break;
                            }
                        }
                    }

                    let lived_ms = chrono::Utc::now()
                        .timestamp_millis()
                        .saturating_sub(connected_at_ms);
                    log_event(
                        "hyperliquid_disconnected",
                        json!({
                            "coin": cfg.coin,
                            "connected_lifetime_ms": lived_ms
                        }),
                    );
                }
                Err(err) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    log_event(
                        "hyperliquid_connect_failed",
                        json!({
                            "stream_url": cfg.stream_url,
                            "coin": cfg.coin,
                            "error": err.to_string(),
                            "retry_in_sec": backoff,
                            "consecutive_failures": consecutive_failures
                        }),
                    );
                }
            }

            sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(cfg.reconnect_max_sec.max(backoff));
        }
    })
}

async fn process_trade_message(state: &SharedSignalState, raw: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    let Some(channel) = parsed.get("channel").and_then(|value| value.as_str()) else {
        return false;
    };
    if channel != "trades" {
        return true;
    }
    let Some(trades) = parsed.get("data").and_then(|value| value.as_array()) else {
        return false;
    };
    let Some(last_trade) = trades.last() else {
        return true;
    };
    let Some(price) = last_trade
        .get("px")
        .and_then(|value| value.as_str())
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
    else {
        return false;
    };
    let Some(fill_ts_ms) = last_trade
        .get("time")
        .and_then(|value| value.as_i64())
        .filter(|value| *value > 0)
    else {
        return false;
    };
    let mut guard = state.write().await;
    guard.hl_flow.last_price = Some(price);
    guard.hl_flow.last_fill_ts_ms = Some(fill_ts_ms);
    guard.hl_flow_updated_at_ms = fill_ts_ms;
    guard.updated_at_ms = guard.newest_source_update_ms();
    true
}
