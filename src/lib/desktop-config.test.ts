import { describe, expect, it } from "vitest";
import {
  inferPremarketLadderSafetyMode,
  PREMARKET_DEFAULT_LADDER_PRICES,
  PREMARKET_DEFAULT_LADDER_WEIGHTS,
  premarketLadderPricesForMode,
  premarketLadderWeights,
} from "./desktop-config";

describe("premarket ladder safety helpers", () => {
  it("derives safe and extra safe ladders from the defaults", () => {
    expect(premarketLadderPricesForMode("safe")).toEqual([0.36, 0.27, 0.22, 0.19, 0.14, 0.11]);
    expect(premarketLadderPricesForMode("extra_safe")).toEqual([
      0.32, 0.24, 0.2, 0.17, 0.12, 0.1,
    ]);
    expect(premarketLadderWeights()).toEqual([...PREMARKET_DEFAULT_LADDER_WEIGHTS]);
  });

  it("infers preset and custom ladder modes correctly", () => {
    expect(
      inferPremarketLadderSafetyMode(
        [...PREMARKET_DEFAULT_LADDER_PRICES],
        [...PREMARKET_DEFAULT_LADDER_WEIGHTS]
      )
    ).toBe("normal");
    expect(
      inferPremarketLadderSafetyMode(
        premarketLadderPricesForMode("safe"),
        [...PREMARKET_DEFAULT_LADDER_WEIGHTS]
      )
    ).toBe("safe");
    expect(
      inferPremarketLadderSafetyMode(
        premarketLadderPricesForMode("extra_safe"),
        [...PREMARKET_DEFAULT_LADDER_WEIGHTS]
      )
    ).toBe("extra_safe");
    expect(inferPremarketLadderSafetyMode([0.41, 0.29, 0.24], [0.23, 0.23, 0.17])).toBe(
      "custom"
    );
    expect(inferPremarketLadderSafetyMode(undefined, undefined)).toBe("normal");
  });
});
