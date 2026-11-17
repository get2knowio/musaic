import { useState, useEffect } from "react";
import {
  AlertTriangle,
  Check,
  Cpu,
  Disc3,
  ExternalLink,
  Fingerprint,
  Globe,
  Pause,
  Play,
  Sparkles,
} from "lucide-react";
import { toast } from "sonner";

import { useLocation, useNavigate } from "react-router-dom";
import { ACOUSTID_KEY_HASH } from "@/components/AcoustidPrompt";
import { useFingerprintPoolStatus } from "@/hooks/useFingerprintPoolStatus";
import { fingerprintStatusLine } from "@/lib/fingerprintStatus";
import { useLibraryConfig } from "@/hooks/useLibraryConfig";
import LibraryPathForm from "@/components/LibraryPathForm";
import { libraryPathHelp } from "@/lib/desktop";
import {
  useSettings,
  useUpdateSettings,
  useQueueState,
  useResumeQueue,
  useAiProviders,
} from "@/hooks/useJobs";
import { usePageTitle } from "@/hooks/usePageTitle";
import { useAiStatus } from "@/hooks/useAiStatus";
import { AI_UNAVAILABLE } from "@/lib/aiGating";
import { errorText } from "@/lib/errors";
import MyTagsSettingsPanel from "./SettingsMyTags";
import BackupPanel from "./SettingsBackup";
import FilenameTemplatesPanel from "./SettingsFilenameTemplates";

/**
 * Settings page. Grouped into Library, AI, and External services so the page
 * doesn't read as a flat list of unrelated config blobs as it grows.
 */
