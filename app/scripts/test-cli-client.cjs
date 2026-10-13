const assert = require("node:assert/strict");
const { test } = require("node:test");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { createClient } = require("../cli/client.cjs");
const { argumentsFor, createEventRenderer } = require("../cli/index.cjs");

const run = { run_id: "run_task", chat_interface_id: "chat_task", provider_interface_id: "provider",
  provider_bound_interface_id: "provider_task", client_surface_role: "task", writable: true, turn_open: true, turn_state: "working" };
const receipt = { runId: run.run_id, chatInterfaceId: run.chat_interface_id, modelInterfaceId: run.provider_bound_interface_id,
  messageId: "message", messageVerdict: { admitted: { seq: 100 } }, afterSeq: 100, cursor: { offset: 10, seq: 100 } };

test("Stop uses scoped exact Home and run identities without decoding any conversation", async () => {
  const scope = { status: "ready", home_run_id: "run_home", home_interface_id: "chat_home", client_runs: [
    { run_id: "run_home", interface_id: "chat_home", surface_role: "home", archived: false },
    { run_id: "run_task", interface_id: "chat_task", surface_role: "task", archived: false },
    { run_id: "run_old", interface_id: "chat_old", surface_role: "task", archived: true },
  ] };
  const aliases = [scope.client_runs[2], scope.client_runs[1], scope.client_runs[0]].map(row => ({
    run_id: row.run_id, chat_interface_id: row.interface_id, title: row.run_id === "run_task" ? "Work" : row.run_id, writable: true,
  }));
  let aliasReads = 0, submitted = 0;
  const store = { async read({ method, params }) {
    if (method === "projection.operator.os.scope") return scope;
    assert.equal(method, "projection.operator.chats.list", "Stop must not request conversation payloads");
    assert.equal(params.control_targets_only, true); aliasReads++;
    return { chats: aliases };
  }, invalidate() {}, close() {} };
  const client = createClient({ home: os.tmpdir(), packageRoots: {}, status: async () => ({ runtimeReady: true }) }, {
    store, loader: { load: async () => ({ async submitOperatorTurnControl(_options, input) {
      submitted++; assert.equal(input.action, "stop"); return { ...input, verdict: { admitted: { seq: 1 } } };
    } }) }, metadata: {},
  });
  try {
    assert.equal((await client.stop("home")).interfaceId, "chat_home");
    assert.equal((await client.stop("run_task")).interfaceId, "chat_task");
    assert.equal(aliasReads, 0);
    for (const selector of ["last", "Work", "run_ta"]) assert.equal((await client.stop(selector)).runId, "run_task");
    assert.equal(aliasReads, 3);
    await assert.rejects(client.stop("run_"), /ambiguous/);
    await assert.rejects(client.stop("missing"), /No task/);
    await assert.rejects(client.stop("run_old"), /cannot accept Stop/);
    const before = submitted;
    scope.home_interface_id = "other"; await assert.rejects(client.stop("home"), /No Home/);
    scope.home_interface_id = "chat_home";
    aliases[1].chat_interface_id = "foreign"; await assert.rejects(client.stop("Work"), /differs/);
    scope.client_runs.push(scope.client_runs[1]); await assert.rejects(client.stop("run_task"), /invalid control target/);
    scope.client_runs.pop(); scope.status = "degraded"; await assert.rejects(client.stop("home"), /valid client scope/);
    assert.equal(submitted, before);
  } finally { await client.close(); }
});
function movement(seq, overrides = {}) {
  return { source: "movement", run_id: run.run_id, interface_id: run.chat_interface_id,
    from_interface_id: run.provider_bound_interface_id, from_interface_role: "provider",
    movement_seq: seq, recorded_at_unix_ms: seq, checkpoint_id: "cp", content_hash: "a".repeat(64),
    artifact_id: `reply:${seq}`, label: "interface.artifact.output", event_kind: "message",
    event_data: {}, text: "Reply", error: false, terminal: true, will_retry: false, ...overrides };
}
async function fixture(batches, check, { turnOpen = true, messages = [], authorize } = {}) {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), "oct-cli-contract-"));
  let reads = 0;
  const store = {
    async read({ method }) {
      if (method === "projection.operator.chats.list") return { chats: [{ ...run, turn_open: turnOpen,
        turn_state: turnOpen ? "working" : "idle" }, { run_id: "internal_writer" }] };
      if (method === "projection.operator.chat.messages") return { messages };
      if (method === "projection.operator.chats.status") return { chats: [{ run_id: run.run_id,
        turn_open: turnOpen, turn_state: turnOpen ? "working" : "idle", turn_opened_movement_seq: 100, last_movement_seq: 110 }] };
      throw new Error(`Unexpected projection: ${method}`);
    }, close() {}, invalidate() {},
  };
  const transport = { assertGenericOperatorChatArtifactAllowed: authorize, async readOperatorChatArtifacts(_options, request) {
    assert.equal(request.runId, run.run_id);
    assert.equal(request.interfaceId, run.chat_interface_id);
    assert.equal(request.afterSeq, 100);
    const artifacts = typeof batches === "function" ? await batches(reads++, request) : batches[reads++];
    assert.ok(artifacts, "follow must stop at the genuine current terminal");
    return { artifacts, cursor: { offset: 100 + reads, seq: 100 + reads } };
  } };
  const client = createClient({ home, packageRoots: {}, status: async () => ({ runtimeReady: true }), accountClient() {} },
    { store, loader: { load: async () => transport }, metadata: {} });
  try { await check(client, () => reads); }
  finally { await client.close(); await fs.rm(home, { recursive: true, force: true }); }
}

