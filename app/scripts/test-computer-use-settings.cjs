'use strict';
const assert = require('node:assert/strict');
const { test } = require('node:test');
const { EventEmitter } = require('node:events');
const { PassThrough, Writable } = require('node:stream');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { createNativeDesktopSettingsHost } = require('../electron/nativeDesktopSettings.cjs');
const { createDesktopPermissionHost } = require('../electron/desktopSurface.cjs');
const { readComputerUsePreference, writeComputerUsePreference } = require('../../registry/worker_environment_kind/desktop_session/packages/octopus-system-gui-env/native/setup-preferences.cjs');

test('upfront status and setup share one verified helper and retain exact observed permission identity', async () => {
  let release, preparations = 0, nextPid = 100, valid = true, home = '/home';
  const barrier = new Promise(resolve => { release = resolve; }), children = [], actions = [], nativeCalls = [];
  const owner = { id: 1, current() { if (!valid) throw new Error('owner closed'); }, windowHandle: () => Buffer.alloc(8) };
  const host = createNativeDesktopSettingsHost({ homePath: () => home, prepareHelper: async () => {
    ++preparations; await barrier;
    return { home: '/home', program: '/installed/native-helper', directory: '/installed', timeout: 1000, current() { if (home !== '/home') throw new Error('Home changed'); }, preference: async () => ({ schema_version: 1, enabled: false }) };
  }, spawn(program, args, options) {
    assert.equal(program, '/installed/native-helper'); assert.deepEqual(args, ['--permission-setup']);
    assert.deepEqual(Object.keys(options.env).sort(), ['LANG','PATH']);
    const child = new EventEmitter(); child.pid = ++nextPid; child.stdout = new PassThrough(); child.stderr = new PassThrough();
    child.kill = () => { child.killed = true; }; children.push(child);
    child.stdin = new Writable({ write(bytes, _encoding, done) {
      const request = JSON.parse(bytes); actions.push(request.action); done();
      const helper = { executable_path: program, display_name: 'Native helper', process_id: child.pid, bundle: null, responsible_app: null };
      const permissions = ['screen_capture','accessibility'].map(kind => ({ id: kind + '-observed', kind, title: kind, status: 'not_granted', can_request: false, can_open_settings: true, detail: null,
        provenance: { process_id: child.pid, api: kind === 'screen_capture' ? 'CGPreflightScreenCaptureAccess' : 'AXIsProcessTrusted' } }));
      queueMicrotask(() => child.stdout.write(JSON.stringify({ id: request.id, ok: true, result: { helper, permissions } }) + '\n'));
    } }); return child;
  } });
  const permissions = createDesktopPermissionHost({ homePath: () => home, prepareSource: (_input, owner) => host.source(owner),
    automationSources() { throw new Error('upfront setup must not manufacture an app grant'); },
    getNativeHost: () => Object.fromEntries(['showPermissions','updatePermissions','updatePermissionSettings','closePermissions'].map(name => [name, value => nativeCalls.push([name,value])])) });
  try {
    const status = host.source(owner).then(source => source.exchange('status', null));
    const setup = permissions.open({ setup: true }, owner);
    assert.equal(preparations, 1); release();
    const [snapshot, opened] = await Promise.all([status, setup]);
    assert.deepEqual(opened, { opened: true }); assert.equal(children.length, 1);
    assert.equal(snapshot.helper.process_id, children[0].pid); assert(actions.every(value => value.operation === 'status'));
    const source = await host.source(owner);
    await assert.rejects(source.exchange('request', 'screen_capture-observed'), /status and explicit/);
    const before = actions.length; await source.exchange('open_settings', 'screen_capture-observed');
    assert.equal(actions.length, before + 1); assert.equal(actions.at(-1).permission, 'screen_capture-observed');
    const presentation = nativeCalls.find(row => row[0] === 'showPermissions')[1];
    assert.equal(presentation.snapshot.helper.process_id, children[0].pid);
    await permissions.onEvent({ type: 'permission-action', presentationId: presentation.presentationId, generation: presentation.generation, operation: 'dismiss' });
    assert(children[0].killed); assert.equal((await host.source(owner)).home, '/home'); assert.equal(children.length, 2);
    home = '/other'; await assert.rejects(host.source(owner), /Home|current/);
  } finally { permissions.stop(); host.stop(); }
});

