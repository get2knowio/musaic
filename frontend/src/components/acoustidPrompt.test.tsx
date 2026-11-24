/**
 * The one-time "add an AcoustID key" prompt (show, dismiss, never again) and
 * the fingerprinting status line in Settings.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import AcoustidPrompt from "@/components/AcoustidPrompt";
import SettingsPage from "@/pages/Settings";
import { fingerprintStatusLine, shouldShowAcoustidPrompt } from "@/lib/fingerprintStatus";
import type { AppSettings } from "@/lib/api";
import type { FingerprintPoolStatus } from "@/lib/types";

const SCANNED = "2026-11-17T10:00:00Z";

let calls: { url: string; method: string }[];
let dismissedOnServer: boolean;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  calls = [];
  dismissedOnServer = false;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      calls.push({ url: String(url), method });
      if (url === "/api/prompts/acoustid") return json({ dismissed: dismissedOnServer });
      if (url === "/api/prompts/acoustid/dismiss" && method === "POST") {
        dismissedOnServer = true;
        return json({ dismissed: true });
      }
      return json({ detail: "not found" }, 404);
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

const settings = (keySet: boolean) => ({ acoustid_api_key_set: keySet }) as AppSettings;

const pool = (over: Partial<FingerprintPoolStatus> = {}): FingerprintPoolStatus => ({
  status: "idle",
  library_dir: null,
  total: 0,
  completed: 0,
  errors: 0,
  current_album_id: null,
  started_at: null,
  completed_at: null,
  skip_reason: null,
  ...over,
});

function Where() {
  const l = useLocation();
  return <div data-testid="where">{l.pathname + l.hash}</div>;
}

function renderPrompt(opts: { keySet: boolean; scannedAt: string | null; dismissed?: boolean }) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["settings"], settings(opts.keySet));
  if (opts.dismissed !== undefined) {
    qc.setQueryData(["acoustid-prompt"], { dismissed: opts.dismissed });
    dismissedOnServer = opts.dismissed;
  }
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={["/library"]}>
        <Routes>
          <Route path="*" element={<><AcoustidPrompt scannedAt={opts.scannedAt} /><Where /></>} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("shouldShowAcoustidPrompt", () => {
  it("needs a completed scan, no key and no dismissal", () => {
    expect(shouldShowAcoustidPrompt({ scannedAt: SCANNED, keySet: false, dismissed: false })).toBe(true);
    expect(shouldShowAcoustidPrompt({ scannedAt: null, keySet: false, dismissed: false })).toBe(false);
    expect(shouldShowAcoustidPrompt({ scannedAt: SCANNED, keySet: true, dismissed: false })).toBe(false);
    expect(shouldShowAcoustidPrompt({ scannedAt: SCANNED, keySet: false, dismissed: true })).toBe(false);
  });

  it("stays hidden while anything is still unknown", () => {
    expect(shouldShowAcoustidPrompt({ scannedAt: SCANNED, keySet: undefined, dismissed: false })).toBe(false);
    expect(shouldShowAcoustidPrompt({ scannedAt: SCANNED, keySet: false, dismissed: undefined })).toBe(false);
  });
});

describe("AcoustidPrompt", () => {
  it("explains what a key improves and goes to the key field in Settings", async () => {
    renderPrompt({ keySet: false, scannedAt: SCANNED, dismissed: false });
    const region = screen.getByRole("region", { name: "Improve matching" });
    expect(region).toHaveTextContent("match albums by their audio when names and tags are not enough");
    expect(region).toHaveTextContent("free for non-commercial use");
    fireEvent.click(screen.getByRole("button", { name: "Add a key in Settings" }));
    expect(screen.getByTestId("where")).toHaveTextContent("/settings#acoustid-key");
  });

  it("dismissing hides it, tells the server, and a later visit (new cache) does not show it", async () => {
    const first = renderPrompt({ keySet: false, scannedAt: SCANNED, dismissed: false });
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    await waitFor(() => expect(screen.queryByRole("region", { name: "Improve matching" })).toBeNull());
    expect(calls).toContainEqual({ url: "/api/prompts/acoustid/dismiss", method: "POST" });
    first.unmount();

    // A new launch: an empty cache, the server remembers.
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    qc.setQueryData(["settings"], settings(false));
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter>
          <AcoustidPrompt scannedAt={SCANNED} />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    await waitFor(() => expect(calls.filter((c) => c.url === "/api/prompts/acoustid").length).toBe(1));
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByRole("region", { name: "Improve matching" })).toBeNull();
  });

  it("does not show before a scan has completed, or once a key is set", () => {
    renderPrompt({ keySet: false, scannedAt: null, dismissed: false });
    expect(screen.queryByRole("region", { name: "Improve matching" })).toBeNull();
  });

  it("does not show when a key is set", () => {
    renderPrompt({ keySet: true, scannedAt: SCANNED, dismissed: false });
    expect(screen.queryByRole("region", { name: "Improve matching" })).toBeNull();
  });
});

describe("fingerprintStatusLine", () => {
  it("says nothing until the key flag is known", () => {
    expect(fingerprintStatusLine(undefined, pool())).toBeNull();
  });

  it("no key: says albums are not matched by their audio", () => {
    const s = fingerprintStatusLine(false, pool({ status: "done", completed: 3 }));
    expect(s?.tone).toBe("warn");
    expect(s?.text).toContain("No AcoustID key is set, so albums are not matched by their audio.");
  });

  it("no key while fingerprints are computed: says they cannot be looked up", () => {
    const s = fingerprintStatusLine(false, pool({ status: "running", total: 10, completed: 4 }));
    expect(s?.text).toContain("(4 of 10)");
    expect(s?.text).toContain("cannot be looked up without a key");
  });

  it("with a key: running shows N of M", () => {
    expect(fingerprintStatusLine(true, pool({ status: "running", total: 12, completed: 5 }))?.text).toBe(
      "Fingerprinting albums: 5 of 12.",
    );
  });

  it("with a key: done, with skipped albums counted", () => {
    expect(fingerprintStatusLine(true, pool({ status: "done", completed: 12, errors: 2 }))).toEqual({
      tone: "ok",
      text: "Fingerprinting finished: 12 albums checked, 2 skipped.",
    });
    expect(fingerprintStatusLine(true, pool({ status: "done", completed: 1 }))?.text).toBe(
      "Fingerprinting finished: 1 album checked.",
    );
  });

  it("with a key: stopped, and unavailable with its reason", () => {
    expect(fingerprintStatusLine(true, pool({ status: "cancelled", total: 9, completed: 3 }))?.text).toBe(
      "Fingerprinting stopped before it finished (3 of 9). It starts again after the next scan.",
    );
    expect(fingerprintStatusLine(true, pool({ status: "unavailable", skip_reason: "no decoder" }))?.text).toBe(
      "Fingerprinting could not run: no decoder",
    );
  });

  it("with a key: idle says it has not started", () => {
    expect(fingerprintStatusLine(true, pool())?.text).toContain("has not started yet");
  });
});

describe("Settings: fingerprinting section", () => {
  function renderSettings(keySet: boolean, status: FingerprintPoolStatus, hash = "") {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    qc.setQueryData(["config"], { library_dir: null, exists: false });
    qc.setQueryData(["jobs"], []);
    qc.setQueryData(["settings"], settings(keySet));
    qc.setQueryData(["fingerprint-pool-status"], status);
    return render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={[`/settings${hash}`]}>
          <SettingsPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
  }

  it("no longer tells anyone to install anything, and shows the status line", () => {
    renderSettings(false, pool({ status: "done" }));
    expect(screen.queryByText(/chromaprint/i)).toBeNull();
    expect(screen.queryByText(/fpcalc/i)).toBeNull();
    expect(screen.getByText(/Nothing needs installing/)).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("No AcoustID key is set");
  });

  it("with a key, the status line follows the fingerprinting progress", () => {
    renderSettings(true, pool({ status: "running", total: 8, completed: 2 }));
    expect(screen.getByRole("status")).toHaveTextContent("Fingerprinting albums: 2 of 8.");
  });

  it("arriving from the prompt focuses the key field", () => {
    renderSettings(false, pool(), "#acoustid-key");
    expect(screen.getByLabelText("AcoustID API key")).toHaveFocus();
  });
});
