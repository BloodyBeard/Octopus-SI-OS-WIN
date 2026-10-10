const fs = require("node:fs/promises");
const path = require("node:path");
const { createHash, randomUUID } = require("node:crypto");

const MAX_DIRECTORY_ENTRIES = 2_000;
const MAX_TEXT_PREVIEW_BYTES = 512 * 1024;
const MAX_BINARY_PREVIEW_BYTES = 40 * 1024 * 1024;
const MAX_ATTACHMENT_BYTES = 100 * 1024 * 1024;
const MAX_RELATIVE_PATH_BYTES = 4_096;

const BINARY_PREVIEW_TYPES = new Map([
  [".avif", "image/avif"],
  [".bmp", "image/bmp"],
  [".gif", "image/gif"],
  [".jpeg", "image/jpeg"],
  [".jpg", "image/jpeg"],
  [".pdf", "application/pdf"],
  [".png", "image/png"],
  [".webp", "image/webp"],
]);

const ATTACHMENT_TYPES = new Map([
  [".avif", "image/avif"],
  [".bmp", "image/bmp"],
  [".csv", "text/csv"],
  [".gif", "image/gif"],
  [".html", "text/html"],
  [".jpeg", "image/jpeg"],
  [".jpg", "image/jpeg"],
  [".json", "application/json"],
  [".md", "text/markdown"],
  [".pdf", "application/pdf"],
  [".png", "image/png"],
  [".svg", "image/svg+xml"],
  [".toml", "text/plain"],
  [".tsv", "text/tab-separated-values"],
  [".txt", "text/plain"],
  [".webp", "image/webp"],
  [".xml", "application/xml"],
  [".yaml", "application/yaml"],
  [".yml", "application/yaml"],
]);

