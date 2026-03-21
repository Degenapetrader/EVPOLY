use anyhow::{Context, Result};
use clap::Parser;
use futures_util::stream::{self, StreamExt};
use polymarket_arbitrage_bot::models::OrderRequest;
use polymarket_arbitrage_bot::{Config, PolymarketApi};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "manual_inventory_exit")]
#[command(about = "Parallel SELL exit for all live wallet positions using top-ask LIMIT orders")]
struct Args {
    /// Execute real orders. If omitted, prints dry-run plan only.
    #[arg(long)]
    execute: bool,

    /// Config file path
    #[arg(short, long, default_value = "config.json")]
    config: String,

    /// Tracking DB path
    #[arg(long, default_value = "tracking.db")]
    db: String,

    /// Min shares per token side to include
    #[arg(long, default_value = "0.01")]
    min_shares: f64,

    /// Max parallel token exits in-flight
    #[arg(long, default_value_t = 32)]
    concurrency: usize,

    /// Enforce maker-only posting
    #[arg(long, default_value_t = false)]
    post_only: bool,

    /// Skip per-token open-order cancel before placing exit
    #[arg(long, default_value_t = false)]
    skip_cancel_open: bool,

    /// Optional cap for number of targets
    #[arg(long)]
    max_targets: Option<usize>,
}

#[derive(Debug, Clone)]
struct ExitTarget {
    token_id: String,
    condition_id: String,
    outcome: String,
    title: String,
    shares: f64,
    snapshot_ts_ms: i64,
}

#[derive(Debug, Clone)]
enum ExitState {
    DryRun,
    Placed,
    Skipped,
    Failed,
}

#[derive(Debug, Clone)]
struct ExitResultRow {
    target: ExitTarget,
    live_shares: f64,
    order_shares: f64,
    ask_price: f64,
    est_notional: f64,
    state: ExitState,
    detail: String,
}

fn floor_2(value: f64) -> f64 {
    (value * 100.0).floor() / 100.0
}

