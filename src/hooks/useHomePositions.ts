import { useCallback } from "react";
import { getHomePositionsApi, type HomeApiPositionItem } from "../lib/tauri-commands";
import { usePolledList } from "./usePolledList";

export function useHomePositions(limit = 80, enabled = true) {
  const load = useCallback(
    () => getHomePositionsApi(limit).then((items) => items ?? ([] as HomeApiPositionItem[])),
    [limit]
  );

  return usePolledList<HomeApiPositionItem>({ enabled, pollMs: 12_000, load });
}