test("the terminal lists only actual chat surfaces and follows durable exact-provider completion", async () => {
  const stream = { source: "stream", run_id: run.run_id, interface_id: run.chat_interface_id,
    from_interface_id: run.provider_bound_interface_id, from_interface_role: "provider", stream_id: "a".repeat(64),
    stream_seq: 1, byte_start_offset: 0, byte_end_offset: 80, source_movement_seq: 105, recorded_at_unix_ms: 105,
    event_kind: "turn_completed", event_data: { turn: { id: "turn", status: "completed" } }, terminal: true };
  await fixture([[stream], [movement(90)], [movement(111, { from_interface_id: "other_provider" })],
    [movement(112, { error: true, will_retry: true })], [movement(120)]], async (client, reads) => {
    assert.equal((await client.tasks()).length, 1);
    const events = await Array.fromAsync(client.follow(run.run_id, receipt));
    assert.equal(reads(), 5);
    const final = events.at(-1);
    assert.equal(final.type, "status");
    assert.equal(final.state, "idle");
    assert.equal(final.correlated, true);
    assert.equal(final.terminal, true);
  });
});

test("wrong task receipts fail before reading, detach does not send Stop, watch uses the recorded frontier", async () => {
  await fixture([ [movement(120)] ], async (client, reads) => {
    await assert.rejects(Array.fromAsync(client.follow(run.run_id, { ...receipt, modelInterfaceId: "other" })), /different task/);
    assert.equal(reads(), 0);
    const controller = new AbortController(); controller.abort();
    await Array.fromAsync(client.follow(run.run_id, { ...receipt, signal: controller.signal }));
    assert.equal(reads(), 0);
    const events = await Array.fromAsync(client.follow(run.run_id));
    assert.equal(events.at(-1).terminal, true);
    assert.equal(reads(), 1);
  });
});

test("an early idle projection cannot hide a final reply that has not arrived yet", async () => {
  await fixture([[], [movement(120)]], async (client, reads) => {
    const events = await Array.fromAsync(client.follow(run.run_id));
    assert.equal(reads(), 2);
    assert.ok(events.some((event) => event.entry?.text === "Reply"));
    assert.equal(events.at(-1).terminal, true);
  }, { turnOpen: false });
});

