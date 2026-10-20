module.exports = { admitOperatorMessage, assertExactOperatorMessagePayload, findPreparedOperatorMessage, operatorMessageAdmissionExpected, operatorMessageAdmissionIdentity, operatorMessageAdmissionPath, operatorMessageAdmissionRecord, operatorMessageContinuationIntent, operatorMessageMovement, operatorMessageRequestDigest, prepareOperatorMessageAdmission, readExistingOperatorMessageEnvelope, readOrCreateOperatorMessageAdmission, readOrCreateOperatorMessageIntent, recoverOperatorMessageAdmission, validateAdmittedOperatorMessage, withOperatorMessageAdmissionLock, writeOperatorMessageAdmission };

const fs = require("node:fs/promises");
const path = require("node:path");
const { readCompleteLedgerTail, readLedgerMovementAtSeq } = require("./chatArtifacts.cjs");
const { OPERATOR_MESSAGE_ADMISSION_SCHEMA, OPERATOR_MESSAGE_INTENT_SCHEMA, operatorMessageAdmissionLocks } = require("./constants.cjs");
const { assertOperatorMessageRequestMatchesEnvelope, materializeAttachments, operatorMessageEnvelopeFromBytes, operatorMessagePayloadPath, planOperatorAttachments, readDurableOperatorMessageEnvelope, verifyOperatorMessageAttachments, writeExactOperatorMessageFile } = require("./operatorMessages.cjs");
const { commitClientWrites } = require("./publication.cjs");
const { canonicalJson, interfaceEndpoint, operatorContextArtifactBindings, operatorDoorHostSlots, operatorExternalInputs, operatorExternalRelays, operatorModelOutputs, operatorPayloadArtifactBindings, operatorSelectedTools, operatorSelectedWorkerEnvironmentKinds, operatorSurfaceRole, safeContractToken, safeSha256 } = require("./topologyPlan.cjs");
const { absolutePath, artifactIdHasLabel, assertAdmitted, contentHashHex, exactArrivalRuntime, isObject, nonNegativeInteger, optionalServiceTier, readJsonDocumentIfPresent, requiredFunction, requiredObject, requiredString, safeSegment, sha256Hex, stringValue } = require("./values.cjs");

async function readExistingOperatorMessageEnvelope(options, input) {
  const admissionPath = operatorMessageAdmissionPath(input);
  const document = await readJsonDocumentIfPresent(admissionPath);
  if (!document) {
    const messagePath = operatorMessagePayloadPath(
      path.join(input.artifactRoot, "payloads"),
      input.messageId,
    );
    const messageBytes = await fs.readFile(messagePath).catch((error) => {
      if (error?.code === "ENOENT") return null;
      throw error;
    });
    if (!messageBytes) return null;
    const envelope = operatorMessageEnvelopeFromBytes({
      attachmentRoot: input.attachmentRoot,
      messageBytes,
      messageId: input.messageId,
      messagePath,
    });
    assertOperatorMessageRequestMatchesEnvelope(input, envelope.payload);
    if (input.attachmentInputs.length > 0) {
      await materializeAttachments(await planOperatorAttachments(
        input.attachmentRoot,
        input.attachmentInputs,
        envelope.payload.attachments,
      ));
    }
    await verifyOperatorMessageAttachments(envelope.payload.attachments);
    return { ...envelope, recovered: null };
  }
  if (!input.runDocumentPresent) {
    throw new Error(`operator message '${input.messageId}' admission has no authored run`);
  }
  const admission = operatorMessageAdmissionRecord(
    document.value,
    operatorMessageAdmissionIdentity(input),
  );
  const expectedMessagePath = operatorMessagePayloadPath(
    path.join(input.artifactRoot, "payloads"),
    input.messageId,
  );
  if (admission.payload_path !== expectedMessagePath) {
    throw new Error(`operator message '${input.messageId}' admission points outside its stable envelope`);
  }
  const envelope = await readDurableOperatorMessageEnvelope({
    admission,
    attachmentRoot: input.attachmentRoot,
    messageId: input.messageId,
  });
  assertOperatorMessageRequestMatchesEnvelope(input, envelope.payload);
  if (input.attachmentInputs.length > 0) {
    await materializeAttachments(await planOperatorAttachments(
      input.attachmentRoot,
      input.attachmentInputs,
      envelope.payload.attachments,
    ));
  }
  await verifyOperatorMessageAttachments(envelope.payload.attachments);
  const recoveryInput = {
    ...input,
    messageBytes: envelope.messageBytes,
    messagePath: envelope.messagePath,
  };
  return {
    ...envelope,
    recovered: await recoverOperatorMessageAdmission(
      options,
      recoveryInput,
      admissionPath,
      document,
      admission,
    ),
  };
}

