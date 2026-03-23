import type { ReactNode } from "react";
import { SideRail, type SideRailItem } from "./SideRail";
import { TopStrip } from "./TopStrip";

export function AppShell({
  railTitle,
  railSubtitle,
  railLogoSrc,
  railLogoAlt,
  railItems,
  railChildren,
  eyebrow,
  title,
  description,
  meta,
  banner,
  children,
  contentClassName = "",
}: {
  railTitle?: string;
  railSubtitle?: string;
  railLogoSrc?: string;
  railLogoAlt?: string;
  railItems: SideRailItem[];
  railChildren?: ReactNode;
  eyebrow?: string;
  title: string;
  description?: string;
  meta?: ReactNode;
  banner?: ReactNode;
  children: ReactNode;
  contentClassName?: string;
}) {
  return (
    <div className="app-shell">
      <SideRail
        title={railTitle}
        subtitle={railSubtitle}
        logoSrc={railLogoSrc}
        logoAlt={railLogoAlt}
        items={railItems}
      >
        {railChildren}
      </SideRail>
      <div className="app-shell__main">
        <TopStrip eyebrow={eyebrow} title={title} description={description} meta={meta} />
        {banner}
        <div className={`app-shell__scroll ${contentClassName}`.trim()}>{children}</div>
      </div>
    </div>
  );
}
