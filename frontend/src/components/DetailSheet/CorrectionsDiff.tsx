/**
 * The one place the album panel shows proposed corrections and lets the
 * user pick which to apply: a row per field (current → proposed) with a
 * checkbox, per-track rows, and a cover row with a preview. The rows come
 * from `useCorrections` (corrections.ts), whichever source proposed them;
 * Apply and Discard are in the panel's footer.
 */
import { Loader2, Music, RotateCcw } from "lucide-react";

import { coverUrl } from "@/lib/api";
import type { CorrectionRow } from "@/lib/suggestions";
import type { Album } from "@/lib/types";
import type { CoverPreviewState } from "./corrections";

export const CORRECTIONS_ID = "album-corrections";

const ALBUM_FIELD_LABELS: Record<string, string> = {
  title: "title",
  album_artist: "album artist",
  artist: "artist",
  year: "year",
  genre: "genre",
  album_artist_sort: "album-artist sort",
};

const TRACK_FIELD_LABELS: Record<string, string> = {
  title: "title",
  artist: "artist",
  tracknumber: "#",
};

interface Props {
  album: Album;
  title: string;
  /** Right of the title: the source, or a job's confidence. */
  meta?: React.ReactNode;
  rows: CorrectionRow[];
  isSelected: (key: string) => boolean;
  isSelectable: (key: string) => boolean;
  onToggle: (key: string) => void;
  coverPreview: CoverPreviewState | null;
  onRetryCoverPreview: () => void;
  /** Shown instead of rows when the source proposed nothing. */
  emptyMessage?: string;
  /** A job's notes. */
  notes?: string | null;
  /** While an apply runs, the ticks are frozen. */
  disabled?: boolean;
}

export default function CorrectionsDiff({
  album,
  title,
  meta,
  rows,
  isSelected,
  isSelectable,
  onToggle,
  coverPreview,
  onRetryCoverPreview,
  emptyMessage,
  notes,
  disabled = false,
}: Props) {
  const albumRows = rows.filter((r) => r.kind === "album");
  const trackRows = rows.filter((r) => r.kind === "track");
  const coverRow = rows.find((r) => r.kind === "cover");
  const selectedCount = rows.filter((r) => isSelected(r.key)).length;
  const headingId = `${CORRECTIONS_ID}-heading`;

  return (
    <section
      id={CORRECTIONS_ID}
      aria-labelledby={headingId}
      className="border-b border-line bg-bg-2"
    >
      <div className="flex items-center gap-2 px-4 h-9">
        <h3
          id={headingId}
          className="mono text-[11px] uppercase tracking-[0.08em] text-fg-2 font-normal"
        >
          {title}
        </h3>
        {rows.length > 0 && (
          <span className="mono text-xs text-fg-4 tabular-nums">
            {selectedCount} of {rows.length} selected
          </span>
        )}
        {meta && <span className="ml-auto mono text-xs text-fg-4 truncate">{meta}</span>}
      </div>

      <div className="px-4 pb-3 space-y-3 text-sm">
        {rows.length === 0 && emptyMessage && (
          <p className="text-fg-3 text-sm">{emptyMessage}</p>
        )}

        {albumRows.length > 0 && (
          <div className="space-y-1.5">
            {albumRows.map((r) => (
              <DiffRow
                key={r.key}
                label={ALBUM_FIELD_LABELS[r.field] ?? r.field}
                current={r.current}
                proposed={r.proposed}
                checked={isSelected(r.key)}
                disabled={disabled}
                onToggle={() => onToggle(r.key)}
              />
            ))}
          </div>
        )}

        {trackRows.length > 0 && (
          <div className="space-y-1 pt-2 border-t border-line">
            <div className="mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4 mb-1.5">
              Per-track ({trackRows.length})
            </div>
            {trackRows.map((r) => {
              const rename =
                r.field === "tracknumber" ? predictRenameTarget(r.filename, r.proposed) : null;
              return (
                <DiffRow
                  key={r.key}
                  label={`${r.filename} · ${TRACK_FIELD_LABELS[r.field] ?? r.field}`}
                  current={r.current}
                  proposed={r.proposed}
                  note={rename && rename !== r.filename ? `renames the file to ${rename}` : undefined}
                  compact
                  checked={isSelected(r.key)}
                  disabled={disabled}
                  onToggle={() => onToggle(r.key)}
                />
              );
            })}
          </div>
        )}

        {coverRow && (
          <CoverRow
            album={album}
            preview={coverPreview}
            checked={isSelected(coverRow.key)}
            selectable={isSelectable(coverRow.key)}
            disabled={disabled}
            onToggle={() => onToggle(coverRow.key)}
            onRetry={onRetryCoverPreview}
          />
        )}

        {notes && (
          <p className="text-xs text-fg-3 leading-relaxed">
            <span className="text-fg-2">Notes:</span> {notes}
          </p>
        )}
      </div>
    </section>
  );
}

