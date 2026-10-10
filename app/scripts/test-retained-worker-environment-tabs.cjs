const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {test} = require('node:test');
const ts = require('typescript');
const {build} = require('esbuild');
const {JSDOM} = require('jsdom');
const React = require('react');
const appRoot = path.resolve(__dirname, '..');

test('real worker file drafts survive launcher selection, hiding and reordering; close releases them', async () => {
  const dom = new JSDOM('<!doctype html><div id="root"></div>', {url: 'http://localhost', pretendToBeVisual: true});
  const w = dom.window;
  for (const key of ['window', 'document', 'HTMLElement', 'Element', 'SVGElement', 'Node', 'CustomEvent', 'Event', 'MouseEvent', 'getComputedStyle', 'navigator', 'customElements', 'MutationObserver']) {
    Object.defineProperty(globalThis, key, {configurable: true, value: key === 'window' ? w : key === 'getComputedStyle' ? w[key].bind(w) : w[key]});
  }
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
  globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
  globalThis.ResizeObserver = class {observe() {} disconnect() {}};
  w.matchMedia = () => ({matches: true, addEventListener() {}, removeEventListener() {}});
  w.HTMLElement.prototype.getBoundingClientRect = () => ({left: 0, top: 0, width: 800, height: 600, right: 800, bottom: 600});
  const channels = [], NativeBroadcastChannel = globalThis.BroadcastChannel;
  globalThis.BroadcastChannel = class extends NativeBroadcastChannel {constructor(name) {super(name); channels.push(this);}};
  const listeners = new Set(), add = w.addEventListener.bind(w), remove = w.removeEventListener.bind(w);
  w.addEventListener = (type, listener, ...args) => {if (type === 'octopus:worker-environment-local-mutation') listeners.add(listener); add(type, listener, ...args);};
  w.removeEventListener = (type, listener, ...args) => {if (type === 'octopus:worker-environment-local-mutation') listeners.delete(listener); remove(type, listener, ...args);};
  const {createRoot} = require('react-dom/client');
  const result = await build({
    stdin: {contents: `import './src/components/icon-glyphs/shell'; import './src/components/icon-glyphs/chat'; export {RetainedWorkerEnvironmentTabs} from './src/components/shell/RetainedWorkerEnvironmentTabs';`, resolveDir: appRoot, loader: 'tsx'},
    bundle: true, platform: 'node', format: 'cjs', write: false, loader: {'.css': 'empty', '.png': 'dataurl'},
    external: ['react', 'react-dom', 'react-dom/client', 'react/jsx-runtime'], logLevel: 'silent',
    plugins: [{name: 'display-dependencies', setup(b) {
      b.onResolve({filter: /(?:^|\/)appearance$/}, () => ({path: 'appearance', namespace: 'test'}));
      b.onResolve({filter: /^@pierre\/diffs\/react$/}, () => ({path: 'diffs', namespace: 'test'}));
      b.onResolve({filter: /octopusDiffThemes$/}, () => ({path: 'themes', namespace: 'test'}));
      b.onLoad({filter: /.*/, namespace: 'test'}, ({path: name}) => ({contents: name === 'appearance'
        ? 'export function useAppearance(){return {themeMode:"dark"}}; export function useShouldReduceMotion(){return true}'
        : name === 'themes' ? 'export function registerOctopusDiffThemes(){}; export function octopusDiffThemeName(){return "test"}'
        : 'import React from "react"; export function File({file}){return React.createElement("pre",null,file.contents)}; export function FileDiff(){return null}', loader: 'js'}));
    }}],
  });
  const module = {exports: {}};
  Function('require', 'module', 'exports', result.outputFiles[0].text)(require, module, module.exports);
  const browserSource = ts.createSourceFile('browser.tsx', fs.readFileSync(path.join(appRoot, 'src/components/worker-environment/RunEnvironmentBrowserSurfaces.tsx'), 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const geometrySource = browserSource.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === 'nativeBrowserGeometry');
  const geometry = Function('window', ts.transpileModule(`${geometrySource.getText(browserSource)}\nreturn nativeBrowserGeometry;`, {compilerOptions: {target: ts.ScriptTarget.ES2022}}).outputText)(w);
  const room = id => ({id, revision: '1', runId: '', lifecycle: 'active', surfaces: [{id: `files-${id}`, kind: 'files', label: 'Files', mode: 'direct', protocol: 'filesystem.readwrite', readOnly: false, root: `/bounded/${id}`, data: {}}]});
  const rooms = [room('a'), room('b')];
  rooms[0].surfaces.push({id: 'browser', kind: 'browser', label: 'Browser', mode: 'direct', protocol: 'browser.native.v1', root: '', data: {target_id: 'page-a'}});
  const snapshot = {environments: [{id: 'ee', name: 'EE', runs: [], workerEnvironments: rooms}]};
  const fileTab = id => ({id: `file-${id}`, title: 'same.json', target: {kind: 'worker_environment', target: {schemaVersion: 1, workerEnvironmentId: id, surfaceId: `files-${id}`, panelKind: 'file', filePath: 'same.json'}}});
  const a = fileTab('a'), b = fileTab('b');
  const launcher = {id: 'new-tab', target: {kind: 'launcher', runId: 'run', workspaceKey: 'octopus-os'}};
  const browser = {id: 'browser', target: {kind: 'worker_environment', target: {schemaVersion: 1, workerEnvironmentId: 'a', surfaceId: 'browser', panelKind: 'browser'}}};
  let tabs = [a, b, launcher, browser];
  const reads = [], writes = [], failures = [], browserReleases = [];
  let prepareBrowser, browserRequest, browserListeners = 0;
  w.octopusTransport = {
    readWorkerEnvironmentFile: async request => {reads.push(request); return {...request, kind: 'text', text: `original-${request.workerEnvironmentId}`, size: 10, modifiedAt: 'now', digest: 'a'.repeat(64), truncated: false};},
    writeWorkerEnvironmentFile: async request => {writes.push(request); throw Error('Unexpected save');},
    logClientEvent: async event => {failures.push(event);},
    onNativeBrowserViewerEvent: () => {browserListeners++; return () => {browserListeners--;};},
    prepareNativeBrowserViewer: request => {browserRequest = request; return new Promise(resolve => {prepareBrowser = resolve;});},
    closeNativeBrowserViewer: async request => {browserReleases.push(request);},
  };
  const root = createRoot(w.document.getElementById('root'));
  const render = async (activeTabId, active = true) => React.act(async () => root.render(React.createElement(module.exports.RetainedWorkerEnvironmentTabs, {accessProfile: 'live', active, activeTabId, snapshot, surfaces: [], tabs})));
  const edit = async text => {
    await React.act(async () => [...w.document.querySelectorAll('[aria-label="Edit file manually"]')].find(button => !button.closest('[hidden]')).click());
    await React.act(async () => [...w.document.querySelectorAll('button')].find(button => button.textContent === 'Continue manually').click());
    const editor = [...w.document.querySelectorAll('textarea')].find(element => !element.closest('[hidden]'));
    await React.act(async () => {Object.getOwnPropertyDescriptor(w.HTMLTextAreaElement.prototype, 'value').set.call(editor, text); editor.dispatchEvent(new w.Event('input', {bubbles: true}));});
    assert.equal(editor.closest('.we-file-preview').querySelector('small').textContent, 'Unsaved');
    return editor;
  };
  const assertHidden = editor => {
    assert(editor.isConnected);
    const holder = editor.closest('[hidden][inert][aria-hidden="true"]');
    assert(holder);
    assert.equal(holder.style.display, 'none');
    assert.equal(geometry(editor, {emulatedSize: null}, false).visible, false);
  };
  try {
    await render(a.id);
    assert.deepEqual(reads, [{workerEnvironmentId: 'a', surfaceId: 'files-a', path: 'same.json'}], 'unvisited tabs never mount');
    const editorA = await edit('unsaved A');
    await render(launcher.id);
    assertHidden(editorA);
    await render(a.id);
    assert.equal(w.document.querySelector('textarea'), editorA);
    assert.equal(editorA.value, 'unsaved A');
    assert.equal(geometry(editorA, {emulatedSize: null}, false).visible, true);
    assert.equal(reads.length, 1, 'return does not remount the actual file reader');

    await render(b.id);
    assert.deepEqual(reads[1], {workerEnvironmentId: 'b', surfaceId: 'files-b', path: 'same.json'});
    const editorB = await edit('unsaved B');
    assertHidden(editorA);
    tabs = [b, launcher, a, browser];
    await render(a.id);
    assert.equal(editorA.value, 'unsaved A');
    assert.equal(editorB.value, 'unsaved B');
    assertHidden(editorB);
    await render(a.id, false);
    assertHidden(editorA);

    await render(browser.id);
    const browserPane = w.document.querySelector('.we-browser-shell.is-native');
    assert(browserPane);
    assert.equal(browserRequest.workerEnvironmentId, 'a');
    assert.equal(browserRequest.surfaceId, 'browser');
    assert.equal(browserListeners, 1);
    assertHidden(editorA);
    await render(launcher.id);
    assert.equal(browserPane.isConnected, false, 'native browser panes are unmounted, never retained hidden');
    assert.equal(w.document.querySelector('.we-browser-shell'), null);
    assert.equal(browserListeners, 0);
    await React.act(async () => prepareBrowser({hostGeneration: 'host', mountGeneration: 1}));
    assert.deepEqual(browserReleases, [{attachmentId: browserRequest.attachmentId, hostGeneration: 'host', mountGeneration: 1}], 'even a late native preparation is released after the launcher switch');

    assert.equal(listeners.size, 2);
    tabs = tabs.filter(tab => tab.id !== a.id);
    await render(launcher.id);
    assert.equal(editorA.isConnected, false);
    assert.equal(listeners.size, 1, 'closing releases the file editor mutation subscription');
    assertHidden(editorB);
    tabs.push(a);
    await render(a.id);
    assert.equal(w.document.querySelectorAll('textarea').length, 1, 'reopened A has no retained draft; only B remains in edit mode');
    assert.equal(w.document.querySelector('pre').textContent, 'original-a');
    assert.equal(reads.length, 3);
    assert.equal(editorB.value, 'unsaved B');
    assert.deepEqual(writes, [], 'selection never persists or auto-saves a draft');
    assert.deepEqual(failures, []);
  } finally {
    await React.act(async () => root.unmount());
    assert.equal(listeners.size, 0);
    channels.forEach(channel => channel.close());
    globalThis.BroadcastChannel = NativeBroadcastChannel;
    dom.window.close();
  }
});
