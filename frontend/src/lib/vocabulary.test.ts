import { describe, it, expect } from "vitest";
import { renameTarget, sidecarsNotWritten, sidecarsNotWrittenMessage } from "@/lib/vocabulary";
import type { VocabularyResponse } from "@/lib/types";

describe("renameTarget", () => {
  it("sends a case-only rename", () => {
    expect(renameTarget("cocktail hour", "Cocktail Hour")).toBe("Cocktail Hour");
    expect(renameTarget("été", "Été")).toBe("Été");
  });

  it("sends an ordinary rename, trimmed", () => {
    expect(renameTarget("Xmas", "  Christmas ")).toBe("Christmas");
  });

  it("sends nothing for a blank or identical draft", () => {
    expect(renameTarget("Christmas", "")).toBeNull();
    expect(renameTarget("Christmas", "   ")).toBeNull();
    expect(renameTarget("Christmas", "Christmas")).toBeNull();
    expect(renameTarget("Christmas", " Christmas ")).toBeNull();
  });
});

describe("sidecarsNotWritten", () => {
  const base: VocabularyResponse = { vocabulary: [], orphans: [] };

  it("is empty when the result names none", () => {
    expect(sidecarsNotWritten(base)).toEqual([]);
    expect(sidecarsNotWritten({ ...base, rename_result: { albums: ["a"] } })).toEqual([]);
    expect(sidecarsNotWritten(undefined)).toEqual([]);
  });

  it("collects the ids from rename / merge / delete results", () => {
    expect(
      sidecarsNotWritten({ ...base, rename_result: { albums: ["a", "b"], sidecars_not_written: ["b"] } }),
    ).toEqual(["b"]);
    expect(
      sidecarsNotWritten({ ...base, merge_result: { sidecars_not_written: ["c", "d"] } }),
    ).toEqual(["c", "d"]);
    expect(
      sidecarsNotWritten({ ...base, delete_result: { sidecars_not_written: ["e"] } }),
    ).toEqual(["e"]);
  });
});

describe("sidecarsNotWrittenMessage", () => {
  it("is null for no albums", () => {
    expect(sidecarsNotWrittenMessage([], () => null)).toBeNull();
  });

  it("names the albums, falling back to the id", () => {
    const msg = sidecarsNotWrittenMessage(["a", "b"], (id) => (id === "a" ? "Album A" : null));
    expect(msg).toContain("2 albums");
    expect(msg).toContain("Album A, b");
  });

  it("shortens a long list", () => {
    const msg = sidecarsNotWrittenMessage(["a", "b", "c", "d", "e"], (id) => id);
    expect(msg).toContain("a, b, c and 2 more");
  });
});
