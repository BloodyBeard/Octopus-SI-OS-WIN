const assert = require("node:assert/strict");
const path = require("node:path");
const { test } = require("node:test");
const { buildSync } = require("esbuild");

const result = buildSync({ entryPoints: [path.join(__dirname, "../src/components/chat/subagentActivity.ts")], bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent" });
const bundle = { exports: {} };
Function("require", "module", "exports", result.outputFiles[0].text)(require, bundle, bundle.exports);
const { normalizeMultiAgentActivity, normalizeMultiAgentState, normalizeSubagentActivity, multiAgentIds, multiAgentStateIsTerminal, multiAgentStateStatus } = bundle.exports;
const message = (id, eventKind, eventData) => ({ id, role: "assistant", text: "", createdAtMs: 1, eventKind, eventData });

test("collaboration calls read the same in either field spelling", () => {
  const camel = message("m1", "collaboration", { tool: "spawnAgent", status: "inProgress", model: "model-a", prompt: "Check totals",
    receiverThreadIds: ["thread-b", "thread-a"], agentsStates: { "thread-c": { status: "running" } } });
  const snake = message("m1", "collaboration", { tool: "spawnAgent", status: "inProgress", model: "model-a", prompt: "Check totals",
    receiver_thread_ids: ["thread-b", "thread-a"], agents_states: { "thread-c": { status: "running" } } });
  assert.deepEqual(normalizeMultiAgentActivity(snake), normalizeMultiAgentActivity(camel));
  assert.deepEqual(normalizeMultiAgentActivity(camel), { action: "spawnAgent", agentsStates: { "thread-c": { status: "running" } }, id: "m1", model: "model-a",
    prompt: "Check totals", receiverThreadIds: ["thread-b", "thread-a"], status: "inProgress" });
  assert.deepEqual(multiAgentIds(normalizeMultiAgentState(camel)), ["thread-a", "thread-b", "thread-c"]);
});

test("waiting on agents carries state but is not an agent action", () => {
  const wait = message("w1", "collaboration", { tool: "wait", agents_states: { "thread-a": { status: "completed" } } });
  assert.deepEqual(normalizeMultiAgentState(wait), { agentsStates: { "thread-a": { status: "completed" } }, id: "w1", receiverThreadIds: [] });
  assert.equal(normalizeMultiAgentActivity(wait), null);
  assert.equal(normalizeMultiAgentState(message("x1", "collaboration", { tool: "shell" })), null);
  assert.equal(normalizeMultiAgentState(message("x2", "dynamic_tool", { tool: "spawnAgent" })), null);
});

test("subagent activity names the agent from its path and maps its kind", () => {
  assert.deepEqual(normalizeSubagentActivity(message("s1", "subagent_activity", { agent_thread_id: "thread-a", agent_path: "root/data_checker", kind: "started" })),
    { agentThreadId: "thread-a", displayName: "Data checker", displayStatus: "active", id: "s1" });
  assert.deepEqual(normalizeSubagentActivity(message("s2", "subagent_activity", { agentThreadId: "thread-b", agentPath: "root", kind: "interacted" })),
    { agentThreadId: "thread-b", displayName: "Agent", displayStatus: "updated", id: "s2" });
  assert.equal(normalizeSubagentActivity(message("s3", "subagent_activity", { agent_thread_id: "thread-c", kind: "finished" })), null);
  assert.equal(normalizeSubagentActivity(message("s4", "subagent_activity", { kind: "started" })), null);
});

test("only settled agent states are terminal", () => {
  assert.deepEqual(["pendingInit", "running", "interrupted", "shutdown", "completed", "errored", "notFound", "paused"].map((status) => multiAgentStateIsTerminal(multiAgentStateStatus(status))),
    [false, false, true, true, true, true, true, false]);
});
