export function UpdateBanner({
  version,
  onUpdate,
}: {
  version: string | null;
  onUpdate: () => void;
}) {
  if (!version) return null;

  return (
    <div className="surface-panel">
      <div className="surface-panel__body flex flex-wrap items-center justify-between gap-3 pt-[var(--space-5)]">
        <span className="text-sm text-[var(--text-primary)]">
          Update available <span className="mono-data text-[var(--text-secondary)]">({version})</span>
        </span>
        <button
          onClick={onUpdate}
          className="ui-button ui-button--accent text-sm"
        >
          Update Now
        </button>
      </div>
    </div>
  );
}
