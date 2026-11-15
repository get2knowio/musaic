import * as Dialog from "@radix-ui/react-dialog";
import { Loader2, Trash2 } from "lucide-react";
import { useRef } from "react";

import { useReturnFocus } from "./useReturnFocus";

interface Props {
  open: boolean;
  albumTitle: string;
  removing: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * Confirms "Remove from library" for a Missing album
 * (`DELETE /api/albums/{id}`). The list says what that request really
 * drops: the album's row and the rows that cascade with it (MusicBrainz
 * blocklist, fingerprints, My Tags). It never touches the music folder.
 */
export default function RemoveAlbumDialog({
  open,
  albumTitle,
  removing,
  onConfirm,
  onCancel,
}: Props) {
  const returnFocus = useReturnFocus(open);
  const cancelRef = useRef<HTMLButtonElement>(null);

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o && !removing) onCancel(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-[60] bg-black/45 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed top-1/2 left-1/2 z-[61] -translate-x-1/2 -translate-y-1/2 w-[min(460px,95vw)] bg-panel border border-line rounded-lg shadow-popup outline-none flex flex-col"
          // A destructive confirm starts on Cancel, so Enter does not remove.
          onOpenAutoFocus={(e) => {
            e.preventDefault();
            cancelRef.current?.focus();
          }}
          onCloseAutoFocus={returnFocus}
        >
          <div className="px-4 pt-4 pb-3.5">
            <Dialog.Title className="font-semibold text-md break-words">
              Remove “{albumTitle}” from Musaic?
            </Dialog.Title>
            <Dialog.Description asChild>
              <div className="text-sm text-fg-2 mt-2 leading-relaxed space-y-2">
                <p>This deletes Musaic’s record of the album from its database:</p>
                <ul className="list-disc pl-5 text-fg-3 space-y-0.5">
                  <li>the album details shown in this panel</li>
                  <li>its MusicBrainz match, and the matches you rejected for it</li>
                  <li>its audio fingerprints</li>
                  <li>the My Tags on its tracks</li>
                </ul>
                <p>
                  Your music files and folders are not touched. If the folder
                  comes back, the next scan adds it as a new album, without its
                  previous MusicBrainz match and fingerprints.
                </p>
              </div>
            </Dialog.Description>
          </div>
          <div className="flex items-center justify-end gap-2 px-4 h-12 border-t border-line bg-panel-2 rounded-b-lg">
            <button
              ref={cancelRef}
              type="button"
              onClick={onCancel}
              disabled={removing}
              className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition disabled:opacity-60"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={onConfirm}
              disabled={removing}
              className="h-8 px-3 rounded border border-err/40 bg-err-soft text-err text-sm font-medium hover:bg-err/15 transition disabled:opacity-60 inline-flex items-center gap-1.5"
            >
              {removing ? (
                <><Loader2 className="w-3.5 h-3.5 spin" /> Removing…</>
              ) : (
                <><Trash2 className="w-3.5 h-3.5" /> Remove from library</>
              )}
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
