import {
  type BotConfig,
  type Position,
  type Trade,
  type TradeStats,
  type UiDashboardSummary,
  type UiStrategyState,
} from "./tauri-commands";

export type StrategyKey = keyof BotConfig["strategies"];

export const STRATEGY_META: Record<
  StrategyKey,
  { label: string; summary: string }
> = {
  premarket: {
    label: "Premarket",
    summary: "Looks for early price moves before the crowd reacts.",
  },
  endgame: {
    label: "Endgame",
    summary: "Waits for late pricing edges before taking the trade.",
  },
  evcurve: {
    label: "EVCurve",
    summary: "Trades curve-based setups when the price path lines up.",
  },
  session_band: {
    label: "SessionBand",
    summary: "Looks for session swings and band reversals.",
  },
  evsnipe: {
    label: "EVSnipe",
    summary: "Takes faster entries only when the setup is clean enough.",
  },
  mm_rewards: {
    label: "MM Rewards",
    summary: "Refreshes quotes on reward markets automatically.",
  },
  mm_sport: {
    label: "MM Sport",
    summary: "Quotes sports reward markets when enabled.",
  },
};

export function formatLatency(ms: number | null | undefined): string {
  if (typeof ms !== "number" || !Number.isFinite(ms) || ms < 0) {
    return "--";
  }
  if (ms < 1000) {
    return `${Math.round(ms)}ms`;
  }
  if (ms < 10_000) {
    return `${(ms / 1000).toFixed(2)}s`;
  }
  return `${(ms / 1000).toFixed(1)}s`;
}

export function formatCurrency(value: number): string {
  const sign = value < 0 ? "-" : "";
  return `${sign}$${Math.abs(value).toFixed(2)}`;
}

