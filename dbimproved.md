# DB Maintenance And Endgame Lock-Isolation Plan

## Goal

Turn the current `main2` DB mitigation into a real maintenance system that:

- Keeps `tracking.db`, `tracking.db-wal`, and `tracking.db-shm` from growing forever.
- Keeps terminal/high-churn rows pruned in small batches.
- Runs safe SQLite maintenance only when the runtime is idle enough.
- Avoids putting heavy DB work on the latency-critical endgame submit path.
- Makes DB maintenance visible through logs/admin status so we can prove whether it helped.

## Updated Recommendation

Use this plan in three coding passes, not one large refactor:

1. **Pass 1: observe and maintain files safely.**
   Add DB/WAL file-size reporting, `PRAGMA optimize`, passive checkpoint, and idle-only WAL truncate. This is the smallest change that can fix the large WAL problem without changing trading logic.
2. **Pass 2: prune only reviewed high-churn tables.**
   Keep existing terminal `pending_orders` pruning, then add opt-in side-table pruning one table family at a time. Defaults should stay conservative until we confirm the table is not accounting/audit data.
3. **Pass 3: isolate latency-critical write pressure.**
   Only after Pass 1 and Pass 2 are measured, move non-critical endgame/sessionband diagnostics off the hot submit path. Do not queue order-ack or accounting writes unless the correctness model is reviewed separately.

This keeps the first implementation small enough to review and test. It also avoids hiding a real scheduling/order-placement bug behind a DB refactor.

## Current State Evidence

`TrackingDb` already uses WAL and `synchronous=NORMAL`:

```rust
src/tracking_db.rs
4077:    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
4078:        let db_path = path.as_ref().to_path_buf();
4079:        let mut conn = Connection::open(&db_path)?;
...
4085:        conn.busy_timeout(std::time::Duration::from_millis(busy_timeout_ms))?;
4086:        conn.pragma_update(None, "journal_mode", "WAL")?;
4087:        conn.pragma_update(None, "synchronous", "NORMAL")?;
```

It has one shared write connection behind a mutex plus a read pool:

```rust
src/tracking_db.rs
1007:pub struct TrackingDb {
1008:    conn: Mutex<Connection>,
1009:    read_conns: Vec<Mutex<Connection>>,
1010:    read_rr: AtomicUsize,
1011:}
```

`with_conn` holds the shared write mutex while it executes the closure:

```rust
src/tracking_db.rs
4888:        if can_block_in_place {
4889:            tokio::task::block_in_place(|| {
4890:                let wait_started = Instant::now();
4891:                let conn = self
4892:                    .conn
4893:                    .lock()
4894:                    .map_err(|_| anyhow!("tracking db mutex poisoned"))?;
4895:                let wait_ms = i64::try_from(wait_started.elapsed().as_millis())
...
4899:                let result = f(&conn);
4900:                let hold_ms = i64::try_from(hold_started.elapsed().as_millis())
```

Pending-order pruning exists, but it only deletes old terminal `pending_orders` rows:

```rust
src/main.rs
4631:    if env_bool_named("EVPOLY_PENDING_ORDER_PRUNE_ENABLE", true) {
4632:        let pending_orders_prune_interval_sec =
4633:            env_u64_named("EVPOLY_PENDING_ORDER_PRUNE_INTERVAL_SEC", 300).clamp(30, 86_400);
4634:        let pending_orders_prune_ttl_minutes =
4635:            env_i64_named("EVPOLY_PENDING_ORDER_PRUNE_TTL_MINUTES", 60).clamp(1, 10_080);
```

There are no current source matches for `VACUUM`, `wal_checkpoint`, or `PRAGMA optimize`.

## Design Rules

1. Do not run heavy maintenance inside the normal `TrackingDb::with_conn` write mutex unless the operation is tiny.
2. Use a separate short-lived SQLite maintenance connection for `PRAGMA optimize`, checkpoint, and optional manual vacuum.
3. Never run `VACUUM` automatically in the live hot path. `VACUUM` can take an exclusive lock and can block trading. Make it explicit/manual or startup-only behind a hard opt-in.
4. Run `wal_checkpoint(TRUNCATE)` only when idle enough. If readers/writers are active, skip and retry later.
5. Keep pruning batched and time-bounded. Prefer deleting 1k-50k rows per pass over one giant transaction.
6. Do not delete audit-critical rows by default. Use conservative retention defaults and separate env gates for each table family.
7. Make maintenance observable: every run should log rows deleted, duration, DB size, WAL size, checkpoint result, and skip reason.
8. Skip maintenance near known close/entry windows for endgame, sessionband, premarket, and other latency-sensitive strategy checkpoints. "Idle DB" is not enough if a trade window is about to open.
9. Keep the first patch free of queueing changes. File maintenance and pruning should be proven before changing write ordering.

