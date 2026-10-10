'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const verifiedEngines = new WeakMap();
const packageCopies = new WeakMap();
const packagedEngines = new WeakMap();
const ENGINE_ROOT = 'Contents/Frameworks';
const PACKAGE_RECEIPT = 'owned-browser-engine.json';
const SPEC_TITLES = [
  'retains one document, Session and debugger through transfer and embedder destruction',
  'rejects wrong frame/embedder, consumed lease and cancellation without changing page',
  'retains the exact native popup with its opener Session and independent lifetime',
  'captures before DOM or emulation, then retains the same sized page after detach',
  'keeps the model viewport and captures fresh hidden native frames without showing an owner',
  'retains the crashed WebContents and replaces only its diagnostic history entry',
  'restores a BFCache document across native view replacement and owner destruction',
  'retains actual download GUID and SQLite rows across a fresh engine process without recreating a page',
  'records native enrolled navigation and preserves visits across a fresh process',
];
function requireValue(value, message) { if (!value) throw new Error(message); }
function canonical(file) { requireValue(typeof file === 'string' && path.isAbsolute(file) && path.normalize(file) === file && fs.realpathSync(file) === file, 'Engine evidence must use a canonical absolute path'); return file; }
function fileBytes(file, maximum = 16 * 1024 * 1024) {
  canonical(file);
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  try { const stat = fs.fstatSync(fd); requireValue(stat.isFile() && stat.size <= maximum, 'Engine evidence must be a bounded regular file'); const bytes = fs.readFileSync(fd); requireValue(bytes.length <= maximum, 'Engine evidence grew beyond its bound'); return bytes; }
  finally { fs.closeSync(fd); }
}
function hashFile(file) {
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  try {
    requireValue(fs.fstatSync(fd).isFile(), 'Engine bundle member must be a regular file');
    const hash = crypto.createHash('sha256'), buffer = Buffer.alloc(1024 * 1024); let count;
    while ((count = fs.readSync(fd, buffer, 0, buffer.length, null)) > 0) hash.update(buffer.subarray(0, count));
    return hash.digest('hex');
  } finally { fs.closeSync(fd); }
}
function receiptFile(ref) { requireValue(ref && /^[a-f0-9]{64}$/.test(ref.sha256), 'Engine evidence digest is missing'); const bytes = fileBytes(ref.path); requireValue(sha(bytes) === ref.sha256, 'Engine evidence digest changed'); return JSON.parse(bytes); }
function inventory(bundle, roots = ['']) {
  canonical(bundle); requireValue(fs.statSync(bundle).isDirectory() && bundle.endsWith('.app'), 'Owned engine must be an explicit app bundle directory');
  const members = [], pending = [...roots];
  for (const relative of roots.filter(Boolean)) {
    const file = path.join(bundle, relative), stat = fs.lstatSync(file);
    requireValue(stat.isDirectory() && !stat.isSymbolicLink() && fs.realpathSync(file) === file, 'Engine inventory root must be a canonical directory');
    members.push({ path: relative, type: 'directory', mode: stat.mode & 0o777 });
  }
  for (let cursor = 0; cursor < pending.length; cursor++) {
    const directory = pending[cursor];
    for (const name of fs.readdirSync(path.join(bundle, directory)).sort()) {
      const relative = path.posix.join(directory, name), file = path.join(bundle, relative), stat = fs.lstatSync(file);
      requireValue(members.length < 100000, 'Engine inventory exceeds the declared manifest member bound');
      const mode = stat.mode & 0o777;
      if (stat.isSymbolicLink()) {
        const link = fs.readlinkSync(file), resolved = fs.realpathSync(file);
        requireValue(!path.isAbsolute(link) && resolved.startsWith(bundle + path.sep), 'Engine framework symlink escapes its bundle');
        members.push({ path: relative, type: 'symlink', mode, link });
      } else if (stat.isDirectory()) { members.push({ path: relative, type: 'directory', mode }); pending.push(relative); }
      else { requireValue(stat.isFile(), 'Engine inventory contains a non-file member'); members.push({ path: relative, type: 'file', mode, size: stat.size, sha256: hashFile(file) }); }
    }
  }
  members.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
  return { members, sha256: sha(JSON.stringify(members)) };
}
function sourceContract() {
  const foundationBytes = fileBytes(path.join(__dirname, 'manifest.json'));
  const productionBytes = fileBytes(path.join(__dirname, 'production.json'));
  const historyBytes = fileBytes(path.join(__dirname, 'download-history.json'));
  const navigationBytes = fileBytes(path.join(__dirname, 'navigation-history.json'));
  const foundation = JSON.parse(foundationBytes), production = JSON.parse(productionBytes), history = JSON.parse(historyBytes), navigation = JSON.parse(navigationBytes);
  requireValue(production.schema_version === 1 && production.foundation_patch_sha256 === foundation.patch.sha256 && production.build_arguments.enable_owned_guest_embedder === true, 'Owned engine production source contract differs');
  requireValue(history.schema_version === 1 && history.foundation_patch_sha256 === foundation.patch.sha256 && history.production_patch_sha256 === production.patch.sha256
    && production.download_history?.path === 'download-history.json' && production.download_history.sha256 === sha(historyBytes), 'Owned download history source contract differs');
  requireValue(navigation.schema_version === 1 && navigation.foundation_patch_sha256 === foundation.patch.sha256
    && navigation.production_patch_sha256 === production.patch.sha256 && navigation.download_history_patch_sha256 === history.patch.sha256
    && navigation.download_history_manifest_sha256 === sha(historyBytes) && production.navigation_history?.path === 'navigation-history.json'
    && production.navigation_history.sha256 === sha(navigationBytes), 'Owned navigation history source contract differs');
  for (const descriptor of [foundation.patch, production.patch, history.patch, navigation.patch]) requireValue(sha(fileBytes(path.join(__dirname, descriptor.path))) === descriptor.sha256, 'Owned engine patch hash differs');
  return { foundation, production, history, navigation, navigation_sha256: sha(navigationBytes), manifest_sha256: sha(foundationBytes), production_sha256: sha(productionBytes), history_sha256: sha(historyBytes) };
}
function validatePinnedSource(source, contract = sourceContract()) {
  const { foundation, production } = contract;
  requireValue(source && source.electron_commit === foundation.electron.commit && source.chromium_commit === foundation.chromium.commit && source.node_version === foundation.node_version && source.electron_version === foundation.electron.version && source.chromium_version === foundation.chromium.version
    && source.foundation_patch_sha256 === foundation.patch.sha256 && source.production_patch_sha256 === production.patch.sha256 && source.manifest_sha256 === contract.manifest_sha256 && source.production_manifest_sha256 === contract.production_sha256
    && source.download_history_patch_sha256 === contract.history.patch.sha256 && source.download_history_manifest_sha256 === contract.history_sha256
    && source.navigation_history_patch_sha256 === contract.navigation.patch.sha256 && source.navigation_history_manifest_sha256 === contract.navigation_sha256, 'Engine receipt does not match the pinned current source');
  return contract;
}
function validateSource(record, architecture) {
  const contract = sourceContract(), { foundation, production } = contract;
  requireValue(record && record.schema_version === 1 && record.engine_api === 'electron.ownedGuest.v1' && record.variant === production.variant && record.platform === 'darwin' && record.architecture === architecture, 'Engine receipt does not identify the selected native variant');
  const source = record.source;
  validatePinnedSource(source, contract);
  const build = receiptFile(record.build);
  requireValue(build.schema_version === 1 && build.exit_code === 0 && build.architecture === architecture && build.target === 'electron:electron' && build.variant === production.variant, 'Owned engine build did not complete for this architecture');
  requireValue(build.source && Object.keys(source).every(key => build.source[key] === source[key]), 'Native build source identity differs');
  requireValue(build.arguments?.enable_owned_guest_embedder === true && build.arguments?.target_os === 'mac' && build.arguments?.target_cpu === architecture && build.arguments?.electron_vendor_version === 'octopus_owned_guest:1' && build.arguments?.cc_wrapper === '' && build.arguments?.use_remoteexec === false, 'Owned engine build arguments differ');
  requireValue(build.runtime_feature_switches?.length === 0 && build.testing_passkey_used === false, 'Engine activation cannot use runtime switches or testing PassKeys');
  const release = receiptFile(build.release_patch_series);
  requireValue(release.electron_commit === source.electron_commit && release.chromium_commit === source.chromium_commit && release.patch_list_sha256 === production.canonical_release_patch_list_sha256 && Array.isArray(release.patches) && release.patches.length > 0 && release.applied === true && release.source_tree_clean === true, 'Canonical Electron release patch series has not been verified');
  requireValue(release.source_files && [...contract.foundation.files, ...production.files, ...contract.history.files, ...contract.navigation.files].every(member => /^[a-f0-9]{64}$/.test(release.source_files[member.path] || '')), 'Final canonical release source composition is incomplete');
  for (const ref of Object.values(build.toolchain ?? {})) { canonical(ref.path); requireValue(hashFile(ref.path) === ref.sha256, 'Native compiler or SDK evidence changed'); }
  requireValue(build.toolchain?.compiler && build.toolchain?.sdk_settings && build.toolchain?.gn && build.toolchain?.ninja && build.toolchain?.python, 'Native toolchain evidence is incomplete');
  const compile = build.compile_command;
  requireValue(compile && Array.isArray(compile.command) && compile.compiler_sha256 === build.toolchain.compiler.sha256
    && compile.sdk_settings_sha256 === build.toolchain.sdk_settings.sha256
    && fs.realpathSync(path.resolve(compile.directory, compile.command[0])) === build.toolchain.compiler.path, 'Native build did not bind the actual compiler command');
  return { contract, build, release };
}
function verifyEngineArtifact(receiptPath, architecture = process.arch) {
  requireValue(['arm64', 'x64'].includes(architecture), 'Unsupported owned engine architecture');
  const bytes = fileBytes(receiptPath), record = JSON.parse(bytes);
  const { contract, build } = validateSource(record, architecture);
  requireValue(record.readiness === 'native-accepted', 'Owned engine source is not native-accepted; source apply and compile are insufficient');
  const current = inventory(record.bundle_path);
  requireValue(current.sha256 === record.bundle_inventory_sha256 && build.bundle_inventory_sha256 === current.sha256, 'Owned engine bundle differs from its compiled receipt');
  const acceptance = receiptFile(record.native_acceptance);
  requireValue(acceptance.schema_version === 1 && acceptance.engine_inventory_sha256 === current.sha256 && acceptance.production_patch_sha256 === contract.production.patch.sha256 && acceptance.download_history_patch_sha256 === contract.history.patch.sha256 && acceptance.navigation_history_patch_sha256 === contract.navigation.patch.sha256 && acceptance.runtime_feature_switches?.length === 0 && acceptance.testing_passkey_used === false, 'Native acceptance belongs to another engine or activation route');
  const spec = contract.navigation.files.find(value => value.path === 'electron/spec/api-owned-guest-spec.ts');
  requireValue(acceptance.spec_source_sha256 === spec.after_sha256, 'Native acceptance used another owned guest spec');
  const nativeRun = receiptFile(acceptance.native_spec_run);
  requireValue(nativeRun.schema_version === 1 && nativeRun.exit_code === 0 && nativeRun.engine_inventory_sha256 === current.sha256
    && nativeRun.executable === path.join(record.bundle_path, 'Contents/MacOS/Electron') && nativeRun.spec_source_sha256 === spec.after_sha256
    && nativeRun.runner_sha256 === sha(fileBytes(path.join(__dirname, 'native-spec-runner.cjs'))) && nativeRun.runtime_feature_switches?.length === 0,
    'Native spec execution does not identify this compiled engine');
  const mocha = receiptFile(nativeRun.result);
  requireValue(mocha.engine?.inventory_sha256 === current.sha256 && mocha.engine?.electron_version === contract.foundation.electron.version && mocha.engine?.node_version === contract.foundation.node_version
    && mocha.spec_source_sha256 === spec.after_sha256 && mocha.engine?.executable === nativeRun.executable
    && mocha.stats?.failures === 0 && mocha.stats?.pending === 0 && Array.isArray(mocha.passes) && Array.isArray(mocha.failures) && mocha.failures.length === 0, 'Owned guest native spec did not fully pass');
  for (const title of SPEC_TITLES) requireValue(mocha.passes.some(test => test.fullTitle === `owned guest presentation ${title}`), `Missing real native spec: ${title}`);
  const physical = receiptFile(acceptance.physical_input);
  requireValue(physical.schema_version === 1 && physical.engine_inventory_sha256 === current.sha256 && physical.no_host_autofocus === true && physical.physical_pointer === true && physical.physical_keyboard === true && physical.cdp_input === true && physical.same_web_contents === true && physical.owner_accepted === true, 'Physical native input and focus have not been accepted for this engine');
  const result = Object.freeze({ bundlePath: record.bundle_path, receiptSha256: sha(bytes), inventorySha256: current.sha256, source: Object.freeze({ ...record.source }), nativeAcceptanceSha256: record.native_acceptance.sha256, architecture });
  const retained = current.members.filter(member => member.path === ENGINE_ROOT || member.path.startsWith(ENGINE_ROOT + '/'));
  requireValue(retained.some(member => member.type === 'file' && member.path === 'Contents/Frameworks/Electron Framework.framework/Versions/A/Electron Framework'), 'Accepted engine has no native framework binary');
  verifiedEngines.set(result, { retainedSha256: sha(JSON.stringify(retained)), memberCount: retained.length });
  return result;
}

