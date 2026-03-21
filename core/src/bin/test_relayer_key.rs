//! Smoke-test Relayer API key auth headers.
//!
//! This validates RELAYER_API_KEY + RELAYER_API_KEY_ADDRESS against relayer-v2
//! read endpoints, and performs a schema-only /submit auth check.

use anyhow::{bail, Context, Result};
use clap::Parser;
use polymarket_arbitrage_bot::Config;
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "test_relayer_key")]
#[command(about = "Test RELAYER_API_KEY + RELAYER_API_KEY_ADDRESS against relayer-v2")]
struct Args {
    /// Config path (used only to load .env consistently with bot binaries)
    #[arg(short, long, default_value = "config.json")]
    config: String,

    /// Relayer API key (falls back to RELAYER_API_KEY env var)
    #[arg(long)]
    relayer_api_key: Option<String>,

    /// Relayer API key owner address (falls back to RELAYER_API_KEY_ADDRESS env var)
    #[arg(long)]
    relayer_api_key_address: Option<String>,

    /// Relayer base URL
    #[arg(long, default_value = "https://relayer-v2.polymarket.com")]
    base_url: String,

    /// HTTP timeout (seconds)
    #[arg(long, default_value_t = 15)]
    timeout_sec: u64,
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn mask_secret(value: &str) -> String {
    let len = value.chars().count();
    if len <= 8 {
        return "*".repeat(len.max(1));
    }
    let prefix: String = value.chars().take(4).collect();
    let suffix: String = value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{}...{}", prefix, suffix)
}

fn is_eth_address(value: &str) -> bool {
    if value.len() != 42 || !value.starts_with("0x") {
        return false;
    }
    value.chars().skip(2).all(|c| c.is_ascii_hexdigit())
}

fn summarize_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "<empty>".to_string();
    }

    if let Ok(json) = serde_json::from_str::<Value>(trimmed) {
        if let Some(arr) = json.as_array() {
            return format!("json_array(len={})", arr.len());
        }
        if let Some(obj) = json.as_object() {
            let mut keys: Vec<String> = obj.keys().cloned().collect();
            keys.sort();
            let shown = keys.into_iter().take(6).collect::<Vec<_>>().join(",");
            return format!("json_object(keys={})", shown);
        }
    }

    let mut compact = trimmed.replace('\n', " ");
    if compact.len() > 220 {
        compact.truncate(220);
        compact.push_str("...");
    }
    compact
}

async fn call_with_relayer_headers(
    client: &Client,
    method: Method,
    url: &str,
    key: &str,
    address: &str,
    body: Option<&str>,
) -> Result<(StatusCode, String)> {
    let mut req = client
        .request(method, url)
        .header("Accept", "application/json")
        .header("RELAYER_API_KEY", key)
        .header("RELAYER_API_KEY_ADDRESS", address);
    if let Some(payload) = body {
        req = req
            .header("Content-Type", "application/json")
            .body(payload.to_string());
    }
    let response = req
        .send()
        .await
        .with_context(|| format!("request failed for {}", url))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .with_context(|| format!("failed to read response body for {}", url))?;
    Ok((status, text))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let _ = Config::load(&PathBuf::from(&args.config))
        .with_context(|| format!("failed to load config/env from {}", args.config))?;

    let api_key = args
        .relayer_api_key
        .or_else(|| env_nonempty("RELAYER_API_KEY"))
        .ok_or_else(|| anyhow::anyhow!("missing RELAYER_API_KEY (flag or env)"))?;
    let address = args
        .relayer_api_key_address
        .or_else(|| env_nonempty("RELAYER_API_KEY_ADDRESS"))
        .ok_or_else(|| anyhow::anyhow!("missing RELAYER_API_KEY_ADDRESS (flag or env)"))?;

    if !is_eth_address(address.as_str()) {
        bail!("RELAYER_API_KEY_ADDRESS must be a 0x-prefixed 40-hex Ethereum address");
    }

    let base = args.base_url.trim_end_matches('/').to_string();
    let timeout = Duration::from_secs(args.timeout_sec.clamp(3, 120));
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .context("failed to build reqwest client")?;

    println!("Relayer key auth smoke test");
    println!("base_url={}", base);
    println!("RELAYER_API_KEY={}", mask_secret(api_key.as_str()));
    println!("RELAYER_API_KEY_ADDRESS={}", address);
    println!();

    let read_probes = [
        ("list_relayer_api_keys", Method::GET, "/relayer/api/keys"),
        ("list_recent_transactions", Method::GET, "/transactions"),
    ];

    let mut failures = 0usize;

    for (name, method, path) in read_probes {
        let url = format!("{}{}", base, path);
        let (status, body) = call_with_relayer_headers(
            &client,
            method,
            url.as_str(),
            api_key.as_str(),
            address.as_str(),
            None,
        )
        .await?;
        let ok = status.is_success();
        println!(
            "{}: status={} ok={} summary={}",
            name,
            status.as_u16(),
            ok,
            summarize_body(body.as_str())
        );
        if !ok {
            failures = failures.saturating_add(1);
        }
    }

    // /submit is the path used by redeem/merge flows; this checks auth only.
    // We send an intentionally invalid body and consider auth passing if status
    // is anything except 401/403.
    let submit_url = format!("{}/submit", base);
    let (submit_status, submit_body) = call_with_relayer_headers(
        &client,
        Method::POST,
        submit_url.as_str(),
        api_key.as_str(),
        address.as_str(),
        Some("{}"),
    )
    .await?;
    let submit_auth_ok =
        submit_status != StatusCode::UNAUTHORIZED && submit_status != StatusCode::FORBIDDEN;
    println!(
        "submit_schema_auth_check: status={} auth_ok={} summary={}",
        submit_status.as_u16(),
        submit_auth_ok,
        summarize_body(submit_body.as_str())
    );
    if !submit_auth_ok {
        failures = failures.saturating_add(1);
    }

    if failures > 0 {
        bail!(
            "relayer key smoke test failed ({} failing probe(s))",
            failures
        );
    }

    println!();
    println!("All relayer key probes passed.");
    Ok(())
}
