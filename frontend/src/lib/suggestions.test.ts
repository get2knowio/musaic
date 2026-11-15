import { describe, expect, it } from "vitest";
import { writesValue, writtenSuggestions } from "./suggestions";

describe("writesValue", () => {
  it("is false for values the backend skips", () => {
    for (const v of [undefined, null, "", "   ", "\t\n"]) {
      expect(writesValue(v)).toBe(false);
    }
  });

  it("is true for anything with content", () => {
    expect(writesValue("Velvet Quarry")).toBe(true);
    expect(writesValue(" 7 ")).toBe(true);
  });
});

describe("writtenSuggestions", () => {
  it("drops empty and whitespace-only values, keeping order", () => {
    expect(
      Object.entries(
        writtenSuggestions({ genre: "", title: "Lanterns", artist: "  ", year: "2011" }),
      ),
    ).toEqual([
      ["title", "Lanterns"],
      ["year", "2011"],
    ]);
  });
});
