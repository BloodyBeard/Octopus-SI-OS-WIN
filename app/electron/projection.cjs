const { parseScheduledDueAt } = require("./scheduledTime.cjs");
const { projectHomeVoiceContext } = require("./homeVoiceProjection.cjs");
const { payloadArtifactBindings } = require("./contentAddressedArtifact.cjs");
const { artifactReadProjectionForLabel } = require("./boundedJsonProjection.cjs");
const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const path = require("node:path");
const { blake3 } = require("@noble/hashes/blake3");
const { parseProjectionReadRequest } = require("./projectionReadContract.cjs");
const { operatorActivityRows } = require("./operatorActivity.cjs");
const { readLocalPackageCatalog } = require("./localPackages.cjs");
const { readWorkspaceSetupState } = require("./workspaceSetup.cjs");
const { clientAgentCatalogPath, readClientAgentCatalog } = require("./clientAgentCatalog.cjs");
const { readApplicationPackageCatalog } = require("./projection/applicationCatalog.cjs");
const { applicationTaskCapabilityReferences } = require("./projection/applicationPackages.cjs");
const { ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES, ARTIFACT_PAYLOAD_READ_CONCURRENCY, ARTIFACT_PREVIEW_MAX_BYTES, DESCRIPTOR_REGISTRIES, INTERFACE_MEDIA_MAX_BYTES, ProjectionEvidenceError, RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_BYTES, RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_ENTRIES } = require("./projection/constants.cjs");
const { activationDriverRef, cronScheduleFingerprint, parseCronActivationDocument, scheduledInterfaceDisplayName } = require("./projection/cronActivation.cjs");
const { readJsonFile, readOptionalRegularJsonDocument, stableJson } = require("./projection/jsonFiles.cjs");
const { artifactAttachmentBytes, artifactMovementCoordinates, artifactPayloadFilename, artifactPayloadMimeType, compareArtifactMovementNewest, compareMovementOrder, escapeSqlLike, filterByEnvelopeKind, filterInterfaceArtifactTransferTargets, immutableClientAction, indexedMovementRunId, indexedUnsignedInteger, interfaceArtifactTransferTargetRow, isClientSurfaceDocumentPayload, ledgerRowsForRun, loadLedgerArtifactPayload, matchesMovementPage, movementBaseKind, movementBelongsToRun, movementBoundaryInterface, movementEndpoint, movementKindText, movementPageRequiresIndex, movementPayloadPreview, movementRows, movementRunId, nestedArtifactMedia, normalizeIndexedProjectionMovement, normalizedMovementKindFilter, openValidatedLedgerIndex, optionalActivityAfterSeq, optionalUnsignedInteger, pendingArtifactRows, projectMovementRows, projectedTextFile, projectionLedgerSourceCursor, projectionRequiresFullLedger, readChangeJournal, readIndexedTaskSearchCandidates, readStableContentAddressedArtifactBytes, recentMovementRows, summarizeInterfaceArtifactTransferTargets, taskSearchSnippet, validateCanonicalMovement } = require("./projection/movementRows.cjs");
const { activityDirection, appendOperatorChatAssistantMessage, nativeTurnRecordedInputs, operatorActivityAgent, operatorActivityMessage, operatorChatActivityPayload, operatorChatAttachments, operatorChatEventData, operatorChatEventIdentity, operatorChatEventKind, operatorChatEventText, operatorChatExecutionScope, operatorChatExpandedStreamRows, operatorChatProjectedRowId, operatorChatProviderFailureActivity, operatorChatRequestKey, operatorChatResponseKey, operatorChatRunProvider, operatorChatRuntimeProviderId, operatorChatSearchMessage, operatorChatStreamPositionCovered, operatorChatTurnText, operatorTurnControlEndsTurn, operatorTurnControlOutcome } = require("./projection/operatorChatEvents.cjs");
const { mutateApplicationPrivateConfiguration, readApplicationLogoAsset, readInterfacePrivateConfigurationProjection } = require("./projection/privateConfiguration.cjs");
const { listWorkerEnvironmentRecordFiles, providerInterfaceDescriptorRows, readOperatorChatProtocol, readProviderInterfaceDescriptors } = require("./projection/providerDescriptors.cjs");
const { applicationPrivateConfigurationProjection, baseInterfaceRows, bindingRows, descendantEnvironmentIds, descriptorInventoryRows, descriptorKindRows, descriptorRows, eeInstanceRows, eeTemplateDescriptorRows, eeTemplateRows, envelopeRows, environmentLineage, filterInterfaces, goalRows, interfaceActivationEntries, isOperatorChatProviderFailureRow, isOperatorChatProviderMovement, lowestCommonEnvironment, markKindRows, markRows, normalizeDescriptorRow, octopusOsInterfaceSidecars, operatorOsProjectRows, policyRows, preserveProjectionEvidenceError, projectedFilesystemRootSurfaces, projectedGitSurfaces, projectedRunInterfaceIds, projectedWorktreeStatusSurfaces, projectionMemo, projectionSnapshot, projectionTruthChange, readEeInstanceRecords, readHostRootRefs, readRunRecords, recordCheckpointRowsFor, requiredVerifiedArtifactOutcomeIndex, runCheckpointRows, runRows, toolRows, unavailableViewerSurface, validAddressWithProtocols, validHttpAddress, validWebSocketAddress, validWorkerEnvironmentLiveState, validateEePolicyProjectionSnapshot, verifierRows, withoutRawProjectionEvidence, workerEnvironmentCapabilities, workerEnvironmentLiveAvailable, workerEnvironmentRows, workerEnvironmentSession, workerEnvironmentWorkspace, workerRows } = require("./projection/registryRows.cjs");
const { armSetupTriggers, mutateScheduledInterface, setupTriggerEntries, setupTriggerProof } = require("./projection/scheduled.cjs");
const { normalizeRuntimeLaunchEnvironment, parseApplicationManifest, parseClientWorkerTemplate, parseOctopusOsSidecar } = require("./projection/sidecars.cjs");
const { runInterfaceReferences } = require("./projection/taskDeletion.cjs");
const { arrayValue, contentHashHex, dedupeRows, endpointId, filterByString, groupRowsBy, interfaceArtifactTransferProjection, interfaceBindingId, isAdmittedInterfaceArtifactMovement, isPathInside, isPlainRecord, ledgerArtifactId, ledgerArtifactLabel, limitRows, mapConcurrentOrdered, numberValue, objectOrEmpty, requiredContentHash, requiredString, rowTimeMs, safeInterfacePathSegment, stringValue, timestampOf, topologyArtifactRow, validWorkerEnvironmentFormation } = require("./projection/values.cjs");
const { mutateOctopusOsWorkspace } = require("./projection/workspaceMutations.cjs");

