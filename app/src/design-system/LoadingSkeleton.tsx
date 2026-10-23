import type { CSSProperties, ReactNode } from "react";
import { useShouldReduceMotion } from "../appearance";
import "./loadingSkeleton.css";

type SkeletonProps = {
  className?: string;
  delayMs?: number;
  size?: "sm" | "md" | "lg" | "thumbnail" | "fill";
  width?: CSSProperties["width"];
};

export function OcSkeleton({ className = "", delayMs = 0, size = "md", width }: SkeletonProps) {
  const reduceMotion = useShouldReduceMotion();
  return <span
    aria-hidden="true"
    className={`oc-skeleton ${className}`}
    data-reduced-motion={reduceMotion || undefined}
    data-size={size}
    style={{ width, "--oc-skeleton-delay": `${delayMs}ms` } as CSSProperties}
  />;
}

export function OcLoadingText({ children, className = "", label }: {
  children?: ReactNode;
  className?: string;
  label: string;
}) {
  return <span aria-label={label} aria-busy="true" className={`oc-loading-text ${className}`} role="status">
    <span aria-hidden="true" className="oc-loading-text-measure">{children ?? label}</span>
    <span aria-hidden="true" className="oc-loading-text-bar"><OcSkeleton size="sm" width="100%" /></span>
  </span>;
}

export function OcLoadingRows({ className = "", label, rows = 3 }: {
  className?: string;
  label: string;
  rows?: number;
}) {
  return <div aria-label={label} aria-busy="true" className={`oc-loading-rows ${className}`} role="status">
    {Array.from({ length: rows }, (_, index) => <div className="oc-loading-row" key={index}>
      <OcSkeleton delayMs={-index * 120} width={`${[72, 56, 64][index % 3]}%`} />
    </div>)}
  </div>;
}
