/**
 * useScanFollower — keeps the Activity panel's scan record current while
 * the Library page is not reading the scan stream (lib/scanActivity.ts).
 *
 * Mounted once in App. It reads `GET /api/scan/status` once at startup (a
 * scan may already be running), then joins `/api/scan/stream` only for a
 * scan known to be running: one started from Review or Settings, or one
 * still running when the user left Library. Joining is what the stream is
 * for; opening it with no scan running would start one, so it is never
 * opened just to look, and nothing is polled.
 */
import { useEffect } from "react";

import { getScanStatus } from "@/lib/api";
import {
  clearScanFollow,
  getScanActivity,
  isScanActive,
  noteScanEvent,
  noteScanStatus,
  scanFollowWanted,
  scanHasOwner,
  subscribeScanActivity,
} from "@/lib/scanActivity";
import { openScanStream, type SseHandle } from "@/lib/sse";
import { noteStreamLive, noteStreamLost } from "@/lib/streamHealth";

/** Ask the server once where the scan is (startup, or after losing it). */
export async function checkScanStatus(): Promise<void> {
  try {
    noteScanStatus(await getScanStatus());
  } catch {
    // Unknown stays unknown: the record keeps what it had.
  }
}

export function useScanFollower(): void {
  useEffect(() => {
    let handle: SseHandle | null = null;
    let disposed = false;

    const stop = () => {
      handle?.close();
      handle = null;
    };

    const sync = () => {
      if (disposed) return;
      // Library reads the stream itself while it is open.
      if (scanHasOwner()) {
        stop();
        return;
      }
      if (handle || !scanFollowWanted()) return;
      handle = openScanStream("/api/scan/stream", (ev) => {
        noteStreamLive("scan");
        if (ev.type === "error" && ev.lost) noteStreamLost("scan");
        noteScanEvent(ev);
        if (!isScanActive(getScanActivity().phase)) {
          clearScanFollow();
          stop();
        }
      });
    };

    const unsubscribe = subscribeScanActivity(sync);
    void checkScanStatus();
    sync();
    return () => {
      disposed = true;
      unsubscribe();
      stop();
    };
  }, []);
}
