import type { BotConfig } from "./tauri-commands";

export const CORE_SYMBOLS = ["BTC", "ETH", "SOL", "XRP"] as const;
export const EXTRA_SYMBOLS = ["DOGE", "BNB", "HYPE"] as const;

export const STRATEGIES = [
  {
    key: "premarket",
    label: "Premarket",
    summary: "Looks for early price moves before the crowd reacts.",
  },
  {
    key: "endgame",
    label: "Endgame",
    summary: "Waits for late pricing edges before taking the trade.",
  },
  {
    key: "evcurve",
    label: "EVCurve",
    summary: "Trades curve-based setups when the price path lines up.",
  },
  {
    key: "session_band",
    label: "SessionBand",
    summary: "Looks for session swings and reversal bands.",
  },
  {
    key: "evsnipe",
    label: "EVSnipe",
    summary: "Takes quicker entries when the setup is clean enough.",
  },
  {
    key: "mm_rewards",
    label: "MM Rewards",
    summary: "Refreshes quotes on rewards markets automatically.",
  },
  {
    key: "mm_sport",
    label: "MM Sport",
    summary: "Quotes sports markets when extra activity is enabled.",
  },
] as const;

export type StrategyKey = (typeof STRATEGIES)[number]["key"];
export type StrategyEditorSection = "general" | "risk" | "symbols" | "advanced";

export interface DashboardStrategyEditorState {
  selectedStrategy: StrategyKey;
  visibleSections: StrategyEditorSection[];
  dirty: boolean;
  hasActiveProfile: boolean;
}

export const DEFAULT_CONFIG: BotConfig = {
  private_key: "",
  eoa_wallet: "",
  proxy_wallet: "",
  sig_type: 1,
  symbols: ["BTC", "ETH", "SOL", "XRP", "DOGE", "BNB", "HYPE"],
  strategies: {
    premarket: true,
    endgame: true,
    evcurve: true,
    session_band: false,
    evsnipe: true,
    mm_rewards: false,
    mm_sport: false,
  },
  sizing: {
    premarket: 10,
    endgame: 10,
    evcurve: 10,
    session_band: 10,
    evsnipe_per_hit: 10,
  },
  caps: {
    premarket: 100000,
    endgame: 100000,
    evcurve: 100000,
    session_band: 100000,
    evsnipe: 100000,
  },
  mm_tuning: {
    rewards_min_share_multiple: 1.0,
    sport_quote_size_multiplier: 1.2,
  },
  simulation: false,
  relayer_api_key: "",
  relayer_api_key_address: "",
  remote_signer_token: "",
  remote_discovery_token: "",
  remote_premarket_alpha_token: "",
  remote_endgame_alpha_token: "",
  remote_mm_rewards_alpha_token: "",
  remote_evsnipe_discovery_token: "",
  admin_api_token: "",
};

export function mergeConfig(saved: Partial<BotConfig> | null | undefined): BotConfig {
  return {
    ...DEFAULT_CONFIG,
    ...saved,
    strategies: {
      ...DEFAULT_CONFIG.strategies,
      ...saved?.strategies,
    },
    sizing: {
      ...DEFAULT_CONFIG.sizing,
      ...saved?.sizing,
    },
    caps: {
      ...DEFAULT_CONFIG.caps,
      ...saved?.caps,
    },
    mm_tuning: {
      ...DEFAULT_CONFIG.mm_tuning,
      ...saved?.mm_tuning,
    },
    symbols: saved?.symbols?.length ? saved.symbols : DEFAULT_CONFIG.symbols,
  };
}

export function parseNonNegative(value: string, fallback: number): number {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 0) return fallback;
  return parsed;
}

export function formatUsd(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value)) return "--";
  return new Intl.NumberFormat("en-US", {
    style: "currency",
    currency: "USD",
    maximumFractionDigits: 2,
  }).format(value);
}

