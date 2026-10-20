module.exports = { arrayValue, atomicWriteJson, capSetStrings, contentHashHex, createJsonFile, dedupeRows, descriptorIdentity, endpointId, exactObjectKeys, filterByString, groupRowsBy, interfaceActivationRefs, interfaceArtifactTransferProjection, interfaceBindingArtifactNames, interfaceBindingId, isAdmittedInterfaceArtifactMovement, isEeInstanceJsonPath, isPathInside, isPlainRecord, isRunRecordJsonPath, isSortedUniqueStrings, isStringPathMap, ledgerArtifactId, ledgerArtifactLabel, limitRows, mapConcurrentOrdered, numberValue, objectOrEmpty, requiredAbsolutePath, requiredApplicationOperationId, requiredArtifactBindingId, requiredBoolean, requiredContentHash, requiredContractToken, requiredDigest, requiredLocalPackageId, requiredSafeRegistryId, requiredSafeToolId, requiredString, rowTimeMs, safeInterfacePathSegment, streamFragmentValue, stringValue, syncDirectory, timestampOf, topologyArtifactRow, unixSocketReadiness, validJsonPointer, validWorkerEnvironmentFormation, validateAuthoredInterfaceBinding };

const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const net = require("node:net");
const path = require("node:path");

function validJsonPointer(value) {
  return typeof value === "string"
    && value.startsWith("/")
    && value !== "/"
    && !value.split("/").slice(1).some((token) => /~(?:[^01]|$)/.test(token));
}

function requiredContractToken(value, label) {
  const token = requiredString(value, label);
  if (token.length > 160 || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(token)) {
    throw new Error(`${label} must be a compact contract token`);
  }
  return token;
}

