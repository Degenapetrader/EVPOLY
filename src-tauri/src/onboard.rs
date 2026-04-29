#![allow(dead_code)]

use ethers_signers::{LocalWallet, Signer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const DEFAULT_ONBOARD_API_BASE: &str = "https://im23e4zz3k.execute-api.eu-west-1.amazonaws.com";
const ONBOARD_TIMEOUT_SECS: u64 = 10;

fn wallet_address_hex(wallet: &LocalWallet) -> String {
    format!("{:#x}", wallet.address())
}

fn onboard_url(operation: &str) -> String {
    format!("{DEFAULT_ONBOARD_API_BASE}/onboard/{operation}")
}

fn first_nonempty(values: &[Option<&Value>]) -> Option<String> {
    values
        .iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .map(str::trim)
        .find(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

fn derive_order_signer_primary_token(
    runtime: &serde_json::Map<String, Value>,
    remote_signer_token: &Option<String>,
) -> Option<String> {
    first_nonempty(&[runtime.get("order_signer_token")]).or_else(|| remote_signer_token.clone())
}

async fn post_json(client: &reqwest::Client, url: &str, payload: &Value) -> Result<Value, String> {
    let response = client
        .post(url)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .json(payload)
        .send()
        .await
        .map_err(|e| format!("onboard request failed: {e}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("onboard response read failed: {e}"))?;

    if !status.is_success() {
        return Err(format!("onboard returned {}: {}", status, body));
    }

    serde_json::from_str::<Value>(&body).map_err(|e| format!("parse onboard response: {e}"))
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct OnboardResult {
    pub eoa_wallet: Option<String>,
    pub bound_wallet: Option<String>,
    pub remote_signer_token: Option<String>,
    pub signer_token: Option<String>,
    pub order_signer_primary_token: Option<String>,
    pub discovery_token: Option<String>,
    pub premarket_alpha_token: Option<String>,
    pub endgame_alpha_token: Option<String>,
    pub evsnipe_discovery_token: Option<String>,
    pub admin_api_token: Option<String>,
}

pub async fn run_onboarding(
    private_key: &str,
    signature_type: u8,
    proxy_wallet: &str,
) -> Result<OnboardResult, String> {
    let key = private_key.trim();
    if key.is_empty() {
        return Err("private key is required for onboarding".to_string());
    }
    if !matches!(signature_type, 0..=2) {
        return Err("signature_type must be 0, 1, or 2".to_string());
    }

    let local_wallet: LocalWallet = key
        .parse()
        .map_err(|e| format!("invalid private key: {e}"))?;
    let derived_eoa = wallet_address_hex(&local_wallet);
    let signature_wallet = derived_eoa.clone();

    let bind_wallet = if matches!(signature_type, 1 | 2) {
        let proxy = proxy_wallet.trim();
        if proxy.is_empty() {
            return Err("proxy_wallet is required when signature_type is 1 or 2".to_string());
        }
        proxy.to_string()
    } else {
        signature_wallet.clone()
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(ONBOARD_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("build onboard client: {e}"))?;
    let start_url = onboard_url("start");
    let finish_url = onboard_url("finish");

    let mut start_payload = serde_json::json!({
        "wallet": signature_wallet,
        "signature_type": signature_type,
    });
    if matches!(signature_type, 1 | 2) {
        start_payload["proxy_wallet"] = Value::String(bind_wallet.clone());
    }
    let start_response = post_json(&client, &start_url, &start_payload).await?;
    let challenge_id = start_response
        .get("challenge_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("invalid onboard/start response: {}", start_response))?;
    let message = start_response
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("invalid onboard/start response: {}", start_response))?;

    let signature = local_wallet
        .sign_message(message)
        .await
        .map_err(|e| format!("failed to sign onboarding message: {e}"))?
        .to_string();

    let mut finish_payload = serde_json::json!({
        "challenge_id": challenge_id,
        "wallet": signature_wallet,
        "signature": signature,
        "signature_type": signature_type,
    });
    if matches!(signature_type, 1 | 2) {
        finish_payload["proxy_wallet"] = Value::String(bind_wallet.clone());
    }
    let finish_response = post_json(&client, &finish_url, &finish_payload).await?;
    let runtime = finish_response
        .get("runtime")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let remote_signer_token = first_nonempty(&[
        finish_response.get("remote_signer_token"),
        finish_response.get("signer_token"),
        finish_response.get("token"),
        finish_response.get("api_key"),
    ]);
    if remote_signer_token.is_none() {
        return Err(format!(
            "onboard/finish response missing signer token: {}",
            finish_response
        ));
    }

    let shared_alpha_token = first_nonempty(&[
        runtime.get("remote_alpha_token"),
        runtime.get("remote_discovery_token"),
    ]);
    let order_signer_primary_token =
        derive_order_signer_primary_token(&runtime, &remote_signer_token);

    let mut result = OnboardResult {
        eoa_wallet: Some(derived_eoa),
        bound_wallet: Some(bind_wallet.clone()),
        remote_signer_token: remote_signer_token.clone(),
        signer_token: remote_signer_token,
        order_signer_primary_token,
        discovery_token: first_nonempty(&[
            runtime.get("remote_discovery_token"),
            runtime.get("remote_market_discovery_token"),
        ]),
        premarket_alpha_token: first_nonempty(&[runtime.get("remote_premarket_alpha_token")])
            .or_else(|| shared_alpha_token.clone()),
        endgame_alpha_token: first_nonempty(&[runtime.get("remote_endgame_alpha_token")])
            .or_else(|| shared_alpha_token.clone()),
        evsnipe_discovery_token: first_nonempty(&[
            runtime.get("remote_evsnipe_discovery_token"),
            runtime.get("remote_discovery_token"),
        ]),
        admin_api_token: first_nonempty(&[
            finish_response.get("admin_api_token"),
            runtime.get("admin_api_token"),
        ]),
    };

    if result.remote_signer_token.is_none() {
        result.remote_signer_token = result.signer_token.clone();
    }
    if result.signer_token.is_none() {
        result.signer_token = result.remote_signer_token.clone();
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{derive_order_signer_primary_token, onboard_url, wallet_address_hex};
    use ethers_signers::LocalWallet;
    use serde_json::json;

    #[test]
    fn wallet_address_hex_uses_full_address() {
        let wallet: LocalWallet =
            "0x59d07efa05b4c7f6f1f450f0a3cb2ed0d49b98f6000000000000000000000001"
                .parse()
                .expect("valid private key");

        let address = wallet_address_hex(&wallet);

        assert_eq!(address.len(), 42);
        assert!(address.starts_with("0x"));
        assert!(address.chars().skip(2).all(|c| c.is_ascii_hexdigit()));
        assert!(!address.contains('…'));
    }

    #[test]
    fn onboard_url_points_to_root_api_gateway_path() {
        assert_eq!(
            onboard_url("start"),
            "https://im23e4zz3k.execute-api.eu-west-1.amazonaws.com/onboard/start"
        );
        assert_eq!(
            onboard_url("finish"),
            "https://im23e4zz3k.execute-api.eu-west-1.amazonaws.com/onboard/finish"
        );
    }

    #[test]
    fn derive_order_signer_primary_token_falls_back_to_remote_signer_token() {
        let runtime = serde_json::Map::new();
        let remote_signer_token = Some("remote-token".to_string());

        assert_eq!(
            derive_order_signer_primary_token(&runtime, &remote_signer_token),
            Some("remote-token".to_string())
        );
    }

    #[test]
    fn derive_order_signer_primary_token_prefers_runtime_override() {
        let runtime = json!({ "order_signer_token": "primary-token" })
            .as_object()
            .cloned()
            .expect("object");
        let remote_signer_token = Some("remote-token".to_string());

        assert_eq!(
            derive_order_signer_primary_token(&runtime, &remote_signer_token),
            Some("primary-token".to_string())
        );
    }
}