function CoverRow({
  album,
  preview,
  checked,
  selectable,
  disabled,
  onToggle,
  onRetry,
}: {
  album: Album;
  preview: CoverPreviewState | null;
  checked: boolean;
  selectable: boolean;
  disabled: boolean;
  onToggle: () => void;
  onRetry: () => void;
}) {
  const hasCover = album.has_embedded_cover || !!album.cover_file;
  const status = preview?.status ?? "loading";
  const dim = checked ? "" : "opacity-40";
  return (
    <div className="pt-2 border-t border-line" data-testid="cover-row">
      <label
        className={`grid items-center gap-2 ${selectable ? "cursor-pointer" : ""}`}
        style={{ gridTemplateColumns: "auto auto auto auto 1fr" }}
      >
        <input
          type="checkbox"
          checked={checked}
          disabled={!selectable || disabled}
          onChange={onToggle}
          className="w-3.5 h-3.5 accent-accent"
          aria-label="Apply cover correction"
          title={selectable ? undefined : "A cover can be applied once its preview has loaded."}
        />
        <span className="mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4">cover</span>
        <span className={`w-12 h-12 rounded bg-bg-3 overflow-hidden grid place-items-center text-fg-4 ${dim}`}>
          {hasCover ? (
            <img src={coverUrl(album)} alt="current cover" className="w-full h-full object-cover" />
          ) : (
            <Music className="w-4 h-4" aria-label="no cover" />
          )}
        </span>
        <span className={`text-fg-4 ${dim}`}>→</span>
        <span className="flex items-center gap-2 min-w-0">
          {status === "ok" && preview?.status === "ok" ? (
            <img
              src={preview.dataUrl}
              alt="proposed cover"
              className={`w-12 h-12 rounded object-cover bg-bg-3 shrink-0 ${dim}`}
            />
          ) : (
            <span className="w-12 h-12 rounded bg-bg-3 grid place-items-center text-fg-4 shrink-0">
              {status === "loading" ? <Loader2 className="w-4 h-4 spin" /> : <Music className="w-4 h-4" />}
            </span>
          )}
          <span className="text-xs text-fg-3 min-w-0">{coverStatusText(status)}</span>
        </span>
      </label>
      {status === "unavailable" && (
        <button
          type="button"
          onClick={onRetry}
          className="mt-1.5 ml-6 h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition inline-flex items-center gap-1"
        >
          <RotateCcw className="w-3 h-3" /> Retry preview
        </button>
      )}
    </div>
  );
}

function coverStatusText(status: string): string {
  switch (status) {
    case "ok":
      return "Proposed cover";
    case "loading":
      return "Loading preview…";
    case "none":
      return "No cover to preview — nothing to apply.";
    case "missing":
      return "The album's folder is missing; its cover cannot change.";
    default:
      return "Preview unavailable: the cover could not be fetched. It is applied only after you have seen it.";
  }
}

