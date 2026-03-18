import { useState, useEffect, useRef } from "react";
import {
  listProfiles,
  setActiveProfile,
  type Profile,
} from "../lib/tauri-commands";

function truncateAddress(addr: string): string {
  if (addr.length <= 12) return addr;
  return `${addr.slice(0, 6)}...${addr.slice(-4)}`;
}

export function ProfileSwitcher({
  activeProfileId,
  onSwitch,
}: {
  activeProfileId: string | null;
  onSwitch: (id: string) => void;
}) {
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    listProfiles()
      .then(setProfiles)
      .catch(() => {});
  }, [activeProfileId]);

  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, []);

  const active = profiles.find((p) => p.id === activeProfileId);

  const handleSelect = async (id: string) => {
    try {
      await setActiveProfile(id);
      onSwitch(id);
    } catch {
      // handle error silently
    }
    setOpen(false);
  };

  return (
    <div ref={ref} className="relative">
      <button
        onClick={() => setOpen(!open)}
        className="flex items-center gap-2 px-3 py-2 rounded-lg bg-[var(--bg-tertiary)] border border-[var(--border)] hover:border-[var(--accent)] transition-colors text-sm"
      >
        <span className="text-[var(--text-primary)]">
          {active?.name || "No Profile"}
        </span>
        {active && (
          <span className="text-[var(--text-secondary)] text-xs">
            {truncateAddress(active.wallet_address)}
          </span>
        )}
        <svg
          className={`w-4 h-4 text-[var(--text-secondary)] transition-transform ${
            open ? "rotate-180" : ""
          }`}
          fill="none"
          stroke="currentColor"
          viewBox="0 0 24 24"
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2}
            d="M19 9l-7 7-7-7"
          />
        </svg>
      </button>

      {open && (
        <div className="absolute right-0 top-full mt-1 w-64 bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg shadow-xl z-40 overflow-hidden">
          {profiles.map((p) => (
            <button
              key={p.id}
              onClick={() => handleSelect(p.id)}
              className={`w-full text-left px-4 py-3 flex flex-col gap-0.5 hover:bg-[var(--bg-tertiary)] transition-colors ${
                p.id === activeProfileId
                  ? "bg-[var(--bg-tertiary)]"
                  : ""
              }`}
            >
              <span className="text-sm text-[var(--text-primary)]">
                {p.name}
              </span>
              <span className="text-xs text-[var(--text-secondary)]">
                {truncateAddress(p.wallet_address)}
              </span>
            </button>
          ))}
          {profiles.length === 0 && (
            <div className="px-4 py-3 text-sm text-[var(--text-secondary)]">
              No profiles found
            </div>
          )}
        </div>
      )}
    </div>
  );
}
