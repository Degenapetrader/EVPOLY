import { useCallback, useEffect, useMemo, useState, type CSSProperties } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { check } from "@tauri-apps/plugin-updater";
import { AppShell } from "../components/AppShell";
import { GeoAccessDialog } from "../components/GeoAccessDialog";
import { HomePortfolioTabs } from "../components/HomePortfolioTabs";
import { LogsDrawer } from "../components/LogsDrawer";
import { PerformanceShareCardModal } from "../components/PerformanceShareCardModal";
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
  mmSportRouteDefaultCaps,
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
  getHomePerformanceApi,
  getSavedConfig,
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
  type ProfilePerformancePoint,
  type ProfilePerformanceView,
} from "../lib/tauri-commands";
import {
  buildHomePerformanceSnapshot,
} from "../lib/home-performance-snapshot";
import {
  buildDailyPnlShareCard,
  buildLiquidityRewardShareCard,
  pickPerformanceShareCardBackground,
  type PerformanceShareCardPayload,
} from "../lib/performance-share-card";

const PUBLIC_EVPOINT_LEADERBOARD_URL = "https://www.evplus.ai/points";

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

function RefreshGlyph() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <path
        d="M20 12a8 8 0 0 1-13.7 5.6M4 12A8 8 0 0 1 17.7 6.4M18 3v4h-4M6 21v-4h4"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.8"
      />
    </svg>
  );
}

function ShareGlyph() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <path
        d="M9.7 10.7l4.6-3.4M9.7 13.3l4.6 3.4"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.8"
      />
      <circle cx="7" cy="12" r="2.5" fill="none" stroke="currentColor" strokeWidth="1.8" />
      <circle cx="17" cy="6" r="2.5" fill="none" stroke="currentColor" strokeWidth="1.8" />
      <circle cx="17" cy="18" r="2.5" fill="none" stroke="currentColor" strokeWidth="1.8" />
    </svg>
  );
}

