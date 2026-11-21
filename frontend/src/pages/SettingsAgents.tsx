import { ChevronDown, ChevronRight, Plug } from "lucide-react";
import * as Dialog from "@radix-ui/react-dialog";
import { useRef, useState } from "react";
import { toast } from "sonner";

import {
  useAgentActivity,
  useAgentChanges,
  useAgentPairings,
  useCreateAgentPairing,
  useRevokeAgentPairing,
  useSetAgentPolicy,
  useUndoAgentChange,
} from "@/hooks/useAgents";
import { useReturnFocus } from "@/components/DetailSheet/useReturnFocus";
import {
  AGENT_KINDS,
  type AgentChange,
  type AgentDecision,
  type AgentKind,
  type AgentPairing,
  type AgentPairingCreated,
} from "@/lib/api";
import { AGENT_DECISION_LABEL, AGENT_KIND_LABEL, decisionsFor, outcomeText } from "@/lib/agentCopy";
import { errorText } from "@/lib/errors";

/** Read and propose are the safe starting scopes; the rest are opt-in. */
const DEFAULT_SCOPES: readonly AgentKind[] = ["library.read", "proposal.submit"];

const BUTTON =
  "h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition disabled:opacity-60 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent";

function when(iso: string | null): string {
  if (!iso) return "never";
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

/**
 * Assistants panel for the Settings page: pair an assistant, choose what it
 * may do, see what it did, and revoke it. A pairing's secret is handed to
 * the assistant's own tool when it is set up and is never shown here.
 */
export default function AgentsPanel() {
  const pairings = useAgentPairings();
  const [created, setCreated] = useState<AgentPairingCreated | null>(null);
  const [revoking, setRevoking] = useState<AgentPairing | null>(null);
  const revoke = useRevokeAgentPairing();

  const all = pairings.data ?? [];
  const active = all.filter((p) => !p.revokedAt);
  const revoked = all.filter((p) => p.revokedAt);

  const confirmRevoke = () => {
    if (!revoking) return;
    const target = revoking;
    revoke.mutate(target.id, {
      onSuccess: () => {
        toast.success(`Revoked ${target.name}`, { duration: 2500 });
        setRevoking(null);
      },
      onError: (err) => {
        toast.error(errorText(err, "Could not revoke the pairing"), { duration: 4000 });
        setRevoking(null);
      },
    });
  };

  return (
    <div className="space-y-4 mb-6">
      <p className="text-xs text-fg-3">
        Anyone who can run programs as you on this computer can use a pairing. Revoke pairings you no
        longer use.
      </p>

      <PairForm onCreated={setCreated} />
      {created && <ConnectInfo pairing={created} onDone={() => setCreated(null)} />}

      {pairings.isError && !pairings.data ? (
        <div role="alert" className="rounded-md border border-warn/40 bg-warn-soft px-3 py-2 text-sm flex items-center gap-3">
          <span className="flex-1 text-fg-2">Couldn't load the paired assistants.</span>
          <button type="button" onClick={() => void pairings.refetch()} className={BUTTON}>
            Retry
          </button>
        </div>
      ) : pairings.isPending ? (
        <p className="text-sm text-fg-3">Loading…</p>
      ) : all.length === 0 ? (
        <p className="rounded-md border border-dashed border-line px-3 py-6 text-sm text-fg-3">
          No assistants are connected. Pair one to let it read your library and suggest fixes.
        </p>
      ) : (
        <ul aria-label="Paired assistants" className="rounded-md border border-line bg-panel-2 divide-y divide-line">
          {[...active, ...revoked].map((p) => (
            <PairingRow key={p.id} pairing={p} onRevoke={() => setRevoking(p)} />
          ))}
        </ul>
      )}

      {revoking && (
        <RevokeConfirm
          pairing={revoking}
          busy={revoke.isPending}
          onConfirm={confirmRevoke}
          onCancel={() => setRevoking(null)}
        />
      )}
    </div>
  );
}

function PairForm({ onCreated }: { onCreated: (p: AgentPairingCreated) => void }) {
  const create = useCreateAgentPairing();
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<Set<string>>(() => new Set(DEFAULT_SCOPES));

  const toggle = (kind: string) =>
    setScopes((prev) => {
      const next = new Set(prev);
      if (next.has(kind)) next.delete(kind);
      else next.add(kind);
      return next;
    });

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    create.mutate(
      { name: name.trim(), scopes: AGENT_KINDS.filter((k) => scopes.has(k)) },
      {
        onSuccess: (p) => {
          onCreated(p);
          setName("");
          setScopes(new Set(DEFAULT_SCOPES));
        },
      },
    );
  };

  return (
    <form onSubmit={submit} aria-label="Pair an assistant" className="rounded-md border border-line bg-panel-2 p-3 space-y-3">
      <div>
        <label htmlFor="agent-name" className="block text-sm font-medium mb-1">
          Assistant name
        </label>
        <input
          id="agent-name"
          data-settings-focus
          type="text"
          value={name}
          // The backend refuses a name over 64 characters (once trimmed).
          maxLength={64}
          onChange={(e) => setName(e.target.value)}
          placeholder="For example: My assistant"
          className="h-8 w-full max-w-xs px-2 rounded border border-line bg-panel text-sm"
        />
      </div>
      <fieldset>
        <legend className="text-sm font-medium mb-1">What it may do</legend>
        <div className="space-y-1">
          {AGENT_KINDS.map((kind) => (
            <label key={kind} className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={scopes.has(kind)} onChange={() => toggle(kind)} />
              {AGENT_KIND_LABEL[kind]}
            </label>
          ))}
        </div>
      </fieldset>
      {create.isError && (
        <p role="alert" className="text-xs text-err">
          {errorText(create.error, "Could not pair the assistant")}
        </p>
      )}
      <button type="submit" disabled={!name.trim() || scopes.size === 0 || create.isPending} className={`${BUTTON} inline-flex items-center gap-1.5`}>
        <Plug className="w-3.5 h-3.5" aria-hidden />
        {create.isPending ? "Pairing…" : "Pair assistant"}
      </button>
    </form>
  );
}

