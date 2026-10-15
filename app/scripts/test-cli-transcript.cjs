"use strict";
const assert = require("node:assert/strict");
const { test } = require("node:test");
const { normalizeHistory, createTranscriptReducer, renderMessageData, safeText } = require("../cli/transcript.cjs");

const STREAM = "a".repeat(64);
function stream(seq, kind, data = {}, overrides = {}) {
  return { source: "stream", stream_id: STREAM, stream_seq: seq,
    byte_start_offset: seq * 100, byte_end_offset: (seq + 1) * 100, source_movement_seq: 100,
    recorded_at_unix_ms: seq * 10, run_id: "run", interface_id: "chat",
    from_interface_id: "provider", from_interface_role: "provider", label: "agent.message.delta",
    event_kind: kind, event_data: { turn_id: "turn", item_id: "item", ...data }, text: "projected preview",
    error: false, terminal: false, will_retry: false, ...overrides };
}
function movement(seq, kind, data = {}, overrides = {}) {
  return { source: "movement", movement_seq: seq, artifact_id: `artifact-${seq}`, content_hash: `hash-${seq}`,
    checkpoint_id: "cp", recorded_at_unix_ms: seq * 10, run_id: "run", interface_id: "chat",
    from_interface_id: "provider", from_interface_role: "provider", label: "interface.artifact.output",
    event_kind: kind, event_data: { turn_id: "turn", item_id: "item", ...data }, text: "Done",
    error: false, terminal: false, will_retry: false, ...overrides };
}
const receipt = (overrides = {}) => ({ runId: "run", chatInterfaceId: "chat", modelInterfaceId: "provider",
  messageId: "message", messageVerdict: { admitted: { seq: 100 } }, outboundCursor: { offset: 500, seq: null }, ...overrides });
function recorded(seq, overrides = {}) {
  return stream(seq, "input_recorded", { type: "input_recorded", seq, source_movement_seq: 105,
    stage: "input_recorded_in_turn", source_event: "item/started", native_event_at_ms: 1,
    provider_interface: "provider", run: "run", thread_id: "thread", turn_id: "turn", item_id: "user-item", client_id: "message" },
  { source_movement_seq: 105, label: "turn.stream", text: "", ...overrides });
}
const ended = (events) => events.filter((event) => event.type === "status" && event.terminal && event.correlated);

test("safeText strips control strings, C1, incomplete tails and bidi without losing ordinary text", () => {
  assert.equal(safeText("hi\x1b[31mred\x1b[0m\r\n\t🙂"), "hired\n\t🙂");
  for (const hidden of ["\x1b]52;c;secret\x07", "\x1b]8;;url\x1b\\", "\x90secret\x9c", "\x1bPsecret\x1b\\", "\x1b_secret\x1b\\", "\x9dsecret\x9c"]) {
    assert.equal(safeText(`A${hidden}B`), "AB");
  }
  assert.equal(safeText("A\x9b2JB\0\b\x7f\u202eC\u2066D"), "ABCD");
  assert.equal(safeText("safe\x1b]52;c;unfinished"), "safe");
  assert.equal(safeText("safe\x1b["), "safe");
  assert.equal(safeText({ text: "do not stringify arbitrary payloads" }), "");
});

test("raw and desktop history keep composition, attachments and diagnostics", () => {
  const raw = { id: "history", run_id: "run", interface_id: "chat", movement_seq: 8,
    role: "assistant", event_kind: "message_delta", event_data: { text: "last fragment" }, text: "All fragments", terminal: false };
  const typed = { id: "user", runId: "run", interfaceId: "chat", movementSeq: 7, role: "user", text: "Read this", eventKind: "message",
    eventData: {}, attachments: [{ id: "file", name: "a\x1b[2J.txt", type: "text/plain", size: 12, artifact_id: "file:hash" }] };
  const result = normalizeHistory([raw, typed]);
  assert.equal(result[1].text, "All fragments");
  assert.equal(result[0].attachments[0].name, "a.txt");
  assert.equal(result[0].attachments[0].artifactId, "file:hash");
  assert.equal(result[0].metadata.movementSeq, 7);
  const malformed = renderMessageData({ ...raw, event_data: {} });
  assert.equal(malformed.kind, "generic_activity");
  assert.match(malformed.metadata.diagnostic.reason, /missing typed/);
  assert.equal(raw.text, "All fragments");
});

