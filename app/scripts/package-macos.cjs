const fs = require("node:fs");
const crypto = require("node:crypto");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { copyTree, copyFile } = require("./copy-macos.cjs");
const { publishClientPresentation, sealClientPresentation } = require("./client-presentation-artifact.cjs");
const { readGeneratedSourcePaths, assertDeclaredGeneratedOutput } = require("./generated-source-boundary.cjs");

const appRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(appRoot, "..");
const generatedOutputs = readGeneratedSourcePaths(repoRoot);
const packageVersion = JSON.parse(fs.readFileSync(path.join(appRoot, "package.json"), "utf8")).version;
if (!/^\d+\.\d+\.\d+$/.test(packageVersion)) {
  throw new Error(`package version must be a three-part numeric version: ${packageVersion}`);
}
const registryRoot = path.join(repoRoot, "registry");
const distributionRoot = path.join(appRoot, "distribution");
const baselineManifestPath = path.join(distributionRoot, "system-baseline.json");
const buildRoot = path.join(appRoot, "build");
const bundleRoot = path.join(buildRoot, "Octopus.app");
const ownedEngineVerifier = require(path.join(registryRoot, "worker_environment_kind", "browser_session", "packages", "octopus-browser-session-env", "native", "macos", "engine-artifact.cjs"));
const browserEnginePackage = require(path.join(registryRoot, "worker_environment_kind", "browser_session", "packages", "octopus-browser-session-env", "native", "engine-mode.cjs"));
const browserEngineMode = browserEnginePackage.packagingMode(process.argv.slice(2), process.env);
const ownedEngineReceiptPath = process.env.OCTOPUS_OWNED_ELECTRON_RECEIPT;
const ownedEngine = browserEngineMode === "owned"
  ? ownedEngineVerifier.verifyEngineArtifact(ownedEngineReceiptPath, process.arch)
  : null;
const electronBundle = ownedEngine ? ownedEngine.bundlePath : path.join(appRoot, "node_modules", "electron", "dist", "Electron.app");
const browserElectronVersion = ownedEngine ? ownedEngine.source.electron_version : JSON.parse(fs.readFileSync(path.join(appRoot, "node_modules", "electron", "package.json"), "utf8")).version;
console.log(`Packaging browser engine: ${browserEngineMode} (${browserEnginePackage.engineInfo(browserEngineMode).presentationProtocol}).`);
const resourcesRoot = path.join(bundleRoot, "Contents", "Resources");
const bundledAppRoot = path.join(resourcesRoot, "app");
const macosRoot = path.join(bundleRoot, "Contents", "MacOS");
const plistPath = path.join(bundleRoot, "Contents", "Info.plist");
const icnsPath = path.join(appRoot, "public", "octopus.icns");
const openCodexOwnerRoot = path.join(
  registryRoot,
  "interface",
  "world_provider_codex",
  "packages",
  "octopus-provider-codex",
);
const openCodexBridgeRoot = path.join(openCodexOwnerRoot, "opencodex-bridge");
const managedCodexRoot = path.join(openCodexOwnerRoot, "managed-cli");
const managedCodexDistribution = require(path.join(managedCodexRoot, "bootstrap.cjs"));
const openCodexLifeBinary = "octopus-provider-opencodex-life";
const inputIntakeLifeBinary = "octopus-input-intake-life";

