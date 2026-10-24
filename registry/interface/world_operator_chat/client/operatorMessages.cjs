module.exports = { assertOperatorAttachmentInputLimits, assertOperatorMessageRequestMatchesEnvelope, materializeAttachments, operatorClientContext, operatorMessageChatId, operatorMessageEnvelopeFromBytes, operatorMessagePayload, operatorMessagePayloadPath, operatorTurnClientContext, planOperatorAttachments, readDurableOperatorMessageEnvelope, resolveRunRecordsRoot, sha256File, verifyOperatorMessageAttachments, writeContentAddressedPayload, writeExactOperatorMessageFile };

const crypto = require("node:crypto");
const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const path = require("node:path");
const { homeStateClientContext } = require("../home-state.cjs");
const { MAX_OPERATOR_ATTACHMENT_BYTES, MAX_OPERATOR_ATTACHMENT_COUNT, MAX_OPERATOR_ATTACHMENT_TOTAL_BYTES, MAX_OPERATOR_CLIENT_CONTEXT_BYTES, MAX_OPERATOR_CLIENT_CONTEXT_ENTRIES } = require("./constants.cjs");
const { syncDirectory } = require("./publication.cjs");
const { canonicalJson, pathIsWithin, safeContractToken } = require("./topologyPlan.cjs");
const { absolutePath, contentHashHex, nonNegativeInteger, randomToken, readJsonIfPresent, requiredArray, requiredObject, requiredString, safeFilename, sha256Hex, stringValue } = require("./values.cjs");

async function readDurableOperatorMessageEnvelope(input) {
  const messageBytes = await fs.readFile(input.admission.payload_path);
  if (sha256Hex(messageBytes) !== input.admission.payload_sha256) {
    throw new Error(`operator message '${input.messageId}' durable payload changed after preparation`);
  }
  return operatorMessageEnvelopeFromBytes({
    attachmentRoot: input.attachmentRoot,
    messageBytes,
    messageId: input.messageId,
    messagePath: input.admission.payload_path,
  });
}

function operatorMessageEnvelopeFromBytes(input) {
  let value;
  try {
    value = JSON.parse(input.messageBytes.toString("utf8"));
  } catch (error) {
    throw new Error(`operator message '${input.messageId}' durable payload is malformed JSON: ${error.message}`);
  }
  return {
    messageBytes: input.messageBytes,
    messagePath: input.messagePath,
    payload: operatorMessagePayload(value, input.messageId, input.attachmentRoot),
  };
}

function assertOperatorMessageRequestMatchesEnvelope(input, payload) {
  if (canonicalJson(input.replacesMessage ?? null) !== canonicalJson(payload.replaces_message ?? null)) {
    throw new Error("Message revision differs from its prepared envelope");
  }
  if (payload.text !== input.requestedText) {
    throw new Error(`operator message '${input.messageId}' conflicts with its durable prepared envelope`);
  }
  if (input.replacesMessage) return;
  if (input.recoverPreparedMessage) {
    if (input.attachmentInputs.length !== 0 || payload.attachments.length === 0) {
      throw new Error(`operator message '${input.messageId}' is not an attachment-backed prepared message`);
    }
    return;
  }
  const requestedAttachments = input.attachmentInputs.map((attachment) => ({
    name: requiredString(attachment?.name, "attachment.name"),
    label: requiredString(attachment?.label, "attachment.label"),
    mime_type: stringValue(attachment?.type) || "application/octet-stream",
    size: nonNegativeInteger(attachment?.size, "attachment.size"),
    payload_content_hash: contentHashHex(attachment?.contentHash, "attachment.contentHash"),
    last_modified: nonNegativeInteger(attachment?.lastModified, "attachment.lastModified"),
  }));
  const durableAttachments = payload.attachments.map((attachment) => ({
    name: attachment.name,
    label: attachment.label,
    mime_type: attachment.mime_type,
    size: attachment.size,
    payload_content_hash: attachment.payload_content_hash,
    last_modified: attachment.last_modified,
  }));
  if (canonicalJson(requestedAttachments) !== canonicalJson(durableAttachments)) {
    throw new Error(`operator message '${input.messageId}' conflicts with its durable prepared envelope`);
  }
}

async function operatorTurnClientContext(options, clientSurfaceRole, clientContext) {
  const entries = clientContext === undefined ? [] : operatorClientContext(clientContext);
  if (entries.some(entry => entry.name === "home_state")) {
    throw new Error("home_state must come from the trusted client projection");
  }
  if (clientSurfaceRole !== "home" || options.readProjection === undefined) return entries;
  if (typeof options.readProjection !== "function") {
    throw new Error("readProjection must be a trusted client callback");
  }
  return operatorClientContext([await homeStateClientContext(options.readProjection, Date.now()), ...entries]);
}

