import type { OctopusOsFile, OctopusOsFileRoots, OctopusOsFileWriteOutcome, OctopusOsFileWriteRequest } from "../../electron";
import type { FilesOpenTarget } from "./types";
import { filesRootMatches, filesSnapshotDirty, normalizeFilesPaneSnapshot, type FilesPaneSnapshot } from "./filesPaneSnapshot";

export type FilesPaneState = {
  snapshot: FilesPaneSnapshot;
  saving: boolean;
  transferPending: boolean;
  nativeClosePending: boolean;
  closeRequested: boolean;
  error: string;
};
type SaveTransport = {
  osFileRoots(): Promise<OctopusOsFileRoots>;
  writeOsFile(request: OctopusOsFileWriteRequest): Promise<OctopusOsFileWriteOutcome>;
};
type Pane = { state: FilesPaneState; listeners: Set<() => void>; closePromise?: Promise<boolean>; resolveClose?: (allow: boolean) => void; flush?: () => void };
const panes = new Map<string, Pane>();
const listeners = new Set<() => void>();
let generation = 0;
let windowCloseLock: { requestId: string; profile?: string } | null = null;

export function filesPaneKey(profile: string, tabId: string): string { return JSON.stringify([profile, tabId]); }
export function ensureFilesPane(key: string, target?: FilesOpenTarget, initial?: FilesPaneSnapshot): FilesPaneState {
  const existing = panes.get(key);
  if (existing) {
    if (initial && existing.state.snapshot.id !== initial.id) throw new Error("Files pane already owns another document snapshot.");
    return existing.state;
  }
  const snapshot = initial ? normalizeFilesPaneSnapshot(initial) : {
    version: 1 as const, id: crypto.randomUUID(), revision: 0, ...(target ? { target } : {}),
    rootId: "", rootPath: "", file: null, draft: "", view: { scrollTop: 0, scrollLeft: 0 },
  };
  if (!snapshot) throw new Error("Files pane snapshot is invalid.");
  const state = { snapshot, saving: false, transferPending: false, nativeClosePending: Boolean(windowCloseLock && matchesProfile(key, windowCloseLock.profile)), closeRequested: false, error: "" };
  panes.set(key, { state, listeners: new Set() });
  return state;
}
export function readFilesPane(key: string): FilesPaneState { return requirePane(key).state; }
export function hasFilesPane(key: string): boolean { return panes.has(key); }
export function subscribeFilesPane(key: string, listener: () => void): () => void {
  const pane = requirePane(key); pane.listeners.add(listener); return () => { pane.listeners.delete(listener); };
}
export function subscribeFilesPaneDialogs(listener: () => void): () => void { listeners.add(listener); return () => { listeners.delete(listener); }; }
export function filesPaneGeneration(): number { return generation; }
export function pendingFilesPaneClose(): { key: string; state: FilesPaneState } | null {
  for (const [key, pane] of panes) if (pane.state.closeRequested) return { key, state: pane.state };
  return null;
}
export function updateFilesPane(key: string, update: (snapshot: FilesPaneSnapshot) => FilesPaneSnapshot): void {
  const pane = requirePane(key);
  if (pane.state.nativeClosePending) throw new Error("This window is closing; its Files documents are locked until that request is cancelled.");
  if (pane.state.transferPending) throw new Error("This file is being transferred; its source remains unchanged until handoff completes.");
  const snapshot = update(pane.state.snapshot);
  if (snapshot === pane.state.snapshot) return;
  if (pane.state.snapshot.revision === Number.MAX_SAFE_INTEGER) throw new Error("Files pane revision is exhausted; the current draft has been retained.");
  publish(pane, { snapshot: { ...snapshot, revision: pane.state.snapshot.revision + 1 } });
}
export function setFilesPaneError(key: string, error: string): void { publish(requirePane(key), { error }); }
export function registerFilesPaneFlush(key: string, flush: () => void): () => void {
  const pane = requirePane(key); pane.flush = flush;
  return () => { if (pane.flush === flush) pane.flush = undefined; };
}
export function captureFilesPane(key: string): FilesPaneSnapshot | undefined {
  const pane = panes.get(key);
  if (!pane) return undefined;
  if (pane.state.nativeClosePending) throw new Error("A Files snapshot cannot move while this window is closing.");
  if (pane.state.saving || pane.state.closeRequested) throw new Error("Finish saving or closing this file before moving it.");
  if (!pane.state.transferPending) pane.flush?.();
  const snapshot = normalizeFilesPaneSnapshot(pane.state.snapshot);
  if (!snapshot) throw new Error("The Files snapshot cannot be transferred without losing document data.");
  return snapshot;
}
export function filesPaneMatchesSnapshot(key: string, expected: FilesPaneSnapshot): boolean {
  const current = captureFilesPane(key);
  return current !== undefined && JSON.stringify(current) === JSON.stringify(expected);
}
export function setFilesPaneTransferPending(key: string, transferPending: boolean): void {
  const pane = panes.get(key);
  if (pane?.state.nativeClosePending && transferPending) throw new Error("A Files transfer cannot start while this window is closing.");
  if (pane) publish(pane, { transferPending });
}
export function forgetFilesPane(key: string, expected?: FilesPaneSnapshot): void {
  const pane = panes.get(key);
  if (!pane) return;
  if (pane.state.saving) throw new Error("A file save is still in progress.");
  if (expected && !filesPaneMatchesSnapshot(key, expected)) throw new Error("The Files pane changed after its handoff snapshot was captured.");
  if (filesSnapshotDirty(pane.state.snapshot) && !expected) throw new Error("The unsaved Files draft still belongs to this pane.");
  pane.resolveClose?.(false);
  panes.delete(key);
  generation += 1;
  for (const listener of listeners) listener();
}
export async function requestCloseFilesPanes(keys: readonly string[]): Promise<boolean> {
  return requestCloseFilesPanesOwned(keys);
}
async function requestCloseFilesPanesOwned(keys: readonly string[], signal?: AbortSignal, exclusive = false): Promise<boolean> {
  const approved = new Map<string, { id: string; revision: number }>();
  const initiated = new Map<Pane, Promise<boolean>>();
  const abort = () => {
    for (const [pane, promise] of initiated) {
      if (pane.closePromise !== promise) continue;
      const resolve = pane.resolveClose;
      pane.resolveClose = undefined; pane.closePromise = undefined;
      publish(pane, { closeRequested: false });
      resolve?.(false);
    }
  };
  signal?.addEventListener("abort", abort, { once: true });
  try {
    if (signal?.aborted) return false;
    for (const key of new Set(keys)) {
      const pane = panes.get(key);
      if (!pane) continue;
      if (signal?.aborted || pane.state.transferPending || pane.state.nativeClosePending || pane.state.saving || exclusive && pane.state.closeRequested) return false;
      pane.flush?.();
      if (filesSnapshotDirty(pane.state.snapshot)) {
        if (!pane.closePromise) {
          pane.closePromise = new Promise((resolve) => { pane.resolveClose = resolve; });
          initiated.set(pane, pane.closePromise);
          publish(pane, { closeRequested: true, error: "" });
        }
        if (signal?.aborted) { abort(); return false; }
        if (!await pane.closePromise || signal?.aborted) return false;
      }
      approved.set(key, { id: pane.state.snapshot.id, revision: pane.state.snapshot.revision });
    }
    return [...approved].every(([key, expected]) => {
      const state = panes.get(key)?.state;
      return state !== undefined && state.snapshot.id === expected.id && state.snapshot.revision === expected.revision && !state.saving && !state.transferPending && !state.nativeClosePending && !state.closeRequested && !filesSnapshotDirty(state.snapshot);
    });
  } finally {
    signal?.removeEventListener("abort", abort);
  }
}
export async function requestCloseAllFilesPanes(profile?: string, signal?: AbortSignal): Promise<boolean> {
  const matches = (key: string) => matchesProfile(key, profile);
  const keys = [...panes.keys()].filter(matches);
  if (signal?.aborted || keys.some((key) => requirePane(key).state.closeRequested)) return false;
  if (!await requestCloseFilesPanesOwned(keys, signal, true) || signal?.aborted) return false;
  return [...panes].every(([key, pane]) => !matches(key) || !pane.state.saving && !pane.state.transferPending && !pane.state.nativeClosePending && !pane.state.closeRequested && !filesSnapshotDirty(pane.state.snapshot));
}
export function acquireFilesWindowClose(requestId: string, profile?: string): boolean {
  if (!requestId.trim() || requestId.length > 128 || /[\u0000-\u001f\u007f]/.test(requestId)) throw new Error("Native Files close requires an exact request id.");
  if (windowCloseLock) return windowCloseLock.requestId === requestId && windowCloseLock.profile === profile;
  for (const [key, pane] of panes) {
    if (!matchesProfile(key, profile)) continue;
    if (pane.state.saving || pane.state.transferPending || pane.state.closeRequested) return false;
    pane.flush?.();
  }
  const matching = [...panes].filter(([key]) => matchesProfile(key, profile));
  if (matching.some(([, pane]) => filesSnapshotDirty(pane.state.snapshot) || pane.state.saving || pane.state.transferPending || pane.state.closeRequested)) return false;
  windowCloseLock = { requestId, ...(profile === undefined ? {} : { profile }) };
  for (const [, pane] of matching) pane.state = { ...pane.state, nativeClosePending: true };
  for (const [, pane] of matching) publish(pane, {});
  return true;
}
export function releaseFilesWindowClose(requestId: string): void {
  if (windowCloseLock?.requestId !== requestId) return;
  windowCloseLock = null;
  for (const pane of panes.values()) if (pane.state.nativeClosePending) publish(pane, { nativeClosePending: false });
}
export async function validateFilesPaneRoot(key: string, transport: Pick<SaveTransport, "osFileRoots">): Promise<void> {
  const snapshot = captureFilesPane(key);
  if (!snapshot) throw new Error("The incoming Files pane is unavailable.");
  if (snapshot.rootId && !filesRootMatches(snapshot, (await transport.osFileRoots()).roots)) throw new Error("The incoming Files document belongs to a different admitted root.");
}
export async function resolveFilesPaneClose(key: string, decision: "save" | "discard" | "cancel", transport?: SaveTransport): Promise<void> {
  const pane = requirePane(key);
  if (!pane.state.closeRequested || pane.state.saving || pane.state.transferPending || pane.state.nativeClosePending) return;
  const closePromise = pane.closePromise;
  if (decision === "save" && !await saveFilesPane(key, transport)) return;
  if (pane.closePromise !== closePromise || !pane.state.closeRequested) return;
  if (decision === "discard") updateFilesPane(key, (snapshot) => ({ ...snapshot, draft: snapshot.file?.text ?? "" }));
  const resolve = pane.resolveClose;
  pane.resolveClose = undefined; pane.closePromise = undefined;
  publish(pane, { closeRequested: false });
  resolve?.(decision !== "cancel");
}
export async function saveFilesPane(key: string, transport?: SaveTransport): Promise<boolean> {
  const pane = requirePane(key);
  if (pane.state.saving || pane.state.transferPending || pane.state.nativeClosePending) return false;
  const sent = pane.state.snapshot, file = sent.file;
  if (!filesSnapshotDirty(sent)) return true;
  if (!transport || !file || file.kind !== "text" || file.truncated || !file.digest) {
    publish(pane, { error: "This file has no complete writable document or desktop save transport." });
    return false;
  }
  publish(pane, { saving: true, error: "" });
  try {
    if (!filesRootMatches(sent, (await transport.osFileRoots()).roots)) throw new Error("The admitted Files root changed. The unsaved draft has been retained.");
    const saved = await transport.writeOsFile({ rootId: file.rootId, path: file.path, text: sent.draft, expectedDigest: file.digest, expectedModifiedAt: file.modifiedAt, expectedSize: file.size });
    if (!saved.outsideOctopusEvidence || saved.rootId !== file.rootId || saved.path !== file.path || saved.kind !== "text" || saved.truncated || saved.text !== sent.draft || !/^[a-f0-9]{64}$/.test(saved.digest)) throw new Error("OS Files returned an invalid manual-edit receipt.");
    if (pane.state.snapshot.file !== file) throw new Error("The file changed during save; its current draft has been retained.");
    const { outsideOctopusEvidence: _, ...savedFile } = saved;
    updateFilesPane(key, (current) => ({ ...current, file: savedFile as OctopusOsFile, draft: current.draft === sent.draft ? saved.text : current.draft }));
    return !filesSnapshotDirty(pane.state.snapshot);
  } catch (error) {
    publish(pane, { error: error instanceof Error ? error.message : String(error) });
    return false;
  } finally { publish(pane, { saving: false }); }
}
function requirePane(key: string): Pane {
  const pane = panes.get(key);
  if (!pane) throw new Error("The exact Files pane is unavailable.");
  return pane;
}
function matchesProfile(key: string, profile?: string): boolean { return profile === undefined || JSON.parse(key)[0] === profile; }
function publish(pane: Pane, change: Partial<FilesPaneState>): void {
  pane.state = { ...pane.state, ...change };
  for (const listener of pane.listeners) listener();
  generation += 1;
  for (const listener of listeners) listener();
}
