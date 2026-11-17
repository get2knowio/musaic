import { Grid, List, Loader2, Moon, RefreshCw, Search, Settings, Sun } from "lucide-react";
import { Link, useLocation, useSearchParams } from "react-router-dom";
import { useState, useSyncExternalStore } from "react";
import { toast } from "sonner";

import { setTheme } from "@/lib/theme";

import { useLibraryConfig } from "@/hooks/useLibraryConfig";
import {
  COLUMN_MAX,
  COLUMN_MIN,
  setTileColumns,
  useTileColumns,
} from "@/hooks/useTileColumns";
import {
  getScanning,
  subscribeScanning,
  triggerRescan,
} from "@/lib/rescanController";
import CheckSweepPill from "./CheckSweepPill";
import FingerprintPoolPill from "./FingerprintPoolPill";

type ViewMode = "tiles" | "list";

function readView(params: URLSearchParams): ViewMode {
  return params.get("view") === "list" ? "list" : "tiles";
}

const ROUTE_LABELS: { match: (path: string) => boolean; label: string }[] = [
  { match: (p) => p === "/" || p.startsWith("/library"), label: "Library" },
  { match: (p) => p.startsWith("/attention"), label: "Needs attention" },
  { match: (p) => p.startsWith("/settings"), label: "Settings" },
];

/**
 * Titlebar — 36px tall, full-width chrome with brand, the current page name,
 * and (on Library only) search and the view toggle, then rescan, theme
 * toggle and settings. The library folder is shown once, in the Library
 * filter bar, not here.
 *
 * Narrow windows: the header never makes the page wider than the window
 * (`min-w-0` lets the app grid shrink it). The location crumb and the search
 * box shrink first; below Tailwind's `lg` (1024 px) the tile-size slider is
 * hidden and Rescan shows only its icon. The fingerprint pill already hides
 * below `md`.
 *
 * The Rescan button delegates to the Library page's `useScanStream.rescan`
 * via the rescanController module-level registry. When Library isn't
 * mounted, it falls back to a bare API call + toast pointing the user at
 * the Library page so they can watch the progress.
 */
export default function Titlebar() {
  const [params, setParams] = useSearchParams();
  const location = useLocation();
  const onLibrary = location.pathname.startsWith("/library");
  const query = params.get("q") ?? "";
  const view = readView(params);
  const config = useLibraryConfig();

  const scanning = useSyncExternalStore(subscribeScanning, getScanning, getScanning);
  const [rescanning, setRescanning] = useState(false);

  const onSearchChange = (value: string) => {
    const next = new URLSearchParams(params);
    if (value) next.set("q", value);
    else next.delete("q");
    setParams(next, { replace: true });
  };

  const setView = (next: ViewMode) => {
    const p = new URLSearchParams(params);
    // Default is tiles, so we omit the param when it's the default to keep
    // URLs clean. ?view=list is sticky across navigation via the URL.
    if (next === "list") p.set("view", "list");
    else p.delete("view");
    setParams(p, { replace: true });
  };

  const onRescan = async () => {
    if (rescanning || scanning) return;
    setRescanning(true);
    try {
      const { via } = await triggerRescan();
      if (via === "api") {
        toast("Rescan started in the background", {
          description: "Open Library to watch the progress.",
          duration: 4000,
        });
      }
    } catch (err) {
      const e = err as Error;
      toast.error(e.message || "Couldn't start rescan");
    } finally {
      setRescanning(false);
    }
  };

  const crumb =
    ROUTE_LABELS.find((r) => r.match(location.pathname))?.label ?? "";

  const rescanDisabled = rescanning || scanning || !config.data?.exists;
  const rescanTitle = !config.data?.exists
    ? "Configure a library directory in Settings first"
    : scanning || rescanning
    ? "Scanning…"
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

      {/* Background-pool status pill — only visible while the eager
          fingerprinting pool is actively running. Auto-hides otherwise. */}
      <FingerprintPoolPill />

      {/* Background health-check sweep — only visible while it has work. */}
      <CheckSweepPill />

      {/* Search and the view toggle only act on the Library page, so they
          only show there. Search writes ?q=… to the URL for Library to filter. */}
      {onLibrary && (
      <div className="flex items-center gap-2 bg-panel border border-line rounded-md px-2.5 h-8 w-80 min-w-32 shrink focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--accent-soft)] transition">
        <Search className="w-3.5 h-3.5 text-fg-3 shrink-0" />
        <input
          type="search"
          value={query}
          onChange={(e) => onSearchChange(e.target.value)}
          placeholder="Filter albums and artists…"
          className="bg-transparent border-0 outline-0 flex-1 min-w-0 text-sm placeholder:text-fg-4"
        />
      </div>
      )}

      {/* Tile-size slider — only meaningful when viewing the Library
          tile grid. List view ignores the column count, so we hide
          rather than disable to keep the chrome tight. */}
      {onLibrary && view === "tiles" && <TileColumnsSlider />}

      {/* View toggle */}
      {onLibrary && (
      <div className="inline-flex shrink-0 border border-line rounded-md h-8 overflow-hidden bg-panel">
        <button
          className={`px-2.5 text-sm flex items-center gap-1 ${view === "tiles" ? "bg-bg-3 text-fg" : "text-fg-3"}`}
          onClick={() => setView("tiles")}
          title="Tile view"
        >
          <Grid className="w-3.5 h-3.5" />
        </button>
        <button
          className={`px-2.5 text-sm flex items-center gap-1 border-l border-line ${view === "list" ? "bg-bg-3 text-fg" : "text-fg-3"}`}
          onClick={() => setView("list")}
          title="List view"
        >
          <List className="w-3.5 h-3.5" />
        </button>
      </div>
      )}

      {/* Rescan */}
      <button
        onClick={onRescan}
        aria-label={scanning || rescanning ? "Scanning…" : "Rescan"}
        disabled={rescanDisabled}
        className="inline-flex shrink-0 items-center gap-2 h-8 px-2.5 lg:px-3 border border-line rounded-md bg-panel text-fg-2 text-sm hover:bg-bg-3 hover:text-fg transition disabled:opacity-60 disabled:cursor-not-allowed"
        title={rescanTitle}
      >
        {scanning || rescanning ? (
          <Loader2 className="w-3.5 h-3.5 spin" />
        ) : (
          <RefreshCw className="w-3.5 h-3.5" />
        )}
        <span className="hidden lg:inline">{scanning || rescanning ? "Scanning…" : "Rescan"}</span>
      </button>

      {/* Theme toggle */}
      <ThemeToggle />

      {/* Settings */}
      <Link
        to="/settings"
        className="inline-flex shrink-0 items-center justify-center w-8 h-8 border border-line rounded-md bg-panel text-fg-2 hover:bg-bg-3 hover:text-fg transition"
        title="Settings"
        aria-label="Settings"
      >
        <Settings className="w-3.5 h-3.5" />
      </Link>
    </header>
  );
}

