import { useEffect, useLayoutEffect, useMemo, useRef, useState, type FormEvent } from "react";
import type { OctopusClientContextMenuItem } from "../../electron";
import type { ChatRun } from "../../chatRuns";
import {
  clearClientTaskAlias,
  clientTaskTitle,
  removeClientTaskPresentation,
  renameClientTask,
  restoreClientTaskAliasIfAbsent,
  useClientTaskOrganization,
} from "../../shell/clientWorkspaces";
import {
  createNativeTaskMenuSession,
  nativeTaskMenuLabel,
  type ClientTaskMenuSnapshot,
  type ClientTaskActionsMenuRequest,
  subscribeOpenClientTaskActionsMenu,
  subscribeRenameClientTask,
  subscribeRestoreClientTask,
} from "../../shell/taskActionsMenu";
import { useKeyboardShortcuts } from "../../keyboardShortcuts";
import { TaskActionsPopover, type TaskAction } from "./TaskActionsPopover";
import type { OctopusOsProjectWorkspace } from "../../shell/useOctopusOsProjects";
import { copyText } from "../../runtimeFormat";
import { appToast } from "../../toasts";
import {
  OcButton,
  OcDialog,
  OcDialogFooter,
  OcDialogHeader,
  OcTextField,
} from "../../design-system/components";

type TaskTarget = { runId: string; scope: string; identity: string };
type TaskMenuState = TaskTarget & ClientTaskActionsMenuRequest & { signature: string };

function taskIdentity(run: ChatRun): string {
  return JSON.stringify([run.runId, run.chatInterfaceId, run.eeId, run.osRootEeId, run.createdAtMs, run.writable, run.archived]);
}

function nativeTaskItems(actions: TaskAction[]): OctopusClientContextMenuItem[] {
  return actions.map((action) => action.type === "separator" ? action : {
    id: action.id, label: nativeTaskMenuLabel(action.description ? `${action.label} (${action.description})` : action.label), enabled: action.enabled !== false,
    ...(action.type ? { type: action.type } : {}),
    ...(action.checked !== undefined ? { checked: action.checked } : {}),
    ...(action.submenu ? { submenu: nativeTaskItems(action.submenu) } : {}),
  });
}

function selectTaskAction(actions: TaskAction[], id: string, enabled = true): boolean {
  for (const action of actions) {
    if (action.type === "separator") continue;
    const available = enabled && action.enabled !== false;
    if (action.submenu && selectTaskAction(action.submenu, id, available)) return true;
    if (action.id !== id) continue;
    if (!available || !action.onSelect || action.submenu) throw new Error("That task action is no longer available.");
    action.onSelect();
    return true;
  }
  return false;
}

