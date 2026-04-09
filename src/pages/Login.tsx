import { useEffect, useState, type ReactNode } from "react";
import { useNavigate } from "react-router-dom";
import { GeoAccessDialog } from "../components/GeoAccessDialog";
import { InfoPill } from "../components/InfoPill";
import { SectionPanel } from "../components/SectionPanel";
import { LegalModal, hasAcceptedTerms } from "../components/LegalModal";
import {
  createProfile,
  getGeoAccessStatus,
  getActiveProfileId,
  type GeoAccessStatus,
  initializePassword,
  isAuthInitialized,
  listProfiles,
  resetLocalAppData,
  setActiveProfile,
  verifyPassword,
} from "../lib/tauri-commands";
import { useAppContext } from "../App";

const DEFAULT_PROFILE_NAME = "Main Profile";
const DEFAULT_SIGNATURE_TYPE = 1;

function AuthShell({
  badge,
  title,
  description,
  panel,
}: {
  badge: string;
  title: string;
  description: string;
  panel: ReactNode;
}) {
  return (
    <div className="min-h-[100dvh] bg-[radial-gradient(circle_at_top_left,rgba(54,211,153,0.08),transparent_26%),radial-gradient(circle_at_bottom_right,rgba(73,116,255,0.08),transparent_24%),var(--bg-primary)]">
      <div className="mx-auto grid min-h-[100dvh] max-w-6xl gap-6 px-5 py-6 lg:grid-cols-[minmax(0,1.05fr)_minmax(24rem,0.88fr)] lg:px-8 lg:py-8">
        <section className="flex min-h-[16rem] flex-col justify-between rounded-[32px] border border-[var(--border)] bg-[linear-gradient(180deg,rgba(18,25,36,0.96),rgba(12,17,25,0.96))] px-6 py-6 shadow-[var(--shadow-soft)] lg:px-8 lg:py-8">
          <div>
            <div className="flex items-center gap-3">
              <img src="/logo.png" alt="EVPoly" className="h-10 w-auto" />
              <InfoPill tone="accent">{badge}</InfoPill>
            </div>
            <div className="mt-8 max-w-xl">
              <div className="text-xs uppercase tracking-[0.12em] text-[var(--text-muted)]">
                Secure desktop trading
              </div>
              <h1 className="mt-3 text-[clamp(2.2rem,1.8rem+1.5vw,3.8rem)] font-semibold tracking-[-0.05em] text-[var(--text-primary)]">
                {title}
              </h1>
              <p className="mt-4 max-w-lg text-base leading-7 text-[var(--text-secondary)]">
                {description}
              </p>
            </div>
          </div>

          <div className="grid gap-3 md:grid-cols-3">
            <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4">
              <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">Private</div>
              <div className="mt-2 text-sm text-[var(--text-secondary)]">
                Your password protects your desktop profiles on this machine.
              </div>
            </div>
            <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4">
              <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">Simple</div>
              <div className="mt-2 text-sm text-[var(--text-secondary)]">
                One password unlocks the app and your saved trading setup.
              </div>
            </div>
            <div className="rounded-[20px] border border-[var(--border)] bg-[rgba(16,22,31,0.72)] px-4 py-4">
              <div className="text-xs uppercase tracking-[0.08em] text-[var(--text-muted)]">Local first</div>
              <div className="mt-2 text-sm text-[var(--text-secondary)]">
                Nothing changes until you unlock and choose what to do next.
              </div>
            </div>
          </div>
        </section>

        <div className="flex items-center">
          <div className="w-full">{panel}</div>
        </div>
      </div>
    </div>
  );
}

