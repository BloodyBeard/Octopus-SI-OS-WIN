const { spawn } = require("node:child_process");
const crypto = require("node:crypto");
const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const net = require("node:net");
const path = require("node:path");

const CONFIG_PATH = path.join(__dirname, "client.json");
const LIVE_EVENT_DATA_MAX_BYTES = 8 * 1024;
const LIVE_EVENT_DATA_MAX_STRING_LENGTH = 1_024;
const LIVE_EVENT_DATA_MAX_ARRAY_ITEMS = 12;
const LIVE_EVENT_DATA_MAX_OBJECT_KEYS = 48;
const LIVE_EVENT_DATA_MAX_DEPTH = 4;
const LIVE_STREAM_ARTIFACT_LABEL = "turn.stream";
const LIVE_STREAM_SCHEMA_VERSION = 1;
const LIVE_STREAM_MAX_BYTES = 256 * 1024 * 1024;
const LIVE_STREAM_MAX_HEADER_BYTES = 64 * 1024;
const LIVE_STREAM_MAX_MANIFEST_BYTES = 64 * 1024;
const WORKER_ENVIRONMENT_RELEASE_LABEL = "worker_environment.release";
const WORKER_ENVIRONMENT_RELEASED_LABEL = "worker_environment.released";
const PROVIDER_ACCOUNT_EVIDENCE_SCHEMA = "octopus.provider_account_observation";
const PROVIDER_ACCOUNT_REQUEST_LABEL = "provider.account.read";
const PROVIDER_ACCOUNT_SNAPSHOT_LABEL = "provider.account.snapshot";
const PROVIDER_ACCOUNT_LOGIN_START_REQUEST_LABEL = "provider.account.login.start";
const PROVIDER_ACCOUNT_LOGIN_STARTED_LABEL = "provider.account.login.started";
const PROVIDER_ACCOUNT_LOGIN_STATUS_REQUEST_LABEL = "provider.account.login.status.read";
const PROVIDER_ACCOUNT_LOGIN_STATUS_LABEL = "provider.account.login.status";
const PROVIDER_ACCOUNT_LOGIN_COMPLETED_LABEL = "provider.account.login.completed";
const PROVIDER_ACCOUNT_UPDATED_LABEL = "provider.account.updated";
const PROVIDER_OPERATION_ERROR_LABEL = "provider.operation.error";
const PROVIDER_MODELS_LIST_LABEL = "provider.models.list";
const PROVIDER_MODELS_CATALOG_LABEL = "provider.models.catalog";
const OPERATOR_MESSAGE_ADMISSION_SCHEMA = "octopus.operator_message_admission";
const OPERATOR_MESSAGE_INTENT_SCHEMA = "octopus.operator_message_intent";
const MAX_OPERATOR_ATTACHMENT_BYTES = 100 * 1024 * 1024;
const MAX_OPERATOR_ATTACHMENT_COUNT = 12;
const MAX_OPERATOR_ATTACHMENT_TOTAL_BYTES = 512 * 1024 * 1024;
const runAuthoringLocks = new Map();
const operatorMessageAdmissionLocks = new Map();
const workspaceTopologyAuthoringLocks = new Map();
const liveLedgerCursors = new Map();
const liveCursorWriteTails = new Map();
const ledgerChangeHubs = new Map();

async function submitOperatorTurn(options, request) {
  const config = await readConfig();
  const text = stringValue(request?.text);
  const attachmentInputs = arrayValue(request?.attachments);
  assertOperatorAttachmentInputLimits(attachmentInputs);
  const recoverPreparedMessage = request?.recoverPreparedMessage === true;
  if (recoverPreparedMessage && attachmentInputs.length > 0) {
    throw new Error("recoverPreparedMessage cannot carry transient attachments");
  }
  if (!text && attachmentInputs.length === 0 && !recoverPreparedMessage) {
    throw new Error("operator turn must carry text or an attachment");
  }
  const providerReference = safeSegment(request?.providerInterfaceId, "providerInterfaceId");
  const providerModel = requiredString(request?.providerModel, "providerModel");
  const clientSurfaceRole = operatorSurfaceRole(request?.clientSurfaceRole);
  const messageId = safeContractToken(
    stringValue(request?.messageId) || `msg_${randomToken(20)}`,
    "messageId",
  );
  const cycle = randomToken(24);
  const timeoutMs = numberValue(request?.timeoutMs, 120_000);
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const runPrefix = requiredString(config.run.run_prefix, "client.run.run_prefix");
  const requestedRunId = stringValue(request?.runId);
  const continuation = requestedRunId
    ? await resolveContinuationState({
      config,
      octopusHome,
      requestedRunId,
      request: request?.continuation,
    })
    : null;
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  let runId;
  let chatId;
  let chatInterfaceId;
  let existingRunDocument = null;
  let existingConversationState = null;
  let stableExistingSubmission = null;
  if (requestedRunId) {
    runId = safeSegment(requestedRunId, "runId");
    existingRunDocument = await readJsonDocumentIfPresent(path.join(runsDir, `${runId}.json`));
    if (!existingRunDocument) throw new Error(`operator chat run '${runId}' does not exist`);
    const identity = chatIdentityFromRunRecord(config, runId, existingRunDocument.value);
    chatId = identity.chatId;
    chatInterfaceId = identity.interfaceId;
  } else {
    chatId = operatorMessageChatId(messageId);
    runId = `${runPrefix}${chatId}`;
    chatInterfaceId = `${config.run.interface_prefix}${chatId}`;
    existingRunDocument = await readJsonDocumentIfPresent(path.join(runsDir, `${runId}.json`));
  }
  if (existingRunDocument) {
    const identity = chatIdentityFromRunRecord(config, runId, existingRunDocument.value);
    if (identity.chatId !== chatId || identity.interfaceId !== chatInterfaceId) {
      throw new Error(`operator chat run '${runId}' changed its exact client interface`);
    }
    stableExistingSubmission = existingRunTopology(
      config,
      runId,
      existingRunDocument.value,
      chatInterfaceId,
      {
        providerChoiceInterfaceId: providerReference,
        clientSurfaceRole,
        eeId: stringValue(request?.eeId),
        workerInterfaceId: stringValue(request?.workerInterfaceId),
      },
    );
    existingConversationState = await readConversationState({
      octopusHome,
      existing: existingRunDocument.value,
      runId,
      interfaceId: chatInterfaceId,
      terminalArtifactLabel: config.artifact_labels.terminal_output,
      errorArtifactLabel: config.artifact_labels.error_output,
      continuation,
    });
  }
  const clientRoot = absolutePath(
    options.clientArtifactsRoot(chatInterfaceId),
    "clientArtifactsRoot",
  );
  const chatRunPath = path.join(runsDir, `${runId}.json`);

  const artifactRoot = path.join(clientRoot, "operator-chat");
  const payloadRoot = path.join(artifactRoot, "payloads");
  const attachmentRoot = path.join(artifactRoot, "attachments");
  await fs.mkdir(payloadRoot, { recursive: true });
  await fs.mkdir(attachmentRoot, { recursive: true });
  let pendingAttachmentPlans = null;
  let envelope = await withOperatorMessageAdmissionLock(
    chatInterfaceId,
    messageId,
    () => readExistingOperatorMessageEnvelope(options, {
      artifactRoot,
      attachmentInputs,
      attachmentRoot,
      chatInterfaceId,
      clientSurfaceRole: stableExistingSubmission?.clientSurfaceRole ?? clientSurfaceRole,
      label: config.artifact_labels.message,
      messageId,
      modelInterfaceId: stableExistingSubmission?.modelInterfaceId ?? providerReference,
      octopusHome,
      recoverPreparedMessage,
      requestedText: text,
      runDocumentPresent: Boolean(existingRunDocument),
      runId,
      workerInterfaceId: stableExistingSubmission?.workerInterfaceId
        ?? safeSegment(request?.workerInterfaceId || config.run.worker, "workerInterfaceId"),
    }),
  );
  if (!envelope) {
    if (recoverPreparedMessage) {
      throw new Error(`operator message '${messageId}' has no durable envelope; reattach its files before retrying`);
    } else {
      pendingAttachmentPlans = await planOperatorAttachments(attachmentRoot, attachmentInputs);
      const attachments = pendingAttachmentPlans.map((plan) => plan.attachment);
      const messageBytes = Buffer.from(JSON.stringify({
        text,
        message_id: messageId,
        attachments,
      }));
      const messagePath = operatorMessagePayloadPath(payloadRoot, messageId);
      await writeExactOperatorMessageFile(messagePath, messageBytes, "payload");
      envelope = {
        messageBytes,
        messagePath,
        payload: operatorMessagePayload(JSON.parse(messageBytes.toString("utf8")), messageId, attachmentRoot),
        recovered: null,
      };
    }
  }

  let preparedPayloadSelection = null;
  let authoredRequest = request;
  await withOperatorMessageAdmissionLock(chatInterfaceId, messageId, async () => {
    const resolved = { clientSurfaceRole, providerReference };
    const sourceRequestDigest = operatorMessageRequestDigest(config, request, resolved);
    if (options.preparePayloadArtifactBindings !== undefined) {
      if (typeof options.preparePayloadArtifactBindings !== "function") {
        throw new Error("preparePayloadArtifactBindings must be a trusted client callback");
      }
      preparedPayloadSelection = await prepareOperatorPayloadArtifactBindings(options, {
        config, request, existingRunDocument, artifactRoot, chatInterfaceId, messageId, runId,
        clientSurfaceRole, sourceRequestDigest,
      });
      await assertPreparedOperatorPayloadSelection(preparedPayloadSelection, await readJsonDocumentIfPresent(chatRunPath));
      authoredRequest = { ...request, payloadArtifactBindings: preparedPayloadSelection.bindings };
    }
    await readOrCreateOperatorMessageIntent({
      artifactRoot, chatInterfaceId, messageBytes: envelope.messageBytes, messageId, runId,
      requestDigest: operatorMessageRequestDigest(config, authoredRequest, resolved),
      ...(preparedPayloadSelection ? {
        sourceRequestDigest, payloadArtifactBindings: preparedPayloadSelection.bindings,
      } : {}),
    });
  });
  if (envelope.recovered) {
    return {
      runId,
      chatInterfaceId,
      modelInterfaceId: stableExistingSubmission.modelInterfaceId,
      workerInterfaceId: stableExistingSubmission.workerInterfaceId,
      clientSurfaceRole: stableExistingSubmission.clientSurfaceRole,
      label: config.artifact_labels.message,
      messageId,
      payloadPath: envelope.messagePath,
      contentHash: sha256Hex(envelope.messageBytes),
      attachmentCount: envelope.payload.attachments.length,
      clientWrites: [],
      messageRecovered: true,
      messageVerdict: envelope.recovered.verdict,
      outboundCursor: {
        offset: envelope.recovered.outboundCursorOffset,
        seq: null,
      },
    };
  }

  if (pendingAttachmentPlans) {
    await materializeAttachments(pendingAttachmentPlans);
  }

  if (existingRunDocument) {
    const recovered = await withOperatorMessageAdmissionLock(
      chatInterfaceId,
      messageId,
      () => prepareOperatorMessageAdmission(options, {
        artifactRoot,
        chatInterfaceId,
        clientSurfaceRole: stableExistingSubmission.clientSurfaceRole,
        label: config.artifact_labels.message,
        messageBytes: envelope.messageBytes,
        messageId,
        messagePath: envelope.messagePath,
        modelInterfaceId: stableExistingSubmission.modelInterfaceId,
        octopusHome,
        outboundCursorOffset: existingConversationState.ledgerOffset,
        runId,
        workerInterfaceId: stableExistingSubmission.workerInterfaceId,
      }),
    );
    if (recovered) {
      return {
        runId,
        chatInterfaceId,
        modelInterfaceId: stableExistingSubmission.modelInterfaceId,
        workerInterfaceId: stableExistingSubmission.workerInterfaceId,
        clientSurfaceRole: stableExistingSubmission.clientSurfaceRole,
        label: config.artifact_labels.message,
        messageId,
        payloadPath: envelope.messagePath,
        contentHash: sha256Hex(envelope.messageBytes),
        attachmentCount: envelope.payload.attachments.length,
        clientWrites: [],
        messageRecovered: true,
        messageVerdict: recovered.verdict,
        outboundCursor: {
          offset: recovered.outboundCursorOffset,
          seq: null,
        },
      };
    }
  }

  const authorSubmission = () => withRunAuthoringLock(runId, async () => {
    const currentRunDocument = await readJsonDocumentIfPresent(chatRunPath);
    if (preparedPayloadSelection) {
      await assertPreparedOperatorPayloadSelection(preparedPayloadSelection, currentRunDocument);
    }
    if (!currentRunDocument) {
      if (requestedRunId) {
        throw new Error(`operator chat run '${runId}' no longer exists`);
      }
      const plan = await planOperatorChatSurface(options, {
        chatId,
        cycle,
        providerInterfaceId: providerReference,
        providerModel,
        reasoningEffort: request?.reasoningEffort,
        serviceTier: request?.serviceTier,
        eeId: request?.eeId,
        workerInterfaceId: request?.workerInterfaceId,
        doorHostSlots: request?.doorHostSlots,
        externalInputs: request?.externalInputs,
        externalRelays: request?.externalRelays,
        modelOutputs: request?.modelOutputs,
        contextArtifactBindings: request?.contextArtifactBindings,
        payloadArtifactBindings: authoredRequest?.payloadArtifactBindings,
        selectedWorkerEnvironmentKinds: request?.selectedWorkerEnvironmentKinds,
        selectedTools: request?.selectedTools,
        clientSurfaceRole,
        continuation: request?.continuation,
      });
      return {
        submission: {
          modelInterfaceId: plan.modelInterfaceId,
          workerInterfaceId: plan.workerInterfaceId,
          clientSurfaceRole: plan.clientSurfaceRole,
        },
        clientWrites: await commitClientWrites(plan.writes),
        outboundCursorOffset: plan.ledgerOffset,
      };
    }
    const existingChat = currentRunDocument.value;
    const identity = chatIdentityFromRunRecord(config, runId, existingChat);
    if (identity.chatId !== chatId || identity.interfaceId !== chatInterfaceId) {
      throw new Error(`operator chat run '${runId}' changed its exact client interface`);
    }
    const activeRun = existingRunSubmission(config, runId, existingChat, chatInterfaceId, {
      providerChoiceInterfaceId: providerReference,
      clientSurfaceRole,
      eeId: stringValue(request?.eeId),
      workerInterfaceId: stringValue(request?.workerInterfaceId),
    });
    if (activeRun) {
      const operatorMessageCheckpoint = activeOperatorMessageCheckpoint(
        existingChat,
        runId,
        chatInterfaceId,
        activeRun.modelInterfaceId,
        config.artifact_labels.message,
      );
      const conversationState = await readConversationState({
        octopusHome,
        existing: existingChat,
        runId,
        interfaceId: chatInterfaceId,
        terminalArtifactLabel: config.artifact_labels.terminal_output,
        errorArtifactLabel: config.artifact_labels.error_output,
        operatorMessageArtifactLabel: config.artifact_labels.message,
        operatorMessageCheckpoint,
        continuation,
      });
      const providerSelectionWrites = await planExistingProviderSelection(options, {
        chatId,
        chatInterfaceId,
        clientSurfaceRole,
        conversationState,
        currentRunDocument,
        providerChoiceInterfaceId: providerReference,
        providerModel,
        reasoningEffort: request?.reasoningEffort,
        serviceTier: request?.serviceTier,
        payloadArtifactBindings: authoredRequest?.payloadArtifactBindings,
        preparedPayloadSelection,
        runId,
        authorNextCycle: conversationState.terminalAfterOperatorMessage,
      });
      const clientDirectory = path.join(resolveRegistryRoot(options), "interface", chatInterfaceId);
      const changesClientConfiguration = (filePath) => path.dirname(filePath) === clientDirectory
        && path.extname(filePath) === ".json";
      const previousSocket = providerSelectionWrites.some((write) => changesClientConfiguration(write.filePath))
        ? await interfaceSocketIdentity(octopusHome, chatInterfaceId) : null;
      const clientWrites = await commitClientWrites(providerSelectionWrites);
      return {
        submission: activeRun,
        clientWrites,
        previousClientSocket: clientWrites.some((write) => write.changed && changesClientConfiguration(write.path))
          ? previousSocket : null,
        outboundCursorOffset: conversationState.ledgerOffset,
      };
    }
    throw new Error(`operator chat run '${runId}' has no active exact client input`);
  });
  const { submission, clientWrites, outboundCursorOffset, previousClientSocket } = requestedRunId
    ? await authorSubmission()
    : await withOperatorWorkspaceTopologyLock(octopusHome, authorSubmission);

  await waitForInterfaceSocket(octopusHome, chatInterfaceId, timeoutMs, previousClientSocket);
  const messageAdmission = await withOperatorMessageAdmissionLock(
    chatInterfaceId,
    messageId,
    () => admitOperatorMessage(options, {
      artifactRoot,
      chatInterfaceId,
      clientSurfaceRole: submission.clientSurfaceRole,
      label: config.artifact_labels.message,
      messageBytes: envelope.messageBytes,
      messageId,
      messagePath: envelope.messagePath,
      modelInterfaceId: submission.modelInterfaceId,
      octopusHome,
      outboundCursorOffset,
      runId,
      timeoutMs,
      workerInterfaceId: submission.workerInterfaceId,
    }),
  );
  return {
    runId,
    chatInterfaceId,
    modelInterfaceId: submission.modelInterfaceId,
    workerInterfaceId: submission.workerInterfaceId,
    clientSurfaceRole: submission.clientSurfaceRole,
    label: config.artifact_labels.message,
    messageId,
    payloadPath: envelope.messagePath,
    contentHash: sha256Hex(envelope.messageBytes),
    attachmentCount: envelope.payload.attachments.length,
    clientWrites,
    messageRecovered: messageAdmission.recovered,
    messageVerdict: messageAdmission.verdict,
    outboundCursor: {
      offset: messageAdmission.outboundCursorOffset,
      seq: null,
    },
  };
}

async function prepareOperatorPayloadArtifactBindings(options, input) {
  const requested = input.request?.payloadArtifactBindings === undefined
    ? undefined : operatorPayloadArtifactBindings(input.request.payloadArtifactBindings);
  let sidecar = null;
  let sidecarPath = null;
  let previous = [];
  if (input.existingRunDocument) {
    sidecarPath = path.join(resolveRegistryRoot(options), "interface", input.chatInterfaceId, "octopus_os.json");
    sidecar = await readJsonDocumentIfPresent(sidecarPath);
    const value = requiredObject(sidecar?.value, "operator payload authoring sidecar");
    const profile = requiredObject(value.authoring_profile, "operator payload authoring profile");
    const state = requiredObject(input.existingRunDocument.value.interface_state?.[input.chatInterfaceId], "operator payload client state");
    if (value.schema_version !== 1 || value.workspace !== "octopus_os" || value.component !== "client"
      || value.interface !== input.chatInterfaceId || value.owner_interface !== input.chatInterfaceId
      || value.surface_role !== input.clientSurfaceRole
      || profile.ee_id !== input.existingRunDocument.value.ee
      || profile.worker_choice_interface !== state.worker_choice_interface) {
      throw new Error("operator payload profile differs from its exact run, client or authoring owner");
    }
    const fields = new Set(["ee_id", "worker_choice_interface", "door_host_slots", "external_inputs", "external_relays",
      "model_outputs", "context_artifact_bindings", "payload_artifact_bindings", "selected_worker_environment_kinds", "selected_tools"]);
    if (Object.keys(profile).some((field) => !fields.has(field))) throw new Error("operator payload profile has undeclared fields");
    operatorDoorHostSlots(requiredObject(profile.door_host_slots, "authoring_profile.door_host_slots"));
    for (const field of ["external_inputs", "external_relays", "model_outputs", "context_artifact_bindings", "selected_tools"]) {
      if (Object.hasOwn(profile, field)) {
        for (const entry of requiredArray(profile[field], `authoring_profile.${field}`)) {
          requiredObject(entry, `authoring_profile.${field} entry`);
        }
      }
    }
    operatorContextArtifactBindings(profile.context_artifact_bindings);
    if (Object.hasOwn(profile, "selected_worker_environment_kinds")) {
      operatorSelectedWorkerEnvironmentKinds(requiredArray(profile.selected_worker_environment_kinds, "authoring_profile.selected_worker_environment_kinds"));
    }
    previous = operatorPayloadArtifactBindings(profile.payload_artifact_bindings);
  }
  const intentPath = path.join(input.artifactRoot, "message-intents", `${input.messageId}.json`);
  const saved = await readJsonDocumentIfPresent(intentPath);
  let bindings;
  if (saved) {
    if (saved.value.source_request_sha256 !== input.sourceRequestDigest
      || saved.value.run_id !== input.runId || saved.value.interface_id !== input.chatInterfaceId
      || saved.value.message_id !== input.messageId || !Object.hasOwn(saved.value, "payload_artifact_bindings")) {
      throw new Error("operator payload staging conflicts with its original durable intent");
    }
    bindings = operatorPayloadArtifactBindings(saved.value.payload_artifact_bindings);
  } else {
    const base = requested ?? previous;
    const proposed = await options.preparePayloadArtifactBindings({
      runId: input.runId, chatInterfaceId: input.chatInterfaceId, messageId: input.messageId,
      previousPayloadArtifactBindings: operatorPayloadArtifactBindings(previous),
      requestedPayloadArtifactBindings: requested === undefined ? undefined : operatorPayloadArtifactBindings(requested),
      payloadArtifactBindings: operatorPayloadArtifactBindings(base),
    });
    if (!Array.isArray(proposed)) throw new Error("payload preparation must return an explicit ArtifactRef array");
    bindings = operatorPayloadArtifactBindings(proposed);
    if (bindings.length < base.length || base.some((ref, index) => canonicalJson(ref) !== canonicalJson(bindings[index]))) {
      throw new Error("payload staging changed its explicitly selected base or an existing artifact content hash");
    }
  }
  const context = sidecar ? sidecar.value.authoring_profile.context_artifact_bindings : input.request?.contextArtifactBindings;
  const contextIds = new Set(operatorContextArtifactBindings(context).map((binding) => binding.id));
  if (bindings.some((reference) => contextIds.has(reference.id))) {
    throw new Error("payload staging collides with an explicit context artifact");
  }
  return { bindings, sidecar, sidecarPath, runText: input.existingRunDocument?.text ?? null };
}

async function assertPreparedOperatorPayloadSelection(prepared, currentRunDocument) {
  if ((currentRunDocument?.text ?? null) !== prepared.runText) {
    throw new Error("operator payload staging RunRecord preimage changed before authoring");
  }
  if (prepared.sidecar && await fs.readFile(prepared.sidecarPath, "utf8") !== prepared.sidecar.text) {
    throw new Error("operator payload staging private profile preimage changed before authoring");
  }
}

