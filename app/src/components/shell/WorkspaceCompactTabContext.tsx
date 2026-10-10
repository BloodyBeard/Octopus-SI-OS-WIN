import { createContext, useContext, type PointerEvent, type ReactNode } from "react";

export type WorkspaceCompactTabDrag = {
  tabId: string;
  onPointerDown?: (event: PointerEvent<HTMLElement>) => void;
};

const WorkspaceCompactTabContext = createContext<WorkspaceCompactTabDrag | null>(null);

export function WorkspaceCompactTabProvider({ children, value }: {
  children: ReactNode;
  value: WorkspaceCompactTabDrag | null;
}) {
  return <WorkspaceCompactTabContext.Provider value={value}>{children}</WorkspaceCompactTabContext.Provider>;
}

export function useWorkspaceCompactTab(): WorkspaceCompactTabDrag | null {
  return useContext(WorkspaceCompactTabContext);
}
