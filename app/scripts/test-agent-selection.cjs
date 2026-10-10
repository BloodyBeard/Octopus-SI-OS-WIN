const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), vm = require('node:vm');
const { test } = require('node:test');
const ts = require('typescript'), React = require('react');
const { renderToStaticMarkup } = require('react-dom/server');
const app = path.resolve(__dirname, '..');
const ast = ts.createSourceFile('workspace.ts', fs.readFileSync(path.join(app, 'src/components/chat/useChatWorkspace.ts'), 'utf8'), ts.ScriptTarget.Latest, true);
const functions = new Map(); let resolvePending;
(function visit(node) {
  if (ts.isFunctionDeclaration(node)) functions.set(node.name?.text, node.getText(ast));
  if (ts.isCallExpression(node) && node.expression.getText(ast) === 'useEffect'
    && node.arguments[0]?.getText(ast).includes('const target = pendingNewTaskTargetRef.current;')) resolvePending = node.arguments[0].getText(ast);
  ts.forEachChild(node, visit);
})(ast);
const compile = source => ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.CommonJS } }).outputText;
test('An existing task continues through its exact provider without a saved Agent template', () => {
  const module = { exports: {} };
  Function('module', 'exports', 'require', compile(fs.readFileSync(path.join(app, 'src/components/chat/chatWorkspaceCapabilities.ts'), 'utf8')))(module, module.exports, () => ({}));
  const { providerForExistingRun, providerInExecutionContext } = module.exports;
  const provider = { id: 'provider' }, selection = { runId: 'task', eeId: 'project', workerInterfaceId: 'custom-template' };
  const run = { ...selection, workerInterfaceId: 'concrete-worker', workerChoiceInterfaceId: 'custom-template', providerInterfaceId: 'provider', providerBoundInterfaceId: 'private-provider', writable: true, archived: false };
  assert.equal(providerInExecutionContext([], 'project', 'custom-template', provider), null);
  assert.equal(providerForExistingRun(provider, run, selection), provider);
  for (const patch of [{ providerInterfaceId: 'other' }, { providerBoundInterfaceId: '' }, { eeId: 'other' }, { runId: 'other' }, { workerChoiceInterfaceId: 'other' }, { writable: false }, { archived: true }]) assert.equal(providerForExistingRun(provider, { ...run, ...patch }, selection), null);
  assert.equal(providerForExistingRun(provider, undefined, selection), null);
});
const code = compile(['chooseAgentPreset', 'createChat', 'clientAgentSelectionFailure', 'projectAgents'].map(name => functions.get(name)).join('\n') + `\nfunction resolvePendingSelection() { (${resolvePending})(); }`);
function selection() {
  const state = {
    activeRunId: '', activeRunIdRef: { current: '' }, pendingNewTaskTargetRef: { current: null }, capabilitySelectionOwnerRef: { current: '' },
    executionContextsLoaded: true, providerInterfacesLoaded: false,
    agentPresets: ['first', 'second'].map(id => ({ id, name: id, projectEeId: 'ee', workerInterfaceId: id, providerInterfaceId: 'provider', reasoningEffort: '', applicationIds: [], driverNames: [] })),
    executionContexts: [{ eeId: 'ee', workers: ['first', 'second'].map(id => ({ id, providerInterfaceIds: ['provider'] })) }],
    providerInterfaces: [{ id: 'provider', models: [{ model: 'not-admitted' }, { model: 'admitted' }] }],
    providerForExecutionContext: provider => ({ ...provider, models: provider.models.filter(model => model.model === 'admitted') }),
    providerDefaultModel: provider => provider.models[0], providerReasoningEfforts: () => [], executionContextTaskCapabilities: () => ({}),
    defaultCapabilitySelection: () => ({ applicationIds: [], driverNames: [] }),
    runsLoaded: false, runs: [], accessProfile: 'live', accessMode: 'project_access', eeId: 'ee', error: '',
    saveCurrentDraft() {}, deleteDraft() {}, resetInputHistory() {}, applyDraft: draft => Object.assign(state, draft),
  };
  for (const field of ['agentPresetId', 'eeId', 'workerInterfaceId', 'providerInterfaceId', 'providerModel', 'reasoningEffort', 'applicationIds', 'driverNames', 'accessMode', 'error', 'newTaskTargetResolutionBlocked', 'activeRunId', 'messages', 'loadingMessages', 'messageQuery']) state[`set${field[0].toUpperCase()}${field.slice(1)}`] = value => { state[field] = value; };
  vm.createContext(state); vm.runInContext(code, state); return state;
}