function createProjectionStore(options) {
  const resolveRepoRoot = options.resolveRepoRoot;
  const resolveSourceRegistry = typeof options.sourceRegistry === "function"
    ? options.sourceRegistry
    : () => path.join(resolveRepoRoot(), "registry");
  const resolveLocalPackageRegistry = typeof options.localPackageRegistry === "function"
    ? options.localPackageRegistry
    : resolveSourceRegistry;
  const resolveLocalPackageLibrary = typeof options.localPackageLibrary === "function"
    ? options.localPackageLibrary
    : () => path.join(resolveRepoRoot(), "app", "distribution", "local-packages");
  const octopusHome = options.octopusHome;
  const onChange = typeof options.onChange === "function" ? options.onChange : () => {};
  const beforeRead = typeof options.beforeRead === "function" ? options.beforeRead : async () => {};
  const nativeBrowserTargetsForProjection = typeof options.nativeBrowserTargetsForProjection === "function"
    ? options.nativeBrowserTargetsForProjection : null;
  const nativeBrowserDownloadsForProjection = typeof options.nativeBrowserDownloadsForProjection === "function"
    ? options.nativeBrowserDownloadsForProjection : null;
  const runtimeLaunchEnvironment = typeof options.runtimeLaunchEnvironment === "function"
    ? options.runtimeLaunchEnvironment
    : () => null;


  let projectionGeneration = 1;
  let projectionStructureGeneration = 1;
  let configurationProjectionGeneration = 1;
  let configurationProjectionBuild = null;
  let projectionBuild = null;
  let projectionBuildPending = null;
  let policyProjectionBuild = null;
  let policyProjectionGeneration = 1;
  let projectionBroadcastTimer = null;
  let projectionBroadcastDeadline = 0;
  let projectionBroadcastKind = "ledger";
  const projectionWatchers = new Map();
  const projectionLedgerWatchers = new Map();
  const projectionWatchHealth = new Map();
  let projectionFallbackTimer = null;
  let projectionFallbackFingerprint = "";
  let projectionFallbackReading = false;
  let projectionFallbackError = "";

  function projectionWatchRoots() {
    return [octopusHome()];
  }

  function projectionLedgerFiles() {
    return [path.join(octopusHome(), "ledger", "movements.jsonl")];
  }

  function ensureProjectionWatchers() {
    for (const root of projectionWatchRoots()) {
      if (projectionWatchers.has(root)) continue;
      try {
        const watcher = require("node:fs").watch(root, { recursive: true, persistent: false }, (_event, filename) => {
          const change = projectionTruthChange(filename);
          if (change === "policy") invalidateProjection(true);
          else if (change === "structure") invalidateProjection(false);
          else if (change === "runs") invalidateProjection(false, "runs");
          else if (change === "files") invalidateProjection(false, "files");
          else if (change === "ledger") signalLedgerProjectionChange();
        });
        watcher.on("error", (error) => {
          try { watcher.close(); } catch {}
          projectionWatchers.delete(root);
          projectionWatchHealth.set(root, String(error?.message ?? error));
          invalidateProjection();
          ensureProjectionFallbackWatchers();
          ensureProjectionFallbackPoller();
        });
        projectionWatchers.set(root, watcher);
        projectionWatchHealth.delete(root);
        invalidateProjection();
      } catch (error) {
        projectionWatchHealth.set(root, String(error?.message ?? error));
      }
    }
    if (projectionWatchers.size > 0) {
      stopProjectionFallbackPoller();
      for (const [file, listener] of [...projectionLedgerWatchers]) {
        fsSync.unwatchFile(file, listener);
        projectionLedgerWatchers.delete(file);
      }
      return;
    }
    ensureProjectionFallbackWatchers();
    ensureProjectionFallbackPoller();
  }

  function ensureProjectionFallbackWatchers() {
    for (const file of projectionLedgerFiles()) {
      if (projectionLedgerWatchers.has(file)) continue;
      const listener = (current, previous) => {
        if (
          current.dev === previous.dev
          && current.ino === previous.ino
          && current.size === previous.size
          && current.mtimeMs === previous.mtimeMs
          && current.nlink === previous.nlink
        ) return;
        signalLedgerProjectionChange();
      };
      fsSync.watchFile(file, { interval: 250, persistent: false }, listener);
      projectionLedgerWatchers.set(file, listener);
    }
  }

  function projectionWatchWarnings() {
    const warnings = [...projectionWatchHealth.entries()].map(([root, detail]) => (
      `Projection filesystem watch degraded for ${root}: ${detail}; authoritative fingerprint polling is active until the watcher recovers.`
    ));
    if (projectionFallbackError) warnings.push(projectionFallbackError);
    return warnings;
  }

  function ensureProjectionFallbackPoller() {
    if (projectionFallbackTimer) return;
    const poll = async () => {
      if (projectionFallbackReading || projectionWatchers.size > 0) return;
      projectionFallbackReading = true;
      try {
        const fingerprint = await readProjectionTruthFingerprint();
        if (projectionFallbackFingerprint && fingerprint !== projectionFallbackFingerprint) invalidateProjection();
        projectionFallbackFingerprint = fingerprint;
        if (projectionFallbackError) {
          projectionFallbackError = "";
          invalidateProjection();
        }
      } catch (error) {
        const detail = `Projection fingerprint polling failed: ${String(error?.message ?? error)}`;
        if (projectionFallbackError !== detail) {
          projectionFallbackError = detail;
          invalidateProjection();
        }
      } finally {
        projectionFallbackReading = false;
      }
    };
    void poll();
    projectionFallbackTimer = setInterval(() => void poll(), 1_000);
    projectionFallbackTimer.unref?.();
  }

  function stopProjectionFallbackPoller() {
    if (projectionFallbackTimer) clearInterval(projectionFallbackTimer);
    projectionFallbackTimer = null;
    projectionFallbackFingerprint = "";
    projectionFallbackReading = false;
    projectionFallbackError = "";
  }

  async function readProjectionTruthFingerprint() {
    const root = octopusHome();
    const pending = [
      path.join(root, "registry"),
      path.join(root, "state", "runs"),
      path.join(root, "state", "ee_instances"),
      path.join(root, "interfaces"),
      path.join(root, "app-server-control"),
      path.join(root, "client", "package-state"),
      path.join(root, "client", "agents"),
      path.join(root, "client-state", "workspace"),
    ];
    const facts = [];
    const hostManifest = path.join(root, "host.json");
    try {
      const stat = await fs.stat(hostManifest);
      facts.push(`host.json\0${stat.dev}\0${stat.ino}\0${stat.size}\0${stat.mtimeMs}`);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
    while (pending.length > 0) {
      const current = pending.pop();
      let entries;
      try {
        entries = await fs.readdir(current, { withFileTypes: true });
      } catch (error) {
        if (error?.code === "ENOENT") continue;
        throw error;
      }
      for (const entry of entries) {
        if (entry.isSymbolicLink()) continue;
        const absolute = path.join(current, entry.name);
        if (entry.isDirectory()) {
          pending.push(absolute);
          continue;
        }
        if (!entry.isFile()) continue;
        const relative = path.relative(root, absolute);
        if (!projectionTruthChange(relative)) continue;
        const stat = await fs.stat(absolute);
        facts.push(`${relative}\0${stat.dev}\0${stat.ino}\0${stat.size}\0${stat.mtimeMs}`);
      }
    }
    facts.sort();
    return crypto.createHash("sha256").update(facts.join("\n")).digest("hex");
  }

  function fireProjectionBroadcast() {
    projectionBroadcastTimer = null;
    projectionBroadcastDeadline = 0;
    const kind = projectionBroadcastKind;
    projectionBroadcastKind = "ledger";
    onChange(projectionGeneration, kind);
  }

  function scheduleProjectionBroadcast(kind, coalesce) {
    const priority = { ledger: 0, runs: 1, files: 2, structure: 3 };
    if (priority[kind] > priority[projectionBroadcastKind]) projectionBroadcastKind = kind;
    if (!coalesce) {
      if (projectionBroadcastTimer) return;
      projectionBroadcastTimer = setTimeout(fireProjectionBroadcast, 120);
      return;
    }
    const now = Date.now();
    if (projectionBroadcastDeadline === 0) projectionBroadcastDeadline = now + 480;
    if (projectionBroadcastTimer) clearTimeout(projectionBroadcastTimer);
    const wait = Math.max(0, Math.min(120, projectionBroadcastDeadline - now));
    projectionBroadcastTimer = setTimeout(fireProjectionBroadcast, wait);
  }

  function signalLedgerProjectionChange() {
    projectionGeneration += 1;
    scheduleProjectionBroadcast("ledger", false);
  }

  function invalidateProjection(policyChanged = true, kind = "structure") {
    projectionGeneration += 1;
    projectionStructureGeneration += 1;
    if (kind !== "runs") configurationProjectionGeneration += 1;
    if (policyChanged) {
      policyProjectionGeneration += 1;
    }
    projectionBuild = null;
    scheduleProjectionBroadcast(kind, true);
  }

  function fileProjectionScope(repoRoot) {
    return JSON.stringify([repoRoot, octopusHome(), resolveSourceRegistry(), resolveLocalPackageRegistry(), resolveLocalPackageLibrary()]);
  }

  async function cachedFileProjection(repoRoot, includeLedger) {
    ensureProjectionWatchers();
    const scope = fileProjectionScope(repoRoot);
    for (;;) {
      let build;
      if (projectionBuild && projectionBuild.structureGeneration === projectionStructureGeneration && projectionBuild.scope === scope) {
        build = projectionBuild;
      } else if (projectionBuildPending?.scope === scope) {
        build = projectionBuildPending;
      } else {
        const promise = buildFileProjection(repoRoot);
        build = { structureGeneration: projectionStructureGeneration, scope, promise };
        projectionBuild = build;
        projectionBuildPending = build;
        const settled = () => {
          if (projectionBuildPending === build) projectionBuildPending = null;
        };
        void promise.then(settled, settled);
        void promise.catch(() => {
          if (projectionBuild === build) projectionBuild = null;
        });
      }
      const projection = await build.promise;
      if (scope !== fileProjectionScope(repoRoot)) throw new Error("Projection Home changed during read");
      if (build.structureGeneration !== projectionStructureGeneration) continue;
      if (!includeLedger) return projection;
      return refreshLedgerProjection(projection);
    }
  }

  async function refreshLedgerProjection(projection) {
    const ledger = await readLedgerProjection(projection.durableRoot, projection.warnings);
    if (ledger.revision !== projection.ledgerRevision) {
      projection.ledgerRows = ledger.rows;
      projection.ledgerRowsByRunSources = ledger.rowsByRunSources;
      projection.invalidLedgerRows = ledger.invalidRows;
      projection.ledgerRevision = ledger.revision;
      projection.ledgerMemo.clear();
      projection.operatorChatRunIndexCache = null;
      projection.generatedAt = new Date().toISOString();
      if (projection.generation === projectionGeneration) signalLedgerProjectionChange();
      projection.generation = projectionGeneration;
    }
    return projection;
  }

  async function readProjection(request) {
    await beforeRead();
    const { method, params } = parseProjectionReadRequest(request);
    if (method === "projection.interface_artifacts.list" || method === "projection.client_actions.list"
      || method === "projection.movements.list") {
      const home = octopusHome();
      if (method === "projection.client_actions.list" && params.home !== home) {
        throw new Error("Client action read does not match the selected Home.");
      }
      ensureProjectionWatchers();
      const ledgerView = {
        durableRoot: resolveDurableRoot(),
        warnings: [],
        ledgerRows: [],
        ledgerRevision: "",
        ledgerMemo: new Map(),
      };
      switch (method) {
        case "projection.interface_artifacts.list":
          return { artifacts: await interfaceArtifactPayloadRows(ledgerView, params) };
        case "projection.client_actions.list":
          return { home, run_id: params.run_id, interface_id: params.interface_id,
            actions: await clientActionRows(ledgerView, params) };
        case "projection.movements.list":
          return readMovementPage(ledgerView, params);
      }
    }
    const exactInterface = params.interface_id ?? params.interface;
    if (method === "projection.interfaces.artifact_transfer.targets"
      && typeof exactInterface === "string" && exactInterface) {
      const interfaces = await readDescriptorRows(
        path.join(octopusHome(), "registry"),
        DESCRIPTOR_REGISTRIES.find((registry) => registry.kind === "interface"),
        [], null, new Set([exactInterface]),
      );
      const runProjection = params.run_id === undefined ? undefined : {
        runRecords: (await readRunRecords(path.join(octopusHome(), "state", "runs"))).records,
      };
      return { artifact_transfer_targets: limitRows(
        filterInterfaceArtifactTransferTargets(interfaceArtifactTransferTargetRowsFor(interfaces), params, runProjection), params,
      ) };
    }
    const repoRoot = resolveRepoRoot();
    const projection = await cachedFileProjection(
      repoRoot,
      projectionRequiresFullLedger(method, params),
    );
    switch (method) {
      case "projection.runtime.snapshot":
        return projectionSnapshot(projection);
      case "projection.runtime.topology":
        return cachedTopologySnapshot(projection, params);
      case "projection.ee_instances.list":
        return { ee_instances: limitRows(eeInstanceRows(projection, params.include_ledger !== false), params) };
      case "projection.runs.list":
        return {
          runs: limitRows(runRows(projection, params.include_ledger !== false), params),
          invalid_run_records: projection.invalidRunRecords,
        };
      case "projection.runs.checkpoints.list": {
        const includeLedger = params.include_ledger !== false;
        const outcomeIndex = includeLedger ? requiredVerifiedArtifactOutcomeIndex(projection) : new Map();
        const recordByRun = new Map(arrayValue(projection.runRecords).map((record) => [record.run_id, record]));
        const runs = limitRows(runRows(projection, includeLedger), params);
        return {
          run_checkpoints: runs.map((run) => {
            const runId = stringValue(run.run_id);
            return {
              run_id: runId,
              checkpoints: recordCheckpointRowsFor(projection, runId, outcomeIndex, recordByRun)
                ?? runCheckpointRows(projection, runId),
            };
          }),
        };
      }
      case "projection.runs.get": {
        const runId = requiredString(params.run_id, "run_id");
        const includeLedger = params.include_ledger !== false;
        const run = runRows(projection, includeLedger).find((row) => stringValue(row.run_id) === runId);
        if (!run) throw new Error(`run is not present in file projection: ${runId}`);
        const workerEnvironments = workerEnvironmentRows(projection)
          .filter((row) => stringValue(row.run_id) === runId);
        return {
          run,
          bindings: bindingRows(projection).filter((row) => stringValue(row.run_id) === runId),
          artifacts: includeLedger
            ? artifactRows(projection).filter((row) => stringValue(row.run_id ?? row.meta?.run_id) === runId)
            : [],
          marks: includeLedger
            ? markRows(projection).filter((row) => stringValue(row.run_id ?? row.meta?.run_id) === runId)
            : [],
          checkpoints: recordCheckpointRowsFor(
            projection,
            runId,
            includeLedger ? requiredVerifiedArtifactOutcomeIndex(projection) : new Map(),
          ) ?? (includeLedger ? runCheckpointRows(projection, runId) : []),
          worker_environments: workerEnvironments,
          ...(workerEnvironments.length === 1 ? { worker_environment: workerEnvironments[0] } : {}),
        };
      }
      case "projection.trace.recent": {
        const runId = requiredString(params.run_id, "run_id");
        const rows = envelopeRows(projection).filter((row) => stringValue(row.run_id ?? row.meta?.run_id ?? row.payload?.run_id) === runId);
        return { events: limitRows(rows, params) };
      }
      case "projection.interfaces.list":
        return { interfaces: limitRows(filterInterfaces(interfaceRows(projection), params), params) };
      case "projection.interfaces.artifact_transfer.targets":
        return {
          artifact_transfer_targets: limitRows(
            filterInterfaceArtifactTransferTargets(interfaceArtifactTransferTargetRows(projection), params, projection),
            params,
          ),
        };
      case "projection.bindings.list":
        return { bindings: limitRows(bindingRows(projection), params) };
      case "projection.artifacts.list":
        return artifactListProjection(projection, params);
      case "projection.marks.list":
        return { marks: limitRows(markRows(projection), params) };
      case "projection.worker_environments.list":
        return { worker_environments: limitRows(workerEnvironmentRows(projection), params) };
      case "projection.worker_environment_pools.list":
        return { worker_environment_pools: [] };
      case "projection.envelopes.list":
        return { envelopes: limitRows(filterByEnvelopeKind(envelopeRows(projection), params.kind), params) };
      case "projection.interfaces.pending.list":
        return { pending_artifacts: limitRows(pendingArtifactRows(projection), params) };
      case "projection.goals.snapshot":
        return { goals: limitRows(goalRows(projection), params) };
      case "projection.ee_templates.list":
        return { ee_templates: limitRows(eeTemplateRows(projection), params) };
      case "projection.workers.list":
        return { workers: limitRows(workerRows(projection), params) };
      case "projection.tools.list":
        return { tools: limitRows(toolRows(projection), params) };
      case "projection.verifiers.list":
        return { verifiers: limitRows(verifierRows(projection), params) };
      case "projection.policy.ee.list":
        return { policy: limitRows(policyRows(projection), params) };
      case "projection.descriptors.inventory.list":
        return { descriptors: descriptorInventoryRows(projection) };
      case "projection.descriptors.kinds.list":
        return { descriptors: descriptorKindRows() };
      case "projection.descriptors.artifact_labels.list":
        return { descriptors: limitRows(artifactLabelRows(projection), params) };
      case "projection.descriptors.mark_kinds.list":
        return { descriptors: limitRows(markKindRows(projection), params) };
      case "projection.descriptors.tool.list":
        return { descriptors: limitRows(toolRows(projection), params) };
      case "projection.descriptors.worker_environment_kind.list":
        return { descriptors: limitRows(descriptorRows(projection, "worker_environment_kind"), params) };
      case "projection.descriptors.ee.list":
        return { descriptors: limitRows(eeTemplateDescriptorRows(projection), params) };
      case "projection.operator.chats.list": {
        if (params.control_targets_only === true) return { chats: await operatorChatControlTargets(projection, params.include_titles === true) };
        const chats = limitRows(await operatorChatsList(projection), params);
        return { chats, ...(params.include_activity === true ? { activity: await operatorActivity(projection, chats) } : {}) };
      }
      case "projection.operator.chats.status":
        return await operatorChatStatuses(projection, params);
      case "projection.operator.os.scope":
        return operatorOsScope(projection);
      case "projection.operator.os.local_packages":
        return projection.localPackages;
      case "projection.operator.os.workspace_setup":
        return projection.workspaceSetup;
      case "projection.operator.os.applications":
        return operatorOsApplications(projection);
      case "projection.operator.os.drivers":
        return operatorOsDrivers(projection);
      case "projection.operator.run.activity":
        return await operatorRunActivity(projection, requiredString(params.run_id, "run_id"), params);
      case "projection.operator.chat.contexts": {
        const applications = arrayValue(operatorOsApplications(projection).applications);
        return {
          contexts: operatorChatContextRows(projection),
          application_catalog: applications.map((application) => ({
            id: stringValue(application.id),
            name: stringValue(application.name),
            description: stringValue(application.description),
            category: stringValue(application.category),
            icon: stringValue(application.icon),
            logo_data_url: stringValue(application.logo_data_url),
            logo_dark_data_url: stringValue(application.logo_dark_data_url),
            interfaces: arrayValue(application.interfaces),
            artifact_bindings: arrayValue(application.artifact_bindings),
            task_capabilities: objectOrEmpty(application.task_capabilities),
          })),
        };
      }
      case "projection.operator.chat.messages":
        return { messages: await operatorChatMessages(projection, requiredString(params.run_id, "run_id")) };
      case "projection.operator.chat.search":
        return await operatorChatSearch(projection, params);
      case "projection.operator.providers.list":
        return { providers: providerInterfaceDescriptorRows(baseInterfaceRows(projection)) };
      case "projection.operator.providers.status":
        return { providers: await operatorProviderStatusRows(projection) };
      case "projection.scheduled_interfaces.list":
        return { scheduled_interfaces: await scheduledInterfaceRows(projection), scope_options: scheduledScopeOptions(projection) };
      case "projection.movements.payload":
        return await movementPayloadPreview(params);
      case "projection.client_surfaces.list":
        return await clientSurfaceDocumentSnapshot(projection);
      case "projection.artifact.bytes.read":
        return await admittedArtifactBytes(projection, params);
      case "projection.interface_media.read":
        return await interfaceArtifactMedia(projection, params);
      default:
        throw new Error(`projection method is not implemented: ${method}`);
    }
  }

  async function readMovementPage(projection, params) {
    const indexed = await readIndexedMovementPage(projection, params);
    if (indexed) return indexed;
    if (movementPageRequiresIndex(params)) {
      throw new Error("ledger index is required for cursor paging or search");
    }
    await refreshLedgerProjection(projection);
    const movements = await movementRows(projection);
    return {
      movements: limitRows(movements, params),
      total: movements.length,
      next_seq: movements.reduce((next, row) => Math.max(next, row.seq + 1), 0),
    };
  }

  async function filteredMovementRows(projection, params) {
    const kinds = normalizedMovementKindFilter(params);
    if (kinds.length === 0) return null;
    const limit = Math.min(5_000, Math.max(1, numberValue(params.limit, 1_000)));
    const indexed = await readIndexedFilteredMovements(projection, new Set(kinds), limit);
    if (indexed) {
      const movements = projectMovementRows(projection, indexed.rows).slice(0, limit);
      return { movements, total: indexed.total };
    }
    await refreshLedgerProjection(projection);
    const rows = projection.ledgerRows.filter((row) => kinds.includes(movementBaseKind(row)));
    const movements = projectMovementRows(projection, rows);
    return { movements: movements.slice(0, limit), total: movements.length };
  }

  async function readIndexedMovementPage(projection, params) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const limit = Math.min(1_000, Math.max(1, numberValue(params?.limit, 500)));
      const beforeSeq = optionalUnsignedInteger(params?.before_seq);
      const afterSeq = optionalUnsignedInteger(params?.after_seq);
      const recordedAfter = optionalUnsignedInteger(params?.recorded_after_unix_ms);
      const recordedBefore = optionalUnsignedInteger(params?.recorded_before_unix_ms);
      const query = stringValue(params?.query).toLocaleLowerCase();
      const kinds = normalizedMovementKindFilter(params);
      if (query.length > 512) throw new Error("movement query exceeds 512 characters");
      if (beforeSeq !== null && afterSeq !== null && beforeSeq <= afterSeq) {
        throw new Error("movement cursor before_seq must be greater than after_seq");
      }
      const predicates = ["seq < ?"];
      const values = [Math.min(beforeSeq ?? index.watermark, index.watermark)];
      if (afterSeq !== null) {
        predicates.push("seq > ?");
        values.push(afterSeq);
      }
      if (recordedAfter !== null) {
        predicates.push("recorded_at_unix_ms >= ?");
        values.push(recordedAfter);
      }
      if (recordedBefore !== null) {
        predicates.push("recorded_at_unix_ms <= ?");
        values.push(recordedBefore);
      }
      if (kinds.length > 0) {
        predicates.push(`kind IN (${kinds.map(() => "?").join(", ")})`);
        values.push(...kinds);
      }
      if (query) {
        const like = `%${escapeSqlLike(query)}%`;
        predicates.push(`(
          LOWER(kind) LIKE ? ESCAPE '\\'
          OR LOWER(trace) LIKE ? ESCAPE '\\'
          OR LOWER(COALESCE(artifact_id, '')) LIKE ? ESCAPE '\\'
          OR LOWER(COALESCE(artifact_label, '')) LIKE ? ESCAPE '\\'
          OR LOWER(COALESCE(from_id, '')) LIKE ? ESCAPE '\\'
          OR LOWER(COALESCE(to_id, '')) LIKE ? ESCAPE '\\'
        )`);
        values.push(like, like, like, like, like, like);
      }
      const where = predicates.join("\nAND ");
      const select = `SELECT
        seq, lamport, recorded_at_unix_ms, trace, kind, mark,
        from_kind, from_id, to_kind, to_id,
        artifact_id, artifact_content,
        runtime_run, runtime_checkpoint, runtime_interface
        FROM movements
        WHERE ${where}
        ORDER BY seq DESC
        LIMIT ?`;
      const indexedRows = index.database.prepare(select)
        .all(...values, limit)
        .map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const indexedCount = indexedUnsignedInteger(index.database.prepare(
        `SELECT COUNT(*) AS count FROM movements WHERE ${where}`,
      ).get(...values)?.count);
      if (indexedCount === null) return null;
      const tail = await readMovementLedgerTailPage(index.ledgerPath, index.durableEndOffset, index.watermark, {
        afterSeq,
        beforeSeq,
        kinds: new Set(kinds),
        query,
        recordedAfter,
        recordedBefore,
        limit,
      });
      if (tail === null) return null;
      const rows = projectMovementRows(projection, [...indexedRows, ...tail.rows])
        .slice(0, limit);
      const total = indexedCount + tail.total;
      const oldest = rows.reduce((value, row) => {
        const seq = indexedUnsignedInteger(row.seq);
        return seq === null ? value : value === null ? seq : Math.min(value, seq);
      }, null);
      return {
        movements: rows,
        total,
        next_seq: tail.nextSeq,
        ...(oldest !== null && rows.length < total ? { next_before_seq: oldest } : {}),
      };
    } finally {
      index.database.close();
    }
  }

  async function readMovementLedgerTailPage(ledgerPath, startOffset, expectedSeq, filter) {
    const stat = await fs.stat(ledgerPath);
    if (stat.size < startOffset) return null;
    if (stat.size === startOffset) return { rows: [], total: 0, nextSeq: expectedSeq };
    const rows = [];
    let total = 0;
    let pending = Buffer.alloc(0);
    for await (const chunk of fsSync.createReadStream(ledgerPath, { start: startOffset, end: stat.size - 1 })) {
      const bytes = pending.length ? Buffer.concat([pending, chunk]) : chunk;
      let recordStart = 0;
      for (;;) {
        const newline = bytes.indexOf(0x0a, recordStart);
        if (newline < 0) break;
        const end = newline > recordStart && bytes[newline - 1] === 0x0d ? newline - 1 : newline;
        const raw = bytes.subarray(recordStart, end);
        recordStart = newline + 1;
        if (!raw.length) continue;
        const movement = parseCanonicalMovement(raw, ledgerPath, expectedSeq + 1);
        const seq = indexedUnsignedInteger(movement?.seq);
        if (seq === null || seq !== expectedSeq) return null;
        expectedSeq += 1;
        if (!matchesMovementPage(movement, seq, filter)) continue;
        total += 1;
        rows.push({
          ...movement,
          kind: movementKindText(movement.kind),
          envelope_id: stringValue(movement.envelope_id ?? movement.id) || `movements:${seq + 1}`,
          source_file: ledgerPath,
          source_line: seq + 1,
        });
        if (rows.length >= filter.limit * 2) rows.splice(0, rows.length - filter.limit);
      }
      pending = bytes.subarray(recordStart);
    }
    return { rows: rows.slice(-filter.limit), total, nextSeq: expectedSeq };
  }

  async function readIndexedFilteredMovements(projection, kinds, limit) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const kindList = [...kinds];
      const placeholders = kindList.map(() => "?").join(", ");
      const indexedRows = index.database.prepare(
        `SELECT
           seq, lamport, recorded_at_unix_ms, trace, kind, mark,
           from_kind, from_id, to_kind, to_id,
           artifact_id, artifact_content,
           runtime_run, runtime_checkpoint, runtime_interface
         FROM movements
         WHERE kind IN (${placeholders})
         ORDER BY seq DESC
         LIMIT ?`,
      ).all(...kindList, limit).map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const countRow = index.database.prepare(
        `SELECT COUNT(*) AS count
         FROM movements
         WHERE kind IN (${placeholders})`,
      ).get(...kindList);
      const indexedCount = indexedUnsignedInteger(countRow?.count);
      if (indexedCount === null) return null;
      const tail = await readFilteredLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        kinds,
      );
      if (tail === null) return null;
      return {
        rows: [...indexedRows, ...tail.rows],
        total: indexedCount + tail.count,
      };
    } catch (error) {
      preserveProjectionEvidenceError(error);
      return null;
    } finally {
      index.database.close();
    }
  }

  async function readFilteredLedgerTail(ledgerPath, startOffset, expectedSeq, kinds) {
    const stat = await fs.stat(ledgerPath);
    if (stat.size < startOffset) return null;
    if (stat.size === startOffset) return { rows: [], count: 0 };
    const rows = [];
    let count = 0;
    let pending = Buffer.alloc(0);
    for await (const chunk of fsSync.createReadStream(ledgerPath, { start: startOffset })) {
      const bytes = pending.length ? Buffer.concat([pending, chunk]) : chunk;
      let recordStart = 0;
      for (;;) {
        const newline = bytes.indexOf(0x0a, recordStart);
        if (newline < 0) break;
        const end = newline > recordStart && bytes[newline - 1] === 0x0d ? newline - 1 : newline;
        const raw = bytes.subarray(recordStart, end);
        recordStart = newline + 1;
        if (!raw.length) continue;
        const movement = parseCanonicalMovement(raw, ledgerPath, expectedSeq + 1);
        const seq = indexedUnsignedInteger(movement?.seq);
        if (seq === null || seq !== expectedSeq) return null;
        expectedSeq += 1;
        if (!kinds.has(movementBaseKind(movement))) continue;
        count += 1;
        rows.push({
          ...movement,
          kind: movementKindText(movement.kind),
          envelope_id: stringValue(movement.envelope_id ?? movement.id) || `movements:${seq + 1}`,
          source_file: ledgerPath,
          source_line: seq + 1,
        });
      }
      pending = bytes.subarray(recordStart);
    }
    return { rows, count };
  }

  async function indexedArtifactLabelRows(projection, label) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const indexed = index.database.prepare(
        `SELECT
           seq, lamport, recorded_at_unix_ms, trace, kind, mark,
           from_kind, from_id, to_kind, to_id,
           artifact_id, artifact_content,
           runtime_run, runtime_checkpoint, runtime_interface
         FROM movements
         WHERE artifact_label = ?
         ORDER BY seq ASC`,
      ).all(label);
      const rows = indexed.map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const tail = await readArtifactLabelLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        label,
      );
      if (tail === null) return null;
      rows.push(...tail);
      return rows.sort(compareMovementOrder);
    } catch (error) {
      preserveProjectionEvidenceError(error);
      return null;
    } finally {
      index.database.close();
    }
  }

  async function readArtifactLabelLedgerTail(ledgerPath, startOffset, expectedSeq, label) {
    const stat = await fs.stat(ledgerPath);
    if (stat.size < startOffset) return null;
    if (stat.size === startOffset) return [];
    const rows = [];
    let pending = Buffer.alloc(0);
    for await (const chunk of fsSync.createReadStream(ledgerPath, { start: startOffset })) {
      const bytes = pending.length ? Buffer.concat([pending, chunk]) : chunk;
      let recordStart = 0;
      for (;;) {
        const newline = bytes.indexOf(0x0a, recordStart);
        if (newline < 0) break;
        const end = newline > recordStart && bytes[newline - 1] === 0x0d ? newline - 1 : newline;
        const raw = bytes.subarray(recordStart, end);
        recordStart = newline + 1;
        if (!raw.length) continue;
        const movement = parseCanonicalMovement(raw, ledgerPath, expectedSeq + 1);
        const seq = indexedUnsignedInteger(movement?.seq);
        if (seq === null || seq !== expectedSeq) return null;
        expectedSeq += 1;
        if (ledgerArtifactLabel(movement) !== label) continue;
        rows.push({
          ...movement,
          kind: movementKindText(movement.kind),
          envelope_id: stringValue(movement.envelope_id ?? movement.id) || `movements:${seq + 1}`,
          source_file: ledgerPath,
          source_line: seq + 1,
        });
      }
      pending = bytes.subarray(recordStart);
    }
    return rows;
  }

  async function clientSurfaceDocumentSnapshot(projection) {
    const documents = [];
    const errors = [];
    const indexedRows = await indexedArtifactLabelRows(projection, "client.surface.document");
    if (indexedRows === null) await refreshLedgerProjection(projection);
    for (const row of indexedRows ?? projection.ledgerRows) {
      const artifactId = ledgerArtifactId(row);
      if (!isAdmittedInterfaceArtifactMovement(row)) continue;
      if (ledgerArtifactLabel(row) !== "client.surface.document") continue;
      if (!artifactId) continue;
      const runId = movementRunId(row);
      const interfaceId = stringValue(objectOrEmpty(row.to).Interface);
      const fromInterfaceId = stringValue(objectOrEmpty(row.from).Interface);
      if (!runId || !interfaceId || !fromInterfaceId || !Number.isSafeInteger(row.seq) || row.seq < 0) {
        errors.push(`${artifactId}: client surface delivery requires an exact run, source and recipient interfaces, and movement sequence`);
        continue;
      }
      let payload;
      try {
        payload = await resolveLedgerArtifactPayload(row);
      } catch {
        documents.push({
          path: `artifact:${artifactId}`,
          digest: `sha256:${crypto.createHash("sha256").update("").digest("hex")}`,
          bytes: 0,
          modifiedAt: timestampOf(row),
          text: "",
          artifactId,
          runId,
          interfaceId,
          fromInterfaceId,
          sequence: row.seq,
        });
        continue;
      }
      if (!isClientSurfaceDocumentPayload(payload)) {
        errors.push(`${artifactId}: client.surface.document does not satisfy octopus.client_surface@1`);
        continue;
      }
      const text = JSON.stringify(payload);
      const bytes = Buffer.byteLength(text, "utf8");
      const digest = crypto.createHash("sha256").update(text).digest("hex");
      documents.push({
        path: `artifact:${artifactId}`,
        digest: `sha256:${digest}`,
        bytes,
        modifiedAt: timestampOf(row),
        text,
        artifactId,
        runId,
        interfaceId,
        fromInterfaceId,
        sequence: row.seq,
      });
    }
    return {
      generation: projectionGeneration,
      root: "artifact://ledger",
      documents,
      errors,
    };
  }

  async function clientActionRows(projection, params) {
    const runId = params.run_id;
    const interfaceId = params.interface_id;
    const afterSeq = params.after_seq ?? -1;
    const labels = new Set(["client.action"]);
    const indexed = await readIndexedInterfaceArtifactRows(
      projection, runId, interfaceId, labels, afterSeq, true,
    );
    if (indexed === null) {
      await refreshLedgerProjection(projection);
      if (projection.invalidLedgerRows.length) {
        throw new ProjectionEvidenceError("Client actions cannot be consumed from malformed ledger evidence.");
      }
    }
    const movements = indexed ?? ledgerRowsForRun(projection, runId);
    const page = movements.filter((row) => matchesInterfaceArtifactMovement(
      row, runId, interfaceId, labels, afterSeq, true,
    ))
      .sort((left, right) => left.seq - right.seq)
      .slice(0, params.limit ?? 128);
    const closures = page.length ? await clientActionTurnClosures(projection, runId, interfaceId, page[0].seq) : [];
    return Promise.all(page.map(async (row) => {
      const identity = {
        artifact_id: row.artifact.id, run_id: runId, seq: row.seq,
        movement_kind: "InterfaceOutbound", from: row.from, to: row.to,
      };
      try {
        return { ...identity, action: await immutableClientAction(row), turn_closure: await clientActionTurnClosure(closures, row.seq) };
      } catch (error) {
        return { ...identity, error: String(error?.message ?? error) };
      }
    }));
  }

  async function clientActionTurnClosures(projection, runId, interfaceId, afterSeq) {
    const labels = new Set(["turn.completed", "turn.interrupted.result", "turn.control.acknowledged"]);
    const indexed = await readIndexedInterfaceArtifactRows(projection, runId, interfaceId, labels, afterSeq, true);
    if (indexed === null) await refreshLedgerProjection(projection);
    return (indexed ?? ledgerRowsForRun(projection, runId))
      .filter((row) => matchesInterfaceArtifactMovement(row, runId, interfaceId, labels, afterSeq, true))
      .sort((left, right) => left.seq - right.seq);
  }

  async function clientActionTurnClosure(closures, actionSeq) {
    for (const row of closures) {
      if (row.seq <= actionSeq) continue;
      const label = ledgerArtifactLabel(row);
      if (label === "turn.interrupted.result") return "stopped";
      const payload = objectOrEmpty(await resolveLedgerArtifactPayload(row));
      if (label === "turn.control.acknowledged") {
        if (["stop", "interrupt"].includes(stringValue(payload.action))) return "stopped";
        continue;
      }
      return stringValue(objectOrEmpty(payload.turn).status).toLowerCase() === "completed" ? "completed" : "stopped";
    }
    return null;
  }

  async function interfaceArtifactPayloadRows(projection, params) {
    const runId = stringValue(params.run_id);
    const interfaceId = stringValue(params.interface_id);
    const labels = new Set(arrayValue(params.labels).map(stringValue).filter(Boolean));
    const afterSeq = numberValue(params.after_seq, -1);
    if (params.order !== undefined && params.order !== "oldest" && params.order !== "newest") {
      throw new Error("interface artifact order must be oldest or newest");
    }
    const newest = params.order === "newest";
    const indexed = await readIndexedInterfaceArtifactRows(
      projection,
      runId,
      interfaceId,
      labels,
      afterSeq,
    );
    if (indexed === null) await refreshLedgerProjection(projection);
    const seen = new Set();
    const selected = [];
    const movements = indexed ?? (runId ? ledgerRowsForRun(projection, runId) : projection.ledgerRows);
    for (const row of movements) {
      if (!matchesInterfaceArtifactMovement(row, runId, interfaceId, labels, afterSeq)) continue;
      const artifactId = ledgerArtifactId(row);
      if (seen.has(artifactId)) continue;
      seen.add(artifactId);
      selected.push(row);
    }
    const ordered = selected.sort((left, right) => numberValue(left.seq, -1) - numberValue(right.seq, -1));
    const page = limitRows(newest ? ordered.reverse() : ordered, params);
    const payloads = await resolveLedgerArtifactPayloadResults(page);
    const artifacts = [];
    for (const row of page) {
      const artifactId = ledgerArtifactId(row);
      const label = ledgerArtifactLabel(row);
      const seq = numberValue(row.seq, -1);
      const coordinates = artifactMovementCoordinates(row);
      const resolved = payloads.get(row);
      if (resolved?.error) {
        artifacts.push({
          artifact_id: artifactId,
          label,
          seq,
          run_id: coordinates.runId,
          interface_id: coordinates.interfaceId,
          error: resolved.error,
        });
        continue;
      }
      artifacts.push({
        artifact_id: artifactId,
        label,
        seq,
        run_id: coordinates.runId,
        interface_id: coordinates.interfaceId,
        movement_kind: stringValue(row.kind),
        created_at: timestampOf(row),
        payload: resolved?.payload ?? null,
      });
    }
    return artifacts.sort((left, right) => newest ? right.seq - left.seq : left.seq - right.seq);
  }

  function matchesInterfaceArtifactMovement(row, runId, interfaceId, labels, afterSeq, outboundRecipientOnly = false) {
    if (outboundRecipientOnly ? row.kind !== "InterfaceOutbound" : !isAdmittedInterfaceArtifactMovement(row)) return false;
    const artifactId = ledgerArtifactId(row);
    const label = outboundRecipientOnly ? row.artifact?.id?.split(":")[0] : ledgerArtifactLabel(row);
    const seq = numberValue(row.seq, -1);
    if (!artifactId || seq <= afterSeq) return false;
    if (labels.size > 0 && !labels.has(label)) return false;
    if (outboundRecipientOnly) {
      return row.invocation_metadata?.runtime?.run === runId && row.to?.Interface === interfaceId;
    }
    const coordinates = artifactMovementCoordinates(row);
    if (runId && coordinates.runId !== runId) return false;
    return !interfaceId || coordinates.interfaceId === interfaceId
      || row.from?.Interface === interfaceId || row.to?.Interface === interfaceId;
  }

  async function admittedArtifactMovement(projection, artifactId, predicate) {
    const indexed = await readIndexedArtifactMovementRows(projection, artifactId);
    if (indexed !== null) return indexed.find(predicate) ?? null;
    await refreshLedgerProjection(projection);
    return projection.ledgerRows.find((row) => (
      ledgerArtifactId(row) === artifactId
      && isAdmittedInterfaceArtifactMovement(row)
      && predicate(row)
    )) ?? null;
  }

  async function readIndexedArtifactMovementRows(projection, artifactId) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const indexedRows = index.database.prepare(
        `SELECT
           seq, lamport, recorded_at_unix_ms, trace, kind, mark,
           from_kind, from_id, to_kind, to_id,
           artifact_id, artifact_content,
           runtime_run, runtime_checkpoint, runtime_interface
         FROM movements
         WHERE artifact_id = ?
         ORDER BY seq ASC`,
      ).all(artifactId).map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const tail = await readArtifactLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        artifactId,
      );
      if (tail === null) return null;
      return [...indexedRows, ...tail]
        .filter(isAdmittedInterfaceArtifactMovement)
        .sort(compareMovementOrder);
    } catch (error) {
      preserveProjectionEvidenceError(error);
      return null;
    } finally {
      index.database.close();
    }
  }

  async function readArtifactLedgerTail(ledgerPath, startOffset, expectedSeq, artifactId) {
    const stat = await fs.stat(ledgerPath);
    if (stat.size < startOffset) return null;
    if (stat.size === startOffset) return [];
    const rows = [];
    let pending = Buffer.alloc(0);
    for await (const chunk of fsSync.createReadStream(ledgerPath, { start: startOffset })) {
      const bytes = pending.length ? Buffer.concat([pending, chunk]) : chunk;
      let recordStart = 0;
      for (;;) {
        const newline = bytes.indexOf(0x0a, recordStart);
        if (newline < 0) break;
        const end = newline > recordStart && bytes[newline - 1] === 0x0d ? newline - 1 : newline;
        const raw = bytes.subarray(recordStart, end);
        recordStart = newline + 1;
        if (!raw.length) continue;
        const movement = parseCanonicalMovement(raw, ledgerPath, expectedSeq + 1);
        const seq = indexedUnsignedInteger(movement?.seq);
        if (seq === null || seq !== expectedSeq) return null;
        expectedSeq += 1;
        if (
          ledgerArtifactId(movement) !== artifactId
          || !isAdmittedInterfaceArtifactMovement(movement)
        ) continue;
        rows.push({
          ...movement,
          kind: movementKindText(movement.kind),
          envelope_id: stringValue(movement.envelope_id ?? movement.id) || `movements:${seq + 1}`,
          source_file: ledgerPath,
          source_line: seq + 1,
        });
      }
      pending = bytes.subarray(recordStart);
    }
    return rows;
  }

  async function readIndexedInterfaceArtifactRows(
    projection,
    runId,
    interfaceId,
    labels,
    afterSeq,
    outboundRecipientOnly = false,
  ) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const predicates = [
        "artifact_id IS NOT NULL",
        outboundRecipientOnly ? "kind = 'InterfaceOutbound'" : "kind IN ('InterfaceArrival', 'InterfaceOutbound')",
        "seq > ?",
      ];
      const values = [afterSeq];
      if (labels.size > 0) {
        predicates.push(`artifact_label IN (${[...labels].map(() => "?").join(", ")})`);
        values.push(...labels);
      }
      if (runId) {
        predicates.push(`(
          runtime_run = ?
        )`);
        values.push(runId);
      }
      if (interfaceId && outboundRecipientOnly) {
        predicates.push("to_kind = 'interface' AND to_id = ?");
        values.push(interfaceId);
      } else if (interfaceId) {
        predicates.push(`(
          runtime_interface = ?
          OR (from_kind = 'interface' AND from_id = ?)
          OR (to_kind = 'interface' AND to_id = ?)
        )`);
        values.push(interfaceId, interfaceId, interfaceId);
      }
      const indexedRows = index.database.prepare(
        `SELECT
           seq, lamport, recorded_at_unix_ms, trace, kind, mark,
           from_kind, from_id, to_kind, to_id,
           artifact_id, artifact_content,
           runtime_run, runtime_checkpoint, runtime_interface
         FROM movements
         WHERE ${predicates.join("\nAND ")}
         ORDER BY seq ASC`,
      ).all(...values).map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const tail = await readInterfaceArtifactLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        runId,
        interfaceId,
        labels,
        afterSeq,
        outboundRecipientOnly,
      );
      if (tail === null) return null;
      const rows = [...indexedRows, ...tail].sort(compareMovementOrder);
      return rows;
    } catch (error) {
      preserveProjectionEvidenceError(error);
      return null;
    } finally {
      index.database.close();
    }
  }

  async function readInterfaceArtifactLedgerTail(
    ledgerPath,
    startOffset,
    expectedSeq,
    runId,
    interfaceId,
    labels,
    afterSeq,
    outboundRecipientOnly = false,
  ) {
    const stat = await fs.stat(ledgerPath);
    if (stat.size < startOffset) return null;
    if (stat.size === startOffset) return [];
    const rows = [];
    let pending = Buffer.alloc(0);
    for await (const chunk of fsSync.createReadStream(ledgerPath, { start: startOffset })) {
      const bytes = pending.length ? Buffer.concat([pending, chunk]) : chunk;
      let recordStart = 0;
      for (;;) {
        const newline = bytes.indexOf(0x0a, recordStart);
        if (newline < 0) break;
        const end = newline > recordStart && bytes[newline - 1] === 0x0d ? newline - 1 : newline;
        const raw = bytes.subarray(recordStart, end);
        recordStart = newline + 1;
        if (!raw.length) continue;
        const movement = parseCanonicalMovement(raw, ledgerPath, expectedSeq + 1);
        const seq = indexedUnsignedInteger(movement?.seq);
        if (seq === null || seq !== expectedSeq) return null;
        expectedSeq += 1;
        const projected = {
          ...movement,
          kind: movementKindText(movement.kind),
          envelope_id: stringValue(movement.envelope_id ?? movement.id) || `movements:${seq + 1}`,
          source_file: ledgerPath,
          source_line: seq + 1,
        };
        if (matchesInterfaceArtifactMovement(
          projected,
          runId,
          interfaceId,
          labels,
          afterSeq,
          outboundRecipientOnly,
        )) rows.push(projected);
      }
      pending = bytes.subarray(recordStart);
    }
    return rows;
  }

  async function readIndexedRunActivityRows(projection, runId, options) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const limit = Math.min(1_000, Math.max(1, numberValue(options.limit, 100)));
      const afterSeq = optionalUnsignedInteger(options.afterSeq);
      const beforeSeq = optionalUnsignedInteger(options.beforeSeq);
      const fromEnd = options.fromEnd === true;
      if (afterSeq !== null && beforeSeq !== null && beforeSeq <= afterSeq) {
        throw new Error("run activity before_seq must be greater than after_seq");
      }
      const basePredicate = `
        artifact_id IS NOT NULL
        AND (
          runtime_run = ?
        )`;
      const predicates = [basePredicate];
      const values = [runId];
      if (afterSeq !== null) {
        predicates.push("seq > ?");
        values.push(afterSeq);
      }
      if (beforeSeq !== null) {
        predicates.push("seq < ?");
        values.push(beforeSeq);
      }
      const predicate = predicates.join("\nAND ");
      const indexedCount = indexedUnsignedInteger(index.database.prepare(
        `SELECT COUNT(*) AS count FROM movements WHERE ${predicate}`,
      ).get(...values)?.count);
      if (indexedCount === null) return null;
      const descending = fromEnd || beforeSeq !== null;
      const indexedRows = index.database.prepare(
        `SELECT
           seq, lamport, recorded_at_unix_ms, trace, kind, mark,
           from_kind, from_id, to_kind, to_id,
           artifact_id, artifact_content,
           runtime_run, runtime_checkpoint, runtime_interface
         FROM movements
         WHERE ${predicate}
         ORDER BY seq ${descending ? "DESC" : "ASC"}
         LIMIT ?`,
      ).all(...values, limit).map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      const tail = await readRunActivityLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        runId,
        { afterSeq, beforeSeq },
      );
      if (tail === null) return null;
      const candidates = [...indexedRows, ...tail.rows].sort(compareMovementOrder);
      const rows = descending
        ? candidates.slice(Math.max(0, candidates.length - limit))
        : candidates.slice(0, limit);
      const indexedBounds = index.database.prepare(
        `SELECT MIN(seq) AS oldest, MAX(seq) AS latest FROM movements WHERE ${basePredicate}`,
      ).get(runId);
      const indexedOldest = indexedUnsignedInteger(indexedBounds?.oldest);
      const indexedLatest = indexedUnsignedInteger(indexedBounds?.latest);
      const oldestSeq = rows.length ? numberValue(rows[0].seq, null) : null;
      const latestSeq = rows.length ? numberValue(rows[rows.length - 1].seq, null) : null;
      const allOldest = [indexedOldest, tail.allOldest].filter((value) => value !== null).reduce((current, value) => current === null ? value : Math.min(current, value), null);
      const allLatest = [indexedLatest, tail.allLatest].filter((value) => value !== null).reduce((current, value) => current === null ? value : Math.max(current, value), null);
      return {
        rows,
        total: indexedCount + tail.count,
        hasOlder: oldestSeq !== null && allOldest !== null && allOldest < oldestSeq,
        hasNewer: latestSeq !== null && allLatest !== null && allLatest > latestSeq,
        oldestSeq,
        latestSeq,
      };
    } finally {
      index.database.close();
    }
  }

  async function readRunActivityRows(projection, runId, options) {
    const indexed = await readIndexedRunActivityRows(projection, runId, options);
    if (indexed !== null) return indexed;
    return readLedgerRunActivityRows(projection, runId, options);
  }

  async function readLedgerRunActivityRows(projection, runId, options) {
    const limit = Math.min(1_000, Math.max(1, numberValue(options.limit, 100)));
    const afterSeq = optionalUnsignedInteger(options.afterSeq);
    const beforeSeq = optionalUnsignedInteger(options.beforeSeq);
    const fromEnd = options.fromEnd === true;
    if (afterSeq !== null && beforeSeq !== null && beforeSeq <= afterSeq) {
      throw new Error("run activity before_seq must be greater than after_seq");
    }
    await refreshLedgerProjection(projection);
    const allRows = ledgerRowsForRun(projection, runId)
      .filter((row) => ledgerArtifactId(row))
      .sort(compareMovementOrder);
    const candidates = allRows.filter((row) => {
      const seq = indexedUnsignedInteger(row.seq);
      if (seq === null) {
        throw new ProjectionEvidenceError("run activity movement has no valid sequence coordinate");
      }
      return (afterSeq === null || seq > afterSeq)
        && (beforeSeq === null || seq < beforeSeq);
    });
    const descending = fromEnd || beforeSeq !== null;
    const rows = descending
      ? candidates.slice(Math.max(0, candidates.length - limit))
      : candidates.slice(0, limit);
    const oldestSeq = rows.length ? numberValue(rows[0].seq, null) : null;
    const latestSeq = rows.length ? numberValue(rows[rows.length - 1].seq, null) : null;
    const allOldest = allRows.length ? numberValue(allRows[0].seq, null) : null;
    const allLatest = allRows.length ? numberValue(allRows[allRows.length - 1].seq, null) : null;
    return {
      rows,
      total: candidates.length,
      hasOlder: oldestSeq !== null && allOldest !== null && allOldest < oldestSeq,
      hasNewer: latestSeq !== null && allLatest !== null && allLatest > latestSeq,
      oldestSeq,
      latestSeq,
    };
  }

  async function readRunActivityLedgerTail(ledgerPath, startOffset, expectedSeq, runId, options) {
    const stat = await fs.stat(ledgerPath);
    if (stat.size < startOffset) return null;
    if (stat.size === startOffset) return { allLatest: null, allOldest: null, count: 0, rows: [] };
    const rows = [];
    let count = 0;
    let allOldest = null;
    let allLatest = null;
    let pending = Buffer.alloc(0);
    for await (const chunk of fsSync.createReadStream(ledgerPath, { start: startOffset })) {
      const bytes = pending.length ? Buffer.concat([pending, chunk]) : chunk;
      let recordStart = 0;
      for (;;) {
        const newline = bytes.indexOf(0x0a, recordStart);
        if (newline < 0) break;
        const end = newline > recordStart && bytes[newline - 1] === 0x0d ? newline - 1 : newline;
        const raw = bytes.subarray(recordStart, end);
        recordStart = newline + 1;
        if (!raw.length) continue;
        const movement = parseCanonicalMovement(raw, ledgerPath, expectedSeq + 1);
        const seq = indexedUnsignedInteger(movement?.seq);
        if (seq === null || seq !== expectedSeq) return null;
        expectedSeq += 1;
        if (!ledgerArtifactId(movement) || !movementBelongsToRun(movement, runId)) continue;
        allOldest = allOldest === null ? seq : Math.min(allOldest, seq);
        allLatest = allLatest === null ? seq : Math.max(allLatest, seq);
        if (
          options.afterSeq !== null && seq <= options.afterSeq
          || options.beforeSeq !== null && seq >= options.beforeSeq
        ) continue;
        count += 1;
        rows.push({
          ...movement,
          kind: movementKindText(movement.kind),
          envelope_id: stringValue(movement.envelope_id ?? movement.id) || `movements:${seq + 1}`,
          source_file: ledgerPath,
          source_line: seq + 1,
        });
      }
      pending = bytes.subarray(recordStart);
    }
    return { allLatest, allOldest, count, rows };
  }

  async function operatorRunActivity(projection, runId, params) {
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const afterSeq = optionalActivityAfterSeq(params.after_seq);
    const beforeSeq = optionalUnsignedInteger(params.before_seq);
    const requestedLimit = Math.min(1_000, Math.max(1, numberValue(params.limit, 100)));
    const fromEnd = params.from_end === true;
    if (afterSeq !== null && beforeSeq !== null && beforeSeq <= afterSeq) {
      throw new Error("run activity before_seq must be greater than after_seq");
    }
    const indexed = await readRunActivityRows(
      projection,
      runId,
      { afterSeq, beforeSeq, fromEnd, limit: requestedLimit },
    );
    const selected = indexed.rows;
    const resolvedPayloads = await resolveLedgerArtifactPayloadResults(selected, { projection: protocol.artifactReadProjection });
    const sidecars = octopusOsInterfaceSidecars(projection);
    const interfaceRoles = new Map(baseInterfaceRows(projection).map((descriptor) => [
      stringValue(descriptor.rule_id ?? descriptor.id),
      stringValue(descriptor.role),
    ]).filter(([interfaceId]) => interfaceId));
    const toolCallOrigins = new Map();
    const activity = [];
    for (const row of selected) {
      const artifactId = ledgerArtifactId(row);
      const label = ledgerArtifactLabel(row);
      if (arrayValue(protocol.hiddenActivityLabels).includes(label)) continue;
      const movementKind = movementKindText(row.kind);
      const from = movementEndpoint(row.from);
      const to = movementEndpoint(row.to);
      const runtime = objectOrEmpty(objectOrEmpty(row.invocation_metadata).runtime);
      const direction = activityDirection(row, runId);
      const interfaceId = direction === "in"
        ? endpointId(row.from, "Interface") || stringValue(runtime.interface)
        : direction === "out"
          ? endpointId(row.to, "Interface") || stringValue(runtime.interface)
          : endpointId(row.from, "Interface") || endpointId(row.to, "Interface") || stringValue(runtime.interface);
      const resolved = resolvedPayloads.get(row);
      const payload = resolved?.payload ?? null;
      const payloadError = resolved?.error ?? "";
      const projectedActivity = operatorChatActivityPayload(payload, label, protocol);
      const eventKind = projectedActivity.eventKind;
      const eventPayload = projectedActivity.payload;
      const sidecar = sidecars.get(interfaceId);
      const role = direction === "out"
        ? "assistant"
        : direction === "in" && sidecar?.component === "client"
          ? "user"
          : "system";
      const sourceInterfaceId = movementBoundaryInterface(row, "from");
      const callId = stringValue(eventPayload?.call_id ?? eventPayload?.correlation_id);
      let activityOrigin = {
        interface_id: sourceInterfaceId,
        interface_role: interfaceRoles.get(sourceInterfaceId) ?? "",
      };
      if (eventKind === "dynamic_tool" && callId) {
        if (stringValue(eventPayload?.status) === "started") {
          toolCallOrigins.set(callId, activityOrigin);
        } else {
          activityOrigin = toolCallOrigins.get(callId) ?? activityOrigin;
        }
      }
      const payloadAlreadyPresented = sidecar?.component === "client";
      const evidenceOnly = payloadAlreadyPresented
        || ["usage", "item_started", "turn_started", "turn_completed"].includes(eventKind);
      const presentedEventKind = evidenceOnly ? "event" : eventKind;
      const movementSeq = numberValue(row.seq, 0);
      const evidenceText = [
        label,
        `${Object.entries(objectOrEmpty(row.from)).map(([kind, id]) => `${kind}:${stringValue(id)}`).find((value) => !value.endsWith(":")) ?? "unknown"} → ${Object.entries(objectOrEmpty(row.to)).map(([kind, id]) => `${kind}:${stringValue(id)}`).find((value) => !value.endsWith(":")) ?? "unknown"}`,
        `movement ${movementSeq}`,
        artifactId ? `artifact ${artifactId}` : "",
      ].filter(Boolean).join(" · ");
      const text = evidenceOnly
        ? evidenceText
        : typeof payload === "string" ? payload : projectedActivity.text;
      activity.push({
        id: `${runId}:activity:${Number.isSafeInteger(row.seq) ? row.seq : `line-${row.source_line}`}:${movementKind}`,
        run_id: runId,
        role,
        label,
        direction,
        event_kind: presentedEventKind,
        event_data: {
          ...operatorChatEventData(eventPayload, eventKind, row),
          _run_activity: true,
          _activity_origin: activityOrigin,
          ...(payloadAlreadyPresented ? { _movement_evidence: true } : {}),
        },
        text,
        error: payloadAlreadyPresented ? false : Boolean(payloadError) || projectedActivity.error || movementKind.startsWith("VerificationFailed") || movementKind.startsWith("DeliveryRefused"),
        terminal: false,
        created_at_ms: rowTimeMs(row),
        movement_seq: movementSeq,
        movement_kind: movementKind,
        from,
        to,
        verification: stringValue(objectOrEmpty(row.invocation_metadata).verification),
        artifact_id: artifactId,
        content_hash: contentHashHex(row?.artifact?.content),
        interface_id: interfaceId,
        attachments: activityAttachments(eventPayload, artifactId, label, rowTimeMs(row)),
        payload_error: payloadError,
      });
    }
    const sourceCursor = await projectionLedgerSourceCursor(
      projection,
      indexed.latestSeq ?? afterSeq ?? -1,
    );
    return {
      run_id: runId,
      total: indexed.total,
      truncated: selected.length < indexed.total,
      has_older: indexed.hasOlder,
      has_newer: indexed.hasNewer,
      oldest_seq: indexed.oldestSeq,
      latest_seq: indexed.latestSeq,
      activity,
      source_cursor: sourceCursor,
    };
  }

  function activityAttachments(payload, artifactId, label, createdAtMs) {
    const attachments = operatorChatAttachments(payload);
    const media = objectOrEmpty(payload.media);
    const mimeType = stringValue(media.mime_type ?? payload.mime_type);
    if (!mimeType) return attachments;
    const mediaAttachment = {
      id: artifactId,
      name: stringValue(media.name ?? payload.name) || label || "Artifact",
      size: numberValue(media.size ?? payload.size, 0),
      type: mimeType,
      label: label || "artifact",
      last_modified: createdAtMs,
    };
    return attachments.some((attachment) => attachment.id === artifactId)
      ? attachments
      : [...attachments, mediaAttachment];
  }

  async function interfaceArtifactMedia(projection, params) {
    const artifactId = requiredString(params.artifact_id, "artifact_id");
    const runId = requiredString(params.run_id, "run_id");
    const interfaceId = requiredString(params.interface_id, "interface_id");
    const row = await admittedArtifactMovement(projection, artifactId, (candidate) => {
      if (ledgerArtifactLabel(candidate) !== "realtime.session.audio") return false;
      const coordinates = artifactMovementCoordinates(candidate);
      return coordinates.runId === runId && coordinates.interfaceId === interfaceId;
    });
    if (!row) {
      throw new Error("The requested realtime audio artifact is not present in the ledger projection.");
    }
    const { runId: rowRunId, interfaceId: rowInterfaceId } = artifactMovementCoordinates(row);
    const payload = await resolveLedgerArtifactPayload(row);
    if (!payload || payload.schema !== "octopus.voice_audio" || payload.schema_version !== 1) {
      throw new Error("The realtime audio artifact does not satisfy octopus.voice_audio@1.");
    }
    const media = objectOrEmpty(payload.media);
    const mediaPath = requiredString(media.path, "media.path");
    const mediaType = requiredString(media.mime_type, "media.mime_type");
    const expectedHash = requiredString(media.content_hash, "media.content_hash").toLowerCase();
    const expectedSize = numberValue(media.size, -1);
    if (!path.isAbsolute(mediaPath)) throw new Error("Realtime audio media.path must be absolute.");
    if (!/^audio\/[A-Za-z0-9.+-]+(?:[;][ -~]*)?$/.test(mediaType)) throw new Error("Realtime audio media.mime_type must be an audio MIME type.");
    if (!/^[0-9a-f]{64}$/.test(expectedHash)) throw new Error("Realtime audio media.content_hash must be a SHA-256 hex digest.");
    const stat = await fs.stat(mediaPath);
    if (!stat.isFile()) throw new Error("Realtime audio media.path is not a regular file.");
    if (stat.size <= 0 || stat.size > INTERFACE_MEDIA_MAX_BYTES) {
      throw new Error(`Realtime audio media size must be between 1 and ${INTERFACE_MEDIA_MAX_BYTES} bytes.`);
    }
    if (expectedSize !== stat.size) throw new Error("Realtime audio media.size does not match the admitted file.");
    const bytes = await fs.readFile(mediaPath);
    const actualHash = crypto.createHash("sha256").update(bytes).digest("hex");
    if (actualHash !== expectedHash) throw new Error("Realtime audio media.content_hash does not match the admitted file.");
    return {
      artifact_id: artifactId,
      run_id: rowRunId,
      interface_id: rowInterfaceId,
      correlation_id: stringValue(payload.correlation_id),
      mime_type: mediaType,
      content_hash: actualHash,
      size: bytes.byteLength,
      base64: bytes.toString("base64"),
    };
  }

  async function admittedArtifactBytes(projection, params) {
    const runId = requiredString(params.run_id, "run_id");
    const byCall = Object.hasOwn(params, "call_id");
    const row = byCall
      ? await admittedToolResultMovement(projection, runId, requiredString(params.call_id, "call_id"))
      : await admittedArtifactMovement(projection, requiredString(params.artifact_id, "artifact_id"), (candidate) => {
        const coordinates = artifactMovementCoordinates(candidate);
        return coordinates.runId === runId && coordinates.interfaceId === requiredString(params.interface_id, "interface_id");
      });
    if (!row) {
      throw new Error(byCall
        ? "The tool result for this call is not admitted in this run."
        : "The requested artifact is not admitted for this run and interface.");
    }
    const artifactId = ledgerArtifactId(row);
    const interfaceId = artifactMovementCoordinates(row).interfaceId;
    const manifestHash = contentHashHex(row?.artifact?.content);
    if (!manifestHash || !row.source_file) throw new Error("The admitted artifact has no content-addressed manifest.");
    const blobRoot = path.join(path.dirname(row.source_file), "blobs");
    const manifestBytes = await readStableContentAddressedArtifactBytes(
      path.join(blobRoot, manifestHash),
      manifestHash,
      ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES,
      "artifact manifest",
    );
    let manifest;
    try {
      manifest = JSON.parse(manifestBytes.toString("utf8"));
    } catch {
      throw new Error("The admitted artifact manifest is not valid JSON.");
    }
    if (!manifest || typeof manifest !== "object" || Array.isArray(manifest)) {
      throw new Error("The admitted artifact manifest is not a JSON object.");
    }
    const label = requiredString(manifest.label, "artifact manifest label");
    if (label !== ledgerArtifactLabel(row) || artifactId !== `${label}:${manifestHash}`) {
      throw new Error("The admitted artifact identity does not match its content-addressed manifest.");
    }
    const payloadHash = requiredContentHash(manifest.payload_content_hash, "artifact manifest payload_content_hash");
    const payloadPath = requiredString(manifest.payload_path, "artifact manifest payload_path");
    if (!path.isAbsolute(payloadPath)) throw new Error("The admitted artifact payload_path must be absolute.");
    const payloadBytes = await readStableContentAddressedArtifactBytes(
      path.join(blobRoot, payloadHash),
      payloadHash,
      ARTIFACT_PREVIEW_MAX_BYTES,
      "artifact payload blob",
    );
    const nested = Object.hasOwn(params, "attachment_id")
      ? await artifactAttachmentBytes(payloadBytes, params.attachment_id)
      : await nestedArtifactMedia(payloadBytes);
    const selected = nested ?? {
      bytes: payloadBytes,
      contentHash: payloadHash,
      filename: artifactPayloadFilename(label, payloadPath, payloadBytes),
      mimeType: artifactPayloadMimeType(payloadBytes),
    };
    return {
      artifact_id: artifactId,
      run_id: runId,
      interface_id: interfaceId,
      ...(Object.hasOwn(params, "attachment_id") ? { attachment_id: params.attachment_id } : {}),
      filename: selected.filename,
      mime_type: selected.mimeType,
      content_hash: selected.contentHash,
      size: selected.bytes.byteLength,
      base64: selected.bytes.toString("base64"),
    };
  }

  async function admittedToolResultMovement(projection, runId, callId) {
    const index = await openValidatedLedgerIndex(projection);
    let rows;
    if (index) {
      try {
        rows = index.database.prepare(
          `SELECT
             seq, lamport, recorded_at_unix_ms, trace, kind, mark,
             from_kind, from_id, to_kind, to_id,
             artifact_id, artifact_content,
             runtime_run, runtime_checkpoint, runtime_interface
           FROM movements
           WHERE runtime_run = ? AND kind = 'InterfaceOutbound' AND artifact_id LIKE '%.result:%'
           ORDER BY seq ASC`,
        ).all(runId).map((row) => normalizeIndexedProjectionMovement(row, index.ledgerPath));
      } finally {
        index.database.close();
      }
    } else {
      await refreshLedgerProjection(projection);
      rows = projection.ledgerRows.filter((row) => (
        movementRunId(row) === runId
        && stringValue(row.kind) === "InterfaceOutbound"
        && ledgerArtifactLabel(row).endsWith(".result")
      ));
    }
    for (const row of rows.filter(isAdmittedInterfaceArtifactMovement)) {
      const payload = objectOrEmpty(await resolveLedgerArtifactPayload(row));
      if (stringValue(payload.call_id ?? payload.correlation_id) === callId) return row;
    }
    return null;
  }

  async function cachedConfigurationProjection(repoRoot) {
    const scope = fileProjectionScope(repoRoot);
    for (;;) {
      if (scope !== fileProjectionScope(repoRoot)) throw new Error("Projection Home changed during configuration read");
      let build = configurationProjectionBuild;
      if (!build || build.scope !== scope) {
        build = { scope, generation: configurationProjectionGeneration, promise: buildConfigurationProjection(repoRoot) };
        configurationProjectionBuild = build;
      }
      try {
        const projection = await build.promise;
        if (scope !== fileProjectionScope(repoRoot)) throw new Error("Projection Home changed during configuration read");
        if (build.generation === configurationProjectionGeneration) return projection;
      } catch (error) {
        if (build.generation === configurationProjectionGeneration) {
          if (configurationProjectionBuild === build) configurationProjectionBuild = null;
          throw error;
        }
      }
      if (configurationProjectionBuild === build) configurationProjectionBuild = null;
    }
  }

  async function buildConfigurationProjection(repoRoot) {
    const warnings = projectionWatchWarnings();
    const homeRoot = octopusHome();
    const durableRoot = resolveDurableRoot();
    const registryRoot = path.join(homeRoot, "registry");
    const stateRoot = path.join(homeRoot, "state");
    const runRecordsRoot = path.join(stateRoot, "runs");
    let hostRootRefs = {};
    let hostManifestError = "";
    try {
      hostRootRefs = await readHostRootRefs(homeRoot);
    } catch (error) {
      hostManifestError = String(error?.message ?? error);
      warnings.push(hostManifestError);
    }
    let launchEnvironment = null;
    try {
      launchEnvironment = normalizeRuntimeLaunchEnvironment(runtimeLaunchEnvironment());
    } catch (error) {
      warnings.push(`Managed runtime launch environment could not be verified: ${String(error?.message ?? error)}`);
    }
    const descriptorEntries = await Promise.all(DESCRIPTOR_REGISTRIES.map(async (registry) => {
      return [registry.kind, await readDescriptorRows(registryRoot, registry, warnings, launchEnvironment)];
    }));
    const descriptors = Object.fromEntries(descriptorEntries);
    const sourceRegistryRoot = resolveSourceRegistry();
    const localPackageRegistryRoot = resolveLocalPackageRegistry();
    const localPackageLibraryRoot = resolveLocalPackageLibrary();
    const localPackages = await readLocalPackageCatalog({
      sourceRegistryRoot: localPackageRegistryRoot,
      activeRegistryRoot: registryRoot,
      libraryRoot: localPackageLibraryRoot,
      stateRoot: path.join(homeRoot, "client", "package-state"),
    });
    warnings.push(...arrayValue(localPackages.errors));
    const setupSourceProviders = await readProviderInterfaceDescriptors(localPackageRegistryRoot);
    const setupInstalledProviders = await readProviderInterfaceDescriptors(registryRoot);
    for (const pack of localPackages.packs) {
      const id = pack.workspace_setup?.home?.provider_interface_id;
      if (!id) continue;
      const owner = pack.components.find((component) => component.product_id === id);
      const providers = owner?.materialized === true || owner?.partially_materialized === true ? setupInstalledProviders : setupSourceProviders;
      pack.setup_provider = providers.find((provider) => provider.id === id) ?? null;
    }
    let clientAgentCatalog = null;
    let clientAgentCatalogError = "";
    try {
      clientAgentCatalog = await readClientAgentCatalog(homeRoot);
      if (!clientAgentCatalog.exists) {
        clientAgentCatalogError = `Client Agent catalog is absent: ${clientAgentCatalog.file}`;
        warnings.push(clientAgentCatalogError);
      }
    } catch (error) {
      clientAgentCatalogError = String(error?.message ?? error);
      warnings.push(clientAgentCatalogError);
    }
    const applicationPackages = await readApplicationPackageCatalog(
      registryRoot,
      localPackageRegistryRoot,
      warnings,
    );
    const descriptorIndexes = Object.fromEntries(Object.entries(descriptors).map(([kind, rows]) => [
      kind,
      new Map(rows.flatMap((row) => {
        const identities = new Set([
          stringValue(row.id),
          stringValue(row.rule_id),
          stringValue(row.name),
          stringValue(row.registry_dir),
        ].filter(Boolean));
        return [...identities].map((identity) => [identity, row]);
      })),
    ]));
    const eeInstances = await readEeInstanceRecords(path.join(stateRoot, "ee_instances"), warnings);
    let eePolicyProjection = { environments: [], issues: [] };
    let eePolicyProjectionError = "";
    try {
      eePolicyProjection = await cachedEePolicyProjection();
      warnings.push(...eePolicyProjection.issues);
    } catch (error) {
      eePolicyProjectionError = String(error?.message ?? error);
      warnings.push(eePolicyProjectionError);
    }
    const operatorChatProtocol = await readOperatorChatProtocol(registryRoot);
    return {
      repoRoot,
      octopusHome: homeRoot,
      registryRoot,
      sourceRegistryRoot,
      localPackageRegistryRoot,
      localPackageLibraryRoot,
      stateRoot,
      runRecordsRoot,
      durableRoot,
      descriptors,
      localPackages,
      clientAgentCatalog,
      clientAgentCatalogError,
      applicationPackages,
      descriptorIndexes,
      operatorChatProtocol,
      eeInstances,
      eePolicyProjection,
      eePolicyProjectionError,
      hostRootRefs,
      hostManifestError,
      warnings,
    };
  }

  async function buildFileProjection(repoRoot) {
    const configuration = await cachedConfigurationProjection(repoRoot);
    const warnings = [...configuration.warnings];
    const runRecordProjection = await readRunRecords(configuration.runRecordsRoot, warnings);
    let workspaceSetup;
    try {
      workspaceSetup = await (options.readWorkspaceSetupState ?? readWorkspaceSetupState)({ octopusHomeRoot: configuration.octopusHome, registryRoot: configuration.registryRoot });
      warnings.push(...arrayValue(workspaceSetup.errors));
    } catch (error) {
      const message = String(error?.message ?? error);
      workspaceSetup = { status: "degraded", receipt: null, errors: [message] };
      warnings.push(message);
    }
    const workerEnvironments = await readWorkerEnvironmentRecords(configuration.octopusHome, warnings, configuration.descriptorIndexes.worker_environment_kind);
    const cachedLedger = ledgerProjectionSnapshot?.durableRoot === configuration.durableRoot
      ? ledgerProjectionSnapshot
      : null;
    return {
      ...configuration,
      workspaceSetup,
      workerEnvironments,
      generatedAt: new Date().toISOString(),
      generation: projectionGeneration,
      ledgerRows: cachedLedger?.rows ?? [],
      ledgerRowsByRunSources: cachedLedger?.rowsByRunSources ?? [],
      invalidLedgerRows: cachedLedger?.invalidRows ?? [],
      ledgerRevision: cachedLedger?.revision ?? "",
      structureMemo: new Map(),
      ledgerMemo: new Map(),
      operatorChatsListCache: null,
      operatorActivityCache: null,
      topologySnapshotCache: null,
      operatorChatMessagesCache: new Map(),
      operatorChatRunIndexCache: null,
      operatorChatStatusCursor: -1,
      operatorChatStatusInitialized: false,
      operatorChatStatusRefreshTail: Promise.resolve(),
      operatorChatTurnStateCache: new Map(),
      runRecords: runRecordProjection.records,
      invalidRunRecords: runRecordProjection.invalidRecords,
      warnings,
    };
  }

  async function cachedEePolicyProjection() {
    if (typeof options.projectEePolicies !== "function") {
      throw new Error("canonical EE policy projector is unavailable");
    }
    while (true) {
      let build = policyProjectionBuild;
      if (!build) {
        const generation = policyProjectionGeneration;
        const promise = options.projectEePolicies().then(validateEePolicyProjectionSnapshot);
        build = { generation, promise };
        policyProjectionBuild = build;
      }
      try {
        const snapshot = await build.promise;
        if (build.generation === policyProjectionGeneration) return snapshot;
      } catch (error) {
        if (build.generation === policyProjectionGeneration) {
          if (policyProjectionBuild === build) policyProjectionBuild = null;
          throw error;
        }
      }
      if (policyProjectionBuild === build) policyProjectionBuild = null;
    }
  }

  async function readWorkerEnvironmentRecords(octopusHomeRoot, warnings = [], kindDescriptors = new Map()) {
    const interfacesRoot = path.join(octopusHomeRoot, "interfaces");
    const files = await listWorkerEnvironmentRecordFiles(interfacesRoot);
    const rows = [];
    for (const file of files) {
      const relativeParts = path.relative(interfacesRoot, file).split(path.sep);
      if (relativeParts.length !== 4
        || !safeInterfacePathSegment(relativeParts[0])
        || relativeParts[1] !== "worker-environments"
        || relativeParts[3] !== "worker_environment.json") {
        warnings.push(`worker environment record is outside the canonical interface store layout: ${file}`);
        continue;
      }
      let record;
      try {
        record = await readJsonFile(file);
      } catch (error) {
        warnings.push(String(error?.message ?? error));
        continue;
      }
      const formation = objectOrEmpty(record?.formation);
      const workerEnvironmentId = stringValue(formation.worker_environment_id);
      const workerEnvironmentKind = stringValue(formation.worker_environment_kind);
      const lifecycle = stringValue(record?.lifecycle).toLowerCase();
      const validFormation = record?.schema_version === 5
        && validWorkerEnvironmentFormation(formation)
        && workerEnvironmentId
        && workerEnvironmentId === path.basename(path.dirname(file))
        && workerEnvironmentKind
        && stringValue(formation.run)
        && stringValue(formation.ee)
        && stringValue(formation.worker)
        && stringValue(formation.door_interface)
        && stringValue(formation.door_interface) === relativeParts[0]
        && ["active", "released", "failed"].includes(lifecycle)
        && Number.isSafeInteger(record.opened_at_movement_seq)
        && Number.isSafeInteger(record.last_activity_movement_seq)
        && record.last_activity_movement_seq >= record.opened_at_movement_seq;
      if (!validFormation) {
        warnings.push(`worker environment record does not satisfy formation schema v5: ${file}`);
        continue;
      }

      const interfaceVarDir = path.join(interfacesRoot, relativeParts[0]);
      const liveStatePath = path.join(path.dirname(file), "live_state.json");
      let liveState = null;
      try {
        const liveStateStat = await fs.lstat(liveStatePath).catch((error) => {
          if (error?.code === "ENOENT") return null;
          throw error;
        });
        if (liveStateStat?.isSymbolicLink()) {
          warnings.push(`worker environment live state must not be a symbolic link: ${liveStatePath}`);
        } else if (liveStateStat?.isFile()) {
          liveState = await readJsonFile(liveStatePath);
        } else if (liveStateStat) {
          warnings.push(`worker environment live state is not a regular file: ${liveStatePath}`);
        }
      } catch (error) {
        warnings.push(String(error?.message ?? error));
      }
      if (liveState !== null && !validWorkerEnvironmentLiveState(
        liveState,
        workerEnvironmentId,
        workerEnvironmentKind,
      )) {
        warnings.push(`worker environment live state does not match its formation: ${liveStatePath}`);
        liveState = null;
      }

      const live = objectOrEmpty(liveState);
      const rootBindings = Object.entries(objectOrEmpty(formation.door_root_bindings));
      const viewer = objectOrEmpty(live.viewer);
      const liveStatus = stringValue(live.status).toLowerCase();
      const liveAvailable = workerEnvironmentLiveAvailable(lifecycle, liveStatus);
      const kindDescriptor = kindDescriptors.get(workerEnvironmentKind);
      const kindCapabilities = arrayValue(kindDescriptor?.capabilities).map(stringValue).filter(Boolean);
      rows.push({
        worker_environment_id: workerEnvironmentId,
        formation_pin: stringValue(formation.formation_pin),
        worker_environment_kind: workerEnvironmentKind,
        kind: workerEnvironmentKind,
        status: lifecycle === "active" ? liveStatus || "not projected" : lifecycle,
        live_available: liveAvailable,
        ee_id: stringValue(formation.ee),
        run_id: stringValue(formation.run),
        worker: stringValue(formation.worker),
        door: stringValue(formation.door_interface),
        substrate_family: stringValue(live.substrate_family),
        root: workerEnvironmentWorkspace(live, rootBindings),
        session_id: workerEnvironmentSession(live),
        transport: stringValue(viewer.protocol),
        capabilities: workerEnvironmentCapabilities(formation, kindCapabilities),
        tools: arrayValue(formation.granted_tools),
        tool_descriptor_hashes: objectOrEmpty(formation.tool_descriptor_hashes),
        client_surfaces: await projectedWorkerEnvironmentSurfaces({
          octopusHomeRoot,
          formation,
          interfaceVarDir,
          lifecycle,
          liveState: live,
          kindCapabilities,
          openedAtMovementSeq: record.opened_at_movement_seq,
          lastActivityMovementSeq: record.last_activity_movement_seq,
          warnings,
        }),
        description: stringValue(live.detail),
        record_path: file,
        live_state_path: liveState ? liveStatePath : "",
        lifecycle,
        opened_at_movement_seq: record.opened_at_movement_seq,
        last_activity_movement_seq: record.last_activity_movement_seq,
        projection_kind: "worker_environment.formation.v5",
      });
    }
    return rows.sort((left, right) => (
      numberValue(right.last_activity_movement_seq, 0) - numberValue(left.last_activity_movement_seq, 0)
      || stringValue(left.worker_environment_id).localeCompare(stringValue(right.worker_environment_id))
    ));
  }

  async function projectedWorkerEnvironmentSurfaces(input) {
    const { formation, interfaceVarDir, kindCapabilities, lifecycle, liveState, openedAtMovementSeq, lastActivityMovementSeq, warnings } = input;
    const id = stringValue(formation.worker_environment_id);
    const workerEnvironmentKind = stringValue(formation.worker_environment_kind);
    const viewer = objectOrEmpty(liveState.viewer);
    const protocol = stringValue(viewer.protocol).toLowerCase();
    const address = stringValue(viewer.address);
    const status = lifecycle === "active" ? stringValue(liveState.status) || "not projected" : lifecycle;
    const rootBindings = Object.entries(objectOrEmpty(formation.door_root_bindings));
    const workspace = workerEnvironmentWorkspace(liveState, rootBindings);
    const connection = {
      id: `${id}:connection`,
      kind: "connection",
      label: "Connection",
      mode: "evidence",
      status,
      protocol: "worker_environment.formation.v5",
      read_only: true,
      data: {
        name: id,
        provider: stringValue(formation.door_interface),
        address,
        transport: protocol,
        workspace,
        persistence: formation.requires_release === true ? "Explicit release" : "Runtime lifecycle",
        state: status,
        sync: Number.isFinite(liveState.generation) ? `Generation ${liveState.generation}` : "Not projected",
        health: stringValue(liveState.detail) || (status === "error" || status === "failed" ? "Failed" : "Projected"),
        capabilities: workerEnvironmentCapabilities(formation, kindCapabilities),
        events: [
          { state: "active", label: "Environment formed", detail: `Movement ${openedAtMovementSeq}`, time: `seq ${openedAtMovementSeq}` },
          { state: status, label: lifecycle === "active" ? "Last activity" : `Environment ${lifecycle}`, detail: stringValue(liveState.detail), time: `seq ${lastActivityMovementSeq}` },
        ],
      },
    };
    const baseSurfaces = [
      connection,
      ...projectedFilesystemRootSurfaces({ formation, id, lifecycle, rootBindings, status }),
      ...await projectedGitSurfaces({ id, kindCapabilities, lifecycle, rootBindings, status }),
      ...projectedWorktreeStatusSurfaces({ id, liveState, status }),
    ];
    const activeCells = arrayValue(liveState.active_cells);
    const pendingToolCalls = arrayValue(liveState.pending_tool_calls);
    const runtimeDetail = stringValue(liveState.detail);
    if (!protocol && (activeCells.length || pendingToolCalls.length || runtimeDetail)) {
      return [...baseSurfaces, {
        id: `${id}:runtime-activity`, kind: "runtime", label: "Runtime activity", mode: "evidence", status,
        protocol: "worker_environment.live_state.v1", read_only: true,
        data: {
          active_cells: activeCells,
          pending_tool_calls: pendingToolCalls,
          detail: runtimeDetail,
        },
      }];
    }
    if (!protocol) return baseSurfaces;

    if (protocol === "browser.native.v1") {
      const engineHandle = stringValue(viewer.engine_handle);
      const hostGeneration = stringValue(viewer.host_generation);
      const browserContextId = stringValue(viewer.browser_context_id);
      const primaryTargetId = stringValue(viewer.target_id);
      let downloads = null;
      try {
        if (!nativeBrowserDownloadsForProjection) throw new Error("The native download history host is not connected.");
        const history = await nativeBrowserDownloadsForProjection({ octopusHome: input.octopusHomeRoot,
          worker_environment_id: id, formation_pin: stringValue(formation.formation_pin) });
        const historySupported = history?.engine?.capabilities?.persistentDownloadHistory;
        if (typeof historySupported !== "boolean") throw new Error("The native download history host returned no history capability declaration.");
        if (historySupported) {
          if (!stringValue(history.host_generation) || !history.downloads || history.downloads.available === false) throw new Error("The native download history host returned no exact available history owner.");
          downloads = { id: `${id}:downloads`, kind: "downloads", label: "Downloads", mode: "direct", status,
            protocol: "browser.native.downloads.v1", read_only: false,
            data: { formation_pin: stringValue(formation.formation_pin), host_generation: history.host_generation, downloads: history.downloads } };
        }
      } catch (error) { warnings.push(`Native browser download history ${id}: ${String(error?.message ?? error)}`); }
      const historySurfaces = () => downloads ? [downloads] : [];
      const unavailable = (reason, reasonCode = "unavailable") => {
        const recordedTabs = arrayValue(liveState.tabs);
        const ids = new Set();
        const exactTabs = browserContextId && recordedTabs.length && recordedTabs.every((tab) => {
          const targetId = stringValue(tab?.target_id);
          if (!targetId || ids.has(targetId) || stringValue(tab?.browser_context_id) !== browserContextId) return false;
          ids.add(targetId); return true;
        }) ? recordedTabs : [];
        const pages = exactTabs.length ? exactTabs : [null];
        return [...baseSurfaces, ...historySurfaces(), ...pages.map((tab) => ({
          ...unavailableViewerSurface({ id, label: "Browser", protocol, status: "unavailable", reason }),
          ...(tab ? { id: `${id}:browser:${encodeURIComponent(tab.target_id)}` } : {}),
          kind: "browser",
          label: (tab ? stringValue(tab.title) || stringValue(tab.url) : stringValue(liveState.title)) || "Browser",
          data: {
            detail: reason,
            reason_code: reasonCode,
            last_known_url: tab ? stringValue(tab.url) : stringValue(liveState.url),
            last_known_title: tab ? stringValue(tab.title) : stringValue(liveState.title),
          },
        }))];
      };
      if (lifecycle !== "active" || !engineHandle || !hostGeneration || !browserContextId || !stringValue(formation.formation_pin)) {
        return unavailable(lifecycle !== "active" ? `The browser room is ${lifecycle}; its native pages are no longer active.`
          : "The browser room has not projected its exact native engine and context.");
      }
      let observation;
      try {
        if (!nativeBrowserTargetsForProjection) throw new Error("The native browser host is not connected.");
        observation = await nativeBrowserTargetsForProjection({
          octopusHome: input.octopusHomeRoot,
          worker_environment_id: id, formation_pin: stringValue(formation.formation_pin),
          engine_handle: engineHandle, host_generation: hostGeneration, browser_context_id: browserContextId,
        });
      } catch (error) {
        return unavailable(String(error?.message ?? error), stringValue(error?.code) || "unavailable");
      }
      if (!observation || !Array.isArray(observation.targets)) return unavailable("The native browser host returned no page inventory.");
      const tabs = observation.targets;
      const targets = new Set();
      const validTabs = tabs.every((tab) => {
        const targetId = stringValue(tab?.target_id);
        if (!targetId || targets.has(targetId) || stringValue(tab?.browser_context_id) !== browserContextId) return false;
        targets.add(targetId);
        return true;
      });
      if (!validTabs) return unavailable("The native browser page inventory does not match its exact context.");
      if (downloads && downloads.data.host_generation === hostGeneration) Object.assign(downloads.data, { engine_handle: engineHandle, browser_context_id: browserContextId });
      if (!tabs.length) return [...baseSurfaces, ...historySurfaces()];
      const openIntent = observation.openIntent;
      if (openIntent != null && (!Number.isSafeInteger(openIntent.id) || openIntent.id <= 0
        || !targets.has(openIntent.targetId) || !["popup", "context-menu", "pip", "tool"].includes(openIntent.source) || typeof openIntent.activate !== "boolean")) {
        return unavailable("The native browser presentation request does not match its page inventory.");
      }
      return [...baseSurfaces, ...historySurfaces(), ...tabs.map((tab) => ({
        id: `${id}:browser:${encodeURIComponent(tab.target_id)}`,
        kind: "browser", label: stringValue(tab.title) || stringValue(tab.url) || "Browser", mode: "direct", status,
        protocol, read_only: false,
        data: {
          formation_pin: stringValue(formation.formation_pin),
          engine_handle: engineHandle,
          host_generation: hostGeneration,
          browser_context_id: browserContextId,
          target_id: tab.target_id,
          primary_target_id: primaryTargetId,
          observation_source: "client.native_browser",
          downloads: observation.downloads ?? null,
          native_open_intent: openIntent?.targetId === tab.target_id ? openIntent : null,
          browser_preview: typeof options.nativeBrowserPresentationsForProjection === "function" ? options.nativeBrowserPresentationsForProjection(id).find(value => value.target_id === tab.target_id) ?? null : null,
          presentation_intent: liveState.presentation_intent?.target_id === tab.target_id ? liveState.presentation_intent : null,
          opened_at_movement_seq: openedAtMovementSeq,
          url: stringValue(tab.url),
          title: stringValue(tab.title),
          lifetime: tab.lifetime,
          model_controlled: tab.model_controlled,
          disposition: tab.disposition,
          activity: tab.target_id === primaryTargetId ? liveState.activity ?? null : null,
          tabs,
          last_capture: tab.target_id === primaryTargetId ? liveState.last_capture ?? null : null,
        },
      }))];
    }

    if (protocol === "cdp") {
      if (lifecycle !== "active" || !stringValue(viewer.target_id ?? liveState.target_id) || !validAddressWithProtocols(address, ["http:", "https:", "ws:", "wss:"])) {
        return [...baseSurfaces, unavailableViewerSurface({
          id,
          label: "Browser",
          protocol,
          reason: lifecycle !== "active"
            ? `The browser room is ${lifecycle}; its interactive viewer is no longer active.`
            : "The browser room must project an exact target and a supported CDP endpoint.",
          status,
        })];
      }
      return [...baseSurfaces, {
        id: `${id}:browser`, kind: "browser", label: "Browser", mode: "direct", status,
        viewer_url: address, protocol, read_only: false,
        data: {
          url: stringValue(liveState.url),
          title: stringValue(liveState.title),
          target_id: stringValue(viewer.target_id ?? liveState.target_id),
          opened_at_movement_seq: openedAtMovementSeq,
          activity: liveState.activity ?? null,
          tabs: arrayValue(liveState.tabs),
          last_capture: liveState.last_capture ?? null,
        },
      }];
    }
    if (protocol === "desktop_session") {
      if (!["active", "failed"].includes(lifecycle) || !validHttpAddress(address)) {
        return [...baseSurfaces, unavailableViewerSurface({
          id,
          label: "Desktop",
          protocol,
          reason: !["active", "failed"].includes(lifecycle)
            ? `The desktop room is ${lifecycle}; its interactive viewer is no longer active.`
            : "The projected desktop viewer is not an embeddable absolute HTTP(S) URL.",
          status,
        })];
      }
      return [...baseSurfaces, {
        id: `${id}:desktop`, kind: "desktop", label: "Desktop", mode: "direct", status,
        viewer_url: address, protocol, read_only: false,
        data: {
          detail: stringValue(liveState.detail),
          desktop: liveState.desktop ?? null,
          opened_at_movement_seq: openedAtMovementSeq,
          last_activity_movement_seq: lastActivityMovementSeq,
          formation_pin: stringValue(formation.formation_pin),
          host_generation: stringValue(viewer.host_generation),
          native_presentations: typeof options.nativeDesktopPresentationsForProjection === "function" ? options.nativeDesktopPresentationsForProjection(id) : [],
        },
      }];
    }
    if (protocol === "remote_exec_server") {
      return [...baseSurfaces, {
        id: `${id}:remote-exec`, kind: "runtime", label: "Remote environment", mode: "evidence", status,
        protocol, read_only: true,
        data: {
          detail: "The projected endpoint speaks the structured exec-server protocol. An interactive terminal requires a separate xterm.websocket surface descriptor.",
          server: {
            transport: "Exec-server JSON-RPC",
            address,
            workspace,
          },
        },
      }];
    }
    if (protocol === "xterm.websocket") {
      if (lifecycle !== "active" || !validWebSocketAddress(address)) {
        return [...baseSurfaces, unavailableViewerSurface({
          id,
          label: "Terminal",
          protocol,
          reason: lifecycle !== "active"
            ? `The terminal room is ${lifecycle}; its interactive stream is no longer active.`
            : "The projected terminal stream is not an absolute WebSocket URL.",
          status,
        })];
      }
      return [...baseSurfaces, {
        id: `${id}:terminal`, kind: "terminal", label: "Terminal", mode: "direct", status,
        viewer_url: address, protocol, read_only: false,
        data: { workspace },
      }];
    }
    if (protocol === "process_session.direct.v1") {
      const authFile = stringValue(viewer.auth_file);
      const tty = liveState.tty === true;
      const hasProcess = Boolean(stringValue(liveState.process_id));
      const initialShellAvailable = !hasProcess && viewer.start_new_shell_available === true;
      const validDirectCoordinates = workerEnvironmentKind === "process_session"
        && lifecycle === "active"
        && Number.isSafeInteger(liveState.generation)
        && liveState.generation >= 0
        && typeof liveState.tty === "boolean"
        && path.isAbsolute(address)
        && path.isAbsolute(authFile)
        && isPathInside(interfaceVarDir, address)
        && isPathInside(interfaceVarDir, authFile);
      let validDirectEndpoint = false;
      if (validDirectCoordinates) {
        try {
          const [socketStat, authStat, resolvedInterfaceVarDir, resolvedSocketParent, resolvedAuth] = await Promise.all([
            fs.lstat(address),
            fs.lstat(authFile),
            fs.realpath(interfaceVarDir),
            fs.realpath(path.dirname(address)),
            fs.realpath(authFile),
          ]);
          validDirectEndpoint = socketStat.isSocket()
            && !socketStat.isSymbolicLink()
            && authStat.isFile()
            && !authStat.isSymbolicLink()
            && isPathInside(resolvedInterfaceVarDir, resolvedSocketParent)
            && isPathInside(resolvedInterfaceVarDir, resolvedAuth);
        } catch {
          validDirectEndpoint = false;
        }
      }
      if (!validDirectEndpoint) {
        return [...baseSurfaces, unavailableViewerSurface({
          id,
          label: "Terminal",
          protocol,
          reason: "The process room did not project an exact private viewer endpoint.",
          status,
        })];
      }
      return [...baseSurfaces, {
        id: `${id}:terminal`, kind: "terminal", label: tty || !hasProcess ? "Terminal" : "Output", mode: "direct", status,
        viewer_url: address, protocol, read_only: !tty && !initialShellAvailable,
        data: {
          workspace,
          auth_file: authFile,
          worker_environment_id: id,
          generation: liveState.generation,
          process_id: stringValue(liveState.process_id),
          process_status: stringValue(liveState.status),
          tty,
          exit_code: Number.isSafeInteger(liveState.exit_code) ? liveState.exit_code : null,
          closed: liveState.closed === true,
          failure: stringValue(liveState.failure),
          warning: stringValue(viewer.warning),
          restart_available: viewer.restart_available === true,
          start_new_shell_available: viewer.start_new_shell_available === true,
          launch_label: stringValue(viewer.launch_label),
        },
      }];
    }
    if (protocol === "terminal_transcript") {
      const transcript = await projectedTextFile(address, interfaceVarDir, warnings, "terminal transcript");
      return [...baseSurfaces, {
        id: `${id}:terminal`, kind: "terminal", label: "Terminal", mode: "evidence", status,
        protocol, read_only: true,
        data: {
          lines: transcript ? transcript.split(/\r?\n/) : [],
          workspace,
          ...(transcript ? {} : { unavailable: "The projected transcript could not be read." }),
        },
      }];
    }
    if (protocol === "container_runtime") {
      const container = objectOrEmpty(liveState.container);
      return [...baseSurfaces, {
        id: `${id}:container`, kind: "container", label: "Runtime", mode: "evidence", status,
        protocol, read_only: true,
        data: {
          name: stringValue(container.name ?? viewer.container_name) || id,
          image: stringValue(container.image),
          state: stringValue(container.status) || status,
          running: container.running === true,
          health: stringValue(container.health),
          exit_code: Number.isSafeInteger(container.exit_code) ? container.exit_code : null,
          uptime: stringValue(container.uptime),
          processes: arrayValue(container.processes),
          ports: arrayValue(container.ports),
          mounts: arrayValue(container.mounts),
          raw: container,
        },
      }];
    }
    if (protocol === "mcp_stdio_room") {
      const eventLog = await projectedTextFile(stringValue(viewer.event_log), interfaceVarDir, warnings, "MCP event log");
      const stderrLog = await projectedTextFile(stringValue(viewer.stderr_log), interfaceVarDir, warnings, "MCP stderr log");
      return [...baseSurfaces, {
        id: `${id}:mcp`, kind: "runtime", label: "MCP server", mode: "evidence", status,
        protocol, read_only: true,
        data: {
          server: objectOrEmpty(liveState.server),
          event_log: eventLog,
          stderr_log: stderrLog,
          error: stringValue(liveState.error),
        },
      }];
    }
    if (["preview", "site_preview", "http_preview"].includes(protocol) && validHttpAddress(address)) {
      if (lifecycle !== "active") {
        return [...baseSurfaces, unavailableViewerSurface({
          id,
          label: "Preview",
          protocol,
          reason: `The preview room is ${lifecycle}; its interactive viewer is no longer active.`,
          status,
        })];
      }
      return [...baseSurfaces, {
        id: `${id}:preview`, kind: "preview", label: "Preview", mode: "direct", status,
        viewer_url: address, protocol, read_only: true,
        data: {
          title: stringValue(liveState.title) || "Application preview",
          detail: stringValue(liveState.detail),
        },
      }];
    }
    return [...baseSurfaces, {
      id: `${id}:runtime`, kind: "runtime", label: "Runtime", mode: "evidence", status,
      protocol, read_only: true, data: liveState,
    }];
  }

  async function readDescriptorRows(registryRoot, registry, warnings = [], launchEnvironment = null, names = null) {
    const root = path.join(registryRoot, registry.kind);
    let entries;
    try {
      entries = await fs.readdir(root, { withFileTypes: true });
    } catch (error) {
      if (error?.code === "ENOENT") return [];
      throw error;
    }
    const transferReadiness = new Map();
    const rows = await mapConcurrentOrdered(
      entries.filter((entry) => entry.isDirectory() && (!names || names.has(entry.name))),
      32,
      async (entry) => {
        const descriptorPath = path.join(root, entry.name, registry.file);
        let descriptor;
        try {
          descriptor = JSON.parse(await fs.readFile(descriptorPath, "utf8"));
        } catch (error) {
          if (error?.code === "ENOENT") return null;
          warnings.push(`descriptor parse failed: ${descriptorPath}: ${error.message}`);
          return null;
        }
        let row = normalizeDescriptorRow(registryRoot, registry, entry.name, descriptorPath, descriptor);
        if (registry.kind === "interface") {
          const octopusOsPath = path.join(root, entry.name, "octopus_os.json");
          try {
            const octopusOs = await readOptionalRegularJsonDocument(
              octopusOsPath,
              `Octopus OS sidecar ${entry.name}`,
            );
            if (octopusOs) {
              row.octopus_os_sidecar = {
                ...parseOctopusOsSidecar(octopusOs.document, entry.name, octopusOsPath),
                source_digest: octopusOs.digest,
              };
            }
          } catch (error) {
            row.octopus_os_sidecar = null;
            warnings.push(String(error?.message ?? error));
          }
          const applicationPath = path.join(root, entry.name, "application.json");
          try {
            const application = await readOptionalRegularJsonDocument(
              applicationPath,
              `application manifest ${entry.name}`,
            );
            if (application) {
              const manifest = parseApplicationManifest(
                application.document,
                entry.name,
                applicationPath,
              );
              row.application_manifest = {
                ...manifest,
                logo_data_url: await readApplicationLogoAsset(
                  path.join(root, entry.name),
                  manifest.logo,
                  `application logo ${entry.name}`,
                ),
                logo_dark_data_url: manifest.logo_dark
                  ? await readApplicationLogoAsset(
                      path.join(root, entry.name),
                      manifest.logo_dark,
                      `application dark logo ${entry.name}`,
                    )
                  : "",
              };
              row.application_manifest_path = applicationPath;
              row.application_manifest_digest = application.digest;
            }
          } catch (error) {
            row.application_manifest = null;
            warnings.push(String(error?.message ?? error));
          }
          try {
            row.interface_private_configuration = await readInterfacePrivateConfigurationProjection({
              descriptor,
              interfaceId: entry.name,
              interfaceRegistryDirectory: path.join(root, entry.name),
              interfacePrivateDirectory: path.join(octopusHome(), "interfaces", entry.name),
              launchEnvironment,
            });
          } catch (error) {
            const message = String(error?.message ?? error);
            row.interface_private_configuration = {
              interface_id: entry.name,
              status: "invalid",
              document_status: "invalid",
              document_digest: "",
              fields: [],
              error: message,
            };
            warnings.push(message);
          }
          const activationPath = path.join(root, entry.name, "activation.json");
          const activation = await readJsonFile(activationPath);
          row.activation_sidecars = activation ? [{
            ...activation,
            descriptor_path: activationPath,
            outbound_interface: stringValue(descriptor.outbound?.interface),
          }] : [];
          if (
            stringValue(descriptor.interface_type).toLowerCase() === "system"
            && stringValue(descriptor.role).toLowerCase() === "worker"
          ) {
            const workerPolicyPath = path.join(root, entry.name, "policy.json");
            row.worker_policy_path = workerPolicyPath;
            try {
              const workerPolicy = await readJsonFile(workerPolicyPath);
              row.worker_policy = workerPolicy;
              if (!workerPolicy) {
                row.worker_policy_status = "missing";
              } else if (
                !workerPolicy
                || typeof workerPolicy !== "object"
                || Array.isArray(workerPolicy)
                || stringValue(workerPolicy.id) !== stringValue(row.rule_id)
                || !workerPolicy.ceiling
                || typeof workerPolicy.ceiling !== "object"
                || Array.isArray(workerPolicy.ceiling)
              ) {
                row.worker_policy_status = "invalid";
                warnings.push(`worker policy is malformed or does not match its interface: ${workerPolicyPath}`);
              } else {
                row.worker_policy_status = "registered";
              }
            } catch (error) {
              row.worker_policy = null;
              row.worker_policy_status = "invalid";
              warnings.push(String(error?.message ?? error));
            }
            const clientTemplatePath = path.join(root, entry.name, "client_template.json");
            row.client_worker_template_path = clientTemplatePath;
            try {
              const clientTemplate = await readOptionalRegularJsonDocument(
                clientTemplatePath,
                `client worker template ${entry.name}`,
              );
              row.client_worker_template = clientTemplate
                ? parseClientWorkerTemplate(clientTemplate.document, entry.name, clientTemplatePath)
                : null;
              row.client_worker_template_digest = clientTemplate?.digest ?? "";
              row.client_worker_template_status = clientTemplate ? "registered" : "absent";
            } catch (error) {
              row.client_worker_template = null;
              row.client_worker_template_digest = "";
              row.client_worker_template_status = "invalid";
              warnings.push(String(error?.message ?? error));
            }
          }
          row = await attachInterfaceArtifactTransferProjection(row, transferReadiness);
        }
        return row;
      },
    );
    return rows
      .filter(Boolean)
      .sort((a, b) => stringValue(a.name ?? a.id).localeCompare(stringValue(b.name ?? b.id)));
  }

  async function attachInterfaceArtifactTransferProjection(row, readinessCache) {
    const transfer = await interfaceArtifactTransferProjection(octopusHome(), row, readinessCache);
    return {
      ...row,
      artifact_transfer: transfer,
      artifact_transfer_surface_status: transfer.status,
      artifact_transfer_path: stringValue(transfer.path),
      artifact_transfer_reason: stringValue(transfer.reason),
      activation_transport_refs: transfer.activation_refs,
    };
  }

  function resolveDurableRoot() {
    return path.join(octopusHome(), "ledger");
  }

  const ledgerCursors = new Map();
  const resolvedMovementGrants = new Map();
  const resolvedArtifactPayloads = new Map();
  let resolvedArtifactPayloadBytes = 0;
  let ledgerProjectionReadTail = Promise.resolve();
  let ledgerProjectionSnapshot = null;

  function readLedgerProjection(durableRoot, warnings = []) {
    const read = ledgerProjectionReadTail.then(() => (
      readLedgerProjectionUnlocked(durableRoot, warnings)
    ));
    ledgerProjectionReadTail = read.then(() => undefined, () => undefined);
    return read;
  }

  async function readLedgerProjectionUnlocked(durableRoot, warnings) {
    const files = [path.join(durableRoot, "movements.jsonl")];
    const cursors = [];
    const revisions = [];
    for (const file of files) {
      const cursor = await advanceLedgerCursor(file, warnings);
      if (cursor) cursors.push(cursor);
      revisions.push([
        file,
        cursor?.dev ?? null,
        cursor?.ino ?? null,
        cursor?.offset ?? null,
        cursor?.lineBase ?? null,
        cursor?.rows.length ?? null,
      ]);
    }
    const revision = JSON.stringify(revisions);
    if (
      ledgerProjectionSnapshot
      && ledgerProjectionSnapshot.durableRoot === durableRoot
      && ledgerProjectionSnapshot.revision === revision
    ) return ledgerProjectionSnapshot;
    const rows = cursors.length === 1 ? cursors[0].rows : cursors.flatMap((cursor) => cursor.rows);
    const rowsByRunSources = cursors.map((cursor) => cursor.rowsByRun);
    const invalidRows = cursors.length === 1
      ? cursors[0].invalidRows
      : cursors.flatMap((cursor) => cursor.invalidRows);
    ledgerProjectionSnapshot = { durableRoot, rows, rowsByRunSources, invalidRows, revision };
    return ledgerProjectionSnapshot;
  }

  async function advanceLedgerCursor(file, warnings = []) {
    let stat;
    try {
      stat = await fs.stat(file);
    } catch (error) {
      if (error?.code === "ENOENT") {
        ledgerCursors.delete(file);
        return null;
      }
      throw error;
    }
    let cursor = ledgerCursors.get(file);
    if (
      !cursor
      || cursor.dev !== stat.dev
      || cursor.ino !== stat.ino
      || stat.size < cursor.offset
    ) {
      cursor = {
        dev: stat.dev,
        ino: stat.ino,
        offset: 0,
        lineBase: 0,
        rows: [],
        rowsByRun: new Map(),
        invalidRows: [],
      };
    }
    if (stat.size === cursor.offset) {
      ledgerCursors.set(file, cursor);
      return cursor;
    }
    let pending = [];
    let pendingLength = 0;
    for await (const chunk of fsSync.createReadStream(file, { start: cursor.offset, end: stat.size - 1 })) {
      let start = 0;
      for (;;) {
        const newline = chunk.indexOf(0x0a, start);
        if (newline < 0) break;
        const fragment = chunk.subarray(start, newline);
        const bytes = pendingLength ? Buffer.concat([...pending, fragment], pendingLength + fragment.length) : fragment;
        const line = bytes.toString("utf8");
        cursor.offset += bytes.length + 1;
        cursor.lineBase += 1;
        start = newline + 1;
        pending = [];
        pendingLength = 0;
        if (line.trim()) parseLedgerLine(line, file, cursor.lineBase, cursor, warnings);
      }
      if (start < chunk.length) {
        const remainder = chunk.subarray(start);
        pending.push(remainder);
        pendingLength += remainder.length;
      }
    }
    ledgerCursors.set(file, cursor);
    return cursor;
  }

  function parseLedgerLine(line, file, lineNumber, cursor, warnings) {
    const index = lineNumber - 1;
    let row;
    try {
      row = parseCanonicalMovement(line, file, index + 1);
    } catch (error) {
      const message = String(error?.message ?? error);
      warnings.push(message);
      cursor.invalidRows.push({ source_path: file, source_line: index + 1, error: message });
      return;
    }
    const kind = movementKindText(row.kind ?? row.movement_class ?? row.type ?? row.event) || "ledger.row";
    const projected = {
      ...row,
      envelope_id: stringValue(row.envelope_id ?? row.id) || `${path.basename(file, ".jsonl")}:${index + 1}`,
      kind,
      source_file: file,
      source_line: index + 1,
    };
    cursor.rows.push(projected);
    const runId = movementRunId(projected);
    if (runId) {
      const runRows = cursor.rowsByRun.get(runId) ?? [];
      runRows.push(projected);
      cursor.rowsByRun.set(runId, runRows);
    }
  }

  function parseCanonicalMovement(source, file, lineNumber) {
    let row;
    try {
      row = JSON.parse(Buffer.isBuffer(source) ? source.toString("utf8") : source);
    } catch (error) {
      throw new ProjectionEvidenceError(`ledger movement JSON is invalid: ${file}:${lineNumber}: ${error.message}`);
    }
    const grantContent = decodeDurableMovementGrant(row, file, lineNumber);
    const movement = validateCanonicalMovement(row, file, lineNumber);
    if (grantContent) {
      delete movement.invocation_metadata.grant;
      movement.invocation_metadata.grant_content = grantContent;
    }
    return movement;
  }

  function decodeDurableMovementGrant(row, file, lineNumber) {
    const metadata = row?.invocation_metadata;
    if (!metadata || typeof metadata !== "object" || Array.isArray(metadata)) return "";
    const hasGrant = Object.prototype.hasOwnProperty.call(metadata, "grant");
    const hasGrantContent = Object.prototype.hasOwnProperty.call(metadata, "grant_content");
    if (!hasGrantContent) return "";
    if (hasGrant) {
      throw new ProjectionEvidenceError(
        `ledger movement metadata carries both inline and content-addressed grant: ${file}:${lineNumber}`,
      );
    }
    const content = metadata.grant_content;
    if (typeof content !== "string" || !/^[0-9a-f]{64}$/.test(content)) {
      throw new ProjectionEvidenceError(
        `ledger movement grant content address is not canonical BLAKE3: ${file}:${lineNumber}`,
      );
    }
    const metadataRoot = path.join(path.dirname(file), "metadata");
    const cacheKey = `${metadataRoot}:${content}`;
    let grant = resolvedMovementGrants.get(cacheKey);
    if (grant === undefined) {
      const grantPath = path.join(metadataRoot, content);
      let bytes;
      try {
        bytes = fsSync.readFileSync(grantPath);
      } catch (error) {
        throw new ProjectionEvidenceError(
          `ledger movement grant content is unreadable: ${grantPath}: ${error.message}`,
        );
      }
      const actual = Buffer.from(blake3(bytes)).toString("hex");
      if (actual !== content) {
        throw new ProjectionEvidenceError(
          `ledger movement grant content integrity mismatch: ${grantPath}: expected ${content}, got ${actual}`,
        );
      }
      try {
        grant = JSON.parse(bytes.toString("utf8"));
      } catch (error) {
        throw new ProjectionEvidenceError(
          `ledger movement grant content JSON is invalid: ${grantPath}: ${error.message}`,
        );
      }
      resolvedMovementGrants.set(cacheKey, grant);
    }
    delete metadata.grant_content;
    metadata.grant = grant;
    return content;
  }


  async function resolveLedgerArtifactPayload(row, readOptions = {}) {
    const hex = contentHashHex(row && row.artifact && row.artifact.content);
    if (!hex || !row.source_file) return null;
    const contract = Object.hasOwn(readOptions, "projection")
      ? artifactReadProjectionForLabel(readOptions.projection, ledgerArtifactLabel(row)) : null;
    const cacheKey = `${row.source_file}:${hex}:${contract ? JSON.stringify([contract, ledgerArtifactId(row)]) : "raw"}`;
    const cached = resolvedArtifactPayloads.get(cacheKey);
    if (cached) {
      resolvedArtifactPayloads.delete(cacheKey);
      resolvedArtifactPayloads.set(cacheKey, cached);
      return cached.promise;
    }
    const entry = { promise: null, weight: 0 };
    const pending = loadLedgerArtifactPayload(row, hex, false, contract).then(({ payload, cacheable, retainedBytes }) => {
      if (!cacheable) {
        removeResolvedArtifactPayload(cacheKey, entry);
      } else if (resolvedArtifactPayloads.get(cacheKey) === entry) {
        const weight = Math.max(1_024, retainedBytes * 4);
        if (weight > RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_BYTES) {
          removeResolvedArtifactPayload(cacheKey, entry);
        } else {
          entry.weight = weight;
          resolvedArtifactPayloadBytes += weight;
          trimResolvedArtifactPayloads();
        }
      }
      return payload;
    }).catch((error) => {
      removeResolvedArtifactPayload(cacheKey, entry);
      throw error;
    });
    entry.promise = pending;
    resolvedArtifactPayloads.set(cacheKey, entry);
    trimResolvedArtifactPayloads();
    return pending;
  }

  function removeResolvedArtifactPayload(cacheKey, entry) {
    if (resolvedArtifactPayloads.get(cacheKey) !== entry) return;
    resolvedArtifactPayloads.delete(cacheKey);
    resolvedArtifactPayloadBytes = Math.max(0, resolvedArtifactPayloadBytes - entry.weight);
  }

  function trimResolvedArtifactPayloads() {
    while (
      resolvedArtifactPayloads.size > RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_ENTRIES
      || resolvedArtifactPayloadBytes > RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_BYTES
    ) {
      const oldest = resolvedArtifactPayloads.entries().next().value;
      if (!oldest) break;
      removeResolvedArtifactPayload(oldest[0], oldest[1]);
    }
  }

  async function resolveLedgerArtifactPayloads(rows, required = false, readOptions = {}) {
    const unique = [...new Set(rows)];
    const payloads = new Map();
    let next = 0;
    const workers = Array.from({ length: Math.min(ARTIFACT_PAYLOAD_READ_CONCURRENCY, unique.length) }, async () => {
      for (;;) {
        const index = next;
        next += 1;
        if (index >= unique.length) return;
        const row = unique[index];
        if (required) {
          const hash = contentHashHex(row?.artifact?.content);
          if (!hash || !row.source_file) throw new Error(`Activity has no readable artifact receipt: ${ledgerArtifactId(row)}`);
          const contract = Object.hasOwn(readOptions, "projection")
            ? artifactReadProjectionForLabel(readOptions.projection, ledgerArtifactLabel(row)) : null;
          payloads.set(row, (await loadLedgerArtifactPayload(row, hash, true, contract)).payload);
        } else payloads.set(row, await resolveLedgerArtifactPayload(row, readOptions));
      }
    });
    await Promise.all(workers);
    return payloads;
  }

  async function resolveLedgerArtifactPayloadResults(rows, readOptions = {}) {
    const unique = [...new Set(rows)];
    const results = new Map();
    let next = 0;
    const workers = Array.from({ length: Math.min(ARTIFACT_PAYLOAD_READ_CONCURRENCY, unique.length) }, async () => {
      for (;;) {
        const index = next;
        next += 1;
        if (index >= unique.length) return;
        const row = unique[index];
        try {
          results.set(row, { payload: await resolveLedgerArtifactPayload(row, readOptions), error: "" });
        } catch (error) {
          results.set(row, {
            payload: null,
            error: error instanceof Error ? error.message : String(error),
          });
        }
      }
    });
    await Promise.all(workers);
    return results;
  }

  function operatorChatIncrementalEventKey(payload, eventKind) {
    if (![
      "commentary_delta",
      "reasoning_delta",
      "plan_delta",
      "tool_output_delta",
      "terminal_interaction",
      "file_change_delta",
      "mcp_progress",
    ].includes(eventKind)) return "";
    const identity = operatorChatEventIdentity(payload, eventKind);
    if (!identity.turn_id || !identity.item_id) return "";
    const summaryIndex = eventKind === "reasoning_delta"
      ? indexedUnsignedInteger(payload?.summary_index ?? payload?.summaryIndex)
      : null;
    const contentIndex = eventKind === "reasoning_delta"
      ? indexedUnsignedInteger(payload?.content_index ?? payload?.contentIndex)
      : null;
    const discriminator = summaryIndex !== null
      ? `:summary:${summaryIndex}`
      : contentIndex !== null
        ? `:content:${contentIndex}`
        : "";
    return `${identity.turn_id}:${identity.item_id}:${eventKind}${discriminator}`;
  }

  function operatorOsScope(projection) {
    return projectionMemo(projection, "structure", "operatorOsScope", () => {
    const sidecars = octopusOsInterfaceSidecars(projection);
    const clientInterfaces = new Map([...sidecars].filter(([, sidecar]) => sidecar.component === "client"));
    const errors = [];
    if (projection.eePolicyProjectionError) {
      errors.push(`canonical EE policy projection failed: ${projection.eePolicyProjectionError}`);
    }
    const policiesByEe = new Map(arrayValue(projection.eePolicyProjection?.environments).map((row) => [
      stringValue(row?.ee_id),
      row,
    ]).filter(([eeId]) => eeId));
    const clientRuns = [];
    for (const record of arrayValue(projection.runRecords)) {
      const matches = [...new Set([
        ...projectedRunInterfaceIds(record),
        ...arrayValue(record.archived_client_interface_ids),
      ])].filter((interfaceId) => clientInterfaces.has(interfaceId));
      if (matches.length > 1) {
        errors.push(`run '${record.run_id}' projects more than one exact Octopus OS client interface`);
        continue;
      }
      const interfaceId = matches[0];
      if (!interfaceId) continue;
      const sidecar = clientInterfaces.get(interfaceId);
      const clientState = record.interface_state?.[interfaceId];
      if (clientState?.client_surface_role === undefined) continue;
      if (clientState.client_surface_role !== sidecar.surface_role) {
        errors.push(`run '${record.run_id}' conflicts with its exact client surface role`);
        continue;
      }
      const authoringProfile = sidecar.authoring_profile ?? null;
      if (authoringProfile && stringValue(authoringProfile.worker_choice_interface)
        !== stringValue(sidecar.worker_choice_interface)) {
        errors.push(`Octopus OS authoring profile '${interfaceId}' conflicts with run '${record.run_id}'`);
      }
      clientRuns.push({
        run_id: stringValue(record.run_id),
        ee_id: stringValue(record.ee_id),
        record_path: stringValue(record.record_path),
        record_digest: stringValue(record.record_digest),
        interface_id: interfaceId,
        surface_role: stringValue(sidecar.surface_role),
        archived: arrayValue(record.archived_client_interface_ids).includes(interfaceId),
        provider_choice_interface: stringValue(sidecar.provider_choice_interface),
        worker_choice_interface: stringValue(sidecar.worker_choice_interface),
        authoring_profile: authoringProfile,
      });
    }
    if (!clientRuns.length) {
      return {
        status: errors.length ? "invalid" : "unbound",
        root_ee_id: "",
        root_environment: null,
        home_run_id: "",
        home_interface_id: "",
        space_name: "",
        home_sidecar_digest: "",
        client_interface_ids: [],
        client_runs: [],
        task_authoring_profiles: [],
        agent_catalog_path: clientAgentCatalogPath(octopusHome()),
        agent_catalog_digest: "",
        agent_project_sources: [],
        agent_presentations: [],
        read_only_runs: [],
        environments: [],
        projects: [],
        project_presentations: [],
        ee_templates: [],
        errors,
      };
    }
    const environments = eeInstanceRows(projection, false);
    const eeById = new Map(environments.map((row) => [stringValue(row.ee_id), row]).filter(([id]) => id));
    const rootEeId = lowestCommonEnvironment(
      [...new Set(clientRuns.map((row) => row.ee_id).filter(Boolean))],
      eeById,
      errors,
    );
    const scopeEeIds = rootEeId ? descendantEnvironmentIds(rootEeId, eeById) : new Set();
    const scopedClientRuns = clientRuns.filter((row) => scopeEeIds.has(row.ee_id));
    const homeCandidates = scopedClientRuns.filter((row) => row.surface_role === "home");
    let homeRun = null;
    if (homeCandidates.length > 1) {
      errors.push(`Octopus OS scope '${rootEeId}' projects more than one Home run`);
    } else if (homeCandidates.length === 1) {
      if (homeCandidates[0].ee_id !== rootEeId) {
        errors.push(`Home run '${homeCandidates[0].run_id}' is not directly inside OS root EE '${rootEeId}'`);
      } else {
        homeRun = homeCandidates[0];
      }
    }
    const homeSidecar = clientInterfaces.get(homeRun?.interface_id);
    const homeDescriptor = projection.descriptorIndexes?.interface?.get(homeRun?.interface_id);
    if (homeDescriptor?.interface_driver_config?.private_config?.application_consent_authority !== undefined) {
      errors.push("Home application_consent_authority selection is retired and requires explicit configuration repair");
    }
    const taskAuthoringProfiles = arrayValue(homeSidecar?.task_authoring_profiles);
    for (const profile of taskAuthoringProfiles) {
      const profileEeId = stringValue(profile.ee_id);
      const profileWorkerId = stringValue(profile.worker_choice_interface);
      const worker = projection.descriptorIndexes?.interface?.get(profileWorkerId);
      if (!scopeEeIds.has(profileEeId)) {
        errors.push(`Octopus OS task authoring profile references EE '${profileEeId}' outside workspace scope '${rootEeId}'`);
      }
      if (stringValue(worker?.interface_type).toLowerCase() !== "system"
        || stringValue(worker?.role).toLowerCase() !== "worker") {
        errors.push(`Octopus OS task authoring profile references missing worker '${profileWorkerId}'`);
      }
    }
    const clientRunIds = new Set(scopedClientRuns.map((row) => row.run_id));
    const runRecordsById = new Map(arrayValue(projection.runRecords).map((record) => [
      stringValue(record?.run_id),
      record,
    ]).filter(([runId]) => runId));
    const readOnlyRuns = runRows(projection, false)
      .filter((row) => scopeEeIds.has(stringValue(row.ee_id)) && !clientRunIds.has(stringValue(row.run_id)))
      .flatMap((row) => {
        const runId = stringValue(row.run_id);
        const record = runRecordsById.get(runId);
        const recordPath = stringValue(record?.record_path);
        const recordDigest = stringValue(record?.record_digest);
        if (!recordPath || !recordDigest) {
          errors.push(`read-only run '${runId}' has no authorable record evidence`);
          return [];
        }
        const recordedClientInterfaces = Object.keys(objectOrEmpty(record?.interface_state))
          .filter((interfaceId) => clientInterfaces.has(interfaceId)
            && record.interface_state[interfaceId]?.client_surface_role === clientInterfaces.get(interfaceId).surface_role)
          .sort();
        if (recordedClientInterfaces.length > 1) {
          errors.push(`read-only run '${runId}' records more than one exact Octopus OS client interface`);
          return [];
        }
        const interfaceId = recordedClientInterfaces[0] ?? "";
        const sidecar = clientInterfaces.get(interfaceId) ?? {};
        return [{
          run_id: runId,
          ee_id: stringValue(row.ee_id),
          record_path: recordPath,
          record_digest: recordDigest,
          interface_id: interfaceId,
          surface_role: stringValue(sidecar.surface_role) || "task",
          provider_choice_interface: stringValue(sidecar.provider_choice_interface),
          worker_choice_interface: stringValue(sidecar.worker_choice_interface),
        }];
      });
    const templates = new Map(descriptorRows(projection, "ee_template").map((row) => [
      stringValue(row.id ?? row.name),
      row,
    ]).filter(([id]) => id));
    const taskRunIdsByEe = new Map();
    for (const row of scopedClientRuns.filter((candidate) => candidate.surface_role === "task")) {
      const ids = taskRunIdsByEe.get(row.ee_id) ?? [];
      ids.push(row.run_id);
      taskRunIdsByEe.set(row.ee_id, ids);
    }
    const readOnlyRunIdsByEe = new Map();
    for (const row of readOnlyRuns) {
      const ids = readOnlyRunIdsByEe.get(row.ee_id) ?? [];
      ids.push(row.run_id);
      readOnlyRunIdsByEe.set(row.ee_id, ids);
    }
    const scopedEnvironments = [...scopeEeIds].flatMap((eeId) => {
      const environment = eeById.get(eeId);
      if (!environment) return [];
      const policy = policiesByEe.get(eeId);
      if (!policy) {
        errors.push(`canonical EE policy projection is missing EE '${eeId}'`);
        return [];
      }
      const templateName = stringValue(environment.template_name);
      const template = templates.get(templateName) ?? {};
      return [{
        ee_id: eeId,
        parent_ee_id: stringValue(environment.parent_ee_id),
        template_name: templateName,
        goal: stringValue(environment.goal),
        status: "projected",
        record_path: stringValue(environment.record_path),
        record_digest: stringValue(environment.record_digest),
        config: objectOrEmpty(environment.config),
        policy_narrowing: objectOrEmpty(environment.policy_narrowing),
        formed_envelope: policy.formed_envelope,
        boundary_envelope: policy.boundary_envelope,
        allowed_child_ee_templates: arrayValue(template.allowed_child_ee_templates).map(stringValue).filter(Boolean),
        client_run_ids: (taskRunIdsByEe.get(eeId) ?? []).sort(),
        read_only_run_ids: (readOnlyRunIdsByEe.get(eeId) ?? []).sort(),
        depth: Math.max(0, environmentLineage(eeId, eeById, []).length - 1),
      }];
    }).sort((left, right) => left.depth - right.depth || left.goal.localeCompare(right.goal) || left.ee_id.localeCompare(right.ee_id));
    const rootEnvironment = scopedEnvironments.find((row) => row.ee_id === rootEeId) ?? null;
    const projects = scopedEnvironments.filter((row) => row.ee_id !== rootEeId);
    const projectEeIds = new Set(projects.map((row) => row.ee_id));
    const workspacePresentation = clientInterfaces.get(homeRun?.interface_id)?.presentation;
    const projectPresentations = arrayValue(workspacePresentation?.projects);
    for (const presentation of projectPresentations) {
      if (!projectEeIds.has(stringValue(presentation.ee_id))) {
        errors.push(
          `Octopus OS Home presentation references project EE '${stringValue(presentation.ee_id)}' outside scope '${rootEeId}'`,
        );
      }
    }
    const profileIds = taskAuthoringProfiles.map((profile) => stringValue(profile?.ee_id)).filter(Boolean);
    const profileScopeIds = [...new Set(profileIds)].sort();
    const presentationIds = projectPresentations.map((presentation) => stringValue(presentation?.ee_id)).filter(Boolean);
    if (new Set(presentationIds).size !== presentationIds.length) {
      errors.push("Octopus OS Home repeats a Project presentation");
    }
    const agentCatalog = projection.clientAgentCatalog?.document ?? null;
    const agentProjectSources = arrayValue(agentCatalog?.project_sources);
    const agentPresentations = arrayValue(agentCatalog?.agents);
    if (!agentCatalog || projection.clientAgentCatalogError) {
      errors.push(projection.clientAgentCatalogError || "Client Agent catalog is unavailable");
    } else {
      const sourceProjectIds = agentProjectSources.map((source) => stringValue(source?.project_ee_id)).sort();
      if (stableJson(sourceProjectIds) !== stableJson(profileScopeIds)) {
        errors.push("Client Agent source workers and task-authoring EEs are not a bijection");
      }
      for (const source of agentProjectSources) {
        const sourceMatches = taskAuthoringProfiles.filter((profile) => (
          stringValue(profile?.ee_id) === stringValue(source?.project_ee_id)
          && stringValue(profile?.worker_choice_interface) === stringValue(source?.source_worker_interface)
        ));
        if (sourceMatches.length !== 1) {
          errors.push(
            `Client Agent source '${stringValue(source?.source_worker_interface)}' has ${sourceMatches.length} exact authoring profiles in '${stringValue(source?.project_ee_id)}'`,
          );
        }
      }
      for (const agent of agentPresentations) {
        const agentMatches = taskAuthoringProfiles.filter((profile) => (
          stringValue(profile?.ee_id) === stringValue(agent?.project_ee_id)
          && stringValue(profile?.worker_choice_interface) === stringValue(agent?.worker_interface)
        ));
        if (!scopeEeIds.has(stringValue(agent?.project_ee_id)) || agentMatches.length !== 1) {
          errors.push(
            `Client Agent '${stringValue(agent?.id)}' has no exact Project worker-template profile`,
          );
        }
        if (numberValue(agent?.deleted_at_ms, 0) === 0) {
          const worker = projection.descriptorIndexes?.interface?.get(stringValue(agent?.worker_interface));
          if (!worker?.client_worker_template) {
            errors.push(`Client Agent '${stringValue(agent?.id)}' has no active client worker template`);
          }
        }
      }
    }
    const templateRows = [...templates].map(([id, template]) => ({
      id,
      goal_class: stringValue(template.goal_class ?? template.goal ?? id),
    })).sort((left, right) => left.goal_class.localeCompare(right.goal_class) || left.id.localeCompare(right.id));
    return {
      status: errors.length ? "invalid" : homeRun ? "ready" : "awaiting_home",
      root_ee_id: rootEeId,
      root_environment: rootEnvironment,
      home_run_id: homeRun?.run_id ?? "",
      home_interface_id: homeRun?.interface_id ?? "",
      space_name: stringValue(workspacePresentation?.name),
      home_sidecar_digest: stringValue(homeSidecar?.source_digest),
      client_interface_ids: scopedClientRuns.map((row) => row.interface_id).sort(),
      client_runs: scopedClientRuns,
      task_authoring_profiles: taskAuthoringProfiles,
      agent_catalog_path: projection.clientAgentCatalog?.file ?? clientAgentCatalogPath(octopusHome()),
      agent_catalog_digest: projection.clientAgentCatalog?.digest ?? "",
      agent_project_sources: agentProjectSources,
      agent_presentations: agentPresentations,
      read_only_runs: readOnlyRuns,
      environments: scopedEnvironments,
      projects,
      project_presentations: projectPresentations,
      ee_templates: templateRows,
      errors,
    };
    });
  }

  function operatorOsApplications(projection) {
    return projectionMemo(projection, "structure", "operatorOsApplications", () => {
      const scope = operatorOsScope(projection);
      const environments = arrayValue(scope.environments);
      const scopeEeIds = new Set(environments.map((environment) => stringValue(environment.ee_id)).filter(Boolean));
      const runRecords = arrayValue(projection.runRecords).filter((record) => scopeEeIds.has(stringValue(record.ee_id)));
      const taskProfilesByRun = new Map(arrayValue(scope.client_runs).map((run) => [
        stringValue(run?.run_id),
        objectOrEmpty(run?.authoring_profile),
      ]).filter(([runId]) => runId));
      const descriptors = descriptorRows(projection, "interface");
      const descriptorsByInterface = new Map(descriptors.map((candidate) => [
        stringValue(candidate.id ?? candidate.rule_id),
        candidate,
      ]).filter(([interfaceId]) => interfaceId));
      const toolDescriptors = new Map(descriptorRows(projection, "tool").map((descriptor) => [
        stringValue(descriptor.id),
        descriptor,
      ]).filter(([id]) => id));
      const descriptorIds = new Set(descriptors.map((descriptor) => stringValue(descriptor.id ?? descriptor.rule_id)).filter(Boolean));
      const installedDescriptors = new Map(descriptors.flatMap((descriptor) => {
        const manifest = descriptor.application_manifest;
        return isPlainRecord(manifest) ? [[stringValue(manifest.id), descriptor]] : [];
      }));
      const packageCatalog = objectOrEmpty(projection.applicationPackages);
      const activePackages = new Map(arrayValue(packageCatalog.active).map((candidate) => [
        stringValue(candidate?.manifest?.id),
        candidate,
      ]).filter(([id]) => id));
      const sourcePackages = new Map(arrayValue(packageCatalog.sources).map((candidate) => [
        stringValue(candidate?.manifest?.id),
        candidate,
      ]).filter(([id]) => id));
      const sourceConflicts = objectOrEmpty(packageCatalog.source_conflicts);
      const localApplicationPackages = new Map(arrayValue(projection.localPackages?.packages)
        .filter((candidate) => stringValue(candidate?.product_kind) === "application")
        .map((candidate) => [stringValue(candidate?.product_id), candidate])
        .filter(([id]) => id));
      const applicationIds = [...new Set([
        ...installedDescriptors.keys(),
        ...sourcePackages.keys(),
        ...localApplicationPackages.keys(),
      ])].sort();
      const applications = applicationIds.flatMap((applicationId) => {
        const descriptor = installedDescriptors.get(applicationId);
        const sourcePackage = sourcePackages.get(applicationId);
        const localPackage = localApplicationPackages.get(applicationId);
        const installedPackage = activePackages.get(applicationId);
        const ownerFoldersPresent = Boolean(descriptor);
        const receiptState = stringValue(localPackage?.receipt_state);
        const materialized = localPackage?.materialized === true || ownerFoldersPresent;
        const partiallyMaterialized = localPackage?.partially_materialized === true;
        const installed = localPackage?.installed === true
          && receiptState === "valid"
          && ownerFoldersPresent;
        const materializationNeedsAttention = !installed && (
          materialized || partiallyMaterialized || receiptState === "stale"
        );
        const manifest = descriptor?.application_manifest ?? sourcePackage?.manifest;
        if (!isPlainRecord(manifest)) return [];
        const interfaceIds = arrayValue(manifest.interfaces).map(stringValue).filter(Boolean);
        const missingInterfaceIds = ownerFoldersPresent
          ? interfaceIds.filter((interfaceId) => !descriptorIds.has(interfaceId))
          : [];
        const collidingInterfaceIds = installed
          ? []
          : interfaceIds.filter((interfaceId) => descriptorIds.has(interfaceId));
        const artifactIds = arrayValue(manifest.artifact_bindings)
          .map((binding) => stringValue(binding?.artifact_id))
          .filter(Boolean);
        const taskTools = arrayValue(manifest?.task_capabilities?.tools);
        const capabilityContractValid = taskTools.every((capability) => {
          const toolId = stringValue(capability?.tool);
          const interfaceId = stringValue(capability?.interface);
          const kind = stringValue(capability?.worker_environment_kind);
          const descriptor = toolDescriptors.get(toolId);
          return Boolean(toolId && interfaceId && kind && descriptor)
            && stringValue(descriptor.interface) === interfaceId
            && arrayValue(descriptor.supported_worker_environment_kinds).map(stringValue).includes(kind);
        });
        const runApplicationReferences = runRecords.map((record) => {
          const projectedInterfaces = projectedRunInterfaceIds(record);
          const projectedArtifactIds = new Set(arrayValue(record.checkpoints)
            .filter((checkpoint) => checkpoint.status === "projected")
            .flatMap((checkpoint) => arrayValue(checkpoint.artifact_binding_ids).map(stringValue).filter(Boolean)));
          const runId = stringValue(record.run_id);
          const profile = taskProfilesByRun.get(runId) ?? {};
          const selectedTaskCapabilities = applicationTaskCapabilityReferences(
            profile,
            manifest,
          );
          return {
            runId,
            connected: interfaceIds.every((interfaceId) => projectedInterfaces.has(interfaceId))
              && artifactIds.every((artifactId) => projectedArtifactIds.has(artifactId))
              && selectedTaskCapabilities.complete,
            referenced: interfaceIds.some((interfaceId) => projectedInterfaces.has(interfaceId))
              || artifactIds.some((artifactId) => projectedArtifactIds.has(artifactId)),
          };
        });
        const connectedRunIds = runApplicationReferences
          .filter((reference) => reference.runId && reference.connected)
          .map((reference) => reference.runId)
          .sort();
        const referencedRunIds = runApplicationReferences
          .filter((reference) => reference.runId && reference.referenced)
          .map((reference) => reference.runId)
          .sort();
        const connectedRunIdSet = new Set(connectedRunIds);
        const partiallyConnectedRunIds = referencedRunIds.filter((runId) => !connectedRunIdSet.has(runId));
        const projectStates = environments.map((environment) => {
          const effective = objectOrEmpty(environment.formed_envelope);
          const allowedInterfaces = Array.isArray(effective.allowed_interfaces)
            ? new Set(effective.allowed_interfaces.map(stringValue).filter(Boolean))
            : null;
          const ceiling = objectOrEmpty(environment.boundary_envelope);
          const ceilingInterfaces = Array.isArray(ceiling.allowed_interfaces)
            ? new Set(ceiling.allowed_interfaces.map(stringValue).filter(Boolean))
            : null;
          const allowedTools = Array.isArray(effective.allowed_tools)
            ? new Set(effective.allowed_tools.map(stringValue).filter(Boolean))
            : null;
          const allowedKinds = Array.isArray(effective.allowed_worker_environment_kinds)
            ? new Set(effective.allowed_worker_environment_kinds.map(stringValue).filter(Boolean))
            : null;
          const ceilingTools = Array.isArray(ceiling.allowed_tools)
            ? new Set(ceiling.allowed_tools.map(stringValue).filter(Boolean))
            : null;
          const ceilingKinds = Array.isArray(ceiling.allowed_worker_environment_kinds)
            ? new Set(ceiling.allowed_worker_environment_kinds.map(stringValue).filter(Boolean))
            : null;
          const capabilityInterfaces = taskTools.map((capability) => stringValue(capability?.interface)).filter(Boolean);
          const capabilityTools = taskTools.map((capability) => stringValue(capability?.tool)).filter(Boolean);
          const capabilityKinds = taskTools.map((capability) => stringValue(capability?.worker_environment_kind)).filter(Boolean);
          const requiredInterfaces = [...new Set([...interfaceIds, ...capabilityInterfaces])];
          const policyAllowed = capabilityContractValid && interfaceIds.length > 0
            && (allowedInterfaces === null || requiredInterfaces.every((interfaceId) => allowedInterfaces.has(interfaceId)))
            && (allowedTools === null || capabilityTools.every((toolId) => allowedTools.has(toolId)))
            && (allowedKinds === null || capabilityKinds.every((kind) => allowedKinds.has(kind)));
          const defaults = new Set(arrayValue(
            objectOrEmpty(objectOrEmpty(environment.config).octopus_os).application_defaults,
          ).map(stringValue).filter(Boolean));
          return {
            ee_id: stringValue(environment.ee_id),
            is_root: stringValue(environment.ee_id) === stringValue(scope.root_ee_id),
            policy_allowed: policyAllowed,
            ceiling_allowed: capabilityContractValid
              && (ceilingInterfaces === null || requiredInterfaces.every((interfaceId) => ceilingInterfaces.has(interfaceId)))
              && (ceilingTools === null || capabilityTools.every((toolId) => ceilingTools.has(toolId)))
              && (ceilingKinds === null || capabilityKinds.every((kind) => ceilingKinds.has(kind))),
            selected_by_default: defaults.has(stringValue(manifest.id)),
            record_digest: stringValue(environment.record_digest),
          };
        });
        const sourceConflict = stringValue(sourceConflicts[applicationId]);
        const packageError = stringValue(localPackage?.error);
        const installBlockReason = installed
          ? "Application is already installed."
          : sourceConflict
            || packageError
            || (materializationNeedsAttention
              ? "Application files exist without one valid local install receipt. Repair the package before continuing."
              : "")
            || (!sourcePackage || localPackage?.available !== true
              ? "No verified package source is available."
              : collidingInterfaceIds.length
                ? `Cannot install while interface folders already exist: ${collidingInterfaceIds.join(", ")}`
                : "");
        const sourceKind = localPackage?.available === true ? "local" : "none";
        const managed = stringValue(localPackage?.release_status) === "system";
        const configuration = applicationPrivateConfigurationProjection(
          manifest,
          interfaceIds,
          descriptorsByInterface,
          installed,
        );
        const availableProjects = projectStates.filter((state) => state.policy_allowed);
        const defaultProjects = projectStates.filter((state) => state.selected_by_default);
        const removalBlockReason = !installed
          ? "Application is not installed."
          : managed
            ? "This application is supplied by the Octopus installation and cannot be removed independently."
            : stringValue(localPackage?.receipt_state) !== "valid"
              ? "This application has no valid local install receipt and must be repaired before removal."
            : referencedRunIds.length
              ? partiallyConnectedRunIds.length
                ? `Repair this application's incomplete connection in ${partiallyConnectedRunIds.length} task${partiallyConnectedRunIds.length === 1 ? "" : "s"} before removing it.`
                : `Disconnect this application from ${connectedRunIds.length} task${connectedRunIds.length === 1 ? "" : "s"} before removing it.`
              : availableProjects.length || defaultProjects.length
                ? "Remove this application from every project before removing its package."
                : missingInterfaceIds.length
                  ? "Repair the incomplete package before removing it."
                  : "";
        const presentation = descriptor?.application_manifest ?? sourcePackage;
        return [{
          ...manifest,
          logo_data_url: stringValue(presentation?.logo_data_url),
          logo_dark_data_url: stringValue(presentation?.logo_dark_data_url),
          installed,
          materialized,
          partially_materialized: partiallyMaterialized,
          valid: installed
            ? missingInterfaceIds.length === 0 && capabilityContractValid
            : materializationNeedsAttention
              ? false
            : Boolean(sourcePackage)
              && localPackage?.available === true
              && !packageError
              && !sourceConflict
              && collidingInterfaceIds.length === 0,
          missing_interface_ids: missingInterfaceIds,
          descriptor_path: stringValue(descriptor?.descriptor_path ?? sourcePackage?.descriptor_path),
          manifest_path: stringValue(descriptor?.application_manifest_path ?? sourcePackage?.manifest_path),
          manifest_digest: stringValue(descriptor?.application_manifest_digest ?? sourcePackage?.manifest_digest),
          package_id: stringValue(localPackage?.id) || applicationId,
          installed_digest: stringValue(localPackage?.installed_digest ?? installedPackage?.package_digest),
          source_available: Boolean(sourcePackage) && localPackage?.available === true,
          source_kind: sourceKind,
          install_packages: arrayValue(localPackage?.install_packages),
          source_digest: stringValue(localPackage?.source_digest),
          source_file_count: numberValue(localPackage?.source_file_count, 0),
          receipt_state: receiptState,
          receipt_digest: stringValue(localPackage?.receipt_digest),
          managed,
          install_block_reason: installBlockReason,
          removal_allowed: !removalBlockReason,
          removal_block_reason: removalBlockReason,
          connected_run_ids: connectedRunIds,
          partially_connected_run_ids: partiallyConnectedRunIds,
          project_states: projectStates,
          configuration,
        }];
      }).sort((left, right) => (
        stringValue(left.category).localeCompare(stringValue(right.category))
        || stringValue(left.name).localeCompare(stringValue(right.name))
      ));
      return {
        status: stringValue(scope.status),
        root_ee_id: stringValue(scope.root_ee_id),
        applications,
        projects: operatorOsProjectRows(scope, environments),
        errors: [
          ...arrayValue(scope.errors),
          ...arrayValue(projection.warnings).filter((warning) => {
            const normalized = stringValue(warning).toLowerCase();
            return normalized.includes("application manifest")
              || normalized.includes("application logo")
              || normalized.includes("application package")
              || normalized.includes("application source");
          }),
        ],
      };
    });
  }

  function operatorOsDrivers(projection) {
    return projectionMemo(projection, "structure", "operatorOsDrivers", () => {
      const scope = operatorOsScope(projection);
      const environments = arrayValue(scope.environments);
      const scopeEeIds = new Set(environments.map((environment) => stringValue(environment.ee_id)).filter(Boolean));
      const tools = descriptorRows(projection, "tool");
      const instances = workerEnvironmentRows(projection).filter((instance) => (
        scopeEeIds.has(stringValue(instance.ee_id))
      ));
      const installedDriverDescriptors = new Map(descriptorRows(projection, "worker_environment_kind").map((descriptor) => [
        stringValue(
          descriptor.worker_environment_kind_id
          ?? descriptor.worker_environment_kind
          ?? descriptor.kind
          ?? descriptor.id,
        ),
        descriptor,
      ]).filter(([name]) => name));
      const localDriverPackages = new Map(arrayValue(projection.localPackages?.packages)
        .filter((candidate) => stringValue(candidate?.product_kind) === "driver")
        .map((candidate) => [stringValue(candidate?.product_id), candidate])
        .filter(([name]) => name));
      const driverNames = [...new Set([
        ...installedDriverDescriptors.keys(),
        ...localDriverPackages.keys(),
      ])].sort();
      const drivers = driverNames.map((name) => {
        const installedDescriptor = installedDriverDescriptors.get(name);
        const localPackage = localDriverPackages.get(name);
        const descriptor = installedDescriptor ?? objectOrEmpty(localPackage?.product_metadata?.descriptor);
        const receiptState = stringValue(localPackage?.receipt_state);
        const materialized = localPackage?.materialized === true || Boolean(installedDescriptor);
        const partiallyMaterialized = localPackage?.partially_materialized === true;
        const installed = localPackage?.installed === true
          && receiptState === "valid"
          && Boolean(installedDescriptor);
        const materializationNeedsAttention = !installed && (
          materialized || partiallyMaterialized || receiptState === "stale"
        );
        const supportedTools = tools.filter((tool) => (
          arrayValue(tool.supported_worker_environment_kinds).map(stringValue).includes(name)
        ));
        const toolIds = installedDescriptor
          ? supportedTools.map((tool) => stringValue(tool.id)).filter(Boolean).sort()
          : arrayValue(localPackage?.product_metadata?.tool_ids).map(stringValue).filter(Boolean).sort();
        const toolBindings = supportedTools.map((tool) => ({
          tool_id: stringValue(tool.id),
          interface_id: stringValue(tool.interface),
        })).filter((binding) => binding.tool_id && binding.interface_id)
          .sort((left, right) => left.tool_id.localeCompare(right.tool_id));
        const doorInterfaces = installedDescriptor
          ? [...new Set(supportedTools.map((tool) => stringValue(tool.interface)).filter(Boolean))].sort()
          : arrayValue(localPackage?.product_metadata?.door_interfaces).map(stringValue).filter(Boolean).sort();
        const activeInstances = instances.filter((instance) => (
          stringValue(instance.worker_environment_kind) === name
          && instance.live_available === true
        ));
        const projectStates = environments.map((environment) => {
          const effective = objectOrEmpty(environment.formed_envelope);
          const allowedKinds = Array.isArray(effective.allowed_worker_environment_kinds)
            ? new Set(effective.allowed_worker_environment_kinds.map(stringValue).filter(Boolean))
            : null;
          const ceiling = objectOrEmpty(environment.boundary_envelope);
          const ceilingKinds = Array.isArray(ceiling.allowed_worker_environment_kinds)
            ? new Set(ceiling.allowed_worker_environment_kinds.map(stringValue).filter(Boolean))
            : null;
          const octopusOsConfig = objectOrEmpty(objectOrEmpty(environment.config).octopus_os);
          const defaults = new Set(arrayValue(
            octopusOsConfig.driver_defaults,
          ).map(stringValue).filter(Boolean));
          return {
            ee_id: stringValue(environment.ee_id),
            is_root: stringValue(environment.ee_id) === stringValue(scope.root_ee_id),
            policy_allowed: allowedKinds === null || allowedKinds.has(name),
            ceiling_allowed: ceilingKinds === null || ceilingKinds.has(name),
            selected_by_default: defaults.has(name),
            record_digest: stringValue(environment.record_digest),
            host_slots: objectOrEmpty(objectOrEmpty(octopusOsConfig.driver_host_slots)[name]),
          };
        });
        const projectConfigured = projectStates.some((state) => (
          state.policy_allowed || state.selected_by_default
        ));
        return {
          name,
          display_name: stringValue(localPackage?.display?.name),
          description: stringValue(localPackage?.display?.description),
          package_id: stringValue(localPackage?.id) || `driver.${name}`,
          installed,
          materialized,
          partially_materialized: partiallyMaterialized,
          source_available: localPackage?.available === true,
          source_digest: stringValue(localPackage?.source_digest),
          installed_digest: stringValue(localPackage?.installed_digest),
          source_file_count: numberValue(localPackage?.source_file_count, 0),
          install_packages: arrayValue(localPackage?.install_packages),
          agent_presets: arrayValue(projection.localPackages?.packs).flatMap((pack) => arrayValue(pack.workspace_setup?.agent_presets)
            .filter((preset) => preset.component_package_id === localPackage?.id)
            .map((preset) => {
              const worker = descriptorRows(projection, "interface").find((entry) => entry.id === preset.worker_interface_id);
              const providerIds = arrayValue(worker?.worker_policy?.ceiling?.interfaces);
              return { ...preset, pack_id: pack.id, pack_manifest_digest: pack.manifest_digest,
                provider_interface_ids: providerIds,
                project_ee_ids: environments.filter((environment) => arrayValue(scope.task_authoring_profiles).some((profile) => profile.ee_id === environment.ee_id)
                  && (!Array.isArray(environment.formed_envelope?.allowed_interfaces) || environment.formed_envelope.allowed_interfaces.includes(preset.worker_interface_id))
                  && arrayValue(projection.eePolicyProjection?.environments).some((entry) => entry.ee_id === environment.ee_id && arrayValue(entry.workers).some((candidate) => candidate.worker_interface_id === preset.worker_interface_id)))
                  .map((environment) => environment.ee_id),
              };
            })),
          receipt_state: receiptState,
          install_block_reason: installed
            ? "Driver is already installed."
            : stringValue(localPackage?.error)
              || (materializationNeedsAttention
                ? "Driver files exist without one valid local install receipt. Repair the package before continuing."
                : "")
              || (localPackage?.available === true ? "" : "No verified package source is available."),
          removal_allowed: installed
            && receiptState === "valid"
            && activeInstances.length === 0
            && !projectConfigured,
          removal_block_reason: !installed
            ? "Driver is not installed."
            : receiptState !== "valid"
              ? "This Driver has no valid local install receipt and must be repaired before removal."
              : activeInstances.length
                ? `Release ${activeInstances.length} active worker environment${activeInstances.length === 1 ? "" : "s"} before removing this Driver.`
                : projectConfigured
                  ? "Remove this Driver from every project before removing its package."
                  : "",
          substrate_family: stringValue(descriptor.substrate_family),
          worker_consent: stringValue(descriptor.worker_consent),
          capabilities: arrayValue(descriptor.capabilities).map(stringValue).filter(Boolean),
          supported_executor_kinds: arrayValue(descriptor.supported_executor_kinds).map(stringValue).filter(Boolean),
          instance_config_files: arrayValue(descriptor.instance_config_files).map(stringValue).filter(Boolean),
          door_interfaces: doorInterfaces,
          door_driver_refs: arrayValue(descriptor.door_driver_refs).map(stringValue).filter(Boolean),
          tool_ids: toolIds,
          tool_bindings: toolBindings,
          boundary_defaults: objectOrEmpty(descriptor.boundary_defaults),
          lifecycle: objectOrEmpty(descriptor.lifecycle),
          descriptor_path: stringValue(descriptor.descriptor_path),
          active_instances: activeInstances,
          project_states: projectStates,
        };
      }).filter((driver) => driver.name).sort((left, right) => left.name.localeCompare(right.name));
      return {
        status: stringValue(scope.status),
        root_ee_id: stringValue(scope.root_ee_id),
        host_root_refs: objectOrEmpty(projection.hostRootRefs),
        host_manifest_ready: !projection.hostManifestError,
        octopus_home: projection.octopusHome,
        drivers,
        projects: operatorOsProjectRows(scope, environments),
        errors: [
          ...arrayValue(scope.errors),
          ...arrayValue(projection.localPackages?.errors),
          ...(projection.hostManifestError ? [projection.hostManifestError] : []),
        ],
      };
    });
  }

  function operatorChatContextRows(projection) {
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const osScope = operatorOsScope(projection);
    const projectNames = new Map(arrayValue(osScope.project_presentations).map((presentation) => [
      stringValue(presentation.ee_id),
      stringValue(presentation.name),
    ]).filter(([eeId, name]) => eeId && name));
    const projectProfileIndexes = new Map();
    const taskProfiles = osScope.status === "invalid" ? [] : arrayValue(osScope.task_authoring_profiles);
    const agentProjectSources = new Map(arrayValue(osScope.agent_project_sources).map((source) => [
      stringValue(source?.project_ee_id),
      stringValue(source?.source_worker_interface),
    ]).filter(([projectEeId, workerInterfaceId]) => projectEeId && workerInterfaceId));
    const agentPresentations = new Map(arrayValue(osScope.agent_presentations).map((agent) => [
      `${stringValue(agent?.project_ee_id)}\0${stringValue(agent?.worker_interface)}`,
      agent,
    ]));
    const indexedTaskProfiles = taskProfiles.map((profile) => {
      const eeId = stringValue(profile.ee_id);
      const index = projectProfileIndexes.get(eeId) ?? 0;
      projectProfileIndexes.set(eeId, index + 1);
      return { profile, index };
    });
    const homeProfile = osScope.status === "invalid"
      ? null
      : arrayValue(osScope.client_runs)
        .find((run) => stringValue(run?.surface_role) === "home")?.authoring_profile ?? null;
    const authoringProfiles = new Map([
      ...indexedTaskProfiles,
      ...(homeProfile ? [{ profile: homeProfile, index: 0 }] : []),
    ].map(({ profile, index }) => [
      `${stringValue(profile.ee_id)}\0${stringValue(profile.worker_choice_interface)}`,
      { profile, index },
    ]));
    const scopedEnvironmentIds = new Set(arrayValue(osScope.environments).map((row) => stringValue(row.ee_id)).filter(Boolean));
    const allEnvironments = eeInstanceRows(projection);
    const environments = scopedEnvironmentIds.size
      ? allEnvironments.filter((environment) => scopedEnvironmentIds.has(stringValue(environment.ee_id)))
      : osScope.status === "invalid"
        ? []
        : allEnvironments;
    const policiesByEe = new Map(arrayValue(projection.eePolicyProjection?.environments).map((row) => [
      stringValue(row?.ee_id),
      row,
    ]).filter(([eeId]) => eeId));
    const privateInterfaceIds = new Set(octopusOsInterfaceSidecars(projection).keys());
    const providerIds = descriptorRows(projection, "interface")
      .filter((row) => (
        !privateInterfaceIds.has(stringValue(row.id ?? row.rule_id))
        && stringValue(row.interface_type).toLowerCase() === "world"
        && stringValue(row.role).toLowerCase() === "provider"
        && arrayValue(row.interface_driver_config?.private_config?.client_projection_output_labels).includes(protocol.terminalOutputLabel)
        && arrayValue(row.interface_driver_config?.private_config?.client_projection_output_labels).includes(protocol.errorOutputLabel)
      ))
      .map((row) => stringValue(row.id ?? row.rule_id))
      .filter(Boolean);
    const workers = descriptorRows(projection, "interface")
      .filter((row) => (
        !privateInterfaceIds.has(stringValue(row.id ?? row.rule_id))
        && stringValue(row.interface_type).toLowerCase() === "system"
        && stringValue(row.role).toLowerCase() === "worker"
      ));
    const installedApplications = arrayValue(operatorOsApplications(projection).applications);
    const installedDrivers = arrayValue(operatorOsDrivers(projection).drivers);
    return environments.flatMap((environment) => {
      const projectedPolicy = policiesByEe.get(stringValue(environment.ee_id));
      if (!projectedPolicy) return [];
      const effectivePolicy = projectedPolicy.formed_envelope;
      const formedWorkerPolicies = new Map(arrayValue(projectedPolicy.workers).map((row) => [
        stringValue(row?.worker_interface_id),
        objectOrEmpty(row?.interface_policy),
      ]).filter(([workerInterfaceId]) => workerInterfaceId));
      const allowedInterfaces = Array.isArray(effectivePolicy.allowed_interfaces)
        ? new Set(effectivePolicy.allowed_interfaces)
        : null;
      const allowedTools = Array.isArray(effectivePolicy.allowed_tools)
        ? new Set(effectivePolicy.allowed_tools)
        : null;
      const allowedKinds = Array.isArray(effectivePolicy.allowed_worker_environment_kinds)
        ? new Set(effectivePolicy.allowed_worker_environment_kinds)
        : null;
      const interfaceAllowed = (interfaceId) => allowedInterfaces === null || allowedInterfaces.has(interfaceId);
      const availableWorkers = workers
        .filter((worker) => interfaceAllowed(stringValue(worker.id)))
        .map((worker) => {
          const ceiling = objectOrEmpty(objectOrEmpty(worker.worker_policy).ceiling);
          const workerInterfaces = new Set(arrayValue(ceiling.interfaces).map(stringValue).filter(Boolean));
          const workerTools = new Set(arrayValue(ceiling.tools).map(stringValue).filter(Boolean));
          const workerKinds = new Set(arrayValue(ceiling.wek).map(stringValue).filter(Boolean));
          const authoredTemplate = authoringProfiles.get(
            `${stringValue(environment.ee_id)}\0${stringValue(worker.id)}`,
          );
          const authoringProfile = authoredTemplate?.profile;
          const agentPresentation = agentPresentations.get(
            `${stringValue(environment.ee_id)}\0${stringValue(worker.id)}`,
          );
          const clientTemplate = objectOrEmpty(worker.client_worker_template);
          const clientTemplateMatchesProject = stringValue(clientTemplate.project_ee_id) === stringValue(environment.ee_id)
            && stringValue(clientTemplate.worker_interface) === stringValue(worker.id);
          const profileInputs = new Set(arrayValue(authoringProfile?.external_inputs).map((route) => stringValue(route?.interface_id)).filter(Boolean));
          const profileOutputs = new Set(arrayValue(authoringProfile?.model_outputs).map((route) => stringValue(route?.interface_id)).filter(Boolean));
          const profileArtifacts = new Set(arrayValue(authoringProfile?.context_artifact_bindings).map((binding) => stringValue(binding?.id)).filter(Boolean));
          const profileKinds = new Set(arrayValue(authoringProfile?.selected_worker_environment_kinds).map(stringValue).filter(Boolean));
          const applications = installedApplications.flatMap((application) => {
            const projectState = arrayValue(application?.project_states).find((state) => (
              stringValue(state?.ee_id) === stringValue(environment.ee_id)
            ));
            const interfaces = arrayValue(application?.interfaces).map(stringValue).filter(Boolean);
            const artifactBindings = arrayValue(application?.artifact_bindings);
            const taskTools = arrayValue(application?.task_capabilities?.tools);
            const available = application?.valid === true
              && projectState?.policy_allowed === true
              && interfaces.every((interfaceId) => (
                interfaceAllowed(interfaceId) && workerInterfaces.has(interfaceId)
              ))
              && taskTools.every((capability) => {
                const tool = stringValue(capability?.tool);
                const owner = stringValue(capability?.interface);
                const kind = stringValue(capability?.worker_environment_kind);
                return tool && owner && kind
                  && workerTools.has(tool)
                  && workerInterfaces.has(owner)
                  && workerKinds.has(kind)
                  && (allowedTools === null || allowedTools.has(tool))
                  && interfaceAllowed(owner)
                  && (allowedKinds === null || allowedKinds.has(kind));
              });
            if (!available) return [];
            const explicitlySelected = interfaces.some((interfaceId) => (
              profileInputs.has(interfaceId) || profileOutputs.has(interfaceId)
            )) || artifactBindings.some((binding) => profileArtifacts.has(stringValue(binding?.artifact_id)));
            return [{
              id: stringValue(application.id),
              selected_by_default: projectState?.selected_by_default === true || explicitlySelected,
            }];
          });
          const drivers = installedDrivers.flatMap((driver) => {
            const name = stringValue(driver?.name);
            const projectState = arrayValue(driver?.project_states).find((state) => (
              stringValue(state?.ee_id) === stringValue(environment.ee_id)
            ));
            if (
              !name
              || projectState?.policy_allowed !== true
              || !workerKinds.has(name)
              || allowedKinds !== null && !allowedKinds.has(name)
            ) return [];
            const toolBindings = arrayValue(driver?.tool_bindings).filter((binding) => {
              const toolId = stringValue(binding?.tool_id);
              const interfaceId = stringValue(binding?.interface_id);
              return toolId
                && interfaceId
                && workerTools.has(toolId)
                && workerInterfaces.has(interfaceId)
                && (allowedTools === null || allowedTools.has(toolId))
                && interfaceAllowed(interfaceId);
            });
            if (!toolBindings.length) return [];
            return [{
              name,
              capabilities: arrayValue(driver.capabilities).map(stringValue).filter(Boolean),
              root_slots: [...new Set([
                ...arrayValue(driver?.boundary_defaults?.read_root_refs),
                ...arrayValue(driver?.boundary_defaults?.write_root_refs),
              ].map((root) => stringValue(root?.path)).filter(Boolean))].sort(),
              tool_ids: [...new Set(toolBindings.map((binding) => stringValue(binding.tool_id)))].sort(),
              door_interfaces: [...new Set(toolBindings.map((binding) => stringValue(binding.interface_id)))].sort(),
              tools: toolBindings.map((binding) => ({
                tool: stringValue(binding.tool_id),
                interface: stringValue(binding.interface_id),
                worker_environment_kind: name,
              })),
              host_slots: objectOrEmpty(projectState.host_slots),
              selected_by_default: projectState.selected_by_default === true || profileKinds.has(name),
            }];
          });
          const workerProviderIds = providerIds
            .filter((providerId) => workerInterfaces.has(providerId) && interfaceAllowed(providerId))
            .sort();
          const interfacePolicy = formedWorkerPolicies.get(stringValue(worker.id));
          if (!interfacePolicy) return null;
          return {
            id: stringValue(worker.id),
            label: arrayValue(worker.source_names).map(stringValue).find(Boolean) || stringValue(worker.id),
            authoring_profile: Boolean(authoringProfile),
            authoring_profile_index: authoredTemplate?.index ?? -1,
            agent_source_template: agentProjectSources.get(stringValue(environment.ee_id)) === stringValue(worker.id),
            client_agent_id: stringValue(agentPresentation?.id),
            client_agent_name: stringValue(agentPresentation?.name),
            client_agent_icon: stringValue(agentPresentation?.icon),
            client_agent_created_at_ms: numberValue(agentPresentation?.created_at_ms, 0),
            client_agent_updated_at_ms: numberValue(agentPresentation?.updated_at_ms, 0),
            client_agent_deleted_at_ms: numberValue(agentPresentation?.deleted_at_ms, 0),
            client_template: clientTemplateMatchesProject,
            client_template_provider_interface_id: clientTemplateMatchesProject
              ? stringValue(clientTemplate.provider_interface_id)
              : "",
            client_template_provider_model: clientTemplateMatchesProject
              ? stringValue(clientTemplate.provider_model)
              : "",
            client_template_reasoning_effort: clientTemplateMatchesProject
              ? stringValue(clientTemplate.reasoning_effort)
              : "",
            client_template_service_tier: clientTemplateMatchesProject
              ? stringValue(clientTemplate.service_tier) : "",
            client_template_digest: clientTemplateMatchesProject
              ? stringValue(worker.client_worker_template_digest)
              : "",
            client_template_application_ids: clientTemplateMatchesProject
              ? arrayValue(clientTemplate.application_ids)
              : [],
            client_template_worker_environment_kinds: clientTemplateMatchesProject
              ? arrayValue(clientTemplate.worker_environment_kinds)
              : [],
            provider_interface_ids: workerProviderIds,
            interface_policy: structuredClone(interfacePolicy),
            role_artifact_ids: arrayValue(worker.worker_policy?.role_artifacts).map(stringValue).filter(Boolean),
            authoring_room_interface_id: stringValue(protocol.authoringRoomInterfaceId),
            door_host_slots: objectOrEmpty(authoringProfile?.door_host_slots),
            external_inputs: arrayValue(authoringProfile?.external_inputs),
            external_relays: arrayValue(authoringProfile?.external_relays),
            model_outputs: arrayValue(authoringProfile?.model_outputs),
            context_artifact_bindings: arrayValue(authoringProfile?.context_artifact_bindings),
            payload_artifact_bindings: payloadArtifactBindings(authoringProfile?.payload_artifact_bindings),
            selected_worker_environment_kinds: arrayValue(authoringProfile?.selected_worker_environment_kinds),
            selected_tools: arrayValue(authoringProfile?.selected_tools),
            applications,
            drivers,
          };
        })
        .filter((worker) => worker?.authoring_profile)
        .sort((left, right) => left.authoring_profile_index - right.authoring_profile_index || left.id.localeCompare(right.id));
      const defaultTaskAccessMode = stringValue(
        objectOrEmpty(objectOrEmpty(environment.config).octopus_os).task_access_default,
      ) || "project_access";
      if (!["project_access", "full_access"].includes(defaultTaskAccessMode)) {
        throw new Error(`EE '${stringValue(environment.ee_id)}' has an invalid Octopus OS task access default`);
      }
      return [{
        ee_id: stringValue(environment.ee_id),
        is_os_root: stringValue(environment.ee_id) === stringValue(osScope.root_ee_id),
        display_name: projectNames.get(stringValue(environment.ee_id)) ?? "",
        goal: stringValue(environment.goal),
        template_name: stringValue(environment.template_name),
        parent_ee_id: stringValue(environment.parent_ee_id),
        agent_catalog_digest: stringValue(osScope.agent_catalog_digest),
        default_task_access_mode: defaultTaskAccessMode,
        host_root_refs: objectOrEmpty(projection.hostRootRefs),
        formed_envelope: effectivePolicy,
        workers: availableWorkers,
      }];
    }).filter((row) => row.ee_id && row.workers.length > 0);
  }

  async function operatorChatRunIndex(projection) {
    const cached = projection.operatorChatRunIndexCache;
    if (cached && cached.generation === projectionGeneration) return cached.promise;
    const promise = buildOperatorChatRunIndex(projection);
    projection.operatorChatRunIndexCache = {
      generation: projectionGeneration,
      promise,
    };
    promise.catch(() => {
      if (projection.operatorChatRunIndexCache?.promise === promise) {
        projection.operatorChatRunIndexCache = null;
      }
    });
    return promise;
  }

  async function buildOperatorChatRunIndex(projection) {
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const inboundLabels = new Set([
      protocol.inputLabel,
      protocol.approvalResponseLabel,
      protocol.elicitationResponseLabel,
      protocol.userInputResponseLabel,
    ].filter(Boolean));
    const scope = operatorOsScope(projection);
    const recordsByRun = new Map(arrayValue(projection.runRecords).map((record) => [
      stringValue(record.run_id),
      record,
    ]).filter(([runId]) => runId));
    const byRun = new Map(arrayValue(scope.client_runs).map((row) => {
      const runId = stringValue(row.run_id);
      return [runId, {
        runId,
        record: recordsByRun.get(runId) ?? null,
        writable: true,
        chatInterfaceId: stringValue(row.interface_id),
        surfaceRole: stringValue(row.surface_role),
        osRootEeId: stringValue(scope.root_ee_id),
        providerChoiceInterfaceId: stringValue(row.provider_choice_interface),
        workerChoiceInterfaceId: stringValue(row.worker_choice_interface),
        doorHostSlots: objectOrEmpty(row.authoring_profile?.door_host_slots),
        externalInputs: arrayValue(row.authoring_profile?.external_inputs),
        inbound: [],
        outbound: [],
        activity: [],
      }];
    }).filter(([runId, entry]) => runId && entry.chatInterfaceId));
    for (const row of arrayValue(scope.read_only_runs)) {
      const runId = stringValue(row.run_id);
      if (!runId || byRun.has(runId)) continue;
      byRun.set(runId, {
        runId,
        record: recordsByRun.get(runId) ?? null,
        writable: false,
        chatInterfaceId: stringValue(row.interface_id),
        surfaceRole: stringValue(row.surface_role) || "task",
        osRootEeId: stringValue(scope.root_ee_id),
        providerChoiceInterfaceId: stringValue(row.provider_choice_interface),
        workerChoiceInterfaceId: stringValue(row.worker_choice_interface),
        doorHostSlots: objectOrEmpty(row.authoring_profile?.door_host_slots),
        externalInputs: arrayValue(row.authoring_profile?.external_inputs),
        inbound: [],
        outbound: [],
        activity: [],
      });
    }
    const visibleLabels = new Set([
      protocol.terminalOutputLabel,
      protocol.errorOutputLabel,
      "turn.stream",
      ...arrayValue(protocol.streamOutputLabels),
    ]);
    const indexedRows = await operatorChatLedgerRows(projection, new Set(byRun.keys()));
    for (const [runId, entry] of byRun) {
      for (const row of indexedRows.get(runId) ?? []) {
        if (row.kind === "InterfaceArrival") {
          entry.activity.push(row);
          if (!entry.writable || (
            inboundLabels.has(ledgerArtifactLabel(row))
            && movementBoundaryInterface(row, "from") === entry.chatInterfaceId
          )) {
            entry.inbound.push(row);
          }
        } else if (row.kind === "InterfaceOutbound") {
          entry.activity.push(row);
          const label = ledgerArtifactLabel(row);
          if (!entry.writable || (
            movementBoundaryInterface(row, "to") === entry.chatInterfaceId
            && (visibleLabels.has(label) || isOperatorChatProviderFailureRow(entry, row))
          )) {
            entry.outbound.push(row);
          }
        }
      }
    }
    return Array.from(byRun.values());
  }

  async function operatorChatLedgerRows(projection, runIds) {
    const indexed = await readIndexedOperatorChatLedgerRows(projection, runIds);
    if (indexed) {
      projection.operatorChatStatusCursor = Math.max(
        projection.operatorChatStatusCursor,
        indexed.nextMovementSeq,
      );
      return indexed.rowsByRun;
    }
    await refreshLedgerProjection(projection);
    projection.operatorChatStatusCursor = projection.ledgerRows.reduce(
      (latest, row) => Math.max(latest, numberValue(row.seq, -1)),
      projection.operatorChatStatusCursor,
    );
    return new Map([...runIds].map((runId) => [
      runId,
      ledgerRowsForRun(projection, runId),
    ]));
  }

  async function readIndexedOperatorChatLedgerRows(projection, runIds) {
    if (runIds.size === 0) return new Map();
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const rowsByRun = new Map([...runIds].map((runId) => [runId, []]));
      const ids = [...runIds];
      for (let offset = 0; offset < ids.length; offset += 128) {
        const chunk = ids.slice(offset, offset + 128);
        const placeholders = chunk.map(() => "?").join(", ");
        const rows = index.database.prepare(
          `SELECT
            seq, lamport, recorded_at_unix_ms, trace, kind,
            from_kind, from_id, to_kind, to_id,
            artifact_id, artifact_content,
            runtime_run, runtime_checkpoint, runtime_interface
          FROM movements
          WHERE kind IN ('InterfaceArrival', 'InterfaceOutbound')
            AND seq < ?
            AND (
              runtime_run IN (${placeholders})
            )
          ORDER BY seq ASC`,
        ).all(index.watermark, ...chunk);
        for (const row of rows) {
          const projected = normalizeIndexedProjectionMovement(row, index.ledgerPath);
          const runId = indexedMovementRunId(row);
          if (!runIds.has(runId)) continue;
          rowsByRun.get(runId).push(projected);
        }
      }
      const tail = await readIndexedLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        runIds,
      );
      if (tail === null) return null;
      for (const [runId, rows] of tail.rowsByRun) {
        rowsByRun.get(runId).push(...rows);
      }
      return {
        rowsByRun,
        nextMovementSeq: tail.nextMovementSeq,
      };
    } catch (error) {
      preserveProjectionEvidenceError(error);
      return null;
    } finally {
      index.database.close();
    }
  }

  async function readIndexedLedgerTail(ledgerPath, startOffset, expectedSeq, runIds) {
    const rowsByRun = new Map([...runIds].map((runId) => [runId, []]));
    const stat = await fs.stat(ledgerPath);
    if (stat.size < startOffset) return null;
    if (stat.size === startOffset) return rowsByRun;
    let pending = Buffer.alloc(0);
    for await (const chunk of fsSync.createReadStream(ledgerPath, { start: startOffset })) {
      const bytes = pending.length ? Buffer.concat([pending, chunk]) : chunk;
      let recordStart = 0;
      for (;;) {
        const newline = bytes.indexOf(0x0a, recordStart);
        if (newline < 0) break;
        const end = newline > recordStart && bytes[newline - 1] === 0x0d ? newline - 1 : newline;
        const raw = bytes.subarray(recordStart, end);
        recordStart = newline + 1;
        if (!raw.length) continue;
        const movement = parseCanonicalMovement(raw, ledgerPath, expectedSeq + 1);
        const seq = indexedUnsignedInteger(movement?.seq);
        if (seq === null || seq !== expectedSeq) return null;
        expectedSeq += 1;
        const kind = movementKindText(movement?.kind);
        if (kind !== "InterfaceArrival" && kind !== "InterfaceOutbound") continue;
        const projected = {
          ...movement,
          kind,
          envelope_id: stringValue(movement.envelope_id ?? movement.id) || `movements:${seq + 1}`,
          source_file: ledgerPath,
          source_line: seq + 1,
        };
        const runId = movementRunId(projected);
        if (runIds.has(runId)) rowsByRun.get(runId).push(projected);
      }
      pending = bytes.subarray(recordStart);
    }
    return {
      rowsByRun,
      nextMovementSeq: expectedSeq - 1,
    };
  }

  async function operatorChatLedgerDelta(projection, runIds, afterSeq) {
    const indexed = await readIndexedOperatorChatLedgerDelta(projection, runIds, afterSeq);
    if (indexed) return indexed;
    await refreshLedgerProjection(projection);
    const rowsByRun = new Map([...runIds].map((runId) => [runId, []]));
    let nextMovementSeq = afterSeq;
    for (const row of projection.ledgerRows) {
      const seq = numberValue(row.seq, -1);
      nextMovementSeq = Math.max(nextMovementSeq, seq);
      if (seq <= afterSeq || (row.kind !== "InterfaceArrival" && row.kind !== "InterfaceOutbound")) continue;
      const runId = movementRunId(row);
      if (runIds.has(runId)) rowsByRun.get(runId).push(row);
    }
    return { rowsByRun, nextMovementSeq };
  }

  async function readIndexedOperatorChatLedgerDelta(projection, runIds, afterSeq) {
    const index = await openValidatedLedgerIndex(projection);
    if (!index) return null;
    try {
      const rowsByRun = new Map([...runIds].map((runId) => [runId, []]));
      const rows = index.database.prepare(
        `SELECT
          seq, lamport, recorded_at_unix_ms, trace, kind,
          from_kind, from_id, to_kind, to_id,
          artifact_id, artifact_content,
          runtime_run, runtime_checkpoint, runtime_interface
        FROM movements
        WHERE seq > ?
          AND seq < ?
          AND kind IN ('InterfaceArrival', 'InterfaceOutbound')
        ORDER BY seq ASC`,
      ).all(afterSeq, index.watermark);
      for (const row of rows) {
        const runId = indexedMovementRunId(row);
        if (!runIds.has(runId)) continue;
        rowsByRun.get(runId).push(normalizeIndexedProjectionMovement(row, index.ledgerPath));
      }
      const tail = await readIndexedLedgerTail(
        index.ledgerPath,
        index.durableEndOffset,
        index.watermark,
        runIds,
      );
      if (tail === null) return null;
      for (const [runId, tailRows] of tail.rowsByRun) {
        rowsByRun.get(runId).push(...tailRows.filter((row) => numberValue(row.seq, -1) > afterSeq));
      }
      return {
        rowsByRun,
        nextMovementSeq: Math.max(afterSeq, tail.nextMovementSeq),
      };
    } catch (error) {
      preserveProjectionEvidenceError(error);
      return null;
    } finally {
      index.database.close();
    }
  }

  async function operatorChatTurnOpening(entry, row, protocol) {
    if (movementRunId(row) !== entry.runId) return null;
    const label = ledgerArtifactLabel(row);
    if (row.kind === "InterfaceArrival"
      && label === protocol.inputLabel
      && movementBoundaryInterface(row, "from") === entry.chatInterfaceId) {
      return "waiting";
    }
    const provider = operatorChatRuntimeProviderId(entry);
    if (!provider) return null;
    if (row.kind === "InterfaceOutbound" && movementBoundaryInterface(row, "to") === provider
      && arrayValue(entry.externalInputs).some((input) => input.interface_id === movementBoundaryInterface(row, "from")
        && input.checkpoint_id === stringValue(row.runtime?.checkpoint ?? row.invocation_metadata?.runtime?.checkpoint))) {
      return "waiting";
    }
    if (row.kind === "InterfaceOutbound"
      && movementBoundaryInterface(row, "to") === provider
      && label === "provider.peer.input") {
      const payload = await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection });
      return payload?.message?.action === "followup" ? "waiting" : null;
    }
    if (!isOperatorChatProviderMovement(entry, row)) return null;
    if (label === "turn.started") return "working";
    if (label === "provider.transport.dispatch") {
      const payload = await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection });
      return payload?.provider_interface === provider && payload?.method === "turn/start"
        ? "working" : null;
    }
    return null;
  }

  async function reduceOperatorChatTurnState(initial, rows, entry, protocol) {
    const ordered = [...rows].sort((left, right) => numberValue(left.seq, -1) - numberValue(right.seq, -1));
    const executionScope = operatorChatExecutionScope(entry);
    let executionObserved = initial.executionObserved ?? !executionScope;
    let open = initial.open;
    let state = initial.state;
    let lastMovementSeq = initial.lastMovementSeq;
    let updatedAtMs = initial.updatedAtMs;
    let openedMovementSeq = initial.openedMovementSeq ?? null;
    let activeNativeTurn = initial.activeNativeTurn ?? null;
    let pendingInputs = [...(initial.pendingInputs ?? [])];
    const nativeTurns = new Map(initial.nativeTurns ?? []);
    const recordedInputs = new Map([...(initial.recordedInputs ?? [])].map(([key, inputs]) => [key, new Set(inputs)]));
    const nativeKey = (payload) => {
      const thread = stringValue(payload?.thread_id ?? payload?.threadId);
      const turn = operatorChatEventIdentity(payload, "").turn_id;
      return thread && turn ? JSON.stringify([thread, turn]) : null;
    };
    const observeNative = (payload, source) => {
      const key = nativeKey(payload);
      if (!key || !Number.isSafeInteger(source) || source < 0) return null;
      nativeTurns.set(key, Math.min(nativeTurns.get(key) ?? source, source));
      return key;
    };
    const settle = (payload, row, outcome) => {
      const key = nativeKey(payload), nativeSource = nativeTurns.get(key);
      const source = payload?.source_movement_seq ?? payload?.additional_details?.source_movement_seq;
      pendingInputs = pendingInputs.filter((input) => {
        if (row.seq <= input.seq) return true;
        if (input.clientId && recordedInputs.get(key)?.has(input.clientId)) return false;
        return nativeSource < input.seq || Number.isSafeInteger(source) && source < input.seq;
      });
      if (!key || activeNativeTurn === true || activeNativeTurn === key) activeNativeTurn = null;
      open = pendingInputs.length > 0 || activeNativeTurn !== null;
      state = open ? "working" : outcome;
      recordedInputs.delete(key);
      if (!open) recordedInputs.clear();
    };
    for (const row of ordered) {
      lastMovementSeq = Math.max(lastMovementSeq, numberValue(row.seq, -1));
      updatedAtMs = Math.max(updatedAtMs, rowTimeMs(row));
      if (!executionObserved) {
        executionObserved = executionScope.checkpoints.has(stringValue(
          row.runtime?.checkpoint ?? row.invocation_metadata?.runtime?.checkpoint,
        )) || row.seq === executionScope.sourceMovementSeq;
        if (!executionObserved) continue;
      }
      const label = ledgerArtifactLabel(row);
      const providerMovement = isOperatorChatProviderMovement(entry, row);
      let providerPayload;
      if (providerMovement && label === "turn.stream") {
        const payloads = new Map([[row, await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection })]]);
        const expanded = operatorChatExpandedStreamRows([row], payloads, entry.runId,
          (header) => observeNative(header, header.opened_source_movement_seq));
        for (const event of expanded) {
          if (event.__stream.event_kind !== "input_recorded") continue;
          const receipt = payloads.get(event);
          const key = observeNative(receipt, receipt.source_movement_seq);
          if (!recordedInputs.has(key)) recordedInputs.set(key, new Set());
          recordedInputs.get(key).add(receipt.client_id);
        }
      }
      if (providerMovement && label !== "turn.stream") {
        providerPayload = await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection });
        observeNative(providerPayload, providerPayload?.source_movement_seq ?? providerPayload?.additional_details?.source_movement_seq ?? row.seq);
        if (label === "turn.completed") {
          for (const input of nativeTurnRecordedInputs(providerPayload)) {
            const key = nativeKey(input);
            if (!recordedInputs.has(key)) recordedInputs.set(key, new Set());
            recordedInputs.get(key).add(input.client_id);
          }
        }
      }
      const opening = await operatorChatTurnOpening(entry, row, protocol);
      if (opening) {
        if (opening === "working") activeNativeTurn = nativeKey(providerPayload) ?? true;
        if (row.kind === "InterfaceArrival" && label === protocol.inputLabel
          && movementBoundaryInterface(row, "from") === entry.chatInterfaceId) {
          const payload = await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection });
          pendingInputs.push({ seq: row.seq, clientId: stringValue(payload?.message_id) });
        }
        if (!open) {
          open = true;
          openedMovementSeq = numberValue(row.seq, -1);
          state = opening;
        } else if (opening === "working") {
          state = "working";
        }
        continue;
      }
      if (["turn.control.acknowledged", "turn.control.failed"].includes(label)) {
        if (!providerMovement) continue;
        const outcome = operatorTurnControlOutcome(
          await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection }),
          row,
        );
        if (outcome?.status === "acknowledged" && outcome.action === "resume") {
          open = true;
          activeNativeTurn = nativeKey(outcome.payload) ?? true;
          openedMovementSeq = numberValue(row.seq, -1);
          state = "working";
          continue;
        }
        if (!open) continue;
        if (label === "turn.control.failed" && outcome?.status === "failed" && outcome.action === "steer") {
          const source = outcome.payload.source_movement_seq;
          const previous = pendingInputs.length;
          pendingInputs = pendingInputs.filter((input) => !(input.clientId === outcome.controlId
            && Number.isSafeInteger(source) && source >= input.seq && source < row.seq));
          if (pendingInputs.length !== previous) {
            open = pendingInputs.length > 0 || activeNativeTurn !== null;
            state = open ? "working" : "failed";
            continue;
          }
        }
        if (operatorTurnControlEndsTurn(outcome)) {
          settle(outcome.payload, row, "idle");
          continue;
        }
        state = "working";
        continue;
      }
      if (!open) continue;
      if (providerMovement && label === "turn.completed") {
        const payload = await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection });
        settle(payload, row, stringValue(payload?.status ?? payload?.turn?.status) === "failed" ? "failed" : "idle");
        continue;
      }
      if (providerMovement && label === protocol.terminalOutputLabel) {
        settle(await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection }), row, "idle");
        continue;
      }
      if (providerMovement && label === "turn.interrupted.result") {
        settle(await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection }), row, "idle");
        continue;
      }
      if (providerMovement && label === protocol.errorOutputLabel) {
        const payload = await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection });
        if (payload?.will_retry === true) {
          state = "working";
          continue;
        }
        settle(payload, row, "failed");
        continue;
      }
      if (isOperatorChatProviderFailureRow(entry, row)) {
        const payload = await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection });
        operatorChatProviderFailureActivity(entry, row, payload);
        settle(payload, row, "failed");
        continue;
      }
      state = "working";
    }
    return { open, state, lastMovementSeq, updatedAtMs, openedMovementSeq, executionObserved, activeNativeTurn, pendingInputs, nativeTurns, recordedInputs };
  }

  async function operatorChatTurnState(projection, entry, protocol) {
    const state = await reduceOperatorChatTurnState(
      { open: false, state: "idle", lastMovementSeq: -1, updatedAtMs: 0 },
      entry.activity,
      entry,
      protocol,
    );
    projection.operatorChatTurnStateCache.set(entry.runId, state);
    return state;
  }

  async function operatorChatStatuses(projection, params) {
    if (!Array.isArray(params.run_ids)) {
      throw new Error("run_ids must be an array of exact projected task run ids");
    }
    const runIds = [...new Set(params.run_ids.map((value) => requiredString(value, "run_ids[]")))];
    if (runIds.length > 5_000) throw new Error("task status scope exceeds 5000 projected tasks");
    const refresh = projection.operatorChatStatusRefreshTail.then(() => (
      refreshOperatorChatTurnStates(projection)
    ));
    projection.operatorChatStatusRefreshTail = refresh.then(() => undefined, () => undefined);
    await refresh;
    const scopeRuns = new Map(arrayValue(operatorOsScope(projection).client_runs).map((row) => [stringValue(row.run_id), row]));
    return {
      chats: runIds.flatMap((runId) => {
        if (!scopeRuns.has(runId)) return [];
        const state = projection.operatorChatTurnStateCache.get(runId);
        return state ? [{
          run_id: runId,
          archived: scopeRuns.get(runId).archived === true,
          turn_open: state.open,
          turn_state: state.state,
          turn_opened_movement_seq: state.openedMovementSeq,
          updated_at_ms: state.updatedAtMs,
          last_movement_seq: Math.max(0, state.lastMovementSeq),
        }] : [];
      }),
      ledger_cursor_seq: projection.operatorChatStatusCursor,
    };
  }

  async function refreshOperatorChatTurnStates(projection) {
    if (!projection.operatorChatStatusInitialized) await operatorChatsList(projection);
    const scope = operatorOsScope(projection);
    const recordsByRun = new Map(arrayValue(projection.runRecords).map((record) => [
      stringValue(record.run_id), record,
    ]));
    const entries = new Map(arrayValue(scope.client_runs).flatMap((row) => {
      const runId = stringValue(row.run_id);
      const chatInterfaceId = stringValue(row.interface_id);
      return runId && chatInterfaceId
        ? [[runId, { runId, chatInterfaceId, record: recordsByRun.get(runId) ?? null,
          externalInputs: arrayValue(row.authoring_profile?.external_inputs) }]] : [];
    }));
    if (entries.size === 0) return;
    const delta = await operatorChatLedgerDelta(
      projection,
      new Set(entries.keys()),
      projection.operatorChatStatusCursor,
    );
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    for (const [runId, rows] of delta.rowsByRun) {
      if (rows.length === 0) continue;
      const entry = entries.get(runId);
      if (!entry) continue;
      const current = projection.operatorChatTurnStateCache.get(runId)
        ?? { open: false, state: "idle", lastMovementSeq: -1, updatedAtMs: 0 };
      const next = await reduceOperatorChatTurnState(
        current,
        rows.filter((row) => numberValue(row.seq, -1) > current.lastMovementSeq),
        entry,
        protocol,
      );
      projection.operatorChatTurnStateCache.set(runId, next);
    }
    projection.operatorChatStatusCursor = Math.max(
      projection.operatorChatStatusCursor,
      delta.nextMovementSeq,
    );
  }

  async function operatorChatsList(projection) {
    const cached = projection.operatorChatsListCache;
    if (cached?.generation === projectionGeneration) return cached.promise;
    const promise = buildOperatorChatsList(projection);
    projection.operatorChatsListCache = { generation: projectionGeneration, promise };
    promise.catch(() => {
      if (projection.operatorChatsListCache?.promise === promise) {
        projection.operatorChatsListCache = null;
      }
    });
    return promise;
  }

  async function operatorChatTitle(entry, sidecars, protocol, includeInputTitle = true) {
    const authoredTitle = entry.surfaceRole === "task"
      ? stringValue(sidecars.get(entry.chatInterfaceId)?.presentation?.name)
      : "";
    if (authoredTitle || !includeInputTitle) return authoredTitle || entry.runId;
    const firstInbound = entry.inbound
      .filter((row) => !entry.writable || ledgerArtifactLabel(row) === protocol.inputLabel)
      .sort((a, b) => rowTimeMs(a) - rowTimeMs(b))[0];
    if (firstInbound) {
      const text = operatorChatTurnText(await resolveLedgerArtifactPayload(firstInbound, { projection: protocol.artifactReadProjection }), "in");
      if (text) return text.replace(/\s+/g, " ").trim().slice(0, 48);
    }
    return entry.runId;
  }

  async function operatorChatControlTargets(projection, includeTitles) {
    const scope = operatorOsScope(projection);
    if (scope.status !== "ready") throw new Error("Control target lookup requires the current valid client scope");
    const scoped = new Map(scope.client_runs.map(row => [row.run_id, row]));
    const sidecars = octopusOsInterfaceSidecars(projection);
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const rows = await Promise.all((await operatorChatRunIndex(projection)).filter(entry => scoped.has(entry.runId)).map(async entry => {
      const owner = scoped.get(entry.runId);
      if (owner.interface_id !== entry.chatInterfaceId) throw new Error("Control target differs from its scoped client interface");
      const title = await operatorChatTitle(entry, sidecars, protocol, includeTitles);
      const movements = entry.activity.length ? entry.activity : [...entry.inbound, ...entry.outbound];
      return { run_id: entry.runId, chat_interface_id: entry.chatInterfaceId, title,
        updated_at_ms: movements.reduce((last, row) => Math.max(last, rowTimeMs(row)), 0),
        archived: owner.archived, writable: entry.writable && !owner.archived, client_surface_role: owner.surface_role };
    }));
    return rows.sort((left, right) => right.updated_at_ms - left.updated_at_ms);
  }

  async function buildOperatorChatsList(projection) {
    const voiceRunsById = new Map(arrayValue(projection.runRecords).map((run) => [run.run_id, run]));
    const clientSidecars = octopusOsInterfaceSidecars(projection);
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const projectColors = new Map(arrayValue(operatorOsScope(projection).project_presentations)
      .map((presentation) => [stringValue(presentation.ee_id), stringValue(presentation.color)]));
    const connectedApplicationsByRun = new Map();
    for (const application of arrayValue(operatorOsApplications(projection).applications)) {
      const applicationId = stringValue(application?.id);
      if (!applicationId) continue;
      for (const runId of arrayValue(application?.connected_run_ids).map(stringValue).filter(Boolean)) {
        const ids = connectedApplicationsByRun.get(runId) ?? [];
        ids.push(applicationId);
        connectedApplicationsByRun.set(runId, ids);
      }
    }
    const eeById = new Map(eeInstanceRows(projection, false).map((row) => [stringValue(row.ee_id), row]));
    const chats = await Promise.all((await operatorChatRunIndex(projection)).map(async (entry) => {
      const movements = entry.activity.length ? entry.activity : [...entry.inbound, ...entry.outbound];
      const times = movements.map(rowTimeMs).filter(Boolean);
      const lastMovementSeq = movements.reduce(
        (latest, row) => Math.max(latest, numberValue(row.seq, 0)),
        0,
      );
      const title = await operatorChatTitle(entry, clientSidecars, protocol);
      const provider = operatorChatRunProvider(projection, entry);
      const record = entry.record;
      const eeId = stringValue(record?.ee_id);
      const ee = eeById.get(eeId) ?? {};
      const workerInterfaceId = stringValue(record?.worker);
      const chatInterfaceId = entry.chatInterfaceId;
      const chatState = objectOrEmpty(record?.interface_state?.[chatInterfaceId]);
      const archived = arrayValue(record?.archived_client_interface_ids).includes(chatInterfaceId);
      const turn = entry.writable
        ? await operatorChatTurnState(projection, entry, protocol)
        : { open: false, state: "idle" };
      const lastTerminalMessage = await operatorChatLastTerminalMessage(entry, protocol);
      const activityAgent = operatorActivityAgent(projection, eeId, entry.workerChoiceInterfaceId || workerInterfaceId);
      return {
        run_id: entry.runId,
        title,
        chat_interface_id: chatInterfaceId,
        client_surface_role: entry.surfaceRole,
        writable: entry.writable && !archived,
        archived,
        os_root_ee_id: entry.osRootEeId,
        voice_context: projectHomeVoiceContext(record, octopusOsInterfaceSidecars(projection).get(chatInterfaceId), chatState, voiceRunsById),
        turn_open: turn.open,
        turn_state: turn.state,
        ee_id: eeId,
        ee_goal: stringValue(ee?.goal),
        ee_template_name: stringValue(ee?.template_name),
        worker_interface_id: workerInterfaceId,
        worker_choice_interface_id: entry.workerChoiceInterfaceId || workerInterfaceId,
        door_host_slots: entry.doorHostSlots,
        provider_interface_id: provider.id,
        provider_bound_interface_id: provider.boundId,
        provider_model: provider.model,
        provider_models_refreshable: provider.modelCatalogRefreshable,
        reasoning_effort: provider.reasoningEffort,
        service_tier: provider.serviceTier,
        created_at_ms: times.length ? Math.min(...times) : 0,
        updated_at_ms: times.length ? Math.max(...times) : 0,
        last_movement_seq: lastMovementSeq,
        message_count: entry.writable
          ? entry.inbound.filter((row) => ledgerArtifactLabel(row) === protocol.inputLabel).length
            + entry.outbound.filter((row) => {
          const label = ledgerArtifactLabel(row);
          return label === protocol.terminalOutputLabel || label === protocol.errorOutputLabel;
            }).length
          : entry.inbound.length + entry.outbound.length,
        pinned_artifact_ids: arrayValue(chatState.pinned_artifact_bindings)
          .map((binding) => stringValue(binding?.id))
          .filter(Boolean),
        pinned_context_items: arrayValue(chatState.pinned_context_items),
        continued_from: objectOrEmpty(chatState.continued_from),
        record_digest: stringValue(record?.record_digest),
        connected_application_ids: (connectedApplicationsByRun.get(entry.runId) ?? []).sort(),
        last_terminal_message: lastTerminalMessage,
        agent_id: activityAgent?.id ?? null,
        agent_name: activityAgent?.name ?? null,
        agent_icon: activityAgent?.icon ?? null,
        agent_color: activityAgent ? projectColors.get(eeId) || null : null,
      };
    }));
    projection.operatorChatStatusInitialized = true;
    return chats.sort((a, b) => b.updated_at_ms - a.updated_at_ms);
  }

  async function operatorChatLastTerminalMessage(entry, protocol) {
    const rows = entry.outbound
      .filter((candidate) => (
        ledgerArtifactLabel(candidate) === protocol.terminalOutputLabel
        && movementBoundaryInterface(candidate, "to") === entry.chatInterfaceId
      ))
      .sort(compareMovementOrder);
    const row = rows.at(-1);
    const artifactId = ledgerArtifactId(row);
    const movementSeq = numberValue(row?.seq, 0);
    if (!row || !artifactId || !movementSeq) return {};
    const payload = await resolveLedgerArtifactPayload(row, { projection: protocol.artifactReadProjection });
    const text = operatorChatTurnText(payload, "out").replace(/\s+/g, " ").trim().slice(0, 240);
    return {
      artifact_id: artifactId,
      created_at_ms: rowTimeMs(row),
      interface_id: entry.chatInterfaceId,
      movement_seq: movementSeq,
      text,
    };
  }

  async function operatorActivity(projection, chats) {
    const key = JSON.stringify(chats.map((chat) => chat.run_id));
    const cached = projection.operatorActivityCache;
    if (cached?.generation === projectionGeneration && cached.key === key) return cached.promise;
    const promise = buildOperatorActivity(projection, chats);
    projection.operatorActivityCache = { generation: projectionGeneration, key, promise };
    promise.catch(() => {
      if (projection.operatorActivityCache?.promise === promise) projection.operatorActivityCache = null;
    });
    return promise;
  }

  async function buildOperatorActivity(projection, chats) {
    if (chats.length > 1000) throw new Error("Activity exceeds the 1000-chat read boundary");
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const entries = new Map((await operatorChatRunIndex(projection)).map((entry) => [entry.runId, entry]));
    const items = [];
    const outputLabels = new Set([protocol.terminalOutputLabel, "operator.approval.request", "operator.elicitation.request", "operator.user_input.request", "turn.operator_request.resolved"]);
    const responseLabels = new Set([protocol.approvalResponseLabel, protocol.elicitationResponseLabel, protocol.userInputResponseLabel]);
    for (const chat of chats) {
      const entry = entries.get(chat.run_id);
      if (!entry) throw new Error(`Activity chat has no projected run: ${chat.run_id}`);
      const outbound = entry.outbound.filter((row) => row.kind === "InterfaceOutbound" && movementBoundaryInterface(row, "to") === entry.chatInterfaceId);
      const direct = outbound.filter((row) => outputLabels.has(ledgerArtifactLabel(row)));
      const inbound = entry.inbound.filter((row) => row.kind === "InterfaceArrival" && movementBoundaryInterface(row, "from") === entry.chatInterfaceId && responseLabels.has(ledgerArtifactLabel(row)));
      if (direct.length + inbound.length > 50_000) throw new Error(`Activity exceeds the 50000-receipt read boundary: ${chat.run_id}`);
      const payloads = await resolveLedgerArtifactPayloads([...direct, ...inbound], true, { projection: protocol.artifactReadProjection });
      const deliveries = direct.filter((row) => ledgerArtifactLabel(row) === protocol.terminalOutputLabel).map((row) => ({
          artifact_id: ledgerArtifactId(row), seq: row.seq,
          movement_id: `${path.basename(stringValue(row.source_file) || "ledger", ".jsonl")}:${numberValue(row.source_line, 0)}`,
        }));
      const responses = [];
      for (const row of inbound) {
        const label = ledgerArtifactLabel(row);
        const key = operatorChatResponseKey(label, payloads.get(row), protocol);
        if (!key) throw new Error(`Activity response has no request identity: ${ledgerArtifactId(row)}`);
        responses.push({ key, seq: row.seq });
      }
      const messages = direct.map((row) => operatorActivityMessage(row, payloads.get(row), protocol));
      const input = { chat, messages, deliveries, responses, agent: operatorActivityAgent(projection, chat.ee_id, chat.worker_choice_interface_id) };
      let rows = operatorActivityRows(input);
      const pending = rows.filter((row) => row.kind !== "message");
      if (pending.length) {
        const after = Math.min(...pending.map((row) => row.seq));
        const streams = outbound.filter((row) => ledgerArtifactLabel(row) === "turn.stream" && row.seq > after);
        if (streams.length > 1000) throw new Error(`Activity exceeds the 1000-stream resolution read boundary: ${chat.run_id}`);
        const streamPayloads = await resolveLedgerArtifactPayloads(streams, true);
        for (const row of operatorChatExpandedStreamRows(streams, streamPayloads, entry.runId)) {
          if (ledgerArtifactLabel(row) === "turn.operator_request.resolved") messages.push(operatorActivityMessage(row, streamPayloads.get(row), protocol));
        }
        rows = operatorActivityRows(input);
      }
      items.push(...rows);
    }
    if (items.length > 10_000) throw new Error("Activity exceeds the 10000-item read boundary");
    return items.sort((a, b) => (b.created_at_ms ?? 0) - (a.created_at_ms ?? 0) || b.seq - a.seq || a.id.localeCompare(b.id));
  }

  async function operatorChatSearch(projection, params) {
    const query = requiredString(params.query, "query").replace(/\s+/g, " ").trim();
    if (query.length < 2) throw new Error("task transcript search query must contain at least two characters");
    if (query.length > 512) throw new Error("task transcript search query must not exceed 512 characters");
    if (!Array.isArray(params.task_run_ids)) throw new Error("task_run_ids must be an array of exact projected task run ids");
    const requestedTaskRunIds = [...new Set(params.task_run_ids.map((value) => requiredString(value, "task_run_ids[]")))];
    if (requestedTaskRunIds.length === 0) throw new Error("task transcript search requires at least one projected task run id");
    if (requestedTaskRunIds.length > 5_000) throw new Error("task transcript search scope exceeds 5000 projected tasks");
    const parsedLimit = params.limit === undefined ? 12 : optionalUnsignedInteger(params.limit);
    const limit = Math.min(50, Math.max(1, parsedLimit ?? 12));
    const requestedBeforeSeq = optionalUnsignedInteger(params.before_seq);
    const searchScope = operatorChatSearchScope(projection, requestedTaskRunIds);
    const index = await openValidatedLedgerIndex(projection);
    if (!index) throw new Error("ledger index is required for task transcript search");
    try {
      if (index.durableEndOffset !== index.ledgerSize) {
        throw new Error("ledger index must be current before task transcript search can run");
      }
      if (requestedBeforeSeq !== null && requestedBeforeSeq > index.watermark) {
        throw new Error("task transcript search cursor is beyond the ledger index watermark");
      }
      const needle = query.toLocaleLowerCase();
      const matches = [];
      let cursor = requestedBeforeSeq ?? index.watermark;
      let exhausted = cursor === 0;
      while (matches.length < limit && !exhausted) {
        const candidates = readIndexedTaskSearchCandidates(
          index,
          searchScope.targetsBySourceRun,
          cursor,
          Math.max(16, limit - matches.length),
          projection.operatorChatProtocol,
        );
        if (candidates.length === 0) {
          exhausted = true;
          break;
        }
        const resolved = await resolveLedgerArtifactPayloadResults(candidates.map((candidate) => candidate.row), { projection: projection.operatorChatProtocol?.artifactReadProjection });
        let stoppedAtLimit = false;
        for (const candidate of candidates) {
          cursor = numberValue(candidate.row.seq, cursor);
          const payloadResult = resolved.get(candidate.row);
          if (!payloadResult || payloadResult.error || payloadResult.payload === null) {
            throw new Error(`task transcript search could not resolve movement ${cursor}: ${payloadResult?.error || "artifact payload is unavailable"}`);
          }
          const message = operatorChatSearchMessage(
            candidate.row,
            candidate.target,
            objectOrEmpty(projection.operatorChatProtocol),
            payloadResult?.payload ?? null,
          );
          if (!message) continue;
          const normalizedText = message.text.replace(/\s+/g, " ").trim().toLocaleLowerCase();
          const matchIndex = normalizedText.indexOf(needle);
          if (matchIndex < 0) continue;
          matches.push({
            task_run_id: candidate.target.taskRunId,
            source_run_id: candidate.target.sourceRunId,
            snippet: taskSearchSnippet(message.text, matchIndex, needle.length),
            message,
          });
          if (matches.length >= limit) {
            stoppedAtLimit = true;
            break;
          }
        }
        if (stoppedAtLimit) break;
        if (candidates.length < Math.max(16, limit - matches.length)) exhausted = true;
      }
      const hasOlder = !exhausted && cursor > 0 && readIndexedTaskSearchCandidates(
        index,
        searchScope.targetsBySourceRun,
        cursor,
        1,
        projection.operatorChatProtocol,
      ).length > 0;
      return {
        matches,
        has_older: hasOlder,
        next_before_seq: hasOlder ? cursor : -1,
        index_watermark: index.watermark,
        scope: {
          kind: "octopus_os_task_transcripts",
          task_count: searchScope.taskCount,
          run_count: searchScope.targetsBySourceRun.size,
          message_classes: ["operator", "terminal_agent", "terminal_error"],
        },
      };
    } finally {
      index.database.close();
    }
  }

  function operatorChatSearchScope(projection, requestedTaskRunIds) {
    const scope = operatorOsScope(projection);
    const recordsByRun = new Map(arrayValue(projection.runRecords).map((record) => [
      stringValue(record.run_id),
      record,
    ]).filter(([runId]) => runId));
    const entriesByRunId = new Map([
      ...arrayValue(scope.client_runs),
      ...arrayValue(scope.read_only_runs),
    ].flatMap((row) => {
      const runId = stringValue(row.run_id);
      const record = recordsByRun.get(runId);
      const interfaceId = stringValue(row.interface_id);
      return runId && record && interfaceId
        ? [[runId, { runId, record, interfaceId }]]
        : [];
    }));
    const targetsBySourceRun = new Map();
    for (const taskRunId of requestedTaskRunIds) {
      const entry = entriesByRunId.get(taskRunId);
      if (!entry) throw new Error(`task transcript search scope contains an unavailable run: ${taskRunId}`);
      targetsBySourceRun.set(entry.runId, {
        taskRunId,
        sourceRunId: entry.runId,
        chatInterfaceId: entry.interfaceId,
      });
    }
    return { taskCount: requestedTaskRunIds.length, targetsBySourceRun };
  }

  async function operatorChatMessages(projection, runId) {
    const cached = projection.operatorChatMessagesCache.get(runId);
    if (cached?.generation === projectionGeneration) return cached.promise;
    const promise = buildOperatorChatMessages(projection, runId);
    projection.operatorChatMessagesCache.set(runId, { generation: projectionGeneration, promise });
    promise.catch(() => {
      if (projection.operatorChatMessagesCache.get(runId)?.promise === promise) {
        projection.operatorChatMessagesCache.delete(runId);
      }
    });
    return promise;
  }

  async function buildOperatorChatMessages(projection, runId) {
    const protocol = objectOrEmpty(projection.operatorChatProtocol);
    const index = await operatorChatRunIndex(projection);
    const entry = index.find((candidate) => candidate.runId === runId);
    if (!entry) return [];
    if (!entry.writable) return operatorReadOnlyRunMessages(projection, entry, protocol);
    return buildSingleOperatorChatMessages(projection, entry, protocol);
  }

  async function buildSingleOperatorChatMessages(projection, entry, protocol) {
    if (!entry) return [];
    if (!entry.writable) return operatorReadOnlyRunMessages(projection, entry, protocol);
    const runId = entry.runId;
    const messages = [];
    const operatorResponses = new Map();
    const turnControlOutcomes = new Map();
    const interfaceRoles = new Map(baseInterfaceRows(projection).map((descriptor) => [
      stringValue(descriptor.rule_id ?? descriptor.id),
      stringValue(descriptor.role),
    ]).filter(([interfaceId]) => interfaceId));
    const toolCallOrigins = new Map();
    const activityOriginFor = (row, payload, eventKind) => {
      const sourceInterfaceId = movementBoundaryInterface(row, "from");
      const callId = stringValue(payload?.call_id ?? payload?.correlation_id);
      let activityOrigin = {
        interface_id: sourceInterfaceId,
        interface_role: interfaceRoles.get(sourceInterfaceId) ?? "",
      };
      if (eventKind === "dynamic_tool" && callId) {
        if (stringValue(payload?.status) === "started") {
          toolCallOrigins.set(callId, activityOrigin);
        } else {
          activityOrigin = toolCallOrigins.get(callId) ?? activityOrigin;
        }
      }
      return activityOrigin;
    };
    const projectedEventDataFor = (payload, eventKind, row) => ({
      ...operatorChatEventData(payload, eventKind, row),
      _activity_origin: activityOriginFor(row, payload, eventKind),
    });
    const payloads = await resolveLedgerArtifactPayloads([
      ...entry.inbound,
      ...entry.outbound,
    ], false, { projection: protocol.artifactReadProjection });
    const expandedOutbound = operatorChatExpandedStreamRows(entry.outbound, payloads, entry.runId);
    const recordedInputs = new Map();
    const nativeRecordedInputs = new Map();
    const runtimeProvider = operatorChatRuntimeProviderId(entry);
    for (const row of expandedOutbound) {
      if (ledgerArtifactLabel(row) === "turn.completed" && movementBoundaryInterface(row, "from") === runtimeProvider) {
        for (const input of nativeTurnRecordedInputs(payloads.get(row))) {
          if (!nativeRecordedInputs.has(input.client_id)) nativeRecordedInputs.set(input.client_id, {
            ...input, movement_seq: row.seq, artifact_id: ledgerArtifactId(row),
          });
        }
      }
      if (row.__stream?.event_kind !== "input_recorded") continue;
      const receipt = payloads.get(row);
      if (receipt.provider_interface === runtimeProvider && !recordedInputs.has(receipt.client_id)) {
        recordedInputs.set(receipt.client_id, operatorChatEventData(receipt, "input_recorded", row));
      }
    }
    for (const row of entry.inbound) {
      const label = ledgerArtifactLabel(row);
      if (![protocol.approvalResponseLabel, protocol.elicitationResponseLabel, protocol.userInputResponseLabel].includes(label)) continue;
      const payload = payloads.get(row);
      const key = operatorChatResponseKey(label, payload, protocol);
      if (key) operatorResponses.set(key, payload);
    }
    for (const row of entry.outbound) {
      const label = ledgerArtifactLabel(row);
      if (!["turn.control.acknowledged", "turn.control.failed"].includes(label)) continue;
      const outcome = operatorTurnControlOutcome(payloads.get(row), row);
      if (!outcome) continue;
      const previous = turnControlOutcomes.get(outcome.controlId);
      if (!previous || numberValue(previous.row?.seq, 0) < numberValue(row.seq, 0)) {
        turnControlOutcomes.set(outcome.controlId, outcome);
      }
    }
    const ordered = [
      ...entry.inbound.map((row) => ({ direction: "in", row })),
      ...expandedOutbound.map((row) => ({ direction: "out", row })),
    ].sort((left, right) => (
      rowTimeMs(left.row) - rowTimeMs(right.row)
      || numberValue(left.row.seq, 0) - numberValue(right.row.seq, 0)
    ));
    let turnStartedAtMs = 0;
    const assistantMessages = new Map();
    let turnOpen = false;
    const incrementalMessages = new Map();
    for (const movement of ordered) {
      const row = movement.row;
      const payload = payloads.get(row);
      const createdAtMs = rowTimeMs(row);
      if (movement.direction === "in") {
        if (ledgerArtifactLabel(row) !== protocol.inputLabel) continue;
        const text = operatorChatTurnText(payload, "in");
        if (!text) continue;
        const steering = turnOpen;
        if (!turnOpen) {
          turnStartedAtMs = createdAtMs;
          assistantMessages.clear();
        }
        turnOpen = true;
        const inboundEventKind = steering ? "steering" : "message";
        const controlOutcome = steering
          ? turnControlOutcomes.get(stringValue(payload?.message_id)) ?? null
          : null;
        const recordedInput = steering ? recordedInputs.get(stringValue(payload?.message_id)) : null;
        const nativeCandidate = steering ? nativeRecordedInputs.get(stringValue(payload?.message_id)) : null;
        const nativeRecordedInput = nativeCandidate?.movement_seq > row.seq ? nativeCandidate : null;
        messages.push({
          id: stringValue(payload?.message_id) || ledgerArtifactId(row) || `${runId}:in:${row.source_line}`,
          run_id: runId,
          role: "user",
          label: ledgerArtifactLabel(row) || protocol.inputLabel,
          direction: "in",
          event_kind: inboundEventKind,
          event_data: {
            ...projectedEventDataFor(payload, inboundEventKind, row),
            ...(steering ? {
              steering_status: recordedInput || nativeRecordedInput ? "input_recorded" : controlOutcome?.status ?? "submitted",
              steering_provider_interface: runtimeProvider,
              steering_input_recorded: recordedInput ?? null,
              steering_native_input: nativeRecordedInput ?? null,
              steering_control: controlOutcome
                ? operatorChatEventData(controlOutcome.payload, "turn_control", controlOutcome.row)
                : null,
            } : {}),
          },
          text,
          attachments: operatorChatAttachments(payload),
          created_at_ms: createdAtMs,
          movement_seq: numberValue(row.seq, 0),
          artifact_id: ledgerArtifactId(row),
          content_hash: contentHashHex(row?.artifact?.content),
          interface_id: movementBoundaryInterface(row, "from"),
          latency_ms: 0,
          terminal: true,
        });
        continue;
      }
      const label = ledgerArtifactLabel(row) || protocol.terminalOutputLabel;
      if (arrayValue(protocol.hiddenActivityLabels).includes(label)) continue;
      const providerFailure = operatorChatProviderFailureActivity(entry, row, payload);
      const eventKind = row.__stream?.event_kind ?? providerFailure?.eventKind ?? operatorChatEventKind(label, protocol, payload);
      const activityPayload = providerFailure?.payload ?? payload;
      const text = providerFailure?.text ?? operatorChatEventText(payload, eventKind);
      const latencyMs = turnStartedAtMs ? Math.max(0, createdAtMs - turnStartedAtMs) : 0;
      const requestKey = operatorChatRequestKey(eventKind, activityPayload);
      const projectedEventData = projectedEventDataFor(activityPayload, eventKind, row);
      const eventData = requestKey
        ? { ...projectedEventData, resolved: operatorResponses.has(requestKey), response: operatorResponses.get(requestKey) ?? null }
        : projectedEventData;
      if (eventKind === "message_delta" || eventKind === "commentary_delta") {
        if (!text) continue;
        const streamed = {
          id: operatorChatProjectedRowId(row, `${runId}:stream:${row.source_line}`),
          run_id: runId,
          role: "assistant",
          label,
          direction: "out",
          event_kind: eventKind,
          event_data: eventData,
          text,
          error: false,
          terminal: false,
          created_at_ms: createdAtMs,
          movement_seq: numberValue(row.seq, 0),
          artifact_id: ledgerArtifactId(row),
          content_hash: contentHashHex(row?.artifact?.content),
          interface_id: movementBoundaryInterface(row, "to"),
          latency_ms: latencyMs,
        };
        appendOperatorChatAssistantMessage(messages, assistantMessages, streamed);
        continue;
      }
      const incrementalKey = operatorChatIncrementalEventKey(activityPayload, eventKind);
      if (incrementalKey) {
        const current = incrementalMessages.get(incrementalKey);
        if (current && operatorChatStreamPositionCovered(current.event_data, eventData)) continue;
        if (current) {
          if (text) current.text += text;
          current.event_data = {
            ...objectOrEmpty(current.event_data),
            ...eventData,
            _event: {
              ...objectOrEmpty(current.event_data?._event),
              ...objectOrEmpty(eventData?._event),
            },
          };
          current.movement_seq = numberValue(row.seq, current.movement_seq);
          current.artifact_id = ledgerArtifactId(row) || current.artifact_id;
          current.content_hash = contentHashHex(row?.artifact?.content) || current.content_hash;
          current.latency_ms = latencyMs;
        } else {
          const streamed = {
            id: `${runId}:${incrementalKey}`,
            run_id: runId,
            role: "assistant",
            label,
            direction: "out",
            event_kind: eventKind,
            event_data: eventData,
            text,
            error: false,
            terminal: false,
            created_at_ms: createdAtMs,
            movement_seq: numberValue(row.seq, 0),
            artifact_id: ledgerArtifactId(row),
            content_hash: contentHashHex(row?.artifact?.content),
            interface_id: movementBoundaryInterface(row, "to"),
            latency_ms: latencyMs,
          };
          incrementalMessages.set(incrementalKey, streamed);
          messages.push(streamed);
        }
        continue;
      }
      const terminalError = Boolean(providerFailure) || label === protocol.errorOutputLabel;
      const controlOutcome = eventKind === "turn_control" ? operatorTurnControlOutcome(activityPayload, row) : null;
      const isError = terminalError || controlOutcome?.status === "failed";
      const terminal = label === protocol.terminalOutputLabel
        || (terminalError && activityPayload?.will_retry !== true)
        || eventKind === "interrupted_result"
        || operatorTurnControlEndsTurn(controlOutcome);
      appendOperatorChatAssistantMessage(messages, assistantMessages, {
        id: operatorChatProjectedRowId(row, `${runId}:out:${row.source_line}`),
        run_id: runId,
        role: "assistant",
        label,
        direction: "out",
        event_kind: eventKind,
        event_data: eventData,
        text,
        error: isError,
        terminal,
        created_at_ms: createdAtMs,
        movement_seq: numberValue(row.seq, 0),
        artifact_id: ledgerArtifactId(row),
        content_hash: contentHashHex(row?.artifact?.content),
        interface_id: movementBoundaryInterface(row, "to"),
        latency_ms: latencyMs,
      });
      if (controlOutcome?.status === "acknowledged" && controlOutcome.action === "resume") {
        turnStartedAtMs = createdAtMs;
        assistantMessages.clear();
        turnOpen = true;
      }
      if (operatorTurnControlEndsTurn(controlOutcome)) turnOpen = false;
      if (terminal) turnOpen = false;
    }
    return messages;
  }

  async function operatorReadOnlyRunMessages(projection, entry, protocol) {
    const presentedArtifactVersions = new Set();
    const sourceRows = [...entry.activity].sort(compareMovementOrder).filter((row) => {
      const artifactId = ledgerArtifactId(row);
      if (!artifactId) return true;
      const artifactVersion = `${artifactId}:${contentHashHex(row?.artifact?.content)}`;
      if (presentedArtifactVersions.has(artifactVersion)) return false;
      presentedArtifactVersions.add(artifactVersion);
      return true;
    });
    const payloads = await resolveLedgerArtifactPayloads(sourceRows, false, { projection: protocol.artifactReadProjection });
    const ordered = operatorChatExpandedStreamRows(sourceRows, payloads, entry.runId).sort(compareMovementOrder);
    const interfaceRoles = new Map(baseInterfaceRows(projection).map((descriptor) => [
      stringValue(descriptor.rule_id ?? descriptor.id),
      stringValue(descriptor.role),
    ]).filter(([interfaceId]) => interfaceId));
    const toolCallOrigins = new Map();
    const messages = [];
    let turnOpen = false;
    let turnStartedAtMs = 0;
    const assistantMessages = new Map();
    const incrementalMessages = new Map();
    for (const row of ordered) {
      const artifactId = ledgerArtifactId(row);
      const contentHash = contentHashHex(row?.artifact?.content);
      const label = ledgerArtifactLabel(row);
      if (arrayValue(protocol.hiddenActivityLabels).includes(label)) continue;
      const payload = payloads.get(row);
      const createdAtMs = rowTimeMs(row);
      const sourceInterfaceId = movementBoundaryInterface(row, "from");
      const input = label === protocol.inputLabel && sourceInterfaceId === entry.chatInterfaceId;
      if (input) {
        const text = operatorChatTurnText(payload, "in");
        if (!text) continue;
        const steering = turnOpen;
        if (!turnOpen) {
          turnStartedAtMs = createdAtMs;
          assistantMessages.clear();
        }
        turnOpen = true;
        messages.push({
          id: stringValue(payload?.message_id) || artifactId || `${entry.runId}:in:${row.source_line}`,
          run_id: entry.runId,
          role: "user",
          label,
          direction: "in",
          event_kind: steering ? "steering" : "message",
          event_data: {
            ...operatorChatEventData(payload, steering ? "steering" : "message", row),
            read_only_run: true,
            ...(steering ? { steering_provider_interface: operatorChatRuntimeProviderId(entry) } : {}),
          },
          text,
          attachments: operatorChatAttachments(payload),
          error: false,
          terminal: true,
          created_at_ms: createdAtMs,
          movement_seq: numberValue(row.seq, 0),
          artifact_id: artifactId,
          content_hash: contentHash,
          interface_id: sourceInterfaceId,
          latency_ms: 0,
        });
        continue;
      }
      const providerFailure = operatorChatProviderFailureActivity(entry, row, payload);
      const projected = row.__stream?.event_kind === "input_recorded"
        ? { eventKind: "input_recorded", payload, text: "", error: false }
        : providerFailure ?? operatorChatActivityPayload(payload, label, protocol);
      const callId = stringValue(projected.payload?.call_id ?? projected.payload?.correlation_id);
      let activityOrigin = {
        interface_id: sourceInterfaceId,
        interface_role: interfaceRoles.get(sourceInterfaceId) ?? "",
      };
      if (projected.eventKind === "dynamic_tool" && callId) {
        if (stringValue(projected.payload?.status) === "started") {
          toolCallOrigins.set(callId, activityOrigin);
        } else {
          activityOrigin = toolCallOrigins.get(callId) ?? activityOrigin;
        }
      }
      const terminalError = Boolean(providerFailure) || label === protocol.errorOutputLabel;
      const controlOutcome = projected.eventKind === "turn_control"
        ? operatorTurnControlOutcome(payload, row)
        : null;
      const terminal = label === protocol.terminalOutputLabel
        || (terminalError && payload?.will_retry !== true)
        || projected.eventKind === "interrupted_result"
        || operatorTurnControlEndsTurn(controlOutcome);
      const eventData = {
        ...operatorChatEventData(projected.payload, projected.eventKind, row),
        _activity_origin: activityOrigin,
        ...(terminal ? {} : { _run_activity: true }),
        read_only_run: true,
      };
      const activityOriginRole = stringValue(activityOrigin.interface_role).toLowerCase();
      if (projected.eventKind === "dynamic_tool" && activityOriginRole && activityOriginRole !== "provider") {
        continue;
      }
      const projectedText = projected.eventKind.endsWith("_delta")
        ? projected.text
        : projected.eventKind === "compaction"
          ? ""
          : projected.eventKind === "message"
            ? projected.text || operatorChatTurnText(projected.payload, "out")
            : projected.text || operatorChatTurnText(projected.payload, "out") || label;
      if (projected.eventKind === "message_delta" || projected.eventKind === "commentary_delta") {
        if (!projectedText) continue;
        const responseEventData = { ...eventData, _run_activity: false };
        const streamed = {
          id: operatorChatProjectedRowId(row, `${entry.runId}:stream:${row.source_line}`),
          run_id: entry.runId,
          role: "assistant",
          label,
          direction: "out",
          event_kind: projected.eventKind,
          event_data: responseEventData,
          text: projectedText,
          attachments: [],
          error: false,
          terminal: false,
          created_at_ms: createdAtMs,
          movement_seq: numberValue(row.seq, 0),
          artifact_id: artifactId,
          content_hash: contentHash,
          interface_id: sourceInterfaceId,
          latency_ms: turnStartedAtMs ? Math.max(0, createdAtMs - turnStartedAtMs) : 0,
        };
        appendOperatorChatAssistantMessage(messages, assistantMessages, streamed);
        continue;
      }
      const incrementalKey = operatorChatIncrementalEventKey(projected.payload, projected.eventKind);
      if (incrementalKey) {
        const current = incrementalMessages.get(incrementalKey);
        if (current && operatorChatStreamPositionCovered(current.event_data, eventData)) continue;
        if (current) {
          if (projectedText) current.text += projectedText;
          current.event_data = {
            ...objectOrEmpty(current.event_data),
            ...eventData,
            _event: {
              ...objectOrEmpty(current.event_data?._event),
              ...objectOrEmpty(eventData?._event),
            },
          };
          current.movement_seq = numberValue(row.seq, current.movement_seq);
          current.artifact_id = artifactId || current.artifact_id;
          current.content_hash = contentHash || current.content_hash;
          current.latency_ms = turnStartedAtMs ? Math.max(0, createdAtMs - turnStartedAtMs) : 0;
        } else {
          const streamed = {
            id: `${entry.runId}:${incrementalKey}`,
            run_id: entry.runId,
            role: "assistant",
            label,
            direction: "out",
            event_kind: projected.eventKind,
            event_data: eventData,
            text: projectedText,
            attachments: [],
            error: false,
            terminal: false,
            created_at_ms: createdAtMs,
            movement_seq: numberValue(row.seq, 0),
            artifact_id: artifactId,
            content_hash: contentHash,
            interface_id: sourceInterfaceId,
            latency_ms: turnStartedAtMs ? Math.max(0, createdAtMs - turnStartedAtMs) : 0,
          };
          incrementalMessages.set(incrementalKey, streamed);
          messages.push(streamed);
        }
        continue;
      }
      appendOperatorChatAssistantMessage(messages, assistantMessages, {
        id: operatorChatProjectedRowId(row, `${entry.runId}:activity:${row.source_line}`),
        run_id: entry.runId,
        role: "assistant",
        label,
        direction: "out",
        event_kind: projected.eventKind,
        event_data: eventData,
        text: label === protocol.terminalOutputLabel && projectedText === label ? "" : projectedText,
        attachments: [],
        error: projected.error || terminalError,
        terminal,
        created_at_ms: createdAtMs,
        movement_seq: numberValue(row.seq, 0),
        artifact_id: artifactId,
        content_hash: contentHash,
        interface_id: sourceInterfaceId,
        latency_ms: turnStartedAtMs ? Math.max(0, createdAtMs - turnStartedAtMs) : 0,
      });
      if (controlOutcome?.status === "acknowledged" && controlOutcome.action === "resume") {
        turnStartedAtMs = createdAtMs;
        assistantMessages.clear();
        turnOpen = true;
      }
      if (terminal) turnOpen = false;
    }
    return messages;
  }

  async function operatorProviderStatusRows(projection) {
    const latestByProvider = new Map();
    for (const entry of await operatorChatRunIndex(projection)) {
      const provider = operatorChatRunProvider(projection, entry);
      if (!provider.id) continue;
      for (const row of entry.outbound) {
        if (ledgerArtifactLabel(row) !== "account.rate_limits") continue;
        const movementSeq = numberValue(row.seq, 0);
        const observedAtMs = rowTimeMs(row);
        const previous = latestByProvider.get(provider.id);
        if (
          previous
          && (previous.observed_at_ms > observedAtMs
            || previous.observed_at_ms === observedAtMs && previous.movement_seq >= movementSeq)
        ) continue;
        const payload = objectOrEmpty(await resolveLedgerArtifactPayload(row));
        const snapshot = objectOrEmpty(payload.rate_limits ?? payload.rateLimits ?? payload);
        if (Object.keys(snapshot).length === 0) continue;
        latestByProvider.set(provider.id, {
          provider_interface_id: provider.id,
          run_id: entry.runId,
          movement_seq: movementSeq,
          observed_at_ms: observedAtMs,
          rate_limits: snapshot,
        });
      }
    }
    return Array.from(latestByProvider.values()).sort((left, right) => (
      right.observed_at_ms - left.observed_at_ms
      || right.movement_seq - left.movement_seq
      || left.provider_interface_id.localeCompare(right.provider_interface_id)
    ));
  }

  function scheduledScopeOptions(projection) {
    const scope = operatorOsScope(projection);
    const names = new Map(arrayValue(scope.project_presentations).map((row) => [stringValue(row?.ee_id), stringValue(row?.name)]));
    return arrayValue(scope.projects).map((row) => ({
      ee_id: stringValue(row.ee_id),
      name: names.get(stringValue(row.ee_id)) || stringValue(row.goal) || stringValue(row.ee_id),
    })).filter((row) => row.ee_id);
  }

  async function scheduledInterfaceRows(projection) {
    const rows = [];
    const boundInterfaces = new Set(arrayValue(projection.runRecords).flatMap((record) => (
      [...runInterfaceReferences(record)]
    )).filter(Boolean));
    const descriptors = baseInterfaceRows(projection).filter((descriptor) => (
      arrayValue(arrayValue(descriptor.activation_sidecars)[0]?.activations)
        .some((activation) => activationDriverRef(activation) === "cron_life")
    ));
    const indexedEvidence = descriptors.length > 0
      ? await indexedArtifactLabelRows(projection, "cron.current_time")
      : [];
    if (indexedEvidence === null) await refreshLedgerProjection(projection);
    const evidenceRows = (indexedEvidence ?? projection.ledgerRows).filter((movement) => (
      ledgerArtifactLabel(movement) === "cron.current_time"
      && isAdmittedInterfaceArtifactMovement(movement)
    ));
    for (const descriptor of descriptors) {
      const interfaceId = stringValue(descriptor.rule_id ?? descriptor.id);
      const sidecar = arrayValue(descriptor.activation_sidecars)[0];
      const activationPath = stringValue(sidecar?.descriptor_path);
      let activationDigest = "";
      try {
        activationDigest = crypto.createHash("sha256").update(await fs.readFile(activationPath)).digest("hex");
        const parsed = parseCronActivationDocument({
          schema_version: sidecar.schema_version,
          interface: sidecar.interface,
          activations: sidecar.activations,
        }, interfaceId);
        const schedule = parsed.config.schedule;
        const runTemplate = parsed.config.run_template;
        const trigger = runTemplate.checkpoints.find((checkpoint) => interfaceBindingId(checkpoint.interface_binding) === interfaceId);
        const cloneEligible = parsed.config.target.mode === "new_run"
          || arrayValue(trigger?.outbound_interface_bindings).some((value) => interfaceBindingId(value));
        const template = !boundInterfaces.has(interfaceId);
        if (template) {
          if (parsed.activation.enabled !== false) throw new Error(`scheduled interface '${interfaceId}' is not bound to a run`);
          if (!cloneEligible) continue;
        }
        const evidence = template
          ? { lastRunAtMs: null, lastMovementSeq: null, nextRunAtMs: null, nextRunSource: "", inactivityState: null, deliveries: [] }
          : await scheduledInterfaceEvidence(projection, interfaceId, parsed.config, evidenceRows);
        rows.push({
          interface_id: interfaceId,
          display_name: scheduledInterfaceDisplayName(interfaceId, runTemplate.id),
          template,
          status: template ? "template" : parsed.activation.enabled === false ? "disabled" : "enabled",
          valid: true,
          activation_digest: activationDigest,
          activation_path: activationPath,
          descriptor_path: stringValue(descriptor.descriptor_path),
          schedule: schedule.inactivity != null
            ? { type: "inactivity", ...schedule.inactivity }
            : schedule.due_at != null
            ? { type: "once", due_at: schedule.due_at }
            : schedule.cron == null
              ? { type: "interval", interval_seconds: schedule.interval_seconds }
              : { type: "cron", cron: schedule.cron },
          timezone: schedule.timezone,
          fire_on_start: schedule.fire_on_start,
          instructions: schedule.reminder_default,
          reminder_env: schedule.reminder_env,
          target: parsed.config.target,
          run_template: {
            run_id: runTemplate.id,
            ee_id: runTemplate.ee,
            worker_interface_id: runTemplate.worker,
            checkpoint_count: runTemplate.checkpoints.length,
            outbound_interface_ids: arrayValue(trigger?.outbound_interface_bindings).map(interfaceBindingId).filter(Boolean),
            artifact_binding_count: arrayValue(trigger?.artifact_bindings).length,
          },
          clone_eligible: cloneEligible,
          last_run_at_ms: evidence.lastRunAtMs,
          last_movement_seq: evidence.lastMovementSeq,
          next_run_at_ms: parsed.activation.enabled === false ? null : evidence.nextRunAtMs,
          next_run_source: parsed.activation.enabled === false ? "" : evidence.nextRunSource,
          inactivity_state: evidence.inactivityState,
          deliveries: evidence.deliveries,
        });
      } catch (error) {
        rows.push({
          interface_id: interfaceId,
          display_name: scheduledInterfaceDisplayName(interfaceId, ""),
          status: "invalid",
          valid: false,
          activation_digest: activationDigest,
          activation_path: activationPath,
          descriptor_path: stringValue(descriptor.descriptor_path),
          error: String(error?.message ?? error),
        });
      }
    }
    return rows.sort((left, right) => (
      numberValue(left.next_run_at_ms, Number.MAX_SAFE_INTEGER) - numberValue(right.next_run_at_ms, Number.MAX_SAFE_INTEGER)
      || stringValue(left.display_name).localeCompare(stringValue(right.display_name))
    ));
  }

  async function scheduledInterfaceEvidence(projection, interfaceId, config, evidenceRows) {
    let lastRunAtMs = 0;
    let lastMovementSeq = 0;
    const deliveries = [];
    const deadline = config.schedule.due_at == null ? null : parseScheduledDueAt(config.schedule.due_at);
    for (const movement of evidenceRows) {
      if (
        movementBoundaryInterface(movement, "from") !== interfaceId
      ) continue;
      if (deadline !== null) {
        const payload = objectOrEmpty(await resolveLedgerArtifactPayload(movement));
        if (payload.schedule_kind !== "one_shot" || typeof payload.due_at !== "string" || parseScheduledDueAt(payload.due_at) !== deadline) continue;
      }
      const at = rowTimeMs(movement);
      const seq = numberValue(movement.seq, 0);
      if (at > lastRunAtMs || at === lastRunAtMs && seq > lastMovementSeq) {
        lastRunAtMs = at;
        lastMovementSeq = seq;
      }
      deliveries.push({ seq, at_ms: at, run_id: movementRunId(movement) });
    }
    const schedule = config.schedule;
    let nextRunAtMs = null;
    let nextRunSource = "";
    let inactivityState = null;
    if (schedule.inactivity != null) {
      const document = await readOptionalRegularJsonDocument(path.join(projection.octopusHome, "interfaces", interfaceId, "cron-inactivity-state.json"), "inactivity trigger state");
      const state = document?.document;
      if (state && (state.schema_version !== 1 || !["idle", "waiting", "prepared", "admitted"].includes(state.status))) throw new Error("inactivity trigger state has an invalid schema or status");
      if (state && ["waiting", "prepared"].includes(state.status) && (!Number.isSafeInteger(state.next_due_unix_ms) || state.next_due_unix_ms < 0)) throw new Error("inactivity trigger state has an invalid deadline");
      const current = state?.schema_version === 1 && stableJson(state.inactivity ?? null) === stableJson(schedule.inactivity);
      if (current && ["waiting", "prepared"].includes(state.status)
        && Number.isSafeInteger(state.next_due_unix_ms) && state.next_due_unix_ms >= 0) {
        nextRunAtMs = state.next_due_unix_ms;
        nextRunSource = "driver_state";
      }
      inactivityState = {
        status: stringValue(state?.status) || "unstarted",
        current: Boolean(current) || ["idle", "admitted"].includes(stringValue(state?.status)),
        last_activity_at_ms: Number.isSafeInteger(state?.activity?.recorded_at_unix_ms) ? state.activity.recorded_at_unix_ms : null,
        last_activity_run_id: stringValue(state?.activity?.run),
      };
    }
    if (deadline !== null && !lastRunAtMs) {
      nextRunAtMs = Number(deadline / 1_000_000n);
      nextRunSource = "schedule";
    }
    if (schedule.interval_seconds != null) {
      const fingerprint = cronScheduleFingerprint(interfaceId, config);
      const statePath = path.join(projection.octopusHome, "interfaces", interfaceId, "cron-interval-state.json");
      const state = await readJsonFile(statePath);
      if (
        state
        && state.schema_version === 1
        && state.schedule_fingerprint === fingerprint
        && ["waiting", "prepared"].includes(state.status)
        && typeof state.next_due_unix_ns === "number"
        && Number.isFinite(state.next_due_unix_ns)
        && Number.isInteger(state.next_due_unix_ns)
        && state.next_due_unix_ns >= 0
      ) {
        nextRunAtMs = Math.floor(state.next_due_unix_ns / 1_000_000);
        nextRunSource = "driver_state";
      }
    }
    if (!lastRunAtMs && config.schedule.cron != null) {
      const state = await readJsonFile(path.join(projection.octopusHome, "interfaces", interfaceId, "cron-state.json"));
      const parsed = typeof state?.last_cron_minute === "string" ? Date.parse(state.last_cron_minute) : NaN;
      if (Number.isFinite(parsed)) lastRunAtMs = parsed;
    }
    const clientRuns = new Map(arrayValue(operatorOsScope(projection).client_runs).map((row) => [stringValue(row.run_id), row]));
    const recent = deliveries.sort((left, right) => right.seq - left.seq).slice(0, 8);
    for (const delivery of recent) {
      delivery.chat_interface_id = stringValue(clientRuns.get(delivery.run_id)?.interface_id);
      delivery.journal = delivery.run_id ? await readChangeJournal(projection, delivery.run_id) : null;
    }
    return {
      lastRunAtMs: lastRunAtMs || null,
      lastMovementSeq: lastMovementSeq || null,
      nextRunAtMs,
      nextRunSource,
      inactivityState,
      deliveries: recent,
    };
  }

  function cachedTopologySnapshot(projection, params) {
    const key = JSON.stringify([
      projection.generation,
      params.include_ledger !== false,
      [...new Set(arrayValue(params.focused_run_ids).map(stringValue).filter(Boolean))].sort(),
    ]);
    const cached = projection.topologySnapshotCache;
    if (cached?.key === key) return cached.promise;
    const promise = topologySnapshot(projection, params);
    projection.topologySnapshotCache = { key, promise };
    promise.catch(() => {
      if (projection.topologySnapshotCache?.promise === promise) projection.topologySnapshotCache = null;
    });
    return promise;
  }

  async function topologySnapshot(projection, params) {
    const includeLedger = params.include_ledger !== false;
    const runs = limitRows(runRows(projection, includeLedger), { limit: 1_000 });
    const outcomeIndex = includeLedger ? requiredVerifiedArtifactOutcomeIndex(projection) : new Map();
    const recordByRun = new Map(arrayValue(projection.runRecords).map((record) => [record.run_id, record]));
    const bindings = bindingRows(projection);
    const artifacts = includeLedger ? artifactRows(projection).map(topologyArtifactRow) : [];
    const marks = includeLedger ? markRows(projection).map(withoutRawProjectionEvidence) : [];
    const envelopes = includeLedger ? envelopeRows(projection) : [];
    const bindingsByRun = groupRowsBy(bindings, "run_id");
    const artifactsByRun = groupRowsBy(artifacts, "run_id");
    const marksByRun = groupRowsBy(marks, "run_id");
    const envelopesByRun = new Map();
    for (const row of envelopes) {
      const runId = stringValue(row.run_id ?? row.meta?.run_id ?? row.payload?.run_id);
      if (!runId) continue;
      const group = envelopesByRun.get(runId) ?? [];
      group.push(row);
      envelopesByRun.set(runId, group);
    }
    const requestedRunIds = new Set(arrayValue(params.focused_run_ids).map(stringValue).filter(Boolean));
    for (const run of runs.slice(0, 12)) requestedRunIds.add(stringValue(run.run_id));
    const runDetails = {};
    for (const runId of requestedRunIds) {
      const run = runs.find((row) => stringValue(row.run_id) === runId);
      if (!run) continue;
      const workerEnvironments = workerEnvironmentRows(projection)
        .filter((row) => stringValue(row.run_id) === runId);
      runDetails[runId] = {
        detail: {
          run,
          bindings: bindingsByRun.get(runId) ?? [],
          artifacts: artifactsByRun.get(runId) ?? [],
          marks: marksByRun.get(runId) ?? [],
          checkpoints: recordCheckpointRowsFor(projection, runId, outcomeIndex, recordByRun)
            ?? (includeLedger ? runCheckpointRows(projection, runId) : []),
          worker_environments: workerEnvironments,
          ...(workerEnvironments.length === 1 ? { worker_environment: workerEnvironments[0] } : {}),
        },
        trace: includeLedger
          ? { events: limitRows(envelopesByRun.get(runId) ?? [], { limit: 80 }) }
          : { envelopes: [] },
      };
    }
    return {
      generation: projection.generation,
      snapshotRaw: projectionSnapshot(projection),
      eeRaw: { ee_instances: limitRows(eeInstanceRows(projection, includeLedger), { limit: 300 }) },
      runsRaw: { runs, invalid_run_records: projection.invalidRunRecords },
      runCheckpointsRaw: {
        run_checkpoints: runs.map((run) => ({
          run_id: stringValue(run.run_id),
          checkpoints: recordCheckpointRowsFor(projection, stringValue(run.run_id), outcomeIndex, recordByRun)
            ?? runCheckpointRows(projection, stringValue(run.run_id)),
        })),
      },
      interfacesRaw: { interfaces: interfaceRows(projection) },
      bindingsRaw: { bindings: bindings.filter((row) => stringValue(row.status) === "projected") },
      artifactsRaw: { artifacts: limitRows(artifacts, { limit: 300 }) },
      marksRaw: { marks: limitRows(marks, { limit: 300 }) },
      workerEnvironmentsRaw: { worker_environments: limitRows(workerEnvironmentRows(projection), { limit: 1_000 }) },
      movementsRaw: includeLedger
        ? { movements: (await recentMovementRows(projection, 1_000)).map(withoutRawProjectionEvidence), total: projection.ledgerRows.length }
        : { movements: [], total: 0 },
      runDetailsRaw: runDetails,
    };
  }

  function interfaceRows(projection) {
    const rows = baseInterfaceRows(projection);
    const targetsByInterface = groupRowsBy(
      interfaceArtifactTransferTargetRowsFor(rows),
      "interface_id",
    );
    return rows.map((row) => {
      const interfaceId = stringValue(row.rule_id ?? row.id);
      const targetSummary = summarizeInterfaceArtifactTransferTargets(
        targetsByInterface.get(interfaceId) ?? [],
        objectOrEmpty(row.artifact_transfer),
      );
      return {
        ...row,
        artifact_transfer_protocol_status: targetSummary.status,
        artifact_transfer_target_count: targetSummary.target_count,
        artifact_transfer_ready_targets: targetSummary.ready_targets,
        artifact_transfer_target_reason: targetSummary.reason,
      };
    });
  }

  function interfaceArtifactTransferTargetRows(projection) {
    return interfaceArtifactTransferTargetRowsFor(baseInterfaceRows(projection));
  }

  function interfaceArtifactTransferTargetRowsFor(interfaces) {
    const rows = [];
    for (const iface of interfaces) {
      const interfaceId = stringValue(iface.rule_id ?? iface.id);
      if (!interfaceId) continue;
      const transfer = objectOrEmpty(iface.artifact_transfer);
      const activations = interfaceActivationEntries(iface.activation_sidecars);
      const transferPath = stringValue(transfer.path);
      const transferStatus = stringValue(transfer.status) || "unknown";
      const transferReason = stringValue(transfer.reason);
      const filesystemActivations = activations.filter((activation) => activation.ref === "app_server_transport");
      if (filesystemActivations.length === 0) {
        rows.push(interfaceArtifactTransferTargetRow({
          interfaceId,
          iface,
          activation: {},
          transferPath,
          status: "not_declared",
          reason: transferReason || "no activation declares an artifact hand-in socket",
        }));
        continue;
      }
      for (const activation of filesystemActivations) {
        if (transferStatus !== "ready") {
          rows.push(interfaceArtifactTransferTargetRow({
            interfaceId,
            iface,
            activation,
            transferPath,
            status: transferStatus === "not_declared" ? "not_declared" : "unavailable",
            reason: transferReason || "artifact hand-in socket is not ready",
          }));
          continue;
        }
        rows.push(interfaceArtifactTransferTargetRow({
          interfaceId,
          iface,
          activation,
          transferPath,
          status: "ready",
          reason: "declared artifact hand-in socket is ready",
        }));
      }
    }
    return dedupeRows(rows, "target_id");
  }

  function artifactRows(projection) {
    return projectionMemo(projection, "ledger", "artifactRows", () => {
      const registryArtifacts = descriptorRows(projection, "artifact").map((row) => ({
        ...row,
        artifact_id: stringValue(row.id) || stringValue(row.name),
        label: stringValue(row.label) || stringValue(row.id) || stringValue(row.name),
        status: "registered",
        route_label: stringValue(row.route_label),
        created_at: stringValue(row.created_at),
      }));
      return dedupeRows([...registryArtifacts, ...ledgerArtifactRows(projection)], "artifact_id");
    });
  }

  function ledgerArtifactRows(projection) {
    return projectionMemo(projection, "ledger", "ledgerArtifactRows", () => {
      const ledgerArtifacts = new Map();
      for (const row of envelopeRows(projection)) {
        const artifactId = ledgerArtifactId(row);
        if (!artifactId) continue;
        const coordinates = artifactMovementCoordinates(row);
        const previewRank = isAdmittedInterfaceArtifactMovement(row)
          ? (coordinates.runId && coordinates.interfaceId ? 2 : 1)
          : 0;
        const current = ledgerArtifacts.get(artifactId);
        if (
          !current
          || previewRank > current.previewRank
          || previewRank === current.previewRank && compareArtifactMovementNewest(row, current.row) < 0
        ) {
          ledgerArtifacts.set(artifactId, { coordinates, previewRank, row });
        }
      }
      return [...ledgerArtifacts.entries()].map(([artifactId, entry]) => {
        const runtime = objectOrEmpty(objectOrEmpty(entry.row.invocation_metadata).runtime);
        return {
          artifact_id: artifactId,
          label: ledgerArtifactLabel(entry.row),
          route_label: "",
          run_id: entry.coordinates.runId,
          interface_id: entry.coordinates.interfaceId,
          ee_id: stringValue(runtime.ee),
          worker: stringValue(runtime.worker),
          status: "recorded",
          created_at: timestampOf(entry.row),
          seq: numberValue(entry.row.seq, null),
          movement_kind: stringValue(entry.row.kind),
          previewable: entry.previewRank === 2,
          source: "ledger",
          raw: entry.row,
        };
      }).sort((left, right) => (
        compareArtifactMovementNewest(left.raw, right.raw)
        || stringValue(left.artifact_id).localeCompare(stringValue(right.artifact_id))
      ));
    });
  }

  function artifactListProjection(projection, params) {
    const requestedSource = stringValue(params?.source);
    if (requestedSource && !["ledger", "registry"].includes(requestedSource)) {
      throw new Error("artifact list source must be ledger or registry");
    }
    if (params?.previewable_only !== undefined && typeof params.previewable_only !== "boolean") {
      throw new Error("artifact list previewable_only must be a boolean");
    }
    const parsedLimit = params?.limit === undefined ? 1_000 : indexedUnsignedInteger(params.limit);
    if (parsedLimit === null || parsedLimit < 1 || parsedLimit > 5_000) {
      throw new Error("artifact list limit must be an integer between 1 and 5000");
    }
    const allRows = requestedSource === "ledger" ? ledgerArtifactRows(projection) : artifactRows(projection);
    const sourceRows = requestedSource
      ? allRows.filter((row) => stringValue(row.source) === requestedSource)
      : allRows;
    const matchingRows = filterByString(sourceRows, "label", params?.label);
    const previewableTotal = matchingRows.filter((row) => row.previewable === true).length;
    const unavailableTotal = matchingRows.length - previewableTotal;
    const selectedRows = params?.previewable_only === true
      ? matchingRows.filter((row) => row.previewable === true)
      : matchingRows;
    const artifacts = selectedRows.slice(0, parsedLimit).map(({ raw: _raw, ...row }) => row);
    return {
      artifacts,
      total: matchingRows.length,
      previewable_total: previewableTotal,
      unavailable_total: unavailableTotal,
      returned: artifacts.length,
      limit: parsedLimit,
      truncated: selectedRows.length > artifacts.length,
    };
  }

  function artifactLabelRows(projection) {
    const byLabel = new Map();
    for (const artifact of artifactRows(projection)) {
      const label = stringValue(artifact.label);
      if (!label || byLabel.has(label)) continue;
      byLabel.set(label, {
        label,
        lifecycle: stringValue(artifact.lifecycle) || stringValue(artifact.status) || "artifact",
        allowed_writers: arrayValue(artifact.allowed_writers),
        finalize_recommended: Boolean(artifact.finalize_recommended),
        source: stringValue(artifact.source) || "projection.artifacts",
      });
    }
    return Array.from(byLabel.values()).sort((a, b) => a.label.localeCompare(b.label));
  }

  return {
    read: readProjection,
    generation: () => projectionGeneration,
    invalidate: invalidateProjection,
    ensureWatchers: ensureProjectionWatchers,
    close: () => {
      if (projectionBroadcastTimer) clearTimeout(projectionBroadcastTimer);
      projectionBroadcastTimer = null;
      projectionBroadcastKind = "ledger";
      projectionBuild = null;
      projectionBuildPending = null;
      configurationProjectionBuild = null;
      ledgerProjectionSnapshot = null;
      ledgerCursors.clear();
      resolvedArtifactPayloads.clear();
      resolvedArtifactPayloadBytes = 0;
      for (const watcher of projectionWatchers.values()) {
        try { watcher.close(); } catch {}
      }
      projectionWatchers.clear();
      projectionWatchHealth.clear();
      stopProjectionFallbackPoller();
      for (const [file, listener] of projectionLedgerWatchers) {
        fsSync.unwatchFile(file, listener);
      }
      projectionLedgerWatchers.clear();
    },
  };
}

module.exports = {
  nativeTurnRecordedInputs,
  topologyArtifactRow,
  createProjectionStore,
  mutateApplicationPrivateConfiguration,
  mutateOctopusOsWorkspace,
  mutateScheduledInterface,
  setupTriggerEntries,
  setupTriggerProof,
  armSetupTriggers,
  readProviderInterfaceDescriptors,
  parseApplicationManifest,
  readApplicationLogoAsset,
  DESCRIPTOR_REGISTRIES,
  stringValue,
  arrayValue,
  requiredString,
  numberValue,
  objectOrEmpty,
};
