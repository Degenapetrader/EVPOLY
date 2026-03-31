import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { open } from "@tauri-apps/plugin-shell";
import { AppShell } from "../components/AppShell";
import { GeoAccessDialog } from "../components/GeoAccessDialog";
import { InfoPill } from "../components/InfoPill";
import { LogsDrawer } from "../components/LogsDrawer";
import { OfficialLinks } from "../components/OfficialLinks";
import { ProfileSwitcher } from "../components/ProfileSwitcher";
import { SectionPanel } from "../components/SectionPanel";
import { StatusBadge } from "../components/StatusBadge";
import { useAppContext } from "../App";
import { useBotStatus } from "../hooks/useBotStatus";
import { useHomeOverview } from "../hooks/useHomeOverview";
import { useWalletSyncStatus } from "../hooks/useWalletSyncStatus";
import {
  DEFAULT_CONFIG,
  STRATEGIES,
  formatMaybeTime,
  formatUsd,
  mergeConfig,
} from "../lib/desktop-config";
import { OFFICIAL_LINKS } from "../lib/official-links";
import {
  createProfile,
  exportConfig,
  getActiveProfileId,
  getDataDirPath,
  getGeoAccessStatus,
  getSavedConfig,
  importConfig,
  listProfiles,
  lockSession,
  openLogsFolder,
  runOnboarding,
  runWalletSyncNow,
  saveConfig,
  setActiveProfile,
  type BotConfig,
  type GeoAccessStatus,
  type OnboardResult,
  type Profile,
} from "../lib/tauri-commands";

type SettingsTab = "setup" | "profiles" | "security" | "diagnostics";

const TAB_LABELS: Record<SettingsTab, string> = {
  setup: "Setup",
  profiles: "Profiles",
  security: "Security",
  diagnostics: "Diagnostics",
};

function Field({
  label,
  value,
  onChange,
  type = "text",
  placeholder,
}: {
  label: string;
  value: string | number;
  onChange: (value: string) => void;
  type?: string;
  placeholder?: string;
}) {
  return (
    <div>
      <label className="field-label">{label}</label>
      <input
        type={type}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder={placeholder}
        className="field-input"
      />
    </div>
  );
}

function getErrorText(err: unknown, fallback: string): string {
  if (typeof err === "string" && err.trim()) return err;
  if (err instanceof Error && err.message.trim()) return err.message;
  return fallback;
}

function walletModeLabel(sigType: number): string {
  if (sigType === 1) return "Proxy Wallet";
  if (sigType === 2) return "Safe Wallet";
  return "EOA";
}

function walletModeHelp(sigType: number): string {
  if (sigType === 1) return "Use this if you signed up for Polymarket with email.";
  if (sigType === 2) return "Use this if you signed up for Polymarket with a Web3 wallet.";
  return "Use this if you want to pay gas fees yourself.";
}

function toStatusLabel(value: string | null | undefined): string {
  if (!value) return "Unknown";
  return value
    .split(/[_\\-\\s]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1).toLowerCase())
    .join(" ");
}

function summarizeWalletSyncResult(result: string | null) {
  if (!result) return [];
  const matches = Array.from(result.matchAll(/([a-z_]+)=([^\\s]+)/g));
  const values = new Map(matches.map((match) => [match[1], match[2]]));

  return [
    values.get("wallet") ? { label: "Wallet", value: values.get("wallet") as string } : null,
    values.get("positions")
      ? { label: "Positions", value: values.get("positions") as string }
      : null,
    values.get("activity") ? { label: "Activity", value: values.get("activity") as string } : null,
    values.get("portfolio_value")
      ? {
          label: "Portfolio",
          value: formatUsd(Number(values.get("portfolio_value"))),
        }
      : null,
    values.get("source") ? { label: "Source", value: values.get("source") as string } : null,
  ].filter((item): item is { label: string; value: string } => Boolean(item));
}

