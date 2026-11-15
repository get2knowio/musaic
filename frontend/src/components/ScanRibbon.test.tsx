/**
 * The scan ribbon's Cancel control (issue #193): live while a scan runs,
 * "Cancelling…" and disabled once asked, and the ribbon is gone once the
 * scan ended in any way (done, cancelled, aborted, error).
 */
import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import ScanRibbon from "@/components/ScanRibbon";
import type { ScanState } from "@/hooks/useScanStream";

function scan(over: Partial<ScanState>): ScanState {
  return {
    status: "scanning",
    canCancel: true,
    total: 10,
    scanned: 3,
    cachedCount: 0,
    freshCount: 0,
    markedMissingCount: 0,
    message: null,
    scannedAt: null,
    ...over,
  };
}

describe("ScanRibbon", () => {
  it("cancels a running scan", () => {
    const onCancel = vi.fn();
    render(<ScanRibbon scan={scan({})} onCancel={onCancel} />);
    const button = screen.getByRole("button", { name: "Cancel" });
    expect(button).toBeEnabled();
    fireEvent.click(button);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("shows the cancel in progress and does not ask twice", () => {
    const onCancel = vi.fn();
    render(<ScanRibbon scan={scan({ status: "cancelling" })} onCancel={onCancel} />);
    expect(screen.getByText("Cancelling scan…")).toBeInTheDocument();
    const button = screen.getByRole("button", { name: "Cancelling…" });
    expect(button).toBeDisabled();
  });

  it("has nothing to cancel before the scan stream is open", () => {
    render(<ScanRibbon scan={scan({ status: "loading-cache", canCancel: false })} onCancel={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
  });

  it.each(["done", "cancelled", "aborted", "error", "idle"] as const)(
    "is hidden once the scan is %s",
    (status) => {
      const { container } = render(<ScanRibbon scan={scan({ status })} onCancel={vi.fn()} />);
      expect(container).toBeEmptyDOMElement();
    },
  );
});