function PerformanceSparkline({ points }: { points: ProfilePerformancePoint[] }) {
  if (points.length < 2) {
    return (
      <div className="situation-sparkline situation-sparkline--empty">
        <span>Feed pending</span>
      </div>
    );
  }

  const values = points.map((point) => point.value);
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
  "Stops new weekend entries for Premarket, Endgame, and EVCurve.";
const WEEKEND_POLICY_TOOLTIP_OFF =
  "Premarket, Endgame, and EVCurve keep trading on weekends.";

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
  const [overviewRefreshing, setOverviewRefreshing] = useState(false);
  const [performanceShareCard, setPerformanceShareCard] =
    useState<PerformanceShareCardPayload | null>(null);
  const [performanceShareBackground, setPerformanceShareBackground] = useState<string | null>(null);
  const [homePerformance, setHomePerformance] = useState<ProfilePerformanceView | null>(null);
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

  const dirty = useMemo(() => JSON.stringify(config) !== savedSnapshot, [config, savedSnapshot]);
  const displayError = actionError || overviewError;
  const canOperate = Boolean(activeProfileId && configLoaded);
  const botRunning = overview?.bot_state === "running";
  const otherProfileRunning = Boolean(overview?.other_profile_running);
  const globalBotBusy = ["starting", "running", "stopping"].includes(
    overview?.global_bot_state ?? overview?.bot_state ?? ""
  );
  const liveProfileLabel = overview?.live_profile_name?.trim() || "live profile";
  const refreshHomePerformance = useCallback(async () => {
    if (!activeProfileId) {
      setHomePerformance(null);
      return;
    }
    try {
      setHomePerformance(await getHomePerformanceApi("1d"));
    } catch {
      setHomePerformance(null);
    }
  }, [activeProfileId]);

  useEffect(() => {
    void refreshHomePerformance();
  }, [refreshHomePerformance, portfolioFeedSeed]);

  const performanceSnapshot = useMemo(
    () =>
      buildHomePerformanceSnapshot({
        overview,
        performance: homePerformance,
        publicOpenPositionsValue: overview?.portfolio_value ?? null,
      }),
    [overview, homePerformance]
  );

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
    setHomePerformance(null);
    setActiveProfileId(profileId);
    await loadProfileConfig(profileId);
    await refreshOverview(true);
    setPortfolioFeedSeed((current) => current + 1);
  };

  const refreshHomeData = useCallback(async () => {
    setOverviewRefreshing(true);
    try {
      await refreshOverview(true);
      setPortfolioFeedSeed((current) => current + 1);
    } finally {
      setOverviewRefreshing(false);
    }
  }, [refreshOverview]);

  const openPerformanceShare = useCallback((card: PerformanceShareCardPayload) => {
    setPerformanceShareCard(card);
    setPerformanceShareBackground(pickPerformanceShareCardBackground(card));
  }, []);

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
    { label: "Public EVPoint Leaderboard", href: PUBLIC_EVPOINT_LEADERBOARD_URL },
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
                            ...mmSportRouteDefaultCaps(value),
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
    const availableBalance = performanceSnapshot.availableBalance;
    const openExposure = performanceSnapshot.openPositionsValue;
    const totalEquity = performanceSnapshot.portfolioValue;
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
    const pnlUpdated = performanceSnapshot.pnlAsOfUtc
      ? formatRelativeTime(performanceSnapshot.pnlAsOfUtc)
      : null;
    const dailyPnlShareCard = buildDailyPnlShareCard({
      pnl: performanceSnapshot.pnl,
      openPnl: performanceSnapshot.openPnl,
      realizedPnl: performanceSnapshot.realizedPnl,
      sourceLabel: performanceSnapshot.pnlSourceLabel,
      feedLabel: performanceSnapshot.pnlFeedLabel,
      updatedLabel: pnlUpdated,
      series: performanceSnapshot.series,
    });
    const liquidityRewardShareCard = buildLiquidityRewardShareCard({
      reward: performanceSnapshot.rewardsToday,
      title: "Liquidity Rewards",
      updatedLabel: rewardsUpdated || null,
    });

    return (
      <div className="page-stack overview-operator">
        <div className="home-overview-grid">
          <div className="home-overview-grid__capital">
            <SectionPanel
              title="Capital"
              subtitle="Wallet snapshot and current exposure."
              actions={
                <button
                  type="button"
                  className="ui-button ui-button--compact performance-share-trigger"
                  onClick={() => void refreshHomeData()}
                  disabled={overviewRefreshing}
                >
                  <RefreshGlyph />
                  <span>{overviewRefreshing ? "Refreshing" : "Refresh"}</span>
                </button>
              }
            >
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
            <SectionPanel
              title="Profit/Loss"
              subtitle="Polymarket account movement for the past day."
              actions={
                <button
                  type="button"
                  className="ui-button ui-button--compact performance-share-trigger"
                  onClick={() => dailyPnlShareCard ? openPerformanceShare(dailyPnlShareCard) : undefined}
                  disabled={!dailyPnlShareCard}
                >
                  <ShareGlyph />
                  <span>Share</span>
                </button>
              }
            >
              <div className="situation-card-body situation-card-body--pnl">
                <div className="situation-pnl-main">
                  <div className="home-capital-card__label">Today</div>
                  <div className={metricToneClass(performanceSnapshot.pnl)}>
                    {performanceSnapshot.pnl === null || performanceSnapshot.pnl === undefined
                      ? "N/A"
                      : formatUsd(performanceSnapshot.pnl)}
                  </div>
                  <div className="situation-inline-metrics">
                    <span>
                      Open <strong>{formatUsd(performanceSnapshot.openPnl)}</strong>
                    </span>
                    <span>
                      Feed <strong>{performanceSnapshot.pnlFeedLabel}</strong>
                    </span>
                  </div>
                </div>
                <PerformanceSparkline points={performanceSnapshot.series} />
                <div className="home-overview__detail home-overview__detail--nowrap">
                  Source {performanceSnapshot.pnlSourceLabel}
                  {pnlUpdated ? ` | Updated ${pnlUpdated}` : ""}
                </div>
              </div>
            </SectionPanel>
          </div>

          <div className="home-overview-grid__metric">
            <SectionPanel
              title="Liquidity Rewards"
              subtitle="Maker rewards credited to the active wallet."
              actions={
                <button
                  type="button"
                  className="ui-button ui-button--compact performance-share-trigger"
                  onClick={() => liquidityRewardShareCard ? openPerformanceShare(liquidityRewardShareCard) : undefined}
                  disabled={!liquidityRewardShareCard}
                >
                  <ShareGlyph />
                  <span>Share</span>
                </button>
              }
            >
              <div className="situation-card-body">
                <div className="home-capital-card__label">Today</div>
                <div className={metricClass(performanceSnapshot.rewardsToday)}>
                  {performanceSnapshot.rewardsToday === null ||
                  performanceSnapshot.rewardsToday === undefined
                    ? "Unavailable"
                    : formatUsd(performanceSnapshot.rewardsToday)}
                </div>
                <div className="situation-meter situation-meter--reward" aria-hidden="true">
                  <span
                    style={{
                      width:
                        performanceSnapshot.rewardsToday && performanceSnapshot.rewardsToday > 0
                          ? "42%"
                          : "8%",
                    }}
                  />
                </div>
                <div className="home-overview__detail home-overview__detail--nowrap">
                  {performanceSnapshot.rewardsError
                    ? performanceSnapshot.rewardsError
                    : `Lifetime ${formatUsd(performanceSnapshot.rewardsLifetime)}${
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
          profileId={activeProfileId}
          walletAddress={null}
          workerAvailable={botRunning}
          botState={overview?.bot_state}
          refreshToken={portfolioFeedSeed}
          onOpenLogs={() => setLogsOpen(true)}
          onSharePosition={openPerformanceShare}
          onShareReward={openPerformanceShare}
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
          <ProfileSwitcher
            activeProfileId={activeProfileId}
            onSwitch={(id) => void handleProfileSwitch(id)}
            onCreateWallet={(method) => navigate("/settings", { state: { createWalletMethod: method } })}
          />
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
              onClick={() =>
                overview?.live_profile_id ? void handleOpenLiveProfile() : void handleStop()
              }
              disabled={actionLoading}
              className={`ui-button ${
                overview?.live_profile_id ? "ui-button--accent" : "ui-button--danger"
              }`}
              title={
                overview?.live_profile_id
                  ? `Switch to ${liveProfileLabel}`
                  : "Stop the live bot before starting this profile"
              }
            >
              {overview?.live_profile_id ? "Another Bot is Running" : "Stop Live Bot"}
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
          {!selectedStrategyMeta ? (
            <div className="rounded-[8px] border border-[#24496e] bg-[#102136] px-5 py-4 text-[18px] text-[var(--text-primary)] shadow-[inset_0_1px_0_rgba(255,255,255,0.03)]">
              <strong className="font-semibold">Builder fees apply to all trades:</strong>{" "}
              <span className="text-[#9fc7ff]">0.1%</span>{" "}
              <span className="text-[var(--text-secondary)]">on both taker and maker fills.</span>
            </div>
          ) : null}
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

      {performanceShareCard && performanceShareBackground ? (
        <PerformanceShareCardModal
          card={performanceShareCard}
          backgroundPath={performanceShareBackground}
          onClose={() => {
            setPerformanceShareCard(null);
            setPerformanceShareBackground(null);
          }}
        />
      ) : null}
    </AppShell>
  );
}