export function Config() {
  const navigate = useNavigate();
  const { activeProfileId, setActiveProfileId, setAuthenticated } = useAppContext();
  const { status } = useBotStatus();
  const { overview } = useHomeOverview();
  const { status: walletSyncStatus, refresh: refreshWalletSync } = useWalletSyncStatus();
  const [tab, setTab] = useState<SettingsTab>("setup");
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [config, setConfig] = useState<BotConfig>(DEFAULT_CONFIG);
  const [configLoaded, setConfigLoaded] = useState(false);
  const [showPrivateKey, setShowPrivateKey] = useState(false);
  const [saveLoading, setSaveLoading] = useState(false);
  const [saveMessage, setSaveMessage] = useState<string | null>(null);
  const [onboardLoading, setOnboardLoading] = useState(false);
  const [onboardResult, setOnboardResult] = useState<OnboardResult | null>(null);
  const [logsOpen, setLogsOpen] = useState(false);
  const [exportPw, setExportPw] = useState("");
  const [importPw, setImportPw] = useState("");
  const [currentDesktopPassword, setCurrentDesktopPassword] = useState("");
  const [importData, setImportData] = useState("");
  const [createName, setCreateName] = useState("New Profile");
  const [createProxy, setCreateProxy] = useState("");
  const [createSigType, setCreateSigType] = useState("1");
  const [dataDir, setDataDir] = useState<string>("");
  const [savedSnapshot, setSavedSnapshot] = useState<string>(JSON.stringify(DEFAULT_CONFIG));
  const [geoDialogStatus, setGeoDialogStatus] = useState<GeoAccessStatus | null>(null);

  const refreshProfiles = useCallback(async () => {
    const nextProfiles = await listProfiles();
    setProfiles(nextProfiles);
  }, []);

  const loadProfileConfig = useCallback(async (profileId: string) => {
    const saved = await getSavedConfig(profileId);
    const merged = mergeConfig(saved);
    setConfig(merged);
    const snapshot = JSON.stringify(merged);
    setSavedSnapshot(snapshot);
    setConfigLoaded(true);
  }, []);

  useEffect(() => {
    void (async () => {
      try {
        const [dir, listed] = await Promise.all([getDataDirPath(), listProfiles()]);
        setDataDir(dir);
        setProfiles(listed);
        const current = await getActiveProfileId();
        if (current) {
          setActiveProfileId(current);
          await loadProfileConfig(current);
        } else {
          setConfigLoaded(true);
        }
      } catch (err) {
        setSaveMessage(getErrorText(err, "failed to load settings"));
        setConfigLoaded(true);
      }
    })();
  }, [loadProfileConfig, setActiveProfileId]);

  const dirty = useMemo(() => JSON.stringify(config) !== savedSnapshot, [config, savedSnapshot]);
  const enabledStrategies = STRATEGIES.filter((strategy) => config.strategies[strategy.key]);
  const walletSyncDetails = useMemo(
    () => summarizeWalletSyncResult(walletSyncStatus?.last_result ?? null),
    [walletSyncStatus?.last_result]
  );

  const setupReady = Boolean(
    config.private_key.trim() && (config.sig_type === 0 || config.proxy_wallet.trim())
  );
  const onboardingReady = Boolean(
    setupReady &&
      (config.remote_signer_token.trim() ||
        config.remote_discovery_token.trim() ||
        config.relayer_api_key.trim())
  );

  const railItems = [
    { label: "Home", to: "/home" },
    { label: "Settings", to: "/settings" },
    { label: "Open Logs", onClick: () => setLogsOpen(true) },
  ];

  const handleProfileSwitch = async (profileId: string) => {
    setActiveProfileId(profileId);
    await loadProfileConfig(profileId);
    await refreshProfiles();
  };

  const handleSave = async () => {
    if (!activeProfileId) {
      setSaveMessage("Create a profile first in the Profiles tab.");
      return;
    }
    setSaveLoading(true);
    setSaveMessage(null);
    try {
      await saveConfig(activeProfileId, config);
      const snapshot = JSON.stringify(config);
      setSavedSnapshot(snapshot);
      setSaveMessage("Settings saved.");
    } catch (err) {
      setSaveMessage(getErrorText(err, "failed to save settings"));
    } finally {
      setSaveLoading(false);
    }
  };

  const handleExport = async () => {
    if (!activeProfileId || !exportPw || !currentDesktopPassword) {
      setSaveMessage("Enter the current desktop password before exporting.");
      return;
    }
    try {
      const data = await exportConfig(activeProfileId, exportPw, currentDesktopPassword);
      await navigator.clipboard.writeText(data);
      setSaveMessage("Encrypted profile copied to clipboard.");
    } catch (err) {
      setSaveMessage(getErrorText(err, "failed to export profile"));
    }
  };

  const handleImport = async () => {
    if (!importData || !importPw || !currentDesktopPassword) {
      setSaveMessage("Enter the current desktop password before importing.");
      return;
    }
    try {
      const importedProfileId = await importConfig(importData, importPw, currentDesktopPassword);
      await setActiveProfile(importedProfileId);
      setActiveProfileId(importedProfileId);
      await loadProfileConfig(importedProfileId);
      await refreshProfiles();
      setSaveMessage("Imported profile activated.");
    } catch (err) {
      setSaveMessage(getErrorText(err, "failed to import profile"));
    }
  };

  const handleLock = async () => {
    try {
      await lockSession();
      setActiveProfileId(null);
      setAuthenticated(false);
      navigate("/");
    } catch (err) {
      setSaveMessage(getErrorText(err, "failed to lock the session"));
    }
  };

  const performRunOnboarding = async () => {
    setOnboardLoading(true);
    setSaveMessage(null);
    try {
      if (!activeProfileId) {
        throw new Error("Create a profile first in the Profiles tab.");
      }
      if (!config.private_key.trim()) {
        throw new Error("Private key is required before onboarding.");
      }
      if ((config.sig_type === 1 || config.sig_type === 2) && !config.proxy_wallet.trim()) {
        throw new Error("Proxy wallet address is required for proxy or safe mode.");
      }
        const result = await runOnboarding(config.private_key, config.sig_type, config.proxy_wallet.trim());
        const nextRemoteSignerToken =
          (typeof result.remote_signer_token === "string" && result.remote_signer_token.trim()) ||
          (typeof result.signer_token === "string" && result.signer_token.trim()) ||
          config.remote_signer_token;
        const nextOrderSignerPrimaryToken =
          (typeof result.order_signer_primary_token === "string" &&
            result.order_signer_primary_token.trim()) ||
          "";
        const nextConfig = {
          ...config,
          eoa_wallet:
            (typeof result.eoa_wallet === "string" && result.eoa_wallet.trim()) || config.eoa_wallet,
          remote_signer_token: nextRemoteSignerToken,
          order_signer_primary_token_internal:
            nextOrderSignerPrimaryToken && nextOrderSignerPrimaryToken !== nextRemoteSignerToken
              ? nextOrderSignerPrimaryToken
              : "",
          remote_discovery_token:
            (typeof result.discovery_token === "string" && result.discovery_token.trim()) ||
            config.remote_discovery_token,
        remote_premarket_alpha_token:
          (typeof result.premarket_alpha_token === "string" &&
            result.premarket_alpha_token.trim()) ||
          config.remote_premarket_alpha_token,
        remote_endgame_alpha_token:
          (typeof result.endgame_alpha_token === "string" &&
            result.endgame_alpha_token.trim()) ||
          config.remote_endgame_alpha_token,
        remote_mm_rewards_alpha_token:
          (typeof result.mm_rewards_alpha_token === "string" &&
            result.mm_rewards_alpha_token.trim()) ||
          config.remote_mm_rewards_alpha_token,
        remote_evsnipe_discovery_token:
          (typeof result.evsnipe_discovery_token === "string" &&
            result.evsnipe_discovery_token.trim()) ||
          config.remote_evsnipe_discovery_token,
        admin_api_token:
          (typeof result.admin_api_token === "string" && result.admin_api_token.trim()) ||
          config.admin_api_token,
      };
      setOnboardResult(result);
      setConfig(nextConfig);
      setSaveLoading(true);
      try {
        await saveConfig(activeProfileId, nextConfig);
        setSavedSnapshot(JSON.stringify(nextConfig));
        await refreshProfiles();
        setSaveMessage("Onboarding finished and saved.");
      } catch (saveErr) {
        setSaveMessage(
          getErrorText(saveErr, "onboarding finished but failed to save the profile")
        );
      } finally {
        setSaveLoading(false);
      }
    } catch (err) {
      setSaveMessage(getErrorText(err, "failed to run onboarding"));
    } finally {
      setOnboardLoading(false);
    }
  };

  const handleRunOnboarding = async () => {
    try {
      const status = await getGeoAccessStatus();
      if (status.status === "blocked") {
        setGeoDialogStatus(status);
        setSaveMessage(status.reason);
        return;
      }
      if (status.status === "unknown") {
        setGeoDialogStatus(status);
        return;
      }
      await performRunOnboarding();
    } catch (err) {
      setSaveMessage(getErrorText(err, "failed to verify access restrictions"));
    }
  };

  const handleCreateProfile = async () => {
    try {
      const created = await createProfile(createName.trim() || "New Profile", createProxy.trim(), Number(createSigType));
      await setActiveProfile(created.id);
      setActiveProfileId(created.id);
      await loadProfileConfig(created.id);
      await refreshProfiles();
      setTab("setup");
      setSaveMessage("Profile created and activated.");
    } catch (err) {
      setSaveMessage(getErrorText(err, "failed to create profile"));
    }
  };

  const handleRunWalletSyncNow = async () => {
    try {
      await runWalletSyncNow();
      await refreshWalletSync();
      setSaveMessage("Wallet sync completed.");
      setTab("diagnostics");
    } catch (err) {
      setSaveMessage(getErrorText(err, "wallet sync failed"));
    }
  };

  return (
    <AppShell
      railSubtitle="BY EVPLUS"
      railLogoSrc="/logo.png"
      railLogoAlt="EVPlus"
      railItems={railItems}
      eyebrow="Settings"
      title="Settings"
      description="Manage wallet setup, profiles, security, logs, and diagnostics."
      meta={
        <div className="flex flex-wrap items-center justify-end gap-3">
          <ProfileSwitcher activeProfileId={activeProfileId} onSwitch={(id) => void handleProfileSwitch(id)} />
          <StatusBadge status={status} />
          <button type="button" onClick={() => void handleLock()} className="ui-button">
            Lock
          </button>
          <button
            type="button"
            onClick={handleSave}
            disabled={saveLoading || !configLoaded}
            className="ui-button ui-button--primary"
          >
            {saveLoading ? "Saving..." : dirty ? "Save Settings" : "Saved"}
          </button>
        </div>
      }
      banner={saveMessage ? <div className="inline-alert inline-alert--warning">{saveMessage}</div> : null}
      contentClassName="page-stack"
    >
      <div className="page-stack">
        <div className="flex flex-wrap gap-2">
          {(["setup", "profiles", "security", "diagnostics"] as SettingsTab[]).map((item) => (
            <button
              key={item}
              type="button"
              onClick={() => setTab(item)}
              className={`section-tab ${tab === item ? "section-tab--active" : ""}`.trim()}
            >
              {TAB_LABELS[item]}
            </button>
          ))}
        </div>

        {tab === "setup" ? (
          <div className="page-stack">
            <div
              className={`status-strip ${
                onboardingReady ? "status-strip--success" : "status-strip--warning"
              }`.trim()}
            >
              <div className="status-strip__title">
                {onboardingReady ? "Onboarding complete" : "Finish wallet setup"}
              </div>
              <div className="status-strip__copy">
                {onboardingReady
                  ? "This profile is ready to trade. Save changes any time you update the private key, proxy wallet, or relayer fields."
                  : "Set the wallet mode, private key, proxy wallet when needed, and required tokens before running onboarding."}
              </div>
            </div>

            <div className="page-split xl:grid-cols-[minmax(0,1.2fr)_minmax(20rem,0.8fr)]">
              <SectionPanel
                title="Wallet and Onboarding"
                subtitle="Set the wallet mode, keys, and relayer fields the runtime needs before you run onboarding."
              >
                <div className="grid gap-4 xl:grid-cols-2">
                  <Field
                    label="Proxy Wallet Address"
                    value={config.proxy_wallet}
                    onChange={(value) =>
                      setConfig((current) => ({ ...current, proxy_wallet: value }))
                    }
                  />

                  <div className="xl:col-span-2">
                    <label className="field-label">Private Key</label>
                    <div className="flex gap-3">
                      <input
                        type={showPrivateKey ? "text" : "password"}
                        value={config.private_key}
                        onChange={(event) =>
                          setConfig((current) => ({
                            ...current,
                            private_key: event.target.value,
                          }))
                        }
                        className="field-input"
                      />
                      <button
                        type="button"
                        onClick={() => setShowPrivateKey((current) => !current)}
                        className="ui-button"
                      >
                        {showPrivateKey ? "Hide" : "Show"}
                      </button>
                    </div>
                  </div>

                  <div>
                    <label className="field-label">Wallet Mode</label>
                    <select
                      value={String(config.sig_type)}
                      onChange={(event) =>
                        setConfig((current) => ({
                          ...current,
                          sig_type: Number(event.target.value),
                        }))
                      }
                      className="field-input"
                    >
                      <option value="1">Proxy Wallet</option>
                      <option value="2">Safe Wallet</option>
                      <option value="0">EOA</option>
                    </select>
                    <div className="metric-detail">{walletModeHelp(config.sig_type)}</div>
                  </div>

                  <Field
                    label="Relayer API Key"
                    value={config.relayer_api_key}
                    onChange={(value) =>
                      setConfig((current) => ({ ...current, relayer_api_key: value }))
                    }
                  />

                  <Field
                    label="Relayer API Key Address"
                    value={config.relayer_api_key_address}
                    onChange={(value) =>
                      setConfig((current) => ({
                        ...current,
                        relayer_api_key_address: value,
                      }))
                    }
                  />
                </div>

                <div className="mt-6 flex flex-wrap gap-3">
                  <div className="w-full rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4 text-sm leading-6 text-[var(--text-secondary)]">
                    <div className="text-sm font-semibold text-[var(--text-primary)]">
                      Support EVPoly with our referral link
                    </div>
                    <div className="mt-2">
                      New to Polymarket? Create your account with EVPoly before onboarding.
                    </div>
                    <div className="mt-1 text-[var(--text-muted)]">
                      Already have a Polymarket account? Skip this step.
                    </div>
                    <div className="mt-4 flex flex-wrap items-center gap-3">
                      <button
                        type="button"
                        onClick={() => void open(OFFICIAL_LINKS.referral)}
                        className="ui-button ui-button--accent"
                      >
                        Open Polymarket
                      </button>
                      <span className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                        Referral link
                      </span>
                    </div>
                  </div>
                  <button
                    type="button"
                    onClick={() => void handleRunOnboarding()}
                    disabled={onboardLoading}
                    className="ui-button"
                  >
                    {onboardLoading ? "Running..." : "Run Onboarding"}
                  </button>
                  <button
                    type="button"
                    onClick={handleSave}
                    disabled={saveLoading || !configLoaded}
                    className="ui-button ui-button--primary"
                  >
                    {saveLoading ? "Saving..." : "Save Setup"}
                  </button>
                </div>
              </SectionPanel>

              <div className="page-stack page-aside">
                <SectionPanel
                  title="Profile readiness"
                  subtitle="Use this to confirm the current profile is fully wired before you trade."
                >
                  <div className="space-y-4">
                    <div className="flex flex-wrap items-center gap-2">
                      <InfoPill tone={setupReady ? "accent" : "warning"}>
                        {setupReady ? "Wallet Ready" : "Wallet Incomplete"}
                      </InfoPill>
                      <InfoPill tone={onboardingReady ? "success" : "warning"}>
                        {onboardingReady ? "Ready to trade" : "Needs onboarding"}
                      </InfoPill>
                    </div>

                    <div className="diagnostics-summary">
                      <div className="diagnostics-summary__item">
                        <div className="diagnostics-summary__label">Wallet mode</div>
                        <div className="diagnostics-summary__value">
                          {walletModeLabel(config.sig_type)}
                        </div>
                      </div>
                      <div className="diagnostics-summary__item">
                        <div className="diagnostics-summary__label">Enabled strategies</div>
                        <div className="diagnostics-summary__value">
                          {enabledStrategies.length}
                        </div>
                      </div>
                      <div className="diagnostics-summary__item">
                        <div className="diagnostics-summary__label">Profile</div>
                        <div className="diagnostics-summary__value">
                          {activeProfileId ? "Loaded" : "Not set"}
                        </div>
                      </div>
                    </div>

                    <div className="text-sm leading-6 text-[var(--text-secondary)]">
                      {enabledStrategies.length > 0
                        ? `Active strategies: ${enabledStrategies
                            .map((strategy) => strategy.label)
                            .join(", ")}.`
                        : "No strategies are enabled yet."}
                    </div>

                    {onboardResult ? (
                      <div className="surface-panel surface-panel--subtle">
                        <div className="surface-panel__body space-y-2">
                          <div className="metric-label">Latest onboarding result</div>
                          <div className="text-sm leading-6 text-[var(--text-secondary)]">
                            Returned fields were merged into the profile and saved automatically.
                          </div>
                          <div className="diagnostics-summary">
                            {[
                              onboardResult.eoa_wallet
                                ? { label: "EOA", value: onboardResult.eoa_wallet }
                                : null,
                              onboardResult.bound_wallet
                                ? {
                                    label: "Bound wallet",
                                    value: String(onboardResult.bound_wallet),
                                  }
                                : null,
                              onboardResult.discovery_token
                                ? { label: "Discovery", value: "Received" }
                                : null,
                            ]
                              .filter(
                                (item): item is { label: string; value: string } =>
                                  Boolean(item)
                              )
                              .map((item) => (
                                <div key={item.label} className="diagnostics-summary__item">
                                  <div className="diagnostics-summary__label">{item.label}</div>
                                  <div className="diagnostics-summary__value">{item.value}</div>
                                </div>
                              ))}
                          </div>
                        </div>
                      </div>
                    ) : null}
                  </div>
                </SectionPanel>
              </div>
            </div>
          </div>
        ) : null}

        {tab === "profiles" ? (
          <div className="page-split xl:grid-cols-[minmax(0,1fr)_minmax(22rem,0.85fr)]">
            <SectionPanel
              title="Profiles"
              subtitle="Switch the active desktop profile or create a new one for another wallet."
            >
              <div className="space-y-3">
                {profiles.length === 0 ? (
                  <div className="empty-state">
                    No profiles yet. Create one on the right to start using EVPoly on this
                    machine.
                  </div>
                ) : (
                  profiles.map((profile) => {
                    const isActive = profile.id === activeProfileId;
                    return (
                      <div key={profile.id} className="strategy-row">
                        <div className="min-w-0">
                          <div className="text-lg font-semibold tracking-[-0.04em] text-[var(--text-primary)]">
                            {profile.name}
                          </div>
                          <div className="mt-1 text-sm text-[var(--text-secondary)]">
                            {profile.wallet_address || "Wallet not set yet"}
                          </div>
                          <div className="mt-2 flex flex-wrap gap-2">
                            <InfoPill tone={isActive ? "success" : "neutral"}>
                              {isActive ? "Active" : "Available"}
                            </InfoPill>
                            <InfoPill>{walletModeLabel(profile.signature_type)}</InfoPill>
                          </div>
                        </div>

                        <button
                          type="button"
                          onClick={() => void handleProfileSwitch(profile.id)}
                          className={`ui-button ${isActive ? "ui-button--accent" : ""}`.trim()}
                          disabled={isActive}
                        >
                          {isActive ? "Selected" : "Open"}
                        </button>
                      </div>
                    );
                  })
                )}
              </div>
            </SectionPanel>

            <SectionPanel
              title="Create profile"
              subtitle="Start another local profile without disturbing the current one."
            >
              <div className="space-y-4">
                <Field label="Profile name" value={createName} onChange={setCreateName} />
                <Field
                  label="Proxy Wallet Address"
                  value={createProxy}
                  onChange={setCreateProxy}
                />
                <div className="text-sm leading-6 text-[var(--text-secondary)]">
                  EOA address is derived from the private key during onboarding.
                </div>
                <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4 text-sm leading-6 text-[var(--text-secondary)]">
                  <div className="text-sm font-semibold text-[var(--text-primary)]">
                    Support EVPoly with our referral link
                  </div>
                  <div className="mt-2">
                    New to Polymarket? Create your account with EVPoly before onboarding.
                  </div>
                  <div className="mt-1 text-[var(--text-muted)]">
                    Already have a Polymarket account? Skip this step.
                  </div>
                  <div className="mt-4 flex flex-wrap items-center gap-3">
                    <button
                      type="button"
                      onClick={() => void open(OFFICIAL_LINKS.referral)}
                      className="ui-button"
                    >
                      Open Polymarket
                    </button>
                    <span className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                      Referral link
                    </span>
                  </div>
                </div>

                <div>
                  <label className="field-label">Wallet Mode</label>
                  <select
                    value={createSigType}
                    onChange={(event) => setCreateSigType(event.target.value)}
                    className="field-input"
                  >
                    <option value="1">Proxy Wallet</option>
                    <option value="2">Safe Wallet</option>
                    <option value="0">EOA</option>
                  </select>
                </div>

                <button
                  type="button"
                  onClick={() => void handleCreateProfile()}
                  className="ui-button ui-button--primary"
                >
                  Create Profile
                </button>
              </div>
            </SectionPanel>
          </div>
        ) : null}

        {tab === "security" ? (
          <div className="page-split xl:grid-cols-[minmax(0,1fr)_minmax(24rem,0.92fr)]">
            <SectionPanel
              title="Runtime tokens"
              subtitle="Keep signer, discovery, and strategy tokens in the active profile. Admin API token is managed internally."
            >
              <div className="grid gap-4 xl:grid-cols-2">
                <Field
                  label="Remote signer token"
                  value={config.remote_signer_token}
                  onChange={(value) =>
                    setConfig((current) => ({ ...current, remote_signer_token: value }))
                  }
                />
                <Field
                  label="Remote discovery token"
                  value={config.remote_discovery_token}
                  onChange={(value) =>
                    setConfig((current) => ({ ...current, remote_discovery_token: value }))
                  }
                />
                <Field
                  label="Premarket alpha token"
                  value={config.remote_premarket_alpha_token}
                  onChange={(value) =>
                    setConfig((current) => ({
                      ...current,
                      remote_premarket_alpha_token: value,
                    }))
                  }
                />
                <Field
                  label="Endgame alpha token"
                  value={config.remote_endgame_alpha_token}
                  onChange={(value) =>
                    setConfig((current) => ({
                      ...current,
                      remote_endgame_alpha_token: value,
                    }))
                  }
                />
                <Field
                  label="MM Rewards alpha token"
                  value={config.remote_mm_rewards_alpha_token}
                  onChange={(value) =>
                    setConfig((current) => ({
                      ...current,
                      remote_mm_rewards_alpha_token: value,
                    }))
                  }
                />
                <Field
                  label="EVSnipe discovery token"
                  value={config.remote_evsnipe_discovery_token}
                  onChange={(value) =>
                    setConfig((current) => ({
                      ...current,
                      remote_evsnipe_discovery_token: value,
                    }))
                  }
                />
              </div>
            </SectionPanel>

            <SectionPanel
              title="Import and export"
              subtitle="Move encrypted profiles between desktops with the current desktop password."
            >
              <div className="space-y-4">
                <Field
                  label="Current desktop password"
                  value={currentDesktopPassword}
                  onChange={setCurrentDesktopPassword}
                  type="password"
                />
                <Field
                  label="Export password"
                  value={exportPw}
                  onChange={setExportPw}
                  type="password"
                />
                <button type="button" onClick={() => void handleExport()} className="ui-button">
                  Copy encrypted export
                </button>

                <Field
                  label="Import password"
                  value={importPw}
                  onChange={setImportPw}
                  type="password"
                />
                <div>
                  <label className="field-label">Encrypted import data</label>
                  <textarea
                    value={importData}
                    onChange={(event) => setImportData(event.target.value)}
                    className="field-textarea mono-data"
                  />
                </div>
                <button
                  type="button"
                  onClick={() => void handleImport()}
                  className="ui-button ui-button--primary"
                >
                  Import and activate
                </button>
              </div>
            </SectionPanel>
          </div>
        ) : null}

        {tab === "diagnostics" ? (
          <div className="page-stack">
            <div className="grid gap-4 xl:grid-cols-[minmax(0,1.1fr)_minmax(0,0.9fr)]">
              <SectionPanel
                title="Wallet Sync Diagnostics"
                subtitle="Background sync health, last run, and a manual trigger when you need it."
              >
                <div className="page-stack">
                  <div className="flex flex-wrap gap-2">
                    <InfoPill
                      tone={
                        walletSyncStatus?.error
                          ? "danger"
                          : walletSyncStatus?.state === "running"
                          ? "success"
                          : "accent"
                      }
                    >
                      {toStatusLabel(walletSyncStatus?.state)}
                    </InfoPill>
                    <InfoPill tone="accent">
                      {walletSyncStatus?.managed ? "Managed" : "Manual"}
                    </InfoPill>
                  </div>

                  <div className="diagnostics-summary">
                    <div className="diagnostics-summary__item">
                      <div className="diagnostics-summary__label">Last run</div>
                      <div className="diagnostics-summary__value">
                        {formatMaybeTime(walletSyncStatus?.last_run_at)}
                      </div>
                    </div>
                    <div className="diagnostics-summary__item">
                      <div className="diagnostics-summary__label">Wallet</div>
                      <div className="diagnostics-summary__value mono-data">
                        {walletSyncStatus?.wallet_address ?? "Not set"}
                      </div>
                    </div>
                    <div className="diagnostics-summary__item">
                      <div className="diagnostics-summary__label">Mode</div>
                      <div className="diagnostics-summary__value">
                        {status === "running" ? "Live managed" : "Idle"}
                      </div>
                    </div>
                  </div>

                  {walletSyncDetails.length ? (
                    <div className="diagnostics-summary">
                      {walletSyncDetails.map((item) => (
                        <div key={item.label} className="diagnostics-summary__item">
                          <div className="diagnostics-summary__label">{item.label}</div>
                          <div className="diagnostics-summary__value">{item.value}</div>
                        </div>
                      ))}
                    </div>
                  ) : null}

                  {walletSyncStatus?.error ? (
                    <div className="inline-alert">{walletSyncStatus.error}</div>
                  ) : null}

                  <div className="flex flex-wrap gap-3">
                    <button
                      type="button"
                      onClick={() => void handleRunWalletSyncNow()}
                      className="ui-button ui-button--accent"
                    >
                      Run Wallet Sync Now
                    </button>
                    <button
                      type="button"
                      onClick={() => setLogsOpen(true)}
                      className="ui-button"
                    >
                      Open Logs Drawer
                    </button>
                    <button
                      type="button"
                      onClick={() => void openLogsFolder()}
                      className="ui-button"
                    >
                      Open Logs Folder
                    </button>
                  </div>
                </div>
              </SectionPanel>

              <SectionPanel
                title="Runtime health"
                subtitle="Ack timing and the local desktop paths behind this profile."
              >
                <div className="page-stack">
                  <div className="diagnostics-summary">
                    <div className="diagnostics-summary__item">
                      <div className="diagnostics-summary__label">Ack latency</div>
                      <div className="diagnostics-summary__value">
                        {overview?.avg_ack_latency_ms !== null &&
                        overview?.avg_ack_latency_ms !== undefined
                          ? `${overview.avg_ack_latency_ms.toFixed(1)} ms`
                          : "--"}
                      </div>
                    </div>
                    <div className="diagnostics-summary__item">
                      <div className="diagnostics-summary__label">Ack samples</div>
                      <div className="diagnostics-summary__value">
                        {overview?.ack_sample_count ?? 0}
                      </div>
                    </div>
                    <div className="diagnostics-summary__item">
                      <div className="diagnostics-summary__label">Recent ack warnings</div>
                      <div className="diagnostics-summary__value">
                        {overview?.ack_warning_count_recent ?? 0}
                      </div>
                    </div>
                    <div className="diagnostics-summary__item">
                      <div className="diagnostics-summary__label">Bot status</div>
                      <div className="diagnostics-summary__value">
                        {toStatusLabel(status)}
                      </div>
                    </div>
                  </div>

                  <div className="surface-panel surface-panel--subtle">
                    <div className="surface-panel__body">
                      <div className="metric-label">Data directory</div>
                      <div className="metric-detail mono-data">{dataDir || "Loading..."}</div>
                    </div>
                  </div>
                </div>
              </SectionPanel>
            </div>
          </div>
        ) : null}

          <SectionPanel
            title="Official links"
            subtitle="Use these links for EVPlus updates, Polymarket signup, repository access, Terms, and the restricted-jurisdictions policy."
          >
            <div className="space-y-4">
              <OfficialLinks includeReferral />
              <div className="text-sm leading-6 text-[var(--text-secondary)]">
                EVPoly may be unavailable in certain restricted jurisdictions due to regulatory,
                sanctions, or platform restrictions.
            </div>
          </div>
        </SectionPanel>
      </div>

      {geoDialogStatus ? (
        <GeoAccessDialog
          status={geoDialogStatus}
          onContinue={
            geoDialogStatus.status === "unknown"
              ? () => {
                  setGeoDialogStatus(null);
                  void performRunOnboarding();
                }
              : undefined
          }
          onClose={() => setGeoDialogStatus(null)}
        />
      ) : null}

      <LogsDrawer open={logsOpen} onClose={() => setLogsOpen(false)} />
    </AppShell>
  );
}
