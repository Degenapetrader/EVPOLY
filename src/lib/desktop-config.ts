import type {
  BotConfig,
  StrategySettings,
  WeekendPolicy,
} from "./tauri-commands";

export const CORE_SYMBOLS = ["BTC", "ETH", "SOL", "XRP"] as const;
export const EXTRA_SYMBOLS = ["DOGE", "BNB", "HYPE"] as const;

export const STRATEGIES = [
  {
    key: "premarket",
    label: "Premarket",
    summary: "Looks for early price moves before the crowd reacts.",
    tooltip: "Early ladder entries before the crowd reacts.",
  },
  {
    key: "endgame",
    label: "Endgame",
    summary: "Waits for late pricing edges before taking the trade.",
    tooltip: "Late sweep entries near open or resolution.",
  },
  {
    key: "evcurve",
    label: "EVCurve",
    summary: "Trades curve-based setups when the price path lines up.",
    tooltip: "Curve-based entries when price and EVPlus checks align.",
  },
  {
    key: "session_band",
    label: "S-Band",
    summary: "Trades late-window S-Band setups with EVPlus checks.",
    tooltip: "Late-window S-Band entries with local sizing and EVPlus checks.",
  },
  {
    key: "evsnipe",
    label: "EVSnipe",
    summary: "Takes quicker entries on hit markets when the setup is clean enough.",
    tooltip: "Pre-hit and confirm-hit entries on discovered hit setups.",
  },
  {
    key: "mm_rewards",
    label: "MM Rewards",
    summary: "Refreshes quotes on rewards markets automatically.",
    tooltip: "Auto-quote reward markets using ranking and filters.",
  },
  {
    key: "mm_sport",
    label: "MM 2.0",
    summary: "Quotes selected reward markets with Alpha risk gating.",
    tooltip: "Quote selected reward markets with pUSD caps and inventory-aware exits.",
  },
] as const;

export const VISIBLE_STRATEGIES = STRATEGIES.filter(
  (strategy) => strategy.key !== "mm_rewards"
);

export type StrategyKey = (typeof STRATEGIES)[number]["key"];
export type StrategyEditorSection = "general" | "risk" | "symbols" | "advanced";
type StrategyMeta = (typeof STRATEGIES)[number];
type Timeframe = "5m" | "15m" | "1h" | "4h" | "1d";

const ALL_TIMEFRAMES: readonly Timeframe[] = ["5m", "15m", "1h", "4h", "1d"] as const;
const PREMARKET_TIMEFRAMES: readonly Timeframe[] = ["5m", "15m", "1h", "4h"] as const;
const SESSIONBAND_TIMEFRAMES: readonly Timeframe[] = ["5m", "15m", "1h", "4h"] as const;

function normalizeMMRewardsMarketMode(value: string | undefined) {
  return value?.trim().toLowerCase() === "hybrid" ? "hybrid" : "auto";
}

function normalizeMMSportQuoteSizeMode(value: string | undefined) {
  return value?.trim().toLowerCase() === "depth_ratio" ? "depth_ratio" : "multiple";
}

function normalizeMMSportEntryPriceMode(value: string | undefined) {
  const normalized = value?.trim().toLowerCase();
  return normalized === "best_bid" || normalized === "best-bid" || normalized === "bestbid"
    ? "best_bid"
    : "passive";
}

function normalizeMMSportDiscoveryRoute(value: string | undefined) {
  const normalized = value?.trim().toLowerCase();
  if (normalized === "nonsports" || normalized === "non-sports" || normalized === "non_sports") {
    return "nonsports";
  }
  if (normalized === "dual") return "dual";
  return "sports";
}

export function mmSportRouteDefaultCaps(value: string | undefined) {
  const route = normalizeMMSportDiscoveryRoute(value);
  if (route === "nonsports") {
    return { active_sport_market_cap: 0, active_nonsport_market_cap: 100 };
  }
  if (route === "dual") {
    return { active_sport_market_cap: 50, active_nonsport_market_cap: 50 };
  }
  return { active_sport_market_cap: 100, active_nonsport_market_cap: 0 };
}

function normalizeNonNegativeInteger(value: number | undefined, fallback: number): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return fallback;
  return Math.floor(value);
}

function normalizeUtcMinute(value: number | undefined, fallback: number): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return fallback;
  return Math.min(1439, Math.floor(value));
}

