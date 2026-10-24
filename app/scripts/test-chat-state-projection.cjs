const assert = require('node:assert/strict');
const path = require('node:path');
const { test } = require('node:test');
const { build } = require('esbuild');
const { parseOctopusOsAuthoringProfile } = require('../electron/projection/authoringProfiles.cjs');

const helpers = build({ stdin: { contents: 'export {latestContextUsage} from "./contextUsage"; export {withRunOperationTargets} from "./runOperationTargets";',
  resolveDir: path.resolve(__dirname, '../src/components/chat'), loader: 'ts' }, bundle: true, write: false, format: 'cjs', platform: 'node' })
  .then(result => { const module = { exports: {} }; new Function('module', 'exports', result.outputFiles[0].text)(module, module.exports); return module.exports; });

test('client retains native publication selection provenance when projecting a created task', () => {
  const reference = id => ({ id, content: Array(32).fill(1) });
  const evidence = { pin: { source: reference('library'), entry_id: 'widget', revision: 'a'.repeat(64) },
    entry: { id: 'widget', schema_version: 1 }, snapshot: reference('snapshot'), context_sources: [reference('guide')] };
  const profile = { ee_id: 'project', worker_choice_interface: 'worker', door_host_slots: {}, publication_evidence: [evidence] };
  assert.deepEqual(parseOctopusOsAuthoringProfile(profile, 'worker', 'task').publication_evidence, [evidence]);
  assert.throws(() => parseOctopusOsAuthoringProfile({ ...profile, publication_evidence: [{ ...evidence, pin: { ...evidence.pin, entry_id: 'another' } }] }, 'worker', 'task'), /Invalid publication/);
});

test('recipient names and links resolve exact declared interfaces, including waits', async () => {
  const { withRunOperationTargets } = await helpers;
  const runs = [{ runId: 'child', title: 'Counter widget', inputInterfaceIds: ['peer'] }, { runId: 'unrelated', title: 'Other', inputInterfaceIds: ['other'] }];
  const event = (tool, argumentsValue = {}, body = {}) => ({ eventData: { namespace: 'formed_interfaces', tool, arguments: argumentsValue, body } });
  const messages = [event('list_runs', {}, { declared_peer_interfaces: ['peer'] }), event('followup_task', { interface: 'peer' }), event('wait_for_runs'), event('followup_task', { interface: 'unknown' })];
  const projected = withRunOperationTargets(messages, runs);
  for (const row of projected.slice(0, 3)) assert.deepEqual(row.relatedRuns, [{ runId: 'child', title: 'Counter widget' }]);
  assert.equal(projected[3], messages[3]);
});

test('context circle uses reported current usage, never cumulative usage or text size', async () => {
  const { latestContextUsage } = await helpers;
  const usage = { eventData: { context_window_usage: { used_tokens: 42000, context_window: 200000 }, provider_token_usage: { cumulative: { total_tokens: 3200000 } } } };
  assert.deepEqual(latestContextUsage([usage, { text: 'later reply' }]), { usedTokens: 42000, contextWindow: 200000, percent: 21 });
  assert.equal(latestContextUsage([{ text: 'long answer'.repeat(500) }]), null);
  assert.equal(latestContextUsage([{ eventData: { context_window_usage: { used_tokens: 5, context_window: 0 } } }]), null);
});
