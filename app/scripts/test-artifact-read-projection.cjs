const assert = require("node:assert/strict");
const { test } = require("node:test");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { blake3 } = require("@noble/hashes/blake3");
const { BoundedJsonProjection, parseArtifactReadProjection, artifactReadProjectionForLabel } = require("../electron/boundedJsonProjection.cjs");
const { readContentAddressedArtifactProjection, readContentAddressedArtifactPayload } = require("../electron/contentAddressedArtifact.cjs");

const contract = { schema_version: 1, json_object_labels: ["reply", "interface.artifact.output"], excluded_root_fields: ["private"], max_source_bytes: 2 * 1024 * 1024, max_projected_bytes: 64 * 1024 };
function project(input, size = 7, policy = contract) {
  const bytes = Buffer.isBuffer(input) ? input : Buffer.from(input);
  const parser = new BoundedJsonProjection(policy);
  for (let start = 0; start < bytes.length; start += size) parser.write(bytes.subarray(start, start + size));
  return parser.finish();
}

test("projection validates JSON across every byte boundary and preserves all public values", () => {
  const cases = [
    '{}', '{"private":{}}', '{"private":[null,true,false,-12.345e+67],"text":"A 🌊 Ї Z","data":{"private":"keep","n":-0}}',
    ' { "text" : "a\\n\\uD800\\uDC00\\/\\t\\b\\f\\r\\\\\\\"", "pr\\u0069vate" : {"other":[{},[],0,1e2,2E-3]}, "ok":true } ',
    '{"private":"' + "x".repeat(20000) + '","data":[1,2,3],"last":null}',
    '{"zero":0,"fraction":0.5,"exponent":-0e+10,"private":{"duplicate":1,"duplicate":2},"__proto__":{"safe":true}}',
  ];
  for (const input of cases) {
    const expected = JSON.parse(input); delete expected.private;
    for (const size of [1, 2, 3, 5, 7, 31, 1024, 65536]) {
      const result = project(input, size);
      assert.deepEqual(result.payload, expected);
      assert.equal(result.originalBytes, Buffer.byteLength(input));
      assert.equal(result.projectedBytes.length <= contract.max_projected_bytes, true);
    }
  }
});

test("malformed excluded data, duplicate root keys, invalid UTF-8 and exhausted bounds refuse", () => {
  const invalid = [
    '', '[]', 'null', '{', '{"private":}', '{"private":{"a":1,}}', '{"private":[1,]}',
    '{"private":01}', '{"private":-}', '{"private":1.}', '{"private":1e}', '{"private":1e+}',
    '{"private":tru}', '{"private":truefalse}', '{"private":"\\q"}', '{"private":"\\u0X00"}',
    '{"private":"\n"}', '{"private":{a:1}}', '{"private":[1 2]}', '{"private" 1}',
    '{"a":1,"\\u0061":2}', '{"private":{},"private":[]}', '{"private":{} } []',
    '{"private":[}', '{"private":{"a":[]]', '{"private":"unterminated}', '\uFEFF{}',
  ];
  for (const input of invalid) for (const size of [1, 7, 65536]) {
    assert.throws(() => project(input, size), /malformed|invalid UTF-8/, input);
  }
  for (const invalidUtf8 of [[0xc0, 0x80], [0xed, 0xa0, 0x80], [0xf4, 0x90, 0x80, 0x80], [0xe2], [0x80]]) {
    const bytes = Buffer.concat([Buffer.from('{"private":"'), Buffer.from(invalidUtf8), Buffer.from('"}')]);
    assert.throws(() => project(bytes, 1), /invalid UTF-8/);
  }
  assert.throws(() => project('{"private":[{}]}', 1, { ...contract, max_source_bytes: 2 }), /source byte bound/);
  assert.throws(() => project('{"text":"abcd"}', 1, { ...contract, max_projected_bytes: 10 }), /projected byte bound/);
  assert.throws(() => project('{"private":' + '['.repeat(512) + '0' + ']'.repeat(512) + '}'), /nesting/);
  assert.throws(() => project('{"' + 'k'.repeat(65537) + '":1}'), /member name/);
  for (const invalidContract of [null, {}, { ...contract, extra: true }, { ...contract, max_source_bytes: 0 },
    { ...contract, max_projected_bytes: Infinity }, { ...contract, excluded_root_fields: ["private", "private"] },
    { ...contract, json_object_labels: undefined }, { ...contract, json_object_labels: ["reply", "reply"] }]) {
    assert.throws(() => parseArtifactReadProjection(invalidContract), /projection/i);
  }
});

test("explicit JSON selection preserves raw text, binary, arrays and scalars without a parser fallback", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "oct-raw-carriers-"));
  const hash = bytes => Buffer.from(blake3(bytes)).toString("hex");
  const read = input => artifactReadProjectionForLabel(contract, input.label)
    ? readContentAddressedArtifactProjection({ ...input, projection: contract }) : readContentAddressedArtifactPayload(input);
  try {
    for (const [label, bytes, expected] of [["note", Buffer.from("plain text"), { __raw: "plain text" }],
      ["array", Buffer.from("[1,2]"), [1, 2]], ["scalar", Buffer.from("3"), 3], ["binary", Buffer.from([255, 0, 1]), null],
      ["reply", Buffer.from('{"private":"discard","text":"Reply"}'), { text: "Reply" }], ["reply", Buffer.from("[1,2]"), undefined]]) {
      const payloadHash = hash(bytes), manifest = Buffer.from(JSON.stringify({ label, payload_path: path.join(root, payloadHash), payload_content_hash: payloadHash }));
      const manifestHash = hash(manifest);
      await fs.writeFile(path.join(root, payloadHash), bytes); await fs.writeFile(path.join(root, manifestHash), manifest);
      const input = { blobsRoot: root, label, manifestHash, artifactId: `${label}:${manifestHash}` };
      if (expected === undefined) await assert.rejects(read(input), /root must be an object/);
      else {
        const result = await read(input);
        if (expected === null) assert.equal(result.payload.__binary, true); else assert.deepEqual(result.payload, expected);
        assert.equal(Object.hasOwn(result, "payloadBytes"), label !== "reply");
      }
    }
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});

