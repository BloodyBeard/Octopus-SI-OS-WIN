const fs = require("node:fs");
const path = require("node:path");
const { createHash, randomBytes } = require("node:crypto");

const MAX_SELECTED_FILES = 12;
const MAX_SELECTED_FILE_BYTES = 100 * 1024 * 1024;
const MAX_SELECTED_TOTAL_BYTES = 512 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES = 2_000;
const MAX_DIRECTORY_TEXT_BYTES = 12 * 1024 * 1024;
const MAX_DIRECTORY_FILE_TEXT_BYTES = 1024 * 1024;
const MAX_STAGE_CHUNK_BYTES = 4 * 1024 * 1024;
const ATTACHMENT_STAGE_TTL_MS = 5 * 60 * 1000;

async function selectClientAttachments(dialog, browserWindow, request, staging) {
  const mode = ["directory", "files", "files_and_folders"].includes(request?.mode) ? request.mode : "files";
  const mixed = mode === "files_and_folders";
  const result = await dialog.showOpenDialog(browserWindow, {
    buttonLabel: mode === "directory" ? "Attach folder" : "Attach",
    message: mode === "directory" ? "Choose a folder to attach" : mixed ? "Choose files or folders to attach" : "Choose files to attach",
    properties: mode === "directory"
      ? ["openDirectory"]
      : mixed
        ? ["openFile", "openDirectory", "multiSelections"]
        : ["openFile", "multiSelections"],
  });
  if (result.canceled) return { canceled: true, attachments: [] };
  return stageClientAttachmentPaths(mode === "directory" ? result.filePaths.slice(0, 1) : result.filePaths, staging, mode);
}

async function stageClientAttachmentPaths(selectedPaths, staging, mode = "files_and_folders") {
  if (typeof staging?.stageFile !== "function" || typeof staging?.stageBytes !== "function" || typeof staging?.discard !== "function") {
    throw new Error("Attachment selection requires staging and cleanup writers.");
  }
  if (!Array.isArray(selectedPaths) || selectedPaths.some((selected) => typeof selected !== "string" || !path.isAbsolute(selected) || selected.includes("\0"))) {
    throw new Error("Attachment selection requires native absolute file paths.");
  }
  if (!["directory", "files", "files_and_folders"].includes(mode)) throw new Error("Unknown attachment selection mode.");
  if (selectedPaths.length === 0) return { canceled: true, attachments: [] };
  if (selectedPaths.length > MAX_SELECTED_FILES || mode === "directory" && selectedPaths.length !== 1) {
    throw new Error(`A task can include up to ${MAX_SELECTED_FILES} selected files.`);
  }
  const attachments = [];
  let totalBytes = 0;
  try {
    for (const selected of selectedPaths) {
      const stat = await fs.promises.stat(selected);
      if (stat.isDirectory()) {
        if (mode === "files") throw new Error(`Selected path is not a regular file: ${selected}`);
        const attachment = await packageDirectory(selected);
        totalBytes += attachment.bytes.byteLength;
        if (totalBytes > MAX_SELECTED_TOTAL_BYTES) throw new Error("Selected files and folders exceed 512 MB in total.");
        attachments.push(await staging.stageBytes({
          label: "operator.file",
          name: attachment.name,
          type: attachment.type,
          lastModified: attachment.lastModified,
        }, attachment.bytes));
        continue;
      }
      if (mode === "directory" || !stat.isFile()) throw new Error(`Selected path is not a regular file or directory: ${selected}`);
      if (stat.size > MAX_SELECTED_FILE_BYTES) throw new Error(`Selected file exceeds 100 MB: ${path.basename(selected)}`);
      totalBytes += stat.size;
      if (totalBytes > MAX_SELECTED_TOTAL_BYTES) throw new Error("Selected files exceed 512 MB in total.");
      attachments.push(await staging.stageFile({
        label: "operator.file",
        name: path.basename(selected),
        type: mediaTypeForName(selected),
      }, selected));
    }
  } catch (error) {
    const cleanup = await Promise.allSettled(attachments.map((handle) => staging.discard(handle)));
    const failures = cleanup.filter((result) => result.status === "rejected").map((result) => result.reason);
    if (failures.length) throw new AggregateError([error, ...failures], `${error.message}; attachment cleanup failed.`);
    throw error;
  }
  return { canceled: false, attachments };
}

