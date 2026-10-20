const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");

const CLIENT_AGENT_CATALOG_SCHEMA = "octopus.client.agent_catalog";
const CLIENT_AGENT_CATALOG_ICONS = new Set([
  "default",
  "strict",
  "calm",
  "curious",
  "glasses",
  "goggles",
  "visor",
  "eyepiece",
  "headphones",
  "headset",
  "headband",
  "hardhat",
]);

function clientAgentCatalogPath(octopusHomeRoot) {
  return path.join(path.resolve(octopusHomeRoot), "client", "agents", "catalog.json");
}

function emptyClientAgentCatalog() {
  return {
    schema: CLIENT_AGENT_CATALOG_SCHEMA,
    schema_version: 1,
    project_sources: [],
    agents: [],
  };
}

function planClientAgentTemplate({ operationId, projectEeId, sourceInterface, sourcePolicy, sourceProfile, settings, name, icon, now = Date.now() }) {
  if (typeof operationId !== "string" || !/^[A-Za-z0-9_-]{8,96}$/.test(operationId)) throw new Error("Agent operation identity is invalid");
  const sourceId = requiredExactId(sourceInterface?.id, "Agent source worker");
  requiredExactId(projectEeId, "Agent Project EE");
  if (sourceInterface.interface_type !== "system" || sourceInterface.role !== "worker" || sourcePolicy?.id !== sourceId
      || sourceProfile?.worker_choice_interface !== sourceId || sourceProfile.ee_id !== projectEeId) {
    throw new Error("Agent source documents do not identify one Project worker profile");
  }
  const workerInterfaceId = `system_worker_template_${crypto.createHash("sha256").update(`${operationId}\0${projectEeId}\0${sourceId}`).digest("hex").slice(0, 32)}`;
  const agentId = `agent_${crypto.createHash("sha256").update(`${operationId}\0${workerInterfaceId}`).digest("hex").slice(0, 32)}`;
  return {
    workerInterfaceId,
    sourceWorkerInterfaceId: sourceId,
    agentId,
    interface: { ...structuredClone(sourceInterface), id: workerInterfaceId, copy_of: sourceInterface.copy_of ?? sourceId },
    policy: { ...structuredClone(sourcePolicy), id: workerInterfaceId },
    clientTemplate: { ...structuredClone(settings), worker_interface: workerInterfaceId, project_ee_id: projectEeId },
    profile: { ...structuredClone(sourceProfile), worker_choice_interface: workerInterfaceId },
    presentation: {
      id: agentId,
      name: requiredClientAgentName(name),
      icon: requiredClientAgentIcon(icon),
      project_ee_id: projectEeId,
      worker_interface: workerInterfaceId,
      created_at_ms: requiredTimestamp(now, "Agent creation time"),
      updated_at_ms: now,
      deleted_at_ms: 0,
    },
  };
}

function requireAgentSourceAdmission(document, field, sourceWorkerInterfaceId, label) {
  if (!Object.prototype.hasOwnProperty.call(document, field) || document[field] === null) return;
  if (
    !Array.isArray(document[field])
    || document[field].some((value) => typeof value !== "string" || !value || value.trim() !== value)
    || new Set(document[field]).size !== document[field].length
  ) {
    throw new Error(`${label}.${field} must be a unique string array or null`);
  }
  if (!document[field].includes(sourceWorkerInterfaceId)) {
    throw new Error(`${label}.${field} does not admit exact source worker '${sourceWorkerInterfaceId}'`);
  }
}

async function readClientAgentCatalog(octopusHomeRoot) {
  const file = clientAgentCatalogPath(octopusHomeRoot);
  let stat;
  try {
    stat = await fs.lstat(file);
  } catch (error) {
    if (error?.code === "ENOENT") {
      return {
        file,
        exists: false,
        digest: null,
        document: emptyClientAgentCatalog(),
      };
    }
    throw error;
  }
  if (stat.isSymbolicLink() || !stat.isFile()) {
    throw new Error(`Client Agent catalog is not a regular file: ${file}`);
  }
  const bytes = await fs.readFile(file);
  const after = await fs.lstat(file);
  if (
    after.isSymbolicLink()
    || !after.isFile()
    || stat.dev !== after.dev
    || stat.ino !== after.ino
    || stat.size !== after.size
    || stat.mtimeMs !== after.mtimeMs
  ) {
    throw new Error(`Client Agent catalog changed while it was read: ${file}`);
  }
  let value;
  try {
    value = JSON.parse(bytes.toString("utf8"));
  } catch (error) {
    throw new Error(`Client Agent catalog is malformed JSON: ${file}: ${error.message}`);
  }
  return {
    file,
    exists: true,
    digest: crypto.createHash("sha256").update(bytes).digest("hex"),
    document: parseClientAgentCatalog(value, file),
  };
}

