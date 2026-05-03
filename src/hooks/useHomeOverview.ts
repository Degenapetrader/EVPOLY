import { useCallback, useEffect, useRef, useState } from "react";
import { getHomeOverview, type HomeOverview } from "../lib/tauri-commands";

export function useHomeOverview() {
  const [overview, setOverview] = useState<HomeOverview | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const inFlightRef = useRef(false);

  const refresh = useCallback(async (forceRefresh = false) => {
    if (inFlightRef.current) {
      return;
    }
    inFlightRef.current = true;
    try {
      const next = await getHomeOverview(forceRefresh);
      setOverview(next);
      setError(null);
    } catch (err) {
      setError(
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "overview refresh failed"
      );
    } finally {
      inFlightRef.current = false;
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    intervalRef.current = setInterval(() => void refresh(), 10_000);
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [refresh]);

  return { overview, isLoading, error, refresh };
}
