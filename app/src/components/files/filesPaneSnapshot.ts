import type { OctopusOsFile } from "../../electron";
import type { FilesOpenTarget } from "./types";

export type FilesEditorViewState = { scrollTop: number; scrollLeft: number };

export type FilesPaneSnapshot = {
  version: 1;
  id: string;
  revision: number;
  target?: FilesOpenTarget;
  rootId: string;
  rootPath: string;
  file: OctopusOsFile | null;
  draft: string;
  view: FilesEditorViewState;
};

const textLimit = 2 * 1024 * 1024;

export function normalizeFilesPaneSnapshot(value: unknown): FilesPaneSnapshot | null {
  if (!record(value, ["version", "id", "revision", "target", "rootId", "rootPath", "file", "draft", "view"])) return null;
  if (value.version !== 1 || !token(value.id, 128) || !integer(value.revision) || !pathText(value.rootId, 512) || !pathText(value.rootPath, 4096)) return null;
  if (Boolean(value.rootId) !== Boolean(value.rootPath) || !text(value.draft)) return null;
  const view = value.view;
  if (!record(view, ["scrollTop", "scrollLeft"]) || !offset(view.scrollTop) || !offset(view.scrollLeft)) return null;
  const target = value.target === undefined ? undefined : normalizeTarget(value.target);
  if (value.target !== undefined && !target) return null;
  let file: OctopusOsFile | null = null;
  if (value.file !== null) {
    const row = value.file;
    if (!record(row, ["rootId", "path", "name", "size", "modifiedAt", "kind", "text", "truncated", "digest"])) return null;
    if (!token(row.rootId, 512) || row.rootId !== value.rootId || !token(row.path, 4096) || !token(row.name, 512) || !integer(row.size)) return null;
    if (!token(row.modifiedAt, 128) || !Number.isFinite(Date.parse(row.modifiedAt)) || (row.kind !== "text" && row.kind !== "binary") || !text(row.text) || typeof row.truncated !== "boolean" || typeof row.digest !== "string") return null;
    if (row.kind === "text" && !row.truncated ? !/^[a-f0-9]{64}$/.test(row.digest) : row.digest !== "") return null;
    if ((row.kind !== "text" || row.truncated) && value.draft !== row.text) return null;
    if (target?.kind === "artifact") return null;
    file = { rootId: row.rootId, path: row.path, name: row.name, size: row.size, modifiedAt: row.modifiedAt, kind: row.kind, text: row.text, truncated: row.truncated, digest: row.digest };
  } else if (value.draft !== "") return null;
  return {
    version: 1, id: value.id, revision: value.revision, ...(target ? { target } : {}),
    rootId: value.rootId, rootPath: value.rootPath, file, draft: value.draft,
    view: { scrollTop: view.scrollTop, scrollLeft: view.scrollLeft },
  };
}

export function filesSnapshotDirty(snapshot: FilesPaneSnapshot): boolean {
  return snapshot.file?.kind === "text" && !snapshot.file.truncated && snapshot.draft !== snapshot.file.text;
}

export function filesRootMatches(snapshot: FilesPaneSnapshot, roots: readonly { id: string; path: string }[]): boolean {
  return roots.some((root) => root.id === snapshot.rootId && root.path === snapshot.rootPath);
}

function normalizeTarget(value: unknown): FilesOpenTarget | null {
  if (!record(value, ["kind", "absolutePath", "path", "rootId", "artifactId", "createdAtMs", "interfaceId", "label", "runId"])) return null;
  if (value.kind === "file") {
    if (!record(value, ["kind", "absolutePath", "path", "rootId"])) return null;
    if (value.absolutePath !== undefined && !token(value.absolutePath, 4096)) return null;
    if ((value.path !== undefined) !== (value.rootId !== undefined)) return null;
    if (value.path !== undefined && (!pathText(value.path, 4096) || !token(value.rootId, 512))) return null;
    if (value.absolutePath === undefined && value.path === undefined) return null;
    return { kind: "file", ...(typeof value.absolutePath === "string" ? { absolutePath: value.absolutePath } : {}), ...(typeof value.path === "string" && typeof value.rootId === "string" ? { path: value.path, rootId: value.rootId } : {}) };
  }
  if (value.kind !== "artifact" || !record(value, ["kind", "artifactId", "createdAtMs", "interfaceId", "label", "runId"])) return null;
  if (!token(value.artifactId, 1024) || !token(value.interfaceId, 512) || !token(value.label, 1024) || !token(value.runId, 512) || value.createdAtMs !== undefined && !integer(value.createdAtMs)) return null;
  return { kind: "artifact", artifactId: value.artifactId, interfaceId: value.interfaceId, label: value.label, runId: value.runId, ...(typeof value.createdAtMs === "number" ? { createdAtMs: value.createdAtMs } : {}) };
}

function record(value: unknown, keys: readonly string[]): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value) && Object.keys(value).every((key) => keys.includes(key));
}
function integer(value: unknown): value is number { return Number.isSafeInteger(value) && (value as number) >= 0; }
function offset(value: unknown): value is number { return typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= Number.MAX_SAFE_INTEGER; }
function pathText(value: unknown, maximum: number): value is string { return typeof value === "string" && value.length <= maximum && !/[\u0000-\u001f\u007f]/.test(value); }
function token(value: unknown, maximum: number): value is string { return pathText(value, maximum) && value.trim().length > 0; }
function text(value: unknown): value is string { return typeof value === "string" && new TextEncoder().encode(value).byteLength <= textLimit; }
