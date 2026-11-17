/**
 * A "Tags don't match canonical" finding says when MusicBrainz suggests a
 * genre for a blank one, and points at the album panel's corrections list
 * (the only way to accept it). The line shows with the finding's
 * `genre_available` detail and not without it; the value is text.
 */
import { afterEach, describe, it, expect, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

import { FindingRow } from "@/components/FindingRow";
import FindingsSection from "@/components/DetailSheet/FindingsSection";
import { suggestedGenre } from "@/lib/genreHint";
import { makeAlbum } from "@/test/fixtures";
import type { Finding } from "@/lib/types";

const finding = (detail: Finding["detail"]): Finding => ({
  id: 1,
  check_id: "tags-out-of-sync",
  album_id: "a1",
  severity: "info",
  summary: "Missing genre",
  detail,
  first_seen_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
});

const album = makeAlbum({ id: "a1", title: "T" });

function client(findings: Finding[]) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["findings"], findings);
  qc.setQueryData(["checks"], []);
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  qc.setQueryData(["jobs"], []);
  qc.setQueryData(["albums-snapshot"], []);
  return qc;
}

function row(f: Finding, a = album) {
  return render(
    <QueryClientProvider client={client([f])}>
      <MemoryRouter>
        <FindingRow checkId={f.check_id} finding={f} fixKind="deterministic" album={a} siblings={[f]} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function section(f: Finding, genreRowShown: boolean) {
  return render(
    <QueryClientProvider client={client([f])}>
      <MemoryRouter>
        <FindingsSection album={album} genreRowShown={genreRowShown} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const LINE = /MusicBrainz suggests .folk. as the genre\. Review it in the album.s corrections\./;

afterEach(cleanup);

describe("suggestedGenre", () => {
  it("reads only a non-blank string", () => {
    expect(suggestedGenre(finding({ genre_available: "folk" }))).toBe("folk");
    expect(suggestedGenre(finding({ genre_available: "  " }))).toBeNull();
    expect(suggestedGenre(finding({ genre_available: 3 }))).toBeNull();
    expect(suggestedGenre(finding({}))).toBeNull();
    expect(suggestedGenre(finding(null))).toBeNull();
  });
});

describe("Needs attention row", () => {
  it("shows the line, linking to the album, when the detail has the key", () => {
    const { container } = row(finding({ genre_available: "folk" }));
    expect(container.textContent).toMatch(LINE);
    const link = screen.getByRole("link", { name: /Review it in the album.s corrections/ });
    expect(link.getAttribute("href")).toBe("/library/a1");
  });

  it("shows nothing without the key", () => {
    const { container } = row(finding({ missing_core: ["genre"] }));
    expect(container.textContent).not.toMatch(/MusicBrainz suggests/);
  });

  it("renders the value as text, not markup", () => {
    const { container } = row(finding({ genre_available: "<b>x</b>" }));
    expect(container.querySelector("b")).toBeNull();
    expect(container.textContent).toContain("<b>x</b>");
  });

  it("shows nothing for a Missing album, whose panel has no corrections", () => {
    const { container } = row(
      finding({ genre_available: "folk" }),
      makeAlbum({ id: "a1", missing_since: "2026-01-01T00:00:00Z" }),
    );
    expect(container.textContent).not.toMatch(/MusicBrainz suggests/);
  });
});

describe("album panel findings", () => {
  it("shows the line with the key while the corrections list has the genre row", () => {
    const { container } = section(finding({ genre_available: "folk" }), true);
    expect(container.textContent).toMatch(LINE);
  });

  it("shows nothing without the key", () => {
    const { container } = section(finding({}), true);
    expect(container.textContent).toContain("Missing genre");
    expect(container.textContent).not.toMatch(/MusicBrainz suggests/);
  });

  it("shows nothing when the corrections list has no genre row", () => {
    const { container } = section(finding({ genre_available: "folk" }), false);
    expect(container.textContent).not.toMatch(/MusicBrainz suggests/);
  });
});
