import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { check } from "@tauri-apps/plugin-updater";
import { AppShell } from "../components/AppShell";
import { EmptyState } from "../components/EmptyState";
import { InfoPill } from "../components/InfoPill";
import { LogsDrawer } from "../components/LogsDrawer";
import { MarketBadge } from "../components/MarketBadge";
import { ProfileSwitcher } from "../components/ProfileSwitcher";
import { SectionPanel } from "../components/SectionPanel";
import { StatusBadge } from "../components/StatusBadge";
import { UpdateBanner } from "../components/UpdateBanner";
import { useBotStatus } from "../hooks/useBotStatus";
import {
  type PortfolioHistoryRow,
  type PortfolioOpenOrderRow,
  type PortfolioPositionRow,
  type TradeStats,
  type UiDashboardSummary,
  botApiRequest,
  getActiveProfileId,
  getPortfolioHistory,
  getPortfolioOpenOrders,
  getPortfolioPositions,
  getSavedConfig,
  getTradeStats,
  getWalletBalance,
  restartBot,
  startBot,
  stopBot,
} from "../lib/tauri-commands";
import {
  formatCents,
  formatCurrency,
  formatLatency,
  formatRelativeTime,
  formatUsd,
} from "../lib/ui-adapters";

type PortfolioTab = "positions" | "open-orders" | "history";

function TabButton({
  label,
  active,
  onClick,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`workspace-tab ${active ? "workspace-tab--active" : ""}`}
    >
      {label}
    </button>
  );
}

function SummaryMetric({
  label,
  value,
  detail,
  tone = "neutral",
}: {
  label: string;
  value: string;
  detail?: string;
  tone?: "neutral" | "success" | "danger" | "accent";
}) {
  return (
    <div className="desk-metric">
      <div className="desk-metric__label">{label}</div>
      <div className={`desk-metric__value desk-metric__value--${tone}`}>{value}</div>
      {detail ? <div className="desk-metric__detail">{detail}</div> : null}
    </div>
  );
}

function DeskButton({
  children,
  tone = "default",
  onClick,
  disabled,
}: {
  children: ReactNode;
  tone?: "default" | "primary" | "danger";
  onClick: () => void;
  disabled?: boolean;
}) {
  const className =
    tone === "primary"
      ? "ui-button ui-button--primary"
      : tone === "danger"
      ? "ui-button ui-button--danger"
      : "ui-button";
  return (
    <button type="button" onClick={onClick} disabled={disabled} className={className}>
      {children}
    </button>
  );
}

function PositionRow({ row }: { row: PortfolioPositionRow }) {
  const valueTone = row.pnl_usd > 0 ? "success" : row.pnl_usd < 0 ? "danger" : "neutral";
  return (
    <div className="portfolio-row portfolio-row--positions">
      <div className="portfolio-market">
        <MarketBadge
          title={row.market_title}
          symbol={row.symbol}
          imageUrl={row.image_url}
          iconUrl={row.icon_url}
        />
        <div className="portfolio-market__copy">
          <div className="portfolio-market__title">{row.market_title}</div>
          <div className="portfolio-market__meta">
            <InfoPill tone="accent">{row.side_label}</InfoPill>
            <span className="mono-data">{row.shares.toFixed(row.shares >= 100 ? 0 : 2)} shares</span>
          </div>
        </div>
      </div>
      <div className="portfolio-cell">
        <div className="portfolio-cell__value">
          {formatCents(row.avg_price)}{row.current_price !== null ? ` -> ${formatCents(row.current_price)}` : ""}
        </div>
        <div className="portfolio-cell__label">Avg to now</div>
      </div>
      <div className="portfolio-cell">
        <div className="portfolio-cell__value">{formatUsd(row.traded_usd)}</div>
        <div className="portfolio-cell__label">Traded</div>
      </div>
      <div className="portfolio-cell">
        <div className="portfolio-cell__value">{formatUsd(row.to_win_usd)}</div>
        <div className="portfolio-cell__label">To win</div>
      </div>
      <div className="portfolio-cell portfolio-cell--value">
        <div className="portfolio-cell__value">{formatUsd(row.value_usd)}</div>
        <div className={`portfolio-cell__pnl portfolio-cell__pnl--${valueTone}`}>
          {formatCurrency(row.pnl_usd)}
        </div>
      </div>
      <div className="portfolio-action">
        <button type="button" className="portfolio-action__button" disabled>
          {row.action_label}
        </button>
      </div>
    </div>
  );
}

