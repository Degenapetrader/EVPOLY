import { useCallback, useEffect, useMemo, useState } from "react";
import { AppShell } from "../components/AppShell";
import { EmptyState } from "../components/EmptyState";
import { InfoPill } from "../components/InfoPill";
import { LogsDrawer } from "../components/LogsDrawer";
import { MarketPickerShell } from "../components/MarketPickerShell";
import { SectionPanel } from "../components/SectionPanel";
import { StatusBadge } from "../components/StatusBadge";
import {
  getActiveProfileId,
  getManualServiceStatus,
  getSavedConfig,
  manualApiRequest,
  startManualService,
  stopManualService,
  type UiManualBalanceSummary,
  type UiManualHealth,
  type UiManualPosition,
  type UiManualRun,
  type UiMarket,
} from "../lib/tauri-commands";
import {
  asRecord,
  buildManualOverview,
  buildMarketPickerItems,
  formatShares,
  formatUsd,
  readString,
  sentenceCase,
  type RawManualRun,
} from "../lib/ui-adapters";

type ManualOrderState = {
  conditionId: string;
  side: "up" | "down";
  size: string;
  sizeUnit: "shares" | "usd";
  mode: "chase_limit" | "limit" | "market";
};

type TicketKind = "open" | "close";

type ManualRunsResponse = {
  runs?: RawManualRun[];
  ui_runs?: UiManualRun[];
};

type ManualPositionsResponse = {
  positions?: unknown[];
  ui_positions?: UiManualPosition[];
};

type ManualBalanceResponse = {
  ui_summary?: UiManualBalanceSummary;
  [key: string]: unknown;
};

type ManualHealthResponse = {
  ui_health?: UiManualHealth;
  [key: string]: unknown;
};

type ManualMarketsResponse = {
  markets?: UiMarket[];
};

type ManualMarketResponse = {
  market?: UiMarket;
};

const DEFAULT_ORDER: ManualOrderState = {
  conditionId: "",
  side: "up",
  size: "10",
  sizeUnit: "shares",
  mode: "chase_limit",
};

function parseRuns(payload: unknown): RawManualRun[] {
  const record = asRecord(payload);
  if (!record) return [];
  const uiRuns = record.ui_runs;
  if (Array.isArray(uiRuns)) {
    return uiRuns.filter((item) => item && typeof item === "object") as RawManualRun[];
  }
  const runs = record.runs;
  if (!Array.isArray(runs)) return [];
  return runs.filter((item) => item && typeof item === "object") as RawManualRun[];
}

function parseMarkets(payload: unknown): UiMarket[] {
  const record = asRecord(payload);
  if (!record || !Array.isArray(record.markets)) return [];
  return record.markets.filter((item) => item && typeof item === "object") as UiMarket[];
}

function parseMarket(payload: unknown): UiMarket | null {
  const record = asRecord(payload);
  if (!record) return null;
  const market = record.market;
  if (!market || typeof market !== "object" || Array.isArray(market)) {
    return null;
  }
  return market as UiMarket;
}

function TicketChoice({
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
      className={`rounded-full border px-4 py-2 text-sm transition-colors ${
        active
          ? "border-[rgba(54,211,153,0.28)] bg-[rgba(20,35,29,0.94)] text-[var(--text-primary)]"
          : "border-[var(--border)] bg-[rgba(16,22,31,0.78)] text-[var(--text-secondary)]"
      }`}
    >
      {label}
    </button>
  );
}

function SnapshotRow({
  label,
  value,
  detail,
}: {
  label: string;
  value: string;
  detail?: string;
}) {
  return (
    <div className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-3">
      <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">{label}</div>
      <div className="mt-2 text-lg font-semibold tracking-[-0.03em] text-[var(--text-primary)]">
        {value}
      </div>
      {detail ? <div className="mt-1 text-sm text-[var(--text-secondary)]">{detail}</div> : null}
    </div>
  );
}

