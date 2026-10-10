const crypto = require("node:crypto");
const fs = require("node:fs");
const fsPromises = require("node:fs/promises");
const path = require("node:path");

const MAX_DOCUMENTS = 128;
const MAX_DOCUMENT_BYTES = 640 * 1024;
const DOCUMENT_SUFFIX = ".surface.json";

function createClientSurfaceDocumentStore(options) {
  const root = path.resolve(requiredString(options?.root, "client surface root"));
  const onChange = typeof options?.onChange === "function" ? options.onChange : () => {};
  let generation = 0;
  let watcher = null;
  let debounceTimer = null;

  async function snapshot() {
    await ensureRoot();
    ensureWatcher();
    let entries;
    try {
      entries = await fsPromises.readdir(root, { withFileTypes: true });
    } catch (error) {
      return { generation, root, documents: [], errors: [formatError(error)] };
    }
    const candidates = entries
      .filter((entry) => entry.name.endsWith(DOCUMENT_SUFFIX))
      .sort((left, right) => left.name.localeCompare(right.name, undefined, { numeric: true }));
    if (candidates.length > MAX_DOCUMENTS) {
      return {
        generation,
        root,
        documents: [],
        errors: [`client surface root contains ${candidates.length} documents; maximum is ${MAX_DOCUMENTS}`],
      };
    }

    const documents = [];
    const errors = [];
    for (const entry of candidates) {
      const target = path.join(root, entry.name);
      try {
        if (!entry.isFile()) throw new Error("surface document must be a regular file");
        const stat = await fsPromises.lstat(target);
        if (!stat.isFile() || stat.isSymbolicLink()) throw new Error("surface document must not be a symlink");
        if (stat.size > MAX_DOCUMENT_BYTES) {
          throw new Error(`surface document is ${stat.size} bytes; maximum is ${MAX_DOCUMENT_BYTES}`);
        }
        const bytes = await fsPromises.readFile(target);
        documents.push({
          path: entry.name,
          digest: `sha256:${crypto.createHash("sha256").update(bytes).digest("hex")}`,
          bytes: bytes.byteLength,
          modifiedAt: stat.mtime.toISOString(),
          text: bytes.toString("utf8"),
        });
      } catch (error) {
        errors.push(`${entry.name}: ${formatError(error)}`);
      }
    }
    return { generation, root, documents, errors };
  }

  async function ensureRoot() {
    await fsPromises.mkdir(root, { recursive: true });
    const stat = await fsPromises.lstat(root);
    if (!stat.isDirectory() || stat.isSymbolicLink()) {
      throw new Error("client surface root must be a real directory");
    }
  }

  function ensureWatcher() {
    if (watcher) return;
    try {
      watcher = fs.watch(root, () => scheduleChange());
      watcher.on("error", () => closeWatcher());
    } catch {
      watcher = null;
    }
  }

  function scheduleChange() {
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      debounceTimer = null;
      generation += 1;
      onChange(generation);
    }, 80);
  }

  function closeWatcher() {
    if (watcher) {
      try { watcher.close(); } catch {}
      watcher = null;
    }
  }

  function close() {
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = null;
    closeWatcher();
  }

  return { root, snapshot, ensureWatcher, close, generation: () => generation };
}

function filterClientSurfaceSnapshot(snapshot, profile) {
  if (profile !== "live" && profile !== "lab") throw new Error(`Unknown client access profile: ${profile}`);
  if (profile === "live") return { ...snapshot, documents: [], errors: [] };
  const documents = snapshot.documents.filter((source) => {
    let document;
    try {
      document = JSON.parse(source.text);
    } catch {
      return true;
    }
    if (document?.release_channel === "stable") return true;
    if (document?.release_channel === "lab") return true;
    return true;
  });
  return { ...snapshot, documents };
}

function requiredString(value, name) {
  const text = typeof value === "string" ? value.trim() : "";
  if (!text) throw new Error(`${name} is required`);
  if (!path.isAbsolute(text)) throw new Error(`${name} must be absolute`);
  return text;
}

function formatError(error) {
  return error instanceof Error ? error.message : String(error);
}

module.exports = { createClientSurfaceDocumentStore, filterClientSurfaceSnapshot };
