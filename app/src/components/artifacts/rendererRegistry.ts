import type { ArtifactPreviewModel, ArtifactReference, ArtifactRendererKind } from "./types";

const MAX_INLINE_BYTES = 40 * 1024 * 1024;
const MAX_DATA_URL_CHARS = Math.ceil(MAX_INLINE_BYTES / 3) * 4 + 1024;

const CODE_EXTENSIONS = new Set([
  "c", "cc", "cpp", "cs", "css", "go", "h", "hpp", "html", "java", "js", "jsx", "kt", "lua", "m", "mdx",
  "php", "pl", "py", "rb", "rs", "scala", "sh", "sql", "swift", "toml", "ts", "tsx", "vue", "xml", "yaml", "yml",
]);

const TEXT_EXTENSIONS = new Set(["csv", "ini", "log", "md", "rst", "text", "txt"]);
const DOCUMENT_EXTENSIONS = new Set(["doc", "docx", "odt", "rtf"]);

export function createArtifactPreviewModel(reference: ArtifactReference, payload: unknown): ArtifactPreviewModel {
  const record = object(payload);
  const media = object(record.media);
  const filename = firstText(
    media.name,
    record.name,
    record.file_name,
    record.filename,
    basename(firstText(media.path, record.path)),
    reference.label,
  ) || reference.artifactId;
  const extension = fileExtension(filename);
  const declaredMediaType = firstText(media.mime_type, media.mimeType, record.mime_type, record.mimeType, record.content_type);
  const sourceText = payloadText(payload);
  const dataUrl = embeddedDataUrl(record, media, declaredMediaType || mimeForExtension(extension));
  const mediaType = declaredMediaType || dataUrlMediaType(dataUrl) || mimeForExtension(extension);
  const bytes = embeddedBytes(record, media);
  const kind = rendererKind({ extension, mediaType, payload, record, sourceText });
  const title = firstText(record.title, record.display_name, filename, reference.label) || "Artifact";
  return {
    bytes,
    dataUrl,
    filename,
    kind,
    language: firstText(record.language, record.lang) || languageFor(extension),
    mediaType,
    payload,
    searchableText: searchableText(kind, payload, sourceText),
    title,
  };
}

export function artifactKindHint(label: string): ArtifactRendererKind {
  const extension = fileExtension(label);
  if (extension === "ipynb") return "notebook";
  if (extension === "pdf") return "pdf";
  if (["avif", "bmp", "gif", "jpeg", "jpg", "png", "svg", "webp"].includes(extension)) return "image";
  if (["aac", "flac", "m4a", "mp3", "mp4", "ogg", "wav", "webm"].includes(extension)) return "media";
  if (DOCUMENT_EXTENSIONS.has(extension)) return "document";
  if (CODE_EXTENSIONS.has(extension)) return "code";
  if (TEXT_EXTENSIONS.has(extension)) return "text";
  if (extension === "json" || label.toLocaleLowerCase().includes("json")) return "json";
  return "unsupported";
}

export function previewDownload(model: ArtifactPreviewModel): { bytes: Uint8Array; filename: string; mediaType: string } | null {
  if (model.bytes) return { bytes: model.bytes, filename: model.filename, mediaType: model.mediaType || "application/octet-stream" };
  if (!model.dataUrl) return null;
  const parsed = decodeDataUrl(model.dataUrl);
  return parsed ? { ...parsed, filename: model.filename } : null;
}

export function payloadText(payload: unknown): string {
  if (typeof payload === "string") return payload;
  const record = object(payload);
  for (const value of [record.__raw, record.text, record.content, record.code, record.markdown, record.message, record.summary]) {
    if (typeof value === "string") return value;
  }
  return "";
}

function rendererKind(input: {
  extension: string;
  mediaType: string;
  payload: unknown;
  record: Record<string, unknown>;
  sourceText: string;
}): ArtifactRendererKind {
  const mime = input.mediaType.toLocaleLowerCase();
  if (isNotebook(input.payload) || input.extension === "ipynb") return "notebook";
  if (mime === "application/pdf" || input.extension === "pdf") return "pdf";
  if (mime.startsWith("image/") || ["avif", "bmp", "gif", "jpeg", "jpg", "png", "svg", "webp"].includes(input.extension)) return "image";
  if (mime.startsWith("audio/") || mime.startsWith("video/") || ["aac", "flac", "m4a", "mp3", "mp4", "ogg", "wav", "webm"].includes(input.extension)) return "media";
  if (DOCUMENT_EXTENSIONS.has(input.extension) || Array.isArray(input.record.pages)) return "document";
  if (CODE_EXTENSIONS.has(input.extension) || firstText(input.record.language, input.record.lang)) return "code";
  if (input.extension === "json" || isJsonText(input.sourceText) || (input.payload !== null && typeof input.payload === "object")) return "json";
  if (input.sourceText || TEXT_EXTENSIONS.has(input.extension) || mime.startsWith("text/")) return "text";
  return "unsupported";
}

function searchableText(kind: ArtifactRendererKind, payload: unknown, sourceText: string): string {
  if (kind === "notebook") return notebookText(payload);
  if (sourceText) return sourceText;
  if (payload !== undefined) {
    try {
      return JSON.stringify(payload, null, 2);
    } catch {
      return "";
    }
  }
  return "";
}

