use std::str::FromStr;

use alloy_primitives::Address;
use alloy_signer::Signer;
use alloy_signer_local::LocalSigner;
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE;
use base64::Engine;
use chrono::{NaiveDate, Utc};
use hmac::{Hmac, Mac};
use polymarket_client_sdk::clob::types::SignatureType;
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::POLYGON;
use secrecy::ExposeSecret;
use serde_json::Value;
use sha2::Sha256;

const TODAY_TTL_MS: i64 = 60_000;
const LIFETIME_TTL_MS: i64 = 30 * 60_000;

#[derive(Clone)]
pub struct LiquidityRewardsQuery {
    pub private_key: String,
    pub maker_address: String,
    pub signature_type: u8,
    pub start_date: NaiveDate,
}

#[derive(Clone, Default)]
pub struct LiquidityRewardsCacheEntry {
    pub wallet_address: String,
    pub signature_type: u8,
    pub today_date: Option<NaiveDate>,
    pub today_rewards_usd: Option<f64>,
    pub today_fetched_at_ms: i64,
    pub lifetime_start_date: Option<NaiveDate>,
    pub lifetime_rewards_usd: Option<f64>,
    pub lifetime_fetched_at_ms: i64,
    pub as_of_utc: Option<String>,
}

#[derive(Clone)]
pub struct LiquidityRewardsSummary {
    pub today_rewards_usd: f64,
    pub lifetime_rewards_usd: f64,
    pub as_of_utc: String,
    pub cache: LiquidityRewardsCacheEntry,
}

fn parse_signature_type(raw: u8) -> Result<SignatureType> {
    match raw {
        0 => Ok(SignatureType::Eoa),
        1 => Ok(SignatureType::Proxy),
        2 => Ok(SignatureType::GnosisSafe),
        _ => bail!("unsupported POLY_SIGNATURE_TYPE={raw}"),
    }
}

fn hmac_signature(secret_b64url: &str, message: &str) -> Result<String> {
    let decoded_secret = URL_SAFE.decode(secret_b64url)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&decoded_secret)?;
    mac.update(message.as_bytes());
    let result = mac.finalize().into_bytes();
    Ok(URL_SAFE.encode(result))
}

async fn raw_get(
    http: &reqwest::Client,
    url: &str,
    eoa_address: Address,
    api_key: &str,
    passphrase: &str,
    secret_b64url: &str,
) -> Result<Value> {
    let timestamp = Utc::now().timestamp();
    let parsed = reqwest::Url::parse(url)?;
    let message = format!("{timestamp}GET{}", parsed.path());
    let signature = hmac_signature(secret_b64url, &message)?;

    let text = http
        .get(url)
        .header("POLY_ADDRESS", format!("{eoa_address:#x}"))
        .header("POLY_API_KEY", api_key)
        .header("POLY_PASSPHRASE", passphrase)
        .header("POLY_SIGNATURE", signature)
        .header("POLY_TIMESTAMP", timestamp.to_string())
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    serde_json::from_str(&text).with_context(|| format!("invalid rewards json: {text}"))
}

fn parse_total_earnings(value: &Value) -> f64 {
    let Some(rows) = value.as_array() else {
        return 0.0;
    };
    rows.iter()
        .filter_map(|row| match row.get("earnings") {
            Some(Value::Number(number)) => number.as_f64(),
            Some(Value::String(text)) => text.parse::<f64>().ok(),
            _ => None,
        })
        .sum()
}

