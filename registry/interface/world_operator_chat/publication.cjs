const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");

const MAX_WRITES = 1024;
const MAX_REQUEST_BYTES = 64 * 1024 * 1024;
const RECEIPT_FIELDS = ["digest_algorithm", "operation_id", "path", "before_hash", "after_hash", "bytes", "outcome", "error"];
const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const nullableHash = (value) => value === null || (typeof value === "string" && /^[0-9a-f]{64}$/.test(value));

function publicationCondition(value) {
  publicationObject(value, ["octopus_home", "preimages"], "file publication condition");
  const normalPath = target => {
    if (typeof target !== "string" || !path.isAbsolute(target) || path.resolve(target) !== target
      || target.includes("\0") || target.split(path.sep).includes(".octopus-publication")) {
      throw new Error("file publication condition requires normal absolute paths");
    }
    return target;
  };
  if (!Array.isArray(value.preimages) || value.preimages.length > 4096) throw new Error("file publication condition exceeds its preimage bound");
  const seen = new Set();
  return { octopus_home: normalPath(value.octopus_home), preimages: value.preimages.map(pin => {
    publicationObject(pin, ["path", "sha256"], "file publication preimage");
    const target = normalPath(pin.path);
    if (seen.has(target) || !nullableHash(pin.sha256)) throw new Error("file publication condition has duplicate paths or an invalid SHA-256 preimage");
    seen.add(target);
    return { path: target, sha256: pin.sha256 };
  }) };
}

async function publishFiles(writes, publish, condition) {
  if (typeof publish !== "function") throw new Error("native file publication callback is required");
  if (!Array.isArray(writes) || writes.length > MAX_WRITES) throw new Error("file publication exceeds its write bound");
  if (writes.length === 0) return [];
  const prepared = writes.map((write) => {
    if (typeof write.filePath !== "string" || !path.isAbsolute(write.filePath)
      || !(typeof write.content === "string" || write.content === null) || !nullableHash(write.expectedBeforeHash)
      || write.content === null && write.expectedBeforeHash === null) {
      throw new Error("file publication requires an absolute path, text or null removal and exact SHA-256 preimage (nonnull for removal)");
    }
    const operationId = write.operationId === undefined ? crypto.randomUUID() : write.operationId;
    if (typeof operationId !== "string" || !operationId || operationId.trim() !== operationId
      || Buffer.byteLength(operationId) > 256 || /\p{Cc}/u.test(operationId)) {
      throw new Error("file publication operation id is invalid");
    }
    return {
      tag: write.tag,
      afterHash: write.content === null ? null : hash(Buffer.from(write.content)),
      bytes: write.content === null ? 0 : Buffer.byteLength(write.content),
      request: { path: write.filePath, operation_id: operationId,
        expected_before_hash: write.expectedBeforeHash, content: write.content },
    };
  });
  const request = { writes: prepared.map((entry) => entry.request),
    ...(condition === undefined ? {} : { condition: publicationCondition(condition) }) };
  if (request.condition && prepared.some(entry => entry.request.content === null || entry.request.expected_before_hash === null)) {
    throw new Error("conditioned file publication supports existing-file updates only");
  }
  if (request.condition && prepared.some(entry => path.resolve(entry.request.path) !== entry.request.path)) {
    throw new Error("conditioned file publication requires original normal absolute targets");
  }
  if (Buffer.byteLength(JSON.stringify(request)) > MAX_REQUEST_BYTES) throw new Error("file publication request exceeds its byte bound");
  if (!request.condition) for (const entry of prepared) entry.request.path = await publicationPath(entry.request.path);
  if (new Set(request.writes.map((write) => write.path)).size !== writes.length) throw new Error("file publication repeats a logical target");
  if (Buffer.byteLength(JSON.stringify(request)) > MAX_REQUEST_BYTES) throw new Error("file publication request exceeds its byte bound");
  const receipts = [];
  let publicationResult = null;
  try {
    const response = publicationResult = await publish(request);
    if (!response || Object.keys(response).length !== 2 || !Array.isArray(response.receipts) || response.receipts.length > prepared.length
      || (response.error !== null && (typeof response.error !== "string" || !response.error))) {
      throw new Error("native file publication returned a malformed result");
    }
    for (const [index, receipt] of response.receipts.entries()) {
      const entry = prepared[index];
      if (receipts.at(-1)?.outcome !== undefined && receipts.at(-1).outcome !== "applied") throw new Error("native file publication continued after refusal");
      if (!receipt || Object.keys(receipt).length !== RECEIPT_FIELDS.length
        || RECEIPT_FIELDS.some((field) => !Object.hasOwn(receipt, field))
        || receipt.digest_algorithm !== "sha256"
        || receipt.operation_id !== entry.request.operation_id || receipt.path !== entry.request.path
        || !nullableHash(receipt.before_hash) || !nullableHash(receipt.after_hash)
        || !Number.isSafeInteger(receipt.bytes) || receipt.bytes < 0
        || !["applied", "not_applied", "uncertain"].includes(receipt.outcome)
        || (receipt.error !== null && (typeof receipt.error !== "string" || !receipt.error))) {
        throw new Error("native file publication receipt differs from its exact submitted operation");
      }
      if (receipt.before_hash !== entry.request.expected_before_hash
        || receipt.after_hash !== entry.afterHash || receipt.bytes !== entry.bytes) {
        throw new Error("native file publication receipt has different submitted byte or preimage evidence");
      }
      if (receipt.outcome === "applied" && receipt.error !== null) {
        throw new Error("native file publication applied receipt carries an error");
      }
      receipts.push(receipt);
    }
    const refused = receipts.find((receipt) => receipt.outcome !== "applied");
    if (refused) throw new Error(`client authoring ${refused.outcome} for '${refused.path}': ${refused.error || "publication was not applied"}`);
    if (response.error !== null) throw new Error(`native file publication failed: ${response.error}`);
    if (receipts.length !== prepared.length) throw new Error("native file publication omitted a submitted write");
    return receipts;
  } catch (error) {
    const failure = error instanceof Error ? error : new Error(String(error));
    failure.code = "client_publication_failed";
    failure.publicationReceipts = receipts;
    failure.publicationResult = publicationResult;
    failure.publicationRequests = prepared.map((entry) => ({
      path: entry.request.path, operation_id: entry.request.operation_id,
      expected_before_hash: entry.request.expected_before_hash, after_hash: entry.afterHash,
    }));
    throw failure;
  }
}

