import type { WorkspacePaneTabState } from "./clientWindowModel";

export function rollbackDestinationPaneTransfer<Tab extends WorkspacePaneTabState>(
  current: { tabs: Tab[]; activeTabId: string },
  previous: { tabs: Tab[]; activeTabId: string },
  staged: Tab,
) {
  const currentTab = current.tabs.find((tab) => tab.id === staged.id);
  if (!currentTab || JSON.stringify(currentTab.target) !== JSON.stringify(staged.target)) return current;
  const original = previous.tabs.find((tab) => tab.id === staged.id);
  const tabs = original
    ? current.tabs.map((tab) => tab.id === staged.id ? original : tab)
    : current.tabs.filter((tab) => tab.id !== staged.id);
  const activeTabId = current.activeTabId !== staged.id ? current.activeTabId
    : tabs.some((tab) => tab.id === previous.activeTabId) ? previous.activeTabId : tabs[0]?.id ?? "";
  return { tabs, activeTabId };
}
