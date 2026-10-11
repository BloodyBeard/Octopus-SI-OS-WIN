const crypto = require("node:crypto");
const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const Ajv = require("ajv");
const { blake3 } = require("@noble/hashes/blake3");
const runRecordSchema = require("../../schemas/run_record.schema.json");
const { parseHomeVoiceContext } = require("./homeVoiceProjection.cjs");
const { payloadArtifactBindings } = require("./contentAddressedArtifact.cjs");
const {
  clientAgentCatalogPath,
  emptyClientAgentCatalog,
  parseClientAgentCatalog,
  readClientAgentCatalog,
  appendExactAgentWorkerAdmission,
} = require("./clientAgentCatalog.cjs");

const SAFE_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,159}$/;
const SAFE_OPERATION_ID = /^[A-Za-z0-9_-]{8,96}$/;
const DIGEST = /^[a-f0-9]{64}$/;
const transactionTails = new Map();
const runRecordValidator = new Ajv({ allErrors: true, strict: false, logger: false }).compile(runRecordSchema);

async function initializeWorkspace(options) {
  const roots = await normalizeRoots(options);
  const request = parseInitializeRequest(options?.request);
  if (typeof options?.planOperatorSurface !== "function") {
    throw new Error("workspace initialization requires the operator topology planner");
  }
  return withTransactionLock(roots.stateRoot, async () => {
    await ensureStateRoots(roots);
    await recoverWorkspaceTransactionsUnlocked(roots);
    if (await readProjectReceipt(roots, request.operationId)) {
      throw new Error(`workspace setup operation '${request.operationId}' is already a Project operation`);
    }
    const existingReceipt = await readReceipt(roots, request.operationId);
    if (existingReceipt) {
      if (existingReceipt.request_digest !== request.digest) {
        throw new Error(`workspace setup operation '${request.operationId}' belongs to another request`);
      }
      await verifyWorkspaceReceiptIdentity(roots, existingReceipt);
      return receiptOutcome(existingReceipt, false);
    }
    if (await directoryExists(path.join(roots.transactionsRoot, request.operationId))) {
      await recoverTransaction(roots, path.join(roots.transactionsRoot, request.operationId));
      const recovered = await readReceipt(roots, request.operationId);
      if (!recovered) throw new Error(`workspace setup operation '${request.operationId}' did not produce a receipt`);
      if (recovered.request_digest !== request.digest) {
        throw new Error(`workspace setup operation '${request.operationId}' recovered a different request`);
      }
      await verifyWorkspaceReceiptIdentity(roots, recovered);
      return receiptOutcome(recovered, false);
    }
    await assertWorkspaceAbsent(roots, request.root.existingEeId);
    reportProgress(options.onProgress, request.operationId, "planning", 5, "Preparing your workspace");
    const existingRoot = request.root.existingEeId
      ? await readExistingWorkspaceRoot(roots, request.root) : null;
    const bootstrap = existingRoot ? null : await readBootstrapRoot(roots);
    const materialized = materializeWorkspaceIntent(request, bootstrap, existingRoot);
    await validateWorkspaceTemplates(roots, materialized);
    const overlay = await createEeOverlay(roots, materialized.eeWrites);
    let plan;
    let projectProfiles;
    const agentPlans = [];
    try {
      if (existingRoot) await verifyExistingRootObservation({ ...roots, eeInstancesRoot: overlay }, {
        ...existingRoot, path: path.join(overlay, `${existingRoot.ee_id}.json`),
      }, true);
      plan = parseHomePlan(await options.planOperatorSurface({
        eeInstancesRoot: overlay,
        request: {
          chatId: materialized.chatId,
          cycle: materialized.cycle,
          providerInterfaceId: request.home.providerInterfaceId,
          providerModel: request.home.providerModel,
          workerInterfaceId: request.home.workerInterfaceId,
          ...(request.home.reasoningEffort ? { reasoningEffort: request.home.reasoningEffort } : {}),
          ...(request.home.serviceTier ? { serviceTier: request.home.serviceTier } : {}),
          ...(Object.keys(request.home.doorHostSlots).length ? { doorHostSlots: request.home.doorHostSlots } : {}),
          ...(request.home.contextArtifactBindings.length
            ? { contextArtifactBindings: request.home.contextArtifactBindings }
            : {}),
          ...(request.home.selectedWorkerEnvironmentKinds.length
            ? { selectedWorkerEnvironmentKinds: request.home.selectedWorkerEnvironmentKinds }
            : {}),
          eeId: materialized.rootEeId,
          clientSurfaceRole: "home",
        },
      }), request, materialized);
      projectProfiles = await preflightInitialWorkspaceProjects({
        eeInstancesRoot: overlay,
        materialized,
        operationId: request.operationId,
        planOperatorSurface: options.planOperatorSurface,
        projects: request.projects,
        providerInterfaceId: request.home.providerInterfaceId,
        providerModel: request.home.providerModel,
        homeAuthoringProfile: plan.authoringProfile,
        task: request.task,
      });
      for (const choice of request.agentPresets ?? []) {
        if (typeof options.planAgentPreset !== "function") throw new Error("Workspace Agent preset planner is unavailable");
        for (const projectKey of choice.projectKeys) {
          const operationId = `agent_${sha256(Buffer.from(`${request.operationId}\0${choice.packId}\0${choice.presetId}`)).slice(0, 32)}`;
          agentPlans.push(await options.planAgentPreset({ choice, projectEeId: requiredMapValue(materialized.projectEeIds, projectKey), operationId, eeInstancesRoot: overlay }));
        }
      }
    } finally {
      await fs.rm(overlay, { recursive: true, force: true });
    }
    const writes = await workspaceWrites(roots, request, materialized, plan, projectProfiles, agentPlans);
    if (existingRoot && writes.some((write) => write.destination === existingRoot.path)) {
      throw new Error("Workspace attachment cannot modify the selected existing EE");
    }
    for (const agent of agentPlans) await agent.verify();
    const needsBrowserNativeHost = writes.some((write) => {
      if (path.basename(write.destination) !== "interface.json" || typeof write.content !== "string") return false;
      return JSON.parse(write.content)?.interface_driver_config?.private_config?.browser_room?.transport?.wire === "browser.native.v1";
    });
    if (needsBrowserNativeHost && typeof options.planBrowserNativeHost !== "function") {
      throw new Error("Native browser workspace setup requires its verified installed package author");
    }
    const setupPlans = [];
    async function rollbackSetupPlans() {
      const failures = [];
      for (const setup of [...setupPlans].reverse()) {
        try { await setup.rollback(); } catch (error) { failures.push(error); }
      }
      if (failures.length) throw new AggregateError(failures, "Workspace capability setup rollback failed");
    }
    try {
      if (needsBrowserNativeHost) {
        const setup = await options.planBrowserNativeHost({ octopusHomeRoot: roots.octopusHomeRoot, plannedWrites: writes });
        if (typeof setup?.rollback === "function") setupPlans.push(setup);
        if (!Array.isArray(setup?.writes) || typeof setup.rollback !== "function" || setup.writes.length !== 1
          || setup.writes.some((write) => write.destination !== path.join(roots.octopusHomeRoot, "host.json") || write.publishBarrier !== false)) {
          throw new Error("Native browser setup author returned an invalid host manifest plan");
        }
        writes.unshift(...setup.writes);
      }
      const needsDesktopPreferences = writes.some((write) => path.basename(write.destination) === "desktop.json"
        && typeof write.content === "string" && JSON.parse(write.content)?.host?.application_preferences != null);
      if (needsDesktopPreferences) {
        if (typeof options.planDesktopApplicationPreferences !== "function") throw new Error("Computer Use setup requires its verified installed package author");
        const setup = await options.planDesktopApplicationPreferences({ octopusHomeRoot: roots.octopusHomeRoot, plannedWrites: writes });
        if (typeof setup?.rollback === "function") setupPlans.push(setup);
        const hostPath = path.join(roots.octopusHomeRoot, "host.json");
        const prior = writes.filter((write) => write.destination === hostPath);
        if (!Array.isArray(setup?.writes) || typeof setup.rollback !== "function" || setup.writes.length !== 1
          || setup.writes[0].destination !== hostPath || setup.writes[0].publishBarrier !== false || prior.length > 1
          || prior.length === 1 && setup.writes[0].expectedBeforeDigest !== prior[0].expectedBeforeDigest) {
          throw new Error("Computer Use setup author returned an invalid merged host manifest plan");
        }
        if (prior.length) writes.splice(writes.indexOf(prior[0]), 1, setup.writes[0]);
        else writes.unshift(setup.writes[0]);
      }
    } catch (error) {
      try { await rollbackSetupPlans(); } catch (rollbackError) { throw new AggregateError([error, rollbackError], "Workspace capability setup failed during rollback"); }
      throw error;
    }
    let transactionRoot;
    try {
      const receipt = workspaceReceipt(request, materialized, plan, writes);
      reportProgress(options.onProgress, request.operationId, "staging", 24, "Verifying the setup plan");
      transactionRoot = await stageTransaction(roots, request, writes, receipt);
    } catch (error) {
      try { await rollbackSetupPlans(); } catch (rollbackError) { throw new AggregateError([error, rollbackError], "Workspace staging rollback failed"); }
      throw error;
    }
    try {
      await commitTransaction(roots, transactionRoot, options.onProgress);
    } catch (error) {
      const durableReceipt = await readReceipt(roots, request.operationId).catch(() => null);
      if (durableReceipt && durableReceipt.request_digest === request.digest) {
        await verifyWorkspaceReceiptIdentity(roots, durableReceipt);
        return receiptOutcome(durableReceipt, true);
      }
      let journal;
      try {
        journal = await readJournal(roots, transactionRoot);
      } catch {}
      if (journal && ["published", "committed"].includes(journal.phase)) {
        try {
          await recoverTransaction(roots, transactionRoot);
          const recovered = await readReceipt(roots, request.operationId);
          if (!recovered) throw new Error("published workspace setup has no durable receipt");
          await verifyWorkspaceReceiptIdentity(roots, recovered);
          return receiptOutcome(recovered, true);
        } catch (recoveryError) {
          throw new Error(`${errorMessage(error)}; published workspace recovery failed closed: ${errorMessage(recoveryError)}`);
        }
      }
      try {
        const failures = [];
        try { await rollbackTransaction(roots, transactionRoot); } catch (failure) { failures.push(failure); }
        try { await rollbackSetupPlans(); } catch (failure) { failures.push(failure); }
        if (failures.length) throw new AggregateError(failures, "Workspace rollback did not fully complete");
      } catch (rollbackError) {
        throw new Error(`${errorMessage(error)}; workspace rollback failed closed: ${errorMessage(rollbackError)}`);
      }
      throw error;
    }
    const completed = await readReceipt(roots, request.operationId);
    if (!completed) throw new Error("workspace setup committed without a durable receipt");
    await verifyWorkspaceReceiptIdentity(roots, completed);
    return receiptOutcome(completed, true);
  });
}

async function mutateWorkspaceProject(options) {
  const roots = await normalizeRoots(options);
  const request = parseProjectMutationRequest(options?.request);
  const scope = parseProjectMutationScope(options?.scope);
  return withTransactionLock(roots.stateRoot, async () => {
    await ensureStateRoots(roots);
    await recoverWorkspaceTransactionsUnlocked(roots);
    if (await readReceipt(roots, request.operationId)) {
      throw new Error(`workspace project operation '${request.operationId}' is already a setup operation`);
    }
    const existingReceipt = await readProjectReceipt(roots, request.operationId);
    if (existingReceipt) {
      if (existingReceipt.request_digest !== request.digest) {
        throw new Error(`workspace project operation '${request.operationId}' belongs to another request`);
      }
      return projectReceiptOutcome(existingReceipt, false);
    }
    const transactionRoot = path.join(roots.transactionsRoot, request.operationId);
    if (await directoryExists(transactionRoot)) {
      await recoverTransaction(roots, transactionRoot);
      const recovered = await readProjectReceipt(roots, request.operationId);
      if (!recovered) throw new Error(`workspace project operation '${request.operationId}' did not produce a receipt`);
      if (recovered.request_digest !== request.digest) {
        throw new Error(`workspace project operation '${request.operationId}' recovered a different request`);
      }
      return projectReceiptOutcome(recovered, false);
    }
    const workspaceReceipt = await requireConfiguredWorkspace(roots, scope, request.workspace);
    const planned = request.action === "create_project"
      ? await planProjectCreate(roots, scope, request, options?.planOperatorSurface)
      : request.action === "delete_project"
        ? await planProjectDelete(roots, scope, request, workspaceReceipt)
        : await planProjectPresentationUpdate(roots, scope, request, workspaceReceipt);
    const receipt = projectMutationReceipt(request, planned);
    const staged = await stageProjectTransaction(roots, request, planned.writes, receipt);
    try {
      await commitTransaction(roots, staged);
    } catch (error) {
      const durableReceipt = await readProjectReceipt(roots, request.operationId).catch(() => null);
      if (durableReceipt && durableReceipt.request_digest === request.digest) {
        return projectReceiptOutcome(durableReceipt, true);
      }
      let journal;
      try {
        journal = await readJournal(roots, staged);
      } catch {}
      if (journal && ["published", "committed"].includes(journal.phase)) {
        try {
          await recoverTransaction(roots, staged);
          const recovered = await readProjectReceipt(roots, request.operationId);
          if (!recovered) throw new Error("published workspace Project mutation has no durable receipt");
          if (recovered.request_digest !== request.digest) {
            throw new Error(`workspace Project operation '${request.operationId}' recovered a different request`);
          }
          return projectReceiptOutcome(recovered, true);
        } catch (recoveryError) {
          throw new Error(`${errorMessage(error)}; published workspace Project recovery failed closed: ${errorMessage(recoveryError)}`);
        }
      }
      try {
        await rollbackTransaction(roots, staged);
      } catch (rollbackError) {
        throw new Error(`${errorMessage(error)}; workspace project rollback failed closed: ${errorMessage(rollbackError)}`);
      }
      throw error;
    }
    const completed = await readProjectReceipt(roots, request.operationId);
    if (!completed) throw new Error("workspace project mutation committed without a durable receipt");
    return projectReceiptOutcome(completed, true);
  });
}

async function readWorkspaceSetupState(options) {
  const roots = await normalizeRoots(options);
  return withTransactionLock(roots.stateRoot, async () => {
    await ensureStateRoots(roots);
    const recoveryErrors = [];
    try {
      await recoverWorkspaceTransactionsUnlocked(roots);
    } catch (error) {
      recoveryErrors.push(errorMessage(error));
    }
    const receipts = [];
    for (const entry of await fs.readdir(roots.receiptsRoot, { withFileTypes: true })) {
      if (!entry.isFile() || !entry.name.endsWith(".json")) {
        recoveryErrors.push(`workspace receipt root contains an invalid entry '${entry.name}'`);
        continue;
      }
      try {
        const receipt = await readReceiptFile(path.join(roots.receiptsRoot, entry.name));
        await verifyWorkspaceReceiptIdentity(roots, receipt);
        receipts.push(receipt);
      } catch (error) {
        recoveryErrors.push(errorMessage(error));
      }
    }
    receipts.sort((left, right) => right.completed_at.localeCompare(left.completed_at));
    if (receipts.length > 1) {
      recoveryErrors.push(`workspace receipt root contains ${receipts.length} configured workspaces`);
    }
    const receiptedProjectEeIds = new Set();
    for (const entry of await fs.readdir(roots.projectReceiptsRoot, { withFileTypes: true })) {
      if (!entry.isFile() || !entry.name.endsWith(".json")) {
        recoveryErrors.push(`workspace Project receipt root contains an invalid entry '${entry.name}'`);
        continue;
      }
      try {
        const receipt = await readProjectReceiptFile(path.join(roots.projectReceiptsRoot, entry.name));
        verifyProjectReceiptContract(roots, receipt);
        const workspace = receipts[0];
        if (workspace && (
          receipt.root_ee_id !== workspace.root_ee_id
          || receipt.home_run_id !== workspace.home_run_id
          || receipt.home_interface_id !== workspace.home_interface_id
        )) {
          throw new Error(`Workspace Project receipt '${entry.name}' belongs to another workspace`);
        }
        if (workspace && receipt.action === "create_project") receiptedProjectEeIds.add(receipt.outcome.ee_id);
      } catch (error) {
        recoveryErrors.push(errorMessage(error));
      }
    }
    try {
      const topology = await findWorkspaceTopology(roots);
      const expectedRoots = new Set([...receipts.map((receipt) => receipt.root_ee_id), ...receiptedProjectEeIds]);
      const expectedRuns = new Set(receipts.map((receipt) => receipt.home_run_id));
      const expectedInterfaces = new Set(receipts.map((receipt) => receipt.home_interface_id));
      const orphanedRoots = topology.operatorSpaceEeIds.filter((eeId) => !expectedRoots.has(eeId));
      const orphanedRuns = topology.homeRunIds.filter((runId) => !expectedRuns.has(runId));
      const orphanedInterfaces = topology.homeInterfaceIds.filter((interfaceId) => !expectedInterfaces.has(interfaceId));
      if (orphanedRoots.length) recoveryErrors.push(`workspace contains operator spaces without receipts: ${orphanedRoots.join(", ")}`);
      if (orphanedRuns.length) recoveryErrors.push(`workspace contains Home runs without receipts: ${orphanedRuns.join(", ")}`);
      if (orphanedInterfaces.length) recoveryErrors.push(`workspace contains Home interfaces without receipts: ${orphanedInterfaces.join(", ")}`);
    } catch (error) {
      recoveryErrors.push(errorMessage(error));
    }
    return {
      status: recoveryErrors.length ? "degraded" : receipts.length ? "configured" : "not_configured",
      receipt: receipts[0] ?? null,
      errors: recoveryErrors,
    };
  });
}

async function recoverWorkspaceTransactions(options) {
  const roots = await normalizeRoots(options);
  if (!fsSync.existsSync(roots.transactionsRoot)) return { recovered: 0 };
  return withTransactionLock(roots.stateRoot, async () => {
    await ensureStateRoots(roots);
    return recoverWorkspaceTransactionsUnlocked(roots);
  });
}

async function commitClientAuthoringTransaction(options) {
  const roots = await normalizeRoots(options);
  const plan = parseClientAuthoringPlan(options?.plan);
  return withTransactionLock(roots.stateRoot, async () => {
    await ensureStateRoots(roots);
    await recoverWorkspaceTransactionsUnlocked(roots);
    const existingReceipt = await readClientAuthoringReceipt(roots, plan.operationId);
    if (existingReceipt) {
      if (existingReceipt.label !== plan.label) {
        throw new Error(`Client authoring operation '${plan.operationId}' belongs to another request`);
      }
      await verifyClientAuthoringReceipt(roots, existingReceipt);
      return { operationId: plan.operationId, recovered: false, cleanupWarnings: [] };
    }
    const transactionRoot = await stageClientAuthoringTransaction(roots, plan);
    let recovered = false;
    try {
      await commitClientAuthoringJournal(roots, transactionRoot);
    } catch (error) {
      try {
        await recoverClientAuthoringTransaction(roots, transactionRoot);
        recovered = true;
      } catch (recoveryError) {
        throw new Error(`${errorMessage(error)}; client authoring recovery failed closed: ${errorMessage(recoveryError)}`);
      }
    }
    return { operationId: plan.operationId, recovered, cleanupWarnings: [] };
  });
}

function parseClientAuthoringPlan(value) {
  const plan = exactObject(
    value,
    ["operationId", "label", "writes", "removals"],
    ["operationId", "label", "writes", "removals"],
    "Client authoring plan",
  );
  if (!Array.isArray(plan.writes) || !plan.writes.length) {
    throw new Error("Client authoring plan must contain at least one write");
  }
  if (!Array.isArray(plan.removals)) throw new Error("Client authoring plan removals must be an array");
  const writes = plan.writes.map((value, index) => {
    const write = exactObject(
      value,
      ["destination", "expectedBeforeDigest", "content", "tag", "publishBarrier"],
      ["destination", "expectedBeforeDigest", "content", "tag", "publishBarrier"],
      `Client authoring write ${index + 1}`,
    );
    if (write.content !== null && typeof write.content !== "string") {
      throw new Error(`Client authoring write ${index + 1} content must be text or null`);
    }
    if (typeof write.publishBarrier !== "boolean") {
      throw new Error(`Client authoring write ${index + 1} has an invalid publish barrier`);
    }
    return {
      destination: requiredAbsolutePath(write.destination, `Client authoring write ${index + 1} destination`),
      expectedBeforeDigest: optionalDigest(
        write.expectedBeforeDigest,
        `Client authoring write ${index + 1} expected digest`,
      ),
      content: write.content,
      afterDigest: write.content === null ? null : sha256(Buffer.from(write.content)),
      tag: requiredHumanText(write.tag, `Client authoring write ${index + 1} tag`, 240),
      publishBarrier: write.publishBarrier,
    };
  });
  if (writes.filter((write) => write.publishBarrier).length !== 1) {
    throw new Error("Client authoring plan must contain exactly one publish barrier");
  }
  const removals = plan.removals.map((value, index) => {
    const removal = exactObject(
      value,
      ["destination", "type", "expectedDev", "expectedIno", "tag"],
      ["destination", "type", "expectedDev", "expectedIno", "tag"],
      `Client authoring removal ${index + 1}`,
    );
    if (removal.type !== "directory") {
      throw new Error(`Client authoring removal ${index + 1} must be a directory`);
    }
    return {
      destination: requiredAbsolutePath(
        removal.destination,
        `Client authoring removal ${index + 1} destination`,
      ),
      type: "directory",
      expectedDev: requiredFileIdentityNumber(
        removal.expectedDev,
        `Client authoring removal ${index + 1} device`,
      ),
      expectedIno: requiredFileIdentityNumber(
        removal.expectedIno,
        `Client authoring removal ${index + 1} inode`,
      ),
      tag: requiredHumanText(removal.tag, `Client authoring removal ${index + 1} tag`, 240),
    };
  });
  const paths = [...writes.map((write) => write.destination), ...removals.map((removal) => removal.destination)];
  for (let left = 0; left < paths.length; left += 1) {
    for (let right = left + 1; right < paths.length; right += 1) {
      if (paths[left] === paths[right] || pathInside(paths[left], paths[right]) || pathInside(paths[right], paths[left])) {
        throw new Error(`Client authoring plan has overlapping destinations '${paths[left]}' and '${paths[right]}'`);
      }
    }
  }
  return {
    operationId: requiredOperationId(plan.operationId),
    label: requiredHumanText(plan.label, "Client authoring plan label", 240),
    writes,
    removals,
  };
}

async function normalizeRoots(options) {
  const octopusHomeRoot = await realPlainDirectory(requiredAbsolutePath(options?.octopusHomeRoot, "octopusHomeRoot"), "Octopus home");
  const registryRoot = await realPlainDirectory(requiredAbsolutePath(options?.registryRoot, "registryRoot"), "Active Registry");
  const stateRoot = path.join(octopusHomeRoot, "client-state", "workspace");
  return {
    octopusHomeRoot,
    registryRoot,
    eeInstancesRoot: path.join(octopusHomeRoot, "state", "ee_instances"),
    runsRoot: path.join(octopusHomeRoot, "state", "runs"),
    interfaceRoot: path.join(registryRoot, "interface"),
    toolRoot: path.join(registryRoot, "tool"),
    artifactRoot: path.join(registryRoot, "artifact"),
    eeTemplateRoot: path.join(registryRoot, "ee_template"),
    privateInterfacesRoot: path.join(octopusHomeRoot, "interfaces"),
    interfaceHandInRoot: path.join(octopusHomeRoot, "interface-hand-in"),
    clientAgentsRoot: path.join(octopusHomeRoot, "client", "agents"),
    stateRoot,
    receiptsRoot: path.join(stateRoot, "receipts"),
    projectReceiptsRoot: path.join(stateRoot, "topology-receipts"),
    clientAuthoringReceiptsRoot: path.join(stateRoot, "client-authoring-receipts"),
    transactionsRoot: path.join(stateRoot, "transactions"),
  };
}

