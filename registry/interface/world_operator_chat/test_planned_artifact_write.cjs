const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { createRequire } = require("node:module");
const test = require("node:test");
const vm = require("node:vm");

const filename = path.join(__dirname, "client.cjs"), loaded = { exports: {} };
vm.runInNewContext(`${fs.readFileSync(filename, "utf8")}\nmodule.exports = { plannedClientWrite, plannedArtifactOwner, safeSegment };`, {
  Buffer, __dirname, __filename: filename, clearTimeout, console, module: loaded, process, require: createRequire(filename), setTimeout,
});
const { plannedClientWrite, plannedArtifactOwner, safeSegment } = loaded.exports;
const registry = path.resolve(__dirname, "../../..");
const topologyOwner = "artifact-code-mode-topology__system_code_mode__project_preflight_66cb898c4b507fb5de287d0d217df62d";
const write = (id) => ({ path: path.join(registry, "artifact", id, "artifact.json"), content: '{"schema_version":1}', tag: `artifact:${id}`, expected_before_hash: null });

test("planned Code Mode artifact keeps its exact 97-character owner and binding tag", () => {
  assert.equal(topologyOwner.length, 97);
  assert.match(topologyOwner, /^[A-Za-z0-9_-]+$/);
  const planned = write(topologyOwner), actual = plannedClientWrite(planned, 0, registry);
  assert.equal(actual.filePath, planned.path);
  assert.equal(actual.tag, planned.tag);
  assert.equal(actual.content, planned.content);
  assert.equal(actual.expectedBeforeHash, null);
  assert.throws(() => safeSegment(topologyOwner, "run id"), /letters, digits/);
  assert.throws(() => safeSegment("artifact.context", "run id"), /letters, digits/);
});

test("artifact owners follow the canonical context-artifact component contract without normalization", () => {
  for (const id of ["artifact.context:reference", "artifact-café", `artifact-${"a".repeat(192)}`]) {
    assert.equal(plannedArtifactOwner(id), id);
    assert.equal(plannedClientWrite(write(id), 0, registry).tag, `artifact:${id}`);
  }
  for (const id of ["", ".", "..", " leading", "trailing ", "with/slash", "with\\slash", "with\0null", null]) {
    assert.throws(() => plannedArtifactOwner(id), /planned artifact owner/);
  }
});

test("planned artifact writes still reject changed tags, nonobjects, and escaped or noncanonical files", () => {
  const planned = write(topologyOwner);
  assert.throws(() => plannedClientWrite({ ...planned, tag: "artifact:another-owner" }, 0, registry), /identity differs/);
  assert.throws(() => plannedClientWrite({ ...planned, content: "[]" }, 0, registry), /must be an object/);
  assert.throws(() => plannedClientWrite({ ...planned, path: path.join(registry, "artifact", "payload.json") }, 0, registry), /escapes/);
  assert.throws(() => plannedClientWrite({ ...planned, path: path.resolve(registry, "..", "artifact.json") }, 0, registry), /escapes/);
  assert.throws(() => plannedClientWrite({ ...planned, expected_before_hash: "wrong" }, 0, registry), /SHA-256/);
});
