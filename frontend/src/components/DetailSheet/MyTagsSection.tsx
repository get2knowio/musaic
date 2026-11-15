import { Plus, Settings as SettingsIcon, Tag as TagIcon, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";

import {
  addAlbumTag,
  addTrackTag,
  removeAlbumTag,
  removeTrackTag,
} from "@/lib/api";
import type { AlbumTagsResponse, Track, VocabularyEntry } from "@/lib/types";
import { useAlbumTags, useVocabulary } from "@/hooks/useVocabulary";
import Section from "./Section";

interface MyTagsSectionProps {
  albumId: string;
  /** Tracks come from useTracks() in the parent so the per-track editor
   *  always knows the full filename list (even tracks with no tags). */
  tracks: Track[];
}

/**
 * "My Tags" — the user's curated tagging taxonomy ("Cocktail Hour",
 * "Christmas", "Instrumental"…). Distinct from the FLAC GENRE field.
 *
 * Tags are picked from the vocabulary defined in Settings → My Tags.
 * Free-typing isn't allowed here so a typo can't quietly drop tracks
 * from a Plex playlist query later.
 *
 * Default view: album-level chip strip + an "Add tag" picker. The
 * "Edit per track" toggle expands a per-track grid where the user can
 * apply different tags to different tracks (e.g. one "Christmas"
 * track on an otherwise non-Christmas album).
 */
export default function MyTagsSection({ albumId, tracks }: MyTagsSectionProps) {
  const qc = useQueryClient();
  const tagsQuery = useAlbumTags(albumId);
  const vocabQuery = useVocabulary();

  const onMutated = (next: AlbumTagsResponse) => {
    qc.setQueryData(["album-tags", albumId], next);
    // The vocabulary's count column drives the Settings panel — refresh.
    qc.invalidateQueries({ queryKey: ["vocabulary"] });
    qc.invalidateQueries({ queryKey: ["all-tags"] });
  };

  const addAlbum = useMutation({
    mutationFn: ({ tag }: { tag: string }) => addAlbumTag(albumId, tag),
    onSuccess: onMutated,
    onError: (e: Error) => toast.error(e.message || "Couldn't add tag"),
  });
  const removeAlbum = useMutation({
    mutationFn: ({ tag }: { tag: string }) => removeAlbumTag(albumId, tag),
    onSuccess: onMutated,
    onError: (e: Error) => toast.error(e.message || "Couldn't remove tag"),
  });
  const addTrack = useMutation({
    mutationFn: ({ filename, tag }: { filename: string; tag: string }) =>
      addTrackTag(albumId, filename, tag),
    onSuccess: onMutated,
    onError: (e: Error) => toast.error(e.message || "Couldn't add tag"),
  });
  const removeTrack = useMutation({
    mutationFn: ({ filename, tag }: { filename: string; tag: string }) =>
      removeTrackTag(albumId, filename, tag),
    onSuccess: onMutated,
    onError: (e: Error) => toast.error(e.message || "Couldn't remove tag"),
  });

  const data = tagsQuery.data;
  const albumTags = data?.album_tags ?? [];
  const trackTags = data?.track_tags ?? {};
  const vocabulary = vocabQuery.data?.vocabulary ?? [];

  // Tags applied to *some but not all* tracks (the partial set). Surfaced
  // separately from album-level chips so the user can tell at a glance
  // that the chip won't propagate to playlist queries that look at the
  // album-level intersection.
  const partialTags = useMemo(() => {
    const counts = new Map<string, number>();
    for (const tags of Object.values(trackTags)) {
      for (const t of tags) counts.set(t, (counts.get(t) ?? 0) + 1);
    }
    const albumSet = new Set(albumTags.map((t) => t.toLowerCase()));
    const total = Math.max(1, tracks.length || Object.keys(trackTags).length);
    const out: { tag: string; count: number }[] = [];
    for (const [tag, n] of counts.entries()) {
      if (!albumSet.has(tag.toLowerCase()) && n < total) {
        out.push({ tag, count: n });
      }
    }
    out.sort((a, b) => a.tag.localeCompare(b.tag));
    return out;
  }, [trackTags, albumTags, tracks.length]);

  const [showPerTrack, setShowPerTrack] = useState(false);

  const meta = useMemo(() => {
    const n = albumTags.length;
    if (n === 0 && partialTags.length === 0) return null;
    return (
      <span className="mono text-xs text-fg-4">
        {n} album-wide{partialTags.length > 0 ? ` · ${partialTags.length} partial` : ""}
      </span>
    );
  }, [albumTags, partialTags]);

  // Empty-vocabulary state: no tags to pick from. Direct the user to
  // Settings rather than letting them hit a "+" that does nothing.
  const vocabularyEmpty = vocabQuery.isSuccess && vocabulary.length === 0;

  return (
    <Section id="my-tags" title="My Tags" meta={meta}>
      <div className="px-4 py-3 space-y-2.5">
        {/* Album-level chips */}
        <div className="flex flex-wrap items-center gap-1.5">
          {albumTags.length === 0 && partialTags.length === 0 && !vocabularyEmpty && (
            <span className="text-xs text-fg-4 italic mr-1">
              No tags yet. Add one to start building playlists.
            </span>
          )}
          {albumTags.map((t) => (
            <TagChip
              key={t}
              tag={t}
              onRemove={() => removeAlbum.mutate({ tag: t })}
              tone="album"
            />
          ))}
          {partialTags.map(({ tag, count }) => (
            <TagChip
              key={"partial:" + tag}
              tag={tag}
              count={count}
              onRemove={() => removeAlbum.mutate({ tag })}
              tone="partial"
              title={`Applied to ${count} of ${tracks.length || Object.keys(trackTags).length} tracks. Remove to strip from all.`}
            />
          ))}
          {!vocabularyEmpty && (
            <TagPicker
              vocabulary={vocabulary}
              alreadyApplied={new Set(albumTags.map((t) => t.toLowerCase()))}
              onPick={(tag) => addAlbum.mutate({ tag })}
              ariaLabel="Add tag to all tracks on this album"
            />
          )}
        </div>

        {vocabularyEmpty && (
          <div className="text-xs text-fg-3 leading-relaxed bg-bg-3 border border-line rounded px-3 py-2">
            <div className="flex items-start gap-2">
              <SettingsIcon className="w-3.5 h-3.5 mt-0.5 shrink-0 text-fg-4" />
              <div>
                You haven't defined any tags yet. Add a few in{" "}
                <Link to="/settings" className="text-accent hover:underline">
                  Settings → My Tags
                </Link>
                {" "}— that approved list is what the picker draws from.
              </div>
            </div>
          </div>
        )}

        {/* Per-track editor toggle */}
        {tracks.length > 0 && !vocabularyEmpty && (
          <div>
            <button
              onClick={() => setShowPerTrack((v) => !v)}
              className="text-xs text-fg-3 hover:text-fg underline-offset-2 hover:underline"
            >
              {showPerTrack ? "Hide per-track tags" : "Edit per track…"}
            </button>
          </div>
        )}

        {showPerTrack && !vocabularyEmpty && (
          <PerTrackEditor
            tracks={tracks}
            trackTags={trackTags}
            vocabulary={vocabulary}
            onAdd={(filename, tag) => addTrack.mutate({ filename, tag })}
            onRemove={(filename, tag) => removeTrack.mutate({ filename, tag })}
          />
        )}
      </div>
    </Section>
  );
}

function TagChip({
  tag,
  count,
  onRemove,
  tone,
  title,
}: {
  tag: string;
  count?: number;
  onRemove: () => void;
  tone: "album" | "partial" | "track";
  title?: string;
}) {
  const cls =
    tone === "album"
      ? "bg-accent/10 text-accent border-accent/30"
      : tone === "partial"
      ? "bg-bg-3 text-fg-2 border border-line border-dashed"
      : "bg-bg-3 text-fg-2 border border-line";
  return (
    <span
      className={`mono text-[11px] inline-flex items-center gap-1 pl-2 pr-1 py-0.5 rounded-full ${cls}`}
      title={title}
    >
      <TagIcon className="w-2.5 h-2.5 opacity-70" />
      <span>{tag}</span>
      {count !== undefined && (
        <span className="mono text-[10px] text-fg-4 ml-0.5">×{count}</span>
      )}
      <button
        onClick={onRemove}
        className="w-4 h-4 grid place-items-center rounded-full hover:bg-fg/10 ml-0.5"
        aria-label={`Remove ${tag}`}
      >
        <X className="w-2.5 h-2.5" />
      </button>
    </span>
  );
}

/**
 * Vocabulary picker: a "+" button that opens a popover of approved
 * tags. Items already applied are dimmed and unclickable. Search
 * input filters the list. Keyboard: Esc closes; Enter applies the
 * top match.
 *
 * Compact = small chevron-style trigger used inside per-track rows.
 */
function TagPicker({
  vocabulary,
  alreadyApplied,
  onPick,
  ariaLabel,
  compact,
}: {
  vocabulary: VocabularyEntry[];
  alreadyApplied: Set<string>;
  onPick: (tag: string) => void;
  ariaLabel: string;
  compact?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const wrapRef = useRef<HTMLDivElement>(null);

  // Close on outside click / Esc.
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    // Capture phase + preventDefault: this Esc closes the picker only, not
    // the album panel around it (which ignores an Esc already handled).
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return vocabulary.filter((e) => !q || e.tag.toLowerCase().includes(q));
  }, [vocabulary, query]);

  const pick = (tag: string) => {
    onPick(tag);
    setOpen(false);
    setQuery("");
  };

  return (
    <div className="relative inline-block" ref={wrapRef}>
      <button
        onClick={() => setOpen((v) => !v)}
        aria-label={ariaLabel}
        className={
          compact
            ? "mono text-[11px] inline-flex items-center gap-1 pl-1.5 pr-2 py-0.5 rounded-full border border-line border-dashed text-fg-3 hover:bg-bg-3 hover:text-fg transition"
            : "mono text-[11px] inline-flex items-center gap-1 pl-1.5 pr-2 py-0.5 rounded-full border border-line border-dashed text-fg-3 hover:bg-bg-3 hover:text-fg transition"
        }
      >
        <Plus className="w-2.5 h-2.5" />
        <span>{compact ? "tag" : "Add tag"}</span>
      </button>
      {open && (
        <div className="absolute z-20 left-0 top-full mt-1 w-56 bg-panel border border-line rounded shadow-popover overflow-hidden">
          <input
            type="text"
            placeholder="Search tags…"
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                const first = filtered.find((e) => !alreadyApplied.has(e.tag.toLowerCase()));
                if (first) pick(first.tag);
              }
            }}
            className="w-full h-8 px-2.5 text-sm bg-bg-2 border-0 border-b border-line focus:outline-none placeholder:text-fg-4"
          />
          <div className="max-h-56 overflow-auto py-1">
            {filtered.length === 0 ? (
              <div className="px-3 py-2 text-xs text-fg-4">No matches.</div>
            ) : (
              filtered.map((entry) => {
                const applied = alreadyApplied.has(entry.tag.toLowerCase());
                return (
                  <button
                    key={entry.tag}
                    onClick={() => !applied && pick(entry.tag)}
                    disabled={applied}
                    className={`w-full text-left px-2.5 py-1.5 text-sm flex items-center justify-between gap-2 ${
                      applied
                        ? "text-fg-4 cursor-not-allowed"
                        : "text-fg-2 hover:bg-bg-3 hover:text-fg"
                    }`}
                  >
                    <span className="inline-flex items-center gap-1.5 truncate">
                      <TagIcon className="w-3 h-3 text-fg-4 shrink-0" />
                      <span className="truncate">{entry.tag}</span>
                    </span>
                    <span className="mono text-[10px] text-fg-4 shrink-0">
                      {applied ? "applied" : entry.count}
                    </span>
                  </button>
                );
              })
            )}
          </div>
          <Link
            to="/settings"
            onClick={() => setOpen(false)}
            className="block border-t border-line px-2.5 py-1.5 text-[11px] text-fg-3 hover:bg-bg-3 hover:text-fg transition"
          >
            Manage vocabulary →
          </Link>
        </div>
      )}
    </div>
  );
}

