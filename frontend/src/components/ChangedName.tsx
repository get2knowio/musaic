import { nameDiff } from "@/lib/nameDiff";

/**
 * One side of an old → new name pair, cut short so that the part that
 * changes stays on screen: the text the two names share (before and after
 * the change) gives up its room first, and the changed part is marked.
 * The whole name is the tooltip and what a screen reader reads.
 */
export default function ChangedName({
  oldName,
  newName,
  side,
  className = "",
}: {
  oldName: string;
  newName: string;
  side: "old" | "new";
  className?: string;
}) {
  const d = nameDiff(oldName, newName);
  const full = side === "old" ? oldName : newName;
  const mid = side === "old" ? d.oldMid : d.newMid;
  // Shared text shrinks long before the changed part does (flex-shrink is
  // weighted), but keeps a couple of characters so the place is recognisable.
  const shared = "shrink-[1000] min-w-[2ch] overflow-hidden text-ellipsis whitespace-pre";
  return (
    <div className={`flex min-w-0 overflow-hidden ${className}`} title={full} aria-label={full} data-changed-name={side}>
      {d.prefix && <span className={shared}>{d.prefix}</span>}
      {/* Bold and underlined, not a tint of the text colour: where colour
          mixing is unsupported a tint falls back to the solid colour and
          would hide the text. */}
      {mid && (
        <span
          data-changed-part
          className="shrink min-w-0 overflow-hidden text-ellipsis whitespace-pre font-bold underline underline-offset-2"
        >
          {mid}
        </span>
      )}
      {d.suffix && <span className={shared}>{d.suffix}</span>}
    </div>
  );
}
