import { createContext, useContext, type ReactNode } from "react";
import { createPortal } from "react-dom";

const WorkspacePaneHeaderActionsHostContext = createContext<HTMLElement | null>(null);

export function useWorkspacePaneHeaderActionsHost() {
  return useContext(WorkspacePaneHeaderActionsHostContext);
}

export function WorkspacePaneHeaderActionsProvider({ children, host }: {
  children: ReactNode;
  host: HTMLElement | null;
}) {
  return (
    <WorkspacePaneHeaderActionsHostContext.Provider value={host}>
      {children}
    </WorkspacePaneHeaderActionsHostContext.Provider>
  );
}

export function WorkspacePaneHeaderActions({ children }: { children: ReactNode }) {
  const host = useContext(WorkspacePaneHeaderActionsHostContext);
  return host ? createPortal(children, host) : null;
}
