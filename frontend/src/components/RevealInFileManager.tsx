/**
 * "Show in Finder" (Explorer / file manager) control, desktop window only
 * Renders nothing in a plain browser, or when the shell's
 * marker does not advertise the reveal endpoints (lib/desktop.ts).
 */
import { useState } from "react";
import { FolderOpen, Loader2 } from "lucide-react";
import { toast } from "sonner";

import { revealAlbum } from "@/lib/commands";
import { canRevealInFileManager, revealLabel, revealTrash } from "@/lib/desktop";

export type RevealTarget = { kind: "album"; albumId: string } | { kind: "trash" };

interface Props {
  target: RevealTarget;
  className?: string;
}

export default function RevealInFileManager({ target, className = "" }: Props) {
  const [busy, setBusy] = useState(false);
  if (!canRevealInFileManager()) return null;
  const label = revealLabel();
  const onClick = async () => {
    if (busy) return;
    setBusy(true);
    try {
      // An album goes the way Album → Show in Finder goes (lib/commands.ts).
      await (target.kind === "album" ? revealAlbum(target.albumId) : revealTrash());
    } catch (e) {
      toast.error((e as Error).message || "Couldn't open the folder");
    } finally {
      setBusy(false);
    }
  };
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={busy}
      title={target.kind === "album" ? `${label}: this album's folder` : `${label}: the app's trash folder`}
      className={`text-xs text-fg-4 hover:text-fg-2 transition disabled:opacity-60 inline-flex items-center gap-1 whitespace-nowrap ${className}`}
    >
      {busy ? <Loader2 className="w-3 h-3 spin" /> : <FolderOpen className="w-3 h-3" />}
      {label}
    </button>
  );
}
