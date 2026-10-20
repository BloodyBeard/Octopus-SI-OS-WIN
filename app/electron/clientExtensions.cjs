const crypto = require("node:crypto");
const fs = require("node:fs");
const fsPromises = require("node:fs/promises");
const path = require("node:path");
const Ajv2020 = require("ajv/dist/2020");

const clientExtensionManifestSchema = require("../schemas/client_extension_manifest.schema.json");
const clientSurfaceDocumentSchema = require("../schemas/client_surface_document.schema.json");
const projectionReadContractSchema = require("../schemas/projection_read_contract.schema.json");

const MAX_MANIFESTS = 128;
const MAX_MANIFEST_BYTES = 1024 * 1024;
const MAX_STATE_BYTES = 512 * 1024;
const MAX_SOURCE_VALUE_BYTES = 8192;
const MAX_CREDENTIAL_FILE_BYTES = 64 * 1024;
const MAX_CREDENTIAL_SECRET_BYTES = 4096;
const MAX_FILE_SOURCE_BYTES = 1024 * 1024;
const MAX_DIRECTORY_ENTRIES = 500;
const MAX_DIRECTORY_DEPTH = 8;
const EXTENSION_ID = /^[a-z0-9][a-z0-9._-]{1,63}$/;
const OPERATION_ID = /^[A-Za-z0-9_-]{8,96}$/;
const REVISION_DIGEST = /^sha256:[a-f0-9]{64}$/;
const RAW_DIGEST = /^[a-f0-9]{64}$/;
const BUILTIN_CATALOG_PATH = path.resolve(__dirname, "../distribution/client-extensions/builtin-manifests.json");
const CONTRACT_FILES = Object.freeze({
  manifest: "client_extension_manifest.schema.json",
  document: "client_surface_document.schema.json",
  projection: "projection_read_contract.schema.json",
});
const CLIENT_SURFACE_READ_METHODS = Object.freeze([
  ...(projectionReadContractSchema.$defs?.clientSurfaceProjectionReadMethod?.enum ?? []),
]);
const transactionTails = new Map();
let manifestSemanticsModulePromise = null;
let surfaceSemanticsModulePromise = null;

const validators = new Ajv2020({ allErrors: true, allowUnionTypes: true, strict: true });
validators.addSchema(projectionReadContractSchema);
validators.addSchema(clientSurfaceDocumentSchema);
const validateClientExtensionManifest = validators.compile(clientExtensionManifestSchema);

