/**
 * The apply registry keeps an album's apply stream and progress outside the
 * album panel: the panel may show another album or close mid-write (the
 * server finishes the apply regardless) and the progress is there again on
 * reopening. A settled result is kept until a view has shown it.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";

vi.mock("@/lib/sse", () => ({ postSse: vi.fn() }));

import { postSse } from "@/lib/sse";
import {
  getApplyState,
  resetApplyRegistryForTests,
  startApply,
  useApplyState,
} from "@/lib/applyRegistry";
import { makeAlbum } from "@/test/fixtures";

type Handlers = Record<string, (data: unknown) => void>;

/** postSse that hands its handlers to the test and resolves on demand. */
function controllable() {
  let handlers: Handlers = {};
  let finish: () => void = () => {};
  vi.mocked(postSse).mockImplementation(
    (_url, _body, events: Handlers) =>
      new Promise<void>((resolve) => {
        handlers = events;
        finish = resolve;
      }),
  );
  return {
    emit: (name: string, data: unknown) => act(() => handlers[name]?.(data)),
    finish: async () => {
      await act(async () => finish());
    },
  };
}

function Progress({ albumId }: { albumId: string }) {
  const s = useApplyState(albumId);
  return <div data-testid="progress">{`${s.status} ${s.scannedCount}/${s.fileCount}`}</div>;
}

const flushMicrotasks = () => act(async () => {});

beforeEach(() => {
  vi.mocked(postSse).mockReset();
  resetApplyRegistryForTests();
});

describe("apply registry", () => {
  it("keeps progress across unmount and remount, and reports completion while unmounted", async () => {
    const sse = controllable();
    const onUpdatedAlbum = vi.fn();
    const view = render(<Progress albumId="a1" />);

    let run!: Promise<void>;
    act(() => {
      run = startApply("a1", { suggestions: {} } as never, { onUpdatedAlbum });
    });
    await sse.emit("start", { file_count: 2 });
    await sse.emit("file_started", { index: 0, filename: "01.flac" });
    await sse.emit("file_done", { filename: "01.flac", diffs: {}, art_changed: false, saved: true, error: null });
    expect(screen.getByTestId("progress")).toHaveTextContent("writing 1/2");

    // The panel moves on mid-write; the stream keeps going.
    view.unmount();
    await flushMicrotasks();
    await sse.emit("file_started", { index: 1, filename: "02.flac" });
    await sse.emit("file_done", { filename: "02.flac", diffs: {}, art_changed: false, saved: true, error: null });
    expect(getApplyState("a1").scannedCount).toBe(2);

    // Reopened: the progress is back.
    const again = render(<Progress albumId="a1" />);
    expect(screen.getByTestId("progress")).toHaveTextContent("writing 2/2");
    again.unmount();
    await flushMicrotasks();

    const updated = makeAlbum({ id: "a1", title: "Renamed" });
    await sse.emit("done", { saved_count: 2, errors: 0, art_changed_count: 0 });
    await sse.emit("updated_album", updated);
    await sse.finish();
    await run;
    expect(onUpdatedAlbum).toHaveBeenCalledWith(updated);

    // Finished while nobody looked: shown once on reopening, then dropped.
    const third = render(<Progress albumId="a1" />);
    expect(screen.getByTestId("progress")).toHaveTextContent("done 2/2");
    third.unmount();
    await flushMicrotasks();
    expect(getApplyState("a1").status).toBe("idle");
  });

  it("keeps a settled result through an immediate remount (StrictMode, quick swap back)", async () => {
    const sse = controllable();
    const view = render(<Progress albumId="a1" />);
    act(() => {
      void startApply("a1", { suggestions: {} } as never);
    });
    await sse.emit("start", { file_count: 1 });
    await sse.emit("done", { saved_count: 1, errors: 0, art_changed_count: 0 });
    await sse.finish();

    view.unmount();
    render(<Progress albumId="a1" />); // before the deferred release runs
    await flushMicrotasks();
    expect(screen.getByTestId("progress")).toHaveTextContent("done 0/1");
  });

  it("tracks albums independently and refuses a second start while one runs", async () => {
    controllable();
    act(() => {
      void startApply("a1", { suggestions: {} } as never);
    });
    await act(async () => {
      await startApply("a1", { suggestions: {} } as never);
    });
    expect(postSse).toHaveBeenCalledTimes(1);
    expect(getApplyState("a1").status).toBe("writing");
    expect(getApplyState("a2").status).toBe("idle");
  });

  it("a stream that ends without a terminal frame is reported as an error", async () => {
    const sse = controllable();
    let run!: Promise<void>;
    act(() => {
      run = startApply("a1", { suggestions: {} } as never);
    });
    await sse.emit("start", { file_count: 3 });
    await sse.finish();
    await run;
    expect(getApplyState("a1").status).toBe("error");
  });
});
