module.exports = { adoptOperatorChatPackageUpdates, assertGenericOperatorChatArtifactAllowed, planOperatorChatPackageAdoption, planOperatorChatSurface, planOperatorChatTaskAmendment, submitOperatorTurnControl };

const fs = require("node:fs/promises");
const path = require("node:path");
const { assertExactOperatorClientInterface } = require("./artifactBatches.cjs");
const { chatIdentityFromRunRecord, readConversationState, resolveContinuationState } = require("./conversation.cjs");
const { resolveRunRecordsRoot, writeContentAddressedPayload } = require("./operatorMessages.cjs");
const { runOperatorRevisionCommand } = require("./operatorTurns.cjs");
const { openNativeAdoptionBatch } = require("./nativeAdoptionBatch.cjs");
const { validatePlannedHostBindings } = require("./hostBindings.cjs");
const { readFileHashIfPresent, resolveRegistryRoot, waitForInterfaceSocket, withRunAuthoringLock } = require("./publication.cjs");
const { readConfig, resolveEeInstancesRoot, resolveInstallationRoot, resolvePackageExecutable } = require("./roots.cjs");
const { assertPublicationExpansion, interfaceEndpoint, operatorContextArtifactBindings, operatorDoorHostSlots, operatorExternalInputs, operatorExternalRelays, operatorModelOutputs, operatorPayloadArtifactBindings, operatorPublications, operatorSelectedTools, operatorSelectedWorkerEnvironmentKinds, operatorSurfaceRole, planOperatorChatTopology, plannedAdditionalRunWrites, plannedClientRemoval, plannedClientWrite, plannedExactIds, safeContractToken, safeSha256, validatePlannedRunRecord } = require("./topologyPlan.cjs");
const { absolutePath, assertAdmitted, isObject, numberValue, optionalServiceTier, randomToken, readJsonDocumentIfPresent, requiredArray, requiredFunction, requiredObject, requiredString, safeSegment, sha256Hex, stringValue } = require("./values.cjs");

async function submitOperatorTurnControl(options, request) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const interfaceId = safeSegment(request?.interfaceId, "interfaceId");
  const runId = safeSegment(request?.runId, "runId");
  const controlId = safeContractToken(request?.controlId, "controlId");
  const action = requiredString(request?.action, "action").toLowerCase();
  if (!["interrupt", "resume", "stop"].includes(action)) {
    throw new Error("operator turn control action must be 'interrupt', 'resume', or 'stop'");
  }
  const timeoutMs = numberValue(request?.timeoutMs, 45_000);
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const runDocument = await readJsonDocumentIfPresent(path.join(runsDir, `${runId}.json`));
  if (!runDocument) throw new Error(`operator chat run '${runId}' does not exist`);
  const identity = chatIdentityFromRunRecord(config, runId, runDocument.value);
  if (identity.interfaceId !== interfaceId) {
    throw new Error(`operator chat run '${runId}' is not owned by interface '${interfaceId}'`);
  }
  await assertExactOperatorClientInterface(octopusHome, interfaceId);
  await waitForInterfaceSocket(octopusHome, interfaceId, timeoutMs);
  const artifactRoot = path.join(
    absolutePath(options.clientArtifactsRoot(interfaceId), "clientArtifactsRoot"),
    "operator-chat",
    "payloads",
  );
  await fs.mkdir(artifactRoot, { recursive: true });
  const label = requiredString(config.artifact_labels.turn_control, "client.artifact_labels.turn_control");
  const payload = { action, control_id: controlId };
  const payloadPath = await writeContentAddressedPayload(
    artifactRoot,
    label,
    Buffer.from(JSON.stringify(payload)),
  );
  const verdict = await requiredFunction(options.handInArtifact, "handInArtifact")({
    endpoint: interfaceEndpoint(octopusHome, interfaceId),
    interfaceId,
    manifest: { label, payload_path: payloadPath },
    timeoutMs,
  });
  assertAdmitted(verdict, label);
  return {
    interfaceId,
    runId,
    label,
    controlId,
    action,
    payloadPath,
    verdict,
  };
}