async function prepareOperatorMessageAdmission(options, input) {
  const state = await readOrCreateOperatorMessageAdmission(options, input);
  return recoverOperatorMessageAdmission(
    options,
    input,
    state.admissionPath,
    state.document,
    state.admission,
  );
}

async function admitOperatorMessage(options, input) {
  const state = await readOrCreateOperatorMessageAdmission(options, input);
  let { admission, document } = state;
  const { admissionPath } = state;
  const recovered = await recoverOperatorMessageAdmission(
    options,
    input,
    admissionPath,
    document,
    admission,
  );
  if (recovered) return recovered;

  const verdict = await requiredFunction(options.handInArtifact, "handInArtifact")({
    endpoint: interfaceEndpoint(input.octopusHome, input.chatInterfaceId),
    interfaceId: input.chatInterfaceId,
    manifest: {
      label: input.label,
      payload_path: input.messagePath,
    },
    timeoutMs: input.timeoutMs,
  });
  assertAdmitted(verdict, input.label);
  const movementSeq = nonNegativeInteger(
    verdict.admitted.seq,
    "operator message admission seq",
  );
  admission = { ...admission, phase: "admitted", movement_seq: movementSeq };
  await writeOperatorMessageAdmission(options, admissionPath, document, admission);
  return {
    outboundCursorOffset: admission.outbound_cursor_offset,
    recovered: false,
    verdict,
  };
}

async function readOrCreateOperatorMessageAdmission(options, input) {
  const admissionRoot = path.join(input.artifactRoot, "message-admissions");
  await fs.mkdir(admissionRoot, { recursive: true, mode: 0o700 });
  await fs.chmod(admissionRoot, 0o700);
  const admissionPath = operatorMessageAdmissionPath(input);
  const expected = operatorMessageAdmissionExpected(input);
  let document = await readJsonDocumentIfPresent(admissionPath);
  let admission;
  if (document) {
    admission = operatorMessageAdmissionRecord(document.value, expected);
  } else {
    admission = {
      schema: OPERATOR_MESSAGE_ADMISSION_SCHEMA,
      schema_version: 1,
      phase: "prepared",
      ...expected,
      outbound_cursor_offset: nonNegativeInteger(
        input.outboundCursorOffset,
        "operator message outbound cursor offset",
      ),
      movement_seq: null,
    };
    document = await writeOperatorMessageAdmission(options, admissionPath, null, admission);
  }
  return { admissionPath, document, admission };
}

async function readOrCreateOperatorMessageIntent(input) {
  const intentRoot = path.join(input.artifactRoot, "message-intents");
  await fs.mkdir(intentRoot, { recursive: true, mode: 0o700 });
  await fs.chmod(intentRoot, 0o700);
  const intentPath = path.join(intentRoot, `${input.messageId}.json`);
  const expected = {
    schema: OPERATOR_MESSAGE_INTENT_SCHEMA,
    schema_version: 1,
    interface_id: input.chatInterfaceId,
    message_id: input.messageId,
    message_payload_sha256: sha256Hex(input.messageBytes),
    request_sha256: safeSha256(input.requestDigest, "operator message request digest"),
    run_id: input.runId,
    ...(input.sourceRequestDigest !== undefined ? {
      source_request_sha256: safeSha256(input.sourceRequestDigest, "operator source request digest"),
      payload_artifact_bindings: operatorPayloadArtifactBindings(input.payloadArtifactBindings),
    } : {}),
  };
  const bytes = Buffer.from(`${JSON.stringify(expected, null, 2)}\n`);
  await writeExactOperatorMessageFile(intentPath, bytes, "intent");
  let value;
  try {
    value = JSON.parse((await fs.readFile(intentPath)).toString("utf8"));
  } catch (error) {
    throw new Error(`operator message intent is malformed JSON: ${error.message}`);
  }
  const record = requiredObject(value, "operator message intent");
  if (
    Object.keys(record).length !== Object.keys(expected).length
    || Object.keys(expected).some((field) => canonicalJson(record[field]) !== canonicalJson(expected[field]))
  ) {
    throw new Error(`operator message '${input.messageId}' conflicts with its durable submission intent`);
  }
  return intentPath;
}