async function publicationPath(target) {
  const suffix = [path.basename(target)];
  let parent = path.dirname(target);
  for (;;) {
    try { return path.join(await fs.realpath(parent), ...suffix); }
    catch (error) {
      if (error.code !== "ENOENT") throw error;
      if (path.dirname(parent) === parent) throw error;
      suffix.unshift(path.basename(parent));
      parent = path.dirname(parent);
    }
  }
}

async function publishClientWrites(writes, publish) {
  const receipts = await publishFiles(writes, publish);
  return receipts.map((receipt, index) => ({
    path: writes[index].filePath, tag: writes[index].tag,
    beforeHash: receipt.before_hash, afterHash: receipt.after_hash, bytes: receipt.bytes,
    changed: receipt.before_hash !== receipt.after_hash,
  }));
}

function publicationObject(value, fields, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).length !== fields.length || fields.some(field => !Object.hasOwn(value, field))) {
    throw new Error(`${label} has an invalid exact field set`);
  }
  return value;
}

function publicationNumber(value, maximum = Number.MAX_SAFE_INTEGER) {
  if (!Number.isSafeInteger(value) || value < 0 || value > maximum) throw new Error("directory publication has an invalid exact integer");
  return value;
}

function directoryPath(value, relative = false) {
  if (typeof value !== "string" || !value || value.includes("\0") || Buffer.from(value).toString() !== value
    || path.isAbsolute(value) === relative || path.normalize(value) !== value
    || value.split("/").some(part => [".", "..", ".octopus-publication"].includes(part))
    || (relative ? value.split("/").some(part => !part) : value !== path.resolve(value) || value === path.parse(value).root)) {
    throw new Error("directory publication requires an ordinary normalized path");
  }
  return value;
}

