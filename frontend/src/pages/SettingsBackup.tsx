import {
  AlertTriangle,
  Camera,
  Database,
  Download,
  FileUp,
  Loader2,
  RotateCcw,
  Trash2,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";

import {
  createDbSnapshot,
  deleteDbSnapshot,
  downloadDbBackup,
  getDbInfo,
  listDbSnapshots,
  restoreDbFromFile,
  restoreDbSnapshot,
} from "@/lib/api";
import { useSettings, useUpdateSettings } from "@/hooks/useJobs";
import { useAiStatus } from "@/hooks/useAiStatus";
import { reloadAfterRestore, takeRestoreNotice } from "@/lib/restoreReload";
import type { DbInfo, DbSnapshot } from "@/lib/types";

/**
 * Backup & Snapshots panel for the Settings page.
 *
 * The DB at ~/.musaic/library.db carries data that's expensive
 * to recover after a wipe — AI-driven MBID anchors (API fees),
 * AcoustID fingerprints (slow to recompute), the user's tag taxonomy. This panel surfaces
 * three protections:
 *
 *   1. **Automatic snapshots.** Taken on app startup if the configured
 *      interval has elapsed; retention pruned to N most recent.
 *   2. **Manual snapshot now.** Bypasses the interval gate.
 *   3. **Off-machine backup.** Download the live DB to disk so the
 *      user can copy it to iCloud / Time Machine / etc.
 *
 * Restore comes in two flavors: pick from the on-disk snapshot list,
 * or upload an external .db file (for off-machine recovery). Both
 * funnel through the same destructive confirm.
 */
export default function BackupPanel({
  onRestored = reloadAfterRestore,
}: {
  /** What a successful restore does with its confirmation message. The
   *  default reloads the app and shows the message afterwards
   *  (lib/restoreReload.ts); tests pass a spy. */
  onRestored?: (message: string) => void;
} = {}) {
  const ai = useAiStatus();
  // The confirmation of a restore that reloaded the page.
  // Deferred a tick so the App-level <Toaster> has subscribed.
  useEffect(() => {
    const notice = takeRestoreNotice();
    if (notice === null) return;
    const t = setTimeout(() => toast.success(notice), 0);
    return () => clearTimeout(t);
  }, []);
  const dbInfo = useQuery({
    queryKey: ["db-info"],
    queryFn: getDbInfo,
    staleTime: 10_000,
  });
  const snapshots = useQuery({
    queryKey: ["db-snapshots"],
    queryFn: listDbSnapshots,
    staleTime: 10_000,
  });
  const qc = useQueryClient();

  const refetchAll = () => {
    qc.invalidateQueries({ queryKey: ["db-info"] });
    qc.invalidateQueries({ queryKey: ["db-snapshots"] });
  };

  const snapshot = useMutation({
    mutationFn: () => createDbSnapshot(),
    onSuccess: (r) => {
      toast.success(
        `Snapshot saved (${humanBytes(r.size_bytes)})${r.pruned.length ? `; pruned ${r.pruned.length} older` : ""}`,
      );
      refetchAll();
    },
    onError: (e: Error) => toast.error(e.message || "Couldn't take snapshot"),
  });

  const downloadBackup = useMutation({
    mutationFn: () => downloadDbBackup(),
    onSuccess: () => toast.success("Backup download started"),
    onError: (e: Error) => toast.error(e.message || "Couldn't download backup"),
  });

  const deleteOne = useMutation({
    mutationFn: (name: string) => deleteDbSnapshot(name),
    onSuccess: () => {
      toast.success("Snapshot deleted");
      refetchAll();
    },
    onError: (e: Error) => toast.error(e.message || "Couldn't delete snapshot"),
  });

  const restoreFromSnap = useMutation({
    mutationFn: (name: string) => restoreDbSnapshot(name),
    onSuccess: () => onRestored("Restored from snapshot."),
    onError: (e: Error) => toast.error(e.message || "Restore failed"),
  });

  const restoreFromUpload = useMutation({
    mutationFn: (file: File) => restoreDbFromFile(file),
    onSuccess: () => onRestored("Restored from the uploaded backup."),
    onError: (e: Error) => toast.error(e.message || "Restore failed"),
  });

  const fileInputRef = useRef<HTMLInputElement>(null);
  const [pendingRestore, setPendingRestore] = useState<
    | { kind: "snapshot"; name: string; snapshot: DbSnapshot }
    | { kind: "upload"; file: File }
    | null
  >(null);

  const onPickFile = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";  // reset so picking the same file twice triggers
    if (!file) return;
    setPendingRestore({ kind: "upload", file });
  };

  const onConfirmRestore = () => {
    if (!pendingRestore) return;
    if (pendingRestore.kind === "snapshot") {
      restoreFromSnap.mutate(pendingRestore.name);
    } else {
      restoreFromUpload.mutate(pendingRestore.file);
    }
    setPendingRestore(null);
  };

  return (
    <section className="border border-line rounded-md bg-panel-2 overflow-hidden mb-6">
      <header className="flex items-center gap-2 px-4 h-10 border-b border-line bg-panel">
        <Database className="w-3.5 h-3.5 text-accent" />
        <h2 className="text-md font-semibold">Backup &amp; snapshots</h2>
      </header>

      <div className="px-4 py-4 space-y-5">
        <p className="text-sm text-fg-3 leading-relaxed">
          {ai.ready ? (
            <>
              Your library DB carries data that's expensive to rebuild — AI-driven
              MBID anchors, AcoustID fingerprints, your tag taxonomy.
              Automatic snapshots are taken on app startup so you can roll back
              a mistake.
            </>
          ) : (
            <>
              Your library DB carries data that's expensive to rebuild —
              MusicBrainz matches, AcoustID fingerprints, your tag taxonomy.
              Automatic snapshots are taken on app startup so you can roll back
              a mistake.
            </>
          )}
        </p>

        {/* Current DB stats */}
        {dbInfo.data && <CurrentDbCard info={dbInfo.data} aiReady={ai.ready} />}

        <SnapshotPolicy />

        {/* Action buttons */}
        <div className="border-t border-line pt-4 flex flex-wrap gap-2">
          <button
            onClick={() => snapshot.mutate()}
            disabled={snapshot.isPending}
            className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
          >
            {snapshot.isPending ? (
              <><Loader2 className="w-3.5 h-3.5 spin" /> Snapshotting…</>
            ) : (
              <><Camera className="w-3.5 h-3.5" /> Snapshot now</>
            )}
          </button>
          <button
            onClick={() => downloadBackup.mutate()}
            disabled={downloadBackup.isPending}
            className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition disabled:opacity-60 inline-flex items-center gap-1.5"
          >
            {downloadBackup.isPending ? (
              <><Loader2 className="w-3.5 h-3.5 spin" /> Preparing…</>
            ) : (
              <><Download className="w-3.5 h-3.5" /> Download backup</>
            )}
          </button>
          <button
            onClick={() => fileInputRef.current?.click()}
            className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1.5"
          >
            <FileUp className="w-3.5 h-3.5" /> Restore from file…
          </button>
          <input
            ref={fileInputRef}
            type="file"
            accept=".db,application/octet-stream,application/x-sqlite3"
            onChange={onPickFile}
            className="hidden"
          />
        </div>

        {/* Snapshot list */}
        <div className="border-t border-line pt-4 space-y-2">
          <div className="text-xs mono uppercase tracking-[0.06em] text-fg-4">
            Saved snapshots ({snapshots.data?.snapshots.length ?? 0})
          </div>
          {snapshots.isLoading ? (
            <div className="text-xs text-fg-4">Loading…</div>
          ) : (snapshots.data?.snapshots.length ?? 0) === 0 ? (
            <div className="text-sm text-fg-4 italic py-1">
              No snapshots yet. One will be taken automatically on the next app
              start, or you can take one manually with the button above.
            </div>
          ) : (
            <ul className="border border-line rounded divide-y divide-line">
              {snapshots.data!.snapshots.map((s) => (
                <SnapshotRow
                  key={s.filename}
                  snapshot={s}
                  onRestore={() => setPendingRestore({ kind: "snapshot", name: s.filename, snapshot: s })}
                  onDelete={() => deleteOne.mutate(s.filename)}
                  busy={
                    (restoreFromSnap.isPending && restoreFromSnap.variables === s.filename) ||
                    (deleteOne.isPending && deleteOne.variables === s.filename)
                  }
                />
              ))}
            </ul>
          )}
        </div>
      </div>

      {pendingRestore && (
        <RestoreConfirm
          target={pendingRestore}
          onConfirm={onConfirmRestore}
          onCancel={() => setPendingRestore(null)}
        />
      )}
    </section>
  );
}

