import { useCallback, useLayoutEffect, useRef } from "react";

/**
 * For a controlled Radix dialog opened without a `Dialog.Trigger`: Radix
 * returns focus to the trigger on close, and with none it leaves focus on
 * `<body>`. This puts it back on whatever had focus when the dialog opened
 * (inside the album panel, so the panel's Esc and F6 keep working).
 *
 * Pass the result as `onCloseAutoFocus` on `Dialog.Content`.
 */
export function useReturnFocus(open: boolean): (event: Event) => void {
  const opener = useRef<HTMLElement | null>(null);
  // Layout effect: runs before Radix's focus scope moves focus into the
  // dialog (a passive effect), so this still sees the opener.
  useLayoutEffect(() => {
    if (open) opener.current = document.activeElement as HTMLElement | null;
  }, [open]);
  return useCallback((event: Event) => {
    const el = opener.current;
    opener.current = null;
    if (el && el !== document.body && el.isConnected) {
      event.preventDefault();
      el.focus();
    }
  }, []);
}
