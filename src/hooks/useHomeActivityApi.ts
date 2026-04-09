import { useCallback } from "react";
import { getHomeActivityApi, type HomeApiActivityItem } from "../lib/tauri-commands";
import { usePolledList } from "./usePolledList";

export function useHomeActivityApi(limit = 24, enabled = true) {
  const load = useCallback(
    () => getHomeActivityApi(limit).then((items) => items ?? ([] as HomeApiActivityItem[])),
    [limit]
  );

  return usePolledList<HomeApiActivityItem>({ enabled, pollMs: 8_000, load });
}