function OpenOrderRow({ row }: { row: PortfolioOpenOrderRow }) {
  return (
    <div className="portfolio-row portfolio-row--orders">
      <div className="portfolio-market">
        <MarketBadge
          title={row.market_title}
          symbol={row.symbol}
          imageUrl={row.image_url}
          iconUrl={row.icon_url}
        />
        <div className="portfolio-market__copy">
          <div className="portfolio-market__title">{row.market_title}</div>
          <div className="portfolio-market__meta">
            <span>{row.market_subtitle}</span>
          </div>
        </div>
      </div>
      <div className="portfolio-cell">
        <div className="portfolio-cell__value">{row.order_count}</div>
        <div className="portfolio-cell__label">Orders</div>
      </div>
      <div className="portfolio-cell">
        <div className="portfolio-cell__value">{formatUsd(row.total_size_usd)}</div>
        <div className="portfolio-cell__label">Quoted</div>
      </div>
      <div className="portfolio-cell">
        <div className="portfolio-cell__value">
          {row.updated_at ? formatRelativeTime(row.updated_at) : "--"}
        </div>
        <div className="portfolio-cell__label">Updated</div>
      </div>
      <div className="portfolio-action">
        <button type="button" className="portfolio-action__button" disabled>
          {row.action_label}
        </button>
      </div>
    </div>
  );
}

function HistoryRow({ row }: { row: PortfolioHistoryRow }) {
  const valueTone = row.pnl_usd > 0 ? "success" : row.pnl_usd < 0 ? "danger" : "neutral";
  return (
    <div className="portfolio-row portfolio-row--history">
      <div className="portfolio-history__activity">
        <div className="portfolio-history__badge">
          {row.action_label === "Exit" ? "-" : "+"}
        </div>
        <div className="portfolio-history__label">{row.action_label}</div>
      </div>
      <div className="portfolio-market">
        <MarketBadge
          title={row.market_title}
          symbol={row.symbol}
          imageUrl={row.image_url}
          iconUrl={row.icon_url}
        />
        <div className="portfolio-market__copy">
          <div className="portfolio-market__title">{row.market_title}</div>
          <div className="portfolio-market__meta">
            <InfoPill tone="accent">{row.side_label}</InfoPill>
            <span className="mono-data">{row.shares.toFixed(row.shares >= 100 ? 0 : 2)} shares</span>
          </div>
        </div>
      </div>
      <div className={`portfolio-cell__value portfolio-cell__pnl portfolio-cell__pnl--${valueTone}`}>
        {formatUsd(row.value_usd)}
      </div>
      <div className="portfolio-cell__value">{formatRelativeTime(row.timestamp)}</div>
    </div>
  );
}

