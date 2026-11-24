/**
 * The snapshot policy is a local draft of both fields, written together by
 * one Save. Typing writes nothing; a failed save keeps the draft; the
 * saved values come back from the server after a reopen.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }),
}));

import BackupPanel from "@/pages/SettingsBackup";
import type { DbInfo } from "@/lib/types";

let stored: Record<string, unknown>;
let failNext: number;
let patches: Record<string, unknown>[];

function mount() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["db-info"], {
    counts: { albums: 0, albums_with_mbid: 0, track_fingerprints: 0, track_tags: 0, tag_vocabulary: 0, ai_jobs: 0, check_findings_open: 0 },
  } as unknown as DbInfo);
  qc.setQueryData(["db-snapshots"], { snapshots: [] });
  return render(
    <QueryClientProvider client={qc}>
      <BackupPanel onRestored={vi.fn()} />
    </QueryClientProvider>,
  );
}

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

beforeEach(() => {
  stored = { snapshot_retention_count: 10, snapshot_min_interval_hours: 24 };
  failNext = 0;
  patches = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      if (String(url) === "/api/settings" && init?.method === "PATCH") {
        if (failNext > 0) {
          failNext--;
          return json({ detail: "settings.json is read-only" }, 500);
        }
        const patch = JSON.parse(String(init.body));
        patches.push(patch);
        stored = { ...stored, ...patch };
        return json(stored);
      }
      if (String(url) === "/api/settings") return json(stored);
      return json({ detail: "not found" }, 404);
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

const keep = () => screen.findByRole("spinbutton", { name: /keep last/i });
const hrs = () => screen.getByRole("spinbutton", { name: /at least/i });
const save = () => screen.getByRole("button", { name: "Save snapshot policy" });

describe("snapshot policy", () => {
  it("typing and tabbing away writes nothing; Save writes both fields and they persist across a fresh cache", async () => {
    const first = mount();
    const k = await keep();
    fireEvent.change(k, { target: { value: "7" } });
    fireEvent.blur(k);
    fireEvent.change(hrs(), { target: { value: "0.5" } });
    fireEvent.blur(hrs());
    expect(patches).toHaveLength(0);
    expect(save()).toBeEnabled();
    fireEvent.click(save());
    await screen.findByText("Snapshot policy saved.");
    expect(patches).toEqual([{ snapshot_retention_count: 7, snapshot_min_interval_hours: 0.5 }]);
    first.unmount();

    mount();
    await waitFor(async () => expect(await keep()).toHaveValue(7));
    expect(hrs()).toHaveValue(0.5);
    expect(save()).toBeDisabled();
  });

  it("zero values are valid and saved", async () => {
    mount();
    fireEvent.change(await keep(), { target: { value: "0" } });
    fireEvent.change(hrs(), { target: { value: "0" } });
    fireEvent.click(save());
    await screen.findByText("Snapshot policy saved.");
    expect(patches).toEqual([{ snapshot_retention_count: 0, snapshot_min_interval_hours: 0 }]);
  });

  it("a failed save keeps the draft, says so, and a retry succeeds", async () => {
    mount();
    fireEvent.change(await keep(), { target: { value: "3" } });
    fireEvent.change(hrs(), { target: { value: "1.5" } });
    failNext = 1;
    fireEvent.click(save());
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("settings.json is read-only");
    expect(screen.queryByText("Snapshot policy saved.")).toBeNull();
    expect(await keep()).toHaveValue(3);
    expect(hrs()).toHaveValue(1.5);
    expect(save()).toBeEnabled();

    fireEvent.click(save());
    await screen.findByText("Snapshot policy saved.");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(stored).toMatchObject({ snapshot_retention_count: 3, snapshot_min_interval_hours: 1.5 });
  });

  it("an invalid draft cannot be saved", async () => {
    mount();
    fireEvent.change(await keep(), { target: { value: "101" } });
    expect(save()).toBeDisabled();
    fireEvent.change(await keep(), { target: { value: "2.5" } });
    expect(save()).toBeDisabled();
    expect(patches).toHaveLength(0);
  });
});
