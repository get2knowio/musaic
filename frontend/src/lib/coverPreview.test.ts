import { describe, it, expect } from "vitest";
import { jobCoverPreviewUrl } from "@/lib/coverPreview";

describe("jobCoverPreviewUrl", () => {
  it("is a same-origin path naming the job, never the remote URL", () => {
    expect(jobCoverPreviewUrl("0000000000000001")).toBe(
      "/api/ai/jobs/0000000000000001/cover-preview",
    );
  });

  it("encodes the job id as one path segment", () => {
    expect(jobCoverPreviewUrl("a/b?c")).toBe("/api/ai/jobs/a%2Fb%3Fc/cover-preview");
  });
});