function compareDirectoryPaths(left, right) {
  const a = left.split("/"), b = right.split("/");
  for (let index = 0; index < Math.min(a.length, b.length); index += 1) {
    const compared = Buffer.compare(Buffer.from(a[index]), Buffer.from(b[index]));
    if (compared) return compared;
  }
  return a.length - b.length;
}

function directoryIdentity(value) {
  publicationObject(value, ["device", "inode"], "directory identity");
  return { device: publicationNumber(value.device), inode: publicationNumber(value.inode) };
}

function directoryPreimage(value) {
  publicationObject(value, ["identity", "mode", "digest_algorithm", "entries"], "directory preimage");
  if (value.digest_algorithm !== "sha256" || !Array.isArray(value.entries) || value.entries.length > 16384) {
    throw new Error("directory preimage requires a bounded SHA-256 manifest");
  }
  const parents = new Map();
  let previous;
  return { identity: directoryIdentity(value.identity), mode: publicationNumber(value.mode, 0o7777), digest_algorithm: "sha256",
    entries: value.entries.map(entry => {
      publicationObject(entry, ["relative_path", "kind", "identity", "mode", "bytes", "digest"], "directory entry");
      const relative = directoryPath(entry.relative_path, true), parent = path.dirname(relative);
      if (previous !== undefined && compareDirectoryPaths(previous, relative) >= 0
        || parent !== "." && parents.get(parent) !== "directory"
        || !["directory", "file", "symlink"].includes(entry.kind)
        || !nullableHash(entry.digest) || (entry.kind === "directory" ? entry.digest !== null || entry.bytes !== 0 : entry.digest === null)) {
        throw new Error("directory manifest has invalid order, parent or typed content evidence");
      }
      previous = relative; parents.set(relative, entry.kind);
      return { relative_path: relative, kind: entry.kind, identity: directoryIdentity(entry.identity),
        mode: publicationNumber(entry.mode, 0o7777), bytes: publicationNumber(entry.bytes), digest: entry.digest };
    }) };
}

function directoryMovement(value) {
  publicationObject(value, ["source", "destination", "expected"], "directory movement");
  const source = directoryPath(value.source), destination = directoryPath(value.destination);
  if (source === destination) throw new Error("directory movement repeats its endpoint");
  return { source, destination, expected: directoryPreimage(value.expected) };
}

function directoryReceipt(value) {
  publicationObject(value, ["schema_version", "operation_id", "index", "plan_hash", "movement", "outcome", "error"], "directory receipt");
  if (value.schema_version !== 1 || typeof value.operation_id !== "string" || !value.operation_id
    || value.operation_id.trim() !== value.operation_id || Buffer.byteLength(value.operation_id) > 256 || /\p{Cc}/u.test(value.operation_id)
    || !nullableHash(value.plan_hash) || value.plan_hash === null || !["applied", "not_applied", "uncertain"].includes(value.outcome)
    || (value.outcome === "applied" ? value.error !== null : typeof value.error !== "string" || !value.error)) {
    throw new Error("directory receipt has invalid operation or outcome evidence");
  }
  return { schema_version: 1, operation_id: value.operation_id, index: publicationNumber(value.index, 127),
    plan_hash: value.plan_hash, movement: directoryMovement(value.movement), outcome: value.outcome, error: value.error };
}

function directoryTransport(request, publish) {
  if (typeof publish !== "function") throw new Error("native directory publication callback is required");
  const encoded = JSON.stringify(request);
  if (Buffer.byteLength(encoded) > MAX_REQUEST_BYTES) throw new Error("directory publication exceeds its byte bound");
  return publish(JSON.parse(encoded));
}

async function snapshotDirectory(target, publish) {
  const request = { path: directoryPath(target), digest_algorithm: "sha256" };
  const result = await directoryTransport(request, publish);
  publicationObject(result, ["path", "preimage"], "directory snapshot result");
  if (result.path !== request.path) throw new Error("directory snapshot returned another path");
  return directoryPreimage(result.preimage);
}

