import type { FilesOpenTarget } from "../components/files/types";
import { normalizeFilesPaneSnapshot, type FilesPaneSnapshot } from "../components/files/filesPaneSnapshot";
import type { TaskSourcesPaneTarget } from "../components/chat/taskSourcesPane";
import type { WorkerEnvironmentPaneTarget } from "../components/worker-environment/workerEnvironmentPane";
import type { ClientWorkspaceKey } from "./clientWorkspaces";

export type WorkspaceSurfacePaneTarget = {
  kind: "surface";
  runId: string;
  surfaceId: string;
  workspaceKey: ClientWorkspaceKey;
};

export type WorkspaceFilesPaneTarget = {
  kind: "files";
  openTarget?: FilesOpenTarget;
  snapshot?: FilesPaneSnapshot;
  workspaceKey: ClientWorkspaceKey;
};

export type WorkspaceTerminalPaneTarget = {
  kind: "terminal";
  cwd?: string;
  terminalId: string;
  title: string;
  workspaceKey: ClientWorkspaceKey;
};

export type WorkspaceTaskSourcesPaneTarget = {
  kind: "task_sources";
  target: TaskSourcesPaneTarget;
  workspaceKey: ClientWorkspaceKey;
};

export type WorkspaceLauncherPaneTarget = {
  kind: "launcher";
  runId: string;
  source?: Pick<WorkspaceTerminalPaneTarget, "kind" | "cwd">
    | Pick<WorkspaceWorkerEnvironmentPaneTarget, "kind" | "target">;
  workspaceKey: ClientWorkspaceKey;
};

export type WorkspaceWorkerEnvironmentPaneTarget = {
  kind: "worker_environment";
  target: WorkerEnvironmentPaneTarget;
  workspaceKey: ClientWorkspaceKey;
};

export type TransferableWorkspacePaneTarget =
  | WorkspaceFilesPaneTarget
  | WorkspaceSurfacePaneTarget
  | WorkspaceWorkerEnvironmentPaneTarget;

export type WorkspacePaneTarget =
  | TransferableWorkspacePaneTarget
  | WorkspaceLauncherPaneTarget
  | WorkspaceTaskSourcesPaneTarget
  | WorkspaceTerminalPaneTarget;

export type WorkspacePaneTabState = {
  id: string;
  target: WorkspacePaneTarget;
};

export type ShellSurfaceSelection = {
  runId: string;
  surfaceId: string;
  workspaceKey: ClientWorkspaceKey;
};

export type ClientPaneRole = "primary" | "secondary";

export type ActiveClientSurfaceIdentity = ShellSurfaceSelection & {
  pane: ClientPaneRole;
  ready: boolean;
};

export type ActiveClientSurfaceIdentityInput = {
  activePane: ClientPaneRole;
  primary: {
    target: ShellSurfaceSelection;
    workspaceActiveRunId: string;
  };
  secondary: {
    fallbackWorkspaceKey: ClientWorkspaceKey;
    open: boolean;
    target: WorkspacePaneTarget | null;
    workspaceActiveRunId: string;
  };
};

export type ClientWindowBootstrap = {
  schema: "octopus.client-window";
  schemaVersion: 1;
  initialTarget?: TransferableWorkspacePaneTarget;
  reattachWindowId?: string;
  primary: boolean;
  scope: string;
  windowId: string;
};

export type ClientWindowOpenRequest = {
  initialTarget?: TransferableWorkspacePaneTarget;
};

export type ClientWindowOpenOutcome = {
  opened: true;
  windowId: string;
};

export type ClientWindowReadyRequest = {
  windowId: string;
};

export type ClientPaneDragPayload = {
  schema: "octopus.client-pane-transfer";
  schemaVersion: 1;
  sourceWindowId: string;
  transferId: string;
};

export type ClientPaneTransferBeginRequest = {
  sourcePane: "primary" | "secondary";
  sourceTabId: string;
  target: TransferableWorkspacePaneTarget;
};

export type ClientPaneTransferRequest = {
  transferId: string;
};

