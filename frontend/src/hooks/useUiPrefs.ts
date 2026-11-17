/**
 * The light/dark theme and the Library tile size, kept by the server.
 *
 * The desktop window's address changes on every launch, so anything stored in
 * the browser is lost; localStorage stays only as a cache that lets the first
 * paint use the last choice. Once the server answers, its values win. Writes
 * go out from setTheme / setTileColumns; the server's SettingsChanged event
 * (useLibraryEventsStream) brings other windows up to date.
 */
import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";

import { getUiPrefs } from "@/lib/api";
import { applyServerTheme } from "@/lib/theme";
import { applyServerTileColumns } from "./useTileColumns";

export const UI_PREFS_QUERY_KEY = ["ui-prefs"] as const;

export function useUiPrefs() {
  return useQuery({
    queryKey: UI_PREFS_QUERY_KEY,
    queryFn: getUiPrefs,
    staleTime: Infinity, // changes only by a write, announced on the event stream
  });
}

/** Mount once (App): apply the server's preferences whenever they arrive. */
export function useSyncUiPrefs(): void {
  const { data } = useUiPrefs();
  useEffect(() => {
    if (!data) return;
    applyServerTheme(data.theme);
    applyServerTileColumns(data.tile_columns);
  }, [data]);
}
