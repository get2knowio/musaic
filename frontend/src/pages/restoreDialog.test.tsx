/**
 * The restore confirmation is an accessible, named modal (the pattern of
 * DetailSheet/RemoveAlbumDialog): it starts on Cancel, keeps Tab inside,
 * Escape dismisses it, focus goes back to the button that opened it, and
 * it says which source replaces which database.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }),
}));

import BackupPanel from "@/pages/SettingsBackup";
import type { DbInfo, DbSnapshot } from "@/lib/types";

const snapshot: DbSnapshot = {
  filename: "library.db.snapshot-20261114T000000Z",
  size_bytes: 2048,
  modified_at: "2026-11-14T00:00:00Z",
  schema_version: 16,
  valid: true,
  counts: { albums: 3 },
};

function renderPanel(onRestored: (m: string) => void) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["db-info"], {
    path: "/home/me/.musaic/library.db",
    exists: true,
    size_bytes: 1,
    schema_version: 16,
    current_schema: 16,
    modified_at: null,
    counts: { albums: 3, albums_with_mbid: 0, track_fingerprints: 0, track_tags: 0, tag_vocabulary: 0, ai_jobs: 0, check_findings_open: 0 },
  } as unknown as DbInfo);
  qc.setQueryData(["db-snapshots"], { snapshots: [snapshot] });
  qc.setQueryData(["settings"], { snapshot_retention_count: 10, snapshot_min_interval_hours: 24 });
  return render(
    <QueryClientProvider client={qc}>
      <BackupPanel onRestored={onRestored} />
    </QueryClientProvider>,
  );
}

let fail: boolean;
let restorePosts: number;
beforeEach(() => {
  sessionStorage.clear();
  fail = false;
  restorePosts = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      if (String(url).includes("/restore") && init?.method === "POST") {
        restorePosts++;
        if (fail) {
          return new Response(JSON.stringify({ detail: "disk is full" }), {
            status: 500,
            headers: { "content-type": "application/json" },
          });
        }
        return new Response(JSON.stringify({ schema_version: 16 }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      return new Response(JSON.stringify({ detail: "not found" }), { status: 404 });
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

function openFromSnapshot() {
  const opener = screen.getByRole("button", { name: "Restore" });
  opener.focus();
  fireEvent.click(opener);
  return opener;
}

describe("restore confirmation modal", () => {
  it("is a named dialog that says the source, the replaced database, the first backup and the reload", () => {
    renderPanel(vi.fn());
    openFromSnapshot();
    const dialog = screen.getByRole("dialog", { name: "Restore the catalog database?" });
    const text = dialog.textContent ?? "";
    expect(text).toContain(snapshot.filename);
    expect(text).toContain("/home/me/.musaic/library.db");
    expect(text).toContain("library.db.bak-");
    expect(text).toContain("Musaic reloads itself once the restore is done.");
    expect(text).toMatch(/FLAC files, embedded tags, cover files and\s+filenames/);
  });

  it("starts on Cancel, keeps Tab inside, and Escape dismisses without restoring", async () => {
    const onRestored = vi.fn();
    renderPanel(onRestored);
    const opener = openFromSnapshot();
    const dialog = screen.getByRole("dialog");
    const cancel = within(dialog).getByRole("button", { name: "Cancel" });
    const restore = within(dialog).getByRole("button", { name: "Restore" });
    await waitFor(() => expect(cancel).toHaveFocus());

    // Tab from the last control wraps to the first; Shift+Tab goes back.
    restore.focus();
    fireEvent.keyDown(restore, { key: "Tab" });
    expect(cancel).toHaveFocus();
    fireEvent.keyDown(cancel, { key: "Tab", shiftKey: true });
    expect(restore).toHaveFocus();

    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(restorePosts).toBe(0);
    expect(onRestored).not.toHaveBeenCalled();
    await waitFor(() => expect(opener).toHaveFocus());
  });

  it("the background is not reachable while it is open", () => {
    renderPanel(vi.fn());
    openFromSnapshot();
    // aria-hidden background: only the dialog's own buttons are exposed.
    expect(screen.queryByRole("button", { name: /Snapshot now/ })).toBeNull();
  });

  it("a failed restore leaves the panel usable and a retry restores", async () => {
    const onRestored = vi.fn();
    renderPanel(onRestored);
    fail = true;
    openFromSnapshot();
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Restore" }));
    await waitFor(() => expect(restorePosts).toBe(1));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(onRestored).not.toHaveBeenCalled();

    fail = false;
    openFromSnapshot();
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Restore" }));
    await waitFor(() => expect(onRestored).toHaveBeenCalledWith("Restored from snapshot."));
    expect(restorePosts).toBe(2);
  });

  it("is labelled Back up catalog database and explains what a restore recovers", () => {
    renderPanel(vi.fn());
    expect(screen.getByRole("button", { name: /Back up catalog database/ })).toBeInTheDocument();
    expect(screen.getByText(/does not touch your music/)).toBeInTheDocument();
  });
});
