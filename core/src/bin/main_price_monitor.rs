// Price monitoring only - no trading
// Monitors real-time prices and records them to history folder
use anyhow::Result;
use clap::Parser;
use log::warn;
use polymarket_arbitrage_bot::config::{Args, Config};
use std::sync::Arc;

use polymarket_arbitrage_bot::api::PolymarketApi;
use polymarket_arbitrage_bot::market_discovery::{
    discover_market, discover_solana_market, discover_xrp_market, get_or_discover_markets,
};
use polymarket_arbitrage_bot::monitor::MarketMonitor;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();
    let config = Config::load(&args.config)?;

    eprintln!("═══════════════════════════════════════════════════════════");
    eprintln!("📊 PRICE MONITORING MODE");
    eprintln!("═══════════════════════════════════════════════════════════");
    eprintln!("This mode only monitors and records prices - NO TRADING");
    eprintln!("Prices will be recorded to: history/market_<PERIOD>_prices.toml");
    eprintln!("═══════════════════════════════════════════════════════════");
    eprintln!("");

    // Create API client (no authentication needed for price monitoring)
    let api = Arc::new(PolymarketApi::new(
        config.polymarket.gamma_api_url.clone(),
        config.polymarket.clob_api_url.clone(),
        config.polymarket.private_key.clone(),
        config.polymarket.proxy_wallet_address.clone(),
        config.polymarket.signature_type,
    ));

    // Get market data for BTC, ETH, Solana, and XRP markets
    eprintln!("🔍 Discovering BTC, ETH, Solana, and XRP markets...");
    let (eth_market_data, btc_market_data, solana_market_data, xrp_market_data) =
        get_or_discover_markets(&api, &config).await?;

    eprintln!("✅ Markets discovered:");
    eprintln!(
        "   ETH: {} ({})",
        eth_market_data.slug,
        &eth_market_data.condition_id[..16]
    );
    eprintln!(
        "   BTC: {} ({})",
        btc_market_data.slug,
        &btc_market_data.condition_id[..16]
    );
    eprintln!(
        "   Solana: {} ({})",
        solana_market_data.slug,
        &solana_market_data.condition_id[..16]
    );
    eprintln!(
        "   XRP: {} ({})",
        xrp_market_data.slug,
        &xrp_market_data.condition_id[..16]
    );
    eprintln!("");

    // Create market monitor (simulation_mode = false, but we're not trading anyway)
    let monitor = Arc::new(MarketMonitor::new(
        api.clone(),
        eth_market_data,
        btc_market_data,
        solana_market_data,
        xrp_market_data,
        config.trading.check_interval_ms,
        false, // Not simulation mode, but we're just monitoring
    )?);

    eprintln!("🔄 Starting price monitoring...");
    eprintln!("   Check interval: {}ms", config.trading.check_interval_ms);
    eprintln!("   Price files: history/market_<PERIOD>_prices.toml");
    eprintln!("");

    // Start a background task to detect new 15-minute periods and discover new markets
    let monitor_for_period_check = monitor.clone();
    let api_for_period_check = api.clone();
    tokio::spawn(async move {
        loop {
            let current_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            let current_period = (current_time / 900) * 900;
            let current_market_timestamp = monitor_for_period_check
                .get_current_market_timestamp()
                .await;

            // Check if we need to discover a new market (current market is from a different period)
            if current_market_timestamp != current_period && current_market_timestamp != 0 {
                eprintln!(
                    "🔄 New 15-minute period detected! (Period: {}) Discovering new markets...",
                    current_period
                );

                let mut seen_ids = std::collections::HashSet::new();
                let (eth_id, btc_id) = monitor_for_period_check.get_current_condition_ids().await;
                seen_ids.insert(eth_id);
                seen_ids.insert(btc_id);

                // Discover ETH, BTC, Solana, and XRP for the new period
                let eth_result = discover_market(
                    &api_for_period_check,
                    "ETH",
                    &["eth"],
                    current_time,
                    &mut seen_ids,
                )
                .await;
                let btc_result = discover_market(
                    &api_for_period_check,
                    "BTC",
                    &["btc"],
                    current_time,
                    &mut seen_ids,
                )
                .await;
                let solana_market =
                    discover_solana_market(&api_for_period_check, current_time, &mut seen_ids)
                        .await;
                let xrp_market =
                    discover_xrp_market(&api_for_period_check, current_time, &mut seen_ids).await;

                match (eth_result, btc_result) {
                    (Ok(eth_market), Ok(btc_market)) => {
                        if let Err(e) = monitor_for_period_check
                            .update_markets(eth_market, btc_market, solana_market, xrp_market)
                            .await
                        {
                            warn!("Failed to update markets: {}", e);
                        } else {
                            eprintln!("✅ Markets updated for period {}", current_period);
                        }
                    }
                    _ => {
                        warn!("Failed to discover markets for period {}", current_period);
                    }
                }
            } else {
                // Calculate when next period starts
                let next_period_timestamp = current_period + 900;
                let sleep_duration = if next_period_timestamp > current_time {
                    next_period_timestamp - current_time
                } else {
                    0
                };

                if sleep_duration > 0 {
                    tokio::time::sleep(tokio::time::Duration::from_secs(sleep_duration)).await;
                } else {
                    // Next period already started, discover new market immediately
                    eprintln!("🔄 Next period already started, discovering new market...");
                }
            }
        }
    });

    // Main loop: continuously fetch and record prices
    loop {
        match monitor.fetch_market_data().await {
            Ok(_snapshot) => {
                // Prices are automatically written to history files by MarketMonitor
                // The monitor already logs prices to files, so we don't need to log here
                // Just continue monitoring
            }
            Err(e) => {
                warn!("Error fetching market data: {}", e);
            }
        }

        // Sleep for the configured interval
        tokio::time::sleep(tokio::time::Duration::from_millis(
            config.trading.check_interval_ms,
        ))
        .await;
    }
}
