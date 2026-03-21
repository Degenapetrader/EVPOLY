import { useEffect, useRef, useState } from "react";
import { AppShell } from "../components/AppShell";
import { InfoPill } from "../components/InfoPill";
import { LogsDrawer } from "../components/LogsDrawer";
import { SectionPanel } from "../components/SectionPanel";
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
type SizeKey = keyof BotConfig["sizing"];
type CapKey = keyof BotConfig["caps"];

const STRATEGY_DETAILS: Record<
  StrategyKey,
  { description: string; tone?: "accent" | "success" }
> = {
  premarket: {
    description: "Looks for early pricing moves before the crowd catches up.",
  },
  endgame: {
    description: "Waits for a better late price before taking the trade.",
    tone: "accent",
  },
  evcurve: {
    description: "Trades curve-based setups when the shape lines up cleanly.",
  },
  session_band: {
    description: "Looks for session swings and reversal bands.",
  },
  evsnipe: {
    description: "Takes quick entries only when the setup is strong enough.",
  },
  mm_rewards: {
    description: "Refreshes quotes automatically on reward markets.",
    tone: "success",
  },
  mm_sport: {
    description: "Quotes sports reward markets when you want extra activity.",
  },
};

const SIZE_FIELDS: Array<{
  key: SizeKey;
  label: string;
  help: string;
}> = [
  {
    key: "premarket",
    label: "Premarket size",
    help: "How much to risk on each premarket trade.",
  },
  {
    key: "endgame",
    label: "Endgame size",
    help: "Base size when Endgame finds a fillable exit.",
  },
  {
    key: "evcurve",
    label: "EVCurve size",
    help: "Base size for curve-based entries.",
  },
  {
    key: "session_band",
    label: "SessionBand size",
    help: "Base size for band reversal trades.",
  },
  {
    key: "evsnipe_per_hit",
    label: "EVSnipe per hit",
    help: "How much EVSnipe uses on each triggered entry.",
  },
];

const CAP_FIELDS: Array<{ key: CapKey; label: string }> = [
  { key: "premarket", label: "Premarket cap" },
  { key: "endgame", label: "Endgame cap" },
  { key: "evcurve", label: "EVCurve cap" },
  { key: "session_band", label: "SessionBand cap" },
  { key: "evsnipe", label: "EVSnipe cap" },
];

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

function mergeConfig(saved: Partial<BotConfig>): BotConfig {
  return {
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
  };
}

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

function SummaryRow({
  label,
  value,
  muted,
}: {
  label: string;
  value: string;
  muted?: boolean;
}) {
  return (
    <div className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-3">
      <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
        {label}
      </div>
      <div
        className={`mt-2 text-lg font-semibold tracking-[-0.03em] ${
          muted ? "text-[var(--text-secondary)]" : "text-[var(--text-primary)]"
        }`}
      >
        {value}
      </div>
    </div>
  );
}

function SimpleModeCard({
  title,
  description,
  active,
  onClick,
}: {
  title: string;
  description: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`w-full rounded-[20px] border px-4 py-4 text-left transition-colors ${
        active
          ? "border-[rgba(54,211,153,0.32)] bg-[rgba(20,35,29,0.96)]"
          : "border-[var(--border)] bg-[rgba(16,22,31,0.78)] hover:border-[var(--border-strong)]"
      }`}
    >
      <div className="flex items-start justify-between gap-3">
        <div>
          <div className="text-base font-semibold text-[var(--text-primary)]">{title}</div>
          <div className="mt-1 text-sm text-[var(--text-secondary)]">{description}</div>
        </div>
        <InfoPill tone={active ? "success" : "neutral"}>
          {active ? "Selected" : "Off"}
        </InfoPill>
      </div>
    </button>
  );
}

function StrategyCard({
  label,
  description,
  enabled,
  onToggle,
  tone = "neutral",
}: {
  label: string;
  description: string;
  enabled: boolean;
  onToggle: () => void;
  tone?: "neutral" | "accent" | "success";
}) {
  return (
    <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="text-base font-semibold text-[var(--text-primary)]">{label}</div>
          <div className="mt-1 text-sm text-[var(--text-secondary)]">{description}</div>
        </div>
        <div className="flex items-center gap-3">
          <InfoPill tone={enabled ? tone : "neutral"}>
            {enabled ? "On" : "Off"}
          </InfoPill>
          <Toggle enabled={enabled} onChange={onToggle} />
        </div>
      </div>
    </div>
  );
}

