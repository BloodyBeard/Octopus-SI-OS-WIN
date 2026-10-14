const crypto = require("node:crypto");
const fsSync = require("node:fs");
const fs = require("node:fs/promises");
const path = require("node:path");
const Ajv = require("ajv/dist/2020");
const packageManifestSchema = require("../schemas/local_package_manifest.schema.json");
const applicationPackSchema = require("../schemas/local_application_pack.schema.json");
const packageReceiptSchema = require("../schemas/local_package_receipt.schema.json");
const packageTransactionSchema = require("../schemas/local_package_transaction.schema.json");

const validators = new Ajv({ allErrors: true, strict: false, logger: false });
validators.addSchema(packageReceiptSchema);
const validatePackageManifest = validators.compile(packageManifestSchema);
const validateApplicationPack = validators.compile(applicationPackSchema);
const validatePackageReceipt = validators.compile(packageReceiptSchema);
const validatePackageTransaction = validators.compile(packageTransactionSchema);

const SAFE_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const SAFE_OPERATION_ID = /^[A-Za-z0-9_-]{8,96}$/;
const PREPARED_TRANSACTION_ROOT = /^\.prepare\.([A-Za-z0-9_-]{8,96})\.[a-f0-9]{12}$/;
const CLEANUP_TRANSACTION_ROOT = /^\.cleanup\.([A-Za-z0-9_-]{8,96})\.[a-f0-9]{12}$/;
const DIGEST = /^[a-f0-9]{64}$/;
const MAX_CONFIGURATION_BYTES = 2 * 1024 * 1024;
const MAX_RESOLUTION_BYTES = 16 * 1024 * 1024;
const INTERFACE_IMPORT_PREFIX = "interface://";
const PACKAGE_KINDS = new Set(["application", "driver", "artifact"]);
const REGISTRY_KINDS = new Set([
  "interface",
  "worker_environment_kind",
  "tool",
  "artifact",
  "mark",
  "verifier",
  "ee_template",
]);
const SOURCE_EXCLUDES = new Set([".DS_Store", ".git", "__pycache__", "node_modules", "target"]);
const sourceSnapshotCache = new Map();
const configurationValidators = new Map();
const packageMutationGuards = new Map();

async function readLocalPackageCatalog(options) {
  const roots = normalizeRoots(options);
  return withPackageStateRead(roots, async () => {
    const library = await loadLocalPackageLibrary(roots);
    const packages = [];
    for (const entry of library.packages) {
      try {
        const source = await snapshotPackageSource(roots, entry.manifest);
        const state = await readPackageState(roots, entry.manifest, source);
        packages.push(packageCatalogRecord(entry, source, state));
      } catch (error) {
        packages.push({
          ...entry.manifest,
          manifest_digest: entry.manifestDigest,
          available: false,
          installed: false,
          materialized: false,
          partially_materialized: false,
          receipt_state: "invalid",
          source_digest: "",
          installed_digest: "",
          source_file_count: 0,
          error: errorMessage(error),
        });
      }
    }
    const packageById = new Map(packages.map((entry) => [entry.id, entry]));
    for (const entry of packages) {
      try {
        entry.install_packages = packageDependencyOrder(library, [entry.id]).map(({ manifest }) => {
          const record = packageById.get(manifest.id);
          return { id: manifest.id, name: manifest.display.name, source_digest: record?.source_digest ?? "", installed: record?.installed === true, receipt_state: record?.receipt_state ?? "missing" };
        });
      } catch (error) {
        entry.available = false;
        entry.error = errorMessage(error);
      }
    }
    const packs = library.packs.map((entry) => packCatalogRecord(entry, packageById));
    return { packages, packs, errors: library.errors };
  });
}

async function readLocalPackageSourcePackage(options) {
  const sourceRegistryRoot = path.resolve(requiredAbsolutePath(options?.sourceRegistryRoot, "sourceRegistryRoot"));
  const libraryRoot = path.resolve(requiredAbsolutePath(options?.libraryRoot, "libraryRoot"));
  await requirePlainDirectory(sourceRegistryRoot, sourceRegistryRoot);
  await requirePlainDirectory(libraryRoot, libraryRoot);
  const library = await loadLocalPackageLibrary({ sourceRegistryRoot, libraryRoot });
  const packageId = requiredId(options?.packageId, "packageId");
  const entry = requireLibraryPackage(library, packageId);
  const source = await snapshotPackageSource({ sourceRegistryRoot, libraryRoot }, entry.manifest, { fresh: true });
  return {
    manifest: structuredClone(entry.manifest),
    manifest_path: entry.file,
    manifest_digest: source.manifestDigest,
    source_digest: source.sourceDigest,
    source_file_count: source.fileCount,
    members: source.members.map((member) => ({
      registry_kind: member.registry_kind,
      owner: member.owner,
      primary: member.primary === true,
      source_directory: member.source_directory,
      digest: member.digest,
      files: member.files.map((file) => ({ ...file })),
    })),
  };
}

async function readLocalPackageInstallation(options) {
  return withLocalPackageInstallation(options, (installation) => installation);
}

async function inspectLocalPackageInstallation(options) {
  const roots = normalizeRoots(options);
  await requirePlainDirectory(roots.sourceRegistryRoot, roots.sourceRegistryRoot);
  await requirePlainDirectory(roots.activeRegistryRoot, roots.activeRegistryRoot);
  await requirePlainDirectory(roots.libraryRoot, roots.libraryRoot);
  if (await pathExists(roots.stateRoot)) {
    await requirePlainDirectory(roots.stateRoot, roots.stateRoot);
  }
  if (await pathExists(roots.receiptsRoot)) {
    await requirePlainDirectory(roots.receiptsRoot, roots.receiptsRoot);
  }
  const library = await loadLocalPackageLibrary(roots);
  const packageId = requiredId(options?.packageId, "packageId");
  const entry = requireLibraryPackage(library, packageId);
  const source = await snapshotPackageSource(roots, entry.manifest, { fresh: true });
  const state = await readPackageState(roots, entry.manifest, source);
  return {
    ...packageCatalogRecord(entry, source, state),
    ...(options.includeFileSnapshot === true ? { file_snapshot: packageMemberFileSnapshot(source, state) } : {}),
  };
}

async function inspectLocalPackageCatalog(options) {
  const roots = normalizeRoots(options);
  await requirePlainDirectory(roots.sourceRegistryRoot, roots.sourceRegistryRoot);
  await requirePlainDirectory(roots.activeRegistryRoot, roots.activeRegistryRoot);
  await requirePlainDirectory(roots.libraryRoot, roots.libraryRoot);
  if (await pathExists(roots.stateRoot)) {
    await requirePlainDirectory(roots.stateRoot, roots.stateRoot);
  }
  if (await pathExists(roots.receiptsRoot)) {
    await requirePlainDirectory(roots.receiptsRoot, roots.receiptsRoot);
  }
  const library = await loadLocalPackageLibrary(roots);
  const packages = [];
  for (const entry of library.packages) {
    try {
      const source = await snapshotPackageSource(roots, entry.manifest, { fresh: true });
      const state = await readPackageState(roots, entry.manifest, source);
      packages.push(packageCatalogRecord(entry, source, state));
    } catch (error) {
      packages.push({
        ...entry.manifest,
        manifest_path: entry.file,
        manifest_digest: entry.manifestDigest,
        available: false,
        member_states: [],
        error: errorMessage(error),
      });
    }
  }
  return { packages, errors: library.errors };
}

async function inspectLocalPackageDiagnostics(options) {
  const roots = normalizeRoots(options);
  for (const root of [roots.sourceRegistryRoot, roots.activeRegistryRoot, roots.libraryRoot]) {
    await requirePlainDirectory(root, root);
  }
  for (const root of [roots.stateRoot, roots.receiptsRoot]) {
    if (await pathExists(root)) await requirePlainDirectory(root, root);
  }
  const library = await loadLocalPackageLibrary(roots);
  const entry = requireLibraryPackage(library, requiredId(options.packageId, "packageId"));
  const source = await snapshotPackageSource(roots, entry.manifest, { fresh: true });
  let receipt = null, receiptState = "missing";
  try {
    receipt = await readReceipt(roots, entry.manifest.id);
    if (receipt) receiptState = "present";
  } catch {
    receiptState = "invalid";
  }
  const includePaths = options.includePaths === true;
  const members = [];
  for (const member of source.members) {
    const baseline = receipt?.members.find((candidate) => memberKey(candidate) === memberKey(member));
    const report = {
      index: members.length,
      ...(includePaths ? { registry_kind: member.registry_kind, owner: member.owner } : {}),
      source_matches_receipt: baseline ? member.digest === (baseline.source_files ? snapshotDigest(receiptSourceFiles(baseline)) : baseline.digest) : null,
      active_matches_source: null,
      active_matches_receipt: null,
      state: "missing",
      files: [],
      omitted_files: 0,
      active_files_not_in_source: 0,
    };
    members.push(report);
    let active;
    try {
      const target = memberPath(roots.activeRegistryRoot, member);
      if (!await pathExists(target)) continue;
      await requirePlainDirectory(path.dirname(target), path.dirname(target));
      active = await snapshotDirectory(target);
    } catch {
      report.state = "unreadable";
      continue;
    }
    report.state = "present";
    report.active_matches_source = active.digest === member.digest;
    report.active_matches_receipt = baseline ? active.digest === baseline.digest : null;
    const sourcePaths = new Set(member.files.map((file) => snapshotRelativePath(file.relative_path)));
    report.active_files_not_in_source = active.files.filter((file) => !sourcePaths.has(snapshotRelativePath(file.relative_path))).length;
    const rows = [];
    for (const [index, file] of member.files.entries()) {
      const relative = snapshotRelativePath(file.relative_path);
      const current = snapshotFile(active, relative);
      const saved = baseline?.json_files?.find((candidate) => candidate.path === relative);
      const savedSource = baseline?.source_files?.find(candidate => candidate.path === relative) ?? (baseline?.source_files ? null : saved);
      const row = {
        index,
        ...(includePaths ? { path: relative } : {}),
        state: !current ? "missing" : current.sha256 === file.sha256 && current.mode === file.mode ? "matches_source" : "modified",
        source_matches_receipt: savedSource ? file.sha256 === savedSource.sha256 && file.size === savedSource.size
          && (savedSource.mode === undefined || file.mode === savedSource.mode) : null,
        active_matches_receipt: saved && current ? current.sha256 === saved.sha256 : null,
      };
      const configuration = member.configuration?.find((candidate) => candidate.path === relative);
      if (configuration && current) {
        try {
          const projection = await projectMemberConfiguration(active, [configuration]);
          const actual = projection.projectedFiles.get(relative), expected = member.projectedFiles.get(relative);
          row.configuration = {
            state: "valid",
            owned_matches_source: actual.owned_sha256 === expected.owned_sha256,
            owned_matches_receipt: saved?.configuration_digest === actual.configuration_digest
              ? actual.owned_sha256 === saved.owned_sha256 : null,
            fields: actual.fields.map((field, fieldIndex) => ({
              index: fieldIndex,
              ...(includePaths ? { pointer: field.pointer } : {}),
              present: field.present,
              matches_source: stableJson(field) === stableJson(expected.fields[fieldIndex]),
            })),
          };
        } catch {
          row.configuration = { state: "invalid_or_changed_during_read" };
        }
      }
      rows.push(row);
    }
    rows.sort((left, right) => Number(left.state === "matches_source") - Number(right.state === "matches_source") || left.index - right.index);
    report.files = rows.slice(0, 256);
    report.omitted_files = Math.max(0, rows.length - report.files.length);
  }
  return {
    ...(includePaths ? { package_id: entry.manifest.id, package_version: entry.manifest.version } : {}),
    product_kind: entry.manifest.product_kind,
    source_definition: entry.source,
    source_digest: source.sourceDigest,
    manifest_digest: source.manifestDigest,
    receipt: {
      state: receiptState,
      source_matches_receipt: receipt ? source.sourceDigest === receipt.source_digest : null,
      manifest_matches_receipt: receipt ? source.manifestDigest === receipt.manifest_digest : null,
      comparison: receipt?.schema_version === 2
        ? "Source comparisons use source_files (bytes and modes). Active receipt comparisons use materialized owner digests and JSON bytes; local extras are not source-owned."
        : "Last install/repair receipt, not edit history. Per-file receipt comparisons cover JSON bytes; member comparisons also cover modes.",
    },
    catalog_issue_count: library.errors.length,
    members,
  };
}

async function withLocalPackageInstallation(options, operation) {
  if (typeof operation !== "function") throw new Error("local package installation reader requires an operation");
  const roots = normalizeRoots(options);
  return withPackageStateRead(roots, async () => {
    const library = await loadLocalPackageLibrary(roots);
    const packageId = requiredId(options?.packageId, "packageId");
    const entry = requireLibraryPackage(library, packageId);
    const source = await snapshotPackageSource(roots, entry.manifest, { fresh: true });
    const state = await readPackageState(roots, entry.manifest, source);
    return operation(packageCatalogRecord(entry, source, state), packageMemberFileSnapshot(source, state));
  });
}

function packageMemberFileSnapshot(source, state) {
  return { members: source.members.map((member) => ({
    registry_kind: member.registry_kind,
    owner: member.owner,
    source_files: member.files,
    installed_files: state.activeMembers.find((active) => memberKey(active) === memberKey(member))?.files ?? [],
  })) };
}

async function installLocalPackages(options) {
  const roots = normalizeRoots(options);
  return withPackageStateLock(roots.stateRoot, async () => {
    await ensurePackageState(roots);
    await recoverLocalPackageTransactionsUnlocked(roots);
    const operationId = requiredOperationId(options?.operationId);
    const library = await loadLocalPackageLibrary(roots);
    const packageIds = requiredUniqueIds(options?.packageIds, "packageIds");
    const expected = normalizeDigestMap(options?.expectedSourceDigests, packageIds, "expectedSourceDigests");
    const selected = requireReviewedPackageClosure(library, packageIds);
    return installResolvedPackages(roots, selected, {
      operationId,
      expected,
      assertMutable: options?.assertMutable,
      packId: optionalId(options?.packId, "packId"),
      onProgress: options?.onProgress,
    });
  });
}

async function removeLocalPackages(options) {
  const roots = normalizeRoots(options);
  return withPackageStateLock(roots.stateRoot, async () => {
    await ensurePackageState(roots);
    await recoverLocalPackageTransactionsUnlocked(roots);
    const operationId = requiredOperationId(options?.operationId);
    const library = await loadLocalPackageLibrary(roots);
    const packageIds = requiredUniqueIds(options?.packageIds, "packageIds");
    await assertPackageMutationAllowed(roots, options?.assertMutable, packageIds);
    const expected = normalizeDigestMap(options?.expectedInstalledDigests, packageIds, "expectedInstalledDigests");
    const selected = packageIds.map((packageId) => requireLibraryPackage(library, packageId));
    await assertNoMaterializedPackageDependents(roots, library, packageIds);
    if (typeof options?.assertRemovable === "function") {
      for (const entry of selected) await options.assertRemovable(entry.manifest);
    }
    return removeResolvedPackages(roots, packageDependencyOrder(library, packageIds).filter((entry) => packageIds.includes(entry.manifest.id)).reverse(), {
      operationId,
      expected,
      onProgress: options?.onProgress,
    });
  });
}

async function repairLocalPackages(options) {
  const roots = normalizeRoots(options);
  return withPackageStateLock(roots.stateRoot, async () => {
    await ensurePackageState(roots);
    await recoverLocalPackageTransactionsUnlocked(roots);
    const operationId = requiredOperationId(options?.operationId);
    const library = await loadLocalPackageLibrary(roots);
    const packageIds = requiredUniqueIds(options?.packageIds, "packageIds");
    await assertPackageMutationAllowed(roots, options?.assertMutable, packageIds);
    const expectedRepairs = normalizeRepairExpectations(options?.expectedRepairs, packageIds);
    const selected = packageDependencyOrder(library, packageIds).filter((entry) => packageIds.includes(entry.manifest.id));
    for (const dependency of packageDependencyOrder(library, packageIds)) {
      if (packageIds.includes(dependency.manifest.id)) continue;
      const source = await snapshotPackageSource(roots, dependency.manifest, { fresh: true });
      const state = await readPackageState(roots, dependency.manifest, source);
      if (!state.materialized || state.receiptState !== "valid") throw new Error(`Repair requires installed dependency '${dependency.manifest.id}'`);
    }
    return repairResolvedPackages(roots, selected, {
      operationId,
      expectedRepairs,
      onProgress: options?.onProgress,
    });
  });
}

async function adoptLocalPackageUpdates(options) {
  const roots = normalizeRoots(options);
  const resolutions = normalizePackageConflictResolutions(options?.conflictResolutions);
  const assertMutable = options?.assertMutable, onProgress = options?.onProgress;
  if (resolutions.size && typeof assertMutable !== "function") {
    throw new Error("Package conflict resolutions require an explicit mutation guard");
  }
  return withPackageStateLock(roots.stateRoot, async () => {
    if (resolutions.size) await assertPackageMutationAllowed(roots, assertMutable, [...new Set([...resolutions.values()].map(row => row.packageId))]);
    await ensurePackageState(roots);
    await recoverLocalPackageTransactionsUnlocked(roots);
    const receipts = new Map();
    for (const entry of await fs.readdir(roots.receiptsRoot, { withFileTypes: true })) {
      if (!entry.name.endsWith(".json")) continue;
      if (!entry.isFile()) throw new Error(`Installed package receipt '${entry.name}' is not a regular file`);
      const id = requiredId(entry.name.slice(0, -5), "installed package id");
      const receipt = await readReceipt(roots, id);
      if (!receipt) throw new Error(`Installed package receipt '${id}' disappeared before update`);
      receipts.set(id, receipt);
    }
    if (receipts.size === 0) {
      assertConflictResolutionsUsed(resolutions, new Set());
      return { action: "adopt_package_updates", changed: false, packageIds: [], receiptUpgrades: [] };
    }
    const library = await loadLocalPackageLibrary(roots);
    const selected = requireReviewedPackageClosure(library, [...receipts.keys()].sort());
    const updates = [];
    const receiptUpgrades = [];
    const localChanges = [];
    const expectedRepairs = {};
    const usedResolutions = new Set();
    for (const entry of selected) {
      const source = await snapshotPackageSource(roots, entry.manifest, { fresh: true });
      await verifyPackageDefinition(entry);
      const state = await readPackageState(roots, entry.manifest, source);
      const receipt = receipts.get(entry.manifest.id);
      if (!state.receipt || hashJson(state.receipt) !== hashJson(receipt)) {
        throw new Error(`Installed package '${entry.manifest.id}' receipt changed before update`);
      }
      if (receipt.source_digest === source.sourceDigest && receipt.manifest_digest === source.manifestDigest) {
        if (state.receiptState !== "valid" || state.localChanges.length) {
          localChanges.push({ packageId: entry.manifest.id, receiptState: state.receiptState,
            materialized: state.materialized, installedDigest: state.installedDigest,
            receiptDigest: hashJson(receipt), files: state.localChanges });
          reportProgress(onProgress, { phase: "preserving_local_changes", packageId: entry.manifest.id, percent: 0 });
        }
        if (receipt.schema_version !== 1 || entry.manifest.release_status === "system"
          || sourceBaselineIssue(entry.manifest, source, state) !== null) continue;
        receiptUpgrades.push(entry.manifest.id);
      } else {
        if (state.receiptState === "valid") continue;
        await preparePackageUpdate(roots, entry.manifest, source, state, resolutions, usedResolutions);
        if (state.localChanges.length) localChanges.push({ packageId: entry.manifest.id, files: state.localChanges });
      }
      updates.push(entry);
      expectedRepairs[entry.manifest.id] = {
        sourceDigest: source.sourceDigest,
        installedDigest: state.installedDigest,
        receiptState: state.receiptState,
        receiptDigest: hashJson(receipt),
      };
    }
    const packageIds = updates.map(entry => entry.manifest.id);
    assertConflictResolutionsUsed(resolutions, usedResolutions);
    if (!updates.length) return { action: "adopt_package_updates", changed: false, packageIds, localChanges, receiptUpgrades };
    await assertPackageMutationAllowed(roots, assertMutable, packageIds);
    const identity = resolutions.size ? { expectedRepairs,
      resolutions: [...resolutions.values()].map(({ body, ...proof }) => proof) } : expectedRepairs;
    const outcome = await repairResolvedPackages(roots, updates, {
      operationId: `release_${hashJson(identity)}`,
      expectedRepairs,
      prepareMaterialization: (manifest, source, state) => receiptUpgrades.includes(manifest.id)
        ? prepareSourceBaseline(manifest, source, state) : preparePackageUpdate(roots, manifest, source, state, resolutions),
      allowOwnerAdditions: true,
      assertMutable,
      onProgress,
    });
    return { ...outcome, action: "adopt_package_updates", localChanges, receiptUpgrades };
  });
}

