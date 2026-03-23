import { useEffect, useMemo, useState } from "react";
import { InfoPill } from "./InfoPill";
import { OfficialLinks } from "./OfficialLinks";
import type { GeoAccessStatus } from "../lib/tauri-commands";

function geoHeadline(status: GeoAccessStatus) {
  if (status.status === "blocked") {
    return "EVPoly is unavailable in your jurisdiction";
  }
  return "Location verification required";
}

function geoBody(status: GeoAccessStatus) {
  if (status.status === "blocked") {
    return (
      status.reason ||
      "Access is unavailable due to regulatory, sanctions, or platform restrictions."
    );
  }
  return (
    status.reason ||
    "We could not verify your location right now. By continuing, you confirm that you are not accessing EVPoly from a restricted jurisdiction and are not using a VPN, proxy, or similar service to bypass geographic restrictions."
  );
}

export function GeoAccessDialog({
  status,
  onContinue,
  onClose,
  fullScreen = false,
}: {
  status: GeoAccessStatus;
  onContinue?: () => void;
  onClose?: () => void;
  fullScreen?: boolean;
}) {
  const [checked, setChecked] = useState(false);

  useEffect(() => {
    setChecked(false);
  }, [status.status, status.checked_at]);

  const badgeTone = status.status === "blocked" ? "danger" : "warning";
  const badgeText = status.status === "blocked" ? "Access unavailable" : "Verification needed";
  const locationDetail = useMemo(() => {
    const locationParts = [status.country_name, status.region_name].filter(Boolean);
    return locationParts.length ? locationParts.join(" | ") : "Location unavailable";
  }, [status.country_name, status.region_name]);

  const containerClass = fullScreen
    ? "fixed inset-0 z-50 bg-[rgba(5,8,12,0.88)] backdrop-blur-md"
    : "fixed inset-0 z-50 bg-[rgba(5,8,12,0.72)] backdrop-blur-sm";

  return (
    <div className={containerClass}>
      <div className="mx-auto flex min-h-[100dvh] max-w-4xl items-center px-5 py-6 lg:px-8">
        <div className="w-full overflow-hidden rounded-[32px] border border-[var(--border)] bg-[linear-gradient(180deg,rgba(18,25,36,0.98),rgba(12,17,25,0.98))] shadow-[var(--shadow-soft)]">
          <div className="grid gap-0 lg:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)]">
            <div className="border-b border-[var(--border)] px-6 py-6 lg:border-b-0 lg:border-r lg:px-8 lg:py-8">
              <div className="flex items-center gap-3">
                <img src="/logo.png" alt="EVPoly" className="h-10 w-auto" />
                <InfoPill tone={badgeTone}>{badgeText}</InfoPill>
              </div>
              <div className="mt-8">
                <div className="text-xs uppercase tracking-[0.12em] text-[var(--text-muted)]">
                  Restricted jurisdictions
                </div>
                <h2 className="mt-3 text-[clamp(1.8rem,1.5rem+1vw,2.8rem)] font-semibold tracking-[-0.05em] text-[var(--text-primary)]">
                  {geoHeadline(status)}
                </h2>
                <p className="mt-4 max-w-md text-base leading-7 text-[var(--text-secondary)]">
                  {geoBody(status)}
                </p>
              </div>

              <div className="mt-8 space-y-3">
                <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4 text-sm text-[var(--text-secondary)]">
                  Location: <span className="text-[var(--text-primary)]">{locationDetail}</span>
                </div>
                <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4 text-sm text-[var(--text-secondary)]">
                  Access may be unavailable in certain countries and regions due to regulatory,
                  sanctions, or platform restrictions.
                </div>
              </div>
            </div>

            <div className="px-6 py-6 lg:px-8 lg:py-8">
              <div className="space-y-4 text-sm leading-7 text-[var(--text-secondary)]">
                <p>
                  Review the official links below for the full Terms of Service and the detailed
                  restricted-jurisdictions policy.
                </p>
                <p>
                  Use of VPNs, proxies, or similar services to bypass geographic restrictions is
                  prohibited.
                </p>
              </div>

              <div className="mt-6">
                <OfficialLinks />
              </div>

              {status.status === "unknown" ? (
                <label className="mt-6 flex cursor-pointer items-start gap-3 rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4">
                  <input
                    type="checkbox"
                    checked={checked}
                    onChange={(event) => setChecked(event.target.checked)}
                    className="mt-1 h-4 w-4 rounded border-[var(--border)] bg-[var(--bg-tertiary)] accent-[var(--accent)]"
                  />
                  <span className="text-sm text-[var(--text-primary)]">
                    I confirm that I am not accessing EVPoly from a restricted jurisdiction and am
                    not using a VPN, proxy, or similar service to bypass geographic restrictions.
                  </span>
                </label>
              ) : null}

              <div className="mt-6 flex flex-wrap gap-3">
                {status.status === "unknown" && onContinue ? (
                  <button
                    type="button"
                    onClick={onContinue}
                    disabled={!checked}
                    className="ui-button ui-button--primary"
                  >
                    Continue
                  </button>
                ) : null}

                {onClose ? (
                  <button type="button" onClick={onClose} className="ui-button">
                    {status.status === "blocked" ? "Close" : "Cancel"}
                  </button>
                ) : null}
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
