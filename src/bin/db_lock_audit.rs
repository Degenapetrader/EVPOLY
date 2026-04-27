use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};

#[derive(Default)]
struct ScopeStats {
    waits: Vec<i64>,
    holds: Vec<i64>,
}

fn percentile(values: &mut [i64], pct: f64) -> i64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let rank = ((values.len() - 1) as f64 * pct).round() as usize;
    values[rank.min(values.len() - 1)]
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "events.jsonl".to_string());
    let lookback_minutes = std::env::args()
        .nth(2)
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(60)
        .max(1);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let min_ts_ms = now_ms.saturating_sub(lookback_minutes.saturating_mul(60_000));

    let file = File::open(path.as_str())?;
    let reader = BufReader::new(file);
    let mut by_scope: BTreeMap<String, ScopeStats> = BTreeMap::new();
    let mut total_rows = 0_u64;
    let mut maintenance_events: BTreeMap<String, u64> = BTreeMap::new();
    let mut latest_maintenance_payload: Option<Value> = None;
    let mut queue_drops = 0_u64;

    for line in reader.lines() {
        let line = match line {
            Ok(v) => v,
            Err(_) => continue,
        };
        let row: Value = match serde_json::from_str(line.as_str()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind != "db_lock_profile" {
            if matches!(
                kind,
                "db_maintenance_started"
                    | "db_maintenance_completed"
                    | "db_maintenance_skipped"
                    | "db_maintenance_failed"
                    | "pending_orders_prune_skipped"
                    | "pending_orders_pruned"
                    | "db_feature_snapshot_queue_deferred"
                    | "db_feature_snapshot_queue_drop"
                    | "db_feature_snapshot_queue_write_failed"
            ) {
                let ts_ms = row.get("ts_ms").and_then(Value::as_i64).unwrap_or(0);
                if ts_ms >= min_ts_ms {
                    *maintenance_events.entry(kind.to_string()).or_insert(0) += 1;
                    if kind == "db_feature_snapshot_queue_drop" {
                        queue_drops = queue_drops.saturating_add(1);
                    }
                    if matches!(
                        kind,
                        "db_maintenance_completed"
                            | "db_maintenance_skipped"
                            | "db_maintenance_failed"
                    ) {
                        latest_maintenance_payload = row.get("payload").cloned();
                    }
                }
            }
            continue;
        }
        let ts_ms = row.get("ts_ms").and_then(Value::as_i64).unwrap_or(0);
        if ts_ms < min_ts_ms {
            continue;
        }
        let payload = match row.get("payload") {
            Some(v) => v,
            None => continue,
        };
        let scope = payload
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let wait_ms = payload.get("wait_ms").and_then(Value::as_i64).unwrap_or(0);
        let hold_ms = payload.get("hold_ms").and_then(Value::as_i64).unwrap_or(0);
        let entry = by_scope.entry(scope).or_default();
        entry.waits.push(wait_ms);
        entry.holds.push(hold_ms);
        total_rows = total_rows.saturating_add(1);
    }

    println!(
        "db_lock_audit lookback={}m rows={} scopes={} queue_drops={}",
        lookback_minutes,
        total_rows,
        by_scope.len(),
        queue_drops
    );
    if !maintenance_events.is_empty() {
        println!("maintenance_events:");
        for (kind, count) in maintenance_events {
            println!("{} | {}", kind, count);
        }
    }
    if let Some(payload) = latest_maintenance_payload {
        println!("latest_maintenance_payload={}", payload);
    }
    println!("scope | n | wait_p50 | wait_p95 | hold_p50 | hold_p95");
    for (scope, mut stats) in by_scope {
        let mut waits = std::mem::take(&mut stats.waits);
        let mut holds = std::mem::take(&mut stats.holds);
        let n = waits.len().max(holds.len());
        if n == 0 {
            continue;
        }
        let wait_p50 = percentile(&mut waits, 0.50);
        let wait_p95 = percentile(&mut waits, 0.95);
        let hold_p50 = percentile(&mut holds, 0.50);
        let hold_p95 = percentile(&mut holds, 0.95);
        println!(
            "{} | {} | {} | {} | {} | {}",
            scope, n, wait_p50, wait_p95, hold_p50, hold_p95
        );
    }
    Ok(())
}
