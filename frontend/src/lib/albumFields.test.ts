import { describe, it, expect } from "vitest";
import { displayArtist } from "@/lib/albumFields";

describe("displayArtist", () => {
  it("prefers album_artist when present", () => {
    expect(displayArtist({ album_artist: "Henry Mancini", artist: "Orchestra" })).toBe(
      "Henry Mancini",
    );
  });

  it("falls back to artist when album_artist is empty (the Mancini case)", () => {
    // A library carrying only an ARTIST tag must not lose the artist.
    expect(displayArtist({ album_artist: "", artist: "Henry Mancini" })).toBe(
      "Henry Mancini",
    );
    expect(displayArtist({ album_artist: null, artist: "Henry Mancini" })).toBe(
      "Henry Mancini",
    );
  });

  it("trims whitespace and treats blank-only values as empty", () => {
    expect(displayArtist({ album_artist: "   ", artist: "Real" })).toBe("Real");
    expect(displayArtist({ album_artist: "  Spaced  ", artist: "x" })).toBe("Spaced");
  });

  it("returns the fallback when nothing is set", () => {
    expect(displayArtist({ album_artist: null, artist: null })).toBe("");
    expect(displayArtist({ album_artist: null, artist: null }, "Unknown artist")).toBe(
      "Unknown artist",
    );
  });
});
