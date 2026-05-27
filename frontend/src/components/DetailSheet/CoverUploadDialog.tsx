/**
 * Manual cover-art upload.
 *
 * Three input modes share one Apply button:
 *
 *   - file picker / drag-drop  → multipart POST /api/albums/{id}/cover/upload
 *   - paste from clipboard     → same as above (pulled from a paste event)
 *   - paste image URL          → JSON POST /api/albums/{id}/apply/stream
 *                                with `cover_url` (server downloads + embeds)
 *
 * Both paths funnel through `services.apply.apply_album_changes_iter`
 * server-side, so the on-disk result is identical: every FLAC re-embedded
 * + cover.jpg sidecar + thumb invalidated + post-scan checks rerun.
 */
import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Image as ImageIcon, Link as LinkIcon, Loader2, Upload, X } from "lucide-react";
import { toast } from "sonner";

import { postSse } from "@/lib/sse";
import type { Album } from "@/lib/types";

interface Props {
  album: Album;
  open: boolean;
  onClose: () => void;
}

type Source =
  | { kind: "empty" }
  | { kind: "file"; file: File; previewUrl: string }
  | { kind: "url"; url: string };

const MAX_BYTES = 25 * 1024 * 1024;

export default function CoverUploadDialog({ album, open, onClose }: Props) {
  const [source, setSource] = useState<Source>({ kind: "empty" });
  const [dragOver, setDragOver] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Reset state on open. Revoking object URLs on close is handled by
  // the cleanup effect below.
  useEffect(() => {
    if (open) {
      setSource({ kind: "empty" });
      setDragOver(false);
      setSubmitting(false);
    }
  }, [open]);

  // Free any blob URL we created when the source changes or we close.
  useEffect(() => {
    return () => {
      if (source.kind === "file") URL.revokeObjectURL(source.previewUrl);
    };
  }, [source]);

  // Clipboard paste — listen on document while the dialog is open so a
  // bare Cmd+V (without focusing the URL field) lands an image.
  useEffect(() => {
    if (!open) return;
    const onPaste = (e: ClipboardEvent) => {
      const items = e.clipboardData?.items;
      if (!items) return;
      for (const item of Array.from(items)) {
        if (item.kind === "file" && item.type.startsWith("image/")) {
          const file = item.getAsFile();
          if (file) {
            e.preventDefault();
            acceptFile(file);
            return;
          }
        }
      }
    };
    document.addEventListener("paste", onPaste);
    return () => document.removeEventListener("paste", onPaste);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const acceptFile = (file: File) => {
    if (!file.type.startsWith("image/")) {
      toast.error(`Not an image: ${file.type || "(unknown type)"}`);
      return;
    }
    if (file.size === 0) {
      toast.error("Empty file");
      return;
    }
    if (file.size > MAX_BYTES) {
      toast.error(`File too large (${(file.size / 1024 / 1024).toFixed(1)} MB; max 25 MB)`);
      return;
    }
    setSource((prev) => {
      if (prev.kind === "file") URL.revokeObjectURL(prev.previewUrl);
      return { kind: "file", file, previewUrl: URL.createObjectURL(file) };
    });
  };

  const onFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (file) acceptFile(file);
    // Reset the input so picking the same file twice in a row still fires.
    e.target.value = "";
  };

  const onDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    const file = e.dataTransfer?.files?.[0];
    if (file) acceptFile(file);
  };

  const onApply = async () => {
    if (source.kind === "empty" || submitting) return;
    setSubmitting(true);

    try {
      let coverChanged = false;
      const handlers: Record<string, (data: unknown) => void> = {
        cover_downloaded: () => { coverChanged = true; },
        error: (data) => {
          throw new Error((data as { detail: string }).detail);
        },
      };

      if (source.kind === "file") {
        const form = new FormData();
        form.append("cover", source.file, source.file.name);
        await postSse(
          `/api/albums/${encodeURIComponent(album.id)}/cover/upload`,
          form,
          handlers,
        );
      } else {
        const url = source.url.trim();
        if (!url) {
          toast.error("Paste an image URL or pick a file");
          setSubmitting(false);
          return;
        }
        await postSse(
          `/api/albums/${encodeURIComponent(album.id)}/apply/stream`,
          { cover_url: url, suggestions: {}, track_suggestions: {} },
          handlers,
        );
      }

      if (coverChanged) {
        toast.success("Cover updated", { duration: 2500 });
        // Cache invalidation is handled by the App-level
        // useLibraryEventsStream subscriber — services/apply
        // publishes AlbumCoverChanged + AlbumTagsChanged.
      } else {
        toast.warning("Apply finished but no cover change was reported");
      }
      onClose();
    } catch (err) {
      toast.error((err as Error).message || "Cover upload failed");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o && !submitting) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-[60] bg-black/45 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed top-1/2 left-1/2 z-[61] -translate-x-1/2 -translate-y-1/2 w-[min(520px,95vw)] bg-panel border border-line rounded-lg shadow-popup outline-none flex flex-col"
          aria-describedby={undefined}
          onEscapeKeyDown={(e) => { if (submitting) e.preventDefault(); }}
          onPointerDownOutside={(e) => { if (submitting) e.preventDefault(); }}
        >
          <div className="flex items-center justify-between px-4 h-11 border-b border-line">
            <div className="flex items-center gap-2">
              <ImageIcon className="w-3.5 h-3.5 text-accent" />
              <Dialog.Title className="font-semibold text-md">Replace cover art</Dialog.Title>
            </div>
            <button
              onClick={onClose}
              disabled={submitting}
              className="w-8 h-8 grid place-items-center text-fg-3 hover:text-fg hover:bg-bg-3 rounded transition disabled:opacity-50"
              aria-label="Close"
            >
              <X className="w-4 h-4" />
            </button>
          </div>

          <div className="px-4 py-4 space-y-3">
            <div
              onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
              onDragLeave={() => setDragOver(false)}
              onDrop={onDrop}
              onClick={() => fileInputRef.current?.click()}
              className={`border-2 border-dashed rounded-md p-4 text-center cursor-pointer transition ${
                dragOver
                  ? "border-accent bg-accent-soft"
                  : "border-line hover:border-fg-3 hover:bg-bg-2"
              }`}
              role="button"
              tabIndex={0}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  fileInputRef.current?.click();
                }
              }}
            >
              {source.kind === "file" ? (
                <div className="flex items-center gap-3 text-left">
                  <img
                    src={source.previewUrl}
                    alt=""
                    className="w-16 h-16 rounded object-cover border border-line shrink-0"
                  />
                  <div className="flex-1 min-w-0">
                    <div className="text-sm font-medium truncate" title={source.file.name}>
                      {source.file.name}
                    </div>
                    <div className="mono text-xs text-fg-4 mt-0.5">
                      {(source.file.size / 1024).toFixed(1)} KB · {source.file.type}
                    </div>
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        URL.revokeObjectURL(source.previewUrl);
                        setSource({ kind: "empty" });
                      }}
                      className="text-xs text-accent hover:underline mt-1"
                    >
                      Replace
                    </button>
                  </div>
                </div>
              ) : (
                <div className="text-sm text-fg-3">
                  <Upload className="w-5 h-5 mx-auto mb-1.5 text-fg-4" />
                  <div>
                    <span className="text-fg">Click</span>, drop, or paste an image
                  </div>
                  <div className="text-xs text-fg-4 mt-0.5">
                    PNG, JPEG, WEBP — up to 25 MB. Auto-converted to JPEG.
                  </div>
                </div>
              )}
              <input
                ref={fileInputRef}
                type="file"
                accept="image/*"
                onChange={onFileChange}
                className="hidden"
              />
            </div>

            <div className="flex items-center gap-2 text-xs text-fg-4 mono">
              <span className="flex-1 border-t border-line" />
              or
              <span className="flex-1 border-t border-line" />
            </div>

            <label className="block">
              <div className="mono text-[11px] uppercase tracking-[0.06em] text-fg-4 mb-1 flex items-center gap-1">
                <LinkIcon className="w-3 h-3" />
                Paste an image URL
              </div>
              <input
                type="url"
                value={source.kind === "url" ? source.url : ""}
                onChange={(e) => {
                  const url = e.target.value;
                  if (source.kind === "file") URL.revokeObjectURL(source.previewUrl);
                  setSource(url ? { kind: "url", url } : { kind: "empty" });
                }}
                placeholder="https://i.ebayimg.com/images/g/…"
                className="w-full bg-bg border border-line rounded-md px-3 py-2 text-sm placeholder:text-fg-4 focus:outline-none focus:border-accent transition"
              />
            </label>
          </div>

          <div className="flex items-center justify-end gap-2 px-4 h-12 border-t border-line bg-panel-2 rounded-b-lg">
            <button
              onClick={onClose}
              disabled={submitting}
              className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition disabled:opacity-50"
            >
              Cancel
            </button>
            <button
              onClick={onApply}
              disabled={source.kind === "empty" || submitting}
              className="h-8 px-3 rounded bg-accent text-accent-fg font-medium text-sm hover:opacity-90 transition disabled:opacity-50 inline-flex items-center gap-1.5"
            >
              {submitting ? (
                <><Loader2 className="w-3.5 h-3.5 spin" /> Applying…</>
              ) : (
                <><Upload className="w-3.5 h-3.5" /> Apply cover</>
              )}
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
