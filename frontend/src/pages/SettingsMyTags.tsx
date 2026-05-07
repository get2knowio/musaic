import { Edit3, GitMerge, Plus, Tag as TagIcon, Trash2 } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";

import {
  addVocabularyEntry,
  deleteVocabularyEntry,
  mergeVocabularyEntry,
  patchVocabularyEntry,
  promoteOrphanTag,
  renameVocabularyEntry,
} from "@/lib/api";
import { useVocabulary } from "@/hooks/useVocabulary";
import type { VocabularyEntry, VocabularyResponse } from "@/lib/types";

/**
 * Settings panel for the user's curated tag vocabulary.
 *
 * The vocabulary is the approved list of tags that drives the picker
 * UI in the album / track tag editors. Free-typing isn't allowed in
 * the editors any more; everything goes through this list to keep
 * playlist queries from being silently broken by typos and case
 * drift ("Cocktail Hour" vs "cocktail hour").
 *
 * Two subsections:
 *   - **Approved tags** — the vocabulary, with usage counts. Inline
 *     rename, merge-into, delete with strip-or-leave choice.
 *   - **Orphan tags** — tags found in the library that aren't in the
 *     vocabulary yet. Surfaces from the legacy free-tagging period
 *     and from sidecar imports. Each one offers Promote (add to
 *     vocabulary as-is) or Rename-to-existing (merge into an
 *     existing entry).
 */
