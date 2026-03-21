use anyhow::{Context, Result};

use crate::api::PolymarketApi;
use crate::config::Config;
use crate::models::Market;

pub async fn get_or_discover_markets(
    api: &PolymarketApi,
    config: &Config,
) -> Result<(Market, Market, Market, Market)> {
    let current_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let mut seen_ids = std::collections::HashSet::new();

    let eth_market = if config.trading.enable_eth_trading {
        discover_market(api, "ETH", &["eth"], current_time, &mut seen_ids)
            .await
            .unwrap_or_else(|_| {
                eprintln!("⚠️  Could not discover ETH market - using fallback");
                eth_disabled_fallback_market()
            })
    } else {
        eprintln!("ℹ️  ETH trading disabled by config - skipping ETH discovery.");
        eth_disabled_fallback_market()
    };
    seen_ids.insert(eth_market.condition_id.clone());

    eprintln!("🔍 Discovering BTC market...");
    let btc_market = discover_market(api, "BTC", &["btc"], current_time, &mut seen_ids)
        .await
        .context("Failed to discover BTC market")?;
    seen_ids.insert(btc_market.condition_id.clone());

    let solana_market = if config.trading.enable_solana_trading {
        eprintln!("🔍 Discovering Solana market...");
        discover_solana_market(api, current_time, &mut seen_ids).await
    } else {
        eprintln!("ℹ️  Solana trading disabled by config - skipping Solana discovery.");
        solana_disabled_fallback_market()
    };

    let xrp_market = if config.trading.enable_xrp_trading {
        eprintln!("🔍 Discovering XRP market...");
        discover_xrp_market(api, current_time, &mut seen_ids).await
    } else {
        eprintln!("ℹ️  XRP trading disabled by config - skipping XRP discovery.");
        xrp_disabled_fallback_market()
    };

    if eth_market.condition_id == btc_market.condition_id
        && eth_market.condition_id != "dummy_eth_fallback"
    {
        anyhow::bail!("ETH and BTC markets have the same condition ID: {}. This is incorrect. Please set condition IDs manually in config.json", eth_market.condition_id);
    }
    if solana_market.condition_id != "dummy_solana_fallback" {
        if eth_market.condition_id == solana_market.condition_id
            && eth_market.condition_id != "dummy_eth_fallback"
        {
            anyhow::bail!("ETH and Solana markets have the same condition ID: {}. This is incorrect. Please set condition IDs manually in config.json", eth_market.condition_id);
        }
        if btc_market.condition_id == solana_market.condition_id {
            anyhow::bail!("BTC and Solana markets have the same condition ID: {}. This is incorrect. Please set condition IDs manually in config.json", btc_market.condition_id);
        }
    }
    if xrp_market.condition_id != "dummy_xrp_fallback" {
        if eth_market.condition_id == xrp_market.condition_id
            && eth_market.condition_id != "dummy_eth_fallback"
        {
            anyhow::bail!("ETH and XRP markets have the same condition ID: {}. This is incorrect. Please set condition IDs manually in config.json", eth_market.condition_id);
        }
        if btc_market.condition_id == xrp_market.condition_id {
            anyhow::bail!("BTC and XRP markets have the same condition ID: {}. This is incorrect. Please set condition IDs manually in config.json", btc_market.condition_id);
        }
        if solana_market.condition_id == xrp_market.condition_id
            && solana_market.condition_id != "dummy_solana_fallback"
        {
            anyhow::bail!("Solana and XRP markets have the same condition ID: {}. This is incorrect. Please set condition IDs manually in config.json", solana_market.condition_id);
        }
    }

    Ok((eth_market, btc_market, solana_market, xrp_market))
}

pub fn eth_disabled_fallback_market() -> Market {
    Market {
        condition_id: "dummy_eth_fallback".to_string(),
        slug: "eth-updown-15m-fallback".to_string(),
        active: false,
        closed: true,
        market_id: None,
        question: "ETH Trading Disabled".to_string(),
        description: None,
        resolution_source: None,
        end_date: None,
        end_date_iso: None,
        end_date_iso_alt: None,
        game_start_time: None,
        start_date: None,
        tokens: None,
        clob_token_ids: None,
        outcomes: None,
        competitive: None,
    }
}

pub fn btc_disabled_fallback_market() -> Market {
    Market {
        condition_id: "dummy_btc_fallback".to_string(),
        slug: "btc-updown-15m-fallback".to_string(),
        active: false,
        closed: true,
        market_id: None,
        question: "BTC Trading Disabled".to_string(),
        description: None,
        resolution_source: None,
        end_date: None,
        end_date_iso: None,
        end_date_iso_alt: None,
        game_start_time: None,
        start_date: None,
        tokens: None,
        clob_token_ids: None,
        outcomes: None,
        competitive: None,
    }
}