function DiffRow({
  label,
  current,
  proposed,
  note,
  compact = false,
  checked,
  disabled,
  onToggle,
}: {
  label: string;
  current: string;
  proposed: string;
  note?: string;
  compact?: boolean;
  checked: boolean;
  disabled: boolean;
  onToggle: () => void;
}) {
  const [curParts, newParts] = diffSegments(current, proposed);
  // Unticked rows dim their values, not the checkbox or label, so they are
  // easy to tick again.
  const dim = checked ? "" : "opacity-40";
  return (
    <label
      className={`grid items-baseline gap-2 cursor-pointer ${compact ? "text-xs" : ""}`}
      style={{ gridTemplateColumns: "auto auto 1fr auto 1fr" }}
    >
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={onToggle}
        className="w-3.5 h-3.5 accent-accent translate-y-[1px]"
        aria-label={`Apply ${label} correction`}
      />
      <span className={`mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4 truncate ${dim}`}>
        {label}
      </span>
      <span className={`text-fg-3 truncate line-through opacity-70 ${dim}`} title={diffTitle(current, proposed, "current")}>
        {current
          ? curParts.map((p, i) =>
              p.changed
                ? <span key={i} className="bg-amber-500/25 text-amber-200 no-underline rounded-sm px-0.5">{visualize(p.text)}</span>
                : <span key={i}>{p.text}</span>,
            )
          : <em className="not-italic text-fg-4">(empty)</em>}
      </span>
      <span className={`text-fg-4 ${dim}`}>→</span>
      <span className={`text-fg truncate font-medium ${dim}`} title={diffTitle(current, proposed, "proposed")}>
        {newParts.map((p, i) =>
          p.changed
            ? <span key={i} className="bg-emerald-500/25 text-emerald-200 rounded-sm px-0.5">{visualize(p.text)}</span>
            : <span key={i}>{p.text}</span>,
        )}
        {note && <span className="block text-[11px] text-fg-4 font-normal truncate">{note}</span>}
      </span>
    </label>
  );
}

interface Seg { text: string; changed: boolean }

/** Split two strings into [equal prefix, differing middle, equal suffix].
 *  Enough for short tag values, and makes subtle differences (smart quotes,
 *  dashes, NBSP, case) stand out in the highlight. */
function diffSegments(a: string, b: string): [Seg[], Seg[]] {
  const aChars = [...a];
  const bChars = [...b];
  let pre = 0;
  while (pre < aChars.length && pre < bChars.length && aChars[pre] === bChars[pre]) pre++;
  let suf = 0;
  while (
    suf < aChars.length - pre &&
    suf < bChars.length - pre &&
    aChars[aChars.length - 1 - suf] === bChars[bChars.length - 1 - suf]
  ) suf++;
  const aMid = aChars.slice(pre, aChars.length - suf).join("");
  const bMid = bChars.slice(pre, bChars.length - suf).join("");
  const prefix = aChars.slice(0, pre).join("");
  const suffix = aChars.slice(aChars.length - suf).join("");
  const aSegs: Seg[] = [];
  const bSegs: Seg[] = [];
  if (prefix) { aSegs.push({ text: prefix, changed: false }); bSegs.push({ text: prefix, changed: false }); }
  if (aMid) aSegs.push({ text: aMid, changed: true });
  if (bMid) bSegs.push({ text: bMid, changed: true });
  if (suffix) { aSegs.push({ text: suffix, changed: false }); bSegs.push({ text: suffix, changed: false }); }
  return [aSegs, bSegs];
}

/** Make invisible characters visible inside the highlight. */
function visualize(s: string): string {
  return s
    .replace(/ /g, "␣") // NBSP
    .replace(/​/g, "·") // zero-width space
    .replace(/[‎‏﻿]/g, "·"); // bidi marks, BOM
}

/** Tooltip with the code points of the differing part. */
function diffTitle(current: string, proposed: string, side: "current" | "proposed"): string {
  const [aSegs, bSegs] = diffSegments(current, proposed);
  const segs = side === "current" ? aSegs : bSegs;
  const target = side === "current" ? current : proposed;
  const changed = segs.find((s) => s.changed);
  if (!changed) return target;
  const codes = [...changed.text]
    .map((c) => `U+${c.codePointAt(0)!.toString(16).toUpperCase().padStart(4, "0")}`)
    .join(" ");
  return `${target}\n\nDiffering: "${changed.text}"  [${codes}]`;
}

/** The filename a new track number gives (the apply's rename rule: the
 *  leading number is replaced, zero-padded to two digits). */
function predictRenameTarget(filename: string, newTrackNumber: string): string | null {
  const m = filename.match(/^(\d+(?:[-.]\d+)?)([\s\-_.]+)/);
  if (!m) return null;
  const n = parseInt(newTrackNumber, 10);
  if (isNaN(n)) return null;
  return n.toString().padStart(2, "0") + filename.slice(m[1].length);
}
