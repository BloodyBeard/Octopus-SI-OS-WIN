module.exports = { activationDriverRef, cronScheduleFingerprint, parseCronActivationDocument, parseUnsignedCronInteger, scheduledInterfaceDisplayName, validateCronExpression, validateCronField, validateCronRunTemplate, validateCronSchedule, validateCronTarget };

const { parseScheduledDueAt } = require("../scheduledTime.cjs");
const crypto = require("node:crypto");
const path = require("node:path");
const { stableJson } = require("./jsonFiles.cjs");
const { arrayValue, exactObjectKeys, interfaceBindingId, requiredSafeRegistryId, stringValue, validateAuthoredInterfaceBinding } = require("./values.cjs");

function activationDriverRef(activation) {
  return stringValue(
    activation?.activation_driver_ref
    ?? activation?.handler_ref
    ?? activation?.activation_transport_ref,
  );
}

function scheduledInterfaceDisplayName(interfaceId, runId) {
  const source = stringValue(runId).replace(/^run_/, "") || interfaceId.replace(/^(world|system)_/, "");
  const words = source.replace(/[_-]+/g, " ").trim();
  return words ? `${words[0].toUpperCase()}${words.slice(1)}` : interfaceId;
}

function parseCronActivationDocument(document, interfaceId) {
  exactObjectKeys(document, ["schema_version", "interface", "activations"], ["schema_version", "interface", "activations"], "activation document");
  if (document.schema_version !== 1) throw new Error("activation schema_version must be 1");
  if (document.interface !== interfaceId) throw new Error("activation interface does not match its registry folder");
  if (!Array.isArray(document.activations) || document.activations.length === 0) throw new Error("activation document must contain activations");
  const matches = [];
  document.activations.forEach((activation, index) => {
    exactObjectKeys(activation, ["activation_driver_ref", "enabled", "config"], ["activation_driver_ref", "config"], `activation #${index}`);
    if (activation.enabled !== undefined && typeof activation.enabled !== "boolean") throw new Error(`activation #${index}.enabled must be boolean`);
    if (activationDriverRef(activation) === "cron_life") matches.push({ activation, index });
  });
  if (matches.length !== 1) throw new Error(`expected exactly one cron_life activation; found ${matches.length}`);
  const { activation, index } = matches[0];
  const config = activation.config;
  exactObjectKeys(config, ["run_template", "schedule", "authoring", "carry_forward", "target"], ["run_template", "schedule", "authoring", "target"], "cron activation config");
  const authoring = config.authoring;
  exactObjectKeys(
    authoring,
    ["source_interface", "environment_interface", "file_write_tool", "file_write_label", "result_timeout_ms", "max_files_per_call"],
    ["source_interface", "environment_interface", "file_write_tool", "file_write_label", "result_timeout_ms"],
    "cron authoring config",
  );
  for (const field of ["source_interface", "environment_interface", "file_write_tool", "file_write_label"]) {
    if (!stringValue(authoring[field])) throw new Error(`cron authoring config.${field} must be non-empty`);
  }
  if (!Number.isSafeInteger(authoring.result_timeout_ms) || authoring.result_timeout_ms <= 0) throw new Error("cron authoring result_timeout_ms must be a positive integer");
  if (authoring.max_files_per_call !== undefined && (!Number.isSafeInteger(authoring.max_files_per_call) || authoring.max_files_per_call <= 0)) throw new Error("cron authoring max_files_per_call must be a positive integer");
  if (config.carry_forward !== undefined) {
    exactObjectKeys(config.carry_forward, ["mark", "artifact_label"], ["mark", "artifact_label"], "cron carry_forward config");
    if (!stringValue(config.carry_forward.mark) || !stringValue(config.carry_forward.artifact_label)) throw new Error("cron carry_forward fields must be non-empty");
  }
  validateCronSchedule(config.schedule);
  validateCronRunTemplate(config.run_template, interfaceId, authoring);
  validateCronTarget(config.target, config.run_template, interfaceId, config.carry_forward);
  return { activation, activationIndex: index, config };
}