test('closing an owner during shared preparation starts no helper', async () => {
  let release, spawned = 0;
  const wait = new Promise(resolve => { release = resolve; });
  const host = createNativeDesktopSettingsHost({ homePath: () => '/home', prepareHelper: async () => { await wait; return { home: '/home', current() {}, timeout: 1000 }; }, spawn() { ++spawned; throw new Error('unexpected spawn'); } });
  const owner = { id: 7, current() {} };
  const requests = [host.source(owner), host.source(owner)];
  const results = requests.map(request => assert.rejects(request, /closed/)); host.closeOwner(7); release();
  await Promise.all(results); assert.equal(spawned, 0); host.stop();
});

test('saved enable selection is exact, private and atomic; missing is disabled and malformed refuses', async () => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'octopus-computer-setting-'));
  const root = await fs.realpath(temporary); await fs.chmod(root, 0o700);
  try {
    assert.deepEqual(await readComputerUsePreference(root), { schema_version: 1, enabled: false });
    assert.equal((await writeComputerUsePreference(root, true)).enabled, true);
    assert.equal((await fs.stat(path.join(root,'computer-use.json'))).mode & 0o777, 0o600);
    assert.equal((await writeComputerUsePreference(root, false)).enabled, false);
    await assert.rejects(writeComputerUsePreference(root, 'true'), /Boolean/);
    await fs.writeFile(path.join(root,'computer-use.json'), '{"schema_version":1,"enabled":"true"}');
    await assert.rejects(readComputerUsePreference(root), /malformed/); await assert.rejects(writeComputerUsePreference(root, true), /malformed/);
    await fs.rm(path.join(root,'computer-use.json')); await fs.symlink('/unbound', path.join(root,'computer-use.json'));
    await assert.rejects(readComputerUsePreference(root));
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});

test('cold saved-app Settings binds typed child to current package/root/owner and refuses stale removal before dispatch', async () => {
  let generation='package-a',hold=false,nextPid=200;const calls=[],children=[];
  const owner={id:1,identity:'authenticated-owner',current(){}};
  const host=createNativeDesktopSettingsHost({homePath:()=>'/home',prepareHelper:async()=>({home:'/home',program:'/native-helper',settingsProgram:'/installed/octopus-desktop-host',
    directory:'/installed',preferenceRoot:'/declared-private-root',applicationIdentity:generation,timeout:1000,current(){}}),
    spawn(program,args,options){
      assert.equal(program,'/installed/octopus-desktop-host');assert.deepEqual(args,['--application-settings','/declared-private-root']);
      assert.deepEqual(Object.keys(options.env).sort(),['LANG','PATH']);
      const child=new EventEmitter();child.pid=++nextPid;child.stdout=new PassThrough();child.stderr=new PassThrough();child.kill=()=>{child.killed=true;};children.push(child);
      child.stdin=new Writable({write(bytes,_encoding,done){const action=JSON.parse(bytes);calls.push(action);done();if(hold)return;
        queueMicrotask(()=>{child.stdout.end(JSON.stringify({schema_version:1,applications:action.operation==='list'?[{bundle_id:'example.app',display_name:'Example'}]:[],removed:action.operation==='remove'}));child.emit('close',0);});
      }});return child;
    }});
  try {
    const saved=await host.applicationPermissions({operation:'list'},owner);assert.equal(saved.applications[0].bundle_id,'example.app');
    const result=await host.applicationPermissions({operation:'remove',sourceId:saved.sourceId,bundleId:'example.app'},owner);assert(result.removed);
    assert.deepEqual(calls,[{operation:'list'},{operation:'remove',bundle_id:'example.app'}]);
    await assert.rejects(host.applicationPermissions({operation:'allow',bundleId:'example.app'},owner),/Invalid/);
    generation='package-b';await assert.rejects(host.applicationPermissions({operation:'remove',sourceId:saved.sourceId,bundleId:'example.app'},owner),/source changed/);assert.equal(calls.length,2);
    generation='package-a';await assert.rejects(host.applicationPermissions({operation:'remove',sourceId:saved.sourceId,bundleId:'example.app'},{...owner,identity:'other-session'}),/source changed/);assert.equal(calls.length,2);
    hold=true;const pending=host.applicationPermissions({operation:'list'},owner),closed=assert.rejects(pending,/owner closed/);
    await new Promise(setImmediate);host.closeOwner(owner.id);await closed;assert(children.at(-1).killed);
    assert(calls.every(value=>Object.keys(value).every(key=>['operation','bundle_id'].includes(key))),'child receives no synthetic scope/grant');
  } finally {host.stop();}
});
