/**
 * The Ignore popover has one commit button and Cancel, and a finished ignore
 * offers Undo (#329).
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const toasts = vi.hoisted(() => ({ error: vi.fn(), success: vi.fn() }));
vi.mock("sonner", () => ({ toast: toasts }));

import IgnoreFindingControl from "@/components/IgnoreFindingControl";

let calls: { url: string; method: string; body: unknown }[];

beforeEach(() => {
  calls = [];
  toasts.error.mockClear();
  toasts.success.mockClear();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const body = typeof init?.body === "string" ? JSON.parse(init.body) : undefined;
      calls.push({ url: String(url), method: init?.method ?? "GET", body });
      return new Response(JSON.stringify({ id: 7 }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
});

function openPopover() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <IgnoreFindingControl checkId="track-gaps" findingId={7} />
    </QueryClientProvider>,
  );
  // Radix opens its menu on pointer down, not click.
  const trigger = screen.getByRole("button", { name: "Ignore" });
  fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false });
}

describe("IgnoreFindingControl", () => {
  it("has one commit button and Cancel, no separate Skip reason", async () => {
    openPopover();
    expect(await screen.findByRole("button", { name: "Ignore finding" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Skip reason/ })).toBeNull();
  });

  it("Cancel changes nothing", async () => {
    openPopover();
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Ignore finding" })).toBeNull());
    expect(calls).toEqual([]);
    expect(toasts.success).not.toHaveBeenCalled();
  });

  it("ignores with no reason, then offers Undo that restores the finding", async () => {
    openPopover();
    fireEvent.click(await screen.findByRole("button", { name: "Ignore finding" }));
    await waitFor(() => expect(toasts.success).toHaveBeenCalled());
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toContain("/api/checks/track-gaps/findings/7/ignore");
    expect(calls[0].method).toBe("POST");

    const [message, opts] = toasts.success.mock.calls[0];
    expect(message).toBe("Finding ignored");
    expect(opts.action.label).toBe("Undo");
    opts.action.onClick();
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls[1].url).toContain("/api/checks/track-gaps/findings/7/unignore");
  });

  it("sends the typed reason", async () => {
    openPopover();
    fireEvent.change(await screen.findByRole("textbox"), { target: { value: " self-released " } });
    fireEvent.click(screen.getByRole("button", { name: "Ignore finding" }));
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0].body).toEqual({ reason: "self-released" });
  });
});
