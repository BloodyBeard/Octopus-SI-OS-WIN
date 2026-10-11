import { cubicCirclePath, orthogonalBranchPath } from "../icon-glyphs/geometric-primitives";
import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode, type SVGProps } from "react";
import { createPortal } from "react-dom";
import type { ClientAgentDefinition } from "../../agentPresets";
import type { ChatApplicationChoice, ChatExecutionContext, ChatWorkerEnvironmentRoom } from "../../chatRuns";
import { AgentPresetLogo } from "../agent/AgentPresetLogo";
import { Glyph } from "../IconGlyphs";
import type { TaskAccessMode } from "./taskAccess";
import { ShellTooltip } from "../shell/ShellTooltip";

const CHIP_BUTTON_CLASS = "border-token-border no-drag cursor-interaction flex items-center gap-1 border whitespace-nowrap select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40 rounded-full text-token-text-tertiary enabled:hover:bg-token-foreground/5 data-[state=open]:bg-token-list-hover-background border-transparent h-token-button-composer-sm px-1.5 py-0 text-sm leading-[18px] composer-utility-label min-w-0 items-center";
const CHIP_FOREGROUND_CLASS = "text-token-text-primary [&_svg]:size-4 [&_svg]:text-current";
const CHIP_ICON_CLASS = `flex size-4 shrink-0 items-center justify-center self-center leading-none [&_svg]:block ${CHIP_FOREGROUND_CLASS}`;
const CHIP_TEXT_CLASS = "composer-utility-dropdown-label-text inline-flex min-w-0 -translate-y-px items-center gap-1 text-left";
const CHIP_CATEGORY_CLASS = "composer-utility-dropdown-label-category hidden shrink-0 font-normal text-token-text-primary";
const CHIP_VALUE_CLASS = `composer-utility-dropdown-label-value min-w-0 truncate font-normal whitespace-nowrap ${CHIP_FOREGROUND_CLASS}`;
const CHIP_VALUE_CONTENT_CLASS = "composer-utility-dropdown-label-value-content block max-w-full min-w-0 truncate";
const CHIP_CHEVRON_CLASS = `composer-utility-dropdown-label-chevron icon-xs shrink-0 ${CHIP_FOREGROUND_CLASS}`;
const CHIP_COLLAPSIBLE_CHEVRON_CLASS = "composer-utility-secondary-chevron composer-utility-dropdown-label-chevron inline-flex size-4 items-center justify-center self-center";
const CLEAR_GROUP_CLASS = "group/project-selector relative inline-flex shrink-0 rounded-full";
const CLEAR_BUTTON_CLASS = "composer-utility-project-clear-button no-drag pointer-events-none absolute inset-y-0 left-0 z-10 flex aspect-square cursor-interaction items-center justify-center rounded-full text-token-text-tertiary opacity-0 focus:outline-none focus-visible:ring-1 focus-visible:ring-token-focus-border hover:text-token-foreground";
const CLEAR_BUTTON_AVAILABLE_CLASS = "group-hover/project-selector:pointer-events-auto group-hover/project-selector:opacity-100 focus-visible:pointer-events-auto focus-visible:opacity-100";
const MENU_CONTENT_MAX_HEIGHT = "min(350px, calc(100vh - 16px))";
const MENU_CONTENT_CLASS = "no-drag bg-token-dropdown-background/90 text-token-foreground ring-token-border z-50 m-px flex select-none flex-col overflow-y-auto rounded-xl ring-[0.5px] px-1 py-1 shadow-xl-spread backdrop-blur-sm";
const MENU_ITEM_BASE = "outline-hidden rounded-lg px-[var(--padding-row-x)] py-[var(--padding-row-y)] text-sm";
const MENU_ITEM_INTERACTIVE = "group hover:bg-token-list-hover-background focus:bg-token-list-hover-background cursor-interaction";
const MENU_ITEM_CONTENT = "flex w-full items-center gap-1.5";
const MENU_ITEM_CONTENT_SUBTEXT = "flex w-full items-center gap-3";
const MENU_ITEM_ICON = "shrink-0 opacity-75 group-focus:opacity-100 group-hover:opacity-100";
const MENU_BODY_CLASS = "flex w-72 flex-col gap-1.5 overflow-hidden";
const MENU_LIST_CLASS = "vertical-scroll-fade-mask flex h-[200px] flex-col gap-1.5 overflow-y-auto";
const MENU_SECTION_LABEL_CLASS = "px-[var(--padding-row-x)] py-1 text-sm text-token-description-foreground";
const MENU_SECTION_CLASS = "flex flex-col";
const MENU_SEARCH_ROW_CLASS = "flex min-w-0 items-center gap-1 px-[var(--padding-row-x)] py-[var(--padding-row-y)]";
const MENU_SEARCH_INPUT_CLASS = "!w-auto flex-1 appearance-none !rounded-none !border-none bg-transparent !px-0 !py-0 text-sm text-token-foreground placeholder:text-token-input-placeholder-foreground focus:outline-none";
type Props = {
  contexts: ChatExecutionContext[];
  agentPresets: readonly ClientAgentDefinition[];
  agentPresetId: string;
  accessMode: TaskAccessMode;
  fullAccessUnavailableReason: string;
  applicationIds: string[];
  applicationLocked: boolean;
  applicationLockReason: "" | "turn" | "saving" | "read-only";
  driverNames: string[];
  eeId: string;
  locked: boolean;
  rooms: ChatWorkerEnvironmentRoom[];
  runId: string;
  workerInterfaceId: string;
  onAgentChange: (agentPresetId: string) => void;
  onAccessModeChange: (accessMode: TaskAccessMode) => void;
  onEeChange: (eeId: string) => void;
  onApplicationToggle: (applicationId: string) => void;
  onDriverToggle: (driverName: string) => void;
};