test('Agent choice waits for catalogs, retains explicit choice, and validates admitted models', () => {
  const state = selection(); state.createChat('ee', 'second');
  assert.equal(state.error, ''); assert.equal(state.workerInterfaceId, ''); assert.equal(state.pendingNewTaskTargetRef.current.agentPresetId, 'second');
  state.resolvePendingSelection(); assert.equal(state.workerInterfaceId, '');
  state.providerInterfacesLoaded = true; state.resolvePendingSelection();
  assert.equal(state.agentPresetId, 'second'); assert.equal(state.workerInterfaceId, 'second'); assert.equal(state.providerModel, 'admitted');
  assert.equal(state.error, ''); assert.equal(state.pendingNewTaskTargetRef.current, null);
  state.executionContextsLoaded = false; state.chooseAgentPreset('first');
  assert.equal(state.error, ''); assert.equal(state.workerInterfaceId, '');
  state.executionContextsLoaded = true; state.resolvePendingSelection(); assert.equal(state.workerInterfaceId, 'first');
  state.providerInterfaces = []; state.chooseAgentPreset('second');
  assert.match(state.error, /model interface 'provider' is unavailable/); assert.equal(state.workerInterfaceId, '');
  const early = selection(), agents = early.agentPresets;
  early.executionContextsLoaded = false; early.agentPresets = []; early.createChat('', 'second');
  assert.equal(early.error, ''); assert.equal(early.pendingNewTaskTargetRef.current.agentPresetId, 'second');
  early.agentPresets = agents; early.executionContextsLoaded = true; early.providerInterfacesLoaded = true; early.resolvePendingSelection();
  assert.equal(early.workerInterfaceId, 'second');
  early.providerInterfacesLoaded = false; early.chooseAgentPreset('first'); early.chooseAgentPreset('');
  assert.equal(early.pendingNewTaskTargetRef.current, null); assert.equal(early.agentPresetId, '');
});

test('Composer exposes models only for a selected Agent or an existing run', () => {
  const module = { exports: {} }, noop = () => null, primitives = new Proxy({}, { get: () => noop });
  const mocks = {
    './ProviderModelPicker': { ProviderModelPicker: () => React.createElement('button', { 'data-model-picker': true }, 'Model') },
    './useLocalDictation': { useLocalDictation: () => ({}) }, '../../clientPreferences': { useClientPreferences: () => ({}) },
    '../../clientActions': { clientCommands: [], subscribeClientCommands: noop },
  };
  Function('module', 'exports', 'require', compile(fs.readFileSync(path.join(app, 'src/components/chat/ChatComposer.tsx'), 'utf8')))(module, module.exports, id => id === 'react' || id === 'react/jsx-runtime' ? require(id) : mocks[id] ?? primitives);
  const props = { text: '', attachments: [], providerInterfaces: [], providerInterfacesLoaded: false, fileInputRef: { current: null }, onTextChange: noop, onSubmit: noop };
  const rendersPicker = patch => renderToStaticMarkup(React.createElement(module.exports.ChatComposer, { ...props, ...patch })).includes('data-model-picker');
  assert.equal(rendersPicker({}), false); assert.equal(rendersPicker({ workerInterfaceId: 'stale-worker' }), false);
  assert.equal(rendersPicker({ agentPresetId: 'pending-agent' }), false);
  assert.equal(rendersPicker({ agentPresetId: 'agent', workerInterfaceId: 'worker' }), true);
  assert.equal(rendersPicker({ runId: 'existing-run' }), true); assert.equal(rendersPicker({ variant: 'home', runId: 'home' }), false);
});
