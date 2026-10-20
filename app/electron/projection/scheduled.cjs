module.exports = { applyScheduledDraft, armSetupTriggers, cloneCronRunTemplate, collectScheduledRunInterfaces, createScheduledInterface, deleteScheduledTopology, importScheduledLife, mutateScheduledInterface, privatizeScheduledActivation, readScheduledInterfaceCloneSource, remapScheduledValue, scheduledSiblingDescriptor, setupTriggerEntries, setupTriggerProof, setupTriggerState, validateScheduledInterfaceDescriptor, withScheduleEditLock };

const { parseScheduledDueAt } = require("../scheduledTime.cjs");
const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const path = require("node:path");
const { undoChangeJournal } = require("./changeJournal.cjs");
const { scheduleEditTails } = require("./constants.cjs");
const { activationDriverRef, parseCronActivationDocument, validateCronExpression, validateCronSchedule } = require("./cronActivation.cjs");
const { readOptionalRegularJsonDocument, readRegularJsonDocument, requirePlainDirectory, requireUnchangedJsonDigest } = require("./jsonFiles.cjs");
const { snapshotRemovalDirectory } = require("./taskDeletion.cjs");
const { arrayValue, atomicWriteJson, exactObjectKeys, interfaceBindingId, objectOrEmpty, requiredDigest, requiredSafeRegistryId, requiredSafeToolId, stringValue } = require("./values.cjs");

async function mutateScheduledInterface(registryRoot, octopusHomeRoot, request, hooks = {}) {
  const action = stringValue(request?.action);
  if (!["create", "update", "set_enabled", "delete", "undo_changes"].includes(action)) throw new Error("scheduled mutation action is invalid");
  if (action === "undo_changes") return undoChangeJournal(registryRoot, octopusHomeRoot, request, hooks.commitClientAuthoring);
  const interfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  if (action === "create") {
    return createScheduledInterface(
      interfaceRoot,
      octopusHomeRoot,
      request,
      hooks.commitClientAuthoring,
    );
  }
  const interfaceId = requiredSafeRegistryId(request?.interfaceId, "interfaceId");
  const interfaceDirectory = path.join(interfaceRoot, interfaceId);
  await requirePlainDirectory(interfaceDirectory, "scheduled interface directory");
  return withScheduleEditLock(interfaceDirectory, async () => {
    const activationPath = path.join(interfaceDirectory, "activation.json");
    const { document, digest } = await readRegularJsonDocument(activationPath, "activation document");
    const expectedDigest = requiredDigest(request?.expectedDigest, "expectedDigest");
    if (digest !== expectedDigest) throw new Error("scheduled interface changed before the edit could be applied");
    const parsed = parseCronActivationDocument(document, interfaceId);
    if (action === "delete") {
      await requireUnchangedJsonDigest(activationPath, digest, "activation document");
      return deleteScheduledTopology(
        interfaceRoot,
        octopusHomeRoot,
        interfaceId,
        parsed,
        hooks.commitClientAuthoring,
      );
    }
    const next = structuredClone(document);
    const nextActivation = next.activations[parsed.activationIndex];
    if (action === "set_enabled") {
      if (typeof request.enabled !== "boolean") throw new Error("enabled must be boolean");
      nextActivation.enabled = request.enabled;
    } else {
      applyScheduledDraft(nextActivation.config.schedule, request?.draft, false);
    }
    parseCronActivationDocument(next, interfaceId);
    await requireUnchangedJsonDigest(activationPath, digest, "activation document");
    await atomicWriteJson(activationPath, next);
    return {
      action,
      interfaceId,
      activationDigest: crypto.createHash("sha256").update(await fs.readFile(activationPath)).digest("hex"),
    };
  });
}

