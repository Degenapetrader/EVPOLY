import { useCallback, useEffect, useMemo, useState, type CSSProperties } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { check } from "@tauri-apps/plugin-updater";
import { AppShell } from "../components/AppShell";
import { GeoAccessDialog } from "../components/GeoAccessDialog";
import { HomePortfolioTabs } from "../components/HomePortfolioTabs";
import { LogsDrawer } from "../components/LogsDrawer";
import { ProfileSwitcher } from "../components/ProfileSwitcher";
import { SectionPanel } from "../components/SectionPanel";
import { SetupDoctorDialog } from "../components/SetupDoctorDialog";
import { StatusBadge } from "../components/StatusBadge";
import { StrategyEditorPane } from "../components/StrategyEditorPane";
import { UpdateBanner } from "../components/UpdateBanner";
import { useAppContext } from "../App";
import { useHomeOverview } from "../hooks/useHomeOverview";
import {
  STRATEGIES,
  VISIBLE_STRATEGIES,
  DEFAULT_CONFIG,
  formatUsd,
  mergeConfig,
  parseNonNegative,
  setEVSnipePreHitEnabled,
  strategyControlSuffix,
  strategyControlTooltip,
  strategySizeLabel,
  strategySizeValue,
  strategyTooltip,
  updateStrategyEnabled,
  updateStrategySettingsSection,
  updateStrategySize,
  type StrategyEditorSection,
  type StrategyKey,
} from "../lib/desktop-config";
import {
  getGeoAccessStatus,
  getActiveProfileId,
  getSavedConfig,
  getTradeStats,
  lockSession,
  restartBot,
  runSetupDoctor,
  saveConfig,
  setActiveProfile,
  startBot,
  stopBot,
  type BotConfig,
  type GeoAccessStatus,
  type SetupDoctorResult,
  type TradeStats,
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

function formatPusdAmount(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value)) return "--";
  return value.toFixed(2);
}

function metricClass(value: number | null | undefined): string {
  return value === null || value === undefined
    ? "home-overview__metric home-overview__metric--placeholder"
    : "home-overview__metric";
}

function metricToneClass(value: number | null | undefined): string {
  const base = metricClass(value);
  if (value === null || value === undefined || value === 0) return base;
  return value > 0
    ? `${base} home-overview__metric--positive`
    : `${base} home-overview__metric--negative`;
}

function clampPercent(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.min(100, Math.max(0, value));
}

function PerformanceSparkline({ points }: { points: TradeStats["pnl_history"] }) {
  if (points.length < 2) {
    return (
      <div className="situation-sparkline situation-sparkline--empty">
        <span>Feed pending</span>
      </div>
    );
  }

  const values = points.map((point) => point.pnl);
  const min = Math.min(...values);
  const max = Math.max(...values);
  const range = Math.max(max - min, 1);
  const width = 240;
  const height = 76;
  const xStep = width / Math.max(points.length - 1, 1);
  const coords = values.map((value, index) => {
    const x = index * xStep;
    const y = height - ((value - min) / range) * height;
    return [x, y] as const;
  });
  const linePath = coords
    .map(([x, y], index) => `${index === 0 ? "M" : "L"} ${x.toFixed(2)} ${y.toFixed(2)}`)
    .join(" ");
  const fillPath = `${linePath} L ${width} ${height} L 0 ${height} Z`;

  return (
    <svg className="situation-sparkline" viewBox={`0 0 ${width} ${height}`} role="img" aria-label="PnL history">
      <path className="situation-sparkline__fill" d={fillPath} />
      <path className="situation-sparkline__line" d={linePath} />
    </svg>
  );
}

function strategyKeyFromRoute(strategySlug?: string): StrategyKey | null {
  if (!strategySlug) return null;
  return (
    VISIBLE_STRATEGIES.find((strategy) => strategy.key === strategySlug)?.key ?? null
  );
}

const WEEKEND_POLICY_TOOLTIP_PAUSE =
  "Stops new weekend entries for Premarket, Endgame, EVCurve, and S-Band.";
const WEEKEND_POLICY_TOOLTIP_OFF =
  "Premarket, Endgame, EVCurve, and S-Band keep trading on weekends.";