async function planExistingProviderSelection(options, input) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const registryRoot = resolveRegistryRoot(options);
  const eeInstancesRoot = await resolveEeInstancesRoot(octopusHome, options);
  const clientState = requiredObject(
    requiredObject(input.currentRunDocument.value.interface_state, "run.interface_state")[input.chatInterfaceId],
    `run.interface_state.${input.chatInterfaceId}`,
  );
  const providerInterfaceId = safeSegment(
    clientState.provider_runtime_interface,
    "provider runtime interface",
  );
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
  const privateConfig = requiredObject(
    requiredObject(
      providerDescriptor.value.interface_driver_config,
      "provider interface_driver_config",
    ).private_config,
    "provider private_config",
  );
  const currentModel = requiredString(privateConfig.model, "provider model");
  const currentServiceTier = stringValue(privateConfig.service_tier);
  const requestedServiceTier = optionalServiceTier(input.serviceTier);
  const currentReasoningEffort = stringValue(privateConfig.reasoning_effort);
  const requestedReasoningEffort = stringValue(input.reasoningEffort);
  const providerSelectionChanged = !(
    currentModel === input.providerModel
    && (!requestedServiceTier || currentServiceTier === requestedServiceTier)
    && (!requestedReasoningEffort || currentReasoningEffort === requestedReasoningEffort)
  );
  const sidecarPath = path.join(
    registryRoot,
    "interface",
    input.chatInterfaceId,
    "octopus_os.json",
  );
  if (!providerSelectionChanged && !input.authorNextCycle && input.payloadArtifactBindings === undefined) return [];
  if (input.preparedPayloadSelection && input.preparedPayloadSelection.sidecarPath !== sidecarPath) {
    throw new Error("operator payload staging changed its original registry owner path");
  }
  const sidecar = input.preparedPayloadSelection
    ? input.preparedPayloadSelection.sidecar : await readJsonDocumentIfPresent(sidecarPath);
  const authoringProfile = requiredObject(
    sidecar?.value?.authoring_profile,
    `operator chat sidecar '${input.chatInterfaceId}' authoring_profile`,
  );
  const previousPayloadBindings = operatorPayloadArtifactBindings(authoringProfile.payload_artifact_bindings);
  const payloadArtifactBindings = input.payloadArtifactBindings === undefined
    ? previousPayloadBindings
    : operatorPayloadArtifactBindings(input.payloadArtifactBindings);
  const payloadSelectionChanged = canonicalJson(previousPayloadBindings) !== canonicalJson(payloadArtifactBindings);
  let authorNextCycle = input.authorNextCycle;
  if (payloadSelectionChanged) {
    assertPayloadInputCycleReady(input.currentRunDocument.value, providerInterfaceId, input.conversationState);
    authorNextCycle = true;
  }
  if (!providerSelectionChanged && !authorNextCycle) return [];
  const topologyServiceTier = requestedServiceTier || currentServiceTier;
  const topologyReasoningEffort = !providerSelectionChanged && authorNextCycle
    ? currentReasoningEffort
    : requestedReasoningEffort;
  const expectedProfile = { ...authoringProfile };
  if (payloadArtifactBindings.length) expectedProfile.payload_artifact_bindings = payloadArtifactBindings;
  else delete expectedProfile.payload_artifact_bindings;
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
    chatId: input.chatId,
    providerInterfaceId: input.providerChoiceInterfaceId,
    providerModel: input.providerModel,
    reasoningEffort: topologyReasoningEffort || null,
    serviceTier: topologyServiceTier,
    eeId: safeSegment(input.currentRunDocument.value.ee, "run.ee"),
    workerInterfaceId: safeSegment(
      clientState.worker_choice_interface,
      "client worker choice",
    ),
    doorHostSlots: requiredObject(authoringProfile.door_host_slots, "authoring_profile.door_host_slots"),
    externalInputs: arrayValue(authoringProfile.external_inputs),
    externalRelays: arrayValue(authoringProfile.external_relays),
    modelOutputs: arrayValue(authoringProfile.model_outputs),
    contextArtifactBindings: arrayValue(authoringProfile.context_artifact_bindings),
    payloadArtifactBindings,
    selectedWorkerEnvironmentKinds: arrayValue(authoringProfile.selected_worker_environment_kinds),
    selectedTools: arrayValue(authoringProfile.selected_tools),
    clientSurfaceRole: input.clientSurfaceRole,
    cycle: randomToken(24),
    existingRunRecord: input.currentRunDocument.value,
    conversation: {
      ledger_offset: input.conversationState.ledgerOffset,
      artifact_bindings: input.conversationState.artifactBindings,
      pinned_artifact_bindings: input.conversationState.pinnedArtifactBindings,
      pinned_context_items: input.conversationState.pinnedContextItems,
      continued_from: input.conversationState.continuedFrom,
    },
  });
  if (
    topologyPlan.expected_before_hash_algorithm !== "sha256"
    || topologyPlan.run_id !== input.runId
    || topologyPlan.chat_interface_id !== input.chatInterfaceId
    || topologyPlan.model_interface_id !== providerInterfaceId
    || topologyPlan.model !== input.providerModel
    || (topologyReasoningEffort && topologyPlan.reasoning_effort !== topologyReasoningEffort)
    || (topologyServiceTier && topologyPlan.service_tier !== topologyServiceTier)
  ) {
    throw new Error("operator chat provider selection changed its exact task identity");
  }
  if (canonicalJson(topologyPlan.authoring_profile) !== canonicalJson(expectedProfile)) {
    throw new Error("next-cycle payload selection differs from its explicit authoring profile");
  }
  const removals = requiredArray(topologyPlan.removals, "plan.removals");
  if (removals.length > 0) {
    throw new Error("operator chat provider selection unexpectedly removes private capabilities");
  }
  const writes = topologyPlan.interface_writes.map((write, index) => (
    plannedClientWrite(write, index, registryRoot)
  ));
  const providerWrite = writes.find((write) => write.filePath === providerDescriptorPath);
  if (providerSelectionChanged && !providerWrite) {
    throw new Error("operator chat provider selection produced no exact provider write");
  }
  const selectedWrites = plannedAdditionalRunWrites(topologyPlan, await resolveRunRecordsRoot(octopusHome));
  if (providerWrite) {
    const providerDescriptorHash = sha256Hex(Buffer.from(providerDescriptor.text));
    if (providerWrite.expectedBeforeHash !== providerDescriptorHash) {
      throw new Error("operator chat provider selection did not preserve its exact descriptor revision");
    }
    providerWrite.content = selectedProviderDescriptorContent(
      providerDescriptor.text,
      providerWrite.content,
      providerInterfaceId,
      topologyPlan.model,
      topologyPlan.reasoning_effort,
      topologyPlan.service_tier,
    );
    if (providerDescriptor.text !== providerWrite.content) selectedWrites.push(providerWrite);
  }
  if (authorNextCycle) {
    const profileWrite = writes.find((write) => write.filePath === sidecarPath);
    if (payloadSelectionChanged) {
      if (!profileWrite || profileWrite.expectedBeforeHash !== sha256Hex(Buffer.from(sidecar.text))) {
        throw new Error("next-cycle payload selection has no exact checked profile write");
      }
      const nextSidecar = JSON.parse(profileWrite.content);
      const expectedSidecar = { ...sidecar.value, authoring_profile: expectedProfile };
      if (canonicalJson(nextSidecar) !== canonicalJson(expectedSidecar)) {
        throw new Error("next-cycle payload selection changed unrelated client sidecar data");
      }
      selectedWrites.push(profileWrite);
    } else if (profileWrite) {
      throw new Error("next-cycle selection unexpectedly changes the client profile");
    }
    const unexpectedWrites = writes.filter((write) => write !== providerWrite && write !== profileWrite);
    if (unexpectedWrites.length > 0) {
      throw new Error("operator chat next cycle requires repair of changed private capabilities");
    }
    const plannedRun = validatePlannedRunRecord(
      topologyPlan.run_record_content,
      input.runId,
      input.chatInterfaceId,
      safeSegment(input.currentRunDocument.value.ee, "run.ee"),
      safeSegment(input.currentRunDocument.value.worker, "run.worker"),
      input.clientSurfaceRole,
    );
    const plannedCheckpoint = activeOperatorMessageCheckpoint(
      plannedRun,
      input.runId,
      input.chatInterfaceId,
      providerInterfaceId,
      config.artifact_labels.message,
    );
    const currentCheckpoint = activeOperatorMessageCheckpoint(
      input.currentRunDocument.value,
      input.runId,
      input.chatInterfaceId,
      providerInterfaceId,
      config.artifact_labels.message,
    );
    if (
      plannedCheckpoint.id === currentCheckpoint.id
      && plannedCheckpoint.epoch === currentCheckpoint.epoch
    ) {
      throw new Error("operator chat topology planner did not author a fresh message checkpoint");
    }
    const plannedClientState = requiredObject(
      requiredObject(plannedRun.interface_state, "plan run record.interface_state")[input.chatInterfaceId],
      `plan run record.interface_state.${input.chatInterfaceId}`,
    );
    if (
      nonNegativeInteger(plannedClientState.ledger_offset, "plan client ledger_offset")
      !== input.conversationState.ledgerOffset
    ) {
      throw new Error("operator chat topology planner did not advance the next cycle ledger offset");
    }
    selectedWrites.push({
      filePath: path.join(await resolveRunRecordsRoot(octopusHome), `${input.runId}.json`),
      content: topologyPlan.run_record_content,
      tag: `run:${input.runId}:next-cycle`,
      expectedBeforeHash: sha256Hex(Buffer.from(input.currentRunDocument.text)),
    });
  }
  return selectedWrites;
}

function assertPayloadInputCycleReady(record, providerInterfaceId, conversation) {
  if (record.status !== "idle"
    || typeof conversation.operatorMessageCheckpointAdmitted !== "boolean"
    || typeof conversation.terminalAfterOperatorMessage !== "boolean"
    || (conversation.operatorMessageCheckpointAdmitted && !conversation.terminalAfterOperatorMessage)) {
    throw new Error("Attachments can change after the task's current input has settled");
  }
  const state = requiredObject(record.interface_state?.[providerInterfaceId], "provider authoring state");
  const generation = nonNegativeInteger(state.authoring_generation, "provider authoring_generation");
  if (state.pending_handoff === undefined) {
    if (generation !== 0) throw new Error("Attachment selection has no completed provider handoff evidence");
    return;
  }
  const pending = requiredObject(state.pending_handoff, "provider pending_handoff");
  if (pending.terminal !== true || (pending.context_publication !== undefined && pending.context_publication !== null)) {
    throw new Error("Attachments can change after the provider has published its completed context");
  }
}

function selectedProviderDescriptorContent(
  currentText,
  plannedText,
  providerInterfaceId,
  model,
  reasoningEffort,
  serviceTier,
) {
  const plannedProviderDescriptor = requiredObject(
    JSON.parse(plannedText),
    "planned provider descriptor",
  );
  const selectedProviderDescriptor = requiredObject(
    JSON.parse(currentText),
    "selected provider descriptor",
  );
  if (
    plannedProviderDescriptor.id !== providerInterfaceId
    || selectedProviderDescriptor.id !== providerInterfaceId
  ) {
    throw new Error("operator chat provider selection planned a different private provider identity");
  }
  selectedProviderDescriptor.interface_policy = requiredObject(
    plannedProviderDescriptor.interface_policy,
    "planned provider interface_policy",
  );
  const selectedProviderDriverConfig = requiredObject(
    selectedProviderDescriptor.interface_driver_config,
    "selected provider interface_driver_config",
  );
  const plannedProviderDriverConfig = requiredObject(
    plannedProviderDescriptor.interface_driver_config,
    "planned provider interface_driver_config",
  );
  selectedProviderDriverConfig.supported_reasoning_efforts = requiredArray(
    plannedProviderDriverConfig.supported_reasoning_efforts,
    "planned provider supported_reasoning_efforts",
  );
  const selectedProviderPrivateConfig = requiredObject(
    selectedProviderDriverConfig.private_config,
    "selected provider private_config",
  );
  const plannedProviderPrivateConfig = requiredObject(
    plannedProviderDriverConfig.private_config,
    "planned provider private_config",
  );
  selectedProviderPrivateConfig.model_catalog = requiredArray(
    plannedProviderPrivateConfig.model_catalog,
    "planned provider model_catalog",
  );
  selectedProviderPrivateConfig.model = requiredString(model, "planned provider model");
  const resolvedServiceTier = optionalServiceTier(serviceTier);
  if (resolvedServiceTier) selectedProviderPrivateConfig.service_tier = resolvedServiceTier;
  else delete selectedProviderPrivateConfig.service_tier;
  const resolvedReasoningEffort = stringValue(reasoningEffort);
  if (resolvedReasoningEffort) {
    selectedProviderPrivateConfig.reasoning_effort = resolvedReasoningEffort;
  } else {
    delete selectedProviderPrivateConfig.reasoning_effort;
  }
  return `${JSON.stringify(selectedProviderDescriptor, null, 2)}\n`;
}

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
  const state = await readOrCreateOperatorMessageAdmission(input);
  return recoverOperatorMessageAdmission(
    options,
    input,
    state.admissionPath,
    state.document,
    state.admission,
  );
}

async function admitOperatorMessage(options, input) {
  const state = await readOrCreateOperatorMessageAdmission(input);
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
  await writeOperatorMessageAdmission(admissionPath, document, admission);
  return {
    outboundCursorOffset: admission.outbound_cursor_offset,
    recovered: false,
    verdict,
  };
}

async function readOrCreateOperatorMessageAdmission(input) {
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
    document = await writeOperatorMessageAdmission(admissionPath, null, admission);
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
    await writeOperatorMessageAdmission(admissionPath, document, admission);
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

async function writeOperatorMessageAdmission(filePath, prior, value) {
  const text = `${JSON.stringify(value, null, 2)}\n`;
  await commitClientWrites([{
    filePath,
    content: text,
    tag: `operator-message-admission:${value.message_id}`,
    expectedBeforeHash: prior ? sha256Hex(Buffer.from(prior.text)) : null,
  }]);
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
  if (canonicalJson(authoringProfile) !== canonicalJson(expectedAuthoringProfile)) {
    throw new Error("topology plan authoring profile drifted from the requested topology");
  }
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
  writes.push(...plannedAdditionalRunWrites(topologyPlan, runsDir));
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
    authoringProfile: expectedAuthoringProfile,
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
  writes.push(...plannedAdditionalRunWrites(topologyPlan, runsDir));
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
  return {
    runId,
    writes,
    removals: requiredArray(topologyPlan.removals, "plan.removals").map((removal, index) => (
      plannedClientRemoval(removal, index, registryRoot)
    )),
  };
}

async function verifyProviderAccount(options, request) {
  const input = providerAccountRequest(request, false);
  const submitArtifact = requiredFunction(options?.submitArtifact, "submitArtifact");
  const readArtifactPayload = requiredFunction(
    options?.readArtifactPayload,
    "readArtifactPayload",
  );
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  return withRunAuthoringLock(input.runId, async () => {
    const context = await resolveProviderAccountContext(
      octopusHome,
      input.runId,
      input.interfaceId,
      input.providerInterfaceId,
      true,
    );
    const cursorKey = `${octopusHome}\0${input.interfaceId}`;
    const startCursor = liveLedgerCursors.get(cursorKey)
      ?? await readPersistedLiveCursor(octopusHome, input.interfaceId)
      ?? { offset: 0, seq: null };
    const payload = { refresh_token: true };
    const submission = await submitArtifact({
      interfaceId: input.interfaceId,
      runId: input.runId,
      label: context.requestLabel,
      payload,
      timeoutMs: input.timeoutMs,
    });
    const requestSeq = providerAccountSubmissionSeq(submission, input, context.requestLabel, payload);
    if (requestSeq <= input.afterSeq) {
      throw new Error("provider account request admission is not after the required ledger sequence");
    }
    const requestMovement = await readLedgerMovementAtSeq(
      path.join(octopusHome, "ledger", "movements.jsonl"),
      requestSeq,
    );
    const requestCoordinates = await providerAccountRequestCoordinates(
      requestMovement,
      input,
      context,
      readArtifactPayload,
    );
    let observed;
    try {
      observed = await waitForProviderAccountObservation({
        ...input,
        context,
        cursor: startCursor,
        movementsPath: path.join(octopusHome, "ledger", "movements.jsonl"),
        readArtifactPayload,
        requestCoordinates,
      });
    } catch (error) {
      if (isObject(error?.providerAccountCursor)) {
        rememberLiveCursor(cursorKey, error.providerAccountCursor);
        await persistLiveCursor(octopusHome, input.interfaceId, error.providerAccountCursor);
      }
      throw error;
    }
    rememberLiveCursor(cursorKey, observed.cursor);
    await persistLiveCursor(octopusHome, input.interfaceId, observed.cursor);
    return observed.evidence;
  });
}

async function refreshProviderModels(options, request) {
  const input = providerModelCatalogRequest(request);
  const submitArtifact = requiredFunction(options?.submitArtifact, "submitArtifact");
  const readArtifactPayload = requiredFunction(
    options?.readArtifactPayload,
    "readArtifactPayload",
  );
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  return withRunAuthoringLock(input.runId, async () => {
    let context = await resolveProviderModelCatalogContext(
      octopusHome,
      input.runId,
      input.interfaceId,
      input.providerInterfaceId,
    );
    const movementsPath = path.join(octopusHome, "ledger", "movements.jsonl");
    const deadline = Date.now() + input.timeoutMs;
    const models = new Map();
    const cursors = new Set();
    let cursor = null;
    let observedAtUnixMs = 0;
    for (;;) {
      const payload = cursor === null ? {} : { cursor };
      const pending = await findProviderModelCatalogAdmission(movementsPath, input, context);
      const startOffset = pending?.startOffset ?? await readCompleteLedgerEndOffset(movementsPath);
      let requestMovement = pending?.movement;
      if (!requestMovement) {
        const submission = await submitArtifact({
          interfaceId: input.interfaceId,
          runId: input.runId,
          label: context.requestLabel,
          payload,
          timeoutMs: providerModelCatalogRemainingMs(deadline),
        });
        const requestSeq = providerOperationSubmissionSeq(submission, input, context.requestLabel, payload);
        requestMovement = await readLedgerMovementAtSeq(movementsPath, requestSeq);
      }
      const requestSeq = nonNegativeInteger(requestMovement.seq, "provider model catalog admission seq");
      await providerOperationRequestCoordinates(
        requestMovement,
        input,
        context.requestLabel,
        payload,
        readArtifactPayload,
        "provider model catalog request",
      );
      const observation = await waitForProviderModelCatalogObservation({
        context,
        deadline,
        input,
        movementsPath,
        readArtifactPayload,
        requestSeq,
        startOffset,
      });
      observedAtUnixMs = Math.max(observedAtUnixMs, observation.observedAtUnixMs);
      for (const model of observation.models) {
        if (models.has(model.model)) {
          throw new Error(`provider model catalog repeats '${model.model}' across pages`);
        }
        models.set(model.model, model);
      }
      context = await waitForProviderModelCatalogRearm(octopusHome, input, context,
        invocationRuntime(requestMovement), deadline);
      if (observation.nextCursor === null) break;
      if (!cursors.add(observation.nextCursor)) {
        throw new Error("provider model catalog repeated its pagination cursor");
      }
      cursor = observation.nextCursor;
    }
    if (models.size === 0) {
      throw new Error(`provider '${context.runtimeProviderInterfaceId}' returned an empty formed model catalog`);
    }
    return {
      providerInterfaceId: input.providerInterfaceId,
      runtimeProviderInterfaceId: context.runtimeProviderInterfaceId,
      selectedModel: context.selectedModel,
      models: [...models.values()],
      observedAtUnixMs,
    };
  });
}

async function findProviderModelCatalogAdmission(movementsPath, input, context) {
  const tail = await readCompleteLedgerTail(movementsPath, 0);
  let pending = null;
  let expectedSeq = null;
  for (const record of tail.records) {
    const movement = JSON.parse(record.text);
    const seq = nonNegativeInteger(movement.seq, "provider operation ledger seq");
    if (expectedSeq !== null && seq !== expectedSeq) {
      throw new Error(`provider operation ledger is non-contiguous at seq ${seq}; expected ${expectedSeq}`);
    }
    expectedSeq = seq + 1;
    const runtime = exactArrivalRuntime(movement, input.interfaceId);
    if (runtime?.run !== input.runId
      || runtime.checkpoint !== context.inputCheckpoint.id
      || runtime.epoch !== context.inputCheckpoint.epoch) continue;
    if (runtime.worker !== context.run.worker || runtime.ee !== context.run.ee) {
      throw new Error("pending provider operation differs from its exact run provenance");
    }
    const metadata = movement.invocation_metadata;
    if (metadata.checkpoint_admission?.complete !== true
      || metadata.checkpoint_admission.artifact_label !== context.requestLabel
      || canonicalJson(metadata.outbound_interface_bindings) !== canonicalJson([context.runtimeProviderInterfaceId])) {
      throw new Error("pending provider operation lacks complete exact provider admission");
    }
    if (pending) throw new Error("provider operation checkpoint has multiple admitted requests");
    pending = { movement, startOffset: record.nextOffset };
  }
  return pending;
}

async function waitForProviderModelCatalogRearm(octopusHome, input, context, admitted, deadline) {
  for (;;) {
    const change = watchLedgerChange(context.runPath, providerModelCatalogRemainingMs(deadline));
    try {
      const next = await resolveProviderModelCatalogContext(octopusHome, input.runId,
        input.interfaceId, input.providerInterfaceId);
      if (next.inputCheckpoint.id !== admitted.checkpoint || next.inputCheckpoint.epoch !== admitted.epoch) {
        return next;
      }
      if (!(await change.promise)) {
        throw new Error("provider model catalog was observed but its provider has not rearmed the exact input checkpoint");
      }
    } finally {
      change.cancel();
    }
  }
}

async function loginProviderAccount(options, request) {
  const input = providerAccountLoginRequest(request);
  const submitArtifact = requiredFunction(options?.submitArtifact, "submitArtifact");
  const readArtifactPayload = requiredFunction(
    options?.readArtifactPayload,
    "readArtifactPayload",
  );
  const openExternalUrl = requiredFunction(options?.openExternalUrl, "openExternalUrl");
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  return withRunAuthoringLock(input.runId, async () => {
    const context = await resolveProviderAccountContext(
      octopusHome,
      input.runId,
      input.interfaceId,
      input.providerInterfaceId,
      true,
      true,
    );
    const cursorKey = `${octopusHome}\0${input.interfaceId}`;
    let cursor = liveLedgerCursors.get(cursorKey)
      ?? await readPersistedLiveCursor(octopusHome, input.interfaceId)
      ?? { offset: 0, seq: null };
    const movementsPath = path.join(octopusHome, "ledger", "movements.jsonl");
    const deadline = Date.now() + input.timeoutMs;
    const rememberCursor = async (nextCursor) => {
      cursor = normalizeLiveCursor(nextCursor);
      rememberLiveCursor(cursorKey, cursor);
      await persistLiveCursor(octopusHome, input.interfaceId, cursor);
    };
    const wait = async (operation) => {
      try {
        const observed = await operation();
        await rememberCursor(observed.cursor);
        return observed;
      } catch (error) {
        if (isObject(error?.providerAccountCursor)) {
          await rememberCursor(error.providerAccountCursor);
        }
        throw error;
      }
    };
    const startPayload = {
      method: "chatgpt_browser",
      codex_streamlined_login: true,
    };
    const startSubmission = await submitArtifact({
      interfaceId: input.interfaceId,
      runId: input.runId,
      label: context.login.startRequestLabel,
      payload: startPayload,
      timeoutMs: providerLoginRemainingMs(deadline),
    });
    const startSeq = providerOperationSubmissionSeq(
      startSubmission,
      input,
      context.login.startRequestLabel,
      startPayload,
    );
    if (startSeq <= input.afterSeq) {
      throw new Error("provider account login admission is not after the required ledger sequence");
    }
    await providerOperationRequestCoordinates(
      await readLedgerMovementAtSeq(movementsPath, startSeq),
      input,
      context.login.startRequestLabel,
      startPayload,
      readArtifactPayload,
      "provider account login start request",
    );
    const started = await wait(() => waitForProviderLoginStarted({
      ...input,
      context,
      cursor,
      deadline,
      movementsPath,
      readArtifactPayload,
      requestSeq: startSeq,
    }));
    await openExternalUrl(started.authUrl);
    let poll = 0;
    let requestFloor = startSeq;
    for (;;) {
      if (poll > 0) {
        await delayProviderLoginPoll(input.statusPollMs, deadline);
      }
      poll += 1;
      const statusPayload = { login_id: started.loginId };
      const statusSubmission = await submitArtifact({
        interfaceId: input.interfaceId,
        runId: input.runId,
        label: context.login.statusRequestLabel,
        payload: statusPayload,
        timeoutMs: providerLoginRemainingMs(deadline),
      });
      const statusSeq = providerOperationSubmissionSeq(
        statusSubmission,
        input,
        context.login.statusRequestLabel,
        statusPayload,
      );
      if (statusSeq <= requestFloor) {
        throw new Error("provider account login-status admission did not follow the previous operation request");
      }
      requestFloor = statusSeq;
      await providerOperationRequestCoordinates(
        await readLedgerMovementAtSeq(movementsPath, statusSeq),
        input,
        context.login.statusRequestLabel,
        statusPayload,
        readArtifactPayload,
        "provider account login status request",
      );
      const status = await wait(() => waitForProviderLoginStatus({
        ...input,
        context,
        cursor,
        deadline,
        startSeq,
        loginId: started.loginId,
        movementsPath,
        readArtifactPayload,
        requestSeq: statusSeq,
      }));
      if (status.completed === true) return { status: "completed" };
    }
  });
}

async function validateProviderAccountObservation(options, request) {
  const input = providerAccountRequest(request, true);
  if (input.evidence === null) return null;
  const readArtifactPayload = requiredFunction(
    options?.readArtifactPayload,
    "readArtifactPayload",
  );
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const context = await resolveProviderAccountContext(
    octopusHome,
    input.runId,
    input.interfaceId,
    input.providerInterfaceId,
    false,
  );
  const evidence = providerAccountEvidence(input.evidence);
  if (
    evidence.operation_id !== input.operationId
    || evidence.run_id !== input.runId
    || evidence.interface_id !== input.interfaceId
    || evidence.provider_interface_id !== input.providerInterfaceId
    || evidence.runtime_provider_interface_id !== context.runtimeProviderInterfaceId
  ) {
    throw new Error("provider account evidence does not match its expected setup coordinates");
  }
  const movements = await readLedgerMovementsAtSeqs(
    path.join(octopusHome, "ledger", "movements.jsonl"),
    [evidence.request_movement_seq, evidence.snapshot_movement_seq],
  );
  const requestCoordinates = await providerAccountRequestCoordinates(
    movements.get(evidence.request_movement_seq),
    input,
    context,
    readArtifactPayload,
  );
  const snapshotCoordinates = await providerAccountSnapshotCoordinates(
    movements.get(evidence.snapshot_movement_seq),
    input,
    context,
    readArtifactPayload,
    evidence.request_movement_seq,
  );
  const validated = providerAccountEvidenceFromCoordinates(
    { ...input, context },
    requestCoordinates,
    snapshotCoordinates,
  );
  if (canonicalJson(validated) !== canonicalJson(evidence)) {
    throw new Error("provider account evidence differs from the durable ledger observation");
  }
  return validated;
}

async function releaseOperatorTaskWorkerEnvironments(options, request) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const runId = safeSegment(request?.runId, "runId");
  const expectedDigest = contentHashHex(request?.expectedDigest, "expectedDigest");
  const timeoutMs = numberValue(request?.timeoutMs, 45_000);
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const runPath = path.join(runsDir, `${runId}.json`);

  return withRunAuthoringLock(runId, async () => {
    const initial = await readJsonDocumentIfPresent(runPath);
    if (!initial) throw new Error(`operator chat task '${runId}' does not exist`);
    const initialDigest = sha256Hex(Buffer.from(initial.text));
    if (initialDigest !== expectedDigest) {
      throw new Error(`operator chat task '${runId}' changed before worker environments could be released`);
    }
    const identity = chatIdentityFromRunRecord(config, runId, initial.value);
    const clientState = requiredObject(
      initial.value.interface_state?.[identity.interfaceId],
      `operator chat task '${runId}' client state`,
    );
    if (clientState.client_surface_role !== "task") {
      throw new Error(`operator chat run '${runId}' is not a deletable task surface`);
    }
    const exactInterfaces = await ownedOperatorTaskInterfaces(octopusHome, identity.interfaceId);
    const formations = await operatorTaskWorkerEnvironmentRecords(
      octopusHome,
      initial.value,
      runId,
      exactInterfaces,
    );
    const pending = formations.filter((entry) => (
      entry.record.formation.requires_release === true
      && entry.record.lifecycle !== "released"
    ));
    if (pending.length === 0) {
      return {
        runId,
        expectedDigest: initialDigest,
        releasedWorkerEnvironmentIds: [],
      };
    }

    const movementsPath = path.join(octopusHome, "ledger", "movements.jsonl");
    const artifactRoot = path.join(
      absolutePath(options.clientArtifactsRoot(identity.interfaceId), "clientArtifactsRoot"),
      "operator-chat",
      "worker-environment-lifecycle",
    );
    await fs.mkdir(artifactRoot, { recursive: true });
    await waitForInterfaceSocket(octopusHome, identity.interfaceId, timeoutMs);
    const mandatoryRoleBindings = await exactWorkerRoleArtifactBindings(
      octopusHome,
      initial.value.worker,
    );

    let current = initial;
    let currentDigest = initialDigest;
    const releasedWorkerEnvironmentIds = [];
    for (const entry of pending) {
      const formation = entry.record.formation;
      const cycle = randomToken(20);
      const inputCheckpointId = `cp_task_release_input_${cycle}`;
      const outputCheckpointId = `cp_task_release_output_${cycle}`;
      const next = structuredClone(current.value);
      const retainedCheckpoints = requiredArray(next.checkpoints, "task checkpoints")
        .filter((checkpoint) => !stringValue(checkpoint?.id).startsWith("cp_task_release_"));
      const checkpointEpoch = nextLifecycleCheckpointEpoch(retainedCheckpoints);
      retainedCheckpoints.push(
        lifecycleCheckpoint(
          runId,
          inputCheckpointId,
          identity.interfaceId,
          formation.door_interface,
          checkpointEpoch,
          [WORKER_ENVIRONMENT_RELEASE_LABEL],
          mandatoryRoleBindings,
        ),
        lifecycleCheckpoint(
          runId,
          outputCheckpointId,
          formation.door_interface,
          identity.interfaceId,
          checkpointEpoch,
          [
            WORKER_ENVIRONMENT_RELEASED_LABEL,
            `${formation.door_interface}.failure`,
          ],
          mandatoryRoleBindings,
        ),
      );
      next.checkpoints = retainedCheckpoints;
      next.active_checkpoints = [inputCheckpointId, outputCheckpointId];
      requiredObject(
        next.interface_state?.[identity.interfaceId],
        `operator chat task '${runId}' client state`,
      ).cycle_checkpoint_ids = [inputCheckpointId, outputCheckpointId];
      const nextText = `${JSON.stringify(next, null, 2)}\n`;
      await commitClientWrites([{
        filePath: runPath,
        content: nextText,
        tag: `run-release:${runId}:${formation.worker_environment_id}`,
        expectedBeforeHash: currentDigest,
      }]);
      current = { value: next, text: nextText };
      currentDigest = sha256Hex(Buffer.from(nextText));

      const ledgerOffset = await readCompleteLedgerEndOffset(movementsPath);
      const payloadPath = await writeContentAddressedPayload(
        artifactRoot,
        WORKER_ENVIRONMENT_RELEASE_LABEL,
        Buffer.from(JSON.stringify({
          worker_environment_id: formation.worker_environment_id,
        })),
      );
      const verdict = await options.handInArtifact({
        endpoint: interfaceEndpoint(octopusHome, identity.interfaceId),
        interfaceId: identity.interfaceId,
        manifest: {
          label: WORKER_ENVIRONMENT_RELEASE_LABEL,
          payload_path: payloadPath,
        },
        timeoutMs,
      });
      assertAdmitted(verdict, WORKER_ENVIRONMENT_RELEASE_LABEL);
      await waitForWorkerEnvironmentRelease({
        clientInterfaceId: identity.interfaceId,
        doorInterfaceId: formation.door_interface,
        formationPath: entry.path,
        movementsPath,
        outputCheckpointId,
        runId,
        startOffset: ledgerOffset,
        timeoutMs,
        workerEnvironmentId: formation.worker_environment_id,
      });
      releasedWorkerEnvironmentIds.push(formation.worker_environment_id);
    }

    return {
      runId,
      expectedDigest: currentDigest,
      releasedWorkerEnvironmentIds,
    };
  });
}

