use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use polymarket_arbitrage_bot::builder_attribution::configured_builder_code;
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, COOKIE, USER_AGENT,
};
use reqwest::{Client, Method, StatusCode};
use serde::Serialize;
use serde_json::Value;

const DEFAULT_CLOB_URL: &str = "https://clob.polymarket.com";

#[derive(Debug, Clone, Copy, ValueEnum)]
enum WriteMethod {
    Patch,
    Put,
    Post,
}

impl WriteMethod {
    fn as_reqwest_method(self) -> Method {
        match self {
            WriteMethod::Patch => Method::PATCH,
            WriteMethod::Put => Method::PUT,
            WriteMethod::Post => Method::POST,
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "builder_fees")]
#[command(about = "Check or set Polymarket CLOB V2 builder fee rates")]
struct Args {
    /// CLOB base URL. Defaults to POLY_CLOB_API_URL or CLOB v2.
    #[arg(long)]
    clob_url: Option<String>,

    /// Builder code. Defaults to POLY_BUILDER_CODE or the official EVPOLY builder code.
    #[arg(long)]
    builder_code: Option<String>,

    /// Submit a fee update instead of only reading current fee settings.
    #[arg(long)]
    set: bool,

    /// Maker fee in basis points. Required with --set.
    #[arg(long)]
    maker_bps: Option<u16>,

    /// Taker fee in basis points. Required with --set.
    #[arg(long)]
    taker_bps: Option<u16>,

    /// HTTP method for the write call. If the method is not allowed, the tool tries the others.
    #[arg(long, value_enum, default_value_t = WriteMethod::Patch)]
    method: WriteMethod,

    /// Required with --set to prevent accidental live fee changes.
    #[arg(long)]
    yes: bool,

    /// Extra HTTP header, for example: --header "Authorization: Bearer <token>".
    #[arg(long)]
    header: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BuilderFeeUpdate {
    builder_maker_fee_rate_bps: u16,
    builder_taker_fee_rate_bps: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let clob_url = args
        .clob_url
        .clone()
        .or_else(|| std::env::var("POLY_CLOB_API_URL").ok())
        .unwrap_or_else(|| DEFAULT_CLOB_URL.to_string());
    let builder_code = args
        .builder_code
        .clone()
        .unwrap_or_else(configured_builder_code);

    validate_builder_code(builder_code.as_str())?;

    let endpoint = format!(
        "{}/fees/builder-fees/{}",
        clob_url.trim_end_matches('/'),
        builder_code
    );
    let client = Client::builder()
        .user_agent("evpoly-builder-fees/1.0")
        .build()
        .context("failed to build HTTP client")?;
    let headers = build_headers(&args)?;

    if !args.set {
        let response = client
            .get(endpoint.as_str())
            .headers(headers)
            .send()
            .await
            .context("failed to query builder fees")?;
        return print_response("GET", response).await;
    }

    let maker_bps = args
        .maker_bps
        .context("--maker-bps is required when --set is used")?;
    let taker_bps = args
        .taker_bps
        .context("--taker-bps is required when --set is used")?;

    let payload = BuilderFeeUpdate {
        builder_maker_fee_rate_bps: maker_bps,
        builder_taker_fee_rate_bps: taker_bps,
    };

    if !args.yes {
        eprintln!(
            "Refusing live write without --yes. Would set builder={} maker={}bps taker={}bps",
            builder_code, maker_bps, taker_bps
        );
        eprintln!(
            "Current fee status is still safe to check with: cargo run --bin builder_fees --"
        );
        std::process::exit(2);
    }

    let methods = write_methods_with_fallback(args.method);
    let mut last_method_not_allowed = false;
    for method in methods {
        let response = client
            .request(method.clone(), endpoint.as_str())
            .headers(headers.clone())
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("failed to submit builder fee update via {}", method))?;

        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            last_method_not_allowed = true;
            continue;
        }

        return print_response(method.as_str(), response).await;
    }

    if last_method_not_allowed {
        bail!("builder fee endpoint rejected PATCH, PUT, and POST with 405 Method Not Allowed");
    }

    bail!("builder fee update did not receive a response")
}

fn validate_builder_code(builder_code: &str) -> Result<()> {
    let stripped = builder_code.strip_prefix("0x").unwrap_or(builder_code);
    if stripped.len() != 64 {
        bail!("builder code must be a 32-byte hex value");
    }
    hex::decode(stripped).context("builder code must be hex")?;
    Ok(())
}

fn build_headers(args: &Args) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("evpoly-builder-fees/1.0"),
    );
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

    if let Some(token) = env_nonempty("POLY_BUILDER_FEE_BEARER_TOKEN") {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(format!("Bearer {}", token).as_str())
                .context("invalid POLY_BUILDER_FEE_BEARER_TOKEN header value")?,
        );
    }
    if let Some(cookie) = env_nonempty("POLY_BUILDER_FEE_COOKIE") {
        headers.insert(
            COOKIE,
            HeaderValue::from_str(cookie.as_str())
                .context("invalid POLY_BUILDER_FEE_COOKIE header value")?,
        );
    }

    for raw in &args.header {
        let (name, value) = raw
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("header must be in 'Name: Value' format: {}", raw))?;
        let name = HeaderName::from_bytes(name.trim().as_bytes())
            .with_context(|| format!("invalid header name: {}", name.trim()))?;
        let value = HeaderValue::from_str(value.trim())
            .with_context(|| format!("invalid header value for {}", name))?;
        headers.insert(name, value);
    }

    Ok(headers)
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn write_methods_with_fallback(preferred: WriteMethod) -> Vec<Method> {
    let mut methods = vec![preferred.as_reqwest_method()];
    for method in [Method::PATCH, Method::PUT, Method::POST] {
        if !methods.iter().any(|existing| existing == &method) {
            methods.push(method);
        }
    }
    methods
}

async fn print_response(method: &str, response: reqwest::Response) -> Result<()> {
    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed to read response body")?;
    println!("{} {}", method, status.as_u16());

    if !status.is_success() {
        if body.trim().is_empty() {
            bail!("builder fee endpoint returned {}", status);
        }
        bail!("builder fee endpoint returned {}: {}", status, body);
    }

    if body.trim().is_empty() {
        return Ok(());
    }

    match serde_json::from_str::<Value>(&body) {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
        Err(_) => println!("{}", body),
    }

    Ok(())
}
