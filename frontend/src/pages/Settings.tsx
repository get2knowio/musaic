import { useState, useEffect } from "react";
import {
  AlertTriangle,
  ArrowLeft,
  Check,
  Disc3,
  ExternalLink,
  Fingerprint,
} from "lucide-react";
import { toast } from "sonner";

import { Link, useLocation, useNavigate } from "react-router-dom";
import {
  RETURN_FOCUS_KEY,
  SETTINGS_SECTIONS,
  focusSection,
  sectionForHash,
  type SettingsLinkState,
} from "@/lib/settingsSections";
import { ACOUSTID_KEY_HASH } from "@/components/AcoustidPrompt";
import { useFingerprintPoolStatus } from "@/hooks/useFingerprintPoolStatus";
import { fingerprintStatusLine } from "@/lib/fingerprintStatus";
import { useLibraryConfig } from "@/hooks/useLibraryConfig";
import LibraryPathForm from "@/components/LibraryPathForm";
import { libraryPathHelp } from "@/lib/desktop";
import { useSettings, useUpdateSettings } from "@/hooks/useJobs";
import { usePageTitle } from "@/hooks/usePageTitle";
import { errorText } from "@/lib/errors";
import MyTagsSettingsPanel from "./SettingsMyTags";
import BackupPanel from "./SettingsBackup";
import AgentsPanel from "./SettingsAgents";
import FilenameTemplatesPanel from "./SettingsFilenameTemplates";

/**
 * Settings page. Grouped by task (library, tags, file naming, matching,
 * backup, assistants) so the page doesn't read as a flat list of unrelated
 * config blobs as it grows.
 */
export default function SettingsPage() {
  usePageTitle("Settings");
  // A link to a section (/settings#tags) scrolls to it and focuses its
  // control, the way "#acoustid-key" does for the AcoustID field. The
  // location key is a dependency so choosing the same link again repeats it.
  const { hash, key, state } = useLocation();
  const navigate = useNavigate();
  useEffect(() => {
    const id = sectionForHash(hash);
    if (id) focusSection(id);
  }, [hash, key]);
  const returnTo = (state as SettingsLinkState | null)?.returnTo;
  const backToAlbum = () => {
    try {
      sessionStorage.setItem(RETURN_FOCUS_KEY, "my-tags");
    } catch {
      // Without storage the album still opens; focus just stays at its top.
    }
    // The album's own history entry is the one just below this one, so going
    // back restores its browse / review context exactly (the album route's
    // filter, search and where it was opened from).
    navigate(-1);
  };
  return (
    <div className="flex-1 overflow-y-auto px-6 py-6">
      <div className="max-w-2xl">
        {returnTo && (
          <button
            type="button"
            onClick={backToAlbum}
            className="mb-3 h-7 px-2 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1.5"
          >
            <ArrowLeft className="w-3 h-3" /> Back to album
          </button>
        )}
        <h1 className="text-lg font-semibold mb-1">Settings</h1>
        <p className="text-sm text-fg-3 mb-3">
          Tune how Musaic talks to MusicBrainz and AcoustID.{" "}
          Jump to the task you are here for:
        </p>
        <SettingsIndex />
        <KeyStorageNotices />
        <SettingsSection id="library-folder" title="Library folder">
          <LibrarySection />
        </SettingsSection>
        <SettingsSection id="tags" title="Tags">
          <MyTagsSettingsPanel />
        </SettingsSection>
        <SettingsSection id="file-naming" title="File naming">
          <FilenameTemplatesPanel />
        </SettingsSection>
        <SettingsSection id="matching-integrations" title="Matching & integrations">
          <FingerprintSection />
        </SettingsSection>
        <SettingsSection id="backup-restore" title="Backup & restore">
          <BackupPanel />
        </SettingsSection>
        <SettingsSection id="agents" title="Assistants">
          <AgentsPanel />
        </SettingsSection>
      </div>
    </div>
  );
}