async function prepareLocalPackageUpdateBaselines(options) {
  const roots = normalizeRoots(options);
  const packageIds = requiredUniqueIds(options?.packageIds, "packageIds");
  const operationId = requiredOperationId(options?.operationId);
  const expectedSources = normalizeDigestMap(options?.expectedSourceDigests, packageIds, "expectedSourceDigests");
  const expectedReceipts = normalizeDigestMap(options?.expectedReceiptDigests, packageIds, "expectedReceiptDigests");
  if (typeof options?.assertMutable !== "function") throw new Error("Source baseline preparation requires an explicit stopped-Home mutation guard");
  return withPackageStateLock(roots.stateRoot, async () => {
    await assertPackageMutationAllowed(roots, options.assertMutable, packageIds);
    await ensurePackageState(roots);
    await recoverLocalPackageTransactionsUnlocked(roots);
    const library = await loadLocalPackageLibrary(roots);
    const pending = [], expectedRepairs = {};
    for (const packageId of packageIds) {
      const entry = requireLibraryPackage(library, packageId);
      const source = await snapshotPackageSource(roots, entry.manifest, { fresh: true });
      await verifyPackageDefinition(entry);
      const state = await readPackageState(roots, entry.manifest, source);
      if (source.sourceDigest !== expectedSources[packageId]) throw new Error(`Source baseline '${packageId}' changed before preparation`);
      prepareSourceBaseline(entry.manifest, source, state);
      if (state.receipt.schema_version === 2 && state.receipt.operation_id === operationId) continue;
      if (hashJson(state.receipt) !== expectedReceipts[packageId]) throw new Error(`Source baseline '${packageId}' receipt changed before preparation`);
      if (state.receipt.schema_version === 2) continue;
      pending.push(entry);
      expectedRepairs[packageId] = { sourceDigest: source.sourceDigest, installedDigest: state.installedDigest,
        receiptState: state.receiptState, receiptDigest: hashJson(state.receipt) };
    }
    if (!pending.length) return { action: "prepare_package_update_baselines", changed: false, packageIds };
    const outcome = await repairResolvedPackages(roots, pending, { operationId, expectedRepairs,
      prepareMaterialization: prepareSourceBaseline, assertMutable: options.assertMutable, onProgress: options.onProgress });
    return { ...outcome, action: "prepare_package_update_baselines" };
  });
}

function prepareSourceBaseline(manifest, source, state) {
  const issue = sourceBaselineIssue(manifest, source, state);
  if (issue !== null) throw new Error(issue);
  return { ...source, members: state.activeMembers };
}

function sourceBaselineIssue(manifest, source, state) {
  const receipt = state.receipt;
  if (!receipt || !state.materialized || state.partiallyMaterialized
    || receipt.source_digest !== source.sourceDigest || receipt.manifest_digest !== source.manifestDigest
    || receipt.product_kind !== manifest.product_kind || receipt.product_id !== manifest.product_id
    || receipt.members.length !== source.members.length) {
    return `Source baseline '${manifest.id}' requires the exact previously receipted source and manifest`;
  }
  for (const member of source.members) {
    const baseline = receipt.members.find(candidate => memberKey(candidate) === memberKey(member));
    const digest = baseline?.source_files ? snapshotDigest(receiptSourceFiles(baseline)) : baseline?.digest;
    if (baseline?.target !== memberRelative(member) || digest !== member.digest) {
      return `Source baseline '${manifest.id}' cannot prove previous owner '${memberKey(member)}'`;
    }
  }
  return null;
}

function assertPackageUpdateOwnership(manifest, source, state) {
  const receipt = state.receipt;
  if (!receipt || receipt.product_kind !== manifest.product_kind || receipt.product_id !== manifest.product_id) {
    throw new Error(`Installed package '${manifest.id}' changed its declared owner set; prior installation is preserved`);
  }
  for (const baseline of receipt.members) {
    const member = source.members.find(candidate => memberKey(candidate) === memberKey(baseline));
    const active = state.activeMembers.find(candidate => memberKey(candidate) === memberKey(baseline));
    if (!member || baseline.target !== memberRelative(member) || !active?.present) {
      throw new Error(`Installed package '${manifest.id}' changed ownership of '${memberKey(baseline)}'; prior owners must remain declared and present`);
    }
    const jsonFiles = new Map((baseline.source_files ?? baseline.json_files ?? []).map(file => [file.path, file]));
    const evolution = configurationEvolution(member, baseline);
    for (const [relative, saved] of jsonFiles) {
      if ((baseline.source_files || saved.configuration_digest !== undefined)
        && member.projectedFiles.get(relative)?.configuration_digest !== saved.configuration_digest && !evolution.has(relative)) {
        throw new Error(`Installed package '${manifest.id}' changed configuration ownership at '${memberKey(member)}/${relative}'; prior installation is preserved`);
      }
    }
    if (baseline.source_files) continue;
    const files = active.files.map(file => {
      const relative = snapshotRelativePath(file.relative_path);
      const saved = jsonFiles.get(relative);
      if (!saved || saved.sha256 === file.sha256 && saved.size === file.size) return file;
      const configured = active.projectedFiles.get(relative);
      if (!configured || configured.configuration_digest !== saved.configuration_digest
        || configured.owned_sha256 !== saved.owned_sha256) {
        throw new Error(`Installed package '${manifest.id}' has unowned JSON changes at '${memberKey(member)}/${relative}' without a per-file source baseline; prior installation is preserved`);
      }
      return { ...file, sha256: saved.sha256, size: saved.size };
    });
    if (snapshotDigest(files) !== baseline.digest) {
      throw new Error(`Installed package '${manifest.id}' has changed source-owned files at '${memberKey(member)}' without a per-file source baseline; prior installation is preserved`);
    }
  }
  const prior = new Set(receipt.members.map(memberKey));
  for (const member of source.members) {
    if (!prior.has(memberKey(member)) && state.activeMembers.find(candidate => memberKey(candidate) === memberKey(member))?.present) {
      throw new Error(`Installed package '${manifest.id}' cannot claim an existing unowned target '${memberKey(member)}'`);
    }
  }
}

function assertAdditiveConfiguration(prior, next, label) {
  const fields = new Map((next?.fields ?? []).map(field => [field.pointer, field]));
  if (!next || prior.fields.some(field => stableJson(fields.get(field.pointer)) !== stableJson(field))) {
    throw new Error(`${label} changed configuration ownership; prior pointers and schemas must remain unchanged`);
  }
}

function configurationEvolution(member, baseline) {
  const evolution = new Map();
  for (const saved of baseline?.source_files ?? baseline?.json_files ?? []) {
    const next = member.configuration?.find(entry => entry.path === saved.path);
    if (next && hashJson(next) === saved.configuration_digest || !next && saved.configuration_digest === undefined) continue;
    const label = `Package owner '${memberKey(member)}/${saved.path}'`;
    if (!baseline.source_files) throw new Error(`${label} changed configuration ownership without a per-file source baseline`);
    const prior = saved.configuration_digest === undefined ? { path: saved.path, fields: [] }
      : [...(baseline.configuration ?? []), ...(member.configuration_predecessors ?? [])]
        .find(entry => entry.path === saved.path && hashJson(entry) === saved.configuration_digest);
    if (!prior) throw new Error(`${label} changed configuration ownership without an exact predecessor declaration`);
    assertAdditiveConfiguration(prior, next, label);
    evolution.set(saved.path, { prior, next });
  }
  return evolution;
}

async function priorConfigurationFiles(member, evolution) {
  const files = sourceFileMap(member);
  if (!evolution.size) return files;
  const configuration = [...evolution.values()].filter(entry => entry.prior.fields.length).map(entry => entry.prior);
  const projected = (await projectMemberConfiguration(member, configuration)).projectedFiles;
  for (const relative of evolution.keys()) {
    const file = files.get(relative), prior = projected.get(relative);
    if (file) files.set(relative, { ...file, configuration_digest: prior?.configuration_digest, owned_sha256: prior?.owned_sha256 });
  }
  return files;
}

function additiveConfigurationFields(entry, saved, priorActive, priorIncoming, active, incoming) {
  const contentMatches = file => file && sameOwnedFile({ ...file, mode: saved.mode }, saved);
  const activeIsBaseline = contentMatches(priorActive), incomingIsBaseline = contentMatches(priorIncoming);
  const priorPointers = new Set(entry.prior.fields.map(field => field.pointer));
  const installed = new Map(active.fields.map(field => [field.pointer, field]));
  const ambiguous = new Set();
  const fields = incoming.fields.map(field => {
    const current = installed.get(field.pointer);
    if (priorPointers.has(field.pointer)) return current;
    if (activeIsBaseline) return field;
    if (incomingIsBaseline || stableJson(current) === stableJson(field)) return current;
    ambiguous.add(field.pointer);
    return current;
  });
  return { fields, ambiguous };
}

async function assertPackageOwnersUnclaimed(roots, manifest, source) {
  const owners = new Set(source.members.map(memberKey));
  for (const file of await fs.readdir(roots.receiptsRoot)) {
    if (!file.endsWith(".json")) continue;
    const id = requiredId(file.slice(0, -5), "installed package id");
    if (id === manifest.id) continue;
    const receipt = await readReceipt(roots, id);
    const collision = receipt?.members.find(member => owners.has(memberKey(member)));
    if (collision) throw new Error(`Installed package '${manifest.id}' cannot claim owner '${memberKey(collision)}' already receipted by '${id}'`);
  }
}

function sourceFileMap(member) {
  return new Map(member.files.map(file => {
    const relative = snapshotRelativePath(file.relative_path), projected = member.projectedFiles.get(relative);
    return [relative, { ...file, ...(projected ? { owned_sha256: projected.owned_sha256, configuration_digest: projected.configuration_digest } : {}) }];
  }));
}

function sameOwnedFile(left, right) {
  if (!left || !right) return left === right;
  if (left.mode !== right.mode) return false;
  if (left.configuration_digest !== undefined || right.configuration_digest !== undefined) {
    return left.configuration_digest === right.configuration_digest && left.owned_sha256 === right.owned_sha256;
  }
  return left.sha256 === right.sha256 && left.size === right.size;
}

function compareSnapshotPaths(left, right) {
  const a = left.split("/"), b = right.split("/");
  for (let index = 0; index < Math.min(a.length, b.length); index += 1) {
    const order = a[index].localeCompare(b[index]);
    if (order) return order;
  }
  return a.length - b.length;
}

function conflictResolutionKey(packageId, member, relative) {
  return `${packageId}\0${memberKey(member)}\0${relative}`;
}

function normalizePackageConflictResolutions(value) {
  if (value === undefined) return new Map();
  if (!Array.isArray(value) || !value.length || value.length > 128) throw new Error("conflictResolutions must contain 1 to 128 explicit file resolutions");
  const fields = ["packageId", "registry_kind", "owner", "path", "previousSource", "active", "incoming", "contentBase64", "mode"].sort().join("\0");
  const rows = new Map();
  let totalBytes = 0;
  for (const row of structuredClone(value)) {
    if (!isRecord(row) || Object.keys(row).sort().join("\0") !== fields) throw new Error("Malformed package conflict resolution fields");
    requiredId(row.packageId, "conflict resolution packageId");
    requiredRegistryKind(row.registry_kind);
    requiredId(row.owner, "conflict resolution owner");
    if (typeof row.path !== "string" || !row.path || row.path.length > 4096
      || path.posix.isAbsolute(row.path) || path.win32.parse(row.path).root || path.posix.normalize(row.path) !== row.path
      || row.path.split("/").some(part => !part || part === "." || part === ".." || /[\\\u0000-\u001f:]/.test(part) || SOURCE_EXCLUDES.has(part))) {
      throw new Error("Conflict resolution requires an exact relative, non-excluded file path");
    }
    for (const name of ["previousSource", "active", "incoming"]) {
      const pin = row[name];
      if (!isRecord(pin) || Object.keys(pin).sort().join("\0") !== ["mode", "sha256", "size"].join("\0")
        || !DIGEST.test(pin.sha256) || !Number.isSafeInteger(pin.size) || pin.size < 0
        || !Number.isInteger(pin.mode) || pin.mode < 0 || pin.mode > 0o777) throw new Error(`Malformed conflict resolution ${name} pin`);
    }
    if (!Number.isInteger(row.mode) || row.mode < 0 || row.mode > 0o777
      || typeof row.contentBase64 !== "string" || row.contentBase64.length > Math.ceil(MAX_RESOLUTION_BYTES / 3) * 4) {
      throw new Error("Malformed conflict resolution content or mode");
    }
    const body = Buffer.from(row.contentBase64, "base64");
    totalBytes += body.byteLength;
    if (body.toString("base64") !== row.contentBase64 || body.byteLength > MAX_RESOLUTION_BYTES || totalBytes > 4 * MAX_RESOLUTION_BYTES) {
      throw new Error("Conflict resolution content must be canonical base64, at most 16 MiB per file and 64 MiB total");
    }
    const key = conflictResolutionKey(row.packageId, row, row.path);
    if (rows.has(key)) throw new Error(`Duplicate conflict resolution for '${row.packageId}:${memberKey(row)}/${row.path}'`);
    const { contentBase64, mode, ...proof } = row;
    rows.set(key, { ...proof, result: { sha256: sha256Bytes(body), size: body.byteLength, mode }, body });
  }
  return new Map([...rows].sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0));
}

function assertConflictResolutionsUsed(resolutions, used) {
  for (const [key, row] of resolutions) {
    if (!used.has(key)) throw new Error(`Unused or out-of-scope conflict resolution '${row.packageId}:${memberKey(row)}/${row.path}'`);
  }
}

async function resolvePackageFileConflict(resolution, before, current, next, member, active, preservedFields) {
  const label = `${resolution.packageId}:${memberKey(member)}/${resolution.path}`;
  for (const [name, file] of [["previousSource", before], ["active", current], ["incoming", next]]) {
    const pin = resolution[name];
    if (!file || pin.sha256 !== file.sha256 || pin.size !== file.size || pin.mode !== file.mode) {
      throw new Error(`Conflict resolution '${label}' ${name} pin does not match an existing regular file`);
    }
  }
  const file = { ...snapshotFile(member, resolution.path), body: resolution.body, ...resolution.result,
    input_sha256: next.sha256, input_size: next.size, input_mode: next.mode };
  if (path.extname(resolution.path).toLowerCase() === ".json") await readSnapshotJson(file, `Conflict resolution '${label}'`);
  const declaration = member.configuration?.find(entry => entry.path === resolution.path);
  if (!declaration) return { file };
  const projected = (await projectMemberConfiguration({ files: [file] }, [declaration])).projectedFiles.get(resolution.path);
  const installed = active.projectedFiles.get(resolution.path);
  const required = preservedFields ?? installed?.fields;
  if (!required || required.some(field => stableJson(projected.fields.find(candidate => candidate.pointer === field.pointer)) !== stableJson(field))) {
    throw new Error(`Conflict resolution '${label}' changes active configuration ownership`);
  }
  return { file: { ...file, configuration_input: { ...current } }, projected };
}

async function preparePackageUpdate(roots, manifest, source, state, resolutions = new Map(), usedResolutions = new Set()) {
  assertPackageUpdateOwnership(manifest, source, state);
  await assertPackageOwnersUnclaimed(roots, manifest, source);
  if (state.receipt.schema_version === 1) {
    if ([...resolutions.values()].some(row => row.packageId === manifest.id)) throw new Error(`Conflict resolutions for '${manifest.id}' require a v2 per-file source baseline`);
    const prior = new Set(state.receipt.members.map(memberKey));
    const repaired = await preparePackageRepair(manifest, { ...source, members: source.members.filter(member => prior.has(memberKey(member))) }, state);
    const members = source.members.map(member => {
      if (!prior.has(memberKey(member))) return member;
      const replacement = repaired.members.find(candidate => memberKey(candidate) === memberKey(member));
      if (!replacement) throw new Error(`Installed package '${manifest.id}' lost prior owner '${memberKey(member)}' during update`);
      return replacement;
    });
    return { ...source, members, fileCount: members.reduce((total, member) => total + member.files.length, 0) };
  }
  const members = [];
  for (const member of source.members) {
    const active = state.activeMembers.find(candidate => memberKey(candidate) === memberKey(member));
    const receipt = state.receipt.members.find(candidate => memberKey(candidate) === memberKey(member));
    if (!receipt) { members.push(member); continue; }
    const baseline = new Map(receipt.source_files.map(file => [file.path, file]));
    const incoming = sourceFileMap(member), local = sourceFileMap(active);
    const evolution = configurationEvolution(member, receipt);
    const priorLocal = await priorConfigurationFiles(active, evolution), priorIncoming = await priorConfigurationFiles(member, evolution);
    const paths = [...new Set([...baseline.keys(), ...incoming.keys(), ...local.keys()])].sort(compareSnapshotPaths);
    const files = [], projectedFiles = new Map();
    for (const relative of paths) {
      const before = baseline.get(relative), next = incoming.get(relative), current = local.get(relative);
      const transition = evolution.get(relative);
      const configured = transition && additiveConfigurationFields(transition, before, priorLocal.get(relative), priorIncoming.get(relative),
        active.projectedFiles.get(relative), member.projectedFiles.get(relative));
      let selected, resolved, conflict = false;
      if (sameOwnedFile(priorLocal.get(relative), before)) selected = next;
      else if (sameOwnedFile(priorIncoming.get(relative), before) || sameOwnedFile(current, next)) selected = current;
      else conflict = true;
      if (conflict || configured?.ambiguous.size) {
        const key = conflictResolutionKey(manifest.id, member, relative), resolution = resolutions.get(key);
        if (!resolution) throw new Error(`Installed package '${manifest.id}' has overlapping source-owned changes at '${memberKey(member)}/${relative}'; prior installation is preserved`);
        resolved = await resolvePackageFileConflict(resolution, before, current, next, member, active,
          configured?.fields.filter(field => !configured.ambiguous.has(field.pointer)));
        usedResolutions.add(key);
        selected = resolved.file;
      }
      if (!selected) continue;
      const projected = (selected === next ? member : active).projectedFiles.get(relative);
      let merged = resolved ?? (projected && (selected === next || configured)
        ? mergeConfiguredFile(selected, projected, configured ? { fields: configured.fields } : active.projectedFiles.get(relative), current)
        : { file: selected, projected });
      if (transition) {
        const validated = await projectMemberConfiguration({ files: [merged.file] }, [transition.next]);
        merged = { file: { ...merged.file, configuration_input: { ...current } }, projected: validated.projectedFiles.get(relative) };
      }
      files.push(merged.file);
      if (merged.projected) projectedFiles.set(relative, merged.projected);
    }
    const filePaths = new Set(files.map(file => snapshotRelativePath(file.relative_path)));
    const baselineDirectories = new Set([""]);
    for (const relative of baseline.keys()) {
      const components = relative.split("/");
      for (let length = 1; length < components.length; length += 1) baselineDirectories.add(components.slice(0, length).join("/"));
    }
    const fileAncestor = relative => {
      const components = relative.split("/");
      for (let length = components.length; length > 0; length -= 1) {
        const candidate = components.slice(0, length).join("/");
        if (filePaths.has(candidate)) return candidate;
      }
      return null;
    };
    const directories = new Set(["", ...member.directories]);
    for (const directory of active.directories) {
      const relative = snapshotRelativePath(directory);
      if (fileAncestor(relative) && baselineDirectories.has(relative)) continue;
      directories.add(relative);
    }
    for (const relative of filePaths) {
      const components = relative.split("/");
      for (let length = 1; length < components.length; length += 1) directories.add(components.slice(0, length).join("/"));
    }
    for (const directory of directories) {
      if (fileAncestor(directory)) throw new Error(`Installed package '${manifest.id}' has overlapping file/directory ownership at '${memberKey(member)}/${directory}'`);
    }
    members.push({ ...member, files, directories: [...directories].sort(compareSnapshotPaths), projectedFiles,
      digest: snapshotDigest(files), contentDigest: snapshotDigest(files.map(file => {
        const projected = projectedFiles.get(snapshotRelativePath(file.relative_path));
        return projected ? { ...file, size: projected.owned_size, sha256: projected.owned_sha256 } : file;
      })) });
  }
  return { ...source, members, fileCount: members.reduce((total, member) => total + member.files.length, 0) };
}

async function applyLocalApplicationPack(options) {
  const roots = normalizeRoots(options);
  return withPackageStateLock(roots.stateRoot, async () => {
    await ensurePackageState(roots);
    await recoverLocalPackageTransactionsUnlocked(roots);
    const operationId = requiredOperationId(options?.operationId);
    const packId = requiredId(options?.packId, "packId");
    const library = await loadLocalPackageLibrary(roots);
    const pack = library.packs.find((entry) => entry.pack.id === packId);
    if (!pack) throw new Error(`local Application Pack '${packId}' is unavailable`);
    const recommended = new Set(pack.pack.components
      .filter((component) => component.selection === "recommended")
      .map((component) => component.package_id));
    const selectedRecommended = requiredUniqueIds(options?.selectedRecommended ?? [], "selectedRecommended", true);
    for (const packageId of selectedRecommended) {
      if (!recommended.has(packageId)) {
        throw new Error(`Application Pack '${packId}' does not recommend package '${packageId}'`);
      }
    }
    const packageIds = pack.pack.components.flatMap((component) => (
      component.selection === "included"
      || component.selection === "recommended" && selectedRecommended.includes(component.package_id)
        ? [component.package_id]
        : []
    ));
    if (packageIds.length === 0) throw new Error(`Application Pack '${packId}' selects no installable packages`);
    const expected = normalizeDigestMap(options?.expectedSourceDigests, packageIds, "expectedSourceDigests");
    const selected = requireReviewedPackageClosure(library, packageIds);
    const result = await installResolvedPackages(roots, selected, {
      operationId,
      expected,
      assertMutable: options?.assertMutable,
      packId,
      onProgress: options?.onProgress,
    });
    return { ...result, action: "apply_application_pack", packId };
  });
}