test("history retains explicit terminal errors and Stop proofs without promoting native completion activity", () => {
  const raw = (seq, kind, data, flags) => {
    const row = movement(seq, kind, { ...data, _activity_origin: { interface_id: "provider", interface_role: "provider" } }, flags);
    delete row.source; delete row.from_interface_id; delete row.from_interface_role;
    return row;
  };
  const rows = [raw(315, "turn_completed", { status: "completed" }, { terminal: false }),
    raw(329, "message", {}, { terminal: true }), raw(340, "error", { message: "Failed", will_retry: false }, { terminal: true, error: true }),
    raw(350, "turn_control", { action: "stop", status: "acknowledged" }, { terminal: true }),
    raw(360, "error", { message: "Retrying", will_retry: true }, { terminal: false, error: true })];
  const entries = normalizeHistory(rows);
  assert.deepEqual(entries.map(entry => [entry.metadata.movementSeq, entry.terminal, entry.error]),
    [[315, false, false], [329, true, false], [340, true, true], [350, true, false], [360, false, true]]);
  for (const entry of entries) {
    assert.equal(entry.metadata.source, "movement");
    assert.equal(entry.metadata.fromInterfaceId, "provider");
    assert.equal(entry.metadata.fromInterfaceRole, "provider");
    assert.equal(entry.metadata.direction, "out");
  }
  const reducer = createTranscriptReducer();
  reducer.beginTurn(receipt({ messageVerdict: { admitted: { seq: 300 } } }));
  assert.equal(ended(reducer.pushBatch(rows.slice(0, 1))).length, 0);
  assert.equal(ended(reducer.pushBatch([stream(315, "turn_completed", { status: "completed" }, { terminal: true })])).length, 0);
  assert.equal(ended(reducer.pushBatch([])).length, 0);
  const final = ended(reducer.pushBatch(rows.slice(1, 2)));
  assert.equal(final.length, 1);
  assert.equal(final[0].proof, "durable_movement_after_admission");
  assert.equal(reducer.currentTurn().outcome.movementSeq, 329);
});

test("stream fragments append exactly, replay is inert, completion replaces only its item", () => {
  const reducer = createTranscriptReducer();
  let events = reducer.pushBatch({ artifacts: [stream(1, "message_delta", { text: "Hel" })], cursor: { offset: 50 } });
  const id = events[0].entry.id;
  assert.equal(events[0].entry.text, "Hel");
  events = reducer.pushBatch([stream(2, "message_delta", { text: "lo\n" })]);
  assert.deepEqual(events, [{ type: "delta", id, role: "assistant", text: "lo\n" }]);
  assert.deepEqual(reducer.pushBatch([stream(2, "message_delta", { text: "lo\n" })]), []);
  reducer.pushBatch([stream(3, "message_delta", { text: "Other", item_id: "other" })]);
  events = reducer.pushBatch([movement(120, "message", { lifecycle: "completed" }, { text: "Hello!", terminal: true })]);
  assert.equal(events[0].entry.id, id);
  assert.equal(events[0].entry.text, "Hello!");
  reducer.pushBatch([stream(4, "message_delta", { text: " stale fragment" })]);
  assert.deepEqual(reducer.snapshot().map((entry) => entry.text), ["Hello!", "Other"]);
});

test("split terminal escapes remain inert across batches and typed empty deltas stay empty", () => {
  const reducer = createTranscriptReducer();
  reducer.pushBatch([stream(1, "message_delta", { text: "A\x1b]52;c;" })]);
  assert.equal(reducer.snapshot()[0].text, "A");
  const events = reducer.pushBatch([stream(2, "message_delta", { text: "c2VjcmV0\x07B" })]);
  assert.equal(events[0].text, "B");
  assert.equal(reducer.snapshot()[0].text, "AB");
  assert.deepEqual(reducer.pushBatch([stream(3, "message_delta", { text: "" })]), []);
  assert.equal(reducer.snapshot()[0].text, "AB");
});

