const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const { build } = require('esbuild');
const { test } = require('node:test');

const root = path.resolve(__dirname, '..');
const projectionSource = fs.readFileSync(path.join(root, 'electron/projection.cjs'), 'utf8');
function declaration(source, name) {
  const tree = ts.createSourceFile('source.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let found;
  const visit = node => { if (ts.isFunctionDeclaration(node) && node.name?.text === name) found = node.getText(tree); ts.forEachChild(node, visit); };
  visit(tree); assert(found, name); return found;
}
const projectContext = vm.createContext({
  options: {},
  projectedFilesystemRootSurfaces: () => [], projectedGitSurfaces: async () => [], projectedWorktreeStatusSurfaces: () => [],
});
for (const name of ['stringValue', 'arrayValue', 'objectOrEmpty', 'workerEnvironmentCapabilities', 'workerEnvironmentWorkspace', 'unavailableViewerSurface', 'projectedWorkerEnvironmentSurfaces']) {
  vm.runInContext(declaration(projectionSource, name), projectContext);
}
const input = () => ({
  octopusHomeRoot: '/Home-A',
  formation: { worker_environment_id: 'room-a', formation_pin: 'a'.repeat(64), run: 'run-a', door_interface: 'door-a', worker_environment_kind: 'browser_session' },
  interfaceVarDir: '/fixture', kindCapabilities: [], lifecycle: 'active', openedAtMovementSeq: 10, lastActivityMovementSeq: 20, warnings: [],
  liveState: { status: 'running', viewer: { protocol: 'browser.native.v1', engine_handle: 'engine', host_generation: 'generation', browser_context_id: 'context', target_id: 'target-a' },
    activity: { status: 'running', source_movement_seq: 20 },
    tabs: [{ target_id: 'target-a', browser_context_id: 'context', url: 'https://a.example', title: 'A' }, { target_id: 'target-b', browser_context_id: 'context', title: 'B' }] },
});
async function project(raw) {
  projectContext.nativeBrowserTargetsForProjection = async (tuple) => {
    assert.equal(tuple.octopusHome, '/Home-A'); assert.equal(tuple.browser_context_id, raw.liveState.viewer.browser_context_id);
    if (raw.observationError) throw raw.observationError;
    return raw.observation ?? { targets: raw.liveState.tabs, openIntent: null };
  };
  return projectContext.projectedWorkerEnvironmentSurfaces(raw);
}
const compiled = build({
  entryPoints: [path.join(root, 'src/components/worker-environment/workerEnvironmentPane.ts')], bundle: true, platform: 'node', format: 'cjs', write: false, logLevel: 'silent',
}).then(result => { const module = { exports: {} }; Function('module', 'exports', 'require', result.outputFiles[0].text)(module, module.exports, require); return module.exports; });
function snapshot(raw, surfaces) {
  const room = { id: 'room-a', runId: 'run-a', lifecycle: raw.lifecycle, label: 'Browser', surfaces: surfaces.map(surface => ({ ...surface, readOnly: surface.read_only })) };
  return { projection: { mode: 'live', endpoint: '/Home-A' }, environments: [{ id: 'ee', name: 'EE', workerEnvironments: [room], runs: [{ id: 'run-a', workerEnvironments: [room] }] }] };
}

test('native projection exposes only exact context members, never fabricates current target or URL transport', async () => {
  const raw = input();
  const surfaces = await project(raw);
  const pages = surfaces.filter(surface => surface.kind === 'browser');
  assert.equal(pages.length, 2);
  assert.equal(pages[0].data.formation_pin, raw.formation.formation_pin);
  assert.equal(pages[0].data.activity.source_movement_seq, 20);
  assert.equal(pages[1].data.activity, null);
  assert.equal(pages[0].data.opened_at_movement_seq, 10);
  assert.equal('viewer_url' in pages[0], false);
  for (const modify of [r => { r.lifecycle = 'released'; }, r => { r.liveState.tabs[1].browser_context_id = 'foreign'; }, r => { r.liveState.tabs[1].target_id = 'target-a'; }, r => { r.liveState.tabs = []; }, r => { delete r.liveState.viewer.engine_handle; }]) {
    const invalid = input(); modify(invalid);
    assert.equal((await project(invalid)).filter(surface => surface.kind === 'browser' && surface.mode === 'direct').length, 0);
  }
  const stale = input();
  stale.liveState.status = 'ready'; stale.liveState.url = 'https://a.example'; stale.liveState.title = 'A';
  stale.observationError = Object.assign(new Error('Native browser host generation is unavailable'), { code: 'host_unavailable' });
  const staleSurfaces = await project(stale), offline = staleSurfaces.find(surface => surface.kind === 'browser');
  assert.equal(offline.status, 'unavailable'); assert.equal(offline.mode, 'evidence'); assert.equal(offline.read_only, true);
  assert.equal(offline.data.reason_code, 'host_unavailable'); assert.equal(offline.data.last_known_url, 'https://a.example');
  assert.equal('target_id' in offline.data, false); assert.equal('engine_handle' in offline.data, false);
  const { resolvePaneSurface, paneTargetTitle } = await compiled, exactPage = { surfaceId: pages[0].id, panelKind: 'browser' };
  assert.equal(resolvePaneSurface(snapshot(stale, staleSurfaces).environments[0].workerEnvironments[0], exactPage)?.id, pages[0].id);
  assert.equal(staleSurfaces.find(surface => surface.id === pages[1].id).data.last_known_url, '', 'secondary evidence cannot borrow the primary URL');
  const withdrawn = { ...snapshot(raw, surfaces).environments[0].workerEnvironments[0], label: 'Downloads', surfaces: [{ id: 'room-a:downloads', kind: 'downloads', protocol: 'browser.native.downloads.v1', label: 'Downloads' }] };
  assert.equal(resolvePaneSurface(withdrawn, exactPage), null); assert.deepEqual(paneTargetTitle(withdrawn, exactPage), { title: 'Browser', glyph: 'browser' });
  const { activeWorkerEnvironmentPaneActivities } = await compiled;
  assert.equal(activeWorkerEnvironmentPaneActivities(snapshot(stale, staleSurfaces), ['run-a']).length, 0);
  const model = snapshot(raw, surfaces);
  const activities = activeWorkerEnvironmentPaneActivities(model, ['run-a']);
  assert.equal(activities.length, 1); assert.equal(activities[0].running, true);
  assert.equal(activeWorkerEnvironmentPaneActivities(model, ['run-b']).length, 0);
  assert.equal(activeWorkerEnvironmentPaneActivities(model, []).length, 0);
  const identity = activities[0].identity;
  pages[0].data.opened_at_movement_seq = 30;
  assert.notEqual(activeWorkerEnvironmentPaneActivities(model, ['run-a'])[0].identity, identity);
});

test('closed browser pages retain Downloads actions only with an explicit history capability', async () => {
  const modes = require('../../registry/worker_environment_kind/browser_session/packages/octopus-browser-session-env/native/engine-modes.json');
  const { workerEnvironmentPaneActions } = await compiled;
  try { for (const mode of ['standard', 'owned', 'disabled', 'undeclared']) {
    const engine = mode === 'disabled' ? { ...modes.standard, capabilities: { ...modes.standard.capabilities, persistentDownloadHistory: false } } : modes[mode];
    projectContext.nativeBrowserDownloadsForProjection = async () => ({ engine, host_generation: 'generation', downloads: { sequence: 0, downloads: [], unacknowledgedIds: [], preferences: null, error: null } });
    const raw = input(); raw.liveState.tabs = [];
    const surfaces = await project(raw);
    assert.equal(surfaces.some(surface => surface.protocol === 'browser.native.downloads.v1'), mode === 'owned' || mode === 'standard');
    assert.equal(workerEnvironmentPaneActions(snapshot(raw, surfaces), ['run-a']).some(action => action.title.includes('Downloads')), mode === 'owned' || mode === 'standard');
  } } finally { delete projectContext.nativeBrowserDownloadsForProjection; }
});

test('actual shell presentation effect fences Home, historical completed rows and transfers; secondary task retains its own tab', async () => {
  const { activeWorkerEnvironmentPaneActivities } = await compiled;
  const source = fs.readFileSync(path.join(root, 'src/components/shell/OperatorShell.tsx'), 'utf8');
  const tree = ts.createSourceFile('shell.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let callback;
  const visit = node => {
    if (ts.isCallExpression(node) && node.expression.getText(tree) === 'useEffect' && node.arguments[0]?.getText(tree).includes('const unseen = activities.filter')) callback = node.arguments[0].getText(tree);
    ts.forEachChild(node, visit);
  }; visit(tree); assert(callback);
  const js = ts.transpile(`const effect = ${callback};`, { target: ts.ScriptTarget.ES2022 });
  let tabs = [], activated = [], taskTabs = {}, opens = 0, focused = 0;
  const raw = input(); raw.liveState.activity.status = 'completed';
  const c = { accessProfile: 'admin', runtimeHost: { home: '/Home-A', ready: true }, topology: snapshot(raw, await project(raw)),
    activeClientSurfaceIdentity: { ready: true, pane: 'primary', runId: 'run-a' }, activeSecondaryTab: null,
    presentedBrowserTargetsRef: { current: { scope: '', baseline: new Map(), identities: new Set() } },
    nativeWindowCloseRequestRef: { current: null }, pendingAcceptedPaneTransfersRef: { current: new Map() }, preparedSourceTransfersRef: { current: new Map() },
    sourceHandoffPending: false, sourceFinalizationPendingCount: 0, workerEnvironmentActionRunIds: ['run-a'], activeWorkerEnvironmentPaneActivities,
    workspacePaneTabForTarget: target => ({ id: target.target.surfaceId, target }), emptyTaskBrowserTabs: { activeTabId: '', tabs: [], open: false },
    setSecondaryTabs: update => { tabs = update(tabs); }, activateSecondaryTab: tab => activated.push(tab), setSecondaryPaneOpen: () => opens++,
    setTaskBrowserTabs: update => { taskTabs = update(taskTabs); }, setSecondaryPaneFocused: () => focused++,
  };
  const context = vm.createContext(c); vm.runInContext(js, context); const run = () => vm.runInContext('effect()', context);
  run(); assert.equal(tabs.length, 0);
  raw.liveState.activity.source_movement_seq = 21;
  c.topology = snapshot(raw, await project(raw));
  c.runtimeHost.home = '/Home-B'; run(); assert.equal(tabs.length, 0);
  c.runtimeHost.home = '/Home-A'; c.sourceFinalizationPendingCount = 1; run(); assert.equal(tabs.length, 0);
  c.sourceFinalizationPendingCount = 0; run(); assert.equal(tabs.length, 1); assert.equal(activated.length, 1); assert.equal(opens, 1);
  run(); assert.equal(activated.length, 1);
  raw.liveState.viewer.target_id = 'target-b'; raw.liveState.activity.source_movement_seq = 22;
  c.topology = snapshot(raw, await project(raw));
  c.activeClientSurfaceIdentity = { ready: true, pane: 'secondary', runId: 'run-a' };
  c.activeSecondaryTab = { id: 'task-a', target: { kind: 'surface', surfaceId: 'chat', runId: 'run-a' } };
  run(); assert.equal(tabs.length, 1); assert.equal(activated.length, 1); assert.equal(c.activeSecondaryTab.id, 'task-a');
  assert.equal(Object.values(taskTabs)[0].tabs.length, 1); assert.equal(focused, 1);
  raw.liveState.presentation_intent = { id: 24, target_id: 'target-b', source: 'tool', activate: true };
  c.topology = snapshot(raw, await project(raw)); run(); assert.equal(focused, 2); assert.equal(Object.values(taskTabs)[0].tabs.length, 1);
  raw.observation = { targets: [...raw.liveState.tabs, { target_id: 'popup', browser_context_id: 'context', title: 'Popup' }], openIntent: { id: 1, targetId: 'popup', source: 'popup', activate: false } };
  c.topology = snapshot(raw, await project(raw)); run(); assert.equal(focused, 2); assert.equal(Object.values(taskTabs)[0].tabs.length, 2);
  raw.observation.openIntent = { id: 2, targetId: 'popup', source: 'context-menu', activate: true };
  c.topology = snapshot(raw, await project(raw)); run(); assert.equal(focused, 3); assert.equal(Object.values(taskTabs)[0].tabs.length, 2);
  const desktop = { ...input(), formation: { ...input().formation, worker_environment_kind: 'desktop_session' },
    liveState: { status: 'ready', viewer: { protocol: 'desktop_session', address: 'http://127.0.0.1:8123', host_generation: 'actual-generation' }, desktop: { status: 'ready' } } };
  projectContext.options = {}; projectContext.validHttpAddress = value => new URL(value).protocol === 'http:';
  c.topology = snapshot(desktop, await project(desktop));
  c.activeSecondaryTab = { id: 'home', target: { kind: 'surface', surfaceId: 'voice' } };
  run(); assert.equal(c.activeSecondaryTab.id, 'home'); assert.equal(focused, 4);
  const homeDesktop = Object.values(taskTabs)[0].tabs.find(tab => tab.target.panelKind === 'desktop');
  assert(homeDesktop); assert.equal(homeDesktop.target.workerEnvironmentId, 'room-a');
  assert.equal(activated.length, 1, 'secondary Home is retained instead of replaced by its environment');
  c.workerEnvironmentActionRunIds = ['run-b']; run(); assert.equal(focused, 4, 'another run cannot open Home desktop');

});

test('actual task panel preserves composer DOM, draft and focus while browser tabs open and close', async () => {
  const { JSDOM } = require('jsdom');
  const React = require('react');
  const dom = new JSDOM('<div id="root"></div>', { url: 'http://fixture.invalid', pretendToBeVisual: true });
  const w = dom.window;
  for (const key of ['window', 'document', 'HTMLElement', 'Element', 'SVGElement', 'Node', 'CustomEvent', 'Event', 'MouseEvent', 'getComputedStyle', 'navigator']) Object.defineProperty(globalThis, key, { configurable: true, value: key === 'window' ? w : key === 'getComputedStyle' ? w[key].bind(w) : w[key] });
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w); globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
  globalThis.ResizeObserver = class { observe() {} disconnect() {} };
  globalThis.IntersectionObserver = class { observe() {} unobserve() {} disconnect() {} };
  w.matchMedia = () => ({ matches: true, addEventListener() {}, removeEventListener() {} });
  w.HTMLElement.prototype.scrollIntoView = function () {};
  w.HTMLElement.prototype.getBoundingClientRect = function () { return { x: 0, y: 0, left: 0, top: 0, right: 1024, bottom: 768, width: 1024, height: 768 }; };
  const { createRoot } = require('react-dom/client');
  const errors = []; w.addEventListener('error', event => errors.push(event.error));
  const result = await build({
    stdin: { contents: "import './src/components/icon-glyphs/shell'; export * from './src/components/shell/TaskBrowserPane';", resolveDir: root, loader: 'tsx' },
    bundle: true, platform: 'node', format: 'cjs', write: false, loader: { '.css': 'empty' }, external: ['react', 'react-dom', 'react-dom/client', 'react/jsx-runtime'],
    plugins: [{ name: 'native-boundary', setup(b) {
      b.onResolve({ filter: /\/WorkerEnvironmentPaneTab$/ }, () => ({ path: 'native', namespace: 'fixture' }));
      b.onResolve({ filter: /(?:^|\/)appearance$/ }, () => ({ path: 'appearance', namespace: 'fixture' }));
      b.onLoad({ filter: /.*/, namespace: 'fixture' }, ({ path: name }) => ({ contents: name === 'appearance' ? 'export function useShouldReduceMotion(){return true}' : "import React from 'react'; export function WorkerEnvironmentPaneTab({target}){return React.createElement('div',{'data-native-target':target.surfaceId})}", loader: 'js' }));
    }}], logLevel: 'silent',
  });
  const module = { exports: {} }; Function('require', 'module', 'exports', result.outputFiles[0].text)(require, module, module.exports);
  const { TaskBrowserPane, emptyTaskBrowserTabs } = module.exports;
  const raw = input(), model = snapshot(raw, await project(raw));
  const reactRoot = createRoot(w.document.getElementById('root'));
  let state = emptyTaskBrowserTabs;
  const render = () => reactRoot.render(React.createElement(TaskBrowserPane, { accessProfile: 'admin', active: true, snapshot: model, state, surfaces: [], onChange: update => { state = update(state); render(); } }, React.createElement('input', { id: 'composer', defaultValue: 'unsent draft' })));
  const flush = () => React.act(async () => { await new Promise(resolve => setTimeout(resolve, 50)); });
  await React.act(render); await flush();
  const composer = w.document.getElementById('composer'); await React.act(() => { composer.focus(); composer.value = 'unsent draft + new typing'; });
  state = { activeTabId: 'a', open: true, tabs: [{ id: 'a', target: { schemaVersion: 1, workerEnvironmentId: 'room-a', surfaceId: 'room-a:browser:target-a', panelKind: 'browser' } }] };
  await React.act(render); await flush();
  assert.equal(w.document.getElementById('composer'), composer); assert.equal(w.document.activeElement, composer); assert.equal(composer.value, 'unsent draft + new typing');
  assert.equal(w.document.querySelector('[data-native-target]').getAttribute('data-native-target'), 'room-a:browser:target-a');
  const closes = []; w.octopusTransport = { closeNativeBrowserPage: async request => closes.push(request) };
  await React.act(() => w.document.querySelector('[aria-label="Close secondary pane"]').click()); await flush();
  assert.equal(state.tabs.length, 1); assert.equal(state.open, false); assert.equal(closes.length, 0);
  assert.equal(w.document.getElementById('composer'), composer); assert.equal(w.document.activeElement, composer); assert.equal(w.document.querySelector('[data-native-target]'), null);
  state = { ...state, open: true }; await React.act(render); await flush();
  await React.act(async () => w.document.querySelector('[aria-label="Close A tab"]').click()); await flush();
  assert.deepEqual(closes, [{ home: '/Home-A', workerEnvironmentId: 'room-a', surfaceId: 'room-a:browser:target-a' }]);
  assert.equal(state.tabs.length, 0); assert.equal(w.document.getElementById('composer'), composer);
  for (const mode of ['evidence', 'direct']) {
    model.environments[0].workerEnvironments[0].surfaces = [{ id: 'unavailable', kind: 'browser', protocol: 'browser.native.v1', label: 'Browser', mode, readOnly: true, status: 'unavailable', data: {} }];
    state = { activeTabId: 'stale', open: true, tabs: [{ id: 'stale', target: { schemaVersion: 1, workerEnvironmentId: 'room-a', surfaceId: 'unavailable', panelKind: 'browser' } }] };
    await React.act(render); await flush();
    await React.act(async () => w.document.querySelector('[aria-label="Close Browser tab"]').click()); await flush();
    assert.equal(state.tabs.length, 0); assert.equal(closes.length, 1, 'dismissing a read-only tab cannot close a native page');
    assert.equal(w.document.getElementById('composer'), composer);
  }
  await React.act(() => reactRoot.unmount()); assert.deepEqual(errors, []); dom.window.close();
});