function operatorMessageAdmissionPath(input) {
  return path.join(input.artifactRoot, "message-admissions", `${input.messageId}.json`);
}

function operatorMessageRequestDigest(config, request, resolved) {
  const continuation = request?.continuation === undefined || request?.continuation === null
    ? null
    : operatorMessageContinuationIntent(request.continuation);
  const identity = {
    client_surface_role: resolved.clientSurfaceRole,
    context_artifact_bindings: operatorContextArtifactBindings(request?.contextArtifactBindings),
    ...(request?.payloadArtifactBindings !== undefined
      ? { payload_artifact_bindings: operatorPayloadArtifactBindings(request.payloadArtifactBindings) }
      : {}),
    continuation,
    door_host_slots: operatorDoorHostSlots(request?.doorHostSlots),
    ee_id: stringValue(request?.eeId) || requiredString(config.run.ee, "client.run.ee"),
    external_inputs: operatorExternalInputs(request?.externalInputs),
    external_relays: operatorExternalRelays(request?.externalRelays),
    model_outputs: operatorModelOutputs(request?.modelOutputs),
    provider_interface_id: resolved.providerReference,
    provider_model: requiredString(request?.providerModel, "providerModel"),
    reasoning_effort: stringValue(request?.reasoningEffort),
    ...(optionalServiceTier(request?.serviceTier) ? { service_tier: request.serviceTier } : {}),
    selected_tools: operatorSelectedTools(request?.selectedTools),
    selected_worker_environment_kinds: operatorSelectedWorkerEnvironmentKinds(
      request?.selectedWorkerEnvironmentKinds,
    ),
    worker_interface_id: stringValue(request?.workerInterfaceId)
      || requiredString(config.run.worker, "client.run.worker"),
  };
  return sha256Hex(Buffer.from(canonicalJson(identity)));
}

function operatorMessageContinuationIntent(value) {
  const continuation = requiredObject(value, "continuation");
  const fields = ["sourceRunId", "sourceArtifactId"];
  if (
    Object.keys(continuation).length !== fields.length
    || fields.some((field) => !Object.hasOwn(continuation, field))
  ) {
    throw new Error("continuation has an invalid field set");
  }
  return {
    source_artifact_id: requiredString(
      continuation.sourceArtifactId,
      "continuation.sourceArtifactId",
    ),
    source_run_id: safeSegment(continuation.sourceRunId, "continuation.sourceRunId"),
  };
}

function operatorMessageAdmissionExpected(input) {
  return {
    ...operatorMessageAdmissionIdentity(input),
    payload_path: input.messagePath,
    payload_sha256: sha256Hex(input.messageBytes),
  };
}

function operatorMessageAdmissionIdentity(input) {
  return {
    artifact_label: input.label,
    client_surface_role: input.clientSurfaceRole,
    interface_id: input.chatInterfaceId,
    message_id: input.messageId,
    model_interface_id: input.modelInterfaceId,
    run_id: input.runId,
    worker_interface_id: input.workerInterfaceId,
  };
}

async function recoverOperatorMessageAdmission(options, input, admissionPath, document, admission) {
  const recovered = admission.phase === "admitted"
    ? await validateAdmittedOperatorMessage(options, input, admission)
    : await findPreparedOperatorMessage(options, input, admission);
  if (recovered === null) return null;
  if (admission.phase === "prepared") {
    admission = { ...admission, phase: "admitted", movement_seq: recovered };
    await writeOperatorMessageAdmission(options, admissionPath, document, admission);
  }
  return {
    outboundCursorOffset: admission.outbound_cursor_offset,
    recovered: true,
    verdict: { admitted: { seq: recovered } },
  };
}

