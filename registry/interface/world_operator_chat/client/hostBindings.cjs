module.exports = { validatePlannedHostBindings };

const fs = require("node:fs/promises");
const path = require("node:path");
const { absolutePath, requiredArray, requiredObject, requiredString } = require("./values.cjs");

const hostFormedSlots = new Set([
  "registry_interface_root", "registry_ee_template_root", "registry_artifact_root",
  "registry_mark_root", "registry_verifier_root", "registry_tool_root",
  "registry_worker_environment_kind_root", "artifact_blobs_root",
  "app_server_control_root", "interface_hand_in_root", "interface_life_install_root",
  "activation_secrets_home", "interface_local_ipc_root",
]);
const privateHostSlots = new Set([
  "activation_secrets_home", "interface_life_install_root", "interface_local_ipc_root",
]);
const requiredAuthorityFields = ["read_root_slots", "write_root_slots", "executable_root_slots"];
const optionalAuthorityFields = ["optional_read_root_slots", "optional_write_root_slots", "optional_executable_root_slots"];

async function validatePlannedHostBindings({ octopusHome, registryRoot, plan }) {
  const host = requiredObject(JSON.parse(await fs.readFile(path.join(octopusHome, "host.json"), "utf8")), "host manifest");
  if (host.host_manifest_version !== 1) throw new Error("Unsupported host manifest version");
  const defaults = requiredObject(host.root_refs, "host.root_refs");
  for (const [slot, root] of Object.entries(defaults)) {
    rootSlot(slot, "host root slot");
    absolutePath(root, `host.root_refs.${slot}`);
  }
  const pending = new Map(requiredArray(plan.writes, "plan.writes").map(write => [
    absolutePath(write.filePath, "planned descriptor path"), write.content,
  ]));
  const documents = new Map();
  async function read(kind, id, file) {
    if (!/^[A-Za-z0-9_-][A-Za-z0-9_.-]*$/.test(requiredString(id, `${kind} identity`))) {
      throw new Error(`Invalid ${kind} identity '${id}'`);
    }
    const target = path.join(registryRoot, kind, id, file);
    if (!documents.has(target)) {
      const content = pending.has(target) ? pending.get(target) : await fs.readFile(target, "utf8");
      documents.set(target, requiredObject(JSON.parse(content), target));
    }
    return documents.get(target);
  }
  const toolsByDoor = new Map();
  for (const id of requiredArray(plan.requiredToolIds, "plan.requiredToolIds")) {
    const tool = await read("tool", id, "tool.json");
    if (tool.id !== id) throw new Error(`Tool '${id}' has a different descriptor identity`);
    const tools = toolsByDoor.get(tool.interface) ?? [];
    tools.push(tool);
    toolsByDoor.set(tool.interface, tools);
  }
  for (const [id, tools] of toolsByDoor) {
    const descriptor = await read("interface", id, "interface.json");
    if (descriptor.id !== id) throw new Error(`Interface '${id}' has a different descriptor identity`);
    const overrides = descriptor.interface_driver_config?.host_slots ?? {};
    requiredObject(overrides, `${id}.host_slots`);
    const available = new Set([...Object.keys(defaults), ...hostFormedSlots]);
    for (const [slot, binding] of Object.entries(overrides)) {
      rootSlot(slot, `${id} host slot`);
      if (privateHostSlots.has(slot)) throw new Error(`Interface '${id}' cannot override host-formed root slot '${slot}'`);
      absolutePath(requiredObject(binding, `${id}.host_slots.${slot}`).path, `${id}.host_slots.${slot}.path`);
      available.add(slot);
    }
    const life = await read("interface", id, "life.json");
    const processes = requiredArray(life.processes, `${id}.life.processes`);
    if (!processes.length) throw new Error(`Interface '${id}' has no life processes`);
    const declared = new Set(Object.keys(overrides));
    const requireSlots = (slots, owner) => {
      for (const slot of slots) {
        if (!available.has(slot)) {
          throw new Error(`Interface '${id}' requires unbound host root slot '${slot}' (${owner}); configure host.root_refs or this door's host_slots before creating the task`);
        }
      }
    };
    for (const process of processes) {
      const authority = requiredObject(process.authority, `${id}.life.authority`);
      const required = processRequiredSlots(process);
      requireSlots(required, `life process ${process.bin}`);
      const slots = [...required, ...optionalAuthorityFields.flatMap(field => rootSlots(authority[field] ?? [], field))];
      for (const slot of slots) {
        declared.add(slot);
      }
    }
    for (const tool of tools) {
      const kinds = requiredArray(tool.supported_worker_environment_kinds, `${tool.id}.supported_worker_environment_kinds`);
      if (kinds.length !== 1) throw new Error(`Tool '${tool.id}' must declare exactly one worker environment kind`);
      const kind = await read("worker_environment_kind", kinds[0], "worker_environment_kind.json");
      if (kind.worker_environment_kind_id !== kinds[0]
        || !requiredArray(kind.door_driver_refs, `${kinds[0]}.door_driver_refs`).includes(descriptor.interface_driver_ref)) {
        throw new Error(`Tool '${tool.id}' has no matching declared door driver`);
      }
      const required = processRequiredSlots(requiredObject(tool.process, `${tool.id}.process`))
        .filter(slot => declared.has(slot));
      requireSlots(required, `tool ${tool.id}`);
    }
  }
}

function rootSlots(value, field) {
  return requiredArray(value, field).map(slot => rootSlot(slot, field));
}

function rootSlot(value, field) {
  if (typeof value !== "string" || !value || value.trim() !== value) {
    throw new Error(`${field} must be a non-empty trimmed root slot name`);
  }
  return value;
}

function processRequiredSlots(process) {
  const authority = requiredObject(process.authority, "process.authority");
  return [
    ...requiredAuthorityFields.flatMap(field => rootSlots(authority[field], field)),
    ...requiredArray(authority.executable_root_subpaths ?? [], "executable_root_subpaths")
      .map(entry => rootSlot(entry.root_slot, "executable root subpath slot")),
    ...rootSlots(process.coordinate_root_slots ?? [], "coordinate_root_slots"),
  ];
}