async function ensureStateRoots(roots) {
  for (const directory of [roots.eeInstancesRoot, roots.interfaceRoot, roots.toolRoot]) {
    await realPlainDirectory(directory, directory);
  }
  await ensurePlainParent(roots.octopusHomeRoot, roots.runsRoot);
  await ensurePlainParent(roots.octopusHomeRoot, roots.clientAgentsRoot);
  await ensurePlainParent(roots.octopusHomeRoot, roots.stateRoot);
  await ensurePlainParent(roots.stateRoot, roots.receiptsRoot);
  await ensurePlainParent(roots.stateRoot, roots.projectReceiptsRoot);
  await ensurePlainParent(roots.stateRoot, roots.clientAuthoringReceiptsRoot);
  await ensurePlainParent(roots.stateRoot, roots.transactionsRoot);
  for (const directory of [
    roots.runsRoot,
    roots.clientAgentsRoot,
    roots.stateRoot,
    roots.receiptsRoot,
    roots.projectReceiptsRoot,
    roots.clientAuthoringReceiptsRoot,
    roots.transactionsRoot,
  ]) {
    await realPlainDirectory(directory, directory);
  }
}

function parseProjectMutationRequest(value) {
  const request = exactObject(
    value,
    [
      "action", "operationId", "workspace", "parentEeId", "expectedParentDigest", "templateName",
      "name", "goal", "presentation", "eeId", "expectedProjectDigest", "projects",
    ],
    ["action", "operationId", "workspace"],
    "Workspace project mutation",
  );
  if (!["create_project", "delete_project", "update_project_presentation"].includes(request.action)) {
    throw new Error("Workspace project mutation has an invalid action");
  }
  const operationId = requiredOperationId(request.operationId);
  const workspace = parseProjectWorkspace(request.workspace);
  let canonical;
  if (request.action === "create_project") {
    const presentation = exactObject(request.presentation, ["icon", "color"], ["icon", "color"], "Project presentation");
    canonical = {
      action: "create_project",
      operationId,
      workspace,
      parentEeId: requiredId(request.parentEeId, "Project parent EE"),
      expectedParentDigest: requiredDigest(request.expectedParentDigest, "Project parent digest"),
      templateName: requiredId(request.templateName, "Project policy template"),
      name: requiredHumanText(request.name, "Project name", 80),
      goal: requiredHumanText(request.goal, "Project goal", 240),
      presentation: {
        icon: requiredHumanText(presentation.icon, "Project icon", 64),
        color: requiredHumanText(presentation.color, "Project color", 64),
      },
    };
  } else if (request.action === "delete_project") {
    canonical = {
      action: "delete_project",
      operationId,
      workspace,
      eeId: requiredId(request.eeId, "Project EE"),
      expectedProjectDigest: requiredDigest(request.expectedProjectDigest, "Project digest"),
    };
  } else {
    if (!Array.isArray(request.projects) || request.projects.length > 24) {
      throw new Error("Workspace Project presentation must contain at most 24 Projects");
    }
    const projects = request.projects.map((value, index) => {
      const project = exactObject(
        value,
        ["eeId", "name", "icon", "color"],
        ["eeId", "name", "icon", "color"],
        `Workspace Project presentation ${index + 1}`,
      );
      return {
        eeId: requiredId(project.eeId, `Workspace Project presentation ${index + 1} EE`),
        name: requiredHumanText(project.name, `Workspace Project presentation ${index + 1} name`, 80),
        icon: requiredHumanText(project.icon, `Workspace Project presentation ${index + 1} icon`, 64),
        color: requiredHumanText(project.color, `Workspace Project presentation ${index + 1} color`, 64),
      };
    });
    if (new Set(projects.map((project) => project.eeId)).size !== projects.length) {
      throw new Error("Workspace Project presentation repeats an EE");
    }
    canonical = {
      action: "update_project_presentation",
      operationId,
      workspace,
      projects,
    };
  }
  return {
    ...canonical,
    digest: sha256(Buffer.from(stableJson(canonical))),
  };
}

function parseProjectWorkspace(value) {
  const workspace = exactObject(
    value,
    ["rootEeId", "homeRunId", "homeInterfaceId", "expectedHomeSidecarDigest"],
    ["rootEeId", "homeRunId", "homeInterfaceId", "expectedHomeSidecarDigest"],
    "Workspace project identity",
  );
  return {
    rootEeId: requiredId(workspace.rootEeId, "Workspace root EE"),
    homeRunId: requiredId(workspace.homeRunId, "Workspace Home run"),
    homeInterfaceId: requiredId(workspace.homeInterfaceId, "Workspace Home interface"),
    expectedHomeSidecarDigest: requiredDigest(workspace.expectedHomeSidecarDigest, "Workspace Home sidecar digest"),
  };
}

function parseProjectMutationScope(value) {
  const scope = exactObject(
    value,
    ["status", "rootEeId", "homeRunId", "homeInterfaceId", "homeSidecarDigest", "environments", "clientRuns", "readOnlyRuns"],
    ["status", "rootEeId", "homeRunId", "homeInterfaceId", "homeSidecarDigest", "environments", "clientRuns", "readOnlyRuns"],
    "Workspace project scope",
  );
  if (scope.status !== "ready") throw new Error(`Workspace project mutation requires ready scope, found '${String(scope.status)}'`);
  if (!Array.isArray(scope.environments) || !Array.isArray(scope.clientRuns) || !Array.isArray(scope.readOnlyRuns)) {
    throw new Error("Workspace project scope has invalid topology arrays");
  }
  const environments = scope.environments.map((value, index) => {
    const environment = exactObject(
      value,
      ["eeId", "parentEeId", "templateName", "recordDigest", "allowedChildTemplateIds"],
      ["eeId", "parentEeId", "templateName", "recordDigest", "allowedChildTemplateIds"],
      `Workspace project scope EE ${index + 1}`,
    );
    if (!Array.isArray(environment.allowedChildTemplateIds)) throw new Error(`Workspace project scope EE ${index + 1} has invalid child templates`);
    return {
      eeId: requiredId(environment.eeId, `Workspace project scope EE ${index + 1}`),
      parentEeId: optionalText(environment.parentEeId, `Workspace project scope EE ${index + 1} parent`, 160),
      templateName: requiredId(environment.templateName, `Workspace project scope EE ${index + 1} template`),
      recordDigest: requiredDigest(environment.recordDigest, `Workspace project scope EE ${index + 1} digest`),
      allowedChildTemplateIds: parseSortedUniqueIds(
        [...environment.allowedChildTemplateIds].sort(),
        `Workspace project scope EE ${index + 1} child templates`,
        true,
      ),
    };
  });
  const byId = new Map(environments.map((environment) => [environment.eeId, environment]));
  if (byId.size !== environments.length) throw new Error("Workspace project scope repeats an EE");
  const parseRun = (value, index, label) => {
    const run = exactObject(value, ["runId", "eeId"], ["runId", "eeId"], `${label} ${index + 1}`);
    return {
      runId: requiredId(run.runId, `${label} ${index + 1}`),
      eeId: requiredId(run.eeId, `${label} ${index + 1} EE`),
    };
  };
  return {
    status: "ready",
    rootEeId: requiredId(scope.rootEeId, "Workspace project scope root EE"),
    homeRunId: requiredId(scope.homeRunId, "Workspace project scope Home run"),
    homeInterfaceId: requiredId(scope.homeInterfaceId, "Workspace project scope Home interface"),
    homeSidecarDigest: requiredDigest(scope.homeSidecarDigest, "Workspace project scope Home sidecar digest"),
    environments,
    byId,
    clientRuns: scope.clientRuns.map((run, index) => parseRun(run, index, "Workspace client run")),
    readOnlyRuns: scope.readOnlyRuns.map((run, index) => parseRun(run, index, "Workspace read-only run")),
  };
}

async function requireConfiguredWorkspace(roots, scope, requested) {
  const entries = await fs.readdir(roots.receiptsRoot, { withFileTypes: true });
  if (entries.length !== 1 || !entries[0].isFile() || !entries[0].name.endsWith(".json")) {
    throw new Error(`Workspace project mutation requires exactly one setup receipt, found ${entries.length}`);
  }
  const receipt = await readReceiptFile(path.join(roots.receiptsRoot, entries[0].name));
  await verifyWorkspaceReceiptIdentity(roots, receipt);
  if (
    receipt.root_ee_id !== scope.rootEeId
    || receipt.home_run_id !== scope.homeRunId
    || receipt.home_interface_id !== scope.homeInterfaceId
    || requested.rootEeId !== scope.rootEeId
    || requested.homeRunId !== scope.homeRunId
    || requested.homeInterfaceId !== scope.homeInterfaceId
    || requested.expectedHomeSidecarDigest !== scope.homeSidecarDigest
  ) {
    throw new Error("Workspace project mutation identity does not match the configured Home receipt and scope");
  }
  return receipt;
}

async function planProjectCreate(roots, scope, request, planOperatorSurface) {
  if (typeof planOperatorSurface !== "function") {
    throw new Error("Project creation requires the operator task topology planner");
  }
  const parent = scope.byId.get(request.parentEeId);
  if (!parent) throw new Error(`Project parent EE '${request.parentEeId}' is outside the workspace scope`);
  if (parent.recordDigest !== request.expectedParentDigest) throw new Error(`Project parent EE '${request.parentEeId}' changed before creation`);
  if (!parent.allowedChildTemplateIds.includes(request.templateName)) {
    throw new Error(`Project parent EE '${request.parentEeId}' does not allow '${request.templateName}'`);
  }
  const parentPath = path.join(roots.eeInstancesRoot, `${parent.eeId}.json`);
  const parentDocument = await readJson(parentPath, `Project parent EE ${parent.eeId}`);
  if (parentDocument.digest !== request.expectedParentDigest || parentDocument.value.ee_id !== parent.eeId) {
    throw new Error(`Project parent EE '${parent.eeId}' changed before creation`);
  }
  const template = await readTemplate(roots, request.templateName);
  if (template.id !== request.templateName) throw new Error("Project template identity changed before creation");
  if (template.bootstrapRoot) throw new Error("A bootstrap-root policy cannot be used for a child Project");
  const parentTemplate = await readTemplate(roots, parentDocument.value.template_name);
  if (!parentTemplate.allowedChildren.includes(request.templateName)) {
    throw new Error(`Project parent EE '${parent.eeId}' no longer allows '${request.templateName}'`);
  }
  const eeId = deterministicId("ee_project", request.operationId, "project");
  if (scope.byId.has(eeId) || await readFileIfPresent(path.join(roots.eeInstancesRoot, `${eeId}.json`))) {
    throw new Error(`Project EE '${eeId}' already exists without this operation receipt`);
  }
  const sidecar = await readMutableWorkspaceSidecar(roots, scope, request.workspace.expectedHomeSidecarDigest);
  const agentCatalog = await requireWorkspaceAgentCatalog(roots);
  const sourceProfile = exactProjectAgentSourceProfile(
    sidecar.value.task_authoring_profiles,
    agentCatalog.document,
    parent.eeId,
  );
  if (sidecar.value.task_authoring_profiles.some((profile) => profile?.ee_id === eeId)) {
    throw new Error(`Workspace Home already contains an authoring profile for '${eeId}'`);
  }
  if (sidecar.value.presentation.projects.some((project) => project?.ee_id === eeId)) {
    throw new Error(`Workspace Home already contains a presentation for '${eeId}'`);
  }
  const nextSidecar = structuredClone(sidecar.value);
  nextSidecar.task_authoring_profiles.push({ ...structuredClone(sourceProfile), ee_id: eeId });
  nextSidecar.presentation.projects.push({
    ee_id: eeId,
    name: request.name,
    icon: request.presentation.icon,
    color: request.presentation.color,
  });
  const nextAgentCatalog = structuredClone(agentCatalog.document);
  nextAgentCatalog.project_sources.push({
    project_ee_id: eeId,
    source_worker_interface: requiredId(
      sourceProfile.worker_choice_interface,
      `Project '${eeId}' Agent source worker`,
    ),
  });
  parseClientAgentCatalog(nextAgentCatalog, agentCatalog.file);
  const projectEe = {
    eeId,
    templateName: request.templateName,
    parentEeId: parent.eeId,
    goal: request.goal,
    config: projectChildConfig(parentDocument.value.config),
  };
  const eeContent = eeDocument(projectEe);
  const projectProfile = projectAuthoringProfile(sourceProfile, eeId);
  const homeRun = await readJson(path.join(roots.runsRoot, `${scope.homeRunId}.json`), "Home provider selection");
  const homeProvider = homeRun.value.interface_state?.[scope.homeInterfaceId];
  if (homeProvider?.provider_choice_interface !== sidecar.value.provider_choice_interface) throw new Error("Home provider selection changed before Project creation");
  const providerPin = requiredId(homeProvider.provider_runtime_interface, "Home exact provider interface");
  const provider = await readJson(path.join(roots.interfaceRoot, providerPin, "interface.json"), "Home exact provider descriptor");
  if (provider.value.id !== providerPin) throw new Error("Home exact provider identity changed");
  const providerModel = requiredHumanText(provider.value.interface_driver_config?.private_config?.model, "Home selected model", 256);
  const overlay = await createEeOverlay(roots, [projectEe]);
  try {
    await preflightProjectTaskSurface({
      authoringProfile: projectProfile,
      eeId,
      eeInstancesRoot: overlay,
      operationId: request.operationId,
      planOperatorSurface,
      projectKey: "project",
      providerInterfaceId: requiredId(sidecar.value.provider_choice_interface, "Workspace provider choice interface"),
      providerModel,
    });
  } finally {
    await fs.rm(overlay, { recursive: true, force: true });
  }
  if ((await readJson(path.join(roots.runsRoot, `${scope.homeRunId}.json`), "Home provider selection")).digest !== homeRun.digest
      || (await readJson(path.join(roots.interfaceRoot, providerPin, "interface.json"), "Home exact provider descriptor")).digest !== provider.digest) throw new Error("Home model changed during Project creation");
  if ((await readJson(parentPath, "Project parent EE")).digest !== parentDocument.digest
    || (await readTemplate(roots, request.templateName)).digest !== template.digest
    || (await readTemplate(roots, parentTemplate.id)).digest !== parentTemplate.digest) {
    throw new Error("Project policy changed during creation; review the policy and try again");
  }
  const sidecarContent = `${JSON.stringify(nextSidecar, null, 2)}\n`;
  return {
    outcome: { action: "create_project", eeId, parentEeId: parent.eeId, templateName: request.templateName, goal: request.goal },
    writes: [
      projectWrite(path.join(roots.eeInstancesRoot, `${eeId}.json`), null, eeContent, `ee:${eeId}`, false),
      projectWrite(
        agentCatalog.file,
        agentCatalog.digest,
        `${JSON.stringify(nextAgentCatalog, null, 2)}\n`,
        "client-agent-catalog",
        false,
      ),
      projectWrite(sidecar.path, sidecar.digest, sidecarContent, `octopus-os:${scope.homeInterfaceId}`, true),
    ],
  };
}

async function planProjectDelete(roots, scope, request) {
  if (request.eeId === scope.rootEeId) throw new Error("The workspace root EE cannot be deleted as a Project");
  const project = scope.byId.get(request.eeId);
  if (!project) throw new Error(`Project EE '${request.eeId}' is outside the workspace scope`);
  if (project.recordDigest !== request.expectedProjectDigest) throw new Error(`Project EE '${request.eeId}' changed before deletion`);
  if (scope.environments.some((candidate) => candidate.parentEeId === request.eeId)) {
    throw new Error("A Project with child Projects cannot be deleted");
  }
  if ([...scope.clientRuns, ...scope.readOnlyRuns].some((run) => run.eeId === request.eeId)) {
    throw new Error("A Project with tasks cannot be deleted");
  }
  await assertNoRunReferencesEe(roots, request.eeId);
  const eePath = path.join(roots.eeInstancesRoot, `${request.eeId}.json`);
  const ee = await readJson(eePath, `Project EE ${request.eeId}`);
  if (
    ee.digest !== request.expectedProjectDigest
    || ee.value.ee_id !== request.eeId
    || ee.value.template_name !== project.templateName
  ) {
    throw new Error(`Project EE '${request.eeId}' changed before deletion`);
  }
  const sidecar = await readMutableWorkspaceSidecar(roots, scope, request.workspace.expectedHomeSidecarDigest);
  const agentCatalog = await requireWorkspaceAgentCatalog(roots);
  if (agentCatalog.document.agents.some((agent) => (
    agent.project_ee_id === request.eeId && agent.deleted_at_ms === 0
  ))) {
    throw new Error("A Project with Agents cannot be deleted");
  }
  const nextSidecar = structuredClone(sidecar.value);
  nextSidecar.task_authoring_profiles = nextSidecar.task_authoring_profiles.filter((profile) => profile.ee_id !== request.eeId);
  nextSidecar.presentation.projects = nextSidecar.presentation.projects.filter((projectRow) => projectRow.ee_id !== request.eeId);
  const nextAgentCatalog = structuredClone(agentCatalog.document);
  nextAgentCatalog.project_sources = nextAgentCatalog.project_sources.filter((source) => source.project_ee_id !== request.eeId);
  nextAgentCatalog.agents = nextAgentCatalog.agents.filter((agent) => agent.project_ee_id !== request.eeId);
  parseClientAgentCatalog(nextAgentCatalog, agentCatalog.file);
  const sidecarContent = `${JSON.stringify(nextSidecar, null, 2)}\n`;
  return {
    outcome: { action: "delete_project", eeId: request.eeId },
    writes: [
      projectWrite(
        agentCatalog.file,
        agentCatalog.digest,
        `${JSON.stringify(nextAgentCatalog, null, 2)}\n`,
        "client-agent-catalog",
        false,
      ),
      projectWrite(sidecar.path, sidecar.digest, sidecarContent, `octopus-os:${scope.homeInterfaceId}`, false),
      projectWrite(eePath, ee.digest, null, `ee:${request.eeId}`, true),
    ],
  };
}

async function planProjectPresentationUpdate(roots, scope, request) {
  const projectIds = scope.environments
    .filter((environment) => environment.eeId !== scope.rootEeId)
    .map((environment) => environment.eeId)
    .sort();
  const requestedIds = request.projects.map((project) => project.eeId).sort();
  if (stableJson(requestedIds) !== stableJson(projectIds)) {
    throw new Error("Workspace Project presentation must cover every current Project exactly once");
  }
  const sidecar = await readMutableWorkspaceSidecar(roots, scope, request.workspace.expectedHomeSidecarDigest);
  const profileIds = [...new Set(sidecar.value.task_authoring_profiles.map((profile) => requiredId(profile?.ee_id, "Workspace Project authoring profile EE")))].filter((eeId) => eeId !== scope.rootEeId).sort();
  const existingPresentationIds = sidecar.value.presentation.projects.map((project) => requiredId(project?.ee_id, "Workspace Project presentation EE")).sort();
  if (stableJson(profileIds) !== stableJson(projectIds) || stableJson(existingPresentationIds) !== stableJson(projectIds)) {
    throw new Error("Workspace Project presentation cannot update an incomplete Home topology");
  }
  const nextSidecar = structuredClone(sidecar.value);
  nextSidecar.presentation.projects = request.projects.map((project) => ({
    ee_id: project.eeId,
    name: project.name,
    icon: project.icon,
    color: project.color,
  }));
  return {
    outcome: { action: "update_project_presentation", projectEeIds: request.projects.map((project) => project.eeId) },
    writes: [
      projectWrite(
        sidecar.path,
        sidecar.digest,
        `${JSON.stringify(nextSidecar, null, 2)}\n`,
        `octopus-os:${scope.homeInterfaceId}`,
        true,
      ),
    ],
  };
}

async function readMutableWorkspaceSidecar(roots, scope, expectedDigest) {
  const file = path.join(roots.interfaceRoot, scope.homeInterfaceId, "octopus_os.json");
  const document = await readJson(file, "Workspace Home sidecar");
  if (document.digest !== expectedDigest || document.digest !== scope.homeSidecarDigest) {
    throw new Error("Workspace Home sidecar changed before Project mutation");
  }
  const sidecar = exactObject(
    document.value,
    [
      "schema_version", "workspace", "interface", "owner_interface", "component", "surface_role",
      "provider_choice_interface", "worker_choice_interface", "authoring_profile", "task_authoring_profiles", "presentation",
      "voice_context",
    ],
    [
      "schema_version", "workspace", "interface", "owner_interface", "component", "surface_role",
      "provider_choice_interface", "worker_choice_interface", "authoring_profile", "task_authoring_profiles", "presentation",
    ],
    "Workspace Home sidecar",
  );
  if (
    sidecar.schema_version !== 1
    || sidecar.workspace !== "octopus_os"
    || sidecar.interface !== scope.homeInterfaceId
    || sidecar.owner_interface !== scope.homeInterfaceId
    || sidecar.component !== "client"
    || sidecar.surface_role !== "home"
    || !Array.isArray(sidecar.task_authoring_profiles)
  ) {
    throw new Error("Workspace Home sidecar has invalid identity or authoring profiles");
  }
  if (Object.hasOwn(sidecar, "voice_context")) parseHomeVoiceContext(sidecar.voice_context);
  const presentation = exactObject(sidecar.presentation, ["name", "projects"], ["projects"], "Workspace Home presentation");
  if (presentation.name !== undefined) {
    requiredHumanText(presentation.name, "Workspace Home presentation name", 80);
  }
  if (!Array.isArray(presentation.projects)) throw new Error("Workspace Home presentation has invalid Projects");
  sidecar.presentation = presentation;
  return { path: file, digest: document.digest, value: sidecar };
}

function exactlyOne(values, predicate, label) {
  const matches = values.filter(predicate);
  if (matches.length !== 1) throw new Error(`Workspace Home must contain exactly one ${label}, found ${matches.length}`);
  return matches[0];
}

async function requireWorkspaceAgentCatalog(roots) {
  const catalog = await readClientAgentCatalog(roots.octopusHomeRoot);
  if (!catalog.exists || !catalog.digest) {
    throw new Error(`Workspace has no authored Client Agent catalog: ${catalog.file}`);
  }
  return catalog;
}

function exactProjectAgentSourceProfile(profiles, catalog, eeId) {
  const sources = catalog.project_sources.filter((candidate) => candidate.project_ee_id === eeId);
  if (sources.length !== 1) {
    throw new Error(`Client Agent catalog exposes ${sources.length} source workers for Project EE '${eeId}'`);
  }
  const matches = profiles.filter((candidate) => (
    candidate?.ee_id === eeId
    && candidate?.worker_choice_interface === sources[0].source_worker_interface
  ));
  if (matches.length !== 1) {
    throw new Error(`Workspace Home exposes ${matches.length} exact Agent source profiles for Project EE '${eeId}'`);
  }
  return matches[0];
}

async function assertNoRunReferencesEe(roots, eeId) {
  for (const entry of await fs.readdir(roots.runsRoot, { withFileTypes: true })) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const record = (await readJson(path.join(roots.runsRoot, entry.name), `Run record ${entry.name}`)).value;
    if (record.ee === eeId) throw new Error(`Project EE '${eeId}' is still referenced by run '${String(record.id)}'`);
  }
}

