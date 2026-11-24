import { Check, ChevronDown, Keyboard, RefreshCw } from "lucide-react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useLocation } from "react-router-dom";
import { useState } from "react";

import { useCommands } from "@/hooks/useCommands";
import type { CommandId } from "@/lib/commands";
import ActivityButton from "./ActivityButton";

const ROUTE_LABELS: { match: (path: string) => boolean; label: string }[] = [
  { match: (p) => p === "/" || p.startsWith("/library"), label: "Library" },
  { match: (p) => p.startsWith("/review"), label: "Review" },
  { match: (p) => p.startsWith("/settings"), label: "Settings" },
];

/**
 * Titlebar — 36px tall, full-width chrome: brand, the current page name,
 * the Activity control (all background progress), Rescan and the View menu.
 * Library's own controls (search, sort, Tiles/List, tile size) sit together
 * above the collection (FilterBar); Settings is the sidebar's destination.
 *
 * Every control runs a command of the registry (lib/commands.ts), the same
 * one the macOS menu bar runs, so they are available at the same moments.
 * The View menu is the window's own way to the commands of the native View
 * and Help menus (Windows and Linux have no menu bar).
 *
 * Narrow windows: the header never makes the page wider than the window
 * (`min-w-0` lets the app grid shrink it). The location crumb shrinks
 * first; below Tailwind's `lg` (1024 px) Rescan shows only its icon. The
 * Activity control truncates its text.
 */
export default function Titlebar() {
  const location = useLocation();
  const commands = useCommands();
  const { state } = commands;
  const [rescanning, setRescanning] = useState(false);

  const onRescan = async () => {
    if (rescanning) return;
    setRescanning(true);
    try {
      // Progress shows in the Activity control wherever the user is.
      await commands.run("library.rescan");
    } finally {
      setRescanning(false);
    }
  };

  const crumb = ROUTE_LABELS.find((r) => r.match(location.pathname))?.label ?? "";

  const busy = state.scanning || rescanning;
  const rescanDisabled = rescanning || !commands.enabled("library.rescan");
  const rescanTitle =
    state.library !== "available"
      ? "Configure a library directory in Settings first"
      : busy
        ? "A scan is running"
        : "Rescan the library";

  return (
    <header className="flex items-center gap-3 min-w-0 overflow-hidden h-[var(--titlebar-h)] px-3.5 border-b border-line bg-panel-2 select-none relative z-10">
      {/* Brand */}
      <div className="flex items-center gap-2.5 font-semibold text-md tracking-tight shrink-0">
        <BrandMark />
        <span>Musaic</span>
      </div>

      {/* Breadcrumb / current location */}
      <div className="flex items-center gap-1.5 mono text-sm text-fg-3 ml-2 min-w-0 truncate max-w-md">
        <span className="text-fg-2 truncate">{crumb}</span>
      </div>

      <div className="flex-1" />

      {/* Activity: scans, checks, fingerprinting, bulk fixes and jobs, in
          one control that opens the Activity panel over this page. */}
      <ActivityButton />

      {/* Rescan. It keeps its name while a scan runs and is only disabled:
          the Activity control beside it is the one place that reports
          progress, so the toolbar never says "Scanning" twice. */}
      <button
        onClick={onRescan}
        aria-label="Rescan"
        disabled={rescanDisabled}
        className="inline-flex shrink-0 items-center gap-2 h-8 px-2.5 lg:px-3 border border-line rounded-md bg-panel text-fg-2 text-sm hover:bg-bg-3 hover:text-fg transition disabled:opacity-60 disabled:cursor-not-allowed"
        title={rescanTitle}
      >
        <RefreshCw className="w-3.5 h-3.5" />
        <span className="hidden lg:inline">Rescan</span>
      </button>

      <ViewMenu commands={commands} />
    </header>
  );
}

type Commands = ReturnType<typeof useCommands>;

/** View → Show Activity, Appearance; Help → Keyboard Shortcuts. */
function ViewMenu({ commands }: { commands: Commands }) {
  const item = (id: CommandId, icon?: React.ReactNode) => {
    const checked = commands.checked(id);
    return (
      <DropdownMenu.Item
        key={id}
        disabled={!commands.enabled(id)}
        onSelect={() => void commands.run(id)}
        role={checked === undefined ? "menuitem" : "menuitemradio"}
        aria-checked={checked === undefined ? undefined : checked}
        className="flex items-center gap-2 px-2 py-1.5 rounded-sm cursor-pointer outline-none text-fg-2 data-[highlighted]:bg-bg-3 data-[disabled]:opacity-50 data-[disabled]:cursor-default"
      >
        <span className="w-3 h-3 grid place-items-center shrink-0">
          {checked ? <Check className="w-3 h-3" /> : icon}
        </span>
        <span>{commands.label(id)}</span>
      </DropdownMenu.Item>
    );
  };
  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          className="inline-flex shrink-0 items-center gap-1 h-8 px-2.5 border border-line rounded-md bg-panel text-fg-2 text-sm hover:bg-bg-3 hover:text-fg transition focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
          title="Activity, appearance and keyboard shortcuts"
        >
          View
          <ChevronDown className="w-3.5 h-3.5 text-fg-3" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="end"
          sideOffset={4}
          className="z-50 min-w-[12rem] rounded-md border border-line bg-panel shadow-popup p-1 text-sm"
        >
          {item("view.activity")}
          <DropdownMenu.Separator className="my-1 h-px bg-line" />
          <DropdownMenu.Label className="px-2 pt-1 pb-0.5 mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4">
            Appearance
          </DropdownMenu.Label>
          {item("appearance.light")}
          {item("appearance.dark")}
          <DropdownMenu.Separator className="my-1 h-px bg-line" />
          {item("help.shortcuts", <Keyboard className="w-3 h-3" />)}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

function BrandMark() {
  return (
    <div
      className="w-[18px] h-[18px] rounded-[4px] bg-accent grid place-items-center text-accent-fg font-bold text-[11px] relative"
      aria-hidden
    >
      M
      {/* Faux record-label ring */}
      <span className="absolute inset-[3px] border rounded-full" style={{ borderColor: "color-mix(in oklch, var(--accent-fg), transparent 60%)" }} />
      <span className="absolute w-[3px] h-[3px] bg-accent-fg rounded-full" />
    </div>
  );
}
