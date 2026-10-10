import type { ReactNode } from "react";
import "./surfacePage.css";

export function SurfacePage({ children }: { children: ReactNode }) {
  return <div className="surface-page">{children}</div>;
}