async function validateAdmittedOperatorMessage(options, input, admission) {
  const movement = await readLedgerMovementAtSeq(
    path.join(input.octopusHome, "ledger", "movements.jsonl"),
    admission.movement_seq,
  );
  const resolved = await operatorMessageMovement(options, input, movement);
  if (!resolved || resolved.messageId !== input.messageId) {
    throw new Error("operator message admission journal points at a different ledger movement");
  }
  assertExactOperatorMessagePayload(input, resolved);
  return admission.movement_seq;
}

async function findPreparedOperatorMessage(options, input, admission) {
  const movementsPath = path.join(input.octopusHome, "ledger", "movements.jsonl");
  const tail = await readCompleteLedgerTail(
    movementsPath,
    admission.outbound_cursor_offset,
  );
  const exact = [];
  let otherOperatorMessages = 0;
  let expectedSeq = null;
  for (let index = 0; index < tail.records.length; index += 1) {
    let movement;
    try {
      movement = JSON.parse(tail.records[index].text);
    } catch (error) {
      throw new Error(`movement ledger contains malformed complete JSON at recovery line ${index + 1}: ${error.message}`);
    }
    const seq = nonNegativeInteger(movement?.seq, "movement seq");
    if (expectedSeq !== null && seq !== expectedSeq) {
      throw new Error(`movement ledger is non-contiguous at seq ${seq}; expected ${expectedSeq}`);
    }
    expectedSeq = seq + 1;
    const resolved = await operatorMessageMovement(options, input, movement);
    if (!resolved) continue;
    if (resolved.messageId !== input.messageId) {
      otherOperatorMessages += 1;
      continue;
    }
    assertExactOperatorMessagePayload(input, resolved);
    exact.push({
      runRecordHash: resolved.runRecordHash,
      seq,
    });
  }
  const exactGenerations = new Set(exact.map((entry) => entry.runRecordHash));
  if (exactGenerations.size > 1) {
    throw new Error(`operator message '${input.messageId}' was admitted in multiple run-record generations`);
  }
  if (exact.length > 0) return Math.min(...exact.map((entry) => entry.seq));
  if (otherOperatorMessages > 0) {
    throw new Error(`prepared operator message '${input.messageId}' was superseded by another operator message`);
  }
  return null;
}

async function operatorMessageMovement(options, input, movement) {
  const runtime = exactArrivalRuntime(movement, input.chatInterfaceId);
  if (runtime?.run !== input.runId) return null;
  const artifact = requiredObject(movement.artifact, "operator message movement artifact");
  const artifactId = requiredString(artifact.id, "operator message artifact id");
  if (!artifactIdHasLabel(artifactId, input.label)) return null;
  const manifestHash = contentHashHex(
    artifact.content,
    "operator message artifact content",
  );
  if (artifactId !== `${input.label}:${manifestHash}`) {
    throw new Error("operator message artifact identity does not match its content hash");
  }
  if (runtime.worker !== input.workerInterfaceId) {
    throw new Error("operator message movement has the wrong exact runtime coordinates");
  }
  const resolved = await requiredFunction(
    options.readArtifactPayload,
    "readArtifactPayload",
  )({
    artifactId,
    manifestHash,
    label: input.label,
  });
  if (
    !isObject(resolved)
    || resolved.label !== input.label
    || contentHashHex(resolved.manifestHash, "resolved operator message manifest hash") !== manifestHash
  ) {
    throw new Error("resolved operator message differs from its ledger identity");
  }
  const payload = requiredObject(resolved.payload, "operator message payload");
  if (!Buffer.isBuffer(resolved.payloadBytes)) {
    throw new Error("resolved operator message is missing its exact payload bytes");
  }
  return {
    messageId: safeContractToken(payload.message_id, "operator message payload.message_id"),
    payload,
    payloadBytes: resolved.payloadBytes,
    payloadHash: contentHashHex(
      resolved.payloadHash,
      "resolved operator message payload hash",
    ),
    payloadPath: absolutePath(resolved.payloadPath, "resolved operator message payloadPath"),
    runRecordHash: contentHashHex(
      runtime.run_record_hash,
      "operator message runtime.run_record_hash",
    ),
  };
}

