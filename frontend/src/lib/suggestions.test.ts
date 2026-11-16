import { describe, expect, it } from "vitest";
import {
  correctionRows,
  correctionsPayload,
  isChange,
  writesValue,
  type CorrectionRow,
  type Proposal,
} from "./suggestions";
import { makeAlbum } from "@/test/fixtures";
import type { Track } from "./types";

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

describe("isChange (the one comparison rule)", () => {
  it("an empty or whitespace-only proposal is never a change, whatever the current value", () => {
    for (const proposed of [undefined, null, "", " ", "\t\n"]) {
      expect(isChange("Lanterns", proposed)).toBe(false);
      expect(isChange("", proposed)).toBe(false);
      expect(isChange(null, proposed)).toBe(false);
    }
  });

  it("ignores leading, trailing and repeated whitespace", () => {
    expect(isChange("Lanterns", "  Lanterns ")).toBe(false);
    expect(isChange(" Lanterns\t", "Lanterns")).toBe(false);
    expect(isChange("A  B", "A B")).toBe(false);
    expect(isChange("A\tB", " A B\n")).toBe(false);
  });

  it("counts a case-only difference as a change", () => {
    expect(isChange("So Much In Love", "So Much in Love")).toBe(true);
    expect(isChange("the beatles", "The Beatles")).toBe(true);
    expect(isChange("  the  beatles ", "The Beatles")).toBe(true);
    expect(isChange("ÉTÉ", "Été")).toBe(true);
  });

  it("counts anything else, look-alike characters included", () => {
    expect(isChange("Don't", "Don’t")).toBe(true);
    expect(isChange("So Much In Love", "Too Much Love")).toBe(true);
    expect(isChange("1999", "2001")).toBe(true);
  });

  it("filling an empty or absent tag is a change", () => {
    expect(isChange(null, "2011")).toBe(true);
    expect(isChange("", "2011")).toBe(true);
  });
});

const tracks: Track[] = [
  { filename: "01 - One.flac", tracknumber: "1", title: "One", artist: "Artist" } as Track,
  { filename: "02 - Two.flac", tracknumber: "2", title: "two", artist: "Artist" } as Track,
];

describe("correctionRows", () => {
  const album = makeAlbum({ title: "So Much In Love", album_artist: "Artist", year: "2001", genre: "Rock" });

  it("lists only real changes, in field order, then tracks, then the cover", () => {
    const proposal: Proposal = {
      album: {
        genre: "Soul",
        title: "Too Much Love",
        year: "2001",
        album_artist: "   ",
        album_artist_sort: "Artist",
      },
      tracks: [
        { filename: "02 - Two.flac", fields: { title: "Two!", artist: "ARTIST", tracknumber: "" } },
        { filename: "gone.flac", fields: { title: "Gone" } },
      ],
      cover: { url: "https://coverartarchive.org/release-group/x/front", previewSrc: "/p" },
    };
    const rows = correctionRows(proposal, album, tracks);
    expect(rows.map((r) => r.key)).toEqual([
      "album:title",
      "album:genre",
      "album:album_artist_sort",
      "track:02 - Two.flac:title",
      "track:02 - Two.flac:artist",
      "cover",
    ]);
    const title = rows[0] as Extract<CorrectionRow, { kind: "album" }>;
    expect(title.current).toBe("So Much In Love");
    expect(title.proposed).toBe("Too Much Love");
  });

  it("starts genre unticked and everything else ticked", () => {
    const rows = correctionRows(
      { album: { genre: "Jazz", year: "1999" }, tracks: [], cover: null },
      album,
      tracks,
    );
    expect(Object.fromEntries(rows.map((r) => [r.key, r.defaultSelected]))).toEqual({
      "album:year": true,
      "album:genre": false,
    });
  });

  it("has no cover row without a cover URL", () => {
    expect(
      correctionRows({ album: {}, tracks: [], cover: { url: " ", previewSrc: "/p" } }, album, tracks),
    ).toEqual([]);
  });
});