const POLICY_INPUT_CLASS =
  "w-16 h-7 px-2 text-sm bg-bg border border-line rounded tabular-nums focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30";

/** The retention count as a whole number 0..100, or null when the text is not one. */
function parseRetention(text: string): number | null {
  const t = text.trim();
  if (!/^\d+$/.test(t)) return null;
  const n = Number(t);
  return n <= 100 ? n : null;
}

/** The interval as a non-negative number (fractions allowed), or null. */
function parseHours(text: string): number | null {
  const t = text.trim();
  if (!/^(\d+\.?\d*|\.\d+)$/.test(t)) return null;
  const v = Number(t);
  return Number.isFinite(v) ? v : null;
}

/**
 * Both snapshot-policy fields are held as a local draft and written
 * together by one explicit Save. Typing never touches the settings cache,
 * and a failed save leaves the draft in place for another try.
 */
function SnapshotPolicy() {
  const settings = useSettings();
  const update = useUpdateSettings();
  const savedCount = settings.data?.snapshot_retention_count;
  const savedHours = settings.data?.snapshot_min_interval_hours;
  const [count, setCount] = useState<string | null>(null);
  const [hours, setHours] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<
    { kind: "saved" } | { kind: "failed"; message: string } | null
  >(null);

  // A field the user has not edited follows the saved value.
  const countText = count ?? String(savedCount ?? 10);
  const hoursText = hours ?? String(savedHours ?? 24);
  const parsedCount = parseRetention(countText);
  const parsedHours = parseHours(hoursText);
  const valid = parsedCount !== null && parsedHours !== null;
  const dirty =
    settings.data !== undefined &&
    (parsedCount !== savedCount || parsedHours !== savedHours);

  const onSave = async () => {
    if (!valid) return;
    setOutcome(null);
    try {
      await update.mutateAsync({
        snapshot_retention_count: parsedCount,
        snapshot_min_interval_hours: parsedHours,
      });
      setCount(null);
      setHours(null);
      setOutcome({ kind: "saved" });
    } catch (e) {
      setOutcome({
        kind: "failed",
        message: (e as Error).message || "The settings could not be written.",
      });
    }
  };

  const edited = () => setOutcome(null);

  return (
    <form
      className="border-t border-line pt-4 space-y-2.5"
      aria-labelledby="snapshot-policy-heading"
      onSubmit={(e) => {
        e.preventDefault();
        void onSave();
      }}
    >
      <div
        id="snapshot-policy-heading"
        className="text-xs mono uppercase tracking-[0.06em] text-fg-4"
      >
        Snapshot policy
      </div>
      <div className="flex items-center gap-3 flex-wrap">
        <label className="text-sm flex items-center gap-2">
          <span className="text-fg-2">Keep last</span>
          <input
            type="number"
            min={0}
            max={100}
            step={1}
            value={countText}
            disabled={settings.isLoading || update.isPending}
            aria-invalid={parsedCount === null}
            onChange={(e) => {
              setCount(e.target.value);
              edited();
            }}
            className={POLICY_INPUT_CLASS}
          />
          <span className="text-fg-2">snapshots</span>
        </label>
        <span className="text-fg-4">·</span>
        <label className="text-sm flex items-center gap-2">
          <span className="text-fg-2">at least</span>
          <input
            type="number"
            min={0}
            step="0.5"
            value={hoursText}
            disabled={settings.isLoading || update.isPending}
            aria-invalid={parsedHours === null}
            onChange={(e) => {
              setHours(e.target.value);
              edited();
            }}
            className={POLICY_INPUT_CLASS}
          />
          <span className="text-fg-2">hours apart</span>
        </label>
      </div>
      <p className="text-xs text-fg-4 leading-relaxed">
        0 retention disables snapshotting. 0 hours forces a fresh snapshot every
        time the app starts (useful when actively editing). Keep last is a whole
        number from 0 to 100; hours may be fractional (0.5 is half an hour).
      </p>
      <div className="flex items-center gap-3 flex-wrap">
        <button
          type="submit"
          disabled={!dirty || !valid || update.isPending}
          className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
        >
          {update.isPending ? (
            <><Loader2 className="w-3.5 h-3.5 spin" /> Saving…</>
          ) : (
            "Save snapshot policy"
          )}
        </button>
        <div aria-live="polite" className="text-xs">
          {!valid && (
            <span className="text-warn">
              Keep last must be a whole number from 0 to 100, and hours a number
              of 0 or more.
            </span>
          )}
          {valid && outcome?.kind === "saved" && (
            <span className="text-ok">Snapshot policy saved.</span>
          )}
        </div>
      </div>
      {outcome?.kind === "failed" && (
        <p role="alert" className="text-xs text-err leading-relaxed">
          Snapshot policy was not saved: {outcome.message} Your changes are still
          here; check the message and press Save snapshot policy to try again.
        </p>
      )}
    </form>
  );
}