function packageDependencyOrder(library, packageIds) {
  const visited = new Set();
  const visiting = new Set();
  const ordered = [];
  const visit = (id) => {
    if (visiting.has(id)) throw new Error(`Local package dependency cycle at '${id}'`);
    if (visited.has(id)) return;
    const entry = requireLibraryPackage(library, id);
    visiting.add(id);
    for (const dependency of entry.manifest.required_packages ?? []) visit(dependency);
    visiting.delete(id);
    visited.add(id);
    ordered.push(entry);
  };
  for (const id of packageIds) visit(id);
  return ordered;
}

function requireReviewedPackageClosure(library, packageIds) {
  const ordered = packageDependencyOrder(library, packageIds);
  const missing = ordered.filter((entry) => !packageIds.includes(entry.manifest.id));
  if (missing.length) throw new Error(`Package installation requires its reviewed dependency closure and exact digests: ${missing.map((entry) => entry.manifest.id).join(", ")}`);
  return ordered;
}

async function assertNoMaterializedPackageDependents(roots, library, removedIds) {
  const entries = new Map(library.packages.map((entry) => [entry.manifest.id, entry.manifest]));
  const receipts = new Map();
  for (const file of await fs.readdir(roots.receiptsRoot)) {
    if (!file.endsWith(".json")) continue;
    const id = requiredId(file.slice(0, -5), "package receipt id");
    const receipt = await readReceipt(roots, id);
    if (receipt) receipts.set(id, receipt);
  }
  const dependencies = (id) => [...new Set([
    ...(entries.get(id)?.required_packages ?? []),
    ...(receipts.get(id)?.required_packages ?? []),
  ])];
  const reachesRemoval = (id, visited = new Set()) => {
    if (removedIds.includes(id)) return id;
    if (visited.has(id)) return null;
    visited.add(id);
    for (const dependency of dependencies(id)) {
      const removed = reachesRemoval(dependency, visited);
      if (removed) return removed;
    }
    return null;
  };
  for (const id of new Set([...entries.keys(), ...receipts.keys()])) {
    if (removedIds.includes(id)) continue;
    const removed = reachesRemoval(id);
    if (!removed) continue;
    const members = [...(entries.get(id)?.members ?? []), ...(receipts.get(id)?.members ?? [])];
    for (const member of members) {
      if (await pathExists(memberPath(roots.activeRegistryRoot, member))) {
        throw new Error(`Cannot remove package '${removed}': materialized dependent '${id}' declares or pins this dependency. Remove the dependent explicitly first; missing or stale receipts do not mean its owners are absent.`);
      }
    }
  }
}

async function recoverLocalPackageTransactions(options) {
  const roots = normalizeRoots(options);
  return withPackageStateLock(roots.stateRoot, async () => {
    await ensurePackageState(roots);
    return recoverLocalPackageTransactionsUnlocked(roots);
  });
}

function normalizeRoots(options) {
  const sourceRegistryRoot = requiredAbsolutePath(options?.sourceRegistryRoot, "sourceRegistryRoot");
  const activeRegistryRoot = requiredAbsolutePath(options?.activeRegistryRoot, "activeRegistryRoot");
  const libraryRoot = requiredAbsolutePath(options?.libraryRoot, "libraryRoot");
  const stateRoot = requiredAbsolutePath(options?.stateRoot, "stateRoot");
  return {
    sourceRegistryRoot: path.resolve(sourceRegistryRoot),
    activeRegistryRoot: path.resolve(activeRegistryRoot),
    libraryRoot: path.resolve(libraryRoot),
    stateRoot: path.resolve(stateRoot),
    receiptsRoot: path.join(path.resolve(stateRoot), "receipts"),
    transactionsRoot: path.join(path.resolve(stateRoot), "transactions"),
  };
}

async function ensurePackageState(roots) {
  await requirePlainDirectory(roots.libraryRoot, roots.libraryRoot);
  await fs.mkdir(roots.activeRegistryRoot, { recursive: true, mode: 0o700 });
  await requirePlainDirectory(roots.activeRegistryRoot, roots.activeRegistryRoot);
  for (const directory of [roots.stateRoot, roots.receiptsRoot, roots.transactionsRoot]) {
    await fs.mkdir(directory, { recursive: true, mode: 0o700 });
    await requirePlainDirectory(directory, directory);
  }
}

async function loadLocalPackageLibrary(roots) {
  const errors = [];
  const explicit = await readExplicitPackageManifests(roots.libraryRoot, errors);
  if (errors.length) throw new Error(`local package library has invalid explicit manifests: ${errors.join("; ")}`);
  const claimedOwners = new Set(explicit.flatMap((entry) => entry.manifest.members.map(memberKey)));
  const synthesizedApplications = await synthesizeApplicationPackages(
    roots.sourceRegistryRoot,
    claimedOwners,
    errors,
  );
  for (const entry of synthesizedApplications) {
    for (const member of entry.manifest.members) claimedOwners.add(memberKey(member));
  }
  const synthesizedDrivers = await synthesizeDriverPackages(
    roots.sourceRegistryRoot,
    claimedOwners,
    errors,
  );
  const packages = [...explicit, ...synthesizedApplications, ...synthesizedDrivers]
    .sort((left, right) => left.manifest.id.localeCompare(right.manifest.id));
  validatePackageOwnership(packages);
  for (const entry of packages) entry.manifest = await withApplicationConfiguration(roots.sourceRegistryRoot, entry.manifest);
  const packs = await readApplicationPacks(roots.libraryRoot, packages, errors);
  return { packages, packs, errors };
}

async function readExplicitPackageManifests(libraryRoot, errors) {
  const root = path.join(libraryRoot, "packages");
  const entries = await readOptionalDirectory(root);
  const manifests = [];
  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const file = path.join(root, entry.name);
    try {
      const document = await readJsonDocument(file, `local package manifest ${entry.name}`);
      const manifest = parsePackageManifest(document.value, file);
      if (`${manifest.id}.json` !== entry.name) {
        throw new Error(`local package manifest file name disagrees with id '${manifest.id}': ${file}`);
      }
      manifests.push({ manifest, manifestDigest: document.digest, definitionDigest: document.digest, file, source: "explicit" });
    } catch (error) {
      errors.push(errorMessage(error));
    }
  }
  return manifests;
}

async function withApplicationConfiguration(sourceRegistryRoot, manifest) {
  const configured = structuredClone(manifest);
  for (const member of manifest.members.filter((member) => member.registry_kind === "interface")) {
    const file = path.join(memberPath(sourceRegistryRoot, member), "application.json");
    if (!await pathExists(file)) continue;
    const document = await readJsonDocument(file, `package Application setup '${member.owner}'`);
    const application = parseApplicationManifest(document.value, member.owner, file);
    for (const field of application.setup.fields) {
      const target = configured.members.find((candidate) => candidate.registry_kind === "interface" && candidate.owner === field.interface);
      if (!target) throw new Error(`Application setup '${member.owner}' has no package member '${field.interface}'`);
      target.configuration ??= [];
      let configuration = target.configuration.find((entry) => entry.path === "interface.json");
      if (!configuration) { configuration = { path: "interface.json", fields: [] }; target.configuration.push(configuration); }
      configuration.fields.push({ pointer: field.pointer, schema: { type: "string" } });
    }
  }
  return parsePackageManifest(configured, `configuration:${manifest.id}`);
}

async function synthesizeApplicationPackages(sourceRegistryRoot, claimedOwners, errors) {
  const root = path.join(sourceRegistryRoot, "interface");
  const entries = await readOptionalDirectory(root);
  const packages = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || !SAFE_ID.test(entry.name)) continue;
    const file = path.join(root, entry.name, "application.json");
    if (!await pathExists(file)) continue;
    try {
      const document = await readJsonDocument(file, `application manifest ${entry.name}`);
      const application = parseApplicationManifest(document.value, entry.name, file);
      const members = application.interfaces.map((owner) => ({
        registry_kind: "interface",
        owner,
        ...(owner === entry.name ? { primary: true } : {}),
      }));
      if (members.some((member) => claimedOwners.has(memberKey(member)))) continue;
      const manifest = parsePackageManifest({
        schema: "octopus.local_package",
        schema_version: 1,
        id: application.id,
        version: application.version,
        product_kind: "application",
        product_id: application.id,
        release_status: "optional",
        display: {
          name: application.name,
          description: application.description,
          developer: application.developer,
          category: application.category,
          ...(application.icon ? { icon: application.icon } : {}),
        },
        members,
      }, file);
      packages.push({
        manifest,
        manifestDigest: hashJson(manifest),
        definitionDigest: document.digest,
        file,
        source: "application_manifest",
      });
    } catch (error) {
      errors.push(errorMessage(error));
    }
  }
  return packages;
}

async function synthesizeDriverPackages(sourceRegistryRoot, claimedOwners, errors) {
  const kindRoot = path.join(sourceRegistryRoot, "worker_environment_kind");
  const toolRoot = path.join(sourceRegistryRoot, "tool");
  const kindEntries = await readOptionalDirectory(kindRoot);
  const toolEntries = await readOptionalDirectory(toolRoot);
  const tools = [];
  for (const entry of toolEntries) {
    if (!entry.isDirectory() || !SAFE_ID.test(entry.name)) continue;
    const file = path.join(toolRoot, entry.name, "tool.json");
    if (!await pathExists(file)) continue;
    try {
      const document = await readJsonDocument(file, `tool descriptor ${entry.name}`);
      const id = requiredId(document.value?.id, `tool descriptor ${entry.name} id`);
      if (id !== entry.name || !Array.isArray(document.value?.supported_worker_environment_kinds)) {
        throw new Error(`tool descriptor '${entry.name}' has invalid ownership`);
      }
      tools.push({ id, interface: requiredId(document.value?.interface, `tool '${id}' interface`), document: document.value });
    } catch (error) {
      errors.push(errorMessage(error));
    }
  }
  const packages = [];
  for (const entry of kindEntries) {
    if (!entry.isDirectory() || !SAFE_ID.test(entry.name)) continue;
    const file = path.join(kindRoot, entry.name, "worker_environment_kind.json");
    if (!await pathExists(file)) continue;
    try {
      const document = await readJsonDocument(file, `worker environment kind ${entry.name}`);
      const descriptor = document.value;
      const identity = [
        descriptor?.worker_environment_kind_id,
        descriptor?.worker_environment_kind,
        descriptor?.kind,
        descriptor?.id,
      ].map(stringValue).find(Boolean);
      if (identity !== entry.name) throw new Error(`worker environment kind '${entry.name}' has drifted identity`);
      const ownedTools = tools.filter((tool) => (
        tool.document.supported_worker_environment_kinds.map(stringValue).includes(entry.name)
      ));
      const members = [
        { registry_kind: "worker_environment_kind", owner: entry.name, primary: true },
        ...ownedTools.map((tool) => ({ registry_kind: "tool", owner: tool.id })),
        ...[...new Set(ownedTools.map((tool) => tool.interface))].sort()
          .map((owner) => ({ registry_kind: "interface", owner })),
      ];
      if (members.some((member) => claimedOwners.has(memberKey(member)))) continue;
      const manifest = parsePackageManifest({
        schema: "octopus.local_package",
        schema_version: 1,
        id: `driver.${entry.name}`,
        version: "1.0.0",
        product_kind: "driver",
        product_id: entry.name,
        release_status: "optional",
        display: {
          name: humanizeId(entry.name),
          description: driverDescription(descriptor),
          developer: "Octopus",
          category: "worker_environment",
          icon: "hard-drive",
        },
        members,
      }, file);
      packages.push({
        manifest,
        manifestDigest: hashJson(manifest),
        definitionDigest: document.digest,
        file,
        source: "worker_environment_kind",
        productMetadata: {
          descriptor,
          tool_ids: ownedTools.map((tool) => tool.id).sort(),
          door_interfaces: [...new Set(ownedTools.map((tool) => tool.interface))].sort(),
        },
      });
    } catch (error) {
      errors.push(errorMessage(error));
    }
  }
  return packages;
}

async function readApplicationPacks(libraryRoot, packages, errors) {
  const root = path.join(libraryRoot, "packs");
  const entries = await readOptionalDirectory(root);
  const available = new Set(packages.map((entry) => entry.manifest.id));
  const packageById = new Map(packages.map((entry) => [entry.manifest.id, entry.manifest]));
  const packs = [];
  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    const file = path.join(root, entry.name);
    try {
      const document = await readJsonDocument(file, `local Application Pack ${entry.name}`);
      const pack = parseApplicationPack(document.value, file);
      if (`${pack.id}.json` !== entry.name) throw new Error(`Application Pack file name disagrees with id '${pack.id}'`);
      const repeated = repeatedValues(pack.components.map((component) => component.package_id));
      if (repeated.length) throw new Error(`Application Pack '${pack.id}' repeats packages: ${repeated.join(", ")}`);
      const unresolved = pack.components
        .map((component) => component.package_id)
        .filter((packageId) => !available.has(packageId));
      if (unresolved.length) throw new Error(`Application Pack '${pack.id}' has unavailable packages: ${unresolved.join(", ")}`);
      validatePackWorkspaceSetup(pack, packageById);
      packs.push({ pack, packDigest: document.digest, file });
    } catch (error) {
      errors.push(errorMessage(error));
    }
  }
  return packs.sort((left, right) => left.pack.id.localeCompare(right.pack.id));
}

function validatePackWorkspaceSetup(pack, packageById) {
  if (!pack.workspace_setup) return;
  const componentIds = new Set(pack.components.map((component) => component.package_id));
  const componentPackages = [...componentIds].map((packageId) => packageById.get(packageId));
  const interfaceOwners = new Set(componentPackages.flatMap((manifest) => manifest.members
    .filter((member) => member.registry_kind === "interface")
    .map((member) => member.owner)));
  const artifactOwners = new Set(componentPackages.flatMap((manifest) => manifest.members
    .filter((member) => member.registry_kind === "artifact")
    .map((member) => member.owner)));
  const driverNames = new Set(componentPackages
    .filter((manifest) => manifest.product_kind === "driver")
    .map((manifest) => manifest.product_id));
  for (const [field, interfaceId] of [
    ["home.provider_interface_id", pack.workspace_setup.home.provider_interface_id],
    ["home.worker_interface_id", pack.workspace_setup.home.worker_interface_id],
    ["task.worker_interface_id", pack.workspace_setup.task.worker_interface_id],
  ]) {
    if (!interfaceOwners.has(interfaceId)) {
      throw new Error(`Application Pack '${pack.id}' workspace_setup.${field} is not supplied by a component`);
    }
  }
  for (const binding of [...pack.workspace_setup.home.context_artifact_bindings, ...pack.workspace_setup.task.context_artifact_bindings]) {
    if (!artifactOwners.has(binding.id)) {
      throw new Error(`Application Pack '${pack.id}' workspace context artifact '${binding.id}' is not supplied by a component`);
    }
  }
  for (const kind of [...pack.workspace_setup.home.selected_worker_environment_kinds, ...pack.workspace_setup.task.selected_worker_environment_kinds]) {
    if (!driverNames.has(kind)) {
      throw new Error(`Application Pack '${pack.id}' workspace worker environment '${kind}' is not supplied by a Driver component`);
    }
  }
  const projectDefaults = pack.workspace_setup.project_defaults.map((entry) => entry.package_id);
  const repeatedDefaults = repeatedValues(projectDefaults);
  if (repeatedDefaults.length) {
    throw new Error(`Application Pack '${pack.id}' repeats project defaults: ${repeatedDefaults.join(", ")}`);
  }
  const unknownDefaults = projectDefaults.filter((packageId) => !componentIds.has(packageId));
  if (unknownDefaults.length) {
    throw new Error(`Application Pack '${pack.id}' has project defaults outside its components: ${unknownDefaults.join(", ")}`);
  }
  const presetIds = new Set();
  for (const preset of pack.workspace_setup.agent_presets ?? []) {
    if (presetIds.has(preset.id)) throw new Error(`Application Pack '${pack.id}' repeats Agent preset '${preset.id}'`);
    presetIds.add(preset.id);
    if (!componentIds.has(preset.component_package_id)
        || !interfaceOwners.has(preset.worker_interface_id)
        || !interfaceOwners.has(preset.provider_interface_id)
        || preset.context_artifact_bindings.some((binding) => !artifactOwners.has(binding.id))
        || preset.selected_worker_environment_kinds.some((kind) => !driverNames.has(kind))
        || preset.selected_application_ids.some((id) => !componentPackages.some((entry) => entry.product_kind === "application" && entry.product_id === id))) {
      throw new Error(`Application Pack '${pack.id}' Agent preset '${preset.id}' is outside its component closure`);
    }
  }
}

async function withInstalledAgentPreset(options, choice, operation) {
  const roots = normalizeRoots(options);
  return withPackageStateLock(roots.stateRoot, async () => {
    const library = await loadLocalPackageLibrary(roots);
    const packId = requiredId(choice?.packId, "Agent preset pack");
    const presetId = requiredId(choice?.presetId, "Agent preset");
    const pack = library.packs.find((entry) => entry.pack.id === packId);
    if (!pack || !DIGEST.test(choice.expectedPackManifestDigest ?? "") || pack.packDigest !== choice.expectedPackManifestDigest) {
      throw new Error("Agent preset pack changed or is unavailable");
    }
    const recipe = pack.pack.workspace_setup?.agent_presets?.find((entry) => entry.id === presetId);
    if (!recipe) throw new Error(`Pack '${packId}' does not declare Agent preset '${presetId}'`);
    const documents = [];
    const read = async (kind, owner, name) => {
      const directory = await realPlainDirectory(path.join(roots.activeRegistryRoot, kind, owner), `Agent preset ${kind}/${owner}`);
      if (directory !== path.join(roots.activeRegistryRoot, kind, owner)) throw new Error("Agent preset owner is not canonical");
      const file = path.join(directory, name);
      const document = await readJsonDocument(file, `Agent preset ${owner}/${name}`);
      documents.push({ file, digest: document.digest });
      return document.value;
    };
    const sourceInterface = await read("interface", recipe.worker_interface_id, "interface.json");
    const sourcePolicy = await read("interface", recipe.worker_interface_id, "policy.json");
    if (sourceInterface.id !== recipe.worker_interface_id || sourceInterface.interface_type !== "system" || sourceInterface.role !== "worker"
        || sourcePolicy.id !== recipe.worker_interface_id || !isRecord(sourcePolicy.ceiling) || !Array.isArray(sourcePolicy.role_artifacts)) {
      throw new Error("Agent preset source must be an exact worker pin with a policy");
    }
    const dependencies = [{ registry_kind: "interface", owner: recipe.worker_interface_id }];
    for (const [field, kind, values] of [
      ["tools", "tool", sourcePolicy.ceiling.tools],
      ["wek", "worker_environment_kind", sourcePolicy.ceiling.wek],
      ["mark_outcomes", "mark", sourcePolicy.ceiling.mark_outcomes],
      ["role_artifacts", "artifact", sourcePolicy.role_artifacts],
    ]) {
      if (!Array.isArray(values)) throw new Error(`Agent preset source ${field} is malformed`);
      for (const owner of values) dependencies.push({ registry_kind: kind, owner: requiredId(owner, `Agent preset ${field}`) });
    }
    for (const binding of recipe.context_artifact_bindings) dependencies.push({ registry_kind: "artifact", owner: binding.id });
    if (sourcePolicy.role_artifacts.some((id) => !recipe.context_artifact_bindings.some((binding) => binding.id === id))) {
      throw new Error("Agent preset omits a source worker role artifact");
    }
    const packageIds = new Set([recipe.component_package_id]);
    for (const dependency of dependencies) {
      const owners = library.packages.filter((entry) => entry.manifest.members.some((member) => memberKey(member) === memberKey(dependency)));
      if (owners.length !== 1 || !pack.pack.components.some((component) => component.package_id === owners[0].manifest.id)) {
        throw new Error(`Agent preset dependency '${memberKey(dependency)}' has no exact pack owner`);
      }
      packageIds.add(owners[0].manifest.id);
    }
    const verify = async () => {
      const currentPack = await readJsonDocument(pack.file, "Agent preset pack");
      if (currentPack.digest !== pack.packDigest) throw new Error("Agent preset pack changed during authoring");
      for (const entry of packageDependencyOrder(library, [...packageIds])) {
        const id = entry.manifest.id;
        if (entry.source === "explicit" && (await readJsonDocument(entry.file, `Agent preset dependency '${id}'`)).digest !== entry.manifestDigest) throw new Error(`Agent preset dependency manifest '${id}' changed during authoring`);
        const source = await snapshotPackageSource(roots, entry.manifest, { fresh: true });
        const state = await readPackageState(roots, entry.manifest, source);
        const record = packageCatalogRecord(entry, source, state);
        if (!record.installed || record.receipt_state !== "valid") throw new Error(`Agent preset requires verified installed package '${id}'`);
      }
      for (const document of documents) {
        if ((await readJsonDocument(document.file, "Agent preset source")).digest !== document.digest) throw new Error("Agent preset source changed during authoring");
      }
    };
    await verify();
    return operation({ recipe: structuredClone(recipe), sourceInterface, sourcePolicy, verify });
  });
}

