/**
 * Assistant (agent) pairings, their activity and the approvals they wait on.
 *
 * Every query here is kept fresh by the `AgentStateChanged` bus event, which
 * useLibraryEventsStream turns into an invalidation of ["agent", what]
 * (rule 5), so none of them goes stale on its own and no mutation below
 * invalidates anything: the event reaches the UI when the backend has
 * written the change.
 */
import { useMutation, useQuery } from "@tanstack/react-query";
import {
  createAgentPairing,
  decideAgentApproval,
  listAgentActivity,
  listAgentApprovals,
  listAgentPairings,
  revokeAgentPairing,
  setAgentPolicy,
  type AgentDecision,
  type AgentKind,
} from "@/lib/api";

export const AGENT_QUERY_KEY = ["agent"] as const;

export function useAgentPairings() {
  return useQuery({
    queryKey: ["agent", "pairings"],
    queryFn: listAgentPairings,
    staleTime: Infinity, // AgentStateChanged keeps it fresh
  });
}

export function useAgentApprovals(state: "pending" | "all" = "pending") {
  return useQuery({
    queryKey: ["agent", "approvals", state],
    queryFn: () => listAgentApprovals(state),
    staleTime: Infinity, // AgentStateChanged keeps it fresh
  });
}

export function useAgentActivity(id: string, enabled = true) {
  return useQuery({
    queryKey: ["agent", "activity", id],
    queryFn: () => listAgentActivity(id),
    enabled,
    staleTime: Infinity, // AgentStateChanged keeps it fresh
  });
}

export function useCreateAgentPairing() {
  return useMutation({ mutationFn: (body: { name: string; scopes: string[] }) => createAgentPairing(body) });
}

export function useRevokeAgentPairing() {
  return useMutation({ mutationFn: (id: string) => revokeAgentPairing(id) });
}

export function useSetAgentPolicy() {
  return useMutation({
    mutationFn: (args: { id: string; policy: Partial<Record<AgentKind, AgentDecision>> }) =>
      setAgentPolicy(args.id, args.policy),
  });
}

export function useApproveAgentApproval() {
  return useMutation({ mutationFn: (id: string) => decideAgentApproval(id, "approve") });
}

export function useDenyAgentApproval() {
  return useMutation({ mutationFn: (id: string) => decideAgentApproval(id, "deny") });
}
