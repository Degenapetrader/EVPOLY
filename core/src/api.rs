use crate::models::*;
use crate::polymarket_ws::{SharedPolymarketWsState, WsOrderStatusSnapshot, WsTradeSnapshot};
use anyhow::{Context, Result};
use base64;
use base64::Engine as _;
use hex;
use log::{error, warn};
use reqwest::{Client, Proxy};
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// Official SDK imports for proper order signing
use alloy::primitives::Address as AlloyAddress;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer as _;
use alloy::sol_types::SolCall;
use chrono::{TimeZone, Utc};
use polymarket_client_sdk::auth::builder::{Builder as BuilderAuth, Config as BuilderConfig};
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::{Credentials, LocalSigner, Normal};
use polymarket_client_sdk::clob::types::request::CancelMarketOrderRequest;
use polymarket_client_sdk::clob::types::response::{CancelOrdersResponse, OpenOrderResponse};
use polymarket_client_sdk::clob::types::{
    Amount, OrderType, Side, SignatureType, SignedOrder as ClobSignedOrder,
};
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::{contract_config, derive_safe_wallet, POLYGON};

// CTF (Conditional Token Framework) imports for redemption
// Based on docs: https://docs.polymarket.com/developers/builders/relayer-client#redeem-positions
use alloy::primitives::{keccak256, B256, U256};
use alloy::providers::ProviderBuilder;

// Contract interfaces for direct RPC calls (like SDK example)
use alloy::sol;
use polymarket_client_sdk::types::Address;

sol! {
    #[sol(rpc)]
    interface IERC20 {
        function approve(address spender, uint256 value) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
    }

    #[sol(rpc)]
    interface IERC1155 {
        function setApprovalForAll(address operator, bool approved) external;
        function isApprovedForAll(address account, address operator) external view returns (bool);
    }

    struct ProxyCall {
        uint8 typeCode;
        address to;
        uint256 value;
        bytes data;
    }

    #[sol(rpc)]
    interface IProxyWalletFactory {
        function proxy(ProxyCall[] calls) external payable returns (bytes[] returnValues);
    }
}

const ORDER_SIGNER_PRIMARY_URL_DEFAULT: &str = "https://signer.evplus.ai/sign/order";
const ORDER_SIGNER_AWS_FALLBACK_URL_DEFAULT: &str =
    "https://im23e4zz3k.execute-api.eu-west-1.amazonaws.com/sign/order";
const SUBMIT_SIGNER_AWS_URL_DEFAULT: &str =
    "https://im23e4zz3k.execute-api.eu-west-1.amazonaws.com/sign/submit";
const ORDER_SIGNER_PRIMARY_FALLBACK_TIMEOUT_MS: u64 = 500;
const ORDER_SIGNER_DEGRADED_WINDOW: Duration = Duration::from_secs(5 * 60);
const ORDER_SIGNER_AWS_FORCE_WINDOW: Duration = Duration::from_secs(60 * 60);
const USDC_BALANCE_CACHE_HIT_TTL_MS: i64 = 60_000;
const USDC_BALANCE_CACHE_FALLBACK_TTL_MS: i64 = 180_000;
const TICK_METADATA_RL_BACKOFF_BASE_MS: i64 = 1_000;
const TICK_METADATA_RL_BACKOFF_MAX_MS: i64 = 30_000;
const ACTIVE_MARKETS_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Deserialize, Default)]
struct ActiveMarketsEventRow {
    #[serde(default)]
    markets: Vec<Market>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ActiveMarketsEnvelope {
    EventArray(Vec<ActiveMarketsEventRow>),
    DataArray { data: Vec<ActiveMarketsEventRow> },
}

fn parse_active_markets_payload(bytes: &[u8]) -> Result<Vec<Market>> {
    let parsed: ActiveMarketsEnvelope =
        serde_json::from_slice(bytes).context("Failed to parse active markets payload")?;
    let mut markets = Vec::new();
    match parsed {
        ActiveMarketsEnvelope::EventArray(events) => {
            for event in events {
                markets.extend(event.markets);
            }
        }
        ActiveMarketsEnvelope::DataArray { data } => {
            for event in data {
                markets.extend(event.markets);
            }
        }
    }
    Ok(markets)
}

pub struct ClobClientHandle {
    pub client: ClobClient<Authenticated<Normal>>,
    pub builder_client_primary: ClobClient<Authenticated<BuilderAuth>>,
    pub builder_client_fallback: ClobClient<Authenticated<BuilderAuth>>,
    pub signer: PrivateKeySigner,
}

#[derive(Debug, Default)]
struct OrderSignerCircuitState {
    degraded_since: Option<Instant>,
    force_aws_until: Option<Instant>,
}

#[derive(Debug, Default)]
struct ClobAuthBackoffState {
    consecutive_failures: u32,
    retry_after_ms: i64,
}

#[derive(Debug, Clone, Default)]
pub struct MmReadTelemetrySnapshot {
    pub proxy_attempts: u64,
    pub direct_attempts: u64,
    pub proxy_successes: u64,
    pub direct_successes: u64,
    pub proxy_429s: u64,
    pub direct_429s: u64,
    pub proxy_errors: u64,
    pub direct_errors: u64,
}

#[derive(Debug, Clone, Default)]
pub struct PlaceOrderTiming {
    pub total_api_ms: i64,
    pub attempts_used: u32,
    pub retry_count: u32,
    pub retry_policy: String,
    pub order_type_effective: String,
    pub get_client_ms: i64,
    pub prewarm_ms: i64,
    pub prewarm_cache_hit: bool,
    pub build_order_ms: i64,
    pub sign_ms: i64,
    pub build_sign_ms: i64,
    pub post_order_ms: i64,
    pub backoff_sleep_ms: i64,
    pub order_signer_primary_post_ms: i64,
    pub order_signer_fallback_post_ms: i64,
    pub order_signer_primary_attempts: u32,
    pub order_signer_fallback_attempts: u32,
    pub order_signer_final_route: String,
    pub order_signer_primary_url: String,
    pub order_signer_fallback_url: String,
}

#[derive(Debug, Clone, Copy, Default)]
struct PlaceOrderHandleTiming {
    prewarm_ms: i64,
    prewarm_cache_hit: bool,
    build_order_ms: i64,
    sign_ms: i64,
    build_sign_ms: i64,
    post_order_ms: i64,
    order_signer_primary_post_ms: i64,
    order_signer_fallback_post_ms: i64,
    order_signer_primary_attempts: u32,
    order_signer_fallback_attempts: u32,
    order_signer_final_route: Option<OrderSignerRoute>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrderSignerRoute {
    Primary,
    Fallback,
}

impl OrderSignerRoute {
    fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct OrderSignerPostStats {
    primary_post_ms: i64,
    fallback_post_ms: i64,
    primary_attempts: u32,
    fallback_attempts: u32,
    final_route: Option<OrderSignerRoute>,
}

impl OrderSignerPostStats {
    fn record_attempt(&mut self, route: OrderSignerRoute, elapsed_ms: i64) {
        match route {
            OrderSignerRoute::Primary => {
                self.primary_attempts = self.primary_attempts.saturating_add(1);
                self.primary_post_ms = self.primary_post_ms.saturating_add(elapsed_ms.max(0));
            }
            OrderSignerRoute::Fallback => {
                self.fallback_attempts = self.fallback_attempts.saturating_add(1);
                self.fallback_post_ms = self.fallback_post_ms.saturating_add(elapsed_ms.max(0));
            }
        }
    }

    fn record_success(&mut self, route: OrderSignerRoute) {
        self.final_route = Some(route);
    }
}

#[derive(Debug, Clone)]
pub struct BatchPlaceOrderResult {
    pub response: Option<OrderResponse>,
    pub timing: PlaceOrderTiming,
    pub error: Option<String>,
}

#[derive(Debug)]
struct PreparedSignedOrder {
    signed_order: ClobSignedOrder,
    timing: PlaceOrderHandleTiming,
    effective_order_type: String,
    token_id: String,
    side: String,
    size: String,
    price: String,
}

#[derive(Debug, Clone, Copy)]
pub struct MarketConstraintSnapshot {
    pub accepting_orders: bool,
    pub active: bool,
    pub closed: bool,
    pub minimum_order_size: rust_decimal::Decimal,
    pub minimum_tick_size: rust_decimal::Decimal,
    pub min_size_shares: rust_decimal::Decimal,
}

#[derive(Debug, Clone)]
pub struct RedeemConditionRequest {
    pub condition_id: String,
    pub outcome_label: String,
}

#[derive(Debug, Clone)]
pub struct MergeConditionRequest {
    pub condition_id: String,
    pub pair_shares: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RewardsConfigEntry {
    #[serde(default, alias = "ratePerDay", alias = "rewardsDailyRate")]
    pub rate_per_day: Option<f64>,
    #[serde(default, alias = "startDate")]
    pub start_date: Option<String>,
    #[serde(default, alias = "endDate")]
    pub end_date: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RewardsMarketEntry {
    pub condition_id: String,
    pub market_slug: String,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub market_competitiveness: Option<f64>,
    #[serde(default)]
    pub rewards_max_spread: Option<f64>,
    #[serde(default)]
    pub rewards_min_size: Option<f64>,
    #[serde(default)]
    pub rewards_config: Vec<RewardsConfigEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct RewardsMarketsApiResponse {
    #[serde(default)]
    data: Vec<RewardsMarketEntry>,
    #[serde(default, alias = "nextCursor")]
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct RemoteBuilderSignerRequest {
    method: String,
    path: String,
    body: String,
    timestamp: String,
}

#[derive(Debug, Deserialize)]
struct RemoteBuilderSignerResponse {
    poly_builder_api_key: String,
    poly_builder_timestamp: String,
    poly_builder_passphrase: String,
    poly_builder_signature: String,
}

impl RewardsMarketEntry {
    pub fn reward_rate_hint(&self) -> f64 {
        self.rewards_config
            .iter()
            .filter_map(|entry| entry.rate_per_day)
            .filter(|v| v.is_finite() && *v > 0.0)
            .fold(0.0_f64, f64::max)
    }
}

#[derive(Debug, Clone, Default)]
struct GammaRewardAccumulator {
    condition_id: String,
    market_slug: String,
    question: Option<String>,
    market_competitiveness: Option<f64>,
    rewards_max_spread: Option<f64>,
    rewards_min_size: Option<f64>,
    max_rate_per_day: f64,
}

#[derive(Debug, Clone)]
pub struct WalletRedeemablePosition {
    pub condition_id: String,
    pub token_id: String,
    pub shares: f64,
}

pub struct PolymarketApi {
    client: Client,
    mm_proxy_clients: Vec<Client>,
    mm_proxy_rr: AtomicUsize,
    mm_proxy_fallback_max: usize,
    mm_read_proxy_attempts: AtomicU64,
    mm_read_direct_attempts: AtomicU64,
    mm_read_proxy_successes: AtomicU64,
    mm_read_direct_successes: AtomicU64,
    mm_read_proxy_429s: AtomicU64,
    mm_read_direct_429s: AtomicU64,
    mm_read_proxy_errors: AtomicU64,
    mm_read_direct_errors: AtomicU64,
    gamma_url: String,
    clob_url: String,
    private_key: Option<String>,
    // Proxy wallet configuration (for Polymarket proxy wallet)
    proxy_wallet_address: Option<String>,
    signature_type: Option<u8>, // 0 = EOA, 1 = Proxy, 2 = GnosisSafe
    // Track if authentication was successful at startup
    authenticated: Arc<tokio::sync::Mutex<bool>>,
    cached_clob_client: Arc<tokio::sync::Mutex<Option<Arc<ClobClientHandle>>>>,
    clob_client_build_lock: Arc<tokio::sync::Mutex<()>>,
    clob_auth_backoff_state: Arc<tokio::sync::Mutex<ClobAuthBackoffState>>,
    cached_usdc_balance_allowance: Arc<tokio::sync::Mutex<Option<(Decimal, Decimal, i64)>>>,
    usdc_balance_allowance_fetch_lock: Arc<tokio::sync::Mutex<()>>,
    prewarmed_order_metadata: Arc<tokio::sync::Mutex<HashSet<String>>>,
    prewarming_order_metadata: Arc<tokio::sync::Mutex<HashSet<String>>>,
    tick_metadata_retry_after_ms_by_token: Arc<tokio::sync::Mutex<HashMap<String, i64>>>,
    tick_metadata_rl_failures_by_token: Arc<tokio::sync::Mutex<HashMap<String, u32>>>,
    tick_metadata_retry_after_ms_global: Arc<tokio::sync::Mutex<i64>>,
    tick_metadata_rl_failures_global: Arc<tokio::sync::Mutex<u32>>,
    order_signer_circuit_state: Arc<tokio::sync::Mutex<OrderSignerCircuitState>>,
    ws_state: Arc<tokio::sync::Mutex<Option<SharedPolymarketWsState>>>,
    ws_enabled: bool,
    ws_market_stale_ms: i64,
    ws_order_stale_ms: i64,
}

impl PolymarketApi {
    fn normalize_post_order_id(order_id: &str) -> Option<String> {
        let trimmed = order_id.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    pub fn new(
        gamma_url: String,
        clob_url: String,
        private_key: Option<String>,
        proxy_wallet_address: Option<String>,
        signature_type: Option<u8>,
    ) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to create HTTP client");
        let mm_proxy_clients = Self::build_mm_proxy_clients();
        let mm_proxy_fallback_max = std::env::var("EVPOLY_MM_PROXY_FALLBACK_MAX")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(2)
            .clamp(0, 8);
        let ws_enabled = Self::env_bool("EVPOLY_PM_WS_ENABLE", true);
        let ws_market_stale_ms = Self::env_i64("EVPOLY_PM_WS_MARKET_STALE_MS", 600).max(250);
        let ws_order_stale_ms = Self::env_i64("EVPOLY_PM_WS_ORDER_STALE_MS", 5_000).max(500);

        Self {
            client,
            mm_proxy_clients,
            mm_proxy_rr: AtomicUsize::new(0),
            mm_proxy_fallback_max,
            mm_read_proxy_attempts: AtomicU64::new(0),
            mm_read_direct_attempts: AtomicU64::new(0),
            mm_read_proxy_successes: AtomicU64::new(0),
            mm_read_direct_successes: AtomicU64::new(0),
            mm_read_proxy_429s: AtomicU64::new(0),
            mm_read_direct_429s: AtomicU64::new(0),
            mm_read_proxy_errors: AtomicU64::new(0),
            mm_read_direct_errors: AtomicU64::new(0),
            gamma_url,
            clob_url,
            private_key,
            proxy_wallet_address,
            signature_type,
            authenticated: Arc::new(tokio::sync::Mutex::new(false)),
            cached_clob_client: Arc::new(tokio::sync::Mutex::new(None)),
            clob_client_build_lock: Arc::new(tokio::sync::Mutex::new(())),
            clob_auth_backoff_state: Arc::new(tokio::sync::Mutex::new(
                ClobAuthBackoffState::default(),
            )),
            cached_usdc_balance_allowance: Arc::new(tokio::sync::Mutex::new(None)),
            usdc_balance_allowance_fetch_lock: Arc::new(tokio::sync::Mutex::new(())),
            prewarmed_order_metadata: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            prewarming_order_metadata: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            tick_metadata_retry_after_ms_by_token: Arc::new(
                tokio::sync::Mutex::new(HashMap::new()),
            ),
            tick_metadata_rl_failures_by_token: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            tick_metadata_retry_after_ms_global: Arc::new(tokio::sync::Mutex::new(0)),
            tick_metadata_rl_failures_global: Arc::new(tokio::sync::Mutex::new(0)),
            order_signer_circuit_state: Arc::new(tokio::sync::Mutex::new(
                OrderSignerCircuitState::default(),
            )),
            ws_state: Arc::new(tokio::sync::Mutex::new(None)),
            ws_enabled,
            ws_market_stale_ms,
            ws_order_stale_ms,
        }
    }

    fn build_mm_proxy_clients() -> Vec<Client> {
        let raw = std::env::var("POLY_MM_PROXIES")
            .ok()
            .map(|v| v.trim().to_string())
            .unwrap_or_default();
        if raw.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for proxy_url in raw
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .take(64)
        {
            match Proxy::all(proxy_url) {
                Ok(proxy) => {
                    match Client::builder()
                        .timeout(std::time::Duration::from_secs(10))
                        .proxy(proxy)
                        .build()
                    {
                        Ok(client) => out.push(client),
                        Err(e) => {
                            warn!(
                                "Ignoring invalid POLY_MM_PROXIES client ({}): {}",
                                proxy_url, e
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "Ignoring invalid POLY_MM_PROXIES entry ({}): {}",
                        proxy_url, e
                    );
                }
            }
        }
        out
    }

    fn mm_read_clients_order(&self) -> Vec<(&Client, bool)> {
        let fallback_count = self.mm_proxy_fallback_max.min(self.mm_proxy_clients.len());
        if fallback_count == 0 {
            return vec![(&self.client, false)];
        }
        let len = self.mm_proxy_clients.len();
        let start = self.mm_proxy_rr.fetch_add(1, Ordering::Relaxed) % len;
        // Direct first for speed; proxies are fallback for non-trading reads only.
        let mut clients = Vec::with_capacity(fallback_count + 1);
        clients.push((&self.client, false));
        for idx in 0..fallback_count {
            clients.push((&self.mm_proxy_clients[(start + idx) % len], true));
        }
        clients
    }

    async fn send_readonly_get_with_proxy_fallback<F>(
        &self,
        mut build_request: F,
    ) -> Result<reqwest::Response>
    where
        F: FnMut(&Client) -> reqwest::RequestBuilder,
    {
        let read_timeout_ms = std::env::var("EVPOLY_MM_READ_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(2_500)
            .clamp(250, 15_000);
        let retry_base_ms = std::env::var("EVPOLY_MM_READ_RETRY_BASE_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(120)
            .clamp(10, 5_000);
        let retry_jitter_ms = std::env::var("EVPOLY_MM_READ_RETRY_JITTER_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(100)
            .clamp(0, 5_000);

        let clients = self.mm_read_clients_order();
        let mut last_err: Option<anyhow::Error> = None;
        for (idx, (client, via_proxy)) in clients.iter().enumerate() {
            if *via_proxy {
                self.mm_read_proxy_attempts.fetch_add(1, Ordering::Relaxed);
            } else {
                self.mm_read_direct_attempts.fetch_add(1, Ordering::Relaxed);
            }
            let jitter_seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let retry_jitter = if retry_jitter_ms > 0 {
                jitter_seed.wrapping_add(idx as u64) % (retry_jitter_ms.saturating_add(1))
            } else {
                0
            };
            match build_request(client)
                .timeout(std::time::Duration::from_millis(read_timeout_ms))
                .send()
                .await
            {
                Ok(response) => {
                    if response.status().as_u16() == 429 && idx + 1 < clients.len() {
                        if *via_proxy {
                            self.mm_read_proxy_429s.fetch_add(1, Ordering::Relaxed);
                        } else {
                            self.mm_read_direct_429s.fetch_add(1, Ordering::Relaxed);
                        }
                        last_err = Some(anyhow::anyhow!("429 Too Many Requests"));
                        let sleep_ms = retry_base_ms.saturating_add(retry_jitter);
                        tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
                        continue;
                    }
                    if response.status().as_u16() == 429 {
                        if *via_proxy {
                            self.mm_read_proxy_429s.fetch_add(1, Ordering::Relaxed);
                        } else {
                            self.mm_read_direct_429s.fetch_add(1, Ordering::Relaxed);
                        }
                    } else if *via_proxy {
                        self.mm_read_proxy_successes.fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.mm_read_direct_successes
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    return Ok(response);
                }
                Err(e) => {
                    if *via_proxy {
                        self.mm_read_proxy_errors.fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.mm_read_direct_errors.fetch_add(1, Ordering::Relaxed);
                    }
                    last_err = Some(anyhow::anyhow!(e.to_string()));
                    if idx + 1 < clients.len() {
                        let sleep_ms = retry_base_ms.saturating_add(retry_jitter);
                        tokio::time::sleep(std::time::Duration::from_millis(sleep_ms)).await;
                        continue;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all read clients failed")))
    }

    pub fn mm_read_telemetry_snapshot(&self) -> MmReadTelemetrySnapshot {
        MmReadTelemetrySnapshot {
            proxy_attempts: self.mm_read_proxy_attempts.load(Ordering::Relaxed),
            direct_attempts: self.mm_read_direct_attempts.load(Ordering::Relaxed),
            proxy_successes: self.mm_read_proxy_successes.load(Ordering::Relaxed),
            direct_successes: self.mm_read_direct_successes.load(Ordering::Relaxed),
            proxy_429s: self.mm_read_proxy_429s.load(Ordering::Relaxed),
            direct_429s: self.mm_read_direct_429s.load(Ordering::Relaxed),
            proxy_errors: self.mm_read_proxy_errors.load(Ordering::Relaxed),
            direct_errors: self.mm_read_direct_errors.load(Ordering::Relaxed),
        }
    }

    fn env_nonempty(key: &str) -> Option<String> {
        std::env::var(key).ok().and_then(|v| {
            let trimmed = v.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
    }

    fn env_bool(key: &str, default: bool) -> bool {
        std::env::var(key)
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(default)
    }

    fn env_i64(key: &str, default: i64) -> i64 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(default)
    }

    fn data_api_base_url() -> String {
        std::env::var("EVPOLY_POLY_DATA_API_URL")
            .ok()
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "https://data-api.polymarket.com".to_string())
    }

    fn market_constraints_retry_attempts() -> u32 {
        std::env::var("EVPOLY_MARKET_CONSTRAINTS_RETRY_ATTEMPTS")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(2)
            .clamp(1, 6)
    }

    fn market_constraints_retry_base_ms() -> u64 {
        std::env::var("EVPOLY_MARKET_CONSTRAINTS_RETRY_BASE_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(120)
            .clamp(25, 5_000)
    }

    fn wallet_positions_retry_attempts() -> u32 {
        std::env::var("EVPOLY_WALLET_POSITIONS_RETRY_ATTEMPTS")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(3)
            .clamp(1, 6)
    }

    fn wallet_positions_retry_base_ms() -> u64 {
        std::env::var("EVPOLY_WALLET_POSITIONS_RETRY_BASE_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(200)
            .clamp(25, 5_000)
    }

    fn parse_decimal_field(value: Option<&Value>) -> Option<rust_decimal::Decimal> {
        match value {
            Some(Value::Number(n)) => rust_decimal::Decimal::from_str(&n.to_string()).ok(),
            Some(Value::String(s)) => rust_decimal::Decimal::from_str(s.trim()).ok(),
            _ => None,
        }
    }

    fn ensure_no_local_builder_secrets(&self) -> Result<()> {
        for key in [
            "POLY_BUILDER_API_KEY",
            "POLY_BUILDER_API_SECRET",
            "POLY_BUILDER_API_PASSPHRASE",
        ] {
            if Self::env_nonempty(key).is_some() {
                anyhow::bail!(
                    "{} is not supported on poly-remote. Use remote signer onboarding token only.",
                    key
                );
            }
        }
        Ok(())
    }

    fn submit_signer_token(&self) -> Result<String> {
        self.ensure_no_local_builder_secrets()?;
        let signer_token = Self::env_nonempty("EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN")
            .ok_or_else(|| anyhow::anyhow!("EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN is required"))?;
        Ok(signer_token)
    }

    fn relayer_api_key_primary_credentials(&self) -> Result<Option<(String, String)>> {
        let api_key = Self::env_nonempty("RELAYER_API_KEY");
        let api_key_address = Self::env_nonempty("RELAYER_API_KEY_ADDRESS");
        match (api_key, api_key_address) {
            (None, None) => Ok(None),
            (Some(_), None) | (None, Some(_)) => anyhow::bail!(
                "RELAYER_API_KEY and RELAYER_API_KEY_ADDRESS must both be set for relayer primary submit"
            ),
            (Some(key), Some(address_raw)) => {
                let parsed = Self::parse_hex_address(address_raw.as_str()).with_context(|| {
                    format!(
                        "invalid RELAYER_API_KEY_ADDRESS for relayer primary submit: {}",
                        address_raw
                    )
                })?;
                Ok(Some((key, format!("{:#x}", parsed))))
            }
        }
    }

    fn order_primary_signer_url(&self) -> String {
        Self::env_nonempty("EVPOLY_ORDER_SIGNER_PRIMARY_URL")
            .unwrap_or_else(|| ORDER_SIGNER_PRIMARY_URL_DEFAULT.to_string())
    }

    fn order_primary_signer_token(&self) -> Option<String> {
        Self::env_nonempty("EVPOLY_ORDER_SIGNER_PRIMARY_TOKEN")
    }

    fn order_fallback_signer_url(&self) -> String {
        Self::env_nonempty("EVPOLY_ORDER_SIGNER_FALLBACK_URL")
            .unwrap_or_else(|| ORDER_SIGNER_AWS_FALLBACK_URL_DEFAULT.to_string())
    }

    fn submit_signer_url(&self) -> String {
        Self::env_nonempty("EVPOLY_SUBMIT_SIGNER_URL")
            .unwrap_or_else(|| SUBMIT_SIGNER_AWS_URL_DEFAULT.to_string())
    }

    fn order_primary_builder_config(&self) -> Result<BuilderConfig> {
        self.ensure_no_local_builder_secrets()?;
        let signer_url = self.order_primary_signer_url();
        let signer_token = self.order_primary_signer_token();
        BuilderConfig::remote(signer_url.as_str(), signer_token)
            .context("Invalid order primary signer URL (EVPOLY_ORDER_SIGNER_PRIMARY_URL)")
    }

    fn order_fallback_builder_config(&self) -> Result<BuilderConfig> {
        self.ensure_no_local_builder_secrets()?;
        let fallback_url = self.order_fallback_signer_url();
        BuilderConfig::remote(fallback_url.as_str(), None)
            .context("Invalid order fallback signer URL (EVPOLY_ORDER_SIGNER_FALLBACK_URL)")
    }

    async fn should_force_aws_order_signer(&self) -> bool {
        let now = Instant::now();
        let mut state = self.order_signer_circuit_state.lock().await;
        match state.force_aws_until {
            Some(until) if now < until => true,
            Some(_) => {
                state.force_aws_until = None;
                state.degraded_since = None;
                warn!("Order signer fallback window ended; retrying primary signer");
                false
            }
            None => false,
        }
    }

    async fn note_primary_order_signer_success(&self) {
        let mut state = self.order_signer_circuit_state.lock().await;
        state.degraded_since = None;
    }

    async fn note_primary_order_signer_timeout(&self) {
        let now = Instant::now();
        let mut state = self.order_signer_circuit_state.lock().await;
        if let Some(until) = state.force_aws_until {
            if now < until {
                return;
            }
            state.force_aws_until = None;
        }
        let degraded_since = state.degraded_since.get_or_insert(now);
        if now.duration_since(*degraded_since) >= ORDER_SIGNER_DEGRADED_WINDOW {
            state.force_aws_until = Some(now + ORDER_SIGNER_AWS_FORCE_WINDOW);
            state.degraded_since = None;
            warn!(
                "Primary order signer exceeded {}ms for >= {}s; forcing AWS signer for {}s",
                ORDER_SIGNER_PRIMARY_FALLBACK_TIMEOUT_MS,
                ORDER_SIGNER_DEGRADED_WINDOW.as_secs(),
                ORDER_SIGNER_AWS_FORCE_WINDOW.as_secs()
            );
        }
    }

    async fn request_remote_builder_headers_via(
        &self,
        sign_url: &str,
        signer_token: Option<&str>,
        method: &str,
        path: &str,
        body: &str,
        timeout_ms: Option<u64>,
    ) -> Result<RemoteBuilderSignerResponse> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();

        let payload = RemoteBuilderSignerRequest {
            method: method.to_ascii_uppercase(),
            path: path.to_string(),
            body: body.to_string(),
            timestamp,
        };
        let mut request = self.client.post(sign_url);
        request = request
            .header("User-Agent", "polymarket-trading-bot/1.0")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&payload);
        if let Some(ms) = timeout_ms {
            request = request.timeout(std::time::Duration::from_millis(ms));
        }
        if let Some(token) = signer_token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .context("Failed to call EVPOLY remote builder signer")?;

        let status = response.status();
        let body_text = response
            .text()
            .await
            .context("Failed to read remote builder signer response")?;
        if !status.is_success() {
            anyhow::bail!(
                "Remote builder signer rejected request (status {}): {}",
                status,
                &body_text[..200.min(body_text.len())]
            );
        }

        serde_json::from_str::<RemoteBuilderSignerResponse>(&body_text).context(
            "Failed to parse remote builder signer response (expected poly_builder_* fields)",
        )
    }

    async fn request_remote_builder_headers(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<RemoteBuilderSignerResponse> {
        let normalized_method = method.to_ascii_uppercase();
        let order_primary_url = self.order_primary_signer_url();
        let order_primary_token = self.order_primary_signer_token();
        let order_fallback_url = self.order_fallback_signer_url();
        let submit_signer_url = self.submit_signer_url();
        match path {
            "/submit" => {
                let submit_token = self.submit_signer_token()?;
                self.request_remote_builder_headers_via(
                    submit_signer_url.as_str(),
                    Some(submit_token.as_str()),
                    normalized_method.as_str(),
                    path,
                    body,
                    None,
                )
                .await
            }
            "/order" | "/orders" => {
                if self.should_force_aws_order_signer().await {
                    return self
                        .request_remote_builder_headers_via(
                            order_fallback_url.as_str(),
                            None,
                            normalized_method.as_str(),
                            path,
                            body,
                            None,
                        )
                        .await;
                }
                match self
                    .request_remote_builder_headers_via(
                        order_primary_url.as_str(),
                        order_primary_token.as_deref(),
                        normalized_method.as_str(),
                        path,
                        body,
                        Some(ORDER_SIGNER_PRIMARY_FALLBACK_TIMEOUT_MS),
                    )
                    .await
                {
                    Ok(resp) => {
                        self.note_primary_order_signer_success().await;
                        Ok(resp)
                    }
                    Err(primary_err) => {
                        if Self::is_timeout_error(&primary_err) {
                            self.note_primary_order_signer_timeout().await;
                        }
                        if Self::should_fallback_to_aws_signer(&primary_err) {
                            warn!(
                                "Primary order signer transient failure; falling back to AWS signer. path={} err={}",
                                path, primary_err
                            );
                            return self
                                .request_remote_builder_headers_via(
                                    order_fallback_url.as_str(),
                                    None,
                                    normalized_method.as_str(),
                                    path,
                                    body,
                                    None,
                                )
                                .await;
                        }
                        if Self::is_rate_limit_error(&primary_err) {
                            warn!(
                                "Primary order signer rate-limited; skipping AWS fallback and letting caller back off. path={} err={}",
                                path, primary_err
                            );
                        } else {
                            warn!(
                                "Primary order signer failed without fallback condition; skipping AWS fallback. path={} err={}",
                                path, primary_err
                            );
                        }
                        Err(primary_err)
                    }
                }
            }
            _ => anyhow::bail!("Unsupported remote signer path: {}", path),
        }
    }

    fn order_attribution_builder_config(&self) -> Result<BuilderConfig> {
        self.order_primary_builder_config().context(
            "Builder attribution requires configured order signer endpoints on poly-remote.",
        )
    }

    fn order_attribution_builder_fallback_config(&self) -> Result<BuilderConfig> {
        self.order_fallback_builder_config()
            .context("Failed to build fallback builder attribution config")
    }

    fn parse_hex_address(address: &str) -> Result<AlloyAddress> {
        let clean = address.strip_prefix("0x").unwrap_or(address);
        let bytes = hex::decode(clean)
            .with_context(|| format!("Failed to decode hex address: {}", address))?;
        if bytes.len() != 20 {
            anyhow::bail!(
                "Invalid address length for {}: expected 20 bytes, got {}",
                address,
                bytes.len()
            );
        }
        Ok(AlloyAddress::from_slice(&bytes))
    }

    fn parse_u256_id(raw: &str, label: &str) -> Result<U256> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            anyhow::bail!("{} is empty", label);
        }
        U256::from_str(trimmed)
            .with_context(|| format!("Failed to parse {} as U256: {}", label, raw))
    }

    fn derive_proxy_wallet_address(
        owner: AlloyAddress,
        proxy_factory: AlloyAddress,
    ) -> Result<AlloyAddress> {
        // builder-relayer-client constant (POL chain).
        const PROXY_INIT_CODE_HASH: &str =
            "0xd21df8dc65880a8606f09fe0ce3df9b8869287ab0b058be05aa9e8af6330a00b";

        let init_hash = B256::from_str(
            PROXY_INIT_CODE_HASH
                .strip_prefix("0x")
                .unwrap_or(PROXY_INIT_CODE_HASH),
        )
        .context("Failed to parse PROXY_INIT_CODE_HASH")?;
        // JS SDK: keccak256(encodePacked(["address"], [owner]))
        let salt = keccak256(owner.as_slice());

        // CREATE2: keccak256(0xff ++ factory ++ salt ++ init_code_hash)[12:]
        let mut preimage = Vec::with_capacity(1 + 20 + 32 + 32);
        preimage.push(0xff);
        preimage.extend_from_slice(proxy_factory.as_slice());
        preimage.extend_from_slice(salt.as_slice());
        preimage.extend_from_slice(init_hash.as_slice());

        let create2_hash = keccak256(preimage);
        Ok(AlloyAddress::from_slice(&create2_hash.as_slice()[12..]))
    }

    fn build_proxy_struct_hash(
        from: AlloyAddress,
        to: AlloyAddress,
        data_hex: &str,
        gas_limit: u64,
        nonce: u64,
        relay_hub: AlloyAddress,
        relay: AlloyAddress,
    ) -> Result<B256> {
        // builder-relayer-client (proxy):
        // keccak256("rlx:" ++ from ++ to ++ data ++ txFee(32) ++ gasPrice(32) ++ gasLimit(32) ++ nonce(32) ++ relayHub ++ relay)
        let data = hex::decode(data_hex.strip_prefix("0x").unwrap_or(data_hex))
            .context("Failed to decode proxy data hex for struct hash")?;

        let mut payload = Vec::with_capacity(4 + 20 + 20 + data.len() + 32 * 4 + 20 + 20);
        payload.extend_from_slice(b"rlx:");
        payload.extend_from_slice(from.as_slice());
        payload.extend_from_slice(to.as_slice());
        payload.extend_from_slice(&data);
        payload.extend_from_slice(&U256::ZERO.to_be_bytes::<32>()); // txFee / relayerFee
        payload.extend_from_slice(&U256::ZERO.to_be_bytes::<32>()); // gasPrice
        payload.extend_from_slice(&U256::from(gas_limit).to_be_bytes::<32>());
        payload.extend_from_slice(&U256::from(nonce).to_be_bytes::<32>());
        payload.extend_from_slice(relay_hub.as_slice());
        payload.extend_from_slice(relay.as_slice());

        Ok(keccak256(payload))
    }

    fn polygon_rpc_http_urls() -> Vec<String> {
        let mut urls = Vec::new();
        if let Some(primary) = Self::env_nonempty("POLY_POLYGON_RPC_HTTP_URL") {
            urls.push(primary);
        }
        if let Some(fallback) = Self::env_nonempty("POLY_POLYGON_RPC_HTTP_FALLBACK_URL") {
            if !urls
                .iter()
                .any(|v| v.eq_ignore_ascii_case(fallback.as_str()))
            {
                urls.push(fallback);
            }
        }
        if urls.is_empty() {
            urls.push("https://polygon-rpc.com".to_string());
        }
        urls
    }

    async fn estimate_proxy_relayer_gas_limit(
        &self,
        from: AlloyAddress,
        to: AlloyAddress,
        data_hex: &str,
    ) -> Result<u64> {
        let from_hex = format!("{:#x}", from);
        let to_hex = format!("{:#x}", to);
        let mut last_error: Option<anyhow::Error> = None;

        for rpc_url in Self::polygon_rpc_http_urls() {
            let estimate_req = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_estimateGas",
                "params": [{
                    "from": from_hex,
                    "to": to_hex,
                    "value": "0x0",
                    "data": data_hex,
                }]
            });

            let resp = match self
                .client
                .post(rpc_url.as_str())
                .json(&estimate_req)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    last_error = Some(
                        anyhow::Error::new(e)
                            .context(format!("eth_estimateGas request failed via {}", rpc_url)),
                    );
                    continue;
                }
            };
            let status = resp.status();
            let text = match resp.text().await {
                Ok(text) => text,
                Err(e) => {
                    last_error = Some(anyhow::Error::new(e).context(format!(
                        "Failed to read eth_estimateGas response via {}",
                        rpc_url
                    )));
                    continue;
                }
            };
            if !status.is_success() {
                last_error = Some(anyhow::anyhow!(
                    "eth_estimateGas HTTP {} via {}: {}",
                    status,
                    rpc_url,
                    text
                ));
                continue;
            }
            let payload: serde_json::Value = match serde_json::from_str(text.as_str()) {
                Ok(payload) => payload,
                Err(e) => {
                    last_error = Some(anyhow::Error::new(e).context(format!(
                        "Failed to parse eth_estimateGas JSON via {}",
                        rpc_url
                    )));
                    continue;
                }
            };
            if let Some(err) = payload.get("error") {
                last_error = Some(anyhow::anyhow!(
                    "eth_estimateGas returned error via {}: {}",
                    rpc_url,
                    err
                ));
                continue;
            }
            let Some(raw_hex) = payload.get("result").and_then(|v| v.as_str()) else {
                last_error = Some(anyhow::anyhow!(
                    "eth_estimateGas missing result field via {}",
                    rpc_url
                ));
                continue;
            };
            let base_gas = match u64::from_str_radix(raw_hex.trim_start_matches("0x"), 16) {
                Ok(base_gas) => base_gas,
                Err(e) => {
                    last_error = Some(anyhow::Error::new(e).context(format!(
                        "Failed to parse eth_estimateGas result '{}' via {}",
                        raw_hex, rpc_url
                    )));
                    continue;
                }
            };
            // Keep the signed gas value realistic but with headroom.
            // Oversized gas values can trip RelayHub's gas-left precheck.
            let with_buffer = base_gas.saturating_mul(12).saturating_div(10);
            return Ok(with_buffer.clamp(120_000, 2_000_000));
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!(
                "Failed to estimate proxy relayer gas: no usable Polygon RPC endpoints available"
            )
        }))
    }

    async fn build_proxy_relayer_request(
        &self,
        target_contract: AlloyAddress,
        call_data_hex: &str,
        metadata: &str,
    ) -> Result<serde_json::Value> {
        self.build_relayer_request_for_calls(
            vec![(target_contract, call_data_hex.to_string())],
            metadata,
        )
        .await
    }

    fn resolve_relayer_wallet_mode(&self) -> Result<&'static str> {
        match self.signature_type {
            Some(2) => Ok("SAFE"),
            Some(1) => Ok("PROXY"),
            Some(0) => anyhow::bail!(
                "Relayer redeem/merge is not supported when POLY_SIGNATURE_TYPE=0 (EOA). Use 1 (Proxy) or 2 (Safe)."
            ),
            Some(other) => anyhow::bail!("Unsupported signature_type for relayer flow: {}", other),
            None => Ok("PROXY"),
        }
    }

    fn configured_funder_wallet_address(&self) -> Result<Option<AlloyAddress>> {
        match self.proxy_wallet_address.as_deref().map(str::trim) {
            Some(raw) if !raw.is_empty() => Ok(Some(
                AlloyAddress::parse_checksummed(raw, None).with_context(|| {
                    format!(
                        "Failed to parse proxy_wallet_address for relayer request: {}",
                        raw
                    )
                })?,
            )),
            _ => Ok(None),
        }
    }

    fn resolve_safe_wallet_address(&self, owner: AlloyAddress) -> Result<AlloyAddress> {
        if let Some(configured) = self.configured_funder_wallet_address()? {
            return Ok(configured);
        }
        let derived = derive_safe_wallet(owner, POLYGON).ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to derive safe wallet address for owner {:#x} on chain {}",
                owner,
                POLYGON
            )
        })?;
        Ok(derived)
    }

    fn resolve_proxy_wallet_address(
        &self,
        owner: AlloyAddress,
        proxy_factory: AlloyAddress,
    ) -> Result<AlloyAddress> {
        if let Some(configured) = self.configured_funder_wallet_address()? {
            return Ok(configured);
        }
        Self::derive_proxy_wallet_address(owner, proxy_factory)
    }

    fn parse_decimal_u256(raw: &str, label: &str) -> Result<U256> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            anyhow::bail!("{} is empty", label);
        }
        if trimmed.starts_with("0x") || trimmed.starts_with("0X") {
            U256::from_str(trimmed)
                .with_context(|| format!("Failed to parse {} as hex U256: {}", label, raw))
        } else {
            U256::from_str_radix(trimmed, 10)
                .with_context(|| format!("Failed to parse {} as decimal U256: {}", label, raw))
        }
    }

    fn pad_address_32(addr: AlloyAddress) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[12..].copy_from_slice(addr.as_slice());
        out
    }

    fn safe_domain_separator(chain_id: u64, verifying_contract: AlloyAddress) -> B256 {
        let domain_typehash =
            keccak256("EIP712Domain(uint256 chainId,address verifyingContract)".as_bytes());
        let mut payload = Vec::with_capacity(32 * 3);
        payload.extend_from_slice(domain_typehash.as_slice());
        payload.extend_from_slice(&U256::from(chain_id).to_be_bytes::<32>());
        payload.extend_from_slice(&Self::pad_address_32(verifying_contract));
        keccak256(payload)
    }

    fn safe_tx_struct_hash(
        to: AlloyAddress,
        data_hex: &str,
        operation: u8,
        nonce: U256,
    ) -> Result<B256> {
        let safe_tx_typehash = keccak256(
            "SafeTx(address to,uint256 value,bytes data,uint8 operation,uint256 safeTxGas,uint256 baseGas,uint256 gasPrice,address gasToken,address refundReceiver,uint256 nonce)"
                .as_bytes(),
        );
        let data = hex::decode(data_hex.strip_prefix("0x").unwrap_or(data_hex))
            .context("Failed to decode SAFE transaction call data")?;
        let data_hash = keccak256(data);
        let zero_address = AlloyAddress::ZERO;

        let mut payload = Vec::with_capacity(32 * 11);
        payload.extend_from_slice(safe_tx_typehash.as_slice());
        payload.extend_from_slice(&Self::pad_address_32(to));
        payload.extend_from_slice(&U256::ZERO.to_be_bytes::<32>()); // value
        payload.extend_from_slice(data_hash.as_slice()); // bytes data hashed
        payload.extend_from_slice(&U256::from(operation).to_be_bytes::<32>());
        payload.extend_from_slice(&U256::ZERO.to_be_bytes::<32>()); // safeTxGas
        payload.extend_from_slice(&U256::ZERO.to_be_bytes::<32>()); // baseGas
        payload.extend_from_slice(&U256::ZERO.to_be_bytes::<32>()); // gasPrice
        payload.extend_from_slice(&Self::pad_address_32(zero_address)); // gasToken
        payload.extend_from_slice(&Self::pad_address_32(zero_address)); // refundReceiver
        payload.extend_from_slice(&nonce.to_be_bytes::<32>());
        Ok(keccak256(payload))
    }

    fn safe_eip712_hash(chain_id: u64, safe_wallet: AlloyAddress, tx_struct_hash: B256) -> B256 {
        let domain_separator = Self::safe_domain_separator(chain_id, safe_wallet);
        let mut payload = Vec::with_capacity(2 + 32 + 32);
        payload.extend_from_slice(&[0x19, 0x01]);
        payload.extend_from_slice(domain_separator.as_slice());
        payload.extend_from_slice(tx_struct_hash.as_slice());
        keccak256(payload)
    }

    fn pack_safe_signature_for_relayer(signature_hex: &str) -> Result<String> {
        let bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap_or(signature_hex))
            .context("Failed to decode SAFE signature hex")?;
        if bytes.len() != 65 {
            anyhow::bail!(
                "Invalid SAFE signature length: expected 65 bytes, got {}",
                bytes.len()
            );
        }
        let mut packed = bytes;
        let v = packed[64];
        packed[64] = match v {
            0 | 1 => v + 31,
            27 | 28 => v + 4,
            _ => anyhow::bail!("Invalid SAFE signature recovery id: {}", v),
        };
        Ok(format!("0x{}", hex::encode(packed)))
    }

