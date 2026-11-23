/**
 * The delegated work Musaic is waiting on or holding, for Review.
 *
 * Kept fresh by the `work` family of AgentStateChanged (useLibraryEventsStream
 * invalidates ["research"], which this key sits under), so it never goes stale
 * on its own and the Retry mutation below invalidates nothing (rule 5).
 */
import { useMutation, useQuery } from "@tanstack/react-query";

import { listDelegatedWork, retryDelegatedWork, type DelegatedWorkItem } from "@/lib/api";

export const DELEGATED_WORK_QUERY_KEY = ["research", "work"] as const;

export function useDelegatedWork() {
  return useQuery({
    queryKey: DELEGATED_WORK_QUERY_KEY,
    queryFn: listDelegatedWork,
    staleTime: Infinity, // AgentStateChanged(work) keeps it fresh
  });
}

export function useRetryDelegatedWork() {
  return useMutation({
    mutationFn: (item: Pick<DelegatedWorkItem, "workId" | "revision">) => retryDelegatedWork(item),
  });
}
