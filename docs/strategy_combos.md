# Strategy Combo Guide

## Hard Rules
- `mm_sport_v1` (MM 2.0) should not be combined with high-frequency directional profiles until sizing and inventory risk are reviewed.
- `endgame_sweep_v1` and `sessionband_v1` (S-Band) are both late-window close strategies; review sizing before running both heavy.

## Recommended Profiles

### Profile A: Premarket Only
- Enable: `premarket_v1`
- Disable: `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`, `mm_sport_v1`

### Profile B: Premarket + EVcurve
- Enable: `premarket_v1`, `evcurve_v1`
- Disable: `endgame_sweep_v1`, `sessionband_v1`, `evsnipe_v1`, `mm_sport_v1`

### Profile C: Directional Stack
- Enable: `premarket_v1`, `endgame_sweep_v1`, optional `evcurve_v1`, optional `sessionband_v1`, optional `evsnipe_v1`
- Disable: `mm_sport_v1`

### Profile D: MM 2.0
- Enable: `mm_sport_v1`
- Disable or review sizing for: `premarket_v1`, `endgame_sweep_v1`, `evcurve_v1`, `sessionband_v1`, `evsnipe_v1`

## Example Toggle Set (Directional)
```bash
EVPOLY_STRATEGY_PREMARKET_ENABLE=true
EVPOLY_STRATEGY_ENDGAME_ENABLE=true
EVPOLY_STRATEGY_EVCURVE_ENABLE=true
EVPOLY_STRATEGY_SESSIONBAND_ENABLE=true
EVPOLY_STRATEGY_EVSNIPE_ENABLE=true
EVPOLY_STRATEGY_MM_SPORT_ENABLE=false
```

## Example Toggle Set (MM 2.0)
```bash
EVPOLY_STRATEGY_PREMARKET_ENABLE=false
EVPOLY_STRATEGY_ENDGAME_ENABLE=false
EVPOLY_STRATEGY_EVCURVE_ENABLE=false
EVPOLY_STRATEGY_SESSIONBAND_ENABLE=false
EVPOLY_STRATEGY_EVSNIPE_ENABLE=false
EVPOLY_STRATEGY_MM_SPORT_ENABLE=true
```
