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
            {
              filename: "01 a.flac",
              new_filename: "02 a.flac",
              error: "02 a.flac already exists",
              reason_code: "already_exists",
              user_message: "\u201c01 a.flac\u201d was not renamed to \u201c02 a.flac\u201d because a file with that name is already in the folder. Rename or move that file, then try again.",
            },
            {
              filename: "02 b.flac",
              new_filename: "03 b.flac",
              error: "Permission denied (os error 13)",
              reason_code: "permission_denied",
              user_message: "\u201c02 b.flac\u201d could not be renamed to \u201c03 b.flac\u201d because Musaic isn't allowed to change it. Check that you can change files in this folder, then try again.",
            },
            {
              filename: "03 c.flac",
              new_filename: "01 c.flac",
              error: "not renamed: the batch stopped at 02 b.flac",
              reason_code: "batch_stopped",
              user_message: "\u201c03 c.flac\u201d was not renamed to \u201c01 c.flac\u201d because renaming \u201c02 b.flac\u201d failed first. Fix that file, then try again.",
            },
          ],
        })}
      />,
    );
    expect(screen.getByText("3 files kept their old names")).toBeTruthy();
    expect(screen.getByText(/because a file with that name is already in the folder/)).toBeTruthy();
    expect(screen.getByText(/Musaic isn't allowed to change it/)).toBeTruthy();
    expect(screen.getByText(/failed first/)).toBeTruthy();
    expect(screen.getAllByRole("listitem")).toHaveLength(3);
  });

  it("never shows the raw error text", () => {
    render(
      <ApplyProgress
        state={done({
          renameFailures: [
            { filename: "a.flac", new_filename: "b.flac", error: "Permission denied (os error 13)" },
            {
              filename: "c.flac",
              new_filename: "d.flac",
              error: "Permission denied (os error 13)",
              user_message: "\u201cc.flac\u201d could not be renamed to \u201cd.flac\u201d because Musaic isn't allowed to change it.",
            },
          ],
        })}
      />,
    );
    const text = document.body.textContent ?? "";
    expect(text).not.toContain("os error");
    expect(text).not.toContain("Permission denied");
    // Without a sentence from the server the file is still named, plainly.
    expect(screen.getByText(/\u201ca\.flac\u201d could not be renamed to \u201cb\.flac\u201d/)).toBeTruthy();
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
