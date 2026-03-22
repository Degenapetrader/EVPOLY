import { useCallback, useEffect, useRef, useState } from "react";
import { getWalletSyncStatus, type WalletSyncStatus } from "../lib/tauri-commands";

export function useWalletSyncStatus() {
  const [status, setStatus] = useState<WalletSyncStatus | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const inFlightRef = useRef(false);

  const refresh = useCallback(async () => {
    if (inFlightRef.current) {
      return;
    }
    inFlightRef.current = true;
    try {
      const next = await getWalletSyncStatus();
      setStatus(next);
      setError(null);
    } catch (err) {
      setError(
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "wallet sync status refresh failed"
      );
    } finally {
      inFlightRef.current = false;
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    intervalRef.current = setInterval(() => void refresh(), 5_000);
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [refresh]);

  return { status, isLoading, error, refresh };
}
