import type { ReactNode } from "react";

type PillTone = "neutral" | "success" | "warning" | "danger" | "accent";

export function InfoPill({
  children,
  tone = "neutral",
  className = "",
}: {
  children: ReactNode;
  tone?: PillTone;
  className?: string;
}) {
  const toneClass =
    tone === "success"
      ? "info-pill--success"
      : tone === "warning"
      ? "info-pill--warning"
      : tone === "danger"
      ? "info-pill--danger"
      : tone === "accent"
      ? "info-pill--accent"
      : "";

  return <span className={`info-pill ${toneClass} ${className}`.trim()}>{children}</span>;
}
