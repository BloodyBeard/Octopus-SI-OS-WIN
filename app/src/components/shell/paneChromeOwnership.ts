export type PaneChromeOwner = "window-titlebar" | "pane-tab-strip" | "pane-content-header" | "pane-launcher";

type PaneChromeTab = {
  showWhenSingle?: boolean;
};

export function paneChromeOwner(
  pane: "primary" | "secondary",
  tabs: readonly PaneChromeTab[],
): PaneChromeOwner {
  if (pane === "primary") return "window-titlebar";
  if (tabs.length === 0) return "pane-launcher";
  if (tabs.length > 1 || tabs[0]?.showWhenSingle === true) return "pane-tab-strip";
  return "pane-content-header";
}

export function paneOwnsWindowTitlebar(pane: "primary" | "secondary"): boolean {
  return paneChromeOwner(pane, []) === "window-titlebar";
}

export function secondaryPaneUsesTabStrip(tabs: readonly PaneChromeTab[]): boolean {
  return paneChromeOwner("secondary", tabs) === "pane-tab-strip";
}
