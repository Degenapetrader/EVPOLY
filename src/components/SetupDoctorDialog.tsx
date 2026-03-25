import { InfoPill } from "./InfoPill";
import type { SetupDoctorResult } from "../lib/tauri-commands";

function doctorTone(status: SetupDoctorResult["status"]): "success" | "warning" | "danger" | "accent" {
  if (status === "ready" || status === "fixed") return "success";
  if (status === "needs_you") return "warning";
  return "danger";
}

function doctorBadge(status: SetupDoctorResult["status"]): string {
  if (status === "ready") return "Setup looks complete";
  if (status === "fixed") return "Fixed missing setup";
  if (status === "needs_you") return "Needs your input";
  return "Doctor failed";
}

function itemTone(status: string): "success" | "warning" | "danger" | "accent" {
  if (status === "ok" || status === "fixed") return "success";
  if (status === "missing_user" || status === "missing_generated") return "warning";
  return "danger";
}

function itemLabel(status: string): string {
  if (status === "ok") return "OK";
  if (status === "fixed") return "Fixed";
  if (status === "missing_user") return "Need Input";
  if (status === "missing_generated") return "Can Fix";
  return "Failed";
}

export function SetupDoctorDialog({
  result,
  onClose,
  onOpenSetup,
}: {
  result: SetupDoctorResult | null;
  onClose: () => void;
  onOpenSetup: () => void;
}) {
  if (!result) return null;

  return (
    <div className="fixed inset-0 z-50 bg-[rgba(5,8,12,0.72)] backdrop-blur-sm">
      <div className="mx-auto flex min-h-[100dvh] max-w-5xl items-center px-5 py-6 lg:px-8">
        <div className="w-full overflow-hidden rounded-[32px] border border-[var(--border)] bg-[linear-gradient(180deg,rgba(18,25,36,0.98),rgba(12,17,25,0.98))] shadow-[var(--shadow-soft)]">
          <div className="grid gap-0 lg:grid-cols-[minmax(0,0.82fr)_minmax(0,1.18fr)]">
            <div className="border-b border-[var(--border)] px-6 py-6 lg:border-b-0 lg:border-r lg:px-8 lg:py-8">
              <div className="flex items-center gap-3">
                <InfoPill tone={doctorTone(result.status)}>{doctorBadge(result.status)}</InfoPill>
              </div>
              <div className="mt-8">
                <div className="text-xs uppercase tracking-[0.12em] text-[var(--text-muted)]">
                  Setup Doctor
                </div>
                <h2 className="mt-3 text-[clamp(1.8rem,1.5rem+1vw,2.7rem)] font-semibold tracking-[-0.05em] text-[var(--text-primary)]">
                  {result.popup?.title || doctorBadge(result.status)}
                </h2>
                <p className="mt-4 max-w-md text-base leading-7 text-[var(--text-secondary)]">
                  {result.popup?.body ||
                    "Doctor checked your saved setup, repaired anything it could regenerate, and flagged the rest in plain English."}
                </p>
              </div>

              <div className="mt-8 diagnostics-summary">
                <div className="diagnostics-summary__item">
                  <div className="diagnostics-summary__label">Fixed</div>
                  <div className="diagnostics-summary__value">{result.fixed_count}</div>
                </div>
                <div className="diagnostics-summary__item">
                  <div className="diagnostics-summary__label">Need Input</div>
                  <div className="diagnostics-summary__value">{result.missing_user_count}</div>
                </div>
                <div className="diagnostics-summary__item">
                  <div className="diagnostics-summary__label">Bot Running</div>
                  <div className="diagnostics-summary__value">
                    {result.bot_was_running ? "Yes" : "No"}
                  </div>
                </div>
                <div className="diagnostics-summary__item">
                  <div className="diagnostics-summary__label">Bot Restarted</div>
                  <div className="diagnostics-summary__value">
                    {result.bot_restarted ? "Yes" : "No"}
                  </div>
                </div>
              </div>

              <div className="mt-6 flex flex-wrap gap-3">
                {result.popup?.cta_target === "setup" ? (
                  <button type="button" onClick={onOpenSetup} className="ui-button ui-button--primary">
                    {result.popup.cta_label || "Open Setup"}
                  </button>
                ) : null}
                <button type="button" onClick={onClose} className="ui-button">
                  Close
                </button>
              </div>
            </div>

            <div className="px-6 py-6 lg:px-8 lg:py-8">
              <div className="text-xs uppercase tracking-[0.12em] text-[var(--text-muted)]">
                Checklist
              </div>
              <div className="mt-4 max-h-[60vh] space-y-3 overflow-auto pr-1">
                {result.items.length === 0 ? (
                  <div className="empty-state">
                    No setup issues were found. This profile is ready to run with its current saved
                    credentials.
                  </div>
                ) : (
                  result.items.map((item) => (
                    <div
                      key={`${item.key}-${item.strategy ?? "core"}`}
                      className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4"
                    >
                      <div className="flex flex-wrap items-start justify-between gap-3">
                        <div className="min-w-0">
                          <div className="text-sm font-semibold tracking-[-0.02em] text-[var(--text-primary)]">
                            {item.label}
                          </div>
                          {item.strategy ? (
                            <div className="mt-1 text-xs uppercase tracking-[0.12em] text-[var(--text-muted)]">
                              {item.strategy}
                            </div>
                          ) : null}
                        </div>
                        <InfoPill tone={itemTone(item.status)}>{itemLabel(item.status)}</InfoPill>
                      </div>
                      <p className="mt-3 text-sm leading-6 text-[var(--text-secondary)]">
                        {item.message}
                      </p>
                    </div>
                  ))
                )}
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