function createClientExtensionStore(options) {
  const root = path.resolve(requiredAbsolutePath(options?.root, "client extension root"));
  const packagesRoot = path.join(root, "packages");
  const receiptsRoot = path.join(root, "lifecycle-receipts");
  const operationsRoot = path.join(root, "lifecycle-operations");
  const credentialsRoot = path.join(root, "credentials");
  const statePath = path.join(root, "state.json");
  const contractRoot = path.join(root, "contract");
  const contract = {
    root: contractRoot,
    files: Object.fromEntries(Object.entries(CONTRACT_FILES).map(([key, name]) => [key, path.join(contractRoot, name)])),
  };
  let contractPublished = false;
  const onChange = typeof options?.onChange === "function" ? options.onChange : () => {};
  const builtinCatalog = normalizeBuiltinCatalog(
    options?.builtinCatalog ?? readJsonSync(BUILTIN_CATALOG_PATH, "built-in client extension catalog"),
  );
  let generation = 0;
  let watcher = null;
  let debounceTimer = null;
  let lastFingerprint = "";

  async function ensureRoot() {
    await fsPromises.mkdir(packagesRoot, { recursive: true, mode: 0o700 });
    await fsPromises.mkdir(receiptsRoot, { recursive: true, mode: 0o700 });
    await fsPromises.mkdir(operationsRoot, { recursive: true, mode: 0o700 });
    await fsPromises.mkdir(credentialsRoot, { recursive: true, mode: 0o700 });
    for (const candidate of [root, packagesRoot, receiptsRoot, operationsRoot, credentialsRoot]) {
      const stat = await fsPromises.lstat(candidate);
      if (!stat.isDirectory() || stat.isSymbolicLink()) {
        throw new Error(`client extension path must be a real directory: ${candidate}`);
      }
    }
    if (!contractPublished) {
      await publishContract();
      contractPublished = true;
    }
  }

  async function publishContract() {
    await fsPromises.mkdir(contractRoot, { recursive: true, mode: 0o700 });
    const stat = await fsPromises.lstat(contractRoot);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`client extension contract path must be a real directory: ${contractRoot}`);
    for (const name of Object.values(CONTRACT_FILES)) {
      const bytes = await fsPromises.readFile(path.resolve(__dirname, "../schemas", name));
      const destination = path.join(contractRoot, name);
      const current = await fsPromises.readFile(destination).catch((error) => {
        if (error?.code === "ENOENT") return null;
        throw error;
      });
      if (!current || !current.equals(bytes)) await writeBytesAtomic(destination, bytes);
    }
  }

  async function snapshot() {
    return withExtensionStateLock(root, async () => {
      await ensureRoot();
      await recoverInterruptedWrites();
      await initializeState();
      return snapshotUnlocked({ detectExternalChange: true });
    });
  }

  async function snapshotUnlocked({ detectExternalChange = false } = {}) {
    ensureWatcher();
    const data = await readSnapshotData();
    const fingerprint = snapshotFingerprint(data);
    if (detectExternalChange && lastFingerprint && fingerprint !== lastFingerprint) changed();
    lastFingerprint = fingerprint;
    return {
      generation,
      root,
      contract: { root: contract.root, files: { ...contract.files } },
      ...data,
    };
  }

  async function readSnapshotData() {
    const errors = [];
    const manifests = await readManifests(errors);
    const stateResult = await readState();
    if (stateResult.error) errors.push(stateResult.error);
    const lifecycle = await readLifecycle(manifests, errors);
    const credentials = await readCredentialStatus(errors);
    return {
      manifests,
      state: stateResult.state,
      stateStatus: stateResult.status,
      lifecycle,
      credentials,
      errors,
    };
  }

  async function mutate(request) {
    return withExtensionStateLock(root, async () => {
      await ensureRoot();
      await recoverInterruptedWrites();
      await initializeState();
      if (!request || typeof request !== "object" || Array.isArray(request)) {
        throw new Error("Client extension mutation must be an object.");
      }
      if (Object.prototype.hasOwnProperty.call(request, "installed")) {
        throw new Error("Client extension installation requires the receipt-backed lifecycle API.");
      }
      const id = requiredExtensionId(request.id);
      requireExpectedGeneration(request.expectedGeneration);
      const stateResult = await readState();
      if (stateResult.error) throw new Error(stateResult.error);
      const current = stateResult.state[id] ?? {};
      const next = { ...current };
      const reviewed = Object.prototype.hasOwnProperty.call(request, "reviewed")
        ? normalizeRevision(request.reviewed, "Client extension review")
        : null;
      const dismissed = Object.prototype.hasOwnProperty.call(request, "dismissed")
        ? normalizeRevision(request.dismissed, "Client extension dismissal")
        : null;
      if (reviewed && dismissed) throw new Error("Client extension mutation cannot review and dismiss the same revision.");
      const revision = reviewed ?? dismissed;
      if (revision) {
        const currentRevision = await currentRevisionFor(id);
        if (!sameRevision(currentRevision, revision)) {
          throw new Error("Client extension package changed before admission; review the current revision.");
        }
      }
      if (Object.prototype.hasOwnProperty.call(request, "sources")) next.sources = normalizeSources(request.sources);
      if (reviewed) {
        next.reviewed = reviewed;
        delete next.dismissed;
      }
      if (dismissed) {
        next.dismissed = dismissed;
        delete next.reviewed;
      }
      const state = { ...stateResult.state, [id]: next };
      await writeState(state);
      changed();
      return snapshotUnlocked();
    });
  }

  async function applyLifecycle(request) {
    return withExtensionStateLock(root, async () => {
      await ensureRoot();
      await recoverInterruptedWrites();
      await initializeState();
      const normalized = normalizeLifecycleRequest(request);
      requireExpectedGeneration(normalized.expectedGeneration);
      const stateResult = await readState();
      if (stateResult.error) throw new Error(stateResult.error);
      const currentRevision = await currentRevisionFor(normalized.id);
      if (!sameRevision(currentRevision, normalized.expectedRevision)) {
        throw new Error("Client extension revision changed before its lifecycle request was applied.");
      }
      if (normalized.action === "install" && !builtinCatalog.byId.has(normalized.id)) {
        const state = stateResult.state[normalized.id] ?? {};
        if (!sameRevision(state.reviewed, currentRevision)) {
          throw new Error("Client extension revision is unreviewed; confirm its current digest before installing.");
        }
      }
      const desiredState = normalized.action === "install" ? "installed" : "available";
      const requestDigest = lifecycleRequestDigest(normalized.id, desiredState, currentRevision);
      const historicalResult = await readLifecycleOperationReceipt(normalized.id, normalized.operationId);
      if (historicalResult.error) throw new Error(historicalResult.error);
      const existingResult = await readLifecycleReceipt(normalized.id);
      const existing = existingResult.receipt;
      if (historicalResult.receipt) {
        if (historicalResult.receipt.request_digest !== requestDigest) {
          throw new Error(`Client extension lifecycle operation '${normalized.operationId}' already has another request.`);
        }
        const activeMatches = !existingResult.error && existing && stableJson(existing) === stableJson(historicalResult.receipt);
        if (!activeMatches) await writeLifecycleReceipt(historicalResult.receipt);
        if (!activeMatches) changed();
        return {
          changed: !activeMatches,
          receipt: projectLifecycleReceipt(historicalResult.receipt),
          snapshot: await snapshotUnlocked(),
        };
      }
      if (existingResult.error) throw new Error(existingResult.error);
      const requestedReceipt = {
        schema: "octopus.client_extension_lifecycle_receipt",
        schema_version: 1,
        extension_id: normalized.id,
        operation_id: normalized.operationId,
        request_digest: requestDigest,
        desired_state: desiredState,
        source_revision: currentRevision,
        completed_at: new Date().toISOString(),
      };
      await writeLifecycleReceipt(requestedReceipt);
      let receipt;
      try {
        receipt = await writeLifecycleOperationReceipt(requestedReceipt);
      } catch (error) {
        changed();
        throw error;
      }
      if (stableJson(receipt) !== stableJson(requestedReceipt)) await writeLifecycleReceipt(receipt);
      changed();
      return {
        changed: true,
        receipt: projectLifecycleReceipt(receipt),
        snapshot: await snapshotUnlocked(),
      };
    });
  }

  async function withVerifiedLifecycleSnapshot(operation) {
    if (typeof operation !== "function") {
      throw new Error("Client extension lifecycle authorization requires an operation callback.");
    }
    return withExtensionStateLock(root, async () => {
      await ensureRoot();
      await recoverInterruptedWrites();
      await initializeState();
      const snapshot = await snapshotUnlocked({ detectExternalChange: true });
      if (snapshot.stateStatus !== "valid") {
        throw new Error("Client Extension state is degraded; lifecycle-backed actions are unavailable.");
      }
      return operation({
        snapshot,
        installedExtensionIds: Object.entries(snapshot.lifecycle).flatMap(([id, evidence]) => (
          evidence.receiptState === "valid" && evidence.desiredState === "installed" ? [id] : []
        )),
      });
    });
  }

  function requireExpectedGeneration(value) {
    const expectedGeneration = Number(value);
    if (!Number.isSafeInteger(expectedGeneration) || expectedGeneration !== generation) {
      throw new Error(`Client extension catalog changed; expected generation ${expectedGeneration}, current ${generation}.`);
    }
  }

  function changed() {
    generation += 1;
    onChange(generation);
  }

  async function initializeState() {
    const stateResult = await readState();
    if (stateResult.status === "invalid") return;
    await backfillLifecycleOperationHistory();
    if (stateResult.schemaVersion === 2) return;
    const migrationOperationId = stateResult.schemaVersion === 1
      ? "extension_state_migration_v2"
      : "extension_builtin_bootstrap_v1";
    for (const entry of builtinCatalog.extensions) {
      const existing = await readLifecycleReceipt(entry.manifest.id);
      if (existing.error) throw new Error(existing.error);
      if (existing.receipt) continue;
      const legacyChoice = stateResult.legacyInstalled[entry.manifest.id];
      const desiredState = (legacyChoice ?? entry.manifest.default_installed) ? "installed" : "available";
      await writeMigrationReceipt(entry.manifest.id, migrationOperationId, desiredState, entry.revision);
    }
    if (stateResult.schemaVersion === 1) {
      for (const [id, installed] of Object.entries(stateResult.legacyInstalled)) {
        if (id.startsWith("builtin.")) continue;
        const existing = await readLifecycleReceipt(id);
        if (existing.error || existing.receipt) continue;
        let revision;
        try {
          revision = await readCurrentPackageRevision(id);
        } catch {
          continue;
        }
        const reviewed = stateResult.state[id]?.reviewed;
        const desiredState = installed === true && sameRevision(reviewed, revision) ? "installed" : "available";
        await writeMigrationReceipt(id, migrationOperationId, desiredState, revision);
      }
    }
    await writeState(stateResult.state);
  }

  async function recoverInterruptedWrites() {
    for (const directory of [root, receiptsRoot, credentialsRoot]) await recoverInterruptedWritesIn(directory);
    const entries = await fsPromises.readdir(operationsRoot, { withFileTypes: true });
    for (const entry of entries) {
      if (!EXTENSION_ID.test(entry.name)) {
        throw new Error(`client extension lifecycle operation directory '${entry.name}' has an invalid identity`);
      }
      const directory = path.join(operationsRoot, entry.name);
      const stat = await fsPromises.lstat(directory);
      if (!entry.isDirectory() || !stat.isDirectory() || stat.isSymbolicLink()) {
        throw new Error(`client extension lifecycle operation path '${directory}' is not a real directory`);
      }
      await recoverInterruptedWritesIn(directory);
    }
  }

  async function recoverInterruptedWritesIn(directory) {
    const entries = await fsPromises.readdir(directory, { withFileTypes: true });
    let changedDirectory = false;
    for (const entry of entries) {
      if (!/^\.[A-Za-z0-9._-]+\.json-[0-9]+-[0-9a-f-]{36}\.tmp$/.test(entry.name)) continue;
      const file = path.join(directory, entry.name);
      const stat = await fsPromises.lstat(file);
      if (!entry.isFile() || !stat.isFile() || stat.isSymbolicLink()) {
        throw new Error(`client extension interrupted write '${file}' is not a regular file`);
      }
      await fsPromises.unlink(file);
      changedDirectory = true;
    }
    if (changedDirectory) await syncDirectory(directory);
  }

  async function writeMigrationReceipt(id, operationId, desiredState, revision) {
    const requestDigest = lifecycleRequestDigest(id, desiredState, revision);
    const receipt = {
      schema: "octopus.client_extension_lifecycle_receipt",
      schema_version: 1,
      extension_id: id,
      operation_id: operationId,
      request_digest: requestDigest,
      desired_state: desiredState,
      source_revision: revision,
      completed_at: new Date().toISOString(),
    };
    await writeLifecycleReceipt(receipt);
    const durableReceipt = await writeLifecycleOperationReceipt(receipt);
    if (stableJson(durableReceipt) !== stableJson(receipt)) await writeLifecycleReceipt(durableReceipt);
  }

  async function backfillLifecycleOperationHistory() {
    const entries = await fsPromises.readdir(receiptsRoot, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
      const id = entry.name.slice(0, -5);
      if (!EXTENSION_ID.test(id)) continue;
      const active = await readLifecycleReceipt(id);
      if (active.error || !active.receipt) continue;
      const historical = await readLifecycleOperationReceipt(id, active.receipt.operation_id);
      if (historical.error) throw new Error(historical.error);
      if (historical.receipt) {
        if (historical.receipt.request_digest !== active.receipt.request_digest) {
          throw new Error(`client extension '${id}' lifecycle operation history disagrees with its active receipt`);
        }
        if (stableJson(historical.receipt) !== stableJson(active.receipt)) await writeLifecycleReceipt(historical.receipt);
        continue;
      }
      await writeLifecycleOperationReceipt(active.receipt);
    }
  }

  async function readManifests(errors) {
    let entries;
    try {
      entries = await fsPromises.readdir(packagesRoot, { withFileTypes: true });
    } catch (error) {
      errors.push(formatError(error));
      return [];
    }
    const candidates = entries
      .filter((entry) => !entry.name.startsWith("."))
      .sort((left, right) => left.name.localeCompare(right.name));
    if (candidates.length > MAX_MANIFESTS) {
      errors.push(`client extension root contains ${candidates.length} package entries; maximum is ${MAX_MANIFESTS}`);
      return [];
    }
    const directories = candidates.filter((entry) => {
      if (!EXTENSION_ID.test(entry.name)) {
        errors.push(`${entry.name}: extension package directory name is invalid`);
        return false;
      }
      if (!entry.isDirectory()) {
        errors.push(`${entry.name}: extension package must be a real directory`);
        return false;
      }
      return true;
    });
    const manifests = [];
    for (const entry of directories) {
      const packageRoot = path.join(packagesRoot, entry.name);
      const manifestPath = path.join(packageRoot, "extension.json");
      try {
        const packageStat = await fsPromises.lstat(packageRoot);
        if (!packageStat.isDirectory() || packageStat.isSymbolicLink()) throw new Error("extension package must be a real directory");
        const stat = await fsPromises.lstat(manifestPath);
        if (!stat.isFile() || stat.isSymbolicLink()) throw new Error("extension.json must be a regular file");
        if (stat.size > MAX_MANIFEST_BYTES) throw new Error(`extension.json is ${stat.size} bytes; maximum is ${MAX_MANIFEST_BYTES}`);
        const bytes = await stableReadFile(manifestPath, MAX_MANIFEST_BYTES, "extension.json");
        manifests.push({
          path: path.relative(root, manifestPath),
          digest: `sha256:${crypto.createHash("sha256").update(bytes).digest("hex")}`,
          bytes: bytes.byteLength,
          modifiedAt: stat.mtime.toISOString(),
          text: bytes.toString("utf8"),
        });
      } catch (error) {
        errors.push(`${entry.name}/extension.json: ${formatError(error)}`);
      }
    }
    return manifests;
  }

  async function readState() {
    let bytes;
    try {
      bytes = await stableReadFile(statePath, MAX_STATE_BYTES, "client extension state");
    } catch (error) {
      if (error?.code === "ENOENT") {
        return { state: {}, error: "", status: "missing", schemaVersion: 0, legacyInstalled: {} };
      }
      return invalidState(`client extension state: ${formatError(error)}`);
    }
    let value;
    try {
      value = JSON.parse(bytes.toString("utf8"));
    } catch (error) {
      return invalidState(`client extension state is not valid JSON: ${formatError(error)}`);
    }
    try {
      const normalized = normalizeState(value);
      return { ...normalized, error: "", status: "valid" };
    } catch (error) {
      return invalidState(`client extension state is invalid: ${formatError(error)}`);
    }
  }

  function invalidState(error) {
    return { state: {}, error, status: "invalid", schemaVersion: 0, legacyInstalled: {} };
  }

  async function readLifecycle(manifests, errors) {
    const currentRevisions = new Map(builtinCatalog.extensions.map((entry) => [entry.manifest.id, entry.revision]));
    for (const source of manifests) {
      try {
        const manifest = JSON.parse(source.text);
        const packageId = requiredExtensionId(path.basename(path.dirname(source.path)));
        const revision = await validateDownloadedManifest(manifest, packageId, source.digest);
        currentRevisions.set(packageId, revision);
      } catch (error) {
        errors.push(`${source.path}: ${formatError(error)}`);
      }
    }
    const lifecycle = {};
    const entries = await fsPromises.readdir(receiptsRoot, { withFileTypes: true });
    for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
      if (!entry.isFile() || !entry.name.endsWith(".json")) {
        errors.push(`client extension lifecycle receipt '${entry.name}' is not a regular JSON file`);
        continue;
      }
      const id = entry.name.slice(0, -5);
      if (!EXTENSION_ID.test(id)) {
        errors.push(`client extension lifecycle receipt '${entry.name}' has an invalid identity`);
        continue;
      }
      const result = await readLifecycleReceipt(id);
      if (result.error || !result.receipt) {
        const error = result.error || `client extension lifecycle receipt '${id}' is missing`;
        errors.push(error);
        lifecycle[id] = invalidLifecycleEvidence(error);
        continue;
      }
      const revision = currentRevisions.get(id);
      const receiptState = revision && sameRevision(result.receipt.source_revision, revision) ? "valid" : "stale";
      if (receiptState === "stale") errors.push(`client extension '${id}' lifecycle receipt does not match its current revision`);
      lifecycle[id] = projectLifecycleEvidence(result.receipt, receiptState);
    }
    return lifecycle;
  }

  async function readLifecycleReceipt(idValue) {
    const id = requiredExtensionId(idValue);
    const file = path.join(receiptsRoot, `${id}.json`);
    let bytes;
    try {
      bytes = await stableReadFile(file, MAX_STATE_BYTES, `client extension '${id}' lifecycle receipt`);
    } catch (error) {
      if (error?.code === "ENOENT") return { receipt: null, error: "" };
      return { receipt: null, error: `client extension '${id}' lifecycle receipt: ${formatError(error)}` };
    }
    try {
      const receipt = normalizeLifecycleReceipt(JSON.parse(bytes.toString("utf8")));
      if (receipt.extension_id !== id) throw new Error("receipt identity does not match its file name");
      return { receipt, error: "" };
    } catch (error) {
      return { receipt: null, error: `client extension '${id}' lifecycle receipt is invalid: ${formatError(error)}` };
    }
  }

  async function writeLifecycleReceipt(value) {
    const receipt = normalizeLifecycleReceipt(value);
    await writeJsonAtomic(path.join(receiptsRoot, `${receipt.extension_id}.json`), receipt);
  }

  async function readLifecycleOperationReceipt(idValue, operationIdValue) {
    const id = requiredExtensionId(idValue);
    const operationId = requiredOperationId(operationIdValue);
    const file = path.join(operationsRoot, id, `${operationId}.json`);
    let bytes;
    try {
      bytes = await stableReadFile(file, MAX_STATE_BYTES, `client extension '${id}' lifecycle operation '${operationId}'`);
    } catch (error) {
      if (error?.code === "ENOENT") return { receipt: null, error: "" };
      return { receipt: null, error: `client extension '${id}' lifecycle operation '${operationId}': ${formatError(error)}` };
    }
    try {
      const receipt = normalizeLifecycleReceipt(JSON.parse(bytes.toString("utf8")));
      if (receipt.extension_id !== id || receipt.operation_id !== operationId) {
        throw new Error("operation history identity does not match its path");
      }
      return { receipt, error: "" };
    } catch (error) {
      return { receipt: null, error: `client extension '${id}' lifecycle operation '${operationId}' is invalid: ${formatError(error)}` };
    }
  }

  async function writeLifecycleOperationReceipt(value) {
    const receipt = normalizeLifecycleReceipt(value);
    const directory = path.join(operationsRoot, receipt.extension_id);
    await fsPromises.mkdir(directory, { recursive: true, mode: 0o700 });
    const stat = await fsPromises.lstat(directory);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`client extension operation history is not a real directory: ${directory}`);
    const file = path.join(directory, `${receipt.operation_id}.json`);
    const existing = await readLifecycleOperationReceipt(receipt.extension_id, receipt.operation_id);
    if (existing.error) throw new Error(existing.error);
    if (existing.receipt) {
      if (existing.receipt.request_digest !== receipt.request_digest) {
        throw new Error(`Client extension lifecycle operation '${receipt.operation_id}' already has another request.`);
      }
      return existing.receipt;
    }
    await writeJsonCreate(file, receipt);
    return receipt;
  }

  async function currentRevisionFor(id) {
    const builtin = builtinCatalog.byId.get(id);
    return builtin?.revision ?? readCurrentPackageRevision(id);
  }

  async function readCurrentPackageRevision(id) {
    return (await readCurrentPackage(id)).revision;
  }

  async function readCurrentPackage(id) {
    const packageRoot = path.join(packagesRoot, id);
    const packageStat = await fsPromises.lstat(packageRoot);
    if (!packageStat.isDirectory() || packageStat.isSymbolicLink()) throw new Error("Client extension package must be a real directory.");
    const manifestPath = path.join(packageRoot, "extension.json");
    const stat = await fsPromises.lstat(manifestPath);
    if (!stat.isFile() || stat.isSymbolicLink()) throw new Error("Client extension manifest is not a regular file.");
    if (stat.size > MAX_MANIFEST_BYTES) throw new Error(`extension.json is ${stat.size} bytes; maximum is ${MAX_MANIFEST_BYTES}`);
    const bytes = await stableReadFile(manifestPath, MAX_MANIFEST_BYTES, "extension.json");
    let manifest;
    try {
      manifest = JSON.parse(bytes.toString("utf8"));
    } catch (error) {
      throw new Error(`Client extension manifest is not valid JSON: ${formatError(error)}`);
    }
    const revision = await validateDownloadedManifest(
      manifest,
      id,
      `sha256:${crypto.createHash("sha256").update(bytes).digest("hex")}`,
    );
    return { manifest, revision };
  }

  async function credentialSourceFor(idValue, sourceIdValue) {
    const id = requiredExtensionId(idValue);
    const sourceId = requiredExtensionId(sourceIdValue);
    const manifest = builtinCatalog.byId.get(id)?.manifest ?? (await readCurrentPackage(id)).manifest;
    const source = (manifest.sources ?? []).find((candidate) => candidate.id === sourceId);
    if (!source || source.kind !== "credential") {
      throw new Error(`Client extension '${id}' declares no credential source '${sourceId}'.`);
    }
    return { id, sourceId, source };
  }

  async function readCredentialFile(id) {
    const file = path.join(credentialsRoot, `${id}.json`);
    let bytes;
    try {
      bytes = await stableReadFile(file, MAX_CREDENTIAL_FILE_BYTES, `client extension '${id}' credentials`);
    } catch (error) {
      if (error?.code === "ENOENT") return {};
      throw error;
    }
    return normalizeCredentialFile(JSON.parse(bytes.toString("utf8")), id);
  }

  async function readCredentialStatus(errors) {
    let entries;
    try {
      entries = await fsPromises.readdir(credentialsRoot, { withFileTypes: true });
    } catch (error) {
      if (error?.code === "ENOENT") return {};
      errors.push(formatError(error));
      return {};
    }
    const status = {};
    for (const entry of entries) {
      const match = /^([a-z0-9][a-z0-9._-]{1,63})\.json$/.exec(entry.name);
      if (!match) continue;
      try {
        const sources = await readCredentialFile(match[1]);
        status[match[1]] = Object.fromEntries(Object.entries(sources).map(([sourceId, record]) => [
          sourceId,
          { origin: record.origin, updatedAt: record.updated_at },
        ]));
      } catch (error) {
        errors.push(`credentials/${entry.name}: ${formatError(error)}`);
      }
    }
    return status;
  }

  async function setCredential(request) {
    return withExtensionStateLock(root, async () => {
      await ensureRoot();
      await recoverInterruptedWrites();
      if (!request || typeof request !== "object" || Array.isArray(request)) {
        throw new Error("Client extension credential update must be an object.");
      }
      requireExpectedGeneration(request.expectedGeneration);
      const { id, sourceId, source } = await credentialSourceFor(request.id, request.sourceId);
      const sources = await readCredentialFile(id);
      if (request.secret === null) {
        delete sources[sourceId];
      } else {
        sources[sourceId] = {
          origin: source.origin,
          secret: requiredCredentialSecret(request.secret),
          updated_at: new Date().toISOString(),
        };
      }
      const file = path.join(credentialsRoot, `${id}.json`);
      if (Object.keys(sources).length === 0) {
        await fsPromises.unlink(file).catch((error) => {
          if (error?.code !== "ENOENT") throw error;
        });
        await syncDirectory(credentialsRoot);
      } else {
        await writeJsonAtomic(file, {
          schema: "octopus.client_extension_credentials",
          schema_version: 1,
          extension_id: id,
          sources,
        });
      }
      changed();
      return snapshotUnlocked();
    });
  }

  async function credential(idValue, sourceIdValue) {
    return withExtensionStateLock(root, async () => {
      const { id, sourceId, source } = await credentialSourceFor(idValue, sourceIdValue);
      const record = (await readCredentialFile(id))[sourceId];
      if (!record) throw new Error(`${source.label} is not set up.`);
      if (record.origin !== source.origin) {
        throw new Error(`${source.label} was saved for ${record.origin}; save it again for ${source.origin}.`);
      }
      return { origin: source.origin, checkPath: source.check_path ?? "", secret: record.secret };
    });
  }

  async function writeState(state) {
    const bytes = Buffer.from(JSON.stringify({ schema_version: 2, extensions: state }, null, 2) + "\n", "utf8");
    if (bytes.byteLength > MAX_STATE_BYTES) throw new Error(`Client extension state exceeds ${MAX_STATE_BYTES} bytes.`);
    await writeBytesAtomic(statePath, bytes);
  }

  function ensureWatcher() {
    if (watcher) return;
    try {
      watcher = fs.watch(root, { recursive: true }, () => scheduleChange());
      watcher.on("error", () => closeWatcher());
    } catch {
      watcher = null;
    }
  }

  function scheduleChange() {
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      debounceTimer = null;
      void withExtensionStateLock(root, async () => {
        await ensureRoot();
        await recoverInterruptedWrites();
        await initializeState();
        const data = await readSnapshotData();
        const fingerprint = snapshotFingerprint(data);
        if (lastFingerprint && fingerprint === lastFingerprint) return;
        lastFingerprint = fingerprint;
        changed();
      }).catch(() => {
        changed();
      });
    }, 80);
  }

  function closeWatcher() {
    if (watcher) {
      try { watcher.close(); } catch {}
      watcher = null;
    }
  }

  function close() {
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = null;
    closeWatcher();
    return withExtensionStateLock(root, async () => {});
  }

  return {
    root,
    snapshot,
    mutate,
    setCredential,
    credential,
    applyLifecycle,
    withVerifiedLifecycleSnapshot,
    ensureRoot,
    close,
    generation: () => generation,
    currentRevision(id) {
      return withExtensionStateLock(root, async () => {
        await ensureRoot();
        await recoverInterruptedWrites();
        await initializeState();
        return { ...await currentRevisionFor(requiredExtensionId(id)) };
      });
    },
    builtinRevision(id) {
      const entry = builtinCatalog.byId.get(requiredExtensionId(id));
      return entry ? { ...entry.revision } : null;
    },
  };
}

