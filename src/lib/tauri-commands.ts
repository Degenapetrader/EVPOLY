import { invoke } from "@tauri-apps/api/core";

export interface Profile {
  id: string;
  name: string;
  eoa_wallet_address: string;
  proxy_wallet_address: string;
  wallet_address: string;
  signature_type: number;
  created_at: string;
}

export interface LogLine {
  timestamp: string;
  level: "INFO" | "WARN" | "ERROR";
  content: string;
}

export interface LogTailBatch {
  next_cursor: number;
  reset: boolean;
  lines: LogLine[];
}

export interface UiDashboardSummary {
  bot_state: string;
  mode: "live" | "dry_run" | string;
  headline: string;
  detail: string;
  last_activity_at_ms: number | null;
  last_activity_at: string | null;
  recent_result: string | null;
  blocker_reason: string | null;
  enabled_strategies: string[];
  open_positions_count: number;
  recent_orders_count: number;
  free_balance: number | null;
  avg_ack_latency_ms: number | null;
  total_pnl: number;
  total_trades: number;
  winning_trades: number;
  losing_trades: number;
}

export interface WalletSyncStatus {
  state: string;
  managed: boolean;
  wallet_address: string | null;
  last_run_at: string | null;
  last_run_at_ms: number | null;
  last_result: string | null;
  error: string | null;
  interval_sec: number;
}

export interface GeoAccessStatus {
  status: "allowed" | "blocked" | "unknown" | string;
  country_code: string | null;
  country_name: string | null;
  region_name: string | null;
  reason: string;
  checked_at: string;
}

export interface HomeOverview {
  profile_ready: boolean;
  portfolio_value: number | null;
  available_balance: number | null;
  total_equity: number | null;
  pnl_today_utc: number;
  liquidity_rewards_today: number | null;
  liquidity_rewards_lifetime: number | null;
  liquidity_rewards_as_of_utc: string | null;
  liquidity_rewards_error: string | null;
  bot_state: string;
  mode: "live" | "dry_run" | string;
  active_strategy_count: number;
  wallet_sync: WalletSyncStatus;
  wallet_sync_status: string;
  wallet_sync_last_run_at: string | null;
  wallet_sync_last_run_at_ms: number | null;
  last_heartbeat_at: string | null;
  last_heartbeat_at_ms: number | null;
  available_balance_error: string | null;
  portfolio_value_error: string | null;
  ack_warning_count_recent: number;
  avg_ack_latency_ms: number | null;
  ack_sample_count: number;
  warnings: string[];
}

export interface HomeActivityItem {
  timestamp: string;
  severity: "info" | "warning" | "error" | string;
  source: string;
  kind: string;
  message: string;
  action?: string | null;
  thumbnail_url?: string | null;
  market_title?: string | null;
  title?: string | null;
  outcome?: string | null;
  detail?: string | null;
  quantity?: number | null;
  cashflow_usd?: number | null;
  value_usd?: number | null;
}

export interface HomeActivityBatch {
  next_cursor: number;
  reset: boolean;
  items: HomeActivityItem[];
}

export interface HomeApiActivityItem {
  timestamp: string;
  action: string | null;
  message: string;
  market_title: string | null;
  title: string | null;
  outcome: string | null;
  quantity: number | null;
  cashflow_usd: number | null;
  thumbnail_url: string | null;
  detail: string | null;
  condition_id: string | null;
  token_id: string | null;
  activity_type: string | null;
  side: string | null;
  transaction_hash: string | null;
}

export interface HomeApiPositionItem {
  condition_id: string | null;
  token_id: string | null;
  market_title: string | null;
  market_slug: string | null;
  thumbnail_url: string | null;
  event_slug: string | null;
  outcome: string | null;
  opposite_outcome: string | null;
  size: number | null;
  avg_price: number | null;
  current_price: number | null;
  initial_value: number | null;
  current_value: number | null;
  cash_pnl: number | null;
  percent_pnl: number | null;
  realized_pnl: number | null;
  total_bought: number | null;
  redeemable: boolean | null;
  mergeable: boolean | null;
  end_date: string | null;
}

export interface HomeApiOpenOrderItem {
  id: string;
  status: string | null;
  condition_id: string | null;
  token_id: string | null;
  market_title: string | null;
  thumbnail_url: string | null;
  outcome: string | null;
  side: string | null;
  price: number | null;
  original_size: number | null;
  size_matched: number | null;
  remaining_size: number | null;
  total_notional_usd: number | null;
  created_at: string | null;
  expiration: string | null;
  order_type: string | null;
}

