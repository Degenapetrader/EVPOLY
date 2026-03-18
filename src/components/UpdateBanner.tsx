export function UpdateBanner({
  version,
  onUpdate,
}: {
  version: string | null;
  onUpdate: () => void;
}) {
  if (!version) return null;

  return (
    <div className="flex items-center justify-center gap-3 px-4 py-2 bg-[var(--accent)] text-white text-sm">
      <span>Update available ({version})</span>
      <button
        onClick={onUpdate}
        className="px-3 py-0.5 rounded bg-white/20 hover:bg-white/30 transition-colors text-xs font-medium"
      >
        Update Now
      </button>
    </div>
  );
}
