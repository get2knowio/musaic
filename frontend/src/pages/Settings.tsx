import { useState, useEffect } from "react";
import {
  AlertTriangle,
  Check,
  Disc3,
  ExternalLink,
  Fingerprint,
  Globe,
  Pause,
  Play,
  Sparkles,
  X,
} from "lucide-react";
import { toast } from "sonner";

import { useLibraryConfig, useSetLibraryDir } from "@/hooks/useLibraryConfig";
import { useSettings, useUpdateSettings, useQueueState, useResumeQueue } from "@/hooks/useJobs";
import { usePageTitle } from "@/hooks/usePageTitle";
import MyTagsSettingsPanel from "./SettingsMyTags";
import BackupPanel from "./SettingsBackup";
import FilenameTemplatesPanel from "./SettingsFilenameTemplates";

/**
 * Settings page. Grouped into Library, AI, and External services so the page
 * doesn't read as a flat list of unrelated config blobs as it grows.
 */
export default function SettingsPage() {
  usePageTitle("Settings");
  return (
    <div className="flex-1 overflow-y-auto px-6 py-6">
      <div className="max-w-2xl">
        <h1 className="text-lg font-semibold mb-1">Settings</h1>
        <p className="text-sm text-fg-3 mb-6">
          Tune how Media Manager talks to Claude, MusicBrainz, AcoustID, and FireCrawl.
        </p>
        <GroupHeader>Library</GroupHeader>
        <LibrarySection />
        <GroupHeader>Tagging</GroupHeader>
        <MyTagsSettingsPanel />
        <GroupHeader>Files</GroupHeader>
        <FilenameTemplatesPanel />
        <GroupHeader>AI</GroupHeader>
        <AISection />
        <GroupHeader>External services</GroupHeader>
        <FingerprintSection />
        <FireCrawlSection />
        <GroupHeader>Database</GroupHeader>
        <BackupPanel />
      </div>
    </div>
  );
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
 * button; when clearing/changing, we expose a paste-only text input.
 *
 * The native HTML file picker only emits files (not folders) without
 * the non-standard `webkitdirectory` attribute, and even then it picks
 * a folder by uploading every file inside — useless for our needs.
 * So we ask the user to paste/type the path. The validation message
 * comes back from the server when `exists` is false.
 */
function LibrarySection() {
  const config = useLibraryConfig();
  const setDir = useSetLibraryDir();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");

  const currentDir = config.data?.library_dir ?? null;
  const isSet = !!currentDir;
  const dirExists = config.data?.exists === true;

  // When the user toggles into edit mode, start with the current value
  // (so they can correct a typo without retyping the whole path).
  useEffect(() => {
    if (editing) setDraft(currentDir ?? "");
  }, [editing, currentDir]);

  // If the configured directory disappeared (renamed/unmounted), nudge the
  // user into edit mode automatically so the input is right there for them.
  useEffect(() => {
    if (isSet && !dirExists && !editing) setEditing(true);
  }, [isSet, dirExists, editing]);

  const onSave = () => {
    const trimmed = draft.trim();
    if (!trimmed) return;
    setDir.mutate(trimmed, {
      onSuccess: (next) => {
        if (next.exists) {
          toast.success("Library directory updated");
          setEditing(false);
        } else {
          // Server accepted the path but it doesn't exist on disk; keep the
          // input open so the user can fix it.
          toast.warning("Saved, but that directory doesn't exist", {
            description: "Check the path and try again.",
          });
        }
      },
      onError: (e) => toast.error((e as Error).message || "Couldn't save library directory"),
    });
  };

  return (
    <section className="border border-line rounded-md bg-panel-2 overflow-hidden mb-6">
      <header className="flex items-center gap-2 px-4 h-10 border-b border-line bg-panel">
        <Disc3 className="w-3.5 h-3.5 text-accent" />
        <h2 className="text-md font-semibold">Library directory</h2>
      </header>

      <div className="px-4 py-4 space-y-4">
        <p className="text-sm text-fg-3 leading-relaxed">
          Point Media Manager at a folder containing your album directories.
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

        <Row
          label="Path"
          help="Paste an absolute path. We don't pop a native folder picker because the browser file dialog returns files, not folders."
        >
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
            <>
              <input
                type="text"
                placeholder="/Users/you/Music/FLAC"
                value={draft}
                disabled={setDir.isPending}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") onSave();
                }}
                className="w-72 h-8 px-2 border border-line rounded bg-bg text-sm text-fg focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30 disabled:opacity-60"
              />
              <button
                onClick={onSave}
                disabled={setDir.isPending || !draft.trim()}
                className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60"
              >
                {setDir.isPending ? "Saving…" : "Save"}
              </button>
              {isSet && (
                <button
                  onClick={() => {
                    setEditing(false);
                    setDraft(currentDir ?? "");
                  }}
                  className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition"
                >
                  <X className="w-3.5 h-3.5" />
                </button>
              )}
            </>
          )}
        </Row>
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
        onError: () => toast.error("Couldn't save settings"),
      },
    );
  };

  const onResume = () => {
    resume.mutate(undefined, {
      onSuccess: () => toast.success("Queue resumed"),
      onError: () => toast.error("Couldn't resume queue"),
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
            help="The queue auto-pauses when an AI job fails (most failures are rate-limit / capacity-related). Resume manually after retrying or waiting."
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
        <div className="flex items-center gap-2 ml-auto">{children}</div>
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
        onError: () => toast.error("Couldn't save AcoustID key"),
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
        onError: () => toast.error("Couldn't clear AcoustID key"),
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

        <div className="border border-line rounded p-3 bg-panel">
          <div className="text-xs mono uppercase tracking-[0.06em] text-fg-4 mb-1.5">
            Requirements
          </div>
          <ul className="text-xs text-fg-3 space-y-1 leading-relaxed">
            <li>
              <code className="mono text-fg-2">brew install chromaprint</code>{" "}
              (provides the <code className="mono">fpcalc</code> binary)
            </li>
            <li>
              An AcoustID API key from{" "}
              <a
                href="https://acoustid.org/new-application"
                target="_blank"
                rel="noopener"
                className="text-accent hover:underline"
              >
                acoustid.org/new-application
              </a>
            </li>
          </ul>
        </div>

        <Row
          label="AcoustID API key"
          help="Used only by the track-gaps and missing-fingerprints fix paths. Stored locally; never sent to Claude."
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
 * those genuinely can't get the answer. Each FireCrawl call burns paid
 * quota.
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
        onError: () => toast.error("Couldn't save FireCrawl key"),
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
        onError: () => toast.error("Couldn't clear FireCrawl key"),
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
          When Claude's built-in WebSearch and WebFetch can't get through
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
          . The system prompt tells Claude to treat FireCrawl as a last
          resort — each call burns your paid FireCrawl quota.
        </p>

        <div className="border border-line rounded p-3 bg-panel">
          <div className="text-xs mono uppercase tracking-[0.06em] text-fg-4 mb-1.5">
            How it's used
          </div>
          <ul className="text-xs text-fg-3 space-y-1 leading-relaxed">
            <li>Only attached to AI metadata + cover-art lookups (not bulk MB passes).</li>
            <li>Tools exposed to Claude: <code className="mono text-fg-2">firecrawl_search</code>, <code className="mono text-fg-2">firecrawl_scrape</code>, <code className="mono text-fg-2">firecrawl_extract</code>.</li>
            <li>Leave the key blank to disable — Claude falls back to MusicBrainz instead.</li>
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