export default function SettingsPage() {
  usePageTitle("Settings");
  // No listed provider can run a model: the AI backend settings (and the
  // FireCrawl key, used only by AI lookups) have nothing to configure, so
  // one notice stands in for them (lib/aiGating.ts).
  const ai = useAiStatus();
  const noAiBackend = ai.known && !ai.anyReady;
  return (
    <div className="flex-1 overflow-y-auto px-6 py-6">
      <div className="max-w-2xl">
        <h1 className="text-lg font-semibold mb-1">Settings</h1>
        <p className="text-sm text-fg-3 mb-6">
          {noAiBackend
            ? "Tune how Musaic talks to MusicBrainz and AcoustID."
            : "Tune how Musaic talks to your AI provider, MusicBrainz, AcoustID, and FireCrawl."}
        </p>
        <KeyStorageNotices />
        <GroupHeader>Library</GroupHeader>
        <LibrarySection />
        <GroupHeader>Tagging</GroupHeader>
        <MyTagsSettingsPanel />
        <GroupHeader>Files</GroupHeader>
        <FilenameTemplatesPanel />
        <GroupHeader>AI</GroupHeader>
        {noAiBackend ? <AIUnavailableSection /> : <AIBackendSection />}
        {/* The AI worker card (parallel jobs, queue status) only
            when a provider can run. */}
        {!noAiBackend && <AISection />}
        <GroupHeader>External services</GroupHeader>
        <FingerprintSection />
        {!noAiBackend && <FireCrawlSection />}
        <GroupHeader>Database</GroupHeader>
        <BackupPanel />
      </div>
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

/** Stands in for the AI backend settings when no provider can run. */
function AIUnavailableSection() {
  return (
    <section className="border border-line rounded-md bg-panel-2 overflow-hidden mb-6">
      <header className="flex items-center gap-2 px-4 h-10 border-b border-line bg-panel">
        <Cpu className="w-3.5 h-3.5 text-accent" />
        <h2 className="text-md font-semibold">AI backend</h2>
      </header>
      <div className="px-4 py-4">
        <p className="text-sm text-fg-3 leading-relaxed">{AI_UNAVAILABLE}</p>
      </div>
    </section>
  );
}

/** Toast for a failed save: the action as the title, the server's reason
 *  (409 restore running, 503 keychain write failed, 400 validation) below. */
function saveFailed(what: string, e: unknown) {
  const reason = errorText(e, "");
  toast.error(what, reason ? { description: reason } : undefined);
}

function GroupHeader({ children }: { children: React.ReactNode }) {
  return (
    <h2 className="mono text-[11px] uppercase tracking-[0.08em] text-fg-4 mb-2 mt-1 first-of-type:mt-0">
      {children}
    </h2>
  );
}

/**
 * Library directory configuration. Two-state UX mirrors the AcoustID /
 * FireCrawl sections: when set, we show "configured: /path" with a Change
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

/**
 * AI backend selection — the provider/model/key the AI worker dispatches
 * through (via airframe). The flexibility is per-install, not per-request: a
 * given user picks one provider, one model, and (for key-based providers) one
 * API key.
 *
 * Credentials are handled per provider's auth model. Key-based providers
 * (Claude / Kimi / OpenAI-compatible gateways) show a write-only key field —
 * the key is injected into the airframe adapter constructor. Providers that
 * authenticate via their own env / CLI chain (Copilot via `gh`, Bedrock via
 * the AWS chain, the OpenCode server via loopback) show their auth hint
 * instead of a key field — there's nothing for the user to paste here.
 */
function AIBackendSection() {
  const { data: settings } = useSettings();
  const { data: providersData } = useAiProviders();
  const update = useUpdateSettings();

  const [modelDraft, setModelDraft] = useState("");
  const [editingKey, setEditingKey] = useState(false);
  const [keyDraft, setKeyDraft] = useState("");

  useEffect(() => {
    if (settings) setModelDraft(settings.ai_model_id);
  }, [settings?.ai_model_id]);
  useEffect(() => {
    if (editingKey) setKeyDraft("");
  }, [editingKey]);

  const providers = providersData?.providers ?? [];
  const uninstalled = providersData?.uninstalled ?? [];
  const selectedId = settings?.ai_provider_id ?? "claude";
  const selected = providers.find((p) => p.id === selectedId);
  const keySet = !!settings?.ai_api_key_set;
  const installedIds = new Set(providers.map((p) => p.id));
  // All ids to show in the dropdown: installed (selectable) + uninstalled
  // (disabled) + the current selection (so the control reflects state even if
  // its provider was later uninstalled).
  const optionIds = Array.from(new Set([...providers.map((p) => p.id), ...uninstalled, selectedId]));

  const onProvider = (pid: string) => {
    if (!settings || pid === settings.ai_provider_id) return;
    update.mutate(
      { ai_provider_id: pid },
      {
        onSuccess: () => toast.success(`Provider set to ${pid}`),
        onError: (e) => toast.error((e as Error).message || "Couldn't set provider"),
      },
    );
  };

  const commitModel = () => {
    const m = modelDraft.trim();
    if (!settings || m === settings.ai_model_id) return;
    update.mutate(
      { ai_model_id: m },
      { onError: (e) => saveFailed("Couldn't save model", e) },
    );
  };

  const onSaveKey = () => {
    const k = keyDraft.trim();
    update.mutate(
      { ai_api_key: k },
      {
        onSuccess: () => {
          setEditingKey(false);
          setKeyDraft("");
          toast.success(k ? "API key saved" : "API key cleared");
        },
        onError: (e) => saveFailed("Couldn't save API key", e),
      },
    );
  };

  const onClearKey = () => {
    update.mutate(
      { ai_api_key: "" },
      {
        onSuccess: () => {
          setEditingKey(false);
          toast.success("API key cleared");
        },
        onError: (e) => saveFailed("Couldn't clear API key", e),
      },
    );
  };

  return (
    <section className="border border-line rounded-md bg-panel-2 overflow-hidden mb-6">
      <header className="flex items-center gap-2 px-4 h-10 border-b border-line bg-panel">
        <Cpu className="w-3.5 h-3.5 text-accent" />
        <h2 className="text-md font-semibold">AI backend</h2>
      </header>

      <div className="px-4 py-4 space-y-5">
        <p className="text-sm text-fg-3 leading-relaxed">
          The agent backend Musaic runs metadata + cover-art suggestions through.
          Defaults to Claude; install additional airframe adapters to target
          another provider.
        </p>

        <Row
          label="Provider"
          help="Only installed providers can be selected. Add more with pip install airframe-agents[<extra>], then restart."
        >
          <select
            value={selectedId}
            disabled={update.isPending || !providersData}
            onChange={(e) => onProvider(e.target.value)}
            className="h-8 px-2 border border-line rounded bg-bg text-sm text-fg focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30 disabled:opacity-60"
          >
            {optionIds.map((id) => {
              const isInstalled = installedIds.has(id);
              return (
                <option key={id} value={id} disabled={!isInstalled}>
                  {id}
                  {isInstalled ? "" : " (not installed)"}
                </option>
              );
            })}
          </select>
        </Row>

        {/* Readiness — the selected provider's adapter shells out to a CLI
            (claude / gh) that may be absent (esp. in the packaged desktop
            app). When it's missing, AI calls fail; say so + how to fix, and
            reassure that deterministic matching still works. */}
        {selected && selected.ready === false && (
          <div className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2">
            <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
            <span>
              AI suggestions are unavailable: {selectedId} needs{" "}
              {selected.requirement ?? "an external CLI"}, which isn't installed.{" "}
              {selected.remedy}
            </span>
          </div>
        )}

        <Row
          label="Model"
          help="Vendor model id for the selected provider (e.g. claude-sonnet-4-6). Commit with Enter or by leaving the field."
        >
          <input
            type="text"
            placeholder="claude-sonnet-4-6"
            value={modelDraft}
            disabled={update.isPending}
            onChange={(e) => setModelDraft(e.target.value)}
            onBlur={commitModel}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            }}
            className="w-64 h-8 px-2 border border-line rounded bg-bg text-sm text-fg focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30 disabled:opacity-60"
          />
        </Row>

        {/* Credentials — shape depends on the provider's auth model. */}
        {!providersData ? null : selected?.accepts_api_key ? (
          <Row
            label="API key"
            help="Stored locally and passed to the provider's airframe adapter. Write-only — never echoed back."
          >
            {keySet && !editingKey ? (
              <>
                <span className="inline-flex items-center gap-1.5 mono text-xs text-ok">
                  <Check className="w-3.5 h-3.5" />
                  Configured
                </span>
                <button
                  onClick={() => setEditingKey(true)}
                  className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition"
                >
                  Change
                </button>
                <button
                  onClick={onClearKey}
                  disabled={update.isPending}
                  className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition disabled:opacity-60"
                >
                  Clear
                </button>
              </>
            ) : (
              <>
                <input
                  type="password"
                  placeholder={keySet ? "Enter new key…" : "Paste key…"}
                  value={keyDraft}
                  disabled={update.isPending}
                  onChange={(e) => setKeyDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") onSaveKey();
                  }}
                  className="w-64 h-8 px-2 border border-line rounded bg-bg text-sm text-fg focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30 disabled:opacity-60"
                />
                <button
                  onClick={onSaveKey}
                  disabled={update.isPending || !keyDraft.trim()}
                  className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60"
                >
                  {update.isPending ? "Saving…" : "Save"}
                </button>
                {keySet && (
                  <button
                    onClick={() => {
                      setEditingKey(false);
                      setKeyDraft("");
                    }}
                    className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition"
                  >
                    Cancel
                  </button>
                )}
              </>
            )}
          </Row>
        ) : selected ? (
          <div className="flex items-start gap-2 text-xs text-fg-3 leading-relaxed bg-bg-2 border border-line rounded px-3 py-2">
            <Check className="w-3.5 h-3.5 text-fg-4 shrink-0 mt-0.5" />
            <span>{selected.auth_hint}</span>
          </div>
        ) : (
          <div className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2">
            <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
            <span>
              The configured provider <code className="mono text-fg-3">{selectedId}</code>{" "}
              isn't installed here. Install its airframe extra, or pick an installed provider above.
            </span>
          </div>
        )}

        {uninstalled.length > 0 && (
          <p className="text-xs text-fg-4 leading-relaxed">
            Also supported once installed:{" "}
            <span className="mono">{uninstalled.join(", ")}</span>. Add with{" "}
            <code className="mono text-fg-3">pip install airframe-agents[&lt;extra&gt;]</code> and restart.
          </p>
        )}
      </div>
    </section>
  );
}