function beginPackagedEngine(verified, bundleRoot) {
  const accepted = verifiedEngines.get(verified);
  requireValue(accepted, 'Packaging requires this verifier\'s actual accepted engine result');
  canonical(bundleRoot);
  requireValue(bundleRoot !== verified.bundlePath && !bundleRoot.startsWith(verified.bundlePath + path.sep), 'Packaging must preserve the accepted source bundle');
  requireValue(inventory(bundleRoot).sha256 === verified.inventorySha256, 'Copied engine differs before client packaging transformations');
  requireValue(inventory(verified.bundlePath).sha256 === verified.inventorySha256, 'Accepted source engine changed while copying');
  const token = Object.freeze({});
  packageCopies.set(token, { verified, accepted, bundleRoot, sealed: false });
  return token;
}
function assertRetainedEngine(copy) {
  const current = inventory(copy.bundleRoot, [ENGINE_ROOT]);
  requireValue(current.sha256 === copy.accepted.retainedSha256 && current.members.length === copy.accepted.memberCount,
    'Packaging changed the accepted native engine framework, helper, resource or signature bytes');
  return current;
}
function sealPackagedEngine(token) {
  const copy = packageCopies.get(token);
  requireValue(copy && !copy.sealed, 'Engine package copy is missing or already sealed');
  const { verified, accepted, bundleRoot } = copy;
  validatePinnedSource(verified.source);
  assertRetainedEngine(copy);
  const record = {
    schema_version: 2, producer: 'octopus.browser.engine-package', producer_sha256: sha(fileBytes(__filename)),
    engine_api: 'electron.ownedGuest.v1', history_api: 'electron.ownedNavigationHistory.v1', readiness: 'native-accepted',
    architecture: verified.architecture, source: verified.source,
    source_receipt_sha256: verified.receiptSha256, source_inventory_sha256: verified.inventorySha256,
    native_acceptance_sha256: verified.nativeAcceptanceSha256,
    retained_engine: { root: ENGINE_ROOT, sha256: accepted.retainedSha256, member_count: accepted.memberCount },
    client_transformation: { executable: 'Contents/MacOS/Octopus', source_executable: 'Contents/MacOS/Electron',
      engine_frameworks: 'preserve-byte-identical', signing: 'client-resources-and-outer-bundle-only' },
  };
  const file = path.join(bundleRoot, 'Contents/Resources', PACKAGE_RECEIPT);
  fs.writeFileSync(file, JSON.stringify(record, null, 2) + '\n', { flag: 'wx', mode: 0o644 });
  copy.receiptSha256 = sha(fileBytes(file)); copy.sealed = true;
  return Object.freeze({ receiptSha256: copy.receiptSha256, retainedEngineSha256: accepted.retainedSha256 });
}
function verifyPackagedEngineAfterSigning(token) {
  const copy = packageCopies.get(token);
  requireValue(copy?.sealed, 'Engine package was not sealed before outer signing');
  assertRetainedEngine(copy);
  requireValue(sha(fileBytes(path.join(copy.bundleRoot, 'Contents/Resources', PACKAGE_RECEIPT))) === copy.receiptSha256, 'Outer signing changed the packaged engine receipt');
  requireValue(inventory(copy.verified.bundlePath).sha256 === copy.verified.inventorySha256, 'Packaging mutated the accepted source engine');
  packageCopies.delete(token);
}
function verifyPackagedEngine({ resourcesRoot, executablePath, isPackaged, architecture, nodeVersion, electronVersion }) {
  requireValue(isPackaged === true, 'Owned browser engine requires its packaged implementation receipt');
  canonical(resourcesRoot); canonical(executablePath);
  const bundleRoot = path.dirname(path.dirname(resourcesRoot)); canonical(bundleRoot);
  requireValue(resourcesRoot === path.join(bundleRoot, 'Contents/Resources') && executablePath === path.join(bundleRoot, 'Contents/MacOS/Octopus'),
    'Engine receipt does not belong to this running packaged client');
  const receiptPath = path.join(resourcesRoot, PACKAGE_RECEIPT), bytes = fileBytes(receiptPath, 65536), record = JSON.parse(bytes);
  const fields = ['schema_version','producer','producer_sha256','engine_api','history_api','readiness','architecture','source','source_receipt_sha256','source_inventory_sha256','native_acceptance_sha256','retained_engine','client_transformation'];
  requireValue(record && Object.keys(record).length === fields.length && fields.every(key => Object.hasOwn(record, key))
    && record.schema_version === 2 && record.producer === 'octopus.browser.engine-package' && record.producer_sha256 === sha(fileBytes(__filename))
    && record.engine_api === 'electron.ownedGuest.v1' && record.history_api === 'electron.ownedNavigationHistory.v1' && record.readiness === 'native-accepted'
    && ['arm64','x64'].includes(architecture) && record.architecture === architecture
    && ['source_receipt_sha256','source_inventory_sha256','native_acceptance_sha256'].every(key => /^[a-f0-9]{64}$/.test(record[key])), 'Packaged engine implementation receipt is incomplete or differs from its owner');
  const contract = validatePinnedSource(record.source);
  requireValue(nodeVersion === contract.foundation.node_version && electronVersion === contract.foundation.electron.version, 'Packaged engine runtime identity differs');
  const retained = record.retained_engine;
  requireValue(retained && Object.keys(retained).length === 3 && retained.root === ENGINE_ROOT && /^[a-f0-9]{64}$/.test(retained.sha256)
    && Number.isSafeInteger(retained.member_count) && retained.member_count > 0 && retained.member_count < 100000, 'Packaged native engine inventory is invalid');
  const transform = record.client_transformation;
  requireValue(transform && Object.keys(transform).length === 4 && transform.executable === 'Contents/MacOS/Octopus'
    && transform.source_executable === 'Contents/MacOS/Electron' && transform.engine_frameworks === 'preserve-byte-identical'
    && transform.signing === 'client-resources-and-outer-bundle-only', 'Packaged engine transformation contract differs');
  const current = inventory(bundleRoot, [ENGINE_ROOT]);
  requireValue(current.sha256 === retained.sha256 && current.members.length === retained.member_count
    && current.members.some(member => member.type === 'file' && member.path === 'Contents/Frameworks/Electron Framework.framework/Versions/A/Electron Framework'),
    'Running client native engine differs from the package-produced accepted engine inventory');
  const proof = Object.freeze({ architecture, source: Object.freeze({ ...record.source }), receiptSha256: sha(bytes),
    retainedEngineSha256: retained.sha256, nativeAcceptanceSha256: record.native_acceptance_sha256 });
  const files = [receiptPath, ...current.members.map(member => path.join(bundleRoot, member.path))];
  const stamp = file => { const stat = fs.lstatSync(file, { bigint: true }); return [stat.dev,stat.ino,stat.mode,stat.size,stat.mtimeNs,stat.ctimeNs].map(String).join(':'); };
  packagedEngines.set(proof, { receiptPath, files: files.map(file => [file, stamp(file)]), stamp });
  return proof;
}
function requirePackagedEngine(proof) {
  const checked = packagedEngines.get(proof);
  requireValue(checked, 'Native history requires the main-owned verified packaged engine handle');
  validatePinnedSource(proof.source);
  for (const [file, before] of checked.files) requireValue(checked.stamp(file) === before, 'Verified packaged engine changed; restart with a valid package');
  return proof;
}

