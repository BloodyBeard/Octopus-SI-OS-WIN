import { createContext, useContext, type ReactNode } from "react";

const WindowPaneControlsContext = createContext<HTMLElement | null>(null);

export function WindowPaneControlsProvider({ children, host }: { children: ReactNode; host: HTMLElement | null }) {
  return <WindowPaneControlsContext.Provider value={host}>{children}</WindowPaneControlsContext.Provider>;
}

export function useWindowPaneControlsHost() {
  return useContext(WindowPaneControlsContext);
}
