#!/usr/bin/env python3
"""Hourly wallet sync worker for EVPOLY.

What it does per run:
1) Fetch wallet positions from Polymarket Data API and persist snapshots.
2) Fetch wallet activity history and upsert into DB.
3) Mark open local positions with unrealized PnL using entry avg vs live mid price.
4) Trigger MM inventory reconcile + optional MM dust-sell maintenance via local admin API.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import sqlite3
import sys
import time
from dataclasses import dataclass
from typing import Any, Dict, Iterable, List, Optional, Tuple

import requests

DATA_API_BASE = os.getenv("EVPOLY_POLY_DATA_API_URL", "https://data-api.polymarket.com").rstrip("/")
CLOB_BASE = os.getenv("EVPOLY_POLY_CLOB_URL", "https://clob.polymarket.com").rstrip("/")
DEFAULT_INTERVAL_SEC = int(os.getenv("EVPOLY_WALLET_SYNC_INTERVAL_SEC", "3600"))
DEFAULT_ACTIVITY_LIMIT = int(os.getenv("EVPOLY_WALLET_SYNC_ACTIVITY_LIMIT", "1000"))
REQUEST_TIMEOUT_SEC = float(os.getenv("EVPOLY_WALLET_SYNC_TIMEOUT_SEC", "12"))
DB_BUSY_TIMEOUT_MS = int(os.getenv("EVPOLY_WALLET_SYNC_DB_BUSY_TIMEOUT_MS", "60000"))
DB_LOCK_RETRIES = int(os.getenv("EVPOLY_WALLET_SYNC_DB_LOCK_RETRIES", "20"))
DB_LOCK_RETRY_SEC = float(os.getenv("EVPOLY_WALLET_SYNC_DB_LOCK_RETRY_SEC", "3"))
ADMIN_API_BIND = os.getenv("EVPOLY_ADMIN_API_BIND", "127.0.0.1:8787").strip()
ADMIN_API_TOKEN = os.getenv("EVPOLY_ADMIN_API_TOKEN", "").strip()
MM_RECONCILE_MIN_DRIFT_SHARES = float(
    os.getenv("EVPOLY_MM_INVENTORY_RECONCILE_MIN_DRIFT_SHARES", "25")
)
MM_RECONCILE_LIMIT = int(os.getenv("EVPOLY_MM_INVENTORY_RECONCILE_LIMIT", "256"))
MM_RECONCILE_TIMEOUT_SEC = 30.0
MM_RECONCILE_STRATEGIES: Tuple[str, ...] = ("mm_sport_v1",)
MM_DUST_SELL_ENABLE = os.getenv("EVPOLY_MM_DUST_SELL_ENABLE", "1").strip().lower() not in {
    "0",
    "false",
    "no",
    "off",
}
MM_DUST_SELL_MAX_NOTIONAL_USD = float(os.getenv("EVPOLY_MM_DUST_SELL_MAX_NOTIONAL_USD", "5"))
MM_DUST_SELL_MAX_SHARES = float(os.getenv("EVPOLY_MM_DUST_SELL_MAX_SHARES", "25"))
MM_DUST_SELL_SCAN_LIMIT = int(os.getenv("EVPOLY_MM_DUST_SELL_SCAN_LIMIT", "2000"))
MM_DUST_SELL_MAX_ORDERS = int(os.getenv("EVPOLY_MM_DUST_SELL_MAX_ORDERS", "12"))
MM_DUST_SELL_TIMEOUT_SEC = float(os.getenv("EVPOLY_MM_DUST_SELL_TIMEOUT_SEC", "45"))


@dataclass
class RunStats:
    wallet: str
    started_ms: int
    positions_rows: int = 0
    activity_rows: int = 0
    activity_new_rows: int = 0
    open_positions_marked: int = 0
    book_queries: int = 0
    inventory_reconcile_status: str = "not_attempted"
    inventory_reconcile_rows: int = 0
    inventory_reconcile_repaired_shares: float = 0.0
    inventory_reconcile_error: Optional[str] = None
    dust_sell_status: str = "not_attempted"
    dust_sell_candidates: int = 0
    dust_sell_submitted: int = 0
    dust_sell_error: Optional[str] = None
    status: str = "ok"
    error: Optional[str] = None


def now_ms() -> int:
    return int(time.time() * 1000)


def load_wallet_from_env_or_args(wallet_arg: Optional[str]) -> str:
    if wallet_arg and wallet_arg.strip():
        return wallet_arg.strip().lower()
    for key in ("POLY_PROXY_WALLET_ADDRESS", "EVPOLY_WALLET_SYNC_ADDRESS"):
        value = os.getenv(key, "").strip()
        if value:
            return value.lower()
    raise RuntimeError("wallet address missing. set POLY_PROXY_WALLET_ADDRESS or pass --wallet")


def new_session() -> requests.Session:
    s = requests.Session()
    s.headers.update({"User-Agent": "evpoly-wallet-sync/1.0"})
    return s


def fetch_json(session: requests.Session, url: str, params: Optional[Dict[str, Any]] = None) -> Any:
    resp = session.get(url, params=params, timeout=REQUEST_TIMEOUT_SEC)
    resp.raise_for_status()
    return resp.json()


def fetch_wallet_positions(session: requests.Session, wallet: str) -> List[Dict[str, Any]]:
    payload = fetch_json(session, f"{DATA_API_BASE}/positions", {"user": wallet})
    if isinstance(payload, list):
        return payload
    return []


def fetch_wallet_activity(session: requests.Session, wallet: str, limit: int) -> List[Dict[str, Any]]:
    payload = fetch_json(session, f"{DATA_API_BASE}/activity", {"user": wallet, "limit": max(1, limit)})
    if isinstance(payload, list):
        return payload
    return []


def parse_best_levels(book: Any) -> Tuple[Optional[float], Optional[float]]:
    if not isinstance(book, dict):
        return None, None

    def best_bid(vals: Any) -> Optional[float]:
        if not isinstance(vals, list):
            return None
        out: Optional[float] = None
        for x in vals:
            try:
                p = float(x.get("price") if isinstance(x, dict) else x[0])
                if p <= 0:
                    continue
                if out is None or p > out:
                    out = p
            except Exception:
                continue
        return out

    def best_ask(vals: Any) -> Optional[float]:
        if not isinstance(vals, list):
            return None
        out: Optional[float] = None
        for x in vals:
            try:
                p = float(x.get("price") if isinstance(x, dict) else x[0])
                if p <= 0:
                    continue
                if out is None or p < out:
                    out = p
            except Exception:
                continue
        return out

    return best_bid(book.get("bids")), best_ask(book.get("asks"))


def fetch_book_mid(session: requests.Session, token_id: str) -> Tuple[Optional[float], Optional[float], Optional[float], str]:
    url = f"{CLOB_BASE}/book"
    try:
        resp = session.get(url, params={"token_id": token_id}, timeout=REQUEST_TIMEOUT_SEC)
        if resp.status_code != 200:
            return None, None, None, f"status_{resp.status_code}"
        best_bid, best_ask = parse_best_levels(resp.json())
        if best_bid is not None and best_ask is not None:
            return best_bid, best_ask, (best_bid + best_ask) / 2.0, "midpoint"
        if best_bid is not None:
            return best_bid, best_ask, best_bid, "bid_only"
        if best_ask is not None:
            return best_bid, best_ask, best_ask, "ask_only"
        return best_bid, best_ask, None, "empty"
    except Exception as e:
        return None, None, None, f"error:{type(e).__name__}"


def ensure_column(conn: sqlite3.Connection, table: str, column: str, definition: str) -> None:
    exists = conn.execute(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
        (table,),
    ).fetchone()
    if not exists or int(exists[0] or 0) <= 0:
        return
    existing = {
        str(row[1]).strip().lower()
        for row in conn.execute(f"PRAGMA table_info({table})")
    }
    if column.strip().lower() in existing:
        return
    conn.execute(f"ALTER TABLE {table} ADD COLUMN {column} {definition}")


def ensure_tables(conn: sqlite3.Connection) -> None:
    cur = conn.cursor()
    cur.executescript(
        """