function lifecycleCheckpoint(
  runId,
  checkpointId,
  inboundInterfaceId,
  outboundInterfaceId,
  epoch,
  allowedArtifactNames,
  mandatoryRoleBindings,
) {
  const artifactPolicy = {
    mode: "allow",
    names: allowedArtifactNames,
    namespaces: [],
  };
  const binding = (interfaceId) => ({
    interface: interfaceId,
    config: { artifact_policy: artifactPolicy },
  });
  return {
    id: checkpointId,
    run: runId,
    trace: `trace:${runId}:${checkpointId}`,
    epoch,
    interface_binding: binding(inboundInterfaceId),
    outbound_interface_bindings: [binding(outboundInterfaceId)],
    artifact_bindings: mandatoryRoleBindings.map((binding) => ({ ...binding })),
    required_verified_artifacts: [],
  };
}

async function exactWorkerRoleArtifactBindings(octopusHome, workerId) {
  const worker = safeSegment(workerId, "operator task worker interface");
  const interfaceRoot = await fs.realpath(path.join(octopusHome, "registry", "interface"));
  const workerRoot = path.join(interfaceRoot, worker);
  const workerRootStat = await fs.lstat(workerRoot);
  if (workerRootStat.isSymbolicLink() || !workerRootStat.isDirectory()) {
    throw new Error(`operator task worker interface '${worker}' must be a plain registry directory`);
  }
  const policyPath = path.join(workerRoot, "policy.json");
  const policyStat = await fs.lstat(policyPath);
  if (policyStat.isSymbolicLink() || !policyStat.isFile()) {
    throw new Error(`operator task worker policy '${worker}' must be a plain file`);
  }
  const policy = await readJsonIfPresent(policyPath);
  if (!policy || policy.id !== worker) {
    throw new Error(`operator task worker policy '${worker}' does not match its exact registry folder`);
  }
  const seen = new Set();
  return requiredArray(policy.role_artifacts, `operator task worker policy '${worker}'.role_artifacts`)
    .map((artifact, index) => safeSegment(artifact, `operator task worker role_artifacts[${index}]`))
    .filter((artifact) => {
      if (seen.has(artifact)) return false;
      seen.add(artifact);
      return true;
    })
    .map((id) => ({ id }));
}

function nextLifecycleCheckpointEpoch(checkpoints) {
  let maximum = -1;
  for (const checkpoint of checkpoints) {
    const epoch = checkpoint?.epoch;
    if (!Number.isSafeInteger(epoch) || epoch < 0) {
      throw new Error("task checkpoint epoch must be a non-negative safe integer");
    }
    maximum = Math.max(maximum, epoch);
  }
  if (maximum >= Number.MAX_SAFE_INTEGER) throw new Error("task checkpoint epoch overflowed");
  return maximum + 1;
}

async function ownedOperatorTaskInterfaces(octopusHome, ownerInterfaceId) {
  const root = path.join(octopusHome, "registry", "interface");
  const rootStat = await fs.lstat(root);
  if (rootStat.isSymbolicLink() || !rootStat.isDirectory()) {
    throw new Error("operator task interface registry must be a plain directory");
  }
  const entries = await fs.readdir(root, { withFileTypes: true });
  const owned = new Set();
  for (const entry of entries) {
    if (!entry.isDirectory() || !/^[A-Za-z0-9_-]+$/.test(entry.name)) continue;
    const sidecarPath = path.join(root, entry.name, "octopus_os.json");
    const sidecarStat = await fs.lstat(sidecarPath).catch((error) => {
      if (error?.code === "ENOENT") return null;
      throw error;
    });
    if (!sidecarStat) continue;
    if (sidecarStat.isSymbolicLink() || !sidecarStat.isFile()) {
      throw new Error(`Octopus OS interface '${entry.name}' has a non-regular sidecar`);
    }
    const sidecar = await readJsonIfPresent(sidecarPath);
    if (!sidecar || sidecar.workspace !== "octopus_os") continue;
    if (sidecar.interface !== entry.name) {
      throw new Error(`Octopus OS interface '${entry.name}' has drifted sidecar identity`);
    }
    if (sidecar.owner_interface === ownerInterfaceId) owned.add(entry.name);
  }
  if (!owned.has(ownerInterfaceId)) {
    throw new Error(`operator chat task interface '${ownerInterfaceId}' owns no exact topology`);
  }
  return owned;
}

async function operatorTaskWorkerEnvironmentRecords(octopusHome, run, runId, exactInterfaces) {
  const worker = requiredString(run.worker, `operator chat task '${runId}' worker`);
  const ee = requiredString(run.ee, `operator chat task '${runId}' ee`);
  const records = [];
  for (const interfaceId of [...exactInterfaces].sort()) {
    const root = path.join(octopusHome, "interfaces", interfaceId, "worker-environments");
    let entries;
    try {
      const rootStat = await fs.lstat(root);
      if (rootStat.isSymbolicLink() || !rootStat.isDirectory()) {
        throw new Error(`worker-environment root for '${interfaceId}' must be a plain directory`);
      }
      entries = await fs.readdir(root, { withFileTypes: true });
    } catch (error) {
      if (error?.code === "ENOENT") continue;
      throw error;
    }
    for (const entry of entries) {
      if (!entry.isDirectory() || !/^we_[0-9a-f]{32}$/.test(entry.name)) {
        throw new Error(`worker-environment state under '${interfaceId}' has an invalid identity`);
      }
      const recordPath = path.join(root, entry.name, "worker_environment.json");
      const recordStat = await fs.lstat(recordPath);
      if (recordStat.isSymbolicLink() || !recordStat.isFile()) {
        throw new Error(`worker-environment record '${recordPath}' must be a regular file`);
      }
      const record = await readJsonIfPresent(recordPath);
      if (!record) throw new Error(`worker-environment record '${recordPath}' is absent`);
      const formation = requiredObject(record.formation, `worker-environment '${entry.name}' formation`);
      if (
        record.schema_version !== 5
        || formation.worker_environment_id !== entry.name
        || formation.door_interface !== interfaceId
        || !["active", "failed", "released"].includes(record.lifecycle)
      ) {
        throw new Error(`worker-environment record '${recordPath}' violates its lifecycle identity`);
      }
      if (formation.run !== runId) {
        await validateTaskWriterFormation(octopusHome, run, formation, exactInterfaces);
        if (formation.requires_release && record.lifecycle !== "released") {
          throw new Error(`task writer '${formation.run}' retains an unreleased worker environment`);
        }
      }
      if (formation.worker !== worker || formation.ee !== ee) {
        throw new Error(`worker-environment '${entry.name}' differs from task '${runId}' coordinates`);
      }
      if (typeof formation.requires_release !== "boolean") {
        throw new Error(`worker-environment '${entry.name}' has no release contract`);
      }
      records.push({ path: recordPath, record });
    }
  }
  return records.sort((left, right) => (
    left.record.formation.worker_environment_id
      .localeCompare(right.record.formation.worker_environment_id)
  ));
}

async function validateTaskWriterFormation(octopusHome, run, formation, exactInterfaces) {
  const writerId = safeSegment(formation.run, "task writer run");
  const declarations = Object.entries(requiredObject(run.interface_state, "task interface state"))
    .flatMap(([owner, state]) => {
      const binding = state?.run_authoring_binding?.writer_run ?? state?.writer_run;
      return binding?.run === writerId ? [{ owner, binding }] : [];
    });
  if (declarations.length !== 1) throw new Error(`task-owned interface '${formation.door_interface}' retains a formation for another run`);
  const { owner, binding } = declarations[0];
  if (Object.hasOwn(binding, "initial_record_format") && !["pretty", "compact"].includes(binding.initial_record_format)) {
    throw new Error("task writer Run declaration has invalid initial record format");
  }
  const initial = requiredObject(binding.initial_record, "task writer initial record");
  const target = initial.interface_state?.[owner]?.file_writer_target;
  if (!exactInterfaces.has(owner) || initial.id !== writerId || initial.ee !== run.ee
    || initial.worker !== run.worker || target?.run !== run.id || target.provider !== owner
    || target.generation !== 0 || target.door !== formation.door_interface
    || !exactInterfaces.has(target.source) || !exactInterfaces.has(target.door) || target.source === target.door) {
    throw new Error(`task writer '${writerId}' differs from its exact task declaration`);
  }
  const file = path.join(await resolveRunRecordsRoot(octopusHome), `${writerId}.json`);
  const stat = await fs.lstat(file);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`task writer '${writerId}' must be a plain file`);
  const current = await readJsonIfPresent(file);
  const generation = current?.interface_state?.[owner]?.file_writer_target?.generation;
  if (!Number.isSafeInteger(generation) || generation < 0) throw new Error(`task writer '${writerId}' has invalid generation`);
  const expected = structuredClone(initial);
  if (generation > 0) {
    expected.interface_state[owner].file_writer_target.generation = generation;
    for (const checkpoint of requiredArray(expected.checkpoints, "task writer checkpoints")) {
      checkpoint.id = `${checkpoint.id}_${generation}`;
      checkpoint.trace = `trace:${writerId}:${checkpoint.id}`;
    }
    expected.active_checkpoints = expected.checkpoints.map((checkpoint) => checkpoint.id);
  }
  if (canonicalJson(current) !== canonicalJson(expected)) throw new Error(`task writer '${writerId}' drifted from its declared generation`);
}

async function waitForWorkerEnvironmentRelease(input) {
  const deadline = Date.now() + input.timeoutMs;
  let cursor = { offset: input.startOffset, seq: null };
  for (;;) {
    const remaining = Math.max(0, deadline - Date.now());
    const change = watchLedgerChange(input.movementsPath, remaining);
    const tail = await readCompleteLedgerTail(input.movementsPath, cursor.offset);
    cursor = { offset: tail.nextOffset, seq: null };
    for (const line of tail.lines) {
      let movement;
      try {
        movement = JSON.parse(line);
      } catch (error) {
        change.cancel();
        throw new Error(`movement ledger contains malformed JSON while awaiting release: ${error.message}`);
      }
      const runtime = exactOutboundRuntime(movement, input.clientInterfaceId);
      if (
        !runtime
        || runtime.run !== input.runId
        || runtime.checkpoint !== input.outputCheckpointId
        || endpointInterfaceId(movement.from) !== input.doorInterfaceId
      ) continue;
      const label = artifactLabel(requiredString(movement?.artifact?.id, "release artifact id"));
      change.cancel();
      if (label !== WORKER_ENVIRONMENT_RELEASED_LABEL) {
        throw new Error(
          `worker environment '${input.workerEnvironmentId}' release returned '${label}'`,
        );
      }
      const released = await readJsonIfPresent(input.formationPath);
      if (
        !released
        || released.formation?.worker_environment_id !== input.workerEnvironmentId
        || released.lifecycle !== "released"
      ) {
        throw new Error(
          `worker environment '${input.workerEnvironmentId}' emitted release without durable Released state`,
        );
      }
      return;
    }
    if (remaining === 0 || !(await change.promise)) {
      change.cancel();
      throw new Error(`worker environment '${input.workerEnvironmentId}' release timed out`);
    }
  }
}

async function updateOperatorChatPins(options, request) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const installationRoot = resolveInstallationRoot(options);
  const artifactMaterializer = resolvePackageExecutable(
    config,
    installationRoot,
    resolveRegistryRoot(options),
    "artifact_materializer",
  );
  const runId = safeSegment(request?.runId, "runId");
  const sourceArtifactId = requiredString(request?.artifactId, "artifactId");
  if (typeof request?.pinned !== "boolean") throw new Error("pinned must be a boolean");
  const runsDir = await resolveRunRecordsRoot(octopusHome);
  const chatRunPath = path.join(runsDir, `${runId}.json`);

  return withRunAuthoringLock(runId, async () => {
    const document = await readJsonDocumentIfPresent(chatRunPath);
    if (!document) throw new Error(`operator chat run '${runId}' does not exist`);
    const identity = chatIdentityFromRunRecord(config, runId, document.value);
    const state = document.value.interface_state?.[identity.interfaceId];
    if (!isObject(state)) {
      throw new Error(`operator chat run '${runId}' has no state for '${identity.interfaceId}'`);
    }
    const current = state.pinned_artifact_bindings === undefined
      ? []
      : uniqueArtifactBindings(requiredArray(
        state.pinned_artifact_bindings,
        "interface_state pinned_artifact_bindings",
      ));
    const currentItems = normalizePinnedContextItems(state.pinned_context_items);
    let artifactId = sourceArtifactId;
    let nextItems = currentItems;
    if (request.pinned) {
      await assertRunOwnsArtifact({
        octopusHome,
        runId,
        interfaceId: identity.interfaceId,
        artifactId: sourceArtifactId,
      });
      if (request.excerpt !== undefined) {
        const excerpt = validatePinnedExcerpt(request.excerpt);
        const materialized = await materializePinnedExcerpt(
          octopusHome,
          artifactMaterializer,
          runId,
          sourceArtifactId,
          excerpt,
        );
        artifactId = materialized.artifactId;
        nextItems = [
          ...currentItems.filter((item) => item.artifact_id !== artifactId),
          materialized.item,
        ];
      }
    } else {
      nextItems = currentItems.filter((item) => item.artifact_id !== artifactId);
      if (!current.some((binding) => binding.id === artifactId)) {
        throw new Error(`artifact '${artifactId}' is not pinned in operator chat run '${runId}'`);
      }
    }
    const next = request.pinned
      ? uniqueArtifactBindings([...current, { id: artifactId }])
      : current.filter((binding) => binding.id !== artifactId);
    state.pinned_artifact_bindings = next;
    state.pinned_context_items = nextItems;
    const content = `${JSON.stringify(document.value, null, 2)}\n`;
    // notes2 2026-07-16: client administrative edits stay outside runtime evidence; the owner may revisit this boundary.
    const clientWrites = await commitClientWrites([{
      filePath: chatRunPath,
      content,
      tag: `run-pins:${runId}`,
      expectedBeforeHash: sha256Hex(Buffer.from(document.text)),
    }]);
    return {
      runId,
      pinnedArtifactIds: next.map((binding) => binding.id),
      pinnedContextItems: nextItems.map(clientPinnedContextItem),
      clientWrites,
    };
  });
}

function validatePinnedExcerpt(value) {
  if (!isObject(value)) throw new Error("excerpt must be an object");
  const unknown = Object.keys(value).filter((key) => !["sourceMessageId", "text", "start", "end", "role"].includes(key));
  if (unknown.length > 0) throw new Error(`excerpt contains unknown fields: ${unknown.join(", ")}`);
  const text = requiredString(value.text, "excerpt.text");
  const start = nonNegativeInteger(value.start, "excerpt.start");
  const end = nonNegativeInteger(value.end, "excerpt.end");
  if (end <= start) throw new Error("excerpt.end must be greater than excerpt.start");
  const role = requiredString(value.role, "excerpt.role");
  if (!["user", "assistant", "system"].includes(role)) throw new Error("excerpt.role is unsupported");
  return {
    sourceMessageId: requiredString(value.sourceMessageId, "excerpt.sourceMessageId"),
    text,
    start,
    end,
    role,
  };
}

async function materializePinnedExcerpt(octopusHome, materializer, runId, sourceArtifactId, excerpt) {
  const blobsDir = path.join(octopusHome, "ledger", "blobs");
  if (!(await fileExists(materializer))) {
    throw new Error(
      `operator chat artifact materializer is absent at its declared package path '${materializer}'`,
    );
  }
  const output = await runJsonProcess(materializer, {
    blobs_root: blobsDir,
    label: "operator.pinned_excerpt",
    payload: {
      text: excerpt.text,
      role: excerpt.role,
      source_run_id: runId,
      source_artifact_id: sourceArtifactId,
      source_message_id: excerpt.sourceMessageId,
      source_range: { start: excerpt.start, end: excerpt.end },
    },
  });
  const artifactId = requiredString(output?.artifact_id, "artifact materializer artifact_id");
  return {
    artifactId,
    item: {
      artifact_id: artifactId,
      source_artifact_id: sourceArtifactId,
      source_message_id: excerpt.sourceMessageId,
      text: excerpt.text,
      start: excerpt.start,
      end: excerpt.end,
      role: excerpt.role,
    },
  };
}

async function runJsonProcess(binary, request) {
  const stdout = await new Promise((resolve, reject) => {
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"] });
    const output = [];
    const errors = [];
    child.stdout.on("data", (chunk) => output.push(chunk));
    child.stderr.on("data", (chunk) => errors.push(chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) {
        resolve(Buffer.concat(output).toString("utf8"));
        return;
      }
      reject(new Error(Buffer.concat(errors).toString("utf8").trim() || `process exited with code ${code}`));
    });
    child.stdin.on("error", reject);
    child.stdin.end(JSON.stringify(request));
  });
  const response = JSON.parse(stdout);
  if (!isObject(response)) throw new Error("process returned a non-object response");
  return response;
}

function normalizePinnedContextItems(value) {
  if (value === undefined) return [];
  return requiredArray(value, "interface_state pinned_context_items").map((item, index) => {
    if (!isObject(item)) throw new Error(`pinned_context_items[${index}] must be an object`);
    const role = requiredString(item.role, `pinned_context_items[${index}].role`);
    if (!["user", "assistant", "system"].includes(role)) {
      throw new Error(`pinned_context_items[${index}].role is unsupported`);
    }
    return {
      artifact_id: requiredString(item.artifact_id, `pinned_context_items[${index}].artifact_id`),
      source_artifact_id: requiredString(item.source_artifact_id, `pinned_context_items[${index}].source_artifact_id`),
      source_message_id: requiredString(item.source_message_id, `pinned_context_items[${index}].source_message_id`),
      text: requiredString(item.text, `pinned_context_items[${index}].text`),
      start: nonNegativeInteger(item.start, `pinned_context_items[${index}].start`),
      end: nonNegativeInteger(item.end, `pinned_context_items[${index}].end`),
      role,
    };
  });
}

function clientPinnedContextItem(item) {
  return {
    artifactId: item.artifact_id,
    sourceArtifactId: item.source_artifact_id,
    sourceMessageId: item.source_message_id,
    text: item.text,
    start: item.start,
    end: item.end,
    role: item.role,
  };
}

async function readConfig() {
  const value = JSON.parse(await fs.readFile(CONFIG_PATH, "utf8"));
  if (!isObject(value) || value.schema_version !== 2) {
    throw new Error("world_operator_chat/client.json has an unsupported schema_version");
  }
  requiredString(value.interface, "client.interface");
  requiredObject(value.executables, "client.executables");
  requiredString(value.executables.topology_planner, "client.executables.topology_planner");
  requiredString(value.executables.artifact_materializer, "client.executables.artifact_materializer");
  requiredObject(value.artifact_labels, "client.artifact_labels");
  requiredString(value.artifact_labels.message, "client.artifact_labels.message");
  requiredString(value.artifact_labels.terminal_output, "client.artifact_labels.terminal_output");
  requiredString(value.artifact_labels.error_output, "client.artifact_labels.error_output");
  requiredArray(value.artifact_labels.stream_outputs, "client.artifact_labels.stream_outputs")
    .forEach((label, index) => requiredString(label, `client.artifact_labels.stream_outputs[${index}]`));
  requiredString(value.artifact_labels.tool_call, "client.artifact_labels.tool_call");
  requiredObject(value.authoring, "client.authoring");
  for (const field of ["source_interface", "room_interface", "tool"]) {
    requiredString(value.authoring[field], `client.authoring.${field}`);
  }
  positiveInteger(value.authoring.result_timeout_ms, "client.authoring.result_timeout_ms");
  requiredObject(value.run, "client.run");
  for (const field of [
    "interface_prefix",
    "run_prefix",
    "ee",
    "worker",
    "tool_call_mark",
    "tool_call_verifier",
    "listen_env",
  ]) {
    requiredString(value.run[field], `client.run.${field}`);
  }
  requiredArray(value.run.context_artifact_bindings, "client.run.context_artifact_bindings");
  return value;
}

function resolveInstallationRoot(options) {
  if (typeof options?.installationRoot !== "function") {
    throw new Error("installationRoot resolver is required");
  }
  return absolutePath(options.installationRoot(), "installationRoot");
}

function resolvePackageExecutable(config, installationRoot, registryRoot, name) {
  const declared = requiredString(config.executables?.[name], `client.executables.${name}`);
  const ownerRelative = declared.startsWith("owner://");
  const relative = ownerRelative ? declared.slice("owner://".length) : declared;
  if (path.isAbsolute(relative)) {
    throw new Error(`client.executables.${name} must be owner- or installation-relative`);
  }
  const components = relative.split(/[\\/]/);
  if (!relative || components.some((component) => !component || component === "." || component === "..")) {
    throw new Error(`client.executables.${name} is not a normalized relative path`);
  }
  const base = ownerRelative
    ? path.join(registryRoot, "interface", config.interface)
    : installationRoot;
  const executable = path.resolve(base, relative);
  const prefix = base.endsWith(path.sep) ? base : `${base}${path.sep}`;
  if (!executable.startsWith(prefix)) {
    throw new Error(`client.executables.${name} escapes its executable owner`);
  }
  return executable;
}

async function resolveEeInstancesRoot(octopusHome, options = {}) {
  if (typeof options.eeInstancesRoot === "function") {
    return absolutePath(options.eeInstancesRoot(), "ee_instances_root");
  }
  const host = await readJsonIfPresent(path.join(octopusHome, "host.json"));
  const configured = stringValue(host?.root_refs?.ee_instances_root);
  const root = configured || path.join(octopusHome, "state", "ee_instances");
  return absolutePath(root, "ee_instances_root");
}

