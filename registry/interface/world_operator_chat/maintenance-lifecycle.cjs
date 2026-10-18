const fs = require("node:fs/promises");
const { constants } = require("node:fs");
const crypto = require("node:crypto");
const path = require("node:path");
const { decodeMaintenanceDeclaration } = require("./maintenance-client.cjs");

const MAX_JSON_BYTES = 8 * 1024 * 1024;

async function readMaintenanceTaskTopology(options) {
  const source = object(options.sourceRecord, "source Run");
  const declarations = Object.entries(object(source.interface_state, "source interface state"))
    .filter(([, state]) => state?.maintenance_observer !== undefined);
  if (!declarations.length) return null;
  if (declarations.length !== 1) fail("source has ambiguous maintenance ownership");
  const [owner] = declarations[0];
  const decoded = decodeMaintenanceDeclaration({ run_id: source.id, worker_interface_id: source.worker,
    model_interface_id: owner, chat_interface_id: options.sourceClientInterface }, source);
  const d = decoded.declaration;
  if (source.interface_state[options.sourceClientInterface]?.provider_runtime_interface !== owner) {
    fail("maintenance differs from the exact source client provider");
  }
  const interfaces = new Set(d.lifecycle_ownership.private_interface_ids);
  const tools = new Set(d.lifecycle_ownership.private_tool_ids);
  const registry = await plainRoot(options.registryRoot, "Registry");
  const runsRoot = await plainRoot(options.runRecordsRoot, "Run records");
  const projection = d.projection_run;
  const records = new Map(), targets = new Map();
  for (const [role, declaration] of [["observer", d], ["projection", projection]]) {
    const initial = decoded[role].record;
    const file = path.join(runsRoot, `${component(declaration.run, `${role} Run`)}.json`);
    const current = await readJson(file, `${role} Run`);
    assertCurrentRecord(current.document, initial, declaration.provider_interface, interfaces);
    const entry = { ...current, file, role, provider: declaration.provider_interface, targetRun: initial.id,
      interfaces: recordInterfaces(initial, true) };
    records.set(initial.id, entry);
    targets.set(initial.id, entry);
  }
  const declaredTools = new Set();
  for (const id of interfaces) {
    const descriptor = await descriptorJson(registry, "interface", id);
    if (descriptor.document.id !== id) fail(`private interface '${id}' changed identity`);
    if ([d.worker_interface, projection.worker_interface].includes(id)) {
      const policy = await readJson(path.join(registry, "interface", id, "policy.json"), `worker '${id}' ceiling`);
      if (policy.document.id !== id) fail(`worker '${id}' changed its policy identity`);
      const ceiling = object(policy.document.ceiling, "companion ceiling");
      for (const tool of list(ceiling.tools, "companion tools", true)) declaredTools.add(tool);
      for (const pin of list(ceiling.interfaces, "companion interfaces")) {
        if (!interfaces.has(pin)) fail(`companion ceiling reaches interface '${pin}' outside its inventory`);
      }
    }
  }
  equalSets(tools, declaredTools, "private tool inventory differs from companion ceilings");
  for (const id of tools) {
    const tool = (await descriptorJson(registry, "tool", id)).document;
    if (tool.id !== id || !interfaces.has(tool.interface)) fail(`private tool '${id}' has a foreign door`);
  }
  if (!tools.has(d.publication_tool) || !interfaces.has(d.publication_door_interface)) fail("publication has no exact private owner");
  const collection = (await descriptorJson(registry, "artifact", d.collection_source)).document;
  const entriesRoot = path.join(registry, "artifact", d.collection_source, "entries");
  if (collection.id !== d.collection_source || collection.schema_version !== 1
    || collection.label !== "context.collection" || collection.directory !== "entries"
    || d.collection_entries_root !== entriesRoot) fail("publication collection root changed");
  await plainRoot(entriesRoot, "publication collection entries");
  const publication = (await descriptorJson(registry, "interface", d.publication_door_interface)).document;
  if (publication.interface_driver_config?.private_config?.memory_collection !== d.collection_source
    || publication.interface_driver_config?.host_slots?.registry_artifact_root?.path !== path.join(registry, "artifact")) {
    fail("publication door escapes its declared collection");
  }
  if (typeof options.readWriterRuns !== "function") fail("exact writer reader is unavailable");
  for (const target of targets.values()) {
    const writers = await options.readWriterRuns(target.document, interfaces);
    for (const [id, writer] of writers.records) {
      if (id === source.id || records.has(id)) fail("two targets claim the same writer Run");
      const entry = { ...writer, role: "writer", targetRun: target.document.id };
      records.set(id, entry);
    }
    if (!writers.ids.size) fail(`companion '${target.document.id}' lost its detached writer`);
  }
  assertSourceTerminalRoute(source, d);
  return { declaration: d, records, targets, interfaces, tools };
}