function AISection() {
  const { data: settings, isLoading } = useSettings();
  const { data: queueState } = useQueueState();
  const update = useUpdateSettings();
  const resume = useResumeQueue();

  // Track the input value locally so the user can type without each
  // keystroke firing a PATCH. We commit on blur or Enter.
  const [parallelDraft, setParallelDraft] = useState<string>("1");

  useEffect(() => {
    if (settings) setParallelDraft(String(settings.ai_max_parallel));
  }, [settings?.ai_max_parallel]);

  const commitParallel = () => {
    const n = Math.max(1, Math.min(8, parseInt(parallelDraft, 10) || 1));
    setParallelDraft(String(n));
    if (settings && n === settings.ai_max_parallel) return;
    update.mutate(
      { ai_max_parallel: n },
      {
        onError: (e) => saveFailed("Couldn't save settings", e),
      },
    );
  };

  const onResume = () => {
    resume.mutate(undefined, {
      onSuccess: () => toast.success("Queue resumed"),
      onError: (e) => saveFailed("Couldn't resume queue", e),
    });
  };

  return (
    <section className="border border-line rounded-md bg-panel-2 overflow-hidden mb-6">
      <header className="flex items-center gap-2 px-4 h-10 border-b border-line bg-panel">
        <Sparkles className="w-3.5 h-3.5 text-accent" />
        <h2 className="text-md font-semibold">AI worker</h2>
      </header>

      <div className="px-4 py-4 space-y-5">
        {/* Parallel jobs */}
        <Row
          label="Parallel jobs"
          help="How many AI suggestions to run in parallel. Default 1; bump up if you have a lot queued and don't mind several running at once."
        >
          <input
            type="number"
            min={1}
            max={8}
            value={parallelDraft}
            disabled={isLoading || update.isPending}
            onChange={(e) => setParallelDraft(e.target.value)}
            onBlur={commitParallel}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            }}
            className="w-20 h-8 px-2 border border-line rounded bg-panel text-sm text-fg tabular-nums focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30 disabled:opacity-60"
          />
          <span className="text-xs text-fg-4">1 – 8</span>
        </Row>

        {/* Queue pause state */}
        <div className="border-t border-line pt-4">
          <Row
            label="Queue status"
            help="The queue auto-pauses when an AI job fails, except when it failed only because no AI backend is available (this build has none). Resume manually after retrying or waiting."
          >
            {queueState?.paused ? (
              <>
                <span className="inline-flex items-center gap-1.5 mono text-xs text-warn">
                  <Pause className="w-3.5 h-3.5" />
                  Paused
                </span>
                <button
                  onClick={onResume}
                  disabled={resume.isPending}
                  className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
                >
                  <Play className="w-3.5 h-3.5" />
                  {resume.isPending ? "Resuming…" : "Resume queue"}
                </button>
              </>
            ) : (
              <span className="inline-flex items-center gap-1.5 mono text-xs text-ok">
                <Play className="w-3.5 h-3.5" />
                Running
              </span>
            )}
            {queueState && (
              <span className="text-xs text-fg-4 ml-auto mono tabular-nums">
                {queueState.running} running · {queueState.queued} queued
              </span>
            )}
          </Row>
          {queueState?.paused && (
            <div className="mt-3 flex items-start gap-2 text-xs text-fg-3 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2">
              <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
              <span>
                We pause the queue automatically on the first error. Failed jobs can be retried from the AI Jobs page; remaining queued jobs will start dispatching once you resume.
              </span>
            </div>
          )}
        </div>
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
  const ai = useAiStatus();
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
          help={
            ai.ready
              ? "Used only by the track-gaps and unidentified-album fix paths. Stored locally; never sent to Claude."
              : "Used only by the track-gaps and unidentified-album fix paths. Stored locally."
          }
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


