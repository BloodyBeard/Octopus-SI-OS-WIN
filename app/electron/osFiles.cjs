const fs = require("node:fs/promises");
const path = require("node:path");
const { createHash, randomUUID } = require("node:crypto");

const MAX_DIRECTORY_ENTRIES = 2_000;
const MAX_TEXT_BYTES = 2 * 1024 * 1024;
const MAX_RELATIVE_PATH_BYTES = 4_096;

function createOsFileStore(options) {
  if (typeof options?.resolveRoots !== "function") {
    throw new Error("OS file store requires an explicit root resolver");
  }

  async function roots() {
    const resolved = await resolveRoots(options);
    return {
      roots: resolved.map(({ id, label, root }) => ({ id, label, path: root })),
    };
  }

  async function list(request) {
    const descriptor = await resolveRoot(options, request?.rootId);
    const location = await resolveInsideRoot(descriptor.root, request?.path);
    const stat = await fs.lstat(location.target);
    if (stat.isSymbolicLink() || !stat.isDirectory()) {
      throw new Error("OS Files target is not a readable directory");
    }
    const rows = await fs.readdir(location.target, { withFileTypes: true });
    if (rows.length > MAX_DIRECTORY_ENTRIES) {
      throw new Error(`Directory has ${rows.length} entries; OS Files limit is ${MAX_DIRECTORY_ENTRIES}`);
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
      rootId: descriptor.id,
      path: location.relativePath,
      entries,
    };
  }

  async function read(request) {
    const descriptor = await resolveRoot(options, request?.rootId);
    const location = await resolveInsideRoot(descriptor.root, request?.path);
    const stat = await fs.lstat(location.target);
    if (stat.isSymbolicLink() || !stat.isFile()) {
      throw new Error("OS Files target is not a readable file");
    }
    const handle = await fs.open(location.target, "r");
    try {
      const bytesToRead = Math.min(stat.size, MAX_TEXT_BYTES);
      const buffer = Buffer.alloc(bytesToRead);
      const { bytesRead } = await handle.read(buffer, 0, bytesToRead, 0);
      const content = buffer.subarray(0, bytesRead);
      const text = decodeUtf8Text(content);
      return {
        rootId: descriptor.id,
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

  async function write(request) {
    const descriptor = await resolveRoot(options, request?.rootId);
    const text = typeof request?.text === "string" ? request.text : null;
    if (text == null) throw new Error("OS Files save requires text");
    const bytes = Buffer.from(text, "utf8");
    if (bytes.byteLength > MAX_TEXT_BYTES) {
      throw new Error(`OS Files save exceeds ${MAX_TEXT_BYTES / (1024 * 1024)} MB`);
    }
    const expectedDigest = requiredDigest(request?.expectedDigest);
    const expectedModifiedAt = requiredString(request?.expectedModifiedAt, "expectedModifiedAt");
    const expectedSize = Number(request?.expectedSize);
    if (!Number.isSafeInteger(expectedSize) || expectedSize < 0 || expectedSize > MAX_TEXT_BYTES) {
      throw new Error("OS Files expected size is invalid");
    }
    const location = await resolveInsideRoot(descriptor.root, request?.path);
    const stat = await fs.lstat(location.target);
    if (stat.isSymbolicLink() || !stat.isFile()) throw new Error("OS Files target is not a writable file");
    const current = await fs.readFile(location.target);
    if (current.includes(0) || decodeUtf8Text(current) == null) throw new Error("Binary files cannot be edited as text");
    if (stat.size !== expectedSize || stat.mtime.toISOString() !== expectedModifiedAt || sha256(current) !== expectedDigest) {
      throw new Error("File changed after it was opened; reload before saving");
    }
    const temporary = path.join(path.dirname(location.target), `.${path.basename(location.target)}.${process.pid}.${randomUUID()}.tmp`);
    try {
      await fs.writeFile(temporary, bytes, { flag: "wx", mode: stat.mode & 0o777 });
      const latestStat = await fs.lstat(location.target);
      const latest = await fs.readFile(location.target);
      if (
        latestStat.isSymbolicLink()
        || !latestStat.isFile()
        || latestStat.dev !== stat.dev
        || latestStat.ino !== stat.ino
        || latestStat.size !== expectedSize
        || latestStat.mtime.toISOString() !== expectedModifiedAt
        || sha256(latest) !== expectedDigest
      ) {
        throw new Error("File changed while the replacement was being prepared; reload before saving");
      }
      await fs.rename(temporary, location.target);
    } finally {
      await fs.rm(temporary, { force: true }).catch(() => {});
    }
    const after = await fs.lstat(location.target);
    const target = await fs.realpath(location.target);
    assertInside(location.root, target);
    return {
      rootId: descriptor.id,
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

  async function resolvePath(request) {
    const descriptor = await resolveRoot(options, request?.rootId);
    const location = await resolveInsideRoot(descriptor.root, request?.path);
    const stat = await fs.lstat(location.target);
    if (stat.isSymbolicLink() || (!stat.isFile() && !stat.isDirectory())) {
      throw new Error("OS Files target is not a regular file or directory");
    }
    return {
      rootId: descriptor.id,
      path: location.relativePath,
      absolutePath: location.target,
      kind: stat.isDirectory() ? "directory" : "file",
    };
  }

  return { list, read, resolvePath, roots, write };
}

async function resolveRoots(options) {
  const candidates = await options.resolveRoots();
  if (!Array.isArray(candidates)) throw new Error("OS file roots are unavailable");
  const seenIds = new Set();
  const seenPaths = new Set();
  const roots = [];
  for (const candidate of candidates) {
    const id = requiredId(candidate?.id, "root id");
    const label = requiredString(candidate?.label, "root label");
    const rootValue = requiredString(candidate?.root, "root path");
    if (!path.isAbsolute(rootValue)) throw new Error(`OS file root ${id} must be absolute`);
    const stat = await fs.lstat(rootValue);
    if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error(`OS file root ${id} is not a regular directory`);
    const root = await fs.realpath(rootValue);
    if (seenIds.has(id)) throw new Error(`OS file root id is duplicated: ${id}`);
    seenIds.add(id);
    if (seenPaths.has(root)) continue;
    seenPaths.add(root);
    roots.push({ id, label, root });
  }
  if (!roots.length) throw new Error("OS Files has no admitted roots");
  return roots;
}

async function resolveRoot(options, rootIdValue) {
  const rootId = requiredId(rootIdValue, "rootId");
  const roots = await resolveRoots(options);
  const descriptor = roots.find((candidate) => candidate.id === rootId);
  if (!descriptor) throw new Error(`OS file root is not admitted: ${rootId}`);
  return descriptor;
}

async function resolveInsideRoot(rootValue, pathValue) {
  const relativePath = safeRelativePath(pathValue);
  const root = await fs.realpath(rootValue);
  const candidate = path.resolve(root, relativePath || ".");
  assertInside(root, candidate);
  const target = await fs.realpath(candidate);
  assertInside(root, target);
  return { root, target, relativePath: portableRelative(root, target) };
}

function safeRelativePath(value) {
  const text = typeof value === "string" ? value.trim() : "";
  if (Buffer.byteLength(text) > MAX_RELATIVE_PATH_BYTES || text.includes("\0") || path.isAbsolute(text)) {
    throw new Error("OS file path must be a bounded relative path");
  }
  const portable = text.replaceAll("\\", "/");
  if (portable.split("/").some((part) => part === "..")) {
    throw new Error("OS file path cannot traverse above its root");
  }
  return portable;
}

function assertInside(root, target) {
  const relative = path.relative(root, target);
  if (relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative))) return;
  throw new Error("OS file target resolves outside its root");
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
  if (!/^[a-f0-9]{64}$/.test(digest)) throw new Error("OS Files expected digest is invalid");
  return digest;
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
  if (text.length > 128 || !/^[a-z0-9][a-z0-9_-]*$/.test(text)) throw new Error(`${label} is invalid`);
  return text;
}

function requiredString(value, label) {
  const text = typeof value === "string" ? value.trim() : "";
  if (!text) throw new Error(`${label} is required`);
  return text;
}

module.exports = { createOsFileStore };