for (const asset of [
  path.join(appRoot, "public", "favicon.png"),
  path.join(appRoot, "public", "octopus-app-icon.png"),
  path.join(appRoot, "public", "octopus-dev-dock-icon.png"),
  icnsPath,
]) {
  if (!fs.existsSync(asset)) throw new Error(`packaging asset is missing: ${asset}`);
}
run("npm", ["run", "build"], appRoot);
buildOpenCodexBridge();
const desktopNativeRoot = path.join(registryRoot, "worker_environment_kind", "desktop_session", "packages", "octopus-system-gui-env", "native");
const desktopNativeExecutable = path.join(desktopNativeRoot, "bin", "octopus-desktop-native");
assertDeclaredGeneratedOutput(desktopNativeExecutable, repoRoot, generatedOutputs);
require(path.join(desktopNativeRoot, "build.cjs")).buildDesktopNative(desktopNativeExecutable);
const pipBuilder = require(path.join(desktopNativeRoot, "pip", "build.cjs"));
const pipHeaders = process.env.OCTOPUS_NATIVE_NODE_HEADERS;
if (!pipHeaders) throw new Error("Packaging native PiP requires explicit OCTOPUS_NATIVE_NODE_HEADERS for Node24.15.0.");
const pipCompiler = fs.realpathSync(capture("xcrun", ["--find", "clang++"]));
const pipSwiftCompiler = fs.realpathSync(capture("xcrun", ["--find", "swiftc"]));
const pipSdk = fs.realpathSync(capture("xcrun", ["--sdk", "macosx", "--show-sdk-path"]));
const pipOutput = fs.realpathSync(fs.mkdtempSync(path.join(require("node:os").tmpdir(), "octopus-package-pip-")));
process.once("exit", () => fs.rmSync(pipOutput, { recursive: true, force: true }));
const pipReceipt = pipBuilder.build({ compiler: pipCompiler, swiftCompiler: pipSwiftCompiler, sdk: pipSdk, headers: fs.realpathSync(pipHeaders),
  outputRoot: pipOutput, architecture: process.arch, target: "addon", deploymentTarget: "14.0" });
const bundledBinaries = collectBundledBinaries(registryRoot);
buildBundledBinaries([...bundledBinaries, openCodexLifeBinary, inputIntakeLifeBinary]);
materializeProviderLife(openCodexOwnerRoot, openCodexLifeBinary);
materializeProviderLife(path.join(openCodexOwnerRoot, "intake"), inputIntakeLifeBinary);
validateOpenCodexOwnerAssets(openCodexOwnerRoot);

fs.rmSync(bundleRoot, { recursive: true, force: true });
fs.mkdirSync(buildRoot, { recursive: true });
copyTree(electronBundle, bundleRoot, { recursive: true, verbatimSymlinks: true });
const ownedEngineCopy = ownedEngine ? ownedEngineVerifier.beginPackagedEngine(ownedEngine, bundleRoot) : null;

const electronExecutable = path.join(macosRoot, "Electron");
const octopusExecutable = path.join(macosRoot, "Octopus");
if (fs.existsSync(electronExecutable)) {
  fs.renameSync(electronExecutable, octopusExecutable);
}

fs.rmSync(path.join(resourcesRoot, "default_app.asar"), { force: true });
fs.rmSync(bundledAppRoot, { recursive: true, force: true });
fs.mkdirSync(bundledAppRoot, { recursive: true });
publishClientPresentation({ sourceRoot: path.join(desktopNativeRoot, "pip"), buildReceipt: pipReceipt, destination: path.join(resourcesRoot, "native-presentation") });

for (const item of ["dist", "electron", "package.json", "schemas", "distribution"]) {
  copyTree(path.join(appRoot, item), path.join(bundledAppRoot, item), { recursive: true });
}
copyTree(path.join(repoRoot, "schemas"), path.join(resourcesRoot, "schemas"), { recursive: true });
const bundledRegistryRoot = path.join(bundledAppRoot, "registry");
const localPackageRoot = path.join(bundledAppRoot, "local-packages");
const localPackageRegistryRoot = path.join(localPackageRoot, "registry");
copyRegistryBaseline(registryRoot, bundledRegistryRoot, baselineManifestPath);
copyRegistrySurface(registryRoot, localPackageRegistryRoot);
copyLocalPackageDefinitions(path.join(distributionRoot, "local-packages"), localPackageRoot);
const executableOwnership = registryExecutableOwnership(localPackageRegistryRoot);
const ownerExecutables = new Set();
rewriteBundledExecutablePaths(localPackageRegistryRoot, executableOwnership, ownerExecutables);
rewriteBundledExecutablePaths(bundledRegistryRoot, executableOwnership, ownerExecutables);
const installedBinaries = copyBundledBinaries(bundledBinaries);
stripExecutables([...ownerExecutables, ...installedBinaries]);
const managedCodexRoots = bundleManagedCodex([bundledRegistryRoot, localPackageRegistryRoot]);
for (const dependency of [
  "@noble/hashes",
  "ajv",
  "fast-deep-equal",
  "fast-uri",
  "json-schema-traverse",
  "node-pty",
  "require-from-string",
  "ws",
]) {
  const destination = path.join(bundledAppRoot, "node_modules", dependency);
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  copyTree(path.join(appRoot, "node_modules", dependency), destination, { recursive: true });
}
copyFile(icnsPath, path.join(resourcesRoot, "octopus.icns"));
validateBundledLayout([bundledRegistryRoot, localPackageRegistryRoot], bundledBinaries, ownerExecutables);