/** A compact, keyboard-reachable list of links, one per Settings task. */
function SettingsIndex() {
  const { state } = useLocation();
  return (
    <nav aria-label="Settings sections" className="mb-6">
      <ul className="flex flex-wrap gap-x-4 gap-y-1.5 text-sm">
        {SETTINGS_SECTIONS.map((s) => (
          <li key={s.id}>
            {/* replace: moving around the page does not add history entries,
                so "Back to album" still lands on the album's entry. */}
            <Link
              to={{ hash: `#${s.id}` }}
              replace
              state={state}
              className="text-accent hover:underline focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/40 rounded-sm"
            >
              {s.label}
            </Link>
          </li>
        ))}
      </ul>
    </nav>
  );
}

/** One task's group: a stable anchor (id = URL hash) that can take focus. */
function SettingsSection({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div
      id={id}
      tabIndex={-1}
      role="group"
      aria-labelledby={`${id}-heading`}
      className="scroll-mt-4 outline-none focus-visible:ring-2 focus-visible:ring-accent/40 rounded-md"
    >
      <h2
        id={`${id}-heading`}
        className="mono text-[11px] uppercase tracking-[0.08em] text-fg-4 mb-2 mt-1"
      >
        {title}
      </h2>
      {children}
    </div>
  );
}

/**
 * Notices about where API keys are kept, from two settings fields some
 * backends add: `keyring_available: false` (the system keychain can't be
 * reached, so keys read as not set and can't be saved) and
 * `keys_pending_removal: true` (keys from an earlier version are still in
 * the database file, its backup or an older snapshot while their removal is
 * retried).
 * Absent fields show nothing.
 */
function KeyStorageNotices() {
  const { data: settings } = useSettings();
  if (!settings) return null;
  const notices: string[] = [];
  if (settings.keyring_available === false) {
    notices.push(
      "The system keychain can't be reached, so saved API keys show as not set and new keys can't be saved. Features that need a key stay off until the keychain is available again.",
    );
  }
  if (settings.keys_pending_removal === true) {
    notices.push(
      "API keys saved by an earlier version are still in the library database file, its backup or an older snapshot. Musaic keeps trying to remove them, and the server log names any file it could not clean; this notice goes away once they are gone.",
    );
  }
  if (notices.length === 0) return null;
  return (
    <div className="space-y-2 mb-6">
      {notices.map((n) => (
        <div
          key={n}
          role="status"
          className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2"
        >
          <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
          <span>{n}</span>
        </div>
      ))}
    </div>
  );
}

/** Toast for a failed save: the action as the title, the server's reason
 *  (409 restore running, 503 keychain write failed, 400 validation) below. */
function saveFailed(what: string, e: unknown) {
  const reason = errorText(e, "");
  toast.error(what, reason ? { description: reason } : undefined);
}

/**
 * Library directory configuration. Two-state UX mirrors the AcoustID
 * section: when set, we show "configured: /path" with a Change
 * button; when clearing/changing, we expose the path entry.
 *
 * The native HTML file picker only emits files (not folders) without
 * the non-standard `webkitdirectory` attribute, and even then it picks
 * a folder by uploading every file inside — useless for our needs.
 * So in a browser we ask the user to paste/type the path; the desktop
 * window adds a native folder picker (components/LibraryPathForm.tsx).
 * The server refuses a path that is not a readable folder and keeps the
 * current one; the form shows its reason beside the field.
 */