export default function MyTagsSettingsPanel() {
  const vocab = useVocabulary();
  const qc = useQueryClient();

  const onUpdated = (next: VocabularyResponse) => {
    qc.setQueryData(["vocabulary"], next);
    qc.invalidateQueries({ queryKey: ["album-tags"] });
    qc.invalidateQueries({ queryKey: ["all-tags"] });
  };

  const add = useMutation({
    mutationFn: ({ tag, description }: { tag: string; description?: string }) =>
      addVocabularyEntry({ tag, description }),
    onSuccess: onUpdated,
    onError: (e: Error) => toast.error(e.message || "Couldn't add tag"),
  });

  const patchEntry = useMutation({
    mutationFn: ({ tag, description }: { tag: string; description: string }) =>
      patchVocabularyEntry(tag, { description }),
    onSuccess: onUpdated,
    onError: (e: Error) => toast.error(e.message || "Couldn't update tag"),
  });

  const rename = useMutation({
    mutationFn: ({ tag, newTag }: { tag: string; newTag: string }) =>
      renameVocabularyEntry(tag, newTag),
    onSuccess: (next) => {
      onUpdated(next);
      toast.success("Renamed across library");
    },
    onError: (e: Error & { status?: number }) => {
      if (e.status === 409) {
        toast.error("That tag already exists — use Merge instead.");
      } else {
        toast.error(e.message || "Couldn't rename tag");
      }
    },
  });

  const merge = useMutation({
    mutationFn: ({ tag, intoTag }: { tag: string; intoTag: string }) =>
      mergeVocabularyEntry(tag, intoTag),
    onSuccess: (next) => {
      onUpdated(next);
      toast.success("Merged");
    },
    onError: (e: Error) => toast.error(e.message || "Couldn't merge"),
  });

  const removeEntry = useMutation({
    mutationFn: ({ tag, strip }: { tag: string; strip: boolean }) =>
      deleteVocabularyEntry(tag, { stripFromTracks: strip }),
    onSuccess: (next, vars) => {
      onUpdated(next);
      toast.success(vars.strip ? "Removed and stripped from tracks" : "Removed (tracks kept)");
    },
    onError: (e: Error) => toast.error(e.message || "Couldn't delete tag"),
  });

  const promote = useMutation({
    mutationFn: ({ tag }: { tag: string }) => promoteOrphanTag(tag),
    onSuccess: (next) => {
      onUpdated(next);
      toast.success("Added to vocabulary");
    },
    onError: (e: Error) => toast.error(e.message || "Couldn't promote tag"),
  });

  // Add-tag input state.
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  const submitAdd = () => {
    const tag = draft.trim();
    if (!tag) return;
    add.mutate(
      { tag },
      {
        onSuccess: (next) => {
          onUpdated(next);
          setDraft("");
          inputRef.current?.focus();
        },
      },
    );
  };

  const entries = vocab.data?.vocabulary ?? [];
  const orphans = vocab.data?.orphans ?? [];

  return (
    <section className="border border-line rounded-md bg-panel-2 overflow-hidden mb-6">
      <header className="flex items-center gap-2 px-4 h-10 border-b border-line bg-panel">
        <TagIcon className="w-3.5 h-3.5 text-accent" />
        <h2 className="text-md font-semibold">My Tags</h2>
      </header>

      <div className="px-4 py-4 space-y-5">
        <p className="text-sm text-fg-3 leading-relaxed">
          Your curated taxonomy for shuffling and Plex playlists ("Cocktail
          Hour", "Christmas", "Instrumental"). The album and track tag
          editors only let you pick from this list — free-typing is
          off-limits so a typo can't quietly drop tracks from a playlist.
          Tags are stored separately from FLAC GENRE and never written to
          your audio files.
        </p>

        {/* Add new entry */}
        <div className="flex items-center gap-2">
          <input
            ref={inputRef}
            type="text"
            placeholder="New tag (e.g. Cocktail Hour)"
            value={draft}
            disabled={add.isPending}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") submitAdd(); }}
            className="flex-1 max-w-xs h-8 px-2.5 border border-line rounded bg-bg text-sm text-fg focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30 disabled:opacity-60"
          />
          <button
            onClick={submitAdd}
            disabled={!draft.trim() || add.isPending}
            className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
          >
            <Plus className="w-3.5 h-3.5" />
            Add tag
          </button>
        </div>

        {/* Approved list */}
        <div className="border-t border-line pt-4">
          <div className="text-xs mono uppercase tracking-[0.06em] text-fg-4 mb-2">
            Approved tags ({entries.length})
          </div>
          {entries.length === 0 ? (
            <div className="text-sm text-fg-4 italic py-1">
              No tags yet. Add one above to start tagging tracks.
            </div>
          ) : (
            <ul className="divide-y divide-line border border-line rounded">
              {entries.map((entry) => (
                <VocabularyRow
                  key={entry.tag}
                  entry={entry}
                  allEntries={entries}
                  onPatch={(description) =>
                    patchEntry.mutate({ tag: entry.tag, description })
                  }
                  onRename={(newTag) => rename.mutate({ tag: entry.tag, newTag })}
                  onMerge={(intoTag) =>
                    merge.mutate({ tag: entry.tag, intoTag })
                  }
                  onDelete={(strip) =>
                    removeEntry.mutate({ tag: entry.tag, strip })
                  }
                />
              ))}
            </ul>
          )}
        </div>

        {/* Orphans */}
        {orphans.length > 0 && (
          <div className="border-t border-line pt-4">
            <div className="text-xs mono uppercase tracking-[0.06em] text-fg-4 mb-2">
              Unapproved tags found in library ({orphans.length})
            </div>
            <p className="text-xs text-fg-3 mb-2 leading-relaxed">
              These tags appear on tracks but aren't in your vocabulary.
              Promote to add them as-is, or use the rename action on an
              approved entry above to merge them in.
            </p>
            <ul className="divide-y divide-line border border-line rounded">
              {orphans.map((o) => (
                <li
                  key={o.tag}
                  className="flex items-center gap-2 px-3 py-2"
                >
                  <span className="mono text-sm text-fg-2 truncate flex-1">
                    {o.tag}
                  </span>
                  <span className="mono text-xs text-fg-4 tabular-nums">
                    {o.count} {o.count === 1 ? "track" : "tracks"}
                  </span>
                  <button
                    onClick={() => promote.mutate({ tag: o.tag })}
                    disabled={promote.isPending}
                    className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1 disabled:opacity-60"
                  >
                    <Plus className="w-3 h-3" />
                    Promote
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </section>
  );
}

function VocabularyRow({
  entry,
  allEntries,
  onPatch,
  onRename,
  onMerge,
  onDelete,
}: {
  entry: VocabularyEntry;
  allEntries: VocabularyEntry[];
  onPatch: (description: string) => void;
  onRename: (newTag: string) => void;
  onMerge: (intoTag: string) => void;
  onDelete: (strip: boolean) => void;
}) {
  const [mode, setMode] = useState<"view" | "rename" | "merge" | "delete">("view");
  const [draft, setDraft] = useState("");
  const [descDraft, setDescDraft] = useState(entry.description);
  const otherEntries = useMemo(
    () => allEntries.filter((e) => e.tag.toLowerCase() !== entry.tag.toLowerCase()),
    [allEntries, entry.tag],
  );

  const enterRename = () => {
    setMode("rename");
    setDraft(entry.tag);
  };
  const enterMerge = () => {
    setMode("merge");
    setDraft(otherEntries[0]?.tag ?? "");
  };
  const enterDelete = () => setMode("delete");
  const cancel = () => {
    setMode("view");
    setDraft("");
    setDescDraft(entry.description);
  };

  return (
    <li className="px-3 py-2.5">
      {mode === "view" && (
        <div className="flex items-start gap-3">
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2">
              <span className="mono text-sm text-fg font-medium truncate">
                {entry.tag}
              </span>
              <span className="mono text-xs text-fg-4 tabular-nums shrink-0">
                · {entry.count} {entry.count === 1 ? "track" : "tracks"}
              </span>
            </div>
            <input
              type="text"
              placeholder="Add description (optional)"
              value={descDraft}
              onChange={(e) => setDescDraft(e.target.value)}
              onBlur={() => {
                if (descDraft !== entry.description) onPatch(descDraft);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") (e.target as HTMLInputElement).blur();
              }}
              className="mt-1 w-full h-7 px-2 text-xs bg-transparent border-0 border-b border-transparent focus:border-line focus:outline-none text-fg-3 placeholder:text-fg-4"
            />
          </div>
          <div className="flex items-center gap-1 shrink-0">
            <ActionBtn onClick={enterRename} title="Rename across library">
              <Edit3 className="w-3 h-3" />
              Rename
            </ActionBtn>
            <ActionBtn
              onClick={enterMerge}
              title="Merge into another tag"
              disabled={otherEntries.length === 0}
            >
              <GitMerge className="w-3 h-3" />
              Merge
            </ActionBtn>
            <ActionBtn onClick={enterDelete} title="Delete from vocabulary" danger>
              <Trash2 className="w-3 h-3" />
              Delete
            </ActionBtn>
          </div>
        </div>
      )}

      {mode === "rename" && (
        <InlineEdit
          label="Rename to"
          help="Updates every track in your library that carries this tag."
          value={draft}
          onChange={setDraft}
          onSubmit={() => {
            const next = draft.trim();
            if (!next || next.toLowerCase() === entry.tag.toLowerCase()) {
              cancel();
              return;
            }
            onRename(next);
            cancel();
          }}
          onCancel={cancel}
        />
      )}

      {mode === "merge" && (
        <div className="space-y-1.5">
          <div className="text-xs text-fg-3">
            Merge <strong>{entry.tag}</strong> into:
          </div>
          <div className="flex items-center gap-2">
            <select
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              className="h-8 px-2 border border-line rounded bg-bg text-sm text-fg focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30"
            >
              {otherEntries.map((e) => (
                <option key={e.tag} value={e.tag}>{e.tag}</option>
              ))}
            </select>
            <button
              onClick={() => {
                if (!draft) return;
                onMerge(draft);
                cancel();
              }}
              disabled={!draft}
              className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60"
            >
              Merge
            </button>
            <button
              onClick={cancel}
              className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition"
            >
              Cancel
            </button>
          </div>
          <div className="text-xs text-fg-4 leading-relaxed">
            Every track tagged <strong>{entry.tag}</strong> will be retagged with the destination, and the entry is removed from the vocabulary.
          </div>
        </div>
      )}

      {mode === "delete" && (
        <div className="space-y-2">
          <div className="text-sm text-fg-2">
            Delete <strong>{entry.tag}</strong> from your vocabulary?
          </div>
          <div className="text-xs text-fg-3 leading-relaxed">
            {entry.count > 0
              ? "Tracks currently carrying this tag can keep it (it'll show up as an orphan you can re-promote later) or be stripped now."
              : "No tracks are using it, so nothing else changes."}
          </div>
          <div className="flex items-center gap-2 flex-wrap">
            <button
              onClick={() => { onDelete(false); cancel(); }}
              className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition"
            >
              Delete (keep tracks tagged)
            </button>
            {entry.count > 0 && (
              <button
                onClick={() => { onDelete(true); cancel(); }}
                className="h-8 px-3 border border-err/40 rounded text-sm text-err hover:bg-err-soft transition"
              >
                Delete + strip from {entry.count} tracks
              </button>
            )}
            <button
              onClick={cancel}
              className="h-8 px-3 text-sm text-fg-3 hover:text-fg transition"
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </li>
  );
}

function InlineEdit({
  label,
  help,
  value,
  onChange,
  onSubmit,
  onCancel,
}: {
  label: string;
  help: string;
  value: string;
  onChange: (s: string) => void;
  onSubmit: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="space-y-1.5">
      <div className="text-xs text-fg-3">{label}:</div>
      <div className="flex items-center gap-2">
        <input
          type="text"
          autoFocus
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSubmit();
            else if (e.key === "Escape") onCancel();
          }}
          className="flex-1 max-w-xs h-8 px-2 border border-line rounded bg-bg text-sm text-fg focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30"
        />
        <button
          onClick={onSubmit}
          className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition"
        >
          Save
        </button>
        <button
          onClick={onCancel}
          className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition"
        >
          Cancel
        </button>
      </div>
      <div className="text-xs text-fg-4 leading-relaxed">{help}</div>
    </div>
  );
}

function ActionBtn({
  children,
  onClick,
  disabled,
  title,
  danger,
}: {
  children: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
  title?: string;
  danger?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      title={title}
      className={
        "h-7 px-2 border border-line rounded text-xs transition inline-flex items-center gap-1 disabled:opacity-50 " +
        (danger ? "text-err hover:bg-err-soft" : "text-fg-2 hover:bg-bg-3 hover:text-fg")
      }
    >
      {children}
    </button>
  );
}
