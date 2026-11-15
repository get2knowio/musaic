import { AlertTriangle } from "lucide-react";

/**
 * Notes from tag saves that had to rewrite a FLAC (the new tags did not
 * fit) and could not keep everything the filesystem stored about the old
 * file: an extended attribute (Finder tags, comments), the owner, or a hard
 * link. The tags were written; each line names the file and what was lost.
 */
export default function RewriteWarnings({ warnings }: { warnings: string[] }) {
  if (warnings.length === 0) return null;
  const n = warnings.length;
  return (
    <div className="mt-2 text-xs text-warn space-y-1" role="status">
      <div className="flex items-center gap-1.5 font-medium">
        <AlertTriangle className="w-3 h-3 shrink-0" />
        <span>
          {n} warning{n === 1 ? "" : "s"}: file attributes not kept
        </span>
      </div>
      <ul className="list-disc pl-5 space-y-0.5 text-fg-3">
        {warnings.map((w, i) => (
          <li key={i} className="break-words">
            {w}
          </li>
        ))}
      </ul>
    </div>
  );
}
