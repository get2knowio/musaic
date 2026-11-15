/**
 * useResizableWidth — width of a panel docked on the RIGHT of a row, resized
 * from a handle on its left edge: dragging left makes it wider, dragging
 * right narrower.
 *
 * The user's preferred width is persisted to localStorage. The width actually
 * used is that preference clamped to `[minWidth, maxWidth]` and, when the
 * row's width is known (`available`), to what leaves `reserve` px for the
 * content beside the panel. `minWidth` wins when the row is too narrow for
 * both.
 *
 * The handle is a focusable `role="separator"`: ArrowLeft / ArrowRight change
 * the width by `step` px, Home / End jump to the minimum / maximum, and a
 * double-click restores the default.
 */
import { useCallback, useEffect, useRef, useState } from "react";

interface Options {
  /** localStorage key for persisting the width. */
  storageKey: string;
  /** Initial width if nothing's persisted; also what double-click restores. */
  defaultWidth: number;
  /** Smallest allowed width in px. */
  minWidth: number;
  /** Largest allowed width in px. */
  maxWidth: number;
  /** Width of the row the panel is docked in, when measured. */
  available?: number | null;
  /** Px to leave for the content beside the panel. */
  reserve?: number;
  /** Keyboard step in px. */
  step?: number;
}

export function useResizableWidth({
  storageKey,
  defaultWidth,
  minWidth,
  maxWidth,
  available = null,
  reserve = 0,
  step = 16,
}: Options) {
  const [preferred, setPreferred] = useState<number>(() => loadWidth(storageKey, defaultWidth));
  const [resizing, setResizing] = useState(false);
  const startXRef = useRef(0);
  const startWidthRef = useRef(0);

  const max =
    available !== null && available > 0
      ? Math.max(minWidth, Math.min(maxWidth, available - reserve))
      : maxWidth;
  const width = clamp(preferred, minWidth, max);

  // Reapply storage on mount in case the entry was added by another window.
  useEffect(() => {
    setPreferred(loadWidth(storageKey, defaultWidth));
  }, [storageKey, defaultWidth]);

  // Persist width whenever it settles (i.e. at the end of a drag).
  useEffect(() => {
    if (resizing) return;
    try { localStorage.setItem(storageKey, String(Math.round(preferred))); } catch { /* ignore */ }
  }, [preferred, resizing, storageKey]);

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
        setPreferred(clamp(startWidthRef.current + delta, minWidth, max));
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
    [width, minWidth, max],
  );

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent) => {
      let next: number | null = null;
      switch (event.key) {
        case "ArrowLeft": next = width + step; break;
        case "ArrowRight": next = width - step; break;
        case "Home": next = minWidth; break;
        case "End": next = max; break;
      }
      if (next === null) return;
      event.preventDefault();
      setPreferred(clamp(next, minWidth, max));
    },
    [width, step, minWidth, max],
  );

  const onDoubleClick = useCallback(() => setPreferred(defaultWidth), [defaultWidth]);

  return {
    width,
    min: minWidth,
    max,
    resizing,
    handleProps: { onPointerDown, onKeyDown, onDoubleClick },
  };
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