function projectWrite(destination, beforeDigest, content, tag, publishBarrier) {
  return {
    destination,
    expectedBeforeDigest: beforeDigest,
    afterDigest: content === null ? null : sha256(Buffer.from(content)),
    content,
    tag,
    publishBarrier,
  };
}

function parseInitializeRequest(value) {
  const request = exactObject(value, ["action", "operationId", "root", "home", "task", "projects", "agentPresets"], ["operationId", "root", "home", "task", "projects"], "Workspace setup request");
  if (request.action !== undefined && request.action !== "initialize_workspace") {
    throw new Error("Workspace setup request has an invalid action");
  }
  const operationId = requiredOperationId(request.operationId);
  const root = parseRootIntent(request.root);
  const home = parseHomeIntent(request.home);
  const task = parseTaskIntent(request.task);
  if (!Array.isArray(request.projects) || request.projects.length > 24) {
    throw new Error("Workspace setup projects must be an array with at most 24 entries");
  }
  const projects = request.projects.map(parseProjectIntent);
  validateProjectIntents(projects);
  const agentPresets = parseAgentPresetChoices(request.agentPresets, new Set(projects.map((project) => project.key)));
  const canonicalRequest = { operationId, root, home, task, projects, ...(agentPresets.length ? { agentPresets } : {}) };
  return {
    ...canonicalRequest,
    digest: sha256(Buffer.from(stableJson(canonicalRequest))),
  };
}

function parseAgentPresetChoices(value, projectKeys) {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > 32) throw new Error("Workspace Agent presets must be a bounded array");
  const seen = new Set();
  return value.map((entry) => {
    exactObject(entry, ["packId", "presetId", "expectedPackManifestDigest", "projectKeys", "providerInterfaceId", "providerModel", "reasoningEffort", "serviceTier"], ["packId", "presetId", "expectedPackManifestDigest", "projectKeys", "providerInterfaceId", "providerModel", "reasoningEffort"], "Workspace Agent preset");
    const choice = {
      packId: requiredId(entry.packId, "Agent preset pack"), presetId: requiredId(entry.presetId, "Agent preset"),
      expectedPackManifestDigest: requiredDigest(entry.expectedPackManifestDigest, "Agent preset pack digest"),
      projectKeys: parseUniqueIds(entry.projectKeys, "Agent preset Project keys", true).sort(),
      providerInterfaceId: requiredId(entry.providerInterfaceId, "Agent preset provider"),
      providerModel: requiredHumanText(entry.providerModel, "Agent preset model", 256),
      reasoningEffort: optionalText(entry.reasoningEffort, "Agent preset reasoning effort", 80),
      serviceTier: optionalText(entry.serviceTier, "Agent preset service tier", 80),
    };
    const key = `${choice.packId}\0${choice.presetId}`;
    if (seen.has(key) || choice.projectKeys.some((id) => !projectKeys.has(id))) throw new Error("Workspace Agent preset repeats or targets an unknown Project");
    seen.add(key);
    return choice;
  }).sort((a, b) => `${a.packId}\0${a.presetId}`.localeCompare(`${b.packId}\0${b.presetId}`));
}

function parseRootIntent(value) {
  if (value && Object.hasOwn(value, "existingEeId")) {
    const root = exactObject(value, ["key", "existingEeId", "expectedEeDigest", "name"], ["key", "existingEeId", "expectedEeDigest", "name"], "Existing workspace root");
    if (root.key !== "root") throw new Error("Existing workspace root must use the root key");
    return {
      key: "root",
      existingEeId: requiredId(root.existingEeId, "Existing workspace EE"),
      expectedEeDigest: requiredDigest(root.expectedEeDigest, "Existing workspace EE digest"),
      name: requiredHumanText(root.name, "Workspace name", 80),
    };
  }
  const root = exactObject(value, ["key", "templateName", "name", "goal"], ["key", "templateName", "name", "goal"], "Workspace root");
  if (root.key !== "root" || root.templateName !== "existence") {
    throw new Error("Workspace root must author the actual bootstrap-root existence EE");
  }
  return {
    key: "root",
    templateName: "existence",
    name: requiredHumanText(root.name, "Workspace name", 80),
    goal: requiredHumanText(root.goal, "Workspace goal", 240),
  };
}

function parseHomeIntent(value) {
  const home = exactObject(
    value,
    [
      "name", "providerInterfaceId", "providerModel", "workerInterfaceId", "reasoningEffort", "serviceTier", "doorHostSlots",
      "contextArtifactBindings", "selectedWorkerEnvironmentKinds",
    ],
    ["name", "providerInterfaceId", "providerModel", "workerInterfaceId"],
    "Workspace Home",
  );
  if (home.name !== "Home") throw new Error("Workspace setup must create the canonical Home surface");
  return {
    name: "Home",
    providerInterfaceId: requiredId(home.providerInterfaceId, "Home provider interface"),
    providerModel: requiredHumanText(home.providerModel, "Home model", 256),
    workerInterfaceId: requiredId(home.workerInterfaceId, "Home worker interface"),
    reasoningEffort: optionalText(home.reasoningEffort, "Home reasoning effort", 80),
    serviceTier: optionalText(home.serviceTier, "Home service tier", 80),
    doorHostSlots: parseDoorHostSlots(home.doorHostSlots),
    contextArtifactBindings: parseArtifactBindings(home.contextArtifactBindings),
    selectedWorkerEnvironmentKinds: parseUniqueIds(
      home.selectedWorkerEnvironmentKinds,
      "Home selected worker environments",
      true,
    ),
  };
}

function parseTaskIntent(value) {
  const task = exactObject(value, ["workerInterfaceId", "contextArtifactBindings", "selectedWorkerEnvironmentKinds"],
    ["workerInterfaceId", "contextArtifactBindings", "selectedWorkerEnvironmentKinds"], "Workspace Task source");
  return {
    workerInterfaceId: requiredId(task.workerInterfaceId, "Task source worker interface"),
    contextArtifactBindings: parseArtifactBindings(task.contextArtifactBindings),
    selectedWorkerEnvironmentKinds: parseUniqueIds(task.selectedWorkerEnvironmentKinds, "Task source worker environments"),
  };
}

function parseProjectIntent(value, index) {
  const project = exactObject(
    value,
    ["key", "parentKey", "templateName", "name", "goal", "presentation"],
    ["key", "parentKey", "templateName", "name", "goal", "presentation"],
    `Workspace project ${index + 1}`,
  );
  if (project.templateName !== "project") {
    throw new Error(`Workspace project ${index + 1} must expand to the generic project template`);
  }
  const presentation = exactObject(
    project.presentation,
    ["icon", "color"],
    ["icon", "color"],
    `Workspace project ${index + 1} presentation`,
  );
  return {
    key: requiredId(project.key, `Workspace project ${index + 1} key`),
    parentKey: project.parentKey === "root" ? "root" : requiredId(project.parentKey, `Workspace project ${index + 1} parent`),
    templateName: "project",
    name: requiredHumanText(project.name, `Workspace project ${index + 1} name`, 80),
    goal: requiredHumanText(project.goal, `Workspace project ${index + 1} goal`, 240),
    presentation: {
      icon: requiredHumanText(presentation.icon, `Workspace project ${index + 1} icon`, 64),
      color: requiredHumanText(presentation.color, `Workspace project ${index + 1} color`, 64),
    },
  };
}

function validateProjectIntents(projects) {
  const byKey = new Map();
  for (const project of projects) {
    if (byKey.has(project.key)) throw new Error(`Workspace setup repeats project '${project.key}'`);
    byKey.set(project.key, project);
  }
  for (const project of projects) {
    if (project.parentKey !== "root" && !byKey.has(project.parentKey)) {
      throw new Error(`Workspace project '${project.name}' references a missing parent`);
    }
    const seen = new Set([project.key]);
    let cursor = project.parentKey;
    while (cursor !== "root") {
      if (seen.has(cursor)) throw new Error(`Workspace project '${project.name}' creates a cycle`);
      seen.add(cursor);
      cursor = byKey.get(cursor)?.parentKey;
      if (!cursor) throw new Error(`Workspace project '${project.name}' has an incomplete ancestry`);
    }
  }
}

async function readBootstrapRoot(roots) {
  const entries = await fs.readdir(roots.eeInstancesRoot, { withFileTypes: true });
  const candidates = [];
  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const file = path.join(roots.eeInstancesRoot, entry.name);
    const document = await readJson(file, `EE instance ${entry.name}`);
    if (document.value.template_name !== "existence") continue;
    if (document.value.parent_ee_id != null) continue;
    const eeId = requiredId(document.value.ee_id, "Bootstrap EE id");
    if (`${eeId}.json` !== entry.name) throw new Error(`Bootstrap EE '${eeId}' has a mismatched file name`);
    candidates.push({ eeId, document });
  }
  if (candidates.length !== 1) {
    throw new Error(`Workspace setup requires exactly one existence root, found ${candidates.length}`);
  }
  return candidates[0];
}

async function readExistingWorkspaceRoot(roots, root) {
  const file = path.join(roots.eeInstancesRoot, `${root.existingEeId}.json`);
  await validateExistingPath(roots.eeInstancesRoot, file);
  const document = await readJson(file, "Existing workspace EE");
  if (document.digest !== root.expectedEeDigest) throw new Error("Existing workspace EE changed before planning");
  const record = document.value;
  if (record.ee_id !== root.existingEeId) throw new Error("Existing workspace EE has a foreign identity");
  return parseExistingRootObservation({
    path: file,
    digest: document.digest,
    ee_id: record.ee_id,
    template_name: record.template_name,
    parent_ee_id: record.parent_ee_id,
  });
}

function parseExistingRootObservation(value) {
  const root = exactObject(value, ["path", "digest", "ee_id", "template_name", "parent_ee_id"], ["path", "digest", "ee_id", "template_name", "parent_ee_id"], "Existing workspace root observation");
  return {
    path: requiredAbsolutePath(root.path, "Existing workspace root path"),
    digest: requiredDigest(root.digest, "Existing workspace root digest"),
    ee_id: requiredId(root.ee_id, "Existing workspace root EE"),
    template_name: requiredId(root.template_name, "Existing workspace root template"),
    parent_ee_id: root.parent_ee_id === null ? null : requiredId(root.parent_ee_id, "Existing workspace root parent"),
  };
}

async function verifyExistingRootObservation(roots, observation, exactBytes) {
  const root = parseExistingRootObservation(observation);
  if (root.path !== path.join(roots.eeInstancesRoot, `${root.ee_id}.json`)) {
    throw new Error("Existing workspace root observation belongs to another Home");
  }
  await validateExistingPath(roots.eeInstancesRoot, root.path);
  const document = await readJson(root.path, "Existing workspace root EE");
  if (exactBytes && document.digest !== root.digest) throw new Error("Existing workspace root EE changed during initialization");
  if (document.value.ee_id !== root.ee_id || document.value.template_name !== root.template_name
    || document.value.parent_ee_id !== root.parent_ee_id) {
    throw new Error("Existing workspace root EE identity is no longer valid");
  }
}

async function verifySetupRootObservation(roots, receipt) {
  if (receipt.existing_root) await verifyExistingRootObservation(roots, receipt.existing_root, true);
}

function materializeWorkspaceIntent(request, bootstrap, existingRoot) {
  const rootEeId = existingRoot?.ee_id ?? bootstrap.eeId;
  const projectEeIds = new Map(request.projects.map((project) => [
    project.key,
    deterministicId("ee_project", request.operationId, project.key),
  ]));
  const eeWrites = [...(existingRoot ? [] : [{
    eeId: rootEeId,
    templateName: request.root.templateName,
    parentEeId: null,
    goal: request.root.goal,
    document: { ...bootstrap.document.value, goal: request.root.goal },
    expectedBeforeDigest: bootstrap.document.digest,
  }]), ...request.projects.map((project) => ({
    eeId: requiredMapValue(projectEeIds, project.key),
    templateName: project.templateName,
    parentEeId: project.parentKey === "root" ? rootEeId : requiredMapValue(projectEeIds, project.parentKey),
    goal: project.goal,
  }))];
  const suffix = sha256(Buffer.from(`${request.operationId}\u0000home`)).slice(0, 24);
  return {
    rootEeId,
    existingRoot,
    projectEeIds,
    eeWrites,
    chatId: `home_${suffix}`,
    cycle: `setup_${suffix}`,
  };
}

async function validateWorkspaceTemplates(roots, materialized) {
  const rootWrite = materialized.eeWrites.find((write) => write.eeId === materialized.rootEeId);
  const rootTemplate = materialized.existingRoot?.template_name ?? rootWrite.templateName;
  const rootParent = materialized.existingRoot ? materialized.existingRoot.parent_ee_id : rootWrite.parentEeId;
  if (rootParent !== null) throw new Error("Home must live directly in the root Existence EE; select its actual root EE");
  const templateByEe = new Map([
    [materialized.rootEeId, rootTemplate],
    ...materialized.eeWrites.map((write) => [write.eeId, write.templateName]),
  ]);
  const templates = new Map();
  for (const id of new Set(templateByEe.values())) templates.set(id, await readTemplate(roots, id));
  if (templates.get(rootTemplate).bootstrapRoot !== true) throw new Error("Home requires the declared bootstrap-root EE");
  for (const write of materialized.eeWrites) {
    if (write.eeId === materialized.rootEeId) continue;
    if (!templates.get(templateByEe.get(write.parentEeId))?.allowedChildren.includes(write.templateName)) {
      throw new Error(`Workspace parent EE '${write.parentEeId}' does not allow template '${write.templateName}'`);
    }
  }
}

async function readTemplate(roots, id) {
  const file = path.join(roots.registryRoot, "ee_template", id, "ee_template.json");
  const document = await readJson(file, `EE template ${id}`);
  if (document.value.id !== id) throw new Error(`EE template '${id}' has drifted identity`);
  if (!Array.isArray(document.value.allowed_child_ee_templates)) {
    throw new Error(`EE template '${id}' has no explicit child-template list`);
  }
  return {
    id,
    digest: document.digest,
    bootstrapRoot: document.value.bootstrap_root,
    allowedChildren: document.value.allowed_child_ee_templates.map((child, index) => requiredId(child, `EE template ${id} child ${index}`)),
  };
}

async function createEeOverlay(roots, eeWrites) {
  const overlay = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-workspace-ee-")));
  try {
    const entries = await fs.readdir(roots.eeInstancesRoot, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isFile() || !entry.name.endsWith(".json")) {
        throw new Error(`EE instance root contains an invalid entry '${entry.name}'`);
      }
      const source = path.join(roots.eeInstancesRoot, entry.name);
      const target = path.join(overlay, entry.name);
      const bytes = await readFileNoFollow(source);
      await writeSyncedFile(target, bytes, 0o600, "wx");
    }
    for (const write of eeWrites) {
      if (write.expectedBeforeDigest) {
        const target = path.join(overlay, `${write.eeId}.json`);
        const before = await readFileIfPresent(target);
        if (!before || sha256(before) !== write.expectedBeforeDigest) throw new Error(`Workspace root EE '${write.eeId}' changed before planning`);
        await fs.unlink(target);
      }
      await writeSyncedFile(
        path.join(overlay, `${write.eeId}.json`),
        Buffer.from(eeDocument(write)),
        0o600,
        "wx",
      );
    }
    await syncDirectory(overlay);
    return overlay;
  } catch (error) {
    await fs.rm(overlay, { recursive: true, force: true });
    throw error;
  }
}

async function preflightInitialWorkspaceProjects({
  eeInstancesRoot,
  materialized,
  operationId,
  planOperatorSurface,
  projects,
  providerInterfaceId,
  providerModel,
  homeAuthoringProfile,
  task,
}) {
  const rootProfile = {
    ee_id: materialized.rootEeId,
    worker_choice_interface: task.workerInterfaceId,
    door_host_slots: parseDoorHostSlots(homeAuthoringProfile.door_host_slots),
    ...(task.contextArtifactBindings.length ? { context_artifact_bindings: structuredClone(task.contextArtifactBindings) } : {}),
    ...(task.selectedWorkerEnvironmentKinds.length ? { selected_worker_environment_kinds: [...task.selectedWorkerEnvironmentKinds] } : {}),
  };
  await preflightProjectTaskSurface({
    authoringProfile: rootProfile, eeId: materialized.rootEeId, eeInstancesRoot, operationId,
    planOperatorSurface, projectKey: "root", providerInterfaceId, providerModel,
  });
  const profiles = new Map([["root", rootProfile]]);
  const remaining = new Map(projects.map((project) => [project.key, project]));
  while (remaining.size) {
    const ready = projects.filter((project) => remaining.has(project.key) && profiles.has(project.parentKey));
    if (!ready.length) throw new Error("Workspace Project profiles cannot be derived from their parent chain");
    for (const project of ready) {
      const eeId = requiredMapValue(materialized.projectEeIds, project.key);
      const profile = projectAuthoringProfile(profiles.get(project.parentKey), eeId);
      await preflightProjectTaskSurface({
        authoringProfile: profile,
        eeId,
        eeInstancesRoot,
        operationId,
        planOperatorSurface,
        projectKey: project.key,
        providerInterfaceId,
        providerModel,
      });
      profiles.set(project.key, profile);
      remaining.delete(project.key);
    }
  }
  return profiles;
}

function projectAuthoringProfile(sourceProfile, eeId) {
  if (!sourceProfile || typeof sourceProfile !== "object" || Array.isArray(sourceProfile)) {
    throw new Error(`Project EE '${eeId}' has no parent authoring profile`);
  }
  return { ...structuredClone(sourceProfile), ee_id: requiredId(eeId, "Project authoring profile EE") };
}

async function preflightProjectTaskSurface({
  authoringProfile,
  eeId,
  eeInstancesRoot,
  operationId,
  planOperatorSurface,
  projectKey,
  providerInterfaceId,
  providerModel,
}) {
  if (typeof planOperatorSurface !== "function") {
    throw new Error("Project creation requires the operator task topology planner");
  }
  const token = sha256(Buffer.from(`${operationId}\0${projectKey}\0${eeId}`)).slice(0, 32);
  const preflight = await planOperatorSurface({
    eeInstancesRoot,
    request: {
      chatId: `project_preflight_${token}`,
      cycle: `project_preflight_${token}`,
      providerInterfaceId: requiredId(providerInterfaceId, "Workspace provider choice interface"),
      providerModel: requiredHumanText(providerModel, "Project preflight model", 256),
      workerInterfaceId: requiredId(authoringProfile.worker_choice_interface, "Project authoring worker"),
      doorHostSlots: parseDoorHostSlots(authoringProfile.door_host_slots),
      externalInputs: projectProfileExternalInputs(authoringProfile.external_inputs),
      externalRelays: projectProfileExternalRelays(authoringProfile.external_relays),
      modelOutputs: projectProfileModelOutputs(authoringProfile.model_outputs),
      contextArtifactBindings: parseArtifactBindings(authoringProfile.context_artifact_bindings),
      payloadArtifactBindings: payloadArtifactBindings(authoringProfile.payload_artifact_bindings),
      selectedWorkerEnvironmentKinds: parseUniqueIds(
        authoringProfile.selected_worker_environment_kinds,
        "Project authoring worker environments",
        true,
      ),
      selectedTools: parseSelectedTools(authoringProfile.selected_tools),
      eeId,
      clientSurfaceRole: "task",
    },
  });
  verifyProjectTaskPreflight(preflight, authoringProfile, eeId);
}

async function workspaceWrites(roots, request, materialized, validatedPlan, projectProfiles, agentPlans = []) {
  const planWrites = validatedPlan.writes.map((write, index) => (
    parsePlannedWrite(roots, write, index, validatedPlan, request, materialized)
  ));
  const runPath = path.join(roots.runsRoot, `${validatedPlan.runId}.json`);
  const runIndexes = planWrites.flatMap((write, index) => write.destination === runPath ? [index] : []);
  if (runIndexes.length !== 1) throw new Error("Home topology plan must publish exactly one run record");
  const runs = validatePlannedHomeRuns(roots, validatedPlan, planWrites, JSON.parse(planWrites[runIndexes[0]].content));
  await validatePlannedHomeOwnership(roots, validatedPlan, planWrites, runs);
  const sidecarPath = path.join(roots.interfaceRoot, validatedPlan.chatInterfaceId, "octopus_os.json");
  const sidecarIndex = planWrites.findIndex((write) => write.destination === sidecarPath);
  if (sidecarIndex < 0) throw new Error("Home topology plan omitted its Octopus OS sidecar");
  planWrites[sidecarIndex] = {
    ...planWrites[sidecarIndex],
    content: workspaceHomeSidecar(
      planWrites[sidecarIndex].content,
      request,
      materialized,
      validatedPlan,
      projectProfiles,
      agentPlans,
      runs.find((run) => run.id === validatedPlan.runId).interface_state?.[validatedPlan.chatInterfaceId]?.voice_context,
    ),
  };
  planWrites[sidecarIndex].afterDigest = sha256(Buffer.from(planWrites[sidecarIndex].content));
  for (const interfaceId of [
    validatedPlan.chatInterfaceId,
    validatedPlan.modelInterfaceId,
    validatedPlan.workerInterfaceId,
  ]) {
    const descriptor = path.join(roots.interfaceRoot, interfaceId, "interface.json");
    if (!planWrites.some((write) => write.destination === descriptor)) {
      throw new Error(`Home topology plan omitted exact interface descriptor '${interfaceId}'`);
    }
  }
  const workerPolicy = path.join(roots.interfaceRoot, validatedPlan.workerInterfaceId, "policy.json");
  if (!planWrites.some((write) => write.destination === workerPolicy)) {
    throw new Error("Home topology plan omitted its exact worker policy");
  }
  const runWrite = planWrites.splice(runIndexes[0], 1)[0];
  const eeWrites = materialized.eeWrites.map((write) => {
    const content = eeDocument(write);
    return {
      destination: path.join(roots.eeInstancesRoot, `${write.eeId}.json`),
      expectedBeforeDigest: write.expectedBeforeDigest ?? null,
      afterDigest: sha256(Buffer.from(content)),
      content,
      tag: `ee:${write.eeId}`,
      publishBarrier: false,
    };
  });
  const agentCatalog = workspaceAgentCatalog(request, materialized, projectProfiles);
  agentCatalog.agents.push(...agentPlans.map((agent) => agent.presentation));
  parseClientAgentCatalog(agentCatalog);
  const agentCatalogContent = `${JSON.stringify(agentCatalog, null, 2)}\n`;
  const agentCatalogWrite = {
    destination: clientAgentCatalogPath(roots.octopusHomeRoot),
    expectedBeforeDigest: null,
    afterDigest: sha256(Buffer.from(agentCatalogContent)),
    content: agentCatalogContent,
    tag: "client-agent-catalog",
    publishBarrier: false,
  };
  const writes = [
    ...eeWrites,
    ...agentPlans.flatMap((agent) => [
      ["interface.json", agent.interface], ["policy.json", agent.policy], ["client_template.json", agent.clientTemplate],
    ].map(([name, document]) => {
      const content = `${JSON.stringify(document, null, 2)}\n`;
      return { destination: path.join(roots.interfaceRoot, agent.workerInterfaceId, name), expectedBeforeDigest: null, afterDigest: sha256(Buffer.from(content)), content, tag: `agent:${agent.workerInterfaceId}`, publishBarrier: false };
    })),
    ...planWrites.map((write) => ({ ...write, publishBarrier: false })),
    agentCatalogWrite,
    { ...runWrite, publishBarrier: true },
  ];
  await admitWorkspaceAgentWorkers(roots, writes, agentPlans);
  const destinations = new Set();
  for (const write of writes) {
    if (destinations.has(write.destination)) throw new Error(`Workspace setup repeats destination '${write.destination}'`);
    destinations.add(write.destination);
  }
  return writes;
}