function normalizeMMSportRouteCaps(
  route: string,
  sportCap: number | undefined,
  nonsportCap: number | undefined
) {
  const defaults = mmSportRouteDefaultCaps(route);
  const sport = normalizeNonNegativeInteger(sportCap, defaults.active_sport_market_cap);
  const nonsport = normalizeNonNegativeInteger(
    nonsportCap,
    defaults.active_nonsport_market_cap
  );
  const savedSportsOnly = sport === 100 && nonsport === 0;
  const savedNonsportsOnly = sport === 0 && nonsport === 100;

  if (
    (route === "dual" && (savedSportsOnly || savedNonsportsOnly)) ||
    (route === "sports" && savedNonsportsOnly) ||
    (route === "nonsports" && savedSportsOnly)
  ) {
    return defaults;
  }

  return { active_sport_market_cap: sport, active_nonsport_market_cap: nonsport };
}

function normalizeCooldownPair(
  minValue: number | undefined,
  maxValue: number | undefined,
  fallbackMin: number,
  fallbackMax: number
) {
  const min = normalizeNonNegativeInteger(minValue, fallbackMin);
  const max = Math.max(normalizeNonNegativeInteger(maxValue, fallbackMax), min);
  return { quote_cooldown_min_sec: min, quote_cooldown_max_sec: max };
}

function normalizeMMSportInventoryExitMode(value: string | undefined) {
  const normalized = value?.trim().toLowerCase();
  if (normalized === "aggressive") return "aggressive";
  if (normalized === "no_exit" || normalized === "no-exit" || normalized === "hold") {
    return "no_exit";
  }
  return "normal";
}

function mmSportUsesNonSportRailProfile(config: BotConfig) {
  return config.strategy_settings.mm_sport.discovery_route === "nonsports";
}

function mmSportUsesDepthRatio(config: BotConfig) {
  const mmSport = config.strategy_settings.mm_sport;
  return mmSportUsesNonSportRailProfile(config)
    ? mmSport.nonsport_quote_size_mode === "depth_ratio"
    : mmSport.quote_size_mode === "depth_ratio";
}

function normalizeWeekendPolicy(value: string | undefined): WeekendPolicy {
  return value?.trim().toLowerCase() === "pause" ? "pause" : "off";
}

export interface DashboardStrategyEditorState {
  selectedStrategy: StrategyKey;
  visibleSections: StrategyEditorSection[];
  dirty: boolean;
  hasActiveProfile: boolean;
}

