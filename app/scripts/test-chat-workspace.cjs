const assert = require("node:assert/strict");
const path = require("node:path");
const { test, after } = require("node:test");
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<div id="root"></div>', { url: "http://fixture.invalid", pretendToBeVisual: true });
const w = dom.window;
for (const key of ["window", "document", "HTMLElement", "Element", "Node", "Event", "CustomEvent", "localStorage", "sessionStorage"]) {
  Object.defineProperty(globalThis, key, { configurable: true, value: key === "window" ? w : w[key] });
}
globalThis.requestAnimationFrame = w.requestAnimationFrame.bind(w);
w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });

function memoryIndexedDb() {
  const stores = new Map();
  const later = (callback) => setTimeout(callback, 0);
  const database = {
    createObjectStore: (name) => { stores.set(name, new Map()); },
    close() {},
    transaction(name) {
      const store = stores.get(name);
      const writes = [];
      const transaction = {
        aborted: false,
        error: null,
        abort() { transaction.aborted = true; later(() => transaction.onabort?.()); },
        objectStore: () => ({
          get(key) {
            const request = {};
            later(() => {
              request.result = store.has(key) ? structuredClone(store.get(key)) : undefined;
              request.onsuccess?.();
              later(() => {
                if (transaction.aborted) return;
                for (const [value, writeKey] of writes) store.set(writeKey, structuredClone(value));
                transaction.oncomplete?.();
              });
            });
            return request;
          },
          put(value, key) { writes.push([value, key]); },
        }),
      };
      return transaction;
    },
  };
  return {
    open() {
      const request = { result: database };
      later(() => {
        if (!stores.has("queues")) request.onupgradeneeded?.();
        request.onsuccess?.();
      });
      return request;
    },
  };
}
globalThis.indexedDB = memoryIndexedDb();
globalThis.BroadcastChannel = class extends BroadcastChannel { constructor(name) { super(name); this.unref(); } };
const heldLocks = new Set();
Object.defineProperty(globalThis, "navigator", { configurable: true, value: {
  userAgent: "fixture",
  locks: {
    request: async (name, callback) => { heldLocks.add(name); try { return await callback(); } finally { heldLocks.delete(name); } },
    query: async () => ({ held: [...heldLocks].map((name) => ({ name })) }),
  },
} });

const React = require("react");
const { createRoot } = require("react-dom/client");
const compiled = build({
  stdin: {
    contents: 'export { useChatWorkspace } from "./src/components/chat/useChatWorkspace"; export { ClientPreferencesProvider } from "./src/clientPreferences";',
    resolveDir: path.join(__dirname, ".."), loader: "ts",
  },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  define: { "import.meta.env.DEV": "false", "import.meta.env.PROD": "true" },
  loader: { ".css": "empty", ".svg": "dataurl", ".png": "dataurl" },
  external: ["react", "react-dom", "react-dom/client", "react/jsx-runtime", "motion/react"],
}).then((result) => {
  const module = { exports: {} };
  Function("require", "module", "exports", result.outputFiles[0].text)(require, module, module.exports);
  return module.exports;
});

const provider = {
  id: "provider.test", label: "Test provider", descriptorPath: "registry/interface/provider.test", sourceNames: [], supportedReasoningEfforts: ["medium"],
  models: [{ model: "model-a", displayName: "Model A", description: "", defaultReasoningEffort: "medium", supportedReasoningEfforts: ["medium"], showInPicker: true, isDefault: true }],
};
const runRow = (runId, extra = {}) => ({
  run_id: runId, title: `Task ${runId}`, chat_interface_id: `chat.${runId}`, client_surface_role: "task", writable: true, ee_id: "ee.root",
  provider_interface_id: provider.id, provider_bound_interface_id: provider.id, provider_model: "model-a", reasoning_effort: "medium", worker_interface_id: "worker.test",
  turn_open: false, turn_state: "idle", last_movement_seq: 2, updated_at_ms: 1_000, ...extra,
});
const messageRow = (runId, seq, role, text, extra = {}) => ({
  id: `${runId}:${seq}`, run_id: runId, role, text, created_at_ms: seq * 100, movement_seq: seq, artifact_id: `${runId}.artifact.${seq}`,
  direction: role === "user" ? "in" : "out", interface_id: `chat.${runId}`, event_kind: "message", terminal: true, ...extra,
});
const movementArtifact = (row) => ({
  source: "movement", recorded_at_unix_ms: row.created_at_ms, run_id: row.run_id, interface_id: row.interface_id,
  from_interface_id: row.role === "user" ? row.interface_id : provider.id, from_interface_role: row.role === "user" ? "operator" : "provider",
  label: row.role === "user" ? "operator.message" : "provider.message", event_kind: row.event_kind, event_data: row.event_data ?? {}, text: row.text,
  error: row.error === true, terminal: row.terminal !== false, will_retry: false, movement_seq: row.movement_seq, checkpoint_id: `checkpoint.${row.movement_seq}`,
  artifact_id: row.artifact_id, content_hash: "",
});
const control = (runId, action, controlId) => ({ role: "assistant", event_kind: "turn_control",
  event_data: { action, status: "acknowledged", control_id: controlId, _movement: { from: { Interface: provider.id }, to: { Interface: `chat.${runId}` } } } });

