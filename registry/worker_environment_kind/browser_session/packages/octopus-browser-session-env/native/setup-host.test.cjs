'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { planBrowserNativeHost } = require('./setup-host.cjs');

test('native setup preserves host slots, plans exact preimage and removes only empty owned creations', async () => {
  const home = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), 'browser-host-setup-')));
  try {
    const host = { host_manifest_version: 1, root_refs: { unrelated: home } };
    const hostPath = path.join(home, 'host.json');
    await fs.writeFile(hostPath, JSON.stringify(host), { mode: 0o600 });
    const plannedWrites = [{ destination: path.join(home, 'registry/interface/door/interface.json'), content: JSON.stringify({ interface_driver_config: { private_config: { browser_room: { transport: { wire: 'browser.native.v1', host_root_slot: 'browser_native_host_root' } } } } }) }];
    const plan = await planBrowserNativeHost({ octopusHomeRoot: home, plannedWrites });
    assert.equal(plan.writes.length, 1);
    assert.equal(plan.writes[0].destination, hostPath);
    assert.equal(plan.writes[0].publishBarrier, false);
    assert.match(plan.writes[0].expectedBeforeDigest, /^[a-f0-9]{64}$/);
    assert.deepEqual(JSON.parse(await fs.readFile(hostPath, 'utf8')), host);
    const root = JSON.parse(plan.writes[0].content).root_refs.browser_native_host_root;
    assert.equal((await fs.stat(root)).mode & 0o777, 0o700);
    await fs.writeFile(path.join(root, 'owned-by-host'), 'keep');
    await plan.rollback();
    assert.equal(await fs.readFile(path.join(root, 'owned-by-host'), 'utf8'), 'keep');
    await fs.unlink(path.join(root, 'owned-by-host'));
    await plan.rollback();
    await assert.rejects(fs.stat(root), { code: 'ENOENT' });
    const custom = path.join(home, 'custom');
    await fs.mkdir(custom, { mode: 0o700 });
    host.root_refs.browser_native_host_root = custom;
    await fs.writeFile(hostPath, JSON.stringify(host));
    const preserved = await planBrowserNativeHost({ octopusHomeRoot: home, plannedWrites });
    assert.deepEqual(JSON.parse(preserved.writes[0].content), host);
    await preserved.rollback();
    assert.ok((await fs.stat(custom)).isDirectory());
    await fs.symlink(custom, path.join(home, 'alias'));
    host.root_refs.browser_native_host_root = path.join(home, 'alias');
    await fs.writeFile(hostPath, JSON.stringify(host));
    await assert.rejects(planBrowserNativeHost({ octopusHomeRoot: home, plannedWrites }), /canonical directory/);
    assert.deepEqual((await planBrowserNativeHost({ octopusHomeRoot: home, plannedWrites: [] })).writes, []);
  } finally { await fs.rm(home, { recursive: true, force: true }); }
});
