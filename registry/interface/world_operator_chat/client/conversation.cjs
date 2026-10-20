module.exports = { activeOperatorMessageCheckpoint, assertRunOwnsArtifact, authoredInterfaceBindingId, chatIdentityFromRunRecord, conversationArrivalKey, conversationFoldPath, emptyConversationFold, existingRunSubmission, existingRunTopology, foldConversationLedger, operatorChatLiveContext, readConversationState, resolveContinuationState, uniqueNonEmptyStrings, usableConversationFold, writeConversationFold };

const fs = require("node:fs/promises");
const path = require("node:path");
const { readExactInterfaceRole } = require("./artifactBatches.cjs");
const { readCompleteLedgerEndOffset, readCompleteLedgerTail } = require("./chatArtifacts.cjs");
const { resolveRunRecordsRoot } = require("./operatorMessages.cjs");
const { normalizePinnedContextItems } = require("./pinnedContext.cjs");
const { artifactBindingAdmitsLabel } = require("./providerRoutes.cjs");
const { uniqueArtifactBindings } = require("./publication.cjs");
const { artifactIdHasLabel, exactArrivalRuntime, exactOutboundRuntime, isObject, nonNegativeInteger, randomToken, readJsonDocumentIfPresent, readJsonIfPresent, requiredArray, requiredObject, requiredString, safeSegment, stringValue } = require("./values.cjs");

function chatIdentityFromRunRecord(config, runId, record) {
  if (!isObject(record) || requiredString(record.id, "operator chat run.id") !== runId) {
    throw new Error(`operator chat run '${runId}' has invalid record identity`);
  }
  const candidates = Object.entries(isObject(record.interface_state) ? record.interface_state : {})
    .filter(([, state]) => isObject(state) && ["home", "task"].includes(state.client_surface_role));
  if (candidates.length !== 1) {
    throw new Error(`operator chat run '${runId}' must contain exactly one client interface state`);
  }
  const [interfaceId] = candidates[0];
  const interfacePrefix = requiredString(config.run.interface_prefix, "client.run.interface_prefix");
  if (!interfaceId.startsWith(interfacePrefix)) {
    throw new Error(`operator chat run '${runId}' client interface '${interfaceId}' is outside this client`);
  }
  const chatId = safeSegment(interfaceId.slice(interfacePrefix.length), "client interface chat key");
  return {
    chatId,
    interfaceId,
  };
}

async function operatorChatLiveContext(config, octopusHome, runId, interfaceId) {
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const record = await readJsonIfPresent(path.join(runsDir, `${runId}.json`));
  if (!record) throw new Error(`operator chat run '${runId}' does not exist`);
  const identity = chatIdentityFromRunRecord(config, runId, record);
  if (identity.interfaceId !== interfaceId) {
    throw new Error(`operator chat run '${runId}' changed its exact client interface`);
  }
  const clientState = requiredObject(
    record.interface_state?.[interfaceId],
    `operator chat run '${runId}' client interface state`,
  );
  const providerInterfaceId = safeSegment(
    clientState.provider_runtime_interface,
    `operator chat run '${runId}' provider_runtime_interface`,
  );
  if (!isObject(record.interface_state?.[providerInterfaceId])) {
    throw new Error(`operator chat run '${runId}' has no state for provider '${providerInterfaceId}'`);
  }
  const providerRole = await readExactInterfaceRole(octopusHome, providerInterfaceId);
  if (providerRole !== "provider") {
    throw new Error(`operator chat run '${runId}' provider '${providerInterfaceId}' is not a provider interface`);
  }
  return {
    providerInterfaceId,
    providerRole,
    streamsRoot: path.join(octopusHome, "interfaces", providerInterfaceId, "streams"),
  };
}