export function ComposerUtilityBar({
  contexts,
  agentPresets,
  agentPresetId,
  accessMode,
  fullAccessUnavailableReason,
  applicationIds,
  applicationLocked,
  applicationLockReason,
  driverNames,
  eeId,
  locked,
  rooms,
  runId,
  workerInterfaceId,
  onAgentChange,
  onAccessModeChange,
  onEeChange,
  onApplicationToggle,
  onDriverToggle,
}: Props) {
  const selectedContext = contexts.find((context) => context.eeId === eeId) ?? null;
  const rootContext = contexts.find((context) => context.isOsRoot) ?? null;
  const projectContexts = contexts.filter((context) => !context.isOsRoot);
  const workers = selectedContext?.workers ?? [];
  const selectedWorker = workers.find((worker) => worker.id === workerInterfaceId) ?? null;
  const allAgentOptions = agentPresets.map((agent) => {
    const context = contexts.find((candidate) => candidate.eeId === agent.projectEeId) ?? null;
    const projectLabel = context?.displayName || context?.goal || context?.templateName || agent.projectEeId;
    return {
      agent,
      defaultAccessMode: context?.defaultTaskAccessMode ?? "project_access",
      projectColor: context?.presentationColor,
      searchValue: `${agent.name} ${projectLabel}`,
      subText: projectLabel,
    };
  });
  const selectedAgentOption = allAgentOptions.find((option) => option.agent.id === agentPresetId) ?? null;
  const selectedAgent = selectedAgentOption?.agent ?? null;
  const taskConfiguration = Boolean(runId && workerInterfaceId && !selectedAgent);
  const agentOptions = selectedAgentOption
    ? allAgentOptions
    : eeId
      ? allAgentOptions.filter((option) => option.agent.projectEeId === eeId)
      : allAgentOptions;
  const workerApplications = selectedWorker?.applications ?? [];
  const workerDrivers = selectedWorker?.drivers ?? [];
  const attachedRooms = uniqueRooms(rooms.filter((room) => room.runId === runId && roomIsAvailable(room)));
  const applications = workerApplications.filter((application) => selectedAgent
    ? selectedAgent.applicationIds.includes(application.id)
    : runId ? applicationIds.includes(application.id) : false);
  const drivers = workerDrivers.filter((driver) => selectedAgent
    ? selectedAgent.driverNames.includes(driver.name)
    : runId ? attachedRooms.some((room) => room.kind === driver.name) : false);
  const effectiveDriverNames = runId
    ? attachedRooms
      .map((room) => room.kind)
      .filter((name) => drivers.some((driver) => driver.name === name))
    : driverNames;
  const projectSelected = Boolean(selectedContext && !selectedContext.isOsRoot);
  const selectedProjectName = selectedContext
    ? selectedContext.displayName || selectedContext.goal || selectedContext.templateName || selectedContext.eeId
    : "";

  return (
    <div
      className="composer-utility-bar composer-utility-home -mx-px -mb-4.5 flex flex-nowrap items-center gap-2 overflow-hidden rounded-t-2xl select-none"
      style={{ backgroundColor: "var(--color-token-composer-utility-background, var(--color-token-side-bar-background))" }}
    >
      <UtilityBarContent>
        <ProjectClearSlot
          available={projectSelected && !locked && !selectedAgent && rootContext != null}
          onClear={() => rootContext && onEeChange(rootContext.eeId)}
        >
          <UtilityBarDropdown
            collapse="xs"
            disabled={locked || Boolean(selectedAgent) || !contexts.length}
            icon={<ProjectIcon />}
            iconMarked={projectSelected && rootContext != null}
            indicator="none"
            navigationTarget="workspace-project"
            title={selectedAgent
              ? `Project fixed by ${selectedAgent.name}`
              : projectSelected ? "Change the project for this task" : "Select a project to run your task in"}
            value={selectedContext
              ? selectedContext.isOsRoot
                ? "No project"
                : selectedProjectName
              : "Choose project"}
            valueClassName="!max-w-60 text-token-foreground"
          >
            {(close, query) => (
              <div className={MENU_BODY_CLASS}>
                <MenuSearch placeholder="Search projects" />
                <div className={MENU_LIST_CLASS}>
                  <div className={MENU_SECTION_LABEL_CLASS}>Projects</div>
                  <div className={MENU_SECTION_CLASS}>
                    {rootContext && matches("No project", query) ? (
                      <UtilityBarItem
                        icon={<ProjectIcon />}
                        label="No project"
                        selected={rootContext.eeId === eeId}
                        onSelect={() => {
                          onEeChange(rootContext.eeId);
                          close();
                        }}
                      />
                    ) : null}
                    {projectContexts
                      .filter((context) => matches(`${context.displayName} ${context.goal} ${context.templateName} ${context.eeId}`, query))
                      .map((context) => (
                        <UtilityBarItem
                          icon={<ProjectIcon />}
                          key={context.eeId}
                          label={context.displayName || context.goal || context.templateName || context.eeId}
                          selected={context.eeId === eeId}
                          subText={context.displayName && context.goal !== context.displayName ? context.goal : undefined}
                          onSelect={() => {
                            onEeChange(context.eeId);
                            close();
                          }}
                        />
                      ))}
                    {!contexts.length ? <UtilityBarDisabledItem label="No projects found" /> : null}
                  </div>
                </div>
              </div>
            )}
          </UtilityBarDropdown>
        </ProjectClearSlot>

        <div className={`${CHIP_BUTTON_CLASS} composer-agent-access-control !gap-1 !py-0 !pr-1.5 !pl-0`}>
          <UtilityBarDropdown
            collapse="none"
            disabled={locked || !agentOptions.length}
            icon={selectedAgent ? (
              <AgentPresetLogo color={selectedAgentOption?.projectColor} icon={selectedAgent.icon} size={16} />
            ) : <AgentIcon />}
            indicator="none"
            navigationTarget="workspace-agent"
            title={selectedAgent
              ? `Agent: ${selectedAgent.name}`
              : taskConfiguration
                ? "This task uses its own configuration"
              : locked
                ? "This task's Agent identity is unavailable"
                : "Select an agent for this task"}
            triggerVariant="embedded"
            value={selectedAgent?.name || (taskConfiguration ? "Task configuration" : locked ? "Agent unavailable" : "Choose agent")}
            valueClassName="max-w-40 text-sm"
          >
            {(close, query) => (
              <div className={MENU_BODY_CLASS}>
                <MenuSearch placeholder="Search agents" />
                <div className={MENU_LIST_CLASS}>
                  {agentOptions.some((option) => matches(option.searchValue, query)) ? (
                    <div className={MENU_SECTION_CLASS}>
                      <div className={MENU_SECTION_LABEL_CLASS}>Agents</div>
                      {agentOptions
                        .filter((option) => matches(option.searchValue, query))
                        .map(({ agent, defaultAccessMode, projectColor, subText }) => (
                          <UtilityBarItem
                            icon={<AgentPresetLogo color={projectColor} icon={agent.icon} size={16} />}
                            key={agent.id}
                            label={(
                              <span className="flex min-w-0 items-center gap-1">
                                <span className="min-w-0 truncate">{agent.name}</span>
                                <TaskAccessIcon
                                  accessMode={agent.id === agentPresetId ? accessMode : defaultAccessMode}
                                  className="icon-xs shrink-0"
                                />
                              </span>
                            )}
                            selected={agent.id === agentPresetId}
                            subText={subText}
                            title={agent.name}
                            onSelect={() => {
                              onAgentChange(agent.id);
                              close();
                            }}
                          />
                        ))}
                    </div>
                  ) : null}
                  {!agentOptions.some((option) => matches(option.searchValue, query))
                    ? <UtilityBarDisabledItem label="No agents found" />
                    : null}
                </div>
              </div>
            )}
          </UtilityBarDropdown>

          <button
            aria-label={accessMode === "full_access" ? "Use project access" : "Use full access"}
            aria-pressed={accessMode === "full_access"}
            className="composer-agent-access-toggle inline-flex size-4 shrink-0 cursor-interaction items-center justify-center bg-transparent focus:outline-none focus-visible:ring-1 focus-visible:ring-token-focus-border disabled:cursor-not-allowed disabled:opacity-40"
            data-composer-navigation-target="permissions"
            disabled={locked || !selectedWorker || accessMode !== "full_access" && Boolean(fullAccessUnavailableReason)}
            onClick={() => onAccessModeChange(accessMode === "full_access" ? "project_access" : "full_access")}
            aria-description={accessMode === "full_access" ? undefined : fullAccessUnavailableReason || undefined}
            type="button"
          >
            <TaskAccessIcon accessMode={accessMode} className="icon-xs shrink-0" />
          </button>
        </div>

        <UtilityBarDropdown
          collapse="sm"
          disabled={applicationLocked || !applications.length}
          icon={<Glyph name="storefront" size={16} />}
          indicator="collapsible-chevron"
          navigationTarget="workspace-applications"
          title={applicationLocked
            ? applicationLockReason === "turn"
              ? "Applications can be changed after this turn settles"
              : applicationLockReason === "saving"
                ? "Application changes are being saved"
                : "This task is read-only"
            : applications.length
              ? runId
                ? "Choose applications for this task"
                : "Choose which applications provided by this Agent this task can use"
              : "No applications are available for this task"}
          value={selectionLabel(
            applicationIds,
            applications.map((application) => ({
              id: application.id,
              label: application.name,
              selectedByDefault: application.selectedByDefault,
            })),
            "applications",
            false,
          )}
          valueClassName="max-w-40 text-sm"
        >
          {(_close, query) => (
            <div className={MENU_BODY_CLASS}>
              <MenuSearch placeholder="Search applications" />
              <div className={MENU_LIST_CLASS}>
                {(["connector", "plugin", "artifact"] as const).map((category) => {
                  const rows = applications.filter((application) => (
                    application.category === category
                    && matches(`${application.name} ${application.id} ${application.description}`, query)
                  ));
                  if (!rows.length) return null;
                  return (
                    <div className={MENU_SECTION_CLASS} key={category}>
                      <div className={MENU_SECTION_LABEL_CLASS}>{applicationCategoryLabel(category)}</div>
                      {rows.map((application) => (
                        <UtilityBarItem
                          icon={<ApplicationChoiceLogo application={application} />}
                          key={application.id}
                          label={application.name}
                          selected={applicationIds.includes(application.id)}
                          subText={capabilityChoiceDescription(
                            !runId && application.selectedByDefault,
                            application.description || application.id,
                          )}
                          onSelect={() => onApplicationToggle(application.id)}
                        />
                      ))}
                    </div>
                  );
                })}
                {!applications.some((application) => matches(`${application.name} ${application.id} ${application.description}`, query))
                  ? <UtilityBarDisabledItem label="No applications found" />
                  : null}
              </div>
            </div>
          )}
        </UtilityBarDropdown>

        <UtilityBarDropdown
          collapse="sm"
          disabled={locked || !drivers.length}
          icon={<Glyph name="plugins" size={16} />}
          indicator="collapsible-chevron"
          navigationTarget="workspace-drivers"
          title={locked
            ? "Worker Environments are fixed for this active task"
            : drivers.length
              ? runId
                ? "Worker Environments are fixed for this active task"
                : "Choose which Worker Environments provided by this Agent this task can use"
              : "No Worker Environments are available for this task"}
          value={selectionLabel(
            effectiveDriverNames,
            drivers.map((driver) => ({
              id: driver.name,
              label: displayDriverName(driver.name),
              selectedByDefault: driver.selectedByDefault,
            })),
            "Worker Environments",
            false,
          )}
          valueClassName="max-w-40 text-sm"
        >
          {(_close, query) => (
            <div className={MENU_BODY_CLASS}>
              <MenuSearch placeholder="Search Worker Environments" />
              <div className={MENU_LIST_CLASS}>
                <div className={MENU_SECTION_LABEL_CLASS}>Worker Environments</div>
                <div className={MENU_SECTION_CLASS}>
                  {drivers.filter((driver) => matches(`${driver.name} ${driver.capabilities.join(" ")}`, query)).map((driver) => (
                    <UtilityBarItem
                      icon={<Glyph name="plugins" size={16} />}
                      key={driver.name}
                      label={displayDriverName(driver.name)}
                      selected={effectiveDriverNames.includes(driver.name)}
                      subText={capabilityChoiceDescription(
                        !runId && driver.selectedByDefault,
                        driver.capabilities.join(" · ") || `${driver.toolIds.length} tools`,
                      )}
                      onSelect={() => onDriverToggle(driver.name)}
                    />
                  ))}
                  {!drivers.some((driver) => matches(`${driver.name} ${driver.capabilities.join(" ")}`, query))
                    ? <UtilityBarDisabledItem label="No Worker Environments found" />
                    : null}
                </div>
              </div>
            </div>
          )}
        </UtilityBarDropdown>

      </UtilityBarContent>
    </div>
  );
}