function createWorkerEnvironmentFileReader(options) {
  if (typeof options?.resolveSurface !== "function") {
    throw new Error("Worker Environment file reader requires a projected surface resolver");
  }

  async function list(request) {
    const descriptor = await resolveDescriptor(options, request);
    const location = await resolveInsideProjectedRoot(descriptor.root, request?.path);
    const stat = await fs.lstat(location.target);
    if (stat.isSymbolicLink() || !stat.isDirectory()) {
      throw new Error("Projected filesystem target is not a readable directory");
    }
    const rows = await fs.readdir(location.target, { withFileTypes: true });
    if (rows.length > MAX_DIRECTORY_ENTRIES) {
      throw new Error(`Projected directory has ${rows.length} entries; client limit is ${MAX_DIRECTORY_ENTRIES}`);
    }
    const entries = await Promise.all(rows.map(async (entry) => {
      const target = path.join(location.target, entry.name);
      const detail = await fs.lstat(target).catch(() => null);
      const kind = detail?.isSymbolicLink()
        ? "symlink"
        : detail?.isDirectory()
          ? "directory"
          : detail?.isFile()
            ? "file"
            : "other";
      return {
        name: entry.name,
        path: joinPortable(location.relativePath, entry.name),
        kind,
        size: kind === "file" && Number.isSafeInteger(detail?.size) ? detail.size : null,
        modifiedAt: detail?.mtime?.toISOString?.() ?? "",
      };
    }));
    entries.sort((left, right) => (
      fileKindRank(left.kind) - fileKindRank(right.kind)
      || left.name.localeCompare(right.name, undefined, { numeric: true, sensitivity: "base" })
    ));
    return {
      workerEnvironmentId: descriptor.workerEnvironmentId,
      surfaceId: descriptor.id,
      path: location.relativePath,
      entries,
    };
  }

  async function read(request) {
    const descriptor = await resolveDescriptor(options, request);
    const location = await resolveInsideProjectedRoot(descriptor.root, request?.path);
    const stat = await fs.lstat(location.target);
    if (stat.isSymbolicLink() || !stat.isFile()) {
      throw new Error("Projected filesystem target is not a readable file");
    }
    const handle = await fs.open(location.target, "r");
    try {
      const bytesToRead = Math.min(stat.size, MAX_TEXT_PREVIEW_BYTES);
      const buffer = Buffer.alloc(bytesToRead);
      const { bytesRead } = await handle.read(buffer, 0, bytesToRead, 0);
      const content = buffer.subarray(0, bytesRead);
      const text = decodeUtf8Text(content);
      return {
        workerEnvironmentId: descriptor.workerEnvironmentId,
        surfaceId: descriptor.id,
        path: location.relativePath,
        name: path.basename(location.target),
        size: stat.size,
        modifiedAt: stat.mtime.toISOString(),
        kind: text == null ? "binary" : "text",
        text: text ?? "",
        truncated: stat.size > bytesRead,
        digest: text != null && stat.size === bytesRead ? sha256(content) : "",
      };
    } finally {
      await handle.close();
    }
  }

  async function readPreview(request) {
    const descriptor = await resolveDescriptor(options, request);
    const location = await resolveInsideProjectedRoot(descriptor.root, request?.path);
    const stat = await fs.lstat(location.target);
    if (stat.isSymbolicLink() || !stat.isFile()) {
      throw new Error("Projected filesystem target is not a readable file");
    }
    const mimeType = BINARY_PREVIEW_TYPES.get(path.extname(location.target).toLowerCase());
    if (!mimeType) {
      throw new Error("This binary format has no bounded client preview");
    }
    if (stat.size <= 0 || stat.size > MAX_BINARY_PREVIEW_BYTES) {
      throw new Error(`Binary preview must be between 1 byte and ${MAX_BINARY_PREVIEW_BYTES / (1024 * 1024)} MB`);
    }
    const content = await fs.readFile(location.target);
    if (content.byteLength !== stat.size || content.byteLength > MAX_BINARY_PREVIEW_BYTES) {
      throw new Error("Projected file changed while its preview was being read");
    }
    if (!matchesPreviewSignature(content, mimeType)) {
      throw new Error(`Projected file bytes do not match ${mimeType}`);
    }
    return {
      workerEnvironmentId: descriptor.workerEnvironmentId,
      surfaceId: descriptor.id,
      path: location.relativePath,
      name: path.basename(location.target),
      size: content.byteLength,
      modifiedAt: stat.mtime.toISOString(),
      mimeType,
      bytes: content.buffer.slice(content.byteOffset, content.byteOffset + content.byteLength),
    };
  }

  async function readAttachment(request) {
    const descriptor = await resolveDescriptor(options, request);
    const location = await resolveInsideProjectedRoot(descriptor.root, request?.path);
    const stat = await fs.lstat(location.target);
    if (stat.isSymbolicLink() || !stat.isFile()) {
      throw new Error("Projected filesystem target is not a readable file");
    }
    if (stat.size > MAX_ATTACHMENT_BYTES) {
      throw new Error(`Projected attachment exceeds the ${MAX_ATTACHMENT_BYTES / (1024 * 1024)} MB client limit`);
    }
    const handle = await fs.open(location.target, "r");
    try {
      const before = await handle.stat();
      const content = await handle.readFile();
      const after = await handle.stat();
      if (content.byteLength !== before.size || before.size !== after.size || before.mtimeMs !== after.mtimeMs) {
        throw new Error("Projected file changed while it was being attached");
      }
      return {
        workerEnvironmentId: descriptor.workerEnvironmentId,
        surfaceId: descriptor.id,
        path: location.relativePath,
        name: path.basename(location.target),
        size: content.byteLength,
        modifiedAt: after.mtime.toISOString(),
        mimeType: ATTACHMENT_TYPES.get(path.extname(location.target).toLowerCase()) ?? "application/octet-stream",
        bytes: content.buffer.slice(content.byteOffset, content.byteOffset + content.byteLength),
      };
    } finally {
      await handle.close();
    }
  }

  async function resolvePath(request) {
    const descriptor = await resolveDescriptor(options, request);
    const location = await resolveInsideProjectedRoot(descriptor.root, request?.path);
    const stat = await fs.lstat(location.target);
    if (stat.isSymbolicLink() || (!stat.isFile() && !stat.isDirectory())) {
      throw new Error("Projected filesystem target is not a regular file or directory");
    }
    return {
      workerEnvironmentId: descriptor.workerEnvironmentId,
      surfaceId: descriptor.id,
      path: location.relativePath,
      absolutePath: location.target,
      kind: stat.isDirectory() ? "directory" : "file",
    };
  }

  async function write(request) {
    const descriptor = await resolveDescriptor(options, request);
    if (descriptor.readOnly !== false || stringValue(descriptor.protocol).toLowerCase() !== "filesystem.readwrite") {
      throw new Error("Worker Environment filesystem surface is not projected as writable");
    }
    const text = typeof request?.text === "string" ? request.text : null;
    if (text == null) throw new Error("Manual file edit requires text");
    const bytes = Buffer.from(text, "utf8");
    if (bytes.byteLength > MAX_TEXT_PREVIEW_BYTES) {
      throw new Error(`Manual file edit exceeds ${MAX_TEXT_PREVIEW_BYTES / 1024} KB`);
    }
    const expectedDigest = requiredDigest(request?.expectedDigest);
    const expectedModifiedAt = requiredString(request?.expectedModifiedAt, "expectedModifiedAt");
    const expectedSize = Number(request?.expectedSize);
    if (!Number.isSafeInteger(expectedSize) || expectedSize < 0 || expectedSize > MAX_TEXT_PREVIEW_BYTES) {
      throw new Error("Manual file edit expected size is invalid");
    }
    const location = await resolveInsideProjectedRoot(descriptor.root, request?.path);
    const stat = await fs.lstat(location.target);
    if (stat.isSymbolicLink() || !stat.isFile()) throw new Error("Projected filesystem target is not a writable file");
    const current = await fs.readFile(location.target);
    if (current.includes(0) || decodeUtf8Text(current) == null) throw new Error("Binary files cannot be edited as text");
    if (stat.size !== expectedSize || stat.mtime.toISOString() !== expectedModifiedAt || sha256(current) !== expectedDigest) {
      throw new Error("Projected file changed after it was opened; reload before saving");
    }
    const temporary = path.join(path.dirname(location.target), `.${path.basename(location.target)}.${process.pid}.${randomUUID()}.tmp`);
    try {
      await fs.writeFile(temporary, bytes, { flag: "wx", mode: stat.mode & 0o777 });
      const latestStat = await fs.lstat(location.target);
      if (latestStat.isSymbolicLink() || !latestStat.isFile()) {
        throw new Error("Projected file stopped being a regular file before it could be saved");
      }
      const latest = await fs.readFile(location.target);
      if (
        latestStat.dev !== stat.dev
        || latestStat.ino !== stat.ino
        || latestStat.size !== expectedSize
        || latestStat.mtime.toISOString() !== expectedModifiedAt
        || sha256(latest) !== expectedDigest
      ) {
        throw new Error("Projected file changed while the replacement was being prepared; reload before saving");
      }
      await fs.rename(temporary, location.target);
    } finally {
      await fs.rm(temporary, { force: true }).catch(() => {});
    }
    const after = await fs.lstat(location.target);
    if (after.isSymbolicLink() || !after.isFile()) throw new Error("Saved file is no longer a regular file");
    const target = await fs.realpath(location.target);
    assertInside(location.root, target);
    return {
      workerEnvironmentId: descriptor.workerEnvironmentId,
      surfaceId: descriptor.id,
      path: location.relativePath,
      name: path.basename(location.target),
      size: after.size,
      modifiedAt: after.mtime.toISOString(),
      kind: "text",
      text,
      truncated: false,
      digest: sha256(bytes),
      outsideOctopusEvidence: true,
    };
  }

  return { list, read, readPreview, readAttachment, resolvePath, write };
}