function CurrentDbCard({ info, aiReady }: { info: DbInfo; aiReady: boolean }) {
  return (
    <div className="border border-line rounded p-3 bg-panel">
      <div className="flex items-center justify-between gap-3 mb-2">
        <div className="mono text-xs uppercase tracking-[0.06em] text-fg-4">Current database</div>
        <div className="mono text-[11px] text-fg-3 tabular-nums">
          {humanBytes(info.size_bytes)}
          {info.schema_version !== null && (
            <span className="ml-2 text-fg-4">· schema v{info.schema_version}</span>
          )}
        </div>
      </div>
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-x-4 gap-y-1.5 text-xs">
        <Stat label="Albums"            value={info.counts.albums} />
        <Stat label="With MBID"         value={info.counts.albums_with_mbid} />
        <Stat label="Fingerprints"      value={info.counts.track_fingerprints} />
        <Stat label="Track tags"        value={info.counts.track_tags} />
        <Stat label="Vocabulary"        value={info.counts.tag_vocabulary} />
        <Stat label={aiReady ? "AI jobs" : "Jobs"} value={info.counts.ai_jobs} />
        <Stat label="Open findings"     value={info.counts.check_findings_open} />
      </div>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: number }) {
  return (
    <div className="flex items-baseline gap-1.5 min-w-0">
      <span className="text-fg-4 truncate">{label}</span>
      <span className="mono tabular-nums text-fg font-medium ml-auto">{value.toLocaleString()}</span>
    </div>
  );
}

