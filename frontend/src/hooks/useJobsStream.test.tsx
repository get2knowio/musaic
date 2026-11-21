/**
 * The jobs stream is opened once for the life of the app: a route change
 * must not close and reopen it (#201), yet a toast action still navigates
 * with the router's current navigate function.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useEffect } from "react";
import { act, render } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, useNavigate } from "react-router-dom";

const toast = vi.hoisted(() =>
  Object.assign(vi.fn(), { warning: vi.fn(), error: vi.fn(), success: vi.fn() }),
);
vi.mock("sonner", () => ({ toast }));

import { useJobsStream } from "@/hooks/useJobsStream";
import { MockEventSource } from "@/test/setup";

let go: ((to: string) => void) | null = null;

function Probe() {
  useJobsStream();
  const navigate = useNavigate();
  useEffect(() => {
    go = navigate;
  }, [navigate]);
  return null;
}

beforeEach(() => {
  MockEventSource.instances.length = 0;
  toast.success.mockClear();
});

describe("useJobsStream", () => {
  it("keeps one stream open across navigation", () => {
    const qc = new QueryClient();
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/library"]}>
          <Probe />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(MockEventSource.instances).toHaveLength(1);
    act(() => go!("/review"));
    act(() => go!("/settings"));
    expect(MockEventSource.instances).toHaveLength(1);
    expect(MockEventSource.last!.closed).toBe(false);
  });
});
