use alloy::signers::local::LocalSigner;
use alloy::signers::Signer as _;
use anyhow::{Context, Result};
use clap::Parser;
use polymarket_arbitrage_bot::{Config, PolymarketApi};
use polymarket_client_sdk::POLYGON;
use rust_decimal::Decimal;
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(name = "test_allowance")]
#[command(
    about = "Test allowance: setApprovalForAll (on-chain) and/or update_balance_allowance (cache refresh)"
)]
struct Args {
    /// Token ID to test (optional - if not provided, will scan portfolio and list all tokens)
    #[arg(short, long)]
    token_id: Option<String>,

    /// Config file path
    #[arg(short, long, default_value = "config.json")]
    config: String,

    /// Number of times to call update_balance_allowance (default: 1)
    #[arg(short, long, default_value = "1")]
    iterations: u32,

    /// Delay between iterations in milliseconds (default: 500)
    #[arg(short, long, default_value = "500")]
    delay_ms: u64,

    /// Scan portfolio and list all tokens with balance
    #[arg(long)]
    list: bool,

    /// Run setApprovalForAll (on-chain) first, then run the update_balance_allowance test. Use this if allowance is 0 because approval was never set.
    #[arg(long)]
    approve: bool,

    /// Only run setApprovalForAll (on-chain) and exit. Does not test update_balance_allowance.
    #[arg(long)]
    approve_only: bool,

    /// Check all approvals (USDC + CTF) for all contracts (CTF Exchange, Neg Risk Exchange, Neg Risk Adapter). Similar to SDK's check_approvals example.
    #[arg(long)]
    check: bool,
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

    // Get condition IDs from config for portfolio scanning
    let btc_condition_id = config.trading.btc_condition_id.as_deref();
    let eth_condition_id = config.trading.eth_condition_id.as_deref();

    // --check: Check all approvals for all contracts (like SDK's check_approvals example)
    if args.check {
        println!("═══════════════════════════════════════════════════════════");
        println!("🔍 Checking all approvals for all contracts");
        println!("═══════════════════════════════════════════════════════════");

        // Determine and display wallet address
        if let Some(proxy_addr) = &config.polymarket.proxy_wallet_address {
            println!("   Wallet: {} (proxy wallet)", proxy_addr);
        } else if let Some(pk) = &config.polymarket.private_key {
            let signer = LocalSigner::from_str(pk)
                .context("Failed to create signer from private key")?
                .with_chain_id(Some(POLYGON));
            println!("   Wallet: {:#x} (EOA)", signer.address());
        } else {
            anyhow::bail!("Need either proxy_wallet_address or private_key to check approvals");
        }

        println!("   Chain: Polygon Mainnet (137)\n");

        match api.check_all_approvals().await {
            Ok(approvals) => {
                let mut all_approved = true;

                for (name, usdc_approved, ctf_approved) in &approvals {
                    let status = if *usdc_approved && *ctf_approved {
                        "✅ APPROVED"
                    } else {
                        all_approved = false;
                        "⚠️  MISSING"
                    };

                    println!("   Contract: {}", name);
                    println!(
                        "      USDC Allowance: {}",
                        if *usdc_approved {
                            "✅ Approved"
                        } else {
                            "❌ Not Approved"
                        }
                    );
                    println!(
                        "      CTF Approval:   {}",
                        if *ctf_approved {
                            "✅ Approved"
                        } else {
                            "❌ Not Approved"
                        }
                    );
                    println!("      Status: {}\n", status);
                }

                println!("═══════════════════════════════════════════════════════════");
                if all_approved {
                    println!("✅ All contracts properly approved - ready for trading!");
                } else {
                    println!("⚠️  Some approvals missing");
                    println!("   Run with --approve-only to set all approvals:");
                    println!("   cargo run --bin test_allowance -- --approve-only");
                }
                println!("═══════════════════════════════════════════════════════════\n");
            }
            Err(e) => {
                eprintln!("❌ Error checking approvals: {}", e);
                return Err(e);
            }
        }

        return Ok(());
    }

    // Authenticate (needed for other operations)
    println!("═══════════════════════════════════════════════════════════");
    println!("🔐 Authenticating with Polymarket CLOB API...");
    println!("═══════════════════════════════════════════════════════════");
    api.authenticate().await?;
    println!("✅ Authentication successful!\n");