test("submission, watch and restored idle projections wait past Stop ACK and reject stale closure providers", async () => {
  const ack = movement(110, { label: "turn.control.acknowledged", event_kind: "turn_control", event_data: {
    action: "stop", status: "acknowledged", turn_id: "turn", thread_id: "thread", detail: { exact_result_pending: true } } });
  const closure = movement(125, { label: "turn.interrupted.result", event_kind: "interrupted_result", event_data: {
    result_kind: "tool_result", correlation_id: "call", turn_id: "turn", thread_id: "thread", ok: true,
    turn_interrupted: true, returned_to_model: false, result_disposition: "durably_admitted_after_interrupt", source_artifact: { id: "result", content: Array(32).fill(10) } } });
  for (const mode of ["submission", "watch", "restored"]) {
    const originalReceipt = structuredClone(receipt);
    const batches = [[ack], [{ ...closure, movement_seq: 120, artifact_id: "wrong-provider", from_interface_id: "old-provider" }],
      [{ ...closure, movement_seq: 121, artifact_id: "old-turn", event_data: { ...closure.event_data, turn_id: "old" } }], [closure]];
    await fixture((index, request) => {
      assert.deepEqual(request.cursor, index ? { offset: 100 + index, seq: 100 + index } : receipt.cursor);
      return batches[index];
    }, async (client, reads) => {
      const events = await Array.fromAsync(client.follow(run.run_id, mode === "submission" ? receipt : { cursor: receipt.cursor }));
      assert.equal(reads(), 4);
      assert.ok(events.some((event) => event.type === "status" && event.state === "stopping" && !event.terminal));
      assert.equal(events.filter((event) => event.type === "status" && event.terminal).length, 1);
      assert.equal(events.at(-1).entry.kind, "interrupted_result");
      assert.equal(events.at(-1).interrupted, true);
      assert.deepEqual(receipt, originalReceipt);
    }, { turnOpen: false, messages: mode === "restored" ? [{ ...ack, id: "ack-history" }] : [] });
  }
});

