# v2.6.3 dependency review — 2026-09-18

Compatible lockfile updates fix the h2 empty DATA frame issue (0.4.16),
ruint shift issue (1.20.0), and rustls encryption-level boundary issue (0.23.45).
rust_decimal 1.43.0 removes the optional, unsupported rkyv 0.7 archive graph.
chacha20 moves to the non-yanked 0.10.2. No trading/signing API is replaced.

The audit remains strict and rejects unreviewed warnings. Exceptions below expire
2026-10-31 and require another review; they are not permanent suppressions.

| Advisory | Evidence and disposition |
| --- | --- |
| RUSTSEC-2025-0012 | `cargo tree -i backoff`: SDK websocket/RTDS retry dependency, unmaintained. Replacing it changes retry behavior and is deferred to a dedicated SDK update. |
| RUSTSEC-2024-0384 | `instant` comes through that backoff dependency; maintenance warning, not a reported vulnerability. |
| RUSTSEC-2024-0436 | `paste` is a compile-time Alloy macro dependency; maintenance warning. |
| RUSTSEC-2026-0173 | `proc-macro-error2` is compile-time macro error handling; maintenance warning. |
| RUSTSEC-2024-0388 | `cargo tree -i derivative --target all` has no active path; optional lockfile-only macro. |
| RUSTSEC-2026-0253 | `lru 0.16.4` is used only by `alloy-provider 1.8.3`. `src/blocks.rs` uses `LruCache<BlockNumber, ...>` (`BlockNumber = u64`); `src/layers/cache.rs` uses `LruCache<B256, String, ...>`. Both key types have no Drop implementation. The advisory requires a panicking key destructor during `pop()`, unwinding recovery, and reuse of the damaged cache; that condition is unavailable with these concrete key types. Re-review before changing Alloy or cache key types. |

The old quinn-proto exception is removed because the fresh audit no longer needs it.
There are no unexcepted vulnerability findings after the compatible updates.

Source: [RustSec lru advisory](https://rustsec.org/advisories/RUSTSEC-2026-0253.html).
Validation uses `scripts/security_audit.sh` and `cargo tree` against the committed lockfile.