function existingRunSubmission(
  config,
  runId,
  record,
  expectedClientInterfaceId,
  requested,
) {
  const submission = existingRunTopology(
    config,
    runId,
    record,
    expectedClientInterfaceId,
    requested,
  );
  const providerInterfaceId = submission.modelInterfaceId;
  const identity = chatIdentityFromRunRecord(config, runId, record);
  const activeIds = uniqueNonEmptyStrings(
    requiredArray(record.active_checkpoints, "active_checkpoints"),
    "active checkpoint",
  );
  const checkpoints = requiredArray(record.checkpoints, "checkpoints");
  const checkpointsById = new Map();
  for (const checkpoint of checkpoints) {
    requiredObject(checkpoint, "checkpoint");
    const checkpointId = requiredString(checkpoint.id, "checkpoint.id");
    if (checkpointsById.has(checkpointId)) {
      throw new Error(`operator chat run '${runId}' repeats checkpoint '${checkpointId}'`);
    }
    checkpointsById.set(checkpointId, checkpoint);
  }
  const activeClientInputs = [];
  for (const checkpointId of activeIds) {
    const checkpoint = checkpointsById.get(checkpointId);
    if (!checkpoint) {
      throw new Error(`operator chat run '${runId}' active checkpoint '${checkpointId}' is absent`);
    }
    if (
      checkpoint.interface_binding !== undefined
      && checkpoint.interface_binding !== null
      && authoredInterfaceBindingId(checkpoint.interface_binding) === identity.interfaceId
    ) {
      activeClientInputs.push(checkpoint);
    }
  }
  if (activeClientInputs.length === 0) {
    throw new Error(`operator chat run '${runId}' has no active exact client input`);
  }
  for (const checkpoint of activeClientInputs) {
    const outboundInterfaces = requiredArray(
      checkpoint.outbound_interface_bindings,
      "client input outbound bindings",
    ).map(authoredInterfaceBindingId);
    if (
      checkpoint.run !== runId
      || outboundInterfaces.length !== 1
      || outboundInterfaces[0] !== providerInterfaceId
    ) {
      throw new Error(`operator chat run '${runId}' active client input differs from its provider route`);
    }
  }
  return submission;
}

function activeOperatorMessageCheckpoint(
  record,
  runId,
  clientInterfaceId,
  providerInterfaceId,
  messageArtifactLabel,
) {
  const activeIds = new Set(requiredArray(record.active_checkpoints, "active_checkpoints"));
  const candidates = requiredArray(record.checkpoints, "checkpoints").filter((checkpoint) => (
    isObject(checkpoint)
    && activeIds.has(checkpoint.id)
    && checkpoint.run === runId
    && authoredInterfaceBindingId(checkpoint.interface_binding) === clientInterfaceId
    && artifactBindingAdmitsLabel(checkpoint.interface_binding, messageArtifactLabel)
  ));
  if (candidates.length !== 1) {
    throw new Error(`operator chat run '${runId}' must have one active operator message checkpoint`);
  }
  const checkpoint = candidates[0];
  const outbound = requiredArray(
    checkpoint.outbound_interface_bindings,
    "operator message outbound bindings",
  );
  if (
    outbound.length !== 1
    || authoredInterfaceBindingId(outbound[0]) !== providerInterfaceId
    || !artifactBindingAdmitsLabel(outbound[0], messageArtifactLabel)
  ) {
    throw new Error(`operator chat run '${runId}' operator message checkpoint differs from its provider route`);
  }
  return {
    id: requiredString(checkpoint.id, "operator message checkpoint.id"),
    epoch: nonNegativeInteger(checkpoint.epoch, "operator message checkpoint.epoch"),
  };
}

function existingRunTopology(
  config,
  runId,
  record,
  expectedClientInterfaceId,
  requested,
) {
  const identity = chatIdentityFromRunRecord(config, runId, record);
  if (identity.interfaceId !== expectedClientInterfaceId) {
    throw new Error(`operator chat run '${runId}' changed its exact client interface`);
  }
  const clientState = record.interface_state[identity.interfaceId];
  const providerInterfaceId = requiredString(
    clientState.provider_runtime_interface,
    "provider_runtime_interface",
  );
  const providerState = record.interface_state[providerInterfaceId];
  if (!isObject(providerState)) {
    throw new Error(`operator chat run '${runId}' has no state for provider '${providerInterfaceId}'`);
  }
  const runWorkerInterfaceId = requiredString(record.worker, `operator chat run '${runId}' worker`);
  const runEeId = requiredString(record.ee, `operator chat run '${runId}' ee`);
  const providerChoiceInterfaceId = requiredString(
    clientState.provider_choice_interface,
    `operator chat run '${runId}' provider choice`,
  );
  const workerChoiceInterfaceId = requiredString(
    clientState.worker_choice_interface,
    `operator chat run '${runId}' worker choice`,
  );
  const clientSurfaceRole = requiredString(
    clientState.client_surface_role,
    `operator chat run '${runId}' client surface role`,
  );
  if (
    requested.providerChoiceInterfaceId !== providerChoiceInterfaceId
    || requested.clientSurfaceRole !== clientSurfaceRole
    || (requested.eeId && requested.eeId !== runEeId)
    || (
      requested.workerInterfaceId
      && requested.workerInterfaceId !== workerChoiceInterfaceId
      && requested.workerInterfaceId !== runWorkerInterfaceId
    )
  ) {
    throw new Error(`operator chat run '${runId}' conflicts with requested topology`);
  }
  return {
    modelInterfaceId: providerInterfaceId,
    workerInterfaceId: runWorkerInterfaceId,
    clientSurfaceRole,
  };
}