async function resolveDescriptor(options, request) {
  const workerEnvironmentId = requiredId(request?.workerEnvironmentId, "workerEnvironmentId");
  const surfaceId = requiredId(request?.surfaceId, "surfaceId");
  const descriptor = await options.resolveSurface(workerEnvironmentId, surfaceId);
  validateProjectedFileSurface(descriptor);
  return { ...descriptor, workerEnvironmentId };
}

function validateProjectedFileSurface(descriptor) {
  if (!descriptor || descriptor.kind !== "files" || descriptor.mode !== "direct") {
    throw new Error("Worker Environment did not project a direct filesystem surface");
  }
  const protocol = stringValue(descriptor.protocol).toLowerCase();
  const validAccess = protocol === "filesystem.readonly"
    ? descriptor.readOnly === true
    : protocol === "filesystem.readwrite" && descriptor.readOnly === false;
  if (!validAccess) {
    throw new Error("Worker Environment filesystem surface has an invalid access projection");
  }
  const root = requiredString(descriptor.root, "projected filesystem root");
  if (!path.isAbsolute(root)) throw new Error("Projected filesystem root must be absolute");
}

async function resolveInsideProjectedRoot(rootValue, pathValue) {
  const projectedRoot = requiredString(rootValue, "projected filesystem root");
  const relativePath = safeRelativePath(pathValue);
  const rootStat = await fs.lstat(projectedRoot);
  if (rootStat.isSymbolicLink() || !rootStat.isDirectory()) {
    throw new Error("Projected filesystem root must be a regular directory, not a symbolic link");
  }
  const root = await fs.realpath(projectedRoot);
  const candidate = path.resolve(root, relativePath || ".");
  assertInside(root, candidate);
  const target = await fs.realpath(candidate);
  assertInside(root, target);
  return { root, target, relativePath: portableRelative(root, target) };
}

