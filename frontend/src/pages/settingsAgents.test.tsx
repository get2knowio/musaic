/**
 * Settings, Assistants: pairing, what each pairing may do, revoking, and
 * that a secret is never on the page.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import AgentsPanel from "@/pages/SettingsAgents";
import type { AgentPairing } from "@/lib/api";

const allow = { "library.read": "allow", "proposal.submit": "allow", "compute.scan": "refuse", "compute.analyze": "refuse", "tags.write": "ask", "covers.write": "refuse", "files.rename": "refuse", "work.cancel": "refuse" } as const;

const helper: AgentPairing = {
  id: "p1",
  name: "Helper",
  scopes: ["library.read", "proposal.submit", "tags.write"],
  policy: { ...allow },
  createdAt: "2026-01-02T03:04:05Z",
  lastUsedAt: null,
  revokedAt: null,
};
const old: AgentPairing = { ...helper, id: "p2", name: "Old one", revokedAt: "2026-02-01T00:00:00Z" };

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

let calls: { url: string; init?: RequestInit }[] = [];
let handler: (url: string, init?: RequestInit) => Response;

beforeEach(() => {
  calls = [];
  handler = () => json({ detail: "not found" }, 404);
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      return handler(String(url), init);
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

function renderPanel(pairings: AgentPairing[] | null) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  if (pairings) qc.setQueryData(["agent", "pairings"], pairings);
  qc.setQueryData(["agent", "diagnostics"], []);
  render(
    <QueryClientProvider client={qc}>
      <AgentsPanel />
    </QueryClientProvider>,
  );
  return qc;
}

describe("Assistants section", () => {
  it("says nothing is connected, and keeps the note about local programs", () => {
    renderPanel([]);
    expect(
      screen.getByText("No assistants are connected. Add one to let it read your library and suggest fixes."),
    ).toBeTruthy();
    expect(screen.getByText(/Anyone who can run programs as you on this computer can use an assistant you added/)).toBeTruthy();
  });

  it("the pair form starts with read and suggest checked and nothing else", () => {
    renderPanel([]);
    const form = screen.getByRole("form", { name: "Add an assistant" });
    const boxes = within(form).getAllByRole("checkbox") as HTMLInputElement[];
    expect(boxes).toHaveLength(8);
    expect(boxes.map((b) => b.checked)).toEqual([true, true, false, false, false, false, false, false]);
    expect(within(form).getByRole("button", { name: "Add assistant" })).toBeDisabled();
  });

  it("pairing posts the name and scopes, then shows how to connect and no secret", async () => {
    handler = (url, init) =>
      url === "/api/agent/pairings" && init?.method === "POST"
        ? json({ ...helper, connect: { command: "musaic-cli", args: ["setup"] } })
        : json({ detail: "not found" }, 404);
    renderPanel([]);
    fireEvent.change(screen.getByRole("textbox", { name: "Assistant name" }), { target: { value: " Helper " } });
    fireEvent.click(screen.getByRole("checkbox", { name: "Change tags" }));
    fireEvent.click(screen.getByRole("button", { name: "Add assistant" }));

    const section = await screen.findByRole("region", { name: "Connect the assistant" });
    expect(within(section).getByText("musaic-cli setup")).toBeTruthy();
    expect(section.textContent).toContain("docs/agents.md");
    const post = calls.find((c) => c.init?.method === "POST")!;
    expect(JSON.parse(String(post.init!.body))).toEqual({
      name: "Helper",
      scopes: ["library.read", "proposal.submit", "tags.write"],
    });
    expect(document.body.textContent).not.toMatch(/secret:|token|password/i);
    expect(document.querySelector("input[type=password]")).toBeNull();
  });

  it("a refused pairing (20 active) shows the reason", async () => {
    handler = () => json({ detail: "Too many pairings; revoke one first" }, 409);
    renderPanel([]);
    fireEvent.change(screen.getByRole("textbox", { name: "Assistant name" }), { target: { value: "X" } });
    fireEvent.click(screen.getByRole("button", { name: "Add assistant" }));
    expect((await screen.findByRole("alert")).textContent).toContain("Too many pairings");
  });

  it("policy selectors: disabled outside the scope, Allow or Refuse only for read and suggest", () => {
    renderPanel([helper]);
    const group = screen.getByRole("group", { name: "Permissions for Helper" });
    const sel = (name: string) => within(group).getByRole("combobox", { name }) as HTMLSelectElement;
    const options = (s: HTMLSelectElement) => Array.from(s.options).map((o) => o.textContent);

    expect(options(sel("Read your library"))).toEqual(["Allow", "Refuse"]);
    expect(options(sel("Suggest fixes for you to review"))).toEqual(["Allow", "Refuse"]);
    expect(options(sel("Change tags"))).toEqual(["Allow", "Ask me", "Refuse"]);
    expect(sel("Change tags").value).toBe("ask");
    expect(sel("Change tags")).not.toBeDisabled();
    expect(sel("Scan the library")).toBeDisabled();
    expect(sel("Set covers")).toBeDisabled();
    expect(sel("Rename files and folders")).toBeDisabled();
    expect(options(sel("Cancel delegated research"))).toEqual(["Allow", "Refuse"]);
    expect(sel("Cancel delegated research")).toBeDisabled();
  });

  it("changing a policy sends only that kind and does not touch the cache itself", async () => {
    handler = () => json(helper);
    const qc = renderPanel([helper]);
    const invalidate = vi.spyOn(qc, "invalidateQueries");
    fireEvent.change(screen.getByRole("combobox", { name: "Change tags" }), { target: { value: "allow" } });
    await waitFor(() => expect(calls.length).toBe(1));
    expect(calls[0].url).toBe("/api/agent/pairings/p1/policy");
    expect(calls[0].init?.method).toBe("PUT");
    expect(JSON.parse(String(calls[0].init!.body))).toEqual({ "tags.write": "allow" });
    expect(invalidate).not.toHaveBeenCalled();
  });

  it("revoking asks first, starts on Cancel, and only Revoke sends the request", async () => {
    handler = () => json({ ...helper, revokedAt: "2026-03-01T00:00:00Z" });
    renderPanel([helper]);
    fireEvent.click(screen.getByRole("button", { name: "Revoke Helper" }));
    const dialog = await screen.findByRole("dialog", { name: "Revoke Helper?" });
    expect(calls).toHaveLength(0);
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toHaveFocus();

    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(calls).toHaveLength(0);

    fireEvent.click(screen.getByRole("button", { name: "Revoke Helper" }));
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Revoke" }));
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0].url).toBe("/api/agent/pairings/p1");
    expect(calls[0].init?.method).toBe("DELETE");
  });

  it("a revoked pairing says so and has no controls", () => {
    renderPanel([old]);
    const row = screen.getByText("Old one").closest("li")!;
    expect(row.textContent).toContain("Revoked");
    expect(within(row).queryByRole("button", { name: /Revoke/ })).toBeNull();
    expect(within(row).queryAllByRole("combobox")).toHaveLength(0);
  });

  it("the activity list opens on demand and puts outcomes in plain words", async () => {
    handler = (url) =>
      url === "/api/agent/pairings/p1/activity"
        ? json([
            { at: "2026-01-02T03:04:05Z", tool: "musaic_tags_write", outcome: "refused:FORBIDDEN" },
            { at: "2026-01-02T03:00:00Z", tool: "musaic_albums_list", outcome: "ok" },
          ])
        : json({ detail: "not found" }, 404);
    renderPanel([helper]);
    expect(calls).toHaveLength(0);
    const toggle = screen.getByRole("button", { name: "Activity" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    const list = await screen.findByRole("list", { name: "Recent activity" });
    expect(list.textContent).toContain("Refused (forbidden)");
    expect(list.textContent).toContain("Done");
    expect(list.textContent).toContain("musaic_albums_list");
  });
});

describe("Assistant diagnostics", () => {
  const base = {
    pairingId: "p1",
    name: "Helper",
    scopes: ["library.read"],
    revoked: false,
    paired: { at: "2026-11-23T10:00:00Z" },
    reachable: { at: null },
    subscription: { active: false, at: null },
    lastClaim: { at: null, workId: null },
    host: null,
  };

  function renderWith(diagnostics: unknown) {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    qc.setQueryData(["agent", "pairings"], [helper]);
    qc.setQueryData(["agent", "diagnostics"], diagnostics);
    render(
      <QueryClientProvider client={qc}>
        <AgentsPanel />
      </QueryClientProvider>,
    );
    return qc;
  }

  it("a pairing that never connected shows Added and nothing else", () => {
    renderWith([base]);
    const list = document.querySelector("dl[aria-label='What Helper has done']")!;
    const text = list.textContent ?? "";
    expect(text).toContain("Added");
    expect(text).toContain("Connected");
    expect(text).toMatch(/Connected\s*not yet/);
    expect(text).toMatch(/Listening for work\s*no/);
    expect(text).toMatch(/Last request picked up\s*never/);
    expect(text).not.toContain("Host");
  });

  it("never says ready, and shows the host as reported by the host", () => {
    renderWith([
      {
        ...base,
        reachable: { at: new Date().toISOString() },
        subscription: { active: true, at: new Date().toISOString() },
        host: {
          clientName: "Some Host",
          clientVersion: "1.2",
          profile: "2026-07-28",
          tasks: true,
          skills: false,
          events: true,
          observedAt: new Date().toISOString(),
        },
      },
    ]);
    const body = document.body.textContent ?? "";
    expect(body.toLowerCase()).not.toContain("ready");
    expect(body).toContain("Some Host 1.2");
    expect(body).toContain("reported by the host");
    expect(body).toContain("protocol 2026-07-28");
    expect(body).toContain("extensions: Tasks, Events");
    expect(body).toMatch(/Listening for work\s*yes, listening now/);
    // Listening: no instructions needed.
    expect(screen.queryByRole("button", { name: "Process pending Musaic work" })).toBeNull();
  });

  it("offers the Process pending Musaic work disclosure with the keyboard when nobody is listening", () => {
    renderWith([base]);
    const button = screen.getByRole("button", { name: "Process pending Musaic work" });
    expect(button.getAttribute("aria-expanded")).toBe("false");
    button.focus();
    expect(document.activeElement).toBe(button);
    fireEvent.click(button);
    expect(button.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText(/Process pending Musaic work using the musaic-work-delegation skill/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Copy" })).toBeTruthy();
  });

  it("a revoked pairing has no diagnostics", () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    qc.setQueryData(["agent", "pairings"], [old]);
    qc.setQueryData(["agent", "diagnostics"], [{ ...base, pairingId: "p2", name: "Old one", revoked: true }]);
    render(
      <QueryClientProvider client={qc}>
        <AgentsPanel />
      </QueryClientProvider>,
    );
    expect(document.querySelector("dl")).toBeNull();
  });
});

describe("Claude section", () => {
  it("offers both actions, the skill steps and the not-available-yet list without controls", () => {
    renderPanel([]);
    expect(screen.getByRole("button", { name: "Connect Claude Desktop" })).toBeTruthy();
    const skill = screen.getByRole("link", { name: "Save the Musaic skill for Claude" });
    expect(skill.getAttribute("href")).toBe("/api/agent/skill-archive");
    const steps = screen.getByRole("list", { name: "Add the skill in Claude" });
    expect(within(steps).getAllByRole("listitem")).toHaveLength(3);
    expect(screen.getByText("Musaic cannot install a skill into Claude for you.")).toBeTruthy();
    const heading = screen.getByRole("heading", { name: "Not available yet" });
    const block = heading.parentElement!;
    expect(block.textContent).toContain("Grok app, the ChatGPT app and web, the Gemini app, and Claude on the web and on phones");
    expect(within(block).queryAllByRole("button")).toHaveLength(0);
    expect(within(block).queryAllByRole("link")).toHaveLength(0);
  });

  it("marks a program older than the app, with both versions", () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    qc.setQueryData(["agent", "pairings"], [helper]);
    qc.setQueryData(["agent", "diagnostics"], [
      {
        pairingId: "p1",
        name: "Helper",
        scopes: ["library.read"],
        revoked: false,
        paired: { at: "2026-01-02T03:04:05Z" },
        reachable: { at: null },
        subscription: { active: false, at: null },
        lastClaim: { at: null, workId: null },
        host: null,
        cliVersion: "0.1.0",
        cliOlder: true,
        appVersion: "0.2.0",
      },
    ]);
    render(
      <QueryClientProvider client={qc}>
        <AgentsPanel />
      </QueryClientProvider>,
    );
    expect(
      screen.getByText("Running Musaic's command-line program 0.1.0; the app is 0.2.0. Connect Claude Desktop again to update it."),
    ).toBeTruthy();
  });
});
