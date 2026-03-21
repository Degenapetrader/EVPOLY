import type { ReactNode } from "react";

export function SectionPanel({
  title,
  subtitle,
  actions,
  className = "",
  bodyClassName = "",
  children,
}: {
  title?: string;
  subtitle?: string;
  actions?: ReactNode;
  className?: string;
  bodyClassName?: string;
  children: ReactNode;
}) {
  return (
    <section className={`surface-panel ${className}`.trim()}>
      {title || subtitle || actions ? (
        <div className="surface-panel__header">
          <div className="surface-panel__copy">
            {title ? <h2 className="surface-panel__title">{title}</h2> : null}
            {subtitle ? <p className="surface-panel__subtitle">{subtitle}</p> : null}
          </div>
          {actions ? <div className="shrink-0">{actions}</div> : null}
        </div>
      ) : null}
      <div className={`surface-panel__body ${bodyClassName}`.trim()}>{children}</div>
    </section>
  );
}
