const assert = require('node:assert/strict');
const path = require('node:path');
const {test} = require('node:test');
const {build} = require('esbuild');
const {JSDOM} = require('jsdom');
const React = require('react');
const {createRoot} = require('react-dom/client');
const {renderToStaticMarkup} = require('react-dom/server');
const dom = new JSDOM('<!doctype html><div id="root"></div>', {url: 'http://localhost', pretendToBeVisual: true});
const w = dom.window;
for (const key of ['window', 'document', 'HTMLElement', 'Element', 'SVGElement', 'Node', 'CustomEvent', 'Event', 'MouseEvent', 'getComputedStyle', 'navigator']) Object.defineProperty(globalThis, key, {configurable: true, value: key === 'window' ? w : key === 'getComputedStyle' ? w[key].bind(w) : w[key]});
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
w.matchMedia = () => ({matches: true, addEventListener() {}, removeEventListener() {}});
const compiled = build({
  stdin: {contents: `import './src/components/icon-glyphs/shell'; import './src/components/icon-glyphs/chat'; import './src/components/icon-glyphs/worker'; export * from './src/components/worker-environment/taskEnvironmentSummaryModel'; export {TaskSummaryPanel} from './src/components/chat/TaskSummaryPanel';`, resolveDir: path.resolve(__dirname, '..'), loader: 'tsx'},
  bundle: true, platform: 'node', format: 'cjs', write: false, loader: {'.css': 'empty'}, external: ['react', 'react-dom', 'react-dom/client', 'react/jsx-runtime'],
  plugins: [{name: 'summary-boundaries', setup(b) {
    b.onResolve({filter: /(?:^|\/)appearance$/}, () => ({path: 'appearance', namespace: 'test'}));
    b.onResolve({filter: /\/ArtifactWorkbench$/}, () => ({path: 'artifact', namespace: 'test'}));
    b.onLoad({filter: /.*/, namespace: 'test'}, ({path: name}) => ({contents: name === 'appearance'
      ? `export function useShouldReduceMotion(){return ${process.env.TASK_SUMMARY_REDUCED_MOTION !== 'false'}}`
      : 'export function ArtifactWorkbench(){throw new Error("Artifact preview is outside this test")}', loader: 'js'}));
  }}], logLevel: 'silent',
}).then(result => {
  const module = {exports: {}};
  Function('require', 'module', 'exports', result.outputFiles[0].text)(require, module, module.exports);
  return module.exports;
});
const navCompiled = build({
  stdin: {contents: `import './src/components/icon-glyphs/shell'; export * from './src/components/navigation/OctopusNav'; export {loadTaskHoverEnvironmentDetails} from './src/components/navigation/TaskHoverCard';`, resolveDir: path.resolve(__dirname, '..'), loader: 'tsx'},
  bundle: true, platform: 'node', format: 'cjs', write: false, loader: {'.css': 'empty'}, external: ['react', 'react-dom', 'react/jsx-runtime'],
  define: {'import.meta.env.DEV': 'false'}, logLevel: 'silent',
}).then(result => {
  const module = {exports: {}};
  Function('require', 'module', 'exports', result.outputFiles[0].text)(require, module, module.exports);
  return module.exports;
});
const surface = (id, kind, root = '', data = {}) => ({id, kind, root, data, label: kind, mode: 'evidence', status: 'ready', viewerUrl: '', protocol: '', readOnly: true});
const files = root => surface(`files:${root}`, 'files', root, {slot: 'readable_root'});
const environment = (id, surfaces) => ({id, runId: 'run-a', revision: '', eeId: 'ee-a', kind: 'shell', lifecycle: 'active', status: 'ready', live: true, label: 'Files', detail: '', substrateFamily: '', door: '', root: '/', session: '', transport: 'process_session.direct.v1', cells: [], capabilities: [], tools: [], surfaces});
const roots = ['/', '/Library/Developer/CommandLineTools', '/opt/homebrew', '/var/octopus/ee_instances'].map(files);
const review = {...surface('review:a', 'changes', '/work/project-a'), mode: 'direct', protocol: 'git.worktree'};
const worktree = surface('worktree:a', 'worktree_status', '', {worktree_git_root: '/work/project-a', worktree_workspace_root: '/work/project-a/src', branch: 'feature/context'});
const room = environment('room-a', [...roots, files('/work/project-a/src'), review, worktree, surface('terminal:a', 'terminal', '', {workspace: '/work/project-a/src'}), surface('browser:a', 'browser')]);
const second = environment('room-b', [surface('review:b', 'changes', '/other/project-b', {branch: 'feature/second', base_branch: 'main', additions: 7, deletions: 2})]);

