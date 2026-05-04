import { useCallback, useEffect, useMemo, useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { open } from "@tauri-apps/plugin-shell";
import { AppShell } from "../components/AppShell";
import { InfoPill } from "../components/InfoPill";
import { LogsDrawer } from "../components/LogsDrawer";
import { OfficialLinks } from "../components/OfficialLinks";
import { ProfileSwitcher, type WalletProfileAction } from "../components/ProfileSwitcher";
import { SectionPanel } from "../components/SectionPanel";
import { StatusBadge } from "../components/StatusBadge";
import { useAppContext } from "../App";
import { useBotStatus } from "../hooks/useBotStatus";
import { useHomeOverview } from "../hooks/useHomeOverview";
import { useWalletSyncStatus } from "../hooks/useWalletSyncStatus";
import { completeDesktopMagicWalletOnboarding } from "../lib/desktop-magic-onboarding";
import {
  DEFAULT_CONFIG,
  VISIBLE_STRATEGIES,
  formatMaybeTime,
  formatUsd,
  mergeConfig,
} from "../lib/desktop-config";
import { OFFICIAL_LINKS } from "../lib/official-links";
import {
  createProfile,
  deleteProfile,
  derivePolymarketFunderAddresses,
  exportConfig,
  getActiveProfileId,
  getDataDirPath,
  getSavedConfig,
  importConfig,
  listProfiles,
  lockSession,
  openLogsFolder,
  runWalletSyncNow,
  saveConfig,
  setActiveProfile,
  type BotConfig,
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
  if (sigType === 3) return "Deposit Wallet";
  return "EOA";
}

function walletModeHelp(sigType: number): string {
  if (sigType === 1) return "Use this if you signed up for Polymarket with email.";
  if (sigType === 2) return "Use this if you signed up for Polymarket with a Web3 wallet.";
  if (sigType === 3) return "Use this for new API users with a deployed deposit wallet.";
  return "Use this if you want to pay gas fees yourself.";
}

const WALLET_MODE_OPTIONS = [
  { value: 1, label: "Proxy Wallet" },
  { value: 2, label: "Safe Wallet" },
  { value: 3, label: "Deposit Wallet" },
  { value: 0, label: "EOA" },
] as const;

function funderAddressLabel(sigType: number): string {
  if (sigType === 3) return "Deposit Wallet Address";
  return "Proxy Wallet Address";
}

function activeFunderAddress(config: BotConfig): string {
  if (config.sig_type === 3) return config.deposit_wallet.trim();
  return config.proxy_wallet.trim();
}

function matchesProxyOrSafe(sigType: number): boolean {
  return sigType === 1 || sigType === 2;
}

function WalletModeSelector({
  value,
  onChange,
}: {
  value: number;
  onChange: (value: number) => void;
}) {
  return (
    <div className="segmented-control" role="radiogroup" aria-label="Wallet Mode">
      {WALLET_MODE_OPTIONS.map((option) => {
        const active = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={active}
            onClick={() => onChange(option.value)}
            className={`segmented-control__option ${
              active ? "segmented-control__option--active" : ""
            }`.trim()}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
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

function uniqueProfileName(baseName: string, profiles: Profile[]): string {
  const base = baseName.slice(0, 40) || "Wallet Profile";
  const existingNames = new Set(profiles.map((profile) => profile.name.toLowerCase()));
  if (!existingNames.has(base.toLowerCase())) {
    return base;
  }
  for (let index = 2; index < 100; index += 1) {
    const candidate = `${base} ${index}`;
    if (!existingNames.has(candidate.toLowerCase())) {
      return candidate;
    }
  }
  return `${base} ${Date.now()}`;
}

function importedProfileName(address: string, profiles: Profile[]): string {
  const trimmed = address.trim();
  const suffix =
    trimmed.length > 12 ? `${trimmed.slice(0, 6)}...${trimmed.slice(-4)}` : trimmed || "Wallet";
  return uniqueProfileName(`Imported ${suffix}`, profiles);
}

function magicProfileName(email: string, profiles: Profile[]): string {
  const localPart = email.split("@")[0]?.trim() || "Wallet";
  return uniqueProfileName(`Magic ${localPart}`, profiles);
}

export function Config() {
  const navigate = useNavigate();
  const location = useLocation();
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
  const [walletProfileMessage, setWalletProfileMessage] = useState<string | null>(null);
  const [createWalletMethod, setCreateWalletMethod] = useState<WalletProfileAction>("magic");
  const [magicEmail, setMagicEmail] = useState("");
  const [magicLoading, setMagicLoading] = useState(false);
  const [importPrivateKey, setImportPrivateKey] = useState("");
  const [importSigType, setImportSigType] = useState("2");
  const [importDepositWallet, setImportDepositWallet] = useState("");
  const [importLoading, setImportLoading] = useState(false);
  const [showImportPrivateKey, setShowImportPrivateKey] = useState(false);
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

  useEffect(() => {
    const state = location.state as { createWalletMethod?: WalletProfileAction } | null;
    if (state?.createWalletMethod === "magic" || state?.createWalletMethod === "private_key") {
      setCreateWalletMethod(state.createWalletMethod);
      setTab("setup");
      setWalletProfileMessage(null);
    }
  }, [location.state]);

  const dirty = useMemo(() => JSON.stringify(config) !== savedSnapshot, [config, savedSnapshot]);
  const enabledStrategies = VISIBLE_STRATEGIES.filter((strategy) => config.strategies[strategy.key]);
  const walletSyncDetails = useMemo(
    () => summarizeWalletSyncResult(walletSyncStatus?.last_result ?? null),
    [walletSyncStatus?.last_result]
  );

  const setupReady = Boolean(
    config.private_key.trim() && (config.sig_type === 0 || activeFunderAddress(config))
  );
  const onboardingReady = Boolean(
    setupReady &&
      config.alpha_key.trim() &&
      (config.relayer_remote_signer_token.trim() || config.remote_signer_token.trim())
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
      const saved = mergeConfig(await getSavedConfig(activeProfileId));
      setConfig(saved);
      setSavedSnapshot(JSON.stringify(saved));
      setSaveMessage(
        saved.alpha_key.trim() && saved.relayer_remote_signer_token.trim()
          ? "Settings saved. Onboarding credentials are ready."
          : "Settings saved."
      );
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

  const saveNewWalletProfile = async ({
    profileName,
    privateKey,
    eoaWallet,
    signatureType,
    proxyWallet,
    depositWallet,
  }: {
    profileName: string;
    privateKey: string;
    eoaWallet: string;
    signatureType: number;
    proxyWallet: string;
    depositWallet: string;
  }) => {
    let createdProfileId: string | null = null;
    let activatedProfile = false;
    try {
      const created = await createProfile(profileName, proxyWallet, signatureType, depositWallet);
      createdProfileId = created.id;
      const saved = mergeConfig(await getSavedConfig(created.id));
      const nextConfig = {
        ...saved,
        private_key: privateKey,
        eoa_wallet: eoaWallet,
        sig_type: signatureType,
        proxy_wallet: proxyWallet,
        deposit_wallet: depositWallet,
        alpha_key: "",
        relayer_remote_signer_token: "",
        relayer_submit_signer_url: "",
        wallet_binding: "",
        onboarding_status: "wallet_saved",
        approval_status: "",
        remote_signer_token: "",
        order_signer_primary_token_internal: "",
      };

      await saveConfig(created.id, nextConfig);
      const persisted = mergeConfig(await getSavedConfig(created.id));
      await setActiveProfile(created.id);
      activatedProfile = true;
      setActiveProfileId(created.id);
      setConfig(persisted);
      setSavedSnapshot(JSON.stringify(persisted));
      await refreshProfiles();
      return persisted;
    } catch (err) {
      if (createdProfileId && !activatedProfile) {
        try {
          await deleteProfile(createdProfileId);
          await refreshProfiles();
        } catch {
          // Best effort cleanup for a profile created during a failed save.
        }
      }
      throw err;
    }
  };

  const handleCreateImportedWalletProfile = async () => {
    const privateKey = importPrivateKey.trim();
    const signatureType = Number(importSigType);
    if (!privateKey) {
      setWalletProfileMessage("Enter a private key.");
      return;
    }
    if (!Number.isFinite(signatureType) || signatureType < 0 || signatureType > 3) {
      setWalletProfileMessage("Choose a valid wallet mode.");
      return;
    }

    setImportLoading(true);
    setWalletProfileMessage(null);
    setSaveMessage(null);
    try {
      const funders = await derivePolymarketFunderAddresses(privateKey);
      const depositWallet = signatureType === 3 ? importDepositWallet.trim() : "";
      const proxyWallet =
        signatureType === 0
          ? ""
          : signatureType === 1
            ? funders.proxy_wallet || ""
            : signatureType === 2
              ? funders.safe_wallet
              : "";
      if (matchesProxyOrSafe(signatureType) && !proxyWallet) {
        setWalletProfileMessage("Could not derive the proxy or safe wallet for this private key.");
        return;
      }
      if (signatureType === 3 && !depositWallet) {
        setWalletProfileMessage("Enter the deployed deposit wallet address.");
        return;
      }
      const profileName = importedProfileName(funders.eoa_wallet, profiles);
      await saveNewWalletProfile({
        profileName,
        privateKey,
        eoaWallet: funders.eoa_wallet,
        signatureType,
        proxyWallet,
        depositWallet,
      });
      setImportPrivateKey("");
      setImportDepositWallet("");
      setShowImportPrivateKey(false);
      const message = "Imported private key profile created and selected. Onboarding credentials are ready.";
      setWalletProfileMessage(message);
      setSaveMessage(message);
    } catch (err) {
      setWalletProfileMessage(getErrorText(err, "failed to import private key profile"));
    } finally {
      setImportLoading(false);
    }
  };

  const handleCreateMagicWalletProfile = async () => {
    if (!magicEmail.trim()) {
      setWalletProfileMessage("Enter an email address.");
      return;
    }

    setMagicLoading(true);
    setWalletProfileMessage(null);
    setSaveMessage(null);
    try {
      const email = magicEmail.trim();
      const result = await completeDesktopMagicWalletOnboarding(email, null);
      if (result.signatureType !== 3) {
        setWalletProfileMessage("Magic bridge did not return a Deposit Wallet account.");
        return;
      }
      if (!result.depositWalletAddress) {
        setWalletProfileMessage("Magic bridge did not return a deposit wallet address.");
        return;
      }
      const funders = await derivePolymarketFunderAddresses(result.privateKey);
      if (
        result.signerAddress &&
        funders.eoa_wallet.trim().toLowerCase() !== result.signerAddress.trim().toLowerCase()
      ) {
        throw new Error("Exported Magic private key does not match the provisioned signer.");
      }

      const profileName = magicProfileName(email, profiles);
      await saveNewWalletProfile({
        profileName,
        privateKey: result.privateKey,
        eoaWallet: funders.eoa_wallet,
        signatureType: 3,
        proxyWallet: "",
        depositWallet: result.depositWalletAddress,
      });
      setMagicEmail("");
      const statusText = result.provisioningStatus
        ? ` Provisioning status: ${toStatusLabel(result.provisioningStatus)}.`
        : "";
      const message = `New Deposit Wallet profile created and selected.${statusText}`;
      setWalletProfileMessage(message);
      setSaveMessage(message);
    } catch (err) {
      setWalletProfileMessage(getErrorText(err, "failed to create Magic wallet profile"));
    } finally {
      setMagicLoading(false);
    }
  };

  const handleCreateProfile = async () => {
    try {
      const signatureType = Number(createSigType);
      const address = createProxy.trim();
      const created = await createProfile(
        createName.trim() || "New Profile",
        matchesProxyOrSafe(signatureType) ? address : "",
        signatureType,
        signatureType === 3 ? address : ""
      );
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

  const handleOpenCreateWallet = (method: WalletProfileAction) => {
    setCreateWalletMethod(method);
    setTab("setup");
    setWalletProfileMessage(null);
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
          <ProfileSwitcher
            activeProfileId={activeProfileId}
            onSwitch={(id) => void handleProfileSwitch(id)}
            onCreateWallet={handleOpenCreateWallet}
          />
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
                  ? "This profile is ready to trade. Save changes any time you update the private key, wallet address, or relayer fields."
                  : "Set the wallet mode, private key, and wallet address when needed. Save will generate EVPOLY alpha and relayer signer credentials automatically."}
              </div>
            </div>

            <div className="page-split xl:grid-cols-[minmax(0,1.2fr)_minmax(20rem,0.8fr)]">
              <SectionPanel
                title="Wallet and Onboarding"
                subtitle="Set the wallet mode, keys, and relayer fields the runtime needs before saving setup."
              >
                <div className="grid gap-4 xl:grid-cols-2">
                  <Field
                    label={funderAddressLabel(config.sig_type)}
                    value={config.sig_type === 3 ? config.deposit_wallet : config.proxy_wallet}
                    onChange={(value) =>
                      setConfig((current) =>
                        current.sig_type === 3
                          ? { ...current, deposit_wallet: value }
                          : { ...current, proxy_wallet: value }
                      )
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
                    <WalletModeSelector
                      value={config.sig_type}
                      onChange={(value) =>
                        setConfig((current) => ({
                          ...current,
                          sig_type: value,
                        }))
                      }
                    />
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
                  </div>
                </SectionPanel>

                <SectionPanel
                  title="Create New Wallet Profile"
                  subtitle="Create a separate local signing profile with email OTP or an existing private key."
                >
                  <div className="space-y-4">
                    {walletProfileMessage ? (
                      <div className="inline-alert inline-alert--warning">{walletProfileMessage}</div>
                    ) : null}
                    <div
                      className="segmented-control"
                      role="radiogroup"
                      aria-label="Wallet profile creation method"
                    >
                      {[
                        ["magic", "Email OTP"],
                        ["private_key", "Private Key"],
                      ].map(([value, label]) => {
                        const active = createWalletMethod === value;
                        return (
                          <button
                            key={value}
                            type="button"
                            role="radio"
                            aria-checked={active}
                            onClick={() => setCreateWalletMethod(value as WalletProfileAction)}
                            className={`segmented-control__option ${
                              active ? "segmented-control__option--active" : ""
                            }`.trim()}
                          >
                            {label}
                          </button>
                        );
                      })}
                    </div>

                    {createWalletMethod === "magic" ? (
                      <>
                        <Field
                          label="Email"
                          value={magicEmail}
                          onChange={setMagicEmail}
                          type="email"
                        />
                        <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-3 text-sm leading-6 text-[var(--text-secondary)]">
                          Magic email OTP creates a new Deposit Wallet profile. The private key is exported locally and saved into this encrypted desktop profile.
                        </div>
                        <button
                          type="button"
                          onClick={() => void handleCreateMagicWalletProfile()}
                          disabled={magicLoading}
                          className="ui-button ui-button--primary"
                        >
                          {magicLoading ? "Creating..." : "Create Wallet with Email OTP"}
                        </button>
                      </>
                    ) : (
                      <>
                        <div>
                          <label className="field-label">Private Key</label>
                          <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-2">
                            <input
                              type={showImportPrivateKey ? "text" : "password"}
                              value={importPrivateKey}
                              onChange={(event) => setImportPrivateKey(event.target.value)}
                              className="field-input"
                              autoComplete="off"
                            />
                            <button
                              type="button"
                              onClick={() => setShowImportPrivateKey((value) => !value)}
                              className="ui-button"
                            >
                              {showImportPrivateKey ? "Hide" : "Show"}
                            </button>
                          </div>
                        </div>
                        <div>
                          <label className="field-label">Wallet Mode</label>
                          <WalletModeSelector
                            value={Number(importSigType)}
                            onChange={(value) => setImportSigType(String(value))}
                          />
                        </div>
                        {Number(importSigType) === 3 ? (
                          <Field
                            label="Deposit Wallet Address"
                            value={importDepositWallet}
                            onChange={setImportDepositWallet}
                            placeholder="0x..."
                          />
                        ) : null}
                        {Number(importSigType) !== 0 ? (
                          <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-3 text-sm leading-6 text-[var(--text-secondary)]">
                            {Number(importSigType) === 3
                              ? "Deposit wallet address must already be deployed, funded, and approved before live trading."
                              : "Proxy/Safe funder address is derived locally during import."}
                          </div>
                        ) : null}
                        <button
                          type="button"
                          onClick={() => void handleCreateImportedWalletProfile()}
                          disabled={importLoading}
                          className="ui-button ui-button--primary"
                        >
                          {importLoading ? "Importing..." : "Import Private Key Profile"}
                        </button>
                      </>
                    )}
                    <div className="text-sm leading-6 text-[var(--text-secondary)]">
                      Creates a separate profile. Existing profiles are not changed.
                    </div>
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
                  label={funderAddressLabel(Number(createSigType))}
                  value={createProxy}
                  onChange={setCreateProxy}
                />
                <div className="text-sm leading-6 text-[var(--text-secondary)]">
                  {Number(createSigType) === 3
                    ? "Use a deployed deposit wallet address for new API user profiles."
                    : "EOA address is derived from the private key during onboarding."}
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
                  <WalletModeSelector
                    value={Number(createSigType)}
                    onChange={(value) => setCreateSigType(String(value))}
                  />
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
          <div className="page-stack">
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

      <LogsDrawer open={logsOpen} onClose={() => setLogsOpen(false)} />
    </AppShell>
  );
}
