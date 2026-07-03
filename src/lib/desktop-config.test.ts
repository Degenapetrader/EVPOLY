import { describe, expect, it } from "vitest";
import {
  DEFAULT_CONFIG,
  VISIBLE_STRATEGIES,
  mergeConfig,
  strategyControlSuffix,
  strategySizeValue,
  updateStrategySize,
} from "./desktop-config";
import type { BotConfig } from "./tauri-commands";

describe("desktop config strategy defaults", () => {
  it("keeps Endgame last and off for new configs", () => {
    expect(DEFAULT_CONFIG.strategies.endgame).toBe(false);
    expect(mergeConfig(null).strategies.endgame).toBe(false);
    expect(VISIBLE_STRATEGIES[VISIBLE_STRATEGIES.length - 1]?.key).toBe("endgame");
  });

  it("preserves explicitly enabled Endgame profiles", () => {
    expect(
      mergeConfig({
        strategies: { endgame: true },
      } as Partial<BotConfig>).strategies.endgame
    ).toBe(true);
  });
});

describe("desktop config MM 2.0 sizing profiles", () => {
  it("uses route-aware fresh market cap defaults", () => {
    expect(mergeConfig(null).strategy_settings.mm_sport).toMatchObject({
      discovery_route: "sports",
      active_sport_market_cap: 100,
      active_nonsport_market_cap: 0,
    });

    expect(
      mergeConfig({
        strategy_settings: { mm_sport: { discovery_route: "nonsports" } },
      } as Partial<BotConfig>).strategy_settings.mm_sport
    ).toMatchObject({
      active_sport_market_cap: 0,
      active_nonsport_market_cap: 100,
    });

    expect(
      mergeConfig({
        strategy_settings: { mm_sport: { discovery_route: "dual" } },
      } as Partial<BotConfig>).strategy_settings.mm_sport
    ).toMatchObject({
      active_sport_market_cap: 50,
      active_nonsport_market_cap: 50,
    });
  });

  it("normalizes fresh caps and cooldown windows", () => {
    const merged = mergeConfig({
      strategy_settings: {
        mm_sport: {
          discovery_route: "dual",
          active_sport_market_cap: 50.9,
          active_nonsport_market_cap: -1,
          quote_cooldown_min_sec: 61.8,
          quote_cooldown_max_sec: 10,
          nonsport_entry_schedule_start_minute_utc: 2000,
          nonsport_entry_schedule_end_minute_utc: -1,
        },
      },
    } as Partial<BotConfig>);

    expect(merged.strategy_settings.mm_sport.active_sport_market_cap).toBe(50);
    expect(merged.strategy_settings.mm_sport.active_nonsport_market_cap).toBe(50);
    expect(merged.strategy_settings.mm_sport.quote_cooldown_min_sec).toBe(61);
    expect(merged.strategy_settings.mm_sport.quote_cooldown_max_sec).toBe(61);
    expect(merged.strategy_settings.mm_sport.sport_entry_schedule_start_minute_utc).toBe(1439);
    expect(merged.strategy_settings.mm_sport.sport_entry_schedule_end_minute_utc).toBe(240);
    expect(merged.strategy_settings.mm_sport.nonsport_entry_schedule_start_minute_utc).toBe(1439);
    expect(merged.strategy_settings.mm_sport.nonsport_entry_schedule_end_minute_utc).toBe(240);
  });

  it("normalizes MM 2.0 entry price mode", () => {
    expect(mergeConfig(null).strategy_settings.mm_sport.entry_price_mode).toBe("best_bid");
    expect(
      mergeConfig({
        strategy_settings: { mm_sport: { entry_price_mode: "best-bid" } },
      } as unknown as Partial<BotConfig>).strategy_settings.mm_sport.entry_price_mode
    ).toBe("best_bid");
    expect(
      mergeConfig({
        strategy_settings: { mm_sport: { entry_price_mode: "market" } },
      } as unknown as Partial<BotConfig>).strategy_settings.mm_sport.entry_price_mode
    ).toBe("best_bid");
  });

  it("defaults and falls back MM 2.0 max quote share caps", () => {
    expect(mergeConfig(null).strategy_settings.mm_sport).toMatchObject({
      max_quote_shares: 1000,
      nonsport_max_quote_shares: 200,
    });

    const merged = mergeConfig({
      strategy_settings: { mm_sport: { max_quote_shares: 250 } },
    } as unknown as Partial<BotConfig>);

    expect(merged.strategy_settings.mm_sport.max_quote_shares).toBe(250);
    expect(merged.strategy_settings.mm_sport.nonsport_max_quote_shares).toBe(250);
  });

  it("migrates stale route-default caps when the MM 2.0 route changes", () => {
    expect(
      mergeConfig({
        strategy_settings: {
          mm_sport: {
            discovery_route: "dual",
            active_sport_market_cap: 0,
            active_nonsport_market_cap: 100,
          },
        },
      } as Partial<BotConfig>).strategy_settings.mm_sport
    ).toMatchObject({
      active_sport_market_cap: 50,
      active_nonsport_market_cap: 50,
    });

    expect(
      mergeConfig({
        strategy_settings: {
          mm_sport: {
            discovery_route: "sports",
            active_sport_market_cap: 0,
            active_nonsport_market_cap: 100,
          },
        },
      } as Partial<BotConfig>).strategy_settings.mm_sport
    ).toMatchObject({
      active_sport_market_cap: 100,
      active_nonsport_market_cap: 0,
    });
  });

  it("falls back missing Non-S sizing fields to Sport values", () => {
    const merged = mergeConfig({
      mm_tuning: {
        rewards_min_share_multiple: 1,
        sport_quote_size_multiplier: 2,
      },
      strategy_settings: {
        mm_sport: {
          quote_size_mode: "multiple",
          max_share_ratio: 0.12,
          min_top_depth_usd: 1500,
        },
      },
    } as Partial<BotConfig>);

    expect(merged.mm_tuning.nonsport_quote_size_multiplier).toBe(2);
    expect(merged.strategy_settings.mm_sport.nonsport_quote_size_mode).toBe("multiple");
    expect(merged.strategy_settings.mm_sport.nonsport_multiple_collateral_cap_mult).toBe(0.45);
    expect(merged.strategy_settings.mm_sport.nonsport_depth_ratio_collateral_cap_mult).toBe(0.45);
    expect(merged.strategy_settings.mm_sport.nonsport_max_share_ratio).toBe(0.12);
    expect(merged.strategy_settings.mm_sport.nonsport_min_top_depth_usd).toBe(1500);
  });

  it("preserves explicit Non-S sizing overrides", () => {
    const merged = mergeConfig({
      mm_tuning: {
        ...DEFAULT_CONFIG.mm_tuning,
        sport_quote_size_multiplier: 2,
        nonsport_quote_size_multiplier: 0.7,
      },
      strategy_settings: {
        mm_sport: {
          ...DEFAULT_CONFIG.strategy_settings.mm_sport,
          quote_size_mode: "multiple",
          nonsport_quote_size_mode: "depth_ratio",
          nonsport_max_share_ratio: 0.05,
          nonsport_min_top_depth_usd: 900,
        },
      },
    } as Partial<BotConfig>);

    expect(merged.mm_tuning.nonsport_quote_size_multiplier).toBe(0.7);
    expect(merged.strategy_settings.mm_sport.nonsport_quote_size_mode).toBe("depth_ratio");
    expect(merged.strategy_settings.mm_sport.nonsport_multiple_collateral_cap_mult).toBe(0.45);
    expect(merged.strategy_settings.mm_sport.nonsport_depth_ratio_collateral_cap_mult).toBe(0.45);
    expect(merged.strategy_settings.mm_sport.nonsport_max_share_ratio).toBe(0.05);
    expect(merged.strategy_settings.mm_sport.nonsport_min_top_depth_usd).toBe(900);
  });

  it("uses the active Sport or Non-S route for the compact MM 2.0 rail size", () => {
    const config: BotConfig = {
      ...DEFAULT_CONFIG,
      mm_tuning: {
        ...DEFAULT_CONFIG.mm_tuning,
        sport_quote_size_multiplier: 2,
        nonsport_quote_size_multiplier: 0.7,
      },
      strategy_settings: {
        ...DEFAULT_CONFIG.strategy_settings,
        mm_sport: {
          ...DEFAULT_CONFIG.strategy_settings.mm_sport,
          discovery_route: "nonsports",
          quote_size_mode: "multiple",
          nonsport_quote_size_mode: "depth_ratio",
          max_share_ratio: 0.12,
          nonsport_max_share_ratio: 0.05,
        },
      },
    };

    expect(strategySizeValue(config, "mm_sport")).toBe(0.05);
    expect(strategyControlSuffix("mm_sport", config)).toBe("DEPTH");

    const updated = updateStrategySize(config, "mm_sport", 0.08);

    expect(updated.strategy_settings.mm_sport.max_share_ratio).toBe(0.12);
    expect(updated.strategy_settings.mm_sport.nonsport_max_share_ratio).toBe(0.08);
  });

  it("does not mutate MM 2.0 sizing from the compact rail in Dual route", () => {
    const config: BotConfig = {
      ...DEFAULT_CONFIG,
      strategy_settings: {
        ...DEFAULT_CONFIG.strategy_settings,
        mm_sport: {
          ...DEFAULT_CONFIG.strategy_settings.mm_sport,
          discovery_route: "dual",
          max_share_ratio: 0.05,
          nonsport_max_share_ratio: 0.1,
        },
      },
    };

    expect(updateStrategySize(config, "mm_sport", 0.2)).toBe(config);
  });
});
