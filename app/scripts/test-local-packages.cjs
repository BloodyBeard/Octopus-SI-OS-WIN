const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");
const { once } = require("node:events");
const {
  applyLocalApplicationPack,
  adoptLocalPackageUpdates,
  prepareLocalPackageUpdateBaselines,
  inspectLocalPackageInstallation,
  readLocalPackageCatalog,
  readLocalPackageInstallation,
  withLocalPackageInstallation,
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
  if (mode === "hold-read") {
    await withLocalPackageInstallation({ ...roots, packageId: "application.test_provider" }, () => {
      require("node:fs").writeSync(1, "paused\n"); process.kill(process.pid, "SIGSTOP");
    });
    throw new Error("package reader did not pause");
  }
  if (mode === "hold-live") {
    await applyLocalApplicationPack({ ...roots, operationId: "held_install_001", packId: "test-pack", selectedRecommended: [],
      expectedSourceDigests: { "application.test_provider": currentPackage.source_digest }, onProgress(progress) {
        if (progress.phase === "copying") { require("node:fs").writeSync(1, "paused\n"); process.kill(process.pid, "SIGSTOP"); }
      } });
    throw new Error("live package preparation did not pause");
  }
  if (mode.startsWith("update-")) {
    await adoptLocalPackageUpdates({ ...roots, onProgress(progress) {
      if (progress.phase === mode.slice(7)) process.kill(process.pid, "SIGKILL");
    } });
    throw new Error("update crash point was not reached");
  }
  if (mode === "baseline-receipting") {
    await prepareLocalPackageUpdateBaselines({ ...roots, operationId: "baseline_capture_001",
      packageIds: ["application.test_provider"], expectedSourceDigests: { "application.test_provider": currentPackage.source_digest },
      expectedReceiptDigests: { "application.test_provider": currentPackage.receipt_digest }, assertMutable: async () => {},
      onProgress(progress) { if (progress.phase === "receipting") process.kill(process.pid, "SIGKILL"); } });
    throw new Error("baseline receipt crash point was not reached");
  }
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
      worker_interface_id: "world_test_provider", context_artifact_bindings: [], selected_worker_environment_kinds: [] },
      task: { worker_interface_id: "world_test_provider", context_artifact_bindings: [], selected_worker_environment_kinds: [] }, project_defaults: [] };
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

