import { beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

import {
  createProfile,
  exportConfig,
  getSavedConfig,
  runOnboarding,
  saveConfig,
  type BotConfig,
} from "./tauri-commands";

const SAMPLE_CONFIG: BotConfig = {
  private_key: "0xpk",
  eoa_wallet: "0xeoa",
  proxy_wallet: "0xproxy",
  sig_type: 1,
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
    endgame: 10,
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
  },
  simulation: true,
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

describe("tauri command payload contracts", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
  });

  it("sends compatible payload keys for create_profile", async () => {
    await createProfile("Default", "0xeoa", "0xproxy", 1);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("create_profile", {
      name: "Default",
      eoaWalletAddress: "0xeoa",
      eoa_wallet_address: "0xeoa",
      proxyWalletAddress: "0xproxy",
      proxy_wallet_address: "0xproxy",
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
    });
  });

  it("sends compatible payload keys for run_onboarding", async () => {
    await runOnboarding("0xeoa", "0xprivate", 1, "0xproxy");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("run_onboarding", {
      wallet: "0xeoa",
      privateKey: "0xprivate",
      private_key: "0xprivate",
      signatureType: 1,
      signature_type: 1,
      proxyWallet: "0xproxy",
      proxy_wallet: "0xproxy",
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
    await exportConfig("profile-1", "secret");
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith("export_config", {
      profileId: "profile-1",
      profile_id: "profile-1",
      password: "secret",
    });
  });
});
