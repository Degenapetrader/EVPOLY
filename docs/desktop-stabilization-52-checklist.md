# Desktop Stabilization 52-Task Checklist

## Data and Contract
1. [x] Replace open-position `current_price=entry_price` mapping.
2. [x] Add mark-price join from `marks_v2` for open positions.
3. [x] Add fallback query when `marks_v2` is unavailable.
4. [x] Expose `realized_pnl` in open-position payload.
5. [x] Expose `unrealized_pnl` in open-position payload.
6. [x] Keep backward-compatible aggregate `pnl` field.
7. [x] Update dashboard table to handle nullable current price.
8. [x] Update dashboard table to display realized/unrealized separately.
9. [x] Add explicit `eoa_wallet_address` to profile model.
10. [x] Add explicit `proxy_wallet_address` to profile model.
11. [x] Add profile wallet-field normalization/migration.
12. [x] Add primary-wallet resolver (`sig_type` aware).
13. [x] Update config/env generation to use primary wallet.
14. [x] Update wallet balance lookup to use primary wallet.
15. [x] Extend desktop config with `eoa_wallet`.
16. [x] Extend desktop config with full onboarding token fields.
17. [x] Persist onboarding discovery/alpha/admin tokens in secrets.
18. [x] Return onboarding tokens back through config load API.

## Frontend/Backend Boundary
19. [x] Send compatible camelCase + snake_case args for `create_profile`.
20. [x] Send compatible camelCase + snake_case args for `save_config`.
21. [x] Send compatible camelCase + snake_case args for `get_saved_config`.
22. [x] Send compatible camelCase + snake_case args for `export_config`.
23. [x] Send compatible camelCase + snake_case args for `run_onboarding`.
24. [x] Add command-contract unit tests for invoke payloads.
25. [x] Add `npm test` script for CI command-contract checks.
26. [x] Wire frontend tests into desktop preflight workflow.
27. [x] Wire frontend tests into release preflight jobs.

## Config, Import, Onboarding Flows
28. [x] Add EOA wallet input in configuration UI.
29. [x] Keep proxy wallet input explicit in configuration UI.
30. [x] Update default profile creation to pass EOA + proxy fields.
31. [x] Change `import_config` command to return imported profile id.
32. [x] Activate imported profile immediately after import.
33. [x] Reload config immediately after import activation.
34. [x] Apply onboarding token results to editable config state.
35. [x] Save onboarding-derived wallet back into config state.
36. [x] Add advanced inputs for all remote token fields.
37. [x] Surface config-load warnings instead of silent defaulting.

## Status and Runtime Semantics
38. [x] Stop mapping bot status poll failures to `stopped`.
39. [x] Add `unknown` visual state in status badge.
40. [x] Surface profile switch failures in UI.
41. [x] Surface trade-data stale/error state.
42. [x] Surface wallet-balance stale/error state.
43. [x] Keep bot status `starting` until runtime output appears.
44. [x] Keep manual service status `starting` until runtime output appears.
45. [x] Keep `stopping` state until process termination event.
46. [x] Block start while service state is transitional (`starting/stopping`).

## Auth, UX, and Accessibility
47. [x] Run auth initialization before legal modal gating.
48. [x] Surface auth initialization failure with retry UI.
49. [x] Fix Terms link to `desktop` branch document path.
50. [x] Remove global `user-select: none`.
51. [x] Remove global hidden-overflow lock on `body/#root`.
52. [x] Add responsive breakpoints for dashboard core grids.