setPlist("CFBundleName", "Octopus");
setPlist("CFBundleDisplayName", "Octopus");
setPlist("CFBundleExecutable", "Octopus");
setPlist("CFBundleIdentifier", "com.octopus.desktop");
setPlist("CFBundleIconFile", "octopus.icns");
setPlist("CFBundleShortVersionString", packageVersion);
setPlist("CFBundleVersion", packageVersion);
setPlist("NSMicrophoneUsageDescription", "Octopus uses the microphone for persistent voice conversations with your agent.");
setPlist("NSCameraUsageDescription", "Octopus uses the camera locally to recognize hand gestures. Frames are not recorded or sent to the runtime.");
setPlistBoolean("NSCameraUseContinuityCameraDeviceType", true);

const providerSigningInputs = captureProviderSigningInputs([bundledRegistryRoot, localPackageRegistryRoot]);
const managedCodexSigningInputs = managedCodexDistribution.captureSigning(managedCodexRoots);
if (!ownedEngine) {
  for (const entry of fs.readdirSync(path.join(bundleRoot, "Contents", "Frameworks"))) {
    const nested = path.join(bundleRoot, "Contents", "Frameworks", entry);
    if (entry.endsWith(".framework") || entry.endsWith(".app")) run("codesign", ["--force", "--deep", "--sign", "-", nested], appRoot);
  }
}
signClientResources(resourcesRoot);
managedCodexDistribution.sealSigning(managedCodexSigningInputs);
sealProviderSigningInputs(providerSigningInputs);
sealClientPresentation(path.join(resourcesRoot, "native-presentation"));
if (ownedEngineCopy) ownedEngineVerifier.sealPackagedEngine(ownedEngineCopy);
browserEnginePackage.sealBrowserEngine({ mode: browserEngineMode, bundleRoot, architecture: process.arch, electronVersion: browserElectronVersion });
run("codesign", ["--force", "--sign", "-", bundleRoot], appRoot);
if (ownedEngineCopy) ownedEngineVerifier.verifyPackagedEngineAfterSigning(ownedEngineCopy);
run("codesign", ["--verify", "--deep", "--strict", bundleRoot], appRoot);

console.log(`Packaged ${bundleRoot}`);

function bundleManagedCodex(registries) {
  const sharedBin = path.join(bundledAppRoot, "bin");
  run(process.execPath, [path.join(managedCodexRoot, "bootstrap.cjs"), "materialize", "--destination", sharedBin], appRoot);
  const roots = [sharedBin];
  for (const registry of registries) {
    const owner = path.join(registry, "interface", "world_provider_codex");
    if (!fs.existsSync(owner)) continue;
    const destination = path.join(owner, "packages", "bin");
    managedCodexDistribution.copyInstallation(sharedBin, destination);
    roots.push(destination);
  }
  return roots;
}

function captureProviderSigningInputs(registries) {
  const records = [];
  for (const registry of registries) {
    const provider = path.join(registry, "interface", "world_provider_codex", "packages", "octopus-provider-codex");
    if (!fs.existsSync(provider)) continue;
    for (const owner of [provider, path.join(provider, "intake"), path.join(provider, "opencodex-bridge")]) {
      const receiptPath = path.join(owner, "build-receipt.json");
      const receipt = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
      const executable = path.join(owner, receipt.executable);
      const bytes = fs.readFileSync(executable);
      if (bytes.length !== receipt.executable_bytes || crypto.createHash("sha256").update(bytes).digest("hex") !== receipt.executable_sha256) {
        throw new Error(`Provider compiler receipt does not match the unsigned packaging input: ${executable}`);
      }
      records.push({ receiptPath, executable, before: receipt });
    }
  }
  return records;
}