async function cloneIsolationFixture() {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-clone-install-")));
  const nativeFiles = require("../electron/nativeFiles.cjs"), original = nativeFiles.cloneFile;
  const syncFs = require("node:fs"), crypto = require("node:crypto");
  try {
    const roots = await createFixture(root);
    const relative = path.join("interface", "world_test_provider", "payload.bin");
    const source = path.join(roots.sourceRegistryRoot, relative), target = path.join(roots.activeRegistryRoot, relative);
    const bytes = crypto.randomBytes(8 * 1024 * 1024);
    await fs.writeFile(source, bytes, { mode: 0o750 });
    const catalog = await readLocalPackageCatalog(roots);
    const request = { packId: "test-pack", selectedRecommended: [], expectedSourceDigests: { "application.test_provider": packageRecord(catalog).source_digest } };
    nativeFiles.cloneFile = () => { throw Object.assign(new Error("clone unsupported"), { code: "ENOTSUP" }); };
    await assert.rejects(applyLocalApplicationPack({ ...roots, ...request, operationId: "clone_refused_001" }), /clone unsupported/);
    assert.deepEqual(await transactionEntries(roots), []);
    assert.equal(await fs.access(target).then(() => true, () => false), false);
    nativeFiles.cloneFile = original;
    await applyLocalApplicationPack({ ...roots, ...request, operationId: "clone_install_001" });
    const second = { ...roots, ...rootsFor(path.join(root, "second")), sourceRegistryRoot: roots.sourceRegistryRoot, libraryRoot: roots.libraryRoot };
    await applyLocalApplicationPack({ ...second, ...request, operationId: "clone_sibling_001" });
    const sibling = path.join(second.activeRegistryRoot, relative);
    const allocations = () => {
      const probe = spawnSync("/usr/bin/python3", ["-c", `
import ctypes,json,os,struct,sys
class A(ctypes.Structure):
 _fields_=[('count',ctypes.c_ushort),('reserved',ctypes.c_ushort),('common',ctypes.c_uint32),('volume',ctypes.c_uint32),('directory',ctypes.c_uint32),('file',ctypes.c_uint32),('extended',ctypes.c_uint32)]
get=ctypes.CDLL('/usr/lib/libSystem.B.dylib',use_errno=True).getattrlist
get.argtypes=[ctypes.c_char_p,ctypes.POINTER(A),ctypes.c_void_p,ctypes.c_size_t,ctypes.c_uint32]
rows=[]
for path in sys.argv[1:]:
 attrs=A(5,0,0x80000000,0,0,0,0x1108);buf=ctypes.create_string_buffer(128)
 if get(os.fsencode(path),ctypes.byref(attrs),buf,128,0x21): raise OSError(ctypes.get_errno(),path)
 length,common,volume,directory,file,extended=struct.unpack_from('<IIIIII',buf)
 if length!=44 or extended!=0x1108: raise RuntimeError('APFS clone attributes unavailable')
 private,clone_id,refs=struct.unpack_from('<QQI',buf,24);s=os.stat(path)
 rows.append(dict(logical=s.st_size,allocated=s.st_blocks*512,private=private,clone_id=clone_id,refs=refs,inode=s.st_ino,nlink=s.st_nlink))
print(json.dumps(rows))
`, source, target, sibling], { encoding: "utf8" });
      assert.equal(probe.status, 0, probe.stderr);
      return JSON.parse(probe.stdout);
    };
    const before = allocations();
    assert.equal(new Set(before.map(row => row.inode)).size, 3);
    assert.equal(new Set(before.map(row => row.clone_id)).size, 1);
    for (const row of before) { assert.equal(row.private, 0); assert.equal(row.nlink, 1); assert.equal(row.logical, bytes.length); assert.ok(row.refs >= 3); }
    assert.equal((await fs.stat(target)).mode & 0o777, 0o750);
    const input = await fs.open(source, "r"), parent = await fs.open(path.dirname(target), "r");
    try { assert.throws(() => original(input.fd, parent.fd, path.basename(target)), /File exists/); }
    finally { await input.close(); await parent.close(); }
    const handle = await fs.open(target, "r+");
    try { await handle.write(Buffer.from([bytes[0] ^ 0xff]), 0, 1, 0); await handle.sync(); }
    finally { await handle.close(); }
    assert.deepEqual(await fs.readFile(source), bytes);
    assert.deepEqual(await fs.readFile(sibling), bytes);
    assert.notDeepEqual(await fs.readFile(target), bytes);
    const after = allocations();
    assert.ok(after[1].private > 0);
    const third = { ...roots, ...rootsFor(path.join(root, "third")), sourceRegistryRoot: roots.sourceRegistryRoot, libraryRoot: roots.libraryRoot };
    nativeFiles.cloneFile = (fd, directory, name) => {
      if (name === "payload.bin") syncFs.writeFileSync(source, Buffer.alloc(bytes.length, 0x23));
      return original(fd, directory, name);
    };
    await assert.rejects(applyLocalApplicationPack({ ...third, ...request, operationId: "clone_drift_001" }), /source changed while copying/);
    assert.deepEqual(await transactionEntries(third), []);
    assert.equal(await fs.access(path.join(third.activeRegistryRoot, relative)).then(() => true, () => false), false);
    assert.deepEqual(await fs.readFile(sibling), bytes);
    process.stdout.write(`APFS package clone fixture: ${JSON.stringify({ before, after })}\n`);
  } finally { nativeFiles.cloneFile = original; await fs.rm(root, { recursive: true, force: true }); }
}