    // If list flag is set, scan portfolio and list all tokens
    if args.list {
        println!("═══════════════════════════════════════════════════════════");
        println!("📊 Scanning portfolio for tokens with balance...");
        println!("═══════════════════════════════════════════════════════════");

        // Get portfolio tokens
        let tokens = api
            .get_portfolio_tokens_all(btc_condition_id, eth_condition_id)
            .await?;

        if tokens.is_empty() {
            println!("   ⚠️  No tokens found in portfolio");
            return Ok(());
        }

        println!("   Found {} token(s) in portfolio:\n", tokens.len());

        // Check on-chain approval status once (applies to all tokens)
        println!("🔍 Checking on-chain approval status (isApprovedForAll)...");
        match api.check_is_approved_for_all().await {
            Ok(true) => {
                println!("   ✅ setApprovalForAll is SET (Exchange is approved)");
            }
            Ok(false) => {
                println!("   ⚠️  setApprovalForAll is NOT SET (Exchange is not approved)");
                println!("   💡 This is why allowance is 0. Buying tokens doesn't set allowance.");
                println!("   💡 Run with --approve-only to set it: cargo run --bin test_allowance -- --approve-only");
            }
            Err(e) => {
                eprintln!("   ⚠️  Could not check approval status: {}", e);
            }
        }
        println!();

        for (token_id, balance, description, _condition_id) in tokens.iter() {
            println!("   Token ID: {}", token_id);
            println!("   Description: {}", description);
            println!("   Balance: {:.6} shares", balance);

            // Check allowance
            match api.check_balance_allowance(token_id).await {
                Ok((_balance, allowance)) => {
                    let allowance_decimal = allowance / Decimal::from(1_000_000u64);
                    let allowance_f64 = f64::try_from(allowance_decimal).unwrap_or(0.0);
                    println!("   Allowance: {:.6} shares", allowance_f64);
                    if allowance_f64 == 0.0 && balance > &0.0 {
                        println!("   ⚠️  Allowance is 0 even though you have balance (need setApprovalForAll)");
                    }
                }
                Err(e) => {
                    println!("   Allowance: Error checking - {}", e);
                }
            }
            println!();
        }

        return Ok(());
    }

    // --approve-only: run setApprovalForAll (on-chain) for all contracts and verify
    if args.approve_only {
        println!("═══════════════════════════════════════════════════════════");
        println!("🔐 Setting approvals for all contracts");
        println!("═══════════════════════════════════════════════════════════");
        println!("   This will approve:");
        println!("   1. CTF Exchange - Standard market trading");
        println!("   2. Neg Risk CTF Exchange - Neg-risk market trading");
        println!("   3. Neg Risk Adapter - Token minting/splitting (if available)");
        println!("\n   Each contract needs:");
        println!("   - ERC-20 approval for USDC (collateral token)");
        println!("   - ERC-1155 approval for Conditional Tokens (outcome tokens)\n");

        // Check current approvals first
        println!("📊 Checking current approvals...");
        match api.check_all_approvals().await {
            Ok(approvals) => {
                for (name, usdc_approved, ctf_approved) in &approvals {
                    println!(
                        "   {}: USDC={}, CTF={}",
                        name,
                        if *usdc_approved { "✅" } else { "❌" },
                        if *ctf_approved { "✅" } else { "❌" }
                    );
                }
                println!();
            }
            Err(e) => {
                eprintln!(
                    "   ⚠️  Could not check current approvals: {} (continuing anyway)\n",
                    e
                );
            }
        }

        // Set both USDC + CTF approvals for all contracts.
        println!("🔐 Setting USDC + CTF approvals for all contracts...");
        api.set_all_trading_approvals().await?;
        println!("✅ Approval batch done. Waiting 3s for chain to confirm...\n");
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Verify full approval matrix after setting.
        println!("🔍 Verifying approvals...");
        match api.check_all_approvals().await {
            Ok(approvals) => {
                let mut all_approved = true;
                for (name, usdc_approved, ctf_approved) in &approvals {
                    println!(
                        "   {}: USDC={}, CTF={}",
                        name,
                        if *usdc_approved {
                            "✅ Approved"
                        } else {
                            "❌ Not Approved"
                        },
                        if *ctf_approved {
                            "✅ Approved"
                        } else {
                            "❌ Not Approved"
                        }
                    );
                    if !usdc_approved || !ctf_approved {
                        all_approved = false;
                    }
                }
                println!();

                if all_approved {
                    println!("✅ All approvals verified!");
                } else {
                    println!("⚠️  Some approvals are still missing. Wait a few seconds and run --check again.");
                }
            }
            Err(e) => {
                eprintln!("   ⚠️  Could not verify approvals: {}", e);
            }
        }

        println!("\n✅ Approval process completed. You can now run the allowance test");
        println!("   without --approve, or use --approve to run both in one go.\n");
        return Ok(());
    }