function operatorClientContext(value) {
  const entries = requiredArray(value, "operator message payload.client_context");
  if (entries.length === 0 || entries.length > MAX_OPERATOR_CLIENT_CONTEXT_ENTRIES) {
    throw new Error(`operator message client_context carries ${entries.length} entries; allowed 1 to ${MAX_OPERATOR_CLIENT_CONTEXT_ENTRIES}`);
  }
  const names = new Set();
  let totalBytes = 0;
  return entries.map((entry, index) => {
    const field = `operator message payload.client_context[${index}]`;
    const object = requiredObject(entry, field);
    if (Object.keys(object).length !== 2 || !Object.hasOwn(object, "name") || !Object.hasOwn(object, "text")) {
      throw new Error(`${field} has an invalid field set`);
    }
    if (typeof object.name !== "string" || !/^[a-z][a-z0-9_]{0,63}$/.test(object.name) || names.has(object.name)) {
      throw new Error(`${field}.name must be a unique lowercase token`);
    }
    names.add(object.name);
    if (typeof object.text !== "string" || !object.text.trim() || object.text.trim() !== object.text) {
      throw new Error(`${field}.text must be a nonempty trimmed string`);
    }
    const bytes = Buffer.byteLength(object.text, "utf8");
    const entryLimit = object.name === "client_observation" ? 4 * 1024
      : object.name === "home_state" ? 12 * 1024 : MAX_OPERATOR_CLIENT_CONTEXT_BYTES;
    if (bytes > entryLimit) throw new Error(`${field}.text exceeds ${entryLimit} bytes`);
    totalBytes += bytes;
    if (totalBytes > MAX_OPERATOR_CLIENT_CONTEXT_BYTES) {
      throw new Error(`operator message client_context exceeds ${MAX_OPERATOR_CLIENT_CONTEXT_BYTES} bytes`);
    }
    return { name: object.name, text: object.text };
  });
}

function operatorMessagePayload(value, messageId, attachmentRoot) {
  const payload = requiredObject(value, "operator message payload");
  const fields = ["text", "message_id", "attachments"];
  const optionalFields = ["client_context", "replaces_message"];
  if (
    fields.some((field) => !Object.hasOwn(payload, field))
    || Object.keys(payload).some((field) => !fields.includes(field) && !optionalFields.includes(field))
  ) {
    throw new Error("operator message payload has an invalid field set");
  }
  const clientContext = Object.hasOwn(payload, "client_context")
    ? operatorClientContext(payload.client_context)
    : null;
  if (typeof payload.text !== "string" || payload.text.trim() !== payload.text) {
    throw new Error("operator message payload.text must be a trimmed string");
  }
  if (safeContractToken(payload.message_id, "operator message payload.message_id") !== messageId) {
    throw new Error(`operator message '${messageId}' durable payload has a different message id`);
  }
  const attachments = requiredArray(payload.attachments, "operator message payload.attachments")
    .map((value, index) => {
      const attachment = requiredObject(value, `operator message payload.attachments[${index}]`);
      const attachmentFields = [
        "name",
        "label",
        "mime_type",
        "size",
        "payload_path",
        "payload_content_hash",
        "last_modified",
      ];
      if (
        Object.keys(attachment).length !== attachmentFields.length
        || attachmentFields.some((field) => !Object.hasOwn(attachment, field))
      ) {
        throw new Error(`operator message payload.attachments[${index}] has an invalid field set`);
      }
      const payloadPath = absolutePath(
        attachment.payload_path,
        `operator message payload.attachments[${index}].payload_path`,
      );
      if (!pathIsWithin(attachmentRoot, payloadPath)) {
        throw new Error(`operator message payload.attachments[${index}] has an invalid durable path`);
      }
      return {
        name: requiredString(attachment.name, `operator message payload.attachments[${index}].name`),
        label: requiredString(attachment.label, `operator message payload.attachments[${index}].label`),
        mime_type: requiredString(attachment.mime_type, `operator message payload.attachments[${index}].mime_type`),
        size: nonNegativeInteger(attachment.size, `operator message payload.attachments[${index}].size`),
        payload_path: payloadPath,
        payload_content_hash: contentHashHex(
          attachment.payload_content_hash,
          `operator message payload.attachments[${index}].payload_content_hash`,
        ),
        last_modified: nonNegativeInteger(
          attachment.last_modified,
          `operator message payload.attachments[${index}].last_modified`,
        ),
      };
    });
  if (attachments.length > MAX_OPERATOR_ATTACHMENT_COUNT) {
    throw new Error(`operator message carries ${attachments.length} attachments; maximum is ${MAX_OPERATOR_ATTACHMENT_COUNT}`);
  }
  let totalAttachmentBytes = 0;
  for (const attachment of attachments) {
    if (attachment.size > MAX_OPERATOR_ATTACHMENT_BYTES) {
      throw new Error(`operator message attachment '${attachment.name}' exceeds ${MAX_OPERATOR_ATTACHMENT_BYTES} bytes`);
    }
    totalAttachmentBytes += attachment.size;
    if (totalAttachmentBytes > MAX_OPERATOR_ATTACHMENT_TOTAL_BYTES) {
      throw new Error(`operator message attachments exceed ${MAX_OPERATOR_ATTACHMENT_TOTAL_BYTES} bytes in total`);
    }
  }
  if (!payload.text && attachments.length === 0) {
    throw new Error("operator message payload must carry text or an attachment");
  }
  return {
    text: payload.text,
    message_id: messageId,
    attachments,
    ...(payload.replaces_message ? { replaces_message: require("./messageRevision.cjs").messageRevision(payload.replaces_message) } : {}),
    ...(clientContext ? { client_context: clientContext } : {}),
  };
}

