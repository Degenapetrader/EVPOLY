type BotStatus =
  | "unknown"
  | "stopped"
  | "starting"
  | "running"
  | "stopping"
  | `error:${string}`;

export function StatusBadge({ status }: { status: BotStatus | string }) {
  const isError = status.startsWith("error:");
  const isRunning = status === "running";
  const isStarting = status === "starting";
  const isStopping = status === "stopping";
  const isUnknown = status === "unknown";

  const label = isError
    ? "Error"
    : isStarting
    ? "Starting"
    : isStopping
    ? "Stopping"
    : isRunning
    ? "Running"
    : isUnknown
    ? "Unknown"
    : "Stopped";
  const toneClass = isRunning
    ? "info-pill--success"
    : isStarting || isStopping
    ? "info-pill--warning"
    : isUnknown
    ? ""
    : "info-pill--danger";
  const dotClass = isRunning
    ? "bg-[var(--green)]"
    : isStarting || isStopping
    ? "bg-[var(--yellow)]"
    : isUnknown
    ? "bg-[var(--text-secondary)]"
    : "bg-[var(--red)]";

  return (
    <span className={`info-pill ${toneClass}`.trim()}>
      <span className={`w-2 h-2 rounded-full ${dotClass}`} />
      <span>{label}</span>
    </span>
  );
}
