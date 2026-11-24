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
import { connectClaudeDesktop } from "@/lib/desktop";
import {
  createAgentPairing,
  decideAgentApproval,
  getAgentDiagnostics,
  getClaudeDesktopStatus,
  listAgentActivity,
  listAgentApprovals,
  listAgentChanges,
  listAgentPairings,
  revokeAgentPairing,
  setAgentPolicy,
  undoAgentChange,
  type AgentDecision,
  type AgentKind,
} from "@/lib/api";

export const AGENT_QUERY_KEY = ["agent"] as const;
/** The recorded changes. Whether one can be undone also depends on the
 * album's files, so the album events refresh it too. */
export const AGENT_CHANGES_QUERY_KEY = ["agent", "changes"] as const;

export function useAgentPairings() {
  return useQuery({
    queryKey: ["agent", "pairings"],
    queryFn: listAgentPairings,
    staleTime: Infinity, // AgentStateChanged keeps it fresh
  });
}

/** The four facts per pairing. Refreshed by `AgentStateChanged(hosts)` and by
 * the `work` family (a claim changes "last work claimed"); times shown are
 * the time the backend recorded, so nothing here ticks on its own. */
export const AGENT_DIAGNOSTICS_QUERY_KEY = ["agent", "diagnostics"] as const;

export function useAgentDiagnostics() {
  return useQuery({
    queryKey: AGENT_DIAGNOSTICS_QUERY_KEY,
    queryFn: getAgentDiagnostics,
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

export function useAgentChanges(pairingId: string, enabled = true) {
  return useQuery({
    queryKey: [...AGENT_CHANGES_QUERY_KEY, pairingId],
    queryFn: () => listAgentChanges(pairingId),
    enabled,
    staleTime: Infinity, // AgentStateChanged and the album events keep it fresh
  });
}

export function useUndoAgentChange() {
  return useMutation({ mutationFn: (id: string) => undoAgentChange(id) });
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

/** What Connect Claude Desktop reports. It depends on files outside Musaic
 * (Claude's own folder and configuration) that no event reports, so unlike
 * every other query here it is read again each time the dialog opens
 * (stale time 0, dropped when the dialog closes). */
export function useClaudeDesktopStatus(enabled: boolean) {
  return useQuery({
    queryKey: ["agent", "claude-desktop"],
    queryFn: getClaudeDesktopStatus,
    enabled,
    staleTime: 0, // external files no event reports
    gcTime: 0,
  });
}

/** The hand-off: the shell writes the extension file and opens it. */
export function useConnectClaudeDesktop() {
  return useMutation({ mutationFn: connectClaudeDesktop });
}