async function admitWorkspaceAgentWorkers(roots, writes, agentPlans) {
  const documents = new Map();
  const read = async (file) => {
    if (documents.has(file)) return documents.get(file);
    const planned = writes.find((write) => write.destination === file);
    const current = planned ? { value: JSON.parse(planned.content), digest: planned.expectedBeforeDigest } : await readJson(file, "Agent admission document");
    const document = { file, planned, before: stableJson(current.value), value: current.value, digest: current.digest };
    documents.set(file, document);
    return document;
  };
  for (const agent of agentPlans) {
    let eeId = agent.presentation.project_ee_id;
    const visited = new Set();
    while (eeId) {
      if (visited.has(eeId)) throw new Error("Agent admission EE ancestry contains a cycle");
      visited.add(eeId);
      const instance = await read(path.join(roots.eeInstancesRoot, `${requiredId(eeId, "Agent admission EE")}.json`));
      if (instance.value.ee_id !== eeId) throw new Error("Agent admission EE identity changed");
      const templateId = requiredId(instance.value.template_name, "Agent admission template");
      const template = await read(path.join(roots.registryRoot, "ee_template", templateId, "ee_template.json"));
      if (template.value.id !== templateId) throw new Error("Agent admission template identity changed");
      appendExactAgentWorkerAdmission(template.value, "allowed_interfaces", agent.sourceWorkerInterfaceId, agent.workerInterfaceId, `EE template '${templateId}'`);
      if (instance.value.policy_narrowing !== undefined) {
        if (!instance.value.policy_narrowing || typeof instance.value.policy_narrowing !== "object" || Array.isArray(instance.value.policy_narrowing)) throw new Error("Agent admission EE narrowing must be an object");
        appendExactAgentWorkerAdmission(instance.value.policy_narrowing, "allowed_interfaces", agent.sourceWorkerInterfaceId, agent.workerInterfaceId, `EE '${eeId}' narrowing`);
      }
      eeId = instance.value.parent_ee_id;
      if (eeId !== null) requiredId(eeId, "Agent admission parent EE");
    }
  }
  for (const document of documents.values()) {
    if (document.before === stableJson(document.value)) continue;
    const content = `${JSON.stringify(document.value, null, 2)}\n`;
    const write = { destination: document.file, expectedBeforeDigest: document.digest, content, afterDigest: sha256(Buffer.from(content)), tag: "agent-worker-admission", publishBarrier: false };
    if (document.planned) Object.assign(document.planned, write);
    else writes.unshift(write);
  }
}

function workspaceAgentCatalog(request, materialized, projectProfiles) {
  const catalog = emptyClientAgentCatalog();
  catalog.project_sources = [{ key: "root" }, ...request.projects].map((project) => {
    const profile = projectProfiles.get(project.key);
    if (!profile) throw new Error(`Workspace Project '${project.key}' has no Agent source worker`);
    return {
      project_ee_id: project.key === "root" ? materialized.rootEeId : requiredMapValue(materialized.projectEeIds, project.key),
      source_worker_interface: requiredId(
        profile.worker_choice_interface,
        `Workspace Project '${project.key}' Agent source worker`,
      ),
    };
  });
  return parseClientAgentCatalog(catalog);
}

function parseHomePlan(value, request, materialized) {
  const plan = exactObject(
    value,
    [
      "runId", "chatInterfaceId", "modelInterfaceId", "workerInterfaceId", "clientSurfaceRole",
      "reasoningEffort", "serviceTier", "providerModel", "authoringProfile", "requiredInterfaceIds", "requiredToolIds", "writes", "ledgerOffset",
    ],
    [
      "runId", "chatInterfaceId", "modelInterfaceId", "workerInterfaceId", "clientSurfaceRole",
      "authoringProfile", "requiredInterfaceIds", "requiredToolIds", "writes",
    ],
    "Home topology plan",
  );
  if (plan.clientSurfaceRole !== "home") throw new Error("Home topology planner returned a non-Home surface");
  const expectedRunId = `run_operator_chat_${materialized.chatId}`;
  const expectedChatInterfaceId = `world_operator_chat_${materialized.chatId}`;
  const expectedModelInterfaceId = `${request.home.providerInterfaceId}_${materialized.chatId}`;
  const expectedWorkerInterfaceId = `${request.home.workerInterfaceId}_${materialized.chatId}`;
  if (plan.runId !== expectedRunId || plan.chatInterfaceId !== expectedChatInterfaceId) {
    throw new Error("Home topology planner changed the deterministic Home identity");
  }
  if (plan.modelInterfaceId !== expectedModelInterfaceId) {
    throw new Error("Home topology planner changed the exact provider interface identity");
  }
  if (plan.providerModel !== request.home.providerModel) throw new Error("Home topology planner changed the selected model");
  if (plan.workerInterfaceId !== expectedWorkerInterfaceId) {
    throw new Error("Home topology planner changed the exact worker interface identity");
  }
  const serviceTier = optionalText(plan.serviceTier, "Home planned service tier", 80);
  if (request.home.serviceTier && serviceTier !== request.home.serviceTier) throw new Error("Home topology planner changed the selected service tier");
  const reasoningEffort = optionalText(plan.reasoningEffort, "Home planned reasoning effort", 80);
  if (request.home.reasoningEffort && reasoningEffort !== request.home.reasoningEffort) {
    throw new Error("Home topology planner changed the selected reasoning effort");
  }
  const authoringProfile = exactObject(plan.authoringProfile, [
    "ee_id", "worker_choice_interface", "door_host_slots", "external_inputs", "external_relays",
    "model_outputs", "context_artifact_bindings", "selected_worker_environment_kinds",
    "selected_tools",
  ], ["ee_id", "worker_choice_interface", "door_host_slots"], "Home authoring profile");
  const expectedAuthoringProfile = {
    ee_id: materialized.rootEeId,
    worker_choice_interface: request.home.workerInterfaceId,
    door_host_slots: request.home.doorHostSlots,
    ...(request.home.contextArtifactBindings.length
      ? { context_artifact_bindings: request.home.contextArtifactBindings }
      : {}),
    ...(request.home.selectedWorkerEnvironmentKinds.length
      ? { selected_worker_environment_kinds: request.home.selectedWorkerEnvironmentKinds }
      : {}),
  };
  if (stableJson(authoringProfile) !== stableJson(expectedAuthoringProfile)) {
    throw new Error("Home topology planner changed the requested authoring profile");
  }
  if (!Array.isArray(plan.writes) || !plan.writes.length) throw new Error("Home topology plan has no writes");
  const requiredInterfaceIds = parseSortedUniqueIds(plan.requiredInterfaceIds, "Home required interfaces");
  const requiredToolIds = parseSortedUniqueIds(plan.requiredToolIds, "Home required tools");
  for (const interfaceId of [expectedChatInterfaceId, expectedModelInterfaceId, expectedWorkerInterfaceId]) {
    if (!requiredInterfaceIds.includes(interfaceId)) {
      throw new Error(`Home topology plan omitted required exact interface '${interfaceId}'`);
    }
  }
  return {
    ...plan,
    runId: requiredId(plan.runId, "Home run id"),
    chatInterfaceId: requiredId(plan.chatInterfaceId, "Home interface id"),
    modelInterfaceId: requiredId(plan.modelInterfaceId, "Home provider interface id"),
    workerInterfaceId: requiredId(plan.workerInterfaceId, "Home worker interface id"),
    reasoningEffort,
    serviceTier,
    authoringProfile,
    requiredInterfaceIds,
    requiredToolIds,
  };
}

function parsePlannedWrite(roots, value, index, plan, request, materialized) {
  const write = exactObject(value, ["filePath", "content", "tag", "expectedBeforeHash"], ["filePath", "content", "tag", "expectedBeforeHash"], `Home write ${index + 1}`);
  const destination = requiredAbsolutePath(write.filePath, `Home write ${index + 1} destination`);
  if (typeof write.content !== "string" || !write.content.length) throw new Error(`Home write ${index + 1} has no content`);
  if (write.expectedBeforeHash !== null) {
    throw new Error(`Home write ${index + 1} must target an absent fresh-workspace file`);
  }
  let parsed;
  try {
    parsed = JSON.parse(write.content);
  } catch (error) {
    throw new Error(`Home write ${index + 1} is malformed JSON: ${errorMessage(error)}`);
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error(`Home write ${index + 1} must contain a JSON object`);
  }
  const runPath = path.join(roots.runsRoot, `${plan.runId}.json`);
  if (pathInside(roots.runsRoot, destination)) {
    const id = requiredId(parsed.id, "Home planned Run id");
    if (destination !== path.join(roots.runsRoot, `${id}.json`) || write.tag !== `run:${id}`) {
      throw new Error(`Home write ${index + 1} must create its exact declared Run path and tag`);
    }
    validatePlannedRunRecord(parsed, plan, materialized);
    if (destination === runPath) validateHomeRunRecord(parsed, plan, request, materialized);
  } else if (pathInside(roots.interfaceRoot, destination)) {
    const owner = plannedOwner(roots.interfaceRoot, destination, `Home interface write ${index + 1}`);
    if (!plan.requiredInterfaceIds.includes(owner)) {
      throw new Error(`Home write ${index + 1} targets unrequired interface '${owner}'`);
    }
    if (["interface.json", "policy.json"].includes(path.basename(destination)) && parsed.id !== owner) {
      throw new Error(`Home write ${index + 1} changes interface identity '${owner}'`);
    }
  } else if (pathInside(roots.toolRoot, destination)) {
    const owner = plannedOwner(roots.toolRoot, destination, `Home tool write ${index + 1}`);
    if (!plan.requiredToolIds.includes(owner)) {
      throw new Error(`Home write ${index + 1} targets unrequired tool '${owner}'`);
    }
    if (path.basename(destination) === "tool.json" && parsed.id !== owner) {
      throw new Error(`Home write ${index + 1} changes tool identity '${owner}'`);
    }
  } else if (pathInside(roots.artifactRoot, destination)) {
    const owner = plannedArtifactOwner(roots.artifactRoot, destination);
    if (write.tag !== `artifact:${owner}`) {
      throw new Error(`Home artifact write ${index + 1} differs from its exact owner '${owner}'`);
    }
  } else {
    throw new Error(`Home write ${index + 1} targets a file outside its exact interface/tool/artifact/run plan`);
  }
  return {
    destination,
    content: write.content,
    tag: requiredHumanText(write.tag, `Home write ${index + 1} tag`, 240),
    expectedBeforeDigest: null,
    afterDigest: sha256(Buffer.from(write.content)),
  };
}

function plannedArtifactOwner(root, destination) {
  const parts = path.relative(root, destination).split(path.sep);
  const owner = parts[0];
  if (parts.length !== 2 || parts[1] !== "artifact.json" || !owner || owner.trim() !== owner
    || owner === "." || owner === ".." || /[/\\\0]/.test(owner)) {
    throw new Error("Home artifact write must name one exact artifact owner and its artifact.json");
  }
  return owner;
}

function plannedOwner(root, destination, label) {
  const segments = path.relative(root, destination).split(path.sep);
  if (segments.length < 2 || !segments[0] || !segments[1]) {
    throw new Error(`${label} does not name an owner folder and file`);
  }
  return requiredId(segments[0], `${label} owner`);
}

function validatePlannedHomeRuns(roots, plan, writes, home) {
  const records = new Map();
  for (const write of writes.filter((write) => pathInside(roots.runsRoot, write.destination))) {
    const record = JSON.parse(write.content);
    if (records.has(record.id)) throw new Error("Home topology repeats a Run identity");
    records.set(record.id, record);
  }
  const declared = new Map([[home.id, home]]);
  const execution = [home];
  const context = home.interface_state?.[plan.chatInterfaceId]?.voice_context;
  if (context !== undefined) {
    const fields = ["run_id", "control_interface_id", "provider_id", "delegation_interface_id"];
    exactObject(context, fields, fields, "Home voice context");
    for (const field of fields) requiredId(context[field], `Home voice ${field}`);
    const voice = records.get(context.run_id);
    const origin = voice?.interface_state?.[context.control_interface_id];
    const topology = voice?.interface_state?.[context.provider_id]?.run_authoring_binding;
    if (!voice || voice.id === home.id || origin?.home_run !== home.id || origin?.home_interface !== plan.chatInterfaceId
      || topology?.run !== voice.id || topology?.worker !== voice.worker || topology?.provider_interface !== context.provider_id
      || home.interface_state?.[context.provider_id]
      || !hasActiveRunEdge(voice, context.control_interface_id, context.provider_id)
      || !hasActiveRunEdge(voice, context.provider_id, context.delegation_interface_id)
      || !hasActiveRunEdge(home, context.delegation_interface_id, plan.modelInterfaceId)
      || !Array.isArray(topology.checkpoints) || !topology.checkpoints.some((cp) => cp.role === "provider_interface_output"
        && cp.outbound_interface_bindings?.some((binding) => binding.interface === context.delegation_interface_id))) {
      throw new Error("Home voice Run differs from its exact independent execution binding");
    }
    declared.set(voice.id, voice);
    execution.push(voice);
  }
  for (const run of execution) {
    for (const [owner, state] of Object.entries(run.interface_state ?? {})) {
      const writer = state.run_authoring_binding?.writer_run ?? state.writer_run;
      if (writer === undefined || writer === null) continue;
      const id = requiredId(writer.run, "Home declared writer Run");
      if (Object.hasOwn(writer, "initial_record_format") && !["pretty", "compact"].includes(writer.initial_record_format)) {
        throw new Error("Home writer Run declaration has invalid initial record format");
      }
      const initial = writer.initial_record;
      if (declared.has(id) || initial?.id !== id || initial.ee !== run.ee || initial.worker !== run.worker
        || initial.interface_state?.[owner]?.file_writer_target?.run !== run.id) {
        throw new Error("Home writer Run declaration changes its owner, target, worker or EE");
      }
      declared.set(id, initial);
    }
  }
  if (records.size !== declared.size) throw new Error("Home topology omits or adds an undeclared Run");
  for (const [id, initial] of declared) {
    if (!records.has(id) || stableJson(records.get(id)) !== stableJson(initial)) {
      throw new Error(`Home planned Run '${id}' differs from its exact declared initial record`);
    }
  }
  return [...records.values()];
}

function hasActiveRunEdge(run, inbound, outbound) {
  return run.checkpoints.some((cp) => run.active_checkpoints.includes(cp.id)
    && cp.interface_binding?.interface === inbound
    && cp.outbound_interface_bindings?.some((binding) => binding.interface === outbound));
}

async function validatePlannedHomeOwnership(roots, plan, writes, runs) {
  const documents = new Map();
  const interfaceOwners = new Set();
  const toolOwners = new Set();
  const artifactOwners = new Set();
  for (const write of writes) {
    if (documents.has(write.destination)) throw new Error(`Home topology plan repeats destination '${write.destination}'`);
    documents.set(write.destination, JSON.parse(write.content));
    if (pathInside(roots.interfaceRoot, write.destination)) {
      interfaceOwners.add(plannedOwner(roots.interfaceRoot, write.destination, "Home interface write"));
    } else if (pathInside(roots.toolRoot, write.destination)) {
      toolOwners.add(plannedOwner(roots.toolRoot, write.destination, "Home tool write"));
    } else if (pathInside(roots.artifactRoot, write.destination)) {
      artifactOwners.add(plannedArtifactOwner(roots.artifactRoot, write.destination));
    }
  }
  const checkpoints = runs.flatMap((run) => run.checkpoints);
  const readableArtifacts = new Set(checkpoints.flatMap((checkpoint) => (
    (checkpoint.artifact_bindings ?? []).filter((binding) => binding.access?.read !== false).map((binding) => binding.id)
  )));
  const neighbors = new Map();
  const connect = (from, to) => {
    if (!neighbors.has(from)) neighbors.set(from, new Set());
    neighbors.get(from).add(to);
  };
  for (const run of runs) {
    const active = new Set(run.active_checkpoints);
    for (const checkpoint of run.checkpoints) {
      if (!active.has(checkpoint.id) || !checkpoint.interface_binding) continue;
      const inbound = checkpoint.interface_binding.interface;
      for (const outbound of checkpoint.outbound_interface_bindings ?? []) {
        connect(inbound, outbound.interface);
        connect(outbound.interface, inbound);
      }
    }
  }
  const connected = new Set([plan.chatInterfaceId]);
  const pending = [...connected];
  while (pending.length) {
    for (const next of neighbors.get(pending.pop()) ?? []) {
      if (connected.has(next)) continue;
      connected.add(next);
      pending.push(next);
    }
  }
  const policy = documents.get(path.join(roots.interfaceRoot, plan.workerInterfaceId, "policy.json"));
  if (!Array.isArray(policy?.ceiling?.interfaces) || !Array.isArray(policy?.ceiling?.tools)) {
    throw new Error("Home topology plan must declare exact worker interface and tool ceilings");
  }
  const allowedInterfaces = new Set(policy.ceiling.interfaces);
  const allowedTools = new Set(policy.ceiling.tools);
  for (const owner of interfaceOwners) {
    const descriptor = documents.get(path.join(roots.interfaceRoot, owner, "interface.json"));
    if (descriptor?.id !== owner) throw new Error(`Home topology plan omitted exact interface descriptor '${owner}'`);
    if (!connected.has(owner) || !allowedInterfaces.has(owner)) {
      throw new Error(`Home planned interface '${owner}' is outside its active Home bindings or worker ceiling`);
    }
  }
  for (const owner of toolOwners) {
    const descriptor = documents.get(path.join(roots.toolRoot, owner, "tool.json"));
    if (descriptor?.id !== owner) throw new Error(`Home topology plan omitted exact tool descriptor '${owner}'`);
    const door = requiredId(descriptor.interface, `Home tool '${owner}' interface`);
    if (!connected.has(door) || !allowedInterfaces.has(door) || !allowedTools.has(owner)) {
      throw new Error(`Home planned tool '${owner}' is outside its active Home bindings or worker ceiling`);
    }
  }
  for (const run of runs) {
    for (const [owner, state] of Object.entries(run.interface_state ?? {})) {
      if (state.prelude_sources === undefined) continue;
      if (!connected.has(owner) || !allowedInterfaces.has(owner) || !Array.isArray(state.prelude_sources)) {
        throw new Error("Home prelude snapshots require an exact connected owner");
      }
      for (const source of state.prelude_sources) {
        exactObject(source, ["source", "snapshot", "source_sha256"], ["source", "snapshot", "source_sha256"], "Home prelude source");
        for (const reference of [source.source, source.snapshot]) {
          exactObject(reference, ["id", "content"], ["id", "content"], "Home prelude artifact reference");
          if (!Array.isArray(reference.content) || reference.content.length !== 32
            || reference.content.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)) {
            throw new Error("Home prelude artifact reference has an invalid content pin");
          }
        }
        const sourceId = source?.source?.id;
        const snapshotId = source?.snapshot?.id;
        if (typeof sourceId !== "string" || typeof snapshotId !== "string") throw new Error("Home prelude source has no exact artifact identity");
        const sourcePath = path.join(roots.artifactRoot, sourceId, "artifact.json");
        const snapshotPath = path.join(roots.artifactRoot, snapshotId, "artifact.json");
        if (plannedArtifactOwner(roots.artifactRoot, sourcePath) !== sourceId
          || plannedArtifactOwner(roots.artifactRoot, snapshotPath) !== snapshotId) throw new Error("Home prelude source has a noncanonical artifact path");
        const snapshot = writes.find((write) => write.destination === snapshotPath);
        if (!snapshot) throw new Error("Home prelude snapshot differs from its exact source evidence");
        const fields = ["id", "run", "source", "source_sha256", "source_content"];
        const wrapper = exactObject(documents.get(snapshotPath), fields, fields, "Home prelude snapshot");
        if (sourceId === snapshotId || wrapper.id !== snapshotId || wrapper.run !== run.id
          || stableJson(wrapper.source) !== stableJson(source.source)
          || typeof source.source_sha256 !== "string" || !DIGEST.test(source.source_sha256)
          || wrapper.source_sha256 !== source.source_sha256 || typeof wrapper.source_content !== "string"
          || sha256(Buffer.from(wrapper.source_content, "utf8")) !== source.source_sha256
          || !Buffer.from(blake3(Buffer.from(stableJson(wrapper)))).equals(Buffer.from(source.snapshot.content))) {
          throw new Error("Home prelude snapshot differs from its exact source evidence");
        }
        const original = JSON.parse(wrapper.source_content);
        if (original?.id !== sourceId
          || !Buffer.from(blake3(Buffer.from(stableJson(original)))).equals(Buffer.from(source.source.content))) {
          throw new Error("Home prelude snapshot differs from its exact source evidence");
        }
        await requireClientAuthoringRoot(path.dirname(sourcePath));
        if (sha256(await readFileNoFollow(sourcePath)) !== source.source_sha256) {
          throw new Error("Home prelude snapshot source changed during planning");
        }
        readableArtifacts.add(snapshotId);
      }
    }
  }
  for (const owner of artifactOwners) {
    if (!readableArtifacts.has(owner)) throw new Error(`Home planned artifact '${owner}' has no exact readable checkpoint binding or pinned source snapshot`);
  }
}

function validatePlannedRunRecord(record, plan, materialized) {
  if (!runRecordValidator(record)) {
    const details = runRecordValidator.errors
      ?.map((error) => `${error.instancePath || "/"} ${error.message}`)
      .join("; ") || "unknown schema error";
    throw new Error(`Home run record failed the canonical schema: ${details}`);
  }
  if (record.ee !== materialized.rootEeId || record.worker !== plan.workerInterfaceId) {
    throw new Error("Home run record changed its run, EE, or exact worker identity");
  }
  const checkpointIds = new Set();
  const referencedInterfaces = new Set();
  for (const checkpoint of record.checkpoints) {
    if (checkpoint.run !== record.id || checkpointIds.has(checkpoint.id)) {
      throw new Error("Home run record contains a foreign or duplicate checkpoint");
    }
    checkpointIds.add(checkpoint.id);
    if (checkpoint.interface_binding) referencedInterfaces.add(checkpoint.interface_binding.interface);
    for (const binding of checkpoint.outbound_interface_bindings ?? []) {
      referencedInterfaces.add(binding.interface);
    }
  }
  if (new Set(record.active_checkpoints).size !== record.active_checkpoints.length) {
    throw new Error("Home run record repeats an active checkpoint");
  }
  for (const checkpointId of record.active_checkpoints) {
    if (!checkpointIds.has(checkpointId)) throw new Error("Home run record activates an absent checkpoint");
  }
  for (const interfaceId of referencedInterfaces) {
    if (!plan.requiredInterfaceIds.includes(interfaceId)) {
      throw new Error(`Home run record references unplanned interface '${interfaceId}'`);
    }
  }
  return referencedInterfaces;
}

function validateHomeRunRecord(record, plan, request, materialized) {
  const referencedInterfaces = validatePlannedRunRecord(record, plan, materialized);
  if (record.id !== plan.runId) throw new Error("Home run record changed its exact run identity");
  const clientState = record.interface_state?.[plan.chatInterfaceId];
  if (
    !clientState
    || clientState.client_surface_role !== "home"
    || clientState.provider_choice_interface !== request.home.providerInterfaceId
    || clientState.provider_runtime_interface !== plan.modelInterfaceId
    || clientState.worker_choice_interface !== request.home.workerInterfaceId
  ) {
    throw new Error("Home run record changed its client/provider/worker surface identity");
  }
  for (const interfaceId of [plan.chatInterfaceId, plan.modelInterfaceId, plan.workerInterfaceId]) {
    if (!referencedInterfaces.has(interfaceId)) {
      throw new Error(`Home run record does not bind exact interface '${interfaceId}'`);
    }
  }
}

