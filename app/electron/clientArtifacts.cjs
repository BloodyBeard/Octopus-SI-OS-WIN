const crypto = require("node:crypto");
const { constants } = require("node:fs");
const fs = require("node:fs/promises");
const path = require("node:path");
const { projectionReadRequest } = require("./projectionReadContract.cjs");

const MAX_PAYLOAD_BYTES = 8 * 1024 * 1024;
const MAX_STREAM_CHUNK_BYTES = 4 * 1024 * 1024;
const MAX_STREAM_BYTES = 2 * 1024 * 1024 * 1024;

async function submitClientInterfaceArtifact(options, request) {
  const interfaceId = requiredSegment(request?.interfaceId, "interfaceId");
  const runId = optionalString(request?.runId);
  const label = requiredString(request?.label, "label");
  const payload = requiredObject(request?.payload, "payload");
  const timeoutMs = positiveNumber(request?.timeoutMs, 45_000);
  await authorizeSubmission(options, { interfaceId, runId, label, payload });
  const artifactRoot = resolveClientArtifactsRoot(options, interfaceId);
  const target = await resolveReadyTarget(options, interfaceId);
  const transferPath = requiredString(target.transfer_path, "artifact target transfer_path");
  const bytes = Buffer.from(JSON.stringify(payload));
  if (bytes.byteLength > MAX_PAYLOAD_BYTES) {
    throw new Error(`Artifact payload is ${bytes.byteLength} bytes; maximum is ${MAX_PAYLOAD_BYTES}.`);
  }
  const digest = crypto.createHash("sha256").update(bytes).digest("hex");
  const payloadRoot = path.join(artifactRoot, "interface-artifacts");
  await fs.mkdir(payloadRoot, { recursive: true, mode: 0o700 });
  await fs.chmod(payloadRoot, 0o700);
  const payloadPath = path.join(payloadRoot, `${safeFilename(label)}-${digest}.json`);
  await writeContentAddressedBytes(payloadPath, bytes, digest);
  const verdict = await requiredFunction(options?.handInArtifact, "handInArtifact")({
    endpoint: transferPath.startsWith("/") ? `unix://${transferPath}` : transferPath,
    interfaceId,
    manifest: { label, payload_path: payloadPath },
    timeoutMs,
  });
  const response = { interfaceId, ...(runId ? { runId } : {}), label, contentHash: digest, verdict };
  if (request?.reportSpentGeneration === true && verdict?.refused?.reason === "checkpoint_spent") {
    return { ...response, spentGeneration: true };
  }
  assertAdmitted(verdict, label);
  return response;
}

async function writeContentAddressedBytes(filePath, bytes, expectedDigest) {
  const temporary = `${filePath}.${crypto.randomBytes(16).toString("hex")}.partial`;
  try {
    const handle = await fs.open(temporary, "wx", 0o600);
    try {
      await handle.writeFile(bytes);
      await handle.sync();
    } finally {
      await handle.close();
    }
    await publishContentAddressedFile(temporary, filePath, expectedDigest, bytes.byteLength);
  } finally {
    await fs.unlink(temporary).catch((error) => {
      if (error?.code !== "ENOENT") throw error;
    });
  }
}

async function publishContentAddressedFile(source, destination, expectedDigest, expectedSize) {
  try {
    await fs.link(source, destination);
  } catch (error) {
    if (error?.code !== "EEXIST") throw error;
  }
  const handle = await fs.open(destination, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const info = await handle.stat();
    if (!info.isFile() || info.size !== expectedSize) {
      throw new Error(`Content-addressed artifact '${destination}' does not match its declared size.`);
    }
    const hash = crypto.createHash("sha256");
    for await (const chunk of handle.createReadStream({ autoClose: false })) hash.update(chunk);
    if (hash.digest("hex") !== expectedDigest) {
      throw new Error(`Content-addressed artifact '${destination}' does not match its filename digest.`);
    }
  } finally {
    await handle.close();
  }
  const directory = await fs.open(path.dirname(destination), "r");
  try {
    await directory.sync();
  } finally {
    await directory.close();
  }
}