function uniqueNonEmptyStrings(values, field) {
  const output = new Set();
  for (const value of values) {
    const id = requiredString(value, field);
    if (output.has(id)) throw new Error(`${field} repeats '${id}'`);
    output.add(id);
  }
  if (output.size === 0) throw new Error(`${field} must not be empty`);
  return output;
}

function authoredInterfaceBindingId(value) {
  const binding = requiredObject(value, "authored interface binding");
  const interfaceId = requiredString(binding.interface, "authored interface binding.interface");
  const config = requiredObject(binding.config, "authored interface binding.config");
  const artifactPolicy = requiredObject(
    config.artifact_policy,
    "authored interface binding.config.artifact_policy",
  );
  if (artifactPolicy.mode !== "all" && artifactPolicy.mode !== "allow") {
    throw new Error("authored interface binding has unsupported artifact policy");
  }
  return interfaceId;
}

async function resolveContinuationState({ config, octopusHome, requestedRunId, request }) {
  if (request === undefined || request === null) return null;
  if (requestedRunId) throw new Error("continuation is only valid when creating a new run");
  if (!isObject(request)) throw new Error("continuation must be an object");
  const unknown = Object.keys(request).filter((key) => ![
    "sourceRunId",
    "sourceArtifactId",
  ].includes(key));
  if (unknown.length > 0) throw new Error(`continuation contains unknown fields: ${unknown.join(", ")}`);
  const sourceRunId = safeSegment(request.sourceRunId, "continuation.sourceRunId");
  const sourceArtifactId = requiredString(request.sourceArtifactId, "continuation.sourceArtifactId");
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const sourceDocument = await readJsonDocumentIfPresent(path.join(runsDir, `${sourceRunId}.json`));
  if (!sourceDocument) throw new Error(`continuation source run '${sourceRunId}' does not exist`);
  const sourceIdentity = chatIdentityFromRunRecord(config, sourceRunId, sourceDocument.value);
  await assertRunOwnsArtifact({
    octopusHome,
    runId: sourceRunId,
    interfaceId: sourceIdentity.interfaceId,
    artifactId: sourceArtifactId,
    outboundOnly: true,
    allowedArtifactLabels: [
      config.artifact_labels.terminal_output,
      config.artifact_labels.error_output,
    ],
  });
  const sourceState = isObject(sourceDocument.value.interface_state?.[sourceIdentity.interfaceId])
    ? sourceDocument.value.interface_state[sourceIdentity.interfaceId]
    : {};
  const pinnedArtifactBindings = sourceState.pinned_artifact_bindings === undefined
    ? []
    : uniqueArtifactBindings(requiredArray(
      sourceState.pinned_artifact_bindings,
      "continuation source pinned_artifact_bindings",
    ));
  const pinnedContextItems = normalizePinnedContextItems(sourceState.pinned_context_items);
  return {
    artifactBindings: [{ id: sourceArtifactId }],
    pinnedArtifactBindings,
    pinnedContextItems,
    continuedFrom: {
      run_id: sourceRunId,
      artifact_id: sourceArtifactId,
    },
  };
}