function safeRelativePath(value) {
  const text = typeof value === "string" ? value.trim() : "";
  if (Buffer.byteLength(text) > MAX_RELATIVE_PATH_BYTES || text.includes("\0") || path.isAbsolute(text)) {
    throw new Error("Projected filesystem path must be a bounded relative path");
  }
  const portable = text.replaceAll("\\", "/");
  if (portable.split("/").some((part) => part === "..")) {
    throw new Error("Projected filesystem path cannot traverse above its root");
  }
  return portable;
}

function assertInside(root, target) {
  const relative = path.relative(root, target);
  if (relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative))) return;
  throw new Error("Projected filesystem target resolves outside its root");
}

function decodeUtf8Text(buffer) {
  if (buffer.includes(0)) return null;
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(buffer);
  } catch {
    return null;
  }
}

function sha256(buffer) {
  return createHash("sha256").update(buffer).digest("hex");
}

function requiredDigest(value) {
  const digest = requiredString(value, "expectedDigest").toLowerCase();
  if (!/^[a-f0-9]{64}$/.test(digest)) throw new Error("Manual file edit expected digest is invalid");
  return digest;
}

function matchesPreviewSignature(buffer, mimeType) {
  if (mimeType === "image/png") return startsWithBytes(buffer, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  if (mimeType === "image/jpeg") return startsWithBytes(buffer, [0xff, 0xd8, 0xff]);
  if (mimeType === "image/gif") return startsWithText(buffer, "GIF87a") || startsWithText(buffer, "GIF89a");
  if (mimeType === "image/webp") return startsWithText(buffer, "RIFF") && buffer.subarray(8, 12).toString("ascii") === "WEBP";
  if (mimeType === "image/bmp") return startsWithText(buffer, "BM");
  if (mimeType === "application/pdf") return startsWithText(buffer, "%PDF-");
  if (mimeType === "image/avif") {
    if (buffer.subarray(4, 8).toString("ascii") !== "ftyp") return false;
    const brands = buffer.subarray(8, Math.min(buffer.byteLength, 64)).toString("ascii");
    return brands.includes("avif") || brands.includes("avis");
  }
  return false;
}

function startsWithBytes(buffer, signature) {
  return signature.every((byte, index) => buffer[index] === byte);
}

function startsWithText(buffer, signature) {
  return buffer.subarray(0, signature.length).toString("ascii") === signature;
}

function fileKindRank(kind) {
  if (kind === "directory") return 0;
  if (kind === "file") return 1;
  return 2;
}

function portableRelative(root, target) {
  return path.relative(root, target).split(path.sep).join("/");
}

function joinPortable(parent, name) {
  return [parent, name].filter(Boolean).join("/");
}

function requiredId(value, label) {
  const text = requiredString(value, label);
  if (text.length > 512 || /[\u0000-\u001f]/.test(text)) throw new Error(`${label} is invalid`);
  return text;
}

function requiredString(value, label) {
  const text = stringValue(value);
  if (!text) throw new Error(`${label} is required`);
  return text;
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

module.exports = { createWorkerEnvironmentFileReader };