CREATE TABLE IF NOT EXISTS wallet_positions_live_v1 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_ts_ms INTEGER NOT NULL,
    wallet_address TEXT NOT NULL,
    condition_id TEXT,
    token_id TEXT,
    outcome TEXT,
    opposite_outcome TEXT,
    slug TEXT,
    event_slug TEXT,
    title TEXT,
    token_type TEXT,
    position_size REAL,
    avg_price REAL,
    cur_price REAL,
    initial_value REAL,
    current_value REAL,
    cash_pnl REAL,
    realized_pnl REAL,
    redeemable INTEGER,
    mergeable INTEGER,
    negative_risk INTEGER,
    end_date TEXT,
    raw_json TEXT
);
CREATE INDEX IF NOT EXISTS idx_wallet_positions_live_snapshot
    ON wallet_positions_live_v1(snapshot_ts_ms DESC, wallet_address);
CREATE INDEX IF NOT EXISTS idx_wallet_positions_live_token
    ON wallet_positions_live_v1(wallet_address, condition_id, token_id, snapshot_ts_ms DESC);

CREATE TABLE IF NOT EXISTS wallet_positions_live_latest_v1 (
    wallet_address TEXT NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    snapshot_ts_ms INTEGER NOT NULL,
    outcome TEXT,
    opposite_outcome TEXT,
    slug TEXT,
    event_slug TEXT,
    title TEXT,
    token_type TEXT,
    position_size REAL,
    avg_price REAL,
    cur_price REAL,
    initial_value REAL,
    current_value REAL,
    cash_pnl REAL,
    realized_pnl REAL,
    redeemable INTEGER,
    mergeable INTEGER,
    negative_risk INTEGER,
    end_date TEXT,
    raw_json TEXT,
    PRIMARY KEY (wallet_address, condition_id, token_id)
);