function assertOperatorAttachmentInputLimits(inputs) {
  if (inputs.length > MAX_OPERATOR_ATTACHMENT_COUNT) {
    throw new Error(`operator message carries ${inputs.length} attachments; maximum is ${MAX_OPERATOR_ATTACHMENT_COUNT}`);
  }
  let totalBytes = 0;
  for (const input of inputs) {
    const name = requiredString(input?.name, "attachment.name");
    const size = nonNegativeInteger(input?.size, "attachment.size");
    if (size > MAX_OPERATOR_ATTACHMENT_BYTES) {
      throw new Error(`operator message attachment '${name}' exceeds ${MAX_OPERATOR_ATTACHMENT_BYTES} bytes`);
    }
    totalBytes += size;
    if (totalBytes > MAX_OPERATOR_ATTACHMENT_TOTAL_BYTES) {
      throw new Error(`operator message attachments exceed ${MAX_OPERATOR_ATTACHMENT_TOTAL_BYTES} bytes in total`);
    }
  }
}

async function verifyOperatorMessageAttachments(attachments) {
  for (const attachment of attachments) {
    const stat = await fs.lstat(attachment.payload_path);
    if (stat.isSymbolicLink() || !stat.isFile() || stat.size !== attachment.size) {
      throw new Error(`operator message attachment '${attachment.name}' differs from its durable envelope`);
    }
    if (await sha256File(attachment.payload_path) !== attachment.payload_content_hash) {
      throw new Error(`operator message attachment '${attachment.name}' differs from its durable envelope`);
    }
  }
}

function operatorMessagePayloadPath(payloadRoot, messageId) {
  return path.join(payloadRoot, `${safeFilename(safeContractToken(messageId, "messageId"))}.payload`);
}

function operatorMessageChatId(messageId) {
  return sha256Hex(Buffer.from(`operator-chat-run\0${safeContractToken(messageId, "messageId")}`)).slice(0, 32);
}

async function writeExactOperatorMessageFile(filePath, bytes, description) {
  const parent = path.dirname(filePath);
  const temporary = path.join(
    parent,
    `.${path.basename(filePath)}.${process.pid}-${randomToken(16)}.tmp`,
  );
  try {
    const handle = await fs.open(temporary, "wx", 0o600);
    try {
      await handle.writeFile(bytes);
      await handle.sync();
    } finally {
      await handle.close();
    }
    try {
      await fs.link(temporary, filePath);
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
      const existing = await fs.readFile(filePath);
      if (!existing.equals(bytes)) {
        throw new Error(`operator message ${description} '${filePath}' conflicts with its stable message identity`);
      }
    }
    await syncDirectory(parent);
  } finally {
    await fs.unlink(temporary).catch((error) => {
      if (error?.code !== "ENOENT") throw error;
    });
  }
  const existing = await fs.readFile(filePath);
  if (!existing.equals(bytes)) {
    throw new Error(`operator message ${description} '${filePath}' conflicts with its stable message identity`);
  }
}