function UtilityBarContent({ children }: { children: ReactNode }) {
  return (
    <div
      aria-label="Composer utility bar"
      className="flex min-w-0 flex-1 items-center gap-1 overflow-hidden"
      role="group"
    >
      {children}
    </div>
  );
}

function ProjectClearSlot({
  available,
  children,
  onClear,
}: {
  available: boolean;
  children: ReactNode;
  onClear: () => void;
}) {
  const [suppressed, setSuppressed] = useState(false);
  const active = available && !suppressed;
  return (
    <div
      className={`${CLEAR_GROUP_CLASS}${active ? " composer-utility-project-clear-available hover:bg-token-list-hover-background" : ""}`}
      onPointerDownCapture={(event) => {
        if (event.target instanceof Element && event.target.closest("[data-clear-project-button]") != null) return;
        setSuppressed(true);
      }}
      onPointerEnter={() => setSuppressed(false)}
      onPointerLeave={() => setSuppressed(false)}
    >
      <ShellTooltip className="pointer-events-none absolute inset-y-0 left-0 z-10 aspect-square" disabled={!active} label="Don't work in a project">
        <button
          aria-label="Don't work in a project"
          className={`${CLEAR_BUTTON_CLASS}${active ? ` ${CLEAR_BUTTON_AVAILABLE_CLASS}` : ""}`}
          data-clear-project-button=""
          disabled={!active}
          onClick={onClear}
          type="button"
        >
          <ClearProjectIcon aria-hidden className="size-4" />
        </button>
      </ShellTooltip>
      {children}
    </div>
  );
}