async function deleteScheduledTopology(
  interfaceRoot,
  octopusHomeRoot,
  interfaceId,
  parsed,
  commitClientAuthoring,
) {
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const runId = requiredSafeRegistryId(parsed.config.run_template.id, "scheduled run id");
  if (runId !== `run_${interfaceId}`) throw new Error("scheduled topology run id is not owned by this schedule");
  const suffix = crypto.createHash("sha256").update(interfaceId).digest("hex").slice(0, 10);
  const interfaceIds = collectScheduledRunInterfaces(parsed.config.run_template, parsed.config.authoring, interfaceId);
  if (!interfaceIds.includes(interfaceId)) throw new Error("scheduled topology does not contain its schedule interface");
  for (const candidate of interfaceIds) {
    if (candidate !== interfaceId && !candidate.endsWith(`_${suffix}`)) {
      throw new Error(`scheduled topology contains a non-private interface: ${candidate}`);
    }
    const directory = path.join(interfaceRoot, candidate);
    await requirePlainDirectory(directory, `scheduled private interface ${candidate}`);
    const descriptor = await readRegularJsonDocument(path.join(directory, "interface.json"), `scheduled private interface ${candidate}`);
    if (descriptor.document?.id !== candidate) throw new Error(`scheduled private interface ${candidate} has drifted identity`);
  }
  const runRoot = await fs.realpath(path.join(octopusHomeRoot, "state", "runs"));
  const runPath = path.join(runRoot, `${runId}.json`);
  const run = await readRegularJsonDocument(runPath, `scheduled run ${runId}`);
  if (run.document?.id !== runId) throw new Error(`scheduled run ${runId} has drifted identity`);
  const removals = await Promise.all(interfaceIds.map((candidate) => (
    snapshotRemovalDirectory(
      path.join(interfaceRoot, candidate),
      `scheduled private interface ${candidate}`,
      false,
    )
  )));
  const authoringTool = parsed.config.authoring.file_write_tool;
  if (authoringTool.endsWith(`_${suffix}`)) {
    const toolDirectory = path.join(interfaceRoot, "..", "tool", requiredSafeToolId(authoringTool, "scheduled authoring tool"));
    const tool = await readRegularJsonDocument(path.join(toolDirectory, "tool.json"), "scheduled authoring tool");
    if (tool.document.id !== authoringTool || tool.document.interface !== parsed.config.authoring.environment_interface) throw new Error("scheduled authoring tool has drifted ownership");
    removals.push(await snapshotRemovalDirectory(toolDirectory, "scheduled authoring tool", false));
  }
  const committed = await commitClientAuthoring({
    label: `delete scheduled interface ${interfaceId}`,
    writes: [{ file: runPath, beforeDigest: run.digest, next: null }],
    removals,
    publishBarrierPath: runPath,
  });
  return {
    action: "delete",
    interfaceId,
    runId,
    deleted: true,
    removedInterfaceIds: interfaceIds,
    cleanupWarnings: arrayValue(committed?.cleanupWarnings),
  };
}

