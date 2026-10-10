import type { OctopusClientAttachmentSelection, OctopusClientAttachmentStageHandle } from "../../electron";
import { CLIENT_PANE_DRAG_MIME } from "../../shell/clientWindowModel";
import { MAX_ATTACHMENT_BYTES } from "./constants";

type DropEntry = { file: File; isDirectory: boolean };
export type DroppedAttachment = File | OctopusClientAttachmentStageHandle;
type DropTransport = {
  stageDroppedAttachments(files: readonly File[]): Promise<OctopusClientAttachmentSelection & { fileIndices: number[] }>;
  discardClientAttachmentStages(handles: readonly OctopusClientAttachmentStageHandle[]): Promise<void>;
};

export function isAttachmentDrop(transfer: DataTransfer): boolean {
  const types = Array.from(transfer.types);
  return !types.includes(CLIENT_PANE_DRAG_MIME)
    && (types.includes("Files") || Array.from(transfer.items).some((item) => item.kind === "file") || transfer.files.length > 0);
}

export function captureAttachmentDrop(transfer: DataTransfer): DropEntry[] {
  if (!isAttachmentDrop(transfer)) return [];
  const files = Array.from(transfer.files);
  const items = Array.from(transfer.items).filter((item) => item.kind === "file").map((item) => ({
    file: item.getAsFile(),
    isDirectory: item.webkitGetAsEntry?.()?.isDirectory === true,
  }));
  const entries = items.length >= files.length && items.length > 0
    ? items.map((item, index) => ({ ...item, file: item.file ?? files[index] }))
    : files.map((file, index) => ({ file, isDirectory: items[index]?.isDirectory ?? false }));
  if (entries.some((entry) => !entry.file)) throw new Error("A dropped file or folder could not be read. Drop it again or use Attach.");
  return entries as DropEntry[];
}

export function validateAttachmentDrop(values: readonly DroppedAttachment[], remaining: number): void {
  if (values.length > remaining) throw new Error(`A task can include up to 12 attachments; this drop has ${values.length} items and ${remaining} slots are available.`);
  for (const value of values) {
    if (value.size > MAX_ATTACHMENT_BYTES) throw new Error(`Attachment too large: ${value.name}`);
    if (!(value instanceof File) && !value.contentHash) throw new Error(`Attachment staging did not complete: ${value.name}`);
  }
  if (values.reduce((sum, value) => sum + value.size, 0) > 512 * 1024 * 1024) throw new Error("Dropped files and folders exceed 512 MB in total.");
}

export async function discardAttachmentDrop(values: readonly DroppedAttachment[], transport?: DropTransport): Promise<void> {
  const handles = values.filter((value): value is OctopusClientAttachmentStageHandle => !(value instanceof File));
  if (!handles.length) return;
  if (!transport?.discardClientAttachmentStages) throw new Error("Attachment cleanup is unavailable in this client host.");
  await transport.discardClientAttachmentStages(handles);
}

export async function resolveAttachmentDrop(entries: readonly DropEntry[], transport?: DropTransport): Promise<DroppedAttachment[] | null> {
  const files = entries.map((entry) => entry.file);
  validateAttachmentDrop(files, 12);
  if (!files.length) return [];
  if (!transport?.stageDroppedAttachments) {
    if (entries.some((entry) => entry.isDirectory)) throw new Error("Folder drops require native attachment packaging. Use Attach in the desktop client.");
    return files;
  }
  if (!transport.discardClientAttachmentStages) throw new Error("Attachment cleanup is unavailable in this client host.");
  const selection = await transport.stageDroppedAttachments(files);
  if (selection.canceled) {
    await discardAttachmentDrop(selection.attachments, transport);
    return null;
  }
  try {
    const indices = selection.fileIndices;
    if (indices.length !== selection.attachments.length || new Set(indices).size !== indices.length
      || indices.some((index) => !Number.isInteger(index) || index < 0 || index >= files.length)) {
      throw new Error("Dropped attachment staging returned an invalid file mapping.");
    }
    const staged = new Map(indices.map((index, position) => [index, selection.attachments[position]]));
    const resolved = entries.map((entry, index) => {
      const handle = staged.get(index);
      if (handle) return handle;
      if (entry.isDirectory) throw new Error(`The dropped folder '${entry.file.name}' has no native file handle. Use Attach.`);
      return entry.file;
    });
    validateAttachmentDrop(resolved, 12);
    return resolved;
  } catch (error) {
    try { await discardAttachmentDrop(selection.attachments, transport); }
    catch (cleanup) { throw new Error(`${error instanceof Error ? error.message : String(error)}; attachment cleanup failed: ${cleanup instanceof Error ? cleanup.message : String(cleanup)}`); }
    throw error;
  }
}
