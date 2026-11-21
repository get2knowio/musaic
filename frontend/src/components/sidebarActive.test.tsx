/**
 * The Review entry looks active for any /review URL, whatever its query
 * string (a check filter, a type filter, an album filter, a linked job).
 */
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

import Sidebar from "@/components/Sidebar";

function renderAt(url: string) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["checks"], []);
  qc.setQueryData(["findings"], []);
  qc.setQueryData(["jobs"], []);
  qc.setQueryData(["albums-snapshot"], []);
  render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[url]}>
        <Sidebar />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return screen.getByRole("link", { name: /Review/ });
}

describe("sidebar active state", () => {
  for (const url of [
    "/review",
    "/review?check=filename-mismatch",
    "/review?kind=proposed",
    "/review?album=a1",
    "/review?job=j1",
  ]) {
    it(`${url} is active`, () => {
      const link = renderAt(url);
      expect(link.className).toContain("text-accent");
      expect(link.className).toContain("bg-accent-soft");
    });
  }
  it("other pages are not", () => {
    const link = renderAt("/settings");
    expect(link.className).not.toContain("text-accent");
  });
});