async function packageDirectory(selected) {
  const root = await fs.promises.realpath(selected);
  const stat = await fs.promises.stat(root);
  if (!stat.isDirectory()) throw new Error(`Selected path is not a directory: ${selected}`);
  const entries = [];
  let embeddedBytes = 0;
  await walkDirectory(root, "", entries, () => embeddedBytes, (value) => { embeddedBytes = value; });
  const payload = Buffer.from(JSON.stringify({
    schema: "octopus.client.directory_attachment",
    schema_version: 1,
    name: path.basename(root),
    entries,
  }, null, 2), "utf8");
  if (payload.byteLength > MAX_SELECTED_FILE_BYTES) throw new Error("Directory attachment manifest exceeds 100 MB.");
  return {
    bytes: payload,
    lastModified: Math.trunc(stat.mtimeMs),
    name: `${safeBaseName(path.basename(root))}.directory.json`,
    type: "application/json",
  };
}

async function walkDirectory(root, relativeDirectory, entries, readEmbeddedBytes, writeEmbeddedBytes) {
  const directoryPath = path.join(root, relativeDirectory);
  const children = await fs.promises.readdir(directoryPath, { withFileTypes: true });
  children.sort((left, right) => left.name.localeCompare(right.name));
  for (const child of children) {
    if (entries.length >= MAX_DIRECTORY_ENTRIES) throw new Error(`Directory contains more than ${MAX_DIRECTORY_ENTRIES.toLocaleString()} entries.`);
    const relativePath = path.posix.join(relativeDirectory.split(path.sep).join(path.posix.sep), child.name);
    const absolutePath = path.join(root, relativeDirectory, child.name);
    if (child.isSymbolicLink()) {
      entries.push({ kind: "symlink", path: relativePath, target: await fs.promises.readlink(absolutePath) });
      continue;
    }
    if (child.isDirectory()) {
      entries.push({ kind: "directory", path: relativePath });
      await walkDirectory(root, path.join(relativeDirectory, child.name), entries, readEmbeddedBytes, writeEmbeddedBytes);
      continue;
    }
    if (!child.isFile()) {
      entries.push({ kind: "other", path: relativePath });
      continue;
    }
    const stat = await fs.promises.stat(absolutePath);
    const record = { kind: "file", path: relativePath, size: stat.size };
    if (stat.size > MAX_SELECTED_FILE_BYTES) {
      entries.push({ ...record, content: { included: false, reason: "file_exceeds_100_mb" } });
      continue;
    }
    const bytes = await fs.promises.readFile(absolutePath);
    const hash = createHash("sha256").update(bytes).digest("hex");
    if (!textFileName(child.name)) {
      entries.push({ ...record, sha256: hash, content: { included: false, reason: "binary_or_unsupported_text" } });
      continue;
    }
    if (bytes.byteLength > MAX_DIRECTORY_FILE_TEXT_BYTES) {
      entries.push({ ...record, sha256: hash, content: { included: false, reason: "text_file_exceeds_1_mb" } });
      continue;
    }
    const nextEmbeddedBytes = readEmbeddedBytes() + bytes.byteLength;
    if (nextEmbeddedBytes > MAX_DIRECTORY_TEXT_BYTES) {
      entries.push({ ...record, sha256: hash, content: { included: false, reason: "directory_text_budget_exhausted" } });
      continue;
    }
    writeEmbeddedBytes(nextEmbeddedBytes);
    entries.push({ ...record, sha256: hash, content: { included: true, text: bytes.toString("utf8") } });
  }
}

function textFileName(name) {
  return /(^|\.)(txt|md|mdx|json|jsonl|csv|tsv|log|rs|toml|lock|ts|tsx|js|jsx|mjs|cjs|css|scss|html|xml|yaml|yml|py|rb|go|java|kt|kts|swift|c|cc|cpp|h|hpp|cs|sh|zsh|bash|fish|sql|graphql|gql|proto|ini|cfg|conf|env|gitignore|dockerfile)$/i.test(name)
    || /^(Dockerfile|Makefile|Justfile|Procfile|LICENSE|README)$/i.test(name);
}