async function sha256File(filePath) {
  const hash = crypto.createHash("sha256");
  await new Promise((resolve, reject) => {
    const stream = fsSync.createReadStream(filePath);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.once("error", reject);
    stream.once("end", resolve);
  });
  return hash.digest("hex");
}

async function planOperatorAttachments(root, inputs, durableAttachments) {
  if (durableAttachments !== undefined && durableAttachments.length !== inputs.length) {
    throw new Error("Attachment selection differs from its durable envelope.");
  }
  const plans = [];
  for (const [index, input] of inputs.entries()) {
    const name = requiredString(input?.name, "attachment.name");
    const sourcePath = absolutePath(input?.payloadPath, "attachment.payloadPath");
    const expectedHash = contentHashHex(input?.contentHash, "attachment.contentHash");
    const expectedSize = nonNegativeInteger(input?.size, "attachment.size");
    const source = await fs.lstat(sourcePath);
    if (source.isSymbolicLink() || !source.isFile() || source.size !== expectedSize) {
      throw new Error("Attachment stage no longer matches its declared size.");
    }
    const hash = expectedHash;
    if (await sha256File(sourcePath) !== hash) {
      throw new Error("Attachment stage no longer matches its declared content hash.");
    }
    const payloadPath = durableAttachments === undefined
      ? path.join(root, hash)
      : absolutePath(durableAttachments[index].payload_path, "attachment.payload_path");
    if (!pathIsWithin(root, payloadPath)) throw new Error("Attachment durable path leaves its owned root.");
    plans.push({
      sourcePath,
      attachment: {
        name,
        label: requiredString(input?.label, "attachment.label"),
        mime_type: stringValue(input.type) || "application/octet-stream",
        size: expectedSize,
        payload_path: payloadPath,
        payload_content_hash: hash,
        last_modified: nonNegativeInteger(input?.lastModified, "attachment.lastModified"),
      },
    });
  }
  return plans;
}

async function materializeAttachments(plans) {
  for (const plan of plans) {
    const { attachment, sourcePath } = plan;
    const expectedHash = attachment.payload_content_hash;
    const expectedSize = attachment.size;
    const payloadPath = attachment.payload_path;
    const root = path.dirname(payloadPath);
    const existing = await fs.lstat(payloadPath).catch((error) => {
      if (error?.code === "ENOENT") return null;
      throw error;
    });
    if (existing) {
      if (existing.isSymbolicLink() || !existing.isFile() || existing.size !== expectedSize || await sha256File(payloadPath) !== expectedHash) {
        throw new Error(`Durable attachment '${payloadPath}' conflicts with its content address.`);
      }
      await syncDirectory(root);
      continue;
    }
    const temporary = path.join(
      root,
      `.${path.basename(payloadPath)}.${process.pid}-${randomToken(16)}.tmp`,
    );
    try {
      await fs.copyFile(sourcePath, temporary, fsSync.constants.COPYFILE_EXCL);
      await fs.chmod(temporary, 0o600);
      const copied = await fs.lstat(temporary);
      if (copied.isSymbolicLink() || !copied.isFile() || copied.size !== expectedSize || await sha256File(temporary) !== expectedHash) {
        throw new Error("Attachment stage changed while it was being made durable.");
      }
      const handle = await fs.open(temporary, "r");
      try {
        await handle.sync();
      } finally {
        await handle.close();
      }
      try {
        await fs.link(temporary, payloadPath);
      } catch (error) {
        if (error?.code !== "EEXIST") throw error;
        const existing = await fs.lstat(payloadPath);
        if (existing.isSymbolicLink() || !existing.isFile() || existing.size !== expectedSize || await sha256File(payloadPath) !== expectedHash) {
          throw new Error(`Durable attachment '${payloadPath}' conflicts with its content address.`);
        }
      }
      await syncDirectory(root);
    } finally {
      await fs.unlink(temporary).catch((error) => {
        if (error?.code !== "ENOENT") throw error;
      });
    }
  }
}

async function writeContentAddressedPayload(root, label, bytes) {
  const file = path.join(root, `${safeFilename(label)}-${sha256Hex(bytes)}.payload`);
  await writeExactOperatorMessageFile(file, bytes, "content-addressed payload");
  return file;
}

async function resolveRunRecordsRoot(octopusHome) {
  const hostPath = path.join(octopusHome, "host.json");
  const host = await readJsonIfPresent(hostPath);
  const configured = stringValue(host?.root_refs?.run_records_root);
  const root = configured
    || path.join(octopusHome, "state", "runs");
  return absolutePath(root, "run_records_root");
}
