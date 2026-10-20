module.exports = { commitClientWrites, fileExists, loadStoragePublisher, publishOperatorFiles, readFileHashIfPresent, resolveRegistryRoot, socketAcceptsConnections, syncDirectory, uniqueArtifactBindings, waitForInterfaceSocket, withOperatorRunAuthoringLock, withOperatorWorkspaceTopologyLock, withRunAuthoringLock };

const fs = require("node:fs/promises");
const net = require("node:net");
const path = require("node:path");
const { runAuthoringLocks, workspaceTopologyAuthoringLocks } = require("./constants.cjs");
const { runOperatorRevisionCommand } = require("./operatorTurns.cjs");
const { readConfig, resolveInstallationRoot, resolvePackageExecutable } = require("./roots.cjs");
const { interfaceSocketIdentity, interfaceSocketPath } = require("./topologyPlan.cjs");
const { absolutePath, isObject, requiredString, safeSegment, sha256Hex, stringValue } = require("./values.cjs");

async function withRunAuthoringLock(runId, operation) {
  const previous = runAuthoringLocks.get(runId) ?? Promise.resolve();
  let release;
  const current = new Promise((resolve) => { release = resolve; });
  const tail = previous.then(() => current);
  runAuthoringLocks.set(runId, tail);
  await previous;
  try {
    return await operation();
  } finally {
    release();
    if (runAuthoringLocks.get(runId) === tail) runAuthoringLocks.delete(runId);
  }
}

async function withOperatorRunAuthoringLock(runId, operation) {
  return withRunAuthoringLock(safeSegment(runId, "runId"), operation);
}

async function withOperatorWorkspaceTopologyLock(octopusHome, operation) {
  const key = absolutePath(octopusHome, "octopusHome");
  const previous = workspaceTopologyAuthoringLocks.get(key) ?? Promise.resolve();
  let release;
  const current = new Promise((resolve) => { release = resolve; });
  const tail = previous.then(() => current);
  workspaceTopologyAuthoringLocks.set(key, tail);
  await previous;
  try {
    return await operation();
  } finally {
    release();
    if (workspaceTopologyAuthoringLocks.get(key) === tail) {
      workspaceTopologyAuthoringLocks.delete(key);
    }
  }
}

async function commitClientWrites(writes, options) {
  const paths = new Set();
  for (const write of writes) {
    write.filePath = absolutePath(write.filePath, `${write.tag}.path`);
    if (typeof write.content !== "string" && write.content !== null) throw new Error(`${write.tag}.content must be text or null`);
    if (write.expectedBeforeHash !== null
      && !/^[0-9a-f]{64}$/.test(stringValue(write.expectedBeforeHash))) {
      throw new Error(`${write.tag}.expectedBeforeHash must be a lowercase SHA-256 hash or null`);
    }
    if (paths.has(write.filePath)) {
      throw new Error(`client authoring repeats path '${write.filePath}'`);
    }
    paths.add(write.filePath);
  }
  if (writes.length === 0) return [];
  const receipts = await publishOperatorFiles(options, writes);
  return receipts.map((receipt, index) => ({ path: writes[index].filePath, tag: writes[index].tag,
    beforeHash: receipt.before_hash, afterHash: receipt.after_hash, bytes: receipt.bytes,
    changed: receipt.before_hash !== receipt.after_hash }));
}

async function publishOperatorFiles(options, writes) {
  if (Array.isArray(writes) && writes.length === 0) return [];
  return (await loadStoragePublisher(options)).publishFiles(writes);
}

async function loadStoragePublisher(options) {
  const config = await readConfig();
  const planner = resolvePackageExecutable(
    config, resolveInstallationRoot(options), resolveRegistryRoot(options), "topology_planner",
  );
  return require("../publication.cjs").createStoragePublisher(planner, runOperatorRevisionCommand);
}

async function readFileHashIfPresent(filePath) {
  try {
    const bytes = await fs.readFile(filePath);
    return { bytes, hash: sha256Hex(bytes) };
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

async function syncDirectory(directory) {
  const handle = await fs.open(directory, "r");
  try {
    await handle.sync();
  } finally {
    await handle.close();
  }
}

async function waitForInterfaceSocket(octopusHome, interfaceId, timeoutMs, previousSocket = null) {
  const socketPath = interfaceSocketPath(octopusHome, interfaceId);
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (previousSocket === null) {
      if (await socketAcceptsConnections(socketPath)) return;
    } else {
      const current = await interfaceSocketIdentity(octopusHome, interfaceId);
      if (current !== null && current !== previousSocket && await socketAcceptsConnections(socketPath)
        && await interfaceSocketIdentity(octopusHome, interfaceId) === current) return;
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(
    `chat interface socket ${socketPath} did not ${previousSocket ? "replace its prior listener" : "appear"} within ${timeoutMs}ms; the activation reconciler lifts it from the chat folder's activation.json`,
  );
}

function socketAcceptsConnections(socketPath) {
  return new Promise((resolve) => {
    const socket = net.createConnection(socketPath);
    let settled = false;
    const finish = (ready) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      resolve(ready);
    };
    socket.once("connect", () => finish(true));
    socket.once("error", () => finish(false));
    socket.setTimeout(500, () => finish(false));
  });
}

async function fileExists(filePath) {
  try {
    await fs.access(filePath);
    return true;
  } catch {
    return false;
  }
}

function resolveRegistryRoot(options) {
  if (typeof options.registryRoot === "function") {
    return absolutePath(options.registryRoot(), "registryRoot");
  }
  return absolutePath(path.resolve(__dirname, "..", "..", ".."), "registryRoot");
}

function uniqueArtifactBindings(bindings) {
  const seen = new Set();
  const output = [];
  for (const binding of bindings) {
    if (!isObject(binding)) throw new Error("artifact binding must be an object");
    const unknownFields = Object.keys(binding).filter((key) => key !== "id");
    if (unknownFields.length > 0) {
      throw new Error(`artifact binding contains unknown fields: ${unknownFields.join(", ")}`);
    }
    const id = requiredString(binding.id, "artifact binding.id");
    if (seen.has(id)) continue;
    seen.add(id);
    output.push({ id });
  }
  return output;
}
