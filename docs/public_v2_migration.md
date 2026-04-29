# Public V2 Migration Guide

This branch is the public EVPOLY CLOB V2 surface.

## Public Strategy Set

- `premarket_v1`
- `endgame_sweep_v1`
- `evcurve_v1`
- `sessionband_v1` (S-Band)
- `evsnipe_v1`
- `mm_sport_v1` (MM 2.0)

The legacy generic rewards-MM strategy is retired from the public runtime. Historical changelog entries may still mention it.

## User Setup Changes

- CLOB V2 order posting is local through the Rust SDK.
- Main2 is CLOB V2 only. Do not point the public runtime back at CLOB V1.
- The official EVPOLY builder code is built in; normal users should leave `POLY_BUILDER_CODE` blank.
- Builder fee rates are managed by Polymarket, not by EVPOLY env settings. Set maker/taker rates in the Polymarket Builder Profile; the CLOB applies the active server-side rates when attributed orders match.
- EVPOLY does not set `feeRateBps` or local builder maker/taker bps on orders. It only attaches the builder code.
- `EVPOLY_ALPHA_KEY` is self-serve. With `EVPOLY_ALPHA_AUTO_ONBOARD=true`, runtime auto-registers it on first start when the proxy wallet is present.
- Per-endpoint alpha tokens are optional; blank values fall back to `EVPOLY_ALPHA_KEY`.
- Relayer API keys are still manual for redeem, merge, approval, and Auto-Redeem approval submits.

## Env Template Expectations

- Starter profile enables `premarket`, `endgame`, `evcurve`, `sessionband`, and `evsnipe`.
- `POLY_CLOB_API_URL` should stay on `https://clob.polymarket.com` for the public V2 runtime.
- Starter profile leaves `mm_sport` off until the operator explicitly enables it.
- `EVPOLY_MM_SPORT_*` keys are the only public MM 2.0 strategy controls.
- Old generic rewards-MM controls are not part of the public template.

## Cutover Checklist

1. Copy `.env.example` to `.env`.
2. Fill `POLY_PRIVATE_KEY`, `POLY_SIGNATURE_TYPE`, and `POLY_PROXY_WALLET_ADDRESS` when using proxy/safe mode.
3. Set strategy base sizes explicitly.
4. Leave `EVPOLY_ALPHA_KEY` blank unless you already have one.
5. Confirm active builder fee rates on Polymarket before public release; profile changes may be delayed by Polymarket policy.
6. Start with `./ev start dry`, then move to `./ev start live` after local checks pass.