async function planOperatorChatTopology({
  octopusHome,
  topologyPlanner,
  registryRoot,
  eeInstancesRoot,
  chatId,
  providerInterfaceId,
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
  payloadArtifactBindings,
  selectedWorkerEnvironmentKinds,
  selectedTools,
  clientSurfaceRole,
  cycle,
  existingRunRecord,
  conversation,
}) {
  if (!(await fileExists(topologyPlanner))) {
    throw new Error(
      `operator chat topology planner is absent at its declared package path '${topologyPlanner}'`,
    );
  }
  const request = JSON.stringify({
    registry_root: registryRoot,
    ee_instances_root: await resolveEeInstancesRoot(octopusHome),
    ee_policy_root: eeInstancesRoot,
    run_records_root: await resolveRunRecordsRoot(octopusHome),
    write_hash_algorithm: "sha256",
    interface_private_state_root: path.join(octopusHome, "interfaces"),
    chat_id: chatId,
    provider_interface_id: providerInterfaceId,
    model: stringValue(providerModel) || null,
    reasoning_effort: stringValue(reasoningEffort) || null,
    service_tier: optionalServiceTier(serviceTier) || null,
    ee_id: stringValue(eeId) || null,
    worker_interface_id: stringValue(workerInterfaceId) || null,
    client_surface_role: clientSurfaceRole,
    cycle,
    door_host_slots: doorHostSlots,
    external_inputs: externalInputs,
    external_relays: externalRelays,
    model_outputs: modelOutputs,
    context_artifact_bindings: contextArtifactBindings,
    payload_artifact_bindings: operatorPayloadArtifactBindings(payloadArtifactBindings),
    selected_worker_environment_kinds: selectedWorkerEnvironmentKinds,
    selected_tools: selectedTools.map((selection) => ({
      tool: selection.tool,
      interface: selection.interface,
      worker_environment_kind: selection.worker_environment_kind,
    })),
    existing_run_record: existingRunRecord ?? null,
    conversation,
  });
  const stdout = await new Promise((resolve, reject) => {
    const child = spawn(topologyPlanner, [], { stdio: ["pipe", "pipe", "pipe"] });
    const out = [];
    const err = [];
    child.stdout.on("data", (chunk) => out.push(chunk));
    child.stderr.on("data", (chunk) => err.push(chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) {
        resolve(Buffer.concat(out).toString("utf8"));
        return;
      }
      reject(new Error(
        Buffer.concat(err).toString("utf8").trim()
          || `operator chat topology planner exited with code ${code}`,
      ));
    });
    child.stdin.on("error", reject);
    child.stdin.end(request);
  });
  const plan = JSON.parse(stdout);
  if (!isObject(plan)) throw new Error("operator chat topology planner returned a non-object plan");
  requiredString(plan.run_id, "plan.run_id");
  requiredString(plan.chat_interface_id, "plan.chat_interface_id");
  requiredString(plan.model_interface_id, "plan.model_interface_id");
  requiredString(plan.worker_interface_id, "plan.worker_interface_id");
  requiredObject(plan.authoring_profile, "plan.authoring_profile");
  requiredArray(plan.required_interface_ids, "plan.required_interface_ids");
  requiredArray(plan.required_tool_ids, "plan.required_tool_ids");
  requiredArray(plan.interface_writes, "plan.interface_writes");
  requiredString(plan.run_record_content, "plan.run_record_content");
  return plan;
}

function plannedExactIds(value, field, validate) {
  const values = requiredArray(value, field).map((item, index) => (
    validate(item, `${field}[${index}]`)
  ));
  if (values.length === 0) throw new Error(`${field} must not be empty`);
  if (new Set(values).size !== values.length) throw new Error(`${field} repeats an exact id`);
  const sorted = [...values].sort();
  if (values.some((item, index) => item !== sorted[index])) {
    throw new Error(`${field} must be sorted`);
  }
  return values;
}

function plannedAdditionalRunWrites(plan, runsRoot) {
  const target = requiredObject(JSON.parse(plan.run_record_content), "planned target Run");
  const proposed = requiredArray(plan.run_writes, "plan.run_writes").map((write, index) => {
    requiredObject(write, `plan.run_writes[${index}]`);
    if (Object.keys(write).some((key) => !["path", "content", "tag", "expected_before_hash"].includes(key))) {
      throw new Error("planned additional Run contains unknown fields");
    }
    if (typeof write.content !== "string" || !write.content.trim()) throw new Error("additional Run content is required");
    const content = write.content;
    const record = requiredObject(JSON.parse(content), "additional Run record");
    const id = safeSegment(record.id, "additional Run id");
    const filePath = absolutePath(write.path, "additional Run path");
    if (id === target.id || filePath !== path.join(runsRoot, `${id}.json`)
      || write.tag !== `run:${id}` || write.expected_before_hash !== null) {
      throw new Error("additional Run write must create its exact declared path");
    }
    return { record, write: { filePath, content, tag: write.tag, expectedBeforeHash: null } };
  });
  if (new Set(proposed.map(({record}) => record.id)).size !== proposed.length) {
    throw new Error("additional Run writes repeat an identity");
  }
  const declared = new Map();
  const execution = [target];
  const context = target.interface_state?.[plan.chat_interface_id]?.voice_context;
  const voice = proposed.find(({record}) => record.id === context?.run_id)?.record;
  if (voice) {
    const origin = voice.interface_state?.[context.control_interface_id];
    const topology = voice.interface_state?.[context.provider_id]?.run_authoring_binding;
    if (voice.ee !== target.ee || voice.worker !== target.worker
      || origin?.home_run !== target.id || origin?.home_interface !== plan.chat_interface_id
      || topology?.run !== voice.id || topology?.worker !== voice.worker
      || topology?.provider_interface !== context.provider_id
      || target.interface_state?.[context.provider_id]
      || !topology?.checkpoints?.some((cp) => cp.role === "provider_interface_output"
        && cp.outbound_interface_bindings?.some((binding) => binding.interface === context.delegation_interface_id))) {
      throw new Error("voice Run differs from its exact independent Home binding");
    }
    declared.set(voice.id, voice);
    execution.push(voice);
  }
  for (const target of execution) {
    const states = requiredObject(target.interface_state, "planned target state");
    for (const [owner, state] of Object.entries(states)) {
      const writer = state.run_authoring_binding?.writer_run ?? state.writer_run;
      if (!writer) continue;
      const id = safeSegment(writer.run, "declared writer Run");
      if (declared.has(id) || execution.some((run) => run.id === id)) throw new Error("two owners declare the same writer Run");
      if (Object.hasOwn(writer, "initial_record_format") && !["pretty", "compact"].includes(writer.initial_record_format)) {
        throw new Error("writer Run declaration has invalid initial record format");
      }
      const record = requiredObject(writer.initial_record, "declared writer initial record");
      if (record.id !== id || record.ee !== target.ee || record.worker !== target.worker
        || record.interface_state?.[owner]?.file_writer_target?.run !== target.id) {
        throw new Error("writer Run declaration changes its target, worker or EE");
      }
      declared.set(id, record);
    }
  }
  return proposed.map(({record, write}) => {
    if (!declared.has(record.id) || canonicalJson(record) !== canonicalJson(declared.get(record.id))) {
      throw new Error("planned write differs from its exact declared initial Run");
    }
    declared.delete(record.id);
    return write;
  });
}

function plannedClientWrite(value, index, registryRoot) {
  const write = requiredObject(value, `plan.interface_writes[${index}]`);
  const unknown = Object.keys(write).filter((field) => ![
    "path",
    "content",
    "tag",
    "expected_before_hash",
  ].includes(field));
  if (unknown.length > 0) {
    throw new Error(`plan.interface_writes[${index}] contains unknown fields: ${unknown.join(", ")}`);
  }
  const filePath = absolutePath(write.path, `plan.interface_writes[${index}].path`);
  const descriptorRoots = [
    path.join(registryRoot, "interface"),
    path.join(registryRoot, "tool"),
  ];
  const artifactRoot = path.join(registryRoot, "artifact");
  const artifactParts = path.relative(artifactRoot, filePath).split(path.sep);
  const isArtifactDescriptor = artifactParts.length === 2
    && artifactParts[1] === "artifact.json"
    && pathIsWithin(artifactRoot, filePath);
  if (isArtifactDescriptor) {
    const id = plannedArtifactOwner(artifactParts[0]);
    const descriptor = requiredObject(JSON.parse(write.content), "planned artifact descriptor");
    if (descriptor.id !== id || write.tag !== `artifact:${id}`) {
      throw new Error("planned artifact descriptor identity differs from its exact owner");
    }
  }
  if (!isArtifactDescriptor && !descriptorRoots.some((root) => pathIsWithin(root, filePath))) {
    throw new Error(`plan.interface_writes[${index}].path escapes the interface/tool/artifact registries`);
  }
  if (typeof write.content !== "string" || write.content.length === 0) {
    throw new Error(`plan.interface_writes[${index}].content must be a non-empty string`);
  }
  const expectedBeforeHash = write.expected_before_hash;
  if (expectedBeforeHash !== null
    && (typeof expectedBeforeHash !== "string" || !/^[0-9a-f]{64}$/.test(expectedBeforeHash))) {
    throw new Error(
      `plan.interface_writes[${index}].expected_before_hash must be a lowercase SHA-256 hash or null`,
    );
  }
  return {
    filePath,
    content: write.content,
    tag: requiredString(write.tag, `plan.interface_writes[${index}].tag`),
    expectedBeforeHash,
  };
}

function plannedArtifactOwner(value) {
  const id = requiredTrimmedString(value, "planned artifact owner");
  if (id === "." || id === ".." || /[/\\\0]/.test(id)) {
    throw new Error("planned artifact owner must be one exact registry path component");
  }
  return id;
}

function plannedClientRemoval(value, index, registryRoot) {
  const removal = requiredObject(value, `plan.removals[${index}]`);
  const unknown = Object.keys(removal).filter((field) => !["path", "tag", "files"].includes(field));
  if (unknown.length > 0) {
    throw new Error(`plan.removals[${index}] contains unknown fields: ${unknown.join(", ")}`);
  }
  const directory = absolutePath(removal.path, `plan.removals[${index}].path`);
  const roots = [path.join(registryRoot, "interface"), path.join(registryRoot, "tool")];
  if (!roots.some((root) => pathIsWithin(root, directory))) {
    throw new Error(`plan.removals[${index}].path escapes the interface/tool/artifact registries`);
  }
  const files = requiredArray(removal.files, `plan.removals[${index}].files`).map((candidate, fileIndex) => {
    const file = requiredObject(candidate, `plan.removals[${index}].files[${fileIndex}]`);
    const relativePath = requiredString(
      file.relative_path,
      `plan.removals[${index}].files[${fileIndex}].relative_path`,
    );
    if (path.basename(relativePath) !== relativePath || relativePath.includes("\0")) {
      throw new Error(`plan.removals[${index}] contains a non-local file path`);
    }
    return {
      relativePath,
      sha256: safeSha256(file.sha256, `plan.removals[${index}].files[${fileIndex}].sha256`),
    };
  });
  if (files.length === 0 || new Set(files.map((file) => file.relativePath)).size !== files.length) {
    throw new Error(`plan.removals[${index}] must name unique files`);
  }
  return {
    directory,
    tag: requiredString(removal.tag, `plan.removals[${index}].tag`),
    files,
  };
}

function safeSha256(value, field) {
  const hash = requiredString(value, field);
  if (!/^[0-9a-f]{64}$/.test(hash)) throw new Error(`${field} must be a lowercase SHA-256 hash`);
  return hash;
}

function pathIsWithin(root, candidate) {
  const relative = path.relative(path.resolve(root), path.resolve(candidate));
  return relative !== ""
    && relative !== ".."
    && !relative.startsWith(`..${path.sep}`)
    && !path.isAbsolute(relative);
}

function validatePlannedRunRecord(
  content,
  runId,
  chatInterfaceId,
  eeId,
  workerInterfaceId,
  clientSurfaceRole,
) {
  let record;
  try {
    record = JSON.parse(content);
  } catch (error) {
    throw new Error(`plan.run_record_content is malformed JSON: ${error.message}`);
  }
  requiredObject(record, "plan run record");
  if (requiredString(record.id, "plan run record.id") !== runId) {
    throw new Error("topology plan run record changed its run identity");
  }
  if (requiredString(record.ee, "plan run record.ee") !== eeId) {
    throw new Error("topology plan run record changed its EE identity");
  }
  if (requiredString(record.worker, "plan run record.worker") !== workerInterfaceId) {
    throw new Error("topology plan run record changed its worker identity");
  }
  const interfaceState = requiredObject(record.interface_state, "plan run record.interface_state");
  const clientState = requiredObject(
    interfaceState[chatInterfaceId],
    `plan run record.interface_state.${chatInterfaceId}`,
  );
  if (clientState.client_surface_role !== clientSurfaceRole) {
    throw new Error("topology plan run record changed its client surface role");
  }
  return record;
}

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (isObject(value)) {
    return `{${Object.keys(value).sort().map((key) => (
      `${JSON.stringify(key)}:${canonicalJson(value[key])}`
    )).join(",")}}`;
  }
  return JSON.stringify(value);
}

function operatorSurfaceRole(value) {
  const role = requiredString(value, "clientSurfaceRole");
  if (role !== "home" && role !== "task") {
    throw new Error("clientSurfaceRole must be 'home' or 'task'");
  }
  return role;
}

function operatorDoorHostSlots(value) {
  if (value === undefined || value === null) return {};
  if (!isObject(value)) throw new Error("doorHostSlots must be a JSON object");
  const result = {};
  for (const [interfaceId, slots] of Object.entries(value)) {
    const interfaceKey = safeSegment(interfaceId, "doorHostSlots interface");
    if (!isObject(slots)) {
      throw new Error(`doorHostSlots.${interfaceKey} must be a JSON object`);
    }
    const configured = {};
    for (const [slot, root] of Object.entries(slots)) {
      const slotKey = safeSegment(slot, `doorHostSlots.${interfaceKey} slot`);
      configured[slotKey] = absolutePath(
        root,
        `doorHostSlots.${interfaceKey}.${slotKey}`,
      );
    }
    result[interfaceKey] = configured;
  }
  return result;
}

function operatorExternalInputs(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("externalInputs must be an array");
  const checkpoints = new Set();
  const interfaces = new Set();
  return value.map((input, index) => {
    if (!isObject(input)) throw new Error(`externalInputs[${index}] must be a JSON object`);
    const checkpointId = safeSegment(input.checkpointId, `externalInputs[${index}].checkpointId`);
    const interfaceId = safeSegment(input.interfaceId, `externalInputs[${index}].interfaceId`);
    const requiredVerifiedArtifacts = operatorRequiredVerifiedArtifacts(
      input.requiredVerifiedArtifacts,
      `externalInputs[${index}].requiredVerifiedArtifacts`,
    );
    if (checkpoints.has(checkpointId)) throw new Error(`externalInputs repeats checkpoint '${checkpointId}'`);
    if (interfaces.has(interfaceId)) throw new Error(`externalInputs repeats interface '${interfaceId}'`);
    checkpoints.add(checkpointId);
    interfaces.add(interfaceId);
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      ...(requiredVerifiedArtifacts.length > 0
        ? { required_verified_artifacts: requiredVerifiedArtifacts }
        : {}),
    };
  });
}

function operatorExternalRelays(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("externalRelays must be an array");
  return value.map((relay, index) => {
    if (!isObject(relay)) throw new Error(`externalRelays[${index}] must be a JSON object`);
    const checkpointId = safeSegment(relay.checkpointId, `externalRelays[${index}].checkpointId`);
    const interfaceId = safeSegment(relay.interfaceId, `externalRelays[${index}].interfaceId`);
    const requiredVerifiedArtifacts = operatorRequiredVerifiedArtifacts(
      relay.requiredVerifiedArtifacts,
      `externalRelays[${index}].requiredVerifiedArtifacts`,
    );
    if (!Array.isArray(relay.outboundInterfaceIds) || relay.outboundInterfaceIds.length === 0) {
      throw new Error(`externalRelays[${index}].outboundInterfaceIds must be a non-empty array`);
    }
    const outboundInterfaceIds = relay.outboundInterfaceIds.map((target, targetIndex) => (
      safeSegment(target, `externalRelays[${index}].outboundInterfaceIds[${targetIndex}]`)
    ));
    if (new Set(outboundInterfaceIds).size !== outboundInterfaceIds.length) {
      throw new Error(`externalRelays[${index}] repeats an outbound interface`);
    }
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      outbound_interface_ids: outboundInterfaceIds,
      ...(requiredVerifiedArtifacts.length > 0
        ? { required_verified_artifacts: requiredVerifiedArtifacts }
        : {}),
    };
  });
}

function operatorRequiredVerifiedArtifacts(value, field) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error(`${field} must be an array`);
  const labels = new Set();
  return value.map((requirement, index) => {
    if (!isObject(requirement) || !isObject(requirement.params)) {
      throw new Error(`${field}[${index}] must carry object params`);
    }
    const artifactLabel = safeContractToken(
      requirement.artifactLabel,
      `${field}[${index}].artifactLabel`,
    );
    if (labels.has(artifactLabel)) {
      throw new Error(`${field} repeats artifact label '${artifactLabel}'`);
    }
    labels.add(artifactLabel);
    return {
      mark: safeContractToken(requirement.mark, `${field}[${index}].mark`),
      artifact_label: artifactLabel,
      verifier: safeSegment(requirement.verifier, `${field}[${index}].verifier`),
      params: requirement.params,
    };
  });
}

function operatorModelOutputs(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("modelOutputs must be an array");
  const checkpoints = new Set();
  const interfaces = new Set();
  return value.map((output, index) => {
    if (!isObject(output)) throw new Error(`modelOutputs[${index}] must be a JSON object`);
    const checkpointId = safeSegment(output.checkpointId, `modelOutputs[${index}].checkpointId`);
    const interfaceId = safeSegment(output.interfaceId, `modelOutputs[${index}].interfaceId`);
    if (checkpoints.has(checkpointId)) throw new Error(`modelOutputs repeats checkpoint '${checkpointId}'`);
    if (interfaces.has(interfaceId)) throw new Error(`modelOutputs repeats interface '${interfaceId}'`);
    checkpoints.add(checkpointId);
    interfaces.add(interfaceId);
    const requirement = output.requiredVerifiedArtifact;
    if (!isObject(requirement) || !isObject(requirement.params)) {
      throw new Error(`modelOutputs[${index}].requiredVerifiedArtifact must carry object params`);
    }
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      required_verified_artifact: {
        mark: safeContractToken(requirement.mark, `modelOutputs[${index}].requiredVerifiedArtifact.mark`),
        artifact_label: safeContractToken(
          requirement.artifactLabel,
          `modelOutputs[${index}].requiredVerifiedArtifact.artifactLabel`,
        ),
        verifier: safeSegment(
          requirement.verifier,
          `modelOutputs[${index}].requiredVerifiedArtifact.verifier`,
        ),
        params: requirement.params,
      },
    };
  });
}

function operatorPayloadArtifactBindings(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error("payloadArtifactBindings must be an array");
  const ids = new Set();
  return value.map((reference, index) => {
    const field = `payloadArtifactBindings[${index}]`;
    if (!isObject(reference) || Object.keys(reference).length !== 2
      || !Object.hasOwn(reference, "id") || !Object.hasOwn(reference, "content")) {
      throw new Error(`${field} must carry exactly id and content`);
    }
    const id = reference.id;
    if (typeof id !== "string" || !id || id.trim() !== id || ids.has(id)) {
      throw new Error(`${field} has an invalid or repeated exact artifact identity`);
    }
    if (!Array.isArray(reference.content) || reference.content.length !== 32
      || reference.content.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)) {
      throw new Error(`${field}.content must be the actual 32-byte ArtifactRef hash`);
    }
    ids.add(id);
    return { id, content: [...reference.content] };
  });
}

function operatorContextArtifactBindings(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("contextArtifactBindings must be an array");
  const ids = new Set();
  return value.map((binding, index) => {
    if (!isObject(binding)) throw new Error(`contextArtifactBindings[${index}] must be a JSON object`);
    const id = safeSegment(binding.id, `contextArtifactBindings[${index}].id`);
    if (ids.has(id)) throw new Error(`contextArtifactBindings repeats '${id}'`);
    ids.add(id);
    return { id };
  });
}

function operatorSelectedWorkerEnvironmentKinds(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("selectedWorkerEnvironmentKinds must be an array");
  const kinds = new Set();
  return value.map((kind, index) => {
    const name = safeSegment(kind, `selectedWorkerEnvironmentKinds[${index}]`);
    if (kinds.has(name)) throw new Error(`selectedWorkerEnvironmentKinds repeats '${name}'`);
    kinds.add(name);
    return name;
  });
}

function operatorSelectedTools(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new Error("selectedTools must be an array");
  const tools = new Set();
  return value.map((selection, index) => {
    const selected = requiredObject(selection, `selectedTools[${index}]`);
    const unknown = Object.keys(selected).filter((field) => ![
      "tool", "interface", "workerEnvironmentKind",
    ].includes(field));
    if (unknown.length > 0) {
      throw new Error(`selectedTools[${index}] contains unknown fields: ${unknown.join(", ")}`);
    }
    const id = safeContractToken(selected.tool, `selectedTools[${index}].tool`);
    if (tools.has(id)) throw new Error(`selectedTools repeats '${id}'`);
    tools.add(id);
    return {
      tool: id,
      interface: safeSegment(selected.interface, `selectedTools[${index}].interface`),
      worker_environment_kind: safeSegment(
        selected.workerEnvironmentKind,
        `selectedTools[${index}].workerEnvironmentKind`,
      ),
    };
  });
}

function safeContractToken(value, field) {
  const token = requiredString(value, field);
  if (token.length > 160 || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(token)) {
    throw new Error(`${field} must be a compact contract token`);
  }
  return token;
}

function interfaceEndpoint(octopusHome, interfaceId) {
  return `unix://${interfaceSocketPath(octopusHome, interfaceId)}`;
}

function interfaceSocketPath(octopusHome, interfaceId) {
  return path.join(
    octopusHome,
    "app-server-control",
    `${crypto.createHash("sha256").update(safeSegment(interfaceId, "interfaceId")).digest("hex").slice(0, 24)}.sock`,
  );
}

async function interfaceSocketIdentity(octopusHome, interfaceId) {
  let stat;
  try { stat = await fs.lstat(interfaceSocketPath(octopusHome, interfaceId), { bigint: true }); }
  catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
  if (!stat.isSocket()) throw new Error("Chat interface endpoint is not a native socket");
  return `${stat.dev}:${stat.ino}:${stat.birthtimeNs}`;
}

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

async function commitClientWrites(writes) {
  const paths = new Set();
  for (const write of writes) {
    write.filePath = absolutePath(write.filePath, `${write.tag}.path`);
    if (typeof write.content !== "string") throw new Error(`${write.tag}.content must be a string`);
    if (write.expectedBeforeHash !== null
      && !/^[0-9a-f]{64}$/.test(stringValue(write.expectedBeforeHash))) {
      throw new Error(`${write.tag}.expectedBeforeHash must be a lowercase SHA-256 hash or null`);
    }
    if (paths.has(write.filePath)) {
      throw new Error(`client authoring repeats path '${write.filePath}'`);
    }
    paths.add(write.filePath);
  }
  const parents = new Set(writes.map((write) => path.dirname(write.filePath)));
  await Promise.all([...parents].map((parent) => fs.mkdir(parent, { recursive: true })));
  const staged = new Array(writes.length).fill(null);
  const evidence = new Array(writes.length).fill(null);
  let next = 0;
  let stageFailure = null;
  const lanes = Math.max(1, Math.min(8, writes.length));
  await Promise.all(Array.from({ length: lanes }, async () => {
    for (;;) {
      if (stageFailure) return;
      const index = next;
      next += 1;
      if (index >= writes.length) return;
      try {
        const outcome = await stageClientWrite(writes[index]);
        if (outcome.evidence) evidence[index] = outcome.evidence;
        else staged[index] = outcome;
      } catch (error) {
        if (!stageFailure) stageFailure = error;
        return;
      }
    }
  }));
  try {
    if (stageFailure) throw stageFailure;
    for (let index = 0; index < writes.length; index += 1) {
      if (evidence[index]) continue;
      evidence[index] = await installClientWrite(writes[index], staged[index]);
    }
  } finally {
    for (const entry of staged) {
      if (!entry) continue;
      await fs.unlink(entry.staged).catch((error) => {
        if (error?.code !== "ENOENT") throw error;
      });
    }
    for (const parent of parents) await syncDirectory(parent);
  }
  return evidence;
}

async function stageClientWrite(write) {
  const bytes = Buffer.from(write.content);
  const afterHash = sha256Hex(bytes);
  const parent = path.dirname(write.filePath);
  const initial = await readFileHashIfPresent(write.filePath);
  if (initial?.hash === afterHash) {
    return {
      evidence: {
        path: write.filePath,
        tag: write.tag,
        beforeHash: write.expectedBeforeHash,
        afterHash,
        bytes: bytes.byteLength,
        changed: false,
      },
    };
  }
  const observedHash = initial?.hash ?? null;
  if (observedHash !== write.expectedBeforeHash) {
    throw new Error(
      `client authoring conflict for '${write.filePath}': expected ${write.expectedBeforeHash ?? "<absent>"}, observed ${observedHash ?? "<absent>"}`,
    );
  }
  const staged = path.join(
    parent,
    `.${path.basename(write.filePath)}.octopus-client-${process.pid}-${randomToken(16)}.tmp`,
  );
  try {
    const handle = await fs.open(staged, "wx", 0o600);
    try {
      await handle.writeFile(bytes);
      await handle.sync();
    } finally {
      await handle.close();
    }
  } catch (error) {
    await fs.unlink(staged).catch(() => {});
    throw error;
  }
  return { staged, afterHash, byteLength: bytes.byteLength };
}