    fn encode_safe_multisend_call(
        &self,
        calls: &[(AlloyAddress, String)],
    ) -> Result<(AlloyAddress, String, u8)> {
        const SAFE_MULTISEND: &str = "0xA238CBeb142c10Ef7Ad8442C6D1f9E89e07e7761";
        if calls.is_empty() {
            anyhow::bail!("SAFE multisend requires at least one call");
        }
        if calls.len() == 1 {
            return Ok((calls[0].0, calls[0].1.clone(), 0));
        }

        let mut transactions = Vec::new();
        for (target, call_data_hex) in calls {
            let call_data = hex::decode(call_data_hex.strip_prefix("0x").unwrap_or(call_data_hex))
                .context("Failed to decode SAFE inner call data")?;
            transactions.push(0u8); // OperationType.Call
            transactions.extend_from_slice(target.as_slice()); // address (20 bytes)
            transactions.extend_from_slice(&U256::ZERO.to_be_bytes::<32>()); // value
            transactions.extend_from_slice(&U256::from(call_data.len()).to_be_bytes::<32>());
            transactions.extend_from_slice(&call_data);
        }

        let selector = Self::function_selector("multiSend(bytes)");
        let mut payload = Vec::new();
        payload.extend_from_slice(&selector);
        payload.extend_from_slice(&U256::from(32).to_be_bytes::<32>()); // offset
        payload.extend_from_slice(&U256::from(transactions.len()).to_be_bytes::<32>()); // bytes length
        payload.extend_from_slice(&transactions);
        while payload.len() % 32 != 0 {
            payload.push(0);
        }

        let multisend = Self::parse_hex_address(SAFE_MULTISEND)?;
        Ok((multisend, format!("0x{}", hex::encode(payload)), 1))
    }

    async fn fetch_relayer_nonce(&self, address: &str, signer_type: &str) -> Result<String> {
        const RELAYER_NONCE_URL: &str = "https://relayer-v2.polymarket.com/nonce";
        let resp = self
            .client
            .get(RELAYER_NONCE_URL)
            .query(&[("address", address), ("type", signer_type)])
            .send()
            .await
            .context("Failed to request relayer nonce")?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .context("Failed to read relayer nonce response")?;
        if !status.is_success() {
            anyhow::bail!("Relayer nonce request failed (status {}): {}", status, text);
        }
        let payload: serde_json::Value =
            serde_json::from_str(text.as_str()).context("Failed to parse relayer nonce JSON")?;
        payload
            .get("nonce")
            .and_then(|v| {
                v.as_str()
                    .map(|s| s.to_string())
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .ok_or_else(|| anyhow::anyhow!("Relayer nonce response missing nonce: {}", payload))
    }

    async fn build_safe_relayer_request_for_calls(
        &self,
        calls: Vec<(AlloyAddress, String)>,
        metadata: &str,
    ) -> Result<serde_json::Value> {
        let private_key = self
            .private_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Private key required to build SAFE relayer request"))?;
        let signer = PrivateKeySigner::from_str(private_key)
            .context("Failed to parse private key for SAFE relayer signing")?;
        let from = signer.address();
        let from_hex = format!("{:#x}", from);
        let safe_wallet = self.resolve_safe_wallet_address(from)?;
        let (safe_to, safe_data_hex, safe_operation) = self.encode_safe_multisend_call(&calls)?;
        let nonce_str = self.fetch_relayer_nonce(from_hex.as_str(), "SAFE").await?;
        let nonce_u256 = Self::parse_decimal_u256(nonce_str.as_str(), "relayer safe nonce")?;

        let tx_struct_hash =
            Self::safe_tx_struct_hash(safe_to, safe_data_hex.as_str(), safe_operation, nonce_u256)?;
        let safe_eip712_hash = Self::safe_eip712_hash(POLYGON, safe_wallet, tx_struct_hash);
        let sig = signer
            .sign_message(safe_eip712_hash.as_slice())
            .await
            .context("Failed to sign SAFE relayer struct hash")?;
        let packed_sig = Self::pack_safe_signature_for_relayer(sig.to_string().as_str())?;

        Ok(serde_json::json!({
            "from": from_hex,
            "to": format!("{:#x}", safe_to),
            "proxyWallet": format!("{:#x}", safe_wallet),
            "data": safe_data_hex,
            "nonce": nonce_str,
            "signature": packed_sig,
            "signatureParams": {
                "gasPrice": "0",
                "operation": safe_operation.to_string(),
                "safeTxnGas": "0",
                "baseGas": "0",
                "gasToken": format!("{:#x}", AlloyAddress::ZERO),
                "refundReceiver": format!("{:#x}", AlloyAddress::ZERO),
            },
            "type": "SAFE",
            "metadata": metadata
        }))
    }

    async fn build_proxy_relayer_request_for_calls(
        &self,
        calls: Vec<(AlloyAddress, String)>,
        metadata: &str,
    ) -> Result<serde_json::Value> {
        // POL chain config from builder-relayer-client.
        const RELAYER_PAYLOAD_URL: &str = "https://relayer-v2.polymarket.com/relay-payload";
        const PROXY_FACTORY: &str = "0xaB45c5A4B0c941a2F231C04C3f49182e1A254052";
        const RELAY_HUB: &str = "0xD216153c06E857cD7f72665E0aF1d7D82172F494";
        const FALLBACK_GAS_LIMIT: u64 = 500_000;
        if calls.is_empty() {
            anyhow::bail!("Proxy relayer request requires at least one call");
        }

        let private_key = self.private_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Private key required to build PROXY relayer request")
        })?;
        let signer = PrivateKeySigner::from_str(private_key)
            .context("Failed to parse private key for PROXY relayer signing")?;
        let from = signer.address();
        let from_hex = format!("{:#x}", from);

        let proxy_factory = Self::parse_hex_address(PROXY_FACTORY)?;
        let relay_hub = Self::parse_hex_address(RELAY_HUB)?;
        let proxy_wallet = self.resolve_proxy_wallet_address(from, proxy_factory)?;

        let mut proxy_calls: Vec<ProxyCall> = Vec::with_capacity(calls.len());
        for (target_contract, call_data_hex) in calls {
            let call_data_bytes = hex::decode(
                call_data_hex
                    .strip_prefix("0x")
                    .unwrap_or(call_data_hex.as_str()),
            )
            .context("Failed to decode target call data")?;
            proxy_calls.push(ProxyCall {
                typeCode: 1u8,
                to: target_contract,
                value: U256::ZERO,
                data: call_data_bytes.into(),
            });
        }

        // Encode ProxyFactory.proxy([Call]) where Call = (typeCode=1, to, value=0, data).
        let proxy_data = IProxyWalletFactory::proxyCall { calls: proxy_calls }.abi_encode();
        let proxy_data_hex = format!("0x{}", hex::encode(proxy_data));
        let estimated_gas_limit = match self
            .estimate_proxy_relayer_gas_limit(from, proxy_factory, proxy_data_hex.as_str())
            .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "Proxy relayer gas estimation failed; falling back to {} gas: {}",
                    FALLBACK_GAS_LIMIT, e
                );
                FALLBACK_GAS_LIMIT
            }
        };

        let relay_payload_resp = self
            .client
            .get(RELAYER_PAYLOAD_URL)
            .query(&[("address", from_hex.as_str()), ("type", "PROXY")])
            .send()
            .await
            .context("Failed to request relayer payload")?;
        let relay_payload_status = relay_payload_resp.status();
        let relay_payload_text = relay_payload_resp
            .text()
            .await
            .context("Failed to read relayer payload response")?;
        if !relay_payload_status.is_success() {
            anyhow::bail!(
                "Relayer payload request failed (status {}): {}",
                relay_payload_status,
                relay_payload_text
            );
        }

        let relay_payload: serde_json::Value = serde_json::from_str(&relay_payload_text)
            .context("Failed to parse relayer payload JSON")?;
        let relay_address_str = relay_payload
            .get("address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Relayer payload missing address"))?;
        let relay_nonce = relay_payload
            .get("nonce")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
            .ok_or_else(|| anyhow::anyhow!("Relayer payload missing/invalid nonce"))?;
        let relay_address = Self::parse_hex_address(relay_address_str)?;

        let struct_hash = Self::build_proxy_struct_hash(
            from,
            proxy_factory,
            proxy_data_hex.as_str(),
            estimated_gas_limit,
            relay_nonce,
            relay_hub,
            relay_address,
        )?;
        // Match polymarket builder-relayer-client behavior:
        // signMessage(bytes32 structHash), i.e. EIP-191 prefixed personal_sign over bytes.
        let signature = signer
            .sign_message(struct_hash.as_slice())
            .await
            .context("Failed to sign proxy relayer struct hash")?;