export function formatClock(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

export function formatQuantity(value: number): string {
  if (!Number.isFinite(value)) return "--";
  if (Math.abs(value) >= 100) return value.toFixed(0);
  if (Math.abs(value) >= 1) return value.toFixed(2);
  return value.toFixed(4);
}

export function summarizeSymbols(symbols: string[]): string {
  if (symbols.length === 0) return "Selected markets";
  if (symbols.length <= 4) return symbols.join(" / ");
  return `${symbols.slice(0, 4).join(" / ")} +${symbols.length - 4}`;
}

export function describePositionPrices(position: Position): string {
  const currentPrice =
    typeof position.current_price === "number"
      ? formatCurrency(position.current_price)
      : "Not available";
  return `Entry ${formatCurrency(position.entry_price)} / Current ${currentPrice}`;
}

export function describeTradeFill(trade: Trade): string {
  return `${trade.side.toUpperCase()} / ${formatQuantity(trade.size)} @ ${formatCurrency(
    trade.price
  )}`;
}

export function sentenceCase(value: string | undefined, fallback: string): string {
  if (!value) return fallback;
  return value
    .replace(/_/g, " ")
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

export function formatUsd(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return "--";
  return `$${value.toFixed(2)}`;
}

export function formatShares(value: unknown): string {
  if (typeof value === "number" && Number.isFinite(value)) {
    if (Math.abs(value) >= 100) return value.toFixed(0);
    if (Math.abs(value) >= 1) return value.toFixed(2);
    return value.toFixed(4);
  }
  return "--";
}

export function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  return value as Record<string, unknown>;
}

export function readString(value: unknown, keys: string[]): string | null {
  const record = asRecord(value);
  if (!record) return null;
  for (const key of keys) {
    const candidate = record[key];
    if (typeof candidate === "string" && candidate.trim()) {
      return candidate.trim();
    }
  }
  return null;
}

export function readNumber(value: unknown, keys: string[]): number | null {
  const record = asRecord(value);
  if (!record) return null;
  for (const key of keys) {
    const candidate = record[key];
    if (typeof candidate === "number" && Number.isFinite(candidate)) {
      return candidate;
    }
    if (typeof candidate === "string" && candidate.trim()) {
      const parsed = Number(candidate);
      if (Number.isFinite(parsed)) {
        return parsed;
      }
    }
  }
  return null;
}

export function countItems(value: unknown, keys: string[]): number {
  if (Array.isArray(value)) return value.length;
  const record = asRecord(value);
  if (!record) return 0;
  for (const key of keys) {
    const candidate = record[key];
    if (Array.isArray(candidate)) {
      return candidate.length;
    }
    if (typeof candidate === "number" && Number.isFinite(candidate)) {
      return candidate;
    }
  }
  return 0;
}

function describeBotActivity({
  isRunning,
  displayError,
  positionsCount,
  tradeCount,
  simulation,
}: {
  isRunning: boolean;
  displayError: string | null;
  positionsCount: number;
  tradeCount: number;
  simulation: boolean;
}) {
  if (displayError) {
    return {
      tone: "danger" as const,
      eyebrow: "Needs attention",
      headline: "Something stopped the flow",
      detail:
        "Check the message below, then open logs only if you need the technical details.",
    };
  }

  if (!isRunning) {
    return {
      tone: "neutral" as const,
      eyebrow: "Stopped",
      headline: "The bot is not trading right now",
      detail: "Press Start when you want EVPoly to begin watching and trading again.",
    };
  }

  if (positionsCount > 0) {
    return {
      tone: simulation ? ("warning" as const) : ("success" as const),
      eyebrow: simulation ? "Dry run" : "Active trading",
      headline: `Managing ${positionsCount} open ${positionsCount === 1 ? "position" : "positions"}`,
      detail:
        "EVPoly is already in the market and will keep monitoring open trades and fresh opportunities.",
    };
  }

  if (tradeCount > 0) {
    return {
      tone: simulation ? ("warning" as const) : ("success" as const),
      eyebrow: simulation ? "Dry run" : "Orders placed",
      headline: "Recent orders have been placed",
      detail:
        "The bot is running and has traded recently. It may now be waiting for the next clean setup.",
    };
  }

  return {
    tone: simulation ? ("warning" as const) : ("accent" as const),
    eyebrow: simulation ? "Dry run" : "Watching",
    headline: "The bot is waiting for a better market",
    detail:
      "Nothing is wrong. EVPoly is on and ready, but it has not found a trade worth taking yet.",
  };
}

function describeBotActivityFromSummary(
  summary: UiDashboardSummary,
  displayError: string | null
) {
  if (displayError) {
    return describeBotActivity({
      isRunning: summary.bot_state === "running",
      displayError,
      positionsCount: summary.open_positions_count,
      tradeCount: summary.recent_orders_count,
      simulation: summary.mode === "dry_run",
    });
  }

  const simulation = summary.mode === "dry_run";
  const eyebrow =
    summary.bot_state === "running"
      ? simulation
        ? "Dry run"
        : summary.open_positions_count > 0 || summary.recent_orders_count > 0
        ? "Active trading"
        : "Watching"
      : sentenceCase(summary.bot_state, "Stopped");

  const tone: "neutral" | "warning" | "success" | "danger" | "accent" =
    summary.blocker_reason
      ? "warning"
      : summary.bot_state !== "running"
      ? "neutral"
      : simulation
      ? "warning"
      : summary.open_positions_count > 0 || summary.recent_orders_count > 0
      ? "success"
      : "accent";

  return {
    tone,
    eyebrow,
    headline: summary.headline || "Watching the market",
    detail: summary.detail || "EVPoly is ready and waiting for a better setup.",
  };
}

function strategyToneFromState(
  state: string,
  enabled: boolean
): "neutral" | "warning" | "success" | "danger" | "accent" {
  if (!enabled || state === "disabled") return "neutral";
  if (state === "error") return "danger";
  if (state === "blocked") return "warning";
  if (state === "running") return "success";
  if (state === "watching") return "accent";
  return "neutral";
}

function strategyStateLabel(state: string, enabled: boolean): string {
  if (!enabled) return "Disabled";
  return sentenceCase(state, "Ready");
}

function looksTechnicalRecentResult(value: string): boolean {
  return (
    /0x[a-f0-9]{12,}/i.test(value) ||
    /\b(BUY|SELL)\s+\d{16,}\b/.test(value) ||
    /\b\d{24,}\b/.test(value)
  );
}

function humanizeTradeResult(trade: Trade | null): string | null {
  if (!trade) return null;
  return `Recent order: ${sentenceCase(trade.side, "Trade")} ${trade.market}`;
}

export function buildDashboardViewModel({
  isRunning,
  displayError,
  positions,
  trades,
  simulation,
  savedConfig,
  stats,
  uiSummary,
  uiStrategies,
}: {
  isRunning: boolean;
  displayError: string | null;
  positions: Position[];
  trades: Trade[];
  simulation: boolean;
  savedConfig: BotConfig | null;
  stats: TradeStats | null;
  uiSummary?: UiDashboardSummary | null;
  uiStrategies?: UiStrategyState[] | null;
}) {
  const enabledStrategyKeys = savedConfig
    ? (Object.entries(savedConfig.strategies).filter(([, enabled]) => enabled) as [
        StrategyKey,
        boolean,
      ][]).map(([key]) => key)
    : [];

  const activity = uiSummary
    ? describeBotActivityFromSummary(uiSummary, displayError)
    : describeBotActivity({
        isRunning,
        displayError,
        positionsCount: positions.length,
        tradeCount: trades.length,
        simulation,
      });

  const latestTrade = trades[0] ?? null;
  const preferredSummaryResult =
    uiSummary?.recent_result && !looksTechnicalRecentResult(uiSummary.recent_result)
      ? uiSummary.recent_result
      : null;
  const recentResult =
    preferredSummaryResult ||
    humanizeTradeResult(latestTrade) ||
    (positions.length > 0
      ? `Currently managing ${positions.length} ${positions.length === 1 ? "position" : "positions"}`
      : isRunning
      ? "Watching for the next clean setup"
      : "Bot is stopped");

  const idleHelp = !isRunning
    ? "Start the bot when you want to trade."
    : uiSummary?.blocker_reason
    ? uiSummary.blocker_reason
    : enabledStrategyKeys.length === 0
    ? "Turn on a strategy in Settings first."
    : uiSummary?.detail || "EVPoly is waiting for a setup that meets your rules.";

  const enabledStrategies =
    uiStrategies && uiStrategies.length > 0
      ? uiStrategies
          .filter((strategy) => strategy.enabled)
          .map((strategy) => ({
            key: strategy.slug as StrategyKey,
            label: strategy.label,
            summary: strategy.summary,
            stateTone: strategyToneFromState(strategy.state, strategy.enabled),
            stateLabel: strategyStateLabel(strategy.state, strategy.enabled),
            scopeSummary: strategy.scope_summary,
            blockerReason: strategy.blocker_reason,
          }))
      : enabledStrategyKeys.map((key) => {
          const stateTone: "neutral" | "warning" | "success" = !isRunning
            ? "neutral"
            : simulation
            ? "warning"
            : "success";

          return {
            key,
            label: STRATEGY_META[key].label,
            summary: STRATEGY_META[key].summary,
            stateTone,
            stateLabel: !isRunning ? "Ready" : simulation ? "Watching" : "Running",
            scopeSummary:
              key === "mm_rewards"
                ? "Reward markets chosen automatically"
                : savedConfig
                ? summarizeSymbols(savedConfig.symbols)
                : "Selected markets",
            blockerReason: null,
          };
        });

  return {
    activity,
    latestTrade,
    recentResult,
    idleHelp,
    activeScope: savedConfig ? summarizeSymbols(savedConfig.symbols) : "Selected markets",
    avgAckLatency: uiSummary
      ? formatLatency(uiSummary.avg_ack_latency_ms)
      : stats?.ack_sample_count
      ? formatLatency(stats.avg_ack_latency_ms)
      : "--",
    pnlValue: uiSummary?.total_pnl ?? stats?.total_pnl ?? 0,
    openPositionsCount: uiSummary?.open_positions_count ?? positions.length,
    recentOrdersCount: uiSummary?.recent_orders_count ?? trades.length,
    freeBalanceValue: uiSummary?.free_balance ?? null,
    enabledStrategies,
  };
}

