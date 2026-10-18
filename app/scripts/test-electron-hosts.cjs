const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const vm = require("node:vm");
const { promisify } = require("node:util");
const { execFile } = require("node:child_process");
const { EventEmitter } = require("node:events");
const { app } = require("electron");
const {
  cascadeClientWindowBounds,
  createClientWindowRegistry,
  validateClientWindowOpenRequest,
} = require("../electron/clientWindows.cjs");
const { createRuntimeHost } = require("../electron/runtimeHost.cjs");
const { createTerminalHost } = require("../electron/terminalHost.cjs");
const { createWorkerEnvironmentOperatorActions } = require("../electron/workerEnvironmentActions.cjs");
const {
  OPERATOR_CHAT_SUBMIT_ERROR_MESSAGE,
  operatorChatSubmitFailure,
  operatorChatSubmitSuccess,
  unwrapOperatorChatSubmitResult,
} = require("../electron/operatorChatSubmitResult.cjs");
const {
  isOperatorChatClientInterfaceCandidate,
} = require("../electron/operatorChatClientContract.cjs");
const {
  PROVIDER_AUTHENTICATION_INCIDENT,
  PROVIDER_CREDENTIAL_REPAIR_ACTION,
  PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
  executeClientExtensionAction,
  readClientExtensionActionStatuses,
} = require("../electron/clientExtensionActions.cjs");

const execFileAsync = promisify(execFile);

async function main() {
  await app.whenReady();
  await verifySandboxedPreloadImports();
  await verifyClientWindowHostContract();
  await verifyBootSourceContract();
  await verifyRuntimeReadinessEvidence();
  await verifyBootTerminalBehavior();
  await verifyBootTerminalQuit();
  await verifyTerminalOwnership();
  await verifyGitIndexMutation();
  await verifyProviderCredentialRepair();
  verifyOperatorChatSubmitResult();
  process.stdout.write("electron host checks passed\n");
  app.quit();
}

