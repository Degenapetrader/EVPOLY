use alloy_primitives::Address;
use alloy_signer::Signer;
use alloy_signer_local::LocalSigner;
use polymarket_client_sdk_v2::clob::types::request::OrdersRequest;
use polymarket_client_sdk_v2::clob::types::response::OpenOrderResponse;
use polymarket_client_sdk_v2::clob::types::SignatureType;
use polymarket_client_sdk_v2::clob::{Client, Config};
use polymarket_client_sdk_v2::POLYGON;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;
use std::time::Duration;

const DATA_API_BASE: &str = "https://data-api.polymarket.com";
const REQUEST_TIMEOUT_SECS: u64 = 10;
const TERMINAL_CURSOR: &str = "LTE=";

#[derive(Clone, Debug, Deserialize)]
pub struct PortfolioValueRow {
    pub user: String,
    pub value: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionRow {
    #[serde(default, rename = "proxyWallet")]
    pub proxy_wallet: Option<String>,
    #[serde(default, rename = "conditionId")]
    pub condition_id: Option<String>,
    #[serde(default)]
    pub asset: Option<String>,
    #[serde(default)]
    pub size: Option<f64>,
    #[serde(default, rename = "avgPrice")]
    pub avg_price: Option<f64>,
    #[serde(default, rename = "initialValue")]
    pub initial_value: Option<f64>,
    #[serde(default, rename = "currentValue")]
    pub current_value: Option<f64>,
    #[serde(default, rename = "cashPnl")]
    pub cash_pnl: Option<f64>,
    #[serde(default, rename = "percentPnl")]
    pub percent_pnl: Option<f64>,
    #[serde(default, rename = "totalBought")]
    pub total_bought: Option<f64>,
    #[serde(default, rename = "realizedPnl")]
    pub realized_pnl: Option<f64>,
    #[serde(default, rename = "percentRealizedPnl")]
    pub percent_realized_pnl: Option<f64>,
    #[serde(default, rename = "curPrice")]
    pub current_price: Option<f64>,
    #[serde(default)]
    pub redeemable: Option<bool>,
    #[serde(default)]
    pub mergeable: Option<bool>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default, rename = "eventId")]
    pub event_id: Option<String>,
    #[serde(default, rename = "eventSlug")]
    pub event_slug: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default, rename = "outcomeIndex")]
    pub outcome_index: Option<u64>,
    #[serde(default, rename = "oppositeOutcome")]
    pub opposite_outcome: Option<String>,
    #[serde(default, rename = "oppositeAsset")]
    pub opposite_asset: Option<String>,
    #[serde(default, rename = "endDate")]
    pub end_date: Option<String>,
    #[serde(default, rename = "negativeRisk")]
    pub negative_risk: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityRow {
    #[serde(default, rename = "proxyWallet")]
    pub proxy_wallet: Option<String>,
    #[serde(default)]
    pub timestamp: Option<i64>,
    #[serde(default, rename = "conditionId")]
    pub condition_id: Option<String>,
    #[serde(default, rename = "type")]
    pub activity_type: Option<String>,
    #[serde(default)]
    pub size: Option<f64>,
    #[serde(default, rename = "usdcSize")]
    pub usdc_size: Option<f64>,
    #[serde(default, rename = "transactionHash")]
    pub transaction_hash: Option<String>,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub asset: Option<String>,
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default, rename = "outcomeIndex")]
    pub outcome_index: Option<i64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default, rename = "eventSlug")]
    pub event_slug: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
}

#[derive(Clone)]
pub struct AuthenticatedClobQuery {
    pub private_key: String,
    pub maker_address: String,
    pub signature_type: u8,
}

fn body_preview(raw: &str) -> String {
    let condensed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = condensed.chars();
    let preview: String = chars.by_ref().take(160).collect();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

async fn get_json(path: &str, query: &[(&str, String)]) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("portfolio client: {e}"))?;
    let url = format!("{DATA_API_BASE}{path}");
    let response = client
        .get(url)
        .query(query)
        .send()
        .await
        .map_err(|e| format!("portfolio request: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("portfolio response read: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "portfolio http status {}: {}",
            status.as_u16(),
            body_preview(&body)
        ));
    }
    serde_json::from_str(&body)
        .map_err(|e| format!("portfolio parse: {e}; body={}", body_preview(&body)))
}

fn parse_signature_type(raw: u8) -> Result<SignatureType, String> {
    match raw {
        0 => Ok(SignatureType::Eoa),
        1 => Ok(SignatureType::Proxy),
        2 => Ok(SignatureType::GnosisSafe),
        3 => Ok(SignatureType::Poly1271),
        _ => Err(format!("unsupported POLY_SIGNATURE_TYPE={raw}")),
    }
}

pub async fn fetch_portfolio_value(wallet_address: &str) -> Result<Vec<PortfolioValueRow>, String> {
    let payload = get_json("/value", &[("user", wallet_address.to_string())]).await?;
    serde_json::from_value(payload).map_err(|e| format!("portfolio value decode: {e}"))
}

pub async fn fetch_positions(
    wallet_address: &str,
    limit: usize,
) -> Result<Vec<PositionRow>, String> {
    let payload = get_json(
        "/positions",
        &[
            ("user", wallet_address.to_string()),
            ("limit", limit.clamp(1, 500).to_string()),
        ],
    )
    .await?;
    serde_json::from_value(payload).map_err(|e| format!("portfolio positions decode: {e}"))
}

pub async fn fetch_activity(
    wallet_address: &str,
    limit: usize,
) -> Result<Vec<ActivityRow>, String> {
    let payload = get_json(
        "/activity",
        &[
            ("user", wallet_address.to_string()),
            ("limit", limit.clamp(1, 1000).to_string()),
        ],
    )
    .await?;
    serde_json::from_value(payload).map_err(|e| format!("portfolio activity decode: {e}"))
}

