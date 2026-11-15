/**
 * After a database restore the whole app reloads itself.
 *
 * Why a reload and not a cache invalidation: the server swaps
 * `library.db` in place and publishes no library event, so nothing tells
 * the open page that every album, track, finding, job, setting and
 * vocabulary row may have changed. The App-level SSE connections do
 * survive, but state outside TanStack Query does not refresh by
 * invalidation: the Library grid's scan-stream state and its once-per-
 * session scan flag (hooks/useScanStream.ts), the remembered library URL
 * (it may name an album the restored database no longer has) and the
 * open dialogs. The restore also stops the fingerprint pool, which only a
 * scan starts again. A reload resets all of it, reconnects the streams,
 * and the first Library visit runs the session's scan, which is what the
 * Python version's "refresh the page" advice did.
 *
 * The confirmation is carried across the reload in sessionStorage and
 * shown once by `takeRestoreNotice()`.
 */

const KEY = "musaic.restore.notice";

/** Reload the page; separate so tests can replace it. */
export function reloadPage(): void {
  window.location.reload();
}

/** Remember `message` for after the reload, then reload. */
export function reloadAfterRestore(message: string, reload: () => void = reloadPage): void {
  try {
    sessionStorage.setItem(KEY, message);
  } catch {
    /* storage unavailable: the reload still brings the data up to date */
  }
  reload();
}

/** The message left by `reloadAfterRestore`, once; `null` otherwise. */
export function takeRestoreNotice(): string | null {
  try {
    const m = sessionStorage.getItem(KEY);
    if (m !== null) sessionStorage.removeItem(KEY);
    return m;
  } catch {
    return null;
  }
}