async fn authenticate_and_fetch(
    query: &LiquidityRewardsQuery,
    fetch_today: bool,
    fetch_lifetime: bool,
) -> Result<(Address, Option<f64>, Option<f64>)> {
    let signature_type = parse_signature_type(query.signature_type)?;
    let signer = LocalSigner::from_str(&query.private_key)?.with_chain_id(Some(POLYGON));
    let eoa_address = signer.address();
    let maker_address = Address::from_str(&query.maker_address)
        .with_context(|| format!("invalid maker address: {}", query.maker_address))?;

    let config = Config::builder().use_server_time(true).build();
    let auth_builder =
        Client::new("https://clob.polymarket.com", config)?.authentication_builder(&signer);
    let client = match signature_type {
        SignatureType::Eoa => {
            auth_builder
                .signature_type(signature_type)
                .authenticate()
                .await?
        }
        SignatureType::Proxy | SignatureType::GnosisSafe => {
            auth_builder
                .funder(maker_address)
                .signature_type(signature_type)
                .authenticate()
                .await?
        }
        _ => bail!("unsupported signature type"),
    };

    let http = reqwest::Client::new();
    let creds = client.credentials();
    let today = Utc::now().date_naive();
    let mut today_rewards = None;
    let mut lifetime_rewards = None;

    if fetch_today {
        let total_url = format!(
            "https://clob.polymarket.com/rewards/user/total?date={today}&signature_type={}&maker_address={maker_address:#x}",
            query.signature_type
        );
        let total = raw_get(
            &http,
            &total_url,
            eoa_address,
            &creds.key().to_string(),
            creds.passphrase().expose_secret(),
            creds.secret().expose_secret(),
        )
        .await?;
        today_rewards = Some(parse_total_earnings(&total));
    }

    if fetch_lifetime {
        let mut running_total = 0.0;
        let start = query.start_date.min(today);
        let mut day = start;
        while day <= today {
            let url = format!(
                "https://clob.polymarket.com/rewards/user/total?date={day}&signature_type={}&maker_address={maker_address:#x}",
                query.signature_type
            );
            let total = raw_get(
                &http,
                &url,
                eoa_address,
                &creds.key().to_string(),
                creds.passphrase().expose_secret(),
                creds.secret().expose_secret(),
            )
            .await?;
            running_total += parse_total_earnings(&total);
            day = match day.succ_opt() {
                Some(next) => next,
                None => break,
            };
        }
        lifetime_rewards = Some(running_total);
    }

    Ok((maker_address, today_rewards, lifetime_rewards))
}

pub async fn fetch_summary(
    query: &LiquidityRewardsQuery,
    cached: Option<LiquidityRewardsCacheEntry>,
) -> Result<LiquidityRewardsSummary> {
    let now_ms = Utc::now().timestamp_millis();
    let today = Utc::now().date_naive();
    let mut cache = cached.unwrap_or_default();
    let wallet_key = query.maker_address.trim().to_ascii_lowercase();

    let wallet_matches =
        cache.wallet_address == wallet_key && cache.signature_type == query.signature_type;
    let today_fresh = wallet_matches
        && cache.today_date == Some(today)
        && now_ms.saturating_sub(cache.today_fetched_at_ms) < TODAY_TTL_MS;
    let lifetime_fresh = wallet_matches
        && cache.lifetime_start_date == Some(query.start_date)
        && cache.lifetime_rewards_usd.is_some()
        && now_ms.saturating_sub(cache.lifetime_fetched_at_ms) < LIFETIME_TTL_MS;

    if !today_fresh || !lifetime_fresh {
        let (maker_address, today_rewards, lifetime_rewards) =
            authenticate_and_fetch(query, !today_fresh, !lifetime_fresh).await?;
        cache.wallet_address = format!("{maker_address:#x}").to_ascii_lowercase();
        cache.signature_type = query.signature_type;
        cache.as_of_utc = Some(Utc::now().to_rfc3339());

        if let Some(value) = today_rewards {
            cache.today_date = Some(today);
            cache.today_rewards_usd = Some(value);
            cache.today_fetched_at_ms = now_ms;
        }
        if let Some(value) = lifetime_rewards {
            cache.lifetime_start_date = Some(query.start_date);
            cache.lifetime_rewards_usd = Some(value);
            cache.lifetime_fetched_at_ms = now_ms;
        }
    }

    Ok(LiquidityRewardsSummary {
        today_rewards_usd: cache.today_rewards_usd.unwrap_or(0.0),
        lifetime_rewards_usd: cache.lifetime_rewards_usd.unwrap_or(0.0),
        as_of_utc: cache
            .as_of_utc
            .clone()
            .unwrap_or_else(|| Utc::now().to_rfc3339()),
        cache,
    })
}
