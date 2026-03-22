import { useCallback, useEffect, useRef, useState } from "react";
import { getHomeActivity, type HomeActivityItem } from "../lib/tauri-commands";

export function useHomeActivity(limit = 12) {
  const [items, setItems] = useState<HomeActivityItem[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const cursorRef = useRef<number | null>(null);
  const inFlightRef = useRef(false);

  const refresh = useCallback(async (options?: { reset?: boolean }) => {
    if (inFlightRef.current) {
      return;
    }
    inFlightRef.current = true;
    try {
      const next = await getHomeActivity(limit, options?.reset ? null : cursorRef.current);
      cursorRef.current = next.next_cursor;
      setItems((current) => {
        if (options?.reset || next.reset) {
          return next.items;
        }
        return [...current, ...next.items].slice(-limit);
      });
      setError(null);
    } catch (err) {
      setError(
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "activity refresh failed"
      );
    } finally {
      inFlightRef.current = false;
      setIsLoading(false);
    }
  }, [limit]);

  useEffect(() => {
    cursorRef.current = null;
    setItems([]);
    setIsLoading(true);
    void refresh({ reset: true });
    intervalRef.current = setInterval(() => void refresh(), 4_000);
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [refresh]);

  return { items, isLoading, error, refresh };
}