function createClientArtifactStreamStore(options) {
  const streams = new Map();

  async function begin(owner, request) {
    const ownerId = requiredString(owner, "stream owner");
    const interfaceId = requiredSegment(request?.interfaceId, "interfaceId");
    const runId = optionalString(request?.runId);
    const label = requiredString(request?.label, "label");
    const mediaType = requiredMediaType(request?.mediaType);
    const extension = safeExtension(request?.extension);
    const payload = request?.payload == null ? {} : requiredObject(request.payload, "payload");
    await authorizeSubmission(options, { interfaceId, runId, label, payload });
    await resolveReadyTarget(options, interfaceId);
    const artifactRoot = resolveClientArtifactsRoot(options, interfaceId);
    const streamRoot = path.join(artifactRoot, "interface-artifacts", "streams");
    await fs.mkdir(streamRoot, { recursive: true, mode: 0o700 });
    await fs.chmod(streamRoot, 0o700);
    const streamId = crypto.randomBytes(24).toString("base64url");
    const partialPath = path.join(streamRoot, `${streamId}.partial`);
    const handle = await fs.open(partialPath, "wx", 0o600);
    streams.set(streamId, {
      ownerId,
      interfaceId,
      runId,
      label,
      mediaType,
      extension,
      payload,
      partialPath,
      handle,
      hash: crypto.createHash("sha256"),
      bytes: 0,
      acceptedBytes: 0,
      failure: null,
      closed: false,
      queue: Promise.resolve(),
    });
    return { streamId, interfaceId, ...(runId ? { runId } : {}), label, mediaType };
  }

  async function append(owner, request) {
    const entry = ownedStream(streams, owner, request?.streamId);
    if (entry.closed) throw new Error("Artifact stream is already closing.");
    if (entry.failure) throw entry.failure;
    const input = streamBytes(request?.bytes, request?.base64);
    if (input.byteLength > MAX_STREAM_CHUNK_BYTES) {
      throw new Error(`Artifact stream chunk is ${input.byteLength} bytes; maximum is ${MAX_STREAM_CHUNK_BYTES}.`);
    }
    if (entry.acceptedBytes + input.byteLength > MAX_STREAM_BYTES) {
      entry.failure = new Error(`Artifact stream exceeds the ${MAX_STREAM_BYTES} byte client limit.`);
      throw entry.failure;
    }
    entry.acceptedBytes += input.byteLength;
    const bytes = Buffer.from(input);
    entry.queue = entry.queue.then(async () => {
      await entry.handle.writeFile(bytes);
      entry.hash.update(bytes);
      entry.bytes += bytes.byteLength;
    });
    await entry.queue;
    return { streamId: request.streamId, bytes: entry.bytes };
  }

  async function finalize(owner, request) {
    const streamId = requiredString(request?.streamId, "streamId");
    const entry = ownedStream(streams, owner, streamId);
    if (entry.closed) throw new Error("Artifact stream is already closing.");
    entry.closed = true;
    try {
      await entry.queue;
      if (entry.failure) throw entry.failure;
      if (entry.bytes === 0) throw new Error("Artifact stream contains no media bytes.");
      await entry.handle.sync();
      await entry.handle.close();
      const digest = entry.hash.digest("hex");
      const mediaPath = path.join(path.dirname(entry.partialPath), `${safeFilename(entry.label)}-${digest}.${entry.extension}`);
      await publishContentAddressedFile(entry.partialPath, mediaPath, digest, entry.bytes);
      await fs.unlink(entry.partialPath);
      const outcome = await submitClientInterfaceArtifact(options, {
        interfaceId: entry.interfaceId,
        runId: entry.runId,
        label: entry.label,
        payload: {
          ...entry.payload,
          media: {
            path: mediaPath,
            mime_type: entry.mediaType,
            content_hash: digest,
            size: entry.bytes,
          },
        },
        timeoutMs: request?.timeoutMs,
      });
      return { ...outcome, mediaType: entry.mediaType, mediaContentHash: digest, mediaBytes: entry.bytes };
    } finally {
      streams.delete(streamId);
      await entry.handle.close().catch(() => {});
      await fs.unlink(entry.partialPath).catch((error) => {
        if (error?.code !== "ENOENT") throw error;
      });
    }
  }

  async function abort(owner, request) {
    const streamId = requiredString(request?.streamId, "streamId");
    const entry = ownedStream(streams, owner, streamId);
    if (entry.closed) throw new Error("Artifact stream is already closing; its delivery outcome is unconfirmed.");
    entry.closed = true;
    streams.delete(streamId);
    await entry.queue.catch(() => {});
    await entry.handle.close().catch(() => {});
    await fs.unlink(entry.partialPath).catch((error) => {
      if (error?.code !== "ENOENT") throw error;
    });
    return { streamId, aborted: true };
  }

  async function abortOwner(owner) {
    const ownerId = String(owner ?? "");
    const ids = [...streams.entries()].filter(([, entry]) => entry.ownerId === ownerId).map(([id]) => id);
    await Promise.all(ids.map((streamId) => abort(ownerId, { streamId })));
  }

  async function abortAll() {
    const entries = [...streams.entries()].map(([streamId, entry]) => ({ streamId, ownerId: entry.ownerId }));
    await Promise.all(entries.map(({ streamId, ownerId }) => abort(ownerId, { streamId })));
  }

  return { abort, abortAll, abortOwner, append, begin, finalize };
}

