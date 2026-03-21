import type { ReactNode } from "react";

export function TopStrip({
  eyebrow,
  title,
  description,
  meta,
}: {
  eyebrow?: string;
  title: string;
  description?: string;
  meta?: ReactNode;
}) {
  return (
    <header className="top-strip">
      <div className="top-strip__copy">
        {eyebrow ? <div className="top-strip__eyebrow">{eyebrow}</div> : null}
        <h1 className="top-strip__title">{title}</h1>
        {description ? <p className="top-strip__description">{description}</p> : null}
      </div>
      {meta ? <div className="top-strip__meta">{meta}</div> : null}
    </header>
  );
}
