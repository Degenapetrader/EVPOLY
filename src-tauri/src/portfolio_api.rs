use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

const DATA_API_BASE: &str = "https://data-api.polymarket.com";
const REQUEST_TIMEOUT_SECS: u64 = 10;

#[derive(Clone, Debug, Deserialize)]
pub struct PortfolioValueRow {
    pub user: String,
    pub value: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PositionRow {
    #[serde(default)]
    pub asset: Option<String>,
    #[serde(default)]
    pub size: Option<f64>,
    #[serde(default, rename = "currentValue")]
    pub current_value: Option<f64>,
    #[serde(default, rename = "cashPnl")]
    pub cash_pnl: Option<f64>,
    #[serde(default, rename = "realizedPnl")]
    pub realized_pnl: Option<f64>,
    #[serde(default, rename = "curPrice")]
    pub current_price: Option<f64>,
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

pub async fn fetch_activity(wallet_address: &str, limit: usize) -> Result<Vec<Value>, String> {
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
    use super::{PortfolioValueRow, PositionRow};

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
            "size": 2.0,
            "currentValue": 12.5,
            "cashPnl": 1.0,
            "realizedPnl": 0.5,
            "curPrice": 0.61
        }]);
        let rows: Vec<PositionRow> =
            serde_json::from_value(payload).expect("decode portfolio position rows");
        assert_eq!(rows[0].asset.as_deref(), Some("token"));
        assert_eq!(rows[0].size, Some(2.0));
        assert_eq!(rows[0].current_value, Some(12.5));
        assert_eq!(rows[0].cash_pnl, Some(1.0));
        assert_eq!(rows[0].realized_pnl, Some(0.5));
        assert_eq!(rows[0].current_price, Some(0.61));
    }
}