test("noninteractive results retain only the selected turn reply through Stop, failure, replay and streaming", async () => {
  const Module = require("node:module"), file = path.resolve(__dirname, "../cli/index.cjs");
  const source = await fs.readFile(file, "utf8");
  const ack = movement(110, { event_kind: "turn_control", event_data: { action: "stop", status: "acknowledged",
    turn_id: "turn", thread_id: "thread", detail: { exact_result_pending: true } } });
  const closure = movement(120, { event_kind: "interrupted_result", event_data: { result_kind: "tool_result", correlation_id: "call",
    turn_id: "turn", thread_id: "thread", ok: true, turn_interrupted: true, returned_to_model: false,
    result_disposition: "durably_admitted_after_interrupt", source_artifact: { id: "result", content: Array(32).fill(10) } } });
  const oldReply = movement(90, { text: "Old baseline answer", event_data: { turn_id: "old", thread_id: "thread" } });
  for (const [result, expectedCode, command, restored, partial] of [
    ["stopped", 130, "watch", false, false], ["failed", 1, "watch", false, false], ["timeout", 124, "watch", false, false],
    ["stopped", 130, "watch", true, false], ["stopped", 130, "watch", true, true],
    ["stopped", 130, "send", false, true], ["failed", 1, "send", false, true], ["completed", 0, "send", false, true],
  ]) {
    const ending = result === "completed" ? movement(120, { text: "Current final", event_data: { turn_id: "turn", thread_id: "thread" } })
      : { ...closure, event_data: { ...closure.event_data, ok: result !== "failed" } };
    const fragments = ["Current ", "partial"].map((text, index) => ({
      source: "stream", run_id: run.run_id, interface_id: run.chat_interface_id,
      from_interface_id: run.provider_bound_interface_id, from_interface_role: "provider", recorded_at_unix_ms: 105 + index,
      source_movement_seq: 100, stream_id: "b".repeat(64), stream_seq: index + 1,
      byte_start_offset: index * 100, byte_end_offset: (index + 1) * 100, terminal: false, event_kind: "message_delta", text,
      event_data: { text, turn_id: "turn", thread_id: "thread", item_id: "current-reply" },
    }));
    const history = [oldReply, ...(restored ? [
      ...(partial ? [movement(105, { text: "Current partial", terminal: false, event_data: { turn_id: "turn", thread_id: "thread" } })] : []),
      movement(108, { text: "Other provider reply", terminal: false, from_interface_id: "other-provider", event_data: { turn_id: "turn", thread_id: "thread" } }),
      ack, ending,
    ] : [])];
    await fixture(async (index) => {
    if (!index) return [...(partial ? fragments : []), ...(result === "completed" ? [] : [ack])];
    if (result !== "timeout") assert.equal(index, 1);
    await new Promise((resolve) => setTimeout(resolve, 30));
    return result === "timeout" ? [] : [ending];
  }, async (client, reads) => {
    const unit = new Module(file, module); unit.filename = file; unit.paths = Module._nodeModulePaths(path.dirname(file));
    const nativeRequire = Module.createRequire(file);
    const observed = { ...client, async send(input) { assert.equal(input.runId, run.run_id); return receipt; }, async *follow(...args) {
      yield { type: "message", entry: { id: "commentary", role: "assistant", kind: "commentary", text: "I am working" } };
      yield { type: "delta", id: "commentary", role: "assistant", text: " on this." };
      yield* client.follow(...args);
    } };
    unit.require = (id) => id === "./environment.cjs" ? { createEnvironment: () => ({ close() {}, start() { throw new Error("No runtime startup allowed"); } }) }
      : id === "./client.cjs" ? { createClient: () => observed } : nativeRequire(id);
    unit._compile(source, file);
    let output = "";
    const destination = path.join(client.env.home, "last-message.txt");
    const code = await unit.exports.main([command, run.run_id, ...(command === "send" ? ["Continue"] : []), "--json", "--output-last-message", destination,
      ...(result === "timeout" ? ["--timeout", "0.01"] : [])], {
      stdin: { isTTY: false }, stdout: { write: (text) => { output += text; } }, stderr: { write() {} },
    });
    const rows = output.trim().split("\n").map(JSON.parse);
    assert.equal(code, expectedCode, JSON.stringify({ result, command, restored, partial, final: rows.at(-1) }));
    assert.equal(reads(), restored ? 0 : 2);
    if (!restored && result !== "completed") assert.ok(rows.some((row) => row.type === "status" && row.state === "stopping" && !row.terminal));
    assert.equal(rows.some((row) => row.type === "result" && row.status === "completed"), result === "completed");
    assert.equal(rows.at(-1).type, result === "timeout" ? "timeout" : "result");
    if (result !== "timeout") {
      assert.equal(rows.at(-1).status, result);
      const expected = result === "completed" ? "Current final" : partial ? "Current partial" : "";
      assert.equal(rows.at(-1).text, expected);
      assert.equal(await fs.readFile(destination, "utf8"), expected);
    } else await assert.rejects(fs.access(destination), { code: "ENOENT" });
  }, { turnOpen: !restored, messages: history });
  }
});

test("task deletion and follow-up refuse active work or an unresolved Stop even with an idle projection", async () => {
  const ack = movement(110, { event_kind: "turn_control", event_data: { action: "stop", status: "acknowledged",
    turn_id: "turn", thread_id: "thread", detail: { exact_result_pending: true } } });
  for (const turnOpen of [true, false]) await fixture([], async (client) => {
    await assert.rejects(client.deleteTask(run.run_id), /wait for it to settle/);
    await assert.rejects(client.send({ runId: run.run_id, text: "Do not send while stopping" }),
      (error) => error.code === (turnOpen ? "task_busy" : "task_stopping"));
  }, { turnOpen, messages: turnOpen ? [] : [ack] });
});

test("bound approvals accept terminal decisions and preserve exact CLI responses", async () => {
  const data = { request_id: "req", call_id: "call", kind: "application", detail: { approval_scope: "formed_app_target" } };
  const messages = [{ id: "question", event_kind: "approval", event_data: data, run_id: run.run_id, interface_id: run.chat_interface_id }];
  const captured = [];
  const boundary = new Error("Pure artifact boundary; no delivery attempted");
  await fixture([], async (client) => {
    for (const answer of [{ kind: "approval", decision: "accept" }, { kind: "approval", decision: "decline" }, { kind: "approval", decision: "cancel" }, { decision: "decline" }]) {
      await assert.rejects(client.respond(run.run_id, { kind: "approval", payload: data }, answer), (error) => error === boundary);
      assert.deepEqual(captured.at(-1).payload, { operator_approval_response: { request_id: "req", call_id: "call", decision: answer.decision } });
    }
    await assert.rejects(client.respond(run.run_id, "req", { kind: "approval", decision: "acceptForSession" }), /no supported decision/);
    assert.equal(captured.length, 4);
  }, { messages, authorize: (request) => { captured.push(request); throw boundary; } });
});

