module.exports = { artifactBindingAdmitsLabel, assertProviderAccountActiveRoute, assertProviderModelCatalogActiveRoute, assertProviderOperationRoute, providerAccountLoginRequest, providerAccountRequest, providerAccountRequestCoordinates, providerAccountSubmissionSeq, providerModelCatalogRequest, providerOperationRequestCoordinates, providerOperationSubmissionSeq, resolveProviderAccountContext, resolveProviderModelCatalogContext };

const path = require("node:path");
const { assertExactOperatorClientInterface } = require("./artifactBatches.cjs");
const { PROVIDER_ACCOUNT_LOGIN_COMPLETED_LABEL, PROVIDER_ACCOUNT_LOGIN_STARTED_LABEL, PROVIDER_ACCOUNT_LOGIN_START_REQUEST_LABEL, PROVIDER_ACCOUNT_LOGIN_STATUS_LABEL, PROVIDER_ACCOUNT_LOGIN_STATUS_REQUEST_LABEL, PROVIDER_ACCOUNT_REQUEST_LABEL, PROVIDER_ACCOUNT_SNAPSHOT_LABEL, PROVIDER_ACCOUNT_UPDATED_LABEL, PROVIDER_MODELS_CATALOG_LABEL, PROVIDER_MODELS_LIST_LABEL, PROVIDER_OPERATION_ERROR_LABEL } = require("./constants.cjs");
const { authoredInterfaceBindingId, chatIdentityFromRunRecord, uniqueNonEmptyStrings } = require("./conversation.cjs");
const { resolveRunRecordsRoot } = require("./operatorMessages.cjs");
const { providerAccountArtifactCoordinates, resolveProviderAccountArtifact } = require("./providerObservations.cjs");
const { readConfig } = require("./roots.cjs");
const { canonicalJson } = require("./topologyPlan.cjs");
const { boundedPositiveInteger, contentHashHex, exactArrivalRuntime, integerAtLeast, isObject, nonNegativeInteger, readJsonDocumentIfPresent, readJsonIfPresent, requiredArray, requiredObject, requiredString, safeSegment, sha256Hex, stringValue } = require("./values.cjs");

function providerAccountRequest(request, includeEvidence) {
  if (!isObject(request)) throw new Error("provider account request must be an object");
  const allowed = includeEvidence
    ? new Set(["operationId", "runId", "interfaceId", "providerInterfaceId", "evidence"])
    : new Set([
      "operationId",
      "runId",
      "interfaceId",
      "providerInterfaceId",
      "afterSeq",
      "timeoutMs",
    ]);
  const unknown = Object.keys(request).filter((field) => !allowed.has(field));
  if (unknown.length > 0) {
    throw new Error(`provider account request contains unknown fields: ${unknown.join(", ")}`);
  }
  const input = {
    operationId: safeSegment(request.operationId, "operationId"),
    runId: safeSegment(request.runId, "runId"),
    interfaceId: safeSegment(request.interfaceId, "interfaceId"),
    providerInterfaceId: safeSegment(request.providerInterfaceId, "providerInterfaceId"),
  };
  if (includeEvidence) {
    return {
      ...input,
      evidence: request.evidence === undefined || request.evidence === null
        ? null
        : request.evidence,
    };
  }
  return {
    ...input,
    afterSeq: request.afterSeq === undefined
      ? -1
      : integerAtLeast(request.afterSeq, -1, "afterSeq"),
    timeoutMs: boundedPositiveInteger(request.timeoutMs, 45_000, 120_000, "timeoutMs"),
  };
}

function providerModelCatalogRequest(request) {
  if (!isObject(request)) throw new Error("provider model catalog request must be an object");
  const allowed = new Set([
    "runId",
    "interfaceId",
    "providerInterfaceId",
    "timeoutMs",
  ]);
  const unknown = Object.keys(request).filter((field) => !allowed.has(field));
  if (unknown.length > 0) {
    throw new Error(`provider model catalog request contains unknown fields: ${unknown.join(", ")}`);
  }
  return {
    runId: safeSegment(request.runId, "runId"),
    interfaceId: safeSegment(request.interfaceId, "interfaceId"),
    providerInterfaceId: safeSegment(request.providerInterfaceId, "providerInterfaceId"),
    timeoutMs: boundedPositiveInteger(request.timeoutMs, 45_000, 120_000, "timeoutMs"),
  };
}