async function assertRunOwnsArtifact({
  octopusHome,
  runId,
  interfaceId,
  artifactId,
  outboundOnly = false,
  allowedArtifactLabels = [],
}) {
  if (allowedArtifactLabels.length > 0
    && !allowedArtifactLabels.some((label) => artifactIdHasLabel(artifactId, label))) {
    throw new Error(`artifact '${artifactId}' is not a terminal operator chat snapshot`);
  }
  const movementsPath = path.join(octopusHome, "ledger", "movements.jsonl");
  const ledger = await readCompleteLedgerTail(movementsPath, 0);
  for (let index = ledger.lines.length - 1; index >= 0; index -= 1) {
    let movement;
    try {
      movement = JSON.parse(ledger.lines[index]);
    } catch (error) {
      throw new Error(`movement ledger contains malformed complete JSON at line ${index + 1}: ${error.message}`);
    }
    if (stringValue(movement?.artifact?.id) !== artifactId) continue;
    const outboundRuntime = exactOutboundRuntime(movement, interfaceId);
    const outbound = outboundRuntime?.run === runId;
    const inbound = !outboundOnly
      && exactArrivalRuntime(movement, interfaceId)?.run === runId;
    if (outbound || inbound) return;
  }
  throw new Error(`artifact '${artifactId}' is not a visible message of operator chat run '${runId}'`);
}

async function readConversationState({
  octopusHome,
  existing,
  runId,
  interfaceId,
  terminalArtifactLabel,
  errorArtifactLabel,
  operatorMessageArtifactLabel,
  operatorMessageCheckpoint,
  continuation,
}) {
  const priorState = isObject(existing?.interface_state?.[interfaceId])
    ? existing.interface_state[interfaceId]
    : {};
  const priorOffset = priorState.ledger_offset === undefined
    ? 0
    : nonNegativeInteger(priorState.ledger_offset, "interface_state ledger_offset");
  const priorBindings = priorState.conversation_artifact_bindings === undefined
    ? []
    : requiredArray(
      priorState.conversation_artifact_bindings,
      "interface_state conversation_artifact_bindings",
    );
  const pinnedArtifactBindings = priorState.pinned_artifact_bindings === undefined
    ? continuation?.pinnedArtifactBindings ?? []
    : uniqueArtifactBindings(requiredArray(
      priorState.pinned_artifact_bindings,
      "interface_state pinned_artifact_bindings",
    ));
  const pinnedContextItems = priorState.pinned_context_items === undefined
    ? continuation?.pinnedContextItems ?? []
    : normalizePinnedContextItems(priorState.pinned_context_items);
  const continuedFrom = priorState.continued_from === undefined
    ? continuation?.continuedFrom ?? null
    : priorState.continued_from;
  const movementsPath = path.join(octopusHome, "ledger", "movements.jsonl");
  if (!existing) {
    const retainedSnapshot = continuation?.artifactBindings?.at(-1) ?? null;
    return {
      ledgerOffset: await readCompleteLedgerEndOffset(movementsPath),
      artifactBindings: retainedSnapshot ? uniqueArtifactBindings([retainedSnapshot]) : [],
      pinnedArtifactBindings,
      pinnedContextItems,
      continuedFrom,
    };
  }
  const fold = await foldConversationLedger({
    octopusHome,
    movementsPath,
    runId,
    interfaceId,
    priorOffset,
    terminalArtifactLabel,
    errorArtifactLabel,
    operatorMessageArtifactLabel,
    currentArrivalKey: operatorMessageCheckpoint
      ? conversationArrivalKey(operatorMessageCheckpoint.id, operatorMessageCheckpoint.epoch)
      : null,
  });
  const operatorMessageSeq = operatorMessageCheckpoint
    ? fold.operator_arrivals[conversationArrivalKey(operatorMessageCheckpoint.id, operatorMessageCheckpoint.epoch)] ?? null
    : null;
  const terminalAfterOperatorMessage = operatorMessageSeq !== null
    && fold.latest_snapshot !== null
    && fold.latest_snapshot.seq > operatorMessageSeq;
  const latestSnapshot = fold.latest_snapshot ? { id: fold.latest_snapshot.id } : null;
  const retainedSnapshot = latestSnapshot
    ?? priorBindings.at(-1)
    ?? continuation?.artifactBindings?.at(-1)
    ?? null;
  return {
    ledgerOffset: fold.offset,
    artifactBindings: retainedSnapshot ? uniqueArtifactBindings([retainedSnapshot]) : [],
    pinnedArtifactBindings,
    pinnedContextItems,
    continuedFrom,
    terminalAfterOperatorMessage,
    operatorMessageCheckpointAdmitted: operatorMessageSeq !== null,
  };
}

function conversationArrivalKey(checkpoint, epoch) {
  return `${requiredString(checkpoint, "operator message checkpoint")}@${nonNegativeInteger(epoch, "operator message epoch")}`;
}

