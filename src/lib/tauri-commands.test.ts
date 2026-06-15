import { beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

import {
  createProfile,
  derivePolymarketFunderAddresses,
  deriveWalletAddress,
  desktopMagicFinish,
  desktopMagicStart,
  exportConfig,
  getGeoAccessStatus,
  getSavedConfig,
  runOnboarding,
  saveConfig,
  type BotConfig,
} from "./tauri-commands";

const SAMPLE_CONFIG: BotConfig = {
  private_key: "0xpk",
  eoa_wallet: "0xeoa",
  proxy_wallet: "0xproxy",
  deposit_wallet: "",
  sig_type: 1,
  weekend_policy: "off",
  symbols: ["BTC"],
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
    premarket: 1000,
    endgame: 1000,
    evcurve: 1000,
    session_band: 1000,
    evsnipe: 1000,
  },
  mm_tuning: {
    rewards_min_share_multiple: 1,
    sport_quote_size_multiplier: 1.2,
    nonsport_quote_size_multiplier: 1.2,
  },
  size_policy: {
    symbol_multipliers: {
      btc: 1,
      eth: 0.8,
      sol: 0.5,
      xrp: 0.5,
      doge: 0.5,
      bnb: 0.5,
      hype: 0.5,
    },
    premarket_timeframe_multipliers: {
      m5: 0.75,
      m15: 1,
      h1: 1.25,
      h4: 1.25,
      d1: 1.25,
    },
    evcurve_timeframe_multipliers: {
      m15: 0.75,
      h1: 1,
      h4: 1.25,
      d1: 1.25,
    },
  },
  strategy_settings: {
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
      quote_size_mode: "multiple",
      nonsport_quote_size_mode: "multiple",
      entry_price_mode: "passive",
      multiple_collateral_cap_mult: 0.45,
      nonsport_multiple_collateral_cap_mult: 0.45,
      depth_ratio_collateral_cap_mult: 0.9,
      nonsport_depth_ratio_collateral_cap_mult: 0.9,
      min_reward_rate_per_day: 300,
      match_only: false,
      allowed_sport_league_codes: "",
      blocked_sport_league_codes: "",
      blocked_competition_levels: "",
      market_allowlist_keywords: "",
      market_blacklist_keywords: "",
      reward_min_shares_cap: 0,
      polymarket_live_guard_enable: true,
      polymarket_live_guard_ws_enable: true,
      polymarket_live_guard_ws_stale_ms: 600000,
      pause_after_fill_sec: 7200,
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
      quote_expiry_min_sec: 180,
      quote_expiry_max_sec: 300,
      quote_cooldown_min_sec: 10,
      quote_cooldown_max_sec: 60,
      fifo_max_share_ratio: 0.2,
      active_sport_market_cap: 50,
      active_nonsport_market_cap: 50,
    },
  },
  simulation: true,
  alpha_key: "",
  relayer_api_key: "",
  relayer_api_key_address: "",
  relayer_remote_signer_token: "",
  relayer_submit_signer_url: "",
  wallet_binding: "",
  onboarding_status: "",
  approval_status: "",
  remote_signer_token: "",
  remote_discovery_token: "",
  remote_premarket_alpha_token: "",
  remote_endgame_alpha_token: "",
  remote_mm_rewards_alpha_token: "",
  remote_evsnipe_discovery_token: "",
  admin_api_token: "",
};

describe("tauri command payload contracts", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
  });

  it("sends compatible payload keys for create_profile", async () => {
    await createProfile("Default", "0xproxy", 1);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("create_profile", {
      name: "Default",
      proxyWalletAddress: "0xproxy",
      proxy_wallet_address: "0xproxy",
      depositWalletAddress: "",
      deposit_wallet_address: "",
      signatureType: 1,
      signature_type: 1,
    });
  });

  it("sends compatible payload keys for save_config", async () => {
    await saveConfig("profile-1", SAMPLE_CONFIG);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("save_config", {
      profileId: "profile-1",
      profile_id: "profile-1",
      config: SAMPLE_CONFIG,
      generateCredentials: undefined,
      generate_credentials: undefined,
    });
  });

  it("can skip generated credentials while saving a wallet profile", async () => {
    await saveConfig("profile-1", SAMPLE_CONFIG, { generateCredentials: false });
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("save_config", {
      profileId: "profile-1",
      profile_id: "profile-1",
      config: SAMPLE_CONFIG,
      generateCredentials: false,
      generate_credentials: false,
    });
  });

  it("sends compatible payload keys for run_onboarding", async () => {
    await runOnboarding("0xprivate", 1, "0xproxy");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("run_onboarding", {
      privateKey: "0xprivate",
      private_key: "0xprivate",
      signatureType: 1,
      signature_type: 1,
      proxyWallet: "0xproxy",
      proxy_wallet: "0xproxy",
      depositWallet: "",
      deposit_wallet: "",
    });
  });

  it("sends deposit wallet keys for run_onboarding", async () => {
    await runOnboarding("0xprivate", 3, "", "0xdeposit");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("run_onboarding", {
      privateKey: "0xprivate",
      private_key: "0xprivate",
      signatureType: 3,
      signature_type: 3,
      proxyWallet: "",
      proxy_wallet: "",
      depositWallet: "0xdeposit",
      deposit_wallet: "0xdeposit",
    });
  });

  it("sends deposit wallet keys for create_profile", async () => {
    await createProfile("Deposit", "", 3, "0xdeposit");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("create_profile", {
      name: "Deposit",
      proxyWalletAddress: "",
      proxy_wallet_address: "",
      depositWalletAddress: "0xdeposit",
      deposit_wallet_address: "0xdeposit",
      signatureType: 3,
      signature_type: 3,
    });
  });

  it("sends compatible payload keys for desktop Magic start", async () => {
    await desktopMagicStart("user@example.com", "profile-1");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("desktop_magic_start", {
      email: "user@example.com",
      profileId: "profile-1",
      profile_id: "profile-1",
    });
  });

  it("sends compatible payload keys for desktop Magic finish", async () => {
    await desktopMagicFinish("session-1", "did-token", "public-key");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("desktop_magic_finish", {
      desktopOnboardSessionId: "session-1",
      desktop_onboard_session_id: "session-1",
      didToken: "did-token",
      did_token: "did-token",
      rsaPublicKey: "public-key",
      rsa_public_key: "public-key",
    });
  });

  it("sends compatible payload keys for derive_wallet_address", async () => {
    await deriveWalletAddress("0xprivate");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("derive_wallet_address", {
      privateKey: "0xprivate",
      private_key: "0xprivate",
    });
  });

  it("sends compatible payload keys for derive_polymarket_funder_addresses", async () => {
    await derivePolymarketFunderAddresses("0xprivate");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("derive_polymarket_funder_addresses", {
      privateKey: "0xprivate",
      private_key: "0xprivate",
    });
  });

  it("sends compatible payload keys for get_saved_config", async () => {
    await getSavedConfig("profile-1");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("get_saved_config", {
      profileId: "profile-1",
      profile_id: "profile-1",
    });
  });

  it("sends compatible payload keys for export_config", async () => {
    await exportConfig("profile-1", "secret", "desktop-pass");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("export_config", {
      profileId: "profile-1",
      profile_id: "profile-1",
      password: "secret",
      currentPassword: "desktop-pass",
      current_password: "desktop-pass",
    });
  });

  it("requests geo access status without payload", async () => {
    await getGeoAccessStatus();
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("get_geo_access_status");
  });
});
