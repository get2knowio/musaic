/**
 * Research batches: the requests one Fix all run sent to the assistant, shown
 * as one entry in Review and Activity.
 *
 * Kept fresh by the `work` family of AgentStateChanged: useLibraryEventsStream
 * invalidates ["research"], which every key here sits under, so none of them
 * goes stale on its own and the Cancel mutation invalidates nothing (rule 5).
 */
import { useMutation, useQuery } from "@tanstack/react-query";

import { cancelResearchBatch, getResearchBatch, listResearchBatches } from "@/lib/api";

export const RESEARCH_BATCHES_QUERY_KEY = ["research", "batches"] as const;

export function useResearchBatches() {
  return useQuery({
    queryKey: RESEARCH_BATCHES_QUERY_KEY,
    queryFn: listResearchBatches,
    staleTime: Infinity, // AgentStateChanged(work) keeps it fresh
  });
}

/** One batch with its items; read only while `enabled` (the entry is open). */
export function useResearchBatch(batchId: string, enabled: boolean) {
  return useQuery({
    queryKey: [...RESEARCH_BATCHES_QUERY_KEY, batchId],
    queryFn: () => getResearchBatch(batchId),
    enabled,
    staleTime: Infinity, // AgentStateChanged(work) keeps it fresh
  });
}

export function useCancelResearchBatch() {
  return useMutation({
    mutationFn: (batchId: string) => cancelResearchBatch(batchId),
  });
}
