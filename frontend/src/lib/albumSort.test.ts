import { describe, it, expect } from "vitest";
import { parseSort, sortAlbums, DEFAULT_SORT } from "@/lib/albumSort";
import { makeAlbum } from "@/test/fixtures";

describe("parseSort", () => {
  it("returns the default for null / unknown keys", () => {
    expect(parseSort(null)).toBe(DEFAULT_SORT);
    expect(parseSort("bogus-key")).toBe(DEFAULT_SORT);
  });

  it("passes through a recognized key", () => {
    expect(parseSort("year-desc")).toBe("year-desc");
  });
});

describe("sortAlbums", () => {
  it("does not mutate the input array", () => {
    const input = [makeAlbum({ id: "1" }), makeAlbum({ id: "2" })];
    const out = sortAlbums(input, "artist-asc");
    expect(out).not.toBe(input);
    expect(input.map((a) => a.id)).toEqual(["1", "2"]);
  });

  it("sorts by artist ascending, treating compilations as 'various artists'", () => {
    const albums = [
      makeAlbum({ id: "z", album_artist: "Zappa" }),
      makeAlbum({ id: "comp", is_compilation: true, album_artist: "Aardvark" }),
      makeAlbum({ id: "a", album_artist: "ABBA" }),
    ];
    // "abba" < "various artists" < "zappa" — the compilation sorts by the
    // synthetic key, NOT its album_artist tag.
    expect(sortAlbums(albums, "artist-asc").map((a) => a.id)).toEqual(["a", "comp", "z"]);
  });

  it("sorts by year, with blank years treated as 0 (first when ascending)", () => {
    const albums = [
      makeAlbum({ id: "new", year: "2010" }),
      makeAlbum({ id: "blank", year: null }),
      makeAlbum({ id: "old", year: "1995" }),
    ];
    expect(sortAlbums(albums, "year-asc").map((a) => a.id)).toEqual(["blank", "old", "new"]);
    expect(sortAlbums(albums, "year-desc").map((a) => a.id)).toEqual(["new", "old", "blank"]);
  });

  it("breaks ties on title to avoid row-shuffle jitter", () => {
    const albums = [
      makeAlbum({ id: "2", album_artist: "Same", title: "Beta" }),
      makeAlbum({ id: "1", album_artist: "Same", title: "Alpha" }),
    ];
    expect(sortAlbums(albums, "artist-asc").map((a) => a.id)).toEqual(["1", "2"]);
  });
});
