import { useCallback, useRef, useState } from "react";

/**
 * A callback ref and the element's current width (null until measured, and
 * where ResizeObserver is missing). Layouts that depend on the space a
 * container is given, not on the window: the album panel beside the grid,
 * the album list's columns.
 */
export function useElementWidth<T extends HTMLElement>(): [(el: T | null) => void, number | null] {
  const [width, setWidth] = useState<number | null>(null);
  const observer = useRef<ResizeObserver | null>(null);
  const ref = useCallback((el: T | null) => {
    observer.current?.disconnect();
    observer.current = null;
    if (!el || typeof ResizeObserver === "undefined") return;
    setWidth(el.getBoundingClientRect().width);
    observer.current = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect.width;
      if (w !== undefined) setWidth(w);
    });
    observer.current.observe(el);
  }, []);
  return [ref, width];
}
