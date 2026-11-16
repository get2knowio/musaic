import { afterEach, describe, it, expect, vi } from "vitest";
import { albumCoverPreviewUrl, fetchCoverPreview, jobCoverPreviewUrl } from "@/lib/coverPreview";

describe("preview URLs", () => {
  it("are same-origin paths naming the job or the album, never the remote URL", () => {
    expect(jobCoverPreviewUrl("0000000000000001")).toBe(
      "/api/ai/jobs/0000000000000001/cover-preview",
    );
    expect(albumCoverPreviewUrl("abcdef0123456789")).toBe(
      "/api/albums/abcdef0123456789/cover-preview",
    );
  });

  it("encode the id as one path segment", () => {
    expect(jobCoverPreviewUrl("a/b?c")).toBe("/api/ai/jobs/a%2Fb%3Fc/cover-preview");
    expect(albumCoverPreviewUrl("a/b?c")).toBe("/api/albums/a%2Fb%3Fc/cover-preview");
  });
});

describe("fetchCoverPreview", () => {
  afterEach(() => vi.unstubAllGlobals());

  const answer = (res: Response | Error) =>
    vi.stubGlobal("fetch", vi.fn(async () => {
      if (res instanceof Error) throw res;
      return res;
    }));

  it("an image becomes a data: URL", async () => {
    answer(new Response(new Uint8Array([0xff, 0xd8, 0xff]), { headers: { "content-type": "image/jpeg" } }));
    const p = await fetchCoverPreview("/x");
    expect(p.status).toBe("ok");
    expect(p.status === "ok" && p.dataUrl).toBe("data:image/jpeg;base64,/9j/");
  });

  it("404 is nothing to preview, 409 a Missing album, 502 / failures unavailable", async () => {
    answer(new Response("{}", { status: 404 }));
    expect(await fetchCoverPreview("/x")).toEqual({ status: "none" });
    answer(new Response("{}", { status: 409 }));
    expect(await fetchCoverPreview("/x")).toEqual({ status: "missing" });
    answer(new Response("{}", { status: 502 }));
    expect(await fetchCoverPreview("/x")).toEqual({ status: "unavailable" });
    answer(new TypeError("network"));
    expect(await fetchCoverPreview("/x")).toEqual({ status: "unavailable" });
    answer(new Response("<html>", { status: 200, headers: { "content-type": "text/html" } }));
    expect(await fetchCoverPreview("/x")).toEqual({ status: "unavailable" });
  });
});
