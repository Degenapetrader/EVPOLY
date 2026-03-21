import {
  useState,
  useEffect,
  useCallback,
} from "react";
import { check } from "@tauri-apps/plugin-updater";
import { AppShell } from "../components/AppShell";
import { EmptyState } from "../components/EmptyState";
import { InfoPill } from "../components/InfoPill";
import { LogsDrawer } from "../components/LogsDrawer";
import { SectionPanel } from "../components/SectionPanel";
import { StatusBadge } from "../components/StatusBadge";
import { ProfileSwitcher } from "../components/ProfileSwitcher";
import { UpdateBanner } from "../components/UpdateBanner";
import { useBotStatus } from "../hooks/useBotStatus";
import { useTradeData } from "../hooks/useTradeData";
import { useWalletBalance } from "../hooks/useWalletBalance";
import {
  type BotConfig,
  type UiDashboardSummary,
  type UiStrategyState,
  botApiRequest,
  startBot,
  stopBot,
  restartBot,
  getActiveProfileId,
  getSavedConfig,
} from "../lib/tauri-commands";
import {
  buildDashboardViewModel,
  describePositionPrices,
  describeTradeFill,
  formatClock,
  formatCurrency,
  formatQuantity,
} from "../lib/ui-adapters";

function StatCard({
  label,
  value,
  color,
}: {
  label: string;
  value: string;
  color?: string;
}) {
  return (
    <div className="surface-panel">
      <div className="surface-panel__body pt-[var(--space-5)]">
        <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)] mb-2">
          {label}
        </div>
        <div
          className="text-2xl font-semibold mono-data"
          style={{ color: color || "var(--text-primary)" }}
        >
          {value}
        </div>
      </div>
    </div>
  );
}