test("history stream frontier prevents replay when attaching; new fragments extend composed text", () => {
  const row = { id: "composed", runId: "run", interfaceId: "chat", role: "assistant", text: "Hello", eventKind: "message_delta",
    eventData: { text: "lo", turn_id: "turn", item_id: "item", _activity_origin: { interface_id: "provider", interface_role: "provider" },
      _stream: { stream_id: STREAM, stream_seq: 2, byte_end_offset: 300 } } };
  const reducer = createTranscriptReducer({ history: [row] });
  assert.deepEqual(reducer.pushBatch([stream(1, "message_delta", { text: "Hel" }), stream(2, "message_delta", { text: "lo" })]), []);
  assert.equal(reducer.pushBatch([stream(3, "message_delta", { text: "!" })])[0].text, "!");
  assert.equal(reducer.snapshot()[0].text, "Hello!");
});

test("reasoning sections and distinct provider/item identities are not conflated", () => {
  const reducer = createTranscriptReducer();
  reducer.pushBatch([stream(1, "reasoning_delta", { delta: "summary", summary_index: 0 }),
    stream(2, "reasoning_delta", { delta: "content", content_index: 0 }),
    stream(3, "message_delta", { text: "one" }),
    stream(4, "message_delta", { text: "two" }, { from_interface_id: "other" })]);
  assert.deepEqual(reducer.snapshot().map((entry) => entry.text), ["summary", "content", "one", "two"]);
});

test("command start/output/completion retain actual arguments, outputs and call origin", () => {
  const reducer = createTranscriptReducer();
  reducer.pushBatch([stream(1, "dynamic_tool", { tool: "exec_command", call_id: "call", status: "started",
    arguments: { argv: ["printf", "hello world"], workdir: "/work" } })]);
  reducer.pushBatch([stream(2, "tool_output_delta", { call_id: "call", delta: "hello " })]);
  assert.equal(reducer.snapshot()[0].activity.output, "hello ");
  reducer.pushBatch([movement(115, "dynamic_tool", { call_id: "call", status: "completed", ok: true,
    body: { output: { stdout: "hello world\n", stderr: "detail\n", exit_code: 0 } } }, { from_interface_id: "exec-door", from_interface_role: "system" })]);
  const [entry] = reducer.snapshot();
  assert.equal(reducer.snapshot().length, 1);
  assert.equal(entry.activity.command, "printf 'hello world'");
  assert.equal(entry.activity.cwd, "/work");
  assert.equal(entry.activity.stdout, "hello world\n");
  assert.equal(entry.activity.stderr, "detail\n");
  assert.equal(entry.status, "exited");
  assert.equal(entry.metadata.fromInterfaceId, "provider");
  assert.equal(entry.metadata.artifactId, "artifact-115");
});

test("completed invocation can still be running; binary/truncation evidence is explicit", () => {
  const entry = renderMessageData({ ...movement(111, "dynamic_tool", { tool: "execute", status: "completed", arguments: { script: "print('hello')\n" },
    body: { process_id: "process", completed: false, continuation_available: true,
      output: { running: true, stdout: { encoding: "base64", data: "AA==" }, stderr: { text: "warn" }, stderr_omitted_bytes: 50 } },
    _payload_projection: { truncated: true, contains_process_output: true } }), id: "tool" });
  assert.equal(entry.activity.status, "running");
  assert.equal(entry.activity.yielded, true);
  assert.equal(entry.activity.script, "print('hello')\n");
  assert.equal(entry.activity.command, "print('hello')\n");
  assert.equal(entry.activity.continuationAvailable, true);
  assert.equal(entry.activity.exitCode, null);
  assert.match(entry.activity.notices.join("\n"), /Binary output/);
  assert.match(entry.activity.notices.join("\n"), /50 bytes omitted/);
  assert.equal(entry.metadata.payloadProjection.truncated, true);
  assert.equal(typeof entry.activity.result, "object");
});

test("recorded CodeMode calculation results expose actual completion and script failure", () => {
  const recorded = [
    { script: "17 + 8 + 6", result: { cell_id: "1", emissions: [], error_text: null, output: [], pending_tool_calls: [], state: "completed" } },
    { script: "console.log(17 + 8 + 6)", result: { cell_id: "2", emissions: [],
      error_text: "ReferenceError: console is not defined\n    at <anonymous>:1:1", output: [], pending_tool_calls: [], state: "completed" } },
  ];
  for (const [index, fixture] of recorded.entries()) {
    const result = fixture.result;
    const entry = renderMessageData(movement(111 + index, "dynamic_tool", {
      tool: "code_mode.run__135ec22a8a7fa82a1358557038038d6f", status: "completed",
      arguments: { script: fixture.script }, result,
    }));
    assert.equal(entry.status, result.error_text ? "failed" : "completed");
    assert.equal(entry.activity.failure, result.error_text ?? "");
    assert.equal(entry.activity.output, result.error_text ?? "");
    assert.equal(entry.activity.script, fixture.script);
    assert.equal(entry.terminal, false);
    assert.deepEqual(entry.activity.result, result);
  }
});

