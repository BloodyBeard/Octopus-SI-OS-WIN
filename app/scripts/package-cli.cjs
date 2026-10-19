const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const { copyTree, copyFile } = require("./copy-macos.cjs");
const { createRuntimePackager } = require("./runtime-package.cjs");

function packageCliRuntime({ appRoot = path.resolve(__dirname, "..") } = {}) {
  appRoot = fs.realpathSync(appRoot);
  const repoRoot = path.dirname(appRoot);
  const outputParent = path.join(appRoot, "build", "cli");
  const outputRoot = path.join(outputParent, "runtime");
  fs.mkdirSync(outputParent, { recursive: true });
  const stage = fs.mkdtempSync(path.join(outputParent, ".runtime-stage-"));
  let previous = null;
  try {
    const clientFilesystem = require("../native/build.cjs").buildClientFilesystem();
    fs.mkdirSync(path.join(stage, "native", "build"), { recursive: true });
    copyFile(clientFilesystem, path.join(stage, "native", "build", path.basename(clientFilesystem)));
    const packager = createRuntimePackager({ repoRoot, appRoot, outputRoot: stage });
    const source = path.join(repoRoot, "registry");
    const baseline = path.join(stage, "registry");
    const library = path.join(stage, "local-packages");
    const packages = path.join(library, "registry");
    const distribution = path.join(appRoot, "distribution");
    const binaries = packager.collectBundledBinaries(source);
    for (const binary of binaries) {
      const file = path.join(repoRoot, "target", "release", binary);
      const stat = fs.lstatSync(file);
      if (!stat.isFile() || !(stat.mode & 0o111)) throw new Error(`Missing release executable: ${file}`);
    }
    packager.copyRegistryBaseline(source, baseline, path.join(distribution, "system-baseline.json"));
    packager.copyRegistrySurface(source, packages);
    packager.copyLocalPackageDefinitions(path.join(distribution, "local-packages"), library);
    copyTree(path.join(repoRoot, "schemas"), path.join(stage, "schemas"), { recursive: true });
    copyTree(distribution, path.join(stage, "distribution"), { recursive: true });
    const ownership = packager.registryExecutableOwnership(packages);
    const ownerExecutables = new Set();
    const installationReferences = new Set();
    packager.rewriteBundledExecutablePaths(packages, ownership, ownerExecutables, installationReferences);
    packager.rewriteBundledExecutablePaths(baseline, ownership, ownerExecutables, installationReferences);
    const installationBinaries = packager.sharedBinaries(installationReferences);
    const sharedExecutables = packager.copyBundledBinaries(installationBinaries);
    packager.stripExecutables([...ownerExecutables, ...sharedExecutables]);
    const managedRoots = packager.bundleManagedCodex([baseline, packages]);
    const managed = require(path.join(source, "interface/world_provider_codex/packages/octopus-provider-codex/managed-cli/bootstrap.cjs"));
    const providerSigning = packager.captureProviderSigningInputs([baseline, packages]);
    const managedSigning = managed.captureSigning(managedRoots);
    packager.signClientResources(stage);
    managed.sealSigning(managedSigning);
    packager.sealProviderSigningInputs(providerSigning);
    packager.validateBundledLayout([baseline, packages], installationBinaries, ownerExecutables);
    const receipt = {
      schema: "octopus.cli_runtime_distribution", schema_version: 1,
      package_version: JSON.parse(fs.readFileSync(path.join(appRoot, "package.json"), "utf8")).version,
      platform: process.platform, architecture: process.arch,
      binaries: installationBinaries.map((binary) => {
        const bytes = fs.readFileSync(path.join(stage, "bin", binary));
        return { path: `bin/${binary}`, bytes: bytes.length, sha256: crypto.createHash("sha256").update(bytes).digest("hex") };
      }),
    };
    fs.writeFileSync(path.join(stage, "runtime-build-receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`);
    if (fs.existsSync(outputRoot)) {
      if (!fs.lstatSync(outputRoot).isDirectory()) throw new Error(`Runtime output is not a plain directory: ${outputRoot}`);
      previous = fs.mkdtempSync(path.join(outputParent, ".runtime-previous-"));
      fs.rmdirSync(previous);
      fs.renameSync(outputRoot, previous);
    }
    try { fs.renameSync(stage, outputRoot); }
    catch (error) { if (previous) fs.renameSync(previous, outputRoot); previous = null; throw error; }
    if (previous) fs.rmSync(previous, { recursive: true });
    return { outputRoot, binaryCount: installationBinaries.length, ownerExecutableCount: ownerExecutables.size };
  } finally {
    if (fs.existsSync(stage)) fs.rmSync(stage, { recursive: true });
  }
}

if (require.main === module) {
  try {
    if (process.argv.length !== 2) throw new Error("Usage: node app/scripts/package-cli.cjs (consumes target/release binaries)");
    console.log(JSON.stringify(packageCliRuntime()));
  } catch (error) {
    console.error(`CLI runtime packaging failed: ${error.message}`);
    process.exitCode = 1;
  }
}

module.exports = { packageCliRuntime };
