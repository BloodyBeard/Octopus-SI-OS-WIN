'use strict';

const assert = require('node:assert/strict');
const { PassThrough } = require('node:stream');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { Terminal } = require('@xterm/xterm');
const { setTimeout: delay } = require('node:timers/promises');
const { runInteractive, InputDecoder, Draft } = require('../cli/terminal.cjs');
const { safeText, widthOf, composerLayout } = require('../cli/presentation.cjs');

function feed(signal) {
  const items = [];
  let waiter;
  let ended = false;
  const abort = () => { ended = true; waiter?.({ done: true }); waiter = null; };
  signal.addEventListener('abort', abort, { once: true });
  return {
    push(value) { if (ended) return; if (waiter) { const take = waiter; waiter = null; take({ value, done: false }); } else items.push(value); },
    [Symbol.asyncIterator]() { return this; },
    next() { if (ended) return Promise.resolve({ done: true }); if (items.length) return Promise.resolve({ value: items.shift(), done: false }); return new Promise((resolve) => { waiter = resolve; }); },
    return() { abort(); signal.removeEventListener('abort', abort); return Promise.resolve({ done: true }); },
  };
}

function fakeClient({ empty = false, automatic = false } = {}) {
  const calls = [];
  const streams = new Map();
  const receipts = [];
  const agent = { id: 'alpha', name: 'Alpha', projectId: 'project', providerId: 'provider-a', model: 'model-a', effort: 'high', workerId: 'worker-alpha' };
  const beta = { ...agent, id: 'beta', name: 'Beta', providerId: 'provider-b', model: 'model-b', effort: 'medium' };
  const catalog = { projects: [{ id: 'root', name: 'Home', isHome: true, sourceWorkerId: '', agents: [] }, { id: 'project', name: 'Workspace', sourceWorkerId: 'source-project', agents: empty ? [] : [agent, beta] }], providers: [{ id: 'provider-a', label: 'Provider A · old-default-model', sourceNames: ['Provider A'] }, { id: 'provider-b', label: 'Provider B · old-default-model', sourceNames: ['Provider B'] }] };
  const tasks = [{ id: 'home-task', title: 'run_operator_chat_home_fixture', projectId: 'root', providerId: 'provider-a', model: 'model-a', effort: 'high', state: 'idle', archived: false, writable: true, isHome: true }];
  const histories = new Map();
  const record = (method, input) => { calls.push({ method, input }); };
  const client = {
    calls, streams, receipts, catalog, tasksData: tasks, histories,
    async status() { record('status'); return { home: '/fixture/Home', ready: true }; },
    async contexts() { record('contexts'); return structuredClone(catalog); },
    async tasks(options = {}) { record('tasks', options); return tasks.filter((task) => (options.archived === undefined || task.archived === options.archived) && (!options.search || task.title.includes(options.search))).map((task) => ({ ...task })); },
    async task(id) { record('task', id); const task = tasks.find((task) => task.id === id || id === 'home' && task.isHome); if (!task) throw new Error('Task not found'); return { ...task }; },
    async history(id) { record('history', id); return structuredClone(histories.get(id === 'home' ? 'home-task' : id) || []); },
    async models(selection) { record('models', selection); return ['a', 'b'].filter((key) => !selection.runId || tasks.find((task) => task.id === selection.runId)?.providerId === `provider-${key}`).map((key) => ({ providerId: `provider-${key}`, model: `model-${key}`, displayName: `Model ${key.toUpperCase()}`, reasoningEfforts: ['medium', 'high'], defaultReasoningEffort: 'medium' })); },
    async projectPolicies(parentId) { record('projectPolicies', parentId); return [{ id: 'inherited', label: 'Same access as parent', description: 'Use the parent’s permissions', inheritsParentPolicy: true }]; },
    async send(input) {
      record('send', input);
      if (client.rejectNext) { client.rejectNext = false; throw new Error('Provider unavailable; message was not sent.'); }
      const id = input.runId || `task-${receipts.length + 1}`;
      const existing = tasks.find((task) => task.id === id);
      const task = { ...input, id, title: input.text.trim().split('\n')[0] || 'Files', state: 'working', archived: false, writable: true, turnOpen: true };
      if (existing) Object.assign(existing, task); else tasks.push(task);
      const receipt = { runId: id, chatInterfaceId: `chat-${id}`, modelInterfaceId: `model-${id}`, messageId: `message-${receipts.length + 1}`, messageVerdict: { admitted: { seq: receipts.length + 10 } }, cursor: { seq: receipts.length + 10 }, afterSeq: receipts.length + 10, outboundCursor: { seq: receipts.length + 10 } };
      receipts.push(receipt);
      histories.set(id, [{ id: receipt.messageId, role: 'user', kind: 'message', text: input.text }]);
      return receipt;
    },
    follow(id, options) {
      record('follow', { id, options });
      const stream = feed(options.signal);
      streams.set(id, stream);
      stream.push({ type: 'status', state: 'working', terminal: false, correlated: true });
      if (automatic) {
        const message = { id: `reply-${id}`, role: 'assistant', kind: 'message', text: '', status: 'streaming' };
        setTimeout(() => stream.push({ type: 'message', entry: message }), 20);
        setTimeout(() => stream.push({ type: 'delta', id: message.id, role: 'assistant', text: 'A real streamed fixture reply.' }), 80);
        setTimeout(() => {
          stream.push({ type: 'activity', id: 'tool-pty', title: 'Checking files', command: 'inspect fixture', status: 'exited', output: 'Done', entry: { id: 'tool-pty', role: 'tool', kind: 'tool', text: '' } });
          client.complete(id);
        }, 130);
      }
      return stream;
    },
    complete(id, state = 'idle') { const task = tasks.find((task) => task.id === id); if (task) { task.state = state; task.turnOpen = false; } streams.get(id)?.push({ type: 'status', state, terminal: true, correlated: true }); },
    async stop(id) { record('stop', id); return { admitted: true }; },
    async respond(id, request, answer) { record('respond', { id, request, answer }); return { admitted: true }; },
    async rename(id, title) { record('rename', { id, title }); tasks.find((task) => task.id === id).title = title; },
    async archive(id, archived) { record('archive', { id, archived }); tasks.find((task) => task.id === id).archived = archived; },
    async deleteTask(id) { record('deleteTask', id); tasks.splice(tasks.findIndex((task) => task.id === id), 1); },
    async createProject(input) { record('createProject', input); catalog.projects.push({ id: 'created-project', name: input.name, sourceWorkerId: 'created-source', agents: [] }); return { eeId: 'created-project' }; },
    async createAgent(input) { record('createAgent', input); catalog.projects.find((project) => project.id === input.projectId).agents.push({ ...input, id: 'created-agent' }); return { agentId: 'created-agent' }; },
    async search(query, id) { record('search', { query, id }); return { matches: [{ task_run_id: id || 'home-task', snippet: `Found ${query}`, message: { role: 'assistant' } }], has_older: false }; },
  };
  return client;
}