const DEFAULT_STRATEGY_SETTINGS: StrategySettings = {
  premarket: {
    tp_enabled: true,
    active_cap_per_asset: 100,
    timeframes: ["5m", "15m", "1h", "4h"],
    cancel_after_open_sec: {
      m5: 20,
      m15: 15,
      h1: 60,
      h4: 180,
    },
  },
  endgame: {
    timeframes: ["5m", "15m", "1h", "4h"],
    per_period_cap_usd: 10000,
    tick0_multiplier: 0.2,
    tick1_multiplier: 0.4,
    tick2_multiplier: 0.4,
  },
  evcurve: {
    timeframes: ["15m", "1h", "4h", "1d"],
    min_buy_price: 0.6,
    d1_enabled: true,
    d1_cap_usd: 10000,
  },
  session_band: {
    timeframes: ["5m", "15m", "1h", "4h"],
    flip_threshold_pct: 2,
    tau2_enabled: true,
    tau1_enabled: true,
    tau2_multiplier: 0.3,
    tau1_multiplier: 0.7,
  },
  evsnipe: {
    pre_hit_enabled: true,
    pre_leg_ratio: 0.3,
    saved_pre_leg_ratio: 0.3,
    pre_trigger_bps: 1,
    strike_window_pct: 0.1,
    max_days_to_expiry: 30,
  },
  mm_rewards: {
    market_mode: "auto",
    single_market_slugs: "",
    auto_top_n: 80,
    auto_refresh_sec: 900,
    auto_rank_budget_usd: 2000,
    blacklist_keywords: "",
    reward_min_shares_cap: 0,
  },
  mm_sport: {
    discovery_route: "sports",
    quote_size_mode: "depth_ratio",
    nonsport_quote_size_mode: "depth_ratio",
    entry_price_mode: "passive",
    multiple_collateral_cap_mult: 0.45,
    nonsport_multiple_collateral_cap_mult: 0.45,
    depth_ratio_collateral_cap_mult: 0.9,
    nonsport_depth_ratio_collateral_cap_mult: 0.9,
    min_reward_rate_per_day: 5,
    match_only: true,
    allowed_sport_league_codes: "",
    blocked_sport_league_codes: "",
    blocked_competition_levels: "",
    market_allowlist_keywords: "",
    market_blacklist_keywords: "",
    reward_min_shares_cap: 0,
    polymarket_live_guard_enable: true,
    polymarket_live_guard_ws_enable: true,
    polymarket_live_guard_ws_stale_ms: 600000,
    pause_after_fill_sec: 600,
    inventory_exit_start_hours: 8,
    nonsport_end_exit_start_hours: 48,
    nonsport_entry_schedule_enabled: false,
    nonsport_entry_schedule_days_utc: "mon,tue,wed,thu,fri",
    nonsport_entry_schedule_start_minute_utc: 780,
    nonsport_entry_schedule_end_minute_utc: 240,
    inventory_exit_max_loss_cents: 10,
    inventory_exit_mode: "normal",
    max_share_ratio: 0.05,
    nonsport_max_share_ratio: 0.05,
    max_quote_shares: 0,
    nonsport_max_quote_shares: 0,
    min_top_depth_usd: 1100,
    nonsport_min_top_depth_usd: 1100,
    min_entry_top_bid_price: 0.1,
    allow_sponsored_rewards: true,
    sponsored_reward_min_share: 0.5,
    quote_expiry_min_sec: 65,
    quote_expiry_max_sec: 185,
    quote_cooldown_min_sec: 10,
    quote_cooldown_max_sec: 60,
    fifo_max_share_ratio: 0.2,
    active_sport_market_cap: 100,
    active_nonsport_market_cap: 0,
  },
};

const DEFAULT_SIZE_POLICY = {
  symbol_multipliers: {
    btc: 1.0,
    eth: 0.8,
    sol: 0.5,
    xrp: 0.5,
    doge: 0.5,
    bnb: 0.5,
    hype: 0.5,
  },
  premarket_timeframe_multipliers: {
    m5: 0.75,
    m15: 1.0,
    h1: 1.25,
    h4: 1.25,
    d1: 1.25,
  },
  evcurve_timeframe_multipliers: {
    m15: 0.75,
    h1: 1.0,
    h4: 1.25,
    d1: 1.25,
  },
} satisfies BotConfig["size_policy"];

export const DEFAULT_CONFIG: BotConfig = {
  private_key: "",
  eoa_wallet: "",
  proxy_wallet: "",
  deposit_wallet: "",
  sig_type: 1,
  weekend_policy: "off",
  symbols: ["BTC", "ETH", "SOL", "XRP", "DOGE", "BNB", "HYPE"],
  strategies: {
    premarket: true,
    endgame: true,
    evcurve: true,
    session_band: true,
    evsnipe: true,
    mm_rewards: false,
    mm_sport: false,
  },
  sizing: {
    premarket: 10,
    endgame: 50,
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
    nonsport_quote_size_multiplier: 1.2,
  },
  size_policy: DEFAULT_SIZE_POLICY,
  strategy_settings: DEFAULT_STRATEGY_SETTINGS,
  simulation: false,
  alpha_key: "",
  relayer_api_key: "",
  relayer_api_key_address: "",
  relayer_remote_signer_token: "",
  relayer_submit_signer_url: "",
  wallet_binding: "",
  onboarding_status: "",
  approval_status: "",
  remote_signer_token: "",
  order_signer_primary_token_internal: "",
  remote_discovery_token: "",
  remote_premarket_alpha_token: "",
  remote_endgame_alpha_token: "",
  remote_mm_rewards_alpha_token: "",
  remote_evsnipe_discovery_token: "",
  admin_api_token: "",
};

