import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { OcButton, OcIconButton, OcMenuItem, OcMenuSectionLabel, OcSearchField, OcSegmented } from "../design-system/components";
import { OcDropdown } from "../design-system/OcDropdown";
import type { OctopusClientAccessProfile } from "../electron";
import { formatKeyboardShortcut, useKeyboardShortcuts } from "../keyboardShortcuts";
import {
  enabledFilterCount,
  statusKeys,
  statusLabels,
  type DashboardFilters,
  type DashboardScope,
  type DashboardTopologyMode,
  type FilterStatus,
} from "../uiState";

type Props = {
  className?: string;
  accessProfile: OctopusClientAccessProfile;
  inspectorOpen?: boolean;
  title: string;
  query: string;
  onQueryChange: (value: string) => void;
  placeholder: string;
  toolbar?: ReactNode;
  mode?: "dashboard" | "wide";
  dashboardScope?: DashboardScope;
  onDashboardScopeChange?: (scope: DashboardScope) => void;
  dashboardTopologyMode?: DashboardTopologyMode;
  onDashboardTopologyModeChange?: (mode: DashboardTopologyMode) => void;
  dashboardFilters?: DashboardFilters;
  onDashboardFiltersChange?: (filters: DashboardFilters) => void;
  onFullscreen?: () => void;
  showFullscreen?: boolean;
  searchFocusTick?: number;
  titleInWindowChrome?: boolean;
};

export function TopBar({
  className = "",
  accessProfile,
  inspectorOpen = true,
  title,
  query,
  onQueryChange,
  placeholder,
  toolbar,
  mode = "dashboard",
  dashboardScope = "System",
  onDashboardScopeChange,
  dashboardTopologyMode = "drilldown",
  onDashboardTopologyModeChange,
  dashboardFilters,
  onDashboardFiltersChange,
  onFullscreen,
  showFullscreen = true,
  searchFocusTick = 0,
  titleInWindowChrome = false,
}: Props) {
  const dashboardMode = mode === "dashboard";
  const [openMenu, setOpenMenu] = useState<"filters" | null>(null);
  const filterAnchorRef = useRef<HTMLButtonElement>(null);
  const keyboardShortcuts = useKeyboardShortcuts(accessProfile);
  const searchShortcut = keyboardShortcuts.focus_search?.[0];
  const filterSummary = useMemo(() => {
    if (!dashboardFilters) return "All";
    const enabled = enabledFilterCount(dashboardFilters);
    return enabled === statusKeys.length ? "All" : `${enabled}/${statusKeys.length}`;
  }, [dashboardFilters]);

  function toggleStatus(key: FilterStatus) {
    if (!dashboardFilters || !onDashboardFiltersChange) return;
    onDashboardFiltersChange({
      statuses: { ...dashboardFilters.statuses, [key]: !dashboardFilters.statuses[key] },
    });
  }

  return (
    <header className={`topbar${dashboardMode ? "" : " is-wide"}${titleInWindowChrome ? " is-window-titled" : ""} ${className}`}>
      <div aria-hidden={titleInWindowChrome || undefined} className="topbar-title">{title}</div>
      <div className="topbar-row">
        <SurfaceSearchBox
          onQueryChange={onQueryChange}
          placeholder={placeholder}
          query={query}
          searchFocusTick={searchFocusTick}
          shortcut={searchShortcut ? formatKeyboardShortcut(searchShortcut) : undefined}
          title={title}
        />
        {toolbar}
        {dashboardMode ? (
          <>
            <div className="toolbar-menu topbar-filter-menu">
              <OcButton
                buttonRef={filterAnchorRef}
                aria-label={`Filters ${filterSummary}`}
                className="topbar-filter-button"
                leadingIcon="filter"
                meta={filterSummary}
                aria-expanded={openMenu === "filters"}
                selected={openMenu === "filters"}
                onClick={() => setOpenMenu((current) => current === "filters" ? null : "filters")}
              >
                Filters
              </OcButton>
              <OcDropdown aria-label="Status filters" anchorRef={filterAnchorRef} open={openMenu === "filters" && Boolean(dashboardFilters)} onOpenChange={(open) => setOpenMenu(open ? "filters" : null)} style={{ width: 196 }}>
                <OcMenuSectionLabel>Status</OcMenuSectionLabel>
                {dashboardFilters && statusKeys.map((key) => (
                  <OcMenuItem
                    aria-checked={dashboardFilters.statuses[key]}
                    key={key}
                    label={statusLabels[key]}
                    onClick={(event) => { event.preventDefault(); toggleStatus(key); }}
                    role="menuitemcheckbox"
                    selected={dashboardFilters.statuses[key]}
                  />
                ))}
              </OcDropdown>
            </div>
            <OcSegmented
              className="topology-mode-toggle"
              label="Topology view mode"
              value={dashboardTopologyMode}
              options={dashboardScope === "System"
                ? [
                  { value: "drilldown", label: "Drilldown" },
                  { value: "zones", label: "Zones" },
                ]
                : [
                  { value: "drilldown", label: "Instances" },
                  { value: "zones", label: "Families" },
                ]}
              onChange={(next) => onDashboardTopologyModeChange?.(next)}
            />
          </>
        ) : null}
      </div>
      {dashboardMode ? (
        <div className="topbar-actions">
          <OcSegmented
            className="topbar-scope-toggle"
            label="Scope"
            value={dashboardScope}
            options={[
              { value: "System", label: "System" },
              { value: "World", label: "World" },
            ]}
            onChange={(next) => onDashboardScopeChange?.(next)}
          />
          {showFullscreen ? (
            <OcIconButton aria-pressed={!inspectorOpen} className="topbar-fullscreen-button" glyph={inspectorOpen ? "expand" : "restore"} label={inspectorOpen ? "Hide inspector" : "Show inspector"} onClick={onFullscreen} variant="ghost" />
          ) : null}
        </div>
      ) : null}
    </header>
  );
}

type SurfaceSearchBoxProps = {
  shortcut?: string;
  onQueryChange: (value: string) => void;
  placeholder: string;
  query: string;
  searchFocusTick?: number;
  title: string;
};

function SurfaceSearchBox({
  shortcut,
  onQueryChange,
  placeholder,
  query,
  searchFocusTick = 0,
  title,
}: SurfaceSearchBoxProps) {
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (searchFocusTick > 0) inputRef.current?.focus();
  }, [searchFocusTick]);

  return (
    <OcSearchField
      className="surface-search-field"
      inputRef={inputRef}
      label={`Search ${title}`}
      onChange={onQueryChange}
      placeholder={placeholder}
      trailing={shortcut ? <span className="shrink-0 text-xs text-token-input-placeholder-foreground">{shortcut}</span> : undefined}
      value={query}
    />
  );
}