export interface UiStrategyState {
  strategy_id: string;
  slug: string;
  label: string;
  enabled: boolean;
  state: string;
  summary: string;
  scope_summary: string;
  last_action: string | null;
  last_action_at_ms: number | null;
  last_action_at: string | null;
  blocker_reason: string | null;
  open_orders_count: number;
  open_positions_count: number;
}

export interface UiMarketSide {
  token_id: string;
  label: string;
  outcome: string;
  price: number | null;
}

export interface UiMarket {
  condition_id: string;
  market_slug: string;
  title: string;
  subtitle: string;
  description?: string | null;
  status: string;
  tradable: boolean;
  close_time?: string | null;
  symbol?: string | null;
  timeframe?: string | null;
  sides: UiMarketSide[];
}

export interface TradeStats {
  total_pnl: number;
  win_rate: number;
  total_trades: number;
  winning_trades: number;
  losing_trades: number;
  avg_ack_latency_ms: number | null;
  ack_sample_count: number;
  pnl_history: { timestamp: string; pnl: number }[];
}

export interface Trade {
  id: string;
  timestamp: string;
  market: string;
  side: string;
  size: number;
  price: number;
  outcome: string;
  pnl: number;
}

export interface Position {
  market: string;
  side: string;
  size: number;
  entry_price: number;
  current_price: number | null;
  realized_pnl: number;
  unrealized_pnl: number | null;
  pnl: number;
}

export interface OnboardResult {
  eoa_wallet?: string;
  bound_wallet?: string;
  remote_signer_token?: string;
  signer_token?: string;
  order_signer_primary_token?: string;
  discovery_token?: string;
  premarket_alpha_token?: string;
  endgame_alpha_token?: string;
  mm_rewards_alpha_token?: string;
  evsnipe_discovery_token?: string;
  admin_api_token?: string;
  [key: string]: unknown;
}

export type SetupDoctorStatus = "ready" | "fixed" | "needs_you" | "failed";
export type SetupDoctorItemStatus =
  | "ok"
  | "fixed"
  | "missing_user"
  | "missing_generated"
  | "failed";

export interface SetupDoctorItem {
  key: string;
  label: string;
  status: SetupDoctorItemStatus | string;
  message: string;
  strategy?: string | null;
}

export interface SetupDoctorPopup {
  title: string;
  body: string;
  cta_label: string;
  cta_target: string;
}

export interface SetupDoctorResult {
  status: SetupDoctorStatus | string;
  items: SetupDoctorItem[];
  fixed_count: number;
  missing_user_count: number;
  bot_was_running: boolean;
  bot_restarted: boolean;
  popup?: SetupDoctorPopup | null;
}

export type PremarketLadderSafetyMode = "normal" | "safe" | "aggressive";
export type WeekendPolicy = "off" | "pause";

export interface PremarketSettings {
  tp_enabled: boolean;
  active_cap_per_asset: number;
  timeframes: string[];
  entry_ladder_mode_5m: PremarketLadderSafetyMode;
  entry_ladder_mode_non_m5: PremarketLadderSafetyMode;
  cancel_after_open_sec: {
    m5: number;
    m15: number;
    h1: number;
    h4: number;
  };
}

export interface EndgameSettings {
  timeframes: string[];
  per_period_cap_usd: number;
  tick0_multiplier: number;
  tick1_multiplier: number;
  tick2_multiplier: number;
}

export interface EVCurveSettings {
  timeframes: string[];
  max_flip_prob: number;
  min_buy_price: number;
  d1_enabled: boolean;
  d1_cap_usd: number;
}

export interface SessionBandSettings {
  timeframes: string[];
  flip_threshold_pct: number;
  tau2_enabled: boolean;
  tau1_enabled: boolean;
  tau2_multiplier: number;
  tau1_multiplier: number;
}

export interface EVSnipeSettings {
  pre_hit_enabled: boolean;
  pre_leg_ratio: number;
  saved_pre_leg_ratio: number;
  pre_trigger_bps: number;
  strike_window_pct: number;
  max_days_to_expiry: number;
}

export type MMRewardsMarketMode = "auto" | "hybrid";

export interface MMRewardsSettings {
  market_mode: MMRewardsMarketMode;
  single_market_slugs: string;
  auto_top_n: number;
  auto_refresh_sec: number;
  auto_rank_budget_usd: number;
  blacklist_keywords: string;
  reward_min_shares_cap: number;
}

