import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { check } from "@tauri-apps/plugin-updater";
import { AppShell } from "../components/AppShell";
import { InfoPill } from "../components/InfoPill";
import { LogsDrawer } from "../components/LogsDrawer";
import { ProfileSwitcher } from "../components/ProfileSwitcher";
import { SectionPanel } from "../components/SectionPanel";
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
  mergeConfig,
  parseNonNegative,
  strategySizeLabel,
  strategySizeValue,
  updateStrategyEnabled,
  updateStrategySize,
  type StrategyKey,
} from "../lib/desktop-config";
import {
  getActiveProfileId,
  getSavedConfig,
  lockSession,
  restartBot,
  saveConfig,
  startBot,
  stopBot,
  type BotConfig,
  type HomeActivityItem,
} from "../lib/tauri-commands";

function getErrorText(err: unknown, fallback: string): string {
  if (typeof err === "string" && err.trim()) return err;
  if (err instanceof Error && err.message.trim()) return err.message;
  return fallback;
}

function actionTone(
  severity: HomeActivityItem["severity"]
): "accent" | "warning" | "danger" | "success" {
  if (severity === "error") return "danger";
  if (severity === "warning") return "warning";
  return "accent";
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

function strategyKeyFromRoute(strategySlug?: string): StrategyKey | null {
  if (!strategySlug) return null;
  return (
    STRATEGIES.find((strategy) => strategy.key === strategySlug)?.key ?? null
  );
}

function strategyTone(enabled: boolean): "accent" | "neutral" {
  return enabled ? "accent" : "neutral";
}

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

  const handleStart = async () => {
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

  const handleRestart = async () => {
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

  const railItems = [
    { label: "Home", to: "/home" },
    { label: "Settings", to: "/settings" },
    { label: "Open Logs", onClick: () => setLogsOpen(true) },
  ];

  const renderStrategyList = () => (
    <div className="strategy-rail">
      <div>
        <div className="strategy-rail__title">Strategy List</div>
        <div className="strategy-rail__subtitle">
          Switch a strategy into the main workspace and edit its live control value here.
        </div>
      </div>

      <div className="strategy-rail__list">
        {STRATEGIES.map((strategy) => {
          const enabled = config.strategies[strategy.key];
          const value = strategySizeValue(config, strategy.key);
          const label = strategySizeLabel(strategy.key);
          const selected = strategy.key === selectedStrategy;

          return (
            <div
              key={strategy.key}
              className={`strategy-rail__row ${
                selected ? "strategy-rail__row--active" : ""
              }`.trim()}
            >
              <button
                type="button"
                onClick={() => navigate(`/home/${strategy.key}`)}
                className="strategy-rail__link"
              >
                <div className="strategy-rail__label">{strategy.label}</div>
                <div className="strategy-rail__summary">{strategy.summary}</div>
              </button>

              <div className="strategy-rail__controls">
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

                <div className="strategy-rail__field">
                  <label className="field-label">{label}</label>
                  <input
                    type="number"
                    min="0"
                    step="0.1"
                    value={value}
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
                  />
                </div>
              </div>

              <div className="strategy-rail__meta">
                <InfoPill tone={strategyTone(enabled)}>{enabled ? "Enabled" : "Off"}</InfoPill>
                <span className="text-xs text-[var(--text-muted)]">
                  {formatControlValue(value)}
                </span>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );

  const renderOverview = () => (
    <div className="page-stack">
      <div className="grid gap-4 xl:grid-cols-4">
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

        <SectionPanel title="Latency" subtitle="Average acknowledgement latency from tracked samples.">
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
        subtitle="Trade-first activity with only the warnings that matter."
        actions={
          <button type="button" onClick={() => setLogsOpen(true)} className="ui-button">
            Open Logs
          </button>
        }
      >
        {activityItems.length === 0 ? (
          <div className="empty-state">
            {overview?.bot_state === "running"
              ? "The bot is active. Recent trade and order events will appear here when something operator-relevant happens."
              : "No recent activity. Start the bot or finish setup in Settings first."}
          </div>
        ) : (
          <div className="activity-feed">
            {activityItems.map((item, index) => (
              <div key={`${item.timestamp}-${index}`} className="activity-feed__row">
                <div className="activity-feed__action">
                  <InfoPill tone={actionTone(item.severity)}>{item.action ?? item.kind}</InfoPill>
                </div>

                <div className="activity-feed__content">
                  <div className="activity-feed__title">
                    {item.title || item.message}
                  </div>

                  <div className="activity-feed__meta">
                    {item.outcome ? <span className="activity-feed__chip">{item.outcome}</span> : null}
                    {item.quantity !== null && item.quantity !== undefined ? (
                      <span>{formatControlValue(item.quantity)} shares</span>
                    ) : null}
                    {item.detail ? <span>{item.detail}</span> : null}
                  </div>
                </div>

                <div className="activity-feed__aside">
                  {item.value_usd !== null && item.value_usd !== undefined ? (
                    <div className="activity-feed__value">{formatUsd(item.value_usd)}</div>
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
      railSubtitle="Live Workspace"
      railItems={railItems}
      railChildren={renderStrategyList()}
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
          <button type="button" onClick={() => void handleLock()} className="ui-button">
            Lock
          </button>
          <button
            type="button"
            onClick={() => void handleSave()}
            disabled={saveLoading || !canOperate}
            className="ui-button"
          >
            {saveLoading ? "Saving..." : dirty ? "Save" : "Saved"}
          </button>
          <button
            type="button"
            onClick={handleStart}
            disabled={actionLoading || !canOperate}
            className="ui-button ui-button--primary"
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
          {overview?.warnings?.length ? (
            <div className="inline-alert inline-alert--warning">
              {overview.warnings.join(" ")}
            </div>
          ) : null}
          {saveMessage ? <div className="inline-alert inline-alert--warning">{saveMessage}</div> : null}
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
        />
      ) : (
        renderOverview()
      )}

      <LogsDrawer open={logsOpen} onClose={() => setLogsOpen(false)} />
    </AppShell>
  );
}