async function readClientExtensionFilesystemSource(request) {
  const sourcePath = requiredAbsolutePath(request?.path, "client extension source path");
  const mode = ["text", "json", "directory"].includes(request?.read) ? request.read : "";
  if (!mode) throw new Error("Client extension filesystem read mode is invalid.");
  const requestedStat = await fsPromises.lstat(sourcePath);
  if (requestedStat.isSymbolicLink()) throw new Error("Client extension source must not be a symbolic link.");
  const realPath = await fsPromises.realpath(sourcePath);
  const stat = await fsPromises.lstat(realPath);
  if (stat.isSymbolicLink()) throw new Error("Client extension source must not be a symbolic link.");
  if (mode === "directory") {
    if (!stat.isDirectory()) throw new Error("Client extension directory source is not a directory.");
    const entries = [];
    const state = { truncated: false };
    await walkDirectory(realPath, "", 0, entries, state);
    return {
      kind: "directory",
      path: realPath,
      name: path.basename(realPath),
      modifiedAt: stat.mtime.toISOString(),
      entries,
      truncated: state.truncated,
      files: entries.filter((entry) => entry.kind === "file").length,
      directories: entries.filter((entry) => entry.kind === "directory").length,
    };
  }
  if (!stat.isFile()) throw new Error("Client extension file source is not a regular file.");
  const bytes = await stableReadFile(realPath, MAX_FILE_SOURCE_BYTES, "client extension file source");
  const text = bytes.toString("utf8");
  if (text.includes("\uFFFD")) throw new Error("Client extension file source is not UTF-8 text.");
  const common = {
    kind: mode,
    path: realPath,
    name: path.basename(realPath),
    size: bytes.byteLength,
    modifiedAt: stat.mtime.toISOString(),
    digest: `sha256:${crypto.createHash("sha256").update(bytes).digest("hex")}`,
  };
  if (mode === "json") {
    let value;
    try { value = JSON.parse(text); } catch (error) {
      throw new Error(`Client extension JSON source is invalid: ${formatError(error)}`);
    }
    return { ...common, value };
  }
  return { ...common, text };
}

