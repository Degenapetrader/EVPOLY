import { open } from "@tauri-apps/plugin-shell";
import { OFFICIAL_LINKS } from "../lib/official-links";

const LINK_ITEMS = [
  { label: "Website", href: OFFICIAL_LINKS.website },
  { label: "X", href: OFFICIAL_LINKS.x },
  { label: "GitHub", href: OFFICIAL_LINKS.github },
  { label: "Terms", href: OFFICIAL_LINKS.terms },
  { label: "Restricted Jurisdictions", href: OFFICIAL_LINKS.restricted },
];

export function OfficialLinks({
  className = "",
  includeDocs = true,
}: {
  className?: string;
  includeDocs?: boolean;
}) {
  const items = includeDocs ? LINK_ITEMS : LINK_ITEMS.slice(0, 3);

  return (
    <div className={`official-links ${className}`.trim()}>
      {items.map((item) => (
        <button
          key={item.label}
          type="button"
          onClick={() => void open(item.href)}
          className="ui-button ui-button--compact"
        >
          {item.label}
        </button>
      ))}
    </div>
  );
}
