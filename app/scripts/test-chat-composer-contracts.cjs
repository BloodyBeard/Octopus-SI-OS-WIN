const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const appRoot = path.resolve(__dirname, "..");

function source(relativePath) {
  return require("./splitSource.cjs").readSource(path.join(appRoot, relativePath));
}

function sourceBetween(value, startMarker, endMarker) {
  const start = value.indexOf(startMarker);
  assert.notEqual(start, -1, `missing source marker: ${startMarker}`);
  const end = value.indexOf(endMarker, start + startMarker.length);
  assert.notEqual(end, -1, `missing source marker: ${endMarker}`);
  return value.slice(start, end);
}

function composerInvocation(relativePath) {
  const value = source(relativePath);
  const start = value.indexOf("<ChatComposer");
  assert.notEqual(start, -1, `${relativePath} must render ChatComposer`);
  const end = value.indexOf("/>", start);
  assert.notEqual(end, -1, `${relativePath} must close its ChatComposer invocation`);
  return value.slice(start, end + 2);
}

function assertWorkspaceContract(relativePath, owner) {
  const invocation = composerInvocation(relativePath);
  assert.match(
    invocation,
    new RegExp(`\\bisSubmitting=\\{${owner}\\.submitting\\}`),
    `${relativePath} must pass the workspace submit-pending state`,
  );
  assert.match(
    invocation,
    new RegExp(`\\bsubmitBlockReason=\\{${owner}\\.submitBlockReason\\}`),
    `${relativePath} must pass the workspace submit block reason`,
  );
  assert.match(
    invocation,
    new RegExp(`\\bonResume=\\{${owner}\\.resumeActiveTurn\\}`),
    `${relativePath} must pass the exact-run Resume action`,
  );
  assert.match(
    invocation,
    new RegExp(`\\bresumeAvailable=\\{${owner}\\.resumeAvailable\\}`),
    `${relativePath} must pass the projected Resume state`,
  );
}

assertWorkspaceContract("src/components/ChatView.tsx", "chat");
assertWorkspaceContract("src/components/QuickTaskWindow.tsx", "chat");
assertWorkspaceContract("src/components/voice/VoiceView.tsx", "workspace");

