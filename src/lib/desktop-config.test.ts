import { describe, expect, it } from "vitest";
import { DEFAULT_CONFIG, mergeConfig } from "./desktop-config";
import type { BotConfig } from "./tauri-commands";

describe("desktop config MM 2.0 sizing profiles", () => {
  it("falls back missing Non-S sizing fields to Sport values", () => {
    const merged = mergeConfig({
      mm_tuning: {
        rewards_min_share_multiple: 1,
        sport_quote_size_multiplier: 2,
      },
      strategy_settings: {
        mm_sport: {
          quote_size_mode: "multiple",
          multiple_collateral_cap_mult: 0.3,
          depth_ratio_collateral_cap_mult: 0.8,
          max_share_ratio: 0.12,
          min_top_depth_usd: 1500,
        },
      },
    } as Partial<BotConfig>);

    expect(merged.mm_tuning.nonsport_quote_size_multiplier).toBe(2);
    expect(merged.strategy_settings.mm_sport.nonsport_quote_size_mode).toBe("multiple");
    expect(merged.strategy_settings.mm_sport.nonsport_multiple_collateral_cap_mult).toBe(0.3);
    expect(merged.strategy_settings.mm_sport.nonsport_depth_ratio_collateral_cap_mult).toBe(0.8);
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
          nonsport_multiple_collateral_cap_mult: 0.25,
          nonsport_depth_ratio_collateral_cap_mult: 0.55,
          nonsport_max_share_ratio: 0.05,
          nonsport_min_top_depth_usd: 900,
        },
      },
    } as Partial<BotConfig>);

    expect(merged.mm_tuning.nonsport_quote_size_multiplier).toBe(0.7);
    expect(merged.strategy_settings.mm_sport.nonsport_quote_size_mode).toBe("depth_ratio");
    expect(merged.strategy_settings.mm_sport.nonsport_multiple_collateral_cap_mult).toBe(0.25);
    expect(merged.strategy_settings.mm_sport.nonsport_depth_ratio_collateral_cap_mult).toBe(0.55);
    expect(merged.strategy_settings.mm_sport.nonsport_max_share_ratio).toBe(0.05);
    expect(merged.strategy_settings.mm_sport.nonsport_min_top_depth_usd).toBe(900);
  });
});
