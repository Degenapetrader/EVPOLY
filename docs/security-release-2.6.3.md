# v2.6.3 local dependency review

Reviewed 2026-09-18. Release checks run on the local computer because GitHub Actions credits are exhausted.

Compatible lockfile updates address h2, ruint, rustls, quinn-proto, tar, anyhow, and quick-xml (through plist). rust_decimal removes the optional rkyv 0.7 graph. chacha20 is no longer yanked. Linux also updates crossbeam-epoch. Trading and wallet APIs are unchanged.

A fresh cargo audit reports zero vulnerability findings. Existing informational warnings remain in the Tauri/SDK graph:

- Maintenance warnings: derivative, fxhash, paste, proc-macro-error, proc-macro-error2, and the five unic crates. A framework migration is outside this release.
- lru 0.16.4 is lockfile-only for this UI feature set: cargo tree -i lru --target all reports no active path.
- rand 0.7.3 is a build-time perfect-hash generator. The affected rand log feature is disabled. The same missing log feature excludes the reported reentrant-logger condition for any remaining SDK rand 0.10.0 entry.
- glib 0.18.5 is a Linux GTK dependency. RUSTSEC-2024-0429 affects VariantStrIter/str_iter. No use was found in app code or the immediate Tauri, Tao, Wry, GTK and Gio sources. This source review is not a native Linux runtime test; native Linux build and smoke verification remain necessary before Linux publication.

These observations do not suppress future audit findings. Re-review retained framework warnings by 2026-10-31, or sooner if framework versions/features change.