/** A command-line word as it must be typed: quoted when it holds a space
 * (an absolute path to the bundled CLI or a config folder can). */
function shellWord(word: string): string {
  return /\s/.test(word) ? `"${word.replace(/"/g, '\\"')}"` : word;
}

function ConnectInfo({ pairing, onDone }: { pairing: AgentPairingCreated; onDone: () => void }) {
  const command = [pairing.connect.command, ...pairing.connect.args].map(shellWord).join(" ");
  return (
    <section aria-label="Connect the assistant" className="rounded-md border border-accent-line bg-accent-soft p-3 space-y-2 text-sm">
      <h3 className="font-medium">{pairing.name} is paired. Now connect it.</h3>
      <p className="text-fg-2">Run this command in a terminal on this computer and follow its steps:</p>
      <pre className="mono text-xs bg-panel rounded border border-line px-2 py-1.5 overflow-x-auto select-all">{command}</pre>
      <p className="text-fg-3 text-xs">
        To set it up by hand instead, see docs/agents.md. Musaic never shows a secret here.
      </p>
      <button type="button" onClick={onDone} className={BUTTON}>
        Done
      </button>
    </section>
  );
}

function PairingRow({ pairing, onRevoke }: { pairing: AgentPairing; onRevoke: () => void }) {
  const setPolicy = useSetAgentPolicy();
  const [open, setOpen] = useState(false);
  const [changesOpen, setChangesOpen] = useState(false);
  const isRevoked = !!pairing.revokedAt;
  const scoped = AGENT_KINDS.filter((k) => pairing.scopes.includes(k));

  const change = (kind: AgentKind, decision: AgentDecision) =>
    setPolicy.mutate(
      { id: pairing.id, policy: { [kind]: decision } },
      { onError: (err) => toast.error(errorText(err, "Could not change the permission"), { duration: 4000 }) },
    );

  return (
    <li className="p-3 space-y-2" data-pairing-id={pairing.id}>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="font-medium text-sm">{pairing.name}</span>
        {isRevoked && <span className="text-xs text-err">Revoked {when(pairing.revokedAt)}</span>}
        <div className="flex-1" />
        {!isRevoked && (
          <button type="button" onClick={onRevoke} aria-label={`Revoke ${pairing.name}`} className={BUTTON}>
            Revoke
          </button>
        )}
      </div>
      <div className="text-xs text-fg-3">
        Can: {scoped.length ? scoped.map((k) => AGENT_KIND_LABEL[k]).join("; ") : "nothing"}
        <span className="mx-1.5">·</span>Paired {when(pairing.createdAt)}
        <span className="mx-1.5">·</span>Last used {when(pairing.lastUsedAt)}
      </div>
      {!isRevoked && (
        <div role="group" aria-label={`Permissions for ${pairing.name}`} className="grid grid-cols-[1fr_auto] gap-x-3 gap-y-1 items-center text-sm">
          {AGENT_KINDS.map((kind) => {
            const inScope = pairing.scopes.includes(kind);
            const id = `policy-${pairing.id}-${kind}`;
            return (
              <div key={kind} className="contents">
                <label htmlFor={id} className={inScope ? "text-fg-2" : "text-fg-4"}>
                  {AGENT_KIND_LABEL[kind]}
                </label>
                <select
                  id={id}
                  disabled={!inScope}
                  value={pairing.policy[kind] ?? "refuse"}
                  onChange={(e) => change(kind, e.target.value as AgentDecision)}
                  className="h-7 px-1.5 rounded border border-line bg-panel text-xs disabled:opacity-60"
                >
                  {decisionsFor(kind).map((d) => (
                    <option key={d} value={d}>
                      {AGENT_DECISION_LABEL[d]}
                    </option>
                  ))}
                </select>
              </div>
            );
          })}
        </div>
      )}
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-controls={`activity-${pairing.id}`}
        className="inline-flex items-center gap-1 text-xs text-accent hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent rounded-sm"
      >
        {open ? <ChevronDown className="w-3 h-3" aria-hidden /> : <ChevronRight className="w-3 h-3" aria-hidden />}
        Activity
      </button>
      <div id={`activity-${pairing.id}`} hidden={!open}>
        {open && <ActivityList id={pairing.id} />}
      </div>
      <button
        type="button"
        onClick={() => setChangesOpen((o) => !o)}
        aria-expanded={changesOpen}
        aria-controls={`changes-${pairing.id}`}
        className="inline-flex items-center gap-1 text-xs text-accent hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent rounded-sm"
      >
        {changesOpen ? <ChevronDown className="w-3 h-3" aria-hidden /> : <ChevronRight className="w-3 h-3" aria-hidden />}
        Recent changes
      </button>
      <div id={`changes-${pairing.id}`} hidden={!changesOpen}>
        {changesOpen && <ChangesList pairing={pairing} />}
      </div>
    </li>
  );
}

