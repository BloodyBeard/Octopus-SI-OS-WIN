const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const { test } = require('node:test');

const source = fs.readFileSync(path.join(__dirname, '../electron/projection.cjs'), 'utf8');
const tree = ts.createSourceFile('projection.cjs', source, ts.ScriptTarget.Latest, true);
const declarations = new Map();
function visit(node) {
  if (ts.isFunctionDeclaration(node) && node.name) declarations.set(node.name.text, node.getText(tree));
  ts.forEachChild(node, visit);
}
visit(tree);
const protocol = { inputLabel: 'input', terminalOutputLabel: 'output', errorOutputLabel: 'error', hiddenActivityLabels: [] };
function row(seq, kind, text, item = 'a', phase = null, from = 'provider') {
  return { seq, source_line: seq, from, to: 'chat', label: kind === 'message' ? 'output' : kind,
    payload: { kind, text, turn_id: 'turn', item_id: item, phase, lifecycle: ['message', 'commentary'].includes(kind) ? 'completed' : 'streaming' } };
}
async function project(rows, writable) {
  const context = vm.createContext({
    stringValue: value => typeof value === 'string' ? value : '',
    objectOrEmpty: value => value && typeof value === 'object' && !Array.isArray(value) ? value : {},
    arrayValue: value => Array.isArray(value) ? value : [], numberValue: (value, fallback) => Number.isFinite(value) ? value : fallback,
    baseInterfaceRows: () => [{ id: 'provider', role: 'provider' }, { id: 'other-provider', role: 'provider' }],
    movementBoundaryInterface: (value, side) => value[side], ledgerArtifactId: value => `artifact-${value.seq}`,
    ledgerArtifactLabel: value => value.label, contentHashHex: () => '', rowTimeMs: value => value.seq * 100,
    compareMovementOrder: (a, b) => a.seq - b.seq,
    resolveLedgerArtifactPayloads: async values => new Map(values.map(value => [value, value.payload])),
    operatorChatExpandedStreamRows: values => values, operatorChatRuntimeProviderId: () => 'provider',
    operatorChatEventData: value => ({ ...value }), operatorChatEventKind: (_label, _protocol, value) => value.kind,
    operatorChatEventText: value => value.text, operatorChatTurnText: value => value.text || '', operatorChatAttachments: () => [],
    operatorChatProjectedRowId: value => `row-${value.seq}`, operatorChatProviderFailureActivity: () => null,
    operatorChatActivityPayload: value => ({ eventKind: value.kind, text: value.text, payload: value, error: false }),
    operatorChatRequestKey: () => '', operatorChatIncrementalEventKey: () => '', operatorTurnControlOutcome: () => null,
    operatorTurnControlEndsTurn: () => false,
  });
  for (const name of ['operatorChatAssistantItemCompleted', 'appendOperatorChatAssistantMessage', 'buildSingleOperatorChatMessages', 'operatorReadOnlyRunMessages']) {
    assert(declarations.has(name)); vm.runInContext(declarations.get(name), context);
  }
  const entry = { runId: 'run', chatInterfaceId: 'chat', writable, inbound: [], outbound: rows, activity: rows };
  const result = await context.buildSingleOperatorChatMessages({}, entry, protocol);
  return JSON.parse(JSON.stringify(result));
}

test('both chat history paths preserve interleaved item identity and replace completed snapshots', async () => {
  for (const writable of [true, false]) for (const finalText of ['', 'Done!']) {
    const messages = await project([
      row(1, 'message_delta', 'Pro'), row(2, 'dynamic_tool', 'Read', 'tool'),
      row(3, 'message_delta', 'First', 'b'), row(4, 'message_delta', 'gress'),
      row(5, 'commentary', 'Progress', 'a', 'commentary'), row(6, 'dynamic_tool', 'Read complete', 'tool'),
      row(7, 'message_delta', ' second', 'b'), row(8, 'commentary', 'First second', 'b', 'commentary'),
      row(9, 'message_delta', ' late'), row(10, 'message_delta', 'Done', 'final', 'final_answer'),
      row(11, 'message', finalText, 'final', 'final_answer'),
    ], writable);
    assert.deepEqual(messages.map(message => message.text), ['Progress', 'Read', 'First second', 'Read complete', finalText || 'Done']);
    assert.deepEqual(messages.filter(message => message.event_kind === 'commentary').map(message => message.event_data.item_id), ['a', 'b']);
    assert.equal(messages[0].created_at_ms, 100); assert.equal(messages[0].movement_seq, 5);
    assert.equal(messages.at(-1).event_data.item_id, 'final'); assert.equal(messages.at(-1).terminal, true);
  }
});

test('provider identity and missing item identity never merge unrelated text', async () => {
  for (const writable of [true, false]) {
    const messages = await project([
      row(1, 'message_delta', 'Same'), row(2, 'message_delta', 'Same', 'a', null, 'other-provider'),
      row(3, 'message_delta', 'Unknown', ''), row(4, 'message_delta', 'Unknown', ''),
    ], writable);
    assert.deepEqual(messages.map(message => message.text), ['Same', 'Same', 'Unknown', 'Unknown']);
  }
});

test('completed partial answers retain their text without completing the turn', async () => {
  const classify = vm.runInNewContext(`(${declarations.get('operatorChatEventKind')})`, {
    stringValue: value => typeof value === 'string' ? value : '',
  });
  assert.equal(classify('turn.agent_message', protocol, { phase: 'partial_answer' }), 'message');
  for (const writable of [true, false]) {
    const partial = row(2, 'message', 'Answer so far', 'partial', 'partial_answer');
    partial.label = 'turn.agent_message';
    const messages = await project([
      row(1, 'message_delta', 'Answer', 'partial', 'partial_answer'), partial,
      row(3, 'dynamic_tool', 'Continue researching', 'tool'),
      row(4, 'message', 'Complete answer', 'final', 'final_answer'),
    ], writable);
    assert.deepEqual(messages.map(message => message.text), ['Answer so far', 'Continue researching', 'Complete answer']);
    assert.equal(messages[0].terminal, false);
    assert.equal(messages[0].event_data.phase, 'partial_answer');
    assert.equal(messages.at(-1).terminal, true);
  }
});