async function assertGenericOperatorChatArtifactAllowed(request) {
  const config = await readConfig();
  const interfaceId = safeSegment(request?.interfaceId, "interfaceId");
  const label = requiredString(request?.label, "label");
  const interfacePrefix = requiredString(config.run.interface_prefix, "client.run.interface_prefix");
  if (!interfaceId.startsWith(interfacePrefix)) return;
  const reservedLabels = new Set([
    requiredString(config.artifact_labels.message, "client.artifact_labels.message"),
    requiredString(config.artifact_labels.turn_control, "client.artifact_labels.turn_control"),
  ]);
  if (reservedLabels.has(label)) {
    throw new Error(`operator chat artifact '${label}' requires its interface-owned admission path`);
  }
}

async function planOperatorChatSurface(options, request) {
  const allowedFields = new Set([
    "chatId",
    "cycle",
    "providerInterfaceId",
    "providerModel",
    "reasoningEffort",
    "serviceTier",
    "eeId",
    "workerInterfaceId",
    "doorHostSlots",
    "externalInputs",
    "externalRelays",
    "modelOutputs",
    "contextArtifactBindings",
    "payloadArtifactBindings",
    "publications",
    "selectedWorkerEnvironmentKinds",
    "selectedTools",
    "clientSurfaceRole",
    "continuation",
  ]);
  if (!isObject(request)) throw new Error("operator chat surface plan must be a JSON object");
  const unknownFields = Object.keys(request).filter((field) => !allowedFields.has(field));
  if (unknownFields.length > 0) {
    throw new Error(`operator chat surface plan contains unknown fields: ${unknownFields.join(", ")}`);
  }
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const registryRoot = resolveRegistryRoot(options);
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const eeInstancesRoot = await resolveEeInstancesRoot(octopusHome, options);
  const chatId = safeSegment(request.chatId, "chatId");
  const cycle = safeSegment(request.cycle, "cycle");
  const providerInterfaceId = safeSegment(
    request.providerInterfaceId,
    "providerInterfaceId",
  );
  const providerModel = requiredString(request.providerModel, "providerModel");
  const clientSurfaceRole = operatorSurfaceRole(request.clientSurfaceRole);
  const eeId = request.eeId === undefined || request.eeId === null
    ? safeSegment(config.run.ee, "client.run.ee")
    : safeSegment(request.eeId, "eeId");
  const workerInterfaceId = request.workerInterfaceId === undefined
      || request.workerInterfaceId === null
    ? safeSegment(config.run.worker, "client.run.worker")
    : safeSegment(request.workerInterfaceId, "workerInterfaceId");
  const runId = `${requiredString(config.run.run_prefix, "client.run.run_prefix")}${chatId}`;
  const chatInterfaceId = `${requiredString(
    config.run.interface_prefix,
    "client.run.interface_prefix",
  )}${chatId}`;
  const chatRunPath = path.join(runsDir, `${runId}.json`);
  if (await readJsonDocumentIfPresent(chatRunPath)) {
    throw new Error(`operator chat surface plan refuses existing run '${runId}'`);
  }
  const continuation = await resolveContinuationState({
    config,
    octopusHome,
    requestedRunId: "",
    request: request.continuation,
  });
  const conversationState = await readConversationState({
    octopusHome,
    existing: null,
    runId,
    interfaceId: chatInterfaceId,
    terminalArtifactLabel: config.artifact_labels.terminal_output,
    errorArtifactLabel: config.artifact_labels.error_output,
    continuation,
  });
  const doorHostSlots = operatorDoorHostSlots(request.doorHostSlots);
  const externalInputs = operatorExternalInputs(request.externalInputs);
  const externalRelays = operatorExternalRelays(request.externalRelays);
  const modelOutputs = operatorModelOutputs(request.modelOutputs);
  const contextArtifactBindings = operatorContextArtifactBindings(
    request.contextArtifactBindings,
  );
  const payloadArtifactBindings = operatorPayloadArtifactBindings(request.payloadArtifactBindings);
  const publications = operatorPublications(request.publications);
  const selectedWorkerEnvironmentKinds = operatorSelectedWorkerEnvironmentKinds(
    request.selectedWorkerEnvironmentKinds,
  );
  const selectedTools = operatorSelectedTools(request.selectedTools);
  const topologyPlan = await planOperatorChatTopology({
    octopusHome,
    topologyPlanner: resolvePackageExecutable(
      config,
      resolveInstallationRoot(options),
      registryRoot,
      "topology_planner",
    ),
    registryRoot,
    eeInstancesRoot,
    chatId,
    providerInterfaceId,
    providerModel,
    reasoningEffort: stringValue(request.reasoningEffort) || null,
    serviceTier: optionalServiceTier(request.serviceTier),
    eeId,
    workerInterfaceId,
    doorHostSlots,
    externalInputs,
    externalRelays,
    modelOutputs,
    contextArtifactBindings,
    payloadArtifactBindings,
    publications,
    selectedWorkerEnvironmentKinds,
    selectedTools,
    clientSurfaceRole,
    cycle,
    existingRunRecord: null,
    conversation: {
      ledger_offset: conversationState.ledgerOffset,
      artifact_bindings: conversationState.artifactBindings,
      pinned_artifact_bindings: conversationState.pinnedArtifactBindings,
      pinned_context_items: conversationState.pinnedContextItems,
      continued_from: conversationState.continuedFrom,
    },
  });
  if (topologyPlan.expected_before_hash_algorithm !== "sha256") {
    throw new Error("topology plan did not honor the requested SHA-256 write contract");
  }
  if (topologyPlan.run_id !== runId || topologyPlan.chat_interface_id !== chatInterfaceId) {
    throw new Error(
      `topology plan identity drifted: planned run '${topologyPlan.run_id}' interface '${topologyPlan.chat_interface_id}', expected run '${runId}' interface '${chatInterfaceId}'`,
    );
  }
  const expectedAuthoringProfile = {
    ee_id: eeId,
    worker_choice_interface: workerInterfaceId,
    door_host_slots: doorHostSlots,
    ...(externalInputs.length > 0 ? { external_inputs: externalInputs } : {}),
    ...(externalRelays.length > 0 ? { external_relays: externalRelays } : {}),
    ...(modelOutputs.length > 0 ? { model_outputs: modelOutputs } : {}),
    ...(contextArtifactBindings.length > 0
      ? { context_artifact_bindings: contextArtifactBindings }
      : {}),
    ...(payloadArtifactBindings.length > 0 ? { payload_artifact_bindings: payloadArtifactBindings } : {}),
    ...(selectedWorkerEnvironmentKinds.length > 0
      ? { selected_worker_environment_kinds: selectedWorkerEnvironmentKinds }
      : {}),
    ...(selectedTools.length > 0 ? { selected_tools: selectedTools } : {}),
  };
  const authoringProfile = requiredObject(
    topologyPlan.authoring_profile,
    "plan.authoring_profile",
  );
  assertPublicationExpansion(topologyPlan, expectedAuthoringProfile, publications);
  const requiredInterfaceIds = plannedExactIds(
    topologyPlan.required_interface_ids,
    "plan.required_interface_ids",
    safeSegment,
  );
  const requiredToolIds = plannedExactIds(
    topologyPlan.required_tool_ids,
    "plan.required_tool_ids",
    safeContractToken,
  );
  for (const required of [
    topologyPlan.chat_interface_id,
    topologyPlan.model_interface_id,
    topologyPlan.worker_interface_id,
  ]) {
    if (!requiredInterfaceIds.includes(required)) {
      throw new Error(`topology plan required interfaces omit '${required}'`);
    }
  }
  const writes = topologyPlan.interface_writes.map((write, index) => (
    plannedClientWrite(write, index, registryRoot)
  ));
  writes.push(...plannedAdditionalRunWrites(topologyPlan, runsDir, registryRoot));
  const runRecordContent = topologyPlan.run_record_content;
  if (typeof runRecordContent !== "string" || !runRecordContent.trim()) {
    throw new Error("plan.run_record_content must be a non-empty string");
  }
  validatePlannedRunRecord(
    runRecordContent,
    runId,
    chatInterfaceId,
    eeId,
    topologyPlan.worker_interface_id,
    clientSurfaceRole,
  );
  writes.push({
    filePath: chatRunPath,
    content: runRecordContent,
    tag: `run:${runId}`,
    expectedBeforeHash: null,
  });
  const writePaths = new Set();
  for (const write of writes) {
    if (writePaths.has(write.filePath)) {
      throw new Error(`topology plan repeats write path '${write.filePath}'`);
    }
    writePaths.add(write.filePath);
  }
  return {
    runId,
    chatInterfaceId,
    modelInterfaceId: safeSegment(topologyPlan.model_interface_id, "plan.model_interface_id"),
    providerModel: requiredString(topologyPlan.model, "plan.model"),
    workerInterfaceId: safeSegment(topologyPlan.worker_interface_id, "plan.worker_interface_id"),
    clientSurfaceRole: operatorSurfaceRole(topologyPlan.client_surface_role),
    reasoningEffort: stringValue(topologyPlan.reasoning_effort),
    serviceTier: optionalServiceTier(topologyPlan.service_tier),
    authoringProfile,
    requiredInterfaceIds,
    requiredToolIds,
    writes,
    ledgerOffset: conversationState.ledgerOffset,
  };
}