pub fn solana_disabled_fallback_market() -> Market {
    Market {
        condition_id: "dummy_solana_fallback".to_string(),
        slug: "solana-updown-15m-fallback".to_string(),
        active: false,
        closed: true,
        market_id: None,
        question: "Solana Trading (disabled or market not found)".to_string(),
        description: None,
        resolution_source: None,
        end_date: None,
        end_date_iso: None,
        end_date_iso_alt: None,
        game_start_time: None,
        start_date: None,
        tokens: None,
        clob_token_ids: None,
        outcomes: None,
        competitive: None,
    }
}

pub fn xrp_disabled_fallback_market() -> Market {
    Market {
        condition_id: "dummy_xrp_fallback".to_string(),
        slug: "xrp-updown-15m-fallback".to_string(),
        active: false,
        closed: true,
        market_id: None,
        question: "XRP Trading (disabled or market not found)".to_string(),
        description: None,
        resolution_source: None,
        end_date: None,
        end_date_iso: None,
        end_date_iso_alt: None,
        game_start_time: None,
        start_date: None,
        tokens: None,
        clob_token_ids: None,
        outcomes: None,
        competitive: None,
    }
}

pub async fn discover_solana_market(
    api: &PolymarketApi,
    current_time: u64,
    seen_ids: &mut std::collections::HashSet<String>,
) -> Market {
    eprintln!("🔍 Discovering Solana market...");
    if let Ok(market) =
        discover_market(api, "Solana", &["solana", "sol"], current_time, seen_ids).await
    {
        return market;
    }
    eprintln!("⚠️  Could not discover Solana 15-minute market (tried: solana, sol). Using fallback - Solana trading disabled for this run.");
    eprintln!("   To enable Solana: set solana_condition_id in config.json, or ensure Polymarket has an active solana/sol 15m up/down market.");
    solana_disabled_fallback_market()
}

pub async fn discover_xrp_market(
    api: &PolymarketApi,
    current_time: u64,
    seen_ids: &mut std::collections::HashSet<String>,
) -> Market {
    eprintln!("🔍 Discovering XRP market...");
    if let Ok(market) = discover_market(api, "XRP", &["xrp"], current_time, seen_ids).await {
        return market;
    }
    eprintln!("⚠️  Could not discover XRP 15-minute market (tried: xrp). Using fallback - XRP trading disabled for this run.");
    eprintln!("   To enable XRP: set xrp_condition_id in config.json, or ensure Polymarket has an active xrp 15m up/down market.");
    xrp_disabled_fallback_market()
}

pub async fn discover_market(
    api: &PolymarketApi,
    market_name: &str,
    slug_prefixes: &[&str],
    current_time: u64,
    seen_ids: &mut std::collections::HashSet<String>,
) -> Result<Market> {
    discover_market_with_previous(
        api,
        market_name,
        slug_prefixes,
        current_time,
        seen_ids,
        true,
    )
    .await
}

pub async fn discover_market_with_previous(
    api: &PolymarketApi,
    market_name: &str,
    slug_prefixes: &[&str],
    current_time: u64,
    seen_ids: &mut std::collections::HashSet<String>,
    include_previous: bool,
) -> Result<Market> {
    let rounded_time = (current_time / 900) * 900;

    for (i, prefix) in slug_prefixes.iter().enumerate() {
        if i > 0 {
            eprintln!(
                "🔍 Trying {} market with slug prefix '{}'...",
                market_name, prefix
            );
        }

        let slug = format!("{}-updown-15m-{}", prefix, rounded_time);
        if let Ok(market) = api.get_market_by_slug(&slug).await {
            if !seen_ids.contains(&market.condition_id) && market.active && !market.closed {
                eprintln!(
                    "Found {} market by slug: {} | Condition ID: {}",
                    market_name, market.slug, market.condition_id
                );
                return Ok(market);
            }
        }

        if include_previous {
            for offset in 1..=3 {
                let try_time = rounded_time - (offset * 900);
                let try_slug = format!("{}-updown-15m-{}", prefix, try_time);
                eprintln!(
                    "Trying previous {} market by slug: {}",
                    market_name, try_slug
                );
                if let Ok(market) = api.get_market_by_slug(&try_slug).await {
                    if !seen_ids.contains(&market.condition_id) && market.active && !market.closed {
                        eprintln!(
                            "Found {} market by slug: {} | Condition ID: {}",
                            market_name, market.slug, market.condition_id
                        );
                        return Ok(market);
                    }
                }
            }
        }
    }

    let tried = slug_prefixes.join(", ");
    anyhow::bail!(
        "Could not find active {} 15-minute up/down market (tried prefixes: {}). Set condition_id in config.json if needed.",
        market_name,
        tried
    )
}