test("native permission refusal and MCP decline/cancel preserve typed backend responses", async () => {
  const permissions = { network: { enabled: true }, fileSystem: { read: ["/fixture/read"], write: ["/fixture/write"] } };
  const approval = { request_id: "permission", method: "item/permissions/requestApproval", detail: { permissions } };
  const elicitation = { request_id: "mcp", method: "mcpServer/elicitation/request", requested_schema: { type: "object", required: ["count"], properties: { count: { type: "integer" } } } };
  const messages = [["approval", approval], ["elicitation", elicitation]].map(([event_kind, event_data]) => ({
    id: event_data.request_id, event_kind, event_data, run_id: run.run_id, interface_id: run.chat_interface_id,
  }));
  const boundary = new Error("Pure artifact boundary; no delivery attempted"), captured = [];
  await fixture([], async (client) => {
    for (const decision of [{ permissions: {}, scope: "turn" }, { permissions, scope: "turn" }, { permissions, scope: "session" }]) {
      await assert.rejects(client.respond(run.run_id, "permission", { kind: "approval", decision }), (error) => error === boundary);
      assert.deepEqual(captured.at(-1).payload, { operator_approval_response: { request_id: "permission", decision } });
      assert.equal(captured.at(-1).label, "operator.approval.response");
    }
    for (const action of ["decline", "cancel"]) {
      const answer = { action, _meta: null };
      await assert.rejects(client.respond(run.run_id, "mcp", { kind: "elicitation", answer }), (error) => error === boundary);
      assert.deepEqual(captured.at(-1).payload, { operator_elicitation_response: { request_id: "mcp", answer } });
      assert.equal(captured.at(-1).label, "operator.elicitation.response");
    }
    assert.ok(captured.every((row) => row.runId === run.run_id && row.interfaceId === run.chat_interface_id));
  }, { messages, authorize: (request) => { captured.push(request); throw boundary; } });
});

test("noninteractive text is incremental, safe and complete; unsupported flags fail", () => {
  let output = "", errors = "";
  const emit = createEventRenderer({ stdout: { write: text => { output += text; } }, stderr: { write: text => { errors += text; } } });
  emit({ type: "message", entry: { id: "reply", role: "assistant", text: "Hel" } });
  emit({ type: "delta", id: "reply", role: "assistant", text: "lo" });
  emit({ type: "message", entry: { id: "reply", role: "assistant", text: "Hello" } });
  emit({ type: "message", entry: { id: "account", role: "assistant", kind: "event", text: "provider.account.updated" } });
  emit({ type: "activity", id: "tool", title: "Run\x1b[2J", status: "working" });
  assert.equal(output, "\nHello");
  assert.equal(errors, "Run · working\n");
  const failed = { type: "activity", id: "tool", title: "Code", status: "failed", script: "text(31); invalid();",
    output: "31", failure: "ReferenceError: invalid is not defined", notices: ["Output retained in the artifact."] };
  emit(failed);
  emit(failed);
  assert.equal(errors, "Run · working\nCode · failed\ntext(31); invalid();\n31\nReferenceError: invalid is not defined\nOutput retained in the artifact.\n");
  emit({ type: "activity", id: "exit", title: "Command", status: "exited", exitCode: 2 });
  assert.ok(errors.endsWith("Command · exited · exit 2\n"));
  assert.throws(() => argumentsFor(["setup", "--workspace-root", "/tmp"]), /Unknown option/);
  assert.throws(() => argumentsFor(["exec", "--timeout", "NaN"]), /between/);
});