function InputField({
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
      <label className="mb-1.5 block text-xs text-[var(--text-secondary)]">{label}</label>
      <input
        type={type}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder={placeholder}
        className="w-full rounded-[16px] border border-[var(--border)] bg-[var(--bg-tertiary)] px-4 py-3 text-sm text-[var(--text-primary)] outline-none transition-colors focus:border-[var(--accent)]"
      />
    </div>
  );
}

function RunCard({
  kind,
  run,
  onStop,
}: {
  kind: TicketKind;
  run: RawManualRun;
  onStop: (kind: TicketKind, runId: string) => void;
}) {
  const runId = typeof run.run_id === "string" ? run.run_id : "";
  const title = run.market_title || (kind === "open" ? "Open run" : "Close run");
  const detailParts = [
    typeof run.market_subtitle === "string" && run.market_subtitle.trim()
      ? run.market_subtitle.trim()
      : null,
    typeof run.side_label === "string" && run.side_label.trim()
      ? run.side_label.trim()
      : run.side
      ? sentenceCase(run.side, "")
      : null,
    typeof run.progress_summary === "string" && run.progress_summary.trim()
      ? run.progress_summary.trim()
      : typeof run.target_shares === "number"
      ? `${formatShares(run.target_shares)} shares target`
      : null,
  ].filter(Boolean);

  return (
    <div className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="text-sm font-semibold text-[var(--text-primary)]">{title}</div>
          <div className="mt-1 break-all text-xs text-[var(--text-secondary)]">
            {runId || run.condition_id || "Waiting for run details"}
          </div>
          <div className="mt-2 text-sm text-[var(--text-secondary)]">
            {detailParts.length > 0 ? detailParts.join(" / ") : "Waiting for more details"}
          </div>
        </div>
        <InfoPill tone={String(run.status ?? "").startsWith("running") ? "success" : "neutral"}>
          {typeof run.status_label === "string" && run.status_label.trim()
            ? run.status_label
            : sentenceCase(run.status as string | undefined, "Active")}
        </InfoPill>
      </div>
      {runId ? (
        <button
          type="button"
          onClick={() => onStop(kind, runId)}
          className="ui-button mt-4 min-h-[40px] px-3 py-2 text-sm"
        >
          Stop run
        </button>
      ) : null}
    </div>
  );
}