function providerAccountLoginRequest(request) {
  if (!isObject(request)) throw new Error("provider account login request must be an object");
  const allowed = new Set([
    "operationId",
    "runId",
    "interfaceId",
    "providerInterfaceId",
    "afterSeq",
    "timeoutMs",
    "statusPollMs",
  ]);
  const unknown = Object.keys(request).filter((field) => !allowed.has(field));
  if (unknown.length > 0) {
    throw new Error(`provider account login request contains unknown fields: ${unknown.join(", ")}`);
  }
  return {
    operationId: safeSegment(request.operationId, "operationId"),
    runId: safeSegment(request.runId, "runId"),
    interfaceId: safeSegment(request.interfaceId, "interfaceId"),
    providerInterfaceId: safeSegment(request.providerInterfaceId, "providerInterfaceId"),
    afterSeq: request.afterSeq === undefined
      ? -1
      : integerAtLeast(request.afterSeq, -1, "afterSeq"),
    timeoutMs: boundedPositiveInteger(request.timeoutMs, 300_000, 600_000, "timeoutMs"),
    statusPollMs: boundedPositiveInteger(request.statusPollMs, 1_000, 10_000, "statusPollMs"),
  };
}

async function resolveProviderAccountContext(
  octopusHome,
  runId,
  interfaceId,
  providerInterfaceId,
  requireActiveRoute,
  requireLogin = false,
) {
  const config = await readConfig();
  await assertExactOperatorClientInterface(octopusHome, interfaceId);
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const runDocument = await readJsonDocumentIfPresent(path.join(runsDir, `${runId}.json`));
  if (!runDocument) throw new Error(`provider account Home run '${runId}' does not exist`);
  const run = runDocument.value;
  const identity = chatIdentityFromRunRecord(config, runId, run);
  if (identity.interfaceId !== interfaceId) {
    throw new Error("provider account request does not use the run's exact Home interface");
  }
  const clientState = requiredObject(
    run.interface_state?.[interfaceId],
    `provider account Home state '${interfaceId}'`,
  );
  if (clientState.client_surface_role !== "home") {
    throw new Error(`operator chat run '${runId}' is not the exact Home surface`);
  }
  if (clientState.provider_choice_interface !== providerInterfaceId) {
    throw new Error("provider account request does not match Home's provider choice");
  }
  const runtimeProviderInterfaceId = safeSegment(
    clientState.provider_runtime_interface,
    "provider_runtime_interface",
  );
  requiredObject(
    run.interface_state?.[runtimeProviderInterfaceId],
    `provider account runtime state '${runtimeProviderInterfaceId}'`,
  );
  const descriptor = await readJsonIfPresent(path.join(
    octopusHome,
    "registry",
    "interface",
    runtimeProviderInterfaceId,
    "interface.json",
  ));
  if (
    !isObject(descriptor)
    || descriptor.id !== runtimeProviderInterfaceId
    || stringValue(descriptor.interface_type).toLowerCase() !== "world"
    || stringValue(descriptor.role).toLowerCase() !== "provider"
  ) {
    throw new Error(`interface '${runtimeProviderInterfaceId}' is not the exact World provider`);
  }
  const privateConfig = requiredObject(
    descriptor.interface_driver_config?.private_config,
    `provider '${runtimeProviderInterfaceId}' private interface config`,
  );
  const operations = requiredObject(
    privateConfig.provider_operations,
    `provider '${runtimeProviderInterfaceId}' operations`,
  );
  const accountRead = requiredObject(
    operations.account_read,
    `provider '${runtimeProviderInterfaceId}' account_read`,
  );
  const requestLabel = requiredString(accountRead.request_label, "provider account request label");
  const snapshotLabel = requiredString(accountRead.output_label, "provider account snapshot label");
  const operationErrorLabel = requiredString(
    operations.error_output_label,
    "provider operation error label",
  );
  const turnErrorLabel = requiredString(config.artifact_labels.error_output, "turn error label");
  if (
    requestLabel !== PROVIDER_ACCOUNT_REQUEST_LABEL
    || snapshotLabel !== PROVIDER_ACCOUNT_SNAPSHOT_LABEL
    || operationErrorLabel !== PROVIDER_OPERATION_ERROR_LABEL
    || turnErrorLabel !== "turn.error"
  ) {
    throw new Error(`provider '${runtimeProviderInterfaceId}' account-read vocabulary drifted`);
  }
  const clientProjection = requiredArray(
    privateConfig.client_projection_output_labels,
    `provider '${runtimeProviderInterfaceId}' client projection labels`,
  ).map((label, index) => requiredString(label, `client projection label ${index}`));
  let login = null;
  if (requireLogin) {
    const loginStart = requiredObject(
      operations.account_login_start,
      `provider '${runtimeProviderInterfaceId}' account_login_start`,
    );
    const loginStatus = requiredObject(
      operations.account_login_status_read,
      `provider '${runtimeProviderInterfaceId}' account_login_status_read`,
    );
    login = {
      startRequestLabel: requiredString(loginStart.request_label, "provider login-start request label"),
      startedLabel: requiredString(loginStart.output_label, "provider login-start output label"),
      statusRequestLabel: requiredString(loginStatus.request_label, "provider login-status request label"),
      statusLabel: requiredString(loginStatus.output_label, "provider login-status output label"),
      completedLabel: requiredString(
        operations.account_login_completed_output_label,
        "provider login-completed output label",
      ),
      accountUpdatedLabel: requiredString(
        operations.account_updated_output_label,
        "provider account-updated output label",
      ),
    };
    if (
      login.startRequestLabel !== PROVIDER_ACCOUNT_LOGIN_START_REQUEST_LABEL
      || login.startedLabel !== PROVIDER_ACCOUNT_LOGIN_STARTED_LABEL
      || login.statusRequestLabel !== PROVIDER_ACCOUNT_LOGIN_STATUS_REQUEST_LABEL
      || login.statusLabel !== PROVIDER_ACCOUNT_LOGIN_STATUS_LABEL
      || login.completedLabel !== PROVIDER_ACCOUNT_LOGIN_COMPLETED_LABEL
      || login.accountUpdatedLabel !== PROVIDER_ACCOUNT_UPDATED_LABEL
    ) {
      throw new Error(`provider '${runtimeProviderInterfaceId}' account-login vocabulary drifted`);
    }
  }
  const projectedLabels = [snapshotLabel, operationErrorLabel, turnErrorLabel];
  if (login) {
    projectedLabels.push(
      login.startedLabel,
      login.statusLabel,
      login.completedLabel,
      login.accountUpdatedLabel,
    );
  }
  for (const label of projectedLabels) {
    if (!clientProjection.includes(label)) {
      throw new Error(`provider '${runtimeProviderInterfaceId}' does not project '${label}' to Home`);
    }
  }
  const context = {
    run,
    runtimeProviderInterfaceId,
    requestLabel,
    snapshotLabel,
    operationErrorLabel,
    turnErrorLabel,
    login,
  };
  if (requireActiveRoute) {
    assertProviderAccountActiveRoute(
      run,
      runId,
      interfaceId,
      runtimeProviderInterfaceId,
      context,
    );
  }
  return context;
}

