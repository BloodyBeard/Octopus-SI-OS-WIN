const assert = require("node:assert/strict");
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

const sidebar = source("src/components/Sidebar.tsx");
const messageList = source("src/components/chat/ChatMessageList.tsx");
const foundation = source("src/foundation.css");
const ownedUtilities = source("src/owned-utilities.css");
const queueStyles = source("src/components/chat/queue.css");
assert.match(queueStyles, /\.composer-primary-action \{\s*background-color: var\(--color-background-composer-primary\);\s*color: var\(--color-text-composer-primary\);/);
assert.match(queueStyles, /\.composer-primary-action:focus-visible \{ outline-color: var\(--color-background-composer-primary\);/);
assert.match(queueStyles, /\.composer-queue-action \{[^}]*border-radius: 9999px;/);
assert.match(queueStyles, /\.composer-queue-action\.is-icon \{[^}]*border-radius: var\(--radius-lg\);/);
assert.match(queueStyles, /\.oc-menu-item\.composer-queue-menu-item \{ color: var\(--color-text-description\);/);
assert.match(sidebar, /onClick=\{\(\) => setAgentDialog\(\{ mode: "edit", presetId: preset\.id \}\)\}/);
assert.doesNotMatch(sidebar, /mode: "view"|preset\.origin/);
assert.doesNotMatch(sidebar, /onNewChat\(preset\.id\)/);
assert.match(sidebar, /projectById\.has\(preset\.projectEeId\)/);
assert.match(sidebar, /meta=\{agentTaskMeta\(preset, taskChats\)\}/);
assert.doesNotMatch(sidebar, /function agentPresetMeta/);
assert.match(sidebar, /\? \[run\.runId\]/);
assert.match(ownedUtilities, /--color-token-warning: var\(--color-text-warning\);/);
assert.doesNotMatch(sidebar, /deleteClientTaskAgentAssignment|createClientAgentPreset|updateClientAgentPreset|deleteClientAgentPreset/);
assert.doesNotMatch(messageList + sourceBetween(source("src/components/chat/chat.css"), ".octopus-chat-scroll {", "\n}"), /thread-scroll-container relative h-full|position:\s*relative/);
assert.doesNotMatch(foundation, /\*\s*\{\s*scroll-behavior:\s*smooth;/);

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
  process.stdout.write("chat composer source contracts and template file binding passed\n");
}).catch((error) => { console.error(error); process.exitCode = 1; });