export type MMSportQuoteSizeMode = "multiple" | "depth_ratio";
export type MMSportInventoryExitMode = "normal" | "aggressive" | "no_exit";
export type MMSportDiscoveryRoute = "sports" | "nonsports" | "dual";

export interface MMSportSettings {
  discovery_route: MMSportDiscoveryRoute;
  quote_size_mode: MMSportQuoteSizeMode;
  multiple_collateral_cap_mult: number;
  depth_ratio_collateral_cap_mult: number;
  min_reward_rate_per_day: number;
  allowed_sport_league_codes: string;
  blocked_sport_league_codes: string;
  blocked_competition_levels: string;
  market_allowlist_keywords: string;
  market_blacklist_keywords: string;
  reward_min_shares_cap: number;
  polymarket_live_guard_enable: boolean;
  polymarket_live_guard_ws_enable: boolean;
  polymarket_live_guard_ws_stale_ms: number;
  pause_after_fill_sec: number;
  inventory_exit_start_hours: number;
  inventory_exit_mode: MMSportInventoryExitMode;
  max_share_ratio: number;
  min_top_depth_usd: number;
  quote_expiry_min_sec: number;
  quote_expiry_max_sec: number;
}

export interface SharedSymbolMultipliers {
  btc: number;
  eth: number;
  sol: number;
  xrp: number;
  doge: number;
  bnb: number;
  hype: number;
}

export interface PremarketTimeframeMultipliers {
  m5: number;
  m15: number;
  h1: number;
  h4: number;
  d1: number;
}

export interface EVCurveTimeframeMultipliers {
  m15: number;
  h1: number;
  h4: number;
  d1: number;
}

export interface SizePolicySettings {
  symbol_multipliers: SharedSymbolMultipliers;
  premarket_timeframe_multipliers: PremarketTimeframeMultipliers;
  evcurve_timeframe_multipliers: EVCurveTimeframeMultipliers;
}

export interface StrategySettings {
  premarket: PremarketSettings;
  endgame: EndgameSettings;
  evcurve: EVCurveSettings;
  session_band: SessionBandSettings;
  evsnipe: EVSnipeSettings;
  mm_rewards: MMRewardsSettings;
  mm_sport: MMSportSettings;
}

export interface BotConfig {
  private_key: string;
  eoa_wallet: string;
  proxy_wallet: string;
  sig_type: number;
  weekend_policy: WeekendPolicy;
  symbols: string[];
  strategies: {
    premarket: boolean;
    endgame: boolean;
    evcurve: boolean;
    session_band: boolean;
    evsnipe: boolean;
    mm_rewards: boolean;
    mm_sport: boolean;
  };
  sizing: {
    premarket: number;
    endgame: number;
    evcurve: number;
    session_band: number;
    evsnipe_per_hit: number;
  };
  caps: {
    premarket: number;
    endgame: number;
    evcurve: number;
    session_band: number;
    evsnipe: number;
  };
  mm_tuning: {
    rewards_min_share_multiple: number;
    sport_quote_size_multiplier: number;
  };
  size_policy: SizePolicySettings;
  strategy_settings: StrategySettings;
  simulation: boolean;
  relayer_api_key: string;
  relayer_api_key_address: string;
  remote_signer_token: string;
  order_signer_primary_token_internal?: string;
  remote_discovery_token: string;
  remote_premarket_alpha_token: string;
  remote_endgame_alpha_token: string;
  remote_mm_rewards_alpha_token: string;
  remote_evsnipe_discovery_token: string;
  admin_api_token: string;
}

// Auth
export const verifyPassword = (password: string): Promise<boolean> =>
  invoke("verify_password", { password });

export const initializePassword = (password: string): Promise<void> =>
  invoke("initialize_password", { password });

export const isAuthInitialized = (): Promise<boolean> =>
  invoke("is_auth_initialized");

export const lockSession = (): Promise<void> => invoke("lock_session");

export const resetLocalAppData = (): Promise<void> => invoke("reset_local_app_data");

// Profiles
export const listProfiles = (): Promise<Profile[]> =>
  invoke("list_profiles");

export const createProfile = (
  name: string,
  proxyWallet: string,
  sigType: number
): Promise<Profile> =>
  invoke("create_profile", {
    name,
    proxyWalletAddress: proxyWallet,
    proxy_wallet_address: proxyWallet,
    signatureType: sigType,
    signature_type: sigType,
  });

