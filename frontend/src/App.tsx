import { Navigate, Route, Routes } from "react-router-dom";
import { Toaster } from "sonner";

import LibraryPage from "@/pages/Library";
import AttentionPage from "@/pages/Attention";
import { HealthRedirect, JobsRedirect } from "@/pages/attentionRedirects";
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
  // backend pool state into the cache so the titlebar pill renders
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

  // `/` focuses the Library filter, `,` opens Settings.
  useGlobalShortcuts();

  // Save the user's last /library URL to sessionStorage so the Sidebar's
  // "Library" link can restore the same filter / search / open album when
  // they come back from /attention, /settings, etc.
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
            <Route path="/attention" element={<AttentionPage />} />
            {/* The old Jobs and Health pages are part of Needs attention;
                their links (including per-check and per-job deep links) land there. */}
            <Route path="/jobs" element={<JobsRedirect />} />
            <Route path="/jobs/:jobId" element={<JobsRedirect />} />
            <Route path="/health" element={<HealthRedirect />} />
            <Route path="/health/:checkId" element={<HealthRedirect />} />
            <Route path="/settings" element={<SettingsPage />} />
            <Route path="*" element={<Navigate to="/library" replace />} />
          </Routes>
        </main>
      </div>
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