async function installClientWrite(write, entry) {
  const current = await readFileHashIfPresent(write.filePath);
  const currentHash = current?.hash ?? null;
  if (currentHash !== write.expectedBeforeHash) {
    throw new Error(
      `client authoring conflict for '${write.filePath}' after staging: expected ${write.expectedBeforeHash ?? "<absent>"}, observed ${currentHash ?? "<absent>"}`,
    );
  }
  if (write.expectedBeforeHash === null) {
    try {
      await fs.link(entry.staged, write.filePath);
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
      const raced = await readFileHashIfPresent(write.filePath);
      if (raced?.hash !== entry.afterHash) {
        throw new Error(`client authoring create raced with different bytes at '${write.filePath}'`);
      }
    }
  } else {
    await fs.rename(entry.staged, write.filePath);
  }
  const installed = await readFileHashIfPresent(write.filePath);
  if (installed?.hash !== entry.afterHash) {
    throw new Error(`client authoring installed unexpected bytes at '${write.filePath}'`);
  }
  return {
    path: write.filePath,
    tag: write.tag,
    beforeHash: write.expectedBeforeHash,
    afterHash: entry.afterHash,
    bytes: entry.byteLength,
    changed: true,
  };
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
  return absolutePath(path.resolve(__dirname, "..", ".."), "registryRoot");
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
  if (payload.text !== input.requestedText) {
    throw new Error(`operator message '${input.messageId}' conflicts with its durable prepared envelope`);
  }
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

function operatorMessagePayload(value, messageId, attachmentRoot) {
  const payload = requiredObject(value, "operator message payload");
  const fields = ["text", "message_id", "attachments"];
  if (
    Object.keys(payload).length !== fields.length
    || fields.some((field) => !Object.hasOwn(payload, field))
  ) {
    throw new Error("operator message payload has an invalid field set");
  }
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
  const tail = await readCompleteLedgerTail(movementsPath, priorOffset);
  let latestSnapshot = null;
  let operatorMessageSeq = null;
  let terminalAfterOperatorMessage = false;
  for (let index = 0; index < tail.lines.length; index += 1) {
    let movement;
    try {
      movement = JSON.parse(tail.lines[index]);
    } catch (error) {
      throw new Error(`movement ledger contains malformed complete JSON at tail line ${index + 1}: ${error.message}`);
    }
    const artifactId = stringValue(movement?.artifact?.id);
    const arrivalRuntime = operatorMessageCheckpoint
      && artifactIdHasLabel(artifactId, operatorMessageArtifactLabel)
      ? exactArrivalRuntime(movement, interfaceId)
      : null;
    if (
      arrivalRuntime?.run === runId
      && arrivalRuntime.checkpoint === operatorMessageCheckpoint.id
      && arrivalRuntime.epoch === operatorMessageCheckpoint.epoch
    ) {
      operatorMessageSeq = nonNegativeInteger(movement.seq, "operator message arrival seq");
    }
    const outboundRuntime = exactOutboundRuntime(movement, interfaceId);
    const snapshot = outboundRuntime?.run === runId && (
        artifactIdHasLabel(artifactId, terminalArtifactLabel)
        || artifactIdHasLabel(artifactId, errorArtifactLabel)
      );
    if (!snapshot) continue;
    if (
      operatorMessageSeq !== null
      && nonNegativeInteger(movement.seq, "terminal snapshot seq") > operatorMessageSeq
    ) {
      terminalAfterOperatorMessage = true;
    }
    if (!isObject(movement.artifact)) continue;
    latestSnapshot = {
      id: requiredString(movement.artifact.id, "movement artifact.id"),
    };
  }
  const retainedSnapshot = latestSnapshot
    ?? priorBindings.at(-1)
    ?? continuation?.artifactBindings?.at(-1)
    ?? null;
  return {
    ledgerOffset: tail.nextOffset,
    artifactBindings: retainedSnapshot ? uniqueArtifactBindings([retainedSnapshot]) : [],
    pinnedArtifactBindings,
    pinnedContextItems,
    continuedFrom,
    terminalAfterOperatorMessage,
    operatorMessageCheckpointAdmitted: operatorMessageSeq !== null,
  };
}

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

async function waitForProviderModelCatalogObservation(input) {
  let offset = input.startOffset;
  for (;;) {
    const remaining = providerModelCatalogRemainingMs(input.deadline);
    const change = watchLedgerChange(input.movementsPath, remaining);
    let tail;
    try {
      tail = await readCompleteLedgerTail(input.movementsPath, offset);
      for (let index = 0; index < tail.records.length; index += 1) {
        let movement;
        try {
          movement = JSON.parse(tail.records[index].text);
        } catch (error) {
          throw new Error(`movement ledger contains malformed complete JSON at model-catalog tail line ${index + 1}: ${error.message}`);
        }
        offset = tail.records[index].nextOffset;
        const observed = await providerModelCatalogOutput(
          movement,
          input,
        );
        if (!observed) continue;
        change.cancel();
        return observed;
      }
    } catch (error) {
      change.cancel();
      throw error;
    }
    if (!(await change.promise)) {
      change.cancel();
      throw new Error("provider model catalog did not produce an exact client observation in time");
    }
  }
}

async function providerModelCatalogOutput(movement, input) {
  if (
    movement?.kind !== "InterfaceOutbound"
    || endpointInterfaceId(movement.to) !== input.input.interfaceId
  ) {
    return null;
  }
  const seq = nonNegativeInteger(movement.seq, "provider model catalog movement seq");
  if (seq <= input.requestSeq) return null;
  const runtime = exactOutboundRuntime(movement, input.input.interfaceId);
  if (!runtime || runtime.run !== input.input.runId) return null;
  if (endpointInterfaceId(movement.from) !== input.context.runtimeProviderInterfaceId) return null;
  const artifactId = requiredString(movement?.artifact?.id, "provider model catalog artifact id");
  const label = artifactLabel(artifactId);
  if (![input.context.outputLabel, input.context.operationErrorLabel, input.context.turnErrorLabel].includes(label)) {
    return null;
  }
  const artifact = providerAccountArtifactCoordinates(
    movement,
    label,
    "provider model catalog output",
  );
  const resolved = await resolveProviderAccountArtifact(
    input.readArtifactPayload,
    artifact,
    label,
  );
  if (label === input.context.operationErrorLabel) {
    throw providerAccountOperationError(resolved.payload, input.context.requestLabel);
  }
  if (label === input.context.turnErrorLabel) {
    const error = providerAccountTurnError(resolved.payload);
    if (!error.repairable) {
      error.name = "ProviderModelCatalogError";
      error.code = "provider_model_catalog_turn_error";
      error.message = "The provider failed to refresh the model catalog.";
    }
    throw error;
  }
  return {
    ...validateProviderModelCatalogPayload(resolved.payload, input.context.outputLabel),
    observedAtUnixMs: nonNegativeInteger(
      movement.recorded_at_unix_ms,
      "provider model catalog observation timestamp",
    ),
  };
}

function validateProviderModelCatalogPayload(value, expectedLabel) {
  const payload = requiredObject(value, "provider model catalog payload");
  if (payload.label !== expectedLabel) {
    throw new Error("provider model catalog payload carries the wrong label");
  }
  const models = requiredArray(payload.models, "provider model catalog models");
  const normalized = [];
  const seen = new Set();
  for (let index = 0; index < models.length; index += 1) {
    const model = requiredObject(models[index], `provider model catalog model ${index}`);
    const modelId = requiredString(model.model, `provider model catalog model ${index}.model`);
    if (seen.has(modelId)) throw new Error(`provider model catalog repeats '${modelId}'`);
    seen.add(modelId);
    const efforts = requiredArray(
      model.supportedReasoningEfforts,
      `provider model catalog model ${index}.supportedReasoningEfforts`,
    ).map((value, effortIndex) => {
      const effort = requiredObject(
        value,
        `provider model catalog model ${index}.supportedReasoningEfforts ${effortIndex}`,
      );
      return requiredString(
        effort.reasoningEffort,
        `provider model catalog model ${index}.supportedReasoningEfforts ${effortIndex}.reasoningEffort`,
      );
    });
    if (new Set(efforts).size !== efforts.length) {
      throw new Error(`provider model '${modelId}' repeats a reasoning effort`);
    }
    const defaultReasoningEffort = requiredString(
      model.defaultReasoningEffort,
      `provider model catalog model ${index}.defaultReasoningEffort`,
    );
    if (!efforts.includes(defaultReasoningEffort)) {
      throw new Error(`provider model '${modelId}' default reasoning effort is outside its supported list`);
    }
    normalized.push({
      model: modelId,
      displayName: requiredString(
        model.displayName,
        `provider model catalog model ${index}.displayName`,
      ),
      description: stringValue(model.description),
      defaultReasoningEffort,
      supportedReasoningEfforts: efforts,
      showInPicker: model.hidden !== true,
      isDefault: model.isDefault === true,
    });
  }
  let nextCursor = null;
  if (payload.next_cursor !== null) {
    nextCursor = requiredString(payload.next_cursor, "provider model catalog next_cursor");
  }
  return { models: normalized, nextCursor };
}

function providerModelCatalogRemainingMs(deadline) {
  const remaining = deadline - Date.now();
  if (remaining <= 0) throw new Error("provider model catalog refresh timed out");
  return remaining;
}

async function providerAccountSnapshotCoordinates(
  movement,
  input,
  context,
  readArtifactPayload,
  requestSeq,
) {
  const seq = nonNegativeInteger(movement?.seq, "provider account snapshot movement seq");
  if (seq <= requestSeq) {
    throw new Error("provider account snapshot does not follow its admitted request");
  }
  const runtime = exactOutboundRuntime(movement, input.interfaceId);
  if (!runtime || runtime.run !== input.runId) {
    throw new Error("provider account snapshot has the wrong Home/run coordinates");
  }
  if (endpointInterfaceId(movement.from) !== context.runtimeProviderInterfaceId) {
    throw new Error("provider account snapshot came from the wrong exact provider interface");
  }
  const artifact = providerAccountArtifactCoordinates(
    movement,
    context.snapshotLabel,
    "provider account snapshot",
  );
  const resolved = await resolveProviderAccountArtifact(
    readArtifactPayload,
    artifact,
    context.snapshotLabel,
  );
  validateProviderAccountSnapshotPayload(resolved.payload);
  return {
    snapshotMovementSeq: seq,
    snapshotCheckpointId: requiredString(runtime.checkpoint, "provider account checkpoint"),
    snapshotArtifactId: artifact.artifactId,
    snapshotManifestHash: artifact.manifestHash,
    snapshotPayloadHash: resolved.payloadHash,
    observedAtUnixMs: nonNegativeInteger(
      movement.recorded_at_unix_ms,
      "provider account observation timestamp",
    ),
  };
}

function providerAccountArtifactCoordinates(movement, expectedLabel, field) {
  const artifact = requiredObject(movement?.artifact, `${field} artifact`);
  const artifactId = requiredString(artifact.id, `${field} artifact id`);
  const manifestHash = contentHashHex(artifact.content, `${field} artifact content`);
  if (artifactId !== `${expectedLabel}:${manifestHash}`) {
    throw new Error(`${field} identity does not match its content hash`);
  }
  return { artifactId, manifestHash };
}

async function resolveProviderAccountArtifact(readArtifactPayload, artifact, label) {
  const resolved = await readArtifactPayload({
    artifactId: artifact.artifactId,
    manifestHash: artifact.manifestHash,
    label,
  });
  if (
    !isObject(resolved)
    || resolved.label !== label
    || contentHashHex(resolved.manifestHash, "resolved provider artifact manifest hash")
      !== artifact.manifestHash
  ) {
    throw new Error("resolved provider artifact differs from its ledger identity");
  }
  return {
    payload: resolved.payload,
    payloadHash: contentHashHex(
      resolved.payloadHash,
      "resolved provider artifact payload hash",
    ),
  };
}

function validateProviderAccountSnapshotPayload(value) {
  const payload = requiredObject(value, "provider account snapshot payload");
  if (payload.account === null) {
    throw providerAccountVerificationError(
      "provider_account_auth_required",
      "The provider account requires a fresh sign-in.",
      true,
      true,
    );
  }
  if (typeof payload.requiresOpenaiAuth !== "boolean" || !isObject(payload.account)) {
    throw providerAccountVerificationError(
      "provider_account_snapshot_malformed",
      "The provider account snapshot did not satisfy its declared contract.",
      false,
      false,
    );
  }
}

function providerAccountVerificationError(code, message, retryable, repairable) {
  const error = new Error(message);
  error.name = "ProviderAccountVerificationError";
  error.code = code;
  error.retryable = retryable;
  error.repairable = repairable;
  return error;
}

async function waitForProviderAccountObservation(input) {
  const deadline = Date.now() + input.timeoutMs;
  let cursor = normalizeLiveCursor(input.cursor);
  for (;;) {
    const remaining = Math.max(0, deadline - Date.now());
    const change = watchLedgerChange(input.movementsPath, remaining);
    let tail;
    try {
      tail = await readCompleteLedgerTail(input.movementsPath, cursor.offset);
      let expectedSeq = cursor.seq;
      for (let index = 0; index < tail.records.length; index += 1) {
        const record = tail.records[index];
        let movement;
        try {
          movement = JSON.parse(record.text);
        } catch (error) {
          throw new Error(`movement ledger contains malformed complete JSON at tail line ${index + 1}: ${error.message}`);
        }
        const seq = nonNegativeInteger(movement?.seq, "movement seq");
        if (expectedSeq !== null && seq !== expectedSeq) {
          throw new Error(`movement ledger is non-contiguous at seq ${seq}; expected ${expectedSeq}`);
        }
        expectedSeq = seq + 1;
        const nextCursor = { offset: record.nextOffset, seq: expectedSeq };
        let snapshot;
        try {
          snapshot = await providerAccountHomeOutput(
            movement,
            input,
            seq,
            input.requestCoordinates.requestMovementSeq,
          );
        } catch (error) {
          if (error?.consumeProviderAccountOutput === true) {
            Object.defineProperty(error, "providerAccountCursor", { value: nextCursor });
          }
          throw error;
        }
        cursor = nextCursor;
        if (!snapshot) continue;
        change.cancel();
        return {
          cursor,
          evidence: providerAccountEvidenceFromCoordinates(
            input,
            input.requestCoordinates,
            snapshot,
          ),
        };
      }
    } catch (error) {
      change.cancel();
      throw error;
    }
    if (remaining === 0 || !(await change.promise)) {
      change.cancel();
      throw providerAccountVerificationError(
        "provider_account_read_timeout",
        "The provider account read did not produce an exact Home observation in time.",
        true,
        false,
      );
    }
  }
}

async function providerAccountHomeOutput(movement, input, seq, requestSeq) {
  if (movement?.kind !== "InterfaceOutbound" || endpointInterfaceId(movement.to) !== input.interfaceId) {
    return null;
  }
  const runtime = exactOutboundRuntime(movement, input.interfaceId);
  if (!runtime || runtime.run !== input.runId) {
    throw new Error("provider account wait observed an outbound for the wrong Home run");
  }
  const source = endpointInterfaceId(movement.from);
  if (!source) throw new Error("provider account wait observed a non-interface Home outbound");
  const artifactId = requiredString(movement?.artifact?.id, "Home outbound artifact id");
  const label = artifactLabel(artifactId);
  providerAccountArtifactCoordinates(movement, label, "Home outbound");
  if (seq <= requestSeq) {
    if (source === input.context.runtimeProviderInterfaceId && label === input.context.snapshotLabel) return null;
    if (source === input.context.runtimeProviderInterfaceId && label === input.context.operationErrorLabel) {
      const artifact = providerAccountArtifactCoordinates(movement, label, "provider operation error");
      const resolved = await resolveProviderAccountArtifact(
        input.readArtifactPayload,
        artifact,
        label,
      );
      const payload = requiredObject(resolved.payload, "provider operation error payload");
      if (payload.operation === input.context.requestLabel && payload.status === "error") return null;
    }
    if (source === input.context.runtimeProviderInterfaceId && label === input.context.turnErrorLabel) {
      const artifact = providerAccountArtifactCoordinates(movement, label, "provider turn error");
      const resolved = await resolveProviderAccountArtifact(
        input.readArtifactPayload,
        artifact,
        label,
      );
      if (providerAccountTurnError(resolved.payload).repairable === true) return null;
    }
    throw new Error("provider account wait refuses to consume an unrelated pending Home output");
  }
  if (source !== input.context.runtimeProviderInterfaceId) {
    throw new Error("provider account wait observed output from the wrong exact provider interface");
  }
  if (label === input.context.snapshotLabel) {
    try {
      return await providerAccountSnapshotCoordinates(
        movement,
        input,
        input.context,
        input.readArtifactPayload,
        requestSeq,
      );
    } catch (error) {
      if (error?.name === "ProviderAccountVerificationError") {
        Object.defineProperty(error, "consumeProviderAccountOutput", { value: true });
      }
      throw error;
    }
  }
  if (label === input.context.operationErrorLabel) {
    const artifact = providerAccountArtifactCoordinates(movement, label, "provider operation error");
    const resolved = await resolveProviderAccountArtifact(
      input.readArtifactPayload,
      artifact,
      label,
    );
    const error = providerAccountOperationError(resolved.payload, input.context.requestLabel);
    Object.defineProperty(error, "consumeProviderAccountOutput", { value: true });
    throw error;
  }
  if (label === input.context.turnErrorLabel) {
    const artifact = providerAccountArtifactCoordinates(movement, label, "provider turn error");
    const resolved = await resolveProviderAccountArtifact(
      input.readArtifactPayload,
      artifact,
      label,
    );
    const error = providerAccountTurnError(resolved.payload);
    if (error.repairable === true) {
      Object.defineProperty(error, "consumeProviderAccountOutput", { value: true });
    }
    throw error;
  }
  throw new Error("provider account wait refuses to consume an unrelated Home output");
}

function providerAccountOperationError(value, expectedOperation) {
  const payload = requiredObject(value, "provider operation error payload");
  const detail = requiredObject(payload.error, "provider operation error detail");
  if (payload.operation !== expectedOperation || payload.status !== "error") {
    return providerAccountVerificationError(
      "provider_account_operation_error_malformed",
      "The provider returned an invalid operation error artifact.",
      false,
      false,
    );
  }
  return providerAccountVerificationError(
    "provider_account_operation_error",
    "The provider operation failed inside the provider interface.",
    detail.retryable === true,
    false,
  );
}

function providerAccountTurnError(value) {
  const payload = requiredObject(value, "provider turn error payload");
  const authenticationFailure = payload.failure_code === "provider.authentication.invalid";
  return providerAccountVerificationError(
    authenticationFailure ? "provider_account_auth_invalid" : "provider_account_turn_error",
    authenticationFailure
      ? "The provider rejected the current account authentication."
      : "The provider emitted a turn error while reading the account.",
    authenticationFailure || payload.will_retry === true,
    authenticationFailure,
  );
}

async function waitForProviderLoginStarted(input) {
  return waitForProviderLoginMovement(input, async (movement, seq) => {
    const output = await providerLoginStartedHomeOutput(movement, input, seq);
    if (!output) return null;
    return { done: true, loginId: output.loginId, authUrl: output.authUrl };
  });
}

async function waitForProviderLoginStatus(input) {
  let completion = null;
  return waitForProviderLoginMovement(input, async (movement, seq) => {
    const output = await providerLoginStatusHomeOutput(movement, input, seq);
    if (!output || output.kind === "account_updated") return null;
    if (output.kind === "completion") {
      if (completion !== null && completion !== output.success) {
        throw providerAccountVerificationError(
          "provider_account_login_completion_conflict",
          "The provider emitted conflicting login completion observations.",
          false,
          false,
        );
      }
      completion = output.success;
      return null;
    }
    if (output.status === "pending") {
      if (completion !== null) {
        throw providerAccountVerificationError(
          "provider_account_login_status_conflict",
          "The provider login status conflicts with its completion observation.",
          false,
          false,
        );
      }
      return { done: true, completed: false };
    }
    if (output.status === "cancelled") {
      throw providerAccountVerificationError(
        "provider_account_login_cancelled",
        "The provider sign-in was cancelled.",
        true,
        false,
      );
    }
    if (completion !== null && completion !== output.success) {
      throw providerAccountVerificationError(
        "provider_account_login_status_conflict",
        "The provider login status conflicts with its completion observation.",
        false,
        false,
      );
    }
    if (output.success === true && completion !== true) {
      throw providerAccountVerificationError(
        "provider_account_login_completion_missing",
        "The provider reported a completed sign-in without its exact completion observation.",
        false,
        false,
      );
    }
    if (output.success !== true) {
      throw providerAccountVerificationError(
        "provider_account_login_failed",
        "The provider sign-in did not complete successfully.",
        true,
        false,
      );
    }
    return { done: true, completed: true };
  });
}

async function waitForProviderLoginMovement(input, observe) {
  let cursor = normalizeLiveCursor(input.cursor);
  for (;;) {
    const remaining = providerLoginRemainingMs(input.deadline);
    const change = watchLedgerChange(input.movementsPath, remaining);
    try {
      const tail = await readCompleteLedgerTail(input.movementsPath, cursor.offset);
      let expectedSeq = cursor.seq;
      for (let index = 0; index < tail.records.length; index += 1) {
        let movement;
        try {
          movement = JSON.parse(tail.records[index].text);
        } catch (error) {
          throw new Error(`movement ledger contains malformed complete JSON at tail line ${index + 1}: ${error.message}`);
        }
        const seq = nonNegativeInteger(movement?.seq, "movement seq");
        if (expectedSeq !== null && seq !== expectedSeq) {
          throw new Error(`movement ledger is non-contiguous at seq ${seq}; expected ${expectedSeq}`);
        }
        expectedSeq = seq + 1;
        const nextCursor = { offset: tail.records[index].nextOffset, seq: expectedSeq };
        let observed;
        try {
          observed = await observe(movement, seq);
        } catch (error) {
          Object.defineProperty(error, "providerAccountCursor", { value: nextCursor });
          throw error;
        }
        cursor = nextCursor;
        if (!observed?.done) continue;
        change.cancel();
        return { ...observed, cursor };
      }
    } catch (error) {
      change.cancel();
      throw error;
    }
    if (!(await change.promise)) {
      change.cancel();
      const error = providerAccountVerificationError(
        "provider_account_login_timeout",
        "The provider sign-in did not produce an exact Home observation in time.",
        true,
        false,
      );
      Object.defineProperty(error, "providerAccountCursor", { value: cursor });
      throw error;
    }
  }
}

async function providerLoginStartedHomeOutput(movement, input, seq) {
  if (movement?.kind !== "InterfaceOutbound" || endpointInterfaceId(movement.to) !== input.interfaceId) {
    return null;
  }
  const runtime = exactOutboundRuntime(movement, input.interfaceId);
  if (!runtime || runtime.run !== input.runId) {
    throw new Error("provider login start observed an outbound for the wrong Home run");
  }
  if (endpointInterfaceId(movement.from) !== input.context.runtimeProviderInterfaceId) {
    throw new Error("provider login start observed output from the wrong exact provider interface");
  }
  if (seq <= input.requestSeq) {
    throw new Error("provider login start observed a Home output that does not follow its request");
  }
  const artifactId = requiredString(movement?.artifact?.id, "provider login Home output artifact id");
  const label = artifactLabel(artifactId);
  const artifact = providerAccountArtifactCoordinates(movement, label, "provider login Home output");
  const resolved = await resolveProviderAccountArtifact(input.readArtifactPayload, artifact, label);
  if (label === input.context.login.startedLabel) {
    return validateProviderLoginStartedPayload(resolved.payload, input.context.login.startRequestLabel);
  }
  if (label === input.context.operationErrorLabel) {
    throw providerAccountOperationError(resolved.payload, input.context.login.startRequestLabel);
  }
  if (label === input.context.turnErrorLabel) {
    throw providerAccountTurnError(resolved.payload);
  }
  throw new Error("provider login start refuses to consume an unrelated Home output");
}

async function providerLoginStatusHomeOutput(movement, input, seq) {
  if (movement?.kind !== "InterfaceOutbound" || endpointInterfaceId(movement.to) !== input.interfaceId) {
    return null;
  }
  const runtime = exactOutboundRuntime(movement, input.interfaceId);
  if (!runtime || runtime.run !== input.runId) {
    throw new Error("provider login status observed an outbound for the wrong Home run");
  }
  if (endpointInterfaceId(movement.from) !== input.context.runtimeProviderInterfaceId) {
    throw new Error("provider login status observed output from the wrong exact provider interface");
  }
  const artifactId = requiredString(movement?.artifact?.id, "provider login status artifact id");
  const label = artifactLabel(artifactId);
  const artifact = providerAccountArtifactCoordinates(movement, label, "provider login status output");
  const resolved = await resolveProviderAccountArtifact(input.readArtifactPayload, artifact, label);
  if (label === input.context.login.completedLabel) {
    if (seq <= input.startSeq) {
      throw new Error("provider login completion does not follow its login-start request");
    }
    return { kind: "completion", ...validateProviderLoginCompletedPayload(resolved.payload, input.loginId) };
  }
  if (label === input.context.login.statusLabel) {
    if (seq <= input.requestSeq) {
      throw new Error("provider login status does not follow its status-read request");
    }
    return { kind: "status", ...validateProviderLoginStatusPayload(resolved.payload, input.loginId, input.context.login.statusRequestLabel) };
  }
  if (label === input.context.login.accountUpdatedLabel) {
    if (seq <= input.startSeq) {
      throw new Error("provider account update does not follow its login-start request");
    }
    requiredObject(resolved.payload, "provider account-updated payload");
    return { kind: "account_updated" };
  }
  if (label === input.context.operationErrorLabel) {
    if (seq <= input.requestSeq) {
      throw new Error("provider login-status error does not follow its request");
    }
    throw providerAccountOperationError(resolved.payload, input.context.login.statusRequestLabel);
  }
  if (label === input.context.turnErrorLabel) {
    if (seq <= input.requestSeq) {
      throw new Error("provider login turn error does not follow its status request");
    }
    throw providerAccountTurnError(resolved.payload);
  }
  throw new Error("provider login status refuses to consume an unrelated Home output");
}

function validateProviderLoginStartedPayload(value, expectedOperation) {
  const payload = providerExactPayload(value, "provider login-start payload", [
    "operation",
    "method",
    "status",
    "login_id",
    "auth_url",
  ]);
  if (
    payload.operation !== expectedOperation
    || payload.method !== "chatgpt_browser"
    || payload.status !== "pending"
  ) {
    throw providerAccountVerificationError(
      "provider_account_login_started_malformed",
      "The provider returned an invalid browser sign-in start observation.",
      false,
      false,
    );
  }
  return {
    loginId: providerLoginId(payload.login_id),
    authUrl: providerLoginExternalUrl(payload.auth_url),
  };
}

function validateProviderLoginCompletedPayload(value, expectedLoginId) {
  const payload = providerExactPayload(value, "provider login-completed payload", [
    "login_id",
    "status",
    "success",
    "failure",
  ]);
  if (providerLoginId(payload.login_id) !== expectedLoginId) {
    throw providerAccountVerificationError(
      "provider_account_login_id_mismatch",
      "The provider completed a different sign-in attempt.",
      false,
      false,
    );
  }
  if (
    typeof payload.success !== "boolean"
    || payload.status !== (payload.success ? "completed" : "failed")
    || (payload.success ? payload.failure !== null : !isObject(payload.failure))
  ) {
    throw providerAccountVerificationError(
      "provider_account_login_completed_malformed",
      "The provider returned an invalid sign-in completion observation.",
      false,
      false,
    );
  }
  return { success: payload.success };
}

function validateProviderLoginStatusPayload(value, expectedLoginId, expectedOperation) {
  const payload = providerExactPayload(value, "provider login-status payload", [
    "operation",
    "login_id",
    "state",
  ]);
  if (payload.operation !== expectedOperation || providerLoginId(payload.login_id) !== expectedLoginId) {
    throw providerAccountVerificationError(
      "provider_account_login_id_mismatch",
      "The provider returned status for a different sign-in attempt.",
      false,
      false,
    );
  }
  const state = requiredObject(payload.state, "provider login state");
  if (state.status === "pending") {
    providerExactPayload(state, "provider pending login state", ["status", "method"]);
    if (state.method !== "chatgpt_browser") {
      throw providerAccountVerificationError(
        "provider_account_login_status_malformed",
        "The provider returned an invalid pending browser sign-in state.",
        false,
        false,
      );
    }
    return { status: "pending", success: null };
  }
  if (state.status === "completed") {
    providerExactPayload(state, "provider completed login state", ["status", "success"]);
    if (typeof state.success !== "boolean") {
      throw providerAccountVerificationError(
        "provider_account_login_status_malformed",
        "The provider returned an invalid completed sign-in state.",
        false,
        false,
      );
    }
    return { status: "completed", success: state.success };
  }
  if (state.status === "cancelled") {
    providerExactPayload(state, "provider cancelled login state", ["status"]);
    return { status: "cancelled", success: false };
  }
  throw providerAccountVerificationError(
    "provider_account_login_status_malformed",
    "The provider returned an unknown sign-in state.",
    false,
    false,
  );
}

function providerExactPayload(value, field, fields) {
  const payload = requiredObject(value, field);
  const keys = Object.keys(payload);
  if (keys.length !== fields.length || keys.some((key) => !fields.includes(key))) {
    throw providerAccountVerificationError(
      "provider_account_login_payload_malformed",
      `The ${field} has an invalid field set.`,
      false,
      false,
    );
  }
  return payload;
}

function providerLoginId(value) {
  const loginId = requiredTrimmedString(value, "provider login_id");
  if (loginId.length > 512 || /[\u0000-\u001f\u007f]/.test(loginId)) {
    throw providerAccountVerificationError(
      "provider_account_login_id_invalid",
      "The provider returned an invalid sign-in identity.",
      false,
      false,
    );
  }
  return loginId;
}

function providerLoginExternalUrl(value) {
  const authUrl = requiredTrimmedString(value, "provider auth_url");
  let parsed;
  try {
    parsed = new URL(authUrl);
  } catch {
    parsed = null;
  }
  if (
    !parsed
    || parsed.protocol !== "https:"
    || !parsed.hostname
    || parsed.username
    || parsed.password
    || authUrl.length > 4_096
  ) {
    throw providerAccountVerificationError(
      "provider_account_login_url_invalid",
      "The provider returned an unsafe browser sign-in URL.",
      false,
      false,
    );
  }
  return authUrl;
}

function providerLoginRemainingMs(deadline) {
  const remaining = deadline - Date.now();
  if (!Number.isSafeInteger(remaining) || remaining <= 0) {
    throw providerAccountVerificationError(
      "provider_account_login_timeout",
      "The provider sign-in did not complete before its bounded deadline.",
      true,
      false,
    );
  }
  return remaining;
}

async function delayProviderLoginPoll(delayMs, deadline) {
  const remaining = providerLoginRemainingMs(deadline);
  await new Promise((resolve) => setTimeout(resolve, Math.min(delayMs, remaining)));
  providerLoginRemainingMs(deadline);
}

function providerAccountEvidenceFromCoordinates(input, request, snapshot) {
  return {
    schema: PROVIDER_ACCOUNT_EVIDENCE_SCHEMA,
    schema_version: 1,
    operation_id: input.operationId,
    run_id: input.runId,
    interface_id: input.interfaceId,
    provider_interface_id: input.providerInterfaceId,
    runtime_provider_interface_id: input.context.runtimeProviderInterfaceId,
    request_movement_seq: request.requestMovementSeq,
    request_artifact_id: request.requestArtifactId,
    request_manifest_hash: request.requestManifestHash,
    request_payload_hash: request.requestPayloadHash,
    snapshot_movement_seq: snapshot.snapshotMovementSeq,
    snapshot_checkpoint_id: snapshot.snapshotCheckpointId,
    snapshot_artifact_id: snapshot.snapshotArtifactId,
    snapshot_manifest_hash: snapshot.snapshotManifestHash,
    snapshot_payload_hash: snapshot.snapshotPayloadHash,
    observed_at_unix_ms: snapshot.observedAtUnixMs,
  };
}

function providerAccountEvidence(value) {
  const evidence = requiredObject(value, "provider account evidence");
  const fields = [
    "schema",
    "schema_version",
    "operation_id",
    "run_id",
    "interface_id",
    "provider_interface_id",
    "runtime_provider_interface_id",
    "request_movement_seq",
    "request_artifact_id",
    "request_manifest_hash",
    "request_payload_hash",
    "snapshot_movement_seq",
    "snapshot_checkpoint_id",
    "snapshot_artifact_id",
    "snapshot_manifest_hash",
    "snapshot_payload_hash",
    "observed_at_unix_ms",
  ];
  const unknown = Object.keys(evidence).filter((field) => !fields.includes(field));
  if (unknown.length > 0 || fields.some((field) => !Object.hasOwn(evidence, field))) {
    throw new Error("provider account evidence has an invalid field set");
  }
  if (evidence.schema !== PROVIDER_ACCOUNT_EVIDENCE_SCHEMA || evidence.schema_version !== 1) {
    throw new Error("provider account evidence has an unsupported schema");
  }
  const normalized = {
    schema: PROVIDER_ACCOUNT_EVIDENCE_SCHEMA,
    schema_version: 1,
    operation_id: safeSegment(evidence.operation_id, "evidence.operation_id"),
    run_id: safeSegment(evidence.run_id, "evidence.run_id"),
    interface_id: safeSegment(evidence.interface_id, "evidence.interface_id"),
    provider_interface_id: safeSegment(
      evidence.provider_interface_id,
      "evidence.provider_interface_id",
    ),
    runtime_provider_interface_id: safeSegment(
      evidence.runtime_provider_interface_id,
      "evidence.runtime_provider_interface_id",
    ),
    request_movement_seq: nonNegativeInteger(
      evidence.request_movement_seq,
      "evidence.request_movement_seq",
    ),
    request_artifact_id: requiredString(
      evidence.request_artifact_id,
      "evidence.request_artifact_id",
    ),
    request_manifest_hash: contentHashHex(
      evidence.request_manifest_hash,
      "evidence.request_manifest_hash",
    ),
    request_payload_hash: contentHashHex(
      evidence.request_payload_hash,
      "evidence.request_payload_hash",
    ),
    snapshot_movement_seq: nonNegativeInteger(
      evidence.snapshot_movement_seq,
      "evidence.snapshot_movement_seq",
    ),
    snapshot_checkpoint_id: requiredString(
      evidence.snapshot_checkpoint_id,
      "evidence.snapshot_checkpoint_id",
    ),
    snapshot_artifact_id: requiredString(
      evidence.snapshot_artifact_id,
      "evidence.snapshot_artifact_id",
    ),
    snapshot_manifest_hash: contentHashHex(
      evidence.snapshot_manifest_hash,
      "evidence.snapshot_manifest_hash",
    ),
    snapshot_payload_hash: contentHashHex(
      evidence.snapshot_payload_hash,
      "evidence.snapshot_payload_hash",
    ),
    observed_at_unix_ms: nonNegativeInteger(
      evidence.observed_at_unix_ms,
      "evidence.observed_at_unix_ms",
    ),
  };
  if (normalized.snapshot_movement_seq <= normalized.request_movement_seq) {
    throw new Error("provider account evidence snapshot does not follow its request");
  }
  return normalized;
}

async function readLedgerMovementAtSeq(movementsPath, seq) {
  return (await readLedgerMovementsAtSeqs(movementsPath, [seq])).get(seq);
}

async function readLedgerMovementsAtSeqs(movementsPath, seqs) {
  const requested = new Set(seqs.map((seq) => nonNegativeInteger(seq, "requested movement seq")));
  if (requested.size !== seqs.length) throw new Error("requested movement sequences repeat");
  const tail = await readCompleteLedgerTail(movementsPath, 0);
  const found = new Map();
  let expectedSeq = null;
  for (let index = 0; index < tail.records.length; index += 1) {
    let movement;
    try {
      movement = JSON.parse(tail.records[index].text);
    } catch (error) {
      throw new Error(`movement ledger contains malformed complete JSON at line ${index + 1}: ${error.message}`);
    }
    const seq = nonNegativeInteger(movement?.seq, "movement seq");
    if (expectedSeq !== null && seq !== expectedSeq) {
      throw new Error(`movement ledger is non-contiguous at seq ${seq}; expected ${expectedSeq}`);
    }
    expectedSeq = seq + 1;
    if (requested.has(seq)) found.set(seq, movement);
    if (found.size === requested.size) break;
  }
  for (const seq of requested) {
    if (!found.has(seq)) throw new Error(`movement ledger has no complete row at seq ${seq}`);
  }
  return found;
}

async function readCompleteLedgerEndOffset(movementsPath) {
  let handle;
  try {
    handle = await fs.open(movementsPath, "r");
  } catch (error) {
    if (error?.code === "ENOENT") return 0;
    throw error;
  }
  try {
    const stat = await handle.stat();
    let end = stat.size;
    while (end > 0) {
      const start = Math.max(0, end - 64 * 1024);
      const bytes = Buffer.alloc(end - start);
      let filled = 0;
      while (filled < bytes.length) {
        const { bytesRead } = await handle.read(
          bytes,
          filled,
          bytes.length - filled,
          start + filled,
        );
        if (bytesRead === 0) {
          throw new Error("movement ledger changed while resolving its complete tail");
        }
        filled += bytesRead;
      }
      const lastNewline = bytes.lastIndexOf(0x0a);
      if (lastNewline >= 0) return start + lastNewline + 1;
      end = start;
    }
    return 0;
  } finally {
    await handle.close();
  }
}

async function* readCompleteLedgerRecords(movementsPath, startOffset) {
  let handle;
  try {
    handle = await fs.open(movementsPath, "r");
  } catch (error) {
    if (error?.code === "ENOENT" && startOffset === 0) return;
    throw error;
  }
  try {
    const stat = await handle.stat();
    if (stat.size < startOffset) throw new Error("movement ledger shrank below the interface cursor");
    if (stat.size === startOffset) return;
    let offset = startOffset;
    let pending = [];
    let pendingLength = 0;
    for (let position = startOffset; position < stat.size;) {
      const buffer = Buffer.allocUnsafe(Math.min(64 * 1024, stat.size - position));
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, position);
      if (bytesRead === 0) throw new Error("movement ledger changed while reading its complete tail");
      position += bytesRead;
      const chunk = buffer.subarray(0, bytesRead);
      let start = 0;
      for (;;) {
        const newline = chunk.indexOf(0x0a, start);
        if (newline < 0) break;
        const fragment = chunk.subarray(start, newline);
        const bytes = pendingLength ? Buffer.concat([...pending, fragment], pendingLength + fragment.length) : fragment;
        offset += bytes.length + 1;
        const text = bytes.subarray(0, bytes.at(-1) === 0x0d ? -1 : undefined).toString("utf8");
        pending = [];
        pendingLength = 0;
        start = newline + 1;
        yield { text, nextOffset: offset };
      }
      if (start < chunk.length) {
        const remainder = chunk.subarray(start);
        pending.push(remainder);
        pendingLength += remainder.length;
      }
    }
  } finally {
    await handle.close();
  }
}