## Phase 1: Add DB Maintenance API

Create a new set of methods in `src/tracking_db.rs`.

### New Types

Add:

```rust
pub struct DbMaintenanceConfig {
    pub enable: bool,
    pub interval_sec: u64,
    pub idle_p95_wait_ms: i64,
    pub close_window_guard_ms: i64,
    pub max_runtime_ms: i64,
    pub optimize_enable: bool,
    pub checkpoint_enable: bool,
    pub checkpoint_truncate_enable: bool,
    pub side_table_prune_enable: bool,
    pub vacuum_enable: bool,
}

pub struct DbMaintenanceReport {
    pub started_ms: i64,
    pub finished_ms: i64,
    pub duration_ms: i64,
    pub skipped: bool,
    pub skip_reason: Option<String>,
    pub optimize_ran: bool,
    pub checkpoint_mode: Option<String>,
    pub checkpoint_busy: Option<i64>,
    pub checkpoint_log_frames: Option<i64>,
    pub checkpoint_checkpointed_frames: Option<i64>,
    pub pruned_rows: Vec<DbPruneResult>,
    pub db_bytes_before: Option<u64>,
    pub db_bytes_after: Option<u64>,
    pub wal_bytes_before: Option<u64>,
    pub wal_bytes_after: Option<u64>,
}

pub struct DbPruneResult {
    pub table: String,
    pub deleted_rows: u64,
    pub cutoff_ms: i64,
}
```

### New Methods

Add:

- `TrackingDb::maintenance_config_from_env() -> DbMaintenanceConfig`
- `TrackingDb::run_light_maintenance(&self, cfg: &DbMaintenanceConfig) -> Result<DbMaintenanceReport>`
- `TrackingDb::run_manual_vacuum(&self) -> Result<DbMaintenanceReport>`
- `TrackingDb::maintenance_db_path(&self) -> PathBuf`
- `TrackingDb::open_maintenance_conn(&self) -> Result<rusqlite::Connection>`
- `TrackingDb::pragma_optimize(&self, conn: &Connection) -> Result<()>`
- `TrackingDb::wal_checkpoint(&self, conn: &Connection, truncate: bool) -> Result<CheckpointResult>`
- `TrackingDb::database_file_sizes(&self) -> DbFileSizes`

Important: store `db_path: PathBuf` on `TrackingDb`, because today only `new()` has the `db_path` local.

First-pass scope:

- Implement file-size reporting.
- Implement `PRAGMA optimize`.
- Implement passive checkpoint.
- Implement guarded truncate checkpoint.
- Do not implement side-table pruning or fast queueing in the same patch unless the first pass is already stable.

## Phase 2: Safe Periodic Runtime Loop

Add a runtime task in `src/main.rs` near the existing pending-order prune loop.

Default env:

```text
EVPOLY_DB_MAINTENANCE_ENABLE=true
EVPOLY_DB_MAINTENANCE_INTERVAL_SEC=900
EVPOLY_DB_MAINTENANCE_IDLE_P95_WAIT_MS=25
EVPOLY_DB_MAINTENANCE_CLOSE_WINDOW_GUARD_MS=10000
EVPOLY_DB_MAINTENANCE_MAX_RUNTIME_MS=3000
EVPOLY_DB_MAINTENANCE_OPTIMIZE_ENABLE=true
EVPOLY_DB_MAINTENANCE_CHECKPOINT_ENABLE=true
EVPOLY_DB_MAINTENANCE_CHECKPOINT_TRUNCATE_ENABLE=true
EVPOLY_DB_MAINTENANCE_SIDE_PRUNE_ENABLE=false
EVPOLY_DB_VACUUM_ENABLE=false
```

Runtime logic:

1. Every `EVPOLY_DB_MAINTENANCE_INTERVAL_SEC`, read `db_lock_contention_snapshot()`.
2. If `p95_wait_ms > EVPOLY_DB_MAINTENANCE_IDLE_P95_WAIT_MS`, skip and log `db_maintenance_skipped`.
3. If the runtime is inside `EVPOLY_DB_MAINTENANCE_CLOSE_WINDOW_GUARD_MS` of a known strategy checkpoint, skip and log `db_maintenance_skipped`.
4. If not skipped, run `run_light_maintenance()`.
5. Wrap the maintenance call in `tokio::task::spawn_blocking`.
6. Apply a timeout from `EVPOLY_DB_MAINTENANCE_MAX_RUNTIME_MS`.
7. Log `db_maintenance_completed` with the full report.

This avoids running maintenance while DB contention is already bad.

## Phase 3: SQLite Maintenance Commands

### `PRAGMA optimize`

Run on the separate maintenance connection:

```sql
PRAGMA optimize;
```

Why:

- Low risk.
- Lets SQLite refresh planner statistics when useful.
- Does not shrink the DB file, but can improve query planning.

### WAL Checkpoint

Run:

```sql
PRAGMA wal_checkpoint(PASSIVE);
```

Then only if the runtime is idle and WAL is large enough:

```sql
PRAGMA wal_checkpoint(TRUNCATE);
```

Suggested env:

```text
EVPOLY_DB_WAL_TRUNCATE_MIN_BYTES=67108864
EVPOLY_DB_WAL_TRUNCATE_IDLE_P95_WAIT_MS=10
EVPOLY_DB_WAL_TRUNCATE_CLOSE_WINDOW_GUARD_MS=10000
```

Rules:

- If checkpoint returns busy frames, log and do not retry immediately.
- If WAL is below threshold, skip truncate.
- If p95 DB wait is above threshold, skip truncate.

### VACUUM

Do not run automatically by default.

Add explicit/manual paths only:

- CLI/bin: `cargo run --bin db_maintenance -- --vacuum`
- Admin endpoint: optional, guarded by admin token.
- Startup-only hard opt-in: `EVPOLY_DB_VACUUM_ON_STARTUP=true`

Rules:

- Must not run when live trading is active unless explicitly forced.
- Must log estimated DB size before/after.
- Must warn that it can block writes.

## Phase 4: Side-Table Pruning

The current prune only handles terminal `pending_orders`. Add small-batch pruning for high-churn tables that are not required forever.

### Keep Existing Pending Order Prune

Keep current defaults:

```text
EVPOLY_PENDING_ORDER_PRUNE_ENABLE=true
EVPOLY_PENDING_ORDER_PRUNE_INTERVAL_SEC=300
EVPOLY_PENDING_ORDER_PRUNE_TTL_MINUTES=60
EVPOLY_PENDING_ORDER_PRUNE_BATCH_SIZE=50000
```

But rename log wording from "archive" to "prune" unless an archive table is actually added. Today the function name says `archive_and_prune`, but the implementation deletes from `pending_orders`.

### Add Retention For These Tables

Add env-gated batch deletes:

```text
EVPOLY_DB_PRUNE_STRATEGY_FEATURE_SNAPSHOTS_ENABLE=false
EVPOLY_DB_PRUNE_STRATEGY_FEATURE_SNAPSHOTS_TTL_HOURS=168
EVPOLY_DB_PRUNE_STRATEGY_FEATURE_SNAPSHOTS_BATCH_SIZE=25000

EVPOLY_DB_PRUNE_WALLET_ACTIVITY_ENABLE=false
EVPOLY_DB_PRUNE_WALLET_ACTIVITY_TTL_HOURS=168
EVPOLY_DB_PRUNE_WALLET_ACTIVITY_BATCH_SIZE=25000

EVPOLY_DB_PRUNE_PARAMETER_EVENTS_ENABLE=false
EVPOLY_DB_PRUNE_PARAMETER_EVENTS_TTL_HOURS=168
EVPOLY_DB_PRUNE_PARAMETER_EVENTS_BATCH_SIZE=10000

EVPOLY_DB_PRUNE_MM_HOLDER_SNAPSHOTS_ENABLE=false
EVPOLY_DB_PRUNE_MM_HOLDER_SNAPSHOTS_TTL_HOURS=24
EVPOLY_DB_PRUNE_MM_HOLDER_SNAPSHOTS_BATCH_SIZE=25000
```

