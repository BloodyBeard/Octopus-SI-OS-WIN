const fs = require("node:fs/promises");
const { constants } = require("node:fs");
const crypto = require("node:crypto");
const path = require("node:path");
const { blake3 } = require("@noble/hashes/blake3");

const DEFAULT_MAX_MANIFEST_BYTES = 2 * 1024 * 1024;
const DEFAULT_MAX_PAYLOAD_BYTES = 40 * 1024 * 1024;

async function readContentAddressedArtifactPayload(options) {
  const blobsRoot = absolutePath(options?.blobsRoot, "blobsRoot");
  const manifestHash = contentHash(options?.manifestHash, "manifestHash");
  const expectedArtifactId = requiredString(options?.artifactId, "artifactId");
  const expectedLabel = requiredString(options?.label, "label");
  const manifestBytes = await readStableContentAddressedBytes(
    path.join(blobsRoot, manifestHash),
    manifestHash,
    positiveInteger(options?.maxManifestBytes, DEFAULT_MAX_MANIFEST_BYTES),
    "artifact manifest",
  );
  let manifest;
  try {
    manifest = JSON.parse(manifestBytes.toString("utf8"));
  } catch (error) {
    throw new Error(`artifact manifest is malformed JSON: ${error.message}`);
  }
  if (!isObject(manifest)) throw new Error("artifact manifest must be a JSON object");
  const label = requiredString(manifest.label, "artifact manifest label");
  if (label !== expectedLabel || expectedArtifactId !== `${label}:${manifestHash}`) {
    throw new Error("artifact identity does not match its content-addressed manifest");
  }
  const payloadPath = absolutePath(manifest.payload_path, "artifact manifest payload_path");
  const payloadHash = contentHash(
    manifest.payload_content_hash,
    "artifact manifest payload_content_hash",
  );
  const payloadBytes = await readStableContentAddressedBytes(
    path.join(blobsRoot, payloadHash),
    payloadHash,
    positiveInteger(options?.maxPayloadBytes, DEFAULT_MAX_PAYLOAD_BYTES),
    "artifact payload",
  );
  const text = payloadBytes.toString("utf8");
  let payload;
  if (Buffer.from(text, "utf8").equals(payloadBytes)) {
    try {
      payload = JSON.parse(text);
    } catch {
      payload = { __raw: text };
    }
  } else {
    payload = {
      __binary: true,
      content_hash: payloadHash,
      payload_path: payloadPath,
      size: payloadBytes.byteLength,
    };
  }
  return {
    label,
    manifestHash,
    payloadHash,
    payloadBytes,
    payloadPath,
    payload,
  };
}