function notebookText(payload: unknown): string {
  const notebook = notebookRecord(payload);
  if (!notebook) return "";
  return notebook.cells.flatMap((cell) => {
    const row = object(cell);
    return [joinedText(row.source), ...array(row.outputs).flatMap((output) => {
      const value = object(output);
      return [joinedText(value.text), joinedText(value.traceback), json(value.data)];
    })];
  }).filter(Boolean).join("\n");
}

function isNotebook(payload: unknown): boolean {
  return notebookRecord(payload) !== null;
}

function notebookRecord(payload: unknown): { cells: unknown[] } | null {
  const direct = object(payload);
  if (Array.isArray(direct.cells)) return { cells: direct.cells };
  const nested = object(direct.notebook);
  if (Array.isArray(nested.cells)) return { cells: nested.cells };
  const document = object(direct.document);
  return Array.isArray(document.cells) ? { cells: document.cells } : null;
}

function embeddedDataUrl(record: Record<string, unknown>, media: Record<string, unknown>, mediaType: string): string | undefined {
  for (const value of [media.data_url, media.dataUrl, record.data_url, record.dataUrl, record.url]) {
    if (typeof value === "string" && safeDataUrl(value)) return value;
  }
  const bytes = embeddedBytes(record, media);
  if (!bytes || !mediaType || !allowedInlineMediaType(mediaType)) return undefined;
  return `data:${mediaType};base64,${bytesToBase64(bytes)}`;
}

function embeddedBytes(record: Record<string, unknown>, media: Record<string, unknown>): Uint8Array | undefined {
  const value = firstText(media.base64, media.contents_base64, media.contentsBase64, record.base64, record.contents_base64, record.contentsBase64);
  if (!value) return undefined;
  return decodeBase64(value);
}

function safeDataUrl(value: string): boolean {
  if (value.length > MAX_DATA_URL_CHARS) return false;
  const match = /^data:([^;,]+)(?:;[^,]*)?,/i.exec(value);
  return Boolean(match && allowedInlineMediaType(match[1]));
}

function dataUrlMediaType(value: string | undefined): string {
  if (!value) return "";
  return /^data:([^;,]+)(?:;[^,]*)?,/i.exec(value)?.[1] ?? "";
}

function allowedInlineMediaType(value: string): boolean {
  const mime = value.toLocaleLowerCase();
  return mime === "application/pdf" || mime.startsWith("image/") || mime.startsWith("audio/") || mime.startsWith("video/");
}

function decodeDataUrl(value: string): { bytes: Uint8Array; mediaType: string } | null {
  const match = /^data:([^;,]+)(;base64)?,(.*)$/is.exec(value);
  if (!match || !allowedInlineMediaType(match[1])) return null;
  try {
    const bytes = match[2]
      ? decodeBase64(match[3])
      : new TextEncoder().encode(decodeURIComponent(match[3]));
    return bytes && bytes.length <= MAX_INLINE_BYTES ? { bytes, mediaType: match[1] } : null;
  } catch {
    return null;
  }
}

function decodeBase64(value: string): Uint8Array | undefined {
  const normalized = value.replaceAll(/\s/g, "");
  if (!normalized || normalized.length > Math.ceil(MAX_INLINE_BYTES / 3) * 4 || !/^[A-Za-z0-9+/]*={0,2}$/.test(normalized)) return undefined;
  try {
    const binary = atob(normalized);
    if (binary.length > MAX_INLINE_BYTES) return undefined;
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
    return bytes;
  } catch {
    return undefined;
  }
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let index = 0; index < bytes.length; index += chunk) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunk));
  }
  return btoa(binary);
}

function languageFor(extension: string): string {
  const aliases: Record<string, string> = {
    c: "c", cc: "cpp", cpp: "cpp", cs: "csharp", h: "c", hpp: "cpp", js: "javascript", jsx: "jsx",
    md: "markdown", py: "python", rb: "ruby", rs: "rust", sh: "shell", ts: "typescript", tsx: "tsx", yml: "yaml",
  };
  return aliases[extension] || extension || "text";
}

function mimeForExtension(extension: string): string {
  const aliases: Record<string, string> = {
    aac: "audio/aac",
    avif: "image/avif",
    bmp: "image/bmp",
    flac: "audio/flac",
    gif: "image/gif",
    jpeg: "image/jpeg",
    jpg: "image/jpeg",
    m4a: "audio/mp4",
    mp3: "audio/mpeg",
    mp4: "video/mp4",
    ogg: "audio/ogg",
    pdf: "application/pdf",
    png: "image/png",
    svg: "image/svg+xml",
    wav: "audio/wav",
    webm: "video/webm",
    webp: "image/webp",
  };
  return aliases[extension] || "";
}

function fileExtension(value: string): string {
  const clean = value.split(/[?#]/, 1)[0] ?? "";
  const index = clean.lastIndexOf(".");
  return index < 0 ? "" : clean.slice(index + 1).toLocaleLowerCase();
}

function basename(value: string): string {
  return value.split(/[\\/]/).filter(Boolean).at(-1) ?? "";
}

function object(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function array(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function joinedText(value: unknown): string {
  if (typeof value === "string") return value;
  return Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === "string").join("") : "";
}

function json(value: unknown): string {
  if (value === undefined) return "";
  try {
    return JSON.stringify(value);
  } catch {
    return "";
  }
}

function firstText(...values: unknown[]): string {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return "";
}

function isJsonText(value: string): boolean {
  const text = value.trim();
  if (!text || !["{", "["].includes(text[0])) return false;
  try {
    JSON.parse(text);
    return true;
  } catch {
    return false;
  }
}