function assertSourceTerminalRoute(source, d) {
  const topology = object(source.interface_state[d.source_provider_interface]?.run_authoring_binding, "source rearm topology");
  const outputs = array(topology.checkpoints, "source templates").filter((cp) =>
    array(cp.outbound_interface_bindings, "source outbound bindings").some((binding) => binding.interface === d.source_relay_interface));
  if (outputs.length !== 1 || outputs[0].role !== "provider_artifact_output"
    || outputs[0].interface_binding?.interface !== d.source_provider_interface) fail("source relay lost its exact terminal owner");
  for (const binding of [outputs[0].interface_binding,
    outputs[0].outbound_interface_bindings.find((binding) => binding.interface === d.source_relay_interface)]) {
    if (canonical(binding.config?.artifact_policy) !== canonical({ mode: "allow", names: [d.terminal_label], namespaces: [] })) {
      fail("source relay admits an undeclared nonterminal output");
    }
  }
  if (!outputs[0].outbound_interface_bindings.some((binding) => binding.interface === d.source_client_interface)) {
    fail("source terminal lost its exact client return");
  }
}

function assertCurrentRecord(record, initial, provider, interfaces) {
  if (record.id !== initial.id || record.ee !== initial.ee || record.worker !== initial.worker) fail("companion current Run changed identity or active EE");
  const state = object(record.interface_state, "companion private state");
  const topology = object(state[provider]?.run_authoring_binding, "companion current rearm topology");
  if (canonical(topology) !== canonical(initial.interface_state?.[provider]?.run_authoring_binding)) {
    fail("companion current rearm topology changed its accepted ownership");
  }
  const ids = new Set();
  for (const cp of array(record.checkpoints, "companion checkpoints")) {
    component(cp.id, "companion checkpoint");
    if (ids.has(cp.id) || cp.run !== record.id) fail("companion checkpoint has conflicting ownership");
    ids.add(cp.id);
  }
  for (const id of list(record.active_checkpoints, "companion active checkpoints", true)) {
    if (!ids.has(id)) fail("companion active checkpoint is absent");
  }
  for (const id of recordInterfaces(record, true)) {
    if (!interfaces.has(id)) fail(`companion binds interface '${id}' outside exact ownership`);
  }
}

function recordInterfaces(record, includeTemplates) {
  const ids = new Set([component(record.worker, "worker")]);
  for (const [id, state] of Object.entries(object(record.interface_state, "interface state"))) {
    ids.add(component(id, "private state owner"));
    if (includeTemplates && state.run_authoring_binding) {
      const topology = object(state.run_authoring_binding, "rearm topology");
      for (const cp of array(topology.checkpoints, "rearm templates")) addEdges(ids, cp);
    }
  }
  for (const cp of array(record.checkpoints, "checkpoints")) addEdges(ids, cp);
  return ids;
}

function addEdges(ids, cp) {
  ids.add(component(cp.interface_binding?.interface, "checkpoint source"));
  for (const binding of array(cp.outbound_interface_bindings, "checkpoint returns")) ids.add(component(binding.interface, "checkpoint return"));
}

async function readTaskCatalogueEvidence(options) {
  const registry = await plainRoot(options.registryRoot, "Registry");
  const documents = new Map();
  for (const record of options.records) {
    for (const [provider, state] of Object.entries(object(record.interface_state, "catalogue Run state"))) {
      if (state.collection_catalogue_adoption === undefined) continue;
      const adoption = object(state.collection_catalogue_adoption, "accepted catalogue adoption");
      fields(adoption, ["schema_version", "scope", "snapshot"], ["schema_version", "scope", "snapshot"], "catalogue adoption");
      if (adoption.schema_version !== 1 || scope(adoption.scope).worker_interface !== record.worker) fail("catalogue capture changed its task worker pin");
      const reference = artifactRef(adoption.snapshot);
      const bindings = array(record.checkpoints, "catalogue checkpoints").filter((cp) =>
        cp.outbound_interface_bindings?.some((binding) => binding.interface === provider))
        .flatMap((cp) => array(cp.artifact_bindings, "catalogue bindings")).filter((binding) => binding.id === reference.id);
      if (!bindings.length || bindings.some((binding) => binding.access?.read === false)) fail("accepted catalogue lost its readable provider binding");
      const file = path.join(registry, "artifact", component(reference.id, "catalogue id"), "artifact.json");
      const captured = await readJson(file, "accepted catalogue");
      const dependency = catalogueProvenanceProjection(captured.document, adoption, options.canonicalContentHash);
      if (documents.has(file) && documents.get(file).digest !== captured.digest) fail("catalogue changed during ownership read");
      documents.set(file, { ...captured, dependency });
    }
  }
  return documents;
}

