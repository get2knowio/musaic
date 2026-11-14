/**
 * A successful database restore reloads the app (the server
 * swaps library.db and publishes no event, so cached and component state
 * cannot be trusted) and never tells the user to refresh. The
 * confirmation survives the reload through sessionStorage and is shown
 * once afterwards.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const toastSuccess = vi.fn();
const toastError = vi.fn();
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: (...a: unknown[]) => toastError(...a), success: (...a: unknown[]) => toastSuccess(...a) }),
}));

import BackupPanel from "@/pages/SettingsBackup";
import { reloadAfterRestore, takeRestoreNotice } from "@/lib/restoreReload";
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
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  qc.setQueryData(["db-info"], {
    path: "/x/library.db",
    exists: true,
    size_bytes: 1,
    schema_version: 16,
    current_schema: 16,
    modified_at: null,
    counts: { albums: 3, albums_with_mbid: 0, track_fingerprints: 0, track_tags: 0, tag_vocabulary: 0, ai_jobs: 0, check_findings_open: 0 },
  } as unknown as DbInfo);
  qc.setQueryData(["db-snapshots"], { snapshots: [snapshot] });
  return render(
    <QueryClientProvider client={qc}>
      <BackupPanel onRestored={onRestored} />
    </QueryClientProvider>,
  );
}

let fetchMock: ReturnType<typeof vi.fn>;
beforeEach(() => {
  sessionStorage.clear();
  toastSuccess.mockClear();
  toastError.mockClear();
  fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    if (String(url).includes("/restore") && init?.method === "POST") {
      return new Response(
        JSON.stringify({ schema_version: 16, size_bytes: 2048, counts: {}, restored_at: "now", aside_paths: [] }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ detail: "not found" }), {
      status: 404,
      headers: { "content-type": "application/json" },
    });
  });
  vi.stubGlobal("fetch", fetchMock);
});
afterEach(() => vi.unstubAllGlobals());

describe("restore brings the app up to date by itself", () => {
  it("confirm copy does not ask the user to refresh", () => {
    renderPanel(vi.fn());
    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    const body = screen.getByText(/overwrite your current library database/);
    expect(body.textContent).toContain("Musaic reloads itself once the restore is done.");
    expect(body.textContent).not.toMatch(/refresh/i);
  });

  it("a successful snapshot restore hands its message to the reload, with no 'refresh' toast", async () => {
    const onRestored = vi.fn();
    renderPanel(onRestored);
    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    const buttons = screen.getAllByRole("button", { name: "Restore" });
    fireEvent.click(buttons[buttons.length - 1]);
    await waitFor(() => expect(onRestored).toHaveBeenCalledWith("Restored from snapshot."));
    const [url, init] = fetchMock.mock.calls.find(([, i]) => (i as RequestInit | undefined)?.method === "POST")!;
    expect(String(url)).toBe(`/api/db/snapshots/${encodeURIComponent(snapshot.filename)}/restore`);
    expect((init as RequestInit).method).toBe("POST");
    for (const call of toastSuccess.mock.calls) expect(String(call[0])).not.toMatch(/refresh/i);
  });

  it("a failed restore does not reload", async () => {
    fetchMock.mockImplementation(async () =>
      new Response(JSON.stringify({ detail: "A restore is already running." }), {
        status: 409,
        headers: { "content-type": "application/json" },
      }),
    );
    const onRestored = vi.fn();
    renderPanel(onRestored);
    fireEvent.click(screen.getByRole("button", { name: "Restore" }));
    const buttons = screen.getAllByRole("button", { name: "Restore" });
    fireEvent.click(buttons[buttons.length - 1]);
    await waitFor(() => expect(toastError).toHaveBeenCalled());
    expect(onRestored).not.toHaveBeenCalled();
  });

  it("reloadAfterRestore keeps the message for after the reload, and the panel shows it once", async () => {
    const reload = vi.fn();
    reloadAfterRestore("Restored from snapshot.", reload);
    expect(reload).toHaveBeenCalledTimes(1);
    // The reloaded page mounts the panel again.
    renderPanel(vi.fn());
    await waitFor(() => expect(toastSuccess).toHaveBeenCalledWith("Restored from snapshot."));
    expect(takeRestoreNotice()).toBeNull();
  });

  it("takeRestoreNotice is null when nothing was restored", () => {
    expect(takeRestoreNotice()).toBeNull();
  });
});