function LibrarySection() {
  const config = useLibraryConfig();
  const navigate = useNavigate();
  const [editing, setEditing] = useState(false);

  const currentDir = config.data?.library_dir ?? null;
  const isSet = !!currentDir;
  const dirExists = config.data?.exists === true;

  // If the configured directory disappeared (renamed/unmounted), nudge the
  // user into edit mode automatically so the input is right there for them.
  useEffect(() => {
    if (isSet && !dirExists && !editing) setEditing(true);
  }, [isSet, dirExists, editing]);

  return (
    <section className="border border-line rounded-md bg-panel-2 overflow-hidden mb-6">
      <header className="flex items-center gap-2 px-4 h-10 border-b border-line bg-panel">
        <Disc3 className="w-3.5 h-3.5 text-accent" />
        <h2 className="text-md font-semibold">Library directory</h2>
      </header>

      <div className="px-4 py-4 space-y-4">
        <p className="text-sm text-fg-3 leading-relaxed">
          Point Musaic at a folder containing your album directories.
          Subfolders with FLAC files are treated as albums. Changing the
          directory triggers a fresh scan.
        </p>

        {isSet && !dirExists && !editing && (
          <div className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2">
            <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
            <span>
              The configured directory doesn't exist:{" "}
              <code className="mono text-fg-3">{currentDir}</code>
            </span>
          </div>
        )}

        <Row label="Path" help={libraryPathHelp()}>
          {isSet && !editing ? (
            <>
              <span
                className="inline-flex items-center gap-1.5 mono text-xs text-ok truncate max-w-[300px]"
                title={currentDir ?? undefined}
              >
                <Check className="w-3.5 h-3.5 shrink-0" />
                <span className="truncate">{currentDir}</span>
              </span>
              <button
                onClick={() => setEditing(true)}
                className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition"
              >
                Change
              </button>
            </>
          ) : (
            // Shared with the Library empty state; a saved,
            // existing directory goes to the grid, where its scan starts.
            <LibraryPathForm
              initial={currentDir ?? ""}
              onSaved={() => {
                setEditing(false);
                navigate("/library");
              }}
              onCancel={isSet ? () => setEditing(false) : undefined}
            />
          )}
        </Row>
      </div>
    </section>
  );
}

function Row({
  label,
  help,
  children,
}: {
  label: string;
  help: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="flex items-center gap-3 mb-1">
        <label className="text-sm font-medium">{label}</label>
        <div className="flex flex-wrap items-center justify-end gap-2 ml-auto">{children}</div>
      </div>
      <p className="text-xs text-fg-3 leading-relaxed">{help}</p>
    </div>
  );
}


/**
 * AcoustID API key configuration. Powers the per-track fingerprint cascade
 * in the track-gaps fix path (and future duplicate / MBID-anchoring checks).
 *
 * Two-state UX:
 *   - Key NOT set: input is editable, copy explains what to do.
 *   - Key set: shows "Configured" with a green check + a small "Change" link
 *     to reveal the input. We don't echo the saved value back from the
 *     server (it's write-only on the API), so the input always starts
 *     empty when revealed.
 */