function sealProviderSigningInputs(records) {
  const transformations = records.map(({ receiptPath, executable, before }) => {
    run("codesign", ["--verify", "--strict", executable], appRoot);
    const bytes = fs.readFileSync(executable);
    const after = { ...before, executable_sha256: crypto.createHash("sha256").update(bytes).digest("hex"), executable_bytes: bytes.length };
    fs.writeFileSync(receiptPath, `${JSON.stringify(after, null, 2)}\n`, { mode: 0o644 });
    return { receipt: path.relative(bundleRoot, receiptPath), signing: "ad-hoc", before, after };
  });
  fs.writeFileSync(path.join(buildRoot, "provider-signing.json"), `${JSON.stringify({ schema_version: 1, transformations }, null, 2)}\n`);
}

function signClientResources(root) {
  const pending = [root]; let entries = 0;
  const executables = [];
  for (let cursor = 0; cursor < pending.length; cursor++) for (const name of fs.readdirSync(pending[cursor]).sort()) {
    if (++entries > 100000) throw new Error("Client code signing inventory exceeds its bound");
    const file = path.join(pending[cursor], name), stat = fs.lstatSync(file);
    if (stat.isSymbolicLink()) {
      if (!fs.realpathSync(file).startsWith(root + path.sep)) throw new Error("Client signing source symlink escapes resources");
      continue;
    }
    if (stat.isDirectory()) { pending.push(file); continue; }
    if (!stat.isFile()) throw new Error("Client signing source must be a file or directory");
    const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW), header = Buffer.alloc(4);
    let count; try { count = fs.readSync(fd, header, 0, 4, 0); } finally { fs.closeSync(fd); }
    if (count === 4 && ["cefaedfe","cffaedfe","feedface","feedfacf","cafebabe","bebafeca","cafebabf","bfbafeca"].includes(header.toString("hex"))) {
      executables.push(file);
    }
  }
  transformExecutableCopies(executables, file => run("codesign", ["--force", "--sign", "-", file], appRoot));
}

function copyRegistrySurface(source, destination) {
  copyTree(source, destination, {
    recursive: true,
    verbatimSymlinks: true,
    filter: registryCopyFilter,
  });
}

function copyLocalPackageDefinitions(source, destination) {
  for (const directory of ["packages", "packs"]) {
    const from = path.join(source, directory);
    const to = path.join(destination, directory);
    if (!fs.statSync(from).isDirectory()) throw new Error(`local package definition root is missing: ${from}`);
    copyTree(from, to, { recursive: true, verbatimSymlinks: true });
  }
}

function copyRegistryBaseline(source, destination, manifestPath) {
  const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  if (
    manifest?.schema_version !== 1
    || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(manifest?.id || "")
    || !Array.isArray(manifest?.registry_owners)
  ) {
    throw new Error(`system baseline manifest is invalid: ${manifestPath}`);
  }
  const identity = JSON.parse(fs.readFileSync(path.join(source, ".octopus-source.json"), "utf8"));
  if (identity?.schema_version !== 1 || identity?.id !== manifest.id) {
    throw new Error(`system baseline identity disagrees with registry source: ${manifestPath}`);
  }
  fs.rmSync(destination, { recursive: true, force: true });
  fs.mkdirSync(destination, { recursive: true });
  copyFile(path.join(source, ".octopus-source.json"), path.join(destination, ".octopus-source.json"));
  for (const entry of fs.readdirSync(source, { withFileTypes: true })) {
    if (entry.isDirectory() && !["target", "node_modules", "__pycache__", ".git"].includes(entry.name)) {
      fs.mkdirSync(path.join(destination, entry.name), { recursive: true });
    }
  }
  const seen = new Set();
  for (const owner of manifest.registry_owners) {
    if (
      !owner
      || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(owner.kind || "")
      || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(owner.id || "")
    ) {
      throw new Error(`system baseline contains an invalid registry owner: ${JSON.stringify(owner)}`);
    }
    const key = `${owner.kind}/${owner.id}`;
    if (seen.has(key)) throw new Error(`system baseline repeats registry owner '${key}'`);
    seen.add(key);
    const ownerSource = path.join(source, owner.kind, owner.id);
    const ownerTarget = path.join(destination, owner.kind, owner.id);
    if (!fs.statSync(ownerSource).isDirectory()) throw new Error(`system baseline owner is missing: ${ownerSource}`);
    copyTree(ownerSource, ownerTarget, {
      recursive: true,
      verbatimSymlinks: true,
      filter: registryCopyFilter,
    });
  }
}

