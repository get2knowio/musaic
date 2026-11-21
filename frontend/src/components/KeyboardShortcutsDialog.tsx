import * as Dialog from "@radix-ui/react-dialog";
import { X } from "lucide-react";

import { getCommand, modShortcut, type CommandId } from "@/lib/commands";
import { hasNativeMenu, isDesktopWindow } from "@/lib/desktop";
import { closeShortcutsDialog, useShortcutsDialogOpen } from "@/lib/uiRequests";

interface Row {
  keys: string[];
  what: string;
}

/** The menu commands that have a Cmd / Ctrl key, as the list names them. */
const MOD_COMMANDS: [CommandId, string][] = [
  ["app.settings", "Settings"],
  ["library.find", "Find in Library (from Review too: opens Library)"],
  ["view.library", "Show Library"],
  ["view.review", "Show Review"],
];

/**
 * The keys the app answers to. Menu keys (Cmd on macOS, Ctrl elsewhere)
 * are listed only in the desktop window, where they are wired (the native
 * menu on macOS, useGlobalShortcuts on Windows and Linux); a browser keeps
 * them for itself.
 */
function shortcutRows(): { title: string; rows: Row[] }[] {
  const menuKeys = isDesktopWindow() || hasNativeMenu();
  const commandRows: Row[] = menuKeys
    ? MOD_COMMANDS.map(([id, what]) => ({ keys: [modShortcut(getCommand(id).modKey ?? "")], what }))
    : [];
  return [
    {
      title: "Anywhere",
      rows: [
        ...commandRows,
        { keys: [","], what: "Settings" },
        { keys: ["?"], what: "Keyboard Shortcuts" },
        { keys: ["Esc"], what: "Close a dialog or the Activity panel" },
      ],
    },
    {
      title: "Library",
      rows: [
        { keys: ["/"], what: "Focus the search field" },
        { keys: ["Enter"], what: "Open the focused album in the inspector" },
        { keys: ["F6"], what: "Move focus between the album list and the inspector" },
        { keys: ["Esc"], what: "Close the inspector (back to where you opened it)" },
      ],
    },
  ];
}

/** Help → Keyboard Shortcuts (also `?`). Opens nothing that writes. */
export default function KeyboardShortcutsDialog() {
  const open = useShortcutsDialogOpen();
  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o) closeShortcutsDialog(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-[60] bg-black/45 backdrop-blur-[1px]" />
        <Dialog.Content className="fixed top-1/2 left-1/2 z-[61] -translate-x-1/2 -translate-y-1/2 w-[min(480px,95vw)] max-h-[90vh] overflow-y-auto bg-panel border border-line rounded-lg shadow-popup outline-none">
          <div className="flex items-center justify-between px-4 pt-4 pb-2">
            <Dialog.Title className="font-semibold text-md">Keyboard Shortcuts</Dialog.Title>
            <Dialog.Close
              className="w-7 h-7 grid place-items-center rounded text-fg-3 hover:text-fg hover:bg-bg-3"
              aria-label="Close"
            >
              <X className="w-4 h-4" />
            </Dialog.Close>
          </div>
          <Dialog.Description className="sr-only">The keys Musaic answers to.</Dialog.Description>
          <div className="px-4 pb-4 space-y-4">
            {shortcutRows().map((g) => (
              <section key={g.title}>
                <h3 className="mono text-[11px] uppercase tracking-[0.08em] text-fg-4 mb-1.5">{g.title}</h3>
                <dl className="text-sm">
                  {g.rows.map((r) => (
                    <div key={r.what} className="flex items-center gap-3 py-1 border-b border-line last:border-b-0">
                      <dt className="w-24 shrink-0">
                        {r.keys.map((k) => (
                          <kbd key={k} className="mono text-xs text-fg-2 border border-line bg-bg-2 px-1.5 py-0.5 rounded-sm">
                            {k}
                          </kbd>
                        ))}
                      </dt>
                      <dd className="text-fg-2">{r.what}</dd>
                    </div>
                  ))}
                </dl>
              </section>
            ))}
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