test('workspace model separates permission roots, exact workspace identities and missing change evidence', async () => {
  const {taskEnvironmentContext, workspaceRevision, projectedChangeStats} = await compiled;
  const model = taskEnvironmentContext([room, second, room]);
  assert.deepEqual(model.workspaces.map(w => [w.environment.id, w.root]), [['room-a', '/work/project-a/src'], ['room-b', '/other/project-b']]);
  assert.deepEqual(model.workspaces[0].surfaces.map(s => s.id), ['review:a', 'worktree:a', 'files:/work/project-a/src', 'terminal:a']);
  assert.equal(workspaceRevision(model.workspaces[0]).branch, 'feature/context');
  assert.equal(workspaceRevision({...model.workspaces[0], surfaces: [worktree, {...review, data: {branch: 'conflicting'}}]}).conflict, true);
  assert.deepEqual(model.views[0].surfaces.map(s => s.id), [...roots.map(s => s.id), 'browser:a']);
  const connection = surface('connection:a', 'connection');
  const runtime = surface('runtime:a', 'runtime');
  assert.deepEqual(taskEnvironmentContext([environment('room-c', [connection, runtime])]).views[0].surfaces, [connection, runtime]);
  assert.equal(taskEnvironmentContext([environment('permissions-only', roots)]).workspaces.length, 0);
  assert.equal(taskEnvironmentContext([environment('plain', [surface('workspace', 'files', '/notes', {slot: 'workspace_root'})])]).workspaces[0].root, '/notes');
  assert.equal(projectedChangeStats({}), 'Open review');
  assert.equal(projectedChangeStats({diffs: []}), 'No changes reported');
  assert.equal(projectedChangeStats({diffs: [null]}), 'Open review');
  assert.equal(projectedChangeStats({additions: 0, diffs: [{path: 'a', additions: 99}]}), '1 changed');
  assert.equal(projectedChangeStats({changes: [{path: 'a', additions: 2, deletions: 3}, null]}), '+2 −3');
});

test('actual OctopusNav SSR closure and hover loader retain contextual workspaces and reject mismatched projection identity', async () => {
  const {OctopusTaskRow, loadTaskHoverEnvironmentDetails} = await navCompiled;
  const rawRoom = {worker_environment_id: room.id, run_id: 'run-a', ee_id: 'ee-a', root: '/', transport: room.transport, client_surfaces: room.surfaces};
  let payload = {run: {run_id: 'run-a', ee_id: 'ee-a'}, worker_environments: [rawRoom, {...rawRoom, worker_environment_id: 'another-room'}]};
  const calls = [];
  w.octopusTransport = {projectionRead: async request => {calls.push(request); return payload;}};
  const markup = renderToStaticMarkup(React.createElement(OctopusTaskRow, {title: 'Contextual task', active: false, onClick() {}, hoverDetails: {runId: 'run-a', eeId: 'ee-a'}}));
  assert.match(markup, /Contextual task/);
  assert.deepEqual(calls, []);
  const details = await loadTaskHoverEnvironmentDetails('run-a', 'ee-a');
  assert.deepEqual(details.folders.map(folder => folder.path), ['/work/project-a/src', '/work/project-a/src']);
  assert.equal(new Set(details.folders.map(folder => folder.id)).size, 2);
  assert(details.folders.every(folder => folder.worktree));
  assert.equal('location' in details, false);
  assert.deepEqual(calls[0].params, {run_id: 'run-a', include_ledger: false});
  payload.worker_environments = [{...rawRoom, client_surfaces: roots}];
  assert.deepEqual((await loadTaskHoverEnvironmentDetails('run-a', 'ee-a')).folders, []);
  payload.worker_environments = [{...rawRoom, run_id: 'another-run'}];
  await assert.rejects(loadTaskHoverEnvironmentDetails('run-a', 'ee-a'), /identity does not match/);
  payload = {run: {run_id: 'run-a', ee_id: 'another-ee'}, worker_environments: []};
  await assert.rejects(loadTaskHoverEnvironmentDetails('run-a', 'ee-a'), /identity does not match/);
  delete w.octopusTransport;
});

