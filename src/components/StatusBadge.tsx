export function StatusBadge({ isRunning }: { isRunning: boolean }) {
  return (
    <span className="inline-flex items-center gap-2 text-sm font-medium">
      <span
        className={`w-2 h-2 rounded-full ${
          isRunning ? "bg-[var(--green)] shadow-[0_0_6px_var(--green)]" : "bg-[var(--red)]"
        }`}
      />
      <span
        className={
          isRunning ? "text-[var(--green)]" : "text-[var(--red)]"
        }
      >
        {isRunning ? "Running" : "Stopped"}
      </span>
    </span>
  );
}
