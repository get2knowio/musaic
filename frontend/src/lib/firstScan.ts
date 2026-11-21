/**
 * The first scan of a newly chosen library folder is reported on the
 * Library page (what it found, whether checks are running, Review
 * findings), once. Saving a folder marks it; the report's Dismiss, or
 * choosing to review, ends it. Kept in sessionStorage so a visit to
 * another page in between does not lose it; without storage there is just
 * no report.
 */
const KEY = "mm.library.firstScan";

export function markFirstScan(): void {
  try {
    // When it was marked: only a scan that ends after this is reported.
    sessionStorage.setItem(KEY, String(Date.now()));
  } catch {
    // No report; the library works the same.
  }
}

export function clearFirstScan(): void {
  try {
    sessionStorage.removeItem(KEY);
  } catch {
    // Nothing to clear.
  }
}

export function firstScanPending(): boolean {
  return firstScanMarkedAt() !== null;
}

/** When the pending first scan was marked (ms), or null when none is. */
export function firstScanMarkedAt(): number | null {
  try {
    const v = sessionStorage.getItem(KEY);
    if (v === null) return null;
    const at = Number(v);
    return Number.isFinite(at) ? at : 0;
  } catch {
    return null;
  }
}