export function Manual() {
  const [serviceStatus, setServiceStatus] = useState("stopped");
  const [serviceStopping, setServiceStopping] = useState(false);
  const [simulation, setSimulation] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const [health, setHealth] = useState<ManualHealthResponse | null>(null);
  const [balance, setBalance] = useState<ManualBalanceResponse | null>(null);
  const [positions, setPositions] = useState<ManualPositionsResponse | null>(null);
  const [openRuns, setOpenRuns] = useState<RawManualRun[]>([]);
  const [closeRuns, setCloseRuns] = useState<RawManualRun[]>([]);

  const [ticketKind, setTicketKind] = useState<TicketKind>("open");
  const [openOrder, setOpenOrder] = useState<ManualOrderState>(DEFAULT_ORDER);
  const [closeOrder, setCloseOrder] = useState<ManualOrderState>(DEFAULT_ORDER);
  const [selectedOpenMarket, setSelectedOpenMarket] = useState<UiMarket | null>(null);
  const [selectedCloseMarket, setSelectedCloseMarket] = useState<UiMarket | null>(null);
  const [marketSearch, setMarketSearch] = useState("");
  const [marketSearchLoading, setMarketSearchLoading] = useState(false);
  const [searchResults, setSearchResults] = useState<UiMarket[]>([]);
  const [recentMarkets, setRecentMarkets] = useState<UiMarket[]>([]);
  const [logsOpen, setLogsOpen] = useState(false);

  const serviceRunning = serviceStatus.startsWith("running");
  const activeOrder = ticketKind === "open" ? openOrder : closeOrder;
  const activeSelectedMarket =
    ticketKind === "open" ? selectedOpenMarket : selectedCloseMarket;

  const setActiveOrder = (patch: Partial<ManualOrderState>) => {
    if (ticketKind === "open") {
      setOpenOrder((previous) => ({ ...previous, ...patch }));
      return;
    }
    setCloseOrder((previous) => ({ ...previous, ...patch }));
  };

  const setSelectedMarketForKind = useCallback(
    (kind: TicketKind, market: UiMarket | null) => {
      if (kind === "open") {
        setSelectedOpenMarket(market);
        return;
      }
      setSelectedCloseMarket(market);
    },
    []
  );

  const refreshStatus = useCallback(async () => {
    try {
      const next = await getManualServiceStatus();
      setServiceStatus(next);
      if (!next.startsWith("running")) {
        setServiceStopping(false);
      }
    } catch (err) {
      setServiceStatus(`error:${String(err)}`);
      setServiceStopping(false);
    }
  }, []);

  const fetchRecentMarkets = useCallback(async () => {
    if (!serviceRunning || serviceStopping) {
      setRecentMarkets([]);
      return;
    }
    try {
      const response = await manualApiRequest<ManualMarketsResponse>(
        "GET",
        "/manual/markets/recent",
        { limit: 8 }
      );
      setRecentMarkets(parseMarkets(response));
    } catch {
      setRecentMarkets([]);
    }
  }, [serviceRunning, serviceStopping]);

  const fetchOverview = useCallback(async () => {
    if (!serviceRunning || serviceStopping) return;
    try {
      const [healthResp, balanceResp, positionsResp, openRunsResp, closeRunsResp] =
        await Promise.all([
          manualApiRequest<ManualHealthResponse>("GET", "/manual/health"),
          manualApiRequest<ManualBalanceResponse>("GET", "/manual/balance"),
          manualApiRequest<ManualPositionsResponse>("GET", "/manual/positions"),
          manualApiRequest<ManualRunsResponse>("GET", "/manual/open/runs"),
          manualApiRequest<ManualRunsResponse>("GET", "/manual/close/runs"),
        ]);
      setHealth(healthResp);
      setBalance(balanceResp);
      setPositions(positionsResp);
      setOpenRuns(parseRuns(openRunsResp));
      setCloseRuns(parseRuns(closeRunsResp));
    } catch (err) {
      if (serviceStopping) {
        return;
      }
      setError(String(err));
    }
  }, [serviceRunning, serviceStopping]);

  useEffect(() => {
    void (async () => {
      try {
        const profileId = await getActiveProfileId();
        if (!profileId) return;
        const saved = await getSavedConfig(profileId);
        setSimulation(saved.simulation);
      } catch {
        // keep current mode
      }
    })();
  }, []);

  useEffect(() => {
    void refreshStatus();
    const timer = setInterval(() => void refreshStatus(), 2500);
    return () => clearInterval(timer);
  }, [refreshStatus]);

  useEffect(() => {
    if (!serviceRunning || serviceStopping) return;
    void fetchOverview();
    void fetchRecentMarkets();
    const timer = setInterval(() => void fetchOverview(), 4000);
    return () => clearInterval(timer);
  }, [serviceRunning, serviceStopping, fetchOverview, fetchRecentMarkets]);

  useEffect(() => {
    if (!serviceRunning || serviceStopping) {
      setSearchResults([]);
      setMarketSearchLoading(false);
      return;
    }
    const query = marketSearch.trim();
    if (!query) {
      setSearchResults([]);
      setMarketSearchLoading(false);
      return;
    }

    const timer = setTimeout(() => {
      void (async () => {
        setMarketSearchLoading(true);
        try {
          const response = await manualApiRequest<ManualMarketsResponse>(
            "GET",
            "/manual/markets/search",
            { q: query, limit: 8 }
          );
          setSearchResults(parseMarkets(response));
        } catch {
          setSearchResults([]);
        } finally {
          setMarketSearchLoading(false);
        }
      })();
    }, 300);

    return () => clearTimeout(timer);
  }, [marketSearch, serviceRunning, serviceStopping]);

  useEffect(() => {
    const nextSelectedMarket =
      ticketKind === "open" ? selectedOpenMarket : selectedCloseMarket;
    setMarketSearch(nextSelectedMarket?.title ?? "");
  }, [ticketKind, selectedOpenMarket, selectedCloseMarket]);

  useEffect(() => {
    if (!serviceRunning || serviceStopping) return;
    const conditionId = activeOrder.conditionId.trim();
    if (!conditionId) return;
    if (activeSelectedMarket?.condition_id === conditionId) return;

    const timer = setTimeout(() => {
      void (async () => {
        try {
          const response = await manualApiRequest<ManualMarketResponse>(
            "GET",
            `/manual/markets/${encodeURIComponent(conditionId)}`
          );
          const market = parseMarket(response);
          if (market) {
            setSelectedMarketForKind(ticketKind, market);
          }
        } catch {
          // Keep the fallback ID flow usable even if lookup fails.
        }
      })();
    }, 250);

    return () => clearTimeout(timer);
  }, [
    activeOrder.conditionId,
    activeSelectedMarket,
    serviceRunning,
    serviceStopping,
    setSelectedMarketForKind,
    ticketKind,
  ]);

  const clearMessages = () => {
    setError(null);
    setNotice(null);
  };

  const railItems = [
    { label: "Dashboard", to: "/dashboard" },
    { label: "Manual Trade", to: "/manual" },
    { label: "Settings", to: "/config" },
    { label: "Open Logs", onClick: () => setLogsOpen(true) },
  ];

  const handleStart = async () => {
    clearMessages();
    setBusy(true);
    try {
      setServiceStopping(false);
      await startManualService(simulation);
      setNotice(`Manual service started in ${simulation ? "dry run" : "live"} mode.`);
      await refreshStatus();
      await Promise.all([fetchOverview(), fetchRecentMarkets()]);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const handleStop = async () => {
    clearMessages();
    setBusy(true);
    try {
      setServiceStopping(true);
      setServiceStatus("stopping");
      setHealth(null);
      setBalance(null);
      setPositions(null);
      setOpenRuns([]);
      setCloseRuns([]);
      setRecentMarkets([]);
      setSearchResults([]);
      await stopManualService();
      setNotice("Manual service stopped.");
      await refreshStatus();
    } catch (err) {
      setServiceStopping(false);
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const submitOrder = async () => {
    clearMessages();
    setBusy(true);
    try {
      const sizeNum = Number(activeOrder.size);
      if (!activeOrder.conditionId.trim()) {
        throw new Error("Choose a market first.");
      }
      if (!Number.isFinite(sizeNum) || sizeNum <= 0) {
        throw new Error("Size must be a positive number.");
      }

      const body: Record<string, unknown> = {
        condition_id: activeOrder.conditionId.trim(),
        side: activeOrder.side,
        size: sizeNum,
        mode: activeOrder.mode,
      };
      if (activeOrder.sizeUnit === "usd") {
        body.size_unit = "usd";
      }

      const response = await manualApiRequest<Record<string, unknown>>(
        "POST",
        ticketKind === "open" ? "/manual/open" : "/manual/close",
        undefined,
        body
      );

      const responseRecord = asRecord(response);
      const uiRun = asRecord(responseRecord?.ui_run);
      const runId =
        readString(uiRun, ["run_id"]) || readString(responseRecord, ["run_id", "id"]);
      const marketTitle =
        readString(uiRun, ["market_title"]) ||
        activeSelectedMarket?.title ||
        "selected market";

      setNotice(
        runId
          ? `${ticketKind === "open" ? "Open" : "Close"} request sent for ${marketTitle}. Run ${runId} is now active.`
          : `${ticketKind === "open" ? "Open" : "Close"} request sent for ${marketTitle}.`
      );
      await Promise.all([fetchOverview(), fetchRecentMarkets()]);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const stopRun = async (kind: TicketKind, runId: string) => {
    clearMessages();
    setBusy(true);
    try {
      await manualApiRequest(
        "POST",
        kind === "open" ? `/manual/open/runs/${runId}/stop` : `/manual/close/runs/${runId}/stop`
      );
      setNotice(`Stop requested for run ${runId}.`);
      await fetchOverview();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const manualOverview = buildManualOverview({
    health,
    balance,
    positions,
    openRuns,
    closeRuns,
    serviceRunning,
  });

  const searchItems = useMemo(() => buildMarketPickerItems(searchResults), [searchResults]);
  const recentItems = useMemo(() => buildMarketPickerItems(recentMarkets), [recentMarkets]);

  const reviewMarketLabel = activeSelectedMarket?.title
    ? activeSelectedMarket.title
    : activeOrder.conditionId.trim()
    ? "selected market"
    : "no market yet";
  const reviewText = `${ticketKind === "open" ? "Open" : "Close"} ${
    activeOrder.side === "up" ? "Up" : "Down"
  } on ${reviewMarketLabel} using ${activeOrder.size || "0"} ${activeOrder.sizeUnit} in ${
    activeOrder.mode === "chase_limit"
      ? "chase limit"
      : activeOrder.mode === "limit"
      ? "limit"
      : "market"
  } mode.`;

  return (
    <AppShell
      railSubtitle="Manual trade"
      railItems={railItems}
      railChildren={
        <SectionPanel
          title="One trade at a time"
          subtitle="Use manual mode when you want to guide a specific trade yourself."
        >
          <div className="space-y-3 text-sm text-[var(--text-secondary)]">
            <p>Start the manual service once, then submit a single ticket below.</p>
            <div className="flex flex-wrap gap-2">
              <InfoPill tone={simulation ? "warning" : "success"}>
                {simulation ? "Dry Run" : "Live"}
              </InfoPill>
              <StatusBadge status={serviceStatus} />
            </div>
          </div>
        </SectionPanel>
      }
      eyebrow="Manual control"
      title="Place a Manual Trade"
      description="Start the service, pick a market, and send one clean manual ticket."
      meta={
        <>
          <InfoPill tone={simulation ? "warning" : "success"}>
            {simulation ? "Dry Run" : "Live"}
          </InfoPill>
          <StatusBadge status={serviceStatus} />
        </>
      }
      contentClassName="page-stack"
    >
      <div className="page-split xl:grid-cols-[minmax(0,1.18fr)_minmax(20rem,0.82fr)]">
        <div className="space-y-[var(--space-6)]">
          <SectionPanel title="Service" subtitle="Start this once before sending a manual order.">
            <div className="grid gap-3 md:grid-cols-3">
              <SnapshotRow
                label="Service"
                value={serviceRunning ? "Running" : "Stopped"}
                detail={serviceRunning ? "Ready for manual requests." : "Start it when you want to trade."}
              />
              <SnapshotRow
                label="Mode"
                value={simulation ? "Dry Run" : "Live"}
                detail={simulation ? "No real orders will be sent." : "Real orders will be placed."}
              />
              <SnapshotRow
                label="Active runs"
                value={`${manualOverview.totalRuns}`}
                detail={
                  manualOverview.totalRuns === 1
                    ? "One run is active."
                    : "Open and close runs combined."
                }
              />
            </div>

            <div className="mt-5 flex flex-wrap gap-3">
              <button
                type="button"
                onClick={handleStart}
                disabled={busy}
                className="ui-button ui-button--primary"
              >
                Start Service
              </button>
              <button
                type="button"
                onClick={handleStop}
                disabled={busy}
                className="ui-button ui-button--danger"
              >
                Stop Service
              </button>
              <button
                type="button"
                onClick={() => void fetchOverview()}
                disabled={busy || !serviceRunning}
                className="ui-button"
              >
                Refresh
              </button>
              <button type="button" onClick={() => setLogsOpen(true)} className="ui-button">
                Open Logs
              </button>
            </div>

            <div className="mt-5">
              <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                Run mode
              </div>
              <div className="mt-3 flex flex-wrap gap-3">
                <TicketChoice label="Live" active={!simulation} onClick={() => setSimulation(false)} />
                <TicketChoice label="Dry Run" active={simulation} onClick={() => setSimulation(true)} />
              </div>
            </div>

            {error ? (
              <div className="inline-alert mt-5">{error}</div>
            ) : notice ? (
              <div className="mt-5 rounded-[18px] border border-[rgba(54,211,153,0.25)] bg-[rgba(20,35,29,0.92)] px-4 py-3 text-sm text-[#b5f1d0]">
                {notice}
              </div>
            ) : null}
          </SectionPanel>

          <SectionPanel title="Order Ticket" subtitle="Pick a market, set the side and size, then submit.">
            <div className="flex flex-wrap gap-3">
              <TicketChoice
                label="Open position"
                active={ticketKind === "open"}
                onClick={() => setTicketKind("open")}
              />
              <TicketChoice
                label="Close position"
                active={ticketKind === "close"}
                onClick={() => setTicketKind("close")}
              />
            </div>

            <div className="mt-5 grid gap-4">
              <MarketPickerShell
                searchValue={marketSearch}
                onSearchChange={setMarketSearch}
                searchReady={serviceRunning}
                results={searchItems}
                recent={recentItems}
                value={activeOrder.conditionId}
                onValueChange={(value) => {
                  setActiveOrder({ conditionId: value });
                  if (activeSelectedMarket?.condition_id !== value) {
                    setSelectedMarketForKind(ticketKind, null);
                  }
                }}
                disabled={busy}
              />

              {marketSearchLoading ? (
                <div className="text-sm text-[var(--text-secondary)]">Searching markets...</div>
              ) : null}

              {activeSelectedMarket ? (
                <div className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4">
                  <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                    Selected market
                  </div>
                  <div className="mt-2 text-base font-semibold text-[var(--text-primary)]">
                    {activeSelectedMarket.title}
                  </div>
                  <div className="mt-1 text-sm text-[var(--text-secondary)]">
                    {activeSelectedMarket.subtitle}
                  </div>
                </div>
              ) : null}

              <div className="grid gap-4 lg:grid-cols-[minmax(0,0.7fr)_minmax(0,1fr)]">
                <div>
                  <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                    Side
                  </div>
                  <div className="mt-3 flex flex-wrap gap-3">
                    <TicketChoice
                      label="Up"
                      active={activeOrder.side === "up"}
                      onClick={() => setActiveOrder({ side: "up" })}
                    />
                    <TicketChoice
                      label="Down"
                      active={activeOrder.side === "down"}
                      onClick={() => setActiveOrder({ side: "down" })}
                    />
                  </div>
                </div>

                <div className="grid gap-4 md:grid-cols-[minmax(0,1fr)_auto]">
                  <InputField
                    label="Size"
                    value={activeOrder.size}
                    onChange={(value) => setActiveOrder({ size: value })}
                    type="number"
                    placeholder="10"
                  />
                  <div>
                    <label className="mb-1.5 block text-xs text-[var(--text-secondary)]">Unit</label>
                    <select
                      value={activeOrder.sizeUnit}
                      onChange={(event) =>
                        setActiveOrder({
                          sizeUnit: event.target.value as ManualOrderState["sizeUnit"],
                        })
                      }
                      className="w-full rounded-[16px] border border-[var(--border)] bg-[var(--bg-tertiary)] px-4 py-3 text-sm text-[var(--text-primary)] outline-none transition-colors focus:border-[var(--accent)]"
                    >
                      <option value="shares">Shares</option>
                      <option value="usd">USD</option>
                    </select>
                  </div>
                </div>
              </div>

              <div>
                <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                  Submit style
                </div>
                <div className="mt-3 flex flex-wrap gap-3">
                  <TicketChoice
                    label="Chase Limit"
                    active={activeOrder.mode === "chase_limit"}
                    onClick={() => setActiveOrder({ mode: "chase_limit" })}
                  />
                  <TicketChoice
                    label="Limit"
                    active={activeOrder.mode === "limit"}
                    onClick={() => setActiveOrder({ mode: "limit" })}
                  />
                  <TicketChoice
                    label="Market"
                    active={activeOrder.mode === "market"}
                    onClick={() => setActiveOrder({ mode: "market" })}
                  />
                </div>
              </div>
            </div>

            <div className="mt-5 rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.78)] px-4 py-4">
              <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">
                Review
              </div>
              <div className="mt-2 text-sm text-[var(--text-primary)]">{reviewText}</div>
              <div className="mt-1 text-sm text-[var(--text-secondary)]">
                {serviceRunning
                  ? "The manual service is running and ready to receive this ticket."
                  : "Start the manual service first, then submit the ticket."}
              </div>
            </div>

            <button
              type="button"
              onClick={submitOrder}
              disabled={busy || !serviceRunning}
              className="ui-button ui-button--primary mt-5 w-full justify-center"
            >
              {ticketKind === "open" ? "Submit Open" : "Submit Close"}
            </button>
          </SectionPanel>
        </div>

        <div className="page-aside space-y-[var(--space-6)] xl:sticky xl:top-[var(--space-6)]">
          <SectionPanel title="Account Snapshot" subtitle="A quick view of what the manual service sees right now.">
            <div className="grid gap-3">
              <SnapshotRow
                label="Balance"
                value={formatUsd(manualOverview.balanceValue)}
                detail="Available buying power"
              />
              <SnapshotRow
                label="Positions"
                value={`${manualOverview.positionCount}`}
                detail={
                  manualOverview.positionCount === 1
                    ? "One position tracked"
                    : "Positions currently tracked"
                }
              />
              <SnapshotRow
                label="Health"
                value={manualOverview.healthLabel}
                detail={manualOverview.healthDetail}
              />
            </div>
          </SectionPanel>

          <SectionPanel title="Active Runs" subtitle="Stop a run if you want to cancel manual management.">
            <div className="space-y-4">
              <div>
                <div className="mb-3 flex items-center justify-between gap-3">
                  <div className="text-sm font-semibold text-[var(--text-primary)]">Open runs</div>
                  <InfoPill tone={openRuns.length > 0 ? "accent" : "neutral"}>
                    {openRuns.length}
                  </InfoPill>
                </div>
                <div className="space-y-3">
                  {openRuns.length === 0 ? (
                    <EmptyState
                      title="No open runs"
                      description="Open trades you start manually will appear here."
                    />
                  ) : (
                    openRuns.map((run, index) => (
                      <RunCard
                        key={String(run.run_id ?? `open-${index}`)}
                        kind="open"
                        run={run}
                        onStop={stopRun}
                      />
                    ))
                  )}
                </div>
              </div>

              <div>
                <div className="mb-3 flex items-center justify-between gap-3">
                  <div className="text-sm font-semibold text-[var(--text-primary)]">Close runs</div>
                  <InfoPill tone={closeRuns.length > 0 ? "accent" : "neutral"}>
                    {closeRuns.length}
                  </InfoPill>
                </div>
                <div className="space-y-3">
                  {closeRuns.length === 0 ? (
                    <EmptyState
                      title="No close runs"
                      description="Close-side manual flows will show up here once they start."
                    />
                  ) : (
                    closeRuns.map((run, index) => (
                      <RunCard
                        key={String(run.run_id ?? `close-${index}`)}
                        kind="close"
                        run={run}
                        onStop={stopRun}
                      />
                    ))
                  )}
                </div>
              </div>
            </div>
          </SectionPanel>
        </div>
      </div>
      <LogsDrawer open={logsOpen} mode="manual" onClose={() => setLogsOpen(false)} />
    </AppShell>
  );
}