function parsePackageManifest(value, file) {
  if (!validatePackageManifest(value)) throw schemaError(`local package manifest ${file}`, validatePackageManifest.errors);
  const manifest = structuredClone(value);
  const memberKeys = manifest.members.map(memberKey);
  const repeated = repeatedValues(memberKeys);
  if (repeated.length) throw new Error(`local package '${manifest.id}' repeats registry owners: ${repeated.join(", ")}`);
  if (manifest.members.filter((member) => member.primary === true).length > 1) {
    throw new Error(`local package '${manifest.id}' has more than one primary owner`);
  }
  if (!PACKAGE_KINDS.has(manifest.product_kind)) throw new Error(`local package '${manifest.id}' has invalid product kind`);
  for (const member of manifest.members) {
    const label = `${manifest.id}:${memberKey(member)}`;
    validateConfigurationOwnership(member.configuration ?? [], label, member.registry_kind);
    const predecessors = new Set();
    for (const prior of member.configuration_predecessors ?? []) {
      validateConfigurationOwnership([prior], label, member.registry_kind);
      const digest = hashJson(prior);
      if (predecessors.has(digest)) throw new Error(`${label} repeats a configuration predecessor`);
      predecessors.add(digest);
    }
    validateDataDirectoryOwnership(member, label);
  }
  return manifest;
}

function validateDataDirectoryOwnership(member, label) {
  const directories = [];
  for (const directory of member.data_directories ?? []) {
    if (path.posix.isAbsolute(directory) || path.win32.isAbsolute(directory)
      || path.posix.normalize(directory) !== directory
      || directory.split("/").some(component => !component || component === "." || component === ".."
        || /[\\\u0000-\u001f:]/.test(component) || SOURCE_EXCLUDES.has(component))) {
      throw new Error(`${label} has an invalid data directory '${directory}'`);
    }
    if (directories.some(other => directory === other || directory.startsWith(`${other}/`) || other.startsWith(`${directory}/`))) {
      throw new Error(`${label} has overlapping data directories at '${directory}'`);
    }
    if ((member.configuration ?? []).some(entry => entry.path.startsWith(`${directory}/`))) {
      throw new Error(`${label} data directory '${directory}' overlaps configuration ownership`);
    }
    directories.push(directory);
  }
}

function isMemberDataPath(member, relative) {
  return (member.data_directories ?? []).some(directory => relative.startsWith(`${directory}/`));
}

function validateMemberDataPaths(member, snapshot) {
  for (const absolute of snapshot.excludedEntries) {
    const relative = snapshotRelativePath(path.relative(snapshot.root, absolute));
    if (isMemberDataPath(member, relative)) {
      throw new Error(`Package owner '${memberKey(member)}' has an excluded data entry without materialization proof at '${relative}'`);
    }
  }
  for (const file of snapshot.files) {
    const relative = snapshotRelativePath(file.relative_path);
    if ((member.data_directories ?? []).some(directory => directory === relative || directory.startsWith(`${relative}/`))) {
      throw new Error(`Package owner '${memberKey(member)}' data directory is a file at '${relative}'`);
    }
  }
}

function validateConfigurationOwnership(configuration, label, registryKind) {
  const paths = new Set();
  for (const entry of configuration) {
    const relative = configurationPath(entry.path);
    if (paths.has(relative)) throw new Error(`${label} repeats configuration file '${relative}'`);
    paths.add(relative);
    const pointers = [];
    for (const field of entry.fields) {
      const components = configurationPointer(field.pointer);
      if (entry.path === `${registryKind}.json` && ["id", "worker_environment_kind_id"].includes(components[0])) {
        throw new Error(`${label} configuration cannot change registry identity at '${field.pointer}'`);
      }
      if (pointers.some((other) => other.every((value, index) => components[index] === value)
        || components.every((value, index) => other[index] === value))) {
        throw new Error(`${label} has overlapping configuration ownership at '${field.pointer}'`);
      }
      pointers.push(components);
      configurationValidator(field.schema);
    }
  }
}

function configurationPath(value) {
  if (typeof value !== "string" || path.extname(value) !== ".json"
    || value.split("/").some((component) => !component || component === "." || component === ".." || component.includes("\\") || component.includes("\0"))) {
    throw new Error(`invalid package configuration path '${value}'`);
  }
  return value;
}

function configurationPointer(value) {
  if (typeof value !== "string" || !value.startsWith("/") || /~(?![01])/.test(value)) {
    throw new Error(`invalid package configuration pointer '${value}'`);
  }
  const components = value.slice(1).split("/").map((part) => part.replaceAll("~1", "/").replaceAll("~0", "~"));
  if (components.some((part) => !part || ["__proto__", "prototype", "constructor"].includes(part))) {
    throw new Error(`invalid package configuration pointer '${value}'`);
  }
  return components;
}

function configurationValidator(schema) {
  const key = stableJson(schema);
  if (key.length > 8192) throw new Error("package configuration schema exceeds its bound");
  if (!configurationValidators.has(key)) {
    if (configurationValidators.size >= 128) configurationValidators.clear();
    configurationValidators.set(key, validators.compile(schema));
  }
  return configurationValidators.get(key);
}

function configurationSlot(document, pointer, allowMissingParents = false) {
  const components = configurationPointer(pointer);
  const key = components.at(-1);
  let parent = document;
  for (const component of components.slice(0, -1)) {
    if (!isRecord(parent) && !Array.isArray(parent)) throw new Error(`package configuration field has no object parent at '${pointer}'`);
    if (!Object.hasOwn(parent, component)) {
      if (allowMissingParents && isRecord(parent)) return { parent: null, key, present: false, value: undefined };
      throw new Error(`package configuration parent is missing at '${pointer}'`);
    }
    parent = parent[component];
  }
  if (!isRecord(parent)) throw new Error(`package configuration field has no object parent at '${pointer}'`);
  return { parent, key, present: Object.hasOwn(parent, key), value: parent[key] };
}

async function projectMemberConfiguration(snapshot, configuration = [], deferredFields = new Set()) {
  const projectedFiles = new Map();
  for (const entry of configuration) {
    const file = snapshotFile(snapshot, entry.path);
    if (!file) throw new Error(`package configuration file '${entry.path}' is missing`);
    if (file.size > MAX_CONFIGURATION_BYTES || file.mode & 0o111) throw new Error(`package configuration file '${entry.path}' exceeds its data boundary`);
    const document = await readSnapshotJson(file, `package configuration '${entry.path}'`);
    if (!isRecord(document)) throw new Error(`package configuration '${entry.path}' must be a JSON object`);
    const owned = structuredClone(document);
    const fields = [];
    for (const field of entry.fields) {
      const deferred = deferredFields.has(`${entry.path}\0${field.pointer}`);
      const slot = configurationSlot(document, field.pointer, deferred);
      const validate = configurationValidator(field.schema);
      if (slot.present && !deferred && !validate(slot.value)) {
        throw schemaError(`package configuration '${entry.path}${field.pointer}'`, validate.errors);
      }
      const ownedSlot = configurationSlot(owned, field.pointer, deferred);
      if (ownedSlot.present) delete ownedSlot.parent[ownedSlot.key];
      fields.push({ pointer: field.pointer, present: slot.present, ...(slot.present ? { value: slot.value } : {}) });
    }
    const body = Buffer.from(stableJson(owned));
    projectedFiles.set(entry.path, {
      document,
      fields,
      owned_sha256: sha256Bytes(body),
      owned_size: body.byteLength,
      configuration_digest: hashJson(entry),
    });
  }
  return {
    ...snapshot,
    configuration,
    projectedFiles,
    contentDigest: snapshotDigest(snapshot.files.map((file) => {
      const projected = projectedFiles.get(snapshotRelativePath(file.relative_path));
      return projected ? { ...file, size: projected.owned_size, sha256: projected.owned_sha256 } : file;
    })),
  };
}

function sha256Bytes(body) {
  return crypto.createHash("sha256").update(body).digest("hex");
}

function snapshotDigest(files) {
  return hashJson(files.map((file) => ({
    path: snapshotRelativePath(file.relative_path), size: file.size, mode: file.mode, sha256: file.sha256,
  })));
}

function parseApplicationPack(value, file) {
  if (!validateApplicationPack(value)) throw schemaError(`local Application Pack ${file}`, validateApplicationPack.errors);
  return structuredClone(value);
}

function parseApplicationManifest(value, owner, file) {
  if (!isRecord(value) || value.schema_version !== 3 || value.id !== owner) {
    throw new Error(`application manifest does not satisfy schema v3: ${file}`);
  }
  for (const key of ["name", "description", "developer", "version", "category"]) {
    if (!stringValue(value[key])) throw new Error(`application manifest '${owner}' has no ${key}`);
  }
  if (!["connector", "plugin", "artifact", "provider"].includes(value.category)) {
    throw new Error(`application manifest '${owner}' has invalid category '${value.category}'`);
  }
  if (!Array.isArray(value.interfaces) || value.interfaces.length === 0) {
    throw new Error(`application manifest '${owner}' has no interfaces`);
  }
  const interfaces = value.interfaces.map((candidate, index) => requiredId(candidate, `application '${owner}' interface ${index}`));
  if (!interfaces.includes(owner) || new Set(interfaces).size !== interfaces.length) {
    throw new Error(`application manifest '${owner}' has invalid owner membership`);
  }
  const taskCapabilities = parseApplicationTaskCapabilities(value.task_capabilities, owner, file);
  const setup = parseApplicationSetup(value.setup, interfaces, owner, file);
  return {
    id: owner,
    name: value.name,
    description: value.description,
    developer: value.developer,
    version: value.version,
    category: value.category,
    icon: stringValue(value.icon),
    interfaces,
    task_capabilities: taskCapabilities,
    setup,
  };
}

function parseApplicationTaskCapabilities(value, owner, file) {
  if (value === undefined) return { tools: [] };
  if (!isRecord(value) || Object.keys(value).some((key) => key !== "tools")) {
    throw new Error(`application manifest '${owner}' has invalid task_capabilities: ${file}`);
  }
  if (!Array.isArray(value.tools)) {
    throw new Error(`application manifest '${owner}' has invalid task capability lists: ${file}`);
  }
  const tools = value.tools.map((entry, index) => {
    if (
      !isRecord(entry)
      || Object.keys(entry).sort().join("\0") !== ["interface", "tool", "worker_environment_kind"].join("\0")
    ) {
      throw new Error(`application manifest '${owner}' task capability tool ${index} is invalid`);
    }
    return {
      tool: requiredId(entry.tool, `application '${owner}' task capability tool ${index}`),
      interface: requiredId(entry.interface, `application '${owner}' task capability interface ${index}`),
      worker_environment_kind: requiredId(
        entry.worker_environment_kind,
        `application '${owner}' task capability worker environment ${index}`,
      ),
    };
  });
  if (new Set(tools.map((tool) => tool.tool)).size !== tools.length) {
    throw new Error(`application manifest '${owner}' repeats a task capability tool`);
  }
  return { tools };
}

function parseApplicationSetup(value, interfaces, owner, file) {
  if (value === undefined) return { fields: [] };
  if (!isRecord(value) || Object.keys(value).some((key) => key !== "fields") || !Array.isArray(value.fields)) {
    throw new Error(`application manifest '${owner}' has invalid setup: ${file}`);
  }
  const interfaceIds = new Set(interfaces);
  const fields = value.fields.map((entry, index) => {
    if (
      !isRecord(entry)
      || Object.keys(entry).sort().join("\0") !== ["interface", "kind", "label", "pointer", "required"].join("\0")
    ) {
      throw new Error(`application manifest '${owner}' setup field ${index} is invalid`);
    }
    const interfaceId = requiredId(entry.interface, `application '${owner}' setup interface ${index}`);
    const pointer = stringValue(entry.pointer);
    const label = stringValue(entry.label);
    if (
      !interfaceIds.has(interfaceId)
      || !pointer.startsWith("/")
      || pointer === "/"
      || pointer.split("/").slice(1).some((token) => /~(?:[^01]|$)/.test(token))
      || !label
      || label.length > 96
      || !["text", "url", "secret"].includes(entry.kind)
      || typeof entry.required !== "boolean"
    ) {
      throw new Error(`application manifest '${owner}' setup field ${index} is invalid`);
    }
    return { interface: interfaceId, pointer, label, kind: entry.kind, required: entry.required };
  });
  if (new Set(fields.map((field) => `${field.interface}\0${field.pointer}`)).size !== fields.length) {
    throw new Error(`application manifest '${owner}' repeats a setup field`);
  }
  return { fields };
}

function validatePackageOwnership(packages) {
  const packageIds = new Set();
  const owners = new Map();
  for (const entry of packages) {
    const packageId = entry.manifest.id;
    if (packageIds.has(packageId)) throw new Error(`local package library repeats package '${packageId}'`);
    packageIds.add(packageId);
    for (const member of entry.manifest.members) {
      const key = memberKey(member);
      const previous = owners.get(key);
      if (previous) throw new Error(`registry owner '${key}' belongs to both '${previous}' and '${packageId}'`);
      owners.set(key, packageId);
    }
  }
}

async function snapshotPackageSource(roots, manifest, options = {}) {
  const manifestDigest = hashJson(manifest);
  const members = [];
  for (const member of orderedMembers(manifest.members)) {
    const sourceDirectory = memberPath(roots.sourceRegistryRoot, member);
    await requirePlainDirectory(path.dirname(sourceDirectory), path.dirname(sourceDirectory));
    const snapshot = await snapshotDirectory(sourceDirectory, {
      cacheKey: options.fresh ? "" : `${roots.sourceRegistryRoot}\0${memberKey(member)}`,
    });
    validateMemberDataPaths(member, snapshot);
    members.push({ ...member, source_directory: sourceDirectory, ...await projectMemberConfiguration(snapshot, member.configuration) });
  }
  await assertApplicationInterfaceImportClosure(manifest, members, roots, options);
  const sourceDigest = hashJson({
    manifest_digest: manifestDigest,
    members: members.map((member) => ({
      registry_kind: member.registry_kind,
      owner: member.owner,
      digest: member.digest,
    })),
  });
  return {
    manifestDigest,
    sourceDigest,
    members,
    fileCount: members.reduce((total, member) => total + member.files.length, 0),
  };
}

async function assertApplicationInterfaceImportClosure(manifest, members, roots, options) {
  if (manifest.product_kind !== "application") return;
  const jsonDocuments = new Map();
  const imports = [];
  for (const member of members) {
    for (const file of member.files) {
      if (path.extname(file.relative_path).toLowerCase() !== ".json") continue;
      const relativePath = snapshotRelativePath(file.relative_path);
      const value = await readSnapshotJson(file, `local Application package '${manifest.id}' member '${memberKey(member)}/${relativePath}'`);
      jsonDocuments.set(file.path, value);
      collectInterfaceImports(value, "", imports, {
        member: memberKey(member),
        relativePath,
        packageId: manifest.id,
      });
    }
  }
  if (imports.length === 0) return;
  const interfaceMembers = new Map(members
    .filter((member) => member.registry_kind === "interface")
    .map((member) => [member.owner, member]));
  const externalOwners = new Set(imports.filter(imported => !interfaceMembers.has(imported.owner)).map(imported => imported.owner));
  if (externalOwners.size && manifest.required_packages?.length) {
    const library = await loadLocalPackageLibrary(roots);
    for (const dependency of packageDependencyOrder(library, manifest.required_packages)) {
      for (const member of dependency.manifest.members) {
        if (member.registry_kind !== "interface" || !externalOwners.has(member.owner)) continue;
        const sourceDirectory = memberPath(roots.sourceRegistryRoot, member);
        await requirePlainDirectory(path.dirname(sourceDirectory), path.dirname(sourceDirectory));
        const snapshot = await snapshotDirectory(sourceDirectory, {
          cacheKey: options.fresh ? "" : `${roots.sourceRegistryRoot}\0${memberKey(member)}`,
        });
        interfaceMembers.set(member.owner, { ...member, source_directory: sourceDirectory, ...snapshot });
      }
    }
  }
  const validatedOwners = new Set();
  for (const imported of imports) {
    const source = interfaceMembers.get(imported.owner);
    if (!source) {
      throw new Error(
        `local Application package '${manifest.id}' imports interface owner '${imported.owner}' at ${imported.location} but manifest members do not include 'interface/${imported.owner}' and no explicit required package owns it`,
      );
    }
    if (!validatedOwners.has(imported.owner)) {
      const descriptorFile = snapshotFile(source, "interface.json");
      if (!descriptorFile) {
        throw new Error(`local Application package '${manifest.id}' imported interface owner '${imported.owner}' has no interface.json in its snapshot`);
      }
      const descriptor = jsonDocuments.has(descriptorFile.path)
        ? jsonDocuments.get(descriptorFile.path)
        : await readSnapshotJson(
          descriptorFile,
          `local Application package '${manifest.id}' imported interface descriptor '${imported.owner}'`,
        );
      if (!isRecord(descriptor) || descriptor.id !== imported.owner) {
        throw new Error(`local Application package '${manifest.id}' imported interface owner '${imported.owner}' has a drifted descriptor id`);
      }
      validatedOwners.add(imported.owner);
    }
    if (!snapshotFile(source, imported.relativePath) && !snapshotDirectoryEntry(source, imported.relativePath)) {
      throw new Error(
        `local Application package '${manifest.id}' interface import '${imported.uri}' at ${imported.location} does not exist in the '${imported.owner}' snapshot`,
      );
    }
  }
}

async function readSnapshotJson(file, label) {
  const body = file.body === undefined ? await fs.readFile(file.path) : file.body;
  const digest = crypto.createHash("sha256").update(body).digest("hex");
  if (body.byteLength !== file.size || digest !== file.sha256) {
    throw new Error(`${label} changed after its package snapshot was formed`);
  }
  try {
    const value = JSON.parse(body.toString("utf8"));
    requireFiniteJson(value, label);
    return value;
  } catch (error) {
    throw new Error(`${label} is malformed JSON: ${errorMessage(error)}`);
  }
}

function requireFiniteJson(value, label) {
  if (typeof value === "number" && !Number.isFinite(value)) throw new Error(`${label} contains a non-finite number`);
  if (value && typeof value === "object") for (const entry of Object.values(value)) requireFiniteJson(entry, label);
}

function collectInterfaceImports(value, pointer, imports, source) {
  if (typeof value === "string") {
    if (!value.includes(INTERFACE_IMPORT_PREFIX)) return;
    const location = `${source.member}/${source.relativePath}${pointer || "/"}`;
    imports.push(parseInterfaceImport(value, location, source.packageId));
    return;
  }
  if (Array.isArray(value)) {
    value.forEach((entry, index) => collectInterfaceImports(entry, `${pointer}/${index}`, imports, source));
    return;
  }
  if (!isRecord(value)) return;
  for (const [key, entry] of Object.entries(value)) {
    collectInterfaceImports(entry, `${pointer}/${jsonPointerToken(key)}`, imports, source);
  }
}

function parseInterfaceImport(uri, location, packageId) {
  if (!uri.startsWith(INTERFACE_IMPORT_PREFIX) || uri.indexOf(INTERFACE_IMPORT_PREFIX, INTERFACE_IMPORT_PREFIX.length) !== -1) {
    throw new Error(`local Application package '${packageId}' has an invalid interface import value at ${location}`);
  }
  const components = uri.slice(INTERFACE_IMPORT_PREFIX.length).split("/");
  const owner = components.shift();
  if (
    !SAFE_ID.test(owner ?? "")
    || components.length === 0
    || components.some((component) => !component || component === "." || component === ".." || component.includes("\\") || component.includes("\0"))
  ) {
    throw new Error(`local Application package '${packageId}' has invalid interface import '${uri}' at ${location}`);
  }
  return { uri, owner, relativePath: components.join("/"), location };
}

function snapshotFile(member, relativePath) {
  return member.files.find((file) => snapshotRelativePath(file.relative_path) === relativePath);
}

function snapshotDirectoryEntry(member, relativePath) {
  return member.directories.some((directory) => snapshotRelativePath(directory) === relativePath);
}

function snapshotRelativePath(value) {
  return value.split(path.sep).join("/");
}

function jsonPointerToken(value) {
  return value.replaceAll("~", "~0").replaceAll("/", "~1");
}