async function readCompleteLedgerTail(movementsPath, startOffset) {
  const records = [];
  let nextOffset = startOffset;
  for await (const record of readCompleteLedgerRecords(movementsPath, startOffset)) {
    nextOffset = record.nextOffset;
    if (record.text.length > 0) records.push(record);
  }
  return { lines: records.map(record => record.text), records, nextOffset };
}

async function readOperatorChatArtifacts(options, request) {
  const config = await readConfig();
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const interfaceId = safeSegment(request?.interfaceId, "interfaceId");
  const runId = safeSegment(request?.runId, "runId");
  const afterSeq = integerAtLeast(request?.afterSeq, -1, "afterSeq");
  const waitMs = boundedPositiveInteger(request?.waitMs, 30_000, 60_000, "waitMs");
  const limit = boundedPositiveInteger(request?.limit, 256, 1_024, "limit");
  const resolveArtifactPayload = requiredFunction(
    options.readArtifactPayload,
    "readArtifactPayload",
  );
  await assertExactOperatorClientInterface(octopusHome, interfaceId);
  const liveContext = await operatorChatLiveContext(
    config,
    octopusHome,
    runId,
    interfaceId,
  );
  const movementsPath = path.join(octopusHome, "ledger", "movements.jsonl");
  let cursor = normalizeLiveCursor(request?.cursor) ?? { offset: 0, seq: null };
  const deadline = Date.now() + waitMs;
  const streamRunRoot = path.join(
    liveContext.streamsRoot,
    streamIdentityHash([liveContext.providerInterfaceId, runId]),
  );
  for (;;) {
    const remaining = Math.max(0, deadline - Date.now());
    const priorStreamCursors = operatorStreamCursors(cursor);
    const ledgerChange = watchLedgerChange(movementsPath, remaining);
    const streamChange = watchLedgerChange(streamRunRoot, remaining);
    let ledgerBatch;
    let openStreamBatch;
    const openStreamLimit = Math.max(1, Math.ceil(limit / 2));
    const ledgerLimit = limit - openStreamLimit;
    try {
      const reads = await Promise.allSettled([
        readOperatorChatArtifactBatch({
          afterSeq,
          cursor,
          interfaceId,
          limit: ledgerLimit,
          movementsPath,
          octopusHome,
          runId,
        }),
        readOpenOperatorTurnStream({
          cursors: priorStreamCursors,
          limit: openStreamLimit,
          liveContext,
          runId,
          runRoot: streamRunRoot,
          waitMs: remaining,
        }),
      ]);
      if (reads[1].status === "fulfilled") openStreamBatch = reads[1].value;
      const failed = reads.find((result) => result.status === "rejected");
      if (failed) throw failed.reason;
      ledgerBatch = reads[0].value;
      if (ledgerLimit === 0 && openStreamBatch.events.length === 0) {
        ledgerBatch = await readOperatorChatArtifactBatch({
          afterSeq,
          cursor,
          interfaceId,
          limit,
          movementsPath,
          octopusHome,
          runId,
        });
      }
    } catch (error) {
      ledgerChange.cancel();
      streamChange.cancel();
      cancelChanges(openStreamBatch?.changes);
      throw error;
    }
    const sealedStreamPending = ledgerBatch.rows.some((row) => (
      artifactLabel(requiredString(row?.artifact?.id, "outbound artifact.id"))
        === LIVE_STREAM_ARTIFACT_LABEL
    ));
    const openStreamEvents = sealedStreamPending ? [] : openStreamBatch.events;
    let streamCursors = sealedStreamPending
      ? priorStreamCursors
      : mergeOperatorStreamCursors(priorStreamCursors, openStreamBatch.cursors);
    let ledgerCursor = ledgerBatch.cursor;
    cursor = withOperatorStreamCursors(ledgerCursor, streamCursors);
    if (ledgerBatch.rows.length > 0 || openStreamBatch.events.length > 0) {
      ledgerChange.cancel();
      streamChange.cancel();
      cancelChanges(openStreamBatch.changes);
      const sourceRoles = new Map(await Promise.all(
        [...new Set(ledgerBatch.rows.map((row) => endpointInterfaceId(row.from)))].map(async (sourceInterfaceId) => (
          [sourceInterfaceId, await readExactInterfaceRole(octopusHome, sourceInterfaceId)]
        )),
      ));
      const movementArtifacts = [];
      const sealedStreamArtifacts = [];
      for (const row of ledgerBatch.rows) {
        const artifactId = requiredString(row?.artifact?.id, "outbound artifact.id");
        const contentHash = contentHashHex(row?.artifact?.content, "outbound artifact.content");
        const label = artifactLabel(artifactId);
        const resolved = await resolveArtifactPayload({
          artifactId,
          manifestHash: contentHash,
          label,
        });
        if (label === LIVE_STREAM_ARTIFACT_LABEL) {
          const parsed = parseOperatorTurnStream(
            requiredBuffer(resolved?.payloadBytes, "sealed turn.stream payloadBytes"),
            {
              complete: true,
              providerInterfaceId: liveContext.providerInterfaceId,
              runId,
            },
          );
          const remainingBudget = Math.max(
            0,
            limit - openStreamEvents.length - movementArtifacts.length - sealedStreamArtifacts.length,
          );
          const selected = selectOperatorTurnStreamEvents(
            parsed,
            streamCursors.get(parsed.streamId) ?? null,
            remainingBudget,
          );
          streamCursors = mergeOperatorStreamCursors(streamCursors, new Map([[parsed.streamId, selected.cursor]]));
          sealedStreamArtifacts.push(...selected.events.map((event) => operatorChatStreamArtifact({
            config,
            event,
            interfaceId,
            providerInterfaceId: liveContext.providerInterfaceId,
            providerRole: liveContext.providerRole,
            runId,
          })));
          const fullyConsumed = selected.cursor.offset === parsed.completeEndOffset
            && selected.cursor.seq === parsed.nextSeq;
          ledgerCursor = fullyConsumed
            ? row.__ledger_cursor_after
            : row.__ledger_cursor_before;
          continue;
        }
        movementArtifacts.push(operatorChatMovementArtifact({
          config,
          contentHash,
          interfaceId,
          label,
          payload: resolved?.payload,
          providerInterfaceId: liveContext.providerInterfaceId,
          role: sourceRoles.get(endpointInterfaceId(row.from)),
          row,
          runId,
        }));
        ledgerCursor = row.__ledger_cursor_after;
      }
      cursor = withOperatorStreamCursors(ledgerCursor, streamCursors);
      const streamArtifacts = deduplicateOperatorTurnStreamArtifacts([
        ...openStreamEvents.map((event) => operatorChatStreamArtifact({
          config,
          event,
          interfaceId,
          providerInterfaceId: liveContext.providerInterfaceId,
          providerRole: liveContext.providerRole,
          runId,
        })),
        ...sealedStreamArtifacts,
      ]);
      const artifacts = [...movementArtifacts, ...streamArtifacts].sort((left, right) => (
        left.recorded_at_unix_ms - right.recorded_at_unix_ms
        || operatorChatArtifactOrder(left) - operatorChatArtifactOrder(right)
        || (left.source === "stream" ? left.stream_seq : 0) - (right.source === "stream" ? right.stream_seq : 0)
      ));
      assertOperatorChatLiveArtifactBatch({ artifacts, cursor });
      return { artifacts, cursor };
    }
    cursor = withOperatorStreamCursors(ledgerBatch.cursor, streamCursors);
    const changed = await Promise.race([
      ledgerChange.promise,
      streamChange.promise,
      ...openStreamBatch.changes.map((change) => change.promise),
    ]);
    ledgerChange.cancel();
    streamChange.cancel();
    cancelChanges(openStreamBatch.changes);
    if (remaining === 0 || !changed) {
      return { artifacts: [], cursor };
    }
  }
}

function operatorChatMovementArtifact({ config, contentHash, interfaceId, label, payload, providerInterfaceId, role, row, runId }) {
  const providerFailure = operatorChatProviderFailureActivity({
    label,
    payload,
    providerInterfaceId,
    sourceInterfaceId: endpointInterfaceId(row.from),
  });
  const activity = providerFailure ?? operatorChatLiveActivityPayload(label, config, payload);
  const eventKind = activity.eventKind;
  const terminalError = label === config.artifact_labels.error_output;
  const willRetry = terminalError && payload?.will_retry === true;
  const eventData = compactOperatorChatLiveEventData(activity.payload);
  delete eventData.continuation;
  if (eventKind === "compaction") delete eventData.text;
  return {
    source: "movement",
    movement_seq: nonNegativeInteger(row.seq, "outbound movement seq"),
    recorded_at_unix_ms: nonNegativeInteger(
      row.recorded_at_unix_ms,
      "outbound movement recorded_at_unix_ms",
    ),
    run_id: runId,
    interface_id: interfaceId,
    checkpoint_id: stringValue(row.runtime?.checkpoint),
    from_interface_id: endpointInterfaceId(row.from),
    from_interface_role: requiredString(role, "outbound movement source interface role"),
    artifact_id: requiredString(row?.artifact?.id, "outbound artifact.id"),
    content_hash: contentHash,
    label,
    event_kind: eventKind,
    event_data: eventData,
    text: activity.text,
    error: Boolean(providerFailure) || terminalError || activity.error || label === "turn.control.failed",
    terminal: Boolean(providerFailure) || operatorChatLiveEventEndsTurn({
      config,
      eventKind,
      label,
      payload,
      willRetry,
    }),
    will_retry: willRetry,
  };
}

function operatorChatProviderFailureActivity({ label, payload, providerInterfaceId, sourceInterfaceId }) {
  if (!providerInterfaceId || sourceInterfaceId !== providerInterfaceId) return null;
  if (label !== `${providerInterfaceId}.failure`) return null;
  const failure = requiredObject(payload, "provider failure payload");
  const failureClass = requiredString(failure.failure_class, "provider failure class");
  nonNegativeInteger(failure.source_movement_seq, "provider failure source_movement_seq");
  const details = requiredObject(failure.details, "provider failure details");
  const text = stringValue(details.error)
    || stringValue(details.message)
    || `Provider failed before completing the turn (${failureClass}).`;
  return {
    eventKind: "error",
    payload: { ...failure, failure_code: failureClass },
    text,
    error: true,
  };
}

function operatorChatStreamArtifact({ config, event, interfaceId, providerInterfaceId, providerRole, runId }) {
  const inputRecorded = event.type === "input_recorded";
  const activity = inputRecorded
    ? { eventKind: "input_recorded", payload: event.payload, text: "", error: false }
    : operatorChatLiveActivityPayload(event.label, config, event.payload);
  const eventData = inputRecorded ? {
    ...activity.payload,
    _stream: {
      stream_id: event.streamId,
      stream_seq: event.seq,
      byte_start_offset: event.startOffset,
      byte_end_offset: event.endOffset,
      recorded_at_unix_ms: event.recordedAtUnixMs,
    },
  } : compactOperatorChatLiveEventData(activity.payload);
  delete eventData.continuation;
  if (activity.eventKind === "compaction") delete eventData.text;
  return {
    source: "stream",
    stream_id: event.streamId,
    stream_seq: event.seq,
    byte_start_offset: event.startOffset,
    byte_end_offset: event.endOffset,
    source_movement_seq: event.sourceMovementSeq,
    recorded_at_unix_ms: event.recordedAtUnixMs,
    run_id: runId,
    interface_id: interfaceId,
    from_interface_id: providerInterfaceId,
    from_interface_role: providerRole,
    label: inputRecorded ? LIVE_STREAM_ARTIFACT_LABEL : event.label,
    event_kind: activity.eventKind,
    event_data: eventData,
    text: activity.text,
    error: activity.error,
    terminal: false,
    will_retry: false,
  };
}

function operatorChatArtifactOrder(artifact) {
  return artifact.source === "movement" ? artifact.movement_seq : artifact.source_movement_seq;
}