CREATE TABLE IF NOT EXISTS wallet_activity_v1 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    wallet_address TEXT NOT NULL,
    activity_key TEXT NOT NULL UNIQUE,
    ts_ms INTEGER NOT NULL,
    activity_type TEXT,
    condition_id TEXT,
    token_id TEXT,
    outcome TEXT,
    side TEXT,
    size REAL,
    usdc_size REAL,
    inventory_consumed_units REAL,
    price REAL,
    transaction_hash TEXT,
    slug TEXT,
    title TEXT,
    raw_json TEXT,
    synced_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_wallet_activity_wallet_ts
    ON wallet_activity_v1(wallet_address, ts_ms DESC);
CREATE INDEX IF NOT EXISTS idx_wallet_activity_tx
    ON wallet_activity_v1(transaction_hash);

CREATE TABLE IF NOT EXISTS positions_unrealized_mid_v1 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    snapshot_ts_ms INTEGER NOT NULL,
    position_key TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    token_type TEXT,
    remaining_units REAL NOT NULL,
    remaining_cost_usd REAL NOT NULL,
    entry_avg_price REAL,
    best_bid REAL,
    best_ask REAL,
    mid_price REAL,
    unrealized_mid_pnl_usd REAL,
    mark_source TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_unrealized_mid_snapshot_position
    ON positions_unrealized_mid_v1(snapshot_ts_ms, position_key);