async function planOperatorChatTaskAmendment(options, request) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const registryRoot = resolveRegistryRoot(options);
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const eeInstancesRoot = await resolveEeInstancesRoot(octopusHome, options);
  const runId = safeSegment(request?.runId, "runId");
  const expectedRunHash = safeSha256(request?.expectedRunHash, "expectedRunHash");
  const runPath = path.join(runsDir, `${runId}.json`);
  const current = await readJsonDocumentIfPresent(runPath);
  if (!current || sha256Hex(Buffer.from(current.text)) !== expectedRunHash) {
    throw new Error(`operator chat run '${runId}' changed before capability amendment`);
  }
  const identity = chatIdentityFromRunRecord(config, runId, current.value);
  const clientState = requiredObject(
    requiredObject(current.value.interface_state, "run.interface_state")[identity.interfaceId],
    `run.interface_state.${identity.interfaceId}`,
  );
  const providerChoiceInterfaceId = safeSegment(
    clientState.provider_choice_interface,
    "client provider choice",
  );
  const providerInterfaceId = safeSegment(
    clientState.provider_runtime_interface,
    "client provider runtime",
  );
  const workerInterfaceId = safeSegment(
    clientState.worker_choice_interface,
    "client worker choice",
  );
  const clientSurfaceRole = operatorSurfaceRole(clientState.client_surface_role);
  const eeId = safeSegment(current.value.ee, "run.ee");
  const selectedTools = operatorSelectedTools(request?.selectedTools);
  const externalInputs = operatorExternalInputs(request?.externalInputs);
  const externalRelays = operatorExternalRelays(request?.externalRelays);
  const modelOutputs = operatorModelOutputs(request?.modelOutputs);
  const contextArtifactBindings = operatorContextArtifactBindings(request?.contextArtifactBindings);
  const publications = operatorPublications(request?.publications);
  const selectedWorkerEnvironmentKinds = operatorSelectedWorkerEnvironmentKinds(
    request?.selectedWorkerEnvironmentKinds,
  );
  const doorHostSlots = operatorDoorHostSlots(request?.doorHostSlots);
  const providerDescriptorPath = path.join(
    registryRoot,
    "interface",
    providerInterfaceId,
    "interface.json",
  );
  const providerDescriptor = await readJsonDocumentIfPresent(providerDescriptorPath);
  if (!providerDescriptor) {
    throw new Error(`operator chat provider '${providerInterfaceId}' has no exact descriptor`);
  }
  const providerPrivateConfig = requiredObject(
    requiredObject(
      providerDescriptor.value.interface_driver_config,
      "provider interface_driver_config",
    ).private_config,
    "provider private_config",
  );
  const providerModel = requiredString(providerPrivateConfig.model, "provider model");
  const reasoningEffort = stringValue(providerPrivateConfig.reasoning_effort);
  const serviceTier = optionalServiceTier(providerPrivateConfig.service_tier);
  const conversationState = await readConversationState({
    octopusHome,
    existing: current.value,
    runId,
    interfaceId: identity.interfaceId,
    terminalArtifactLabel: config.artifact_labels.terminal_output,
    errorArtifactLabel: config.artifact_labels.error_output,
    operatorMessageArtifactLabel: config.artifact_labels.message,
    continuation: null,
  });
  const topologyPlan = await planOperatorChatTopology({
    octopusHome,
    topologyPlanner: resolvePackageExecutable(
      config,
      resolveInstallationRoot(options),
      registryRoot,
      "topology_planner",
    ),
    registryRoot,
    eeInstancesRoot,
    chatId: identity.chatId,
    providerInterfaceId: providerChoiceInterfaceId,
    providerModel,
    reasoningEffort,
    serviceTier,
    eeId,
    workerInterfaceId,
    doorHostSlots,
    externalInputs,
    externalRelays,
    modelOutputs,
    contextArtifactBindings,
    publications,
    selectedWorkerEnvironmentKinds,
    selectedTools,
    clientSurfaceRole,
    cycle: randomToken(24),
    existingRunRecord: current.value,
    conversation: {
      ledger_offset: conversationState.ledgerOffset,
      artifact_bindings: conversationState.artifactBindings,
      pinned_artifact_bindings: conversationState.pinnedArtifactBindings,
      pinned_context_items: conversationState.pinnedContextItems,
      continued_from: conversationState.continuedFrom,
    },
  });
  if (
    topologyPlan.expected_before_hash_algorithm !== "sha256"
    || topologyPlan.run_id !== runId
    || topologyPlan.chat_interface_id !== identity.interfaceId
    || topologyPlan.model_interface_id !== providerInterfaceId
    || topologyPlan.model !== providerModel
    || topologyPlan.reasoning_effort !== reasoningEffort
    || (serviceTier && topologyPlan.service_tier !== serviceTier)
  ) {
    throw new Error("operator chat capability amendment changed its exact task identity");
  }
  const writes = topologyPlan.interface_writes.map((write, index) => (
    plannedClientWrite(write, index, registryRoot)
  ));
  writes.push(...plannedAdditionalRunWrites(topologyPlan, runsDir, registryRoot));
  const plannedProviderWrite = writes.find((write) => write.filePath === providerDescriptorPath);
  const providerDescriptorHash = sha256Hex(Buffer.from(providerDescriptor.text));
  if (plannedProviderWrite && plannedProviderWrite.expectedBeforeHash !== providerDescriptorHash) {
    throw new Error("operator chat capability amendment did not preserve its exact provider descriptor revision");
  }
  validatePlannedRunRecord(
    topologyPlan.run_record_content,
    runId,
    identity.interfaceId,
    eeId,
    topologyPlan.worker_interface_id,
    clientSurfaceRole,
  );
  writes.push({
    filePath: runPath,
    content: topologyPlan.run_record_content,
    tag: `run:${runId}`,
    expectedBeforeHash: expectedRunHash,
  });
  await validatePlannedHostBindings({ octopusHome, registryRoot, plan: { writes, requiredToolIds: topologyPlan.required_tool_ids } });
  return {
    runId,
    writes,
    removals: requiredArray(topologyPlan.removals, "plan.removals").map((removal, index) => (
      plannedClientRemoval(removal, index, registryRoot)
    )),
  };
}

