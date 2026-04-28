use crate::models::{Market, MarketDetails, MarketToken, Token};
use serde::{Deserialize, Serialize};

const KNOWN_SYMBOLS: [&str; 7] = ["BTC", "ETH", "SOL", "XRP", "DOGE", "BNB", "HYPE"];
const KNOWN_TIMEFRAMES: [&str; 7] = ["5m", "15m", "1h", "4h", "12h", "24h", "1d"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiMarketSide {
    pub token_id: String,
    pub label: String,
    pub outcome: String,
    pub price: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiMarket {
    pub condition_id: String,
    pub market_slug: String,
    pub title: String,
    pub subtitle: String,
    pub description: Option<String>,
    pub status: String,
    pub tradable: bool,
    pub close_time: Option<String>,
    pub symbol: Option<String>,
    pub timeframe: Option<String>,
    pub sides: Vec<UiMarketSide>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiManualRun {
    pub run_id: String,
    pub kind: String,
    pub kind_label: String,
    pub status: String,
    pub status_label: String,
    pub condition_id: String,
    pub market_slug: String,
    pub market_title: String,
    pub market_subtitle: String,
    pub side: String,
    pub side_label: String,
    pub mode: String,
    pub mode_label: String,
    pub target_shares: f64,
    pub filled_shares: f64,
    pub remaining_shares: f64,
    pub requested_size_usd: Option<f64>,
    pub progress_ratio: f64,
    pub progress_summary: String,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub post_only: bool,
    pub tp_price: Option<f64>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiManualPosition {
    pub condition_id: String,
    pub market_slug: String,
    pub market_title: String,
    pub market_subtitle: String,
    pub side: String,
    pub side_label: String,
    pub size: f64,
    pub entry_price: Option<f64>,
    pub current_price: Option<f64>,
    pub realized_pnl: Option<f64>,
    pub unrealized_pnl: Option<f64>,
    pub redeemable: bool,
    pub mergeable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiManualHealth {
    pub status: String,
    pub mode: String,
    pub message: String,
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiDashboardSummary {
    pub bot_state: String,
    pub mode: String,
    pub headline: String,
    pub detail: String,
    pub last_activity_at_ms: Option<i64>,
    pub last_activity_at: Option<String>,
    pub recent_result: Option<String>,
    pub blocker_reason: Option<String>,
    pub enabled_strategies: Vec<String>,
    pub open_positions_count: u64,
    pub recent_orders_count: u64,
    pub free_balance: Option<f64>,
    pub avg_ack_latency_ms: Option<f64>,
    pub total_pnl: f64,
    pub total_trades: u64,
    pub winning_trades: u64,
    pub losing_trades: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiStrategyState {
    pub strategy_id: String,
    pub slug: String,
    pub label: String,
    pub enabled: bool,
    pub state: String,
    pub summary: String,
    pub scope_summary: String,
    pub last_action: Option<String>,
    pub last_action_at_ms: Option<i64>,
    pub last_action_at: Option<String>,
    pub blocker_reason: Option<String>,
    pub open_orders_count: u64,
    pub open_positions_count: u64,
}

pub fn build_ui_market(market: &Market, details: Option<&MarketDetails>) -> UiMarket {
    let condition_id = market.condition_id.trim().to_string();
    let market_slug = preferred_market_slug(market, details);
    let title = preferred_title(market, details);
    let description = preferred_description(market, details);
    let close_time = preferred_close_time(market, details);
    let symbol = infer_symbol(market_slug.as_str(), title.as_str());
    let timeframe = infer_timeframe(market_slug.as_str(), title.as_str());
    let sides = build_sides(market, details);
    let active = details.map(|value| value.active).unwrap_or(market.active);
    let closed = details.map(|value| value.closed).unwrap_or(market.closed);
    let accepting_orders = details.map(|value| value.accepting_orders).unwrap_or(true);
    let status = if closed {
        "closed".to_string()
    } else if active && accepting_orders {
        "active".to_string()
    } else if active {
        "paused".to_string()
    } else {
        "inactive".to_string()
    };
    let tradable = active && !closed && accepting_orders && sides.len() >= 2;
    let subtitle = build_subtitle(
        symbol.as_deref(),
        timeframe.as_deref(),
        close_time.as_deref(),
        description.as_deref(),
        market_slug.as_str(),
    );

    UiMarket {
        condition_id,
        market_slug,
        title,
        subtitle,
        description,
        status,
        tradable,
        close_time,
        symbol,
        timeframe,
        sides,
    }
}

fn preferred_market_slug(market: &Market, details: Option<&MarketDetails>) -> String {
    let from_market = market.slug.trim();
    if !from_market.is_empty() {
        return from_market.to_string();
    }
    let from_details = details
        .map(|value| value.market_slug.trim())
        .filter(|value| !value.is_empty());
    if let Some(value) = from_details {
        return value.to_string();
    }
    market.condition_id.trim().to_string()
}

fn preferred_title(market: &Market, details: Option<&MarketDetails>) -> String {
    let from_market = market.question.trim();
    if !from_market.is_empty() {
        return from_market.to_string();
    }
    let from_details = details
        .map(|value| value.question.trim())
        .filter(|value| !value.is_empty());
    if let Some(value) = from_details {
        return value.to_string();
    }
    preferred_market_slug(market, details)
}

fn preferred_description(market: &Market, details: Option<&MarketDetails>) -> Option<String> {
    market
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            details
                .map(|value| value.description.trim())
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
}

fn preferred_close_time(market: &Market, details: Option<&MarketDetails>) -> Option<String> {
    market
        .end_date_iso
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            market
                .end_date
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .or_else(|| {
            details
                .map(|value| value.end_date_iso.trim())
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
}

fn build_sides(market: &Market, details: Option<&MarketDetails>) -> Vec<UiMarketSide> {
    if let Some(tokens) = details.map(|value| value.tokens.as_slice()) {
        return tokens.iter().map(ui_side_from_market_token).collect();
    }
    market
        .tokens
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(ui_side_from_token)
        .collect()
}

fn ui_side_from_market_token(token: &MarketToken) -> UiMarketSide {
    UiMarketSide {
        token_id: token.token_id.clone(),
        label: humanize_outcome(token.outcome.as_str()),
        outcome: token.outcome.clone(),
        price: f64::try_from(token.price).ok(),
    }
}

fn ui_side_from_token(token: &Token) -> UiMarketSide {
    UiMarketSide {
        token_id: token.token_id.clone(),
        label: humanize_outcome(token.outcome.as_str()),
        outcome: token.outcome.clone(),
        price: token.price.and_then(|value| f64::try_from(value).ok()),
    }
}

fn humanize_outcome(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Unknown".to_string();
    }
    match trimmed.to_ascii_uppercase().as_str() {
        "YES" => "Yes".to_string(),
        "NO" => "No".to_string(),
        "UP" => "Up".to_string(),
        "DOWN" => "Down".to_string(),
        _ => {
            let mut out = String::with_capacity(trimmed.len());
            for (index, chunk) in trimmed.split_whitespace().enumerate() {
                if index > 0 {
                    out.push(' ');
                }
                let mut chars = chunk.chars();
                if let Some(first) = chars.next() {
                    out.push(first.to_ascii_uppercase());
                    for ch in chars {
                        out.push(ch.to_ascii_lowercase());
                    }
                }
            }
            if out.is_empty() {
                trimmed.to_string()
            } else {
                out
            }
        }
    }
}

fn infer_symbol(slug: &str, title: &str) -> Option<String> {
    infer_known_token(slug)
        .or_else(|| infer_known_token(title))
        .map(ToString::to_string)
}

fn infer_timeframe(slug: &str, title: &str) -> Option<String> {
    infer_known_timeframe(slug)
        .or_else(|| infer_known_timeframe(title))
        .map(ToString::to_string)
}

fn infer_known_token(raw: &str) -> Option<&'static str> {
    let normalized = raw
        .replace(['/', '_', '.'], "-")
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(|value| value.trim().to_ascii_uppercase())
        .collect::<Vec<_>>();
    for token in normalized {
        let symbol = match token.as_str() {
            "SOLANA" => "SOL",
            "DOGECOIN" => "DOGE",
            other => other,
        };
        if KNOWN_SYMBOLS.contains(&symbol) {
            return Some(match symbol {
                "BTC" => "BTC",
                "ETH" => "ETH",
                "SOL" => "SOL",
                "XRP" => "XRP",
                "DOGE" => "DOGE",
                "BNB" => "BNB",
                "HYPE" => "HYPE",
                _ => unreachable!(),
            });
        }
    }
    None
}

fn infer_known_timeframe(raw: &str) -> Option<&'static str> {
    let normalized = raw
        .replace(['/', '_', '.'], "-")
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    for token in normalized {
        if KNOWN_TIMEFRAMES.contains(&token.as_str()) {
            return Some(match token.as_str() {
                "5m" => "5m",
                "15m" => "15m",
                "1h" => "1h",
                "4h" => "4h",
                "12h" => "12h",
                "24h" => "24h",
                "1d" => "1d",
                _ => unreachable!(),
            });
        }
    }
    None
}

fn build_subtitle(
    symbol: Option<&str>,
    timeframe: Option<&str>,
    close_time: Option<&str>,
    description: Option<&str>,
    slug: &str,
) -> String {
    let mut parts = Vec::new();
    match (symbol, timeframe) {
        (Some(symbol), Some(timeframe)) => parts.push(format!("{symbol} / {timeframe}")),
        (Some(symbol), None) => parts.push(symbol.to_string()),
        (None, Some(timeframe)) => parts.push(timeframe.to_string()),
        (None, None) => {}
    }
    if let Some(value) = close_time
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(compact_time_label)
    {
        parts.push(format!("Closes {value}"));
    }
    if parts.is_empty() {
        if let Some(value) = description
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(72).collect::<String>())
        {
            return value;
        }
        if !slug.trim().is_empty() {
            return slug.replace('-', " ");
        }
        return "Market details".to_string();
    }
    parts.join(" / ")
}

fn compact_time_label(raw: &str) -> String {
    raw.trim()
        .replace('T', " ")
        .replace(".000Z", " UTC")
        .replace('Z', " UTC")
}

pub fn manual_run_kind_label(kind: &str) -> &'static str {
    match kind.trim().to_ascii_lowercase().as_str() {
        "open" => "Open",
        "close" => "Close",
        _ => "Manual",
    }
}

pub fn manual_run_status_label(status: &str) -> &'static str {
    match status.trim().to_ascii_lowercase().as_str() {
        "running" => "Running",
        "stopping" => "Stopping",
        "completed" => "Completed",
        "canceled" => "Canceled",
        "failed" => "Needs attention",
        other if other.is_empty() => "Unknown",
        _ => "Updated",
    }
}

pub fn manual_mode_label(mode: &str) -> &'static str {
    match mode.trim().to_ascii_lowercase().as_str() {
        "chase_limit" => "Chase Limit",
        "limit" => "Limit",
        "market" => "Market",
        _ => "Manual",
    }
}

pub fn manual_side_label(side: &str) -> String {
    humanize_outcome(side)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Market, Token};

    fn sample_market() -> Market {
        Market {
            condition_id: "0xabc".to_string(),
            market_id: Some("123".to_string()),
            question: "Will BTC finish above 100,000 this 15m period?".to_string(),
            slug: "btc-updown-15m-1767726000".to_string(),
            description: Some("A simple BTC up/down market".to_string()),
            resolution_source: None,
            end_date: Some("2026-03-21T12:00:00Z".to_string()),
            end_date_iso: Some("2026-03-21T12:00:00Z".to_string()),
            end_date_iso_alt: None,
            game_start_time: None,
            start_date: None,
            sports_market_type: None,
            active: true,
            closed: false,
            tokens: Some(vec![
                Token {
                    token_id: "yes-token".to_string(),
                    outcome: "YES".to_string(),
                    price: None,
                },
                Token {
                    token_id: "no-token".to_string(),
                    outcome: "NO".to_string(),
                    price: None,
                },
            ]),
            clob_token_ids: None,
            outcomes: None,
            competitive: None,
            events: None,
        }
    }

    #[test]
    fn ui_market_normalizes_common_fields() {
        let ui_market = build_ui_market(&sample_market(), None);
        assert_eq!(ui_market.condition_id, "0xabc");
        assert_eq!(ui_market.market_slug, "btc-updown-15m-1767726000");
        assert_eq!(
            ui_market.title,
            "Will BTC finish above 100,000 this 15m period?"
        );
        assert_eq!(ui_market.symbol.as_deref(), Some("BTC"));
        assert_eq!(ui_market.timeframe.as_deref(), Some("15m"));
        assert_eq!(ui_market.status, "active");
        assert!(ui_market.tradable);
        assert_eq!(ui_market.sides.len(), 2);
        assert_eq!(ui_market.sides[0].label, "Yes");
        assert!(ui_market.subtitle.contains("BTC / 15m"));
    }
}
