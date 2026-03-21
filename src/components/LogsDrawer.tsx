import { useCallback, useEffect, useState } from "react";
import {
  getLogLines,
  getManualLogLines,
  openLogsFolder,
  type LogLine,
} from "../lib/tauri-commands";

type LogsDrawerMode = "bot" | "manual";

export function LogsDrawer({
  open,
  mode,
  onClose,
}: {
  open: boolean;
  mode: LogsDrawerMode;
  onClose: () => void;
}) {
  const [lines, setLines] = useState<LogLine[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadLines = useCallback(async () => {
    if (!open) return;
    setLoading(true);
    try {
      const next =
        mode === "manual"
          ? await getManualLogLines(120)
          : await getLogLines(120);
      setLines(next);
      setError(null);
    } catch (err) {
      setError(
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "Failed to load logs"
      );
    } finally {
      setLoading(false);
    }
  }, [mode, open]);

  const handleOpenFolder = async () => {
    try {
      await openLogsFolder();
      setError(null);
    } catch (err) {
      setError(
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "Failed to open log folder"
      );
    }
  };

  useEffect(() => {
    if (!open) return;
    void loadLines();
    const timer = setInterval(() => void loadLines(), 4000);
    return () => clearInterval(timer);
  }, [open, loadLines]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div className="logs-drawer" role="dialog" aria-modal="true" aria-label="Logs">
      <button
        type="button"
        className="logs-drawer__backdrop"
        aria-label="Close logs"
        onClick={onClose}
      />
      <aside className="logs-drawer__panel">
        <div className="logs-drawer__header">
          <div>
            <div className="logs-drawer__eyebrow">Hidden by default</div>
            <div className="logs-drawer__title">
              {mode === "manual" ? "Manual Service Logs" : "Bot Logs"}
            </div>
            <div className="logs-drawer__subtitle">
              Keep the main UI clean. Open logs only when you need the details.
            </div>
          </div>
          <div className="logs-drawer__actions">
            <button type="button" className="ui-button" onClick={() => void loadLines()}>
              Refresh
            </button>
            <button type="button" className="ui-button" onClick={handleOpenFolder}>
              Open Folder
            </button>
            <button type="button" className="ui-button ui-button--danger" onClick={onClose}>
              Close
            </button>
          </div>
        </div>

        {error ? <div className="inline-alert">{error}</div> : null}

        <div className="logs-drawer__body">
          {loading && lines.length === 0 ? (
            <div className="empty-state">Loading logs...</div>
          ) : lines.length === 0 ? (
            <div className="empty-state">No recent log lines yet.</div>
          ) : (
            <div className="logs-list">
              {lines.map((line, index) => (
                <div
                  key={`${line.timestamp}-${index}`}
                  className={`logs-list__row logs-list__row--${line.level.toLowerCase()}`}
                >
                  <div className="logs-list__meta">
                    <span className="logs-list__time">{line.timestamp}</span>
                    <span className="logs-list__level">{line.level}</span>
                  </div>
                  <div className="logs-list__content">{line.content}</div>
                </div>
              ))}
            </div>
          )}
        </div>
      </aside>
    </div>
  );
}
