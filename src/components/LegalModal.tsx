import { useState } from "react";
import { open } from "@tauri-apps/plugin-shell";

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
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 backdrop-blur-sm">
      <div className="w-full max-w-lg mx-4 bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-6">
        <h2 className="text-xl font-semibold text-[var(--text-primary)] mb-4">
          Terms of Use & Risk Disclaimer
        </h2>

        <div className="text-sm text-[var(--text-secondary)] space-y-3 mb-6 max-h-60 overflow-y-auto pr-2">
          <p>
            This software is experimental and provided "as-is" without warranty
            of any kind. By using this application, you acknowledge that:
          </p>
          <ul className="list-disc pl-5 space-y-2">
            <li>
              Trading digital assets involves substantial risk of loss. You may
              lose some or all of your invested capital.
            </li>
            <li>
              The developers of this software are not financial advisors. Nothing
              in this application constitutes financial, investment, or trading
              advice.
            </li>
            <li>
              You are solely responsible for all trading decisions and outcomes
              resulting from the use of this software.
            </li>
            <li>
              Past performance is not indicative of future results. Simulated
              results do not guarantee live performance.
            </li>
          </ul>
        </div>

        <button
          onClick={() =>
            open(
              "https://github.com/Degenapetrader/EVPOLY/blob/main/poly-desktop/TERMS_OF_SERVICE.md"
            )
          }
          className="text-sm text-[var(--accent)] hover:underline mb-4 block"
        >
          View full Terms of Service
        </button>

        <label className="flex items-center gap-3 mb-5 cursor-pointer select-none">
          <input
            type="checkbox"
            checked={checked}
            onChange={(e) => setChecked(e.target.checked)}
            className="w-4 h-4 rounded border-[var(--border)] bg-[var(--bg-tertiary)] accent-[var(--accent)]"
          />
          <span className="text-sm text-[var(--text-primary)]">
            I have read and accept the terms
          </span>
        </label>

        <button
          onClick={handleAccept}
          disabled={!checked}
          className="w-full py-2.5 rounded-lg font-medium text-sm transition-colors bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white disabled:opacity-40 disabled:cursor-not-allowed"
        >
          Accept & Continue
        </button>
      </div>
    </div>
  );
}