export type ClientPaneRect = { x: number; y: number; width: number; height: number };
export type ClientPaneDragBeginRequest = ClientPaneTransferRequest & {
  captureRect: ClientPaneRect;
  rowRect: ClientPaneRect;
  offset: { x: number; y: number };
  appearance?: { radius: number; background?: string };
};
export type ClientPaneDropAnchorRequest = ClientPaneTransferRequest & {
  rect: ClientPaneRect;
};
export type ClientPaneDragState = ClientPaneTransferRequest & {
  phase: "preparing" | "prepared" | "floating" | "transferring" | "closed";
};
export type ClientPaneDragOutcome = { status: "cancelled" } | { status: "transferred"; windowId: string; transferId: string };

export type ClientPaneTransferAccepted = ClientPaneDragPayload & {
  sourcePane: "primary" | "secondary";
  sourceTabId: string;
  target: TransferableWorkspacePaneTarget;
};

export type ClientPaneTransferCompleted = ClientPaneDragPayload & {
  sourcePane: "primary" | "secondary";
  sourceTabId: string;
  target: TransferableWorkspacePaneTarget;
};

export type ClientPaneTransferAcknowledgementRequest = ClientPaneTransferRequest & {
  released: boolean;
};

export const CLIENT_PANE_DRAG_MIME = "application/x-octopus-client-pane";

export const MAIN_CLIENT_WINDOW_BOOTSTRAP: ClientWindowBootstrap = {
  schema: "octopus.client-window",
  schemaVersion: 1,
  primary: true,
  scope: "main",
  windowId: "main",
};

export function workspaceSurfacePaneTab(
  workspaceKey: ClientWorkspaceKey,
  surfaceId: string,
  runId: string,
): WorkspacePaneTabState {
  return {
    id: surfaceId === "chat"
      ? `chat:${workspaceKey}:${runId || "new"}`
      : `surface:${workspaceKey}:${surfaceId}`,
    target: {
      kind: "surface",
      runId: surfaceId === "chat" ? runId : "",
      surfaceId,
      workspaceKey,
    },
  };
}

export function workspacePaneTabForTarget(target: TransferableWorkspacePaneTarget): WorkspacePaneTabState {
  if (target.kind === "surface") {
    return workspaceSurfacePaneTab(target.workspaceKey, target.surfaceId, target.runId);
  }
  if (target.kind === "files") {
    return { id: target.snapshot ? `files:${target.snapshot.id}` : "operator:files", target };
  }
  const surfaceKey = target.target.filePath
    ? `file:${target.target.surfaceId ?? ""}:${target.target.filePath}`
    : target.target.clientSurface
      ? `document:${target.target.surfaceId ?? ""}:${target.target.clientSurface.sourceDigest}`
      : target.target.surfaceId ?? target.target.panelKind ?? "primary";
  return {
    id: `we:${target.target.workerEnvironmentId}:${surfaceKey}`,
    target,
  };
}

export function workspaceTaskSourcesPaneTab(
  workspaceKey: ClientWorkspaceKey,
  target: TaskSourcesPaneTarget,
): WorkspacePaneTabState {
  return {
    id: `task-sources:${target.runId}`,
    target: { kind: "task_sources", target, workspaceKey },
  };
}

export function clientSurfaceRunIdentityReady(
  target: ShellSurfaceSelection,
  workspaceActiveRunId: string,
): boolean {
  return target.surfaceId !== "chat" || target.runId === workspaceActiveRunId;
}

function paneSurfaceIdentity(
  pane: ClientPaneRole,
  target: ShellSurfaceSelection,
  workspaceActiveRunId: string,
): ActiveClientSurfaceIdentity {
  const ready = clientSurfaceRunIdentityReady(target, workspaceActiveRunId);
  return {
    ...target,
    pane,
    ready,
    runId: ready && target.surfaceId === "chat" ? target.runId : "",
  };
}