async function walkDirectory(root, relativeDirectory, depth, entries, state) {
  if (state.truncated || depth > MAX_DIRECTORY_DEPTH) {
    state.truncated = true;
    return;
  }
  const absolute = path.join(root, relativeDirectory);
  const children = await fsPromises.readdir(absolute, { withFileTypes: true });
  children.sort((left, right) => left.name.localeCompare(right.name));
  for (const child of children) {
    if (entries.length >= MAX_DIRECTORY_ENTRIES) {
      state.truncated = true;
      return;
    }
    const relativePath = path.join(relativeDirectory, child.name);
    const absolutePath = path.join(root, relativePath);
    const stat = await fsPromises.lstat(absolutePath);
    if (stat.isSymbolicLink()) continue;
    if (stat.isDirectory()) {
      entries.push({ kind: "directory", name: child.name, path: relativePath, modifiedAt: stat.mtime.toISOString() });
      await walkDirectory(root, relativePath, depth + 1, entries, state);
      continue;
    }
    if (stat.isFile()) {
      entries.push({ kind: "file", name: child.name, path: relativePath, size: stat.size, modifiedAt: stat.mtime.toISOString() });
    }
  }
}

async function stableReadFile(filePath, maximumBytes, label) {
  const noFollow = typeof fs.constants.O_NOFOLLOW === "number" ? fs.constants.O_NOFOLLOW : 0;
  const handle = await fsPromises.open(filePath, fs.constants.O_RDONLY | noFollow);
  try {
    const before = await handle.stat();
    if (!before.isFile() || before.size > maximumBytes) throw new Error(`${label} exceeds ${maximumBytes} bytes or is not a regular file`);
    const bytes = await handle.readFile();
    const after = await handle.stat();
    if (bytes.byteLength !== before.size || before.size !== after.size || before.mtimeMs !== after.mtimeMs) {
      throw new Error(`${label} changed while it was being read`);
    }
    return bytes;
  } finally {
    await handle.close();
  }
}

