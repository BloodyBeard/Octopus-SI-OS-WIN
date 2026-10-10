const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");
const { createClientDoctorReport } = require("../electron/clientDoctorReport.cjs");
const { inspectLocalPackageInstallation, installLocalPackages } = require("../electron/localPackages.cjs");

async function json(file, value) {
  await fs.mkdir(path.dirname(file), { recursive: true });
  await fs.writeFile(file, `${JSON.stringify(value)}\n`);
}
async function fingerprint(root) {
  const entries = await fs.readdir(root, { recursive: true });
  return Promise.all(entries.sort().map(async (entry) => {
    const file = path.join(root, entry), stat = await fs.lstat(file);
    return [entry, stat.mode, stat.isSymbolicLink() ? await fs.readlink(file) : stat.isFile() ? await fs.readFile(file, "hex") : "directory"];
  }));
}

async function run() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-diagnostic-contract-"));
  try {
    const roots = Object.fromEntries(["sourceRegistryRoot", "activeRegistryRoot", "libraryRoot", "stateRoot"].map((key) => [key, path.join(root, key)]));
    await fs.mkdir(roots.activeRegistryRoot);
    const relative = "interface/world_fixture/interface.json";
    const sourceFile = path.join(roots.sourceRegistryRoot, relative), activeFile = path.join(roots.activeRegistryRoot, relative);
    const secret = "PRIVATE_VALUE_must_not_be_shared", privateName = "PRIVATE_LOCAL_FILENAME.json";
    const original = { id: "world_fixture", config: { token: "default" }, contract: 1 };
    await json(sourceFile, original);
    await json(path.join(roots.libraryRoot, "packages/fixture.json"), {
      schema: "octopus.local_package", schema_version: 1, id: "fixture", version: "1.0.0", product_kind: "application", product_id: "world_fixture", release_status: "optional",
      display: { name: secret, description: secret, developer: "Octopus", category: "provider" },
      members: [{ registry_kind: "interface", owner: "world_fixture", configuration: [
        { path: "interface.json", fields: [{ pointer: "/config/token", schema: { type: "string" } }] },
      ] }],
    });
    const initial = await inspectLocalPackageInstallation({ ...roots, packageId: "fixture" });
    await installLocalPackages({ ...roots, packageIds: ["fixture"], operationId: "fixture_install_001", expectedSourceDigests: { fixture: initial.source_digest } });
    await json(activeFile, { ...original, config: { token: secret } });
    await fs.writeFile(path.join(path.dirname(activeFile), privateName), secret);
    await fs.mkdir(path.join(roots.stateRoot, "transactions/.prepare.unfinished_001.0123456789ab"));
    const pending = path.join(roots.stateRoot, "transactions/.prepare.unfinished_001.0123456789ab/journal.json");
    await fs.writeFile(pending, secret);
    const diagnose = (extra = {}) => createClientDoctorReport({ roots, packageId: "fixture", ...extra });
    const before = await fingerprint(root);
    let report = await diagnose();
    assert.deepEqual(await fingerprint(root), before);
    assert.equal(report.status, "observed");
    assert.equal(report.observation.receipt.source_matches_receipt, true);
    let member = report.observation.members[0], row = member.files[0];
    assert.equal(member.active_files_not_in_source, 1);
    assert.equal(row.state, "modified");
    assert.equal(row.configuration.owned_matches_source, true);
    assert.equal(row.configuration.owned_matches_receipt, true);
    assert.deepEqual(row.configuration.fields, [{ index: 0, present: true, matches_source: false }]);
    for (const value of [secret, privateName, root, "world_fixture", "/config/token"]) assert.ok(!JSON.stringify(report).includes(value));
    report = await diagnose({ includePaths: true });
    assert.equal(report.observation.members[0].files[0].configuration.fields[0].pointer, "/config/token");
    assert.ok(!JSON.stringify(report).includes(secret));
    await json(sourceFile, { ...original, contract: 2 });
    report = await diagnose();
    row = report.observation.members[0].files[0];
    assert.equal(report.observation.receipt.source_matches_receipt, false);
    assert.equal(row.configuration.owned_matches_source, false);
    assert.equal(row.configuration.owned_matches_receipt, true);
    await json(activeFile, { ...original, contract: 3 });
    assert.equal((await diagnose()).observation.members[0].files[0].configuration.owned_matches_receipt, false);
    await fs.writeFile(activeFile, `{"${secret}":`);
    report = await diagnose();
    assert.equal(report.observation.members[0].files[0].configuration.state, "invalid_or_changed_during_read");
    assert.ok(!JSON.stringify(report).includes(secret));
    const receiptPath = path.join(roots.stateRoot, "receipts/fixture.json");
    await fs.writeFile(receiptPath, secret);
    assert.equal((await diagnose()).observation.receipt.state, "invalid");
    await fs.unlink(receiptPath);
    assert.equal((await diagnose()).observation.receipt.state, "missing");

    await json(sourceFile, original); await json(activeFile, original);
    const hash = crypto.createHash("sha256").update(await fs.readFile(sourceFile)).digest("hex");
    const baseline = { schema_version: 2, source_id: "fixture-source", source_registry: root,
      files: { [relative]: { sha256: hash, mode: (await fs.stat(sourceFile)).mode & 0o777 } } };
    const manifestPath = path.join(roots.activeRegistryRoot, ".octopus-source-manifest.json");
    await json(manifestPath, baseline);
    await json(path.join(roots.sourceRegistryRoot, ".octopus-source.json"), { schema_version: 1, id: "fixture-source" });
    const fileReport = (registryFile = relative) => createClientDoctorReport({ roots, registryFile });
    report = await fileReport();
    assert.equal(report.observation.origin, "baseline_recorded");
    assert.equal(report.observation.active_matches_baseline, true);
    await json(activeFile, { ...original, config: { token: secret } });
    report = await fileReport();
    assert.equal(report.observation.active_matches_baseline, false);
    assert.equal(report.observation.source_matches_baseline, true);
    await json(sourceFile, { ...original, contract: 2 });
    assert.equal((await fileReport()).observation.source_matches_baseline, false);
    await json(manifestPath, { ...baseline, source_id: "different-source" });
    report = await fileReport();
    assert.equal(report.observation.baseline.source_identity_matches, false);
    assert.equal(report.observation.source_matches_baseline, null);
    await json(manifestPath, { ...baseline, schema_version: 99 });
    assert.equal((await fileReport()).observation.baseline.state, "invalid");
    await json(manifestPath, baseline);
    const localFile = `interface/private_owner/${privateName}`;
    await json(path.join(roots.activeRegistryRoot, localFile), { [secret]: secret });
    report = await fileReport(localFile);
    assert.equal(report.observation.origin, "local_only");
    for (const value of [secret, privateName, root, "private_owner"]) assert.ok(!JSON.stringify(report).includes(value));
    await fs.unlink(activeFile); await fs.symlink(sourceFile, activeFile);
    assert.equal((await fileReport()).observation.active.state, "unsafe_path");
    await fs.unlink(activeFile);
    assert.equal((await fileReport()).observation.active.state, "missing");
    await fs.writeFile(activeFile, Buffer.alloc(4 * 1024 * 1024 + 1));
    assert.equal((await fileReport()).observation.active.state, "size_limit_exceeded");
    await fs.writeFile(activeFile, `{"${secret}":`);
    assert.equal((await fileReport()).observation.active.state, "invalid_json_object");
    assert.equal((await fileReport("../escape.json")).status, "inspection_failed");
    const cliArgs = [path.resolve(__dirname, "../electron/clientDoctor.cjs"), ...Object.values(roots), "--registry-file", relative];
    const snapshot = await fingerprint(root);
    const cli = spawnSync(process.execPath, cliArgs, { encoding: "utf8" });
    assert.equal(cli.status, 0, cli.stderr);
    assert.equal(cli.stderr, "");
    assert.ok(!cli.stdout.includes(secret));
    assert.equal(JSON.parse(cli.stdout).observation.active.state, "invalid_json_object");
    assert.deepEqual(await fingerprint(root), snapshot);
    cliArgs[cliArgs.length - 1] = "../escape.json";
    const refused = spawnSync(process.execPath, cliArgs, { encoding: "utf8" });
    assert.equal(refused.status, 2);
    assert.equal(refused.stderr, "");
    process.stdout.write("configuration diagnostics: provenance, redaction, read-only and CLI contracts passed\n");
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
}
module.exports = { runConfigurationDiagnostics: run };
if (require.main === module) run().catch((error) => { console.error(error); process.exitCode = 1; });