CREATE TABLE IF NOT EXISTS positions_unrealized_mid_latest_v1 (
    position_key TEXT PRIMARY KEY,
    snapshot_ts_ms INTEGER NOT NULL,
    strategy_id TEXT NOT NULL,
    timeframe TEXT NOT NULL,
    period_timestamp INTEGER NOT NULL,
    condition_id TEXT NOT NULL,
    token_id TEXT NOT NULL,
    token_type TEXT,
    remaining_units REAL NOT NULL,
    remaining_cost_usd REAL NOT NULL,
    entry_avg_price REAL,
    best_bid REAL,
    best_ask REAL,
    mid_price REAL,
    unrealized_mid_pnl_usd REAL,
    mark_source TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS wallet_sync_runs_v1 (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms INTEGER NOT NULL,
    wallet_address TEXT NOT NULL,
    positions_rows INTEGER NOT NULL,
    activity_rows INTEGER NOT NULL,
    activity_new_rows INTEGER NOT NULL,
    open_positions_marked INTEGER NOT NULL,
    book_queries INTEGER NOT NULL,
    inventory_reconcile_status TEXT NOT NULL DEFAULT 'not_attempted',
    inventory_reconcile_rows INTEGER NOT NULL DEFAULT 0,
    inventory_reconcile_repaired_shares REAL NOT NULL DEFAULT 0.0,
    inventory_reconcile_error TEXT,
    dust_sell_status TEXT NOT NULL DEFAULT 'not_attempted',
    dust_sell_candidates INTEGER NOT NULL DEFAULT 0,
    dust_sell_submitted INTEGER NOT NULL DEFAULT 0,
    dust_sell_error TEXT,
    duration_ms INTEGER NOT NULL,
    status TEXT NOT NULL,
    error TEXT
);
CREATE INDEX IF NOT EXISTS idx_wallet_sync_runs_ts ON wallet_sync_runs_v1(ts_ms DESC);
"""
    )
    ensure_column(conn, "wallet_activity_v1", "inventory_consumed_units", "REAL")
    ensure_column(conn, "positions_v2", "inventory_consumed_units", "REAL NOT NULL DEFAULT 0.0")
    ensure_column(conn, "positions_v2", "inventory_consumed_cost_usd", "REAL NOT NULL DEFAULT 0.0")
    ensure_column(
        conn,
        "wallet_sync_runs_v1",
        "inventory_reconcile_status",
        "TEXT NOT NULL DEFAULT 'not_attempted'",
    )
    ensure_column(
        conn,
        "wallet_sync_runs_v1",
        "inventory_reconcile_rows",
        "INTEGER NOT NULL DEFAULT 0",
    )
    ensure_column(
        conn,
        "wallet_sync_runs_v1",
        "inventory_reconcile_repaired_shares",
        "REAL NOT NULL DEFAULT 0.0",
    )
    ensure_column(conn, "wallet_sync_runs_v1", "inventory_reconcile_error", "TEXT")
    ensure_column(
        conn,
        "wallet_sync_runs_v1",
        "dust_sell_status",
        "TEXT NOT NULL DEFAULT 'not_attempted'",
    )
    ensure_column(
        conn,
        "wallet_sync_runs_v1",
        "dust_sell_candidates",
        "INTEGER NOT NULL DEFAULT 0",
    )
    ensure_column(
        conn,
        "wallet_sync_runs_v1",
        "dust_sell_submitted",
        "INTEGER NOT NULL DEFAULT 0",
    )
    ensure_column(conn, "wallet_sync_runs_v1", "dust_sell_error", "TEXT")
    conn.commit()


def to_i01(v: Any) -> int:
    return 1 if bool(v) else 0


def persist_wallet_positions(
    conn: sqlite3.Connection,
    wallet: str,
    snapshot_ts_ms: int,
    rows: List[Dict[str, Any]],
) -> int:
    cur = conn.cursor()

    # Treat the "latest" table as an exact snapshot of the live wallet.
    # If a token disappears from the live positions payload, it must be removed
    # here instead of lingering as a stale row.
    cur.execute(
        "DELETE FROM wallet_positions_live_latest_v1 WHERE wallet_address=?",
        (wallet,),
    )

    inserted = 0
    for row in rows:
        condition_id = str(row.get("conditionId") or "").strip()
        token_id = str(row.get("asset") or "").strip()
        if not condition_id or not token_id:
            continue

        values = (
            snapshot_ts_ms,
            wallet,
            condition_id,
            token_id,
            row.get("outcome"),
            row.get("oppositeOutcome"),
            row.get("slug"),
            row.get("eventSlug"),
            row.get("title"),
            row.get("tokenType"),
            float(row.get("size") or 0.0),
            float(row.get("avgPrice") or 0.0),
            float(row.get("curPrice") or 0.0),
            float(row.get("initialValue") or 0.0),
            float(row.get("currentValue") or 0.0),
            float(row.get("cashPnl") or 0.0),
            float(row.get("realizedPnl") or 0.0),
            to_i01(row.get("redeemable")),
            to_i01(row.get("mergeable")),
            to_i01(row.get("negativeRisk")),
            row.get("endDate"),
            json.dumps(row, separators=(",", ":"), ensure_ascii=False),
        )

        cur.execute(
            """
INSERT INTO wallet_positions_live_v1 (
    snapshot_ts_ms, wallet_address, condition_id, token_id, outcome, opposite_outcome,
    slug, event_slug, title, token_type, position_size, avg_price, cur_price,
    initial_value, current_value, cash_pnl, realized_pnl, redeemable, mergeable,
    negative_risk, end_date, raw_json
) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
""",
            values,
        )

        cur.execute(
            """
INSERT INTO wallet_positions_live_latest_v1 (
    wallet_address, condition_id, token_id, snapshot_ts_ms, outcome, opposite_outcome,
    slug, event_slug, title, token_type, position_size, avg_price, cur_price,
    initial_value, current_value, cash_pnl, realized_pnl, redeemable, mergeable,
    negative_risk, end_date, raw_json
) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
ON CONFLICT(wallet_address, condition_id, token_id) DO UPDATE SET
    snapshot_ts_ms=excluded.snapshot_ts_ms,
    outcome=excluded.outcome,
    opposite_outcome=excluded.opposite_outcome,
    slug=excluded.slug,
    event_slug=excluded.event_slug,
    title=excluded.title,
    token_type=excluded.token_type,
    position_size=excluded.position_size,
    avg_price=excluded.avg_price,
    cur_price=excluded.cur_price,
    initial_value=excluded.initial_value,
    current_value=excluded.current_value,
    cash_pnl=excluded.cash_pnl,
    realized_pnl=excluded.realized_pnl,
    redeemable=excluded.redeemable,
    mergeable=excluded.mergeable,
    negative_risk=excluded.negative_risk,
    end_date=excluded.end_date,
    raw_json=excluded.raw_json
""",
            (
                wallet,
                condition_id,
                token_id,
                snapshot_ts_ms,
                row.get("outcome"),
                row.get("oppositeOutcome"),
                row.get("slug"),
                row.get("eventSlug"),
                row.get("title"),
                row.get("tokenType"),
                float(row.get("size") or 0.0),
                float(row.get("avgPrice") or 0.0),
                float(row.get("curPrice") or 0.0),
                float(row.get("initialValue") or 0.0),
                float(row.get("currentValue") or 0.0),
                float(row.get("cashPnl") or 0.0),
                float(row.get("realizedPnl") or 0.0),
                to_i01(row.get("redeemable")),
                to_i01(row.get("mergeable")),
                to_i01(row.get("negativeRisk")),
                row.get("endDate"),
                json.dumps(row, separators=(",", ":"), ensure_ascii=False),
            ),
        )
        inserted += 1

    conn.commit()
    return inserted


def activity_key(wallet: str, row: Dict[str, Any]) -> str:
    payload = "|".join(
        [
            wallet,
            str(row.get("timestamp") or ""),
            str(row.get("type") or ""),
            str(row.get("transactionHash") or ""),
            str(row.get("asset") or ""),
            str(row.get("size") or ""),
            str(row.get("usdcSize") or ""),
        ]
    )
    return hashlib.sha1(payload.encode("utf-8")).hexdigest()


def infer_inventory_consumed_units(row: Dict[str, Any]) -> float:
    for key in ("inventoryConsumed", "inventoryConsumedUnits", "inventoryConsumedShares"):
        try:
            value = float(row.get(key) or 0.0)
            if value > 0.0:
                return value
        except Exception:
            continue
    activity_type = str(row.get("type") or "").strip().upper()
    if activity_type == "MERGE":
        try:
            return max(float(row.get("size") or 0.0), 0.0)
        except Exception:
            return 0.0
    return 0.0


def persist_wallet_activity(
    conn: sqlite3.Connection,
    wallet: str,
    snapshot_ts_ms: int,
    rows: List[Dict[str, Any]],
) -> Tuple[int, int]:
    cur = conn.cursor()
    seen = 0
    inserted = 0
    for row in rows:
        key = activity_key(wallet, row)
        ts_ms = int(float(row.get("timestamp") or 0) * 1000)
        usdc_size = float(row.get("usdcSize") or 0.0)
        size = float(row.get("size") or 0.0)
        inventory_consumed_units = infer_inventory_consumed_units(row)
        price = (usdc_size / size) if size > 0 else None
        cur.execute(
            """
INSERT OR IGNORE INTO wallet_activity_v1 (
    wallet_address, activity_key, ts_ms, activity_type, condition_id, token_id,
    outcome, side, size, usdc_size, inventory_consumed_units, price,
    transaction_hash, slug, title, raw_json, synced_at_ms
) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
""",
            (
                wallet,
                key,
                ts_ms,
                row.get("type"),
                row.get("conditionId"),
                row.get("asset"),
                row.get("outcome"),
                row.get("side"),
                size,
                usdc_size,
                inventory_consumed_units,
                price,
                row.get("transactionHash"),
                row.get("slug"),
                row.get("title"),
                json.dumps(row, separators=(",", ":"), ensure_ascii=False),
                snapshot_ts_ms,
            ),
        )
        seen += 1
        if cur.rowcount > 0:
            inserted += 1

    conn.commit()
    return seen, inserted


def iter_open_positions(conn: sqlite3.Connection) -> Iterable[sqlite3.Row]:
    exists = conn.execute(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='positions_v2'"
    ).fetchone()
    if not exists or int(exists[0] or 0) <= 0:
        return
    q = """
SELECT
    position_key, strategy_id, timeframe, period_timestamp,
    condition_id, token_id, token_type,
    COALESCE(entry_units,0.0) AS entry_units,
    COALESCE(exit_units,0.0) AS exit_units,
    COALESCE(inventory_consumed_units,0.0) AS inventory_consumed_units,
    COALESCE(entry_notional_usd,0.0) AS entry_notional_usd,
    COALESCE(entry_fee_usd,0.0) AS entry_fee_usd
FROM positions_v2
WHERE status='OPEN' AND strategy_id!='legacy_default'
  AND condition_id IS NOT NULL AND token_id IS NOT NULL
"""
    for row in conn.execute(q):
        yield row


def persist_unrealized_mid_marks(
    conn: sqlite3.Connection,
    session: requests.Session,
    snapshot_ts_ms: int,
) -> Tuple[int, int]:
    book_cache: Dict[str, Tuple[Optional[float], Optional[float], Optional[float], str]] = {}
    marked = 0
    book_queries = 0
    rows_history: List[Tuple[Any, ...]] = []
    rows_latest: List[Tuple[Any, ...]] = []

    # Compute mark rows first (includes remote book lookups), then write in batch.
    # This keeps DB write-lock windows short and reduces SQLITE_BUSY races.
    for row in iter_open_positions(conn):
        entry_units = float(row["entry_units"] or 0.0)
        exit_units = float(row["exit_units"] or 0.0)
        inventory_consumed_units = float(row["inventory_consumed_units"] or 0.0)
        remaining_units = max(entry_units - exit_units - inventory_consumed_units, 0.0)
        if remaining_units <= 1e-12:
            continue

        total_entry_cost = max(float(row["entry_notional_usd"] or 0.0) + float(row["entry_fee_usd"] or 0.0), 0.0)
        remaining_cost = total_entry_cost * (remaining_units / entry_units) if entry_units > 1e-12 else 0.0
        entry_avg = (remaining_cost / remaining_units) if remaining_units > 1e-12 else None

        token_id = str(row["token_id"])
        mark = book_cache.get(token_id)
        if mark is None:
            mark = fetch_book_mid(session, token_id)
            book_cache[token_id] = mark
            book_queries += 1

        best_bid, best_ask, mid_price, mark_source = mark
        unrealized = None
        if mid_price is not None and entry_avg is not None:
            unrealized = (mid_price - entry_avg) * remaining_units

        values_history = (
            snapshot_ts_ms,
            str(row["position_key"]),
            str(row["strategy_id"]),
            str(row["timeframe"]),
            int(row["period_timestamp"] or 0),
            str(row["condition_id"]),
            token_id,
            row["token_type"],
            remaining_units,
            remaining_cost,
            entry_avg,
            best_bid,
            best_ask,
            mid_price,
            unrealized,
            mark_source,
        )
        values_latest = (
            str(row["position_key"]),
            snapshot_ts_ms,
            str(row["strategy_id"]),
            str(row["timeframe"]),
            int(row["period_timestamp"] or 0),
            str(row["condition_id"]),
            token_id,
            row["token_type"],
            remaining_units,
            remaining_cost,
            entry_avg,
            best_bid,
            best_ask,
            mid_price,
            unrealized,
            mark_source,
        )

        rows_history.append(values_history)
        rows_latest.append(values_latest)
        marked += 1

    if marked <= 0:
        return 0, book_queries

    cur = conn.cursor()
    cur.executemany(
        """
INSERT OR REPLACE INTO positions_unrealized_mid_v1 (
    snapshot_ts_ms, position_key, strategy_id, timeframe, period_timestamp,
    condition_id, token_id, token_type, remaining_units, remaining_cost_usd,
    entry_avg_price, best_bid, best_ask, mid_price, unrealized_mid_pnl_usd,
    mark_source
) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
""",
        rows_history,
    )

    cur.executemany(
        """
INSERT INTO positions_unrealized_mid_latest_v1 (
    position_key, snapshot_ts_ms, strategy_id, timeframe, period_timestamp,
    condition_id, token_id, token_type, remaining_units, remaining_cost_usd,
    entry_avg_price, best_bid, best_ask, mid_price, unrealized_mid_pnl_usd,
    mark_source
) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
ON CONFLICT(position_key) DO UPDATE SET
    snapshot_ts_ms=excluded.snapshot_ts_ms,
    strategy_id=excluded.strategy_id,
    timeframe=excluded.timeframe,
    period_timestamp=excluded.period_timestamp,
    condition_id=excluded.condition_id,
    token_id=excluded.token_id,
    token_type=excluded.token_type,
    remaining_units=excluded.remaining_units,
    remaining_cost_usd=excluded.remaining_cost_usd,
    entry_avg_price=excluded.entry_avg_price,
    best_bid=excluded.best_bid,
    best_ask=excluded.best_ask,
    mid_price=excluded.mid_price,
    unrealized_mid_pnl_usd=excluded.unrealized_mid_pnl_usd,
    mark_source=excluded.mark_source
""",
        rows_latest,
    )

    conn.commit()
    return marked, book_queries


def record_run(conn: sqlite3.Connection, stats: RunStats) -> None:
    duration_ms = now_ms() - stats.started_ms
    conn.execute(
        """
INSERT INTO wallet_sync_runs_v1 (
    ts_ms, wallet_address, positions_rows, activity_rows, activity_new_rows,
    open_positions_marked, book_queries, inventory_reconcile_status,
    inventory_reconcile_rows, inventory_reconcile_repaired_shares,
    inventory_reconcile_error, dust_sell_status, dust_sell_candidates,
    dust_sell_submitted, dust_sell_error, duration_ms, status, error
) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
""",
        (
            now_ms(),
            stats.wallet,
            stats.positions_rows,
            stats.activity_rows,
            stats.activity_new_rows,
            stats.open_positions_marked,
            stats.book_queries,
            stats.inventory_reconcile_status,
            stats.inventory_reconcile_rows,
            stats.inventory_reconcile_repaired_shares,
            stats.inventory_reconcile_error,
            stats.dust_sell_status,
            stats.dust_sell_candidates,
            stats.dust_sell_submitted,
            stats.dust_sell_error,
            duration_ms,
            stats.status,
            stats.error,
        ),
    )
    conn.commit()


def normalize_min_drift_shares(value: float) -> float:
    return value if math.isfinite(value) and value > 0.0 else 25.0


def normalize_limit(value: int) -> int:
    return max(1, min(int(value), 10_000))


def try_reconcile_mm_inventory_for_strategy(
    session: requests.Session,
    strategy_id: str,
    min_drift_shares: float,
    limit: int,
) -> Tuple[str, int, float, Optional[str]]:
    normalized_strategy_id = (strategy_id or "").strip().lower()
    if not normalized_strategy_id:
        return "error", 0, 0.0, "strategy_id_missing"
    url = (
        f"http://{ADMIN_API_BIND}/admin/mm/reconcile/inventory"
        f"?strategy_id={normalized_strategy_id}&min_drift_shares={min_drift_shares}&limit={limit}"
    )
    headers: Dict[str, str] = {}
    if ADMIN_API_TOKEN:
        headers["x-evpoly-admin-token"] = ADMIN_API_TOKEN
    try:
        resp = session.post(url, headers=headers, timeout=MM_RECONCILE_TIMEOUT_SEC)
        resp.raise_for_status()
        payload = resp.json()
        rows = payload.get("rows") if isinstance(payload, dict) else None
        count = int(payload.get("count") or 0) if isinstance(payload, dict) else 0
        repaired = 0.0
        if isinstance(rows, list):
            for row in rows:
                if not isinstance(row, dict):
                    continue
                repaired += float(row.get("applied_consume_shares") or 0.0)
                repaired += float(row.get("applied_add_shares") or 0.0)
        return "ok", count, repaired, None
    except Exception as e:
        return "error", 0, 0.0, f"{type(e).__name__}: {e}"


def try_reconcile_mm_inventory_from_wallet(session: requests.Session) -> Tuple[str, int, float, Optional[str]]:
    min_drift_shares = normalize_min_drift_shares(MM_RECONCILE_MIN_DRIFT_SHARES)
    limit = normalize_limit(MM_RECONCILE_LIMIT)
    total_rows = 0
    total_repaired = 0.0
    errors: List[str] = []
    for strategy_id in MM_RECONCILE_STRATEGIES:
        status, rows, repaired, error = try_reconcile_mm_inventory_for_strategy(
            session, strategy_id, min_drift_shares, limit
        )
        total_rows += rows
        total_repaired += repaired
        if status != "ok":
            errors.append(f"{strategy_id}: {error or status}")
    if not errors:
        return "ok", total_rows, total_repaired, None
    overall_status = "error" if len(errors) == len(MM_RECONCILE_STRATEGIES) else "partial"
    return overall_status, total_rows, total_repaired, " | ".join(errors)


def normalize_dust_notional(value: float) -> float:
    return max(0.10, min(float(value), 1_000.0)) if math.isfinite(value) else 5.0


def normalize_dust_shares(value: float) -> float:
    return max(0.01, min(float(value), 10_000.0)) if math.isfinite(value) else 25.0


def normalize_dust_orders(value: int) -> int:
    return max(1, min(int(value), 1_000))


def try_dust_sell_mm_inventory(session: requests.Session) -> Tuple[str, int, int, Optional[str]]:
    if not MM_DUST_SELL_ENABLE:
        return "disabled", 0, 0, None
    max_notional_usd = normalize_dust_notional(MM_DUST_SELL_MAX_NOTIONAL_USD)
    max_shares = normalize_dust_shares(MM_DUST_SELL_MAX_SHARES)
    scan_limit = normalize_limit(MM_DUST_SELL_SCAN_LIMIT)
    max_orders = normalize_dust_orders(MM_DUST_SELL_MAX_ORDERS)
    url = (
        f"http://{ADMIN_API_BIND}/admin/mm/dust-sell"
        f"?max_notional_usd={max_notional_usd}&max_shares={max_shares}"
        f"&scan_limit={scan_limit}&max_orders={max_orders}"
    )
    headers: Dict[str, str] = {}
    if ADMIN_API_TOKEN:
        headers["x-evpoly-admin-token"] = ADMIN_API_TOKEN
    try:
        resp = session.post(url, headers=headers, timeout=MM_DUST_SELL_TIMEOUT_SEC)
        resp.raise_for_status()
        payload = resp.json()
        if isinstance(payload, dict) and payload.get("enabled") is False:
            return "disabled", 0, 0, None
        candidates = int(payload.get("candidates") or 0) if isinstance(payload, dict) else 0
        submitted = int(payload.get("submitted") or 0) if isinstance(payload, dict) else 0
        return "ok", candidates, submitted, None
    except Exception as e:
        return "error", 0, 0, f"{type(e).__name__}: {e}"


def run_once(db_path: str, wallet: str, activity_limit: int) -> RunStats:
    started = now_ms()
    stats = RunStats(wallet=wallet, started_ms=started)

    conn = sqlite3.connect(db_path, timeout=max(5.0, DB_BUSY_TIMEOUT_MS / 1000.0))
    conn.row_factory = sqlite3.Row
    try:
        conn.execute(f"PRAGMA busy_timeout={max(1000, DB_BUSY_TIMEOUT_MS)}")
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA synchronous=NORMAL")
        ensure_tables(conn)
        session = new_session()
        snapshot_ts = now_ms()

        positions = fetch_wallet_positions(session, wallet)
        stats.positions_rows = persist_wallet_positions(conn, wallet, snapshot_ts, positions)

        activity = fetch_wallet_activity(session, wallet, activity_limit)
        seen, inserted = persist_wallet_activity(conn, wallet, snapshot_ts, activity)
        stats.activity_rows = seen
        stats.activity_new_rows = inserted

        marked, book_queries = persist_unrealized_mid_marks(conn, session, snapshot_ts)
        stats.open_positions_marked = marked
        stats.book_queries = book_queries

        reconcile_status, reconcile_rows, repaired_shares, reconcile_error = (
            try_reconcile_mm_inventory_from_wallet(session)
        )
        stats.inventory_reconcile_status = reconcile_status
        stats.inventory_reconcile_rows = reconcile_rows
        stats.inventory_reconcile_repaired_shares = repaired_shares
        stats.inventory_reconcile_error = reconcile_error
        if reconcile_status != "ok":
            stats.status = "partial"

        dust_status, dust_candidates, dust_submitted, dust_error = (
            try_dust_sell_mm_inventory(session)
        )
        stats.dust_sell_status = dust_status
        stats.dust_sell_candidates = dust_candidates
        stats.dust_sell_submitted = dust_submitted
        stats.dust_sell_error = dust_error
        if dust_status not in {"ok", "disabled"}:
            stats.status = "partial"

        record_run(conn, stats)
        return stats
    except Exception as e:
        stats.status = "error"
        stats.error = f"{type(e).__name__}: {e}"
        try:
            record_run(conn, stats)
        except Exception:
            pass
        raise
    finally:
        conn.close()


def run_once_with_retry(db_path: str, wallet: str, activity_limit: int) -> RunStats:
    attempts = max(1, DB_LOCK_RETRIES)
    for attempt in range(1, attempts + 1):
        try:
            return run_once(db_path, wallet, activity_limit)
        except sqlite3.OperationalError as e:
            msg = str(e).lower()
            if "database is locked" in msg or "database is busy" in msg:
                if attempt >= attempts:
                    raise
                time.sleep(max(0.5, DB_LOCK_RETRY_SEC))
                continue
            raise
    raise RuntimeError("unexpected wallet sync retry state")


def main() -> int:
    parser = argparse.ArgumentParser(description="Sync wallet history + unrealized mid marks into tracking.db")
    parser.add_argument("--db", default="tracking.db", help="SQLite DB path (default: tracking.db)")
    parser.add_argument("--wallet", default=None, help="Wallet address (default from POLY_PROXY_WALLET_ADDRESS)")
    parser.add_argument("--activity-limit", type=int, default=DEFAULT_ACTIVITY_LIMIT, help="Activity fetch limit")
    parser.add_argument("--loop", action="store_true", help="Run forever")
    parser.add_argument("--interval-sec", type=int, default=DEFAULT_INTERVAL_SEC, help="Loop interval seconds")
    args = parser.parse_args()

    wallet = load_wallet_from_env_or_args(args.wallet)

    if not args.loop:
        stats = run_once_with_retry(args.db, wallet, args.activity_limit)
        print(
            f"wallet_sync {stats.status} wallet={wallet} positions={stats.positions_rows} "
            f"activity={stats.activity_rows} new_activity={stats.activity_new_rows} "
            f"marks={stats.open_positions_marked} book_queries={stats.book_queries} "
            f"reconcile_status={stats.inventory_reconcile_status} "
            f"reconcile_rows={stats.inventory_reconcile_rows} "
            f"reconcile_repaired_shares={stats.inventory_reconcile_repaired_shares:.6f} "
            f"dust_sell_status={stats.dust_sell_status} "
            f"dust_sell_candidates={stats.dust_sell_candidates} "
            f"dust_sell_submitted={stats.dust_sell_submitted}"
        )
        return 0

    interval = max(60, int(args.interval_sec))
    print(f"wallet_sync worker started wallet={wallet} interval_sec={interval} db={args.db}")
    while True:
        try:
            stats = run_once_with_retry(args.db, wallet, args.activity_limit)
            print(
                f"wallet_sync {stats.status} wallet={wallet} positions={stats.positions_rows} "
                f"activity={stats.activity_rows} new_activity={stats.activity_new_rows} "
                f"marks={stats.open_positions_marked} book_queries={stats.book_queries} "
                f"reconcile_status={stats.inventory_reconcile_status} "
                f"reconcile_rows={stats.inventory_reconcile_rows} "
                f"reconcile_repaired_shares={stats.inventory_reconcile_repaired_shares:.6f} "
                f"dust_sell_status={stats.dust_sell_status} "
                f"dust_sell_candidates={stats.dust_sell_candidates} "
                f"dust_sell_submitted={stats.dust_sell_submitted}"
            )
        except Exception as e:
            print(f"wallet_sync error: {type(e).__name__}: {e}", file=sys.stderr)
        time.sleep(interval)


if __name__ == "__main__":
    raise SystemExit(main())