function normalizeState(value) {
  if (!value || typeof value !== "object" || Array.isArray(value) || ![1, 2].includes(value.schema_version)) {
    throw new Error("expected schema_version 1 or 2");
  }
  const schemaVersion = value.schema_version;
  if (Object.keys(value).some((key) => key !== "schema_version" && key !== "extensions")) {
    throw new Error("state contains unknown fields");
  }
  const extensions = value.extensions;
  if (!extensions || typeof extensions !== "object" || Array.isArray(extensions)) throw new Error("extensions must be an object");
  const state = {};
  const legacyInstalled = {};
  for (const [id, record] of Object.entries(extensions)) {
    requiredExtensionId(id);
    if (!record || typeof record !== "object" || Array.isArray(record)) throw new Error(`${id} state must be an object`);
    const keys = Object.keys(record);
    if (keys.some((key) => key !== "sources" && key !== "reviewed" && key !== "dismissed" && !(schemaVersion === 1 && key === "installed"))) {
      throw new Error(`${id} state contains unknown fields`);
    }
    const normalized = {};
    if (Object.prototype.hasOwnProperty.call(record, "installed")) {
      if (typeof record.installed !== "boolean") throw new Error(`${id}.installed must be boolean`);
      legacyInstalled[id] = record.installed;
    }
    if (Object.prototype.hasOwnProperty.call(record, "sources")) normalized.sources = normalizeSources(record.sources);
    if (Object.prototype.hasOwnProperty.call(record, "reviewed")) normalized.reviewed = normalizeRevision(record.reviewed, `${id}.reviewed`);
    if (Object.prototype.hasOwnProperty.call(record, "dismissed")) normalized.dismissed = normalizeRevision(record.dismissed, `${id}.dismissed`);
    if (normalized.reviewed && normalized.dismissed) throw new Error(`${id} state cannot review and dismiss the same extension record`);
    state[id] = normalized;
  }
  return { state, schemaVersion, legacyInstalled };
}