export function Config() {
  const [config, setConfig] = useState<BotConfig>(DEFAULT_CONFIG);
  const [profileId, setProfileId] = useState<string | null>(null);
  const [showPrivateKey, setShowPrivateKey] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveMsg, setSaveMsg] = useState("");
  const [onboardLoading, setOnboardLoading] = useState(false);
  const [onboardResult, setOnboardResult] = useState<OnboardResult | null>(null);
  const [exportPw, setExportPw] = useState("");
  const [importPw, setImportPw] = useState("");
  const [importData, setImportData] = useState("");
  const [logsOpen, setLogsOpen] = useState(false);
  const advancedSectionRef = useRef<HTMLDivElement | null>(null);

  const loadProfileConfig = async (id: string) => {
    const saved = await getSavedConfig(id);
    setConfig(mergeConfig(saved));
  };

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
          await loadProfileConfig(id);
        }
      } catch (err) {
        const message =
          typeof err === "string"
            ? err
            : err instanceof Error
            ? err.message
            : "failed to load profile config";
        setSaveMsg(`Load warning: ${message}`);
      }
    })();
  }, []);

  useEffect(() => {
    if (!advancedOpen) return;
    advancedSectionRef.current?.scrollIntoView({
      behavior: "smooth",
      block: "start",
    });
  }, [advancedOpen]);

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
      config.eoa_wallet.trim(),
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
      const importedProfileId = await importConfig(importData, importPw);
      await setActiveProfile(importedProfileId);
      setProfileId(importedProfileId);
      await loadProfileConfig(importedProfileId);
      setSaveMsg("Config imported and activated");
      setTimeout(() => setSaveMsg(""), 3000);
    } catch (err) {
      setSaveMsg(`Import error: ${err}`);
    }
  };

  const handleRunOnboarding = async () => {
    setOnboardLoading(true);
    setSaveMsg("");
    try {
      const wallet = config.eoa_wallet.trim();
      if (!config.private_key.trim()) {
        throw new Error("Private key is required for onboarding");
      }
      if ((config.sig_type === 1 || config.sig_type === 2) && !config.proxy_wallet.trim()) {
        throw new Error("Proxy Wallet Address is required for signature type 1 or 2");
      }

      const result = await runOnboarding(
        wallet,
        config.private_key,
        config.sig_type,
        config.proxy_wallet
      );
      setOnboardResult(result);

      const updateFields: Partial<BotConfig> = {};
      if (typeof result.eoa_wallet === "string" && result.eoa_wallet.trim()) {
        updateFields.eoa_wallet = result.eoa_wallet.trim();
      } else if (wallet) {
        updateFields.eoa_wallet = wallet;
      }

      const signerToken =
        (result.remote_signer_token as string | undefined) ||
        (result.signer_token as string | undefined) ||
        "";
      if (signerToken) {
        updateFields.remote_signer_token = signerToken;
      }
      if (typeof result.discovery_token === "string" && result.discovery_token.trim()) {
        updateFields.remote_discovery_token = result.discovery_token.trim();
      }
      if (typeof result.premarket_alpha_token === "string" && result.premarket_alpha_token.trim()) {
        updateFields.remote_premarket_alpha_token = result.premarket_alpha_token.trim();
      }
      if (typeof result.endgame_alpha_token === "string" && result.endgame_alpha_token.trim()) {
        updateFields.remote_endgame_alpha_token = result.endgame_alpha_token.trim();
      }
      if (typeof result.mm_rewards_alpha_token === "string" && result.mm_rewards_alpha_token.trim()) {
        updateFields.remote_mm_rewards_alpha_token = result.mm_rewards_alpha_token.trim();
      }
      if (
        typeof result.evsnipe_discovery_token === "string" &&
        result.evsnipe_discovery_token.trim()
      ) {
        updateFields.remote_evsnipe_discovery_token = result.evsnipe_discovery_token.trim();
      }
      if (typeof result.admin_api_token === "string" && result.admin_api_token.trim()) {
        updateFields.admin_api_token = result.admin_api_token.trim();
      }
      if (Object.keys(updateFields).length > 0) {
        setConfig((prev) => ({ ...prev, ...updateFields }));
      }

      setSaveMsg("Onboarding finished. Review values and click Save Configuration.");
      setTimeout(() => setSaveMsg(""), 5000);
    } catch (err) {
      setSaveMsg(`Onboarding error: ${err}`);
    } finally {
      setOnboardLoading(false);
    }
  };

  const onboardedWallet = config.eoa_wallet.trim();
  const enabledStrategies = STRATEGIES.filter((strategy) => config.strategies[strategy.key]);
  const strategySummary =
    enabledStrategies.length > 0
      ? enabledStrategies.map((strategy) => strategy.label).join(" + ")
      : "No strategy selected";
  const primarySizeKey: SizeKey = config.strategies.endgame
    ? "endgame"
    : config.strategies.premarket
    ? "premarket"
    : config.strategies.evcurve
    ? "evcurve"
    : config.strategies.session_band
    ? "session_band"
    : "evsnipe_per_hit";
  const sizeSummary = `$${config.sizing[primarySizeKey]} base size`;
  const setupReady = Boolean(
    config.private_key.trim() && (config.sig_type === 0 || config.proxy_wallet.trim())
  );
  const symbolSummary = config.symbols.join(", ");
  const railItems = [
    { label: "Dashboard", to: "/dashboard" },
    { label: "Manual Trade", to: "/manual" },
    { label: "Settings", to: "/config" },
    { label: "Open Logs", onClick: () => setLogsOpen(true) },
  ];

  return (
    <AppShell
      railSubtitle="Settings"
      railItems={railItems}
      railChildren={
        <SectionPanel title="Keep it simple" subtitle="Most people only need setup, mode, strategy choice, and size.">
          <div className="space-y-3 text-sm text-[var(--text-secondary)]">
            <p>Use advanced settings only when you are fixing a specific issue.</p>
            <div className="flex flex-wrap gap-2">
              <InfoPill tone={setupReady ? "success" : "warning"}>
                {setupReady ? "Setup ready" : "Needs setup"}
              </InfoPill>
              <InfoPill tone={config.simulation ? "warning" : "success"}>
                {config.simulation ? "Dry Run" : "Live"}
              </InfoPill>
            </div>
          </div>
        </SectionPanel>
      }
      eyebrow={profileId ? "Profile ready" : "Setup"}
      title="Setup Your Trading"
      description="Choose how the bot should trade, set your size, and keep the technical details tucked away until you need them."
      meta={
        <>
          <InfoPill tone={config.simulation ? "warning" : "success"}>
            {config.simulation ? "Dry Run" : "Live Trading"}
          </InfoPill>
          {saveMsg ? (
            <InfoPill tone={saveMsg.startsWith("Error") || saveMsg.includes("error") ? "danger" : "accent"}>
              {saveMsg}
            </InfoPill>
          ) : null}
        </>
      }
      contentClassName="page-stack"
    >
      <div className="page-split xl:grid-cols-[minmax(0,1.35fr)_minmax(19rem,0.75fr)]">
        <div className="space-y-[var(--space-6)]">
          <SectionPanel
            title="Easy Setup"
            subtitle="Connect the wallet details the bot needs, then run onboarding once."
            actions={
              <InfoPill tone={setupReady ? "success" : "warning"}>
                {setupReady ? "Ready to onboard" : "Needs wallet details"}
              </InfoPill>
            }
          >
            <div className="grid gap-4 lg:grid-cols-2">
              <div className="lg:col-span-2">
                <div className="flex items-center justify-between gap-3">
                  <label className="block text-xs text-[var(--text-secondary)]">Private key</label>
                  <button
                    type="button"
                    onClick={() => setShowPrivateKey((current) => !current)}
                    className="text-xs text-[var(--text-secondary)] transition-colors hover:text-[var(--text-primary)]"
                  >
                    {showPrivateKey ? "Hide" : "Show"}
                  </button>
                </div>
                <input
                  type={showPrivateKey ? "text" : "password"}
                  value={config.private_key}
                  onChange={(e) => update("private_key", e.target.value)}
                  placeholder="Paste your private key"
                  className="mt-1.5 w-full rounded-[16px] border border-[var(--border)] bg-[var(--bg-tertiary)] px-4 py-3 text-sm text-[var(--text-primary)] outline-none transition-colors focus:border-[var(--accent)]"
                />
              </div>
              <InputField
                label="Proxy wallet address"
                value={config.proxy_wallet}
                onChange={(v) => update("proxy_wallet", v)}
                placeholder="0x..."
              />
              <div>
                <label className="mb-1.5 block text-xs text-[var(--text-secondary)]">Wallet mode</label>
                <select
                  value={config.sig_type}
                  onChange={(e) => update("sig_type", Number(e.target.value))}
                  className="w-full rounded-[16px] border border-[var(--border)] bg-[var(--bg-tertiary)] px-4 py-3 text-sm text-[var(--text-primary)] outline-none transition-colors focus:border-[var(--accent)]"
                >
                  <option value={1}>Proxy wallet</option>
                  <option value={2}>Safe wallet</option>
                  <option value={0}>EOA wallet</option>
                </select>
              </div>
              <InputField
                label="Relayer API key"
                value={config.relayer_api_key}
                onChange={(v) => update("relayer_api_key", v)}
              />
              <InputField
                label="Relayer API key address"
                value={config.relayer_api_key_address}
                onChange={(v) => update("relayer_api_key_address", v)}
                placeholder="0x..."
              />
            </div>
            <div className="mt-5 grid gap-4 lg:grid-cols-[minmax(0,1fr)_auto] lg:items-end">
              <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4">
                <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                  Onboarded wallet
                </div>
                <div className="mt-2 break-all font-mono text-sm text-[var(--text-primary)]">
                  {onboardedWallet || "Not onboarded yet. Run setup to derive the wallet from your key."}
                </div>
              </div>
              <button
                type="button"
                onClick={handleRunOnboarding}
                disabled={onboardLoading}
                className="ui-button ui-button--primary min-w-[12rem] justify-center"
              >
                {onboardLoading ? "Running setup..." : onboardedWallet ? "Run setup again" : "Run setup"}
              </button>
            </div>
            {onboardResult ? (
              <details className="mt-4 rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.62)] px-4 py-3">
                <summary className="cursor-pointer text-sm font-medium text-[var(--text-primary)]">
                  View onboarding details
                </summary>
                <pre className="mt-3 max-h-56 overflow-auto rounded-[14px] border border-[var(--border)] bg-[var(--bg-tertiary)] p-3 text-xs text-[var(--text-secondary)]">
                  {JSON.stringify(onboardResult, null, 2)}
                </pre>
              </details>
            ) : null}
          </SectionPanel>

          <SectionPanel
            title="Trading Rules"
            subtitle="Pick the mode, strategies, and size that match how active you want the bot to be."
          >
            <div className="grid gap-4 lg:grid-cols-2">
              <SimpleModeCard
                title="Live Trading"
                description="Places real orders and manages them for you."
                active={!config.simulation}
                onClick={() => update("simulation", false)}
              />
              <SimpleModeCard
                title="Dry Run"
                description="Lets you watch the bot behave without placing real orders."
                active={config.simulation}
                onClick={() => update("simulation", true)}
              />
            </div>

            <div className="mt-5">
              <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                Strategies
              </div>
              <div className="mt-3 grid gap-3">
                {STRATEGIES.map((strategy) => (
                  <StrategyCard
                    key={strategy.key}
                    label={strategy.label}
                    description={STRATEGY_DETAILS[strategy.key].description}
                    enabled={config.strategies[strategy.key]}
                    tone={STRATEGY_DETAILS[strategy.key].tone}
                    onToggle={() => toggleStrategy(strategy.key)}
                  />
                ))}
              </div>
            </div>

            <div className="mt-5">
              <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                Base trade sizes
              </div>
              <div className="mt-3 grid gap-4 md:grid-cols-2">
                {SIZE_FIELDS.map((field) => (
                  <div
                    key={field.key}
                    className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4"
                  >
                    <InputField
                      label={field.label}
                      value={config.sizing[field.key]}
                      onChange={(v) =>
                        update("sizing", {
                          ...config.sizing,
                          [field.key]: Number(v) || 0,
                        })
                      }
                      type="number"
                    />
                    <div className="mt-2 text-xs text-[var(--text-secondary)]">{field.help}</div>
                  </div>
                ))}
              </div>
            </div>
          </SectionPanel>

          <SectionPanel
            title="Profile Tools"
            subtitle="Back up your settings or move them to another machine when you need to."
          >
            <div className="grid gap-4 xl:grid-cols-2">
              <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4">
                <div className="text-sm font-semibold text-[var(--text-primary)]">Export profile</div>
                <div className="mt-1 text-sm text-[var(--text-secondary)]">
                  Copy your encrypted config to the clipboard using a password you choose.
                </div>
                <div className="mt-4 space-y-3">
                  <InputField
                    label="Export password"
                    value={exportPw}
                    onChange={setExportPw}
                    type="password"
                    placeholder="Create an export password"
                  />
                  <button
                    type="button"
                    onClick={handleExport}
                    disabled={!exportPw || !profileId}
                    className="ui-button w-full justify-center"
                  >
                    Export Config
                  </button>
                </div>
              </div>

              <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4">
                <div className="text-sm font-semibold text-[var(--text-primary)]">Import profile</div>
                <div className="mt-1 text-sm text-[var(--text-secondary)]">
                  Paste encrypted config data and unlock it with the matching password.
                </div>
                <div className="mt-4 space-y-3">
                  <div>
                    <label className="mb-1.5 block text-xs text-[var(--text-secondary)]">Import data</label>
                    <textarea
                      value={importData}
                      onChange={(e) => setImportData(e.target.value)}
                      rows={4}
                      className="w-full resize-none rounded-[16px] border border-[var(--border)] bg-[var(--bg-tertiary)] px-4 py-3 font-mono text-sm text-[var(--text-primary)] outline-none transition-colors focus:border-[var(--accent)]"
                      placeholder="Paste encrypted config data"
                    />
                  </div>
                  <InputField
                    label="Import password"
                    value={importPw}
                    onChange={setImportPw}
                    type="password"
                    placeholder="Enter the import password"
                  />
                  <button
                    type="button"
                    onClick={handleImport}
                    disabled={!importData || !importPw}
                    className="ui-button w-full justify-center"
                  >
                    Import Config
                  </button>
                </div>
              </div>
            </div>
          </SectionPanel>

          <div ref={advancedSectionRef}>
            <SectionPanel
            title="Advanced"
            subtitle="Only open this if you need raw tokens, market scope, or extra limits."
            actions={
              <button
                type="button"
                onClick={() => setAdvancedOpen((current) => !current)}
                className="ui-button"
              >
                {advancedOpen ? "Hide advanced" : "Open advanced"}
              </button>
            }
          >
            {advancedOpen ? (
              <div className="space-y-5">
                <div>
                  <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                    Market scope
                  </div>
                  <div className="mt-3 flex flex-wrap gap-2">
                    {CORE_SYMBOLS.map((symbol) => (
                      <label
                        key={symbol}
                        className={`rounded-full border px-3 py-1.5 text-sm transition-colors ${
                          config.symbols.includes(symbol)
                            ? "border-[rgba(54,211,153,0.28)] bg-[rgba(20,35,29,0.94)] text-[var(--text-primary)]"
                            : "border-[var(--border)] bg-[rgba(16,22,31,0.78)] text-[var(--text-secondary)]"
                        } ${symbol === "BTC" ? "cursor-not-allowed opacity-70" : "cursor-pointer"}`}
                      >
                        <input
                          type="checkbox"
                          checked={config.symbols.includes(symbol)}
                          onChange={() => toggleSymbol(symbol)}
                          disabled={symbol === "BTC"}
                          className="sr-only"
                        />
                        {symbol}
                      </label>
                    ))}
                    {EXTRA_SYMBOLS.map((symbol) => (
                      <label
                        key={symbol}
                        className={`cursor-pointer rounded-full border px-3 py-1.5 text-sm transition-colors ${
                          config.symbols.includes(symbol)
                            ? "border-[rgba(73,116,255,0.32)] bg-[rgba(24,29,44,0.94)] text-[var(--text-primary)]"
                            : "border-[var(--border)] bg-[rgba(16,22,31,0.78)] text-[var(--text-secondary)]"
                        }`}
                      >
                        <input
                          type="checkbox"
                          checked={config.symbols.includes(symbol)}
                          onChange={() => toggleSymbol(symbol)}
                          className="sr-only"
                        />
                        {symbol}
                      </label>
                    ))}
                  </div>
                </div>

                <div className="grid gap-4 md:grid-cols-2">
                  <InputField
                    label="Remote signer token"
                    value={config.remote_signer_token}
                    onChange={(v) => update("remote_signer_token", v)}
                  />
                  <InputField
                    label="Remote discovery token"
                    value={config.remote_discovery_token}
                    onChange={(v) => update("remote_discovery_token", v)}
                  />
                  <InputField
                    label="Premarket alpha token"
                    value={config.remote_premarket_alpha_token}
                    onChange={(v) => update("remote_premarket_alpha_token", v)}
                  />
                  <InputField
                    label="Endgame alpha token"
                    value={config.remote_endgame_alpha_token}
                    onChange={(v) => update("remote_endgame_alpha_token", v)}
                  />
                  <InputField
                    label="MM rewards alpha token"
                    value={config.remote_mm_rewards_alpha_token}
                    onChange={(v) => update("remote_mm_rewards_alpha_token", v)}
                  />
                  <InputField
                    label="EVSnipe discovery token"
                    value={config.remote_evsnipe_discovery_token}
                    onChange={(v) => update("remote_evsnipe_discovery_token", v)}
                  />
                  <InputField
                    label="Admin API token"
                    value={config.admin_api_token}
                    onChange={(v) => update("admin_api_token", v)}
                  />
                </div>

                <div className="grid gap-4 md:grid-cols-2">
                  {CAP_FIELDS.map((field) => (
                    <InputField
                      key={field.key}
                      label={field.label}
                      value={config.caps[field.key]}
                      onChange={(v) =>
                        update("caps", {
                          ...config.caps,
                          [field.key]: Number(v) || 0,
                        })
                      }
                      type="number"
                    />
                  ))}
                  <InputField
                    label="MM rewards minimum share multiple"
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
                    label="MM sport quote size multiplier"
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
            ) : (
              <div className="text-sm text-[var(--text-secondary)]">
                Advanced fields stay out of the way until you need them for deeper tuning or troubleshooting.
              </div>
            )}
            </SectionPanel>
          </div>
        </div>

        <div className="page-aside space-y-[var(--space-6)] xl:sticky xl:top-[var(--space-6)]">
          <SectionPanel
            title="Before You Save"
            subtitle="A quick plain-English summary of what this profile will do."
          >
            <div className="grid gap-3">
              <SummaryRow label="Mode" value={config.simulation ? "Dry Run" : "Live Trading"} />
              <SummaryRow
                label="Main strategies"
                value={strategySummary}
                muted={enabledStrategies.length === 0}
              />
              <SummaryRow label="Trade size" value={sizeSummary} />
              <SummaryRow
                label="Wallet mode"
                value={
                  config.sig_type === 1
                    ? "Proxy wallet"
                    : config.sig_type === 2
                    ? "Safe wallet"
                    : "EOA wallet"
                }
              />
              <SummaryRow
                label="Wallet"
                value={onboardedWallet || "Needs onboarding"}
                muted={!onboardedWallet}
              />
              <SummaryRow label="Markets" value={symbolSummary} />
            </div>

            <div className="mt-5 rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4">
              <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">Message</div>
              <div className="mt-2 text-sm text-[var(--text-secondary)]">
                {saveMsg ||
                  "Save after you finish setup. Advanced fields are optional unless you know you need them."}
              </div>
            </div>

            <div className="mt-5 space-y-3">
              <button
                type="button"
                onClick={handleSave}
                disabled={saving}
                className="ui-button ui-button--primary w-full justify-center"
              >
                {saving ? "Saving..." : "Save Settings"}
              </button>
              <button
                type="button"
                onClick={() => setAdvancedOpen((current) => !current)}
                className="ui-button w-full justify-center"
              >
                {advancedOpen ? "Hide Advanced Settings" : "Open Advanced Settings"}
              </button>
            </div>
          </SectionPanel>

          <SectionPanel title="What this means" subtitle="The page should stay readable, calm, and fullscreen-safe.">
            <div className="space-y-3 text-sm text-[var(--text-secondary)]">
              <p>
                The main screen shows only the choices most users make often: setup, mode, strategies,
                and sizing.
              </p>
              <p>
                Raw tokens, market scope, and deeper caps stay hidden until you open Advanced.
              </p>
            </div>
          </SectionPanel>
        </div>
      </div>
      <LogsDrawer open={logsOpen} mode="bot" onClose={() => setLogsOpen(false)} />
    </AppShell>
  );
}