async function planOperatorChatPackageAdoption(options, request) {
  return planPackageAdoption(options, request, (planner, payload) => runOperatorRevisionCommand(planner, "--plan-native-adoption", payload));
}

async function planPackageAdoption(options, request, planNative) {
  const config = await readConfig();
  const home = absolutePath(options.octopusHome(), "octopusHome");
  const registryRoot = resolveRegistryRoot(options);
  const runId = safeSegment(request?.runId, "runId");
  const expectedRunHash = safeSha256(request?.expectedRunHash, "expectedRunHash");
  const runsRoot = await resolveRunRecordsRoot(home);
  const runPath = path.join(runsRoot, `${runId}.json`);
  const current = await readJsonDocumentIfPresent(runPath);
  if (!current || sha256Hex(Buffer.from(current.text)) !== expectedRunHash) {
    throw new Error(`native package adoption run '${runId}' changed before planning`);
  }
  const identity = chatIdentityFromRunRecord(config, runId, current.value);
  const provider = safeSegment(current.value.interface_state[identity.interfaceId].provider_runtime_interface, "provider runtime interface");
  const providerPath = path.join(registryRoot, "interface", provider, "interface.json");
  const descriptor = await readJsonDocumentIfPresent(providerPath);
  if (!descriptor) throw new Error(`native package adoption provider '${provider}' is absent`);
  const eeInstancesRoot = await resolveEeInstancesRoot(home, options);
  const planner = resolvePackageExecutable(config, resolveInstallationRoot(options), registryRoot, "topology_planner");
  const plan = await planNative(planner, {
    roots: { registry_root: registryRoot, ee_instances_root: eeInstancesRoot,
      run_records_root: runsRoot, interface_private_state_root: path.join(home, "interfaces") },
    ledger_root: path.join(home, "ledger"), client_template_interface: config.interface,
    run_id: runId, expected_run_hash: expectedRunHash, expected_provider_hash: sha256Hex(Buffer.from(descriptor.text)),
  });
  if (plan.run_id !== runId || plan.provider_interface_id !== provider
    || !["unchanged", "deferred", "planned"].includes(plan.status)) {
    throw new Error("native package adoption planner changed its exact identity or status");
  }
  const writes = requiredArray(plan.writes, "native adoption writes");
  const catalogPath = path.join(registryRoot, "artifact", `artifact-run-affordances-${identity.chatId}`, "artifact.json");
  const worker = safeSegment(current.value.worker, "native adoption worker");
  const workerPolicyPath = path.join(registryRoot, "interface", worker, "policy.json");
  const toolPaths = new Map(requiredArray(Object.hasOwn(config, "tool_contract_updates") ? config.tool_contract_updates : [], "client.tool_contract_updates").map((update) => {
    const tool = `${safeContractToken(update.source_tool, "tool contract source tool")}__${identity.chatId}`;
    const door = `${safeSegment(update.source_interface, "tool contract source interface")}_${identity.chatId}`;
    return [path.join(registryRoot, "tool", tool, "tool.json"), { tool, door }];
  }));
  const policyWrite = writes.find(write => write.path === workerPolicyPath);
  const runWrite = writes.find(write => write.path === runPath);
  let workerOwnership;
  if (plan.status === "planned") {
    const toolWrites = writes.filter(write => toolPaths.has(write.path));
    const nativeWrites = writes.filter(write => !toolPaths.has(write.path) && write !== policyWrite && write !== runWrite);
    const nativePathsValid = nativeWrites.length === 0
      || nativeWrites.length === 1 && nativeWrites[0].path === providerPath
      || nativeWrites.length === 2 && nativeWrites[0].path === catalogPath && nativeWrites[1].path === providerPath;
    const ordered = [...toolWrites, ...(policyWrite ? [policyWrite] : []), ...(runWrite ? [runWrite] : []), ...nativeWrites];
    if (!writes.length || new Set(writes.map((write) => write.path)).size !== writes.length
      || ordered.some((write, index) => write !== writes[index]) || policyWrite && !runWrite
      || !nativePathsValid) {
      throw new Error("native package adoption may write only declared private tools, its exact worker policy and target Run, followed by its exact provider declaration with an optional preceding catalog");
    }
    for (const write of writes) {
      safeSha256(write.expected_before_hash, "native adoption write preimage");
      requiredString(write.content, "native adoption content");
      if (write.path === providerPath && JSON.parse(write.content).id !== provider) {
        throw new Error("provider contract adoption changed its exact private provider identity");
      }
      const expected = toolPaths.get(write.path);
      if (expected) {
        const descriptor = JSON.parse(write.content);
        if (descriptor.id !== expected.tool || descriptor.interface !== expected.door) {
          throw new Error("tool contract adoption changed its exact private tool or door identity");
        }
      }
    }
    if (runWrite) {
      const next = requiredObject(JSON.parse(runWrite.content), "native adoption target Run");
      const nextIdentity = chatIdentityFromRunRecord(config, runId, next);
      if (runWrite.tag !== `run:${runId}` || runWrite.expected_before_hash !== expectedRunHash
        || next.worker !== worker || next.ee !== current.value.ee || nextIdentity.interfaceId !== identity.interfaceId
        || next.interface_state[identity.interfaceId].provider_runtime_interface !== provider) {
        throw new Error("native result adoption changed its exact target Run or owner identity");
      }
    }
    if (policyWrite) {
      const next = requiredObject(JSON.parse(policyWrite.content), "native adoption worker policy");
      workerOwnership = await readJsonDocumentIfPresent(path.join(registryRoot, "interface", worker, "octopus_os.json"));
      const owner = workerOwnership?.value;
      if (policyWrite.tag !== `policy:${worker}` || next.id !== worker || owner?.schema_version !== 1
        || owner.workspace !== "octopus_os" || owner.interface !== worker || owner.owner_interface !== identity.interfaceId
        || owner.component !== "worker" || owner.surface_role !== current.value.interface_state[identity.interfaceId].client_surface_role) {
        throw new Error("native result adoption policy is not owned by its exact client worker");
      }
    }
  } else if (writes.length) throw new Error("deferred or unchanged native adoption contains writes");
  if (plan.status === "deferred") requiredString(plan.reason, "native adoption deferral reason");
  const evidencePaths = new Set([runPath, path.join(home, "ledger/movements.jsonl"), ...writes.flatMap((write) => {
    const tool = toolPaths.get(write.path);
    return tool ? [path.join(home, "interfaces", tool.door, "tool-door-effect-frontier.jsonl"),
      path.join(home, "interfaces", tool.door, "ledger-cursor.tool-door.json")] : [];
  })]);
  const bound = current.value.interface_state[provider]?.run_authoring_binding;
  for (const checkpoint of bound?.checkpoints ?? []) {
    const door = checkpoint.role === "provider_tool_output" && checkpoint.outbound_interface_bindings?.length === 1
      ? checkpoint.outbound_interface_bindings[0].interface
      : checkpoint.role === "authoring_result" ? checkpoint.interface_binding?.interface : null;
    if (door === null) continue;
    const root = path.join(home, "interfaces", safeSegment(door, "native adoption evidence door"));
    for (const name of ["tool-door-effect-frontier.jsonl", "ledger-cursor.tool-door.json"]) evidencePaths.add(path.join(root, name));
  }
  const preimages = requiredArray(plan.preimages, "native adoption preimages"), pins = new Map();
  for (const pin of preimages) {
    if (pins.has(pin.path) && pins.get(pin.path) !== pin.sha256) throw new Error("native adoption repeats inconsistent dependency preimages");
    pins.set(pin.path, pin.sha256);
  }
  const ancestors = new Set();
  for (let ee = safeSegment(current.value.ee, "native adoption Run EE"); ee !== undefined && ee !== null;) {
    const file = path.join(eeInstancesRoot, `${safeSegment(ee, "native adoption EE")}.json`);
    if (ancestors.has(file)) throw new Error("native adoption EE chain cycles");
    ancestors.add(file); evidencePaths.add(file);
    const document = await readJsonDocumentIfPresent(file);
    if (!document || pins.get(file) !== sha256Hex(Buffer.from(document.text))) throw new Error(`native adoption preimage changed at '${file}'`);
    ee = document.value.parent_ee_id;
  }
  const deliveryRoot = path.join(home, "interfaces", provider, "handoff-delivery");
  const grantRoot = path.join(home, "ledger", "metadata");
  for (const preimage of preimages) {
    const file = absolutePath(preimage.path, "native adoption preimage path");
    const delivery = path.dirname(file) === deliveryRoot && /^[0-9a-f]{64}\.json$/.test(path.basename(file));
    const grant = path.dirname(file) === grantRoot && /^[0-9a-f]{64}$/.test(path.basename(file)) && preimage.sha256 !== null;
    if (file !== preimage.path || !file.startsWith(`${registryRoot}${path.sep}`) && !evidencePaths.has(file) && !delivery && !grant) {
      throw new Error("native adoption preimage is outside its exact run, Registry and tool completion evidence");
    }
    if (preimage.sha256 !== null) safeSha256(preimage.sha256, "native adoption preimage hash");
  }
  if (plan.status === "planned") {
    for (const write of writes) if (pins.get(write.path) !== write.expected_before_hash) throw new Error("native adoption write lacks its exact dependency preimage");
    if (workerOwnership && pins.get(path.join(registryRoot, "interface", worker, "octopus_os.json")) !== sha256Hex(Buffer.from(workerOwnership.text))) {
      throw new Error("native adoption worker ownership lacks its exact dependency preimage");
    }
  }
  return plan;
}

