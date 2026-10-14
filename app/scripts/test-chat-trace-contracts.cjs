const assert = require("node:assert/strict");
const fs = require("node:fs");
const { createRequire } = require("node:module");
const path = require("node:path");
const ts = require("typescript");

const appRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(appRoot, "..");

function source(relativePath) {
  return fs.readFileSync(path.join(appRoot, relativePath), "utf8");
}

function sourceBetween(value, startMarker, endMarker) {
  const start = value.indexOf(startMarker);
  assert.notEqual(start, -1, `missing source marker: ${startMarker}`);
  const end = value.indexOf(endMarker, start + startMarker.length);
  assert.notEqual(end, -1, `missing source marker: ${endMarker}`);
  return value.slice(start, end);
}

function typescriptModule(relativePath, mocks = {}) {
  const filename = path.join(appRoot, relativePath);
  const compiled = ts.transpileModule(fs.readFileSync(filename, "utf8"), {
    compilerOptions: { esModuleInterop: true, module: ts.ModuleKind.CommonJS, resolveJsonModule: true, target: ts.ScriptTarget.ES2022 },
    fileName: filename,
  }).outputText;
  const module = { exports: {} };
  const localRequire = createRequire(filename);
  Function("module", "exports", "require", compiled)(module, module.exports, (specifier) => (
    Object.prototype.hasOwnProperty.call(mocks, specifier)
      ? mocks[specifier]
      : localRequire(specifier)
  ));
  return module.exports;
}

const turnActivity = source("src/components/chat/TurnActivity.tsx");
const chatView = source("src/components/ChatView.tsx");
const applicationsView = source("src/components/ApplicationsView.tsx");
const messageList = source("src/components/chat/ChatMessageList.tsx");
const messageTooltip = source("src/components/chat/MessageTooltip.tsx");
const sidebar = source("src/components/Sidebar.tsx");
const codexNav = source("src/components/navigation/OctopusNav.tsx");
const sidebarDnd = source("src/components/navigation/SidebarDnd.tsx");
const operatorShell = source("src/components/shell/OperatorShell.tsx");
const surfaceHost = source("src/components/shell/SurfaceHost.tsx");
const paneTabStrip = source("src/components/shell/PaneTabStrip.tsx");
const workspaceSurfacePane = source("src/components/shell/WorkspaceSurfacePane.tsx");
const chatThreadHeader = source("src/components/chat/ChatThreadHeader.tsx");
const taskSummaryPanel = source("src/components/chat/TaskSummaryPanel.tsx");
const taskSourcesPane = source("src/components/chat/taskSourcesPane.ts");
const taskSourcesPaneTab = source("src/components/chat/TaskSourcesPaneTab.tsx");
const taskActionsMenu = source("src/shell/taskActionsMenu.ts");
const compactTabContext = source("src/components/shell/WorkspaceCompactTabContext.tsx");
const paneHeaderActions = source("src/components/shell/WorkspacePaneHeaderActions.tsx");
const operatorShellState = source("src/components/shell/operatorShellState.ts");
const taskActionsController = source("src/components/task/TaskActionsController.tsx");
const workerEnvironmentPaneTab = source("src/components/worker-environment/WorkerEnvironmentPaneTab.tsx");
const runEnvironmentView = source("src/components/worker-environment/RunEnvironmentView.tsx");
const chatWorkspace = source("src/components/chat/useChatWorkspace.ts");
const chatStorageSource = source("src/components/chat/storage.ts");
const chatDraftStateSource = source("src/components/chat/useChatDraftState.ts");
const chatTurnQueueSource = source("src/components/chat/useChatTurnQueue.ts");
const clientTurnQueueSource = source("src/components/chat/clientTurnQueue.ts");
const chatActivityProjectionSource = source("src/components/chat/chatActivityProjection.ts");
const chatCapabilityCatalog = source("src/components/chat/useChatCapabilityCatalog.ts");
const conversationTrace = source("src/components/chat/conversationTrace.ts");
const chatRuns = source("src/chatRuns.ts");
const electronProjection = source("electron/projection.cjs");
const electronMain = source("electron/main.cjs");
const operatorChatClient = fs.readFileSync(path.join(repoRoot, "registry", "interface", "world_operator_chat", "client.cjs"), "utf8");
const viteConfig = source("vite.config.ts");
const chatCss = source("src/components/chat/chat.css");
const iconGlyphs = source("src/components/IconGlyphs.tsx");
const shellGlyphs = source("src/components/icon-glyphs/shell.tsx");
const iconNotices = JSON.parse(source("licenses/icon-assets/notices.json"));
const glyphAsset = (name) => iconNotices.glyphs.find((entry) => entry.name === name);
const appIndex = source("index.html");
const app = source("src/App.tsx");
const clientPreferencesSource = source("src/clientPreferences.ts");
const octopusLoadingLogo = source("src/components/OctopusLoadingLogo.tsx");
const toolActivityPresentationCatalog = JSON.parse(source("src/components/chat/toolActivityPresentationCatalog.json"));
const launchTerminal = source("src/components/onboarding/LaunchTerminal.tsx");
const workspaceSetupGate = source("src/components/onboarding/WorkspaceSetupGate.tsx");
const voiceView = source("src/components/voice/VoiceView.tsx");
const voiceCss = source("src/components/voice/voice.css");
const chatComposer = source("src/components/chat/ChatComposer.tsx");
const localDictation = source("src/components/chat/useLocalDictation.ts");
const voiceWidgets = source("src/components/voice/voiceWidgetDocuments.ts");
const clientSurfaceRenderer = source("src/client-surfaces/ClientSurfaceRenderer.tsx");
const clientWorkspacesSource = source("src/shell/clientWorkspaces.ts");
const appShell = source("src/components/shell/AppShell.tsx");
const shellCss = source("src/shell.css");
const stylesCss = source("src/styles.css");
const paneChromeOwnershipSource = source("src/components/shell/paneChromeOwnership.ts");
const assistantDirectives = typescriptModule("src/components/chat/assistantDirectives.ts");
const toolPresentation = typescriptModule("src/components/chat/toolPresentation.ts");
const reasoningSummary = typescriptModule("src/components/chat/reasoningSummary.ts");
const chatTurnControl = typescriptModule("src/components/chat/chatTurnControl.ts");
const paneChromeOwnership = typescriptModule("src/components/shell/paneChromeOwnership.ts");

assert.equal(
  reasoningSummary.reasoningSummaryHeading("**Gathering full architecture documentation**\n\n**Planning parallel document retrieval**"),
  "Planning parallel document retrieval",
);
assert.equal(reasoningSummary.reasoningSummaryHeading("<!-- partial -->\n### Mapping action blocks"), "Mapping action blocks");
assert.equal(reasoningSummary.reasoningSummaryHeading("<!--"), "");

function turnControlMessage({
  error = false,
  eventData = {},
  eventKind,
  movementSeq,
  role = "assistant",
  terminal = true,
}) {
  return {
    id: `message-${movementSeq}`,
    runId: "run-resume",
    role,
    label: "test",
    direction: role === "user" ? "in" : "out",
    text: "test",
    createdAtMs: movementSeq,
    movementSeq,
    artifactId: `artifact-${movementSeq}`,
    contentHash: "a".repeat(64),
    interfaceId: "world_operator_chat_resume",
    eventKind,
    eventData,
    latencyMs: 0,
    attachments: [],
    error,
    terminal,
  };
}

const successfulTurn = turnControlMessage({ eventKind: "message", movementSeq: 1 });
const failedTurn = turnControlMessage({ error: true, eventKind: "error", movementSeq: 2 });
const interruptedTurn = turnControlMessage({ eventKind: "interrupted_result", movementSeq: 3 });
const failedResume = turnControlMessage({
  error: true,
  eventData: { action: "resume", control_id: "control-failed", status: "failed" },
  eventKind: "turn_control",
  movementSeq: 4,
  terminal: false,
});
const acknowledgedResume = turnControlMessage({
  eventData: { action: "resume", control_id: "control-ack", status: "acknowledged" },
  eventKind: "turn_control",
  movementSeq: 5,
  terminal: false,
});
assert.equal(chatTurnControl.operatorTurnResumeAvailable([successfulTurn], false), false);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([failedTurn], false), true);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([interruptedTurn], false), true);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([{ ...interruptedTurn, eventData: { native_terminal_status: "completed", turn_interrupted: false } }], false), false);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([interruptedTurn, failedResume], false), true);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([interruptedTurn, acknowledgedResume], false), false);
assert.equal(chatTurnControl.operatorTurnResumeAvailable([failedTurn], true), false);
assert.equal(chatTurnControl.operatorTurnResumeAcknowledgedOpen([interruptedTurn, acknowledgedResume]), true);
assert.equal(chatTurnControl.operatorTurnResumeAcknowledgedOpen([
  interruptedTurn,
  acknowledgedResume,
  { ...failedTurn, movementSeq: 6, createdAtMs: 6 },
]), false);
const chatActivityProjectionModule = typescriptModule("src/components/chat/chatActivityProjection.ts", {
  "../../chatRuns": { normalizeChatRunEventProjection: (eventKind, eventData, text) => ({ eventData, eventKind, text }) },
  "./toolPresentation": { clientToolActivityPresentation: () => null },
});
const conversationTraceModule = typescriptModule("src/components/chat/conversationTrace.ts", {
  "./toolPresentation": { dynamicToolPresentation: () => ({ conversationVisible: true }) },
});
const threadVirtualizer = typescriptModule("src/components/chat/threadVirtualizer.ts");
const clientWindowModel = typescriptModule("src/shell/clientWindowModel.ts", {
  "../components/files/filesPaneSnapshot": typescriptModule("src/components/files/filesPaneSnapshot.ts"),
});
const {
  assertOperatorChatArtifactBatch,
  requireMaterializedOperatorChatClientOwner,
} = require("../electron/operatorChatClientContract.cjs");

const previousThreadLayout = threadVirtualizer.buildThreadVirtualizerLayout({
  entries: [{ turnKey: "turn-a" }, { turnKey: "turn-b" }],
  gapPx: 10,
  measuredHeightsByKey: { "turn-a": 100, "turn-b": 100 },
});
const nextThreadLayout = threadVirtualizer.buildThreadVirtualizerLayout({
  entries: [{ turnKey: "turn-a" }, { turnKey: "turn-b" }, { turnKey: "turn-c" }],
  gapPx: 10,
  measuredHeightsByKey: { "turn-a": 100, "turn-b": 100, "turn-c": 50 },
});
assert.equal(threadVirtualizer.distanceFromBottomAnchoredToTurn({
  anchorKey: "turn-a",
  distanceFromBottomPx: 40,
  nextLayout: nextThreadLayout,
  previousLayout: previousThreadLayout,
}), 100);

