import type { ReactNode } from "react";
import { OctopusSpinner } from "./OctopusSpinner";

export function OctopusLoadingState({ children }: { children: ReactNode }) {
  return <div className="flex items-center gap-2 py-2 text-sm text-secondary" role="status">
    <OctopusSpinner className="icon-xs shrink-0 text-secondary" />
    <span className="text-balance">{children}</span>
  </div>;
}
