import { useCallback, useEffect, useState, type ReactNode } from "react";
import { AppShell } from "../components/AppShell";
import { EmptyState } from "../components/EmptyState";
import { InfoPill } from "../components/InfoPill";
import { LogsDrawer } from "../components/LogsDrawer";
import { MarketBadge } from "../components/MarketBadge";
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
  type UiMarketOrderbook,
} from "../lib/tauri-commands";
import {
  asRecord,
  buildManualOverview,
  formatCents,
  formatRelativeTime,
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

type ManualOrderbookResponse = {
  market?: UiMarket;
  books?: UiMarketOrderbook[];
};

const DEFAULT_ORDER: ManualOrderState = {
  conditionId: "",
  side: "up",
  size: "10",
  sizeUnit: "usd",
  mode: "chase_limit",
};

const SIZE_PRESETS = [1, 5, 10, 100];

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

function parseOrderbooks(payload: unknown): UiMarketOrderbook[] {
  const record = asRecord(payload);
  if (!record || !Array.isArray(record.books)) return [];
  return record.books.filter((item) => item && typeof item === "object") as UiMarketOrderbook[];
}

function matchesSide(side: "up" | "down", outcome: string): boolean {
  const normalized = outcome.trim().toLowerCase();
  if (side === "up") return normalized === "up" || normalized === "yes";
  return normalized === "down" || normalized === "no";
}

function orderModeLabel(mode: ManualOrderState["mode"]): string {
  if (mode === "chase_limit") return "Chase Limit";
  if (mode === "limit") return "Limit";
  return "Market";
}

function SearchResultRow({
  market,
  onSelect,
}: {
  market: UiMarket;
  onSelect: (market: UiMarket) => void;
}) {
  return (
    <button type="button" className="market-search-row" onClick={() => onSelect(market)}>
      <MarketBadge
        title={market.title}
        symbol={market.symbol}
        imageUrl={market.image_url}
        iconUrl={market.icon_url}
        size="sm"
      />
      <div className="market-search-row__copy">
        <div className="market-search-row__title">{market.title}</div>
        <div className="market-search-row__subtitle">{market.subtitle}</div>
      </div>
      <InfoPill tone={market.tradable ? "accent" : "warning"}>
        {market.tradable ? sentenceCase(market.status, "Active") : "Paused"}
      </InfoPill>
    </button>
  );
}

function TicketToggle({
  active,
  children,
  onClick,
}: {
  active: boolean;
  children: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`ticket-toggle ${active ? "ticket-toggle--active" : ""}`}
    >
      {children}
    </button>
  );
}

