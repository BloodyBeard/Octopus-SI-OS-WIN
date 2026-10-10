'use strict';

const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

const SLOT = 'browser_native_host_root';
const MAX_HOST_BYTES = 1024 * 1024;
const digest = bytes => crypto.createHash('sha256').update(bytes).digest('hex');

function needsNativeBrowserHost(plannedWrites) {
  return plannedWrites.some(write => {
    if (path.basename(write.destination) !== 'interface.json' || typeof write.content !== 'string') return false;
    const descriptor = JSON.parse(write.content);
    const transport = descriptor.interface_driver_config?.private_config?.browser_room?.transport;
    if (transport?.wire !== 'browser.native.v1') return false;
    if (transport.host_root_slot !== SLOT) throw new Error('Browser native setup has an unknown host slot');
    return true;
  });
}

async function plainDirectory(directory, privateMode) {
  const stat = await fs.lstat(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== process.getuid()
      || (privateMode && (stat.mode & 0o777) !== 0o700)
      || await fs.realpath(directory) !== directory) {
    throw new Error('Browser host setup requires an owned canonical directory');
  }
  return stat;
}

async function planBrowserNativeHost({ octopusHomeRoot, plannedWrites }) {
  const created = [];
  async function rollback() {
    for (const entry of [...created].reverse()) {
      try {
        const stat = await fs.lstat(entry.path);
        if (!stat.isDirectory() || stat.isSymbolicLink() || stat.dev !== entry.dev || stat.ino !== entry.ino) continue;
        await fs.rmdir(entry.path);
      } catch (error) {
        if (!['ENOENT', 'ENOTEMPTY', 'EEXIST'].includes(error.code)) throw error;
      }
    }
  }
  if (!needsNativeBrowserHost(plannedWrites)) return { writes: [], rollback };
  try {
    if (!path.isAbsolute(octopusHomeRoot)) throw new Error('Browser host setup requires an absolute Home');
    await plainDirectory(octopusHomeRoot, false);
    const hostPath = path.join(octopusHomeRoot, 'host.json');
    const handle = await fs.open(hostPath, constants.O_RDONLY | constants.O_NOFOLLOW);
    let before;
    try {
      const stat = await handle.stat();
      if (!stat.isFile() || stat.nlink !== 1 || stat.uid !== process.getuid() || stat.size > MAX_HOST_BYTES) {
        throw new Error('Browser host setup requires an owned bounded host manifest');
      }
      before = await handle.readFile();
      if (before.length > MAX_HOST_BYTES) throw new Error('Browser host manifest grew beyond its byte limit');
    } finally { await handle.close(); }
    const host = JSON.parse(before.toString('utf8'));
    if (!host || host.host_manifest_version !== 1 || !host.root_refs || Array.isArray(host.root_refs)
        || typeof host.root_refs !== 'object' || Object.keys(host).some(key => !['host_manifest_version', 'root_refs'].includes(key))) {
      throw new Error('Browser host setup requires the canonical host manifest schema');
    }
    for (const value of Object.values(host.root_refs)) {
      if (typeof value !== 'string' || !path.isAbsolute(value)) throw new Error('Host manifest root must be an absolute path');
    }
    const root = Object.hasOwn(host.root_refs, SLOT)
      ? host.root_refs[SLOT]
      : path.join(octopusHomeRoot, 'client', 'browser-native-host');
    if (path.normalize(root) !== root) throw new Error('Browser native host root must be canonical');
    const missing = [];
    let cursor = root;
    while (true) {
      try { await plainDirectory(cursor, cursor === root); break; }
      catch (error) {
        if (error.code !== 'ENOENT') throw error;
        missing.push(cursor);
        const parent = path.dirname(cursor);
        if (parent === cursor) throw new Error('Browser native host root has no existing parent');
        cursor = parent;
      }
    }
    for (const directory of missing.reverse()) {
      await fs.mkdir(directory, { mode: 0o700 });
      const stat = await plainDirectory(directory, true);
      created.push({ path: directory, dev: stat.dev, ino: stat.ino });
    }
    await plainDirectory(root, true);
    host.root_refs[SLOT] = root;
    const content = `${JSON.stringify(host, null, 2)}\n`;
    return {
      writes: [{ destination: hostPath, expectedBeforeDigest: digest(before), afterDigest: digest(Buffer.from(content)), content, tag: 'browser-native-host-slot', publishBarrier: false }],
      rollback,
    };
  } catch (error) {
    await rollback();
    throw error;
  }
}

module.exports = { planBrowserNativeHost, needsNativeBrowserHost };