Do not prune these by default until explicitly reviewed:

- `trade_events`
- `fills_v2`
- `trade_lifecycle_v1`
- `wallet_cashflow_events_v1`

Reason: those are audit/accounting data and should not be deleted casually.

### Prune Implementation Pattern

Each prune method should:

1. Accept `cutoff_ms`, `batch_size`, and `max_rows`.
2. Delete using `rowid IN (SELECT rowid ... LIMIT ?)` where possible.
3. Stop when deleted rows are `0` or less than batch size.
4. Return deleted count.
5. Log each table's deleted rows.

Example pattern:

```sql
DELETE FROM some_table
WHERE rowid IN (
    SELECT rowid
    FROM some_table
    WHERE updated_at_ms > 0
      AND updated_at_ms < ?1
    ORDER BY updated_at_ms ASC
    LIMIT ?2
);
```

## Phase 5: Endgame DB Lock Isolation

This is a later phase, after file maintenance and safe pruning are measured.

The goal is not to remove all DB writes from endgame forever. The goal is to keep non-critical DB work out of the narrow pre-submit window.

### Current Risk

Endgame, SessionBand, Premarket, MM Sport, admin tasks, and maintenance can all write through the same `TrackingDb::conn` mutex. If another task holds it at the wrong time, endgame can be late.

### Add Fast Event Queue Only For Non-Critical Writes

Add a non-blocking DB event writer queue:

- `tokio::sync::mpsc::channel<DbWriteJob>`
- One background writer task owns DB writes from this queue.
- Hot submit paths use `try_send`.
- If queue is full, log to `events.jsonl` and increment a drop counter, but do not block submit.

Suggested env:

```text
EVPOLY_DB_FAST_WRITE_QUEUE_ENABLE=false
EVPOLY_DB_FAST_WRITE_QUEUE_CAPACITY=20000
EVPOLY_DB_FAST_WRITE_QUEUE_DROP_ON_FULL=true
EVPOLY_DB_FAST_WRITE_QUEUE_WARN_DEPTH=10000
```

### Classify Writes

Immediate/blocking writes still allowed:

- Order ack row after actual CLOB response, if needed for correctness.
- Inventory/cashflow state that must be consistent before the next decision.
- Any row needed to recover open order state after a process crash.

Queued/non-blocking writes:

- Decision snapshots.
- Diagnostic trade events.
- Feature snapshots.
- `db_lock_profile`-style telemetry.
- Non-critical skip/reason events.

### Endgame Specific Change

For endgame near close:

1. Build and submit order first.
2. Capture the CLOB result in memory.
3. Persist correctness-critical order state through the existing reliable path.
4. Queue only non-critical snapshot/event writes after submit.
5. If DB queue is full, do not delay order submit; write fallback to `events.jsonl`.

Add event:

```text
db_fast_queue_drop
```

Payload:

- strategy_id
- event_type
- request_id
- queue_depth
- reason
- ts_ms

## Phase 6: Admin And CLI Controls

Add a small maintenance binary:

```text
cargo run --bin db_maintenance -- --status
cargo run --bin db_maintenance -- --optimize
cargo run --bin db_maintenance -- --checkpoint
cargo run --bin db_maintenance -- --checkpoint-truncate
cargo run --bin db_maintenance -- --prune
cargo run --bin db_maintenance -- --vacuum
```

Add admin endpoints only if wanted:

```text
GET  /admin/db/maintenance/status
POST /admin/db/maintenance/run
POST /admin/db/maintenance/checkpoint-truncate
POST /admin/db/maintenance/vacuum
```

The `vacuum` endpoint must require:

- Admin token.
- Explicit JSON body: `{ "confirm": "VACUUM tracking.db" }`
- Live-trading warning in response.

## Phase 7: Observability

Add events:

- `db_maintenance_started`
- `db_maintenance_completed`
- `db_maintenance_skipped`
- `db_checkpoint_completed`
- `db_checkpoint_skipped`
- `db_vacuum_started`
- `db_vacuum_completed`
- `db_prune_completed`
- `db_fast_queue_depth`
- `db_fast_queue_drop`

Report fields:

- `db_bytes_before`
- `db_bytes_after`
- `wal_bytes_before`
- `wal_bytes_after`
- `duration_ms`
- `p95_wait_ms_before`
- `checkpoint_busy`
- `checkpoint_log_frames`
- `checkpoint_checkpointed_frames`
- `table_prune_rows`

Update `db_lock_audit` to also summarize:

- Maintenance events.
- WAL size changes.
- Queue drops.
- Top DB lock hold scopes before/after maintenance.

## Phase 8: Tests

### Unit Tests

Add tests in `tracking_db.rs`:

1. `maintenance_config_defaults_are_safe`
2. `pending_prune_keeps_active_orders`
3. `side_table_prune_obeys_ttl_and_batch_size`
4. `wal_checkpoint_report_parses_busy_log_checkpointed`
5. `manual_vacuum_is_disabled_by_default`
6. `maintenance_skips_when_contention_p95_high`

### Integration/Smoke Tests

Add a temp DB test that:

1. Creates a DB.
2. Inserts many terminal `pending_orders`.
3. Inserts high-churn side-table rows.
4. Runs prune.
5. Runs `PRAGMA optimize`.
6. Runs checkpoint truncate.
7. Verifies active rows are kept and terminal old rows are removed.

### Runtime Validation

Manual validation commands:

```bash
cargo test tracking_db::tests::batched_pending_order_prune_obeys_max_rows
cargo test db_maintenance
cargo run --bin db_maintenance -- --status
cargo run --bin db_maintenance -- --checkpoint-truncate
cargo run --bin db_lock_audit -- events.jsonl 60
```

## Phase 9: Rollout Plan

### Step 1: Metrics Only

Ship status/reporting first:

- File sizes.
- WAL size.
- DB contention p95.
- Table row counts for target prune tables.

No deletes yet except existing `pending_orders` prune.

### Step 2: Enable `PRAGMA optimize`

Turn on:

```text
EVPOLY_DB_MAINTENANCE_OPTIMIZE_ENABLE=true
EVPOLY_DB_MAINTENANCE_CHECKPOINT_ENABLE=false
EVPOLY_DB_MAINTENANCE_SIDE_PRUNE_ENABLE=false
```

Watch DB lock p95 and runtime logs.

### Step 3: Enable Passive Checkpoint

Turn on:

```text
EVPOLY_DB_MAINTENANCE_CHECKPOINT_ENABLE=true
EVPOLY_DB_MAINTENANCE_CHECKPOINT_TRUNCATE_ENABLE=false
```

Confirm it does not raise p95 lock wait.

### Step 4: Enable WAL Truncate When Idle

Turn on:

```text
EVPOLY_DB_MAINTENANCE_CHECKPOINT_TRUNCATE_ENABLE=true
EVPOLY_DB_WAL_TRUNCATE_MIN_BYTES=67108864
```

Confirm WAL shrinks and no endgame late drops increase.

### Step 5: Enable Side-Table Pruning

Turn on one table family at a time.

Start with low-risk operational tables:

1. `strategy_feature_snapshot_transition_rejections`
2. parameter event/history tables
3. wallet activity/latest tables if confirmed non-accounting
4. MM holder snapshots

Do not prune accounting/audit tables until a separate data-retention decision is made.

### Step 6: Add Fast DB Queue For Non-Critical Endgame/SessionBand Diagnostics

Ship behind:

```text
EVPOLY_DB_FAST_WRITE_QUEUE_ENABLE=false
```

Turn it on for dry-run first, then live. Keep correctness-critical order state on the existing reliable path unless a separate recovery design is approved.

## Acceptance Criteria

This work is done when:

- `src/` contains real `PRAGMA optimize` and `wal_checkpoint` implementation.
- Runtime logs show periodic maintenance reports.
- WAL file is truncated when idle and above threshold.
- Heavy maintenance skips when DB contention is high.
- Existing `pending_orders` prune still passes tests.
- Added side-table prune tests pass.
- Endgame submit path does not synchronously wait on non-critical DB snapshot/event writes.
- `db_lock_audit` can show before/after contention and maintenance activity.
- `cargo check --all-targets` and `cargo test --all-targets` pass.

## Non-Goals

- Do not delete accounting-grade history by default.
- Do not run automatic live `VACUUM`.
- Do not replace SQLite with another database in this task.
- Do not refactor every DB call at once. Start by isolating only latency-critical endgame/sessionband submit-adjacent writes.

