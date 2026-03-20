import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  getManualLogLines,
  getManualServiceStatus,
  getActiveProfileId,
  getSavedConfig,
  manualApiRequest,
  startManualService,
  stopManualService,
  type LogLine,
} from "../lib/tauri-commands";

type ManualRun = {
  run_id?: string;
  status?: string;
  condition_id?: string;
  side?: string;
  target_shares?: number;
  [key: string]: unknown;
};

type ManualOrderState = {
  conditionId: string;
  side: "up" | "down";
  size: string;
  sizeUnit: "shares" | "usd";
  mode: "chase_limit" | "limit" | "market";
};

const DEFAULT_ORDER: ManualOrderState = {
  conditionId: "",
  side: "up",
  size: "10",
  sizeUnit: "shares",
  mode: "chase_limit",
};

function pretty(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function parseRuns(payload: unknown): ManualRun[] {
  if (!payload || typeof payload !== "object") return [];
  const runs = (payload as { runs?: unknown }).runs;
  if (!Array.isArray(runs)) return [];
  return runs.filter((item) => item && typeof item === "object") as ManualRun[];
}

export function Manual() {
  const navigate = useNavigate();

  const [serviceStatus, setServiceStatus] = useState("stopped");
  const [simulation, setSimulation] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const [health, setHealth] = useState<unknown>(null);
  const [balance, setBalance] = useState<unknown>(null);
  const [positions, setPositions] = useState<unknown>(null);
  const [openRuns, setOpenRuns] = useState<ManualRun[]>([]);
  const [closeRuns, setCloseRuns] = useState<ManualRun[]>([]);

  const [openOrder, setOpenOrder] = useState<ManualOrderState>(DEFAULT_ORDER);
  const [closeOrder, setCloseOrder] = useState<ManualOrderState>(DEFAULT_ORDER);
  const [logs, setLogs] = useState<LogLine[]>([]);

  const logRef = useRef<HTMLDivElement>(null);
  const stickLogToBottomRef = useRef(true);

  const refreshStatus = useCallback(async () => {
    try {
      const next = await getManualServiceStatus();
      setServiceStatus(next);
    } catch (err) {
      setServiceStatus(`error:${String(err)}`);
    }
  }, []);

  const fetchOverview = useCallback(async () => {
    if (!serviceStatus.startsWith("running")) return;
    try {
      const [healthResp, balanceResp, positionsResp, openRunsResp, closeRunsResp, logLines] =
        await Promise.all([
          manualApiRequest("GET", "/manual/health"),
          manualApiRequest("GET", "/manual/balance"),
          manualApiRequest("GET", "/manual/positions"),
          manualApiRequest("GET", "/manual/open/runs"),
          manualApiRequest("GET", "/manual/close/runs"),
          getManualLogLines(120),
        ]);
      setHealth(healthResp);
      setBalance(balanceResp);
      setPositions(positionsResp);
      setOpenRuns(parseRuns(openRunsResp));
      setCloseRuns(parseRuns(closeRunsResp));
      setLogs(logLines);
    } catch (err) {
      setError(String(err));
    }
  }, [serviceStatus]);

  useEffect(() => {
    (async () => {
      try {
        const profileId = await getActiveProfileId();
        if (!profileId) return;
        const saved = await getSavedConfig(profileId);
        setSimulation(saved.simulation);
      } catch {
        // keep current simulation mode
      }
    })();
  }, []);

  useEffect(() => {
    refreshStatus();
    const timer = setInterval(refreshStatus, 2500);
    return () => clearInterval(timer);
  }, [refreshStatus]);

  useEffect(() => {
    if (!serviceStatus.startsWith("running")) return;
    fetchOverview();
    const timer = setInterval(fetchOverview, 4000);
    return () => clearInterval(timer);
  }, [serviceStatus, fetchOverview]);

  useLayoutEffect(() => {
    const node = logRef.current;
    if (!node) return;
    if (stickLogToBottomRef.current) {
      node.scrollTop = node.scrollHeight;
    }
  }, [logs]);

  const handleLogScroll = useCallback(() => {
    const node = logRef.current;
    if (!node) return;
    const distanceFromBottom = node.scrollHeight - node.scrollTop - node.clientHeight;
    stickLogToBottomRef.current = distanceFromBottom < 16;
  }, []);

  const clearMessages = () => {
    setError(null);
    setNotice(null);
  };

  const handleStart = async () => {
    clearMessages();
    setBusy(true);
    try {
      await startManualService(simulation);
      setNotice(`manual service started (${simulation ? "simulation" : "live"})`);
      await refreshStatus();
      await fetchOverview();
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
      await stopManualService();
      setNotice("manual service stopped");
      setOpenRuns([]);
      setCloseRuns([]);
      await refreshStatus();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const submitOrder = async (kind: "open" | "close", state: ManualOrderState) => {
    clearMessages();
    setBusy(true);
    try {
      const sizeNum = Number(state.size);
      if (!state.conditionId.trim()) {
        throw new Error("condition_id is required");
      }
      if (!Number.isFinite(sizeNum) || sizeNum <= 0) {
        throw new Error("size must be a positive number");
      }

      const body: Record<string, unknown> = {
        condition_id: state.conditionId.trim(),
        side: state.side,
        size: sizeNum,
        mode: state.mode,
      };
      if (state.sizeUnit === "usd") {
        body.size_unit = "usd";
      }

      const response = await manualApiRequest(
        "POST",
        kind === "open" ? "/manual/open" : "/manual/close",
        undefined,
        body
      );
      setNotice(`${kind} submitted: ${pretty(response)}`);
      await fetchOverview();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const stopRun = async (kind: "open" | "close", runId: string) => {
    clearMessages();
    setBusy(true);
    try {
      await manualApiRequest(
        "POST",
        kind === "open"
          ? `/manual/open/runs/${runId}/stop`
          : `/manual/close/runs/${runId}/stop`
      );
      setNotice(`stop requested for ${runId}`);
      await fetchOverview();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="h-full bg-[var(--bg-primary)] flex flex-col overflow-hidden">
      <div className="flex items-center justify-between px-6 py-4 border-b border-[var(--border)]">
        <div className="flex items-center gap-3">
          <button
            onClick={() => navigate("/dashboard")}
            className="p-2 rounded-lg bg-[var(--bg-tertiary)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors"
          >
            <svg
              className="w-4 h-4 text-[var(--text-secondary)]"
              fill="none"
              stroke="currentColor"
              viewBox="0 0 24 24"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth={2}
                d="M15 19l-7-7 7-7"
              />
            </svg>
          </button>
          <h1 className="text-lg font-semibold text-[var(--text-primary)]">Manual Trading</h1>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto px-6 py-4 pb-24">
        <div className="max-w-6xl mx-auto space-y-4">
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
            <div className="flex flex-wrap items-center gap-3">
              <div className="text-sm text-[var(--text-secondary)]">
                Service: <span className="text-[var(--text-primary)]">{serviceStatus}</span>
              </div>
              <label className="inline-flex items-center gap-2 text-sm text-[var(--text-secondary)]">
                <input
                  type="checkbox"
                  checked={!simulation}
                  onChange={(e) => setSimulation(!e.target.checked)}
                  className="accent-[var(--accent)]"
                />
                Live mode
              </label>
              <button
                onClick={handleStart}
                disabled={busy}
                className="px-4 py-2 text-sm rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors disabled:opacity-50"
              >
                Start Manual Service
              </button>
              <button
                onClick={handleStop}
                disabled={busy}
                className="px-4 py-2 text-sm rounded-lg bg-[var(--bg-tertiary)] border border-[var(--border)] text-[var(--text-primary)] hover:border-[var(--accent)] transition-colors disabled:opacity-50"
              >
                Stop Manual Service
              </button>
              <button
                onClick={fetchOverview}
                disabled={busy || !serviceStatus.startsWith("running")}
                className="px-4 py-2 text-sm rounded-lg bg-[var(--bg-tertiary)] border border-[var(--border)] text-[var(--text-primary)] hover:border-[var(--accent)] transition-colors disabled:opacity-50"
              >
                Refresh
              </button>
            </div>
            {error && <div className="text-[var(--red)] text-sm mt-3">{error}</div>}
            {notice && <div className="text-[var(--green)] text-sm mt-3">{notice}</div>}
          </div>

          <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
            <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4 space-y-3">
              <h3 className="text-sm font-semibold text-[var(--text-primary)]">Open (BUY)</h3>
              <input
                value={openOrder.conditionId}
                onChange={(e) =>
                  setOpenOrder((prev) => ({ ...prev, conditionId: e.target.value }))
                }
                placeholder="condition_id"
                className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-sm outline-none focus:border-[var(--accent)]"
              />
              <div className="grid grid-cols-3 gap-2">
                <select
                  value={openOrder.side}
                  onChange={(e) =>
                    setOpenOrder((prev) => ({
                      ...prev,
                      side: e.target.value as "up" | "down",
                    }))
                  }
                  className="bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-sm"
                >
                  <option value="up">up</option>
                  <option value="down">down</option>
                </select>
                <input
                  value={openOrder.size}
                  onChange={(e) =>
                    setOpenOrder((prev) => ({ ...prev, size: e.target.value }))
                  }
                  placeholder="size"
                  className="bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-sm"
                />
                <select
                  value={openOrder.sizeUnit}
                  onChange={(e) =>
                    setOpenOrder((prev) => ({
                      ...prev,
                      sizeUnit: e.target.value as "shares" | "usd",
                    }))
                  }
                  className="bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-sm"
                >
                  <option value="shares">shares</option>
                  <option value="usd">usd</option>
                </select>
              </div>
              <select
                value={openOrder.mode}
                onChange={(e) =>
                  setOpenOrder((prev) => ({
                    ...prev,
                    mode: e.target.value as "chase_limit" | "limit" | "market",
                  }))
                }
                className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-sm"
              >
                <option value="chase_limit">chase_limit</option>
                <option value="limit">limit</option>
                <option value="market">market</option>
              </select>
              <button
                onClick={() => submitOrder("open", openOrder)}
                disabled={busy || !serviceStatus.startsWith("running")}
                className="w-full px-4 py-2 text-sm rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors disabled:opacity-50"
              >
                Submit Open
              </button>
            </div>

            <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4 space-y-3">
              <h3 className="text-sm font-semibold text-[var(--text-primary)]">Close (SELL)</h3>
              <input
                value={closeOrder.conditionId}
                onChange={(e) =>
                  setCloseOrder((prev) => ({ ...prev, conditionId: e.target.value }))
                }
                placeholder="condition_id"
                className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-sm outline-none focus:border-[var(--accent)]"
              />
              <div className="grid grid-cols-3 gap-2">
                <select
                  value={closeOrder.side}
                  onChange={(e) =>
                    setCloseOrder((prev) => ({
                      ...prev,
                      side: e.target.value as "up" | "down",
                    }))
                  }
                  className="bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-sm"
                >
                  <option value="up">up</option>
                  <option value="down">down</option>
                </select>
                <input
                  value={closeOrder.size}
                  onChange={(e) =>
                    setCloseOrder((prev) => ({ ...prev, size: e.target.value }))
                  }
                  placeholder="size"
                  className="bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-sm"
                />
                <select
                  value={closeOrder.sizeUnit}
                  onChange={(e) =>
                    setCloseOrder((prev) => ({
                      ...prev,
                      sizeUnit: e.target.value as "shares" | "usd",
                    }))
                  }
                  className="bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-sm"
                >
                  <option value="shares">shares</option>
                  <option value="usd">usd</option>
                </select>
              </div>
              <select
                value={closeOrder.mode}
                onChange={(e) =>
                  setCloseOrder((prev) => ({
                    ...prev,
                    mode: e.target.value as "chase_limit" | "limit" | "market",
                  }))
                }
                className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-sm"
              >
                <option value="chase_limit">chase_limit</option>
                <option value="limit">limit</option>
                <option value="market">market</option>
              </select>
              <button
                onClick={() => submitOrder("close", closeOrder)}
                disabled={busy || !serviceStatus.startsWith("running")}
                className="w-full px-4 py-2 text-sm rounded-lg bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors disabled:opacity-50"
              >
                Submit Close
              </button>
            </div>
          </div>

          <div className="grid grid-cols-1 xl:grid-cols-3 gap-4">
            <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
              <h3 className="text-sm font-semibold text-[var(--text-primary)] mb-2">Health</h3>
              <pre className="text-xs text-[var(--text-secondary)] whitespace-pre-wrap overflow-auto max-h-64">
                {pretty(health)}
              </pre>
            </div>
            <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
              <h3 className="text-sm font-semibold text-[var(--text-primary)] mb-2">Balance</h3>
              <pre className="text-xs text-[var(--text-secondary)] whitespace-pre-wrap overflow-auto max-h-64">
                {pretty(balance)}
              </pre>
            </div>
            <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
              <h3 className="text-sm font-semibold text-[var(--text-primary)] mb-2">Positions</h3>
              <pre className="text-xs text-[var(--text-secondary)] whitespace-pre-wrap overflow-auto max-h-64">
                {pretty(positions)}
              </pre>
            </div>
          </div>

          <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
            <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
              <h3 className="text-sm font-semibold text-[var(--text-primary)] mb-2">Open Runs</h3>
              <div className="space-y-2 max-h-64 overflow-auto">
                {openRuns.length === 0 && (
                  <div className="text-xs text-[var(--text-secondary)]">No active open runs</div>
                )}
                {openRuns.map((run, idx) => {
                  const runId = String(run.run_id ?? "");
                  return (
                    <div
                      key={runId || `open-${idx}`}
                      className="bg-[var(--bg-tertiary)] border border-[var(--border)] rounded p-2"
                    >
                      <div className="text-xs text-[var(--text-primary)] break-all">{runId}</div>
                      <div className="text-xs text-[var(--text-secondary)]">
                        {String(run.status ?? "unknown")}
                      </div>
                      {runId && (
                        <button
                          onClick={() => stopRun("open", runId)}
                          className="mt-2 px-2 py-1 text-xs rounded bg-[var(--bg-primary)] border border-[var(--border)] hover:border-[var(--accent)]"
                        >
                          Stop
                        </button>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>

            <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
              <h3 className="text-sm font-semibold text-[var(--text-primary)] mb-2">Close Runs</h3>
              <div className="space-y-2 max-h-64 overflow-auto">
                {closeRuns.length === 0 && (
                  <div className="text-xs text-[var(--text-secondary)]">No active close runs</div>
                )}
                {closeRuns.map((run, idx) => {
                  const runId = String(run.run_id ?? "");
                  return (
                    <div
                      key={runId || `close-${idx}`}
                      className="bg-[var(--bg-tertiary)] border border-[var(--border)] rounded p-2"
                    >
                      <div className="text-xs text-[var(--text-primary)] break-all">{runId}</div>
                      <div className="text-xs text-[var(--text-secondary)]">
                        {String(run.status ?? "unknown")}
                      </div>
                      {runId && (
                        <button
                          onClick={() => stopRun("close", runId)}
                          className="mt-2 px-2 py-1 text-xs rounded bg-[var(--bg-primary)] border border-[var(--border)] hover:border-[var(--accent)]"
                        >
                          Stop
                        </button>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          </div>

          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
            <h3 className="text-sm font-semibold text-[var(--text-primary)] mb-2">Manual Logs</h3>
            <div
              ref={logRef}
              onScroll={handleLogScroll}
              className="max-h-56 overflow-auto bg-[var(--bg-primary)] border border-[var(--border)] rounded p-2 font-mono text-xs"
            >
              {logs.length === 0 && (
                <div className="text-[var(--text-secondary)]">No logs yet</div>
              )}
              {logs.map((line, idx) => (
                <div key={`${line.timestamp}-${idx}`} className="mb-0.5">
                  <span className="text-[var(--text-secondary)]">{line.timestamp}</span>{" "}
                  <span className="text-[var(--text-primary)]">{line.level}</span>{" "}
                  <span className="text-[var(--text-primary)]">{line.content}</span>
                </div>
              ))}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
