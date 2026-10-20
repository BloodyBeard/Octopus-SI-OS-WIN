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
const workspace = source("src/components/chat/useChatWorkspace.ts");
const projectionClient = source("src/projectionClient.ts");
const operatorChatClient = source("../registry/interface/world_operator_chat/client.cjs");
const agentDialog = source("src/components/agent/AgentPresetDialog.tsx");
const agentCatalog = source("src/agentPresets.ts");
const capabilitySelection = source("src/components/chat/chatWorkspaceCapabilities.ts");
const productError = source("src/components/chat/chatProductError.ts");
const sidebar = source("src/components/Sidebar.tsx");
const taskSummary = source("src/components/chat/TaskSummaryPanel.tsx");
const subagentActivity = source("src/components/chat/subagentActivity.ts");
const turnActivity = source("src/components/chat/TurnActivity.tsx");
const chatView = source("src/components/ChatView.tsx");
const messageList = source("src/components/chat/ChatMessageList.tsx");
const foundation = source("src/foundation.css");
const ownedUtilities = source("src/owned-utilities.css");
assert.match(composer, /const resumeActionAvailable = !stopAvailable[\s\S]*?&& !hasSubmissionPayload[\s\S]*?&& resumeAvailable[\s\S]*?&& Boolean\(onResume\);/);
assert.match(composer, /const resumeActionBlocked = resumeActionAvailable[\s\S]*?submitBlockReason !== "Type a message or attach a file\.";/);
assert.match(composer, /const submitAccessibleName = editingQueuedTurn \? "Save queued message" : stopAvailable \? "Stop" : resumeActionAvailable \? "Resume" : "Send";/);
assert.match(composer, /const submitPending = stopAvailable \|\| resumeActionAvailable[\s\S]*?Boolean\(interruptPending\)/);
assert.match(composer, /const submitDisabled = stopAvailable \|\| resumeActionAvailable[\s\S]*?Boolean\(interruptPending \|\| resumeActionBlocked\)/);
assert.match(composer, /onClick=\{\(\) => \{\s*if \(submitDisabled\) return;\s*stopAvailable \? onInterrupt\?\.\(\) : resumeActionAvailable \? onResume\?\.\(\) : onSubmit\(\);\s*\}\}/);
assert.match(composer, /name=\{stopAvailable \? "stop" : resumeActionAvailable \? "resume"/);
assert.match(composer, /aria-label=\{submitAccessibleName\}/);
assert.match(composer, /<ShellTooltip label=\{submitTooltip\}/);
assert.doesNotMatch(composer, /title=\{submitTooltip\}/);
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
const projectedModels = sourceBetween(capabilitySelection, "export function providerModels(", "export function providerDefaultModel(");
assert.doesNotMatch(projectedModels, /model:\s*provider\.model|displayName:\s*provider\.model/);
const selectedModel = sourceBetween(capabilitySelection, "export function providerSelectedModel(", "export function providerReasoningEfforts(");
assert.match(selectedModel, /if \(!requested\) return providerDefaultModel\(provider\);/);
assert.match(selectedModel, /find\(\(candidate\) => candidate\.model === requested\) \?\? null/);
assert.match(projectionClient, /export async function refreshProviderModels\([\s\S]*?host\.refreshProviderModels\(request\)/);
assert.match(operatorChatClient, /model\.supportedReasoningEfforts[\s\S]*?effort\.reasoningEffort/);
assert.doesNotMatch(operatorChatClient, /supportedReasoningEfforts[\s\S]{0,500}effort\.effort/);
assert.match(operatorChatClient, /selectedProviderDescriptorContent\([\s\S]*?topologyPlan\.model/);
assert.match(operatorChatClient, /selectedProviderDescriptor\.interface_policy = requiredObject/);
assert.match(operatorChatClient, /selectedProviderPrivateConfig\.model_catalog = requiredArray/);
assert.match(operatorChatClient, /return `\$\{JSON\.stringify\(selectedProviderDescriptor, null, 2\)\}\\n`/);
assert.doesNotMatch(operatorChatClient, /provider selection requires a coupled topology amendment/);
const modelRefresh = sourceBetween(workspace, "  const refreshProviderModels = useCallback(", "  const executionContextReady");
assert.doesNotMatch(modelRefresh, /runtimeReady|turnOpen|writable|runId|interfaceId:/);
assert.match(modelRefresh, /accessProfile === "live" && enabled/);
assert.match(modelRefresh, /await refreshProviderInterfaces\(\)/);
assert.doesNotMatch(workspace, /providerModelCatalogs|setProviderModelCatalogs/);
assert.doesNotMatch(modelPicker, /world_provider_codex|world_provider_openrouter|z-ai\//);
assert.match(agentDialog, /providerModels\(formed\)\.map/);
assert.match(agentDialog, /setProviderModel\(preset\.providerModel\)/);
assert.match(workspace, /setProviderModel\(preset\.providerModel\)/);
assert.match(workspace, /function chooseProviderModel\(nextProviderInterfaceId: string, nextProviderModel: string, requestedEffort\?: string\)/);
assert.doesNotMatch(workspace, /setProviderModel\(\(current\) => current\.trim\(\) \|\| providerDefaultModel\(activeProvider\)/);
assert.doesNotMatch(workspace, /providerModels\(activeProvider\)\.find\([\s\S]{0,160}\?\? providerDefaultModel\(activeProvider\)/);
assert.match(workspace, /activeRunIdRef\.current && nextProviderInterfaceId !== resolvedProviderInterfaceId/);
assert.match(workspace, /providerModel: claimed\.providerModel/);
assert.doesNotMatch(workspace, /predates exact model persistence/);
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
const explicitAgentProjection = sourceBetween(
  agentCatalog,
  "export function resolveClientAgents(",
  "export function visibleClientAgents(",
);
assert.doesNotMatch(explicitAgentProjection, /worker\.label|builtInAgent|source_names/);
assert.match(explicitAgentProjection, /if \(!worker\.clientAgentId\) continue;/);
assert.match(explicitAgentProjection, /id: worker\.clientAgentId/);
assert.match(explicitAgentProjection, /name: worker\.clientAgentName/);
assert.match(explicitAgentProjection, /agentCatalogDigest: context\.agentCatalogDigest/);
assert.match(explicitAgentProjection, /worker\.clientTemplateProviderInterfaceId/);
assert.match(explicitAgentProjection, /worker\.clientTemplateApplicationIds/);
assert.match(explicitAgentProjection, /worker\.clientTemplateWorkerEnvironmentKinds/);
assert.match(agentCatalog, /agent\.deletedAtMs === 0 && Boolean\(agent\.clientTemplateDigest\)/);
assert.doesNotMatch(agentCatalog, /localStorage|useSyncExternalStore|reconcileClientAgentPresets|agentPresets\.v[12]/);
assert.match(agentDialog, /worker\.authoringProfile && worker\.agentSourceTemplate/);
assert.match(agentDialog, /sourceWorkers\.length === 1 \? sourceWorkers\[0\] : null/);
assert.doesNotMatch(sourceBetween(agentDialog, "function defaultsForContext(", "function defaultsForWorker("), /authoringWorkers\[0\]/);
assert.match(workspace, /migrateDraft\("", submitted\.runId\);/);
assert.match(workspace, /sameStringSelection\(agent\.driverNames, driverNames\)/);
assert.match(workspace, /if \(!selectedClientAgent\?\.applicationIds\.includes\(applicationId\)\) return;/);
assert.match(workspace, /!selectedClientAgent\?\.driverNames\.includes\(driverName\)/);
assert.match(workspace, /&& !selectedClientAgentCapabilityFailure/);
assert.match(sourceBetween(workspace, "function chooseAgentPreset(", "function toggleApplication("), /setEeId\(preset\.projectEeId\)/);
assert.match(capabilitySelection, /Application '\$\{application\?\.name \|\| applicationId\}' requires Worker Environment '\$\{missingKind\}'/);
assert.match(capabilitySelection, /worker\.selectedTools[\s\S]*?\.filter\(\(selection\) => selectedDriverNames\.has\(selection\.workerEnvironmentKind\)\)/);
assert.match(workspace, /return taskCapabilitySelectionFailure\(worker, applicationIds, driverNames, accessMode\);/);
assert.match(capabilitySelection, /accessMode === "full_access"[\s\S]*?HOST_FILESYSTEM_ROOT_SLOT/);
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
const agentReadiness = sourceBetween(
  workspace,
  "const clientAgentSelectionReady = Boolean(",
  ");\n  const fullAccessUnavailableReason",
);
assert.match(agentReadiness, /selectedClientAgent\.projectEeId === eeId/);
assert.match(agentReadiness, /selectedClientAgent\.workerInterfaceId === workerInterfaceId/);
assert.doesNotMatch(agentReadiness, /providerInterfaceId|reasoningEffort|applicationIds|driverNames/);
assert.match(workspace, /&& clientAgentSelectionReady/);
assert.match(workspace, /const admissibleAgents = agentPresets\.filter\(\(agent\) => !clientAgentSelectionFailure\(agent\)\);/);
assert.match(workspace, /if \(matchingAgent\) \{[\s\S]*?setAgentPresetId\(matchingAgent\.id\);[\s\S]*?setNewTaskTargetResolutionBlocked\(false\);/);
assert.doesNotMatch(workspace, /This draft does not identify one current Agent|The selected project exposes no complete Agent/);
assert.match(sourceBetween(workspace, "async function submit(", "async function resolvePausedQueueSubmission("), /if \(!activeRunIdRef\.current && !executionContextReady\) \{\s*return;\s*\}/);
assert.doesNotMatch(workspace, /if \(selectedExecutionContext\.workers\.some\(\(worker\) => worker\.id === workerInterfaceId\)\) return;/);
assert.match(workspace, /if \(workerInterfaceId\) setWorkerInterfaceId\(""\);/);
const exactDraftAgent = sourceBetween(workspace, "function exactClientAgentConfiguration(", "function clientAgentForRun(");
assert.match(exactDraftAgent, /return matches\.length === 1 \? matches\[0\] : null;/);
assert.doesNotMatch(exactDraftAgent, /agent\.origin|built_in/);
assert.doesNotMatch(exactDraftAgent, /defaultForProject/);
const runAgentIdentity = sourceBetween(workspace, "function clientAgentForRun(", "function sameStringSelection(");
assert.match(runAgentIdentity, /agent\.projectEeId === run\.eeId/);
assert.match(runAgentIdentity, /agent\.workerInterfaceId === workerInterfaceId/);
assert.match(runAgentIdentity, /return projectedCandidates\.length === 1 \? projectedCandidates\[0\] : null;/);
assert.doesNotMatch(runAgentIdentity, /run\.providerInterfaceId|connectedApplicationIds|sameStringSelection/);
assert.match(workspace, /preferred: provider\.id === preferredProviderInterfaceId/);
assert.doesNotMatch(workspace, /providerInterfacesLoaded:\s*providerSelectionReady/);
assert.match(productError, /\(chat\.activeRunId \|\| chat\.agentPresetId\)\s*&& chat\.providerInterfacesLoaded\s*&& Boolean\(chat\.providerInterfaceId\)\s*&& !chat\.declaredProviderInterfaces\.some\(\(provider\) => provider\.id === chat\.providerInterfaceId\)/);
assert.doesNotMatch(productError, /chat\.providerInterfaceId \|\| "provider"/);
assert.doesNotMatch(workspace, /TaskAgentAssignment|taskAgentAssignments|readClientTaskAgentAssignment|stageClientTaskAgentAssignment|copyClientTaskAgentAssignment/);
assert.match(workspace, /activeTaskAgent\?\.applicationIds\.includes\(applicationId\)/);
assert.doesNotMatch(sidebar, /deleteClientTaskAgentAssignment|createClientAgentPreset|updateClientAgentPreset|deleteClientAgentPreset/);
const runtimeSubmissionStart = workspace.indexOf("submitOperatorTurn({");
const runtimeSubmissionEnd = workspace.indexOf("          });", runtimeSubmissionStart);
assert.notEqual(runtimeSubmissionStart, -1);
assert.notEqual(runtimeSubmissionEnd, -1);
const runtimeSubmission = workspace.slice(runtimeSubmissionStart, runtimeSubmissionEnd);
assert.doesNotMatch(runtimeSubmission, /agentPresetId|ClientAgent/);
assert.match(runtimeSubmission, /providerModel: turn\.providerModel/);
assert.doesNotMatch(taskSummary, /Execution environment|Worker binding|Provider|<SummaryFact|title="Checkpoints"/);
const environmentSummary = source("src/components/worker-environment/TaskEnvironmentSummary.tsx");
assert.match(environmentSummary, /const panel = workerEnvironmentPanelKind\(surface\);/);
assert.match(environmentSummary, /onOpen\(environment\.id, panel, surface\.id\)/);
assert.doesNotMatch(taskSummary, /Commit or push|Create pull request|onPrepareInstruction/);
assert.doesNotMatch(environmentSummary, /octopusTransport|inspectWorkerEnvironmentGit/);
assert.match(taskSummary, /<SubagentSummary items=\{subagents\} \/>/);
assert.match(taskSummary, /environments=\{workerEnvironments\}/);
assert.doesNotMatch(taskSummary, /task-summary-subagents" onClick/);
assert.doesNotMatch(taskSummary, /slice\(0, 12\)/);
assert.doesNotMatch(taskSummary, /meta: `\$\{status\} · \$\{(?:identity|processId)\}`/);
assert.match(taskSummary, /normalizeMultiAgentState\(message\)/);
assert.match(taskSummary, /normalizeSubagentActivity\(message\)/);
assert.match(taskSummary, /multiAgentStateIsTerminal\(status\)/);
assert.match(taskSummary, /subagent\.displayStatus === "interrupted" \? "done" : "working"/);
assert.match(subagentActivity, /!multiAgentAction\(data\.tool\) && text\(data\.tool\) !== "wait"/);
assert.match(subagentActivity, /data\.agentThreadId \?\? data\.agent_thread_id/);
assert.match(subagentActivity, /data\.receiverThreadIds \?\? data\.receiver_thread_ids/);
assert.match(subagentActivity, /data\.agentsStates \?\? data\.agents_states/);
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

const transpileDiscovery = (code) => require("typescript").transpileModule(code, { compilerOptions: { target: require("typescript").ScriptTarget.ES2022 } }).outputText;
const discoveryEvents = Function(transpileDiscovery(sourceBetween(source("src/projectionLive.ts"), "let generation = 0;", "function attach("))
  + "return { bump, useProjectionChange: () => projectionChange, useProjectionStructureGeneration: () => structureGeneration };")();
let discoveryReads = 0, discoveryEffectIndex = 0, discoveryAvailable = ["home", "parent"];
const discoveryDeps = [], discoveryRuns = { current: [] };
const discoveryScope = {
  ...discoveryEvents, activeRunId: "", refreshMessages: () => {},
  refreshRuns: () => { discoveryReads++; discoveryRuns.current = discoveryAvailable.map((runId) => ({ runId })); },
  useEffect: (effect, deps) => {
    const previous = discoveryDeps[discoveryEffectIndex]; discoveryDeps[discoveryEffectIndex++] = deps;
    if (!previous || deps.some((value, index) => !Object.is(value, previous[index]))) effect();
  },
};
const discoveryRender = Function(...Object.keys(discoveryScope), transpileDiscovery(sourceBetween(workspace,
  "  const projectionChange = useProjectionChange();", "  useEffect(() => {\n    if (!enabled || !projectionGeneration")));
const discover = () => { discoveryEffectIndex = 0; discoveryRender(...Object.values(discoveryScope)); };
discover(); discoveryAvailable = ["home", "parent", "child-1", "child-2", "child-3"];
discoveryEvents.bump({ generation: 101, kind: "structure" }); discoveryEvents.bump({ generation: 102, kind: "ledger" }); discover();
assert.deepEqual(discoveryRuns.current.map((run) => run.runId), discoveryAvailable, "ledger bursts must not erase structural run discovery");
discoveryEvents.bump({ generation: 103, kind: "ledger" }); discover();
assert.equal(discoveryReads, 2, "ledger-only changes retain incremental status reads");

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
