/**
 * A long name in the rename dialog is cut short. The part that changes must
 * stay on screen, so it is a separate, marked piece that gives up room last,
 * and the whole name stays available as the tooltip.
 */
import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";

import ChangedName from "@/components/ChangedName";

const OLD = "17 - Ernest Tubb - Have You Ever Been Lonely_.flac";
const NEW = "17 - Ernest Tubb - Have You Ever Been Lonely.flac";

describe("ChangedName", () => {
  it("marks the changed part of the old name and keeps the whole name as the tooltip", () => {
    const { container } = render(<ChangedName side="old" oldName={OLD} newName={NEW} />);
    const root = container.firstElementChild as HTMLElement;
    expect(root).toHaveAttribute("title", OLD);
    expect(root).toHaveAccessibleName(OLD);
    expect(root.textContent).toBe(OLD);
    const part = root.querySelector("[data-changed-part]") as HTMLElement;
    expect(part.textContent).toBe("_");
    // The shared text gives up its room first; the changed part last.
    expect(part).not.toHaveClass("shrink-[1000]");
    expect(root.firstElementChild).toHaveClass("shrink-[1000]", "text-ellipsis");
  });

  it("the new name of a pure removal has no marked part and reads whole", () => {
    const { container } = render(<ChangedName side="new" oldName={OLD} newName={NEW} />);
    const root = container.firstElementChild as HTMLElement;
    expect(root.textContent).toBe(NEW);
    expect(root).toHaveAttribute("title", NEW);
    expect(root.querySelector("[data-changed-part]")).toBeNull();
  });

  it("keeps spaces at the edges of each piece", () => {
    const { container } = render(
      <ChangedName side="new" oldName="09 - Billy _Crash_ Craddock.flac" newName="09 - Billy - Crash - Craddock.flac" />,
    );
    const root = container.firstElementChild as HTMLElement;
    expect(root.textContent).toBe("09 - Billy - Crash - Craddock.flac");
    for (const piece of Array.from(root.children)) expect(piece).toHaveClass("whitespace-pre");
  });
});
