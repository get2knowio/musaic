/**
 * The command registry (`lib/commands.ts`) wired to the page: the state its
 * availability rules read, a `run` that every control calls, and the host
 * (mounted once in App) that takes commands from the native menu and tells
 * the menu what is available.
 */
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { useLocation, useNavigate } from "react-router-dom";

import { useAlbumsSnapshot } from "@/hooks/useAlbumsSnapshot";
import { useLibraryConfig } from "@/hooks/useLibraryConfig";
import { useRememberedLibraryUrl } from "@/hooks/useRememberedLibraryUrl";
import {
  COMMAND_EVENT,
  commandChecked,
  commandEnabled,
  getCommand,
  isCommandId,
  menuState,
  runCommand,
  type CommandId,
  type CommandState,
} from "@/lib/commands";
import { canRevealInFileManager, hasNativeMenu, postMenuState } from "@/lib/desktop";
import { getScanning, subscribeScanning } from "@/lib/rescanController";
import { isScanActive, useScanActivity } from "@/lib/scanActivity";
import { useTheme } from "@/lib/theme";

const MODAL_SELECTOR = '[role="dialog"], [role="alertdialog"]';

/** Whether a modal dialog is open right now (read from the DOM). */
export function modalIsOpen(): boolean {
  return typeof document !== "undefined" && document.querySelector(MODAL_SELECTOR) !== null;
}

/** Whether a modal dialog is open, re-rendering when one opens or closes. */
function useModalOpen(): boolean {
  const [open, setOpen] = useState(modalIsOpen);
  useEffect(() => {
    // The observer already batches mutations per task, and one selector
    // query per batch is cheap. (Not requestAnimationFrame: a hidden or
    // occluded window may not run frames, and the menu must still dim.)
    const obs = new MutationObserver(() => setOpen(modalIsOpen()));
    obs.observe(document.body, { childList: true, subtree: true });
    return () => obs.disconnect();
  }, []);
  return open;
}

/** The album id an inspector route names, or null. */
export function selectedAlbumId(pathname: string): string | null {
  const m = /^\/library\/([^/]+)\/?$/.exec(pathname);
  return m ? decodeURIComponent(m[1]) : null;
}

/** What the registry's availability rules read, from the page's state. */
export function useCommandState(): CommandState {
  const location = useLocation();
  const libraryUrl = useRememberedLibraryUrl();
  const config = useLibraryConfig();
  const { data: albums } = useAlbumsSnapshot();
  const rescanning = useSyncExternalStore(subscribeScanning, getScanning, getScanning);
  const scan = useScanActivity();
  const modalOpen = useModalOpen();
  const theme = useTheme();

  const albumId = selectedAlbumId(location.pathname);
  const album = albumId ? (albums?.find((a) => a.id === albumId) ?? null) : null;
  const library = config.data ? (config.data.exists ? "available" : "unavailable") : "unknown";

  return useMemo<CommandState>(
    () => ({
      pathname: location.pathname,
      search: location.search,
      libraryUrl,
      library,
      scanning: rescanning || isScanActive(scan.phase),
      modalOpen,
      canReveal: canRevealInFileManager(),
      theme,
      selection: albumId ? { id: albumId, album: album ? { id: album.id, missing_since: album.missing_since } : null } : null,
    }),
    [location.pathname, location.search, libraryUrl, library, rescanning, scan.phase, modalOpen, theme, albumId, album],
  );
}

/**
 * The registry for a control: whether each command is available or
 * checked now, and `run`, which re-reads the modal state at the moment it
 * runs (a dialog may have opened since the last render).
 */
export function useCommands() {
  const state = useCommandState();
  const navigate = useNavigate();
  const stateRef = useRef(state);
  useLayoutEffect(() => {
    stateRef.current = state;
  }, [state]);

  const run = useCallback(
    (id: CommandId) =>
      runCommand(id, {
        state: { ...stateRef.current, modalOpen: modalIsOpen() },
        navigate: (to, opts) => navigate(to, opts),
      }),
    [navigate],
  );
  return {
    state,
    run,
    enabled: (id: CommandId) => commandEnabled(id, state),
    checked: (id: CommandId) => commandChecked(id, state),
    label: (id: CommandId) => getCommand(id).label(state),
  };
}

/**
 * Mounted once in App: runs the commands the native menu sends (menu.rs),
 * and keeps the menu's enabled / checked state in step with the page.
 */
export function useCommandHost(): void {
  const { state, run } = useCommands();

  useEffect(() => {
    const onCommand = (e: Event) => {
      const id = (e as CustomEvent<unknown>).detail;
      if (isCommandId(id)) void run(id);
    };
    window.addEventListener(COMMAND_EVENT, onCommand);
    return () => window.removeEventListener(COMMAND_EVENT, onCommand);
  }, [run]);

  const posted = useRef<string | null>(null);
  useEffect(() => {
    if (!hasNativeMenu()) return;
    const items = menuState(state);
    const key = JSON.stringify(items);
    if (key === posted.current) return;
    posted.current = key;
    void postMenuState(items);
  }, [state]);
}
