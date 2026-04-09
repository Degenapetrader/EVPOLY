import { useCallback, useEffect, useRef, useState } from "react";

type PollOptions<T> = {
  enabled?: boolean;
  pollMs?: number;
  load: () => Promise<T[]>;
};

export function usePolledList<T>({ enabled = true, pollMs = 8_000, load }: PollOptions<T>) {
  const [items, setItems] = useState<T[]>([]);
  const [isLoading, setIsLoading] = useState(enabled);
  const [error, setError] = useState<string | null>(null);
  const inFlightRef = useRef(false);

  const refresh = useCallback(async () => {
    if (!enabled || inFlightRef.current) {
      return;
    }
    inFlightRef.current = true;
    try {
      const next = await load();
      setItems(next);
      setError(null);
    } catch (err) {
      setError(
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "refresh failed"
      );
    } finally {
      inFlightRef.current = false;
      setIsLoading(false);
    }
  }, [enabled, load]);

  useEffect(() => {
    if (!enabled) {
      setIsLoading(false);
      return;
    }
    setIsLoading(true);
    void refresh();
    const timer = setInterval(() => void refresh(), pollMs);
    return () => clearInterval(timer);
  }, [enabled, pollMs, refresh]);

  return { items, isLoading, error, refresh };
}