function workspaceHomeSidecar(content, request, materialized, plan, projectProfiles, agentPlans, voiceContext) {
  let sidecar;
  try {
    sidecar = JSON.parse(content);
  } catch (error) {
    throw new Error(`Home Octopus OS sidecar is malformed JSON: ${errorMessage(error)}`);
  }
  exactObject(
    sidecar,
    [
      "schema_version", "workspace", "interface", "owner_interface", "component", "surface_role",
      "provider_choice_interface", "worker_choice_interface", "authoring_profile", "task_authoring_profiles", "presentation",
      "voice_context",
    ],
    [
      "schema_version", "workspace", "interface", "owner_interface", "component", "surface_role",
      "provider_choice_interface", "worker_choice_interface", "authoring_profile",
    ],
    "Home Octopus OS sidecar",
  );
  if (
    sidecar.schema_version !== 1
    || sidecar.workspace !== "octopus_os"
    || sidecar.interface !== plan.chatInterfaceId
    || sidecar.owner_interface !== plan.chatInterfaceId
    || sidecar.component !== "client"
    || sidecar.surface_role !== "home"
    || sidecar.provider_choice_interface !== request.home.providerInterfaceId
    || sidecar.worker_choice_interface !== request.home.workerInterfaceId
    || stableJson(sidecar.authoring_profile) !== stableJson(plan.authoringProfile)
  ) {
    throw new Error("Home Octopus OS sidecar drifted from the planned surface identity");
  }
  if (Object.hasOwn(sidecar, "voice_context") !== (voiceContext !== undefined)
    || voiceContext !== undefined && stableJson(sidecar.voice_context) !== stableJson(voiceContext)) {
    throw new Error("Home sidecar voice context differs from its exact execution Run");
  }
  const taskAuthoringProfiles = [{ key: "root" }, ...request.projects].map((project) => {
    const profile = projectProfiles.get(project.key);
    if (!profile) throw new Error(`Workspace Project '${project.key}' has no verified authoring profile`);
    return structuredClone(profile);
  });
  taskAuthoringProfiles.push(...agentPlans.map((agent) => structuredClone(agent.profile)));
  const presentation = {
    name: request.root.name,
    projects: request.projects.map((project) => ({
      ee_id: requiredMapValue(materialized.projectEeIds, project.key),
      name: project.name,
      icon: project.presentation.icon,
      color: project.presentation.color,
    })),
  };
  return `${JSON.stringify({
    ...sidecar,
    task_authoring_profiles: taskAuthoringProfiles,
    presentation,
  }, null, 2)}\n`;
}

function eeDocument(write) {
  const record = write.document ?? {
    ee_id: write.eeId,
    template_name: write.templateName,
    parent_ee_id: write.parentEeId,
    goal: write.goal,
    config: write.config ?? {},
  };
  if (write.policyNarrowing && Object.keys(write.policyNarrowing).length) {
    record.policy_narrowing = write.policyNarrowing;
  }
  return `${JSON.stringify(record, null, 2)}\n`;
}

function projectChildConfig(value) {
  if (value === undefined) return {};
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Project parent EE config must be an object");
  }
  if (value.octopus_os === undefined) return {};
  if (!value.octopus_os || typeof value.octopus_os !== "object" || Array.isArray(value.octopus_os)) {
    throw new Error("Project parent EE Octopus OS config must be an object");
  }
  return { octopus_os: structuredClone(value.octopus_os) };
}

function workspaceReceipt(request, materialized, plan, writes) {
  return {
    schema: "octopus.workspace_setup_receipt",
    schema_version: 1,
    operation_id: request.operationId,
    request_digest: request.digest,
    completed_at: "",
    root_ee_id: materialized.rootEeId,
    ...(materialized.existingRoot ? { existing_root: materialized.existingRoot } : {}),
    home_run_id: plan.runId,
    home_interface_id: plan.chatInterfaceId,
    project_ee_ids: request.projects.map((project) => ({
      key: project.key,
      ee_id: requiredMapValue(materialized.projectEeIds, project.key),
    })),
    writes: writes.map((write) => ({
      path: write.destination,
      digest: write.afterDigest,
      tag: write.tag,
      publish_barrier: write.publishBarrier,
    })),
  };
}

function projectMutationReceipt(request, planned) {
  const outcome = planned.outcome.action === "create_project"
    ? {
        ee_id: planned.outcome.eeId,
        parent_ee_id: planned.outcome.parentEeId,
        template_name: planned.outcome.templateName,
        goal: planned.outcome.goal,
      }
    : planned.outcome.action === "delete_project"
      ? { ee_id: planned.outcome.eeId }
      : { project_ee_ids: planned.outcome.projectEeIds };
  return {
    schema: "octopus.workspace_project_receipt",
    schema_version: 1,
    operation_id: request.operationId,
    request_digest: request.digest,
    completed_at: "",
    action: request.action,
    root_ee_id: request.workspace.rootEeId,
    home_run_id: request.workspace.homeRunId,
    home_interface_id: request.workspace.homeInterfaceId,
    outcome,
    writes: planned.writes.map((write) => ({
      path: write.destination,
      digest: write.afterDigest,
      tag: write.tag,
      publish_barrier: write.publishBarrier,
    })),
  };
}

async function stageTransaction(roots, request, writes, receipt) {
  return stageWorkspaceTransaction(
    roots,
    request,
    writes,
    receipt,
    "octopus.workspace_setup_transaction",
  );
}

async function stageProjectTransaction(roots, request, writes, receipt) {
  return stageWorkspaceTransaction(
    roots,
    request,
    writes,
    receipt,
    "octopus.workspace_project_transaction",
  );
}

async function stageWorkspaceTransaction(roots, request, writes, receipt, schema) {
  if (schema === "octopus.workspace_setup_transaction") await verifySetupRootObservation(roots, receipt);
  const transactionRoot = path.join(roots.transactionsRoot, request.operationId);
  if (!pathInside(roots.transactionsRoot, transactionRoot)) throw new Error("Workspace transaction path escapes its state root");
  const stagingRoot = path.join(
    roots.transactionsRoot,
    `.stage-${request.operationId}-${crypto.randomBytes(8).toString("hex")}`,
  );
  await fs.mkdir(stagingRoot, { recursive: false, mode: 0o700 });
  await fs.mkdir(path.join(stagingRoot, "after"), { mode: 0o700 });
  await fs.mkdir(path.join(stagingRoot, "before"), { mode: 0o700 });
  await syncDirectory(roots.transactionsRoot);
  const journalWrites = [];
  try {
    for (const [index, write] of writes.entries()) {
      const destinationRoot = allowedDestinationRoot(roots, write.destination);
      await validateExistingPath(destinationRoot, write.destination);
      const before = await readFileIfPresent(write.destination);
      const beforeDigest = before ? sha256(before) : null;
      if (beforeDigest === write.afterDigest) {
        if (beforeDigest !== write.expectedBeforeDigest) {
          throw new Error(`Workspace write '${write.tag}' changed before staging`);
        }
        journalWrites.push({
          destination: write.destination,
          expected_before_digest: write.expectedBeforeDigest,
          before_digest: beforeDigest,
          after_digest: write.afterDigest,
          after_file: "",
          before_file: "",
          tag: write.tag,
          publish_barrier: write.publishBarrier,
          state: "unchanged",
        });
        continue;
      }
      if (beforeDigest !== write.expectedBeforeDigest) {
        throw new Error(`Workspace write '${write.tag}' expected ${write.expectedBeforeDigest ?? "an absent file"}, found ${beforeDigest ?? "an absent file"}`);
      }
      if (write.afterDigest === null && beforeDigest === null) {
        throw new Error(`Client authoring deletion '${write.tag}' expected an existing file`);
      }
      let afterFile = "";
      if (write.content !== null) {
        afterFile = path.join("after", `${String(index).padStart(4, "0")}.bin`);
        await writeSyncedFile(path.join(stagingRoot, afterFile), Buffer.from(write.content), 0o600, "wx");
      }
      let beforeFile = "";
      if (before) {
        beforeFile = path.join("before", `${String(index).padStart(4, "0")}.bin`);
        await writeSyncedFile(path.join(stagingRoot, beforeFile), before, 0o600, "wx");
      }
      journalWrites.push({
        destination: write.destination,
        expected_before_digest: write.expectedBeforeDigest,
        before_digest: beforeDigest,
        after_digest: write.afterDigest,
        after_file: afterFile,
        before_file: beforeFile,
        tag: write.tag,
        publish_barrier: write.publishBarrier,
        state: "staged",
      });
    }
    await syncDirectory(path.join(stagingRoot, "after"));
    await syncDirectory(path.join(stagingRoot, "before"));
    await syncDirectory(stagingRoot);
    const journal = {
      schema,
      schema_version: 1,
      operation_id: request.operationId,
      request_digest: request.digest,
      phase: "staged",
      receipt,
      writes: journalWrites,
    };
    await writeJournal(stagingRoot, journal);
    await fs.rename(stagingRoot, transactionRoot);
    await syncDirectory(roots.transactionsRoot);
    return transactionRoot;
  } catch (error) {
    await fs.rm(stagingRoot, { recursive: true, force: true });
    await syncDirectory(roots.transactionsRoot);
    throw error;
  }
}

async function stageClientAuthoringTransaction(roots, plan) {
  const transactionRoot = path.join(roots.transactionsRoot, plan.operationId);
  if (!pathInside(roots.transactionsRoot, transactionRoot)) {
    throw new Error("Client authoring transaction path escapes its state root");
  }
  if (await directoryExists(transactionRoot)) {
    throw new Error(`Client authoring transaction '${plan.operationId}' already exists`);
  }
  const stagingRoot = path.join(
    roots.transactionsRoot,
    `.stage-${plan.operationId}-${crypto.randomBytes(8).toString("hex")}`,
  );
  await fs.mkdir(stagingRoot, { recursive: false, mode: 0o700 });
  await fs.mkdir(path.join(stagingRoot, "after"), { mode: 0o700 });
  await fs.mkdir(path.join(stagingRoot, "before"), { mode: 0o700 });
  await fs.mkdir(path.join(stagingRoot, "removed"), { mode: 0o700 });
  await syncDirectory(roots.transactionsRoot);
  try {
    const journalWrites = [];
    for (const [index, write] of plan.writes.entries()) {
      const destinationRoot = allowedDestinationRoot(roots, write.destination);
      await validateExistingPath(destinationRoot, write.destination);
      const before = await readFileIfPresent(write.destination);
      const beforeDigest = before ? sha256(before) : null;
      if (beforeDigest !== write.expectedBeforeDigest) {
        throw new Error(
          `Client authoring write '${write.tag}' expected ${write.expectedBeforeDigest ?? "an absent file"}, found ${beforeDigest ?? "an absent file"}`,
        );
      }
      if (beforeDigest === write.afterDigest) {
        journalWrites.push({
          destination: write.destination,
          expected_before_digest: write.expectedBeforeDigest,
          before_digest: beforeDigest,
          after_digest: write.afterDigest,
          after_file: "",
          before_file: "",
          tag: write.tag,
          publish_barrier: write.publishBarrier,
          state: "unchanged",
        });
        continue;
      }
      let afterFile = "";
      if (write.content !== null) {
        afterFile = path.join("after", `${String(index).padStart(4, "0")}.bin`);
        await writeSyncedFile(path.join(stagingRoot, afterFile), Buffer.from(write.content), 0o600, "wx");
      }
      let beforeFile = "";
      if (before) {
        beforeFile = path.join("before", `${String(index).padStart(4, "0")}.bin`);
        await writeSyncedFile(path.join(stagingRoot, beforeFile), before, 0o600, "wx");
      }
      journalWrites.push({
        destination: write.destination,
        expected_before_digest: write.expectedBeforeDigest,
        before_digest: beforeDigest,
        after_digest: write.afterDigest,
        after_file: afterFile,
        before_file: beforeFile,
        tag: write.tag,
        publish_barrier: write.publishBarrier,
        state: "staged",
      });
    }
    const journalRemovals = [];
    for (const [index, removal] of plan.removals.entries()) {
      const destinationRoot = allowedDestinationRoot(roots, removal.destination);
      await validateExistingRemovalPath(destinationRoot, removal.destination, removal.type);
      const identity = await snapshotClientAuthoringRemoval(removal);
      journalRemovals.push({
        destination: removal.destination,
        type: removal.type,
        expected_dev: removal.expectedDev,
        expected_ino: removal.expectedIno,
        tree_digest: identity.treeDigest,
        tree_entries: identity.treeEntries,
        tombstone: path.join("removed", String(index).padStart(4, "0")),
        tag: removal.tag,
        state: "staged",
      });
    }
    await syncDirectory(path.join(stagingRoot, "after"));
    await syncDirectory(path.join(stagingRoot, "before"));
    await syncDirectory(path.join(stagingRoot, "removed"));
    await syncDirectory(stagingRoot);
    await writeJournal(stagingRoot, {
      schema: "octopus.client_authoring_transaction",
      schema_version: 1,
      operation_id: plan.operationId,
      label: plan.label,
      phase: "staged",
      writes: journalWrites,
      removals: journalRemovals,
    });
    await fs.rename(stagingRoot, transactionRoot);
    await syncDirectory(roots.transactionsRoot);
    return transactionRoot;
  } catch (error) {
    await fs.rm(stagingRoot, { recursive: true, force: true });
    await syncDirectory(roots.transactionsRoot);
    throw error;
  }
}

async function commitTransaction(roots, transactionRoot, onProgress) {
  let journal = await readJournal(roots, transactionRoot);
  if (journal.phase === "committed") {
    await finalizeCommittedTransaction(roots, transactionRoot, journal);
    return;
  }
  if (journal.phase === "rolling_back") {
    await rollbackTransaction(roots, transactionRoot);
    throw new Error(`Workspace setup operation '${journal.operation_id}' was rolled back`);
  }
  if (journal.schema === "octopus.workspace_setup_transaction") await verifySetupRootObservation(roots, journal.receipt);
  journal.phase = "committing";
  await writeJournal(transactionRoot, journal);
  const ordered = [...journal.writes.keys()].sort((left, right) => (
    Number(journal.writes[left].publish_barrier) - Number(journal.writes[right].publish_barrier)
    || left - right
  ));
  for (const [position, index] of ordered.entries()) {
    const write = journal.writes[index];
    if (write.publish_barrier && journal.schema === "octopus.workspace_setup_transaction") {
      await verifySetupRootObservation(roots, journal.receipt);
    }
    if (write.state === "unchanged" || write.state === "applied") {
      await verifyAppliedWrite(write);
      continue;
    }
    await applyJournalWrite(roots, transactionRoot, write);
    journal.writes[index].state = "applied";
    await writeJournal(transactionRoot, journal);
    reportProgress(
      onProgress,
      journal.operation_id,
      write.publish_barrier ? "publishing" : "applying",
      Math.round(28 + (position + 1) / ordered.length * 62),
      write.publish_barrier ? "Publishing Home" : "Creating your space",
    );
  }
  await verifyJournalWrites(journal.writes);
  if (journal.schema === "octopus.workspace_setup_transaction") await verifySetupRootObservation(roots, journal.receipt);
  journal.receipt = {
    ...journal.receipt,
    completed_at: journal.receipt.completed_at || new Date().toISOString(),
  };
  journal.phase = "published";
  await writeJournal(transactionRoot, journal);
  await writeJournalReceipt(roots, journal);
  journal.phase = "committed";
  await writeJournal(transactionRoot, journal);
  reportProgress(onProgress, journal.operation_id, "complete", 100, "Octopus is ready");
  await finalizeCommittedTransaction(roots, transactionRoot, journal);
}

async function commitClientAuthoringJournal(roots, transactionRoot) {
  let journal = await readJournal(roots, transactionRoot);
  if (journal.schema !== "octopus.client_authoring_transaction") {
    throw new Error("Client authoring commit received another transaction schema");
  }
  if (journal.phase === "committed") {
    await finalizeClientAuthoringTransaction(roots, transactionRoot, journal);
    return;
  }
  if (journal.phase !== "published") {
    journal.phase = "committing";
    await writeJournal(transactionRoot, journal);
  }
  const ordered = [...journal.writes.keys()].sort((left, right) => (
    Number(journal.writes[left].publish_barrier) - Number(journal.writes[right].publish_barrier)
    || left - right
  ));
  for (const index of ordered) {
    const write = journal.writes[index];
    if (write.state === "unchanged" || write.state === "applied") {
      await verifyAppliedWrite(write);
      continue;
    }
    await applyJournalWrite(roots, transactionRoot, write);
    journal.writes[index].state = "applied";
    await writeJournal(transactionRoot, journal);
  }
  await verifyJournalWrites(journal.writes);
  journal.phase = "published";
  await writeJournal(transactionRoot, journal);
  for (const index of journal.removals.keys()) {
    await applyClientAuthoringRemoval(roots, transactionRoot, journal.removals[index], journal);
    journal.removals[index].state = "moved";
    await writeJournal(transactionRoot, journal);
  }
  await verifyClientAuthoringRemovals(roots, transactionRoot, journal);
  journal.phase = "committed";
  await writeJournal(transactionRoot, journal);
  await writeClientAuthoringReceipt(roots, clientAuthoringReceipt(journal));
  await finalizeClientAuthoringTransaction(roots, transactionRoot, journal);
}

async function applyClientAuthoringRemoval(roots, transactionRoot, removal, journal) {
  const destinationRoot = allowedDestinationRoot(roots, removal.destination);
  await requireClientAuthoringRoot(destinationRoot);
  const tombstone = transactionFile(transactionRoot, removal.tombstone, "removal tombstone");
  const source = await inspectClientAuthoringRemoval(removal.destination, removal.type);
  const removed = await inspectClientAuthoringRemoval(tombstone, removal.type);
  const retiredState = await isRetiredInterfaceStateRemoval(roots, journal, removal);
  if (source.exists && removed.exists) {
    throw new Error(`Client authoring removal '${removal.tag}' exists at both source and tombstone`);
  }
  if (source.exists) {
    assertClientAuthoringRemovalIdentity(source, removal, "source", retiredState);
    const parentStat = await fs.lstat(path.dirname(removal.destination));
    if (!parentStat.isDirectory() || parentStat.isSymbolicLink()) {
      throw new Error(`Client authoring removal '${removal.tag}' parent changed`);
    }
    await fs.rename(removal.destination, tombstone);
    await syncDirectory(path.dirname(removal.destination));
    await syncDirectory(path.dirname(tombstone));
  } else if (!removed.exists) {
    throw new Error(`Client authoring removal '${removal.tag}' is absent from source and tombstone`);
  }
  const durable = await inspectClientAuthoringRemoval(tombstone, removal.type);
  assertClientAuthoringRemovalIdentity(durable, removal, "tombstone", retiredState);
  if (await pathExists(removal.destination)) {
    throw new Error(`Client authoring removal '${removal.tag}' remained published after tombstoning`);
  }
}

async function verifyClientAuthoringRemovals(roots, transactionRoot, journal) {
  for (const removal of journal.removals) {
    if (await pathExists(removal.destination)) {
      throw new Error(`Client authoring removal '${removal.tag}' is still present`);
    }
    const tombstone = transactionFile(transactionRoot, removal.tombstone, "removal tombstone");
    const durable = await inspectClientAuthoringRemoval(tombstone, removal.type);
    assertClientAuthoringRemovalIdentity(durable, removal, "tombstone",
      await isRetiredInterfaceStateRemoval(roots, journal, removal));
  }
}

async function isRetiredInterfaceStateRemoval(roots, journal, removal) {
  if (![roots.privateInterfacesRoot, roots.interfaceHandInRoot].includes(path.dirname(removal.destination))) return false;
  const interfaceId = path.basename(removal.destination);
  if (!SAFE_ID.test(interfaceId)) return false;
  const descriptorPath = path.join(roots.interfaceRoot, interfaceId);
  const descriptor = journal.removals.find((entry) => entry.destination === descriptorPath);
  if (!descriptor || descriptor.state !== "moved" || await pathExists(descriptorPath)) return false;
  const references = (value) => typeof value === "string" ? value === interfaceId
    : Array.isArray(value) ? value.some(references)
    : value !== null && typeof value === "object"
      ? Object.entries(value).some(([key, entry]) => key === interfaceId || references(entry)) : false;
  for (const entry of await fs.readdir(roots.runsRoot, { withFileTypes: true })) {
    if (!entry.name.endsWith(".json")) continue;
    const run = await readJson(path.join(roots.runsRoot, entry.name), "Run before private state removal");
    if (references(run.value)) throw new Error(`Interface '${interfaceId}' private state is still referenced by '${entry.name}'`);
  }
  return true;
}

async function finalizeClientAuthoringTransaction(roots, transactionRoot, journal) {
  await verifyJournalWrites(journal.writes);
  await verifyClientAuthoringRemovals(roots, transactionRoot, journal);
  const receipt = await readClientAuthoringReceipt(roots, journal.operation_id);
  if (!receipt) await writeClientAuthoringReceipt(roots, clientAuthoringReceipt(journal));
  else if (stableJson(receipt) !== stableJson(clientAuthoringReceipt(journal))) {
    throw new Error(`Client authoring receipt '${journal.operation_id}' conflicts with its committed journal`);
  }
  const cleanupRoot = path.join(roots.transactionsRoot, `.cleanup-${journal.operation_id}`);
  if (await pathExists(cleanupRoot)) {
    throw new Error(`Client authoring cleanup path already exists for '${journal.operation_id}'`);
  }
  await fs.rename(transactionRoot, cleanupRoot);
  await syncDirectory(roots.transactionsRoot);
  await cleanupCommittedClientAuthoringTransaction(roots, cleanupRoot);
}