async function createScheduledInterface(
  interfaceRoot,
  octopusHomeRoot,
  request,
  commitClientAuthoring,
) {
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const sourceInterfaceId = requiredSafeRegistryId(request?.sourceInterfaceId, "sourceInterfaceId");
  const interfaceId = requiredSafeRegistryId(request?.interfaceId, "interfaceId");
  if (sourceInterfaceId === interfaceId) throw new Error("new scheduled interface id must differ from its template");
  const sourceDirectory = path.join(interfaceRoot, sourceInterfaceId);
  await requirePlainDirectory(sourceDirectory, "scheduled template directory");
  const sourcePackage = await readScheduledInterfaceCloneSource(interfaceRoot, sourceInterfaceId);
  const sourceActivation = await readRegularJsonDocument(path.join(sourceDirectory, "activation.json"), "activation document");
  if (sourceActivation.digest !== requiredDigest(request?.sourceActivationDigest, "sourceActivationDigest")) {
    throw new Error("scheduled template changed before it could be cloned");
  }
  const parsed = parseCronActivationDocument(sourceActivation.document, sourceInterfaceId);
  const trigger = parsed.config.run_template.checkpoints.find((checkpoint) => interfaceBindingId(checkpoint.interface_binding) === sourceInterfaceId);
  if (parsed.config.target.mode !== "new_run" && !arrayValue(trigger?.outbound_interface_bindings).some((value) => interfaceBindingId(value))) {
    throw new Error("selected scheduled template has no outbound interface binding and cannot create useful work");
  }
  const targetDirectory = path.join(interfaceRoot, interfaceId);
  const existing = await fs.lstat(targetDirectory).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
  if (existing) throw new Error(`interface already exists: ${interfaceId}`);
  const suffix = crypto.createHash("sha256").update(interfaceId).digest("hex").slice(0, 10);
  const runId = requiredSafeRegistryId(`run_${interfaceId}`, "derived run id");
  const sourceInterfaces = collectScheduledRunInterfaces(parsed.config.run_template, parsed.config.authoring, sourceInterfaceId);
  const interfaceMap = new Map();
  for (const sourceId of sourceInterfaces) {
    interfaceMap.set(sourceId, sourceId === sourceInterfaceId ? interfaceId : requiredSafeRegistryId(`${sourceId}_${suffix}`, "private interface id"));
  }
  const sourceToolId = requiredSafeToolId(parsed.config.authoring.file_write_tool, "scheduled authoring tool");
  const sourceTool = await readRegularJsonDocument(path.join(interfaceRoot, "..", "tool", sourceToolId, "tool.json"), "scheduled authoring tool");
  if (sourceTool.document.id !== sourceToolId || sourceTool.document.interface !== parsed.config.authoring.environment_interface) throw new Error("scheduled authoring tool must belong to its declared door");
  const targetToolId = requiredSafeToolId(`${sourceToolId}_${suffix}`, "private authoring tool");
  interfaceMap.set(sourceToolId, targetToolId);
  const clonedTopology = cloneCronRunTemplate(parsed.config.run_template, interfaceMap, runId, suffix);
  const runTemplate = clonedTopology.runTemplate;
  const activation = remapScheduledValue(
    structuredClone(sourceActivation.document.activations[parsed.activationIndex]),
    interfaceMap,
    parsed.config.run_template.id,
    runId,
    clonedTopology.checkpointMap,
  );
  activation.enabled = true;
  activation.config.run_template = runTemplate;
  activation.config.schedule.reminder_env = "";
  applyScheduledDraft(activation.config.schedule, request?.draft, true);
  const activationDocument = { schema_version: 1, interface: interfaceId, activations: [activation] };
  parseCronActivationDocument(activationDocument, interfaceId);
  const packages = [];
  for (const sourceId of sourceInterfaces) {
    const targetId = interfaceMap.get(sourceId);
    const cloneSource = sourceId === sourceInterfaceId
      ? sourcePackage
      : await readScheduledInterfaceCloneSource(interfaceRoot, sourceId);
    const descriptor = scheduledSiblingDescriptor(cloneSource, targetId, interfaceMap, parsed.config.run_template.id, runId, clonedTopology.checkpointMap);
    const life = cloneSource.life
      ? remapScheduledValue(importScheduledLife(cloneSource.life, sourceId), interfaceMap, parsed.config.run_template.id, runId, clonedTopology.checkpointMap)
      : null;
    const policySource = await readOptionalRegularJsonDocument(path.join(interfaceRoot, sourceId, "policy.json"), `interface policy ${sourceId}`);
    let policy = null;
    if (policySource) {
      if (policySource.document?.id !== sourceId) throw new Error(`interface policy ${sourceId} does not match its registry folder`);
      policy = remapScheduledValue(policySource.document, interfaceMap, parsed.config.run_template.id, runId, clonedTopology.checkpointMap);
      policy.id = targetId;
    }
    let interfaceActivation = null;
    if (sourceId !== sourceInterfaceId) {
      const activationSource = await readOptionalRegularJsonDocument(path.join(interfaceRoot, sourceId, "activation.json"), `interface activation ${sourceId}`);
      if (activationSource) {
        if (activationSource.document?.interface !== sourceId) throw new Error(`interface activation ${sourceId} does not match its registry folder`);
        interfaceActivation = remapScheduledValue(activationSource.document, interfaceMap, parsed.config.run_template.id, runId, clonedTopology.checkpointMap);
        interfaceActivation.interface = targetId;
        privatizeScheduledActivation(interfaceActivation, targetId);
      }
    }
    packages.push({ sourceId, targetId, descriptor, life, policy, activation: interfaceActivation });
  }
  const runRoot = await fs.realpath(path.join(octopusHomeRoot, "state", "runs"));
  const runPath = path.join(runRoot, `${runId}.json`);
  if (await fs.lstat(runPath).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error))) throw new Error(`run record already exists: ${runId}`);
  for (const packagePlan of packages) {
    const target = path.join(interfaceRoot, packagePlan.targetId);
    if (await fs.lstat(target).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error))) {
      throw new Error(`private interface already exists: ${packagePlan.targetId}`);
    }
  }
  const writes = [];
  const targetToolPath = path.join(interfaceRoot, "..", "tool", targetToolId, "tool.json");
  writes.push({ file: targetToolPath, beforeDigest: null, next: {
    ...remapScheduledValue(sourceTool.document, interfaceMap, parsed.config.run_template.id, runId, clonedTopology.checkpointMap),
    copy_of: sourceTool.document.copy_of ?? sourceToolId,
  } });
  for (const packagePlan of packages) {
    const directory = path.join(interfaceRoot, packagePlan.targetId);
    writes.push({ file: path.join(directory, "interface.json"), beforeDigest: null, next: packagePlan.descriptor });
    if (packagePlan.life) writes.push({ file: path.join(directory, "life.json"), beforeDigest: null, next: packagePlan.life });
    if (packagePlan.policy) writes.push({ file: path.join(directory, "policy.json"), beforeDigest: null, next: packagePlan.policy });
    if (packagePlan.activation) {
      writes.push({ file: path.join(directory, "activation.json"), beforeDigest: null, next: packagePlan.activation });
    }
  }
  writes.push({ file: runPath, beforeDigest: null, next: runTemplate });
  const ownerActivationPath = path.join(targetDirectory, "activation.json");
  writes.push({ file: ownerActivationPath, beforeDigest: null, next: activationDocument });
  await commitClientAuthoring({
    label: `create scheduled interface ${interfaceId}`,
    writes,
    removals: [],
    publishBarrierPath: ownerActivationPath,
  });
  return {
    action: "create",
    interfaceId,
    runId,
    activationDigest: crypto.createHash("sha256")
      .update(Buffer.from(`${JSON.stringify(activationDocument, null, 2)}\n`))
      .digest("hex"),
  };
}