async function readPackageState(roots, manifest, source) {
  const receiptResult = await readReceipt(roots, manifest.id);
  const activeMembers = [];
  let present = 0;
  for (const member of orderedMembers(manifest.members)) {
    const target = memberPath(roots.activeRegistryRoot, member);
    if (!await pathExists(target)) {
      activeMembers.push({ ...member, target, present: false, digest: "" });
      continue;
    }
    await requirePlainDirectory(path.dirname(target), path.dirname(target));
    present += 1;
    const snapshot = await snapshotDirectory(target);
    validateMemberDataPaths(member, snapshot);
    const baseline = receiptResult?.members.find(candidate => memberKey(candidate) === memberKey(member));
    const deferred = new Set();
    for (const [relative, entry] of configurationEvolution(member, baseline)) {
      const priorPointers = new Set(entry.prior.fields.map(field => field.pointer));
      for (const field of entry.next.fields) if (!priorPointers.has(field.pointer)) deferred.add(`${relative}\0${field.pointer}`);
    }
    activeMembers.push({ ...member, target, present: true, ...await projectMemberConfiguration(snapshot, member.configuration, deferred) });
  }
  const materialized = present === manifest.members.length;
  const partiallyMaterialized = present > 0 && !materialized;
  const installedDigest = present > 0 ? hashJson({
    manifest_digest: source.manifestDigest,
    members: activeMembers.map((member) => ({
      registry_kind: member.registry_kind,
      owner: member.owner,
      digest: member.present ? member.digest : null,
    })),
  }) : "";
  let receiptState = receiptResult ? "valid" : "missing";
  if (receiptResult && (
    receiptResult.source_digest !== source.sourceDigest
    || receiptResult.manifest_digest !== source.manifestDigest
    || receiptResult.package_version !== manifest.version
    || receiptResult.product_kind !== manifest.product_kind
    || receiptResult.product_id !== manifest.product_id
    || stableJson(receiptResult.required_packages ?? []) !== stableJson(manifest.required_packages ?? [])
    || receiptResult.members.length !== source.members.length
    || !materialized
    || receiptResult.members.some((receiptMember) => {
      const active = activeMembers.find((candidate) => (
        candidate.registry_kind === receiptMember.registry_kind && candidate.owner === receiptMember.owner
      ));
      const expected = source.members.find((candidate) => memberKey(candidate) === memberKey(receiptMember));
      if (!active || !expected || receiptMember.target !== memberRelative(active)) return true;
      return !receiptMemberMatchesSource(receiptMember, expected, active);
    })
  )) receiptState = "stale";
  const customizations = source.members.flatMap((member) => {
    const active = activeMembers.find((candidate) => memberKey(candidate) === memberKey(member));
    if (!active?.present) return [];
    return [...member.projectedFiles].flatMap(([file, expected]) => {
      const installed = active.projectedFiles.get(file);
      return expected.fields.flatMap((field, index) => {
        const actual = installed.fields[index];
        if (stableJson(field) === stableJson(actual)) return [];
        return [{ registry_kind: member.registry_kind, owner: member.owner, path: file, pointer: field.pointer,
          source_digest: hashJson(field), installed_digest: hashJson(actual) }];
      });
    });
  });
  return {
    materialized,
    partiallyMaterialized,
    installedDigest,
    activeMembers,
    receipt: receiptResult,
    receiptState,
    customizations,
    localChanges: receiptResult?.schema_version === 2 ? receiptResult.members.flatMap(member => {
      const active = activeMembers.find(candidate => memberKey(candidate) === memberKey(member));
      const baseline = new Map(member.source_files.map(file => [file.path, file]));
      const installed = active?.present ? sourceFileMap(active) : new Map();
      return [...new Set([...baseline.keys(), ...installed.keys()])].sort(compareSnapshotPaths).flatMap(relative => {
        const before = baseline.get(relative), current = installed.get(relative);
        return sameOwnedFile(before, current) ? [] : [{ registry_kind: member.registry_kind, owner: member.owner, path: relative,
          change: !before ? "added" : !current ? "removed" : "modified",
          source_digest: before?.sha256 ?? null, installed_digest: current?.sha256 ?? null,
          source_mode: before?.mode ?? null, installed_mode: current?.mode ?? null }];
      });
    }) : [],
  };
}

function receiptMemberMatchesSource(receipt, source, active) {
  if (receipt.source_files) {
    if (snapshotDigest(receiptSourceFiles(receipt)) !== source.digest) return false;
    if (active.digest === receipt.digest) return true;
    const baselines = new Map((receipt.json_files ?? []).map(file => [file.path, file]));
    const normalized = active.files.map(file => {
      const relative = snapshotRelativePath(file.relative_path), projected = active.projectedFiles.get(relative), saved = baselines.get(relative);
      return projected && saved?.configuration_digest === projected.configuration_digest && saved.owned_sha256 === projected.owned_sha256
        ? { ...file, size: saved.size, sha256: saved.sha256 } : file;
    });
    const saved = source.data_directories?.length ? receiptMaterializedFiles(receipt) : null;
    if (saved) {
      return snapshotDigest(normalized.filter(file => !isMemberDataPath(source, snapshotRelativePath(file.relative_path))))
        === snapshotDigest(saved.filter(file => !isMemberDataPath(source, file.path)));
    }
    if (!source.configuration?.length || active.contentDigest !== receipt.content_digest) return false;
    return snapshotDigest(normalized) === receipt.digest;
  }
  if (!source.configuration?.length) return receipt.digest === source.digest && active.digest === receipt.digest;
  if (receipt.content_digest !== source.contentDigest || active.contentDigest !== source.contentDigest) return false;
  const baselines = new Map((receipt.json_files ?? []).map((file) => [file.path, file]));
  const jsonFiles = source.files.filter((file) => path.extname(file.relative_path) === ".json");
  if (baselines.size !== jsonFiles.length) return false;
  for (const file of jsonFiles) {
    const relative = snapshotRelativePath(file.relative_path), baseline = baselines.get(relative);
    if (!baseline) return false;
    const projected = source.projectedFiles.get(relative);
    if (projected ? baseline.owned_sha256 !== projected.owned_sha256 || baseline.configuration_digest !== projected.configuration_digest
      : baseline.sha256 !== file.sha256 || baseline.size !== file.size || baseline.owned_sha256 !== undefined) return false;
  }
  const materializedFiles = source.files.map((file) => {
    const relative = snapshotRelativePath(file.relative_path);
    if (!source.projectedFiles.has(relative)) return file;
    const baseline = baselines.get(relative);
    return { ...file, size: baseline.size, sha256: baseline.sha256 };
  });
  return snapshotDigest(materializedFiles) === receipt.digest;
}

function packageCatalogRecord(entry, source, state) {
  return {
    ...entry.manifest,
    manifest_path: entry.file,
    manifest_digest: source.manifestDigest,
    source_kind: "local",
    available: true,
    installed: state.materialized && state.receiptState === "valid",
    materialized: state.materialized,
    partially_materialized: state.partiallyMaterialized,
    receipt_state: state.receiptState,
    receipt_operation_id: state.receipt?.operation_id ?? "",
    receipt_digest: state.receipt ? hashJson(state.receipt) : "",
    source_digest: source.sourceDigest,
    installed_digest: state.installedDigest,
    customized: state.customizations.length > 0,
    customizations: state.customizations,
    locally_modified: state.localChanges.length > 0,
    local_changes: state.localChanges,
    source_file_count: source.fileCount,
    ...(entry.productMetadata ? { product_metadata: entry.productMetadata } : {}),
    member_states: source.members.map((member) => {
      const active = state.activeMembers.find((candidate) => memberKey(candidate) === memberKey(member));
      return {
        registry_kind: member.registry_kind,
        owner: member.owner,
        source_digest: member.digest,
        installed_digest: active?.digest ?? "",
        content_source_digest: member.contentDigest,
        content_installed_digest: active?.contentDigest ?? "",
        materialized: active?.present === true,
      };
    }),
    error: state.partiallyMaterialized
      ? `package '${entry.manifest.id}' is only partially materialized`
      : state.receiptState === "stale"
        ? `package '${entry.manifest.id}' receipt does not match active owner folders`
        : state.materialized && state.receiptState !== "valid"
          ? `package '${entry.manifest.id}' has materialized owner folders without a valid install receipt`
        : "",
  };
}

function packCatalogRecord(entry, packageById) {
  const components = entry.pack.components.map((component) => {
    const packageRecord = packageById.get(component.package_id);
    return {
      ...component,
      product_kind: packageRecord?.product_kind ?? "",
      product_id: packageRecord?.product_id ?? "",
      name: packageRecord?.display?.name ?? component.package_id,
      available: packageRecord?.available === true,
      installed: packageRecord?.installed === true,
      materialized: packageRecord?.materialized === true,
      partially_materialized: packageRecord?.partially_materialized === true,
      receipt_state: packageRecord?.receipt_state ?? "missing",
      receipt_digest: packageRecord?.receipt_digest ?? "",
      source_digest: packageRecord?.source_digest ?? "",
      installed_digest: packageRecord?.installed_digest ?? "",
      error: packageRecord?.error ?? "",
    };
  });
  return {
    ...entry.pack,
    manifest_path: entry.file,
    manifest_digest: entry.packDigest,
    components,
    required_components_materialized: components
      .filter((component) => component.selection !== "recommended")
      .every((component) => component.installed && component.receipt_state === "valid"),
  };
}

async function snapshotDirectory(rootValue, options = {}) {
  const root = await realPlainDirectory(rootValue, rootValue);
  const inventory = [];
  const files = [];
  const directories = [""];
  const excludedEntries = [];
  const visit = async (directory, relativeDirectory) => {
    const entries = (await fs.readdir(directory, { withFileTypes: true }))
      .sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const absolute = path.join(directory, entry.name);
      const relative = path.join(relativeDirectory, entry.name);
      if (SOURCE_EXCLUDES.has(entry.name)) { excludedEntries.push(absolute); continue; }
      const stat = await fs.lstat(absolute);
      if (stat.isSymbolicLink()) throw new Error(`local package contains a symbolic link: ${absolute}`);
      if (stat.isDirectory()) {
        directories.push(relative);
        await visit(absolute, relative);
        continue;
      }
      if (!stat.isFile()) throw new Error(`local package contains a non-regular entry: ${absolute}`);
      inventory.push({ path: absolute, relative_path: relative, stat });
    }
  };
  await visit(root, "");
  const fingerprint = hashJson({ directories, files: inventory.map((file) => ({
    path: file.relative_path, size: file.stat.size, mode: file.stat.mode, mtime: file.stat.mtimeMs, ctime: file.stat.ctimeMs, inode: file.stat.ino,
  })) });
  const cached = options.cacheKey ? sourceSnapshotCache.get(options.cacheKey) : null;
  if (cached?.fingerprint === fingerprint) return cached.snapshot;
  for (const { path: absolute, relative_path: relative, stat } of inventory) {
    const handle = await fs.open(absolute, fsSync.constants.O_RDONLY | fsSync.constants.O_NOFOLLOW);
    let content;
    try { content = await hashFileHandle(handle); }
    finally { await handle.close(); }
    const current = await fs.lstat(absolute);
    if (content.size !== stat.size || current.size !== stat.size || current.mtimeMs !== stat.mtimeMs
      || current.ctimeMs !== stat.ctimeMs || current.mode !== stat.mode || current.ino !== stat.ino || !current.isFile()) {
      throw new Error(`local package file changed while reading: ${absolute}`);
    }
    files.push({ path: absolute, relative_path: relative, size: stat.size, mode: stat.mode & 0o777, sha256: content.sha256 });
  }
  const digest = snapshotDigest(files);
  const snapshot = { root, directories, files, digest, excludedEntries };
  if (options.cacheKey) {
    if (sourceSnapshotCache.size >= 256) sourceSnapshotCache.delete(sourceSnapshotCache.keys().next().value);
    sourceSnapshotCache.set(options.cacheKey, { fingerprint, snapshot });
  }
  return snapshot;
}

function assertNoExcludedActiveEntries(snapshot) {
  if (snapshot.excludedEntries.length) {
    throw new Error(`Package owner '${snapshot.root}' has excluded active entries without receipt ownership proof; replacement/removal is blocked: ${snapshot.excludedEntries.join(", ")}`);
  }
}

async function installResolvedPackages(roots, entries, operation) {
  const prepared = [];
  for (const entry of entries) {
    reportProgress(operation.onProgress, {
      operationId: operation.operationId,
      packageId: entry.manifest.id,
      operation: "install",
      phase: "verifying",
      percent: 0,
    });
    if (entry.manifest.release_status === "system") {
      throw new Error(`System package '${entry.manifest.id}' cannot be installed independently`);
    }
    const source = await snapshotPackageSource(roots, entry.manifest, { fresh: true });
    await verifyPackageDefinition(entry);
    if (operation.expected[entry.manifest.id] !== source.sourceDigest) {
      throw new Error(`local package '${entry.manifest.id}' changed before installation`);
    }
    const state = await readPackageState(roots, entry.manifest, source);
    if (state.materialized && state.receiptState === "valid") {
      prepared.push({ entry, source, state, alreadyInstalled: true });
      continue;
    }
    if (state.materialized || state.partiallyMaterialized || state.receipt) {
      throw new Error(`local package '${entry.manifest.id}' has ambiguous active ownership and requires repair`);
    }
    prepared.push({ entry, source, state, alreadyInstalled: false });
  }
  const pending = prepared.filter((candidate) => !candidate.alreadyInstalled);
  if (pending.length === 0) {
    return {
      action: "install_packages",
      installed: true,
      changed: false,
      packageIds: entries.map((entry) => entry.manifest.id),
    };
  }
  await assertPackageMutationAllowed(roots, operation.assertMutable, pending.map((candidate) => candidate.entry.manifest.id));
  assertNoRepeatedTransactionOwners(pending);
  const receipts = pending.map(({ entry, source }) => packageReceipt(
    entry.manifest,
    source,
    operation.operationId,
  ));
  const moves = pending.flatMap(({ entry, source }) => installationMembers(source).map((member) => ({
    package_id: entry.manifest.id,
    registry_kind: member.registry_kind,
    owner: member.owner,
    target: memberRelative(member),
    stage: transactionRelative(member, operation.operationId, "stage"),
    backup: transactionRelative(member, operation.operationId, "backup"),
    digest: member.digest,
    state: "planned",
  })));
  let journal = {
    schema: "octopus.local_package_transaction",
    schema_version: 1,
    operation: "install",
    operation_id: operation.operationId,
    ...(operation.packId ? { pack_id: operation.packId } : {}),
    phase: "prepared",
    receipts,
    moves,
  };
  const transactionRoot = await createTransactionRoot(roots, operation.operationId, journal);
  const journalPath = path.join(transactionRoot, "journal.json");
  try {
    const totalFiles = pending.reduce((total, candidate) => total + candidate.source.fileCount, 0);
    let copiedFiles = 0;
    for (const [index, move] of journal.moves.entries()) {
      const sourceMember = pending
        .find((candidate) => candidate.entry.manifest.id === move.package_id)
        .source.members.find((member) => memberKey(member) === `${move.registry_kind}/${move.owner}`);
      if (!sourceMember) throw new Error(`transaction lost source member '${move.registry_kind}/${move.owner}'`);
      const stage = resolveTransactionPath(roots, move.stage);
      const target = resolveTransactionPath(roots, move.target);
      const backup = resolveTransactionPath(roots, move.backup);
      if (await pathExists(stage) || await pathExists(backup) || await pathExists(target)) {
        throw new Error(`package target or transaction path already exists for '${move.target}'`);
      }
      await fs.mkdir(path.dirname(stage), { recursive: true, mode: 0o755 });
      await requirePlainDirectory(path.dirname(stage), path.dirname(stage));
      await stagePackageSnapshot(sourceMember, stage, transactionRoot, index, () => {
        copiedFiles += 1;
        reportProgress(operation.onProgress, {
          operationId: operation.operationId,
          packageId: move.package_id,
          operation: "install",
          phase: "copying",
          percent: totalFiles ? Math.round(copiedFiles / totalFiles * 70) : 70,
        });
      });
      const staged = await snapshotDirectory(stage);
      if (staged.digest !== move.digest) throw new Error(`staged owner '${move.target}' does not match its source`);
      journal.moves[index].state = "staged";
      await writeJournal(journalPath, journal);
    }
    for (const candidate of pending) {
      await verifyPackageDefinition(candidate.entry);
      const refreshed = await snapshotPackageSource(roots, candidate.entry.manifest, { fresh: true });
      if (refreshed.sourceDigest !== candidate.source.sourceDigest) {
        throw new Error(`local package '${candidate.entry.manifest.id}' changed while staging`);
      }
    }
    journal.phase = "committing";
    await writeJournal(journalPath, journal);
    for (const [index, move] of journal.moves.entries()) {
      const stage = resolveTransactionPath(roots, move.stage);
      const target = resolveTransactionPath(roots, move.target);
      if (await pathExists(target)) throw new Error(`package owner target appeared during commit: ${move.target}`);
      await renameDirectoryExclusive(stage, target);
      await syncDirectory(path.dirname(target));
      journal.moves[index].state = "materialized";
      await writeJournal(journalPath, journal);
      reportProgress(operation.onProgress, {
        operationId: operation.operationId,
        packageId: move.package_id,
        operation: "install",
        phase: "installing",
        percent: Math.round(72 + (index + 1) / journal.moves.length * 22),
      });
    }
    await verifyMaterializedMoves(roots, journal.moves);
    journal.phase = "owners_materialized";
    await writeJournal(journalPath, journal);
    for (const entry of pending) {
      reportProgress(operation.onProgress, {
        operationId: operation.operationId,
        packageId: entry.entry.manifest.id,
        operation: "install",
        phase: "finalizing",
        percent: 96,
      });
    }
    await verifyMaterializedMoves(roots, journal.moves);
    await writeReceipts(roots, journal.receipts);
    journal.phase = "receipts_written";
    await writeJournal(journalPath, journal);
    journal.phase = "committed";
    await writeJournal(journalPath, journal);
    await removeTransactionRoot(transactionRoot, roots.transactionsRoot);
  } catch (error) {
    let recovery;
    try {
      recovery = await recoverTransaction(roots, transactionRoot, journalPath);
    } catch (recoveryError) {
      throw new Error(`${errorMessage(error)}; package transaction recovery failed closed: ${errorMessage(recoveryError)}`, { cause: error });
    }
    if (recovery !== "committed") throw error;
  }
  for (const entry of entries) {
    reportProgress(operation.onProgress, {
      operationId: operation.operationId,
      packageId: entry.manifest.id,
      operation: "install",
      phase: "complete",
      percent: 100,
    });
  }
  return {
    action: "install_packages",
    installed: true,
    changed: true,
    packageIds: entries.map((entry) => entry.manifest.id),
  };
}

function installationMembers(source) {
  const worker = (member) => member.registry_kind === "interface" && member.files.some((file) => file.relative_path === "policy.json");
  return [...source.members].sort((left, right) => Number(worker(left)) - Number(worker(right)));
}