const projectionListeners = new Set();
let currentHost = null;
w.octopusTransport = new Proxy({}, { get: (_, name) => name === "onProjectionChanged"
  ? (listener) => { projectionListeners.add(listener); return () => projectionListeners.delete(listener); }
  : currentHost?.transport[name] });

function fakeHost() {
  const host = {
    seq: 2,
    runs: [runRow("run-1")],
    messages: { "run-1": [messageRow("run-1", 1, "user", "Draft the report"), messageRow("run-1", 2, "assistant", "Here is the report.")] },
    reads: [], turns: [], steers: [], controls: [], waiters: new Set(), holdMessages: null, failNextTurn: null, recoverNextTurn: false,
    run: (runId) => host.runs.find((run) => run.run_id === runId),
    changed(kind) { if (currentHost === host) for (const listener of [...projectionListeners]) listener({ kind }); },
    append(runId, role, text, extra = {}) {
      const seq = ++host.seq;
      host.messages[runId] = [...(host.messages[runId] ?? []), messageRow(runId, seq, role, text, extra)];
      host.run(runId).last_movement_seq = seq;
      setTimeout(() => {
        host.changed("ledger");
        for (const waiter of [...host.waiters]) if (waiter()) host.waiters.delete(waiter);
      }, 0);
      return seq;
    },
    reply(runId, text) {
      host.append(runId, "assistant", text);
      Object.assign(host.run(runId), { turn_open: false, turn_state: "idle" });
    },
  };
  host.transport = {
    projectionRead: async (request) => {
      host.reads.push(request.method);
      switch (request.method) {
        case "projection.operator.chats.list": return { chats: structuredClone(host.runs) };
        case "projection.operator.chats.status": return { chats: host.runs.filter((run) => request.params.run_ids.includes(run.run_id)).map((run) => ({ ...run })) };
        case "projection.operator.chat.messages":
          if (host.holdMessages) await host.holdMessages;
          return { messages: structuredClone(host.messages[request.params.run_id] ?? []) };
        case "projection.operator.chat.contexts": return { contexts: [{ ee_id: "ee.root", is_os_root: true, display_name: "Root", workers: [{ id: "worker.test", label: "Worker", provider_interface_ids: [provider.id], interface_policy: { [provider.id]: { models: ["model-a"] } } }] }] };
        case "projection.worker_environments.list": return { worker_environments: [] };
        case "projection.tools.list": return { tools: [] };
        case "projection.operator.providers.status": return { providers: [] };
        default: return {};
      }
    },
    providerInterfaces: async () => ({ repoRoot: "/repo", providers: [structuredClone(provider)] }),
    defaults: async () => ({ endpoint: "http://runtime.invalid" }),
    readOperatorChatArtifacts: (request) => {
      const after = Math.max(request.afterSeq, request.cursor?.seq ?? -1);
      const batch = () => {
        const rows = (host.messages[request.runId] ?? []).filter((row) => row.movement_seq > after);
        return rows.length ? { artifacts: rows.map(movementArtifact), cursor: { offset: 0, seq: rows.at(-1).movement_seq } } : null;
      };
      const ready = batch();
      if (ready) return Promise.resolve(ready);
      return new Promise((resolve) => host.waiters.add(() => { const next = batch(); if (next) resolve(next); return Boolean(next); }));
    },
    submitChatTurn: async (request) => {
      host.turns.push(structuredClone(request));
      const runId = request.runId;
      if (host.failNextTurn) {
        const { failure, admitted } = host.failNextTurn;
        host.failNextTurn = null;
        if (admitted) { host.append(runId, "user", request.text, { id: request.messageId }); Object.assign(host.run(runId), { turn_open: true, turn_state: "working" }); }
        throw failure;
      }
      const recovered = host.recoverNextTurn;
      host.recoverNextTurn = false;
      const seq = recovered ? host.messages[runId].find((row) => row.id === request.messageId).movement_seq : host.append(runId, "user", request.text, { id: request.messageId });
      if (!recovered) Object.assign(host.run(runId), { turn_open: true, turn_state: "working" });
      return { runId, chatInterfaceId: `chat.${runId}`, messageVerdict: { admitted: { seq } }, outboundCursor: null, messageRecovered: recovered };
    },
    submitChatSteer: async (request) => {
      host.steers.push(structuredClone(request));
      const seq = host.append(request.runId, "user", request.text, { id: request.messageId, event_kind: "steering" });
      return { runId: request.runId, messageVerdict: { admitted: { seq } } };
    },
    submitOperatorTurnControl: async (request) => { host.controls.push(structuredClone(request)); return { accepted: true }; },
  };
  return host;
}

