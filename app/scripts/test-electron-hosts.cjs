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
  const preload = loadPreloadTransport(source, {
    "octopus:access-inherit": (claim) => claim === "claim-1" ? "token-1" : "",
    "octopus:client-window-bootstrap": () => ({ windowId: "window-1" }),
    "octopus:client-pane-transfer-begin": (token, request) => token === "token-1" && request.tabId !== "refused"
      ? { ok: true, value: { transferId: "transfer-1" } } : { ok: false, error: { message: "Pane is not transferable." } },
    "octopus:client-pane-transfer-acknowledge": (token, request) => ({ ok: true, value: { released: true, transferId: request.transferId } }),
  });
  assert.equal(preload.name, "octopusTransport");
  const request = { marker: "request" };
  for (const [method, args, channel] of [
    ["openLaunchTerminal", [request], "octopus:launch-terminal-open"],
    ["writeLaunchTerminal", ["session-1", "ls\n"], "octopus:launch-terminal-write"],
    ["resizeLaunchTerminal", ["session-1", request], "octopus:launch-terminal-resize"],
    ["bootStatus", [], "octopus:boot-status"],
    ["openClientWindow", [request], "octopus:client-window-open"],
    ["clientWindowReady", [request], "octopus:client-window-ready"],
    ["acceptClientPaneTransfer", [request], "octopus:client-pane-transfer-accept"],
    ["commitClientPaneTransfer", [request], "octopus:client-pane-transfer-commit"],
    ["pendingClientPaneTransfers", [], "octopus:client-pane-transfer-pending"],
    ["pendingClientPaneDestinationTransfers", [], "octopus:client-pane-transfer-destination-pending"],
    ["resumeClientPaneTransfer", [request], "octopus:client-pane-transfer-resume"],
    ["finalizeClientPaneTransfer", [request], "octopus:client-pane-transfer-finalize"],
    ["submitOperatorTurnControl", [request], "octopus:submit-operator-turn-control"],
  ]) {
    preload.calls.length = 0;
    await preload.api[method](...args);
    assert.deepEqual(preload.calls, [["invoke", channel, "token-1", ...args]], `${method} invokes ${channel} under the window's access token`);
  }
  preload.calls.length = 0;
  assert.deepEqual(preload.api.clientWindowBootstrap(), { windowId: "window-1" });
  assert.deepEqual(preload.calls, [["sendSync", "octopus:client-window-bootstrap"]]);
  assert.deepEqual(preload.api.beginClientPaneTransfer({ tabId: "chat" }), { transferId: "transfer-1" });
  assert.throws(() => preload.api.beginClientPaneTransfer({ tabId: "refused" }), /Pane is not transferable\./);
  assert.deepEqual(preload.api.acknowledgeClientPaneTransfer({ transferId: "transfer-1" }), { released: true, transferId: "transfer-1" });
  const completions = [];
  const stopCompletions = preload.api.onClientPaneTransferCompleted((completion) => completions.push(completion));
  preload.emit("octopus:client-pane-transfer-completed", { transferId: "transfer-1" });
  stopCompletions();
  preload.emit("octopus:client-pane-transfer-completed", { transferId: "transfer-2" });
  assert.deepEqual(completions, [{ transferId: "transfer-1" }]);
  preload.calls.length = 0;
  const hostStates = [];
  const stopHostStates = preload.api.onRuntimeHostState((state) => hostStates.push(state));
  preload.emit("octopus:runtime-host-state", { phase: "ready" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(hostStates, [{ phase: "ready" }]);
  assert.deepEqual(preload.calls.map(([kind, channel, token]) => [kind, channel, token]), [
    ["invoke", "octopus:runtime-events-unsubscribe", "token-1"], ["invoke", "octopus:runtime-events-subscribe", "token-1"],
  ]);
  stopHostStates();
  assert.deepEqual(preload.calls.at(-1).slice(0, 3), ["invoke", "octopus:runtime-events-unsubscribe", "token-1"]);
  const locks = [];
  preload.api.onClientAccessLocked((status) => locks.push(status));
  preload.calls.length = 0;
  preload.emit("octopus:client-access-locked", { locked: true });
  assert.deepEqual(locks, [{ locked: true }]);
  assert.deepEqual(preload.calls, [["invoke", "octopus:runtime-events-unsubscribe", "token-1"]]);
  preload.calls.length = 0;
  await preload.api.bootStatus();
  assert.deepEqual(preload.calls, [["invoke", "octopus:boot-status", ""]], "a locked window stops presenting its access token");
}

function loadPreloadTransport(source, sendSyncReplies) {
  const calls = [];
  const listeners = new Map();
  let exposed = null;
  const ipcRenderer = {
    invoke: (channel, ...args) => { calls.push(["invoke", channel, ...args]); return Promise.resolve(null); },
    sendSync: (channel, ...args) => { calls.push(["sendSync", channel, ...args]); return sendSyncReplies[channel]?.(...args); },
    send: (channel, ...args) => { calls.push(["send", channel, ...args]); },
    on: (channel, listener) => { listeners.set(channel, [...(listeners.get(channel) ?? []), listener]); },
    removeListener: (channel, listener) => { listeners.set(channel, (listeners.get(channel) ?? []).filter((candidate) => candidate !== listener)); },
  };
  const electron = {
    contextBridge: { exposeInMainWorld: (name, api) => { exposed = { name, api }; } },
    ipcRenderer,
    webFrame: { getZoomFactor: () => 1 },
    webUtils: { getPathForFile: () => "" },
  };
  vm.runInNewContext(source, {
    require: (id) => { assert.equal(id, "electron"); return electron; },
    process: { argv: ["electron", "--octopus-access-claim=claim-1"] },
    console, setTimeout, clearTimeout, setImmediate, URL, Error, Promise,
  });
  return {
    ...exposed,
    calls,
    emit: (channel, ...args) => { for (const listener of listeners.get(channel) ?? []) listener({}, ...args); },
  };
}

async function verifyClientWindowHostContract() {
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

    await fs.writeFile(authPath, machineAuth, { mode: 0o600 });
    await fs.writeFile(path.join(descriptorRoot, "interface.json"), JSON.stringify({
      id: "world_provider_codex",
      interface_driver_config: { residual_ambient: { private_auth_store: { auth_file_env: "CODEX_HOME" } } },
    }));
    await assert.rejects(executeClientExtensionAction({
      codexHome,
      installedExtensionIds: [PROVIDER_CREDENTIAL_REPAIR_EXTENSION],
      octopusHome,
    }, request), (error) => error.code === "provider_auth_store_unsupported");
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
