const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const {
  applyLocalApplicationPack,
  inspectLocalPackageInstallation,
  readLocalPackageCatalog,
  readLocalPackageInstallation,
  registerLocalPackageMutationGuard,
  repairLocalPackages,
  removeLocalPackages,
} = require("../electron/localPackages.cjs");
const { runClientDoctor } = require("../electron/clientDoctor.cjs");
const { createProjectionStore, mutateOctopusOsWorkspace } = require("../electron/projection.cjs");

function rootsFor(root) {
  const octopusHomeRoot = path.join(root, "octopus-home");
  return {
    sourceRegistryRoot: path.join(root, "source-registry"),
    activeRegistryRoot: path.join(octopusHomeRoot, "registry"),
    libraryRoot: path.join(root, "local-library"),
    stateRoot: path.join(octopusHomeRoot, "client", "package-state"),
    octopusHomeRoot,
  };
}

async function writeJson(file, value) {
  await fs.mkdir(path.dirname(file), { recursive: true });
  await fs.writeFile(file, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

async function createFixture(root) {
  const roots = rootsFor(root);
  await writeJson(path.join(roots.sourceRegistryRoot, "interface", "world_test_provider", "interface.json"), {
    id: "world_test_provider",
    interface_type: "world",
    role: "provider",
    direction: "bidirectional",
    source_names: ["test_provider"],
  });
  await writeJson(path.join(roots.sourceRegistryRoot, "artifact", "artifact-test-context", "artifact.json"), {
    id: "artifact-test-context",
    payload: { text: "test context" },
  });
  await writeJson(path.join(roots.libraryRoot, "packages", "application.test_provider.json"), {
    schema: "octopus.local_package",
    schema_version: 1,
    id: "application.test_provider",
    version: "1.0.0",
    product_kind: "application",
    product_id: "world_test_provider",
    release_status: "optional",
    display: {
      name: "Test Provider",
      description: "A multi-owner provider package used by the lifecycle contract test.",
      developer: "Octopus",
      category: "provider",
    },
    members: [
      { registry_kind: "interface", owner: "world_test_provider", primary: true },
      { registry_kind: "artifact", owner: "artifact-test-context" },
    ],
  });
  await writeJson(path.join(roots.libraryRoot, "packs", "test-pack.json"), {
    schema: "octopus.local_application_pack",
    schema_version: 1,
    id: "test-pack",
    version: "1.0.0",
    name: "Test Pack",
    description: "A local Application Pack used by the lifecycle contract test.",
    components: [
      {
        package_id: "application.test_provider",
        selection: "included",
        reason: "Exercises atomic materialization of two independent registry owners.",
      },
    ],
  });
  return roots;
}

function packageRecord(catalog) {
  return catalog.packages.find((entry) => entry.id === "application.test_provider");
}

function repairExpectation(record) {
  return {
    sourceDigest: record.source_digest,
    installedDigest: record.installed_digest,
    receiptState: record.receipt_state,
    receiptDigest: record.receipt_digest || null,
  };
}

function packRecord(catalog) {
  return catalog.packs.find((entry) => entry.id === "test-pack");
}

async function transactionEntries(roots) {
  return fs.readdir(path.join(roots.stateRoot, "transactions"));
}

function runCrashChild(mode, root) {
  const child = spawnSync(process.execPath, [__filename, "--child", mode, root], {
    encoding: "utf8",
  });
  assert.equal(child.status, null, child.stderr || child.stdout);
  assert.equal(child.signal, "SIGKILL", child.stderr || child.stdout);
}

async function crashChild(mode, root) {
  const roots = rootsFor(root);
  const catalog = await readLocalPackageCatalog(roots);
  const currentPackage = packageRecord(catalog);
  assert.ok(currentPackage);
  if (mode === "rollback-install") {
    await applyLocalApplicationPack({
      ...roots,
      operationId: "rollback_install_001",
      packId: "test-pack",
      selectedRecommended: [],
      expectedSourceDigests: { "application.test_provider": currentPackage.source_digest },
      onProgress(progress) {
        if (progress.phase === "installing") process.kill(process.pid, "SIGKILL");
      },
    });
    throw new Error("rollback crash point was not reached");
  }
  if (mode === "forward-install") {
    await applyLocalApplicationPack({
      ...roots,
      operationId: "forward_install_001",
      packId: "test-pack",
      selectedRecommended: [],
      expectedSourceDigests: { "application.test_provider": currentPackage.source_digest },
      onProgress(progress) {
        if (progress.phase === "finalizing") process.kill(process.pid, "SIGKILL");
      },
    });
    throw new Error("install roll-forward crash point was not reached");
  }
  if (mode === "forward-remove") {
    await removeLocalPackages({
      ...roots,
      operationId: "forward_remove_001",
      packageIds: ["application.test_provider"],
      expectedInstalledDigests: { "application.test_provider": currentPackage.installed_digest },
      onProgress(progress) {
        if (progress.phase === "finalizing") process.kill(process.pid, "SIGKILL");
      },
    });
    throw new Error("remove roll-forward crash point was not reached");
  }
  if ([
    "rollback-repair",
    "rollback-repair-backup",
    "rollback-repair-materialize",
    "forward-repair",
    "receipt-only-forward",
  ].includes(mode)) {
    await repairLocalPackages({
      ...roots,
      operationId: `${mode.replaceAll("-", "_")}_001`,
      packageIds: ["application.test_provider"],
      expectedRepairs: { "application.test_provider": repairExpectation(currentPackage) },
      onProgress(progress) {
        if (mode === "rollback-repair" && progress.phase === "installing") process.kill(process.pid, "SIGKILL");
        if (mode === "rollback-repair-backup" && progress.phase === "backing_up") process.kill(process.pid, "SIGKILL");
        if (mode === "rollback-repair-materialize" && progress.phase === "materializing") process.kill(process.pid, "SIGKILL");
        if (mode === "forward-repair" && progress.phase === "finalizing") process.kill(process.pid, "SIGKILL");
        if (mode === "receipt-only-forward" && progress.phase === "receipting") process.kill(process.pid, "SIGKILL");
      },
    });
    throw new Error("repair crash point was not reached");
  }
  throw new Error(`unknown crash test mode '${mode}'`);
}

async function consentRecipeFixture() {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-consent-package-")));
  try {
    const roots = await createFixture(root);
    const packFile = path.join(roots.libraryRoot, "packs", "test-pack.json");
    const pack = JSON.parse(await fs.readFile(packFile, "utf8"));
    pack.workspace_setup = { selection: "included", home: { provider_interface_id: "world_test_provider",
      worker_interface_id: "world_test_provider", context_artifact_bindings: [], selected_worker_environment_kinds: [] }, project_defaults: [] };
    await writeJson(packFile, pack);
    const ordinary = await readLocalPackageCatalog(roots);
    assert.ok(ordinary.packs.some((entry) => entry.id === pack.id), JSON.stringify(ordinary.errors));
    pack.workspace_setup.application_consent_authority = { selection: "optional",
      component_package_id: "application.test_provider", policy_artifact: "artifact-test-context" };
    const selectedBytes = `${JSON.stringify(pack, null, 2)}\n`;
    await fs.writeFile(packFile, selectedBytes);
    const retired = await readLocalPackageCatalog(roots);
    assert.equal(retired.packs.some((entry) => entry.id === pack.id), false);
    assert.ok(retired.errors.some((error) => String(error).includes("additional properties")), JSON.stringify(retired.errors));
    assert.equal(retired.packages.some((entry) => entry.id === "application.test_provider"), true);
    assert.equal(await fs.readFile(packFile, "utf8"), selectedBytes);
  } finally { await fs.rm(root, { recursive: true, force: true }); }
}

async function run() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-local-packages-"));
  try {
    const roots = await createFixture(root);
    await fs.mkdir(roots.activeRegistryRoot, { recursive: true });
    await fs.cp(
      path.join(roots.sourceRegistryRoot, "interface", "world_test_provider"),
      path.join(roots.activeRegistryRoot, "interface", "world_test_provider"),
      { recursive: true },
    );
    const inspected = await inspectLocalPackageInstallation({
      ...roots,
      packageId: "application.test_provider",
    });
    const inspectedOwner = inspected.member_states.find((member) => member.owner === "world_test_provider");
    assert.equal(inspectedOwner.materialized, true);
    assert.equal(inspectedOwner.source_digest, inspectedOwner.installed_digest);
    await assert.rejects(fs.stat(roots.stateRoot), { code: "ENOENT" });
    await fs.rm(path.join(roots.activeRegistryRoot, "interface", "world_test_provider"), { recursive: true });
    const exactDigest = "a".repeat(64);
    const clientFile = {
      relative_path: "client.cjs",
      path: path.join(roots.activeRegistryRoot, "interface", "world_operator_chat", "client.cjs"),
      mode: 0o644,
      size: 10,
      sha256: exactDigest,
    };
    const clientSnapshot = (sha256 = exactDigest) => ({ members: [{
      registry_kind: "interface",
      owner: "world_operator_chat",
      source_files: [clientFile],
      installed_files: [{ ...clientFile, sha256 }],
    }] });
    const clientDoctor = await runClientDoctor({
      roots,
      inspectInstallation: async () => ({
        id: "world_operator_chat",
        available: true,
        file_snapshot: clientSnapshot(),
        member_states: [{
          registry_kind: "interface",
          owner: "world_operator_chat",
          materialized: true,
          source_digest: exactDigest,
          installed_digest: exactDigest,
        }],
      }),
    });
    assert.deepEqual(clientDoctor, { schema_version: 2, checks: 1, passed: 1, findings: [], errors: [] });
    const staleClientDoctor = await runClientDoctor({
      roots,
      inspectInstallation: async () => ({
        id: "world_operator_chat",
        available: true,
        file_snapshot: clientSnapshot("b".repeat(64)),
        member_states: [{
          registry_kind: "interface",
          owner: "world_operator_chat",
          materialized: true,
          source_digest: exactDigest,
          installed_digest: "b".repeat(64),
        }],
      }),
    });
    assert.equal(staleClientDoctor.passed, 0);
    assert.equal(staleClientDoctor.findings[0].code, "client.package_owner_stale");
    assert.match(staleClientDoctor.findings[0].next_step, /owner-authorized package repair/);
    assert.match(staleClientDoctor.errors[0], /differs from its package source/);
    await writeJson(path.join(roots.sourceRegistryRoot, "artifact", "artifact-unowned-context", "artifact.json"), {
      id: "artifact-unowned-context",
      label: "context.reference",
      text: "canonical context",
    });
    await writeJson(path.join(roots.activeRegistryRoot, "artifact", "artifact-unowned-context", "artifact.json"), {
      id: "artifact-unowned-context",
      label: "context.reference",
      text: "stale production context",
    });
    await writeJson(path.join(roots.activeRegistryRoot, "artifact", "artifact-local-context", "artifact.json"), {
      id: "artifact-local-context", label: "context.reference", text: "locally authored context",
    });
    await writeJson(path.join(roots.activeRegistryRoot, "interface", "system_worker_context_test", "policy.json"), {
      id: "system_worker_context_test",
      ceiling: { interfaces: [], tools: [], wek: [], mark_outcomes: [], hard_blocked_tools: [] },
      role_artifacts: ["artifact-unowned-context", "artifact-local-context"],
    });
    const runsRoot = path.join(roots.octopusHomeRoot, "state", "runs");
    await writeJson(path.join(roots.octopusHomeRoot, "host.json"), {
      host_manifest_version: 1,
      root_refs: { run_records_root: runsRoot },
    });
    for (const [id, status] of [["run_ghost", "running"], ["run_observed", "running"], ["run_idle", "idle"]]) {
      await writeJson(path.join(runsRoot, `${id}.json`), { id, status, checkpoints: [], active_checkpoints: [] });
    }
    await writeJson(path.join(runsRoot, "run_idle.json"), {
      id: "run_idle",
      status: "idle",
      checkpoints: [{ id: "cp_missing_context", artifact_bindings: [{ id: "artifact-missing-context" }] }],
      active_checkpoints: ["cp_missing_context"],
    });
    await fs.mkdir(path.join(roots.octopusHomeRoot, "ledger"), { recursive: true });
    await fs.writeFile(path.join(roots.octopusHomeRoot, "ledger", "movements.jsonl"), `${JSON.stringify({
      from: { Interface: "world_test" },
      to: { Interface: "world_operator_chat_test" },
      invocation_metadata: { runtime: { run: "run_observed" } },
    })}\n`);
    const contextDoctor = await runClientDoctor({
      roots,
      inspectInstallation: async () => ({
        id: "world_operator_chat",
        available: true,
        file_snapshot: clientSnapshot(),
        member_states: [{
          registry_kind: "interface",
          owner: "world_operator_chat",
          materialized: true,
          source_digest: exactDigest,
          installed_digest: exactDigest,
        }],
      }),
    });
    const contextFinding = contextDoctor.findings.find((finding) => finding.subject === "artifact-unowned-context");
    assert.equal(contextFinding, undefined);
    assert.equal(contextDoctor.findings.find((finding) => finding.subject === "artifact-local-context"), undefined);
    const ghostFinding = contextDoctor.findings.find((finding) => finding.subject === "run_ghost");
    assert.equal(ghostFinding.code, "run.running_without_ledger_movement");
    assert.equal(ghostFinding.evidence.movement_count, 0);
    assert.match(ghostFinding.next_step, /Create a new task and leave 'run_ghost' untouched/);
    assert.equal(JSON.parse(await fs.readFile(path.join(runsRoot, "run_ghost.json"), "utf8")).status, "running");
    assert.equal(
      contextDoctor.findings.find((finding) => finding.subject === "artifact-missing-context").code,
      "context.bound_artifact_active_missing",
    );
    await assert.rejects(fs.stat(roots.stateRoot), { code: "ENOENT" });
    let catalog = await readLocalPackageCatalog(roots);
    assert.deepEqual(catalog.errors, []);
    assert.equal(packageRecord(catalog)?.available, true);
    assert.equal(packageRecord(catalog)?.installed, false);
    assert.equal(packRecord(catalog)?.required_components_materialized, false);
    await fs.mkdir(path.join(roots.stateRoot, "transactions", ".prepare.abandoned_001.0123456789ab"));
    await fs.writeFile(path.join(
      roots.stateRoot,
      "transactions",
      ".prepare.abandoned_001.0123456789ab",
      ".journal.json.partial.tmp",
    ), "partial", "utf8");
    await fs.mkdir(path.join(roots.stateRoot, "transactions", ".cleanup.abandoned_002.abcdef012345"));
    catalog = await readLocalPackageCatalog(roots);
    assert.deepEqual(await transactionEntries(roots), []);

    const projectionStore = createProjectionStore({
      resolveRepoRoot: () => path.resolve(__dirname, "../.."),
      sourceRegistry: () => roots.sourceRegistryRoot,
      localPackageRegistry: () => roots.sourceRegistryRoot,
      localPackageLibrary: () => roots.libraryRoot,
      octopusHome: () => roots.octopusHomeRoot,
      projectEePolicies: async () => ({ environments: [], issues: [] }),
    });
    try {
      const scope = await projectionStore.read({
        contract_version: 1,
        method: "projection.operator.os.scope",
      });
      const projectedCatalog = await projectionStore.read({
        contract_version: 1,
        method: "projection.operator.os.local_packages",
      });
      assert.equal(scope.status, "unbound");
      assert.equal(packageRecord(projectedCatalog)?.available, true);
      assert.equal(packRecord(projectedCatalog)?.required_components_materialized, false);
    } finally {
      projectionStore.close();
    }

    runCrashChild("rollback-install", root);
    catalog = await readLocalPackageCatalog(roots);
    assert.equal(packageRecord(catalog)?.installed, false);
    assert.equal(packageRecord(catalog)?.receipt_state, "missing");
    assert.deepEqual(await transactionEntries(roots), []);

    runCrashChild("forward-install", root);
    catalog = await readLocalPackageCatalog(roots);
    assert.equal(packageRecord(catalog)?.installed, true);
    assert.equal(packageRecord(catalog)?.receipt_state, "valid");
    assert.equal(packRecord(catalog)?.required_components_materialized, true);
    assert.deepEqual(await transactionEntries(roots), []);

    runCrashChild("forward-remove", root);
    catalog = await readLocalPackageCatalog(roots);
    assert.equal(packageRecord(catalog)?.installed, false);
    assert.equal(packageRecord(catalog)?.receipt_state, "missing");
    assert.deepEqual(await transactionEntries(roots), []);

    for (const [kind, owner] of [["interface", "world_test_provider"], ["artifact", "artifact-test-context"]]) {
      await fs.mkdir(path.join(roots.activeRegistryRoot, kind), { recursive: true });
      await fs.cp(
        path.join(roots.sourceRegistryRoot, kind, owner),
        path.join(roots.activeRegistryRoot, kind, owner),
        { recursive: true },
      );
    }
    catalog = await readLocalPackageCatalog(roots);
    assert.equal(packageRecord(catalog)?.materialized, true);
    assert.equal(packageRecord(catalog)?.installed, false);
    assert.equal(packageRecord(catalog)?.receipt_state, "missing");
    assert.match(packageRecord(catalog)?.error ?? "", /without a valid install receipt/);
    const missingReceiptExpectation = repairExpectation(packageRecord(catalog));
    await assert.rejects(
      mutateOctopusOsWorkspace(roots.activeRegistryRoot, roots.octopusHomeRoot, {}, {
        action: "repair_package",
        operationId: "receipt_repair_rejected_001",
        packageIds: ["application.test_provider"],
        expectedRepairs: {
          "application.test_provider": {
            ...missingReceiptExpectation,
            installedDigest: "f".repeat(64),
          },
        },
      }, {
        localPackageRegistryRoot: roots.sourceRegistryRoot,
        localPackageLibraryRoot: roots.libraryRoot,
      }),
      /active owners changed before repair/,
    );
    assert.deepEqual(await transactionEntries(roots), []);
    runCrashChild("receipt-only-forward", root);
    await writeJson(path.join(roots.activeRegistryRoot, "interface", "world_test_provider", "interface.json"), {
      id: "world_test_provider",
      interface_type: "world",
      role: "provider",
      direction: "bidirectional",
      source_names: ["foreign_during_recovery"],
    });
    await assert.rejects(
      readLocalPackageCatalog(roots),
      /repair receipt owner .* changed before commit/,
    );
    await fs.rm(path.join(roots.activeRegistryRoot, "interface", "world_test_provider"), { recursive: true });
    await fs.cp(
      path.join(roots.sourceRegistryRoot, "interface", "world_test_provider"),
      path.join(roots.activeRegistryRoot, "interface", "world_test_provider"),
      { recursive: true },
    );
    catalog = await readLocalPackageCatalog(roots);
    assert.equal(packageRecord(catalog)?.installed, true);
    assert.equal(packageRecord(catalog)?.receipt_state, "valid");
    const receiptOnlyRepair = await repairLocalPackages({
      ...roots,
      operationId: "receipt_only_forward_001",
      packageIds: ["application.test_provider"],
      expectedRepairs: { "application.test_provider": missingReceiptExpectation },
    });
    assert.equal(receiptOnlyRepair.changed, false);
    await removeLocalPackages({
      ...roots,
      operationId: "receipt_repair_remove_001",
      packageIds: ["application.test_provider"],
      expectedInstalledDigests: { "application.test_provider": packageRecord(catalog).installed_digest },
    });
    catalog = await readLocalPackageCatalog(roots);

    const installPhases = [];
    let assertedMutablePackageIds = [];
    const sourceDigest = packageRecord(catalog).source_digest;
    const install = await applyLocalApplicationPack({
      ...roots,
      operationId: "normal_install_001",
      packId: "test-pack",
      selectedRecommended: [],
      expectedSourceDigests: { "application.test_provider": sourceDigest },
      assertMutable(packageIds) {
        assertedMutablePackageIds = packageIds;
      },
      onProgress(progress) {
        installPhases.push(progress.phase);
      },
    });
    assert.equal(install.changed, true);
    assert.equal(install.action, "apply_application_pack");
    assert.deepEqual(assertedMutablePackageIds, ["application.test_provider"]);
    assert.deepEqual(new Set(installPhases), new Set(["verifying", "copying", "installing", "finalizing", "complete"]));

    catalog = await readLocalPackageCatalog(roots);
    const exactInstallation = await readLocalPackageInstallation({
      ...roots,
      packageId: "application.test_provider",
    });
    assert.equal(exactInstallation.receipt_state, "valid");
    assert.equal(exactInstallation.source_digest, exactInstallation.installed_digest);
    await writeJson(path.join(roots.sourceRegistryRoot, "interface", "world_test_provider", "interface.json"), {
      id: "world_test_provider",
      interface_type: "world",
      role: "provider",
      direction: "bidirectional",
      source_names: ["test_provider_v2"],
    });
    let currentInstallation = await readLocalPackageInstallation({
      ...roots,
      packageId: "application.test_provider",
    });
    assert.equal(currentInstallation.installed, false);
    assert.equal(currentInstallation.receipt_state, "stale");
    const staleRepairExpectation = repairExpectation(currentInstallation);
    runCrashChild("rollback-repair", root);
    const [pendingRepair] = await transactionEntries(roots);
    assert.ok(pendingRepair);
    const pendingRepairJournalPath = path.join(roots.stateRoot, "transactions", pendingRepair, "journal.json");
    const pendingRepairJournal = JSON.parse(await fs.readFile(pendingRepairJournalPath, "utf8"));
    const crossWiredJournal = structuredClone(pendingRepairJournal);
    crossWiredJournal.receipts[0].operation_id = "foreign_repair_001";
    await writeJson(pendingRepairJournalPath, crossWiredJournal);
    await assert.rejects(
      readLocalPackageCatalog(roots),
      /repair receipt .* disagrees with transaction operation/,
    );
    await writeJson(pendingRepairJournalPath, pendingRepairJournal);
    const unregisterMutationGuard = registerLocalPackageMutationGuard({
      stateRoot: roots.stateRoot,
      guard(packageIds) {
        if (packageIds.includes("application.test_provider")) {
          throw new Error("loaded package cannot be recovered in place");
        }
      },
    });
    await assert.rejects(
      readLocalPackageCatalog(roots),
      /loaded package cannot be recovered in place/,
    );
    assert.deepEqual(await transactionEntries(roots), [pendingRepair]);
    unregisterMutationGuard();
    currentInstallation = await readLocalPackageInstallation({
      ...roots,
      packageId: "application.test_provider",
    });
    assert.equal(currentInstallation.installed, false);
    assert.equal(currentInstallation.receipt_state, "stale");
    assert.deepEqual(
      JSON.parse(await fs.readFile(path.join(roots.activeRegistryRoot, "interface", "world_test_provider", "interface.json"), "utf8")).source_names,
      ["test_provider"],
    );
    runCrashChild("rollback-repair-backup", root);
    currentInstallation = await readLocalPackageInstallation({
      ...roots,
      packageId: "application.test_provider",
    });
    assert.equal(currentInstallation.receipt_state, "stale");
    runCrashChild("rollback-repair-materialize", root);
    currentInstallation = await readLocalPackageInstallation({
      ...roots,
      packageId: "application.test_provider",
    });
    assert.equal(currentInstallation.receipt_state, "stale");
    runCrashChild("forward-repair", root);
    currentInstallation = await readLocalPackageInstallation({
      ...roots,
      packageId: "application.test_provider",
    });
    assert.equal(currentInstallation.installed, true);
    assert.equal(currentInstallation.receipt_state, "valid");
    assert.deepEqual(
      JSON.parse(await fs.readFile(path.join(roots.activeRegistryRoot, "interface", "world_test_provider", "interface.json"), "utf8")).source_names,
      ["test_provider_v2"],
    );
    const repeatedRepair = await repairLocalPackages({
      ...roots,
      operationId: "forward_repair_001",
      packageIds: ["application.test_provider"],
      expectedRepairs: { "application.test_provider": staleRepairExpectation },
    });
    assert.equal(repeatedRepair.changed, false);
    const installedDigest = currentInstallation.installed_digest;
    const dependentManifest = {
      schema: "octopus.local_package", schema_version: 1, id: "application.dependent", version: "1.0.0",
      product_kind: "application", product_id: "world_dependent", release_status: "optional",
      display: { name: "Dependent", description: "Declared dependency removal fixture.", developer: "Octopus", category: "connector" },
      required_packages: ["application.test_provider"],
      members: [{ registry_kind: "interface", owner: "world_dependent", primary: true }],
    };
    await writeJson(path.join(roots.libraryRoot, "packages", "application.dependent.json"), dependentManifest);
    await writeJson(path.join(roots.sourceRegistryRoot, "interface", "world_dependent", "interface.json"), { id: "world_dependent" });
    const dependentOwner = path.join(roots.activeRegistryRoot, "interface", "world_dependent");
    await writeJson(path.join(dependentOwner, "interface.json"), { id: "world_dependent", source_names: ["edited"] });
    const dependentState = (await readLocalPackageCatalog(roots)).packages.find((entry) => entry.id === dependentManifest.id);
    assert.equal(dependentState.installed, false);
    assert.equal(dependentState.materialized, true);
    assert.equal(dependentState.receipt_state, "missing");
    await assert.rejects(removeLocalPackages({
      ...roots, operationId: "blocked_dependency_001", packageIds: ["application.test_provider"],
      expectedInstalledDigests: { "application.test_provider": installedDigest },
    }), /materialized dependent 'application.dependent'/);
    assert.equal((await readLocalPackageInstallation({ ...roots, packageId: "application.test_provider" })).installed_digest, installedDigest);
    await fs.rm(dependentOwner, { recursive: true });
    const removePhases = [];
    const removal = await removeLocalPackages({
      ...roots,
      operationId: "normal_remove_001",
      packageIds: ["application.test_provider"],
      expectedInstalledDigests: { "application.test_provider": installedDigest },
      onProgress(progress) {
        removePhases.push(progress.phase);
      },
    });
    assert.equal(removal.changed, true);
    assert.deepEqual(new Set(removePhases), new Set(["verifying", "removing", "finalizing", "complete"]));

    catalog = await readLocalPackageCatalog(roots);
    assert.equal(packageRecord(catalog)?.installed, false);
    assert.equal(await fs.stat(path.join(roots.sourceRegistryRoot, "interface", "world_test_provider")).then((stat) => stat.isDirectory()), true);
    assert.deepEqual(await transactionEntries(roots), []);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
}

if (process.argv[2] === "--child") {
  crashChild(process.argv[3], process.argv[4]).catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.stack : String(error)}\n`);
    process.exitCode = 1;
  });
} else {
  (process.argv[2] === "--consent-recipe" ? consentRecipeFixture() : run()).then(async () => {
    if (process.argv[2] !== "--consent-recipe") await require("./test-package-ownership.cjs").runPackageOwnership();
    if (process.argv[2] !== "--consent-recipe") await require("./test-configuration-diagnostics.cjs").runConfigurationDiagnostics();
    process.stdout.write("local package lifecycle contract passed\n");
  }).catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.stack : String(error)}\n`);
    process.exitCode = 1;
  });
}