async function until(predicate, label = 'condition') {
  const deadline = Date.now() + 2500;
  while (!predicate()) { if (Date.now() > deadline) throw new Error(`Timed out waiting for ${label}`); await delay(5); }
  await delay(5);
}

async function session(client = fakeClient(), options = {}) {
  const input = new PassThrough();
  input.isTTY = true;
  input.isRaw = false;
  input.rawModes = [];
  input.setRawMode = (value) => { input.isRaw = value; input.rawModes.push(value); };
  const output = new PassThrough();
  output.isTTY = true;
  output.columns = 84;
  output.rows = 24;
  let text = '';
  output.on('data', (chunk) => { text += chunk.toString(); });
  const pending = runInteractive(client, { input, output, processSignals: false, term: 'xterm-256color', returnState: true, ...options });
  await until(() => safeText(text).includes('Home:'), 'startup');
  const ui = {
    client, input, output, pending, get text() { return text; },
    async write(value) { input.write(value); await delay(25); },
    async command(value) { input.write(`\x15${value}\r`); await delay(35); },
    async close() { input.write('\x04'); const result = await pending; assert.equal(input.isRaw, false); assert.deepEqual(input.rawModes, [true, false]); assert.ok(text.includes('\x1b[?2004l')); assert.equal(input.listenerCount('data'), 0); return result; },
  };
  return ui;
}

