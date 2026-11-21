import { Navigate, Route, Routes } from "react-router-dom";
import { Toaster } from "sonner";

import LibraryPage from "@/pages/Library";
import ReviewPage from "@/pages/Review";
import { AttentionRedirect, HealthRedirect, JobsRedirect } from "@/pages/reviewRedirects";
import SettingsPage from "@/pages/Settings";
import Titlebar from "@/components/Titlebar";
import Sidebar from "@/components/Sidebar";
import { useJobsStream } from "@/hooks/useJobsStream";
import { useChecksStream } from "@/hooks/useChecksStream";
import { useFingerprintPoolStream } from "@/hooks/useFingerprintPoolStream";
import { useLibraryEventsStream } from "@/hooks/useLibraryEventsStream";
import { useGlobalShortcuts } from "@/hooks/useGlobalShortcuts";
import { useSyncUiPrefs } from "@/hooks/useUiPrefs";
import { useTrackLibraryUrl } from "@/hooks/useRememberedLibraryUrl";
import { useScanFollower } from "@/hooks/useScanFollower";
import { useActivityLinks } from "@/hooks/useActivityLinks";
import ActivityPanel from "@/components/ActivityPanel";
import KeyboardShortcutsDialog from "@/components/KeyboardShortcutsDialog";
import { useCommandHost } from "@/hooks/useCommands";

/**
 * Top-level layout: titlebar across the top, sidebar on the left, route
 * outlet fills the main canvas. Mirrors the handoff's grid:
 *   [titlebar 36px]
 *   [sidebar 188px | main 1fr]
 */
export default function App() {
  // Single subscriber to the AI jobs SSE stream — keeps the cached jobs
  // query fresh and fires Sonner toasts on completion / failure transitions.
  useJobsStream();

  // Single subscriber to the health-checks SSE stream — keeps the cached
  // checks summary + open findings fresh as runs / ignores happen.
  useChecksStream();

  // Single subscriber to the fingerprint-pool SSE stream — mirrors
  // backend pool state into the cache so the Activity panel renders
  // live progress without polling.
  useFingerprintPoolStream();

  // Single subscriber to the library_events SSE stream — owns cache
  // invalidation for the whole app. Mutation handlers don't repeat
  // the invalidate-checklist; they trigger backend mutations and the
  // bus events round-trip back here to invalidate the right keys.
  // See docs/connectivity_design.md.
  useLibraryEventsStream();

  // The theme and tile size live on the server (the desktop window's
  // address changes on every launch); show them once they arrive.
  useSyncUiPrefs();

  // `/` focuses the Library filter, `,` opens Settings, `?` lists the keys;
  // Ctrl-F / 1 / 2 / , in the desktop window on Windows and Linux.
  useGlobalShortcuts();

  // The macOS menu bar's commands arrive here, and the menu's enabled
  // state follows the page (lib/commands.ts, desktop menu.rs).
  useCommandHost();

  // The Activity panel's scan record while Library is not reading the scan
  // stream (a rescan started elsewhere, a scan left running).
  useScanFollower();

  // `?activity=…` links open the Activity panel over the current page.
  useActivityLinks();

  // Save the user's last /library URL to sessionStorage so the Sidebar's
  // "Library" link can restore the same filter / search / open album when
  // they come back from /review, /settings, etc.
  useTrackLibraryUrl();

  return (
    <div className="grid h-screen grid-rows-[var(--titlebar-h)_1fr] bg-bg text-fg overflow-hidden">
      <Titlebar />
      <div className="grid grid-cols-[var(--sidebar-w)_1fr] min-h-0">
        <Sidebar />
        <main className="relative min-h-0 flex flex-col overflow-hidden">
          <Routes>
            <Route path="/" element={<Navigate to="/library" replace />} />
            {/* /library and /library/:albumId share the same LibraryPage
                instance — nesting keeps the page mounted across album
                open/close so scroll position, scan stream, and other
                local state are preserved. The DetailSheet reads albumId
                via useParams() regardless of nesting depth. */}
            <Route path="/library" element={<LibraryPage />}>
              <Route path=":albumId" element={null} />
            </Route>
            <Route path="/review" element={<ReviewPage />} />
            {/* Needs attention, and the old Jobs and Health pages before it,
                are Review now; their links (including per-check and per-job
                deep links) land there with the nearest filter. */}
            <Route path="/attention" element={<AttentionRedirect />} />
            <Route path="/jobs" element={<JobsRedirect />} />
            <Route path="/jobs/:jobId" element={<JobsRedirect />} />
            <Route path="/health" element={<HealthRedirect />} />
            <Route path="/health/:checkId" element={<HealthRedirect />} />
            <Route path="/settings" element={<SettingsPage />} />
            <Route path="*" element={<Navigate to="/library" replace />} />
          </Routes>
        </main>
      </div>
      {/* Over whichever page is open; opened by the titlebar's Activity
          control, openActivityPanel() or a `?activity=` link. */}
      <ActivityPanel />
      {/* Help → Keyboard Shortcuts, the View menu, or `?`. */}
      <KeyboardShortcutsDialog />
      {/* Top-right so toasts don't overlap with the right-anchored DetailSheet's
          footer buttons (which sit bottom-right of the visible area). */}
      <Toaster
        position="top-right"
        theme="dark"
        toastOptions={{
          classNames: {
            toast: "!bg-panel !border-line !text-fg",
            description: "!text-fg-3",
            actionButton: "!bg-accent !text-accent-fg",
          },
        }}
      />
    </div>
  );
}