async function publishDirectoryMoves(operationId, movements, publish) {
  if (typeof operationId !== "string" || !operationId || operationId.trim() !== operationId
    || Buffer.byteLength(operationId) > 256 || /\p{Cc}/u.test(operationId)) throw new Error("directory publication operation id is invalid");
  if (!Array.isArray(movements) || !movements.length || movements.length > 128) throw new Error("directory publication exceeds its move bound");
  const request = { operation_id: operationId, moves: movements.map(directoryMovement) };
  if (request.moves.reduce((count, movement) => count + movement.expected.entries.length, 0) > 16384) throw new Error("directory publication exceeds its entry bound");
  const planHash = hash(JSON.stringify(request.moves)), receipts = [];
  let publicationResult;
  try {
    const result = publicationResult = await directoryTransport(request, publish);
    publicationObject(result, ["receipts", "error"], "directory publication result");
    if (!Array.isArray(result.receipts) || result.receipts.length > request.moves.length
      || result.error !== null && (typeof result.error !== "string" || !result.error)) throw new Error("directory publication returned a malformed result");
    for (const [index, receipt] of result.receipts.entries()) {
      const observed = directoryReceipt(receipt);
      if (observed.operation_id !== operationId || observed.index !== index || observed.plan_hash !== planHash
        || JSON.stringify(observed.movement) !== JSON.stringify(request.moves[index])
        || observed.outcome === "applied" && receipts.some(prior => prior.outcome !== "applied")) {
        throw new Error("directory receipt differs from its exact submitted batch");
      }
      receipts.push(receipt);
    }
    const refused = receipts.find(receipt => receipt.outcome !== "applied");
    if (refused) {
      const details = [...new Set([refused.error, result.error].filter(Boolean))].join("; ");
      throw new Error(`directory publication ${refused.outcome} for '${refused.movement.source}' -> '${refused.movement.destination}': ${details || "movement was not applied"}`);
    }
    if (result.error !== null) throw new Error(`directory publication failed: ${result.error}`);
    if (receipts.length !== request.moves.length) throw new Error("directory publication omitted a submitted movement");
    return receipts;
  } catch (error) {
    const failure = error instanceof Error ? error : new Error(String(error));
    failure.code = "client_publication_failed";
    failure.publicationReceipts = receipts; failure.publicationRequests = request;
    failure.publicationResult = publicationResult ?? null;
    throw failure;
  }
}

async function reclaimDirectory(tombstone, publication, expected, publish) {
  const request = { tombstone: directoryPath(tombstone), publication: directoryReceipt(publication), expected: directoryPreimage(expected) };
  if (request.publication.outcome !== "applied" || request.tombstone !== request.publication.movement.destination
    || JSON.stringify(request.expected) !== JSON.stringify(request.publication.movement.expected)) {
    throw new Error("directory reclamation requires its exact applied destination and manifest");
  }
  const result = await directoryTransport(request, publish);
  publicationObject(result, ["tombstone", "publication", "reclaimed"], "directory reclaim result");
  if (result.reclaimed !== true || result.tombstone !== request.tombstone
    || JSON.stringify(directoryReceipt(result.publication)) !== JSON.stringify(request.publication)) {
    throw new Error("directory reclamation returned another original movement");
  }
}

function createStoragePublisher(binary, runCommand) {
  if (typeof binary !== "string" || !path.isAbsolute(binary) || typeof runCommand !== "function") {
    throw new Error("storage publisher requires an absolute native executable and explicit command transport");
  }
  const command = (flag, maximum) => request => runCommand(binary, flag, request, maximum);
  return Object.freeze({
    publishFiles: (writes, condition) => publishFiles(writes, command("--publish-files", 4 * 1024 * 1024), condition),
    snapshotDirectory: target => snapshotDirectory(target, command("--snapshot-directory", MAX_REQUEST_BYTES)),
    publishDirectoryMoves: (operationId, moves) => publishDirectoryMoves(operationId, moves, command("--publish-directory-moves", MAX_REQUEST_BYTES)),
    reclaimDirectory: (target, receipt, expected) => reclaimDirectory(target, receipt, expected, command("--reclaim-directory", MAX_REQUEST_BYTES)),
  });
}

module.exports = { createStoragePublisher, publicationCondition, publishFiles, publishClientWrites, snapshotDirectory, publishDirectoryMoves, reclaimDirectory };
