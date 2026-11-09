import { useCallback, useRef, useState, type SetStateAction } from "react";
import { usePaneTabModel, type PaneTabState } from "./paneTabs";
import type { SecondaryPaneTabState, StoredSecondaryPaneState, WorkspacePaneLayout } from "./operatorShellState";

export type WorkspacePaneState = StoredSecondaryPaneState & { layout: WorkspacePaneLayout };

export function useWorkspacePaneState(scope: string, initialState: WorkspacePaneState) {
  const [states, setStates] = useState<Record<string, WorkspacePaneState>>({});
  const state = states[scope] ?? initialState;
  const statesRef = useRef(states);
  statesRef.current = { ...states, [scope]: state };
  const getStates = useCallback(() => statesRef.current, []);
  const getStateAt = useCallback((key: string) => statesRef.current[key], []);
  const setStateAt = useCallback((key: string, update: SetStateAction<WorkspacePaneState>) => {
    setStates((current) => {
      const previous = current[key] ?? statesRef.current[key];
      if (typeof update === "function" && !previous) throw new Error("Workspace pane state is unavailable");
      const next = typeof update === "function" ? update(previous) : update;
      return next === previous ? current : { ...current, [key]: next };
    });
  }, []);
  const getState = useCallback(() => statesRef.current[scope] ?? initialState, [initialState, scope]);
  const setState = useCallback((update: SetStateAction<WorkspacePaneState>) => {
    setStates((current) => {
      const previous = current[scope] ?? initialState;
      const next = typeof update === "function" ? update(previous) : update;
      return next === previous ? current : { ...current, [scope]: next };
    });
  }, [initialState, scope]);
  const setTabState = useCallback((update: SetStateAction<PaneTabState<SecondaryPaneTabState>>) => {
    setState((current) => {
      const next = typeof update === "function" ? update(current) : update;
      return next === current ? current : { ...current, ...next };
    });
  }, [setState]);
  const tabs = usePaneTabModel<SecondaryPaneTabState>([state, setTabState]);
  const setOpen = useCallback((open: boolean) => setState((current) => ({
    ...current, open, layout: open ? current.layout : { ...current.layout, focused: false, activePane: "primary" },
  })), [setState]);
  const setFocused = useCallback((focused: boolean) => setState((current) => ({ ...current, layout: { ...current.layout, focused } })), [setState]);
  const setActivePane = useCallback((activePane: WorkspacePaneLayout["activePane"]) => setState((current) => (
    current.layout.activePane === activePane ? current : { ...current, layout: { ...current.layout, activePane } }
  )), [setState]);
  const setSidebarOpen = useCallback((sidebarOpen: boolean) => setState((current) => ({ ...current, layout: { ...current.layout, sidebarOpen } })), [setState]);
  const setSidebarWidth = useCallback((sidebarWidth: number) => setState((current) => ({ ...current, layout: { ...current.layout, sidebarWidth } })), [setState]);
  const setWidthPercent = useCallback((widthPercent: number) => setState((current) => ({ ...current, layout: { ...current.layout, widthPercent } })), [setState]);
  return { state, tabs, getState, getStateAt, getStates, setStateAt, setState, setOpen, setFocused, setActivePane, setSidebarOpen, setSidebarWidth, setWidthPercent };
}
