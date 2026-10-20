const assert = require("node:assert/strict");
const path = require("node:path");

const appRoot = path.resolve(__dirname, "..");

function source(relativePath) {
  return require("./splitSource.cjs").readSource(path.join(appRoot, relativePath));
}


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
