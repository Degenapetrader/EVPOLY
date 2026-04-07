export function UpdateBanner({
  version,
  onUpdate,
  actionLabel,
  detail,
  disabled = false,
}: {
  version: string | null;
  onUpdate: () => void;
  actionLabel?: string;
  detail?: string;
  disabled?: boolean;
}) {
  if (!version) return null;

  return (
    <div className="surface-panel">
      <div className="surface-panel__body flex flex-wrap items-center justify-between gap-3 pt-[var(--space-5)]">
        <div className="space-y-1">
          <div className="text-sm text-[var(--text-primary)]">
            Update available{" "}
            <span className="mono-data text-[var(--text-secondary)]">({version})</span>
          </div>
          {detail ? <div className="text-xs text-[var(--text-secondary)]">{detail}</div> : null}
        </div>
        <button
          type="button"
          onClick={onUpdate}
          disabled={disabled}
          className="ui-button ui-button--accent text-sm"
        >
          {actionLabel ?? "Update Now"}
        </button>
      </div>
    </div>
  );
}