const CHANGE_STATE_LABEL: Record<AgentChange["state"], string> = {
  recorded: "Not finished",
  applied: "Applied",
  partial: "Partly applied",
  failed: "Not applied",
  undone: "Undone",
};

/** The changes this assistant applied, newest first, each with an Undo the
 * backend says whether it can run (the files must still hold what the
 * change left). */
function ChangesList({ pairing }: { pairing: AgentPairing }) {
  const changes = useAgentChanges(pairing.id);
  const undo = useUndoAgentChange();
  const [confirming, setConfirming] = useState<AgentChange | null>(null);

  const confirmUndo = () => {
    if (!confirming) return;
    const target = confirming;
    undo.mutate(target.id, {
      onSuccess: (r) => {
        if (r.complete) toast.success(`Undone on ${albumName(target)}`, { duration: 2500 });
        else toast.warning(r.message ?? "Not everything could be put back", { duration: 6000 });
        setConfirming(null);
      },
      onError: (err) => {
        toast.error(errorText(err, "Could not undo the change"), { duration: 6000 });
        setConfirming(null);
      },
    });
  };

  if (changes.isPending) return <p className="text-xs text-fg-3">Loading…</p>;
  if (changes.isError && !changes.data) return <p className="text-xs text-fg-3">Couldn't load the changes.</p>;
  const rows = changes.data ?? [];
  if (rows.length === 0) return <p className="text-xs text-fg-3">No changes yet.</p>;
  return (
    <>
      <ul aria-label={`Recent changes by ${pairing.name}`} className="max-h-72 overflow-y-auto text-xs divide-y divide-line border border-line rounded">
        {rows.map((c) => {
          const reasonId = `undo-reason-${c.id}`;
          return (
            <li key={c.id} className="px-2 py-1.5 flex flex-wrap items-center gap-x-3 gap-y-1" data-change-id={c.id}>
              <span className="text-fg-3 tabular-nums">{when(c.createdAt)}</span>
              <span className="text-fg font-medium">{albumName(c)}</span>
              <span className="text-fg-2">{c.summary}</span>
              <span className="text-fg-3">{CHANGE_STATE_LABEL[c.state] ?? c.state}</span>
              <div className="flex-1" />
              {!c.undoes && (
                <button
                  type="button"
                  disabled={!c.undoable || undo.isPending}
                  onClick={() => setConfirming(c)}
                  aria-label={`Undo the change on ${albumName(c)}`}
                  aria-describedby={c.reason ? reasonId : undefined}
                  className={`${BUTTON} h-7 px-2 text-xs`}
                >
                  Undo
                </button>
              )}
              {!c.undoable && c.reason && !c.undoes && (
                <span id={reasonId} className="basis-full text-fg-3">
                  {c.reason}
                </span>
              )}
            </li>
          );
        })}
      </ul>
      {confirming && (
        <UndoConfirm
          change={confirming}
          busy={undo.isPending}
          onConfirm={confirmUndo}
          onCancel={() => setConfirming(null)}
        />
      )}
    </>
  );
}