function registryCopyFilter(candidate) {
  return !candidate.split(path.sep).some((part) => (
    part === ".git"
    || part === "target"
    || part === "node_modules"
    || part === "__pycache__"
    || part === ".DS_Store"
  ));
}

function collectBundledBinaries(source) {
  const binaries = new Set([
    "octopus",
    "octopus-operator-chat-artifact",
    "octopus-operator-chat-topology",
    "octopus-secret-store",
    "octopus-world-cron-task-plan",
  ]);
  visitJson(source, (value) => {
    if (typeof value !== "string") return;
    const match = /^target\/debug\/([^/]+)$/.exec(value);
    if (match) binaries.add(match[1]);
  });
  return [...binaries].sort();
}

function buildOpenCodexBridge() {
  run("npm", ["ci", "--ignore-scripts"], openCodexBridgeRoot);
  run("npm", ["run", "build"], openCodexBridgeRoot);
}

function materializeProviderLife(ownerRoot, binary) {
  const source = path.join(repoRoot, "target", "release", binary);
  const destination = path.join(ownerRoot, "life-bin", binary);
  assertDeclaredGeneratedOutput(destination, repoRoot, generatedOutputs);
  assertDeclaredGeneratedOutput(path.join(ownerRoot, "build-receipt.json"), repoRoot, generatedOutputs);
  const stat = fs.statSync(source);
  if (!stat.isFile()) throw new Error(`built provider life is not a regular file: ${source}`);
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  copyFile(source, destination);
  fs.chmodSync(destination, 0o755);
  const bytes = fs.readFileSync(destination);
  const receipt = {
    schema_version: 1,
    executable: `life-bin/${binary}`,
    executable_sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
    executable_bytes: bytes.length,
    inputs: {
      "Cargo.lock": crypto.createHash("sha256").update(fs.readFileSync(path.join(repoRoot, "Cargo.lock"))).digest("hex"),
      "Cargo.toml": crypto.createHash("sha256").update(fs.readFileSync(path.join(openCodexOwnerRoot, "Cargo.toml"))).digest("hex"),
    },
  };
  fs.writeFileSync(path.join(ownerRoot, "build-receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`, { mode: 0o644 });
}

function validateOpenCodexOwnerAssets(ownerRoot) {
  const life = path.join(ownerRoot, "life-bin", openCodexLifeBinary);
  const lifeReceiptFile = path.join(ownerRoot, "build-receipt.json");
  const bridgeRoot = path.join(ownerRoot, "opencodex-bridge");
  const bridge = path.join(bridgeRoot, "bin", "octopus-opencodex-bridge");
  const receiptFile = path.join(bridgeRoot, "build-receipt.json");
  for (const executable of [life, bridge]) {
    const stat = fs.statSync(executable);
    if (!stat.isFile() || (stat.mode & 0o111) === 0) {
      throw new Error(`OpenCodex owner executable is not runnable: ${executable}`);
    }
  }
  const lifeBytes = fs.readFileSync(life);
  const lifeReceipt = JSON.parse(fs.readFileSync(lifeReceiptFile, "utf8"));
  const lifeDigest = crypto.createHash("sha256").update(lifeBytes).digest("hex");
  if (
    lifeReceipt?.schema_version !== 1
    || lifeReceipt.executable !== `life-bin/${openCodexLifeBinary}`
    || lifeReceipt.executable_bytes !== lifeBytes.length
    || lifeReceipt.executable_sha256 !== lifeDigest
  ) {
    throw new Error(`OpenCodex owner build receipt does not attest the imported life: ${lifeReceiptFile}`);
  }
  const receipt = JSON.parse(fs.readFileSync(receiptFile, "utf8"));
  const bridgeBytes = fs.readFileSync(bridge);
  const bridgeDigest = crypto.createHash("sha256").update(bridgeBytes).digest("hex");
  if (
    receipt?.schema_version !== 1
    || receipt.executable !== "bin/octopus-opencodex-bridge"
    || receipt.executable_bytes !== bridgeBytes.length
    || receipt.executable_sha256 !== bridgeDigest
    || receipt?.upstream?.npm_package !== "@bitkyc08/opencodex@2.39.0"
    || receipt?.upstream?.commit !== "af6113a0381d6fff2e4dce587652825c7eeb6423"
  ) {
    throw new Error(`OpenCodex owner build receipt does not attest the packaged bridge: ${receiptFile}`);
  }
}

function visitJson(root, visit) {
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    const candidate = path.join(root, entry.name);
    if (!registryCopyFilter(candidate)) continue;
    if (entry.isDirectory()) {
      visitJson(candidate, visit);
      continue;
    }
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    walkJson(JSON.parse(fs.readFileSync(candidate, "utf8")), visit);
  }
}

function walkJson(value, visit) {
  visit(value);
  if (Array.isArray(value)) {
    for (const item of value) walkJson(item, visit);
  } else if (value && typeof value === "object") {
    for (const item of Object.values(value)) walkJson(item, visit);
  }
}

function buildBundledBinaries(binaries) {
  const args = ["build", "--release"];
  for (const binary of binaries) args.push("--bin", binary);
  run("cargo", args, repoRoot);
}

function copyBundledBinaries(binaries) {
  const destination = path.join(bundledAppRoot, "bin");
  fs.mkdirSync(destination, { recursive: true });
  const installed = [];
  for (const binary of binaries) {
    const source = path.join(repoRoot, "target", "release", binary);
    if (!fs.existsSync(source)) throw new Error(`built runtime binary is missing: ${source}`);
    const target = path.join(destination, binary);
    copyFile(source, target);
    fs.chmodSync(target, 0o755);
    installed.push(target);
  }
  return installed;
}

function registryExecutableOwnership(registry) {
  const toolBinaryKinds = new Map();
  const doorInterfaceKinds = new Map();
  const kindRoot = path.join(registry, "worker_environment_kind");
  for (const entry of fs.readdirSync(kindRoot, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const file = path.join(kindRoot, entry.name, "worker_environment_kind.json");
    if (!fs.existsSync(file)) continue;
    const descriptor = JSON.parse(fs.readFileSync(file, "utf8"));
    if (descriptor.worker_environment_kind_id !== entry.name || !Array.isArray(descriptor.tool_process_bins)) {
      throw new Error(`worker environment kind has invalid executable ownership: ${file}`);
    }
    for (const declared of descriptor.tool_process_bins) {
      const match = /^target\/debug\/([^/]+)$/.exec(declared);
      if (!match) continue;
      const kinds = toolBinaryKinds.get(match[1]) ?? new Set();
      kinds.add(entry.name);
      toolBinaryKinds.set(match[1], kinds);
    }
  }
  const toolRoot = path.join(registry, "tool");
  for (const entry of fs.readdirSync(toolRoot, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const file = path.join(toolRoot, entry.name, "tool.json");
    if (!fs.existsSync(file)) continue;
    const descriptor = JSON.parse(fs.readFileSync(file, "utf8"));
    if (
      descriptor.id !== entry.name
      || !Array.isArray(descriptor.supported_worker_environment_kinds)
      || descriptor.supported_worker_environment_kinds.length !== 1
      || typeof descriptor.interface !== "string"
    ) throw new Error(`tool descriptor has invalid executable ownership: ${file}`);
    const kind = descriptor.supported_worker_environment_kinds[0];
    const kinds = doorInterfaceKinds.get(descriptor.interface) ?? new Set();
    kinds.add(kind);
    doorInterfaceKinds.set(descriptor.interface, kinds);
  }
  return { registry, toolBinaryKinds, doorInterfaceKinds };
}

function rewriteBundledExecutablePaths(root, ownership, ownerExecutables) {
  visitJsonFiles(root, (file, value) => {
    const relative = path.relative(root, file).split(path.sep);
    const registryKind = relative[0] || "";
    const owner = relative[1] || "";
    let rewritten = mapJsonEntries(value, [], (item, keys) => {
      if (typeof item !== "string") return item;
      const match = /^target\/debug\/([^/]+)$/.exec(item);
      if (!match) return item;
      const binary = match[1];
      const key = keys.at(-1);
      if (registryKind === "interface" && key === "bin") {
        materializeOwnerExecutable(root, registryKind, owner, binary, ownerExecutables);
        return `owner://packages/bin/${binary}`;
      }
      if (registryKind === "tool" && key === "bin") {
        const descriptor = value;
        const kinds = Array.isArray(descriptor.supported_worker_environment_kinds)
          ? descriptor.supported_worker_environment_kinds
          : [];
        if (kinds.length !== 1) throw new Error(`tool '${owner}' has no exact executable owner`);
        materializeOwnerExecutable(root, "worker_environment_kind", kinds[0], binary, ownerExecutables);
        return `owner://packages/bin/${binary}`;
      }
      if (registryKind === "worker_environment_kind" && keys.includes("tool_process_bins")) {
        materializeOwnerExecutable(root, registryKind, owner, binary, ownerExecutables);
        return `owner://packages/bin/${binary}`;
      }
      if (registryKind === "interface" && keys.includes("executable_paths")) {
        const kind = executableKindForInterface(ownership, owner, binary);
        if (kind) {
          materializeOwnerExecutable(root, "worker_environment_kind", kind, binary, ownerExecutables);
          return path.posix.join("..", "..", "worker_environment_kind", kind, "packages", "bin", binary);
        }
        materializeOwnerExecutable(root, registryKind, owner, binary, ownerExecutables);
        return `packages/bin/${binary}`;
      }
      if (registryKind === "interface" && keys.includes("executables")) {
        materializeOwnerExecutable(root, registryKind, owner, binary, ownerExecutables);
        return `owner://packages/bin/${binary}`;
      }
      if (registryKind === "tool" && keys.includes("executable_paths")) {
        const descriptor = value;
        const kinds = Array.isArray(descriptor.supported_worker_environment_kinds)
          ? descriptor.supported_worker_environment_kinds
          : [];
        if (kinds.length !== 1) throw new Error(`tool '${owner}' has no exact executable owner`);
        materializeOwnerExecutable(root, "worker_environment_kind", kinds[0], binary, ownerExecutables);
        return `packages/bin/${binary}`;
      }
      return `bin/${binary}`;
    });
    if (registryKind === "interface") {
      rewritten = rewriteInterfaceExecutableImports(rewritten);
    }
    fs.writeFileSync(file, `${JSON.stringify(rewritten, null, 2)}\n`);
  });
}

function rewriteInterfaceExecutableImports(value) {
  if (Array.isArray(value)) return value.map(rewriteInterfaceExecutableImports);
  if (!value || typeof value !== "object") return value;
  const rewritten = Object.fromEntries(Object.entries(value).map(([key, item]) => [
    key,
    rewriteInterfaceExecutableImports(item),
  ]));
  if (!Array.isArray(rewritten.executable_paths)) return rewritten;
  const retained = [];
  const imports = Array.isArray(rewritten.executable_root_subpaths)
    ? [...rewritten.executable_root_subpaths]
    : [];
  for (const executable of rewritten.executable_paths) {
    const match = /^\.\.\/\.\.\/worker_environment_kind\/([^/]+)\/packages\/bin\/([^/]+)$/.exec(executable);
    if (!match) {
      retained.push(executable);
      continue;
    }
    imports.push({
      root_slot: "registry_worker_environment_kind_root",
      relative_path: `${match[1]}/packages/bin/${match[2]}`,
    });
  }
  rewritten.executable_paths = retained;
  if (imports.length) {
    rewritten.executable_root_subpaths = imports.sort((left, right) => (
      `${left.root_slot}/${left.relative_path}`.localeCompare(`${right.root_slot}/${right.relative_path}`)
    ));
  }
  return rewritten;
}

function executableKindForInterface(ownership, interfaceId, binary) {
  const interfaceKinds = ownership.doorInterfaceKinds.get(interfaceId) ?? new Set();
  const binaryKinds = ownership.toolBinaryKinds.get(binary) ?? new Set();
  const candidates = [...interfaceKinds].filter((kind) => binaryKinds.has(kind));
  if (candidates.length > 1) {
    throw new Error(`interface '${interfaceId}' has ambiguous owner for executable '${binary}'`);
  }
  if (candidates.length === 1) return candidates[0];
  if (binaryKinds.size === 1) return [...binaryKinds][0];
  return "";
}

function materializeOwnerExecutable(registry, registryKind, owner, binary, ownerExecutables) {
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(owner)) throw new Error(`invalid executable owner '${registryKind}/${owner}'`);
  const source = path.join(repoRoot, "target", "release", binary);
  if (!fs.statSync(source).isFile()) throw new Error(`built owner executable is missing: ${source}`);
  const ownerRoot = path.join(registry, registryKind, owner);
  if (!fs.statSync(ownerRoot).isDirectory()) throw new Error(`executable owner folder is missing: ${ownerRoot}`);
  const targetRoot = path.join(ownerRoot, "packages", "bin");
  const target = path.join(targetRoot, binary);
  fs.mkdirSync(targetRoot, { recursive: true });
  if (!fs.existsSync(target)) copyFile(source, target);
  fs.chmodSync(target, 0o755);
  ownerExecutables.add(target);
}

function stripExecutables(executables) {
  transformExecutableCopies(executables, file => run("strip", ["-x", file], appRoot));
}

function transformExecutableCopies(executables, transform) {
  const copies = new Map();
  for (const file of executables) {
    const digest = crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
    const key = `${path.basename(file)}:${fs.statSync(file).mode & 0o777}:${digest}`;
    if (!copies.has(key)) copies.set(key, []);
    copies.get(key).push(file);
  }
  for (const [first, ...rest] of copies.values()) {
    transform(first);
    for (const file of rest) copyFile(first, file);
  }
}

function visitJsonFiles(root, visit) {
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    const candidate = path.join(root, entry.name);
    if (!registryCopyFilter(candidate)) continue;
    if (entry.isDirectory()) {
      visitJsonFiles(candidate, visit);
      continue;
    }
    if (entry.isFile() && entry.name.endsWith(".json")) {
      visit(candidate, JSON.parse(fs.readFileSync(candidate, "utf8")));
    }
  }
}

function mapJsonEntries(value, keys, transform) {
  if (Array.isArray(value)) return transform(value.map((item, index) => mapJsonEntries(item, [...keys, String(index)], transform)), keys);
  if (value && typeof value === "object") {
    return transform(Object.fromEntries(Object.entries(value).map(([key, item]) => [
      key,
      mapJsonEntries(item, [...keys, key], transform),
    ])), keys);
  }
  return transform(value, keys);
}

function validateBundledLayout(registries, binaries, ownerExecutables) {
  for (const directory of ["public", "target"]) {
    if (fs.existsSync(path.join(bundledAppRoot, directory))) {
      throw new Error(`packaged application contains development directory '${directory}'`);
    }
  }
  for (const registry of registries) {
    visitJson(registry, (value) => {
      if (typeof value !== "string") return;
      if (value.startsWith("target/debug/")) {
        throw new Error(`packaged registry retains a debug executable path: ${value}`);
      }
    });
    const ownerRoot = path.join(
      registry,
      "interface",
      "world_provider_codex",
      "packages",
      "octopus-provider-codex",
    );
    if (fs.existsSync(ownerRoot)) validateOpenCodexOwnerAssets(ownerRoot);
  }
  for (const binary of binaries) {
    const file = path.join(bundledAppRoot, "bin", binary);
    const stat = fs.statSync(file);
    if (!stat.isFile() || (stat.mode & 0o111) === 0) {
      throw new Error(`packaged executable is not a runnable regular file: ${file}`);
    }
  }
  for (const file of ownerExecutables) {
    const stat = fs.statSync(file);
    if (!stat.isFile() || (stat.mode & 0o111) === 0) {
      throw new Error(`package-owned executable is not runnable: ${file}`);
    }
  }
}

function setPlistBoolean(key, value) {
  spawnSync("/usr/libexec/PlistBuddy", ["-c", `Delete :${key}`, plistPath], { encoding: "utf8" });
  run("/usr/libexec/PlistBuddy", ["-c", `Add :${key} bool ${value ? "true" : "false"}`, plistPath], appRoot);
}

function setPlist(key, value) {
  const set = spawnSync("/usr/libexec/PlistBuddy", ["-c", `Set :${key} ${value}`, plistPath], { encoding: "utf8" });
  if (set.status === 0) return;
  run("/usr/libexec/PlistBuddy", ["-c", `Add :${key} string ${value}`, plistPath], appRoot);
}

function run(command, args, cwd) {
  const result = spawnSync(command, args, { cwd, stdio: "inherit" });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed with status ${result.status}`);
  }
}

function capture(command, args) {
  const result = spawnSync(command, args, { cwd: appRoot, encoding: "utf8", maxBuffer: 1024 * 1024 });
  if (result.error || result.status !== 0) throw new Error(`Package toolchain lookup failed: ${command}`);
  return result.stdout.trim();
}