        Ok(serde_json::json!({
            "from": from_hex,
            "to": format!("{:#x}", proxy_factory),
            "proxyWallet": format!("{:#x}", proxy_wallet),
            "data": proxy_data_hex,
            "nonce": relay_nonce.to_string(),
            "signature": signature.to_string(),
            "signatureParams": {
                "gasPrice": "0",
                "relayerFee": "0",
                "gasLimit": estimated_gas_limit.to_string(),
                "relayHub": format!("{:#x}", relay_hub),
                "relay": format!("{:#x}", relay_address)
            },
            "type": "PROXY",
            "metadata": metadata
        }))
    }

    async fn build_relayer_request_for_calls(
        &self,
        calls: Vec<(AlloyAddress, String)>,
        metadata: &str,
    ) -> Result<serde_json::Value> {
        match self.resolve_relayer_wallet_mode()? {
            "SAFE" => {
                self.build_safe_relayer_request_for_calls(calls, metadata)
                    .await
            }
            _ => {
                self.build_proxy_relayer_request_for_calls(calls, metadata)
                    .await
            }
        }
    }

    /// Authenticate with Polymarket CLOB API at startup
    /// This verifies credentials (private_key + API credentials)
    /// Equivalent to JavaScript: new ClobClient(HOST, CHAIN_ID, signer, apiCreds, signatureType, funderAddress)
    pub async fn authenticate(&self) -> Result<()> {
        let _handle = self.get_or_create_clob_client().await?;
        *self.authenticated.lock().await = true;

        eprintln!("✅ Successfully authenticated with Polymarket CLOB API");
        eprintln!("   ✓ Private key: Valid");
        eprintln!("   ✓ API credentials: Valid");
        if let Some(proxy_addr) = &self.proxy_wallet_address {
            eprintln!("   ✓ Proxy wallet: {}", proxy_addr);
        } else {
            eprintln!("   ✓ Trading account: EOA (private key account)");
        }
        eprintln!("   ✓ Builder attribution headers: enabled for all order posts");
        Ok(())
    }

    fn is_auth_error(error: &anyhow::Error) -> bool {
        let msg = error.to_string().to_ascii_lowercase();
        msg.contains("401")
            || msg.contains("403")
            || msg.contains("unauthorized")
            || msg.contains("forbidden")
    }

    // Keep cache invalidation strict so transient/WAF 403s don't trigger auth-rebuild storms.
    fn should_invalidate_auth_cache(error: &anyhow::Error) -> bool {
        let msg = error.to_string().to_ascii_lowercase();
        msg.contains("401")
            || msg.contains("unauthorized")
            || msg.contains("invalid api")
            || msg.contains("api key")
            || msg.contains("invalid signature")
    }

    fn is_transient_order_error(error: &anyhow::Error) -> bool {
        let msg = error.to_string().to_ascii_lowercase();
        msg.contains("429")
            || msg.contains("status: 500")
            || msg.contains("status 500")
            || msg.contains("status: 502")
            || msg.contains("status 502")
            || msg.contains("status: 503")
            || msg.contains("status 503")
            || msg.contains("status: 504")
            || msg.contains("status 504")
            || msg.contains("rate limit")
            || msg.contains("too many requests")
            || msg.contains("timeout")
            || msg.contains("timed out")
            || msg.contains("temporar")
            || msg.contains("service unavailable")
            || msg.contains("bad gateway")
            || msg.contains("gateway timeout")
            || msg.contains("connection reset")
            || msg.contains("connection aborted")
            || msg.contains("connection refused")
    }

    fn is_timeout_error(error: &anyhow::Error) -> bool {
        let msg = error.to_string().to_ascii_lowercase();
        msg.contains("timeout")
            || msg.contains("timed out")
            || msg.contains("gateway timeout")
            || msg.contains("deadline exceeded")
    }

    fn is_rate_limit_error(error: &anyhow::Error) -> bool {
        let msg = error.to_string().to_ascii_lowercase();
        msg.contains("429")
            || msg.contains("1015")
            || msg.contains("rate limit")
            || msg.contains("too many requests")
    }

    fn is_network_or_server_error(error: &anyhow::Error) -> bool {
        let msg = error.to_string().to_ascii_lowercase();
        msg.contains("status: 500")
            || msg.contains("status 500")
            || msg.contains("status: 502")
            || msg.contains("status 502")
            || msg.contains("status: 503")
            || msg.contains("status 503")
            || msg.contains("status: 504")
            || msg.contains("status 504")
            || msg.contains("service unavailable")
            || msg.contains("bad gateway")
            || msg.contains("gateway timeout")
            || msg.contains("connection reset")
            || msg.contains("connection aborted")
            || msg.contains("connection refused")
            || msg.contains("network is unreachable")
            || msg.contains("failed to connect")
            || msg.contains("dns")
            || msg.contains("no such host")
            || msg.contains("name resolution")
            || msg.contains("tls")
            || msg.contains("handshake")
    }

    fn should_fallback_to_aws_signer(error: &anyhow::Error) -> bool {
        if Self::is_rate_limit_error(error) {
            return false;
        }
        Self::is_timeout_error(error)
            || Self::is_network_or_server_error(error)
            || (Self::fallback_on_primary_post_errors()
                && Self::is_primary_builder_post_error(error))
    }

    fn order_retry_attempts() -> u32 {
        // Latency-critical submit path: single attempt by default.
        1
    }

    fn order_retry_base_delay_ms() -> u64 {
        250
    }

    fn fallback_on_primary_post_errors() -> bool {
        std::env::var("EVPOLY_ORDER_SIGNER_FALLBACK_ON_PRIMARY_POST_ERRORS")
            .ok()
            .map(|v| {
                let lc = v.trim().to_ascii_lowercase();
                lc == "1" || lc == "true" || lc == "yes" || lc == "on"
            })
            .unwrap_or(true)
    }

    fn is_primary_builder_post_error(error: &anyhow::Error) -> bool {
        let msg = error.to_string().to_ascii_lowercase();
        msg.contains("failed to post order with primary builder attribution")
            || msg.contains("failed to post batch orders with primary builder attribution")
            || msg.contains("primary order signer failed")
            || msg.contains("primary batch order signer failed")
    }

    fn order_signer_rate_limit_retry_attempts() -> u32 {
        1
    }

    fn order_signer_rate_limit_retry_base_ms() -> u64 {
        75
    }

    fn signer_rate_limit_backoff_ms(retry_idx: u32, base_ms: u64) -> u64 {
        let shift = retry_idx.min(6);
        base_ms.saturating_mul(1_u64 << shift)
    }

    fn clob_auth_build_backoff_ms(failures: u32, error: &anyhow::Error) -> i64 {
        let base_ms = if Self::is_rate_limit_error(error) {
            1_000_i64
        } else if Self::is_timeout_error(error) || Self::is_network_or_server_error(error) {
            500_i64
        } else {
            300_i64
        };
        let shift = failures.saturating_sub(1).min(6);
        base_ms
            .saturating_mul(1_i64 << shift)
            .clamp(250_i64, 60_000_i64)
    }

    fn align_fak_buy_usdc_notional_for_precision_caps(
        requested_usdc_notional: rust_decimal::Decimal,
        price: rust_decimal::Decimal,
        taker_probe_scale_hint: Option<u32>,
    ) -> rust_decimal::Decimal {
        const USDC_SCALE: u32 = 2;
        const TAKER_MAX_SCALE: u32 = 4;
        const MAX_ADJUST_STEPS: u32 = 200;

        if requested_usdc_notional <= rust_decimal::Decimal::ZERO
            || price <= rust_decimal::Decimal::ZERO
        {
            return requested_usdc_notional;
        }

        let one_cent = rust_decimal::Decimal::new(1, USDC_SCALE);
        let mut candidate = requested_usdc_notional
            .round_dp_with_strategy(USDC_SCALE, RoundingStrategy::MidpointAwayFromZero);
        let taker_probe_scale = taker_probe_scale_hint
            .unwrap_or_else(|| price.scale().saturating_add(2))
            .clamp(TAKER_MAX_SCALE, 8);

        for _ in 0..=MAX_ADJUST_STEPS {
            let taker_amount = (candidate / price)
                .trunc_with_scale(taker_probe_scale)
                .normalize();
            if taker_amount.scale() <= TAKER_MAX_SCALE {
                return candidate;
            }
            if candidate <= one_cent {
                break;
            }
            candidate -= one_cent;
        }

        requested_usdc_notional
            .round_dp_with_strategy(USDC_SCALE, RoundingStrategy::MidpointAwayFromZero)
    }

    fn prewarm_token_cache_max() -> usize {
        std::env::var("EVPOLY_PREWARM_TOKEN_CACHE_MAX")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(10_000)
            .max(100)
    }

    async fn invalidate_clob_client_cache(&self) {
        *self.cached_clob_client.lock().await = None;
        self.prewarmed_order_metadata.lock().await.clear();
    }

    async fn authenticate_clob_client(
        &self,
        signer: &PrivateKeySigner,
    ) -> Result<ClobClient<Authenticated<Normal>>> {
        let mut auth_builder = ClobClient::new(&self.clob_url, ClobConfig::default())
            .context("Failed to create CLOB client")?
            .authentication_builder(signer);
        self.prewarming_order_metadata.lock().await.clear();

        // Optional nonce support: forces create/derive against a specific nonce.
        if let Some(raw_nonce) = Self::env_nonempty("EVPOLY_CLOB_AUTH_NONCE") {
            match raw_nonce.parse::<u32>() {
                Ok(nonce) => {
                    auth_builder = auth_builder.nonce(nonce);
                }
                Err(e) => {
                    warn!(
                        "Invalid EVPOLY_CLOB_AUTH_NONCE='{}': {}. Ignoring nonce override.",
                        raw_nonce, e
                    );
                }
            }
        }

        if let Some(proxy_addr) = &self.proxy_wallet_address {
            let funder_address =
                AlloyAddress::parse_checksummed(proxy_addr, None).context(format!(
                "Failed to parse proxy_wallet_address: {}. Ensure it's a valid Ethereum address.",
                proxy_addr
            ))?;

            auth_builder = auth_builder.funder(funder_address);

            let sig_type = match self.signature_type {
                Some(1) => SignatureType::Proxy,
                Some(2) => SignatureType::GnosisSafe,
                Some(0) => {
                    anyhow::bail!(
                        "Invalid configuration: proxy_wallet_address is set but signature_type is 0 (EOA)"
                    );
                }
                None => SignatureType::Proxy,
                Some(n) => anyhow::bail!(
                    "Invalid signature_type: {}. Must be 0 (EOA), 1 (Proxy), or 2 (GnosisSafe)",
                    n
                ),
            };

            auth_builder = auth_builder.signature_type(sig_type);
        } else if let Some(sig_type_num) = self.signature_type {
            let sig_type = match sig_type_num {
                0 => SignatureType::Eoa,
                1 | 2 => anyhow::bail!(
                    "signature_type {} requires proxy_wallet_address to be set",
                    sig_type_num
                ),
                n => anyhow::bail!(
                    "Invalid signature_type: {}. Must be 0 (EOA), 1 (Proxy), or 2 (GnosisSafe)",
                    n
                ),
            };
            auth_builder = auth_builder.signature_type(sig_type);
        }

        match auth_builder.authenticate().await {
            Ok(client) => Ok(client),
            Err(e) => {
                let detail = e.to_string();
                let lowered = detail.to_ascii_lowercase();
                if lowered.contains("/auth/derive-api-key")
                    || lowered.contains("429")
                    || lowered.contains("1015")
                {
                    warn!(
                        "CLOB auth derive-api-key failed (likely rate-limited): {}",
                        detail
                    );
                } else {
                    warn!("CLOB auth failed: {}", detail);
                }
                Err(anyhow::Error::new(e)
                    .context(format!("Failed to authenticate with CLOB API: {}", detail)))
            }
        }
    }

    async fn build_attributed_order_client_with_config(
        &self,
        signer: &PrivateKeySigner,
        builder_config: BuilderConfig,
    ) -> Result<ClobClient<Authenticated<BuilderAuth>>> {
        let base_client = self.authenticate_clob_client(signer).await?;
        let builder_client = base_client
            .promote_to_builder(builder_config)
            .await
            .context("Failed to promote CLOB client to builder attribution mode")?;
        Ok(builder_client)
    }

    async fn build_authenticated_client(&self) -> Result<ClobClientHandle> {
        let private_key = self.private_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Private key is required for authentication. Please set private_key in config.json"
            )
        })?;

        let signer = LocalSigner::from_str(private_key)
            .context("Failed to create signer from private key. Ensure private_key is a valid hex string.")?
            .with_chain_id(Some(POLYGON));

        let client = self.authenticate_clob_client(&signer).await?;
        let builder_client_primary = self
            .build_attributed_order_client_with_config(
                &signer,
                self.order_attribution_builder_config()?,
            )
            .await?;
        let builder_client_fallback = self
            .build_attributed_order_client_with_config(
                &signer,
                self.order_attribution_builder_fallback_config()?,
            )
            .await?;

        Ok(ClobClientHandle {
            client,
            builder_client_primary,
            builder_client_fallback,
            signer,
        })
    }

    async fn get_or_create_clob_client(&self) -> Result<Arc<ClobClientHandle>> {
        if let Some(existing) = self.cached_clob_client.lock().await.as_ref().cloned() {
            return Ok(existing);
        }

        // Singleflight guard: prevent concurrent cache-miss auth builds from stampeding
        // /auth/derive-api-key and amplifying rate-limit loops.
        let _build_guard = self.clob_client_build_lock.lock().await;

        // Another task may have populated the cache while we waited.
        if let Some(existing) = self.cached_clob_client.lock().await.as_ref().cloned() {
            return Ok(existing);
        }

        let now_ms = Utc::now().timestamp_millis();
        {
            let backoff = self.clob_auth_backoff_state.lock().await;
            if backoff.retry_after_ms > now_ms {
                let retry_in_ms = backoff.retry_after_ms.saturating_sub(now_ms);
                anyhow::bail!(
                    "CLOB auth backoff active (retry_in_ms={}, consecutive_failures={})",
                    retry_in_ms,
                    backoff.consecutive_failures
                );
            }
        }

        let built = match self.build_authenticated_client().await {
            Ok(handle) => {
                let mut backoff = self.clob_auth_backoff_state.lock().await;
                backoff.consecutive_failures = 0;
                backoff.retry_after_ms = 0;
                Arc::new(handle)
            }
            Err(e) => {
                let now_ms = Utc::now().timestamp_millis();
                let mut backoff = self.clob_auth_backoff_state.lock().await;
                backoff.consecutive_failures = backoff.consecutive_failures.saturating_add(1);
                let backoff_ms = Self::clob_auth_build_backoff_ms(backoff.consecutive_failures, &e);
                backoff.retry_after_ms = now_ms.saturating_add(backoff_ms);
                warn!(
                    "CLOB auth build failed (streak={} backoff_ms={}): {}",
                    backoff.consecutive_failures, backoff_ms, e
                );
                return Err(e.context(format!("CLOB auth retry backoff set for {}ms", backoff_ms)));
            }
        };

        let mut cache = self.cached_clob_client.lock().await;
        if let Some(existing) = cache.as_ref().cloned() {
            return Ok(existing);
        }
        *cache = Some(built.clone());
        Ok(built)
    }

    pub async fn attach_ws_state(&self, ws_state: SharedPolymarketWsState) {
        *self.ws_state.lock().await = Some(ws_state);
    }

    pub fn ws_enabled(&self) -> bool {
        self.ws_enabled
    }

    pub fn ws_market_stale_ms(&self) -> i64 {
        self.ws_market_stale_ms
    }

    pub fn ws_order_stale_ms(&self) -> i64 {
        self.ws_order_stale_ms
    }

    pub async fn ws_auth_context(&self) -> Result<(Credentials, Address)> {
        let handle = self.get_or_create_clob_client().await?;
        Ok((handle.client.credentials().clone(), handle.client.address()))
    }

    async fn ws_state_snapshot(&self) -> Option<SharedPolymarketWsState> {
        self.ws_state.lock().await.clone()
    }

    /// Get all active markets (using events endpoint)
    pub async fn get_all_active_markets(&self, limit: u32) -> Result<Vec<Market>> {
        self.get_all_active_markets_page(limit, 0).await
    }

    /// Get active markets page from events endpoint with offset pagination.
    pub async fn get_all_active_markets_page(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Market>> {
        self.get_all_active_markets_page_tagged(limit, offset, None)
            .await
    }

    /// Get active markets page from events endpoint with optional Gamma tag filtering.
    pub async fn get_all_active_markets_page_tagged(
        &self,
        limit: u32,
        offset: u32,
        tag_slug: Option<&str>,
    ) -> Result<Vec<Market>> {
        let url = format!("{}/events", self.gamma_url);
        let limit_str = limit.to_string();
        let offset_str = offset.to_string();
        let mut params = HashMap::new();
        params.insert("active", "true");
        params.insert("closed", "false");
        params.insert("limit", &limit_str);
        params.insert("offset", &offset_str);
        let tag_slug = tag_slug.map(str::trim).filter(|value| !value.is_empty());
        if let Some(tag_slug) = tag_slug {
            params.insert("tag_slug", tag_slug);
        }

        let response = self
            .client
            .get(&url)
            .query(&params)
            .send()
            .await
            .context("Failed to fetch all active markets")?;

        let status = response.status();
        let body = response
            .bytes()
            .await
            .context("Failed to read active markets response body")?;
        if body.len() > ACTIVE_MARKETS_MAX_RESPONSE_BYTES {
            anyhow::bail!(
                "active markets payload too large: {} bytes (max {})",
                body.len(),
                ACTIVE_MARKETS_MAX_RESPONSE_BYTES
            );
        }

        if !status.is_success() {
            let response_text = String::from_utf8_lossy(&body);
            let truncated = response_text.chars().take(512).collect::<String>();
            log::warn!(
                "Get all active markets API returned error status {}: {}",
                status,
                truncated
            );
            anyhow::bail!("API returned error status {}: {}", status, truncated);
        }
        let all_markets = parse_active_markets_payload(&body)?;

        log::debug!(
            "Fetched {} active markets from events endpoint (limit={}, offset={})",
            all_markets.len(),
            limit,
            offset
        );
        Ok(all_markets)
    }

    /// Fetch CLOB markets with pagination cursor.
    /// Returns market detail rows that include reward metadata (`rewards.rates`, `min_size`, `max_spread`).
    pub async fn get_all_clob_markets(
        &self,
        page_limit: u32,
        max_pages: u32,
    ) -> Result<Vec<MarketDetails>> {
        let mut all_rows: Vec<MarketDetails> = Vec::new();
        let url = format!("{}/markets", self.clob_url);
        let mut next_cursor: Option<String> = None;
        let mut seen_cursors: HashSet<String> = HashSet::new();
        let effective_page_limit = page_limit.clamp(50, 1_000);
        let effective_max_pages = max_pages.max(1);
        let window_rows = usize::try_from(effective_page_limit)
            .ok()
            .unwrap_or(1_000)
            .saturating_mul(usize::try_from(effective_max_pages).ok().unwrap_or(1));

        // CLOB `/markets` is cursor-ordered oldest -> newest. Start near the tail of the
        // dataset so recent/open markets are included in the scan window.
        let bootstrap = self
            .send_readonly_get_with_proxy_fallback(|client| {
                client
                    .get(&url)
                    .query(&[("limit", effective_page_limit.to_string())])
            })
            .await
            .context("Failed to fetch CLOB markets bootstrap page")?;
        let bootstrap_status = bootstrap.status();
        let bootstrap_body = bootstrap
            .text()
            .await
            .context("Failed to read CLOB markets bootstrap body")?;
        if !bootstrap_status.is_success() {
            anyhow::bail!(
                "Failed to fetch CLOB markets bootstrap page (status {}): {}",
                bootstrap_status,
                bootstrap_body
            );
        }
        let bootstrap_json: Value = serde_json::from_str(&bootstrap_body)
            .with_context(|| format!("Failed to parse CLOB bootstrap body: {}", bootstrap_body))?;
        let total_count = bootstrap_json
            .get("count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if total_count > window_rows {
            let start_offset = total_count.saturating_sub(window_rows);
            let cursor = base64::engine::general_purpose::STANDARD
                .encode(start_offset.to_string().as_bytes());
            next_cursor = Some(cursor);
        }

        for _ in 0..effective_max_pages {
            let response = self
                .send_readonly_get_with_proxy_fallback(|client| {
                    let mut request = client
                        .get(&url)
                        .query(&[("limit", effective_page_limit.to_string())]);
                    if let Some(cursor) = next_cursor.as_deref() {
                        request = request.query(&[("next_cursor", cursor)]);
                    }
                    request
                })
                .await
                .context("Failed to fetch CLOB markets")?;
            let status = response.status();
            let body = response
                .text()
                .await
                .context("Failed to read CLOB markets response body")?;
            if !status.is_success() {
                anyhow::bail!("Failed to fetch CLOB markets (status {}): {}", status, body);
            }

            let json: Value = serde_json::from_str(&body)
                .with_context(|| format!("Failed to parse CLOB markets body: {}", body))?;
            let Some(rows) = json.get("data").and_then(Value::as_array) else {
                break;
            };
            if rows.is_empty() {
                break;
            }
            for row in rows {
                if let Ok(market) = serde_json::from_value::<MarketDetails>(row.clone()) {
                    all_rows.push(market);
                }
            }

            let next = json
                .get("next_cursor")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty() && *v != "LTE=" && *v != "null")
                .map(str::to_string);
            let Some(cursor) = next else {
                break;
            };
            if !seen_cursors.insert(cursor.clone()) {
                break;
            }
            next_cursor = Some(cursor);

            if all_rows.len() >= 50_000 {
                break;
            }
        }

        Ok(all_rows)
    }

    /// Load reward-ranked markets directly from Polymarket rewards API.
    pub async fn get_rewards_markets_api(
        &self,
        max_pages: u32,
        query: Option<&str>,
        tag_slug: Option<&str>,
        sponsored: bool,
    ) -> Result<Vec<RewardsMarketEntry>> {
        let url = "https://polymarket.com/api/rewards/markets";
        let effective_pages = max_pages.clamp(1, 200);
        let mut all_rows = Vec::new();
        let mut next_cursor: Option<String> = None;
        let mut seen_cursors: HashSet<String> = HashSet::new();

        for _ in 0..effective_pages {
            let sponsored_value = if sponsored { "true" } else { "false" };
            let response = self
                .send_readonly_get_with_proxy_fallback(|client| {
                    let mut request = client
                        .get(url)
                        .header("User-Agent", "Mozilla/5.0 EVPoly")
                        .query(&[
                            ("orderBy", "rate_per_day"),
                            ("position", "DESC"),
                            ("requestPath", "/rewards"),
                            ("sponsored", sponsored_value),
                        ]);
                    if let Some(query_value) =
                        query.map(str::trim).filter(|value| !value.is_empty())
                    {
                        request = request.query(&[("query", query_value)]);
                    }
                    if let Some(tag_value) =
                        tag_slug.map(str::trim).filter(|value| !value.is_empty())
                    {
                        request = request.query(&[("tagSlug", tag_value)]);
                    }
                    if let Some(cursor) = next_cursor.as_deref() {
                        request = request.query(&[("nextCursor", cursor)]);
                    }
                    request
                })
                .await
                .context("Failed to fetch rewards markets API page")?;
            let status = response.status();
            let body = response
                .text()
                .await
                .context("Failed to read rewards markets API body")?;
            if !status.is_success() {
                anyhow::bail!(
                    "Failed to fetch rewards markets API (status {}): {}",
                    status,
                    body
                );
            }
            let parsed: RewardsMarketsApiResponse = serde_json::from_str(&body)
                .with_context(|| format!("Failed to parse rewards API body: {}", body))?;
            if parsed.data.is_empty() {
                break;
            }
            all_rows.extend(parsed.data);

            let next = parsed
                .next_cursor
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty() && *value != "null")
                .map(str::to_string);
            let Some(cursor) = next else {
                break;
            };
            if !seen_cursors.insert(cursor.clone()) {
                break;
            }
            next_cursor = Some(cursor);
        }

        if all_rows.is_empty() {
            return Ok(all_rows);
        }

        let mut deduped = Vec::new();
        let mut seen = HashSet::new();
        for row in all_rows {
            let key = if !row.condition_id.trim().is_empty() {
                row.condition_id.trim().to_ascii_lowercase()
            } else {
                row.market_slug.trim().to_ascii_lowercase()
            };
            if key.is_empty() || !seen.insert(key) {
                continue;
            }
            deduped.push(row);
        }

        Ok(deduped)
    }

    /// Load reward-ranked markets from Polymarket rewards page dehydrated payload.
    /// This returns the page's current market list (typically top 100 by `rate_per_day`).
    pub async fn get_rewards_markets_page(&self) -> Result<Vec<RewardsMarketEntry>> {
        if let Ok(rows) = self.get_rewards_markets_api(1, None, None, true).await {
            if !rows.is_empty() {
                return Ok(rows);
            }
        }

        let url = "https://polymarket.com/rewards?desc=true&id=rate_per_day&q=";
        let html = self
            .send_readonly_get_with_proxy_fallback(|client| {
                client.get(url).header("User-Agent", "Mozilla/5.0 EVPoly")
            })
            .await
            .context("Failed to fetch rewards page HTML")?
            .text()
            .await
            .context("Failed to read rewards page HTML")?;

        let script_marker = "<script id=\"__NEXT_DATA__\"";
        let Some(marker_idx) = html.find(script_marker) else {
            anyhow::bail!("Rewards page missing __NEXT_DATA__ marker");
        };
        let Some(script_tag_close_rel) = html[marker_idx..].find('>') else {
            anyhow::bail!("Rewards page malformed __NEXT_DATA__ tag");
        };
        let json_start = marker_idx + script_tag_close_rel + 1;
        let Some(script_end_rel) = html[json_start..].find("</script>") else {
            anyhow::bail!("Rewards page missing __NEXT_DATA__ closing tag");
        };
        let json_end = json_start + script_end_rel;
        let json_blob = &html[json_start..json_end];
        let next_data: Value = serde_json::from_str(json_blob)
            .context("Failed to parse rewards __NEXT_DATA__ JSON")?;

        let queries = next_data
            .get("props")
            .and_then(|v| v.get("pageProps"))
            .and_then(|v| v.get("dehydratedState"))
            .and_then(|v| v.get("queries"))
            .and_then(Value::as_array)
            .context("Rewards payload missing dehydrated queries")?;

        for query in queries {
            let Some(rows) = query
                .get("state")
                .and_then(|v| v.get("data"))
                .and_then(|v| v.get("data"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            if rows.is_empty() {
                continue;
            }
            let first_has_market_shape = rows
                .first()
                .and_then(Value::as_object)
                .map(|obj| obj.contains_key("market_slug") && obj.contains_key("condition_id"))
                .unwrap_or(false);
            if !first_has_market_shape {
                continue;
            }
            let mut parsed = Vec::with_capacity(rows.len());
            for row in rows {
                if let Ok(entry) = serde_json::from_value::<RewardsMarketEntry>(row.clone()) {
                    parsed.push(entry);
                }
            }
            if !parsed.is_empty() {
                return Ok(parsed);
            }
        }

        anyhow::bail!("Rewards payload did not contain market rows")
    }

    /// Load reward-eligible markets from Gamma `/markets` using high-volume ordering.
    /// This captures reward markets that may be absent from rewards-page top rows.
    pub async fn get_rewards_markets_gamma(
        &self,
        page_limit: u32,
        max_pages: u32,
    ) -> Result<Vec<RewardsMarketEntry>> {
        let url = format!("{}/markets", self.gamma_url);
        let effective_limit = page_limit.clamp(100, 500);
        let effective_pages = max_pages.max(1);
        let mut offset = 0_u32;
        let mut out: HashMap<String, GammaRewardAccumulator> = HashMap::new();

        for _ in 0..effective_pages {
            let limit_str = effective_limit.to_string();
            let offset_str = offset.to_string();
            let mut params = HashMap::new();
            params.insert("active", "true");
            params.insert("closed", "false");
            params.insert("acceptingOrders", "true");
            params.insert("order", "volumeNum");
            params.insert("ascending", "false");
            params.insert("limit", limit_str.as_str());
            params.insert("offset", offset_str.as_str());

            let response = self
                .send_readonly_get_with_proxy_fallback(|client| client.get(&url).query(&params))
                .await
                .context("Failed to fetch Gamma reward markets")?;
            let status = response.status();
            let body = response
                .text()
                .await
                .context("Failed to read Gamma reward markets response body")?;
            if !status.is_success() {
                anyhow::bail!(
                    "Failed to fetch Gamma reward markets (status {}): {}",
                    status,
                    body
                );
            }
            let rows: Value = serde_json::from_str(&body)
                .with_context(|| format!("Failed to parse Gamma reward markets body: {}", body))?;
            let Some(markets) = rows.as_array() else {
                break;
            };
            if markets.is_empty() {
                break;
            }

            for row in markets {
                let condition_id = value_string(row, &["conditionId", "condition_id"]);
                let market_slug = value_string(row, &["slug", "market_slug"]);
                if condition_id.trim().is_empty() || market_slug.trim().is_empty() {
                    continue;
                }

                let max_rate = reward_rate_from_gamma_row(row);
                let rewards_max_spread =
                    value_f64(row, &["rewardsMaxSpread", "rewards_max_spread"]);
                let rewards_min_size = value_f64(row, &["rewardsMinSize", "rewards_min_size"]);
                let reward_params_valid = rewards_max_spread
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .is_some()
                    && rewards_min_size
                        .filter(|v| v.is_finite() && *v > 0.0)
                        .is_some();
                if !(max_rate.is_finite() && max_rate > 0.0 && reward_params_valid) {
                    continue;
                }

                let key = condition_id.to_ascii_lowercase();
                let entry = out.entry(key).or_insert_with(|| GammaRewardAccumulator {
                    condition_id: condition_id.clone(),
                    market_slug: market_slug.clone(),
                    question: value_string_opt(row, &["question"]),
                    market_competitiveness: value_f64(
                        row,
                        &["market_competitiveness", "competitive"],
                    ),
                    rewards_max_spread,
                    rewards_min_size,
                    max_rate_per_day: max_rate,
                });
                if max_rate > entry.max_rate_per_day {
                    entry.max_rate_per_day = max_rate;
                }
                if entry.rewards_max_spread.is_none() {
                    entry.rewards_max_spread = rewards_max_spread;
                }
                if entry.rewards_min_size.is_none() {
                    entry.rewards_min_size = rewards_min_size;
                }
                if entry.market_competitiveness.is_none() {
                    entry.market_competitiveness =
                        value_f64(row, &["market_competitiveness", "competitive"]);
                }
            }

            if markets.len() < usize::try_from(effective_limit).ok().unwrap_or(500) {
                break;
            }
            offset = offset.saturating_add(effective_limit);
        }

        let mut rows = out
            .into_values()
            .map(|entry| RewardsMarketEntry {
                condition_id: entry.condition_id,
                market_slug: entry.market_slug,
                question: entry.question,
                market_competitiveness: entry.market_competitiveness,
                rewards_max_spread: entry.rewards_max_spread,
                rewards_min_size: entry.rewards_min_size,
                rewards_config: vec![RewardsConfigEntry {
                    rate_per_day: Some(entry.max_rate_per_day),
                    start_date: None,
                    end_date: None,
                }],
            })
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| b.reward_rate_hint().total_cmp(&a.reward_rate_hint()));
        Ok(rows)
    }

    /// Get market by slug (e.g., "btc-updown-15m-1767726000")
    /// The API returns an event object with a markets array
    pub async fn get_market_by_slug(&self, slug: &str) -> Result<Market> {
        let direct_url = format!("{}/markets", self.gamma_url);
        if let Ok(response) = self
            .client
            .get(&direct_url)
            .query(&[("slug", slug)])
            .send()
            .await
        {
            if response.status().is_success() {
                if let Ok(json) = response.json::<Value>().await {
                    if let Some(market_json) = json.as_array().and_then(|arr| arr.first()) {
                        if let Ok(market) = Self::parse_market_from_gamma_value(market_json.clone())
                        {
                            return Ok(market);
                        }
                    } else if let Some(market_json) = json
                        .get("data")
                        .and_then(|d| d.as_array())
                        .and_then(|arr| arr.first())
                    {
                        if let Ok(market) = Self::parse_market_from_gamma_value(market_json.clone())
                        {
                            return Ok(market);
                        }
                    }
                }
            }
        }
        self.get_market_from_event_slug(slug, None).await
    }

    /// Get a market from an event slug, optionally matching a specific market slug inside that event.
    pub async fn get_market_from_event_slug(
        &self,
        event_slug: &str,
        market_slug: Option<&str>,
    ) -> Result<Market> {
        let url = format!("{}/events/slug/{}", self.gamma_url, event_slug);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context(format!("Failed to fetch market by slug: {}", event_slug))?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!(
                "Failed to fetch market by slug: {} (status: {})",
                event_slug,
                status
            );
        }
        let json: Value = response
            .json()
            .await
            .context("Failed to parse market response")?;

        // The response is an event object with a "markets" array.
        if let Some(markets) = json.get("markets").and_then(|m| m.as_array()) {
            let selected = if let Some(target_slug) = market_slug {
                markets.iter().find(|entry| {
                    entry
                        .get("slug")
                        .and_then(|v| v.as_str())
                        .map(|slug| slug.eq_ignore_ascii_case(target_slug))
                        .unwrap_or(false)
                })
            } else {
                markets.first()
            };
            if let Some(market_json) = selected {
                if let Ok(market) = Self::parse_market_from_gamma_value(market_json.clone()) {
                    return Ok(market);
                }
            }
            if let Some(target_slug) = market_slug {
                anyhow::bail!(
                    "Event slug {} does not contain market slug {}",
                    event_slug,
                    target_slug
                );
            }
        }

        anyhow::bail!("Invalid market response format: no markets array found")
    }

    fn parse_market_from_gamma_value(market_json: Value) -> Result<Market> {
        let mut normalized = market_json;
        if let Some(obj) = normalized.as_object_mut() {
            if let Some(clob_ids) = obj.get("clobTokenIds").cloned() {
                if clob_ids.is_array() {
                    obj.insert(
                        "clobTokenIds".to_string(),
                        Value::String(clob_ids.to_string()),
                    );
                }
            }
            if let Some(outcomes) = obj.get("outcomes").cloned() {
                if outcomes.is_array() {
                    obj.insert("outcomes".to_string(), Value::String(outcomes.to_string()));
                }
            }
            if obj.get("endDateISO").is_none() && obj.get("endDateIso").is_none() {
                if let Some(end_date) = obj.get("endDate").cloned() {
                    obj.insert("endDateISO".to_string(), end_date);
                }
            }
            if obj.get("active").is_none() {
                obj.insert("active".to_string(), Value::Bool(true));
            }
            if obj.get("closed").is_none() {
                obj.insert("closed".to_string(), Value::Bool(false));
            }
        }
        serde_json::from_value::<Market>(normalized).context("Failed to parse market response")
    }

    /// Get order book for a specific token
    pub async fn get_orderbook(&self, token_id: &str) -> Result<OrderBook> {
        if self.ws_enabled {
            if let Some(ws_state) = self.ws_state_snapshot().await {
                if let Some(book) = ws_state
                    .get_orderbook(token_id, self.ws_market_stale_ms)
                    .await
                {
                    return Ok(book);
                }
            }
        }

        let url = format!("{}/book", self.clob_url);
        let params = [("token_id", token_id)];

        let response = self
            .client
            .get(&url)
            .query(&params)
            .send()
            .await
            .context("Failed to fetch orderbook")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("Failed to read orderbook response body")?;

        if let Ok(v) = serde_json::from_str::<Value>(&body) {
            if let Some(err_msg) = v.get("error").and_then(Value::as_str) {
                let err_lc = err_msg.to_ascii_lowercase();
                if err_lc.contains("no orderbook exists") {
                    anyhow::bail!("no_orderbook: {}", err_msg);
                }
                anyhow::bail!("orderbook_api_error: {}", err_msg);
            }
        }

        if !status.is_success() {
            anyhow::bail!("Failed to fetch orderbook (status {}): {}", status, body);
        }

        let orderbook: OrderBook = serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse orderbook: {}", body))?;

        Ok(orderbook)
    }

    /// Get market details by condition ID
    pub async fn get_market(&self, condition_id: &str) -> Result<MarketDetails> {
        let url = format!("{}/markets/{}", self.clob_url, condition_id);
        let response = self.client.get(&url).send().await.context(format!(
            "Failed to fetch market for condition_id: {}",
            condition_id
        ))?;

        let status = response.status();

        if !status.is_success() {
            anyhow::bail!("Failed to fetch market (status: {})", status);
        }

        let json_text = response
            .text()
            .await
            .context("Failed to read response body")?;

        let market: MarketDetails = serde_json::from_str(&json_text).map_err(|e| {
            log::error!(
                "Failed to parse market response: {}. Response was: {}",
                e,
                json_text
            );
            anyhow::anyhow!("Failed to parse market response: {}", e)
        })?;

        Ok(market)
    }

    /// MM/non-trading market details fetch with proxy pool fallback to reduce direct rate-limit pressure.
    pub async fn get_market_via_proxy_pool(&self, condition_id: &str) -> Result<MarketDetails> {
        let url = format!("{}/markets/{}", self.clob_url, condition_id);
        let response = self
            .send_readonly_get_with_proxy_fallback(|client| client.get(&url))
            .await
            .context(format!(
                "Failed to fetch market via proxy pool for condition_id: {}",
                condition_id
            ))?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("Failed to fetch market (status: {})", status);
        }
        let json_text = response
            .text()
            .await
            .context("Failed to read response body")?;
        let market: MarketDetails = serde_json::from_str(&json_text).map_err(|e| {
            log::error!(
                "Failed to parse market response: {}. Response was: {}",
                e,
                json_text
            );
            anyhow::anyhow!("Failed to parse market response: {}", e)
        })?;
        Ok(market)
    }

    /// Fetch only order-constraint fields with tolerant parsing.
    /// This avoids brittle deserialization failures on unrelated market fields.
    pub async fn get_market_constraints(
        &self,
        condition_id: &str,
    ) -> Result<MarketConstraintSnapshot> {
        let attempts = Self::market_constraints_retry_attempts();
        let retry_base_ms = Self::market_constraints_retry_base_ms();
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1..=attempts {
            match self.get_market_constraints_once(condition_id).await {
                Ok(snapshot) => return Ok(snapshot),
                Err(err) => {
                    let with_attempt = err.context(format!(
                        "market constraints fetch failed for {} (attempt {}/{})",
                        condition_id, attempt, attempts
                    ));
                    last_err = Some(with_attempt);
                    if attempt < attempts {
                        let shift = attempt.saturating_sub(1).min(6);
                        let delay_ms = retry_base_ms.saturating_mul(1u64 << shift);
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            anyhow::anyhow!(
                "market constraints fetch failed after {} attempts for {}",
                attempts,
                condition_id
            )
        }))
    }

    async fn get_market_constraints_once(
        &self,
        condition_id: &str,
    ) -> Result<MarketConstraintSnapshot> {
        let url = format!("{}/markets/{}", self.clob_url, condition_id);
        let response = self
            .send_readonly_get_with_proxy_fallback(|client| client.get(&url))
            .await
            .with_context(|| {
                format!(
                    "Failed to fetch market constraints for condition_id: {}",
                    condition_id
                )
            })?;

        let status = response.status();
        let body = response
            .bytes()
            .await
            .context("Failed to read market constraints response body")?;
        let body_preview = String::from_utf8_lossy(body.as_ref())
            .chars()
            .take(320)
            .collect::<String>();
        if !status.is_success() {
            anyhow::bail!(
                "Failed to fetch market constraints (status {}): {}",
                status,
                body_preview
            );
        }

        let json: Value = serde_json::from_slice(body.as_ref()).with_context(|| {
            format!(
                "Failed to parse market constraints body ({} bytes): {}",
                body.len(),
                body_preview
            )
        })?;
        let accepting_orders = json
            .get("accepting_orders")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let active = json.get("active").and_then(Value::as_bool).unwrap_or(false);
        let closed = json.get("closed").and_then(Value::as_bool).unwrap_or(false);

        // Use conservative defaults if fields are missing; exchange-side checks still enforce final validity.
        let minimum_order_size = Self::parse_decimal_field(json.get("minimum_order_size"))
            .or_else(|| Self::parse_decimal_field(json.get("minimumOrderSize")))
            .unwrap_or_else(|| rust_decimal::Decimal::from(5u64));
        let minimum_tick_size = Self::parse_decimal_field(json.get("minimum_tick_size"))
            .or_else(|| Self::parse_decimal_field(json.get("minimumTickSize")))
            .unwrap_or_else(|| rust_decimal::Decimal::new(1, 2));
        let min_size_shares = json
            .get("rewards")
            .and_then(|v| v.get("min_size"))
            .and_then(|v| Self::parse_decimal_field(Some(v)))
            .unwrap_or(rust_decimal::Decimal::ZERO);

        Ok(MarketConstraintSnapshot {
            accepting_orders,
            active,
            closed,
            minimum_order_size,
            minimum_tick_size,
            min_size_shares,
        })
    }

    /// Get price for a token (for trading)
    /// side: "BUY" or "SELL"
    pub async fn get_price(&self, token_id: &str, side: &str) -> Result<rust_decimal::Decimal> {
        let url = format!("{}/price", self.clob_url);
        let params = [("side", side), ("token_id", token_id)];

        log::debug!(
            "Fetching price from: {}?side={}&token_id={}",
            url,
            side,
            token_id
        );

        let response = self
            .client
            .get(&url)
            .query(&params)
            .send()
            .await
            .context("Failed to fetch price")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("Failed to read price response body")?;

        let json: serde_json::Value = serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse price response: {}", body))?;

        if let Some(err_msg) = json.get("error").and_then(Value::as_str) {
            let err_lc = err_msg.to_ascii_lowercase();
            if err_lc.contains("no orderbook exists") {
                anyhow::bail!("no_price: {}", err_msg);
            }
            anyhow::bail!("price_api_error: {}", err_msg);
        }

        if !status.is_success() {
            anyhow::bail!("Failed to fetch price (status {}): {}", status, body);
        }

        let price_str = json
            .get("price")
            .and_then(|p| p.as_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid price response format: {}", body))?;

        let price = rust_decimal::Decimal::from_str(price_str)
            .context(format!("Failed to parse price: {}", price_str))?;

        log::debug!("Price for token {} (side={}): {}", token_id, side, price);

        Ok(price)
    }

    /// Get best bid/ask prices for a token (from orderbook)
    pub async fn get_best_price(&self, token_id: &str) -> Result<Option<TokenPrice>> {
        if self.ws_enabled {
            if let Some(ws_state) = self.ws_state_snapshot().await {
                if let Some(best) = ws_state
                    .get_best_price(token_id, self.ws_market_stale_ms)
                    .await
                {
                    return Ok(Some(best));
                }
            }
        }

        let orderbook = self.get_orderbook(token_id).await?;
        let (best_bid, best_ask) = Self::best_bid_ask_from_orderbook(&orderbook);

        if best_ask.is_some() || best_bid.is_some() {
            Ok(Some(TokenPrice {
                token_id: token_id.to_string(),
                bid: best_bid,
                ask: best_ask,
            }))
        } else {
            Ok(None)
        }
    }

    fn timeframe_seconds_for_proxy_resolution(timeframe: &str) -> Option<u64> {
        match timeframe.trim().to_ascii_lowercase().as_str() {
            "5m" => Some(300),
            "15m" => Some(900),
            "1h" => Some(3_600),
            "4h" => Some(14_400),
            "1d" | "d1" | "24h" | "daily" => Some(86_400),
            _ => None,
        }
    }

    async fn fetch_coinbase_open_close(
        &self,
        symbol: &str,
        timeframe: &str,
        period_open_ts: u64,
        period_close_ts: u64,
    ) -> Result<Option<(f64, f64)>> {
        let granularity = match timeframe.trim().to_ascii_lowercase().as_str() {
            "5m" => 300,
            "15m" => 900,
            "1h" => 3_600,
            // Coinbase candles don't support 4h directly; compose from 1h.
            "4h" => 3_600,
            // Daily EVcurve periods are noon-to-noon ET, so compose from 1h buckets
            // instead of relying on exchange-native daily alignment.
            "1d" | "d1" | "24h" | "daily" => 3_600,
            _ => return Ok(None),
        };

        let start_iso = Utc
            .timestamp_opt(period_open_ts as i64, 0)
            .single()
            .ok_or_else(|| anyhow::anyhow!("invalid coinbase start ts"))?
            .to_rfc3339();
        let end_iso = Utc
            .timestamp_opt(period_close_ts as i64, 0)
            .single()
            .ok_or_else(|| anyhow::anyhow!("invalid coinbase end ts"))?
            .to_rfc3339();
        let product = format!("{}-USD", symbol.trim().to_ascii_uppercase());
        let url = format!(
            "https://api.exchange.coinbase.com/products/{}/candles",
            product
        );

        let response = self
            .client
            .get(&url)
            .header(
                reqwest::header::USER_AGENT,
                "EVPOLY/1.0 (+https://www.evplus.ai)",
            )
            .query(&[
                ("granularity", granularity.to_string()),
                ("start", start_iso),
                ("end", end_iso),
            ])
            .send()
            .await
            .with_context(|| format!("coinbase candles request failed product={}", product))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("coinbase candles body read failed")?;
        if !status.is_success() {
            anyhow::bail!(
                "coinbase candles status={} product={} body={}",
                status,
                product,
                body
            );
        }

        let rows: Vec<Vec<Value>> = serde_json::from_str(&body)
            .with_context(|| format!("coinbase candles parse failed body={}", body))?;
        if rows.is_empty() {
            return Ok(None);
        }

        let mut candles: Vec<(i64, f64, f64)> = Vec::new(); // (open_ts, open, close)
        for row in rows {
            if row.len() < 5 {
                continue;
            }
            let open_ts = row.first().and_then(Value::as_i64);
            let open_px = row.get(3).and_then(Value::as_f64);
            let close_px = row.get(4).and_then(Value::as_f64);
            if let (Some(ts), Some(open), Some(close)) = (open_ts, open_px, close_px) {
                if open.is_finite() && close.is_finite() && open > 0.0 && close > 0.0 {
                    candles.push((ts, open, close));
                }
            }
        }
        if candles.is_empty() {
            return Ok(None);
        }
        candles.sort_by_key(|row| row.0);

        let start = period_open_ts as i64;
        let end = period_close_ts as i64;
        let mut in_window = candles
            .into_iter()
            .filter(|(ts, _, _)| *ts >= start && *ts < end)
            .collect::<Vec<_>>();
        if in_window.is_empty() {
            return Ok(None);
        }
        in_window.sort_by_key(|row| row.0);
        let open_px = in_window.first().map(|row| row.1);
        let close_px = in_window.last().map(|row| row.2);
        Ok(open_px.zip(close_px))
    }

    async fn fetch_binance_open_close(
        &self,
        symbol: &str,
        timeframe: &str,
        period_open_ts: u64,
        period_close_ts: u64,
    ) -> Result<Option<(f64, f64)>> {
        let interval = match timeframe.trim().to_ascii_lowercase().as_str() {
            "1h" => "1h",
            "1d" | "d1" | "24h" | "daily" => "1d",
            _ => return Ok(None),
        };
        let pair = format!("{}USDT", symbol.trim().to_ascii_uppercase());
        let url = "https://api.binance.com/api/v3/klines";
        let start_ms = (period_open_ts as i64).saturating_mul(1_000);
        let end_ms = (period_close_ts as i64)
            .saturating_mul(1_000)
            .saturating_add(1_000);

        let response = self
            .client
            .get(url)
            .query(&[
                ("symbol", pair.clone()),
                ("interval", interval.to_string()),
                ("startTime", start_ms.to_string()),
                ("endTime", end_ms.to_string()),
                ("limit", "3".to_string()),
            ])
            .send()
            .await
            .with_context(|| format!("binance kline request failed symbol={}", pair))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("binance kline body read failed")?;
        if !status.is_success() {
            anyhow::bail!(
                "binance klines status={} symbol={} body={}",
                status,
                pair,
                body
            );
        }

        let rows: Vec<Vec<Value>> = serde_json::from_str(&body)
            .with_context(|| format!("binance klines parse failed body={}", body))?;
        if rows.is_empty() {
            return Ok(None);
        }

        let mut parsed: Vec<(i64, f64, f64)> = Vec::new(); // (open_ms, open, close)
        for row in rows {
            if row.len() < 5 {
                continue;
            }
            let open_ms = row.first().and_then(Value::as_i64);
            let open_px = row
                .get(1)
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<f64>().ok());
            let close_px = row
                .get(4)
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<f64>().ok());
            if let (Some(ts), Some(open), Some(close)) = (open_ms, open_px, close_px) {
                if open.is_finite() && close.is_finite() && open > 0.0 && close > 0.0 {
                    parsed.push((ts, open, close));
                }
            }
        }
        if parsed.is_empty() {
            return Ok(None);
        }
        parsed.sort_by_key(|row| row.0);
        Ok(parsed.first().map(|row| (row.1, row.2)))
    }

    pub async fn fetch_coinbase_period_open_price(
        &self,
        symbol: &str,
        timeframe: &str,
        period_open_ts: u64,
    ) -> Result<Option<f64>> {
        let Some(period_secs) = Self::timeframe_seconds_for_proxy_resolution(timeframe) else {
            return Ok(None);
        };
        let period_close_ts = period_open_ts.saturating_add(period_secs);
        if period_close_ts <= period_open_ts {
            return Ok(None);
        }
        Ok(self
            .fetch_coinbase_open_close(symbol, timeframe, period_open_ts, period_close_ts)
            .await?
            .map(|(open_px, _)| open_px))
    }

    pub async fn fetch_binance_period_open_price(
        &self,
        symbol: &str,
        timeframe: &str,
        period_open_ts: u64,
    ) -> Result<Option<f64>> {
        let Some(period_secs) = Self::timeframe_seconds_for_proxy_resolution(timeframe) else {
            return Ok(None);
        };
        let period_close_ts = period_open_ts.saturating_add(period_secs);
        if period_close_ts <= period_open_ts {
            return Ok(None);
        }
        Ok(self
            .fetch_binance_open_close(symbol, timeframe, period_open_ts, period_close_ts)
            .await?
            .map(|(open_px, _)| open_px))
    }

    /// Fallback proxy resolver for up/down markets when CLOB winner flags are delayed.
    /// Returns `Some(true)` if proxy close > proxy open (UP wins), `Some(false)` for DOWN,
    /// and `None` when data is unavailable or flat.
    pub async fn infer_updown_outcome_from_proxy_candle(
        &self,
        symbol: &str,
        timeframe: &str,
        period_open_ts: u64,
    ) -> Result<Option<bool>> {
        let Some(period_seconds) = Self::timeframe_seconds_for_proxy_resolution(timeframe) else {
            return Ok(None);
        };
        let period_close_ts = period_open_ts.saturating_add(period_seconds);
        if period_close_ts <= period_open_ts {
            return Ok(None);
        }

        let source_tf = timeframe.trim().to_ascii_lowercase();
        let maybe_open_close = if matches!(source_tf.as_str(), "1h" | "1d" | "d1" | "24h" | "daily")
        {
            self.fetch_binance_open_close(symbol, timeframe, period_open_ts, period_close_ts)
                .await?
        } else {
            self.fetch_coinbase_open_close(symbol, timeframe, period_open_ts, period_close_ts)
                .await?
        };

        let Some((open_px, close_px)) = maybe_open_close else {
            return Ok(None);
        };
        if !open_px.is_finite() || !close_px.is_finite() || open_px <= 0.0 || close_px <= 0.0 {
            return Ok(None);
        }
        if (close_px - open_px).abs() <= 1e-12 {
            return Ok(None);
        }
        Ok(Some(close_px > open_px))
    }

    fn best_bid_ask_from_orderbook(
        orderbook: &OrderBook,
    ) -> (Option<rust_decimal::Decimal>, Option<rust_decimal::Decimal>) {
        // CLOB /book arrays are not guaranteed best-first; derive extrema explicitly.
        let best_bid = orderbook
            .bids
            .iter()
            .filter(|level| {
                level.price > rust_decimal::Decimal::ZERO
                    && level.size > rust_decimal::Decimal::ZERO
            })
            .map(|level| level.price)
            .max();
        let best_ask = orderbook
            .asks
            .iter()
            .filter(|level| {
                level.price > rust_decimal::Decimal::ZERO
                    && level.size > rust_decimal::Decimal::ZERO
            })
            .map(|level| level.price)
            .min();
        (best_bid, best_ask)
    }

    /// Place an order using the official SDK with proper private key signing
    ///
    /// This method uses the official polymarket-client-sdk to:
    /// 1. Create signer from private key
    /// 2. Authenticate with the CLOB API
    /// 3. Create and sign the order
    /// 4. Post the signed order
    ///
    /// Equivalent to JavaScript: client.createAndPostOrder(userOrder)
    pub async fn place_order(&self, order: &OrderRequest) -> Result<OrderResponse> {
        let (response, _) = self.place_order_with_timing(order).await?;
        Ok(response)
    }

    pub async fn place_order_with_timing(
        &self,
        order: &OrderRequest,
    ) -> Result<(OrderResponse, PlaceOrderTiming)> {
        let (effective_order_type, retry_policy, attempts) =
            Self::retry_policy_for_order_type(order.order_type.as_str());
        let base_delay_ms = Self::order_retry_base_delay_ms();
        let mut last_err: Option<anyhow::Error> = None;
        let mut timing = PlaceOrderTiming::default();
        timing.order_type_effective = effective_order_type;
        timing.retry_policy = retry_policy.to_string();
        timing.order_signer_primary_url = self.order_primary_signer_url();
        timing.order_signer_fallback_url = self.order_fallback_signer_url();
        let started = Instant::now();

        for attempt in 1..=attempts {
            let get_client_started = Instant::now();
            let handle = self.get_or_create_clob_client().await?;
            timing.get_client_ms += get_client_started.elapsed().as_millis() as i64;
            match self.place_order_with_handle(&handle, order).await {
                Ok((resp, handle_timing)) => {
                    timing.attempts_used = attempt;
                    timing.retry_count = attempt.saturating_sub(1);
                    timing.prewarm_ms += handle_timing.prewarm_ms;
                    timing.prewarm_cache_hit = handle_timing.prewarm_cache_hit;
                    timing.build_order_ms += handle_timing.build_order_ms;
                    timing.sign_ms += handle_timing.sign_ms;
                    timing.build_sign_ms += handle_timing.build_sign_ms;
                    timing.post_order_ms += handle_timing.post_order_ms;
                    timing.order_signer_primary_post_ms +=
                        handle_timing.order_signer_primary_post_ms;
                    timing.order_signer_fallback_post_ms +=
                        handle_timing.order_signer_fallback_post_ms;
                    timing.order_signer_primary_attempts = timing
                        .order_signer_primary_attempts
                        .saturating_add(handle_timing.order_signer_primary_attempts);
                    timing.order_signer_fallback_attempts = timing
                        .order_signer_fallback_attempts
                        .saturating_add(handle_timing.order_signer_fallback_attempts);
                    timing.order_signer_final_route = handle_timing
                        .order_signer_final_route
                        .map(OrderSignerRoute::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    timing.total_api_ms = started.elapsed().as_millis() as i64;
                    return Ok((resp, timing));
                }
                Err(e) => {
                    let is_auth = Self::is_auth_error(&e);
                    let is_transient = Self::is_transient_order_error(&e);
                    if Self::should_invalidate_auth_cache(&e) {
                        self.invalidate_clob_client_cache().await;
                    }
                    if (is_auth || is_transient) && attempt < attempts {
                        let shift = attempt.saturating_sub(1).min(6);
                        let delay_ms = base_delay_ms.saturating_mul(1u64 << shift);
                        warn!(
                            "Transient place_order failure (attempt {}/{}): {}. Retrying in {}ms",
                            attempt, attempts, e, delay_ms
                        );
                        last_err = Some(e);
                        timing.backoff_sleep_ms += delay_ms as i64;
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("place_order failed after retries")))
    }

    pub async fn place_orders_with_timing_batch(
        &self,
        orders: &[OrderRequest],
    ) -> Result<Vec<BatchPlaceOrderResult>> {
        if orders.is_empty() {
            anyhow::bail!("place_orders_with_timing_batch called with empty order list");
        }
        let mut all_results: Vec<BatchPlaceOrderResult> = Vec::with_capacity(orders.len());
        for chunk in orders.chunks(15) {
            let chunk_results = self.place_orders_with_timing_batch_chunk(chunk).await?;
            all_results.extend(chunk_results);
        }
        Ok(all_results)
    }

    async fn place_orders_with_timing_batch_chunk(
        &self,
        orders: &[OrderRequest],
    ) -> Result<Vec<BatchPlaceOrderResult>> {
        if orders.is_empty() {
            return Ok(Vec::new());
        }
        let all_fak = orders.iter().all(|order| {
            let (effective_order_type, _, _) =
                Self::retry_policy_for_order_type(order.order_type.as_str());
            effective_order_type == "FAK"
        });
        let attempts = if all_fak {
            1
        } else {
            Self::order_retry_attempts()
        };
        let retry_policy = if all_fak {
            "batch_fak_no_retry"
        } else {
            "batch_default_retry"
        };
        let base_delay_ms = Self::order_retry_base_delay_ms();
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=attempts {
            let get_client_started = Instant::now();
            let handle = self.get_or_create_clob_client().await?;
            let get_client_ms = get_client_started.elapsed().as_millis() as i64;
            match self.place_orders_with_handle(&handle, orders).await {
                Ok(mut results) => {
                    for result in results.iter_mut() {
                        result.timing.attempts_used = attempt;
                        result.timing.retry_count = attempt.saturating_sub(1);
                        result.timing.retry_policy = retry_policy.to_string();
                        result.timing.get_client_ms += get_client_ms;
                    }
                    return Ok(results);
                }
                Err(e) => {
                    let is_auth = Self::is_auth_error(&e);
                    let is_transient = Self::is_transient_order_error(&e);
                    if Self::should_invalidate_auth_cache(&e) {
                        self.invalidate_clob_client_cache().await;
                    }
                    if (is_auth || is_transient) && attempt < attempts {
                        let shift = attempt.saturating_sub(1).min(6);
                        let delay_ms = base_delay_ms.saturating_mul(1u64 << shift);
                        warn!(
                            "Transient batch place_order failure (attempt {}/{}): {}. Retrying in {}ms",
                            attempt, attempts, e, delay_ms
                        );
                        last_err = Some(e);
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            anyhow::anyhow!("place_orders_with_timing_batch failed after retries")
        }))
    }

    fn retry_policy_for_order_type(order_type: &str) -> (String, &'static str, u32) {
        let effective_order_type = match order_type.trim().to_ascii_uppercase().as_str() {
            "IOC" => "FAK".to_string(),
            "" => "LIMIT".to_string(),
            other => other.to_string(),
        };
        if effective_order_type == "FAK" {
            (effective_order_type, "fak_no_retry", 1)
        } else {
            (
                effective_order_type,
                "default_retry",
                Self::order_retry_attempts(),
            )
        }
    }

    async fn prewarm_order_metadata(
        &self,
        client: &ClobClient<Authenticated<Normal>>,
        token_id: U256,
    ) -> Result<bool> {
        let token_key = token_id.to_string();
        {
            let cache = self.prewarmed_order_metadata.lock().await;
            if cache.contains(token_key.as_str()) {
                return Ok(true);
            }
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        if let Some(retry_after_ms) = self.tick_metadata_retry_after_ms(token_key.as_str()).await {
            if now_ms < retry_after_ms {
                let remaining_ms = retry_after_ms.saturating_sub(now_ms);
                warn!(
                    "Tick metadata prewarm backoff active token={} remaining_ms={}",
                    token_key, remaining_ms
                );
                anyhow::bail!(
                    "tick metadata backoff active token={} remaining_ms={}",
                    token_key,
                    remaining_ms
                );
            }
            self.clear_tick_metadata_backoff(token_key.as_str()).await;
        }
        loop {
            {
                let cache = self.prewarmed_order_metadata.lock().await;
                if cache.contains(token_key.as_str()) {
                    return Ok(true);
                }
            }
            let mut inflight = self.prewarming_order_metadata.lock().await;
            if !inflight.contains(token_key.as_str()) {
                inflight.insert(token_key.clone());
                break;
            }
            drop(inflight);
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let (tick_size, fee_rate, neg_risk) = tokio::join!(
            client.tick_size(token_id),
            client.fee_rate_bps(token_id),
            client.neg_risk(token_id)
        );
        {
            let mut inflight = self.prewarming_order_metadata.lock().await;
            inflight.remove(token_key.as_str());
        }
        if let Err(e) = tick_size {
            let err_anyhow = anyhow::anyhow!(e.to_string());
            if Self::is_rate_limit_error(&err_anyhow) {
                let retry_after_ms = self.set_tick_metadata_backoff(token_key.as_str()).await;
                let remaining_ms =
                    retry_after_ms.saturating_sub(chrono::Utc::now().timestamp_millis());
                anyhow::bail!(
                    "tick metadata prewarm rate-limited token={} remaining_ms={} source=tick_size err={}",
                    token_key,
                    remaining_ms.max(0),
                    err_anyhow
                );
            }
            anyhow::bail!("Failed to prewarm tick_size: {}", e);
        }
        if let Err(e) = fee_rate {
            let err_anyhow = anyhow::anyhow!(e.to_string());
            if Self::is_rate_limit_error(&err_anyhow) {
                let retry_after_ms = self.set_tick_metadata_backoff(token_key.as_str()).await;
                let remaining_ms =
                    retry_after_ms.saturating_sub(chrono::Utc::now().timestamp_millis());
                anyhow::bail!(
                    "tick metadata prewarm rate-limited token={} remaining_ms={} source=fee_rate err={}",
                    token_key,
                    remaining_ms.max(0),
                    err_anyhow
                );
            }
            anyhow::bail!("Failed to prewarm fee_rate: {}", e);
        }
        if let Err(e) = neg_risk {
            let err_anyhow = anyhow::anyhow!(e.to_string());
            if Self::is_rate_limit_error(&err_anyhow) {
                let retry_after_ms = self.set_tick_metadata_backoff(token_key.as_str()).await;
                let remaining_ms =
                    retry_after_ms.saturating_sub(chrono::Utc::now().timestamp_millis());
                anyhow::bail!(
                    "tick metadata prewarm rate-limited token={} remaining_ms={} source=neg_risk err={}",
                    token_key,
                    remaining_ms.max(0),
                    err_anyhow
                );
            }
            anyhow::bail!("Failed to prewarm neg_risk: {}", e);
        }
        let mut cache = self.prewarmed_order_metadata.lock().await;
        let max_entries = Self::prewarm_token_cache_max();
        if cache.len() >= max_entries {
            cache.clear();
        }
        cache.insert(token_key);
        Ok(false)
    }

    async fn invalidate_prewarmed_order_metadata(&self, token_id: U256) {
        let token_key = token_id.to_string();
        let mut cache = self.prewarmed_order_metadata.lock().await;
        cache.remove(token_key.as_str());
        drop(cache);
        let mut inflight = self.prewarming_order_metadata.lock().await;
        inflight.remove(token_key.as_str());
    }

    pub async fn prewarm_token_metadata(&self, token_id: &str) -> Result<bool> {
        let token_id = Self::parse_u256_id(token_id, "token_id")?;
        let handle = self.get_or_create_clob_client().await?;
        self.prewarm_order_metadata(&handle.client, token_id).await
    }

    async fn tick_metadata_retry_after_ms(&self, token_id: &str) -> Option<i64> {
        let token_retry_after_ms = self
            .tick_metadata_retry_after_ms_by_token
            .lock()
            .await
            .get(token_id)
            .copied()
            .unwrap_or(0);
        let now_ms = chrono::Utc::now().timestamp_millis();
        let global_retry_after_ms = {
            let mut global_failures = self.tick_metadata_rl_failures_global.lock().await;
            let mut global_retry_after = self.tick_metadata_retry_after_ms_global.lock().await;
            if *global_retry_after <= now_ms {
                *global_retry_after = 0;
                *global_failures = 0;
                0
            } else {
                *global_retry_after
            }
        };
        let retry_after_ms = token_retry_after_ms.max(global_retry_after_ms);
        (retry_after_ms > now_ms).then_some(retry_after_ms)
    }

    async fn set_tick_metadata_backoff(&self, token_id: &str) -> i64 {
        let token_failures = {
            let mut guard = self.tick_metadata_rl_failures_by_token.lock().await;
            let next = guard
                .get(token_id)
                .copied()
                .unwrap_or(0)
                .saturating_add(1)
                .min(32);
            guard.insert(token_id.to_string(), next);
            next
        };
        let token_shift = token_failures.saturating_sub(1).min(8);
        let token_backoff_ms = TICK_METADATA_RL_BACKOFF_BASE_MS
            .saturating_mul(1_i64 << token_shift)
            .clamp(
                TICK_METADATA_RL_BACKOFF_BASE_MS,
                TICK_METADATA_RL_BACKOFF_MAX_MS,
            );
        let now_ms = chrono::Utc::now().timestamp_millis();
        let token_retry_after_ms = now_ms.saturating_add(token_backoff_ms);
        self.tick_metadata_retry_after_ms_by_token
            .lock()
            .await
            .insert(token_id.to_string(), token_retry_after_ms);
        let global_failures = {
            let mut guard = self.tick_metadata_rl_failures_global.lock().await;
            let next = guard.saturating_add(1).min(32);
            *guard = next;
            next
        };
        let global_shift = global_failures.saturating_sub(1).min(8);
        let global_backoff_ms = TICK_METADATA_RL_BACKOFF_BASE_MS
            .saturating_mul(1_i64 << global_shift)
            .clamp(
                TICK_METADATA_RL_BACKOFF_BASE_MS,
                TICK_METADATA_RL_BACKOFF_MAX_MS,
            );
        let global_retry_after_ms = now_ms.saturating_add(global_backoff_ms);
        {
            let mut guard = self.tick_metadata_retry_after_ms_global.lock().await;
            if global_retry_after_ms > *guard {
                *guard = global_retry_after_ms;
            }
        }
        warn!(
            "Tick metadata backoff set token={} token_backoff_ms={} token_failures={} global_backoff_ms={} global_failures={}",
            token_id,
            token_backoff_ms,
            token_failures,
            global_backoff_ms,
            global_failures
        );
        token_retry_after_ms.max(global_retry_after_ms)
    }

    async fn clear_tick_metadata_backoff(&self, token_id: &str) {
        self.tick_metadata_retry_after_ms_by_token
            .lock()
            .await
            .remove(token_id);
        self.tick_metadata_rl_failures_by_token
            .lock()
            .await
            .remove(token_id);
    }

    pub async fn heartbeat_closed_only_mode(&self) -> Result<()> {
        let first = self.get_or_create_clob_client().await?;
        match first.client.ok().await {
            Ok(_) => Ok(()),
            Err(e) => {
                let err = anyhow::anyhow!(e.to_string());
                if Self::should_invalidate_auth_cache(&err) {
                    self.invalidate_clob_client_cache().await;
                    let second = self.get_or_create_clob_client().await?;
                    second.client.ok().await?;
                    Ok(())
                } else {
                    Err(err).context("heartbeat ok failed")
                }
            }
        }
    }

    async fn place_order_with_handle(
        &self,
        handle: &ClobClientHandle,
        order: &OrderRequest,
    ) -> Result<(OrderResponse, PlaceOrderHandleTiming)> {
        let mut prepared = self.build_and_sign_order_with_handle(handle, order).await?;
        let post_started = Instant::now();
        let mut signer_post_stats = OrderSignerPostStats::default();
        let rate_limit_retry_attempts = Self::order_signer_rate_limit_retry_attempts();
        let rate_limit_retry_base_ms = Self::order_signer_rate_limit_retry_base_ms();
        let mut primary_response: Option<
            polymarket_client_sdk::clob::types::response::PostOrderResponse,
        > = None;
        let mut primary_error: Option<anyhow::Error> = None;

        for retry_idx in 0..=rate_limit_retry_attempts {
            match self
                .post_signed_order_with_builder(
                    handle,
                    prepared.signed_order,
                    false,
                    &mut signer_post_stats,
                )
                .await
            {
                Ok(resp) => {
                    primary_response = Some(resp);
                    primary_error = None;
                    break;
                }
                Err(err) => {
                    if Self::is_timeout_error(&err) {
                        self.note_primary_order_signer_timeout().await;
                    }
                    if Self::is_rate_limit_error(&err) && retry_idx < rate_limit_retry_attempts {
                        let delay_ms =
                            Self::signer_rate_limit_backoff_ms(retry_idx, rate_limit_retry_base_ms);
                        warn!(
                            "Primary order signer rate-limited; retrying in {}ms (retry {}/{})",
                            delay_ms,
                            retry_idx + 1,
                            rate_limit_retry_attempts
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        prepared = self
                            .build_and_sign_order_with_handle(handle, order)
                            .await
                            .context(
                                "Failed to rebuild order for primary signer rate-limit retry",
                            )?;
                        continue;
                    }
                    primary_error = Some(err);
                    break;
                }
            }
        }

        let response = if let Some(resp) = primary_response {
            resp
        } else {
            let primary_err = primary_error.unwrap_or_else(|| {
                anyhow::anyhow!("Primary order signer failed with unknown error")
            });
            if !Self::should_fallback_to_aws_signer(&primary_err) {
                warn!(
                    "Primary order signer error did not qualify for fallback. full_chain={:#}",
                    primary_err
                );
                return Err(primary_err.context(
                    "Primary order signer failed (AWS fallback disabled for this error class)",
                ));
            }
            warn!(
                "Primary order signer post hit transient failure; retrying with AWS fallback signer: {}",
                primary_err
            );
            prepared = self
                .build_and_sign_order_with_handle(handle, order)
                .await
                .context("Failed to rebuild order for fallback signer")?;
            self.post_signed_order_with_builder(
                handle,
                prepared.signed_order,
                true,
                &mut signer_post_stats,
            )
                .await
                .map_err(|fallback_err| {
                    anyhow::anyhow!(
                        "Failed to post order via primary and fallback builder signers (primary: {}; fallback: {})",
                        primary_err,
                        fallback_err
                    )
                })?
        };
        let post_order_ms = post_started.elapsed().as_millis() as i64;
        self.clear_tick_metadata_backoff(prepared.token_id.as_str())
            .await;

        if !response.success {
            let error_msg = response.error_msg.as_deref().unwrap_or("Unknown error");
            error!("❌ Order rejected by API: {}", error_msg);
            anyhow::bail!(
                "Order was rejected: {}\n\
                Order details: Token ID={}, Side={}, Size={}, Price={}",
                error_msg,
                prepared.token_id,
                prepared.side,
                prepared.size,
                prepared.price
            );
        }

        let normalized_order_id = Self::normalize_post_order_id(response.order_id.as_str());
        if normalized_order_id.is_some() {
            eprintln!(
                "✅ Order placed successfully! Order ID: {}",
                response.order_id
            );
        } else {
            warn!("Order post succeeded but returned empty orderID from upstream");
        }
        Ok((
            OrderResponse {
                order_id: normalized_order_id.clone(),
                status: response.status.to_string(),
                message: Some(
                    normalized_order_id
                        .as_deref()
                        .map(|order_id| {
                            format!("Order placed successfully. Order ID: {}", order_id)
                        })
                        .unwrap_or_else(|| {
                            "Order placed successfully. Order ID unavailable".to_string()
                        }),
                ),
            },
            PlaceOrderHandleTiming {
                post_order_ms,
                order_signer_primary_post_ms: signer_post_stats.primary_post_ms,
                order_signer_fallback_post_ms: signer_post_stats.fallback_post_ms,
                order_signer_primary_attempts: signer_post_stats.primary_attempts,
                order_signer_fallback_attempts: signer_post_stats.fallback_attempts,
                order_signer_final_route: signer_post_stats.final_route,
                ..prepared.timing
            },
        ))
    }

    async fn build_signed_orders_with_meta(
        &self,
        handle: &ClobClientHandle,
        orders: &[OrderRequest],
    ) -> Result<(
        Vec<(PlaceOrderHandleTiming, String, String, String, String)>,
        Vec<ClobSignedOrder>,
    )> {
        let mut prepared_orders_meta: Vec<(
            PlaceOrderHandleTiming,
            String,
            String,
            String,
            String,
        )> = Vec::with_capacity(orders.len());
        let mut signed_orders: Vec<ClobSignedOrder> = Vec::with_capacity(orders.len());
        for order in orders {
            let prepared = self.build_and_sign_order_with_handle(handle, order).await?;
            prepared_orders_meta.push((
                prepared.timing,
                prepared.effective_order_type,
                prepared.token_id,
                prepared.side,
                prepared.size,
            ));
            signed_orders.push(prepared.signed_order);
        }
        Ok((prepared_orders_meta, signed_orders))
    }

    async fn place_orders_with_handle(
        &self,
        handle: &ClobClientHandle,
        orders: &[OrderRequest],
    ) -> Result<Vec<BatchPlaceOrderResult>> {
        if orders.is_empty() {
            return Ok(Vec::new());
        }
        let started = Instant::now();
        let (mut prepared_orders_meta, mut signed_orders) =
            self.build_signed_orders_with_meta(handle, orders).await?;
        let post_started = Instant::now();
        let mut signer_post_stats = OrderSignerPostStats::default();
        let rate_limit_retry_attempts = Self::order_signer_rate_limit_retry_attempts();
        let rate_limit_retry_base_ms = Self::order_signer_rate_limit_retry_base_ms();
        let mut primary_responses: Option<
            Vec<polymarket_client_sdk::clob::types::response::PostOrderResponse>,
        > = None;
        let mut primary_error: Option<anyhow::Error> = None;

        for retry_idx in 0..=rate_limit_retry_attempts {
            match self
                .post_signed_orders_with_builder(
                    handle,
                    signed_orders,
                    false,
                    &mut signer_post_stats,
                )
                .await
            {
                Ok(resp) => {
                    primary_responses = Some(resp);
                    primary_error = None;
                    break;
                }
                Err(err) => {
                    if Self::is_timeout_error(&err) {
                        self.note_primary_order_signer_timeout().await;
                    }
                    if Self::is_rate_limit_error(&err) && retry_idx < rate_limit_retry_attempts {
                        let delay_ms =
                            Self::signer_rate_limit_backoff_ms(retry_idx, rate_limit_retry_base_ms);
                        warn!(
                            "Primary batch order signer rate-limited; retrying in {}ms (retry {}/{})",
                            delay_ms,
                            retry_idx + 1,
                            rate_limit_retry_attempts
                        );
                        let (rebuilt_meta, rebuilt_orders) = self
                            .build_signed_orders_with_meta(handle, orders)
                            .await
                            .context(
                                "Failed to rebuild batch orders for primary signer rate-limit retry",
                            )?;
                        prepared_orders_meta = rebuilt_meta;
                        signed_orders = rebuilt_orders;
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    primary_error = Some(err);
                    break;
                }
            }
        }

        let responses = if let Some(resp) = primary_responses {
            resp
        } else {
            let primary_err = primary_error.unwrap_or_else(|| {
                anyhow::anyhow!("Primary batch order signer failed with unknown error")
            });
            if !Self::should_fallback_to_aws_signer(&primary_err) {
                warn!(
                    "Primary batch order signer error did not qualify for fallback. full_chain={:#}",
                    primary_err
                );
                return Err(primary_err.context(
                    "Primary batch order signer failed (AWS fallback disabled for this error class)",
                ));
            }
            warn!(
                "Primary batch order signer post hit transient failure; retrying with AWS fallback signer: {}",
                primary_err
            );
            let (rebuilt_meta, rebuilt_orders) = self
                .build_signed_orders_with_meta(handle, orders)
                .await
                .context("Failed to rebuild batch orders for fallback signer")?;
            prepared_orders_meta = rebuilt_meta;
            self.post_signed_orders_with_builder(
                handle,
                rebuilt_orders,
                true,
                &mut signer_post_stats,
            )
                .await
                .map_err(|fallback_err| {
                    anyhow::anyhow!(
                        "Failed to post batch orders via primary and fallback builder signers (primary: {}; fallback: {})",
                        primary_err,
                        fallback_err
                    )
                })?
        };
        let post_order_ms = post_started.elapsed().as_millis() as i64;
        for (_, _, token_id, _, _) in prepared_orders_meta.iter() {
            self.clear_tick_metadata_backoff(token_id.as_str()).await;
        }
        if responses.len() != prepared_orders_meta.len() {
            anyhow::bail!(
                "Batch post response length mismatch: expected {} got {}",
                prepared_orders_meta.len(),
                responses.len()
            );
        }
        let total_api_ms = started.elapsed().as_millis() as i64;
        let order_signer_primary_url = self.order_primary_signer_url();
        let order_signer_fallback_url = self.order_fallback_signer_url();
        let order_signer_final_route = signer_post_stats
            .final_route
            .map(OrderSignerRoute::as_str)
            .unwrap_or("unknown")
            .to_string();
        let mut results: Vec<BatchPlaceOrderResult> = Vec::with_capacity(responses.len());
        for ((timing_base, effective_order_type, token_id, side, size), response) in
            prepared_orders_meta.into_iter().zip(responses.into_iter())
        {
            let timing = PlaceOrderTiming {
                total_api_ms,
                attempts_used: 1,
                retry_count: 0,
                retry_policy: "batch_single_attempt".to_string(),
                order_type_effective: effective_order_type.clone(),
                get_client_ms: 0,
                prewarm_ms: timing_base.prewarm_ms,
                prewarm_cache_hit: timing_base.prewarm_cache_hit,
                build_order_ms: timing_base.build_order_ms,
                sign_ms: timing_base.sign_ms,
                build_sign_ms: timing_base.build_sign_ms,
                post_order_ms,
                backoff_sleep_ms: 0,
                order_signer_primary_post_ms: signer_post_stats.primary_post_ms,
                order_signer_fallback_post_ms: signer_post_stats.fallback_post_ms,
                order_signer_primary_attempts: signer_post_stats.primary_attempts,
                order_signer_fallback_attempts: signer_post_stats.fallback_attempts,
                order_signer_final_route: order_signer_final_route.clone(),
                order_signer_primary_url: order_signer_primary_url.clone(),
                order_signer_fallback_url: order_signer_fallback_url.clone(),
            };
            if response.success {
                let normalized_order_id = Self::normalize_post_order_id(response.order_id.as_str());
                if normalized_order_id.is_none() {
                    warn!(
                        "Batch order post succeeded but returned empty orderID from upstream (token={}, side={}, size={})",
                        token_id,
                        side,
                        size
                    );
                }
                results.push(BatchPlaceOrderResult {
                    response: Some(OrderResponse {
                        order_id: normalized_order_id.clone(),
                        status: response.status.to_string(),
                        message: Some(
                            normalized_order_id
                                .as_deref()
                                .map(|order_id| {
                                    format!("Order placed successfully. Order ID: {}", order_id)
                                })
                                .unwrap_or_else(|| {
                                    "Order placed successfully. Order ID unavailable".to_string()
                                }),
                        ),
                    }),
                    timing,
                    error: None,
                });
            } else {
                let error_msg = response
                    .error_msg
                    .clone()
                    .unwrap_or_else(|| "Unknown batch order error".to_string());
                results.push(BatchPlaceOrderResult {
                    response: None,
                    timing,
                    error: Some(format!(
                        "{} (token={}, side={}, size={})",
                        error_msg, token_id, side, size
                    )),
                });
            }
        }
        Ok(results)
    }

    async fn post_signed_order_with_builder(
        &self,
        handle: &ClobClientHandle,
        signed_order: ClobSignedOrder,
        use_fallback_signer: bool,
        signer_post_stats: &mut OrderSignerPostStats,
    ) -> Result<polymarket_client_sdk::clob::types::response::PostOrderResponse> {
        let route = if use_fallback_signer || self.should_force_aws_order_signer().await {
            OrderSignerRoute::Fallback
        } else {
            OrderSignerRoute::Primary
        };
        let post_started = Instant::now();
        match route {
            OrderSignerRoute::Fallback => {
                let result = handle
                    .builder_client_fallback
                    .post_order(signed_order)
                    .await;
                let post_ms = post_started.elapsed().as_millis() as i64;
                signer_post_stats.record_attempt(route, post_ms);
                match result {
                    Ok(resp) => {
                        signer_post_stats.record_success(route);
                        Ok(resp)
                    }
                    Err(e) => Err(anyhow::anyhow!(
                        "Failed to post order with fallback builder attribution: {}",
                        e
                    )),
                }
            }
            OrderSignerRoute::Primary => {
                let result = tokio::time::timeout(
                    tokio::time::Duration::from_millis(ORDER_SIGNER_PRIMARY_FALLBACK_TIMEOUT_MS),
                    handle.builder_client_primary.post_order(signed_order),
                )
                .await;
                let post_ms = post_started.elapsed().as_millis() as i64;
                signer_post_stats.record_attempt(route, post_ms);
                match result {
                    Ok(result) => match result {
                        Ok(resp) => {
                            self.note_primary_order_signer_success().await;
                            signer_post_stats.record_success(route);
                            Ok(resp)
                        }
                        Err(e) => Err(anyhow::anyhow!(
                            "Failed to post order with primary builder attribution: {}",
                            e
                        )),
                    },
                    Err(_) => {
                        self.note_primary_order_signer_timeout().await;
                        anyhow::bail!(
                            "Primary builder signer post timed out after {}ms",
                            ORDER_SIGNER_PRIMARY_FALLBACK_TIMEOUT_MS
                        )
                    }
                }
            }
        }
    }

    async fn post_signed_orders_with_builder(
        &self,
        handle: &ClobClientHandle,
        signed_orders: Vec<ClobSignedOrder>,
        use_fallback_signer: bool,
        signer_post_stats: &mut OrderSignerPostStats,
    ) -> Result<Vec<polymarket_client_sdk::clob::types::response::PostOrderResponse>> {
        let route = if use_fallback_signer || self.should_force_aws_order_signer().await {
            OrderSignerRoute::Fallback
        } else {
            OrderSignerRoute::Primary
        };
        let post_started = Instant::now();
        match route {
            OrderSignerRoute::Fallback => {
                let result = handle
                    .builder_client_fallback
                    .post_orders(signed_orders)
                    .await;
                let post_ms = post_started.elapsed().as_millis() as i64;
                signer_post_stats.record_attempt(route, post_ms);
                match result {
                    Ok(resp) => {
                        signer_post_stats.record_success(route);
                        Ok(resp)
                    }
                    Err(e) => Err(anyhow::anyhow!(
                        "Failed to post batch orders with fallback builder attribution: {}",
                        e
                    )),
                }
            }
            OrderSignerRoute::Primary => {
                let result = tokio::time::timeout(
                    tokio::time::Duration::from_millis(ORDER_SIGNER_PRIMARY_FALLBACK_TIMEOUT_MS),
                    handle.builder_client_primary.post_orders(signed_orders),
                )
                .await;
                let post_ms = post_started.elapsed().as_millis() as i64;
                signer_post_stats.record_attempt(route, post_ms);
                match result {
                    Ok(result) => match result {
                        Ok(resp) => {
                            self.note_primary_order_signer_success().await;
                            signer_post_stats.record_success(route);
                            Ok(resp)
                        }
                        Err(e) => Err(anyhow::anyhow!(
                            "Failed to post batch orders with primary builder attribution: {}",
                            e
                        )),
                    },
                    Err(_) => {
                        self.note_primary_order_signer_timeout().await;
                        anyhow::bail!(
                            "Primary builder batch post timed out after {}ms",
                            ORDER_SIGNER_PRIMARY_FALLBACK_TIMEOUT_MS
                        )
                    }
                }
            }
        }
    }

    async fn build_and_sign_order_with_handle(
        &self,
        handle: &ClobClientHandle,
        order: &OrderRequest,
    ) -> Result<PreparedSignedOrder> {
        let side = match order.side.as_str() {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            _ => anyhow::bail!(
                "Invalid order side: {}. Must be 'BUY' or 'SELL'",
                order.side
            ),
        };

        let price = rust_decimal::Decimal::from_str(&order.price)
            .context(format!("Failed to parse price: {}", order.price))?;
        let size = rust_decimal::Decimal::from_str(&order.size)
            .context(format!("Failed to parse size: {}", order.size))?;

        eprintln!(
            "📤 Creating and posting order: {} {} {} @ {}",
            order.side, order.size, order.token_id, order.price
        );

        // Enforce explicit order semantics for limit builder path.
        // Legacy: order_type="LIMIT" => GTD when expiration exists (or auto-injected for BUY),
        // otherwise GTC.
        // Explicit TIF: GTC/GTD/FOK/FAK are honored directly.
        // IOC alias maps to FAK.
        let requested_order_type = order.order_type.trim().to_ascii_uppercase();
        let explicit_tif = match requested_order_type.as_str() {
            "" | "LIMIT" => None,
            "GTC" => Some(OrderType::GTC),
            "GTD" => Some(OrderType::GTD),
            "FOK" => Some(OrderType::FOK),
            "FAK" | "IOC" => Some(OrderType::FAK),
            "MARKET" => {
                anyhow::bail!(
                    "order_type=MARKET is not supported in place_order(). Use place_market_order()"
                );
            }
            other => {
                anyhow::bail!(
                    "Unsupported order_type '{}'. Use LIMIT, GTC, GTD, FOK, FAK, or IOC",
                    other
                );
            }
        };

        let default_ttl_secs = std::env::var("EVPOLY_LIMIT_ORDER_TTL_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1200)
            .max(61);
        let mut expiration_dt = order
            .expiration_ts
            .and_then(|ts| Utc.timestamp_opt(ts, 0).single());

        let effective_order_type = if let Some(explicit_tif) = explicit_tif {
            match explicit_tif {
                OrderType::GTC => {
                    if expiration_dt.is_some() {
                        anyhow::bail!(
                            "order_type=GTC cannot be used with expiration_ts; use order_type=GTD"
                        );
                    }
                    OrderType::GTC
                }
                OrderType::GTD => {
                    if expiration_dt.is_none() {
                        expiration_dt =
                            Some(Utc::now() + chrono::Duration::seconds(default_ttl_secs as i64));
                    }
                    let min_exp = Utc::now() + chrono::Duration::seconds(61);
                    if let Some(exp) = expiration_dt {
                        expiration_dt = Some(if exp < min_exp { min_exp } else { exp });
                    }
                    OrderType::GTD
                }
                OrderType::FOK | OrderType::FAK => {
                    if expiration_dt.is_some() {
                        anyhow::bail!(
                            "order_type={} cannot be used with expiration_ts",
                            explicit_tif
                        );
                    }
                    if order.post_only.unwrap_or(false) {
                        anyhow::bail!(
                            "order_type={} cannot be used with postOnly=true",
                            explicit_tif
                        );
                    }
                    explicit_tif
                }
                _ => {
                    anyhow::bail!("Unsupported explicit order type: {}", explicit_tif);
                }
            }
        } else {
            // Legacy limit semantics.
            if expiration_dt.is_none() && order.side.eq_ignore_ascii_case("BUY") {
                expiration_dt =
                    Some(Utc::now() + chrono::Duration::seconds(default_ttl_secs as i64));
            }
            if let Some(exp) = expiration_dt {
                let min_exp = Utc::now() + chrono::Duration::seconds(61);
                expiration_dt = Some(if exp < min_exp { min_exp } else { exp });
            }
            if expiration_dt.is_some() {
                OrderType::GTD
            } else {
                OrderType::GTC
            }
        };

        let token_id = Self::parse_u256_id(order.token_id.as_str(), "order.token_id")?;
        let token_key = token_id.to_string();
        let prewarm_started = Instant::now();
        let prewarm_cache_hit = match self.prewarm_order_metadata(&handle.client, token_id).await {
            Ok(hit) => hit,
            Err(e) => {
                let err_text = e.to_string();
                let normalized = err_text.to_ascii_lowercase();
                let prewarm_blocked = normalized.contains("tick metadata backoff active")
                    || normalized.contains("tick metadata prewarm rate-limited");
                if prewarm_blocked {
                    anyhow::bail!(
                        "order metadata prewarm blocked token={} side={} price={} size={} err={}",
                        order.token_id,
                        order.side,
                        order.price,
                        order.size,
                        err_text
                    );
                }
                warn!(
                    "Order metadata prewarm failed token={} side={} price={} size={} err={}. Continuing with on-demand metadata fetch.",
                    order.token_id,
                    order.side,
                    order.price,
                    order.size,
                    err_text
                );
                false
            }
        };
        let prewarm_ms = prewarm_started.elapsed().as_millis() as i64;
        let mut build_order_ms = 0_i64;
        let mut sign_ms = 0_i64;
        let signed_order = if matches!(effective_order_type, OrderType::FOK | OrderType::FAK) {
            // FAK/FOK use market-order builder with explicit price cap.
            // This aligns maker/taker precision with exchange rules for immediate orders.
            let amount = match side {
                Side::Buy => {
                    let requested_usdc_notional = (size * price)
                        .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
                    let tick_size_scale: Option<u32> = match handle.client.tick_size(token_id).await
                    {
                        Ok(tick_size) => Some(tick_size.minimum_tick_size.as_decimal().scale()),
                        Err(e) => {
                            warn!(
                                "Failed to fetch token tick_size for FAK/FOK BUY precision alignment token={} side={} err={}; falling back to price-scale probe",
                                order.token_id,
                                order.side,
                                e
                            );
                            None
                        }
                    };
                    let taker_probe_scale = tick_size_scale
                        .map(|scale| scale.saturating_add(2))
                        .unwrap_or_else(|| price.scale().saturating_add(2))
                        .clamp(4, 8);
                    let usdc_notional = Self::align_fak_buy_usdc_notional_for_precision_caps(
                        requested_usdc_notional,
                        price,
                        Some(taker_probe_scale),
                    );
                    if usdc_notional <= rust_decimal::Decimal::ZERO {
                        anyhow::bail!(
                            "Invalid FAK/FOK BUY notional (size*price <= 0): size={} price={}",
                            size,
                            price
                        );
                    }
                    if usdc_notional < requested_usdc_notional {
                        warn!(
                            "Adjusted FAK/FOK BUY notional for precision caps: requested={} adjusted={} price={} price_scale={} tick_size_scale={:?} taker_probe_scale={}",
                            requested_usdc_notional,
                            usdc_notional,
                            price,
                            price.scale(),
                            tick_size_scale,
                            taker_probe_scale
                        );
                    }
                    Amount::usdc(usdc_notional).context(format!(
                        "Failed to build FAK/FOK BUY amount (usdc={}): token={}",
                        usdc_notional, order.token_id
                    ))?
                }
                Side::Sell => {
                    let share_amount =
                        size.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
                    if share_amount <= rust_decimal::Decimal::ZERO {
                        anyhow::bail!(
                            "Invalid FAK/FOK SELL share amount (size <= 0): size={}",
                            size
                        );
                    }
                    Amount::shares(share_amount).context(format!(
                        "Failed to build FAK/FOK SELL amount (shares={}): token={}",
                        share_amount, order.token_id
                    ))?
                }
                Side::Unknown => anyhow::bail!("Invalid order side: UNKNOWN"),
                _ => anyhow::bail!("Invalid non-exhaustive order side"),
            };

            let order_builder = handle
                .client
                .market_order()
                .token_id(token_id)
                .side(side)
                .amount(amount)
                .price(price)
                .order_type(effective_order_type.clone());

            let build_started = Instant::now();
            let signable = match order_builder.build().await {
                Ok(signable) => signable,
                Err(first_err) => {
                    let first_anyhow = anyhow::anyhow!(first_err.to_string());
                    if Self::is_rate_limit_error(&first_anyhow) {
                        let retry_after_ms =
                            self.set_tick_metadata_backoff(token_key.as_str()).await;
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        let remaining_ms = retry_after_ms.saturating_sub(now_ms);
                        anyhow::bail!(
                            "tick metadata build rate-limited token={} remaining_ms={} err={}",
                            order.token_id,
                            remaining_ms.max(0),
                            first_anyhow
                        );
                    }
                    self.invalidate_prewarmed_order_metadata(token_id).await;
                    let retry_builder = handle
                        .client
                        .market_order()
                        .token_id(token_id)
                        .side(side)
                        .amount(amount)
                        .price(price)
                        .order_type(effective_order_type.clone());
                    retry_builder.build().await.map_err(|retry_err| {
                        anyhow::anyhow!(
                            "Failed to build FAK/FOK order: first={} retry={}",
                            first_err,
                            retry_err
                        )
                    })?
                }
            };
            build_order_ms += build_started.elapsed().as_millis() as i64;
            let sign_started = Instant::now();
            let signed = handle
                .client
                .sign(&handle.signer, signable)
                .await
                .context("Failed to sign FAK/FOK order")?;
            sign_ms += sign_started.elapsed().as_millis() as i64;
            signed
        } else {
            let mut order_builder = handle
                .client
                .limit_order()
                .token_id(token_id)
                .size(size)
                .price(price)
                .side(side)
                .order_type(effective_order_type.clone());
            if matches!(effective_order_type, OrderType::GTD) {
                let exp = expiration_dt.ok_or_else(|| {
                    anyhow::anyhow!("order_type=GTD requires expiration_ts or default TTL")
                })?;
                order_builder = order_builder.expiration(exp);
            }
            if let Some(post_only) = order.post_only {
                order_builder = order_builder.post_only(post_only);
            }

            let build_started = Instant::now();
            let signable = match order_builder.build().await {
                Ok(signable) => signable,
                Err(first_err) => {
                    let first_anyhow = anyhow::anyhow!(first_err.to_string());
                    if Self::is_rate_limit_error(&first_anyhow) {
                        let retry_after_ms =
                            self.set_tick_metadata_backoff(token_key.as_str()).await;
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        let remaining_ms = retry_after_ms.saturating_sub(now_ms);
                        anyhow::bail!(
                            "tick metadata build rate-limited token={} remaining_ms={} err={}",
                            order.token_id,
                            remaining_ms.max(0),
                            first_anyhow
                        );
                    }
                    self.invalidate_prewarmed_order_metadata(token_id).await;
                    let mut retry_builder = handle
                        .client
                        .limit_order()
                        .token_id(token_id)
                        .size(size)
                        .price(price)
                        .side(side)
                        .order_type(effective_order_type.clone());
                    if matches!(effective_order_type, OrderType::GTD) {
                        let exp = expiration_dt.ok_or_else(|| {
                            anyhow::anyhow!("order_type=GTD requires expiration_ts or default TTL")
                        })?;
                        retry_builder = retry_builder.expiration(exp);
                    }
                    if let Some(post_only) = order.post_only {
                        retry_builder = retry_builder.post_only(post_only);
                    }
                    retry_builder.build().await.map_err(|retry_err| {
                        anyhow::anyhow!(
                            "Failed to build order: first={} retry={}",
                            first_err,
                            retry_err
                        )
                    })?
                }
            };
            build_order_ms += build_started.elapsed().as_millis() as i64;
            let sign_started = Instant::now();
            let signed = handle
                .client
                .sign(&handle.signer, signable)
                .await
                .context("Failed to sign order")?;
            sign_ms += sign_started.elapsed().as_millis() as i64;
            signed
        };
        let build_sign_ms = build_order_ms.saturating_add(sign_ms);

        Ok(PreparedSignedOrder {
            signed_order,
            timing: PlaceOrderHandleTiming {
                prewarm_ms,
                prewarm_cache_hit,
                build_order_ms,
                sign_ms,
                build_sign_ms,
                post_order_ms: 0,
                order_signer_primary_post_ms: 0,
                order_signer_fallback_post_ms: 0,
                order_signer_primary_attempts: 0,
                order_signer_fallback_attempts: 0,
                order_signer_final_route: None,
            },
            effective_order_type: effective_order_type.to_string(),
            token_id: order.token_id.clone(),
            side: order.side.clone(),
            size: order.size.clone(),
            price: order.price.clone(),
        })
    }

    pub async fn cancel_order(&self, order_id: &str) -> Result<CancelOrdersResponse> {
        let attempts = Self::order_retry_attempts();
        let base_delay_ms = Self::order_retry_base_delay_ms();
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=attempts {
            let handle = self.get_or_create_clob_client().await?;
            // Use batch cancel endpoint even for single-order requests.
            // CLOB single-order delete can return ambiguous "not found" responses for valid IDs.
            match handle.client.cancel_orders(&[order_id]).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let err = anyhow::anyhow!(e.to_string());
                    let is_auth = Self::is_auth_error(&err);
                    let is_transient = Self::is_transient_order_error(&err);
                    if Self::should_invalidate_auth_cache(&err) {
                        self.invalidate_clob_client_cache().await;
                    }
                    if (is_auth || is_transient) && attempt < attempts {
                        let shift = attempt.saturating_sub(1).min(6);
                        let delay_ms = base_delay_ms.saturating_mul(1u64 << shift);
                        warn!(
                            "Transient cancel_order failure (attempt {}/{}): {}. Retrying in {}ms",
                            attempt, attempts, err, delay_ms
                        );
                        last_err = Some(err);
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    return Err(err).context("Failed to cancel order");
                }
            }
        }

        let err = last_err.unwrap_or_else(|| anyhow::anyhow!("cancel_order failed after retries"));
        Err(err).context("Failed to cancel order")
    }

    pub async fn get_order(&self, order_id: &str) -> Result<OpenOrderResponse> {
        if self.ws_enabled {
            if let Some(ws_state) = self.ws_state_snapshot().await {
                if let Some(snapshot) = ws_state
                    .get_order_status(order_id, self.ws_order_stale_ms)
                    .await
                {
                    return Ok(Self::open_order_from_ws_snapshot(snapshot));
                }
            }
        }

        let first_handle = self.get_or_create_clob_client().await?;
        match first_handle.client.order(order_id).await {
            Ok(v) => Ok(v),
            Err(e) => {
                let err = anyhow::anyhow!(e.to_string());
                if Self::should_invalidate_auth_cache(&err) {
                    self.invalidate_clob_client_cache().await;
                    let second_handle = self.get_or_create_clob_client().await?;
                    Ok(second_handle.client.order(order_id).await?)
                } else {
                    Err(err).context("Failed to fetch order")
                }
            }
        }
    }

    pub async fn get_last_trade_snapshot(&self, token_id: &str) -> Option<WsTradeSnapshot> {
        if !self.ws_enabled {
            return None;
        }
        let ws_state = self.ws_state_snapshot().await?;
        ws_state
            .get_last_trade(token_id, self.ws_market_stale_ms)
            .await
    }

    fn open_order_from_ws_snapshot(snapshot: WsOrderStatusSnapshot) -> OpenOrderResponse {
        let now = Utc::now();
        OpenOrderResponse::builder()
            .id(snapshot.order_id)
            .status(snapshot.status)
            .owner(polymarket_client_sdk::auth::ApiKey::default())
            .maker_address(polymarket_client_sdk::types::Address::ZERO)
            .market(snapshot.market)
            .asset_id(snapshot.asset_id)
            .side(snapshot.side)
            .original_size(snapshot.original_size.max(Decimal::ZERO))
            .size_matched(snapshot.size_matched.max(Decimal::ZERO))
            .price(snapshot.price.max(Decimal::ZERO))
            .associate_trades(Vec::new())
            .outcome(snapshot.outcome)
            .created_at(now)
            .expiration(now + chrono::Duration::minutes(5))
            .order_type(OrderType::GTC)
            .build()
    }

    pub async fn cancel_orders(&self, order_ids: &[&str]) -> Result<CancelOrdersResponse> {
        let attempts = Self::order_retry_attempts();
        let base_delay_ms = Self::order_retry_base_delay_ms();
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=attempts {
            let handle = self.get_or_create_clob_client().await?;
            match handle.client.cancel_orders(order_ids).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let err = anyhow::anyhow!(e.to_string());
                    let is_auth = Self::is_auth_error(&err);
                    let is_transient = Self::is_transient_order_error(&err);
                    if Self::should_invalidate_auth_cache(&err) {
                        self.invalidate_clob_client_cache().await;
                    }
                    if (is_auth || is_transient) && attempt < attempts {
                        let shift = attempt.saturating_sub(1).min(6);
                        let delay_ms = base_delay_ms.saturating_mul(1u64 << shift);
                        warn!(
                            "Transient cancel_orders failure (attempt {}/{}): {}. Retrying in {}ms",
                            attempt, attempts, err, delay_ms
                        );
                        last_err = Some(err);
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    return Err(err).context("Failed to cancel orders");
                }
            }
        }

        let err = last_err.unwrap_or_else(|| anyhow::anyhow!("cancel_orders failed after retries"));
        Err(err).context("Failed to cancel orders")
    }

    pub async fn cancel_all_orders(&self) -> Result<CancelOrdersResponse> {
        let attempts = Self::order_retry_attempts();
        let base_delay_ms = Self::order_retry_base_delay_ms();
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=attempts {
            let handle = self.get_or_create_clob_client().await?;
            match handle.client.cancel_all_orders().await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let err = anyhow::anyhow!(e.to_string());
                    let is_auth = Self::is_auth_error(&err);
                    let is_transient = Self::is_transient_order_error(&err);
                    if Self::should_invalidate_auth_cache(&err) {
                        self.invalidate_clob_client_cache().await;
                    }
                    if (is_auth || is_transient) && attempt < attempts {
                        let shift = attempt.saturating_sub(1).min(6);
                        let delay_ms = base_delay_ms.saturating_mul(1u64 << shift);
                        warn!(
                            "Transient cancel_all_orders failure (attempt {}/{}): {}. Retrying in {}ms",
                            attempt, attempts, err, delay_ms
                        );
                        last_err = Some(err);
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        continue;
                    }
                    return Err(err).context("Failed to cancel all orders");
                }
            }
        }

        let err =
            last_err.unwrap_or_else(|| anyhow::anyhow!("cancel_all_orders failed after retries"));
        Err(err).context("Failed to cancel all orders")
    }

    pub async fn cancel_market_orders(&self, asset_id: &str) -> Result<CancelOrdersResponse> {
        let asset_u256 = Self::parse_u256_id(asset_id, "asset_id")?;
        let req = CancelMarketOrderRequest::builder()
            .asset_id(asset_u256)
            .build();
        let first_handle = self.get_or_create_clob_client().await?;
        match first_handle.client.cancel_market_orders(&req).await {
            Ok(v) => Ok(v),
            Err(e) => {
                let err = anyhow::anyhow!(e.to_string());
                if Self::should_invalidate_auth_cache(&err) {
                    self.invalidate_clob_client_cache().await;
                    let second_handle = self.get_or_create_clob_client().await?;
                    Ok(second_handle.client.cancel_market_orders(&req).await?)
                } else {
                    Err(err).context("Failed to cancel market orders")
                }
            }
        }
    }

    /// Discover current BTC or ETH 15-minute market
    /// Similar to main bot's discover_market function
    pub async fn discover_current_market(&self, asset: &str) -> Result<Option<String>> {
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Calculate current 15-minute period
        let current_period = (current_time / 900) * 900;

        // Try to find market for current period and a few previous periods (in case market is slightly delayed)
        for offset in 0..=2 {
            let period_to_check = current_period - (offset * 900);
            let slug = format!("{}-updown-15m-{}", asset.to_lowercase(), period_to_check);

            // Try to get market by slug
            if let Ok(market) = self.get_market_by_slug(&slug).await {
                return Ok(Some(market.condition_id));
            }
        }

        // If slug-based discovery fails, try searching active markets
        if let Ok(markets) = self.get_all_active_markets(50).await {
            let asset_upper = asset.to_uppercase();
            for market in markets {
                // Check if this is a BTC/ETH 15-minute market
                if market
                    .slug
                    .contains(&format!("{}-updown-15m", asset.to_lowercase()))
                    || market
                        .question
                        .to_uppercase()
                        .contains(&format!("{} 15", asset_upper))
                {
                    return Ok(Some(market.condition_id));
                }
            }
        }

        Ok(None)
    }

    /// Get all tokens in portfolio with balance > 0
    /// Get all tokens in portfolio with balance > 0, checking recent markets (not just current)
    /// Checks current market and recent past markets (up to 10 periods = 2.5 hours) to find tokens from resolved markets
    pub async fn get_portfolio_tokens_all(
        &self,
        _btc_condition_id: Option<&str>,
        _eth_condition_id: Option<&str>,
    ) -> Result<Vec<(String, f64, String, String)>> {
        let mut tokens_with_balance = Vec::new();

        // Check BTC markets (current + recent past)
        println!("🔍 Scanning BTC markets (current + recent past)...");
        for offset in 0..=10 {
            // Check last 10 periods (2.5 hours)
            let current_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let period_to_check = (current_time / 900) * 900 - (offset * 900);
            let slug = format!("btc-updown-15m-{}", period_to_check);

            if let Ok(market) = self.get_market_by_slug(&slug).await {
                let condition_id = market.condition_id.clone();
                println!(
                    "   📊 Checking BTC market: {} (period: {})",
                    &condition_id[..16],
                    period_to_check
                );

                if let Ok(market_details) = self.get_market(&condition_id).await {
                    for token in &market_details.tokens {
                        match self.check_balance_only(&token.token_id).await {
                            Ok(balance) => {
                                let balance_decimal =
                                    balance / rust_decimal::Decimal::from(1_000_000u64);
                                let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                                if balance_f64 > 0.0 {
                                    let description = format!(
                                        "BTC {} (period: {})",
                                        token.outcome, period_to_check
                                    );
                                    tokens_with_balance.push((
                                        token.token_id.clone(),
                                        balance_f64,
                                        description,
                                        condition_id.clone(),
                                    ));
                                    println!(
                                        "      ✅ Found token with balance: {} shares",
                                        balance_f64
                                    );
                                }
                            }
                            Err(_) => continue,
                        }
                    }
                }
            }
        }

        // Check ETH markets (current + recent past)
        println!("🔍 Scanning ETH markets (current + recent past)...");
        for offset in 0..=10 {
            // Check last 10 periods (2.5 hours)
            let current_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let period_to_check = (current_time / 900) * 900 - (offset * 900);
            let slug = format!("eth-updown-15m-{}", period_to_check);

            if let Ok(market) = self.get_market_by_slug(&slug).await {
                let condition_id = market.condition_id.clone();
                println!(
                    "   📊 Checking ETH market: {} (period: {})",
                    &condition_id[..16],
                    period_to_check
                );

                if let Ok(market_details) = self.get_market(&condition_id).await {
                    for token in &market_details.tokens {
                        match self.check_balance_only(&token.token_id).await {
                            Ok(balance) => {
                                let balance_decimal =
                                    balance / rust_decimal::Decimal::from(1_000_000u64);
                                let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                                if balance_f64 > 0.0 {
                                    let description = format!(
                                        "ETH {} (period: {})",
                                        token.outcome, period_to_check
                                    );
                                    tokens_with_balance.push((
                                        token.token_id.clone(),
                                        balance_f64,
                                        description,
                                        condition_id.clone(),
                                    ));
                                    println!(
                                        "      ✅ Found token with balance: {} shares",
                                        balance_f64
                                    );
                                }
                            }
                            Err(_) => continue,
                        }
                    }
                }
            }
        }

        // Check Solana markets (current + recent past)
        println!("🔍 Scanning Solana markets (current + recent past)...");
        for offset in 0..=10 {
            // Check last 10 periods (2.5 hours)
            let current_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let period_to_check = (current_time / 900) * 900 - (offset * 900);

            // Try both slug formats
            let slugs = vec![
                format!("solana-updown-15m-{}", period_to_check),
                format!("sol-updown-15m-{}", period_to_check),
            ];

            for slug in slugs {
                if let Ok(market) = self.get_market_by_slug(&slug).await {
                    let condition_id = market.condition_id.clone();
                    println!(
                        "   📊 Checking Solana market: {} (period: {})",
                        &condition_id[..16],
                        period_to_check
                    );

                    if let Ok(market_details) = self.get_market(&condition_id).await {
                        for token in &market_details.tokens {
                            match self.check_balance_only(&token.token_id).await {
                                Ok(balance) => {
                                    let balance_decimal =
                                        balance / rust_decimal::Decimal::from(1_000_000u64);
                                    let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                                    if balance_f64 > 0.0 {
                                        let description = format!(
                                            "Solana {} (period: {})",
                                            token.outcome, period_to_check
                                        );
                                        tokens_with_balance.push((
                                            token.token_id.clone(),
                                            balance_f64,
                                            description,
                                            condition_id.clone(),
                                        ));
                                        println!(
                                            "      ✅ Found token with balance: {} shares",
                                            balance_f64
                                        );
                                    }
                                }
                                Err(_) => continue,
                            }
                        }
                    }
                    break; // Found a valid market, no need to try other slug format
                }
            }
        }

        Ok(tokens_with_balance)
    }

    /// Fetch live wallet positions from Polymarket Data API and return only
    /// redeemable token balances (`redeemable=true`, `size>0`).
    fn resolve_data_api_wallet_address(&self, wallet_address: Option<&str>) -> Result<String> {
        if let Some(explicit_wallet) = wallet_address
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(explicit_wallet.to_ascii_lowercase());
        }

        if let Some(proxy_wallet) = self
            .proxy_wallet_address
            .as_ref()
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(proxy_wallet.to_ascii_lowercase());
        }

        self.trading_account_address()
            .map(|value| value.to_ascii_lowercase())
            .context("wallet address is required for data-api query")
    }

    /// Fetch live wallet positions from Polymarket Data API.
    pub async fn get_wallet_positions_live(
        &self,
        wallet_address: Option<&str>,
    ) -> Result<Vec<Value>> {
        let wallet = self.resolve_data_api_wallet_address(wallet_address)?;
        let attempts = Self::wallet_positions_retry_attempts();
        let retry_base_ms = Self::wallet_positions_retry_base_ms();
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=attempts {
            match self.get_wallet_positions_live_once(wallet.as_str()).await {
                Ok(rows) => return Ok(rows),
                Err(err) => {
                    let with_attempt = err.context(format!(
                        "wallet positions scan failed for wallet {} (attempt {}/{})",
                        wallet, attempt, attempts
                    ));
                    last_err = Some(with_attempt);
                    if attempt < attempts {
                        let shift = attempt.saturating_sub(1).min(6);
                        let delay_ms = retry_base_ms.saturating_mul(1u64 << shift);
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            anyhow::anyhow!(
                "wallet positions scan failed after {} attempts for wallet {}",
                attempts,
                wallet
            )
        }))
    }

    async fn get_wallet_positions_live_once(&self, wallet: &str) -> Result<Vec<Value>> {
        let url = format!("{}/positions", Self::data_api_base_url());
        let response = self
            .send_readonly_get_with_proxy_fallback(|client| {
                client.get(url.as_str()).query(&[("user", wallet)])
            })
            .await
            .context("Failed to fetch wallet positions")?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .context("Failed to read wallet positions response body")?;
        let body_preview = String::from_utf8_lossy(body.as_ref())
            .chars()
            .take(320)
            .collect::<String>();
        if !status.is_success() {
            anyhow::bail!(
                "Failed to fetch wallet positions (status {}): {}",
                status,
                body_preview
            );
        }
        let payload: Value = serde_json::from_slice(body.as_ref()).with_context(|| {
            format!(
                "Failed to parse wallet positions body ({} bytes): {}",
                body.len(),
                body_preview
            )
        })?;
        if let Some(rows) = payload.as_array() {
            return Ok(rows.clone());
        }
        if let Some(rows) = payload.get("data").and_then(Value::as_array) {
            return Ok(rows.clone());
        }
        anyhow::bail!(
            "Unexpected wallet positions response shape: {}",
            payload.to_string()
        );
    }

    /// Fetch live wallet positions from Polymarket Data API and return only
    /// redeemable token balances (`redeemable=true`, `size>0`).
    pub async fn get_wallet_redeemable_positions(
        &self,
        wallet_address: Option<&str>,
    ) -> Result<Vec<WalletRedeemablePosition>> {
        let rows = self.get_wallet_positions_live(wallet_address).await?;

        let mut out: Vec<WalletRedeemablePosition> = Vec::new();
        for row in &rows {
            let redeemable = row
                .get("redeemable")
                .and_then(|value| {
                    if let Some(v) = value.as_bool() {
                        Some(v)
                    } else if let Some(v) = value.as_i64() {
                        Some(v != 0)
                    } else if let Some(v) = value.as_u64() {
                        Some(v != 0)
                    } else {
                        value.as_str().and_then(|raw| {
                            let normalized = raw.trim().to_ascii_lowercase();
                            match normalized.as_str() {
                                "1" | "true" | "yes" | "on" => Some(true),
                                "0" | "false" | "no" | "off" => Some(false),
                                _ => None,
                            }
                        })
                    }
                })
                .unwrap_or(false);
            if !redeemable {
                continue;
            }
            let condition_id = value_string(row, &["conditionId", "condition_id"]);
            let token_id = value_string(row, &["asset", "token_id"]);
            let shares = value_f64(row, &["size", "position_size"]).unwrap_or(0.0);
            if condition_id.is_empty()
                || token_id.is_empty()
                || !shares.is_finite()
                || shares <= 0.0
            {
                continue;
            }
            out.push(WalletRedeemablePosition {
                condition_id,
                token_id,
                shares,
            });
        }
        Ok(out)
    }

    /// Automatically discovers current BTC and ETH markets if condition IDs are not provided
    pub async fn get_portfolio_tokens(
        &self,
        btc_condition_id: Option<&str>,
        eth_condition_id: Option<&str>,
    ) -> Result<Vec<(String, f64, String)>> {
        let mut tokens_with_balance = Vec::new();

        // Discover BTC market if not provided
        let btc_condition_id_owned: Option<String> = if let Some(id) = btc_condition_id {
            Some(id.to_string())
        } else {
            println!("🔍 Discovering current BTC 15-minute market...");
            match self.discover_current_market("BTC").await {
                Ok(Some(id)) => {
                    println!("   ✅ Found BTC market: {}", id);
                    Some(id)
                }
                Ok(None) => {
                    println!("   ⚠️  Could not find current BTC market");
                    None
                }
                Err(e) => {
                    eprintln!("   ❌ Error discovering BTC market: {}", e);
                    None
                }
            }
        };

        // Discover ETH market if not provided
        let eth_condition_id_owned: Option<String> = if let Some(id) = eth_condition_id {
            Some(id.to_string())
        } else {
            println!("🔍 Discovering current ETH 15-minute market...");
            match self.discover_current_market("ETH").await {
                Ok(Some(id)) => {
                    println!("   ✅ Found ETH market: {}", id);
                    Some(id)
                }
                Ok(None) => {
                    println!("   ⚠️  Could not find current ETH market");
                    None
                }
                Err(e) => {
                    eprintln!("   ❌ Error discovering ETH market: {}", e);
                    None
                }
            }
        };

        // Check BTC market tokens
        if let Some(ref btc_condition_id) = btc_condition_id_owned {
            println!(
                "📊 Checking BTC market tokens for condition: {}",
                btc_condition_id
            );
            if let Ok(btc_market) = self.get_market(btc_condition_id).await {
                println!(
                    "   ✅ Found {} tokens in BTC market",
                    btc_market.tokens.len()
                );
                for token in &btc_market.tokens {
                    println!(
                        "   🔍 Checking balance for token: {} ({})",
                        token.outcome,
                        &token.token_id[..16]
                    );
                    match self.check_balance_only(&token.token_id).await {
                        Ok(balance) => {
                            let balance_decimal =
                                balance / rust_decimal::Decimal::from(1_000_000u64);
                            let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                            println!("      Balance: {:.6} shares", balance_f64);
                            if balance_f64 > 0.0 {
                                tokens_with_balance.push((
                                    token.token_id.clone(),
                                    balance_f64,
                                    format!("BTC {}", token.outcome),
                                ));
                                println!("      ✅ Found token with balance!");
                            }
                        }
                        Err(e) => {
                            println!("      ⚠️  Failed to check balance: {}", e);
                            // Skip tokens that fail balance check (might not exist or network error)
                            continue;
                        }
                    }
                }
            } else {
                eprintln!("   ❌ Failed to fetch BTC market details");
            }
        }

        // Check ETH market tokens
        if let Some(ref eth_condition_id) = eth_condition_id_owned {
            println!(
                "📊 Checking ETH market tokens for condition: {}",
                eth_condition_id
            );
            if let Ok(eth_market) = self.get_market(eth_condition_id).await {
                println!(
                    "   ✅ Found {} tokens in ETH market",
                    eth_market.tokens.len()
                );
                for token in &eth_market.tokens {
                    println!(
                        "   🔍 Checking balance for token: {} ({})",
                        token.outcome,
                        &token.token_id[..16]
                    );
                    match self.check_balance_only(&token.token_id).await {
                        Ok(balance) => {
                            let balance_decimal =
                                balance / rust_decimal::Decimal::from(1_000_000u64);
                            let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
                            println!("      Balance: {:.6} shares", balance_f64);
                            if balance_f64 > 0.0 {
                                tokens_with_balance.push((
                                    token.token_id.clone(),
                                    balance_f64,
                                    format!("ETH {}", token.outcome),
                                ));
                                println!("      ✅ Found token with balance!");
                            }
                        }
                        Err(e) => {
                            println!("      ⚠️  Failed to check balance: {}", e);
                            // Skip tokens that fail balance check
                            continue;
                        }
                    }
                }
            } else {
                eprintln!("   ❌ Failed to fetch ETH market details");
            }
        }

        Ok(tokens_with_balance)
    }

    /// Check USDC balance and allowance for buying tokens
    /// Returns (usdc_balance, usdc_allowance) as Decimal values
    /// For BUY orders, you need USDC balance and USDC allowance to the Exchange contract
    pub async fn check_usdc_balance_allowance(
        &self,
    ) -> Result<(rust_decimal::Decimal, rust_decimal::Decimal)> {
        // Use shared authenticated client and retry once to tolerate transient CLOB errors.
        // This avoids per-request re-auth churn on manual/open checks.
        use polymarket_client_sdk::clob::types::request::BalanceAllowanceRequest;
        use polymarket_client_sdk::clob::types::AssetType;
        let build_request = || {
            // For collateral (USDC), the API expects *no token_id/assetId*.
            // Setting token_id to the USDC ERC20 address triggers a 400 (assetId must be empty).
            BalanceAllowanceRequest::builder()
                .asset_type(AssetType::Collateral)
                .build()
        };

        let now_ms = Utc::now().timestamp_millis();
        let cached_snapshot = self.cached_usdc_balance_allowance.lock().await.clone();
        if let Some((cached_balance, cached_allowance, cached_ts_ms)) = cached_snapshot.as_ref() {
            if now_ms.saturating_sub(*cached_ts_ms) <= USDC_BALANCE_CACHE_HIT_TTL_MS {
                return Ok((cached_balance.clone(), cached_allowance.clone()));
            }
        }

        // Singleflight gate: only one in-flight collateral balance+allowance call.
        let _singleflight_guard = self.usdc_balance_allowance_fetch_lock.lock().await;
        let now_ms = Utc::now().timestamp_millis();
        let cached_snapshot = self.cached_usdc_balance_allowance.lock().await.clone();
        if let Some((cached_balance, cached_allowance, cached_ts_ms)) = cached_snapshot.as_ref() {
            if now_ms.saturating_sub(*cached_ts_ms) <= USDC_BALANCE_CACHE_HIT_TTL_MS {
                return Ok((cached_balance.clone(), cached_allowance.clone()));
            }
        }

        let handle = self
            .get_or_create_clob_client()
            .await
            .context("Failed to initialize CLOB client for USDC balance check")?;

        let balance_allowance_result = match handle.client.balance_allowance(build_request()).await
        {
            Ok(resp) => Ok(resp),
            Err(first_err) => {
                let first_msg = first_err.to_string();
                let first_anyhow = anyhow::anyhow!(first_msg.clone());
                if Self::should_invalidate_auth_cache(&first_anyhow) {
                    warn!(
                        "USDC balance check auth failure: {}. Re-authenticating once.",
                        first_msg
                    );
                    self.invalidate_clob_client_cache().await;
                    let fresh_handle = self
                        .get_or_create_clob_client()
                        .await
                        .context("Failed to re-authenticate CLOB client for USDC balance check")?;
                    match fresh_handle.client.balance_allowance(build_request()).await {
                            Ok(resp) => Ok(resp),
                            Err(second_err) => Err(anyhow::anyhow!(
                                "Failed to fetch USDC balance and allowance after auth refresh (first_error='{}', second_error='{}')",
                                first_msg,
                                second_err
                            )),
                        }
                } else if Self::is_rate_limit_error(&first_anyhow) {
                    Err(anyhow::anyhow!(
                        "Failed to fetch USDC balance and allowance (rate-limited; skipped immediate retry, first_error='{}')",
                        first_msg
                    ))
                } else {
                    match handle.client.balance_allowance(build_request()).await {
                            Ok(resp) => Ok(resp),
                            Err(second_err) => Err(anyhow::anyhow!(
                                "Failed to fetch USDC balance and allowance after transient retry (first_error='{}', second_error='{}')",
                                first_msg,
                                second_err
                            )),
                        }
                }
            }
        };

        let balance_allowance = match balance_allowance_result {
            Ok(resp) => resp,
            Err(err) => {
                if let Some((cached_balance, cached_allowance, cached_ts_ms)) = cached_snapshot {
                    let age_ms = now_ms.saturating_sub(cached_ts_ms);
                    if age_ms <= USDC_BALANCE_CACHE_FALLBACK_TTL_MS {
                        warn!(
                            "USDC balance check failed; using cached balance snapshot age_ms={} error={}",
                            age_ms, err
                        );
                        return Ok((cached_balance, cached_allowance));
                    }
                }
                return Err(err);
            }
        };

        let balance = balance_allowance.balance;
        // Get allowance for the Exchange contract
        let config = contract_config(POLYGON, false)
            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config"))?;
        let exchange_address = config.exchange;

        // Allowances is a HashMap<Address, String> - prefer the configured exchange key.
        // Some signer/account modes can return the allowance under a different key, so
        // fall back to the max reported allowance when the configured exchange lookup is zero.
        let mut allowance = balance_allowance
            .allowances
            .get(&exchange_address)
            .and_then(|s| rust_decimal::Decimal::from_str(s).ok())
            .unwrap_or(rust_decimal::Decimal::ZERO);
        if allowance <= rust_decimal::Decimal::ZERO {
            let max_reported_allowance = balance_allowance
                .allowances
                .values()
                .filter_map(|raw| rust_decimal::Decimal::from_str(raw).ok())
                .fold(rust_decimal::Decimal::ZERO, |acc, value| {
                    if value > acc {
                        value
                    } else {
                        acc
                    }
                });
            if max_reported_allowance > allowance {
                warn!(
                    "USDC allowance for configured exchange {} missing/zero; using max reported allowance fallback",
                    exchange_address
                );
                allowance = max_reported_allowance;
            }
        }

        *self.cached_usdc_balance_allowance.lock().await = Some((
            balance.clone(),
            allowance.clone(),
            Utc::now().timestamp_millis(),
        ));

        Ok((balance, allowance))
    }

    /// Check token balance only (for redemption/portfolio scanning)
    /// Returns balance as Decimal value
    /// This is faster than check_balance_allowance since it doesn't check allowances
    pub async fn check_balance_only(&self, token_id: &str) -> Result<rust_decimal::Decimal> {
        // Get balance using SDK (only balance, not allowance), using cached authenticated client.
        use polymarket_client_sdk::clob::types::request::BalanceAllowanceRequest;
        use polymarket_client_sdk::clob::types::AssetType;
        let token_u256 = Self::parse_u256_id(token_id, "token_id")?;
        let build_request = || {
            BalanceAllowanceRequest::builder()
                .token_id(token_u256)
                .asset_type(AssetType::Conditional)
                .build()
        };

        let handle = self
            .get_or_create_clob_client()
            .await
            .context("Failed to initialize CLOB client for balance check")?;

        let balance_allowance = match handle.client.balance_allowance(build_request()).await {
            Ok(resp) => resp,
            Err(first_err) => {
                let first_msg = first_err.to_string();
                let first_anyhow = anyhow::anyhow!(first_msg.clone());
                if Self::should_invalidate_auth_cache(&first_anyhow) {
                    warn!(
                        "balance check auth failure for token {}: {}. Re-authenticating once.",
                        token_id, first_msg
                    );
                    self.invalidate_clob_client_cache().await;
                    let fresh_handle = self
                        .get_or_create_clob_client()
                        .await
                        .context("Failed to re-authenticate CLOB client for balance check")?;
                    fresh_handle
                        .client
                        .balance_allowance(build_request())
                        .await
                        .context("Failed to fetch balance after client auth refresh")?
                } else {
                    // Non-auth errors are usually transient transport/rate-limit issues.
                    // Retry once with the same authenticated client to avoid auth-cache churn.
                    handle
                        .client
                        .balance_allowance(build_request())
                        .await
                        .context("Failed to fetch balance after transient retry")?
                }
            }
        };

        Ok(balance_allowance.balance)
    }

    /// Check token balance and allowance before selling
    /// Returns (balance, allowance) as Decimal values
    pub async fn check_balance_allowance(
        &self,
        token_id: &str,
    ) -> Result<(rust_decimal::Decimal, rust_decimal::Decimal)> {
        // Get balance and allowance using SDK, using cached authenticated client.
        use polymarket_client_sdk::clob::types::request::BalanceAllowanceRequest;
        use polymarket_client_sdk::clob::types::AssetType;
        let token_u256 = Self::parse_u256_id(token_id, "token_id")?;
        let build_request = || {
            BalanceAllowanceRequest::builder()
                .token_id(token_u256)
                .asset_type(AssetType::Conditional)
                .build()
        };

        let handle = self
            .get_or_create_clob_client()
            .await
            .context("Failed to initialize CLOB client for balance+allowance check")?;

        let balance_allowance = match handle.client.balance_allowance(build_request()).await {
            Ok(resp) => resp,
            Err(first_err) => {
                let first_msg = first_err.to_string();
                let first_anyhow = anyhow::anyhow!(first_msg.clone());
                if Self::should_invalidate_auth_cache(&first_anyhow) {
                    warn!(
                        "balance+allowance auth failure for token {}: {}. Re-authenticating once.",
                        token_id, first_msg
                    );
                    self.invalidate_clob_client_cache().await;
                    let fresh_handle = self.get_or_create_clob_client().await.context(
                        "Failed to re-authenticate CLOB client for balance+allowance check",
                    )?;
                    fresh_handle
                        .client
                        .balance_allowance(build_request())
                        .await
                        .context("Failed to fetch balance+allowance after client auth refresh")?
                } else {
                    // Non-auth errors should not tear down the shared auth client.
                    handle
                        .client
                        .balance_allowance(build_request())
                        .await
                        .context("Failed to fetch balance+allowance after transient retry")?
                }
            }
        };

        let balance = balance_allowance.balance;

        // Get contract config to check which contract address we should be checking allowance for
        let config = contract_config(POLYGON, false)
            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config"))?;
        let exchange_address = config.exchange;

        // Get allowance for the Exchange contract specifically
        let allowance = balance_allowance
            .allowances
            .get(&exchange_address)
            .and_then(|v| rust_decimal::Decimal::from_str(v).ok())
            .unwrap_or_else(|| {
                // If Exchange contract not found, try to get any allowance (fallback)
                balance_allowance
                    .allowances
                    .values()
                    .next()
                    .and_then(|v| rust_decimal::Decimal::from_str(v).ok())
                    .unwrap_or(rust_decimal::Decimal::ZERO)
            });

        let allowance_diagnostics = std::env::var("EVPOLY_ALLOWANCE_DIAGNOSTICS")
            .ok()
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);

        if allowance_diagnostics {
            eprintln!(
                "   🔍 Checking allowances for {} contracts:",
                balance_allowance.allowances.len()
            );
            for (addr, allowance_str) in &balance_allowance.allowances {
                let allowance_val = rust_decimal::Decimal::from_str(allowance_str)
                    .unwrap_or(rust_decimal::Decimal::ZERO);
                let allowance_f64 =
                    f64::try_from(allowance_val / rust_decimal::Decimal::from(1_000_000u64))
                        .unwrap_or(0.0);
                let is_exchange = *addr == exchange_address;
                eprintln!(
                    "      {}: {:.6} shares {}",
                    if is_exchange {
                        "✅ Exchange"
                    } else {
                        "   Other"
                    },
                    allowance_f64,
                    if is_exchange {
                        format!("(matches Exchange: {:#x})", exchange_address)
                    } else {
                        format!("({:#x})", addr)
                    }
                );
            }

            // Optional on-chain approval diagnostics (disabled by default in hot paths).
            let is_approved_for_all = match self.check_is_approved_for_all().await {
                Ok(true) => {
                    eprintln!("   ✅ setApprovalForAll: SET (Exchange can spend all tokens)");
                    true
                }
                Ok(false) => {
                    eprintln!("   ❌ setApprovalForAll: NOT SET (Exchange cannot spend tokens)");
                    false
                }
                Err(e) => {
                    eprintln!("   ⚠️  Could not check setApprovalForAll: {}", e);
                    false
                }
            };

            if is_approved_for_all {
                eprintln!(
                    "   💡 Note: setApprovalForAll is SET - individual token allowance ({:.6}) doesn't matter for selling",
                    f64::try_from(allowance / rust_decimal::Decimal::from(1_000_000u64))
                        .unwrap_or(0.0)
                );
            }
        }

        Ok((balance, allowance))
    }

    /// Refresh cached allowance data for a specific outcome token before selling.
    ///
    /// Per Polymarket: setApprovalForAll() is general approval, but for selling you need
    /// CTF (outcome tokens) approval for CTF Exchange tracked **per token**. The system
    /// caches allowances per token. Calling update_balance_allowance refreshes the backend's
    /// cached allowance for this specific token, reducing "insufficient allowance" errors
    /// when placing the sell order immediately after.
    ///
    /// Call this right before place_market_order(..., "SELL", ...) for the token you're selling.
    pub async fn update_balance_allowance_for_sell(&self, token_id: &str) -> Result<()> {
        let private_key = self.private_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Private key is required. Please set private_key in config.json")
        })?;

        let signer = LocalSigner::from_str(private_key)
            .context("Failed to create signer from private key")?
            .with_chain_id(Some(POLYGON));

        let mut auth_builder = ClobClient::new(&self.clob_url, ClobConfig::default())
            .context("Failed to create CLOB client")?
            .authentication_builder(&signer);

        if let Some(proxy_addr) = &self.proxy_wallet_address {
            let funder_address = AlloyAddress::parse_checksummed(proxy_addr, None).context(
                format!("Failed to parse proxy_wallet_address: {}", proxy_addr),
            )?;
            auth_builder = auth_builder.funder(funder_address);
            let sig_type = match self.signature_type {
                Some(1) => SignatureType::Proxy,
                Some(2) => SignatureType::GnosisSafe,
                Some(0) => anyhow::bail!("proxy_wallet_address set but signature_type is 0 (EOA)"),
                None => SignatureType::Proxy,
                Some(n) => anyhow::bail!("Invalid signature_type: {}", n),
            };
            auth_builder = auth_builder.signature_type(sig_type);
        } else if let Some(sig_type_num) = self.signature_type {
            let sig_type = match sig_type_num {
                0 => SignatureType::Eoa,
                1 | 2 => anyhow::bail!(
                    "signature_type {} requires proxy_wallet_address",
                    sig_type_num
                ),
                n => anyhow::bail!("Invalid signature_type: {}", n),
            };
            auth_builder = auth_builder.signature_type(sig_type);
        }

        let client = auth_builder
            .authenticate()
            .await
            .context("Failed to authenticate for update_balance_allowance")?;

        use polymarket_client_sdk::clob::types::request::UpdateBalanceAllowanceRequest;
        use polymarket_client_sdk::clob::types::AssetType;

        // Outcome tokens (conditional tokens) need AssetType::Conditional
        let request = UpdateBalanceAllowanceRequest::builder()
            .token_id(Self::parse_u256_id(token_id, "token_id")?)
            .asset_type(AssetType::Conditional)
            .build();

        client
            .update_balance_allowance(request)
            .await
            .context("Failed to update balance/allowance cache for token")?;

        Ok(())
    }

    /// Get the CLOB contract address for Polygon using SDK's contract_config
    /// This is the Exchange contract address that needs to be approved via setApprovalForAll
    #[allow(dead_code)]
    fn get_clob_contract_address(&self) -> Result<String> {
        // Use SDK's contract_config to get the correct Exchange contract address
        let config = contract_config(POLYGON, false)
            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config from SDK"))?;
        Ok(format!("{:#x}", config.exchange))
    }

    fn resolve_trading_account_address(&self) -> Result<AlloyAddress> {
        if let Some(proxy_addr) = &self.proxy_wallet_address {
            return AlloyAddress::parse_checksummed(proxy_addr, None).context(format!(
                "Failed to parse proxy_wallet_address: {}",
                proxy_addr
            ));
        }
        let private_key = self
            .private_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Private key required to resolve account address"))?;
        let signer = LocalSigner::from_str(private_key)
            .context("Failed to create signer from private key")?
            .with_chain_id(Some(POLYGON));
        Ok(signer.address())
    }

    pub fn trading_account_address(&self) -> Result<String> {
        Ok(format!("{:#x}", self.resolve_trading_account_address()?))
    }

    pub async fn get_user_activity(&self, user: &str, limit: usize) -> Result<Vec<Value>> {
        let normalized_user = user.trim();
        if normalized_user.is_empty() {
            anyhow::bail!("activity user is empty");
        }
        let limit_clamped = limit.clamp(1, 1_000);
        let limit_str = limit_clamped.to_string();
        let response = self
            .client
            .get("https://data-api.polymarket.com/activity")
            .query(&[("user", normalized_user), ("limit", limit_str.as_str())])
            .send()
            .await
            .context("Failed to fetch user activity")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("Failed to read user activity body")?;
        if !status.is_success() {
            anyhow::bail!(
                "Failed to fetch user activity (status {}): {}",
                status,
                body
            );
        }
        let parsed: Value = serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse user activity response: {}", body))?;
        match parsed {
            Value::Array(rows) => Ok(rows),
            other => anyhow::bail!(
                "Unexpected user activity response shape: {}",
                other.to_string()
            ),
        }
    }

    /// Get the CTF contract address for Polygon using SDK's contract_config
    /// This is where we call setApprovalForAll()
    #[allow(dead_code)]
    fn get_ctf_contract_address(&self) -> Result<String> {
        // Use SDK's contract_config to get the correct CTF contract address
        let config = contract_config(POLYGON, false)
            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config from SDK"))?;
        Ok(format!("{:#x}", config.conditional_tokens))
    }

    /// Check if setApprovalForAll was already set for the Exchange contract
    /// Returns true if the Exchange is already approved to manage all tokens
    pub async fn check_is_approved_for_all(&self) -> Result<bool> {
        let config = contract_config(POLYGON, false)
            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config from SDK"))?;

        let ctf_contract_address = config.conditional_tokens;
        let exchange_address = config.exchange;

        // Determine which address to check (proxy wallet or EOA)
        let account_to_check = if let Some(proxy_addr) = &self.proxy_wallet_address {
            AlloyAddress::parse_checksummed(proxy_addr, None).context(format!(
                "Failed to parse proxy_wallet_address: {}",
                proxy_addr
            ))?
        } else {
            let private_key = self
                .private_key
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Private key required to check approval"))?;
            let signer = LocalSigner::from_str(private_key)
                .context("Failed to create signer from private key")?
                .with_chain_id(Some(POLYGON));
            signer.address()
        };

        let mut last_error: Option<anyhow::Error> = None;
        for rpc_url in Self::polygon_rpc_http_urls() {
            let provider = match ProviderBuilder::new().connect(rpc_url.as_str()).await {
                Ok(provider) => provider,
                Err(e) => {
                    last_error = Some(
                        anyhow::Error::new(e)
                            .context(format!("Failed to connect to Polygon RPC {}", rpc_url)),
                    );
                    continue;
                }
            };

            let ctf = IERC1155::new(ctf_contract_address, provider);
            match ctf
                .isApprovedForAll(account_to_check, exchange_address)
                .call()
                .await
            {
                Ok(approved) => return Ok(approved),
                Err(e) => {
                    last_error = Some(
                        anyhow::Error::new(e)
                            .context(format!("Failed to check isApprovedForAll via {}", rpc_url)),
                    );
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!(
                "Failed to check isApprovedForAll: no usable Polygon RPC endpoint available"
            )
        }))
    }

    /// Check all approvals for all contracts (like SDK's check_approvals example)
    /// Returns a vector of (contract_name, usdc_approved, ctf_approved) tuples
    pub async fn check_all_approvals(&self) -> Result<Vec<(String, bool, bool)>> {
        use polymarket_client_sdk::types::address;

        const USDC_ADDRESS: Address = address!("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174");

        let config = contract_config(POLYGON, false)
            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config from SDK"))?;
        let neg_risk_config = contract_config(POLYGON, true)
            .ok_or_else(|| anyhow::anyhow!("Failed to get neg risk contract config from SDK"))?;

        // Determine which address to check (proxy wallet or EOA)
        let account_to_check = if let Some(proxy_addr) = &self.proxy_wallet_address {
            AlloyAddress::parse_checksummed(proxy_addr, None).context(format!(
                "Failed to parse proxy_wallet_address: {}",
                proxy_addr
            ))?
        } else {
            let private_key = self
                .private_key
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Private key required to check approval"))?;
            let signer = LocalSigner::from_str(private_key)
                .context("Failed to create signer from private key")?
                .with_chain_id(Some(POLYGON));
            signer.address()
        };

        let mut provider_opt = None;
        let mut last_connect_error: Option<anyhow::Error> = None;
        for rpc_url in Self::polygon_rpc_http_urls() {
            match ProviderBuilder::new().connect(rpc_url.as_str()).await {
                Ok(provider) => {
                    provider_opt = Some(provider);
                    break;
                }
                Err(e) => {
                    last_connect_error = Some(
                        anyhow::Error::new(e)
                            .context(format!("Failed to connect to Polygon RPC {}", rpc_url)),
                    );
                }
            }
        }
        let provider = provider_opt.ok_or_else(|| {
            last_connect_error.unwrap_or_else(|| {
                anyhow::anyhow!("Failed to connect to Polygon RPC: no endpoint configured")
            })
        })?;

        let usdc = IERC20::new(USDC_ADDRESS, provider.clone());
        let ctf = IERC1155::new(config.conditional_tokens, provider.clone());

        // Collect all contracts that need approval
        let mut targets: Vec<(&str, Address)> = vec![
            ("CTF Exchange", config.exchange),
            ("Neg Risk CTF Exchange", neg_risk_config.exchange),
        ];

        if let Some(adapter) = neg_risk_config.neg_risk_adapter {
            targets.push(("Neg Risk Adapter", adapter));
        }

        let mut results = Vec::new();

        for (name, target) in &targets {
            let usdc_approved = usdc
                .allowance(account_to_check, *target)
                .call()
                .await
                .map(|allowance| allowance > U256::ZERO)
                .unwrap_or(false);

            let ctf_approved = ctf
                .isApprovedForAll(account_to_check, *target)
                .call()
                .await
                .unwrap_or(false);

            results.push((name.to_string(), usdc_approved, ctf_approved));
        }

        Ok(results)
    }

    /// Set both USDC (ERC-20) and CTF (ERC-1155) approvals for all trading contracts:
    /// - CTF Exchange
    /// - Neg Risk CTF Exchange
    /// - Neg Risk Adapter (if present)
    ///
    /// Proxy wallets use relayer submit flow (gasless); EOAs use direct RPC transactions.
    pub async fn set_all_trading_approvals(&self) -> Result<()> {
        use polymarket_client_sdk::types::address;

        const USDC_ADDRESS: Address = address!("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174");

        let config = contract_config(POLYGON, false)
            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config from SDK"))?;
        let neg_risk_config = contract_config(POLYGON, true)
            .ok_or_else(|| anyhow::anyhow!("Failed to get neg risk contract config from SDK"))?;

        let ctf_contract_address = config.conditional_tokens;
        let mut targets: Vec<(&str, Address)> = vec![
            ("CTF Exchange", config.exchange),
            ("Neg Risk CTF Exchange", neg_risk_config.exchange),
        ];
        if let Some(adapter) = neg_risk_config.neg_risk_adapter {
            targets.push(("Neg Risk Adapter", adapter));
        }

        eprintln!("🔐 Setting approvals for {} contract(s)", targets.len());
        eprintln!("   USDC contract: {:#x}", USDC_ADDRESS);
        eprintln!("   CTF contract:  {:#x}", ctf_contract_address);

        if self.proxy_wallet_address.is_some() {
            // Single relayer submit with all approvals batched as proxy calls.
            let mut calls: Vec<(AlloyAddress, String)> = Vec::with_capacity(targets.len() * 2);
            for (name, target) in &targets {
                eprintln!("   - {} ({:#x})", name, target);
                calls.push((
                    USDC_ADDRESS,
                    Self::encode_erc20_approve_call_data(*target, U256::MAX),
                ));
                calls.push((
                    ctf_contract_address,
                    Self::encode_set_approval_for_all_call_data(*target, true),
                ));
            }

            let metadata = format!(
                "Set USDC+CTF approvals for {} trading contracts",
                targets.len()
            );
            let relayer_request = self
                .build_relayer_request_for_calls(calls, metadata.as_str())
                .await?;
            let _ = self
                .submit_relayer_redeem_request(relayer_request, "Relayer approvals batch", false)
                .await?;
            eprintln!("✅ Relayer approval batch confirmed");
            return Ok(());
        }

        // EOA fallback path: submit direct on-chain txs.
        let private_key = self
            .private_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Private key required for direct approval txs"))?;
        let signer = LocalSigner::from_str(private_key)
            .context("Failed to create signer from private key")?
            .with_chain_id(Some(POLYGON));
        let mut provider_opt = None;
        let mut last_connect_error: Option<anyhow::Error> = None;
        for rpc_url in Self::polygon_rpc_http_urls() {
            match ProviderBuilder::new()
                .wallet(signer.clone())
                .connect(rpc_url.as_str())
                .await
            {
                Ok(provider) => {
                    provider_opt = Some(provider);
                    break;
                }
                Err(e) => {
                    last_connect_error = Some(
                        anyhow::Error::new(e)
                            .context(format!("Failed to connect to Polygon RPC {}", rpc_url)),
                    );
                }
            }
        }
        let provider = provider_opt.ok_or_else(|| {
            last_connect_error.unwrap_or_else(|| {
                anyhow::anyhow!("Failed to connect to Polygon RPC: no endpoint configured")
            })
        })?;

        let usdc = IERC20::new(USDC_ADDRESS, provider.clone());
        let ctf = IERC1155::new(ctf_contract_address, provider.clone());

        for (name, target) in &targets {
            eprintln!("   - {} ({:#x})", name, target);
            let usdc_tx = usdc
                .approve(*target, U256::MAX)
                .send()
                .await
                .with_context(|| format!("USDC approve send failed for {}", name))?
                .watch()
                .await
                .with_context(|| format!("USDC approve confirm failed for {}", name))?;
            eprintln!("     ✅ USDC approved tx={:#x}", usdc_tx);

            let ctf_tx = ctf
                .setApprovalForAll(*target, true)
                .send()
                .await
                .with_context(|| format!("CTF setApprovalForAll send failed for {}", name))?
                .watch()
                .await
                .with_context(|| format!("CTF setApprovalForAll confirm failed for {}", name))?;
            eprintln!("     ✅ CTF approved tx={:#x}", ctf_tx);
        }

        eprintln!("✅ All direct approvals confirmed");
        Ok(())
    }

    fn encode_set_approval_for_all_call_data(operator: AlloyAddress, approved: bool) -> String {
        let function_selector = Self::function_selector("setApprovalForAll(address,bool)");
        let mut encoded_params = Vec::with_capacity(64);

        let mut operator_bytes = [0u8; 32];
        operator_bytes[12..].copy_from_slice(operator.as_slice());
        encoded_params.extend_from_slice(&operator_bytes);

        let approved_num: u64 = if approved { 1 } else { 0 };
        encoded_params.extend_from_slice(&U256::from(approved_num).to_be_bytes::<32>());

        let mut call_data = function_selector.to_vec();
        call_data.extend_from_slice(&encoded_params);
        format!("0x{}", hex::encode(call_data))
    }

    fn encode_erc20_approve_call_data(spender: AlloyAddress, amount: U256) -> String {
        let function_selector = Self::function_selector("approve(address,uint256)");
        let mut encoded_params = Vec::with_capacity(64);

        let mut spender_bytes = [0u8; 32];
        spender_bytes[12..].copy_from_slice(spender.as_slice());
        encoded_params.extend_from_slice(&spender_bytes);
        encoded_params.extend_from_slice(&amount.to_be_bytes::<32>());

        let mut call_data = function_selector.to_vec();
        call_data.extend_from_slice(&encoded_params);
        format!("0x{}", hex::encode(call_data))
    }

    /// Approve the CLOB contract for ALL conditional tokens using CTF contract's setApprovalForAll()
    /// This is the recommended way to avoid allowance errors for all tokens at once
    /// Based on SDK example: https://github.com/Polymarket/rs-clob-client/blob/main/examples/approvals.rs
    ///
    /// For proxy wallets: Uses Polymarket's relayer to execute the transaction (gasless)
    /// For EOA wallets: Uses direct RPC call
    ///
    /// IMPORTANT: The wallet that needs MATIC for gas:
    /// - If using proxy_wallet_address: Uses relayer (gasless, no MATIC needed)
    /// - If NOT using proxy_wallet_address: The wallet derived from private_key needs MATIC
    pub async fn set_approval_for_all_clob(&self) -> Result<()> {
        // Get addresses from SDK's contract_config
        // Based on SDK example: https://github.com/Polymarket/rs-clob-client/blob/main/examples/approvals.rs
        // - config.conditional_tokens = CTF contract (where we call setApprovalForAll)
        // - config.exchange = CTF Exchange (the operator we approve)
        let config = contract_config(POLYGON, false)
            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config from SDK"))?;

        let ctf_contract_address = config.conditional_tokens;
        let exchange_address = config.exchange;

        eprintln!("🔐 Setting approval for all tokens using CTF contract's setApprovalForAll()");
        eprintln!(
            "   CTF Contract (conditional_tokens): {:#x}",
            ctf_contract_address
        );
        eprintln!(
            "   CTF Exchange (exchange/operator): {:#x}",
            exchange_address
        );
        eprintln!(
            "   This will approve the Exchange contract to manage ALL your conditional tokens"
        );

        // For proxy wallets, use relayer (gasless transactions)
        // For EOA wallets, use direct RPC call
        if let Some(proxy_addr) = &self.proxy_wallet_address {
            eprintln!("   🔄 Using Polymarket relayer for proxy wallet (gasless transaction)");
            eprintln!("   Proxy wallet: {}", proxy_addr);

            // Use relayer to execute setApprovalForAll from proxy wallet
            // Based on: https://docs.polymarket.com/developers/builders/relayer-client
            self.set_approval_for_all_via_relayer(ctf_contract_address, exchange_address)
                .await
        } else {
            eprintln!("   🔄 Using direct RPC call for EOA wallet");

            // Check if we have a private key (required for signing)
            let private_key = self.private_key.as_ref()
                .ok_or_else(|| anyhow::anyhow!("Private key is required for token approval. Please set private_key in config.json"))?;

            // Create signer from private key
            let signer = LocalSigner::from_str(private_key)
                .context("Failed to create signer from private key. Ensure private_key is a valid hex string.")?
                .with_chain_id(Some(POLYGON));

            let signer_address = signer.address();
            eprintln!(
                "   💰 Wallet that needs MATIC for gas: {:#x}",
                signer_address
            );

            // Use direct RPC call like SDK example (instead of relayer)
            // Based on: https://github.com/Polymarket/rs-clob-client/blob/main/examples/approvals.rs
            let mut provider_opt = None;
            let mut last_connect_error: Option<anyhow::Error> = None;
            for rpc_url in Self::polygon_rpc_http_urls() {
                match ProviderBuilder::new()
                    .wallet(signer.clone())
                    .connect(rpc_url.as_str())
                    .await
                {
                    Ok(provider) => {
                        provider_opt = Some(provider);
                        break;
                    }
                    Err(e) => {
                        last_connect_error = Some(
                            anyhow::Error::new(e)
                                .context(format!("Failed to connect to Polygon RPC {}", rpc_url)),
                        );
                    }
                }
            }
            let provider = provider_opt.ok_or_else(|| {
                last_connect_error.unwrap_or_else(|| {
                    anyhow::anyhow!("Failed to connect to Polygon RPC: no endpoint configured")
                })
            })?;

            // Create IERC1155 contract instance
            let ctf = IERC1155::new(ctf_contract_address, provider.clone());

            eprintln!("   📤 Sending setApprovalForAll transaction via direct RPC call...");

            // Call setApprovalForAll directly (like SDK example)
            let tx_hash = ctf
                .setApprovalForAll(exchange_address, true)
                .send()
                .await
                .context("Failed to send setApprovalForAll transaction")?
                .watch()
                .await
                .context("Failed to watch setApprovalForAll transaction")?;

            eprintln!("   ✅ Successfully sent setApprovalForAll transaction!");
            eprintln!("   Transaction Hash: {:#x}", tx_hash);

            Ok(())
        }
    }

    /// Set approval for all tokens via Polymarket relayer (for proxy wallets)
    /// Based on: https://docs.polymarket.com/developers/builders/relayer-client
    ///
    /// NOTE: For signature_type 2 (GNOSIS_SAFE), the relayer expects a complex Safe transaction format
    /// with nonce, Safe address derivation, struct hash signing, etc. This implementation uses a
    /// simpler format that may work for signature_type 1 (POLY_PROXY). If you get 400/401 errors
    /// with signature_type 2, the full Safe transaction flow needs to be implemented.
    async fn set_approval_for_all_via_relayer(
        &self,
        ctf_contract_address: Address,
        exchange_address: Address,
    ) -> Result<()> {
        // Check signature_type - warn if using GNOSIS_SAFE (type 2) as it may need different format
        if let Some(2) = self.signature_type {
            eprintln!("   ⚠️  Using signature_type 2 (GNOSIS_SAFE) - relayer may require Safe transaction format");
            eprintln!("   💡 If this fails, the full Safe transaction flow (nonce, Safe address, struct hash) may be needed");
        }

        // Function signature: setApprovalForAll(address operator, bool approved)
        // Function selector: keccak256("setApprovalForAll(address,bool)")[0:4] = 0xa22cb465
        let function_selector =
            hex::decode("a22cb465").context("Failed to decode function selector")?;

        // Encode parameters: (address operator, bool approved)
        let mut encoded_params = Vec::new();

        // Encode operator address (20 bytes, left-padded to 32 bytes)
        let mut operator_bytes = [0u8; 32];
        operator_bytes[12..].copy_from_slice(exchange_address.as_slice());
        encoded_params.extend_from_slice(&operator_bytes);

        // Encode approved (bool) - true = 1, padded to 32 bytes
        let approved_bytes = U256::from(1u64).to_be_bytes::<32>();
        encoded_params.extend_from_slice(&approved_bytes);

        // Combine function selector with encoded parameters
        let mut call_data = function_selector;
        call_data.extend_from_slice(&encoded_params);

        let call_data_hex = format!("0x{}", hex::encode(&call_data));

        eprintln!("   📝 Encoded call data: {}", call_data_hex);

        // Use relayer for gasless transaction. The /execute path returns 404; the
        // builder-relayer-client uses POST /submit. See: Polymarket/builder-relayer-client
        const RELAYER_SUBMIT: &str = "https://relayer-v2.polymarket.com/submit";

        eprintln!("   📤 Sending setApprovalForAll transaction via relayer (POST /submit)...");

        // Build PROXY relayer request (official format).
        let metadata = format!(
            "Set approval for all tokens - approve Exchange contract {:#x}",
            exchange_address
        );
        let relayer_request = self
            .build_proxy_relayer_request(
                ctf_contract_address,
                call_data_hex.as_str(),
                metadata.as_str(),
            )
            .await?;

        let body_string = serde_json::to_string(&relayer_request)
            .context("Failed to serialize relayer request")?;
        let remote_headers = self
            .request_remote_builder_headers("POST", "/submit", body_string.as_str())
            .await?;

        // Send request to relayer
        let response = self
            .client
            .post(RELAYER_SUBMIT)
            .header("User-Agent", "polymarket-trading-bot/1.0")
            .header("POLY_BUILDER_API_KEY", &remote_headers.poly_builder_api_key)
            .header(
                "POLY_BUILDER_TIMESTAMP",
                &remote_headers.poly_builder_timestamp,
            )
            .header(
                "POLY_BUILDER_PASSPHRASE",
                &remote_headers.poly_builder_passphrase,
            )
            .header(
                "POLY_BUILDER_SIGNATURE",
                &remote_headers.poly_builder_signature,
            )
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body_string)
            .send()
            .await
            .context("Failed to send setApprovalForAll request to relayer")?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .context("Failed to read relayer response")?;

        if !status.is_success() {
            let sig_type_hint = if self.signature_type == Some(2) {
                "\n\n   💡 For signature_type 2 (GNOSIS_SAFE), the relayer expects a Safe transaction format:\n\
                  - Get nonce from /nonce endpoint\n\
                  - Derive Safe address from signer\n\
                  - Build SafeTx struct hash\n\
                  - Sign and pack signature\n\
                  - Send: { from, to, proxyWallet, data, nonce, signature, signatureParams, type: \"SAFE\", metadata }\n\
                  \n\
                  Consider using signature_type 1 (POLY_PROXY) if possible, or implement the full Safe flow."
            } else {
                ""
            };

            anyhow::bail!(
                "Relayer rejected setApprovalForAll request (status: {}): {}\n\
                \n\
                CTF Contract Address: {:#x}\n\
                Exchange Contract Address: {:#x}\n\
                Signature Type: {:?}\n\
                \n\
                This may be a relayer endpoint issue, remote-signer authentication problem, or request format mismatch.\n\
                Please verify signer endpoint envs, EVPOLY_BUILDER_REMOTE_SIGNER_TOKEN, and wallet binding.{}",
                status, response_text, ctf_contract_address, exchange_address, self.signature_type, sig_type_hint
            );
        }

        // Parse relayer response
        let relayer_response: serde_json::Value =
            serde_json::from_str(&response_text).context("Failed to parse relayer response")?;

        let transaction_id = relayer_response["transactionID"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing transactionID in relayer response"))?;

        eprintln!("   ✅ Successfully sent setApprovalForAll transaction via relayer!");
        eprintln!("   Transaction ID: {}", transaction_id);
        eprintln!(
            "   💡 The relayer will execute this transaction from your proxy wallet (gasless)"
        );

        // Wait for transaction confirmation (like TypeScript SDK's response.wait())
        eprintln!("   ⏳ Waiting for transaction confirmation...");
        self.wait_for_relayer_transaction(transaction_id).await?;

        Ok(())
    }

    /// Wait for relayer transaction to be confirmed (like TypeScript SDK's response.wait())
    /// Polls the relayer status endpoint until transaction reaches STATE_CONFIRMED or STATE_FAILED
    async fn wait_for_relayer_transaction(&self, transaction_id: &str) -> Result<String> {
        // Based on TypeScript SDK pattern: response.wait() returns transactionHash
        // Relayer states: STATE_NEW, STATE_EXECUTED, STATE_MINED, STATE_CONFIRMED, STATE_FAILED, STATE_INVALID
        // Official endpoint expects query param: /transaction?id=<id>
        let status_url = "https://relayer-v2.polymarket.com/transaction";

        // Poll for transaction confirmation (with timeout)
        let max_wait_seconds = 120;
        let check_interval_seconds = 2;
        let start_time = std::time::Instant::now();

        loop {
            let elapsed = start_time.elapsed().as_secs();
            if elapsed >= max_wait_seconds {
                eprintln!(
                    "   ⏱️  Timeout waiting for relayer confirmation ({}s)",
                    max_wait_seconds
                );
                eprintln!("   💡 Transaction was submitted but confirmation timed out");
                eprintln!("   💡 Check status at: {}", status_url);
                anyhow::bail!(
                    "Relayer transaction confirmation timeout after {} seconds",
                    max_wait_seconds
                );
            }

            // Check transaction status
            match self
                .client
                .get(status_url)
                .query(&[("id", transaction_id)])
                .header("User-Agent", "polymarket-trading-bot/1.0")
                .send()
                .await
            {
                Ok(response) => {
                    if response.status().is_success() {
                        let status_text = response
                            .text()
                            .await
                            .context("Failed to read relayer status response")?;

                        let status_payload: serde_json::Value = serde_json::from_str(&status_text)
                            .context("Failed to parse relayer status response")?;
                        let status_data = status_payload
                            .as_array()
                            .and_then(|arr| arr.first())
                            .cloned()
                            .unwrap_or(status_payload);

                        let state = status_data["state"].as_str().unwrap_or("UNKNOWN");

                        match state {
                            "STATE_CONFIRMED" => {
                                let tx_hash =
                                    status_data["transactionHash"].as_str().unwrap_or("N/A");
                                eprintln!("   ✅ Transaction confirmed! Hash: {}", tx_hash);
                                return Ok(tx_hash.to_string());
                            }
                            "STATE_FAILED" | "STATE_INVALID" => {
                                let error_msg = status_data
                                    .get("errorMsg")
                                    .and_then(|v| v.as_str())
                                    .filter(|v| !v.trim().is_empty())
                                    .or_else(|| {
                                        status_data
                                            .get("metadata")
                                            .and_then(|v| v.as_str())
                                            .filter(|v| !v.trim().is_empty())
                                    })
                                    .unwrap_or("Transaction failed");
                                anyhow::bail!("Relayer transaction failed: {}", error_msg);
                            }
                            "STATE_NEW" | "STATE_EXECUTED" | "STATE_MINED" => {
                                eprintln!(
                                    "   ⏳ Transaction state: {} (elapsed: {}s)",
                                    state, elapsed
                                );
                                tokio::time::sleep(tokio::time::Duration::from_secs(
                                    check_interval_seconds,
                                ))
                                .await;
                                continue;
                            }
                            _ => {
                                eprintln!(
                                    "   ⏳ Transaction state: {} (elapsed: {}s)",
                                    state, elapsed
                                );
                                tokio::time::sleep(tokio::time::Duration::from_secs(
                                    check_interval_seconds,
                                ))
                                .await;
                                continue;
                            }
                        }
                    } else {
                        warn!(
                            "Failed to check relayer status (status: {}): will retry",
                            response.status()
                        );
                        tokio::time::sleep(tokio::time::Duration::from_secs(
                            check_interval_seconds,
                        ))
                        .await;
                        continue;
                    }
                }
                Err(e) => {
                    warn!("Failed to check relayer status: {} - will retry", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(check_interval_seconds))
                        .await;
                    continue;
                }
            }
        }
    }

    pub async fn check_relayer_transaction(&self, transaction_id: &str) -> Result<RedeemResponse> {
        match self.wait_for_relayer_transaction(transaction_id).await {
            Ok(tx_hash) => Ok(RedeemResponse {
                success: true,
                message: Some(format!(
                    "Relayer transaction confirmed. Transaction ID: {}",
                    transaction_id
                )),
                transaction_hash: Some(tx_hash),
                amount_redeemed: None,
            }),
            Err(e) => {
                let err_text = e.to_string().to_ascii_lowercase();
                if err_text.contains("timeout") {
                    Ok(RedeemResponse {
                        success: false,
                        message: Some(format!(
                            "Relayer transaction {} still pending confirmation",
                            transaction_id
                        )),
                        transaction_hash: Some(transaction_id.to_string()),
                        amount_redeemed: None,
                    })
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Fallback: Approve individual tokens (ETH Up/Down, BTC Up/Down) with large allowance
    /// This is used when setApprovalForAll fails via relayer
    /// Triggers SDK auto-approval by placing tiny test sell orders for each token
    pub async fn approve_individual_tokens(
        &self,
        eth_market_data: &crate::models::Market,
        btc_market_data: &crate::models::Market,
    ) -> Result<()> {
        eprintln!("🔄 Fallback: Approving individual tokens with large allowance...");

        // Get token IDs from current markets
        let eth_condition_id = &eth_market_data.condition_id;
        let btc_condition_id = &btc_market_data.condition_id;

        let mut token_ids = Vec::new();

        // Get ETH market tokens
        if let Ok(eth_details) = self.get_market(eth_condition_id).await {
            for token in &eth_details.tokens {
                token_ids.push((token.token_id.clone(), format!("ETH {}", token.outcome)));
            }
        }

        // Get BTC market tokens
        if let Ok(btc_details) = self.get_market(btc_condition_id).await {
            for token in &btc_details.tokens {
                token_ids.push((token.token_id.clone(), format!("BTC {}", token.outcome)));
            }
        }

        if token_ids.is_empty() {
            anyhow::bail!("Could not find any token IDs from current markets");
        }

        eprintln!("   Found {} tokens to approve", token_ids.len());

        // For each token, trigger SDK auto-approval by placing a tiny test sell order
        // The SDK will automatically approve with a large amount (typically max uint256)
        let mut success_count = 0;
        let mut fail_count = 0;

        for (token_id, description) in &token_ids {
            eprintln!("   🔐 Checking {} token balance...", description);

            // Check if user has balance for this token before attempting approval
            match self.check_balance_only(token_id).await {
                Ok(balance) => {
                    let balance_decimal = balance / rust_decimal::Decimal::from(1_000_000u64);
                    let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);

                    if balance_f64 == 0.0 {
                        eprintln!(
                            "   ⏭️  Skipping {} token - no balance (balance: 0)",
                            description
                        );
                        continue; // Skip tokens user doesn't own
                    }

                    eprintln!(
                        "   ✅ {} token has balance: {:.6} - triggering approval...",
                        description, balance_f64
                    );
                }
                Err(e) => {
                    eprintln!(
                        "   ⚠️  Could not check balance for {} token: {} - skipping",
                        description, e
                    );
                    continue; // Skip if we can't check balance
                }
            }

            // Place a tiny sell order (0.01 shares) to trigger SDK's auto-approval
            // This order will likely fail due to size, but it will trigger the approval process
            // Using 0.01 (minimum non-zero with 2 decimal places) instead of 0.000001 which rounds to 0.00
            match self
                .place_market_order(token_id, 0.01, "SELL", Some("FAK"))
                .await
            {
                Ok(_) => {
                    eprintln!("   ✅ {} token approved successfully", description);
                    success_count += 1;
                }
                Err(e) => {
                    // Check if it's an allowance error (which means approval was triggered)
                    let error_str = format!("{}", e);
                    if error_str.contains("balance") || error_str.contains("allowance") {
                        eprintln!("   ✅ {} token approval triggered (order failed but approval succeeded)", description);
                        success_count += 1;
                    } else {
                        eprintln!(
                            "   ⚠️  {} token approval failed: {}",
                            description, error_str
                        );
                        fail_count += 1;
                    }
                }
            }

            // Small delay between approvals
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }

        if success_count > 0 {
            eprintln!(
                "✅ Successfully approved {}/{} tokens with large allowance",
                success_count,
                token_ids.len()
            );
            if fail_count > 0 {
                eprintln!(
                    "   ⚠️  {} tokens failed to approve (will retry on sell if needed)",
                    fail_count
                );
            }
            Ok(())
        } else {
            anyhow::bail!(
                "Failed to approve any tokens. All {} attempts failed.",
                token_ids.len()
            )
        }
    }

    /// Place a market order (FOK/FAK) for immediate execution
    ///
    /// This is used for emergency selling or when you want immediate execution at market price.
    /// Equivalent to JavaScript: client.createAndPostMarketOrder(userMarketOrder)
    ///
    /// Market orders execute immediately at the best available price:
    /// - FOK (Fill-or-Kill): Order must fill completely or be cancelled
    /// - FAK (Fill-and-Kill): Order fills as much as possible, remainder is cancelled
    pub async fn place_market_order(
        &self,
        token_id: &str,
        amount: f64,
        side: &str,
        order_type: Option<&str>, // "FOK" or "FAK", defaults to FOK
    ) -> Result<OrderResponse> {
        // Check if we have a private key (required for signing)
        let private_key = self.private_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Private key is required for order signing. Please set private_key in config.json"
            )
        })?;

        // Create signer from private key
        let signer = LocalSigner::from_str(private_key)
            .context("Failed to create signer from private key. Ensure private_key is a valid hex string.")?
            .with_chain_id(Some(POLYGON));

        // Build authentication builder with proxy wallet support
        let mut auth_builder = ClobClient::new(&self.clob_url, ClobConfig::default())
            .context("Failed to create CLOB client")?
            .authentication_builder(&signer);

        // Configure proxy wallet if provided
        if let Some(proxy_addr) = &self.proxy_wallet_address {
            let funder_address =
                AlloyAddress::parse_checksummed(proxy_addr, None).context(format!(
                "Failed to parse proxy_wallet_address: {}. Ensure it's a valid Ethereum address.",
                proxy_addr
            ))?;

            auth_builder = auth_builder.funder(funder_address);

            // Set signature type based on config or default to Proxy
            // IMPORTANT: The signature type must match what was used when deriving API credentials
            let sig_type = match self.signature_type {
                Some(1) => SignatureType::Proxy,
                Some(2) => SignatureType::GnosisSafe,
                Some(0) => {
                    anyhow::bail!(
                        "Invalid configuration: proxy_wallet_address is set but signature_type is 0 (EOA).\n\
                        For proxy wallets, use signature_type: 1 (POLY_PROXY) for Magic Link/Google login, or 2 (GNOSIS_SAFE) for browser wallet."
                    );
                }
                None => {
                    // Default to Proxy when proxy wallet is set, but warn user
                    eprintln!("⚠️  Using default signature type POLY_PROXY (1) for proxy wallet.");
                    eprintln!("   If you get 'invalid signature' errors, ensure your API credentials were derived with signature type 1.");
                    SignatureType::Proxy
                }
                Some(n) => anyhow::bail!(
                    "Invalid signature_type: {}. Must be 0 (EOA), 1 (Proxy), or 2 (GnosisSafe)",
                    n
                ),
            };

            auth_builder = auth_builder.signature_type(sig_type);
        } else if let Some(sig_type_num) = self.signature_type {
            // If signature type is set but no proxy wallet, validate it's EOA
            let sig_type = match sig_type_num {
                0 => SignatureType::Eoa,
                1 | 2 => anyhow::bail!(
                    "signature_type {} requires proxy_wallet_address to be set",
                    sig_type_num
                ),
                n => anyhow::bail!(
                    "Invalid signature_type: {}. Must be 0 (EOA), 1 (Proxy), or 2 (GnosisSafe)",
                    n
                ),
            };
            auth_builder = auth_builder.signature_type(sig_type);
        }

        // Create CLOB client with authentication (equivalent to: new ClobClient(HOST, CHAIN_ID, signer, apiCreds, signatureType, funderAddress))
        let client = auth_builder
            .authenticate()
            .await
            .context("Failed to authenticate with CLOB API. Check your API credentials.")?;
        let builder_client_primary = self
            .build_attributed_order_client_with_config(
                &signer,
                self.order_attribution_builder_config()?,
            )
            .await
            .context("Failed to initialize primary builder attribution client for market orders")?;
        let builder_client_fallback = self
            .build_attributed_order_client_with_config(
                &signer,
                self.order_attribution_builder_fallback_config()?,
            )
            .await
            .context(
                "Failed to initialize fallback builder attribution client for market orders",
            )?;

        // Convert order side string to SDK Side enum
        let side_enum = match side {
            "BUY" => Side::Buy,
            "SELL" => Side::Sell,
            _ => anyhow::bail!("Invalid order side: {}. Must be 'BUY' or 'SELL'", side),
        };

        // Convert order type (defaults to FOK for immediate execution)
        let order_type_enum = match order_type.unwrap_or("FOK") {
            "FOK" => OrderType::FOK,
            "FAK" => OrderType::FAK,
            _ => OrderType::FOK, // Default to FOK
        };

        use rust_decimal::prelude::*;
        use rust_decimal::{Decimal, RoundingStrategy};

        // Convert amount to Decimal
        // For BUY orders: round to 2 decimal places (USD requirement)
        // For SELL orders: round to 6 decimal places (reasonable precision for shares)
        let amount_decimal = if matches!(side_enum, Side::Buy) {
            // BUY: USD value - round to 2 decimal places (Polymarket requirement for USD)
            Decimal::from_f64_retain(amount)
                .ok_or_else(|| anyhow::anyhow!("Failed to convert amount to Decimal"))?
                .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
        } else {
            // SELL: Shares - round to 2 decimal places (Amount::shares requires <= 2 decimal places)
            // Format to 2 decimal places and parse as Decimal
            let shares_str = format!("{:.2}", amount);
            Decimal::from_str(&shares_str).context(format!(
                "Failed to parse shares '{}' as Decimal",
                shares_str
            ))?
        };

        // For BUY orders, check USDC balance and allowance before placing order
        if matches!(side_enum, Side::Buy) {
            eprintln!("🔍 Checking USDC balance and allowance before BUY order...");
            match self.check_usdc_balance_allowance().await {
                Ok((usdc_balance, usdc_allowance)) => {
                    let usdc_balance_f64 =
                        f64::try_from(usdc_balance / rust_decimal::Decimal::from(1_000_000u64))
                            .unwrap_or(0.0);
                    let usdc_allowance_f64 =
                        f64::try_from(usdc_allowance / rust_decimal::Decimal::from(1_000_000u64))
                            .unwrap_or(0.0);

                    eprintln!("   USDC Balance: ${:.2}", usdc_balance_f64);
                    eprintln!("   USDC Allowance: ${:.2}", usdc_allowance_f64);
                    eprintln!("   Order Amount: ${:.2}", amount_decimal);

                    if usdc_balance_f64 < f64::try_from(amount_decimal).unwrap_or(0.0) {
                        anyhow::bail!(
                            "Insufficient USDC balance for BUY order.\n\
                            Required: ${:.2}, Available: ${:.2}\n\
                            Please deposit USDC to your proxy wallet: {}",
                            amount_decimal,
                            usdc_balance_f64,
                            self.proxy_wallet_address
                                .as_deref()
                                .unwrap_or("your wallet")
                        );
                    }

                    if usdc_allowance_f64 < f64::try_from(amount_decimal).unwrap_or(0.0) {
                        eprintln!(
                            "   ⚠️  USDC allowance (${:.2}) is less than order amount (${:.2})",
                            usdc_allowance_f64, amount_decimal
                        );
                        eprintln!("   💡 The SDK should auto-approve USDC on first attempt, but if this fails, you may need to approve USDC manually.");
                        eprintln!("   💡 Run: cargo run --bin test_allowance -- --check (to check USDC approval status)");
                    }
                }
                Err(e) => {
                    eprintln!(
                        "   ⚠️  Could not check USDC balance/allowance: {} (continuing anyway)",
                        e
                    );
                }
            }
        }

        eprintln!(
            "📤 Creating and posting MARKET order: {} {:.2} {} (type: {:?})",
            side, amount_decimal, token_id, order_type_enum
        );

        // Use actual market order (not limit order)
        // Market orders don't require a price - they execute at the best available market price
        // The SDK handles the price automatically based on current market conditions
        //
        // IMPORTANT: For market orders:
        // - BUY: Use USD value (Amount::usdc) - amount is USD to spend
        // - SELL: Use shares (Amount::shares) - amount is number of shares to sell
        let amount = if matches!(side_enum, Side::Buy) {
            // BUY: amount is USD value to spend
            Amount::usdc(amount_decimal).context("Failed to create Amount from USD value")?
        } else {
            // SELL: amount is number of shares to sell (actual shares, not base units)
            // Ensure the Decimal is positive and non-zero
            if amount_decimal <= Decimal::ZERO {
                anyhow::bail!(
                    "Invalid shares amount: {}. Must be greater than 0.",
                    amount_decimal
                );
            }

            // Debug: Log the exact Decimal value being passed
            eprintln!(
                "   🔍 Creating Amount::shares with Decimal: {} (from f64: {})",
                amount_decimal, amount
            );
            eprintln!(
                "   🔍 Decimal scale: {} (Amount::shares requires <= 2)",
                amount_decimal.scale()
            );

            // Amount::shares() requires Decimal with <= 2 decimal places
            // Round to 2 decimal places if needed
            let rounded_shares = if amount_decimal.scale() > 2 {
                let rounded = amount_decimal.round_dp_with_strategy(
                    2,
                    rust_decimal::RoundingStrategy::MidpointAwayFromZero,
                );
                eprintln!(
                    "   🔄 Rounded from {} to {} (scale: {})",
                    amount_decimal,
                    rounded,
                    rounded.scale()
                );
                rounded
            } else {
                amount_decimal
            };

            // Ensure the Decimal is positive and non-zero
            if rounded_shares <= Decimal::ZERO {
                anyhow::bail!(
                    "Invalid shares amount: {}. Must be greater than 0.",
                    rounded_shares
                );
            }

            Amount::shares(rounded_shares)
                .context(format!("Failed to create Amount from shares: {}. Ensure the value is valid and has <= 2 decimal places.", rounded_shares))?
        };

        // Post order and capture detailed error information
        // For SELL orders, the SDK should handle token approval automatically on the first attempt
        // However, if it fails with allowance error, retry with increasing delays to allow SDK to approve
        // Each conditional token (BTC/ETH) is a separate ERC-20 contract and needs its own approval
        // For SELL orders, try posting with retries for allowance errors
        let mut retry_count = 0;
        let max_retries = if matches!(side_enum, Side::Sell) {
            3
        } else {
            1
        }; // Increased to 3 retries for SELL orders
        let token_id_u256 = Self::parse_u256_id(token_id, "token_id")?;
        let mut use_fallback_signer = self.should_force_aws_order_signer().await;

        let response = loop {
            // Rebuild order builder for each retry (since it's moved when building)
            let order_builder_retry = client
                .market_order()
                .token_id(token_id_u256)
                .amount(amount.clone())
                .side(side_enum)
                .order_type(order_type_enum.clone());

            // Build and sign the order (rebuild for each retry since SignedOrder doesn't implement Clone)
            let order_to_sign = order_builder_retry.build().await?;
            let signed_order = client
                .sign(&signer, order_to_sign)
                .await
                .context("Failed to sign market order")?;

            let result = if use_fallback_signer {
                builder_client_fallback.post_order(signed_order).await
            } else {
                match tokio::time::timeout(
                    tokio::time::Duration::from_millis(ORDER_SIGNER_PRIMARY_FALLBACK_TIMEOUT_MS),
                    builder_client_primary.post_order(signed_order),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => {
                        self.note_primary_order_signer_timeout().await;
                        warn!(
                            "Primary market-order signer timed out after {}ms; switching to AWS fallback signer",
                            ORDER_SIGNER_PRIMARY_FALLBACK_TIMEOUT_MS
                        );
                        use_fallback_signer = true;
                        continue;
                    }
                }
            };

            match result {
                Ok(resp) => {
                    if !use_fallback_signer {
                        self.note_primary_order_signer_success().await;
                    }
                    // Success - break out of retry loop
                    break resp;
                }
                Err(e) => {
                    let primary_err = anyhow::anyhow!("{:?}", e);
                    if !use_fallback_signer {
                        if Self::is_timeout_error(&primary_err) {
                            self.note_primary_order_signer_timeout().await;
                        }
                        if Self::should_fallback_to_aws_signer(&primary_err) {
                            warn!(
                                "Primary market-order signer transient failure; switching to AWS fallback signer: {}",
                                e
                            );
                            use_fallback_signer = true;
                            continue;
                        }
                    }
                    let error_str = format!("{:?}", e);
                    // Separate balance errors from allowance errors
                    // Balance error: You don't own enough tokens (shouldn't retry)
                    // Allowance error: You own tokens but haven't approved contract (should retry - SDK may auto-approve)
                    let is_allowance_error = error_str.contains("allowance")
                        || (error_str.contains("not enough") && error_str.contains("allowance"));
                    let is_balance_error =
                        error_str.contains("balance") && !error_str.contains("allowance");

                    retry_count += 1;

                    // Log the error details
                    eprintln!(
                        "❌ Failed to post market order (attempt {}/{}). Error details: {:?}",
                        retry_count, max_retries, e
                    );
                    eprintln!("   Order details:");
                    eprintln!("      Token ID: {}", token_id);
                    eprintln!("      Side: {}", side);
                    eprintln!("      Amount: ${}", amount_decimal);
                    eprintln!(
                        "      Type: {:?} (Market order - price determined by market)",
                        order_type_enum
                    );

                    // Only retry for allowance errors on SELL orders (not balance errors)
                    // Balance errors mean you don't have the tokens - retrying won't help
                    // Allowance errors mean SDK may need time to auto-approve - retrying can help
                    // CRITICAL: Refresh backend's cached allowance before retrying
                    // The backend checks cached allowance, not on-chain approval directly
                    if is_allowance_error
                        && matches!(side_enum, Side::Sell)
                        && retry_count < max_retries
                    {
                        eprintln!("   ⚠️  Allowance error detected - refreshing backend cache before retry...");
                        eprintln!(
                            "   💡 Backend checks cached allowance, not on-chain approval directly"
                        );
                        if let Err(refresh_err) =
                            self.update_balance_allowance_for_sell(token_id).await
                        {
                            eprintln!(
                                "   ⚠️  Failed to refresh allowance cache: {} (retrying anyway)",
                                refresh_err
                            );
                        } else {
                            eprintln!("   ✅ Allowance cache refreshed - waiting 500ms for backend to process...");
                            // Give backend a moment to process the cache update
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                        }
                        // All retries wait 0.5s for selling
                        let wait_millis = 500;
                        eprintln!(
                            "   🔄 Waiting {}ms before retry (attempt {}/{})...",
                            wait_millis, retry_count, max_retries
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(wait_millis)).await;
                        continue; // Retry the order
                    }

                    // For balance errors, don't retry - return error immediately
                    if is_balance_error {
                        anyhow::bail!(
                            "Insufficient token balance: {}\n\
                            Order details: Side={}, Amount={}, Token ID={}\n\
                            \n\
                            This is a portfolio balance issue - you don't own enough tokens.\n\
                            Retrying won't help. Please check your Polymarket portfolio.",
                            error_str,
                            side,
                            amount_decimal,
                            token_id
                        );
                    }

                    // DISABLED: If we've exhausted retries, try setApprovalForAll before giving up
                    // Temporarily disabled - approval functions are disabled throughout the codebase
                    // if is_allowance_error && matches!(side_enum, Side::Sell) && retry_count >= max_retries {
                    //     eprintln!("\n⚠️  Token allowance issue detected after {} attempts", retry_count);
                    //     eprintln!("   Attempting to approve all tokens using setApprovalForAll()...");
                    //
                    //     // Try to approve all tokens at once using setApprovalForAll
                    //     if let Err(approval_err) = self.set_approval_for_all_clob().await {
                    //         eprintln!("   ⚠️  Failed to set approval for all tokens: {}", approval_err);
                    //         eprintln!("   💡 Each conditional token (BTC/ETH) needs separate approval - SDK may have approved ETH but not BTC");
                    //         eprintln!("   This order will be retried on the next check cycle.");
                    //         eprintln!("   If it continues to fail, you may need to manually approve this token on Polymarket UI.");
                    //     } else {
                    //         eprintln!("   ✅ Successfully approved all tokens via setApprovalForAll()");
                    //         eprintln!("   💡 Retrying sell order after approval...");
                    //         // Wait a moment for approval to propagate
                    //         tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    //         // Retry the order one more time after approval
                    //         continue;
                    //     }
                    // }

                    // Return the error - the bot will retry on the next check cycle
                    if is_allowance_error {
                        if matches!(side_enum, Side::Buy) {
                            // For BUY orders, this is USDC allowance issue
                            anyhow::bail!(
                                "Insufficient USDC allowance for BUY order: {}\n\
                                Order details: Side=BUY, Amount=${}, Token ID={}\n\
                                \n\
                                USDC allowance issue - SDK may need more time to auto-approve USDC.\n\
                                \n\
                                To fix:\n\
                                1. Check USDC approval: cargo run --bin test_allowance -- --check\n\
                                2. Approve USDC manually via Polymarket UI if needed\n\
                                3. Or wait for SDK to auto-approve (will retry on next cycle)\n\
                                \n\
                                This order will be retried on the next check cycle.",
                                error_str, amount_decimal, token_id
                            );
                        } else {
                            // For SELL orders, this is conditional token allowance issue
                            anyhow::bail!(
                                "Insufficient allowance: {}\n\
                                Order details: Side=SELL, Amount={}, Token ID={}\n\
                                \n\
                                Token allowance issue - SDK may need more time to auto-approve.\n\
                                This order will be retried on the next check cycle.",
                                error_str,
                                amount_decimal,
                                token_id
                            );
                        }
                    }

                    anyhow::bail!(
                        "Failed to post market order: {}\n\
                        Order details: Side={}, Amount={}, Token ID={}",
                        e,
                        side,
                        amount_decimal,
                        token_id
                    );
                }
            }
        };

        // Check if the response indicates failure even if the request succeeded
        if !response.success {
            let error_msg = response.error_msg.as_deref().unwrap_or("Unknown error");
            eprintln!("❌ Order rejected by API: {}", error_msg);
            eprintln!("   Order details:");
            eprintln!("      Token ID: {}", token_id);
            eprintln!("      Side: {}", side);
            eprintln!("      Amount: ${}", amount_decimal);
            eprintln!("      Type: Market order (price determined by market)");
            anyhow::bail!(
                "Order was rejected: {}\n\
                    Order details: Token ID={}, Side={}, Amount=${}",
                error_msg,
                token_id,
                side,
                amount_decimal
            );
        }

        // Convert SDK response to our OrderResponse format
        let normalized_order_id = Self::normalize_post_order_id(response.order_id.as_str());
        let order_response = OrderResponse {
            order_id: normalized_order_id.clone(),
            status: response.status.to_string(),
            message: if response.success {
                Some(
                    normalized_order_id
                        .as_deref()
                        .map(|order_id| {
                            format!("Market order executed successfully. Order ID: {}", order_id)
                        })
                        .unwrap_or_else(|| {
                            "Market order executed successfully. Order ID unavailable".to_string()
                        }),
                )
            } else {
                response.error_msg.clone()
            },
        };

        if normalized_order_id.is_some() {
            eprintln!(
                "✅ Market order executed successfully! Order ID: {}",
                response.order_id
            );
        } else {
            warn!("Market order succeeded but returned empty orderID from upstream");
        }

        Ok(order_response)
    }

    fn current_ctf_contract_address() -> Result<AlloyAddress> {
        let config = contract_config(POLYGON, false)
            .ok_or_else(|| anyhow::anyhow!("Failed to get contract config from SDK"))?;
        Ok(config.conditional_tokens)
    }

    fn current_neg_risk_adapter_address() -> Result<Option<AlloyAddress>> {
        let config = contract_config(POLYGON, true)
            .ok_or_else(|| anyhow::anyhow!("Failed to get neg-risk contract config from SDK"))?;
        Ok(config.neg_risk_adapter)
    }

    fn encode_neg_risk_adapter_merge_call_data(
        condition_id: &str,
        pair_amount_shares: f64,
    ) -> Result<String> {
        let condition_id_clean = condition_id.strip_prefix("0x").unwrap_or(condition_id);
        let condition_id_b256 = B256::from_str(condition_id_clean).context(format!(
            "Failed to parse condition_id to B256: {}",
            condition_id
        ))?;
        let amount_raw = (pair_amount_shares.max(0.0) * 1_000_000.0).floor();
        if !amount_raw.is_finite() || amount_raw < 1.0 {
            anyhow::bail!(
                "merge pair amount too small or invalid: pair_shares={}",
                pair_amount_shares
            );
        }
        let amount_raw_u128 = amount_raw as u128;

        // Neg-risk adapter overload: mergePositions(bytes32,uint256)
        let function_selector = Self::function_selector("mergePositions(bytes32,uint256)");
        let mut encoded_params = Vec::new();
        encoded_params.extend_from_slice(condition_id_b256.as_slice());
        encoded_params.extend_from_slice(&U256::from(amount_raw_u128).to_be_bytes::<32>());

        let mut call_data = function_selector.to_vec();
        call_data.extend_from_slice(&encoded_params);
        Ok(format!("0x{}", hex::encode(&call_data)))
    }

    fn encode_neg_risk_adapter_redeem_call_data(
        condition_id: &str,
        amounts_raw: &[u128],
    ) -> Result<String> {
        if amounts_raw.is_empty() {
            anyhow::bail!("neg-risk redeem requires at least one amount");
        }
        let condition_id_clean = condition_id.strip_prefix("0x").unwrap_or(condition_id);
        let condition_id_b256 = B256::from_str(condition_id_clean).context(format!(
            "Failed to parse condition_id to B256: {}",
            condition_id
        ))?;

        // Neg-risk adapter overload: redeemPositions(bytes32,uint256[])
        let function_selector = Self::function_selector("redeemPositions(bytes32,uint256[])");
        let mut encoded_params = Vec::new();
        encoded_params.extend_from_slice(condition_id_b256.as_slice());

        // Dynamic array offset: 2 args in static head => 64 bytes.
        let array_offset = 32 * 2;
        encoded_params.extend_from_slice(&U256::from(array_offset).to_be_bytes::<32>());
        encoded_params.extend_from_slice(&U256::from(amounts_raw.len()).to_be_bytes::<32>());
        for amount in amounts_raw {
            encoded_params.extend_from_slice(&U256::from(*amount).to_be_bytes::<32>());
        }

        let mut call_data = function_selector.to_vec();
        call_data.extend_from_slice(&encoded_params);
        Ok(format!("0x{}", hex::encode(&call_data)))
    }

    fn market_token_ids_for_redeem(market: &crate::models::MarketDetails) -> Vec<String> {
        let mut token_ids: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for token in market.tokens.iter() {
            let token_id = token.token_id.trim();
            if token_id.is_empty() {
                continue;
            }
            if seen.insert(token_id.to_string()) {
                token_ids.push(token_id.to_string());
            }
        }

        token_ids
    }

    async fn encode_neg_risk_adapter_redeem_call_data_for_market(
        &self,
        condition_id: &str,
        market: &crate::models::MarketDetails,
    ) -> Result<String> {
        let token_ids = Self::market_token_ids_for_redeem(market);
        if token_ids.is_empty() {
            anyhow::bail!(
                "neg-risk redeem could not resolve outcome token ids for condition {}",
                condition_id
            );
        }

        let mut amounts_raw: Vec<u128> = Vec::with_capacity(token_ids.len());
        for token_id in token_ids {
            let balance = self
                .check_balance_only(token_id.as_str())
                .await
                .with_context(|| {
                    format!(
                        "failed balance lookup for neg-risk redeem token {} (condition {})",
                        token_id, condition_id
                    )
                })?;
            let normalized = if balance.is_sign_negative() {
                Decimal::ZERO
            } else {
                balance.round_dp_with_strategy(0, RoundingStrategy::ToZero)
            };
            let units_raw = normalized.to_string().parse::<u128>().unwrap_or(0);
            amounts_raw.push(units_raw);
        }

        if amounts_raw.iter().all(|amount| *amount == 0) {
            anyhow::bail!(
                "neg-risk redeem has zero balances for all outcomes (condition {})",
                condition_id
            );
        }

        Self::encode_neg_risk_adapter_redeem_call_data(condition_id, amounts_raw.as_slice())
    }

    fn function_selector(signature: &str) -> [u8; 4] {
        let hash = keccak256(signature.as_bytes());
        [hash[0], hash[1], hash[2], hash[3]]
    }

    fn encode_redeem_positions_call_data(condition_id: &str) -> Result<String> {
        // USDC collateral token address on Polygon.
        let collateral_token =
            AlloyAddress::parse_checksummed("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174", None)
                .context("Failed to parse USDC address")?;
        let condition_id_clean = condition_id.strip_prefix("0x").unwrap_or(condition_id);
        let condition_id_b256 = B256::from_str(condition_id_clean).context(format!(
            "Failed to parse condition_id to B256: {}",
            condition_id
        ))?;
        let parent_collection_id = B256::ZERO;
        let index_sets = [U256::from(1), U256::from(2)];

        // redeemPositions(address,bytes32,bytes32,uint256[]) selector.
        let function_selector =
            Self::function_selector("redeemPositions(address,bytes32,bytes32,uint256[])");
        let mut encoded_params = Vec::new();

        let mut addr_bytes = [0u8; 32];
        addr_bytes[12..].copy_from_slice(collateral_token.as_slice());
        encoded_params.extend_from_slice(&addr_bytes);
        encoded_params.extend_from_slice(parent_collection_id.as_slice());
        encoded_params.extend_from_slice(condition_id_b256.as_slice());

        let array_offset = 32 * 4;
        encoded_params.extend_from_slice(&U256::from(array_offset).to_be_bytes::<32>());
        encoded_params.extend_from_slice(&U256::from(index_sets.len()).to_be_bytes::<32>());
        for idx in index_sets {
            encoded_params.extend_from_slice(&idx.to_be_bytes::<32>());
        }

        let mut call_data = function_selector.to_vec();
        call_data.extend_from_slice(&encoded_params);
        Ok(format!("0x{}", hex::encode(&call_data)))
    }

    fn encode_merge_positions_call_data(
        condition_id: &str,
        pair_amount_shares: f64,
    ) -> Result<String> {
        // USDC collateral token address on Polygon.
        let collateral_token =
            AlloyAddress::parse_checksummed("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174", None)
                .context("Failed to parse USDC address")?;
        let condition_id_clean = condition_id.strip_prefix("0x").unwrap_or(condition_id);
        let condition_id_b256 = B256::from_str(condition_id_clean).context(format!(
            "Failed to parse condition_id to B256: {}",
            condition_id
        ))?;
        let parent_collection_id = B256::ZERO;
        let partition = [U256::from(1), U256::from(2)];
        let amount_raw = (pair_amount_shares.max(0.0) * 1_000_000.0).floor();
        if !amount_raw.is_finite() || amount_raw < 1.0 {
            anyhow::bail!(
                "merge pair amount too small or invalid: pair_shares={}",
                pair_amount_shares
            );
        }
        let amount_raw_u128 = amount_raw as u128;

        // mergePositions(address,bytes32,bytes32,uint256[],uint256) selector.
        let function_selector =
            Self::function_selector("mergePositions(address,bytes32,bytes32,uint256[],uint256)");
        let mut encoded_params = Vec::new();

        let mut addr_bytes = [0u8; 32];
        addr_bytes[12..].copy_from_slice(collateral_token.as_slice());
        encoded_params.extend_from_slice(&addr_bytes);
        encoded_params.extend_from_slice(parent_collection_id.as_slice());
        encoded_params.extend_from_slice(condition_id_b256.as_slice());

        // Partition dynamic array offset (5 args in static head).
        let array_offset = 32 * 5;
        encoded_params.extend_from_slice(&U256::from(array_offset).to_be_bytes::<32>());
        encoded_params.extend_from_slice(&U256::from(amount_raw_u128).to_be_bytes::<32>());
        encoded_params.extend_from_slice(&U256::from(partition.len()).to_be_bytes::<32>());
        for idx in partition {
            encoded_params.extend_from_slice(&idx.to_be_bytes::<32>());
        }

        let mut call_data = function_selector.to_vec();
        call_data.extend_from_slice(&encoded_params);
        Ok(format!("0x{}", hex::encode(&call_data)))
    }

    async fn submit_relayer_redeem_request_with_builder_headers(
        &self,
        relayer_submit_url: &str,
        body_string: &str,
        context_label: &str,
    ) -> Result<String> {
        let remote_headers = self
            .request_remote_builder_headers("POST", "/submit", body_string)
            .await?;

        let response = self
            .client
            .post(relayer_submit_url)
            .header("User-Agent", "polymarket-trading-bot/1.0")
            .header("POLY_BUILDER_API_KEY", &remote_headers.poly_builder_api_key)
            .header(
                "POLY_BUILDER_TIMESTAMP",
                &remote_headers.poly_builder_timestamp,
            )
            .header(
                "POLY_BUILDER_PASSPHRASE",
                &remote_headers.poly_builder_passphrase,
            )
            .header(
                "POLY_BUILDER_SIGNATURE",
                &remote_headers.poly_builder_signature,
            )
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body_string.to_string())
            .send()
            .await
            .context(format!(
                "Failed to send {} request to relayer",
                context_label
            ))?;
        let status = response.status();
        let response_text = response
            .text()
            .await
            .context("Failed to read relayer response")?;
        if !status.is_success() {
            anyhow::bail!(
                "{} failed (status {}): {}",
                context_label,
                status,
                &response_text[..200.min(response_text.len())]
            );
        }
        Ok(response_text)
    }

    async fn submit_relayer_redeem_request_with_api_key(
        &self,
        relayer_submit_url: &str,
        body_string: &str,
        context_label: &str,
        relayer_api_key: &str,
        relayer_api_key_address: &str,
    ) -> Result<String> {
        let response = self
            .client
            .post(relayer_submit_url)
            .header("User-Agent", "polymarket-trading-bot/1.0")
            .header("RELAYER_API_KEY", relayer_api_key)
            .header("RELAYER_API_KEY_ADDRESS", relayer_api_key_address)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body_string.to_string())
            .send()
            .await
            .context(format!(
                "Failed to send {} request to relayer with RELAYER_API_KEY fallback",
                context_label
            ))?;
        let status = response.status();
        let response_text = response
            .text()
            .await
            .context("Failed to read relayer response")?;
        if !status.is_success() {
            anyhow::bail!(
                "{} failed (status {}): {}",
                context_label,
                status,
                &response_text[..200.min(response_text.len())]
            );
        }
        Ok(response_text)
    }

    async fn submit_relayer_redeem_request(
        &self,
        relayer_request: serde_json::Value,
        context_label: &str,
        allow_relayer_api_key_fallback: bool,
    ) -> Result<RedeemResponse> {
        const RELAYER_SUBMIT: &str = "https://relayer-v2.polymarket.com/submit";
        let body_string = serde_json::to_string(&relayer_request)
            .context("Failed to serialize relayer request")?;

        let response_text = if allow_relayer_api_key_fallback {
            match self.relayer_api_key_primary_credentials() {
                Ok(Some((relayer_api_key, relayer_api_key_address))) => {
                    match self
                        .submit_relayer_redeem_request_with_api_key(
                            RELAYER_SUBMIT,
                            body_string.as_str(),
                            context_label,
                            relayer_api_key.as_str(),
                            relayer_api_key_address.as_str(),
                        )
                        .await
                    {
                        Ok(response_text) => response_text,
                        Err(primary_err) => {
                            warn!(
                                "{} RELAYER_API_KEY primary submit failed; retrying with remote signer fallback: {}",
                                context_label, primary_err
                            );
                            self.submit_relayer_redeem_request_with_builder_headers(
                                RELAYER_SUBMIT,
                                body_string.as_str(),
                                context_label,
                            )
                            .await
                            .context(format!(
                                "{} failed via RELAYER_API_KEY primary and remote signer fallback",
                                context_label
                            ))?
                        }
                    }
                }
                Ok(None) => self
                    .submit_relayer_redeem_request_with_builder_headers(
                        RELAYER_SUBMIT,
                        body_string.as_str(),
                        context_label,
                    )
                    .await
                    .context(format!(
                        "{} RELAYER_API_KEY primary not configured; remote signer submit failed",
                        context_label
                    ))?,
                Err(primary_cfg_err) => {
                    warn!(
                        "{} RELAYER_API_KEY primary config invalid ({}); retrying with remote signer fallback",
                        context_label, primary_cfg_err
                    );
                    self.submit_relayer_redeem_request_with_builder_headers(
                        RELAYER_SUBMIT,
                        body_string.as_str(),
                        context_label,
                    )
                    .await
                    .context(format!(
                        "{} failed via invalid RELAYER_API_KEY primary config and remote signer fallback",
                        context_label
                    ))?
                }
            }
        } else {
            self.submit_relayer_redeem_request_with_builder_headers(
                RELAYER_SUBMIT,
                body_string.as_str(),
                context_label,
            )
            .await?
        };

        let relayer_response: serde_json::Value =
            serde_json::from_str(&response_text).context("Failed to parse relayer response")?;
        let transaction_id = relayer_response["transactionID"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing transactionID in relayer response"))?;
        eprintln!(
            "   ✅ {} submitted. tx_id={}",
            context_label, transaction_id
        );
        self.check_relayer_transaction(transaction_id).await
    }

    /// Redeem winning conditional tokens after market resolution.
    pub async fn redeem_tokens(
        &self,
        condition_id: &str,
        _token_id: &str,
        outcome: &str,
    ) -> Result<RedeemResponse> {
        eprintln!(
            "🔄 Redeeming tokens for condition {} (outcome: {})",
            condition_id, outcome
        );
        let ctf_call_data_hex = Self::encode_redeem_positions_call_data(condition_id)?;
        let mut is_neg_risk = false;
        let mut adapter_call_data_hex: Option<String> = None;
        if let Ok(market) = self.get_market(condition_id).await {
            is_neg_risk = market.neg_risk;
            if is_neg_risk {
                match self
                    .encode_neg_risk_adapter_redeem_call_data_for_market(condition_id, &market)
                    .await
                {
                    Ok(call_data_hex) => adapter_call_data_hex = Some(call_data_hex),
                    Err(err) => warn!(
                        "Failed to build neg-risk redeem adapter payload for condition {}: {}. Falling back to CTF redeem.",
                        condition_id, err
                    ),
                }
            }
        }

        let mut attempts: Vec<(AlloyAddress, String, &'static str)> = Vec::new();
        if is_neg_risk {
            if let (Ok(Some(adapter)), Some(adapter_call)) = (
                Self::current_neg_risk_adapter_address(),
                adapter_call_data_hex.clone(),
            ) {
                attempts.push((adapter, adapter_call, "Relayer neg-risk redemption"));
            }
        }
        let ctf_address = Self::current_ctf_contract_address()?;
        attempts.push((ctf_address, ctf_call_data_hex, "Relayer redemption"));

        let mut last_err: Option<anyhow::Error> = None;
        let mut attempted: std::collections::HashSet<AlloyAddress> =
            std::collections::HashSet::new();
        for (target, call_data_hex, context_label) in attempts {
            if !attempted.insert(target) {
                continue;
            }
            let metadata = format!(
                "Redeem {} token for condition {} via {}",
                outcome, condition_id, target
            );
            let relayer_request = self
                .build_proxy_relayer_request(target, call_data_hex.as_str(), metadata.as_str())
                .await?;
            match self
                .submit_relayer_redeem_request(relayer_request, context_label, true)
                .await
            {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    warn!(
                        "Redeem attempt failed for condition {} via {}: {}",
                        condition_id, target, e
                    );
                    last_err = Some(e);
                }
            }
        }

        Err(last_err
            .unwrap_or_else(|| anyhow::anyhow!("Redeem failed: no target contract available")))
    }

    /// Batch redeem multiple resolved conditions in one relayer submit request.
    /// This builds one proxy transaction containing multiple redeemPositions calls.
    pub async fn redeem_tokens_batch(
        &self,
        conditions: &[RedeemConditionRequest],
    ) -> Result<RedeemResponse> {
        if conditions.is_empty() {
            anyhow::bail!("redeem_tokens_batch called with empty conditions");
        }

        let ctf_address = Self::current_ctf_contract_address()?;
        let adapter_address = Self::current_neg_risk_adapter_address().ok().flatten();
        let mut seen: HashSet<String> = HashSet::new();
        let mut calls: Vec<(AlloyAddress, String)> = Vec::with_capacity(conditions.len());
        let mut label_preview: Vec<String> = Vec::new();
        for item in conditions {
            let normalized = item.condition_id.trim().to_string();
            if normalized.is_empty() || !seen.insert(normalized.clone()) {
                continue;
            }
            let mut target = ctf_address;
            let mut call_data_hex = Self::encode_redeem_positions_call_data(normalized.as_str())?;
            if let Some(adapter) = adapter_address {
                if let Ok(market) = self.get_market(normalized.as_str()).await {
                    if market.neg_risk {
                        match self
                            .encode_neg_risk_adapter_redeem_call_data_for_market(
                                normalized.as_str(),
                                &market,
                            )
                            .await
                        {
                            Ok(adapter_call) => {
                                target = adapter;
                                call_data_hex = adapter_call;
                            }
                            Err(err) => warn!(
                                "Failed to build neg-risk redeem adapter payload for condition {}: {}. Falling back to CTF redeem.",
                                normalized, err
                            ),
                        }
                    }
                }
            }
            calls.push((target, call_data_hex));
            if label_preview.len() < 4 {
                label_preview.push(format!(
                    "{}:{}",
                    item.outcome_label,
                    &normalized[..normalized.len().min(10)]
                ));
            }
        }
        if calls.is_empty() {
            anyhow::bail!("redeem_tokens_batch has no unique condition ids to submit");
        }

        let metadata = format!(
            "Batch redeem {} conditions [{}{}]",
            calls.len(),
            label_preview.join(","),
            if calls.len() > label_preview.len() {
                format!(",+{}", calls.len() - label_preview.len())
            } else {
                String::new()
            }
        );
        eprintln!(
            "🔄 Batch redeem submit: {} unique conditions in one relayer request",
            calls.len()
        );
        let relayer_request = self
            .build_relayer_request_for_calls(calls, metadata.as_str())
            .await?;
        self.submit_relayer_redeem_request(relayer_request, "Relayer batch redemption", true)
            .await
    }

    /// Merge complete sets of Up and Down tokens for a condition into USDC.
    /// Burns `pair_amount_shares` of complete sets via CTF mergePositions.
    pub async fn merge_complete_sets(
        &self,
        condition_id: &str,
        pair_amount_shares: f64,
    ) -> Result<RedeemResponse> {
        eprintln!(
            "🔄 Merging complete sets for condition {} (pair_shares: {:.6})",
            condition_id, pair_amount_shares
        );
        let ctf_call_data_hex =
            Self::encode_merge_positions_call_data(condition_id, pair_amount_shares)?;
        let adapter_call_data_hex =
            Self::encode_neg_risk_adapter_merge_call_data(condition_id, pair_amount_shares).ok();
        let mut is_neg_risk = false;
        if let Ok(market) = self.get_market(condition_id).await {
            is_neg_risk = market.neg_risk;
        }

        let mut attempts: Vec<(AlloyAddress, String, &'static str)> = Vec::new();
        if is_neg_risk {
            if let (Ok(Some(adapter)), Some(adapter_call)) = (
                Self::current_neg_risk_adapter_address(),
                adapter_call_data_hex.clone(),
            ) {
                attempts.push((adapter, adapter_call, "Relayer neg-risk merge"));
            }
        }
        let ctf_address = Self::current_ctf_contract_address()?;
        attempts.push((ctf_address, ctf_call_data_hex, "Relayer merge"));

        let mut last_err: Option<anyhow::Error> = None;
        let mut attempted: std::collections::HashSet<AlloyAddress> =
            std::collections::HashSet::new();
        for (target, call_data_hex, context_label) in attempts {
            if !attempted.insert(target) {
                continue;
            }
            let metadata = format!(
                "Merge {:.6} complete sets for condition {} via {}",
                pair_amount_shares, condition_id, target
            );
            let relayer_request = self
                .build_proxy_relayer_request(target, call_data_hex.as_str(), metadata.as_str())
                .await?;
            match self
                .submit_relayer_redeem_request(relayer_request, context_label, true)
                .await
            {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    warn!(
                        "Merge attempt failed for condition {} via {}: {}",
                        condition_id, target, e
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| anyhow::anyhow!("Merge failed: no target contract available")))
    }

    pub async fn merge_complete_sets_batch(
        &self,
        conditions: &[MergeConditionRequest],
    ) -> Result<RedeemResponse> {
        if conditions.is_empty() {
            anyhow::bail!("merge_complete_sets_batch called with empty conditions");
        }
        let ctf_address = Self::current_ctf_contract_address()?;
        let adapter_address = Self::current_neg_risk_adapter_address().ok().flatten();
        let mut seen: HashSet<String> = HashSet::new();
        let mut calls: Vec<(AlloyAddress, String)> = Vec::with_capacity(conditions.len());
        let mut label_preview: Vec<String> = Vec::new();

        for item in conditions {
            let normalized = item.condition_id.trim().to_string();
            if normalized.is_empty()
                || !item.pair_shares.is_finite()
                || item.pair_shares <= 0.0
                || !seen.insert(normalized.clone())
            {
                continue;
            }

            let mut target = ctf_address;
            let mut call_data_hex =
                Self::encode_merge_positions_call_data(normalized.as_str(), item.pair_shares)?;
            if let Some(adapter) = adapter_address {
                if let Ok(market) = self.get_market(normalized.as_str()).await {
                    if market.neg_risk {
                        if let Ok(adapter_call) = Self::encode_neg_risk_adapter_merge_call_data(
                            normalized.as_str(),
                            item.pair_shares,
                        ) {
                            target = adapter;
                            call_data_hex = adapter_call;
                        }
                    }
                }
            }
            calls.push((target, call_data_hex));
            if label_preview.len() < 4 {
                label_preview.push(format!(
                    "{}:{:.3}",
                    &normalized[..normalized.len().min(10)],
                    item.pair_shares
                ));
            }
        }

        if calls.is_empty() {
            anyhow::bail!("merge_complete_sets_batch has no valid unique conditions to submit");
        }

        let metadata = format!(
            "Batch merge {} conditions [{}{}]",
            calls.len(),
            label_preview.join(","),
            if calls.len() > label_preview.len() {
                format!(",+{}", calls.len() - label_preview.len())
            } else {
                String::new()
            }
        );
        let relayer_request = self
            .build_relayer_request_for_calls(calls, metadata.as_str())
            .await?;
        self.submit_relayer_redeem_request(relayer_request, "Relayer batch merge", true)
            .await
    }
}

fn value_string(row: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| row.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_default()
}

fn value_string_opt(row: &Value, keys: &[&str]) -> Option<String> {
    let value = value_string(row, keys);
    (!value.is_empty()).then_some(value)
}

fn value_f64(row: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        let value = row.get(*key)?;
        if let Some(v) = value.as_f64() {
            return Some(v);
        }
        if let Some(v) = value.as_i64() {
            return Some(v as f64);
        }
        if let Some(v) = value.as_u64() {
            return Some(v as f64);
        }
        value
            .as_str()
            .and_then(|raw| raw.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite())
    })
}