async function concurrentUpdateFixture() {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "octopus-concurrent-update-")));
  let child;
  try {
    const roots = await createFixture(root), id = "application.test_provider";
    const source = path.join(roots.sourceRegistryRoot, "interface/world_test_provider/assets");
    const active = path.join(roots.activeRegistryRoot, "interface/world_test_provider/assets");
    await writeJson(path.join(source, "item.json"), { original: true });
    child = spawn(process.execPath, [__filename, "--child", "hold-live", root], { stdio: ["ignore", "pipe", "pipe"] });
    const exited = once(child, "exit");
    assert.equal(await Promise.race([once(child.stdout, "data").then(([data]) => data.toString()),
      exited.then(() => { throw new Error("package writer exited before its preparation boundary"); })]), "paused\n");
    const pending = await transactionEntries(roots);
    assert.equal(pending.length, 1);
    await assert.rejects(readLocalPackageCatalog(roots), error => error.code === "package_maintenance_busy");
    assert.deepEqual(await transactionEntries(roots), pending);
    child.kill("SIGKILL"); await exited; child = null;
    const current = packageRecord(await readLocalPackageCatalog(roots));
    assert.deepEqual(await transactionEntries(roots), []);
    await applyLocalApplicationPack({ ...roots, operationId: "resumed_install_001", packId: "test-pack", selectedRecommended: [], expectedSourceDigests: { [id]: current.source_digest } });
    child = spawn(process.execPath, [__filename, "--child", "hold-read", root], { stdio: ["ignore", "pipe", "pipe"] });
    const readExited = once(child, "exit");
    assert.equal(await Promise.race([once(child.stdout, "data").then(([data]) => data.toString()),
      readExited.then(() => { throw new Error("package reader exited before acquiring its snapshot"); })]), "paused\n");
    const receiptFile = path.join(roots.stateRoot, "receipts", `${id}.json`), receiptBytes = await fs.readFile(receiptFile);
    assert.equal(packageRecord(await readLocalPackageCatalog(roots)).receipt_state, "valid");
    assert.equal((await readLocalPackageInstallation({ ...roots, packageId: id })).receipt_state, "valid");
    await assert.rejects(adoptLocalPackageUpdates(roots), error => error.code === "package_maintenance_busy");
    assert.deepEqual(await fs.readFile(receiptFile), receiptBytes);
    assert.deepEqual(await transactionEntries(roots), []);
    child.kill("SIGKILL"); await readExited; child = null;
    assert.equal((await adoptLocalPackageUpdates(roots)).changed, false);
    await fs.rm(source, { recursive: true }); await fs.writeFile(source, "replacement file\n");
    await fs.mkdir(path.join(active, "local-empty"));
    await assert.rejects(adoptLocalPackageUpdates(roots), /overlapping file\/directory ownership/);
    assert.deepEqual(JSON.parse(await fs.readFile(path.join(active, "item.json"))), { original: true });
    await fs.rmdir(path.join(active, "local-empty"));
    assert.equal((await adoptLocalPackageUpdates(roots)).changed, true);
    assert.equal(await fs.readFile(active, "utf8"), "replacement file\n");
    await fs.rm(source); await writeJson(path.join(source, "next.json"), { next: true });
    assert.equal((await adoptLocalPackageUpdates(roots)).changed, true);
    assert.deepEqual(JSON.parse(await fs.readFile(path.join(active, "next.json"))), { next: true });
    assert.equal((await adoptLocalPackageUpdates(roots)).changed, false);
  } finally { if (child && child.exitCode === null && child.signalCode === null) { const exited = once(child, "exit"); child.kill("SIGKILL"); await exited; } await fs.rm(root, { recursive: true, force: true }); }
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
    const unboundPackageDoctor = await runClientDoctor({ roots,
      inspectInstallation: async () => ({ id: "world_operator_chat", available: true, file_snapshot: clientSnapshot(),
        member_states: [{ registry_kind: "interface", owner: "world_operator_chat", materialized: true,
          source_digest: exactDigest, installed_digest: exactDigest }] }),
      inspectCatalog: async () => ({ errors: [], packages: [{ id: "driver.unbound", materialized: true,
        installed: false, receipt_state: "stale", source_digest: exactDigest, installed_digest: exactDigest,
        receipt_digest: "b".repeat(64), error: "source-owned executable receipt is stale" }] }),
    });
    assert.equal(unboundPackageDoctor.findings[0].code, "package.installation_inconsistent");
    assert.equal(unboundPackageDoctor.findings[0].evidence.package_id, "driver.unbound");
    assert.equal(unboundPackageDoctor.checks, 2);
    await assert.rejects(fs.stat(roots.stateRoot), { code: "ENOENT" });
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

