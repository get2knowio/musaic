import { describe, expect, it } from "vitest";

import { nameDiff } from "./nameDiff";

describe("nameDiff", () => {
  it("finds a change in the middle", () => {
    expect(nameDiff("09 - Billy _Crash_ Craddock - Rub It In.flac", "09 - Billy - Crash - Craddock - Rub It In.flac")).toEqual({
      prefix: "09 - Billy ",
      oldMid: "_Crash_",
      newMid: "- Crash -",
      suffix: " Craddock - Rub It In.flac",
    });
  });

  it("finds a removal near the end of a long name", () => {
    expect(nameDiff("17 - Ernest Tubb - Have You Ever Been Lonely_.flac", "17 - Ernest Tubb - Have You Ever Been Lonely.flac")).toEqual({
      prefix: "17 - Ernest Tubb - Have You Ever Been Lonely",
      oldMid: "_",
      newMid: "",
      suffix: ".flac",
    });
  });

  it("does not count the same characters in both the start and the end", () => {
    expect(nameDiff("aa", "aaa")).toEqual({ prefix: "aa", oldMid: "", newMid: "a", suffix: "" });
  });

  it("names with nothing in common are all change", () => {
    expect(nameDiff("abc", "xyz")).toEqual({ prefix: "", oldMid: "abc", newMid: "xyz", suffix: "" });
  });

  it("equal names have no change", () => {
    expect(nameDiff("same.flac", "same.flac")).toEqual({ prefix: "same.flac", oldMid: "", newMid: "", suffix: "" });
  });

  it("keeps a character outside the basic plane whole", () => {
    const d = nameDiff("a😀b", "a😃b");
    expect(d).toEqual({ prefix: "a", oldMid: "😀", newMid: "😃", suffix: "b" });
  });
});