function verifySourceCheckout(sourceRoot) {
  const { spawnSync } = require('node:child_process');
  const root = canonical(sourceRoot), contract = sourceContract();
  const electron = path.join(root, 'electron'); canonical(electron);
  const temporary = fs.mkdtempSync(path.join(require('node:os').tmpdir(), 'octopus-engine-source-index-'));
  const command = (cwd, args, index, binary = false) => {
    const result = spawnSync('git', args, { cwd, env: { ...process.env, ...(index ? { GIT_INDEX_FILE: index.path, GIT_OBJECT_DIRECTORY: index.objects, GIT_ALTERNATE_OBJECT_DIRECTORIES: JSON.stringify(index.alternate) } : {}) }, encoding: binary ? null : 'utf8', maxBuffer: 32 * 1024 * 1024, timeout: 60000 });
    if (result.error) throw result.error;
    requireValue(result.status === 0, `Pinned native source command failed: git ${args.join(' ')}: ${String(result.stderr).slice(0, 4096)}`);
    return binary ? result.stdout : result.stdout.trim();
  };
  const indexFor = (directory, commit, name) => {
    command(directory, ['cat-file', '-e', `${commit}^{commit}`]);
    const index = { path: path.join(temporary, name), objects: path.join(temporary, name + '-objects'), alternate: canonical(path.resolve(directory, command(directory, ['rev-parse', '--git-path', 'objects']))) };
    fs.mkdirSync(index.objects); command(directory, ['read-tree', commit], index); return index;
  };
  try {
    const ci = indexFor(root, contract.foundation.chromium.commit, 'chromium.index');
    const ei = indexFor(electron, contract.foundation.electron.commit, 'electron.index');
    const listPath = path.join(electron, 'patches/chromium/.patches'), list = fileBytes(listPath);
    requireValue(sha(list) === contract.production.canonical_release_patch_list_sha256, 'Canonical Chromium patch list differs from pinned Electron');
    const patches = [];
    for (const name of list.toString('utf8').split(/\r?\n/).map(value => value.trim()).filter(value => value && !value.startsWith('#'))) {
      requireValue(!path.isAbsolute(name) && !name.split('/').some(value => value === '..' || !value), 'Invalid canonical Chromium patch path');
      const relative = `patches/chromium/${name}`, file = path.join(electron, relative), bytes = fileBytes(file);
      const exact = command(electron, ['show', `${contract.foundation.electron.commit}:${relative}`], null, true);
      requireValue(sha(exact) === sha(bytes), 'Canonical Chromium patch bytes changed');
      command(root, ['apply', '--cached', '--whitespace=nowarn', file], ci);
      patches.push({ path: relative, sha256: sha(bytes) });
    }
    const foundationPatch = path.join(__dirname, contract.foundation.patch.path), productionPatch = path.join(__dirname, contract.production.patch.path);
    const electronMembers = manifest => manifest.files.filter(value => value.path.startsWith('electron/')).map(value => `--include=${value.path.slice('electron/'.length)}`);
    command(electron, ['apply', '--cached', '--whitespace=error', '-p2', ...electronMembers(contract.foundation), foundationPatch], ei);
    command(electron, ['apply', '--cached', '--whitespace=error', '-p2', ...electronMembers(contract.production), productionPatch], ei);
    command(root, ['apply', '--cached', '--whitespace=error', '--exclude=electron/*', productionPatch], ci);
    command(electron, ['apply', '--cached', '--whitespace=error', '-p2', ...electronMembers(contract.history), path.join(__dirname, contract.history.patch.path)], ei);
    command(electron, ['apply', '--cached', '--whitespace=error', '-p2', ...electronMembers(contract.navigation), path.join(__dirname, contract.navigation.patch.path)], ei);
    command(root, ['diff', '--quiet'], ci); command(electron, ['diff', '--quiet'], ei);
    const sourceFiles = {};
    for (const name of new Set([...contract.foundation.files, ...contract.production.files, ...contract.history.files, ...contract.navigation.files].map(value => value.path))) sourceFiles[name] = hashFile(path.join(root, name));
    return { schema_version: 1, electron_commit: contract.foundation.electron.commit, chromium_commit: contract.foundation.chromium.commit,
      patch_list_sha256: sha(list), patches, applied: true, source_tree_clean: true, source_files: sourceFiles,
      expected_chromium_index_sha256: sha(command(root, ['ls-files', '--stage'], ci)), expected_electron_index_sha256: sha(command(electron, ['ls-files', '--stage'], ei)) };
  } finally { fs.rmSync(temporary, { recursive: true, force: true }); }
}
function buildOwnedEngine(request) {
  const { spawnSync } = require('node:child_process');
  const contract = sourceContract(), root = canonical(request.sourceRoot), output = canonical(request.outputRoot);
  requireValue(['arm64', 'x64'].includes(request.architecture), 'Invalid native engine architecture');
  requireValue(output !== root && !output.startsWith(root + path.sep) && fs.readdirSync(output).length === 0, 'Engine receipt output must be a separate empty directory');
  const release = verifySourceCheckout(root);
  const architecture = request.architecture, toolchain = {};
  for (const name of ['compiler', 'sdk_settings', 'gn', 'ninja', 'python']) { const file = canonical(request.toolchain?.[name]); toolchain[name] = { path: file, sha256: hashFile(file) }; }
  const args = { target_os: 'mac', target_cpu: architecture, enable_owned_guest_embedder: true, electron_vendor_version: 'octopus_owned_guest:1', cc_wrapper: '', use_remoteexec: false };
  const buildDirectory = path.join(root, 'out', `OctopusOwnedGuest-${architecture}`);
  requireValue(!fs.existsSync(buildDirectory), 'Owned engine build requires a fresh exact source output directory');
  const gn = `import("//electron/build/args/release.gn") target_os="mac" target_cpu="${architecture}" enable_owned_guest_embedder=true electron_vendor_version="octopus_owned_guest:1" cc_wrapper="" use_remoteexec=false`;
  const runCompiler = (executable, arguments_, log) => {
    requireValue(fs.statfsSync(root).bavail * fs.statfsSync(root).bsize >= 5 * 1024 ** 3, 'Native engine compiler disk floor');
    const fd = fs.openSync(log, 'wx');
    try { const result = spawnSync(executable, arguments_, { cwd: root, stdio: ['ignore', fd, fd], env: process.env }); if (result.error) throw result.error; requireValue(result.status === 0, `Native engine compiler failed; inspect ${log}`); }
    finally { fs.closeSync(fd); }
  };
  runCompiler(toolchain.gn.path, ['gen', buildDirectory, `--args=${gn}`], path.join(output, 'gn.log'));
  const commandResult = spawnSync(toolchain.ninja.path, ['-C', buildDirectory, '-t', 'commands', 'obj/electron/electron_lib/owned_guest_presentation.o'], { cwd: root, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, timeout: 60000 });
  requireValue(!commandResult.error && commandResult.status === 0, 'Owned guest compiler command could not be read from the actual native build graph');
  const commands = commandResult.stdout.split(/\r?\n/).filter(value => value.includes(' -c ') && value.includes('owned_guest_presentation.cc'));
  requireValue(commands.length === 1, 'Native build graph has no unique owned guest translation unit');
  const parsed = spawnSync(toolchain.python.path, ['-c', 'import json,shlex,sys;print(json.dumps(shlex.split(sys.stdin.read())))'], { input: commands[0], encoding: 'utf8', maxBuffer: 1024 * 1024, timeout: 10000 });
  requireValue(!parsed.error && parsed.status === 0, 'Native compiler command tokenization failed');
  const command = JSON.parse(parsed.stdout), sourceIndex = command.indexOf('-c'), sdkIndex = command.indexOf('-isysroot');
  requireValue(fs.realpathSync(path.resolve(buildDirectory, command[0])) === toolchain.compiler.path && sourceIndex >= 0
    && canonical(path.resolve(buildDirectory, command[sourceIndex + 1])) === path.join(root, 'electron/shell/browser/api/owned_guest_presentation.cc'), 'Declared compiler did not own the actual native translation unit');
  requireValue(sdkIndex >= 0 && fs.realpathSync(path.resolve(buildDirectory, command[sdkIndex + 1])) === path.dirname(toolchain.sdk_settings.path), 'Declared SDK differs from the actual native compilation command');
  const compileCommand = { directory: buildDirectory, command, object: 'obj/electron/electron_lib/owned_guest_presentation.o', compiler_sha256: toolchain.compiler.sha256, sdk_settings_sha256: toolchain.sdk_settings.sha256 };
  runCompiler(toolchain.ninja.path, ['-C', buildDirectory, 'electron:electron'], path.join(output, 'ninja.log'));
  const after = verifySourceCheckout(root);
  requireValue(JSON.stringify(after) === JSON.stringify(release), 'Native engine sources changed while compiling');
  for (const ref of Object.values(toolchain)) requireValue(hashFile(ref.path) === ref.sha256, 'Native engine toolchain changed while compiling');
  const bundlePath = path.join(buildDirectory, 'Electron.app'), bundle = inventory(bundlePath);
  const write = (name, value) => { const file = path.join(output, name), bytes = Buffer.from(JSON.stringify(value, null, 2) + '\n'); fs.writeFileSync(file, bytes, { flag: 'wx' }); return { path: file, sha256: sha(bytes) }; };
  const source = { electron_commit: contract.foundation.electron.commit, chromium_commit: contract.foundation.chromium.commit,
    electron_version: contract.foundation.electron.version, chromium_version: contract.foundation.chromium.version, node_version: contract.foundation.node_version,
    foundation_patch_sha256: contract.foundation.patch.sha256, production_patch_sha256: contract.production.patch.sha256,
    manifest_sha256: contract.manifest_sha256, production_manifest_sha256: contract.production_sha256,
    download_history_patch_sha256: contract.history.patch.sha256, download_history_manifest_sha256: contract.history_sha256,
    navigation_history_patch_sha256: contract.navigation.patch.sha256, navigation_history_manifest_sha256: contract.navigation_sha256 };
  const releaseRef = write('release-source.json', release);
  const build = write('build.json', { schema_version: 1, exit_code: 0, target: 'electron:electron', variant: contract.production.variant, architecture,
    source, arguments: args, toolchain, compile_command: compileCommand, source_root: root, runtime_feature_switches: [], testing_passkey_used: false, release_patch_series: releaseRef, bundle_inventory_sha256: bundle.sha256 });
  return write('candidate.json', { schema_version: 1, engine_api: 'electron.ownedGuest.v1', variant: contract.production.variant, platform: 'darwin', architecture,
    source, build, bundle_path: bundlePath, bundle_inventory_sha256: bundle.sha256, readiness: 'compiled-unaccepted', native_acceptance: null });
}