async function cleanupCommittedClientAuthoringTransaction(roots, cleanupRoot) {
  if (!pathInside(roots.transactionsRoot, cleanupRoot)) {
    throw new Error("Client authoring cleanup escapes its transaction root");
  }
  const match = /^\.cleanup-([A-Za-z0-9_-]{8,96})$/.exec(path.basename(cleanupRoot));
  if (!match) throw new Error("Client authoring cleanup directory has an invalid name");
  let document;
  try {
    document = await readJson(
      path.join(cleanupRoot, "journal.json"),
      `Client authoring cleanup ${match[1]}`,
    );
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
    const entries = await fs.readdir(cleanupRoot);
    if (entries.length) throw new Error("Client authoring cleanup lost its journal before becoming empty");
    const receipt = await readClientAuthoringReceipt(roots, match[1]);
    if (!receipt) throw new Error("Client authoring cleanup lost both journal and durable receipt");
    await verifyClientAuthoringReceipt(roots, receipt);
    await fs.rmdir(cleanupRoot);
    await syncDirectory(roots.transactionsRoot);
    return;
  }
  const journal = parseClientAuthoringJournal(document.value);
  if (journal.operation_id !== match[1] || journal.phase !== "committed") {
    throw new Error("Client authoring cleanup does not prove a committed transaction");
  }
  const receipt = await readClientAuthoringReceipt(roots, journal.operation_id);
  if (!receipt || stableJson(receipt) !== stableJson(clientAuthoringReceipt(journal))) {
    throw new Error("Client authoring cleanup has no matching durable receipt");
  }
  await verifyJournalWrites(journal.writes);
  const declaredStagedFiles = new Set(journal.writes.flatMap((write) => (
    [write.after_file, write.before_file].filter(Boolean)
  )));
  for (const directory of ["after", "before"]) {
    const candidate = path.join(cleanupRoot, directory);
    const entries = await directoryEntriesIfPresent(candidate);
    if (entries === null) continue;
    for (const entry of entries) {
      const relative = path.join(directory, entry);
      if (!declaredStagedFiles.has(relative)) {
        throw new Error(`Client authoring cleanup contains unknown staged file '${relative}'`);
      }
    }
  }
  for (const write of journal.writes) {
    for (const [relative, digest] of [
      [write.after_file, write.after_digest],
      [write.before_file, write.before_digest],
    ]) {
      if (!relative) continue;
      const staged = transactionFile(cleanupRoot, relative, "cleanup staged file");
      const bytes = await readFileIfPresent(staged);
      if (!bytes) continue;
      if (sha256(bytes) !== digest) {
        throw new Error(`Client authoring cleanup staged bytes for '${write.tag}' changed`);
      }
      await fs.unlink(staged);
      await syncDirectory(path.dirname(staged));
    }
  }
  for (const removal of journal.removals) {
    if (await pathExists(removal.destination)) {
      throw new Error(`Client authoring cleanup '${removal.tag}' found its source restored`);
    }
    const tombstone = transactionFile(cleanupRoot, removal.tombstone, "cleanup tombstone");
    const durable = await inspectClientAuthoringRemoval(tombstone, removal.type);
    if (!durable.exists) continue;
    await removeCommittedClientAuthoringTombstone(tombstone, durable, removal,
      await isRetiredInterfaceStateRemoval(roots, journal, removal));
  }
  for (const directory of ["after", "before", "removed"]) {
    const candidate = path.join(cleanupRoot, directory);
    const entries = await directoryEntriesIfPresent(candidate);
    if (entries === null) continue;
    if (entries.length) {
      throw new Error(`Client authoring cleanup directory '${directory}' is not empty`);
    }
    await fs.rmdir(candidate);
    await syncDirectory(cleanupRoot);
  }
  await fs.unlink(path.join(cleanupRoot, "journal.json"));
  await syncDirectory(cleanupRoot);
  await fs.rmdir(cleanupRoot);
  await syncDirectory(roots.transactionsRoot);
}

async function removeCommittedClientAuthoringTombstone(tombstone, actual, removal, retiredState = false) {
  if (actual.dev !== removal.expected_dev || actual.ino !== removal.expected_ino) {
    throw new Error(`Client authoring cleanup '${removal.tag}' tombstone identity changed`);
  }
  const expected = new Map((retiredState ? actual.treeEntries : removal.tree_entries).map((entry) => [
    entry.relative_path,
    `${entry.type}\0${entry.digest ?? ""}`,
  ]));
  for (const entry of actual.treeEntries) {
    if (expected.get(entry.relative_path) !== `${entry.type}\0${entry.digest ?? ""}`) {
      throw new Error(`Client authoring cleanup '${removal.tag}' contains unknown or changed data`);
    }
  }
  const ordered = [...actual.treeEntries].sort((left, right) => (
    right.relative_path.split("/").length - left.relative_path.split("/").length
    || Number(left.type === "directory") - Number(right.type === "directory")
    || right.relative_path.localeCompare(left.relative_path)
  ));
  for (const entry of ordered) {
    const candidate = path.resolve(tombstone, ...entry.relative_path.split("/"));
    if (!pathInside(tombstone, candidate)) {
      throw new Error(`Client authoring cleanup '${removal.tag}' entry escapes its tombstone`);
    }
    await requireClientAuthoringRoot(path.dirname(candidate));
    const stat = await fs.lstat(candidate);
    const matchesType = entry.type === "symlink" ? stat.isSymbolicLink()
      : entry.type === "directory" ? stat.isDirectory() : stat.isFile();
    if (!matchesType) {
      throw new Error(`Client authoring cleanup '${removal.tag}' entry changed type`);
    }
    if (entry.type !== "directory") {
      const bytes = entry.type === "symlink"
        ? await fs.readlink(candidate, { encoding: "buffer" })
        : await readFileNoFollow(candidate);
      if (sha256(bytes) !== entry.digest) {
        throw new Error(`Client authoring cleanup '${removal.tag}' entry changed bytes`);
      }
      await fs.unlink(candidate);
    } else {
      await fs.rmdir(candidate);
    }
    await syncDirectory(path.dirname(candidate));
  }
  await fs.rmdir(tombstone);
  await syncDirectory(path.dirname(tombstone));
}

async function directoryEntriesIfPresent(directory) {
  try {
    const stat = await fs.lstat(directory);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`${directory} is not a plain directory`);
    return await fs.readdir(directory);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

async function applyJournalWrite(roots, transactionRoot, write) {
  const destinationRoot = allowedDestinationRoot(roots, write.destination);
  await ensurePlainParent(destinationRoot, path.dirname(write.destination));
  const current = await readFileIfPresent(write.destination);
  const currentDigest = current ? sha256(current) : null;
  if (currentDigest === write.after_digest) return;
  if (currentDigest !== write.before_digest) {
    throw new Error(`Workspace destination '${write.destination}' changed during commit`);
  }
  if (write.after_digest === null) {
    await fs.unlink(write.destination);
    await syncDirectory(path.dirname(write.destination));
    await verifyAppliedWrite(write);
    return;
  }
  const afterPath = transactionFile(transactionRoot, write.after_file, "after file");
  const after = await readFileNoFollow(afterPath);
  if (sha256(after) !== write.after_digest) throw new Error(`Workspace staged bytes for '${write.tag}' are corrupt`);
  if (write.before_digest === null) {
    try {
      await writeSyncedFile(write.destination, after, 0o600, "wx");
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
      const raced = await readFileIfPresent(write.destination);
      if (!raced || sha256(raced) !== write.after_digest) {
        throw new Error(`Workspace write '${write.tag}' raced with different bytes`);
      }
    }
    await syncFile(write.destination);
  } else {
    const temporary = path.join(path.dirname(write.destination), `.${path.basename(write.destination)}.workspace-${crypto.randomBytes(8).toString("hex")}.tmp`);
    try {
      await writeSyncedFile(temporary, after, 0o600, "wx");
      const observed = await readFileIfPresent(write.destination);
      if (!observed || sha256(observed) !== write.before_digest) {
        throw new Error(`Workspace write '${write.tag}' changed after staging`);
      }
      await fs.rename(temporary, write.destination);
    } finally {
      await fs.unlink(temporary).catch((error) => {
        if (error?.code !== "ENOENT") throw error;
      });
    }
  }
  await syncDirectory(path.dirname(write.destination));
  await verifyAppliedWrite(write);
}

async function rollbackTransaction(roots, transactionRoot) {
  let journal = await readJournal(roots, transactionRoot);
  journal.phase = "rolling_back";
  await writeJournal(transactionRoot, journal);
  for (const index of [...journal.writes.keys()].reverse()) {
    const write = journal.writes[index];
    const current = await readFileIfPresent(write.destination);
    const currentDigest = current ? sha256(current) : null;
    if (write.state === "unchanged") {
      if (currentDigest !== write.after_digest) throw new Error(`Workspace unchanged write '${write.tag}' drifted during rollback`);
      continue;
    }
    if (currentDigest === write.before_digest) {
      if (write.state !== "rolled_back") {
        journal.writes[index].state = "rolled_back";
        await writeJournal(transactionRoot, journal);
      }
      continue;
    }
    if (write.state === "rolled_back") {
      throw new Error(`Workspace rolled-back write '${write.tag}' changed after restoration`);
    }
    if (currentDigest !== write.after_digest) {
      throw new Error(`Workspace rollback found changed bytes at '${write.destination}'`);
    }
    if (write.before_digest === null) {
      await fs.unlink(write.destination);
    } else {
      const beforePath = transactionFile(transactionRoot, write.before_file, "before file");
      const before = await readFileNoFollow(beforePath);
      if (sha256(before) !== write.before_digest) throw new Error(`Workspace rollback snapshot for '${write.tag}' is corrupt`);
      const temporary = path.join(path.dirname(write.destination), `.${path.basename(write.destination)}.rollback-${crypto.randomBytes(8).toString("hex")}.tmp`);
      await writeSyncedFile(temporary, before, 0o600, "wx");
      await fs.rename(temporary, write.destination);
    }
    await syncDirectory(path.dirname(write.destination));
    journal.writes[index].state = "rolled_back";
    await writeJournal(transactionRoot, journal);
  }
  await fs.rm(transactionRoot, { recursive: true, force: true });
  await syncDirectory(roots.transactionsRoot);
}

async function recoverWorkspaceTransactionsUnlocked(roots) {
  const entries = await fs.readdir(roots.transactionsRoot, { withFileTypes: true });
  let recovered = 0;
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    if (entry.isDirectory() && /^\.stage-[A-Za-z0-9_-]{8,96}-[a-f0-9]{16}$/.test(entry.name)) {
      await fs.rm(path.join(roots.transactionsRoot, entry.name), { recursive: true, force: true });
      await syncDirectory(roots.transactionsRoot);
      continue;
    }
    if (entry.isDirectory() && /^\.cleanup-[A-Za-z0-9_-]{8,96}$/.test(entry.name)) {
      await cleanupCommittedClientAuthoringTransaction(
        roots,
        path.join(roots.transactionsRoot, entry.name),
      );
      continue;
    }
    if (!entry.isDirectory() || !SAFE_OPERATION_ID.test(entry.name)) {
      throw new Error(`Workspace transaction root contains an invalid entry '${entry.name}'`);
    }
    await recoverTransaction(roots, path.join(roots.transactionsRoot, entry.name));
    recovered += 1;
  }
  return { recovered };
}

async function recoverTransaction(roots, transactionRoot) {
  const journal = await readJournal(roots, transactionRoot);
  if (journal.schema === "octopus.client_authoring_transaction") {
    await recoverClientAuthoringTransaction(roots, transactionRoot);
    return;
  }
  if (journal.phase === "rolling_back") {
    await rollbackTransaction(roots, transactionRoot);
    return;
  }
  if (journal.phase === "committed") {
    await finalizeCommittedTransaction(roots, transactionRoot, journal);
    return;
  }
  await commitTransaction(roots, transactionRoot);
}

async function recoverClientAuthoringTransaction(roots, transactionRoot) {
  const journal = await readJournal(roots, transactionRoot);
  if (journal.schema !== "octopus.client_authoring_transaction") {
    throw new Error("Client authoring recovery received another transaction schema");
  }
  await commitClientAuthoringJournal(roots, transactionRoot);
}

async function finalizeCommittedTransaction(roots, transactionRoot, journal) {
  await verifyJournalWrites(journal.writes);
  const receipt = await readJournalReceipt(roots, journal);
  if (!receipt) await writeJournalReceipt(roots, journal);
  else if (stableJson(receipt) !== stableJson(journal.receipt)) {
    throw new Error(`Workspace receipt '${journal.operation_id}' conflicts with its committed journal`);
  }
  await fs.rm(transactionRoot, { recursive: true, force: true });
  await syncDirectory(roots.transactionsRoot);
}

async function readJournal(roots, transactionRoot) {
  if (!pathInside(roots.transactionsRoot, transactionRoot)) throw new Error("Workspace transaction escapes its state root");
  const file = path.join(transactionRoot, "journal.json");
  const document = await readJson(file, `Workspace transaction ${path.basename(transactionRoot)}`);
  const journal = parseJournal(document.value);
  if (journal.operation_id !== path.basename(transactionRoot)) {
    throw new Error("Workspace transaction directory disagrees with its operation id");
  }
  const stagedFiles = new Set();
  for (const write of journal.writes) {
    allowedDestinationRoot(roots, write.destination);
    if (write.after_file) {
      transactionFile(transactionRoot, write.after_file, "after file");
      if (stagedFiles.has(write.after_file)) throw new Error("Workspace transaction repeats a staged file");
      stagedFiles.add(write.after_file);
    }
    if (write.before_file) {
      transactionFile(transactionRoot, write.before_file, "before file");
      if (stagedFiles.has(write.before_file)) throw new Error("Workspace transaction repeats a staged file");
      stagedFiles.add(write.before_file);
    }
  }
  const destinations = [
    ...journal.writes.map((write) => write.destination),
    ...(journal.removals ?? []).map((removal) => removal.destination),
  ];
  for (let left = 0; left < destinations.length; left += 1) {
    for (let right = left + 1; right < destinations.length; right += 1) {
      if (
        destinations[left] === destinations[right]
        || pathInside(destinations[left], destinations[right])
        || pathInside(destinations[right], destinations[left])
      ) {
        throw new Error("Workspace transaction journal contains overlapping destinations");
      }
    }
  }
  for (const removal of journal.removals ?? []) {
    allowedDestinationRoot(roots, removal.destination);
    transactionFile(transactionRoot, removal.tombstone, "removal tombstone");
    if (stagedFiles.has(removal.tombstone)) throw new Error("Workspace transaction repeats a staged path");
    stagedFiles.add(removal.tombstone);
  }
  if (journal.schema === "octopus.workspace_setup_transaction") {
    const homePath = path.join(roots.interfaceRoot, journal.receipt.home_interface_id, "interface.json");
    const homeWrite = journal.writes.find((write) => write.destination === homePath);
    if (!homeWrite) throw new Error("Workspace setup transaction omits its Home descriptor");
    const homeFile = homeWrite.after_file
      ? transactionFile(transactionRoot, homeWrite.after_file, "Home after file") : homePath;
    const homeBytes = await readFileNoFollow(homeFile);
    if (sha256(homeBytes) !== homeWrite.after_digest) throw new Error("Workspace staged Home descriptor changed");
    assertSupportedHomeConfiguration(JSON.parse(homeBytes.toString("utf8")));
  }
  if (journal.schema === "octopus.workspace_project_transaction") {
    verifyProjectReceiptContract(roots, journal.receipt);
  }
  return journal;
}

function parseJournal(value) {
  if (value?.schema === "octopus.workspace_setup_transaction") {
    return parseSetupJournal(value);
  }
  if (value?.schema === "octopus.workspace_project_transaction") {
    return parseProjectJournal(value);
  }
  if (value?.schema === "octopus.client_authoring_transaction") {
    return parseClientAuthoringJournal(value);
  }
  throw new Error("Workspace transaction journal has an unsupported schema");
}

function parseClientAuthoringJournal(value) {
  const journal = exactObject(
    value,
    ["schema", "schema_version", "operation_id", "label", "phase", "writes", "removals"],
    ["schema", "schema_version", "operation_id", "label", "phase", "writes", "removals"],
    "Client authoring transaction journal",
  );
  if (journal.schema !== "octopus.client_authoring_transaction" || journal.schema_version !== 1) {
    throw new Error("Client authoring transaction journal has an unsupported schema");
  }
  if (!["staged", "committing", "published", "committed"].includes(journal.phase)) {
    throw new Error("Client authoring transaction journal has an invalid phase");
  }
  if (!Array.isArray(journal.writes) || !journal.writes.length || !Array.isArray(journal.removals)) {
    throw new Error("Client authoring transaction journal has invalid writes or removals");
  }
  const writes = journal.writes.map((write, index) => parseJournalWrite(write, index, true));
  if (writes.filter((write) => write.publish_barrier).length !== 1) {
    throw new Error("Client authoring transaction journal must have exactly one publish barrier");
  }
  const removals = journal.removals.map((removal, index) => parseClientAuthoringJournalRemoval(removal, index));
  const destinations = [...writes.map((write) => write.destination), ...removals.map((removal) => removal.destination)];
  if (new Set(destinations).size !== destinations.length) {
    throw new Error("Client authoring transaction journal repeats a destination");
  }
  return {
    schema: journal.schema,
    schema_version: 1,
    operation_id: requiredOperationId(journal.operation_id),
    label: requiredHumanText(journal.label, "Client authoring transaction label", 240),
    phase: journal.phase,
    writes,
    removals,
  };
}

function parseClientAuthoringJournalRemoval(value, index) {
  const removal = exactObject(
    value,
    [
      "destination", "type", "expected_dev", "expected_ino", "tree_digest", "tree_entries",
      "tombstone", "tag", "state",
    ],
    [
      "destination", "type", "expected_dev", "expected_ino", "tree_digest", "tree_entries",
      "tombstone", "tag", "state",
    ],
    `Client authoring journal removal ${index + 1}`,
  );
  if (removal.type !== "directory" || !["staged", "moved"].includes(removal.state)) {
    throw new Error(`Client authoring journal removal ${index + 1} has an invalid type or state`);
  }
  if (!Array.isArray(removal.tree_entries)) {
    throw new Error(`Client authoring journal removal ${index + 1} has no tree entries`);
  }
  const treeEntries = removal.tree_entries.map((value, entryIndex) => {
    const entry = exactObject(
      value,
      ["relative_path", "type", "digest"],
      ["relative_path", "type", "digest"],
      `Client authoring journal removal ${index + 1} tree entry ${entryIndex + 1}`,
    );
    const relativePath = requiredPortableRelativePath(
      entry.relative_path,
      `Client authoring journal removal ${index + 1} tree entry ${entryIndex + 1} path`,
    );
    if (!["directory", "file", "symlink"].includes(entry.type)) {
      throw new Error(`Client authoring journal removal ${index + 1} tree entry ${entryIndex + 1} has an invalid type`);
    }
    const digest = entry.type !== "directory"
      ? requiredDigest(
          entry.digest,
          `Client authoring journal removal ${index + 1} tree entry ${entryIndex + 1} digest`,
        )
      : entry.digest === null
        ? null
        : (() => { throw new Error(`Client authoring journal removal ${index + 1} directory entry has a digest`); })();
    return { relative_path: relativePath, type: entry.type, digest };
  });
  if (new Set(treeEntries.map((entry) => entry.relative_path)).size !== treeEntries.length) {
    throw new Error(`Client authoring journal removal ${index + 1} repeats a tree entry`);
  }
  return {
    destination: requiredAbsolutePath(removal.destination, `Client authoring journal removal ${index + 1} destination`),
    type: "directory",
    expected_dev: requiredFileIdentityNumber(removal.expected_dev, `Client authoring journal removal ${index + 1} device`),
    expected_ino: requiredFileIdentityNumber(removal.expected_ino, `Client authoring journal removal ${index + 1} inode`),
    tree_digest: requiredDigest(removal.tree_digest, `Client authoring journal removal ${index + 1} tree digest`),
    tree_entries: treeEntries,
    tombstone: optionalRelativeFile(removal.tombstone, `Client authoring journal removal ${index + 1} tombstone`),
    tag: requiredHumanText(removal.tag, `Client authoring journal removal ${index + 1} tag`, 240),
    state: removal.state,
  };
}

function parseSetupJournal(value) {
  const journal = exactObject(
    value,
    ["schema", "schema_version", "operation_id", "request_digest", "phase", "receipt", "writes"],
    ["schema", "schema_version", "operation_id", "request_digest", "phase", "receipt", "writes"],
    "Workspace transaction journal",
  );
  if (journal.schema !== "octopus.workspace_setup_transaction" || journal.schema_version !== 1) {
    throw new Error("Workspace transaction journal has an unsupported schema");
  }
  const phases = new Set(["staged", "committing", "published", "committed", "rolling_back"]);
  if (!phases.has(journal.phase)) throw new Error("Workspace transaction journal has an invalid phase");
  if (!Array.isArray(journal.writes) || !journal.writes.length) throw new Error("Workspace transaction journal has no writes");
  const writes = journal.writes.map((write, index) => parseJournalWrite(write, index, false));
  if (writes.filter((write) => write.publish_barrier).length !== 1) {
    throw new Error("Workspace transaction journal must have exactly one publish barrier");
  }
  const operationId = requiredOperationId(journal.operation_id);
  const requestDigest = requiredDigest(journal.request_digest, "Workspace transaction request digest");
  const receipt = parseReceipt(journal.receipt);
  if (receipt.operation_id !== operationId || receipt.request_digest !== requestDigest) {
    throw new Error("Workspace transaction receipt disagrees with its operation or request");
  }
  const receiptWrites = writes.map((write) => ({
    path: write.destination,
    digest: write.after_digest,
    tag: write.tag,
    publish_barrier: write.publish_barrier,
  }));
  if (stableJson(receipt.writes) !== stableJson(receiptWrites)) {
    throw new Error("Workspace transaction receipt disagrees with its journal writes");
  }
  if (["published", "committed"].includes(journal.phase) && !receipt.completed_at) {
    throw new Error("Published workspace transaction has no completion time");
  }
  return {
    schema: journal.schema,
    schema_version: 1,
    operation_id: operationId,
    request_digest: requestDigest,
    phase: journal.phase,
    receipt,
    writes,
  };
}

function parseProjectJournal(value) {
  const journal = exactObject(
    value,
    ["schema", "schema_version", "operation_id", "request_digest", "phase", "receipt", "writes"],
    ["schema", "schema_version", "operation_id", "request_digest", "phase", "receipt", "writes"],
    "Workspace Project transaction journal",
  );
  if (journal.schema !== "octopus.workspace_project_transaction" || journal.schema_version !== 1) {
    throw new Error("Workspace Project transaction journal has an unsupported schema");
  }
  const phases = new Set(["staged", "committing", "published", "committed", "rolling_back"]);
  if (!phases.has(journal.phase)) throw new Error("Workspace Project transaction journal has an invalid phase");
  if (!Array.isArray(journal.writes) || !journal.writes.length) {
    throw new Error("Workspace Project transaction journal has no writes");
  }
  const writes = journal.writes.map((write, index) => parseJournalWrite(write, index, true));
  if (writes.filter((write) => write.publish_barrier).length !== 1) {
    throw new Error("Workspace Project transaction journal must have exactly one publish barrier");
  }
  const operationId = requiredOperationId(journal.operation_id);
  const requestDigest = requiredDigest(journal.request_digest, "Workspace Project transaction request digest");
  const receipt = parseProjectReceipt(journal.receipt);
  if (receipt.operation_id !== operationId || receipt.request_digest !== requestDigest) {
    throw new Error("Workspace Project transaction receipt disagrees with its operation or request");
  }
  const receiptWrites = writes.map((write) => ({
    path: write.destination,
    digest: write.after_digest,
    tag: write.tag,
    publish_barrier: write.publish_barrier,
  }));
  if (stableJson(receipt.writes) !== stableJson(receiptWrites)) {
    throw new Error("Workspace Project transaction receipt disagrees with its journal writes");
  }
  if (["published", "committed"].includes(journal.phase) && !receipt.completed_at) {
    throw new Error("Published Workspace Project transaction has no completion time");
  }
  return {
    schema: journal.schema,
    schema_version: 1,
    operation_id: operationId,
    request_digest: requestDigest,
    phase: journal.phase,
    receipt,
    writes,
  };
}

function parseJournalWrite(value, index, allowDeletion) {
  const write = exactObject(
    value,
    [
      "destination", "expected_before_digest", "before_digest", "after_digest", "after_file",
      "before_file", "tag", "publish_barrier", "state",
    ],
    [
      "destination", "expected_before_digest", "before_digest", "after_digest", "after_file",
      "before_file", "tag", "publish_barrier", "state",
    ],
    `Workspace journal write ${index + 1}`,
  );
  const states = new Set(["unchanged", "staged", "applied", "rolled_back"]);
  if (!states.has(write.state)) throw new Error(`Workspace journal write ${index + 1} has an invalid state`);
  if (typeof write.publish_barrier !== "boolean") throw new Error(`Workspace journal write ${index + 1} has an invalid barrier`);
  const afterDigest = optionalDigest(write.after_digest, `Workspace journal write ${index + 1} after digest`);
  const afterFile = optionalRelativeFile(write.after_file, `Workspace journal write ${index + 1} after file`);
  if (!allowDeletion && afterDigest === null) {
    throw new Error(`Workspace journal write ${index + 1} cannot delete a setup file`);
  }
  if (write.state === "unchanged" && afterFile !== "") {
    throw new Error(`Workspace journal write ${index + 1} has staged bytes for an unchanged write`);
  }
  if (write.state !== "unchanged" && (afterDigest === null) !== (afterFile === "")) {
    throw new Error(`Workspace journal write ${index + 1} has inconsistent staged bytes`);
  }
  return {
    destination: requiredAbsolutePath(write.destination, `Workspace journal write ${index + 1} destination`),
    expected_before_digest: optionalDigest(write.expected_before_digest, `Workspace journal write ${index + 1} expected digest`),
    before_digest: optionalDigest(write.before_digest, `Workspace journal write ${index + 1} before digest`),
    after_digest: afterDigest,
    after_file: afterFile,
    before_file: optionalRelativeFile(write.before_file, `Workspace journal write ${index + 1} before file`),
    tag: requiredHumanText(write.tag, `Workspace journal write ${index + 1} tag`, 240),
    publish_barrier: write.publish_barrier,
    state: write.state,
  };
}

