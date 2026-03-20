import { useState, useEffect, useRef, useCallback } from "react";
import {
  getTradeStats,
  getRecentTrades,
  getOpenPositions,
  type TradeStats,
  type Trade,
  type Position,
} from "../lib/tauri-commands";

export function useTradeData(isRunning: boolean) {
  const [stats, setStats] = useState<TradeStats | null>(null);
  const [trades, setTrades] = useState<Trade[]>([]);
  const [positions, setPositions] = useState<Position[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [isStale, setIsStale] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [s, t, p] = await Promise.all([
        getTradeStats(),
        getRecentTrades(50),
        getOpenPositions(),
      ]);
      setStats(s);
      setTrades(t);
      setPositions(p);
      setIsStale(false);
      setError(null);
    } catch (err) {
      setIsStale(true);
      setError(
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "trade data refresh failed"
      );
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();

    if (intervalRef.current) clearInterval(intervalRef.current);

    if (isRunning) {
      intervalRef.current = setInterval(refresh, 10_000);
    }

    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [isRunning, refresh]);

  return { stats, trades, positions, isLoading, isStale, error, refresh };
}