function runNativeSpecs(request) {
  const { spawnSync } = require('node:child_process');
  const record = JSON.parse(fileBytes(request.candidatePath));
  const { contract, build } = validateSource(record, record.architecture);
  requireValue(record.readiness === 'compiled-unaccepted', 'Native specs require an explicit compiled candidate');
  requireValue(inventory(record.bundle_path).sha256 === record.bundle_inventory_sha256 && build.bundle_inventory_sha256 === record.bundle_inventory_sha256, 'Candidate engine changed before native tests');
  const root = canonical(build.source_root), release = verifySourceCheckout(root);
  requireValue(JSON.stringify(release) === JSON.stringify(receiptFile(build.release_patch_series)), 'Native spec source composition differs from the compiled engine');
  const output = canonical(request.outputRoot); requireValue(fs.readdirSync(output).length === 0, 'Native spec output requires a new empty directory');
  for (const name of ['ELECTRON_RUN_AS_NODE', 'NODE_OPTIONS']) requireValue(!process.env[name], `Native specs cannot run with ${name}`);
  const runner = path.join(__dirname, 'native-spec-runner.cjs'), executable = path.join(record.bundle_path, 'Contents/MacOS/Electron');
  const resultPath = path.join(output, 'native-spec.json'), spec = contract.navigation.files.find(value => value.path === 'electron/spec/api-owned-guest-spec.ts');
  const log = fs.openSync(path.join(output, 'native-spec.log'), 'wx'); let result;
  try { result = spawnSync(executable, [runner, root, resultPath, spec.after_sha256, record.bundle_inventory_sha256], { cwd: root,
    env: process.env, stdio: ['ignore', log, log], timeout: 300000 }); }
  finally { fs.closeSync(log); }
  if (result.error) throw result.error;
  requireValue(result.status === 0, 'Real owned guest native specs did not pass; inspect the separate native output');
  requireValue(inventory(record.bundle_path).sha256 === record.bundle_inventory_sha256 && JSON.stringify(verifySourceCheckout(root)) === JSON.stringify(release), 'Native tests changed engine or source identity');
  const receipt = { schema_version: 1, exit_code: result.status, engine_inventory_sha256: record.bundle_inventory_sha256,
    executable, runner_sha256: sha(fileBytes(runner)), spec_source_sha256: spec.after_sha256, runtime_feature_switches: [],
    result: { path: resultPath, sha256: sha(fileBytes(resultPath)) } };
  const destination = path.join(output, 'native-spec-run.json'); fs.writeFileSync(destination, JSON.stringify(receipt, null, 2) + '\n', { flag: 'wx' });
  return { path: destination, sha256: sha(fileBytes(destination)) };
}
function acceptEngineArtifact(request) {
  const record = JSON.parse(fileBytes(request.candidatePath));
  const { contract } = validateSource(record, record.architecture);
  requireValue(record.readiness === 'compiled-unaccepted', 'Acceptance requires the exact compiled candidate');
  const specPath = canonical(request.nativeSpecReceiptPath), physicalPath = canonical(request.physicalReceiptPath);
  const output = canonical(request.outputRoot); requireValue(fs.readdirSync(output).length === 0, 'Native acceptance requires a new empty evidence directory');
  const spec = contract.navigation.files.find(value => value.path === 'electron/spec/api-owned-guest-spec.ts');
  const acceptance = { schema_version: 1, engine_inventory_sha256: record.bundle_inventory_sha256, production_patch_sha256: contract.production.patch.sha256, download_history_patch_sha256: contract.history.patch.sha256, navigation_history_patch_sha256: contract.navigation.patch.sha256,
    spec_source_sha256: spec.after_sha256, runtime_feature_switches: [], testing_passkey_used: false,
    native_spec_run: { path: specPath, sha256: sha(fileBytes(specPath)) }, physical_input: { path: physicalPath, sha256: sha(fileBytes(physicalPath)) } };
  const acceptancePath = path.join(output, 'native-acceptance.json'); fs.writeFileSync(acceptancePath, JSON.stringify(acceptance, null, 2) + '\n', { flag: 'wx' });
  const accepted = { ...record, readiness: 'native-accepted', native_acceptance: { path: acceptancePath, sha256: sha(fileBytes(acceptancePath)) } };
  const candidate = path.join(output, '.native-acceptance-candidate.json'); fs.writeFileSync(candidate, JSON.stringify(accepted, null, 2) + '\n', { flag: 'wx' });
  verifyEngineArtifact(candidate, record.architecture);
  const destination = path.join(output, 'owned-engine.json'); fs.renameSync(candidate, destination);
  return { path: destination, sha256: sha(fileBytes(destination)) };
}

module.exports = { beginPackagedEngine, sealPackagedEngine, verifyPackagedEngineAfterSigning, verifyPackagedEngine, requirePackagedEngine, verifyEngineArtifact, inventory, sourceContract, validateSource, verifySourceCheckout, buildOwnedEngine, runNativeSpecs, acceptEngineArtifact, SPEC_TITLES };


if (require.main === module) {
  try {
    const [mode, input, ...extra] = process.argv.slice(2);
    const methods = { 'verify-source': value => verifySourceCheckout(value.sourceRoot), build: buildOwnedEngine,
      'native-spec': runNativeSpecs, accept: acceptEngineArtifact, verify: value => verifyEngineArtifact(value.receiptPath, value.architecture) };
    requireValue(!extra.length && Object.hasOwn(methods, mode), 'Usage: node engine-artifact.cjs {verify-source|build|native-spec|accept|verify} /absolute/request.json');
    process.stdout.write(JSON.stringify(methods[mode](JSON.parse(fileBytes(input))), null, 2) + '\n');
  } catch (error) { process.stderr.write(String(error.message) + '\n'); process.exitCode = 1; }
}
