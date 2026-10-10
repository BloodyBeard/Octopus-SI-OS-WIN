import { useState } from "react";
import { cubicCirclePath, threeQuarterRingPath } from "../icon-glyphs/geometric-primitives";

export function OctopusSpinner({ className, duration }: { className?: string; duration?: number }) {
  const [animationDelay] = useState(() => `-${Date.now() % 1000}ms`);
  return <span aria-hidden="true" className="inline-flex h-fit w-fit items-center justify-center leading-none contain-layout contain-paint contain-style motion-safe:animate-spin" style={{ animationDelay, animationDuration: duration == null ? undefined : `${duration}ms` }}>
    <svg className={className} width="24" height="24" viewBox="0 0 24 24" fill="none">
      <path d={cubicCirclePath(12, 12, 8, 8 * 4 * (Math.sqrt(2) - 1) / 3) + cubicCirclePath(12, 12, 6, 8 * (Math.sqrt(2) - 1))} fill="currentColor" fillRule="evenodd" opacity=".3" />
      <path d={threeQuarterRingPath(12, 12, 8, 6)} fill="currentColor" />
    </svg>
  </span>;
}