const primaryShellSelection = { runId: "", surfaceId: "voice", workspaceKey: "octopus-os" };
const secondaryChatTarget = {
  kind: "surface",
  runId: "run-secondary",
  surfaceId: "chat",
  workspaceKey: "octopus-os",
};
assert.deepEqual(
  clientWindowModel.deriveActiveClientSurfaceIdentity({
    activePane: "primary",
    primary: { target: primaryShellSelection, workspaceActiveRunId: "" },
    secondary: {
      fallbackWorkspaceKey: "octopus-os",
      open: true,
      target: secondaryChatTarget,
      workspaceActiveRunId: "run-secondary",
    },
  }),
  { ...primaryShellSelection, pane: "primary", ready: true },
);
assert.deepEqual(
  clientWindowModel.deriveActiveClientSurfaceIdentity({
    activePane: "secondary",
    primary: { target: primaryShellSelection, workspaceActiveRunId: "" },
    secondary: {
      fallbackWorkspaceKey: "octopus-os",
      open: true,
      target: secondaryChatTarget,
      workspaceActiveRunId: "run-secondary",
    },
  }),
  { runId: "run-secondary", surfaceId: "chat", workspaceKey: "octopus-os", pane: "secondary", ready: true },
);
assert.deepEqual(
  clientWindowModel.deriveActiveClientSurfaceIdentity({
    activePane: "secondary",
    primary: { target: primaryShellSelection, workspaceActiveRunId: "" },
    secondary: {
      fallbackWorkspaceKey: "octopus-os",
      open: true,
      target: secondaryChatTarget,
      workspaceActiveRunId: "run-workspace-ghost",
    },
  }),
  { runId: "", surfaceId: "chat", workspaceKey: "octopus-os", pane: "secondary", ready: false },
);
assert.deepEqual(
  clientWindowModel.deriveActiveClientSurfaceIdentity({
    activePane: "secondary",
    primary: { target: primaryShellSelection, workspaceActiveRunId: "" },
    secondary: {
      fallbackWorkspaceKey: "octopus-os",
      open: true,
      target: { kind: "files", workspaceKey: "dashboard" },
      workspaceActiveRunId: "run-ignored",
    },
  }),
  { runId: "", surfaceId: "", workspaceKey: "dashboard", pane: "secondary", ready: true },
);
const taskSourcesTab = clientWindowModel.workspaceTaskSourcesPaneTab("octopus-os", {
  runId: "run-sources",
  schemaVersion: 1,
  sources: [],
});
assert.equal(taskSourcesTab.id, "task-sources:run-sources");
assert.equal(clientWindowModel.transferableWorkspacePaneTarget(taskSourcesTab.target), null);
assert.equal(
  clientWindowModel.screenPointOutsideClientWindow(
    { x: 899, y: 799 },
    { height: 600, width: 800, x: 100, y: 200 },
  ),
  false,
);
assert.equal(
  clientWindowModel.screenPointOutsideClientWindow(
    { x: 900, y: 400 },
    { height: 600, width: 800, x: 100, y: 200 },
  ),
  true,
);
assert.equal(
  clientWindowModel.screenPointOutsideClientWindow(
    null,
    { height: 600, width: 800, x: 100, y: 200 },
  ),
  false,
);
assert.equal(clientWindowModel.clientWindowChatStorageScope("main", "secondary"), "secondary");
assert.equal(clientWindowModel.clientWindowChatStorageScope("window-a", "secondary"), "w_window-a_secondary");
assert.match(
  clientWindowModel.clientWindowChatStorageScope("window-12345678-1234-1234-1234-123456789abc", "secondary"),
  /^[A-Za-z][A-Za-z0-9_-]{0,31}$/,
);
assert.equal(paneChromeOwnership.paneChromeOwner("primary", []), "window-titlebar");
assert.equal(paneChromeOwnership.paneChromeOwner("secondary", []), "pane-launcher");
assert.equal(paneChromeOwnership.paneChromeOwner("secondary", [{}]), "pane-content-header");
assert.equal(paneChromeOwnership.paneChromeOwner("secondary", [{ showWhenSingle: true }]), "pane-tab-strip");
assert.equal(paneChromeOwnership.paneChromeOwner("secondary", [{}, {}]), "pane-tab-strip");
assert.doesNotMatch(paneChromeOwnershipSource, /\b(width|sidebar|resize|glyph|activeTabId)\b/);
assert.match(chatStorageSource, /function chatStateStorage\(_scope: string\): Storage \{\s*return window\.localStorage;/);
assert.match(chatStorageSource, /messageId: string;[\s\S]*?persistedQueuedMessageId\(candidate\.messageId, id\)/);
assert.match(chatStorageSource, /PersistedChatDraft[\s\S]*?messageId: string;[\s\S]*?messageHadAttachments: boolean;[\s\S]*?persistedDraftMessageId\(candidate\.messageId\)/);
assert.match(chatWorkspace, /const identity = randomId\(\);[\s\S]*?id: `queued_\$\{identity\}`,[\s\S]*?messageId: `msg_\$\{identity\}`[\s\S]*?hadAttachments: attachments\.length > 0/);
const queueEdit = sourceBetween(chatWorkspace, "async function editQueuedTurn(", "async function cancelQueuedEdit(");
assert.match(queueEdit, /applyDraft\(\{[\s\S]*?messageId: item\.messageId, messageHadAttachments: item\.hadAttachments[\s\S]*?attachments: savedEdit\?\.attachments \?\? item\.attachments/);
const queueDraftSubmission = sourceBetween(chatWorkspace, "async function submitDraft(", "async function beginSubmission(");
assert.match(queueDraftSubmission, /const queuedAttachmentBytesNeedRecovery = Boolean\([\s\S]*?editedQueuedTurn\?\.hadAttachments[\s\S]*?editedQueuedTurn\.attachments\.length === 0[\s\S]*?const editedQueuePayloadChanged = Boolean\([\s\S]*?body !== editedQueuedTurn\.text[\s\S]*?!queuedAttachmentBytesNeedRecovery[\s\S]*?const submissionMessageId = editedQueuePayloadChanged[\s\S]*?messageId: submissionMessageId[\s\S]*?status: "queued"/);
assert.match(queueDraftSubmission, /if \(mode === "queue" \|\| \(replyOpen && mode === "send"\)\)[\s\S]*?await enqueueCurrentTurn\(body\);[\s\S]*?return;[\s\S]*?if \(mode === "steer"\) \{\s*await submitSteeringMessage\(\{[\s\S]*?messageId: submissionMessageId/);
assert.match(chatWorkspace, /submitOperatorSteer\(\{[\s\S]*?recoverPreparedMessage: input\.recoverPreparedMessage[\s\S]*?providerInterfaceId: run\.providerInterfaceId[\s\S]*?clientSurfaceRole: run\.clientSurfaceRole/);
assert.match(chatWorkspace, /reportClientExtensionIncident\(\{[\s\S]*?interfaceId: turn\.providerInterfaceId \|\| artifact\.from_interface_id/);
assert.match(electronProjection, /recordedSelection !== selected \|\| !bound[\s\S]*?id: selected,[\s\S]*?boundId: bound/);
const steeringSubmission = sourceBetween(chatWorkspace, "async function submitSteeringMessage(", "async function steerQueuedTurn(");
assert.match(steeringSubmission, /const messageId = input\.messageId \?\? `msg_\$\{randomId\(\)\}`;[\s\S]*?await replaceQueuedTurns[\s\S]*?id: queueId, messageId, runKey: input\.runKey[\s\S]*?await queueStore\.claim\(queueId, owner, true\);[\s\S]*?consumeDraft\(run\.runId, claimed\.text, claimed\.attachments\)[\s\S]*?submitOperatorSteer\(\{\s*runId: run\.runId, messageId,[\s\S]*?"refused" in submitted\.verdict[\s\S]*?accepted = true;\s*await queueStore\.settle\(queueId, owner, "accepted", undefined, submitted\.verdict\.admitted\.seq\)/);
assert.match(chatDraftStateSource, /function persistSubmissionIntent\([\s\S]*?saveChatDrafts\([\s\S]*?setMessageId\(nextMessageId\)/);
assert.match(chatDraftStateSource, /function completeSubmissionIntent\([\s\S]*?messageId: ""[\s\S]*?text: ""[\s\S]*?saveChatDrafts\(/);
assert.match(chatDraftStateSource, /function currentDraftForStorage\([\s\S]*?existing\?\.messageId === current\.messageId[\s\S]*?\{ \.\.\.existing, updatedAtMs: current\.updatedAtMs \}/);
assert.match(chatTurnQueueSource, /indexedQueuePersistence\(globalThis\.indexedDB\)[\s\S]*?new ClientTurnQueue\(key, persistence,/);
assert.match(chatTurnQueueSource, /async function replaceQueuedTurns\([\s\S]*?await queueStore\.change\([\s\S]*?turns: typeof update === "function" \? update\(current\.turns\) : update[\s\S]*?return state\.turns;/);
assert.match(clientTurnQueueSource, /await this\.persistence\.update\(this\.key,[\s\S]*?this\.publish\(state\)/);
const recoveredSubmission = sourceBetween(chatWorkspace, "if (submitted.messageRecovered)", "const afterSeq =");
assert.match(recoveredSubmission, /await refreshMessages\(submitted\.runId\);\s*await refreshRuns\(\);\s*current = null;\s*continue;/);
assert.doesNotMatch(recoveredSubmission, /replaceQueuedTurns|executeTurnSequence|submitOperatorTurn\(/);
assert.match(chatWorkspace, /function paneTransferBlockReason\(runId: string\)[\s\S]*?draft\?\.attachments\.length[\s\S]*?draft\?\.text \|\| draft\?\.continuation[\s\S]*?queuedTurnsRef\.current\.some/);
assert.match(operatorShell, /paneTransferBlockReasonRef\.current\(target, "secondary"\)/);
assert.match(operatorShell, /pendingAcceptedPaneTransfersRef[\s\S]*?previousTabs: state\.secondaryTabs[\s\S]*?resumeClientPaneTransfer[\s\S]*?commitClientPaneTransfer[\s\S]*?finalizeClientPaneTransfer[\s\S]*?rollbackAcceptedPaneTransfer/);
assert.match(operatorShell, /pendingClientPaneDestinationTransfers[\s\S]*?stageDestinationPaneTransfer\(transfer, transfer\.committed\)/);
assert.match(operatorShell, /writeSecondaryPaneState\(accessProfile, homeRunId, clientWindow\.scope,[\s\S]*?acknowledgeClientPaneSourceFinalization\(\{ transferId: completion\.transferId \}\)/);
assert.match(operatorShell, /sourceTabId === `client-window:\$\{clientWindow\.windowId\}`[\s\S]*?samePaneTransferTarget\(currentTarget, completion\.target\)/);
const paneTransferCompletionHandler = sourceBetween(operatorShell, "  const handleClientPaneTransferCompletion = useCallback(", "  useEffect(");
assert.notEqual(paneTransferCompletionHandler, "");
assert.doesNotMatch(paneTransferCompletionHandler, /writeSecondaryPaneState|replaceState|windowControl/);
assert.match(paneTransferCompletionHandler, /acknowledgeClientPaneTransfer/);
assert.equal(clientWindowModel.clientWindowStateKey("main", "surface"), "surface");
assert.equal(clientWindowModel.clientWindowStateKey("window-a", "surface"), "octopus.client-window.window-a.surface");
assert.match(operatorShell, /const \[activePane, setActivePane\] = useState<"primary" \| "secondary">\(\s*clientWindowInitialTarget \? "secondary" : "primary"/);
assert.match(operatorShell, /initial-target-consumed\.v1[\s\S]*?window\.sessionStorage\.getItem[\s\S]*?window\.sessionStorage\.setItem/);
const paneDrag = {
  schema: "octopus.client-pane-transfer",
  schemaVersion: 1,
  sourceWindowId: "window-a",
  transferId: "transfer-a",
};
assert.deepEqual(
  clientWindowModel.parseClientPaneDragPayload(JSON.stringify(paneDrag)),
  paneDrag,
);
assert.equal(clientWindowModel.parseClientPaneDragPayload(JSON.stringify({ ...paneDrag, transferId: "bad\ntransfer" })), null);
assert.equal(clientWindowModel.normalizeTransferableWorkspacePaneTarget({
  kind: "terminal",
  terminalId: "terminal-a",
  title: "Terminal",
  workspaceKey: "dashboard",
}), null);
const filesSnapshot = {
  version: 1, id: "editor-a", revision: 7, rootId: "", rootPath: "",
  file: null, draft: "", view: { scrollTop: 24, scrollLeft: 0 },
};
const transferredFiles = { kind: "files", workspaceKey: "octopus-os", snapshot: filesSnapshot };
assert.deepEqual(clientWindowModel.normalizeTransferableWorkspacePaneTarget(transferredFiles), transferredFiles);
assert.equal(clientWindowModel.workspacePaneTabForTarget(transferredFiles).id, "files:editor-a");
assert.notEqual(clientWindowModel.workspacePaneTabForTarget(transferredFiles).id, clientWindowModel.workspacePaneTabForTarget({ kind: "files", workspaceKey: "octopus-os" }).id);
assert.equal(clientWindowModel.normalizeTransferableWorkspacePaneTarget({ ...transferredFiles, snapshot: { ...filesSnapshot, revision: -1 } }), null);
assert.equal(clientWindowModel.normalizeTransferableWorkspacePaneTarget({
  kind: "surface",
  runId: "ignored",
  surfaceId: "drivers",
  workspaceKey: "octopus-os",
}).runId, "");

const exactPackageDigest = "a".repeat(64);
assert.deepEqual(requireMaterializedOperatorChatClientOwner({
  packages: [{
    id: "world_operator_chat",
    available: true,
    installed: false,
    materialized: false,
    partially_materialized: true,
    receipt_state: "missing",
    member_states: [
      {
        registry_kind: "interface",
        owner: "world_operator_chat",
        materialized: true,
        source_digest: exactPackageDigest,
        installed_digest: exactPackageDigest,
      },
      {
        registry_kind: "interface",
        owner: "unrelated_package_member",
        materialized: false,
        source_digest: exactPackageDigest,
        installed_digest: "",
      },
    ],
  }],
}), {
  installedDigest: exactPackageDigest,
  owner: "world_operator_chat",
  registryKind: "interface",
  sourceDigest: exactPackageDigest,
});
assert.throws(
  () => requireMaterializedOperatorChatClientOwner({ packages: [] }),
  (error) => error?.code === "operator_chat_client_contract_mismatch",
);
assert.throws(
  () => requireMaterializedOperatorChatClientOwner({
    packages: [{
      id: "world_operator_chat",
      available: true,
      member_states: [{
        registry_kind: "interface",
        owner: "world_operator_chat",
        materialized: true,
        source_digest: exactPackageDigest,
        installed_digest: "invalid",
      }],
    }],
  }),
  (error) => error?.code === "operator_chat_client_contract_mismatch",
);
assert.match(electronMain, /createOperatorChatClientLoader\(/);
assert.match(viteConfig, /createOperatorChatClientLoader\(/);
assert.equal((viteConfig.match(/operatorChatClientLoader\.load\(\)/g) ?? []).length, 6);
assert.doesNotMatch(viteConfig, /require\(path\.join\(octopusHome, "registry", "interface", "world_operator_chat", "client\.cjs"\)\)/);
assert.match(viteConfig, /assertOperatorChatArtifactBatch\(batch\)/);
assert.match(operatorChatClient, /assertOperatorChatLiveArtifactBatch\(\{ artifacts, cursor \}\);\s*return \{ artifacts, cursor \}/);
assert.match(operatorChatClient, /"turn\.context\.compaction": "compaction"/);
assert.match(operatorChatClient, /if \(label === config\.artifact_labels\.terminal_output\) return "message"/);
assert.match(operatorChatClient, /eventKind === "compaction"\) delete eventData\.text/);
assert.match(operatorChatClient, /payload_content_hash !== undefined && manifest\.payload_content_hash !== null/);
assert.match(operatorChatClient, /watchLedgerChange\(bodyPath, input\.waitMs\)/);
assert.match(operatorChatClient, /readOpenOperatorTurnStreamTail\(bodyPath, input\.cursor/);
assert.match(electronProjection, /"turn\.stream"/);
assert.match(electronProjection, /sealed turn\.stream has no seal row/);
assert.match(electronProjection, /function operatorChatProjectedRowId\(row, fallback\) \{[\s\S]*?return `\$\{row\.__stream\.stream_id\}:\$\{row\.__stream\.stream_seq\}`;[\s\S]*?return ledgerArtifactId\(row\) \|\| fallback;/);
assert.doesNotMatch(operatorChatClient, /run_boundary|context_boundary/);
assert.match(applicationsView, /action: "repair_package"/);
assert.match(applicationsView, /expectedRepairs: \{ \[application\.package_id\]: expectation \}/);
assert.throws(
  () => assertOperatorChatArtifactBatch({
    artifacts: [{ from_interface_id: "world_provider_codex_exact", from_interface_role: "" }],
    cursor: { offset: 0, seq: null },
  }),
  (error) => error?.code === "operator_chat_client_contract_mismatch",
);

const directiveProjection = assistantDirectives.projectAssistantDirectives([
  "Answer body.",
  '::code-comment{title="[P1] Bound path" body="Keep the exact room root." file="/repo/src/a.ts" start=4 end=6 priority=P1}',
  '::codex-inline-vis{file="reports/result.html"}',
].join("\n"));
assert.equal(directiveProjection.markdown, "Answer body.");
assert.deepEqual(directiveProjection.codeComments.map(({ directiveIndex, ...comment }) => comment), [{
  body: "Keep the exact room root.",
  endLine: 6,
  path: "/repo/src/a.ts",
  priority: 1,
  startLine: 4,
  title: "[P1] Bound path",
}]);
assert.deepEqual(directiveProjection.inlineVisualizations, [{ directiveIndex: 1, path: "reports/result.html" }]);
assert.equal(assistantDirectives.directivePathWithinRoot("/repo/reports/result.html", "/repo"), "reports/result.html");
assert.equal(assistantDirectives.directivePathWithinRoot("../outside.html", "/repo"), null);
const literalDirective = "```text\n::code-comment{title=literal}\n```\n::code-comment{title=malformed}";
assert.equal(assistantDirectives.projectAssistantDirectives(literalDirective).markdown, literalDirective);
const projectedCodeComments = assistantDirectives.projectAssistantCodeComments([
  {
    id: "assistant-a",
    role: "assistant",
    runId: "run-a",
    text: 'Answer.\n::code-comment{title="Bound path" body="Keep the exact root." file="src/a.ts" start=4}',
    createdAtMs: 1,
    terminal: true,
  },
  {
    id: "assistant-a-duplicate",
    role: "assistant",
    runId: "run-a",
    text: '::code-comment{title="Bound path" body="Keep the exact root." file="src/a.ts" start=4}',
    createdAtMs: 2,
    terminal: true,
  },
  {
    id: "assistant-other-run",
    role: "assistant",
    runId: "run-b",
    text: '::code-comment{title="Wrong run" body="Do not project." file="src/b.ts"}',
    createdAtMs: 3,
    terminal: true,
  },
], "run-a");
assert.deepEqual(projectedCodeComments.map(({ id, sourceMessageId, title }) => ({ id, sourceMessageId, title })), [{
  id: "assistant-a:code-comment:0",
  sourceMessageId: "assistant-a",
  title: "Bound path",
}]);

const workspaceToolIds = fs.readdirSync(path.join(repoRoot, "registry", "tool"))
  .map((entry) => path.join(repoRoot, "registry", "tool", entry, "tool.json"))
  .filter((descriptorPath) => fs.existsSync(descriptorPath))
  .map((descriptorPath) => JSON.parse(fs.readFileSync(descriptorPath, "utf8")))
  .map((descriptor) => descriptor.id)
  .filter((toolId) => typeof toolId === "string" && toolId.startsWith("workspace."))
  .sort();
assert.deepEqual(
  Object.keys(toolActivityPresentationCatalog).filter((toolId) => toolId.startsWith("workspace.")).sort(),
  workspaceToolIds,
  "every filesystem_workspace tool must have an exact conversation presentation",
);
assert.deepEqual(toolActivityPresentationCatalog["workspace.read_file"].target_arguments, ["path"]);
assert.deepEqual(toolActivityPresentationCatalog["workspace.exec"].target_arguments, ["command"]);
assert.deepEqual(toolActivityPresentationCatalog["workspace.grep"].query_arguments, ["query"]);
assert.equal(toolActivityPresentationCatalog["workspace.apply_patch"].kind, "file_change");
const workspaceToolSuffix = "ffc1f8a5d06b5d4c8f5df13b40f1aeab";
const projectedWorkspaceRead = toolPresentation.dynamicToolPresentation({
  eventData: {
    arguments: { path: "src/life.rs" },
    tool: `workspace.read_file__${workspaceToolSuffix}`,
  },
}, {});
const projectedWorkspaceExec = toolPresentation.dynamicToolPresentation({
  eventData: {
    arguments: { command: "git status --short" },
    tool: `workspace.exec__${workspaceToolSuffix}`,
  },
}, {});
assert.equal(projectedWorkspaceRead.family, "read");
assert.equal(toolPresentation.dynamicToolReadLabel(projectedWorkspaceRead, false), "Read life.rs");
assert.equal(projectedWorkspaceExec.family, "exec");
assert.equal(projectedWorkspaceExec.target, "git status --short");
const projectedBrowserTabs = toolPresentation.dynamicToolPresentation({
  eventData: { arguments: {}, tool: `browser.tabs__${workspaceToolSuffix}` },
}, {});
const projectedContainerStatus = toolPresentation.dynamicToolPresentation({
  eventData: { arguments: {}, tool: `container.status__${workspaceToolSuffix}` },
}, {});
assert.equal(toolPresentation.dynamicToolOperationLabel(projectedBrowserTabs), "Used the browser");
assert.equal(toolPresentation.dynamicToolOperationLabel(projectedContainerStatus), "Container status");
assert.equal(toolPresentation.dynamicToolGlyph(projectedBrowserTabs), "browserUse");
assert.equal(toolPresentation.dynamicToolGlyph(projectedWorkspaceRead), "read");
assert.equal(toolPresentation.dynamicToolGlyph(projectedWorkspaceExec), "terminal");

const exactProviderLiveTool = chatActivityProjectionModule.liveArtifactMessage({
  source: "movement",
  movement_seq: 10,
  recorded_at_unix_ms: 1_700_000_000_000,
  run_id: "run_exact",
  interface_id: "world_operator_chat_exact",
  checkpoint_id: "cp_provider_output",
  from_interface_id: "world_provider_codex_exact",
  from_interface_role: "provider",
  artifact_id: "turn.dynamic_tool_call:exact",
  content_hash: "a".repeat(64),
  label: "turn.dynamic_tool_call",
  event_kind: "dynamic_tool",
  event_data: { call_id: "call_exact", status: "started", tool: "workspace.read_file__exact" },
  text: "workspace.read_file__exact",
  error: false,
  terminal: false,
  will_retry: false,
}, new Map());
assert.equal(exactProviderLiveTool.eventData._activity_origin.interface_role, "provider");
assert.equal(exactProviderLiveTool.eventData._movement.seq, 10);
assert.equal(exactProviderLiveTool.eventData._stream, undefined);
assert.equal(conversationTraceModule.conversationTraceAdmission(exactProviderLiveTool, {}), "visible");
const exactProviderLiveStream = chatActivityProjectionModule.liveArtifactMessage({
  source: "stream",
  stream_id: "b".repeat(64),
  stream_seq: 7,
  byte_start_offset: 100,
  byte_end_offset: 180,
  source_movement_seq: 10,
  recorded_at_unix_ms: 1_700_000_000_007,
  run_id: "run_exact",
  interface_id: "world_operator_chat_exact",
  from_interface_id: "world_provider_codex_exact",
  from_interface_role: "provider",
  label: "turn.reasoning.delta",
  event_kind: "reasoning_delta",
  event_data: { delta: "proof", _movement: { seq: 999 } },
  text: "proof",
  error: false,
  terminal: false,
  will_retry: false,
}, new Map());
assert.equal(exactProviderLiveStream.eventData._movement, undefined);
assert.equal(exactProviderLiveStream.eventData._stream.stream_seq, 7);
assert.equal(exactProviderLiveStream.artifactId, "");
assert.equal(exactProviderLiveStream.contentHash, "");
const durableBetweenStreamRows = {
  ...exactProviderLiveTool,
  id: "durable-between-stream-rows",
  createdAtMs: 1_700_000_000_005,
  movementSeq: 500,
};
assert.deepEqual(
  chatActivityProjectionModule.mergeChatRunMessages(
    [],
    [exactProviderLiveStream, durableBetweenStreamRows],
  ).map((message) => message.id),
  ["durable-between-stream-rows", exactProviderLiveStream.id],
);
assert.doesNotThrow(() => assertOperatorChatArtifactBatch({
  artifacts: [{
    source: "stream",
    stream_id: "b".repeat(64),
    stream_seq: 7,
    byte_start_offset: 100,
    byte_end_offset: 180,
    source_movement_seq: 10,
    recorded_at_unix_ms: 1_700_000_000_007,
    run_id: "run_exact",
    interface_id: "world_operator_chat_exact",
    from_interface_id: "world_provider_codex_exact",
    from_interface_role: "provider",
    label: "turn.reasoning.delta",
    event_kind: "reasoning_delta",
    event_data: {},
    text: "proof",
    error: false,
    terminal: false,
    will_retry: false,
  }],
  cursor: { offset: 0, seq: null },
}));

const duplicateProjection = Array.from({ length: 2_000 }, (_, index) => ({
  id: `projected-${index}`,
  role: "assistant",
  text: `fragment ${index}`,
  createdAtMs: index,
  eventKind: index % 7 === 0 ? "message" : "reasoning_delta",
}));
const deduplicatedProjection = chatActivityProjectionModule.mergeRenderedMessages(
  duplicateProjection,
  duplicateProjection.map((message) => ({ ...message })),
  [],
  true,
);
assert.equal(deduplicatedProjection.length, duplicateProjection.length);
assert.deepEqual(deduplicatedProjection.map((message) => message.id), duplicateProjection.map((message) => message.id));
const pendingSteer = Object.freeze({ id: "steer-message", runId: "run-a", role: "user", text: "exact steering text",
  createdAtMs: 1, eventKind: "steering", pending: true,
  eventData: Object.freeze({ steering_status: "submitted", steering_provider_interface: "provider-exact" }) });
const steerReceipt = (status, sequence, extra = {}) => chatActivityProjectionModule.toRender(chatActivityProjectionModule.liveArtifactMessage({
  source: "movement", run_id: "run-a", artifact_id: `steer-receipt-${sequence}`, movement_seq: sequence,
  recorded_at_unix_ms: sequence, interface_id: "operator-chat", from_interface_id: "provider-exact", from_interface_role: "provider",
  label: `turn.control.${status}`, event_kind: "turn_control", text: "", terminal: false, error: status === "failed",
  event_data: { action: "steer", control_id: pendingSteer.id, status }, ...extra,
}, new Map()));
const mergedSteer = (receipts, message = pendingSteer) => chatActivityProjectionModule.mergeRenderedMessages(receipts, [], [message], false)
  .find((message) => message.id === pendingSteer.id);
assert.equal(mergedSteer([]), pendingSteer);
const acceptedSteer = mergedSteer([steerReceipt("acknowledged", 12)]);
assert.equal(acceptedSteer.eventData.steering_status, "acknowledged");
assert.equal(acceptedSteer.pending, false);
assert.equal(acceptedSteer.text, pendingSteer.text);
assert.equal(pendingSteer.pending, true);
for (const receipt of [
  steerReceipt("acknowledged", 12, { run_id: "run-b" }),
  steerReceipt("acknowledged", 12, { event_data: { action: "steer", control_id: "another-message", status: "acknowledged" } }),
  steerReceipt("acknowledged", 12, { event_data: { action: "stop", control_id: pendingSteer.id, status: "acknowledged" } }),
  steerReceipt("acknowledged", 12, { artifact_id: "" }),
  steerReceipt("acknowledged", 0),
  { id: "ordinary-completion", runId: "run-a", eventKind: "message", role: "assistant", text: "Done", terminal: true, createdAtMs: 12 },
]) assert.equal(mergedSteer([receipt]), pendingSteer);
const refusedSteer = mergedSteer([steerReceipt("failed", 14), steerReceipt("acknowledged", 12)]);
assert.equal(refusedSteer.eventData.steering_status, "failed");
assert.equal(mergedSteer([steerReceipt("acknowledged", 12)], refusedSteer), refusedSteer);
assert.equal(mergedSteer([steerReceipt("acknowledged", 15)], refusedSteer).eventData.steering_status, "acknowledged");
const inputRecorded = (extra = {}) => chatActivityProjectionModule.toRender(chatActivityProjectionModule.liveArtifactMessage({
  source: "stream", run_id: "run-a", stream_id: "a".repeat(64), stream_seq: 2,
  byte_start_offset: 200, byte_end_offset: 400, source_movement_seq: 10, recorded_at_unix_ms: 13,
  interface_id: "operator-chat", from_interface_id: "provider-exact", from_interface_role: "provider",
  label: "turn.stream", event_kind: "input_recorded", text: "", terminal: false, error: false,
  event_data: { type: "input_recorded", stage: "input_recorded_in_turn", seq: 2,
    provider_interface: "provider-exact", run: "run-a", thread_id: "thread-a", turn_id: "turn-a",
    item_id: "native-item", client_id: pendingSteer.id, source_event: "item/started", native_event_at_ms: 12, ...extra },
}, new Map()));
const recordedSteer = mergedSteer([inputRecorded()]);
assert.equal(recordedSteer.eventData.steering_status, "input_recorded");
assert.equal(recordedSteer.text, pendingSteer.text);
for (const status of ["acknowledged", "failed"]) {
  assert.equal(mergedSteer([steerReceipt(status, 15)], recordedSteer), recordedSteer);
  assert.equal(mergedSteer([steerReceipt(status, 15), inputRecorded()]).eventData.steering_status, "input_recorded");
}
for (const extra of [{ client_id: "another" }, { run: "run-b" }, { provider_interface: "foreign-provider" },
  { turn_id: "" }, { stage: "accepted" }, { source_event: "turn/steer" }, { native_event_at_ms: null }, { seq: 0 }]) {
  assert.equal(mergedSteer([inputRecorded(extra)]), pendingSteer);
}
assert.equal(chatActivityProjectionModule.mergeChatRunMessages(
  [inputRecorded()], [steerReceipt("acknowledged", 15)], "snapshot",
).some((message) => message.eventKind === "input_recorded"), true);
const coalescedProjection = chatActivityProjectionModule.mergeRenderedMessages([
  {
    id: "delta-1",
    role: "assistant",
    text: "first ",
    createdAtMs: 1,
    eventKind: "reasoning_delta",
    eventData: { item_id: "reasoning-1", lifecycle: "streaming", summary_index: 0, turn_id: "turn-1" },
  },
  {
    id: "delta-2",
    role: "assistant",
    text: "second",
    createdAtMs: 2,
    eventKind: "reasoning_delta",
    eventData: { item_id: "reasoning-1", lifecycle: "completed", summary_index: 0, turn_id: "turn-1" },
  },
], [], [], false);
assert.equal(coalescedProjection.length, 1);
assert.equal(coalescedProjection[0].text, "first second");
assert.equal(coalescedProjection[0].eventData.lifecycle, "completed");
const assistantDelta = { id: "delta", runId: "run-a", interfaceId: "chat", role: "assistant", createdAtMs: 1,
  eventKind: "message_delta", terminal: false, text: "Progress", eventData: { turn_id: "turn-a", item_id: "item-a", phase: null, lifecycle: "streaming" } };
const assistantCompleted = { ...assistantDelta, id: "completed", createdAtMs: 3, eventKind: "commentary",
  eventData: { ...assistantDelta.eventData, phase: "commentary", lifecycle: "completed" } };
const reconcile = (messages) => chatActivityProjectionModule.mergeRenderedMessages(messages, [], [], false);
const reconciledItems = reconcile([assistantDelta, { id: "tool", role: "assistant", eventKind: "tool", text: "read", createdAtMs: 2 }, assistantCompleted]);
assert.deepEqual(reconciledItems.map(({ text, eventKind }) => [text, eventKind]), [["Progress", "commentary"], ["read", "tool"]]);
assert.equal(reconciledItems[0].id, reconcile([assistantDelta])[0].id);
assert.equal(reconciledItems[0].createdAtMs, 1);
assert.equal(reconcile([assistantDelta, assistantCompleted, { ...assistantDelta, id: "late", createdAtMs: 4 }])[0].text, "Progress");
for (const different of [{ runId: "run-b" }, { interfaceId: "another-chat" }, { eventData: { ...assistantDelta.eventData, item_id: "item-b" } }]) {
  assert.equal(reconcile([assistantDelta, { ...assistantDelta, ...different, id: "independent" }]).length, 2);
}
assert.equal(conversationTraceModule.isFinalAssistantResponse(assistantDelta), false);
assert.equal(conversationTraceModule.isFinalAssistantResponse(assistantCompleted), false);
assert.equal(conversationTraceModule.isFinalAssistantResponse({ ...assistantDelta, eventData: { ...assistantDelta.eventData, phase: "final_answer" } }), true);
assert.equal(conversationTraceModule.isFinalAssistantResponse({ ...assistantCompleted, eventKind: "message", terminal: true, eventData: { phase: "partial_answer" } }), false);
const anchoredProjection = chatActivityProjectionModule.mergeRenderedMessages([
  { id: "operator", role: "user", text: "prompt", createdAtMs: 1 },
], [
  { id: "activity-a", role: "assistant", text: "a", createdAtMs: 2, eventData: { anchor_message_id: "operator" } },
  { id: "activity-b", role: "assistant", text: "b", createdAtMs: 3, eventData: { anchor_message_id: "operator" } },
], [], true);
assert.deepEqual(anchoredProjection.map((message) => message.id), ["operator", "activity-a", "activity-b"]);
assert.match(chatActivityProjectionSource, /const projectedIdentity = projectedMessageIdentity\(projected\);/);
assert.doesNotMatch(chatActivityProjectionSource, /projected\.some\(\(message\) => sameMessage/);
assert.doesNotMatch(chatActivityProjectionSource, /anchored\.set\(anchorId, \[\.\.\./);
assert.match(chatActivityProjectionSource, /text: parts\.join\(""\)/);

assert.match(turnActivity, /if \(text\(data\.tool\) === "wait"\) return \[\];/);
assert.match(turnActivity, /kind: "multi-agent-group"/);
assert.match(turnActivity, /currentAction !== action\) flushMultiAgent\(\)/);
assert.match(turnActivity, /multiAgentGrouped\.push\(entry\)/);
assert.match(turnActivity, /kind: "subagent-group"/);
assert.match(turnActivity, /const visible = items\.slice\(0, 3\)/);
assert.match(turnActivity, /return "started working"/);
assert.doesNotMatch(turnActivity, /summaryEntry/);
assert.match(
  turnActivity,
  /if \(entry\.family === "reasoning"\) \{\s*if \(explorationOpen\) grouped\.push\(entry\);\s*continue;/,
);
assert.match(turnActivity, /grouped\.push\(entry\);\s*explorationOpen = explorationActivityEntry\(entry, toolPresentations\);/);
assert.match(turnActivity, /const flush = \(\) => \{\s*explorationOpen = false;/);
assert.match(
  turnActivity,
  /function explorationActivityEntry\([\s\S]*?if \(entry\.family === "exec"\) return execCategory\(entry\) !== "command";[\s\S]*?dynamicToolPresentation\(activityDetailMessages\(entry\)\[0\], toolPresentations\)\.family === "read";/,
);
assert.match(
  turnActivity,
  /return <ReasoningSummaryActivity entry=\{entry\} message=\{details\.at\(-1\)\} \/>;/,
);
assert.match(turnActivity, /const summary = entry\.active \? "Thinking" : elapsed \? `Thought for \$\{formatElapsed\(elapsed\)\}` : "Thought";/);
assert.match(turnActivity, /defaultExpanded=\{entry\.active\}[\s\S]*?followActive/);
assert.match(turnActivity, /const thinkingFallbackMessage = presentation === "conversation"\s*\? latestReasoningSummaryHeading\(presented\) \|\| "Thinking"\s*: "Thinking";/);
assert.match(turnActivity, /find\(\(entry\) => entry\.active && entry\.family !== "reasoning"\)/);
assert.match(turnActivity, /if \(entry\.family === "reasoning"\) return false;/);
assert.match(turnActivity, /\? `thinking:\$\{thinkingFallbackMessage\}`/);
assert.match(turnActivity, /function latestReasoningSummaryHeading\([\s\S]*?const heading = reasoningSummaryHeading\(message\.text\);/);
assert.doesNotMatch(turnActivity, /activityFamily\(latestConversationActivity\) === "reasoning"/);
assert.match(turnActivity, /return activityWindow === "latest-context" \? latestContextActivityUnits\(projected\) : projected;/);
assert.doesNotMatch(turnActivity, /retainStandaloneReasoning|standaloneReasoningPresentation|codex-reasoning-context/);
assert.doesNotMatch(turnActivity, /latestReasoningIndex|previousBatchIndex/);
assert.match(voiceView, /<HomeTurnActivity activities=\{activity\}/);
assert.doesNotMatch(voiceView, /standaloneReasoningPresentation/);
assert.match(turnActivity, /const startingTask = syntheticSubmitting[\s\S]*?&& !hasWork[\s\S]*?&& !response/);
assert.match(turnActivity, /aria-live="polite"[\s\S]*?role="status"[\s\S]*?<CadencedShimmerText active>Starting your task<\/CadencedShimmerText>/);
assert.match(turnActivity, /const showWorkedForBoundary = statusBoundaryVisible\s*&& \(hasWork \|\| failed \|\| stopped \|\| showStandaloneWorkedBoundary\);[\s\S]*?\{showWorkedForBoundary \? \([\s\S]*?<WorkedForBoundary[\s\S]*?\{showWorkedForBoundary && \(hasWork \|\| showThinkingFallback\) \? <ActivityGap \/> : null\}/);
assert.match(turnActivity, /Worked for \{formatElapsed\(elapsed\)\}/);
assert.match(turnActivity, /activeSlice[\s\S]*?\? thinkingFallbackMessage/);
assert.match(
  turnActivity,
  /const hasConversationReasoningWork = presentation === "conversation"\s*&& visibleActivity\.some\(\(message\) => activityFamily\(message\) === "reasoning"\);/,
);
assert.match(
  turnActivity,
  /const showStandaloneWorkedBoundary = finalStarted[\s\S]*?&& !hasWork[\s\S]*?&& hasConversationReasoningWork;/,
);
assert.match(
  turnActivity,
  /if \(!hasWork && !working && !failed && !stopped && !showStandaloneWorkedBoundary\) return null;/,
);
assert.match(turnActivity, /text: reasoningSummaryContent\(summaries\)/);
assert.match(turnActivity, /first\.startsWith\("\*\*"\) \? first : `\*\*\$\{first\}\*\*`/);
assert.match(turnActivity, /summaryIndex === null \|\| contentIndex !== null \|\| !message\.text\.trim\(\)/);
assert.match(turnActivity, /if \(summaryIndex === null \|\| contentIndex !== null\) continue;/);
assert.match(turnActivity, /delete safe\.content;/);
assert.match(turnActivity, /delete safe\.content_index;/);
assert.match(turnActivity, /delete safe\.contentIndex;/);
assert.doesNotMatch(turnActivity, /return activityFamily\(message\) === "reasoning" \? \[\] : \[message\]/);
assert.doesNotMatch(turnActivity, /dedupeExplorationEntries|seenReadPaths/);
assert.doesNotMatch(turnActivity, /singleActivityStandsAlone/);
assert.doesNotMatch(turnActivity, /latestReasoningActionTitle/);
assert.match(turnActivity, /className="octopus-agent-activity-toggle cursor-interaction"/);
assert.match(turnActivity, /if \(message\.eventKind === "interrupted_result"\) return true;/);
assert.match(turnActivity, /const \[historyDisclosureState, setHistoryDisclosureState\] = useState<DisclosureState>\("collapsed"\);/);
assert.match(turnActivity, /const historyExpanded = historyDisclosureState === "opening" \|\| historyDisclosureState === "expanded";/);
assert.match(turnActivity, /const historyFullyExpanded = historyDisclosureState === "expanded";/);
assert.match(turnActivity, /if \(historyExpanded\) \{\s*setHistoryDisclosureState\(reducedMotion \? "collapsed" : "closing"\);/);
assert.match(turnActivity, /if \(historyDisclosureState === "closing" \|\| reducedMotion\) \{\s*setHistoryDisclosureState\("expanded"\);/);
assert.match(turnActivity, /setHistoryDisclosureState\("opening"\);[\s\S]*?requestAnimationFrame[\s\S]*?current === "opening" \? "expanded" : current/);
assert.match(turnActivity, /\{historyDisclosureState !== "collapsed" \? \([\s\S]*?animate=\{historyFullyExpanded \? \{ height: "auto", opacity: 1 \} : \{ height: 0, opacity: 0 \}\}/);
assert.match(turnActivity, /aria-hidden=\{!historyFullyExpanded\}[\s\S]*?inert=\{!historyFullyExpanded \|\| undefined\}/);
assert.match(turnActivity, /onAnimationComplete=\{\(\) => \{\s*setHistoryDisclosureState\(\(current\) => current === "closing" \? "collapsed" : current\);/);
assert.doesNotMatch(turnActivity, /setHistoryExpanded|useState\(false\);\s*const \[, setClock\]/);
assert.match(turnActivity, /\{finalStarted \? <ActivityGap \/> : null\}/);
assert.doesNotMatch(turnActivity, /preserveDisclosurePosition/);
assert.match(turnActivity, /!turnFinished \|\| !isSafetyBufferingWarning\(message\)/);
assert.match(turnActivity, /record\(message\.eventData\)\.showBufferingUi === true/);
assert.match(turnActivity, /entry\.tone = strongerActivityTone\(entry\.tone, activityTone\(\[message\]\)\);/);
assert.doesNotMatch(turnActivity, /entry\.tone = activityTone\(entry\.messages\);/);

assert.match(conversationTrace, /"turn_control",\s*"interrupted_result",/);
assert.match(conversationTrace, /if \(originRole !== "provider"\) return "hidden";/);
assert.match(chatRuns, /\| "interrupted_result"/);
assert.match(chatRuns, /"turn_control", "interrupted_result", "rate_limits"/);
assert.match(chatRuns, /\.sort\(\(a, b\) => a\.createdAtMs - b\.createdAtMs \|\| a\.movementSeq - b\.movementSeq\)/);
assert.match(chatRuns, /if \(eventKind === "message_delta" \|\| eventKind === "commentary_delta"\) return "text";[\s\S]*?\["reasoning_delta", "plan_delta", "tool_output_delta", "file_change_delta"\]\.includes\(eventKind\)\) return "delta";/);
assert.match(electronProjection, /const projectedText = projected\.eventKind\.endsWith\("_delta"\)\s*\? projected\.text\s*:\s*projected\.eventKind === "compaction"\s*\? ""\s*:\s*projected\.text \|\| operatorChatTurnText\(projected\.payload, "out"\) \|\| label;/);
assert.match(electronProjection, /if \(label === "agent\.message\.delta"\) \{\s*return phase === "commentary" \? "commentary_delta" : "message_delta";/);
assert.match(electronProjection, /"turn\.reasoning\.delta": "reasoning_delta"/);
assert.match(electronProjection, /function compareMovementOrder\(left, right\) \{\s*return rowTimeMs\(left\) - rowTimeMs\(right\)\s*\|\| numberValue\(left\.seq, 0\) - numberValue\(right\.seq, 0\);/);
assert.match(electronProjection, /function isOperatorChatProviderFailureRow\(entry, row\)[\s\S]*?ledgerArtifactLabel\(row\) === `\$\{provider\}\.failure`;/);
assert.match(electronProjection, /if \(isOperatorChatProviderFailureRow\(entry, row\)\) \{[\s\S]*?open = false;[\s\S]*?state = "failed";/);
assert.match(electronProjection, /visibleLabels\.has\(label\) \|\| isOperatorChatProviderFailureRow\(entry, row\)/);
assert.match(electronProjection, /const terminalError = Boolean\(providerFailure\) \|\| label === protocol\.errorOutputLabel;/);

assert.match(chatWorkspace, /const activeRun = projectedActiveRun;/);
assert.match(chatWorkspace, /const turnOpen = Boolean\(activeTurnControlTarget\);/);
assert.match(chatWorkspace, /projectedActiveRun\?\.turnOpen \|\| operatorTurnResumeAcknowledgedOpen\(messages, projectedActiveRun\?\.lastMovementSeq \?\? -1\)/);
assert.match(chatWorkspace, /const replyOpen = activeRunInFlight \|\| turnOpen;/);
assert.match(chatWorkspace, /const resumeAvailable = Boolean\([\s\S]*?operatorTurnResumeAvailable\(messages, turnOpen\)/);
assert.match(chatWorkspace, /reconcileChatRunReceipts\(run, fresh\)/);
assert.doesNotMatch(chatWorkspace, /\[runId\]: \{ interfaceId: latest\.interface_id, runId \}/);
assert.match(chatWorkspace, /if \(isChatReplyTerminal\(latest\.event_kind, latest\.event_data, latest\.terminal\)\) \{\s*setPendingTurnControls\(\(current\) => \{[\s\S]*?delete next\[runId\];/);
const resumeAction = sourceBetween(chatWorkspace, "async function resumeActiveTurn()", "function reorderQueuedTurns(");
assert.match(resumeAction, /const controlId = `control_\$\{randomId\(\)\}`;/);
assert.match(resumeAction, /submitOperatorTurnControl\(\{[\s\S]*?interfaceId: run\.chatInterfaceId,[\s\S]*?runId: run\.runId,[\s\S]*?action: "resume",[\s\S]*?controlId/);
assert.match(resumeAction, /queuedTurnsRef\.current\.some[\s\S]*?replacePausedQueueRunKeys/);
assert.doesNotMatch(resumeAction, /submitOperatorTurn\(|executeTurnSequence\(|resumeQueuedTurns\(|setText\(|operator\.message/);
assert.match(chatWorkspace, /const ready = queueDispatchCandidates\(queueStore\.snapshot, runs\);[\s\S]*?executeTurnSequence\(queuedSubmission\(item\)\)/);
assert.match(chatWorkspace, /loadChatRunMessages\(turn\.runId\)[\s\S]*?loadChatRunStatuses\(\[turn\.runId\]\)[\s\S]*?!queueTurnReady\(claimed, status, history\)/);
assert.match(clientTurnQueueSource, /export function queueTurnReady\([\s\S]*?status\.runId !== turn\.runKey[\s\S]*?turn\.resumeAfterInterrupt === true \|\| !operatorTurnResumeAvailable\(history, false\)/);
const queuedAfterReply = sourceBetween(chatWorkspace, "const queued = nextQueuedTurn(submittedRunKey);", "} catch (failure)");
assert.match(queuedAfterReply, /const resumeRequired = operatorTurnResumeAvailable\(liveMessages, false\);[\s\S]*?if \(queued && resumeRequired\)[\s\S]*?await replacePausedQueueRunKeys[\s\S]*?current = null;/);
assert.match(operatorChatClient, /if \(!\["interrupt", "resume", "stop"\]\.includes\(action\)\)/);
assert.match(operatorChatClient, /const payload = \{ action, control_id: controlId \};/);
assert.match(electronProjection, /outcome\?\.status === "acknowledged" && outcome\.action === "resume"[\s\S]*?open = true;[\s\S]*?state = "working";/);
assert.match(electronMain, /maxPayloadBytes: input\?\.label === "turn\.stream" \? 256 \* 1024 \* 1024 : undefined/);
assert.match(chatWorkspace, /if \(activeRunId\) void refreshRuns\(\);[\s\S]*?\[activeRunId, refreshRuns\]/);
assert.match(chatWorkspace, /projectionChange\.kind !== "ledger"\) return;[\s\S]*?loadChatRunStatuses\(runIds\)[\s\S]*?turnState: status\.turnState/);
assert.doesNotMatch(chatWorkspace, /projectionChange\.kind === "ledger"\) return;/);
assert.match(electronProjection, /case "projection\.operator\.chats\.status":[\s\S]*?operatorChatStatuses\(projection, params\)/);
assert.match(electronProjection, /operatorChatStatusCursor[\s\S]*?operatorChatLedgerDelta[\s\S]*?reduceOperatorChatTurnState/);
assert.match(chatWorkspace, /\[submitted\.runId\]: \{\s*interfaceId: submitted\.chatInterfaceId,\s*runId: submitted\.runId,/);
assert.match(chatView, /turnOpen=\{chat\.turnOpen\}/);
assert.match(chatView, /responseStreaming=\{chat\.busy\}/);
assert.match(chatView, /conversationId=\{chat\.activeRunId\}/);
assert.doesNotMatch(chatView, /key=\{chat\.activeRunId \|\| "new-chat"\}/);
assert.doesNotMatch(chatView, /ThreadLoadingStatus|chat\.loadingMessages \? <ThreadLoadingStatus/);
const selectRunStart = chatWorkspace.indexOf("  function selectRun(runId: string)");
const selectRunEnd = chatWorkspace.indexOf("  function createChat", selectRunStart);
assert.notEqual(selectRunStart, -1);
assert.notEqual(selectRunEnd, -1);
assert.match(chatWorkspace.slice(selectRunStart, selectRunEnd), /if \(runId === activeRunIdRef\.current\) return;/);
assert.match(chatWorkspace.slice(selectRunStart, selectRunEnd), /setActiveRunId\(runId\);\s*setMessages\(cachedMessages \?\? \[\]\);\s*setLoadingMessages\(cachedMessages == null\);/);
assert.doesNotMatch(chatWorkspace, /runSelectionGenerationRef|commitRunSelection/);
assert.match(chatWorkspace, /: loadingMessages\s*\? "Task history is still loading\."/);
assert.doesNotMatch(chatWorkspace, /interfaceRolesById/);
assert.match(chatActivityProjectionSource, /interface_role: artifact\.from_interface_role/);
assert.doesNotMatch(chatCapabilityCatalog, /projection\.interfaces\.list/);
assert.match(chatWorkspace, /const startingNewTask = !turn\.runId;/);
assert.match(chatWorkspace, /work_status: startingNewTask \? "submitting" : "working"/);
assert.doesNotMatch(chatWorkspace, /Interface provenance projection is not ready/);
assert.match(chatCapabilityCatalog, /capabilityCatalogScopeRef\.current !== scope/);
assert.doesNotMatch(chatCapabilityCatalog, /let cancelled = false;\s*setExecutionContextsLoaded\(false\);\s*setInterfaceRolesById\(null\);/);
assert.match(electronProjection, /segments\[0\] === "state" && segments\[1\] === "runs"[\s\S]*?\? "runs" : null/);
assert.match(electronProjection, /async function cachedEePolicyProjection\(\) \{[\s\S]*?while \(true\)[\s\S]*?build\.generation === policyProjectionGeneration/);
assert.match(electronProjection, /const workerEnvironmentKind = stringValue\(formation\.worker_environment_kind\);[\s\S]*?validDirectCoordinates = workerEnvironmentKind === "process_session"/);

assert.match(messageList, /kind\.movePath \?\? kind\.move_path/);
assert.match(messageList, /path: activityFileChangePath\(change\)/);
assert.match(messageList, /flex w-full min-w-0 flex-col items-stretch/);
assert.match(messageList, /effectiveActiveMessageCount === 0 && emptyState === "new-task"/);
assert.match(messageList, /streamKey=\{streaming \? message\.id : undefined\} streaming=\{streaming\}/);
assert.match(messageList, /const MemoizedChatTurn = memo\(ChatTurn, chatTurnPropsEqual\);/);
assert.match(messageList, /const turns = useStableTurns\(renderMessages, activityPresentation === "conversation"\);/);
assert.match(messageList, /projectionValueEqual\(previous\.eventData, next\.eventData\)/);
assert.doesNotMatch(messageList, /previous\.id\.startsWith\("stream:"\)/);
assert.match(messageList, /className="pointer-events-none absolute inset-x-0 bottom-0 z-10 pb-4"/);
assert.match(messageList, /glyph=\{dynamicToolGlyph\(presentation\)\}/);
assert.doesNotMatch(messageList, /name="plugins" \/>[\s\S]{0,180}\$\{lifecycle === "active" \? "Calling" : "Called"\}/);
assert.match(
  messageList,
  /const reasoningStreaming = kind === "reasoning_delta";[\s\S]*?streamKey=\{reasoningStreaming \? message\.id : undefined\}[\s\S]*?streaming=\{reasoningStreaming\}[\s\S]*?variant="reasoning"/,
);
assert.match(
  messageList,
  /if \(kind === "commentary" \|\| kind === "commentary_delta"\)[\s\S]*?const commentaryStreaming = kind === "commentary_delta";[\s\S]*?streamKey=\{commentaryStreaming \? message\.id : undefined\}[\s\S]*?streaming=\{commentaryStreaming\}/,
);
assert.match(messageList, /\{!streaming && message\.terminal !== false \? \([\s\S]*?<MessageActions/);
assert.doesNotMatch(messageList, /message\.latencyMs \? formatLatency/);
assert.match(messageList, /readThreadRestoreState\(conversationId\)/);
assert.match(messageList, /key=\{conversationId \|\| "new-task"\}/);
assert.match(messageList, /initialRestoreState=\{initialVirtualizerRestoreState\}/);
const presentationScrollEffectStart = messageList.indexOf("    if (skipNextPresentationScrollRef.current) {");
const presentationScrollEffectEnd = messageList.indexOf("  }, [presentationMessageCount]);", presentationScrollEffectStart);
assert.notEqual(presentationScrollEffectStart, -1);
assert.notEqual(presentationScrollEffectEnd, -1);
assert.doesNotMatch(
  messageList.slice(presentationScrollEffectStart, presentationScrollEffectEnd),
  /setReverseScrollDistance\(/,
  "streaming deltas must not fight user scrolling with an imperative scroll-to-latest",
);
assert.doesNotMatch(messageList, /const presentationSignal = useMemo/);
assert.match(messageList, /flushSync\(\(\) => setMeasuredHeightsByKey\(next\)\)/);
assert.match(messageList, /const turnKeyByElementRef = useRef\(new Map<Element, string>\(\)\)/);
assert.match(messageList, /if \(!virtualized\) \{[\s\S]*?<MeasuredVirtualTurn/);
assert.match(messageList, /distanceFromBottomAnchoredToTurn\(\{/);
assert.match(messageList, /aria-hidden[\s\S]*?h-\[calc\(var\(--thread-scroll-padding-bottom\)-var\(--spacing\)\*4\)\]/);
assert.match(messageList, /className="pointer-events-none absolute inset-x-0 bottom-0 z-10 pb-4"[\s\S]*?data-thread-scroll-footer="true"/);
assert.doesNotMatch(turnActivity, /`Called \$\{label\}`/);
assert.match(turnActivity, /if \(entry\.family === "mcp" \|\| entry\.family === "hooks"\) return "plugins";/);
assert.match(turnActivity, /return dynamicToolGlyph\(presentation\);/);
assert.match(source("src/components/voice/HomeAgentActivityView.tsx"), /<ChatView[\s\S]*?workspace=\{workspace\}/);
assert.match(messageList, /turnInProgress=\{responseStreaming && isLatestTurn\}/);
assert.match(turnActivity, /turnInProgress \?\? \(/);
assert.doesNotMatch(source("src/components/chat/CollapsibleMessageBody.tsx"), /motion|maxHeight:.*contentHeight/);
assert.doesNotMatch(messageList, /className="sticky bottom-0 z-10 mt-auto w-full pb-4" data-thread-scroll-footer="true" ref=\{footerRef\}/);
assert.match(chatView, /if \(chat\.loadingMessages\) \{[\s\S]*?<OctopusLoadingState className="surface-content-loading-state" label="Loading task"/);
assert.doesNotMatch(chatView, /emptyState=\{chat\.loadingMessages/);
assert.doesNotMatch(messageList, /TaskOpeningIndicator/);
assert.match(messageList, /eventString\(data\.source\) === "model_safety_buffering"/);
assert.match(messageList, /No action is required\. Octopus will keep waiting for the response\./);
assert.match(surfaceHost, /surface\.rendererKind === "builtin\.chat" \|\| surface\.rendererKind === "builtin\.voice"[\s\S]*?<OctopusLoadingState className="surface-content-loading-state"/);

assert.doesNotMatch(operatorShell, /secondaryNewTaskTransitioning|secondary-chat:/);
assert.match(operatorShell, /<SurfaceHost\s+commandsActive=\{secondaryCommandsActive\}\s+key=\{secondarySurface\.id\}/);
assert.doesNotMatch(operatorShell, /setSecondaryWorkspaceKey|setSecondarySurfaceId/);
assert.match(operatorShell, /const activeClientSurfaceIdentity = useMemo\(\(\) => deriveActiveClientSurfaceIdentity\(\{[\s\S]*?activePane: primaryPaneBlocked && secondaryPaneOpen \? "secondary" : activePane,[\s\S]*?target: primarySurfaceTarget,[\s\S]*?target: activeSecondaryTab\?\.target \?\? null,[\s\S]*?workspaceActiveRunId: secondaryChatWorkspace\.activeRunId/);
assert.doesNotMatch(operatorShell, /deriveActiveClientSurfaceIdentity\([\s\S]{0,800}?secondaryPaneFocused/);
assert.doesNotMatch(operatorShell, /\bcommandPane\b|\bsetCommandPane\b/);
assert.match(operatorShell, /const secondaryCommandsActive = !settingsOpen && activeClientSurfaceIdentity\.pane === "secondary" && activeClientSurfaceIdentity\.ready/);
assert.match(operatorShell, /activeChatRunId=\{primarySurfaceTarget\.runId\}/);
assert.match(operatorShell, /if \(!state \|\| state\.id === activeSecondaryTabId && !secondaryRunIdentityReady \|\| !transferableWorkspacePaneTarget\(state\.target\)\) return \[\];/);
assert.match(operatorShell, /const runId = activeClientSurfaceIdentity\.runId;\s*if \(!runId\) return;/);
assert.match(operatorShell, /secondarySurface\.rendererKind === "builtin\.chat" && !secondaryRunIdentityReady[\s\S]*?<SurfaceLoadingState label="Opening task…"/);
assert.match(operatorShell, /activeSurface\.rendererKind === "builtin\.chat" && !primaryRunIdentityReady[\s\S]*?<SurfaceLoadingState label="Opening task…"/);
assert.match(operatorShell, /if \(activeTab\.target\.runId\) \{\s*newSecondaryTaskPhaseRef\.current = "idle";\s*return;\s*\}/);
assert.doesNotMatch(operatorShell, /continuedTab/);
assert.match(operatorShell, /newSecondaryTaskPhaseRef\.current !== "awaiting_run" \|\| !runId[\s\S]*?const completedTab = secondaryPaneTab[\s\S]*?secondaryTabModel\.replaceTab\(activeTab\.id, completedTab\)/);
assert.doesNotMatch(operatorShell, /contextBoundaryRunReplacements|replaceClientRunIds|migrateClientTaskOrganizationRunId/);
assert.doesNotMatch(chatWorkspace, /CONTEXT_BOUNDARY_CONTINUE_PROMPT|waitForProjectedContextBoundarySuccessor|runReplacement/);
assert.doesNotMatch(electronProjection, /operatorChatContextBoundary|context_boundary_count|task_root_run_id/);
assert.match(electronProjection, /"turn\.context\.compaction": "compaction"/);
assert.match(operatorChatClient, /"turn\.context\.compaction": "compaction"/);
assert.match(operatorShell, /if \(!surface\) return surfaceRegistry\.loading;/);
assert.match(operatorShell, /target\.surfaceId !== "chat"[\s\S]*?!chatWorkspace\.runsLoaded[\s\S]*?chatWorkspace\.runs\.some/);
assert.match(operatorShell, /defaultSidebarOpen=\{!clientWindow\.initialTarget\}/);
assert.match(operatorShell, /titlebarSurfaceContext=\{[\s\S]*?title: activeSurface\.label/);
assert.doesNotMatch(operatorShell, /titlebarSurfaceContext=\{activeClientSurfaceIdentity\.pane === "secondary"/);
const titlebarTransferOwner = sourceBetween(operatorShell, "  const titlebarTransferTarget = useMemo", "  const [selectedRef");
assert.match(titlebarTransferOwner, /!paneOwnsWindowTitlebar\(activeClientSurfaceIdentity\.pane\) \|\| secondaryPaneOpen/);
assert.doesNotMatch(titlebarTransferOwner, /activeSecondaryTab|secondaryTabs/);
assert.match(operatorShell, /openClientWindow[\s\S]*?Move to New Window/);
assert.match(operatorShell, /CLIENT_PANE_DRAG_MIME[\s\S]*?parseClientPaneDragPayload/);
assert.match(operatorShell, /onTabWindowPointerDown=\{\(tab, event\) => secondaryWindowPointerDown\(tab\.id, event\)\}/);
assert.match(operatorShell, /window\.addEventListener\("storage", onStorage\)/);
assert.match(clientPreferencesSource, /ensureClientPreferencesStorageListener\(\)[\s\S]*?window\.addEventListener\("storage"/);
assert.match(taskActionsController, /id: "open", label: "Open in new window"[^\n]*onSelect:[^\n]*onOpenChatInNewWindow\(runId\)/);
const taskDropBranchStart = sidebar.indexOf("if (target.containerId === TASKS_CONTAINER)");
const taskDropBranchEnd = sidebar.indexOf("\n    }\n  };", taskDropBranchStart);
assert.notEqual(taskDropBranchStart, -1);
assert.notEqual(taskDropBranchEnd, -1);
assert.doesNotMatch(sidebar.slice(taskDropBranchStart, taskDropBranchEnd), /assignChatToProject/);
assert.match(sidebarDnd, /createPortal\([\s\S]*?<DragOverlay[\s\S]*?document\.body/);
assert.match(sidebarDnd, /transform: CSS\.Transform\.toString\(isDragging \? null : transform\)/);
assert.doesNotMatch(sidebarDnd, /opacity: isDragging/);
assert.match(codexNav, /titleActionsOnHover = false/);
assert.match(codexNav, /text-base font-medium text-\[var\(--color-text-tertiary\)\] opacity-75/);
assert.doesNotMatch(codexNav, /titleClassName \?\? "text-base font-medium text-token-input-placeholder-foreground opacity-75"/);
assert.match(codexNav, /pointer-events-none opacity-0 group-focus-within\/nav-section-title:pointer-events-auto/);
assert.match(codexNav, /transition=\{\{ duration: reduceMotion \? 0 : 0\.3, ease: \[0\.19, 1, 0\.22, 1\] \}\}/);
assert.doesNotMatch(sidebar, /function SectionToggle\(\{\s*action/);
assert.doesNotMatch(sidebar, /group\/section-chevron/);
assert.match(sidebar, /function SidebarSectionTitle\([\s\S]*?<div className="flex min-w-0 flex-1">\s*<SectionToggle/);
assert.match(sidebar, /function SectionToggle\([\s\S]*?return \(\s*<button/);
assert.match(sidebar, /group\/section-toggle flex min-w-0 flex-1 cursor-interaction items-center gap-1 rounded-md py-0\.5 pr-1/);
assert.match(sidebar, /titleActions=\{\([\s\S]*?Create agent[\s\S]*?label="Agents"/);

assert.match(chatCss, /\.octopus-agent-activity-toggle \{[\s\S]*?position: absolute;[\s\S]*?inset: 0;/);
assert.match(messageTooltip, /data-agent-activity-file-link=\{variant === "activity-target" \? true : undefined\}/);
assert.match(messageTooltip, /openEpoch !== openEpochRef\.current/);
assert.doesNotMatch(messageTooltip, /autoUpdateFrameRef/);
assert.match(messageTooltip, /const interactionRef = useRef\(\{ focus: false, hoverLatched: false \}\);/);
assert.doesNotMatch(messageTooltip, /onPointerEnter=/);
assert.match(messageTooltip, /onPointerMove: \(event\) => \{\s*if \(inactive \|\| event\.pointerType === "touch" \|\| interactionRef\.current\.hoverLatched\) return;\s*interactionRef\.current\.hoverLatched = true;\s*scheduleOpen\(\);/);
assert.match(messageTooltip, /onPointerLeave: \(\) => \{\s*interactionRef\.current\.hoverLatched = false;\s*close\(\);/);
assert.match(messageTooltip, /onBlur: \(\) => \{\s*interactionRef\.current\.focus = false;\s*close\(\);/);
assert.match(messageTooltip, /onClick: \(event\) => \{\s*if \(!onActivate\) return;\s*event\.stopPropagation\(\);\s*onActivate\(\);/);
assert.match(messageTooltip, /onClickCapture: close/);
assert.match(messageTooltip, /onPointerDownCapture: close/);
assert.doesNotMatch(messageTooltip, /onClick=\{onActivate \?/);
assert.match(messageTooltip, /window\.addEventListener\("blur", close\);[\s\S]*?document\.addEventListener\("scroll", close, true\);/);
const tooltipCloseStart = messageTooltip.indexOf("const close = useCallback");
const tooltipCloseEnd = messageTooltip.indexOf("  useEffect", tooltipCloseStart);
assert.notEqual(tooltipCloseStart, -1);
assert.notEqual(tooltipCloseEnd, -1);
assert.doesNotMatch(messageTooltip.slice(tooltipCloseStart, tooltipCloseEnd), /hoverLatched/);
assert.match(paneTabStrip, /motionRef\.current\?\.scrollIntoView/);
assert.match(paneTabStrip, /useSortable\(/);
assert.match(paneTabStrip, /onWindowPointerDown\(event\)/);
assert.doesNotMatch(paneTabStrip, /onNativeDragEnd|onTabNativeDragStart/);
assert.match(paneTabStrip, /onAuxClick/);
assert.match(paneTabStrip, /inert=\{/);
assert.match(source("src/components/shell/paneTabs.css"), /\.pane-tab-title-text\.is-overflowing \{[\s\S]*?mask-image: linear-gradient/);
assert.match(workspaceSurfacePane, /<PaneTabStrip[\s\S]*?\bstretch\s+tabs=/);
assert.match(workspaceSurfacePane, /onTabWindowPointerDown=\{onTabWindowPointerDown\}/);
assert.match(workspaceSurfacePane, /const tabStripVisible = secondaryPaneUsesTabStrip\(tabs\);/);
assert.doesNotMatch(workspaceSurfacePane, /data-pane-compact-tab-id|compactTabHandle|compactTabActions/);
assert.match(workspaceSurfacePane, /<WorkspaceCompactTabProvider value=\{compactTabDrag\}>/);
assert.match(compactTabContext, /createContext<WorkspaceCompactTabDrag \| null>/);
assert.match(workspaceSurfacePane, /!controlsInWindowHeader && showTabStrip \? header/);
assert.match(workspaceSurfacePane, /createPortal\(header, windowControlsHost\)/);
assert.match(workspaceSurfacePane, /workspace-pane-header-actions no-drag/);
assert.match(workspaceSurfacePane, /<WorkspacePaneHeaderActionsProvider host=\{controlsInWindowHeader \|\| tabStripVisible \? headerActionsHost : null\}>/);
assert.match(paneHeaderActions, /return host \? createPortal\(children, host\) : null;/);
assert.match(surfaceHost, /headerMode=\{paneRole === "primary" \? "window" : paneTabStripVisible \? "none" : "pane"\}/);
assert.match(operatorShell, /const secondaryPaneTabStripVisible = secondaryPaneUsesTabStrip\(secondaryPaneTabs\);/);
assert.match(operatorShell, /showWhenSingle: target\.surfaceId !== "chat"/);
assert.match(chatView, /headerMode === "pane" \? <ChatThreadHeader[\s\S]*?mode=\{headerMode\}/);
assert.match(chatView, /<WorkspacePaneHeaderActions>[\s\S]*?<ChatThreadHeaderActions/);
assert.match(chatThreadHeader, /dispatchOpenClientTaskActionsMenu\(activeRun\.runId, event\.currentTarget\)/);
assert.match(chatThreadHeader, /export function ChatThreadHeaderActions[\s\S]*?aria-label="Task actions"/);
assert.match(chatThreadHeader, /onPointerDown=\{mode === "pane" \? compactTab\?\.onPointerDown : undefined\}/);
assert.doesNotMatch(chatThreadHeader, /name="chat"/);
assert.match(chatThreadHeader, /data-task-summary-trigger="true"[\s\S]*?name="taskSummary"/);
assert.doesNotMatch(chatThreadHeader, /data-task-summary-trigger="true"[\s\S]{0,500}?name="viewAll"/);
assert.match(chatThreadHeader, /h-token-button-composer shrink-0[\s\S]*?rounded-lg[\s\S]*?disabled:cursor-default/);
assert.equal((chatThreadHeader.match(/chat-thread-header-action-slot no-drag pointer-events-auto flex shrink-0 items-center gap-px/g) ?? []).length, 3);
assert.match(chatThreadHeader, /summaryDisplayMode === "overlay" \? "Toggle summary" : "Toggle pinned summary"/);
assert.match(chatThreadHeader, /<OcPopover[\s\S]*?align="end"[\s\S]*?className="task-summary-popover"[\s\S]*?open=\{summaryOpen\}[\s\S]*?placement="bottom"/);
assert.match(shellGlyphs, /taskSummary:/);
assert.match(shellGlyphs, /taskSummary: \{\s*viewBox: "0 0 20 20"/);
assert.match(shellGlyphs, /taskPlan:/);
assert.match(shellGlyphs, /taskPlan: \{\s*viewBox: "0 0 16 16"/);
assert.match(taskSummaryPanel, /className="task-summary-row task-summary-plan"[\s\S]*?name="taskPlan"/);
assert.match(taskSummaryPanel, /className="task-summary-row task-summary-show-all"[\s\S]*?name="viewAll"/);
assert.doesNotMatch(taskSummaryPanel, /sourcesOpen|CompleteSourcesPanel|projectedGoal/);
assert.match(taskSummaryPanel, /collectTaskSources\(messages\)/);
assert.doesNotMatch(taskSummaryPanel, /onOpenEnvironment\(environmentTarget\.id\)/);
assert.match(chatCss, /--thread-floating-content-top-inset: calc\(var\(--height-toolbar, 46px\) \+ 8px\)/);
assert.match(chatCss, /\.task-summary-surface \{[\s\S]*?z-index: 40;[\s\S]*?pointer-events: none;/);
assert.match(chatCss, /\.task-summary-motion \{[\s\S]*?width: 300px;/);
assert.match(chatCss, /\.task-summary-popover \.task-summary-popover-panel \{[\s\S]*?top: calc\(100% \+ 16px\);[\s\S]*?right: 0;[\s\S]*?width: 300px;[\s\S]*?max-height: calc\(100vh - 61px\);[\s\S]*?border: 0;[\s\S]*?background: transparent;[\s\S]*?box-shadow: none;[\s\S]*?transform-origin: top right;/);
assert.doesNotMatch(chatCss, /margin-right: 316px|\.chat-run-layout\.is-summary-gutter|\.chat-run-layout\.is-summary-shift/);
assert.match(chatCss, /\.task-summary-shell \{[\s\S]*?border-radius: 20px;[\s\S]*?background: var\(--color-token-dropdown-background/);
assert.match(chatView, /const summaryContentX = summaryOpen && summaryDisplayMode === "shift" \? -158 : 0;/);
assert.match(chatView, /contentX=\{summaryContentX\}/);
assert.doesNotMatch(chatView, /summaryPopoverRef|is-summary-gutter|is-summary-shift/);
assert.equal((messageList.match(/animate=\{\{ x: contentX \}\}/g) ?? []).length, 2);
assert.equal((messageList.match(/--thread-wide-block-inline-shift/g) ?? []).length, 2);
assert.match(shellCss, /\.workspace-pane-header-actions \{[\s\S]*?gap: 0;/);
assert.match(taskSourcesPane, /OPEN_TASK_SOURCES_PANE_EVENT = "octopus:open-task-sources-pane"/);
assert.match(taskSourcesPaneTab, /className="task-sources-panel"[\s\S]*?aria-label="Sources"/);
assert.match(operatorShell, /workspaceTaskSourcesPaneTab\("octopus-os", target\)/);
assert.match(operatorShell, /target\.kind === "task_sources"[\s\S]*?showWhenSingle: true/);
assert.match(operatorShell, /OPEN_TASK_SOURCES_PANE_EVENT[\s\S]*?openTaskSourcesPane\(detail\)/);
assert.match(operatorShell, /activeSecondaryTab\?\.target\.kind === "task_sources"[\s\S]*?<TaskSourcesPaneTab/);
assert.match(operatorShell, /collectTaskSources\(workspace\.renderedMessages\.filter/);
assert.ok(
  operatorShell.indexOf("const activeTaskSources = useMemo") < operatorShell.indexOf("if (!activeSurface) {\n    return null;\n  }"),
  "task sources hooks must run before OperatorShell's conditional return",
);
assert.match(operatorShellState, /open: state\.open && tabs\.length > 0/);
assert.match(taskActionsMenu, /OPEN_CLIENT_TASK_ACTIONS_MENU_EVENT[\s\S]*?subscribeOpenClientTaskActionsMenu/);
assert.match(source("src/components/shell/AppShell.tsx"), /<TaskActionsController[\s\S]*?onOpenChatInNewWindow=\{onOpenChatInNewWindow\}/);
assert.match(taskActionsController, /export function TaskActionsController\([\s\S]*?subscribeOpenClientTaskActionsMenu[\s\S]*?setMenu\(\(previous\)/);
assert.match(messageList, /distanceFromTop = list \? Math\.max\(0, list\.scrollHeight - list\.clientHeight \+ list\.scrollTop\) : 0/);
assert.match(chatCss, /--thread-content-top-inset: calc\(var\(--height-toolbar, 46px\) \+ var\(--spacing\) \* 8\)/);
assert.match(chatCss, /\.chat-thread-top-fade\[data-app-shell-main-content-top-fade="visible"\]/);
assert.doesNotMatch(chatView, /RunEnvironmentView|TaskContextPopover|chat-environment-panel/);
assert.match(operatorShell, /projectAssistantCodeComments\(messages, runId\)/);
assert.match(operatorShell, /<RetainedWorkerEnvironmentTabs[\s\S]*?assistantCodeComments=\{activeWorkerEnvironmentAssistantCodeComments\}/);
assert.match(source("src/components/shell/RetainedWorkerEnvironmentTabs.tsx"), /assistantCodeComments=\{active && tab.id === activeTabId \? assistantCodeComments : undefined\}/);
assert.match(source("src/components/shell/RetainedWorkerEnvironmentTabs.tsx"), /<WorkerEnvironmentPaneTab[\s\S]*?assistantCodeComments=\{assistantCodeComments\}/);
assert.match(workerEnvironmentPaneTab, /<WorkerEnvironmentSurfacePanel[\s\S]*?assistantCodeComments=\{assistantCodeComments\}/);
assert.match(runEnvironmentView, /export function WorkerEnvironmentSurfacePanel[\s\S]*?<EnvironmentPanelContent[\s\S]*?assistantCodeComments=\{assistantCodeComments\}/);
assert.match(workspaceSurfacePane, /controlsInWindowHeader && windowControlsHost \? createPortal/);
assert.match(workspaceSurfacePane, /data-pane-manager-controls="true" data-pane-manager-id=\{managerId\}/);
assert.doesNotMatch(workspaceSurfacePane, /workspace-surface-pane-header-(?:tail-)?spacer/);
assert.match(shellCss, /\.workspace-surface-pane-header \{[\s\S]*?-webkit-app-region: drag;/);
assert.match(source("src/components/shell/paneTabs.css"), /\.pane-tabs \{[\s\S]*?-webkit-app-region: no-drag;/);
assert.match(shellCss, /\.workspace-surface-pane\.has-tab-strip \.workspace-surface-pane-content \.surface-titlebar-spacer \{\s*display: none;/);
assert.doesNotMatch(shellCss, /\.workspace-pane-compact-tab|\.workspace-surface-pane\.has-compact-tab/);
assert.match(appShell, /defaultSidebarOpen: boolean;/);
assert.match(appShell, /readSidebarOpen\(accessProfile, clientWindowScope, defaultSidebarOpen\)/);
assert.match(appShell, /return stored === null \? defaultOpen : stored !== "false";/);
assert.match(appShell, /primaryPaneBlocked=\{primaryPaneBlocked\}[\s\S]*?surfaceActions=\{titlebarSurfaceActions\}[\s\S]*?surfaceTitle=\{titlebarSurfaceTitle\}/);
assert.match(appShell, /secondaryFocused=\{secondaryPaneFocused\}/);
assert.match(voiceView, /: working\s*\? true\s*: finalVisible/);
assert.doesNotMatch(voiceView, /isCompactHomeActivity/);
assert.match(voiceCss, /\.home-response-stage\.is-compact \.home-response-output--live \{[\s\S]*?text-align: center;/);
assert.doesNotMatch(voiceCss, /\.home-task-activity|\.home-view-transition__activity/);
assert.match(chatCss, /:hover:not\(:has\(\[data-agent-activity-file-link\]:hover\)\)/);
assert.match(chatCss, /--color-token-conversation-header: color-mix\(in oklab, var\(--color-token-foreground\) 30%, transparent\);/);
assert.doesNotMatch(turnActivity, /octopus-agent-activity-viewport vertical-scroll-fade-mask/);
assert.doesNotMatch(turnActivity, /octopus-reasoning-detail vertical-scroll-fade-mask/);
assert.doesNotMatch(chatCss, /\.octopus-agent-activity-viewport \{[^}]*max-height:/);
assert.doesNotMatch(chatCss, /\.octopus-reasoning-detail \{[^}]*max-height:/);
assert.match(chatCss, /\.octopus-agent-activity-detail-item > \* \{\s*width: 100%;/);
assert.match(chatCss, /\.octopus-exec-activity \{[\s\S]*?width: 100%;/);

assert.match(iconGlyphs, /\| "users"/);
assert.deepEqual(glyphAsset("users").shapes, ["users"]);

assert.match(appIndex, /class="startup-loader"/);
assert.match(appIndex, /<img alt="" class="startup-loader__base" src="\.\/octopus-logo\.png" \/>/);
assert.match(appIndex, /startup-octopus-shimmer 2200ms cubic-bezier\(0\.4, 0, 0\.2, 1\) infinite/);
assert.match(app, /<OctopusLoadingState className="app-loading-state" label="Opening Octopus"/);
assert.match(app, /const launchGate = access\?\.profile !== "lab"/);
assert.doesNotMatch(app, /authenticatedLiveAccess/);
assert.match(octopusLoadingLogo, /<img[\s\S]*?src=\{OCTOPUS_LOGO_URL\}/);
assert.match(octopusLoadingLogo, /WebkitMaskImage: maskImage/);
assert.doesNotMatch(octopusLoadingLogo, /--octopus-loading-logo-mask/);
assert.match(launchTerminal, /<OctopusLoadingLogo \/>/);
assert.match(launchTerminal, /if \(!leaving \|\| phase === "greeting"\) return;/);
assert.match(workspaceSetupGate, /<OctopusLoadingState className="app-loading-state" label="Opening Octopus" \/>/);
assert.match(source("src/components/shell/NavigationRail.tsx"), /data-app-navigation-rail="true"/);
assert.doesNotMatch(sidebar, /data-sidebar-action="notifications"/);
assert.match(shellGlyphs, /activity: \{\s*viewBox: "0 0 16 16"/);
assert.match(shellGlyphs, /activityUnread: \{\s*viewBox: "0 0 16 16"/);
assert.match(operatorShell, /const openActivityMark = useCallback/);
assert.match(electronProjection, /ledgerArtifactLabel\(candidate\) === protocol\.terminalOutputLabel[\s\S]*?movementBoundaryInterface\(candidate, "to"\) === entry\.chatInterfaceId/);
assert.match(electronProjection, /"turn\.context\.compaction": "compaction"/);
assert.match(electronProjection, /if \(label === protocol\.terminalOutputLabel\) return "message"/);
assert.doesNotMatch(electronProjection, /run_boundary|context_boundary/);
assert.match(electronProjection, /last_terminal_message: lastTerminalMessage/);
assert.match(voiceView, /if \(!callActive && homeProjectionLoading\) \{[\s\S]*?<OctopusLoadingState className="surface-content-loading-state" label="Opening Home"/);
assert.match(stylesCss, /\.surface-content-loading-state \{[\s\S]*?position: absolute;[\s\S]*?inset: 0;[\s\S]*?place-items: center;/);
assert.doesNotMatch(voiceView, /<CadencedShimmerText>Opening<\/CadencedShimmerText>/);
assert.match(chatComposer, /aria-label="Cancel dictation"[\s\S]*?onClick=\{dictation\.abortDictation\}[\s\S]*?name="close"/);
assert.doesNotMatch(chatComposer, /formatDuration\(dictation\.recordingDurationMs\)/);
assert.match(localDictation, /const WAVEFORM_BAR_STEP_PX = 6;/);
assert.match(localDictation, /const WAVEFORM_STEP_MS = 200;/);
assert.match(localDictation, /frame = window\.requestAnimationFrame\(animate\);/);
assert.match(localDictation, /if \(values\.length === 0 && canvasRef\.current\?\.clientWidth\) resetForCanvas/);
assert.doesNotMatch(localDictation, /setInterval\(/);
assert.doesNotMatch(operatorShell, /actionId: "notifications"/);
assert.match(operatorShell, /activityContent=\{<MarkActivityView[\s\S]*?onSelect=\{selectInboxItem\}/);
assert.doesNotMatch(operatorShell, /NotificationsPopover|useHomeNotifications/);
assert.doesNotMatch(voiceView, /HomeNotificationsControl|home-notifications-control/);
assert.match(workspaceSurfacePane, /!compactOverlay \? <PaneHeaderButton/);
assert.match(workspaceSurfacePane, /<OcDropdown anchorRef=\{addButtonRef\}[\s\S]*?aria-label="New tab"/);
assert.match(workspaceSurfacePane, /glyph=\{focused \? "restore" : "expand"\}/);
assert.match(appShell, /onPrimaryActionsHostChange=\{setPrimaryActionsHost\}/);
assert.match(shellCss, /\.workspace-pane-manager\.is-secondary-focused > \.workspace-pane-primary \{[\s\S]*?flex: 0 0 0;/);
assert.doesNotMatch(shellCss, /\.home-notifications-control|\.home-notification-row/);
assert.match(voiceWidgets, /id: "builtin\.voice\.live-context",[\s\S]*?slot: "right"/);
assert.match(voiceWidgets, /id: "builtin\.voice\.insights",[\s\S]*?slot: "right"/);
assert.match(clientSurfaceRenderer, /WIDGETS_REHOMED_TO_RIGHT_RAIL/);

async function verifyTranscriptLayout() {
  const React = require("react"), { JSDOM } = require("jsdom"), { createRoot } = require("react-dom/client");
  const dom = new JSDOM('<!doctype html><div id="root"></div>');
  const previousNavigator = Object.getOwnPropertyDescriptor(global, "navigator");
  Object.defineProperty(global, "navigator", { configurable: true, value: dom.window.navigator });
  Object.assign(global, { window: dom.window, document: dom.window.document, IS_REACT_ACT_ENVIRONMENT: true });
  const frames = new Map(), completions = [], artifactReads = [], artifactErrors = [];
  let artifactResponse;
  let frameId = 0;
  window.requestAnimationFrame = (callback) => (frames.set(++frameId, callback), frameId);
  window.cancelAnimationFrame = (id) => frames.delete(id);
  const scope = {
    ...React, ...toolPresentation, ...reasoningSummary, ...conversationTraceModule,
    ...typescriptModule("src/components/chat/subagentActivity.ts"), ...typescriptModule("src/components/chat/commandActivityModel.ts"),
    useShouldReduceMotion: () => true,
    Glyph: ({ name }) => React.createElement("span", { "data-glyph": name }),
    CadencedShimmerText: ({ children, className, active }) => React.createElement("span", { className, "data-active": Boolean(active) }, children),
    MessageContent: ({ text }) => React.createElement("p", null, text),
    MessageTooltip: ({ children, label, onActivate }) => React.createElement("span", { "data-tooltip-target": label?.props?.children, "data-activates-file": Boolean(onActivate), onClick: onActivate }, children),
    readProjection: async (method, params) => { assert.equal(method, "projection.artifact.bytes.read"); artifactReads.push(params); return artifactResponse; },
    appToast: { warning: (error) => artifactErrors.push(error) },
    TraceArtifactWorkbench: ({ reference }) => React.createElement("div", { "data-artifact-preview": reference.artifactId }, reference.inlinePayload),
    motion: { div: ({ animate, initial, transition, onAnimationComplete, ...props }) => {
      React.useEffect(() => { if (onAnimationComplete) completions.push(onAnimationComplete); }, [animate?.height]);
      return React.createElement("div", { ...props, style: { ...props.style, ...animate } });
    } },
  };
  function renderModule(value, names, dependencies) {
    const tree = ts.createSourceFile("layout.tsx", value, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const selected = tree.statements.filter((node) => !ts.isImportDeclaration(node) && (!names || names.includes(node.name?.text)));
    const input = selected.map((node) => node.getText(tree)).join("\n") + (names ? `\nmodule.exports = {${names.join(",")}};` : "");
    const compiled = ts.transpileModule(input, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
    const module = { exports: {} };
    Function("require", "module", "exports", ...Object.keys(dependencies), compiled)(require, module, module.exports, ...Object.values(dependencies));
    return module.exports;
  }
  const turn = renderModule(turnActivity, null, scope);
  const { parsePatchFiles, getSingularPatch } = await import("@pierre/diffs");
  const { parseWorkingPatch } = renderModule(source("src/components/chat/WorkingPatchDiff.tsx"), ["parseWorkingPatch"], { parsePatchFiles });
  for (const [kind, patch, additions, deletions] of [
    ["add", "@@ -0,0 +1,2 @@\n+report\n+total: 42\n", 2, 0],
    ["update", "@@ -1 +1 @@\n-blue\n+teal\n", 1, 1],
    ["delete", "@@ -1 +0,0 @@\n-old\n", 0, 1],
    ["update", "--- old.md\n+++ old.md\n@@ -1 +1 @@\n-old\n+new\n", 1, 1],
  ]) {
    const parsed = parseWorkingPatch(patch, "report with spaces.md", kind);
    assert.equal(parsed.error, null);
    assert.equal(parsed.fileDiff.hunks.reduce((n, hunk) => n + hunk.additionCount, 0), additions);
    assert.equal(parsed.fileDiff.hunks.reduce((n, hunk) => n + hunk.deletionCount, 0), deletions);
    assert.equal(getSingularPatch(parsed.patch).hunks.length, 1);
  }
  assert.match(parseWorkingPatch("not a diff", "report.md", "update").error, /one recorded file diff/);
  assert.ok(parseWorkingPatch("--- a\n+++ a\n@@ -1 +1 @@\n-old\n+new\n--- b\n+++ b\n@@ -1 +1 @@\n-old\n+new\n", "a", "update").error);
  const events = renderModule(messageList, ["AssistantEventContent", "TraceReadTarget", "traceReadArtifactReference", "decodeJsonArtifactPayload", "WebSearchActivity", "StructuredActivity", "activityLifecycle", "activityLifecycleFromStatus", "eventString", "eventRecord", "eventArray", "eventJson", "dynamicToolResult"], { ...scope, ...turn });
  const fileActivity = renderModule(messageList, ["FileChangeActivity", "activityPathLabel"], {
    ...scope,
    InlineUnifiedDiff: ({ diff }) => React.createElement("pre", null, diff),
  });
  const container = document.getElementById("root"), root = createRoot(container);
  const query = (selector) => container.querySelector(selector);
  const all = (selector) => [...container.querySelectorAll(selector)];
  const settle = async () => {
    while (frames.size || completions.length) await React.act(async () => {
      const batch = [...frames.values(), ...completions.splice(0)]; frames.clear(); batch.forEach((callback) => callback());
    });
  };
  const render = async (activities, turnInProgress = true, response, statusBoundaryVisible = false, activityWindow = "all") => {
    await React.act(async () => root.render(React.createElement(turn.TurnActivity, {
      activities, activityWindow, turnInProgress, response, startedAtMs: 1, presentation: "conversation", statusBoundaryVisible, toolPresentations: {},
      renderActivity: (message) => React.createElement(events.AssistantEventContent, { message, toolPresentations: {} }),
    })));
  };
  const click = async (node) => { assert.ok(node); await React.act(async () => node.click()); await settle(); };
  const message = (id, eventKind, text, eventData = {}) => ({ id, eventKind, text, eventData, role: "assistant", createdAtMs: 1 });
  const commentary = message("commentary", "commentary", "Progress explained");
  const reasoning = message("reasoning", "reasoning_delta", "**Reviewing instructions and directory**", { summary_index: 0 });
  const operation = message("operation", "dynamic_tool", "", { tool: "run_list", call_id: "list", arguments: {}, body: { count: 2 }, status: "completed", _activity_origin: { interface_role: "provider" } });
  const read = message("read", "dynamic_tool", "", { ...operation.eventData, call_id: "read", tool: "workspace.read_file", arguments: { path: "src/life.rs" } });
  const rowText = () => all(".octopus-agent-activity-row").map((row) => row.textContent);
  try {
    await render([commentary, operation], true, undefined, false, "latest-context");
    assert.match(container.textContent, /Progress explained/);
    await click(query(".octopus-agent-activity-toggle"));
    assert.deepEqual(rowText(), ["Run list"]);
    await render([commentary, operation, message("new-context", "commentary", "Checking the result")], true, undefined, false, "latest-context");
    assert.match(container.textContent, /Checking the result/);
    assert.doesNotMatch(container.textContent, /Progress explained|Run list/);
    await render([operation], true, assistantDelta, true);
    assert.equal(query(".octopus-turn-activity").dataset.state, "working");
    assert.match(query(".octopus-worked-for-boundary").textContent, /^Working/);
    await render([operation], true, { ...assistantDelta, eventData: { ...assistantDelta.eventData, phase: "final_answer" } }, true);
    assert.equal(query(".octopus-turn-activity").dataset.state, "complete");
    await render([commentary, reasoning]);
    assert.equal(query(".octopus-turn-activity-content").children.length, 2, "commentary and Thinking have one gap owner, with no spacer child");
    assert.equal(query(".octopus-thinking-fallback").textContent, "Reviewing instructions and directory");
    const streamed = query(".octopus-turn-activity-content").textContent;
    await render([commentary, { ...reasoning, eventKind: "reasoning", eventData: { summary: [reasoning.text] } }, message("empty", "commentary_delta", " \n")]);
    assert.equal(query(".octopus-turn-activity-content").textContent, streamed);
    assert.equal(query(".octopus-turn-activity-content").children.length, 2, "empty stream wrappers cannot reserve a row or hide Thinking");
    await render([{ ...commentary, eventKind: "commentary_delta" }]);
    assert.equal(query(".octopus-thinking-fallback"), null, "streamed commentary already owns progress");
    await render([operation, message("empty", "commentary", ""), read], false);
    assert.equal(query(".octopus-turn-activity-content").children.length, 1);
    assert.equal(query(".octopus-agent-activity-body"), null, "settled groups mount collapsed");
    await click(query(".octopus-agent-activity-toggle"));
    assert.deepEqual(rowText(), ["Run list", "Read life.rs"]);
    assert.ok(query('.structured-activity [data-glyph="plugins"]'), "interface operations retain a tool icon");
    assert.equal(query(".structured-activity-body"), null, "closed operation details cannot retain padding");
    await click(query(".structured-activity .octopus-agent-activity-toggle"));
    assert.match(query(".structured-activity-body").textContent, /Arguments\{\}Result[\s\S]*count/);
    const bodyRule = sourceBetween(chatCss, ".octopus-agent-activity-body {", "}");
    assert.doesNotMatch(bodyRule, /padding|margin|gap|height/, "the animated shell must carry no fixed block spacing");
    await click(query(".structured-activity .octopus-agent-activity-toggle"));
    assert.equal(query(".structured-activity-body"), null, "closed operation content unmounts after transition");
    const settledRows = rowText();
    await render([{ ...operation, id: "start", eventData: { ...operation.eventData, status: "started" } }, operation, read]);
    assert.deepEqual(rowText(), settledRows, "streamed and settled operations retain identical rows");
    assert.equal(query(".octopus-thinking-fallback"), null, "a trailing group owns Thinking without duplicate progress");
    await click(query(".octopus-turn-activity-content > .octopus-agent-activity .octopus-agent-activity-toggle"));
    await render([], false); await render([operation, read]);
    assert.equal(query(".octopus-agent-activity-body"), null, "streamed groups mount collapsed just like history");
    const artifactId = `docs.guide.md:${"a".repeat(64)}`;
    const artifactMessage = { ...message("artifact", "dynamic_tool", "", { tool: "artifact.read", call_id: "artifact", status: "completed", arguments: { id: artifactId }, _activity_origin: { interface_role: "provider" } }), artifactId: "receipt:exact-read", runId: "run-test", interfaceId: "provider-test" };
    const readResult = { tool: "artifact.read", call_id: "artifact", ok: true, body: { result: "artifact_read", id: artifactId, text: "Historical content", artifact_size_bytes: 18, offset_bytes: 0, bytes_read: 18, truncated: false } };
    artifactResponse = { artifact_id: artifactMessage.artifactId, run_id: artifactMessage.runId, interface_id: artifactMessage.interfaceId, base64: Buffer.from(JSON.stringify(readResult)).toString("base64") };
    await render([artifactMessage], false);
    await click(query(".octopus-agent-activity-toggle"));
    assert.deepEqual(rowText(), ["Read artifact docs.guide.md"]);
    assert.equal(query("[data-tooltip-target]").getAttribute("data-tooltip-target"), artifactId);
    assert.equal(query("[data-tooltip-target]").getAttribute("data-activates-file"), "true");
    await click(query("[data-tooltip-target]"));
    assert.deepEqual(artifactReads, [{ artifact_id: "receipt:exact-read", run_id: "run-test", interface_id: "provider-test" }]);
    assert.equal(query("[data-artifact-preview]").textContent, "Historical content");
    assert.deepEqual(artifactErrors, []);
    const artifactPresentation = toolPresentation.dynamicToolPresentation(artifactMessage, {});
    assert.throws(() => events.traceReadArtifactReference(artifactMessage, artifactPresentation, { ...artifactResponse, run_id: "another-run" }), /coordinates/);
    assert.throws(() => events.traceReadArtifactReference(artifactMessage, artifactPresentation, { ...artifactResponse, base64: Buffer.from(JSON.stringify({ ...readResult, body: { ...readResult.body, id: "different-artifact" } })).toString("base64") }), /target/);
    const navigation = message("navigate", "dynamic_tool", "", {
      tool: `browser.navigate__${workspaceToolSuffix}`, call_id: "navigate", status: "completed",
      arguments: { url: "https://example.org/path" }, ok: false,
      error: { message: "native host rendezvous was not published" },
      _activity_origin: { interface_role: "provider" },
    });
    await render([], false);
    await render([navigation], false);
    assert.ok(query('.octopus-agent-activity-header [data-glyph="browserUse"]'));
    await click(query(".octopus-agent-activity-toggle"));
    assert.deepEqual(rowText(), ["Used the browser"]);
    assert.ok(query('.structured-activity [data-glyph="browserUse"]'));
    await click(query(".structured-activity .octopus-agent-activity-toggle"));
    assert.match(query(".structured-activity-body").textContent, /https:\/\/example.org\/path/);
    assert.match(query(".structured-activity-body").textContent, /native host rendezvous/);
    for (const [receipt, expected] of [
      [{}, "Diff unavailable"],
      [{ added: 0 }, "Diff unavailable"],
      [{ added: 1, deleted: 0 }, "Diff unavailable"],
      [{ changeKind: "delete" }, "Diff unavailable"],
      [{ added: 0, deleted: 0, lifecycle: "error" }, "Diff unavailable"],
      [{ added: 0, deleted: 0 }, "No changes"],
      [{ detailsLoading: true }, "Loading full diff…"],
      [{ diff: "+recorded content" }, "+recorded content"],
    ]) {
      await React.act(async () => root.render(React.createElement(fileActivity.FileChangeActivity, {
        changeKind: "update", diff: "", lifecycle: "complete", path: "expense-report.md", onOpenFile: () => {}, ...receipt,
      })));
      const disclosure = query('[aria-label="Toggle diff for expense-report.md"]');
      if (disclosure.getAttribute("aria-expanded") === "false") await click(disclosure);
      assert.equal(query(".activity-file-body").textContent, expected);
    }
  } finally {
    await React.act(async () => root.unmount()); dom.window.close();
    delete global.window; delete global.document; delete global.IS_REACT_ACT_ENVIRONMENT;
    if (previousNavigator) Object.defineProperty(global, "navigator", previousNavigator);
    else delete global.navigator;
  }
}

verifyTranscriptLayout().then(() => process.stdout.write("chat trace source contracts and transcript layout fixtures passed\n"))
  .catch((error) => { console.error(error); process.exitCode = 1; });