function catalogueProvenanceProjection(document, adoption, canonicalContentHash) {
  fields(document, ["id", "label", "description", "text", "collection_catalogue"],
    ["id", "label", "description", "text", "collection_catalogue"], "catalogue descriptor");
  const reference = artifactRef(adoption.snapshot);
  const catalogue = object(document.collection_catalogue, "catalogue payload");
  fields(catalogue, ["schema_version", "scope", "author_scope", "limits", "sources", "uninspected_bindings"],
    ["schema_version", "scope", "limits", "sources", "uninspected_bindings"], "catalogue payload");
  if (catalogue.schema_version !== 1 || document.label !== "context.reference" || document.id !== reference.id
    || typeof document.text !== "string" || typeof document.description !== "string"
    || canonical(scope(catalogue.scope)) !== canonical(scope(adoption.scope))) fail("catalogue descriptor differs from its accepted capture");
  if (typeof canonicalContentHash !== "function" || canonicalContentHash(document) !== reference.hash) fail("catalogue content pin changed");
  const unlabelled = { ...document }; delete unlabelled.id;
  if (document.id !== `artifact-collection-catalogue-${canonicalContentHash(unlabelled)}`) fail("catalogue identity differs from its canonical content");
  const limits = object(catalogue.limits, "catalogue limits");
  fields(limits, ["max_sources", "max_entries", "max_snapshot_bytes", "max_scan_bytes"],
    ["max_sources", "max_entries", "max_snapshot_bytes", "max_scan_bytes"], "catalogue limits");
  if (Object.values(limits).some((value) => !Number.isSafeInteger(value) || value <= 0)
    || Buffer.byteLength(JSON.stringify(document)) > limits.max_snapshot_bytes) fail("catalogue limits are malformed");
  let count = 0;
  for (const page of array(catalogue.sources, "catalogue sources")) {
    fields(page, ["source", "description", "snapshot", "entries", "has_more", "cursor", "scope_limited"],
      ["source", "description", "snapshot", "entries", "has_more", "cursor", "scope_limited"], "catalogue source page");
    artifactRef(page.source); hash(page.snapshot);
    if (typeof page.description !== "string" || typeof page.has_more !== "boolean" || typeof page.scope_limited !== "boolean") fail("catalogue page is malformed");
    if (page.cursor !== null) {
      fields(page.cursor, ["snapshot", "after"], ["snapshot", "after"], "catalogue cursor");
      hash(page.cursor.snapshot); text(page.cursor.after);
    }
    for (const entry of array(page.entries, "catalogue entries")) {
      fields(entry, ["pin", "title", "description", "applicability"], ["pin", "title", "description", "applicability"], "catalogue entry");
      fields(entry.pin, ["source", "entry_id", "revision"], ["source", "entry_id", "revision"], "catalogue entry pin");
      artifactRef(entry.pin.source); text(entry.pin.entry_id); hash(entry.pin.revision);
      if ([entry.title, entry.description, entry.applicability].some((value) => typeof value !== "string")) fail("catalogue entry text is malformed");
      count += 1;
    }
  }
  if (catalogue.sources.length > limits.max_sources || count > limits.max_entries) fail("catalogue exceeds its accepted limits");
  for (const binding of array(catalogue.uninspected_bindings, "uninspected bindings")) {
    fields(binding, ["id", "reason"], ["id", "reason"], "uninspected binding"); text(binding.id); text(binding.reason);
  }
  const dependency = structuredClone(document);
  delete dependency.collection_catalogue.scope.worker_interface;
  if (catalogue.author_scope !== undefined) {
    scope(catalogue.author_scope);
    delete dependency.collection_catalogue.author_scope.worker_interface;
  }
  return dependency;
}