describe("correctionRows: case-only corrections", () => {
  const withAlbumTag = (album: string): Track[] =>
    tracks.map((t) => ({ ...t, album }));

  it("lists case-only album and track corrections, and none once applied", () => {
    const album = makeAlbum({ title: "the wall", album_artist: "pink floyd" });
    const proposal: Proposal = {
      album: { title: "The Wall", album_artist: "Pink Floyd" },
      tracks: [{ filename: "02 - Two.flac", fields: { title: "Two" } }],
      cover: null,
    };
    const rows = correctionRows(proposal, album, withAlbumTag("the wall"));
    expect(rows.map((r) => [r.key, r.kind !== "cover" && r.current, r.kind !== "cover" && r.proposed])).toEqual([
      ["album:title", "the wall", "The Wall"],
      ["album:album_artist", "pink floyd", "Pink Floyd"],
      ["track:02 - Two.flac:title", "two", "Two"],
    ]);
    const applied = makeAlbum({ title: "the wall", album_artist: "Pink Floyd" });
    const after = tracks.map((t) => ({ ...t, album: "The Wall", title: t.title === "two" ? "Two" : t.title }));
    expect(correctionRows(proposal, applied, after)).toEqual([]);
  });

  it("compares the title with the files' ALBUM tags, not the folder-derived stored title", () => {
    // Stored title from the folder ("the wall"), tags already "The Wall": nothing to write.
    const album = makeAlbum({ title: "the wall" });
    const proposal: Proposal = { album: { title: "The Wall" }, tracks: [], cover: null };
    expect(correctionRows(proposal, album, withAlbumTag("The Wall"))).toEqual([]);
    // One file still differs: the row shows that file's value.
    const mixed = [{ ...tracks[0], album: "The Wall" }, { ...tracks[1], album: "THE WALL" }];
    const rows = correctionRows(proposal, album, mixed);
    expect(rows).toHaveLength(1);
    expect((rows[0] as Extract<CorrectionRow, { kind: "album" }>).current).toBe("THE WALL");
    // No ALBUM tags in the listing: the stored title is the current value.
    expect(correctionRows(proposal, album, tracks).map((r) => r.key)).toEqual(["album:title"]);
  });

  it("whitespace-only differences stay no change", () => {
    const album = makeAlbum({ title: "The Wall", album_artist: "Pink Floyd" });
    const proposal: Proposal = {
      album: { title: "  The  Wall ", album_artist: "Pink\tFloyd", genre: "   " },
      tracks: [{ filename: "01 - One.flac", fields: { title: " One " } }],
      cover: null,
    };
    expect(correctionRows(proposal, album, withAlbumTag("The Wall"))).toEqual([]);
  });
});

describe("correctionsPayload", () => {
  const album = makeAlbum({ title: "Old", year: "2001" });
  const rows = correctionRows(
    {
      album: { title: "New", year: "2002", album_artist_sort: "Artist, The" },
      tracks: [
        { filename: "01 - One.flac", fields: { title: "Uno", tracknumber: "3" } },
        { filename: "02 - Two.flac", fields: { title: "Two!" } },
      ],
      cover: { url: "https://example.org/front.jpg", previewSrc: "/p" },
    },
    album,
    tracks,
  );

  it("sends exactly the selected rows and never asks to clear a tag", () => {
    const payload = correctionsPayload(
      rows,
      new Set([
        "album:title",
        "album:album_artist_sort",
        "track:01 - One.flac:tracknumber",
        "track:02 - Two.flac:title",
      ]),
    );
    expect(payload).toEqual({
      suggestions: { title: "New", album_artist_sort: "Artist, The" },
      track_suggestions: {
        "01 - One.flac": { tracknumber: "3" },
        "02 - Two.flac": { title: "Two!" },
      },
      cover_url: null,
    });
    expect(payload).not.toHaveProperty("clear");
    expect(payload).not.toHaveProperty("track_clear");
  });

  it("includes the cover only when its row is selected", () => {
    expect(correctionsPayload(rows, new Set(["cover"]))).toEqual({
      suggestions: {},
      track_suggestions: {},
      cover_url: "https://example.org/front.jpg",
    });
  });

  it("an empty selection sends nothing to write", () => {
    expect(correctionsPayload(rows, new Set())).toEqual({
      suggestions: {},
      track_suggestions: {},
      cover_url: null,
    });
  });
});
