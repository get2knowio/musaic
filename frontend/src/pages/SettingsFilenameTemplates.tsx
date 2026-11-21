import { FileText } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";

import { useSettings, useUpdateSettings } from "@/hooks/useJobs";

/**
 * Settings panel for filename templates used by the
 * filename-mismatch health check + its rename apply path.
 *
 * Two templates:
 *   - Single-artist albums: artist usually redundant (already in
 *     parent dir name + albumartist tag), so the default omits it.
 *   - Compilations: per-track artist isn't anywhere else, so the
 *     default includes it.
 *
 * Shows a live preview using sample data so the user can see what
 * a track will be renamed to without leaving the page.
 */
export default function FilenameTemplatesPanel() {
  const { data: settings } = useSettings();
  const update = useUpdateSettings();

  const [singleDraft, setSingleDraft] = useState("");
  const [compDraft, setCompDraft] = useState("");
  const [dirSingleDraft, setDirSingleDraft] = useState("");
  const [dirCompDraft, setDirCompDraft] = useState("");

  // Sync drafts with server values on first load and when the
  // server side changes (e.g. another tab updated them).
  useEffect(() => {
    if (settings?.filename_template_single_artist !== undefined) {
      setSingleDraft(settings.filename_template_single_artist);
    }
  }, [settings?.filename_template_single_artist]);
  useEffect(() => {
    if (settings?.filename_template_compilation !== undefined) {
      setCompDraft(settings.filename_template_compilation);
    }
  }, [settings?.filename_template_compilation]);
  useEffect(() => {
    if (settings?.dir_template_single_artist !== undefined) {
      setDirSingleDraft(settings.dir_template_single_artist);
    }
  }, [settings?.dir_template_single_artist]);
  useEffect(() => {
    if (settings?.dir_template_compilation !== undefined) {
      setDirCompDraft(settings.dir_template_compilation);
    }
  }, [settings?.dir_template_compilation]);

  const commitSingle = () => {
    const v = singleDraft.trim();
    if (!v || v === settings?.filename_template_single_artist) return;
    update.mutate({ filename_template_single_artist: v }, {
      onError: () => toast.error("Couldn't save template"),
    });
  };
  const commitComp = () => {
    const v = compDraft.trim();
    if (!v || v === settings?.filename_template_compilation) return;
    update.mutate({ filename_template_compilation: v }, {
      onError: () => toast.error("Couldn't save template"),
    });
  };
  const commitDirSingle = () => {
    const v = dirSingleDraft.trim();
    if (!v || v === settings?.dir_template_single_artist) return;
    update.mutate({ dir_template_single_artist: v }, {
      onError: () => toast.error("Couldn't save template"),
    });
  };
  const commitDirComp = () => {
    const v = dirCompDraft.trim();
    if (!v || v === settings?.dir_template_compilation) return;
    update.mutate({ dir_template_compilation: v }, {
      onError: () => toast.error("Couldn't save template"),
    });
  };

  const singlePreview    = useMemo(() => previewSingle(singleDraft), [singleDraft]);
  const compPreview      = useMemo(() => previewComp(compDraft), [compDraft]);
  const dirSinglePreview = useMemo(() => previewDirSingle(dirSingleDraft), [dirSingleDraft]);
  const dirCompPreview   = useMemo(() => previewDirComp(dirCompDraft), [dirCompDraft]);

  return (
    <section className="border border-line rounded-md bg-panel-2 overflow-hidden mb-6">
      <header className="flex items-center gap-2 px-4 h-10 border-b border-line bg-panel">
        <FileText className="w-3.5 h-3.5 text-accent" />
        <h2 className="text-md font-semibold">Filename templates</h2>
      </header>

      <div className="px-4 py-4 space-y-5">
        <p className="text-sm text-fg-3 leading-relaxed">
          Used by the <strong>Filename doesn't match template</strong> health
          check to flag and rename FLACs whose names don't match. Two
          templates because compilation tracks need the per-track artist
          baked into the filename (the album folder doesn't carry it),
          but single-artist tracks usually don't.
        </p>

        <TemplateField
          label="Single-artist albums"
          focusTarget
          value={singleDraft}
          onChange={setSingleDraft}
          onCommit={commitSingle}
          preview={singlePreview}
          disabled={update.isPending}
          placeholder="{tracknumber:02} - {title}"
        />

        <TemplateField
          label="Compilations"
          value={compDraft}
          onChange={setCompDraft}
          onCommit={commitComp}
          preview={compPreview}
          disabled={update.isPending}
          placeholder="{tracknumber:02} - {artist} - {title}"
        />

        <div className="border-t border-line pt-4 space-y-3">
          <div className="text-xs mono uppercase tracking-[0.06em] text-fg-4">
            Album folder names
          </div>

          <TemplateField
            label="Single-artist albums (folder)"
            value={dirSingleDraft}
            onChange={setDirSingleDraft}
            onCommit={commitDirSingle}
            preview={dirSinglePreview}
            disabled={update.isPending}
            placeholder="{albumartist} - {album} [FLAC]"
          />

          <TemplateField
            label="Compilations (folder)"
            value={dirCompDraft}
            onChange={setDirCompDraft}
            onCommit={commitDirComp}
            preview={dirCompPreview}
            disabled={update.isPending}
            placeholder="Various Artists - {album} [FLAC]"
          />
          <p className="text-[11px] text-fg-4 leading-relaxed">
            Multi-disc albums automatically get a "(disc N of M)" segment
            appended — you don't need to put it in the template.
          </p>
        </div>

        <div className="border border-line rounded p-3 bg-panel">
          <div className="text-xs mono uppercase tracking-[0.06em] text-fg-4 mb-1.5">
            Available variables
          </div>
          <ul className="text-xs text-fg-3 space-y-1 leading-relaxed mono">
            <li><code className="text-fg-2">{`{tracknumber}`}</code> — supports format specs like <code>{`{tracknumber:02}`}</code> for zero-padding</li>
            <li><code className="text-fg-2">{`{title}`}</code> — track title</li>
            <li><code className="text-fg-2">{`{artist}`}</code> — per-track artist (use on compilations)</li>
            <li><code className="text-fg-2">{`{albumartist}`}</code> — album-level artist credit</li>
            <li><code className="text-fg-2">{`{album}`}</code> — album title</li>
            <li><code className="text-fg-2">{`{year}`}</code></li>
            <li><code className="text-fg-2">{`{disc}`}</code> — disc number for multi-disc sets</li>
          </ul>
          <p className="text-[11px] text-fg-4 mt-2 leading-relaxed">
            The <code className="mono">.flac</code> extension is added automatically.
            Filesystem-illegal characters (<code className="mono">/ \ : * ? " &lt; &gt; |</code>)
            are replaced with <code className="mono">" - "</code>; trailing whitespace
            and dots are trimmed.
          </p>
        </div>
      </div>
    </section>
  );
}

function TemplateField({
  label,
  value,
  onChange,
  onCommit,
  preview,
  disabled,
  placeholder,
  focusTarget,
}: {
  /** The field a link to the File naming section focuses. */
  focusTarget?: boolean;
  label: string;
  value: string;
  onChange: (v: string) => void;
  onCommit: () => void;
  preview: string;
  disabled?: boolean;
  placeholder: string;
}) {
  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium block">{label}</label>
      <input
        type="text"
        data-settings-focus={focusTarget ? "" : undefined}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={onCommit}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        }}
        disabled={disabled}
        placeholder={placeholder}
        className="w-full max-w-md h-8 px-2.5 text-sm bg-bg border border-line rounded mono focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30 disabled:opacity-60"
      />
      <div className="text-xs text-fg-4 leading-relaxed">
        Preview: <code className="mono text-fg-3">{preview}</code>
      </div>
    </div>
  );
}