function exactObjectKeys(value, allowed, required, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be a JSON object`);
  const unknown = Object.keys(value).filter((key) => !allowed.includes(key));
  const missing = required.filter((key) => !Object.prototype.hasOwnProperty.call(value, key));
  if (unknown.length) throw new Error(`${label} contains unknown fields: ${unknown.join(", ")}`);
  if (missing.length) throw new Error(`${label} is missing required fields: ${missing.join(", ")}`);
}

async function mapConcurrentOrdered(values, concurrency, task) {
  const output = new Array(values.length);
  let next = 0;
  const workers = Array.from(
    { length: Math.min(Math.max(1, concurrency), values.length) },
    async () => {
      for (;;) {
        const index = next;
        next += 1;
        if (index >= values.length) return;
        output[index] = await task(values[index], index);
      }
    },
  );
  await Promise.all(workers);
  return output;
}

function requiredApplicationOperationId(value) {
  const operationId = requiredString(value, "operationId");
  if (!/^[A-Za-z0-9_-]{8,96}$/.test(operationId)) throw new Error("operationId is invalid");
  return operationId;
}

function requiredAbsolutePath(value, label) {
  const candidate = requiredString(value, label);
  if (!path.isAbsolute(candidate)) throw new Error(`${label} must be absolute`);
  return candidate;
}

function requiredArtifactBindingId(value, label) {
  if (typeof value !== "string" || !value || value.trim() !== value || value.length > 512 || value.includes("\0")) {
    throw new Error(`${label} must be a non-empty canonical artifact id`);
  }
  return value;
}

function requiredSafeRegistryId(value, label) {
  const id = requiredString(value, label);
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(id)) throw new Error(`${label} must be a registry-safe id`);
  return id;
}

function requiredSafeToolId(value, label) {
  const id = requiredString(value, label);
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(id)) throw new Error(`${label} must be a tool-safe id`);
  return id;
}

function requiredLocalPackageId(value, label) {
  const id = requiredString(value, label);
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(id)) throw new Error(`${label} must be a local package id`);
  return id;
}

function requiredBoolean(value, label) {
  if (typeof value !== "boolean") throw new Error(`${label} must be a boolean`);
  return value;
}

function requiredDigest(value, label) {
  const digest = requiredString(value, label).toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(digest)) throw new Error(`${label} must be a sha256 digest`);
  return digest;
}

function requiredContentHash(value, label) {
  const digest = requiredString(value, label).toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(digest)) throw new Error(`${label} must be a 32-byte content digest`);
  return digest;
}

async function atomicWriteJson(file, value) {
  const directory = path.dirname(file);
  const temporary = path.join(directory, `.${path.basename(file)}.${process.pid}.${crypto.randomUUID()}.tmp`);
  const body = `${JSON.stringify(value, null, 2)}\n`;
  let handle;
  try {
    handle = await fs.open(temporary, "wx", 0o644);
    await handle.writeFile(body, "utf8");
    await handle.sync();
    await handle.close();
    handle = null;
    await fs.rename(temporary, file);
    await syncDirectory(directory);
  } finally {
    await handle?.close().catch(() => {});
    await fs.rm(temporary, { force: true }).catch(() => {});
  }
}

async function createJsonFile(file, value) {
  const body = `${JSON.stringify(value, null, 2)}\n`;
  const handle = await fs.open(file, "wx", 0o644);
  try {
    await handle.writeFile(body, "utf8");
    await handle.sync();
  } finally {
    await handle.close();
  }
  await syncDirectory(path.dirname(file));
}

async function syncDirectory(directory) {
  const handle = await fs.open(directory, "r");
  try {
    await handle.sync();
  } finally {
    await handle.close();
  }
}

function endpointId(endpoint, key) {
  return endpoint && typeof endpoint === "object" && typeof endpoint[key] === "string" ? endpoint[key] : "";
}

function ledgerArtifactLabel(row) {
  if (row?.__stream && typeof row.__stream.label === "string") return row.__stream.label;
  return row && row.artifact && typeof row.artifact.id === "string" ? row.artifact.id.split(":")[0] : "";
}

function ledgerArtifactId(row) {
  return row && row.artifact && typeof row.artifact.id === "string" ? row.artifact.id : "";
}

function isAdmittedInterfaceArtifactMovement(row) {
  return row?.kind === "InterfaceOutbound" || row?.kind === "InterfaceArrival";
}

function rowTimeMs(row) {
  return numberValue(row && row.recorded_at_unix_ms, 0);
}

function contentHashHex(content) {
  if (typeof content === "string" && /^[0-9a-f]{64}$/i.test(content)) {
    return content.toLowerCase();
  }
  if (Buffer.isBuffer(content) || ArrayBuffer.isView(content)) {
    const bytes = Buffer.from(content.buffer, content.byteOffset, content.byteLength);
    return bytes.length === 32 ? bytes.toString("hex") : "";
  }
  if (!Array.isArray(content)) return "";
  return content.map((byte) => (Number(byte) & 0xff).toString(16).padStart(2, "0")).join("");
}

function topologyArtifactRow(row) {
  const fields = [
    "artifact_id", "id", "name", "label", "artifact_label", "route_label", "status",
    "run_id", "ee_id", "recorded_at_unix_ms", "created_at_unix_ms", "ts_unix_ms",
    "created_at", "ts", "time",
  ];
  const projected = {};
  for (const field of fields) {
    if (Object.hasOwn(row, field)) projected[field] = row[field];
  }
  if (row.meta && typeof row.meta === "object" && !Array.isArray(row.meta)) {
    projected.meta = {};
    for (const field of ["run_id", "ee_id"]) {
      if (Object.hasOwn(row.meta, field)) projected.meta[field] = row.meta[field];
    }
  }
  return projected;
}

function limitRows(rows, params) {
  const limit = numberValue(params?.limit, rows.length);
  if (!limit || limit < 0) return rows;
  return rows.slice(0, limit);
}

function descriptorIdentity(descriptor, fields, fallback) {
  for (const field of fields) {
    const value = stringValue(descriptor?.[field]);
    if (value) return value;
  }
  return fallback;
}

function objectOrEmpty(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

function isRunRecordJsonPath(file) {
  const base = path.basename(file);
  if (base === "run.json") return true;
  return path.extname(base) === ".json" && path.basename(path.dirname(file)) === "runs";
}

function isEeInstanceJsonPath(file) {
  return path.extname(file) === ".json" && path.basename(path.dirname(file)) === "ee_instances";
}

function isStringPathMap(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  return Object.entries(value).every(([name, candidate]) => (
    name.length > 0
    && name.trim() === name
    && !/[\u0000-\u001f\u007f]/.test(name)
    && typeof candidate === "string"
    && candidate.length > 0
    && path.isAbsolute(candidate)
  ));
}

function validWorkerEnvironmentFormation(formation) {
  const expectedKeys = [
    "worker_environment_id", "run", "ee", "worker",
    "door_interface", "door_descriptor_hash",
    "worker_environment_kind", "worker_environment_kind_descriptor_hash",
    "tool_descriptor_hashes", "instance_config_hashes", "formation_pin",
    "granted_tools", "door_root_bindings", "readable_root_slots",
    "writable_root_slots", "network_ceiling", "requires_release",
  ];
  const keys = Object.keys(formation).sort();
  if (keys.length !== expectedKeys.length || expectedKeys.some((key) => !keys.includes(key))) return false;
  const identities = [
    formation.run, formation.ee, formation.worker,
    formation.door_interface, formation.worker_environment_kind,
  ];
  if (identities.some((value) => typeof value !== "string" || !value || value.trim() !== value || /[\u0000-\u001f\u007f]/.test(value))) return false;
  if (!/^we_[0-9a-f]{32}$/.test(stringValue(formation.worker_environment_id))) return false;
  if (typeof formation.door_descriptor_hash !== "string" || !/^[0-9a-f]{64}$/.test(formation.door_descriptor_hash)) return false;
  if (typeof formation.worker_environment_kind_descriptor_hash !== "string" || !/^[0-9a-f]{64}$/.test(formation.worker_environment_kind_descriptor_hash)) return false;
  if (typeof formation.formation_pin !== "string" || !/^[0-9a-f]{64}$/.test(formation.formation_pin)) return false;
  if (!isSortedUniqueStrings(formation.granted_tools, true)) return false;
  if (!isSortedUniqueStrings(formation.readable_root_slots, false)) return false;
  if (!isSortedUniqueStrings(formation.writable_root_slots, false)) return false;
  if (!isStringPathMap(formation.door_root_bindings)) return false;
  if (!["denied", "restricted", "full"].includes(formation.network_ceiling)) return false;
  if (typeof formation.requires_release !== "boolean") return false;
  if (!isPlainRecord(formation.tool_descriptor_hashes) || !isPlainRecord(formation.instance_config_hashes)) return false;
  const toolHashes = objectOrEmpty(formation.tool_descriptor_hashes);
  if (Object.keys(toolHashes).sort().join("\0") !== formation.granted_tools.join("\0")) return false;
  if (Object.values(toolHashes).some((hash) => typeof hash !== "string" || !/^[0-9a-f]{64}$/.test(hash))) return false;
  const configHashes = objectOrEmpty(formation.instance_config_hashes);
  if (Object.entries(configHashes).some(([name, hash]) => (
    name.trim() !== name || !name.endsWith(".json") || name.includes("/") || name.includes("\\")
    || typeof hash !== "string" || !/^[0-9a-f]{64}$/.test(hash)
  ))) return false;
  const rootBindings = objectOrEmpty(formation.door_root_bindings);
  return [...formation.readable_root_slots, ...formation.writable_root_slots]
    .every((slot) => Object.prototype.hasOwnProperty.call(rootBindings, slot));
}

function isPlainRecord(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function isSortedUniqueStrings(value, requireNonEmpty) {
  if (!Array.isArray(value) || (requireNonEmpty && value.length === 0)) return false;
  return value.every((item, index) => (
    typeof item === "string"
    && item.length > 0
    && item.trim() === item
    && !/[\u0000-\u001f\u007f]/.test(item)
    && (index === 0 || value[index - 1] < item)
  ));
}

function isPathInside(root, candidate) {
  const relative = path.relative(path.resolve(root), path.resolve(candidate));
  return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative));
}

function groupRowsBy(rows, key) {
  const groups = new Map();
  for (const row of rows) {
    const value = stringValue(row?.[key]);
    if (!value) continue;
    const group = groups.get(value) ?? [];
    group.push(row);
    groups.set(value, group);
  }
  return groups;
}

function dedupeRows(rows, key) {
  const output = [];
  const seen = new Set();
  for (const row of rows) {
    const value = stringValue(row?.[key]);
    if (value && seen.has(value)) continue;
    if (value) seen.add(value);
    output.push(row);
  }
  return output;
}

function timestampOf(row) {
  const text = stringValue(row.recorded_at ?? row.created_at ?? row.ts ?? row.timestamp);
  if (text) return text;
  const ms = numberValue(row.recorded_at_unix_ms ?? row.wall_clock_unix_ms, 0);
  return ms ? new Date(ms).toISOString() : "";
}

function filterByString(rows, key, expected) {
  const expectedText = stringValue(expected).toLowerCase();
  if (!expectedText) return rows;
  return rows.filter((row) => stringValue(row?.[key]).toLowerCase() === expectedText);
}

function arrayValue(value) {
  return Array.isArray(value) ? value : [];
}

function capSetStrings(value) {
  if (Array.isArray(value)) return value.map(stringValue).filter(Boolean);
  if (value && typeof value === "object") {
    for (const key of ["items", "allowed", "values"]) {
      if (Array.isArray(value[key])) return value[key].map(stringValue).filter(Boolean);
    }
  }
  return [];
}

function validateAuthoredInterfaceBinding(value, label) {
  exactObjectKeys(value, ["interface", "config"], ["interface", "config"], label);
  const interfaceId = requiredSafeRegistryId(value.interface, `${label}.interface`);
  exactObjectKeys(value.config, ["artifact_policy"], ["artifact_policy"], `${label}.config`);
  const policy = value.config.artifact_policy;
  if (!policy || typeof policy !== "object" || Array.isArray(policy)) throw new Error(`${label}.config.artifact_policy must be a JSON object`);
  if (policy.mode === "all") {
    exactObjectKeys(policy, ["mode"], ["mode"], `${label}.config.artifact_policy`);
    return interfaceId;
  }
  if (policy.mode !== "allow") throw new Error(`${label}.config.artifact_policy.mode must be all or allow`);
  exactObjectKeys(policy, ["mode", "names", "namespaces"], ["mode", "names", "namespaces"], `${label}.config.artifact_policy`);
  if (!Array.isArray(policy.names) || !Array.isArray(policy.namespaces)) throw new Error(`${label}.config.artifact_policy allow lists must be arrays`);
  if (policy.names.length === 0 && policy.namespaces.length === 0) throw new Error(`${label}.config.artifact_policy allow mode must not be empty`);
  for (const [field, values] of [["names", policy.names], ["namespaces", policy.namespaces]]) {
    const seen = new Set();
    values.forEach((entry, index) => {
      if (typeof entry !== "string" || !entry || entry.trim() !== entry || seen.has(entry)) throw new Error(`${label}.config.artifact_policy.${field}[${index}] is invalid or duplicated`);
      if (field === "namespaces" && entry.endsWith(".")) throw new Error(`${label}.config.artifact_policy.namespaces[${index}] must not end with a dot`);
      seen.add(entry);
    });
  }
  return interfaceId;
}

function interfaceBindingId(value) {
  return isPlainRecord(value) ? stringValue(value.interface) : "";
}

function interfaceBindingArtifactNames(value) {
  const policy = objectOrEmpty(objectOrEmpty(objectOrEmpty(value).config).artifact_policy);
  return policy.mode === "allow" ? arrayValue(policy.names).map(stringValue).filter(Boolean) : [];
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function streamFragmentValue(value) {
  return typeof value === "string" ? value : "";
}

function requiredString(value, field) {
  const text = stringValue(value);
  if (!text) throw new Error(`${field} is required`);
  return text;
}

function numberValue(value, fallback) {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

async function interfaceArtifactTransferProjection(octopusHomeRoot, row, readinessCache) {
  const interfaceId = stringValue(row?.rule_id ?? row?.id ?? row?.template_name);
  const activationRefs = interfaceActivationRefs(row?.activation_sidecars);
  const activation = arrayValue(row?.activation_sidecars)
    .flatMap((sidecar) => arrayValue(sidecar?.activations))
    .find((candidate) => stringValue(
      candidate?.activation_driver_ref
      ?? candidate?.activation_transport_ref
      ?? candidate?.transport_ref,
    ) === "app_server_transport");
  if (!activation) {
    return {
      kind: "artifact_hand_in_socket",
      status: "not_declared",
      path: "",
      reason: "no app_server_transport activation declares artifact hand-in",
      activation_refs: activationRefs,
    };
  }
  const safeId = safeInterfacePathSegment(interfaceId);
  if (!safeId) {
    return {
      kind: "artifact_hand_in_socket",
      status: "unavailable",
      path: "",
      reason: "interface id is not a single filesystem path segment",
      activation_refs: activationRefs,
    };
  }
  const config = objectOrEmpty(activation.config);
  const listenEnv = stringValue(config.listen_env);
  const configured = listenEnv ? stringValue(process.env[listenEnv]) : "";
  let socketPath = configured
    ? (path.isAbsolute(configured) ? configured : path.resolve(configured))
    : "";
  if (!socketPath && config.bind_interface_socket_when_env_unset === true) {
    const digest = crypto.createHash("sha256").update(safeId).digest("hex").slice(0, 24);
    socketPath = path.join(octopusHomeRoot, "app-server-control", `${digest}.sock`);
  }
  if (!socketPath && config.bind_conventional_socket_when_env_unset === true) {
    socketPath = path.join(octopusHomeRoot, "app-server-control", "app-server-control.sock");
  }
  if (!socketPath) {
    return {
      kind: "artifact_hand_in_socket",
      status: "not_declared",
      path: "",
      reason: "app_server_transport activation has no live socket path",
      activation_refs: activationRefs,
    };
  }
  let readiness = readinessCache?.get(socketPath);
  if (!readiness) {
    readiness = unixSocketReadiness(socketPath);
    readinessCache?.set(socketPath, readiness);
  }
  readiness = await readiness;
  return {
    kind: "artifact_hand_in_socket",
    ...readiness,
    activation_refs: activationRefs,
  };
}

function interfaceActivationRefs(sidecars) {
  const refs = new Set();
  for (const sidecar of arrayValue(sidecars)) {
    for (const activation of arrayValue(sidecar?.activations)) {
      const ref = stringValue(
        activation?.activation_transport_ref
        ?? activation?.activation_driver_ref
        ?? activation?.transport_ref,
      );
      if (ref) refs.add(ref);
    }
    const ref = stringValue(
      sidecar?.activation_transport_ref
      ?? sidecar?.activation_driver_ref
      ?? sidecar?.transport_ref,
    );
    if (ref) refs.add(ref);
  }
  return Array.from(refs).sort();
}

async function unixSocketReadiness(socketPath) {
  try {
    const stat = await fs.stat(socketPath);
    if (!stat.isSocket()) {
      return {
        status: "unavailable",
        path: socketPath,
        reason: "artifact hand-in path exists but is not a Unix socket",
      };
    }
    await fs.access(socketPath, fsSync.constants.R_OK | fsSync.constants.W_OK);
    await new Promise((resolve, reject) => {
      const socket = net.createConnection(socketPath);
      const finish = (error) => {
        socket.destroy();
        if (error) reject(error);
        else resolve();
      };
      socket.setTimeout(750);
      socket.once("connect", () => finish());
      socket.once("error", finish);
      socket.once("timeout", () => finish(new Error("socket connection timed out")));
    });
    return {
      status: "ready",
      path: socketPath,
      reason: "artifact hand-in socket is ready",
    };
  } catch (error) {
    const code = stringValue(error?.code);
    if (code === "ENOENT") {
      return {
        status: "unavailable",
        path: socketPath,
        reason: "artifact hand-in socket does not exist",
      };
    }
    if (code === "EACCES" || code === "EPERM") {
      return {
        status: "unavailable",
        path: socketPath,
        reason: "artifact hand-in socket is not readable and writable",
      };
    }
    if (code === "ECONNREFUSED") {
      return {
        status: "unavailable",
        path: socketPath,
        reason: "artifact hand-in socket exists without a live listener",
      };
    }
    return {
      status: "unknown",
      path: socketPath,
      reason: String(error?.message ?? error),
    };
  }
}

function safeInterfacePathSegment(value) {
  const id = stringValue(value);
  if (!id || id === "." || id === ".." || id.includes("/") || id.includes("\\")) return "";
  return id;
}