export function Dashboard() {
  const { status, isRunning, errorMessage } = useBotStatus();
  const {
    stats,
    trades,
    positions,
    isStale: tradeStale,
    error: tradeError,
    refresh: refreshTradeData,
  } =
    useTradeData(isRunning);
  const {
    balance,
    isStale: balanceStale,
    error: balanceError,
    refresh: refreshBalance,
  } = useWalletBalance();

  const [activeProfileId, setActiveProfileId] = useState<string | null>(null);
  const [savedConfig, setSavedConfig] = useState<BotConfig | null>(null);
  const [simulation, setSimulation] = useState(true);
  const [actionLoading, setActionLoading] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [updateVersion, setUpdateVersion] = useState<string | null>(null);
  const [updateDownloading, setUpdateDownloading] = useState(false);
  const [pendingUpdate, setPendingUpdate] =
    useState<Awaited<ReturnType<typeof check>> | null>(null);
  const [logsOpen, setLogsOpen] = useState(false);
  const [uiSummary, setUiSummary] = useState<UiDashboardSummary | null>(null);
  const [uiStrategies, setUiStrategies] = useState<UiStrategyState[]>([]);

  const loadSimulationForProfile = useCallback(async (profileId: string | null) => {
    if (!profileId) return;
    try {
      const saved = await getSavedConfig(profileId);
      setSavedConfig(saved);
      setSimulation(saved.simulation);
    } catch {
      // keep existing simulation mode
    }
  }, []);

  useEffect(() => {
    getActiveProfileId()
      .then(async (id) => {
        setActiveProfileId(id);
        await loadSimulationForProfile(id);
      })
      .catch(() => {});
  }, [loadSimulationForProfile]);

  useEffect(() => {
    (async () => {
      try {
        const update = await check();
        if (update) {
          setPendingUpdate(update);
          setUpdateVersion(update.version);
        } else {
          setPendingUpdate(null);
          setUpdateVersion(null);
        }
      } catch {
        setPendingUpdate(null);
        setUpdateVersion(null);
      }
    })();
  }, []);

  const loadBotUi = useCallback(async () => {
    if (!isRunning) {
      setUiSummary(null);
      setUiStrategies([]);
      return;
    }
    try {
      const [summaryResponse, strategiesResponse] = await Promise.all([
        botApiRequest<{ summary?: UiDashboardSummary }>("GET", "/ui/summary"),
        botApiRequest<{ strategies?: UiStrategyState[] }>("GET", "/ui/strategies"),
      ]);
      setUiSummary(summaryResponse.summary ?? null);
      setUiStrategies(
        Array.isArray(strategiesResponse.strategies)
          ? strategiesResponse.strategies
          : []
      );
    } catch {
      setUiSummary(null);
      setUiStrategies([]);
    }
  }, [isRunning]);

  useEffect(() => {
    if (!isRunning) {
      setUiSummary(null);
      setUiStrategies([]);
      return;
    }
    void loadBotUi();
    const timer = setInterval(() => void loadBotUi(), 4000);
    return () => clearInterval(timer);
  }, [isRunning, loadBotUi]);

  const getErrorText = (err: unknown, fallback: string): string => {
    if (typeof err === "string" && err.trim()) return err;
    if (err && typeof err === "object" && "toString" in err) {
      const text = String(err);
      if (text && text !== "[object Object]") return text;
    }
    return fallback;
  };

  const handleStart = async () => {
    setActionLoading(true);
    try {
      await startBot(simulation);
      setActionError(null);
    } catch (err) {
      setActionError(getErrorText(err, "failed to start bot"));
    }
    setActionLoading(false);
  };

  const handleStop = async () => {
    setActionLoading(true);
    try {
      await stopBot();
      setActionError(null);
    } catch (err) {
      setActionError(getErrorText(err, "failed to stop bot"));
    }
    setActionLoading(false);
  };

  const handleRestart = async () => {
    setActionLoading(true);
    try {
      await restartBot(simulation);
      setActionError(null);
    } catch (err) {
      setActionError(getErrorText(err, "failed to restart bot"));
    }
    setActionLoading(false);
  };

  const handleUpdate = async () => {
    if (updateDownloading) return;
    if (!pendingUpdate) return;
    setUpdateDownloading(true);
    try {
      await pendingUpdate.downloadAndInstall();
      setUpdateVersion(null);
      setPendingUpdate(null);
    } catch {
      // keep banner visible for retry
    }
    setUpdateDownloading(false);
  };

  const displayError =
    errorMessage?.trim() || actionError || tradeError || balanceError;

  const handleProfileSwitch = async (id: string) => {
    setActiveProfileId(id);
    await loadSimulationForProfile(id);
    await Promise.all([refreshTradeData(), refreshBalance()]);
  };

  const dashboardView = buildDashboardViewModel({
    isRunning,
    displayError,
    positions,
    trades,
    simulation,
    savedConfig,
    stats,
    uiSummary,
    uiStrategies,
  });
  const pnlColor =
    dashboardView.pnlValue >= 0 ? "var(--green)" : "var(--red)";

  const railItems = [
    { label: "Dashboard", to: "/dashboard" },
    { label: "Manual Trade", to: "/manual" },
    { label: "Settings", to: "/config" },
    { label: "Open Logs", onClick: () => setLogsOpen(true) },
  ];

  return (
    <AppShell
      railSubtitle="Trading desk"
      railItems={railItems}
      railChildren={
        <SectionPanel title="Bot Status" subtitle="The main screen stays focused on trading, not technical noise.">
          <div className="flex flex-wrap items-center gap-3">
            <StatusBadge status={status} />
            <InfoPill tone={simulation ? "warning" : "success"}>
              {simulation ? "Dry Run" : "Live"}
            </InfoPill>
            {activeProfileId ? (
              <InfoPill tone="accent">Profile loaded</InfoPill>
            ) : null}
          </div>
        </SectionPanel>
      }
      eyebrow="Today"
      title="Trading at a Glance"
      description="Minimal trading-first workspace for bot state, positions, and recent order flow."
      meta={
        <>
          <StatusBadge status={status} />
          <InfoPill tone={simulation ? "warning" : "success"}>
            {simulation ? "Dry Run" : "Live Trading"}
          </InfoPill>
          <ProfileSwitcher
            activeProfileId={activeProfileId}
            onSwitch={(id) => {
              void handleProfileSwitch(id);
            }}
          />
        </>
      }
      banner={
        <UpdateBanner
          version={updateDownloading ? "Downloading..." : updateVersion}
          onUpdate={handleUpdate}
        />
      }
      contentClassName="page-stack"
    >
      <SectionPanel title="Bot Control" subtitle="Simple actions only. Use logs only when you need deeper troubleshooting.">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <button
              onClick={() => setSimulation(!simulation)}
              disabled={isRunning}
              className="ui-button text-sm"
            >
              {simulation ? "Switch to Live" : "Switch to Dry Run"}
            </button>
            {!isRunning ? (
              <button
                onClick={handleStart}
                disabled={actionLoading}
                className="ui-button ui-button--primary text-sm"
              >
                Start
              </button>
            ) : (
              <>
                <button
                  onClick={handleRestart}
                  disabled={actionLoading}
                  className="ui-button ui-button--accent text-sm"
                >
                  Restart
                </button>
                <button
                  onClick={handleStop}
                  disabled={actionLoading}
                  className="ui-button ui-button--danger text-sm"
                >
                  Stop
                </button>
              </>
            )}
          </div>
          <button
            onClick={() => setLogsOpen(true)}
            className="ui-button text-sm"
          >
            Open Logs
          </button>
        </div>
      </SectionPanel>

      {displayError ? (
        <div className="inline-alert">{displayError}</div>
      ) : null}
      {!displayError && (tradeStale || balanceStale) ? (
        <div className="inline-alert inline-alert--warning">
          Data is stale. Last known values are shown until the backend refresh recovers.
        </div>
      ) : null}

      <SectionPanel
        title="Active Trading"
        subtitle="Plain-English status instead of a noisy operator console."
        actions={<InfoPill tone={dashboardView.activity.tone}>{dashboardView.activity.eyebrow}</InfoPill>}
      >
        <div className="page-grid page-grid--two">
          <div className="space-y-3">
            <div>
              <div className="text-[clamp(1.9rem,1.55rem+0.9vw,2.5rem)] font-bold tracking-[-0.05em] leading-tight">
                {dashboardView.activity.headline}
              </div>
              <p className="mt-2 max-w-2xl text-sm text-[var(--text-secondary)]">
                {dashboardView.activity.detail}
              </p>
            </div>

            <div className="space-y-3">
              {dashboardView.enabledStrategies.length === 0 ? (
                <EmptyState
                  title="No strategies are turned on"
                  description="Turn on the strategies you want in Settings, then come back here to start the bot."
                />
              ) : (
                dashboardView.enabledStrategies.map((strategy) => (
                  <div
                    key={strategy.key}
                    className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4"
                  >
                    <div className="flex flex-wrap items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="text-base font-semibold text-[var(--text-primary)]">
                          {strategy.label}
                        </div>
                        <div className="mt-1 text-sm text-[var(--text-secondary)]">
                          {strategy.summary}
                        </div>
                      </div>
                      <InfoPill tone={strategy.stateTone}>{strategy.stateLabel}</InfoPill>
                    </div>
                    <div className="mt-3 text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                      Scope
                    </div>
                    <div className="mt-1 text-sm text-[var(--text-primary)]">
                      {strategy.scopeSummary}
                    </div>
                    {strategy.blockerReason ? (
                      <div className="mt-3 rounded-[14px] border border-[rgba(255,255,255,0.08)] bg-[rgba(255,255,255,0.03)] px-3 py-2 text-sm text-[var(--text-secondary)]">
                        {strategy.blockerReason}
                      </div>
                    ) : null}
                  </div>
                ))
              )}
            </div>
          </div>

          <div className="space-y-3">
            <div className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4">
              <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                Recent result
              </div>
              <div className="mt-2 text-xl font-semibold tracking-[-0.03em] text-[var(--text-primary)]">
                {dashboardView.recentResult}
              </div>
              {dashboardView.latestTrade ? (
                <div className="mt-2 text-sm text-[var(--text-secondary)]">
                  Latest activity at {formatClock(dashboardView.latestTrade.timestamp)}
                </div>
              ) : null}
            </div>

            <div className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4">
              <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                If nothing happens
              </div>
              <div className="mt-2 text-xl font-semibold tracking-[-0.03em] text-[var(--text-primary)]">
                {dashboardView.idleHelp}
              </div>
            </div>
          </div>
        </div>
      </SectionPanel>

      <div className="page-grid page-grid--three">
        <StatCard
          label="Open Positions"
          value={String(dashboardView.openPositionsCount)}
        />
        <StatCard
          label="Recent Orders"
          value={String(dashboardView.recentOrdersCount)}
        />
        <StatCard
          label="Free Balance"
          value={formatCurrency(dashboardView.freeBalanceValue ?? balance)}
        />
        <StatCard
          label="Avg Ack"
          value={dashboardView.avgAckLatency}
        />
        <StatCard
          label="Total PnL"
          value={formatCurrency(dashboardView.pnlValue)}
          color={pnlColor}
        />
      </div>

      <div className="page-grid page-grid--two">
        <SectionPanel title="Open Positions" subtitle="What is live right now.">
          <div className="space-y-3">
            {positions.length === 0 ? (
              <EmptyState title="No open positions" />
            ) : (
              positions.map((pos, index) => {
                const pnl =
                  typeof pos.unrealized_pnl === "number"
                    ? pos.unrealized_pnl
                    : pos.realized_pnl;
                return (
                  <div
                    key={`${pos.market}-${index}`}
                    className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4"
                  >
                    <div className="flex flex-wrap items-start justify-between gap-3">
                      <div>
                        <div className="text-base font-semibold text-[var(--text-primary)]">
                          {pos.market}
                        </div>
                        <div className="mt-1 text-sm text-[var(--text-secondary)]">
                          {describePositionPrices(pos)}
                        </div>
                      </div>
                      <InfoPill tone={pos.side === "long" || pos.side === "buy" ? "success" : "danger"}>
                        {pos.side.toUpperCase()}
                      </InfoPill>
                    </div>

                      <div className="mt-4 flex flex-wrap items-center gap-4 text-sm">
                      <div className="text-[var(--text-secondary)]">
                        Size <span className="mono-data text-[var(--text-primary)]">{formatQuantity(pos.size)}</span>
                      </div>
                      <div
                        className="mono-data font-semibold"
                        style={{ color: pnl >= 0 ? "var(--green)" : "var(--red)" }}
                      >
                        {formatCurrency(pnl)}
                      </div>
                    </div>
                  </div>
                );
              })
            )}
          </div>
        </SectionPanel>

        <SectionPanel title="Recent Trades" subtitle="Latest fills and outcomes.">
          <div className="space-y-3">
            {trades.length === 0 ? (
              <EmptyState title="No recent trades" />
            ) : (
              trades.slice(0, 8).map((trade) => {
                const outcomeTone =
                  trade.outcome === "win"
                    ? "success"
                    : trade.outcome === "loss"
                    ? "danger"
                    : "neutral";
                return (
                  <div
                    key={trade.id}
                    className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4"
                  >
                    <div className="flex flex-wrap items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="text-base font-semibold text-[var(--text-primary)]">
                          {trade.market}
                        </div>
                        <div className="mt-1 text-sm text-[var(--text-secondary)]">
                          {describeTradeFill(trade)}
                        </div>
                      </div>
                      <InfoPill tone={outcomeTone}>{trade.outcome}</InfoPill>
                    </div>

                    <div className="mt-4 flex flex-wrap items-center gap-4 text-sm">
                      <div className="text-[var(--text-secondary)]">
                        {formatClock(trade.timestamp)}
                      </div>
                      <div
                        className="mono-data font-semibold"
                        style={{ color: trade.pnl >= 0 ? "var(--green)" : "var(--red)" }}
                      >
                        {formatCurrency(trade.pnl)}
                      </div>
                    </div>
                  </div>
                );
              })
            )}
          </div>
        </SectionPanel>
      </div>
      <LogsDrawer open={logsOpen} mode="bot" onClose={() => setLogsOpen(false)} />
    </AppShell>
  );
}