test("structured result output and execution states work without a tool-name branch", () => {
  for (const [state, expected] of [["completed", "completed"], ["yielded", "running"], ["terminated", "interrupted"]]) {
    const entry = renderMessageData(movement(111, "dynamic_tool", { tool: "another_tool", status: "completed",
      result: { state, output: [{ type: "text", text: "31" }, { type: "text", text: "safe\x1b[2J" }] } }));
    assert.equal(entry.status, expected);
    assert.equal(entry.activity.output, "31\nsafe");
    assert.equal(entry.activity.yielded, state === "yielded");
  }
  const entry = renderMessageData(movement(111, "dynamic_tool", { status: "completed",
    result: { state: "completed", error_text: "\x1b[31mactual failure", output: [{ type: "text", text: "before" }] } }));
  assert.equal(entry.status, "failed");
  assert.equal(entry.activity.failure, "actual failure");
  assert.equal(entry.activity.output, "before\nactual failure");
});

test("large tool results stay structured and textual output is not shortened", () => {
  const output = "x".repeat(25000);
  const entry = renderMessageData({ ...movement(111, "tool", { command: "arbitrary program", status: "completed",
    output: { chunks: [{ stream: "stdout", content: "\x1b]52;c;" }, { stream: "stdout", content: "bad\x07" + output }], exit_code: 9 } }), id: "tool" });
  assert.equal(entry.activity.output, output);
  assert.equal(entry.activity.exitCode, 9);
});

test("actual required questions, exact option values, schemas and responses survive history/live", () => {
  const reducer = createTranscriptReducer();
  const payload = { request_id: "req", method: "item/tool/requestUserInput", title: "Choose\x1b[2J",
    requested_schema: { questions: [{ id: "q", question: "Which file?", header: "File", isSecret: true, isOther: true,
      options: [{ label: "a\x1b[31m", description: "first" }] }] } };
  const [event] = reducer.pushBatch([movement(111, "elicitation", payload)]);
  assert.equal(event.type, "request");
  assert.equal(event.kind, "user_input");
  assert.equal(event.entry.request.title, "Choose");
  assert.equal(event.entry.request.questions[0].options[0].label, "a");
  assert.equal(event.entry.request.questions[0].options[0].value, "a\x1b[31m");
  assert.equal(event.payload.requested_schema.questions[0].options[0].label, "a\x1b[31m");
  assert.equal(event.entry.request.questions[0].isSecret, true);
  const events = reducer.pushBatch([movement(112, "event", { operator_user_input_response: { request_id: "req", answers: { q: ["a"] } } }, { direction: "in", role: "user" })]);
  const resolved = events.find((entry) => entry.type === "request");
  assert.equal(resolved.status, "resolved");
  assert.deepEqual(resolved.entry.request.response.answers, { q: ["a"] });
  assert.equal(resolved.entry.request.actionable, false);
});

test("approval offers and native request resolutions are scoped by run/interface", () => {
  const reducer = createTranscriptReducer();
  reducer.pushBatch([movement(111, "approval", { request_id: "req", kind: "application", call_id: "call",
    detail: { permission_offer: { client_interface: "chat", offered_choices: [{ decision: "accept", persistence: "once" }] } } })]);
  reducer.pushBatch([movement(112, "operator_request_resolved", { requestId: "req" }, { run_id: "other-run" })]);
  assert.equal(reducer.snapshot()[0].request.status, "pending");
  reducer.pushBatch([movement(113, "operator_request_resolved", { requestId: "req" })]);
  assert.equal(reducer.snapshot()[0].request.status, "resolved");
  assert.deepEqual(reducer.snapshot()[0].request.choices, [{ decision: "accept", persistence: "once" }]);
  assert.equal(reducer.snapshot()[0].request.callId, "call");
});

