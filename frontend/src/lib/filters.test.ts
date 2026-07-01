import { describe, it, expect } from "vitest";
import {
  applyFilter,
  buildNeedsAttentionSet,
  isValidFilterKey,
} from "@/lib/filters";
import { makeAlbum, makeJob } from "@/test/fixtures";

describe("isValidFilterKey", () => {
  it("accepts known keys and rejects everything else", () => {
    expect(isValidFilterKey("missing")).toBe(true);
    expect(isValidFilterKey("compilations")).toBe(true);
    expect(isValidFilterKey("nope")).toBe(false);
    expect(isValidFilterKey(null)).toBe(false);
  });
});

describe("buildNeedsAttentionSet", () => {
  it("includes only completed/failed jobs' album ids", () => {
    const set = buildNeedsAttentionSet([
      makeJob({ album_id: "a", status: "completed" }),
      makeJob({ album_id: "b", status: "failed" }),
      makeJob({ album_id: "c", status: "running" }),
      makeJob({ album_id: "d", status: "queued" }),
    ]);
    expect([...set].sort()).toEqual(["a", "b"]);
  });
});

describe("applyFilter", () => {
  const missingArt = makeAlbum({ id: "art", has_embedded_cover: false, cover_file: null });
  const hasArt = makeAlbum({ id: "ok", has_embedded_cover: true });
  const comp = makeAlbum({ id: "comp", is_compilation: true });
  const missing = makeAlbum({ id: "gone", missing_since: "2026-06-01T00:00:00Z" });
  const all = [missingArt, hasArt, comp, missing];

  it("returns the list unchanged when filterKey is null", () => {
    expect(applyFilter(all, null, new Set())).toBe(all);
  });

  it("missing-art matches albums with neither embedded cover nor cover file", () => {
    expect(applyFilter(all, "missing-art", new Set()).map((a) => a.id)).toEqual(["art"]);
  });

  it("compilations matches is_compilation albums", () => {
    expect(applyFilter(all, "compilations", new Set()).map((a) => a.id)).toEqual(["comp"]);
  });

  it("missing matches albums with a missing_since marker", () => {
    expect(applyFilter(all, "missing", new Set()).map((a) => a.id)).toEqual(["gone"]);
  });

  it("needs-review matches albums in the needs-attention set", () => {
    expect(applyFilter(all, "needs-review", new Set(["ok"])).map((a) => a.id)).toEqual(["ok"]);
  });
});