function setupTriggerEntries(pack, selectedRecommended) {
  const chosen = new Set(arrayValue(selectedRecommended));
  const selected = new Set(arrayValue(pack?.components)
    .filter((component) => component.selection !== "recommended" || chosen.has(component.package_id))
    .map((component) => component.package_id));
  return arrayValue(pack?.workspace_setup?.triggers)
    .filter((entry) => selected.has(entry.package_id))
    .map((entry) => ({
      package_id: entry.package_id,
      template_interface_id: entry.template_interface_id,
      interface_id: entry.interface_id,
      quiet_period_seconds: entry.quiet_period_seconds,
    }))
    .sort((left, right) => left.interface_id.localeCompare(right.interface_id));
}

async function setupTriggerState(interfaceRoot, octopusHomeRoot, entry) {
  const interfaceId = requiredSafeRegistryId(entry?.interface_id, "setup trigger interface id");
  const directory = await fs.lstat(path.join(interfaceRoot, interfaceId)).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
  if (!directory) return null;
  const activation = await readRegularJsonDocument(path.join(interfaceRoot, interfaceId, "activation.json"), `setup trigger ${interfaceId} activation`);
  const parsed = parseCronActivationDocument(activation.document, interfaceId);
  const runId = `run_${interfaceId}`;
  const run = await readOptionalRegularJsonDocument(path.join(octopusHomeRoot, "state", "runs", `${runId}.json`), `setup trigger run ${runId}`);
  if (parsed.activation.enabled !== true || parsed.config.run_template.id !== runId || run?.document?.id !== runId
    || parsed.config.schedule.inactivity?.seconds !== entry.quiet_period_seconds) {
    throw new Error(`setup trigger ${interfaceId} exists but is not the enabled quiet-period trigger bound to ${runId}`);
  }
  return {
    interface_id: interfaceId,
    template_interface_id: entry.template_interface_id,
    run_id: runId,
    quiet_period_seconds: entry.quiet_period_seconds,
    activation_digest: activation.digest,
  };
}

async function setupTriggerProof(registryRoot, octopusHomeRoot, entries) {
  const interfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const proven = [];
  for (const entry of entries) {
    const state = await setupTriggerState(interfaceRoot, octopusHomeRoot, entry);
    if (!state) return null;
    proven.push(state);
  }
  return proven;
}

async function armSetupTriggers(registryRoot, octopusHomeRoot, entries, hooks = {}) {
  const interfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const armed = [];
  for (const entry of entries) {
    if (await setupTriggerState(interfaceRoot, octopusHomeRoot, entry)) continue;
    const sourceInterfaceId = requiredSafeRegistryId(entry.template_interface_id, "setup trigger template");
    const source = await readRegularJsonDocument(path.join(interfaceRoot, sourceInterfaceId, "activation.json"), `setup trigger template ${sourceInterfaceId}`);
    const schedule = parseCronActivationDocument(source.document, sourceInterfaceId).config.schedule;
    if (!schedule.inactivity) throw new Error(`setup trigger template ${sourceInterfaceId} is not a quiet-period trigger`);
    if (!Number.isSafeInteger(entry.quiet_period_seconds) || entry.quiet_period_seconds < 60) throw new Error(`setup trigger ${entry.interface_id} quiet period must be at least one minute`);
    armed.push(await createScheduledInterface(interfaceRoot, octopusHomeRoot, {
      sourceInterfaceId,
      sourceActivationDigest: source.digest,
      interfaceId: entry.interface_id,
      draft: {
        type: "inactivity",
        inactivity: {
          seconds: entry.quiet_period_seconds,
          interface_match: schedule.inactivity.interface_match,
          ...(schedule.inactivity.exclude_interface_match ? { exclude_interface_match: schedule.inactivity.exclude_interface_match } : {}),
          ...(schedule.inactivity.within_ee ? { within_ee: schedule.inactivity.within_ee } : {}),
        },
        timezone: schedule.timezone || "local",
        fireOnStart: false,
        instructions: schedule.reminder_default,
      },
    }, hooks.commitClientAuthoring));
  }
  return armed;
}