async function readStableContentAddressedBytes(filePath, expectedHash, maximumBytes, label) {
  const stat = await fs.lstat(filePath);
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${label} is not a regular file`);
  if (stat.size > maximumBytes) throw new Error(`${label} exceeds ${maximumBytes} bytes`);
  const handle = await fs.open(filePath, "r");
  try {
    const before = await handle.stat();
    const bytes = await handle.readFile();
    const after = await handle.stat();
    if (
      bytes.byteLength !== before.size
      || before.size !== after.size
      || before.mtimeMs !== after.mtimeMs
    ) {
      throw new Error(`${label} changed while it was being read`);
    }
    const actualHash = Buffer.from(blake3(bytes)).toString("hex");
    if (actualHash !== expectedHash) {
      throw new Error(`${label} does not match its content-addressed digest`);
    }
    return bytes;
  } finally {
    await handle.close();
  }
}

function contentHash(value, field) {
  const hash = requiredString(value, field).toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(hash)) throw new Error(`${field} must be a 32-byte content digest`);
  return hash;
}

function absolutePath(value, field) {
  const input = requiredString(value, field);
  if (!path.isAbsolute(input)) throw new Error(`${field} must be absolute`);
  return path.normalize(input);
}

function positiveInteger(value, fallback) {
  return Number.isSafeInteger(value) && value > 0 ? value : fallback;
}

function requiredString(value, field) {
  if (typeof value !== "string" || !value || value.trim() !== value) {
    throw new Error(`${field} must be a non-empty trimmed string`);
  }
  return value;
}

function isObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

async function materializeContentAddressedAttachment({ blobsRoot, attachment, revalidate }) {
  if (typeof revalidate !== "function") throw new Error("Selected payload requires its current client access check");
  await revalidate();
  const root = absolutePath(blobsRoot, "blobsRoot");
  const rootStat = await fs.lstat(root);
  if (!rootStat.isDirectory() || rootStat.isSymbolicLink() || await fs.realpath(root) !== root) {
    throw new Error("Selected payload cache must be the existing canonical artifact cache");
  }
  if (!isObject(attachment)) throw new Error("Selected payload requires an owned attachment stage");
  const label = requiredString(attachment.label, "attachment.label");
  if (label.length > 160 || /[\x00-\x1f\x7f]/.test(label)) throw new Error("Attachment label is invalid");
  const source = absolutePath(attachment.payloadPath, "attachment.payloadPath");
  const expectedSha = contentHash(attachment.contentHash, "attachment.contentHash");
  if (!Number.isSafeInteger(attachment.size) || attachment.size < 0 || attachment.size > 100 * 1024 * 1024) {
    throw new Error("Selected payload exceeds the existing attachment byte bound");
  }
  const temporary = path.join(root, `.client-payload-${crypto.randomBytes(24).toString("hex")}.tmp`);
  const input = await fs.open(source, constants.O_RDONLY | constants.O_NOFOLLOW);
  let output;
  try {
    const before = await input.stat();
    if (!before.isFile() || before.uid !== process.getuid() || before.size !== attachment.size) {
      throw new Error("Selected attachment stage changed before materialization");
    }
    output = await fs.open(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
    const payloadHash = blake3.create(), stageHash = crypto.createHash("sha256"), buffer = Buffer.alloc(4 * 1024 * 1024);
    let size = 0;
    for (;;) {
      const { bytesRead } = await input.read(buffer, 0, buffer.length, null);
      if (bytesRead === 0) break;
      size += bytesRead;
      if (size > attachment.size) throw new Error("Selected attachment stage grew while materializing");
      const bytes = buffer.subarray(0, bytesRead);
      payloadHash.update(bytes); stageHash.update(bytes);
      let offset = 0;
      while (offset < bytesRead) {
        const written = await output.write(bytes, offset, bytesRead - offset);
        if (written.bytesWritten === 0) throw new Error("Artifact cache write made no progress");
        offset += written.bytesWritten;
      }
    }
    const after = await input.stat();
    if (size !== attachment.size || before.size !== after.size || before.mtimeMs !== after.mtimeMs
        || before.ctimeMs !== after.ctimeMs || stageHash.digest("hex") !== expectedSha) {
      throw new Error("Selected attachment bytes differ from the captured stage");
    }
    await output.sync(); await output.close(); output = null;
    await revalidate();
    const payloadDigest = Buffer.from(payloadHash.digest()).toString("hex");
    const payloadPath = path.join(root, payloadDigest);
    await publishImmutableBlob(temporary, payloadPath, payloadDigest, size);
    const manifest = { label, payload_path: payloadPath, payload_content_hash: payloadDigest };
    const manifestBytes = Buffer.from(JSON.stringify(manifest));
    const manifestHash = Buffer.from(blake3(manifestBytes)).toString("hex");
    const manifestTemporary = path.join(root, `.client-manifest-${crypto.randomBytes(24).toString("hex")}.tmp`);
    let file;
    try {
      file = await fs.open(manifestTemporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
      await file.writeFile(manifestBytes); await file.sync(); await file.close(); file = null;
      await publishImmutableBlob(manifestTemporary, path.join(root, manifestHash), manifestHash, manifestBytes.length);
    } finally {
      await file?.close(); await removeTemporaryBlob(manifestTemporary);
    }
    const directory = await fs.open(root, constants.O_RDONLY);
    try { await directory.sync(); } finally { await directory.close(); }
    await revalidate();
    return { id: `${label}:${manifestHash}`, content: Array.from(Buffer.from(manifestHash, "hex")) };
  } finally {
    await input.close(); await output?.close(); await removeTemporaryBlob(temporary);
  }
}

async function prepareContentAddressedPayloadBindings({ blobsRoot, attachments, payloadArtifactBindings: selectedBase, revalidate }) {
  if (!Array.isArray(selectedBase) || !Array.isArray(attachments)) throw new Error("Payload preparation requires its selected base and owned stages");
  if (typeof revalidate !== "function") throw new Error("Payload preparation requires its current client access check");
  const bindings = payloadArtifactBindings(selectedBase);
  await revalidate();
  for (const attachment of attachments) {
    const reference = await materializeContentAddressedAttachment({ blobsRoot, attachment, revalidate });
    const previous = bindings.find(value => value.id === reference.id);
    if (previous && !Buffer.from(previous.content).equals(Buffer.from(reference.content))) {
      throw new Error("Selected file changed its exact artifact identity");
    }
    if (!previous) bindings.push(reference);
  }
  await revalidate();
  return bindings;
}

async function publishImmutableBlob(temporary, destination, digest, size) {
  try { await fs.link(temporary, destination); }
  catch (error) { if (error.code !== "EEXIST") throw error; }
  const file = await fs.open(destination, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const before = await file.stat();
    if (!before.isFile() || before.size !== size) throw new Error("Existing artifact cache entry has another shape or size");
    const hash = blake3.create(), buffer = Buffer.alloc(4 * 1024 * 1024);
    let count = 0;
    for (;;) {
      const {bytesRead} = await file.read(buffer, 0, buffer.length, null);
      if (!bytesRead) break;
      count += bytesRead;
      if (count > size) throw new Error("Artifact cache entry grew during verification");
      hash.update(buffer.subarray(0, bytesRead));
    }
    const after = await file.stat();
    if (count !== size || before.size !== after.size || before.mtimeMs !== after.mtimeMs
        || Buffer.from(hash.digest()).toString("hex") !== digest) throw new Error("Existing artifact cache entry does not match its exact content digest");
  } finally { await file.close(); }
}

async function removeTemporaryBlob(file) {
  try { await fs.unlink(file); } catch (error) { if (error.code !== "ENOENT") throw error; }
}

function payloadArtifactBindings(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error("payloadArtifactBindings must be an explicit ArtifactRef array");
  const ids = new Set();
  return value.map(reference => {
    if (!isObject(reference) || Object.keys(reference).sort().join(",") !== "content,id") throw new Error("Payload selection is not an exact ArtifactRef");
    const id = requiredString(reference.id, "payload artifact id");
    if (id.length > 2048 || /[\x00-\x1f\x7f]/.test(id) || ids.has(id)) throw new Error("Payload selection has an invalid or duplicate artifact identity");
    if (!Array.isArray(reference.content) || reference.content.length !== 32
        || reference.content.some(byte => !Number.isInteger(byte) || byte < 0 || byte > 255)) throw new Error("Payload artifact must retain its actual BLAKE3 byte array");
    ids.add(id); return {id, content: [...reference.content]};
  });
}

module.exports = {
  materializeContentAddressedAttachment,
  prepareContentAddressedPayloadBindings,
  payloadArtifactBindings,
  readContentAddressedArtifactPayload,
};
