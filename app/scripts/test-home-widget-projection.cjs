const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const ts = require("typescript");
const source = ts.createSourceFile("projection.cjs", fs.readFileSync(path.join(__dirname, "../electron/projection.cjs"), "utf8"), ts.ScriptTarget.Latest, true);
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