/** Tiny client-side renderer that mirrors the server's
 *  filename_template.render for preview purposes. We only need to
 *  show the user what their template produces with sample data —
 *  the real apply path runs server-side and uses the full
 *  implementation. Doesn't have to be 100% byte-equivalent (the
 *  server does extra sanitization passes); it just needs to be
 *  close enough to be useful. */
function previewSingle(template: string): string {
  return renderPreview(template, {
    tracknumber: 5,
    title: "Yesterday",
    artist: "The Beatles",
    albumartist: "The Beatles",
    album: "Help!",
    year: "1965",
    disc: 1,
  }) + ".flac";
}

function previewComp(template: string): string {
  return renderPreview(template, {
    tracknumber: 12,
    title: "You Can't Hurry Love",
    artist: "The Supremes",
    albumartist: "Various Artists",
    album: "AM Gold 1966",
    year: "1966",
    disc: 1,
  }) + ".flac";
}

function previewDirSingle(template: string): string {
  return renderPreview(template, {
    title: "Help!",
    album: "Help!",
    artist: "The Beatles",
    albumartist: "The Beatles",
    year: "1965",
  });
}

function previewDirComp(template: string): string {
  return renderPreview(template, {
    title: "AM Gold 1966",
    album: "AM Gold 1966",
    artist: "Various Artists",
    albumartist: "Various Artists",
    year: "1966",
  });
}

const PLACEHOLDER_RE = /\{([a-zA-Z_][a-zA-Z0-9_]*)(?::([^}]*))?\}/g;

function renderPreview(
  template: string,
  values: Record<string, string | number>,
): string {
  if (!template) return "";
  const out = template.replace(PLACEHOLDER_RE, (_, name: string, spec: string) => {
    const raw = values[name];
    if (raw == null) return "";
    if (name === "tracknumber" || name === "disc") {
      const n = typeof raw === "number" ? raw : parseInt(String(raw), 10);
      if (!Number.isFinite(n)) return String(raw);
      // Support the {x:02} zero-pad form.
      const m = spec?.match(/^0(\d+)$/);
      if (m) return n.toString().padStart(parseInt(m[1], 10), "0");
      return n.toString();
    }
    return String(raw).trim();
  });
  // Mirror the server's sanitize pass roughly: strip path-illegal
  // chars, trim, collapse whitespace.
  return out
    .replace(/[/\\:*?"<>|]/g, " - ")
    .replace(/\s+/g, " ")
    .replace(/(?:\s*-\s*){2,}/g, " - ")
    .replace(/^(?:\s*-\s*)+/, "")
    .replace(/(?:\s*-\s*)+$/, "")
    .replace(/[.\s]+$/, "")
    .trim();
}