function OrderbookTable({ book }: { book: UiMarketOrderbook | null }) {
  if (!book) {
    return (
      <EmptyState
        title="No outcome selected"
        description="Choose Up or Down to inspect the live book."
      />
    );
  }

  const hasDepth = book.asks.length > 0 || book.bids.length > 0;
  if (!hasDepth) {
    return (
      <EmptyState
        title="Order book is empty"
        description="No resting depth is available for this outcome right now."
      />
    );
  }

  return (
    <div className="orderbook-shell">
      <div className="orderbook-shell__header">
        <div>
          <div className="orderbook-shell__title">{book.label}</div>
          <div className="orderbook-shell__subtitle">
            Best bid {formatCents(book.best_bid)} / Best ask {formatCents(book.best_ask)}
          </div>
        </div>
        <InfoPill tone="accent">
          Spread {book.spread !== null ? formatCents(book.spread) : "--"}
        </InfoPill>
      </div>
      <div className="orderbook-columns">
        <div className="orderbook-panel orderbook-panel--asks">
          <div className="orderbook-panel__header">
            <span>Ask</span>
            <span>Shares</span>
            <span>Total</span>
          </div>
          <div className="orderbook-panel__body">
            {book.asks.map((level) => (
              <div key={`ask-${level.price}-${level.total}`} className="orderbook-row">
                <span className="orderbook-row__price orderbook-row__price--ask">
                  {formatCents(level.price)}
                </span>
                <span>{formatShares(level.shares)}</span>
                <span>{formatUsd(level.total * level.price)}</span>
              </div>
            ))}
          </div>
        </div>
        <div className="orderbook-panel orderbook-panel--bids">
          <div className="orderbook-panel__header">
            <span>Bid</span>
            <span>Shares</span>
            <span>Total</span>
          </div>
          <div className="orderbook-panel__body">
            {book.bids.map((level) => (
              <div key={`bid-${level.price}-${level.total}`} className="orderbook-row">
                <span className="orderbook-row__price orderbook-row__price--bid">
                  {formatCents(level.price)}
                </span>
                <span>{formatShares(level.shares)}</span>
                <span>{formatUsd(level.total * level.price)}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}

function RunRow({
  run,
  onStop,
}: {
  run: RawManualRun;
  onStop: (runId: string) => void;
}) {
  const runId = typeof run.run_id === "string" ? run.run_id : "";
  return (
    <div className="manual-run-row">
      <div>
        <div className="manual-run-row__title">{run.market_title || "Manual run"}</div>
        <div className="manual-run-row__subtitle">
          {[run.status_label, run.side_label, run.progress_summary].filter(Boolean).join(" / ") ||
            "Waiting for updates"}
        </div>
      </div>
      {runId ? (
        <button type="button" className="ui-button" onClick={() => onStop(runId)}>
          Stop
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
  const [marketBooks, setMarketBooks] = useState<UiMarketOrderbook[]>([]);
  const [marketSearch, setMarketSearch] = useState("");
  const [marketSearchLoading, setMarketSearchLoading] = useState(false);
  const [marketDetailLoading, setMarketDetailLoading] = useState(false);
  const [searchResults, setSearchResults] = useState<UiMarket[]>([]);
  const [recentMarkets, setRecentMarkets] = useState<UiMarket[]>([]);
  const [logsOpen, setLogsOpen] = useState(false);

  const serviceRunning = serviceStatus.startsWith("running");
  const activeOrder = ticketKind === "open" ? openOrder : closeOrder;
  const activeSelectedMarket =
    ticketKind === "open" ? selectedOpenMarket : selectedCloseMarket;
  const activeRuns = ticketKind === "open" ? openRuns : closeRuns;

  const setActiveOrder = (patch: Partial<ManualOrderState>) => {
    if (ticketKind === "open") {
      setOpenOrder((previous) => ({ ...previous, ...patch }));
      return;
    }
    setCloseOrder((previous) => ({ ...previous, ...patch }));
  };

  const setSelectedMarketForKind = useCallback((kind: TicketKind, market: UiMarket | null) => {
    if (kind === "open") {
      setSelectedOpenMarket(market);
      return;
    }
    setSelectedCloseMarket(market);
  }, []);

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
      if (!serviceStopping) {
        setError(String(err));
      }
    }
  }, [serviceRunning, serviceStopping]);

  const loadSelectedMarket = useCallback(
    async (kind: TicketKind, marketRef: string) => {
      if (!serviceRunning || serviceStopping || !marketRef.trim()) return;
      setMarketDetailLoading(true);
      try {
        const [detailResponse, orderbookResponse] = await Promise.all([
          manualApiRequest<ManualMarketResponse>(
            "GET",
            `/ui/market/${encodeURIComponent(marketRef)}/detail`
          ),
          manualApiRequest<ManualOrderbookResponse>(
            "GET",
            `/ui/market/${encodeURIComponent(marketRef)}/orderbook`,
            { depth: 6 }
          ),
        ]);
        const market = parseMarket(detailResponse);
        if (market) {
          setSelectedMarketForKind(kind, market);
        }
        setMarketBooks(parseOrderbooks(orderbookResponse));
      } catch (err) {
        setMarketBooks([]);
        if (!serviceStopping) {
          setError(String(err));
        }
      } finally {
        setMarketDetailLoading(false);
      }
    },
    [serviceRunning, serviceStopping, setSelectedMarketForKind]
  );

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
    const timer = setInterval(() => void fetchOverview(), 5000);
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
    }, 250);

    return () => clearTimeout(timer);
  }, [marketSearch, serviceRunning, serviceStopping]);

  useEffect(() => {
    const nextSelectedMarket = ticketKind === "open" ? selectedOpenMarket : selectedCloseMarket;
    if (!nextSelectedMarket) return;
    setMarketSearch(nextSelectedMarket.title);
  }, [ticketKind, selectedOpenMarket, selectedCloseMarket]);

  useEffect(() => {
    if (!serviceRunning || serviceStopping) return;
    const conditionId = activeOrder.conditionId.trim();
    if (!conditionId) return;
    if (activeSelectedMarket?.condition_id === conditionId && marketBooks.length > 0) return;
    const timer = setTimeout(() => {
      void loadSelectedMarket(ticketKind, conditionId);
    }, 200);
    return () => clearTimeout(timer);
  }, [
    activeOrder.conditionId,
    activeSelectedMarket,
    loadSelectedMarket,
    marketBooks.length,
    serviceRunning,
    serviceStopping,
    ticketKind,
  ]);

  const clearMessages = () => {
    setError(null);
    setNotice(null);
  };

  const railItems = [
    { label: "Portfolio", to: "/dashboard" },
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
      setMarketBooks([]);
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

  const selectMarket = async (market: UiMarket) => {
    clearMessages();
    setActiveOrder({ conditionId: market.condition_id });
    setSelectedMarketForKind(ticketKind, market);
    setMarketSearch(market.title);
    setSearchResults([]);
    await loadSelectedMarket(ticketKind, market.market_slug || market.condition_id);
  };

  const stopRun = async (runId: string) => {
    clearMessages();
    setBusy(true);
    try {
      await manualApiRequest(
        "POST",
        ticketKind === "open" ? `/manual/open/runs/${runId}/stop` : `/manual/close/runs/${runId}/stop`
      );
      setNotice(`Stop requested for run ${runId}.`);
      await fetchOverview();
    } catch (err) {
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
      const marketTitle = activeSelectedMarket?.title || "selected market";

      setNotice(
        runId
          ? `${ticketKind === "open" ? "Buy" : "Sell"} request sent for ${marketTitle}. Run ${runId} is active.`
          : `${ticketKind === "open" ? "Buy" : "Sell"} request sent for ${marketTitle}.`
      );
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

  const selectedBook =
    marketBooks.find((book) => matchesSide(activeOrder.side, book.outcome)) ?? marketBooks[0] ?? null;
  const selectedSide = activeSelectedMarket?.sides.find((side) =>
    matchesSide(activeOrder.side, side.outcome)
  );
  const relatedMarkets = recentMarkets.filter(
    (market) => market.condition_id !== activeSelectedMarket?.condition_id
  );
  const activeUiPositions = Array.isArray(positions?.ui_positions) ? positions.ui_positions : [];

  return (
    <AppShell
      railSubtitle="Manual trade"
      railItems={railItems}
      eyebrow="Manual trading"
      title="Market detail and ticket"
      description="Search a market, inspect the live order book, and place one guided trade from a sticky ticket."
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
      <SectionPanel
        title="Manual desk"
        subtitle="This is the desktop trading page: search, inspect, then buy or sell from the same surface."
        className="workspace-panel"
        bodyClassName="workspace-panel__body"
      >
        <div className="manual-toolbar">
          <div className="manual-toolbar__search">
            <input
              type="text"
              value={marketSearch}
              onChange={(event) => setMarketSearch(event.target.value)}
              placeholder={
                serviceRunning
                  ? "Search by market name, slug, or symbol"
                  : "Start the manual service to search live markets"
              }
              disabled={busy || !serviceRunning}
              className="workspace-search"
            />
            {(marketSearchLoading ||
              searchResults.length > 0 ||
              (!marketSearch.trim() && recentMarkets.length > 0)) &&
            serviceRunning ? (
              <div className="market-search-panel">
                {marketSearchLoading ? (
                  <div className="market-search-panel__empty">Searching markets...</div>
                ) : searchResults.length > 0 ? (
                  searchResults.map((market) => (
                    <SearchResultRow
                      key={market.condition_id}
                      market={market}
                      onSelect={selectMarket}
                    />
                  ))
                ) : !marketSearch.trim() && recentMarkets.length > 0 ? (
                  <>
                    <div className="market-search-panel__label">Recent markets</div>
                    {recentMarkets.map((market) => (
                      <SearchResultRow
                        key={market.condition_id}
                        market={market}
                        onSelect={selectMarket}
                      />
                    ))}
                  </>
                ) : (
                  <div className="market-search-panel__empty">No matching markets right now.</div>
                )}
              </div>
            ) : null}
          </div>
          <div className="manual-toolbar__actions">
            <button
              type="button"
              onClick={handleStart}
              disabled={busy || serviceRunning}
              className="ui-button ui-button--primary"
            >
              Start service
            </button>
            <button
              type="button"
              onClick={handleStop}
              disabled={busy || !serviceRunning}
              className="ui-button ui-button--danger"
            >
              Stop service
            </button>
            <button
              type="button"
              onClick={() => setSimulation((value) => !value)}
              disabled={busy || serviceRunning}
              className="ui-button"
            >
              {simulation ? "Dry Run" : "Live"}
            </button>
            <button type="button" onClick={() => setLogsOpen(true)} className="ui-button">
              Open Logs
            </button>
          </div>
        </div>

        {error ? <div className="inline-alert">{error}</div> : null}
        {!error && notice ? <div className="manual-notice">{notice}</div> : null}
      </SectionPanel>

      <div className="manual-workspace">
        <div className="manual-workspace__main">
          <SectionPanel
            title="Selected market"
            subtitle="Manual trading should feel like a normal exchange: one market, one book, one ticket."
            className="workspace-panel"
            bodyClassName="workspace-panel__body"
          >
            {activeSelectedMarket ? (
              <div className="manual-market-header">
                <div className="manual-market-header__identity">
                  <MarketBadge
                    title={activeSelectedMarket.title}
                    symbol={activeSelectedMarket.symbol}
                    imageUrl={activeSelectedMarket.image_url}
                    iconUrl={activeSelectedMarket.icon_url}
                    size="lg"
                  />
                  <div className="manual-market-header__copy">
                    <div className="manual-market-header__title">{activeSelectedMarket.title}</div>
                    <div className="manual-market-header__subtitle">
                      {activeSelectedMarket.subtitle}
                    </div>
                    <div className="manual-market-header__meta">
                      <InfoPill tone={activeSelectedMarket.tradable ? "success" : "warning"}>
                        {activeSelectedMarket.tradable ? "Tradable" : "Paused"}
                      </InfoPill>
                      {activeSelectedMarket.close_time ? (
                        <InfoPill tone="neutral">
                          Closes {formatRelativeTime(activeSelectedMarket.close_time)}
                        </InfoPill>
                      ) : null}
                    </div>
                  </div>
                </div>
                <div className="manual-market-header__sides">
                  {activeSelectedMarket.sides.map((side) => {
                    const isActive = selectedSide?.token_id === side.token_id;
                    return (
                      <button
                        key={side.token_id}
                        type="button"
                        className={`outcome-chip ${isActive ? "outcome-chip--active" : ""}`}
                        onClick={() =>
                          setActiveOrder({
                            side: matchesSide("up", side.outcome) ? "up" : "down",
                          })
                        }
                      >
                        <span>{side.label}</span>
                        <span>{formatCents(side.price)}</span>
                      </button>
                    );
                  })}
                </div>
              </div>
            ) : (
              <EmptyState
                title="Choose a market"
                description="Search for a market above to load its title, outcomes, and order book."
              />
            )}
          </SectionPanel>

          <SectionPanel
            title="Order book"
            subtitle="Live depth for the selected outcome."
            className="workspace-panel"
            bodyClassName="workspace-panel__body"
          >
            {marketDetailLoading ? (
              <div className="market-search-panel__empty">Loading order book...</div>
            ) : (
              <OrderbookTable book={selectedBook} />
            )}
          </SectionPanel>

          <SectionPanel
            title="Related markets"
            subtitle="Keep one-click access to the other markets you already touched in this session."
            className="workspace-panel"
            bodyClassName="workspace-panel__body"
          >
            {relatedMarkets.length > 0 ? (
              <div className="related-markets">
                {relatedMarkets.map((market) => (
                  <button
                    key={market.condition_id}
                    type="button"
                    className="related-market-row"
                    onClick={() => void selectMarket(market)}
                  >
                    <MarketBadge
                      title={market.title}
                      symbol={market.symbol}
                      imageUrl={market.image_url}
                      iconUrl={market.icon_url}
                      size="sm"
                    />
                    <div className="related-market-row__copy">
                      <div className="related-market-row__title">{market.title}</div>
                      <div className="related-market-row__subtitle">{market.subtitle}</div>
                    </div>
                    <InfoPill tone={market.tradable ? "accent" : "warning"}>
                      {sentenceCase(market.status, "Active")}
                    </InfoPill>
                  </button>
                ))}
              </div>
            ) : (
              <EmptyState
                title="No related markets yet"
                description="Recent manual markets will show up here after you inspect a few names."
              />
            )}
          </SectionPanel>
        </div>

        <div className="manual-workspace__aside">
          <div className="manual-ticket-sticky">
            <SectionPanel
              title="Trade ticket"
              subtitle="Use the same market detail page for entry and exit."
              className="workspace-panel manual-ticket-panel"
              bodyClassName="workspace-panel__body"
            >
              <div className="manual-ticket__tabs">
                <TicketToggle active={ticketKind === "open"} onClick={() => setTicketKind("open")}>
                  Buy
                </TicketToggle>
                <TicketToggle active={ticketKind === "close"} onClick={() => setTicketKind("close")}>
                  Sell
                </TicketToggle>
              </div>

              {activeSelectedMarket ? (
                <>
                  <div className="manual-ticket__mode-label">
                    {ticketKind === "open" ? "Buying" : "Selling"} {activeSelectedMarket.title}
                  </div>

                  <div className="manual-ticket__section">
                    <div className="manual-ticket__section-label">Outcome</div>
                    <div className="manual-ticket__outcomes">
                      {activeSelectedMarket.sides.map((side) => {
                        const normalizedSide = matchesSide("up", side.outcome) ? "up" : "down";
                        return (
                          <button
                            key={side.token_id}
                            type="button"
                            className={`manual-ticket__outcome ${
                              activeOrder.side === normalizedSide
                                ? "manual-ticket__outcome--active"
                                : ""
                            }`}
                            onClick={() => setActiveOrder({ side: normalizedSide })}
                          >
                            <span>{side.label}</span>
                            <span>{formatCents(side.price)}</span>
                          </button>
                        );
                      })}
                    </div>
                  </div>

                  <div className="manual-ticket__section">
                    <div className="manual-ticket__section-label">Amount</div>
                    <div className="manual-ticket__amount-row">
                      <div>
                        <div className="manual-ticket__amount-value">
                          {activeOrder.sizeUnit === "usd" ? "$" : ""}
                          {activeOrder.size || "0"}
                        </div>
                        <div className="manual-ticket__amount-help">
                          Balance {formatUsd(manualOverview.balanceValue)}
                        </div>
                      </div>
                      <select
                        value={activeOrder.sizeUnit}
                        onChange={(event) =>
                          setActiveOrder({
                            sizeUnit: event.target.value as ManualOrderState["sizeUnit"],
                          })
                        }
                        className="manual-ticket__select"
                      >
                        <option value="usd">USD</option>
                        <option value="shares">Shares</option>
                      </select>
                    </div>
                    <input
                      type="number"
                      min="0"
                      step="0.01"
                      value={activeOrder.size}
                      onChange={(event) => setActiveOrder({ size: event.target.value })}
                      className="manual-ticket__input"
                    />
                    <div className="manual-ticket__presets">
                      {SIZE_PRESETS.map((preset) => (
                        <button
                          key={preset}
                          type="button"
                          className="manual-ticket__preset"
                          onClick={() => setActiveOrder({ size: String(preset) })}
                        >
                          {activeOrder.sizeUnit === "usd" ? `$${preset}` : `${preset}`}
                        </button>
                      ))}
                    </div>
                  </div>

                  <div className="manual-ticket__section">
                    <div className="manual-ticket__section-label">Submit style</div>
                    <div className="manual-ticket__mode-grid">
                      {(["chase_limit", "limit", "market"] as const).map((mode) => (
                        <button
                          key={mode}
                          type="button"
                          className={`manual-ticket__mode ${
                            activeOrder.mode === mode ? "manual-ticket__mode--active" : ""
                          }`}
                          onClick={() => setActiveOrder({ mode })}
                        >
                          {orderModeLabel(mode)}
                        </button>
                      ))}
                    </div>
                  </div>

                  <button
                    type="button"
                    className="manual-ticket__submit"
                    onClick={() => void submitOrder()}
                    disabled={busy || !serviceRunning}
                  >
                    {ticketKind === "open" ? "Buy" : "Sell"}{" "}
                    {selectedSide?.label || "selected side"}
                  </button>

                  <div className="manual-ticket__footer">
                    Manual trading uses the running service and the same wallet context already loaded in the app.
                  </div>
                </>
              ) : (
                <EmptyState
                  title="Pick a market first"
                  description="Search above, choose a market, then the ticket will light up here."
                />
              )}
            </SectionPanel>

            <SectionPanel
              title="Active runs"
              subtitle="Open and close requests that are still working."
              className="workspace-panel"
              bodyClassName="workspace-panel__body"
            >
              {activeRuns.length > 0 ? (
                <div className="manual-runs">
                  {activeRuns.map((run) => (
                    <RunRow
                      key={typeof run.run_id === "string" ? run.run_id : JSON.stringify(run)}
                      run={run}
                      onStop={(runId) => void stopRun(runId)}
                    />
                  ))}
                </div>
              ) : (
                <EmptyState
                  title="No active manual runs"
                  description="Submitted manual requests will appear here until they finish or you stop them."
                />
              )}
            </SectionPanel>

            <SectionPanel
              title="Account snapshot"
              subtitle="The manual desk should tell you what matters without dumping raw JSON."
              className="workspace-panel"
              bodyClassName="workspace-panel__body"
            >
              <div className="manual-summary-grid">
                <div className="manual-summary-card">
                  <div className="manual-summary-card__label">Health</div>
                  <div className="manual-summary-card__value">{manualOverview.healthLabel}</div>
                </div>
                <div className="manual-summary-card">
                  <div className="manual-summary-card__label">Available</div>
                  <div className="manual-summary-card__value">
                    {formatUsd(manualOverview.balanceValue)}
                  </div>
                </div>
                <div className="manual-summary-card">
                  <div className="manual-summary-card__label">Positions</div>
                  <div className="manual-summary-card__value">{manualOverview.positionCount}</div>
                </div>
                <div className="manual-summary-card">
                  <div className="manual-summary-card__label">Runs</div>
                  <div className="manual-summary-card__value">{manualOverview.totalRuns}</div>
                </div>
              </div>

              <p className="surface-panel__subtitle">{manualOverview.healthDetail}</p>

              {activeUiPositions.length > 0 ? (
                <div className="manual-position-list">
                  {activeUiPositions.slice(0, 4).map((position) => {
                    const pnl = position.unrealized_pnl ?? position.realized_pnl ?? null;
                    return (
                      <div
                        key={`${position.condition_id}-${position.side}`}
                        className="manual-position-row"
                      >
                        <div>
                          <div className="manual-position-row__title">{position.market_title}</div>
                          <div className="manual-position-row__subtitle">
                            {position.side_label} / {formatShares(position.size)} shares
                          </div>
                        </div>
                        <div className="manual-position-row__value">{formatUsd(pnl)}</div>
                      </div>
                    );
                  })}
                </div>
              ) : null}
            </SectionPanel>
          </div>
        </div>
      </div>

      <LogsDrawer open={logsOpen} mode="manual" onClose={() => setLogsOpen(false)} />
    </AppShell>
  );
}