function validateCronTarget(target, template, interfaceId, carryForward) {
  exactObjectKeys(
    target,
    [
      "mode", "ee_id", "worker_interface_id", "provider_interface_id",
      "reasoning_effort", "client_surface_role", "id_prefix",
      "interface_private_state_root", "door_host_slots",
      "model", "selected_worker_environment_kinds", "context_artifact_bindings",
    ],
    ["mode"],
    "cron target",
  );
  const trigger = arrayValue(template.checkpoints)
    .find((checkpoint) => interfaceBindingId(checkpoint?.interface_binding) === interfaceId);
  const outbound = arrayValue(trigger?.outbound_interface_bindings).map(interfaceBindingId).filter(Boolean);
  if (target.mode === "existing_run") {
    exactObjectKeys(target, ["mode"], ["mode"], "cron target");
    return;
  }
  if (target.mode !== "new_run") throw new Error("cron target.mode must be existing_run or new_run");
  exactObjectKeys(
    target,
    [
      "mode", "ee_id", "worker_interface_id", "provider_interface_id",
      "reasoning_effort", "client_surface_role", "id_prefix",
      "interface_private_state_root", "door_host_slots",
      "model", "selected_worker_environment_kinds", "context_artifact_bindings",
    ],
    [
      "mode", "ee_id", "worker_interface_id", "provider_interface_id",
      "reasoning_effort", "client_surface_role", "id_prefix", "door_host_slots",
    ],
    "cron target",
  );
  if (outbound.length) throw new Error("new_run cron target controller trigger must have no outbound interfaces");
  if (carryForward !== undefined && carryForward !== null) throw new Error("new_run cron target cannot carry controller-run artifacts");
  for (const field of ["ee_id", "worker_interface_id", "provider_interface_id", "id_prefix"]) {
    requiredSafeRegistryId(target[field], `cron target.${field}`);
  }
  if (stringValue(target.id_prefix).length > 48) throw new Error("cron target.id_prefix must not exceed 48 characters");
  if (target.model !== undefined && !stringValue(target.model)) throw new Error("cron target.model must be non-empty");
  if (target.selected_worker_environment_kinds !== undefined) {
    if (!Array.isArray(target.selected_worker_environment_kinds) || new Set(target.selected_worker_environment_kinds).size !== target.selected_worker_environment_kinds.length) throw new Error("cron target.selected_worker_environment_kinds must contain unique kind ids");
    for (const kind of target.selected_worker_environment_kinds) requiredSafeRegistryId(kind, "cron target worker environment kind");
  }
  if (target.context_artifact_bindings !== undefined) {
    if (!Array.isArray(target.context_artifact_bindings)) throw new Error("cron target.context_artifact_bindings must be an array");
    for (const artifact of target.context_artifact_bindings) {
      exactObjectKeys(artifact, ["id"], ["id"], "cron target context artifact");
      requiredSafeRegistryId(artifact.id, "cron target context artifact id");
    }
  }
  if (!stringValue(target.reasoning_effort)) throw new Error("cron target.reasoning_effort must be non-empty");
  if (!["home", "task"].includes(target.client_surface_role)) throw new Error("cron target.client_surface_role must be home or task");
  if (target.interface_private_state_root !== undefined && !path.isAbsolute(stringValue(target.interface_private_state_root))) throw new Error("cron target.interface_private_state_root must be absolute");
  if (!target.door_host_slots || typeof target.door_host_slots !== "object" || Array.isArray(target.door_host_slots)) {
    throw new Error("cron target.door_host_slots must be a JSON object");
  }
  for (const [door, slots] of Object.entries(target.door_host_slots)) {
    requiredSafeRegistryId(door, "cron target door interface");
    if (!slots || typeof slots !== "object" || Array.isArray(slots) || !Object.keys(slots).length) {
      throw new Error(`cron target.door_host_slots.${door} must be a non-empty object`);
    }
    const roots = slots;
    for (const [slot, root] of Object.entries(roots)) {
      requiredSafeRegistryId(slot, `cron target.door_host_slots.${door} slot`);
      if (!path.isAbsolute(stringValue(root))) throw new Error(`cron target.door_host_slots.${door}.${slot} must be absolute`);
    }
  }
}

