import { describe, expect, it } from "vitest";
import {
  PREMARKET_DEFAULT_LADDER_PRICES_M5,
  PREMARKET_DEFAULT_LADDER_PRICES_NON_M5,
  PREMARKET_DEFAULT_LADDER_WEIGHTS,
  premarketLadderPricesForMode,
  premarketLadderWeights,
} from "./desktop-config";

describe("premarket ladder safety helpers", () => {
  it("derives safe and aggressive ladders from the split defaults", () => {
    expect(premarketLadderPricesForMode("safe", "m5")).toEqual([0.28, 0.24, 0.2, 0.15, 0.09, 0.03]);
    expect(premarketLadderPricesForMode("safe", "non_m5")).toEqual([
      0.36, 0.27, 0.22, 0.17, 0.11, 0.06,
    ]);
    expect(premarketLadderPricesForMode("aggressive", "m5")).toEqual([
      0.35, 0.29, 0.25, 0.18, 0.1, 0.04,
    ]);
    expect(premarketLadderPricesForMode("aggressive", "non_m5")).toEqual([
      0.44, 0.33, 0.27, 0.2, 0.14, 0.07,
    ]);
    expect(premarketLadderWeights()).toEqual([...PREMARKET_DEFAULT_LADDER_WEIGHTS]);
  });

  it("keeps the new split defaults stable", () => {
    expect(PREMARKET_DEFAULT_LADDER_PRICES_M5).toEqual([0.31, 0.26, 0.22, 0.16, 0.09, 0.03]);
    expect(PREMARKET_DEFAULT_LADDER_PRICES_NON_M5).toEqual([0.4, 0.3, 0.24, 0.18, 0.12, 0.06]);
  });
});