/**
 * 5-stop slider for choosing how many tile columns the Library grid
 * shows. Default (no preference set) uses the responsive auto-fill
 * layout — slider exposes 3..7 fixed-column overrides; a visible
 * "×" next to the number puts it back to auto.
 */
function TileColumnsSlider() {
  const columns = useTileColumns();
  // The slider visually represents 3-7. When `columns` is null (auto),
  // anchor the thumb at 5 (the typical auto-fill outcome on a wide
  // viewport) so the control isn't sitting empty.
  const value = columns ?? 5;

  return (
    <div className="hidden lg:inline-flex shrink-0 items-center gap-2 h-8 px-2.5 border border-line rounded-md bg-panel">
      <span className="mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4">
        Size
      </span>
      <input
        type="range"
        min={COLUMN_MIN}
        max={COLUMN_MAX}
        step={1}
        value={value}
        onChange={(e) => setTileColumns(parseInt(e.target.value, 10))}
        className="w-20 accent-accent cursor-pointer"
        aria-label={`Tiles per row (currently ${columns === null ? "auto" : columns})`}
        title={columns === null ? "Auto — drag to set fixed column count" : `${columns} columns per row`}
      />
      {columns === null ? (
        <span className="mono text-[11px] text-fg-3 w-9 text-center">Auto</span>
      ) : (
        <button
          type="button"
          onClick={() => setTileColumns(null)}
          className="mono text-[11px] tabular-nums text-fg-3 hover:text-fg w-9 text-center cursor-pointer"
          title="Back to automatic tile size"
          aria-label={`${columns} per row. Back to automatic tile size`}
        >
          {columns} ×
        </button>
      )}
    </div>
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

function ThemeToggle() {
  const [dark, setDark] = useState(() => document.documentElement.classList.contains("dark"));
  return (
    <button
      className="inline-flex shrink-0 items-center justify-center w-6 h-6 border border-line rounded-[5px] bg-panel text-fg-2 hover:bg-bg-3 hover:text-fg transition"
      title="Toggle theme"
      onClick={() => {
        const next = !dark;
        setTheme(next ? "dark" : "light");
        setDark(next);
      }}
    >
      {dark ? <Sun className="w-3 h-3" /> : <Moon className="w-3 h-3" />}
    </button>
  );
}
