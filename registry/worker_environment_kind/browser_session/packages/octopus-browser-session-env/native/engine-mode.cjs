'use strict';

const fs = require('node:fs');
const path = require('node:path');
const modes = require('./engine-modes.json');
const artifact = require('./macos/engine-artifact.cjs');
const RECEIPT = 'browser-engine.json';
const ENGINE_ROOT = 'Contents/Frameworks';

function check(value, message) { if (!value) throw new Error(message); }
function engineInfo(mode) {
  check(['standard', 'owned'].includes(mode) && Object.hasOwn(modes, mode), `Unknown browser engine mode: ${mode}`);
  const info = structuredClone(modes[mode]);
  Object.freeze(info.capabilities); Object.freeze(info.limits);
  return Object.freeze(info);
}
function packagingMode(args, env) {
  check(args.length <= 1 && (!args.length || /^--browser-engine=(standard|owned)$/.test(args[0])), 'Usage: package-macos.cjs [--browser-engine=standard|owned]');
  const flag = args.length ? args[0].split('=')[1] : null;
  const variable = env.OCTOPUS_BROWSER_ENGINE;
  if (variable !== undefined) engineInfo(variable);
  check(!flag || variable === undefined || variable === flag, 'Browser engine flag and OCTOPUS_BROWSER_ENGINE disagree');
  const mode = flag ?? variable ?? 'standard';
  const receipt = env.OCTOPUS_OWNED_ELECTRON_RECEIPT;
  check(mode !== 'owned' || typeof receipt === 'string' && receipt.length > 0, 'Owned browser mode requires OCTOPUS_OWNED_ELECTRON_RECEIPT');
  check(mode !== 'standard' || receipt === undefined, 'Standard browser mode cannot consume OCTOPUS_OWNED_ELECTRON_RECEIPT; select --browser-engine=owned or unset the receipt');
  return mode;
}
function sealBrowserEngine({ mode, bundleRoot, architecture, electronVersion }) {
  const info = engineInfo(mode);
  check(['arm64', 'x64'].includes(architecture) && /^\d+\.\d+\.\d+$/.test(electronVersion), 'Invalid packaged browser engine identity');
  const resources = path.join(bundleRoot, 'Contents/Resources');
  check(fs.existsSync(path.join(resources, 'owned-browser-engine.json')) === (mode === 'owned'), 'Browser mode disagrees with its owned-engine receipt');
  const retained = mode === 'standard' ? artifact.inventory(bundleRoot, [ENGINE_ROOT]) : null;
  const receipt = { schema_version: 1, mode, architecture, electron_version: electronVersion,
    presentation_protocol: info.presentationProtocol,
    retained_engine: retained ? { root: ENGINE_ROOT, sha256: retained.sha256, member_count: retained.members.length } : null };
  fs.writeFileSync(path.join(resources, RECEIPT), JSON.stringify(receipt, null, 2) + '\n', { flag: 'wx', mode: 0o644 });
  return receipt;
}
function verifyBrowserEngine(options) {
  check(options.isPackaged === true, 'Browser engine selection requires its explicit packaged receipt');
  const { resourcesRoot, executablePath, architecture, electronVersion } = options;
  const bundleRoot = path.dirname(path.dirname(resourcesRoot));
  check(path.isAbsolute(resourcesRoot) && fs.realpathSync(resourcesRoot) === resourcesRoot && fs.realpathSync(executablePath) === executablePath
    && resourcesRoot === path.join(bundleRoot, 'Contents/Resources') && executablePath === path.join(bundleRoot, 'Contents/MacOS/Octopus'), 'Browser engine receipt does not belong to this packaged client');
  const file = path.join(resourcesRoot, RECEIPT), stat = fs.lstatSync(file);
  check(stat.isFile() && !stat.isSymbolicLink() && stat.size <= 16384, 'Browser engine receipt must be a bounded regular file');
  const receipt = JSON.parse(fs.readFileSync(file, 'utf8'));
  const fields = ['schema_version', 'mode', 'architecture', 'electron_version', 'presentation_protocol', 'retained_engine'];
  check(receipt && Object.keys(receipt).length === fields.length && fields.every(key => Object.hasOwn(receipt, key)) && receipt.schema_version === 1, 'Browser engine receipt fields differ');
  const info = engineInfo(receipt.mode);
  check(receipt.architecture === architecture && ['arm64', 'x64'].includes(architecture) && receipt.electron_version === electronVersion
    && receipt.presentation_protocol === info.presentationProtocol, 'Browser engine runtime identity differs from its packaged mode');
  if (receipt.mode === 'owned') {
    check(receipt.retained_engine === null, 'Owned mode must use its native acceptance receipt');
    return { mode: 'owned', info, engineProof: artifact.verifyPackagedEngine(options) };
  }
  check(!fs.existsSync(path.join(resourcesRoot, 'owned-browser-engine.json')), 'Standard mode contains a conflicting owned-engine receipt');
  const retained = receipt.retained_engine;
  check(retained && Object.keys(retained).length === 3 && retained.root === ENGINE_ROOT && /^[a-f0-9]{64}$/.test(retained.sha256)
    && Number.isSafeInteger(retained.member_count) && retained.member_count > 0, 'Standard browser engine inventory is invalid');
  const current = artifact.inventory(bundleRoot, [ENGINE_ROOT]);
  check(current.sha256 === retained.sha256 && current.members.length === retained.member_count
    && current.members.some(member => member.type === 'file' && member.path === 'Contents/Frameworks/Electron Framework.framework/Versions/A/Electron Framework'), 'Standard browser engine differs from the packaged inventory');
  return { mode: 'standard', info, engineProof: null };
}

module.exports = { engineInfo, packagingMode, sealBrowserEngine, verifyBrowserEngine };
