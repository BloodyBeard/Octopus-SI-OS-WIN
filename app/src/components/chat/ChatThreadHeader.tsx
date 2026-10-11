import { Fragment, type CSSProperties, type ReactNode } from "react";
import type { ChatRun } from "../../chatRuns";
import { dispatchClientCommand } from "../../clientActions";
import { OcPopover } from "../../design-system/components";
import { dispatchOpenClientTaskActionsMenu } from "../../shell/taskActionsMenu";
import { clientTaskTitle, useClientTaskOrganization } from "../../shell/clientWorkspaces";
import { Glyph } from "../IconGlyphs";
import { useWorkspaceCompactTab } from "../shell/WorkspaceCompactTabContext";
import { ShellTooltip } from "../shell/ShellTooltip";

type Props = {
  actionsVisible?: boolean;
  accessProfile: "live" | "lab";
  activeRun: ChatRun | null;
  edgeScrolled: boolean;
  pinnedContextCount: number;
  pinnedContextOpen: boolean;
  summaryDisplayMode: "overlay" | "shift" | "gutter";
  summaryOpen: boolean;
  summaryPopoverContent: ReactNode;
  mode: "window" | "pane";
  onPinnedContextOpenChange: (open: boolean) => void;
  onSummaryOpenChange: (open: boolean) => void;
};

const TOOLBAR_BUTTON = "no-drag cursor-interaction flex aspect-square h-token-button-composer shrink-0 items-center justify-center gap-1 whitespace-nowrap rounded-lg border border-transparent px-2 py-0 !px-0 text-base leading-[18px] text-token-text-tertiary select-none focus:outline-none focus-visible:ring-2 focus-visible:ring-token-focus-border focus-visible:ring-offset-0 disabled:cursor-default disabled:opacity-40 enabled:hover:bg-token-list-hover-background";

export function ChatThreadHeader({
  actionsVisible = true,
  accessProfile,
  activeRun,
  edgeScrolled,
  pinnedContextCount,
  pinnedContextOpen,
  summaryDisplayMode,
  summaryOpen,
  summaryPopoverContent,
  mode,
  onPinnedContextOpenChange,
  onSummaryOpenChange,
}: Props) {
  const compactTab = useWorkspaceCompactTab();
  const taskOrganization = useClientTaskOrganization(accessProfile);
  const activeTitle = activeRun ? clientTaskTitle(activeRun.runId, activeRun.title, taskOrganization) : "";
  return (
    <header
      className={`app-header-tint pointer-events-none absolute ${mode === "pane" ? "chat-thread-header-pane" : "chat-thread-header-window"} inset-x-0 top-0 z-30 flex h-toolbar min-w-0 items-center`}
      data-app-shell-header-edge-scroll="true"
      style={{ "--spacing-token-safe-header-left": "var(--app-titlebar-safe-left, 98px)" } as CSSProperties}
    >
      <div
        aria-hidden="true"
        className={`pointer-events-none relative h-full shrink-0 [container-type:inline-size]${mode === "pane" ? " chat-thread-header-pane-leading" : ""}`}
        style={{ width: mode === "pane" ? undefined : "8px", minWidth: 0 }}
      />
      <div className="pointer-events-none relative ms-2 flex h-full min-w-0 flex-1 isolate items-center gap-1.5 overflow-hidden [contain:layout_paint] pe-1.5">
        <div className="pointer-events-none flex min-w-0 flex-1 [&_button]:pointer-events-auto [&_input]:pointer-events-auto">
          <div
            className="mr-3 line-clamp-1 flex max-w-full min-w-0 items-center gap-1 truncate electron:h-toolbar"
            style={{ viewTransitionName: "header-title" }}
          >
            {activeRun ? (
              <>
                <ShellTooltip className="min-w-0 max-w-full" label={activeTitle}>
                  <button
                    aria-label={`Open recent tasks. Current task: ${activeTitle}`}
                    className="thread-header-title-button min-w-0 flex-1 truncate !px-0 !py-0 text-left text-sm text-token-foreground hover:opacity-80 electron:font-medium"
                    data-thread-title
                    onPointerDown={mode === "pane" ? compactTab?.onPointerDown : undefined}
                    onClick={() => dispatchClientCommand("open_command_menu", "surface")}
                    onContextMenu={(event) => {
                      event.preventDefault();
                      dispatchOpenClientTaskActionsMenu(activeRun.runId, event.currentTarget, { x: event.clientX, y: event.clientY });
                    }}
                    type="button"
                  >
                    <span className="thread-header-title-text block truncate">{activeTitle}</span>
                  </button>
                </ShellTooltip>
              </>
            ) : null}
          </div>
        </div>
      </div>
      {actionsVisible ? <div
        className="chat-thread-header-actions pointer-events-none relative flex h-full shrink-0 items-center gap-1"
        style={{ paddingInlineEnd: mode === "pane" ? "calc(3 * var(--spacing-token-button-composer) + 12px)" : "8px" }}
      >
        <ChatThreadHeaderActions
          activeRun={activeRun}
          pinnedContextCount={pinnedContextCount}
          pinnedContextOpen={pinnedContextOpen}
          summaryDisplayMode={summaryDisplayMode}
          summaryOpen={summaryOpen}
          summaryPopoverContent={summaryPopoverContent}
          onPinnedContextOpenChange={onPinnedContextOpenChange}
          onSummaryOpenChange={onSummaryOpenChange}
        />
      </div> : null}
      <div
        aria-hidden="true"
        className="chat-thread-top-fade"
        data-app-shell-main-content-top-fade={edgeScrolled ? "visible" : undefined}
      />
    </header>
  );
}