    // Get token_id from args or scan portfolio
    let token_id = if let Some(tid) = args.token_id {
        tid
    } else {
        println!("═══════════════════════════════════════════════════════════");
        println!("📊 Scanning portfolio for tokens with balance...");
        println!("═══════════════════════════════════════════════════════════");

        let tokens = api
            .get_portfolio_tokens_all(btc_condition_id, eth_condition_id)
            .await?;

        if tokens.is_empty() {
            anyhow::bail!("No tokens found in portfolio. Please provide --token-id or ensure you have tokens in your portfolio.");
        }

        // Find first token with balance > 0
        if let Some((tid, balance, description, _)) = tokens.first() {
            println!("   Found token with balance: {}", tid);
            println!("   Description: {}", description);
            println!("   Balance: {:.6} shares\n", balance);
            tid.clone()
        } else {
            anyhow::bail!("No tokens with balance > 0 found in portfolio")
        }
    };

    println!("═══════════════════════════════════════════════════════════");
    println!("🧪 Testing update_balance_allowance_for_sell");
    println!("═══════════════════════════════════════════════════════════");
    println!("Token ID: {}\n", token_id);

    // Check if setApprovalForAll was already set (on-chain approval status)
    println!("🔍 Checking on-chain approval status (isApprovedForAll)...");
    match api.check_is_approved_for_all().await {
        Ok(true) => {
            println!("   ✅ setApprovalForAll is ALREADY SET (Exchange is approved)");
            println!(
                "   💡 If allowance is still 0, update_balance_allowance will refresh the cache.\n"
            );
        }
        Ok(false) => {
            println!("   ⚠️  setApprovalForAll is NOT SET (Exchange is not approved)");
            println!("   💡 This is why allowance is 0. Buying tokens doesn't set allowance.");
            println!(
                "   💡 Run with --approve to set it: cargo run --bin test_allowance -- --approve\n"
            );
        }
        Err(e) => {
            eprintln!(
                "   ⚠️  Could not check approval status: {} (continuing anyway)\n",
                e
            );
        }
    }

    // --approve: run setApprovalForAll first (on-chain). update_balance_allowance only refreshes the cache.
    if args.approve {
        println!("═══════════════════════════════════════════════════════════");
        println!("🔐 Running full USDC + CTF approvals first (on-chain)");
        println!("═══════════════════════════════════════════════════════════");
        api.set_all_trading_approvals().await?;
        println!("✅ Full approvals done. Waiting 2s for chain to confirm...\n");
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }

    // Check balance and allowance BEFORE update_balance_allowance
    println!("📊 BEFORE update_balance_allowance:");
    let (balance_before, allowance_before) = match api.check_balance_allowance(&token_id).await {
        Ok((bal, allow)) => {
            let balance_decimal = bal / Decimal::from(1_000_000u64);
            let allowance_decimal = allow / Decimal::from(1_000_000u64);
            let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
            let allowance_f64 = f64::try_from(allowance_decimal).unwrap_or(0.0);

            println!("   Balance: {:.6} shares", balance_f64);
            println!("   Allowance: {:.6} shares", allowance_f64);
            println!("   Allowance >= Balance: {}", allowance_f64 >= balance_f64);

            (balance_f64, allowance_f64)
        }
        Err(e) => {
            eprintln!("   ❌ Error checking balance/allowance: {}", e);
            return Err(e);
        }
    };

    println!(
        "\n🔄 Calling update_balance_allowance_for_sell ({} iteration(s))...\n",
        args.iterations
    );

    // Call update_balance_allowance multiple times if requested
    for i in 1..=args.iterations {
        println!("   Iteration {}/{}:", i, args.iterations);

        match api.update_balance_allowance_for_sell(&token_id).await {
            Ok(_) => {
                println!("      ✅ update_balance_allowance_for_sell succeeded");
            }
            Err(e) => {
                eprintln!("      ❌ update_balance_allowance_for_sell failed: {}", e);
                return Err(e);
            }
        }

        // Wait between iterations if not the last one
        if i < args.iterations {
            tokio::time::sleep(tokio::time::Duration::from_millis(args.delay_ms)).await;
        }
    }