function normalizeLifecycleRequest(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Client extension lifecycle request must be an object.");
  }
  const keys = Object.keys(value);
  if (keys.some((key) => !["action", "id", "operationId", "expectedGeneration", "expectedRevision"].includes(key))) {
    throw new Error("Client extension lifecycle request contains unknown fields.");
  }
  if (value.action !== "install" && value.action !== "remove") {
    throw new Error("Client extension lifecycle action must be install or remove.");
  }
  const expectedGeneration = Number(value.expectedGeneration);
  if (!Number.isSafeInteger(expectedGeneration) || expectedGeneration < 0) {
    throw new Error("Client extension lifecycle generation is invalid.");
  }
  return {
    action: value.action,
    id: requiredExtensionId(value.id),
    operationId: requiredOperationId(value.operationId),
    expectedGeneration,
    expectedRevision: normalizeRevision(value.expectedRevision, "Client extension lifecycle revision"),
  };
}

function normalizeLifecycleReceipt(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("receipt must be an object");
  }
  const expected = [
    "schema", "schema_version", "extension_id", "operation_id", "request_digest",
    "desired_state", "source_revision", "completed_at",
  ];
  if (Object.keys(value).length !== expected.length || expected.some((key) => !Object.prototype.hasOwnProperty.call(value, key))) {
    throw new Error("receipt fields do not match the lifecycle contract");
  }
  if (value.schema !== "octopus.client_extension_lifecycle_receipt" || value.schema_version !== 1) {
    throw new Error("receipt has an unsupported schema");
  }
  if (value.desired_state !== "installed" && value.desired_state !== "available") {
    throw new Error("receipt desired state is invalid");
  }
  const completedAt = typeof value.completed_at === "string" ? value.completed_at.trim() : "";
  if (!completedAt || !Number.isFinite(Date.parse(completedAt))) throw new Error("receipt completion time is invalid");
  const receipt = {
    schema: value.schema,
    schema_version: 1,
    extension_id: requiredExtensionId(value.extension_id),
    operation_id: requiredOperationId(value.operation_id),
    request_digest: requiredRawDigest(value.request_digest, "Client extension lifecycle request digest"),
    desired_state: value.desired_state,
    source_revision: normalizeRevision(value.source_revision, "Client extension lifecycle receipt revision"),
    completed_at: completedAt,
  };
  if (receipt.request_digest !== lifecycleRequestDigest(receipt.extension_id, receipt.desired_state, receipt.source_revision)) {
    throw new Error("receipt request digest does not match its lifecycle request");
  }
  return receipt;
}