function applyScheduledDraft(schedule, draft, creating) {
  exactObjectKeys(draft, ["type", "intervalSeconds", "inactivity", "cron", "dueAt", "timezone", "fireOnStart", "instructions"], ["type", "timezone", "fireOnStart", "instructions"], "scheduled draft");
  delete schedule.inactivity;
  if (draft.type === "inactivity") {
    schedule.inactivity = structuredClone(draft.inactivity);
    schedule.interval_seconds = null;
    schedule.cron = null;
    delete schedule.due_at;
  } else if (draft.type === "interval") {
    if (typeof draft.intervalSeconds !== "number" || !Number.isFinite(draft.intervalSeconds) || draft.intervalSeconds <= 0) throw new Error("intervalSeconds must be a finite positive number");
    schedule.interval_seconds = draft.intervalSeconds;
    schedule.cron = null;
    delete schedule.due_at;
  } else if (draft.type === "cron") {
    validateCronExpression(draft.cron);
    schedule.interval_seconds = null;
    schedule.cron = draft.cron.trim().replace(/\s+/g, " ");
    delete schedule.due_at;
  } else if (draft.type === "once") {
    parseScheduledDueAt(draft.dueAt);
    schedule.interval_seconds = null;
    schedule.cron = null;
    schedule.due_at = draft.dueAt;
  } else {
    throw new Error("scheduled draft type must be interval, cron, once or inactivity");
  }
  schedule.timezone = stringValue(draft.timezone);
  schedule.fire_on_start = draft.fireOnStart;
  schedule.reminder_default = draft.instructions;
  if (creating) schedule.reminder_env = "";
  validateCronSchedule(schedule);
}

function cloneCronRunTemplate(template, interfaceMap, runId, suffix) {
  const checkpointMap = new Map(template.checkpoints.map((checkpoint) => [checkpoint.id, `${checkpoint.id}_${suffix}`]));
  return {
    runTemplate: remapScheduledValue(template, interfaceMap, template.id, runId, checkpointMap),
    checkpointMap,
  };
}

function collectScheduledRunInterfaces(template, authoring, scheduleInterfaceId) {
  const interfaces = new Set([scheduleInterfaceId, template.worker, authoring.source_interface, authoring.environment_interface]);
  for (const checkpoint of template.checkpoints) {
    const inbound = interfaceBindingId(checkpoint.interface_binding);
    if (inbound) interfaces.add(inbound);
    for (const outbound of arrayValue(checkpoint.outbound_interface_bindings)) {
      const interfaceId = interfaceBindingId(outbound);
      if (interfaceId) interfaces.add(interfaceId);
    }
  }
  for (const interfaceId of Object.keys(objectOrEmpty(template.interface_state))) {
    if (stringValue(interfaceId)) interfaces.add(interfaceId);
  }
  return Array.from(interfaces, (value) => requiredSafeRegistryId(value, "scheduled run interface"));
}

function remapScheduledValue(value, interfaceMap, sourceRunId, runId, checkpointMap) {
  if (Array.isArray(value)) return value.map((entry) => remapScheduledValue(entry, interfaceMap, sourceRunId, runId, checkpointMap));
  if (value && typeof value === "object") {
    const result = {};
    for (const [key, nested] of Object.entries(value)) {
      const remappedKey = interfaceMap.get(key) ?? checkpointMap.get(key) ?? (key === sourceRunId ? runId : key);
      if (Object.prototype.hasOwnProperty.call(result, remappedKey)) throw new Error(`scheduled clone produces duplicate object key: ${remappedKey}`);
      result[remappedKey] = remapScheduledValue(nested, interfaceMap, sourceRunId, runId, checkpointMap);
    }
    return result;
  }
  if (typeof value !== "string") return value;
  return interfaceMap.get(value) ?? checkpointMap.get(value) ?? (value === sourceRunId ? runId : value);
}