async function resolveReadyTarget(options, interfaceId) {
  const projectionStore = requiredFunction(options?.projectionStore, "projectionStore");
  const response = await projectionStore().read(projectionReadRequest(
    "projection.interfaces.artifact_transfer.targets",
    { interface_id: interfaceId, status: "ready", limit: 50 },
  ));
  const targets = Array.isArray(response?.artifact_transfer_targets) ? response.artifact_transfer_targets : [];
  const ready = targets.filter((candidate) => candidate?.interface_id === interfaceId && candidate?.status === "ready");
  const paths = new Set(ready.map((candidate) => optionalString(candidate?.transfer_path)).filter(Boolean));
  if (paths.size === 1) return ready[0];
  if (paths.size > 1) throw new Error(`Interface ${interfaceId} declares multiple ready artifact hand-in sockets.`);
  const all = await projectionStore().read(projectionReadRequest(
    "projection.interfaces.artifact_transfer.targets",
    { interface_id: interfaceId, limit: 50 },
  ));
  const known = Array.isArray(all?.artifact_transfer_targets) ? all.artifact_transfer_targets[0] : null;
  throw new Error(known?.reason || `Interface ${interfaceId} has no ready artifact hand-in socket.`);
}

async function authorizeSubmission(options, request) {
  await requiredFunction(options?.authorizeSubmission, "authorizeSubmission")(request);
}

function assertAdmitted(verdict, label) {
  if (verdict?.admitted && Number.isFinite(verdict.admitted.seq)) return;
  if (verdict?.refused && typeof verdict.refused === "object") {
    throw new Error(`${label} refused (${String(verdict.refused.reason || "unknown")}): ${JSON.stringify(verdict.refused.detail)}`);
  }
  throw new Error(`${label} returned a malformed delivery verdict.`);
}

function safeFilename(value) {
  return value.replace(/[^A-Za-z0-9._-]+/g, "_").slice(0, 96) || "artifact";
}

function safeExtension(value) {
  const extension = String(value ?? "bin").trim().toLowerCase().replace(/^\.+/, "");
  if (!/^[a-z0-9]{1,12}$/.test(extension)) throw new Error("Artifact stream extension is invalid.");
  return extension;
}

function requiredMediaType(value) {
  const mediaType = requiredString(value, "mediaType");
  if (mediaType.length > 128 || !/^[A-Za-z0-9.+-]+\/[A-Za-z0-9.+-]+(?:;[ -~]+)?$/.test(mediaType)) {
    throw new Error("mediaType must be a valid MIME type.");
  }
  return mediaType;
}

function ownedStream(streams, owner, streamId) {
  const id = requiredString(streamId, "streamId");
  const entry = streams.get(id);
  if (!entry || entry.ownerId !== requiredString(owner, "stream owner")) throw new Error("Artifact stream is unavailable.");
  return entry;
}

function streamBytes(value, base64) {
  if (Buffer.isBuffer(value)) return value;
  if (ArrayBuffer.isView(value)) return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  if (value instanceof ArrayBuffer) return Buffer.from(value);
  if (typeof base64 === "string" && base64) {
    const compact = base64.replace(/[\r\n\t ]+/g, "");
    if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(compact)) {
      throw new Error("Artifact stream chunk carries invalid base64 bytes.");
    }
    const bytes = Buffer.from(compact, "base64");
    if (bytes.toString("base64") !== compact) {
      throw new Error("Artifact stream chunk carries non-canonical base64 bytes.");
    }
    return bytes;
  }
  throw new Error("Artifact stream chunk must carry bytes.");
}

function requiredSegment(value, field) {
  const output = requiredString(value, field);
  if (output.length > 96 || !/^[A-Za-z0-9_-]+$/.test(output)) {
    throw new Error(`${field} must contain only letters, digits, underscore, or hyphen.`);
  }
  return output;
}

function requiredString(value, field) {
  const output = typeof value === "string" ? value.trim() : "";
  if (!output) throw new Error(`${field} is required.`);
  return output;
}

function optionalString(value) {
  return typeof value === "string" ? value.trim() : "";
}

function requiredObject(value, field) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${field} must be an object.`);
  return value;
}

function requiredFunction(value, field) {
  if (typeof value !== "function") throw new Error(`${field} must be a function.`);
  return value;
}

function resolveClientArtifactsRoot(options, interfaceId) {
  const resolveRoot = requiredFunction(options?.clientArtifactsRoot, "clientArtifactsRoot");
  return path.resolve(requiredString(resolveRoot(interfaceId), `clientArtifactsRoot('${interfaceId}')`));
}

function positiveNumber(value, fallback) {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : fallback;
}

module.exports = {
  createClientArtifactStreamStore,
  submitClientInterfaceArtifact,
};