export function ChatThreadHeaderActions({
  activeRun,
  pinnedContextCount,
  pinnedContextOpen,
  summaryDisplayMode,
  summaryOpen,
  summaryPopoverContent,
  onPinnedContextOpenChange,
  onSummaryOpenChange,
}: Pick<Props,
  | "activeRun"
  | "pinnedContextCount"
  | "pinnedContextOpen"
  | "summaryDisplayMode"
  | "summaryOpen"
  | "summaryPopoverContent"
  | "onPinnedContextOpenChange"
  | "onSummaryOpenChange"
>) {
  const summaryLabel = summaryDisplayMode === "overlay" ? "Toggle summary" : "Toggle pinned summary";
  const summaryTrigger = activeRun ? (
    <ShellTooltip disabled={summaryOpen} label={summaryLabel}>
      <button
        aria-label={summaryLabel}
        aria-pressed={summaryOpen}
        className={`${TOOLBAR_BUTTON}${summaryOpen ? " bg-token-foreground/5 text-token-foreground" : ""}`}
        data-task-summary-trigger="true"
        onClick={() => onSummaryOpenChange(!summaryOpen)}
        type="button"
      >
        <Glyph className="icon-xs" name="taskSummary" size={16} />
      </button>
    </ShellTooltip>
  ) : null;

  return (
    <Fragment>
      {activeRun?.clientSurfaceRole === "task" ? (
        <div className="chat-thread-header-action-slot no-drag pointer-events-auto flex shrink-0 items-center gap-px">
          <ShellTooltip label="Task actions">
            <button
              aria-haspopup="menu"
              aria-label="Task actions"
              className="app-titlebar-action no-drag"
              data-task-actions-run-id={activeRun.runId}
              onClick={(event) => dispatchOpenClientTaskActionsMenu(activeRun.runId, event.currentTarget)}
              onKeyDown={(event) => {
                if (event.key !== "ArrowDown") return;
                event.preventDefault();
                dispatchOpenClientTaskActionsMenu(activeRun.runId, event.currentTarget);
              }}
              type="button"
            >
              <Glyph className="icon-xs" name="more" size={16} />
            </button>
          </ShellTooltip>
        </div>
      ) : null}
      {pinnedContextCount > 0 ? (
        <div className="chat-thread-header-action-slot no-drag pointer-events-auto flex shrink-0 items-center gap-px">
          <ShellTooltip label={`${pinnedContextCount} pinned context ${pinnedContextCount === 1 ? "item" : "items"}`}>
            <button
              aria-label={pinnedContextOpen ? "Close pinned context" : "Open pinned context"}
              aria-pressed={pinnedContextOpen}
              className={`${TOOLBAR_BUTTON}${pinnedContextOpen ? " bg-token-foreground/5 text-token-foreground" : ""}`}
              onClick={() => onPinnedContextOpenChange(!pinnedContextOpen)}
              type="button"
            >
              <Glyph className="icon-xs" name="pin" size={16} />
            </button>
          </ShellTooltip>
        </div>
      ) : null}
      {summaryTrigger ? (
        <div className="chat-thread-header-action-slot no-drag pointer-events-auto flex shrink-0 items-center gap-px">
          {summaryDisplayMode === "overlay" ? (
            <OcPopover
              align="end"
              anchor={summaryTrigger}
              className="task-summary-popover"
              label="Task summary"
              onDismiss={() => onSummaryOpenChange(false)}
              open={summaryOpen}
              panelClassName="task-summary-popover-panel"
              placement="bottom"
            >
              <div data-task-summary-popover="true">{summaryPopoverContent}</div>
            </OcPopover>
          ) : summaryTrigger}
        </div>
      ) : null}
    </Fragment>
  );
}