/**
 * FireCrawl API key configuration. When set, ai.py mounts the FireCrawl
 * MCP server into Claude Agent SDK calls so Claude can fall back to
 * FireCrawl tools (search / scrape / extract) for albums where the
 * built-in WebSearch / WebFetch can't get past JS-rendered pages,
 * paywalls, or anti-bot blocks.
 *
 * Treated as an absolute LAST RESORT: the system prompt instructs Claude
 * to try WebSearch / WebFetch first and only reach for FireCrawl when
 * those genuinely can't get the answer. Each FireCrawl call uses the
 * user's FireCrawl quota.
 *
 * Same write-only / two-state UX as the AcoustID section.
 */
function FireCrawlSection() {
  const { data: settings, isLoading } = useSettings();
  const update = useUpdateSettings();
  const [editing, setEditing] = useState(false);
  const [key, setKey] = useState("");

  const isSet = !!settings?.firecrawl_api_key_set;

  useEffect(() => {
    if (editing) setKey("");
  }, [editing]);

  const onSave = () => {
    const trimmed = key.trim();
    update.mutate(
      { firecrawl_api_key: trimmed },
      {
        onSuccess: () => {
          setEditing(false);
          setKey("");
          toast.success(trimmed ? "FireCrawl key saved" : "FireCrawl key cleared");
        },
        onError: (e) => saveFailed("Couldn't save FireCrawl key", e),
      },
    );
  };

  const onClear = () => {
    update.mutate(
      { firecrawl_api_key: "" },
      {
        onSuccess: () => {
          setEditing(false);
          toast.success("FireCrawl key cleared");
        },
        onError: (e) => saveFailed("Couldn't clear FireCrawl key", e),
      },
    );
  };

  return (
    <section className="border border-line rounded-md bg-panel-2 overflow-hidden mb-6">
      <header className="flex items-center gap-2 px-4 h-10 border-b border-line bg-panel">
        <Globe className="w-3.5 h-3.5 text-accent" />
        <h2 className="text-md font-semibold">FireCrawl fallback</h2>
      </header>

      <div className="px-4 py-4 space-y-4">
        <p className="text-sm text-fg-3 leading-relaxed">
          When the model's built-in web search and fetch can't get through
          (JS-rendered pages, paywalls, anti-bot blocks), it can fall back
          to{" "}
          <a
            href="https://www.firecrawl.dev/"
            target="_blank"
            rel="noopener"
            className="text-accent hover:underline mx-0.5 inline-flex items-center gap-0.5"
          >
            FireCrawl<ExternalLink className="w-3 h-3" />
          </a>
          . The system prompt tells the model to treat FireCrawl as a last
          resort — each call uses up your FireCrawl quota.
        </p>

        <div className="border border-line rounded p-3 bg-panel">
          <div className="text-xs mono uppercase tracking-[0.06em] text-fg-4 mb-1.5">
            How it's used
          </div>
          <ul className="text-xs text-fg-3 space-y-1 leading-relaxed">
            <li>Only attached to AI metadata + cover-art lookups (not bulk MB passes).</li>
            <li>Tools exposed to the model: <code className="mono text-fg-2">firecrawl_search</code>, <code className="mono text-fg-2">firecrawl_scrape</code>, <code className="mono text-fg-2">firecrawl_extract</code>.</li>
            <li>Leave the key blank to disable — the model falls back to MusicBrainz instead.</li>
          </ul>
        </div>

        <Row
          label="FireCrawl API key"
          help="Stored locally and sent only to the FireCrawl MCP endpoint, in the URL path. Never echoed back from the API."
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
                type="password"
                placeholder={isSet ? "Enter new key…" : "fc-…"}
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