export function TaskActionsController({
  runtimeHome,
  accessProfile,
  activeChatRunId,
  chatOrderIds,
  chatRuns,
  homeRunId,
  pinnedChatIds,
  projectWorkspace,
  onChatOrderIdsChange,
  onOpenChatInNewWindow,
  onPinnedChatIdsChange,
  onSelectChat,
}: {
  runtimeHome: string;
  accessProfile: "live" | "lab";
  activeChatRunId: string;
  chatOrderIds: string[];
  chatRuns: ChatRun[];
  homeRunId: string;
  pinnedChatIds: string[];
  projectWorkspace: OctopusOsProjectWorkspace;
  onChatOrderIdsChange: (runIds: string[]) => void;
  onOpenChatInNewWindow: (runId: string) => void;
  onPinnedChatIdsChange: (runIds: string[]) => void;
  onSelectChat: (runId: string) => void;
}) {
  const organization = useClientTaskOrganization(accessProfile);
  const shortcuts = useKeyboardShortcuts(accessProfile);
  const [menu, setMenu] = useState<TaskMenuState | null>(null);
  const [renamingTask, setRenamingTask] = useState<TaskTarget | null>(null);
  const [deletingTask, setDeletingTask] = useState<TaskTarget | null>(null);
  const chatById = useMemo(() => new Map(chatRuns.filter((chat) => chat.runId !== homeRunId).map((chat) => [chat.runId, chat])), [chatRuns, homeRunId]);
  const scope = JSON.stringify([runtimeHome, accessProfile, homeRunId, projectWorkspace.rootEeId, projectWorkspace.homeInterfaceId]);
  const target = (run: ChatRun): TaskTarget => ({ runId: run.runId, scope, identity: taskIdentity(run) });
  const currentRun = (value: TaskTarget | null) => {
    const run = value ? chatById.get(value.runId) : null;
    return runtimeHome && value?.scope === scope && run && taskIdentity(run) === value.identity ? run : null;
  };
  const renameRun = currentRun(renamingTask), deleteRun = currentRun(deletingTask);
  const error = (failure: unknown) => appToast.danger(failure instanceof Error ? failure.message : String(failure));
  const close = () => setMenu(null);
  const nativeSession = useRef<ReturnType<typeof createNativeTaskMenuSession> | null>(null);

  function taskActions(run: ChatRun): TaskAction[] {
    const runId = run.runId, archived = run.archived, pinned = pinnedChatIds.includes(runId);
    const alias = organization.aliases[runId];
    const setArchived = (value: boolean) => {
      if (projectWorkspace.pending) return;
      close();
      const generation = latest.current.generation;
      void projectWorkspace.setTaskArchived(runId, value).catch((failure) => {
        if (latest.current.scope === scope && latest.current.generation === generation) error(failure);
      });
    };
    const actions: TaskAction[] = archived ? [
      { id: "restore", label: "Restore task", icon: "retry", enabled: accessProfile === "live" && !projectWorkspace.pending, onSelect: () => setArchived(false) },
    ] : [
      { id: "rename", label: "Rename", icon: "edit", onSelect: () => { close(); setRenamingTask(target(run)); } },
      { id: "pin", label: pinned ? "Unpin" : "Pin", icon: pinned ? "pinOff" : "pin", shortcuts: shortcuts.toggle_pin_task, onSelect: () => { onPinnedChatIdsChange(pinned ? pinnedChatIds.filter((id) => id !== runId) : [runId, ...pinnedChatIds]); close(); } },
      ...(alias ? [{ id: "projected-title", label: "Use projected title", icon: "refresh" as const, onSelect: () => {
        clearClientTaskAlias(accessProfile, runId); close();
        const generation = latest.current.generation;
        appToast.info("Using projected task title", 8, { dedupeKey: `task-alias-clear:${accessProfile}:${runId}`, action: { label: "Undo", onSelect: () => {
          if (latest.current.scope === scope && latest.current.generation === generation) restoreClientTaskAliasIfAbsent(accessProfile, runId, alias);
        } } });
      } }] : []),
      ...(run.writable ? [] : [{ id: "readonly", label: "Read-only internal run", icon: "lock" as const, enabled: false }]),
    ];
    const copy = (text: string, label: string) => {
      void copyText(text).then((copied) => copied ? appToast.success(`${label} copied`) : appToast.danger("Clipboard access is unavailable."), error);
      close();
    };
    actions.push({ type: "separator" }, { id: "copy-menu", label: "Copy", icon: "copy", submenu: [
      { id: "copy-title", label: "Task title", icon: "edit", onSelect: () => copy(clientTaskTitle(runId, run.title, organization), "Task title") },
      { id: "copy", label: "Run ID", icon: "copy", shortcuts: shortcuts.copy_run_id, onSelect: () => copy(runId, "Run ID") },
    ] });
    if (!archived) actions.push({ type: "separator" }, { id: "open", label: "Open in new window", icon: "openExternal", onSelect: () => { close(); onOpenChatInNewWindow(runId); } });
    if (!archived && run.writable) actions.push({ type: "separator" }, { id: "archive", label: "Archive", description: run.turnOpen ? "Stop the task before archiving" : undefined, icon: "archive", enabled: accessProfile === "live" && !projectWorkspace.pending && !run.turnOpen, onSelect: () => setArchived(true) });
    if (accessProfile === "live" && !archived) actions.push({ id: "delete", label: "Delete task…", icon: "trash", tone: "danger", enabled: !projectWorkspace.pending, onSelect: () => { close(); setDeletingTask(target(run)); } });
    return actions;
  }

  const readMenu = (runId: string): ClientTaskMenuSnapshot | null => {
    const run = chatById.get(runId);
    if (!runtimeHome || !run || renameRun || deleteRun) return null;
    const actions = taskActions(run), items = nativeTaskItems(actions);
    return {
      items,
      signature: JSON.stringify([scope, taskIdentity(run), clientTaskTitle(runId, run.title, organization), items]),
      select(id) { if (!selectTaskAction(actions, id)) throw new Error("Unknown native task action."); },
    };
  };
  const targetFor = (runId: string) => {
    const run = chatById.get(runId);
    return runtimeHome && run ? target(run) : null;
  };
  const latest = useRef({ scope, generation: 0, readMenu, currentRun, targetFor });
  useLayoutEffect(() => {
    latest.current = { scope, generation: latest.current.generation + Number(latest.current.scope !== scope), readMenu, currentRun, targetFor };
    nativeSession.current?.reconcile();
    if (menu && (!currentRun(menu) || readMenu(menu.runId)?.signature !== menu.signature)) setMenu(null);
    if (renamingTask && !renameRun) setRenamingTask(null);
    if (deletingTask && !deleteRun) setDeletingTask(null);
  });
  useEffect(() => {
    const session = createNativeTaskMenuSession((runId) => latest.current.readMenu(runId), error);
    nativeSession.current = session;
    const unsubscribeMenu = subscribeOpenClientTaskActionsMenu((request) => {
      const current = latest.current.readMenu(request.runId);
      if (!current) return;
      if (request.pointer && window.octopusTransport) { setMenu(null); session.open(request, window.octopusTransport); return; }
      session.cancel();
      const captured = latest.current.targetFor(request.runId);
      if (!captured) return;
      setMenu((previous) => previous?.anchorElement && previous.anchorElement === request.anchorElement && previous.runId === request.runId
        ? null : { ...captured, ...request, signature: current.signature });
    });
    const unsubscribeRename = subscribeRenameClientTask((runId) => {
      const captured = latest.current.targetFor(runId);
      session.cancel(); setMenu(null);
      if (captured) setRenamingTask(captured);
    });
    const unsubscribeRestore = subscribeRestoreClientTask((runId) => {
      const snapshot = latest.current.readMenu(runId);
      if (!snapshot) return;
      try { snapshot.select("restore"); } catch (failure) { error(failure); }
    });
    return () => { unsubscribeMenu(); unsubscribeRename(); unsubscribeRestore(); session.dispose(); nativeSession.current = null; };
  }, []);

  const deleteTask = async () => {
    if (!deleteRun || projectWorkspace.pending || accessProfile !== "live") return;
    const runId = deleteRun.runId, generation = latest.current.generation;
    try {
      await projectWorkspace.deleteTask(runId);
      if (latest.current.scope !== scope || latest.current.generation !== generation) return;
      removeClientTaskPresentation(accessProfile, runId);
      onPinnedChatIdsChange(pinnedChatIds.filter((id) => id !== runId));
      onChatOrderIdsChange(chatOrderIds.filter((id) => id !== runId));
      if (activeChatRunId === runId && homeRunId) onSelectChat(homeRunId);
      setDeletingTask(null);
    } catch (failure) { if (latest.current.scope === scope && latest.current.generation === generation) error(failure); }
  };
  const menuRun = currentRun(menu);
  return <>
    {menuRun && menu ? <TaskActionsPopover key={`${menu.runId}:${menu.signature}`} anchor={menu} actions={taskActions(menuRun)}
      title={menuRun ? clientTaskTitle(menuRun.runId, menuRun.title, organization) : ""} onClose={close}
      onSelect={(id) => {
        const snapshot = latest.current.currentRun(menu) && latest.current.readMenu(menu.runId);
        if (!snapshot || snapshot.signature !== menu.signature) { close(); return; }
        try { snapshot.select(id); } catch (failure) { error(failure); }
      }} /> : null}
    <TaskNameDialog initialName={renameRun ? clientTaskTitle(renameRun.runId, renameRun.title, organization) : ""}
      open={Boolean(renameRun)} busy={projectWorkspace.pending} onCancel={() => setRenamingTask(null)}
      onSubmit={(name) => { if (renameRun && !projectWorkspace.pending) { renameClientTask(accessProfile, renameRun.runId, name); setRenamingTask(null); } }} />
    <TaskDeleteDialog busy={projectWorkspace.pending} open={Boolean(deleteRun)} run={deleteRun}
      title={deleteRun ? clientTaskTitle(deleteRun.runId, deleteRun.title, organization) : ""} onCancel={() => setDeletingTask(null)} onDelete={deleteTask} />
  </>;
}