async function checks() {
  const errors = [];
  const keys = [];
  const decoder = new InputDecoder((key) => keys.push(key), (error) => errors.push(error));
  const bytes = Buffer.from('\x1b[200~/archive\n\t🙂\x03\x1b]52;c;evil\x07\x1b[201~');
  for (let offset = 0; offset < bytes.length; offset += 2) decoder.feed(bytes.subarray(offset, offset + 2));
  decoder.close();
  assert.equal(errors.length, 0);
  assert.equal(keys.length, 1);
  assert.equal(keys[0].key, 'paste');
  assert.ok(keys[0].text.includes('/archive'));
  assert.equal(safeText('\x1b[2J\x1b]52;c;evil\x07hello\u202e\x07'), 'hello');
  const wrappedDraft = new Draft('abcdefghijklmnopqrstuvwxyz');
  assert.equal(wrappedDraft.vertical(-1, 12), true);
  const before = wrappedDraft.cursor;
  wrappedDraft.insert('!');
  assert.ok(before > 0 && before < 26);
  assert.equal(wrappedDraft.text.length, 27);
  for (const width of [1, 7, 19, 47]) {
    const view = composerLayout('🙂日本é\nlong draft\ttext', 8, width, 3);
    assert.ok(view.rows.every((line) => widthOf(line.text) <= width));
    assert.ok(view.cursor.row >= 0 && view.cursor.row < view.rows.length);
    assert.ok(view.cursor.col < width);
  }
  const noTTY = new PassThrough();
  const unopened = fakeClient();
  await assert.rejects(runInteractive(unopened, { input: noTTY, output: noTTY }), { code: 'OCTOPUS_TTY_REQUIRED' });
  assert.equal(unopened.calls.length, 0);

  const home = await session(fakeClient(), { initialPrompt: 'Hello Home' });
  assert.ok(home.client.calls.some((call) => call.method === 'task' && call.input === 'home'));
  assert.ok(!safeText(home.text).includes('Choose Agent'));
  assert.ok(!safeText(home.text).includes('run_operator_chat_home_fixture'));
  assert.ok(!safeText(home.text).includes('old-default-model'));
  assert.ok(safeText(home.text).includes('Home · Provider A / model-a · high'));
  await home.write('\r');
  await until(() => home.client.streams.has('home-task'), 'Home send');
  assert.equal(home.client.calls.find((call) => call.method === 'send').input.text, 'Hello Home');
  assert.equal(home.client.calls.find((call) => call.method === 'send').input.runId, 'home-task');
  await home.command('/new');
  await home.write('Workspace\r');
  await home.write('Beta\r');
  await home.write('New Project task\r');
  await until(() => home.client.receipts.length === 2, 'task after Home');
  assert.equal(home.client.calls.filter((call) => call.method === 'send')[1].input.agentId, 'beta');
  assert.equal(home.client.calls.filter((call) => call.method === 'send')[1].input.projectId, 'project');
  assert.equal(home.client.calls.filter((call) => call.method === 'stop').length, 0);
  await home.close();

  const pick = await session(fakeClient(), { projectId: 'project' });
  await pick.write('Beta\r');
  await pick.write('\x1b[200~First line\n\tsecond🙂\x1b[201~');
  assert.equal(pick.client.calls.filter((call) => call.method === 'send').length, 0);
  await pick.write('\x7f!\r');
  await until(() => pick.client.streams.has('task-1'), 'first follow');
  const sent = pick.client.calls.find((call) => call.method === 'send').input;
  assert.equal(sent.agentId, 'beta');
  assert.equal(sent.providerId, 'provider-b');
  assert.equal(sent.model, 'model-b');
  assert.equal(sent.effort, 'medium');
  assert.equal(sent.text, 'First line\n\tsecond!');
  const follow = pick.client.calls.find((call) => call.method === 'follow').input.options;
  assert.equal(follow.chatInterfaceId, pick.client.receipts[0].chatInterfaceId);
  assert.deepEqual(follow.messageVerdict, pick.client.receipts[0].messageVerdict);
  const events = pick.client.streams.get('task-1');
  events.push({ type: 'status', state: 'idle', correlated: false, terminal: true });
  events.push({ type: 'error', id: 'error-1', text: 'A recoverable error\x1b]0;owned\x07', entry: { id: 'error-1', role: 'system', kind: 'error', text: '', error: true } });
  events.push({ type: 'message', entry: { id: 'reply', role: 'assistant', kind: 'message', text: '', status: 'streaming' } });
  events.push({ type: 'delta', id: 'reply', role: 'assistant', text: 'Streamed answer' });
  events.push({ type: 'activity', id: 'tool', title: 'Reading files', command: 'inspect', status: 'exited', output: 'ok\x1b[2J', entry: { id: 'tool', role: 'tool', kind: 'tool', text: '' } });
  events.push({ type: 'activity', id: 'provider-meta', title: 'Private provider metadata', status: '', entry: { id: 'provider-meta', role: 'assistant', kind: 'generic_activity', text: 'Private provider metadata', activity: { details: { account: 'fixture-private-account' } } } });
  pick.client.histories.set('task-1', [...pick.client.histories.get('task-1'), ...['event', 'usage', 'turn_started', 'turn_completed'].map((kind) => ({ id: kind, role: 'assistant', kind, text: `Hidden ${kind} metadata` }))]);
  await pick.write('next draft');
  await pick.write('\x1b');
  await until(() => pick.client.calls.some((call) => call.method === 'stop'), 'Escape stop');
  assert.equal(follow.signal.aborted, false);
  events.push({ type: 'status', state: 'stopping', terminal: false, correlated: true });
  await pick.write('\r');
  assert.equal(pick.client.receipts.length, 1);
  assert.equal(follow.signal.aborted, false);
  await until(() => safeText(pick.text).includes('Stopping · waiting for current operation'), 'Stopping display');
  pick.client.complete('task-1');
  await until(() => follow.signal.aborted, 'correlated completion');
  assert.ok(safeText(pick.text).includes('Streamed answer'));
  assert.ok(!pick.text.includes('\x1b]0;owned'));
  assert.ok(!pick.text.includes('\x1b[2J'));
  assert.ok(!safeText(pick.text).includes('Private provider metadata'));
  assert.ok(!safeText(pick.text).includes('fixture-private-account'));
  await pick.write('\x1b[A\x1b[B');
  await pick.command('/rename Better title');
  assert.equal(pick.client.tasksData.find((task) => task.id === 'task-1').title, 'Better title');
  await pick.command('/search answer');
  assert.ok(pick.client.calls.some((call) => call.method === 'search' && call.input.id === 'task-1'));
  await pick.write('\x1b'); await delay(45);
  await pick.command('/model');
  assert.equal(pick.client.calls.filter((call) => call.method === 'models').at(-1).input.runId, 'task-1');
  await pick.write('\r'); await pick.write('\r');
  await pick.command('/archive');
  await pick.write('cannot send\r');
  assert.equal(pick.client.receipts.length, 1);
  await pick.command('/restore');
  await pick.command('/history');
  assert.ok(!safeText(pick.text).includes('Hidden event metadata'));
  assert.ok(!safeText(pick.text).includes('Hidden turn_started metadata'));
  assert.ok(!safeText(pick.text).includes('Hidden usage metadata'));
  await pick.command('/delete');
  await pick.write('\r');
  assert.ok(!pick.client.calls.some((call) => call.method === 'deleteTask'));
  await pick.command('/delete');
  await pick.write('\x1b[B\r');
  await until(() => pick.client.calls.some((call) => call.method === 'deleteTask'), 'confirmed task deletion');
  assert.equal(pick.client.calls.find((call) => call.method === 'deleteTask').input, 'task-1');
  await pick.command('/home');
  assert.ok(!safeText(pick.text).includes('run_operator_chat_home_fixture'));
  const result = await pick.close();
  assert.equal(result.runId, 'home-task');
  assert.equal(pick.client.calls.filter((call) => call.method === 'stop').length, 1);

  const slowClient = fakeClient();
  let acknowledge;
  const admission = new Promise((resolve) => { acknowledge = resolve; });
  const sendNormally = slowClient.send;
  slowClient.send = async (input) => { await admission; return sendNormally(input); };
  const slow = await session(slowClient, { agentId: 'alpha', initialPrompt: 'submitted prompt' });
  await slow.write('\rnext draft');
  await slow.write('\x03');
  assert.equal(slowClient.calls.filter((call) => call.method === 'stop').length, 0);
  acknowledge();
  await until(() => slowClient.calls.some((call) => call.method === 'stop'), 'Stop after admission');
  assert.equal(slowClient.calls.find((call) => call.method === 'send').input.text, 'submitted prompt');
  assert.equal((await slow.close()).draft, 'next draft');

  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'octopus-cli-terminal-'));
  try {
    const file = path.join(root, 'a file.txt');
    const folder = path.join(root, 'a folder');
    await fs.writeFile(file, 'fixture');
    await fs.mkdir(folder);
    const attach = await session(fakeClient(), { agentId: 'alpha' });
    await attach.command(`/attach '${file}' '${folder}'`);
    attach.client.rejectNext = true;
    await attach.write('keep this message\r');
    await until(() => safeText(attach.text).includes('Provider unavailable'), 'send error');
    await attach.write('\r');
    await until(() => attach.client.streams.has('task-1'), 'retried follow');
    assert.deepEqual(attach.client.calls.filter((call) => call.method === 'send').at(-1).input.files, [file, folder]);
    assert.equal(attach.client.calls.filter((call) => call.method === 'send').at(-1).input.text, 'keep this message');
    await attach.write('\x03');
    assert.equal(attach.client.calls.filter((call) => call.method === 'stop').length, 1);
    const before = attach.client.calls.length;
    await attach.close();
    assert.equal(attach.client.calls.slice(before).filter((call) => call.method === 'stop').length, 0);
    assert.ok(attach.client.calls.find((call) => call.method === 'follow').input.options.signal.aborted);
  } finally { await fs.rm(root, { recursive: true, force: true }); }

  const create = await session(fakeClient({ empty: true }), { projectId: 'project' });
  await create.write('\r');
  await create.write('Helpful Agent\r');
  await until(() => safeText(create.text).includes('Choose model'), 'creation model picker');
  await create.write('\r'); await create.write('\r');
  await until(() => create.client.calls.some((call) => call.method === 'createAgent'), 'Agent creation');
  const createdAgent = create.client.calls.find((call) => call.method === 'createAgent').input;
  assert.equal(createdAgent.workerId, 'source-project');
  assert.equal(createdAgent.providerId, 'provider-a');
  await create.command('/project create');
  await create.write('Research\r');
  await create.write('\r');
  await until(() => create.client.calls.some((call) => call.method === 'createProject'), 'Project creation');
  assert.deepEqual(create.client.calls.find((call) => call.method === 'createProject').input, { name: 'Research', parentId: 'project', policy: 'inherited' });
  await create.close();

  const answer = await session(fakeClient(), { agentId: 'alpha', initialPrompt: 'request test' });
  await answer.write('\r');
  await until(() => answer.client.streams.has('task-1'));
  const request = { type: 'request', id: 'request-1', kind: 'user_input', payload: { request_id: 'raw-id', questions: [{ id: 'choice', question: 'Which?', options: [{ label: 'Exact value' }] }] }, status: 'pending', entry: { id: 'request-1', role: 'system', kind: 'user_input', text: 'Choose your answer', request: { actionable: true, questions: [{ id: 'choice', question: 'Which?', options: [{ label: 'Exact value', value: 'Exact value' }] }] } } };
  answer.client.streams.get('task-1').push(request);
  await answer.write('kept draft');
  await answer.write('\x14');
  await answer.write('\x1b'); await delay(45);
  await answer.write('\x11');
  await answer.write('\r');
  await until(() => answer.client.calls.some((call) => call.method === 'respond'), 'operator response');
  assert.deepEqual(answer.client.calls.find((call) => call.method === 'respond').input.answer, { kind: 'user_input', answers: { choice: ['Exact value'] } });
  const approvalData = { request_id: 'bound-req', call_id: 'bound-call', kind: 'application', detail: { approval_scope: 'formed_app_target' } };
  answer.client.streams.get('task-1').push({ type: 'request', id: 'bound-request', kind: 'approval', payload: approvalData, status: 'pending',
    entry: { id: 'bound-request', role: 'system', kind: 'approval', text: 'Allow application access?', request: { actionable: true, payload: approvalData } } });
  await answer.write('\x11');
  assert.ok(!safeText(answer.text).includes('Allow for this task'));
  await answer.write('\x1b[B\r');
  await until(() => answer.client.calls.filter((call) => call.method === 'respond').length === 2, 'bound approval');
  assert.deepEqual(answer.client.calls.filter((call) => call.method === 'respond').at(-1).input.answer, { kind: 'approval', decision: 'decline' });
  const responses = () => answer.client.calls.filter((call) => call.method === 'respond');
  const publishRequest = (id, kind, payload) => answer.client.streams.get('task-1').push({ type: 'request', id, kind, payload, status: 'pending',
    entry: { id, role: 'system', kind, text: 'Choose a response', request: { actionable: true, payload } } });
  const permissions = { network: { enabled: true }, fileSystem: { read: ['/fixture/read'], write: ['/fixture/write'] } };
  for (const [index, decision] of [[2, { permissions: {}, scope: 'turn' }], [0, { permissions, scope: 'turn' }], [1, { permissions, scope: 'session' }]]) {
    publishRequest(`permissions-${index}`, 'approval', { request_id: `permission-${index}`, method: 'item/permissions/requestApproval', detail: { permissions } });
    const before = responses().length;
    await answer.write('\x11');
    if (index === 2) {
      await answer.write('\x1b'); await delay(45);
      assert.equal(responses().length, before);
      assert.ok(safeText(answer.text).includes('Response picker closed. No response was sent. Use /answer to reopen.'));
      assert.equal(answer.client.calls.filter((call) => call.method === 'stop').length, 0);
      await answer.write('\x11');
    }
    await answer.write('\x1b[B'.repeat(index) + '\r');
    await until(() => responses().length === before + 1, 'native permission response');
    assert.deepEqual(responses().at(-1).input.answer, { kind: 'approval', decision });
  }
  for (const [index, action] of ['accept', 'decline', 'cancel'].entries()) {
    publishRequest(`mcp-${action}`, 'elicitation', { request_id: `mcp-${action}`, method: 'mcpServer/elicitation/request',
      requested_schema: { type: 'object', required: ['count'], properties: { count: { type: 'integer', minimum: 1, maximum: 9 } } } });
    const before = responses().length;
    await answer.write('\x11');
    if (action === 'accept') {
      await answer.write('\r5\x1b'); await delay(45);
      assert.equal(responses().length, before);
      assert.equal(answer.client.calls.filter((call) => call.method === 'stop').length, 0);
      await answer.write('\x11');
      await answer.write('\r');
      await answer.write('5\r');
    } else await answer.write('\x1b[B'.repeat(index) + '\r');
    await until(() => responses().length === before + 1, 'MCP response');
    assert.deepEqual(responses().at(-1).input.answer, { kind: 'elicitation', answer: { action, ...(action === 'accept' ? { content: { count: 5 } } : {}), _meta: null } });
  }
  answer.client.complete('task-1');
  await delay(30);
  await answer.write('\x15');
  await answer.write('\x1b[200~/archive\x1b[201~');
  await answer.write('\r');
  assert.equal(answer.client.calls.filter((call) => call.method === 'send').at(-1).input.text, '/archive');
  await answer.close();
  assert.equal(answer.client.calls.filter((call) => call.method === 'archive').length, 0);

  const narrow = await session(fakeClient(), { agentId: 'alpha', initialPrompt: '🙂日本\neditable', columns: 20 });
  narrow.output.columns = 20; narrow.output.rows = 7; narrow.output.emit('resize');
  await narrow.write('\x01hello');
  const detached = await narrow.close();
  assert.ok(detached.draft.includes('hello'));
  assert.ok(!narrow.text.includes('\x1b[?1049'));
  console.log('CLI terminal contract checks passed.');
}

