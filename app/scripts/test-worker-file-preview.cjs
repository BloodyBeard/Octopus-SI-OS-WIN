const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {test} = require('node:test');
const {build} = require('esbuild');
const {JSDOM} = require('jsdom');
const React = require('react');
const {createRoot} = require('react-dom/client');
const appRoot = path.resolve(__dirname, '..');

test('file preview owns a compact header and routes each action to its exact room and file', async () => {
  const dom = new JSDOM('<!doctype html><div id="root"></div>', {url: 'http://localhost', pretendToBeVisual: true});
  const w = dom.window;
  const channels = [], NativeBroadcastChannel = globalThis.BroadcastChannel;
  globalThis.BroadcastChannel = class extends NativeBroadcastChannel { constructor(name) {super(name); channels.push(this);} };
  for (const key of ['window', 'document', 'HTMLElement', 'Element', 'SVGElement', 'Node', 'CustomEvent', 'Event', 'MouseEvent', 'getComputedStyle', 'navigator']) Object.defineProperty(globalThis, key, {configurable: true, value: key === 'window' ? w : key === 'getComputedStyle' ? w[key].bind(w) : w[key]});
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
  globalThis.cancelAnimationFrame = w.cancelAnimationFrame.bind(w);
  w.matchMedia = () => ({matches: true, addEventListener() {}, removeEventListener() {}});
  const result = await build({
    stdin: {contents: `import './src/components/icon-glyphs/shell'; import './src/components/icon-glyphs/chat'; import './src/components/icon-glyphs/worker'; export {FilePreviewSurface} from './src/components/worker-environment/RunEnvironmentFileSurfaces';`, resolveDir: appRoot, loader: 'tsx'},
    bundle: true, platform: 'node', format: 'cjs', write: false, outdir: '/unused-file-preview-test', external: ['react', 'react-dom', 'react-dom/client', 'react/jsx-runtime'],
    plugins: [{name: 'native-boundaries', setup(b) {
      b.onResolve({filter: /(?:^|\/)appearance$/}, () => ({path: 'appearance', namespace: 'test'}));
      b.onResolve({filter: /^@pierre\/diffs\/react$/}, () => ({path: 'diffs', namespace: 'test'}));
      b.onResolve({filter: /octopusDiffThemes$/}, () => ({path: 'themes', namespace: 'test'}));
      b.onLoad({filter: /.*/, namespace: 'test'}, ({path: name}) => ({contents: name === 'appearance'
        ? 'export function useAppearance(){return {themeMode:"dark"}}; export function useShouldReduceMotion(){return true}'
        : name === 'themes' ? 'export function registerOctopusDiffThemes(){}; export function octopusDiffThemeName(){return "test"}'
        : 'import React from "react"; export function File({file}){return React.createElement("pre",null,file.contents)}', loader: 'js'}));
    }}], logLevel: 'silent',
  });
  const bundledCss = result.outputFiles.find(f => f.path.endsWith('.css')).text;
  assert.match(bundledCss, /\.we-file-preview-toolbar/);
  const style = w.document.createElement('style');
  style.textContent = fs.readFileSync(path.join(appRoot, 'src/components/worker-environment/filePreview.css'), 'utf8');
  w.document.head.append(style);
  const module = {exports: {}};
  Function('require', 'module', 'exports', result.outputFiles.find(f => f.path.endsWith('.js')).text)(require, module, module.exports);
  const root = createRoot(w.document.getElementById('root'));
  const calls = [];
  const text = '{"exact":true}';
  const identity = {workerEnvironmentId: 'room-a', surfaceId: 'files-a', path: 'state/ee_instances/example.json'};
  w.octopusTransport = {
    readWorkerEnvironmentFile: async request => {
      assert.deepEqual(request, identity);
      return {...identity, kind: 'text', text, size: text.length, modifiedAt: 'now', digest: 'a'.repeat(64), truncated: false};
    },
    performWorkerEnvironmentFileSystemAction: async request => {
      calls.push(request);
      return {...request, absolutePath: '/bounded/state/ee_instances/example.json'};
    },
  };
  const props = {active: true, environment: {id: 'room-a', revision: '1'}, path: identity.path, surface: {id: 'files-a', mode: 'direct', protocol: 'filesystem.readwrite', readOnly: false, root: '/bounded', data: {}}};
  try {
    await React.act(async () => root.render(React.createElement(module.exports.FilePreviewSurface, props)));
    const toolbar = w.document.querySelector('.we-file-preview-toolbar');
    assert.equal(w.getComputedStyle(toolbar).display, 'flex');
    assert.equal(w.getComputedStyle(w.document.querySelector('.we-file-preview-actions')).display, 'flex');
    assert.equal(w.getComputedStyle(w.document.querySelector('.we-file-preview-breadcrumb')).flexDirection, 'row-reverse');
    assert.equal(w.getComputedStyle(w.document.querySelector('.we-file-preview-content')).overflow, 'auto');
    assert.equal(w.document.querySelector('[aria-current="page"]').textContent, 'example.json');
    assert.equal(w.document.querySelector('pre').textContent, text);
    const actions = [...toolbar.querySelectorAll('button')];
    assert.deepEqual(actions.map(b => b.getAttribute('aria-label')), ['Edit file manually', 'Copy absolute path', 'Reveal in file manager', 'Open in default app']);
    for (const [label, action] of [['Reveal in file manager', 'reveal'], ['Open in default app', 'open']]) {
      await React.act(async () => actions.find(b => b.getAttribute('aria-label') === label).click());
      assert.deepEqual(calls.pop(), {...identity, action});
    }
    await React.act(async () => root.render(React.createElement(module.exports.FilePreviewSurface, {...props, surface: {...props.surface, protocol: 'filesystem.readonly', readOnly: true}})));
    assert.equal(w.document.querySelector('button[aria-label="Edit file manually"]'), null);
  } finally {
    await React.act(async () => root.unmount());
    delete w.octopusTransport;
    channels.forEach(channel => channel.close());
    globalThis.BroadcastChannel = NativeBroadcastChannel;
    dom.window.close();
  }
});
