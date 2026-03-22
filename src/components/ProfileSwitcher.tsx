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
  const [loadError, setLoadError] = useState<string | null>(null);
  const [switchError, setSwitchError] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    listProfiles()
      .then((nextProfiles) => {
        setProfiles(nextProfiles);
        setLoadError(null);
      })
      .catch((err) =>
        setLoadError(
          typeof err === "string"
            ? err
            : err instanceof Error
            ? err.message
            : "failed to load profiles"
        )
      );
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
      setSwitchError(null);
    } catch (err) {
      setSwitchError(
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "failed to switch profile"
      );
    }
    setOpen(false);
  };

  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="info-pill"
      >
        <span>{active?.name || "No Profile"}</span>
        {active && (
          <span className="text-xs text-[var(--text-secondary)] mono-data">
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
        <div className="absolute right-0 top-full mt-2 w-72 surface-panel z-40 overflow-hidden">
          {profiles.map((p) => (
            <button
              key={p.id}
              type="button"
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
              <span className="text-xs text-[var(--text-secondary)] mono-data">
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
      {loadError || switchError ? (
        <div className="mt-1 text-xs text-[var(--red)]">{loadError ?? switchError}</div>
      ) : null}
    </div>
  );
}
