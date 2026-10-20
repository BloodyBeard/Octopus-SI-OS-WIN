const assert = require("node:assert/strict");
const path = require("node:path");
const { test } = require("node:test");
const { buildSync } = require("esbuild");

const result = buildSync({
  stdin: {
    contents: 'export { resolveClientAgents, visibleClientAgents } from "./src/agentPresets"; export { projectChatExecutionContexts } from "./src/chatRuns";',
    resolveDir: path.join(__dirname, ".."), loader: "ts",
  },
  bundle: true, platform: "node", format: "cjs", write: false, logLevel: "silent",
  define: { "import.meta.env.DEV": "false" },
});
const bundle = { exports: {} };
Function("require", "module", "exports", result.outputFiles[0].text)(require, bundle, bundle.exports);
const { resolveClientAgents, visibleClientAgents, projectChatExecutionContexts } = bundle.exports;

const worker = (id, agent = {}) => ({
  id, label: id, provider_interface_ids: ["provider.fixture"],
  ...(agent.id ? {
    client_agent_id: agent.id, client_agent_name: agent.name ?? "Writer", client_agent_icon: agent.icon ?? "default",
    client_agent_created_at_ms: agent.createdAtMs ?? 10, client_agent_updated_at_ms: 20, client_agent_deleted_at_ms: agent.deletedAtMs ?? 0,
    agent_source_template: true, client_template_digest: agent.digest ?? "digest-1",
    client_template_provider_interface_id: "provider.fixture", client_template_provider_model: "model-a",
    client_template_reasoning_effort: "medium", client_template_service_tier: "priority",
    client_template_application_ids: ["app.docs"], client_template_worker_environment_kinds: ["code_mode"],
  } : {}),
});
const contexts = (rows) => projectChatExecutionContexts({ contexts: rows });
const context = (eeId, workers, digest = `catalog-${eeId}`) => ({ ee_id: eeId, agent_catalog_digest: digest, workers });

test("agents come only from workers the projection names as client agents, with their template", () => {
  const agents = resolveClientAgents(contexts([context("ee.project", [worker("worker.plain"), worker("worker.writer", { id: "agent-writer" })])]));
  assert.deepEqual(agents, [{
    id: "agent-writer", name: "Writer", icon: "default", projectEeId: "ee.project", workerInterfaceId: "worker.writer",
    createdAtMs: 10, updatedAtMs: 20, deletedAtMs: 0, agentCatalogDigest: "catalog-ee.project", sourceTemplate: true, clientTemplateDigest: "digest-1",
    providerInterfaceId: "provider.fixture", providerModel: "model-a", reasoningEffort: "medium", serviceTier: "priority",
    applicationIds: ["app.docs"], driverNames: ["code_mode"],
  }]);
});

test("one agent seen in two places must agree on its identity", () => {
  const same = context("ee.project", [worker("worker.writer", { id: "agent-writer" })]);
  assert.equal(resolveClientAgents(contexts([same, same])).length, 1);
  assert.throws(() => resolveClientAgents(contexts([same, context("ee.other", [worker("worker.writer", { id: "agent-writer" })])])), /conflicting identities/);
  assert.throws(() => resolveClientAgents(contexts([context("ee.project", [worker("worker.writer", { id: "agent-writer" })], "")])), /incomplete projected identity/);
  assert.throws(() => resolveClientAgents(contexts([context("ee.project", [worker("worker.writer", { id: "agent-writer", icon: "sparkles-unknown" })])])), /icon 'sparkles-unknown' is not declared/);
});

test("deleted or template-less agents stay out of the picker, which orders agents by creation", () => {
  const agents = resolveClientAgents(contexts([context("ee.project", [
    worker("worker.late", { id: "agent-late", name: "Late", createdAtMs: 30 }),
    worker("worker.b", { id: "agent-b", name: "Beta", createdAtMs: 10 }),
    worker("worker.a", { id: "agent-a", name: "Alpha", createdAtMs: 10 }),
    worker("worker.gone", { id: "agent-gone", name: "Gone", deletedAtMs: 40 }),
    worker("worker.draft", { id: "agent-draft", name: "Draft", digest: "" }),
  ])]));
  assert.deepEqual(agents.map((agent) => agent.id), ["agent-a", "agent-b", "agent-draft", "agent-gone", "agent-late"]);
  assert.deepEqual(visibleClientAgents(agents).map((agent) => agent.id), ["agent-a", "agent-b", "agent-late"]);
});
