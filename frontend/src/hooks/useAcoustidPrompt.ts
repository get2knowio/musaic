/**
 * The one-time "add an AcoustID key" prompt's dismissal.
 *
 * Kept by the server (not browser storage): the desktop window's address
 * changes on every launch, so anything stored in the browser is lost. The
 * dismiss answer is written straight into the cache; the server's
 * SettingsChanged event (useLibraryEventsStream) refreshes other windows.
 */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { dismissAcoustidPrompt, getAcoustidPrompt } from "@/lib/api";

export const ACOUSTID_PROMPT_QUERY_KEY = ["acoustid-prompt"] as const;

export function useAcoustidPrompt() {
  return useQuery({
    queryKey: ACOUSTID_PROMPT_QUERY_KEY,
    queryFn: getAcoustidPrompt,
    staleTime: Infinity, // changes only by the dismissal, announced on the event stream
  });
}

export function useDismissAcoustidPrompt() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: dismissAcoustidPrompt,
    onSuccess: (data) => qc.setQueryData(ACOUSTID_PROMPT_QUERY_KEY, data),
  });
}
