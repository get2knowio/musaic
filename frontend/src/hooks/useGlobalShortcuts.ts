import { useEffect } from "react";
import { useNavigate } from "react-router-dom";

/** The field `/` focuses: the one that declares the key with aria-keyshortcuts. */
const SEARCH_TARGET = 'input[aria-keyshortcuts="/"]';

/**
 * App-wide single-key shortcuts: `/` focuses the Library filter field and `,`
 * opens Settings. Mounted once, inside the router.
 *
 * Neither key acts while the user is typing (a field would lose the
 * character), while a modal dialog is open (it owns the keyboard), or with
 * Ctrl, Cmd or Alt held (those belong to the browser and the OS). The search
 * field is found through its `aria-keyshortcuts` rather than a ref so the
 * hint, the announcement and the key cannot drift apart; where the page has
 * no such field (anything but Library) `/` does nothing and is not swallowed.
 */
export function useGlobalShortcuts() {
  const navigate = useNavigate();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing) return;
      if (e.key !== "/" && e.key !== ",") return;
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      const target = e.target instanceof Element ? e.target : null;
      if (target?.closest('input, textarea, select, [contenteditable]:not([contenteditable="false"])')) return;
      if (document.querySelector('[role="dialog"], [role="alertdialog"]')) return;

      if (e.key === "/") {
        const field = document.querySelector<HTMLInputElement>(SEARCH_TARGET);
        if (!field) return;
        // Without this the slash would be typed into the field just focused.
        e.preventDefault();
        field.focus();
        field.select();
      } else {
        e.preventDefault();
        navigate("/settings");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [navigate]);
}