async function resolveProviderModelCatalogContext(
  octopusHome,
  runId,
  interfaceId,
  providerInterfaceId,
) {
  const config = await readConfig();
  await assertExactOperatorClientInterface(octopusHome, interfaceId);
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const runDocument = await readJsonDocumentIfPresent(path.join(runsDir, `${runId}.json`));
  if (!runDocument) throw new Error(`provider model catalog run '${runId}' does not exist`);
  const run = runDocument.value;
  const identity = chatIdentityFromRunRecord(config, runId, run);
  if (identity.interfaceId !== interfaceId) {
    throw new Error("provider model catalog request does not use the run's exact client interface");
  }
  const clientState = requiredObject(
    run.interface_state?.[interfaceId],
    `provider model catalog client state '${interfaceId}'`,
  );
  if (clientState.provider_choice_interface !== providerInterfaceId) {
    throw new Error("provider model catalog request does not match the task's provider choice");
  }
  const runtimeProviderInterfaceId = safeSegment(
    clientState.provider_runtime_interface,
    "provider_runtime_interface",
  );
  requiredObject(
    run.interface_state?.[runtimeProviderInterfaceId],
    `provider model catalog runtime state '${runtimeProviderInterfaceId}'`,
  );
  const descriptor = await readJsonIfPresent(path.join(
    octopusHome,
    "registry",
    "interface",
    runtimeProviderInterfaceId,
    "interface.json",
  ));
  if (
    !isObject(descriptor)
    || descriptor.id !== runtimeProviderInterfaceId
    || stringValue(descriptor.interface_type).toLowerCase() !== "world"
    || stringValue(descriptor.role).toLowerCase() !== "provider"
  ) {
    throw new Error(`interface '${runtimeProviderInterfaceId}' is not the exact World provider`);
  }
  const privateConfig = requiredObject(
    descriptor.interface_driver_config?.private_config,
    `provider '${runtimeProviderInterfaceId}' private interface config`,
  );
  const operation = requiredObject(
    privateConfig.model_catalog_operation,
    `provider '${runtimeProviderInterfaceId}' model_catalog_operation`,
  );
  const requestLabel = requiredString(operation.request_label, "provider model catalog request label");
  const outputLabel = requiredString(operation.output_label, "provider model catalog output label");
  const operationErrorLabel = PROVIDER_OPERATION_ERROR_LABEL;
  const turnErrorLabel = requiredString(config.artifact_labels.error_output, "turn error label");
  if (
    requestLabel !== PROVIDER_MODELS_LIST_LABEL
    || outputLabel !== PROVIDER_MODELS_CATALOG_LABEL
    || turnErrorLabel !== "turn.error"
  ) {
    throw new Error(`provider '${runtimeProviderInterfaceId}' model-catalog vocabulary drifted`);
  }
  const clientProjection = requiredArray(
    privateConfig.client_projection_output_labels,
    `provider '${runtimeProviderInterfaceId}' client projection labels`,
  ).map((label, index) => requiredString(label, `client projection label ${index}`));
  for (const label of [outputLabel, operationErrorLabel, turnErrorLabel]) {
    if (!clientProjection.includes(label)) {
      throw new Error(`provider '${runtimeProviderInterfaceId}' does not project '${label}' to its client`);
    }
  }
  const context = {
    run,
    runtimeProviderInterfaceId,
    requestLabel,
    outputLabel,
    operationErrorLabel,
    turnErrorLabel,
    selectedModel: requiredString(privateConfig.model, "provider selected model"),
    runPath: path.join(runsDir, `${runId}.json`),
  };
  context.inputCheckpoint = assertProviderModelCatalogActiveRoute(
    run,
    runId,
    interfaceId,
    runtimeProviderInterfaceId,
    context,
  );
  return context;
}

