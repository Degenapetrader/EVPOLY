import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { check } from "@tauri-apps/plugin-updater";
import { AppShell } from "../components/AppShell";
import { GeoAccessDialog } from "../components/GeoAccessDialog";
import { LogsDrawer } from "../components/LogsDrawer";
import { ProfileSwitcher } from "../components/ProfileSwitcher";
import { SectionPanel } from "../components/SectionPanel";
import { SetupDoctorDialog } from "../components/SetupDoctorDialog";
import { StatusBadge } from "../components/StatusBadge";
import { StrategyEditorPane } from "../components/StrategyEditorPane";
import { UpdateBanner } from "../components/UpdateBanner";
import { useAppContext } from "../App";
import { useHomeActivity } from "../hooks/useHomeActivity";
import { useHomeOverview } from "../hooks/useHomeOverview";
import {
  STRATEGIES,
  DEFAULT_CONFIG,
  formatUsd,
  formatEndgameSplitTooltip,
  mergeConfig,
  parseNonNegative,
  setEVSnipePreHitEnabled,
  strategyControlSuffix,
  strategyControlTooltip,
  strategySizeLabel,
  strategySizeValue,
  strategyTooltip,
  updateStrategyEnabled,
  updateStrategySize,
  type StrategyKey,
} from "../lib/desktop-config";
import {
  getGeoAccessStatus,
  getActiveProfileId,
  getSavedConfig,
  lockSession,
  restartBot,
  runSetupDoctor,
  saveConfig,
  startBot,
  stopBot,
  type BotConfig,
  type GeoAccessStatus,
  type SetupDoctorResult,
} from "../lib/tauri-commands";

function getErrorText(err: unknown, fallback: string): string {
  if (typeof err === "string" && err.trim()) return err;
  if (err instanceof Error && err.message.trim()) return err.message;
  return fallback;
}

function formatRelativeTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const diffSeconds = Math.round((date.getTime() - Date.now()) / 1000);
  const abs = Math.abs(diffSeconds);
  const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

  if (abs < 60) return rtf.format(diffSeconds, "second");
  if (abs < 3600) return rtf.format(Math.round(diffSeconds / 60), "minute");
  if (abs < 86400) return rtf.format(Math.round(diffSeconds / 3600), "hour");
  return rtf.format(Math.round(diffSeconds / 86400), "day");
}

function formatControlValue(value: number): string {
  return new Intl.NumberFormat("en-US", {
    maximumFractionDigits: value % 1 === 0 ? 0 : 2,
  }).format(value);
}

function activityActionClass(action?: string | null): "buy" | "sell" {
  return action?.toLowerCase() === "sold" ? "sell" : "buy";
}

function activityOutcomeClass(outcome?: string | null): "positive" | "negative" | "neutral" {
  const lower = outcome?.toLowerCase() ?? "";
  if (lower.startsWith("yes") || lower.startsWith("up")) return "positive";
  if (lower.startsWith("no") || lower.startsWith("down")) return "negative";
  return "neutral";
}

function activityValueClass(value?: number | null): "positive" | "negative" | "neutral" {
  if (typeof value !== "number" || !Number.isFinite(value) || value === 0) return "neutral";
  return value > 0 ? "positive" : "negative";
}

function strategyKeyFromRoute(strategySlug?: string): StrategyKey | null {
  if (!strategySlug) return null;
  return (
    STRATEGIES.find((strategy) => strategy.key === strategySlug)?.key ?? null
  );
}

const WEEKEND_POLICY_TOOLTIP_PAUSE =
  "Stops new weekend entries for Premarket, Endgame, EVCurve, and SessionBand.";
const WEEKEND_POLICY_TOOLTIP_OFF =
  "Premarket, Endgame, EVCurve, and SessionBand keep trading on weekends.";

