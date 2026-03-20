import { useState, useEffect, useRef, useCallback } from "react";
import { getBotStatus } from "../lib/tauri-commands";

export function useBotStatus() {
  const [status, setStatus] = useState<string>("unknown");
  const [isLoading, setIsLoading] = useState(true);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const poll = useCallback(async () => {
    try {
      const s = await getBotStatus();
      setStatus(s);
    } catch (err) {
      const message =
        typeof err === "string"
          ? err
          : err instanceof Error
          ? err.message
          : "status poll failed";
      setStatus(`error:${message}`);
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    poll();
    intervalRef.current = setInterval(poll, 2000);
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [poll]);

  return {
    status,
    isRunning: status === "running",
    isStarting: status === "starting",
    isStopping: status === "stopping",
    isUnknown: status === "unknown",
    isError: status.startsWith("error:"),
    errorMessage: status.startsWith("error:")
      ? status.slice("error:".length)
      : null,
    isLoading,
  };
}
