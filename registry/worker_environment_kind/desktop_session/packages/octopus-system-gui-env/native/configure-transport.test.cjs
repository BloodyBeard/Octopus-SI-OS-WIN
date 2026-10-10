'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { planDesktopTransport } = require('./configure-transport.cjs');
const registry = path.resolve(__dirname, '../../../../..');
const read = member => JSON.parse(fs.readFileSync(path.join(registry, member), 'utf8'));

test('explicit remote and native declarations round-trip without credential files or unrelated mutations', () => {
  const native = read('interface/system_gui/desktop.json');
  const life = read('interface/system_gui/life.json');
  const descriptor = read('interface/system_gui/interface.json');
  const operations = ['status', 'observe', 'screenshot', 'click', 'move', 'type', 'key', 'scroll', 'drag', 'control', 'cancel'];
  const tools = operations.map(operation => read(`tool/gui.${operation}/tool.json`));
  const before = JSON.stringify({ native, life, descriptor, tools });
  const remote = { schema_version: 1, host: null, action_url: 'https://desktop.example/v1/action', viewer_url: 'https://desktop.example/view', auth_env_key: 'EXPLICIT_REMOTE_KEY' };
  const options = { interfaceId: 'system_gui', descriptor, currentConfig: native, config: remote, life, tools };
  const planned = planDesktopTransport(options);
  assert.throws(() => planDesktopTransport({ ...options, config: { ...remote, app_approval: native.app_approval } }), /explicit native host/);
  assert.throws(() => planDesktopTransport({ ...options, tools: [...tools, read('tool/gui.app_state/tool.json')] }), /exact desktop door/);
  assert.equal(planned.life.processes.length, life.processes.length - 1);
  for (let index = 0; index < tools.length; index++) {
    const next = planned.tools[index];
    assert.deepEqual(next.process.private_read_subpaths, []);
    assert.deepEqual(next.process.inherit_env, ['EXPLICIT_REMOTE_KEY']);
    const restored = structuredClone(next);
    delete restored.process.inherit_env;
    restored.process.private_read_subpaths = tools[index].process.private_read_subpaths;
    assert.deepEqual(restored, tools[index]);
  }
  assert.deepEqual(planned.life.processes[0].inherit_env, ['EXPLICIT_REMOTE_KEY']);
  const nativeHostProcess = life.processes.find(process => process.bin.endsWith('/octopus-desktop-host'));
  const restored = planDesktopTransport({ ...options, currentConfig: remote, config: native, life: planned.life, tools: planned.tools, nativeHostProcess });
  assert.deepEqual(restored.life, life);
  assert.deepEqual(restored.desktop, native);
  assert.equal(JSON.stringify({ native, life, descriptor, tools }), before);
  const anonymous = planDesktopTransport({ ...options, config: { ...remote, auth_env_key: null } });
  assert(anonymous.tools.every(tool => tool.process.inherit_env.length === 0 && tool.process.private_read_subpaths.length === 0));
  assert.equal(anonymous.life.processes[0].inherit_env.length, 0);
  assert.throws(() => planDesktopTransport({ ...options, currentConfig: remote }), /life does not match/);
  assert.throws(() => planDesktopTransport({ ...options, config: { ...remote, headers: { Authorization: 'forbidden-literal' } } }), /authentication values/);
  assert.throws(() => planDesktopTransport({ ...options, config: { ...remote, action_url: 'http://desktop.example/action' } }), /TLS/);
  assert.throws(() => planDesktopTransport({ ...options, config: { ...remote, max_response_bytes: 1023 } }), /bounds/);
  const mixed = structuredClone(options);
  mixed.tools[0].process.private_read_subpaths = [];
  assert.throws(() => planDesktopTransport(mixed), /private inputs disagree/);
  const missingHost = { ...options, currentConfig: remote, config: native, life: planned.life, tools: planned.tools };
  assert.throws(() => planDesktopTransport(missingHost), /explicit native host/);
  for (const [mutate, reason] of [
    [config => { delete config.host.application_behavior; }, /behavior must be an object/],
    [config => { config.host.application_behavior.defaults.full_ui.attribute = 'unknown'; }, /accessibility options/],
    [config => { config.host.application_behavior.defaults.typing_interval_us = 1000001; }, /typing interval/],
    [config => { config.host.application_behavior.defaults.focus_dom_field_by_click = 'true'; }, /must be boolean/],
    [config => { config.host.application_behavior.instruction_resource.content_sha256 = 'unverified'; }, /SHA-256/],
    [config => { config.host.application_behavior.profiles.push(config.host.application_behavior.profiles[0]); }, /selector/],
    [config => { config.host.application_policy.enabled = 'true'; }, /must be boolean/],
    [config => { config.host.application_policy.required_access.default_access = 'anything'; }, /access decisions/],
    [config => { config.host.application_policy.forbidden_bundle_ids.push(config.host.application_policy.forbidden_bundle_ids[0]); }, /unique values/],
    [config => { config.host.application_preferences.root_slot = '../outside'; }, /root slot/],
    [config => { config.host.application_preferences.extra = true; }, /unsupported fields/],
    [config => { config.host.native_sandbox_mode = 'automatic'; }, /sandbox mode/],
  ]) {
    const invalid = structuredClone(native);
    mutate(invalid);
    assert.throws(() => planDesktopTransport({ ...options, config: invalid }), reason);
  }
  for (const mode of ['enforced', 'disabled']) {
    const config = { ...native, host: { ...native.host, native_sandbox_mode: mode } };
    assert.deepEqual(planDesktopTransport({ ...options, config }).desktop, config);
  }
});
