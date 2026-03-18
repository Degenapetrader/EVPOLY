import { useState, useEffect, useRef, useCallback } from "react";
import { getWalletBalance } from "../lib/tauri-commands";

export function useWalletBalance() {
  const [balance, setBalance] = useState<number>(0);
  const [isLoading, setIsLoading] = useState(true);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const fetch = useCallback(async () => {
    try {
      const b = await getWalletBalance();
      setBalance(b);
    } catch {
      // keep last known balance
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

  return { balance, isLoading };
}
