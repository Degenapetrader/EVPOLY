type BotStatus = "stopped" | "starting" | "running" | "stopping" | `error:${string}`;

export function StatusBadge({ status }: { status: BotStatus | string }) {
  const isError = status.startsWith("error:");
  const isRunning = status === "running";
  const isStarting = status === "starting";
  const isStopping = status === "stopping";

  const label = isError
    ? "Error"
    : isStarting
    ? "Starting"
    : isStopping
    ? "Stopping"
    : isRunning
    ? "Running"
    : "Stopped";
  const colorClass = isRunning
    ? "text-[var(--green)]"
    : isStarting || isStopping
    ? "text-[var(--yellow)]"
    : "text-[var(--red)]";
  const dotClass = isRunning
    ? "bg-[var(--green)] shadow-[0_0_6px_var(--green)]"
    : isStarting || isStopping
    ? "bg-[var(--yellow)] shadow-[0_0_6px_var(--yellow)]"
    : "bg-[var(--red)]";

  return (
    <span className="inline-flex items-center gap-2 text-sm font-medium">
      <span className={`w-2 h-2 rounded-full ${dotClass}`} />
      <span className={colorClass}>{label}</span>
    </span>
  );
}