function PerTrackEditor({
  tracks,
  trackTags,
  vocabulary,
  onAdd,
  onRemove,
}: {
  tracks: Track[];
  trackTags: Record<string, string[]>;
  vocabulary: VocabularyEntry[];
  onAdd: (filename: string, tag: string) => void;
  onRemove: (filename: string, tag: string) => void;
}) {
  return (
    <div className="border-t border-line pt-2.5 space-y-1.5">
      <div className="text-[10px] uppercase mono tracking-wider text-fg-4">
        Per-track tags
      </div>
      <div className="space-y-1">
        {tracks.map((t) => {
          const tags = trackTags[t.filename] ?? [];
          return (
            <div key={t.filename} className="flex items-start gap-2 text-xs py-1">
              <div className="w-8 mono text-fg-4 tabular-nums shrink-0 pt-0.5 text-right">
                {t.tracknumber || "—"}
              </div>
              <div className="flex-1 min-w-0">
                <div className="text-fg-2 truncate" title={t.title || t.filename}>
                  {t.title || t.filename}
                </div>
                <div className="mt-1 flex flex-wrap items-center gap-1">
                  {tags.map((tag) => (
                    <TagChip
                      key={tag}
                      tag={tag}
                      onRemove={() => onRemove(t.filename, tag)}
                      tone="track"
                    />
                  ))}
                  <TagPicker
                    vocabulary={vocabulary}
                    alreadyApplied={new Set(tags.map((tg) => tg.toLowerCase()))}
                    onPick={(tag) => onAdd(t.filename, tag)}
                    ariaLabel={`Add tag to ${t.title || t.filename}`}
                    compact
                  />
                </div>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
