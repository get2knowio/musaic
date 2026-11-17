import { useState } from "react";
import { FolderOpen, X } from "lucide-react";
import { toast } from "sonner";

import { useSetLibraryDir } from "@/hooks/useLibraryConfig";
import { requestScanOnNextLoad } from "@/hooks/useScanStream";
import { hasNativeFolderPicker, pickFolder } from "@/lib/desktop";
import { folderRefusalText } from "@/lib/folderRefusal";
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
 * a saved folder calls `onSaved`, which takes the user to the grid, where
 * the Library page opens the scan stream straight away. The server refuses
 * a path that is not a readable folder and keeps the folder already in
 * use; the reason is shown under the field and the user can try again.
 * Put the form in a wrapping flex row: the reason takes a full line.
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
  const [refusal, setRefusal] = useState<string | null>(null);
  const native = hasNativeFolderPicker();
  const busy = setDir.isPending || picking;

  const save = (path: string) => {
    const trimmed = path.trim();
    if (!trimmed) return;
    // Before the request: the config query flips (and the Library page's
    // scan effect may run) before this call's onSuccess does. Harmless
    // when the save fails: the next load scans the configured directory.
    requestScanOnNextLoad();
    setRefusal(null);
    setDir.mutate(trimmed, {
      onSuccess: (next) => {
        toast.success("Library directory updated");
        onSaved?.(next);
      },
      onError: (e) => setRefusal(folderRefusalText((e as Error).message)),
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
        aria-invalid={refusal ? true : undefined}
        aria-describedby={refusal ? "library-path-refusal" : undefined}
        onChange={(e) => {
          setDraft(e.target.value);
          setRefusal(null);
        }}
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
      {refusal && (
        <p
          id="library-path-refusal"
          role="alert"
          className="basis-full text-xs text-err leading-relaxed text-left"
        >
          {refusal}
        </p>
      )}
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
