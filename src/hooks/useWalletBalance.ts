import { useState, useEffect, useRef, useCallback } from "react";
import { getWalletBalance } from "../lib/tauri-commands";

export function useWalletBalance() {
  const [balance, setBalance] = useState<number>(0);
  const [isLoading, setIsLoading] = useState(true);
  const [isStale, setIsStale] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const fetch = useCallback(async () => {
    try {
      const b = await getWalletBalance();
      setBalance(b);
      setIsStale(false);
      setError(null);
    } catch (err) {
      setIsStale(true);
      setError(
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "wallet balance refresh failed"
      );
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetch();
    intervalRef.current = setInterval(fetch, 30_000);
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [fetch]);

  return { balance, isLoading, isStale, error, refresh: fetch };
}
