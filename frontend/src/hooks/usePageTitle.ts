/**
 * Sets `document.title` to "{title} · Musaic" while the calling page is
 * mounted, restoring the previous title on unmount. Pages call it once near
 * the top of their component body so users with multiple windows open can tell
 * which is which.
 */
import { useEffect } from "react";

const SUFFIX = " · Musaic";

export function usePageTitle(title: string): void {
  useEffect(() => {
    const previous = document.title;
    document.title = title ? `${title}${SUFFIX}` : `Musaic`;
    return () => {
      document.title = previous;
    };
  }, [title]);
}
