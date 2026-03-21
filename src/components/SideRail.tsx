import type { ReactNode } from "react";
import { NavLink } from "react-router-dom";

export type SideRailItem = {
  label: string;
  to?: string;
  onClick?: () => void;
};

export function SideRail({
  title = "EVPoly",
  subtitle,
  items,
  children,
}: {
  title?: string;
  subtitle?: string;
  items: SideRailItem[];
  children?: ReactNode;
}) {
  return (
    <aside className="app-rail">
      <div className="app-rail__brand">
        <div className="app-rail__title">{title}</div>
        {subtitle ? <div className="app-rail__subtitle">{subtitle}</div> : null}
      </div>

      <nav className="app-rail__nav">
        {items.map((item) =>
          item.to ? (
            <NavLink
              key={`${item.label}:${item.to}`}
              to={item.to}
              className={({ isActive }) =>
                `rail-link ${isActive ? "rail-link--active" : ""}`.trim()
              }
            >
              <span>{item.label}</span>
            </NavLink>
          ) : (
            <button
              key={item.label}
              type="button"
              onClick={item.onClick}
              className="rail-link rail-link--action"
            >
              <span>{item.label}</span>
            </button>
          )
        )}
      </nav>

      {children ? <div className="app-rail__extra">{children}</div> : null}
    </aside>
  );
}
