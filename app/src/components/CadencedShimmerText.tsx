import { useEffect, useRef, type HTMLAttributes, type ReactNode } from "react";
import { useShouldReduceMotion } from "../appearance";
import "./CadencedShimmerText.css";

type Props = HTMLAttributes<HTMLElement> & {
  active?: boolean;
  as?: "div" | "span";
  highlight?: ReactNode;
};

export function CadencedShimmerText({ active = true, as: Element = "span", children, className, highlight, ...props }: Props) {
  const reduceMotion = useShouldReduceMotion();
  const elementRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    const element = elementRef.current;
    if (!active || reduceMotion || !element) return;

    let sweepTimer: number | null = null;
    let cadenceTimer: number | null = null;
    const stopSweep = () => {
      if (sweepTimer !== null) window.clearTimeout(sweepTimer);
      sweepTimer = null;
      element.classList.remove("is-shimmering");
    };
    const sweep = () => {
      stopSweep();
      element.classList.add("is-shimmering");
      sweepTimer = window.setTimeout(stopSweep, 1_000);
    };
    const startTimer = window.setTimeout(() => {
      sweep();
      cadenceTimer = window.setInterval(sweep, 4_000);
    }, 600);

    return () => {
      window.clearTimeout(startTimer);
      if (cadenceTimer !== null) window.clearInterval(cadenceTimer);
      stopSweep();
    };
  }, [active, reduceMotion]);

  return (
    <Element
      {...props}
      className={`${active ? "loading-shimmer-pure-text octopus-cadenced-shimmer" : ""}${className ? `${active ? " " : ""}${className}` : ""}` || undefined}
      ref={(element) => {
        elementRef.current = element;
      }}
    >
      {children}
      {active ? (
        <span aria-hidden="true" className="octopus-cadenced-shimmer-sweep">
          <span className="octopus-cadenced-shimmer-highlight">{highlight ?? children}</span>
        </span>
      ) : null}
    </Element>
  );
}