function normalizeBuiltinCatalog(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("built-in client extension catalog must be an object");
  if (
    value.schema !== "octopus.builtin_client_extension_catalog"
    || value.schema_version !== 1
    || !Array.isArray(value.extensions)
    || Object.keys(value).some((key) => !["schema", "schema_version", "extensions"].includes(key))
  ) {
    throw new Error("built-in client extension catalog has an unsupported contract");
  }
  const byId = new Map();
  const extensions = value.extensions.map((entry, index) => {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) throw new Error(`built-in client extension ${index + 1} must be an object`);
    if (Object.keys(entry).length !== 2 || !("manifest" in entry) || !("source_digest" in entry)) {
      throw new Error(`built-in client extension ${index + 1} has invalid fields`);
    }
    if (!validateClientExtensionManifest(entry.manifest)) {
      throw new Error(`built-in client extension ${index + 1} does not satisfy the manifest contract`);
    }
    const id = requiredExtensionId(entry.manifest.id);
    if (!id.startsWith("builtin.")) throw new Error(`built-in client extension '${id}' must use the builtin. prefix`);
    if (byId.has(id)) throw new Error(`built-in client extension '${id}' is duplicated`);
    const declaredDigest = typeof entry.source_digest === "string" ? entry.source_digest.trim() : "";
    const actualDigest = `sha256:${crypto.createHash("sha256").update(stableJson(entry.manifest)).digest("hex")}`;
    if (!REVISION_DIGEST.test(declaredDigest) || declaredDigest !== actualDigest) {
      throw new Error(`built-in client extension '${id}' source digest is invalid`);
    }
    const normalized = {
      manifest: entry.manifest,
      source_digest: declaredDigest,
      revision: {
        digest: declaredDigest,
        revision: requiredRevisionText(entry.manifest.version, `Built-in client extension '${id}' revision`),
      },
    };
    byId.set(id, normalized);
    return normalized;
  });
  return { extensions, byId };
}

function lifecycleRequestDigest(id, desiredState, revision) {
  return crypto.createHash("sha256").update(stableJson({
    extension_id: id,
    desired_state: desiredState,
    source_revision: revision,
  })).digest("hex");
}

function projectLifecycleReceipt(receipt) {
  return {
    extensionId: receipt.extension_id,
    operationId: receipt.operation_id,
    requestDigest: receipt.request_digest,
    desiredState: receipt.desired_state,
    sourceRevision: { ...receipt.source_revision },
    completedAt: receipt.completed_at,
    receiptDigest: crypto.createHash("sha256").update(stableJson(receipt)).digest("hex"),
  };
}

function projectLifecycleEvidence(receipt, receiptState) {
  return {
    receiptState,
    desiredState: receipt.desired_state,
    operationId: receipt.operation_id,
    requestDigest: receipt.request_digest,
    sourceRevision: { ...receipt.source_revision },
    completedAt: receipt.completed_at,
    receiptDigest: crypto.createHash("sha256").update(stableJson(receipt)).digest("hex"),
    error: "",
  };
}

function invalidLifecycleEvidence(error) {
  return {
    receiptState: "invalid",
    desiredState: "",
    operationId: "",
    requestDigest: "",
    sourceRevision: null,
    completedAt: "",
    receiptDigest: "",
    error,
  };
}

function snapshotFingerprint(data) {
  return crypto.createHash("sha256").update(stableJson({
    manifests: data.manifests.map(({ path: manifestPath, digest, bytes, modifiedAt }) => ({
      path: manifestPath,
      digest,
      bytes,
      modifiedAt,
    })),
    state: data.state,
    stateStatus: data.stateStatus,
    lifecycle: data.lifecycle,
    credentials: data.credentials,
    errors: data.errors,
  })).digest("hex");
}

function normalizeCredentialFile(value, id) {
  if (
    !value || typeof value !== "object" || Array.isArray(value)
    || value.schema !== "octopus.client_extension_credentials"
    || value.schema_version !== 1
    || value.extension_id !== id
    || !value.sources || typeof value.sources !== "object" || Array.isArray(value.sources)
  ) {
    throw new Error("credential file has an unsupported contract");
  }
  const sources = {};
  for (const [sourceId, record] of Object.entries(value.sources)) {
    requiredExtensionId(sourceId);
    if (
      !record || typeof record !== "object" || Array.isArray(record)
      || typeof record.origin !== "string" || !/^https:\/\/[a-z0-9.-]+(?::[0-9]{1,5})?$/.test(record.origin)
      || typeof record.updated_at !== "string"
    ) {
      throw new Error(`credential ${sourceId} is invalid`);
    }
    sources[sourceId] = { origin: record.origin, secret: requiredCredentialSecret(record.secret), updated_at: record.updated_at };
  }
  return sources;
}

function requiredCredentialSecret(value) {
  const secret = typeof value === "string" ? value.trim() : "";
  if (!secret || Buffer.byteLength(secret, "utf8") > MAX_CREDENTIAL_SECRET_BYTES || /[\s\u0000-\u001f\u007f]/.test(secret)) {
    throw new Error("Credential must be a single line without spaces.");
  }
  return secret;
}

function normalizeSources(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Extension sources must be an object.");
  const sources = {};
  for (const [sourceId, configuration] of Object.entries(value)) {
    requiredExtensionId(sourceId);
    if (!configuration || typeof configuration !== "object" || Array.isArray(configuration)) {
      throw new Error(`Extension source ${sourceId} must be an object.`);
    }
    const normalized = {};
    for (const [key, raw] of Object.entries(configuration)) {
      if (!/^[a-z][a-z0-9_]{0,63}$/.test(key) || typeof raw !== "string") {
        throw new Error(`Extension source ${sourceId} contains an invalid field.`);
      }
      if (Buffer.byteLength(raw, "utf8") > MAX_SOURCE_VALUE_BYTES) {
        throw new Error(`Extension source ${sourceId}.${key} exceeds ${MAX_SOURCE_VALUE_BYTES} bytes.`);
      }
      normalized[key] = raw;
    }
    sources[sourceId] = normalized;
  }
  return sources;
}