    // Wait a bit for backend to process
    println!(
        "\n   ⏳ Waiting {}ms for backend to process...",
        args.delay_ms
    );
    tokio::time::sleep(tokio::time::Duration::from_millis(args.delay_ms)).await;

    // Check balance and allowance AFTER update_balance_allowance
    println!("\n📊 AFTER update_balance_allowance:");
    let (balance_after, allowance_after) = match api.check_balance_allowance(&token_id).await {
        Ok((bal, allow)) => {
            let balance_decimal = bal / Decimal::from(1_000_000u64);
            let allowance_decimal = allow / Decimal::from(1_000_000u64);
            let balance_f64 = f64::try_from(balance_decimal).unwrap_or(0.0);
            let allowance_f64 = f64::try_from(allowance_decimal).unwrap_or(0.0);

            println!("   Balance: {:.6} shares", balance_f64);
            println!("   Allowance: {:.6} shares", allowance_f64);
            println!("   Allowance >= Balance: {}", allowance_f64 >= balance_f64);

            (balance_f64, allowance_f64)
        }
        Err(e) => {
            eprintln!("   ❌ Error checking balance/allowance: {}", e);
            return Err(e);
        }
    };

    // Compare results
    println!("\n═══════════════════════════════════════════════════════════");
    println!("📈 COMPARISON:");
    println!("═══════════════════════════════════════════════════════════");

    let balance_changed = (balance_before - balance_after).abs() > 0.000001f64;
    let allowance_changed = (allowance_before - allowance_after).abs() > 0.000001f64;

    println!("   Balance:");
    println!("      Before: {:.6} shares", balance_before);
    println!("      After:  {:.6} shares", balance_after);
    println!(
        "      Changed: {}",
        if balance_changed { "✅ YES" } else { "❌ NO" }
    );

    println!("\n   Allowance:");
    println!("      Before: {:.6} shares", allowance_before);
    println!("      After:  {:.6} shares", allowance_after);
    println!(
        "      Changed: {}",
        if allowance_changed {
            "✅ YES"
        } else {
            "❌ NO"
        }
    );

    println!("\n   Allowance Status:");
    let before_sufficient = allowance_before >= balance_before && balance_before > 0.0;
    let after_sufficient = allowance_after >= balance_after && balance_after > 0.0;

    println!(
        "      Before: {}",
        if before_sufficient {
            "✅ SUFFICIENT"
        } else {
            "⚠️  INSUFFICIENT"
        }
    );
    println!(
        "      After:  {}",
        if after_sufficient {
            "✅ SUFFICIENT"
        } else {
            "⚠️  INSUFFICIENT"
        }
    );

    println!("\n═══════════════════════════════════════════════════════════");
    println!("💡 INTERPRETATION:");
    println!("═══════════════════════════════════════════════════════════");

    if allowance_changed {
        println!("   ✅ update_balance_allowance_for_sell WORKED!");
        println!(
            "      Allowance value changed from {:.6} to {:.6}",
            allowance_before, allowance_after
        );
    } else {
        println!("   ⚠️  Allowance value did NOT change");
        println!("      update_balance_allowance only REFRESHES the backend cache from chain.");
        println!("      It does NOT set on-chain approval. If allowance is 0, the chain has");
        println!("      no approval → cache stays 0 after refresh.");
    }

    if !before_sufficient && after_sufficient {
        println!("\n   ✅ SUCCESS: Allowance became sufficient after update!");
        println!("      This confirms update_balance_allowance_for_sell is working correctly.");
    } else if before_sufficient && after_sufficient {
        println!("\n   ✅ Allowance was already sufficient (no change needed)");
    } else if !after_sufficient {
        println!("\n   ⚠️  Allowance is still insufficient after update");
        println!();
        println!("   📌 ROOT CAUSE: setApprovalForAll() was never called on-chain.");
        println!("      update_balance_allowance only refreshes the cache; it cannot");
        println!("      create allowance. You must set on-chain approval first.");
        println!();
        println!("   ▶  Run with --approve to set approval, then re-run the test:");
        println!("      cargo run --bin test_allowance -- --approve");
        println!();
        println!("   ▶  Or run --approve-only once, then use the bot or test as usual:");
        println!("      cargo run --bin test_allowance -- --approve-only");
    }

    println!("\n═══════════════════════════════════════════════════════════\n");

    Ok(())
}