export function deriveActiveClientSurfaceIdentity(
  input: ActiveClientSurfaceIdentityInput,
): ActiveClientSurfaceIdentity {
  const { primary, secondary } = input;
  if (input.activePane !== "secondary" || !secondary.open) {
    return paneSurfaceIdentity("primary", primary.target, primary.workspaceActiveRunId);
  }
  if (!secondary.target) {
    return {
      pane: "secondary",
      ready: true,
      runId: "",
      surfaceId: "",
      workspaceKey: secondary.fallbackWorkspaceKey,
    };
  }
  if (secondary.target.kind !== "surface") {
    return {
      pane: "secondary",
      ready: true,
      runId: secondary.target.kind === "launcher" ? secondary.target.runId : "",
      surfaceId: "",
      workspaceKey: secondary.target.workspaceKey,
    };
  }
  return paneSurfaceIdentity("secondary", {
    runId: secondary.target.surfaceId === "chat" ? secondary.target.runId : "",
    surfaceId: secondary.target.surfaceId,
    workspaceKey: secondary.target.workspaceKey,
  }, secondary.workspaceActiveRunId);
}

export function screenPointOutsideClientWindow(
  point: { x: number; y: number } | null,
  bounds: { height: number; width: number; x: number; y: number },
): boolean {
  if (
    !point
    || !Number.isFinite(point.x)
    || !Number.isFinite(point.y)
    || !Number.isFinite(bounds.x)
    || !Number.isFinite(bounds.y)
    || !Number.isFinite(bounds.width)
    || !Number.isFinite(bounds.height)
    || bounds.width <= 0
    || bounds.height <= 0
  ) return false;
  return point.x < bounds.x
    || point.y < bounds.y
    || point.x >= bounds.x + bounds.width
    || point.y >= bounds.y + bounds.height;
}

export function transferableWorkspacePaneTarget(
  target: WorkspacePaneTarget,
): TransferableWorkspacePaneTarget | null {
  return target.kind === "terminal" || target.kind === "task_sources" || target.kind === "launcher" ? null : target;
}

export function clientWindowChatStorageScope(windowScope: string, pane: "home" | "main" | "secondary"): string {
  if (windowScope === "main") return pane;
  const windowKey = windowScope.replace(/[^A-Za-z0-9_-]/g, "").slice(-20) || "peer";
  return `w_${windowKey}_${pane}`;
}

export function clientWindowStateStorage(windowScope: string): Storage {
  return windowScope === "main" ? window.localStorage : window.sessionStorage;
}

export function clientWindowStateKey(windowScope: string, key: string): string {
  return windowScope === "main" ? key : `octopus.client-window.${windowScope}.${key}`;
}

export function parseClientPaneDragPayload(value: string): ClientPaneDragPayload | null {
  try {
    const parsed: unknown = JSON.parse(value);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    const row = parsed as Record<string, unknown>;
    if (row.schema !== "octopus.client-pane-transfer" || row.schemaVersion !== 1) return null;
    const sourceWindowId = boundedClientWindowText(row.sourceWindowId, 128);
    const transferId = boundedClientWindowText(row.transferId, 128);
    return sourceWindowId && transferId ? {
      schema: "octopus.client-pane-transfer",
      schemaVersion: 1,
      sourceWindowId,
      transferId,
    } : null;
  } catch {
    return null;
  }
}

export function normalizeTransferableWorkspacePaneTarget(value: unknown): TransferableWorkspacePaneTarget | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  const workspaceKey = row.workspaceKey === "dashboard" || row.workspaceKey === "octopus-os"
    ? row.workspaceKey
    : null;
  if (!workspaceKey) return null;
  if (row.kind === "surface") {
    const surfaceId = boundedClientWindowText(row.surfaceId, 512);
    const runId = boundedClientWindowText(row.runId, 512);
    return surfaceId ? {
      kind: "surface",
      runId: surfaceId === "chat" ? runId : "",
      surfaceId,
      workspaceKey,
    } : null;
  }
  if (row.kind === "files") {
    const openTarget = normalizeClientWindowFilesTarget(row.openTarget);
    if (row.openTarget !== undefined && !openTarget) return null;
    const snapshot = row.snapshot === undefined ? undefined : normalizeFilesPaneSnapshot(row.snapshot);
    if (row.snapshot !== undefined && !snapshot) return null;
    return { kind: "files", workspaceKey, ...(openTarget ? { openTarget } : {}), ...(snapshot ? { snapshot } : {}) };
  }
  if (row.kind === "worker_environment") {
    const target = normalizeClientWindowWorkerEnvironmentTarget(row.target);
    return target ? { kind: "worker_environment", target, workspaceKey } : null;
  }
  return null;
}

