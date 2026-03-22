import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { check } from "@tauri-apps/plugin-updater";
import { ProfileSwitcher } from "../components/ProfileSwitcher";
import { UpdateBanner } from "../components/UpdateBanner";
import { useBotStatus } from "../hooks/useBotStatus";
import { useWalletBalance } from "../hooks/useWalletBalance";
import {
  getActiveProfileId,
  getSavedConfig,
  restartBot,
  saveConfig,
  startBot,
  stopBot,
  type BotConfig,
} from "../lib/tauri-commands";

const CORE_SYMBOLS = ["BTC", "ETH", "SOL", "XRP"] as const;
const EXTRA_SYMBOLS = ["DOGE", "BNB", "HYPE"] as const;

const STRATEGIES = [
  { key: "premarket", label: "Premarket" },
  { key: "endgame", label: "Endgame" },
  { key: "evcurve", label: "EVCurve" },
  { key: "session_band", label: "SessionBand" },
  { key: "evsnipe", label: "EVSnipe" },
  { key: "mm_rewards", label: "MM Rewards" },
  { key: "mm_sport", label: "MM Sport" },
] as const;

type StrategyKey = (typeof STRATEGIES)[number]["key"];

type StrategyFieldView = {
  sizeLabel: string;
  sizeValue: number;
  capLabel: string | null;
  capValue: number | null;
};

const DEFAULT_CONFIG: BotConfig = {
  private_key: "",
  eoa_wallet: "",
  proxy_wallet: "",
  sig_type: 1,
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

function mergeConfig(saved: Partial<BotConfig> | null | undefined): BotConfig {
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

function statusText(status: string): string {
  if (status.startsWith("error:")) return "Error";
  if (status === "running") return "Running";
  if (status === "starting") return "Starting";
  if (status === "stopping") return "Stopping";
  if (status === "unknown") return "Unknown";
  return "Stopped";
}

function formatUsd(value: number): string {
  return new Intl.NumberFormat("en-US", {
    style: "currency",
    currency: "USD",
    maximumFractionDigits: 2,
  }).format(value || 0);
}

function parseNonNegative(value: string, fallback: number): number {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 0) return fallback;
  return parsed;
}

function readStrategyFields(config: BotConfig, strategy: StrategyKey): StrategyFieldView {
  switch (strategy) {
    case "premarket":
      return {
        sizeLabel: "Size (USD)",
        sizeValue: config.sizing.premarket,
        capLabel: "Max Exposure (USD)",
        capValue: config.caps.premarket,
      };
    case "endgame":
      return {
        sizeLabel: "Size (USD)",
        sizeValue: config.sizing.endgame,
        capLabel: "Max Exposure (USD)",
        capValue: config.caps.endgame,
      };
    case "evcurve":
      return {
        sizeLabel: "Size (USD)",
        sizeValue: config.sizing.evcurve,
        capLabel: "Max Exposure (USD)",
        capValue: config.caps.evcurve,
      };
    case "session_band":
      return {
        sizeLabel: "Size (USD)",
        sizeValue: config.sizing.session_band,
        capLabel: "Max Exposure (USD)",
        capValue: config.caps.session_band,
      };
    case "evsnipe":
      return {
        sizeLabel: "Size per hit (USD)",
        sizeValue: config.sizing.evsnipe_per_hit,
        capLabel: "Max Exposure (USD)",
        capValue: config.caps.evsnipe,
      };
    case "mm_rewards":
      return {
        sizeLabel: "Min Share Multiple",
        sizeValue: config.mm_tuning.rewards_min_share_multiple,
        capLabel: null,
        capValue: null,
      };
    case "mm_sport":
      return {
        sizeLabel: "Quote Size Multiplier",
        sizeValue: config.mm_tuning.sport_quote_size_multiplier,
        capLabel: null,
        capValue: null,
      };
    default:
      return {
        sizeLabel: "Size (USD)",
        sizeValue: 0,
        capLabel: null,
        capValue: null,
      };
  }
}

function updateStrategySize(config: BotConfig, strategy: StrategyKey, value: number): BotConfig {
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
      mm_tuning: { ...config.mm_tuning, rewards_min_share_multiple: value },
    };
  }
  return {
    ...config,
    mm_tuning: { ...config.mm_tuning, sport_quote_size_multiplier: value },
  };
}

