#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct OnboardResult {
    pub signer_token: Option<String>,
    pub discovery_token: Option<String>,
    pub premarket_alpha_token: Option<String>,
    pub endgame_alpha_token: Option<String>,
    pub mm_rewards_alpha_token: Option<String>,
    pub evsnipe_discovery_token: Option<String>,
    pub admin_api_token: Option<String>,
}

pub async fn run_onboarding(
    wallet: &str,
    private_key: &str,
    signature_type: u8,
    proxy_wallet: &str,
) -> Result<OnboardResult, String> {
    let _ = private_key;

    let client = reqwest::Client::new();
    let payload = serde_json::json!({
        "wallet": wallet,
        "proxy_wallet": proxy_wallet,
        "signature_type": signature_type,
    });

    let resp = client
        .post("https://alpha.evplus.ai/v1/onboard")
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("onboard request failed: {e}. Try manual token entry."))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "onboard returned {status}: {body}. Try manual token entry."
        ));
    }

    let result: OnboardResult = resp
        .json()
        .await
        .map_err(|e| format!("parse onboard response: {e}. Try manual token entry."))?;

    Ok(result)
}