function assertProviderModelCatalogActiveRoute(run, runId, interfaceId, providerInterfaceId, context) {
  return assertProviderOperationRoute(run, runId, interfaceId, providerInterfaceId,
    [context.requestLabel], [context.outputLabel, context.operationErrorLabel, context.turnErrorLabel]);
}

function assertProviderAccountActiveRoute(run, runId, interfaceId, providerInterfaceId, context) {
  const requests = [context.requestLabel];
  const outputs = [context.snapshotLabel, context.operationErrorLabel, context.turnErrorLabel];
  if (context.login) {
    requests.push(context.login.startRequestLabel, context.login.statusRequestLabel);
    outputs.push(context.login.startedLabel, context.login.statusLabel,
      context.login.completedLabel, context.login.accountUpdatedLabel);
  }
  assertProviderOperationRoute(run, runId, interfaceId, providerInterfaceId, requests, outputs);
}

function assertProviderOperationRoute(run, runId, interfaceId, providerInterfaceId, requestLabels, outputLabels) {
  const activeIds = uniqueNonEmptyStrings(
    requiredArray(run.active_checkpoints, "active_checkpoints"), "active checkpoint",
  );
  const checkpoints = new Map();
  for (const value of requiredArray(run.checkpoints, "checkpoints")) {
    const checkpoint = requiredObject(value, "checkpoint");
    const id = requiredString(checkpoint.id, "checkpoint.id");
    if (checkpoints.has(id)) throw new Error(`run '${runId}' repeats checkpoint '${id}'`);
    checkpoints.set(id, checkpoint);
  }
  let inputs = 0;
  let inputCheckpoint = null;
  for (const id of activeIds) {
    const checkpoint = checkpoints.get(id);
    if (!checkpoint) throw new Error(`run '${runId}' omits active checkpoint '${id}'`);
    if (checkpoint.interface_binding == null) continue;
    if (authoredInterfaceBindingId(checkpoint.interface_binding) !== interfaceId
      || !requestLabels.every((label) => artifactBindingAdmitsLabel(checkpoint.interface_binding, label))) continue;
    const outbound = requiredArray(checkpoint.outbound_interface_bindings, "checkpoint outbound interface bindings");
    if (checkpoint.run !== runId || outbound.length !== 1
      || authoredInterfaceBindingId(outbound[0]) !== providerInterfaceId
      || !requestLabels.every((label) => artifactBindingAdmitsLabel(outbound[0], label))) {
      throw new Error("provider operation input differs from its exact provider route");
    }
    inputs += 1;
    nonNegativeInteger(checkpoint.epoch, "provider operation input checkpoint epoch");
    inputCheckpoint = checkpoint;
  }
  if (inputs !== 1) throw new Error("provider operation requires one exact active client input");

  // Responses author a fresh checkpoint for their exact label before admission.
  // The previous response's active checkpoint is not the next response's owner.
  const topology = requiredObject(run.interface_state?.[providerInterfaceId]?.run_authoring_binding,
    "provider run_authoring_binding");
  if (topology.schema_version !== 2 || topology.run !== runId
    || topology.worker !== run.worker || topology.provider_interface !== providerInterfaceId) {
    throw new Error("provider operation output topology differs from its exact run/provider");
  }
  const outputs = requiredArray(topology.checkpoints, "provider authoring checkpoints")
    .map((entry) => requiredObject(entry, "provider authoring checkpoint"))
    .filter((entry) => entry.role === "provider_artifact_output");
  for (const label of outputLabels) {
    // Match the provider's ClientProjection owner selection, including verifiers.
    const owners = outputs.filter((entry) => {
      const outbound = requiredArray(entry.outbound_interface_bindings, "provider output bindings");
      const verified = requiredArray(entry.required_verified_artifacts, "provider output requirements");
      return outbound.length === 1 && artifactBindingAdmitsLabel(outbound[0], label)
        && (verified.length === 0 || (verified.length === 1 && verified[0].artifact_label === label));
    });
    if (owners.length !== 1
      || authoredInterfaceBindingId(owners[0].interface_binding) !== providerInterfaceId
      || !artifactBindingAdmitsLabel(owners[0].interface_binding, label)
      || authoredInterfaceBindingId(owners[0].outbound_interface_bindings[0]) !== interfaceId) {
      throw new Error(`provider operation '${label}' has no exact client output owner`);
    }
  }
  return inputCheckpoint;
}