async function repairResolvedPackages(roots, entries, operation) {
  const prepared = [];
  for (const entry of entries) {
    reportProgress(operation.onProgress, {
      operationId: operation.operationId,
      packageId: entry.manifest.id,
      operation: "repair",
      phase: "verifying",
      percent: 0,
    });
    if (entry.manifest.release_status === "system") {
      throw new Error(`System package '${entry.manifest.id}' cannot be repaired independently`);
    }
    const expected = operation.expectedRepairs[entry.manifest.id];
    const source = await snapshotPackageSource(roots, entry.manifest, { fresh: true });
    await verifyPackageDefinition(entry);
    if (expected.sourceDigest !== source.sourceDigest) {
      throw new Error(`local package '${entry.manifest.id}' source changed before repair`);
    }
    const state = await readPackageState(roots, entry.manifest, source);
    const currentReceiptDigest = state.receipt ? hashJson(state.receipt) : null;
    const alreadyCurrent = state.receiptState === "valid"
      && state.receipt?.operation_id === operation.operationId;
    if (alreadyCurrent) {
      prepared.push({ entry, source, state, alreadyCurrent: true });
      continue;
    }
    if ((!state.materialized || state.partiallyMaterialized) && !operation.allowOwnerAdditions) {
      throw new Error(`local package '${entry.manifest.id}' repair requires every declared active owner`);
    }
    if (expected.installedDigest !== state.installedDigest) {
      throw new Error(`local package '${entry.manifest.id}' active owners changed before repair`);
    }
    if (state.receiptState !== expected.receiptState || currentReceiptDigest !== expected.receiptDigest) {
      throw new Error(`local package '${entry.manifest.id}' receipt changed before repair`);
    }
    const materialization = await (operation.prepareMaterialization ?? preparePackageRepair)(entry.manifest, source, state);
    prepared.push({ entry, source, state, materialization, alreadyCurrent: false });
  }

  const pending = prepared.filter((candidate) => !candidate.alreadyCurrent);
  if (pending.length === 0) {
    return {
      action: "repair_packages",
      installed: true,
      changed: false,
      packageIds: entries.map((entry) => entry.manifest.id),
    };
  }

  assertNoRepeatedTransactionOwners(pending);
  const replacements = pending.filter((candidate) => (
    candidate.state.installedDigest !== hashJson({ manifest_digest: candidate.source.manifestDigest,
      members: candidate.materialization.members.map((member) => ({ registry_kind: member.registry_kind, owner: member.owner, digest: member.digest })) })
  ));
  for (const candidate of replacements) {
    for (const member of candidate.state.activeMembers.filter(member => member.present)) assertNoExcludedActiveEntries(member);
  }
  const receipts = pending.map(({ entry, source, materialization }) => packageReceipt(
    entry.manifest,
    source,
    operation.operationId,
    materialization,
  ));
  const previousReceipts = pending.flatMap(({ state }) => state.receipt ? [state.receipt] : []);

  const moves = replacements.flatMap(({ entry, materialization, state }) => installationMembers(materialization).map((member) => {
    const active = state.activeMembers.find((candidate) => memberKey(candidate) === memberKey(member));
    const prior = state.receipt?.members.find(candidate => memberKey(candidate) === memberKey(member));
    const added = operation.allowOwnerAdditions === true && !prior && active?.present === false;
    if (!added && (!active?.present || !DIGEST.test(active.digest))) {
      throw new Error(`local package '${entry.manifest.id}' repair lost active owner '${memberKey(member)}'`);
    }
    return {
      package_id: entry.manifest.id,
      registry_kind: member.registry_kind,
      owner: member.owner,
      target: memberRelative(member),
      stage: transactionRelative(member, operation.operationId, "stage"),
      backup: transactionRelative(member, operation.operationId, "backup"),
      digest: member.digest,
      previous_digest: added ? null : active.digest,
      state: "planned",
    };
  }));
  let journal = {
    schema: "octopus.local_package_transaction",
    schema_version: 2,
    operation: "repair",
    operation_id: operation.operationId,
    phase: "prepared",
    receipts,
    previous_receipts: previousReceipts,
    moves,
  };
  const transactionRoot = await createTransactionRoot(roots, operation.operationId, journal);
  const journalPath = path.join(transactionRoot, "journal.json");
  try {
    const totalFiles = replacements.reduce((total, candidate) => total + candidate.materialization.fileCount, 0);
    let copiedFiles = 0;
    for (const [index, move] of journal.moves.entries()) {
      const sourceMember = replacements
        .find((candidate) => candidate.entry.manifest.id === move.package_id)
        .materialization.members.find((member) => memberKey(member) === `${move.registry_kind}/${move.owner}`);
      if (!sourceMember) throw new Error(`repair transaction lost source member '${move.registry_kind}/${move.owner}'`);
      const stage = resolveTransactionPath(roots, move.stage);
      const backup = resolveTransactionPath(roots, move.backup);
      if (await pathExists(stage) || await pathExists(backup)
        || move.previous_digest === null && await pathExists(resolveTransactionPath(roots, move.target))) {
        throw new Error(`package repair transaction path already exists for '${move.target}'`);
      }
      await fs.mkdir(path.dirname(stage), { recursive: true, mode: 0o755 });
      await requirePlainDirectory(path.dirname(stage), path.dirname(stage));
      await stagePackageSnapshot(sourceMember, stage, transactionRoot, index, () => {
        copiedFiles += 1;
        reportProgress(operation.onProgress, {
          operationId: operation.operationId,
          packageId: move.package_id,
          operation: "repair",
          phase: "copying",
          percent: totalFiles ? Math.round(copiedFiles / totalFiles * 55) : 55,
        });
      });
      const staged = await snapshotDirectory(stage);
      if (staged.digest !== move.digest) throw new Error(`staged repair owner '${move.target}' does not match its authored content`);
      journal.moves[index].state = "staged";
      await writeJournal(journalPath, journal);
    }
    for (const candidate of pending) {
      await verifyPackageDefinition(candidate.entry);
      const refreshed = await snapshotPackageSource(roots, candidate.entry.manifest, { fresh: true });
      if (refreshed.sourceDigest !== candidate.source.sourceDigest) {
        throw new Error(`local package '${candidate.entry.manifest.id}' source changed while staging repair`);
      }
    }
    await assertPackageMutationAllowed(roots, operation.assertMutable, pending.map(candidate => candidate.entry.manifest.id));
    journal.phase = "committing";
    await writeJournal(journalPath, journal);
    for (const [index, move] of journal.moves.entries()) {
      const stage = resolveTransactionPath(roots, move.stage);
      const target = resolveTransactionPath(roots, move.target);
      const backup = resolveTransactionPath(roots, move.backup);
      if (await pathExists(backup)) throw new Error(`package repair backup appeared during update: ${move.backup}`);
      if (move.previous_digest === null) {
        if (await pathExists(target)) throw new Error(`package owner target appeared during update: ${move.target}`);
      } else {
        const current = await snapshotDirectory(target);
        if (current.digest !== move.previous_digest) {
          throw new Error(`package owner '${move.target}' changed during repair`);
        }
        assertNoExcludedActiveEntries(current);
        await renameDirectoryExclusive(target, backup);
        await syncDirectory(path.dirname(target));
        reportProgress(operation.onProgress, {
          operationId: operation.operationId,
          packageId: move.package_id,
          operation: "repair",
          phase: "backing_up",
          percent: Math.round(56 + index / journal.moves.length * 34),
        });
        journal.moves[index].state = "backed_up";
        await writeJournal(journalPath, journal);
      }
      await renameDirectoryExclusive(stage, target);
      await syncDirectory(path.dirname(target));
      reportProgress(operation.onProgress, {
        operationId: operation.operationId,
        packageId: move.package_id,
        operation: "repair",
        phase: "materializing",
        percent: Math.round(57 + index / journal.moves.length * 34),
      });
      journal.moves[index].state = "materialized";
      await writeJournal(journalPath, journal);
      reportProgress(operation.onProgress, {
        operationId: operation.operationId,
        packageId: move.package_id,
        operation: "repair",
        phase: "installing",
        percent: Math.round(58 + (index + 1) / journal.moves.length * 34),
      });
    }
    await verifyMaterializedMoves(roots, journal.moves);
    await verifyRepairReceiptOwners(roots, journal.receipts);
    journal.phase = "owners_materialized";
    await writeJournal(journalPath, journal);
    for (const candidate of pending) {
      reportProgress(operation.onProgress, {
        operationId: operation.operationId,
        packageId: candidate.entry.manifest.id,
        operation: "repair",
        phase: "finalizing",
        percent: 96,
      });
    }
    await reconcileRepairReceipts(roots, journal);
    for (const candidate of pending) {
      reportProgress(operation.onProgress, {
        operationId: operation.operationId,
        packageId: candidate.entry.manifest.id,
        operation: "repair",
        phase: "receipting",
        percent: 98,
      });
    }
    journal.phase = "receipts_written";
    await writeJournal(journalPath, journal);
    await removeRepairBackups(roots, journal.moves);
    journal.phase = "committed";
    await writeJournal(journalPath, journal);
    await removeTransactionRoot(transactionRoot, roots.transactionsRoot);
  } catch (error) {
    let recovery;
    try {
      recovery = await recoverTransaction(roots, transactionRoot, journalPath);
    } catch (recoveryError) {
      throw new Error(`${errorMessage(error)}; package repair recovery failed closed: ${errorMessage(recoveryError)}`, { cause: error });
    }
    if (recovery !== "committed") throw error;
  }
  for (const entry of entries) {
    reportProgress(operation.onProgress, {
      operationId: operation.operationId,
      packageId: entry.manifest.id,
      operation: "repair",
      phase: "complete",
      percent: 100,
    });
  }
  return {
    action: "repair_packages",
    installed: true,
    changed: true,
    packageIds: entries.map((entry) => entry.manifest.id),
  };
}

async function preparePackageRepair(manifest, source, state) {
  if (state.receipt?.members.some(baseline => {
    const member = source.members.find(candidate => memberKey(candidate) === memberKey(baseline));
    return member && configurationEvolution(member, baseline).size;
  })) throw new Error(`local package '${manifest.id}' configuration ownership evolution requires adopt_package_updates`);
  const changedData = state.localChanges.some(change => {
    const member = source.members.find(candidate => memberKey(candidate) === memberKey(change));
    return member && isMemberDataPath(member, change.path);
  });
  const unprovedData = state.receipt?.schema_version !== 2 && source.members.some(member => {
    if (!member.data_directories?.length) return false;
    const active = state.activeMembers.find(candidate => memberKey(candidate) === memberKey(member));
    return snapshotDigest(member.files.filter(file => isMemberDataPath(member, snapshotRelativePath(file.relative_path))))
      !== snapshotDigest((active?.files ?? []).filter(file => isMemberDataPath(member, snapshotRelativePath(file.relative_path))));
  });
  if (changedData || unprovedData) {
    throw new Error(`local package '${manifest.id}' has local changes in declared data directories; repair cannot discard authored files`);
  }
  if (state.receipt?.schema_version === 2 && state.localChanges.length && state.receipt.members.some(receiptHasLocalChanges)) {
    throw new Error(`local package '${manifest.id}' contains preserved local changes; repair cannot discard authored files`);
  }
  const members = [];
  for (const member of source.members) {
    const active = state.activeMembers.find((candidate) => memberKey(candidate) === memberKey(member));
    const receipt = state.receipt?.members.find((candidate) => memberKey(candidate) === memberKey(member));
    const baselines = new Map((receipt?.source_files ?? receipt?.json_files ?? []).map((file) => [file.path, file]));
    for (const file of active.files.filter((file) => path.extname(file.relative_path).toLowerCase() === ".json")) {
      const relative = snapshotRelativePath(file.relative_path);
      const expected = snapshotFile(member, relative);
      const configured = member.projectedFiles.get(relative);
      const projected = active.projectedFiles.get(relative);
      const baseline = baselines.get(relative);
      const matchesSource = configured ? projected.owned_sha256 === configured.owned_sha256 : expected?.sha256 === file.sha256;
      const matchesBaseline = !receipt?.source_files && receipt?.digest === active.digest || baseline && (file.sha256 === baseline.sha256
        || configured && baseline.configuration_digest === projected.configuration_digest && baseline.owned_sha256 === projected.owned_sha256);
      if (!matchesSource && !matchesBaseline) {
        throw new Error(`local package '${manifest.id}' has unowned JSON changes at '${memberKey(member)}/${relative}'; author exact configuration fields or review the descriptor before repair`);
      }
    }
    const files = [];
    const projectedFiles = new Map(member.projectedFiles);
    for (const file of member.files) {
      const relative = snapshotRelativePath(file.relative_path);
      const configuration = member.projectedFiles.get(relative);
      if (!configuration) { files.push(file); continue; }
      const merged = mergeConfiguredFile(file, configuration, active.projectedFiles.get(relative), snapshotFile(active, relative));
      files.push(merged.file);
      projectedFiles.set(relative, merged.projected);
    }
    members.push({ ...member, files, digest: snapshotDigest(files), projectedFiles });
  }
  return { ...source, members };
}

function receiptHasLocalChanges(member) {
  const materialized = new Map((member.json_files ?? []).map(file => [file.path, file]));
  return snapshotDigest(receiptSourceFiles(member).map(file => {
    const saved = materialized.get(file.path);
    return file.configuration_digest && saved?.configuration_digest === file.configuration_digest && saved.owned_sha256 === file.owned_sha256
      ? { ...file, size: saved.size, sha256: saved.sha256 } : file;
  })) !== member.digest;
}

function mergeConfiguredFile(file, configuration, installed, installedFile) {
  if (!installed) return { file, projected: configuration };
  const document = structuredClone(configuration.document);
  for (const field of installed.fields) {
    const slot = configurationSlot(document, field.pointer);
    if (field.present) slot.parent[slot.key] = structuredClone(field.value);
    else delete slot.parent[slot.key];
  }
  if (stableJson(document) === stableJson(configuration.document)) return { file, projected: configuration };
  const body = Buffer.from(`${JSON.stringify(document, null, 2)}\n`);
  if (body.byteLength > MAX_CONFIGURATION_BYTES) throw new Error(`package repair configuration '${file.relative_path}' exceeds its bound`);
  return { file: { ...file, body, size: body.byteLength, sha256: sha256Bytes(body),
    input_sha256: file.sha256, input_size: file.size, configuration_input: { ...installedFile } },
    projected: { ...configuration, document, fields: installed.fields } };
}

async function verifyPackageDefinition(entry) {
  const expected = entry.definitionDigest ?? entry.manifestDigest;
  if ((await readJsonDocument(entry.file, `local package definition '${entry.manifest.id}'`)).digest !== expected) {
    throw new Error(`local package '${entry.manifest.id}' definition changed during the operation`);
  }
}

async function removeResolvedPackages(roots, entries, operation) {
  const prepared = [];
  for (const entry of entries) {
    reportProgress(operation.onProgress, {
      operationId: operation.operationId,
      packageId: entry.manifest.id,
      operation: "remove",
      phase: "verifying",
      percent: 0,
    });
    if (entry.manifest.release_status === "system") {
      throw new Error(`System package '${entry.manifest.id}' cannot be removed independently`);
    }
    const source = await snapshotPackageSource(roots, entry.manifest, { fresh: true });
    const state = await readPackageState(roots, entry.manifest, source);
    if (state.localChanges.length) throw new Error(`local package '${entry.manifest.id}' has local changes; removal cannot delete authored files`);
    if (!state.materialized || state.receiptState !== "valid" || !state.receipt) {
      throw new Error(`local package '${entry.manifest.id}' has no valid installed receipt`);
    }
    if (operation.expected[entry.manifest.id] !== state.installedDigest) {
      throw new Error(`local package '${entry.manifest.id}' changed before removal`);
    }
    for (const member of state.activeMembers) assertNoExcludedActiveEntries(member);
    prepared.push({ entry, source, state });
  }
  assertNoRepeatedTransactionOwners(prepared);
  const receipts = prepared.map((candidate) => candidate.state.receipt);
  const moves = prepared.flatMap(({ entry, state }) => [...state.activeMembers]
    .sort((left, right) => (
      Number(right.primary === true) - Number(left.primary === true)
      || memberKey(left).localeCompare(memberKey(right))
    ))
    .map((member) => ({
    package_id: entry.manifest.id,
    registry_kind: member.registry_kind,
    owner: member.owner,
    target: memberRelative(member),
    stage: transactionRelative(member, operation.operationId, "stage"),
    backup: transactionRelative(member, operation.operationId, "backup"),
    digest: member.digest,
    state: "planned",
  })));
  let journal = {
    schema: "octopus.local_package_transaction",
    schema_version: 1,
    operation: "remove",
    operation_id: operation.operationId,
    phase: "prepared",
    receipts,
    moves,
  };
  const transactionRoot = await createTransactionRoot(roots, operation.operationId, journal);
  const journalPath = path.join(transactionRoot, "journal.json");
  try {
    journal.phase = "committing";
    await writeJournal(journalPath, journal);
    for (const [index, move] of journal.moves.entries()) {
      const target = resolveTransactionPath(roots, move.target);
      const backup = resolveTransactionPath(roots, move.backup);
      if (await pathExists(backup)) throw new Error(`package removal backup already exists: ${move.backup}`);
      const current = await snapshotDirectory(target);
      if (current.digest !== move.digest) throw new Error(`package owner '${move.target}' changed during removal`);
      assertNoExcludedActiveEntries(current);
      await renameDirectoryExclusive(target, backup);
      await syncDirectory(path.dirname(target));
      journal.moves[index].state = "backed_up";
      await writeJournal(journalPath, journal);
      reportProgress(operation.onProgress, {
        operationId: operation.operationId,
        packageId: move.package_id,
        operation: "remove",
        phase: "removing",
        percent: Math.round(5 + (index + 1) / journal.moves.length * 85),
      });
    }
    journal.phase = "owners_removed";
    await writeJournal(journalPath, journal);
    for (const entry of prepared) {
      reportProgress(operation.onProgress, {
        operationId: operation.operationId,
        packageId: entry.entry.manifest.id,
        operation: "remove",
        phase: "finalizing",
        percent: 94,
      });
    }
    for (const move of journal.moves) {
      if (await pathExists(resolveTransactionPath(roots, move.target))
        || await directoryDigestIfPresent(resolveTransactionPath(roots, move.backup)) !== move.digest) {
        throw new Error(`package owner '${move.target}' changed before removal commit`);
      }
    }
    await removeReceipts(roots, journal.receipts);
    journal.phase = "receipts_written";
    await writeJournal(journalPath, journal);
    for (const move of journal.moves) {
      await removeMatchingDirectory(resolveTransactionPath(roots, move.backup), move.digest, "removal backup");
    }
    journal.phase = "committed";
    await writeJournal(journalPath, journal);
    await removeTransactionRoot(transactionRoot, roots.transactionsRoot);
  } catch (error) {
    let recovery;
    try {
      recovery = await recoverTransaction(roots, transactionRoot, journalPath);
    } catch (recoveryError) {
      throw new Error(`${errorMessage(error)}; package transaction recovery failed closed: ${errorMessage(recoveryError)}`, { cause: error });
    }
    if (recovery !== "committed") throw error;
  }
  for (const entry of entries) {
    reportProgress(operation.onProgress, {
      operationId: operation.operationId,
      packageId: entry.manifest.id,
      operation: "remove",
      phase: "complete",
      percent: 100,
    });
  }
  return {
    action: "remove_packages",
    installed: false,
    changed: true,
    packageIds: entries.map((entry) => entry.manifest.id),
  };
}

async function recoverLocalPackageTransactionsUnlocked(roots) {
  const entries = await fs.readdir(roots.transactionsRoot, { withFileTypes: true });
  const recovered = [];
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    if (!entry.isDirectory()) {
      throw new Error(`local package transaction root contains an invalid entry '${entry.name}'`);
    }
    if (PREPARED_TRANSACTION_ROOT.test(entry.name) || CLEANUP_TRANSACTION_ROOT.test(entry.name)) {
      await removeUnpublishedTransactionRoot(path.join(roots.transactionsRoot, entry.name), roots.transactionsRoot);
      continue;
    }
    if (!SAFE_OPERATION_ID.test(entry.name)) {
      throw new Error(`local package transaction root contains an invalid entry '${entry.name}'`);
    }
    const transactionRoot = path.join(roots.transactionsRoot, entry.name);
    const journalPath = path.join(transactionRoot, "journal.json");
    await recoverTransaction(roots, transactionRoot, journalPath);
    recovered.push(entry.name);
  }
  return { recovered };
}

async function recoverTransaction(roots, transactionRoot, journalPath) {
  const document = await readJsonDocument(journalPath, `local package transaction ${journalPath}`);
  const journal = parseTransaction(document.value, journalPath);
  if (path.basename(transactionRoot) !== journal.operation_id) {
    throw new Error(`local package transaction directory disagrees with operation '${journal.operation_id}'`);
  }
  for (const move of journal.moves) validateMovePaths(roots, move, journal.operation_id);
  await assertRegisteredPackageMutationAllowed(roots, transactionPackageIds(journal));
  if (journal.operation === "repair") {
    if (["owners_materialized", "receipts_written", "committed"].includes(journal.phase)) {
      await reconcileRepairOwnersForward(roots, journal);
      await verifyRepairReceiptOwners(roots, journal.receipts);
      await reconcileRepairReceipts(roots, journal);
      if (journal.phase === "owners_materialized") {
        journal.phase = "receipts_written";
        await writeJournal(journalPath, journal);
      }
      await removeRepairBackups(roots, journal.moves);
      await removeTransactionRoot(transactionRoot, roots.transactionsRoot);
      return "committed";
    }
    const rolling = { ...journal, phase: "rolling_back" };
    await writeJournal(journalPath, rolling);
    const refusals = [];
    for (const move of [...journal.moves].reverse()) {
      try { await rollbackRepairOwner(roots, move); }
      catch (error) { refusals.push(errorMessage(error)); }
    }
    if (refusals.length) throw new Error(refusals.join("; "));
    await removeTransactionRoot(transactionRoot, roots.transactionsRoot);
    return "rolled_back";
  }
  if (journal.operation === "install") {
    if (["owners_materialized", "receipts_written", "committed"].includes(journal.phase)) {
      await verifyMaterializedMoves(roots, journal.moves);
      await writeReceipts(roots, journal.receipts);
      for (const move of journal.moves) {
        await removeInstallStage(resolveTransactionPath(roots, move.stage), move.digest, move.state);
      }
      await removeTransactionRoot(transactionRoot, roots.transactionsRoot);
      return "committed";
    }
    const rolling = { ...journal, phase: "rolling_back" };
    await writeJournal(journalPath, rolling);
    for (const move of [...journal.moves].reverse()) {
      const target = resolveTransactionPath(roots, move.target);
      const stage = resolveTransactionPath(roots, move.stage);
      const observed = await repairOwnerDigests({ target, stage, backup: resolveTransactionPath(roots, move.backup) });
      if (observed.backup !== null || observed.target !== null && (
        observed.target !== move.digest || observed.stage !== null || !["staged", "materialized"].includes(move.state)
      )) {
        throw new Error(`install rollback found an unowned target or stage '${move.target}'`);
      }
      if (observed.target !== null) await removeMatchingDirectory(target, move.digest, "materialized install owner");
      await removeInstallStage(stage, move.digest, move.state);
    }
    await removeOperationReceipts(roots, journal.receipts, journal.operation_id);
    await removeTransactionRoot(transactionRoot, roots.transactionsRoot);
    return "rolled_back";
  }
  if (["owners_removed", "receipts_written", "committed"].includes(journal.phase)) {
    for (const move of journal.moves) {
      if (await pathExists(resolveTransactionPath(roots, move.target))) {
        throw new Error(`remove recovery found materialized target '${move.target}' after owners_removed`);
      }
    }
    await removeReceipts(roots, journal.receipts);
    for (const move of journal.moves) {
      await removeMatchingDirectory(
        resolveTransactionPath(roots, move.backup),
        move.digest,
        "removal backup",
        true,
      );
    }
    await removeTransactionRoot(transactionRoot, roots.transactionsRoot);
    return "committed";
  }
  const rolling = { ...journal, phase: "rolling_back" };
  await writeJournal(journalPath, rolling);
  for (const move of [...journal.moves].reverse()) {
    const target = resolveTransactionPath(roots, move.target);
    const backup = resolveTransactionPath(roots, move.backup);
    const observed = await repairOwnerDigests({ target, stage: resolveTransactionPath(roots, move.stage), backup });
    if (observed.stage !== null) throw new Error(`remove rollback found an unowned stage '${move.stage}'`);
    if (observed.target === null && observed.backup === move.digest) {
      await restoreRepairBackup(target, backup, move.digest);
    } else if (observed.target !== move.digest || observed.backup !== null) {
      throw new Error(`remove rollback found foreign owner state '${move.target}'`);
    }
  }
  await restoreReceipts(roots, journal.receipts);
  await removeTransactionRoot(transactionRoot, roots.transactionsRoot);
  return "rolled_back";
}

