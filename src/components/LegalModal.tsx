import { useState } from "react";
import { InfoPill } from "./InfoPill";
import { OfficialLinks } from "./OfficialLinks";

const STORAGE_KEY = "evpoly_legal_accepted";

export function hasAcceptedTerms(): boolean {
  return localStorage.getItem(STORAGE_KEY) === "true";
}

export function LegalModal({ onAccept }: { onAccept: () => void }) {
  const [checked, setChecked] = useState(false);

  const handleAccept = () => {
    localStorage.setItem(STORAGE_KEY, "true");
    onAccept();
  };

  return (
    <div className="fixed inset-0 z-50 bg-[rgba(5,8,12,0.84)] backdrop-blur-md">
      <div className="mx-auto flex min-h-[100dvh] max-w-4xl items-center px-5 py-6 lg:px-8">
        <div className="w-full overflow-hidden rounded-[32px] border border-[var(--border)] bg-[linear-gradient(180deg,rgba(18,25,36,0.98),rgba(12,17,25,0.98))] shadow-[var(--shadow-soft)]">
          <div className="grid lg:grid-cols-[minmax(0,0.88fr)_minmax(0,1.12fr)]">
            <div className="border-b border-[var(--border)] px-6 py-6 lg:border-b-0 lg:border-r lg:px-8 lg:py-8">
              <div className="flex items-center gap-3">
                <img src="/logo.png" alt="EVPoly" className="h-10 w-auto" />
                <InfoPill tone="warning">Before you continue</InfoPill>
              </div>
              <div className="mt-8">
                <div className="text-xs uppercase tracking-[0.12em] text-[var(--text-muted)]">
                  Terms and risk
                </div>
                <h2 className="mt-3 text-[clamp(1.8rem,1.5rem+1vw,2.8rem)] font-semibold tracking-[-0.05em] text-[var(--text-primary)]">
                  Read this once, then continue with a clear head.
                </h2>
                <p className="mt-4 max-w-md text-base leading-7 text-[var(--text-secondary)]">
                  EVPoly is experimental trading software. You stay in control, and you accept the
                  risk of using it.
                </p>
              </div>

              <div className="mt-8 space-y-3">
                <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4 text-sm text-[var(--text-secondary)]">
                  Trading can lose money quickly. Only trade with risk you understand and accept.
                </div>
                <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4 text-sm text-[var(--text-secondary)]">
                  Nothing in the app is financial advice. You are responsible for every order and every outcome.
                </div>
              </div>
            </div>

            <div className="px-6 py-6 lg:px-8 lg:py-8">
              <div className="space-y-4 text-sm leading-7 text-[var(--text-secondary)]">
                <p>
                  By using EVPoly, you confirm that you understand the software is provided as-is and
                  that live trading always carries real financial risk.
                </p>
                <ul className="space-y-3">
                  <li className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-3">
                    You may lose some or all of the capital you trade with.
                  </li>
                  <li className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-3">
                    Simulated results do not guarantee live results.
                  </li>
                  <li className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-3">
                    You are solely responsible for your trading decisions and their consequences.
                  </li>
                  <li className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-3">
                    EVPoly may be unavailable in certain restricted jurisdictions due to regulatory,
                    sanctions, or platform restrictions.
                  </li>
                </ul>
              </div>

              <div className="mt-6 rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4 text-sm text-[var(--text-secondary)]">
                Review the full Terms of Service and Restricted Jurisdictions policy before
                continuing.
              </div>

              <div className="mt-6">
                <OfficialLinks includeReferral={false} />
              </div>

              <label className="mt-6 flex cursor-pointer items-start gap-3 rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4">
                <input
                  type="checkbox"
                  checked={checked}
                  onChange={(event) => setChecked(event.target.checked)}
                  className="mt-1 h-4 w-4 rounded border-[var(--border)] bg-[var(--bg-tertiary)] accent-[var(--accent)]"
                />
                <span className="text-sm text-[var(--text-primary)]">
                  I have read and accept the terms and understand the risks of using this software.
                </span>
              </label>

              <button
                type="button"
                onClick={handleAccept}
                disabled={!checked}
                className="ui-button ui-button--primary mt-6 w-full justify-center"
              >
                Accept and Continue
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