async function verifySandboxedPreloadImports() {
  const source = await fs.readFile(path.join(__dirname, "../electron/preload.cjs"), "utf8");
  const imports = [...source.matchAll(/require\(\s*(["'])([^"']+)\1\s*\)/g)].map((match) => match[2]);
  assert.deepEqual(imports, ["electron"]);
  assert.match(source, /openLaunchTerminal\(request\)[\s\S]*?invokeAccess\("octopus:launch-terminal-open", request\)/);
  assert.match(source, /writeLaunchTerminal\(sessionId, data\)[\s\S]*?invokeAccess\("octopus:launch-terminal-write", sessionId, data\)/);
  assert.match(source, /resizeLaunchTerminal\(sessionId, request\)[\s\S]*?invokeAccess\("octopus:launch-terminal-resize", sessionId, request\)/);
  assert.match(source, /bootStatus\(\)[\s\S]*?invokeAccess\("octopus:boot-status"\)/);
  assert.match(source, /clientWindowBootstrap\(\)[\s\S]*?sendSync\("octopus:client-window-bootstrap"\)/);
  assert.match(source, /openClientWindow\(request\)[\s\S]*?invokeAccess\("octopus:client-window-open", request\)/);
  assert.match(source, /clientWindowReady\(request\)[\s\S]*?invokeAccess\("octopus:client-window-ready", request\)/);
  assert.match(source, /onClientAccessLocked\(listener\)[\s\S]*?ipcRenderer\.on\("octopus:client-access-locked", wrapped\)/);
  assert.match(source, /beginClientPaneTransfer\(request\)[\s\S]*?beginClientPaneTransferSync\(request\)/);
  assert.match(source, /acceptClientPaneTransfer\(request\)[\s\S]*?invokeAccess\("octopus:client-pane-transfer-accept", request\)/);
  assert.match(source, /commitClientPaneTransfer\(request\)[\s\S]*?invokeAccess\("octopus:client-pane-transfer-commit", request\)/);
  assert.match(source, /acknowledgeClientPaneTransfer\(request\)[\s\S]*?acknowledgeClientPaneTransferSync\(request\)/);
  assert.match(source, /pendingClientPaneTransfers\(\)[\s\S]*?invokeAccess\("octopus:client-pane-transfer-pending"\)/);
  assert.match(source, /pendingClientPaneDestinationTransfers\(\)[\s\S]*?invokeAccess\("octopus:client-pane-transfer-destination-pending"\)/);
  assert.match(source, /resumeClientPaneTransfer\(request\)[\s\S]*?invokeAccess\("octopus:client-pane-transfer-resume", request\)/);
  assert.match(source, /finalizeClientPaneTransfer\(request\)[\s\S]*?invokeAccess\("octopus:client-pane-transfer-finalize", request\)/);
  assert.match(source, /onClientPaneTransferCompleted\(listener\)[\s\S]*?client-pane-transfer-completed/);
  assert.match(source, /subscribeRuntimeEvent\("octopus:runtime-host-state", listener\)/);
  assert.match(source, /submitOperatorTurnControl\(request\)[\s\S]*?invokeAccess\("octopus:submit-operator-turn-control", request\)/);
  const removedPrefix = ["runtime", "terminal"].join("-");
  for (const suffix of ["open", "write", "resize", "read", "data", "exit"]) {
    assert.equal(source.includes(`${removedPrefix}-${suffix}`), false);
  }
}

async function verifyClientWindowHostContract() {
  const [mainSource, preloadSource, typeSource, modelSource] = await Promise.all([
    fs.readFile(path.join(__dirname, "../electron/main.cjs"), "utf8"),
    fs.readFile(path.join(__dirname, "../electron/preload.cjs"), "utf8"),
    fs.readFile(path.join(__dirname, "../src/electron.d.ts"), "utf8"),
    fs.readFile(path.join(__dirname, "../src/shell/clientWindowModel.ts"), "utf8"),
  ]);
  const createWindowSource = mainSource.match(/function createWindow\(inheritedAccessToken = "", options = \{\}\) \{[\s\S]*?(?=\nfunction createQuickTaskWindow)/)?.[0] ?? "";
  const startupSource = mainSource.match(/const gotSingleInstanceLock = app\.requestSingleInstanceLock\(\);[\s\S]*?(?=\nfunction grantLocalMediaPermissions)/)?.[0] ?? "";
  assert.notEqual(createWindowSource, "");
  assert.match(createWindowSource, /const windowState = primary \? loadWindowState\(\) : peerClientWindowState\(\)/);
  assert.match(createWindowSource, /clientWindows\.register\(win, \{[\s\S]*?accessToken: inheritedAccessToken,[\s\S]*?primary/);
  assert.match(createWindowSource, /if \(primary\) trackWindowState\(win\)/);
  assert.match(createWindowSource, /loadRendererWindow\(win\);/);
  assert.match(createWindowSource, /additionalArguments: \[`--octopus-access-claim=\$\{accessClaim\}`\]/);
  assert.doesNotMatch(createWindowSource, /loadRendererWindow\(win,\s*\{/);
  assert.doesNotMatch(createWindowSource, /--octopus-(client-)?(pane|target)/);
  assert.match(mainSource, /ipcMain\.on\("octopus:client-window-bootstrap"[\s\S]*?bootstrapForWebContents\(event\.sender\.id\)/);
  assert.match(mainSource, /ipcMain\.handle\("octopus:client-window-open"[\s\S]*?validateClientWindowOpenRequest\(requestValue\)[\s\S]*?waitUntilReady\(bootstrap\.windowId\)[\s\S]*?win\.close\(\)/);
  assert.match(mainSource, /ipcMain\.handle\("octopus:client-window-ready"[\s\S]*?entry\.accessToken !== token[\s\S]*?markReady\(event\.sender\.id, request\)/);
  assert.match(mainSource, /ipcMain\.on\("octopus:client-pane-transfer-begin"[\s\S]*?beginPaneTransfer\(event\.sender\.id, request\)/);
  assert.match(mainSource, /ipcMain\.handle\("octopus:client-pane-transfer-accept"[\s\S]*?acceptPaneTransfer\(event\.sender\.id, request\)/);
  assert.match(mainSource, /ipcMain\.handle\("octopus:client-pane-transfer-commit"[\s\S]*?commitPaneTransfer\(event\.sender\.id, request\)[\s\S]*?client-pane-transfer-completed[\s\S]*?abortCommittedPaneTransfer[\s\S]*?return result\.outcome/);
  assert.match(mainSource, /ipcMain\.on\("octopus:client-pane-transfer-acknowledge"[\s\S]*?acknowledgePaneTransfer\(event\.sender\.id, request\)/);
  assert.match(mainSource, /ipcMain\.handle\("octopus:client-pane-transfer-pending"[\s\S]*?pendingPaneTransferCompletions\(event\.sender\.id\)/);
  assert.match(mainSource, /ipcMain\.handle\("octopus:client-pane-transfer-destination-pending"[\s\S]*?pendingDestinationPaneTransfers\(event\.sender\.id\)/);
  assert.match(mainSource, /ipcMain\.handle\("octopus:client-pane-transfer-resume"[\s\S]*?resumePaneTransfer\(event\.sender\.id, request\)/);
  assert.match(mainSource, /ipcMain\.handle\("octopus:client-pane-transfer-finalize"[\s\S]*?finalizePaneTransfer\(event\.sender\.id, request\)/);
  assert.match(mainSource, /function requireClientLockWindow\(event, token\)[\s\S]*?entry\.accessToken !== token/);
  const accessLockSource = mainSource.match(/ipcMain\.handle\("octopus:access-lock"[\s\S]*?\n\}\);/)?.[0] ?? "";
  assert.match(accessLockSource, /await clientWindowClose\.requestLogout\(sender, token,[\s\S]*?getClientAccessController\(\)\.lock\(token\)/);
  assert.match(accessLockSource, /clientWindows\.clearAccessToken\(token\);[\s\S]*?"octopus:client-access-locked"/);
  assert.match(accessLockSource, /requireClientLockWindow\(event, token\)/);
  assert.doesNotMatch(accessLockSource, /requireSession\(token\)/);
  assert.match(mainSource, /label: "New Window", click: \(\) => openNewClientWindowFromMenu\(\)/);
  assert.match(mainSource, /function openNewClientWindowFromMenu\(\)[\s\S]*?clientWindows\.select\(\{ focusedWindow: focused \}\)[\s\S]*?authenticatedClientWindowToken\(source\)[\s\S]*?ensurePrimaryClientWindow\(\)[\s\S]*?primary: false/);
  assert.match(startupSource, /second-instance[\s\S]*?focusAppropriateClientWindow\(\)/);
  assert.match(startupSource, /app\.on\("activate"[\s\S]*?focusAppropriateClientWindow\(\)/);
  assert.match(mainSource, /quick-task-open-main[\s\S]*?ensureClientWindowForAccess\(token, source\)/);
  assert.match(mainSource, /worker-environment-window-open-main[\s\S]*?ensureClientWindowForAccess\(token, source\)/);
  assert.match(mainSource, /globalDictationShortcut[\s\S]*?ensureClientWindowForAccess\(token\)/);
  assert.match(preloadSource, /sendSync\("octopus:client-window-bootstrap"\)/);
  assert.match(typeSource, /import type \{[\s\S]*?ClientWindowBootstrap,[\s\S]*?ClientWindowOpenOutcome,[\s\S]*?\} from "\.\/shell\/clientWindowModel";/);
  assert.match(typeSource, /clientWindowBootstrap\(\): ClientWindowBootstrap \| null;/);
  assert.match(typeSource, /openClientWindow\(request\?: ClientWindowOpenRequest\): Promise<ClientWindowOpenOutcome>;/);
  assert.match(typeSource, /clientWindowReady\(request: ClientWindowReadyRequest\): Promise<ClientWindowOpenOutcome>;/);
  assert.match(typeSource, /beginClientPaneTransfer\(request: ClientPaneTransferBeginRequest\): ClientPaneDragPayload;/);
  assert.match(typeSource, /acknowledgeClientPaneTransfer\(request: ClientPaneTransferAcknowledgementRequest\): \{ released: boolean; transferId: string \};/);
  assert.match(typeSource, /pendingClientPaneTransfers\(\): Promise<ClientPaneTransferCompleted\[\]>;/);
  assert.match(typeSource, /pendingClientPaneDestinationTransfers\(\): Promise<\(ClientPaneTransferCompleted & \{ committed: boolean \}\)\[\]>;/);
  assert.match(typeSource, /resumeClientPaneTransfer\(request: ClientPaneTransferRequest\): Promise<ClientPaneTransferCompleted>;/);
  assert.match(typeSource, /finalizeClientPaneTransfer\(request: ClientPaneTransferRequest\): Promise<\{ finalized: true; transferId: string \}>;/);
  assert.match(typeSource, /onClientPaneTransferCompleted\(listener: \(completion: ClientPaneTransferCompleted\) => void\): \(\) => void;/);
  assert.match(modelSource, /export type ClientWindowBootstrap = \{/);
  assert.match(modelSource, /export type ClientWindowOpenOutcome = \{/);

  const surface = {
    kind: "surface",
    runId: "run-1",
    surfaceId: "chat",
    workspaceKey: "octopus-os",
  };
  const request = validateClientWindowOpenRequest({ initialTarget: surface });
  assert.deepEqual(request, { initialTarget: surface });
  assert.deepEqual(validateClientWindowOpenRequest(), {});
  assert.deepEqual(validateClientWindowOpenRequest({}), {});
  assert.deepEqual(validateClientWindowOpenRequest({ initialTarget: {
    kind: "files",
    workspaceKey: "dashboard",
    openTarget: { kind: "file", path: "", rootId: "workspace" },
  } }), { initialTarget: {
    kind: "files",
    workspaceKey: "dashboard",
    openTarget: { kind: "file", path: "", rootId: "workspace" },
  } });
  assert.deepEqual(validateClientWindowOpenRequest({ initialTarget: {
    kind: "worker_environment",
    workspaceKey: "octopus-os",
    target: {
      schemaVersion: 1,
      workerEnvironmentId: "we-1",
      surfaceId: "surface-1",
      panelKind: "custom",
      clientSurface: { documentId: "document-1", revision: 2, sourceDigest: "abc123" },
    },
  } }).initialTarget.target.clientSurface, { documentId: "document-1", revision: 2, sourceDigest: "abc123" });
  assert.throws(() => validateClientWindowOpenRequest(null), /must be an object/);
  assert.throws(() => validateClientWindowOpenRequest({ unexpected: true }), /unsupported field/);
  assert.throws(() => validateClientWindowOpenRequest({ initialTarget: {
    kind: "terminal",
    terminalId: "terminal-1",
    title: "Terminal",
    workspaceKey: "octopus-os",
  } }), /Terminal panes cannot be transferred/);
  assert.throws(() => validateClientWindowOpenRequest({ initialTarget: { ...surface, extra: "field" } }), /unsupported field/);
  assert.throws(() => validateClientWindowOpenRequest({ initialTarget: { ...surface, surfaceId: "x".repeat(513) } }), /surface id is invalid/);
  assert.throws(() => validateClientWindowOpenRequest({ initialTarget: { ...surface, surfaceId: "drivers" } }), /Only a chat surface target can carry a run id/);
  assert.throws(() => validateClientWindowOpenRequest({ initialTarget: {
    kind: "files",
    workspaceKey: "dashboard",
    openTarget: { kind: "artifact", artifactId: "artifact-1", interfaceId: "world-x", label: "result", runId: "run-1", createdAtMs: -1 },
  } }), /creation time is invalid/);
  assert.throws(() => validateClientWindowOpenRequest({ initialTarget: {
    kind: "worker_environment",
    workspaceKey: "octopus-os",
    target: { schemaVersion: 1, workerEnvironmentId: "we-1", clientSurface: { documentId: "doc", revision: 1, sourceDigest: "digest" } },
  } }), /requires an exact surface id/);

  let nextId = 0;
  const registry = createClientWindowRegistry({ idFactory: () => `test-${nextId += 1}`, readyTimeoutMs: 100 });
  const primary = testClientWindow(101);
  const primaryBootstrap = registry.register(primary, { primary: true });
  assert.deepEqual(primaryBootstrap, {
    schema: "octopus.client-window",
    schemaVersion: 1,
    windowId: "main",
    scope: "main",
    primary: true,
  });
  registry.setAccessToken(101, "token-a");
  const peer = testClientWindow(102);
  const peerBootstrap = registry.register(peer, { accessToken: "token-a", initialTarget: surface, primary: false });
  assert.equal(peerBootstrap.primary, false);
  assert.equal(peerBootstrap.windowId, "window-test-1");
  assert.equal(peerBootstrap.scope, "window-test-1");
  assert.deepEqual(registry.bootstrapForWebContents(102)?.initialTarget, surface);
  primary.focus();
  peer.focus();
  assert.equal(registry.select({ focusedWindow: testClientWindow(999) })?.win, peer);
  assert.equal(registry.select({ accessToken: "token-a" })?.win, peer);
  const readiness = registry.waitUntilReady(peerBootstrap.windowId);
  assert.deepEqual(registry.markReady(102, { windowId: peerBootstrap.windowId }), { opened: true, windowId: peerBootstrap.windowId });
  assert.deepEqual(await readiness, { opened: true, windowId: peerBootstrap.windowId });
  assert.throws(() => registry.markReady(102, { windowId: "main" }), /does not match/);
  const transferReference = registry.beginPaneTransfer(102, {
    sourcePane: "secondary",
    sourceTabId: "chat:octopus-os:run-1",
    target: surface,
  });
  assert.throws(() => registry.beginPaneTransfer(102, {
    sourcePane: "secondary",
    sourceTabId: "chat:octopus-os:run-1",
    target: surface,
  }), /already active for this source tab/);
  assert.deepEqual(transferReference, {
    schema: "octopus.client-pane-transfer",
    schemaVersion: 1,
    sourceWindowId: peerBootstrap.windowId,
    transferId: "transfer-test-2",
  });
  assert.throws(() => registry.acceptPaneTransfer(102, { transferId: transferReference.transferId }), /another full client window/);
  assert.deepEqual(registry.acceptPaneTransfer(101, { transferId: transferReference.transferId }), {
    ...transferReference,
    sourcePane: "secondary",
    sourceTabId: "chat:octopus-os:run-1",
    target: surface,
  });
  const competingSource = testClientWindow(107);
  registry.register(competingSource, { accessToken: "token-a", primary: false });
  const competingTransfer = registry.beginPaneTransfer(107, {
    sourcePane: "secondary",
    sourceTabId: "surface:octopus-os:drivers",
    target: { ...surface, runId: "", surfaceId: "drivers" },
  });
  assert.throws(() => registry.acceptPaneTransfer(101, { transferId: competingTransfer.transferId }), /destination is already accepting another tab/);
  registry.cancelPaneTransfer(107, { transferId: competingTransfer.transferId });
  competingSource.close();
  const expectedCompletion = {
    ...transferReference,
    sourcePane: "secondary",
    sourceTabId: "chat:octopus-os:run-1",
    target: surface,
  };
  const committedTransfer = registry.commitPaneTransfer(101, { transferId: transferReference.transferId });
  assert.deepEqual({
    completion: committedTransfer.completion,
    sourceWebContentsId: 102,
  }, {
    completion: expectedCompletion,
    sourceWebContentsId: 102,
  });
  assert.equal(committedTransfer.outcome instanceof Promise, true);
  assert.deepEqual(registry.pendingPaneTransferCompletions(102), [{
    ...expectedCompletion,
  }]);
  assert.throws(() => registry.commitPaneTransfer(101, { transferId: transferReference.transferId }), /already committed/);
  assert.throws(() => registry.cancelPaneTransfer(102, { transferId: transferReference.transferId }), /requires source acknowledgement/);
  assert.throws(() => registry.beginPaneTransfer(102, {
    sourcePane: "secondary",
    sourceTabId: "chat:octopus-os:run-1",
    target: surface,
  }), /already active for this source tab/);
  assert.deepEqual(registry.acknowledgePaneTransfer(102, { released: true, transferId: transferReference.transferId }), {
    released: true,
    transferId: transferReference.transferId,
  });
  assert.deepEqual(await committedTransfer.outcome, expectedCompletion);
  assert.deepEqual(registry.pendingPaneTransferCompletions(102), []);
  assert.deepEqual(registry.pendingDestinationPaneTransfers(101), [{ ...expectedCompletion, committed: true }]);
  assert.deepEqual(await registry.resumePaneTransfer(101, { transferId: transferReference.transferId }), expectedCompletion);
  assert.throws(() => registry.acceptPaneTransfer(101, { transferId: transferReference.transferId }), /already committed/);
  assert.deepEqual(registry.finalizePaneTransfer(101, { transferId: transferReference.transferId }), {
    finalized: true,
    transferId: transferReference.transferId,
  });
  assert.deepEqual(registry.pendingDestinationPaneTransfers(101), []);
  assert.throws(() => registry.acceptPaneTransfer(101, { transferId: transferReference.transferId }), /unavailable or expired/);
  const retainedTransfer = registry.beginPaneTransfer(102, {
    sourcePane: "secondary",
    sourceTabId: "surface:retained",
    target: { ...surface, runId: "", surfaceId: "drivers" },
  });
  registry.acceptPaneTransfer(101, { transferId: retainedTransfer.transferId });
  const retainedCommit = registry.commitPaneTransfer(101, { transferId: retainedTransfer.transferId });
  assert.deepEqual(registry.acknowledgePaneTransfer(102, { released: false, transferId: retainedTransfer.transferId }), {
    released: false,
    transferId: retainedTransfer.transferId,
  });
  await assert.rejects(retainedCommit.outcome, /source retained ownership/);
  assert.deepEqual(registry.pendingPaneTransferCompletions(102), []);
  const reboundTransfer = registry.beginPaneTransfer(102, {
    sourcePane: "secondary",
    sourceTabId: "surface:drivers",
    target: { ...surface, runId: "", surfaceId: "drivers" },
  });
  registry.acceptPaneTransfer(101, { transferId: reboundTransfer.transferId });
  registry.setAccessToken(101, "token-b");
  assert.throws(() => registry.commitPaneTransfer(101, { transferId: reboundTransfer.transferId }), /session changed before commit/);
  registry.setAccessToken(101, "token-a");
  registry.cancelPaneTransfer(102, { transferId: reboundTransfer.transferId });
  const cancelledTransfer = registry.beginPaneTransfer(102, {
    sourcePane: "secondary",
    sourceTabId: "surface:drivers",
    target: { ...surface, runId: "", surfaceId: "drivers" },
  });
  assert.deepEqual(registry.cancelPaneTransfer(102, { transferId: cancelledTransfer.transferId }), {
    cancelled: true,
    transferId: cancelledTransfer.transferId,
  });
  const closeReleaseSource = testClientWindow(108);
  registry.register(closeReleaseSource, { accessToken: "token-a", primary: false });
  const closeReleaseTransfer = registry.beginPaneTransfer(108, {
    sourcePane: "secondary",
    sourceTabId: "surface:close-release",
    target: { ...surface, runId: "", surfaceId: "drivers" },
  });
  registry.acceptPaneTransfer(101, { transferId: closeReleaseTransfer.transferId });
  const closeReleaseCommit = registry.commitPaneTransfer(101, { transferId: closeReleaseTransfer.transferId });
  registry.acknowledgePaneTransfer(108, { released: true, transferId: closeReleaseTransfer.transferId });
  closeReleaseSource.close();
  assert.deepEqual(await closeReleaseCommit.outcome, closeReleaseCommit.completion);
  assert.deepEqual(registry.pendingDestinationPaneTransfers(101), [{ ...closeReleaseCommit.completion, committed: true }]);
  registry.finalizePaneTransfer(101, { transferId: closeReleaseTransfer.transferId });
  const unreleasedSource = testClientWindow(110);
  registry.register(unreleasedSource, { accessToken: "token-a", primary: false });
  const unreleasedTransfer = registry.beginPaneTransfer(110, {
    sourcePane: "secondary",
    sourceTabId: "surface:unreleased-close",
    target: { ...surface, runId: "", surfaceId: "drivers" },
  });
  registry.acceptPaneTransfer(101, { transferId: unreleasedTransfer.transferId });
  const unreleasedCommit = registry.commitPaneTransfer(101, { transferId: unreleasedTransfer.transferId });
  unreleasedSource.close();
  await assert.rejects(unreleasedCommit.outcome, /ended before source release/);
  assert.deepEqual(registry.pendingDestinationPaneTransfers(101), []);
  const closingDestination = testClientWindow(109);
  registry.register(closingDestination, { accessToken: "token-a", primary: false });
  const destinationCloseTransfer = registry.beginPaneTransfer(102, {
    sourcePane: "secondary",
    sourceTabId: "surface:destination-close",
    target: { ...surface, runId: "", surfaceId: "drivers" },
  });
  registry.acceptPaneTransfer(109, { transferId: destinationCloseTransfer.transferId });
  const destinationCloseCommit = registry.commitPaneTransfer(109, { transferId: destinationCloseTransfer.transferId });
  closingDestination.close();
  await assert.rejects(destinationCloseCommit.outcome, /ended before source release/);
  const fallbackPeer = testClientWindow(103);
  registry.register(fallbackPeer, { accessToken: "token-a", primary: false });
  primary.focus();
  fallbackPeer.focus();
  peer.focus();
  peer.close();
  assert.equal(registry.select({ focusedWindow: testClientWindow(998) })?.win, fallbackPeer);
  const closingPeer = testClientWindow(104);
  const closingBootstrap = registry.register(closingPeer, { accessToken: "token-a", primary: false });
  const closedReadiness = registry.waitUntilReady(closingBootstrap.windowId);
  closingPeer.close();
  await assert.rejects(closedReadiness, /closed before destination initialization/);
  const authenticatedPeer = testClientWindow(105);
  registry.register(authenticatedPeer, { accessToken: "token-a", primary: false });
  assert.equal(registry.closeAuthenticatedPeers(), 2);
  assert.equal(peer.isDestroyed(), true);
  assert.equal(fallbackPeer.isDestroyed(), true);
  assert.equal(authenticatedPeer.isDestroyed(), true);
  assert.equal(primary.isDestroyed(), false);
  primary.close();
  assert.equal(registry.primaryEntry(), null);
  const replacementPrimary = testClientWindow(106);
  assert.equal(registry.register(replacementPrimary, { primary: true }).windowId, "main");

  let retentionId = 0;
  const retentionRegistry = createClientWindowRegistry({
    idFactory: () => `retention-${retentionId += 1}`,
    transferTimeoutMs: 10,
  });
  const retentionSource = testClientWindow(301);
  const retentionDestination = testClientWindow(302);
  retentionRegistry.register(retentionSource, { primary: true });
  retentionRegistry.setAccessToken(301, "token-retention");
  retentionRegistry.register(retentionDestination, { accessToken: "token-retention", primary: false });
  const retainedCommittedTransfer = retentionRegistry.beginPaneTransfer(301, {
    sourcePane: "secondary",
    sourceTabId: "surface:retention",
    target: { ...surface, runId: "", surfaceId: "drivers" },
  });
  retentionRegistry.acceptPaneTransfer(302, { transferId: retainedCommittedTransfer.transferId });
  const retainedCommittedOutcome = retentionRegistry.commitPaneTransfer(302, { transferId: retainedCommittedTransfer.transferId });
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.equal(retentionRegistry.pendingPaneTransferCompletions(301).length, 0);
  assert.equal(retentionSource.isDestroyed(), false);
  await assert.rejects(retainedCommittedOutcome.outcome, /ended before source release/);

  const timeoutRegistry = createClientWindowRegistry({ idFactory: () => "timeout", readyTimeoutMs: 10 });
  const timeoutPeer = testClientWindow(201);
  const timeoutBootstrap = timeoutRegistry.register(timeoutPeer, { accessToken: "token-b", primary: false });
  await assert.rejects(timeoutRegistry.waitUntilReady(timeoutBootstrap.windowId), /did not become ready within 10ms/);
  assert.deepEqual(cascadeClientWindowBounds(
    { x: 100, y: 100, width: 900, height: 650 },
    { x: 0, y: 0, width: 1440, height: 900 },
    { width: 1120, height: 720 },
  ), { x: 128, y: 128, width: 900, height: 650 });
  assert.deepEqual(cascadeClientWindowBounds(null, null, { width: 1120, height: 720 }), { width: 1120, height: 720 });
}

async function verifyBootSourceContract() {
  const [mainSource, viteSource, runtimeSource, actionSource, runtimeClientSource, appSource, launchSource, sessionSource, stylesSource, projectionLiveSource, runtimeTopologySource] = await Promise.all([
    fs.readFile(path.join(__dirname, "../electron/main.cjs"), "utf8"),
    fs.readFile(path.join(__dirname, "../vite.config.ts"), "utf8"),
    fs.readFile(path.join(__dirname, "../electron/runtimeHost.cjs"), "utf8"),
    fs.readFile(path.join(__dirname, "../electron/clientExtensionActions.cjs"), "utf8"),
    fs.readFile(path.join(__dirname, "../src/runtimeHost.ts"), "utf8"),
    fs.readFile(path.join(__dirname, "../src/App.tsx"), "utf8"),
    fs.readFile(path.join(__dirname, "../src/components/onboarding/LaunchTerminal.tsx"), "utf8"),
    fs.readFile(path.join(__dirname, "../src/components/terminal/shellSession.ts"), "utf8"),
    fs.readFile(path.join(__dirname, "../src/styles.css"), "utf8"),
    fs.readFile(path.join(__dirname, "../src/projectionLive.ts"), "utf8"),
    fs.readFile(path.join(__dirname, "../src/runtimeTopology.ts"), "utf8"),
  ]);
  const beforeQuitSource = mainSource.match(/app\.on\("before-quit"[\s\S]*?\n\}\);/)?.[0] ?? "";
  const quitCleanupSource = mainSource.match(/function startClientQuitCleanup\(\)[\s\S]*?(?=\nfunction closeClientResources)/)?.[0] ?? "";
  const touchIdStatusSource = mainSource.match(/function touchIdStatus\(\)[\s\S]*?(?=\nfunction touchIdCapabilityAvailable)/)?.[0] ?? "";
  const touchIdPreferenceSource = mainSource.match(/function readTouchIdPreference\(\)[\s\S]*?(?=\nfunction readTouchIdEnrollmentRecord)/)?.[0] ?? "";
  const touchIdRecordSource = mainSource.match(/function readTouchIdEnrollmentRecord\(\)[\s\S]*?(?=\nfunction verifyTouchIdEnrollment)/)?.[0] ?? "";
  const submitChatTurnSource = mainSource.match(/ipcMain\.handle\("octopus:submit-chat-turn"[\s\S]*?\n\}\);(?=\n\nipcMain\.handle\("octopus:operator-chat-artifacts-read")/)?.[0] ?? "";
  const submitTurnControlSource = mainSource.match(/ipcMain\.handle\("octopus:submit-operator-turn-control"[\s\S]*?\n\}\);(?=\n\nipcMain\.handle\("octopus:submit-chat-steer")/)?.[0] ?? "";
  const submitChatSteerSource = mainSource.match(/ipcMain\.handle\("octopus:submit-chat-steer"[\s\S]*?\n\}\);(?=\n\nipcMain\.handle\("octopus:update-chat-pins")/)?.[0] ?? "";
  const authorizeClientArtifactSource = mainSource.match(/async function authorizeClientInterfaceArtifactSubmission\(request\)[\s\S]*?(?=\nfunction getOperatorChatClientLoader)/)?.[0] ?? "";
  const authorizeDevArtifactSource = viteSource.match(/const authorizeClientInterfaceArtifactSubmission = async \(request: Record<string, unknown>\) => \{[\s\S]*?\n      \};/)?.[0] ?? "";
  assert.match(mainSource, /ipcMain\.handle\("octopus:launch-terminal-open"[\s\S]*?openExisting\(owner,[\s\S]*?if \(existing\) return existing;[\s\S]*?preparedCommand: state\.launchCommand/);
  assert.match(mainSource, /function launchTerminalOwner\(event, token\)[\s\S]*?clientWindows\.entryForWebContents\(event\.sender\.id\)[\s\S]*?access\.mode === "bootstrap" && !entry\.primary[\s\S]*?scope: "launch"/);
  assert.match(beforeQuitSource, /event\.preventDefault\(\);[\s\S]*?clientWindowClose\.requestQuit\(\)/);
  assert.match(quitCleanupSource, /const startupRecovery = runtimeInitializationPromise[\s\S]*?await startupRecovery[\s\S]*?const interrupted = terminalHost\?\.interruptLaunch\(\)[\s\S]*?runtimeHost\.waitUntilStopped\(\)[\s\S]*?Promise\.all\(\[stopObservation, terminalHost\?\.terminate\(\)\]\)[\s\S]*?quitReady = true;[\s\S]*?app\.quit\(\)/);
  assert.doesNotMatch(beforeQuitSource, /app\.quit\(\)/);
  assert.match(touchIdStatusSource, /touchIdCapabilityAvailable\(\)[\s\S]*?readTouchIdEnrollmentRecord\(\)/);
  assert.match(touchIdStatusSource, /const prompted = readTouchIdPreference\(\)/);
  assert.match(touchIdPreferenceSource, /stat\.isFile\(\)[\s\S]*?stat\.isSymbolicLink\(\)[\s\S]*?stat\.mode & 0o077[\s\S]*?value\.prompted !== true/);
  assert.doesNotMatch(touchIdStatusSource, /safeStorage/);
  assert.doesNotMatch(touchIdRecordSource, /safeStorage/);
  assert.match(mainSource, /ipcMain\.handle\("octopus:touch-id-set-enabled"[\s\S]*?writeTouchIdPreference\(\);[\s\S]*?if \(!enabled\) \{[\s\S]*?clearTouchIdEnrollment\(\);[\s\S]*?await promptTouchId\("Enable Touch ID to unlock Octopus"\)/);
  assert.match(mainSource, /await promptTouchId\("Unlock Octopus"\)[\s\S]*?verifyTouchIdEnrollment\(currentEnrollment\)[\s\S]*?unlockWithTouchId\(\)/);
  assert.doesNotMatch(mainSource, /executeClientExtensionAction\([\s\S]*?launchTerminal: terminalHost[\s\S]*?runtimeHost:/);
  assert.match(mainSource, /OCTOPUS_MACHINE_CODEX_AUTH_FILE: path\.join\(resolveOperatorCodexHome\(\), "auth\.json"\)/);
  assert.doesNotMatch(mainSource, /runtimeHost\s*\?\?\s*await initializeRuntimeHost\(\)/);
  assert.doesNotMatch(mainSource, /void host\.start\(\)/);
  assert.equal(mainSource.includes("activeRuntime" + "LaunchEnvironment"), false);
  assert.equal(mainSource.includes(["runtime", "terminal"].join("-")), false);
  assert.match(runtimeSource, /const instance = await readRuntimeInstance\(home\)/);
  assert.doesNotMatch(runtimeSource, /node-pty|\n  start\(|\n  stopManaged\(/);
  assert.equal(runtimeSource.includes(["client", "launch", "broker"].join("-")), false);
  assert.equal(runtimeSource.includes(["create", "Runtime", "Launch", "Broker"].join("")), false);
  assert.match(actionSource, /privateStore\.auth_file_env !== MACHINE_CODEX_AUTH_FILE_ENV/);
  assert.match(actionSource, /credentialUpdated: false,\s*runtimeRestarted: false,\s*sharedCredential: true/);
  assert.doesNotMatch(actionSource, /prepareLaunchCommand|submitPreparedLaunchCommand|interruptLaunch|waitUntilStopped|waitUntilReady/);
  assert.equal(actionSource.includes(["stop", "Managed"].join("")), false);
  assert.equal(actionSource.includes("runtimeHost." + "start("), false);
  assert.equal(runtimeClientSource.includes("restarting"), false);
  assert.equal(runtimeClientSource.includes("restart:"), false);
  assert.match(appSource, /const \[runtimeHasBeenReady, setRuntimeHasBeenReady\] = useState\(runtimeHost\.ready\);/);
  assert.match(appSource, /const launchGate = access\?\.profile !== "lab"\s*&& runtimeHost\.available\s*&& runtimeHost\.phase !== "probing"\s*&& runtimeHost\.phase !== "access_required"\s*&& !runtimeHost\.ready\s*&& !runtimeHasBeenReady;/);
  assert.doesNotMatch(appSource, /authenticatedLiveAccess/);
  assert.match(appSource, /useEffect\(\(\) => \{\s*if \(runtimeHost\.ready\) setRuntimeHasBeenReady\(true\);\s*\}, \[runtimeHost\.ready\]\);/);
  assert.match(appSource, /useEffect\(\(\) => \{\s*if \(launchGate\) setLaunchVisible\(true\);\s*\}, \[launchGate\]\);/);
  assert.match(appSource, /if \(access\.profile !== "lab" && runtimeHost\.available && runtimeHost\.phase === "probing" && !runtimeHasBeenReady\) \{/);
  assert.ok(appSource.indexOf("if (localUnlockRequired) {") < appSource.indexOf("if (accountGateRequired && access.profile"));
  assert.match(appSource, /<LaunchTerminal\s+[\s\S]*?key="boot-launch-terminal"/);
  assert.equal(appSource.includes(["Runtime", "Terminal", "Dock"].join("")), false);
  assert.match(mainSource, /let runtimeProjectionHasBeenReady = false;[\s\S]*?const projectionChanged = projectionReady && !runtimeProjectionHasBeenReady;[\s\S]*?if \(projectionReady\) runtimeProjectionHasBeenReady = true;/);
  assert.doesNotMatch(mainSource, /let runtimeProjectionReady = false;/);
  assert.notEqual(submitChatTurnSource, "");
  assert.match(authorizeClientArtifactSource, /if \(!isOperatorChatClientInterfaceCandidate\(request\?\.interfaceId\)\) return;/);
  assert.ok(authorizeClientArtifactSource.indexOf("isOperatorChatClientInterfaceCandidate") < authorizeClientArtifactSource.indexOf("loadOperatorChatClient"));
  assert.match(authorizeDevArtifactSource, /if \(!isOperatorChatClientInterfaceCandidate\(request\?\.interfaceId\)\) return;/);
  assert.ok(authorizeDevArtifactSource.indexOf("isOperatorChatClientInterfaceCandidate") < authorizeDevArtifactSource.indexOf("operatorChatClientLoader.load"));
  assert.doesNotMatch(submitChatTurnSource, /getProjectionStore\("live"\)\.invalidate/);
  assert.match(submitChatTurnSource, /readArtifactPayload:[\s\S]*?readContentAddressedArtifactPayload[\s\S]*?"ledger", "blobs"/);
  assert.notEqual(submitTurnControlSource, "");
  assert.match(submitTurnControlSource, /submitOperatorTurnControl/);
  assert.doesNotMatch(submitTurnControlSource, /prepareLiveProjectionRead|getProjectionStore/);
  assert.notEqual(submitChatSteerSource, "");
  assert.match(submitChatSteerSource, /submitOperatorTurn/);
  assert.match(submitChatSteerSource, /readContentAddressedArtifactPayload/);
  assert.doesNotMatch(submitChatSteerSource, /submitClientOperatorMessageArtifact/);
  assert.doesNotMatch(projectionLiveSource, /onRuntimeHostState/);
  assert.match(runtimeTopologySource, /onRuntimeHostState\?\.\(\(state\) => \{[\s\S]*?clientRuntimeHostObservation\(state\)[\s\S]*?liveProjectionStatus\(next\)/);
  assert.match(mainSource, /backgroundColor: isMac \? "#00000000" : "#181818"/);
  assert.match(mainSource, /transparent: isMac,[\s\S]*?vibrancy: "under-window",[\s\S]*?visualEffectState: "active"/);
  assert.match(mainSource, /const translucent = appearance\?\.translucent !== false;[\s\S]*?const opaqueSurface = !translucent \|\| oversized;[\s\S]*?win\.setVibrancy\("under-window"\)[\s\S]*?windowBackdropModes\.set\(win, opaqueSurface\)/);
  assert.match(launchSource, /<OctopusTerminal id="terminal-panel-launch" session=\{session\} \/>/);
  assert.match(launchSource, /if \(!leaving \|\| phase === "greeting"\) return;/);
  assert.equal(launchSource.includes(["launch", "terminal", "header"].join("-")), false);
  assert.equal(launchSource.includes(["Starting", "Octopus"].join(" ")), false);
  assert.equal(launchSource.includes(["Preparing", "Octopus"].join(" ")), false);
  assert.match(sessionSource, /export function createLaunchShellSession\(\)/);
  assert.match(sessionSource, /bridge\.openLaunchTerminal\(dimensions\)/);
  assert.equal(sessionSource.includes(["create", "Managed", "Runtime", "Terminal", "Session"].join("")), false);
  assert.equal(sessionSource.includes(["read", "Runtime", "Terminal"].join("")), false);
  assert.equal(stylesSource.includes(`.${["launch", "terminal", "header"].join("-")}`), false);
  assert.equal(stylesSource.includes(`.${["runtime", "terminal", "dock"].join("-")}`), false);
  assert.equal(stylesSource.includes(`.${["launch", "scene", "logo", "fill"].join("-")}`), false);
  await verifyBeforeQuitHandlerExecution(mainSource, `${beforeQuitSource}\n${quitCleanupSource}`);
  await verifyStartupPaintOrdering(mainSource);
  verifyLaunchTerminalDeliveryAfterReady(mainSource);
  await verifyLaunchTerminalOpenReadyRace(mainSource);
}

async function verifyStartupPaintOrdering(mainSource) {
  const source = mainSource.match(/const gotSingleInstanceLock = app\.requestSingleInstanceLock\(\);[\s\S]*?(?=\nfunction grantLocalMediaPermissions)/)?.[0] ?? "";
  assert.notEqual(source, "");
  const calls = [];
  const handlers = new Map();
  const context = {
    clientLaunchSelectionPath: "/launch.json",
    resolveOctopusHome: () => "/selected-home",
    resolveOperatorCodexHome: () => "/selected-auth",
    saveClientLaunchSelection: () => {},
    appendClientLog: (...args) => calls.push(["log", ...args]),
    applyThemePreference: () => calls.push(["theme"]),
    createWindow: () => {
      calls.push(["create-window"]);
      const win = {
        focus: () => calls.push(["focus"]),
        isDestroyed: () => false,
        isMinimized: () => false,
        restore: () => calls.push(["restore"]),
      };
      context.primaryWindow = win;
      return win;
    },
    ensureRuntimeHost: () => calls.push(["ensure-observer"]),
    focusAppropriateClientWindow: () => {
      const win = context.primaryWindow || context.createWindow();
      win.focus();
      return win;
    },
    grantLocalMediaPermissions: () => calls.push(["media"]),
    initializeRuntimeHost: () => {
      calls.push(["recover"]);
      return new Promise(() => {});
    },
    loadStoredThemePreference: () => "system",
    primaryWindow: null,
    setAppIdentity: () => calls.push(["identity"]),
    app: {
      on(name, handler) {
        handlers.set(name, handler);
      },
      quit: () => calls.push(["quit"]),
      requestSingleInstanceLock: () => true,
      whenReady: () => Promise.resolve(),
    },
  };
  vm.runInNewContext(source, context, { filename: "main.startup.cjs" });
  await delay(0);
  assert.ok(calls.findIndex(([name]) => name === "create-window") < calls.findIndex(([name]) => name === "recover"));
  assert.equal(calls.some(([name]) => name === "log"), false);
  const secondInstance = handlers.get("second-instance");
  assert.equal(typeof secondInstance, "function");
  context.primaryWindow = null;
  const start = calls.length;
  secondInstance();
  await delay(0);
  const secondCalls = calls.slice(start).map(([name]) => name);
  assert.deepEqual(secondCalls, ["ensure-observer", "create-window", "focus", "recover"]);
}

function verifyLaunchTerminalDeliveryAfterReady(mainSource) {
  const source = mainSource.match(/function publishTerminalEvent\(channel, payload\) \{[\s\S]*?(?=\nfunction terminalOwner)/)?.[0] ?? "";
  assert.notEqual(source, "");
  const sent = [];
  const webContents = {
    id: 73,
    isDestroyed: () => false,
    send: (...args) => sent.push(args),
  };
  const context = {
    BrowserWindow: {
      getAllWindows: () => [{ isDestroyed: () => false, webContents }],
    },
    getClientAccessController: () => ({ requireSession: () => ({ profile: "live" }) }),
    runtimeHost: { snapshot: () => ({ ready: true }) },
    terminalHost: {
      closeOwner: () => assert.fail("launch owner must not be closed"),
      closeSender: () => assert.fail("live launch sender must not be closed"),
    },
  };
  vm.runInNewContext(`${source}\npublishTerminalEvent("octopus:terminal-data", { owner: { scope: "launch", senderId: 73, token: "" }, data: "ready in 4.2s" });`, context, { filename: "main.publish-terminal.cjs" });
  assert.equal(sent.length, 1);
  assert.equal(sent[0][0], "octopus:terminal-data");
  assert.equal(sent[0][1].data, "ready in 4.2s");
}

async function verifyLaunchTerminalOpenReadyRace(mainSource) {
  const source = mainSource.match(/ipcMain\.handle\("octopus:launch-terminal-open"[\s\S]*?(?=\nipcMain\.handle\("octopus:launch-terminal-write")/)?.[0] ?? "";
  assert.notEqual(source, "");
  let handler = null;
  const owner = { scope: "launch", senderId: 73, token: "" };
  const existing = {
    terminalId: "launch",
    sessionId: "existing-launch-session",
    running: true,
    transcript: "ready",
  };
  const calls = [];
  const terminal = {
    openExisting(candidateOwner, request) {
      calls.push(["open-existing", candidateOwner, request]);
      return existing;
    },
    open() {
      assert.fail("a ready-state remount must not create another launch shell");
    },
  };
  const context = {
    getTerminalHost: () => terminal,
    initializeRuntimeHost: () => assert.fail("an existing launch shell must win before a ready-state probe"),
    ipcMain: {
      handle(channel, candidate) {
        assert.equal(channel, "octopus:launch-terminal-open");
        handler = candidate;
      },
    },
    launchTerminalOwner: () => owner,
    terminalHost: terminal,
    trackTerminalSender: () => calls.push(["track-sender"]),
  };
  vm.runInNewContext(source, context, { filename: "main.launch-terminal-open.cjs" });
  assert.equal(typeof handler, "function");
  const result = await handler({ sender: { id: 73 } }, "", { cols: 120, rows: 40 });
  assert.equal(result, existing);
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[0], ["track-sender"]);
  assert.equal(calls[1][0], "open-existing");
  assert.equal(calls[1][1], owner);
  assert.equal(calls[1][2].terminalId, "launch");
  assert.equal(calls[1][2].cols, 120);
  assert.equal(calls[1][2].rows, 40);

  terminal.openExisting = () => null;
  context.terminalHost = null;
  context.initializeRuntimeHost = async () => ({ probe: async () => ({ ready: true }) });
  await assert.rejects(
    handler({ sender: { id: 73 } }, "", { cols: 120, rows: 40 }),
    /already ready for this Home/,
  );
}

async function verifyBeforeQuitHandlerExecution(mainSource, source) {
  const publishRuntimeEventSource = mainSource.match(/function publishRuntimeEvent\(channel, payload\) \{[\s\S]*?(?=\nfunction subscribeRuntimeEvents)/)?.[0] ?? "";
  assert.notEqual(publishRuntimeEventSource, "");
  const calls = [];
  const runtimeEvents = [];
  let handler = null;
  let resolveExit;
  let resolveRecovery;
  const exited = new Promise((resolve) => { resolveExit = resolve; });
  const runtimeInitializationPromise = new Promise((resolve) => { resolveRecovery = resolve; });
  const context = {
    app: {
      on(eventName, candidate) {
        assert.equal(eventName, "before-quit");
        handler = candidate;
      },
      quit() {
        calls.push(["quit"]);
        handler({ preventDefault: () => { throw new Error("Accepted quit was prevented again"); } });
        resolveExit(0);
      },
    },
    appendClientLog: async (...args) => { calls.push(["log", ...args]); },
    closeClientResources() {
      calls.push(["resources-close"]);
    },
    quitCleanupStarted: false,
    quitReady: false,
    clientWindowClose: { requestQuit: () => calls.push(["request-quit"]) },
    nativeBrowserPresentations: null,
    nativeBrowserDownloadHistory: null,
    nativeDesktopPermissions: null,
    nativeDesktopSettings: null,
    nativeDesktopPresentations: null,
    nativeClientPresentations: null,
    nativeBrowserHost: null,
    requireRuntimeStateAccess() {},
    runtimeEventSubscriptions: new Map([[73, {
      sender: {
        isDestroyed: () => false,
        send: (...args) => runtimeEvents.push(args),
      },
      token: "",
    }]]),
    runtimeInitializationPromise,
    runtimeHost: {
      close() {
        calls.push(["runtime-close"]);
      },
      async waitUntilStopped() {
        calls.push(["wait-stopped"]);
        return true;
      },
    },
    terminalHost: {
      closeAll() {
        calls.push(["terminal-close-all"]);
      },
      interruptLaunch() {
        calls.push(["interrupt-launch"]);
        return 1;
      },
      async terminate() {
        calls.push(["terminal-terminate"]);
      },
    },
  };
  vm.runInNewContext(`${publishRuntimeEventSource}\n${source}`, context, { filename: "main.before-quit.cjs" });
  assert.equal(typeof handler, "function");
  assert.equal(context.publishRuntimeEvent("octopus:runtime-host-state", { phase: "unmanaged" }), true);
  assert.deepEqual(runtimeEvents, [["octopus:runtime-host-state", { phase: "unmanaged" }]]);
  runtimeEvents.length = 0;
  handler({ preventDefault: () => calls.push(["prevent-default"]) });
  await delay(0);
  assert.deepEqual(calls, [["prevent-default"], ["request-quit"]]);
  assert.equal(context.publishRuntimeEvent("octopus:runtime-host-state", { phase: "unmanaged" }), true);
  runtimeEvents.length = 0;
  calls.push(["close-approved"]);
  context.startClientQuitCleanup();
  await delay(0);
  assert.deepEqual(calls, [["prevent-default"], ["request-quit"], ["close-approved"]]);
  assert.equal(context.publishRuntimeEvent("octopus:runtime-host-state", { phase: "unmanaged" }), false);
  assert.deepEqual(runtimeEvents, []);
  calls.push(["recovery-complete"]);
  resolveRecovery();
  const exitCode = await Promise.race([
    exited,
    delay(1_000).then(() => { throw new Error("Approved before-quit cleanup did not finish"); }),
  ]);
  assert.equal(exitCode, 0);
  assert.deepEqual(calls, [
    ["prevent-default"],
    ["request-quit"],
    ["close-approved"],
    ["recovery-complete"],
    ["interrupt-launch"],
    ["wait-stopped"],
    ["terminal-terminate"],
    ["runtime-close"],
    ["terminal-close-all"],
    ["resources-close"],
    ["quit"],
    ["resources-close"],
  ]);
}

async function verifyRuntimeReadinessEvidence() {
  const root = await fs.mkdtemp(process.platform === "win32"
    ? path.join(os.tmpdir(), "octopus-runtime-observer-")
    : "/tmp/octopus-runtime-observer-");
  const home = path.join(root, "home");
  const registry = path.join(root, "registry");
  const runtimeBinary = path.join(root, "octopus");
  const endpoint = path.join(home, "app-server-control", "app-server-control.sock");
  const launchEnv = { OCTOPUS_HOME: home, OCTOPUS_SOURCE_REGISTRY: registry };
  const host = createRuntimeHost({
    home: () => home,
    sourceRegistry: () => registry,
    runtimeBinary: () => runtimeBinary,
    repoRoot: () => root,
    launchCwd: () => root,
    launchCommand: () => `${runtimeBinary} up`,
    launchEnv: () => launchEnv,
    endpoint: () => endpoint,
  });
  let server = null;
  try {
    await fs.mkdir(registry, { recursive: true });
    await fs.writeFile(runtimeBinary, "fixture", { mode: 0o755 });
    const absent = await host.probe();
    assert.equal(absent.phase, "unmanaged");
    assert.equal(absent.ready, false);
    assert.equal(absent.ownership, "none");
    assert.equal(typeof host.start, "undefined");
    assert.equal(typeof host.stopManaged, "undefined");
    assert.equal(absent.launchCwd, root);
    assert.equal(absent.launchCommand, `${runtimeBinary} up`);
    assert.deepEqual(absent.launchEnv, launchEnv);
    await assert.rejects(host.waitUntilReady(25), /did not become ready/);

    const stateRoot = path.join(home, "state", "runtime-instance");
    await fs.mkdir(stateRoot, { recursive: true });
    const now = Date.now();
    await fs.writeFile(path.join(stateRoot, "state.json"), JSON.stringify({
      schema_version: 3,
      pid: process.pid,
      phase: "ready",
      started_at_unix_ms: now,
      updated_at_unix_ms: now,
      launch_owner: null,
    }));
    const readyWithoutGenericChat = await host.waitUntilReady(500);
    assert.equal(readyWithoutGenericChat.phase, "ready");
    assert.equal(readyWithoutGenericChat.ready, true);
    assert.equal(readyWithoutGenericChat.ownership, "external");
    assert.equal(readyWithoutGenericChat.pid, process.pid);

    await fs.mkdir(path.dirname(endpoint), { recursive: true });
    server = net.createServer((socket) => socket.end());
    await listen(server, endpoint);
    const ready = await host.probe();
    assert.equal(ready.phase, "ready");
    assert.equal(ready.ready, true);
    assert.equal(ready.ownership, "external");
    assert.equal(ready.pid, process.pid);

    await fs.writeFile(path.join(stateRoot, "state.json"), JSON.stringify({
      schema_version: 3,
      pid: process.pid,
      phase: "ready",
      started_at_unix_ms: now,
      updated_at_unix_ms: now,
      launch_owner: { protocol: "client_boot_handoff_v1" },
    }));
    const legacyOwner = await host.probe();
    assert.equal(legacyOwner.phase, "degraded");
    assert.equal(legacyOwner.ready, false);
    assert.match(legacyOwner.error, /malformed/);

    await fs.writeFile(path.join(stateRoot, "state.json"), JSON.stringify({
      schema_version: 3,
      pid: process.pid,
      phase: "ready",
      started_at_unix_ms: now,
      updated_at_unix_ms: now,
      launch_owner: null,
    }));
    assert.equal((await host.probe()).ready, true);

    await fs.rm(stateRoot, { recursive: true });
    const endpointConflict = await host.probe();
    assert.equal(endpointConflict.phase, "degraded");
    assert.equal(endpointConflict.pid, null);
    assert.equal(await host.waitUntilStopped(50), false);
    await closeServer(server);
    server = null;
    assert.equal(await host.waitUntilStopped(500), true);
  } finally {
    host.close();
    await closeServer(server);
    await fs.rm(root, { recursive: true, force: true });
  }
}

async function verifyBootTerminalBehavior() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-boot-terminal-"));
  const host = createTerminalHost();
  const enterOwner = { scope: "launch", senderId: 101, token: "" };
  const editOwner = { scope: "launch", senderId: 202, token: "" };
  const enteredPath = path.join(root, "entered.txt");
  const cancelledPath = path.join(root, "cancelled.txt");
  const replacementPath = path.join(root, "replacement.txt");
  const sameShellPath = path.join(root, "same-shell.txt");
  const enterCommand = `printf entered > ${shellQuote(enteredPath)}`;
  const cancelledCommand = `printf cancelled > ${shellQuote(cancelledPath)}`;
  try {
    const entered = host.open(enterOwner, {
      terminalId: "launch",
      cwd: root,
      env: { OCTOPUS_TEST_BOOT: "1" },
      preparedCommand: enterCommand,
      cols: 90,
      rows: 24,
    });
    await waitForTerminalData(host, entered.sessionId, "printf entered");
    await assert.rejects(fs.access(enteredPath));
    host.write(enterOwner, entered.terminalId, entered.sessionId, "\r");
    await waitForPath(enteredPath);
    assert.equal(await fs.readFile(enteredPath, "utf8"), "entered");

    const edited = host.open(editOwner, {
      terminalId: "launch",
      cwd: root,
      preparedCommand: cancelledCommand,
      cols: 90,
      rows: 24,
    });
    await waitForTerminalData(host, edited.sessionId, "printf cancelled");
    await assert.rejects(fs.access(cancelledPath));
    host.write(editOwner, edited.terminalId, edited.sessionId, `\x15printf replacement > ${shellQuote(replacementPath)}\r`);
    await waitForPath(replacementPath);
    await assert.rejects(fs.access(cancelledPath));

    host.write(editOwner, edited.terminalId, edited.sessionId, "sleep 30\r");
    await delay(150);
    host.write(editOwner, edited.terminalId, edited.sessionId, "\x03");
    await delay(150);
    host.write(editOwner, edited.terminalId, edited.sessionId, `printf same-shell > ${shellQuote(sameShellPath)}\r`);
    await waitForPath(sameShellPath);
    const existingOnly = host.openExisting(editOwner, { terminalId: "launch", cols: 100, rows: 32 });
    assert.equal(existingOnly.sessionId, edited.sessionId);
    assert.equal(host.openExisting({ scope: "launch", senderId: 303, token: "" }, { terminalId: "launch" }), null);
    const reopened = host.open(editOwner, { terminalId: "launch", cols: 100, rows: 32 });
    assert.equal(reopened.sessionId, edited.sessionId);
  } finally {
    host.closeAll();
    await fs.rm(root, { recursive: true, force: true });
  }
}

async function verifyBootTerminalQuit() {
  if (process.platform === "win32") return;
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-boot-terminal-quit-"));
  const host = createTerminalHost();
  const owner = { scope: "launch", senderId: 212, token: "" };
  const childPidPath = path.join(root, "child.pid");
  const hangupPath = path.join(root, "hangup.txt");
  const script = `trap 'printf hangup > ${shellQuote(hangupPath)}; exit 0' HUP; trap '' INT TERM; printf '%s' "$$" > ${shellQuote(childPidPath)}; while :; do sleep 1; done`;
  const command = `/bin/sh -c ${shellQuote(script)}`;
  let childPid = null;
  try {
    const opened = host.open(owner, {
      terminalId: "launch",
      cwd: root,
      preparedCommand: command,
      cols: 90,
      rows: 24,
    });
    await waitForTerminalData(host, opened.sessionId, "/bin/sh -c");
    host.write(owner, opened.terminalId, opened.sessionId, "\r");
    await waitForPath(childPidPath);
    childPid = Number(await fs.readFile(childPidPath, "utf8"));
    assert.equal(Number.isSafeInteger(childPid) && childPid > 0, true);
    assert.equal(host.interruptLaunch(), 1);
    await delay(75);
    await assert.rejects(fs.access(hangupPath));
    await host.terminate(50);
    await waitForPath(hangupPath);
    assert.equal(host.runningLaunchCount(), 0);
  } finally {
    host.closeAll();
    if (Number.isSafeInteger(childPid) && childPid > 0) {
      try {
        process.kill(childPid, "SIGKILL");
      } catch (error) {
        if (error?.code !== "ESRCH") throw error;
      }
    }
    await fs.rm(root, { recursive: true, force: true });
  }
}

async function verifyTerminalOwnership() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-terminal-test-"));
  const host = createTerminalHost();
  const owner = { scope: "access", senderId: 101, token: "owner-a" };
  const foreign = { scope: "access", senderId: 102, token: "owner-b" };
  const marker = `terminal-smoke-${Date.now()}`;
  try {
    assert.throws(() => host.open(owner, { terminalId: "invalid:id", cwd: root }), /terminal id/);
    const opened = host.open(owner, { terminalId: "user-smoke", cwd: root, cols: 90, rows: 24 });
    assert.equal(opened.cwd, root);
    assert.throws(() => host.write(foreign, opened.terminalId, opened.sessionId, "pwd\r"), /missing or stale/);
    const observed = waitForTerminalData(host, opened.sessionId, marker);
    host.write(owner, opened.terminalId, opened.sessionId, `printf '${marker}\\n'\r`);
    const event = await observed;
    assert.deepEqual(event.owner, owner);
    assert.equal(host.close(foreign, opened.terminalId), false);
    assert.equal(host.close(owner, opened.terminalId), true);
  } finally {
    host.closeAll();
    await fs.rm(root, { recursive: true, force: true });
  }
}

function waitForTerminalData(host, sessionId, marker) {
  return new Promise((resolve, reject) => {
    let transcript = "";
    const timeout = setTimeout(() => {
      host.off("data", onData);
      reject(new Error(`terminal output '${marker}' timed out`));
    }, 8_000);
    const onData = (event) => {
      if (event.sessionId !== sessionId) return;
      transcript += event.data;
      if (!transcript.includes(marker)) return;
      clearTimeout(timeout);
      host.off("data", onData);
      resolve(event);
    };
    host.on("data", onData);
  });
}

async function waitForPath(filePath) {
  const deadline = Date.now() + 8_000;
  while (Date.now() < deadline) {
    if (await fs.access(filePath).then(() => true, () => false)) return;
    await delay(25);
  }
  throw new Error(`path '${filePath}' did not appear`);
}

function shellQuote(value) {
  return `'${value.replaceAll("'", `'\\''`)}'`;
}

function listen(server, endpoint) {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(endpoint, () => {
      server.off("error", reject);
      resolve();
    });
  });
}

function closeServer(server) {
  if (!server?.listening) return Promise.resolve();
  return new Promise((resolve) => server.close(resolve));
}

function verifyOperatorChatSubmitResult() {
  assert.equal(isOperatorChatClientInterfaceCandidate("world_operator_chat_abc123"), true);
  assert.equal(isOperatorChatClientInterfaceCandidate("world_telegram"), false);
  const value = { runId: "run_operator_chat_home" };
  assert.equal(unwrapOperatorChatSubmitResult(operatorChatSubmitSuccess(value)), value);
  assert.throws(
    () => unwrapOperatorChatSubmitResult(operatorChatSubmitFailure()),
    (error) => error.message === OPERATOR_CHAT_SUBMIT_ERROR_MESSAGE
      && !error.message.includes("policy")
      && !error.message.includes("system_worker"),
  );
  assert.throws(
    () => unwrapOperatorChatSubmitResult(operatorChatSubmitFailure({
      code: "operator_chat_client_contract_mismatch",
    })),
    (error) => error.code === "chat_package_repair_required"
      && error.message === "Octopus Home chat package needs repair before sending."
      && error.retryable === false,
  );
  assert.throws(
    () => unwrapOperatorChatSubmitResult({ error: { message: "policy: system_worker leaked" } }),
    (error) => error.message === OPERATOR_CHAT_SUBMIT_ERROR_MESSAGE,
  );
}

async function verifyGitIndexMutation() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-git-test-"));
  const filePath = path.join(root, "sample.txt");
  const runId = "run-smoke";
  const workerEnvironmentId = "we-smoke";
  const surfaceId = "review-smoke";
  const descriptor = {
    data: {},
    formationRevision: "formation-1",
    kind: "changes",
    mode: "direct",
    protocol: "git.worktree",
    readOnly: false,
    root,
    runId,
  };
  const actions = createWorkerEnvironmentOperatorActions({ resolveSurface: async () => descriptor });
  const git = async (...args) => execFileAsync("git", args, { cwd: root, encoding: "utf8" });
  const request = (snapshot, action) => ({
    action,
    paths: ["sample.txt"],
    projectionRevision: snapshot.revision,
    runId,
    surfaceId,
    workerEnvironmentId,
  });
  try {
    await git("init", "-q");
    await git("config", "user.email", "octopus@example.invalid");
    await git("config", "user.name", "Octopus Host Test");
    await fs.writeFile(filePath, "before\n");
    await git("add", "sample.txt");
    await git("commit", "-qm", "base");
    await fs.writeFile(filePath, "after\n");

    let snapshot = await actions.inspect({ runId, surfaceId, workerEnvironmentId });
    await actions.perform(request(snapshot, "stage"));
    assert.equal((await git("show", ":sample.txt")).stdout, "after\n");

    snapshot = await actions.inspect({ runId, surfaceId, workerEnvironmentId });
    await actions.perform(request(snapshot, "unstage"));
    assert.equal((await git("diff", "--cached", "--name-only")).stdout, "");

    snapshot = await actions.inspect({ runId, surfaceId, workerEnvironmentId });
    await actions.perform(request(snapshot, "stage"));
    snapshot = await actions.inspect({ runId, surfaceId, workerEnvironmentId });
    await actions.perform(request(snapshot, "revert"));
    assert.equal(await fs.readFile(filePath, "utf8"), "before\n");
    assert.equal((await git("status", "--porcelain")).stdout, "");
    await fs.writeFile(filePath, "after\n");

    snapshot = await actions.inspect({ runId, surfaceId, workerEnvironmentId });
    const indexPath = path.join(root, ".git", "index");
    const lockPath = `${indexPath}.lock`;
    await fs.writeFile(lockPath, "busy", { flag: "wx" });
    await assert.rejects(actions.perform(request(snapshot, "stage")), /index is busy/);
    await fs.rm(lockPath, { force: true });
    assert.equal((await git("diff", "--cached", "--name-only")).stdout, "");

    snapshot = await actions.inspect({ runId, surfaceId, workerEnvironmentId });
    await fs.writeFile(filePath, "changed concurrently\n");
    await assert.rejects(actions.perform(request(snapshot, "stage")), /changed since this review was loaded/);
    assert.equal((await git("diff", "--cached", "--name-only")).stdout, "");
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
}

async function verifyProviderCredentialRepair() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-provider-repair-test-"));
  const octopusHome = path.join(root, "octopus-home");
  const codexHome = path.join(root, "codex-home");
  const descriptorRoot = path.join(octopusHome, "registry", "interface", "world_provider_codex");
  const authPath = path.join(codexHome, "auth.json");
  const machineAuth = JSON.stringify({
    auth_mode: "chatgpt",
    tokens: {
      access_token: "machine-access",
      refresh_token: "machine-refresh",
      id_token: "machine-id",
    },
  });
  try {
    await fs.mkdir(descriptorRoot, { recursive: true });
    await fs.mkdir(codexHome, { recursive: true });
    await fs.writeFile(path.join(descriptorRoot, "interface.json"), JSON.stringify({
      id: "world_provider_codex",
      interface_driver_config: {
        residual_ambient: {
          private_auth_store: { auth_file_env: "OCTOPUS_MACHINE_CODEX_AUTH_FILE" },
        },
      },
    }));
    await fs.writeFile(authPath, machineAuth, { mode: 0o600 });
    const request = {
      actionId: PROVIDER_CREDENTIAL_REPAIR_ACTION,
      extensionId: PROVIDER_CREDENTIAL_REPAIR_EXTENSION,
      incident: {
        code: PROVIDER_AUTHENTICATION_INCIDENT,
        id: "movement:7:turn.error",
        interfaceId: "world_provider_codex",
      },
    };
    await assert.rejects(executeClientExtensionAction({
      codexHome,
      installedExtensionIds: [],
      octopusHome,
    }, request), /not installed/);

    const outcome = await executeClientExtensionAction({
      codexHome,
      installedExtensionIds: [PROVIDER_CREDENTIAL_REPAIR_EXTENSION],
      octopusHome,
    }, request);
    assert.equal(outcome.credentialUpdated, false);
    assert.equal(outcome.runtimeRestarted, false);
    assert.equal(outcome.sharedCredential, true);
    assert.equal(await fs.readFile(authPath, "utf8"), machineAuth);
    assert.equal(JSON.stringify(outcome).includes("machine-access"), false);
    assert.equal(JSON.stringify(outcome).includes(authPath), false);

    const [status] = await readClientExtensionActionStatuses({
      codexHome,
      installedExtensionIds: [PROVIDER_CREDENTIAL_REPAIR_EXTENSION],
      octopusHome,
    });
    assert.equal(status.state, "configured");
    assert.equal(status.actionAvailable, false);

    await fs.writeFile(authPath, "{}", { mode: 0o600 });
    await assert.rejects(executeClientExtensionAction({
      codexHome,
      installedExtensionIds: [PROVIDER_CREDENTIAL_REPAIR_EXTENSION],
      octopusHome,
    }, request), /complete ChatGPT token data/);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function testClientWindow(webContentsId) {
  const win = new EventEmitter();
  let destroyed = false;
  let focused = false;
  win.webContents = { id: webContentsId };
  win.isDestroyed = () => destroyed;
  win.isFocused = () => focused;
  win.focus = () => {
    focused = true;
    win.emit("focus");
  };
  win.close = () => {
    if (destroyed) return;
    destroyed = true;
    win.emit("closed");
  };
  return win;
}

void main().catch((error) => {
  process.stderr.write(`${error?.stack || error}\n`);
  app.exit(1);
});