const composer = source("src/components/chat/ChatComposer.tsx");
const modelPicker = source("src/components/chat/ProviderModelPicker.tsx");
const capabilityCatalog = source("src/components/chat/useChatCapabilityCatalog.ts");
const utilityBar = source("src/components/chat/ComposerUtilityBar.tsx");
const projectionClient = source("src/projectionClient.ts");
const agentDialog = source("src/components/agent/AgentPresetDialog.tsx");
const productError = source("src/components/chat/chatProductError.ts");
const sidebar = source("src/components/Sidebar.tsx");
const taskSummary = source("src/components/chat/TaskSummaryPanel.tsx");
const subagentActivity = source("src/components/chat/subagentActivity.ts");
const turnActivity = source("src/components/chat/TurnActivity.tsx");
const chatView = source("src/components/ChatView.tsx");
const messageList = source("src/components/chat/ChatMessageList.tsx");
const foundation = source("src/foundation.css");
const ownedUtilities = source("src/owned-utilities.css");
const queueStyles = source("src/components/chat/queue.css");
assert.match(composer, /className="composer-primary-action /);
assert.match(queueStyles, /\.composer-primary-action \{\s*background-color: var\(--color-background-composer-primary\);\s*color: var\(--color-text-composer-primary\);/);
assert.match(queueStyles, /\.composer-primary-action:focus-visible \{ outline-color: var\(--color-background-composer-primary\);/);
assert.match(queueStyles, /\.composer-queue-action \{[^}]*border-radius: 9999px;/);
assert.match(queueStyles, /\.composer-queue-action\.is-icon \{[^}]*border-radius: var\(--radius-lg\);/);
assert.match(queueStyles, /\.oc-menu-item\.composer-queue-menu-item \{ color: var\(--color-text-description\);/);
assert.doesNotMatch(source("src/clientPreferences.ts") + source("src/components/settings/GeneralSettingsSection.tsx") + source("src/components/voice/VoiceView.tsx"), /activeRunSubmitBehavior|ActiveRunSubmitBehavior/);
assert.match(source("src/components/settings/GeneralSettingsSection.tsx"), /Send steers the current reply immediately/);
assert.match(source("src/components/settings/ShortcutSettingsSection.tsx"), /preferences.sendShortcut === "enter" \? "Mod\+Enter" : "Mod\+Shift\+Enter"/);
assert.match(modelPicker, /providerModels\(provider\)\.map\(\(model\) => \(\{ provider, model \}\)\)/);
assert.match(modelPicker, /interfaceLocked \? activeProvider \? \[activeProvider\] : \[\] : providers/);
assert.doesNotMatch(modelPicker, /codex-provider-submenu|label="Interface"|label="Effort"/);
assert.match(modelPicker, /providerModelPicker\.css/);
assert.doesNotMatch(modelPicker, /model-picker\/source|source\/index\.js/);
assert.doesNotMatch(modelPicker, /uniqueModelProviders|function modelValue\(provider/);
assert.match(modelPicker, /const disabled = options\.length === 0/);
assert.match(modelPicker, /name \+ " · " \+ \(sameLabel \? option\.provider\.id : option\.provider\.label \|\| option\.provider\.id\)/);
assert.doesNotMatch(modelPicker, /providerModel[^\n]*\.split\s*\(/);
assert.match(capabilityCatalog, /if \(!enabled\) \{[\s\S]*?setProviderInterfaces\(\[\]\);[\s\S]*?setProviderInterfacesLoaded\(false\);/);
assert.match(projectionClient, /readRuntimeProviderInterfaces\([\s\S]*?if \(bridge\?\.providerInterfaces\) \{\s*return bridge\.providerInterfaces\(\);/);
assert.doesNotMatch(projectionClient, /readRuntimeProviderInterfaces\([\s\S]*?bridge\.bootStatus\(\)/);
assert.match(projectionClient, /export async function refreshProviderModels\([\s\S]*?host\.refreshProviderModels\(request\)/);
assert.doesNotMatch(modelPicker, /world_provider_codex|world_provider_openrouter|z-ai\//);
assert.match(agentDialog, /providerModels\(formed\)\.map/);
assert.match(agentDialog, /setProviderModel\(preset\.providerModel\)/);
assert.match(utilityBar, /--color-token-composer-utility-background/);
// A parent !important hover used to paint a rectangle inside the Agent pill.
assert.doesNotMatch(utilityBar, /\[&_button:enabled:hover\]/);
assert.doesNotMatch(utilityBar, /Agent types?|onWorkerChange/);
assert.doesNotMatch(utilityBar, /Custom Agent configuration|value=\{[^\n]*"Custom Agent"/);
assert.match(utilityBar, />Agents</);
assert.doesNotMatch(utilityBar, /Built-in Agents|Your Agents|workerLabel/);
const draftAgentCapabilities = sourceBetween(utilityBar, "const workerApplications", "const attachedRooms");
assert.match(utilityBar, /const applications = workerApplications\.filter\(\(application\) => selectedAgent[\s\S]*?selectedAgent\.applicationIds\.includes\(application\.id\)[\s\S]*?applicationIds\.includes\(application\.id\)/);
assert.match(utilityBar, /const drivers = workerDrivers\.filter\(\(driver\) => selectedAgent[\s\S]*?selectedAgent\.driverNames\.includes\(driver\.name\)[\s\S]*?attachedRooms\.some/);
assert.match(utilityBar, /disabled=\{locked \|\| Boolean\(selectedAgent\) \|\| !contexts\.length\}/);
assert.doesNotMatch(agentDialog, /Agent types?/i);
assert.doesNotMatch(agentDialog, /label="Role"|Built-in defaults|client projection/);
assert.match(agentDialog, /disabled=\{busy \|\| readOnly \|\| Boolean\(preset\)\}/);
assert.match(sidebar, /onClick=\{\(\) => setAgentDialog\(\{ mode: "edit", presetId: preset\.id \}\)\}/);
assert.doesNotMatch(sidebar, /mode: "view"|preset\.origin/);
assert.doesNotMatch(sidebar, /onNewChat\(preset\.id\)/);
assert.match(sidebar, /projectById\.has\(preset\.projectEeId\)/);
assert.match(sidebar, /meta=\{agentTaskMeta\(preset, taskChats\)\}/);
assert.doesNotMatch(sidebar, /function agentPresetMeta/);
assert.match(sidebar, /\? \[run\.runId\]/);
assert.match(agentDialog, /worker\.authoringProfile && worker\.agentSourceTemplate/);
assert.match(agentDialog, /sourceWorkers\.length === 1 \? sourceWorkers\[0\] : null/);
assert.doesNotMatch(sourceBetween(agentDialog, "function defaultsForContext(", "function defaultsForWorker("), /authoringWorkers\[0\]/);
const agentAccessControl = sourceBetween(
  utilityBar,
  '<div className="composer-agent-access-control',
  "\n\n        <UtilityBarDropdown",
);
assert.match(agentAccessControl, /navigationTarget="workspace-agent"[\s\S]*?indicator="none"|indicator="none"[\s\S]*?navigationTarget="workspace-agent"/);
assert.doesNotMatch(agentAccessControl, /triggerVariant="embedded"/);
const permissionToggle = sourceBetween(
  agentAccessControl,
  "aria-label={accessMode === \"full_access\" ? \"Use project access\" : \"Use full access\"}",
  "</button>",
);
assert.match(permissionToggle, /aria-pressed=\{accessMode === "full_access"\}/);
assert.match(permissionToggle, /onAccessModeChange\(accessMode === "full_access" \? "project_access" : "full_access"\)/);
assert.match(permissionToggle, /composer-agent-access-toggle/);
assert.match(permissionToggle, /CHIP_BUTTON_CLASS.*shrink-0/);
assert.match(permissionToggle, /<span>\{accessMode === "full_access" \? "Full access" : "Project access"\}<\/span>/);
assert.match(permissionToggle, /disabled=\{locked \|\| !selectedWorker \|\| accessMode !== "full_access" && Boolean\(fullAccessUnavailableReason\)\}/);
assert.match(permissionToggle, /aria-description=\{accessMode === "full_access" \? undefined : fullAccessUnavailableReason \|\| undefined\}/);
assert.match(permissionToggle, /<TaskAccessIcon accessMode=\{accessMode\} className="icon-xs shrink-0" \/>/);
assert.doesNotMatch(permissionToggle, /UtilityBarDropdown|UtilityBarItem|aria-haspopup|aria-expanded/);
assert.doesNotMatch(permissionToggle, /title=|name="lock"/);
assert.match(agentAccessControl, /label=\{\([\s\S]*?<TaskAccessIcon[\s\S]*?agent\.id === agentPresetId \? accessMode : defaultAccessMode/);
assert.match(utilityBar, /function TaskAccessIcon[\s\S]*?accessMode === "full_access" \? " text-token-warning" : " text-token-text-tertiary"/);
assert.match(utilityBar, /function TaskAccessIcon[\s\S]*?\? <FullAccessIcon aria-hidden className=\{resolvedClassName\} \/>[\s\S]*?: <ProjectAccessIcon aria-hidden className=\{resolvedClassName\} \/>/);
assert.match(ownedUtilities, /--color-token-warning: var\(--color-text-warning\);/);
assert.match(productError, /\(chat\.activeRunId \|\| chat\.agentPresetId\)\s*&& chat\.providerInterfacesLoaded\s*&& Boolean\(chat\.providerInterfaceId\)\s*&& !chat\.declaredProviderInterfaces\.some\(\(provider\) => provider\.id === chat\.providerInterfaceId\)/);
assert.doesNotMatch(productError, /chat\.providerInterfaceId \|\| "provider"/);
assert.doesNotMatch(sidebar, /deleteClientTaskAgentAssignment|createClientAgentPreset|updateClientAgentPreset|deleteClientAgentPreset/);
assert.doesNotMatch(taskSummary, /Execution environment|Worker binding|Provider|<SummaryFact|title="Checkpoints"/);
const environmentSummary = source("src/components/worker-environment/TaskEnvironmentSummary.tsx");
assert.match(environmentSummary, /const panel = workerEnvironmentPanelKind\(surface\);/);
assert.match(environmentSummary, /onOpen\(environment\.id, panel, surface\.id\)/);
assert.doesNotMatch(taskSummary, /Commit or push|Create pull request|onPrepareInstruction/);
assert.doesNotMatch(environmentSummary, /octopusTransport|inspectWorkerEnvironmentGit/);
assert.match(taskSummary, /environments=\{workerEnvironments\}/);
assert.doesNotMatch(taskSummary, /slice\(0, 12\)/);
assert.doesNotMatch(taskSummary, /meta: `\$\{status\} · \$\{(?:identity|processId)\}`/);
assert.match(turnActivity, /normalizeMultiAgentActivity\(entry\.messages\.at\(-1\)\)/);
assert.match(turnActivity, /normalizeSubagentActivity\(entry\.messages\.at\(-1\)\)/);
assert.match(chatView, /onOpenEnvironment=\{openEnvironmentCapability\}/);
assert.match(chatView, /dispatchOpenWorkerEnvironmentPane\(panelKind \|\| surfaceId \|\| clientSurface/);
assert.doesNotMatch(chatView, /TaskContextPopover|RunEnvironmentView|onPrepareInstruction/);
assert.match(messageList, /function reverseScrollDistance\([\s\S]*?Math\.max\(0, -element\.scrollTop\)/);
assert.doesNotMatch(messageList, /Math\.abs\([^\n]*scrollTop/);
assert.match(messageList, /className="flex min-h-full shrink-0 flex-col justify-start overflow-x-clip"/);
assert.match(messageList, /aria-hidden[\s\S]{0,120}thread-scroll-padding-bottom/);
assert.match(messageList, /pointer-events-none absolute inset-x-0 bottom-0 z-10 pb-4/);
assert.match(messageList, /className=\{`octopus-chat-scroll thread-scroll-container h-full/);
assert.doesNotMatch(messageList + sourceBetween(source("src/components/chat/chat.css"), ".octopus-chat-scroll {", "\n}"), /thread-scroll-container relative h-full|position:\s*relative/);
assert.doesNotMatch(messageList, /className="sticky bottom-0 z-10 mt-auto w-full pb-4" data-thread-scroll-footer="true" ref=\{footerRef\}/);
assert.doesNotMatch(foundation, /\*\s*\{\s*scroll-behavior:\s*smooth;/);

const commandMenu = source("src/components/CommandMenu.tsx");
const titleSearchCode = [
  commandMenu.match(/^const groupOrder = .*$/m)[0],
  sourceBetween(commandMenu, "  const visibleChatRuns = useMemo(", "\n\n  useEffect("),
  sourceBetween(commandMenu, "  const entries = useMemo(", "  const grouped = useMemo("),
  commandMenu.slice(commandMenu.indexOf("function matchScore(")),
  "return filtered;",
].join("\n");
const titleSearchDependencies = {
  useMemo: (compute) => compute(), clientCommands: [], surfaces: [], keyboardShortcuts: {},
  accessProfile: "live", activeSurfaceId: "chat", voiceActive: false,
  loadOlderMessageMatches() {}, onOpenRun() {}, onSelectSurface() {},
  messageSearch: { matches: [], hasOlder: false, nextBeforeSeq: -1, taskCount: 0 },
  clientTaskTitle: (id, title, organization) => organization.taskTitles[id] ?? title,
};
const titleSearch = Function(...Object.keys(titleSearchDependencies), "chatRuns", "organization", "query", require("typescript").transpileModule(titleSearchCode, {
  compilerOptions: { target: require("typescript").ScriptTarget.ES2020 },
}).outputText).bind(null, ...Object.values(titleSearchDependencies));
const titleRuns = Array.from({ length: 30 }, (_, index) => ({ runId: `task-${index}`, title: `Task ${index}`, updatedAtMs: 1000 - index, messageCount: 0 }));
titleRuns.push({ runId: "archived", title: "Quasar Zebra Unique", updatedAtMs: 2000, messageCount: 0, archived: true });
const titleOrganization = { recentRunIds: ["task-7", "task-3"], recentDestinations: [], taskTitles: { "task-29": "Quasar Zebra Unique" } };
const recentTitles = titleSearch(titleRuns, titleOrganization, "").map((entry) => entry.id);
assert.deepEqual(recentTitles, [7, 3, 0, 1, 2, 4, 5, 6, 8, 9, 10, 11].map((index) => `run:task-${index}`));
assert.deepEqual(titleSearch(titleRuns, titleOrganization, "   ").map((entry) => entry.id), recentTitles);
assert.deepEqual(titleSearch(titleRuns, titleOrganization, "  QUASAR   ZEBRA ").map((entry) => entry.id), ["run:task-29"]);
assert.equal(titleSearch(titleRuns, titleOrganization, "Task").length, 12);

async function verifyTemplateFiles() {
  const ts = require("typescript");
  const files = [{ id: `architecture:${"a".repeat(64)}`, content: Array(32).fill(170) }];
  const worker = { id: "worker", payload_artifact_bindings: files };
  const projected = { contexts: [{ ee_id: "ee", workers: [worker] }] };
  function load(relativePath) {
    const module = { exports: {} };
    const code = ts.transpileModule(source(relativePath), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    Function("module", "exports", "require", code)(module, module.exports, (specifier) => (
      specifier === "./projectionClient" ? { readProjection: async () => projected }
        : load(path.join(path.dirname(relativePath), `${specifier}.ts`))
    ));
    return module.exports;
  }
  const { loadChatExecutionContexts } = load("src/chatRuns.ts");
  const { executionContextTaskCapabilities } = load("src/components/chat/chatWorkspaceCapabilities.ts");
  const contexts = await loadChatExecutionContexts();
  const selected = executionContextTaskCapabilities(contexts, "ee", "worker", [], []);
  assert.deepEqual(selected.payloadArtifactBindings, files);
  selected.payloadArtifactBindings[0].content[0] = 0;
  assert.equal(files[0].content[0], 170);
  assert.equal(contexts[0].workers[0].payloadArtifactBindings[0].content[0], 170);
  worker.payload_artifact_bindings = [{ ...files[0], content: [170] }];
  await assert.rejects(loadChatExecutionContexts(), /invalid file reference/);
}

verifyTemplateFiles().then(() => {
  process.stdout.write("chat composer, task-title search and template file binding regressions passed\n");
}).catch((error) => { console.error(error); process.exitCode = 1; });