function parseReceipt(value) {
  const receipt = exactObject(
    value,
    [
      "schema", "schema_version", "operation_id", "request_digest", "completed_at", "root_ee_id",
      "home_run_id", "home_interface_id", "project_ee_ids", "writes", "existing_root",
    ],
    [
      "schema", "schema_version", "operation_id", "request_digest", "completed_at", "root_ee_id",
      "home_run_id", "home_interface_id", "project_ee_ids", "writes",
    ],
    "Workspace setup receipt",
  );
  if (receipt.schema !== "octopus.workspace_setup_receipt" || receipt.schema_version !== 1) {
    throw new Error("Workspace setup receipt has an unsupported schema");
  }
  if (receipt.completed_at && !Number.isFinite(Date.parse(receipt.completed_at))) {
    throw new Error("Workspace setup receipt has an invalid completion time");
  }
  if (!Array.isArray(receipt.project_ee_ids) || !Array.isArray(receipt.writes)) {
    throw new Error("Workspace setup receipt has invalid project/write evidence");
  }
  if (receipt.writes.filter((write) => write?.publish_barrier === true).length !== 1) {
    throw new Error("Workspace setup receipt must have exactly one publish barrier");
  }
  const projectEeIds = receipt.project_ee_ids.map((entry, index) => {
    const row = exactObject(entry, ["key", "ee_id"], ["key", "ee_id"], `Workspace receipt project ${index + 1}`);
    return { key: requiredId(row.key, `Workspace receipt project ${index + 1} key`), ee_id: requiredId(row.ee_id, `Workspace receipt project ${index + 1} EE`) };
  });
  if (
    new Set(projectEeIds.map((entry) => entry.key)).size !== projectEeIds.length
    || new Set(projectEeIds.map((entry) => entry.ee_id)).size !== projectEeIds.length
  ) {
    throw new Error("Workspace setup receipt repeats a project identity");
  }
  const writes = receipt.writes.map((entry, index) => {
    const row = exactObject(entry, ["path", "digest", "tag", "publish_barrier"], ["path", "digest", "tag", "publish_barrier"], `Workspace receipt write ${index + 1}`);
    if (typeof row.publish_barrier !== "boolean") throw new Error(`Workspace receipt write ${index + 1} has an invalid barrier`);
    return {
      path: requiredAbsolutePath(row.path, `Workspace receipt write ${index + 1} path`),
      digest: requiredDigest(row.digest, `Workspace receipt write ${index + 1} digest`),
      tag: requiredHumanText(row.tag, `Workspace receipt write ${index + 1} tag`, 240),
      publish_barrier: row.publish_barrier,
    };
  });
  if (new Set(writes.map((write) => write.path)).size !== writes.length) {
    throw new Error("Workspace setup receipt repeats a write path");
  }
  const existingRoot = receipt.existing_root === undefined ? null : parseExistingRootObservation(receipt.existing_root);
  if (existingRoot && (existingRoot.ee_id !== receipt.root_ee_id
    || writes.some((write) => write.path === existingRoot.path)
    || projectEeIds.some((project) => project.ee_id === existingRoot.ee_id))) {
    throw new Error("Workspace receipt must observe its existing root separately from created EEs and writes");
  }
  return {
    schema: receipt.schema,
    schema_version: 1,
    operation_id: requiredOperationId(receipt.operation_id),
    request_digest: requiredDigest(receipt.request_digest, "Workspace receipt request digest"),
    completed_at: receipt.completed_at,
    root_ee_id: requiredId(receipt.root_ee_id, "Workspace receipt root EE"),
    ...(existingRoot ? { existing_root: existingRoot } : {}),
    home_run_id: requiredId(receipt.home_run_id, "Workspace receipt Home run"),
    home_interface_id: requiredId(receipt.home_interface_id, "Workspace receipt Home interface"),
    project_ee_ids: projectEeIds,
    writes,
  };
}

function parseProjectReceipt(value) {
  const receipt = exactObject(
    value,
    [
      "schema", "schema_version", "operation_id", "request_digest", "completed_at", "action",
      "root_ee_id", "home_run_id", "home_interface_id", "outcome", "writes",
    ],
    [
      "schema", "schema_version", "operation_id", "request_digest", "completed_at", "action",
      "root_ee_id", "home_run_id", "home_interface_id", "outcome", "writes",
    ],
    "Workspace Project receipt",
  );
  if (receipt.schema !== "octopus.workspace_project_receipt" || receipt.schema_version !== 1) {
    throw new Error("Workspace Project receipt has an unsupported schema");
  }
  if (!["create_project", "delete_project", "update_project_presentation"].includes(receipt.action)) {
    throw new Error("Workspace Project receipt has an invalid action");
  }
  if (receipt.completed_at && !Number.isFinite(Date.parse(receipt.completed_at))) {
    throw new Error("Workspace Project receipt has an invalid completion time");
  }
  const expectedWriteCount = receipt.action === "update_project_presentation" ? 1 : 3;
  if (!Array.isArray(receipt.writes) || receipt.writes.length !== expectedWriteCount) {
    throw new Error(`Workspace Project receipt must prove exactly ${expectedWriteCount} write${expectedWriteCount === 1 ? "" : "s"}`);
  }
  const writes = receipt.writes.map((entry, index) => {
    const row = exactObject(
      entry,
      ["path", "digest", "tag", "publish_barrier"],
      ["path", "digest", "tag", "publish_barrier"],
      `Workspace Project receipt write ${index + 1}`,
    );
    if (typeof row.publish_barrier !== "boolean") {
      throw new Error(`Workspace Project receipt write ${index + 1} has an invalid barrier`);
    }
    return {
      path: requiredAbsolutePath(row.path, `Workspace Project receipt write ${index + 1} path`),
      digest: optionalDigest(row.digest, `Workspace Project receipt write ${index + 1} digest`),
      tag: requiredHumanText(row.tag, `Workspace Project receipt write ${index + 1} tag`, 240),
      publish_barrier: row.publish_barrier,
    };
  });
  if (new Set(writes.map((write) => write.path)).size !== writes.length) {
    throw new Error("Workspace Project receipt repeats a write path");
  }
  if (writes.filter((write) => write.publish_barrier).length !== 1) {
    throw new Error("Workspace Project receipt must have exactly one publish barrier");
  }
  const outcomeValue = receipt.action === "create_project"
    ? exactObject(
        receipt.outcome,
        ["ee_id", "parent_ee_id", "template_name", "goal"],
        ["ee_id", "parent_ee_id", "template_name", "goal"],
        "Workspace Project creation outcome",
      )
    : receipt.action === "delete_project"
      ? exactObject(receipt.outcome, ["ee_id"], ["ee_id"], "Workspace Project deletion outcome")
      : exactObject(
          receipt.outcome,
          ["project_ee_ids"],
          ["project_ee_ids"],
          "Workspace Project presentation outcome",
        );
  const outcome = receipt.action === "create_project"
    ? {
        ee_id: requiredId(outcomeValue.ee_id, "Workspace Project receipt EE"),
        parent_ee_id: requiredId(outcomeValue.parent_ee_id, "Workspace Project receipt parent EE"),
        template_name: requiredId(outcomeValue.template_name, "Workspace Project receipt template"),
        goal: requiredHumanText(outcomeValue.goal, "Workspace Project receipt goal", 240),
      }
    : receipt.action === "delete_project"
      ? { ee_id: requiredId(outcomeValue.ee_id, "Workspace Project receipt EE") }
      : {
          project_ee_ids: parseUniqueIds(
            outcomeValue.project_ee_ids,
            "Workspace Project presentation receipt EEs",
          ),
        };
  const barrier = writes.find((write) => write.publish_barrier);
  if (receipt.action === "create_project" && writes.some((write) => write.digest === null)) {
    throw new Error("Workspace Project creation receipt cannot contain a deletion");
  }
  if (receipt.action === "delete_project" && barrier?.digest !== null) {
    throw new Error("Workspace Project deletion receipt must publish by deleting the EE");
  }
  if (receipt.action === "update_project_presentation" && barrier?.digest === null) {
    throw new Error("Workspace Project presentation receipt cannot delete the Home sidecar");
  }
  return {
    schema: receipt.schema,
    schema_version: 1,
    operation_id: requiredOperationId(receipt.operation_id),
    request_digest: requiredDigest(receipt.request_digest, "Workspace Project receipt request digest"),
    completed_at: receipt.completed_at,
    action: receipt.action,
    root_ee_id: requiredId(receipt.root_ee_id, "Workspace Project receipt root EE"),
    home_run_id: requiredId(receipt.home_run_id, "Workspace Project receipt Home run"),
    home_interface_id: requiredId(receipt.home_interface_id, "Workspace Project receipt Home interface"),
    outcome,
    writes,
  };
}

async function writeJournal(transactionRoot, journal) {
  await writeJsonAtomic(path.join(transactionRoot, "journal.json"), journal, 0o600);
}

function clientAuthoringReceipt(journal) {
  return {
    schema: "octopus.client_authoring_receipt",
    schema_version: 1,
    operation_id: journal.operation_id,
    label: journal.label,
    writes: journal.writes.map((write) => ({
      destination: write.destination,
      digest: write.after_digest,
      publish_barrier: write.publish_barrier,
    })),
    removals: journal.removals.map((removal) => ({
      destination: removal.destination,
      tree_digest: removal.tree_digest,
      tag: removal.tag,
    })),
  };
}

function parseClientAuthoringReceipt(value) {
  const receipt = exactObject(
    value,
    ["schema", "schema_version", "operation_id", "label", "writes", "removals"],
    ["schema", "schema_version", "operation_id", "label", "writes", "removals"],
    "Client authoring receipt",
  );
  if (receipt.schema !== "octopus.client_authoring_receipt" || receipt.schema_version !== 1) {
    throw new Error("Client authoring receipt has an unsupported schema");
  }
  if (!Array.isArray(receipt.writes) || !receipt.writes.length || !Array.isArray(receipt.removals)) {
    throw new Error("Client authoring receipt has invalid writes or removals");
  }
  const writes = receipt.writes.map((value, index) => {
    const write = exactObject(
      value,
      ["destination", "digest", "publish_barrier"],
      ["destination", "digest", "publish_barrier"],
      `Client authoring receipt write ${index + 1}`,
    );
    if (typeof write.publish_barrier !== "boolean") {
      throw new Error(`Client authoring receipt write ${index + 1} has an invalid barrier`);
    }
    return {
      destination: requiredAbsolutePath(write.destination, `Client authoring receipt write ${index + 1} destination`),
      digest: optionalDigest(write.digest, `Client authoring receipt write ${index + 1} digest`),
      publish_barrier: write.publish_barrier,
    };
  });
  if (writes.filter((write) => write.publish_barrier).length !== 1) {
    throw new Error("Client authoring receipt must have exactly one publication barrier");
  }
  const removals = receipt.removals.map((value, index) => {
    const removal = exactObject(
      value,
      ["destination", "tree_digest", "tag"],
      ["destination", "tree_digest", "tag"],
      `Client authoring receipt removal ${index + 1}`,
    );
    return {
      destination: requiredAbsolutePath(
        removal.destination,
        `Client authoring receipt removal ${index + 1} destination`,
      ),
      tree_digest: requiredDigest(
        removal.tree_digest,
        `Client authoring receipt removal ${index + 1} tree digest`,
      ),
      tag: requiredHumanText(
        removal.tag,
        `Client authoring receipt removal ${index + 1} tag`,
        240,
      ),
    };
  });
  return {
    schema: receipt.schema,
    schema_version: 1,
    operation_id: requiredOperationId(receipt.operation_id),
    label: requiredHumanText(receipt.label, "Client authoring receipt label", 240),
    writes,
    removals,
  };
}

