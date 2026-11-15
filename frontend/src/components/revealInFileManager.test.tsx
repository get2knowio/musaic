/**
 * "Show in Finder": the album sheet's folder row and the
 * duplicate Resolve dialog's trash text offer a reveal control only in the
 * desktop window (the `window.__musaicDesktop` marker advertising
 * `reveal`). It posts an album id (never a path) or asks for the trash;
 * the label names the platform's file manager from the marker. A plain
 * browser renders neither and makes no such request.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import { toast } from "sonner";
import PropTable from "@/components/DetailSheet/PropTable";
import DuplicateResolveDialog from "@/components/DuplicateResolveDialog";
import { makeAlbum } from "@/test/fixtures";
import type { Finding } from "@/lib/types";

const ALBUM_ID = "7ddd61e60067be1a";

interface Call {
  url: string;
  method: string;
  body: unknown;
}
let calls: Call[];
let revealStatus: number;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  calls = [];
  revealStatus = 204;
  vi.mocked(toast.error).mockClear();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      calls.push({ url: String(url), method, body: init?.body });
      if (String(url).startsWith("/__musaic/reveal/")) {
        return revealStatus === 204
          ? new Response(null, { status: 204 })
          : json({ detail: "This album's folder isn't on disk right now." }, revealStatus);
      }
      return json({ detail: "not found" }, 404);
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
  delete (window as { __musaicDesktop?: unknown }).__musaicDesktop;
});

function desktop(marker: Record<string, unknown>) {
  Object.defineProperty(window, "__musaicDesktop", {
    value: Object.freeze(marker),
    configurable: true,
  });
}

function wrap(ui: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

const album = makeAlbum({ id: ALBUM_ID, path: "/music/Artist - Album [FLAC]" });

const finding: Finding = {
  id: 7,
  check_id: "duplicate-tracks",
  album_id: ALBUM_ID,
  severity: "warn",
  summary: "2 copies of one recording",
  detail: { filenames: ["01 - A.flac", "01 - A (1).flac"], durations_s: [200, 200] },
  first_seen_at: "2026-11-14T00:00:00Z",
  last_seen_at: "2026-11-14T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
};

const revealCalls = () => calls.filter((c) => c.url.startsWith("/__musaic/reveal/"));

describe("album sheet folder row", () => {
  it("has no reveal control in a plain browser", () => {
    wrap(<PropTable album={album} />);
    expect(screen.getByText(album.path)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /show in/i })).toBeNull();
  });

  it("has no reveal control when the marker only advertises the folder picker", () => {
    desktop({ folderPicker: true });
    wrap(<PropTable album={album} />);
    expect(screen.queryByRole("button", { name: /show in/i })).toBeNull();
  });

  it("reveals the album by id in the desktop window, labelled for the platform", async () => {
    desktop({ folderPicker: true, reveal: true, fileManager: "Finder" });
    wrap(<PropTable album={album} />);
    fireEvent.click(screen.getByRole("button", { name: "Show in Finder" }));
    await waitFor(() => expect(revealCalls()).toHaveLength(1));
    expect(revealCalls()[0]).toEqual({
      url: `/__musaic/reveal/album/${ALBUM_ID}`,
      method: "POST",
      body: undefined,
    });
    expect(toast.error).not.toHaveBeenCalled();
  });

  it("falls back to a neutral label and shows the server's refusal", async () => {
    desktop({ reveal: true });
    revealStatus = 404;
    wrap(<PropTable album={album} />);
    fireEvent.click(screen.getByRole("button", { name: "Show in file manager" }));
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith("This album's folder isn't on disk right now."),
    );
  });

  it("offers nothing for a Missing album", () => {
    desktop({ reveal: true, fileManager: "Explorer" });
    wrap(<PropTable album={makeAlbum({ id: ALBUM_ID, missing_since: "2026-11-14T00:00:00" })} />);
    expect(screen.queryByRole("button", { name: /show in/i })).toBeNull();
  });
});

describe("duplicate Resolve dialog trash text", () => {
  it("has no reveal control in a plain browser", async () => {
    wrap(<DuplicateResolveDialog finding={finding} open onClose={() => {}} />);
    expect(await screen.findByText(/moved to the app's trash folder/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /show in/i })).toBeNull();
  });

  it("reveals the trash in the desktop window", async () => {
    desktop({ folderPicker: true, reveal: true, fileManager: "Explorer" });
    wrap(<DuplicateResolveDialog finding={finding} open onClose={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Show in Explorer" }));
    await waitFor(() => expect(revealCalls()).toHaveLength(1));
    expect(revealCalls()[0]).toMatchObject({ url: "/__musaic/reveal/trash", method: "POST" });
  });
});