const container = document.getElementById("root");
const root = createRoot(container);
after(() => { root.unmount(); w.close(); });
const settle = async (rounds = 6) => { for (let round = 0; round < rounds; round += 1) await new Promise((resolve) => setTimeout(resolve, 10)); };

async function mount(host, profile = "lab", home = `/home/${Math.random().toString(16).slice(2)}`) {
  const { useChatWorkspace, ClientPreferencesProvider } = await compiled;
  currentHost = host;
  const view = { current: null };
  function Harness() { view.current = useChatWorkspace(profile, true, true, "fixture", "task", home); return null; }
  root.render(React.createElement(ClientPreferencesProvider, { profile }, React.createElement(Harness)));
  await settle(12);
  return view;
}

const until = async (predicate, message) => {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail(typeof message === "function" ? message() : message);
};
async function openTask(host, profile) {
  const view = await mount(host, profile);
  view.current.selectRun("run-1");
  await until(() => !view.current.loadingMessages && view.current.renderedMessages.length >= 2 && view.current.canSubmit === false && view.current.submitBlockReason === "Type a message or attach a file.", `task never became ready: ${view.current.submitBlockReason}`);
  return view;
}
async function type(view, text) {
  view.current.setText(text);
  await until(() => view.current.text === text, "the composer never took the text");
}
const working = (view) => view.current.renderedMessages.filter((message) => message.pending && message.role === "assistant");

test("refreshing models re-reads the provider catalog only in the live client", { timeout: 15_000 }, async () => {
  for (const [profile, refreshes] of [["live", 1], ["lab", 0]]) {
    const host = fakeHost();
    let catalogReads = 0;
    const read = host.transport.providerInterfaces;
    host.transport.providerInterfaces = async () => { catalogReads += 1; return read(); };
    const view = await openTask(host, profile);
    const before = catalogReads;
    await view.current.refreshProviderModels();
    await settle();
    assert.equal(catalogReads - before, refreshes, profile);
  }
});

test("a live client refreshes task status from ledger changes and ignores stale status", { timeout: 15_000 }, async () => {
  const host = fakeHost();
  const view = await openTask(host, "live");
  const reads = (method) => host.reads.filter((read) => read === method).length;
  const before = { status: reads("projection.operator.chats.status"), list: reads("projection.operator.chats.list") };
  Object.assign(host.run("run-1"), { turn_state: "waiting", turn_open: true, last_movement_seq: 3 });
  host.changed("ledger");
  await until(() => view.current.runs.find((run) => run.runId === "run-1")?.turnState === "waiting", "the ledger change did not refresh status");
  assert.ok(reads("projection.operator.chats.status") > before.status);
  assert.equal(reads("projection.operator.chats.list"), before.list, "a ledger change refreshes status without reloading the task list");
  Object.assign(host.run("run-1"), { turn_state: "idle", turn_open: false, last_movement_seq: 1 });
  const stale = reads("projection.operator.chats.status");
  host.changed("ledger");
  await until(() => reads("projection.operator.chats.status") > stale, "no status read");
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.equal(view.current.runs.find((run) => run.runId === "run-1").turnState, "waiting", "an older status never overrides a newer projection");
});

