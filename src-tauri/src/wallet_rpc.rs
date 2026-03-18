#![allow(dead_code)]

use serde::{Deserialize, Serialize};

const USDC_CONTRACT: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
const BALANCE_OF_SELECTOR: &str = "0x70a08231";

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

pub async fn fetch_usdc_balance(rpc_url: &str, wallet_address: &str) -> Result<f64, String> {
    let addr = wallet_address.trim_start_matches("0x");
    let padded = format!("{:0>64}", addr);
    let data = format!("{BALANCE_OF_SELECTOR}{padded}");

    let request = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        method: "eth_call".into(),
        params: serde_json::json!([
            {
                "to": USDC_CONTRACT,
                "data": data
            },
            "latest"
        ]),
        id: 1,
    };

    let client = reqwest::Client::new();
    let resp = client
        .post(rpc_url)
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("rpc request: {e}"))?
        .json::<JsonRpcResponse>()
        .await
        .map_err(|e| format!("rpc parse: {e}"))?;

    if let Some(err) = resp.error {
        return Err(format!("rpc error: {err}"));
    }

    let hex_str = resp
        .result
        .ok_or_else(|| "no result from rpc".to_string())?;
    let hex_str = hex_str.trim_start_matches("0x");
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
