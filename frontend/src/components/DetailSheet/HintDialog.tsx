import * as Dialog from "@radix-ui/react-dialog";
import { useReturnFocus } from "./useReturnFocus";
import { AlertTriangle, Sparkles, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { Job } from "@/lib/types";

interface HintDialogProps {
  open: boolean;
  /** What the research request asks for — drives the title and placeholder.
   *  `metadata+cover` is the combined "Suggest fixes (+ cover)" path. */
  mode: "metadata" | "cover" | "metadata+cover";
  /** Album title (for context in the dialog header). */
  albumTitle: string;
  /** Pre-fill the textarea (used by "Edit hint & retry" after a failure). */
  initialHint?: string;
  /** Prior cover lookups for this album that didn't yield a cover. Surfaced
   *  in the dialog when mode includes a cover lookup, so the user can see at
   *  the moment of confirmation whether they're about to re-request something
   *  we've already tried. Empty/omitted = no prior attempts to report. */
  priorCoverAttempts?: Job[];
  /** The check id this dialog was opened for, when launched from a finding's
   *  Fix button. Drives the placeholder copy (different checks call for
   *  different hint examples) and the resolver-first callout (e.g.
   *  unidentified-album tries MusicBrainz first before anything goes to the
   *  assistant). Omitted when the dialog is opened from the DetailSheet footer. */
  checkId?: string | null;
  /** Called with the trimmed hint and `attachCover`, always false: a
   *  research request's context carries no image (`WorkContext` has no
   *  cover and the server ignores the flag), so the old "Include album
   *  cover" toggle would promise something nothing does. */
  onSubmit: (hint: string, attachCover: boolean) => void;
  onCancel: () => void;
}

/** Per-check placeholder copy. Falls back to the mode-level default when a
 *  check isn't listed (or when no checkId was passed at all — i.e. opened
 *  from the DetailSheet footer). */
const PLACEHOLDERS_BY_CHECK: Record<string, string> = {
  "unidentified-album":
    "e.g. UK pressing, 1979 original (not the 2012 reissue). Released as 'X' in Japan.",
  "tags-out-of-sync":
    "e.g. This is the soundtrack to the 1979 film, not the band of the same name.",
  "track-gaps":
    "e.g. The CD has a hidden bonus track at position 12.5; track 7 is misnumbered.",
  "no-cover-art":
    "e.g. Use the original 1979 sleeve, not the 2012 reissue artwork.",
};

const PLACEHOLDERS: Record<HintDialogProps["mode"], string> = {
  metadata:
    "e.g. This is a 2012 reissue, original was 1979. Track 7 is mislabeled — it's really 'Comfortably Numb'. The album was released under a different name in the UK.",
  cover:
    "e.g. Use the original 1979 cover, not the 2011 remaster artwork. Skip the live version cover.",
  "metadata+cover":
    "e.g. This is the 1991 Time Life 'Solid Gold Soul 1957' compilation; use that cover, not the original-album artwork.",
};

const TITLES: Record<HintDialogProps["mode"], string> = {
  metadata: "Suggest metadata fixes",
  cover: "Find cover art",
  "metadata+cover": "Suggest fixes + find cover art",
};

const SUBTITLES: Record<HintDialogProps["mode"], string> = {
  metadata:
    "Add any context that might help — release year, edition, mislabeled tracks, anything you notice. Optional.",
  cover:
    "Add any context about which cover you want — original vs reissue, regional variant, etc. Optional.",
  "metadata+cover":
    "Add any context that might help identify the album and its cover — release year, edition, compilation series, regional variant. Optional.",
};

/** Checks where a submit tries the resolver (MusicBrainz) first and sends
 *  only what it can't settle to the assistant. The dialog surfaces this so
 *  the user can predict what their click will do. */
const HAS_MB_DIRECT_PATH = new Set<string>([
  "unidentified-album",
  "tags-out-of-sync",
]);

/**
 * Dialog for adding free-form context to a research request. Shown before
 * a `metadata` or `cover` request is made (POST /api/ai/jobs).
 * The user can submit empty (no hint), supply a few words, or paste a
 * paragraph; the result rides along on the job's `hint` field.
 */
export default function HintDialog({
  open,
  mode,
  albumTitle,
  initialHint = "",
  priorCoverAttempts = [],
  checkId = null,
  onSubmit,
  onCancel,
}: HintDialogProps) {
  const [hint, setHint] = useState(initialHint);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Only show prior-attempts context when the upcoming call will actually do
  // a cover lookup. (Pure-metadata jobs aren't gated by cover history.)
  const showPriorAttempts =
    (mode === "cover" || mode === "metadata+cover") && priorCoverAttempts.length > 0;

  // Per-check placeholder when available, mode-level default otherwise.
  const placeholder =
    (checkId && PLACEHOLDERS_BY_CHECK[checkId]) ?? PLACEHOLDERS[mode];

  // Whether a submit tries the resolver first before anything goes to the
  // assistant. Surfaced as inline copy so the user can
  // predict what their click does.
  const hasMbDirect = checkId !== null && HAS_MB_DIRECT_PATH.has(checkId);

  // Reset / pre-fill hint when the dialog opens; auto-focus textarea.
  useEffect(() => {
    if (open) {
      setHint(initialHint);
      // Defer focus until after Radix's portal mount + animation.
      setTimeout(() => {
        textareaRef.current?.focus();
        textareaRef.current?.select();  // make it easy to overwrite the pre-filled hint
      }, 60);
    }
  }, [open, mode, initialHint]);

  const handleSubmit = () => onSubmit(hint.trim(), false);

  const returnFocus = useReturnFocus(open);

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o) onCancel(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-[60] bg-black/45 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed top-1/2 left-1/2 z-[61] -translate-x-1/2 -translate-y-1/2 w-[min(560px,95vw)] bg-panel border border-line rounded-lg shadow-popup outline-none flex flex-col"
          aria-describedby={undefined}
          onCloseAutoFocus={returnFocus}
          onKeyDown={(e) => {
            // Cmd/Ctrl + Enter to submit
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
              e.preventDefault();
              handleSubmit();
            }
          }}
        >
          <div className="flex items-center justify-between px-4 h-11 border-b border-line">
            <div className="flex items-center gap-2">
              <Sparkles className="w-3.5 h-3.5 text-accent" />
              <Dialog.Title className="font-semibold text-md">{TITLES[mode]}</Dialog.Title>
            </div>
            <Dialog.Close asChild>
              <button
                className="w-8 h-8 grid place-items-center text-fg-3 hover:text-fg hover:bg-bg-3 rounded transition"
                aria-label="Close"
              >
                <X className="w-4 h-4" />
              </button>
            </Dialog.Close>
          </div>

          <div className="px-4 py-3.5">
            <div className="text-sm text-fg-3 mb-2.5">
              <span className="text-fg-2">{SUBTITLES[mode]}</span>
              {albumTitle && (
                <div className="mono text-xs text-fg-4 mt-1.5 truncate">
                  for: {albumTitle}
                </div>
              )}
            </div>
            {showPriorAttempts && (
              <PriorAttemptsBanner attempts={priorCoverAttempts} />
            )}
            {hasMbDirect && (
              <div className="mb-3 flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-bg-2 border border-line rounded px-3 py-2">
                <span className="mono uppercase tracking-[0.06em] text-fg-4 shrink-0 pt-0.5">
                  Free first:
                </span>
                <span>
                  The resolver is tried first. Anything it can&apos;t settle goes
                  to your assistant.
                </span>
              </div>
            )}
            <label className="mono text-xs uppercase tracking-[0.06em] text-fg-4 block mb-1.5">
              Your hint
            </label>
            <textarea
              ref={textareaRef}
              value={hint}
              onChange={(e) => setHint(e.target.value)}
              placeholder={placeholder}
              rows={5}
              className="w-full bg-bg border border-line rounded-md px-3 py-2 text-sm placeholder:text-fg-4 focus:outline-none focus:border-accent focus:shadow-[0_0_0_2px_var(--accent-soft)] resize-none transition"
              maxLength={2000}
            />
            <div className="flex justify-between items-center mt-1.5">
              <span className="text-xs text-fg-4">
                {hint.length > 0 && (
                  <span className="mono tabular-nums">{hint.length} / 2000</span>
                )}
              </span>
              <span className="text-xs text-fg-4">
                <kbd className="mono text-[10.5px] border border-line bg-bg-2 px-1 rounded-sm">⌘ ↵</kbd> to submit
              </span>
            </div>
          </div>

          <div className="flex items-center justify-end gap-2 px-4 h-12 border-t border-line bg-panel-2">
            <button
              onClick={onCancel}
              className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition"
            >
              Cancel
            </button>
            <button
              onClick={handleSubmit}
              className="h-8 px-3 rounded bg-accent text-accent-fg font-medium text-sm hover:opacity-90 transition inline-flex items-center gap-1.5"
            >
              <Sparkles className="w-3.5 h-3.5" />
              {mode === "cover"
                ? "Find cover art"
                : mode === "metadata+cover"
                ? "Suggest fixes + cover"
                : "Suggest fixes"}
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

/**
 * Yellow banner at the top of the hint dialog showing prior cover lookups
 * for this album that didn't yield a cover. The dialog still lets the user
 * proceed — a hint they add now might unstick a previously-stuck case — but
 * they see at confirmation time that they've been here before. Lists up to
 * the 3 most recent attempts (newest first); collapses the rest to a count.
 */
function PriorAttemptsBanner({ attempts }: { attempts: Job[] }) {
  const recent = attempts.slice(0, 3);
  const extra = attempts.length - recent.length;
  return (
    <div className="border border-warn/40 bg-warn-soft rounded-md px-3 py-2 mb-3 text-xs">
      <div className="flex items-center gap-1.5 font-medium text-fg mb-1.5">
        <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0" />
        Previously tried {attempts.length} time{attempts.length === 1 ? "" : "s"} without finding a cover
      </div>
      <ul className="space-y-1 text-fg-2">
        {recent.map((j) => (
          <li key={j.id} className="flex items-baseline gap-2">
            <span className="mono text-fg-4 tabular-nums shrink-0">
              {fmtRelativeShort(j.completed_at || j.created_at)}
            </span>
            <span className="truncate">{describeAttempt(j)}</span>
          </li>
        ))}
        {extra > 0 && (
          <li className="text-fg-4 italic">…and {extra} more earlier</li>
        )}
      </ul>
      <p className="text-fg-3 mt-2 leading-relaxed">
        Add a hint that might help this time (release year, regional variant,
        etc.) — or click Cancel if you didn't mean to retry.
      </p>
    </div>
  );
}

function describeAttempt(j: Job): string {
  if (j.status === "failed") {
    // Compress wordy backend errors to something glanceable.
    const e = (j.error || "").trim();
    if (!e) return "Failed (no detail)";
    if (e.length > 90) return "Failed";
    return `Failed: ${e}`;
  }
  // acted_on with no cover_url — no cover was found and the user dismissed
  // the result.
  const note = j.result?.cover?.notes?.trim() || "Couldn't identify a cover";
  return note.length > 90 ? note.slice(0, 89) + "…" : note;
}

function fmtRelativeShort(iso: string | null): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "—";
  const diffSec = (Date.now() - t) / 1000;
  if (diffSec < 60) return "just now";
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h`;
  if (diffSec < 86400 * 30) return `${Math.floor(diffSec / 86400)}d`;
  return `${Math.floor(diffSec / (86400 * 30))}mo`;
}
