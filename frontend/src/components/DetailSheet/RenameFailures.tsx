import { AlertTriangle } from "lucide-react";
import type { RenameFailure } from "@/lib/types";

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
            <span className="mono">{f.filename}</span> could not be renamed to{" "}
            <span className="mono">{f.new_filename}</span>: {f.error}
          </li>
        ))}
      </ul>
    </div>
  );
}
