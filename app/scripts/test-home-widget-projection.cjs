const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const ts = require("typescript");
const source = ts.createSourceFile("projection.cjs", require("./projectionSource.cjs").projectionSource(), ts.ScriptTarget.Latest, true);
function extract(name) {
  let found;
  function visit(node) { if (ts.isFunctionDeclaration(node) && node.name?.text === name) found = node.getText(source); ts.forEachChild(node, visit); }
  visit(source); assert.ok(found, name); return found;
}
function project(rows, indexed) {
  const environment = {
    crypto: require("node:crypto"), Buffer, projectionGeneration: 7,
    indexedArtifactLabelRows: async () => indexed ? rows : null,
    refreshLedgerProjection: async () => {},
    resolveLedgerArtifactPayload: async (row) => { if (row.unreadable) throw new Error("missing payload"); return row.payload; },
  };
  const code = ["objectOrEmpty", "stringValue", "movementKindText", "movementBaseKind", "movementRunId", "ledgerArtifactId", "ledgerArtifactLabel", "isAdmittedInterfaceArtifactMovement", "numberValue", "timestampOf", "isClientSurfaceDocumentPayload", "clientSurfaceDocumentSnapshot"].map(extract).join("\n");
  return Function(...Object.keys(environment), `${code}; return clientSurfaceDocumentSnapshot;`)(...Object.values(environment))({ ledgerRows: rows });
}
function row(seq, run, recipient, patch = {}) {
  return { seq, kind: "InterfaceOutbound", artifact: { id: "client.surface.document:same-content" }, from: { Interface: "provider" }, to: { Interface: recipient }, invocation_metadata: { runtime: { run, interface: "provider" } }, recorded_at_unix_ms: 1000 + seq, payload: { schema: "octopus.client_surface", schema_version: 1, id: "home.test", runId: "untrusted-run", interfaceId: "untrusted-interface" }, ...patch };
}
test("indexed and ledger projection preserve every exact recipient delivery and host provenance", async () => {
  const rows = [row(1, "home", "home-client"), row(2, "task", "task-client"), row(3, "home", "other-client"), row(4, "home", "home-client")];
  for (const indexed of [false, true]) {
    const result = await project(rows, indexed);
    assert.deepEqual(result.errors, []);
    assert.deepEqual(result.documents.map((item) => [item.runId, item.interfaceId, item.fromInterfaceId, item.sequence]), [["home", "home-client", "provider", 1], ["task", "task-client", "provider", 2], ["home", "other-client", "provider", 3], ["home", "home-client", "provider", 4]]);
    assert.equal(new Set(result.documents.map((item) => item.digest)).size, 1);
    assert.equal(result.documents[0].modifiedAt, new Date(1001).toISOString());
  }
});
test("rejected movements, missing recipients and unreadable revisions cannot produce valid surface documents", async () => {
  const result = await project([row(1, "home", "home-client", { kind: "InterfaceOutboundRefused" }), row(2, "home", "home-client", { to: {} }), row(3, "home", "home-client", { unreadable: true }), row(4, "home", "home-client", { payload: {} }), row(5, "", "home-client")], true);
  assert.equal(result.documents.length, 1);
  assert.equal(result.documents[0].sequence, 3);
  assert.equal(result.documents[0].text, "");
  assert.equal(result.documents[0].bytes, 0);
  assert.equal(result.errors.length, 3);
});

if (process.argv.includes("--publication-fixture")) test("an actually admitted Home publication passes client validation and native rendering", async () => {
  const { rows, scope, summary } = JSON.parse(fs.readFileSync(0, "utf8"));
  const snapshot = await project(rows, true);
  assert.deepEqual(snapshot.errors, []);
  const appRoot = path.resolve(__dirname, "..");
  const compiled = require("esbuild").buildSync({
    stdin: { contents: `export { materializeRegistry } from "./src/client-surfaces/documentRegistry";
      export { selectHomeSurfaceDocuments } from "./src/client-surfaces/homePresentation";
      export { ClientSurfaceRenderer } from "./src/client-surfaces/ClientSurfaceRenderer";`, resolveDir: appRoot, loader: "ts" },
    bundle: true, write: false, platform: "node", format: "cjs", loader: { ".css": "empty" },
    external: ["react", "react-dom", "motion/react"], define: { "import.meta.env.DEV": "false" },
  }).outputFiles[0].text;
  const owner = { exports: {} };
  Function("module", "exports", "require", compiled)(owner, owner.exports, require);
  const registry = owner.exports.materializeRegistry(snapshot, "", false, "live");
  assert.deepEqual(registry.errors, []);
  assert.ok(registry.verdicts.length && registry.verdicts.every(verdict => verdict.passed));
  const selected = owner.exports.selectHomeSurfaceDocuments(registry.documents, scope, Date.now());
  assert.deepEqual(selected.errors, []); assert.equal(selected.documents.length, 1);
  const admitted = selected.documents[0];
  assert.equal(admitted.document.glyph, "weather");
  assert.equal(admitted.document.render.density, "comfortable");
  const html = require("react-dom/server").renderToStaticMarkup(require("react").createElement(owner.exports.ClientSurfaceRenderer, { accessProfile: "live", admitted }));
  assert.ok(html.includes(summary), html);
  assert.ok(html.includes(`data-surface-interface="${scope.interfaceId}"`));
  assert.ok(html.includes(`data-surface-artifact="${admitted.sourceArtifactId}"`));
});
