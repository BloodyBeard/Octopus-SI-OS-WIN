import type { ChatAttachment } from "./types";

export async function fileToBridgeAttachment(attachment: ChatAttachment) {
  if (attachment.stage?.contentHash) {
    return {
      id: attachment.id,
      name: attachment.name,
      label: attachment.label,
      type: attachment.type,
      size: attachment.size,
      lastModified: attachment.lastModified,
      stage: attachment.stage,
    };
  }
  const transport = window.octopusTransport;
  if (!attachment.file) throw new Error(`Attachment '${attachment.name}' has no staged file handle.`);
  if (!transport?.beginClientAttachmentStage || !transport.appendClientAttachmentStage || !transport.finalizeClientAttachmentStage || !transport.abortClientAttachmentStage) {
    throw new Error("Attachment staging is unavailable in this client host.");
  }
  const stage = await transport.beginClientAttachmentStage({
    name: attachment.name,
    label: attachment.label,
    type: attachment.type,
    size: attachment.size,
    lastModified: attachment.lastModified,
  });
  try {
    for (let offset = 0; offset < attachment.size; offset += STAGING_CHUNK_BYTES) {
      const bytes = await attachment.file.slice(offset, Math.min(attachment.size, offset + STAGING_CHUNK_BYTES)).arrayBuffer();
      await transport.appendClientAttachmentStage({ id: stage.id, bytes });
    }
    return {
      id: attachment.id,
      name: attachment.name,
      label: attachment.label,
      type: attachment.type,
      size: attachment.size,
      lastModified: attachment.lastModified,
      stage: await transport.finalizeClientAttachmentStage({ id: stage.id }),
    };
  } catch (error) {
    await transport.abortClientAttachmentStage({ id: stage.id }).catch(() => {});
    throw error;
  }
}

const STAGING_CHUNK_BYTES = 4 * 1024 * 1024;

export function attachmentLabel(file: File): string {
  const mime = (file.type || "").toLowerCase();
  const name = file.name.toLowerCase();
  if (mime.startsWith("image/")) return "operator.media.image";
  if (mime.startsWith("audio/")) return "operator.media.audio";
  if (mime.startsWith("video/")) return "operator.media.video";
  if (mime.startsWith("text/") || /\.(txt|md|json|csv|log|rs|ts|tsx|js|jsx|css|html|toml|yaml|yml)$/i.test(name)) {
    return "operator.file.text";
  }
  return "operator.file";
}

export function randomId(): string {
  const bytes = new Uint8Array(8);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
