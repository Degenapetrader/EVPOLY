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
  discovery_token?: string;
  premarket_alpha_token?: string;
  endgame_alpha_token?: string;
  mm_rewards_alpha_token?: string;
  evsnipe_discovery_token?: string;
  admin_api_token?: string;
  [key: string]: unknown;
}

export interface BotConfig {
  private_key: string;
  eoa_wallet: string;
  proxy_wallet: string;
  sig_type: number;
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
  simulation: boolean;
  relayer_api_key: string;
  relayer_api_key_address: string;
  remote_signer_token: string;
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

export const setPassword = (password: string): Promise<void> =>
  invoke("set_password", { password });

export const isAuthInitialized = (): Promise<boolean> =>
  invoke("is_auth_initialized");

// Profiles
export const listProfiles = (): Promise<Profile[]> =>
  invoke("list_profiles");

export const createProfile = (
  name: string,
  eoaWallet: string,
  proxyWallet: string,
  sigType: number
): Promise<Profile> =>
  invoke("create_profile", {
    name,
    eoaWalletAddress: eoaWallet,
    eoa_wallet_address: eoaWallet,
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

export const getLogLines = (count: number): Promise<LogLine[]> =>
  invoke("get_log_lines", { count });

export const startManualService = (
  simulation: boolean,
  port?: number
): Promise<void> =>
  invoke("start_manual_service", { simulation, port });

export const stopManualService = (): Promise<void> =>
  invoke("stop_manual_service");

export const getManualServiceStatus = (): Promise<string> =>
  invoke("get_manual_service_status");

export const getManualLogLines = (count: number): Promise<LogLine[]> =>
  invoke("get_manual_log_lines", { count });

export const manualApiRequest = (
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE",
  path: string,
  query?: Record<string, unknown>,
  body?: Record<string, unknown>
): Promise<unknown> =>
  invoke("manual_api_request", {
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
  password: string
): Promise<string> =>
  invoke("export_config", {
    profileId,
    profile_id: profileId,
    password,
  });

export const importConfig = (
  data: string,
  password: string
): Promise<string> =>
  invoke("import_config", { data, password });

// Data
export const getTradeStats = (): Promise<TradeStats> =>
  invoke("get_trade_stats");

export const getRecentTrades = (limit: number): Promise<Trade[]> =>
  invoke("get_recent_trades", { limit });

export const getOpenPositions = (): Promise<Position[]> =>
  invoke("get_open_positions");

export const getWalletBalance = (): Promise<number> =>
  invoke("get_wallet_balance");

// Onboarding
export const runOnboarding = (
  wallet: string,
  privateKey: string,
  sigType: number,
  proxyWallet: string
): Promise<OnboardResult> =>
  invoke("run_onboarding", {
    wallet,
    privateKey,
    private_key: privateKey,
    signatureType: sigType,
    signature_type: sigType,
    proxyWallet,
    proxy_wallet: proxyWallet,
  });

export const getDataDirPath = (): Promise<string> =>
  invoke("get_data_dir_path");

export const openLogsFolder = (): Promise<void> =>
  invoke("open_logs_folder");