function TaskNameDialog({
  busy,
  initialName,
  open,
  onCancel,
  onSubmit,
}: {
  busy: boolean;
  initialName: string;
  open: boolean;
  onCancel: () => void;
  onSubmit: (name: string) => void;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [name, setName] = useState("");
  useEffect(() => {
    if (!open) return;
    setName(initialName);
    const frame = window.requestAnimationFrame(() => inputRef.current?.select());
    return () => window.cancelAnimationFrame(frame);
  }, [initialName, open]);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!busy && name.trim()) onSubmit(name.trim());
  };
  return (
    <OcDialog ariaLabelledBy="octopus-task-name-title" initialFocusRef={inputRef} onClose={onCancel} open={open} size="compact">
      <form className="flex flex-col gap-4 p-5" onSubmit={submit}>
        <OcDialogHeader
          className="p-0"
          description="The run remains unchanged. This name belongs to the local Octopus OS projection."
          onClose={onCancel}
          title="Rename task"
          titleId="octopus-task-name-title"
        />
        <OcTextField label="Task name" maxLength={180} onChange={(event) => setName(event.currentTarget.value)} inputRef={inputRef} value={name} />
        <OcDialogFooter className="min-h-0 border-0 p-0">
          <OcButton onClick={onCancel} variant="ghost">Cancel</OcButton>
          <OcButton disabled={busy || !name.trim()} type="submit" variant="primary">Save</OcButton>
        </OcDialogFooter>
      </form>
    </OcDialog>
  );
}

function TaskDeleteDialog({
  busy,
  open,
  run,
  title,
  onCancel,
  onDelete,
}: {
  busy: boolean;
  open: boolean;
  run: ChatRun | null;
  title: string;
  onCancel: () => void;
  onDelete: () => void;
}) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  if (!open || !run) return null;
  return (
    <OcDialog ariaLabelledBy="octopus-task-delete-title" initialFocusRef={cancelRef} onClose={onCancel} open={open} panelClassName="p-5" size="compact">
      <OcDialogHeader className="p-0" title="Delete task?" titleId="octopus-task-delete-title" />
      <p className="mt-2 text-base text-token-description-foreground">
        {`Delete “${title}”? This cannot be undone. Archive it instead if you want to keep the conversation for later.`}
      </p>
      <OcDialogFooter className="mt-5 min-h-0 border-0 p-0">
        <OcButton buttonRef={cancelRef} onClick={onCancel} variant="ghost">Cancel</OcButton>
        <OcButton disabled={busy} onClick={onDelete} tone="danger" variant="primary">Delete task</OcButton>
      </OcDialogFooter>
    </OcDialog>
  );
}
