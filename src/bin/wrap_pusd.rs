use anyhow::{Context, Result};
use clap::Parser;
use polymarket_arbitrage_bot::{Config, PolymarketApi};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(name = "wrap_pusd")]
#[command(
    about = "Wrap USDC.e to pUSD for the configured proxy wallet, then set trading approvals"
)]
struct Args {
    #[arg(long)]
    amount_usd: String,

    #[arg(short, long, default_value = "config.json")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();
    let amount = Decimal::from_str(args.amount_usd.trim())
        .with_context(|| format!("invalid --amount-usd '{}'", args.amount_usd))?;
    if amount <= Decimal::ZERO {
        anyhow::bail!("--amount-usd must be greater than zero");
    }
    let amount_base_units = (amount * Decimal::from(1_000_000u64))
        .round()
        .to_u128()
        .context("amount does not fit into u128 base units")?;

    let config = Config::load(&args.config)?;
    let api = PolymarketApi::new(
        config.polymarket.gamma_api_url.clone(),
        config.polymarket.clob_api_url.clone(),
        config.polymarket.private_key.clone(),
        config.polymarket.proxy_wallet_address.clone(),
        config.polymarket.signature_type,
    );

    println!(
        "Wrapping {} USDC.e to pUSD for configured wallet...",
        amount
    );
    let wrap = api.wrap_usdce_to_pusd_base_units(amount_base_units).await?;
    println!("wrap_success={}", wrap.success);
    if let Some(hash) = wrap.transaction_hash.as_deref() {
        println!("wrap_tx={hash}");
    }

    println!("Setting pUSD and CTF trading approvals...");
    api.set_all_trading_approvals().await?;
    println!("approval_success=true");

    Ok(())
}
