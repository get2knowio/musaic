/**
 * Plain-language wording for assistant pairings and approvals, shared by
 * Settings and the Review inbox.
 */
import type { AgentApproval, AgentApprovalDiff, AgentDecision, AgentKind } from "./api";

/** What each permission lets an assistant do. */
export const AGENT_KIND_LABEL: Record<AgentKind, string> = {
  "library.read": "Read your library",
  "proposal.submit": "Suggest fixes for you to review",
  "compute.scan": "Scan the library",
  "compute.analyze": "Analyse tracks (tempo, loudness)",
  "tags.write": "Change tags",
  "covers.write": "Set covers",
  "files.rename": "Rename files and folders",
  "work.cancel": "Cancel delegated research",
};

export const AGENT_DECISION_LABEL: Record<AgentDecision, string> = {
  allow: "Allow",
  ask: "Ask me",
  refuse: "Refuse",
};

/** Reading and suggesting have no "ask" step: Allow or Refuse only. */
export function decisionsFor(kind: string): AgentDecision[] {
  return kind === "library.read" || kind === "proposal.submit" || kind === "work.cancel" ? ["allow", "refuse"] : ["allow", "ask", "refuse"];
}

const OUTCOME_WORDS: Record<string, string> = {
  ok: "Done",
  started: "Started",
  awaiting_approval: "Waiting for your approval",
};

/** An activity outcome ("ok", "refused:FORBIDDEN") in plain words. */
export function outcomeText(outcome: string): string {
  const known = OUTCOME_WORDS[outcome];
  if (known) return known;
  if (outcome.startsWith("refused:")) {
    const why = outcome.slice("refused:".length).toLowerCase().replace(/_/g, " ");
    return why ? `Refused (${why})` : "Refused";
  }
  return outcome.replace(/[_:]/g, " ");
}

const KIND_ACTION: Record<string, string> = {
  "tags.write": "change tags",
  "covers.write": "set the cover",
  "files.rename": "rename files",
};

const ACTION_TEXT: Record<AgentApproval["action"], string | null> = {
  fix_apply: null,
  scan_start: "scan the library",
  checks_start: "run health checks",
  analysis_start: "analyse and write tags",
};

function joinWords(parts: string[]): string {
  if (parts.length <= 1) return parts.join("");
  return `${parts.slice(0, -1).join(", ")} and ${parts[parts.length - 1]}`;
}

/** "<Assistant> wants to change tags for <Album>". */
export function approvalSentence(a: AgentApproval): string {
  let what = ACTION_TEXT[a.action];
  if (!what) {
    const parts = a.kinds.map((k) => KIND_ACTION[k]).filter((s): s is string => !!s);
    what = parts.length ? joinWords(parts) : "make a change";
  }
  const target = a.albumLabel ? ` for ${a.albumLabel}` : "";
  return `${a.pairingName} wants to ${what}${target}`;
}

/** A one-line label for what the proposal would change. Never a source of truth. */
export function diffSummary(d: AgentApprovalDiff | null): string | null {
  if (!d) return null;
  const parts: string[] = [];
  if (d.tags.length) {
    const fields = [...new Set(d.tags.map((t) => t.field))];
    parts.push(`${d.tags.length === 1 ? "1 tag change" : `${d.tags.length} tag changes`} (${fields.join(", ")})`);
  }
  if (d.cover) parts.push("new cover");
  if (d.files.length) parts.push(d.files.length === 1 ? "1 file renamed" : `${d.files.length} files renamed`);
  if (d.folder) parts.push("folder renamed");
  return parts.length ? parts.join("; ") : null;
}

/** "12 min left", "under a minute left", or "Expired". */
export function timeLeftText(expiresAt: string, now: number = Date.now()): string {
  const t = Date.parse(expiresAt);
  if (Number.isNaN(t)) return "";
  const sec = Math.floor((t - now) / 1000);
  if (sec <= 0) return "Expired";
  if (sec < 60) return "under a minute left";
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min} min left`;
  const h = Math.floor(min / 60);
  return `${h} h ${min % 60} min left`;
}