pub async fn fetch_open_orders(
    query: &AuthenticatedClobQuery,
    limit: usize,
) -> Result<Vec<OpenOrderResponse>, String> {
    let signature_type = parse_signature_type(query.signature_type)?;
    let signer = LocalSigner::from_str(&query.private_key)
        .map_err(|e| format!("portfolio open orders signer: {e}"))?
        .with_chain_id(Some(POLYGON));
    let maker_address = Address::from_str(&query.maker_address)
        .map_err(|e| format!("portfolio maker address: {e}"))?;

    let config = Config::builder().use_server_time(true).build();
    let auth_builder = Client::new("https://clob.polymarket.com", config)
        .map_err(|e| format!("portfolio clob client: {e}"))?
        .authentication_builder(&signer);
    let client = match signature_type {
        SignatureType::Eoa => tokio::time::timeout(
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
            auth_builder.signature_type(signature_type).authenticate(),
        )
        .await
        .map_err(|_| "portfolio clob auth timed out".to_string())?
        .map_err(|e| format!("portfolio clob auth: {e}"))?,
        SignatureType::Proxy | SignatureType::GnosisSafe | SignatureType::Poly1271 => {
            tokio::time::timeout(
                Duration::from_secs(REQUEST_TIMEOUT_SECS),
                auth_builder
                    .funder(maker_address)
                    .signature_type(signature_type)
                    .authenticate(),
            )
            .await
            .map_err(|_| "portfolio clob auth timed out".to_string())?
            .map_err(|e| format!("portfolio clob auth: {e}"))?
        }
        _ => return Err("unsupported signature type".to_string()),
    };

    let mut cursor = None;
    let mut rows = Vec::new();
    let page_limit = limit.clamp(1, 200);

    loop {
        let page = tokio::time::timeout(
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
            client.orders(&OrdersRequest::default(), cursor.clone()),
        )
        .await
        .map_err(|_| "portfolio open orders fetch timed out".to_string())?
        .map_err(|e| format!("portfolio open orders fetch: {e}"))?;
        rows.extend(page.data);
        if rows.len() >= page_limit || page.next_cursor == TERMINAL_CURSOR {
            break;
        }
        cursor = Some(page.next_cursor);
    }

    rows.truncate(page_limit);
    Ok(rows)
}

pub async fn fetch_portfolio_value_with_fallback(
    wallet_address: &str,
) -> Result<(f64, &'static str), String> {
    if let Ok(rows) = fetch_portfolio_value(wallet_address).await {
        if let Some(row) = rows
            .into_iter()
            .find(|row| row.user.eq_ignore_ascii_case(wallet_address))
        {
            return Ok((row.value.max(0.0), "value"));
        }
    }

    let positions = fetch_positions(wallet_address, 500).await?;
    let fallback_sum = positions
        .into_iter()
        .filter_map(|row| row.current_value)
        .sum::<f64>()
        .max(0.0);
    Ok((fallback_sum, "positions"))
}

#[cfg(test)]
mod tests {
    use super::{ActivityRow, PortfolioValueRow, PositionRow};

    #[test]
    fn decodes_value_rows() {
        let payload = serde_json::json!([{ "user": "0xabc", "value": 42.5 }]);
        let rows: Vec<PortfolioValueRow> =
            serde_json::from_value(payload).expect("decode portfolio value rows");
        assert_eq!(rows[0].user, "0xabc");
        assert!((rows[0].value - 42.5).abs() < f64::EPSILON);
    }

    #[test]
    fn decodes_position_rows() {
        let payload = serde_json::json!([{
            "asset": "token",
            "conditionId": "0xabc",
            "size": 2.0,
            "avgPrice": 0.61,
            "initialValue": 1.22,
            "currentValue": 12.5,
            "cashPnl": 1.0,
            "realizedPnl": 0.5,
            "curPrice": 0.61,
            "title": "Example market",
            "icon": "https://example.com/icon.png",
            "outcome": "Yes"
        }]);
        let rows: Vec<PositionRow> =
            serde_json::from_value(payload).expect("decode portfolio position rows");
        assert_eq!(rows[0].asset.as_deref(), Some("token"));
        assert_eq!(rows[0].condition_id.as_deref(), Some("0xabc"));
        assert_eq!(rows[0].size, Some(2.0));
        assert_eq!(rows[0].avg_price, Some(0.61));
        assert_eq!(rows[0].current_value, Some(12.5));
        assert_eq!(rows[0].cash_pnl, Some(1.0));
        assert_eq!(rows[0].realized_pnl, Some(0.5));
        assert_eq!(rows[0].current_price, Some(0.61));
        assert_eq!(rows[0].title.as_deref(), Some("Example market"));
    }

    #[test]
    fn decodes_activity_rows() {
        let payload = serde_json::json!([{
            "timestamp": 1775733901,
            "conditionId": "0xabc",
            "type": "TRADE",
            "size": 9.01351,
            "usdcSize": 8.9298,
            "price": 0.99,
            "asset": "123",
            "side": "BUY",
            "title": "Example activity market",
            "icon": "https://example.com/icon.png",
            "outcome": "Down"
        }]);
        let rows: Vec<ActivityRow> =
            serde_json::from_value(payload).expect("decode portfolio activity rows");
        assert_eq!(rows[0].condition_id.as_deref(), Some("0xabc"));
        assert_eq!(rows[0].activity_type.as_deref(), Some("TRADE"));
        assert_eq!(rows[0].usdc_size, Some(8.9298));
        assert_eq!(rows[0].side.as_deref(), Some("BUY"));
        assert_eq!(rows[0].outcome.as_deref(), Some("Down"));
    }
}
