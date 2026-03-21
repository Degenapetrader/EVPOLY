use anyhow::Result;
use clap::Parser;
use polymarket_arbitrage_bot::{models::OrderRequest, Config, PolymarketApi};
use rust_decimal::Decimal;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Parser, Debug)]
#[command(name = "test_limit_order")]
#[command(about = "Test placing a limit order on Polymarket")]
struct Args {
    /// Token ID to buy (optional - if not provided, will discover current BTC Up token)
    #[arg(short, long)]
    token_id: Option<String>,

    /// Price in cents (e.g., 55 = $0.55)
    #[arg(short, long, default_value = "55")]
    price_cents: u64,

    /// Number of shares
    #[arg(short, long, default_value = "5")]
    shares: u64,

    /// Expiration time in minutes
    #[arg(short, long, default_value = "1")]
    expiration_minutes: u64,

    /// Config file path
    #[arg(short, long, default_value = "config.json")]
    config: String,

    /// Side: BUY or SELL
    #[arg(long, default_value = "BUY")]
    side: String,

    /// Run latency test: place N orders and report min/avg/max ms
    #[arg(long)]
    latency_test: Option<u32>,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();
    let config_path = std::path::PathBuf::from(&args.config);
    let config = Config::load(&config_path)?;

    // Create API client
    let api = PolymarketApi::new(
        config.polymarket.gamma_api_url.clone(),
        config.polymarket.clob_api_url.clone(),
        config.polymarket.private_key.clone(),
        config.polymarket.proxy_wallet_address.clone(),
        config.polymarket.signature_type,
    );

    // Authenticate first
    println!("🔐 Authenticating with Polymarket CLOB API...");
    api.authenticate().await?;
    println!();

    // Discover token ID if not provided
    let token_id = if let Some(id) = args.token_id {
        println!("📋 Using provided token ID: {}", id);
        id
    } else {
        println!("🔍 Discovering current BTC market...");
        let condition_id = api
            .discover_current_market("BTC")
            .await?
            .ok_or_else(|| anyhow::anyhow!("Could not find current BTC market"))?;

        println!("   ✅ Found BTC market: {}", condition_id);

        // Get market details to find the "Up" token
        let market = api.get_market(&condition_id).await?;
        let up_token = market
            .tokens
            .iter()
            .find(|t| t.outcome == "Up")
            .ok_or_else(|| anyhow::anyhow!("Could not find 'Up' token in BTC market"))?;

        println!("   ✅ Found BTC Up token: {}", up_token.token_id);
        up_token.token_id.clone()
    };

    // Get current market price for reference
    println!("\n📊 Checking current market price...");
    if let Ok(Some(price_info)) = api.get_best_price(&token_id).await {
        println!("   Current BID: {:?}", price_info.bid);
        println!("   Current ASK: {:?}", price_info.ask);
        if let Some(mid) = price_info.mid_price() {
            println!("   Mid price: {}", mid);
        }
    }

    // Convert price from cents to decimal (e.g., 55 cents = 0.55)
    let price_decimal = Decimal::from(args.price_cents) / Decimal::from(100);

    // Convert shares to decimal
    let size_decimal = Decimal::from(args.shares);

    // Calculate expiration timestamp (current time + expiration_minutes)
    let expiration_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + (args.expiration_minutes * 60);

    println!("\n📝 Order Details:");
    println!("   Token ID: {}", token_id);
    println!("   Side: {}", args.side);
    println!("   Price: {} ({} cents)", price_decimal, args.price_cents);
    println!("   Size: {} shares", args.shares);
    println!(
        "   Expiration: {} minutes (timestamp: {})",
        args.expiration_minutes, expiration_timestamp
    );
    println!();

    // Create order request
    let order = OrderRequest {
        token_id: token_id.clone(),
        side: args.side.clone(),
        size: size_decimal.to_string(),
        price: price_decimal.to_string(),
        order_type: "LIMIT".to_string(),
        expiration_ts: Some(expiration_timestamp as i64),
        post_only: None,
    };

    // Place the order(s) and record latency
    let n_orders = args.latency_test.unwrap_or(1);
    let mut latencies_ms: Vec<u128> = Vec::with_capacity(n_orders as usize);

    for i in 0..n_orders {
        if n_orders > 1 {
            println!("📤 Placing limit order {}/{}...", i + 1, n_orders);
        } else {
            println!("📤 Placing limit order...");
        }

        let start = Instant::now();
        match api.place_order(&order).await {
            Ok(response) => {
                let elapsed_ms = start.elapsed().as_millis();
                latencies_ms.push(elapsed_ms);

                if n_orders == 1 {
                    println!("\n✅ Order placed successfully!");
                    println!("   Order ID: {:?}", response.order_id);
                    println!("   Status: {}", response.status);
                    println!("   Latency: {} ms (submit → ACK)", elapsed_ms);
                    if let Some(msg) = response.message {
                        println!("   Message: {}", msg);
                    }

                    println!(
                        "\n💡 Note: The SDK automatically sets expiration time when signing the order."
                    );
                    println!(
                        "   Your order will expire in {} minutes if not filled.",
                        args.expiration_minutes
                    );
                }
            }
            Err(e) => {
                eprintln!("\n❌ Failed to place order: {}", e);
                eprintln!("\n💡 Troubleshooting:");
                eprintln!("   - Check that you have sufficient USDC balance");
                eprintln!("   - Check that you have USDC allowance to the Exchange contract");
                eprintln!("   - Verify the token ID is correct");
                eprintln!("   - Ensure the market is accepting orders");
                return Err(e);
            }
        }

        // Brief pause between orders in latency test to avoid rate limits
        if n_orders > 1 && i + 1 < n_orders {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    }

    if n_orders > 1 && !latencies_ms.is_empty() {
        latencies_ms.sort();
        let n = latencies_ms.len();
        let sum: u128 = latencies_ms.iter().sum();
        let avg = sum / n as u128;
        println!("\n═══════════════════════════════════════════════════");
        println!("📊 Order Placement Latency (submit → ACK)");
        println!("   Runs: {}", n);
        println!("   Min:  {} ms", latencies_ms[0]);
        println!("   Avg:  {} ms", avg);
        println!("   P50:  {} ms", latencies_ms[n / 2]);
        println!(
            "   P95:  {} ms",
            latencies_ms[((n * 95) / 100).min(n.saturating_sub(1))]
        );
        println!("   Max:  {} ms", latencies_ms[n - 1]);
        println!("═══════════════════════════════════════════════════");
    }

    Ok(())
}