test("watch ignores prior admission, known stale turns, stream completion, retry errors, and wrong provider", () => {
  const reducer = createTranscriptReducer(); reducer.beginTurn(receipt());
  const stale = movement(100, "message", { turn_id: "old" }, { terminal: true });
  assert.equal(ended(reducer.pushBatch([stale])).length, 0);
  reducer.pushBatch([stream(1, "turn_started")]);
  assert.equal(ended(reducer.pushBatch([movement(110, "message", { turn_id: "old" }, { terminal: true })])).length, 0);
  assert.equal(ended(reducer.pushBatch([stream(2, "turn_completed", { status: "completed" }, { terminal: true })])).length, 0);
  assert.equal(ended(reducer.pushBatch([movement(112, "error", { will_retry: true }, { terminal: true, error: true, will_retry: true })])).length, 0);
  assert.equal(ended(reducer.pushBatch([movement(113, "message", {}, { terminal: true, from_interface_id: "other" })])).length, 0);
  for (const scope of [{ run_id: "other" }, { interface_id: "other" }, { direction: "in" }]) {
    assert.equal(ended(reducer.pushBatch([movement(114, "message", {}, { ...scope, terminal: true })])).length, 0);
  }
  const events = reducer.pushBatch([movement(115, "message", {}, { terminal: true })]);
  assert.equal(ended(events).length, 1);
  assert.equal(ended(events)[0].messageId, "message");
  assert.equal(reducer.currentTurn().terminal, true);
  assert.deepEqual(reducer.pushBatch([stale]), []);
});

test("exact input_recorded links a relay source before durable completion", () => {
  const reducer = createTranscriptReducer(); reducer.beginTurn(receipt());
  reducer.pushBatch([recorded(1)]);
  const events = reducer.pushBatch([movement(120, "message", {}, { terminal: true })]);
  assert.equal(ended(events).length, 1);
  assert.equal(reducer.currentTurn().threadId, "thread");
  assert.deepEqual(reducer.currentTurn().sourceMovementSeqs, [100, 105]);
});

test("new submission does not stop on prior output, even when it arrives after the new admission", () => {
  const prior = stream(1, "turn_started", { thread_id: "thread" });
  for (const mode of ["warm", "cold", "late"]) {
    const historical = { ...prior, source_movement_seq: undefined, event_data: { ...prior.event_data,
      _stream: { stream_id: STREAM, stream_seq: 1, source_movement_seq: 100 } } };
    const reducer = createTranscriptReducer({ history: mode === "cold" ? [historical] : [] });
    if (mode === "warm") reducer.pushBatch([prior]);
    reducer.beginTurn(receipt({ messageId: "next", messageVerdict: { admitted: { seq: 200 } } }));
    if (mode === "late") reducer.pushBatch([prior]);
    assert.equal(reducer.currentTurn().turnId, "");
    assert.equal(ended(reducer.pushBatch([movement(210, "message", { thread_id: "thread" }, { terminal: true })])).length, 0);
    const input = recorded(2, { source_movement_seq: 100 });
    input.event_data = { ...input.event_data, client_id: "next", source_movement_seq: 100 };
    assert.equal(ended(reducer.pushBatch([{ ...input, event_data: { ...input.event_data, client_id: "other" } }])).length, 0);
    input.stream_seq = 3; input.event_data.seq = 3;
    assert.equal(ended(reducer.pushBatch([input])).length, 1);
    assert.equal(reducer.currentTurn().outcome.movementSeq, 210);
  }
});

test("durable admission frontier completes fresh turns without a provider source link", () => {
  const reducer = createTranscriptReducer(); reducer.beginTurn(receipt());
  reducer.pushBatch([stream(1, "turn_started", {}, { source_movement_seq: 105 })]);
  assert.equal(reducer.currentTurn().turnId, "");
  const completed = ended(reducer.pushBatch([movement(120, "message", {}, { terminal: true })]));
  assert.equal(completed.length, 1);
  assert.equal(completed[0].proof, "durable_movement_after_admission");
  assert.equal(reducer.currentTurn().outcome.movementSeq, 120);
  const failed = createTranscriptReducer(); failed.beginTurn(receipt());
  const events = failed.pushBatch([movement(111, "error", { turn_id: null, additional_details: { source_movement_seq: 100 } }, { terminal: true, error: true, text: "Provider could not start" })]);
  assert.equal(ended(events)[0].state, "failed");
  assert.equal(events[0].type, "error");
  assert.equal(events[0].text, "Provider could not start");
});

