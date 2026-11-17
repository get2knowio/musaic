import { AlertTriangle } from "lucide-react";
import type { RenameFailure } from "@/lib/types";

/** What to say when the server gave no sentence for a file. The technical
 *  `error` text is never shown. */
function fallback(f: RenameFailure): string {
  return `“${f.filename}” could not be renamed to “${f.new_filename}”. Rescan the library, then try again.`;
}

/**
 * Files whose track number was written but which kept their old names, each
 * with the reason. The tags are saved; only the rename did not happen.
 */
export default function RenameFailures({ failures }: { failures: RenameFailure[] }) {
  if (failures.length === 0) return null;
  const n = failures.length;
  return (
    <div className="mt-2 text-xs text-warn space-y-1" role="status">
      <div className="flex items-center gap-1.5 font-medium">
        <AlertTriangle className="w-3 h-3 shrink-0" />
        <span>
          {n} file{n === 1 ? "" : "s"} kept {n === 1 ? "its" : "their"} old name{n === 1 ? "" : "s"}
        </span>
      </div>
      <ul className="list-disc pl-5 space-y-0.5 text-fg-3">
        {failures.map((f, i) => (
          <li key={i} className="break-words">
            {f.user_message || fallback(f)}
          </li>
        ))}
      </ul>
    </div>
  );
}