function updateStrategyCap(config: BotConfig, strategy: StrategyKey, value: number): BotConfig {
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

export function Dashboard() {
  const navigate = useNavigate();
  const { status, isRunning, errorMessage } = useBotStatus();
  const {
    balance,
    isStale: balanceStale,
    error: balanceError,
    refresh: refreshBalance,
  } = useWalletBalance();

  const [activeProfileId, setActiveProfileId] = useState<string | null>(null);
  const [selectedStrategy, setSelectedStrategy] = useState<StrategyKey>("endgame");
  const [config, setConfig] = useState<BotConfig>(DEFAULT_CONFIG);
  const [simulation, setSimulation] = useState(true);
  const [actionLoading, setActionLoading] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [savingSettings, setSavingSettings] = useState(false);
  const [saveMessage, setSaveMessage] = useState<string | null>(null);
  const [updateVersion, setUpdateVersion] = useState<string | null>(null);
  const [updateDownloading, setUpdateDownloading] = useState(false);
  const [pendingUpdate, setPendingUpdate] =
    useState<Awaited<ReturnType<typeof check>> | null>(null);

  const enabledStrategyCount = useMemo(
    () => Object.values(config.strategies).filter(Boolean).length,
    [config.strategies]
  );
  const selectedStrategyLabel =
    STRATEGIES.find((s) => s.key === selectedStrategy)?.label ?? "Strategy";
  const selectedFields = useMemo(
    () => readStrategyFields(config, selectedStrategy),
    [config, selectedStrategy]
  );

  const loadProfileState = useCallback(async (profileId: string) => {
    const saved = await getSavedConfig(profileId);
    const merged = mergeConfig(saved);
    setConfig(merged);
    setSimulation(merged.simulation);
  }, []);

  useEffect(() => {
    getActiveProfileId()
      .then(async (id) => {
        setActiveProfileId(id);
        if (id) {
          await loadProfileState(id);
        }
      })
      .catch(() => {});
  }, [loadProfileState]);

  useEffect(() => {
    (async () => {
      try {
        const update = await check();
        setPendingUpdate(update ?? null);
        setUpdateVersion(update?.version ?? null);
      } catch {
        setPendingUpdate(null);
        setUpdateVersion(null);
      }
    })();
  }, []);

  const getErrorText = (err: unknown, fallback: string): string => {
    if (typeof err === "string" && err.trim()) return err;
    if (err && typeof err === "object" && "toString" in err) {
      const text = String(err);
      if (text && text !== "[object Object]") return text;
    }
    return fallback;
  };

  const handleUpdate = async () => {
    if (!pendingUpdate || updateDownloading) return;
    setUpdateDownloading(true);
    try {
      await pendingUpdate.downloadAndInstall();
      setUpdateVersion(null);
      setPendingUpdate(null);
    } catch {
      // keep banner visible for retry
    } finally {
      setUpdateDownloading(false);
    }
  };

  const handleStart = async () => {
    setActionLoading(true);
    try {
      await startBot(simulation);
      setActionError(null);
      await refreshBalance();
    } catch (err) {
      setActionError(getErrorText(err, "failed to start bot"));
    } finally {
      setActionLoading(false);
    }
  };

  const handleRestart = async () => {
    setActionLoading(true);
    try {
      await restartBot(simulation);
      setActionError(null);
      await refreshBalance();
    } catch (err) {
      setActionError(getErrorText(err, "failed to restart bot"));
    } finally {
      setActionLoading(false);
    }
  };

  const handleStop = async () => {
    setActionLoading(true);
    try {
      await stopBot();
      setActionError(null);
    } catch (err) {
      setActionError(getErrorText(err, "failed to stop bot"));
    } finally {
      setActionLoading(false);
    }
  };

  const handleProfileSwitch = async (id: string) => {
    setActiveProfileId(id);
    await loadProfileState(id);
    await refreshBalance();
  };

  const handleToggleStrategyEnabled = () => {
    setConfig((prev) => ({
      ...prev,
      strategies: {
        ...prev.strategies,
        [selectedStrategy]: !prev.strategies[selectedStrategy],
      },
    }));
  };

  const handleSizeChange = (raw: string) => {
    setConfig((prev) => {
      const next = parseNonNegative(raw, selectedFields.sizeValue);
      return updateStrategySize(prev, selectedStrategy, next);
    });
  };

  const handleCapChange = (raw: string) => {
    if (selectedFields.capValue === null) return;
    setConfig((prev) => {
      const next = parseNonNegative(raw, selectedFields.capValue ?? 0);
      return updateStrategyCap(prev, selectedStrategy, next);
    });
  };

  const toggleSymbol = (symbol: string) => {
    if (symbol === "BTC") return;
    setConfig((prev) => ({
      ...prev,
      symbols: prev.symbols.includes(symbol)
        ? prev.symbols.filter((item) => item !== symbol)
        : [...prev.symbols, symbol],
    }));
  };

  const handleSaveSettings = async () => {
    if (!activeProfileId) {
      setSaveMessage("No active profile. Open settings and select a profile.");
      return;
    }
    setSavingSettings(true);
    setSaveMessage(null);
    try {
      const next = { ...config, simulation };
      await saveConfig(activeProfileId, next);
      setConfig(next);
      setSaveMessage("Saved. Restart the bot to apply changes.");
    } catch (err) {
      setSaveMessage(getErrorText(err, "failed to save settings"));
    } finally {
      setSavingSettings(false);
    }
  };

  const displayError =
    errorMessage?.trim() || actionError || balanceError || null;
  const availableBalance = balance;
  const runningText = statusText(status);
  const selectedIsEnabled = config.strategies[selectedStrategy];

  return (
    <div className="h-screen w-full overflow-hidden bg-[var(--bg-primary)] p-6 text-[var(--text-primary)]">
      <UpdateBanner
        version={updateDownloading ? "Downloading..." : updateVersion}
        onUpdate={handleUpdate}
      />
      <div className="mx-auto h-full max-w-[1700px] rounded-2xl border border-[var(--border)] bg-[var(--bg-secondary)] p-3 shadow-[0_10px_50px_rgba(0,0,0,0.25)]">
        <div className="flex h-full gap-3">
          <aside className="flex w-[250px] shrink-0 flex-col rounded-2xl border border-[var(--border)] bg-[var(--bg-tertiary)] px-4 py-4">
            <div>
              <h1 className="text-[40px] font-semibold leading-none tracking-tight">
                EVPoly
              </h1>
              <p className="mt-1 text-[12px] text-[var(--text-secondary)]">
                Beginner Mode
              </p>
            </div>

            <div className="mt-4 rounded-xl bg-[var(--accent-soft)] px-3 py-2 text-[12px]">
              <div className="flex items-center gap-2">
                <span className="h-2 w-2 rounded-full bg-[var(--green)]" />
                <span className="font-medium">{runningText}</span>
                <span className="text-[var(--text-secondary)]">
                  • {simulation ? "Dry Run" : "Live"}
                </span>
              </div>
            </div>

            <div className="mt-5">
              <div className="mb-2 text-[12px] font-medium text-[var(--text-secondary)]">
                Strategies
              </div>
              <div className="space-y-2">
                {STRATEGIES.map((strategy) => {
                  const selected = selectedStrategy === strategy.key;
                  return (
                    <button
                      key={strategy.key}
                      onClick={() => setSelectedStrategy(strategy.key)}
                      className={`flex w-full items-center justify-between rounded-xl border px-3 py-2 text-left text-[15px] transition-colors ${
                        selected
                          ? "border-[var(--accent)] bg-[var(--accent-soft)]"
                          : "border-transparent bg-transparent hover:border-[var(--border)] hover:bg-[var(--surface-plain)]"
                      }`}
                    >
                      <span>{strategy.label}</span>
                      {selected ? (
                        <span className="text-[11px] text-[var(--accent)]">
                          Selected
                        </span>
                      ) : null}
                    </button>
                  );
                })}
              </div>
            </div>

            <div className="mt-auto space-y-2">
              <button
                onClick={() => navigate("/config")}
                className="w-full rounded-xl border border-[var(--border)] bg-[var(--surface-plain)] px-3 py-2 text-left text-[14px] hover:border-[var(--accent)]"
              >
                Open settings
              </button>
              <button
                onClick={() => navigate("/manual")}
                className="w-full rounded-xl border border-[var(--border)] bg-[var(--surface-plain)] px-3 py-2 text-left text-[14px] hover:border-[var(--accent)]"
              >
                Manual controls
              </button>
            </div>
          </aside>

          <main className="flex min-w-0 flex-1 flex-col rounded-2xl border border-[var(--border)] bg-[var(--surface-plain)] p-4">
            <header className="flex items-start justify-between gap-4">
              <div>
                <h2 className="text-[58px] font-semibold leading-none tracking-tight">
                  Dashboard
                </h2>
                <p className="mt-1 text-[13px] text-[var(--text-secondary)]">
                  Simple status and strategy settings
                </p>
              </div>

              <div className="flex flex-col items-end gap-2">
                <ProfileSwitcher
                  activeProfileId={activeProfileId}
                  onSwitch={(id) => {
                    void handleProfileSwitch(id);
                  }}
                />
                <div className="flex items-center gap-2 rounded-xl border border-[var(--border)] bg-[var(--bg-secondary)] px-2 py-2">
                  <button
                    onClick={handleStart}
                    disabled={actionLoading || isRunning}
                    className="rounded-lg bg-[var(--green)] px-6 py-1.5 text-[12px] font-medium text-white disabled:opacity-45"
                  >
                    Start
                  </button>
                  <button
                    onClick={handleRestart}
                    disabled={actionLoading}
                    className="rounded-lg bg-[var(--accent)] px-6 py-1.5 text-[12px] font-medium text-white disabled:opacity-45"
                  >
                    Restart
                  </button>
                  <button
                    onClick={handleStop}
                    disabled={actionLoading || !isRunning}
                    className="rounded-lg bg-[var(--red)] px-6 py-1.5 text-[12px] font-medium text-white disabled:opacity-45"
                  >
                    Stop
                  </button>
                </div>
              </div>
            </header>

            {displayError ? (
              <div className="mt-3 rounded-xl border border-[var(--red)]/50 bg-[var(--red)]/10 px-3 py-2 text-[12px] text-[var(--red)]">
                {displayError}
              </div>
            ) : null}
            {!displayError && balanceStale ? (
              <div className="mt-3 rounded-xl border border-[var(--yellow)]/50 bg-[var(--yellow)]/10 px-3 py-2 text-[12px] text-[var(--yellow)]">
                Balance data is stale. Last known value is shown.
              </div>
            ) : null}

            <section className="mt-3 grid grid-cols-3 gap-2">
              <div className="rounded-xl border border-[var(--border)] bg-[var(--bg-secondary)] px-4 py-3">
                <div className="text-[12px] text-[var(--text-secondary)]">
                  Polymarket Balance
                </div>
                <div className="mt-1 text-[44px] font-semibold leading-none tracking-tight">
                  {formatUsd(balance)}
                </div>
              </div>
              <div className="rounded-xl border border-[var(--border)] bg-[var(--bg-secondary)] px-4 py-3">
                <div className="text-[12px] text-[var(--text-secondary)]">
                  Available Balance
                </div>
                <div className="mt-1 text-[44px] font-semibold leading-none tracking-tight text-[var(--accent)]">
                  {formatUsd(availableBalance)}
                </div>
              </div>
              <div className="rounded-xl border border-[var(--border)] bg-[var(--bg-secondary)] px-4 py-3">
                <div className="text-[12px] text-[var(--text-secondary)]">
                  Strategy Status
                </div>
                <div className="mt-1 text-[44px] font-semibold leading-none tracking-tight">
                  {enabledStrategyCount} active
                </div>
              </div>
            </section>

            <section className="mt-2 flex flex-wrap items-center gap-2 text-[12px] text-[var(--text-secondary)]">
              <span>Mode:</span>
              <span className="rounded-full bg-[var(--accent-soft)] px-2.5 py-1 text-[var(--accent)]">
                {simulation ? "Dry Run" : "Live"}
              </span>
              <button
                onClick={() => setSimulation((prev) => !prev)}
                disabled={isRunning}
                className="rounded-full border border-[var(--border)] bg-[var(--bg-secondary)] px-2.5 py-1 text-[12px] hover:border-[var(--accent)] disabled:opacity-50"
              >
                {simulation ? "Switch to Live" : "Switch to Dry Run"}
              </button>
              <span className="ml-2">Selected:</span>
              <span className="rounded-full bg-[var(--accent-soft)] px-2.5 py-1 text-[var(--accent)]">
                {selectedStrategyLabel}
              </span>
              <span className="rounded-full bg-[var(--accent-soft)] px-2.5 py-1 text-[var(--accent)]">
                {selectedIsEnabled ? "Enabled" : "Disabled"}
              </span>
            </section>

            <section className="mt-2 flex min-h-0 flex-1 flex-col rounded-xl border border-[var(--border)] bg-[var(--bg-secondary)] p-3">
              <div className="text-[12px] text-[var(--text-secondary)]">
                {selectedStrategyLabel} Settings
              </div>
              <div className="mt-2 flex items-center gap-2">
                <span className="rounded-full bg-[var(--accent-soft)] px-2.5 py-1 text-[12px] text-[var(--accent)]">
                  General
                </span>
                <span className="rounded-full bg-[var(--surface-plain)] px-2.5 py-1 text-[12px] text-[var(--text-secondary)]">
                  Risk
                </span>
                <span className="rounded-full bg-[var(--surface-plain)] px-2.5 py-1 text-[12px] text-[var(--text-secondary)]">
                  Symbols
                </span>
              </div>

              <div className="mt-3 grid grid-cols-[220px_1fr] items-center gap-x-4 gap-y-3">
                <div className="text-[12px] text-[var(--text-secondary)]">Enabled</div>
                <button
                  onClick={handleToggleStrategyEnabled}
                  className={`justify-self-end rounded-full px-3 py-1 text-[12px] ${
                    selectedIsEnabled
                      ? "bg-[var(--accent)] text-white"
                      : "border border-[var(--border)] bg-[var(--surface-plain)] text-[var(--text-secondary)]"
                  }`}
                >
                  {selectedIsEnabled ? "On" : "Off"}
                </button>

                <div className="text-[12px] text-[var(--text-secondary)]">
                  {selectedFields.sizeLabel}
                </div>
                <input
                  type="number"
                  min="0"
                  step="0.1"
                  value={selectedFields.sizeValue}
                  onChange={(event) => handleSizeChange(event.target.value)}
                  className="w-full rounded-lg border border-[var(--border)] bg-[var(--surface-plain)] px-3 py-1.5 text-[14px] outline-none focus:border-[var(--accent)]"
                />

                {selectedFields.capLabel && selectedFields.capValue !== null ? (
                  <>
                    <div className="text-[12px] text-[var(--text-secondary)]">
                      {selectedFields.capLabel}
                    </div>
                    <input
                      type="number"
                      min="0"
                      step="1"
                      value={selectedFields.capValue}
                      onChange={(event) => handleCapChange(event.target.value)}
                      className="w-full rounded-lg border border-[var(--border)] bg-[var(--surface-plain)] px-3 py-1.5 text-[14px] outline-none focus:border-[var(--accent)]"
                    />
                  </>
                ) : null}

                <div className="text-[12px] text-[var(--text-secondary)]">Symbols</div>
                <div className="flex flex-wrap gap-2">
                  {CORE_SYMBOLS.map((symbol) => (
                    <button
                      key={symbol}
                      disabled={symbol === "BTC"}
                      onClick={() => toggleSymbol(symbol)}
                      className={`rounded-full px-3 py-1 text-[12px] ${
                        config.symbols.includes(symbol)
                          ? "bg-[var(--accent-soft)] text-[var(--accent)]"
                          : "border border-[var(--border)] bg-[var(--surface-plain)] text-[var(--text-secondary)]"
                      } ${symbol === "BTC" ? "opacity-80" : ""}`}
                    >
                      {symbol}
                    </button>
                  ))}
                  {EXTRA_SYMBOLS.map((symbol) => (
                    <button
                      key={symbol}
                      onClick={() => toggleSymbol(symbol)}
                      className={`rounded-full px-3 py-1 text-[12px] ${
                        config.symbols.includes(symbol)
                          ? "bg-[var(--accent-soft)] text-[var(--accent)]"
                          : "border border-[var(--border)] bg-[var(--surface-plain)] text-[var(--text-secondary)]"
                      }`}
                    >
                      {symbol}
                    </button>
                  ))}
                </div>
              </div>

              <div className="mt-auto pt-3">
                <button
                  onClick={handleSaveSettings}
                  disabled={savingSettings}
                  className="w-full rounded-xl bg-[var(--accent)] px-4 py-2 text-left text-[15px] font-medium text-white disabled:opacity-45"
                >
                  {savingSettings ? "Saving..." : "Save strategy settings"}
                </button>
                {saveMessage ? (
                  <div className="mt-2 text-[12px] text-[var(--text-secondary)]">
                    {saveMessage}
                  </div>
                ) : null}
              </div>
            </section>
          </main>
        </div>
      </div>
    </div>
  );
}