function deduplicateOperatorTurnStreamArtifacts(artifacts) {
  const seen = new Set();
  return artifacts.filter((artifact) => {
    const key = `${artifact.stream_id}\0${artifact.stream_seq}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

async function readOpenOperatorTurnStream(input) {
  const runRoot = input.runRoot;
  let entries;
  try {
    entries = await fs.readdir(runRoot, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") return { changes: [], cursors: input.cursors, events: [] };
    throw error;
  }
  const candidates = [];
  try {
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const turnRoot = path.join(runRoot, entry.name);
      const manifestPath = path.join(turnRoot, "turn-stream.artifact.json");
      let manifestBytes;
      try {
        manifestBytes = await readRegularFile(
          manifestPath,
          LIVE_STREAM_MAX_MANIFEST_BYTES,
          "open turn.stream manifest",
        );
      } catch (error) {
        if (error?.code === "ENOENT") continue;
        throw error;
      }
      let manifest;
      try {
        manifest = JSON.parse(manifestBytes.toString("utf8"));
      } catch (error) {
        throw new Error(`open turn.stream manifest is malformed JSON: ${error.message}`);
      }
      requiredObject(manifest, "open turn.stream manifest");
      if (requiredString(manifest.label, "open turn.stream manifest label") !== LIVE_STREAM_ARTIFACT_LABEL) {
        throw new Error("open turn stream run directory contains a non-turn.stream manifest");
      }
      if (manifest.payload_content_hash !== undefined && manifest.payload_content_hash !== null) {
        contentHashHex(manifest.payload_content_hash, "sealed turn.stream manifest payload_content_hash");
        continue;
      }
      const bodyPath = absolutePath(manifest.payload_path, "open turn.stream payload_path");
      const expectedBodyPath = path.join(turnRoot, "turn-stream.body.jsonl");
      if (bodyPath !== expectedBodyPath) {
        throw new Error("open turn.stream manifest points outside its exact turn directory");
      }
      const changes = [
        watchLedgerChange(bodyPath, input.waitMs),
        watchLedgerChange(manifestPath, input.waitMs),
      ];
      try {
        const parsed = await readOpenOperatorTurnStreamTail(bodyPath, input.cursors, {
          providerInterfaceId: input.liveContext.providerInterfaceId,
          runId: input.runId,
        });
        if (entry.name !== streamIdentityHash([parsed.header.threadId, parsed.header.turnId])) {
          throw new Error("open turn.stream directory differs from its header identity");
        }
        candidates.push({ parsed, changes });
      } catch (error) {
        cancelChanges(changes);
        throw error;
      }
    }
  } catch (error) {
    for (const candidate of candidates) cancelChanges(candidate.changes);
    throw error;
  }
  const events = candidates.flatMap(({ parsed }) => selectOperatorTurnStreamEvents(
    parsed, input.cursors.get(parsed.streamId) ?? null, input.limit,
  ).events).sort((left, right) => left.recordedAtUnixMs - right.recordedAtUnixMs
    || left.sourceMovementSeq - right.sourceMovementSeq
    || left.streamId.localeCompare(right.streamId) || left.seq - right.seq).slice(0, input.limit);
  const counts = new Map();
  for (const event of events) counts.set(event.streamId, (counts.get(event.streamId) ?? 0) + 1);
  const cursors = new Map(input.cursors);
  for (const { parsed } of candidates) {
    const selected = selectOperatorTurnStreamEvents(parsed, cursors.get(parsed.streamId) ?? null, counts.get(parsed.streamId) ?? 0);
    cursors.set(parsed.streamId, selected.cursor);
  }
  return { changes: candidates.flatMap((candidate) => candidate.changes), cursors, events };
}

function cancelChanges(changes) {
  for (const change of changes ?? []) change.cancel();
}

async function readRegularFile(filePath, maximumBytes, field) {
  const stat = await fs.lstat(filePath);
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${field} is not a regular file`);
  if (stat.size > maximumBytes) throw new Error(`${field} exceeds ${maximumBytes} bytes`);
  const handle = await fs.open(filePath, "r");
  try {
    const before = await handle.stat();
    const bytes = Buffer.alloc(before.size);
    let filled = 0;
    while (filled < bytes.byteLength) {
      const { bytesRead } = await handle.read(bytes, filled, bytes.byteLength - filled, filled);
      if (bytesRead === 0) throw new Error(`${field} changed while being read`);
      filled += bytesRead;
    }
    const after = await handle.stat();
    if (after.size < before.size) {
      throw new Error(`${field} changed incompatibly while being read`);
    }
    return bytes;
  } finally {
    await handle.close();
  }
}

async function readOpenOperatorTurnStreamTail(filePath, cursor, expected) {
  const stat = await fs.lstat(filePath);
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error("open turn.stream body is not a regular file");
  if (stat.size > LIVE_STREAM_MAX_BYTES) {
    throw new Error(`open turn.stream body exceeds ${LIVE_STREAM_MAX_BYTES} bytes`);
  }
  const handle = await fs.open(filePath, "r");
  try {
    const before = await handle.stat();
    const headerBytes = await readOperatorTurnStreamHeaderBytes(handle, before.size);
    const headerRecord = parseOperatorTurnStreamRecords(headerBytes, 0, true)[0];
    const header = parseOperatorTurnStreamHeader(headerRecord, expected);
    cursor = cursor instanceof Map ? cursor.get(header.streamId) ?? null : cursor;
    let startOffset = headerRecord.endOffset;
    let state = {
      expectedSeq: 1,
      previousTime: header.openedAtUnixMs,
      previousSourceMovementSeq: header.openedSourceMovementSeq,
      sealed: false,
    };
    if (cursor?.stream_id === header.streamId) {
      startOffset = nonNegativeInteger(cursor.offset, "turn.stream cursor offset");
      if (startOffset < headerRecord.endOffset || startOffset > before.size) {
        throw new Error("turn.stream body shrank below its cursor");
      }
      state = {
        expectedSeq: nonNegativeInteger(cursor.seq, "turn.stream cursor seq"),
        previousTime: positiveInteger(
          cursor.recorded_at_unix_ms,
          "turn.stream cursor recorded_at_unix_ms",
        ),
        previousSourceMovementSeq: nonNegativeInteger(
          cursor.source_movement_seq,
          "turn.stream cursor source_movement_seq",
        ),
        sealed: cursor.sealed === true,
      };
    }
    const tailBytes = await readOperatorTurnStreamRange(handle, startOffset, before.size);
    const lastNewline = tailBytes.lastIndexOf(0x0a);
    const completeTail = lastNewline < 0 ? Buffer.alloc(0) : tailBytes.subarray(0, lastNewline + 1);
    const records = parseOperatorTurnStreamRecords(completeTail, startOffset, false);
    const rows = [];
    for (const record of records) {
      const validated = validateOperatorTurnStreamRow(record.value, record, header, state);
      rows.push(validated.row);
      state = validated.state;
    }
    const after = await handle.stat();
    if (after.size < before.size) throw new Error("open turn.stream body changed incompatibly while being read");
    return {
      header,
      headerEndOffset: headerRecord.endOffset,
      rows,
      streamId: header.streamId,
      nextSeq: state.expectedSeq,
      completeEndOffset: startOffset + completeTail.byteLength,
      tailStartOffset: startOffset,
      tailStartSeq: cursor?.stream_id === header.streamId ? state.expectedSeq - rows.length : 1,
      readBytes: headerBytes.byteLength + tailBytes.byteLength,
    };
  } finally {
    await handle.close();
  }
}

async function readOperatorTurnStreamHeaderBytes(handle, size) {
  const length = Math.min(size, LIVE_STREAM_MAX_HEADER_BYTES);
  if (length === 0) throw new Error("turn.stream body has no complete header row");
  const bytes = await readOperatorTurnStreamRange(handle, 0, length);
  const newline = bytes.indexOf(0x0a);
  if (newline < 0) throw new Error("turn.stream header exceeds its bounded complete row");
  return bytes.subarray(0, newline + 1);
}

async function readOperatorTurnStreamRange(handle, startOffset, endOffset) {
  const length = endOffset - startOffset;
  const bytes = Buffer.alloc(length);
  let filled = 0;
  while (filled < length) {
    const { bytesRead } = await handle.read(bytes, filled, length - filled, startOffset + filled);
    if (bytesRead === 0) throw new Error("turn.stream body changed while being read");
    filled += bytesRead;
  }
  return bytes;
}

function parseOperatorTurnStreamRecords(bytes, baseOffset, requireRecord) {
  const records = [];
  let startOffset = 0;
  for (let index = 0; index < bytes.byteLength; index += 1) {
    if (bytes[index] !== 0x0a) continue;
    const end = index > startOffset && bytes[index - 1] === 0x0d ? index - 1 : index;
    const raw = bytes.subarray(startOffset, end);
    if (raw.byteLength === 0) throw new Error("turn.stream body contains an empty complete row");
    let value;
    try {
      value = JSON.parse(raw.toString("utf8"));
    } catch (error) {
      throw new Error(`turn.stream body contains malformed complete JSON: ${error.message}`);
    }
    records.push({
      value: requiredObject(value, "turn.stream row"),
      startOffset: baseOffset + startOffset,
      endOffset: baseOffset + index + 1,
    });
    startOffset = index + 1;
  }
  if (requireRecord && records.length !== 1) throw new Error("turn.stream body has no exact header row");
  return records;
}

function parseOperatorTurnStreamHeader(headerRecord, expected) {
  const header = headerRecord?.value;
  if (
    header?.type !== "header"
    || header.schema_version !== LIVE_STREAM_SCHEMA_VERSION
    || header.seq !== 0
    || requiredString(header.provider_interface, "turn.stream header provider_interface") !== expected.providerInterfaceId
    || requiredString(header.run, "turn.stream header run") !== expected.runId
  ) {
    throw new Error("turn.stream header has the wrong provider/run identity");
  }
  const threadId = requiredString(header.thread_id, "turn.stream header thread_id");
  const turnId = requiredString(header.turn_id, "turn.stream header turn_id");
  const openedSourceMovementSeq = nonNegativeInteger(
    header.opened_source_movement_seq,
    "turn.stream header opened_source_movement_seq",
  );
  const openedAtUnixMs = positiveInteger(header.opened_at_unix_ms, "turn.stream header opened_at_unix_ms");
  return {
    providerInterfaceId: expected.providerInterfaceId,
    runId: expected.runId,
    threadId,
    turnId,
    openedAtUnixMs,
    openedSourceMovementSeq,
    streamId: streamIdentityHash([expected.providerInterfaceId, expected.runId, threadId, turnId]),
  };
}

function parseOperatorTurnStream(bytes, expected) {
  if (!Buffer.isBuffer(bytes) || bytes.byteLength === 0) {
    throw new Error("turn.stream body has no complete header row");
  }
  if (bytes.byteLength > LIVE_STREAM_MAX_BYTES) {
    throw new Error(`turn.stream body exceeds ${LIVE_STREAM_MAX_BYTES} bytes`);
  }
  if (expected.complete && bytes.at(-1) !== 0x0a) {
    throw new Error("sealed turn.stream body ends with an incomplete row");
  }
  const records = parseOperatorTurnStreamRecords(bytes, 0, false);
  const headerRecord = records.shift();
  if (!headerRecord) throw new Error("turn.stream body has no header row");
  const header = parseOperatorTurnStreamHeader(headerRecord, expected);
  const streamId = header.streamId;
  const rows = [];
  let state = {
    expectedSeq: 1,
    previousTime: header.openedAtUnixMs,
    previousSourceMovementSeq: header.openedSourceMovementSeq,
    sealed: false,
  };
  for (const record of records) {
    const validated = validateOperatorTurnStreamRow(record.value, record, header, state);
    rows.push(validated.row);
    state = validated.state;
  }
  if (expected.complete && !state.sealed) throw new Error("sealed turn.stream body has no seal row");
  return {
    header,
    headerEndOffset: headerRecord.endOffset,
    rows,
    streamId,
    nextSeq: state.expectedSeq,
    completeEndOffset: bytes.byteLength,
    tailStartOffset: headerRecord.endOffset,
    tailStartSeq: 1,
  };
}

function validateOperatorTurnStreamRow(row, record, header, state) {
  const seq = nonNegativeInteger(row.seq, "turn.stream row seq");
  if (seq !== state.expectedSeq) {
    throw new Error(`turn.stream row sequence ${seq} differs from expected ${state.expectedSeq}`);
  }
  const recordedAtUnixMs = positiveInteger(
    row.recorded_at_unix_ms,
    "turn.stream row recorded_at_unix_ms",
  );
  if (recordedAtUnixMs < state.previousTime) throw new Error("turn.stream row time moved backwards");
  const sourceMovementSeq = nonNegativeInteger(
    row.source_movement_seq,
    "turn.stream row source_movement_seq",
  );
  if (sourceMovementSeq < state.previousSourceMovementSeq) {
    throw new Error("turn.stream row source_movement_seq moved backwards");
  }
  const type = requiredString(row.type, "turn.stream row type");
  if (state.sealed) throw new Error("turn.stream contains data after its seal row");
  let sealed = false;
  if (type === "seal") {
    requiredString(row.terminal_label, "turn.stream seal terminal_label");
    contentHashHex(
      row.terminal_artifact_payload_blake3,
      "turn.stream seal terminal_artifact_payload_blake3",
    );
    sealed = true;
  } else if (type === "semantic_boundary") {
    requiredString(row.label, "turn.stream semantic boundary label");
    contentHashHex(row.artifact_payload_blake3, "turn.stream semantic boundary artifact_payload_blake3");
  } else if (type === "recovery") {
    positiveInteger(row.discarded_byte_count, "turn.stream recovery discarded_byte_count");
  } else if (type === "input_recorded") {
    validateOperatorTurnInputRecorded(row, header);
  } else if (type !== "event") {
    throw new Error(`turn.stream row has unknown type '${type}'`);
  }
  return {
    row: {
      type,
      seq,
      streamId: header.streamId,
      startOffset: record.startOffset,
      endOffset: record.endOffset,
      sourceMovementSeq,
      recordedAtUnixMs,
      sealedAfter: sealed,
      ...(type === "event" ? {
        label: requiredString(row.label, "turn.stream event label"),
        payload: row.payload,
      } : {}),
      ...(type === "input_recorded" ? { payload: { ...row } } : {}),
    },
    state: {
      expectedSeq: seq + 1,
      previousTime: recordedAtUnixMs,
      previousSourceMovementSeq: sourceMovementSeq,
      sealed,
    },
  };
}

function validateOperatorTurnInputRecorded(row, header) {
  const fields = new Set([
    "type", "seq", "recorded_at_unix_ms", "source_movement_seq",
    "provider_interface", "run", "thread_id", "turn_id", "item_id", "client_id",
    "stage", "source_event", "native_event_at_ms",
  ]);
  for (const field of Object.keys(row)) {
    if (!fields.has(field)) throw new Error(`turn.stream input_recorded has unknown field '${field}'`);
  }
  for (const field of ["provider_interface", "run", "thread_id", "turn_id", "item_id", "client_id"]) {
    const value = requiredTrimmedString(row[field], `turn.stream input_recorded ${field}`);
    if (/[\r\n\0]/.test(value)) throw new Error(`turn.stream input_recorded has invalid ${field}`);
  }
  if (
    row.provider_interface !== header.providerInterfaceId || row.run !== header.runId
    || row.thread_id !== header.threadId || row.turn_id !== header.turnId
    || row.stage !== "input_recorded_in_turn"
  ) throw new Error("turn.stream input_recorded differs from its turn identity or stage");
  if (row.source_event === "item/started" || row.source_event === "item/completed") {
    nonNegativeInteger(row.native_event_at_ms, "turn.stream input_recorded native_event_at_ms");
  } else if (row.source_event === "thread/resume") {
    if (Object.hasOwn(row, "native_event_at_ms")) {
      throw new Error("turn.stream history input_recorded must omit native event timing");
    }
  } else {
    throw new Error("turn.stream input_recorded has an invalid source_event");
  }
}

function selectOperatorTurnStreamEvents(parsed, cursor, limit = Number.MAX_SAFE_INTEGER) {
  let offset = parsed.headerEndOffset;
  let seq = 1;
  let recordedAtUnixMs = parsed.header.openedAtUnixMs;
  let sourceMovementSeq = parsed.header.openedSourceMovementSeq;
  let sealed = false;
  if (cursor?.stream_id === parsed.streamId) {
    offset = nonNegativeInteger(cursor.offset, "turn.stream cursor offset");
    seq = nonNegativeInteger(cursor.seq, "turn.stream cursor seq");
    if (offset > parsed.completeEndOffset) throw new Error("turn.stream body shrank below its cursor");
    recordedAtUnixMs = positiveInteger(cursor.recorded_at_unix_ms, "turn.stream cursor recorded_at_unix_ms");
    sourceMovementSeq = nonNegativeInteger(cursor.source_movement_seq, "turn.stream cursor source_movement_seq");
    sealed = cursor.sealed === true;
    const boundary = offset === parsed.tailStartOffset && seq === parsed.tailStartSeq
      || offset === parsed.headerEndOffset && seq === 1
      || parsed.rows.some((row) => row.endOffset === offset && row.seq + 1 === seq)
      || offset === parsed.completeEndOffset && seq === parsed.nextSeq;
    if (!boundary) throw new Error("turn.stream cursor does not align to a complete row boundary");
  }
  const events = [];
  for (const row of parsed.rows) {
    if (row.seq < seq) continue;
    if (row.seq !== seq || row.startOffset !== offset) {
      throw new Error("turn.stream cursor encountered a sequence or byte-offset gap");
    }
    const projectsEvent = row.type === "event" || row.type === "input_recorded";
    if (projectsEvent && events.length >= limit) break;
    offset = row.endOffset;
    seq = row.seq + 1;
    recordedAtUnixMs = row.recordedAtUnixMs;
    sourceMovementSeq = row.sourceMovementSeq;
    sealed = row.sealedAfter;
    if (projectsEvent) events.push(row);
    if (events.length >= limit) break;
  }
  return {
    cursor: {
      stream_id: parsed.streamId,
      offset,
      seq,
      recorded_at_unix_ms: recordedAtUnixMs,
      source_movement_seq: sourceMovementSeq,
      ...(sealed ? { sealed: true } : {}),
    },
    events,
  };
}

function laterStreamCursor(current, candidate) {
  if (!current) return candidate;
  if (!candidate) return current;
  if (current.stream_id !== candidate.stream_id) return candidate;
  if (candidate.seq !== current.seq) return candidate.seq > current.seq ? candidate : current;
  return candidate.offset > current.offset ? candidate : current;
}

function streamIdentityHash(parts) {
  const hash = crypto.createHash("sha256");
  for (const part of parts) {
    const value = requiredString(part, "turn.stream identity part");
    const bytes = Buffer.from(value, "utf8");
    const length = Buffer.alloc(8);
    length.writeBigUInt64LE(BigInt(bytes.byteLength));
    hash.update(length);
    hash.update(bytes);
  }
  return hash.digest("hex");
}

function requiredBuffer(value, field) {
  if (!Buffer.isBuffer(value)) throw new Error(`${field} must be a Buffer`);
  return value;
}

function assertOperatorChatLiveArtifactBatch(batch) {
  if (!isObject(batch) || !Array.isArray(batch.artifacts)) {
    throw new Error("operator chat live artifact batch is invalid");
  }
  for (const artifact of batch.artifacts) {
    if (!isObject(artifact)) throw new Error("operator chat live artifact is invalid");
    requiredString(artifact.from_interface_id, "operator chat live artifact from_interface_id");
    requiredString(artifact.from_interface_role, "operator chat live artifact from_interface_role");
  }
  return batch;
}

async function readExactInterfaceRole(octopusHome, interfaceId) {
  const descriptor = await readJsonIfPresent(path.join(
    octopusHome,
    "registry",
    "interface",
    safeSegment(interfaceId, "source interface id"),
    "interface.json",
  ));
  if (!isObject(descriptor) || descriptor.id !== interfaceId) {
    throw new Error(`source interface '${interfaceId}' has no exact descriptor`);
  }
  return requiredString(descriptor.role, `source interface '${interfaceId}' role`).toLowerCase();
}

async function assertExactOperatorClientInterface(octopusHome, interfaceId) {
  const interfaceRoot = path.join(
    octopusHome,
    "registry",
    "interface",
    interfaceId,
  );
  const descriptor = await readJsonIfPresent(path.join(interfaceRoot, "interface.json"));
  const sidecar = await readJsonIfPresent(path.join(interfaceRoot, "octopus_os.json"));
  if (
    !isObject(descriptor)
    || descriptor.id !== interfaceId
    || stringValue(descriptor.interface_type).toLowerCase() !== "world"
    || stringValue(descriptor.role).toLowerCase() !== "user"
    || !arrayValue(descriptor.source_names).includes("operator.chat")
  ) {
    throw new Error(`interface '${interfaceId}' is not an exact operator chat interface`);
  }
  if (
    !isObject(sidecar)
    || sidecar.interface !== interfaceId
    || sidecar.owner_interface !== interfaceId
    || sidecar.component !== "client"
    || sidecar.workspace !== "octopus_os"
  ) {
    throw new Error(`interface '${interfaceId}' has no exact Octopus OS client ownership`);
  }
}

async function readOperatorChatArtifactBatch(input) {
  const requestedCursor = input.cursor;
  if (requestedCursor) {
    return readLedgerOperatorRows({
      afterSeq: input.afterSeq,
      cursor: requestedCursor,
      interfaceId: input.interfaceId,
      limit: input.limit,
      movementsPath: input.movementsPath,
      runId: input.runId,
    });
  }
  const indexed = await readSpotlightOperatorRows(input);
  if (!indexed) {
    return readLedgerOperatorRows({
      afterSeq: input.afterSeq,
      cursor: { offset: 0, seq: null },
      interfaceId: input.interfaceId,
      limit: input.limit,
      movementsPath: input.movementsPath,
      runId: input.runId,
    });
  }
  const tail = await readLedgerOperatorRows({
    afterSeq: input.afterSeq,
    cursor: indexed.cursor,
    interfaceId: input.interfaceId,
    limit: Math.max(0, input.limit - indexed.rows.length),
    movementsPath: input.movementsPath,
    runId: input.runId,
  });
  const rows = [...indexed.rows, ...tail.rows]
    .sort((left, right) => left.seq - right.seq)
    .filter((row, index, values) => index === 0 || row.seq !== values[index - 1].seq)
    .slice(0, input.limit);
  return { rows, cursor: tail.cursor };
}

async function readSpotlightOperatorRows(input) {
  let DatabaseSync;
  try {
    ({ DatabaseSync } = require("node:sqlite"));
  } catch {
    return null;
  }
  const indexPath = path.join(input.octopusHome, "ledger", "ledger-index.sqlite");
  let database;
  try {
    database = new DatabaseSync(indexPath, { readOnly: true });
    database.exec("PRAGMA query_only = ON");
    const meta = database.prepare(
      "SELECT key, value FROM index_meta WHERE key IN ('schema_version', 'watermark', 'durable_end_offset')",
    ).all();
    const values = new Map(meta.map((row) => [String(row.key), String(row.value)]));
    if (values.get("schema_version") !== "3") return null;
    const watermark = decimalSafeInteger(values.get("watermark"), "ledger index watermark");
    const durableEndOffset = decimalSafeInteger(
      values.get("durable_end_offset"),
      "ledger index durable_end_offset",
    );
    const ledgerStat = await fs.stat(input.movementsPath);
    if (durableEndOffset > ledgerStat.size) return null;
    const rows = database.prepare(
      `SELECT
        seq, recorded_at_unix_ms, from_kind, from_id, to_kind, to_id,
        artifact_id, artifact_label, artifact_content,
        runtime_run, runtime_checkpoint, runtime_interface
      FROM movements
      WHERE seq > ?
        AND seq < ?
        AND kind = 'InterfaceOutbound'
        AND from_kind = 'interface'
        AND to_kind = 'interface'
        AND to_id = ?
        AND runtime_interface = ?
        AND runtime_run = ?
      ORDER BY seq ASC
      LIMIT ?`,
    ).all(
      input.afterSeq,
      watermark,
      input.interfaceId,
      input.interfaceId,
      input.runId,
      input.limit + 1,
    ).map((row) => normalizeIndexedOperatorRow(row, input.interfaceId, input.runId));
    if (rows.length > input.limit) return null;
    return {
      rows,
      cursor: {
        offset: durableEndOffset,
        seq: watermark,
      },
    };
  } catch {
    return null;
  } finally {
    database?.close();
  }
}

function normalizeIndexedOperatorRow(row, interfaceId, runId) {
  const seq = databaseInteger(row.seq, "indexed movement seq");
  const recordedAt = databaseInteger(
    row.recorded_at_unix_ms,
    "indexed movement recorded_at_unix_ms",
  );
  const artifactId = requiredString(row.artifact_id, "indexed movement artifact_id");
  const label = requiredString(
    row.artifact_label || artifactLabel(artifactId),
    "indexed movement artifact_label",
  );
  const content = contentHashHex(
    row.artifact_content,
    "indexed movement artifact_content",
  );
  if (
    row.from_kind !== "interface"
    || row.to_kind !== "interface"
    || row.to_id !== interfaceId
    || row.runtime_interface !== interfaceId
    || row.runtime_run !== runId
  ) {
    throw new Error(`indexed outbound movement ${seq} does not match its exact interface coordinates`);
  }
  return {
    seq,
    recorded_at_unix_ms: recordedAt,
    from: { Interface: requiredString(row.from_id, "indexed movement from_id") },
    to: { Interface: interfaceId },
    artifact: {
      id: artifactId,
      content,
    },
    runtime: {
      run: runId,
      checkpoint: requiredString(
        row.runtime_checkpoint,
        "indexed movement runtime_checkpoint",
      ),
      interface: interfaceId,
    },
    label,
  };
}

async function readLedgerOperatorRows(input) {
  if (input.limit === 0) return { rows: [], cursor: input.cursor };
  let expectedSeq = input.cursor.seq;
  let cursor = input.cursor;
  const rows = [];
  let index = 0;
  for await (const record of readCompleteLedgerRecords(input.movementsPath, input.cursor.offset)) {
    if (!record.text.length) continue;
    index += 1;
    const cursorBefore = cursor;
    let movement;
    try {
      movement = JSON.parse(record.text);
    } catch (error) {
      throw new Error(`movement ledger contains malformed complete JSON at tail line ${index}: ${error.message}`);
    }
    const seq = nonNegativeInteger(movement?.seq, "movement seq");
    if (expectedSeq !== null && seq !== expectedSeq) {
      throw new Error(`movement ledger is non-contiguous at seq ${seq}; expected ${expectedSeq}`);
    }
    expectedSeq = seq + 1;
    cursor = {
      offset: record.nextOffset,
      seq: expectedSeq,
    };
    if (seq <= input.afterSeq) continue;
    const runtime = exactOutboundRuntime(movement, input.interfaceId);
    if (!runtime || runtime.run !== input.runId) continue;
    if (!isObject(movement.artifact)) {
      throw new Error(`outbound movement ${seq} carries no artifact`);
    }
    rows.push({
      ...movement,
      runtime,
      __ledger_cursor_before: cursorBefore,
      __ledger_cursor_after: cursor,
    });
    if (artifactLabel(requiredString(movement.artifact.id, "outbound artifact.id")) === LIVE_STREAM_ARTIFACT_LABEL) break;
    if (rows.length >= input.limit) break;
  }
  return {
    rows,
    cursor,
  };
}

function watchLedgerChange(movementsPath, waitMs) {
  let hub = ledgerChangeHubs.get(movementsPath);
  if (!hub) {
    hub = createLedgerChangeHub(movementsPath);
    ledgerChangeHubs.set(movementsPath, hub);
  }
  let settled = false;
  let timer = null;
  let resolvePromise;
  const promise = new Promise((resolve) => {
    resolvePromise = resolve;
  });
  const finish = (value) => {
    if (settled) return;
    settled = true;
    if (timer) clearTimeout(timer);
    hub.waiters.delete(finish);
    if (hub.waiters.size === 0) closeLedgerChangeHub(movementsPath, hub);
    resolvePromise(value);
  };
  hub.waiters.add(finish);
  timer = setTimeout(() => finish(false), waitMs);
  return {
    promise,
    cancel: () => finish(false),
  };
}

function createLedgerChangeHub(movementsPath) {
  const hub = {
    closed: false,
    statListener: null,
    statWatcher: null,
    waiters: new Set(),
    watcher: null,
  };
  const wake = () => {
    if (hub.closed) return;
    const waiters = [...hub.waiters];
    closeLedgerChangeHub(movementsPath, hub);
    for (const finish of waiters) finish(true);
  };
  hub.statListener = (current, previous) => {
    if (
      current.dev === previous.dev
      && current.ino === previous.ino
      && current.size === previous.size
      && current.mtimeMs === previous.mtimeMs
      && current.nlink === previous.nlink
    ) return;
    wake();
  };
  hub.statWatcher = fsSync.watchFile(
    movementsPath,
    { interval: 250, persistent: false },
    hub.statListener,
  );
  try {
    const watcher = fsSync.watch(movementsPath, { persistent: false }, wake);
    hub.watcher = watcher;
    watcher.once("error", () => {
      try {
        watcher.close();
      } catch {
      }
      if (hub.watcher === watcher) hub.watcher = null;
    });
  } catch {
  }
  return hub;
}

function closeLedgerChangeHub(movementsPath, hub) {
  if (ledgerChangeHubs.get(movementsPath) === hub) {
    ledgerChangeHubs.delete(movementsPath);
  }
  if (hub.closed) return;
  hub.closed = true;
  try {
    hub.watcher?.close();
  } catch {
  }
  hub.watcher = null;
  if (hub.statListener) {
    fsSync.unwatchFile(movementsPath, hub.statListener);
    hub.statListener = null;
  }
  hub.statWatcher = null;
}

async function readPersistedLiveCursor(octopusHome, interfaceId) {
  const cursorPath = liveCursorPath(octopusHome, interfaceId);
  const document = await readJsonIfPresent(cursorPath);
  if (document === null) return null;
  return normalizeLiveCursor(document);
}

function persistLiveCursor(octopusHome, interfaceId, cursor) {
  if (!cursor || cursor.seq === null) return Promise.resolve();
  const cursorPath = liveCursorPath(octopusHome, interfaceId);
  const previous = liveCursorWriteTails.get(cursorPath) ?? Promise.resolve();
  const write = previous.then(async () => {
    const current = await readPersistedLiveCursor(octopusHome, interfaceId);
    const next = current ? mergeLiveCursors(current, cursor) : cursor;
    if (current && !liveCursorIsAhead(next, current)) return;
    await fs.mkdir(path.dirname(cursorPath), { recursive: true, mode: 0o700 });
    const temporary = path.join(
      path.dirname(cursorPath),
      `.${path.basename(cursorPath)}.${process.pid}.${randomToken(16)}.tmp`,
    );
    let handle;
    try {
      handle = await fs.open(temporary, "wx", 0o600);
      await handle.writeFile(`${JSON.stringify(next, null, 2)}\n`, "utf8");
      await handle.sync();
      await handle.close();
      handle = null;
      await fs.rename(temporary, cursorPath);
      await fs.chmod(cursorPath, 0o600);
      await syncDirectory(path.dirname(cursorPath));
    } finally {
      await handle?.close().catch(() => {});
      await fs.unlink(temporary).catch((error) => {
        if (error?.code !== "ENOENT") throw error;
      });
    }
  });
  liveCursorWriteTails.set(cursorPath, write.then(() => undefined, () => undefined));
  return write;
}

function rememberLiveCursor(key, cursor) {
  const current = liveLedgerCursors.get(key);
  liveLedgerCursors.set(key, current ? mergeLiveCursors(current, cursor) : cursor);
}

function operatorStreamCursors(cursor) {
  const cursors = new Map(Object.entries(cursor?.streams ?? {}));
  if (cursor?.stream) cursors.set(cursor.stream.stream_id,
    laterStreamCursor(cursors.get(cursor.stream.stream_id), cursor.stream));
  return cursors;
}

function mergeOperatorStreamCursors(current, candidate) {
  const merged = new Map(current);
  for (const [id, cursor] of candidate) merged.set(id, laterStreamCursor(merged.get(id), cursor));
  return merged;
}

function withOperatorStreamCursors(ledger, cursors) {
  const entries = [...cursors.entries()];
  return { offset: ledger.offset, seq: ledger.seq,
    ...(entries.length === 1 ? { stream: entries[0][1] } : {}),
    ...(entries.length > 1 ? { streams: Object.fromEntries(entries) } : {}),
  };
}

function liveCursorIsAhead(candidate, current) {
  if (candidate.seq !== null && current.seq !== null && candidate.seq !== current.seq) {
    return candidate.seq > current.seq;
  }
  if (candidate.offset !== current.offset) return candidate.offset > current.offset;
  const prior = operatorStreamCursors(current);
  return [...operatorStreamCursors(candidate)].some(([id, cursor]) => {
    const before = prior.get(id);
    return !before || cursor.seq > before.seq || cursor.seq === before.seq && cursor.offset > before.offset;
  });
}

function mergeLiveCursors(current, candidate) {
  const ledger = liveCursorIsAhead(
    { offset: candidate.offset, seq: candidate.seq },
    { offset: current.offset, seq: current.seq },
  ) ? candidate : current;
  return withOperatorStreamCursors(ledger, mergeOperatorStreamCursors(
    operatorStreamCursors(current), operatorStreamCursors(candidate),
  ));
}

function liveCursorPath(octopusHome, interfaceId) {
  return path.join(
    octopusHome,
    "interfaces",
    interfaceId,
    "ledger-cursor.client.json",
  );
}

function normalizeLiveCursor(value) {
  if (value === undefined || value === null) return null;
  if (!isObject(value)) throw new Error("operator chat cursor must be an object");
  const unknown = Object.keys(value).filter((key) => !["offset", "seq", "stream", "streams"].includes(key));
  if (unknown.length > 0) {
    throw new Error(`operator chat cursor contains unknown fields: ${unknown.join(", ")}`);
  }
  const offset = nonNegativeInteger(value.offset, "operator chat cursor offset");
  const seq = value.seq === null || value.seq === undefined
    ? null
    : nonNegativeInteger(value.seq, "operator chat cursor seq");
  const normalizeStream = (input) => {
    const row = requiredObject(input, "operator chat stream cursor");
    const streamUnknown = Object.keys(row).filter((key) => ![
      "stream_id",
      "offset",
      "seq",
      "recorded_at_unix_ms",
      "source_movement_seq",
      "sealed",
    ].includes(key));
    if (streamUnknown.length > 0) {
      throw new Error(`operator chat stream cursor contains unknown fields: ${streamUnknown.join(", ")}`);
    }
    return {
      stream_id: contentHashHex(row.stream_id, "operator chat stream cursor stream_id"),
      offset: nonNegativeInteger(row.offset, "operator chat stream cursor offset"),
      seq: nonNegativeInteger(row.seq, "operator chat stream cursor seq"),
      recorded_at_unix_ms: positiveInteger(
        row.recorded_at_unix_ms,
        "operator chat stream cursor recorded_at_unix_ms",
      ),
      source_movement_seq: nonNegativeInteger(
        row.source_movement_seq,
        "operator chat stream cursor source_movement_seq",
      ),
      ...(row.sealed === true ? { sealed: true } : {}),
    };
  };
  const cursors = new Map();
  if (value.streams !== undefined) {
    for (const [id, input] of Object.entries(requiredObject(value.streams, "operator chat stream cursors"))) {
      const cursor = normalizeStream(input);
      if (id !== cursor.stream_id) throw new Error("operator chat stream cursor key differs from its identity");
      cursors.set(id, cursor);
    }
  }
  if (value.stream !== undefined && value.stream !== null) {
    const cursor = normalizeStream(value.stream);
    if (cursors.has(cursor.stream_id)) throw new Error("operator chat cursor repeats a stream identity");
    cursors.set(cursor.stream_id, cursor);
  }
  return withOperatorStreamCursors({ offset, seq }, cursors);
}

function operatorChatLiveActivityPayload(label, config, value) {
  const payload = isObject(value) ? value : {};
  const tool = stringValue(payload.tool);
  const correlationId = stringValue(payload.call_id ?? payload.correlation_id);
  const toolCall = label === config.artifact_labels.tool_call && tool;
  const toolResult = tool && correlationId && (
    label.endsWith(".result")
    || typeof payload.ok === "boolean"
  );
  if (toolCall || toolResult) {
    const refused = Boolean(isObject(payload.body) && payload.body.refused);
    const failed = toolResult && (payload.ok === false || refused || Boolean(payload.error));
    const normalized = {
      ...payload,
      call_id: correlationId,
      status: toolCall ? "started" : failed ? "failed" : "completed",
    };
    return {
      eventKind: "dynamic_tool",
      payload: normalized,
      text: tool,
      error: failed,
    };
  }
  const eventKind = operatorChatLiveEventKind(label, config, payload);
  return {
    eventKind,
    payload,
    text: eventKind === "compaction"
      ? ""
      : eventKind.endsWith("_delta")
        ? operatorChatLiveEventText(payload, eventKind)
        : operatorChatLiveEventText(payload, eventKind) || label,
    error: payload.ok === false || Boolean(payload.error),
  };
}

function operatorChatLiveEventKind(label, config, payload) {
  if (label === config.artifact_labels.terminal_output) return "message";
  if (label === config.artifact_labels.error_output) return "error";
  if (label === config.artifact_labels.tool_call) return "dynamic_tool";
  const phase = stringValue(payload?.phase);
  if (label === "agent.message.delta") {
    return phase === "commentary" ? "commentary_delta" : "message_delta";
  }
  if (label === "turn.agent_message") {
    return phase === "commentary" ? "commentary" : phase === "partial_answer" ? "message" : "event";
  }
  return {
    "turn.reasoning": "reasoning",
    "turn.reasoning.delta": "reasoning_delta",
    "turn.plan.update": "plan",
    "turn.plan.delta": "plan_delta",
    "turn.token_usage": "usage",
    "turn.exec.command": "tool",
    "turn.exec.output_delta": "tool_output_delta",
    "turn.exec.terminal_interaction": "terminal_interaction",
    "turn.file.change": "file_change",
    "turn.file.change_delta": "file_change_delta",
    "turn.diff.updated": "diff",
    "turn.web_search": "web_search",
    "turn.hook.prompt": "hook_prompt",
    "turn.mcp.tool_call": "mcp_tool",
    "turn.mcp.progress": "mcp_progress",
    "turn.mcp.status": "mcp_status",
    "turn.dynamic_tool_call": "dynamic_tool",
    "turn.collaboration": "collaboration",
    "turn.subagent.activity": "subagent_activity",
    "turn.image.view": "image_view",
    "turn.image.generation": "image_generation",
    "turn.sleep": "sleep",
    "turn.review.entered": "review_entered",
    "turn.review.exited": "review_exited",
    "turn.context.compaction": "compaction",
    "turn.item.started": "item_started",
    "turn.started": "turn_started",
    "turn.completed": "turn_completed",
    "turn.model.rerouted": "model_rerouted",
    "turn.model.verification": "model_verification",
    "turn.auth_recovery.started": "auth_recovery_started",
    "turn.auth_recovery.completed": "auth_recovery_completed",
    "turn.warning": "warning",
    "turn.auto_review.started": "auto_review_started",
    "turn.auto_review.completed": "auto_review_completed",
    "turn.operator_request.resolved": "operator_request_resolved",
    "turn.control.acknowledged": "turn_control",
    "turn.input.acknowledged": "turn_control",
    "turn.control.failed": "turn_control",
    "turn.interrupted.result": "interrupted_result",
    "account.rate_limits": "rate_limits",
    "hook.result": "hook",
    "goal.status": "goal",
    "operator.approval.request": "approval",
    "operator.elicitation.request": "elicitation",
    "operator.user_input.request": "user_input",
  }[label] || "event";
}

function operatorChatLiveEventText(payload, eventKind) {
  if (eventKind === "compaction") return "";
  if (eventKind === "message_delta" || eventKind === "commentary_delta") return streamFragmentValue(payload?.text);
  if (["reasoning_delta", "plan_delta", "tool_output_delta", "file_change_delta"].includes(eventKind)) {
    return streamFragmentValue(payload?.delta);
  }
  if (payload?.__raw && typeof payload.__raw === "string") return payload.__raw.trim();
  if (eventKind === "commentary") return stringValue(payload?.text);
  if (eventKind === "reasoning") {
    return [...arrayValue(payload?.summary), ...arrayValue(payload?.content)]
      .map(stringValue)
      .filter(Boolean)
      .join("\n\n");
  }
  if (eventKind === "plan") return stringValue(payload?.text);
  if (eventKind === "tool") return stringValue(payload?.command);
  if (eventKind === "terminal_interaction") return stringValue(payload?.stdin);
  if (eventKind === "diff") return stringValue(payload?.diff);
  if (eventKind === "web_search") return stringValue(payload?.query);
  if (eventKind === "mcp_progress") return stringValue(payload?.message);
  if (eventKind === "mcp_tool") {
    const target = [stringValue(payload?.server), stringValue(payload?.tool)].filter(Boolean).join(" · ");
    return target || stringValue(payload?.error?.message);
  }
  if (eventKind === "dynamic_tool") return stringValue(payload?.tool);
  if (eventKind === "subagent_activity") return stringValue(payload?.agentPath ?? payload?.agent_path);
  if (eventKind === "image_view") return stringValue(payload?.path);
  if (eventKind === "image_generation") return stringValue(payload?.revisedPrompt ?? payload?.revised_prompt);
  if (["review_entered", "review_exited"].includes(eventKind)) return stringValue(payload?.review);
  if (eventKind === "model_rerouted") {
    return [stringValue(payload?.fromModel ?? payload?.from_model), stringValue(payload?.toModel ?? payload?.to_model)]
      .filter(Boolean)
      .join(" → ");
  }
  if (eventKind === "auth_recovery_started" || eventKind === "auth_recovery_completed") {
    return stringValue(payload?.message);
  }
  if (eventKind === "item_started") return stringValue(payload?.item?.type);
  if (eventKind === "file_change") {
    return arrayValue(payload?.changes).map((change) => stringValue(change?.path)).filter(Boolean).join("\n");
  }
  if (eventKind === "warning") return stringValue(payload?.message);
  if (eventKind === "goal") return stringValue(payload?.objective);
  if (eventKind === "approval") return stringValue(payload?.title);
  if (eventKind === "elicitation" || eventKind === "user_input") {
    return stringValue(payload?.message) || stringValue(payload?.title);
  }
  if (eventKind === "interrupted_result") {
    const kind = stringValue(payload?.result_kind).replaceAll("_", " ");
    return `${kind || "exact result"} admitted after interruption · not returned to model`;
  }
  const terminal = arrayValue(payload?.terminal_replies)[0];
  return stringValue(
    payload?.reply
    ?? terminal?.reply
    ?? payload?.data?.text
    ?? payload?.text
    ?? payload?.message
    ?? payload?.error,
  );
}

function operatorChatLiveEventEndsTurn({ config, eventKind, label, payload, willRetry }) {
  if (label === config.artifact_labels.terminal_output) return true;
  if (label === config.artifact_labels.error_output) return !willRetry;
  if (eventKind === "interrupted_result") return true;
  return eventKind === "turn_control"
    && label === "turn.control.acknowledged"
    && stringValue(payload?.status) === "acknowledged"
    && ["interrupt", "stop"].includes(stringValue(payload?.action));
}

function compactOperatorChatLiveEventData(payload) {
  if (!isObject(payload)) return {};
  const originalBytes = Buffer.byteLength(JSON.stringify(payload), "utf8");
  if (originalBytes <= LIVE_EVENT_DATA_MAX_BYTES) return { ...payload };
  const body = payload.body ?? payload.result ?? payload.error?.details;
  const output = body?.output ?? payload.output;
  const containsProcessOutput = isObject(output) && [
    output.aggregated_output,
    output.aggregatedOutput,
    output.stdout,
    output.stderr,
    output.chunks,
  ].some((candidate) => (
    typeof candidate === "string" && candidate.length > 0
    || Array.isArray(candidate) && candidate.length > 0
    || candidate && typeof candidate === "object" && Object.keys(candidate).length > 0
  ));
  const projection = {
    original_bytes: originalBytes,
    truncated: true,
    ...(containsProcessOutput ? { contains_process_output: true } : {}),
  };
  const projected = compactOperatorChatLiveEventValue(payload, 0);
  if (
    isObject(projected)
    && Buffer.byteLength(JSON.stringify(projected), "utf8") <= LIVE_EVENT_DATA_MAX_BYTES
  ) {
    return {
      ...projected,
      _payload_projection: projection,
    };
  }
  const shallow = {};
  for (const [key, item] of Object.entries(payload).slice(0, LIVE_EVENT_DATA_MAX_OBJECT_KEYS)) {
    if (item === null || typeof item === "boolean" || typeof item === "number") {
      shallow[key] = item;
    } else if (typeof item === "string") {
      shallow[key] = item.length > LIVE_EVENT_DATA_MAX_STRING_LENGTH
        ? `${item.slice(0, LIVE_EVENT_DATA_MAX_STRING_LENGTH)}…`
        : item;
    }
  }
  return {
    ...shallow,
    _payload_projection: projection,
  };
}

function compactOperatorChatLiveEventValue(value, depth) {
  if (value === null || typeof value === "boolean" || typeof value === "number") return value;
  if (typeof value === "string") {
    return value.length > LIVE_EVENT_DATA_MAX_STRING_LENGTH
      ? `${value.slice(0, LIVE_EVENT_DATA_MAX_STRING_LENGTH)}…`
      : value;
  }
  if (depth >= LIVE_EVENT_DATA_MAX_DEPTH) {
    if (Array.isArray(value)) return [];
    if (isObject(value)) return {};
    return null;
  }
  if (Array.isArray(value)) {
    return value
      .slice(0, LIVE_EVENT_DATA_MAX_ARRAY_ITEMS)
      .map((item) => compactOperatorChatLiveEventValue(item, depth + 1));
  }
  if (isObject(value)) {
    return Object.fromEntries(
      Object.entries(value)
        .slice(0, LIVE_EVENT_DATA_MAX_OBJECT_KEYS)
        .map(([key, item]) => [key, compactOperatorChatLiveEventValue(item, depth + 1)]),
    );
  }
  return null;
}

function artifactLabel(artifactId) {
  const separator = artifactId.lastIndexOf(":");
  const label = separator > 0 ? artifactId.slice(0, separator) : artifactId;
  return requiredString(label, "artifact label");
}

function contentHashHex(value, field) {
  const hash = typeof value === "string"
    ? value.trim().toLowerCase()
    : Array.isArray(value)
      ? Buffer.from(value).toString("hex")
      : "";
  if (!/^[0-9a-f]{64}$/.test(hash)) throw new Error(`${field} must be a 32-byte content digest`);
  return hash;
}

function databaseInteger(value, field) {
  const number = typeof value === "bigint" ? Number(value) : value;
  return nonNegativeInteger(number, field);
}

function decimalSafeInteger(value, field) {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]*)$/.test(value)) {
    throw new Error(`${field} must be an unsigned decimal integer`);
  }
  const number = Number(value);
  return nonNegativeInteger(number, field);
}