export function Dashboard() {
  const { status, isRunning, errorMessage } = useBotStatus();
  const [activeProfileId, setActiveProfileId] = useState<string | null>(null);
  const [simulation, setSimulation] = useState(true);
  const [actionLoading, setActionLoading] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [updateVersion, setUpdateVersion] = useState<string | null>(null);
  const [updateDownloading, setUpdateDownloading] = useState(false);
  const [pendingUpdate, setPendingUpdate] =
    useState<Awaited<ReturnType<typeof check>> | null>(null);
  const [logsOpen, setLogsOpen] = useState(false);
  const [workspaceTab, setWorkspaceTab] = useState<PortfolioTab>("positions");
  const [search, setSearch] = useState("");
  const [summary, setSummary] = useState<UiDashboardSummary | null>(null);
  const [positions, setPositions] = useState<PortfolioPositionRow[]>([]);
  const [openOrders, setOpenOrders] = useState<PortfolioOpenOrderRow[]>([]);
  const [history, setHistory] = useState<PortfolioHistoryRow[]>([]);
  const [stats, setStats] = useState<TradeStats | null>(null);
  const [walletBalance, setWalletBalance] = useState<number | null>(null);
  const [workspaceError, setWorkspaceError] = useState<string | null>(null);

  const loadSimulationForProfile = useCallback(async (profileId: string | null) => {
    if (!profileId) return;
    try {
      const saved = await getSavedConfig(profileId);
      setSimulation(saved.simulation);
    } catch {
      // keep existing mode
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
    void (async () => {
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

  const loadSummary = useCallback(async () => {
    if (!isRunning) {
      setSummary(null);
      return;
    }
    try {
      const response = await botApiRequest<{ summary?: UiDashboardSummary }>("GET", "/ui/summary");
      setSummary(response.summary ?? null);
    } catch {
      setSummary(null);
    }
  }, [isRunning]);

  const refreshWorkspace = useCallback(async () => {
    const results = await Promise.allSettled([
      getPortfolioPositions(),
      getPortfolioOpenOrders(),
      getPortfolioHistory(80),
      getTradeStats(),
      getWalletBalance(),
    ]);

    const [positionsResult, ordersResult, historyResult, statsResult, balanceResult] = results;

    if (positionsResult.status === "fulfilled") {
      setPositions(positionsResult.value);
    }
    if (ordersResult.status === "fulfilled") {
      setOpenOrders(ordersResult.value);
    }
    if (historyResult.status === "fulfilled") {
      setHistory(historyResult.value);
    }
    if (statsResult.status === "fulfilled") {
      setStats(statsResult.value);
    }
    if (balanceResult.status === "fulfilled") {
      setWalletBalance(balanceResult.value);
    }

    const allFailed = results.every((result) => result.status === "rejected");
    setWorkspaceError(allFailed ? "Portfolio data is not available yet." : null);
  }, []);

  useEffect(() => {
    void refreshWorkspace();
    void loadSummary();
    const timer = setInterval(() => {
      void refreshWorkspace();
      void loadSummary();
    }, isRunning ? 5000 : 12000);
    return () => clearInterval(timer);
  }, [isRunning, loadSummary, refreshWorkspace]);

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

  const handleRestart = async () => {
    setActionLoading(true);
    try {
      await restartBot(simulation);
      setActionError(null);
    } catch (err) {
      setActionError(getErrorText(err, "failed to restart bot"));
    } finally {
      setActionLoading(false);
    }
  };

  const handleUpdate = async () => {
    if (updateDownloading || !pendingUpdate) return;
    setUpdateDownloading(true);
    try {
      await pendingUpdate.downloadAndInstall();
      setUpdateVersion(null);
      setPendingUpdate(null);
    } finally {
      setUpdateDownloading(false);
    }
  };

  const handleProfileSwitch = async (id: string) => {
    setActiveProfileId(id);
    await loadSimulationForProfile(id);
    await refreshWorkspace();
  };

  const displayError = errorMessage?.trim() || actionError || workspaceError;
  const positionsValue = positions.reduce((total, row) => total + row.value_usd, 0);
  const portfolioValue = (walletBalance ?? 0) + positionsValue;
  const enabledStrategyCount = summary?.enabled_strategies.length ?? 0;
  const headline = summary?.headline || (isRunning ? "Watching markets" : "Trading is stopped");
  const detail =
    summary?.detail ||
    (isRunning
      ? "EVPOLY is running. Use the portfolio tabs below to inspect positions, open orders, and recent fills."
      : "Press Start when you want EVPOLY to begin watching and trading again.");

  const searchNeedle = search.trim().toLowerCase();
  const filteredPositions = useMemo(
    () =>
      positions.filter((row) =>
        [row.market_title, row.market_subtitle, row.symbol || ""]
          .join(" ")
          .toLowerCase()
          .includes(searchNeedle)
      ),
    [positions, searchNeedle]
  );
  const filteredOpenOrders = useMemo(
    () =>
      openOrders.filter((row) =>
        [row.market_title, row.market_subtitle, row.symbol || ""]
          .join(" ")
          .toLowerCase()
          .includes(searchNeedle)
      ),
    [openOrders, searchNeedle]
  );
  const filteredHistory = useMemo(
    () =>
      history.filter((row) =>
        [row.market_title, row.market_subtitle, row.side_label, row.symbol || ""]
          .join(" ")
          .toLowerCase()
          .includes(searchNeedle)
      ),
    [history, searchNeedle]
  );

  const currentTabLabel =
    workspaceTab === "positions"
      ? "Search positions"
      : workspaceTab === "open-orders"
      ? "Search open orders"
      : "Search history";

  const railItems = [
    { label: "Portfolio", to: "/dashboard" },
    { label: "Manual Trade", to: "/manual" },
    { label: "Settings", to: "/config" },
    { label: "Open Logs", onClick: () => setLogsOpen(true) },
  ];

  return (
    <AppShell
      railSubtitle="Desktop trading"
      railItems={railItems}
      eyebrow="Portfolio"
      title="Desktop Trading Workspace"
      description="Positions, open orders, and history in one dense workspace instead of scattered cards."
      meta={
        <>
          <StatusBadge status={status} />
          <InfoPill tone={simulation ? "warning" : "success"}>
            {simulation ? "Dry Run" : "Live"}
          </InfoPill>
          <ProfileSwitcher activeProfileId={activeProfileId} onSwitch={(id) => void handleProfileSwitch(id)} />
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
      <SectionPanel
        title="Trading desk"
        subtitle={detail}
        actions={<InfoPill tone={summary?.blocker_reason ? "warning" : isRunning ? "success" : "neutral"}>{summary?.mode === "dry_run" ? "Dry Run" : isRunning ? "Watching" : "Stopped"}</InfoPill>}
        bodyClassName="desk-panel__body"
      >
        <div className="desk-strip">
          <div className="desk-strip__copy">
            <div className="desk-strip__headline">{headline}</div>
            <div className="desk-strip__detail">
              {summary?.blocker_reason || "This workspace stays focused on inventory and actions, not operator noise."}
            </div>
          </div>
          <div className="desk-strip__actions">
            <DeskButton onClick={() => setSimulation((value) => !value)} disabled={isRunning}>
              {simulation ? "Switch to Live" : "Switch to Dry Run"}
            </DeskButton>
            {!isRunning ? (
              <DeskButton tone="primary" onClick={handleStart} disabled={actionLoading}>
                Start
              </DeskButton>
            ) : (
              <>
                <DeskButton onClick={handleRestart} disabled={actionLoading}>
                  Restart
                </DeskButton>
                <DeskButton tone="danger" onClick={handleStop} disabled={actionLoading}>
                  Stop
                </DeskButton>
              </>
            )}
            <DeskButton onClick={() => setLogsOpen(true)}>Open Logs</DeskButton>
          </div>
        </div>

        <div className="desk-metrics">
          <SummaryMetric
            label="Portfolio"
            value={formatUsd(portfolioValue)}
            detail={`${positions.length} live ${positions.length === 1 ? "position" : "positions"}`}
          />
          <SummaryMetric
            label="Available to trade"
            value={formatUsd(walletBalance)}
            detail={enabledStrategyCount > 0 ? `${enabledStrategyCount} strategies enabled` : "No strategies enabled"}
          />
          <SummaryMetric
            label="Profit / Loss"
            value={formatUsd(stats?.total_pnl ?? summary?.total_pnl ?? 0)}
            detail={`Avg ack ${formatLatency(summary?.avg_ack_latency_ms ?? stats?.avg_ack_latency_ms ?? null)}`}
            tone={(stats?.total_pnl ?? summary?.total_pnl ?? 0) >= 0 ? "success" : "danger"}
          />
        </div>
      </SectionPanel>

      {displayError ? <div className="inline-alert">{displayError}</div> : null}

      <SectionPanel
        title="Portfolio"
        subtitle="Use the same familiar structure every trader expects: positions, open orders, and history."
        className="workspace-panel"
        bodyClassName="workspace-panel__body"
      >
        <div className="workspace-toolbar">
          <div className="workspace-tabs">
            <TabButton label="Positions" active={workspaceTab === "positions"} onClick={() => setWorkspaceTab("positions")} />
            <TabButton label="Open orders" active={workspaceTab === "open-orders"} onClick={() => setWorkspaceTab("open-orders")} />
            <TabButton label="History" active={workspaceTab === "history"} onClick={() => setWorkspaceTab("history")} />
          </div>
          <div className="workspace-toolbar__actions">
            <input
              type="text"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              placeholder={currentTabLabel}
              className="workspace-search"
            />
            <button type="button" onClick={() => void refreshWorkspace()} className="ui-button">
              Refresh
            </button>
          </div>
        </div>

        {workspaceTab === "positions" ? (
          filteredPositions.length === 0 ? (
            <EmptyState title="No open positions" description="When EVPOLY is live in the market, positions will show up here." />
          ) : (
            <div className="portfolio-table">
              <div className="portfolio-table__header portfolio-table__header--positions">
                <span>Market</span>
                <span>Avg to now</span>
                <span>Traded</span>
                <span>To win</span>
                <span>Value</span>
                <span />
              </div>
              <div className="portfolio-table__body">
                {filteredPositions.map((row) => (
                  <PositionRow key={row.id} row={row} />
                ))}
              </div>
            </div>
          )
        ) : null}

        {workspaceTab === "open-orders" ? (
          filteredOpenOrders.length === 0 ? (
            <EmptyState title="No open orders" description="Live pending quotes and resting orders will appear here." />
          ) : (
            <div className="portfolio-table">
              <div className="portfolio-table__header portfolio-table__header--orders">
                <span>Market</span>
                <span>Orders</span>
                <span>Quoted</span>
                <span>Updated</span>
                <span />
              </div>
              <div className="portfolio-table__body">
                {filteredOpenOrders.map((row) => (
                  <OpenOrderRow key={row.id} row={row} />
                ))}
              </div>
            </div>
          )
        ) : null}

        {workspaceTab === "history" ? (
          filteredHistory.length === 0 ? (
            <EmptyState title="No history yet" description="Recent fills and outcomes will appear here once the bot starts trading." />
          ) : (
            <div className="portfolio-table">
              <div className="portfolio-table__header portfolio-table__header--history">
                <span>Activity</span>
                <span>Market</span>
                <span>Value</span>
                <span>Time</span>
              </div>
              <div className="portfolio-table__body">
                {filteredHistory.map((row) => (
                  <HistoryRow key={row.id} row={row} />
                ))}
              </div>
            </div>
          )
        ) : null}
      </SectionPanel>
      <LogsDrawer open={logsOpen} mode="bot" onClose={() => setLogsOpen(false)} />
    </AppShell>
  );
}