function normalizeRevision(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object.`);
  const keys = Object.keys(value);
  if (keys.some((key) => key !== "digest" && key !== "revision")) throw new Error(`${label} contains unknown fields.`);
  const digest = typeof value.digest === "string" ? value.digest.trim() : "";
  const revision = typeof value.revision === "string" ? value.revision.trim() : "";
  if (!/^sha256:[a-f0-9]{64}$/.test(digest) || !revision || Buffer.byteLength(revision, "utf8") > 256) {
    throw new Error(`${label} is invalid.`);
  }
  return { digest, revision };
}

function requiredRevisionText(value, label) {
  const revision = typeof value === "string" ? value.trim() : "";
  if (!revision || Buffer.byteLength(revision, "utf8") > 256) throw new Error(`${label} is invalid.`);
  return revision;
}

function requiredOperationId(value) {
  const operationId = typeof value === "string" ? value.trim() : "";
  if (!OPERATION_ID.test(operationId)) throw new Error("Client extension lifecycle operation id is invalid.");
  return operationId;
}

function requiredRawDigest(value, label) {
  const digest = typeof value === "string" ? value.trim() : "";
  if (!RAW_DIGEST.test(digest)) throw new Error(`${label} is invalid.`);
  return digest;
}

function sameRevision(value, revision) {
  return Boolean(value && value.digest === revision.digest && value.revision === revision.revision);
}

function requiredExtensionId(value) {
  const id = typeof value === "string" ? value.trim() : "";
  if (!EXTENSION_ID.test(id)) throw new Error("Client extension id is invalid.");
  return id;
}

function requiredAbsolutePath(value, label) {
  const text = typeof value === "string" ? value.trim() : "";
  if (!text || !path.isAbsolute(text)) throw new Error(`${label} must be an absolute path`);
  return text;
}

function readJsonSync(file, label) {
  let text;
  try {
    text = fs.readFileSync(file, "utf8");
  } catch (error) {
    throw new Error(`${label} cannot be read: ${formatError(error)}`);
  }
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new Error(`${label} is not valid JSON: ${formatError(error)}`);
  }
}

async function clientExtensionManifestSemanticIssues(manifest, options) {
  manifestSemanticsModulePromise ??= import("./clientExtensionManifestSemantics.mjs");
  const module = await manifestSemanticsModulePromise;
  return module.clientExtensionManifestSemanticIssues(manifest, options);
}

async function clientSurfaceDocumentSemanticInspection(document, options) {
  surfaceSemanticsModulePromise ??= import("./clientSurfaceDocumentSemantics.mjs");
  const module = await surfaceSemanticsModulePromise;
  return module.clientSurfaceDocumentSemanticInspection(document, options);
}

async function validateDownloadedManifest(manifest, packageId, digest) {
  if (!validateClientExtensionManifest(manifest)) {
    const detail = [...new Set((validateClientExtensionManifest.errors ?? [])
      .map((error) => `${error.instancePath || "/"} ${error.message || "is invalid"}`))]
      .sort((left, right) => right.split(" ")[0].split("/").length - left.split(" ")[0].split("/").length)
      .slice(0, 8).join("; ");
    throw new Error(`Client extension manifest does not satisfy the client extension contract: ${detail}`);
  }
  if (!manifest || typeof manifest !== "object" || Array.isArray(manifest) || manifest.id !== packageId) {
    throw new Error("Client extension manifest identity does not match its package directory.");
  }
  const issues = await clientExtensionManifestSemanticIssues(manifest, { compiled: false });
  if (manifest.contribution.kind === "surface") {
    const inspection = await clientSurfaceDocumentSemanticInspection(manifest.contribution.document, {
      compiled: false,
      readMethods: CLIENT_SURFACE_READ_METHODS,
    });
    issues.push(...inspection.issues.map((issue) => ({
      ...issue,
      code: `surface.${issue.code}`,
      path: `/contribution/document${issue.path}`,
    })));
  }
  if (issues.length > 0) {
    const detail = issues.map((issue) => `${issue.path}: ${issue.message}`).join("; ");
    throw new Error(`Client extension manifest failed semantic admission: ${detail}`);
  }
  return normalizeRevision({ digest, revision: manifest.version }, "Client extension manifest revision");
}

async function writeJsonAtomic(file, value) {
  await writeBytesAtomic(file, Buffer.from(`${JSON.stringify(value, null, 2)}\n`, "utf8"));
}

async function writeJsonCreate(file, value) {
  const bytes = Buffer.from(`${JSON.stringify(value, null, 2)}\n`, "utf8");
  const directory = path.dirname(file);
  const temporary = path.join(directory, `.${path.basename(file)}-${process.pid}-${crypto.randomUUID()}.tmp`);
  const handle = await fsPromises.open(temporary, "wx", 0o600);
  try {
    await handle.writeFile(bytes);
    await handle.sync();
  } finally {
    await handle.close();
  }
  try {
    await fsPromises.link(temporary, file);
    await fsPromises.unlink(temporary);
    await syncDirectory(directory);
  } catch (error) {
    await fsPromises.unlink(temporary).catch(() => undefined);
    throw error;
  }
}

async function writeBytesAtomic(file, bytes) {
  const directory = path.dirname(file);
  const temporary = path.join(directory, `.${path.basename(file)}-${process.pid}-${crypto.randomUUID()}.tmp`);
  const handle = await fsPromises.open(temporary, "wx", 0o600);
  try {
    await handle.writeFile(bytes);
    await handle.sync();
  } finally {
    await handle.close();
  }
  try {
    await fsPromises.rename(temporary, file);
    await syncDirectory(directory);
  } catch (error) {
    await fsPromises.unlink(temporary).catch(() => undefined);
    throw error;
  }
}

async function syncDirectory(directory) {
  const handle = await fsPromises.open(directory, "r");
  try {
    await handle.sync();
  } finally {
    await handle.close();
  }
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function withExtensionStateLock(root, operation) {
  const key = path.resolve(root);
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

function formatError(error) {
  return error instanceof Error ? error.message : String(error);
}

module.exports = {
  createClientExtensionStore,
  readClientExtensionFilesystemSource,
};
