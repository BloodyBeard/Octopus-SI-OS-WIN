const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const fsSync = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync, execFileSync } = require("node:child_process");
const { installLocalPackages, repairLocalPackages, removeLocalPackages, readLocalPackageCatalog, recoverLocalPackageTransactions } = require("../electron/localPackages.cjs");
const { assertDeclaredGeneratedOutput, isGeneratedSourcePath, readGeneratedSourcePaths } = require("./generated-source-boundary.cjs");
const { validatePublicPayload } = require("./publication-lib.cjs");

function rootsFor(root) {
  return { sourceRegistryRoot: path.join(root, "source"), activeRegistryRoot: path.join(root, "active"),
    libraryRoot: path.join(root, "library"), stateRoot: path.join(root, "state") };
}
function ownerFile(roots, active, file) {
  return path.join(active ? roots.activeRegistryRoot : roots.sourceRegistryRoot, "interface", "world_fixture", file);
}
async function json(file, value) {
  await fs.mkdir(path.dirname(file), { recursive: true });
  await fs.writeFile(file, `${JSON.stringify(value, null, 2)}\n`);
}
async function fixture(root) {
  const roots = rootsFor(root);
  await json(ownerFile(roots, false, "interface.json"), { id: "world_fixture", config: { model: "default", effort: "medium" }, contract: 1 });
  await json(ownerFile(roots, false, "application.json"), { schema_version: 3, id: "world_fixture", name: "Fixture", description: "Declared setup fields", developer: "Octopus", version: "1.0.0", category: "provider", interfaces: ["world_fixture"],
    setup: { fields: [{ interface: "world_fixture", kind: "url", label: "Endpoint", pointer: "/config/endpoint", required: false }] } });
  await fs.writeFile(ownerFile(roots, false, "life-bin"), "version one\n", { mode: 0o755 });
  await json(path.join(roots.sourceRegistryRoot, "tool", "fixture.tool", "tool.json"), { id: "fixture.tool" });
  await json(path.join(roots.libraryRoot, "packages", "fixture.json"), {
    schema: "octopus.local_package", schema_version: 1, id: "fixture", version: "1.0.0", product_kind: "application", product_id: "world_fixture", release_status: "optional",
    display: { name: "Fixture", description: "Declared package ownership", developer: "Octopus", category: "provider" },
    members: [{ registry_kind: "interface", owner: "world_fixture", primary: true,
      configuration: [{ path: "interface.json", fields: [
        { pointer: "/config/model", schema: { type: "string", minLength: 1 } },
        { pointer: "/config/effort", schema: { type: "string", enum: ["medium", "high"] } },
      ] }] }, { registry_kind: "tool", owner: "fixture.tool" }],
  });
  return roots;
}
async function record(roots) { return (await readLocalPackageCatalog(roots)).packages.find((entry) => entry.id === "fixture"); }
function expected(record) {
  return { fixture: { sourceDigest: record.source_digest, installedDigest: record.installed_digest,
    receiptState: record.receipt_state, receiptDigest: record.receipt_digest || null } };
}
async function install(roots) {
  const current = await record(roots);
  return installLocalPackages({ ...roots, operationId: "fixture_install_001", packageIds: ["fixture"], expectedSourceDigests: { fixture: current.source_digest } });
}
async function repair(roots, operationId, onProgress) {
  return repairLocalPackages({ ...roots, operationId, packageIds: ["fixture"], expectedRepairs: expected(await record(roots)), onProgress });
}
async function crash(mode, root) {
  const roots = rootsFor(root), rename = fs.rename;
  fs.rename = async (from, to) => {
    await rename(from, to);
    if (mode === "install-rename" && from.endsWith(".stage") && !path.basename(to).startsWith(".")
      || mode === "remove-rename" && to.endsWith(".backup")
      || mode === "remove-restore" && from.endsWith(".backup")) process.kill(process.pid, "SIGKILL");
  };
  if (mode === "install-rename") await install(roots);
  else if (mode === "remove-restore") await recoverLocalPackageTransactions(roots);
  else if (mode === "remove-rename") await removeLocalPackages({ ...roots, operationId: "fixture_remove_001", packageIds: ["fixture"], expectedInstalledDigests: { fixture: (await record(roots)).installed_digest } });
  else if (mode === "repair-copy") await repair(roots, "fixture_partial_001", (progress) => {
    if (progress.phase === "copying") process.kill(process.pid, "SIGKILL");
  });
  else throw new Error(`unknown crash point ${mode}`);
  throw new Error(`crash point ${mode} was not reached`);
}
function child(mode, root) {
  const result = spawnSync(process.execPath, [__filename, "--crash", mode, root], { encoding: "utf8" });
  assert.equal(result.signal, "SIGKILL", result.stderr || result.stdout);
}
async function sourceBoundaryFixture(root, repoRoot) {
  for (const relative of [".gitignore", "LICENSE", "CONTRIBUTING.md", "README.md", "AGENTS.md",
    "docs/README.md", "docs/architecture.md", "docs/development.md", "docs/getting-started.md", "app/distribution/generated-outputs.json",
    "app/schemas/package_publication.schema.json", "app/schemas/client_extension_publication.schema.json",
    ...["check-public-source.cjs", "generated-source-boundary.cjs", "publication-lib.cjs", "export-public-package.cjs", "export-public-client-extension.cjs", "verify-publication.cjs"].map((name) => `app/scripts/${name}`)]) {
    await fs.mkdir(path.dirname(path.join(root, relative)), { recursive: true });
    await fs.copyFile(path.join(repoRoot, relative), path.join(root, relative));
  }
  await json(path.join(root, "registry", ".octopus-source.json"), { schema_version: 1, id: "fixture" });
  const output = readGeneratedSourcePaths(root).find((entry) => entry.endsWith("life-bin/"));
  const executable = path.join(root, output, "generated"), body = Buffer.from(`${os.homedir()}/build-host-path`);
  await fs.mkdir(path.dirname(executable), { recursive: true }); await fs.writeFile(executable, body);
  const check = (...args) => spawnSync(process.execPath, [path.join(root, "app", "scripts", "check-public-source.cjs"), ...args], { encoding: "utf8" });
  let result = check("--worktree"); assert.equal(result.status, 0, result.stderr);
  const unknown = path.join(root, "registry", "unrelated", "binary"); await fs.mkdir(path.dirname(unknown)); await fs.writeFile(unknown, body);
  result = check("--worktree"); assert.equal(result.status, 1); assert.match(result.stderr, /unrelated\/binary.*absolute user-home path/);
  await fs.unlink(unknown);
  execFileSync("git", ["init", "--quiet", root]); execFileSync("git", ["-C", root, "add", "--force", executable]);
  result = check(); assert.equal(result.status, 1); assert.match(result.stderr, /tracked generated build output/);
  assert.deepEqual(await fs.readFile(executable), body);
}
async function packageExportFixture(root, repoRoot) {
  for (const relative of ["LICENSE", "app/electron/localPackages.cjs", "app/scripts/export-public-package.cjs", "app/scripts/publication-lib.cjs",
    ...["package_publication", "local_package_manifest", "local_application_pack", "local_package_receipt", "local_package_transaction"].map((name) => `app/schemas/${name}.schema.json`)]) {
    await fs.mkdir(path.dirname(path.join(root, relative)), { recursive: true });
    await fs.copyFile(path.join(repoRoot, relative), path.join(root, relative));
  }
  const registry = path.join(root, "registry"), library = path.join(root, "app", "distribution", "local-packages"), out = path.join(root, "published");
  const descriptor = path.join(registry, "interface", "world_provider_codex", "interface.json");
  await json(path.join(registry, ".octopus-source.json"), { schema_version: 1, id: "fixture" });
  await json(descriptor, { id: "world_provider_codex" });
  await json(path.join(library, "packages", "fixture.json"), {
    schema: "octopus.local_package", schema_version: 1, id: "fixture", version: "1.0.0", product_kind: "application", product_id: "world_provider_codex", release_status: "optional",
    display: { name: "Fixture", description: "Selected package export", developer: "Octopus", category: "provider" },
    members: [{ registry_kind: "interface", owner: "world_provider_codex", primary: true }],
  });
  const body = Buffer.from(`${os.homedir()}/private-build-input`), unselected = path.join(registry, "unselected", "payload");
  await fs.mkdir(path.dirname(unselected)); await fs.writeFile(unselected, body);
  const publish = () => spawnSync(process.execPath, [path.join(root, "app", "scripts", "export-public-package.cjs"),
    "--package", "fixture", "--source-registry", registry, "--library-root", library, "--out", out,
    "--license", "Apache-2.0", "--license-file", path.join(root, "LICENSE")],
    { encoding: "utf8", env: { ...process.env, NODE_PATH: path.join(repoRoot, "app", "node_modules") } });
  let result = publish(); assert.equal(result.status, 0, result.stderr);
  const output = JSON.parse(result.stdout).output, publication = JSON.parse(await fs.readFile(path.join(output, "publication.json")));
  assert.deepEqual(publication.members[0].files.map((file) => file.path), ["registry/interface/world_provider_codex/interface.json"]);
  assert.deepEqual(await fs.readFile(path.join(output, "registry", "interface", "world_provider_codex", "interface.json")), await fs.readFile(descriptor));
  const generated = readGeneratedSourcePaths(repoRoot).find((entry) => entry.endsWith("life-bin/") && !entry.includes("/intake/"));
  const selectedBinary = path.join(root, generated, "generated");
  await fs.mkdir(path.dirname(selectedBinary), { recursive: true }); await fs.writeFile(selectedBinary, body);
  result = publish(); assert.equal(result.status, 1); assert.match(result.stderr, /absolute user-home path.*life-bin\/generated/);
  assert.deepEqual(await fs.readFile(selectedBinary), body);
  await fs.writeFile(selectedBinary, "reviewed executable bytes\n");
  await json(path.join(path.dirname(descriptor), "life.json"), { process: { bin: "target/debug/unreviewed" } });
  result = publish(); assert.equal(result.status, 1); assert.match(result.stderr, /development executable path.*life.json/);
  assert.deepEqual(await fs.readdir(out), [path.basename(output)]);
}
async function run() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-package-ownership-"));
  try {
    const roots = await fixture(path.join(root, "configuration"));
    await install(roots);
    await json(ownerFile(roots, true, "interface.json"), { id: "world_fixture", config: { model: "selected", endpoint: "configured" }, contract: 1 });
    let current = await record(roots);
    assert.equal(current.installed, true);
    assert.equal(current.receipt_state, "valid");
    assert.equal(current.customizations.length, 3);
    assert.notEqual(current.source_digest, current.installed_digest);
    const receiptPath = path.join(roots.stateRoot, "receipts", "fixture.json"), originalReceipt = await fs.readFile(receiptPath);
    const baseline = JSON.parse(originalReceipt), crossWired = structuredClone(baseline);
    crossWired.members.find((member) => member.owner === "world_fixture").digest = "a".repeat(64);
    await json(receiptPath, crossWired);
    assert.equal((await record(roots)).receipt_state, "stale");
    crossWired.members = structuredClone(baseline.members);
    crossWired.members.pop();
    await json(receiptPath, crossWired);
    assert.equal((await record(roots)).receipt_state, "stale");
    crossWired.members = [baseline.members[0], baseline.members[0]];
    await json(receiptPath, crossWired);
    assert.equal((await record(roots)).available, false);
    await fs.writeFile(receiptPath, originalReceipt);
    await fs.writeFile(ownerFile(roots, true, "life-bin"), "damaged\n");
    await json(ownerFile(roots, false, "interface.json"), { id: "world_fixture", config: { model: "new-default", effort: "high" }, contract: 2 });
    await fs.writeFile(ownerFile(roots, false, "life-bin"), "version two\n");
    current = await record(roots);
    assert.equal(current.receipt_state, "stale");
    const review = expected(current);
    await repairLocalPackages({ ...roots, operationId: "fixture_repair_001", packageIds: ["fixture"], expectedRepairs: review });
    assert.deepEqual(JSON.parse(await fs.readFile(ownerFile(roots, true, "interface.json"))), { id: "world_fixture", config: { model: "selected", endpoint: "configured" }, contract: 2 });
    assert.equal(await fs.readFile(ownerFile(roots, true, "life-bin"), "utf8"), "version two\n");
    current = await record(roots);
    assert.equal(current.installed, true);
    assert.equal(current.customized, true);
    assert.equal((await repairLocalPackages({ ...roots, operationId: "fixture_repair_001", packageIds: ["fixture"], expectedRepairs: review })).changed, false);
    await json(ownerFile(roots, true, "interface.json"), { id: "world_fixture", config: { model: "selected" }, contract: 999 });
    const unchanged = await fs.readFile(receiptPath);
    await assert.rejects(repair(roots, "fixture_refusal_001"), /unowned JSON changes/);
    assert.deepEqual(await fs.readFile(receiptPath), unchanged);
    assert.equal(JSON.parse(await fs.readFile(ownerFile(roots, true, "interface.json"))).contract, 999);
    await json(ownerFile(roots, true, "interface.json"), { id: "world_fixture", config: { model: 42 }, contract: 2 });
    assert.equal((await record(roots)).available, false);
    await json(ownerFile(roots, true, "interface.json"), { id: "world_fixture", config: { model: "another-selection" }, contract: 2 });
    current = await record(roots);
    assert.equal(current.receipt_state, "valid");
    await removeLocalPackages({ ...roots, operationId: "fixture_remove_002", packageIds: ["fixture"], expectedInstalledDigests: { fixture: current.installed_digest } });
    assert.equal((await record(roots)).materialized, false);
    for (const mode of ["install-rename", "remove-rename", "repair-copy"]) {
      const directory = path.join(root, mode), roots = await fixture(directory);
      if (mode !== "install-rename") await install(roots);
      if (mode === "repair-copy") await fs.writeFile(ownerFile(roots, false, "life-bin"), "version two\n");
      child(mode, directory);
      if (mode === "remove-rename") child("remove-restore", directory);
      const recovered = await record(roots);
      assert.equal(recovered.materialized, mode !== "install-rename");
      assert.equal(recovered.receipt_state, mode === "install-rename" ? "missing" : mode === "repair-copy" ? "stale" : "valid");
      assert.deepEqual(await fs.readdir(path.join(roots.stateRoot, "transactions")), []);
    }
    const collision = await fixture(path.join(root, "foreign-target"));
    child("install-rename", path.join(root, "foreign-target"));
    const pending = JSON.parse(await fs.readFile(path.join(collision.stateRoot, "transactions", "fixture_install_001", "journal.json")));
    const foreignFile = path.join(collision.activeRegistryRoot, pending.moves[0].target, "tool.json");
    await fs.writeFile(foreignFile, "foreign bytes\n");
    await assert.rejects(recoverLocalPackageTransactions(collision), /unowned target or stage/);
    assert.equal(await fs.readFile(foreignFile, "utf8"), "foreign bytes\n");
    const staging = await fixture(path.join(root, "foreign-stage"));
    const foreignStage = path.join(staging.activeRegistryRoot, "tool", ".fixture.tool.fixture_install_001.stage", "foreign.json");
    await json(foreignStage, { retained: true });
    await assert.rejects(install(staging), /install stage/);
    assert.deepEqual(JSON.parse(await fs.readFile(foreignStage)), { retained: true });
    const metadata = await fixture(path.join(root, "metadata"));
    const manifestPath = path.join(metadata.libraryRoot, "packages", "fixture.json");
    const manifest = JSON.parse(await fs.readFile(manifestPath));
    for (const pointer of ["/config/~2", "/id"]) {
      const invalid = structuredClone(manifest); invalid.members[0].configuration[0].fields[0].pointer = pointer;
      await json(manifestPath, invalid);
      await assert.rejects(readLocalPackageCatalog(metadata), /invalid explicit manifests/);
    }
    await json(manifestPath, manifest);
    const before = await record(metadata);
    await fs.writeFile(ownerFile(metadata, false, "life-bin"), "version two\n");
    assert.notEqual((await record(metadata)).source_digest, before.source_digest);
    let changed = false;
    await assert.rejects(installLocalPackages({ ...metadata, operationId: "manifest_race_001", packageIds: ["fixture"], expectedSourceDigests: { fixture: (await record(metadata)).source_digest },
      onProgress(progress) { if (progress.phase === "copying" && !changed) { changed = true; fsSync.writeFileSync(manifestPath, JSON.stringify({ ...manifest, version: "2.0.0" })); } } }), /definition changed/);
    assert.deepEqual(await fs.readdir(path.join(metadata.stateRoot, "transactions")), []);
    const alias = await fixture(path.join(root, "alias")), outside = path.join(root, "outside");
    await fs.mkdir(alias.activeRegistryRoot, { recursive: true }); await fs.mkdir(outside);
    await fs.symlink(outside, path.join(alias.activeRegistryRoot, "tool"));
    await assert.rejects(install(alias), /not a plain directory/);
    assert.deepEqual(await fs.readdir(outside), []);
    const repoRoot = path.resolve(__dirname, "../.."), generated = readGeneratedSourcePaths(repoRoot);
    const ignored = new Set(fsSync.readFileSync(path.join(repoRoot, ".gitignore"), "utf8").split(/\r?\n/));
    for (const output of generated) {
      assert.ok(ignored.has(`/${output}`));
      assert.equal(isGeneratedSourcePath(output, generated), true);
      assertDeclaredGeneratedOutput(path.join(repoRoot, output), repoRoot, generated);
      assert.throws(() => validatePublicPayload(output, Buffer.from(`${os.homedir()}/private-build-input`), { privateRoots: [] }), /absolute user-home path/);
    }
    assert.equal(isGeneratedSourcePath("registry/unrelated/bin/payload", generated), false);
    assert.equal(isGeneratedSourcePath(`${generated[0]}../../unexpected`, generated), false);
    assert.throws(() => assertDeclaredGeneratedOutput(path.join(repoRoot, "registry", "unrelated", "payload"), repoRoot, generated), /not declared/);
    assert.throws(() => validatePublicPayload("life.json", Buffer.from('{"bin":"target/debug/unreviewed"}'), { privateRoots: [], rejectDevelopmentExecutables: true }), /development executable path/);
    await sourceBoundaryFixture(path.join(root, "source-boundary"), repoRoot);
    await packageExportFixture(path.join(root, "package-export"), repoRoot);
    process.stdout.write("package configuration ownership and rename-window recovery passed\n");
  } finally { await fs.rm(root, { recursive: true, force: true }); }
}
if (require.main === module) {
  (process.argv[2] === "--crash" ? crash(process.argv[3], process.argv[4]) : run()).catch((error) => {
    process.stderr.write(`${error.stack || error}\n`); process.exitCode = 1;
  });
}
module.exports = { runPackageOwnership: run };
