/**
 * Inline Ignore control for a finding row. Replaces the bare "Ignore" button
 * with a two-step popover: click → expose a small reason textarea with one
 * "Ignore finding" commit button and Cancel. The reason is optional (matches
 * the API), but capturing it stops the user from coming back three months
 * later wondering why they dismissed the finding. A finished ignore offers
 * Undo in a toast (#329): the row leaves the open list, so the offer cannot
 * live on the row. The ignored-items list with its Restore button stays the
 * way back after the toast is gone.
 *
 * The popover is implemented as a Radix DropdownMenu so we don't add a new
 * dependency. The MenuItems are intercepted with `onSelect={preventDefault}`
 * so the menu stays open while typing.
 */
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";

import { useIgnoreFinding } from "@/hooks/useChecks";
import { unignoreFinding } from "@/lib/api";

interface Props {
  checkId: string;
  findingId: number;
  /** When true, the trigger button is disabled (e.g. another mutation in
   *  flight on the same row). */
  disabled?: boolean;
}

export default function IgnoreFindingControl({ checkId, findingId, disabled }: Props) {
  const ignoreMutation = useIgnoreFinding();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // Reset draft + focus the textarea each time the popover opens.
  useEffect(() => {
    if (open) {
      setReason("");
      const id = window.setTimeout(() => inputRef.current?.focus(), 30);
      return () => window.clearTimeout(id);
    }
  }, [open]);

  const submit = () => {
    const r = reason.trim();
    ignoreMutation.mutate(
      { checkId, findingId, reason: r || undefined },
      {
        onSuccess: () => {
          setOpen(false);
          // Calls the API directly: the row (and this component) may be gone
          // by the time Undo is clicked. The checks stream refreshes the lists.
          toast.success("Finding ignored", {
            duration: 8000,
            action: {
              label: "Undo",
              onClick: () => {
                unignoreFinding(checkId, findingId).catch((e: Error) =>
                  toast.error(e.message || "Couldn't undo"),
                );
              },
            },
          });
        },
        onError: (e) => toast.error((e as Error).message || "Couldn't ignore"),
      },
    );
  };

  return (
    <DropdownMenu.Root open={open} onOpenChange={setOpen}>
      <DropdownMenu.Trigger asChild>
        <button
          disabled={disabled || ignoreMutation.isPending}
          className="h-7 px-2 rounded border border-line text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition disabled:opacity-60"
        >
          Ignore
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="end"
          sideOffset={4}
          className="w-72 rounded-md border border-line-2 bg-panel shadow-popup p-3 z-[60]"
          onCloseAutoFocus={(e) => e.preventDefault()}
        >
          <div className="mono text-[11px] uppercase tracking-[0.06em] text-fg-4 mb-1.5">
            Reason (optional)
          </div>
          <textarea
            ref={inputRef}
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder="e.g. 'Self-released, no MB record', 'White Album track listing is correct'"
            rows={3}
            maxLength={500}
            className="w-full bg-bg border border-line rounded px-2 py-1.5 text-sm placeholder:text-fg-4 focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30 resize-none"
            onKeyDown={(e) => {
              if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                e.preventDefault();
                submit();
              }
            }}
          />
          <div className="flex items-center justify-end gap-2 mt-2">
            <button
              onClick={() => setOpen(false)}
              disabled={ignoreMutation.isPending}
              className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition disabled:opacity-60"
            >
              Cancel
            </button>
            <button
              onClick={submit}
              disabled={ignoreMutation.isPending}
              className="h-7 px-2.5 rounded bg-accent text-accent-fg text-xs font-medium hover:opacity-90 transition disabled:opacity-60"
            >
              {ignoreMutation.isPending ? "Ignoring…" : "Ignore finding"}
            </button>
          </div>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