export const getProfile = (id: string): Promise<Profile | null> =>
  invoke("get_profile", { id });

export const updateProfile = (profile: Profile): Promise<void> =>
  invoke("update_profile", { profile });

export const deleteProfile = (id: string): Promise<void> =>
  invoke("delete_profile", { id });

export const getActiveProfileId = (): Promise<string | null> =>
  invoke("get_active_profile_id");

export const setActiveProfile = (id: string): Promise<void> =>
  invoke("set_active_profile", { id });

// Bot control
export const startBot = (simulation: boolean): Promise<void> =>
  invoke("start_bot", { simulation });

export const stopBot = (): Promise<void> =>
  invoke("stop_bot");

export const restartBot = (simulation: boolean): Promise<void> =>
  invoke("restart_bot", { simulation });

export const getBotStatus = (): Promise<string> =>
  invoke("get_bot_status");

export const getLogLines = (
  count: number,
  cursor?: number | null
): Promise<LogTailBatch> =>
  invoke("get_log_lines", {
    count,
    cursor: cursor ?? null,
  });

export const botApiRequest = <T = unknown>(
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE",
  path: string,
  query?: Record<string, unknown>,
  body?: Record<string, unknown>
): Promise<T> =>
  invoke("bot_api_request", {
    method,
    path,
    query: query ?? null,
    body: body ?? null,
  });

// Config
export const saveConfig = (
  profileId: string,
  config: BotConfig
): Promise<void> =>
  invoke("save_config", {
    profileId,
    profile_id: profileId,
    config,
  });

export const getSavedConfig = (
  profileId: string
): Promise<BotConfig> =>
  invoke("get_saved_config", {
    profileId,
    profile_id: profileId,
  });

export const exportConfig = (
  profileId: string,
  password: string,
  currentPassword: string
): Promise<string> =>
  invoke("export_config", {
    profileId,
    profile_id: profileId,
    password,
    currentPassword,
    current_password: currentPassword,
  });

export const importConfig = (
  data: string,
  password: string,
  currentPassword: string
): Promise<string> =>
  invoke("import_config", {
    data,
    password,
    currentPassword,
    current_password: currentPassword,
  });

// Data
export const getTradeStats = (): Promise<TradeStats> =>
  invoke("get_trade_stats");

export const getRecentTrades = (limit: number): Promise<Trade[]> =>
  invoke("get_recent_trades", { limit });

export const getOpenPositions = (): Promise<Position[]> =>
  invoke("get_open_positions");

export const getWalletBalance = (): Promise<number> =>
  invoke("get_wallet_balance");

export const getHomeOverview = (): Promise<HomeOverview> =>
  invoke("get_home_overview");

export const getHomeActivity = (
  limit: number,
  cursor?: number | null
): Promise<HomeActivityBatch> =>
  invoke("get_home_activity", {
    limit,
    cursor: cursor ?? null,
  });

export const getHomeActivityApi = (
  limit: number
): Promise<HomeApiActivityItem[]> =>
  invoke("get_home_activity_api", { limit });

export const getHomePositionsApi = (
  limit: number
): Promise<HomeApiPositionItem[]> =>
  invoke("get_home_positions_api", { limit });

export const getHomeOpenOrdersApi = (
  limit: number
): Promise<HomeApiOpenOrderItem[]> =>
  invoke("get_home_open_orders_api", { limit });

export const getWalletSyncStatus = (): Promise<WalletSyncStatus> =>
  invoke("get_wallet_sync_status");

export const runWalletSyncNow = (): Promise<WalletSyncStatus> =>
  invoke("run_wallet_sync_now");

export const getGeoAccessStatus = (): Promise<GeoAccessStatus> =>
  invoke("get_geo_access_status");

// Onboarding
export const runOnboarding = (
  privateKey: string,
  sigType: number,
  proxyWallet: string
): Promise<OnboardResult> =>
  invoke("run_onboarding", {
    privateKey,
    private_key: privateKey,
    signatureType: sigType,
    signature_type: sigType,
    proxyWallet,
    proxy_wallet: proxyWallet,
  });

export const runSetupDoctor = (): Promise<SetupDoctorResult> =>
  invoke("run_setup_doctor");

export const getDataDirPath = (): Promise<string> =>
  invoke("get_data_dir_path");

export const openLogsFolder = (): Promise<void> =>
  invoke("open_logs_folder");