function parseTransaction(value, file) {
  if (!validatePackageTransaction(value)) {
    throw schemaError(`local package transaction ${file}`, validatePackageTransaction.errors);
  }
  for (const receipt of value.receipts) parseReceipt(receipt, file);
  for (const receipt of value.previous_receipts ?? []) parseReceipt(receipt, file);
  for (const [label, receipts] of [
    ["receipts", value.receipts],
    ["previous_receipts", value.previous_receipts ?? []],
  ]) {
    const ids = receipts.map((receipt) => receipt.package_id);
    if (new Set(ids).size !== ids.length) throw new Error(`local package transaction repeats ${label}`);
  }
  if (value.operation === "repair") {
    validateRepairJournalContract(value);
  } else {
    validateInstallRemoveJournalContract(value);
  }
  return structuredClone(value);
}

function validateInstallRemoveJournalContract(journal) {
  const installing = journal.operation === "install";
  const terminal = installing ? "owners_materialized" : "owners_removed";
  const complete = [terminal, "receipts_written", "committed"].includes(journal.phase);
  if (!["prepared", "committing", terminal, "receipts_written", "committed", "rolling_back"].includes(journal.phase)) {
    throw new Error(`${journal.operation} transaction has invalid phase '${journal.phase}'`);
  }
  const receipts = new Map(journal.receipts.map((receipt) => [receipt.package_id, receipt]));
  const moved = new Set(), paths = new Set();
  for (const move of journal.moves) {
    const receipt = receipts.get(move.package_id), key = `${move.package_id}:${memberKey(move)}`;
    const member = receipt?.members.find((member) => memberKey(member) === memberKey(move));
    if (!member || member.target !== move.target || moved.has(key)
      || (installing || !member.content_digest) && member.digest !== move.digest) {
      throw new Error(`${journal.operation} move has no exact receipt owner '${key}'`);
    }
    if (installing && receipt.operation_id !== journal.operation_id) throw new Error("install receipt disagrees with transaction operation");
    const states = installing ? ["planned", "staged", "materialized"] : ["planned", "backed_up"];
    if (!states.includes(move.state) || complete && move.state !== (installing ? "materialized" : "backed_up")) {
      throw new Error(`${journal.operation} transaction contains an incomplete move`);
    }
    moved.add(key);
    for (const relative of [move.target, move.stage, move.backup]) {
      if (paths.has(relative)) throw new Error(`${journal.operation} transaction repeats path '${relative}'`);
      paths.add(relative);
    }
  }
  for (const receipt of journal.receipts) {
    if (receipt.members.some((member) => !moved.has(`${receipt.package_id}:${memberKey(member)}`))) {
      throw new Error(`${journal.operation} moves do not cover receipt '${receipt.package_id}' exactly`);
    }
  }
}

function validateRepairJournalContract(journal) {
  const allowedPhases = new Set([
    "prepared",
    "committing",
    "owners_materialized",
    "receipts_written",
    "committed",
    "rolling_back",
  ]);
  if (!allowedPhases.has(journal.phase)) {
    throw new Error(`repair transaction has invalid phase '${journal.phase}'`);
  }
  const receiptsByPackage = new Map(journal.receipts.map((receipt) => [receipt.package_id, receipt]));
  const previousReceipts = journal.previous_receipts ?? [];
  const priorByPackage = new Map(previousReceipts.map(receipt => [receipt.package_id, receipt]));
  if (previousReceipts.some((receipt) => !receiptsByPackage.has(receipt.package_id))) {
    throw new Error("repair transaction has previous receipt without replacement");
  }
  for (const previous of previousReceipts) {
    const next = receiptsByPackage.get(previous.package_id);
    if (next.product_kind !== previous.product_kind || next.product_id !== previous.product_id
      || previous.members.some(member => !next.members.some(candidate => memberKey(candidate) === memberKey(member) && candidate.target === member.target))) {
      throw new Error(`repair receipt '${previous.package_id}' removes or renames a prior owner`);
    }
  }
  for (const receipt of journal.receipts) {
    if (receipt.operation_id !== journal.operation_id) {
      throw new Error(`repair receipt '${receipt.package_id}' disagrees with transaction operation`);
    }
    const memberKeys = new Set();
    const targets = new Set();
    for (const member of receipt.members) {
      const key = memberKey(member);
      if (memberKeys.has(key)) throw new Error(`repair receipt '${receipt.package_id}' repeats owner '${key}'`);
      if (targets.has(member.target)) throw new Error(`repair receipt '${receipt.package_id}' repeats target '${member.target}'`);
      if (member.target !== memberRelative(member)) {
        throw new Error(`repair receipt '${receipt.package_id}' has mismatched target '${member.target}'`);
      }
      memberKeys.add(key);
      targets.add(member.target);
    }
  }

  const moveKeys = new Set();
  const transactionPaths = new Set();
  const movedMembersByPackage = new Map();
  for (const move of journal.moves) {
    const receipt = receiptsByPackage.get(move.package_id);
    if (!receipt) throw new Error(`repair move references unknown package '${move.package_id}'`);
    const key = memberKey(move);
    const moveIdentity = `${move.package_id}\0${key}`;
    if (moveKeys.has(moveIdentity)) throw new Error(`repair transaction repeats owner '${key}'`);
    moveKeys.add(moveIdentity);
    for (const candidate of [move.target, move.stage, move.backup]) {
      if (transactionPaths.has(candidate)) throw new Error(`repair transaction repeats path '${candidate}'`);
      transactionPaths.add(candidate);
    }
    const receiptMember = receipt.members.find((member) => memberKey(member) === key);
    if (!receiptMember || receiptMember.target !== move.target || receiptMember.digest !== move.digest) {
      throw new Error(`repair move disagrees with receipt member '${move.package_id}:${key}'`);
    }
    const previous = priorByPackage.get(move.package_id);
    const priorMember = previous?.members.find(member => memberKey(member) === key);
    if (move.previous_digest === null) {
      if (!previous || priorMember || receipt.schema_version !== 2 || !receiptMember.materialized_files
        || snapshotDigest(receiptSourceFiles(receiptMember)) !== move.digest || move.state === "backed_up") {
        throw new Error(`repair move '${move.package_id}:${key}' has no proven absent new-owner preimage`);
      }
    } else if (previous && !priorMember) {
      throw new Error(`repair move '${move.package_id}:${key}' cannot claim an existing unowned owner`);
    }
    const moved = movedMembersByPackage.get(move.package_id) ?? new Set();
    moved.add(key);
    movedMembersByPackage.set(move.package_id, moved);
  }
  for (const [packageId, moved] of movedMembersByPackage) {
    const receiptMembers = new Set(receiptsByPackage.get(packageId).members.map(memberKey));
    if (moved.size !== receiptMembers.size || [...receiptMembers].some((key) => !moved.has(key))) {
      throw new Error(`repair moves do not cover receipt '${packageId}' exactly`);
    }
  }
  if (["owners_materialized", "receipts_written", "committed"].includes(journal.phase)
    && journal.moves.some((move) => move.state !== "materialized")) {
    throw new Error(`repair transaction phase '${journal.phase}' contains an incomplete move`);
  }
}

function parseReceipt(value, file) {
  if (!validatePackageReceipt(value)) throw schemaError(`local package receipt ${file}`, validatePackageReceipt.errors);
  const members = new Set();
  for (const member of value.members) {
    requiredRegistryKind(member.registry_kind);
    if (members.has(memberKey(member)) || member.target !== memberRelative(member)) throw new Error(`local package receipt '${value.package_id}' has ambiguous owner '${memberKey(member)}'`);
    members.add(memberKey(member));
    const files = new Set();
    for (const entry of member.json_files ?? []) {
      configurationPath(entry.path);
      if (files.has(entry.path)) throw new Error(`local package receipt '${value.package_id}' repeats JSON baseline '${entry.path}'`);
      files.add(entry.path);
    }
    const sourcePaths = new Set();
    for (const entry of member.source_files ?? []) {
      if (entry.path.split("/").some(part => !part || part === "." || part === ".." || part.includes("\\") || part.includes("\0"))
        || sourcePaths.has(entry.path)) throw new Error(`local package receipt '${value.package_id}' has an invalid source path '${entry.path}'`);
      sourcePaths.add(entry.path);
    }
    if (member.configuration) {
      validateConfigurationOwnership(member.configuration, `receipt:${value.package_id}:${memberKey(member)}`, member.registry_kind);
      const declarations = new Map(member.configuration.map(entry => [entry.path, hashJson(entry)]));
      const configured = new Set();
      for (const entry of [...(member.source_files ?? []), ...(member.json_files ?? [])]) {
        if (entry.configuration_digest !== declarations.get(entry.path)) {
          throw new Error(`local package receipt '${value.package_id}' configuration declaration disagrees with '${entry.path}'`);
        }
        if (entry.configuration_digest !== undefined) configured.add(entry.path);
      }
      if (configured.size !== declarations.size) throw new Error(`local package receipt '${value.package_id}' has unproved configuration declarations`);
    }
    if (member.materialized_files) validateReceiptMaterialization(member, value.package_id);
  }
  if (value.schema_version === 2 && hashJson({ manifest_digest: value.manifest_digest,
    members: value.members.map(member => ({ registry_kind: member.registry_kind, owner: member.owner,
      digest: snapshotDigest(receiptSourceFiles(member)) })) }) !== value.source_digest) {
    throw new Error(`local package receipt '${value.package_id}' source baseline does not match its source digest`);
  }
  return structuredClone(value);
}

function validateReceiptMaterialization(member, packageId) {
  const paths = new Map();
  for (const file of member.materialized_files) {
    if (path.posix.isAbsolute(file.path) || path.win32.parse(file.path).root
      || file.path.split("/").some(part => !part || part === "." || part === ".." || /[\\\u0000-\u001f]/.test(part))
      || paths.has(file.path)) {
      throw new Error(`local package receipt '${packageId}' has an invalid materialized path '${file.path}'`);
    }
    paths.set(file.path, file);
  }
  for (const relative of paths.keys()) {
    const components = relative.split("/");
    for (let length = 1; length < components.length; length += 1) {
      if (paths.has(components.slice(0, length).join("/"))) {
        throw new Error(`local package receipt '${packageId}' has overlapping materialized paths at '${relative}'`);
      }
    }
  }
  const files = member.materialized_files.map(file => ({ ...file, relative_path: file.path }));
  if (snapshotDigest(files) !== member.digest) {
    throw new Error(`local package receipt '${packageId}' materialized baseline does not match its saved digest`);
  }
  const jsonFiles = new Map((member.json_files ?? []).map(file => [file.path, file]));
  for (const [relative, file] of paths) {
    if (path.extname(relative) !== ".json") continue;
    const saved = jsonFiles.get(relative);
    if (!saved || saved.sha256 !== file.sha256 || saved.size !== file.size) {
      throw new Error(`local package receipt '${packageId}' JSON baseline disagrees with materialization at '${relative}'`);
    }
    jsonFiles.delete(relative);
  }
  if (jsonFiles.size) throw new Error(`local package receipt '${packageId}' has JSON baselines outside its materialization`);
}

function receiptSourceFiles(member) {
  return member.source_files.map(file => ({ ...file, relative_path: file.path }));
}

function receiptMaterializedFiles(member) {
  if (member.materialized_files) return member.materialized_files.map(file => ({ ...file, relative_path: file.path }));
  if (!member.source_files) return null;
  const jsonFiles = new Map((member.json_files ?? []).map(file => [file.path, file]));
  const files = receiptSourceFiles(member).map(file => {
    const saved = jsonFiles.get(file.path);
    jsonFiles.delete(file.path);
    return saved ? { ...file, sha256: saved.sha256, size: saved.size } : file;
  });
  return !jsonFiles.size && snapshotDigest(files) === member.digest ? files : null;
}

function packageReceipt(manifest, source, operationId, materialization = source) {
  const sourceMembers = new Map(source.members.map(member => [memberKey(member), member]));
  const receipt = {
    schema: "octopus.local_package_receipt",
    schema_version: 2,
    package_id: manifest.id,
    package_version: manifest.version,
    product_kind: manifest.product_kind,
    product_id: manifest.product_id,
    operation_id: operationId,
    manifest_digest: source.manifestDigest,
    source_digest: source.sourceDigest,
    ...(manifest.required_packages?.length ? { required_packages: manifest.required_packages } : {}),
    members: materialization.members.map((member) => ({
      registry_kind: member.registry_kind,
      owner: member.owner,
      target: memberRelative(member),
      digest: member.digest,
      ...(member.configuration?.length ? { configuration: structuredClone(member.configuration) } : {}),
      materialized_files: member.files.map(file => ({ path: snapshotRelativePath(file.relative_path),
        sha256: file.sha256, size: file.size, mode: file.mode })),
      source_files: sourceMembers.get(memberKey(member)).files.map(file => {
        const relative = snapshotRelativePath(file.relative_path);
        const projected = sourceMembers.get(memberKey(member)).projectedFiles.get(relative);
        return { path: relative, sha256: file.sha256, size: file.size, mode: file.mode,
          ...(projected ? { owned_sha256: projected.owned_sha256, configuration_digest: projected.configuration_digest } : {}) };
      }),
      ...(member.configuration?.length ? { content_digest: member.contentDigest } : {}),
      json_files: member.files.filter((file) => path.extname(file.relative_path) === ".json").map((file) => {
        const relative = snapshotRelativePath(file.relative_path);
        const projected = member.projectedFiles.get(relative);
        return { path: relative, sha256: file.sha256, size: file.size,
          ...(projected ? { owned_sha256: projected.owned_sha256, configuration_digest: projected.configuration_digest } : {}) };
      }),
    })),
  };
  return parseReceipt(receipt, `generated:${manifest.id}`);
}

async function readReceipt(roots, packageId) {
  const file = receiptPath(roots, packageId);
  if (!await pathExists(file)) return null;
  const document = await readJsonDocument(file, `local package receipt ${packageId}`);
  const receipt = parseReceipt(document.value, file);
  if (receipt.package_id !== packageId) throw new Error(`local package receipt '${packageId}' has drifted identity`);
  return receipt;
}

async function writeReceipts(roots, receipts) {
  for (const receipt of receipts) {
    const file = receiptPath(roots, receipt.package_id);
    const existing = await readReceipt(roots, receipt.package_id);
    if (existing && stableJson(existing) !== stableJson(receipt)) {
      throw new Error(`local package receipt '${receipt.package_id}' belongs to another operation`);
    }
    if (!existing) await writeJsonAtomic(file, receipt, 0o600);
  }
}

async function reconcileRepairReceipts(roots, journal) {
  const previousByPackage = new Map(journal.previous_receipts.map((receipt) => [receipt.package_id, receipt]));
  const nextIds = new Set(journal.receipts.map((receipt) => receipt.package_id));
  if ([...previousByPackage.keys()].some((packageId) => !nextIds.has(packageId))) {
    throw new Error("repair journal contains a previous receipt without a replacement");
  }
  for (const receipt of journal.receipts) {
    const current = await readReceipt(roots, receipt.package_id);
    if (current && stableJson(current) === stableJson(receipt)) continue;
    const previous = previousByPackage.get(receipt.package_id) ?? null;
    if (
      previous === null ? current !== null : current === null || stableJson(current) !== stableJson(previous)
    ) {
      throw new Error(`local package receipt '${receipt.package_id}' changed during repair`);
    }
    await writeJsonAtomic(receiptPath(roots, receipt.package_id), receipt, 0o600);
  }
}

async function verifyRepairReceiptOwners(roots, receipts) {
  for (const receipt of receipts) {
    for (const member of receipt.members) {
      const target = resolveTransactionPath(roots, member.target);
      const snapshot = await snapshotDirectory(target);
      if (snapshot.digest !== member.digest) {
        throw new Error(`repair receipt owner '${member.target}' changed before commit`);
      }
    }
  }
}

async function restoreReceipts(roots, receipts) {
  for (const receipt of receipts) {
    const existing = await readReceipt(roots, receipt.package_id);
    if (existing && stableJson(existing) !== stableJson(receipt)) {
      throw new Error(`cannot restore changed receipt '${receipt.package_id}'`);
    }
    if (!existing) await writeJsonAtomic(receiptPath(roots, receipt.package_id), receipt, 0o600);
  }
}

async function removeReceipts(roots, receipts) {
  for (const receipt of receipts) {
    const existing = await readReceipt(roots, receipt.package_id);
    if (!existing) continue;
    if (stableJson(existing) !== stableJson(receipt)) throw new Error(`receipt '${receipt.package_id}' changed before removal`);
    await fs.unlink(receiptPath(roots, receipt.package_id));
    await syncDirectory(roots.receiptsRoot);
  }
}

async function removeOperationReceipts(roots, receipts, operationId) {
  for (const receipt of receipts) {
    const existing = await readReceipt(roots, receipt.package_id);
    if (!existing) continue;
    if (existing.operation_id !== operationId || stableJson(existing) !== stableJson(receipt)) {
      throw new Error(`install rollback found a foreign receipt '${receipt.package_id}'`);
    }
    await fs.unlink(receiptPath(roots, receipt.package_id));
    await syncDirectory(roots.receiptsRoot);
  }
}

async function copySnapshot(snapshot, targetRoot, onFile) {
  await fs.mkdir(targetRoot, { recursive: false, mode: 0o755 });
  for (const relativeDirectory of snapshot.directories.filter(Boolean)) {
    await fs.mkdir(path.join(targetRoot, relativeDirectory), { mode: 0o755 });
  }
  for (const file of snapshot.files) {
    const source = await fs.open(file.path, fsSync.constants.O_RDONLY | fsSync.constants.O_NOFOLLOW);
    try {
      const before = await source.stat();
      if (!before.isFile()) throw new Error(`local package source is not a regular file: ${file.path}`);
      const content = await hashFileHandle(source);
      const stat = await source.stat();
      if (!stat.isFile() || stat.size !== (file.input_size ?? file.size) || content.size !== stat.size
        || (stat.mode & 0o777) !== (file.input_mode ?? file.mode) || content.sha256 !== (file.input_sha256 ?? file.sha256)
        || before.mtimeMs !== stat.mtimeMs || before.ctimeMs !== stat.ctimeMs) {
        throw new Error(`local package source changed while copying '${file.path}'`);
      }
      if (file.configuration_input) {
        const input = file.configuration_input;
        const current = await fs.lstat(input.path);
        if (!current.isFile() || current.isSymbolicLink() || current.size !== input.size || (current.mode & 0o777) !== input.mode
          || sha256Bytes(await fs.readFile(input.path)) !== input.sha256) throw new Error(`package configuration changed while copying '${input.path}'`);
      }
      const target = path.join(targetRoot, file.relative_path);
      if (file.body === undefined) {
        const parent = await fs.open(path.dirname(target), fsSync.constants.O_RDONLY | fsSync.constants.O_DIRECTORY | fsSync.constants.O_NOFOLLOW);
        try { require("./nativeFiles.cjs").cloneFile(source.fd, parent.fd, path.basename(target)); }
        finally { await parent.close(); }
      }
      const handle = await fs.open(target, file.body === undefined ? fsSync.constants.O_RDONLY | fsSync.constants.O_NOFOLLOW : "wx", file.mode);
      try {
        if (file.body !== undefined) await handle.writeFile(file.body);
        const installed = await handle.stat();
        if (!installed.isFile() || installed.nlink !== 1 || installed.dev === stat.dev && installed.ino === stat.ino) {
          throw new Error(`local package copy is not isolated: ${target}`);
        }
        await handle.chmod(file.mode);
        await handle.sync();
      } finally { await handle.close(); }
      const after = await source.stat();
      if (after.size !== stat.size || after.mode !== stat.mode || after.mtimeMs !== stat.mtimeMs || after.ctimeMs !== stat.ctimeMs) {
        throw new Error(`local package source changed while copying '${file.path}'`);
      }
    } finally { await source.close(); }
    onFile();
  }
  for (const relativeDirectory of [...snapshot.directories].sort((left, right) => right.length - left.length)) {
    await syncDirectory(path.join(targetRoot, relativeDirectory));
  }
  await syncDirectory(path.dirname(targetRoot));
}

async function hashFileHandle(handle) {
  const hash = crypto.createHash("sha256");
  const stat = await handle.stat();
  if (!stat.isFile()) throw new Error("Local package hashing requires a regular file");
  const buffer = Buffer.allocUnsafe(Math.max(1, Math.min(stat.size, 1024 * 1024)));
  let size = 0;
  for (;;) {
    const { bytesRead } = await handle.read(buffer, 0, buffer.length, size);
    if (bytesRead === 0) return { size, sha256: hash.digest("hex") };
    hash.update(buffer.subarray(0, bytesRead));
    size += bytesRead;
  }
}