for (const profile of ["live", "lab"]) {
  test(`a follow-up is sent once under the task's binding, shows as working until the provider answers, and clears the draft (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => host.turns.length === 1 && view.current.busy && view.current.turnOpen, "the turn was not submitted");
    const [turn] = host.turns;
    assert.deepEqual([turn.runId, turn.providerInterfaceId, turn.providerModel, turn.reasoningEffort, turn.clientSurfaceRole, turn.text, turn.recoverPreparedMessage],
      ["run-1", provider.id, "model-a", "medium", "task", "Add totals", false]);
    assert.match(turn.messageId, /^msg_/);
    assert.equal(view.current.turnOpen, true);
    assert.deepEqual(working(view).map((message) => message.eventData.work_status), ["working"]);
    assert.equal(view.current.text, "");
    host.reply("run-1", "Totals added.");
    await until(() => !view.current.busy, "the reply never closed the turn");
    assert.equal(view.current.turnOpen, false);
    assert.deepEqual(working(view), []);
    assert.ok(view.current.renderedMessages.some((message) => message.text === "Totals added."));
    assert.equal(host.turns.length, 1);
  });

  test(`a message written while a reply is open is queued under its own identity and sent after the reply (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.busy && host.turns.length === 1, "first turn not sent");
    await type(view, "Also chart it");
    await view.current.submit();
    await until(() => view.current.queuedTurns.length === 1, "the follow-up was not queued");
    await until(() => view.current.text === "", "queueing did not clear the composer");
    const [queued] = view.current.queuedTurns;
    assert.match(queued.id, /^queued_/);
    assert.equal(queued.text, "Also chart it");
    assert.equal(host.turns.length, 1);
    assert.equal(view.current.paneTransferBlockReason("run-1"), "Send or remove queued follow-ups before moving this task.");
    const statusReads = host.reads.filter((method) => method === "projection.operator.chats.status").length;
    host.reply("run-1", "Totals added.");
    await until(() => host.turns.length === 2, "the queued follow-up was never dispatched");
    assert.equal(host.turns[1].messageId, queued.id.replace(/^queued_/, "msg_"), "the queued turn is delivered under the identity it was queued with");
    assert.equal(host.turns[1].text, "Also chart it");
    assert.ok(host.reads.filter((method) => method === "projection.operator.chats.status").length > statusReads, "dispatch rechecks the task status first");
    host.reply("run-1", "Chart added.");
    await until(() => !view.current.busy && view.current.queuedTurns.length === 0, "the queue never drained");
    assert.equal(view.current.paneTransferBlockReason("run-1"), "");
  });

  test(`steering sends into the open turn with its own message and binding (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => host.turns.length === 1, "first turn not sent");
    host.changed("runs");
    await until(() => view.current.canSteer, "steering never became available");
    await type(view, "Use euros");
    view.current.steerCurrentTurn();
    await until(() => host.steers.length === 1, "the steering message was not sent");
    const [steer] = host.steers;
    assert.deepEqual([steer.runId, steer.text, steer.providerInterfaceId, steer.clientSurfaceRole, steer.recoverPreparedMessage], ["run-1", "Use euros", provider.id, "task", false]);
    assert.match(steer.messageId, /^msg_/);
    assert.notEqual(steer.messageId, host.turns[0].messageId);
    await until(() => view.current.text === "", "the steering draft was not consumed");
    assert.equal(host.turns.length, 1);
    host.reply("run-1", "Totals in euros.");
    await until(() => !view.current.busy, "the reply never closed the turn");
  });

  test(`a delivery whose confirmation was lost is reconciled from the task history and never sent twice (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    host.failNextTurn = { failure: new Error("transport closed"), admitted: true };
    await view.current.submit();
    await until(() => view.current.queuedTurns.length === 0 && view.current.busy, () => `history never confirmed the admitted message: ${JSON.stringify({ queued: view.current.queuedTurns, busy: view.current.busy, error: view.current.error })}`);
    assert.equal(view.current.error, "");
    host.reply("run-1", "Totals added.");
    await until(() => !view.current.busy, "the reply never closed the turn");
    await type(view, "Next");
    assert.equal(view.current.submitBlockReason, "");
    assert.equal(host.turns.length, 1);
  });

  test(`an unconfirmed delivery that never reached the task blocks sending until it is removed (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    host.failNextTurn = { failure: new Error("transport closed"), admitted: false };
    await view.current.submit();
    await until(() => view.current.queuedTurns[0]?.status === "unknown", "the unconfirmed delivery was not kept");
    host.changed("ledger");
    await new Promise((resolve) => setTimeout(resolve, 40));
    assert.equal(view.current.queuedTurns[0]?.status, "unknown", "history without the message never confirms it");
    await type(view, "Try again");
    assert.equal(view.current.canSubmit, false);
    await view.current.deleteQueuedTurn(view.current.queuedTurns[0].id);
    await until(() => view.current.queuedTurns.length === 0 && view.current.canSubmit, `still blocked: ${view.current.submitBlockReason}`);
    assert.equal(host.turns.length, 1);
  });

  test(`a recovered delivery refreshes the task and is never sent again (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    host.append("run-1", "user", "Add totals", { id: "msg_recovered" });
    host.recoverNextTurn = true;
    await type(view, "Add totals");
    const reads = host.reads.length;
    host.failNextTurn = null;
    host.transport.submitChatTurn = ((submit) => async (request) => submit({ ...request, messageId: "msg_recovered" }))(host.transport.submitChatTurn);
    await view.current.submit();
    await until(() => host.turns.length === 1 && !view.current.busy, "the recovered turn never settled");
    assert.ok(host.reads.slice(reads).includes("projection.operator.chat.messages"));
    assert.ok(host.reads.slice(reads).includes("projection.operator.chats.list"));
    assert.deepEqual(working(view), []);
    await new Promise((resolve) => setTimeout(resolve, 50));
    assert.equal(host.turns.length, 1);
  });

  test(`a draft blocks moving the task until it is sent or cleared (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    assert.equal(view.current.paneTransferBlockReason("run-1"), "");
    await type(view, "Unsent thought");
    assert.equal(view.current.paneTransferBlockReason("run-1"), "Send or clear the current draft before moving this task.");
    await type(view, "");
    assert.equal(view.current.paneTransferBlockReason("run-1"), "");
  });

  test(`sending waits until the task history has loaded (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    let release;
    host.holdMessages = new Promise((resolve) => { release = resolve; });
    const view = await mount(host, profile);
    view.current.selectRun("run-1");
    await type(view, "Add totals");
    await until(() => view.current.submitBlockReason === "Task history is still loading.", `unexpected block: ${view.current.submitBlockReason}`);
    assert.equal(view.current.canSubmit, false);
    host.holdMessages = null;
    release();
    await until(() => view.current.canSubmit, `never ready: ${view.current.submitBlockReason}`);
  });

  test(`stop and resume stay pending until the provider acknowledges them (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.turnOpen, "turn never opened");
    await view.current.interruptActiveTurn();
    assert.deepEqual(host.controls.map((request) => [request.runId, request.action, request.interfaceId]), [["run-1", "stop", "chat.run-1"]]);
    await until(() => view.current.turnControlPending, "the stop is not pending");
    host.append("run-1", "assistant", "", control("run-1", "stop", host.controls[0].controlId));
    Object.assign(host.run("run-1"), { turn_open: false, turn_state: "idle" });
    host.changed("ledger");
    await until(() => !view.current.turnControlPending && !view.current.busy && view.current.resumeAvailable, "the acknowledged stop never settled into a resumable turn");
    await view.current.resumeActiveTurn();
    assert.deepEqual(host.controls.map((request) => request.action), ["stop", "resume"]);
    await until(() => view.current.turnControlPending, "the resume is not pending");
    host.append("run-1", "assistant", "", control("run-1", "resume", host.controls[1].controlId));
    Object.assign(host.run("run-1"), { turn_open: true, turn_state: "working" });
    host.changed("ledger");
    await until(() => !view.current.turnControlPending && view.current.turnOpen && !view.current.resumeAvailable, () => `the acknowledged resume never reopened the turn: ${JSON.stringify({ pending: view.current.turnControlPending, open: view.current.turnOpen, resume: view.current.resumeAvailable, run: view.current.runs[0]?.turnOpen, error: view.current.error })}`);
    host.reply("run-1", "Totals added.");
    await until(() => !view.current.turnOpen, "the reply never closed the turn");
  });

  test(`editing a queued message brings it back into the composer and saves under the same identity (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.busy, "first turn not sent");
    await type(view, "Also chart it");
    await view.current.submit();
    await until(() => view.current.queuedTurns.length === 1, "not queued");
    await until(() => view.current.text === "", "queueing did not clear the composer");
    const [queued] = view.current.queuedTurns;
    await view.current.editQueuedTurn(queued.id);
    await until(() => view.current.editingQueuedTurn && view.current.text === "Also chart it", "the queued text did not return to the composer");
    await type(view, "Also chart it in euros");
    await view.current.submit();
    await until(() => !view.current.editingQueuedTurn && view.current.queuedTurns[0]?.text === "Also chart it in euros", "the edit was not saved");
    assert.deepEqual([view.current.queuedTurns.length, view.current.queuedTurns[0].id, view.current.queuedTurns[0].messageId], [1, queued.id, queued.messageId]);
    host.reply("run-1", "Totals added.");
    await until(() => host.turns.length === 2, "the edited follow-up was never dispatched");
    assert.equal(host.turns[1].text, "Also chart it in euros");
    host.reply("run-1", "Chart added.");
    await until(() => !view.current.busy, "the queue never drained");
  });

  test(`stopping a turn pauses its queued follow-ups until the operator resumes them (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.turnOpen, "turn never opened");
    await type(view, "Also chart it");
    await view.current.submit();
    await until(() => view.current.queuedTurns.length === 1 && view.current.text === "", "not queued");
    await view.current.interruptActiveTurn();
    host.append("run-1", "assistant", "", control("run-1", "stop", host.controls[0].controlId));
    host.append("run-1", "assistant", "Interrupted before totals", { event_kind: "interrupted_result" });
    Object.assign(host.run("run-1"), { turn_open: false, turn_state: "idle" });
    await until(() => !view.current.busy && view.current.queuePaused, () => `the queue did not pause: ${JSON.stringify({ busy: view.current.busy, paused: view.current.queuePaused, queued: view.current.queuedTurns })}`);
    await new Promise((resolve) => setTimeout(resolve, 40));
    assert.equal(host.turns.length, 1, "a stopped turn never releases its queued follow-up by itself");
    await view.current.resumeQueuedTurns();
    await until(() => host.turns.length === 2, "resuming the queue never sent the follow-up");
    assert.equal(host.turns[1].text, "Also chart it");
    host.reply("run-1", "Chart added.");
    await until(() => !view.current.busy && view.current.queuedTurns.length === 0, "the queue never drained");
  });

  test(`a stop the provider refuses is reported and released (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    const view = await openTask(host, profile);
    await type(view, "Add totals");
    void view.current.submit();
    await until(() => view.current.turnOpen, "turn never opened");
    await view.current.interruptActiveTurn();
    await until(() => view.current.turnControlPending, "the stop is not pending");
    const refusal = control("run-1", "stop", host.controls[0].controlId);
    host.append("run-1", "assistant", "Provider busy", { ...refusal, event_data: { ...refusal.event_data, status: "failed", detail: { error: "provider busy" } } });
    await until(() => !view.current.turnControlPending, "the refused stop stayed pending");
    assert.equal(view.current.error, "Stop failed. provider busy");
    host.reply("run-1", "Totals added.");
    await until(() => !view.current.busy, "the reply never closed the turn");
  });

  test(`resume is never offered while the task's turn is open (${profile})`, { timeout: 15_000 }, async () => {
    const host = fakeHost();
    host.append("run-1", "assistant", "", control("run-1", "interrupt", "control_earlier"));
    const view = await openTask(host, profile);
    assert.equal(view.current.resumeAvailable, true);
    Object.assign(host.run("run-1"), { turn_open: true, turn_state: "working" });
    host.changed("runs");
    await until(() => view.current.turnOpen, "the open turn was not projected");
    assert.equal(view.current.resumeAvailable, false);
  });
}