fn reward_rate_from_gamma_row(row: &Value) -> f64 {
    let mut max_rate = 0.0_f64;

    if let Some(rewards_config) = row.get("rewards_config").and_then(Value::as_array) {
        for entry in rewards_config {
            if let Some(rate) = entry
                .get("rate_per_day")
                .and_then(Value::as_f64)
                .or_else(|| {
                    entry
                        .get("rate_per_day")
                        .and_then(Value::as_i64)
                        .map(|v| v as f64)
                })
            {
                if rate.is_finite() && rate > max_rate {
                    max_rate = rate;
                }
            }
        }
    }

    if let Some(clob_rewards) = row.get("clobRewards").and_then(Value::as_array) {
        for entry in clob_rewards {
            let rate = entry
                .get("rewardsDailyRate")
                .and_then(Value::as_f64)
                .or_else(|| {
                    entry
                        .get("rewardsDailyRate")
                        .and_then(Value::as_i64)
                        .map(|v| v as f64)
                })
                .or_else(|| value_f64(entry, &["rewards_daily_rate"]));
            if let Some(rate) = rate.filter(|v| v.is_finite() && *v > max_rate) {
                max_rate = rate;
            }
        }
    }

    max_rate
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::dyn_abi::Eip712Domain;
    use alloy::primitives::Signature;
    use alloy::sol_types::SolStruct as _;
    use polymarket_client_sdk::auth::Uuid;
    use polymarket_client_sdk::clob::types::Order as ClobOrder;
    use std::borrow::Cow;

    #[test]
    fn best_bid_ask_from_orderbook_handles_worst_first_arrays() {
        let orderbook = OrderBook {
            bids: vec![
                OrderBookEntry {
                    price: rust_decimal::Decimal::new(1, 2), // 0.01
                    size: rust_decimal::Decimal::new(100, 0),
                },
                OrderBookEntry {
                    price: rust_decimal::Decimal::new(40, 2), // 0.40
                    size: rust_decimal::Decimal::new(100, 0),
                },
                OrderBookEntry {
                    price: rust_decimal::Decimal::new(25, 2), // 0.25
                    size: rust_decimal::Decimal::new(100, 0),
                },
            ],
            asks: vec![
                OrderBookEntry {
                    price: rust_decimal::Decimal::new(99, 2), // 0.99
                    size: rust_decimal::Decimal::new(100, 0),
                },
                OrderBookEntry {
                    price: rust_decimal::Decimal::new(62, 2), // 0.62
                    size: rust_decimal::Decimal::new(100, 0),
                },
                OrderBookEntry {
                    price: rust_decimal::Decimal::new(55, 2), // 0.55
                    size: rust_decimal::Decimal::new(100, 0),
                },
            ],
        };

        let (best_bid, best_ask) = PolymarketApi::best_bid_ask_from_orderbook(&orderbook);
        assert_eq!(best_bid, Some(rust_decimal::Decimal::new(40, 2)));
        assert_eq!(best_ask, Some(rust_decimal::Decimal::new(55, 2)));
    }

    #[test]
    fn proxy_signature_matches_builder_relayer_reference_vector() {
        // Reference vector from polymarket/builder-relayer-client tests/signatures/index.test.ts.
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let signer = PrivateKeySigner::from_str(
                "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            )
            .expect("signer");
            let from = signer.address();
            let to =
                PolymarketApi::parse_hex_address("0xaB45c5A4B0c941a2F231C04C3f49182e1A254052")
                    .expect("proxy factory");
            let relay_hub =
                PolymarketApi::parse_hex_address("0xD216153c06E857cD7f72665E0aF1d7D82172F494")
                    .expect("relay hub");
            let relay =
                PolymarketApi::parse_hex_address("0xae700edfd9ab986395f3999fe11177b9903a52f1")
                    .expect("relay");

            let proxy_data_hex = "0x34ee979100000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000010000000000000000000000002791bca1f2de4661ed88a30c99a7a9449aa84174000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000044095ea7b30000000000000000000000004d97dcd97ec945f40cf65f87097ace5ea0476045ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff00000000000000000000000000000000000000000000000000000000";

            let struct_hash = PolymarketApi::build_proxy_struct_hash(
                from,
                to,
                proxy_data_hex,
                85_338,
                0,
                relay_hub,
                relay,
            )
            .expect("struct hash");

            let signature = signer
                .sign_message(struct_hash.as_slice())
                .await
                .expect("signature");

            assert_eq!(
                signature.to_string(),
                "0x4c18e2d2294a00d686714aff8e7936ab657cb4655dfccb2b556efadcb7e835f800dc2fecec69c501e29bb36ecb54b4da6b7c410c4dc740a33af2afde2b77297e1b"
            );
        });
    }

    #[test]
    fn ctf_function_selectors_are_stable() {
        assert_eq!(
            PolymarketApi::function_selector("redeemPositions(address,bytes32,bytes32,uint256[])"),
            [0x01, 0xb7, 0x03, 0x7c]
        );
        assert_eq!(
            PolymarketApi::function_selector(
                "mergePositions(address,bytes32,bytes32,uint256[],uint256)"
            ),
            [0x9e, 0x72, 0x12, 0xad]
        );
    }

    fn test_order_domain() -> Eip712Domain {
        let exchange = contract_config(POLYGON, false)
            .expect("contract config for polygon")
            .exchange;
        Eip712Domain {
            name: Some(Cow::Borrowed("Polymarket CTF Exchange")),
            version: Some(Cow::Borrowed("1")),
            chain_id: Some(U256::from(POLYGON)),
            verifying_contract: Some(exchange),
            ..Eip712Domain::default()
        }
    }

    fn fallback_test_order_with_salt(salt: u64) -> ClobOrder {
        let maker = AlloyAddress::from_str("0x1111111111111111111111111111111111111111")
            .expect("maker address");
        let taker = AlloyAddress::from_str("0x0000000000000000000000000000000000000000")
            .expect("taker address");

        let mut order = ClobOrder::default();
        order.salt = U256::from(salt);
        order.maker = maker;
        order.signer = maker;
        order.taker = taker;
        order.tokenId = U256::from(1_u64);
        order.makerAmount = U256::from(10_000_000_u64);
        order.takerAmount = U256::from(5_000_000_u64);
        order.expiration = U256::from(2_000_000_000_u64);
        order.nonce = U256::from(1_u64);
        order.feeRateBps = U256::from(0_u64);
        order.side = Side::Buy as u8;
        order.signatureType = SignatureType::Proxy as u8;
        order
    }

    #[test]
    fn same_signed_payload_keeps_same_hash_and_body() {
        let domain = test_order_domain();
        let order_a = fallback_test_order_with_salt(123_456);
        let order_b = fallback_test_order_with_salt(123_456);

        let hash_a = order_a.eip712_signing_hash(&domain);
        let hash_b = order_b.eip712_signing_hash(&domain);
        assert_eq!(
            hash_a, hash_b,
            "same order fields/salt must produce the same EIP-712 hash"
        );

        let signature = Signature::new(U256::from(1_u64), U256::from(2_u64), false);
        let signed_a = ClobSignedOrder::builder()
            .order(order_a)
            .signature(signature)
            .order_type(OrderType::GTC)
            .owner(Uuid::nil())
            .build();
        let signed_b = ClobSignedOrder::builder()
            .order(order_b)
            .signature(signature)
            .order_type(OrderType::GTC)
            .owner(Uuid::nil())
            .build();

        let body_a = serde_json::to_string(&signed_a).expect("serialize signed order a");
        let body_b = serde_json::to_string(&signed_b).expect("serialize signed order b");
        assert_eq!(
            body_a, body_b,
            "same signed order payload must serialize identically"
        );
    }

    #[test]
    fn rebuilding_with_new_salt_changes_order_hash() {
        let domain = test_order_domain();
        let hash_a = fallback_test_order_with_salt(111_111).eip712_signing_hash(&domain);
        let hash_b = fallback_test_order_with_salt(222_222).eip712_signing_hash(&domain);
        assert_ne!(
            hash_a, hash_b,
            "different salts must produce different EIP-712 hashes"
        );
    }

    #[test]
    fn active_markets_payload_parser_accepts_supported_envelopes() {
        let array_payload = br#"[{"markets": []}]"#;
        let parsed_array = parse_active_markets_payload(array_payload).expect("array parse");
        assert!(parsed_array.is_empty());

        let data_payload = br#"{"data":[{"markets": []}]}"#;
        let parsed_data = parse_active_markets_payload(data_payload).expect("data parse");
        assert!(parsed_data.is_empty());
    }

    #[test]
    fn active_markets_payload_parser_rejects_malformed_json() {
        let malformed = br#"{"data":[{"markets":"bad-shape"}]}"#;
        let err = parse_active_markets_payload(malformed).expect_err("must fail");
        let text = err.to_string();
        assert!(text.contains("Failed to parse active markets payload"));
    }
}