async function readClientAuthoringReceipt(roots, operationId) {
  const file = path.join(roots.clientAuthoringReceiptsRoot, `${requiredOperationId(operationId)}.json`);
  try {
    const document = await readJson(file, `Client authoring receipt ${operationId}`);
    const receipt = parseClientAuthoringReceipt(document.value);
    if (`${receipt.operation_id}.json` !== path.basename(file)) {
      throw new Error(`Client authoring receipt '${file}' has a mismatched file name`);
    }
    return receipt;
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

async function writeClientAuthoringReceipt(roots, receiptValue) {
  const receipt = parseClientAuthoringReceipt(receiptValue);
  const file = path.join(roots.clientAuthoringReceiptsRoot, `${receipt.operation_id}.json`);
  const existing = await readClientAuthoringReceipt(roots, receipt.operation_id);
  if (existing) {
    if (stableJson(existing) !== stableJson(receipt)) {
      throw new Error(`Client authoring receipt '${receipt.operation_id}' already differs`);
    }
    return;
  }
  await writeJsonAtomic(file, receipt, 0o600, "wx");
}

async function verifyClientAuthoringReceipt(roots, receipt) {
  for (const write of receipt.writes) {
    allowedDestinationRoot(roots, write.destination);
    const bytes = await readFileIfPresent(write.destination);
    const digest = bytes ? sha256(bytes) : null;
    if (digest !== write.digest) {
      throw new Error(`Client authoring receipt '${receipt.operation_id}' write drifted`);
    }
  }
  for (const removal of receipt.removals) {
    allowedDestinationRoot(roots, removal.destination);
    if (await pathExists(removal.destination)) {
      throw new Error(
        `Client authoring receipt '${receipt.operation_id}' removal '${removal.tag}' was restored`,
      );
    }
  }
}

async function readReceipt(roots, operationId) {
  const file = path.join(roots.receiptsRoot, `${requiredOperationId(operationId)}.json`);
  try {
    return await readReceiptFile(file);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

async function readReceiptFile(file) {
  const document = await readJson(file, `Workspace setup receipt ${path.basename(file)}`);
  const receipt = parseReceipt(document.value);
  if (`${receipt.operation_id}.json` !== path.basename(file)) {
    throw new Error(`Workspace setup receipt '${file}' has a mismatched file name`);
  }
  return receipt;
}

async function writeReceipt(roots, receiptValue) {
  const receipt = parseReceipt(receiptValue);
  if (!receipt.completed_at) throw new Error("Workspace setup receipt cannot be written before completion");
  const file = path.join(roots.receiptsRoot, `${receipt.operation_id}.json`);
  const existing = await readReceipt(roots, receipt.operation_id);
  if (existing) {
    if (stableJson(existing) !== stableJson(receipt)) throw new Error(`Workspace setup receipt '${receipt.operation_id}' already differs`);
    return;
  }
  await writeJsonAtomic(file, receipt, 0o600, "wx");
}

async function readProjectReceipt(roots, operationId) {
  const file = path.join(roots.projectReceiptsRoot, `${requiredOperationId(operationId)}.json`);
  try {
    const receipt = await readProjectReceiptFile(file);
    verifyProjectReceiptContract(roots, receipt);
    return receipt;
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

async function readProjectReceiptFile(file) {
  const document = await readJson(file, `Workspace Project receipt ${path.basename(file)}`);
  const receipt = parseProjectReceipt(document.value);
  if (`${receipt.operation_id}.json` !== path.basename(file)) {
    throw new Error(`Workspace Project receipt '${file}' has a mismatched file name`);
  }
  return receipt;
}

async function writeProjectReceipt(roots, receiptValue) {
  const receipt = parseProjectReceipt(receiptValue);
  verifyProjectReceiptContract(roots, receipt);
  if (!receipt.completed_at) throw new Error("Workspace Project receipt cannot be written before completion");
  const file = path.join(roots.projectReceiptsRoot, `${receipt.operation_id}.json`);
  const existing = await readProjectReceipt(roots, receipt.operation_id);
  if (existing) {
    if (stableJson(existing) !== stableJson(receipt)) {
      throw new Error(`Workspace Project receipt '${receipt.operation_id}' already differs`);
    }
    return;
  }
  await writeJsonAtomic(file, receipt, 0o600, "wx");
}

function verifyProjectReceiptContract(roots, receipt) {
  for (const write of receipt.writes) allowedDestinationRoot(roots, write.path);
  const sidecarPath = path.join(roots.interfaceRoot, receipt.home_interface_id, "octopus_os.json");
  const barrier = receipt.writes.find((write) => write.publish_barrier);
  if (receipt.action === "update_project_presentation") {
    if (receipt.writes[0].path !== sidecarPath || barrier?.path !== sidecarPath) {
      throw new Error("Workspace Project presentation receipt does not publish the Home sidecar");
    }
    return;
  }
  const eePath = path.join(roots.eeInstancesRoot, `${receipt.outcome.ee_id}.json`);
  const agentCatalogPath = clientAgentCatalogPath(roots.octopusHomeRoot);
  if (
    !receipt.writes.some((write) => write.path === sidecarPath)
    || !receipt.writes.some((write) => write.path === eePath)
    || !receipt.writes.some((write) => write.path === agentCatalogPath)
  ) {
    throw new Error("Workspace Project receipt does not prove its EE, Home sidecar, and Client Agent catalog");
  }
  const expectedBarrier = receipt.action === "create_project" ? sidecarPath : eePath;
  if (barrier?.path !== expectedBarrier) {
    throw new Error("Workspace Project receipt has the wrong publication barrier");
  }
}

async function readJournalReceipt(roots, journal) {
  return journal.schema === "octopus.workspace_project_transaction"
    ? readProjectReceipt(roots, journal.operation_id)
    : readReceipt(roots, journal.operation_id);
}

async function writeJournalReceipt(roots, journal) {
  if (journal.schema === "octopus.workspace_project_transaction") {
    await writeProjectReceipt(roots, journal.receipt);
    return;
  }
  await writeReceipt(roots, journal.receipt);
}

function assertSupportedHomeConfiguration(descriptor) {
  if (descriptor?.interface_driver_config?.private_config?.application_consent_authority !== undefined) {
    throw new Error("Home application_consent_authority selection is retired and requires explicit configuration repair");
  }
}

async function verifyWorkspaceReceiptIdentity(roots, receipt) {
  const writePaths = new Set();
  for (const write of receipt.writes) {
    allowedDestinationRoot(roots, write.path);
    if (writePaths.has(write.path)) throw new Error(`Workspace receipt repeats '${write.path}'`);
    writePaths.add(write.path);
  }
  const rootPath = path.join(roots.eeInstancesRoot, `${receipt.root_ee_id}.json`);
  const runPath = path.join(roots.runsRoot, `${receipt.home_run_id}.json`);
  const descriptorPath = path.join(roots.interfaceRoot, receipt.home_interface_id, "interface.json");
  const sidecarPath = path.join(roots.interfaceRoot, receipt.home_interface_id, "octopus_os.json");
  for (const requiredPath of [...(receipt.existing_root ? [] : [rootPath]), runPath, descriptorPath, sidecarPath]) {
    if (!writePaths.has(requiredPath)) {
      throw new Error(`Workspace receipt omits identity file '${requiredPath}'`);
    }
  }
  const barrier = receipt.writes.find((write) => write.publish_barrier);
  if (!barrier || barrier.path !== runPath) {
    throw new Error("Workspace receipt publish barrier is not the Home run");
  }
  if (receipt.existing_root) {
    await verifyExistingRootObservation(roots, receipt.existing_root, false);
  }
  const root = (await readJson(rootPath, "Workspace root EE")).value;
  if (root.ee_id !== receipt.root_ee_id || root.template_name !== (receipt.existing_root?.template_name ?? "existence")) {
    throw new Error("Workspace root EE identity is no longer valid");
  }
  if (root.parent_ee_id != null || (await readTemplate(roots, root.template_name)).bootstrapRoot !== true) {
    throw new Error("Workspace Home no longer belongs to the declared bootstrap-root EE");
  }
  const run = (await readJson(runPath, "Workspace Home run")).value;
  if (!runRecordValidator(run)) {
    throw new Error("Workspace Home run no longer satisfies the canonical run schema");
  }
  if (
    run.id !== receipt.home_run_id
    || run.ee !== receipt.root_ee_id
    || run.interface_state?.[receipt.home_interface_id]?.client_surface_role !== "home"
  ) {
    throw new Error("Workspace Home run identity is no longer valid");
  }
  const descriptor = (await readJson(descriptorPath, "Workspace Home interface")).value;
  assertSupportedHomeConfiguration(descriptor);
  if (descriptor.id !== receipt.home_interface_id) {
    throw new Error("Workspace Home interface identity is no longer valid");
  }
  const sidecar = (await readJson(sidecarPath, "Workspace Home sidecar")).value;
  if (
    sidecar.schema_version !== 1
    || sidecar.workspace !== "octopus_os"
    || sidecar.interface !== receipt.home_interface_id
    || sidecar.owner_interface !== receipt.home_interface_id
    || sidecar.component !== "client"
    || sidecar.surface_role !== "home"
  ) {
    throw new Error("Workspace Home sidecar identity is no longer valid");
  }
  const agentCatalog = await readClientAgentCatalog(roots.octopusHomeRoot);
  if (!agentCatalog.exists) throw new Error("Workspace Client Agent catalog is absent");
}

async function assertWorkspaceAbsent(roots, existingEeId) {
  const receiptEntries = await fs.readdir(roots.receiptsRoot, { withFileTypes: true });
  if (receiptEntries.length) {
    const names = receiptEntries.map((entry) => entry.name).sort().join(", ");
    throw new Error(`Workspace setup refuses an already configured receipt root: ${names}`);
  }
  const topology = await findWorkspaceTopology(roots);
  const unselectedSpaces = topology.operatorSpaceEeIds.filter((id) => id !== existingEeId);
  if (unselectedSpaces.length) throw new Error(`Workspace setup refuses existing operator space '${unselectedSpaces[0]}' without its requested receipt`);
  if (topology.homeRunIds.length) throw new Error(`Workspace setup refuses existing Home run '${topology.homeRunIds[0]}' without its requested receipt`);
  if (topology.homeInterfaceIds.length) throw new Error(`Workspace setup refuses existing Home interface '${topology.homeInterfaceIds[0]}' without its requested receipt`);
}

async function findWorkspaceTopology(roots) {
  const operatorSpaceEeIds = [];
  for (const entry of await fs.readdir(roots.eeInstancesRoot, { withFileTypes: true })) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const file = path.join(roots.eeInstancesRoot, entry.name);
    const record = (await readJson(file, `EE instance ${entry.name}`)).value;
    if ((record.template_name ?? record.template) !== "operator_space") continue;
    const eeId = requiredId(record.ee_id, "Workspace operator-space EE");
    if (`${eeId}.json` !== entry.name) throw new Error(`Workspace operator-space EE '${eeId}' has a mismatched file name`);
    operatorSpaceEeIds.push(eeId);
  }

  const homeRunIds = [];
  for (const entry of await fs.readdir(roots.runsRoot, { withFileTypes: true })) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const file = path.join(roots.runsRoot, entry.name);
    const record = (await readJson(file, `Run record ${entry.name}`)).value;
    const hasHome = Object.values(record.interface_state ?? {}).some((state) => (
      state && typeof state === "object" && !Array.isArray(state) && state.client_surface_role === "home"
    ));
    if (!hasHome) continue;
    const runId = requiredId(record.id, "Workspace Home run");
    if (`${runId}.json` !== entry.name) throw new Error(`Workspace Home run '${runId}' has a mismatched file name`);
    homeRunIds.push(runId);
  }

  const interfaces = await fs.readdir(roots.interfaceRoot, { withFileTypes: true });
  const homeInterfaceIds = [];
  for (const entry of interfaces) {
    if (!entry.isDirectory()) continue;
    const file = path.join(roots.interfaceRoot, entry.name, "octopus_os.json");
    const bytes = await readFileIfPresent(file);
    if (!bytes) continue;
    let sidecar;
    try {
      sidecar = JSON.parse(bytes.toString("utf8"));
    } catch (error) {
      throw new Error(`Existing Octopus OS sidecar '${file}' is malformed: ${errorMessage(error)}`);
    }
    if (
      sidecar?.workspace === "octopus_os"
      && sidecar?.component === "client"
      && sidecar?.surface_role === "home"
    ) {
      homeInterfaceIds.push(requiredId(entry.name, "Workspace Home interface folder"));
    }
  }
  return {
    operatorSpaceEeIds: operatorSpaceEeIds.sort(),
    homeRunIds: homeRunIds.sort(),
    homeInterfaceIds: homeInterfaceIds.sort(),
  };
}

async function verifyJournalWrites(writes) {
  for (const write of writes) await verifyAppliedWrite(write);
}

async function verifyAppliedWrite(write) {
  const bytes = await readFileIfPresent(write.destination);
  if (write.after_digest === null) {
    if (bytes) throw new Error(`Workspace write '${write.tag}' was not deleted`);
    return;
  }
  if (!bytes || sha256(bytes) !== write.after_digest) {
    throw new Error(`Workspace write '${write.tag}' is not materialized`);
  }
}

function receiptOutcome(receipt, changed) {
  return {
    action: "initialize_workspace",
    operationId: receipt.operation_id,
    rootEeId: receipt.root_ee_id,
    homeRunId: receipt.home_run_id,
    homeInterfaceId: receipt.home_interface_id,
    projects: receipt.project_ee_ids.map((project) => ({ key: project.key, eeId: project.ee_id })),
    receiptDigest: sha256(Buffer.from(stableJson(receipt))),
    changed,
  };
}

function projectReceiptOutcome(receipt, changed) {
  const projectOutcome = receipt.action === "create_project"
    ? {
        eeId: receipt.outcome.ee_id,
        parentEeId: receipt.outcome.parent_ee_id,
        templateName: receipt.outcome.template_name,
        goal: receipt.outcome.goal,
      }
    : receipt.action === "delete_project"
      ? { eeId: receipt.outcome.ee_id }
      : { projectEeIds: receipt.outcome.project_ee_ids };
  return {
    action: receipt.action,
    operationId: receipt.operation_id,
    rootEeId: receipt.root_ee_id,
    homeRunId: receipt.home_run_id,
    homeInterfaceId: receipt.home_interface_id,
    ...projectOutcome,
    receiptDigest: sha256(Buffer.from(stableJson(receipt))),
    changed,
  };
}

function allowedDestinationRoot(roots, candidate) {
  if (candidate === path.join(roots.octopusHomeRoot, "host.json")) return roots.octopusHomeRoot;
  for (const root of [
    roots.eeInstancesRoot,
    roots.runsRoot,
    roots.interfaceRoot,
    roots.toolRoot,
    roots.artifactRoot,
    roots.eeTemplateRoot,
    roots.privateInterfacesRoot,
    roots.interfaceHandInRoot,
    roots.clientAgentsRoot,
  ]) {
    if (pathInside(root, candidate)) return root;
  }
  throw new Error(`Workspace destination '${candidate}' escapes the authoring roots`);
}

async function validateExistingPath(root, candidate) {
  await requireClientAuthoringRoot(root);
  const relative = path.relative(root, candidate);
  const segments = relative.split(path.sep).slice(0, -1);
  let cursor = root;
  for (const segment of segments) {
    cursor = path.join(cursor, segment);
    try {
      const stat = await fs.lstat(cursor);
      if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`Workspace path '${cursor}' is not a plain directory`);
    } catch (error) {
      if (error?.code === "ENOENT") return;
      throw error;
    }
  }
  try {
    const stat = await fs.lstat(candidate);
    if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`Workspace destination '${candidate}' is not a plain file`);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
}

async function validateExistingRemovalPath(root, candidate, type) {
  await requireClientAuthoringRoot(root);
  const relative = path.relative(root, candidate);
  if (!relative || relative === "." || relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) {
    throw new Error(`Client authoring removal '${candidate}' escapes '${root}'`);
  }
  let cursor = root;
  for (const segment of relative.split(path.sep).slice(0, -1)) {
    cursor = path.join(cursor, segment);
    const stat = await fs.lstat(cursor);
    if (!stat.isDirectory() || stat.isSymbolicLink()) {
      throw new Error(`Client authoring removal parent '${cursor}' is not a plain directory`);
    }
  }
  const stat = await fs.lstat(candidate);
  if (stat.isSymbolicLink() || (type === "directory" ? !stat.isDirectory() : !stat.isFile())) {
    throw new Error(`Client authoring removal '${candidate}' has an invalid type`);
  }
}

async function requireClientAuthoringRoot(root) {
  const stat = await fs.lstat(root);
  if (!stat.isDirectory() || stat.isSymbolicLink()) {
    throw new Error(`Client authoring root '${root}' is not a plain directory`);
  }
  if (await fs.realpath(root) !== root) {
    throw new Error(`Client authoring root '${root}' is not canonical`);
  }
}

async function snapshotClientAuthoringRemoval(removal) {
  const inspected = await inspectClientAuthoringRemoval(removal.destination, removal.type);
  if (!inspected.exists) throw new Error(`Client authoring removal '${removal.tag}' is absent`);
  if (inspected.dev !== removal.expectedDev || inspected.ino !== removal.expectedIno) {
    throw new Error(`Client authoring removal '${removal.tag}' changed before staging`);
  }
  return { treeDigest: inspected.treeDigest, treeEntries: inspected.treeEntries };
}

async function inspectClientAuthoringRemoval(candidate, type) {
  let stat;
  try {
    stat = await fs.lstat(candidate);
  } catch (error) {
    if (error?.code === "ENOENT") return { exists: false };
    throw error;
  }
  if (stat.isSymbolicLink() || (type === "directory" ? !stat.isDirectory() : !stat.isFile())) {
    throw new Error(`Client authoring removal '${candidate}' has an invalid type`);
  }
  return {
    exists: true,
    dev: stat.dev,
    ino: stat.ino,
    ...(await clientAuthoringTreeSnapshot(candidate, type)),
  };
}

function assertClientAuthoringRemovalIdentity(actual, removal, location, retiredState = false) {
  if (
    !actual.exists
    || actual.dev !== removal.expected_dev
    || actual.ino !== removal.expected_ino
    || (!retiredState && actual.treeDigest !== removal.tree_digest)
  ) {
    throw new Error(`Client authoring removal '${removal.tag}' ${location} identity changed`);
  }
}

async function clientAuthoringTreeSnapshot(candidate, type) {
  if (type === "file") {
    const digest = sha256(await readFileNoFollow(candidate));
    return { treeDigest: digest, treeEntries: [{ relative_path: path.basename(candidate), type: "file", digest }] };
  }
  const entries = [];
  await collectClientAuthoringTree(candidate, "", entries);
  return {
    treeDigest: sha256(Buffer.from(stableJson(entries))),
    treeEntries: entries,
  };
}

async function collectClientAuthoringTree(root, relative, rows) {
  const directory = relative ? path.join(root, relative) : root;
  await requireClientAuthoringRoot(directory);
  const entries = await fs.readdir(directory, { withFileTypes: true });
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    const childRelative = relative ? `${relative}/${entry.name}` : entry.name;
    const child = path.join(root, childRelative);
    const stat = await fs.lstat(child);
    if (stat.isSymbolicLink()) {
      rows.push({
        relative_path: childRelative,
        type: "symlink",
        digest: sha256(await fs.readlink(child, { encoding: "buffer" })),
      });
      continue;
    }
    if (stat.isDirectory()) {
      rows.push({ relative_path: childRelative, type: "directory", digest: null });
      await collectClientAuthoringTree(root, childRelative, rows);
      continue;
    }
    if (!stat.isFile()) throw new Error(`Client authoring removal contains a non-file entry '${child}'`);
    rows.push({
      relative_path: childRelative,
      type: "file",
      digest: sha256(await readFileNoFollow(child)),
    });
  }
}

async function pathExists(candidate) {
  try {
    await fs.lstat(candidate);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

async function ensurePlainParent(root, parent) {
  const relative = path.relative(root, parent);
  if (relative === "" || relative === ".") return;
  if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) {
    throw new Error(`Workspace parent '${parent}' escapes '${root}'`);
  }
  let cursor = root;
  for (const segment of relative.split(path.sep)) {
    const next = path.join(cursor, segment);
    try {
      const stat = await fs.lstat(next);
      if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`Workspace parent '${next}' is not a plain directory`);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
      await fs.mkdir(next, { mode: 0o700 });
      await syncDirectory(cursor);
    }
    cursor = next;
  }
}

function parseDoorHostSlots(value) {
  if (value === undefined) return {};
  const record = exactObject(value, Object.keys(value ?? {}), [], "Home folder access");
  const output = {};
  for (const [interfaceId, slotsValue] of Object.entries(record)) {
    const id = requiredId(interfaceId, "Home folder-access interface");
    const slots = exactObject(slotsValue, Object.keys(slotsValue ?? {}), [], `Home folder access ${id}`);
    output[id] = {};
    for (const [slot, root] of Object.entries(slots)) {
      output[id][requiredId(slot, `Home folder access ${id} slot`)] = requiredAbsolutePath(root, `Home folder access ${id} root`);
    }
  }
  return output;
}

function projectProfileExternalInputs(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error("Project authoring external inputs must be an array");
  return value.map((entry, index) => {
    const route = exactObject(
      entry,
      ["checkpoint_id", "interface_id", "required_verified_artifacts"],
      ["checkpoint_id", "interface_id"],
      `Project authoring external input ${index + 1}`,
    );
    const requirements = projectProfileRequiredVerifiedArtifacts(
      route.required_verified_artifacts,
      `Project authoring external input ${index + 1}`,
    );
    return {
      checkpointId: requiredId(route.checkpoint_id, `Project authoring external input ${index + 1} checkpoint`),
      interfaceId: requiredId(route.interface_id, `Project authoring external input ${index + 1} interface`),
      ...(requirements.length ? { requiredVerifiedArtifacts: requirements } : {}),
    };
  });
}

function projectProfileExternalRelays(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error("Project authoring external relays must be an array");
  return value.map((entry, index) => {
    const route = exactObject(
      entry,
      ["checkpoint_id", "interface_id", "outbound_interface_ids", "required_verified_artifacts"],
      ["checkpoint_id", "interface_id", "outbound_interface_ids"],
      `Project authoring external relay ${index + 1}`,
    );
    const requirements = projectProfileRequiredVerifiedArtifacts(
      route.required_verified_artifacts,
      `Project authoring external relay ${index + 1}`,
    );
    return {
      checkpointId: requiredId(route.checkpoint_id, `Project authoring external relay ${index + 1} checkpoint`),
      interfaceId: requiredId(route.interface_id, `Project authoring external relay ${index + 1} interface`),
      outboundInterfaceIds: parseUniqueIds(
        route.outbound_interface_ids,
        `Project authoring external relay ${index + 1} outbound interfaces`,
      ),
      ...(requirements.length ? { requiredVerifiedArtifacts: requirements } : {}),
    };
  });
}

function projectProfileRequiredVerifiedArtifacts(value, owner) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`${owner} requirements must be an array`);
  const labels = new Set();
  return value.map((entry, index) => {
    const requirement = exactObject(
      entry,
      ["mark", "artifact_label", "verifier", "params"],
      ["mark", "artifact_label", "verifier", "params"],
      `${owner} requirement ${index + 1}`,
    );
    if (!requirement.params || typeof requirement.params !== "object" || Array.isArray(requirement.params)) {
      throw new Error(`${owner} requirement ${index + 1} params must be an object`);
    }
    const artifactLabel = requiredId(
      requirement.artifact_label,
      `${owner} requirement ${index + 1} artifact label`,
    );
    if (labels.has(artifactLabel)) {
      throw new Error(`${owner} repeats required artifact label '${artifactLabel}'`);
    }
    labels.add(artifactLabel);
    return {
      mark: requiredId(requirement.mark, `${owner} requirement ${index + 1} mark`),
      artifactLabel,
      verifier: requiredId(requirement.verifier, `${owner} requirement ${index + 1} verifier`),
      params: structuredClone(requirement.params),
    };
  });
}

function projectProfileModelOutputs(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error("Project authoring model outputs must be an array");
  return value.map((entry, index) => {
    const route = exactObject(
      entry,
      ["checkpoint_id", "interface_id", "required_verified_artifact"],
      ["checkpoint_id", "interface_id", "required_verified_artifact"],
      `Project authoring model output ${index + 1}`,
    );
    const requirement = exactObject(
      route.required_verified_artifact,
      ["mark", "artifact_label", "verifier", "params"],
      ["mark", "artifact_label", "verifier", "params"],
      `Project authoring model output ${index + 1} requirement`,
    );
    if (!requirement.params || typeof requirement.params !== "object" || Array.isArray(requirement.params)) {
      throw new Error(`Project authoring model output ${index + 1} params must be an object`);
    }
    return {
      checkpointId: requiredId(route.checkpoint_id, `Project authoring model output ${index + 1} checkpoint`),
      interfaceId: requiredId(route.interface_id, `Project authoring model output ${index + 1} interface`),
      requiredVerifiedArtifact: {
        mark: requiredId(requirement.mark, `Project authoring model output ${index + 1} mark`),
        artifactLabel: requiredId(requirement.artifact_label, `Project authoring model output ${index + 1} artifact label`),
        verifier: requiredId(requirement.verifier, `Project authoring model output ${index + 1} verifier`),
        params: structuredClone(requirement.params),
      },
    };
  });
}

function verifyProjectTaskPreflight(value, expectedProfile, eeId) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Project task topology planner returned a malformed plan");
  }
  if (value.clientSurfaceRole !== "task") {
    throw new Error("Project task topology planner returned a non-task surface");
  }
  if (stableJson(value.authoringProfile) !== stableJson(expectedProfile)) {
    throw new Error(`Project EE '${eeId}' cannot reproduce its authored task profile`);
  }
  if (!Array.isArray(value.writes) || value.writes.length === 0) {
    throw new Error(`Project EE '${eeId}' produced no task topology writes`);
  }
  for (const [index, write] of value.writes.entries()) {
    if (!write || typeof write !== "object" || write.expectedBeforeHash !== null) {
      throw new Error(`Project EE '${eeId}' task preflight write ${index + 1} is not fresh`);
    }
  }
}

function parseArtifactBindings(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error("Home context bindings must be an array");
  const ids = new Set();
  return value.map((entry, index) => {
    const binding = exactObject(entry, ["id"], ["id"], `Home context binding ${index + 1}`);
    const id = requiredId(binding.id, `Home context binding ${index + 1} id`);
    if (ids.has(id)) throw new Error(`Home context bindings repeat '${id}'`);
    ids.add(id);
    return { id };
  });
}

function parseUniqueIds(value, label, optional = false) {
  if (value === undefined && optional) return [];
  if (!Array.isArray(value)) throw new Error(`${label} must be an array`);
  const ids = value.map((entry, index) => requiredId(entry, `${label} ${index + 1}`));
  if (new Set(ids).size !== ids.length) throw new Error(`${label} repeats an id`);
  return ids;
}

function parseSelectedTools(value) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error("Project authoring selected tools must be an array");
  const ids = new Set();
  return value.map((entry, index) => {
    const selection = exactObject(
      entry,
      ["tool", "interface", "worker_environment_kind"],
      ["tool", "interface", "worker_environment_kind"],
      `Project authoring selected tool ${index + 1}`,
    );
    const tool = requiredContractToken(selection.tool, `Project authoring selected tool ${index + 1}`);
    if (ids.has(tool)) throw new Error(`Project authoring selected tools repeat '${tool}'`);
    ids.add(tool);
    return {
      tool,
      interface: requiredId(selection.interface, `Project authoring selected tool ${index + 1} interface`),
      workerEnvironmentKind: requiredId(
        selection.worker_environment_kind,
        `Project authoring selected tool ${index + 1} worker environment kind`,
      ),
    };
  });
}

function parseSortedUniqueIds(value, label, allowEmpty = false) {
  const ids = parseUniqueIds(value, label);
  if (!ids.length && !allowEmpty) throw new Error(`${label} must not be empty`);
  const sorted = [...ids].sort();
  if (ids.some((id, index) => id !== sorted[index])) {
    throw new Error(`${label} must be sorted`);
  }
  return ids;
}

function exactObject(value, allowed, required, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object`);
  const unknown = Object.keys(value).filter((key) => !allowed.includes(key));
  if (unknown.length) throw new Error(`${label} contains unknown fields: ${unknown.join(", ")}`);
  const missing = required.filter((key) => !(key in value));
  if (missing.length) throw new Error(`${label} is missing fields: ${missing.join(", ")}`);
  return value;
}

function requiredOperationId(value) {
  if (typeof value !== "string" || !SAFE_OPERATION_ID.test(value)) {
    throw new Error("Workspace setup operation id is invalid");
  }
  return value;
}

function requiredId(value, label) {
  if (typeof value !== "string" || !SAFE_ID.test(value)) throw new Error(`${label} is invalid`);
  return value;
}

function requiredContractToken(value, label) {
  if (typeof value !== "string" || value.length > 160 || !SAFE_ID.test(value)) {
    throw new Error(`${label} is invalid`);
  }
  return value;
}

function requiredFileIdentityNumber(value, label) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${label} must be a non-negative safe integer`);
  return value;
}

function requiredHumanText(value, label, maxLength) {
  if (typeof value !== "string" || value.trim() !== value || !value || value.length > maxLength) {
    throw new Error(`${label} must be non-empty trimmed text up to ${maxLength} characters`);
  }
  return value;
}

function optionalText(value, label, maxLength) {
  if (value === undefined || value === null || value === "") return "";
  return requiredHumanText(value, label, maxLength);
}

function requiredAbsolutePath(value, label) {
  if (typeof value !== "string" || !path.isAbsolute(value)) throw new Error(`${label} must be an absolute path`);
  return path.resolve(value);
}

function requiredDigest(value, label) {
  if (typeof value !== "string" || !DIGEST.test(value)) throw new Error(`${label} must be a lowercase SHA-256 digest`);
  return value;
}

function optionalDigest(value, label) {
  if (value === null) return null;
  return requiredDigest(value, label);
}

function optionalRelativeFile(value, label) {
  if (value === "") return "";
  if (
    typeof value !== "string"
    || path.isAbsolute(value)
    || value.includes("\0")
    || value.split(/[\\/]/).includes("..")
  ) {
    throw new Error(`${label} must be a confined relative path`);
  }
  return value;
}

function requiredPortableRelativePath(value, label) {
  if (
    typeof value !== "string"
    || !value
    || value.startsWith("/")
    || value.startsWith("\\")
    || value.includes("\0")
    || value.includes("\\")
    || value.split("/").some((segment) => !segment || segment === "." || segment === "..")
  ) {
    throw new Error(`${label} must be a confined portable relative path`);
  }
  return value;
}

function deterministicId(prefix, operationId, key) {
  return `${prefix}_${sha256(Buffer.from(`${operationId}\u0000${key}`)).slice(0, 24)}`;
}

function requiredMapValue(values, key) {
  const value = values.get(key);
  if (!value) throw new Error(`Workspace project key '${key}' could not be materialized`);
  return value;
}

function transactionFile(transactionRoot, relative, label) {
  if (!relative) throw new Error(`Workspace transaction ${label} is missing`);
  const candidate = path.resolve(transactionRoot, relative);
  if (!pathInside(transactionRoot, candidate)) throw new Error(`Workspace transaction ${label} escapes its operation root`);
  return candidate;
}

async function readJson(file, label) {
  const body = await readFileNoFollow(file);
  let value;
  try {
    value = JSON.parse(body.toString("utf8"));
  } catch (error) {
    throw new Error(`${label} is malformed JSON: ${errorMessage(error)}`);
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be a JSON object`);
  return { value, digest: sha256(body) };
}

async function readFileIfPresent(file) {
  try {
    return await readFileNoFollow(file);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

async function readFileNoFollow(file) {
  const flags = fsSync.constants.O_RDONLY | fsSync.constants.O_NOFOLLOW;
  const handle = await fs.open(file, flags);
  try {
    const stat = await handle.stat();
    if (!stat.isFile()) throw new Error(`${file} is not a plain file`);
    return await handle.readFile();
  } finally {
    await handle.close();
  }
}

async function writeJsonAtomic(file, value, mode, flag) {
  const body = Buffer.from(`${JSON.stringify(value, null, 2)}\n`);
  const temporary = `${file}.${process.pid}-${crypto.randomBytes(8).toString("hex")}.tmp`;
  try {
    await writeSyncedFile(temporary, body, mode, "wx");
    if (flag === "wx") {
      await fs.link(temporary, file);
      await fs.unlink(temporary);
    } else {
      await fs.rename(temporary, file);
    }
    await syncDirectory(path.dirname(file));
  } finally {
    await fs.unlink(temporary).catch((error) => {
      if (error?.code !== "ENOENT") throw error;
    });
  }
}

async function writeSyncedFile(file, body, mode, flag) {
  const handle = await fs.open(file, flag, mode);
  try {
    await handle.writeFile(body);
    await handle.sync();
  } finally {
    await handle.close();
  }
}

async function syncFile(file) {
  const handle = await fs.open(file, fsSync.constants.O_RDONLY | fsSync.constants.O_NOFOLLOW);
  try {
    await handle.sync();
  } finally {
    await handle.close();
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

async function realPlainDirectory(directory, label) {
  const stat = await fs.lstat(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`${label} is not a plain directory`);
  const resolved = await fs.realpath(directory);
  const resolvedStat = await fs.lstat(resolved);
  if (!resolvedStat.isDirectory() || resolvedStat.isSymbolicLink()) throw new Error(`${label} does not resolve to a plain directory`);
  return resolved;
}

async function directoryExists(directory) {
  try {
    const stat = await fs.lstat(directory);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`${directory} is not a plain directory`);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

function pathInside(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative !== "" && relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

function reportProgress(listener, operationId, phase, percent, label) {
  if (typeof listener !== "function") return;
  try {
    listener({ operationId, phase, percent: Math.max(0, Math.min(100, percent)), label });
  } catch {}
}

function withTransactionLock(stateRoot, operation) {
  const key = path.resolve(stateRoot);
  const previous = transactionTails.get(key) ?? Promise.resolve();
  let release;
  const current = new Promise((resolve) => { release = resolve; });
  const tail = previous.then(() => current);
  transactionTails.set(key, tail);
  return previous.then(operation).finally(() => {
    release();
    if (transactionTails.get(key) === tail) transactionTails.delete(key);
  });
}

module.exports = {
  commitClientAuthoringTransaction,
  initializeWorkspace,
  mutateWorkspaceProject,
  readWorkspaceSetupState,
  recoverWorkspaceTransactions,
};
