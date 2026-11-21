import { useEffect } from "react";

import { useCommands } from "@/hooks/useCommands";
import { getCommand, type CommandId } from "@/lib/commands";
import { hasNativeMenu, isDesktopWindow } from "@/lib/desktop";

/** The field `/` focuses: the one that declares the key with aria-keyshortcuts. */
export const SEARCH_TARGET = 'input[aria-keyshortcuts="/"]';

const TYPING = 'input, textarea, select, [contenteditable]:not([contenteditable="false"])';

/** Single keys, outside text fields: `,` Settings, `?` Keyboard Shortcuts. */
const SINGLE_KEYS: Record<string, CommandId> = {
  ",": "app.settings",
  "?": "help.shortcuts",
};

/**
 * App-wide shortcuts, mounted once inside the router. Every one runs a
 * command of the registry (lib/commands.ts), so it is available exactly
 * when the native menu item and the on-screen control are.
 *
 *  - `/` focuses the Library search field where the page has one, `,`
 *    opens Settings and `?` lists the shortcuts. None acts while the user
 *    is typing (a field would lose the character), while a modal dialog is
 *    open (it owns the keyboard), or with Ctrl, Cmd or Alt held (those
 *    belong to the browser and the OS). The search field is found through
 *    its `aria-keyshortcuts` so the hint and the key cannot drift apart;
 *    where the page has no such field `/` does nothing and is not
 *    swallowed.
 *  - The menu shortcuts (Cmd/Ctrl with `,` `F` `1` `2`) are the native
 *    menu's key equivalents on macOS. In the desktop window on Windows and
 *    Linux, which has no menu bar, they are handled here with Ctrl, also
 *    from a text field, as a menu key would be. In a plain browser they
 *    stay the browser's (find, switch tab).
 */
export function useGlobalShortcuts() {
  const { run } = useCommands();

  useEffect(() => {
    const modKeys = new Map<string, CommandId>();
    for (const id of ["app.settings", "library.find", "view.library", "view.review"] as const) {
      const k = getCommand(id).modKey;
      if (k) modKeys.set(k.toLowerCase(), id);
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing) return;
      if (e.altKey) return;
      if (e.ctrlKey || e.metaKey) {
        if (!isDesktopWindow() || hasNativeMenu() || e.shiftKey) return;
        const id = modKeys.get(e.key.toLowerCase());
        if (!id) return;
        e.preventDefault();
        void run(id);
        return;
      }
      if (e.key !== "/" && !(e.key in SINGLE_KEYS)) return;
      const target = e.target instanceof Element ? e.target : null;
      if (target?.closest(TYPING)) return;
      if (document.querySelector('[role="dialog"], [role="alertdialog"]')) return;

      if (e.key === "/") {
        const field = document.querySelector<HTMLInputElement>(SEARCH_TARGET);
        if (!field) return;
        // Without this the slash would be typed into the field just focused.
        e.preventDefault();
        field.focus();
        field.select();
        return;
      }
      e.preventDefault();
      void run(SINGLE_KEYS[e.key]);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [run]);
}