function integerAtLeast(value, minimum, field) {
  if (!Number.isSafeInteger(value) || value < minimum) {
    throw new Error(`${field} must be a safe integer at or above ${minimum}`);
  }
  return value;
}

function boundedPositiveInteger(value, fallback, maximum, field) {
  const selected = value === undefined ? fallback : value;
  if (!Number.isSafeInteger(selected) || selected <= 0 || selected > maximum) {
    throw new Error(`${field} must be a positive safe integer at or below ${maximum}`);
  }
  return selected;
}

function requiredFunction(value, field) {
  if (typeof value !== "function") throw new Error(`${field} must be a function`);
  return value;
}

async function readJsonIfPresent(file) {
  return (await readJsonDocumentIfPresent(file))?.value ?? null;
}

async function readJsonDocumentIfPresent(file) {
  try {
    const text = await fs.readFile(file, "utf8");
    const value = JSON.parse(text);
    if (!isObject(value)) throw new Error(`${file} must contain a JSON object`);
    return { value, text };
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

function assertAdmitted(verdict, operation) {
  if (isObject(verdict?.admitted) && Number.isFinite(verdict.admitted.seq)) return;
  if (isObject(verdict?.refused)) {
    throw new Error(
      `${operation} refused (${stringValue(verdict.refused.reason) || "unknown"}): ${JSON.stringify(verdict.refused.detail)}`,
    );
  }
  throw new Error(`${operation} returned a malformed delivery verdict`);
}

function endpointInterfaceId(endpoint) {
  return endpointId(endpoint, "Interface");
}

function exactArrivalRuntime(movement, interfaceId) {
  if (movement?.kind !== "InterfaceArrival") return null;
  if (endpointInterfaceId(movement.from) !== interfaceId) return null;
  const runtime = invocationRuntime(movement);
  if (
    runtime.interface !== interfaceId
    || endpointInterfaceId(movement.to) !== runtime.worker
  ) {
    throw new Error("arrival movement interface endpoints do not match its exact runtime provenance");
  }
  return runtime;
}

function exactOutboundRuntime(movement, interfaceId) {
  if (movement?.kind !== "InterfaceOutbound") return null;
  if (!endpointInterfaceId(movement.from)) return null;
  if (endpointInterfaceId(movement.to) !== interfaceId) return null;
  const runtime = invocationRuntime(movement);
  if (runtime.interface !== interfaceId) {
    throw new Error("outbound movement runtime interface does not match its exact target");
  }
  return runtime;
}

function endpointId(endpoint, kind) {
  if (!isObject(endpoint) || Object.keys(endpoint).length !== 1 || !(kind in endpoint)) return "";
  const identifier = endpoint[kind];
  return typeof identifier === "string" && identifier && identifier.trim() === identifier
    ? identifier
    : "";
}

function invocationRuntime(movement) {
  const metadata = requiredObject(
    movement?.invocation_metadata,
    "movement invocation_metadata",
  );
  const runtime = requiredObject(metadata.runtime, "movement invocation_metadata.runtime");
  for (const field of ["run", "ee", "worker", "checkpoint", "interface"]) {
    requiredTrimmedString(runtime[field], `movement runtime.${field}`);
  }
  nonNegativeInteger(runtime.epoch, "movement runtime.epoch");
  return runtime;
}

function artifactIdHasLabel(id, label) {
  return id === label || id.startsWith(`${label}:`);
}

function safeSegment(value, field) {
  const segment = requiredString(value, field);
  if (segment.length > 96 || !/^[A-Za-z0-9_-]+$/.test(segment)) {
    throw new Error(`${field} must contain only letters, digits, underscore, or hyphen`);
  }
  return segment;
}

function safeFilename(value) {
  const filename = path.basename(requiredString(value, "filename"))
    .replace(/[^A-Za-z0-9_. -]/g, "_");
  return filename || "artifact";
}

function absolutePath(value, field) {
  const input = requiredString(value, field);
  if (!path.isAbsolute(input)) throw new Error(`${field} must be absolute`);
  return path.normalize(input);
}

function randomToken(length) {
  return crypto.randomBytes(Math.ceil(length / 2)).toString("hex").slice(0, length);
}

function sha256Hex(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function isObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function requiredObject(value, field) {
  if (!isObject(value)) throw new Error(`${field} must be an object`);
  return value;
}

function arrayValue(value) {
  return Array.isArray(value) ? value : [];
}

function requiredArray(value, field) {
  if (!Array.isArray(value)) throw new Error(`${field} must be an array`);
  return value;
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function streamFragmentValue(value) {
  return typeof value === "string" ? value : "";
}

function requiredString(value, field) {
  const output = stringValue(value);
  if (!output) throw new Error(`${field} is required`);
  return output;
}

function requiredTrimmedString(value, field) {
  if (typeof value !== "string" || !value || value.trim() !== value) {
    throw new Error(`${field} must be a non-empty trimmed string`);
  }
  return value;
}

function numberValue(value, fallback) {
  return typeof value === "number" && Number.isFinite(value) && value > 0
    ? value
    : fallback;
}

function nonNegativeInteger(value, field) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${field} must be a non-negative safe integer`);
  }
  return value;
}

function positiveInteger(value, field) {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${field} must be a positive safe integer`);
  }
  return value;
}

module.exports = {
  assertGenericOperatorChatArtifactAllowed,
  loginProviderAccount,
  planOperatorChatSurface,
  planOperatorChatTaskAmendment,
  readOperatorChatArtifacts,
  refreshProviderModels,
  releaseOperatorTaskWorkerEnvironments,
  submitOperatorTurn,
  submitOperatorTurnControl,
  updateOperatorChatPins,
  validateProviderAccountObservation,
  verifyProviderAccount,
  withOperatorRunAuthoringLock,
  withOperatorWorkspaceTopologyLock,
};

function optionalServiceTier(value) {
  if (value === undefined || value === null || value === "") return "";
  if (typeof value !== "string" || value.trim() !== value || value.length > 80 || /[\u0000-\u001f\u007f]/.test(value)) {
    throw new Error("serviceTier must be a non-empty native tier id");
  }
  return value;
}
