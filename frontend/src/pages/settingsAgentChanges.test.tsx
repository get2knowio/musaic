/**
 * Settings, Assistants, Recent changes: the list of what an assistant
 * applied, and Undo (asks first, says why when it cannot run, and leaves
 * cache freshness to the events).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { toast } from "sonner";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import AgentsPanel from "@/pages/SettingsAgents";
import type { AgentChange, AgentPairing } from "@/lib/api";

const helper: AgentPairing = {
  id: "p1",
  name: "Helper",
  scopes: ["library.read", "proposal.submit", "tags.write"],
  policy: { "library.read": "allow", "proposal.submit": "allow", "tags.write": "allow" },
  createdAt: "2026-01-02T03:04:05Z",
  lastUsedAt: null,
  revokedAt: null,
};

const base: AgentChange = {
  id: "ch_1",
  pairingId: "p1",
  pairingName: "Helper",
  albumId: "0123456789abcdef",
  albumLabel: "Aurora Fields - First Light",
  createdAt: "2026-01-03T10:00:00Z",
  state: "applied",
  summary: "Album genre, year",
  undoes: null,
  undoable: true,
  reason: null,
};
const stale: AgentChange = {
  ...base,
  id: "ch_2",
  albumLabel: "Aurora Fields - Second Light",
  undoable: false,
  reason: "Changed since: 01 - Dawn.flac: genre",
};
const undoRecord: AgentChange = {
  ...base,
  id: "ch_3",
  summary: "Undo: Album genre",
  undoes: "ch_9",
  undoable: false,
  reason: "An undo cannot itself be undone.",
};

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

let calls: { url: string; init?: RequestInit }[] = [];
let handler: (url: string, init?: RequestInit) => Response;

beforeEach(() => {
  calls = [];
  vi.mocked(toast.error).mockClear();
  vi.mocked(toast.success).mockClear();
  handler = (url) =>
    url === "/api/agent/changes?pairing=p1" ? json([base, stale, undoRecord]) : json({ detail: "not found" }, 404);
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      return handler(String(url), init);
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

function renderPanel() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["agent", "pairings"], [helper]);
  qc.setQueryData(["agent", "diagnostics"], []);
  render(
    <QueryClientProvider client={qc}>
      <AgentsPanel />
    </QueryClientProvider>,
  );
  return qc;
}

async function openChanges() {
  const toggle = screen.getByRole("button", { name: "Recent changes" });
  expect(toggle).toHaveAttribute("aria-expanded", "false");
  fireEvent.click(toggle);
  expect(toggle).toHaveAttribute("aria-expanded", "true");
  return screen.findByRole("list", { name: "Recent changes by Helper" });
}

describe("Recent changes", () => {
  it("opens on demand and lists each change with its album, summary and state", async () => {
    renderPanel();
    expect(calls).toHaveLength(0);
    const list = await openChanges();
    expect(calls[0].url).toBe("/api/agent/changes?pairing=p1");
    expect(list.textContent).toContain("Aurora Fields - First Light");
    expect(list.textContent).toContain("Album genre, year");
    expect(list.textContent).toContain("Applied");
  });

  it("an Undo that cannot run is disabled and says why; an undo record has none", async () => {
    renderPanel();
    const list = await openChanges();
    const ok = within(list).getByRole("button", { name: "Undo the change on Aurora Fields - First Light" });
    expect(ok).not.toBeDisabled();
    const blocked = within(list).getByRole("button", { name: "Undo the change on Aurora Fields - Second Light" });
    expect(blocked).toBeDisabled();
    const reason = document.getElementById(blocked.getAttribute("aria-describedby")!);
    expect(reason?.textContent).toBe("Changed since: 01 - Dawn.flac: genre");
    const recordRow = list.querySelector('[data-change-id="ch_3"]')!;
    expect(within(recordRow as HTMLElement).queryByRole("button")).toBeNull();
  });

  it("Undo asks first, starts on Cancel, and only Undo in the dialog sends the request", async () => {
    const qc = renderPanel();
    const invalidate = vi.spyOn(qc, "invalidateQueries");
    const list = await openChanges();
    handler = (url, init) =>
      url === "/api/agent/changes/ch_1/undo" && init?.method === "POST"
        ? json({ change: { ...base, state: "undone", undoable: false }, undoId: "ch_u", complete: true, message: null })
        : json([base]);
    const before = calls.length;

    fireEvent.click(within(list).getByRole("button", { name: "Undo the change on Aurora Fields - First Light" }));
    const dialog = await screen.findByRole("dialog", { name: "Undo this change?" });
    expect(dialog.textContent).toContain(
      "Put back the tags, names and cover this assistant changed on Aurora Fields - First Light?",
    );
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toHaveFocus();
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls.length).toBe(before);

    fireEvent.click(within(list).getByRole("button", { name: "Undo the change on Aurora Fields - First Light" }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(calls.length).toBe(before + 1));
    expect(calls[before].url).toBe("/api/agent/changes/ch_1/undo");
    expect(calls[before].init?.method).toBe("POST");
    await waitFor(() => expect(toast.success).toHaveBeenCalled());
    expect(invalidate).not.toHaveBeenCalled();
  });

  it("a refusal because the album changed since is shown in plain words", async () => {
    renderPanel();
    const list = await openChanges();
    handler = () =>
      json(
        {
          detail: {
            message: "The album changed since the assistant's change, so nothing was put back: 01 - Dawn.flac: genre",
            changed: ["01 - Dawn.flac: genre"],
          },
        },
        409,
      );
    fireEvent.click(within(list).getByRole("button", { name: "Undo the change on Aurora Fields - First Light" }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(toast.error).toHaveBeenCalled());
    expect(String(vi.mocked(toast.error).mock.calls[0][0])).toContain("nothing was put back");
  });
});