test("an exact failed steer ends only its submission in live and restored history", () => {
  const prior = stream(1, "turn_started", { thread_id: "thread" }, { source_movement_seq: 90 });
  const failed = movement(120, "turn_control", { thread_id: "thread", control_id: "message", action: "steer",
    status: "failed", source_movement_seq: 105 }, { label: "turn.control.failed", error: true });
  for (const cold of [false, true]) {
    const reducer = createTranscriptReducer({ history: cold ? [prior, failed] : [prior] });
    let events = reducer.beginTurn(receipt());
    if (!cold) {
      for (const patch of [{ control_id: "other" }, { action: "stop" }, { source_movement_seq: 90 }])
        assert.equal(ended(reducer.pushBatch([{ ...failed, artifact_id: JSON.stringify(patch), event_data: { ...failed.event_data, ...patch } }])).length, 0);
      events = reducer.pushBatch([failed]);
    }
    assert.equal(ended(events)[0].state, "failed"); assert.equal(reducer.currentTurn().outcome.kind, "turn_control");
  }
});

test("malformed native input receipt and conflicting turn identities cannot complete a watch", () => {
  const reducer = createTranscriptReducer(); reducer.beginTurn(receipt());
  reducer.pushBatch([recorded(1, { stream_id: "not-a-stream-hash" })]);
  assert.equal(reducer.currentTurn().turnId, "");
  reducer.pushBatch([stream(2, "turn_started")]);
  reducer.pushBatch([stream(3, "turn_started", { turn_id: "conflict" })]);
  assert.match(reducer.currentTurn().correlationError, /conflicting/);
  assert.equal(ended(reducer.pushBatch([movement(120, "message", {}, { terminal: true })])).length, 0);
});

test("request waiting and stop acknowledgement use the exact current turn", () => {
  const fresh = createTranscriptReducer(); fresh.beginTurn(receipt());
  const freshWaiting = fresh.pushBatch([movement(111, "approval", { request_id: "approve" })]);
  assert.equal(freshWaiting.find((event) => event.type === "status").state, "waiting");
  assert.equal(fresh.currentTurn().terminal, false);
  const reducer = createTranscriptReducer(); reducer.beginTurn(receipt({ turnId: "turn", threadId: "thread" }));
  const waiting = reducer.pushBatch([movement(111, "user_input", { request_id: "question", questions: [{ id: "q", question: "Continue?" }] })]);
  assert.equal(waiting.find((event) => event.type === "status").state, "waiting");
  assert.equal(ended(reducer.pushBatch([movement(112, "turn_control", { turn_id: "old", action: "stop", status: "acknowledged" }, { terminal: true })])).length, 0);
  const stopped = reducer.pushBatch([movement(113, "turn_control", { action: "stop", status: "acknowledged" }, { terminal: true })]);
  assert.equal(ended(stopped)[0].state, "idle");
});

test("Stop pending acknowledgement waits for the exact durable interrupted result", () => {
  const reducer = createTranscriptReducer(); reducer.beginTurn(receipt({ turnId: "turn", threadId: "thread" }));
  const ack = movement(113, "turn_control", { action: "stop", status: "acknowledged", thread_id: "thread", detail: { exact_result_pending: true } }, { terminal: true });
  const waiting = reducer.pushBatch([ack]);
  assert.equal(waiting.find((event) => event.type === "status").state, "stopping");
  assert.equal(reducer.currentTurn().terminal, false);
  assert.equal(normalizeHistory([{ ...ack, id: "ack" }])[0].terminal, false);
  const closure = movement(120, "interrupted_result", { result_kind: "tool_result", correlation_id: "call", thread_id: "thread", ok: true,
    turn_interrupted: true, returned_to_model: false, result_disposition: "durably_admitted_after_interrupt", source_artifact: { id: "result", content: Array(32).fill(10) } }, { terminal: true });
  for (const bad of [{ ...closure, from_interface_id: "stale-provider" }, { ...closure, event_data: { ...closure.event_data, turn_id: "old-turn" } },
    { ...closure, source: "stream", stream_id: STREAM, stream_seq: 4 }, { ...closure, event_data: { ...closure.event_data, returned_to_model: true } },
    movement(119, "message", { thread_id: "thread" }, { terminal: true })]) assert.equal(ended(reducer.pushBatch([bad])).length, 0);
  const settled = ended(reducer.pushBatch([{ ...closure, artifact_id: "closure", movement_seq: 125 }]));
  assert.equal(settled.length, 1);
  assert.equal(settled[0].interrupted, true);
  assert.equal(reducer.currentTurn().outcome.kind, "interrupted_result");
  const completed = createTranscriptReducer(); completed.beginTurn(receipt({ turnId: "turn", threadId: "thread" })); completed.pushBatch([ack]);
  const finished = { ...closure, event_data: { ...closure.event_data, ok: false, turn_interrupted: false,
    native_terminal_status: "completed", result_disposition: "durably_admitted_after_native_terminal" } };
  const completedEvents = ended(completed.pushBatch([finished]));
  assert.equal(completedEvents.length, 1); assert.equal(completedEvents[0].interrupted, false); assert.equal(completedEvents[0].state, "idle");
});

