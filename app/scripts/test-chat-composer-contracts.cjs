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
assert.match(sidebar, /onClick=\{\(\) => setAgentDialog\(\{ mode: "edit", presetId: preset\.id \}\)\}/);
assert.doesNotMatch(sidebar, /mode: "view"|preset\.origin/);
assert.doesNotMatch(sidebar, /onNewChat\(preset\.id\)/);
assert.match(sidebar, /projectById\.has\(preset\.projectEeId\)/);
assert.match(sidebar, /meta=\{agentTaskMeta\(preset, taskChats\)\}/);
assert.doesNotMatch(sidebar, /function agentPresetMeta/);
assert.match(sidebar, /\? \[run\.runId\]/);
assert.match(ownedUtilities, /--color-token-warning: var\(--color-text-warning\);/);
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
