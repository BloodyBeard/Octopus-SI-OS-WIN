const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { createRequire } = require("node:module");
const { test, before, after } = require("node:test");
const ts = require("typescript");
const { createProjectionStore } = require("../electron/projection.cjs");

const appRoot = path.resolve(__dirname, "..");
function load(relative, mocks = {}, source = fs.readFileSync(path.join(appRoot, relative), "utf8")) {
  const filename = path.join(appRoot, relative);
  const code = ts.transpileModule(source, {
    fileName: filename,
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const module = { exports: {} };
  const localRequire = createRequire(filename);
  Function("module", "exports", "require", "window", code)(module, module.exports, (id) => id in mocks ? mocks[id] : localRequire(id), global.window);
  return module.exports;
}
const activity = load("src/components/activity/markActivity.ts", { "../../agentPresets": load("src/agentPresets.ts") });
const ids = (items) => items.map((item) => item.id).sort();
const readKey = (profile = "operator", home = "/test/home") => `octopus.mark-activity.${encodeURIComponent(profile)}.${encodeURIComponent(home)}.read.v1`;

function hookHost() {
  const cells = [];
  let cursor = 0, dirty = true, pending = [], render, result;
  const changed = (a, b) => !a || !b || a.length !== b.length || a.some((v, i) => !Object.is(v, b[i]));
  const react = {
    useState(initial) {
      const index = cursor++;
      if (!cells[index]) {
        const cell = { value: typeof initial === "function" ? initial() : initial };
        cell.set = (next) => { const value = typeof next === "function" ? next(cell.value) : next; if (!Object.is(value, cell.value)) { cell.value = value; dirty = true; } };
        cells[index] = cell;
      }
      return [cells[index].value, cells[index].set];
    },
    useRef(value) { const index = cursor++; return cells[index] ??= { current: value }; },
    useMemo(compute, deps) { const index = cursor++; if (changed(cells[index]?.deps, deps)) cells[index] = { deps, value: compute() }; return cells[index].value; },
    useCallback(callback, deps) { return react.useMemo(() => callback, deps); },
    useEffect(effect, deps) {
      const index = cursor++;
      if (changed(cells[index]?.deps, deps)) pending.push(() => { cells[index]?.cleanup?.(); cells[index] = { deps, cleanup: effect() }; });
    },
  };
  return {
    react,
    mount(next) { render = next; this.flush(); },
    flush() {
      for (let count = 0; dirty; count++) {
        assert.ok(count < 60, "hook update did not settle");
        dirty = false; cursor = 0; result = render();
        const effects = pending; pending = []; effects.forEach((effect) => effect());
      }
      return result;
    },
    update() { dirty = true; return this.flush(); },
    async settle() { for (let i = 0; i < 12; i++) { await Promise.resolve(); this.flush(); } return result; },
    close() { cells.forEach((cell) => cell.cleanup?.()); },
    get value() { return result; },
  };
}

function storageWindow(disk = new Map()) {
  const window = new EventTarget();
  const timers = new Set();
  window.localStorage = {
    getItem(key) { if (window.readFailure) throw Error("read denied"); return disk.get(key) ?? null; },
    setItem(key, value) { if (window.writeFailure) throw Error("write denied"); disk.set(key, value); },
  };
  window.setInterval = (fn) => { timers.add(fn); return fn; };
  window.clearInterval = (fn) => timers.delete(fn);
  window.storage = (key, newValue, oldValue = null) => {
    const event = new Event("storage");
    Object.assign(event, { key, newValue, oldValue, storageArea: window.localStorage });
    window.dispatchEvent(event);
  };
  return window;
}

function mountedActivity({ disk, initialReadFailure = false, primary = true, types = null, mode = "always", activityMode = "all", open = true } = {}) {
  const window = storageWindow(disk);
  window.readFailure = initialReadFailure;
  global.window = window;
  const host = hookHost(), requests = [], notifications = [];
  const props = { profile: "operator", home: "/test/home", primary, open };
  let generation = 0;
  const preferences = { markNotifications: mode, markActivityMode: activityMode, markActivityTypes: types, notificationSound: "silent" };
  const hook = load("src/components/activity/useMarkActivity.ts", {
    react: host.react,
    "../../clientPreferences": { useClientPreferences: () => preferences },
    "../../clientNotifications": { notifyClientEvent: (value) => { notifications.push(value); return true; } },
    "../../projectionClient": { readProjection: (method) => new Promise((resolve, reject) => requests.push({ method, resolve, reject })), formatProjectionError: (error) => error.message },
    "../../projectionLive": { useProjectionGeneration: () => generation },
    "./markActivity": activity,
  }).useMarkActivity;
  host.mount(() => hook(props.profile, props.home, props.primary, props.open));
  return Object.assign(host, {
    window, props, preferences, requests, notifications,
    refresh() { generation++; return host.update(); },
    async respond(marks, kinds = { descriptors: [{ name: "check.passed" }] }, offset = requests.length - 3, attention = { chats: [], activity: [] }) {
      requests[offset].resolve(marks); requests[offset + 1].resolve(kinds); requests[offset + 2].resolve(attention); return host.settle();
    },
  });
}

let temporary, store, projected, movementRows;
const previousWindow = global.window;
before(async () => {
  temporary = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-mark-test-"));
  temporary = fs.realpathSync(temporary);
  fs.mkdirSync(path.join(temporary, "library"));
  const ledger = path.join(temporary, "ledger"), registry = path.join(temporary, "registry");
  fs.mkdirSync(ledger); fs.mkdirSync(path.join(registry, "mark", "check.passed"), { recursive: true });
  fs.mkdirSync(path.join(registry, "interface"));
  fs.writeFileSync(path.join(registry, "mark", "check.passed", "mark.json"), JSON.stringify({ name: "check.passed" }));
  fs.mkdirSync(path.join(temporary, "state", "runs"), { recursive: true });
  fs.writeFileSync(path.join(temporary, "state", "runs", "run-test.json"), JSON.stringify({ id: "run-test", ee: "ee-test", worker: "worker-test", trace: "test-trace", status: "running", active_checkpoints: [], checkpoints: [] }));
  const rows = [0, 1, 2].map((seq) => ({
    seq, lamport: seq, recorded_at_unix_ms: 1_788_600_000_000 + seq * 1000, trace: "test-trace",
    kind: seq === 1 ? "InterfaceOutbound" : { MarkOutcomeAdmitted: "check.passed" },
    from: { Interface: "test-source" }, to: { Interface: "worker-test" },
    artifact: { id: "same-artifact", content: Array(32).fill(1) },
    invocation_metadata: seq === 1
      ? { runtime: { run: "run-test", ee: "ee-test", worker: "worker-test", checkpoint: "cp-test", interface: "worker-test", epoch: 1 } }
      : { name: "check.passed", run: "run-test", run_epoch: 1, class: "success", evidence: { id: "same-artifact", content: Array(32).fill(1) }, subject: "cp-test", verifier: "check", verifier_package: null, details: {} },
  }));
  fs.writeFileSync(path.join(ledger, "movements.jsonl"), rows.map(JSON.stringify).join("\n") + "\n");
  store = createProjectionStore({
    resolveRepoRoot: () => path.resolve(appRoot, ".."), octopusHome: () => temporary,
    sourceRegistry: () => registry, localPackageLibrary: () => path.join(temporary, "library"),
    projectEePolicies: async () => ({ environments: [], issues: [] }),
  });
  projected = await store.read({ contract_version: 1, method: "projection.marks.list" });
  movementRows = (await store.read({ contract_version: 1, method: "projection.movements.list" })).movements;
});
after(() => { store?.close(); if (temporary) fs.rmSync(temporary, { recursive: true, force: true }); global.window = previousWindow; });

const marks = (...rows) => ({ marks: rows });
const nextRow = (seq, patch = {}) => ({ ...projected.marks[0], movement_id: `movements:${seq + 1}`, seq, ...patch });

function attentionRow(seq, kind = "message", patch = {}) {
  return { id: `attention:${seq}`, kind, title: `${kind === "message" ? "Message" : "Approval needed"} — Actual task`, name: kind,
    run_id: "run-test", client_surface_role: "task", worker: "worker-test", agent_id: "agent-test", agent_name: "Actual Agent", agent_icon: "curious", agent_color: "emerald",
    artifact_id: `artifact:${seq}`, movement_id: kind === "message" ? `movements:${seq}` : null, request_id: kind === "message" ? null : `request:${seq}`,
    seq, created_at_ms: Date.now(), ...patch };
}
const attentionSet = (...rows) => ({ chats: [{ run_id: "run-test", agent_id: "agent-test", agent_name: "Actual Agent", agent_icon: "curious", agent_color: "emerald" }], activity: rows });

test("attention defaults and exact additional mark selection preserve actual identities and reject unsafe icons", () => {
  const prefs = load("src/clientPreferences.ts", {}, fs.readFileSync(path.join(appRoot, "src/clientPreferences.ts"), "utf8") + "\nexport { normalizePreferences as auditNormalize };");
  assert.equal(prefs.auditNormalize({}).markActivityMode, "attention");
  for (const types of [[], ["tool.call.allowed"], ["unknown.exact"]]) {
    const value = prefs.auditNormalize({ markActivityTypes: types }); assert.equal(value.markActivityMode, "custom"); assert.deepEqual(value.markActivityTypes, types);
  }
  assert.equal(prefs.auditNormalize({ markActivityTypes: null }).markActivityMode, "attention");
  assert.equal(prefs.auditNormalize({ markActivityMode: "all" }).markActivityMode, "all");
  assert.deepEqual(prefs.auditNormalize({ markActivityMode: "custom", markActivityTypes: null }).markActivityTypes, []);
  assert.deepEqual(prefs.auditNormalize({ markActivityMode: "custom", markActivityTypes: "invalid" }).markActivityTypes, []);
  const items = activity.decodeOperatorActivity(attentionSet(attentionRow(9)), activity.decodeMarkActivity(projected));
  assert.equal(activity.selectActivity(items, "attention", null).length, 1);
  assert.equal(activity.selectActivity(items, "all", []).length, 3);
  assert.equal(activity.selectActivity(items, "custom", []).length, 1);
  assert.equal(activity.selectActivity(items, "custom", null).length, 1);
  assert.equal(activity.selectActivity(items, "custom", ["check.passed"]).length, 3);
  assert.equal(activity.selectActivity(items, "custom", ["check.*"]).length, 1);
  assert.equal(items.find((item) => item.kind === "mark").agentName, "Actual Agent");
  assert.equal(items.find((item) => item.kind === "mark").agentColor, "emerald");
  for (const color of [null, "emerald", "authored-unknown-color"]) {
    const value = attentionSet(attentionRow(9, "message", { agent_color: color })); value.chats[0].agent_color = color;
    assert.equal(activity.decodeOperatorActivity(value)[0].agentColor, color);
  }
  const absent = attentionSet(attentionRow(9));
  for (const row of [...absent.chats, ...absent.activity]) Object.assign(row, { agent_id: null, agent_name: null, agent_icon: null, agent_color: null });
  assert.equal(activity.decodeOperatorActivity(absent)[0].agentName, null);
  for (const icon of ["../wrong", "", "unknown"] ) assert.throws(() => activity.decodeOperatorActivity(attentionSet(attentionRow(9, "message", { agent_icon: icon }))));
  assert.throws(() => activity.decodeOperatorActivity({ chats: [] }));
});

test("reading and clearing notifications persist across close, remount and windows without resolving requests or losing new arrivals", async () => {
  const disk = new Map(), h = mountedActivity({ disk, activityMode: "attention" });
  const set = attentionSet(attentionRow(9), attentionRow(10, "approval"));
  const dismissKey = readKey().replace(".read.v1", ".dismissed.v1");
  try {
    await h.respond(projected, undefined, undefined, set); assert.equal(h.value.unreadCount, 2);
    assert.equal(h.value.markRead(["attention:9"]), true); h.flush();
    assert.deepEqual(ids(h.value.groups[0].items), ["attention:10"]);
    assert.ok(h.value.projectedItems.some((item) => item.id === "attention:9"), "read detail retains its current projection target");
    h.props.open = false; h.update(); h.props.open = true; h.update();
    assert.deepEqual(ids(h.value.groups[0].items), ["attention:10"]);
    h.window.writeFailure = true; assert.equal(h.value.clearNotifications(), false); h.flush();
    assert.ok(h.value.items.some((item) => item.id === "attention:10")); assert.equal(h.value.loaded, false);
    h.window.writeFailure = false; h.window.storage(readKey(), disk.get(readKey())); h.flush();
    assert.equal(h.value.clearNotifications(), true); h.flush(); assert.equal(h.value.unreadCount, 0);
    assert.equal(set.activity.find((item) => item.id === "attention:10").kind, "approval");
    h.refresh(); await h.respond(projected, undefined, undefined, set); assert.equal(h.value.groups[0].items.length, 0);
    h.refresh(); await h.respond(projected, undefined, undefined, attentionSet(...set.activity, attentionRow(11)));
    assert.deepEqual(h.notifications.map((item) => item.target.movementId), ["attention:11"]);
    assert.equal(h.notifications[0].title, "Message — Actual task"); assert.equal(h.notifications[0].body, "Actual Agent");
    const other = mountedActivity({ disk, activityMode: "attention", primary: false });
    try {
      await other.respond(projected, undefined, undefined, attentionSet(...set.activity, attentionRow(11)));
      assert.deepEqual(ids(other.value.groups[0].items), ["attention:11"]);
      assert.equal(other.value.clearNotifications(), true); other.flush();
      h.window.storage(dismissKey, disk.get(dismissKey)); h.flush(); assert.equal(h.value.groups[0].items.length, 0);
      disk.set(dismissKey, '["attention:12"]'); h.window.storage(dismissKey, '["attention:13"]'); h.flush();
      assert.deepEqual([...h.value.dismissed].sort(), ["attention:10", "attention:11", "attention:12", "attention:13"]);
      assert.equal(h.value.read.has("attention:9"), true);
    } finally { other.close(); }
    h.props.home = "/another/home"; h.update(); await h.respond(projected, undefined, undefined, set);
    assert.equal(h.value.dismissed.size, 0); assert.equal(h.value.unreadCount, 2);
  } finally { h.close(); }
});

test("real lab adapter returns explicit fixture Activity and never falls through to live projection", async () => {
  const fixture = require("../electron/fixtureProjection.cjs").createFixtureProjectionStore();
  try {
    const response = await fixture.read({ contract_version: 1, method: "projection.operator.chats.list", params: { include_activity: true } });
    assert.ok(response.activity.length); assert.ok(response.activity.every((row) => row.projection_source === "lab"));
    const decoded = activity.decodeOperatorActivity(response); assert.ok(decoded.some((item) => item.kind === "message"));
    assert.ok(decoded.every((item) => item.navigation.kind === "run"));
  } finally { fixture.close?.(); }
});

test("clearing the latest message never reveals older messages and a new receipt remains independently unread", async () => {
  const h = mountedActivity({ activityMode: "attention" }), rows = [attentionRow(20), attentionRow(21), attentionRow(22, "approval"), attentionRow(23, "user_input")];
  try {
    await h.respond(projected, undefined, undefined, attentionSet(...rows));
    assert.deepEqual(ids(h.value.groups.flatMap((group) => group.items)), ["attention:21", "attention:22", "attention:23"]);
    assert.equal(h.value.clearNotifications(), true); h.flush(); assert.equal(h.value.read.has("attention:20"), false);
    h.refresh(); await h.respond(projected, undefined, undefined, attentionSet(...rows));
    assert.deepEqual(ids(h.value.groups.flatMap((group) => group.items)), []);
    h.refresh(); await h.respond(projected, undefined, undefined, attentionSet(...rows, attentionRow(24)));
    assert.equal(h.value.unreadCount, 1); assert.equal(h.value.read.has("attention:24"), false);
    assert.deepEqual([...h.value.dismissed].sort(), ["attention:21", "attention:22", "attention:23"]);
    assert.deepEqual(h.notifications.map((item) => item.target.movementId), ["attention:24"]);
    assert.equal(h.value.items.some((item) => item.id === "attention:20"), false);
  } finally { h.close(); }
});

test("actual client terminal/request decoders and content-addressed receipts produce attention without marks or error-message substitutes", async () => {
  const relative = "electron/projection.cjs", source = fs.readFileSync(path.join(appRoot, relative), "utf8");
  const marker = "    read: readProjection,"; assert.equal(source.split(marker).length, 2);
  const factory = load(relative, {}, source.replace(marker, `${marker}\n    auditActivity: buildOperatorActivity, auditCachedActivity: operatorActivity,`)).createProjectionStore;
  const audited = factory({ resolveRepoRoot: () => path.resolve(appRoot, ".."), octopusHome: () => temporary });
  const ledger = path.join(temporary, "attention-ledger"), blobs = path.join(ledger, "blobs"); fs.mkdirSync(blobs, { recursive: true });
  const { blake3 } = require("@noble/hashes/blake3");
  const validate = new (require("ajv"))({ strict: false, logger: false }).compile(require("../../schemas/movement.schema.json"));
  function movement(seq, label, payload, direction = "out", target = "chat-interface") {
    const bytes = Buffer.from(JSON.stringify(payload)), hash = Buffer.from(blake3(bytes)).toString("hex");
    fs.writeFileSync(path.join(blobs, hash), bytes);
    const row = { seq, lamport: seq, trace: "attention-trace", recorded_at_unix_ms: Date.now() + seq,
      kind: direction === "out" ? "InterfaceOutbound" : "InterfaceArrival",
      from: { Interface: direction === "out" ? "provider-interface" : "chat-interface" }, to: { Interface: direction === "out" ? target : "worker-test" },
      artifact: { id: `${label}:${seq}`, content: [...Buffer.from(hash, "hex")] }, invocation_metadata: null };
    assert.ok(validate(row), JSON.stringify(validate.errors));
    return { ...row, source_file: path.join(ledger, "movements.jsonl"), source_line: seq + 1 };
  }
  const inbound = [movement(0, "operator.message", { text: "Actual task" }, "in"), movement(6, "operator.approval.response", { operator_approval_response: { request_id: "a", decision: "accept" } }, "in")];
  const outbound = [movement(1, "interface.artifact.output", { reply: "Delivered terminal\r\n" }),
    movement(2, "turn.agent_message", { text: "commentary is not a received terminal" }),
    movement(3, "operator.approval.request", { request_id: "a", title: "Allow command?" }),
    movement(4, "operator.user_input.request", { request_id: "u", questions: [] }),
    movement(5, "operator.elicitation.request", { request_id: "e", message: "Provide information" }),
    movement(7, "turn.operator_request.resolved", { threadId: "provider-thread", requestId: "e" }),
    movement(8, "interface.artifact.error", { error: "failed result" }),
    movement(9, "interface.artifact.output", { error: "must not be a new message" }),
    movement(10, "interface.artifact.output", { reply: "not delivered to this client" }, "out", "other-interface"),
    movement(11, "operator.approval.request", { request_id: "a", title: "A new request after old response" })];
  const chat = { run_id: "run-test", title: "Actual task", chat_interface_id: "chat-interface", client_surface_role: "task", ee_id: "project-a", worker_interface_id: "instantiated-worker", worker_choice_interface_id: "worker-choice" };
  const entry = { runId: "run-test", chatInterfaceId: "chat-interface", writable: true, record: {}, inbound, outbound, activity: [...inbound, ...outbound] };
  const projection = { descriptors: { interface: [] }, operatorChatRunIndexCache: { generation: 1, promise: Promise.resolve([entry]) }, operatorChatMessagesCache: new Map(),
    clientAgentCatalog: { document: { agents: [{ id: "exact-agent", name: "Exact Agent", icon: "glasses", project_ee_id: "project-a", worker_interface: "worker-choice" }, { id: "other-project-agent", name: "Other", icon: "curious", project_ee_id: "project-b", worker_interface: "worker-choice" }] } },
    operatorChatProtocol: { inputLabel: "operator.message", terminalOutputLabel: "interface.artifact.output", errorOutputLabel: "interface.artifact.error", approvalResponseLabel: "operator.approval.response", elicitationResponseLabel: "operator.elicitation.response", userInputResponseLabel: "operator.user_input.response" } };
  try {
    const rows = await audited.auditActivity(projection, [chat]);
    assert.deepEqual(rows.map((row) => row.kind).sort(), ["approval", "message", "user_input"]);
    assert.ok(rows.every((row) => row.agent_name === "Exact Agent" && row.worker === "instantiated-worker"));
    const terminal = rows.find((row) => row.kind === "message"); assert.equal(terminal.title, "Message — Actual task"); assert.equal(terminal.seq, 1); assert.equal(terminal.artifact_id, "interface.artifact.output:1");
    const changedTime = [...rows].map((row) => row.id).sort(); chat.updated_at_ms = Date.now() + 999;
    assert.deepEqual((await audited.auditActivity(projection, [chat])).map((row) => row.id).sort(), changedTime);
    const unrelated = movement(12, "turn.exec.output_delta", { text: "unrelated tool output" });
    fs.unlinkSync(path.join(blobs, Buffer.from(unrelated.artifact.content).toString("hex"))); outbound.push(unrelated);
    assert.deepEqual((await audited.auditActivity(projection, [chat])).map((row) => row.id).sort(), changedTime);
    const time = Date.now(), stream = movement(15, "turn.stream", {});
    const streamBytes = Buffer.from([
      { type: "header", schema_version: 1, seq: 0, provider_interface: "provider-interface", run: "run-test", thread_id: "provider-thread", turn_id: "turn-test", opened_source_movement_seq: 11, opened_at_unix_ms: time },
      { type: "event", seq: 1, source_movement_seq: 13, recorded_at_unix_ms: time + 1, label: "turn.operator_request.resolved", payload: { threadId: "provider-thread", requestId: "a" } },
      { type: "seal", seq: 2, source_movement_seq: 14, recorded_at_unix_ms: time + 2, terminal_label: "interface.artifact.output", terminal_artifact_payload_blake3: "1".repeat(64) },
    ].map(JSON.stringify).join("\n") + "\n");
    const streamHash = Buffer.from(blake3(streamBytes)); stream.artifact.content = [...streamHash]; fs.writeFileSync(path.join(blobs, streamHash.toString("hex")), streamBytes); outbound.push(stream);
    assert.deepEqual((await audited.auditActivity(projection, [chat])).map((row) => row.kind).sort(), ["message", "user_input"]);
    entry.writable = false; projection.operatorChatMessagesCache.clear();
    const readonly = await audited.auditActivity(projection, [{ ...chat, client_surface_role: "home" }]);
    assert.ok(readonly.some((row) => row.kind === "message" && row.client_surface_role === "home"));
    inbound.push(movement(18, "operator.approval.response", { operator_approval_response: { request_id: "a", decision: "accept" } }, "in"),
      movement(19, "operator.user_input.response", { operator_user_input_response: { request_id: "u", answers: {} } }, "in"));
    fs.unlinkSync(path.join(blobs, streamHash.toString("hex")));
    assert.deepEqual((await audited.auditActivity(projection, [chat])).map((row) => row.kind), ["message"]);
    const other = { ...chat, run_id: "other-run", chat_interface_id: "other-interface" };
    const entries = [entry, { ...entry, runId: other.run_id, chatInterfaceId: other.chat_interface_id, inbound: [], outbound: [] }];
    projection.operatorChatRunIndexCache.promise = Promise.resolve(entries);
    const full = [chat, other], cached = await audited.auditCachedActivity(projection, full);
    assert.equal(await audited.auditCachedActivity(projection, full), cached);
    assert.deepEqual(await audited.auditCachedActivity(projection, [other]), []);
    assert.equal((await audited.auditCachedActivity(projection, full)).length, 1);
    const forward = projection.operatorActivityCache.promise;
    await audited.auditCachedActivity(projection, [other, chat]); assert.notEqual(projection.operatorActivityCache.promise, forward);
    const terminalPath = path.join(blobs, Buffer.from(outbound[0].artifact.content).toString("hex")), bytes = fs.readFileSync(terminalPath);
    fs.unlinkSync(terminalPath); audited.invalidate();
    projection.operatorChatRunIndexCache = { generation: audited.generation(), promise: Promise.resolve(entries) };
    await assert.rejects(audited.auditCachedActivity(projection, full), /ENOENT/);
    assert.equal(projection.operatorActivityCache, null);
    fs.writeFileSync(terminalPath, bytes); assert.equal((await audited.auditCachedActivity(projection, full)).length, 1);
  } finally { audited.close(); }
});

test("actual ledger projection decodes only admitted marks with exact source fields", async () => {
  assert.equal(projected.marks.length, 2);
  assert.ok(movementRows.every((row) => row.raw_run_id === "run-test"));
  assert.ok(movementRows.every((row) => row.from.kind === "interface" && row.to.kind === "interface"));
  const decoded = activity.decodeMarkActivity(projected);
  assert.deepEqual(ids(decoded), ["movements:1", "movements:3"]);
  assert.deepEqual(decoded[0], { id: "movements:3", sequence: 2, name: "check.passed", worker: "worker-test", runId: "run-test", artifactId: "same-artifact", createdAt: 1_788_600_002_000,
    kind: "mark", title: "check.passed", agentId: null, agentName: null, agentIcon: null, agentColor: null, movementId: "movements:3", requestId: null, clientSurfaceRole: null, navigation: { kind: "mark", movementId: "movements:3" } });
  assert.equal(projected.marks[0].checkpoint_id, ""); assert.equal(projected.marks[0].interface_id, "");
  assert.deepEqual(activity.decodeMarkKinds(await store.read({ contract_version: 1, method: "projection.descriptors.mark_kinds.list" })), ["check.passed"]);
  assert.deepEqual(activity.decodeMarkActivity({ marks: [] }), []);
  for (const invalid of [null, {}, { marks: null }, marks(null), marks({}), marks(nextRow(1, { seq: -1 })), marks(nextRow(1, { seq: 1.2 })), marks(nextRow(1, { created_at: "invalid" })), marks(nextRow(1), nextRow(1))]) {
    assert.throws(() => activity.decodeMarkActivity(invalid));
  }
  assert.equal(activity.decodeMarkActivity(marks(nextRow(1, { seq: null, created_at: "", worker: "", artifact_id: "", run_id: "" })))[0].createdAt, null);
  for (const invalid of [null, {}, { descriptors: [null] }, { descriptors: [{ name: 1 }] }]) assert.throws(() => activity.decodeMarkKinds(invalid));
  for (const invalid of ["{}", "null", "[1]", '[""]', "bad"]) assert.throws(() => activity.decodeActivityReadState(invalid));
});

test("reading removes only the exact movement immediately and it stays absent across filters, reopening and refresh", async () => {
  const h = mountedActivity();
  try {
    await h.respond(projected);
    assert.equal(h.value.unreadCount, 2);
    assert.equal(h.value.markRead(["movements:1"]), true); h.flush();
    assert.equal(h.value.unreadCount, 1); assert.equal(h.value.read.has("movements:3"), false);
    assert.deepEqual(ids(h.value.items), ["movements:3"]);
    h.props.open = false; h.update(); h.props.open = true; h.update();
    assert.deepEqual(ids(h.value.groups[0].items), ["movements:3"]);
    h.preferences.markActivityMode = "custom"; h.preferences.markActivityTypes = []; h.update(); assert.equal(h.value.unreadCount, 0);
    h.preferences.markActivityMode = "all"; h.preferences.markActivityTypes = null; h.update(); assert.deepEqual(ids(h.value.groups[0].items), ["movements:3"]);
    h.refresh(); await h.respond(marks(projected.marks[0]));
    h.refresh(); await h.respond(projected); assert.deepEqual(ids(h.value.items), ["movements:3"]);
    assert.equal(h.value.clearNotifications(), true); h.flush(); assert.deepEqual(ids(h.value.items), []);
  } finally { h.close(); }
});

test("legacy saved read notifications stay absent when Activity opens and reads never become history", async () => {
  const h = mountedActivity({ open: false, disk: new Map([[readKey(), '["movements:1"]']]) });
  try {
    await h.respond(projected); assert.deepEqual(ids(h.value.items), ["movements:3"]);
    h.props.open = true; h.update(); assert.deepEqual(ids(h.value.groups[0].items), ["movements:3"]);
    h.value.markRead(["movements:3"]); h.flush(); assert.deepEqual(ids(h.value.items), []);
    h.props.open = false; h.update(); h.props.open = true; h.update();
    assert.deepEqual(ids(h.value.groups[0].items), []);
  } finally { h.close(); }
});

test("eraser clears every selected unread page while hidden technical marks and older message receipts stay unread", async () => {
  const disk = new Map(), h = mountedActivity({ disk, activityMode: "custom", types: ["check.passed"] });
  const rows = attentionSet(attentionRow(90), attentionRow(91), ...Array.from({ length: 25 }, (_, i) => attentionRow(100 + i, "approval")));
  const raw = marks(nextRow(30), nextRow(31, { mark_name: "hidden.exact" }));
  try {
    await h.respond(raw, undefined, undefined, rows); assert.equal(h.value.unreadCount, 27);
    const selected = ids(h.value.groups.flatMap((group) => group.items)); assert.equal(selected.length, 27);
    assert.equal(h.value.clearNotifications(), true); h.flush(); assert.equal(h.value.unreadCount, 0);
    assert.deepEqual([...h.value.read].sort(), selected); assert.deepEqual([...h.value.dismissed].sort(), selected);
    assert.equal(h.value.read.has("attention:90"), false); assert.equal(h.value.read.has("movements:32"), false);
    h.preferences.markActivityMode = "all"; h.update(); assert.deepEqual(ids(h.value.groups[0].items), ["movements:32"]);
    h.preferences.markActivityMode = "attention"; h.update(); assert.equal(h.value.unreadCount, 0);
    h.refresh(); await h.respond(raw, undefined, undefined, rows); assert.equal(h.value.unreadCount, 0);
    assert.equal(rows.activity.filter((row) => row.kind === "approval").length, 25);
  } finally { h.close(); }
});

test("filters are exact names: null all, [] none, no inferred hierarchy or severity", async () => {
  const h = mountedActivity();
  try {
    await h.respond(marks(nextRow(3, { mark_name: "check.passed" }), nextRow(4, { mark_name: "check" }), nextRow(5, { mark_name: "CHECK.PASSED" })));
    for (const [types, count] of [[null, 3], [[], 0], [["check"], 1], [["check.passed"], 1], [["check.*"], 0]]) {
      h.preferences.markActivityMode = types === null ? "all" : "custom"; h.preferences.markActivityTypes = types; h.update(); assert.equal(h.value.unreadCount, count);
    }
  } finally { h.close(); }
});

test("pending live refresh, failure, cancelled response and reappearance never duplicate desktop arrivals", async () => {
  const h = mountedActivity();
  try {
    await h.respond(projected); assert.equal(h.notifications.length, 0);
    h.refresh(); assert.equal(h.value.loaded, true); assert.equal(h.value.items.length, 2);
    h.requests.at(-2).reject(Error("projection unavailable")); h.requests.at(-1).resolve({ descriptors: [] }); await h.settle();
    assert.match(h.value.error, /unavailable/); assert.equal(h.value.items.length, 2);
    h.refresh(); const stale = h.requests.length - 3; h.refresh();
    await h.respond(marks(...projected.marks, nextRow(4)));
    await h.respond(marks(nextRow(6)), { descriptors: [] }, stale);
    assert.equal(h.value.items.length, 3); assert.equal(h.notifications.length, 1);
    assert.deepEqual(h.notifications[0].target, { kind: "mark", movementId: "movements:5" });
    assert.equal(h.notifications[0].title, "check.passed"); assert.equal(h.notifications[0].body, "worker-test");
    assert.equal(h.notifications[0].sound, "silent");
    h.refresh(); await h.respond(projected); h.refresh(); await h.respond(marks(...projected.marks, nextRow(4)));
    assert.equal(h.notifications.length, 1);
  } finally { h.close(); }
});

test("initial storage read failure preserves baseline and arrivals for recovery", async () => {
  const h = mountedActivity({ initialReadFailure: true });
  try {
    await h.respond(projected); assert.equal(h.value.loaded, false); assert.match(h.value.error, /read denied/);
    h.refresh(); await h.respond(marks(...projected.marks, nextRow(4))); assert.equal(h.notifications.length, 0);
    h.window.readFailure = false; h.window.storage(readKey(), null); await h.settle();
    assert.equal(h.value.loaded, true); assert.equal(h.notifications.length, 1);
    assert.equal(h.notifications[0].target.movementId, "movements:5");
    assert.equal(h.value.read.size, 0);
  } finally { h.close(); }
});

test("read and write failures do not mark read or report an empty successful load", async () => {
  const disk = new Map([[readKey(), '["movements:1"]']]);
  const h = mountedActivity({ disk });
  try {
    await h.respond(projected);
    h.window.readFailure = true; h.window.storage(readKey(), null); h.flush();
    assert.equal(h.value.read.has("movements:1"), true);
    assert.equal(h.value.loaded, false); assert.equal(h.value.markRead(["movements:3"]), false);
    h.window.readFailure = false; h.window.writeFailure = true;
    assert.equal(h.value.markRead(["movements:3"]), false); h.flush();
    assert.equal(h.value.read.has("movements:3"), false);
    h.window.writeFailure = false; disk.set(readKey(), "broken"); h.window.storage(readKey(), "broken"); h.flush();
    assert.equal(h.value.loaded, false); assert.ok(h.value.error);
    const storage = h.window.localStorage;
    Object.defineProperty(h.window, "localStorage", { get() { throw Error("storage access denied"); } });
    h.window.dispatchEvent(Object.assign(new Event("storage"), { key: readKey(), newValue: null, storageArea: storage })); h.flush();
    assert.match(h.value.error, /storage access denied/);
  } finally { h.close(); }
});

test("invalid projection or kind responses stay failed; unknown and empty read requests fail closed", async () => {
  for (const [rows, kinds] of [[{}, { descriptors: [] }], [{ marks: [] }, {}]]) {
    const h = mountedActivity();
    try {
      await h.respond(rows, kinds); assert.equal(h.value.loaded, false); assert.ok(h.value.error);
      assert.equal(h.value.clearNotifications(), false);
    } finally { h.close(); }
  }
  const h = mountedActivity();
  try {
    await h.respond(projected);
    assert.equal(h.value.markRead(["unknown"]), false); assert.equal(h.value.markRead([]), false); h.flush();
    assert.equal(h.value.read.size, 0);
  } finally { h.close(); }
});

test("arrivals pending storage recovery survive a temporarily absent movement and a second refresh failure", async () => {
  const h = mountedActivity({ initialReadFailure: true });
  try {
    await h.respond(projected); h.refresh(); await h.respond(marks(...projected.marks, nextRow(4)));
    h.refresh(); await h.respond(projected);
    h.window.readFailure = false; h.window.storage(readKey(), null); h.flush();
    assert.equal(h.notifications.length, 0);
    h.refresh(); h.requests.at(-2).reject(Error("offline")); h.requests.at(-1).resolve({ descriptors: [] }); await h.settle();
    h.refresh(); await h.respond(marks(...projected.marks, nextRow(4)));
    assert.deepEqual(h.notifications.map((notification) => notification.target.movementId), ["movements:5"]);
  } finally { h.close(); }
});

test("cross-window concurrent writes merge event values with latest durable and local reads", async () => {
  const disk = new Map(), h = mountedActivity({ disk });
  try {
    await h.respond(projected); h.value.markRead(["movements:1"]); h.flush();
    disk.set(readKey(), '["movements:3"]'); h.window.storage(readKey(), '["movements:3"]'); h.flush();
    assert.deepEqual([...h.value.read].sort(), ["movements:1", "movements:3"]);
    assert.deepEqual(JSON.parse(disk.get(readKey())).sort(), ["movements:1", "movements:3"]);
    disk.set(readKey(), '["movements:1","movements:3","movements:8"]');
    h.window.storage(readKey(), '["movements:7"]'); h.flush();
    assert.deepEqual([...h.value.read].sort(), ["movements:1", "movements:3", "movements:7", "movements:8"]);
  } finally { h.close(); }
});

test("two independently mounted windows reconcile simultaneous read writes without losing either movement", async () => {
  const disk = new Map(), first = mountedActivity({ disk }), second = mountedActivity({ disk, primary: false });
  try {
    await first.respond(projected); await second.respond(projected);
    first.value.markRead(["movements:1"]); first.flush();
    const getItem = second.window.localStorage.getItem;
    second.window.localStorage.getItem = () => null;
    second.value.markRead(["movements:3"]); second.flush(); second.window.localStorage.getItem = getItem;
    first.window.storage(readKey(), disk.get(readKey())); first.flush();
    second.window.storage(readKey(), disk.get(readKey())); second.flush();
    assert.deepEqual([...first.value.read].sort(), ["movements:1", "movements:3"]);
    assert.deepEqual([...second.value.read].sort(), ["movements:1", "movements:3"]);
    assert.deepEqual(JSON.parse(disk.get(readKey())).sort(), ["movements:1", "movements:3"]);
  } finally { first.close(); second.close(); }
});

test("desktop arrivals are primary only, preference filtered, scoped and never retroactive", async () => {
  const h = mountedActivity({ primary: false });
  try {
    await h.respond(projected); h.refresh(); await h.respond(marks(...projected.marks, nextRow(4)));
    assert.equal(h.notifications.length, 0); h.props.primary = true; h.update(); assert.equal(h.notifications.length, 0);
    h.preferences.markNotifications = "never"; h.refresh(); await h.respond(marks(...projected.marks, nextRow(4), nextRow(5)));
    h.preferences.markNotifications = "unfocused"; h.preferences.markActivityMode = "custom"; h.preferences.markActivityTypes = ["other"]; h.update();
    h.refresh(); await h.respond(marks(...projected.marks, nextRow(6))); assert.equal(h.notifications.length, 0);
    h.preferences.markActivityMode = "all"; h.preferences.markActivityTypes = null; h.update(); assert.equal(h.notifications.length, 0);
    h.refresh(); await h.respond(marks(...projected.marks, nextRow(6), nextRow(7))); assert.equal(h.notifications.length, 1);
    assert.equal(h.notifications[0].when, "unfocused");
    h.props.home = "/other/home"; h.update(); assert.equal(h.value.loaded, false);
    await h.respond(projected); assert.equal(h.notifications.length, 1); assert.equal(h.value.read.size, 0);
  } finally { h.close(); }
});

function eventLog() {
  global.window = storageWindow();
  const host = hookHost(), requests = [];
  let generation = 0;
  const client = { hasProjectionBridge: () => true, formatProjectionError: (error) => error.message, readProjection: (method, params) => new Promise((resolve, reject) => requests.push({ method, params, resolve, reject })) };
  const resource = load("src/components/projectionResource.tsx", { react: host.react, "../design-system/components": {}, "../projectionClient": client, "./OctopusLoadingLogo": load("src/components/OctopusLoadingLogo.tsx") });
  const View = load("src/components/EventLogView.tsx", {
    react: host.react, "../projectionClient": client, "../projectionLive": { useProjectionGeneration: () => generation },
    "../runtimeFormat": {}, "./chat/format": {}, "./IconGlyphs": {}, "./JsonPanel": {}, "./SurfaceCopyButton": {},
    "../toasts": { appToast: { danger() {} } }, "./TopBar": {}, "./projectionResource": resource,
  }).EventLogView;
  const props = { focusedMovement: { id: "movements:3", sequence: 2 }, query: "stale search", onQueryChange(value) { props.query = value; host.update(); }, onClearMovement() { props.focusedMovement = null; host.update(); }, searchFocusTick: 0, title: "Event log", placeholder: "Search" };
  host.mount(() => View(props));
  return Object.assign(host, { requests, props, refresh() { generation++; return host.update(); } });
}
function elements(node, predicate) {
  if (!node || typeof node !== "object") return [];
  if (Array.isArray(node)) return node.flatMap((child) => elements(child, predicate));
  return [...(predicate(node) ? [node] : []), ...elements(node.props?.children, predicate)];
}
test("movement rows and expanded payloads remain readable during continuous ledger writes", async () => {
  const h = eventLog();
  const lists = () => h.requests.filter(r => r.method === "projection.movements.list");
  const rows = () => elements(h.value, node => node.type?.name === "MovementRowView");
  const page = () => ({ movements: movementRows.filter(row => row.seq === 2).map(row => ({...row})), total: 1 });
  try {
    for (let i = 0; i < 20; i++) h.refresh();
    assert.equal(lists().length, 1);
    lists()[0].resolve(page()); await h.settle();
    assert.equal(rows().length, 1); assert.equal(lists().length, 2);
    const payload = h.requests.find(r => r.method === "projection.movements.payload");
    for (let i = 0; i < 20; i++) h.refresh();
    assert.equal(lists().length, 2);
    lists()[1].resolve(page()); await h.settle();
    assert.equal(rows().length, 1); assert.equal(lists().length, 3);
    assert.equal(h.requests.filter(r => r.method === "projection.movements.payload").length, 1);
    payload.resolve({resolved: true, text: "readable artifact"}); await h.settle();
    assert.equal(rows()[0].props.payload.text, "readable artifact");
    const old = lists()[2];
    h.props.focusedMovement = {id: "movements:1", sequence: 0}; h.update();
    old.resolve(page()); await h.settle();
    assert.equal(rows().length, 0);
    lists().at(-1).reject(Error("projection unavailable")); await h.settle();
    assert.equal(elements(h.value, node => node.type?.name === "ProjectionResourceFailure").length, 1);
  } finally { h.close(); }
});
test("focused movement queries exact seq and expands the composite row key", async () => {
  const h = eventLog();
  try {
    assert.deepEqual(h.requests[0].params, { limit: 500, query: "", before_seq: 3, after_seq: 1 });
    h.requests[0].resolve({ movements: movementRows.filter((row) => row.seq === 2), total: 1 }); await h.settle();
    const rows = elements(h.value, (node) => node.type?.name === "MovementRowView");
    assert.equal(rows.length, 1); assert.equal(rows[0].props.expanded, true);
    assert.equal(h.requests[1].method, "projection.movements.payload");
    assert.equal(h.requests[1].params.blob_path, movementRows[0].blob_path);
    const sameId = h.props.focusedMovement.id;
    h.props.focusedMovement = { id: sameId, sequence: 1 }; h.update();
    assert.equal(elements(h.value, (node) => node.type?.name === "MovementRowView").length, 0);
    h.props.focusedMovement = { id: "movements:1", sequence: 0 }; h.update();
    assert.deepEqual(h.requests.at(-1).params, { limit: 500, query: "", before_seq: 1 });
    h.requests.at(-1).resolve({ movements: movementRows.filter((row) => row.seq === 0), total: 1 }); await h.settle();
    assert.equal(elements(h.value, (node) => node.type?.name === "MovementRowView")[0].props.expanded, true);
    const toolbar = elements(h.value, node => node.props?.toolbar)[0].props.toolbar;
    const clear = elements(toolbar, node => node.type === "button" && node.props.children === "Show all movements")[0];
    assert.ok(clear);
    clear.props.onClick();
    assert.equal(h.props.focusedMovement, null);
    assert.deepEqual(h.requests.at(-1).params, {limit: 500, query: ""});
    h.requests.at(-1).resolve({movements: movementRows, total: movementRows.length}); await h.settle();
    assert.equal(elements(h.value, node => node.type?.name === "MovementRowView").length, movementRows.length);
  } finally { h.close(); }
});

test("Inbox selection stays on its detail surface and explicit open follows exact targets", () => {
  const relative = "src/components/shell/OperatorShell.tsx";
  const source = fs.readFileSync(path.join(appRoot, relative), "utf8");
  const parsed = ts.createSourceFile(relative, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const owner = parsed.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "OperatorShell");
  const open = owner.body.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((declaration) => declaration.name.getText(parsed) === "openActivityMark"));
  const select = owner.body.statements.find((node) => ts.isVariableStatement(node) && node.declarationList.declarations.some((declaration) => declaration.name.getText(parsed) === "selectInboxItem"));
  const contains = (node, name) => ts.isIdentifier(node) && node.text === name || node.getChildren(parsed).some((child) => contains(child, name));
  const activate = owner.body.statements.find((node) => ts.isExpressionStatement(node) && contains(node, "CLIENT_NOTIFICATION_ACTIVATE_EVENT"));
  assert.ok(open && activate, "Activity callback owners must exist");
  const create = load(relative, {}, `export function create(deps: any) {
    const { useCallback, useEffect, markActivity, appToast, selectSurface, setActivitySelection, setQuery, setSecondaryPaneOpen, openSecondaryChatTab, openSecondaryTab, secondaryPaneTab, homeChatWorkspace, setSecondaryHomeViewMode, setSecondaryMovementSelection, window, CustomEvent, CLIENT_NOTIFICATION_ACTIVATE_EVENT } = deps;
    ${select.getText(parsed)}\n${open.getText(parsed)}\n${activate.getText(parsed)}\nreturn { openActivityMark, selectInboxItem };
  }`).create;
  const calls = [], items = activity.decodeMarkActivity(projected), window = storageWindow();
  class CustomEvent extends Event { constructor(type, options) { super(type); this.detail = options.detail; } }
  let cleanup, readResult = true;
  const handlers = create({
    useCallback: (fn) => fn, useEffect: (fn) => { cleanup = fn(); }, window, CustomEvent, CLIENT_NOTIFICATION_ACTIVATE_EVENT: "activate",
    markActivity: { projectedItems: items, read: new Set(), markRead: (ids) => { calls.push(["read", ids]); return readResult; } },
    appToast: { warning: (message) => calls.push(["warning", message]) },
    selectSurface: (id) => calls.push(["surface", id]), setActivitySelection: (value) => calls.push(["selected", value]),
    openSecondaryChatTab: (id) => calls.push(["chat", id]),
    homeChatWorkspace: {selectRun: (id) => calls.push(["home", id])},
    setSecondaryHomeViewMode: (mode) => calls.push(["home-mode", mode]),
    setSecondaryMovementSelection: value => calls.push(["movement-filter", value]),
    secondaryPaneTab: (workspaceKey, surfaceId, runId) => ({workspaceKey, surfaceId, runId}),
    openSecondaryTab: value => calls.push(["side-tab", value]),
    setQuery: (value) => calls.push(["query", value]), setSecondaryPaneOpen: (value) => calls.push(["pane", value]),
  });
  const handler = handlers.openActivityMark;
  try {
    handlers.selectInboxItem(items[0]);
    assert.deepEqual(calls.map(([kind]) => kind), ["selected", "read"]);
    calls.length = 0;
    handler(items[0]); assert.deepEqual(calls.map(([kind]) => kind), ["movement-filter", "side-tab", "selected", "read"]);
    assert.deepEqual(calls[1], ["side-tab", {workspaceKey: "dashboard", surfaceId: "event-log", runId: ""}]);
    assert.deepEqual(calls[3], ["read", [items[0].id]]);
    assert.notEqual(calls[0][1], items[0]); assert.equal(calls[0][1].sequence, 2);
    calls.length = 0; readResult = false; handler(items[0]); assert.equal(calls.at(-1)[0], "warning");
    calls.length = 0; handler({ ...items[0], id: "absent" }); assert.deepEqual(calls.map(([kind]) => kind), ["warning"]);
    items.push({ ...items[0], id: "unaddressable", sequence: null });
    calls.length = 0; handler(items.at(-1)); assert.deepEqual(calls.map(([kind]) => kind), ["warning"]);
    calls.length = 0; readResult = true;
    window.dispatchEvent(new CustomEvent("activate", { detail: { kind: "mark", movementId: items[0].id } }));
    assert.deepEqual(calls[0], ["pane", false]); assert.deepEqual(calls[1], ["surface", "inbox"]); assert.deepEqual(calls.at(-1), ["read", [items[0].id]]);
    calls.length = 0; window.dispatchEvent(new CustomEvent("activate", { detail: { kind: "run", runId: "not-a-mark" } }));
    assert.deepEqual(calls, []);
    for (const role of ["home", "task"]) {
      const item = activity.decodeOperatorActivity(attentionSet(attentionRow(role === "home" ? 90 : 91, "message", { client_surface_role: role })))[0];
      items.push(item); calls.length = 0; handler(item);
      assert.deepEqual(calls[0], [role === "home" ? "home" : "chat", "run-test"]);
      if (role === "home") {
        assert.deepEqual(calls[1], ["home-mode", "activity"]);
        assert.deepEqual(calls[2], ["side-tab", {workspaceKey: "octopus-os", surfaceId: "voice", runId: "run-test"}]);
      }
      assert.deepEqual(calls.at(-1), ["read", [item.id]]);
      assert.ok(!calls.some(([kind]) => kind === "surface"));
      calls.length = 0; window.dispatchEvent(new CustomEvent("activate", { detail: { kind: "mark", movementId: item.id } }));
      assert.deepEqual(calls[0], ["pane", false]); assert.deepEqual(calls[1], ["surface", "inbox"]); assert.deepEqual(calls.at(-1), ["read", [item.id]]);
    }
  } finally { cleanup(); }
});

test("sidebar selection clears secondary focus and opens the primary pane", () => {
  const relative = "src/components/shell/OperatorShell.tsx";
  const parsed = ts.createSourceFile(relative, fs.readFileSync(path.join(appRoot, relative), "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const owner = parsed.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "OperatorShell");
  const names = ["openSidebarSurface", "openSidebarChat"];
  const callbacks = names.map((name) => owner.body.statements.find((node) => ts.isVariableStatement(node)
    && node.declarationList.declarations.some((declaration) => declaration.name.getText(parsed) === name)).getText(parsed));
  for (const pane of ["primary", "secondary"]) {
    const calls = [];
    const dependencies = {
      useCallback: (fn) => fn, activeClientSurfaceIdentity: { pane, workspaceKey: "octopus-os" }, visibleSurfaces: [{ id: "event-log" }],
      setSecondaryPaneFocused: (...args) => calls.push(["focused", ...args]), setSecondaryPaneOpen: (...args) => calls.push(["pane", ...args]),
      openSecondaryTab: (...args) => calls.push(["side-surface", ...args]), secondaryPaneTab: (...args) => args,
      selectSurface: (...args) => calls.push(["main-surface", ...args]), workspaceForSurface: () => "dashboard",
      openSecondaryChatTab: (...args) => calls.push(["side-task", ...args]), openChat: (...args) => calls.push(["task", ...args]),
    };
    const create = load(relative, {}, `export function create(deps: any) {
      const { ${Object.keys(dependencies).join(",")} } = deps;
      ${callbacks.join("\n")}\nreturn { ${names.join(",")} };
    }`).create;
    const handlers = create(dependencies);
    handlers.openSidebarSurface("event-log");
    assert.deepEqual(calls, [["focused", false], ["pane", false], ["main-surface", "event-log"]]);
    calls.length = 0;
    handlers.openSidebarChat("run"); handlers.openSidebarChat("");
    assert.deepEqual(calls, [["task", "run"], ["task", ""]]);
  }
});

test("desktop notification sender respects focus, permission, dedup and exact activation target", () => {
  const previous = { window: global.window, document: global.document, Notification: global.Notification, CustomEvent: global.CustomEvent };
  const sent = [], window = storageWindow();
  let focused = true, activated;
  class CustomEvent extends Event { constructor(type, options) { super(type); this.detail = options.detail; } }
  class Notification {
    static permission = "granted";
    constructor(title, options) { this.title = title; this.options = options; sent.push(this); }
    close() {}
  }
  Object.assign(global, { window, Notification, CustomEvent, document: { hasFocus: () => focused } });
  window.Notification = Notification;
  window.focus = () => {};
  const sender = load("src/clientNotifications.ts");
  window.addEventListener(sender.CLIENT_NOTIFICATION_ACTIVATE_EVENT, (event) => { activated = event.detail; });
  const notification = { key: "test-mark", title: "check.passed", body: "Worker · worker-test", sound: "silent", target: { kind: "mark", movementId: "movements:3" } };
  try {
    assert.equal(sender.notifyClientEvent(notification), false); focused = false;
    Notification.permission = "denied"; assert.equal(sender.notifyClientEvent(notification), false);
    Notification.permission = "granted"; assert.equal(sender.notifyClientEvent(notification), true);
    assert.equal(sender.notifyClientEvent(notification), false); assert.equal(sent.length, 1);
    assert.equal(sent[0].options.silent, true); sent[0].onclick(); assert.deepEqual(activated, notification.target);
    focused = true; assert.equal(sender.notifyClientEvent({ ...notification, key: "always-mark", when: "always" }), true);
  } finally { Object.assign(global, previous); }
});

test("missing, wrong-sequence, unaddressable and index failures never become a successful focused page", async () => {
  const indexFailure = await store.read({ contract_version: 1, method: "projection.movements.list", params: { before_seq: 3, after_seq: 1 } })
    .then(() => assert.fail("exact paging without an index must fail"), (error) => error);
  assert.match(indexFailure.message, /ledger index is required/);
  for (const response of [{ movements: [], total: 0 }, { movements: [{ ...movementRows[0], seq: 1 }] }, { invalid: [] }, indexFailure]) {
    const h = eventLog();
    try {
      if (response instanceof Error) h.requests[0].reject(response); else h.requests[0].resolve(response);
      await h.settle();
      assert.equal(elements(h.value, (node) => node.type?.name === "MovementRowView").length, 0);
      assert.equal(elements(h.value, (node) => node.type?.name === "ProjectionResourceFailure").length, 1);
    } finally { h.close(); }
  }
  const h = eventLog();
  try {
    h.props.focusedMovement = { id: "movements:3", sequence: null }; h.update(); await h.settle();
    assert.equal(elements(h.value, (node) => node.type?.name === "ProjectionResourceFailure").length, 1);
  } finally { h.close(); }
});