function parseClientAgentCatalog(value, file = "Client Agent catalog") {
  exactObjectKeys(
    value,
    ["schema", "schema_version", "project_sources", "agents"],
    ["schema", "schema_version", "project_sources", "agents"],
    file,
  );
  if (
    value.schema !== CLIENT_AGENT_CATALOG_SCHEMA
    || value.schema_version !== 1
    || !Array.isArray(value.project_sources)
    || !Array.isArray(value.agents)
  ) {
    throw new Error(`Client Agent catalog does not satisfy schema v1: ${file}`);
  }
  const projectIds = new Set();
  const projectSources = value.project_sources.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["project_ee_id", "source_worker_interface"],
      ["project_ee_id", "source_worker_interface"],
      `${file} project source ${index}`,
    );
    const projectEeId = requiredExactId(candidate.project_ee_id, `${file} project source ${index} Project EE`);
    const sourceWorkerInterface = requiredExactId(
      candidate.source_worker_interface,
      `${file} project source ${index} worker interface`,
    );
    if (projectIds.has(projectEeId)) {
      throw new Error(`Client Agent catalog repeats Project source '${projectEeId}': ${file}`);
    }
    projectIds.add(projectEeId);
    return {
      project_ee_id: projectEeId,
      source_worker_interface: sourceWorkerInterface,
    };
  });
  const agentIds = new Set();
  const templates = new Set();
  const agents = value.agents.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      [
        "id",
        "name",
        "icon",
        "project_ee_id",
        "worker_interface",
        "created_at_ms",
        "updated_at_ms",
        "deleted_at_ms",
      ],
      [
        "id",
        "name",
        "icon",
        "project_ee_id",
        "worker_interface",
        "created_at_ms",
        "updated_at_ms",
        "deleted_at_ms",
      ],
      `${file} Agent ${index}`,
    );
    const id = requiredClientId(candidate.id, `${file} Agent ${index} id`);
    const name = requiredHumanText(candidate.name, `${file} Agent ${index} name`, 80);
    const icon = requiredAgentIcon(candidate.icon, `${file} Agent ${index} icon`);
    const projectEeId = requiredExactId(candidate.project_ee_id, `${file} Agent ${index} Project EE`);
    const workerInterface = requiredExactId(candidate.worker_interface, `${file} Agent ${index} worker interface`);
    const createdAtMs = requiredTimestamp(candidate.created_at_ms, `${file} Agent ${index} created_at_ms`);
    const updatedAtMs = requiredTimestamp(candidate.updated_at_ms, `${file} Agent ${index} updated_at_ms`);
    const deletedAtMs = requiredTimestamp(candidate.deleted_at_ms, `${file} Agent ${index} deleted_at_ms`);
    const templateKey = `${projectEeId}\0${workerInterface}`;
    if (
      agentIds.has(id)
      || templates.has(templateKey)
      || updatedAtMs < createdAtMs
      || deletedAtMs > 0 && deletedAtMs < updatedAtMs
    ) {
      throw new Error(`Client Agent catalog Agent ${index} has conflicting identity or timestamps: ${file}`);
    }
    agentIds.add(id);
    templates.add(templateKey);
    return {
      id,
      name,
      icon,
      project_ee_id: projectEeId,
      worker_interface: workerInterface,
      created_at_ms: createdAtMs,
      updated_at_ms: updatedAtMs,
      deleted_at_ms: deletedAtMs,
    };
  });
  return {
    schema: CLIENT_AGENT_CATALOG_SCHEMA,
    schema_version: 1,
    project_sources: projectSources,
    agents,
  };
}

function clientAgentCatalogDigest(document) {
  const parsed = parseClientAgentCatalog(document);
  return crypto.createHash("sha256")
    .update(`${JSON.stringify(parsed, null, 2)}\n`)
    .digest("hex");
}

function requiredExactId(value, label) {
  if (
    typeof value !== "string"
    || !value
    || value.trim() !== value
    || value.length > 320
    || value === "."
    || value === ".."
    || /[\\/\u0000-\u001f\u007f]/.test(value)
  ) {
    throw new Error(`${label} must be an exact confined id`);
  }
  return value;
}

function requiredClientId(value, label) {
  if (
    typeof value !== "string"
    || !value
    || value.trim() !== value
    || value.length > 512
    || /[\u0000-\u001f\u007f]/.test(value)
  ) {
    throw new Error(`${label} must be a stable client id`);
  }
  return value;
}

function requiredHumanText(value, label, maximum) {
  if (
    typeof value !== "string"
    || !value
    || value.trim() !== value
    || value.length > maximum
    || /[\u0000-\u001f\u007f]/.test(value)
  ) {
    throw new Error(`${label} must be trimmed text up to ${maximum} characters`);
  }
  return value;
}

function requiredAgentIcon(value, label) {
  if (typeof value !== "string" || !CLIENT_AGENT_CATALOG_ICONS.has(value)) {
    throw new Error(`${label} is not a declared Agent icon`);
  }
  return value;
}

function requiredClientAgentId(value, label = "Client Agent id") {
  return requiredClientId(value, label);
}

function requiredClientAgentName(value, label = "Client Agent name") {
  return requiredHumanText(value, label, 80);
}

function requiredClientAgentIcon(value, label = "Client Agent icon") {
  return requiredAgentIcon(value, label);
}

function requiredTimestamp(value, label) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${label} must be a non-negative integer timestamp`);
  }
  return value;
}

function exactObjectKeys(value, allowed, required, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const allowedSet = new Set(allowed);
  const unknown = Object.keys(value).filter((key) => !allowedSet.has(key));
  const missing = required.filter((key) => !Object.prototype.hasOwnProperty.call(value, key));
  if (unknown.length || missing.length) {
    throw new Error(`${label} has unknown or missing fields`);
  }
}

module.exports = {
  planClientAgentTemplate,
  requireAgentSourceAdmission,
  CLIENT_AGENT_CATALOG_SCHEMA,
  clientAgentCatalogDigest,
  clientAgentCatalogPath,
  emptyClientAgentCatalog,
  parseClientAgentCatalog,
  readClientAgentCatalog,
  requiredClientAgentIcon,
  requiredClientAgentId,
  requiredClientAgentName,
};
