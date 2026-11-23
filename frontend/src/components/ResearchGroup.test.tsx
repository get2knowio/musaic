import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, expect, it, vi } from "vitest";
import { ResearchGroup, RequestResearch } from "./ResearchGroup";
import type { Job } from "@/lib/types";
import type { ResearchItem } from "@/lib/api";

afterEach(() => vi.unstubAllGlobals());

const job: Job = {
  id: "group",
  album_id: "album",
  album_path: "",
  album_title: "First Light",
  job_type: "metadata+cover",
  hint: "",
  status: "completed",
  result: { delegated: true },
  error: null,
  acted_on_action: null,
  created_at: "",
  started_at: null,
  completed_at: null,
  acted_on_at: null,
};
const metadata: ResearchItem = {
  id: "metadata",
  kind: "metadata.identify",
  state: "awaiting_review",
  revision: 3,
  validation: "verified",
  digest: "result",
  note: null,
  historicalResult: null,
  result: {
    outcome: "proposal",
    evidence: [
      {
        source: "urn:fixture:sleeve",
        claim: "Recorded sleeve credit",
        retrievedAt: "2026-11-20",
        limitations: [],
      },
    ],
    confidence: 0.9,
    uncertainties: [],
    recommendation: { type: "metadata" },
  },
};

it("requests metadata and cover research without a provider", () => {
  const request = vi.fn().mockResolvedValue(undefined);
  render(<RequestResearch onRequest={request} pending={false} />);
  fireEvent.click(screen.getByText("Request assistant research"));
  fireEvent.change(screen.getByRole("combobox"), {
    target: { value: "metadata+cover" },
  });
  fireEvent.change(screen.getByRole("textbox"), {
    target: { value: "Read the sleeve" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Request research" }));
  expect(request).toHaveBeenCalledWith("metadata+cover", "Read the sleeve");
});

it("shows both children and sends the exact preview digest and revision only after approval", async () => {
  const decision = vi.fn();
  const fetcher = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.endsWith("/groups/group"))
      return Response.json([
        metadata,
        {
          ...metadata,
          id: "art",
          kind: "cover.research",
          state: "queued",
          digest: null,
          result: null,
        },
      ]);
    if (url.endsWith("/preview/result"))
      return Response.json({
        revision: 4,
        digest: "preview",
        diff: {
          files: [{ from: "01 First Light.flac", to: "02 First Light.flac" }],
          tags: [
            { scope: "album", field: "artist", before: "Old", after: "New" },
          ],
        },
      });
    if (url.endsWith("/decision")) {
      decision(JSON.parse(String(init?.body)));
      return Response.json({ ok: true });
    }
    throw new Error("Unexpected request " + url);
  });
  vi.stubGlobal("fetch", fetcher);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ResearchGroup job={job} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  expect(await screen.findByText("Identify the release")).toBeTruthy();
  expect(screen.getByText("Find cover art")).toBeTruthy();
  fireEvent.click(screen.getByText("Evidence (1)"));
  expect(screen.getByText(/Recorded sleeve credit/)).toBeTruthy();
  expect(
    screen.queryByRole("button", { name: "Apply reviewed changes" }),
  ).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Preview changes" }));
  expect(await screen.findByText(/Old → New/)).toBeTruthy();
  expect(screen.getByText(/01 First Light.flac → 02 First Light.flac/)).toBeTruthy();
  expect(decision).not.toHaveBeenCalled();
  fireEvent.click(
    screen.getByRole("button", { name: "Apply reviewed changes" }),
  );
  await waitFor(() =>
    expect(decision).toHaveBeenCalledWith({
      action: "apply",
      revision: 4,
      digest: "result",
      previewDigest: "preview",
    }),
  );
});