test("content addressing covers private bytes and projected reads never impersonate raw bytes", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "oct-artifact-projection-"));
  const bytes = Buffer.from('{"private":{"secret":"unseen"},"text":"Complete reply","will_retry":false}');
  const hash = bytes => Buffer.from(blake3(bytes)).toString("hex");
  const payloadHash = hash(bytes), label = "interface.artifact.output";
  const payloadPath = path.join(root, payloadHash);
  const manifest = Buffer.from(JSON.stringify({ label, payload_path: payloadPath, payload_content_hash: payloadHash }));
  const manifestHash = hash(manifest);
  const input = { blobsRoot: root, manifestHash, artifactId: `${label}:${manifestHash}`, label, projection: contract };
  try {
    await fs.writeFile(payloadPath, bytes); await fs.writeFile(path.join(root, manifestHash), manifest);
    const projected = await readContentAddressedArtifactProjection(input);
    assert.deepEqual(projected.payload, { text: "Complete reply", will_retry: false });
    assert.equal(projected.payloadHash, payloadHash); assert.equal(projected.manifestHash, manifestHash);
    assert.equal(Object.hasOwn(projected, "payloadBytes"), false);
    assert.equal(projected.projection.original_bytes, bytes.length);
    const raw = await readContentAddressedArtifactPayload(input);
    assert.deepEqual(raw.payloadBytes, bytes); assert.equal(raw.payload.private.secret, "unseen");
    await fs.writeFile(payloadPath, Buffer.from(bytes.toString().replace("unseen", "broken")));
    await assert.rejects(readContentAddressedArtifactProjection(input), /content-addressed digest/);
    await fs.writeFile(payloadPath, bytes);
    await assert.rejects(readContentAddressedArtifactProjection({ ...input, artifactId: "wrong" }), /artifact identity/);
    await assert.rejects(readContentAddressedArtifactProjection({ ...input, projection: { ...contract, max_source_bytes: 1 } }), /source byte bound/);
    await assert.rejects(readContentAddressedArtifactProjection({ ...input, maxPayloadBytes: 1 }), /source byte bound/);
    await assert.rejects(readContentAddressedArtifactProjection({ ...input, maxPayloadBytes: 0 }), /finite positive/);
    await assert.rejects(readContentAddressedArtifactProjection({ ...input, maxManifestBytes: 1 }), /manifest exceeds/);
    const manifestPath = path.join(root, manifestHash), target = path.join(root, "manifest-copy");
    await fs.writeFile(target, manifest);
    await fs.unlink(manifestPath); await fs.symlink(target, manifestPath);
    await assert.rejects(readContentAddressedArtifactProjection(input), /regular file/);
    await fs.unlink(manifestPath); await fs.writeFile(manifestPath, manifest);
    const originalOpen = fs.open;
    let replaced = false;
    fs.open = async (file, ...args) => {
      if (file === manifestPath && !replaced) {
        replaced = true; await fs.unlink(manifestPath); await fs.symlink(target, manifestPath);
      }
      return originalOpen(file, ...args);
    };
    try { await assert.rejects(readContentAddressedArtifactPayload(input), { code: "ELOOP" }); }
    finally { fs.open = originalOpen; }
    assert.equal(replaced, true);
    await fs.unlink(manifestPath); await fs.writeFile(manifestPath, manifest);
    fs.open = async (file, ...args) => {
      if (file === manifestPath) await fs.appendFile(manifestPath, Buffer.alloc(64));
      return originalOpen(file, ...args);
    };
    try { await assert.rejects(readContentAddressedArtifactProjection({ ...input, maxManifestBytes: manifest.length }), /changed before/); }
    finally { fs.open = originalOpen; }
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});

test("generated large private data crosses the explicit projection boundary while raw stays capped", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "oct-large-projection-")), temporary = path.join(root, "generated");
  const handle = await fs.open(temporary, "wx"), hash = blake3.create();
  const write = async bytes => { await handle.writeFile(bytes); hash.update(bytes); };
  try {
    await write(Buffer.from('{"private":"'));
    const chunk = Buffer.alloc(64 * 1024, 0x78);
    for (let count = 0; count < 640; count++) await write(chunk);
    await write(Buffer.from('","text":"Control reply"}')); await handle.close();
    const payloadHash = Buffer.from(hash.digest()).toString("hex"), payloadPath = path.join(root, payloadHash);
    await fs.rename(temporary, payloadPath);
    const label = "reply", manifest = Buffer.from(JSON.stringify({ label, payload_path: payloadPath, payload_content_hash: payloadHash }));
    const manifestHash = Buffer.from(blake3(manifest)).toString("hex");
    await fs.writeFile(path.join(root, manifestHash), manifest);
    const input = { blobsRoot: root, label, manifestHash, artifactId: `${label}:${manifestHash}`,
      projection: { ...contract, max_source_bytes: 48 * 1024 * 1024, max_projected_bytes: 1024 } };
    await assert.rejects(readContentAddressedArtifactPayload(input), /41943040/);
    const result = await readContentAddressedArtifactProjection(input);
    assert.deepEqual(result.payload, { text: "Control reply" });
    assert.equal(result.projection.original_bytes > 41943040, true); assert.equal(result.projection.projected_bytes < 1024, true);
  } finally { await handle.close(); await fs.rm(root, { recursive: true, force: true }); }
});
