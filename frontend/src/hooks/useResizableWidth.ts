/**
 * useResizableWidth — drives the DetailSheet's drag-to-resize behavior.
 *
 * The handle is anchored on the LEFT edge of a right-anchored panel: dragging
 * left makes it wider, dragging right makes it narrower. Width is bounded
 * between `min` and `min(max, 95vw)` and persisted to localStorage.
 */
import { useCallback, useEffect, useRef, useState } from "react";

interface Options {
  /** localStorage key for persisting the width. */
  storageKey: string;
  /** Initial width if nothing's persisted. */
  defaultWidth: number;
  /** Smallest allowed width in px. */
  minWidth: number;
  /** Largest allowed width in px (additionally clamped to 95vw). */
  maxWidth: number;
}

export function useResizableWidth({ storageKey, defaultWidth, minWidth, maxWidth }: Options) {
  const [width, setWidth] = useState<number>(() => loadWidth(storageKey, defaultWidth));
  const [resizing, setResizing] = useState(false);
  const startXRef = useRef(0);
  const startWidthRef = useRef(0);

  // Reapply storage on mount in case the entry was added by another tab/window.
  useEffect(() => {
    setWidth(clamp(loadWidth(storageKey, defaultWidth), minWidth, maxWidth));
  }, [storageKey, defaultWidth, minWidth, maxWidth]);

  // Persist width whenever it settles (i.e. at the end of a drag).
  useEffect(() => {
    if (resizing) return;
    try { localStorage.setItem(storageKey, String(Math.round(width))); } catch { /* ignore */ }
  }, [width, resizing, storageKey]);

  const onPointerDown = useCallback(
    (event: React.PointerEvent) => {
      // Left button only.
      if (event.button !== 0) return;
      event.preventDefault();
      startXRef.current = event.clientX;
      startWidthRef.current = width;
      setResizing(true);

      const onMove = (e: PointerEvent) => {
        // Right-anchored panel: dragging cursor LEFT (smaller clientX) widens.
        const delta = startXRef.current - e.clientX;
        const next = clamp(startWidthRef.current + delta, minWidth, Math.min(maxWidth, window.innerWidth * 0.95));
        setWidth(next);
      };
      const onUp = () => {
        setResizing(false);
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
      };
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
      // Lock the cursor + suppress text selection during drag.
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
    },
    [width, minWidth, maxWidth],
  );

  return { width, resizing, onPointerDown };
}

function loadWidth(key: string, fallback: number): number {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return fallback;
    const n = parseInt(raw, 10);
    if (Number.isFinite(n) && n > 0) return n;
  } catch { /* ignore */ }
  return fallback;
}

function clamp(n: number, lo: number, hi: number): number {
  return Math.min(Math.max(n, lo), hi);
}
