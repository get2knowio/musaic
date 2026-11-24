/**
 * Connect Claude Desktop: each readiness state says its own sentence and
 * offers the right control; confirming posts once and shows the outcome; and
 * without the desktop marker there is no hand-off control.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import ConnectClaudeDesktop from "@/components/ConnectClaudeDesktop";
import type { ClaudeDesktopStatus } from "@/lib/api";

const PROGRAM = "/Applications/Musaic.app/Contents/MacOS/musaic-cli";

const ready: ClaudeDesktopStatus = {
  platform: "darwin",
  bundleSupported: true,
  claudeFound: true,
  configEntry: "absent",
  extension: "not-detected",
  cli: { state: "found", command: { program: PROGRAM, args: ["mcp"] } },
  shape: "installed",
  route: "bundle",
  setupCommand: { program: PROGRAM, args: ["setup", "--host", "claude-desktop"] },
  uninstallCommand: null,
  appVersion: "0.2.0",
};

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

let calls: { url: string; init?: RequestInit }[] = [];
let status: ClaudeDesktopStatus;
let connectAnswer: () => Response;

beforeEach(() => {
  calls = [];
  status = ready;
  connectAnswer = () => json({ opened: true, command: { program: PROGRAM, args: ["mcp"] }, shape: "installed" });
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      if (String(url) === "/api/agent/claude-desktop") return json(status);
      if (String(url) === "/__musaic/connect/claude-desktop") return connectAnswer();
      return json({ detail: "not found" }, 404);
    }),
  );
  Object.defineProperty(window, "__musaicDesktop", { value: { connectClaude: true }, configurable: true, writable: true });
});
afterEach(() => {
  vi.unstubAllGlobals();
  delete window.__musaicDesktop;
});

function open() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const onClose = vi.fn();
  render(
    <QueryClientProvider client={qc}>
      <ConnectClaudeDesktop onClose={onClose} />
    </QueryClientProvider>,
  );
  return onClose;
}

describe("Connect Claude Desktop", () => {
  it("shows the exact command and one confirm button; nothing is posted until it is pressed", async () => {
    open();
    expect(await screen.findByLabelText("The program the extension runs")).toHaveTextContent(`${PROGRAM} mcp`);
    expect(screen.getByText(/Nothing is installed until you choose Install there/)).toBeTruthy();
    expect(screen.getByText(/publisher is not verified/)).toBeTruthy();
    expect(calls.some((c) => c.init?.method === "POST")).toBe(false);
  });

  it("confirm posts once with no body and shows what happened", async () => {
    open();
    const button = await screen.findByRole("button", { name: "Create and open the extension" });
    fireEvent.click(button);
    fireEvent.click(button);
    expect(await screen.findByRole("status")).toHaveTextContent("The extension file was opened.");
    const posts = calls.filter((c) => c.url === "/__musaic/connect/claude-desktop");
    expect(posts).toHaveLength(1);
    expect(posts[0].init?.method).toBe("POST");
    expect(posts[0].init?.body).toBeUndefined();
    // The hand-off button is gone once opened.
    expect(screen.queryByRole("button", { name: "Create and open the extension" })).toBeNull();
  });

  it("a refused hand-off shows the reason", async () => {
    connectAnswer = () => json({ opened: false, reason: "open-failed", message: "no handler" }, 500);
    open();
    fireEvent.click(await screen.findByRole("button", { name: "Create and open the extension" }));
    expect((await screen.findByRole("alert")).textContent).toContain("Musaic could not open the extension file. no handler");
  });

  it("outside the desktop window there is no hand-off control", async () => {
    delete window.__musaicDesktop;
    open();
    await screen.findByLabelText("The program the extension runs");
    expect(screen.getByText("Open Musaic's desktop app to connect with one click.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Create and open the extension" })).toBeNull();
  });

  const unavailable: [string, Partial<ClaudeDesktopStatus>, RegExp][] = [
    ["Claude not found", { claudeFound: false, route: "blocked", setupCommand: null }, /Claude Desktop was not found on this computer/],
    [
      "Linux",
      { platform: "linux", bundleSupported: false, route: "config-file-only" },
      /Claude Desktop extensions are not available on this platform/,
    ],
    [
      "own entry",
      {
        configEntry: "ours",
        route: "config-file-only",
        uninstallCommand: { program: PROGRAM, args: ["uninstall", "--host", "claude-desktop"] },
      },
      /already connected to Musaic through its configuration file/,
    ],
    ["foreign entry", { configEntry: "foreign", route: "blocked" }, /did not write\. Musaic leaves it alone/],
    ["unreadable", { configEntry: "unreadable", route: "blocked" }, /could not read Claude Desktop's configuration file/],
    ["program missing", { cli: { state: "missing" }, route: "blocked", setupCommand: null }, /command-line program was not found/],
    [
      "unstable",
      { cli: { state: "unstable", reason: "disk-image" }, route: "blocked", setupCommand: null },
      /running from a disk image or a temporary location/,
    ],
  ];
  it.each(unavailable)("%s: says so and offers no hand-off", async (_name, patch, sentence) => {
    status = { ...ready, ...patch };
    open();
    expect(await screen.findByText(sentence)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Create and open the extension" })).toBeNull();
  });

  it("an own entry shows the removal command with a copy control", async () => {
    status = {
      ...ready,
      configEntry: "ours",
      route: "config-file-only",
      uninstallCommand: { program: PROGRAM, args: ["uninstall", "--host", "claude-desktop"] },
    };
    open();
    expect(await screen.findByLabelText("Remove the configuration entry")).toHaveTextContent(
      `${PROGRAM} uninstall --host claude-desktop`,
    );
    expect(screen.getByRole("button", { name: "Copy command" })).toBeTruthy();
  });

  it("Linux offers the configuration-file command", async () => {
    status = { ...ready, platform: "linux", bundleSupported: false, route: "config-file-only" };
    open();
    expect(await screen.findByLabelText("Set up through the configuration file")).toHaveTextContent(
      "setup --host claude-desktop",
    );
  });

  it("Linux without a command does not point at one", async () => {
    status = {
      ...ready,
      platform: "linux",
      bundleSupported: false,
      route: "config-file-only",
      cli: { state: "unstable", reason: "app-image" },
      setupCommand: null,
    };
    open();
    expect(await screen.findByText(/extensions are not available on this platform/)).toBeTruthy();
    expect(screen.queryByText(/command below/)).toBeNull();
  });

  it("a command with an apostrophe or a parenthesis is quoted so it can be pasted", async () => {
    const program = "/Users/o'brien/Apps (old)/musaic-cli";
    status = { ...ready, cli: { state: "found", command: { program, args: ["mcp", "--config-dir", "/a$b"] } } };
    open();
    expect(await screen.findByLabelText("The program the extension runs")).toHaveTextContent(
      `"${program}" mcp --config-dir "/a\\$b"`,
    );
  });

  it("says the extension is already installed, and says nothing when it was not detected", async () => {
    status = { ...ready, extension: "detected" };
    open();
    expect(await screen.findByText(/The Musaic extension is already installed in Claude Desktop/)).toBeTruthy();
    expect(screen.queryByText(/running Musaic twice/)).toBeNull();
    expect(screen.getByRole("button", { name: "Create and open the extension" })).toBeTruthy();
  });

  it("says Musaic runs twice when the extension and a configuration entry are both there", async () => {
    status = { ...ready, extension: "detected", configEntry: "foreign", route: "blocked" };
    open();
    expect(await screen.findByText(/running Musaic twice/)).toBeTruthy();
    // No file can be opened from here, so reinstalling is not offered.
    expect(screen.queryByText(/reinstalls it/)).toBeNull();
  });

  it.each(["not-detected", "unknown"] as const)("%s: no word about an installed extension", async (extension) => {
    status = { ...ready, extension };
    open();
    await screen.findByLabelText("The program the extension runs");
    expect(screen.queryByText(/already installed/)).toBeNull();
    expect(screen.queryByText(/running Musaic twice/)).toBeNull();
  });

  it("closing hands control back to the caller", async () => {
    const onClose = open();
    await screen.findByLabelText("The program the extension runs");
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });
});