function assertExactOperatorMessagePayload(input, resolved) {
  let expectedPayload;
  try {
    expectedPayload = JSON.parse(input.messageBytes.toString("utf8"));
  } catch (error) {
    throw new Error(`operator message bytes are malformed JSON: ${error.message}`);
  }
  if (
    resolved.payloadPath !== input.messagePath
    || !resolved.payloadBytes.equals(input.messageBytes)
    || canonicalJson(resolved.payload) !== canonicalJson(expectedPayload)
  ) {
    throw new Error(`operator message '${input.messageId}' conflicts with its admitted payload`);
  }
}

function operatorMessageAdmissionRecord(value, expected) {
  const record = requiredObject(value, "operator message admission journal");
  const fields = [
    "schema",
    "schema_version",
    "phase",
    "artifact_label",
    "client_surface_role",
    "interface_id",
    "message_id",
    "model_interface_id",
    "payload_path",
    "payload_sha256",
    "run_id",
    "worker_interface_id",
    "outbound_cursor_offset",
    "movement_seq",
  ];
  if (
    Object.keys(record).length !== fields.length
    || fields.some((field) => !Object.hasOwn(record, field))
  ) {
    throw new Error("operator message admission journal has an invalid field set");
  }
  if (record.schema !== OPERATOR_MESSAGE_ADMISSION_SCHEMA || record.schema_version !== 1) {
    throw new Error("operator message admission journal has an unsupported schema");
  }
  if (record.phase !== "prepared" && record.phase !== "admitted") {
    throw new Error("operator message admission journal has an invalid phase");
  }
  const normalized = {
    schema: OPERATOR_MESSAGE_ADMISSION_SCHEMA,
    schema_version: 1,
    phase: record.phase,
    artifact_label: requiredString(record.artifact_label, "admission artifact_label"),
    client_surface_role: operatorSurfaceRole(record.client_surface_role),
    interface_id: safeSegment(record.interface_id, "admission interface_id"),
    message_id: safeContractToken(record.message_id, "admission message_id"),
    model_interface_id: safeSegment(record.model_interface_id, "admission model_interface_id"),
    payload_path: absolutePath(record.payload_path, "admission payload_path"),
    payload_sha256: contentHashHex(record.payload_sha256, "admission payload_sha256"),
    run_id: safeSegment(record.run_id, "admission run_id"),
    worker_interface_id: safeSegment(record.worker_interface_id, "admission worker_interface_id"),
    outbound_cursor_offset: nonNegativeInteger(
      record.outbound_cursor_offset,
      "admission outbound_cursor_offset",
    ),
    movement_seq: record.phase === "prepared"
      ? record.movement_seq
      : nonNegativeInteger(record.movement_seq, "admission movement_seq"),
  };
  if (normalized.phase === "prepared" && normalized.movement_seq !== null) {
    throw new Error("prepared operator message admission must not carry a movement seq");
  }
  for (const [field, expectedValue] of Object.entries(expected)) {
    if (normalized[field] !== expectedValue) {
      throw new Error(`operator message admission journal conflicts on '${field}'`);
    }
  }
  return normalized;
}

async function writeOperatorMessageAdmission(options, filePath, prior, value) {
  const text = `${JSON.stringify(value, null, 2)}\n`;
  await commitClientWrites([{
    filePath,
    content: text,
    tag: `operator-message-admission:${value.message_id}`,
    expectedBeforeHash: prior ? sha256Hex(Buffer.from(prior.text)) : null,
  }], options);
  return { text, value };
}

async function withOperatorMessageAdmissionLock(interfaceId, messageId, operation) {
  const key = `${safeSegment(interfaceId, "interfaceId")}\0${safeContractToken(messageId, "messageId")}`;
  const previous = operatorMessageAdmissionLocks.get(key) ?? Promise.resolve();
  let release;
  const current = new Promise((resolve) => { release = resolve; });
  const tail = previous.then(() => current);
  operatorMessageAdmissionLocks.set(key, tail);
  await previous;
  try {
    return await operation();
  } finally {
    release();
    if (operatorMessageAdmissionLocks.get(key) === tail) {
      operatorMessageAdmissionLocks.delete(key);
    }
  }
}