function UtilityBarDropdown({
  categoryLabel,
  children,
  collapse = "none",
  disabled,
  icon,
  iconMarked = false,
  indicator = "chevron",
  navigationTarget,
  title,
  triggerVariant = "default",
  value,
  valueClassName = "",
}: {
  children: (close: () => void, query: string) => ReactNode;
  categoryLabel?: string;
  collapse?: "none" | "sm" | "xs";
  disabled: boolean;
  icon: ReactNode;
  iconMarked?: boolean;
  indicator?: "none" | "chevron" | "collapsible-chevron";
  navigationTarget: string;
  title: string;
  triggerVariant?: "default" | "embedded";
  value: string;
  valueClassName?: string;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const [anchor, setAnchor] = useState<{ bottom: number; left: number } | null>(null);

  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);

  useLayoutEffect(() => {
    if (!open) {
      setAnchor(null);
      return;
    }
    const update = () => {
      const rect = triggerRef.current?.getBoundingClientRect();
      if (!rect) return;
      setAnchor({
        bottom: window.innerHeight - rect.top + 4,
        left: Math.max(8, Math.min(rect.left, window.innerWidth - 296)),
      });
    };
    update();
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    return () => {
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const close = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!triggerRef.current?.contains(target) && !menuRef.current?.contains(target)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("pointerdown", close);
    window.addEventListener("keydown", escape);
    return () => {
      window.removeEventListener("pointerdown", close);
      window.removeEventListener("keydown", escape);
    };
  }, [open]);

  return (
    <>
      <ShellTooltip className="min-w-0" label={title} disabled={open}>
        <button
          aria-label={title}
          aria-disabled={disabled || undefined}
          aria-expanded={open}
          aria-haspopup="menu"
          className={triggerVariant === "embedded"
            ? "composer-agent-selector-trigger composer-utility-label no-drag flex min-w-0 cursor-interaction items-center gap-1 border-0 bg-transparent px-1.5 !pr-0 py-0 text-sm leading-[18px] text-token-text-tertiary select-none focus:outline-none disabled:cursor-not-allowed disabled:opacity-40"
            : CHIP_BUTTON_CLASS}
          data-composer-navigation-target={navigationTarget}
          data-state={open ? "open" : "closed"}
          disabled={disabled}
          onClick={(event) => {
            if (event.detail !== 0) return;
            setQuery("");
            setOpen((current) => !current);
          }}
          onPointerDown={(event) => {
            if (event.button !== 0) return;
            event.preventDefault();
            setQuery("");
            setOpen((current) => !current);
          }}
          ref={triggerRef}
          type="button"
        >
          <ChipLabel
            categoryLabel={categoryLabel}
            collapse={collapse}
            icon={icon}
            iconMarked={iconMarked}
            indicator={indicator}
            value={value}
            valueClassName={valueClassName}
          />
        </button>
      </ShellTooltip>
      {open && anchor ? createPortal(
        <div
          className={`composer-utility-menu fixed ${MENU_CONTENT_CLASS}`}
          ref={menuRef}
          role="menu"
          style={{ bottom: anchor.bottom, left: anchor.left, maxHeight: MENU_CONTENT_MAX_HEIGHT } as CSSProperties}
        >
          <MenuQueryContext.Provider value={{ query, setQuery }}>
            {children(() => setOpen(false), query)}
          </MenuQueryContext.Provider>
        </div>,
        document.body,
      ) : null}
    </>
  );
}

function ChipLabel({
  categoryLabel,
  collapse,
  icon,
  iconMarked = false,
  indicator,
  value,
  valueClassName,
}: {
  categoryLabel?: string;
  collapse: "none" | "sm" | "xs";
  icon: ReactNode;
  iconMarked?: boolean;
  indicator: "none" | "chevron" | "collapsible-chevron";
  value: string;
  valueClassName: string;
}) {
  const valueRef = useRef<HTMLSpanElement>(null);
  const valueContentRef = useRef<HTMLSpanElement>(null);
  const [valueOverflowing, setValueOverflowing] = useState(false);

  useLayoutEffect(() => {
    const box = valueRef.current;
    const content = valueContentRef.current;
    if (!box || !content) return;
    setValueOverflowing(content.scrollWidth > box.clientWidth);
  }, [value]);

  return (
    <>
      <span className="inline-flex shrink-0" data-project-selector-icon={iconMarked ? true : undefined}>
        <span className={CHIP_ICON_CLASS}>{icon}</span>
      </span>
      <span className={`${CHIP_TEXT_CLASS}${categoryLabel ? " composer-utility-dropdown-label-text-with-category" : ""}`}>
        {categoryLabel ? <span className={CHIP_CATEGORY_CLASS}>{categoryLabel}</span> : null}
        <span className={`${CHIP_VALUE_CLASS} ${collapseClass(collapse)} ${valueClassName}`} ref={valueRef}>
          <span
            className={`${CHIP_VALUE_CONTENT_CLASS}${valueOverflowing ? " composer-utility-dropdown-label-value-overflowing" : ""}`}
            data-tooltip-overflow-target
            ref={valueContentRef}
          >
            {value}
          </span>
        </span>
      </span>
      {indicator === "chevron" ? <Glyph className={CHIP_CHEVRON_CLASS} name="chevronDown" size={16} /> : null}
      {indicator === "collapsible-chevron" ? (
        <span className={CHIP_COLLAPSIBLE_CHEVRON_CLASS}>
          <Glyph className={`icon-xs shrink-0 ${CHIP_FOREGROUND_CLASS}`} name="chevronDown" size={16} />
        </span>
      ) : null}
    </>
  );
}

const MenuQueryContext = createContext<{ query: string; setQuery: (value: string) => void }>({
  query: "",
  setQuery: () => undefined,
});

function MenuSearch({ placeholder }: { placeholder: string }) {
  const { query, setQuery } = useContext(MenuQueryContext);
  return (
    <div className="my-1">
      <div className={MENU_SEARCH_ROW_CLASS}>
        <input
          autoFocus={false}
          className={MENU_SEARCH_INPUT_CLASS}
          onChange={(event) => setQuery(event.currentTarget.value)}
          placeholder={placeholder}
          value={query}
        />
      </div>
    </div>
  );
}

function matches(value: string, query: string): boolean {
  const needle = query.trim().toLocaleLowerCase();
  return !needle || value.toLocaleLowerCase().includes(needle);
}

function selectionLabel(
  selectedIds: readonly string[],
  choices: readonly { id: string; label: string; selectedByDefault: boolean }[],
  pluralLabel: string,
  showProjectDefaults: boolean,
): string {
  const selected = choices.filter((choice) => selectedIds.includes(choice.id));
  const unavailableCount = selectedIds.length - selected.length;
  if (!selected.length && unavailableCount > 0) {
    return unavailableCount === 1 ? "Unavailable selection" : `${unavailableCount} unavailable selections`;
  }
  if (!choices.length) return `No ${pluralLabel} available`;
  if (!selected.length) return `No ${pluralLabel}`;
  const projectDefaults = choices.filter((choice) => choice.selectedByDefault);
  const selectedIsProjectDefault = showProjectDefaults
    && projectDefaults.length > 0
    && projectDefaults.length === selected.length
    && projectDefaults.every((choice) => selectedIds.includes(choice.id));
  if (selected.length === 1) return selectedIsProjectDefault
    ? `${selected[0].label} · default`
    : selected[0].label;
  if (selectedIsProjectDefault) return `${selected.length} default ${pluralLabel}`;
  return `${selected.length} ${pluralLabel}`;
}

function capabilityChoiceDescription(selectedByDefault: boolean, description: string): string {
  return selectedByDefault ? `Project default · ${description}` : description;
}

function applicationCategoryLabel(category: "connector" | "plugin" | "artifact"): string {
  if (category === "connector") return "Connectors";
  if (category === "plugin") return "Plugins";
  return "Artifacts";
}

function collapseClass(collapse: "none" | "sm" | "xs"): string {
  return collapse === "sm" ? "composer-utility-label-sm" : collapse === "xs" ? "composer-utility-label-xs" : "";
}

function UtilityBarItem({
  icon,
  label,
  selected,
  subText,
  title,
  onSelect,
}: {
  icon: ReactNode;
  label: ReactNode;
  selected: boolean;
  subText?: string;
  title?: string;
  onSelect: () => void;
}) {
  const tooltip = title ?? (typeof label === "string" ? label : "");
  return (
    <ShellTooltip className="w-full min-w-0" disabled={!tooltip} label={tooltip}>
      <button className={`${MENU_ITEM_BASE} ${MENU_ITEM_INTERACTIVE} w-full [&>span]:gap-1.5`} onClick={onSelect} role="menuitem" type="button">
        <span className={subText ? MENU_ITEM_CONTENT_SUBTEXT : MENU_ITEM_CONTENT}>
          <span className={`inline-flex size-4 items-center justify-center leading-none [&_svg]:size-4 ${MENU_ITEM_ICON}`}>{icon}</span>
          {subText ? (
            <span className="flex min-w-0 flex-1 flex-col">
              <span className="min-w-0 truncate text-left">{label}</span>
              <span className="min-w-0 truncate text-left text-token-text-secondary">{subText}</span>
            </span>
          ) : (
            <span className="flex-1 min-w-0 truncate text-left">{label}</span>
          )}
          {selected ? <span className="inline-flex items-center justify-center leading-none"><Glyph className={`icon-2xs ${MENU_ITEM_ICON}`} name="check" size={12} /></span> : null}
        </span>
      </button>
    </ShellTooltip>
  );
}

function TaskAccessIcon({ accessMode, className }: { accessMode: TaskAccessMode; className?: string }) {
  const resolvedClassName = `${className ?? ""}${accessMode === "full_access" ? " text-token-warning" : " text-token-text-tertiary"}`;
  return accessMode === "full_access"
    ? <FullAccessIcon aria-hidden className={resolvedClassName} />
    : <ProjectAccessIcon aria-hidden className={resolvedClassName} />;
}

function UtilityBarDisabledItem({ label }: { label: string }) {
  return (
    <div className={`${MENU_ITEM_BASE} cursor-default opacity-50`}>
      <span className={MENU_ITEM_CONTENT}>
        <span className="flex-1 min-w-0 truncate text-left">{label}</span>
      </span>
    </div>
  );
}

function ApplicationChoiceLogo({ application }: { application: ChatApplicationChoice }) {
  const fallback = application.category === "plugin"
    ? "plugins"
    : application.category === "artifact"
      ? "artifact"
      : "interfaces";
  if (!application.logoDataUrl) return <Glyph name={fallback} size={16} />;
  return (
    <span className="flex size-4 shrink-0 items-center justify-center overflow-hidden rounded-[4px]">
      <img
        alt=""
        className={`${application.logoDarkDataUrl ? "electron-dark:hidden " : ""}size-full object-contain`}
        draggable={false}
        src={application.logoDataUrl}
      />
      {application.logoDarkDataUrl ? (
        <img alt="" className="hidden size-full object-contain electron-dark:block" draggable={false} src={application.logoDarkDataUrl} />
      ) : null}
    </span>
  );
}

function FullAccessIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg width={20} height={20} viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg" {...props}>
      <path
        clipRule="evenodd"
        d="M9.06543 1.95123C9.66107 1.69076 10.3389 1.69071 10.9346 1.95123L15.9346 4.13873C16.7832 4.51008 17.3311 5.34917 17.3311 6.27545V10.5528C17.3309 14.6017 14.0489 17.8847 10 17.8848C5.95108 17.8846 2.66813 14.6017 2.66797 10.5528V6.27545C2.66797 5.34924 3.21695 4.51012 4.06543 4.13873L9.06543 1.95123ZM10.4014 3.16998C10.1456 3.05814 9.85444 3.05819 9.59863 3.16998L4.59863 5.35748C4.23427 5.51708 3.99805 5.87764 3.99805 6.27545V10.5528C3.99821 13.8671 6.68563 16.5546 10 16.5547C13.3144 16.5546 16.0008 13.8671 16.001 10.5528V6.27545C16.001 5.87756 15.7658 5.51703 15.4014 5.35748L10.4014 3.16998Z"
        fill="currentColor"
        fillRule="evenodd"
      />
      <path d="M10.8883 13.1116C10.8883 13.6025 10.4903 14.0005 9.99936 14.0005C9.50844 14.0005 9.11047 13.6025 9.11047 13.1116C9.11047 12.6207 9.50844 12.2227 9.99936 12.2227C10.4903 12.2227 10.8883 12.6207 10.8883 13.1116Z" fill="currentColor" />
      <path d="M10.5169 10.8949L11.1135 7.31519C11.2283 6.62672 10.6974 6 9.99941 6C9.30145 6 8.77053 6.62672 8.88528 7.31519L9.4819 10.8949C9.52406 11.1479 9.74294 11.3333 9.99941 11.3333C10.2559 11.3333 10.4748 11.1479 10.5169 10.8949Z" fill="currentColor" />
    </svg>
  );
}

function ProjectAccessIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg width={20} height={20} viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg" {...props}>
      <path
        d="M12.6683 4.16699C12.6683 3.84391 12.4065 3.58203 12.0834 3.58203C11.7603 3.58203 11.4984 3.84391 11.4984 4.16699V7.91699L11.4847 8.05078C11.4227 8.35375 11.1547 8.58203 10.8334 8.58203C10.4662 8.58203 10.1685 8.28411 10.1683 7.91699V3.75C10.1683 3.42691 9.90646 3.16504 9.58337 3.16504C9.26029 3.16504 8.99841 3.42691 8.99841 3.75V7.91699C8.99824 8.28411 8.70053 8.58203 8.33337 8.58203C7.96621 8.58203 7.66851 8.28411 7.66833 7.91699V5C7.66833 4.67691 7.40646 4.41504 7.08337 4.41504C6.76029 4.41504 6.49841 4.67691 6.49841 5V9.30371C6.53326 9.3429 6.56715 9.38359 6.59998 9.42578L8.02478 11.2588C8.25005 11.5486 8.19821 11.9659 7.90857 12.1914C7.6187 12.4169 7.20048 12.365 6.97498 12.0752L5.55017 10.2432C5.15812 9.7391 4.41813 9.73637 4.01501 10.1924C4.04396 10.426 4.11486 10.8323 4.25525 11.3486C4.44664 12.0525 4.75404 12.9113 5.21619 13.7383C6.14103 15.3931 7.62465 16.835 10.0004 16.835C12.8545 16.8348 15.1682 14.5211 15.1683 11.667V6.25C15.1683 5.92691 14.9065 5.66504 14.5834 5.66504C14.2603 5.66504 13.9984 5.92691 13.9984 6.25V9.16699C13.9982 9.53411 13.7005 9.83203 13.3334 9.83203C12.9662 9.83203 12.6685 9.53411 12.6683 9.16699V4.16699ZM13.9984 4.42578C14.1828 4.36671 14.3794 4.33496 14.5834 4.33496C15.641 4.33496 16.4984 5.19237 16.4984 6.25V11.667C16.4982 15.2557 13.589 18.1649 10.0004 18.165C6.95953 18.165 5.10939 16.2734 4.05505 14.3867C3.52774 13.4431 3.1843 12.4787 2.97205 11.6982C2.76447 10.9349 2.66834 10.2954 2.66833 10C2.66833 9.87959 2.70117 9.76148 2.76306 9.6582C3.28988 8.78018 4.26555 8.40372 5.16833 8.56152V5C5.16833 3.94237 6.02575 3.08496 7.08337 3.08496C7.31706 3.08496 7.54039 3.12845 7.74744 3.20508C7.98218 2.41297 8.7151 1.83496 9.58337 1.83496C10.1836 1.83496 10.7186 2.11176 11.0697 2.54395C11.3639 2.35978 11.7107 2.25195 12.0834 2.25195C13.141 2.25195 13.9984 3.10937 13.9984 4.16699V4.42578Z"
        fill="currentColor"
      />
    </svg>
  );
}

function ProjectIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg width={16} height={16} viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg" {...props}>
      <path
        d="M5.36914 2.1416C5.92368 2.14164 6.3602 2.23705 6.73242 2.38965C7.09745 2.53934 7.38155 2.73818 7.61816 2.9043C8.07599 3.22573 8.42077 3.47464 9.16602 3.47461H11.9473C13.3336 3.47484 14.4453 4.61217 14.4453 6V7.06543C14.4453 7.07196 14.4435 7.07845 14.4434 7.08496V11.3311C14.4432 12.7187 13.3316 13.8562 11.9453 13.8564H4.05371C2.66747 13.8562 1.55583 12.7187 1.55566 11.3311V7.35059C1.55545 7.34451 1.55377 7.33815 1.55371 7.33203C1.55371 7.32563 1.55539 7.31884 1.55566 7.3125V4.66699C1.55566 3.27918 2.66737 2.14185 4.05371 2.1416H5.36914ZM2.60547 7.85645V11.3311C2.60563 12.1519 3.26037 12.8054 4.05371 12.8057H11.9453C12.7387 12.8054 13.3934 12.1519 13.3936 11.3311V7.85645H2.60547ZM4.05371 3.19238C3.26027 3.19264 2.60547 3.84598 2.60547 4.66699V6.80664H13.3955V6C13.3955 5.17898 12.7407 4.52562 11.9473 4.52539H9.16699C8.07975 4.52558 7.50694 4.10863 7.01562 3.76367C6.77766 3.5966 6.57849 3.46159 6.33398 3.36133C6.09656 3.264 5.79646 3.19242 5.36914 3.19238H4.05371Z"
        fill="currentColor"
      />
    </svg>
  );
}

