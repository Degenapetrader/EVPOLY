export function EmptyState({
  title,
  description,
}: {
  title: string;
  description?: string;
}) {
  return (
    <div className="empty-state">
      <div className="font-semibold text-[var(--text-primary)]">{title}</div>
      {description ? <div className="mt-1">{description}</div> : null}
    </div>
  );
}