function assertMaintenanceNoPendingEffects(topology) {
  for (const entry of topology.targets.values()) {
    const state = object(entry.document.interface_state[entry.provider], "companion provider state");
    const pending = state.pending_handoff;
    for (const name of ["authoring_generation", "context_generation"]) {
      if (state[name] !== undefined && (!Number.isSafeInteger(state[name]) || state[name] < 0)) fail(`companion '${entry.document.id}' lost ${name} evidence`);
    }
    if (pending === undefined && [state.authoring_generation, state.context_generation].some((value) => value > 0)) {
      fail(`companion '${entry.document.id}' lost accepted handoff evidence`, "maintenance_effect_unresolved");
    }
    if (pending !== undefined && (!pending || pending.terminal !== true
      || pending.context_publication !== undefined && pending.context_publication !== null)) {
      fail(`companion '${entry.document.id}' has an unfinished handoff or publication`, "maintenance_effect_unresolved");
    }
  }
}

async function plainRoot(value, label) {
  if (typeof value !== "string" || !path.isAbsolute(value) || path.normalize(value) !== value) fail(`${label} root must be an exact absolute path`);
  if (await fs.realpath(value) !== value) fail(`${label} root contains a symbolic link`);
  const stat = await fs.lstat(value);
  if (!stat.isDirectory() || stat.isSymbolicLink()) fail(`${label} root must be a plain directory`);
  return value;
}

async function descriptorJson(root, kind, id) {
  const directory = path.join(root, kind, component(id, "descriptor id"));
  if (await fs.realpath(directory) !== directory) fail("descriptor ownership contains a symbolic link");
  return readJson(path.join(directory, `${kind}.json`), `${kind} '${id}'`);
}

async function readJson(file, label) {
  const handle = await fs.open(file, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const before = await handle.stat();
    if (!before.isFile() || before.size > MAX_JSON_BYTES) fail(`${label} is not a bounded regular file`);
    const body = await handle.readFile();
    const after = await handle.stat(), named = await fs.lstat(file);
    if ([after, named].some((stat) => stat.isSymbolicLink()
      || ["dev", "ino", "size", "mtimeMs", "ctimeMs"].some((key) => stat[key] !== before[key]))) fail(`${label} changed during read`);
    const document = object(JSON.parse(body.toString("utf8")), label);
    return { document, digest: crypto.createHash("sha256").update(body).digest("hex"), body };
  } finally { await handle.close(); }
}

function scope(value) { fields(value, ["ee_id", "worker_interface"], ["ee_id", "worker_interface"], "capture scope"); component(value.ee_id, "capture EE"); component(value.worker_interface, "capture worker"); return value; }
function artifactRef(value) { fields(value, ["id", "content"], ["id", "content"], "artifact ref"); return { id: text(value.id), hash: hash(value.content) }; }
function hash(value) { if (Array.isArray(value) && value.length === 32 && value.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)) return Buffer.from(value).toString("hex"); if (typeof value === "string" && /^[0-9a-f]{64}$/.test(value)) return value; fail("artifact ref has a malformed content pin"); }
function object(value, label) { if (!value || typeof value !== "object" || Array.isArray(value)) fail(`${label} must be an object`); return value; }
function array(value, label) { if (!Array.isArray(value)) fail(`${label} must be an array`); return value; }
function list(value, label, empty = false) { array(value, label); if (!empty && !value.length || new Set(value).size !== value.length) fail(`${label} must be an exact unique list`); value.forEach(text); return value; }
function fields(value, allowed, required, label) { object(value, label); if (Object.keys(value).some((key) => !allowed.includes(key)) || required.some((key) => !Object.hasOwn(value, key))) fail(`${label} has missing or unknown fields`); }
function text(value) { if (typeof value !== "string" || !value || value.trim() !== value || /[\u0000-\u001f\u007f]/.test(value)) fail("identity must be an exact nonempty string"); return value; }
function component(value, label) { text(value); if (!/^[a-z0-9][a-z0-9_.-]*$/.test(value) || [".", ".."].includes(value)) fail(`${label} must be an exact path component`); return value; }
function canonical(value) { if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`; if (value && typeof value === "object") return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`; return JSON.stringify(value); }
function equalSets(first, second, message) { if (first.size !== second.size || [...first].some((id) => !second.has(id))) fail(message); }
function fail(message, code = "maintenance_lifecycle_invalid") { const error = new Error(`maintenance lifecycle: ${message}`); error.code = code; throw error; }

module.exports = { readMaintenanceTaskTopology, readTaskCatalogueEvidence, catalogueProvenanceProjection, assertMaintenanceNoPendingEffects };
