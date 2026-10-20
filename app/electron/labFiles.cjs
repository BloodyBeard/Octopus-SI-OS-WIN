const { createHash } = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { createOsFileStore } = require("./osFiles.cjs");

const LAB_ROOT_ID = "octopus-home";
const LAB_ROOT_PATH = "lab://home";
const REGISTRY_DIRECTORY = "registry";
const WORKSPACE_DIRECTORY = "workspace";

function createLabFileStore(options) {
  if (typeof options?.registryRoot !== "function") throw new Error("Test files require the bundled registry root");
  if (typeof options?.workspaceFiles !== "function") throw new Error("Test files require the lab workspace");
  const registry = createOsFileStore({
    resolveRoots: () => [{ id: "registry", label: "Registry", root: options.registryRoot() }],
  });
  const openedAt = new Date().toISOString();
  const workspace = new Map(Object.entries(options.workspaceFiles()).map(([path, text]) => [path, { text, modifiedAt: openedAt }]));

  function roots() {
    return { roots: [{ id: LAB_ROOT_ID, label: "Test Home", path: LAB_ROOT_PATH }] };
  }

  async function list(request) {
    const location = locate(request);
    if (location.area === "") {
      return {
        rootId: LAB_ROOT_ID,
        path: "",
        entries: [
          { name: REGISTRY_DIRECTORY, path: REGISTRY_DIRECTORY, kind: "directory", size: null, modifiedAt: openedAt },
          { name: WORKSPACE_DIRECTORY, path: WORKSPACE_DIRECTORY, kind: "directory", size: null, modifiedAt: openedAt },
        ],
      };
    }
    if (location.area === REGISTRY_DIRECTORY) {
      const descriptor = labInterfaceDescriptor(location.inner);
      if (descriptor?.directory) {
        return {
          rootId: LAB_ROOT_ID,
          path: location.path,
          entries: [{ name: "interface.json", path: `${location.path}/interface.json`, kind: "file", size: Buffer.byteLength(descriptor.text), modifiedAt: openedAt }],
        };
      }
      const result = await registry.list({ rootId: "registry", path: location.inner }).catch(missingAsUnreadable);
      return {
        rootId: LAB_ROOT_ID,
        path: prefixed(REGISTRY_DIRECTORY, result.path),
        entries: result.entries.map((entry) => ({ ...entry, path: prefixed(REGISTRY_DIRECTORY, entry.path) })),
      };
    }
    return { rootId: LAB_ROOT_ID, path: location.path, entries: workspaceEntries(location.inner) };
  }

  async function read(request) {
    const location = locate(request);
    if (location.area === REGISTRY_DIRECTORY) {
      const descriptor = labInterfaceDescriptor(location.inner);
      if (descriptor && !descriptor.directory) return workspaceDocument(location.path, { text: descriptor.text, modifiedAt: openedAt });
      const result = await registry.read({ rootId: "registry", path: location.inner }).catch(missingAsUnreadable);
      return { ...result, rootId: LAB_ROOT_ID, path: prefixed(REGISTRY_DIRECTORY, result.path) };
    }
    const file = workspace.get(location.inner);
    if (!file) throw new Error("OS Files target is not a readable file");
    return workspaceDocument(location.path, file);
  }

  async function write(request) {
    const location = locate(request);
    if (location.area !== WORKSPACE_DIRECTORY) throw new Error("The Test registry is the bundled package library and cannot be edited.");
    const file = workspace.get(location.inner);
    if (!file) throw new Error("OS Files target is not a writable file");
    const text = typeof request?.text === "string" ? request.text : null;
    if (text == null) throw new Error("OS Files save requires text");
    const current = workspaceDocument(location.path, file);
    if (request?.expectedDigest !== current.digest || request?.expectedModifiedAt !== current.modifiedAt || Number(request?.expectedSize) !== current.size) {
      throw new Error("File changed after it was opened; reload before saving");
    }
    const next = { text, modifiedAt: new Date().toISOString() };
    workspace.set(location.inner, next);
    return { ...workspaceDocument(location.path, next), outsideOctopusEvidence: true };
  }

  function labInterfaceDescriptor(inner) {
    const match = /^interface\/([a-z0-9_]+)(\/interface\.json)?$/.exec(inner);
    if (!match || typeof options.interfaceDescriptor !== "function") return null;
    if (fs.existsSync(path.join(options.registryRoot(), "interface", match[1]))) return null;
    const descriptor = options.interfaceDescriptor(match[1]);
    if (!descriptor) return null;
    return { directory: !match[2], text: `${JSON.stringify(descriptor, null, 2)}\n` };
  }

  async function resolvePath() {
    throw new Error("Test files live only inside the isolated lab; there is nothing to open on this Mac.");
  }

  function workspaceEntries(directory) {
    const prefix = directory ? `${directory}/` : "";
    if (directory && ![...workspace.keys()].some((path) => path.startsWith(prefix))) {
      throw new Error("OS Files target is not a readable directory");
    }
    const entries = new Map();
    for (const [path, file] of workspace) {
      if (!path.startsWith(prefix)) continue;
      const rest = path.slice(prefix.length);
      const [name, ...deeper] = rest.split("/");
      if (entries.has(name)) continue;
      entries.set(name, deeper.length
        ? { name, path: prefixed(WORKSPACE_DIRECTORY, `${prefix}${name}`), kind: "directory", size: null, modifiedAt: openedAt }
        : { name, path: prefixed(WORKSPACE_DIRECTORY, path), kind: "file", size: Buffer.byteLength(file.text), modifiedAt: file.modifiedAt });
    }
    return [...entries.values()].sort((left, right) => (
      Number(left.kind !== "directory") - Number(right.kind !== "directory")
      || left.name.localeCompare(right.name, undefined, { numeric: true, sensitivity: "base" })
    ));
  }

  return { list, read, resolvePath, roots, write };
}

function locate(request) {
  if (request?.rootId !== LAB_ROOT_ID) throw new Error(`OS file root is not admitted: ${String(request?.rootId || "")}`);
  const raw = typeof request?.path === "string" ? request.path.trim().replaceAll("\\", "/").replace(/^\/+|\/+$/g, "") : "";
  const parts = raw ? raw.split("/") : [];
  if (parts.some((part) => !part || part === "." || part === "..")) throw new Error("OS file path must be a bounded relative path");
  const [area = "", ...inner] = parts;
  if (area && area !== REGISTRY_DIRECTORY && area !== WORKSPACE_DIRECTORY) throw new Error("OS Files target is not readable in Test");
  return { area, inner: inner.join("/"), path: parts.join("/") };
}

function missingAsUnreadable(error) {
  if (error?.code === "ENOENT") throw new Error("OS Files target does not exist in the Test registry");
  throw error;
}

function prefixed(area, inner) {
  return inner ? `${area}/${inner}` : area;
}

function workspaceDocument(path, file) {
  const bytes = Buffer.from(file.text, "utf8");
  return {
    rootId: LAB_ROOT_ID,
    path,
    name: path.split("/").at(-1),
    size: bytes.byteLength,
    modifiedAt: file.modifiedAt,
    kind: "text",
    text: file.text,
    truncated: false,
    digest: createHash("sha256").update(bytes).digest("hex"),
  };
}

module.exports = { createLabFileStore };
