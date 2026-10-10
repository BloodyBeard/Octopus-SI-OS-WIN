import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";

type FixedVirtualRangeOptions = {
  count: number;
  enabled?: boolean;
  itemSize: number;
  overscan?: number;
};

/**
 * Small first-party fixed-height virtualizer for admitted client projections.
 * Its caller owns row semantics and keyboard behavior; this hook owns only the
 * viewport measurement and the stable spacer geometry.
 */
export function useFixedVirtualRange({ count, enabled = true, itemSize, overscan = 6 }: FixedVirtualRangeOptions) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [scrollElement, setScrollElement] = useState<HTMLDivElement | null>(null);
  const setScrollRef = useCallback((element: HTMLDivElement | null) => {
    scrollRef.current = element;
    setScrollElement(element);
  }, []);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewport, setViewport] = useState({ height: 0, width: 0 });

  useLayoutEffect(() => {
    if (!enabled) return;
    const element = scrollElement;
    if (!element) return;
    const measure = () => {
      const height = element.clientHeight, width = element.clientWidth;
      const top = Math.max(0, Math.min(element.scrollTop, count * itemSize - height));
      if (element.scrollTop !== top) element.scrollTop = top;
      setScrollTop(top);
      setViewport((current) => current.height === height && current.width === width ? current : { height, width });
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [count, enabled, itemSize, scrollElement]);

  const range = useMemo(() => {
    return fixedVirtualRange(count, itemSize, overscan, scrollTop, viewport.height);
  }, [count, itemSize, overscan, scrollTop, viewport.height]);

  return {
    ...range,
    onScroll: () => setScrollTop(scrollRef.current?.scrollTop ?? 0),
    scrollRef,
    setScrollRef,
    totalSize: count * itemSize,
    viewportHeight: viewport.height,
    viewportWidth: viewport.width,
  };
}

export function fixedVirtualRange(count: number, itemSize: number, overscan: number, scrollTop: number, viewportHeight: number) {
  if (count === 0) return { start: 0, end: 0 };
  const top = Math.max(0, Math.min(scrollTop, count * itemSize - viewportHeight));
  const firstVisible = Math.min(count - 1, Math.floor(top / itemSize));
  const visibleCount = Math.ceil(viewportHeight / itemSize);
  const start = Math.max(0, firstVisible - overscan);
  const end = Math.min(count, Math.max(start + 1, firstVisible + visibleCount + overscan));
  return { end, start };
}