export function Home() {
  const navigate = useNavigate();
  const { strategySlug } = useParams();
  const { activeProfileId, setActiveProfileId, setAuthenticated } = useAppContext();
  const { overview, error: overviewError, refresh: refreshOverview } = useHomeOverview();
  const { items, error: activityError, refresh: refreshActivity } = useHomeActivity(14);
  const [config, setConfig] = useState<BotConfig>(DEFAULT_CONFIG);
  const [configLoaded, setConfigLoaded] = useState(false);
  const [actionLoading, setActionLoading] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [saveLoading, setSaveLoading] = useState(false);
  const [saveMessage, setSaveMessage] = useState<string | null>(null);
  const [logsOpen, setLogsOpen] = useState(false);
  const [updateVersion, setUpdateVersion] = useState<string | null>(null);
  const [updateDownloading, setUpdateDownloading] = useState(false);
  const [pendingUpdate, setPendingUpdate] =
    useState<Awaited<ReturnType<typeof check>> | null>(null);
  const [savedSnapshot, setSavedSnapshot] = useState<string>(JSON.stringify(DEFAULT_CONFIG));
  const [geoDialogStatus, setGeoDialogStatus] = useState<GeoAccessStatus | null>(null);
  const [pendingGeoAction, setPendingGeoAction] = useState<"start" | "restart" | null>(null);
  const [doctorLoading, setDoctorLoading] = useState(false);
  const [doctorResult, setDoctorResult] = useState<SetupDoctorResult | null>(null);
  const [doctorDialogOpen, setDoctorDialogOpen] = useState(false);

  const selectedStrategy = useMemo(
    () => strategyKeyFromRoute(strategySlug),
    [strategySlug]
  );
  const selectedStrategyMeta = useMemo(
    () => STRATEGIES.find((strategy) => strategy.key === selectedStrategy) ?? null,
    [selectedStrategy]
  );

  useEffect(() => {
    if (strategySlug && !selectedStrategy) {
      navigate("/home", { replace: true });
    }
  }, [navigate, selectedStrategy, strategySlug]);

  const loadProfileConfig = useCallback(async (profileId: string) => {
    const saved = await getSavedConfig(profileId);
    const merged = mergeConfig(saved);
    setConfig(merged);
    const snapshot = JSON.stringify(merged);
    setSavedSnapshot(snapshot);
    setConfigLoaded(true);
  }, []);

  useEffect(() => {
    getActiveProfileId()
      .then(async (id) => {
        setActiveProfileId(id);
        if (id) {
          await loadProfileConfig(id);
        } else {
          setConfigLoaded(true);
        }
      })
      .catch((err) => {
        setActionError(getErrorText(err, "failed to load the active profile"));
        setConfigLoaded(true);
      });
  }, [loadProfileConfig, setActiveProfileId]);

  useEffect(() => {
    void (async () => {
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

  const dirty = useMemo(() => JSON.stringify(config) !== savedSnapshot, [config, savedSnapshot]);
  const displayError = actionError || overviewError || activityError;
  const canOperate = Boolean(activeProfileId && configLoaded);
  const botRunning = overview?.bot_state === "running";
  const activityItems = useMemo(
    () => [...items].sort((left, right) => right.timestamp.localeCompare(left.timestamp)),
    [items]
  );

  const handleUpdate = async () => {
    if (!pendingUpdate || updateDownloading) return;
    setUpdateDownloading(true);
    try {
      await pendingUpdate.downloadAndInstall();
      setPendingUpdate(null);
      setUpdateVersion(null);
    } finally {
      setUpdateDownloading(false);
    }
  };

  const handleProfileSwitch = async (profileId: string) => {
    setActiveProfileId(profileId);
    await loadProfileConfig(profileId);
    await refreshOverview();
    await refreshActivity({ reset: true });
  };

  const handleLock = async () => {
    try {
      await lockSession();
      setActiveProfileId(null);
      setAuthenticated(false);
      navigate("/");
    } catch (err) {
      setActionError(getErrorText(err, "failed to lock the session"));
    }
  };

  const handleSave = async () => {
    if (!activeProfileId) {
      setSaveMessage("Open Settings to finish the first profile setup.");
      return;
    }
    setSaveLoading(true);
    setSaveMessage(null);
    try {
      await saveConfig(activeProfileId, config);
      const snapshot = JSON.stringify(config);
      setSavedSnapshot(snapshot);
      setSaveMessage("Changes saved.");
      await refreshOverview();
    } catch (err) {
      setSaveMessage(getErrorText(err, "failed to save changes"));
    } finally {
      setSaveLoading(false);
    }
  };

  const performStart = async () => {
    if (!activeProfileId) {
      setActionError("Open Settings and create a profile before starting the bot.");
      return;
    }
    setActionLoading(true);
    try {
      if (dirty) {
        await saveConfig(activeProfileId, config);
        setSavedSnapshot(JSON.stringify(config));
      }
      await startBot(false);
      setActionError(null);
      await refreshOverview();
      await refreshActivity({ reset: true });
    } catch (err) {
      setActionError(getErrorText(err, "failed to start bot"));
    } finally {
      setActionLoading(false);
    }
  };

  const performRestart = async () => {
    if (!activeProfileId) {
      setActionError("Open Settings and create a profile before restarting the bot.");
      return;
    }
    setActionLoading(true);
    try {
      if (dirty) {
        await saveConfig(activeProfileId, config);
        setSavedSnapshot(JSON.stringify(config));
      }
      await restartBot(false);
      setActionError(null);
      await refreshOverview();
      await refreshActivity({ reset: true });
    } catch (err) {
      setActionError(getErrorText(err, "failed to restart bot"));
    } finally {
      setActionLoading(false);
    }
  };

  const promptGeoIfNeeded = async (action: "start" | "restart") => {
    try {
      const status = await getGeoAccessStatus();
      if (status.status === "allowed") {
        return false;
      }
      setPendingGeoAction(action);
      setGeoDialogStatus(status);
      if (status.status === "blocked") {
        setActionError(status.reason);
      }
      return true;
    } catch (err) {
      setActionError(getErrorText(err, "failed to verify access restrictions"));
      return true;
    }
  };

  const handleStart = async () => {
    if (await promptGeoIfNeeded("start")) {
      return;
    }
    await performStart();
  };

  const handleRestart = async () => {
    if (await promptGeoIfNeeded("restart")) {
      return;
    }
    await performRestart();
  };

  const handleStop = async () => {
    setActionLoading(true);
    try {
      await stopBot();
      setActionError(null);
      await refreshOverview();
      await refreshActivity({ reset: true });
    } catch (err) {
      setActionError(getErrorText(err, "failed to stop bot"));
    } finally {
      setActionLoading(false);
    }
  };

  const handleRunSetupDoctor = async () => {
    setDoctorLoading(true);
    try {
      const result = await runSetupDoctor();
      setDoctorResult(result);
      setDoctorDialogOpen(true);
      setActionError(null);
      await refreshOverview();
      await refreshActivity({ reset: true });
      if (activeProfileId) {
        await loadProfileConfig(activeProfileId);
      }
    } catch (err) {
      setDoctorResult({
        status: "failed",
        items: [],
        fixed_count: 0,
        missing_user_count: 0,
        bot_was_running: botRunning,
        bot_restarted: false,
        popup: {
          title: "Doctor failed",
          body: getErrorText(err, "Setup Doctor could not complete right now."),
          cta_label: "Open Setup",
          cta_target: "setup",
        },
      });
      setDoctorDialogOpen(true);
    } finally {
      setDoctorLoading(false);
    }
  };

  const railItems = [
    { label: "Home", to: "/home" },
    { label: "Settings", to: "/settings" },
    { label: "Open Logs", onClick: () => setLogsOpen(true) },
  ];

  const renderStrategyList = () => (
    <div className="strategy-rail">
      <div className="strategy-rail__title-row">
        <div className="strategy-rail__title">Strategy List</div>
        <button
          type="button"
          onClick={() =>
            setConfig((current) => ({
              ...current,
              weekend_policy: current.weekend_policy === "pause" ? "off" : "pause",
            }))
          }
          disabled={!canOperate}
          className={`ui-button ui-button--compact strategy-rail__title-action ${
            config.weekend_policy === "pause" ? "ui-button--accent" : ""
          }`.trim()}
          title={
            config.weekend_policy === "pause"
              ? WEEKEND_POLICY_TOOLTIP_PAUSE
              : WEEKEND_POLICY_TOOLTIP_OFF
          }
          aria-pressed={config.weekend_policy === "pause"}
        >
          {config.weekend_policy === "pause" ? "Weekend" : "NO OFF DAY"}
        </button>
      </div>
      <div className="strategy-rail__header" aria-hidden="true">
        <span>Strategy</span>
        <span>State</span>
        <span>Control</span>
      </div>

      <div className="strategy-rail__list">
        {STRATEGIES.map((strategy) => {
          const enabled = config.strategies[strategy.key];
          const value = strategySizeValue(config, strategy.key);
          const selected = strategy.key === selectedStrategy;
          const suffix = strategyControlSuffix(strategy.key, config);
          const controlTitle = strategyControlTooltip(config, strategy.key);
          const showPreHitRow = strategy.key === "evsnipe";
          const preHitEnabled = config.strategy_settings.evsnipe.pre_hit_enabled;

          return (
            <div
              key={strategy.key}
              className={`strategy-rail__group ${
                selected ? "strategy-rail__group--active" : ""
              }`.trim()}
            >
              <div
                className={`strategy-rail__row ${
                  selected ? "strategy-rail__row--active" : ""
                }`.trim()}
              >
                <button
                  type="button"
                  onClick={() => navigate(`/home/${strategy.key}`)}
                  className="strategy-rail__link"
                  title={strategyTooltip(strategy.key)}
                >
                  <div className="strategy-rail__label">{strategy.label}</div>
                </button>

                <button
                  type="button"
                  onClick={() =>
                    setConfig((current) =>
                      updateStrategyEnabled(current, strategy.key, !enabled)
                    )
                  }
                  disabled={!canOperate}
                  className={`ui-button ui-button--compact ${
                    enabled ? "ui-button--accent" : ""
                  }`.trim()}
                >
                  {enabled ? "On" : "Off"}
                </button>

                <div className="strategy-rail__field" title={controlTitle}>
                  <input
                    type="number"
                    min="0"
                    step="0.1"
                    value={value}
                    aria-label={`${strategy.label} ${strategySizeLabel(strategy.key, config)}`}
                    onChange={(event) =>
                      setConfig((current) =>
                        updateStrategySize(
                          current,
                          strategy.key,
                          parseNonNegative(event.target.value, value)
                        )
                      )
                    }
                    disabled={!canOperate}
                    className="field-input field-input--compact"
                    title={
                      strategy.key === "endgame"
                        ? `Split ${formatEndgameSplitTooltip(config)}`
                        : controlTitle
                    }
                  />
                  <span className="strategy-rail__field-suffix">{suffix}</span>
                </div>
              </div>

              {showPreHitRow ? (
                <div className="strategy-rail__subrow">
                  <button
                    type="button"
                    onClick={() => navigate(`/home/${strategy.key}`)}
                    className="strategy-rail__subrow-label"
                    title="Fast pre-hit entries before the full hit leg is live."
                  >
                    Pre-hit
                  </button>
                  <button
                    type="button"
                    onClick={() =>
                      setConfig((current) => setEVSnipePreHitEnabled(current, !preHitEnabled))
                    }
                    disabled={!canOperate}
                    className={`ui-button ui-button--compact ${
                      preHitEnabled ? "ui-button--accent" : ""
                    }`.trim()}
                  >
                    {preHitEnabled ? "On" : "Off"}
                  </button>
                  <div className="strategy-rail__subrow-hint" title="Turns EVSnipe pre-hit sizing on or off.">
                    {preHitEnabled ? "ratio active" : "ratio = 0"}
                  </div>
                </div>
              ) : null}
            </div>
          );
        })}
      </div>
    </div>
  );

  const renderRailSave = () => (
    <div className="rail-save">
      <div className="strategy-rail__title">Profile Save</div>
      <div className="rail-save__hint">
        Save overview edits from the rail. Strategy-specific saves move into the editor footer.
      </div>
      <button
        type="button"
        onClick={() => void handleSave()}
        disabled={saveLoading || !canOperate}
        className="ui-button ui-button--accent"
      >
        {saveLoading ? "Saving..." : dirty ? "Save changes" : "Saved"}
      </button>
      {saveMessage ? <div className="metric-detail">{saveMessage}</div> : null}
    </div>
  );

  const renderRailContent = () => (
    <div className="space-y-4">
      {renderStrategyList()}
      {!selectedStrategy ? renderRailSave() : null}
    </div>
  );

  const renderOverview = () => (
    <div className="page-stack">
      <div className="grid gap-4 xl:grid-cols-5">
        <SectionPanel title="Portfolio" subtitle="Total account value across cash and open positions.">
          <div className="text-4xl font-semibold tracking-[-0.04em] text-[var(--text-primary)]">
            {formatUsd(overview?.total_equity)}
          </div>
          <div className="mt-3 text-sm text-[var(--text-secondary)]">
            Open positions:{" "}
            <span className="text-[var(--text-primary)]">{formatUsd(overview?.portfolio_value)}</span>
          </div>
          {overview?.portfolio_value_error ? (
            <div className="mt-3 inline-alert inline-alert--warning">
              {overview.portfolio_value_error}
            </div>
          ) : null}
        </SectionPanel>

        <SectionPanel title="Available Balance" subtitle="Free USDC available from the active wallet.">
          <div className="text-4xl font-semibold tracking-[-0.04em] text-[var(--text-primary)]">
            {formatUsd(overview?.available_balance)}
          </div>
          <div className="mt-3 text-sm text-[var(--text-secondary)]">
            Ready for new orders from the active trading wallet.
          </div>
          {overview?.available_balance_error ? (
            <div className="mt-3 inline-alert inline-alert--warning">
              {overview.available_balance_error}
            </div>
          ) : null}
        </SectionPanel>

        <SectionPanel title="PnL Today (UTC)" subtitle="Realized result for the current UTC day.">
          <div className="text-4xl font-semibold tracking-[-0.04em] text-[var(--text-primary)]">
            {formatUsd(overview?.pnl_today_utc)}
          </div>
          <div className="mt-3 text-sm text-[var(--text-secondary)]">
            {overview?.active_strategy_count ?? 0} active strategies
          </div>
        </SectionPanel>

        <SectionPanel
          title="Liquidity Rewards"
          subtitle="Polymarket liquidity rewards from the active trading wallet."
        >
          <div className="text-4xl font-semibold tracking-[-0.04em] text-[var(--text-primary)]">
            {formatUsd(overview?.liquidity_rewards_today)}
          </div>
          <div className="mt-3 text-sm text-[var(--text-secondary)]">
            Today (UTC)
          </div>
          <div className="mt-1 text-sm text-[var(--text-secondary)]">
            Since Using EVPoly:{" "}
            <span className="text-[var(--text-primary)]">
              {formatUsd(overview?.liquidity_rewards_lifetime)}
            </span>
          </div>
          {overview?.liquidity_rewards_as_of_utc ? (
            <div className="mt-1 text-xs text-[var(--text-tertiary)]">
              Updated {formatRelativeTime(overview.liquidity_rewards_as_of_utc)}
            </div>
          ) : null}
          {overview?.liquidity_rewards_error ? (
            <div className="mt-3 inline-alert inline-alert--warning">
              {overview.liquidity_rewards_error}
            </div>
          ) : null}
        </SectionPanel>

        <SectionPanel title="Latency" subtitle="Average acknowledgement latency from the last 24 hours.">
          <div className="text-4xl font-semibold tracking-[-0.04em] text-[var(--text-primary)]">
            {overview?.avg_ack_latency_ms !== null && overview?.avg_ack_latency_ms !== undefined
              ? `${overview.avg_ack_latency_ms.toFixed(1)} ms`
              : "--"}
          </div>
          <div className="mt-3 text-sm text-[var(--text-secondary)]">
            {overview?.ack_sample_count ?? 0} samples
            {(overview?.ack_warning_count_recent ?? 0) > 0
              ? ` | ${overview?.ack_warning_count_recent ?? 0} recent warnings`
              : ""}
          </div>
        </SectionPanel>
      </div>

      <SectionPanel
        title="Activity Feed"
        subtitle="Only completed buys and sells appear here."
        actions={
          <button type="button" onClick={() => setLogsOpen(true)} className="ui-button">
            Open Logs
          </button>
        }
      >
        {activityItems.length === 0 ? (
          <div className="empty-state">
            {overview?.bot_state === "running"
              ? "No recent trades yet. Filled buys and sells will appear here once the bot gets execution."
              : "No recent trades yet. Start the bot or finish setup in Settings first."}
          </div>
        ) : (
          <div className="activity-feed">
            {activityItems.map((item, index) => (
              <div
                key={`${item.timestamp}-${index}`}
                className={`activity-feed__row ${
                  item.thumbnail_url ? "activity-feed__row--with-thumb" : ""
                }`.trim()}
              >
                <div
                  className={`activity-feed__action activity-feed__action--${activityActionClass(
                    item.action
                  )}`}
                >
                  <div className="activity-feed__marker">
                    {activityActionClass(item.action) === "sell" ? "-" : "+"}
                  </div>
                  <div className="activity-feed__action-label">
                    {item.action ?? "Trade"}
                  </div>
                </div>

                {item.thumbnail_url ? (
                  <div className="activity-feed__thumb">
                    <img
                      src={item.thumbnail_url}
                      alt=""
                      className="activity-feed__thumb-image"
                      loading="lazy"
                    />
                  </div>
                ) : null}

                <div className="activity-feed__content">
                  <div className="activity-feed__title">
                    {item.market_title || item.title || item.message}
                  </div>

                  <div className="activity-feed__meta">
                    {item.outcome ? (
                      <span
                        className={`activity-feed__chip activity-feed__chip--${activityOutcomeClass(
                          item.outcome
                        )}`}
                      >
                        {item.outcome}
                      </span>
                    ) : null}
                    {item.quantity !== null && item.quantity !== undefined ? (
                      <span>{formatControlValue(item.quantity)} shares</span>
                    ) : null}
                  </div>
                </div>

                <div className="activity-feed__aside">
                  {item.cashflow_usd !== null && item.cashflow_usd !== undefined ? (
                    <div
                      className={`activity-feed__value activity-feed__value--${activityValueClass(
                        item.cashflow_usd
                      )}`}
                    >
                      {formatUsd(item.cashflow_usd)}
                    </div>
                  ) : null}
                  <div className="activity-feed__time">{formatRelativeTime(item.timestamp)}</div>
                </div>
              </div>
            ))}
          </div>
        )}
      </SectionPanel>
    </div>
  );

  return (
    <AppShell
      railSubtitle="BY EVPLUS"
      railLogoSrc="/logo.png"
      railLogoAlt="EVPlus"
      railItems={railItems}
      railChildren={renderRailContent()}
      eyebrow="HOME"
      title={selectedStrategy ? "Strategy Settings" : "Overview"}
      description={
        selectedStrategy
          ? "Adjust risk, symbols, and advanced options for the selected strategy."
          : "Monitor balance, bot status, and recent activity at a glance."
      }
      meta={
        <div className="flex flex-wrap items-center justify-end gap-3">
          <ProfileSwitcher activeProfileId={activeProfileId} onSwitch={(id) => void handleProfileSwitch(id)} />
          <StatusBadge status={overview?.bot_state ?? "unknown"} />
          <button
            type="button"
            onClick={() => void handleRunSetupDoctor()}
            disabled={doctorLoading}
            className="ui-button"
          >
            {doctorLoading ? "Doctor..." : "Doctor"}
          </button>
          <button type="button" onClick={() => void handleLock()} className="ui-button">
            Lock
          </button>
          <button
            type="button"
            onClick={handleStart}
            disabled={actionLoading || !canOperate || botRunning}
            className={`ui-button ui-button--primary ${
              botRunning ? "ui-button--running-disabled" : ""
            }`.trim()}
          >
            Start
          </button>
          <button
            type="button"
            onClick={handleRestart}
            disabled={actionLoading || !canOperate}
            className="ui-button ui-button--accent"
          >
            Restart
          </button>
          <button
            type="button"
            onClick={handleStop}
            disabled={actionLoading}
            className="ui-button ui-button--danger"
          >
            Stop
          </button>
        </div>
      }
      banner={
        <div className="space-y-3">
          <UpdateBanner
            version={updateDownloading ? "Downloading..." : updateVersion}
            onUpdate={handleUpdate}
          />
          {displayError ? <div className="inline-alert">{displayError}</div> : null}
        </div>
      }
      contentClassName="page-stack"
    >
      {selectedStrategyMeta ? (
        <StrategyEditorPane
          selectedStrategy={selectedStrategyMeta.key}
          config={config}
          setConfig={setConfig}
          activeProfileId={activeProfileId}
          onSave={() => void handleSave()}
          saveLoading={saveLoading}
          dirty={dirty}
          canSave={canOperate}
          saveMessage={saveMessage}
        />
      ) : (
        renderOverview()
      )}

      {geoDialogStatus ? (
        <GeoAccessDialog
          status={geoDialogStatus}
          onContinue={
            geoDialogStatus.status === "unknown"
              ? () => {
                  const nextAction = pendingGeoAction;
                  setGeoDialogStatus(null);
                  setPendingGeoAction(null);
                  if (nextAction === "start") {
                    void performStart();
                  } else if (nextAction === "restart") {
                    void performRestart();
                  }
                }
              : undefined
          }
          onClose={() => {
            setGeoDialogStatus(null);
            setPendingGeoAction(null);
          }}
        />
      ) : null}

      <LogsDrawer open={logsOpen} onClose={() => setLogsOpen(false)} />

      {doctorDialogOpen ? (
        <SetupDoctorDialog
          result={doctorResult}
          onClose={() => setDoctorDialogOpen(false)}
          onOpenSetup={() => {
            setDoctorDialogOpen(false);
            navigate("/settings");
          }}
        />
      ) : null}
    </AppShell>
  );
}