function ClearProjectIcon(props: SVGProps<SVGSVGElement>) {
  const disk = cubicCirclePath(8, 8, 6.52539, 3.60366);
  return (
    <svg width={16} height={16} viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg" {...props}>
      <path
        clipRule="evenodd"
        d={disk + "M10.0918 5.9082C9.88683 5.70329 9.55463 5.7024 9.34961 5.90723L8 7.25684L6.65039 5.90723C6.44535 5.70239 6.11219 5.70227 5.90723 5.90723C5.70232 6.11219 5.70241 6.44537 5.90723 6.65039L7.25684 8L5.90723 9.34961C5.70241 9.55463 5.70232 9.88781 5.90723 10.0928C6.11219 10.2977 6.44535 10.2976 6.65039 10.0928L8 8.74316L9.34961 10.0928C9.55463 10.2976 9.88683 10.2967 10.0918 10.0918C10.2968 9.88683 10.2976 9.55465 10.0928 9.34961L8.74316 8L10.0928 6.65039C10.2976 6.44535 10.2968 6.11317 10.0918 5.9082Z"}
        fill="currentColor"
        fillRule="evenodd"
      />
    </svg>
  );
}

function AgentIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg width={20} height={20} viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg" {...props}>
      <circle cx={5.4165} cy={5} r={1.875} stroke="currentColor" strokeWidth={1.33} />
      <circle cx={5.4165} cy={15} r={1.875} stroke="currentColor" strokeWidth={1.33} />
      <circle cx={14.5833} cy={5} r={1.875} stroke="currentColor" strokeWidth={1.33} />
      <path d={`M${5.4165} ${6.66664}V${13.3333}`} stroke="currentColor" strokeWidth={1.33} strokeLinejoin="round" />
      <path
        d={orthogonalBranchPath(5.41658, 12.5, 7.08325, 11.6667, 10, 12.9166, 14.5833, 8.33333, 7.5, 0.92047, 0.9205, 0.9205, 0.92048)}
        stroke="currentColor"
        strokeWidth={1.33}
        strokeLinejoin="round"
      />
    </svg>
  );
}

function uniqueRooms(rooms: ChatWorkerEnvironmentRoom[]): ChatWorkerEnvironmentRoom[] {
  const seen = new Set<string>();
  return rooms.filter((room) => {
    if (seen.has(room.doorInterfaceId)) return false;
    seen.add(room.doorInterfaceId);
    return true;
  });
}

function roomIsAvailable(room: ChatWorkerEnvironmentRoom): boolean {
  return room.live;
}

function displayDriverName(value: string): string {
  return value.trim().replace(/^system_/, "").replace(/[_-]+/g, " ").replace(/\b\w/g, (letter) => letter.toLocaleUpperCase());
}
