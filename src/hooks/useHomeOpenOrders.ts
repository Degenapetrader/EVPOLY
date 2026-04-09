import { useCallback } from "react";
import { getHomeOpenOrdersApi, type HomeApiOpenOrderItem } from "../lib/tauri-commands";
import { usePolledList } from "./usePolledList";

export function useHomeOpenOrders(limit = 120, enabled = true) {
  const load = useCallback(
    () => getHomeOpenOrdersApi(limit).then((items) => items ?? ([] as HomeApiOpenOrderItem[])),
    [limit]
  );

  return usePolledList<HomeApiOpenOrderItem>({ enabled, pollMs: 10_000, load });
}