function conversationFoldPath(octopusHome, interfaceId) {
  return path.join(octopusHome, "interfaces", interfaceId, "conversation-fold.client.json");
}

function emptyConversationFold(runId, priorOffset) {
  return { schema_version: 1, run: runId, start_offset: priorOffset, offset: priorOffset, latest_snapshot: null, operator_arrivals: {} };
}

function usableConversationFold(saved, runId, priorOffset, ledgerLength) {
  return isObject(saved)
    && saved.schema_version === 1
    && saved.run === runId
    && saved.start_offset === priorOffset
    && Number.isSafeInteger(saved.offset) && saved.offset >= priorOffset && saved.offset <= ledgerLength
    && (saved.latest_snapshot === null || (isObject(saved.latest_snapshot)
      && typeof saved.latest_snapshot.id === "string" && Number.isSafeInteger(saved.latest_snapshot.seq)))
    && isObject(saved.operator_arrivals)
    && Object.values(saved.operator_arrivals).every((seq) => Number.isSafeInteger(seq));
}

async function foldConversationLedger({
  octopusHome,
  movementsPath,
  runId,
  interfaceId,
  priorOffset,
  terminalArtifactLabel,
  errorArtifactLabel,
  operatorMessageArtifactLabel,
  currentArrivalKey,
}) {
  const foldPath = conversationFoldPath(octopusHome, interfaceId);
  const ledgerLength = await fs.stat(movementsPath).then((stat) => stat.size, (error) => {
    if (error?.code === "ENOENT") return 0;
    throw error;
  });
  const saved = await readJsonIfPresent(foldPath).catch(() => null);
  const fold = usableConversationFold(saved, runId, priorOffset, ledgerLength)
    ? saved : emptyConversationFold(runId, priorOffset);
  const tail = await readCompleteLedgerTail(movementsPath, fold.offset);
  for (let index = 0; index < tail.lines.length; index += 1) {
    let movement;
    try {
      movement = JSON.parse(tail.lines[index]);
    } catch (error) {
      throw new Error(`movement ledger contains malformed complete JSON at tail line ${index + 1}: ${error.message}`);
    }
    const artifactId = stringValue(movement?.artifact?.id);
    const arrivalRuntime = artifactIdHasLabel(artifactId, requiredString(operatorMessageArtifactLabel, "operator message artifact label"))
      ? exactArrivalRuntime(movement, interfaceId)
      : null;
    if (arrivalRuntime?.run === runId) {
      fold.operator_arrivals[conversationArrivalKey(arrivalRuntime.checkpoint, arrivalRuntime.epoch)] =
        nonNegativeInteger(movement.seq, "operator message arrival seq");
    }
    const outboundRuntime = exactOutboundRuntime(movement, interfaceId);
    if (outboundRuntime?.run === runId
      && (artifactIdHasLabel(artifactId, terminalArtifactLabel) || artifactIdHasLabel(artifactId, errorArtifactLabel))
      && isObject(movement.artifact)) {
      fold.latest_snapshot = {
        id: requiredString(movement.artifact.id, "movement artifact.id"),
        seq: nonNegativeInteger(movement.seq, "terminal snapshot seq"),
      };
    }
  }
  const advanced = tail.nextOffset !== fold.offset;
  fold.offset = tail.nextOffset;
  for (const [key, seq] of Object.entries(fold.operator_arrivals)) {
    if (currentArrivalKey !== null && key !== currentArrivalKey
      && fold.latest_snapshot !== null && seq < fold.latest_snapshot.seq) {
      delete fold.operator_arrivals[key];
    }
  }
  if (advanced || saved !== fold) await writeConversationFold(foldPath, fold);
  return fold;
}

async function writeConversationFold(foldPath, fold) {
  await fs.mkdir(path.dirname(foldPath), { recursive: true, mode: 0o700 });
  const temporary = path.join(
    path.dirname(foldPath),
    `.${path.basename(foldPath)}.${process.pid}.${randomToken(16)}.tmp`,
  );
  let handle;
  try {
    handle = await fs.open(temporary, "wx", 0o600);
    await handle.writeFile(`${JSON.stringify(fold)}\n`, "utf8");
    await handle.close();
    handle = null;
    await fs.rename(temporary, foldPath);
  } finally {
    await handle?.close().catch(() => {});
    await fs.unlink(temporary).catch((error) => {
      if (error?.code !== "ENOENT") throw error;
    });
  }
}
