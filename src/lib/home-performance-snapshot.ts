import type {
  HomeOverview,
  ProfilePerformanceRange,
  ProfilePerformanceView,
  ProfilePerformancePoint,
} from "./platform-api";

export type HomePerformanceDailyStat = {
  date: string;
  pnl: number | null;
  volume: number | null;
  makerRebate: number | null;
  lpRewards: number | null;
  trades: number;
};

export type HomePerformanceWindow = {
  range: ProfilePerformanceRange;
  label: string;
  pnl: number | null;
  series: ProfilePerformancePoint[];
};

export type HomePerformanceSnapshot = {
  profileName: string | null;
  walletAddress: string | null;
  workerRunning: boolean;
  sourceMode: "live" | "snapshot";
  availableBalance: number | null;
  openPositionsValue: number | null;
  portfolioValue: number | null;
  pnl: number | null;
  openPnl: number | null;
  realizedPnl: number | null;
  allTimePnl: number | null;
  allTimeVolume: number | null;
  allTimeVolumePartial: boolean;
  makerRebateLifetime: number | null;
  makerRebateLifetimePartial: boolean;
  lpRewardsLifetime: number | null;
  pnlAsOfUtc: string | null;
  pnlSourceLabel: string;
  pnlFeedLabel: string;
  rewardsToday: number | null;
  rewardsLifetime: number | null;
  rewardsAsOfUtc: string | null;
  rewardsError: string | null;
  series: ProfilePerformancePoint[];
  windows: Record<ProfilePerformanceRange, HomePerformanceWindow>;
  dailyStats: HomePerformanceDailyStat[];
};

const PERFORMANCE_RANGES: ProfilePerformanceRange[] = ["6h", "1d", "7d", "30d", "all"];

const PERFORMANCE_RANGE_LABELS: Record<ProfilePerformanceRange, string> = {
  "6h": "6H",
  "1d": "1D",
  "7d": "7D",
  "30d": "30D",
  all: "ALL",
};

const firstFinite = (...values: Array<number | null | undefined>): number | null => {
  for (const value of values) {
    if (typeof value === "number" && Number.isFinite(value)) {
      return value;
    }
  }
  return null;
};

const buildEmptyWindows = (): Record<ProfilePerformanceRange, HomePerformanceWindow> => ({
  "6h": { range: "6h", label: PERFORMANCE_RANGE_LABELS["6h"], pnl: null, series: [] },
  "1d": { range: "1d", label: PERFORMANCE_RANGE_LABELS["1d"], pnl: null, series: [] },
  "7d": { range: "7d", label: PERFORMANCE_RANGE_LABELS["7d"], pnl: null, series: [] },
  "30d": { range: "30d", label: PERFORMANCE_RANGE_LABELS["30d"], pnl: null, series: [] },
  all: { range: "all", label: PERFORMANCE_RANGE_LABELS.all, pnl: null, series: [] },
});

export function buildHomePerformanceSnapshot(input: {
  overview: HomeOverview | null;
  performance: ProfilePerformanceView | null;
  publicOpenPositionsValue: number | null;
}): HomePerformanceSnapshot {
  const { overview, performance, publicOpenPositionsValue } = input;
  const workerRunning = overview?.bot_state === "running";
  const sourceMode: HomePerformanceSnapshot["sourceMode"] =
    performance ? "live" : "snapshot";
  const availableBalance = firstFinite(
    overview?.available_balance,
    performance?.available_balance,
  );
  const openPositionsValue = firstFinite(
    publicOpenPositionsValue,
    overview?.portfolio_value,
    performance?.position_value,
  );
  const portfolioValue =
    availableBalance !== null && openPositionsValue !== null
      ? availableBalance + openPositionsValue
      : openPositionsValue ?? availableBalance;

  const livePnl = firstFinite(performance?.profit_loss);
  const snapshotPnl = firstFinite(
    overview?.pnl_today_utc,
    performance?.profit_loss,
  );
  const pnl = sourceMode === "live" ? livePnl : snapshotPnl;
  const allTimePnl = sourceMode === "live"
    ? firstFinite(performance?.all_time?.profit_loss, performance?.windows?.all?.profit_loss)
    : null;
  const lpRewardsLifetime = firstFinite(
    sourceMode === "live" ? performance?.all_time?.lp_rewards : null,
    overview?.liquidity_rewards_lifetime,
  );
  const windows = buildEmptyWindows();
  if (sourceMode === "live" && performance) {
    for (const range of PERFORMANCE_RANGES) {
      const window = performance.windows?.[range] ?? null;
      if (window) {
        windows[range] = {
          range,
          label: window.label || PERFORMANCE_RANGE_LABELS[range],
          pnl: firstFinite(window.profit_loss),
          series: Array.isArray(window.series) ? window.series : [],
        };
      }
    }
    windows["1d"] = {
      ...windows["1d"],
      pnl: firstFinite(windows["1d"].pnl, livePnl),
      series: windows["1d"].series.length > 0 ? windows["1d"].series : performance.series ?? [],
    };
    windows.all = {
      ...windows.all,
      pnl: firstFinite(windows.all.pnl, allTimePnl),
    };
  } else {
    windows["1d"] = {
      ...windows["1d"],
      pnl: snapshotPnl,
    };
    windows.all = {
      ...windows.all,
      pnl: allTimePnl,
    };
  }
  const series = sourceMode === "live" ? windows["1d"].series : [];
  const dailyStatsPartial = sourceMode === "live" && performance?.daily_stats_partial === true;

  return {
    profileName: performance?.profile_name ?? null,
    walletAddress: null,
    workerRunning,
    sourceMode,
    availableBalance,
    openPositionsValue,
    portfolioValue,
    pnl,
    openPnl: sourceMode === "live" ? performance?.open_pnl ?? null : null,
    realizedPnl: sourceMode === "live" ? performance?.realized_pnl ?? null : null,
    allTimePnl,
    allTimeVolume: sourceMode === "live" ? performance?.all_time?.volume ?? null : null,
    allTimeVolumePartial: sourceMode === "live" ? performance?.all_time?.volume_partial === true : false,
    makerRebateLifetime: sourceMode === "live" ? performance?.all_time?.maker_rebate ?? null : null,
    makerRebateLifetimePartial:
      sourceMode === "live" ? performance?.all_time?.maker_rebate_partial === true : false,
    lpRewardsLifetime,
    pnlAsOfUtc:
      sourceMode === "live"
        ? performance?.as_of_utc ?? null
        : overview?.liquidity_rewards_as_of_utc ?? performance?.as_of_utc ?? null,
    pnlSourceLabel:
      sourceMode === "live" && performance?.source?.startsWith("polymarket")
        ? "Polymarket"
        : "Snapshot",
    pnlFeedLabel:
      sourceMode === "live"
        ? performance?.source === "polymarket_user_pnl_empty"
          ? "No chart"
          : "Synced"
        : "Latest",
    rewardsToday: overview?.liquidity_rewards_today ?? null,
    rewardsLifetime: overview?.liquidity_rewards_lifetime ?? null,
    rewardsAsOfUtc: overview?.liquidity_rewards_as_of_utc ?? null,
    rewardsError: overview?.liquidity_rewards_error ?? null,
    series,
    windows,
    dailyStats: sourceMode === "live" && !dailyStatsPartial
      ? (performance?.daily_stats ?? []).map((stat) => ({
          date: stat.date,
          pnl: stat.pnl,
          volume: stat.volume,
          makerRebate: stat.maker_rebate,
          lpRewards: stat.lp_rewards,
          trades: stat.trades,
        }))
      : [],
  };
}
