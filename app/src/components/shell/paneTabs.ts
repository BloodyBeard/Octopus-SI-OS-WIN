import { useCallback, useState, type Dispatch, type SetStateAction } from "react";

export type PaneTabIdentity = {
  id: string;
};

export type PaneTabState<Tab extends PaneTabIdentity> = {
  activeTabId: string;
  tabs: Tab[];
};

export type RestoredPaneTabs<Tab extends PaneTabIdentity> = PaneTabState<Tab> & {
  missingTargetIds: string[];
};

type PaneTabModelOptions = {
  allowEmptySelection?: boolean;
};

export function restorePaneTabs<StoredTab, Tab extends PaneTabIdentity>({
  activeTabId,
  allowEmptySelection = false,
  maximum = 24,
  resolve,
  storedTabs,
}: {
  activeTabId: string;
  allowEmptySelection?: boolean;
  maximum?: number;
  resolve: (stored: StoredTab) => { id: string; tab: Tab | null } | null;
  storedTabs: StoredTab[];
}): RestoredPaneTabs<Tab> {
  const tabs: Tab[] = [];
  const missingTargetIds: string[] = [];
  const seen = new Set<string>();
  for (const stored of storedTabs.slice(0, maximum)) {
    const resolved = resolve(stored);
    if (!resolved || !resolved.id || seen.has(resolved.id)) continue;
    seen.add(resolved.id);
    if (resolved.tab) tabs.push(resolved.tab);
    else missingTargetIds.push(resolved.id);
  }
  const state = normalizePaneTabState(tabs, activeTabId, allowEmptySelection);
  return { ...state, missingTargetIds };
}

export function usePaneTabs<Tab extends PaneTabIdentity>(
  initialState: PaneTabState<Tab>,
  options: PaneTabModelOptions = {},
) {
  const allowEmptySelection = options.allowEmptySelection === true;
  const [state, setState] = useState<PaneTabState<Tab>>(() => (
    normalizePaneTabState(initialState.tabs, initialState.activeTabId, allowEmptySelection)
  ));

  const replaceState = useCallback((next: PaneTabState<Tab>) => {
    setState(normalizePaneTabState(next.tabs, next.activeTabId, allowEmptySelection));
  }, [allowEmptySelection]);

  const setTabs: Dispatch<SetStateAction<Tab[]>> = useCallback((update) => {
    setState((current) => {
      const tabs = typeof update === "function" ? update(current.tabs) : update;
      return normalizePaneTabState(tabs, current.activeTabId, allowEmptySelection);
    });
  }, [allowEmptySelection]);

  const reconcileTabs = useCallback((update: Tab[] | ((tabs: Tab[]) => Tab[])) => {
    setState((current) => {
      const tabs = typeof update === "function" ? update(current.tabs) : update;
      const next = normalizePaneTabState(tabs, current.activeTabId, allowEmptySelection);
      return samePaneTabState(current, next) ? current : next;
    });
  }, [allowEmptySelection]);

  const selectTab = useCallback((tabId: string) => {
    setState((current) => {
      if (!tabId && allowEmptySelection) return current.activeTabId ? { ...current, activeTabId: "" } : current;
      if (!current.tabs.some((tab) => tab.id === tabId) || current.activeTabId === tabId) return current;
      return { ...current, activeTabId: tabId };
    });
  }, [allowEmptySelection]);

  const openTab = useCallback((tab: Tab, replaceTabId?: string) => {
    setState((current) => {
      const index = current.tabs.findIndex((candidate) => candidate.id === tab.id);
      const replaceIndex = current.tabs.findIndex((candidate) => candidate.id === replaceTabId);
      const tabs = index >= 0
        ? current.tabs.filter((candidate) => candidate.id !== replaceTabId || candidate.id === tab.id)
          .map((candidate) => candidate.id === tab.id ? tab : candidate)
        : replaceIndex >= 0
          ? current.tabs.map((candidate, candidateIndex) => candidateIndex === replaceIndex ? tab : candidate)
          : [...current.tabs, tab];
      return { activeTabId: tab.id, tabs };
    });
  }, []);

  const replaceTab = useCallback((tabId: string, tab: Tab) => {
    setState((current) => {
      const index = current.tabs.findIndex((candidate) => candidate.id === tabId);
      if (index < 0) return current;
      const withoutReplacement = current.tabs.filter((candidate) => candidate.id !== tab.id || candidate.id === tabId);
      const replacementIndex = withoutReplacement.findIndex((candidate) => candidate.id === tabId);
      if (replacementIndex < 0) return current;
      const tabs = withoutReplacement.map((candidate, candidateIndex) => candidateIndex === replacementIndex ? tab : candidate);
      return {
        activeTabId: current.activeTabId === tabId ? tab.id : current.activeTabId,
        tabs,
      };
    });
  }, []);

  const closeTab = useCallback((tabId: string) => {
    setState((current) => closePaneTab(current, tabId, allowEmptySelection));
  }, [allowEmptySelection]);

  const closeOtherTabs = useCallback((tabId: string) => {
    setState((current) => {
      const tab = current.tabs.find((candidate) => candidate.id === tabId);
      return tab ? { activeTabId: tab.id, tabs: [tab] } : current;
    });
  }, []);

  const closeTabsToRight = useCallback((tabId: string) => {
    setState((current) => {
      const index = current.tabs.findIndex((tab) => tab.id === tabId);
      if (index < 0 || index === current.tabs.length - 1) return current;
      const tabs = current.tabs.slice(0, index + 1);
      return {
        activeTabId: tabs.some((tab) => tab.id === current.activeTabId) ? current.activeTabId : tabId,
        tabs,
      };
    });
  }, []);

  const reorderTabs = useCallback((activeTabId: string, overTabId: string) => {
    setState((current) => {
      const from = current.tabs.findIndex((tab) => tab.id === activeTabId);
      const to = current.tabs.findIndex((tab) => tab.id === overTabId);
      if (from < 0 || to < 0 || from === to) return current;
      const tabs = [...current.tabs];
      const [moved] = tabs.splice(from, 1);
      if (!moved) return current;
      tabs.splice(to, 0, moved);
      return { ...current, tabs };
    });
  }, []);

  return {
    activeTabId: state.activeTabId,
    closeOtherTabs,
    closeTab,
    closeTabsToRight,
    openTab,
    reconcileTabs,
    reorderTabs,
    replaceState,
    replaceTab,
    selectTab,
    setTabs,
    tabs: state.tabs,
  };
}

