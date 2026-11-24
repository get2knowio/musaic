/**
 * Whether the Activity panel is open, and what it should bring into view.
 *
 * The panel (`components/ActivityPanel.tsx`) lies over whatever page is
 * current (Library, Review, Settings) and never changes the route, so its
 * open state is not in the URL. Anything may open it:
 *
 *  - code: `openActivityPanel()` (a native "View → Show Activity" menu item,
 *    the toolbar control, a toast's action);
 *  - a link: `?activity=` on any route (`activitySearch`), read once by
 *    `useActivityLinks` (mounted in App), which opens the panel and drops
 *    the parameter. `/review?show=running|history` and `/jobs` from older
 *    builds arrive this way.
 */
import { useSyncExternalStore } from "react";

/** The toolbar control's id, so a click on it is not "outside" the panel. */
export const ACTIVITY_TOGGLE_ID = "activity-toggle";
export const ACTIVITY_PANEL_ID = "activity-panel";

/** A part of the panel to bring into view: research requests in progress, or their history. */
export type ActivitySection = "jobs" | "history";

export interface ActivityPanelState {
  open: boolean;
  /** The section to scroll to when the panel opens. */
  section: ActivitySection | null;
  /** A job to mark (a link to one job that is not a review item). */
  job: string | null;
}

let state: ActivityPanelState = { open: false, section: null, job: null };
const listeners = new Set<() => void>();

function set(next: ActivityPanelState) {
  state = next;
  for (const l of listeners) l();
}

/** Open the panel, optionally at a section or a job. */
export function openActivityPanel(focus: { section?: ActivitySection | null; job?: string | null } = {}): void {
  set({ open: true, section: focus.section ?? null, job: focus.job ?? null });
}

export function closeActivityPanel(): void {
  if (!state.open) return;
  set({ open: false, section: null, job: null });
}

export function toggleActivityPanel(): void {
  if (state.open) closeActivityPanel();
  else openActivityPanel();
}

export function getActivityPanel(): ActivityPanelState {
  return state;
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

/** The panel's state, re-rendering on change. */
export function useActivityPanel(): ActivityPanelState {
  return useSyncExternalStore(subscribe, getActivityPanel, getActivityPanel);
}

/** The query parameter that opens the panel from a link. */
export const ACTIVITY_PARAM = "activity";

/**
 * `?activity=…` for a link: `open` (just the panel), `jobs` or `history`
 * (that section), with `job=<id>` to mark one job.
 */
export function activitySearch(section: ActivitySection | "open" = "open", job?: string | null): string {
  const p = new URLSearchParams();
  p.set(ACTIVITY_PARAM, section);
  if (job) p.set("job", job);
  return p.toString();
}

/**
 * What a `?activity=` link asks for, or null when the search has none.
 * `rest` is the search without the activity parameters, for replacing the
 * address once the panel is open.
 */
export function readActivityLink(search: string): { section: ActivitySection | null; job: string | null; rest: string } | null {
  const p = new URLSearchParams(search);
  const v = p.get(ACTIVITY_PARAM);
  if (v === null) return null;
  const job = p.get("job");
  p.delete(ACTIVITY_PARAM);
  p.delete("job");
  return { section: v === "jobs" || v === "history" ? v : null, job, rest: p.toString() };
}

/** Test hook: back to closed. */
export function resetActivityPanel(): void {
  state = { open: false, section: null, job: null };
}
