import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import ApplyProgress from "./ApplyProgress";
import { INITIAL_APPLY_STATE, type ApplyState } from "@/lib/applyRegistry";

function done(over: Partial<ApplyState> = {}): ApplyState {
  return { ...INITIAL_APPLY_STATE, status: "done", fileCount: 3, scannedCount: 3, savedCount: 3, ...over };
}

describe("ApplyProgress: files that kept their old names", () => {
  it("lists each file with its reason", () => {
    render(
      <ApplyProgress
        state={done({
          renameFailures: [
            { filename: "01 a.flac", new_filename: "02 a.flac", error: "02 a.flac already exists" },
            { filename: "02 b.flac", new_filename: "03 b.flac", error: "Permission denied" },
            { filename: "03 c.flac", new_filename: "01 c.flac", error: "file is in use" },
          ],
        })}
      />,
    );
    expect(screen.getByText("3 files kept their old names")).toBeTruthy();
    expect(screen.getByText(/02 a\.flac already exists/)).toBeTruthy();
    expect(screen.getByText(/Permission denied/)).toBeTruthy();
    expect(screen.getAllByRole("listitem")).toHaveLength(3);
  });

  it("uses the singular for one file", () => {
    render(
      <ApplyProgress
        state={done({ renameFailures: [{ filename: "a.flac", new_filename: "b.flac", error: "busy" }] })}
      />,
    );
    expect(screen.getByText("1 file kept its old name")).toBeTruthy();
  });

  it("shows nothing about renames when the list is empty", () => {
    render(<ApplyProgress state={done()} />);
    expect(screen.queryByText(/old name/)).toBeNull();
  });
});