function validateCronSchedule(schedule) {
  exactObjectKeys(
    schedule,
    ["interval_seconds", "cron", "due_at", "inactivity", "fire_on_start", "timezone", "reminder_env", "reminder_default"],
    ["fire_on_start", "timezone", "reminder_env", "reminder_default"],
    "cron schedule",
  );
  if (typeof schedule.fire_on_start !== "boolean") throw new Error("schedule.fire_on_start must be boolean");
  if (typeof schedule.reminder_env !== "string" || !/^(?:[A-Z_][A-Z0-9_]*)?$/.test(schedule.reminder_env)) throw new Error("schedule.reminder_env is invalid");
  if (typeof schedule.reminder_default !== "string") throw new Error("schedule.reminder_default must be a string");
  const hasInterval = schedule.interval_seconds !== null && schedule.interval_seconds !== undefined;
  const hasCron = schedule.cron !== null && schedule.cron !== undefined;
  const hasDueAt = schedule.due_at !== null && schedule.due_at !== undefined;
  const hasInactivity = schedule.inactivity != null;
  if (Number(hasInterval) + Number(hasCron) + Number(hasDueAt) + Number(hasInactivity) !== 1) throw new Error("schedule must declare exactly one of interval_seconds, cron, due_at or inactivity");
  if (hasInactivity) {
    exactObjectKeys(schedule.inactivity, ["seconds", "interface_match", "exclude_interface_match", "within_ee"], ["seconds", "interface_match"], "schedule.inactivity");
    if (schedule.inactivity.within_ee !== undefined) requiredSafeRegistryId(schedule.inactivity.within_ee, "inactivity.within_ee");
    if (typeof schedule.inactivity.seconds !== "number" || !Number.isFinite(schedule.inactivity.seconds) || schedule.inactivity.seconds < 0.001) throw new Error("inactivity.seconds must be finite and at least 0.001");
    for (const field of ["interface_match", "exclude_interface_match"]) {
      const selector = schedule.inactivity[field];
      if (field === "exclude_interface_match" && selector === undefined) continue;
      exactObjectKeys(selector, ["interface_type", "role", "interface_driver_ref"], [], `inactivity.${field}`);
      if (!Object.keys(selector).length || Object.values(selector).some((value) => typeof value !== "string" || !value)) throw new Error(`inactivity.${field} requires non-empty descriptor fields`);
    }
    if (schedule.fire_on_start) throw new Error("inactivity schedule requires fire_on_start=false");
  }
  if (hasDueAt) {
    parseScheduledDueAt(schedule.due_at);
    if (schedule.fire_on_start) throw new Error("one-shot schedule.due_at requires fire_on_start=false");
  }
  if (hasInterval && (typeof schedule.interval_seconds !== "number" || !Number.isFinite(schedule.interval_seconds) || schedule.interval_seconds <= 0)) {
    throw new Error("schedule.interval_seconds must be a finite positive number");
  }
  if (hasCron) validateCronExpression(schedule.cron);
  if (!stringValue(schedule.timezone)) throw new Error("schedule.timezone must be non-empty");
  if (!["UTC", "local"].includes(schedule.timezone)) {
    try {
      new Intl.DateTimeFormat("en-US", { timeZone: schedule.timezone }).format(new Date());
    } catch {
      throw new Error(`schedule.timezone is unknown: ${schedule.timezone}`);
    }
  }
}

function validateCronExpression(expression) {
  if (typeof expression !== "string") throw new Error("schedule.cron must be a string");
  const fields = expression.trim().split(/\s+/);
  if (fields.length !== 5) throw new Error("schedule.cron must contain five fields");
  validateCronField(fields[0], 0, 59, false, "minute");
  validateCronField(fields[1], 0, 23, false, "hour");
  validateCronField(fields[2], 1, 31, false, "day-of-month");
  validateCronField(fields[3], 1, 12, false, "month");
  validateCronField(fields[4], 0, 6, true, "day-of-week");
}

function validateCronField(field, minimum, maximum, sundayAlias, label) {
  const acceptedMaximum = sundayAlias ? 7 : maximum;
  for (const item of field.split(",")) {
    if (!item) throw new Error(`cron ${label} contains an empty list item`);
    const pieces = item.split("/");
    if (pieces.length > 2) throw new Error(`cron ${label} has an invalid step`);
    const step = pieces.length === 2 ? parseUnsignedCronInteger(pieces[1], `${label} step`) : 1;
    if (step <= 0) throw new Error(`cron ${label} step must be greater than zero`);
    const base = pieces[0];
    let lower;
    let upper;
    if (base === "*") {
      lower = minimum;
      upper = acceptedMaximum;
    } else if (base.includes("-")) {
      const bounds = base.split("-");
      if (bounds.length !== 2) throw new Error(`cron ${label} has an invalid range`);
      lower = parseUnsignedCronInteger(bounds[0], `${label} range start`);
      upper = parseUnsignedCronInteger(bounds[1], `${label} range end`);
    } else {
      lower = parseUnsignedCronInteger(base, `${label} value`);
      upper = lower;
    }
    if (lower < minimum || upper > acceptedMaximum || lower > upper) throw new Error(`cron ${label} range is outside ${minimum}-${acceptedMaximum}`);
  }
}