function normalizePaneTabState<Tab extends PaneTabIdentity>(
  tabs: Tab[],
  requestedActiveTabId: string,
  allowEmptySelection: boolean,
): PaneTabState<Tab> {
  const uniqueTabs: Tab[] = [];
  const seen = new Set<string>();
  for (const tab of tabs) {
    if (!tab.id || seen.has(tab.id)) continue;
    seen.add(tab.id);
    uniqueTabs.push(tab);
  }
  const activeTabId = requestedActiveTabId && uniqueTabs.some((tab) => tab.id === requestedActiveTabId)
    ? requestedActiveTabId
    : allowEmptySelection && !requestedActiveTabId
      ? ""
      : uniqueTabs[0]?.id ?? "";
  return { activeTabId, tabs: uniqueTabs };
}

function samePaneTabState<Tab extends PaneTabIdentity>(
  left: PaneTabState<Tab>,
  right: PaneTabState<Tab>,
): boolean {
  return left.activeTabId === right.activeTabId
    && left.tabs.length === right.tabs.length
    && left.tabs.every((tab, index) => tab === right.tabs[index]);
}

export function closePaneTab<Tab extends PaneTabIdentity>(
  current: PaneTabState<Tab>,
  tabId: string,
  allowEmptySelection: boolean,
): PaneTabState<Tab> {
  const index = current.tabs.findIndex((tab) => tab.id === tabId);
  if (index < 0) return current;
  const tabs = current.tabs.filter((tab) => tab.id !== tabId);
  if (current.activeTabId !== tabId) return { ...current, tabs };
  const activeTabId = tabs[Math.min(index, tabs.length - 1)]?.id
    ?? (allowEmptySelection ? "" : tabs[0]?.id ?? "");
  return { activeTabId, tabs };
}