function normalizeClientWindowFilesTarget(value: unknown): FilesOpenTarget | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  if (row.kind === "file") {
    const absolutePath = boundedClientWindowText(row.absolutePath, 4096);
    const path = boundedOptionalClientWindowText(row.path, 4096);
    const rootId = boundedClientWindowText(row.rootId, 512);
    if (!absolutePath && path == null && !rootId) return null;
    if ((path != null) !== Boolean(rootId)) return null;
    return {
      kind: "file",
      ...(absolutePath ? { absolutePath } : {}),
      ...(path != null ? { path, rootId } : {}),
    };
  }
  if (row.kind === "artifact") {
    const artifactId = boundedClientWindowText(row.artifactId, 1024);
    const interfaceId = boundedClientWindowText(row.interfaceId, 512);
    const label = boundedClientWindowText(row.label, 1024);
    const runId = boundedClientWindowText(row.runId, 512);
    const createdAtMs = typeof row.createdAtMs === "number" && Number.isFinite(row.createdAtMs) && row.createdAtMs >= 0
      ? row.createdAtMs
      : undefined;
    if (!artifactId || !interfaceId || !label || !runId || (row.createdAtMs !== undefined && createdAtMs === undefined)) return null;
    return {
      kind: "artifact",
      artifactId,
      interfaceId,
      label,
      runId,
      ...(createdAtMs !== undefined ? { createdAtMs } : {}),
    };
  }
  return null;
}

function normalizeClientWindowWorkerEnvironmentTarget(value: unknown): WorkerEnvironmentPaneTarget | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  const workerEnvironmentId = boundedClientWindowText(row.workerEnvironmentId, 512);
  if (!workerEnvironmentId || row.schemaVersion !== 1) return null;
  const surfaceId = boundedClientWindowText(row.surfaceId, 512);
  const filePath = boundedClientWindowText(row.filePath, 2048);
  const panelKindValue = boundedClientWindowText(row.panelKind, 64);
  const panelKind = [
    "browser",
    "connection",
    "container",
    "runtime",
    "worktree",
    "files",
    "file",
    "terminal",
    "changes",
    "desktop",
    "custom",
    "overview",
  ].includes(panelKindValue) ? panelKindValue as NonNullable<WorkerEnvironmentPaneTarget["panelKind"]> : undefined;
  const clientSurface = normalizeClientWindowSurface(row.clientSurface);
  if (row.clientSurface !== undefined && !clientSurface) return null;
  if (row.panelKind !== undefined && !panelKind) return null;
  return {
    schemaVersion: 1,
    workerEnvironmentId,
    ...(surfaceId ? { surfaceId } : {}),
    ...(filePath ? { filePath } : {}),
    ...(panelKind ? { panelKind } : {}),
    ...(clientSurface ? { clientSurface } : {}),
  };
}

function normalizeClientWindowSurface(value: unknown): NonNullable<WorkerEnvironmentPaneTarget["clientSurface"]> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  const documentId = boundedClientWindowText(row.documentId, 512);
  const sourceDigest = boundedClientWindowText(row.sourceDigest, 128);
  const revision = Number(row.revision);
  return documentId && sourceDigest && Number.isSafeInteger(revision) && revision > 0
    ? { documentId, revision, sourceDigest }
    : null;
}

function boundedOptionalClientWindowText(value: unknown, maximum: number): string | null {
  return typeof value === "string" && value.length <= maximum && !/[\u0000-\u001f\u007f]/.test(value)
    ? value
    : null;
}

function boundedClientWindowText(value: unknown, maximum: number): string {
  const text = typeof value === "string" ? value.trim() : "";
  return text && text.length <= maximum && !/[\u0000-\u001f\u007f]/.test(text) ? text : "";
}