function mediaTypeForName(name) {
  const extension = path.extname(name).toLowerCase();
  if ([".png", ".apng"].includes(extension)) return "image/png";
  if ([".jpg", ".jpeg"].includes(extension)) return "image/jpeg";
  if (extension === ".gif") return "image/gif";
  if (extension === ".webp") return "image/webp";
  if (extension === ".svg") return "image/svg+xml";
  if (extension === ".pdf") return "application/pdf";
  if (extension === ".json") return "application/json";
  if (textFileName(name)) return "text/plain";
  return "application/octet-stream";
}

function safeBaseName(value) {
  return value.replace(/[\\/:*?"<>|\u0000-\u001f]/g, "-").trim() || "folder";
}

function createClientAttachmentStagingStore({ root }) {
  if (typeof root !== "function") throw new Error("attachment staging requires a root resolver");
  const entries = new Map();
  const ready = new Map();

  async function begin(owner, request) {
    sweepExpired();
    const ownerId = requiredString(owner, "attachment stage owner");
    const name = boundedString(request?.name, "attachment.name", 255);
    const label = boundedString(request?.label, "attachment.label", 160);
    const type = boundedString(request?.type || "application/octet-stream", "attachment.type", 160);
    const size = boundedSize(request?.size, "attachment.size");
    const lastModified = nonNegativeInteger(request?.lastModified, "attachment.lastModified");
    if (size > MAX_SELECTED_FILE_BYTES) throw new Error(`Attachment '${name}' exceeds ${MAX_SELECTED_FILE_BYTES} bytes.`);
    const stageRoot = path.join(path.resolve(root()), "attachment-staging");
    await fs.promises.mkdir(stageRoot, { recursive: true, mode: 0o700 });
    await fs.promises.chmod(stageRoot, 0o700);
    const id = randomBytes(24).toString("base64url");
    const partialPath = path.join(stageRoot, `${id}.partial`);
    const handle = await fs.promises.open(partialPath, "wx", 0o600);
    entries.set(id, {
      bytes: 0,
      acceptedBytes: 0,
      failure: null,
      expiresAt: Date.now() + ATTACHMENT_STAGE_TTL_MS,
      handle,
      hash: createHash("sha256"),
      label,
      lastModified,
      name,
      ownerId,
      partialPath,
      size,
      type,
      closing: false,
      queue: Promise.resolve(),
    });
    return { id, label, lastModified, name, size, type };
  }

  async function append(owner, request) {
    sweepExpired();
    const entry = owned(entries, owner, request?.id);
    if (entry.closing) throw new Error("Attachment stage is already closing.");
    if (entry.failure) throw entry.failure;
    const input = ipcBytes(request?.bytes);
    if (input.byteLength > MAX_STAGE_CHUNK_BYTES) throw new Error(`Attachment stage chunk exceeds ${MAX_STAGE_CHUNK_BYTES} bytes.`);
    if (entry.acceptedBytes + input.byteLength > entry.size) {
      entry.failure = new Error("Attachment stage exceeds its declared size.");
      throw entry.failure;
    }
    entry.acceptedBytes += input.byteLength;
    const bytes = Buffer.from(input);
    entry.expiresAt = Date.now() + ATTACHMENT_STAGE_TTL_MS;
    entry.queue = entry.queue.then(async () => {
      await entry.handle.writeFile(bytes);
      entry.hash.update(bytes);
      entry.bytes += bytes.byteLength;
      entry.expiresAt = Date.now() + ATTACHMENT_STAGE_TTL_MS;
      return { bytes: entry.bytes, id: request.id };
    });
    return await entry.queue;
  }

  async function stageFile(owner, request, sourcePath) {
    const source = path.resolve(requiredString(sourcePath, "attachment source path"));
    const initial = await fs.promises.lstat(source);
    if (initial.isSymbolicLink() || !initial.isFile()) throw new Error(`Selected path is not a regular file: ${source}`);
    const expected = {
      ...request,
      lastModified: Math.trunc(initial.mtimeMs),
      size: initial.size,
    };
    const stage = await begin(owner, expected);
    let handle;
    try {
      handle = await fs.promises.open(source, "r");
      let offset = 0;
      while (offset < initial.size) {
        const buffer = Buffer.allocUnsafe(Math.min(MAX_STAGE_CHUNK_BYTES, initial.size - offset));
        const { bytesRead } = await handle.read(buffer, 0, buffer.byteLength, offset);
        if (bytesRead !== buffer.byteLength) throw new Error(`Selected file changed while staging: ${path.basename(source)}`);
        await append(owner, { id: stage.id, bytes: buffer });
        offset += bytesRead;
      }
      const current = await fs.promises.lstat(source);
      if (current.isSymbolicLink() || !current.isFile() || current.dev !== initial.dev || current.ino !== initial.ino || current.size !== initial.size || current.mtimeMs !== initial.mtimeMs) {
        throw new Error(`Selected file changed while staging: ${path.basename(source)}`);
      }
      return finalize(owner, { id: stage.id });
    } catch (error) {
      await abort(owner, { id: stage.id }).catch(() => {});
      throw error;
    } finally {
      await handle?.close();
    }
  }

  async function stageBytes(owner, request, bytes) {
    if (!Buffer.isBuffer(bytes)) throw new Error("Attachment stage bytes must be a Buffer.");
    const stage = await begin(owner, { ...request, size: bytes.byteLength });
    try {
      for (let offset = 0; offset < bytes.byteLength; offset += MAX_STAGE_CHUNK_BYTES) {
        await append(owner, { id: stage.id, bytes: bytes.subarray(offset, Math.min(bytes.byteLength, offset + MAX_STAGE_CHUNK_BYTES)) });
      }
      return finalize(owner, { id: stage.id });
    } catch (error) {
      await abort(owner, { id: stage.id }).catch(() => {});
      throw error;
    }
  }

  async function finalize(owner, request) {
    sweepExpired();
    const entry = owned(entries, owner, request?.id);
    if (entry.closing) throw new Error("Attachment stage is already closing.");
    entry.closing = true;
    entry.finalization = sealStage(entry, request.id);
    return entry.finalization;
  }

  async function sealStage(entry, id) {
    try {
      await entry.queue;
      if (entry.failure) throw entry.failure;
      if (entry.bytes !== entry.size) throw new Error(`Attachment stage is ${entry.bytes} bytes; expected ${entry.size}.`);
    } catch (error) {
      entry.closing = false;
      throw error;
    }
    try {
      await entry.handle.sync();
      await entry.handle.close();
      const digest = entry.hash.digest("hex");
      const readyPath = `${entry.partialPath.slice(0, -".partial".length)}.ready`;
      await fs.promises.rename(entry.partialPath, readyPath);
      const staged = {
        contentHash: digest,
        id,
        label: entry.label,
        lastModified: entry.lastModified,
        name: entry.name,
        payloadPath: readyPath,
        size: entry.size,
        type: entry.type,
      };
      ready.set(id, { ...staged, expiresAt: Date.now() + ATTACHMENT_STAGE_TTL_MS, ownerId: entry.ownerId });
      return {
        contentHash: digest,
        id,
        label: entry.label,
        lastModified: entry.lastModified,
        name: entry.name,
        size: entry.size,
        type: entry.type,
      };
    } catch (error) {
      await entry.handle.close().catch(() => {});
      await fs.promises.unlink(entry.partialPath).catch(() => {});
      throw error;
    } finally {
      entries.delete(id);
    }
  }

  async function abort(owner, request) {
    const entry = owned(entries, owner, request?.id);
    if (entry.closing) throw new Error("Attachment stage is already closing.");
    entry.closing = true;
    entries.delete(request.id);
    await entry.queue.catch(() => {});
    await entry.handle.close().catch(() => {});
    await fs.promises.unlink(entry.partialPath).catch((error) => {
      if (error?.code !== "ENOENT") throw error;
    });
    return { aborted: true, id: request.id };
  }

  async function abortOwner(owner) {
    const ownerId = requiredString(owner, "attachment stage owner");
    await cleanupStages((entry) => entry.ownerId === ownerId);
  }

  async function abortAll() {
    await cleanupStages(() => true);
  }

  async function cleanupStages(matches) {
    const pending = [...entries.entries()].filter(([, entry]) => matches(entry));
    const outcomes = await Promise.allSettled(pending.map(async ([id, entry]) => {
      if (entry.finalization) await entry.finalization.catch(() => {});
      if (entries.get(id) === entry) await abort(entry.ownerId, { id });
    }));
    const sealed = [...ready.entries()].filter(([, entry]) => matches(entry));
    outcomes.push(...await Promise.allSettled(sealed.map(async ([id, entry]) => {
      ready.delete(id);
      await fs.promises.unlink(entry.payloadPath).catch((error) => {
        if (error?.code !== "ENOENT") throw error;
      });
    })));
    const failures = outcomes.filter((result) => result.status === "rejected").map((result) => result.reason);
    if (failures.length) throw new AggregateError(failures, "Attachment staging cleanup failed.");
  }

  async function consume(owner, input) {
    const id = requiredString(input?.id, "attachment stage id");
    sweepExpired();
    const entry = ready.get(id);
    if (!entry || entry.ownerId !== requiredString(owner, "attachment stage owner")) throw new Error("Attachment stage is unavailable.");
    ready.delete(id);
    return {
      contentHash: entry.contentHash,
      label: entry.label,
      lastModified: entry.lastModified,
      name: entry.name,
      payloadPath: entry.payloadPath,
      size: entry.size,
      type: entry.type,
    };
  }

  async function discard(input) {
    const staged = input && typeof input === "object" ? input : {};
    const payloadPath = requiredString(staged.payloadPath, "staged attachment payloadPath");
    await fs.promises.unlink(payloadPath).catch((error) => {
      if (error?.code !== "ENOENT") throw error;
    });
  }

  function sweepExpired() {
    const expired = [...entries.entries()].filter(([, entry]) => !entry.closing && entry.expiresAt <= Date.now());
    for (const [id, entry] of expired) {
      entry.closing = true;
      entries.delete(id);
      void entry.queue.catch(() => {}).then(() => entry.handle.close().catch(() => {})).then(() => fs.promises.unlink(entry.partialPath).catch(() => {}));
    }
    for (const [id, entry] of ready) {
      if (entry.expiresAt > Date.now()) continue;
      ready.delete(id);
      void fs.promises.unlink(entry.payloadPath).catch(() => {});
    }
  }

  return { abort, abortAll, abortOwner, append, begin, consume, discard, finalize, stageBytes, stageFile, sweepExpired };
}

function ipcBytes(value) {
  if (Buffer.isBuffer(value)) return value;
  if (value instanceof ArrayBuffer) return Buffer.from(value);
  if (ArrayBuffer.isView(value)) return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  throw new Error("attachment stage bytes must be an ArrayBuffer");
}

function owned(entries, owner, id) {
  const entry = entries.get(requiredString(id, "attachment stage id"));
  if (!entry || entry.ownerId !== requiredString(owner, "attachment stage owner")) throw new Error("Attachment stage is unavailable.");
  return entry;
}

function boundedSize(value, field) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${field} must be a non-negative integer.`);
  return value;
}

function nonNegativeInteger(value, field) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${field} must be a non-negative integer.`);
  return value;
}

function boundedString(value, field, maximum) {
  const output = typeof value === "string" ? value.trim() : "";
  if (!output) throw new Error(`${field} is required.`);
  if (output.length > maximum) throw new Error(`${field} exceeds ${maximum} characters.`);
  return output;
}

function requiredString(value, field) {
  const output = typeof value === "string" ? value.trim() : "";
  if (!output) throw new Error(`${field} is required.`);
  return output;
}

module.exports = { createClientAttachmentStagingStore, selectClientAttachments, stageClientAttachmentPaths };