async function updateBaselineFixture() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-update-baseline-"));
  try {
    const roots = await createFixture(root), id = "application.test_provider";
    const sourceOwner = path.join(roots.sourceRegistryRoot, "interface", "world_test_provider");
    const activeOwner = path.join(roots.activeRegistryRoot, "interface", "world_test_provider");
    const kind = path.join("worker_environment_kind", "system_room"), script = path.join("packages", "interval_union", "interval_union.py");
    const sourceKind = path.join(roots.sourceRegistryRoot, kind), activeKind = path.join(roots.activeRegistryRoot, kind);
    const manifestPath = path.join(roots.libraryRoot, "packages", `${id}.json`), manifest = JSON.parse(await fs.readFile(manifestPath));
    manifest.members.push({ registry_kind: "worker_environment_kind", owner: "system_room" });
    await writeJson(manifestPath, manifest);
    await writeJson(path.join(sourceKind, "worker_environment_kind.json"), { id: "system_room", tool_process_bins: ["sh"] });
    await fs.writeFile(path.join(sourceOwner, "driver"), "source binary\n", { mode: 0o755 });
    const initial = packageRecord(await readLocalPackageCatalog(roots));
    await applyLocalApplicationPack({ ...roots, operationId: "baseline_install_001", packId: "test-pack", selectedRecommended: [], expectedSourceDigests: { [id]: initial.source_digest } });
    const receiptPath = path.join(roots.stateRoot, "receipts", `${id}.json`);
    const legacy = JSON.parse(await fs.readFile(receiptPath));
    legacy.schema_version = 1;
    for (const member of legacy.members) { delete member.source_files; delete member.materialized_files; delete member.configuration; }
    await writeJson(receiptPath, legacy);
    await writeJson(path.join(activeOwner, "interface.json"), { id: "world_test_provider", local_authoring: true });
    await fs.mkdir(path.join(activeOwner, "packages"));
    await fs.writeFile(path.join(activeOwner, "packages", "local.py"), "authored code\n");
    await writeJson(path.join(activeKind, "worker_environment_kind.json"), { id: "system_room", tool_process_bins: ["sh", "python3"] });
    await fs.mkdir(path.dirname(path.join(activeKind, script)), { recursive: true });
    await fs.writeFile(path.join(activeKind, script), "print('interval_union')\n", { mode: 0o750 });
    const authoredKind = await fs.readFile(path.join(activeKind, "worker_environment_kind.json"));
    const excluded = path.join(activeOwner, "node_modules", "local.json"), runPath = path.join(roots.octopusHomeRoot, "state", "runs", "run_local.json");
    await writeJson(excluded, { authored: true }); await writeJson(runPath, { pending: "untouched" });
    const excludedBytes = await fs.readFile(excluded), runBytes = await fs.readFile(runPath);
    const local = await fs.readFile(path.join(activeOwner, "interface.json")), before = await fs.readFile(receiptPath);
    const record = await inspectLocalPackageInstallation({ ...roots, packageId: id });
    const request = { ...roots, operationId: "baseline_capture_001", packageIds: [id],
      expectedSourceDigests: { [id]: record.source_digest }, expectedReceiptDigests: { [id]: record.receipt_digest }, assertMutable: async () => {} };
    await assert.rejects(prepareLocalPackageUpdateBaselines({ ...request, assertMutable: async () => { throw new Error("runtime is running"); } }), /runtime is running/);
    let guards = 0;
    await assert.rejects(prepareLocalPackageUpdateBaselines({ ...request, assertMutable: async () => { if (++guards === 2) throw new Error("runtime became live"); } }), /runtime became live/);
    assert.deepEqual(await fs.readFile(receiptPath), before);
    assert.deepEqual(await transactionEntries(roots), []);
    await fs.chmod(path.join(sourceOwner, "driver"), 0o700);
    const changed = await inspectLocalPackageInstallation({ ...roots, packageId: id });
    await assert.rejects(prepareLocalPackageUpdateBaselines({ ...request, expectedSourceDigests: { [id]: changed.source_digest } }), /exact previously receipted source/);
    await fs.chmod(path.join(sourceOwner, "driver"), 0o755);
    await fs.writeFile(path.join(sourceOwner, "driver"), "incoming without baseline\n");
    await assert.rejects(adoptLocalPackageUpdates(roots), /without a per-file source baseline/);
    assert.deepEqual(await fs.readFile(receiptPath), before);
    await fs.writeFile(path.join(sourceOwner, "driver"), "source binary\n");
    runCrashChild("baseline-receipting", root);
    assert.equal((await transactionEntries(roots)).length, 1);
    await readLocalPackageCatalog(roots);
    assert.deepEqual(await transactionEntries(roots), []);
    assert.equal((await prepareLocalPackageUpdateBaselines(request)).changed, false);
    const prepared = JSON.parse(await fs.readFile(receiptPath)), owner = prepared.members.find(member => member.owner === "world_test_provider");
    assert.equal(prepared.schema_version, 2);
    assert.equal(owner.source_files.find(file => file.path === "driver").mode, 0o755);
    assert.equal(owner.source_files.some(file => file.path === "packages/local.py"), false);
    assert.notEqual(owner.source_files.find(file => file.path === "interface.json").sha256, owner.json_files[0].sha256);
    assert.deepEqual(await fs.readFile(path.join(activeOwner, "interface.json")), local);
    assert.equal(await fs.readFile(path.join(activeOwner, "packages", "local.py"), "utf8"), "authored code\n");
    assert.equal((await adoptLocalPackageUpdates(roots)).changed, false);
    await fs.writeFile(path.join(sourceOwner, "driver"), "blocked update\n");
    const excludedReceipt = await fs.readFile(receiptPath);
    await assert.rejects(adoptLocalPackageUpdates(roots), /excluded active entries.*node_modules/);
    assert.deepEqual(await fs.readFile(receiptPath), excludedReceipt);
    assert.deepEqual(await fs.readFile(excluded), excludedBytes); assert.deepEqual(await fs.readFile(runPath), runBytes);
    assert.equal(await fs.readFile(path.join(activeOwner, "driver"), "utf8"), "source binary\n");
    await fs.rm(path.dirname(excluded), { recursive: true });
    for (const phase of ["backing_up", "receipting"]) {
      await fs.writeFile(path.join(sourceOwner, "driver"), `updated ${phase}\n`);
      await fs.chmod(path.join(sourceOwner, "driver"), 0o750);
      runCrashChild(`update-${phase}`, root);
      await adoptLocalPackageUpdates(roots);
      assert.equal((await adoptLocalPackageUpdates(roots)).changed, false);
      assert.deepEqual(await transactionEntries(roots), []);
      assert.equal(await fs.readFile(path.join(activeOwner, "driver"), "utf8"), `updated ${phase}\n`);
      assert.equal((await fs.stat(path.join(activeOwner, "driver"))).mode & 0o777, 0o750);
      assert.deepEqual(await fs.readFile(path.join(activeOwner, "interface.json")), local);
      assert.deepEqual(await fs.readFile(path.join(activeKind, "worker_environment_kind.json")), authoredKind);
      assert.equal(await fs.readFile(path.join(activeKind, script), "utf8"), "print('interval_union')\n");
      assert.equal((await fs.stat(path.join(activeKind, script))).mode & 0o777, 0o750);
    }
    const adopted = await inspectLocalPackageInstallation({ ...roots, packageId: id });
    assert.equal(adopted.receipt_state, "valid");
    assert.equal(adopted.locally_modified, true);
    assert.equal(adopted.local_changes.find(file => file.path === "packages/interval_union/interval_union.py").change, "added");
    const latest = JSON.parse(await fs.readFile(receiptPath)).members.find(member => member.owner === "system_room");
    assert.equal(latest.source_files.some(file => file.path === "packages/interval_union/interval_union.py"), false);
    assert.notEqual(latest.source_files[0].sha256, latest.json_files[0].sha256);
    await assert.rejects(removeLocalPackages({ ...roots, operationId: "local_remove_001", packageIds: [id], expectedInstalledDigests: { [id]: adopted.installed_digest } }), /local changes.*removal cannot delete/);
    const rejectUpdate = async pattern => {
      const snapshot = await inspectLocalPackageInstallation({ ...roots, packageId: id, includeFileSnapshot: true }), receiptBytes = await fs.readFile(receiptPath);
      await assert.rejects(adoptLocalPackageUpdates(roots), pattern);
      assert.deepEqual((await inspectLocalPackageInstallation({ ...roots, packageId: id, includeFileSnapshot: true })).file_snapshot, snapshot.file_snapshot);
      assert.deepEqual(await fs.readFile(receiptPath), receiptBytes);
      assert.deepEqual(await transactionEntries(roots), []);
    };
    await fs.writeFile(path.join(activeOwner, "driver"), "local code change\n");
    await fs.chmod(path.join(activeOwner, "driver"), 0o700);
    await fs.writeFile(path.join(sourceOwner, "new-module"), "incoming independent file\n");
    assert.equal((await adoptLocalPackageUpdates(roots)).changed, true);
    assert.equal(await fs.readFile(path.join(activeOwner, "driver"), "utf8"), "local code change\n");
    assert.equal((await fs.stat(path.join(activeOwner, "driver"))).mode & 0o777, 0o700);
    await fs.chmod(path.join(activeOwner, "driver"), 0o750);
    await fs.writeFile(path.join(sourceOwner, "driver"), "incoming code change\n");
    const damaged = await inspectLocalPackageInstallation({ ...roots, packageId: id });
    await assert.rejects(repairLocalPackages({ ...roots, operationId: "local_repair_001", packageIds: [id], expectedRepairs: { [id]: repairExpectation(damaged) } }), /preserved local changes.*repair cannot discard/);
    await rejectUpdate(/overlapping source-owned changes.*driver/);
    for (const ownerPath of [activeOwner, sourceOwner]) await fs.writeFile(path.join(ownerPath, "driver"), "updated receipting\n");
    await fs.chmod(path.join(activeOwner, "driver"), 0o700);
    await fs.chmod(path.join(sourceOwner, "driver"), 0o744);
    await rejectUpdate(/overlapping source-owned changes.*driver/);
    for (const ownerPath of [activeOwner, sourceOwner]) await fs.chmod(path.join(ownerPath, "driver"), 0o750);
    await fs.mkdir(path.dirname(path.join(sourceKind, script)), { recursive: true });
    await fs.writeFile(path.join(sourceKind, script), "incoming collision\n");
    await rejectUpdate(/overlapping source-owned changes.*interval_union.py/);

    const clean = await createFixture(path.join(root, "clean-legacy"));
    const cleanSource = packageRecord(await readLocalPackageCatalog(clean));
    await applyLocalApplicationPack({ ...clean, operationId: "clean_install_001", packId: "test-pack", selectedRecommended: [], expectedSourceDigests: { [id]: cleanSource.source_digest } });
    const cleanReceiptPath = path.join(clean.stateRoot, "receipts", `${id}.json`), cleanReceipt = JSON.parse(await fs.readFile(cleanReceiptPath));
    cleanReceipt.schema_version = 1;
    for (const member of cleanReceipt.members) { delete member.source_files; delete member.materialized_files; delete member.configuration; }
    await writeJson(cleanReceiptPath, cleanReceipt);
    await fs.writeFile(path.join(clean.sourceRegistryRoot, "interface", "world_test_provider", "new-driver"), "new release\n");
    assert.equal((await adoptLocalPackageUpdates(clean)).changed, true);
    assert.equal(JSON.parse(await fs.readFile(cleanReceiptPath)).schema_version, 2);
    const cleanCurrent = packageRecord(await readLocalPackageCatalog(clean)), cleanExcluded = path.join(clean.activeRegistryRoot, "interface", "world_test_provider", "target", "local.json");
    assert.equal(cleanCurrent.receipt_state, "valid");
    await writeJson(cleanExcluded, { authored: true });
    await assert.rejects(removeLocalPackages({ ...clean, operationId: "excluded_remove_001", packageIds: [id], expectedInstalledDigests: { [id]: cleanCurrent.installed_digest } }), /excluded active entries.*target/);
    await fs.writeFile(path.join(clean.sourceRegistryRoot, "interface", "world_test_provider", "new-driver"), "later release\n");
    await assert.rejects(repairLocalPackages({ ...clean, operationId: "excluded_repair_001", packageIds: [id], expectedRepairs: { [id]: repairExpectation(packageRecord(await readLocalPackageCatalog(clean))) } }), /excluded active entries.*target/);
    assert.deepEqual(JSON.parse(await fs.readFile(cleanExcluded)), { authored: true });
    await fs.writeFile(path.join(clean.sourceRegistryRoot, "interface", "world_test_provider", "new-driver"), "new release\n");
    const legacyReceipt = async () => {
      const value = JSON.parse(await fs.readFile(cleanReceiptPath)); value.schema_version = 1;
      for (const member of value.members) { delete member.source_files; delete member.materialized_files; delete member.configuration; }
      await writeJson(cleanReceiptPath, value); return value;
    };
    const migration = await legacyReceipt(), unproved = structuredClone(migration);
    unproved.members[0].digest = "f".repeat(64); await writeJson(cleanReceiptPath, unproved);
    const unchangedReceipt = await fs.readFile(cleanReceiptPath), unprovedResult = await adoptLocalPackageUpdates(clean);
    assert.equal(unprovedResult.changed, false); assert.deepEqual(unprovedResult.receiptUpgrades, []);
    assert.equal(unprovedResult.localChanges[0].receiptState, "stale"); assert.deepEqual(await fs.readFile(cleanReceiptPath), unchangedReceipt);
    await writeJson(cleanReceiptPath, migration);
    const reviewedReceipt = await fs.readFile(cleanReceiptPath);
    await assert.rejects(adoptLocalPackageUpdates({ ...clean, assertMutable: async () => { throw new Error("runtime is running"); } }), /runtime is running/);
    assert.deepEqual(await fs.readFile(cleanReceiptPath), reviewedReceipt);
    const cleanBefore = await inspectLocalPackageInstallation({ ...clean, packageId: id, includeFileSnapshot: true });
    let metadataMoves;
    const upgraded = await adoptLocalPackageUpdates({ ...clean, onProgress(progress) {
      if (progress.phase === "finalizing") metadataMoves = JSON.parse(require("node:fs").readFileSync(path.join(clean.stateRoot, "transactions", progress.operationId, "journal.json"))).moves;
    } });
    assert.equal(upgraded.changed, true); assert.deepEqual(upgraded.receiptUpgrades, [id]); assert.deepEqual(metadataMoves, []);
    assert.equal(JSON.parse(await fs.readFile(cleanReceiptPath)).schema_version, 2);
    assert.deepEqual((await inspectLocalPackageInstallation({ ...clean, packageId: id, includeFileSnapshot: true })).file_snapshot, cleanBefore.file_snapshot);
    assert.equal((await adoptLocalPackageUpdates(clean)).changed, false);
    await legacyReceipt();
    const localPolicy = path.join(clean.activeRegistryRoot, "interface", "world_test_provider", "interface.json");
    await writeJson(localPolicy, { id: "world_test_provider", authored: true }); await fs.chmod(localPolicy, 0o640);
    const authoredBefore = await inspectLocalPackageInstallation({ ...clean, packageId: id, includeFileSnapshot: true });
    runCrashChild("update-receipting", path.join(root, "clean-legacy"));
    const pending = await transactionEntries(clean); assert.equal(pending.length, 1);
    assert.deepEqual(JSON.parse(await fs.readFile(path.join(clean.stateRoot, "transactions", pending[0], "journal.json"))).moves, []);
    assert.equal((await adoptLocalPackageUpdates(clean)).changed, false); assert.deepEqual(await transactionEntries(clean), []);
    assert.equal(JSON.parse(await fs.readFile(cleanReceiptPath)).schema_version, 2);
    assert.deepEqual((await inspectLocalPackageInstallation({ ...clean, packageId: id, includeFileSnapshot: true })).file_snapshot, authoredBefore.file_snapshot);
    assert.deepEqual(JSON.parse(await fs.readFile(cleanExcluded)), { authored: true });
  } finally { await fs.rm(root, { recursive: true, force: true }); }
}

if (process.argv[2] === "--child") {
  crashChild(process.argv[3], process.argv[4]).catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.stack : String(error)}\n`);
    process.exitCode = 1;
  });
} else {
  (process.argv[2] === "--concurrent-update" ? concurrentUpdateFixture() : process.argv[2] === "--update-baselines" ? updateBaselineFixture() : process.argv[2] === "--consent-recipe" ? consentRecipeFixture() : process.argv[2] === "--clone-isolation" ? cloneIsolationFixture() : run()).then(async () => {
    if (!process.argv[2]) await concurrentUpdateFixture();
    if (!process.argv[2]) await updateBaselineFixture();
    if (!process.argv[2]) await cloneIsolationFixture();
    if (!process.argv[2]) await require("./test-package-ownership.cjs").runPackageOwnership();
    if (!process.argv[2]) await require("./test-configuration-diagnostics.cjs").runConfigurationDiagnostics();
    process.stdout.write("local package lifecycle contract passed\n");
  }).catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.stack : String(error)}\n`);
    process.exitCode = 1;
  });
}
