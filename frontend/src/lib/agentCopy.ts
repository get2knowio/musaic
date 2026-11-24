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

/** "3 minutes ago" for a time the backend recorded, or "never". The page
 * shows the time it was last true; it does not tick on its own. */
export function ago(iso: string | null | undefined, now: Date = new Date()): string {
  if (!iso) return "never";
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return iso;
  const secs = Math.max(0, Math.round((now.getTime() - t) / 1000));
  if (secs < 60) return "just now";
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins} ${mins === 1 ? "minute" : "minutes"} ago`;
  const hours = Math.round(mins / 60);
  if (hours < 48) return `${hours} ${hours === 1 ? "hour" : "hours"} ago`;
  return new Date(iso).toLocaleDateString();
}

/** The four facts Settings shows for each pairing, in this order. */
export const AGENT_FACT_LABEL = {
  paired: "Added",
  reachable: "Connected",
  subscription: "Listening for work",
  claimed: "Last request picked up",
} as const;

/** What to tell an agent so it processes waiting work when the person asks. */
export const PROCESS_PENDING_INSTRUCTIONS =
  "Process pending Musaic work using the musaic-work-delegation skill: call musaic_capabilities, list queued work with musaic_work_list, then get, claim, research and complete each item.";

/** Why Musaic cannot say "ready": what each fact does and does not show. */
export const AGENT_FACTS_NOTE =
  "These are separate facts, each with the time it was last true. Musaic does not know whether the assistant will act on waiting work unless it is listening now.";

/** Connect Claude Desktop and the skill download (spec 007). */
export const CLAUDE_COPY = {
  connectAction: "Connect Claude Desktop",
  skillAction: "Save the Musaic skill for Claude",
  dialogLead:
    "Musaic will create an extension file for Claude Desktop and open it. Claude Desktop then asks whether to install it. Nothing is installed until you choose Install there.",
  commandLabel: "Claude Desktop will run this program on this computer:",
  unsignedNote:
    "Claude Desktop may say the extension's publisher is not verified. Musaic's extension is not signed.",
  opened:
    "The extension file was opened. Finish in Claude Desktop, then add an assistant here if you have not yet.",
  openFailed: "Musaic could not open the extension file.",
  notFound: "Claude Desktop was not found on this computer. Install it, then come back here.",
  linux: "Claude Desktop extensions are not available on this platform.",
  /** Follows `linux` only when a command is shown below it. */
  linuxUseBelow: "Use the command below instead.",
  ownEntry: "Claude Desktop is already connected to Musaic through its configuration file.",
  /** Follows `ownEntry` only where the extension exists, with the removal command. */
  ownEntrySwitch: "To switch to the extension, remove that entry first:",
  foreignEntry:
    "Claude Desktop's configuration file already has a \"musaic\" entry that Musaic did not write. Musaic leaves it alone. Remove it yourself before installing the extension, or keep using it.",
  extensionDetected: "The Musaic extension is already installed in Claude Desktop.",
  /** Follows `extensionDetected` only when the file can be opened from here. */
  extensionReinstall: "Opening the file again reinstalls it.",
  extensionAndEntry:
    "Claude Desktop is running Musaic twice: once from its configuration file and once from the extension. Remove one of them.",
  unreadable:
    "Musaic could not read Claude Desktop's configuration file, so it cannot tell whether Musaic is already connected.",
  programMissing: "Musaic's command-line program was not found beside the app. Reinstall Musaic.",
  unstable:
    "Musaic is running from a disk image or a temporary location. Move it to your Applications folder, open it from there, and connect again.",
  outsideShell: "Open Musaic's desktop app to connect with one click.",
  setupInstead: "Or connect through Claude Desktop's configuration file with this command:",
  skillSteps: [
    "Save the zip.",
    "In Claude, open Settings, then the skills section.",
    "Upload the zip.",
  ],
  skillNote: "Musaic cannot install a skill into Claude for you.",
  skillDownload: "Download the skill zip",
  otherHeading: "Not available yet",
  otherBody:
    "The Grok app, the ChatGPT app and web, the Gemini app, and Claude on the web and on phones can only reach servers on the internet. Musaic listens only on this computer, so they cannot connect to it yet.",
} as const;

/** "Running Musaic's command-line program 0.1.0; the app is 0.2.0. ..." */
export function olderProgramText(cli: string, app: string): string {
  return `Running Musaic's command-line program ${cli}; the app is ${app}. Connect Claude Desktop again to update it.`;
}

/** Not the same version, and not comparable as older. */
export function differentProgramText(cli: string, app: string): string {
  return `Running Musaic's command-line program ${cli}; the app is ${app}.`;
}

/** Characters a shell reads as syntax, not as part of a word. The backslash is
 * not among them: it separates a Windows path's folders. */
const SHELL_SYNTAX = /[\s"'$`&|;<>()*?[\]{}#~!]/;

/** A command-line word as it must be typed: quoted when it holds a space or
 * anything else a shell would read (an apostrophe in a home folder's name, a
 * parenthesis in a folder name). Inside the quotes `"`, `$` and the backquote
 * are escaped. */
function shellWord(word: string): string {
  if (!SHELL_SYNTAX.test(word)) return word;
  return `"${word.replace(/(["$`])/g, "\\$1")}"`;
}

/** A program and its arguments as one line to type or read. */
export function commandLine(c: { program: string; args: string[] }): string {
  return [c.program, ...c.args].map(shellWord).join(" ");
}
