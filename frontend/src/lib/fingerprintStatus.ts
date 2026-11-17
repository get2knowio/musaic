/**
 * The fingerprinting status line in Settings, and when the one-time
 * "add an AcoustID key" prompt is due. Both are plain functions of what the
 * API already reports (the settings' "key is set" flag and the
 * fingerprint-pool status); neither ever sees a key value.
 */
import type { FingerprintPoolStatus } from "@/lib/types";

export type FingerprintTone = "ok" | "warn" | "neutral";

export interface FingerprintStatusLine {
  tone: FingerprintTone;
  text: string;
}

const albums = (n: number) => `${n} ${n === 1 ? "album" : "albums"}`;

/**
 * One sentence on where fingerprinting stands; null while the key flag is
 * not known yet (nothing is better than a wrong claim).
 */
export function fingerprintStatusLine(
  keySet: boolean | undefined,
  pool: FingerprintPoolStatus | undefined,
): FingerprintStatusLine | null {
  if (keySet === undefined) return null;

  if (!keySet) {
    const base =
      "No AcoustID key is set, so albums are not matched by their audio. Musaic still uses folder names and tags.";
    if (pool?.status === "running") {
      return {
        tone: "warn",
        text: `${base} Audio fingerprints are being computed (${progress(pool)}), but they cannot be looked up without a key.`,
      };
    }
    return { tone: "warn", text: base };
  }

  if (!pool) return null;
  switch (pool.status) {
    case "running":
      return { tone: "neutral", text: `Fingerprinting albums: ${progress(pool)}.` };
    case "done": {
      const skipped = pool.errors > 0 ? `, ${pool.errors} skipped` : "";
      return {
        tone: "ok",
        text: `Fingerprinting finished: ${albums(pool.completed)} checked${skipped}.`,
      };
    }
    case "cancelled":
      return {
        tone: "warn",
        text: `Fingerprinting stopped before it finished (${progress(pool)}). It starts again after the next scan.`,
      };
    case "unavailable":
      return {
        tone: "warn",
        text: `Fingerprinting could not run${pool.skip_reason ? `: ${pool.skip_reason}` : "."}`,
      };
    default:
      return {
        tone: "neutral",
        text: "Fingerprinting has not started yet. It begins after a library scan.",
      };
  }
}

function progress(pool: FingerprintPoolStatus): string {
  return pool.total > 0 ? `${pool.completed} of ${pool.total}` : `${pool.completed} so far`;
}

/**
 * The prompt shows after a scan has completed, while no key is set and it
 * has not been dismissed. Anything still unknown (undefined) hides it, so it
 * never flashes up for someone who already has a key or dismissed it.
 */
export function shouldShowAcoustidPrompt(input: {
  scannedAt: string | null | undefined;
  keySet: boolean | undefined;
  dismissed: boolean | undefined;
}): boolean {
  return !!input.scannedAt && input.keySet === false && input.dismissed === false;
}
