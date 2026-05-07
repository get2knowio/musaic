import { ChevronRight } from "lucide-react";
import { useCallback, useEffect, useState, type ReactNode } from "react";

interface SectionProps {
  /** Stable id; used as the localStorage key suffix. */
  id: string;
  /** Uppercase mono section label. */
  title: string;
  /** Optional right-aligned meta text shown next to the title. */
  meta?: ReactNode;
  /** Default open state (used when no localStorage entry exists). */
  defaultOpen?: boolean;
  /** Force the section open (ignoring user toggle). For e.g. mid-apply. */
  forceOpen?: boolean;
  children: ReactNode;
}

const STORAGE_PREFIX = "mm.section.";

/**
 * Collapsible section in the detail sheet. Open state persists per-id in
 * localStorage so user choices stick across sessions.
 */
export default function Section({
  id,
  title,
  meta,
  defaultOpen = false,
  forceOpen = false,
  children,
}: SectionProps) {
  const storageKey = STORAGE_PREFIX + id;
  const [open, setOpen] = useState<boolean>(() => {
    if (forceOpen) return true;
    try {
      const stored = localStorage.getItem(storageKey);
      if (stored === "1") return true;
      if (stored === "0") return false;
    } catch {
      /* ignore */
    }
    return defaultOpen;
  });

  // Force open when requested (e.g. analysis section during a run).
  useEffect(() => {
    if (forceOpen) setOpen(true);
  }, [forceOpen]);

  const toggle = useCallback(() => {
    setOpen((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(storageKey, next ? "1" : "0");
      } catch {
        /* ignore */
      }
      return next;
    });
  }, [storageKey]);

  return (
    <div className="border-b border-line">
      <button
        type="button"
        onClick={toggle}
        className="w-full flex items-center gap-2 px-4 h-9 hover:bg-bg-2 transition text-left"
        aria-expanded={open}
      >
        <ChevronRight
          className={`w-3 h-3 text-fg-3 transition-transform duration-150 ${
            open ? "rotate-90" : ""
          }`}
        />
        <span
          className={`mono text-[11px] uppercase tracking-[0.08em] ${
            open ? "text-fg-2" : "text-fg-3"
          }`}
        >
          {title}
        </span>
        {meta !== undefined && (
          <span className="ml-auto mono text-xs text-fg-4 truncate">
            {meta}
          </span>
        )}
      </button>
      {open && <div>{children}</div>}
    </div>
  );
}
