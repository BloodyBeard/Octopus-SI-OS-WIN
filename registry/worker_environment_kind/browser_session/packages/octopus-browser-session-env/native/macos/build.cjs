'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawnSync } = require('node:child_process');

function sha(bytes) {
  return crypto.createHash('sha256').update(bytes).digest('hex');
}

function run(cwd, command, args) {
  const result = spawnSync(command, args, {
    cwd, encoding: 'utf8', timeout: 30000, maxBuffer: 4 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} failed (${result.status}): ${result.stderr || result.stdout}`);
  }
  return result.stdout.trim();
}

function regularFile(file) {
  const info = fs.lstatSync(file);
  if (!info.isFile() || info.isSymbolicLink()) throw new Error(`Not a regular source file: ${file}`);
  return fs.readFileSync(file);
}

function main() {
  const [mode, requestedRoot, ...extra] = process.argv.slice(2);
  const modes = new Set(['check-fragments', 'apply-fragments', 'verify-applied', 'check-checkout', 'check-production', 'apply-production', 'verify-production', 'check-download-history', 'apply-download-history', 'verify-download-history', 'check-navigation-history', 'apply-navigation-history', 'verify-navigation-history']);
  if (!modes.has(mode) || !requestedRoot || extra.length || !path.isAbsolute(requestedRoot)) {
    throw new Error('Usage: node build.cjs {check-fragments|apply-fragments|verify-applied|check-checkout|check-production|apply-production|verify-production|check-download-history|apply-download-history|verify-download-history|check-navigation-history|apply-navigation-history|verify-navigation-history} /absolute/source-root');
  }
  const root = fs.realpathSync(requestedRoot);
  const foundation = JSON.parse(regularFile(path.join(__dirname, 'manifest.json')));
  const production = mode.endsWith('-production'), history = mode.endsWith('-download-history'), navigation = mode.endsWith('-navigation-history');
  const productionManifest = JSON.parse(regularFile(path.join(__dirname, 'production.json')));
  const historyBytes = history || navigation ? regularFile(path.join(__dirname, 'download-history.json')) : null;
  const navigationBytes = navigation ? regularFile(path.join(__dirname, 'navigation-history.json')) : null;
  const manifest = navigation ? { ...foundation, ...JSON.parse(navigationBytes) } : history ? { ...foundation, ...JSON.parse(historyBytes) } : production ? { ...foundation, ...productionManifest } : foundation;
  if ((history || navigation) && (productionManifest.download_history?.path !== 'download-history.json' || productionManifest.download_history.sha256 !== sha(historyBytes)
    || manifest.foundation_patch_sha256 !== foundation.patch.sha256 || manifest.production_patch_sha256 !== productionManifest.patch.sha256)) throw new Error('History patch does not match its exact ordered owned engine source');
  if (navigation && (productionManifest.navigation_history?.path !== 'navigation-history.json' || productionManifest.navigation_history.sha256 !== sha(navigationBytes)
    || manifest.download_history_patch_sha256 !== JSON.parse(historyBytes).patch.sha256 || manifest.download_history_manifest_sha256 !== sha(historyBytes))) throw new Error('Navigation history source does not follow its exact download-history base');
  if (production && manifest.foundation_patch_sha256 !== foundation.patch.sha256) throw new Error('Production patch does not match its foundation');
  if (manifest.schema_version !== 1 || manifest.source_only !== true || manifest.runtime_activation !== 'held') {
    throw new Error('This helper validates the held source patch only');
  }
  if (manifest.initial_surface?.required !== true || manifest.initial_surface.default !== null ||
      manifest.initial_surface.units !== 'unscaled-dip' || manifest.initial_surface.maximum !== 2147483647) {
    throw new Error('Owned guest source requires explicit initial physical dimensions');
  }
  const patch = path.join(__dirname, manifest.patch.path);
  if (sha(regularFile(patch)) !== manifest.patch.sha256) throw new Error('Patch digest mismatch');
  if (mode === 'check-checkout') {
    for (const [directory, expected] of [
      [root, manifest.chromium.commit],
      [path.join(root, 'electron'), manifest.electron.commit],
    ]) {
      if (run(directory, 'git', ['rev-parse', 'HEAD']) !== expected) {
        throw new Error(`Wrong pinned source commit: ${directory}`);
      }
    }
  }
  const after = mode === 'verify-applied' || mode === 'verify-production' || mode === 'verify-download-history' || mode === 'verify-navigation-history';
  for (const entry of manifest.files) {
    if ((!entry.path.startsWith('electron/') && !(production && entry.path.startsWith('content/'))) || entry.path.split('/').includes('..')) {
      throw new Error(`Invalid patch member: ${entry.path}`);
    }
    const file = path.join(root, entry.path);
    const expected = after ? entry.after_sha256 : entry.before_sha256;
    if (expected === null) {
      if (fs.existsSync(file)) throw new Error(`New source member already exists: ${entry.path}`);
    } else if (sha(regularFile(file)) !== expected) {
      throw new Error(`Source ${after ? 'postimage' : 'preimage'} differs: ${entry.path}`);
    }
  }
  if (!after) {
    run(root, 'git', ['apply', '--check', '--whitespace=error', patch]);
    if (mode === 'apply-fragments' || mode === 'apply-production' || mode === 'apply-download-history' || mode === 'apply-navigation-history') {
      run(root, 'git', ['apply', '--whitespace=error', patch]);
      for (const entry of manifest.files) {
        if (sha(regularFile(path.join(root, entry.path))) !== entry.after_sha256) {
          throw new Error(`Applied member differs: ${entry.path}`);
        }
      }
    }
  }
  process.stdout.write(`${JSON.stringify({
    mode, sourceRoot: root, patchSha256: manifest.patch.sha256,
    validatedMembers: manifest.files.length,
    patchApplied: mode === 'apply-fragments' || mode === 'apply-production' || mode === 'apply-download-history' || mode === 'apply-navigation-history',
    sourceOnly: true, nativeCompile: 'not performed', runtimeActivation: 'held',
    initialSurfaceContract: manifest.initial_surface,
    laterBuildContract: manifest.build_contract,
  }, null, 2)}\n`);
}

try {
  main();
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
}