async function stagePackageSnapshot(snapshot, stage, transactionRoot, index, onFile) {
  const preparation = path.join(transactionRoot, `owner-${index}`);
  await copySnapshot(snapshot, preparation, onFile);
  if ((await snapshotDirectory(preparation)).digest !== snapshot.digest) throw new Error(`package preparation changed before staging '${stage}'`);
  if (await pathExists(stage)) throw new Error(`package stage appeared during preparation '${stage}'`);
  await renameDirectoryExclusive(preparation, stage);
  await syncDirectory(transactionRoot);
  await syncDirectory(path.dirname(stage));
}

async function verifyMaterializedMoves(roots, moves) {
  for (const move of moves) {
    const target = resolveTransactionPath(roots, move.target);
    const snapshot = await snapshotDirectory(target);
    if (snapshot.digest !== move.digest) throw new Error(`materialized owner '${move.target}' failed digest verification`);
  }
}

async function removeMatchingDirectory(directory, digest, label, optional = false) {
  if (!await pathExists(directory)) {
    if (optional) return;
    throw new Error(`${label} is missing: ${directory}`);
  }
  const snapshot = await snapshotDirectory(directory);
  if (snapshot.digest !== digest) throw new Error(`${label} changed before cleanup: ${directory}`);
  assertNoExcludedActiveEntries(snapshot);
  await fs.rm(directory, { recursive: true, force: false });
  await syncDirectory(path.dirname(directory));
}

async function removeInstallStage(directory, digest, state) {
  if (!await pathExists(directory)) return;
  if (state === "staged" || state === "planned") {
    await removeMatchingDirectory(directory, digest, "install stage");
    return;
  }
  throw new Error(`install stage remains after transaction state '${state}': ${directory}`);
}

async function removeRepairBackups(roots, moves) {
  for (const move of moves) {
    await removeMatchingDirectory(
      resolveTransactionPath(roots, move.backup),
      move.previous_digest,
      "repair backup",
      true,
    );
  }
}

async function reconcileRepairOwnersForward(roots, journal) {
  for (const move of journal.moves) {
    const target = resolveTransactionPath(roots, move.target);
    const stage = resolveTransactionPath(roots, move.stage);
    const backup = resolveTransactionPath(roots, move.backup);
    const observed = await repairOwnerDigests({ target, stage, backup });
    const backupAllowedAbsent = ["receipts_written", "committed"].includes(journal.phase);
    if (
      observed.target !== move.digest
      || ![null, move.digest].includes(observed.stage)
      || !(
        observed.backup === move.previous_digest
        || backupAllowedAbsent && observed.backup === null
      )
    ) {
      throw repairOwnerStateError(move, observed, "forward recovery");
    }
    if (observed.stage === move.digest) {
      await removeMatchingDirectory(stage, move.digest, "repair stage");
    }
  }
}

async function rollbackRepairOwner(roots, move) {
  const target = resolveTransactionPath(roots, move.target);
  const stage = resolveTransactionPath(roots, move.stage);
  const backup = resolveTransactionPath(roots, move.backup);
  const observed = await repairOwnerDigests({ target, stage, backup });
  if (move.previous_digest === null) {
    if (observed.backup !== null) throw repairOwnerStateError(move, observed, "new-owner rollback");
    if (observed.target === null && [null, move.digest].includes(observed.stage)) {
      if (observed.stage === move.digest) await removeMatchingDirectory(stage, move.digest, "new-owner stage");
      return;
    }
    if (observed.target === move.digest && observed.stage === null && ["staged", "materialized"].includes(move.state)) {
      await removeMatchingDirectory(target, move.digest, "materialized new owner");
      return;
    }
    throw repairOwnerStateError(move, observed, "new-owner rollback");
  }
  if (move.state === "planned" && observed.target === move.previous_digest && observed.backup === null) {
    await removeInstallStage(stage, move.digest, "planned");
    return;
  }
  if (
    observed.target === move.previous_digest
    && [null, move.digest].includes(observed.stage)
    && observed.backup === null
  ) {
    if (observed.stage === move.digest) {
      await removeMatchingDirectory(stage, move.digest, "repair stage");
    }
    return;
  }
  if (
    observed.target === null
    && [null, move.digest].includes(observed.stage)
    && observed.backup === move.previous_digest
  ) {
    await restoreRepairBackup(target, backup, move.previous_digest);
    if (observed.stage === move.digest) {
      await removeMatchingDirectory(stage, move.digest, "repair stage");
    }
    return;
  }
  if (
    observed.target === move.digest
    && observed.stage === null
    && observed.backup === move.previous_digest
  ) {
    await removeMatchingDirectory(target, move.digest, "materialized repair owner");
    await restoreRepairBackup(target, backup, move.previous_digest);
    return;
  }
  throw repairOwnerStateError(move, observed, "rollback recovery");
}

async function repairOwnerDigests(paths) {
  return {
    target: await directoryDigestIfPresent(paths.target),
    stage: await directoryDigestIfPresent(paths.stage),
    backup: await directoryDigestIfPresent(paths.backup),
  };
}

async function directoryDigestIfPresent(directory) {
  return await pathExists(directory) ? (await snapshotDirectory(directory)).digest : null;
}

function repairOwnerStateError(move, observed, phase) {
  return new Error(
    `repair ${phase} found foreign owner state '${move.target}' `
    + `(target=${observed.target ?? "missing"}, stage=${observed.stage ?? "missing"}, backup=${observed.backup ?? "missing"})`,
  );
}

async function restoreRepairBackup(target, backup, digest) {
  const snapshot = await snapshotDirectory(backup);
  if (snapshot.digest !== digest) throw new Error(`repair backup changed before rollback: ${backup}`);
  await renameDirectoryExclusive(backup, target);
  await syncDirectory(path.dirname(target));
}

async function createTransactionRoot(roots, operationId, journal) {
  const transactionRoot = path.join(roots.transactionsRoot, operationId);
  if (!isPathInside(roots.transactionsRoot, transactionRoot)) throw new Error("package operation path escapes transaction root");
  if (journal?.operation_id !== operationId) throw new Error("package transaction journal disagrees with operation");
  const preparationRoot = path.join(
    roots.transactionsRoot,
    `.prepare.${operationId}.${crypto.randomBytes(6).toString("hex")}`,
  );
  let published = false;
  await fs.mkdir(preparationRoot, { recursive: false, mode: 0o700 });
  try {
    await writeJournal(path.join(preparationRoot, "journal.json"), journal);
    if (await pathExists(transactionRoot)) throw new Error(`package transaction '${operationId}' already exists`);
    await renameDirectoryExclusive(preparationRoot, transactionRoot);
    published = true;
    await syncDirectory(roots.transactionsRoot);
    return transactionRoot;
  } catch (error) {
    if (!published && await pathExists(preparationRoot)) {
      await removeUnpublishedTransactionRoot(preparationRoot, roots.transactionsRoot);
    }
    throw error;
  }
}

async function removeTransactionRoot(transactionRoot, transactionsRoot) {
  const operationId = path.basename(transactionRoot);
  if (!SAFE_OPERATION_ID.test(operationId) || path.dirname(transactionRoot) !== transactionsRoot) {
    throw new Error(`invalid transaction root removal target '${transactionRoot}'`);
  }
  const cleanupRoot = path.join(
    transactionsRoot,
    `.cleanup.${operationId}.${crypto.randomBytes(6).toString("hex")}`,
  );
  await renameDirectoryExclusive(transactionRoot, cleanupRoot);
  await syncDirectory(transactionsRoot);
  await removeUnpublishedTransactionRoot(cleanupRoot, transactionsRoot);
}

async function removeUnpublishedTransactionRoot(transactionRoot, transactionsRoot) {
  if (path.dirname(transactionRoot) !== transactionsRoot) {
    throw new Error(`invalid unpublished transaction root '${transactionRoot}'`);
  }
  const name = path.basename(transactionRoot);
  if (!PREPARED_TRANSACTION_ROOT.test(name) && !CLEANUP_TRANSACTION_ROOT.test(name)) {
    throw new Error(`invalid unpublished transaction root '${name}'`);
  }
  await fs.rm(transactionRoot, { recursive: true, force: false });
  await syncDirectory(transactionsRoot);
}

async function writeJournal(file, journal) {
  parseTransaction(journal, file);
  await writeJsonAtomic(file, journal, 0o600);
}

async function writeJsonAtomic(file, value, mode) {
  await fs.mkdir(path.dirname(file), { recursive: true, mode: 0o700 });
  const temporary = path.join(
    path.dirname(file),
    `.${path.basename(file)}.${process.pid}.${crypto.randomBytes(6).toString("hex")}.tmp`,
  );
  const handle = await fs.open(temporary, "wx", mode);
  try {
    await handle.writeFile(`${JSON.stringify(value, null, 2)}\n`, "utf8");
    await handle.chmod(mode);
    await handle.sync();
  } finally {
    await handle.close();
  }
  await fs.rename(temporary, file);
  await syncDirectory(path.dirname(file));
}

async function syncDirectory(directory) {
  const handle = await fs.open(directory, fsSync.constants.O_RDONLY);
  try {
    await handle.sync();
  } finally {
    await handle.close();
  }
}

async function renameDirectoryExclusive(source, destination) {
  const flags = fsSync.constants.O_RDONLY | fsSync.constants.O_DIRECTORY | fsSync.constants.O_NOFOLLOW;
  const sourceParent = await fs.open(path.dirname(source), flags);
  try {
    const destinationParent = await fs.open(path.dirname(destination), flags);
    try {
      require("./nativeFiles.cjs").renameDirectoryExclusive(
        sourceParent.fd, path.basename(source), destinationParent.fd, path.basename(destination),
      );
    } finally { await destinationParent.close(); }
  } finally { await sourceParent.close(); }
}

function validateMovePaths(roots, move, operationId) {
  const member = { registry_kind: move.registry_kind, owner: move.owner };
  if (move.target !== memberRelative(member)) throw new Error(`transaction target disagrees with owner '${memberKey(member)}'`);
  if (move.stage !== transactionRelative(member, operationId, "stage")) throw new Error(`transaction stage disagrees with owner '${memberKey(member)}'`);
  if (move.backup !== transactionRelative(member, operationId, "backup")) throw new Error(`transaction backup disagrees with owner '${memberKey(member)}'`);
  for (const relative of [move.target, move.stage, move.backup]) resolveTransactionPath(roots, relative);
}

function resolveTransactionPath(roots, relative) {
  if (typeof relative !== "string" || path.isAbsolute(relative)) throw new Error(`transaction path must be registry-relative: ${relative}`);
  const resolved = path.resolve(roots.activeRegistryRoot, relative);
  if (!isPathInside(roots.activeRegistryRoot, resolved)) throw new Error(`transaction path escapes active registry: ${relative}`);
  const parent = path.dirname(resolved);
  if (fsSync.existsSync(parent)) {
    const stat = fsSync.lstatSync(parent);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`transaction parent is not a plain directory: ${parent}`);
  }
  return resolved;
}

function transactionRelative(member, operationId, suffix) {
  return path.posix.join(member.registry_kind, `.${member.owner}.${operationId}.${suffix}`);
}

function receiptPath(roots, packageId) {
  const id = requiredId(packageId, "package receipt id");
  return path.join(roots.receiptsRoot, `${id}.json`);
}

function orderedMembers(members) {
  return [...members].sort((left, right) => (
    Number(left.primary === true) - Number(right.primary === true)
    || memberKey(left).localeCompare(memberKey(right))
  ));
}

function memberPath(registryRoot, member) {
  const registryKind = requiredRegistryKind(member.registry_kind);
  const owner = requiredId(member.owner, "registry owner");
  return path.join(registryRoot, registryKind, owner);
}

function memberRelative(member) {
  return path.posix.join(requiredRegistryKind(member.registry_kind), requiredId(member.owner, "registry owner"));
}

function memberKey(member) {
  return `${requiredRegistryKind(member.registry_kind)}/${requiredId(member.owner, "registry owner")}`;
}

function assertNoRepeatedTransactionOwners(candidates) {
  const seen = new Map();
  for (const candidate of candidates) {
    for (const member of candidate.entry.manifest.members) {
      const key = memberKey(member);
      const previous = seen.get(key);
      if (previous) throw new Error(`transaction packages '${previous}' and '${candidate.entry.manifest.id}' share owner '${key}'`);
      seen.set(key, candidate.entry.manifest.id);
    }
  }
}

function requireLibraryPackage(library, packageId) {
  const entry = library.packages.find((candidate) => candidate.manifest.id === packageId);
  if (!entry) throw new Error(`local package '${packageId}' is unavailable`);
  return entry;
}

function normalizeDigestMap(value, packageIds, label) {
  if (!isRecord(value)) throw new Error(`${label} must be an object`);
  const result = {};
  for (const packageId of packageIds) {
    const digest = value[packageId];
    if (typeof digest !== "string" || !DIGEST.test(digest)) throw new Error(`${label}.${packageId} must be a SHA-256 digest`);
    result[packageId] = digest;
  }
  const unexpected = Object.keys(value).filter((packageId) => !packageIds.includes(packageId));
  if (unexpected.length) throw new Error(`${label} contains unselected packages: ${unexpected.join(", ")}`);
  return result;
}

function normalizeRepairExpectations(value, packageIds) {
  if (!isRecord(value)) throw new Error("expectedRepairs must be an object");
  const result = {};
  for (const packageId of packageIds) {
    const expected = value[packageId];
    if (!isRecord(expected)) throw new Error(`expectedRepairs.${packageId} must be an object`);
    const sourceDigest = expected.sourceDigest;
    const installedDigest = expected.installedDigest;
    const receiptState = expected.receiptState;
    const receiptDigest = expected.receiptDigest;
    if (!DIGEST.test(sourceDigest)) throw new Error(`expectedRepairs.${packageId}.sourceDigest must be a SHA-256 digest`);
    if (!DIGEST.test(installedDigest)) throw new Error(`expectedRepairs.${packageId}.installedDigest must be a SHA-256 digest`);
    if (!new Set(["missing", "stale"]).has(receiptState)) {
      throw new Error(`expectedRepairs.${packageId}.receiptState must be missing or stale`);
    }
    if (receiptState === "missing" ? receiptDigest !== null : !DIGEST.test(receiptDigest)) {
      throw new Error(`expectedRepairs.${packageId}.receiptDigest does not match receiptState`);
    }
    result[packageId] = { sourceDigest, installedDigest, receiptState, receiptDigest };
  }
  const unexpected = Object.keys(value).filter((packageId) => !packageIds.includes(packageId));
  if (unexpected.length) throw new Error(`expectedRepairs contains unselected packages: ${unexpected.join(", ")}`);
  return result;
}

function requiredUniqueIds(value, label, allowEmpty = false) {
  if (!Array.isArray(value) || !allowEmpty && value.length === 0) throw new Error(`${label} must be ${allowEmpty ? "an" : "a non-empty"} array`);
  const ids = value.map((candidate, index) => requiredId(candidate, `${label}[${index}]`));
  const repeated = repeatedValues(ids);
  if (repeated.length) throw new Error(`${label} repeats ids: ${repeated.join(", ")}`);
  return ids;
}

function requiredOperationId(value) {
  if (typeof value !== "string" || !SAFE_OPERATION_ID.test(value)) throw new Error("operationId is invalid");
  return value;
}

function requiredRegistryKind(value) {
  if (typeof value !== "string" || !REGISTRY_KINDS.has(value)) throw new Error(`registry kind is invalid: ${value}`);
  return value;
}

function requiredId(value, label) {
  if (typeof value !== "string" || !SAFE_ID.test(value)) throw new Error(`${label} is invalid`);
  return value;
}

function optionalId(value, label) {
  if (value === undefined || value === null || value === "") return "";
  return requiredId(value, label);
}

function requiredAbsolutePath(value, label) {
  if (typeof value !== "string" || !path.isAbsolute(value)) throw new Error(`${label} must be absolute`);
  return value;
}

function repeatedValues(values) {
  const seen = new Set();
  const repeated = new Set();
  for (const value of values) {
    if (seen.has(value)) repeated.add(value);
    seen.add(value);
  }
  return [...repeated].sort();
}

function driverDescription(descriptor) {
  const capabilities = Array.isArray(descriptor?.capabilities)
    ? descriptor.capabilities.map(stringValue).filter(Boolean).slice(0, 6)
    : [];
  return capabilities.length
    ? `Provides ${capabilities.map(humanizeId).join(", ")}.`
    : "Provides a worker environment for agent operations.";
}

function humanizeId(value) {
  return String(value).split(/[._-]+/).filter(Boolean).map((part) => (
    part.charAt(0).toUpperCase() + part.slice(1)
  )).join(" ");
}

function hashJson(value) {
  return crypto.createHash("sha256").update(stableJson(value)).digest("hex");
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function schemaError(label, errors) {
  const detail = (errors ?? []).slice(0, 8).map((error) => `${error.instancePath || "/"} ${error.message}`).join("; ");
  return new Error(`${label} is invalid${detail ? `: ${detail}` : ""}`);
}

async function readJsonDocument(file, label) {
  const stat = await fs.lstat(file);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`${label} is not a regular file: ${file}`);
  if (stat.size <= 0 || stat.size > 4 * 1024 * 1024) throw new Error(`${label} size is invalid: ${file}`);
  const body = await fs.readFile(file);
  if (body.byteLength !== stat.size) throw new Error(`${label} changed while reading: ${file}`);
  let value;
  try {
    value = JSON.parse(body.toString("utf8"));
    requireFiniteJson(value, label);
  } catch (error) {
    throw new Error(`${label} is malformed JSON: ${errorMessage(error)}`);
  }
  if (!isRecord(value)) throw new Error(`${label} must be a JSON object`);
  return { value, digest: crypto.createHash("sha256").update(body).digest("hex") };
}

async function readOptionalDirectory(directory) {
  try {
    await requirePlainDirectory(directory, directory);
    return await fs.readdir(directory, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") return [];
    throw error;
  }
}

async function requirePlainDirectory(directory, label) {
  const stat = await fs.lstat(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`${label} is not a plain directory`);
}

async function realPlainDirectory(directory, label) {
  await requirePlainDirectory(directory, label);
  const resolved = await fs.realpath(directory);
  await requirePlainDirectory(resolved, label);
  return resolved;
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

function isPathInside(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative !== "" && !relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative);
}

function isRecord(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

function reportProgress(onProgress, payload) {
  if (typeof onProgress !== "function") return;
  onProgress({ ...payload, percent: Math.max(0, Math.min(100, Number(payload.percent) || 0)) });
}

function registerLocalPackageMutationGuard(options) {
  const stateRoot = path.resolve(requiredAbsolutePath(options?.stateRoot, "stateRoot"));
  const guard = options?.guard;
  if (typeof guard !== "function") throw new Error("local package mutation guard must be a function");
  const guards = packageMutationGuards.get(stateRoot) ?? new Set();
  guards.add(guard);
  packageMutationGuards.set(stateRoot, guards);
  return () => {
    guards.delete(guard);
    if (guards.size === 0) packageMutationGuards.delete(stateRoot);
  };
}

async function assertPackageMutationAllowed(roots, assertMutable, packageIds) {
  await assertRegisteredPackageMutationAllowed(roots, packageIds);
  if (assertMutable === undefined) return;
  if (typeof assertMutable !== "function") throw new Error("assertMutable must be a function");
  await assertMutable([...packageIds]);
}

async function assertRegisteredPackageMutationAllowed(roots, packageIds) {
  const guards = packageMutationGuards.get(path.resolve(roots.stateRoot));
  if (!guards) return;
  for (const guard of guards) await guard([...packageIds]);
}

function transactionPackageIds(journal) {
  return [...new Set([
    ...journal.receipts.map((receipt) => receipt.package_id),
    ...journal.moves.map((move) => move.package_id),
  ])].sort();
}

async function withPackageStateRead(roots, operation) {
  return withPackageStateLock(roots.stateRoot, async (acquireExclusive) => {
    await ensurePackageState(roots);
    if ((await fs.readdir(roots.transactionsRoot)).length) {
      await acquireExclusive();
      await recoverLocalPackageTransactionsUnlocked(roots);
    }
    return operation();
  }, true);
}

function withPackageStateLock(stateRoot, operation, shared = false) {
  return require("./clientFileLock.cjs").withClientFileLock({
    directory: path.resolve(stateRoot), filename: "maintenance-lock.sqlite", shared,
    busyCode: "package_maintenance_busy",
    busyMessage: "Package files are in use by another client. Wait for the current read or update to finish and retry.",
  }, operation);
}

module.exports = {
  adoptLocalPackageUpdates,
  prepareLocalPackageUpdateBaselines,
  withInstalledAgentPreset,
  applyLocalApplicationPack,
  inspectLocalPackageCatalog,
  inspectLocalPackageDiagnostics,
  inspectLocalPackageInstallation,
  installLocalPackages,
  readLocalPackageCatalog,
  readLocalPackageInstallation,
  readLocalPackageSourcePackage,
  registerLocalPackageMutationGuard,
  recoverLocalPackageTransactions,
  repairLocalPackages,
  removeLocalPackages,
  withLocalPackageInstallation,
};