function albumName(c: AgentChange): string {
  return c.albumLabel ?? "this album";
}

function UndoConfirm({
  change,
  busy,
  onConfirm,
  onCancel,
}: {
  change: AgentChange;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const returnFocus = useReturnFocus(true);
  const cancelRef = useRef<HTMLButtonElement>(null);
  return (
    <Dialog.Root open onOpenChange={(o) => { if (!o) onCancel(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-[60] bg-black/45 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed top-1/2 left-1/2 z-[61] -translate-x-1/2 -translate-y-1/2 w-[min(440px,95vw)] bg-panel border border-line rounded-md shadow-sheet outline-none"
          // Starts on Cancel, so Enter does not overwrite anything.
          onOpenAutoFocus={(e) => {
            e.preventDefault();
            cancelRef.current?.focus();
          }}
          onCloseAutoFocus={returnFocus}
        >
          <div className="px-4 py-3 border-b border-line">
            <Dialog.Title className="font-semibold text-md">Undo this change?</Dialog.Title>
          </div>
          <Dialog.Description className="px-4 py-3.5 text-sm text-fg-2">
            Put back the tags, names and cover this assistant changed on {albumName(change)}?
          </Dialog.Description>
          <p className="px-4 pb-3.5 text-xs text-fg-3">{change.summary}</p>
          <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
            <button ref={cancelRef} type="button" onClick={onCancel} className={BUTTON}>
              Cancel
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={onConfirm}
              className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60"
            >
              {busy ? "Undoing…" : "Undo"}
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function ActivityList({ id }: { id: string }) {
  const activity = useAgentActivity(id);
  if (activity.isPending) return <p className="text-xs text-fg-3">Loading…</p>;
  if (activity.isError && !activity.data) return <p className="text-xs text-fg-3">Couldn't load the activity.</p>;
  const rows = activity.data ?? [];
  if (rows.length === 0) return <p className="text-xs text-fg-3">No activity yet.</p>;
  return (
    <ul aria-label="Recent activity" className="max-h-56 overflow-y-auto text-xs divide-y divide-line border border-line rounded">
      {rows.map((r, i) => (
        <li key={`${r.at}-${i}`} className="px-2 py-1 flex flex-wrap gap-x-3">
          <span className="text-fg-3 tabular-nums">{when(r.at)}</span>
          <span className="mono text-fg-2">{r.tool}</span>
          <span className="text-fg">{outcomeText(r.outcome)}</span>
        </li>
      ))}
    </ul>
  );
}

function RevokeConfirm({
  pairing,
  busy,
  onConfirm,
  onCancel,
}: {
  pairing: AgentPairing;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const returnFocus = useReturnFocus(true);
  const cancelRef = useRef<HTMLButtonElement>(null);
  return (
    <Dialog.Root open onOpenChange={(o) => { if (!o) onCancel(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-[60] bg-black/45 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed top-1/2 left-1/2 z-[61] -translate-x-1/2 -translate-y-1/2 w-[min(440px,95vw)] bg-panel border border-line rounded-md shadow-sheet outline-none"
          // A destructive confirm starts on Cancel, so Enter does not revoke.
          onOpenAutoFocus={(e) => {
            e.preventDefault();
            cancelRef.current?.focus();
          }}
          onCloseAutoFocus={returnFocus}
        >
          <div className="px-4 py-3 border-b border-line">
            <Dialog.Title className="font-semibold text-md">Revoke {pairing.name}?</Dialog.Title>
          </div>
          <Dialog.Description className="px-4 py-3.5 text-sm text-fg-2">
            {pairing.name} will stop working at once and cannot be re-enabled. You can pair it again
            later as a new assistant.
          </Dialog.Description>
          <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
            <button ref={cancelRef} type="button" onClick={onCancel} className={BUTTON}>
              Cancel
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={onConfirm}
              className="h-8 px-3 rounded bg-warn text-warn-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60"
            >
              Revoke
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