export function Home() {
  const navigate = useNavigate();
  const { strategySlug } = useParams();
  const { activeProfileId, setActiveProfileId, setAuthenticated } = useAppContext();
  const { overview, error: overviewError, refresh: refreshOverview } = useHomeOverview();
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
  const [portfolioFeedSeed, setPortfolioFeedSeed] = useState(0);
  const [tradeStats, setTradeStats] = useState<TradeStats | null>(null);
  const [requestedEditorSection, setRequestedEditorSection] =
    useState<StrategyEditorSection | null>(null);
  const [railDraftValues, setRailDraftValues] = useState<Record<StrategyKey, string>>(() =>
    Object.fromEntries(
      STRATEGIES.map((strategy) => [strategy.key, String(strategySizeValue(DEFAULT_CONFIG, strategy.key))])
    ) as Record<StrategyKey, string>
  );

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

  useEffect(() => {
    setRailDraftValues(
      Object.fromEntries(
        STRATEGIES.map((strategy) => [strategy.key, String(strategySizeValue(config, strategy.key))])
      ) as Record<StrategyKey, string>
    );
  }, [config]);

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

  useEffect(() => {
    let active = true;
    let interval: ReturnType<typeof setInterval> | null = null;

    const load = async () => {
      try {
        const nextStats = await getTradeStats();
        if (active) {
          setTradeStats(nextStats);
        }
      } catch {
        if (active) {
          setTradeStats(null);
        }
      }
    };

    void load();
    interval = setInterval(() => void load(), 60_000);

    return () => {
      active = false;
      if (interval) clearInterval(interval);
    };
  }, [activeProfileId, portfolioFeedSeed]);

  const dirty = useMemo(() => JSON.stringify(config) !== savedSnapshot, [config, savedSnapshot]);
  const displayError = actionError || overviewError;
  const canOperate = Boolean(activeProfileId && configLoaded);
  const botRunning = overview?.bot_state === "running";
  const otherProfileRunning = Boolean(overview?.other_profile_running && overview.live_profile_id);
  const globalBotBusy = ["starting", "running", "stopping"].includes(
    overview?.global_bot_state ?? overview?.bot_state ?? ""
  );
  const liveProfileLabel = overview?.live_profile_name?.trim() || "live profile";

  const handleUpdate = async () => {
    if (!pendingUpdate || updateDownloading) return;
    setUpdateDownloading(true);
    try {
      if (globalBotBusy) {
        await stopBot();
        await refreshOverview(true);
      }
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
    await refreshOverview(true);
    setPortfolioFeedSeed((current) => current + 1);
  };

  const handleOpenLiveProfile = async () => {
    if (!overview?.live_profile_id) return;
    setActionLoading(true);
    try {
      await setActiveProfile(overview.live_profile_id);
      await handleProfileSwitch(overview.live_profile_id);
      setActionError(null);
    } catch (err) {
      setActionError(getErrorText(err, "failed to open live profile"));
    } finally {
      setActionLoading(false);
    }
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
      await refreshOverview(true);
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
      await refreshOverview(true);
      setPortfolioFeedSeed((current) => current + 1);
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
      await refreshOverview(true);
      setPortfolioFeedSeed((current) => current + 1);
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
      await refreshOverview(true);
      setPortfolioFeedSeed((current) => current + 1);
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
      await refreshOverview(true);
      setPortfolioFeedSeed((current) => current + 1);
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

  const commitRailDraftValue = (strategy: StrategyKey) => {
    const currentValue = strategySizeValue(config, strategy);
    const nextValue = parseNonNegative(railDraftValues[strategy], currentValue);
    setRailDraftValues((current) => ({ ...current, [strategy]: String(nextValue) }));
    setConfig((current) => updateStrategySize(current, strategy, nextValue));
  };

  const updateRailDraftValue = (strategy: StrategyKey, nextRawValue: string) => {
    setRailDraftValues((current) => ({ ...current, [strategy]: nextRawValue }));
  };

  const openMMSportAdvanced = useCallback(() => {
    setRequestedEditorSection("advanced");
    navigate("/home/mm_sport");
  }, [navigate]);

  const consumeRequestedEditorSection = useCallback(() => {
    setRequestedEditorSection(null);
  }, []);

  const renderStrategyList = () => (
    <div className="strategy-rail">
      <div className="strategy-rail__heading">
        <div className="strategy-rail__heading-row">
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
            className={`strategy-rail__policy-toggle ${
              config.weekend_policy === "pause" ? "strategy-rail__policy-toggle--active" : ""
            }`.trim()}
            title={
              config.weekend_policy === "pause"
                ? WEEKEND_POLICY_TOOLTIP_PAUSE
                : WEEKEND_POLICY_TOOLTIP_OFF
            }
            aria-pressed={config.weekend_policy === "pause"}
          >
            {config.weekend_policy === "pause" ? "OFF-HOURS PAUSE" : "NO OFF DAY"}
          </button>
        </div>
        <p className="strategy-rail__hint">Select a strategy to edit its settings.</p>
      </div>

      <div className="strategy-rail__list">
        {VISIBLE_STRATEGIES.map((strategy) => {
          const enabled = config.strategies[strategy.key];
          const value = strategySizeValue(config, strategy.key);
          const selected = strategy.key === selectedStrategy;
          const suffix = strategyControlSuffix(strategy.key, config);
          const controlTitle = strategyControlTooltip(config, strategy.key);
          const showPreHitRow = strategy.key === "evsnipe";
          const preHitEnabled = config.strategy_settings.evsnipe.pre_hit_enabled;
          const mmSportDualRoute =
            strategy.key === "mm_sport" &&
            config.strategy_settings.mm_sport.discovery_route === "dual";
          const fieldTitle = mmSportDualRoute
            ? "Dual uses separate Sport and Non-S sizing profiles. Open Advanced to edit both."
            : controlTitle;

          return (
            <div
              key={strategy.key}
              className={`strategy-rail__group ${selected ? "strategy-rail__group--active" : ""} ${
                strategy.key === "mm_sport" ? "strategy-rail__group--has-divider" : ""
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
                  <span className="strategy-rail__label">
                    {strategy.label}
                  </span>
                  <span className="strategy-rail__link-chevron" aria-hidden="true">
                    &rsaquo;
                  </span>
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

                <div className="strategy-rail__field" title={fieldTitle}>
                  <input
                    type={mmSportDualRoute ? "text" : "number"}
                    min="0"
                    step="0.1"
                    value={
                      mmSportDualRoute
                        ? "Dual"
                        : railDraftValues[strategy.key] ?? String(value)
                    }
                    aria-label={`${strategy.label} ${strategySizeLabel(strategy.key, config)}`}
                    aria-disabled={mmSportDualRoute}
                    onClick={mmSportDualRoute ? openMMSportAdvanced : undefined}
                    onChange={(event) => updateRailDraftValue(strategy.key, event.target.value)}
                    onBlur={() => {
                      if (!mmSportDualRoute) {
                        commitRailDraftValue(strategy.key);
                      }
                    }}
                    onKeyDown={(event) => {
                      if (mmSportDualRoute && (event.key === "Enter" || event.key === " ")) {
                        event.preventDefault();
                        openMMSportAdvanced();
                        return;
                      }
                      if (event.key === "Enter") {
                        event.currentTarget.blur();
                      }
                    }}
                    readOnly={mmSportDualRoute}
                    disabled={!canOperate}
                    className="field-input field-input--compact"
                    placeholder={mmSportDualRoute ? "Dual" : suffix}
                    title={fieldTitle}
                  />
                  {strategy.key === "mm_sport" ? (
                    <button
                      type="button"
                      className="strategy-rail__field-suffix strategy-rail__field-suffix--toggle"
                      disabled={!canOperate}
                      onClick={() => {
                        if (mmSportDualRoute) {
                          openMMSportAdvanced();
                          return;
                        }
                        setConfig((current) => {
                          const mmSport = current.strategy_settings.mm_sport;
                          const useNonSport = mmSport.discovery_route === "nonsports";
                          const currentMode = useNonSport
                            ? mmSport.nonsport_quote_size_mode
                            : mmSport.quote_size_mode;
                          const nextMode =
                            currentMode === "multiple" ? "depth_ratio" : "multiple";

                          return updateStrategySettingsSection(current, "mm_sport", {
                            ...mmSport,
                            ...(useNonSport
                              ? { nonsport_quote_size_mode: nextMode }
                              : { quote_size_mode: nextMode }),
                          });
                        });
                      }}
                      title={
                        mmSportDualRoute
                          ? "Open Advanced MM 2.0 settings"
                          : "Click to toggle Multiple / Depth Ratio"
                      }
                    >
                      {mmSportDualRoute ? "ADV" : suffix}
                    </button>
                  ) : (
                    <span className="strategy-rail__field-suffix">{suffix}</span>
                  )}
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
                    <span>Pre-hit</span>
                    <span className="strategy-rail__link-chevron" aria-hidden="true">
                      &rsaquo;
                    </span>
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

              {strategy.key === "mm_sport" ? (
                <div className="strategy-rail__route-row">
                  {([
                    ["sports", "Sport"],
                    ["nonsports", "Non-S"],
                    ["dual", "Dual"],
                  ] as const).map(([value, label]) => (
                    <button
                      key={value}
                      type="button"
                      disabled={!canOperate}
                      onClick={() =>
                        setConfig((current) =>
                          updateStrategySettingsSection(current, "mm_sport", {
                            ...current.strategy_settings.mm_sport,
                            discovery_route: value,
                          })
                        )
                      }
                      className={`strategy-rail__mode-btn ${
                        config.strategy_settings.mm_sport.discovery_route === value
                          ? "strategy-rail__mode-btn--active"
                          : ""
                      }`.trim()}
                    >
                      {label}
                    </button>
                  ))}
                </div>
              ) : null}
            </div>
          );
        })}
      </div>

      <div className="strategy-rail__save-wrap">
        <button
          type="button"
          onClick={() => void handleSave()}
          disabled={saveLoading || !dirty || !canOperate}
          className={`strategy-rail__save ${
            dirty ? "strategy-rail__save--dirty" : "strategy-rail__save--synced"
          } ${saveLoading ? "strategy-rail__save--saving" : ""}`.trim()}
        >
          {saveLoading ? "Saving..." : dirty ? "Save" : "Saved"}
        </button>
        {saveMessage ? <div className="metric-detail">{saveMessage}</div> : null}
      </div>
    </div>
  );

  const renderRailContent = () => (
    <div className="space-y-4">
      {renderStrategyList()}
    </div>
  );

  const renderOverview = () => {
    const availableBalance = overview?.available_balance ?? null;
    const openExposure = overview?.portfolio_value ?? null;
    const totalEquity =
      overview?.total_equity ??
      (availableBalance !== null && openExposure !== null
        ? availableBalance + openExposure
        : null);
    const capitalBase =
      (availableBalance ?? 0) + (openExposure ?? 0) > 0
        ? (availableBalance ?? 0) + (openExposure ?? 0)
        : 0;
    const capitalBalanceSharePct =
      capitalBase > 0 ? clampPercent(((availableBalance ?? 0) / capitalBase) * 100) : 0;
    const capitalOpenSharePct =
      capitalBase > 0 ? clampPercent(((openExposure ?? 0) / capitalBase) * 100) : 0;
    const latencySamples = overview?.ack_sample_count ?? 0;
    const latencyWarnings = overview?.ack_warning_count_recent ?? 0;
    const latencyHealth =
      latencySamples > 0
        ? clampPercent(Math.round(((latencySamples - latencyWarnings) / latencySamples) * 100))
        : null;
    const latencyRingStyle = {
      "--latency-health": `${latencyHealth ?? 0}%`,
    } as CSSProperties;
    const rewardsUpdated = overview?.liquidity_rewards_as_of_utc
      ? `Updated ${formatRelativeTime(overview.liquidity_rewards_as_of_utc)}`
      : "";
    const feedLabel = tradeStats ? "Synced" : "Pending";

    return (
      <div className="page-stack overview-operator">
        <div className="home-overview-grid">
          <div className="home-overview-grid__capital">
            <SectionPanel title="Capital" subtitle="Wallet snapshot and current exposure.">
              <div className="situation-card-body situation-card-body--capital">
                <div className="home-capital-card__grid">
                  <div className="home-capital-card__slot">
                    <div className="home-capital-card__label">Portfolio</div>
                    <div className={metricClass(totalEquity)}>
                      {totalEquity === null ? "Unavailable" : formatUsd(totalEquity)}
                    </div>
                  </div>
                  <div className="home-capital-card__slot">
                    <div className="home-capital-card__label">pUSD Balance</div>
                    <div className="home-overview__metric-row">
                      <span className={metricClass(availableBalance)}>
                        {availableBalance === null ? "N/A" : formatPusdAmount(availableBalance)}
                      </span>
                      <span className="home-overview__unit">pUSD</span>
                    </div>
                    <div className="home-capital-card__wrap-balance">pUSD refreshed periodically</div>
                  </div>
                </div>

                <div className="capital-exposure">
                  <div className="capital-exposure__row">
                    <span>Open exposure</span>
                    <strong>{openExposure === null ? "Pending" : formatUsd(openExposure)}</strong>
                  </div>
                  <div className="capital-exposure__track" aria-hidden="true">
                    <span
                      className="capital-exposure__balance"
                      style={{ width: `${capitalBalanceSharePct}%` }}
                    />
                    <span
                      className="capital-exposure__open"
                      style={{ width: `${capitalOpenSharePct}%` }}
                    />
                  </div>
                </div>

                <div className="home-overview__detail">
                  {overview?.available_balance_error ||
                    overview?.portfolio_value_error ||
                    "Live wallet snapshot"}
                </div>
              </div>
            </SectionPanel>
          </div>

          <div className="home-overview-grid__metric">
            <SectionPanel title="Profit/Loss" subtitle="Polymarket account movement for the current UTC day.">
              <div className="situation-card-body situation-card-body--pnl">
                <div className="situation-pnl-main">
                  <div className="home-capital-card__label">Today</div>
                  <div className={metricToneClass(overview?.pnl_today_utc)}>
                    {overview?.pnl_today_utc === null || overview?.pnl_today_utc === undefined
                      ? "N/A"
                      : formatUsd(overview.pnl_today_utc)}
                  </div>
                  <div className="situation-inline-metrics">
                    <span>
                      Total <strong>{formatUsd(tradeStats?.total_pnl ?? null)}</strong>
                    </span>
                    <span>
                      Feed <strong>{feedLabel}</strong>
                    </span>
                  </div>
                </div>
                <PerformanceSparkline points={tradeStats?.pnl_history ?? []} />
                <div className="home-overview__detail home-overview__detail--nowrap">
                  {overview?.active_strategy_count ?? 0} active strategies
                </div>
              </div>
            </SectionPanel>
          </div>

          <div className="home-overview-grid__metric">
            <SectionPanel title="Liquidity Rewards" subtitle="Maker rewards credited to the active wallet.">
              <div className="situation-card-body">
                <div className="home-capital-card__label">Today</div>
                <div className={metricClass(overview?.liquidity_rewards_today)}>
                  {overview?.liquidity_rewards_today === null ||
                  overview?.liquidity_rewards_today === undefined
                    ? "Unavailable"
                    : formatUsd(overview.liquidity_rewards_today)}
                </div>
                <div className="situation-meter situation-meter--reward" aria-hidden="true">
                  <span
                    style={{
                      width:
                        overview?.liquidity_rewards_today && overview.liquidity_rewards_today > 0
                          ? "42%"
                          : "8%",
                    }}
                  />
                </div>
                <div className="home-overview__detail home-overview__detail--nowrap">
                  {overview?.liquidity_rewards_error
                    ? overview.liquidity_rewards_error
                    : `Lifetime ${formatUsd(overview?.liquidity_rewards_lifetime ?? null)}${
                        rewardsUpdated ? ` | ${rewardsUpdated}` : ""
                      }`}
                </div>
              </div>
            </SectionPanel>
          </div>

          <div className="home-overview-grid__metric">
            <SectionPanel title="Latency" subtitle="Acknowledgement speed across entries.">
              <div className="situation-card-body situation-card-body--latency">
                <div>
                  <div className="home-capital-card__label">Avg Ack Time</div>
                  <div className={metricClass(overview?.avg_ack_latency_ms)}>
                    {overview?.avg_ack_latency_ms !== null && overview?.avg_ack_latency_ms !== undefined
                      ? `${overview.avg_ack_latency_ms.toFixed(1)} ms`
                      : "N/A"}
                  </div>
                  <div className="home-overview__detail">
                    {latencySamples} samples
                    {latencyWarnings > 0 ? ` | ${latencyWarnings} slow` : ""}
                  </div>
                </div>
                <div
                  className="situation-health-ring"
                  style={latencyRingStyle}
                  aria-label="Latency health"
                >
                  <strong>{latencyHealth === null ? "--" : `${latencyHealth}%`}</strong>
                </div>
              </div>
            </SectionPanel>
          </div>
        </div>

        <HomePortfolioTabs
          key={`${activeProfileId ?? "none"}-${portfolioFeedSeed}`}
          botState={overview?.bot_state}
          onOpenLogs={() => setLogsOpen(true)}
        />
      </div>
    );
  };

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
          {otherProfileRunning ? (
            <button
              type="button"
              onClick={() => void handleOpenLiveProfile()}
              disabled={actionLoading}
              className="ui-button ui-button--accent"
              title={`Switch to ${liveProfileLabel}`}
            >
              Open Live Profile
            </button>
          ) : (
            <>
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
            </>
          )}
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
          requestedSection={requestedEditorSection}
          onRequestedSectionConsumed={consumeRequestedEditorSection}
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
