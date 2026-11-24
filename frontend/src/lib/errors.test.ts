import { afterEach, describe, it, expect, vi } from "vitest";
import { errorDetailText, errorText } from "@/lib/errors";
import { ApiError, enqueueJob } from "@/lib/api";
import { postSse } from "@/lib/sse";

describe("errorDetailText", () => {
  it("keeps a string detail", () => {
    expect(errorDetailText("A database restore is in progress. Try again when it finishes.")).toBe(
      "A database restore is in progress. Try again when it finishes.",
    );
  });

  it("reads the message of the job-conflict object detail", () => {
    // POST /api/ai/jobs, 409 while the album has another active
    // job (rust/crates/musaic-api/src/routes/jobs.rs `conflict`).
    expect(
      errorDetailText({
        error: "active_job_exists",
        message: "Active job j2 (queued) already exists for this album",
        existing_job_id: "j2",
        existing_status: "queued",
      }),
    ).toBe("Active job j2 (queued) already exists for this album");
  });

  it("falls back to an object's error code when it has no message", () => {
    expect(errorDetailText({ error: "active_job_exists" })).toBe("active_job_exists");
  });

  it("joins the msgs of a validation-error list", () => {
    expect(
      errorDetailText([
        { loc: ["body", "tag"], msg: "Field required", type: "missing" },
        { loc: ["body", "x"], msg: "Bad", type: "value_error" },
      ]),
    ).toBe("Field required; Bad");
  });

  it("returns null when there is nothing readable", () => {
    expect(errorDetailText(undefined)).toBeNull();
    expect(errorDetailText("")).toBeNull();
    expect(errorDetailText({ existing_job_id: "j2" })).toBeNull();
    expect(errorDetailText([])).toBeNull();
  });
});

describe("errorText", () => {
  it("uses the error's message, else the fallback", () => {
    expect(errorText(new Error("busy"), "fallback")).toBe("busy");
    expect(errorText(new Error(""), "fallback")).toBe("fallback");
    expect(errorText(null, "fallback")).toBe("fallback");
  });
});

function jsonResponse(status: number, statusText: string, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    statusText,
    headers: { "content-type": "application/json" },
  });
}

describe("request errors", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("an enqueue 409 with an object detail carries its message, not '409 Conflict'", async () => {
    const detail = {
      error: "active_job_exists",
      message: "Active job j2 (queued) already exists for this album",
      existing_job_id: "j2",
      existing_status: "queued",
    };
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(409, "Conflict", { detail })));
    const err = await enqueueJob({ albumId: "a1", jobType: "metadata" }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).message).toBe("Active job j2 (queued) already exists for this album");
    expect((err as ApiError).status).toBe(409);
    expect((err as ApiError).detail).toEqual(detail);
  });

  it("keeps the status line when the body has no detail", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(500, "Internal Server Error", {})));
    const err = await enqueueJob({ albumId: "a1", jobType: "metadata" }).catch((e: unknown) => e);
    expect((err as Error).message).toBe("500 Internal Server Error");
  });

  it("a refused SSE POST (analyze 409) throws the server's message with its status", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse(409, "Conflict", {
          detail: "A database restore is in progress. Try again when it finishes.",
        }),
      ),
    );
    const err = await postSse("/api/albums/a/analyze/stream", { what: "bpm" }, {}).catch(
      (e: unknown) => e,
    );
    expect((err as Error).message).toBe(
      "A database restore is in progress. Try again when it finishes.",
    );
    expect((err as { status?: number }).status).toBe(409);
  });
});