fn load_targets(db_path: &str, min_shares: f64) -> Result<Vec<ExitTarget>> {
    let conn =
        Connection::open(db_path).with_context(|| format!("failed to open db: {db_path}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT
                condition_id,
                token_id,
                COALESCE(outcome, ''),
                COALESCE(title, ''),
                COALESCE(position_size, 0.0),
                COALESCE(snapshot_ts_ms, 0)
             FROM wallet_positions_live_latest_v1
             WHERE COALESCE(position_size, 0.0) > ?
               AND token_id IS NOT NULL
               AND token_id != ''",
        )
        .context("failed to prepare wallet_positions_live_latest_v1 query")?;

    let mut by_token: BTreeMap<String, ExitTarget> = BTreeMap::new();
    let mut rows = stmt
        .query([min_shares])
        .context("failed to query wallet_positions_live_latest_v1")?;
    while let Some(row) = rows.next().context("failed to iterate rows")? {
        let condition_id: String = row.get(0).unwrap_or_default();
        let token_id: String = row.get(1).unwrap_or_default();
        let outcome: String = row.get(2).unwrap_or_default();
        let title: String = row.get(3).unwrap_or_default();
        let shares: f64 = row.get(4).unwrap_or(0.0);
        let snapshot_ts_ms: i64 = row.get(5).unwrap_or(0);
        if token_id.is_empty() || !shares.is_finite() || shares <= 0.0 {
            continue;
        }

        by_token
            .entry(token_id.clone())
            .and_modify(|existing| {
                existing.shares += shares;
                if existing.condition_id.is_empty() && !condition_id.is_empty() {
                    existing.condition_id = condition_id.clone();
                }
                if existing.outcome.is_empty() && !outcome.is_empty() {
                    existing.outcome = outcome.clone();
                }
                if existing.title.is_empty() && !title.is_empty() {
                    existing.title = title.clone();
                }
                if snapshot_ts_ms > existing.snapshot_ts_ms {
                    existing.snapshot_ts_ms = snapshot_ts_ms;
                }
            })
            .or_insert(ExitTarget {
                token_id,
                condition_id,
                outcome,
                title,
                shares,
                snapshot_ts_ms,
            });
    }

    let mut out = by_token.into_values().collect::<Vec<_>>();
    out.sort_by(|a, b| {
        b.shares
            .partial_cmp(&a.shares)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let args = Args::parse();
    let config_path = std::path::PathBuf::from(&args.config);
    let config = Config::load(&config_path)
        .with_context(|| format!("failed to load config: {}", args.config))?;

    let mut targets = load_targets(args.db.as_str(), args.min_shares)?;
    if let Some(max_targets) = args.max_targets {
        if max_targets > 0 && targets.len() > max_targets {
            targets.truncate(max_targets);
        }
    }
    if targets.is_empty() {
        println!(
            "No live wallet positions above min_shares={:.4} found in {}",
            args.min_shares, args.db
        );
        return Ok(());
    }

    let snapshot_max_ms = targets.iter().map(|t| t.snapshot_ts_ms).max().unwrap_or(0);
    let total_db_shares: f64 = targets.iter().map(|t| t.shares).sum();
    println!(
        "Loaded {} token targets from wallet_positions_live_latest_v1 | total_db_shares={:.4} | max_snapshot_ts_ms={}",
        targets.len(),
        total_db_shares,
        snapshot_max_ms
    );
    println!(
        "Mode: {} | order_type=GTC LIMIT @ top ask | concurrency={} | post_only={} | cancel_open={}",
        if args.execute { "EXECUTE" } else { "DRY_RUN" },
        args.concurrency.max(1),
        args.post_only,
        !args.skip_cancel_open
    );

    let api = Arc::new(PolymarketApi::new(
        config.polymarket.gamma_api_url.clone(),
        config.polymarket.clob_api_url.clone(),
        config.polymarket.private_key.clone(),
        config.polymarket.proxy_wallet_address.clone(),
        config.polymarket.signature_type,
    ));

    let mut can_read_live_balance = false;
    match api.authenticate().await {
        Ok(_) => {
            can_read_live_balance = true;
        }
        Err(e) => {
            if args.execute {
                return Err(e).context("CLOB authentication failed");
            }
            eprintln!(
                "⚠️  Authentication failed in dry-run mode; falling back to DB shares only: {}",
                e
            );
        }
    }

    let min_shares = args.min_shares.max(0.0);
    let post_only = args.post_only;
    let execute = args.execute;
    let cancel_open = !args.skip_cancel_open;
    let concurrency = args.concurrency.max(1).min(256);
    let can_read_live_balance_global = can_read_live_balance;

    let results = stream::iter(targets.into_iter().map(|target| {
        let api = api.clone();
        async move {
            let (live_shares, live_balance_note) = if can_read_live_balance_global {
                match api.check_balance_only(target.token_id.as_str()).await {
                    Ok(v) => (
                        f64::try_from(v / rust_decimal::Decimal::from(1_000_000u64))
                            .unwrap_or(0.0)
                            .max(0.0),
                        None,
                    ),
                    Err(e) => {
                        if execute {
                            return ExitResultRow {
                                target,
                                live_shares: 0.0,
                                order_shares: 0.0,
                                ask_price: 0.0,
                                est_notional: 0.0,
                                state: ExitState::Failed,
                                detail: format!("balance_check_failed: {e}"),
                            };
                        }
                        (
                            target.shares,
                            Some(format!("balance_check_failed_fallback_to_db: {e}")),
                        )
                    }
                }
            } else {
                (
                    target.shares,
                    Some("auth_unavailable_fallback_to_db".to_string()),
                )
            };

            let mut order_shares = floor_2(target.shares.min(live_shares));
            if !order_shares.is_finite() {
                order_shares = 0.0;
            }
            if order_shares < min_shares {
                return ExitResultRow {
                    target,
                    live_shares,
                    order_shares,
                    ask_price: 0.0,
                    est_notional: 0.0,
                    state: ExitState::Skipped,
                    detail: "shares_below_min_after_live_balance_check".to_string(),
                };
            }

            let best = match api.get_best_price(target.token_id.as_str()).await {
                Ok(v) => v,
                Err(e) => {
                    return ExitResultRow {
                        target,
                        live_shares,
                        order_shares,
                        ask_price: 0.0,
                        est_notional: 0.0,
                        state: ExitState::Failed,
                        detail: format!("best_price_failed: {e}"),
                    };
                }
            };
            let Some(best) = best else {
                return ExitResultRow {
                    target,
                    live_shares,
                    order_shares,
                    ask_price: 0.0,
                    est_notional: 0.0,
                    state: ExitState::Failed,
                    detail: "no_orderbook_price".to_string(),
                };
            };
            let Some(ask) = best.ask else {
                return ExitResultRow {
                    target,
                    live_shares,
                    order_shares,
                    ask_price: 0.0,
                    est_notional: 0.0,
                    state: ExitState::Failed,
                    detail: "no_top_ask".to_string(),
                };
            };
            let ask_str = ask.to_string();
            let ask_price = ask_str.parse::<f64>().unwrap_or(0.0);
            if !ask_price.is_finite() || ask_price <= 0.0 {
                return ExitResultRow {
                    target,
                    live_shares,
                    order_shares,
                    ask_price,
                    est_notional: 0.0,
                    state: ExitState::Failed,
                    detail: "invalid_top_ask".to_string(),
                };
            }
            let est_notional = ask_price * order_shares;

            if !execute {
                return ExitResultRow {
                    target,
                    live_shares,
                    order_shares,
                    ask_price,
                    est_notional,
                    state: ExitState::DryRun,
                    detail: live_balance_note.unwrap_or_else(|| "dry_run_only".to_string()),
                };
            }

            if cancel_open {
                let _ = api.cancel_market_orders(target.token_id.as_str()).await;
            }
            let _ = api
                .update_balance_allowance_for_sell(target.token_id.as_str())
                .await;

            let order = OrderRequest {
                token_id: target.token_id.clone(),
                side: "SELL".to_string(),
                size: format!("{order_shares:.2}"),
                price: ask_str,
                order_type: "GTC".to_string(),
                expiration_ts: None,
                post_only: Some(post_only),
            };

            match api.place_order(&order).await {
                Ok(resp) => ExitResultRow {
                    target,
                    live_shares,
                    order_shares,
                    ask_price,
                    est_notional,
                    state: ExitState::Placed,
                    detail: format!(
                        "order_id={} status={}",
                        resp.order_id.unwrap_or_else(|| "none".to_string()),
                        resp.status
                    ),
                },
                Err(e) => ExitResultRow {
                    target,
                    live_shares,
                    order_shares,
                    ask_price,
                    est_notional,
                    state: ExitState::Failed,
                    detail: format!("place_order_failed: {e}"),
                },
            }
        }
    }))
    .buffer_unordered(concurrency)
    .collect::<Vec<_>>()
    .await;

    let mut placed = 0usize;
    let mut dry = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut est_notional_total = 0.0_f64;
    for row in &results {
        match row.state {
            ExitState::Placed => {
                placed += 1;
                est_notional_total += row.est_notional;
            }
            ExitState::DryRun => {
                dry += 1;
                est_notional_total += row.est_notional;
            }
            ExitState::Skipped => skipped += 1,
            ExitState::Failed => failed += 1,
        }
        println!(
            "[{:>7}] token={} shares_db={:.4} shares_live={:.4} sell_shares={:.2} top_ask={:.6} est_notional={:.4} outcome={} title={} detail={}",
            match row.state {
                ExitState::DryRun => "DRYRUN",
                ExitState::Placed => "PLACED",
                ExitState::Skipped => "SKIPPED",
                ExitState::Failed => "FAILED",
            },
            row.target.token_id,
            row.target.shares,
            row.live_shares,
            row.order_shares,
            row.ask_price,
            row.est_notional,
            if row.target.outcome.is_empty() {
                "NA"
            } else {
                row.target.outcome.as_str()
            },
            if row.target.title.is_empty() {
                "NA"
            } else {
                row.target.title.as_str()
            },
            row.detail
        );
    }

    println!(
        "SUMMARY mode={} targets={} placed={} dry_run={} skipped={} failed={} est_notional_total={:.4}",
        if execute { "EXECUTE" } else { "DRY_RUN" },
        results.len(),
        placed,
        dry,
        skipped,
        failed,
        est_notional_total
    );

    if execute && failed > 0 {
        anyhow::bail!("one or more exits failed: failed={failed}");
    }

    Ok(())
}