test("invalid receipts/batches fail explicitly and snapshots cannot mutate adapter state", () => {
  const reducer = createTranscriptReducer();
  assert.throws(() => createTranscriptReducer({ runId: "run", afterSeq: 100, messageId: "message" }), /beginTurn/);
  assert.throws(() => reducer.beginTurn({ runId: "run" }), /chatInterfaceId/);
  assert.throws(() => reducer.beginTurn(receipt({ messageVerdict: { refused: { reason: "no" } } })), /admitted/);
  assert.throws(() => reducer.pushBatch({}), /artifacts array/);
  reducer.pushBatch([stream(1, "message_delta", { text: "safe" })]);
  const snapshot = reducer.snapshot(); snapshot[0].text = "changed"; snapshot[0].metadata.runId = "changed";
  assert.equal(reducer.snapshot()[0].text, "safe");
  assert.equal(reducer.snapshot()[0].metadata.runId, "run");
});

test("empty native command snapshot does not hide live output", () => {
  const reducer = createTranscriptReducer();
  reducer.pushBatch([stream(1, "tool", { command: "cat", status: "in_progress", aggregated_output: "" }),
    stream(2, "tool_output_delta", { delta: "actual output" })]);
  assert.equal(reducer.snapshot()[0].activity.output, "actual output");
});

test("response kind disambiguates request IDs and restored answers stay resolved", () => {
  const reducer = createTranscriptReducer();
  reducer.pushBatch([movement(110, "approval", { request_id: "same" }), movement(111, "user_input", { request_id: "same" }),
    movement(112, "event", { operator_approval_response: { request_id: "same", decision: "decline" } }, { role: "user" })]);
  assert.equal(reducer.snapshot()[0].request.status, "resolved");
  assert.equal(reducer.snapshot()[1].request.status, "pending");
  const [restored] = normalizeHistory([{ ...movement(113, "user_input", { request_id: "answered", resolved: true,
    response: { request_id: "answered", answers: { q: ["selected"] } } }), id: "history-request" }]);
  assert.deepEqual(restored.request.response.answers, { q: ["selected"] });
  assert.equal(restored.request.actionable, false);
});

test("file changes and attached artifact coordinates remain inspectable structured data", () => {
  const [entry] = normalizeHistory([{ ...movement(111, "file_change", { changes: [{ path: "a.txt", kind: "update", diff: "+actual change\n" }],
    attachments: [{ payload_content_hash: "content", name: "a.txt", mime_type: "text/plain", size: 5, label: "operator.file" }],
    _payload_projection: { truncated: true, original_bytes: 4096, contains_unified_diff: true } }), id: "file-change" }]);
  assert.equal(entry.activity.details.changes[0].diff, "+actual change\n");
  assert.equal(entry.metadata.artifactId, "artifact-111");
  assert.equal(entry.metadata.truncated, true);
  assert.equal(entry.attachments[0].id, "content");
  assert.equal(entry.attachments[0].type, "text/plain");
});

test("MCP text/resources retain useful result data without dumping the entire result as JSON", () => {
  const entry = renderMessageData({ ...movement(110, "mcp_tool", { status: "completed", invocation: { tool: "read", arguments: { uri: "file:///result" } },
    result: { content: [{ type: "text", text: "Actual result\n" }, { type: "resource", resource: { uri: "file:///result", mimeType: "text/plain", text: "file text" } }] } }), id: "mcp" });
  assert.equal(entry.activity.tool, "read");
  assert.equal(entry.activity.output, "Actual result\nfile text");
  assert.deepEqual(entry.activity.arguments, { uri: "file:///result" });
  assert.equal(entry.activity.resources[0].uri, "file:///result");
  assert.equal(typeof entry.activity.result, "object");
});
