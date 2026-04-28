#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const PUSD_CONTRACT: &str = "0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB";
const BALANCE_OF_SELECTOR: &str = "0x70a08231";
const RPC_TIMEOUT_SECS: u64 = 8;

#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    params: serde_json::Value,
    id: u64,
}

#[derive(Deserialize)]
struct JsonRpcResponse {
    result: Option<String>,
    error: Option<serde_json::Value>,
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

fn parse_hex_balance(result: &str) -> Result<f64, String> {
    let hex_str = result.trim_start_matches("0x");
    let hex_trimmed = hex_str.trim_start_matches('0');
    let hex_trimmed = if hex_trimmed.is_empty() {
        "0"
    } else {
        hex_trimmed
    };
    let balance_raw =
        u128::from_str_radix(hex_trimmed, 16).map_err(|e| format!("parse balance: {e}"))?;
    Ok(balance_raw as f64 / 1_000_000.0)
}

fn parse_rpc_balance_response(raw: &str) -> Result<f64, String> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|e| format!("rpc parse: {e}; body={}", body_preview(raw)))?;

    let resp: JsonRpcResponse = serde_json::from_value(value.clone())
        .map_err(|e| format!("rpc decode: {e}; body={}", body_preview(raw)))?;

    if let Some(err) = resp.error {
        return Err(format!("rpc error: {err}"));
    }
    if let Some(result) = resp.result {
        return parse_hex_balance(&result);
    }

    if let Some(message) = value.get("message").and_then(Value::as_str) {
        return Err(format!("rpc error: {message}"));
    }
    if let Some(title) = value.get("title").and_then(Value::as_str) {
        return Err(format!("rpc error: {title}"));
    }
    Err(format!(
        "rpc error: missing result field; body={}",
        body_preview(raw)
    ))
}

pub async fn fetch_pusd_balance(rpc_url: &str, wallet_address: &str) -> Result<f64, String> {
    let addr = wallet_address.trim_start_matches("0x");
    let padded = format!("{:0>64}", addr);
    let data = format!("{BALANCE_OF_SELECTOR}{padded}");

    let request = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        method: "eth_call".into(),
        params: serde_json::json!([
            {
                "to": PUSD_CONTRACT,
                "data": data
            },
            "latest"
        ]),
        id: 1,
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(RPC_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("rpc client: {e}"))?;
    let resp = client
        .post(rpc_url)
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("rpc request: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| format!("rpc read: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "rpc http status {} from {}: {}",
            status.as_u16(),
            rpc_url,
            body_preview(&body)
        ));
    }
    parse_rpc_balance_response(&body)
}

pub async fn fetch_pusd_balance_with_fallback(
    rpc_urls: &[&str],
    wallet_address: &str,
) -> Result<f64, String> {
    if rpc_urls.is_empty() {
        return Err("rpc fallback list is empty".to_string());
    }
    let mut errors = Vec::new();
    for rpc_url in rpc_urls {
        match fetch_pusd_balance(rpc_url, wallet_address).await {
            Ok(balance) => return Ok(balance),
            Err(err) => errors.push(format!("{rpc_url}: {err}")),
        }
    }
    Err(format!("all rpc endpoints failed: {}", errors.join(" | ")))
}

#[cfg(test)]
mod tests {
    use super::parse_rpc_balance_response;

    #[test]
    fn parses_success_result() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"result":"0x0f4240"}"#;
        let balance = parse_rpc_balance_response(raw).expect("expected parse success");
        assert!((balance - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parses_standard_rpc_error() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"rate limited"}}"#;
        let err = parse_rpc_balance_response(raw).expect_err("expected rpc error");
        assert!(err.contains("rpc error"));
        assert!(err.contains("rate limited"));
    }

    #[test]
    fn parses_non_standard_json_error_shape() {
        let raw = r#"{"title":"Too many requests","status":429}"#;
        let err = parse_rpc_balance_response(raw).expect_err("expected rpc error");
        assert!(err.contains("Too many requests"));
    }

    #[test]
    fn rejects_invalid_json() {
        let raw = "<html>not json</html>";
        let err = parse_rpc_balance_response(raw).expect_err("expected parse error");
        assert!(err.contains("rpc parse"));
    }
}