async function adoptOperatorChatPackageUpdates(options, request) {
  const assertStopped = requiredFunction(options?.assertStopped, "native adoption stopped-owner guard");
  const commit = requiredFunction(options?.commitClientAuthoring, "native adoption client transaction");
  const onResult = requiredFunction(options?.onResult, "native adoption result projection");
  const seen = new Set();
  const results = [];
  const rows = requiredArray(request?.clientRuns, "native adoption clientRuns");
  if (!rows.length) return { changed: false, results };
  const config = await readConfig();
  const planner = resolvePackageExecutable(config, resolveInstallationRoot(options), resolveRegistryRoot(options), "topology_planner");
  const batch = openNativeAdoptionBatch(planner, { timeoutMs: config.authoring.result_timeout_ms, signal: options.signal });
  try {
    for (const row of rows) {
      const runId = safeSegment(row.run_id, "native adoption run");
      if (seen.has(runId)) throw new Error(`native adoption repeats run '${runId}'`);
      seen.add(runId);
      const result = await withRunAuthoringLock(runId, async () => {
        await assertStopped();
        const plan = await planPackageAdoption(options, { runId, expectedRunHash: row.record_digest }, (binary, payload) => {
          if (binary !== planner) throw new Error("Native adoption planner changed during its batch");
          return batch.plan(payload);
        });
        if (plan.status === "planned") {
          for (const preimage of plan.preimages) {
            const current = await readFileHashIfPresent(preimage.path);
            if ((current?.hash ?? null) !== preimage.sha256) throw new Error(`native adoption preimage changed at '${preimage.path}'`);
          }
          await assertStopped();
          batch.assertActive();
          await commit({ label: `adopt client package declarations for ${runId}`,
            condition: { octopus_home: absolutePath(options.octopusHome(), "octopusHome"), preimages: plan.preimages },
            writes: plan.writes.map((write) => ({file:write.path,content:write.content,beforeDigest:write.expected_before_hash,label:write.tag})),
            removals: [], publishBarrierPath: plan.writes.at(-1).path });
          for (const write of plan.writes) {
            const current = await readFileHashIfPresent(write.path);
            if (current?.hash !== sha256Hex(Buffer.from(write.content))) throw new Error(`native adoption readback differs at '${write.path}'`);
          }
        }
        return { runId, providerInterfaceId: plan.provider_interface_id,
          status: plan.status === "planned" ? "adopted" : plan.status,
          sourceMovementSeq: plan.source_movement_seq, reason: plan.reason };
      });
      results.push(result);
      await onResult(result);
    }
    await batch.finish();
    return { changed: results.some((row) => row.status === "adopted"), results };
  } catch (error) { batch.abort(error); throw error; }
  finally { await batch.close(); }
}
