import { useState, useEffect } from "react";
import { useNavigate } from "react-router-dom";
import {
  isAuthInitialized,
  verifyPassword,
  setPassword,
  getActiveProfileId,
} from "../lib/tauri-commands";
import { LegalModal, hasAcceptedTerms } from "../components/LegalModal";
import { useAppContext } from "../App";

export function Login() {
  const navigate = useNavigate();
  const { setAuthenticated } = useAppContext();
  const [initialized, setInitialized] = useState<boolean | null>(null);
  const [password, setPasswordVal] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [showLegal, setShowLegal] = useState(!hasAcceptedTerms());

  useEffect(() => {
    isAuthInitialized()
      .then(setInitialized)
      .catch(() => setInitialized(false));
  }, []);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");

    if (!initialized) {
      if (password.length < 8) {
        setError("Password must be at least 8 characters");
        return;
      }
      if (password !== confirm) {
        setError("Passwords do not match");
        return;
      }
      setLoading(true);
      try {
        await setPassword(password);
        setAuthenticated(true);
        const active = await getActiveProfileId();
        navigate(active ? "/dashboard" : "/config");
      } catch (err) {
        setError(String(err));
      } finally {
        setLoading(false);
      }
    } else {
      setLoading(true);
      try {
        const valid = await verifyPassword(password);
        if (valid) {
          setAuthenticated(true);
          const active = await getActiveProfileId();
          navigate(active ? "/dashboard" : "/config");
        } else {
          setError("Incorrect password");
        }
      } catch (err) {
        setError(String(err));
      } finally {
        setLoading(false);
      }
    }
  };

  if (showLegal) {
    return <LegalModal onAccept={() => setShowLegal(false)} />;
  }

  if (initialized === null) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-[var(--bg-primary)]">
        <div className="text-[var(--text-secondary)] text-sm">Loading...</div>
      </div>
    );
  }

  return (
    <div className="min-h-screen flex items-center justify-center bg-[var(--bg-primary)]">
      <div className="w-full max-w-sm mx-4">
        <div className="text-center mb-8">
          <img src="/logo.png" alt="EVPoly" className="h-12 mx-auto" />
          <p className="text-[var(--text-secondary)] text-sm mt-2">
            {initialized ? "Welcome back" : "Set up your account"}
          </p>
        </div>

        <form
          onSubmit={handleSubmit}
          className="bg-[var(--bg-secondary)] border border-[var(--border)] rounded-lg p-6 space-y-4"
        >
          <h2 className="text-lg font-medium text-[var(--text-primary)]">
            {initialized ? "Enter Password" : "Create Password"}
          </h2>

          <div>
            <label className="block text-xs text-[var(--text-secondary)] mb-1.5">
              Password
            </label>
            <input
              type="password"
              value={password}
              onChange={(e) => setPasswordVal(e.target.value)}
              className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-[var(--text-primary)] text-sm outline-none focus:border-[var(--accent)] transition-colors"
              autoFocus
            />
          </div>

          {!initialized && (
            <div>
              <label className="block text-xs text-[var(--text-secondary)] mb-1.5">
                Confirm Password
              </label>
              <input
                type="password"
                value={confirm}
                onChange={(e) => setConfirm(e.target.value)}
                className="w-full bg-[var(--bg-tertiary)] border border-[var(--border)] rounded-lg px-3 py-2 text-[var(--text-primary)] text-sm outline-none focus:border-[var(--accent)] transition-colors"
              />
            </div>
          )}

          {error && (
            <div className="text-[var(--red)] text-sm">{error}</div>
          )}

          <button
            type="submit"
            disabled={loading || !password}
            className="w-full py-2.5 rounded-lg font-medium text-sm transition-colors bg-[var(--accent)] hover:bg-[var(--accent-hover)] text-white disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {loading
              ? "..."
              : initialized
              ? "Unlock"
              : "Create & Continue"}
          </button>
        </form>
      </div>
    </div>
  );
}