function FingerprintSection() {
  const { data: settings, isLoading } = useSettings();
  const update = useUpdateSettings();
  const [editing, setEditing] = useState(false);
  const [key, setKey] = useState("");

  const isSet = !!settings?.acoustid_api_key_set;
  const { data: pool } = useFingerprintPoolStatus();
  const statusLine = fingerprintStatusLine(
    settings ? isSet : undefined,
    pool,
  );

  // "Add a key in Settings" (components/AcoustidPrompt.tsx) lands here:
  // scroll the field into view and focus it.
  const { hash } = useLocation();
  useEffect(() => {
    if (hash !== ACOUSTID_KEY_HASH || isLoading) return;
    const field = document.getElementById("acoustid-key-input");
    field?.scrollIntoView?.({ block: "center" });
    field?.focus();
  }, [hash, isLoading]);

  // When the user toggles into edit mode, clear any leftover input.
  useEffect(() => {
    if (editing) setKey("");
  }, [editing]);

  const onSave = () => {
    const trimmed = key.trim();
    update.mutate(
      { acoustid_api_key: trimmed },
      {
        onSuccess: () => {
          setEditing(false);
          setKey("");
          toast.success(trimmed ? "AcoustID key saved" : "AcoustID key cleared");
        },
        onError: (e) => saveFailed("Couldn't save AcoustID key", e),
      },
    );
  };

  const onClear = () => {
    update.mutate(
      { acoustid_api_key: "" },
      {
        onSuccess: () => {
          setEditing(false);
          toast.success("AcoustID key cleared");
        },
        onError: (e) => saveFailed("Couldn't clear AcoustID key", e),
      },
    );
  };

  return (
    <section className="border border-line rounded-md bg-panel-2 overflow-hidden mb-6">
      <header className="flex items-center gap-2 px-4 h-10 border-b border-line bg-panel">
        <Fingerprint className="w-3.5 h-3.5 text-accent" />
        <h2 className="text-md font-semibold">Track fingerprinting</h2>
      </header>

      <div className="px-4 py-4 space-y-4">
        <p className="text-sm text-fg-3 leading-relaxed">
          When MusicBrainz can't match an album by title alone (a rip in the wrong order,
          for example), we can fall back to per-track audio fingerprinting via
          <a
            href="https://acoustid.org/new-application"
            target="_blank"
            rel="noopener"
            className="text-accent hover:underline mx-1 inline-flex items-center gap-0.5"
          >
            AcoustID<ExternalLink className="w-3 h-3" />
          </a>
          to identify each track regardless of what its title says. Free with registration.
        </p>

        <p className="text-xs text-fg-3 leading-relaxed">
          Nothing needs installing: Musaic computes fingerprints itself. All you need is a free
          key from{" "}
          <a
            href="https://acoustid.org/new-application"
            target="_blank"
            rel="noopener"
            className="text-accent hover:underline"
          >
            acoustid.org/new-application
          </a>
          .
        </p>

        {statusLine && (
          <div
            role="status"
            id="fingerprint-status"
            className={`flex items-start gap-2 text-xs leading-relaxed rounded border px-3 py-2 ${
              statusLine.tone === "warn"
                ? "bg-warn-soft border-warn/30 text-fg-2"
                : "bg-panel border-line text-fg-2"
            }`}
          >
            {statusLine.tone === "warn" ? (
              <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
            ) : statusLine.tone === "ok" ? (
              <Check className="w-3.5 h-3.5 text-ok shrink-0 mt-0.5" />
            ) : (
              <Fingerprint className="w-3.5 h-3.5 text-fg-3 shrink-0 mt-0.5" />
            )}
            <span>{statusLine.text}</span>
          </div>
        )}

        <Row
          label="AcoustID API key"
          help="Used only by the track-gaps and unidentified-album fix paths. Stored locally; never sent anywhere but AcoustID."
        >
          {isSet && !editing ? (
            <>
              <span className="inline-flex items-center gap-1.5 mono text-xs text-ok">
                <Check className="w-3.5 h-3.5" />
                Configured
              </span>
              <button
                onClick={() => setEditing(true)}
                className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition"
              >
                Change
              </button>
              <button
                onClick={onClear}
                disabled={update.isPending}
                className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition disabled:opacity-60"
              >
                Clear
              </button>
            </>
          ) : (
            <>
              <input
                id="acoustid-key-input"
                type="password"
                aria-label="AcoustID API key"
                placeholder={isSet ? "Enter new key…" : "Paste key…"}
                value={key}
                disabled={isLoading || update.isPending}
                onChange={(e) => setKey(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") onSave(); }}
                className="w-64 h-8 px-2 border border-line rounded bg-bg text-sm text-fg focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30 disabled:opacity-60"
              />
              <button
                onClick={onSave}
                disabled={update.isPending || !key.trim()}
                className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60"
              >
                {update.isPending ? "Saving…" : "Save"}
              </button>
              {isSet && (
                <button
                  onClick={() => { setEditing(false); setKey(""); }}
                  className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition"
                >
                  Cancel
                </button>
              )}
            </>
          )}
        </Row>
      </div>
    </section>
  );
}