test('actual React summary opens exact panes, reveals related activity, keeps sources and withdraws stale context without native reads', {timeout: 10000}, async () => {
  const nativeCalls = [];
  w.octopusTransport = new Proxy({}, {get(_, key) {nativeCalls.push(key); throw new Error(`Unexpected native access: ${String(key)}`);}});
  const {TaskSummaryPanel} = await compiled;
  const root = createRoot(w.document.getElementById('root'));
  const opened = [], revealed = [], sourceLists = [];
  const messages = [
    {id: 'related', runId: 'run-a', text: '', eventKind: 'subagent_activity', eventData: {agentThreadId: 'agent-b', agentPath: 'team/Research', kind: 'started'}, createdAtMs: 1},
    {id: 'status', runId: 'run-a', text: '', eventKind: 'collaboration', eventData: {tool: 'wait', agentsStates: {'agent-b': {status: 'completed'}}}, createdAtMs: 2},
    {id: 'sources', runId: 'run-a', text: '', attachments: [{id: 'notes', name: 'notes.txt', type: 'text/plain'}], createdAtMs: 3},
  ];
  const custom = {sourceDigest: 'digest-a', document: {id: 'viewer', revision: 2, title: 'Task viewer', glyph: 'browser', target: {kind: 'worker_environment.surface', worker_environment_id: 'room-a', surface_id: 'custom:a', order: 0}}};
  const props = {messages, surfaces: [custom], runProjection: {id: 'run-a', workerEnvironment: room, workerEnvironments: [room, second]}, onOpenEnvironment: (...args) => opened.push(args), onRevealMessage: id => revealed.push(id), onShowAllSources: sources => sourceLists.push(sources)};
  const render = overrides => React.act(async () => root.render(React.createElement(React.StrictMode, null, React.createElement(TaskSummaryPanel, {...props, ...overrides, key: (overrides?.runProjection ?? props.runProjection)?.id}))));
  const buttons = () => [...w.document.querySelectorAll('button')];
  const click = async button => {assert.ok(button); await React.act(async () => button.dispatchEvent(new w.MouseEvent('click', {bubbles: true})));};
  try {
    await render();
    assert.doesNotMatch(w.document.body.textContent, /No workspace attached|Environment details|\bLocal\b|Loading…/);
    assert.match(w.document.body.textContent, /CommandLineTools|homebrew|ee_instances/);
    assert.equal(w.document.querySelectorAll('.task-environment-room').length, 2);
    const toggles = [...w.document.querySelectorAll('.task-environment-toggle')];
    assert.deepEqual(toggles.map(b => b.getAttribute('aria-expanded')), ['true', 'false']);
    const secondBody = w.document.getElementById(toggles[1].getAttribute('aria-controls'));
    assert.equal(secondBody.hidden, true);
    await click(toggles[1]); assert.equal(secondBody.hidden, false);
    await click([...secondBody.querySelectorAll('button')].find(b => b.textContent.startsWith('Changes')));
    assert.deepEqual(opened.pop(), ['room-b', 'changes', 'review:b']);
    await click(buttons().find(b => b.title.includes('files:/work/project-a/src')));
    assert.deepEqual(opened.pop(), ['room-a', 'files', 'files:/work/project-a/src']);
    await click(buttons().find(b => b.title.includes('terminal:a')));
    assert.deepEqual(opened.pop(), ['room-a', 'terminal', 'terminal:a']);
    await click(buttons().find(b => b.textContent === 'Show 1 more'));
    await click(buttons().find(b => b.title.includes('browser:a')));
    assert.deepEqual(opened.pop(), ['room-a', 'browser', 'browser:a']);
    await click(buttons().find(b => b.textContent === 'Task viewer'));
    assert.deepEqual(opened.pop(), ['room-a', 'custom', 'custom:a', {documentId: 'viewer', revision: 2, sourceDigest: 'digest-a'}]);
    await click(buttons().find(b => b.title === 'agent-b')); assert.deepEqual(revealed, ['status']);
    assert.match(w.document.querySelector('.task-summary-related-agents').textContent, /Completed/);
    await click(buttons().find(b => b.textContent === 'View all')); assert.equal(sourceLists[0][0].label, 'notes.txt');
    await render({surfaces: [], messages: [], runProjection: {id: 'run-c', workerEnvironment: environment('only-roots', roots), workerEnvironments: []}});
    assert.doesNotMatch(w.document.body.textContent, /No workspace attached|Environment details/);
    assert.doesNotMatch(w.document.body.textContent, /feature\/context|project-b|Task viewer|Related agents|notes.txt/);
    await click(buttons().find(b => b.title.includes('files:/var/octopus/ee_instances')));
    assert.deepEqual(opened.pop(), ['only-roots', 'files', 'files:/var/octopus/ee_instances']);
    await click(buttons().find(b => b.getAttribute('aria-label') === 'Details for Files')); assert.deepEqual(opened.pop(), ['only-roots', 'overview']);
    await render({messages: [], surfaces: [], runProjection: null}); assert.match(w.document.body.textContent, /Environment information unavailable/);
    assert.deepEqual(nativeCalls, []);
  } finally {await React.act(async () => root.unmount()); dom.window.close();}
});