function SnapshotRow({
  snapshot,
  onRestore,
  onDelete,
  busy,
}: {
  snapshot: DbSnapshot;
  onRestore: () => void;
  onDelete: () => void;
  busy: boolean;
}) {
  const albums = snapshot.counts?.albums;
  const mbid = snapshot.counts?.albums_with_mbid;
  return (
    <li className="px-3 py-2 flex items-center gap-3 text-sm">
      <div className="flex-1 min-w-0">
        <div className="mono text-fg-2 text-xs truncate" title={snapshot.filename}>
          {snapshot.filename}
        </div>
        <div className="mono text-[11px] text-fg-4 mt-0.5 tabular-nums flex flex-wrap gap-x-2.5">
          <span>{formatLocal(snapshot.modified_at)}</span>
          <span>·</span>
          <span>{humanBytes(snapshot.size_bytes)}</span>
          {snapshot.schema_version !== null && (
            <>
              <span>·</span>
              <span>v{snapshot.schema_version}</span>
            </>
          )}
          {albums !== undefined && (
            <>
              <span>·</span>
              <span>{albums} albums</span>
            </>
          )}
          {mbid !== undefined && (
            <>
              <span>·</span>
              <span>{mbid} MBIDs</span>
            </>
          )}
          {!snapshot.valid && (
            <span className="text-warn">· unreadable</span>
          )}
        </div>
      </div>
      <div className="flex items-center gap-1 shrink-0">
        <button
          onClick={onRestore}
          disabled={busy || !snapshot.valid}
          className="h-7 px-2 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1 disabled:opacity-50"
          title={snapshot.valid ? "Restore live DB from this snapshot" : "Snapshot is unreadable; can't restore"}
        >
          <RotateCcw className="w-3 h-3" />
          Restore
        </button>
        <button
          onClick={onDelete}
          disabled={busy}
          className="h-7 px-2 border border-line rounded text-xs text-err hover:bg-err-soft transition inline-flex items-center gap-1 disabled:opacity-50"
          title="Delete this snapshot"
        >
          <Trash2 className="w-3 h-3" />
        </button>
      </div>
    </li>
  );
}

