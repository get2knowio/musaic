/**
 * The suggested cover renders from the same-origin preview route,
 * never from the remote `cover_url` (the desktop CSP blocks remote images).
 */
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import AISuggestions from "@/components/DetailSheet/AISuggestions";
import { jobCoverPreviewUrl } from "@/lib/coverPreview";
import type { CoverSuggestionResponse } from "@/lib/types";
import { makeAlbum } from "@/test/fixtures";

const REMOTE = "https://coverartarchive.org/release-group/0d9e7b1c/front";

const cover = (url: string | null): CoverSuggestionResponse => ({
  album_id: "album-1",
  cover_url: url,
  source: "MusicBrainz / Cover Art Archive",
  notes: "",
  confidence: "high",
  raw_excerpt: "",
});

function renderIt(coverUrl: string | null, coverPreviewSrc?: string | null) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const album = makeAlbum({ id: "album-1" });
  qc.setQueryData(["tracks", album.id], { tracks: [] });
  const noop = () => {};
  return render(
    <QueryClientProvider client={qc}>
      <AISuggestions
        album={album}
        metadata={null}
        cover={cover(coverUrl)}
        onAccept={noop}
        onDiscard={noop}
        onClearAnchor={noop}
        clearingAnchor={false}
        applying={false}
        coverPreviewSrc={coverPreviewSrc}
      />
    </QueryClientProvider>,
  );
}

describe("AISuggestions cover preview", () => {
  it("shows the proposed cover through the job's same-origin preview route", () => {
    const src = jobCoverPreviewUrl("0000000000000001");
    renderIt(REMOTE, src);
    const img = screen.getByAltText("proposed cover") as HTMLImageElement;
    expect(img.getAttribute("src")).toBe(src);
    expect(img.getAttribute("src")).not.toContain("coverartarchive.org");
    // The remote URL is still offered as a link (opened outside the app).
    expect(screen.getByRole("link", { name: REMOTE }).getAttribute("href")).toBe(REMOTE);
  });

  it("renders no remote <img> when there is no preview route", () => {
    const { container } = renderIt(REMOTE, null);
    expect(screen.queryByAltText("proposed cover")).toBeNull();
    for (const img of Array.from(container.querySelectorAll("img"))) {
      expect(img.getAttribute("src") ?? "").not.toMatch(/^https?:/);
    }
  });

  it("shows the empty-cover note and no image when nothing was found", () => {
    renderIt(null, jobCoverPreviewUrl("0000000000000002"));
    expect(screen.queryByAltText("proposed cover")).toBeNull();
  });
});
