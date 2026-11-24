import { describe, it, expect } from "vitest";
import { modelFreeCoverage } from "@/lib/aiGating";
import type { AlbumResolution } from "@/lib/types";

function resolution(over: {
  anchor?: number | null;
  cover?: number | null;
  unresolved?: number;
}): AlbumResolution {
  const fact = (c: number) => ({ value: "x", source: "musicbrainz" as const, confidence: c });
  return {
    anchor: over.anchor == null ? null : fact(over.anchor),
    cover_url: over.cover == null ? null : fact(over.cover),
    unresolved_track_count: over.unresolved ?? 0,
  } as unknown as AlbumResolution;
}

describe("modelFreeCoverage (the resolver's thresholds)", () => {
  it("covers metadata at anchor confidence 0.85 with every track resolved", () => {
    expect(modelFreeCoverage(resolution({ anchor: 0.85 })).metadata).toBe(true);
    expect(modelFreeCoverage(resolution({ anchor: 0.84 })).metadata).toBe(false);
    expect(modelFreeCoverage(resolution({ anchor: 0.99, unresolved: 1 })).metadata).toBe(false);
    expect(modelFreeCoverage(resolution({ anchor: null })).metadata).toBe(false);
  });

  it("covers the cover at cover confidence 0.85", () => {
    expect(modelFreeCoverage(resolution({ cover: 0.85 })).cover).toBe(true);
    expect(modelFreeCoverage(resolution({ cover: 0.5 })).cover).toBe(false);
    expect(modelFreeCoverage(resolution({ cover: null })).cover).toBe(false);
  });

  it("covers nothing while the resolution is loading", () => {
    expect(modelFreeCoverage(undefined)).toEqual({ metadata: false, cover: false });
  });
});