function RestoreConfirm({
  target,
  onConfirm,
  onCancel,
}: {
  target:
    | { kind: "snapshot"; name: string; snapshot: DbSnapshot }
    | { kind: "upload"; file: File };
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const summary = target.kind === "snapshot"
    ? {
        label: target.snapshot.filename,
        size: target.snapshot.size_bytes,
        schema: target.snapshot.schema_version,
        when: formatLocal(target.snapshot.modified_at),
        counts: target.snapshot.counts,
      }
    : {
        label: target.file.name,
        size: target.file.size,
        schema: null,
        when: "from your computer",
        counts: undefined,
      };

  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/45 backdrop-blur-[1px] p-4">
      <div className="w-[min(520px,95vw)] bg-panel border border-line rounded-md shadow-sheet">
        <div className="px-4 py-3 border-b border-line flex items-center gap-2">
          <AlertTriangle className="w-4 h-4 text-warn" />
          <h3 className="font-semibold text-md">Restore database?</h3>
        </div>
        <div className="px-4 py-3.5 space-y-3 text-sm">
          <p className="text-fg-2">
            This will overwrite your current library database. The existing
            file will be moved aside as <code className="mono text-fg">library.db.bak-…</code> first,
            so this is recoverable. Musaic reloads itself once the restore is done.
          </p>
          <div className="border border-line rounded p-3 bg-panel-2 space-y-1 text-xs">
            <div className="mono text-fg-2 truncate" title={summary.label}>{summary.label}</div>
            <div className="mono text-[11px] text-fg-4 tabular-nums">
              {summary.when}
              <span className="mx-1.5">·</span>
              {humanBytes(summary.size)}
              {summary.schema !== null && summary.schema !== undefined && (
                <>
                  <span className="mx-1.5">·</span>
                  schema v{summary.schema}
                </>
              )}
            </div>
            {summary.counts && (
              <div className="mono text-[11px] text-fg-4 tabular-nums">
                {summary.counts.albums ?? "?"} albums
                {summary.counts.albums_with_mbid !== undefined &&
                  ` · ${summary.counts.albums_with_mbid} MBIDs`}
                {summary.counts.track_fingerprints !== undefined &&
                  ` · ${summary.counts.track_fingerprints} fingerprints`}
                {summary.counts.track_tags !== undefined &&
                  ` · ${summary.counts.track_tags} track tags`}
              </div>
            )}
          </div>
        </div>
        <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
          <button
            onClick={onCancel}
            className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition"
          >
            Cancel
          </button>
          <button
            onClick={onConfirm}
            className="h-8 px-3 rounded bg-warn text-warn-fg text-sm font-medium hover:opacity-90 transition inline-flex items-center gap-1.5"
          >
            <RotateCcw className="w-3.5 h-3.5" />
            Restore
          </button>
        </div>
      </div>
    </div>
  );
}

function humanBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

function formatLocal(iso: string | null): string {
  if (!iso) return "—";
  try {
    const d = new Date(iso);
    return d.toLocaleString();
  } catch {
    return iso;
  }
}
