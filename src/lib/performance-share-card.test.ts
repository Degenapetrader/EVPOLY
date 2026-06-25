import { describe, expect, it, vi } from "vitest";

import {
  buildDailyPnlShareCard,
  buildLiquidityRewardShareCard,
  buildPerformanceShareCardSvg,
  pickPerformanceShareCardBackground,
} from "./performance-share-card";

describe("performance share card backgrounds", () => {
  it("uses positive backgrounds for positive PnL and reward cards", () => {
    vi.spyOn(Math, "random").mockReturnValue(0);
    const pnlCard = buildDailyPnlShareCard({
      pnl: 12.34,
      openPnl: 1.23,
      realizedPnl: 11.11,
      sourceLabel: "Snapshot",
      feedLabel: "Synced",
      updatedLabel: "now",
      series: [{ ts: "2026-06-25T00:00:00Z", value: 12.34, raw_value: 12.34 }],
    });
    const rewardCard = buildLiquidityRewardShareCard({ reward: 102.93 });

    expect(pnlCard).not.toBeNull();
    expect(rewardCard).not.toBeNull();
    expect(pickPerformanceShareCardBackground(pnlCard!)).toMatch(
      /^\/assets\/referral-cards\//
    );
    expect(pickPerformanceShareCardBackground(rewardCard!)).toMatch(
      /^\/assets\/referral-cards\//
    );
    expect(buildPerformanceShareCardSvg(rewardCard!)).toContain(
      'x="1210" y="558"'
    );
  });

  it("uses negative backgrounds for negative PnL cards", () => {
    vi.spyOn(Math, "random").mockReturnValue(0);
    const card = buildDailyPnlShareCard({
      pnl: -12.34,
      openPnl: -1.23,
      realizedPnl: -11.11,
      sourceLabel: "Snapshot",
      feedLabel: "Synced",
      updatedLabel: "now",
      series: [{ ts: "2026-06-25T00:00:00Z", value: -12.34, raw_value: -12.34 }],
    });

    expect(card).not.toBeNull();
    expect(pickPerformanceShareCardBackground(card!)).toMatch(
      /^\/assets\/performance-negative-cards\//
    );
    expect(buildPerformanceShareCardSvg(card!)).not.toContain(
      'x="1210" y="558"'
    );
  });
});
