module.exports = { findProviderModelCatalogAdmission, loginProviderAccount, refreshProviderModels, validateProviderAccountObservation, verifyProviderAccount, waitForProviderModelCatalogRearm };

const path = require("node:path");
const { readCompleteLedgerEndOffset, readCompleteLedgerTail, readLedgerMovementAtSeq, readLedgerMovementsAtSeqs } = require("./chatArtifacts.cjs");
const { liveLedgerCursors } = require("./constants.cjs");
const { normalizeLiveCursor, persistLiveCursor, readPersistedLiveCursor, rememberLiveCursor, watchLedgerChange } = require("./liveCursors.cjs");
const { delayProviderLoginPoll, providerAccountEvidence, providerAccountEvidenceFromCoordinates, providerAccountSnapshotCoordinates, providerLoginRemainingMs, providerModelCatalogRemainingMs, waitForProviderAccountObservation, waitForProviderLoginStarted, waitForProviderLoginStatus, waitForProviderModelCatalogObservation } = require("./providerObservations.cjs");
const { providerAccountLoginRequest, providerAccountRequest, providerAccountRequestCoordinates, providerAccountSubmissionSeq, providerModelCatalogRequest, providerOperationRequestCoordinates, providerOperationSubmissionSeq, resolveProviderAccountContext, resolveProviderModelCatalogContext } = require("./providerRoutes.cjs");
const { withRunAuthoringLock } = require("./publication.cjs");
const { canonicalJson } = require("./topologyPlan.cjs");
const { absolutePath, exactArrivalRuntime, invocationRuntime, isObject, nonNegativeInteger, requiredFunction } = require("./values.cjs");

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
    if (runtime.worker !== context.run.worker) {
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
