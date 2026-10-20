const { parseScheduledDueAt } = require("./scheduledTime.cjs");
const { modelServiceTiers } = require("./providerModelTiers.cjs");
const { parseHomeVoiceContext, projectHomeVoiceContext } = require("./homeVoiceProjection.cjs");
const { payloadArtifactBindings } = require("./contentAddressedArtifact.cjs");
const { readContentAddressedArtifactProjection } = require("./contentAddressedArtifact.cjs");
const { artifactReadProjectionForLabel } = require("./boundedJsonProjection.cjs");
const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const crypto = require("node:crypto");
const net = require("node:net");
const path = require("node:path");
const Ajv = require("ajv");
const { blake3 } = require("@noble/hashes/blake3");
const movementSchema = require("../../schemas/movement.schema.json");
const runRecordSchema = require("../../schemas/run_record.schema.json");
const { parseProjectionReadRequest } = require("./projectionReadContract.cjs");
const { operatorActivityRows } = require("./operatorActivity.cjs");
const {
  applyLocalApplicationPack,
  installLocalPackages,
  readLocalPackageCatalog,
  repairLocalPackages,
  removeLocalPackages,
} = require("./localPackages.cjs");
const { readWorkspaceSetupState } = require("./workspaceSetup.cjs");
const {
  clientAgentCatalogDigest,
  clientAgentCatalogPath,
  parseClientAgentCatalog,
  readClientAgentCatalog,
  requiredClientAgentIcon,
  requiredClientAgentId,
  requiredClientAgentName,
  planClientAgentTemplate,
  appendExactAgentWorkerAdmission,
} = require("./clientAgentCatalog.cjs");

const canonicalSchemaValidator = new Ajv({ allErrors: true, strict: false, logger: false });
const movementSchemaValidator = canonicalSchemaValidator.compile(movementSchema);
const runRecordSchemaValidator = canonicalSchemaValidator.compile(runRecordSchema);

class ProjectionEvidenceError extends Error {}

function nativeTurnRecordedInputs(payload) {
  const turn = payload?.turn;
  const exactId = (value) => typeof value === "string" && value.length > 0
    && value.trim() === value && !/[\r\n\0]/.test(value);
  if (!exactId(payload?.threadId) || !exactId(turn?.id) || turn.itemsView !== "full"
    || !["completed", "failed", "interrupted"].includes(turn.status) || !Array.isArray(turn.items)
    || payload._payload_projection?.truncated === true && payload._payload_projection.resolved !== true
    || payload.thread_id !== undefined && payload.thread_id !== payload.threadId
    || payload.turn_id !== undefined && payload.turn_id !== turn.id) return [];
  const inputs = [];
  for (const item of turn.items) {
    if (item?.type !== "userMessage" || item.clientId === null || item.clientId === undefined) continue;
    if (!exactId(item.id) || !exactId(item.clientId) || !Array.isArray(item.content)) return [];
    inputs.push({ thread_id: payload.threadId, turn_id: turn.id, item_id: item.id, client_id: item.clientId });
  }
  return inputs;
}

const DESCRIPTOR_REGISTRIES = [
  { kind: "interface", file: "interface.json", identity: ["id", "rule_id"] },
  { kind: "ee_template", file: "ee_template.json", identity: ["id", "name"] },
  { kind: "worker_environment_kind", file: "worker_environment_kind.json", identity: ["worker_environment_kind_id", "kind", "id"] },
  { kind: "tool", file: "tool.json", identity: ["id", "name"] },
  { kind: "verifier", file: "verifier.json", identity: ["id", "name"] },
  { kind: "artifact", file: "artifact.json", identity: ["id", "label", "name"] },
  { kind: "mark", file: "mark.json", identity: ["name", "id", "kind"] },
];
const CLIENT_EVENT_DATA_MAX_BYTES = 8 * 1024;
const CLIENT_EVENT_DATA_MAX_STRING_LENGTH = 1_024;
const CLIENT_EVENT_DATA_MAX_ARRAY_ITEMS = 12;
const CLIENT_EVENT_DATA_MAX_OBJECT_KEYS = 48;
const CLIENT_EVENT_DATA_MAX_DEPTH = 4;
const CLIENT_REASONING_SUMMARY_MAX_BYTES = 4 * 1024;
const APPLICATION_LOGO_MAX_BYTES = 256 * 1024;
const INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES = 1024 * 1024;
const INTERFACE_PRIVATE_ENVIRONMENT_FILE = "environment.json";
const INTERFACE_PRIVATE_RESERVED_ENV = new Set([
  "OCTOPUS_INTERFACE_ID",
  "OCTOPUS_INTERFACE_DIR",
  "OCTOPUS_INTERFACE_DESCRIPTOR_JSON",
  "OCTOPUS_INTERFACE_HOST_IO_LIMITS",
  "OCTOPUS_VAR_DIR",
  "OCTOPUS_ROOT_SLOTS_JSON",
  "OCTOPUS_MOVEMENTS_JSONL",
  "OCTOPUS_BLOBS_DIR",
  "OCTOPUS_LEDGER_INDEX_SQLITE",
  "TMPDIR",
]);
const FULL_LEDGER_PROJECTION_METHODS = new Set([
  "projection.runtime.topology",
  "projection.runtime.snapshot",
  "projection.ee_instances.list",
  "projection.runs.list",
  "projection.runs.checkpoints.list",
  "projection.runs.get",
  "projection.trace.recent",
  "projection.artifacts.list",
  "projection.marks.list",
  "projection.envelopes.list",
  "projection.interfaces.pending.list",
  "projection.descriptors.artifact_labels.list",
  "projection.movements.list",
]);
let osWorkspaceMutationTail = Promise.resolve();
let applicationPrivateConfigurationMutationTail = Promise.resolve();
const scheduleEditTails = new Map();

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

  function projectionTruthChange(filename) {
    if (filename === null || filename === undefined) return "structure";
    const relative = Buffer.isBuffer(filename) ? filename.toString("utf8") : String(filename);
    const segments = relative.split(/[\\/]/).filter(Boolean);
    if (segments.length === 0) return "structure";
    if (segments.length === 1 && segments[0] === "host.json") return "structure";
    if (segments[0] === "registry") {
      return segments.length <= 3 || (segments.length === 4 && segments[3].endsWith(".json")) ? "policy" : "files";
    }
    if (segments[0] === "state" && segments[1] === "runs") {
      return segments.length === 3 && segments[2].endsWith(".json") ? "runs" : null;
    }
    if (segments[0] === "state" && segments[1] === "ee_instances") {
      return segments.length === 3 && segments[2].endsWith(".json") ? "policy" : null;
    }
    if (segments[0] === "ledger") {
      return segments.length <= 2 && segments[1] === "movements.jsonl"
        ? "ledger"
        : null;
    }
    if (segments[0] === "app-server-control") {
      return segments.length === 2 ? "runs" : null;
    }
    if (segments[0] === "client" && segments[1] === "package-state") {
      if (segments.length === 3 && ["maintenance-lock.sqlite", "maintenance-lock.sqlite-journal", "maintenance-lock.sqlite-wal", "maintenance-lock.sqlite-shm"].includes(segments[2])) return null;
      return "structure";
    }
    if (segments[0] === "client" && segments[1] === "agents") {
      return segments.length === 3 && segments[2] === "catalog.json" ? "structure" : null;
    }
    if (segments[0] === "client-state" && segments[1] === "workspace") {
      if (segments.length === 3 && /^transaction-lock\.sqlite(?:-journal|-wal|-shm)?$/.test(segments[2])) return null;
      return "runs";
    }
    if (segments[0] === "interfaces") {
      if (
        segments.length === 3
        && /^ledger-cursor(?:\.[A-Za-z0-9_-]+)?\.json$/.test(segments[2])
      ) return null;
      if (segments.length === 3 && segments[2].endsWith(".json")) {
        return segments[2] === INTERFACE_PRIVATE_ENVIRONMENT_FILE ? "structure" : "runs";
      }
      return segments[2] === "worker-environments"
        && segments.length === 5
        && ["worker_environment.json", "live_state.json"].includes(segments[4])
        ? "runs"
        : null;
    }
    return null;
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

  function projectionRequiresFullLedger(method, params) {
    if (params.include_ledger === false && [
      "projection.runtime.snapshot",
      "projection.ee_instances.list",
      "projection.runs.list",
      "projection.runs.checkpoints.list",
      "projection.runs.get",
    ].includes(method)) return false;
    return FULL_LEDGER_PROJECTION_METHODS.has(method);
  }

  function projectionMemo(projection, tier, key, create) {
    const cache = tier === "structure" ? projection.structureMemo : projection.ledgerMemo;
    if (cache.has(key)) return cache.get(key);
    const value = create();
    cache.set(key, value);
    return value;
  }


  async function movementRows(projection) {
    return projectMovementRows(projection, projection.ledgerRows);
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

  async function recentMovementRows(projection, limit) {
    const count = Math.max(1, numberValue(limit, 1_000));
    return projectMovementRows(projection, projection.ledgerRows.slice(-count)).slice(0, count);
  }

  function projectMovementRows(projection, sourceRows) {
    const rows = sourceRows.map((row) => {
      const contentHex = contentHashHex(row?.artifact?.content);
      const runtime = objectOrEmpty(objectOrEmpty(row.invocation_metadata).runtime);
      const blobPath = contentHex && row.source_file
        ? path.join(path.dirname(row.source_file), "blobs", contentHex)
        : "";
      return {
        movement_id: `${path.basename(stringValue(row.source_file) || "ledger", ".jsonl")}:${row.source_line}`,
        seq: numberValue(row.seq, null),
        lamport: numberValue(row.lamport, null),
        trace: stringValue(row.trace ?? row.trace_id),
        kind: movementKindText(row.kind),
        recorded_at_unix_ms: numberValue(row.recorded_at_unix_ms ?? row.wall_clock_unix_ms, 0),
        raw_run_id: movementRunId(row),
        raw_ee_id: stringValue(runtime.ee),
        raw_worker: stringValue(runtime.worker),
        artifact_id: ledgerArtifactId(row),
        artifact_label: ledgerArtifactLabel(row),
        content_hash: contentHex,
        blob_path: blobPath,
        blob_size: null,
        from: movementEndpoint(row.from),
        to: movementEndpoint(row.to),
        source_file: stringValue(row.source_file),
        source_line: numberValue(row.source_line, 0),
        raw: rawMovement(row),
      };
    });
    return rows.sort((a, b) => (b.seq ?? -1) - (a.seq ?? -1) || b.recorded_at_unix_ms - a.recorded_at_unix_ms);
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

  function movementPageRequiresIndex(params) {
    return optionalUnsignedInteger(params?.before_seq) !== null
      || optionalUnsignedInteger(params?.after_seq) !== null
      || Boolean(stringValue(params?.query))
      || Number.isFinite(numberValue(params?.recorded_after_unix_ms, NaN))
      || Number.isFinite(numberValue(params?.recorded_before_unix_ms, NaN));
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

  function matchesMovementPage(movement, seq, filter) {
    if (filter.beforeSeq !== null && seq >= filter.beforeSeq) return false;
    if (filter.afterSeq !== null && seq <= filter.afterSeq) return false;
    const recordedAt = indexedUnsignedInteger(movement?.recorded_at_unix_ms);
    if (recordedAt === null) return false;
    if (filter.recordedAfter !== null && recordedAt < filter.recordedAfter) return false;
    if (filter.recordedBefore !== null && recordedAt > filter.recordedBefore) return false;
    if (filter.kinds.size > 0 && !filter.kinds.has(movementBaseKind(movement))) return false;
    if (!filter.query) return true;
    const needle = filter.query;
    return [
      movementKindText(movement?.kind),
      stringValue(movement?.trace),
      ledgerArtifactId(movement),
      ledgerArtifactLabel(movement),
      ...Object.values(objectOrEmpty(movement?.from)).map(stringValue),
      ...Object.values(objectOrEmpty(movement?.to)).map(stringValue),
    ].some((value) => value.toLocaleLowerCase().includes(needle));
  }

  function escapeSqlLike(value) {
    return value.replace(/[\\%_]/g, (character) => `\\${character}`);
  }

  function normalizedMovementKindFilter(params) {
    return [...new Set(arrayValue(params?.kinds).map(stringValue).map((kind) => kind.replace(/\(.+\)$/, "")).filter((kind) => (
      kind
      && kind.length <= 128
      && /^[A-Za-z][A-Za-z0-9_.-]*$/.test(kind)
    )))].sort();
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

  function movementBaseKind(row) {
    return movementKindText(row?.kind).replace(/\(.+\)$/, "");
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

  function isClientSurfaceDocumentPayload(value) {
    return Boolean(
      value
      && typeof value === "object"
      && !Array.isArray(value)
      && value.schema === "octopus.client_surface"
      && value.schema_version === 1,
    );
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

  async function immutableClientAction(row) {
    const manifestHash = contentHashHex(row?.artifact?.content);
    if (!manifestHash || !row.source_file) throw new Error("Client action has no immutable manifest.");
    const blobRoot = path.join(path.dirname(row.source_file), "blobs");
    const manifest = JSON.parse((await readStableContentAddressedArtifactBytes(
      path.join(blobRoot, manifestHash), manifestHash, ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES, "client action manifest",
    )).toString("utf8"));
    if (!manifest || typeof manifest !== "object" || Array.isArray(manifest)
      || manifest.label !== "client.action" || row.artifact.id !== `client.action:${manifestHash}`) {
      throw new Error("Client action identity does not match its immutable manifest.");
    }
    const payloadHash = requiredContentHash(manifest.payload_content_hash, "client action payload_content_hash");
    if (typeof manifest.payload_path !== "string" || !path.isAbsolute(manifest.payload_path)) {
      throw new Error("Client action payload_path must be absolute.");
    }
    const bytes = await readStableContentAddressedArtifactBytes(
      path.join(blobRoot, payloadHash), payloadHash, ARTIFACT_PREVIEW_MAX_BYTES, "client action payload",
    );
    const text = bytes.toString("utf8");
    if (!Buffer.from(text, "utf8").equals(bytes)) throw new Error("Client action payload must be UTF-8 JSON.");
    const { parseClientAction } = await import("./clientActionSemantics.mjs");
    return parseClientAction(JSON.parse(text));
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

  async function openValidatedLedgerIndex(projection) {
    let DatabaseSync;
    try {
      ({ DatabaseSync } = require("node:sqlite"));
    } catch (error) {
      preserveProjectionEvidenceError(error);
      return null;
    }
    const ledgerPath = path.join(projection.durableRoot, "movements.jsonl");
    const indexPath = path.join(projection.durableRoot, "ledger-index.sqlite");
    let database;
    try {
      database = new DatabaseSync(indexPath, { readOnly: true });
      database.exec("PRAGMA query_only = ON; BEGIN");
      const meta = database.prepare(
        "SELECT key, value FROM index_meta WHERE key IN ('schema_version', 'watermark', 'durable_end_offset')",
      ).all();
      const values = new Map(meta.map((row) => [String(row.key), String(row.value)]));
      if (values.get("schema_version") !== "4") {
        database.close();
        return null;
      }
      const watermark = indexedUnsignedInteger(values.get("watermark"));
      const durableEndOffset = indexedUnsignedInteger(values.get("durable_end_offset"));
      const ledgerStat = await fs.stat(ledgerPath);
      if (watermark === null || durableEndOffset === null || durableEndOffset > ledgerStat.size) {
        database.close();
        return null;
      }
      const last = database.prepare("SELECT seq FROM movements ORDER BY seq DESC LIMIT 1").get();
      if (
        watermark === 0
          ? last !== undefined
          : indexedUnsignedInteger(last?.seq) !== watermark - 1
      ) {
        database.close();
        return null;
      }
      return { database, durableEndOffset, ledgerPath, ledgerSize: ledgerStat.size, watermark };
    } catch {
      database?.close();
      return null;
    }
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

  async function projectionLedgerSourceCursor(projection, primarySeq) {
    const sources = [];
    for (const [name, file] of [
      ["movements", path.join(projection.durableRoot, "movements.jsonl")],
    ]) {
      try {
        const stat = await fs.stat(file);
        sources.push({
          name,
          identity: `${stat.dev}:${stat.ino}`,
          mtimeMs: stat.mtimeMs,
          size: stat.size,
        });
      } catch (error) {
        if (error?.code !== "ENOENT") throw error;
        sources.push({ name, identity: "", mtimeMs: 0, size: 0 });
      }
    }
    return {
      identity: JSON.stringify(sources.map((source) => [source.name, source.identity])),
      mtimes: sources.map((source) => source.mtimeMs),
      primary_seq: primarySeq,
      sizes: sources.map((source) => source.size),
    };
  }

  function movementBelongsToRun(row, runId) {
    return movementRunId(row) === runId;
  }

  function movementRunId(row) {
    const metadata = objectOrEmpty(row.invocation_metadata);
    return movementBaseKind(row) === "MarkOutcomeAdmitted"
      ? stringValue(metadata.run)
      : stringValue(objectOrEmpty(metadata.runtime).run);
  }

  function activityDirection(row, runId) {
    if (movementRunId(row) !== runId) return "internal";
    const kind = movementBaseKind(row);
    if (kind === "InterfaceArrival") return "in";
    if (kind === "InterfaceOutbound") return "out";
    return "internal";
  }

  function activityPayloadText(payload) {
    return [payload.text, payload.message, payload.content, payload.summary, payload.status_message]
      .map(stringValue)
      .find(Boolean) || "";
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

  async function artifactAttachmentBytes(payloadBytes, attachmentId) {
    if (typeof attachmentId !== "string" || !/^[0-9a-f]{64}$/.test(attachmentId)) {
      throw new Error("attachment_id must be an exact SHA-256 digest.");
    }
    let payload;
    try {
      payload = JSON.parse(payloadBytes.toString("utf8"));
    } catch {
      throw new Error("The admitted artifact has no attachment manifest.");
    }
    const matches = Array.isArray(payload?.attachments)
      ? payload.attachments.filter((entry) => entry?.payload_content_hash === attachmentId)
      : [];
    if (matches.length !== 1) throw new Error("The requested attachment must match exactly one admitted manifest entry.");
    const attachment = matches[0];
    const filename = requiredString(attachment.name, "attachment.name");
    const mimeType = attachment.mime_type;
    if (typeof mimeType !== "string" || mimeType.length > 160 || !/^[A-Za-z0-9!#$&^_.+-]+\/[A-Za-z0-9!#$&^_.+-]+$/.test(mimeType)) {
      throw new Error("Attachment mime_type is invalid.");
    }
    const payloadPath = requiredString(attachment.payload_path, "attachment.payload_path");
    if (!path.isAbsolute(payloadPath)) throw new Error("Attachment payload_path must be absolute.");
    const size = attachment.size;
    if (!Number.isSafeInteger(size) || size < 0 || size > ARTIFACT_PREVIEW_MAX_BYTES) {
      throw new Error(`Attachment size exceeds the ${ARTIFACT_PREVIEW_MAX_BYTES} byte preview boundary or is invalid.`);
    }
    const stat = await fs.lstat(payloadPath);
    if (stat.isSymbolicLink() || !stat.isFile()) throw new Error("Attachment payload is not a regular file.");
    const handle = await fs.open(payloadPath, fsSync.constants.O_RDONLY | fsSync.constants.O_NOFOLLOW | fsSync.constants.O_NONBLOCK);
    try {
      const before = await handle.stat();
      if (!before.isFile() || before.dev !== stat.dev || before.ino !== stat.ino || before.size !== size) {
        throw new Error("Attachment size or file identity does not match its admitted manifest.");
      }
      const buffer = Buffer.alloc(size + 1);
      let offset = 0;
      while (offset < buffer.length) {
        const { bytesRead } = await handle.read(buffer, offset, buffer.length - offset, offset);
        if (!bytesRead) break;
        offset += bytesRead;
      }
      const after = await handle.stat();
      if (offset !== size || after.size !== size || before.mtimeMs !== after.mtimeMs || before.ctimeMs !== after.ctimeMs) {
        throw new Error("Attachment changed while it was being read.");
      }
      const bytes = buffer.subarray(0, size);
      const contentHash = crypto.createHash("sha256").update(bytes).digest("hex");
      if (contentHash !== attachmentId) throw new Error("Attachment content hash does not match its admitted manifest.");
      return { bytes, contentHash, filename: path.basename(filename), mimeType };
    } finally {
      await handle.close();
    }
  }

  function artifactMovementCoordinates(row) {
    const runtime = objectOrEmpty(objectOrEmpty(row.invocation_metadata).runtime);
    const from = objectOrEmpty(row.from);
    const to = objectOrEmpty(row.to);
    return {
      runId: movementRunId(row),
      interfaceId: stringValue(runtime.interface ?? from.Interface ?? from.interface ?? to.Interface ?? to.interface),
    };
  }

  async function nestedArtifactMedia(payloadBytes) {
    let payload;
    try {
      payload = JSON.parse(payloadBytes.toString("utf8"));
    } catch {
      return null;
    }
    const media = objectOrEmpty(objectOrEmpty(payload).media);
    const mediaPath = stringValue(media.path);
    const mimeType = stringValue(media.mime_type ?? media.mimeType);
    const contentHash = stringValue(media.content_hash ?? media.contentHash).toLowerCase();
    const expectedSize = numberValue(media.size, -1);
    if (!mediaPath && !mimeType && !contentHash && expectedSize < 0) return null;
    if (!path.isAbsolute(mediaPath)) throw new Error("Artifact media.path must be absolute.");
    if (!mimeType || mimeType.length > 160 || /[\u0000-\u001f\u007f]/.test(mimeType)) {
      throw new Error("Artifact media.mime_type is invalid.");
    }
    if (!/^[0-9a-f]{64}$/.test(contentHash)) throw new Error("Artifact media.content_hash must be a SHA-256 digest.");
    const bytes = await readStableArtifactBytes(mediaPath, ARTIFACT_PREVIEW_MAX_BYTES, "artifact media");
    if (expectedSize !== bytes.byteLength) throw new Error("Artifact media.size does not match the admitted file.");
    const actualHash = crypto.createHash("sha256").update(bytes).digest("hex");
    if (actualHash !== contentHash) throw new Error("Artifact media.content_hash does not match the admitted file.");
    return {
      bytes,
      contentHash: actualHash,
      filename: path.basename(mediaPath),
      mimeType,
    };
  }

  async function readStableArtifactBytes(filePath, maximumBytes, label) {
    const stat = await fs.lstat(filePath);
    if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${label} is not a regular file.`);
    if (stat.size < 0 || stat.size > maximumBytes) throw new Error(`${label} exceeds the ${maximumBytes} byte preview boundary.`);
    const handle = await fs.open(filePath, "r");
    try {
      const before = await handle.stat();
      const bytes = await handle.readFile();
      const after = await handle.stat();
      if (bytes.byteLength !== before.size || before.size !== after.size || before.mtimeMs !== after.mtimeMs) {
        throw new Error(`${label} changed while it was being read.`);
      }
      return bytes;
    } finally {
      await handle.close();
    }
  }

  async function readStableContentAddressedArtifactBytes(filePath, expectedHash, maximumBytes, label) {
    const bytes = await readStableArtifactBytes(filePath, maximumBytes, label);
    const actualHash = Buffer.from(blake3(bytes)).toString("hex");
    if (actualHash !== expectedHash) throw new Error(`${label} does not match its content-addressed digest.`);
    return bytes;
  }

  function artifactPayloadFilename(label, payloadPath, payloadBytes) {
    const basename = path.basename(payloadPath);
    if (basename && basename !== "." && basename !== path.sep) return basename;
    const stem = label.replace(/[^A-Za-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "") || "artifact";
    const mimeType = artifactPayloadMimeType(payloadBytes);
    return `${stem}${mimeType === "application/json" ? ".json" : mimeType.startsWith("text/") ? ".txt" : ".bin"}`;
  }

  function artifactPayloadMimeType(payloadBytes) {
    const text = payloadBytes.toString("utf8");
    if (Buffer.from(text, "utf8").equals(payloadBytes)) {
      try {
        JSON.parse(text);
        return "application/json";
      } catch {
        return "text/plain;charset=utf-8";
      }
    }
    return "application/octet-stream";
  }

  function recordCheckpointRowsFor(
    projection,
    runId,
    outcomeIndex = requiredVerifiedArtifactOutcomeIndex(projection),
    recordByRun = null,
  ) {
    const record = recordByRun?.get(runId)
      ?? arrayValue(projection.runRecords).find((row) => row.run_id === runId);
    if (!record) return null;
    return arrayValue(record.checkpoints).map((checkpoint) => ({
      ...checkpoint,
      required_verified_artifacts: arrayValue(checkpoint.required_verified_artifacts).map((required) => ({
        ...required,
        outcome: requiredVerifiedArtifactOutcome(outcomeIndex, runId, required.mark),
      })),
    }));
  }

  function requiredVerifiedArtifactOutcomeIndex(projection) {
    return projectionMemo(projection, "ledger", "requiredVerifiedArtifactOutcomeIndex", () => {
      const outcomes = new Map();
      for (const row of projection.ledgerRows) {
        const runId = movementRunId(row);
        if (!runId) continue;
        const admitted = /^MarkOutcomeAdmitted\((.+)\)$/.exec(stringValue(row.kind));
        if (admitted?.[1]) {
          outcomes.set(`${runId}\u0000${admitted[1]}`, "closed");
          continue;
        }
        if (row.kind !== "VerificationFailed") continue;
        const markId = stringValue(objectOrEmpty(objectOrEmpty(row.invocation_metadata).verification_failure).mark);
        if (!markId) continue;
        const key = `${runId}\u0000${markId}`;
        if (outcomes.get(key) !== "closed") outcomes.set(key, "failed");
      }
      return outcomes;
    });
  }

  function requiredVerifiedArtifactOutcome(outcomeIndex, runId, markId) {
    if (!markId) return "declared";
    return outcomeIndex.get(`${runId}\u0000${markId}`) ?? "declared";
  }

  function runCheckpointRows(projection, runId) {
    return projectionMemo(projection, "ledger", `runCheckpointRows:${runId}`, () => {
      const rows = ledgerRowsForRun(projection, runId).sort((a, b) => rowTimeMs(a) - rowTimeMs(b));
      const checkpoints = new Map();
      for (const row of rows) {
        const runtime = objectOrEmpty(objectOrEmpty(row.invocation_metadata).runtime);
        if (stringValue(runtime.run) !== runId) continue;
        const checkpointId = stringValue(runtime.checkpoint);
        if (!checkpointId) continue;
        const current = checkpoints.get(checkpointId) ?? {
          checkpoint_id: checkpointId,
          label: checkpointId,
          status: "made",
          interface: "",
          out_interfaces: [],
          artifact_label: "",
          artifact_bindings: 0,
          interface_bindings: 0,
          recorded_at_unix_ms: rowTimeMs(row),
        };
        current.artifact_bindings += row.artifact ? 1 : 0;
        if (movementBaseKind(row) === "InterfaceArrival") {
          const interfaceId = interfaceEndpointName(row.from);
          current.interface = interfaceId || current.interface;
          current.artifact_label = ledgerArtifactLabel(row) || current.artifact_label;
        }
        if (movementBaseKind(row) === "InterfaceOutbound") {
          const interfaceId = interfaceEndpointName(row.to);
          if (interfaceId && !current.out_interfaces.includes(interfaceId)) current.out_interfaces.push(interfaceId);
        }
        current.interface_bindings = (current.interface ? 1 : 0) + current.out_interfaces.length;
        checkpoints.set(checkpointId, current);
      }
      return Array.from(checkpoints.values());
    });
  }

  function movementKindText(kind) {
    if (typeof kind === "string") return kind;
    if (kind && typeof kind === "object") {
      for (const [variant, value] of Object.entries(kind)) {
        const text = stringValue(value);
        return text ? `${variant}(${text})` : variant;
      }
    }
    return "";
  }

  function interfaceEndpointName(endpoint) {
    return endpointId(endpoint, "Interface");
  }

  function movementEndpoint(endpoint) {
    const id = stringValue(endpoint?.Interface);
    if (!id || Object.keys(endpoint).length !== 1) {
      throw new ProjectionEvidenceError("ledger movement contains a non-interface endpoint");
    }
    return { kind: "interface", id };
  }

  function rawMovement(row) {
    const { source_file, source_line, envelope_id, ...rest } = row;
    if (rest.artifact && Array.isArray(rest.artifact.content)) {
      rest.artifact = { ...rest.artifact, content: contentHashHex(rest.artifact.content) };
    }
    return rest;
  }

  async function movementPayloadPreview(params) {
    const blobPath = requiredString(params.blob_path, "blob_path");
    const hex = path.basename(blobPath);
    if (!/^[0-9a-f]{64}$/.test(hex) || path.basename(path.dirname(blobPath)) !== "blobs") {
      throw new Error("blob_path must point into a ledger blobs/ directory by content hash");
    }
    let bytes;
    try {
      bytes = await fs.readFile(blobPath);
    } catch (error) {
      if (error?.code === "ENOENT") {
        return { resolved: false, blob_path: blobPath, size: 0, text: "", truncated: false };
      }
      throw error;
    }
    const slice = bytes.subarray(0, MOVEMENT_PAYLOAD_PREVIEW_BYTES);
    const text = slice.toString("utf8");
    const binary = text.includes("\uFFFD");
    return {
      resolved: true,
      blob_path: blobPath,
      size: bytes.byteLength,
      truncated: bytes.byteLength > slice.byteLength,
      text: binary ? "" : text,
      binary,
    };
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

  async function readHostRootRefs(homeRoot) {
    const file = path.join(homeRoot, "host.json");
    const { document } = await readRegularJsonDocument(file, `host manifest ${file}`);
    const keys = Object.keys(document).sort();
    if (keys.length !== 2 || keys[0] !== "host_manifest_version" || keys[1] !== "root_refs") {
      throw new Error(`host manifest has unknown or missing fields: ${file}`);
    }
    if (document.host_manifest_version !== 1) {
      throw new Error(`host manifest has unsupported host_manifest_version '${String(document.host_manifest_version)}': ${file}`);
    }
    if (!isPlainRecord(document.root_refs)) {
      throw new Error(`host manifest root_refs is not an object: ${file}`);
    }
    const entries = Object.entries(document.root_refs).sort(([left], [right]) => left.localeCompare(right));
    for (const [slot, root] of entries) {
      requiredSafeRegistryId(slot, `host manifest root_refs slot in ${file}`);
      if (typeof root !== "string" || !path.isAbsolute(root)) {
        throw new Error(`host manifest root_refs.${slot} is not an absolute path: ${file}`);
      }
    }
    return Object.fromEntries(entries);
  }

  function validateEePolicyProjectionSnapshot(value) {
    exactObjectKeys(value, ["environments", "issues"], ["environments", "issues"], "EE policy projection");
    if (!Array.isArray(value.environments) || !Array.isArray(value.issues)) {
      throw new Error("EE policy projection fields must be arrays");
    }
    if (value.issues.some((issue) => typeof issue !== "string" || !issue.trim())) {
      throw new Error("EE policy projection issues must be non-empty strings");
    }
    const seen = new Set();
    const environments = value.environments.map((row) => {
      exactObjectKeys(
        row,
        ["ee_id", "effective_policy", "policy_ceiling", "workers"],
        ["ee_id", "effective_policy", "policy_ceiling", "workers"],
        "EE policy projection row",
      );
      const eeId = requiredSafeRegistryId(row.ee_id, "EE policy projection ee_id");
      if (seen.has(eeId)) throw new Error(`EE policy projection repeats '${eeId}'`);
      seen.add(eeId);
      return {
        ee_id: eeId,
        formed_envelope: validateProjectedEePolicy(row.effective_policy, `${eeId}.effective_policy`),
        boundary_envelope: validateProjectedEePolicy(row.policy_ceiling, `${eeId}.policy_ceiling`),
        workers: validateProjectedWorkerInterfacePolicies(row.workers, `${eeId}.workers`),
      };
    });
    return { environments, issues: [...value.issues] };
  }

  function validateProjectedWorkerInterfacePolicies(value, label) {
    if (!Array.isArray(value)) throw new Error(`${label} must be an array`);
    let previous = "";
    return value.map((row, index) => {
      exactObjectKeys(
        row,
        ["worker_interface_id", "interface_policy"],
        ["worker_interface_id", "interface_policy"],
        `${label}[${index}]`,
      );
      const workerInterfaceId = requiredSafeRegistryId(
        row.worker_interface_id,
        `${label}[${index}].worker_interface_id`,
      );
      if (previous && previous >= workerInterfaceId) {
        throw new Error(`${label} must be sorted by unique worker_interface_id`);
      }
      previous = workerInterfaceId;
      if (!isPlainRecord(row.interface_policy)) {
        throw new Error(`${label}[${index}].interface_policy must be a JSON object`);
      }
      const interfacePolicy = {};
      for (const [interfaceId, policy] of Object.entries(row.interface_policy)) {
        const exactInterfaceId = requiredSafeRegistryId(
          interfaceId,
          `${label}[${index}].interface_policy interface id`,
        );
        if (!isPlainRecord(policy)) {
          throw new Error(`${label}[${index}].interface_policy.${exactInterfaceId} must be a JSON object`);
        }
        interfacePolicy[exactInterfaceId] = structuredClone(policy);
      }
      return {
        worker_interface_id: workerInterfaceId,
        interface_policy: interfacePolicy,
      };
    });
  }

  function validateProjectedEePolicy(value, label) {
    const fields = [
      "allowed_interfaces",
      "allowed_tools",
      "allowed_worker_environment_kinds",
      "allowed_root_slots",
      "allowed_artifact_verbs",
      "hard_blocked_tools",
    ];
    exactObjectKeys(value, [...fields, "allowed_mark_outcomes"], fields, label);
    for (const field of fields) {
      if (field === "allowed_root_slots" && value[field] === null) continue;
      if (!isSortedUniqueStrings(value[field], false)) {
        throw new Error(`${label}.${field} must be a sorted unique string array`);
      }
    }
    if (value.allowed_artifact_verbs.some((verb) => !["read", "change", "create", "delete"].includes(verb))) {
      throw new Error(`${label}.allowed_artifact_verbs contains an unknown verb`);
    }
    if (Object.prototype.hasOwnProperty.call(value, "allowed_mark_outcomes")
      && value.allowed_mark_outcomes !== null
      && !isSortedUniqueStrings(value.allowed_mark_outcomes, false)) {
      throw new Error(`${label}.allowed_mark_outcomes must be a sorted unique string array`);
    }
    return structuredClone(value);
  }


  async function readRunRecords(runRecordsRoot, warnings = []) {
    const files = await listDirectFiles(runRecordsRoot, isRunRecordJsonPath);
    const records = [];
    const invalidRecords = [];
    for (const file of Array.from(new Set(files)).sort()) {
      let record;
      let recordDigest;
      try {
        const document = await readRegularJsonDocument(file, `run record ${file}`);
        record = document.document;
        recordDigest = document.digest;
      } catch (error) {
        const message = String(error?.message ?? error);
        warnings.push(message);
        invalidRecords.push({ source_path: file, error: message });
        continue;
      }
      const schemaPassed = runRecordSchemaValidator(record);
      let semanticError = "";
      if (schemaPassed) {
        try {
          validateProjectedRunRecordSemantics(record);
        } catch (error) {
          semanticError = String(error?.message ?? error);
        }
      }
      if (!schemaPassed || semanticError) {
        const schemaDetail = schemaPassed ? "" : formatCanonicalSchemaErrors(runRecordSchemaValidator.errors);
        const details = [schemaDetail, semanticError].filter(Boolean).join("; ");
        const detail = details ? `: ${details}` : "";
        const error = `run record does not satisfy the current run_record schema: ${file}${detail}`;
        warnings.push(error);
        invalidRecords.push({ source_path: file, source_digest: recordDigest, error });
        continue;
      }
      const active = new Set(arrayValue(record.active_checkpoints).map(stringValue).filter(Boolean));
      const checkpoints = arrayValue(record.checkpoints).map((checkpoint, index) => {
        const inboundInterface = interfaceBindingId(checkpoint?.interface_binding);
        const outbound = arrayValue(checkpoint?.outbound_interface_bindings)
          .map(interfaceBindingId)
          .filter(Boolean);
        const artifactBindings = arrayValue(checkpoint?.artifact_bindings);
        const grant = objectOrEmpty(checkpoint?.grant);
        const contextGrant = objectOrEmpty(grant.context);
        const checkpointId = stringValue(checkpoint?.id) || `${stringValue(record.id)}:cp:${index + 1}`;
        return {
          checkpoint_id: checkpointId,
          label: `Checkpoint ${index + 1}`,
          status: active.has(stringValue(checkpoint?.id)) ? "projected" : "made",
          interface: inboundInterface,
          interface_artifact_names: interfaceBindingArtifactNames(checkpoint?.interface_binding),
          epoch: numberValue(checkpoint?.epoch, 0),
          out_interfaces: outbound,
          artifact_label: stringValue(artifactBindings[0]?.label ?? artifactBindings[0]?.id),
          artifact_bindings: artifactBindings.length,
          artifact_binding_ids: artifactBindings
            .map((binding) => stringValue(binding?.artifact_id ?? binding?.id))
            .filter(Boolean),
          interface_bindings: (inboundInterface ? 1 : 0) + outbound.length,
          grant_interfaces: capSetStrings(contextGrant.interfaces),
          awaits: objectOrEmpty(checkpoint?.awaits),
          required_verified_artifacts: arrayValue(checkpoint?.required_verified_artifacts).map((row) => ({
            mark: stringValue(row?.mark),
            artifact_label: stringValue(row?.artifact_label),
            verifier: stringValue(row?.verifier),
            params: row?.params ?? {},
          })).filter((row) => row.mark),
        };
      });
      const recordStatus = stringValue(record.status) || "recorded";
      records.push({
        run_id: stringValue(record.id),
        ee_id: stringValue(record.ee),
        worker: stringValue(record.worker),
        status: active.size === 0 ? "settled" : recordStatus,
        record_status: recordStatus,
        active_checkpoint_count: active.size,
        record_path: file,
        record_digest: recordDigest,
        interface_state: objectOrEmpty(record.interface_state),
        archived_client_interface_ids: Object.entries(objectOrEmpty(record.interface_state))
          .filter(([, state]) => state?.client_surface_role === "task" && state.client_archive !== undefined)
          .map(([interfaceId]) => interfaceId),
        checkpoints,
      });
    }
    return { records, invalidRecords };
  }

  function validateProjectedRunRecordSemantics(record) {
    for (const [interfaceId, state] of Object.entries(objectOrEmpty(record.interface_state))) {
      if (state?.client_surface_role !== undefined && state.client_archive !== undefined) {
        readClientTaskArchive(record, interfaceId);
      }
    }
    for (const field of ["id", "ee", "worker", "trace"]) {
      if (!stringValue(record[field])) throw new Error(`/${field} must be a non-empty string`);
    }
    for (const [index, checkpoint] of record.checkpoints.entries()) {
      for (const field of ["id", "run", "trace"]) {
        if (!stringValue(checkpoint[field])) {
          throw new Error(`/checkpoints/${index}/${field} must be a non-empty string`);
        }
      }
      if (checkpoint.interface_binding !== undefined && checkpoint.interface_binding !== null) {
        validateAuthoredInterfaceBinding(checkpoint.interface_binding, `/checkpoints/${index}/interface_binding`);
      }
      arrayValue(checkpoint.outbound_interface_bindings).forEach((binding, bindingIndex) => {
        validateAuthoredInterfaceBinding(
          binding,
          `/checkpoints/${index}/outbound_interface_bindings/${bindingIndex}`,
        );
      });
    }
  }

  function validateEeInstancePolicyNarrowing(value, label) {
    if (value === undefined) return;
    exactObjectKeys(
      value,
      [
        "allowed_tools",
        "allowed_interfaces",
        "allowed_worker_environment_kinds",
        "allowed_mark_outcomes",
        "allowed_root_slots",
        "allowed_artifact_verbs",
        "hard_blocked_tools",
        "interface_policy_overrides",
      ],
      [],
      label,
    );
    for (const field of [
      "allowed_tools",
      "allowed_interfaces",
      "allowed_worker_environment_kinds",
      "allowed_mark_outcomes",
      "allowed_root_slots",
      "hard_blocked_tools",
    ]) {
      if (!Object.prototype.hasOwnProperty.call(value, field)) continue;
      if (value[field] === null && field !== "hard_blocked_tools") continue;
      if (
        !Array.isArray(value[field])
        || value[field].some((item) => typeof item !== "string" || !item || item.trim() !== item)
        || new Set(value[field]).size !== value[field].length
      ) {
        throw new Error(`${label}.${field} must be a unique string array`);
      }
    }
    if (Object.prototype.hasOwnProperty.call(value, "allowed_artifact_verbs")) {
      const verbs = value.allowed_artifact_verbs;
      if (verbs !== null && (
        !Array.isArray(verbs)
        || verbs.some((verb) => !["read", "change", "create", "delete"].includes(verb))
        || new Set(verbs).size !== verbs.length
      )) {
        throw new Error(`${label}.allowed_artifact_verbs is invalid`);
      }
    }
    if (Object.prototype.hasOwnProperty.call(value, "interface_policy_overrides")) {
      if (!isPlainRecord(value.interface_policy_overrides)) {
        throw new Error(`${label}.interface_policy_overrides must be a JSON object`);
      }
      for (const [interfaceId, policy] of Object.entries(value.interface_policy_overrides)) {
        if (!interfaceId || interfaceId.trim() !== interfaceId || !isPlainRecord(policy)) {
          throw new Error(`${label}.interface_policy_overrides contains an invalid interface policy`);
        }
      }
    }
  }

  async function readEeInstanceRecords(eeInstancesRoot, warnings = []) {
    const files = await listDirectFiles(eeInstancesRoot, isEeInstanceJsonPath);
    const records = [];
    for (const file of Array.from(new Set(files)).sort()) {
      let record;
      let recordDigest;
      try {
        const document = await readRegularJsonDocument(file, `EE instance ${file}`);
        record = document.document;
        recordDigest = document.digest;
      } catch (error) {
        warnings.push(String(error?.message ?? error));
        continue;
      }
      if (
        !record
        || typeof record !== "object"
        || Array.isArray(record)
        || !stringValue(record.ee_id)
        || !stringValue(record.template_name)
        || !stringValue(record.goal)
        || (
          Object.prototype.hasOwnProperty.call(record, "config")
          && (!record.config || typeof record.config !== "object" || Array.isArray(record.config))
        )
      ) {
        warnings.push(`EE instance does not satisfy the current state schema: ${file}`);
        continue;
      }
      try {
        exactObjectKeys(
          record,
          ["ee_id", "template_name", "parent_ee_id", "goal", "config", "policy_narrowing"],
          ["ee_id", "template_name", "parent_ee_id", "goal"],
          `EE instance ${file}`,
        );
        validateEeInstancePolicyNarrowing(record.policy_narrowing, `EE instance ${file}.policy_narrowing`);
      } catch (error) {
        warnings.push(String(error?.message ?? error));
        continue;
      }
      records.push({ ...record, record_path: file, record_digest: recordDigest });
    }
    return records;
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

  function projectedFilesystemRootSurfaces({ formation, id, lifecycle, rootBindings, status }) {
    if (lifecycle !== "active") return [];
    const bindings = new Map(rootBindings.map(([name, value]) => [stringValue(name), stringValue(value)]));
    const writableSlots = new Set(arrayValue(formation.writable_root_slots).map(stringValue).filter(Boolean));
    const projectedSlots = [...new Set([
      ...arrayValue(formation.readable_root_slots).map(stringValue).filter(Boolean),
      ...writableSlots,
    ])].sort((left, right) => Number(writableSlots.has(right)) - Number(writableSlots.has(left)) || left.localeCompare(right));
    const seenRoots = new Set();
    let projectedIndex = 0;
    return projectedSlots.flatMap((slot) => {
        const root = bindings.get(slot);
        if (!root || !path.isAbsolute(root) || seenRoots.has(root)) return [];
        seenRoots.add(root);
        const writable = writableSlots.has(slot);
        const first = projectedIndex === 0;
        projectedIndex += 1;
        return [{
          id: `${id}:files:${slot}`,
          kind: "files",
          label: first ? "Files" : rootSlotLabel(slot),
          mode: "direct",
          status,
          protocol: writable ? "filesystem.readwrite" : "filesystem.readonly",
          root,
          read_only: !writable,
          data: { slot, writable },
        }];
      });
  }

  async function projectedGitSurfaces({ id, kindCapabilities, lifecycle, rootBindings, status }) {
    if (lifecycle !== "active" || !kindCapabilities.includes("git")) return [];
    const seenRoots = new Set();
    const candidates = rootBindings.flatMap(([rawSlot, rawRoot]) => {
      const slot = stringValue(rawSlot);
      const root = stringValue(rawRoot);
      if (!slot || !path.isAbsolute(root) || seenRoots.has(root)) return [];
      seenRoots.add(root);
      return [{ root, slot }];
    });
    const exactWorktrees = (await Promise.all(candidates.map(async (candidate) => (
      await isExactGitWorktreeRoot(candidate.root) ? candidate : null
    )))).filter(Boolean);
    return exactWorktrees.map(({ root, slot }, index) => ({
        id: `${id}:changes:${slot}`,
        kind: "changes",
        label: index === 0 ? "Changes" : `Changes · ${rootSlotLabel(slot)}`,
        mode: "direct",
        status,
        protocol: "git.worktree",
        root,
        read_only: false,
        data: { slot },
      }));
  }

  async function isExactGitWorktreeRoot(root) {
    try {
      const marker = await fs.lstat(path.join(root, ".git"));
      return !marker.isSymbolicLink() && (marker.isDirectory() || marker.isFile());
    } catch {
      return false;
    }
  }

  function projectedWorktreeStatusSurfaces({ id, liveState, status }) {
    const phase = stringValue(liveState.phase).toLowerCase();
    if (!["queued", "creating", "setting-up", "worktree-ready", "failed"].includes(phase)) return [];
    return [{
      id: `${id}:worktree`,
      kind: "worktree_status",
      label: "Worktree",
      mode: "evidence",
      status,
      protocol: "worktree.status.v1",
      read_only: true,
      data: {
        phase,
        label: stringValue(liveState.label ?? liveState.name) || id,
        description: stringValue(liveState.description),
        branch: stringValue(liveState.branch),
        worktree_git_root: stringValue(liveState.worktree_git_root),
        worktree_workspace_root: stringValue(liveState.worktree_workspace_root ?? liveState.workspace),
        local_environment_config_path: stringValue(liveState.local_environment_config_path),
        error_message: stringValue(liveState.error_message ?? liveState.error),
        worktree_output_text: stringValue(liveState.worktree_output_text),
        setup_output_text: stringValue(liveState.setup_output_text),
      },
    }];
  }

  function rootSlotLabel(slot) {
    const label = slot.replace(/[_-]+/g, " ").trim();
    return label ? `${label[0].toUpperCase()}${label.slice(1)}` : "Files";
  }

  function validHttpAddress(value) {
    return validAddressWithProtocols(value, ["http:", "https:"]);
  }

  function validWebSocketAddress(value) {
    return validAddressWithProtocols(value, ["ws:", "wss:"]);
  }

  function validAddressWithProtocols(value, protocols) {
    try {
      const parsed = new URL(value);
      return protocols.includes(parsed.protocol)
        && Boolean(parsed.hostname)
        && !parsed.username
        && !parsed.password;
    } catch {
      return false;
    }
  }

  function unavailableViewerSurface({ id, label, protocol, reason, status }) {
    return {
      id: `${id}:${label.toLowerCase().replaceAll(" ", "-")}-unavailable`,
      kind: "runtime",
      label,
      mode: "evidence",
      status,
      protocol,
      read_only: true,
      data: { detail: reason },
    };
  }

  async function projectedTextFile(file, interfaceVarDir, warnings, label) {
    if (!file || !path.isAbsolute(file) || !isPathInside(interfaceVarDir, file)) {
      if (file) warnings.push(`${label} path escapes its interface var directory: ${file}`);
      return "";
    }
    try {
      const [resolvedInterfaceVarDir, resolvedFile] = await Promise.all([
        fs.realpath(interfaceVarDir),
        fs.realpath(file),
      ]);
      if (!isPathInside(resolvedInterfaceVarDir, resolvedFile)) {
        warnings.push(`${label} resolves outside its interface var directory: ${file}`);
        return "";
      }
      const stat = await fs.stat(resolvedFile);
      if (!stat.isFile()) return "";
      const maxBytes = 512 * 1024;
      const start = Math.max(0, stat.size - maxBytes);
      const handle = await fs.open(resolvedFile, "r");
      try {
        const bytes = Buffer.alloc(Math.min(maxBytes, stat.size));
        const { bytesRead } = await handle.read(bytes, 0, bytes.length, start);
        return bytes.subarray(0, bytesRead).toString("utf8");
      } finally {
        await handle.close();
      }
    } catch (error) {
      if (error?.code !== "ENOENT") warnings.push(`${label} cannot be read: ${file}: ${error.message}`);
      return "";
    }
  }

  function validWorkerEnvironmentLiveState(value, id, kind) {
    if (!value || typeof value !== "object" || Array.isArray(value) || value.schema_version !== 1) return false;
    if (stringValue(value.worker_environment_id) !== id) return false;
    if (!stringValue(value.status)) return false;
    const projectedKind = stringValue(value.worker_environment_kind);
    return !projectedKind || projectedKind === kind;
  }

  function workerEnvironmentLiveAvailable(lifecycle, status) {
    return lifecycle === "active"
      && ["active", "running", "online", "ready", "available", "connected"].includes(status);
  }

  function workerEnvironmentCapabilities(formation, kindCapabilities = []) {
    const output = new Set(kindCapabilities);
    for (const slot of arrayValue(formation.readable_root_slots).map(stringValue).filter(Boolean)) output.add(`read:${slot}`);
    for (const slot of arrayValue(formation.writable_root_slots).map(stringValue).filter(Boolean)) output.add(`write:${slot}`);
    const network = stringValue(formation.network_ceiling);
    if (network) output.add(`network:${network}`);
    return [...output];
  }

  function workerEnvironmentWorkspace(liveState, rootBindings) {
    const projected = stringValue(liveState.workspace);
    if (projected) return projected;
    for (const preferred of ["workspace_root", "worktree_root", "workspace", "worktree", "project", "root"]) {
      const match = rootBindings.find(([name]) => name === preferred);
      if (match) return stringValue(match[1]);
    }
    return stringValue(rootBindings[0]?.[1]);
  }

  function workerEnvironmentSession(liveState) {
    const viewer = objectOrEmpty(liveState.viewer);
    const container = objectOrEmpty(liveState.container);
    return stringValue(
      liveState.target_id
      ?? liveState.process_id
      ?? viewer.target_id
      ?? viewer.container_name
      ?? container.id
      ?? container.name,
    );
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

  function normalizeDescriptorRow(registryRoot, registry, dirName, descriptorPath, descriptor) {
    const identity = descriptorIdentity(descriptor, registry.identity, dirName);
    const row = {
      ...descriptor,
      id: stringValue(descriptor.id) || identity,
      name: stringValue(descriptor.name) || stringValue(descriptor.id) || identity,
      descriptor_id: `${registry.kind}:${identity}`,
      descriptor_kind: registry.kind,
      registry: registry.kind,
      registry_dir: dirName,
      descriptor_path: descriptorPath,
      source: "registry",
      source_file: path.relative(path.dirname(registryRoot), descriptorPath),
    };
    if (registry.kind === "interface") {
      row.rule_id = stringValue(descriptor.rule_id ?? descriptor.id) || identity;
      row.template_name = row.rule_id;
      row.source = arrayValue(descriptor.source_names).map(stringValue).find(Boolean) || "registry";
      row.status = "registered";
    }
    if (registry.kind === "worker_environment_kind") {
      row.worker_environment_kind = stringValue(descriptor.kind ?? descriptor.worker_environment_kind_id) || identity;
    }
    return row;
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

  function validateCanonicalMovement(row, file, lineNumber) {
    if (!movementSchemaValidator(row)) {
      throw new ProjectionEvidenceError(
        `ledger movement violates the generated movement schema: ${file}:${lineNumber}: ${formatCanonicalSchemaErrors(movementSchemaValidator.errors)}`,
      );
    }
    if (!stringValue(row.trace)) {
      throw new ProjectionEvidenceError(`ledger movement trace is empty: ${file}:${lineNumber}`);
    }
    return row;
  }

  function formatCanonicalSchemaErrors(errors) {
    return (errors ?? []).map((error) => `${error.instancePath || "/"} ${error.message}`).join("; ");
  }

  function preserveProjectionEvidenceError(error) {
    if (error instanceof ProjectionEvidenceError) throw error;
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

  async function loadLedgerArtifactPayload(row, hex, required = false, projection = null) {
    if (projection) {
      const resolved = await readContentAddressedArtifactProjection({
        blobsRoot: path.join(path.dirname(row.source_file), "blobs"), manifestHash: hex,
        artifactId: ledgerArtifactId(row), label: ledgerArtifactLabel(row), projection,
      });
      return { payload: resolved.payload, cacheable: true, retainedBytes: resolved.projection.projected_bytes };
    }
    const blobPath = path.join(path.dirname(row.source_file), "blobs", hex);
    let bytes;
    try {
      bytes = await readStableContentAddressedArtifactBytes(
        blobPath,
        hex,
        ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES,
        "ledger artifact manifest",
      );
    } catch (error) {
      if (error?.code === "ENOENT" && !required) return { payload: null, cacheable: false, retainedBytes: 0 };
      throw error;
    }
    const text = bytes.toString("utf8");
    let manifest;
    try {
      manifest = JSON.parse(text);
    } catch {
      return { payload: { __raw: text }, cacheable: true, retainedBytes: bytes.byteLength };
    }
    const payloadPath = stringValue(manifest?.payload_path);
    if (!payloadPath) return { payload: manifest, cacheable: true, retainedBytes: bytes.byteLength };
    if (!path.isAbsolute(payloadPath)) {
      throw new Error(`ledger artifact manifest payload_path must be absolute: ${payloadPath}`);
    }
    const expectedPayloadHash = requiredContentHash(
      manifest?.payload_content_hash,
      "ledger artifact manifest payload_content_hash",
    );
    let payloadBytes;
    try {
      payloadBytes = await readStableContentAddressedArtifactBytes(
        path.join(path.dirname(row.source_file), "blobs", expectedPayloadHash),
        expectedPayloadHash,
        ledgerArtifactLabel(row) === "turn.stream" ? 256 * 1024 * 1024 : ARTIFACT_PREVIEW_MAX_BYTES,
        "ledger artifact payload",
      );
    } catch (error) {
      if (error?.code === "ENOENT" && !required) return { payload: manifest, cacheable: false, retainedBytes: 0 };
      throw error;
    }
    const payloadText = payloadBytes.toString("utf8");
    try {
      return { payload: JSON.parse(payloadText), cacheable: true, retainedBytes: bytes.byteLength + payloadBytes.byteLength };
    } catch {
      return { payload: { __raw: payloadText }, cacheable: true, retainedBytes: bytes.byteLength + payloadBytes.byteLength };
    }
  }

  function operatorChatTurnText(payload, direction) {
    if (!payload || typeof payload !== "object") return "";
    if (typeof payload.__raw === "string") return payload.__raw.trim();
    if (direction === "in") {
      const text = stringValue(payload.text);
      if (text) return text;
      const names = arrayValue(payload.attachments).map((item) => stringValue(item?.name)).filter(Boolean);
      return names.length ? names.map((name) => `[attachment] ${name}`).join("\n") : "";
    }
    const data = objectOrEmpty(payload.data);
    const terminal = arrayValue(payload.terminal_replies)[0];
    const text = payload.reply
      ?? (terminal ? terminal.reply : undefined)
      ?? data.text
      ?? payload.text
      ?? payload.message
      ?? payload.error;
    return stringValue(text);
  }

  function operatorChatEventIdentity(payload, eventKind) {
    const item = objectOrEmpty(payload?.item);
    const turn = objectOrEmpty(payload?.turn);
    const turnId = stringValue(
      payload?.turn_id
      ?? payload?.turnId
      ?? turn.id,
    );
    const itemId = stringValue(
      payload?.item_id
      ?? payload?.itemId
      ?? item.id
      ?? payload?.id,
    );
    const lifecycle = stringValue(payload?.lifecycle)
      || (eventKind === "item_started" || eventKind === "turn_started" ? "started" : "")
      || (eventKind === "turn_completed" ? "completed" : "")
      || (eventKind.endsWith("_delta") || eventKind.endsWith("_progress") ? "progress" : "");
    const startedAtMs = numberValue(
      payload?.started_at_ms
      ?? payload?.startedAtMs
      ?? turn.started_at
      ?? turn.startedAt,
      0,
    );
    const completedAtMs = numberValue(
      payload?.completed_at_ms
      ?? payload?.completedAtMs
      ?? turn.completed_at
      ?? turn.completedAt,
      0,
    );
    const durationMs = numberValue(
      payload?.duration_ms
      ?? payload?.durationMs
      ?? turn.duration_ms
      ?? turn.durationMs,
      0,
    );
    return {
      turn_id: turnId,
      item_id: itemId,
      lifecycle,
      started_at_ms: startedAtMs || null,
      completed_at_ms: completedAtMs || null,
      duration_ms: durationMs || null,
    };
  }

  function operatorChatMovementContext(row) {
    const metadata = objectOrEmpty(row?.invocation_metadata);
    const runtime = objectOrEmpty(metadata.runtime);
    return {
      seq: numberValue(row?.seq, 0),
      trace_id: stringValue(row?.trace_id),
      recorded_at_unix_ms: rowTimeMs(row),
      from: row?.from ?? null,
      to: row?.to ?? null,
      metadata: {
        runtime: {
          run: stringValue(runtime.run),
          ee: stringValue(runtime.ee),
          worker: stringValue(runtime.worker),
          checkpoint: stringValue(runtime.checkpoint),
          interface: stringValue(runtime.interface),
          epoch: numberValue(runtime.epoch, 0),
          trace_id: stringValue(runtime.trace_id),
        },
      },
    };
  }

  function operatorChatEventData(payload, eventKind, row) {
    const projected = clientEventDataProjection(payload);
    delete projected.continuation;
    delete projected._movement;
    delete projected._stream;
    if (eventKind === "compaction") delete projected.text;
    if (row?.__stream) {
      return {
        ...projected,
        _event: operatorChatEventIdentity(payload, eventKind),
        _stream: {
          stream_id: row.__stream.stream_id,
          stream_seq: row.__stream.stream_seq,
          byte_start_offset: row.__stream.byte_start_offset,
          byte_end_offset: row.__stream.byte_end_offset,
          source_movement_seq: row.__stream.source_movement_seq,
          recorded_at_unix_ms: row.__stream.recorded_at_unix_ms,
        },
      };
    }
    return {
      ...projected,
      _event: operatorChatEventIdentity(payload, eventKind),
      _movement: operatorChatMovementContext(row),
    };
  }

  function operatorChatProjectedRowId(row, fallback) {
    if (row?.__stream) {
      return `${row.__stream.stream_id}:${row.__stream.stream_seq}`;
    }
    return ledgerArtifactId(row) || fallback;
  }

  function operatorChatExpandedStreamRows(rows, payloads, runId, onHeader) {
    const expanded = [];
    for (const row of rows) {
      if (ledgerArtifactLabel(row) !== "turn.stream") {
        expanded.push(row);
        continue;
      }
      const raw = payloads.get(row)?.__raw;
      if (typeof raw !== "string" || !raw.endsWith("\n")) {
        throw new Error("sealed turn.stream payload must be complete JSONL");
      }
      const bytes = Buffer.from(raw, "utf8");
      if (bytes.byteLength > 256 * 1024 * 1024) throw new Error("sealed turn.stream payload exceeds 256 MiB");
      const records = [];
      let startOffset = 0;
      for (let index = 0; index < bytes.byteLength; index += 1) {
        if (bytes[index] !== 0x0a) continue;
        const rawRow = bytes.subarray(startOffset, index > startOffset && bytes[index - 1] === 0x0d ? index - 1 : index);
        if (rawRow.byteLength === 0) throw new Error("sealed turn.stream contains an empty row");
        let value;
        try {
          value = JSON.parse(rawRow.toString("utf8"));
        } catch (error) {
          throw new Error(`sealed turn.stream contains malformed JSON: ${error.message}`);
        }
        if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("sealed turn.stream row must be an object");
        records.push({ value, startOffset, endOffset: index + 1 });
        startOffset = index + 1;
      }
      const headerRecord = records.shift();
      const header = headerRecord?.value;
      const providerInterface = movementBoundaryInterface(row, "from");
      if (
        header?.type !== "header"
        || header.schema_version !== 1
        || header.seq !== 0
        || stringValue(header.provider_interface) !== providerInterface
        || stringValue(header.run) !== runId
      ) {
        throw new Error("sealed turn.stream header has the wrong provider/run identity");
      }
      const threadId = stringValue(header.thread_id);
      const turnId = stringValue(header.turn_id);
      const openedSourceMovementSeq = header.opened_source_movement_seq;
      const openedAtUnixMs = header.opened_at_unix_ms;
      if (!threadId || !turnId || !Number.isSafeInteger(openedSourceMovementSeq) || openedSourceMovementSeq < 0) {
        throw new Error("sealed turn.stream header identity is malformed");
      }
      if (!Number.isSafeInteger(openedAtUnixMs) || openedAtUnixMs <= 0) throw new Error("sealed turn.stream header time is malformed");
      const streamId = operatorTurnStreamIdentity([providerInterface, runId, threadId, turnId]);
      let expectedSeq = 1;
      let previousTime = openedAtUnixMs;
      let previousSourceMovementSeq = openedSourceMovementSeq;
      let sealed = false;
      for (const record of records) {
        const value = record.value;
        if (sealed) throw new Error("sealed turn.stream contains rows after seal");
        if (value.seq !== expectedSeq) throw new Error("sealed turn.stream sequence is non-contiguous");
        if (!Number.isSafeInteger(value.recorded_at_unix_ms) || value.recorded_at_unix_ms < previousTime) {
          throw new Error("sealed turn.stream time moved backwards");
        }
        if (!Number.isSafeInteger(value.source_movement_seq) || value.source_movement_seq < previousSourceMovementSeq) {
          throw new Error("sealed turn.stream source movement moved backwards");
        }
        const type = stringValue(value.type);
        if (type === "event" || type === "input_recorded") {
          if (type === "input_recorded") validateOperatorRecordedInput(value, header);
          const label = type === "input_recorded" ? "turn.stream" : stringValue(value.label);
          if (!label) throw new Error("sealed turn.stream event has no label");
          const synthetic = {
            ...row,
            seq: value.source_movement_seq,
            source_line: `${row.source_line}:stream:${value.seq}`,
            recorded_at_unix_ms: value.recorded_at_unix_ms,
            artifact: null,
            __stream: {
              stream_id: streamId,
              stream_seq: value.seq,
              byte_start_offset: record.startOffset,
              byte_end_offset: record.endOffset,
              source_movement_seq: value.source_movement_seq,
              recorded_at_unix_ms: value.recorded_at_unix_ms,
              label,
              ...(type === "input_recorded" ? { event_kind: "input_recorded" } : {}),
            },
          };
          payloads.set(synthetic, type === "input_recorded" ? value : value.payload);
          expanded.push(synthetic);
        } else if (type === "semantic_boundary") {
          if (!stringValue(value.label) || !contentHashHex(value.artifact_payload_blake3)) {
            throw new Error("sealed turn.stream semantic boundary is malformed");
          }
        } else if (type === "recovery") {
          if (!Number.isSafeInteger(value.discarded_byte_count) || value.discarded_byte_count <= 0) {
            throw new Error("sealed turn.stream recovery is malformed");
          }
        } else if (type === "seal") {
          if (!stringValue(value.terminal_label) || !contentHashHex(value.terminal_artifact_payload_blake3)) {
            throw new Error("sealed turn.stream seal is malformed");
          }
          sealed = true;
        } else {
          throw new Error(`sealed turn.stream has unknown row type '${type}'`);
        }
        expectedSeq += 1;
        previousTime = value.recorded_at_unix_ms;
        previousSourceMovementSeq = value.source_movement_seq;
      }
      if (!sealed) throw new Error("sealed turn.stream has no seal row");
      onHeader?.(header);
    }
    return expanded;
  }

  function validateOperatorRecordedInput(value, header) {
    const fields = new Set([
      "type", "seq", "recorded_at_unix_ms", "source_movement_seq", "provider_interface", "run",
      "thread_id", "turn_id", "item_id", "client_id", "stage", "source_event", "native_event_at_ms",
    ]);
    if (Object.keys(value).some((field) => !fields.has(field))) {
      throw new Error("turn.stream input_recorded has unknown fields");
    }
    for (const field of ["provider_interface", "run", "thread_id", "turn_id", "item_id", "client_id"]) {
      if (typeof value[field] !== "string" || !value[field] || value[field].trim() !== value[field]
        || /[\r\n\0]/.test(value[field])) {
        throw new Error(`turn.stream input_recorded has invalid ${field}`);
      }
    }
    for (const field of ["provider_interface", "run", "thread_id", "turn_id"]) {
      if (value[field] !== header[field]) throw new Error(`turn.stream input_recorded has wrong ${field}`);
    }
    if (value.stage !== "input_recorded_in_turn"
      || !["item/started", "item/completed", "thread/resume"].includes(value.source_event)) {
      throw new Error("turn.stream input_recorded has invalid observation stage/source");
    }
    if (value.source_event === "thread/resume" ? Object.hasOwn(value, "native_event_at_ms")
      : !Number.isSafeInteger(value.native_event_at_ms) || value.native_event_at_ms < 0) {
      throw new Error("turn.stream input_recorded has invalid native event time");
    }
  }

  function operatorTurnStreamIdentity(parts) {
    const hash = crypto.createHash("sha256");
    for (const part of parts) {
      const bytes = Buffer.from(stringValue(part), "utf8");
      if (bytes.byteLength === 0) throw new Error("turn.stream identity part is empty");
      const length = Buffer.alloc(8);
      length.writeBigUInt64LE(BigInt(bytes.byteLength));
      hash.update(length);
      hash.update(bytes);
    }
    return hash.digest("hex");
  }

  function clientEventDataProjection(value) {
    const source = objectOrEmpty(value);
    const serialized = JSON.stringify(source);
    const originalBytes = Buffer.byteLength(serialized, "utf8");
    if (originalBytes <= CLIENT_EVENT_DATA_MAX_BYTES) return { ...source };
    const payloadProjection = {
      original_bytes: originalBytes,
      truncated: true,
      ...(clientEventHasUnifiedDiff(source) ? { contains_unified_diff: true } : {}),
      ...(clientEventHasProcessOutput(source) ? { contains_process_output: true } : {}),
    };
    const reasoningSummary = clientReasoningSummaryProjection(source.summary);
    const compact = {
      ...objectOrEmpty(compactClientEventValue(source, 0)),
      ...(reasoningSummary.length ? { summary: reasoningSummary } : {}),
    };
    const projected = {
      ...compact,
      _payload_projection: payloadProjection,
    };
    if (Buffer.byteLength(JSON.stringify(projected), "utf8") <= CLIENT_EVENT_DATA_MAX_BYTES) {
      return projected;
    }
    const shallow = reasoningSummary.length ? { summary: reasoningSummary } : {};
    for (const [key, item] of Object.entries(source).slice(0, CLIENT_EVENT_DATA_MAX_OBJECT_KEYS)) {
      if (key === "summary") continue;
      let projectedItem;
      if (item === null || typeof item === "boolean" || typeof item === "number") {
        projectedItem = item;
      } else if (typeof item === "string") {
        projectedItem = item.length > CLIENT_EVENT_DATA_MAX_STRING_LENGTH
          ? `${item.slice(0, CLIENT_EVENT_DATA_MAX_STRING_LENGTH)}…`
          : item;
      }
      if (projectedItem === undefined) continue;
      const candidate = {
        ...shallow,
        [key]: projectedItem,
        _payload_projection: payloadProjection,
      };
      if (Buffer.byteLength(JSON.stringify(candidate), "utf8") <= CLIENT_EVENT_DATA_MAX_BYTES) {
        shallow[key] = projectedItem;
      }
    }
    return {
      ...shallow,
      _payload_projection: payloadProjection,
    };
  }

  function clientEventHasUnifiedDiff(value) {
    const source = objectOrEmpty(value);
    const body = objectOrEmpty(source.body ?? source.result);
    const evidence = objectOrEmpty(body.evidence);
    return [
      source.diff,
      source.unified_diff,
      source.unifiedDiff,
      body.diff,
      body.unified_diff,
      body.unifiedDiff,
      evidence.unified_diff,
      evidence.unifiedDiff,
    ].some((candidate) => typeof candidate === "string" && candidate.length > 0);
  }

  function clientEventHasProcessOutput(value) {
    const source = objectOrEmpty(value);
    const body = objectOrEmpty(source.body ?? source.result ?? objectOrEmpty(source.error).details);
    const output = objectOrEmpty(body.output ?? source.output);
    return [
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
  }

  function clientReasoningSummaryProjection(value) {
    const summaries = arrayValue(value);
    const projected = [];
    let retainedBytes = 0;
    for (const item of summaries) {
      const sourceText = typeof item === "string" ? item : stringValue(item?.text);
      if (!sourceText) continue;
      let retained = "";
      let truncated = false;
      for (const character of sourceText) {
        const characterBytes = Buffer.byteLength(character, "utf8");
        if (retainedBytes + characterBytes > CLIENT_REASONING_SUMMARY_MAX_BYTES) {
          truncated = true;
          break;
        }
        retained += character;
        retainedBytes += characterBytes;
      }
      if (retained) projected.push(truncated ? `${retained}…` : retained);
      if (truncated || retainedBytes >= CLIENT_REASONING_SUMMARY_MAX_BYTES) break;
    }
    return projected;
  }

  function compactClientEventValue(value, depth) {
    if (value === null || typeof value === "boolean" || typeof value === "number") return value;
    if (typeof value === "string") {
      return value.length > CLIENT_EVENT_DATA_MAX_STRING_LENGTH
        ? `${value.slice(0, CLIENT_EVENT_DATA_MAX_STRING_LENGTH)}…`
        : value;
    }
    if (depth >= CLIENT_EVENT_DATA_MAX_DEPTH) {
      if (Array.isArray(value)) return [];
      if (value && typeof value === "object") return {};
      return null;
    }
    if (Array.isArray(value)) {
      return value
        .slice(0, CLIENT_EVENT_DATA_MAX_ARRAY_ITEMS)
        .map((item) => compactClientEventValue(item, depth + 1));
    }
    if (value && typeof value === "object") {
      return Object.fromEntries(
        Object.entries(value)
          .slice(0, CLIENT_EVENT_DATA_MAX_OBJECT_KEYS)
          .map(([key, item]) => [key, compactClientEventValue(item, depth + 1)]),
      );
    }
    return null;
  }

  function operatorChatAssistantItemCompleted(message) {
    const data = objectOrEmpty(message.event_data);
    return ["message", "commentary"].includes(message.event_kind)
      && (message.terminal === true || stringValue(data.lifecycle ?? data._event?.lifecycle) === "completed");
  }

  function operatorChatStreamPositionCovered(folded, next) {
    const covered = objectOrEmpty(objectOrEmpty(folded)._stream);
    const position = objectOrEmpty(objectOrEmpty(next)._stream);
    return Boolean(position.stream_id)
      && position.stream_id === covered.stream_id
      && Number.isSafeInteger(position.stream_seq)
      && position.stream_seq <= covered.stream_seq;
  }

  function appendOperatorChatAssistantMessage(messages, items, message) {
    const data = objectOrEmpty(message.event_data), event = objectOrEmpty(data._event);
    const delta = ["message_delta", "commentary_delta"].includes(message.event_kind);
    const completed = operatorChatAssistantItemCompleted(message);
    const turnId = stringValue(data.turn_id ?? data.turnId ?? event.turn_id);
    const itemId = stringValue(data.item_id ?? data.itemId ?? event.item_id);
    const key = (delta || completed) && turnId && itemId
      ? JSON.stringify([message.run_id, message.interface_id, stringValue(data._activity_origin?.interface_id), turnId, itemId]) : "";
    const previous = key ? items.get(key) : null;
    if (!previous) {
      if (message.event_kind === "message" && !message.text) return;
      if (key) items.set(key, message);
      messages.push(message);
      return;
    }
    if (delta && operatorChatAssistantItemCompleted(previous)) return;
    if (delta && operatorChatStreamPositionCovered(previous.event_data, data)) return;
    const text = delta ? previous.text + message.text : message.terminal && !message.text ? previous.text : message.text;
    const eventData = delta ? {
      ...objectOrEmpty(previous.event_data), ...data,
      _event: { ...objectOrEmpty(previous.event_data?._event), ...event },
    } : data;
    Object.assign(previous, message, { id: previous.id, created_at_ms: previous.created_at_ms, text, event_data: eventData });
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

  function operatorChatActivityPayload(value, label, protocol) {
    const payload = objectOrEmpty(value);
    const tool = stringValue(payload.tool);
    const correlationId = stringValue(payload.call_id ?? payload.correlation_id);
    const toolCall = label === protocol.toolCallLabel && tool;
    const toolResult = tool && correlationId && (
      label.endsWith(".result")
      || typeof payload.ok === "boolean"
    );
    if (toolCall || toolResult) {
      const refused = Boolean(objectOrEmpty(payload.body).refused);
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
    const eventKind = operatorChatEventKind(label, protocol, payload);
    if (eventKind === "generic_activity") {
      return {
        eventKind,
        payload,
        text: activityPayloadText(payload) || label || movementKindText(payload?.kind) || "Recorded activity",
        error: payload.ok === false || Boolean(payload.error),
      };
    }
    return {
      eventKind,
      payload,
      text: eventKind === "compaction"
        ? ""
        : eventKind.endsWith("_delta") || eventKind === "message"
          ? operatorChatEventText(payload, eventKind)
          : operatorChatEventText(payload, eventKind) || activityPayloadText(payload) || label,
      error: payload.ok === false || Boolean(payload.error),
    };
  }

  function operatorChatEventKind(label, protocol, payload) {
    if (label === protocol.terminalOutputLabel) return "message";
    if (label === protocol.errorOutputLabel) return "error";
    if (label === protocol.toolCallLabel) return "dynamic_tool";
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
      "turn.control.failed": "turn_control",
      "turn.interrupted.result": "interrupted_result",
      "account.rate_limits": "rate_limits",
      "hook.result": "hook",
      "goal.status": "goal",
      "operator.approval.request": "approval",
      "operator.elicitation.request": "elicitation",
      "operator.user_input.request": "user_input",
    }[label] || "generic_activity";
  }

  function operatorChatResponseKey(label, payload, protocol) {
    const requestId = stringValue(
      payload?.operator_approval_response?.request_id
      ?? payload?.operator_elicitation_response?.request_id
      ?? payload?.operator_user_input_response?.request_id,
    );
    if (!requestId) return "";
    if (label === protocol.approvalResponseLabel) return `approval:${requestId}`;
    if (label === protocol.elicitationResponseLabel) return `elicitation:${requestId}`;
    if (label === protocol.userInputResponseLabel) return `user_input:${requestId}`;
    return "";
  }

  function operatorChatRequestKey(eventKind, payload) {
    const requestId = stringValue(payload?.request_id);
    return requestId && ["approval", "elicitation", "user_input"].includes(eventKind)
      ? `${eventKind}:${requestId}`
      : "";
  }

  function operatorChatEventText(payload, eventKind) {
    if (eventKind === "compaction") return "";
    if (eventKind === "message_delta" || eventKind === "commentary_delta") return streamFragmentValue(payload?.text);
    if (eventKind === "commentary") return stringValue(payload?.text);
    if (eventKind === "reasoning") {
      return [...arrayValue(payload?.summary), ...arrayValue(payload?.content)]
        .map(stringValue)
        .filter(Boolean)
        .join("\n\n");
    }
    if (["reasoning_delta", "plan_delta", "tool_output_delta", "file_change_delta"].includes(eventKind)) {
      return streamFragmentValue(payload?.delta);
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
    if (eventKind === "warning") return stringValue(payload?.message);
    if (eventKind === "item_started") return stringValue(payload?.item?.type);
    if (eventKind === "file_change") {
      const paths = arrayValue(payload?.changes).map((change) => stringValue(change?.path)).filter(Boolean);
      return paths.join("\n");
    }
    if (eventKind === "goal") return stringValue(payload?.objective);
    if (eventKind === "approval") return stringValue(payload?.title);
    if (eventKind === "elicitation") return stringValue(payload?.message);
    if (eventKind === "user_input") return stringValue(payload?.message) || stringValue(payload?.title);
    if (eventKind === "interrupted_result") {
      const kind = stringValue(payload?.result_kind).replaceAll("_", " ");
      const nativeStatus = stringValue(payload?.native_terminal_status);
      if (nativeStatus) return `${kind || "exact result"} retained · provider turn ${nativeStatus} · not returned to model`;
      return `${kind || "exact result"} admitted after interruption · not returned to model`;
    }
    if (eventKind === "turn_control") {
      const detail = objectOrEmpty(payload?.detail);
      return stringValue(detail.error)
        || stringValue(detail.message)
        || [stringValue(payload?.action), stringValue(payload?.status)].filter(Boolean).join(" · ");
    }
    return operatorChatTurnText(payload, "out");
  }

  function operatorTurnControlOutcome(payload, row) {
    const controlId = stringValue(payload?.control_id);
    const action = stringValue(payload?.action);
    const status = stringValue(payload?.status);
    if (!controlId || !action || !["acknowledged", "failed"].includes(status)) return null;
    return { controlId, action, status, payload, row };
  }

  function operatorTurnControlEndsTurn(outcome) {
    return outcome?.status === "acknowledged" && ["interrupt", "stop"].includes(outcome.action);
  }

  function operatorChatAttachments(payload) {
    return arrayValue(payload?.attachments).map((item, index) => ({
      id: stringValue(item?.payload_content_hash) || `${stringValue(item?.name) || "attachment"}:${index}`,
      name: stringValue(item?.name) || `Attachment ${index + 1}`,
      size: numberValue(item?.size, 0),
      type: stringValue(item?.mime_type) || "application/octet-stream",
      label: stringValue(item?.label) || "operator.file",
      last_modified: numberValue(item?.last_modified, 0),
    }));
  }

  function movementBoundaryInterface(row, endpointKey) {
    return endpointId(row?.[endpointKey], "Interface");
  }

  function octopusOsInterfaceSidecars(projection) {
    return projectionMemo(projection, "structure", "octopusOsInterfaceSidecars", () => new Map(
      descriptorRows(projection, "interface").flatMap((descriptor) => {
      const interfaceId = stringValue(descriptor.id ?? descriptor.rule_id);
      const sidecar = objectOrEmpty(descriptor.octopus_os_sidecar);
      return interfaceId && sidecar.workspace === "octopus_os"
        ? [[interfaceId, sidecar]]
        : [];
      }),
    ));
  }

  function projectedRunInterfaceIds(record) {
    const ids = new Set();
    for (const checkpoint of arrayValue(record?.checkpoints)) {
      if (checkpoint.status !== "projected") continue;
      const inbound = stringValue(checkpoint.interface);
      if (inbound) ids.add(inbound);
      for (const outbound of arrayValue(checkpoint.out_interfaces).map(stringValue).filter(Boolean)) ids.add(outbound);
    }
    return ids;
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
      if (authoringProfile && (
        stringValue(authoringProfile.ee_id) !== stringValue(record.ee_id)
        || stringValue(authoringProfile.worker_choice_interface)
          !== stringValue(sidecar.worker_choice_interface)
      )) {
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

  function lowestCommonEnvironment(eeIds, eeById, errors) {
    if (!eeIds.length) return "";
    const lineages = eeIds.map((eeId) => environmentLineage(eeId, eeById, errors));
    if (lineages.some((lineage) => lineage.length === 0)) return "";
    const remaining = lineages.slice(1).map((lineage) => new Set(lineage));
    return lineages[0].find((candidate) => remaining.every((lineage) => lineage.has(candidate))) ?? "";
  }

  function environmentLineage(eeId, eeById, errors) {
    const lineage = [];
    const seen = new Set();
    let cursor = eeId;
    while (cursor) {
      if (seen.has(cursor)) {
        errors.push(`EE hierarchy cycles at '${cursor}'`);
        return [];
      }
      seen.add(cursor);
      const row = eeById.get(cursor);
      if (!row) {
        errors.push(`Octopus OS client binding references missing EE '${cursor}'`);
        return [];
      }
      lineage.push(cursor);
      cursor = stringValue(row.parent_ee_id);
    }
    return lineage;
  }

  function descendantEnvironmentIds(rootEeId, eeById) {
    const children = new Map();
    for (const [eeId, row] of eeById) {
      const parent = stringValue(row.parent_ee_id);
      if (!parent) continue;
      const entries = children.get(parent) ?? [];
      entries.push(eeId);
      children.set(parent, entries);
    }
    const descendants = new Set();
    const visit = (eeId) => {
      if (descendants.has(eeId)) return;
      descendants.add(eeId);
      for (const child of children.get(eeId) ?? []) visit(child);
    };
    visit(rootEeId);
    return descendants;
  }

  function operatorOsProjectRows(scope, environments) {
    const presentations = new Map(arrayValue(scope.project_presentations).map((presentation) => [
      stringValue(presentation.ee_id),
      presentation,
    ]).filter(([eeId]) => eeId));
    return environments.map((environment) => {
      const presentation = objectOrEmpty(presentations.get(stringValue(environment.ee_id)));
      return {
        ee_id: stringValue(environment.ee_id),
        name: stringValue(presentation.name) || stringValue(environment.goal),
        icon: stringValue(presentation.icon),
        color: stringValue(presentation.color),
        goal: stringValue(environment.goal),
        parent_ee_id: stringValue(environment.parent_ee_id),
        record_digest: stringValue(environment.record_digest),
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

  function applicationPrivateConfigurationProjection(manifest, interfaceIds, descriptorsByInterface, installed) {
    const authoredFields = arrayValue(manifest?.setup?.fields);
    if (!installed) {
      return { status: "not_installed", fields: [], documents: [], error: "" };
    }
    const fields = [];
    const documents = [];
    const errors = [];
    for (const interfaceId of interfaceIds) {
      const configuration = objectOrEmpty(descriptorsByInterface.get(interfaceId)?.interface_private_configuration);
      const status = stringValue(configuration.status);
      if (!configuration.interface_id || status === "invalid") {
        errors.push(stringValue(configuration.error) || `Configuration for '${interfaceId}' could not be verified.`);
        continue;
      }
      try {
        const declarations = arrayValue(configuration.fields);
        const presented = authoredFields.filter((field) => stringValue(field?.interface) === interfaceId);
        const resolved = presented.length
          ? resolveAuthoredApplicationSetupFields(
              authoredFields,
              interfaceId,
              declarations,
              `application '${stringValue(manifest?.id)}' setup`,
            )
          : declarations.map((declaration) => ({
              presentation: {
                label: stringValue(declaration?.name),
                kind: "secret",
                required: declaration?.required === true,
              },
              declaration,
            }));
        if (resolved.length === 0) continue;
        documents.push({
          interface_id: interfaceId,
          status: stringValue(configuration.document_status) || "unavailable",
          digest: stringValue(configuration.document_digest),
        });
        for (const { presentation, declaration: projected } of resolved) {
          fields.push({
            interface_id: interfaceId,
            name: stringValue(projected.name),
            label: stringValue(presentation.label),
            kind: stringValue(presentation.kind),
            required: presentation.required === true,
            configured: projected.configured === true,
            private_saved: projected.private_saved === true,
            source: stringValue(projected.source) || "unknown",
          });
        }
      } catch (error) {
        errors.push(error.message);
      }
    }
    fields.sort((left, right) => left.interface_id.localeCompare(right.interface_id) || left.name.localeCompare(right.name));
    const requiredFields = fields.filter((field) => field.required);
    const conflict = fields.some((field) => field.source === "conflict");
    const provenMissing = requiredFields.some((field) => field.source === "missing");
    const unknown = requiredFields.some((field) => field.source === "unknown");
    if (conflict) errors.push("Saved configuration conflicts with the managed runtime environment. Clear the saved value or update the external configuration.");
    return {
      status: errors.length || conflict
        ? "issue"
        : provenMissing ? "required" : unknown ? "unknown" : "configured",
      fields,
      documents,
      error: errors.join("\n"),
    };
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

  function operatorChatRunProvider(projection, entry) {
    const record = entry.record;
    const unavailable = {
      id: "",
      boundId: "",
      model: "",
      reasoningEffort: "",
      modelCatalogRefreshable: false,
    };
    if (!record) return unavailable;
    const chatState = objectOrEmpty(record.interface_state?.[entry.chatInterfaceId]);
    const selected = entry.providerChoiceInterfaceId;
    const recordedSelection = stringValue(chatState.provider_choice_interface);
    const bound = stringValue(chatState.provider_runtime_interface);
    if (!selected || recordedSelection !== selected || !bound) return unavailable;
    const archivedFrontier = new Set(arrayValue(chatState.client_archive?.active_checkpoints));
    let boundProjected = false;
    for (const checkpoint of arrayValue(record.checkpoints)) {
      if (checkpoint.status !== "projected" && !archivedFrontier.has(checkpoint.checkpoint_id)) continue;
      const interfaces = [checkpoint.interface, ...arrayValue(checkpoint.out_interfaces)];
      if (interfaces.map(stringValue).includes(bound)) boundProjected = true;
    }
    if (!boundProjected) return unavailable;
    const descriptor = projection.descriptorIndexes?.interface?.get(bound);
    const descriptorMetadata = objectOrEmpty(objectOrEmpty(descriptor?.interface_driver_config).private_config);
    const modelCatalogRequestLabel = stringValue(
      objectOrEmpty(descriptorMetadata.model_catalog_operation).request_label,
    );
    const modelCatalogRefreshable = Boolean(modelCatalogRequestLabel) && arrayValue(record.checkpoints).some((checkpoint) => (
      checkpoint.status === "projected"
      && checkpoint.interface === entry.chatInterfaceId
      && arrayValue(checkpoint.out_interfaces).length === 1
      && checkpoint.out_interfaces[0] === bound
      && arrayValue(checkpoint.interface_artifact_names).includes(modelCatalogRequestLabel)
    ));
    return {
      id: selected,
      boundId: bound,
      model: stringValue(descriptorMetadata.model),
      reasoningEffort: stringValue(descriptorMetadata.reasoning_effort),
      serviceTier: stringValue(descriptorMetadata.service_tier),
      modelCatalogRefreshable,
    };
  }

  function operatorChatRuntimeProviderId(entry) {
    return stringValue(
      objectOrEmpty(entry.record?.interface_state?.[entry.chatInterfaceId])
        .provider_runtime_interface,
    );
  }

  function isOperatorChatProviderFailureRow(entry, row) {
    const provider = operatorChatRuntimeProviderId(entry);
    return Boolean(provider)
      && stringValue(row?.kind) === "InterfaceOutbound"
      && movementBoundaryInterface(row, "from") === provider
      && movementBoundaryInterface(row, "to") === entry.chatInterfaceId
      && ledgerArtifactLabel(row) === `${provider}.failure`;
  }

  function operatorChatProviderFailureActivity(entry, row, value) {
    if (!isOperatorChatProviderFailureRow(entry, row)) return null;
    const payload = objectOrEmpty(value);
    const failureClass = requiredString(payload.failure_class, "provider failure class");
    const sourceMovementSeq = indexedUnsignedInteger(payload.source_movement_seq);
    if (sourceMovementSeq === null) {
      throw new Error("provider failure source_movement_seq must be a non-negative integer");
    }
    const details = objectOrEmpty(payload.details);
    return {
      eventKind: "error",
      payload: { ...payload, failure_code: failureClass },
      text: stringValue(details.error)
        || stringValue(details.message)
        || `Provider failed before completing the turn (${failureClass}).`,
      error: true,
    };
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

  function normalizeIndexedProjectionMovement(row, ledgerPath) {
    const seq = indexedUnsignedInteger(row.seq);
    const lamport = indexedUnsignedInteger(row.lamport);
    const recordedAt = indexedUnsignedInteger(row.recorded_at_unix_ms);
    if (seq === null || lamport === null || recordedAt === null) {
      throw new Error("ledger index contains a non-integer movement coordinate");
    }
    const artifactId = stringValue(row.artifact_id);
    const content = contentHashHex(row.artifact_content);
    if (Boolean(artifactId) !== Boolean(content)) {
      throw new ProjectionEvidenceError(`ledger index movement ${seq} carries an incomplete artifact reference`);
    }
    const runtime = {
      run: stringValue(row.runtime_run),
      checkpoint: stringValue(row.runtime_checkpoint),
      interface: stringValue(row.runtime_interface),
    };
    const mark = stringValue(row.mark);
    const from = indexedEndpoint(row.from_kind, row.from_id);
    const to = indexedEndpoint(row.to_kind, row.to_id);
    const canonical = validateCanonicalMovement({
      seq,
      lamport,
      recorded_at_unix_ms: recordedAt,
      trace: stringValue(row.trace),
      kind: mark ? { MarkOutcomeAdmitted: mark } : stringValue(row.kind),
      from,
      to,
      ...(artifactId ? { artifact: { id: artifactId, content: [...Buffer.from(content, "hex")] } } : {}),
    }, ledgerPath, seq + 1);
    return {
      seq,
      lamport,
      recorded_at_unix_ms: recordedAt,
      trace: canonical.trace,
      kind: movementKindText(canonical.kind),
      from,
      to,
      ...(artifactId ? { artifact: { id: artifactId, content } } : {}),
      invocation_metadata: mark ? { run: runtime.run, name: mark } : { runtime },
      runtime,
      envelope_id: `movements:${seq + 1}`,
      source_file: ledgerPath,
      source_line: seq + 1,
    };
  }

  function indexedEndpoint(kindValue, idValue) {
    const id = stringValue(idValue);
    if (kindValue !== "interface" || !id) {
      throw new ProjectionEvidenceError("ledger index contains a non-interface endpoint");
    }
    return { Interface: id };
  }

  function indexedMovementRunId(row) {
    return stringValue(row.runtime_run);
  }

  function indexedUnsignedInteger(value) {
    const number = typeof value === "bigint" ? Number(value) : Number(value);
    return Number.isSafeInteger(number) && number >= 0 ? number : null;
  }

  function optionalUnsignedInteger(value) {
    if (value === undefined || value === null || value === "") return null;
    const parsed = indexedUnsignedInteger(value);
    if (parsed === null) throw new Error("movement cursor and timestamp parameters must be unsigned integers");
    return parsed;
  }

  function optionalActivityAfterSeq(value) {
    return value === -1 ? null : optionalUnsignedInteger(value);
  }

  function compareMovementOrder(left, right) {
    return rowTimeMs(left) - rowTimeMs(right)
      || numberValue(left.seq, 0) - numberValue(right.seq, 0);
  }

  function ledgerRowsForRun(projection, runId) {
    const rows = [];
    for (const source of projection.ledgerRowsByRunSources ?? []) {
      const matching = source instanceof Map ? source.get(runId) : null;
      if (matching) rows.push(...matching);
    }
    return rows;
  }

  function isOperatorChatProviderMovement(entry, row) {
    const provider = operatorChatRuntimeProviderId(entry);
    return Boolean(provider)
      && movementRunId(row) === entry.runId
      && movementBoundaryInterface(row, "from") === provider
      && (row.kind === "InterfaceArrival"
        || row.kind === "InterfaceOutbound" && movementBoundaryInterface(row, "to") === entry.chatInterfaceId);
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

  function operatorChatExecutionScope(entry) {
    const provider = operatorChatRuntimeProviderId(entry);
    const state = objectOrEmpty(entry.record?.interface_state?.[provider]);
    if (state.run_authoring_binding?.run !== entry.runId) return null;
    const client = objectOrEmpty(entry.record?.interface_state?.[entry.chatInterfaceId]);
    return {
      checkpoints: new Set([
        ...arrayValue(client.cycle_checkpoint_ids),
        ...arrayValue(entry.externalInputs).map((input) => stringValue(input.checkpoint_id)),
        ...arrayValue(entry.record?.checkpoints)
          .filter((checkpoint) => checkpoint.status === "projected")
          .map((checkpoint) => stringValue(checkpoint.checkpoint_id)),
      ].filter(Boolean)),
      sourceMovementSeq: state.pending_handoff?.source_movement_seq ?? null,
    };
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

  function operatorActivityMessage(row, payload, protocol) {
    const label = ledgerArtifactLabel(row), kind = operatorChatEventKind(label, protocol, payload);
    return {
      direction: "out", interface_id: movementBoundaryInterface(row, "to"), event_kind: kind,
      event_data: operatorChatEventData(payload, kind, row), text: operatorChatTurnText(payload, "out"),
      error: Boolean(payload?.error) || payload?.ok === false, terminal: label === protocol.terminalOutputLabel,
      artifact_id: ledgerArtifactId(row), movement_seq: row.seq, created_at_ms: rowTimeMs(row),
    };
  }

  function operatorActivityAgent(projection, eeId, workerId) {
    if (projection.clientAgentCatalogError) throw new Error(projection.clientAgentCatalogError);
    const matches = arrayValue(projection.clientAgentCatalog?.document?.agents)
      .filter((agent) => agent.project_ee_id === eeId && agent.worker_interface === workerId);
    if (matches.length > 1) throw new Error(`Activity has conflicting Agent identities: ${eeId}/${workerId}`);
    return matches[0] ?? null;
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

  function readIndexedTaskSearchCandidates(index, targetsBySourceRun, beforeSeq, limit, protocolValue) {
    if (beforeSeq <= 0 || targetsBySourceRun.size === 0) return [];
    const protocol = objectOrEmpty(protocolValue);
    const labels = [protocol.inputLabel, protocol.terminalOutputLabel, protocol.errorOutputLabel]
      .map(stringValue)
      .filter(Boolean);
    if (labels.length !== 3) throw new Error("operator chat protocol does not declare the searchable transcript labels");
    const runIds = [...targetsBySourceRun.keys()];
    const rows = [];
    for (let offset = 0; offset < runIds.length; offset += 96) {
      const chunk = runIds.slice(offset, offset + 96);
      const targetPredicates = [];
      const targetValues = [];
      for (const runId of chunk) {
        const target = targetsBySourceRun.get(runId);
        targetPredicates.push(`(
          (
            runtime_run = ?
          )
          AND (
            (kind = 'InterfaceArrival' AND artifact_label = ? AND from_kind = 'interface' AND from_id = ?)
            OR (kind = 'InterfaceOutbound' AND artifact_label IN (?, ?) AND to_kind = 'interface' AND to_id = ?)
          )
        )`);
        targetValues.push(
          runId,
          protocol.inputLabel,
          target.chatInterfaceId,
          protocol.terminalOutputLabel,
          protocol.errorOutputLabel,
          target.chatInterfaceId,
        );
      }
      const selected = index.database.prepare(
        `SELECT
           seq, lamport, recorded_at_unix_ms, trace, kind, mark,
           from_kind, from_id, to_kind, to_id,
           artifact_id, artifact_label, artifact_content,
           runtime_run, runtime_checkpoint, runtime_interface
         FROM movements
         WHERE seq < ?
           AND artifact_id IS NOT NULL
           AND (${targetPredicates.join("\nOR ")})
         ORDER BY seq DESC
         LIMIT ?`,
      ).all(beforeSeq, ...targetValues, limit);
      rows.push(...selected);
    }
    const bySeq = new Map();
    for (const row of rows) bySeq.set(numberValue(row.seq, -1), row);
    return [...bySeq.values()]
      .sort((left, right) => numberValue(right.seq, 0) - numberValue(left.seq, 0))
      .flatMap((row) => {
        const sourceRunId = indexedMovementRunId(row);
        const target = targetsBySourceRun.get(sourceRunId);
        if (!target) return [];
        const label = stringValue(row.artifact_label);
        const fromInterface = stringValue(row.from_kind).toLowerCase() === "interface" ? stringValue(row.from_id) : "";
        const toInterface = stringValue(row.to_kind).toLowerCase() === "interface" ? stringValue(row.to_id) : "";
        const inbound = stringValue(row.kind) === "InterfaceArrival"
          && label === protocol.inputLabel
          && fromInterface === target.chatInterfaceId;
        const outbound = stringValue(row.kind) === "InterfaceOutbound"
          && [protocol.terminalOutputLabel, protocol.errorOutputLabel].includes(label)
          && toInterface === target.chatInterfaceId;
        return inbound || outbound
          ? [{ row: normalizeIndexedProjectionMovement(row, index.ledgerPath), target }]
          : [];
      })
      .slice(0, limit);
  }

  function operatorChatSearchMessage(row, target, protocol, payload) {
    const label = ledgerArtifactLabel(row);
    const createdAtMs = rowTimeMs(row);
    const movementSeq = numberValue(row.seq, 0);
    const artifactId = ledgerArtifactId(row);
    if (row.kind === "InterfaceArrival" && label === protocol.inputLabel) {
      const text = operatorChatTurnText(payload, "in");
      if (!text) return null;
      return {
        id: stringValue(payload?.message_id) || artifactId || `${target.sourceRunId}:in:${movementSeq}`,
        run_id: target.sourceRunId,
        role: "user",
        label,
        direction: "in",
        event_kind: "message",
        event_data: {},
        text,
        attachments: operatorChatAttachments(payload),
        error: false,
        terminal: true,
        created_at_ms: createdAtMs,
        movement_seq: movementSeq,
        artifact_id: artifactId,
        content_hash: contentHashHex(row?.artifact?.content),
        interface_id: target.chatInterfaceId,
        latency_ms: 0,
      };
    }
    if (row.kind !== "InterfaceOutbound") return null;
    const eventKind = operatorChatEventKind(label, protocol, payload);
    const text = operatorChatEventText(payload, eventKind);
    if (!text) return null;
    return {
      id: artifactId || `${target.sourceRunId}:out:${movementSeq}`,
      run_id: target.sourceRunId,
      role: "assistant",
      label,
      direction: "out",
      event_kind: eventKind,
      event_data: {},
      text,
      attachments: [],
      error: label === protocol.errorOutputLabel,
      terminal: true,
      created_at_ms: createdAtMs,
      movement_seq: movementSeq,
      artifact_id: artifactId,
      content_hash: contentHashHex(row?.artifact?.content),
      interface_id: target.chatInterfaceId,
      latency_ms: 0,
    };
  }

  function taskSearchSnippet(value, normalizedIndex, needleLength) {
    const compact = value.replace(/\s+/g, " ").trim();
    const start = Math.max(0, normalizedIndex - 54);
    const end = Math.min(compact.length, normalizedIndex + needleLength + 90);
    return `${start ? "…" : ""}${compact.slice(start, end)}${end < compact.length ? "…" : ""}`;
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

  async function readChangeJournal(projection, runId) {
    try {
      const plan = await changeJournalPlan(projection.octopusHome, projection.registryRoot, runId);
      if (!plan) return null;
      const { writes, ...journal } = plan;
      return journal;
    } catch (error) {
      return { run_id: runId, error: String(error?.message ?? error) };
    }
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

  function projectionSnapshot(projection) {
    return projectionMemo(projection, "ledger", "projectionSnapshot", () => {
      const counts = {};
      for (const registry of DESCRIPTOR_REGISTRIES) {
        counts[registry.kind] = descriptorRows(projection, registry.kind).length;
      }
      const latestMovement = projection.ledgerRows.reduce((latest, row) => {
        const at = numberValue(row.recorded_at_unix_ms ?? row.wall_clock_unix_ms, 0);
        if (!latest || at > latest.recorded_at_unix_ms || (at === latest.recorded_at_unix_ms && numberValue(row.seq, 0) > latest.seq)) {
          return { seq: numberValue(row.seq, 0), recorded_at_unix_ms: at };
        }
        return latest;
      }, null);
      return {
        projection: {
          generated_at: projection.generatedAt,
          generation: projection.generation,
          repo_root: projection.repoRoot,
          octopus_home: projection.octopusHome,
          registry_root: projection.registryRoot,
          state_root: projection.stateRoot,
          runs_root: projection.runRecordsRoot,
          durable_root: projection.durableRoot,
          ledger_path: path.join(projection.durableRoot, "movements.jsonl"),
          descriptor_counts: counts,
          ledger_loaded: Boolean(projection.ledgerRevision),
          ledger_rows: projection.ledgerRows.length,
          latest_movement_seq: latestMovement?.seq ?? null,
          latest_movement_recorded_at_unix_ms: latestMovement?.recorded_at_unix_ms ?? null,
          invalid_run_records: projection.invalidRunRecords,
          invalid_ledger_rows: projection.invalidLedgerRows,
          warnings: arrayValue(projection.warnings),
        },
      };
    });
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

  function baseInterfaceRows(projection) {
    return descriptorRows(projection, "interface");
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

  function interfaceActivationEntries(sidecars) {
    const rows = [];
    for (const sidecar of arrayValue(sidecars)) {
      for (const activation of arrayValue(sidecar?.activations)) {
        const ref = stringValue(
          activation?.activation_transport_ref
          ?? activation?.activation_driver_ref
          ?? activation?.transport_ref,
        );
        rows.push({
          ref,
          run: stringValue(activation?.run),
          descriptor_path: stringValue(sidecar?.descriptor_path),
          config: objectOrEmpty(activation?.config),
        });
      }
    }
    return rows.filter((row) => row.ref);
  }

  function interfaceArtifactTransferTargetRow(args) {
    return {
      target_id: [
        args.interfaceId,
        stringValue(args.activation?.ref) || "no_transport",
        stringValue(args.activation?.descriptor_path) || "no_descriptor",
      ].join(":"),
      interface_id: args.interfaceId,
      interface_type: stringValue(args.iface?.interface_type),
      role: stringValue(args.iface?.role),
      direction: stringValue(args.iface?.direction),
      activation_transport_ref: stringValue(args.activation?.ref),
      transfer_path: stringValue(args.transferPath),
      status: stringValue(args.status) || "unknown",
      reason: stringValue(args.reason),
      source: "interface_artifact_transfer_protocol",
    };
  }

  function summarizeInterfaceArtifactTransferTargets(targets, transfer) {
    const ready = targets.filter((row) => stringValue(row.status) === "ready").length;
    if (ready > 0) {
      return {
        status: "ready",
        target_count: targets.length,
        ready_targets: ready,
        reason: `${ready} artifact hand-in socket target${ready === 1 ? "" : "s"} ready`,
      };
    }
    const first = targets[0];
    if (first) {
      return {
        status: stringValue(first.status) || "unknown",
        target_count: targets.length,
        ready_targets: 0,
        reason: stringValue(first.reason),
      };
    }
    return {
      status: stringValue(transfer.status) || "unknown",
      target_count: 0,
      ready_targets: 0,
      reason: stringValue(transfer.reason),
    };
  }

  function eeTemplateRows(projection) {
    return descriptorRows(projection, "ee_template").map((row) => ({
      ...row,
      name: stringValue(row.id ?? row.name),
      source: stringValue(row.source) || "registry",
      goal: stringValue(row.goal ?? row.goal_class ?? row.id),
    }));
  }

  function eeTemplateDescriptorRows(projection) {
    return eeTemplateRows(projection).map((row) => ({
      ...row,
      descriptor_id: stringValue(row.descriptor_id) || `ee_template:${row.name}`,
    }));
  }

  function eeInstanceRows(projection, includeLedger = true) {
    return projectionMemo(
      projection,
      includeLedger ? "ledger" : "structure",
      includeLedger ? "eeInstanceRows" : "eeInstanceRows.structure",
      () => {
    const byEe = new Map();
    const templates = new Map(descriptorRows(projection, "ee_template").map((candidate) => [
      stringValue(candidate.id ?? candidate.name),
      candidate,
    ]));
    for (const record of arrayValue(projection.eeInstances)) {
      const eeId = stringValue(record.ee_id);
      if (!eeId) continue;
      const templateName = stringValue(record.template_name);
      const template = templates.get(templateName);
      byEe.set(eeId, {
        ee_id: eeId,
        template_name: templateName,
        status: "projected",
        goal_id: "",
        goal: stringValue(record.goal),
        parent_ee_id: stringValue(record.parent_ee_id),
        created_at: "",
        projection_kind: "state.ee_instance",
        runtime_state: "recorded",
        run_count: 0,
        source: "ee_instance",
        record_path: stringValue(record.record_path),
        record_digest: stringValue(record.record_digest),
        config: objectOrEmpty(record.config),
        policy_narrowing: objectOrEmpty(record.policy_narrowing),
        ee_template_descriptor: template ?? {},
      });
    }
    for (const run of runRows(projection, includeLedger)) {
      const eeId = stringValue(run.ee_id);
      if (!eeId) continue;
      const current = byEe.get(eeId) ?? {
        ee_id: eeId,
        template_name: "",
        status: "projected",
        goal_id: "",
        goal: "",
        parent_ee_id: "",
        created_at: stringValue(run.created_at),
        projection_kind: "run.ee_reference",
        runtime_state: "projected_from_run_evidence",
        run_count: 0,
        source: "run_evidence",
      };
      current.run_count += 1;
      if (current.source !== "ee_instance" && run.source === "run_record") current.status = "projected";
      if (!current.created_at) current.created_at = stringValue(run.created_at);
      byEe.set(eeId, current);
    }
    return Array.from(byEe.values());
      },
    );
  }

  function goalRows() {
    return [];
  }

  function workerRows(projection) {
    return descriptorRows(projection, "interface")
      .filter((row) => (
        stringValue(row.interface_type).toLowerCase() === "system"
        && stringValue(row.role).toLowerCase() === "worker"
      ))
      .map((row) => {
        const policy = objectOrEmpty(row.worker_policy);
        const ceiling = objectOrEmpty(policy.ceiling);
        const policyStatus = stringValue(row.worker_policy_status);
        return {
          worker: stringValue(row.rule_id ?? row.id),
          status: policyStatus === "registered" ? "registered" : `${policyStatus || "missing"}_policy`,
          ceiling_interfaces_count: arrayValue(ceiling.interfaces).length,
          ceiling_tools_count: arrayValue(ceiling.tools).length,
          ceiling_worker_environment_kinds_count: arrayValue(ceiling.wek).length,
          ceiling_mark_outcomes_count: arrayValue(ceiling.mark_outcomes).length,
          role_artifacts_count: arrayValue(policy.role_artifacts).length,
          policy_path: stringValue(row.worker_policy_path),
          interface_descriptor_path: stringValue(row.descriptor_path),
          source: "registry.interface",
          interface: row,
          policy: row.worker_policy ?? null,
        };
      });
  }

  function workerEnvironmentRows(projection) {
    const rows = arrayValue(projection.workerEnvironments);
    if (!arrayValue(projection.ledgerRows).length) return rows;
    return projectionMemo(projection, "ledger", "workerEnvironmentRows", () => {
      const wanted = new Set(rows.flatMap((row) => [row.opened_at_movement_seq, row.last_activity_movement_seq])
        .filter(Number.isSafeInteger));
      const recordedAt = new Map();
      for (const movement of projection.ledgerRows) {
        const seq = movement?.seq;
        if (wanted.has(seq)) recordedAt.set(seq, numberValue(movement.recorded_at_unix_ms ?? movement.wall_clock_unix_ms, 0) || null);
      }
      return rows.map((row) => ({
        ...row,
        opened_at_unix_ms: recordedAt.get(row.opened_at_movement_seq) ?? null,
        last_activity_at_unix_ms: recordedAt.get(row.last_activity_movement_seq) ?? null,
      }));
    });
  }

  function bindingRows(projection) {
    return projectionMemo(projection, "structure", "bindingRows", () => {
      const rows = [];
      for (const record of arrayValue(projection.runRecords)) {
        for (const checkpoint of arrayValue(record.checkpoints)) {
          const common = {
            run_id: stringValue(record.run_id),
            ee_id: stringValue(record.ee_id),
            worker: stringValue(record.worker),
            checkpoint_id: stringValue(checkpoint.checkpoint_id),
            epoch: numberValue(checkpoint.epoch, 0),
            status: stringValue(checkpoint.status) || "projected",
            projection_kind: "run.checkpoint.interface_binding",
          };
          const inbound = stringValue(checkpoint.interface);
          if (inbound) {
            rows.push({
              ...common,
              interface_id: inbound,
              direction: "inbound",
            });
          }
          for (const outbound of arrayValue(checkpoint.out_interfaces).map(stringValue).filter(Boolean)) {
            rows.push({
              ...common,
              interface_id: outbound,
              direction: "outbound",
            });
          }
        }
      }
      return rows;
    });
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

  function compareArtifactMovementNewest(left, right) {
    return numberValue(right?.seq, -1) - numberValue(left?.seq, -1)
      || timestampOf(right) - timestampOf(left)
      || numberValue(right?.source_line, 0) - numberValue(left?.source_line, 0)
      || stringValue(left?.source_file).localeCompare(stringValue(right?.source_file));
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

  function markRows(projection) {
    return projectionMemo(projection, "ledger", "markRows", () => {
      const runs = new Map(arrayValue(projection.runRecords).map((record) => [record.run_id, record]));
      return projection.ledgerRows.flatMap((row) => {
      const match = /^MarkOutcomeAdmitted\((.+)\)$/.exec(stringValue(row.kind));
      if (!match) return [];
      const metadata = objectOrEmpty(row.invocation_metadata);
      const runId = movementRunId(row);
      const run = runs.get(runId);
      return [{
        movement_id: `${path.basename(stringValue(row.source_file) || "ledger", ".jsonl")}:${numberValue(row.source_line, 0)}`,
        mark_name: match[1],
        status: "admitted",
        run_id: runId,
        ee_id: stringValue(run?.ee_id),
        worker: stringValue(run?.worker),
        checkpoint_id: "",
        interface_id: "",
        artifact_id: ledgerArtifactId(row),
        created_at: timestampOf(row),
        seq: numberValue(row.seq, null),
        source: "ledger",
        raw: row,
      }];
      });
    });
  }

  function envelopeRows(projection) {
    return projectionMemo(projection, "ledger", "envelopeRows", () => projection.ledgerRows.map((row) => ({
      ...row,
      ts: timestampOf(row),
    })));
  }

  function pendingArtifactRows(projection) {
    return envelopeRows(projection)
      .filter((row) => ["pending", "open", "in_flight"].includes(stringValue(row.status).toLowerCase()))
      .map((row) => ({
        pending_id: stringValue(row.pending_id ?? row.envelope_id),
        template_name: stringValue(row.interface_id ?? row.template_name ?? row.source_endpoint),
        artifact_id: stringValue(row.artifact_id ?? row.artifact?.artifact_id ?? row.payload?.artifact_id),
        trace_id: stringValue(row.trace_id),
        created_at: timestampOf(row),
        raw: row,
      }));
  }

  function toolRows(projection) {
    return descriptorRows(projection, "tool").map((row) => ({
      ...row,
      name: stringValue(row.id ?? row.name),
      effect_categories: arrayValue(row.effect_categories).length
        ? row.effect_categories
        : (row.is_world_effect ? ["world_effect"] : []),
      is_destructive: Boolean(row.is_destructive),
    }));
  }

  function verifierRows(projection) {
    return descriptorRows(projection, "verifier").map((row) => ({
      ...row,
      name: stringValue(row.id ?? row.name),
      source: stringValue(row.source) || "registry",
    }));
  }

  function policyRows(projection) {
    return eeTemplateRows(projection).map((row) => ({
      name: row.name,
      allowed_interfaces_count: arrayValue(row.allowed_interfaces ?? row.allowed_interface_templates).length,
      allowed_tools_count: arrayValue(row.allowed_tools).length,
      allowed_worker_environment_kinds_count: arrayValue(row.allowed_worker_environment_kinds).length,
      source: "registry.ee_template",
      raw_policy: row,
    }));
  }

  function descriptorInventoryRows(projection) {
    return DESCRIPTOR_REGISTRIES.map((registry) => ({
      registry: registry.kind,
      count: descriptorRows(projection, registry.kind).length,
      runtime_count: 0,
      warning_count: 0,
      descriptor_filename: registry.file,
    }));
  }

  function descriptorKindRows() {
    return DESCRIPTOR_REGISTRIES.map((registry) => ({
      kind: registry.kind,
      identity_field: registry.identity.join(" | "),
      descriptor_filename: registry.file,
      source: "projection.registry_index",
    }));
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

  function markKindRows(projection) {
    return descriptorRows(projection, "mark")
      .map((mark) => {
        const name = stringValue(mark.name ?? mark.id ?? mark.kind);
        if (!name) return null;
        return {
        ...mark,
        name,
        lifecycle: stringValue(mark.lifecycle) || "registered",
        source: "registry.mark",
        };
      })
      .filter(Boolean)
      .sort((a, b) => a.name.localeCompare(b.name));
  }

  function runRows(projection, includeLedger = true) {
    if (!includeLedger) {
      return projectionMemo(projection, "structure", "runRows.structure", () => (
        arrayValue(projection.runRecords).map((record) => ({
        run_id: record.run_id,
        status: record.status,
        record_status: record.record_status,
        active_checkpoint_count: record.active_checkpoint_count,
        worker: record.worker,
        ee_id: record.ee_id,
        created_at: "",
        meta: { record_path: record.record_path },
        source: "run_record",
        }))
      ));
    }
    return projectionMemo(projection, "ledger", "runRows", () => {
    const byRun = new Map(runRows(projection, false).map((row) => [row.run_id, row]));
    for (const row of envelopeRows(projection)) {
      const runtime = objectOrEmpty(objectOrEmpty(row.invocation_metadata).runtime);
      const runId = stringValue(runtime.run);
      const eeId = stringValue(runtime.ee);
      const worker = stringValue(runtime.worker);
      if (!runId || !eeId || !worker) continue;
      if (byRun.get(runId)?.source === "run_record") continue;
      const current = byRun.get(runId) || {
        run_id: runId,
        status: "recorded",
        worker,
        ee_id: eeId,
        created_at: timestampOf(row),
        meta: {},
        source: "ledger",
      };
      current.worker = worker;
      current.ee_id = eeId;
      byRun.set(runId, current);
    }
    return Array.from(byRun.values());
    });
  }

  function filterInterfaces(rows, params) {
    return filterByString(rows, "interface_type", params.interface_type);
  }

  function withoutRawProjectionEvidence(row) {
    const { raw: _raw, ...projected } = row;
    return projected;
  }

  function filterInterfaceArtifactTransferTargets(rows, params, projection) {
    let scopedRows = rows;
    if (params.run_id !== undefined) {
      const runId = requiredString(params.run_id, "run_id");
      const run = arrayValue(projection.runRecords).find((record) => stringValue(record.run_id) === runId);
      if (!run) throw new Error(`run is not present in file projection: ${runId}`);
      const activeInterfaces = new Set();
      for (const checkpoint of arrayValue(run.checkpoints)) {
        if (stringValue(checkpoint.status) !== "projected") continue;
        const inbound = stringValue(checkpoint.interface);
        if (inbound) activeInterfaces.add(inbound);
        for (const outbound of arrayValue(checkpoint.out_interfaces).map(stringValue).filter(Boolean)) {
          activeInterfaces.add(outbound);
        }
      }
      scopedRows = scopedRows
        .filter((row) => activeInterfaces.has(stringValue(row.interface_id)))
        .map((row) => ({ ...row, run_id: runId }));
    }
    return filterByString(
      filterByString(scopedRows, "interface_id", params.interface_id ?? params.interface),
      "status",
      params.status,
    );
  }

  function filterByEnvelopeKind(rows, kind) {
    return filterByString(rows, "kind", kind);
  }

  function descriptorRows(projection, kind) {
    return arrayValue(projection.descriptors[kind]);
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

const MOVEMENT_PAYLOAD_PREVIEW_BYTES = 64 * 1024;
const INTERFACE_MEDIA_MAX_BYTES = 64 * 1024 * 1024;
const ARTIFACT_MANIFEST_PREVIEW_MAX_BYTES = 2 * 1024 * 1024;
const ARTIFACT_PREVIEW_MAX_BYTES = 40 * 1024 * 1024;
const ARTIFACT_PAYLOAD_READ_CONCURRENCY = 8;
const RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_ENTRIES = 512;
const RESOLVED_ARTIFACT_PAYLOAD_CACHE_MAX_BYTES = 64 * 1024 * 1024;

async function readProviderInterfaceDescriptors(registryRoot) {
  const interfaceRoot = path.join(registryRoot, "interface");
  const entries = await fs.readdir(interfaceRoot, { withFileTypes: true });
  const descriptors = [];
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    const descriptorPath = path.join(interfaceRoot, entry.name, "interface.json");
    let descriptor;
    try {
      descriptor = JSON.parse(await fs.readFile(descriptorPath, "utf8"));
    } catch {
      continue;
    }
    if (stringValue(descriptor.interface_type).toLowerCase() !== "world") continue;
    if (stringValue(descriptor.role).toLowerCase() !== "provider") continue;
    const id = stringValue(descriptor.id) || entry.name;
    if (id !== entry.name) continue;
    const octopusOsPath = path.join(interfaceRoot, entry.name, "octopus_os.json");
    const octopusOs = await readJsonFile(octopusOsPath);
    if (octopusOs) {
      parseOctopusOsSidecar(octopusOs, id, octopusOsPath);
      continue;
    }
    descriptors.push({
      ...descriptor,
      descriptor_path: descriptorPath,
      registry_dir: entry.name,
    });
  }
  return providerInterfaceDescriptorRows(descriptors);
}

function providerInterfaceDescriptorRows(descriptors) {
  const providers = [];
  for (const descriptor of descriptors) {
    if (stringValue(descriptor.interface_type).toLowerCase() !== "world") continue;
    if (stringValue(descriptor.role).toLowerCase() !== "provider") continue;
    if (descriptor.octopus_os_sidecar) continue;
    const id = stringValue(descriptor.id) || stringValue(descriptor.registry_dir);
    if (!id || id !== stringValue(descriptor.registry_dir)) continue;
    const descriptorConfig = objectOrEmpty(descriptor.interface_driver_config);
    const privateConfig = objectOrEmpty(descriptorConfig.private_config);
    const supportedReasoningEfforts = arrayValue(
      descriptorConfig.supported_reasoning_efforts,
    ).map((value) => stringValue(value?.effort ?? value)).filter(Boolean);
    const model = stringValue(privateConfig.model);
    if (!model) continue;
    const declaredModels = arrayValue(
      objectOrEmpty(objectOrEmpty(objectOrEmpty(descriptor.interface_policy).variables).models).default,
    ).map(stringValue).filter(Boolean);
    const models = providerModelCatalogRows(descriptorConfig, privateConfig, model, declaredModels);
    providers.push({
      id,
      label: providerLabel(descriptor, id),
      descriptorPath: stringValue(descriptor.descriptor_path),
      runtimeHandlerRef: stringValue(descriptor.interface_driver_ref),
      sourceNames: arrayValue(descriptor.source_names).map(stringValue).filter(Boolean),
      model,
      reasoningEffort: stringValue(privateConfig.reasoning_effort),
      serviceTier: stringValue(privateConfig.service_tier),
      supportedReasoningEfforts,
      models,
    });
  }
  return providers.sort((a, b) => a.id.localeCompare(b.id));
}

function providerModelCatalogRows(descriptorConfig, privateConfig, selectedModel, declaredModels = []) {
  const globalEfforts = arrayValue(descriptorConfig.supported_reasoning_efforts)
    .map((value) => stringValue(value?.effort ?? value))
    .filter(Boolean);
  const selectedEffort = stringValue(privateConfig.reasoning_effort);
  const rows = [];
  const seen = new Set();
  for (const value of arrayValue(privateConfig.model_catalog)) {
    const entry = objectOrEmpty(value);
    const model = stringValue(entry.model);
    if (!model || seen.has(model)) continue;
    seen.add(model);
    const supportedReasoningEfforts = arrayValue(
      entry.supported_reasoning_efforts ?? entry.supportedReasoningEfforts,
    ).map((effort) => stringValue(effort?.effort ?? effort?.reasoningEffort ?? effort))
      .filter(Boolean);
    rows.push({
      model,
      displayName: stringValue(entry.display_name ?? entry.displayName) || model,
      description: stringValue(entry.description),
      defaultReasoningEffort: stringValue(
        entry.default_reasoning_effort ?? entry.defaultReasoningEffort,
      ) || (model === selectedModel ? selectedEffort : ""),
      supportedReasoningEfforts: supportedReasoningEfforts.length
        ? [...new Set(supportedReasoningEfforts)]
        : model === selectedModel ? [...new Set(globalEfforts)] : [],
      showInPicker: entry.show_in_picker !== false && entry.showInPicker !== false,
      isDefault: entry.is_default === true || entry.isDefault === true || model === selectedModel,
      ...modelServiceTiers(entry.service_tiers, entry.default_service_tier, `Provider model '${model}'`),
    });
  }
  for (const model of declaredModels) {
    if (!model || seen.has(model)) continue;
    seen.add(model);
    rows.push({
      model,
      displayName: model,
      description: "",
      defaultReasoningEffort: model === selectedModel ? selectedEffort : "",
      supportedReasoningEfforts: model === selectedModel ? [...new Set(globalEfforts)] : [],
      showInPicker: true,
      isDefault: model === selectedModel,
    });
  }
  if (!seen.has(selectedModel)) {
    rows.push({
      model: selectedModel,
      displayName: selectedModel,
      description: "",
      defaultReasoningEffort: selectedEffort,
      supportedReasoningEfforts: [...new Set(globalEfforts)],
      showInPicker: true,
      isDefault: true,
    });
  }
  return rows;
}


async function readOperatorChatProtocol(registryRoot) {
  try {
    const client = JSON.parse(await fs.readFile(
      path.join(registryRoot, "interface", "world_operator_chat", "client.json"),
      "utf8",
    ));
    const labels = objectOrEmpty(client.artifact_labels);
    const authoring = objectOrEmpty(client.authoring);
    const protocol = {
      interfaceId: stringValue(client.interface),
      outboundInterfaceId: stringValue(client.outbound_interface) || stringValue(client.interface),
      inputLabel: stringValue(labels.message),
      terminalOutputLabel: stringValue(labels.terminal_output),
      errorOutputLabel: stringValue(labels.error_output),
      streamOutputLabels: normalizeLabelList(labels.stream_outputs, []),
      approvalResponseLabel: stringValue(labels.approval_response),
      elicitationResponseLabel: stringValue(labels.elicitation_response),
      userInputResponseLabel: stringValue(labels.user_input_response),
      toolCallLabel: stringValue(labels.tool_call),
      hiddenActivityLabels: normalizeLabelList(labels.hidden_activity, []),
      authoringRoomInterfaceId: stringValue(authoring.room_interface),
      artifactReadProjection: client.artifact_read_projection,
    };
    return Object.values(protocol).slice(0, 5).every(Boolean) ? protocol : null;
  } catch {
    return null;
  }
}



function providerLabel(descriptor, fallback) {
  const source = arrayValue(descriptor.source_names).map(stringValue).find(Boolean);
  return source || fallback;
}

function normalizeLabelList(values, fallback) {
  const normalized = [];
  const seen = new Set();
  for (const value of [...arrayValue(values), ...fallback]) {
    const text = stringValue(value);
    if (!text || seen.has(text)) continue;
    seen.add(text);
    normalized.push(text);
  }
  return normalized;
}


async function readDirectoryEntries(root) {
  try {
    return await fs.readdir(root, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") return [];
    throw error;
  }
}

async function listDirectFiles(root, predicate) {
  const entries = await readDirectoryEntries(root);
  return entries
    .filter((entry) => entry.isFile())
    .map((entry) => path.join(root, entry.name))
    .filter(predicate)
    .sort();
}

async function listWorkerEnvironmentRecordFiles(interfacesRoot) {
  const interfaces = (await readDirectoryEntries(interfacesRoot)).filter((entry) => entry.isDirectory());
  const nested = await Promise.all(interfaces.map(async (interfaceEntry) => {
    const root = path.join(interfacesRoot, interfaceEntry.name, "worker-environments");
    return (await readDirectoryEntries(root))
      .filter((entry) => entry.isDirectory())
      .map((entry) => path.join(root, entry.name, "worker_environment.json"));
  }));
  return nested.flat().sort();
}

async function readJsonFile(file) {
  try {
    return JSON.parse(await fs.readFile(file, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw new Error(`json parse failed: ${file}: ${error.message}`);
  }
}

function parseOctopusOsSidecar(value, interfaceId, file) {
  if (!isPlainRecord(value)) throw new Error(`Octopus OS sidecar is not an object: ${file}`);
  const allowedKeys = new Set([
    "schema_version",
    "workspace",
    "interface",
    "owner_interface",
    "component",
    "surface_role",
    "provider_choice_interface",
    "worker_choice_interface",
    "authoring_profile",
    "task_authoring_profiles",
    "presentation",
    "voice_context",
  ]);
  const unknown = Object.keys(value).filter((key) => !allowedKeys.has(key));
  const surfaceRole = stringValue(value.surface_role);
  const component = stringValue(value.component);
  if (
    unknown.length
    || value.schema_version !== 1
    || value.workspace !== "octopus_os"
    || value.interface !== interfaceId
    || !safeInterfacePathSegment(stringValue(value.owner_interface))
    || !["home", "task"].includes(surfaceRole)
    || !["client", "provider", "worker", "worker_environment_door", "authoring_source", "authoring_environment", "voice_control", "artifact_return"].includes(component)
  ) {
    throw new Error(`Octopus OS sidecar does not satisfy schema v1: ${file}`);
  }
  const providerChoice = stringValue(value.provider_choice_interface);
  const workerChoice = stringValue(value.worker_choice_interface);
  const authoringProfile = value.authoring_profile === undefined
    ? null
    : parseOctopusOsAuthoringProfile(value.authoring_profile, workerChoice, file);
  const taskAuthoringProfiles = value.task_authoring_profiles === undefined
    ? []
    : parseOctopusOsTaskAuthoringProfiles(value.task_authoring_profiles, file);
  const presentation = value.presentation === undefined
    ? null
    : parseOctopusOsPresentation(value.presentation, file);
  const voiceContext = value.voice_context == null ? null : parseHomeVoiceContext(value.voice_context);
  if (voiceContext && (component !== "client" || surfaceRole !== "home")) {
    throw new Error(`Octopus OS voice context must belong to the Home client: ${file}`);
  }
  if (component === "client") {
    if (value.owner_interface !== interfaceId || !safeInterfacePathSegment(providerChoice) || !safeInterfacePathSegment(workerChoice)) {
      throw new Error(`Octopus OS client sidecar has incomplete choice provenance: ${file}`);
    }
    if (!authoringProfile && (taskAuthoringProfiles.length || presentation)) {
      throw new Error(`Octopus OS client configuration has no authoring profile of its own: ${file}`);
    }
    if (presentation && surfaceRole === "task"
      && (!presentation.name || presentation.projects.length || /[\u0000-\u001f\u007f]/u.test(presentation.name))) {
      throw new Error(`Octopus OS task presentation must contain a name and no project presentations: ${file}`);
    }
    if (taskAuthoringProfiles.length && surfaceRole !== "home") {
      throw new Error(`Octopus OS task authoring profiles must belong to the Home client interface: ${file}`);
    }
  } else if (providerChoice || workerChoice || authoringProfile || taskAuthoringProfiles.length || presentation) {
    throw new Error(`Octopus OS non-client sidecar carries client choice fields: ${file}`);
  }
  return {
    ...value,
    authoring_profile: authoringProfile,
    task_authoring_profiles: taskAuthoringProfiles,
    presentation,
    descriptor_path: file,
  };
}

function parseClientWorkerTemplate(value, workerInterfaceId, file) {
  exactObjectKeys(
    value,
    [
      "schema_version",
      "worker_interface",
      "project_ee_id",
      "provider_interface_id",
      "provider_model",
      "reasoning_effort",
      "service_tier",
      "application_ids",
      "worker_environment_kinds",
    ],
    [
      "schema_version",
      "worker_interface",
      "project_ee_id",
      "provider_interface_id",
      "application_ids",
      "worker_environment_kinds",
    ],
    `client worker template ${file}`,
  );
  const workerInterface = requiredSafeRegistryId(value.worker_interface, "client worker template interface");
  const projectEeId = requiredSafeRegistryId(value.project_ee_id, "client worker template Project EE");
  const providerInterfaceId = requiredSafeRegistryId(value.provider_interface_id, "client worker template provider");
  const providerModel = value.provider_model === undefined ? "" : requiredAgentModel(value.provider_model);
  const reasoningEffort = stringValue(value.reasoning_effort);
  const serviceTier = value.service_tier === undefined ? "" : requiredString(value.service_tier, "client worker template service tier");
  if (serviceTier.length > 80 || /[\u0000-\u001f\u007f]/.test(serviceTier)) throw new Error("Agent service tier is invalid");
  if (
    value.schema_version !== 1
    || workerInterface !== workerInterfaceId
    || reasoningEffort.length > 80
    || /[\u0000-\u001f\u007f]/.test(reasoningEffort)
  ) {
    throw new Error(`client worker template does not satisfy schema v1: ${file}`);
  }
  const applicationIds = parseUniqueRegistryIds(value.application_ids, "client worker template application");
  const workerEnvironmentKinds = parseUniqueRegistryIds(
    value.worker_environment_kinds,
    "client worker template worker environment",
  );
  return {
    schema_version: 1,
    worker_interface: workerInterface,
    project_ee_id: projectEeId,
    provider_interface_id: providerInterfaceId,
    ...(providerModel ? { provider_model: providerModel } : {}),
    reasoning_effort: reasoningEffort,
    ...(serviceTier ? { service_tier: serviceTier } : {}),
    application_ids: applicationIds,
    worker_environment_kinds: workerEnvironmentKinds,
  };
}

function parseUniqueRegistryIds(value, label) {
  if (!Array.isArray(value)) throw new Error(`${label} ids must be an array`);
  const ids = value.map((candidate, index) => requiredSafeRegistryId(candidate, `${label} ${index}`));
  if (new Set(ids).size !== ids.length) throw new Error(`${label} ids must be unique`);
  return ids;
}

function parseApplicationManifest(value, interfaceId, file) {
  exactObjectKeys(
    value,
    [
      "schema_version",
      "id",
      "name",
      "description",
      "category",
      "developer",
      "version",
      "icon",
      "logo",
      "logo_dark",
      "interfaces",
      "artifact_bindings",
      "task_capabilities",
      "setup",
    ],
    ["schema_version", "id", "name", "description", "category", "developer", "version", "logo", "interfaces"],
    `application manifest ${file}`,
  );
  const id = requiredSafeRegistryId(value.id, "application manifest id");
  const name = stringValue(value.name);
  const description = stringValue(value.description);
  const category = stringValue(value.category);
  const developer = stringValue(value.developer);
  const version = stringValue(value.version);
  const icon = stringValue(value.icon);
  const logo = stringValue(value.logo);
  const logoDark = stringValue(value.logo_dark);
  if (
    value.schema_version !== 3
    || id !== interfaceId
    || !name
    || !description
    || !developer
    || !version
    || !["connector", "plugin", "artifact", "provider"].includes(category)
    || icon.length > 64
    || !applicationLogoFileName(logo)
    || (logoDark && !applicationLogoFileName(logoDark))
  ) {
    throw new Error(`application manifest does not satisfy schema v3: ${file}`);
  }
  if (
    !Array.isArray(value.interfaces)
    || value.interfaces.length === 0
    || new Set(value.interfaces).size !== value.interfaces.length
  ) {
    throw new Error(`application manifest interfaces are invalid: ${file}`);
  }
  const interfaces = value.interfaces.map((candidate, index) => (
    requiredSafeRegistryId(candidate, `application manifest interface ${index}`)
  ));
  if (!interfaces.includes(interfaceId)) {
    throw new Error(`application manifest does not include its owner interface: ${file}`);
  }
  if (value.artifact_bindings !== undefined && !Array.isArray(value.artifact_bindings)) {
    throw new Error(`application manifest artifact_bindings must be an array: ${file}`);
  }
  const artifactBindings = value.artifact_bindings === undefined
    ? []
    : value.artifact_bindings.map((candidate, index) => {
        exactObjectKeys(
          candidate,
          ["artifact_id", "label"],
          ["artifact_id"],
          `application manifest artifact binding ${index}`,
        );
        const artifactId = requiredSafeRegistryId(
          candidate.artifact_id,
          `application manifest artifact binding ${index} id`,
        );
        const label = stringValue(candidate.label);
        return { artifact_id: artifactId, ...(label ? { label } : {}) };
      });
  const taskCapabilities = parseApplicationTaskCapabilities(
    value.task_capabilities,
    file,
  );
  const setup = parseApplicationSetup(value.setup, interfaces, file);
  return {
    schema_version: 3,
    id,
    name,
    description,
    category,
    developer,
    version,
    icon,
    logo,
    logo_dark: logoDark,
    interfaces,
    artifact_bindings: artifactBindings,
    task_capabilities: taskCapabilities,
    setup,
  };
}

function parseApplicationTaskCapabilities(value, file) {
  if (value === undefined) return { tools: [] };
  exactObjectKeys(
    value,
    ["tools"],
    ["tools"],
    `application manifest task capabilities ${file}`,
  );
  if (!Array.isArray(value.tools)) {
    throw new Error(`application manifest task capability tools must be an array: ${file}`);
  }
  const tools = value.tools.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["tool", "interface", "worker_environment_kind"],
      ["tool", "interface", "worker_environment_kind"],
      `application manifest task capability tool ${index}`,
    );
    return {
      tool: requiredContractToken(candidate.tool, `application task capability tool ${index}`),
      interface: requiredSafeRegistryId(candidate.interface, `application task capability tool ${index} interface`),
      worker_environment_kind: requiredSafeRegistryId(
        candidate.worker_environment_kind,
        `application task capability tool ${index} worker environment kind`,
      ),
    };
  });
  if (new Set(tools.map((tool) => tool.tool)).size !== tools.length) {
    throw new Error(`application manifest task capability tools contain duplicates: ${file}`);
  }
  return { tools };
}

function parseApplicationSetup(value, interfaces, file) {
  if (value === undefined) return { fields: [] };
  exactObjectKeys(value, ["fields"], ["fields"], `application manifest setup ${file}`);
  if (!Array.isArray(value.fields)) {
    throw new Error(`application manifest setup fields must be an array: ${file}`);
  }
  const interfaceIds = new Set(interfaces);
  const fields = value.fields.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["interface", "pointer", "label", "kind", "required"],
      ["interface", "pointer", "label", "kind", "required"],
      `application manifest setup field ${index}`,
    );
    const interfaceId = requiredSafeRegistryId(candidate.interface, `application setup field ${index} interface`);
    const pointer = requiredString(candidate.pointer, `application setup field ${index} pointer`);
    const label = requiredString(candidate.label, `application setup field ${index} label`);
    const kind = requiredString(candidate.kind, `application setup field ${index} kind`);
    if (!interfaceIds.has(interfaceId)) {
      throw new Error(`application setup field ${index} references an interface outside the package: ${file}`);
    }
    if (!validJsonPointer(pointer) || label.length > 96 || !["text", "url", "secret"].includes(kind)) {
      throw new Error(`application manifest setup field ${index} is invalid: ${file}`);
    }
    return {
      interface: interfaceId,
      pointer,
      label,
      kind,
      required: requiredBoolean(candidate.required, `application setup field ${index} required`),
    };
  });
  const identities = fields.map((field) => `${field.interface}\0${field.pointer}`);
  if (new Set(identities).size !== identities.length) {
    throw new Error(`application manifest setup fields contain duplicates: ${file}`);
  }
  return { fields };
}

function resolveAuthoredApplicationSetupFields(authoredFields, interfaceId, declarations, label) {
  const resolved = [];
  const mappedNames = new Set();
  for (const presentation of authoredFields.filter((field) => stringValue(field?.interface) === interfaceId)) {
    const pointer = stringValue(presentation?.pointer);
    const matches = declarations.filter((declaration) => (
      arrayValue(declaration?.pointers).map(stringValue).includes(pointer)
    ));
    if (matches.length !== 1) {
      throw new Error(`${label} field '${pointer}' must resolve to exactly one declared environment field`);
    }
    const name = stringValue(matches[0]?.name);
    if (!name || mappedNames.has(name)) {
      throw new Error(`${label} maps more than one presentation field to the same environment field`);
    }
    mappedNames.add(name);
    resolved.push({ presentation, declaration: matches[0] });
  }
  return resolved;
}

function validJsonPointer(value) {
  return typeof value === "string"
    && value.startsWith("/")
    && value !== "/"
    && !value.split("/").slice(1).some((token) => /~(?:[^01]|$)/.test(token));
}

function normalizeRuntimeLaunchEnvironment(value) {
  if (value === null || value === undefined) return null;
  if (!isPlainRecord(value)) throw new Error("runtime launch environment must be an object or null");
  const output = {};
  for (const [name, entry] of Object.entries(value)) {
    if (typeof entry !== "string") throw new Error(`runtime launch environment '${name}' is not a string`);
    output[name] = entry;
  }
  return output;
}

async function readInterfacePrivateConfigurationProjection({
  descriptor,
  interfaceId,
  interfaceRegistryDirectory,
  interfacePrivateDirectory,
  launchEnvironment,
}) {
  const declarations = await readInterfaceEnvironmentDeclarations(
    descriptor,
    interfaceId,
    interfaceRegistryDirectory,
  );
  const document = await readInterfacePrivateEnvironmentDocument(
    interfacePrivateDirectory,
    interfaceId,
    new Set(declarations.map((declaration) => declaration.name)),
  );
  const environmentKnown = launchEnvironment !== null;
  const fields = declarations.map((declaration) => {
    const privateValue = document.values.get(declaration.name);
    const environmentValue = environmentKnown && Object.prototype.hasOwnProperty.call(launchEnvironment, declaration.name)
      ? launchEnvironment[declaration.name]
      : undefined;
    const source = privateValue !== undefined && environmentValue !== undefined
      ? privateValue === environmentValue ? "both" : "conflict"
      : privateValue !== undefined
        ? "private"
        : environmentValue !== undefined
          ? environmentValue.length > 0 ? "environment" : "missing"
          : environmentKnown ? "missing" : "unknown";
    return {
      name: declaration.name,
      pointers: declaration.pointers,
      required: declaration.required === true,
      configured: source !== "conflict"
        && (privateValue !== undefined || (environmentValue !== undefined && environmentValue.length > 0)),
      private_saved: privateValue !== undefined,
      source,
    };
  });
  return {
    interface_id: interfaceId,
    status: fields.some((field) => field.source === "conflict") ? "conflict" : "valid",
    document_status: document.present ? "valid" : "absent",
    document_digest: document.digest,
    fields,
    error: "",
  };
}

async function readInterfaceEnvironmentDeclarations(descriptor, interfaceId, interfaceRegistryDirectory) {
  const lifePath = path.join(interfaceRegistryDirectory, "life.json");
  const life = await readOptionalRegularJsonDocument(lifePath, `interface life ${interfaceId}`);
  if (!life) return [];
  exactObjectKeys(life.document, ["processes"], ["processes"], `interface life ${interfaceId}`);
  if (!Array.isArray(life.document.processes) || life.document.processes.length === 0) {
    throw new Error(`interface life '${interfaceId}' declares no processes`);
  }
  const declarations = new Map();
  life.document.processes.forEach((processDocument, processIndex) => {
    if (!isPlainRecord(processDocument)) throw new Error(`interface life '${interfaceId}' process ${processIndex} is invalid`);
    const inheritedNames = processDocument.inherit_env === undefined ? [] : processDocument.inherit_env;
    const requiredNames = processDocument.required_env === undefined ? [] : processDocument.required_env;
    if (!Array.isArray(inheritedNames) || inheritedNames.some((name) => !validInterfaceEnvironmentName(name))) {
      throw new Error(`interface life '${interfaceId}' process ${processIndex} has invalid inherit_env`);
    }
    if (!Array.isArray(requiredNames) || requiredNames.some((name) => (
      typeof name !== "string" || !inheritedNames.includes(name)
    ))) {
      throw new Error(`interface life '${interfaceId}' process ${processIndex} has invalid required_env`);
    }
    for (const name of inheritedNames) {
      if (name === "PATH") continue;
      const current = declarations.get(name) ?? { pointers: new Set(), required: false };
      current.required ||= requiredNames.includes(name);
      declarations.set(name, current);
    }
    const pointers = processDocument.inherit_env_from_interface === undefined
      ? []
      : processDocument.inherit_env_from_interface;
    if (!Array.isArray(pointers)) {
      throw new Error(`interface life '${interfaceId}' process ${processIndex} has invalid inherit_env_from_interface`);
    }
    for (const [pointerIndex, pointer] of pointers.entries()) {
      if (!validJsonPointer(pointer)) {
        throw new Error(`interface life '${interfaceId}' process ${processIndex} env pointer ${pointerIndex} is invalid`);
      }
      const selected = resolveJsonPointer(descriptor, pointer);
      if (selected === undefined) {
        throw new Error(`interface life '${interfaceId}' env pointer '${pointer}' is absent from its descriptor`);
      }
      const names = Array.isArray(selected) ? selected : [selected];
      for (const [nameIndex, candidate] of names.entries()) {
        if (typeof candidate !== "string" || !validInterfaceEnvironmentName(candidate)) {
          throw new Error(`interface life '${interfaceId}' env pointer '${pointer}' item ${nameIndex} is invalid`);
        }
        if (candidate === "PATH") continue;
        const current = declarations.get(candidate) ?? { pointers: new Set(), required: false };
        current.pointers.add(pointer);
        current.required ||= requiredNames.includes(candidate);
        declarations.set(candidate, current);
      }
    }
  });
  return [...declarations.entries()]
    .map(([name, declaration]) => ({
      name,
      pointers: [...declaration.pointers].sort(),
      required: declaration.required,
    }))
    .sort((left, right) => left.name.localeCompare(right.name));
}

function resolveJsonPointer(value, pointer) {
  let cursor = value;
  for (const encoded of pointer.split("/").slice(1)) {
    const token = encoded.replaceAll("~1", "/").replaceAll("~0", "~");
    if (Array.isArray(cursor)) {
      if (!/^(?:0|[1-9][0-9]*)$/.test(token)) return undefined;
      cursor = cursor[Number(token)];
    } else if (isPlainRecord(cursor) && Object.prototype.hasOwnProperty.call(cursor, token)) {
      cursor = cursor[token];
    } else {
      return undefined;
    }
  }
  return cursor;
}

function validInterfaceEnvironmentName(value) {
  const name = typeof value === "string" ? value.trim() : "";
  return Boolean(name)
    && name === value
    && !name.includes("=")
    && !name.includes("\0")
    && !INTERFACE_PRIVATE_RESERVED_ENV.has(name);
}

async function readInterfacePrivateEnvironmentDocument(directory, interfaceId, declaredNames) {
  const file = path.join(directory, INTERFACE_PRIVATE_ENVIRONMENT_FILE);
  try {
    await validateInterfacePrivateDirectory(directory, false);
  } catch (error) {
    if (error?.code === "ENOENT") return { present: false, digest: "", values: new Map() };
    throw error;
  }
  let stat;
  try {
    stat = await fs.lstat(file);
  } catch (error) {
    if (error?.code === "ENOENT") return { present: false, digest: "", values: new Map() };
    throw error;
  }
  validateInterfacePrivateFileStat(file, stat);
  const body = await fs.readFile(file);
  const after = await fs.lstat(file);
  validateInterfacePrivateFileStat(file, after);
  if (stat.dev !== after.dev || stat.ino !== after.ino || stat.size !== after.size || stat.mtimeMs !== after.mtimeMs) {
    throw new Error(`interface private environment '${interfaceId}' changed while it was read`);
  }
  let document;
  try {
    document = JSON.parse(body.toString("utf8"));
  } catch (error) {
    throw new Error(`interface private environment '${interfaceId}' is malformed JSON: ${error.message}`);
  }
  exactObjectKeys(document, ["schema_version", "interface", "values"], ["schema_version", "interface", "values"], `interface private environment ${interfaceId}`);
  if (document.schema_version !== 1 || document.interface !== interfaceId || !isPlainRecord(document.values)) {
    throw new Error(`interface private environment '${interfaceId}' has invalid schema or identity`);
  }
  const values = new Map();
  for (const [name, value] of Object.entries(document.values)) {
    if (!declaredNames.has(name) || typeof value !== "string" || value.length === 0 || value.includes("\0")) {
      throw new Error(`interface private environment '${interfaceId}' contains invalid or undeclared field '${name}'`);
    }
    values.set(name, value);
  }
  return {
    present: true,
    digest: crypto.createHash("sha256").update(body).digest("hex"),
    values,
  };
}

function validateInterfacePrivateFileStat(file, stat) {
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`interface private environment '${file}' must be a regular file`);
  if (stat.size > INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES) {
    throw new Error(`interface private environment '${file}' exceeds ${INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES} bytes`);
  }
  validateOwnerOnlyStat(file, stat, false);
}

async function validateInterfacePrivateDirectory(directory, create) {
  try {
    const stat = await fs.lstat(directory);
    if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error(`interface private directory '${directory}' must be a real directory`);
    validateOwnerOnlyStat(directory, stat, true);
  } catch (error) {
    if (!create || error?.code !== "ENOENT") throw error;
    const parent = path.dirname(directory);
    await requirePlainDirectory(parent, `interface private root ${parent}`);
    await fs.mkdir(directory, { mode: 0o700 });
    const stat = await fs.lstat(directory);
    if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error(`interface private directory '${directory}' was not created safely`);
    validateOwnerOnlyStat(directory, stat, true);
  }
}

function validateOwnerOnlyStat(label, stat, directory) {
  if (process.platform === "win32") return;
  const currentUid = typeof process.geteuid === "function" ? process.geteuid() : null;
  if (currentUid !== null && Number.isSafeInteger(stat.uid) && stat.uid !== currentUid) {
    throw new Error(`${directory ? "interface private directory" : "interface private environment"} '${label}' is not owned by the current user`);
  }
  if ((stat.mode & 0o077) !== 0) {
    throw new Error(`${directory ? "interface private directory" : "interface private environment"} '${label}' is not owner-only`);
  }
}

async function mutateApplicationPrivateConfiguration(registryRootValue, octopusHomeRootValue, request, options = {}) {
  const run = applicationPrivateConfigurationMutationTail.then(() => (
    mutateApplicationPrivateConfigurationUnlocked(registryRootValue, octopusHomeRootValue, request, options)
  ));
  applicationPrivateConfigurationMutationTail = run.then(() => undefined, () => undefined);
  return run;
}

async function mutateApplicationPrivateConfigurationUnlocked(registryRootValue, octopusHomeRootValue, request, options) {
  exactObjectKeys(
    request,
    ["applicationId", "interfaceId", "expectedDigest", "updates", "clears"],
    ["applicationId", "interfaceId", "expectedDigest", "updates", "clears"],
    "application private configuration mutation",
  );
  const applicationId = requiredSafeRegistryId(request.applicationId, "application private configuration applicationId");
  const interfaceId = requiredSafeRegistryId(request.interfaceId, "application private configuration interfaceId");
  const expectedDigest = request.expectedDigest === null
    ? null
    : requiredDigest(request.expectedDigest, "application private configuration expectedDigest");
  if (!isPlainRecord(request.updates) || !Array.isArray(request.clears)) {
    throw new Error("application private configuration updates must be an object and clears must be an array");
  }
  const registryRoot = path.resolve(requiredString(registryRootValue, "registry root"));
  const octopusHomeRoot = path.resolve(requiredString(octopusHomeRootValue, "Octopus home root"));
  const applicationDirectory = path.join(registryRoot, "interface", applicationId);
  await requirePlainDirectory(applicationDirectory, `application registry owner ${applicationId}`);
  const applicationManifestPath = path.join(applicationDirectory, "application.json");
  const applicationManifest = parseApplicationManifest(
    (await readRegularJsonDocument(applicationManifestPath, `application manifest ${applicationId}`)).document,
    applicationId,
    applicationManifestPath,
  );
  if (!applicationManifest.interfaces.includes(interfaceId)) {
    throw new Error(`application '${applicationId}' does not own interface '${interfaceId}'`);
  }
  const interfaceRegistryDirectory = path.join(registryRoot, "interface", interfaceId);
  await requirePlainDirectory(interfaceRegistryDirectory, `interface registry owner ${interfaceId}`);
  const descriptor = (await readRegularJsonDocument(
    path.join(interfaceRegistryDirectory, "interface.json"),
    `interface descriptor ${interfaceId}`,
  )).document;
  if (stringValue(descriptor.id ?? descriptor.rule_id) !== interfaceId) {
    throw new Error(`interface descriptor '${interfaceId}' does not match its registry folder`);
  }
  const declarations = await readInterfaceEnvironmentDeclarations(
    descriptor,
    interfaceId,
    interfaceRegistryDirectory,
  );
  const declaredNames = new Set(declarations.map((declaration) => declaration.name));
  const presented = applicationManifest.setup.fields.filter((field) => field.interface === interfaceId);
  const editableNames = new Set((presented.length
    ? resolveAuthoredApplicationSetupFields(
        applicationManifest.setup.fields,
        interfaceId,
        declarations,
        `application '${applicationId}' setup`,
      ).map(({ declaration }) => declaration.name)
    : declarations.map((declaration) => declaration.name)));
  if (editableNames.size === 0) {
    throw new Error(`application '${applicationId}' declares no editable setup fields for interface '${interfaceId}'`);
  }
  const launchEnvironment = normalizeRuntimeLaunchEnvironment(
    typeof options.runtimeLaunchEnvironment === "function" ? options.runtimeLaunchEnvironment() : null,
  );
  const updates = new Map();
  for (const [name, value] of Object.entries(request.updates)) {
    if (!editableNames.has(name) || typeof value !== "string" || value.length === 0 || value.includes("\0") || value.length > INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES) {
      throw new Error(`application private configuration update '${name}' is invalid or undeclared`);
    }
    if (launchEnvironment !== null
      && Object.prototype.hasOwnProperty.call(launchEnvironment, name)
      && launchEnvironment[name] !== value) {
      throw new Error(`application private configuration update '${name}' conflicts with the managed runtime environment`);
    }
    updates.set(name, value);
  }
  const clears = request.clears.map((name, index) => {
    if (typeof name !== "string" || !editableNames.has(name)) {
      throw new Error(`application private configuration clear ${index} is invalid or undeclared`);
    }
    return name;
  });
  if (new Set(clears).size !== clears.length) throw new Error("application private configuration clears contain duplicates");
  if (clears.some((name) => updates.has(name))) throw new Error("application private configuration cannot update and clear the same field");
  if (updates.size === 0 && clears.length === 0) throw new Error("application private configuration mutation contains no changes");

  const interfacePrivateRoot = path.join(octopusHomeRoot, "interfaces");
  await requirePlainDirectory(interfacePrivateRoot, `interface private root ${interfacePrivateRoot}`);
  const interfacePrivateDirectory = path.join(interfacePrivateRoot, interfaceId);
  let current;
  try {
    current = await readInterfacePrivateEnvironmentDocument(interfacePrivateDirectory, interfaceId, declaredNames);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
    current = { present: false, digest: "", values: new Map() };
  }
  if ((current.present ? current.digest : null) !== expectedDigest) {
    throw new Error(`application private configuration '${interfaceId}' changed before the edit could be committed`);
  }
  for (const name of clears) {
    if (!current.values.has(name)) {
      throw new Error(`application private configuration clear '${name}' has no saved value`);
    }
  }
  const values = new Map(current.values);
  for (const [name, value] of updates) values.set(name, value);
  for (const name of clears) values.delete(name);
  const document = {
    schema_version: 1,
    interface: interfaceId,
    values: Object.fromEntries([...values.entries()].sort(([left], [right]) => left.localeCompare(right))),
  };
  const body = Buffer.from(`${JSON.stringify(document, null, 2)}\n`, "utf8");
  if (body.byteLength > INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES) {
    throw new Error(`application private configuration '${interfaceId}' exceeds ${INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES} bytes`);
  }
  if (!current.present && values.size > 0) await validateInterfacePrivateDirectory(interfacePrivateDirectory, true);
  if (current.present) await validateInterfacePrivateDirectory(interfacePrivateDirectory, false);
  const file = path.join(interfacePrivateDirectory, INTERFACE_PRIVATE_ENVIRONMENT_FILE);
  if (values.size === 0) {
    if (!current.present) throw new Error(`application private configuration '${interfaceId}' has no saved values to clear`);
    const unchanged = await readInterfacePrivateEnvironmentDocument(interfacePrivateDirectory, interfaceId, declaredNames);
    if (unchanged.digest !== current.digest) throw new Error(`application private configuration '${interfaceId}' changed before removal`);
    await fs.unlink(file);
    await syncDirectory(interfacePrivateDirectory);
    return { interfaceId, documentStatus: "absent", documentDigest: null, configuredFields: 0 };
  }
  await atomicWritePrivateEnvironment(file, body, current.present ? current.digest : null, interfaceId, declaredNames);
  const committed = await readInterfacePrivateEnvironmentDocument(interfacePrivateDirectory, interfaceId, declaredNames);
  return {
    interfaceId,
    documentStatus: "valid",
    documentDigest: committed.digest,
    configuredFields: committed.values.size,
  };
}

async function atomicWritePrivateEnvironment(file, body, expectedDigest, interfaceId, declaredNames) {
  const directory = path.dirname(file);
  const temporary = path.join(directory, `.${INTERFACE_PRIVATE_ENVIRONMENT_FILE}.${process.pid}.${crypto.randomUUID()}.tmp`);
  let handle;
  try {
    handle = await fs.open(temporary, "wx", 0o600);
    await handle.writeFile(body);
    await handle.sync();
    await handle.close();
    handle = null;
    const temporaryStat = await fs.lstat(temporary);
    validateInterfacePrivateFileStat(temporary, temporaryStat);
    const current = await readInterfacePrivateEnvironmentDocument(directory, interfaceId, declaredNames);
    if ((current.present ? current.digest : null) !== expectedDigest) {
      throw new Error(`application private configuration '${interfaceId}' changed before commit`);
    }
    await fs.rename(temporary, file);
    await syncDirectory(directory);
  } finally {
    await handle?.close().catch(() => {});
    await fs.rm(temporary, { force: true }).catch(() => {});
  }
}

function applicationLogoFileName(value) {
  return typeof value === "string"
    && /^[A-Za-z0-9][A-Za-z0-9._-]*\.(?:png|svg|webp)$/.test(value);
}

async function readApplicationLogoAsset(applicationRoot, relative, label) {
  if (!applicationLogoFileName(relative)) throw new Error(`${label} has an invalid package-owned file name`);
  const root = await fs.realpath(applicationRoot);
  const file = path.join(root, relative);
  const stat = await fs.lstat(file);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`${label} is not a regular package-owned file: ${file}`);
  const resolved = await fs.realpath(file);
  if (!isPathInside(root, resolved)) throw new Error(`${label} escapes its application package: ${file}`);
  if (stat.size <= 0 || stat.size > APPLICATION_LOGO_MAX_BYTES) {
    throw new Error(`${label} size is outside 1..${APPLICATION_LOGO_MAX_BYTES} bytes: ${file}`);
  }
  const extension = path.extname(relative).toLowerCase();
  const mime = extension === ".svg" ? "image/svg+xml" : extension === ".png" ? "image/png" : "image/webp";
  const bytes = await fs.readFile(resolved);
  if (bytes.byteLength !== stat.size) throw new Error(`${label} changed while it was read: ${file}`);
  return `data:${mime};base64,${bytes.toString("base64")}`;
}

async function readApplicationPackageCatalog(activeRegistryRoot, localRegistryRoot, warnings) {
  const [active, local] = await Promise.all([
    scanApplicationPackageRoot(activeRegistryRoot, "active", warnings, true),
    scanApplicationPackageRoot(localRegistryRoot, "local", warnings, false),
  ]);
  return {
    active,
    sources: local,
    source_conflicts: {},
  };
}

async function scanApplicationPackageRoot(registryRootValue, sourceKind, warnings, optionalRoot) {
  const registryRoot = stringValue(registryRootValue);
  if (!registryRoot || !path.isAbsolute(registryRoot)) {
    if (!optionalRoot) warnings.push(`${sourceKind} application source registry is unavailable`);
    return [];
  }
  const interfaceRoot = path.join(registryRoot, "interface");
  let entries;
  try {
    const stat = await fs.lstat(interfaceRoot);
    if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error("interface root is not a plain directory");
    entries = await fs.readdir(interfaceRoot, { withFileTypes: true });
  } catch (error) {
    if (optionalRoot && error?.code === "ENOENT") return [];
    warnings.push(`${sourceKind} application source cannot be read: ${interfaceRoot}: ${error.message}`);
    return [];
  }
  const packages = await mapConcurrentOrdered(
    entries.filter((entry) => entry.isDirectory()),
    8,
    async (entry) => {
      const manifestPath = path.join(interfaceRoot, entry.name, "application.json");
      let manifestDocument;
      try {
        manifestDocument = await readOptionalRegularJsonDocument(
          manifestPath,
          `${sourceKind} application manifest ${entry.name}`,
        );
        if (!manifestDocument) return null;
        return await readApplicationPackageSource(
          interfaceRoot,
          entry.name,
          sourceKind,
          manifestDocument,
        );
      } catch (error) {
        warnings.push(String(error?.message ?? error));
        return null;
      }
    },
  );
  return packages.filter(Boolean);
}

async function readApplicationPackageSource(interfaceRootValue, applicationIdValue, sourceKind, knownManifest = null) {
  const interfaceRoot = await fs.realpath(interfaceRootValue);
  const applicationId = requiredSafeRegistryId(applicationIdValue, "application package id");
  const ownerDirectory = path.join(interfaceRoot, applicationId);
  await requirePlainDirectory(ownerDirectory, `${sourceKind} application package ${applicationId}`);
  const manifestPath = path.join(ownerDirectory, "application.json");
  const manifestDocument = knownManifest ?? await readRegularJsonDocument(
    manifestPath,
    `${sourceKind} application manifest ${applicationId}`,
  );
  const manifest = parseApplicationManifest(
    manifestDocument.document,
    applicationId,
    manifestPath,
  );
  const directories = [];
  for (const interfaceId of manifest.interfaces) {
    const snapshot = await snapshotApplicationPackageDirectory(interfaceRoot, interfaceId, sourceKind);
    const descriptor = await readRegularJsonDocument(
      path.join(snapshot.directory, "interface.json"),
      `${sourceKind} application interface ${interfaceId}`,
    );
    const declaredId = stringValue(descriptor.document?.id ?? descriptor.document?.rule_id);
    if (declaredId !== interfaceId) {
      throw new Error(`${sourceKind} application interface '${interfaceId}' has drifted folder identity`);
    }
    directories.push(snapshot);
  }
  const packageDigest = crypto.createHash("sha256").update(stableJson(
    directories.flatMap((directory) => directory.files.map((file) => ({
      path: path.posix.join("interface", directory.interface_id, file.relative_path.split(path.sep).join("/")),
      sha256: file.sha256,
      mode: file.mode,
    }))),
  )).digest("hex");
  return {
    manifest,
    manifest_path: manifestPath,
    manifest_digest: manifestDocument.digest,
    descriptor_path: path.join(ownerDirectory, "interface.json"),
    package_digest: packageDigest,
    source_kind: sourceKind,
    interface_root: interfaceRoot,
    directories,
    file_count: directories.reduce((count, directory) => count + directory.files.length, 0),
    logo_data_url: await readApplicationLogoAsset(
      ownerDirectory,
      manifest.logo,
      `${sourceKind} application logo ${applicationId}`,
    ),
    logo_dark_data_url: manifest.logo_dark
      ? await readApplicationLogoAsset(
          ownerDirectory,
          manifest.logo_dark,
          `${sourceKind} application dark logo ${applicationId}`,
        )
      : "",
  };
}

async function snapshotApplicationPackageDirectory(interfaceRoot, interfaceId, sourceKind) {
  const directory = path.join(interfaceRoot, interfaceId);
  const resolved = await fs.realpath(directory);
  if (!isPathInside(interfaceRoot, resolved)) {
    throw new Error(`${sourceKind} application interface '${interfaceId}' escapes its package source`);
  }
  await requirePlainDirectory(resolved, `${sourceKind} application interface ${interfaceId}`);
  const files = [];
  const directories = [""];
  const visit = async (current, relativeDirectory) => {
    const entries = (await fs.readdir(current, { withFileTypes: true }))
      .sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const entryPath = path.join(current, entry.name);
      const relativePath = path.join(relativeDirectory, entry.name);
      const stat = await fs.lstat(entryPath);
      if (stat.isSymbolicLink()) {
        throw new Error(`${sourceKind} application package contains a symbolic link: ${entryPath}`);
      }
      if (stat.isDirectory()) {
        directories.push(relativePath);
        await visit(entryPath, relativePath);
        continue;
      }
      if (!stat.isFile()) {
        throw new Error(`${sourceKind} application package contains a non-file entry: ${entryPath}`);
      }
      const before = stat;
      const body = await fs.readFile(entryPath);
      const after = await fs.lstat(entryPath);
      if (
        after.isSymbolicLink()
        || !after.isFile()
        || before.dev !== after.dev
        || before.ino !== after.ino
        || before.size !== after.size
        || before.mtimeMs !== after.mtimeMs
      ) {
        throw new Error(`${sourceKind} application package changed while it was inspected: ${entryPath}`);
      }
      files.push({
        relative_path: relativePath,
        path: entryPath,
        sha256: crypto.createHash("sha256").update(body).digest("hex"),
        mode: before.mode & 0o777,
        size: before.size,
      });
    }
  };
  await visit(resolved, "");
  return { interface_id: interfaceId, directory: resolved, directories, files };
}

function parseOctopusOsPresentation(value, file) {
  exactObjectKeys(
    value,
    ["name", "projects"],
    ["projects"],
    "Octopus OS presentation",
  );
  if (!Array.isArray(value.projects)) {
    throw new Error(`Octopus OS presentation projects must be an array: ${file}`);
  }
  const name = stringValue(value.name);
  if (value.name !== undefined && (!name || [...name].length > 80 || name !== value.name)) {
    throw new Error(`Octopus OS presentation name is invalid: ${file}`);
  }
  const seenEeIds = new Set();
  const projects = value.projects.map((project, index) => {
    exactObjectKeys(
      project,
      ["ee_id", "name", "icon", "color"],
      ["ee_id", "name"],
      `Octopus OS presentation project ${index}`,
    );
    const eeId = requiredSafeRegistryId(
      project.ee_id,
      `Octopus OS presentation project ${index} EE`,
    );
    const name = stringValue(project.name);
    const icon = stringValue(project.icon);
    const color = stringValue(project.color);
    if (
      seenEeIds.has(eeId)
      || !name
      || name.length > 180
      || icon.length > 64
      || color.length > 64
    ) {
      throw new Error(`Octopus OS presentation project ${index} is invalid: ${file}`);
    }
    seenEeIds.add(eeId);
    return { ee_id: eeId, name, icon, color };
  });
  return { name, projects };
}

function parseOctopusOsAuthoringProfile(value, workerChoice, file) {
  exactObjectKeys(
    value,
    [
      "ee_id", "worker_choice_interface", "door_host_slots", "external_inputs", "external_relays",
      "model_outputs", "context_artifact_bindings", "payload_artifact_bindings", "selected_worker_environment_kinds",
      "selected_tools",
    ],
    ["ee_id", "worker_choice_interface", "door_host_slots"],
    "Octopus OS authoring profile",
  );
  const eeId = requiredSafeRegistryId(value.ee_id, "Octopus OS authoring profile EE");
  const worker = requiredSafeRegistryId(
    value.worker_choice_interface,
    "Octopus OS authoring profile worker",
  );
  if (workerChoice && worker !== workerChoice) {
    throw new Error(`Octopus OS authoring profile worker conflicts with client choice: ${file}`);
  }
  if (!isPlainRecord(value.door_host_slots)) {
    throw new Error(`Octopus OS authoring profile door_host_slots is not an object: ${file}`);
  }
  const doorHostSlots = {};
  for (const [interfaceId, slots] of Object.entries(value.door_host_slots)) {
    const door = requiredSafeRegistryId(interfaceId, "Octopus OS authoring profile door");
    if (!isPlainRecord(slots) || Object.keys(slots).length === 0) {
      throw new Error(`Octopus OS authoring profile door '${door}' has no host slots: ${file}`);
    }
    const normalized = {};
    for (const [slotName, root] of Object.entries(slots)) {
      const slot = requiredSafeRegistryId(slotName, "Octopus OS authoring profile root slot");
      const rootPath = stringValue(root);
      if (!path.isAbsolute(rootPath)) {
        throw new Error(`Octopus OS authoring profile root '${door}.${slot}' is not absolute: ${file}`);
      }
      normalized[slot] = rootPath;
    }
    doorHostSlots[door] = normalized;
  }
  const externalInputs = parseOctopusOsExternalInputs(value.external_inputs, file);
  const externalRelays = parseOctopusOsExternalRelays(value.external_relays, file);
  const modelOutputs = parseOctopusOsModelOutputs(value.model_outputs, file);
  const contextArtifactBindings = parseOctopusOsContextArtifactBindings(
    value.context_artifact_bindings,
    file,
  );
  const selectedPayloads = payloadArtifactBindings(value.payload_artifact_bindings);
  if (selectedPayloads.some(reference => contextArtifactBindings.some(context => context.id === reference.id))) throw new Error("Payload and JSON context selections overlap");
  const selectedWorkerEnvironmentKinds = parseOctopusOsSelectedWorkerEnvironmentKinds(
    value.selected_worker_environment_kinds,
    file,
  );
  const selectedTools = parseOctopusOsSelectedTools(value.selected_tools, file);
  const routeCheckpoints = new Set(externalInputs.map((input) => input.checkpoint_id));
  const routeSources = new Set(externalInputs.map((input) => input.interface_id));
  for (const relay of externalRelays) {
    if (routeCheckpoints.has(relay.checkpoint_id) || routeSources.has(relay.interface_id)) {
      throw new Error(`Octopus OS authoring profile repeats an external route: ${file}`);
    }
    routeCheckpoints.add(relay.checkpoint_id);
    routeSources.add(relay.interface_id);
  }
  for (const output of modelOutputs) {
    if (routeCheckpoints.has(output.checkpoint_id)) {
      throw new Error(`Octopus OS authoring profile repeats an external checkpoint: ${file}`);
    }
    routeCheckpoints.add(output.checkpoint_id);
  }
  return {
    ee_id: eeId,
    worker_choice_interface: worker,
    door_host_slots: doorHostSlots,
    external_inputs: externalInputs,
    external_relays: externalRelays,
    model_outputs: modelOutputs,
    context_artifact_bindings: contextArtifactBindings,
    ...(selectedPayloads.length ? {payload_artifact_bindings:selectedPayloads} : {}),
    selected_worker_environment_kinds: selectedWorkerEnvironmentKinds,
    selected_tools: selectedTools,
  };
}

function parseOctopusOsTaskAuthoringProfiles(value, file) {
  if (!Array.isArray(value)) {
    throw new Error(`Octopus OS task authoring profiles must be an array: ${file}`);
  }
  const seen = new Set();
  return value.map((candidate, index) => {
    const profile = parseOctopusOsAuthoringProfile(candidate, "", file);
    const key = `${profile.ee_id}\u0000${profile.worker_choice_interface}`;
    if (seen.has(key)) {
      throw new Error(`Octopus OS task authoring profile ${index} repeats EE and worker: ${file}`);
    }
    seen.add(key);
    return profile;
  });
}

function parseOctopusOsExternalInputs(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS external_inputs must be an array: ${file}`);
  const checkpoints = new Set();
  const interfaces = new Set();
  return value.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["checkpoint_id", "interface_id", "required_verified_artifacts"],
      ["checkpoint_id", "interface_id"],
      `Octopus OS external input ${index}`,
    );
    const checkpointId = requiredSafeRegistryId(candidate.checkpoint_id, `Octopus OS external input ${index} checkpoint`);
    const interfaceId = requiredSafeRegistryId(candidate.interface_id, `Octopus OS external input ${index} interface`);
    if (checkpoints.has(checkpointId) || interfaces.has(interfaceId)) {
      throw new Error(`Octopus OS external input ${index} is duplicated: ${file}`);
    }
    checkpoints.add(checkpointId);
    interfaces.add(interfaceId);
    const requirements = parseOctopusOsRequiredVerifiedArtifacts(
      candidate.required_verified_artifacts,
      `Octopus OS external input ${index}`,
      file,
    );
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      ...(requirements.length > 0 ? { required_verified_artifacts: requirements } : {}),
    };
  });
}

function parseOctopusOsExternalRelays(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS external_relays must be an array: ${file}`);
  const checkpoints = new Set();
  const interfaces = new Set();
  return value.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["checkpoint_id", "interface_id", "outbound_interface_ids", "required_verified_artifacts"],
      ["checkpoint_id", "interface_id", "outbound_interface_ids"],
      `Octopus OS external relay ${index}`,
    );
    const checkpointId = requiredSafeRegistryId(candidate.checkpoint_id, `Octopus OS external relay ${index} checkpoint`);
    const interfaceId = requiredSafeRegistryId(candidate.interface_id, `Octopus OS external relay ${index} interface`);
    if (!Array.isArray(candidate.outbound_interface_ids) || candidate.outbound_interface_ids.length === 0) {
      throw new Error(`Octopus OS external relay ${index} has no outbound interfaces: ${file}`);
    }
    const outboundInterfaceIds = candidate.outbound_interface_ids.map((target, targetIndex) => (
      requiredSafeRegistryId(target, `Octopus OS external relay ${index} outbound interface ${targetIndex}`)
    ));
    if (
      checkpoints.has(checkpointId)
      || interfaces.has(interfaceId)
      || new Set(outboundInterfaceIds).size !== outboundInterfaceIds.length
    ) {
      throw new Error(`Octopus OS external relay ${index} is duplicated: ${file}`);
    }
    checkpoints.add(checkpointId);
    interfaces.add(interfaceId);
    const requirements = parseOctopusOsRequiredVerifiedArtifacts(
      candidate.required_verified_artifacts,
      `Octopus OS external relay ${index}`,
      file,
    );
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      outbound_interface_ids: outboundInterfaceIds,
      ...(requirements.length > 0 ? { required_verified_artifacts: requirements } : {}),
    };
  });
}

function parseOctopusOsRequiredVerifiedArtifacts(value, owner, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) {
    throw new Error(`${owner} required_verified_artifacts must be an array: ${file}`);
  }
  const labels = new Set();
  return value.map((requirement, index) => {
    exactObjectKeys(
      requirement,
      ["mark", "artifact_label", "verifier", "params"],
      ["mark", "artifact_label", "verifier", "params"],
      `${owner} requirement ${index}`,
    );
    if (!isPlainRecord(requirement.params)) {
      throw new Error(`${owner} requirement ${index} params are not an object: ${file}`);
    }
    const artifactLabel = requiredContractToken(
      requirement.artifact_label,
      `${owner} requirement ${index} artifact label`,
    );
    if (labels.has(artifactLabel)) {
      throw new Error(`${owner} repeats required artifact label '${artifactLabel}': ${file}`);
    }
    labels.add(artifactLabel);
    return {
      mark: requiredContractToken(requirement.mark, `${owner} requirement ${index} mark`),
      artifact_label: artifactLabel,
      verifier: requiredSafeRegistryId(
        requirement.verifier,
        `${owner} requirement ${index} verifier`,
      ),
      params: requirement.params,
    };
  });
}

function projectOctopusOsRequiredVerifiedArtifacts(value) {
  return arrayValue(value).map((requirement) => ({
    mark: stringValue(requirement?.mark),
    artifactLabel: stringValue(requirement?.artifact_label),
    verifier: stringValue(requirement?.verifier),
    params: objectOrEmpty(requirement?.params),
  }));
}

function parseOctopusOsModelOutputs(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS model_outputs must be an array: ${file}`);
  const checkpoints = new Set();
  const interfaces = new Set();
  return value.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["checkpoint_id", "interface_id", "required_verified_artifact"],
      ["checkpoint_id", "interface_id", "required_verified_artifact"],
      `Octopus OS model output ${index}`,
    );
    const checkpointId = requiredSafeRegistryId(candidate.checkpoint_id, `Octopus OS model output ${index} checkpoint`);
    const interfaceId = requiredSafeRegistryId(candidate.interface_id, `Octopus OS model output ${index} interface`);
    if (checkpoints.has(checkpointId) || interfaces.has(interfaceId)) {
      throw new Error(`Octopus OS model output ${index} is duplicated: ${file}`);
    }
    checkpoints.add(checkpointId);
    interfaces.add(interfaceId);
    const requirement = candidate.required_verified_artifact;
    exactObjectKeys(
      requirement,
      ["mark", "artifact_label", "verifier", "params"],
      ["mark", "artifact_label", "verifier", "params"],
      `Octopus OS model output ${index} requirement`,
    );
    if (!isPlainRecord(requirement.params)) {
      throw new Error(`Octopus OS model output ${index} requirement params are not an object: ${file}`);
    }
    return {
      checkpoint_id: checkpointId,
      interface_id: interfaceId,
      required_verified_artifact: {
        mark: requiredContractToken(requirement.mark, `Octopus OS model output ${index} mark`),
        artifact_label: requiredContractToken(
          requirement.artifact_label,
          `Octopus OS model output ${index} artifact label`,
        ),
        verifier: requiredSafeRegistryId(requirement.verifier, `Octopus OS model output ${index} verifier`),
        params: requirement.params,
      },
    };
  });
}

function parseOctopusOsContextArtifactBindings(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS context_artifact_bindings must be an array: ${file}`);
  const ids = new Set();
  return value.map((candidate, index) => {
    exactObjectKeys(candidate, ["id"], ["id"], `Octopus OS context artifact binding ${index}`);
    const id = requiredSafeRegistryId(candidate.id, `Octopus OS context artifact binding ${index}`);
    if (ids.has(id)) throw new Error(`Octopus OS context artifact binding '${id}' is duplicated: ${file}`);
    ids.add(id);
    return { id };
  });
}

function parseOctopusOsSelectedWorkerEnvironmentKinds(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS selected_worker_environment_kinds must be an array: ${file}`);
  const kinds = value.map((candidate, index) => (
    requiredSafeRegistryId(candidate, `Octopus OS selected worker environment kind ${index}`)
  ));
  if (new Set(kinds).size !== kinds.length) {
    throw new Error(`Octopus OS selected_worker_environment_kinds contains duplicates: ${file}`);
  }
  return kinds;
}

function parseOctopusOsSelectedTools(value, file) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`Octopus OS selected_tools must be an array: ${file}`);
  const tools = value.map((candidate, index) => {
    exactObjectKeys(
      candidate,
      ["tool", "interface", "worker_environment_kind"],
      ["tool", "interface", "worker_environment_kind"],
      `Octopus OS selected tool ${index}`,
    );
    return {
      tool: requiredContractToken(candidate.tool, `Octopus OS selected tool ${index}`),
      interface: requiredSafeRegistryId(candidate.interface, `Octopus OS selected tool ${index} interface`),
      worker_environment_kind: requiredSafeRegistryId(
        candidate.worker_environment_kind,
        `Octopus OS selected tool ${index} worker environment kind`,
      ),
    };
  });
  if (new Set(tools.map((selection) => selection.tool)).size !== tools.length) {
    throw new Error(`Octopus OS selected_tools contains duplicates: ${file}`);
  }
  return tools;
}

function requiredContractToken(value, label) {
  const token = requiredString(value, label);
  if (token.length > 160 || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(token)) {
    throw new Error(`${label} must be a compact contract token`);
  }
  return token;
}

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

function exactObjectKeys(value, allowed, required, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be a JSON object`);
  const unknown = Object.keys(value).filter((key) => !allowed.includes(key));
  const missing = required.filter((key) => !Object.prototype.hasOwnProperty.call(value, key));
  if (unknown.length) throw new Error(`${label} contains unknown fields: ${unknown.join(", ")}`);
  if (missing.length) throw new Error(`${label} is missing required fields: ${missing.join(", ")}`);
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

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
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

async function mutateOctopusOsWorkspace(registryRoot, octopusHomeRoot, scope, request, hooks = {}) {
  const action = stringValue(request?.action);
  if (![
    "create_project",
    "create_agent_template",
    "add_agent_preset",
    "configure_agent_template",
    "delete_agent_template",
    "delete_project",
    "update_project_presentation",
    "delete_task",
    "rename_task",
    "move_task",
    "set_task_archived",
    "install_application",
    "remove_application",
    "install_package",
    "remove_package",
    "repair_package",
    "apply_application_pack",
    "install_driver",
    "remove_driver",
    "configure_application",
    "configure_driver",
    "configure_task_application",
  ].includes(action)) {
    throw new Error("Octopus OS workspace mutation action is invalid");
  }
  const execute = async () => {
    if ([
      "install_application",
      "remove_application",
      "install_package",
      "remove_package",
      "repair_package",
      "apply_application_pack",
      "install_driver",
      "remove_driver",
    ].includes(action)) {
      return mutateLocalPackageInventory(
        registryRoot,
        octopusHomeRoot,
        scope,
        request,
        hooks,
      );
    }
    const snapshot = requireOctopusOsScope(scope);
    if (action === "set_task_archived") {
      return setOctopusOsTaskArchived(registryRoot, octopusHomeRoot, snapshot, request, hooks);
    }
    if (action === "rename_task") {
      const runId = requiredSafeRegistryId(request.runId, "runId");
      const name = requiredAgentName(request.name);
      const task = snapshot.clientRuns.find((row) => row.run_id === runId && row.surface_role === "task");
      if (!task) throw new Error(`Task '${runId}' is not an editable task in this Home.`);
      if (typeof hooks.commitClientAuthoring !== "function") throw new Error("Client authoring transaction is unavailable.");
      const interfaceId = requiredSafeRegistryId(task.interface_id, "task client interface");
      const file = path.join(registryRoot, "interface", interfaceId, "octopus_os.json");
      const current = await readRegularJsonDocument(file, "task presentation");
      parseOctopusOsSidecar(current.document, interfaceId, file);
      if (current.document.component !== "client" || current.document.surface_role !== "task"
        || current.document.owner_interface !== interfaceId) {
        throw new Error("Task presentation no longer belongs to the selected run.");
      }
      const next = { ...current.document, presentation: { name, projects: [] } };
      parseOctopusOsSidecar(next, interfaceId, file);
      await hooks.commitClientAuthoring({
        label: "rename Octopus task",
        writes: [{ file, beforeDigest: current.digest, next }],
        removals: [], publishBarrierPath: file,
      });
      return { action, runId, name };
    }
    if (action === "add_agent_preset") {
      exactObjectKeys(request, ["action", "operationId", "home", "workspace", "projectEeId", "packId", "expectedPackManifestDigest", "presetId", "providerInterfaceId", "providerModel", "reasoningEffort", "serviceTier"], ["action", "operationId", "home", "workspace", "projectEeId", "packId", "expectedPackManifestDigest", "presetId", "providerInterfaceId", "providerModel", "reasoningEffort"], "Agent preset request");
      if (request.home !== octopusHomeRoot || typeof hooks.withAgentPreset !== "function") throw new Error("Agent preset authoring is unavailable for this Home");
      return hooks.withAgentPreset(request, async (preset) => {
        await preset.verify();
        const result = await createOctopusOsAgentTemplate(registryRoot, octopusHomeRoot, snapshot, {
          ...request,
          sourceWorkerInterfaceId: preset.sourceInterface.id,
          name: preset.recipe.name,
          icon: preset.recipe.icon,
        }, hooks.commitClientAuthoring, null, preset);
        return { ...result, action: "add_agent_preset" };
      });
    }
    if (action === "create_agent_template") {
      return createOctopusOsAgentTemplate(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.commitClientAuthoring,
        hooks.chatContexts,
      );
    }
    if (action === "configure_agent_template" || action === "delete_agent_template") {
      return mutateOctopusOsAgentTemplate(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.commitClientAuthoring,
        hooks.chatContexts,
      );
    }
    if (["create_project", "delete_project", "update_project_presentation"].includes(action)) {
      if (typeof hooks.mutateWorkspaceProject !== "function") {
        throw new Error("Workspace Project authoring transaction is unavailable");
      }
      return hooks.mutateWorkspaceProject(workspaceProjectMutationScope(snapshot), request);
    }
    if (action === "delete_task") {
      return deleteOctopusOsTask(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.releaseTaskWorkerEnvironments,
        hooks.commitClientAuthoring,
        hooks,
      );
    }
    if (action === "configure_application") {
      return configureOctopusOsApplication(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.commitClientAuthoring,
      );
    }
    if (action === "configure_driver") {
      return configureOctopusOsDriver(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.commitClientAuthoring,
      );
    }
    if (action === "configure_task_application") {
      return configureOctopusOsTaskApplication(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.withTaskAuthoringLock,
        hooks.planTaskApplicationAmendment,
        hooks.commitClientAuthoring,
      );
    }
    return moveOctopusOsTask(
      registryRoot,
      octopusHomeRoot,
      snapshot,
      request,
      hooks.commitClientAuthoring,
    );
  };
  const previous = osWorkspaceMutationTail;
  let release;
  const current = new Promise((resolve) => { release = resolve; });
  osWorkspaceMutationTail = previous.then(() => current);
  await previous;
  try {
    return await execute();
  } finally {
    release();
  }
}

async function createOctopusOsAgentTemplate(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  commitClientAuthoring,
  chatContexts,
  preset = null,
) {
  if (scope.status !== "ready") throw new Error("Agent creation requires a ready Octopus OS Home surface");
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const operationId = requiredApplicationOperationId(request?.operationId);
  const projectEeId = requiredSafeRegistryId(request?.projectEeId, "Agent Project EE");
  const sourceWorkerInterfaceId = requiredSafeRegistryId(
    request?.sourceWorkerInterfaceId,
    "Agent source worker template",
  );
  if (!scope.byId.has(projectEeId)) {
    throw new Error(`Agent EE '${projectEeId}' is outside the workspace scope`);
  }
  const requestedWorkspace = objectOrEmpty(request?.workspace);
  if (
    requiredSafeRegistryId(requestedWorkspace.rootEeId, "Agent workspace root EE") !== scope.rootEeId
    || requiredSafeRegistryId(requestedWorkspace.homeRunId, "Agent workspace Home run") !== scope.homeRunId
    || requiredSafeRegistryId(requestedWorkspace.homeInterfaceId, "Agent workspace Home interface") !== scope.homeInterfaceId
  ) {
    throw new Error("Agent creation does not match the projected Octopus OS workspace");
  }
  const expectedHomeSidecarDigest = requiredDigest(
    requestedWorkspace.expectedHomeSidecarDigest,
    "Agent workspace Home sidecar digest",
  );
  const expectedAgentCatalogDigest = requiredDigest(
    requestedWorkspace.expectedAgentCatalogDigest,
    "Agent workspace catalog digest",
  );
  if (
    expectedAgentCatalogDigest !== scope.agentCatalogDigest
    || requiredAbsolutePath(scope.agentCatalogPath, "Agent workspace catalog path")
      !== clientAgentCatalogPath(octopusHomeRoot)
  ) {
    throw new Error("Agent creation does not match the projected Client Agent catalog");
  }
  const projectedSources = scope.agentProjectSources.filter((source) => (
    stringValue(source?.project_ee_id) === projectEeId
  ));
  if (
    !preset && (projectedSources.length !== 1
    || stringValue(projectedSources[0]?.source_worker_interface) !== sourceWorkerInterfaceId
    )
  ) {
    throw new Error(`Project '${projectEeId}' does not name '${sourceWorkerInterfaceId}' as its exact Agent source`);
  }
  const sourceProfiles = scope.taskAuthoringProfiles.filter((profile) => (
    stringValue(profile?.ee_id) === projectEeId
    && stringValue(profile?.worker_choice_interface) === sourceWorkerInterfaceId
  ));
  if (!preset && sourceProfiles.length !== 1) {
    throw new Error(`Project '${projectEeId}' exposes ${sourceProfiles.length} exact source profiles for '${sourceWorkerInterfaceId}'`);
  }
  const clientTemplate = preset?.settings ?? clientAgentTemplateFromRequest(
    request,
    chatContexts,
    projectEeId,
    sourceWorkerInterfaceId,
  );
  const registryHome = await fs.realpath(registryRoot);
  const interfaceRoot = await fs.realpath(path.join(registryHome, "interface"));
  if (path.dirname(interfaceRoot) !== registryHome || path.basename(interfaceRoot) !== "interface") {
    throw new Error("active interface registry is outside the active Registry");
  }
  const homeSidecarPath = path.join(interfaceRoot, scope.homeInterfaceId, "octopus_os.json");
  const homeSidecar = await readRegularJsonDocument(homeSidecarPath, "Octopus OS Home sidecar");
  const parsedHomeSidecar = parseOctopusOsSidecar(
    homeSidecar.document,
    scope.homeInterfaceId,
    homeSidecarPath,
  );
  const sourceProfile = preset ? [preset.profile] : arrayValue(parsedHomeSidecar.task_authoring_profiles).filter((profile) => (
    stringValue(profile?.ee_id) === projectEeId
    && stringValue(profile?.worker_choice_interface) === sourceWorkerInterfaceId
  ));
  if (sourceProfile.length !== 1) {
    throw new Error(`Home sidecar no longer exposes one source profile for '${sourceWorkerInterfaceId}' in '${projectEeId}'`);
  }
  const agentCatalog = await readClientAgentCatalog(octopusHomeRoot);
  if (
    !agentCatalog.exists
    || agentCatalog.digest !== expectedAgentCatalogDigest
    || agentCatalog.file !== scope.agentCatalogPath
  ) {
    throw new Error("Client Agent catalog changed before the Agent template could be created");
  }
  const workerInterfaceId = `system_worker_template_${crypto.createHash("sha256")
    .update(`${operationId}\0${projectEeId}\0${sourceWorkerInterfaceId}`)
    .digest("hex")
    .slice(0, 32)}`;
  const agentId = `agent_${crypto.createHash("sha256")
    .update(`${operationId}\0${workerInterfaceId}`)
    .digest("hex")
    .slice(0, 32)}`;
  const sourceDirectory = path.join(interfaceRoot, sourceWorkerInterfaceId);
  await requirePlainDirectory(sourceDirectory, `source worker template ${sourceWorkerInterfaceId}`);
  const sourceInterfacePath = path.join(sourceDirectory, "interface.json");
  const sourcePolicyPath = path.join(sourceDirectory, "policy.json");
  const sourceInterface = await readRegularJsonDocument(sourceInterfacePath, `source worker descriptor ${sourceWorkerInterfaceId}`);
  const sourcePolicy = await readRegularJsonDocument(sourcePolicyPath, `source worker policy ${sourceWorkerInterfaceId}`);
  if (
    stringValue(sourceInterface.document?.id) !== sourceWorkerInterfaceId
    || stringValue(sourceInterface.document?.interface_type).toLowerCase() !== "system"
    || stringValue(sourceInterface.document?.role).toLowerCase() !== "worker"
    || stringValue(sourcePolicy.document?.id) !== sourceWorkerInterfaceId
    || !isPlainRecord(sourcePolicy.document?.ceiling)
  ) {
    throw new Error(`source worker template '${sourceWorkerInterfaceId}' is malformed`);
  }
  const targetDirectory = path.join(interfaceRoot, workerInterfaceId);
  const targetInterfacePath = path.join(targetDirectory, "interface.json");
  const targetPolicyPath = path.join(targetDirectory, "policy.json");
  const targetClientTemplatePath = path.join(targetDirectory, "client_template.json");
  let targetDirectoryExists = false;
  try {
    const stat = await fs.lstat(targetDirectory);
    if (stat.isSymbolicLink() || !stat.isDirectory()) {
      throw new Error(`Agent worker template '${workerInterfaceId}' is not a plain directory`);
    }
    targetDirectoryExists = true;
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  const existingInterface = await readOptionalRegularJsonDocument(targetInterfacePath, `Agent worker descriptor ${workerInterfaceId}`);
  const existingPolicy = await readOptionalRegularJsonDocument(targetPolicyPath, `Agent worker policy ${workerInterfaceId}`);
  const existingClientTemplate = await readOptionalRegularJsonDocument(targetClientTemplatePath, `Agent client template ${workerInterfaceId}`);
  const existingProfile = sourceProfileForWorker(parsedHomeSidecar.task_authoring_profiles, projectEeId, workerInterfaceId);
  const existingPresentation = arrayValue(agentCatalog.document.agents).find((agent) => (
    stringValue(agent?.project_ee_id) === projectEeId
    && stringValue(agent?.worker_interface) === workerInterfaceId
  ));
  if (targetDirectoryExists || existingInterface || existingPolicy || existingClientTemplate || existingProfile || existingPresentation) {
    if (!existingInterface || !existingPolicy || !existingClientTemplate || !existingProfile || !existingPresentation) {
      throw new Error(`Agent worker template '${workerInterfaceId}' is only partially authored`);
    }
    const expectedInterface = { ...structuredClone(sourceInterface.document), id: workerInterfaceId };
    const expectedPolicy = { ...structuredClone(sourcePolicy.document), id: workerInterfaceId };
    const expectedProfile = { ...structuredClone(sourceProfile[0]), worker_choice_interface: workerInterfaceId };
    const expectedClientTemplate = {
      ...clientTemplate,
      worker_interface: workerInterfaceId,
    };
    const expectedPresentation = {
      ...existingPresentation,
      id: agentId,
      name: requiredAgentName(request?.name),
      icon: requiredAgentIcon(request?.icon),
      project_ee_id: projectEeId,
      worker_interface: workerInterfaceId,
      deleted_at_ms: 0,
    };
    if (
      stableJson(existingInterface.document) !== stableJson(expectedInterface)
      || stableJson(existingPolicy.document) !== stableJson(expectedPolicy)
      || stableJson(existingClientTemplate.document) !== stableJson(expectedClientTemplate)
      || stableJson(existingProfile) !== stableJson(expectedProfile)
      || stableJson(existingPresentation) !== stableJson(expectedPresentation)
    ) {
      throw new Error(`Agent worker template '${workerInterfaceId}' belongs to another authoring operation`);
    }
    return {
      action: "create_agent_template",
      eeId: projectEeId,
      workerInterfaceId,
      agentId,
      clientTemplateDigest: existingClientTemplate.digest,
      agentCatalogDigest: agentCatalog.digest,
      changed: false,
    };
  }
  if (
    homeSidecar.digest !== expectedHomeSidecarDigest
    || homeSidecar.digest !== scope.homeSidecarDigest
  ) {
    throw new Error("Octopus OS Home changed before the Agent template could be created");
  }
  const nextInterface = { ...structuredClone(sourceInterface.document), id: workerInterfaceId };
  const nextPolicy = { ...structuredClone(sourcePolicy.document), id: workerInterfaceId };
  const nextClientTemplate = { ...clientTemplate, worker_interface: workerInterfaceId };
  const materialized = planClientAgentTemplate({
    operationId, projectEeId, sourceInterface: sourceInterface.document, sourcePolicy: sourcePolicy.document,
    sourceProfile: sourceProfile[0], settings: clientTemplate, name: request.name, icon: request.icon,
  });
  const nextProfile = materialized.profile;
  const nextHomeSidecar = structuredClone(homeSidecar.document);
  nextHomeSidecar.task_authoring_profiles.push(nextProfile);
  parseOctopusOsSidecar(nextHomeSidecar, scope.homeInterfaceId, homeSidecarPath);
  const nextAgentCatalog = structuredClone(agentCatalog.document);
  nextAgentCatalog.agents.push(materialized.presentation);
  parseClientAgentCatalog(nextAgentCatalog, agentCatalog.file);
  const admissionWrites = await planAgentWorkerAdmission(
    registryRoot,
    octopusHomeRoot,
    scope,
    projectEeId,
    sourceWorkerInterfaceId,
    workerInterfaceId,
  );
  await requireUnchangedJsonDigest(sourceInterfacePath, sourceInterface.digest, `source worker descriptor ${sourceWorkerInterfaceId}`);
  await requireUnchangedJsonDigest(sourcePolicyPath, sourcePolicy.digest, `source worker policy ${sourceWorkerInterfaceId}`);
  if (preset) await preset.verify();
  await commitClientAuthoring({
    label: `create Agent worker template ${workerInterfaceId}`,
    writes: [
      { file: targetInterfacePath, beforeDigest: null, next: nextInterface },
      { file: targetPolicyPath, beforeDigest: null, next: nextPolicy },
      { file: targetClientTemplatePath, beforeDigest: null, next: nextClientTemplate },
      { file: homeSidecarPath, beforeDigest: homeSidecar.digest, next: nextHomeSidecar },
      ...admissionWrites,
      { file: agentCatalog.file, beforeDigest: agentCatalog.digest, next: nextAgentCatalog },
    ],
    removals: [],
    publishBarrierPath: agentCatalog.file,
  });
  return {
    action: "create_agent_template",
    eeId: projectEeId,
    workerInterfaceId,
    agentId,
    clientTemplateDigest: jsonDocumentDigest(nextClientTemplate),
    agentCatalogDigest: clientAgentCatalogDigest(nextAgentCatalog),
    changed: true,
  };
}

async function mutateOctopusOsAgentTemplate(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  commitClientAuthoring,
  chatContexts,
) {
  if (scope.status !== "ready") throw new Error("Agent editing requires a ready Octopus OS Home surface");
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const action = stringValue(request?.action);
  const projectEeId = requiredSafeRegistryId(request?.projectEeId, "Agent Project EE");
  const workerInterfaceId = requiredSafeRegistryId(request?.workerInterfaceId, "Agent worker template");
  const agentId = requiredAgentId(request?.agentId);
  if (!scope.byId.has(projectEeId)) {
    throw new Error(`Agent EE '${projectEeId}' is outside the workspace scope`);
  }
  const profiles = scope.taskAuthoringProfiles.filter((profile) => (
    stringValue(profile?.ee_id) === projectEeId
    && stringValue(profile?.worker_choice_interface) === workerInterfaceId
  ));
  if (profiles.length !== 1) {
    throw new Error(`Project '${projectEeId}' exposes ${profiles.length} exact profiles for '${workerInterfaceId}'`);
  }
  const expectedAgentCatalogDigest = requiredDigest(
    request?.expectedAgentCatalogDigest,
    "Agent catalog digest",
  );
  if (expectedAgentCatalogDigest !== scope.agentCatalogDigest) {
    throw new Error("Client Agent catalog changed before the Agent could be edited");
  }
  const agentCatalog = await readClientAgentCatalog(octopusHomeRoot);
  if (
    !agentCatalog.exists
    || agentCatalog.digest !== expectedAgentCatalogDigest
    || agentCatalog.file !== scope.agentCatalogPath
  ) {
    throw new Error("Client Agent catalog changed before the Agent could be edited");
  }
  const presentationMatches = agentCatalog.document.agents.filter((agent) => (
    agent.id === agentId
    && agent.project_ee_id === projectEeId
    && agent.worker_interface === workerInterfaceId
  ));
  if (presentationMatches.length !== 1) {
    throw new Error(`Client Agent catalog exposes ${presentationMatches.length} exact presentations for '${agentId}'`);
  }
  const presentation = presentationMatches[0];
  if (presentation.deleted_at_ms > 0) {
    throw new Error(`Agent '${agentId}' is deleted`);
  }
  const interfaceRoot = await fs.realpath(path.join(await fs.realpath(registryRoot), "interface"));
  const workerDirectory = path.join(interfaceRoot, workerInterfaceId);
  await requirePlainDirectory(workerDirectory, `Agent worker template ${workerInterfaceId}`);
  const templatePath = path.join(workerDirectory, "client_template.json");
  const current = await readRegularJsonDocument(templatePath, `Agent client template ${workerInterfaceId}`);
  const parsed = parseClientWorkerTemplate(current.document, workerInterfaceId, templatePath);
  if (parsed.project_ee_id !== projectEeId) {
    throw new Error(`Agent worker template '${workerInterfaceId}' belongs to another Project`);
  }
  const expectedDigest = requiredDigest(request?.expectedClientTemplateDigest, "Agent client template digest");
  if (current.digest !== expectedDigest) {
    throw new Error(`Agent worker template '${workerInterfaceId}' changed before it could be edited`);
  }
  if (action === "delete_agent_template") {
    const now = Date.now();
    const nextAgentCatalog = structuredClone(agentCatalog.document);
    const presentationIndex = nextAgentCatalog.agents.findIndex((agent) => agent.id === agentId);
    nextAgentCatalog.agents[presentationIndex] = {
      ...nextAgentCatalog.agents[presentationIndex],
      updated_at_ms: now,
      deleted_at_ms: now,
    };
    parseClientAgentCatalog(nextAgentCatalog, agentCatalog.file);
    await commitClientAuthoring({
      label: `delete Agent client template ${workerInterfaceId}`,
      writes: [
        { file: templatePath, beforeDigest: current.digest, next: null },
        { file: agentCatalog.file, beforeDigest: agentCatalog.digest, next: nextAgentCatalog },
      ],
      removals: [],
      publishBarrierPath: agentCatalog.file,
    });
    return {
      action,
      eeId: projectEeId,
      workerInterfaceId,
      agentId,
      agentCatalogDigest: clientAgentCatalogDigest(nextAgentCatalog),
      changed: true,
    };
  }
  const next = {
    ...clientAgentTemplateFromRequest(
      request,
      chatContexts,
      projectEeId,
      workerInterfaceId,
    ),
    worker_interface: workerInterfaceId,
  };
  const nextName = requiredAgentName(request?.name);
  const nextIcon = requiredAgentIcon(request?.icon);
  if (
    stableJson(current.document) === stableJson(next)
    && presentation.name === nextName
    && presentation.icon === nextIcon
  ) {
    return {
      action,
      eeId: projectEeId,
      workerInterfaceId,
      agentId,
      clientTemplateDigest: current.digest,
      agentCatalogDigest: agentCatalog.digest,
      changed: false,
    };
  }
  const nextAgentCatalog = structuredClone(agentCatalog.document);
  const presentationIndex = nextAgentCatalog.agents.findIndex((agent) => agent.id === agentId);
  nextAgentCatalog.agents[presentationIndex] = {
    ...nextAgentCatalog.agents[presentationIndex],
    name: nextName,
    icon: nextIcon,
    updated_at_ms: Date.now(),
  };
  parseClientAgentCatalog(nextAgentCatalog, agentCatalog.file);
  await commitClientAuthoring({
    label: `configure Agent client template ${workerInterfaceId}`,
    writes: [
      { file: templatePath, beforeDigest: current.digest, next },
      { file: agentCatalog.file, beforeDigest: agentCatalog.digest, next: nextAgentCatalog },
    ],
    removals: [],
    publishBarrierPath: agentCatalog.file,
  });
  return {
    action,
    eeId: projectEeId,
    workerInterfaceId,
    agentId,
    clientTemplateDigest: jsonDocumentDigest(next),
    agentCatalogDigest: clientAgentCatalogDigest(nextAgentCatalog),
    changed: true,
  };
}

function clientAgentTemplateFromRequest(request, chatContexts, projectEeId, workerInterfaceId) {
  const context = arrayValue(chatContexts?.contexts).find((candidate) => (
    stringValue(candidate?.ee_id) === projectEeId
  ));
  const worker = arrayValue(context?.workers).find((candidate) => (
    stringValue(candidate?.id) === workerInterfaceId
    && candidate?.authoring_profile === true
  ));
  if (!worker) {
    throw new Error(`Agent source worker '${workerInterfaceId}' is not projected in Project '${projectEeId}'`);
  }
  const providerInterfaceId = requiredSafeRegistryId(request?.providerInterfaceId, "Agent provider interface");
  const providerIds = new Set(arrayValue(worker.provider_interface_ids).map(stringValue).filter(Boolean));
  if (!providerIds.has(providerInterfaceId)) {
    throw new Error(`Agent provider '${providerInterfaceId}' is not admitted by worker template '${workerInterfaceId}'`);
  }
  const providerModel = requiredAgentModel(request?.providerModel);
  const admittedModels = worker.interface_policy?.[providerInterfaceId]?.models;
  if (!Array.isArray(admittedModels) || !admittedModels.includes(providerModel)) {
    throw new Error(`Agent model '${providerModel}' is not admitted by worker template '${workerInterfaceId}'`);
  }
  const reasoningEffort = stringValue(request?.reasoningEffort);
  const serviceTier = request?.serviceTier === undefined || request.serviceTier === "" ? "" : requiredString(request.serviceTier, "Agent service tier");
  if (serviceTier.length > 80 || /[\u0000-\u001f\u007f]/.test(serviceTier)) throw new Error("Agent service tier is invalid");
  if (reasoningEffort.length > 80 || /[\u0000-\u001f\u007f]/.test(reasoningEffort)) {
    throw new Error("Agent reasoning effort is invalid");
  }
  const applicationIds = parseUniqueRegistryIds(request?.applicationIds, "Agent application").sort();
  const availableApplicationIds = new Set(arrayValue(worker.applications).map((application) => stringValue(application?.id)).filter(Boolean));
  const unavailableApplicationIds = applicationIds.filter((id) => !availableApplicationIds.has(id));
  if (unavailableApplicationIds.length) {
    throw new Error(`Agent applications are not admitted by the worker template: ${unavailableApplicationIds.join(", ")}`);
  }
  const workerEnvironmentKinds = parseUniqueRegistryIds(request?.driverNames, "Agent worker environment").sort();
  const availableWorkerEnvironmentKinds = new Set(arrayValue(worker.drivers).map((driver) => stringValue(driver?.name)).filter(Boolean));
  const unavailableWorkerEnvironmentKinds = workerEnvironmentKinds.filter((id) => !availableWorkerEnvironmentKinds.has(id));
  if (unavailableWorkerEnvironmentKinds.length) {
    throw new Error(`Agent Worker Environments are not admitted by the worker template: ${unavailableWorkerEnvironmentKinds.join(", ")}`);
  }
  return {
    schema_version: 1,
    worker_interface: workerInterfaceId,
    project_ee_id: projectEeId,
    provider_interface_id: providerInterfaceId,
    provider_model: providerModel,
    reasoning_effort: reasoningEffort,
    ...(serviceTier ? { service_tier: serviceTier } : {}),
    application_ids: applicationIds,
    worker_environment_kinds: workerEnvironmentKinds,
  };
}

function requiredAgentModel(value) {
  if (typeof value !== "string" || !value || value.trim() !== value || value.length > 512 || /[\u0000-\u001f\u007f]/.test(value)) {
    throw new Error("Choose an explicit Agent provider model.");
  }
  return value;
}

function requiredAgentId(value) {
  return requiredClientAgentId(value, "Agent id");
}

function requiredAgentName(value) {
  return requiredClientAgentName(value, "Agent name");
}

function requiredAgentIcon(value) {
  return requiredClientAgentIcon(value, "Agent icon");
}

async function planAgentWorkerAdmission(
  registryRoot,
  octopusHomeRoot,
  scope,
  projectEeId,
  sourceWorkerInterfaceId,
  workerInterfaceId,
) {
  const chain = scopeEnvironmentChain(projectEeId, scope.byId, scope.rootEeId);
  const registryHome = await fs.realpath(registryRoot);
  const templateRoot = await fs.realpath(path.join(registryHome, "ee_template"));
  if (path.dirname(templateRoot) !== registryHome || path.basename(templateRoot) !== "ee_template") {
    throw new Error("active EE template registry is outside the active Registry");
  }
  const templatePlans = new Map();
  const instanceWrites = [];
  for (const environment of chain) {
    const eeId = requiredSafeRegistryId(environment.ee_id, "Agent admission EE");
    const templateName = requiredSafeRegistryId(environment.template_name, `Agent admission EE '${eeId}' template`);
    if (!templatePlans.has(templateName)) {
      const file = path.join(templateRoot, templateName, "ee_template.json");
      const current = await readRegularJsonDocument(file, `Agent admission EE template ${templateName}`);
      if (stringValue(current.document?.id ?? current.document?.name) !== templateName) {
        throw new Error(`Agent admission EE template '${templateName}' has drifted identity`);
      }
      const next = structuredClone(current.document);
      appendExactAgentWorkerAdmission(
        next,
        "allowed_interfaces",
        sourceWorkerInterfaceId,
        workerInterfaceId,
        `EE template '${templateName}'`,
      );
      templatePlans.set(templateName, { file, current, next });
    }
    const recordPath = await scopedRecordPath(
      octopusHomeRoot,
      environment.record_path,
      path.join("state", "ee_instances"),
      `${eeId}.json`,
    );
    const current = await readRegularJsonDocument(recordPath, `Agent admission EE ${eeId}`);
    const projectedDigest = requiredDigest(environment.record_digest, `Agent admission EE '${eeId}' digest`);
    if (current.digest !== projectedDigest) {
      throw new Error(`Agent admission EE '${eeId}' changed before the worker template could be admitted`);
    }
    if (
      stringValue(current.document?.ee_id) !== eeId
      || stringValue(current.document?.template_name) !== templateName
    ) {
      throw new Error(`Agent admission EE '${eeId}' has drifted identity`);
    }
    const next = structuredClone(current.document);
    if (next.policy_narrowing !== undefined) {
      if (!isPlainRecord(next.policy_narrowing)) {
        throw new Error(`Agent admission EE '${eeId}'.policy_narrowing must be an object`);
      }
      appendExactAgentWorkerAdmission(
        next.policy_narrowing,
        "allowed_interfaces",
        sourceWorkerInterfaceId,
        workerInterfaceId,
        `EE '${eeId}' policy narrowing`,
      );
    }
    if (stableJson(next) !== stableJson(current.document)) {
      instanceWrites.push({ file: recordPath, beforeDigest: current.digest, next });
    }
  }
  return [
    ...[...templatePlans.values()].flatMap(({ file, current, next }) => (
      stableJson(next) === stableJson(current.document)
        ? []
        : [{ file, beforeDigest: current.digest, next }]
    )),
    ...instanceWrites,
  ];
}


function jsonDocumentDigest(value) {
  return crypto.createHash("sha256").update(`${JSON.stringify(value, null, 2)}\n`).digest("hex");
}

function sourceProfileForWorker(profiles, eeId, workerInterfaceId) {
  const matches = arrayValue(profiles).filter((profile) => (
    stringValue(profile?.ee_id) === eeId
    && stringValue(profile?.worker_choice_interface) === workerInterfaceId
  ));
  if (matches.length > 1) throw new Error(`Home sidecar repeats worker template '${workerInterfaceId}' in '${eeId}'`);
  return matches[0] ?? null;
}

async function mutateLocalPackageInventory(
  registryRoot,
  octopusHomeRoot,
  _scope,
  request,
  hooks,
) {
  const action = stringValue(request?.action);
  const roots = {
    sourceRegistryRoot: requiredAbsolutePath(
      hooks.localPackageRegistryRoot,
      "local package registry",
    ),
    activeRegistryRoot: requiredAbsolutePath(registryRoot, "active registry"),
    libraryRoot: requiredAbsolutePath(
      hooks.localPackageLibraryRoot,
      "local package library",
    ),
    stateRoot: path.join(octopusHomeRoot, "client", "package-state"),
  };
  const operationId = requiredApplicationOperationId(request?.operationId);
  const onProgress = (progress) => reportLocalPackageProgress(
    hooks.onApplicationProgress,
    progress,
  );
  if (action === "apply_application_pack") {
    return applyLocalApplicationPack({
      ...roots,
      operationId,
      packId: requiredSafeRegistryId(request?.packId, "packId"),
      selectedRecommended: arrayValue(request?.selectedRecommended),
      expectedSourceDigests: objectOrEmpty(request?.expectedSourceDigests),
      assertMutable: hooks.assertLocalPackageMutation,
      onProgress,
    });
  }
  if (action === "repair_package") {
    const packageIds = arrayValue(request?.packageIds).map((packageId, index) => (
      requiredLocalPackageId(packageId, `packageIds[${index}]`)
    ));
    if (packageIds.length === 0 || new Set(packageIds).size !== packageIds.length) {
      throw new Error("packageIds must contain unique local package ids");
    }
    return repairLocalPackages({
      ...roots,
      operationId,
      packageIds,
      expectedRepairs: objectOrEmpty(request?.expectedRepairs),
      assertMutable: hooks.assertLocalPackageMutation,
      onProgress,
    });
  }
  let packageIds;
  let expectedSourceDigests = {};
  let expectedInstalledDigests = {};
  let capabilityId = "";
  if (action === "install_application" || action === "remove_application") {
    capabilityId = requiredSafeRegistryId(request?.applicationId, "applicationId");
    packageIds = [capabilityId];
    if (action === "install_application") {
      expectedSourceDigests[capabilityId] = requiredDigest(request?.expectedSourceDigest, "expectedSourceDigest");
    } else {
      expectedInstalledDigests[capabilityId] = requiredDigest(request?.expectedInstalledDigest, "expectedInstalledDigest");
    }
  } else if (action === "install_driver" || action === "remove_driver") {
    capabilityId = requiredSafeRegistryId(request?.driverName, "driverName");
    packageIds = [`driver.${capabilityId}`];
    if (action === "install_driver") {
      expectedSourceDigests[packageIds[0]] = requiredDigest(request?.expectedSourceDigest, "expectedSourceDigest");
    } else {
      expectedInstalledDigests[packageIds[0]] = requiredDigest(request?.expectedInstalledDigest, "expectedInstalledDigest");
    }
  } else {
    packageIds = arrayValue(request?.packageIds).map((packageId, index) => (
      requiredLocalPackageId(packageId, `packageIds[${index}]`)
    ));
    if (packageIds.length === 0 || new Set(packageIds).size !== packageIds.length) {
      throw new Error("packageIds must contain unique local package ids");
    }
    if (action === "install_package") expectedSourceDigests = objectOrEmpty(request?.expectedSourceDigests);
    else expectedInstalledDigests = objectOrEmpty(request?.expectedInstalledDigests);
  }
  const install = action === "install_application" || action === "install_driver" || action === "install_package";
  const outcome = install
    ? await installLocalPackages({
        ...roots,
        operationId,
        packageIds,
        expectedSourceDigests,
        assertMutable: hooks.assertLocalPackageMutation,
        onProgress,
      })
    : await removeLocalPackages({
        ...roots,
        operationId,
        packageIds,
        expectedInstalledDigests,
        assertMutable: hooks.assertLocalPackageMutation,
        onProgress,
        assertRemovable: async (manifest) => {
          const excludedDirectories = new Set(manifest.members.map((member) => (
            path.join(registryRoot, member.registry_kind, member.owner)
          )));
          await assertApplicationPackageUnreferenced(
            await fs.realpath(registryRoot),
            octopusHomeRoot,
            excludedDirectories,
            new Set(manifest.members.map((member) => member.owner)),
            manifest.id,
          );
        },
      });
  return {
    ...outcome,
    action,
    ...(capabilityId ? { capabilityId } : {}),
  };
}

function reportLocalPackageProgress(onProgress, progress) {
  if (typeof onProgress !== "function") return;
  onProgress({
    ...progress,
    applicationId: stringValue(progress?.packageId),
  });
}

function requireOctopusOsScope(scope) {
  if (!isPlainRecord(scope) || !["ready", "awaiting_home"].includes(stringValue(scope.status))) {
    throw new Error("Octopus OS scope is not writable");
  }
  const rootEeId = requiredSafeRegistryId(scope.root_ee_id, "Octopus OS root EE");
  const environments = arrayValue(scope.environments);
  const byId = new Map();
  for (const environment of environments) {
    const eeId = requiredSafeRegistryId(environment?.ee_id, "scope EE id");
    if (byId.has(eeId)) throw new Error(`Octopus OS scope repeats EE '${eeId}'`);
    byId.set(eeId, environment);
  }
  if (!byId.has(rootEeId)) throw new Error("Octopus OS root EE is absent from its scope");
  return {
    status: stringValue(scope.status),
    rootEeId,
    homeRunId: stringValue(scope.home_run_id),
    homeInterfaceId: stringValue(scope.home_interface_id),
    homeSidecarDigest: stringValue(scope.home_sidecar_digest),
    agentCatalogPath: stringValue(scope.agent_catalog_path),
    agentCatalogDigest: stringValue(scope.agent_catalog_digest),
    agentProjectSources: arrayValue(scope.agent_project_sources),
    agentPresentations: arrayValue(scope.agent_presentations),
    environments,
    byId,
    clientRuns: arrayValue(scope.client_runs),
    taskAuthoringProfiles: arrayValue(scope.task_authoring_profiles),
    readOnlyRuns: arrayValue(scope.read_only_runs),
  };
}

function workspaceProjectMutationScope(scope) {
  if (scope.status !== "ready") {
    throw new Error(`Workspace Project authoring requires ready scope, found '${scope.status}'`);
  }
  return {
    status: "ready",
    rootEeId: requiredSafeRegistryId(scope.rootEeId, "Workspace Project root EE"),
    homeRunId: requiredSafeRegistryId(scope.homeRunId, "Workspace Project Home run"),
    homeInterfaceId: requiredSafeRegistryId(scope.homeInterfaceId, "Workspace Project Home interface"),
    homeSidecarDigest: requiredDigest(scope.homeSidecarDigest, "Workspace Project Home sidecar digest"),
    environments: scope.environments.map((environment) => ({
      eeId: requiredSafeRegistryId(environment?.ee_id, "Workspace Project EE"),
      parentEeId: stringValue(environment?.parent_ee_id),
      templateName: requiredSafeRegistryId(environment?.template_name, "Workspace Project EE template"),
      recordDigest: requiredDigest(environment?.record_digest, "Workspace Project EE digest"),
      allowedChildTemplateIds: arrayValue(environment?.allowed_child_ee_templates)
        .map((value) => requiredSafeRegistryId(value, "Workspace Project child template"))
        .sort(),
    })),
    clientRuns: scope.clientRuns.map((run) => ({
      runId: requiredSafeRegistryId(run?.run_id, "Workspace Project client run"),
      eeId: requiredSafeRegistryId(run?.ee_id, "Workspace Project client run EE"),
    })),
    readOnlyRuns: scope.readOnlyRuns.map((run) => ({
      runId: requiredSafeRegistryId(run?.run_id, "Workspace Project read-only run"),
      eeId: requiredSafeRegistryId(run?.ee_id, "Workspace Project read-only run EE"),
    })),
  };
}

function readClientTaskArchive(run, clientInterfaceId) {
  const state = run.interface_state?.[clientInterfaceId];
  if (state?.client_archive === undefined) return null;
  const label = `task '${run.id}' archive`;
  if (state.client_surface_role !== "task") throw new Error(`${label} belongs to a non-task client`);
  const archive = state.client_archive;
  exactObjectKeys(archive,
    ["schema_version", "active_checkpoints", "checkpoints_digest", "archived_at_ms"],
    ["schema_version", "active_checkpoints", "checkpoints_digest", "archived_at_ms"], label);
  if (archive.schema_version !== 1 || !Number.isSafeInteger(archive.archived_at_ms) || archive.archived_at_ms <= 0) {
    throw new Error(`${label} has invalid metadata`);
  }
  if (!Array.isArray(run.active_checkpoints) || run.active_checkpoints.length !== 0) {
    throw new Error(`${label} still admits artifacts through active checkpoints`);
  }
  if (!Array.isArray(archive.active_checkpoints) || !Array.isArray(run.checkpoints)) {
    throw new Error(`${label} has no saved checkpoint frontier`);
  }
  const savedIds = archive.active_checkpoints.map((id) => requiredSafeRegistryId(id, `${label} checkpoint`));
  const checkpointIds = new Set(run.checkpoints.map((checkpoint) => checkpoint.id));
  if (new Set(savedIds).size !== savedIds.length || savedIds.some((id) => !checkpointIds.has(id))) {
    throw new Error(`${label} has duplicate or missing checkpoints`);
  }
  if (requiredDigest(archive.checkpoints_digest, `${label} topology digest`)
    !== crypto.createHash("sha256").update(JSON.stringify(run.checkpoints)).digest("hex")) {
    throw new Error(`${label} checkpoint topology changed; restore is refused`);
  }
  if (!run.checkpoints.some((checkpoint) => interfaceBindingId(checkpoint.interface_binding) === clientInterfaceId)) {
    throw new Error(`${label} has no exact client checkpoint`);
  }
  return archive;
}

async function setOctopusOsTaskArchived(registryRoot, octopusHomeRoot, scope, request, hooks) {
  exactObjectKeys(request, ["action", "home", "workspace", "runId", "expectedDigest", "archived"],
    ["action", "home", "workspace", "runId", "expectedDigest", "archived"], "Task archive request");
  exactObjectKeys(request.workspace, ["rootEeId", "homeRunId", "homeInterfaceId"],
    ["rootEeId", "homeRunId", "homeInterfaceId"], "Task archive workspace");
  if (request.home !== octopusHomeRoot || request.workspace.rootEeId !== scope.rootEeId
    || request.workspace.homeRunId !== scope.homeRunId || request.workspace.homeInterfaceId !== scope.homeInterfaceId) {
    throw new Error("Task archive request does not match the current Home and workspace");
  }
  const runId = requiredSafeRegistryId(request.runId, "runId");
  const archived = requiredBoolean(request.archived, "archived");
  const expectedDigest = requiredDigest(request.expectedDigest, "expectedDigest");
  const projected = scope.clientRuns.find((row) => row.run_id === runId && row.surface_role === "task");
  if (!projected || runId === scope.homeRunId) throw new Error(`task '${runId}' is not an archivable Octopus OS task`);
  if (!scope.byId.has(projected.ee_id)) throw new Error(`task '${runId}' is outside the Octopus OS scope`);
  if (typeof hooks.withTaskAuthoringLock !== "function" || typeof hooks.readTaskStatus !== "function"
    || typeof hooks.commitClientAuthoring !== "function") {
    throw new Error("task archive authoring is unavailable");
  }
  return hooks.withTaskAuthoringLock(runId, async () => {
    const runPath = await scopedRecordPath(octopusHomeRoot, projected.record_path, path.join("state", "runs"), `${runId}.json`);
    const current = await readRegularJsonDocument(runPath, `task run ${runId}`);
    if (current.digest !== expectedDigest || projected.record_digest !== expectedDigest) {
      throw new Error(`task '${runId}' changed before its archive state could be edited`);
    }
    const next = current.document;
    const clientInterfaceId = requiredSafeRegistryId(projected.interface_id, "task client interface");
    const clientState = next.interface_state?.[clientInterfaceId];
    if (next.id !== runId || next.ee !== projected.ee_id || !isPlainRecord(clientState)
      || clientState.client_surface_role !== "task" || !Array.isArray(next.checkpoints) || !Array.isArray(next.active_checkpoints)) {
      throw new Error(`task '${runId}' has malformed authored topology`);
    }
    const archive = readClientTaskArchive(next, clientInterfaceId);
    const exactInterfaces = await taskArchiveInterfaceIds(registryRoot, next, clientInterfaceId, runId);
    assertRunPrivateStateIsOwned(next, exactInterfaces, runId);
    const ids = next.checkpoints.map((checkpoint) => requiredSafeRegistryId(checkpoint.id, "task checkpoint"));
    const activeIds = next.active_checkpoints.map((id) => requiredSafeRegistryId(id, "active task checkpoint"));
    if (new Set(ids).size !== ids.length || new Set(activeIds).size !== activeIds.length || activeIds.some((id) => !ids.includes(id))) {
      throw new Error(`task '${runId}' has duplicated or missing checkpoints`);
    }
    const providerId = requiredSafeRegistryId(clientState.provider_runtime_interface, "task provider interface");
    const providerState = next.interface_state[providerId];
    if (!isPlainRecord(providerState)) throw new Error(`task '${runId}' has no exact provider state`);
    if (providerState.pending_handoff !== undefined) {
      if (!isPlainRecord(providerState.pending_handoff) || providerState.pending_handoff.terminal !== true) {
        throw new Error(`task '${runId}' is working; stop the task before archiving`);
      }
    } else if (numberValue(providerState.authoring_generation, 0) > 0) {
      throw new Error(`task '${runId}' lost its provider handoff evidence`);
    }
    if (archived === Boolean(archive)) return { action: "set_task_archived", runId, archived, changed: false };
    if (archived) {
      const status = await hooks.readTaskStatus(runId);
      if (!status || status.run_id !== runId || status.turn_open !== false || !["idle", "failed"].includes(status.turn_state)) {
        throw new Error(`task '${runId}' has an open turn; stop the task before archiving`);
      }
      clientState.client_archive = {
        schema_version: 1,
        active_checkpoints: [...activeIds],
        checkpoints_digest: crypto.createHash("sha256").update(JSON.stringify(next.checkpoints)).digest("hex"),
        archived_at_ms: Date.now(),
      };
      next.active_checkpoints = [];
      readClientTaskArchive(next, clientInterfaceId);
    } else {
      next.active_checkpoints = [...archive.active_checkpoints];
      delete clientState.client_archive;
    }
    await hooks.commitClientAuthoring({
      label: archived ? "archive Octopus OS task" : "restore Octopus OS task",
      writes: [{ file: runPath, beforeDigest: expectedDigest, next }], removals: [], publishBarrierPath: runPath,
    });
    return { action: "set_task_archived", runId, archived, changed: true };
  });
}

async function configureOctopusOsTaskApplication(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  withTaskAuthoringLock,
  planTaskApplicationAmendment,
  commitClientAuthoring,
) {
  if (typeof withTaskAuthoringLock !== "function") {
    throw new Error("operator chat task authoring lock is unavailable");
  }
  if (typeof planTaskApplicationAmendment !== "function") {
    throw new Error("operator chat task capability planner is unavailable");
  }
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const runId = requiredSafeRegistryId(request?.runId, "runId");
  const runProjection = scope.clientRuns.find((row) => (
    stringValue(row?.run_id) === runId && stringValue(row?.surface_role) === "task"
  ));
  if (!runProjection) throw new Error(`task '${runId}' is not a writable Octopus OS task`);
  const expectedDigest = requiredDigest(request?.expectedDigest, "expectedDigest");
  const applicationId = requiredSafeRegistryId(request?.applicationId, "applicationId");
  const connected = requiredBoolean(request?.connected, "connected");
  const environment = scope.byId.get(requiredSafeRegistryId(runProjection.ee_id, "task EE"));
  if (!environment) throw new Error(`task '${runId}' belongs to an EE outside the Octopus OS scope`);
  const applicationPath = path.join(
    await fs.realpath(path.join(registryRoot, "interface")),
    applicationId,
    "application.json",
  );
  const applicationDocument = await readRegularJsonDocument(
    applicationPath,
    `application manifest ${applicationId}`,
  );
  const manifest = parseApplicationManifest(
    applicationDocument.document,
    applicationId,
    applicationPath,
  );
  const installedInterfaceIds = new Set(await installedDescriptorIds(
    path.join(registryRoot, "interface"),
    "interface.json",
    ["id", "rule_id"],
  ));
  const missingInterfaceIds = manifest.interfaces.filter((interfaceId) => (
    !installedInterfaceIds.has(interfaceId)
  ));
  if (connected && missingInterfaceIds.length) {
    throw new Error(
      `application '${applicationId}' references missing interfaces: ${missingInterfaceIds.join(", ")}`,
    );
  }
  const allowedInterfaces = objectOrEmpty(environment.formed_envelope).allowed_interfaces;
  if (
    connected
    && Array.isArray(allowedInterfaces)
    && manifest.interfaces.some((interfaceId) => !allowedInterfaces.includes(interfaceId))
  ) {
    throw new Error(`application '${applicationId}' is not available in task EE '${environment.ee_id}'`);
  }
  return withTaskAuthoringLock(runId, async () => {
    const runPath = await scopedRecordPath(
      octopusHomeRoot,
      runProjection.record_path,
      path.join("state", "runs"),
      `${runId}.json`,
    );
    const current = await readRegularJsonDocument(runPath, `task run ${runId}`);
    if (current.digest !== expectedDigest || stringValue(runProjection.record_digest) !== expectedDigest) {
      throw new Error(`task '${runId}' changed before its application binding could be edited`);
    }
    const next = current.document;
    if (
      stringValue(next.id) !== runId
      || stringValue(next.ee) !== stringValue(runProjection.ee_id)
      || !Array.isArray(next.checkpoints)
      || !Array.isArray(next.active_checkpoints)
      || !isPlainRecord(next.interface_state)
    ) {
      throw new Error(`task '${runId}' has malformed authored topology`);
    }
    const chatInterface = requiredSafeRegistryId(runProjection.interface_id, "task client interface");
    const chatState = objectOrEmpty(next.interface_state[chatInterface]);
    if (readClientTaskArchive(next, chatInterface)) throw new Error(`task '${runId}' is archived; restore it before changing applications`);
    const providerInterface = requiredSafeRegistryId(
      chatState.provider_runtime_interface,
      "task provider interface",
    );
    const providerState = next.interface_state[providerInterface];
    if (!isPlainRecord(providerState)) {
      throw new Error(`task '${runId}' has no exact provider state`);
    }
    if (providerState.pending_handoff !== undefined) {
      if (!isPlainRecord(providerState.pending_handoff) || providerState.pending_handoff.terminal !== true) {
        throw new Error(`task '${runId}' is working; applications can be changed after the turn settles`);
      }
    } else if (numberValue(providerState.authoring_generation, 0) > 0) {
      throw new Error(`task '${runId}' lost its provider handoff evidence`);
    }
    const activeIds = new Set(next.active_checkpoints.map((value) => (
      requiredSafeRegistryId(value, `task '${runId}' active checkpoint`)
    )));
    if (activeIds.size !== next.active_checkpoints.length) {
      throw new Error(`task '${runId}' repeats an active checkpoint`);
    }
    const checkpointIds = new Set();
    for (const checkpoint of next.checkpoints) {
      if (!isPlainRecord(checkpoint)) throw new Error(`task '${runId}' contains a malformed checkpoint`);
      const checkpointId = requiredSafeRegistryId(checkpoint.id, `task '${runId}' checkpoint`);
      if (checkpointIds.has(checkpointId) || stringValue(checkpoint.run) !== runId) {
        throw new Error(`task '${runId}' contains duplicated or foreign checkpoint '${checkpointId}'`);
      }
      checkpointIds.add(checkpointId);
    }
    if ([...activeIds].some((checkpointId) => !checkpointIds.has(checkpointId))) {
      throw new Error(`task '${runId}' activates a checkpoint absent from its record`);
    }
    const exactInterfaces = await octopusOsOwnedInterfaceIds(registryRoot, chatInterface);
    assertRunPrivateStateIsOwned(next, exactInterfaces, runId);
    const workerInterface = requiredSafeRegistryId(next.worker, "task worker interface");
    await assertTaskWorkerPinRemainsTaskLocal(
      octopusHomeRoot,
      workerInterface,
      chatInterface,
      next,
      exactInterfaces,
    );
    const installedApplicationDocuments = await readInstalledApplicationManifestDocuments(registryRoot);
    const chatSidecarPath = path.join(
      await fs.realpath(path.join(registryRoot, "interface")),
      chatInterface,
      "octopus_os.json",
    );
    const chatSidecar = await readRegularJsonDocument(chatSidecarPath, `task client sidecar ${chatInterface}`);
    const parsedSidecar = parseOctopusOsSidecar(chatSidecar.document, chatInterface, chatSidecarPath);
    const profile = objectOrEmpty(parsedSidecar.authoring_profile);
    const currentConnected = applicationIsConnectedToAuthoredRun(next, manifest, profile);
    const currentPresent = applicationHasAnyAuthoredBinding(next, manifest);
    if ((connected && currentConnected) || (!connected && !currentPresent)) {
      return {
        action: "configure_task_application",
        runId,
        capabilityId: applicationId,
        connected,
        changed: false,
      };
    }
    const retainedApplicationDocuments = [];
    for (const candidate of installedApplicationDocuments) {
      if (
        candidate.manifest.id === applicationId
        || !applicationHasAnyAuthoredBinding(next, candidate.manifest)
      ) continue;
      const candidateMissingInterfaces = candidate.manifest.interfaces.filter((interfaceId) => (
        !installedInterfaceIds.has(interfaceId)
      ));
      if (
        candidateMissingInterfaces.length
        || !applicationIsConnectedToAuthoredRun(next, candidate.manifest, profile)
      ) {
        throw new Error(
          `task '${runId}' has an incomplete connection to application '${candidate.manifest.id}'; select that application in this task to repair it before changing another application`,
        );
      }
      retainedApplicationDocuments.push(candidate);
    }
    const retainedManifests = retainedApplicationDocuments.map((candidate) => candidate.manifest);
    const selectedManifests = connected
      ? [...retainedManifests, manifest]
      : retainedManifests;
    const baseAuthoringProfile = connected
      ? null
      : taskBaseAuthoringProfile(scope, runProjection, profile);
    const baseExternalInputs = new Set(arrayValue(baseAuthoringProfile?.external_inputs).flatMap((input) => {
      const interfaceId = stringValue(input?.interface_id);
      const checkpointId = stringValue(input?.checkpoint_id);
      return interfaceId && checkpointId ? [`${checkpointId}\0${interfaceId}`] : [];
    }));
    const targetInterfaceIds = new Set(manifest.interfaces);
    const externalInputs = arrayValue(profile.external_inputs).flatMap((input) => {
      const interfaceId = stringValue(input?.interface_id);
      const checkpointId = stringValue(input?.checkpoint_id);
      return interfaceId && checkpointId && (
        !targetInterfaceIds.has(interfaceId)
        || baseExternalInputs.has(`${checkpointId}\0${interfaceId}`)
      )
        ? [{
            checkpointId,
            interfaceId,
            requiredVerifiedArtifacts: projectOctopusOsRequiredVerifiedArtifacts(
              input?.required_verified_artifacts,
            ),
          }]
        : [];
    });
    const applicationCheckpointIds = new Set(externalInputs.map((input) => input.checkpointId));
    const inputInterfaces = new Set(externalInputs.map((input) => input.interfaceId));
    for (const selected of selectedManifests) {
      selected.interfaces.forEach((interfaceId, index) => {
        if (inputInterfaces.has(interfaceId)) return;
        const checkpointId = applicationTaskCheckpointId(selected.id, "input", index);
        if (applicationCheckpointIds.has(checkpointId)) {
          throw new Error(`applications repeat task checkpoint '${checkpointId}'`);
        }
        applicationCheckpointIds.add(checkpointId);
        inputInterfaces.add(interfaceId);
        externalInputs.push({ checkpointId, interfaceId });
      });
    }
    const allApplicationArtifactIds = new Set(installedApplicationDocuments.flatMap((candidate) => (
      candidate.manifest.artifact_bindings.map((binding) => binding.artifact_id)
    )));
    const baseContextArtifactIds = new Set(arrayValue(baseAuthoringProfile?.context_artifact_bindings)
      .map((binding) => stringValue(binding?.id))
      .filter(Boolean));
    const contextArtifactBindings = [...new Set([
      ...[...taskProviderContextArtifactIds(next, activeIds, providerInterface)]
        .filter((artifactId) => (
          !allApplicationArtifactIds.has(artifactId)
          || baseContextArtifactIds.has(artifactId)
        )),
      ...selectedManifests.flatMap((selected) => (
        selected.artifact_bindings.map((binding) => binding.artifact_id)
      )),
    ])].sort().map((id) => ({ id }));
    const baseSelectedTools = new Set(arrayValue(baseAuthoringProfile?.selected_tools).flatMap((capability) => {
      const tool = stringValue(capability?.tool);
      const interfaceId = stringValue(capability?.interface);
      const workerEnvironmentKind = stringValue(capability?.worker_environment_kind);
      return tool && interfaceId && workerEnvironmentKind
        ? [`${tool}\0${interfaceId}\0${workerEnvironmentKind}`]
        : [];
    }));
    const selectedTools = arrayValue(profile.selected_tools).flatMap((capability) => {
      const tool = stringValue(capability?.tool);
      const interfaceId = stringValue(capability?.interface);
      const workerEnvironmentKind = stringValue(capability?.worker_environment_kind);
      const exactKey = `${tool}\0${interfaceId}\0${workerEnvironmentKind}`;
      const targetOwns = manifest.task_capabilities.tools.some((candidate) => (
        candidate.tool === tool
        && candidate.interface === interfaceId
        && candidate.worker_environment_kind === workerEnvironmentKind
      ));
      return tool && interfaceId && workerEnvironmentKind
        && (!targetOwns || connected || baseSelectedTools.has(exactKey))
        ? [{ tool, interface: interfaceId, workerEnvironmentKind }]
        : [];
    });
    const selectedToolOwners = new Map();
    for (const capability of selectedTools) {
      selectedToolOwners.set(
        capability.tool,
        `${capability.interface}\0${capability.workerEnvironmentKind}`,
      );
    }
    for (const selected of selectedManifests) {
      for (const capability of selected.task_capabilities.tools) {
        const key = `${capability.interface}\0${capability.worker_environment_kind}`;
        const owner = selectedToolOwners.get(capability.tool);
        if (owner && owner !== key) {
          throw new Error(`applications bind tool '${capability.tool}' to different exact owners`);
        }
        if (!owner) selectedTools.push({
          tool: capability.tool,
          interface: capability.interface,
          workerEnvironmentKind: capability.worker_environment_kind,
        });
        selectedToolOwners.set(capability.tool, key);
      }
    }
    const amendment = await planTaskApplicationAmendment({
      runId,
      expectedRunHash: current.digest,
      doorHostSlots: objectOrEmpty(profile.door_host_slots),
      externalInputs,
      externalRelays: arrayValue(profile.external_relays).map((relay) => ({
        checkpointId: stringValue(relay?.checkpoint_id),
        interfaceId: stringValue(relay?.interface_id),
        outboundInterfaceIds: arrayValue(relay?.outbound_interface_ids).map(stringValue).filter(Boolean),
        requiredVerifiedArtifacts: projectOctopusOsRequiredVerifiedArtifacts(
          relay?.required_verified_artifacts,
        ),
      })),
      modelOutputs: arrayValue(profile.model_outputs).map((output) => ({
        checkpointId: stringValue(output?.checkpoint_id),
        interfaceId: stringValue(output?.interface_id),
        requiredVerifiedArtifact: {
          mark: stringValue(output?.required_verified_artifact?.mark),
          artifactLabel: stringValue(output?.required_verified_artifact?.artifact_label),
          verifier: stringValue(output?.required_verified_artifact?.verifier),
          params: objectOrEmpty(output?.required_verified_artifact?.params),
        },
      })),
      contextArtifactBindings,
      selectedWorkerEnvironmentKinds: arrayValue(profile.selected_worker_environment_kinds),
      selectedTools,
    });
    await requireUnchangedJsonDigest(
      applicationPath,
      applicationDocument.digest,
      `application manifest ${applicationId}`,
    );
    for (const candidate of retainedApplicationDocuments) {
      await requireUnchangedJsonDigest(
        candidate.file,
        candidate.digest,
        `application manifest ${candidate.manifest.id}`,
      );
    }
    const writes = plannedTaskApplicationWrites(amendment?.writes);
    const removals = await plannedTaskApplicationRemovals(amendment?.removals);
    if (!writes.length && !removals.length) {
      return {
        action: "configure_task_application",
        runId,
        capabilityId: applicationId,
        connected,
        changed: false,
      };
    }
    await commitClientAuthoring({
      label: `configure task application ${applicationId}`,
      writes,
      removals,
      publishBarrierPath: runPath,
    });
    return {
      action: "configure_task_application",
      runId,
      capabilityId: applicationId,
      connected,
      changed: true,
    };
  });
}

async function readInstalledApplicationManifestDocuments(registryRoot) {
  const root = await fs.realpath(path.join(registryRoot, "interface"));
  const entries = await fs.readdir(root, { withFileTypes: true });
  const documents = await mapConcurrentOrdered(
    entries.filter((entry) => entry.isDirectory()),
    32,
    async (entry) => {
      const file = path.join(root, entry.name, "application.json");
      const document = await readOptionalRegularJsonDocument(
        file,
        `application manifest ${entry.name}`,
      );
      if (!document) return null;
      return {
        file,
        digest: document.digest,
        manifest: parseApplicationManifest(document.document, entry.name, file),
      };
    },
  );
  return documents.filter(Boolean);
}

function applicationTaskCheckpointId(applicationId, direction, index) {
  const normalized = requiredSafeRegistryId(applicationId, "application id").replace(/\./g, "_");
  const id = `cp_app_${normalized}_${direction}_${index + 1}`;
  return requiredSafeRegistryId(id, "application task checkpoint");
}

function plannedTaskApplicationWrites(value) {
  if (!Array.isArray(value)) throw new Error("task application planner writes must be an array");
  const paths = new Set();
  return value.map((write, index) => {
    if (!isPlainRecord(write)) throw new Error(`task application planner write ${index} must be an object`);
    const file = requiredAbsolutePath(write.filePath, `task application planner write ${index}`);
    if (paths.has(file)) throw new Error(`task application planner repeats write '${file}'`);
    paths.add(file);
    const beforeDigest = write.expectedBeforeHash === null
      ? null
      : requiredDigest(write.expectedBeforeHash, `task application planner write ${index} digest`);
    if (typeof write.content !== "string" || !write.content) {
      throw new Error(`task application planner write ${index} has no JSON content`);
    }
    let next;
    try {
      next = JSON.parse(write.content);
    } catch (error) {
      throw new Error(`task application planner write ${index} is malformed JSON: ${error.message}`);
    }
    if (!isPlainRecord(next)) throw new Error(`task application planner write ${index} must contain an object`);
    return { file, beforeDigest, content: write.content };
  });
}

async function plannedTaskApplicationRemovals(value) {
  if (!Array.isArray(value)) throw new Error("task application planner removals must be an array");
  const paths = new Set();
  const removals = [];
  for (const [index, removal] of value.entries()) {
    if (!isPlainRecord(removal)) throw new Error(`task application planner removal ${index} must be an object`);
    const directory = requiredAbsolutePath(removal.directory, `task application planner removal ${index}`);
    if (paths.has(directory)) throw new Error(`task application planner repeats removal '${directory}'`);
    paths.add(directory);
    const expectedFiles = new Map(arrayValue(removal.files).map((file, fileIndex) => {
      if (!isPlainRecord(file)) throw new Error(`task application removal file ${fileIndex} must be an object`);
      const name = requiredString(file.relativePath, `task application removal file ${fileIndex}`);
      if (path.basename(name) !== name || name.includes("\0")) {
        throw new Error(`task application removal file ${fileIndex} is not local to its directory`);
      }
      return [name, requiredDigest(file.sha256, `task application removal file ${fileIndex} digest`)];
    }));
    if (!expectedFiles.size) throw new Error(`task application planner removal ${index} has no files`);
    const actual = new Map();
    for (const entry of await fs.readdir(directory, { withFileTypes: true })) {
      if (entry.name === ".octopus-publication") { await requirePrivatePublicationDirectory(path.join(directory, entry.name)); continue; }
      if (!entry.isFile() || entry.isSymbolicLink()) {
        throw new Error(`task application removal '${directory}' contains a non-file entry`);
      }
      const bytes = await fs.readFile(path.join(directory, entry.name));
      actual.set(entry.name, crypto.createHash("sha256").update(bytes).digest("hex"));
    }
    if (
      actual.size !== expectedFiles.size
      || [...expectedFiles].some(([name, digest]) => actual.get(name) !== digest)
    ) {
      throw new Error(`task application removal '${directory}' changed after planning`);
    }
    removals.push(await snapshotRemovalDirectory(
      directory,
      requiredString(removal.tag, `task application planner removal ${index} tag`),
      true,
    ));
    removals[removals.length - 1].directFileDigests = expectedFiles;
  }
  return removals;
}

async function installOctopusOsApplicationPackage(
  registryRoot,
  octopusHomeRoot,
  sourceRegistryRootValue,
  request,
  onProgress,
) {
  const applicationId = requiredSafeRegistryId(request?.applicationId, "applicationId");
  const operationId = requiredApplicationOperationId(request?.operationId);
  const expectedSourceDigest = requiredDigest(request?.expectedSourceDigest, "expectedSourceDigest");
  reportApplicationProgress(onProgress, operationId, applicationId, "install", "preflight", 0);
  const activeInterfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const sourceRegistryRoot = requiredAbsolutePath(sourceRegistryRootValue, "application source registry");
  const archiveRegistryRoot = path.join(octopusHomeRoot, "client", "application-packages");
  const bundled = await optionalApplicationPackageSource(sourceRegistryRoot, applicationId, "bundled");
  const archived = await optionalApplicationPackageSource(archiveRegistryRoot, applicationId, "archived");
  if (bundled && archived) {
    throw new Error(`application '${applicationId}' has conflicting bundled and archived package sources`);
  }
  const source = bundled ?? archived;
  if (!source) throw new Error(`application '${applicationId}' has no verified package source`);
  if (source.manifest.interfaces.length !== 1) {
    throw new Error(`application '${applicationId}' spans multiple interface folders and has no atomic install contract`);
  }
  if (source.package_digest !== expectedSourceDigest) {
    throw new Error(`application '${applicationId}' source changed before installation`);
  }
  const targetDirectories = source.manifest.interfaces.map((interfaceId) => ({
    interfaceId,
    path: path.join(activeInterfaceRoot, interfaceId),
  }));
  const existing = [];
  for (const target of targetDirectories) {
    const stat = await fs.lstat(target.path).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
    if (stat) existing.push(target.interfaceId);
  }
  if (existing.length) {
    throw new Error(`application '${applicationId}' cannot install over existing interface folders: ${existing.join(", ")}`);
  }
  const transactionParent = path.join(octopusHomeRoot, ".client-application-transactions");
  const transactionRoot = path.join(transactionParent, operationId);
  await fs.mkdir(transactionParent, { recursive: true, mode: 0o700 });
  await fs.mkdir(transactionRoot, { mode: 0o700 });
  await createJsonFile(path.join(transactionRoot, "manifest.json"), {
    schema_version: 1,
    operation: "install",
    operation_id: operationId,
    application_id: applicationId,
    source_kind: source.source_kind,
    source_digest: source.package_digest,
    interfaces: source.manifest.interfaces,
  });
  const installed = [];
  let preserveTransaction = false;
  const stagedInterfaceRoot = path.join(transactionRoot, "staged", "interface");
  try {
    let commitSource = source;
    if (source.source_kind === "bundled") {
      await fs.mkdir(stagedInterfaceRoot, { recursive: true, mode: 0o700 });
      await stageApplicationPackage(
        source,
        stagedInterfaceRoot,
        (copied, total) => reportApplicationProgress(
          onProgress,
          operationId,
          applicationId,
          "install",
          "copying",
          total ? Math.round(10 + copied / total * 70) : 80,
        ),
      );
      commitSource = await readApplicationPackageSource(
        stagedInterfaceRoot,
        applicationId,
        "staged",
      );
      if (commitSource.package_digest !== source.package_digest) {
        throw new Error(`application '${applicationId}' staged package does not match its verified source`);
      }
      const refreshed = await readApplicationPackageSource(
        source.interface_root,
        applicationId,
        source.source_kind,
      );
      if (refreshed.package_digest !== source.package_digest) {
        throw new Error(`application '${applicationId}' source changed while installation was staged`);
      }
    }
    for (const [index, interfaceId] of commitSource.manifest.interfaces.entries()) {
      const sourceDirectory = path.join(commitSource.interface_root, interfaceId);
      const targetDirectory = path.join(activeInterfaceRoot, interfaceId);
      if (await fs.lstat(targetDirectory).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error))) {
        throw new Error(`application '${applicationId}' target '${interfaceId}' appeared during installation`);
      }
      await fs.rename(sourceDirectory, targetDirectory);
      const entry = { source: sourceDirectory, target: targetDirectory };
      installed.push(entry);
      reportApplicationProgress(
        onProgress,
        operationId,
        applicationId,
        "install",
        "installing",
        Math.round(82 + (index + 1) / commitSource.manifest.interfaces.length * 16),
      );
    }
    const installedPackage = await readApplicationPackageSource(
      activeInterfaceRoot,
      applicationId,
      "installed",
    );
    if (installedPackage.package_digest !== source.package_digest) {
      throw new Error(`application '${applicationId}' installed package does not match its verified source`);
    }
    await syncDirectory(activeInterfaceRoot);
    if (source.source_kind === "archived") await syncDirectory(source.interface_root);
  } catch (error) {
    const rollbackFailures = [];
    for (const entry of installed.reverse()) {
      if (source.source_kind === "archived") {
        await fs.rename(entry.target, entry.source).catch((failure) => rollbackFailures.push(failure));
      } else {
        await fs.rm(entry.target, { recursive: true, force: true }).catch((failure) => rollbackFailures.push(failure));
      }
    }
    await syncDirectory(activeInterfaceRoot).catch((failure) => rollbackFailures.push(failure));
    if (source.source_kind === "archived") {
      await syncDirectory(source.interface_root).catch((failure) => rollbackFailures.push(failure));
    }
    if (rollbackFailures.length) {
      preserveTransaction = true;
      throw new Error(`${error.message}; application install rollback remains in '${transactionRoot}': ${rollbackFailures[0].message}`);
    }
    throw error;
  } finally {
    if (!preserveTransaction) {
      await fs.rm(transactionRoot, { recursive: true, force: true }).catch(() => {});
      await fs.rmdir(transactionParent).catch(() => {});
    }
  }
  reportApplicationProgress(onProgress, operationId, applicationId, "install", "complete", 100);
  return {
    action: "install_application",
    capabilityId: applicationId,
    installed: true,
    changed: true,
    sourceKind: source.source_kind,
  };
}

async function removeOctopusOsApplicationPackage(
  registryRoot,
  octopusHomeRoot,
  sourceRegistryRootValue,
  _scope,
  request,
  onProgress,
) {
  const applicationId = requiredSafeRegistryId(request?.applicationId, "applicationId");
  const operationId = requiredApplicationOperationId(request?.operationId);
  const expectedInstalledDigest = requiredDigest(request?.expectedInstalledDigest, "expectedInstalledDigest");
  reportApplicationProgress(onProgress, operationId, applicationId, "remove", "preflight", 0);
  const activeInterfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const active = await readApplicationPackageSource(activeInterfaceRoot, applicationId, "active");
  if (active.manifest.interfaces.length !== 1) {
    throw new Error(`application '${applicationId}' spans multiple interface folders and has no atomic removal contract`);
  }
  if (active.package_digest !== expectedInstalledDigest) {
    throw new Error(`application '${applicationId}' changed before removal`);
  }
  const sourceRegistryRoot = requiredAbsolutePath(sourceRegistryRootValue, "application source registry");
  if (await optionalApplicationPackageSource(sourceRegistryRoot, applicationId, "bundled")) {
    throw new Error(`application '${applicationId}' is supplied by the Octopus installation and cannot be removed independently`);
  }
  const exactInterfaces = new Set(active.manifest.interfaces);
  const excludedDirectories = new Set(active.manifest.interfaces.map((interfaceId) => (
    path.join(activeInterfaceRoot, interfaceId)
  )));
  await assertApplicationPackageUnreferenced(
    await fs.realpath(registryRoot),
    octopusHomeRoot,
    excludedDirectories,
    exactInterfaces,
    applicationId,
  );
  const archiveRegistryRoot = path.join(octopusHomeRoot, "client", "application-packages");
  const archiveInterfaceRoot = path.join(archiveRegistryRoot, "interface");
  await fs.mkdir(archiveInterfaceRoot, { recursive: true, mode: 0o700 });
  for (const interfaceId of active.manifest.interfaces) {
    const archivedPath = path.join(archiveInterfaceRoot, interfaceId);
    if (await fs.lstat(archivedPath).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error))) {
      throw new Error(`application '${applicationId}' already has an archived '${interfaceId}' package folder`);
    }
  }
  const transactionParent = path.join(octopusHomeRoot, ".client-application-transactions");
  const transactionRoot = path.join(transactionParent, operationId);
  await fs.mkdir(transactionParent, { recursive: true, mode: 0o700 });
  await fs.mkdir(transactionRoot, { mode: 0o700 });
  await createJsonFile(path.join(transactionRoot, "manifest.json"), {
    schema_version: 1,
    operation: "remove",
    operation_id: operationId,
    application_id: applicationId,
    installed_digest: active.package_digest,
    interfaces: active.manifest.interfaces,
  });
  const moved = [];
  let preserveTransaction = false;
  try {
    for (const [index, interfaceId] of active.manifest.interfaces.entries()) {
      const source = path.join(activeInterfaceRoot, interfaceId);
      const target = path.join(archiveInterfaceRoot, interfaceId);
      const refreshed = await snapshotApplicationPackageDirectory(activeInterfaceRoot, interfaceId, "active");
      const expected = active.directories.find((directory) => directory.interface_id === interfaceId);
      if (!expected || applicationDirectoryDigest(refreshed) !== applicationDirectoryDigest(expected)) {
        throw new Error(`application '${applicationId}' interface '${interfaceId}' changed during removal preflight`);
      }
      await fs.rename(source, target);
      moved.push({ source, target });
      reportApplicationProgress(
        onProgress,
        operationId,
        applicationId,
        "remove",
        "removing",
        Math.round(20 + (index + 1) / active.manifest.interfaces.length * 78),
      );
    }
    const archivedPackage = await readApplicationPackageSource(
      archiveInterfaceRoot,
      applicationId,
      "archived",
    );
    if (archivedPackage.package_digest !== active.package_digest) {
      throw new Error(`application '${applicationId}' archived package does not match the installed package`);
    }
    await syncDirectory(activeInterfaceRoot);
    await syncDirectory(archiveInterfaceRoot);
  } catch (error) {
    const rollbackFailures = [];
    for (const move of moved.reverse()) {
      await fs.rename(move.target, move.source).catch((failure) => rollbackFailures.push(failure));
    }
    await syncDirectory(activeInterfaceRoot).catch((failure) => rollbackFailures.push(failure));
    await syncDirectory(archiveInterfaceRoot).catch((failure) => rollbackFailures.push(failure));
    if (rollbackFailures.length) {
      preserveTransaction = true;
      throw new Error(`${error.message}; application removal rollback remains in '${transactionRoot}': ${rollbackFailures[0].message}`);
    }
    throw error;
  } finally {
    if (!preserveTransaction) {
      await fs.rm(transactionRoot, { recursive: true, force: true }).catch(() => {});
      await fs.rmdir(transactionParent).catch(() => {});
    }
  }
  reportApplicationProgress(onProgress, operationId, applicationId, "remove", "complete", 100);
  return {
    action: "remove_application",
    capabilityId: applicationId,
    installed: false,
    changed: true,
    archivedPath: archiveRegistryRoot,
  };
}

async function optionalApplicationPackageSource(registryRoot, applicationId, sourceKind) {
  const interfaceRoot = path.join(registryRoot, "interface");
  const manifestPath = path.join(interfaceRoot, applicationId, "application.json");
  const manifest = await readOptionalRegularJsonDocument(
    manifestPath,
    `${sourceKind} application manifest ${applicationId}`,
  );
  if (!manifest) return null;
  return readApplicationPackageSource(interfaceRoot, applicationId, sourceKind, manifest);
}

async function stageApplicationPackage(source, stagedInterfaceRoot, onCopy) {
  const total = source.directories.reduce((count, directory) => count + directory.files.length, 0);
  let copied = 0;
  for (const directory of source.directories) {
    const targetRoot = path.join(stagedInterfaceRoot, directory.interface_id);
    await fs.mkdir(targetRoot, { recursive: true, mode: 0o755 });
    for (const relativeDirectory of directory.directories) {
      if (!relativeDirectory) continue;
      await fs.mkdir(path.join(targetRoot, relativeDirectory), { recursive: true, mode: 0o755 });
    }
    for (const file of directory.files) {
      const body = await fs.readFile(file.path);
      const digest = crypto.createHash("sha256").update(body).digest("hex");
      const stat = await fs.lstat(file.path);
      if (
        stat.isSymbolicLink()
        || !stat.isFile()
        || digest !== file.sha256
        || stat.size !== file.size
        || (stat.mode & 0o777) !== file.mode
      ) {
        throw new Error(`application package source changed while copying '${file.path}'`);
      }
      const target = path.join(targetRoot, file.relative_path);
      const handle = await fs.open(target, "wx", file.mode);
      try {
        await handle.writeFile(body);
        await handle.chmod(file.mode);
        await handle.sync();
      } finally {
        await handle.close();
      }
      copied += 1;
      onCopy(copied, total);
    }
    for (const relativeDirectory of [...directory.directories].sort((left, right) => right.length - left.length)) {
      await syncDirectory(path.join(targetRoot, relativeDirectory));
    }
  }
  await syncDirectory(stagedInterfaceRoot);
}

async function assertApplicationPackageUnreferenced(
  registryRoot,
  octopusHomeRoot,
  excludedDirectories,
  exactInterfaces,
  applicationId,
) {
  const registryFiles = await registryControlJsonFiles(registryRoot, excludedDirectories);
  for (const file of registryFiles) {
    const document = await readRegularJsonDocument(file, `registry control document ${file}`);
    const references = exactStringReferencePaths(document.document, exactInterfaces);
    if (references.length) {
      throw new Error(`application '${applicationId}' is still referenced by '${file}' at ${references.slice(0, 4).join(", ")}`);
    }
  }
  for (const relativeRoot of [path.join("state", "runs"), path.join("state", "ee_instances")]) {
    const root = path.join(octopusHomeRoot, relativeRoot);
    let entries;
    try {
      entries = await fs.readdir(root, { withFileTypes: true });
    } catch (error) {
      if (error?.code === "ENOENT") continue;
      throw error;
    }
    for (const entry of entries) {
      if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
      const file = path.join(root, entry.name);
      const document = await readRegularJsonDocument(file, `application removal reference ${file}`);
      const references = exactStringReferencePaths(document.document, exactInterfaces);
      if (references.length) {
        throw new Error(`application '${applicationId}' is still referenced by '${file}' at ${references.slice(0, 4).join(", ")}`);
      }
    }
  }
}

function applicationDirectoryDigest(directory) {
  return crypto.createHash("sha256").update(stableJson(directory.files.map((file) => ({
    path: file.relative_path.split(path.sep).join("/"),
    sha256: file.sha256,
    mode: file.mode,
  })))).digest("hex");
}

function reportApplicationProgress(onProgress, operationId, applicationId, operation, phase, percent) {
  if (typeof onProgress !== "function") return;
  onProgress({
    operationId,
    applicationId,
    operation,
    phase,
    percent: Math.max(0, Math.min(100, Math.round(percent))),
  });
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

function taskCheckpointCarriesProviderContext(checkpoint, providerInterface) {
  return interfaceBindingId(checkpoint?.interface_binding) !== providerInterface
    && arrayValue(checkpoint?.outbound_interface_bindings).length === 1
    && interfaceBindingId(checkpoint.outbound_interface_bindings[0]) === providerInterface;
}

function taskProviderContextArtifactIds(run, activeIds, providerInterface) {
  const ids = new Set();
  let projectedInputs = 0;
  for (const checkpoint of run.checkpoints) {
    if (!activeIds.has(stringValue(checkpoint?.id))) continue;
    if (!taskCheckpointCarriesProviderContext(checkpoint, providerInterface)) continue;
    projectedInputs += 1;
    for (const binding of arrayValue(checkpoint.artifact_bindings)) {
      ids.add(requiredArtifactBindingId(binding?.id, "task context artifact binding"));
    }
  }
  if (projectedInputs === 0) throw new Error(`task '${stringValue(run.id)}' has no projected provider input`);
  return ids;
}

function applicationTaskCapabilityReferences(profile, manifest) {
  const selected = new Set(arrayValue(profile?.selected_tools).flatMap((capability) => {
    const tool = stringValue(capability?.tool);
    const interfaceId = stringValue(capability?.interface);
    const kind = stringValue(capability?.worker_environment_kind);
    return tool && interfaceId && kind ? [`${tool}\0${interfaceId}\0${kind}`] : [];
  }));
  const required = manifest.task_capabilities.tools.map((capability) => (
    `${capability.tool}\0${capability.interface}\0${capability.worker_environment_kind}`
  ));
  return { complete: required.every((capability) => selected.has(capability)) };
}

function taskBaseAuthoringProfile(scope, runProjection, taskProfile) {
  const eeId = requiredSafeRegistryId(runProjection?.ee_id, "task base authoring EE");
  const worker = requiredSafeRegistryId(
    taskProfile?.worker_choice_interface,
    "task base authoring worker",
  );
  const candidates = [...new Map([
    ...scope.taskAuthoringProfiles.filter((profile) => (
      stringValue(profile?.ee_id) === eeId
      && stringValue(profile?.worker_choice_interface) === worker
    )),
    ...scope.clientRuns.flatMap((candidate) => {
        const profile = candidate?.authoring_profile;
        return stringValue(candidate?.surface_role) === "home"
          && stringValue(profile?.ee_id) === eeId
          && stringValue(profile?.worker_choice_interface) === worker
          ? [profile]
          : [];
      }),
  ].map((profile) => [stableJson(profile), profile])).values()];
  if (candidates.length !== 1) {
    throw new Error(
      `task '${stringValue(runProjection?.run_id)}' has ${candidates.length} exact base authoring profiles for '${eeId}' and '${worker}'`,
    );
  }
  return candidates[0];
}

function applicationIsConnectedToAuthoredRun(run, manifest, profile) {
  const active = new Set(arrayValue(run?.active_checkpoints).map(stringValue).filter(Boolean));
  const projectedInterfaces = new Set();
  const projectedArtifacts = new Set();
  for (const checkpoint of arrayValue(run?.checkpoints)) {
    if (!active.has(stringValue(checkpoint?.id))) continue;
    const inbound = interfaceBindingId(checkpoint?.interface_binding);
    if (inbound) projectedInterfaces.add(inbound);
    for (const binding of arrayValue(checkpoint?.artifact_bindings)) {
      const artifactId = stringValue(binding?.id);
      if (artifactId) projectedArtifacts.add(artifactId);
    }
  }
  const capabilities = applicationTaskCapabilityReferences(profile, manifest);
  return manifest.interfaces.every((interfaceId) => projectedInterfaces.has(interfaceId))
    && manifest.artifact_bindings.every((binding) => projectedArtifacts.has(binding.artifact_id))
    && capabilities.complete;
}

function applicationHasAnyAuthoredBinding(run, manifest) {
  const active = new Set(arrayValue(run?.active_checkpoints).map(stringValue).filter(Boolean));
  const interfaceIds = new Set(manifest.interfaces);
  const artifactIds = new Set(manifest.artifact_bindings.map((binding) => binding.artifact_id));
  return arrayValue(run?.checkpoints).some((checkpoint) => {
    if (!active.has(stringValue(checkpoint?.id))) return false;
    if (interfaceIds.has(interfaceBindingId(checkpoint?.interface_binding))) return true;
    return arrayValue(checkpoint?.artifact_bindings).some((binding) => artifactIds.has(stringValue(binding?.id)));
  });
}

function requiredArtifactBindingId(value, label) {
  if (typeof value !== "string" || !value || value.trim() !== value || value.length > 512 || value.includes("\0")) {
    throw new Error(`${label} must be a non-empty canonical artifact id`);
  }
  return value;
}

function projectCapabilityChanges(request, includeHostSlots) {
  if (!Array.isArray(request?.projects) || request.projects.length === 0) {
    throw new Error("project capability setup requires at least one changed project");
  }
  const seen = new Set();
  return request.projects.map((row, index) => {
    if (!isPlainRecord(row)) throw new Error(`projects[${index}] must be an object`);
    const eeId = requiredSafeRegistryId(row.eeId, `projects[${index}].eeId`);
    if (seen.has(eeId)) throw new Error(`project capability setup repeats EE '${eeId}'`);
    seen.add(eeId);
    const available = requiredBoolean(row.available, `projects[${index}].available`);
    const selectedByDefault = requiredBoolean(
      row.selectedByDefault,
      `projects[${index}].selectedByDefault`,
    );
    if (selectedByDefault && !available) {
      throw new Error("an unavailable capability cannot be selected by default");
    }
    return {
      eeId,
      available,
      selectedByDefault,
      expectedDigest: requiredDigest(row.expectedDigest, `projects[${index}].expectedDigest`),
      hostSlots: includeHostSlots ? row.hostSlots : undefined,
    };
  }).sort((left, right) => left.eeId.localeCompare(right.eeId));
}

async function configureOctopusOsApplication(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  commitClientAuthoring,
) {
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const applicationId = requiredSafeRegistryId(request?.applicationId, "applicationId");
  const projects = projectCapabilityChanges(request, false);
  const applicationPath = path.join(
    await fs.realpath(path.join(registryRoot, "interface")),
    applicationId,
    "application.json",
  );
  const applicationDocument = await readRegularJsonDocument(
    applicationPath,
    `application manifest ${applicationId}`,
  );
  const manifest = parseApplicationManifest(
    applicationDocument.document,
    applicationId,
    applicationPath,
  );
  const allInterfaces = await installedDescriptorIds(
    path.join(registryRoot, "interface"),
    "interface.json",
    ["id", "rule_id"],
  );
  const allTools = await installedDescriptorIds(
    path.join(registryRoot, "tool"),
    "tool.json",
    ["id"],
  );
  const allKinds = await installedDescriptorIds(
    path.join(registryRoot, "worker_environment_kind"),
    "worker_environment_kind.json",
    ["worker_environment_kind_id", "worker_environment_kind", "kind", "id"],
  );
  const applicationDocuments = await readInstalledApplicationManifestDocuments(registryRoot);
  const requiredInterfaces = new Set(manifest.interfaces);
  const requiredTools = new Set();
  const requiredKinds = new Set();
  for (const capability of manifest.task_capabilities.tools) {
    requiredInterfaces.add(capability.interface);
    requiredTools.add(capability.tool);
    requiredKinds.add(capability.worker_environment_kind);
    await assertApplicationToolCapability(registryRoot, applicationId, capability);
  }
  for (const interfaceId of requiredInterfaces) {
    if (!allInterfaces.includes(interfaceId)) {
      throw new Error(`application '${applicationId}' references missing interface '${interfaceId}'`);
    }
  }
  const prepared = [];
  for (const project of projects) {
    const environment = scope.byId.get(project.eeId);
    if (!environment) throw new Error(`project EE '${project.eeId}' is outside the Octopus OS scope`);
    const retainedApplications = applicationDocuments.filter((candidate) => (
      candidate.manifest.id !== applicationId
      && projectConfiguredApplication(environment, candidate.manifest)
    ));
    const retainedInterfaces = new Set(retainedApplications.flatMap((candidate) => [
      ...candidate.manifest.interfaces,
      ...candidate.manifest.task_capabilities.tools.map((capability) => capability.interface),
    ]));
    const retainedTools = new Set(retainedApplications.flatMap((candidate) => (
      candidate.manifest.task_capabilities.tools.map((capability) => capability.tool)
    )));
    const retainedKinds = new Set(retainedApplications.flatMap((candidate) => (
      candidate.manifest.task_capabilities.tools.map((capability) => capability.worker_environment_kind)
    )));
    prepared.push(await prepareProjectCapabilityState({
      octopusHomeRoot,
      environment,
      eeId: project.eeId,
      expectedDigest: project.expectedDigest,
      capabilityId: applicationId,
      available: project.available,
      selectedByDefault: project.selectedByDefault,
      axisUpdates: [
        {
          field: "allowed_interfaces",
          values: [...requiredInterfaces],
          installedValues: allInterfaces,
          removeValues: [...requiredInterfaces].filter((value) => !retainedInterfaces.has(value)),
        },
        {
          field: "allowed_tools",
          values: [...requiredTools],
          installedValues: allTools,
          removeValues: [...requiredTools].filter((value) => !retainedTools.has(value)),
        },
        {
          field: "allowed_worker_environment_kinds",
          values: [...requiredKinds],
          installedValues: allKinds,
          removeValues: [...requiredKinds].filter((value) => !retainedKinds.has(value)),
        },
      ],
      defaultField: "application_defaults",
    }));
  }
  const writes = prepared.map((entry) => entry.write);
  await commitClientAuthoring({
    label: `configure_application ${applicationId}`,
    writes,
    removals: [],
    publishBarrierPath: writes.at(-1)?.file,
  });
  return {
    action: "configure_application",
    capabilityId: applicationId,
    projectChanges: prepared.map((entry) => entry.outcome),
  };
}

async function assertApplicationToolCapability(registryRoot, applicationId, capability) {
  const toolPath = path.join(
    await fs.realpath(path.join(registryRoot, "tool")),
    capability.tool,
    "tool.json",
  );
  const tool = (await readRegularJsonDocument(
    toolPath,
    `application '${applicationId}' tool ${capability.tool}`,
  )).document;
  if (
    stringValue(tool.id) !== capability.tool
    || stringValue(tool.interface) !== capability.interface
    || !arrayValue(tool.supported_worker_environment_kinds)
      .map(stringValue)
      .includes(capability.worker_environment_kind)
  ) {
    throw new Error(
      `application '${applicationId}' task capability '${capability.tool}' does not match its exact tool/door/worker-environment descriptor`,
    );
  }
}

function projectConfiguredApplication(environment, manifest) {
  const effective = objectOrEmpty(environment.formed_envelope);
  const allowed = (field, value) => (
    !Array.isArray(effective[field]) || effective[field].includes(value)
  );
  return manifest.interfaces.every((interfaceId) => allowed("allowed_interfaces", interfaceId))
    && manifest.task_capabilities.tools.every((capability) => (
      allowed("allowed_interfaces", capability.interface)
      && allowed("allowed_tools", capability.tool)
      && allowed("allowed_worker_environment_kinds", capability.worker_environment_kind)
    ));
}

async function configureOctopusOsDriver(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  commitClientAuthoring,
) {
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const driverName = requiredSafeRegistryId(request?.driverName, "driverName");
  const projects = projectCapabilityChanges(request, true);
  const kindRoot = await fs.realpath(path.join(registryRoot, "worker_environment_kind"));
  const descriptorPath = path.join(kindRoot, driverName, "worker_environment_kind.json");
  const descriptor = (await readRegularJsonDocument(
    descriptorPath,
    `worker environment kind ${driverName}`,
  )).document;
  const declaredName = stringValue(
    descriptor.worker_environment_kind_id
    ?? descriptor.worker_environment_kind
    ?? descriptor.kind
    ?? descriptor.id,
  );
  if (declaredName !== driverName || stringValue(descriptor.worker_consent) !== "explicit_ceiling") {
    throw new Error(`driver '${driverName}' does not satisfy the current worker-environment-kind contract`);
  }
  const doorDriverRefs = arrayValue(descriptor.door_driver_refs)
    .map((value) => requiredSafeRegistryId(value, `driver '${driverName}' door driver ref`));
  if (doorDriverRefs.length === 0 || new Set(doorDriverRefs).size !== doorDriverRefs.length) {
    throw new Error(`driver '${driverName}' has no unique door driver refs`);
  }
  const allKinds = await installedDescriptorIds(
    kindRoot,
    "worker_environment_kind.json",
    ["worker_environment_kind_id", "worker_environment_kind", "kind", "id"],
  );
  const toolRoot = await fs.realpath(path.join(registryRoot, "tool"));
  const allTools = await installedDescriptorIds(toolRoot, "tool.json", ["id"]);
  const toolIds = [];
  const doorInterfaces = new Set();
  for (const toolId of allTools) {
    const tool = (await readRegularJsonDocument(
      path.join(toolRoot, toolId, "tool.json"),
      `tool descriptor ${toolId}`,
    )).document;
    if (arrayValue(tool.supported_worker_environment_kinds).map(stringValue).includes(driverName)) {
      toolIds.push(toolId);
      doorInterfaces.add(requiredSafeRegistryId(tool.interface, `tool '${toolId}' owner interface`));
    }
  }
  if (doorInterfaces.size === 0) {
    throw new Error(`driver '${driverName}' has no tool-owned door interface`);
  }
  const allInterfaces = await installedDescriptorIds(
    path.join(registryRoot, "interface"),
    "interface.json",
    ["id", "rule_id"],
  );
  const prepared = [];
  for (const project of projects) {
    const environment = scope.byId.get(project.eeId);
    if (!environment) throw new Error(`project EE '${project.eeId}' is outside the Octopus OS scope`);
    prepared.push(await prepareProjectCapabilityState({
      octopusHomeRoot,
      environment,
      eeId: project.eeId,
      expectedDigest: project.expectedDigest,
      capabilityId: driverName,
      available: project.available,
      selectedByDefault: project.selectedByDefault,
      axisUpdates: [
        {
          field: "allowed_worker_environment_kinds",
          values: [driverName],
          installedValues: allKinds,
          removeValues: [driverName],
        },
        {
          field: "allowed_interfaces",
          values: [...doorInterfaces],
          installedValues: allInterfaces,
        },
        {
          field: "allowed_tools",
          values: toolIds,
          installedValues: allTools,
        },
      ],
      defaultField: "driver_defaults",
      driverHostSlots: parseDriverHostSlots(project.hostSlots, descriptor, driverName),
    }));
  }
  const writes = prepared.map((entry) => entry.write);
  await commitClientAuthoring({
    label: `configure_driver ${driverName}`,
    writes,
    removals: [],
    publishBarrierPath: writes.at(-1)?.file,
  });
  return {
    action: "configure_driver",
    capabilityId: driverName,
    projectChanges: prepared.map((entry) => entry.outcome),
  };
}

async function prepareProjectCapabilityState({
  octopusHomeRoot,
  environment,
  eeId,
  expectedDigest,
  capabilityId,
  available,
  selectedByDefault,
  axisUpdates,
  defaultField,
  driverHostSlots,
}) {
  const digest = requiredDigest(expectedDigest, "expectedDigest");
  const eePath = await scopedRecordPath(
    octopusHomeRoot,
    environment.record_path,
    path.join("state", "ee_instances"),
    `${eeId}.json`,
  );
  const current = await readRegularJsonDocument(eePath, `project EE ${eeId}`);
  if (current.digest !== digest || stringValue(environment.record_digest) !== digest) {
    throw new Error(`project EE '${eeId}' changed before its capability setup could be applied`);
  }
  if (stringValue(current.document.ee_id) !== eeId) {
    throw new Error(`project EE '${eeId}' has drifted identity`);
  }
  const next = JSON.parse(JSON.stringify(current.document));
  const narrowing = isPlainRecord(next.policy_narrowing) ? next.policy_narrowing : {};
  const ceiling = objectOrEmpty(environment.boundary_envelope);
  for (const update of axisUpdates) {
    if (available) assertCapabilityValuesWithinCeiling(ceiling, update.field, update.values, capabilityId);
    const authored = Object.prototype.hasOwnProperty.call(narrowing, update.field)
      ? arrayValue(narrowing[update.field]).map(stringValue).filter(Boolean)
      : null;
    if (authored === null && available) continue;
    if (!available && !update.removeValues?.length) continue;
    const base = authored ?? update.installedValues.filter((value) => (
      !Array.isArray(ceiling[update.field]) || ceiling[update.field].includes(value)
    ));
    const values = new Set(base);
    if (available) {
      for (const value of update.values) values.add(value);
    } else {
      for (const value of update.removeValues) values.delete(value);
    }
    narrowing[update.field] = [...values].sort();
  }
  next.policy_narrowing = narrowing;
  const config = isPlainRecord(next.config) ? next.config : {};
  if (config.octopus_os !== undefined && !isPlainRecord(config.octopus_os)) {
    throw new Error(`project EE '${eeId}' has malformed Octopus OS client config`);
  }
  const octopusOs = isPlainRecord(config.octopus_os) ? config.octopus_os : {};
  const defaults = new Set(arrayValue(octopusOs[defaultField]).map(stringValue).filter(Boolean));
  if (selectedByDefault) defaults.add(capabilityId);
  else defaults.delete(capabilityId);
  octopusOs[defaultField] = [...defaults].sort();
  if (driverHostSlots !== undefined) {
    if (octopusOs.driver_host_slots !== undefined && !isPlainRecord(octopusOs.driver_host_slots)) {
      throw new Error(`project EE '${eeId}' has malformed driver host-slot config`);
    }
    const driverSlots = isPlainRecord(octopusOs.driver_host_slots)
      ? octopusOs.driver_host_slots
      : {};
    if (available) driverSlots[capabilityId] = driverHostSlots;
    else delete driverSlots[capabilityId];
    octopusOs.driver_host_slots = driverSlots;
  }
  config.octopus_os = octopusOs;
  next.config = config;
  return {
    write: {
      file: eePath,
      beforeDigest: digest,
      next,
    },
    outcome: {
      eeId,
      available,
      selectedByDefault,
    },
  };
}

function assertCapabilityValuesWithinCeiling(ceiling, field, values, capabilityId) {
  const allowed = ceiling[field];
  if (!Array.isArray(allowed)) return;
  const missing = values.filter((value) => !allowed.includes(value));
  if (missing.length) {
    throw new Error(`capability '${capabilityId}' exceeds the project ${field} ceiling: ${missing.join(", ")}`);
  }
}

function parseDriverHostSlots(value, descriptor, driverName) {
  if (value === undefined) return {};
  if (!isPlainRecord(value)) throw new Error(`driver '${driverName}' hostSlots must be an object`);
  const boundary = objectOrEmpty(descriptor.boundary_defaults);
  const declared = new Set([
    ...arrayValue(boundary.read_root_refs),
    ...arrayValue(boundary.write_root_refs),
    ...arrayValue(boundary.protected_read_only_subpaths),
  ].map((entry) => stringValue(entry?.path)).filter(Boolean));
  const result = {};
  for (const [slot, slotPath] of Object.entries(value)) {
    if (!declared.has(slot) || typeof slotPath !== "string" || !path.isAbsolute(slotPath)) {
      throw new Error(`driver '${driverName}' host slot '${slot}' is not a declared absolute path`);
    }
    result[slot] = path.normalize(slotPath);
  }
  return result;
}

async function installedDescriptorIds(root, fileName, identityFields) {
  const entries = await readDirectoryEntries(root);
  const ids = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || !safeInterfacePathSegment(entry.name)) continue;
    const descriptor = await readOptionalRegularJsonDocument(
      path.join(root, entry.name, fileName),
      `${fileName} descriptor ${entry.name}`,
    );
    if (!descriptor) continue;
    const identity = identityFields.map((field) => stringValue(descriptor.document[field])).find(Boolean);
    if (identity !== entry.name) throw new Error(`${fileName} descriptor '${entry.name}' has drifted identity`);
    ids.push(entry.name);
  }
  return ids.sort();
}

function taskLifecycleHelpers(hooks, record) {
  const required = Object.values(objectOrEmpty(record.interface_state)).some((state) =>
    state?.maintenance_observer !== undefined || state?.collection_catalogue_adoption !== undefined);
  const helpers = hooks.taskLifecycle;
  if (!helpers && !required) return null;
  if (!helpers || ["readMaintenanceTaskTopology", "readTaskCatalogueEvidence", "catalogueProvenanceProjection",
    "assertMaintenanceNoPendingEffects"].some((name) => typeof helpers[name] !== "function")) {
    const error = new Error("task lifecycle ownership reader is unavailable for the accepted maintenance/catalogue contract");
    error.code = "maintenance_lifecycle_unavailable";
    throw error;
  }
  return helpers;
}

async function octopusOsTaskLifecycleGroup(registryRoot, octopusHomeRoot, source, sourceInterfaces, lifecycle, sourceClientInterface) {
  const sourceWriters = await octopusOsOwnedWriterRuns(octopusHomeRoot, source, sourceInterfaces);
  const companions = lifecycle ? await lifecycle.readMaintenanceTaskTopology({
    registryRoot, runRecordsRoot: path.join(octopusHomeRoot, "state", "runs"), sourceRecord: source, sourceClientInterface,
    readWriterRuns: (record, interfaces) => octopusOsOwnedWriterRuns(octopusHomeRoot, record, interfaces),
  }) : null;
  const records = new Map([...sourceWriters.records].map(([id, entry]) => [id, { ...entry, role: "writer", targetRun: source.id }]));
  for (const [id, entry] of companions?.records ?? []) {
    if (id === source.id || records.has(id)) throw new Error(`task '${source.id}' has conflicting companion/writer ownership`);
    records.set(id, entry);
  }
  const exactInterfaces = new Set([...sourceInterfaces, ...companions?.interfaces ?? []]);
  if (companions && [...companions.interfaces].some((id) => sourceInterfaces.has(id))) {
    throw new Error(`task '${source.id}' has overlapping source and companion descriptor owners`);
  }
  const ownedWriters = { records, ids: new Set(records.keys()), writes: [...records.values()]
    .map((entry) => ({ file: entry.file, beforeDigest: entry.digest, next: null })).sort((a, b) => a.file.localeCompare(b.file)) };
  return { sourceInterfaces, exactInterfaces, ownedWriters, companions };
}

function taskLifecycleRetirementRequest(sourcePath, source, group) {
  const rows = [{ ...source, file: sourcePath, role: "source", provider: group.companions.declaration.source_provider_interface },
    ...group.ownedWriters.records.values()];
  return {
    schema_version: 1, source_run: source.document.id,
    interface_ids: [...group.exactInterfaces].sort(),
    records: rows.map((entry) => ({ run: entry.document.id, file: entry.file, expected_digest: entry.digest,
      worker: entry.document.worker, ee: entry.document.ee, role: entry.role,
      provider_interface: typeof entry.provider === "string" ? entry.provider : entry.owner,
      target_run: entry.targetRun ?? entry.document.id })).sort((a, b) => a.run.localeCompare(b.run)),
  };
}

async function octopusOsTaskGroupArtifactTopology(registryRoot, source, group) {
  const targets = [{ document: source, interfaces: group.sourceInterfaces }, ...group.companions?.targets.values() ?? []];
  const ids = new Set(), writes = [];
  for (const target of targets) {
    const owned = await octopusOsOwnedArtifactTopology(registryRoot, target.document, target.interfaces);
    for (const id of owned.ids) {
      if (ids.has(id)) throw new Error(`task '${source.id}' has an artifact owned by two companion targets`);
      ids.add(id);
    }
    writes.push(...owned.writes);
  }
  return { ids, writes };
}

async function deleteOctopusOsTask(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  releaseTaskWorkerEnvironments,
  commitClientAuthoring,
  hooks = {},
) {
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const runId = requiredSafeRegistryId(request?.runId, "runId");
  const runProjection = scope.clientRuns.find((row) => (
    stringValue(row?.run_id) === runId && stringValue(row?.surface_role) === "task"
  ));
  if (!runProjection) {
    const readOnlyProjection = scope.readOnlyRuns.find((row) => stringValue(row?.run_id) === runId);
    if (!readOnlyProjection) throw new Error(`task '${runId}' is absent from the Octopus OS scope`);
    return deleteOctopusOsReadOnlyTask(
      registryRoot,
      octopusHomeRoot,
      readOnlyProjection,
      request,
      commitClientAuthoring,
    );
  }
  let expectedDigest = requiredDigest(request?.expectedDigest, "expectedDigest");
  const runPath = await scopedRecordPath(
    octopusHomeRoot,
    runProjection.record_path,
    path.join("state", "runs"),
    `${runId}.json`,
  );
  const current = await readRegularJsonDocument(runPath, `task run ${runId}`);
  if (current.digest !== expectedDigest || stringValue(runProjection.record_digest) !== expectedDigest) {
    throw new Error(`task run '${runId}' changed before it could be deleted`);
  }
  if (
    stringValue(current.document?.id) !== runId
    || stringValue(current.document?.ee) !== stringValue(runProjection.ee_id)
  ) {
    throw new Error(`task run '${runId}' has drifted topology`);
  }
  const ownerInterface = requiredSafeRegistryId(runProjection.interface_id, "task client interface");
  if (readClientTaskArchive(current.document, ownerInterface)) throw new Error(`task '${runId}' is archived; restore it before deleting`);
  const sourceInterfaces = await octopusOsOwnedInterfaceIds(registryRoot, ownerInterface);
  assertRunPrivateStateIsOwned(current.document, sourceInterfaces, runId);
  const lifecycle = taskLifecycleHelpers(hooks, current.document);
  const group = await octopusOsTaskLifecycleGroup(
    registryRoot, octopusHomeRoot, current.document, sourceInterfaces, lifecycle, ownerInterface,
  );
  const { exactInterfaces, ownedWriters, companions } = group;
  let retirementSelection = companions ? taskLifecycleRetirementRequest(runPath, current, group) : null;
  if (companions) {
    lifecycle.assertMaintenanceNoPendingEffects({ targets: new Map([
      [runId, { document: current.document, provider: companions.declaration.source_provider_interface }],
      ...companions.targets,
    ]) });
  }
  await assertTopologyIsPrivateToRun(octopusHomeRoot, runId, exactInterfaces, new Set(), ownedWriters.ids);

  const registryHomeRoot = await fs.realpath(octopusHomeRoot);
  const registryHome = await fs.realpath(registryRoot);
  if (path.dirname(registryHome) !== registryHomeRoot || path.basename(registryHome) !== "registry") {
    throw new Error("Octopus OS registry root is outside the active Octopus home");
  }
  const interfaceRoot = await fs.realpath(path.join(registryHome, "interface"));
  const interfaceDirectories = await snapshotOwnedRegistryDirectories(
    interfaceRoot,
    exactInterfaces,
    "task interface",
  );
  const ownedTools = await octopusOsOwnedToolTopology(registryHome, exactInterfaces, companions);
  const ownedArtifacts = await octopusOsTaskGroupArtifactTopology(registryHome, current.document, group);
  const catalogueEvidence = lifecycle ? await lifecycle.readTaskCatalogueEvidence({
    registryRoot: registryHome,
    records: [current.document, ...[...companions?.targets.values() ?? []].map((entry) => entry.document)],
    canonicalContentHash: (value) => Buffer.from(blake3(Buffer.from(stableJson(value)))).toString("hex"),
  }) : new Map();
  await assertTopologyIsPrivateToRun(octopusHomeRoot, runId, exactInterfaces, ownedArtifacts.ids, ownedWriters.ids);
  const policyWrites = await taskTopologyPolicyWrites(registryHome, exactInterfaces, ownedTools.ids);
  const plannedDocuments = new Map([...policyWrites, ...ownedArtifacts.writes].map((write) => [write.file, write.next]));
  await assertNoExternalTaskTopologyReferences(
    registryHome,
    new Set([
      ...interfaceDirectories.map((entry) => entry.path),
      ...ownedTools.directories.map((entry) => entry.path),
    ]),
    plannedDocuments,
    new Set([runId, ...ownedWriters.ids, ...exactInterfaces, ...ownedTools.ids, ...ownedArtifacts.ids]),
    lifecycle,
  );

  let releasedWorkerEnvironmentIds = [];
  if (typeof releaseTaskWorkerEnvironments === "function") {
    const released = await releaseTaskWorkerEnvironments({
      runId,
      expectedDigest,
      timeoutMs: request?.timeoutMs,
    });
    if (stringValue(released?.runId) !== runId) {
      throw new Error(`task '${runId}' release hook returned another run identity`);
    }
    expectedDigest = requiredDigest(released?.expectedDigest, "released task digest");
    releasedWorkerEnvironmentIds = arrayValue(released?.releasedWorkerEnvironmentIds)
      .map((id) => requiredSafeRegistryId(id, "released worker environment id"));
    if (new Set(releasedWorkerEnvironmentIds).size !== releasedWorkerEnvironmentIds.length) {
      throw new Error(`task '${runId}' release hook repeated a worker environment`);
    }
    const releasedRun = await readRegularJsonDocument(runPath, `released task run ${runId}`);
    if (
      releasedRun.digest !== expectedDigest
      || stringValue(releasedRun.document?.id) !== runId
      || stringValue(releasedRun.document?.ee) !== stringValue(runProjection.ee_id)
    ) {
      throw new Error(`task run '${runId}' changed outside its exact release cycle`);
    }
    assertRunPrivateStateIsOwned(releasedRun.document, sourceInterfaces, runId);
    await assertTopologyIsPrivateToRun(octopusHomeRoot, runId, exactInterfaces, ownedArtifacts.ids, ownedWriters.ids);
    const releasedGroup = await octopusOsTaskLifecycleGroup(
      registryRoot, octopusHomeRoot, releasedRun.document, sourceInterfaces, lifecycle, ownerInterface,
    );
    if (stableJson([...releasedGroup.exactInterfaces].sort()) !== stableJson([...exactInterfaces].sort())
      || stableJson(releasedGroup.ownedWriters.writes) !== stableJson(ownedWriters.writes)) {
      throw new Error(`task '${runId}' writer ownership changed during release`);
    }
    const releasedArtifacts = await octopusOsTaskGroupArtifactTopology(registryHome, releasedRun.document, releasedGroup);
    if (stableJson(releasedArtifacts.writes) !== stableJson(ownedArtifacts.writes)) {
      throw new Error(`task '${runId}' artifact ownership changed during release`);
    }
    await assertNoExternalTaskTopologyReferences(
      registryHome,
      new Set([
        ...interfaceDirectories.map((entry) => entry.path),
        ...ownedTools.directories.map((entry) => entry.path),
      ]),
      plannedDocuments,
      new Set([runId, ...ownedWriters.ids, ...exactInterfaces, ...ownedTools.ids, ...ownedArtifacts.ids]),
      lifecycle,
    );
    if (companions) retirementSelection = taskLifecycleRetirementRequest(runPath, releasedRun, releasedGroup);
  }

  const runtimeStateDirectories = await snapshotOwnedInterfaceStateDirectories(
    registryHomeRoot,
    exactInterfaces,
  );
  await assertTaskWorkerEnvironmentsReleased(registryHomeRoot, exactInterfaces, runId, ownedWriters.records, current.document);
  for (const [file, snapshot] of catalogueEvidence) {
    await requireUnchangedJsonDigest(file, snapshot.digest, "accepted catalogue evidence");
  }
  for (const removal of [...interfaceDirectories, ...ownedTools.directories]) await requireUnchangedRemoval(removal);
  for (const write of ownedWriters.writes) await snapshotRemovalFile(write.file, write.beforeDigest, "task writer Run");
  await snapshotRemovalFile(runPath, expectedDigest, `task run ${runId}`);
  const committed = await commitClientAuthoring({
    label: "delete Octopus OS task",
    writes: [...policyWrites, ...ownedArtifacts.writes, ...ownedWriters.writes, { file: runPath, beforeDigest: expectedDigest, next: null }],
    removals: [
      ...interfaceDirectories,
      ...ownedTools.directories,
      ...runtimeStateDirectories,
    ],
    publishBarrierPath: runPath,
    ...(retirementSelection ? { retirement: retirementSelection } : {}),
  });
  return {
    action: "delete_task",
    runId,
    removedRuns: [runId, ...ownedWriters.ids].sort(),
    removedInterfaces: [...exactInterfaces].sort(),
    removedTools: [...ownedTools.ids].sort(),
    removedArtifacts: [...ownedArtifacts.ids].sort(),
    releasedWorkerEnvironmentIds,
    cleanupWarnings: arrayValue(committed?.cleanupWarnings),
  };
}

async function deleteOctopusOsReadOnlyTask(
  registryRoot,
  octopusHomeRoot,
  runProjection,
  request,
  commitClientAuthoring,
) {
  const runId = requiredSafeRegistryId(request?.runId, "runId");
  const expectedDigest = requiredDigest(request?.expectedDigest, "expectedDigest");
  const runPath = await scopedRecordPath(
    octopusHomeRoot,
    runProjection.record_path,
    path.join("state", "runs"),
    `${runId}.json`,
  );
  const current = await readRegularJsonDocument(runPath, `read-only task run ${runId}`);
  if (current.digest !== expectedDigest || stringValue(runProjection.record_digest) !== expectedDigest) {
    throw new Error(`read-only task run '${runId}' changed before it could be deleted`);
  }
  if (
    stringValue(current.document?.id) !== runId
    || stringValue(current.document?.ee) !== stringValue(runProjection.ee_id)
  ) {
    throw new Error(`read-only task run '${runId}' has drifted topology`);
  }
  await assertTopologyIsPrivateToRun(octopusHomeRoot, runId, new Set());
  const registryHomeRoot = await fs.realpath(octopusHomeRoot);
  const registryHome = await fs.realpath(registryRoot);
  if (path.dirname(registryHome) !== registryHomeRoot || path.basename(registryHome) !== "registry") {
    throw new Error("Octopus OS registry root is outside the active Octopus home");
  }
  await assertNoExternalTaskTopologyReferences(
    registryHome,
    new Set(),
    new Map(),
    new Set([runId]),
  );
  const workerEnvironmentState = await snapshotReleasedRunWorkerEnvironmentStateDirectories(
    registryHomeRoot,
    current.document,
    runId,
  );
  await snapshotRemovalFile(runPath, expectedDigest, `read-only task run ${runId}`);
  const committed = await commitClientAuthoring({
    label: "delete read-only Octopus OS task",
    writes: [{ file: runPath, beforeDigest: expectedDigest, next: null }],
    removals: workerEnvironmentState.directories,
    publishBarrierPath: runPath,
  });
  return {
    action: "delete_task",
    runId,
    removedInterfaces: [],
    removedTools: [],
    releasedWorkerEnvironmentIds: workerEnvironmentState.ids,
    cleanupWarnings: arrayValue(committed?.cleanupWarnings),
  };
}

async function moveOctopusOsTask(
  registryRoot,
  octopusHomeRoot,
  scope,
  request,
  commitClientAuthoring,
) {
  if (typeof commitClientAuthoring !== "function") {
    throw new Error("client authoring transaction is unavailable");
  }
  const runId = requiredSafeRegistryId(request?.runId, "runId");
  const targetEeId = requiredSafeRegistryId(request?.targetEeId || scope.rootEeId, "targetEeId");
  if (!scope.byId.has(targetEeId)) throw new Error(`target EE '${targetEeId}' is outside the Octopus OS scope`);
  const runProjection = scope.clientRuns.find((row) => (
    stringValue(row?.run_id) === runId && stringValue(row?.surface_role) === "task"
  ));
  if (!runProjection) throw new Error(`writable task '${runId}' is absent from the Octopus OS scope`);
  const currentEeId = requiredSafeRegistryId(runProjection.ee_id, "current task EE");
  if (currentEeId === targetEeId) return { action: "move_task", runId, targetEeId, changed: false };
  const expectedDigest = requiredDigest(request?.expectedDigest, "expectedDigest");
  const runPath = await scopedRecordPath(
    octopusHomeRoot,
    runProjection.record_path,
    path.join("state", "runs"),
    `${runId}.json`,
  );
  const current = await readRegularJsonDocument(runPath, `task run ${runId}`);
  if (current.digest !== expectedDigest || stringValue(runProjection.record_digest) !== expectedDigest) {
    throw new Error(`task run '${runId}' changed before it could be moved`);
  }
  if (stringValue(current.document?.id) !== runId || stringValue(current.document?.ee) !== currentEeId) {
    throw new Error(`task run '${runId}' has drifted topology`);
  }
  const ownerInterface = requiredSafeRegistryId(runProjection.interface_id, "task client interface");
  if (readClientTaskArchive(current.document, ownerInterface)) throw new Error(`task '${runId}' is archived; restore it before moving`);
  if (current.document.interface_state !== undefined && (!isPlainRecord(current.document.interface_state)
    || Object.values(current.document.interface_state).some((state) => !isPlainRecord(state)))) {
    throw new Error(`task '${runId}' has malformed interface state`);
  }
  const exactInterfaces = await octopusOsOwnedInterfaceIds(registryRoot, ownerInterface);
  assertRunPrivateStateIsOwned(current.document, exactInterfaces, runId);
  const ownedWriters = await octopusOsOwnedWriterRuns(octopusHomeRoot, current.document, exactInterfaces);
  scopeEnvironmentChain(currentEeId, scope.byId, scope.rootEeId);
  scopeEnvironmentChain(targetEeId, scope.byId, scope.rootEeId);
  const clientSidecarPath = path.join(
    await fs.realpath(path.join(registryRoot, "interface")),
    ownerInterface,
    "octopus_os.json",
  );
  const clientSidecar = await readRegularJsonDocument(clientSidecarPath, `task client sidecar ${ownerInterface}`);
  const parsedSidecar = parseOctopusOsSidecar(clientSidecar.document, ownerInterface, clientSidecarPath);
  if (
    stringValue(parsedSidecar.owner_interface) !== ownerInterface
    || stringValue(parsedSidecar.component) !== "client"
    || stringValue(parsedSidecar.surface_role) !== "task"
    || !isPlainRecord(clientSidecar.document.authoring_profile)
    || parsedSidecar.authoring_profile.ee_id !== currentEeId
  ) {
    throw new Error(`task client '${ownerInterface}' has no movable authoring profile`);
  }
  if (ownedWriters.ids.size || Object.values(objectOrEmpty(current.document.interface_state)).some((state) =>
    ["run_authoring_binding", "writer_run", "input_intake", "file_writer_target", "maintenance_observer"]
      .some((field) => Object.hasOwn(state, field)))) {
    const error = new Error(`task '${runId}' current execution contract does not admit changing EE`);
    error.code = "task_execution_ee_change_unsupported";
    throw error;
  }
  await assertTopologyIsPrivateToRun(octopusHomeRoot, runId, exactInterfaces, new Set(), ownedWriters.ids);
  const targetEnvironment = scope.byId.get(targetEeId);
  const targetPolicy = objectOrEmpty(targetEnvironment?.formed_envelope);
  assertPolicyAxisAdmits(
    targetPolicy.allowed_interfaces,
    new Set([current.document, ...[...ownedWriters.records.values()].map((entry) => entry.document)]
      .flatMap((record) => [...runInterfaceReferences(record)])),
    `target EE '${targetEeId}' interface policy`,
  );
  const workerInterface = requiredSafeRegistryId(current.document.worker, "task worker interface");
  const workerPolicyPath = path.join(
    await fs.realpath(path.join(registryRoot, "interface")),
    workerInterface,
    "policy.json",
  );
  const workerPolicy = await readRegularJsonDocument(workerPolicyPath, `task worker policy ${workerInterface}`);
  if (
    stringValue(workerPolicy.document?.id) !== workerInterface
    || !isPlainRecord(workerPolicy.document?.ceiling)
    || !Array.isArray(workerPolicy.document.ceiling.tools)
    || !Array.isArray(workerPolicy.document.ceiling.wek)
  ) {
    throw new Error(`task worker '${workerInterface}' has malformed capability ceiling`);
  }
  assertPolicyAxisAdmits(
    targetPolicy.allowed_tools,
    new Set(workerPolicy.document.ceiling.tools.map((value) => {
      const id = requiredString(value, "task worker tool");
      if (!safeInterfacePathSegment(id)) throw new Error("task worker tool must be an exact Registry path segment");
      return id;
    })),
    `target EE '${targetEeId}' tool policy`,
  );
  assertPolicyAxisAdmits(
    targetPolicy.allowed_worker_environment_kinds,
    new Set(workerPolicy.document.ceiling.wek.map((value) => requiredSafeRegistryId(value, "task worker environment kind"))),
    `target EE '${targetEeId}' worker-environment policy`,
  );
  const nextSidecar = structuredClone(clientSidecar.document);
  nextSidecar.authoring_profile.ee_id = targetEeId;
  parseOctopusOsSidecar(nextSidecar, ownerInterface, clientSidecarPath);
  const writes = [{ file: clientSidecarPath, beforeDigest: clientSidecar.digest, next: nextSidecar }];
  const nextRun = structuredClone(current.document);
  nextRun.ee = targetEeId;
  writes.push({ file: runPath, beforeDigest: expectedDigest, next: nextRun });
  await commitClientAuthoring({
    label: "move Octopus OS task",
    writes,
    removals: [],
    publishBarrierPath: runPath,
  });
  return { action: "move_task", runId, targetEeId, changed: true };
}

function assertPolicyAxisAdmits(axis, requiredValues, label) {
  if (!Array.isArray(axis)) return;
  const allowed = new Set(axis.map(stringValue).filter(Boolean));
  const missing = [...requiredValues].filter((value) => !allowed.has(value)).sort();
  if (missing.length) throw new Error(`${label} does not admit: ${missing.join(", ")}`);
}

function requiredGoal(value) {
  const goal = requiredString(value, "goal");
  if (goal.length > 512 || /[\u0000-\u001f\u007f]/.test(goal)) {
    throw new Error("goal must be at most 512 visible characters");
  }
  return goal;
}

async function scopedRecordPath(octopusHomeRoot, projectedPath, relativeRoot, fallbackName) {
  const root = await fs.realpath(path.join(octopusHomeRoot, relativeRoot));
  const candidate = stringValue(projectedPath) || path.join(root, fallbackName);
  const resolvedParent = await fs.realpath(path.dirname(candidate));
  if (resolvedParent !== root || path.basename(candidate) !== fallbackName) {
    throw new Error(`projected record path is outside '${root}'`);
  }
  return path.join(resolvedParent, fallbackName);
}

function scopeEnvironmentChain(eeId, byId, rootEeId) {
  const chain = [];
  const seen = new Set();
  let cursor = eeId;
  while (cursor) {
    if (seen.has(cursor)) throw new Error(`EE hierarchy cycles at '${cursor}'`);
    seen.add(cursor);
    const environment = byId.get(cursor);
    if (!environment) throw new Error(`EE '${cursor}' is absent from the Octopus OS scope`);
    chain.push(environment);
    if (cursor === rootEeId) return chain.reverse();
    cursor = requiredSafeRegistryId(environment.parent_ee_id, `parent of EE ${cursor}`);
  }
  throw new Error(`EE '${eeId}' is not below the Octopus OS root`);
}

async function taskArchiveInterfaceIds(registryRoot, run, ownerInterface, runId) {
  const ids = await octopusOsOwnedInterfaceIds(registryRoot, ownerInterface);
  const providerId = requiredSafeRegistryId(run.interface_state[ownerInterface].provider_runtime_interface, "task provider interface");
  if (!ids.has(providerId)) throw new Error(`task '${runId}' has a provider outside its exact client topology`);
  const interfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const readInterface = async (id) => {
    await requirePlainDirectory(path.join(interfaceRoot, id), `task interface ${id}`);
    const { document } = await readRegularJsonDocument(path.join(interfaceRoot, id, "interface.json"), `task interface ${id}`);
    if (document?.id !== id) throw new Error(`task interface '${id}' has drifted descriptor identity`);
    return document;
  };
  const provider = await readInterface(providerId);
  const selection = provider.interface_driver_config?.private_config?.input_intake;
  if (selection === undefined) return ids;
  const label = `task '${runId}' input intake`;
  exactObjectKeys(selection, ["template_interface_id", "interface_id"], ["template_interface_id", "interface_id"], label);
  requiredSafeRegistryId(selection.template_interface_id, `${label} template`);
  const intakeId = requiredSafeRegistryId(selection.interface_id, `${label} interface`);
  const intake = await readInterface(intakeId);
  const declaration = intake.interface_driver_config?.private_config?.input_intake;
  exactObjectKeys(declaration,
    ["schema_version", "run", "provider_interface", "writer", "writer_required_verified_artifacts", "profile", "provider_profile", "input_label", "artifact_bindings", "notification_action"],
    ["schema_version", "run", "provider_interface", "writer", "writer_required_verified_artifacts", "profile", "provider_profile", "input_label", "artifact_bindings"], label);
  if (declaration.notification_action !== undefined && !["queue", "followup"].includes(declaration.notification_action)) {
    throw new Error(`${label} has an invalid notification action`);
  }
  if (intake.interface_type !== "system" || intake.role !== "service" || intake.interface_driver_ref !== "checked_input_intake"
    || declaration.schema_version !== 1 || declaration.run !== runId || declaration.provider_interface !== providerId
    || !isPlainRecord(run.interface_state[intakeId])
    || stableJson(run.interface_state[intakeId].input_intake) !== stableJson(declaration)) {
    throw new Error(`${label} does not match its exact run/provider declaration`);
  }
  const writer = declaration.writer;
  exactObjectKeys(writer, ["owner", "source", "door", "tool", "call_label", "result_label"],
    ["owner", "source", "door", "tool", "call_label", "result_label"], `${label} writer`);
  const doorId = requiredSafeRegistryId(writer.door, `${label} writer door`);
  const toolId = requiredString(writer.tool, `${label} writer tool`);
  if (!safeInterfacePathSegment(toolId)) throw new Error(`${label} writer tool is not a safe path segment`);
  if (writer.owner !== intakeId || writer.source !== intakeId || new Set([ownerInterface, providerId, intakeId, doorId]).size !== 4) {
    throw new Error(`${label} writer has inconsistent exact interfaces`);
  }
  requiredString(declaration.input_label, `${label} input label`);
  requiredString(writer.call_label, `${label} call label`);
  requiredString(writer.result_label, `${label} result label`);
  for (const profile of [declaration.profile, declaration.provider_profile]) {
    exactObjectKeys(profile, ["file_write_tool", "file_write_label", "file_write_result_label", "result_timeout_ms"],
      ["file_write_tool", "file_write_label", "file_write_result_label", "result_timeout_ms"], `${label} profile`);
    if (!Number.isSafeInteger(profile.result_timeout_ms) || profile.result_timeout_ms <= 0) throw new Error(`${label} profile has invalid timeout`);
  }
  if (declaration.profile.file_write_tool !== toolId || declaration.profile.file_write_label !== writer.call_label
    || declaration.profile.file_write_result_label !== writer.result_label || writer.call_label === declaration.input_label
    || !Array.isArray(declaration.artifact_bindings) || !Array.isArray(declaration.writer_required_verified_artifacts)
    || declaration.writer_required_verified_artifacts.length !== 1
    || declaration.writer_required_verified_artifacts[0]?.artifact_label !== writer.call_label) {
    throw new Error(`${label} has inconsistent writer contract`);
  }
  await readInterface(doorId);
  const toolRoot = await fs.realpath(path.join(registryRoot, "tool"));
  await requirePlainDirectory(path.join(toolRoot, toolId), `${label} tool`);
  const tool = await readRegularJsonDocument(path.join(toolRoot, toolId, "tool.json"), `${label} tool`);
  if (tool.document?.id !== toolId || tool.document?.interface !== doorId) throw new Error(`${label} tool does not belong to its exact writer door`);
  const workerId = requiredSafeRegistryId(run.worker, "task worker interface");
  if (!ids.has(workerId)) throw new Error(`${label} worker is outside its exact client topology`);
  const policy = await readRegularJsonDocument(path.join(interfaceRoot, workerId, "policy.json"), `${label} worker ceiling`);
  if (policy.document?.id !== workerId || !Array.isArray(policy.document.ceiling?.interfaces)
    || !Array.isArray(policy.document.ceiling?.tools)
    || [providerId, intakeId, doorId].some((id) => !policy.document.ceiling.interfaces.includes(id))
    || !policy.document.ceiling.tools.includes(toolId)) {
    throw new Error(`${label} writer is outside its exact worker ceiling`);
  }
  const owned = run.interface_state[intakeId].owned_checkpoint_ids;
  if (!Array.isArray(owned) || owned.length === 0 || new Set(owned).size !== owned.length) throw new Error(`${label} has malformed checkpoint ownership`);
  for (const [interfaceId, state] of Object.entries(run.interface_state)) {
    if (!isPlainRecord(state)) throw new Error(`${label} has malformed private state for '${interfaceId}'`);
    for (const field of ["owned_checkpoint_ids", "frontier_checkpoint_ids", "cycle_checkpoint_ids"]) {
      if (state[field] !== undefined && (!Array.isArray(state[field]) || state[field].some((id) => typeof id !== "string"))) {
        throw new Error(`${label} has malformed ${field} for '${interfaceId}'`);
      }
    }
  }
  for (const id of owned) {
    requiredSafeRegistryId(id, `${label} owned checkpoint`);
    const rows = run.checkpoints.filter((checkpoint) => checkpoint.id === id);
    if (rows.length !== 1 || rows[0].run !== runId
      || ![intakeId, doorId].includes(interfaceBindingId(rows[0].interface_binding))
      || !Array.isArray(rows[0].outbound_interface_bindings) || rows[0].outbound_interface_bindings.length !== 1
      || rows[0].outbound_interface_bindings.some((binding) => ![intakeId, doorId, providerId].includes(interfaceBindingId(binding)))
      || Object.entries(run.interface_state).some(([interfaceId, state]) => interfaceId !== intakeId
        && ["owned_checkpoint_ids", "frontier_checkpoint_ids", "cycle_checkpoint_ids"].some((field) => arrayValue(state?.[field]).includes(id)))) {
      throw new Error(`${label} checkpoint '${id}' is outside its exact ownership`);
    }
  }
  ids.add(intakeId);
  ids.add(doorId);
  return ids;
}

async function octopusOsOwnedInterfaceIds(registryRoot, ownerInterface) {
  const interfaceRoot = await fs.realpath(path.join(registryRoot, "interface"));
  const entries = await fs.readdir(interfaceRoot, { withFileTypes: true });
  const ids = new Set();
  for (const entry of entries) {
    if (!entry.isDirectory() || !safeInterfacePathSegment(entry.name)) continue;
    const sidecarPath = path.join(interfaceRoot, entry.name, "octopus_os.json");
    const sidecar = await readOptionalRegularJsonDocument(sidecarPath, `Octopus OS sidecar ${entry.name}`);
    if (!sidecar) continue;
    const parsed = parseOctopusOsSidecar(sidecar.document, entry.name, sidecarPath);
    if (stringValue(parsed.owner_interface) === ownerInterface) ids.add(entry.name);
  }
  if (!ids.has(ownerInterface)) throw new Error(`task client interface '${ownerInterface}' has no exact topology sidecar`);
  return ids;
}

function assertRunUsesOnlyOwnedInterfaces(run, exactInterfaces, runId) {
  const used = runInterfaceReferences(run);
  for (const checkpoint of arrayValue(run?.checkpoints)) {
    if (stringValue(checkpoint?.run) !== runId) {
      throw new Error(`task run '${runId}' has a checkpoint owned by another run`);
    }
  }
  for (const interfaceId of used) {
    if (!interfaceId || !exactInterfaces.has(interfaceId)) {
      throw new Error(`task run '${runId}' binds interface '${interfaceId || "<empty>"}' outside its exact client topology`);
    }
  }
}

function assertRunPrivateStateIsOwned(run, exactInterfaces, runId) {
  const worker = stringValue(run?.worker);
  if (!worker || !exactInterfaces.has(worker)) {
    throw new Error(`task run '${runId}' wears worker '${worker || "<empty>"}' outside its exact client topology`);
  }
  for (const interfaceId of Object.keys(objectOrEmpty(run?.interface_state))) {
    if (!exactInterfaces.has(interfaceId)) {
      throw new Error(`task run '${runId}' stores private state for interface '${interfaceId}' outside its exact client topology`);
    }
  }
  for (const checkpoint of arrayValue(run?.checkpoints)) {
    if (stringValue(checkpoint?.run) !== runId) {
      throw new Error(`task run '${runId}' has a checkpoint owned by another run`);
    }
  }
}

async function assertTaskWorkerPinRemainsTaskLocal(
  octopusHomeRoot,
  workerInterface,
  ownerInterface,
  run,
  exactInterfaces,
) {
  const writers = await octopusOsOwnedWriterRuns(octopusHomeRoot, run, exactInterfaces);
  const runRoot = await fs.realpath(path.join(octopusHomeRoot, "state", "runs"));
  for (const file of await listDirectFiles(runRoot, isRunRecordJsonPath)) {
    let candidate;
    try {
      candidate = await readRegularJsonDocument(file, `run record ${file}`);
    } catch {
      continue;
    }
    if (stringValue(candidate.document?.worker) !== workerInterface) continue;
    const writer = writers.records.get(candidate.document?.id);
    if (writer && candidate.digest === writer.digest && file === writer.file) continue;
    if (!runInterfaceReferences(candidate.document).has(ownerInterface)) {
      throw new Error(
        `task worker '${workerInterface}' is also worn by run '${stringValue(candidate.document?.id)}' outside exact task interface '${ownerInterface}'`,
      );
    }
  }
}

async function octopusOsOwnedWriterRuns(octopusHomeRoot, run, exactInterfaces) {
  const runRoot = await fs.realpath(path.join(octopusHomeRoot, "state", "runs"));
  return octopusOsOwnedWriterRunsAtRoot(runRoot, run, exactInterfaces);
}

async function octopusOsOwnedWriterRunsAtRoot(runRoot, run, exactInterfaces) {
  const ids = new Set(), writes = [], records = new Map();
  for (const [owner, state] of Object.entries(objectOrEmpty(run.interface_state))) {
    const provider = state?.run_authoring_binding;
    const binding = provider?.writer_run ?? state?.writer_run;
    if (binding === undefined) continue;
    const label = `task '${run.id}' writer for '${owner}'`;
    exactObjectKeys(binding, ["run", "initial_record", "initial_record_format", "role_artifact_bindings"], ["run", "initial_record", "role_artifact_bindings"], label);
    if (Object.hasOwn(binding, "initial_record_format") && !["pretty", "compact"].includes(binding.initial_record_format)) {
      throw new Error(`${label} has invalid initial record format`);
    }
    const id = requiredSafeRegistryId(binding.run, `${label} Run`);
    if (id === run.id || ids.has(id) || !exactInterfaces.has(owner)
      || (provider && state.writer_run !== undefined) || !Array.isArray(binding.role_artifact_bindings)) {
      throw new Error(`${label} has conflicting ownership`);
    }
    const initial = binding.initial_record;
    exactObjectKeys(initial, ["id", "ee", "worker", "trace", "status", "interface_state", "checkpoints", "active_checkpoints"],
      ["id", "ee", "worker", "trace", "status", "interface_state", "checkpoints", "active_checkpoints"], label);
    if (initial.id !== id || initial.ee !== run.ee || initial.worker !== run.worker
      || initial.trace !== `trace:${id}` || initial.status !== "idle"
      || !isPlainRecord(initial.interface_state) || stableJson(Object.keys(initial.interface_state)) !== stableJson([owner])) {
      throw new Error(`${label} changed its exact identity, worker or EE`);
    }
    exactObjectKeys(initial.interface_state[owner], ["file_writer_target"], ["file_writer_target"], label);
    const target = initial.interface_state[owner].file_writer_target;
    exactObjectKeys(target, ["run", "provider", "source", "door", "generation"], ["run", "provider", "source", "door", "generation"], label);
    if (target.run !== run.id || target.provider !== owner || target.generation !== 0
      || !exactInterfaces.has(target.source) || !exactInterfaces.has(target.door) || target.source === target.door) {
      throw new Error(`${label} changed its exact target or private interfaces`);
    }
    let templates;
    if (provider) {
      if (provider.run !== run.id || provider.worker !== run.worker || provider.provider_interface !== owner
        || provider.authoring_source_interface !== target.source || !Array.isArray(provider.checkpoints)) {
        throw new Error(`${label} differs from its provider declaration`);
      }
      templates = ["authoring_call", "authoring_result"].map((role) => {
        const rows = provider.checkpoints.filter((row) => row.role === role);
        if (rows.length !== 1) throw new Error(`${label} has ambiguous authoring roles`);
        return rows[0];
      });
    } else {
      const writer = state.input_intake?.writer;
      if (!writer || writer.owner !== owner || writer.source !== target.source || writer.door !== target.door
        || state.input_intake.run !== run.id) throw new Error(`${label} lacks its exact intake declaration`);
    }
    if (!Array.isArray(initial.checkpoints) || initial.checkpoints.length !== 2 || !Array.isArray(initial.active_checkpoints)) {
      throw new Error(`${label} needs one exact call/result pair`);
    }
    const checkpoints = new Set();
    for (const [source, destination] of [[target.source, target.door], [target.door, owner]]) {
      const rows = initial.checkpoints.filter((cp) => interfaceBindingId(cp.interface_binding) === source
        && Array.isArray(cp.outbound_interface_bindings) && cp.outbound_interface_bindings.length === 1
        && interfaceBindingId(cp.outbound_interface_bindings[0]) === destination);
      if (rows.length !== 1) throw new Error(`${label} changed its exact call/result edge`);
      if (arrayValue(run.checkpoints).some((cp) => interfaceBindingId(cp.interface_binding) === source
        && arrayValue(cp.outbound_interface_bindings).some((binding) => interfaceBindingId(binding) === destination))) {
        throw new Error(`${label} duplicates its writer edge in the target Run`);
      }
      const cp = rows[0], checkpointId = requiredSafeRegistryId(cp.id, `${label} checkpoint`);
      if (checkpoints.has(checkpointId) || cp.run !== id || cp.trace !== `trace:${id}:${checkpointId}`
        || !Number.isSafeInteger(cp.epoch) || cp.epoch < 0
        || !Array.isArray(cp.artifact_bindings) || !Array.isArray(cp.required_verified_artifacts)) {
        throw new Error(`${label} has malformed checkpoint ownership`);
      }
      if (templates) {
        const template = templates[source === target.source ? 0 : 1];
        for (const key of ["interface_binding", "outbound_interface_bindings", "required_verified_artifacts"]) {
          if (stableJson(cp[key]) !== stableJson(template[key])) throw new Error(`${label} changed its declared ${key}`);
        }
      }
      checkpoints.add(checkpointId);
    }
    if (stableJson([...checkpoints].sort()) !== stableJson([...initial.active_checkpoints].sort())) {
      throw new Error(`${label} has an unrelated active checkpoint`);
    }
    const file = path.join(runRoot, `${id}.json`);
    const current = await readRegularJsonDocument(file, label);
    const generation = current.document?.interface_state?.[owner]?.file_writer_target?.generation;
    if (!Number.isSafeInteger(generation) || generation < 0) throw new Error(`${label} has invalid generation`);
    const expected = structuredClone(initial);
    if (generation > 0) {
      expected.interface_state[owner].file_writer_target.generation = generation;
      for (const cp of expected.checkpoints) {
        cp.id = `${cp.id}_${generation}`; cp.trace = `trace:${id}:${cp.id}`;
      }
      expected.active_checkpoints = expected.checkpoints.map((cp) => cp.id);
    }
    if (stableJson(current.document) !== stableJson(expected)) throw new Error(`${label} drifted from its declared generation`);
    ids.add(id);
    records.set(id, { ...current, file, owner, provider: Boolean(provider) });
    writes.push({ file, beforeDigest: current.digest, next: null });
  }
  return { ids, writes, records };
}

async function assertTopologyIsPrivateToRun(octopusHomeRoot, runId, exactInterfaces, artifactIds = new Set(), ownedRunIds = new Set()) {
  const runRoot = await fs.realpath(path.join(octopusHomeRoot, "state", "runs"));
  for (const file of await listDirectFiles(runRoot, isRunRecordJsonPath)) {
    if (path.basename(file) === `${runId}.json` || ownedRunIds.has(path.basename(file, ".json"))) continue;
    const candidate = await readRegularJsonDocument(file, `run record ${file}`);
    const used = runInterfaceReferences(candidate.document);
    for (const interfaceId of exactInterfaces) {
      if (used.has(interfaceId)) throw new Error(`exact interface '${interfaceId}' is also bound by run '${stringValue(candidate.document?.id)}'`);
    }
    const runReferences = exactStringReferencePaths(candidate.document, new Set([runId, ...ownedRunIds, ...artifactIds]));
    if (runReferences.length) {
      throw new Error(`task run '${runId}' is referenced by run '${stringValue(candidate.document?.id)}' at ${runReferences[0]}`);
    }
  }
}

function runInterfaceReferences(run) {
  const used = new Set([stringValue(run?.worker)]);
  for (const interfaceId of Object.keys(objectOrEmpty(run?.interface_state))) used.add(interfaceId);
  for (const checkpoint of arrayValue(run?.checkpoints)) {
    const inbound = interfaceBindingId(checkpoint?.interface_binding);
    if (inbound) used.add(inbound);
    for (const outbound of arrayValue(checkpoint?.outbound_interface_bindings).map(interfaceBindingId).filter(Boolean)) used.add(outbound);
  }
  return used;
}

async function snapshotOwnedRegistryDirectories(root, ids, label) {
  const directories = [];
  for (const id of [...ids].sort()) {
    if (!safeInterfacePathSegment(id)) throw new Error(`${label} id '${id}' is not a safe path segment`);
    const directory = path.join(root, id);
    const descriptor = await readRegularJsonDocument(path.join(directory, "interface.json"), `${label} ${id}`);
    if (stringValue(descriptor.document?.id ?? descriptor.document?.rule_id) !== id) {
      throw new Error(`${label} '${id}' has drifted descriptor identity`);
    }
    directories.push(await snapshotRemovalDirectory(directory, `${label} ${id}`, true));
  }
  return directories;
}

async function octopusOsOwnedToolTopology(registryRoot, exactInterfaces, companions = null) {
  const toolRoot = await fs.realpath(path.join(registryRoot, "tool"));
  const entries = await fs.readdir(toolRoot, { withFileTypes: true });
  const ids = new Set();
  const directories = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || !safeInterfacePathSegment(entry.name)) continue;
    const directory = path.join(toolRoot, entry.name);
    const descriptor = await readRegularJsonDocument(path.join(directory, "tool.json"), `tool descriptor ${entry.name}`);
    if (stringValue(descriptor.document?.id) !== entry.name) {
      throw new Error(`tool descriptor '${entry.name}' has drifted identity`);
    }
    if (!exactInterfaces.has(stringValue(descriptor.document?.interface))) continue;
    if (companions?.interfaces.has(descriptor.document.interface) && !companions.tools.has(entry.name)) {
      throw new Error(`tool '${entry.name}' reaches companion topology without declared ownership`);
    }
    ids.add(entry.name);
    directories.push(await snapshotRemovalDirectory(directory, `task tool ${entry.name}`, true));
  }
  return { ids, directories };
}

async function octopusOsOwnedArtifactTopology(registryRoot, run, exactInterfaces) {
  const ids = new Set(), writes = [], documents = new Map();
  const artifactRoot = path.join(registryRoot, "artifact");
  const reference = (value) => {
    exactObjectKeys(value, ["id", "content"], ["id", "content"], "task artifact reference");
    const id = requiredArtifactBindingId(value.id, "task artifact id");
    if ([".", ".."].includes(id) || id.includes("/") || id.includes("\\")
      || !Array.isArray(value.content) || value.content.length !== 32
      || value.content.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255)) {
      throw new Error(`task artifact '${id}' has invalid identity or content pin`);
    }
    return id;
  };
  const readOwned = async (ref) => {
    const id = reference(ref);
    if (await fs.realpath(artifactRoot) !== artifactRoot) throw new Error("task artifact registry must not be a symbolic link");
    const directory = path.join(artifactRoot, id);
    await requirePlainDirectory(directory, `task artifact ${id}`);
    const file = path.join(directory, "artifact.json");
    const descriptor = await readRegularJsonDocument(file, `task artifact ${id}`);
    const hash = Buffer.from(blake3(Buffer.from(stableJson(descriptor.document)))).toString("hex");
    if (descriptor.document?.id !== id || descriptor.document?.run !== run.id || hash !== contentHashHex(ref.content)) {
      throw new Error(`task artifact '${id}' has drifted identity, ownership or content`);
    }
    if (documents.has(id) && documents.get(id).digest !== descriptor.digest) throw new Error(`task artifact '${id}' changed during ownership validation`);
    if (!ids.has(id)) {
      ids.add(id);
      documents.set(id, descriptor);
      writes.push({ file, beforeDigest: descriptor.digest, next: null });
    }
    return descriptor.document;
  };
  for (const interfaceId of [...exactInterfaces].sort()) {
    const sidecar = await readOptionalRegularJsonDocument(
      path.join(registryRoot, "interface", interfaceId, "code_mode.json"), `task Code Mode ${interfaceId}`,
    );
    if (!sidecar || sidecar.document?.context?.mode !== "persistent") continue;
    const preludeRef = sidecar.document.context.prelude_artifact;
    const topology = await readOwned(sidecar.document.handoff?.topology_artifact);
    const prelude = await readOwned(preludeRef);
    if (topology.owner_interface !== interfaceId || topology.worker !== run.worker
      || prelude.owner_interface !== interfaceId || stableJson(topology.prelude_artifact) !== stableJson(preludeRef)) {
      throw new Error(`task Code Mode '${interfaceId}' has inconsistent artifact ownership`);
    }
    const sources = run.interface_state?.[interfaceId]?.prelude_sources;
    if (!Array.isArray(sources) || sources.length === 0) throw new Error(`task Code Mode '${interfaceId}' has no source ownership evidence`);
    const sourceIds = new Set();
    for (const source of sources) {
      exactObjectKeys(source, ["source", "snapshot", "source_sha256"], ["source", "snapshot", "source_sha256"], "task prelude source");
      const sourceId = reference(source.source);
      const snapshotId = reference(source.snapshot);
      if (sourceId === snapshotId || sourceIds.has(sourceId)) throw new Error("task prelude source ownership is ambiguous");
      sourceIds.add(sourceId);
      const snapshot = await readOwned(source.snapshot);
      if (stableJson(snapshot.source) !== stableJson(source.source) || snapshot.source_sha256 !== source.source_sha256
        || typeof snapshot.source_content !== "string"
        || crypto.createHash("sha256").update(snapshot.source_content).digest("hex") !== source.source_sha256) {
        throw new Error(`task prelude snapshot '${snapshotId}' differs from its source evidence`);
      }
      const original = JSON.parse(snapshot.source_content);
      if (original?.id !== sourceId || Buffer.from(blake3(Buffer.from(stableJson(original)))).toString("hex") !== contentHashHex(source.source.content)) {
        throw new Error(`task prelude snapshot '${snapshotId}' has drifted source identity or content`);
      }
    }
  }
  return { ids, writes };
}

async function taskTopologyPolicyWrites(registryRoot, exactInterfaces, ownedTools) {
  const writes = [];
  const eeTemplateRoot = await fs.realpath(path.join(registryRoot, "ee_template"));
  for (const entry of await fs.readdir(eeTemplateRoot, { withFileTypes: true })) {
    if (!entry.isDirectory() || !safeInterfacePathSegment(entry.name)) continue;
    const file = path.join(eeTemplateRoot, entry.name, "ee_template.json");
    const descriptor = await readRegularJsonDocument(file, `EE template ${entry.name}`);
    if (stringValue(descriptor.document?.id) !== entry.name) {
      throw new Error(`EE template '${entry.name}' has drifted identity`);
    }
    const next = structuredClone(descriptor.document);
    filterDescriptorStringList(next, "allowed_interfaces", exactInterfaces, `EE template ${entry.name}`);
    filterDescriptorStringList(next, "allowed_tools", ownedTools, `EE template ${entry.name}`);
    filterDescriptorStringList(next, "hard_blocked_tools", ownedTools, `EE template ${entry.name}`);
    if (next.interface_policy_overrides !== undefined) {
      if (!isPlainRecord(next.interface_policy_overrides)) {
        throw new Error(`EE template '${entry.name}'.interface_policy_overrides must be an object`);
      }
      for (const interfaceId of exactInterfaces) delete next.interface_policy_overrides[interfaceId];
    }
    if (stableJson(next) !== stableJson(descriptor.document)) {
      writes.push({ file, beforeDigest: descriptor.digest, next });
    }
  }

  return writes;
}

function filterDescriptorStringList(document, field, removed, label) {
  if (document[field] === undefined || document[field] === null) return;
  if (!Array.isArray(document[field]) || document[field].some((value) => !stringValue(value))) {
    throw new Error(`${label}.${field} must be an array of non-empty strings`);
  }
  document[field] = document[field].filter((value) => !removed.has(value));
}

async function assertNoExternalTaskTopologyReferences(
  registryRoot,
  excludedDirectories,
  plannedDocuments,
  topologyIds,
  lifecycle = null,
) {
  const files = await registryControlJsonFiles(registryRoot, excludedDirectories);
  for (const file of files) {
    if (plannedDocuments.has(file) && plannedDocuments.get(file) === null) continue;
    const planned = plannedDocuments.get(file);
    let document = planned ?? (await readRegularJsonDocument(file, `registry control document ${file}`)).document;
    if (lifecycle && path.basename(file) === "artifact.json"
      && path.dirname(path.dirname(file)) === path.join(registryRoot, "artifact")
      && document.collection_catalogue !== undefined) {
      const canonicalContentHash = (value) => Buffer.from(blake3(Buffer.from(stableJson(value)))).toString("hex");
      document = lifecycle.catalogueProvenanceProjection(document, {
        scope: document.collection_catalogue?.scope,
        snapshot: { id: document.id, content: canonicalContentHash(document) },
      }, canonicalContentHash);
    }
    const references = exactStringReferencePaths(document, topologyIds);
    if (references.length) {
      throw new Error(`task topology is still referenced by '${file}' at ${references.slice(0, 4).join(", ")}`);
    }
  }
}

async function registryControlJsonFiles(registryRoot, excludedDirectories) {
  const files = [];
  const rootEntries = await fs.readdir(registryRoot, { withFileTypes: true });
  for (const rootEntry of rootEntries) {
    const rootPath = path.join(registryRoot, rootEntry.name);
    if (rootEntry.isFile() && rootEntry.name.endsWith(".json")) {
      files.push(rootPath);
      continue;
    }
    if (!rootEntry.isDirectory()) continue;
    for (const entry of await fs.readdir(rootPath, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const directory = path.join(rootPath, entry.name);
      if (excludedDirectories.has(directory)) continue;
      for (const file of await fs.readdir(directory, { withFileTypes: true })) {
        if (file.isFile() && file.name.endsWith(".json")) files.push(path.join(directory, file.name));
      }
    }
  }
  return files.sort();
}

function exactStringReferencePaths(value, targets, cursor = "$", found = []) {
  if (typeof value === "string") {
    if (targets.has(value)) found.push(cursor);
    return found;
  }
  if (Array.isArray(value)) {
    value.forEach((entry, index) => exactStringReferencePaths(entry, targets, `${cursor}[${index}]`, found));
    return found;
  }
  if (!isPlainRecord(value)) return found;
  for (const [key, entry] of Object.entries(value)) {
    if (targets.has(key)) found.push(`${cursor}{${JSON.stringify(key)}}`);
    exactStringReferencePaths(entry, targets, `${cursor}.${key}`, found);
  }
  return found;
}

async function snapshotOwnedInterfaceStateDirectories(octopusHomeRoot, exactInterfaces) {
  return [
    ...await snapshotOwnedInterfaceDirectories(
      octopusHomeRoot,
      "interfaces",
      exactInterfaces,
      "interface private state",
    ),
    ...await snapshotOwnedInterfaceDirectories(
      octopusHomeRoot,
      "interface-hand-in",
      exactInterfaces,
      "interface hand-in state",
    ),
  ];
}

async function assertTaskWorkerEnvironmentsReleased(octopusHomeRoot, exactInterfaces, runId, ownedWriters = new Map(), sourceRecord = null) {
  for (const interfaceId of [...exactInterfaces].sort()) {
    const root = path.join(
      octopusHomeRoot,
      "interfaces",
      interfaceId,
      "worker-environments",
    );
    const entries = await readDirectoryEntries(root);
    for (const entry of entries) {
      if (!entry.isDirectory() || !/^we_[0-9a-f]{32}$/.test(entry.name)) {
        throw new Error(`task interface '${interfaceId}' has malformed worker-environment state`);
      }
      const recordPath = path.join(root, entry.name, "worker_environment.json");
      const record = await readOptionalRegularJsonDocument(
        recordPath,
        `worker environment ${entry.name}`,
      );
      if (!record) throw new Error(`worker environment '${entry.name}' has no lifecycle record`);
      const formation = objectOrEmpty(record.document?.formation);
      const writer = ownedWriters.get(formation.run);
      const writerTarget = writer?.document?.interface_state?.[writer.owner]?.file_writer_target;
      const ownedWriter = writer && writerTarget?.run === (writer.targetRun ?? runId) && writerTarget.door === interfaceId
        && formation.worker === writer.document.worker && formation.ee === writer.document.ee;
      const ownedCompanion = writer && ["observer", "projection"].includes(writer.role)
        && writer.interfaces.has(interfaceId)
        && formation.worker === writer.document.worker && formation.ee === writer.document.ee;
      const ownedSource = stringValue(formation.run) === runId
        && (!sourceRecord || formation.worker === sourceRecord.worker && formation.ee === sourceRecord.ee);
      if (
        record.document?.schema_version !== 5
        || stringValue(formation.worker_environment_id) !== entry.name
        || stringValue(formation.door_interface) !== interfaceId
        || (!ownedSource && !ownedWriter && !ownedCompanion)
        || typeof formation.requires_release !== "boolean"
        || !["active", "failed", "released"].includes(stringValue(record.document?.lifecycle))
      ) {
        throw new Error(`worker environment '${entry.name}' does not belong to task '${runId}'`);
      }
      if (
        formation.requires_release === true
        && stringValue(record.document.lifecycle) !== "released"
      ) {
        throw new Error(
          `task '${runId}' cannot be deleted before worker environment '${entry.name}' is released`,
        );
      }
    }
  }
}

async function snapshotReleasedRunWorkerEnvironmentStateDirectories(
  octopusHomeRoot,
  run,
  runId,
) {
  const runEe = requiredSafeRegistryId(run?.ee, `read-only task '${runId}' EE`);
  const runWorker = requiredSafeRegistryId(run?.worker, `read-only task '${runId}' worker`);
  const interfacesRoot = path.join(octopusHomeRoot, "interfaces");
  const records = await listWorkerEnvironmentRecordFiles(interfacesRoot);
  const directories = [];
  const ids = [];
  for (const recordPath of records) {
    const record = await readRegularJsonDocument(recordPath, `worker environment ${recordPath}`);
    const formation = objectOrEmpty(record.document?.formation);
    if (stringValue(formation.run) !== runId) continue;
    const relative = path.relative(interfacesRoot, recordPath).split(path.sep);
    const interfaceId = requiredSafeRegistryId(relative[0], "worker environment door interface");
    const workerEnvironmentId = requiredSafeRegistryId(
      formation.worker_environment_id,
      "worker environment id",
    );
    const lifecycle = stringValue(record.document?.lifecycle);
    if (
      relative.length !== 4
      || relative[1] !== "worker-environments"
      || relative[2] !== workerEnvironmentId
      || relative[3] !== "worker_environment.json"
      || !/^we_[0-9a-f]{32}$/.test(workerEnvironmentId)
      || record.document?.schema_version !== 5
      || !validWorkerEnvironmentFormation(formation)
      || stringValue(formation.door_interface) !== interfaceId
      || stringValue(formation.ee) !== runEe
      || stringValue(formation.worker) !== runWorker
      || !["active", "failed", "released"].includes(lifecycle)
    ) {
      throw new Error(
        `worker environment '${workerEnvironmentId}' does not belong to read-only task '${runId}'`,
      );
    }
    if (lifecycle !== "released") {
      throw new Error(
        `read-only task '${runId}' cannot be deleted while worker environment '${workerEnvironmentId}' is '${lifecycle}'`,
      );
    }
    directories.push(await snapshotRemovalDirectory(
      path.dirname(recordPath),
      `read-only task worker environment ${workerEnvironmentId}`,
      true,
    ));
    ids.push(workerEnvironmentId);
  }
  return { directories, ids: ids.sort() };
}

async function snapshotOwnedInterfaceDirectories(
  octopusHomeRoot,
  rootName,
  exactInterfaces,
  label,
) {
  const root = path.join(octopusHomeRoot, rootName);
  let rootStat;
  try {
    rootStat = await fs.lstat(root);
  } catch (error) {
    if (error?.code === "ENOENT") return [];
    throw error;
  }
  if (rootStat.isSymbolicLink() || !rootStat.isDirectory()) {
    throw new Error(`Octopus ${label} root must be a directory and not a symbolic link`);
  }
  const directories = [];
  for (const interfaceId of [...exactInterfaces].sort()) {
    const directory = path.join(root, interfaceId);
    let stat;
    try {
      stat = await fs.lstat(directory);
    } catch (error) {
      if (error?.code === "ENOENT") continue;
      throw error;
    }
    if (stat.isSymbolicLink() || !stat.isDirectory()) {
      throw new Error(`${label} '${interfaceId}' must be a directory and not a symbolic link`);
    }
    directories.push(removalSnapshot(directory, "directory", stat, `${label} ${interfaceId}`));
  }
  return directories;
}

async function snapshotRemovalDirectory(directory, label, captureDirectJson) {
  const stat = await fs.lstat(directory);
  if (stat.isSymbolicLink() || !stat.isDirectory()) {
    throw new Error(`${label} must be a directory and not a symbolic link`);
  }
  const snapshot = removalSnapshot(directory, "directory", stat, label);
  if (captureDirectJson) snapshot.directJsonDigests = await directJsonDigests(directory, label);
  return snapshot;
}

async function snapshotRemovalFile(file, expectedDigest, label) {
  const stat = await fs.lstat(file);
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${label} must be a regular file`);
  const document = await readRegularJsonDocument(file, label);
  if (document.digest !== expectedDigest) throw new Error(`${label} changed before deletion`);
  return { ...removalSnapshot(file, "file", stat, label), expectedDigest };
}

function removalSnapshot(targetPath, type, stat, label) {
  return {
    path: targetPath,
    type,
    dev: stat.dev,
    ino: stat.ino,
    label,
  };
}

async function directJsonDigests(directory, label) {
  const digests = new Map();
  for (const entry of await fs.readdir(directory, { withFileTypes: true })) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const file = path.join(directory, entry.name);
    const document = await readRegularJsonDocument(file, `${label} control ${entry.name}`);
    digests.set(entry.name, document.digest);
  }
  return digests;
}

async function requireUnchangedRemoval(removal) {
  const stat = await fs.lstat(removal.path);
  const typeMatches = removal.type === "directory" ? stat.isDirectory() : stat.isFile();
  if (stat.isSymbolicLink() || !typeMatches || stat.dev !== removal.dev || stat.ino !== removal.ino) {
    throw new Error(`${removal.label} changed before deletion`);
  }
  if (removal.expectedDigest) {
    await requireUnchangedJsonDigest(removal.path, removal.expectedDigest, removal.label);
  }
  if (removal.directJsonDigests) {
    const current = await directJsonDigests(removal.path, removal.label);
    if (
      current.size !== removal.directJsonDigests.size
      || [...removal.directJsonDigests].some(([name, digest]) => current.get(name) !== digest)
    ) {
      throw new Error(`${removal.label} control documents changed before deletion`);
    }
  }
  if (removal.directFileDigests) {
    const current = new Map();
    for (const entry of await fs.readdir(removal.path, { withFileTypes: true })) {
      if (entry.name === ".octopus-publication") { await requirePrivatePublicationDirectory(path.join(removal.path, entry.name)); continue; }
      if (!entry.isFile() || entry.isSymbolicLink()) {
        throw new Error(`${removal.label} contains a non-file entry before deletion`);
      }
      const bytes = await fs.readFile(path.join(removal.path, entry.name));
      current.set(entry.name, crypto.createHash("sha256").update(bytes).digest("hex"));
    }
    if (
      current.size !== removal.directFileDigests.size
      || [...removal.directFileDigests].some(([name, digest]) => current.get(name) !== digest)
    ) {
      throw new Error(`${removal.label} files changed before deletion`);
    }
  }
}

async function requirePrivatePublicationDirectory(directory) {
  const stat = await fs.lstat(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink() || (stat.mode & 0o077) !== 0 || await fs.realpath(directory) !== directory) {
    throw new Error("Publication metadata must be a plain private directory");
  }
}

async function assertNoRunRecordReferencesEe(octopusHomeRoot, eeId) {
  const runRoot = await fs.realpath(path.join(octopusHomeRoot, "state", "runs"));
  for (const file of await listDirectFiles(runRoot, isRunRecordJsonPath)) {
    const candidate = await readRegularJsonDocument(file, `run record ${file}`);
    if (stringValue(candidate.document?.ee) === eeId) {
      throw new Error(`EE '${eeId}' is still referenced by run '${stringValue(candidate.document?.id)}'`);
    }
  }
}

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
  writes.push({ file: targetToolPath, beforeDigest: null, next: remapScheduledValue(sourceTool.document, interfaceMap, parsed.config.run_template.id, runId, clonedTopology.checkpointMap) });
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
    ["id", "interface_type", "role", "direction", "source_names", "reference", "interface_driver_ref", "interface_driver_config", "interface_policy"],
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

async function readRegularJsonDocument(file, label) {
  const before = await fs.lstat(file);
  if (before.isSymbolicLink() || !before.isFile()) throw new Error(`${label} must be a regular file`);
  const body = await fs.readFile(file);
  const after = await fs.lstat(file);
  if (after.isSymbolicLink() || !after.isFile() || before.ino !== after.ino || before.dev !== after.dev || before.size !== after.size || before.mtimeMs !== after.mtimeMs) {
    throw new Error(`${label} changed while it was read`);
  }
  let document;
  try {
    document = JSON.parse(body.toString("utf8"));
  } catch (error) {
    throw new Error(`${label} is malformed JSON: ${error.message}`);
  }
  return { document, digest: crypto.createHash("sha256").update(body).digest("hex"), body };
}

async function readOptionalRegularJsonDocument(file, label) {
  try {
    return await readRegularJsonDocument(file, label);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

async function requireUnchangedJsonDigest(file, expectedDigest, label) {
  const current = await readRegularJsonDocument(file, label);
  if (current.digest !== expectedDigest) throw new Error(`${label} changed before the edit could be committed`);
}

async function requirePlainDirectory(directory, label) {
  const stat = await fs.lstat(directory);
  if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error(`${label} must be a directory and not a symbolic link`);
}

function requiredSafeRegistryId(value, label) {
  const id = requiredString(value, label);
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(id)) throw new Error(`${label} must be a registry-safe id`);
  return id;
}

const CHANGE_JOURNAL_MAX_BYTES = 1024 * 1024;
const CHANGE_JOURNAL_MAX_FILE_BYTES = 16 * 1024 * 1024;

async function currentFileDigest(file) {
  const stat = await fs.lstat(file).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
  if (!stat) return null;
  if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`${file} is not a regular file`);
  if (stat.size > CHANGE_JOURNAL_MAX_FILE_BYTES) throw new Error(`${file} exceeds the journal file limit`);
  return crypto.createHash("sha256").update(await fs.readFile(file)).digest("hex");
}

async function changeJournalPlan(octopusHome, registryRoot, runId) {
  requiredSafeRegistryId(runId, "change journal run");
  const directory = path.join(octopusHome, "state", "change-journals", runId);
  const file = path.join(directory, "journal.json");
  const stat = await fs.lstat(file).catch((error) => error?.code === "ENOENT" ? null : Promise.reject(error));
  if (!stat) return null;
  if (stat.size > CHANGE_JOURNAL_MAX_BYTES) throw new Error("change journal exceeds its size limit");
  const { document, digest } = await readRegularJsonDocument(file, "change journal");
  exactObjectKeys(document, ["schema_version", "run", "trigger_artifact", "outcome", "summary", "changes", "checks"], ["schema_version", "run", "outcome", "summary", "changes"], "change journal");
  if (document.schema_version !== 1 || document.run !== runId) throw new Error("change journal belongs to another run or schema");
  if (!["changed", "unchanged"].includes(document.outcome)) throw new Error("change journal outcome must be changed or unchanged");
  if (!Array.isArray(document.changes) || (document.checks !== undefined && !Array.isArray(document.checks))) throw new Error("change journal changes and checks must be arrays");
  const restorableRoots = [
    ...["interface", "tool", "artifact", "ee_template"].map((kind) => path.join(registryRoot, kind)),
    path.join(octopusHome, "state", "runs"),
    path.join(octopusHome, "state", "ee_instances"),
    path.join(octopusHome, "interfaces"),
  ];
  const entries = [];
  const writes = [];
  for (const [index, change] of document.changes.entries()) {
    if (change?.kind !== "file") {
      entries.push({ kind: "other", description: stringValue(change?.description), undo: stringValue(change?.undo), status: "manual" });
      continue;
    }
    exactObjectKeys(change, ["kind", "path", "action", "before_sha256", "after_sha256", "before_copy", "reason"], ["kind", "path", "action", "before_sha256", "after_sha256", "reason"], `change journal entry ${index + 1}`);
    const target = stringValue(change.path);
    if (!path.isAbsolute(target) || path.resolve(target) !== target) throw new Error(`change journal entry ${index + 1} needs a canonical absolute path`);
    if (!["created", "modified", "deleted"].includes(change.action)) throw new Error(`change journal entry ${index + 1} has an invalid action`);
    const digest = (value) => value === null ? null : (/^[0-9a-f]{64}$/.test(stringValue(value)) ? value : (() => { throw new Error(`change journal entry ${index + 1} has an invalid hash`); })());
    const before = digest(change.before_sha256);
    const after = digest(change.after_sha256);
    if ((change.action === "created") !== (before === null) || (change.action === "deleted") !== (after === null)) throw new Error(`change journal entry ${index + 1} hashes do not match its action`);
    const current = await currentFileDigest(target);
    const status = current === after ? "applied" : current === before ? "undone" : "changed";
    let restore = "";
    let reason = "";
    if (before !== null) {
      const copy = stringValue(change.before_copy);
      const copyPath = path.resolve(directory, copy);
      if (!copy || !isPathInside(directory, copyPath)) reason = "its before copy is missing";
      else {
        const bytes = await fs.readFile(copyPath).catch(() => null);
        if (!bytes || crypto.createHash("sha256").update(bytes).digest("hex") !== before) reason = "its before copy does not match the recorded hash";
        else if (!Buffer.from(bytes.toString("utf8")).equals(bytes)) reason = "its before copy is not text";
        else restore = bytes.toString("utf8");
      }
    }
    if (!reason && !restorableRoots.some((root) => isPathInside(root, target))) reason = "it lies outside the Home folders the client restores";
    entries.push({ kind: "file", path: target, action: change.action, reason: stringValue(change.reason), status, restorable: !reason, unrestorable_reason: reason });
    if (!reason) writes.push({ file: target, beforeDigest: after, ...(before === null ? { next: null } : { content: restore }), label: `restore ${path.basename(target)}` });
  }
  const files = entries.filter((entry) => entry.kind === "file");
  return {
    digest,
    run_id: runId,
    outcome: document.outcome,
    summary: stringValue(document.summary),
    trigger_artifact: stringValue(document.trigger_artifact),
    checks: arrayValue(document.checks).map((check) => ({ check: stringValue(check?.check), result: stringValue(check?.result), evidence: stringValue(check?.evidence) })),
    changes: entries,
    undoable: files.length > 0 && files.every((entry) => entry.status === "applied" && entry.restorable),
    undone: files.length > 0 && files.every((entry) => entry.status === "undone"),
    writes,
  };
}

async function undoChangeJournal(registryRoot, octopusHomeRoot, request, commitClientAuthoring) {
  if (typeof commitClientAuthoring !== "function") throw new Error("client authoring transaction is unavailable");
  const runId = requiredSafeRegistryId(request?.runId, "runId");
  const plan = await changeJournalPlan(octopusHomeRoot, registryRoot, runId);
  if (!plan) throw new Error(`run ${runId} kept no change journal`);
  if (plan.digest !== requiredDigest(request?.expectedJournalDigest, "expectedJournalDigest")) throw new Error("the change journal changed before undo");
  if (!plan.undoable) {
    const blocked = plan.changes.filter((entry) => entry.kind === "file" && (entry.status !== "applied" || !entry.restorable));
    throw new Error(`undo refused: ${blocked.map((entry) => `${entry.path} ${entry.status !== "applied" ? `is ${entry.status}` : `cannot be restored because ${entry.unrestorable_reason}`}`).join("; ")}`);
  }
  await commitClientAuthoring({
    label: `undo changes recorded by ${runId}`,
    writes: plan.writes,
    removals: [],
    publishBarrierPath: plan.writes.at(-1).file,
  });
  return { action: "undo_changes", runId, restored: plan.writes.map((write) => write.file) };
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