export function formatMaybeTime(value: string | null | undefined): string {
  if (!value) return "Not yet";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

export function enabledStrategyCount(config: BotConfig): number {
  return Object.values(config.strategies).filter(Boolean).length;
}

export function updateStrategyEnabled(
  config: BotConfig,
  strategy: StrategyKey,
  enabled: boolean
): BotConfig {
  return {
    ...config,
    strategies: {
      ...config.strategies,
      [strategy]: enabled,
    },
  };
}

export function updateStrategySize(
  config: BotConfig,
  strategy: StrategyKey,
  value: number
): BotConfig {
  if (strategy === "premarket") {
    return { ...config, sizing: { ...config.sizing, premarket: value } };
  }
  if (strategy === "endgame") {
    return { ...config, sizing: { ...config.sizing, endgame: value } };
  }
  if (strategy === "evcurve") {
    return { ...config, sizing: { ...config.sizing, evcurve: value } };
  }
  if (strategy === "session_band") {
    return { ...config, sizing: { ...config.sizing, session_band: value } };
  }
  if (strategy === "evsnipe") {
    return { ...config, sizing: { ...config.sizing, evsnipe_per_hit: value } };
  }
  if (strategy === "mm_rewards") {
    return {
      ...config,
      mm_tuning: {
        ...config.mm_tuning,
        rewards_min_share_multiple: value,
      },
    };
  }
  return {
    ...config,
    mm_tuning: {
      ...config.mm_tuning,
      sport_quote_size_multiplier: value,
    },
  };
}

export function updateStrategyCap(
  config: BotConfig,
  strategy: StrategyKey,
  value: number
): BotConfig {
  if (strategy === "premarket") {
    return { ...config, caps: { ...config.caps, premarket: value } };
  }
  if (strategy === "endgame") {
    return { ...config, caps: { ...config.caps, endgame: value } };
  }
  if (strategy === "evcurve") {
    return { ...config, caps: { ...config.caps, evcurve: value } };
  }
  if (strategy === "session_band") {
    return { ...config, caps: { ...config.caps, session_band: value } };
  }
  if (strategy === "evsnipe") {
    return { ...config, caps: { ...config.caps, evsnipe: value } };
  }
  return config;
}

export function strategySupportsSymbols(strategy: StrategyKey): boolean {
  return strategy !== "mm_rewards" && strategy !== "mm_sport";
}

export function strategySections(strategy: StrategyKey): StrategyEditorSection[] {
  if (strategy === "mm_rewards" || strategy === "mm_sport") {
    return ["general", "advanced"];
  }
  return ["general", "risk", "symbols"];
}

export function strategySizeLabel(strategy: StrategyKey): string {
  if (strategy === "evsnipe") return "Size per hit (USD)";
  if (strategy === "mm_rewards") return "Min share multiple";
  if (strategy === "mm_sport") return "Quote size multiplier";
  return "Base size (USD)";
}

export function strategySizeValue(config: BotConfig, strategy: StrategyKey): number {
  if (strategy === "premarket") return config.sizing.premarket;
  if (strategy === "endgame") return config.sizing.endgame;
  if (strategy === "evcurve") return config.sizing.evcurve;
  if (strategy === "session_band") return config.sizing.session_band;
  if (strategy === "evsnipe") return config.sizing.evsnipe_per_hit;
  if (strategy === "mm_rewards") return config.mm_tuning.rewards_min_share_multiple;
  return config.mm_tuning.sport_quote_size_multiplier;
}

export function strategyCapValue(config: BotConfig, strategy: StrategyKey): number | null {
  if (strategy === "premarket") return config.caps.premarket;
  if (strategy === "endgame") return config.caps.endgame;
  if (strategy === "evcurve") return config.caps.evcurve;
  if (strategy === "session_band") return config.caps.session_band;
  if (strategy === "evsnipe") return config.caps.evsnipe;
  return null;
}

export function strategySummary(strategy: StrategyKey): string {
  return STRATEGIES.find((item) => item.key === strategy)?.summary ?? "Strategy";
}

export function strategyLabel(strategy: StrategyKey): string {
  return STRATEGIES.find((item) => item.key === strategy)?.label ?? "Strategy";
}

export function adjustStrategySize(
  config: BotConfig,
  strategy: StrategyKey,
  delta: number
): BotConfig {
  const nextValue = Math.max(0, strategySizeValue(config, strategy) + delta);
  return updateStrategySize(config, strategy, nextValue);
}