async function readScheduledInterfaceCloneSource(interfaceRoot, interfaceId) {
  const source = (await readRegularJsonDocument(path.join(interfaceRoot, interfaceId, "interface.json"), `interface descriptor ${interfaceId}`)).document;
  validateScheduledInterfaceDescriptor(source, interfaceId);
  if (stringValue(source.reference)) {
    throw new Error(`interface ${interfaceId} is not self-contained and cannot be cloned into an exact scheduled route`);
  }
  const materialized = structuredClone(source);
  validateScheduledInterfaceDescriptor(materialized, interfaceId);
  const sourceLife = await readOptionalRegularJsonDocument(path.join(interfaceRoot, interfaceId, "life.json"), `interface life ${interfaceId}`);
  return { source, materialized, life: sourceLife?.document ?? null };
}

function validateScheduledInterfaceDescriptor(descriptor, interfaceId) {
  exactObjectKeys(
    descriptor,
    ["id", "interface_type", "role", "direction", "source_names", "reference", "interface_driver_ref", "interface_driver_config", "interface_policy", "copy_of"],
    ["id", "interface_type", "role", "direction", "source_names"],
    `interface descriptor ${interfaceId}`,
  );
  if (descriptor.id !== interfaceId) throw new Error(`interface descriptor ${interfaceId} does not match its registry folder`);
  if (!['world', 'system'].includes(descriptor.interface_type)) throw new Error(`interface descriptor ${interfaceId} has an invalid type`);
  if (!stringValue(descriptor.role) || !['inbound', 'outbound', 'bidirectional'].includes(descriptor.direction)) throw new Error(`interface descriptor ${interfaceId} has an invalid classification`);
  if (!Array.isArray(descriptor.source_names) || descriptor.source_names.length === 0 || descriptor.source_names.some((value) => !stringValue(value))) {
    throw new Error(`interface descriptor ${interfaceId} has invalid source_names`);
  }
}

function importScheduledLife(value, sourceInterfaceId) {
  if (Array.isArray(value)) return value.map(item => importScheduledLife(item, sourceInterfaceId));
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, importScheduledLife(item, sourceInterfaceId)]));
  return typeof value === "string" && value.startsWith("owner://packages/")
    ? `interface://${sourceInterfaceId}/${value.slice("owner://".length)}` : value;
}

function scheduledSiblingDescriptor(cloneSource, interfaceId, interfaceMap, sourceRunId, runId, checkpointMap) {
  const descriptor = remapScheduledValue(
    structuredClone(cloneSource.materialized),
    interfaceMap,
    sourceRunId,
    runId,
    checkpointMap,
  );
  descriptor.id = interfaceId;
  descriptor.copy_of = cloneSource.materialized.copy_of ?? cloneSource.materialized.id;
  delete descriptor.reference;
  validateScheduledInterfaceDescriptor(descriptor, interfaceId);
  return descriptor;
}

function privatizeScheduledActivation(document, interfaceId) {
  exactObjectKeys(document, ["schema_version", "interface", "arrival", "activations"], ["schema_version", "interface", "activations"], `interface activation ${interfaceId}`);
  if (document.schema_version !== 1 || document.interface !== interfaceId || !Array.isArray(document.activations)) {
    throw new Error(`interface activation ${interfaceId} is invalid`);
  }
  document.activations.forEach((activation, index) => {
    exactObjectKeys(activation, ["activation_driver_ref", "enabled", "config"], ["activation_driver_ref", "config"], `interface activation ${interfaceId} entry ${index}`);
    if (activation.enabled !== undefined && typeof activation.enabled !== "boolean") throw new Error(`interface activation ${interfaceId} entry ${index}.enabled must be boolean`);
    const config = objectOrEmpty(activation?.config);
    if (activationDriverRef(activation) === "app_server_transport" && typeof config.listen_env === "string") {
      config.listen_env = `OCTOPUS_${interfaceId.toUpperCase().replace(/[^A-Z0-9_]/g, "_")}_LISTEN`;
    }
  });
}

async function withScheduleEditLock(directory, action) {
  const key = await fs.realpath(directory);
  const previous = scheduleEditTails.get(key) ?? Promise.resolve();
  const current = previous.then(action);
  const settled = current.then(() => undefined, () => undefined);
  scheduleEditTails.set(key, settled);
  try {
    return await current;
  } finally {
    if (scheduleEditTails.get(key) === settled) scheduleEditTails.delete(key);
  }
}