function artifactBindingAdmitsLabel(binding, label) {
  authoredInterfaceBindingId(binding);
  const policy = binding.config.artifact_policy;
  if (policy.mode === "all") return true;
  const names = requiredArray(policy.names, "artifact policy names")
    .map((name, index) => requiredString(name, `artifact policy name ${index}`));
  const namespaces = requiredArray(policy.namespaces, "artifact policy namespaces")
    .map((namespace, index) => requiredString(namespace, `artifact policy namespace ${index}`));
  return names.includes(label) || namespaces.some((namespace) => (
    label === namespace || label.startsWith(`${namespace}.`)
  ));
}

function providerAccountSubmissionSeq(submission, input, requestLabel, payload) {
  return providerOperationSubmissionSeq(submission, input, requestLabel, payload);
}

function providerOperationSubmissionSeq(submission, input, requestLabel, payload) {
  if (!isObject(submission)) throw new Error("provider account submission returned no evidence");
  if (
    submission.interfaceId !== input.interfaceId
    || submission.runId !== input.runId
    || submission.label !== requestLabel
  ) {
    throw new Error("provider account submission returned the wrong run or interface coordinates");
  }
  const expectedPayloadHash = sha256Hex(Buffer.from(JSON.stringify(payload)));
  if (contentHashHex(submission.contentHash, "provider account submission contentHash") !== expectedPayloadHash) {
    throw new Error("provider account submission changed the strict operation request bytes");
  }
  if (!isObject(submission.verdict?.admitted)) {
    throw new Error("provider account submission was not admitted");
  }
  return nonNegativeInteger(
    submission.verdict.admitted.seq,
    "provider account admission seq",
  );
}

async function providerAccountRequestCoordinates(movement, input, context, readArtifactPayload) {
  return providerOperationRequestCoordinates(
    movement,
    input,
    context.requestLabel,
    { refresh_token: true },
    readArtifactPayload,
    "provider account request",
  );
}

async function providerOperationRequestCoordinates(
  movement,
  input,
  requestLabel,
  expectedPayload,
  readArtifactPayload,
  field,
) {
  if (movement?.kind !== "InterfaceArrival") {
    throw new Error("provider account admission seq does not identify an InterfaceArrival");
  }
  const seq = nonNegativeInteger(movement.seq, "provider account request movement seq");
  const runtime = exactArrivalRuntime(movement, input.interfaceId);
  if (runtime?.run !== input.runId) {
    throw new Error("provider account request movement has the wrong Home/run coordinates");
  }
  const artifact = providerAccountArtifactCoordinates(
    movement,
    requestLabel,
    field,
  );
  const resolved = await resolveProviderAccountArtifact(
    readArtifactPayload,
    artifact,
    requestLabel,
  );
  const payload = requiredObject(resolved.payload, `${field} payload`);
  if (canonicalJson(payload) !== canonicalJson(expectedPayload)) {
    throw new Error(`${field} payload differs from its declared operation request`);
  }
  return {
    requestMovementSeq: seq,
    requestArtifactId: artifact.artifactId,
    requestManifestHash: artifact.manifestHash,
    requestPayloadHash: resolved.payloadHash,
  };
}
