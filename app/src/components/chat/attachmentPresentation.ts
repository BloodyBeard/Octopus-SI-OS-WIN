import { readProjection } from "../../projectionClient";
import type { AttachmentIconName } from "./AttachmentIcon";
import { artifactKindHint, createArtifactPreviewModel } from "../artifacts/rendererRegistry";
import type { ArtifactPreviewModel } from "../artifacts/types";
import type { ChatAttachment, ChatAttachmentView, ChatRenderMessage } from "./types";

export type AttachmentScope = Pick<ChatRenderMessage, "artifactId" | "runId" | "interfaceId" | "pending">;

export type AttachmentSource = Pick<ChatAttachment, "id" | "type" | "size" | "file"> & {
  stage?: Pick<NonNullable<ChatAttachment["stage"]>, "id" | "contentHash">;
};

export function imageAttachmentType(type: string): boolean {
  return /^image\/(?:avif|bmp|gif|jpeg|png|webp)$/i.test(type);
}

const fileIcons: Record<string, AttachmentIconName> = Object.fromEntries(([
  ["typescript", ["ts"]], ["react", ["tsx", "jsx"]], ["javascript", ["js", "mjs", "cjs", "hs"]],
  ["python", ["py"]], ["java", ["java"]], ["rust", ["rs"]], ["php", ["php"]], ["css", ["css", "scss", "less", "sass"]],
  ["cplusplus", ["cpp", "cxx", "cc", "c", "hpp", "hh", "h"]], ["code", ["rb", "go", "kt", "swift", "m", "mm", "cs", "sql"]],
  ["json", ["json", "jsonc"]], ["document", ["md", "mdx", "markdown", "mkd", "mdown", "xml", "env", "dotenv", "gitignore", "lock"]],
  ["html", ["html", "htm"]], ["file", ["yaml", "yml"]], ["toml", ["toml"]],
  ["spreadsheet", ["csv", "tsv", "xls", "xlsm", "xlsx"]], ["artifactDocument", ["doc", "docx"]],
  ["notebook", ["ipynb"]], ["presentation", ["ppt", "pptx"]], ["shell", ["sh", "bash", "zsh", "fish", "ps1"]],
  ["terminal", ["dockerfile"]], ["image", ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico"]],
  ["build", ["build", "bazel", "bzl", "ninja", "gradle", "mk", "makefile"]],
  ["hashes", ["sha", "sha1", "sha256", "md5", "checksum", "sum"]], ["pdf", ["pdf"]], ["folder", ["zip", "gz", "tgz", "tar"]],
] satisfies [AttachmentIconName, string[]][]).flatMap(([kind, extensions]) => extensions.map((extension) => [extension, kind])));

export function attachmentPresentation(file: ChatAttachmentView): { extension: string; icon: AttachmentIconName } {
  const name = file.name.toLowerCase().split(/[\\/]/).at(-1) ?? "";
  const dot = name.lastIndexOf(".");
  const extension = dot < 0 ? name : name.slice(dot + 1);
  const mime = file.type.toLowerCase();
  const icon = /[\\/]$/.test(file.name) ? "folder" : name === "skill.md" ? "skill" : fileIcons[extension]
    ?? (mime.startsWith("image/") ? "image" : mime.startsWith("text/") ? "document"
      : mime.startsWith("application/pdf") ? "pdf" : /^application\/(zip|gzip)/.test(mime) ? "folder" : "file");
  return { extension: dot > 0 && dot < name.length - 1 ? extension.toUpperCase() : "", icon };
}

export async function readAttachmentImage(file: AttachmentSource, scope?: AttachmentScope): Promise<Blob> {
  if (!imageAttachmentType(file.type)) throw new Error("This file type has no image preview.");
  const blob = await readAttachmentBytes(file, scope);
  if (!blob.size) throw new Error("The image preview bytes are empty.");
  return blob;
}

export function attachmentPreviewModel(file: ChatAttachmentView, bytes: Uint8Array, scope?: AttachmentScope): ArtifactPreviewModel | null {
  const hint = artifactKindHint(file.name);
  const kind = hint === "json" ? "code" : hint === "unsupported" && file.type.startsWith("text/") ? "text" : hint;
  if (["document", "image", "unsupported"].includes(kind)) return null;
  const textual = ["code", "text", "json", "notebook"].includes(kind);
  const text = textual ? new TextDecoder("utf-8", { fatal: true }).decode(bytes) : "";
  let base64 = "";
  if (!textual) {
    let binary = "";
    for (let index = 0; index < bytes.length; index += 0x8000) binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
    base64 = btoa(binary);
  }
  const payload = kind === "notebook" ? JSON.parse(text) as unknown : textual ? text
    : { media: { name: file.name, mime_type: file.type, base64 } };
  const model = createArtifactPreviewModel({ artifactId: scope?.artifactId ?? file.id, label: file.name,
    runId: scope?.runId, interfaceId: scope?.interfaceId, source: "message" }, payload);
  return { ...model, bytes, filename: file.name, kind, language: hint === "json" ? "json" : model.language, mediaType: file.type };
}

export async function readAttachmentBytes(file: AttachmentSource, scope?: AttachmentScope): Promise<Blob> {
  if (!scope && file.file) {
    if (file.file.size !== file.size || (file.file.type && file.file.type.toLowerCase() !== file.type.toLowerCase())) {
      throw new Error("The selected file metadata does not match this attachment.");
    }
    return file.file;
  }
  let result: { base64: string; mimeType: string; contentHash: string; size: number };
  let expectedHash = "";
  if (scope) {
    const { artifactId, runId, interfaceId } = scope;
    if (!artifactId || !runId || !interfaceId) throw new Error("The file has no admitted attachment coordinates.");
    const nested = file.id !== artifactId;
    const bytes = await readProjection<{
      artifact_id: string; run_id: string; interface_id: string; attachment_id?: string;
      base64: string; mime_type: string; content_hash: string; size: number;
    }>("projection.artifact.bytes.read", {
      artifact_id: artifactId, run_id: runId, interface_id: interfaceId,
      ...(nested ? { attachment_id: file.id } : {}),
    });
    if (bytes.artifact_id !== artifactId || bytes.run_id !== runId || bytes.interface_id !== interfaceId
      || (nested && bytes.attachment_id !== file.id)) throw new Error("The file preview does not match this attachment.");
    expectedHash = nested ? file.id : "";
    result = { base64: bytes.base64, mimeType: bytes.mime_type, contentHash: bytes.content_hash, size: bytes.size };
  } else {
    const stage = file.stage;
    const transport = window.octopusTransport;
    if (!stage?.contentHash || !transport?.readClientAttachmentPreview) throw new Error("The staged file preview is unavailable.");
    expectedHash = stage.contentHash;
    result = await transport.readClientAttachmentPreview({ id: stage.id, contentHash: stage.contentHash });
  }
  if (!/^[A-Za-z0-9!#$&^_.+-]+\/[A-Za-z0-9!#$&^_.+-]+$/.test(result.mimeType) || result.mimeType.toLowerCase() !== file.type.toLowerCase()
    || !/^[0-9a-f]{64}$/.test(result.contentHash) || (expectedHash && result.contentHash !== expectedHash)
    || !Number.isSafeInteger(result.size) || result.size < 0 || result.size !== file.size) {
    throw new Error("The file preview metadata does not match this attachment.");
  }
  const binary = atob(result.base64);
  if (binary.length !== result.size) throw new Error("The file preview bytes are incomplete.");
  return new Blob([Uint8Array.from(binary, (character) => character.charCodeAt(0))], { type: result.mimeType });
}
