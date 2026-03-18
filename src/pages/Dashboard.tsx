import { useState, useEffect, useRef, useCallback } from "react";
import { useNavigate } from "react-router-dom";
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  CartesianGrid,
} from "recharts";
import { open } from "@tauri-apps/plugin-shell";
import { check } from "@tauri-apps/plugin-updater";
import { StatusBadge } from "../components/StatusBadge";
import { ProfileSwitcher } from "../components/ProfileSwitcher";
import { UpdateBanner } from "../components/UpdateBanner";
import { useBotStatus } from "../hooks/useBotStatus";
import { useTradeData } from "../hooks/useTradeData";
import { useWalletBalance } from "../hooks/useWalletBalance";
import {
  startBot,
  stopBot,
  restartBot,
  getLogLines,
  getActiveProfileId,
  getDataDirPath,
  type LogLine,
} from "../lib/tauri-commands";

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
    <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg px-4 py-3">
      <div className="text-xs text-[var(--text-secondary)] mb-1">{label}</div>
      <div
        className="text-xl font-semibold"
        style={{ color: color || "var(--text-primary)" }}
      >
        {value}
      </div>
    </div>
  );
}

export function Dashboard() {
  const navigate = useNavigate();
  const { isRunning } = useBotStatus();
  const { stats, trades, positions } = useTradeData(isRunning);
  const { balance } = useWalletBalance();

  const [activeProfileId, setActiveProfileId] = useState<string | null>(null);
  const [logs, setLogs] = useState<LogLine[]>([]);
  const [simulation, setSimulation] = useState(true);
  const [actionLoading, setActionLoading] = useState(false);
  const [updateVersion, setUpdateVersion] = useState<string | null>(null);
  const [updateDownloading, setUpdateDownloading] = useState(false);
  const pendingUpdateRef = useRef<Awaited<ReturnType<typeof check>> | null>(null);
  const logRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    getActiveProfileId()
      .then(setActiveProfileId)
      .catch(() => {});
  }, []);

  useEffect(() => {
    (async () => {
      try {
        const update = await check();
        if (update) {
          pendingUpdateRef.current = update;
          setUpdateVersion(update.version);
        } else {
          pendingUpdateRef.current = null;
          setUpdateVersion(null);
        }
      } catch {
        pendingUpdateRef.current = null;
        setUpdateVersion(null);
      }
    })();
  }, []);

  const pollLogs = useCallback(async () => {
    try {
      const lines = await getLogLines(50);
      setLogs(lines);
    } catch {
      // ignore
    }
  }, []);

  useEffect(() => {
    pollLogs();
    const interval = setInterval(pollLogs, 3000);
    return () => clearInterval(interval);
  }, [pollLogs]);

  useEffect(() => {
    if (logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [logs]);

  const handleStart = async () => {
    setActionLoading(true);
    try {
      await startBot(simulation);
    } catch {
      // error handled by status poll
    }
    setActionLoading(false);
  };

  const handleStop = async () => {
    setActionLoading(true);
    try {
      await stopBot();
    } catch {
      // error handled by status poll
    }
    setActionLoading(false);
  };

  const handleRestart = async () => {
    setActionLoading(true);
    try {
      await restartBot(simulation);
    } catch {
      // error handled by status poll
    }
    setActionLoading(false);
  };

  const handleUpdate = async () => {
    if (updateDownloading) return;
    const update = pendingUpdateRef.current;
    if (!update) return;
    setUpdateDownloading(true);
    try {
      await update.downloadAndInstall();
      setUpdateVersion(null);
      pendingUpdateRef.current = null;
    } catch {
      // keep banner visible for retry
    }
    setUpdateDownloading(false);
  };

  const handleOpenLogsFolder = async () => {
    try {
      const logsDir = await getDataDirPath();
      await open(logsDir);
    } catch {
      // ignore open failure
    }
  };

  const pnlValue = stats?.total_pnl ?? 0;
  const pnlColor =
    pnlValue >= 0 ? "var(--green)" : "var(--red)";

  const chartData = stats?.pnl_history ?? [];

  return (
    <div className="min-h-screen bg-[var(--bg-primary)] flex flex-col">
      <UpdateBanner
        version={updateDownloading ? "Downloading..." : updateVersion}
        onUpdate={handleUpdate}
      />

      {/* Top Bar */}
      <div className="flex items-center justify-between px-6 py-4 border-b border-[var(--border)]">
        <h1 className="text-xl font-bold text-[var(--text-primary)] tracking-tight">
          EVPoly
        </h1>
        <div className="flex items-center gap-3">
          <ProfileSwitcher
            activeProfileId={activeProfileId}
            onSwitch={setActiveProfileId}
          />
          <button
            onClick={() => navigate("/config")}
            className="p-2 rounded-lg bg-[var(--bg-tertiary)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors"
            title="Settings"
          >
            <svg
              className="w-5 h-5 text-[var(--text-secondary)]"
              fill="none"
              stroke="currentColor"
              viewBox="0 0 24 24"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth={1.5}
                d="M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.325.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 011.37.49l1.296 2.247a1.125 1.125 0 01-.26 1.431l-1.003.827c-.293.241-.438.613-.43.992a7.723 7.723 0 010 .255c-.008.378.137.75.43.991l1.004.827c.424.35.534.955.26 1.43l-1.298 2.247a1.125 1.125 0 01-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.47 6.47 0 01-.22.128c-.331.183-.581.495-.644.869l-.213 1.281c-.09.543-.56.94-1.11.94h-2.594c-.55 0-1.019-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 01-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 01-1.369-.49l-1.297-2.247a1.125 1.125 0 01.26-1.431l1.004-.827c.292-.24.437-.613.43-.991a6.932 6.932 0 010-.255c.007-.38-.138-.751-.43-.992l-1.004-.827a1.125 1.125 0 01-.26-1.43l1.297-2.247a1.125 1.125 0 011.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.086.22-.128.332-.183.582-.495.644-.869l.214-1.28z"
              />
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth={1.5}
                d="M15 12a3 3 0 11-6 0 3 3 0 016 0z"
              />
            </svg>
          </button>
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-6 space-y-4">
        {/* Status + Controls */}
        <div className="flex items-center justify-between bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg px-4 py-3">
          <div className="flex items-center gap-4">
            <StatusBadge isRunning={isRunning} />
            <span
              className={`text-xs px-2 py-0.5 rounded font-medium ${
                simulation
                  ? "bg-[var(--yellow)]/15 text-[var(--yellow)]"
                  : "bg-[var(--green)]/15 text-[var(--green)]"
              }`}
            >
              {simulation ? "Dry Run" : "Live"}
            </span>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={() => setSimulation(!simulation)}
              disabled={isRunning}
              className="px-3 py-1.5 text-xs rounded-lg bg-[var(--bg-tertiary)] border border-[var(--border)] text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {simulation ? "Switch to Live" : "Switch to Dry Run"}
            </button>
            {!isRunning ? (
              <button
                onClick={handleStart}
                disabled={actionLoading}
                className="px-4 py-1.5 text-xs rounded-lg font-medium bg-[var(--green)] hover:bg-[var(--green)]/80 text-white transition-colors disabled:opacity-40"
              >
                Start
              </button>
            ) : (
              <>
                <button
                  onClick={handleRestart}
                  disabled={actionLoading}
                  className="px-4 py-1.5 text-xs rounded-lg font-medium bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white transition-colors disabled:opacity-40"
                >
                  Restart
                </button>
                <button
                  onClick={handleStop}
                  disabled={actionLoading}
                  className="px-4 py-1.5 text-xs rounded-lg font-medium bg-[var(--red)] hover:bg-[var(--red)]/80 text-white transition-colors disabled:opacity-40"
                >
                  Stop
                </button>
              </>
            )}
          </div>
        </div>

        {/* Stats Row */}
        <div className="grid grid-cols-4 gap-4">
          <StatCard
            label="Total PnL"
            value={`$${pnlValue.toFixed(2)}`}
            color={pnlColor}
          />
          <StatCard
            label="Win Rate"
            value={`${(stats?.win_rate ?? 0).toFixed(1)}%`}
          />
          <StatCard
            label="Total Trades"
            value={String(stats?.total_trades ?? 0)}
          />
          <StatCard
            label="USDC Balance"
            value={`$${balance.toFixed(2)}`}
          />
        </div>

        {/* PnL Chart */}
        <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-4">
          <div className="text-xs text-[var(--text-secondary)] mb-3">
            PnL Over Time
          </div>
          <div className="h-48">
            <ResponsiveContainer width="100%" height="100%">
              <LineChart data={chartData}>
                <CartesianGrid
                  strokeDasharray="3 3"
                  stroke="var(--border)"
                />
                <XAxis
                  dataKey="timestamp"
                  tick={{ fill: "var(--text-secondary)", fontSize: 11 }}
                  stroke="var(--border)"
                  tickFormatter={(v: string) => {
                    const d = new Date(v);
                    return `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
                  }}
                />
                <YAxis
                  tick={{ fill: "var(--text-secondary)", fontSize: 11 }}
                  stroke="var(--border)"
                  tickFormatter={(v: number) => `$${v}`}
                />
                <Tooltip
                  contentStyle={{
                    background: "var(--bg-tertiary)",
                    border: "1px solid var(--border)",
                    borderRadius: 8,
                    fontSize: 12,
                  }}
                  labelStyle={{ color: "var(--text-secondary)" }}
                />
                <Line
                  type="monotone"
                  dataKey="pnl"
                  stroke="var(--accent)"
                  strokeWidth={2}
                  dot={false}
                />
              </LineChart>
            </ResponsiveContainer>
          </div>
        </div>

        {/* Tables */}
        <div className="grid grid-cols-2 gap-4">
          {/* Open Positions */}
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg overflow-hidden">
            <div className="px-4 py-3 border-b border-[var(--border)]">
              <span className="text-sm font-medium text-[var(--text-primary)]">
                Open Positions
              </span>
            </div>
            <div className="overflow-x-auto">
              <table className="w-full text-xs">
                <thead>
                  <tr className="text-left text-[var(--text-secondary)] border-b border-[var(--border)]">
                    <th className="px-4 py-2 font-medium">Market</th>
                    <th className="px-4 py-2 font-medium">Side</th>
                    <th className="px-4 py-2 font-medium text-right">Size</th>
                    <th className="px-4 py-2 font-medium text-right">Entry</th>
                    <th className="px-4 py-2 font-medium text-right">Current</th>
                    <th className="px-4 py-2 font-medium text-right">PnL</th>
                  </tr>
                </thead>
                <tbody>
                  {positions.length === 0 ? (
                    <tr>
                      <td
                        colSpan={6}
                        className="px-4 py-6 text-center text-[var(--text-secondary)]"
                      >
                        No open positions
                      </td>
                    </tr>
                  ) : (
                    positions.map((pos, i) => (
                      <tr
                        key={i}
                        className="border-b border-[var(--border)] last:border-0 hover:bg-[var(--bg-tertiary)] transition-colors"
                      >
                        <td className="px-4 py-2 text-[var(--text-primary)]">
                          {pos.market}
                        </td>
                        <td className="px-4 py-2">
                          <span
                            className={
                              pos.side === "long" || pos.side === "buy"
                                ? "text-[var(--green)]"
                                : "text-[var(--red)]"
                            }
                          >
                            {pos.side.toUpperCase()}
                          </span>
                        </td>
                        <td className="px-4 py-2 text-right text-[var(--text-primary)]">
                          {pos.size.toFixed(4)}
                        </td>
                        <td className="px-4 py-2 text-right text-[var(--text-secondary)]">
                          ${pos.entry_price.toFixed(2)}
                        </td>
                        <td className="px-4 py-2 text-right text-[var(--text-secondary)]">
                          ${pos.current_price.toFixed(2)}
                        </td>
                        <td
                          className="px-4 py-2 text-right font-medium"
                          style={{
                            color:
                              pos.pnl >= 0 ? "var(--green)" : "var(--red)",
                          }}
                        >
                          ${pos.pnl.toFixed(2)}
                        </td>
                      </tr>
                    ))
                  )}
                </tbody>
              </table>
            </div>
          </div>

          {/* Recent Trades */}
          <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg overflow-hidden">
            <div className="px-4 py-3 border-b border-[var(--border)]">
              <span className="text-sm font-medium text-[var(--text-primary)]">
                Recent Trades
              </span>
            </div>
            <div className="overflow-x-auto max-h-64 overflow-y-auto">
              <table className="w-full text-xs">
                <thead className="sticky top-0 bg-[var(--bg-secondary)]">
                  <tr className="text-left text-[var(--text-secondary)] border-b border-[var(--border)]">
                    <th className="px-4 py-2 font-medium">Time</th>
                    <th className="px-4 py-2 font-medium">Market</th>
                    <th className="px-4 py-2 font-medium">Side</th>
                    <th className="px-4 py-2 font-medium text-right">Size</th>
                    <th className="px-4 py-2 font-medium text-right">Price</th>
                    <th className="px-4 py-2 font-medium text-right">
                      Outcome
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {trades.length === 0 ? (
                    <tr>
                      <td
                        colSpan={6}
                        className="px-4 py-6 text-center text-[var(--text-secondary)]"
                      >
                        No recent trades
                      </td>
                    </tr>
                  ) : (
                    trades.map((t) => (
                      <tr
                        key={t.id}
                        className="border-b border-[var(--border)] last:border-0 hover:bg-[var(--bg-tertiary)] transition-colors"
                      >
                        <td className="px-4 py-2 text-[var(--text-secondary)]">
                          {new Date(t.timestamp).toLocaleTimeString()}
                        </td>
                        <td className="px-4 py-2 text-[var(--text-primary)]">
                          {t.market}
                        </td>
                        <td className="px-4 py-2">
                          <span
                            className={
                              t.side === "long" || t.side === "buy"
                                ? "text-[var(--green)]"
                                : "text-[var(--red)]"
                            }
                          >
                            {t.side.toUpperCase()}
                          </span>
                        </td>
                        <td className="px-4 py-2 text-right text-[var(--text-primary)]">
                          {t.size.toFixed(4)}
                        </td>
                        <td className="px-4 py-2 text-right text-[var(--text-secondary)]">
                          ${t.price.toFixed(2)}
                        </td>
                        <td
                          className="px-4 py-2 text-right font-medium"
                          style={{
                            color:
                              t.outcome === "win"
                                ? "var(--green)"
                                : t.outcome === "loss"
                                ? "var(--red)"
                                : "var(--text-secondary)",
                          }}
                        >
                          {t.outcome}
                        </td>
                      </tr>
                    ))
                  )}
                </tbody>
              </table>
            </div>
          </div>
        </div>

        {/* Log Tail */}
        <div className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg overflow-hidden">
          <div className="px-4 py-3 border-b border-[var(--border)] flex items-center justify-between">
            <span className="text-sm font-medium text-[var(--text-primary)]">
              Logs
            </span>
            <span className="text-xs text-[var(--text-secondary)]">
              Last 50 lines
            </span>
          </div>
          <div
            ref={logRef}
            className="h-48 overflow-y-auto p-4 font-mono text-xs leading-5"
          >
            {logs.length === 0 ? (
              <div className="text-[var(--text-secondary)]">
                No log output yet
              </div>
            ) : (
              logs.map((line, i) => (
                <div key={i} className="flex gap-2">
                  <span className="text-[var(--text-secondary)] shrink-0">
                    {new Date(line.timestamp).toLocaleTimeString()}
                  </span>
                  <span
                    className={`shrink-0 w-12 text-right ${
                      line.level === "ERROR"
                        ? "text-[var(--red)]"
                        : line.level === "WARN"
                        ? "text-[var(--yellow)]"
                        : "text-[var(--text-secondary)]"
                    }`}
                  >
                    {line.level}
                  </span>
                  <span className="text-[var(--text-primary)]">
                    {line.content}
                  </span>
                </div>
              ))
            )}
          </div>
        </div>

        {/* Footer */}
        <div className="flex justify-end pb-2">
          <button
            onClick={handleOpenLogsFolder}
            className="text-xs text-[var(--text-secondary)] hover:text-[var(--accent)] transition-colors"
          >
            Open Logs Folder
          </button>
        </div>
      </div>
    </div>
  );
}