function parseUnsignedCronInteger(value, label) {
  if (!/^[0-9]+$/.test(value)) throw new Error(`cron ${label} must be an unsigned integer`);
  return Number(value);
}

function validateCronRunTemplate(template, interfaceId, authoring) {
  exactObjectKeys(
    template,
    ["id", "ee", "worker", "trace", "status", "interface_state", "checkpoints", "active_checkpoints"],
    ["id", "ee", "worker", "trace", "status", "checkpoints", "active_checkpoints"],
    "cron run_template",
  );
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(stringValue(template.id))) throw new Error("run_template.id must be registry-safe");
  if (!stringValue(template.ee) || !stringValue(template.worker) || !stringValue(template.trace)) throw new Error("run_template must declare ee, worker, and trace");
  if (!Array.isArray(template.checkpoints) || !Array.isArray(template.active_checkpoints)) throw new Error("run_template checkpoints must be arrays");
  const active = template.active_checkpoints;
  if (active.some((id) => !stringValue(id)) || new Set(active).size !== active.length) throw new Error("run_template active_checkpoints are invalid");
  const byId = new Map();
  for (const checkpoint of template.checkpoints) {
    if (!checkpoint || typeof checkpoint !== "object" || Array.isArray(checkpoint)) throw new Error("run_template checkpoint must be an object");
    const checkpointId = stringValue(checkpoint.id);
    if (!checkpointId || byId.has(checkpointId)) throw new Error("run_template checkpoint ids must be unique and non-empty");
    if (checkpoint.run !== template.id) throw new Error(`run_template checkpoint ${checkpointId} belongs to another run`);
    if (!stringValue(checkpoint.trace)) throw new Error(`run_template checkpoint ${checkpointId} has no trace`);
    if (checkpoint.interface_binding !== undefined && checkpoint.interface_binding !== null) {
      validateAuthoredInterfaceBinding(checkpoint.interface_binding, `run_template checkpoint ${checkpointId}.interface_binding`);
    }
    for (const field of ["outbound_interface_bindings", "artifact_bindings", "required_verified_artifacts"]) {
      if (!Array.isArray(checkpoint[field] ?? [])) throw new Error(`run_template checkpoint ${checkpointId}.${field} must be an array`);
    }
    arrayValue(checkpoint.outbound_interface_bindings).forEach((binding, index) => {
      validateAuthoredInterfaceBinding(binding, `run_template checkpoint ${checkpointId}.outbound_interface_bindings[${index}]`);
    });
    byId.set(checkpointId, checkpoint);
  }
  const state = template.interface_state?.[interfaceId];
  if (!state || typeof state !== "object" || Array.isArray(state)) throw new Error("run_template has no cron interface_state");
  if (!Number.isSafeInteger(state.authoring_generation) || state.authoring_generation < 0) throw new Error("run_template authoring_generation must be unsigned");
  const frontier = state.frontier_checkpoint_ids;
  if (!Array.isArray(frontier) || frontier.length !== 3 || frontier.some((id) => !stringValue(id)) || new Set(frontier).size !== 3) {
    throw new Error("run_template frontier_checkpoint_ids must contain three unique ids");
  }
  if (frontier.some((id) => !byId.has(id) || active.filter((candidate) => candidate === id).length !== 1)) throw new Error("run_template frontier must be exactly active");
  const expectedBindings = [interfaceId, authoring.source_interface, authoring.environment_interface];
  frontier.forEach((id, index) => {
    if (interfaceBindingId(byId.get(id).interface_binding) !== expectedBindings[index]) throw new Error(`run_template frontier checkpoint ${id} has the wrong interface binding`);
  });
  const authoringCall = byId.get(frontier[1]);
  if (JSON.stringify(arrayValue(authoringCall.outbound_interface_bindings).map(interfaceBindingId)) !== JSON.stringify([authoring.environment_interface])) {
    throw new Error("run_template authoring call must have exactly its environment outbound");
  }
}

function cronScheduleFingerprint(interfaceId, config) {
  return crypto.createHash("sha256").update(stableJson({
    interface: interfaceId,
    interval_seconds: config.schedule.interval_seconds ?? null,
    cron: config.schedule.cron ?? null,
    fire_on_start: config.schedule.fire_on_start,
    timezone: config.schedule.timezone,
    reminder_env: config.schedule.reminder_env,
    reminder_default: config.schedule.reminder_default,
    carry_forward: config.carry_forward ?? null,
    target: config.target,
  })).digest("hex");
}