async function ptyCheck() {
  const python = String.raw`import os, pty, select, subprocess, sys, time, termios, fcntl, struct, json, base64
master, slave = pty.openpty()
original = termios.tcgetattr(slave)
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 22, 88, 0, 0))
proc = subprocess.Popen([sys.argv[1], sys.argv[2], '--pty-fixture'], stdin=slave, stdout=slave, stderr=slave, close_fds=True, env={**os.environ, 'TERM':'xterm-256color'})
data = b''
phases = []
offset = 0
def wait_for(needle):
 global data
 deadline = time.time() + 6
 while needle not in data:
  if time.time() > deadline: raise AssertionError('PTY missing '+repr(needle)+'; tail='+repr(data[-1200:]))
  if select.select([master], [], [], .1)[0]:
   try: data += os.read(master, 65536)
   except OSError: break
def snapshot(cols, rows):
 global data, offset
 deadline = time.time() + 3
 quiet = time.time() + .2
 while time.time() < quiet and time.time() < deadline:
  if select.select([master], [], [], .01)[0]:
   data += os.read(master, 65536)
   quiet = time.time() + .2
 phases.append({'cols':cols, 'rows':rows, 'bytes':base64.b64encode(data[offset:]).decode('ascii')})
 offset = len(data)
try:
 wait_for(b'Home:')
 os.write(master, b'/help\r')
 wait_for(b'Talk to Octopus')
 os.write(master, b'\x1b[200~PTY first line\nsecond line\x1b[201~')
 time.sleep(.1)
 os.write(master, b'\r')
 wait_for(b'A real streamed fixture reply.')
 wait_for(b'Checking files')
 snapshot(88, 22)
 fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 9, 29, 0, 0))
 os.kill(proc.pid, 28)
 time.sleep(.08)
 os.write(master, b'/tasks\r')
 wait_for(b'Your tasks')
 os.write(master, b'\x1b')
 time.sleep(.08)
 os.write(master, 'draft 日本🙂 wrap me safely across this narrow terminal'.encode('utf8'))
 wait_for('日本🙂'.encode('utf8'))
 snapshot(29, 9)
 os.write(master, b'\x04')
 wait_for(b'PTY_RESULT')
 proc.wait(timeout=5)
 assert proc.returncode == 0, proc.returncode
 assert termios.tcgetattr(slave) == original, 'raw terminal settings were not restored'
 assert b'\x1b[?2004l' in data
 assert b'\x1b[?1049' not in data
 assert b'\r\r\n' not in data, 'terminal output doubled CRLF'
 assert b'"stops":0' in data
 assert b'PTY first line\\nsecond line' in data
 phases.append({'cols':29, 'rows':9, 'bytes':base64.b64encode(data[offset:]).decode('ascii')})
finally:
 if proc.poll() is None:
  proc.kill()
  proc.wait(timeout=2)
 os.close(master)
 os.close(slave)
print(json.dumps(phases))`;
  const result = spawnSync('python3', ['-c', python, process.execPath, __filename], { encoding: 'utf8', timeout: 20000 });
  if (result.status !== 0) throw new Error(`PTY verification failed: ${result.stderr || result.stdout || result.error}`);
  const phases = JSON.parse(result.stdout);
  const terminal = new Terminal({ cols: 88, rows: 22, scrollback: 2000, allowProposedApi: true });
  for (let index = 0; index < phases.length; index += 1) {
    const phase = phases[index];
    terminal.resize(phase.cols, phase.rows);
    await new Promise((resolve) => terminal.write(Buffer.from(phase.bytes, 'base64').toString('utf8'), resolve));
    const buffer = terminal.buffer.active;
    const rendered = Array.from({ length: buffer.length }, (_, line) => buffer.getLine(line).translateToString(true)).join('\n').replace(/\s/g, '');
    assert.ok(rendered.includes('Arealstreamedfixturereply.'), 'reply disappeared during terminal redraw or resize');
    if (index === 1) assert.ok(rendered.includes('draft日本🙂wrapmesafelyacrossthisnarrowterminal'), 'wide Unicode draft did not wrap correctly');
  }
  terminal.dispose();
  console.log('Real PTY + terminal emulator: multiline send, stream, tools, help, pickers, Unicode wrapping/resize, detach and terminal restoration passed.');
}

if (process.argv.includes('--pty-fixture')) {
  const client = fakeClient({ automatic: true });
  runInteractive(client, { agentId: 'alpha' }).then(() => {
    process.stdout.write(`\nPTY_RESULT ${JSON.stringify({ sent: client.calls.filter((call) => call.method === 'send').map((call) => call.input.text), stops: client.calls.filter((call) => call.method === 'stop').length })}\n`);
  }).catch((error) => { console.error(error); process.exitCode = 1; });
} else {
  checks().then(() => ptyCheck()).catch((error) => { console.error(error); process.exitCode = 1; });
}
