import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { Glyph } from "../IconGlyphs";

export function CollapsibleMessageBody({ children, maxLines = 20 }: { children: ReactNode; maxLines?: number }) {
  const contentRef = useRef<HTMLDivElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [collapsedHeight, setCollapsedHeight] = useState(0);
  const [overflowing, setOverflowing] = useState(false);

  useLayoutEffect(() => {
    const content = contentRef.current;
    if (!content) return;
    const measure = () => {
      const style = window.getComputedStyle(content);
      const fontSize = Number.parseFloat(style.fontSize) || 15;
      const lineHeight = Number.parseFloat(style.lineHeight) || fontSize * 1.5;
      const limit = Math.ceil(lineHeight * maxLines);
      setCollapsedHeight(limit);
      setOverflowing(content.scrollHeight > limit + 2);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(content);
    return () => observer.disconnect();
  }, [children, maxLines]);

  return (
    <div className={`collapsible-message${overflowing ? " is-overflowing" : ""}${expanded ? " is-expanded" : ""}`}>
      <div
        className="collapsible-message-content"
        style={overflowing && !expanded ? { maxHeight: collapsedHeight } : undefined}
      >
        <div ref={contentRef}>{children}</div>
        {overflowing && !expanded ? <span aria-hidden="true" className="collapsible-message-fade" /> : null}
      </div>
      {overflowing ? (
        <button aria-expanded={expanded} className="collapsible-message-toggle" onClick={() => setExpanded((current) => !current)} type="button">
          <span>{expanded ? "Show less" : "Show more"}</span>
          <Glyph className={`icon-2xs transition-transform${expanded ? " rotate-180" : ""}`} name="chevronDown" size={12} />
        </button>
      ) : null}
    </div>
  );
}