export function mergeConfig(saved: Partial<BotConfig> | null | undefined): BotConfig {
  const savedMmSport = saved?.strategy_settings?.mm_sport;
  const mmSportDiscoveryRoute = normalizeMMSportDiscoveryRoute(savedMmSport?.discovery_route);
  const mmSportCaps = normalizeMMSportRouteCaps(
    mmSportDiscoveryRoute,
    savedMmSport?.active_sport_market_cap,
    savedMmSport?.active_nonsport_market_cap
  );
  const mmSportCooldown = normalizeCooldownPair(
    savedMmSport?.quote_cooldown_min_sec,
    savedMmSport?.quote_cooldown_max_sec,
    DEFAULT_CONFIG.strategy_settings.mm_sport.quote_cooldown_min_sec,
    DEFAULT_CONFIG.strategy_settings.mm_sport.quote_cooldown_max_sec
  );
  const sportQuoteSizeMode = normalizeMMSportQuoteSizeMode(savedMmSport?.quote_size_mode);
  const sportQuoteSizeMultiplier =
    saved?.mm_tuning?.sport_quote_size_multiplier ??
    DEFAULT_CONFIG.mm_tuning.sport_quote_size_multiplier;
  const sportMultipleCollateralCap =
    savedMmSport?.multiple_collateral_cap_mult ??
    DEFAULT_CONFIG.strategy_settings.mm_sport.multiple_collateral_cap_mult;
  const sportDepthRatioCollateralCap =
    savedMmSport?.depth_ratio_collateral_cap_mult ??
    DEFAULT_CONFIG.strategy_settings.mm_sport.depth_ratio_collateral_cap_mult;
  const sportMaxShareRatio =
    savedMmSport?.max_share_ratio ?? DEFAULT_CONFIG.strategy_settings.mm_sport.max_share_ratio;
  const sportMinTopDepthUsd =
    savedMmSport?.min_top_depth_usd ?? DEFAULT_CONFIG.strategy_settings.mm_sport.min_top_depth_usd;

  return {
    ...DEFAULT_CONFIG,
    ...saved,
    weekend_policy: normalizeWeekendPolicy(saved?.weekend_policy),
    strategies: {
      ...DEFAULT_CONFIG.strategies,
      ...saved?.strategies,
      mm_rewards: false,
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
      sport_quote_size_multiplier: sportQuoteSizeMultiplier,
      nonsport_quote_size_multiplier:
        saved?.mm_tuning?.nonsport_quote_size_multiplier ?? sportQuoteSizeMultiplier,
    },
    size_policy: {
      ...DEFAULT_CONFIG.size_policy,
      ...saved?.size_policy,
      symbol_multipliers: {
        ...DEFAULT_CONFIG.size_policy.symbol_multipliers,
        ...saved?.size_policy?.symbol_multipliers,
      },
      premarket_timeframe_multipliers: {
        ...DEFAULT_CONFIG.size_policy.premarket_timeframe_multipliers,
        ...saved?.size_policy?.premarket_timeframe_multipliers,
      },
      evcurve_timeframe_multipliers: {
        ...DEFAULT_CONFIG.size_policy.evcurve_timeframe_multipliers,
        ...saved?.size_policy?.evcurve_timeframe_multipliers,
      },
    },
    strategy_settings: {
      ...DEFAULT_CONFIG.strategy_settings,
      ...saved?.strategy_settings,
      premarket: {
        ...DEFAULT_CONFIG.strategy_settings.premarket,
        ...saved?.strategy_settings?.premarket,
        timeframes:
          saved?.strategy_settings?.premarket?.timeframes?.length
            ? saved.strategy_settings.premarket.timeframes
            : DEFAULT_CONFIG.strategy_settings.premarket.timeframes,
        cancel_after_open_sec: {
          ...DEFAULT_CONFIG.strategy_settings.premarket.cancel_after_open_sec,
          ...saved?.strategy_settings?.premarket?.cancel_after_open_sec,
        },
      },
      endgame: {
        ...DEFAULT_CONFIG.strategy_settings.endgame,
        ...saved?.strategy_settings?.endgame,
        timeframes:
          saved?.strategy_settings?.endgame?.timeframes?.length
            ? saved.strategy_settings.endgame.timeframes
            : DEFAULT_CONFIG.strategy_settings.endgame.timeframes,
      },
      evcurve: {
        ...DEFAULT_CONFIG.strategy_settings.evcurve,
        ...saved?.strategy_settings?.evcurve,
        timeframes:
          saved?.strategy_settings?.evcurve?.timeframes?.length
            ? saved.strategy_settings.evcurve.timeframes
            : DEFAULT_CONFIG.strategy_settings.evcurve.timeframes,
      },
      session_band: {
        ...DEFAULT_CONFIG.strategy_settings.session_band,
        ...saved?.strategy_settings?.session_band,
        timeframes:
          saved?.strategy_settings?.session_band?.timeframes?.length
            ? saved.strategy_settings.session_band.timeframes
            : DEFAULT_CONFIG.strategy_settings.session_band.timeframes,
      },
      evsnipe: (() => {
        const merged = {
          ...DEFAULT_CONFIG.strategy_settings.evsnipe,
          ...saved?.strategy_settings?.evsnipe,
        };
        const savedRatio =
          typeof merged.saved_pre_leg_ratio === "number" && merged.saved_pre_leg_ratio > 0
            ? merged.saved_pre_leg_ratio
            : typeof merged.pre_leg_ratio === "number" && merged.pre_leg_ratio > 0
              ? merged.pre_leg_ratio
              : DEFAULT_CONFIG.strategy_settings.evsnipe.saved_pre_leg_ratio;
        const preHitEnabled =
          saved?.strategy_settings?.evsnipe?.pre_hit_enabled ??
          (saved?.strategy_settings?.evsnipe?.pre_leg_ratio ?? merged.pre_leg_ratio) > 0;
        return {
          ...merged,
          pre_hit_enabled: preHitEnabled,
          saved_pre_leg_ratio: savedRatio,
          pre_leg_ratio: preHitEnabled
            ? merged.pre_leg_ratio > 0
              ? merged.pre_leg_ratio
              : savedRatio
            : savedRatio,
        };
      })(),
      mm_rewards: {
        ...DEFAULT_CONFIG.strategy_settings.mm_rewards,
        ...saved?.strategy_settings?.mm_rewards,
        market_mode: normalizeMMRewardsMarketMode(saved?.strategy_settings?.mm_rewards?.market_mode),
      },
      mm_sport: {
        ...DEFAULT_CONFIG.strategy_settings.mm_sport,
        ...savedMmSport,
        discovery_route: mmSportDiscoveryRoute,
        quote_size_mode: sportQuoteSizeMode,
        nonsport_quote_size_mode: normalizeMMSportQuoteSizeMode(
          savedMmSport?.nonsport_quote_size_mode ?? sportQuoteSizeMode
        ),
        entry_price_mode: normalizeMMSportEntryPriceMode(savedMmSport?.entry_price_mode),
        multiple_collateral_cap_mult: sportMultipleCollateralCap,
        nonsport_multiple_collateral_cap_mult:
          savedMmSport?.nonsport_multiple_collateral_cap_mult ?? sportMultipleCollateralCap,
        depth_ratio_collateral_cap_mult: sportDepthRatioCollateralCap,
        nonsport_depth_ratio_collateral_cap_mult:
          savedMmSport?.nonsport_depth_ratio_collateral_cap_mult ?? sportDepthRatioCollateralCap,
        inventory_exit_mode: normalizeMMSportInventoryExitMode(
          savedMmSport?.inventory_exit_mode
        ),
        max_share_ratio: sportMaxShareRatio,
        nonsport_max_share_ratio: savedMmSport?.nonsport_max_share_ratio ?? sportMaxShareRatio,
        max_quote_shares: savedMmSport?.max_quote_shares ?? 0,
        nonsport_max_quote_shares:
          savedMmSport?.nonsport_max_quote_shares ?? savedMmSport?.max_quote_shares ?? 0,
        min_top_depth_usd: sportMinTopDepthUsd,
        nonsport_min_top_depth_usd:
          savedMmSport?.nonsport_min_top_depth_usd ?? sportMinTopDepthUsd,
        quote_cooldown_min_sec: mmSportCooldown.quote_cooldown_min_sec,
        quote_cooldown_max_sec: mmSportCooldown.quote_cooldown_max_sec,
        nonsport_entry_schedule_days_utc:
          savedMmSport?.nonsport_entry_schedule_days_utc ??
          DEFAULT_CONFIG.strategy_settings.mm_sport.nonsport_entry_schedule_days_utc,
        nonsport_entry_schedule_start_minute_utc: normalizeUtcMinute(
          savedMmSport?.nonsport_entry_schedule_start_minute_utc,
          DEFAULT_CONFIG.strategy_settings.mm_sport.nonsport_entry_schedule_start_minute_utc
        ),
        nonsport_entry_schedule_end_minute_utc: normalizeUtcMinute(
          savedMmSport?.nonsport_entry_schedule_end_minute_utc,
          DEFAULT_CONFIG.strategy_settings.mm_sport.nonsport_entry_schedule_end_minute_utc
        ),
        active_sport_market_cap: mmSportCaps.active_sport_market_cap,
        active_nonsport_market_cap: mmSportCaps.active_nonsport_market_cap,
      },
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
  if (strategy === "mm_sport" && config.strategy_settings.mm_sport.discovery_route === "dual") {
    return config;
  }
  if (strategy === "mm_sport" && mmSportUsesNonSportRailProfile(config)) {
    if (mmSportUsesDepthRatio(config)) {
      return {
        ...config,
        strategy_settings: {
          ...config.strategy_settings,
          mm_sport: {
            ...config.strategy_settings.mm_sport,
            nonsport_max_share_ratio: value,
          },
        },
      };
    }
    return {
      ...config,
      mm_tuning: {
        ...config.mm_tuning,
        nonsport_quote_size_multiplier: value,
      },
    };
  }
  if (mmSportUsesDepthRatio(config)) {
    return {
      ...config,
      strategy_settings: {
        ...config.strategy_settings,
        mm_sport: {
          ...config.strategy_settings.mm_sport,
          max_share_ratio: value,
        },
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

export function updateStrategySettingsSection<K extends keyof StrategySettings>(
  config: BotConfig,
  section: K,
  value: StrategySettings[K]
): BotConfig {
  return {
    ...config,
    strategy_settings: {
      ...config.strategy_settings,
      [section]: value,
    },
  };
}

export function updateStrategySymbols(
  config: BotConfig,
  strategy: StrategyKey,
  symbol: string,
  enabled: boolean
): BotConfig {
  if (!strategySupportsSymbols(strategy)) return config;
  const allowed = symbolSetForStrategy(strategy);
  if (!allowed.includes(symbol)) return config;

  const next = enabled
    ? [...new Set([...config.symbols, symbol])]
    : config.symbols.filter((item) => item !== symbol || item === "BTC");

  return {
    ...config,
    symbols: normalizeSymbols(next),
  };
}

export function setEVSnipePreHitEnabled(config: BotConfig, enabled: boolean): BotConfig {
  const current = config.strategy_settings.evsnipe;
  const rememberedRatio =
    current.pre_leg_ratio > 0 ? current.pre_leg_ratio : current.saved_pre_leg_ratio;
  return updateStrategySettingsSection(config, "evsnipe", {
    ...current,
    pre_hit_enabled: enabled,
    saved_pre_leg_ratio: rememberedRatio > 0 ? rememberedRatio : 0.3,
    pre_leg_ratio: enabled ? rememberedRatio || 0.3 : rememberedRatio || 0.3,
  });
}

export function setEVSnipePreLegRatio(config: BotConfig, value: number): BotConfig {
  const next = value > 0 ? value : config.strategy_settings.evsnipe.saved_pre_leg_ratio || 0.3;
  return updateStrategySettingsSection(config, "evsnipe", {
    ...config.strategy_settings.evsnipe,
    pre_leg_ratio: next,
    saved_pre_leg_ratio: next,
    pre_hit_enabled: config.strategy_settings.evsnipe.pre_hit_enabled,
  });
}

export function normalizeSymbols(symbols: string[]): string[] {
  const normalized = symbols.map((symbol) => symbol.trim().toUpperCase());
  const allowed = normalized.filter((symbol) =>
    [...CORE_SYMBOLS, ...EXTRA_SYMBOLS].includes(symbol as (typeof CORE_SYMBOLS)[number])
  );
  if (!allowed.includes("BTC")) {
    allowed.unshift("BTC");
  }
  return [...CORE_SYMBOLS, ...EXTRA_SYMBOLS].filter((symbol) => allowed.includes(symbol));
}

export function strategyAllowsExtraSymbols(strategy: StrategyKey): boolean {
  return strategy === "endgame" || strategy === "evsnipe";
}

export function strategySupportsSymbols(strategy: StrategyKey): boolean {
  return strategy !== "mm_rewards" && strategy !== "mm_sport";
}

export function symbolSetForStrategy(strategy: StrategyKey): string[] {
  return strategyAllowsExtraSymbols(strategy)
    ? [...CORE_SYMBOLS, ...EXTRA_SYMBOLS]
    : [...CORE_SYMBOLS];
}

export function strategySections(strategy: StrategyKey): StrategyEditorSection[] {
  if (strategy === "mm_rewards" || strategy === "mm_sport") {
    return ["general", "advanced"];
  }
  if (strategy === "evsnipe") {
    return ["general", "symbols", "advanced"];
  }
  return ["general", "risk", "symbols", "advanced"];
}

export function strategySizeLabel(strategy: StrategyKey, config?: BotConfig): string {
  if (strategy === "evsnipe") return "Size Per Hit (pUSD)";
  if (strategy === "mm_rewards") return "Min Share Multiple";
  if (strategy === "endgame") return "Base Size (Shares)";
  if (strategy === "mm_sport") {
    return config && mmSportUsesDepthRatio(config) ? "Max Share Ratio" : "Quote Size Multiplier";
  }
  return "Base Size (pUSD)";
}

export function strategySizeValue(config: BotConfig, strategy: StrategyKey): number {
  if (strategy === "premarket") return config.sizing.premarket;
  if (strategy === "endgame") return config.sizing.endgame;
  if (strategy === "evcurve") return config.sizing.evcurve;
  if (strategy === "session_band") return config.sizing.session_band;
  if (strategy === "evsnipe") return config.sizing.evsnipe_per_hit;
  if (strategy === "mm_rewards") return config.mm_tuning.rewards_min_share_multiple;
  if (strategy === "mm_sport" && mmSportUsesNonSportRailProfile(config)) {
    if (mmSportUsesDepthRatio(config)) {
      return config.strategy_settings.mm_sport.nonsport_max_share_ratio;
    }
    return config.mm_tuning.nonsport_quote_size_multiplier;
  }
  if (mmSportUsesDepthRatio(config)) return config.strategy_settings.mm_sport.max_share_ratio;
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
  return strategyMeta(strategy)?.summary ?? "Strategy";
}

export function strategyLabel(strategy: StrategyKey): string {
  return strategyMeta(strategy)?.label ?? "Strategy";
}

export function strategyTooltip(strategy: StrategyKey): string {
  return strategyMeta(strategy)?.tooltip ?? "Strategy behavior";
}

export function strategyControlSuffix(strategy: StrategyKey, config?: BotConfig): string {
  switch (strategy) {
    case "evsnipe":
      return "/HIT";
    case "mm_rewards":
      return "x";
    case "mm_sport":
      return config && mmSportUsesDepthRatio(config) ? "DEPTH" : "MULT";
    case "endgame":
      return "SHARE";
    default:
      return "pUSD";
  }
}

export function strategyControlTooltip(config: BotConfig, strategy: StrategyKey): string {
  switch (strategy) {
    case "premarket":
      return "Base pUSD budget for Premarket entries.";
    case "endgame":
      return "Base share size for Endgame.";
    case "evcurve":
      return "Base pUSD budget for each EVCurve setup.";
    case "session_band":
      return "Base pUSD budget for S-Band entries.";
    case "evsnipe":
      return "Size used for each EVSnipe hit leg.";
    case "mm_rewards":
      return "Multiplier applied to the reward minimum shares target.";
    case "mm_sport":
      return mmSportUsesDepthRatio(config)
        ? "Depth Ratio mode sizes from visible depth and pUSD collateral."
        : "Multiplier applied to MM 2.0 quote size.";
    default:
      return "Strategy control";
  }
}

export function strategyTimeframeOptions(strategy: StrategyKey): readonly Timeframe[] {
  switch (strategy) {
    case "premarket":
      return PREMARKET_TIMEFRAMES;
    case "session_band":
      return SESSIONBAND_TIMEFRAMES;
    case "evcurve":
      return ["15m", "1h", "4h", "1d"];
    case "endgame":
      return ["5m", "15m", "1h", "4h"];
    default:
      return ALL_TIMEFRAMES;
  }
}

export function adjustStrategySize(
  config: BotConfig,
  strategy: StrategyKey,
  delta: number
): BotConfig {
  const nextValue = Math.max(0, strategySizeValue(config, strategy) + delta);
  return updateStrategySize(config, strategy, nextValue);
}

function strategyMeta(strategy: StrategyKey): StrategyMeta | undefined {
  return STRATEGIES.find((item) => item.key === strategy);
}