export function Login() {
  const navigate = useNavigate();
  const { setActiveProfileId, setAuthenticated } = useAppContext();
  const [initialized, setInitialized] = useState<boolean | null>(null);
  const [authInitError, setAuthInitError] = useState<string | null>(null);
  const [password, setPasswordVal] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [showLegal, setShowLegal] = useState(false);
  const [geoStatus, setGeoStatus] = useState<GeoAccessStatus | null>(null);
  const [geoAcknowledged, setGeoAcknowledged] = useState(false);
  const [resetArmed, setResetArmed] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [resetNotice, setResetNotice] = useState("");

  const resolveInit = async () => {
    setAuthInitError(null);
    try {
      const [init, nextGeoStatus] = await Promise.all([
        isAuthInitialized(),
        getGeoAccessStatus(),
      ]);
      setInitialized(init);
      setGeoStatus(nextGeoStatus);
      setGeoAcknowledged(false);
      setShowLegal(!hasAcceptedTerms());
    } catch (err) {
      const message =
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "failed to initialize auth state";
      setAuthInitError(message);
      setInitialized(null);
      setGeoStatus(null);
    }
  };

  useEffect(() => {
    void resolveInit();
  }, []);

  useEffect(() => {
    setResetArmed(false);
    setResetNotice("");
  }, [initialized]);

  const ensureActiveProfile = async () => {
    const current = await getActiveProfileId();
    if (current) {
      setActiveProfileId(current);
      return current;
    }

    const profiles = await listProfiles();
    if (profiles.length > 0) {
      await setActiveProfile(profiles[0].id);
      setActiveProfileId(profiles[0].id);
      return profiles[0].id;
    }

    const created = await createProfile(DEFAULT_PROFILE_NAME, "", DEFAULT_SIGNATURE_TYPE);
    await setActiveProfile(created.id);
    setActiveProfileId(created.id);
    return created.id;
  };

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    setError("");

    if (!initialized) {
      if (password.length < 8) {
        setError("Password must be at least 8 characters.");
        return;
      }
      if (password !== confirm) {
        setError("Passwords do not match.");
        return;
      }

      setLoading(true);
      try {
        await initializePassword(password);
        setAuthenticated(true);
        await ensureActiveProfile();
        navigate("/home");
      } catch (err) {
        setError(String(err));
      } finally {
        setLoading(false);
      }
      return;
    }

    setLoading(true);
    try {
      const valid = await verifyPassword(password);
      if (valid) {
        setAuthenticated(true);
        await ensureActiveProfile();
        navigate("/home");
      } else {
        setError("Incorrect password.");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  const handleResetLocalData = async () => {
    setError("");
    setResetNotice("");
    setResetting(true);
    try {
      await resetLocalAppData();
      setPasswordVal("");
      setConfirm("");
      setResetArmed(false);
      setAuthenticated(false);
      setActiveProfileId(null);
      await resolveInit();
      setResetNotice(
        "Local EVPoly data on this machine was wiped. Create a new password and set the app up again.",
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setResetting(false);
    }
  };

  if (initialized === null && !authInitError) {
    return (
      <AuthShell
        badge="Starting"
        title="Loading your secure workspace"
        description="EVPoly is checking whether this desktop already has a password and profile ready."
        panel={
          <SectionPanel title="Loading" subtitle="This usually only takes a moment.">
            <div className="space-y-3">
              <div className="text-sm text-[var(--text-secondary)]">
                Preparing the auth state and opening the correct flow for this machine.
              </div>
              <div className="inline-alert inline-alert--warning">Please wait...</div>
            </div>
          </SectionPanel>
        }
      />
    );
  }

  if (authInitError) {
    return (
      <AuthShell
        badge="Needs attention"
        title="EVPoly could not open the auth state"
        description="The app hit an initialization problem before the password screen could load."
        panel={
          <SectionPanel title="Auth initialization error" subtitle="Retry once before digging into anything technical.">
            <div className="space-y-4">
              <div className="inline-alert">{authInitError}</div>
              <button
                type="button"
                onClick={() => void resolveInit()}
                className="ui-button ui-button--primary w-full justify-center"
              >
                Retry
              </button>
            </div>
          </SectionPanel>
        }
      />
    );
  }

  if (geoStatus?.status === "blocked") {
    return <GeoAccessDialog status={geoStatus} fullScreen />;
  }

  if (geoStatus?.status === "unknown" && !geoAcknowledged) {
    return (
      <GeoAccessDialog
        status={geoStatus}
        fullScreen
        onContinue={() => setGeoAcknowledged(true)}
      />
    );
  }

  if (showLegal) {
    return <LegalModal onAccept={() => setShowLegal(false)} />;
  }

  return (
    <AuthShell
      badge={initialized ? "Welcome back" : "New setup"}
      title={initialized ? "Unlock EVPoly" : "Create your EVPoly password"}
      description={
        initialized
          ? "Enter the password you already chose for this desktop to unlock your trading profiles."
          : "Pick one password for this desktop. You will use it to unlock your saved profiles and settings."
      }
      panel={
        <SectionPanel
          title={initialized ? "Enter password" : "Create password"}
          subtitle={
            initialized
              ? "Use the same password you created earlier."
              : "Use at least 8 characters so your local profile stays protected."
          }
        >
          <form onSubmit={handleSubmit} className="space-y-4">
            <div>
              <label className="mb-1.5 block text-xs text-[var(--text-secondary)]">Password</label>
              <input
                type="password"
                value={password}
                onChange={(event) => setPasswordVal(event.target.value)}
                autoFocus
                className="w-full rounded-[18px] border border-[var(--border)] bg-[var(--bg-tertiary)] px-4 py-3 text-sm text-[var(--text-primary)] outline-none transition-colors focus:border-[var(--accent)]"
              />
            </div>

            {!initialized ? (
              <div>
                <label className="mb-1.5 block text-xs text-[var(--text-secondary)]">
                  Confirm password
                </label>
                <input
                  type="password"
                  value={confirm}
                  onChange={(event) => setConfirm(event.target.value)}
                  className="w-full rounded-[18px] border border-[var(--border)] bg-[var(--bg-tertiary)] px-4 py-3 text-sm text-[var(--text-primary)] outline-none transition-colors focus:border-[var(--accent)]"
                />
              </div>
            ) : null}

            {error ? <div className="inline-alert">{error}</div> : null}
            {resetNotice ? <div className="inline-alert inline-alert--warning">{resetNotice}</div> : null}

            <button
              type="submit"
              disabled={loading || resetting || !password}
              className="ui-button ui-button--primary w-full justify-center"
            >
              {loading ? "Working..." : initialized ? "Unlock" : "Create and Continue"}
            </button>

            <div className="rounded-[18px] border border-[var(--border)] bg-[rgba(16,22,31,0.76)] px-4 py-4 text-sm text-[var(--text-secondary)]">
              {initialized
                ? "Your password never needs to be typed anywhere else in the app."
                : "After this step, EVPoly will take you straight to setup or the dashboard."}
            </div>

            {initialized ? (
              <div className="rounded-[20px] border border-[rgba(240,109,100,0.24)] bg-[rgba(240,109,100,0.08)] px-4 py-4 text-sm text-[var(--text-secondary)]">
                <div className="flex flex-wrap items-center gap-3">
                  <InfoPill tone="danger">Forgot password?</InfoPill>
                  <div className="text-sm font-medium text-[var(--text-primary)]">
                    Reset local EVPoly data on this computer
                  </div>
                </div>
                <p className="mt-3 leading-6">
                  This wipes saved profiles, encrypted secrets, runtime files, logs, and local bot
                  history for this Windows user. It cannot recover your old password. You will need
                  to onboard again after the reset.
                </p>
                {resetArmed ? (
                  <div className="mt-4 space-y-3">
                    <div className="inline-alert">
                      This action is destructive and only affects local EVPoly data on this machine.
                      The installed app stays in place, but your saved setup does not.
                    </div>
                    <div className="flex flex-col gap-2 sm:flex-row">
                      <button
                        type="button"
                        disabled={loading || resetting}
                        onClick={() => void handleResetLocalData()}
                        className="ui-button ui-button--danger w-full justify-center sm:flex-1"
                      >
                        {resetting ? "Wiping local data..." : "Yes, wipe this device"}
                      </button>
                      <button
                        type="button"
                        disabled={loading || resetting}
                        onClick={() => setResetArmed(false)}
                        className="ui-button w-full justify-center sm:flex-1"
                      >
                        Cancel
                      </button>
                    </div>
                  </div>
                ) : (
                  <button
                    type="button"
                    disabled={loading || resetting}
                    onClick={() => setResetArmed(true)}
                    className="ui-button ui-button--danger mt-4 w-full justify-center"
                  >
                    Reset local data
                  </button>
                )}
              </div>
            ) : null}
          </form>
        </SectionPanel>
      }
    />
  );
}
