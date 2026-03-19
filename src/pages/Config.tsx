import { useState, useEffect } from "react";
import { useNavigate } from "react-router-dom";
import {
  saveConfig,
  getSavedConfig,
  exportConfig,
  importConfig,
  runOnboarding,
  listProfiles,
  createProfile,
  setActiveProfile,
  getActiveProfileId,
  type BotConfig,
  type OnboardResult,
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

const DEFAULT_CONFIG: BotConfig = {
  private_key: "",
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
};

function Toggle({
  enabled,
  onChange,
}: {
  enabled: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <button
      type="button"
      onClick={() => onChange(!enabled)}
      className={`relative w-10 h-5 rounded-full transition-colors ${
        enabled ? "bg-[var(--accent)]" : "bg-[var(--border)]"
      }`}
    >
      <span
        className={`absolute top-0.5 left-0.5 w-4 h-4 rounded-full bg-white transition-transform ${
          enabled ? "translate-x-5" : ""
        }`}
      />
    </button>
  );
}

function SectionHeader({ title }: { title: string }) {
  return (
    <h3 className="text-sm font-semibold text-[var(--text-primary)] mb-3 mt-6 first:mt-0">
      {title}
    </h3>
  );
}

function InputField({
  label,
  value,
  onChange,
  type = "text",
  placeholder,
  disabled,
}: {
  label: string;
  value: string | number;
  onChange: (v: string) => void;
  type?: string;
  placeholder?: string;
  disabled?: boolean;
}) {
  return (
    <div>
      <label className="block text-xs text-[var(--text-secondary)] mb-1.5">
        {label}
      </label>
      <input
        type={type}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        disabled={disabled}
        className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-[var(--text-primary)] text-sm outline-none focus:border-[var(--accent)] transition-colors disabled:opacity-50"
      />
    </div>
  );
}

export function Config() {
  const navigate = useNavigate();
  const [config, setConfig] = useState<BotConfig>(DEFAULT_CONFIG);
  const [profileId, setProfileId] = useState<string | null>(null);
  const [showPrivateKey, setShowPrivateKey] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveMsg, setSaveMsg] = useState("");
  const [onboardWallet, setOnboardWallet] = useState("");
  const [onboardLoading, setOnboardLoading] = useState(false);
  const [onboardResult, setOnboardResult] = useState<OnboardResult | null>(null);
  const [exportPw, setExportPw] = useState("");
  const [importPw, setImportPw] = useState("");
  const [importData, setImportData] = useState("");

  useEffect(() => {
    (async () => {
      try {
        let id = await getActiveProfileId();
        if (!id) {
          const profiles = await listProfiles();
          if (profiles.length > 0) {
            id = profiles[0].id;
            await setActiveProfile(id);
          }
        }
        if (id) {
          setProfileId(id);
          const saved = await getSavedConfig(id);
          setConfig({
            ...DEFAULT_CONFIG,
            ...saved,
            strategies: {
              ...DEFAULT_CONFIG.strategies,
              ...saved.strategies,
            },
            sizing: {
              ...DEFAULT_CONFIG.sizing,
              ...saved.sizing,
            },
            caps: {
              ...DEFAULT_CONFIG.caps,
              ...saved.caps,
            },
            mm_tuning: {
              ...DEFAULT_CONFIG.mm_tuning,
              ...saved.mm_tuning,
            },
          });
        }
      } catch {
        // keep defaults when no profile exists yet
      }
    })();
  }, []);

  const ensureProfile = async (): Promise<string> => {
    if (profileId) return profileId;
    let id = await getActiveProfileId();
    if (id) {
      setProfileId(id);
      return id;
    }
    const profiles = await listProfiles();
    if (profiles.length > 0) {
      id = profiles[0].id;
      await setActiveProfile(id);
      setProfileId(id);
      return id;
    }
    const created = await createProfile(
      "Default",
      config.proxy_wallet.trim(),
      config.sig_type
    );
    await setActiveProfile(created.id);
    setProfileId(created.id);
    return created.id;
  };

  const update = <K extends keyof BotConfig>(
    key: K,
    val: BotConfig[K]
  ) => {
    setConfig((prev) => ({ ...prev, [key]: val }));
  };

  const toggleSymbol = (sym: string) => {
    if (sym === "BTC") return;
    setConfig((prev) => ({
      ...prev,
      symbols: prev.symbols.includes(sym)
        ? prev.symbols.filter((s) => s !== sym)
        : [...prev.symbols, sym],
    }));
  };

  const toggleStrategy = (key: StrategyKey) => {
    setConfig((prev) => ({
      ...prev,
      strategies: {
        ...prev.strategies,
        [key]: !prev.strategies[key],
      },
    }));
  };

  const handleSave = async () => {
    setSaving(true);
    setSaveMsg("");
    try {
      const id = await ensureProfile();
      await saveConfig(id, config);
      setSaveMsg("Configuration saved");
      setTimeout(() => setSaveMsg(""), 3000);
    } catch (err) {
      setSaveMsg(`Error: ${err}`);
    }
    setSaving(false);
  };

  const handleExport = async () => {
    if (!profileId || !exportPw) return;
    try {
      const data = await exportConfig(profileId, exportPw);
      await navigator.clipboard.writeText(data);
      setSaveMsg("Config copied to clipboard");
      setTimeout(() => setSaveMsg(""), 3000);
    } catch (err) {
      setSaveMsg(`Export error: ${err}`);
    }
  };

  const handleImport = async () => {
    if (!importData || !importPw) return;
    try {
      await importConfig(importData, importPw);
      setSaveMsg("Config imported successfully");
      setTimeout(() => setSaveMsg(""), 3000);
    } catch (err) {
      setSaveMsg(`Import error: ${err}`);
    }
  };

  const handleRunOnboarding = async () => {
    setOnboardLoading(true);
    setSaveMsg("");
    try {
      const wallet = onboardWallet.trim() || config.proxy_wallet.trim();
      if (!wallet) {
        throw new Error("Enter wallet address for onboarding");
      }
      if (!config.private_key.trim()) {
        throw new Error("Private key is required for onboarding");
      }

      const result = await runOnboarding(
        wallet,
        config.private_key,
        config.sig_type,
        config.proxy_wallet
      );
      setOnboardResult(result);

      const signerToken =
        (result.remote_signer_token as string | undefined) ||
        (result.signer_token as string | undefined) ||
        "";
      if (signerToken) {
        update("remote_signer_token", signerToken);
      }

      setSaveMsg("Onboarding finished. Review values and click Save Configuration.");
      setTimeout(() => setSaveMsg(""), 5000);
    } catch (err) {
      setSaveMsg(`Onboarding error: ${err}`);
    } finally {
      setOnboardLoading(false);
    }
  };

  return (
    <div className="h-full bg-[var(--bg-primary)] flex flex-col overflow-hidden">
      {/* Top Bar */}
      <div className="flex items-center justify-between px-6 py-4 border-b border-[var(--border)]">
        <div className="flex items-center gap-3">
          <button
            onClick={() => navigate("/dashboard")}
            className="p-2 rounded-lg bg-[var(--bg-tertiary)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors"
          >
            <svg
              className="w-4 h-4 text-[var(--text-secondary)]"
              fill="none"
              stroke="currentColor"
              viewBox="0 0 24 24"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth={2}
                d="M15 19l-7-7 7-7"
              />
            </svg>
          </button>
          <h1 className="text-lg font-semibold text-[var(--text-primary)]">
            Configuration
          </h1>
        </div>
        <button
          onClick={() => navigate("/manual")}
          className="px-3 py-2 rounded-lg bg-[var(--bg-tertiary)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors text-xs text-[var(--text-primary)]"
        >
          Manual
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto px-6 py-4 pb-24">
        <div className="max-w-2xl mx-auto space-y-0">
          {/* Credentials */}
          <SectionHeader title="Credentials" />
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4 space-y-3">
            <div>
              <label className="block text-xs text-[var(--text-secondary)] mb-1.5">
                Private Key
              </label>
              <div className="relative">
                <input
                  type={showPrivateKey ? "text" : "password"}
                  value={config.private_key}
                  onChange={(e) => update("private_key", e.target.value)}
                  className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 pr-10 text-[var(--text-primary)] text-sm outline-none focus:border-[var(--accent)] transition-colors"
                />
                <button
                  type="button"
                  onClick={() => setShowPrivateKey(!showPrivateKey)}
                  className="absolute right-2 top-1/2 -translate-y-1/2 text-[var(--text-secondary)] hover:text-[var(--text-primary)] p-1"
                >
                  {showPrivateKey ? (
                    <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M13.875 18.825A10.05 10.05 0 0112 19c-4.478 0-8.268-2.943-9.543-7a9.97 9.97 0 011.563-3.029m5.858.908a3 3 0 114.243 4.243M9.878 9.878l4.242 4.242M9.88 9.88l-3.29-3.29m7.532 7.532l3.29 3.29M3 3l3.59 3.59m0 0A9.953 9.953 0 0112 5c4.478 0 8.268 2.943 9.543 7a10.025 10.025 0 01-4.132 5.411m0 0L21 21" />
                    </svg>
                  ) : (
                    <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M15 12a3 3 0 11-6 0 3 3 0 016 0z" />
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M2.458 12C3.732 7.943 7.523 5 12 5c4.478 0 8.268 2.943 9.542 7-1.274 4.057-5.064 7-9.542 7-4.477 0-8.268-2.943-9.542-7z" />
                    </svg>
                  )}
                </button>
              </div>
            </div>
            <InputField
              label="Proxy Wallet Address"
              value={config.proxy_wallet}
              onChange={(v) => update("proxy_wallet", v)}
              placeholder="0x..."
            />
            <div>
              <label className="block text-xs text-[var(--text-secondary)] mb-1.5">
                Signature Type
              </label>
              <select
                value={config.sig_type}
                onChange={(e) => update("sig_type", Number(e.target.value))}
                className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-[var(--text-primary)] text-sm outline-none focus:border-[var(--accent)] transition-colors"
              >
                <option value={0}>EOA (0)</option>
                <option value={1}>Proxy (1)</option>
                <option value={2}>Safe (2)</option>
              </select>
            </div>
          </div>

          {/* Onboarding */}
          <SectionHeader title="Onboarding" />
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4 space-y-3">
            <InputField
              label="EOA Wallet Address (optional)"
              value={onboardWallet}
              onChange={setOnboardWallet}
              placeholder="Uses Proxy Wallet Address when empty"
            />
            <button
              onClick={handleRunOnboarding}
              disabled={onboardLoading}
              className="px-4 py-2 text-sm rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {onboardLoading ? "Running onboarding..." : "Run Onboarding"}
            </button>
            {onboardResult ? (
              <pre className="text-xs text-[var(--text-secondary)] bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg p-3 overflow-auto max-h-44">
                {JSON.stringify(onboardResult, null, 2)}
              </pre>
            ) : null}
          </div>

          {/* Symbols */}
          <SectionHeader title="Symbols" />
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4 space-y-3">
            <div className="flex flex-wrap gap-2">
              {CORE_SYMBOLS.map((sym) => (
                <label
                  key={sym}
                  className={`flex items-center gap-2 px-3 py-1.5 rounded-lg border text-sm cursor-pointer transition-colors ${
                    config.symbols.includes(sym)
                      ? "bg-[var(--accent)]/10 border-[var(--accent)] text-[var(--accent)]"
                      : "bg-[var(--bg-tertiary)] border-[var(--border)] text-[var(--text-secondary)]"
                  } ${sym === "BTC" ? "opacity-70 cursor-not-allowed" : ""}`}
                >
                  <input
                    type="checkbox"
                    checked={config.symbols.includes(sym)}
                    onChange={() => toggleSymbol(sym)}
                    disabled={sym === "BTC"}
                    className="sr-only"
                  />
                  {sym}
                </label>
              ))}
            </div>
            <div>
              <p className="text-xs text-[var(--text-secondary)] mb-2">
                Endgame / EVCurve / EVSnipe only
              </p>
              <div className="flex flex-wrap gap-2">
                {EXTRA_SYMBOLS.map((sym) => (
                  <label
                    key={sym}
                    className={`flex items-center gap-2 px-3 py-1.5 rounded-lg border text-sm cursor-pointer transition-colors ${
                      config.symbols.includes(sym)
                        ? "bg-[var(--accent)]/10 border-[var(--accent)] text-[var(--accent)]"
                        : "bg-[var(--bg-tertiary)] border-[var(--border)] text-[var(--text-secondary)]"
                    }`}
                  >
                    <input
                      type="checkbox"
                      checked={config.symbols.includes(sym)}
                      onChange={() => toggleSymbol(sym)}
                      className="sr-only"
                    />
                    {sym}
                  </label>
                ))}
              </div>
            </div>
          </div>

          {/* Strategies */}
          <SectionHeader title="Strategies" />
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
            <div className="grid grid-cols-2 gap-3">
              {STRATEGIES.map((s) => (
                <div
                  key={s.key}
                  className="flex items-center justify-between px-3 py-2 bg-[var(--bg-tertiary)] rounded-lg"
                >
                  <span className="text-sm text-[var(--text-primary)]">
                    {s.label}
                  </span>
                  <Toggle
                    enabled={config.strategies[s.key]}
                    onChange={() => toggleStrategy(s.key)}
                  />
                </div>
              ))}
            </div>
          </div>

          {/* Sizing */}
          <SectionHeader title="Sizing (USD)" />
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
            <div className="grid grid-cols-2 gap-3">
              <InputField
                label="Premarket"
                value={config.sizing.premarket}
                onChange={(v) =>
                  update("sizing", {
                    ...config.sizing,
                    premarket: Number(v) || 0,
                  })
                }
                type="number"
              />
              <InputField
                label="Endgame"
                value={config.sizing.endgame}
                onChange={(v) =>
                  update("sizing", {
                    ...config.sizing,
                    endgame: Number(v) || 0,
                  })
                }
                type="number"
              />
              <InputField
                label="EVCurve"
                value={config.sizing.evcurve}
                onChange={(v) =>
                  update("sizing", {
                    ...config.sizing,
                    evcurve: Number(v) || 0,
                  })
                }
                type="number"
              />
              <InputField
                label="SessionBand"
                value={config.sizing.session_band}
                onChange={(v) =>
                  update("sizing", {
                    ...config.sizing,
                    session_band: Number(v) || 0,
                  })
                }
                type="number"
              />
              <InputField
                label="EVSnipe per hit"
                value={config.sizing.evsnipe_per_hit}
                onChange={(v) =>
                  update("sizing", {
                    ...config.sizing,
                    evsnipe_per_hit: Number(v) || 0,
                  })
                }
                type="number"
              />
            </div>
          </div>

          {/* Strategy Caps */}
          <SectionHeader title="Strategy Caps (Max USD)" />
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
            <div className="grid grid-cols-2 gap-3">
              <InputField
                label="Premarket"
                value={config.caps.premarket}
                onChange={(v) =>
                  update("caps", {
                    ...config.caps,
                    premarket: Number(v) || 0,
                  })
                }
                type="number"
              />
              <InputField
                label="Endgame"
                value={config.caps.endgame}
                onChange={(v) =>
                  update("caps", {
                    ...config.caps,
                    endgame: Number(v) || 0,
                  })
                }
                type="number"
              />
              <InputField
                label="EVCurve"
                value={config.caps.evcurve}
                onChange={(v) =>
                  update("caps", {
                    ...config.caps,
                    evcurve: Number(v) || 0,
                  })
                }
                type="number"
              />
              <InputField
                label="SessionBand"
                value={config.caps.session_band}
                onChange={(v) =>
                  update("caps", {
                    ...config.caps,
                    session_band: Number(v) || 0,
                  })
                }
                type="number"
              />
              <InputField
                label="EVSnipe"
                value={config.caps.evsnipe}
                onChange={(v) =>
                  update("caps", {
                    ...config.caps,
                    evsnipe: Number(v) || 0,
                  })
                }
                type="number"
              />
            </div>
          </div>

          {/* MM Tuning */}
          <SectionHeader title="MM Tuning" />
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
            <div className="grid grid-cols-2 gap-3">
              <InputField
                label="MM Rewards Min Share Multiple"
                value={config.mm_tuning.rewards_min_share_multiple}
                onChange={(v) =>
                  update("mm_tuning", {
                    ...config.mm_tuning,
                    rewards_min_share_multiple: Number(v) || 0,
                  })
                }
                type="number"
              />
              <InputField
                label="MM Sport Quote Size Multiplier"
                value={config.mm_tuning.sport_quote_size_multiplier}
                onChange={(v) =>
                  update("mm_tuning", {
                    ...config.mm_tuning,
                    sport_quote_size_multiplier: Number(v) || 0,
                  })
                }
                type="number"
              />
            </div>
          </div>

          {/* Mode */}
          <SectionHeader title="Mode" />
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
            <div className="flex items-center justify-between">
              <div>
                <span className="text-sm text-[var(--text-primary)]">
                  {config.simulation ? "Dry Run" : "Live Trading"}
                </span>
                <p className="text-xs text-[var(--text-secondary)] mt-0.5">
                  {config.simulation
                    ? "Paper trading mode -- no real orders"
                    : "Real orders will be placed"}
                </p>
              </div>
              <Toggle
                enabled={!config.simulation}
                onChange={(live) => update("simulation", !live)}
              />
            </div>
          </div>

          {/* Advanced */}
          <div className="mt-6">
            <button
              onClick={() => setAdvancedOpen(!advancedOpen)}
              className="flex items-center gap-2 text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors mb-3"
            >
              <svg
                className={`w-4 h-4 transition-transform ${
                  advancedOpen ? "rotate-90" : ""
                }`}
                fill="none"
                stroke="currentColor"
                viewBox="0 0 24 24"
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  strokeWidth={2}
                  d="M9 5l7 7-7 7"
                />
              </svg>
              Advanced
            </button>
            {advancedOpen && (
              <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4 space-y-3">
                <InputField
                  label="Relayer API Key"
                  value={config.relayer_api_key}
                  onChange={(v) => update("relayer_api_key", v)}
                />
                <InputField
                  label="Relayer API Key Address"
                  value={config.relayer_api_key_address}
                  onChange={(v) => update("relayer_api_key_address", v)}
                />
                <InputField
                  label="Remote Signer Token"
                  value={config.remote_signer_token}
                  onChange={(v) => update("remote_signer_token", v)}
                />
              </div>
            )}
          </div>

          {/* Profile Export / Import */}
          <SectionHeader title="Profile" />
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4 space-y-4">
            <div className="flex items-end gap-3">
              <div className="flex-1">
                <label className="block text-xs text-[var(--text-secondary)] mb-1.5">
                  Export Password
                </label>
                <input
                  type="password"
                  value={exportPw}
                  onChange={(e) => setExportPw(e.target.value)}
                  className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-[var(--text-primary)] text-sm outline-none focus:border-[var(--accent)] transition-colors"
                  placeholder="Encryption password"
                />
              </div>
              <button
                onClick={handleExport}
                disabled={!exportPw || !profileId}
                className="px-4 py-2 text-sm rounded-lg bg-[var(--bg-tertiary)] border border-[var(--border)] text-[var(--text-primary)] hover:border-[var(--accent)] transition-colors disabled:opacity-40 disabled:cursor-not-allowed whitespace-nowrap"
              >
                Export Config
              </button>
            </div>
            <div className="border-t border-[var(--border)] pt-4 space-y-3">
              <div>
                <label className="block text-xs text-[var(--text-secondary)] mb-1.5">
                  Import Data
                </label>
                <textarea
                  value={importData}
                  onChange={(e) => setImportData(e.target.value)}
                  rows={3}
                  className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-[var(--text-primary)] text-sm outline-none focus:border-[var(--accent)] transition-colors resize-none font-mono"
                  placeholder="Paste encrypted config data"
                />
              </div>
              <div className="flex items-end gap-3">
                <div className="flex-1">
                  <label className="block text-xs text-[var(--text-secondary)] mb-1.5">
                    Import Password
                  </label>
                  <input
                    type="password"
                    value={importPw}
                    onChange={(e) => setImportPw(e.target.value)}
                    className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-[var(--text-primary)] text-sm outline-none focus:border-[var(--accent)] transition-colors"
                    placeholder="Decryption password"
                  />
                </div>
                <button
                  onClick={handleImport}
                  disabled={!importData || !importPw}
                  className="px-4 py-2 text-sm rounded-lg bg-[var(--bg-tertiary)] border border-[var(--border)] text-[var(--text-primary)] hover:border-[var(--accent)] transition-colors disabled:opacity-40 disabled:cursor-not-allowed whitespace-nowrap"
                >
                  Import Config
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>

      {/* Sticky Save Bar */}
      <div className="fixed bottom-0 left-0 right-0 bg-[var(--bg-secondary)] border-t border-[var(--border)] px-6 py-3 flex items-center justify-between">
        <span className="text-sm text-[var(--text-secondary)]">
          {saveMsg}
        </span>
        <button
          onClick={handleSave}
          disabled={saving}
          className="px-6 py-2 text-sm font-medium rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {saving ? "Saving..." : "Save Configuration"}
        </button>
      </div>
    </div>
  );
}
