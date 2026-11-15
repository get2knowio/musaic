/**
 * A tag save that rewrites a FLAC reports what it could not keep of the old
 * file (`warnings` on `file_done`, and on `album_done` for the ReplayGain
 * album pass). Both streams collect them as "filename: text" and the
 * detail sheet lists them.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, renderHook, screen } from "@testing-library/react";

vi.mock("@/lib/sse", () => ({ postSse: vi.fn() }));

import { postSse } from "@/lib/sse";
import { useApplyStream } from "@/hooks/useApplyStream";
import { useAnalyzeStream } from "@/hooks/useAnalyzeStream";
import RewriteWarnings from "@/components/DetailSheet/RewriteWarnings";

type Handlers = Record<string, (data: unknown) => void>;

/** postSse that replays `frames` through the hook's handlers. */
function replay(frames: [string, unknown][]) {
  vi.mocked(postSse).mockImplementation(async (_url, _body, events: Handlers) => {
    for (const [name, data] of frames) events[name]?.(data);
  });
}

const LINK = "This file had 1 other hard link.";

beforeEach(() => {
  vi.mocked(postSse).mockReset();
});

describe("rewrite warnings", () => {
  it("apply collects file_done warnings per file", async () => {
    replay([
      ["start", { file_count: 2 }],
      ["file_started", { index: 0, filename: "01 - One.flac" }],
      ["file_done", { filename: "01 - One.flac", diffs: {}, art_changed: false, saved: true, error: null, warnings: [LINK] }],
      ["file_started", { index: 1, filename: "02 - Two.flac" }],
      ["file_done", { filename: "02 - Two.flac", diffs: {}, art_changed: false, saved: true, error: null }],
      ["done", { saved_count: 2, errors: 0, art_changed_count: 0 }],
    ]);
    const { result } = renderHook(() => useApplyStream());
    await act(() => result.current.start("abc", { suggestions: {} } as never));
    expect(result.current.state.warnings).toEqual([`01 - One.flac: ${LINK}`]);
    expect(result.current.state.filesByIndex[0].warnings).toEqual([LINK]);
    expect(result.current.state.filesByIndex[1].warnings).toEqual([]);
  });

  it("analysis collects file_done and album_done warnings", async () => {
    replay([
      ["start", { what: "replaygain", file_count: 1 }],
      ["file_started", { index: 0, filename: "01 - One.flac" }],
      ["file_done", { index: 0, filename: "01 - One.flac", replaygain_track_gain: "-1.00 dB", warnings: [LINK] }],
      ["album_done", { replaygain_album_gain: "-1.00 dB", replaygain_album_peak: "0.5", warnings: [`01 - One.flac: ${LINK}`] }],
      ["done", { what: "replaygain", analyzed_count: 1, errors: 0 }],
    ]);
    const { result } = renderHook(() => useAnalyzeStream());
    await act(() => result.current.start("abc", "replaygain"));
    expect(result.current.state.status).toBe("done");
    expect(result.current.state.warnings).toEqual([
      `01 - One.flac: ${LINK}`,
      `01 - One.flac: ${LINK}`,
    ]);
  });

  it("lists each warning, and nothing when there are none", () => {
    const { container, rerender } = render(<RewriteWarnings warnings={[]} />);
    expect(container).toBeEmptyDOMElement();
    rerender(<RewriteWarnings warnings={[`01 - One.flac: ${LINK}`]} />);
    expect(screen.getByText(/1 warning: file attributes not kept/)).toBeInTheDocument();
    expect(screen.getByText(`01 - One.flac: ${LINK}`)).toBeInTheDocument();
  });
});
