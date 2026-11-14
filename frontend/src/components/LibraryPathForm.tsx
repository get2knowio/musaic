import { useState } from "react";
import { FolderOpen, X } from "lucide-react";
import { toast } from "sonner";

import { useSetLibraryDir } from "@/hooks/useLibraryConfig";
import { requestScanOnNextLoad } from "@/hooks/useScanStream";
import { hasNativeFolderPicker, pickFolder } from "@/lib/desktop";
import type { AppConfig } from "@/lib/types";

/**
 * Library directory entry, shared by the Library empty state and Settings.
 *
 * - In the desktop window (lib/desktop.ts) a "Choose folder…" button opens
 *   the native folder picker and saves the chosen folder at once.
 * - Everywhere the text field stays: paste or type an absolute path, then
 *   Save (or Enter).
 *
 * Every save marks the session's scan as due (`requestScanOnNextLoad`);
 * one whose directory exists calls `onSaved`, which takes the user to the
 * grid, where the Library page opens the scan stream straight away. A
 * path that does not exist is saved with a warning and the field stays
 * open, as before.
 */
export default function LibraryPathForm({
  initial = "",
  onSaved,
  onCancel,
  inputClassName = "w-72",
}: {
  initial?: string;
  onSaved?: (config: AppConfig) => void;
  /** Shows a cancel (X) button when given. */
  onCancel?: () => void;
  inputClassName?: string;
}) {
  const setDir = useSetLibraryDir();
  const [draft, setDraft] = useState(initial);
  const [picking, setPicking] = useState(false);
  const native = hasNativeFolderPicker();
  const busy = setDir.isPending || picking;

  const save = (path: string) => {
    const trimmed = path.trim();
    if (!trimmed) return;
    // Before the request: the config query flips (and the Library page's
    // scan effect may run) before this call's onSuccess does. Harmless
    // when the save fails: the next load scans the configured directory.
    requestScanOnNextLoad();
    setDir.mutate(trimmed, {
      onSuccess: (next) => {
        if (next.exists) {
          toast.success("Library directory updated");
          onSaved?.(next);
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

  const onChoose = async () => {
    setPicking(true);
    try {
      const chosen = await pickFolder();
      if (chosen) {
        setDraft(chosen);
        save(chosen);
      }
    } catch (e) {
      toast.error("Couldn't open the folder picker", { description: (e as Error).message });
    } finally {
      setPicking(false);
    }
  };

  return (
    <>
      {native && (
        <button
          type="button"
          onClick={onChoose}
          disabled={busy}
          className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
        >
          <FolderOpen className="w-3.5 h-3.5" />
          {picking ? "Choosing…" : "Choose folder…"}
        </button>
      )}
      <input
        type="text"
        aria-label="Library directory path"
        placeholder="/Users/you/Music/FLAC"
        value={draft}
        disabled={setDir.isPending}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") save(draft);
        }}
        className={`${inputClassName} h-8 px-2 border border-line rounded bg-bg text-sm text-fg focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30 disabled:opacity-60`}
      />
      <button
        type="button"
        onClick={() => save(draft)}
        disabled={busy || !draft.trim()}
        className={
          native
            ? "h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition disabled:opacity-60"
            : "h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60"
        }
      >
        {setDir.isPending ? "Saving…" : "Save"}
      </button>
      {onCancel && (
        <button
          type="button"
          aria-label="Cancel"
          onClick={onCancel}
          className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      )}
    </>
  );
}
