const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const { createRequire } = require("node:module");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");
const { createStoragePublisher, publishClientWrites } = require("./publication.cjs");

const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-client-publication-"));
test.after(() => fs.rmSync(root, { recursive: true, force: true }));
const hash = (text) => crypto.createHash("sha256").update(text).digest("hex");
const write = (name, before = null) => ({ filePath: path.join(root, name), tag: name,
  content: `content:${name}:café`, expectedBeforeHash: before });
const applied = (request) => ({ digest_algorithm: "sha256", operation_id: request.operation_id,
  path: request.path, before_hash: request.expected_before_hash, after_hash: request.content === null ? null : hash(request.content),
  bytes: request.content === null ? 0 : Buffer.byteLength(request.content), outcome: "applied", error: null });

test("ordered native receipts preserve client response shape and UTF-8 byte evidence", async () => {
  const writes = [write("new/first"), write("second", hash("old"))];
  const result = await publishClientWrites(writes, async ({ writes: requests }) => {
    assert.deepEqual(requests.map((row) => row.content), writes.map((row) => row.content));
    assert.notEqual(requests[0].operation_id, requests[1].operation_id);
    assert.equal(requests[0].path, path.join(fs.realpathSync(root), "new/first"));
    return { receipts: requests.map(applied), error: null };
  });
  assert.deepEqual(result, writes.map((row) => ({ path: row.filePath, tag: row.tag,
    beforeHash: row.expectedBeforeHash, afterHash: hash(row.content), bytes: Buffer.byteLength(row.content), changed: true })));
  assert.equal(fs.existsSync(path.join(root, "new")), false);
  const removal = { ...write("removed", hash("prior")), content: null };
  assert.deepEqual(await publishClientWrites([removal], async ({ writes }) => ({ receipts: writes.map(applied), error: null })),
    [{ path: removal.filePath, tag: removal.tag, beforeHash: removal.expectedBeforeHash, afterHash: null, bytes: 0, changed: true }]);
});

test("refusal, uncertainty and I/O failure retain exact native prefixes without byte-only success", async () => {
  for (const outcome of ["not_applied", "uncertain", "io_error"]) {
    let receipts;
    await assert.rejects(publishClientWrites([write("a"), write("b"), write("c")], async ({ writes }) => {
      receipts = [applied(writes[0])];
      if (outcome !== "io_error") receipts.push({ ...applied(writes[1]), outcome, error: "predecessor changed" });
      return { receipts, error: outcome === "io_error" ? "storage failed" : null };
    }), (error) => {
      assert.equal(error.code, "client_publication_failed");
      assert.deepEqual(error.publicationReceipts, receipts);
      assert.equal(error.publicationRequests.length, 3);
      assert.equal(Object.hasOwn(error.publicationRequests[0], "content"), false);
      return true;
    });
  }
});

test("every outcome correlates its exact submitted request and retains only the validated prefix", async () => {
  for (const outcome of ["applied", "not_applied", "uncertain"]) {
    for (const change of [{ operation_id: "other" }, { path: "/other" }, { digest_algorithm: "blake3" },
      { before_hash: hash("other") }, { before_hash: null }, { after_hash: hash("other") }, { after_hash: null }, { bytes: 0 }]) {
      let prefix;
      await assert.rejects(publishClientWrites([write("a"), write("b", hash("old"))], async ({ writes }) => {
        prefix = [applied(writes[0])];
        return { receipts: [...prefix, { ...applied(writes[1]), outcome,
          error: outcome === "applied" ? null : "refused", ...change }], error: null };
      }), (error) => {
        assert.match(error.message, /receipt/);
        assert.deepEqual(error.publicationReceipts, prefix);
        return true;
      });
    }
  }
  await assert.rejects(publishClientWrites([write("a")], async () => ({ receipts: [], error: null })), /omitted/);
});

test("invalid batches never reach native publication", async () => {
  const unreachable = async () => { assert.fail("invalid batch was published"); };
  await assert.rejects(publishClientWrites([write("a", "bad")], unreachable), /SHA-256/);
  await assert.rejects(publishClientWrites([{ ...write("remove"), content: null }], unreachable), /nonnull/);
  await assert.rejects(publishClientWrites(Array(1025).fill(write("a")), unreachable), /bound/);
  await assert.rejects(publishClientWrites([write("a"), write("a")], unreachable), /logical target/);
});

test("conditioned batches retain every dependency and reject unsupported effects and post-refusal tails", async () => {
  const condition = { octopus_home: root, preimages: [{ path: path.join(root, "ee.json"), sha256: hash("authority") },
    { path: path.join(root, "absent-cursor"), sha256: null }] };
  let calls = 0;
  const publisher = createStoragePublisher("/distribution/topology", async (_binary, mode, request) => {
    calls++; assert.equal(mode, "--publish-files"); assert.deepEqual(request.condition, condition);
    return { receipts: request.writes.map(applied), error: null };
  });
  await publisher.publishFiles([write("guarded", hash("old"))], condition); assert.equal(calls, 1);
  const retired = path.join(root, "retired-parent"); fs.writeFileSync(retired, "foreign replacement");
  await publisher.publishFiles([{ ...write("guarded", hash("old")), filePath: path.join(retired, "original.json") }], condition);
  assert.equal(calls, 2); assert.equal(fs.readFileSync(retired, "utf8"), "foreign replacement");
  for (const entry of [write("creation"), { ...write("removal", hash("old")), content: null }]) {
    await assert.rejects(publisher.publishFiles([entry], condition), /existing-file updates/);
  }
  await assert.rejects(publisher.publishFiles([write("guarded", hash("old"))], { ...condition, preimages: [...condition.preimages, condition.preimages[0]] }), /duplicate/);
  assert.equal(calls, 2);
  const { publishFiles } = require("./publication.cjs");
  await assert.rejects(publishFiles([write("refused"), write("tail")], async ({ writes }) => ({ receipts: [
    { ...applied(writes[0]), outcome: "not_applied", error: "refused" }, applied(writes[1])], error: null })),
    error => error.publicationReceipts.length === 1 && /after refusal/.test(error.message));
});

test("client commit and message-admission helpers thread the declared installed executable", async () => {
  const filename = path.join(__dirname, "client.cjs"), loaded = { exports: {} }, calls = [];
  const source = `${fs.readFileSync(filename, "utf8")}\nrunOperatorRevisionCommand = publicationTransport; module.exports = { ...module.exports, commitClientWrites, writeOperatorMessageAdmission };`;
  vm.runInNewContext(source, { Buffer, __dirname, __filename: filename, clearTimeout, console,
    module: loaded, process, require: createRequire(filename), setTimeout,
    publicationTransport: async (binary, mode, { writes }) => {
      calls.push({ binary, mode });
      return { receipts: writes.map(applied), error: null };
    },
  });
  const options = { installationRoot: () => root, registryRoot: () => path.join(root, "registry") };
  await loaded.exports.commitClientWrites([write("direct")], options);
  await loaded.exports.writeOperatorMessageAdmission(options, path.join(root, "admission.json"), null, { message_id: "m1" });
  const publisher = await loaded.exports.loadStoragePublisher(options);
  assert.deepEqual(Object.keys(publisher), ["publishFiles", "snapshotDirectory", "publishDirectoryMoves", "reclaimDirectory"]);
  assert.equal(calls.length, 2);
  await publisher.publishFiles([write("factory")]);
  const declared = JSON.parse(fs.readFileSync(path.join(__dirname, "client.json"), "utf8")).executables.topology_planner;
  assert.deepEqual(calls, Array(3).fill({ binary: path.join(root, declared), mode: "--publish-files" }));
});

const directoryPreimage = inode => ({ identity: { device: 4, inode }, mode: 0o755, digest_algorithm: "sha256", entries: [] });
const directoryMoves = [
  { source: "/publication/active", destination: "/publication/backup", expected: directoryPreimage(10) },
  { source: "/publication/staged", destination: "/publication/active", expected: directoryPreimage(20) },
];
const directoryReceipts = directoryMoves.map((movement, index) => ({ schema_version: 1, operation_id: "repair-v1", index,
  plan_hash: "39e803163220b837985dccf2b837754208e9cb18a0d5b9f981b1cd99224ebc90", movement, outcome: "applied", error: null }));

test("storage factory is lazy and retains one exact native repair batch and reclaim proof", async () => {
  const calls = [], publisher = createStoragePublisher("/distribution/topology", async (binary, mode, request, maximum) => {
    calls.push({ binary, mode, request, maximum });
    if (mode === "--snapshot-directory") return { path: request.path, preimage: directoryPreimage(10) };
    if (mode === "--publish-directory-moves") return { receipts: structuredClone(directoryReceipts), error: null };
    return { tombstone: request.tombstone, publication: Object.fromEntries(Object.entries(request.publication).reverse()), reclaimed: true };
  });
  assert.equal(calls.length, 0);
  assert.deepEqual(await publisher.publishFiles([]), []); assert.equal(calls.length, 0);
  assert.deepEqual(await publisher.snapshotDirectory(directoryMoves[0].source), directoryPreimage(10));
  for (let replay = 0; replay < 2; replay += 1) assert.deepEqual(await publisher.publishDirectoryMoves("repair-v1", directoryMoves), directoryReceipts);
  assert.equal(await publisher.reclaimDirectory(directoryMoves[0].destination, directoryReceipts[0], directoryMoves[0].expected), undefined);
  assert.deepEqual(calls.map(call => call.mode), ["--snapshot-directory", "--publish-directory-moves", "--publish-directory-moves", "--reclaim-directory"]);
  assert.deepEqual(calls[1].request, { operation_id: "repair-v1", moves: directoryMoves });
  assert.deepEqual(calls[1], calls[2]); assert.ok(calls.every(call => call.maximum === 64 * 1024 * 1024));
});

test("directory outcomes correlate the entire immutable request and retain only validated receipts", async () => {
  for (const outcome of ["applied", "not_applied", "uncertain"]) {
    const original = { ...directoryReceipts[1], outcome, error: outcome === "applied" ? null : "conflict" };
    for (const change of [{ schema_version: 2 }, { operation_id: "other" }, { index: 0 }, { plan_hash: hash("other") },
      { movement: { ...directoryMoves[1], source: "/other" } },
      { movement: { ...directoryMoves[1], expected: directoryPreimage(99) } }]) {
      const response = { receipts: [directoryReceipts[0], { ...original, ...change }], error: null };
      const publisher = createStoragePublisher("/distribution/topology", async () => response);
      await assert.rejects(publisher.publishDirectoryMoves("repair-v1", directoryMoves), error => {
        assert.deepEqual(error.publicationReceipts, [directoryReceipts[0]]); assert.equal(error.publicationResult, response); return true;
      });
    }
    if (outcome !== "applied") {
      const response = { receipts: [directoryReceipts[0], original], error: "exact source manifest changed" };
      await assert.rejects(createStoragePublisher("/distribution/topology", async () => response).publishDirectoryMoves("repair-v1", directoryMoves),
        error => { assert.deepEqual(error.publicationReceipts, response.receipts);
          assert.ok(error.message.includes(directoryMoves[1].source) && error.message.includes(directoryMoves[1].destination) && error.message.includes("conflict"));
          assert.ok(error.message.includes(response.error)); return true; });
    }
  }
  const failed = new Error("native transport lost");
  await assert.rejects(createStoragePublisher("/distribution/topology", async () => { throw failed; }).publishDirectoryMoves("repair-v1", directoryMoves), error => error === failed);
});

test("directory manifests and reclaim acknowledgements reject foreign or untyped evidence", async () => {
  let calls = 0, response;
  const publisher = createStoragePublisher("/distribution/topology", async () => { calls += 1; return response; });
  await assert.rejects(publisher.reclaimDirectory("/publication/rebased", directoryReceipts[0], directoryMoves[0].expected), /exact applied/);
  await assert.rejects(publisher.reclaimDirectory(directoryMoves[0].destination, directoryReceipts[0], directoryPreimage(99)), /exact applied/);
  await assert.rejects(publisher.publishDirectoryMoves("repair-v1", [{ ...directoryMoves[0], expected: { ...directoryPreimage(10), digest_algorithm: "blake3" } }]), /SHA-256/);
  assert.equal(calls, 0);
  response = { path: directoryMoves[0].source, preimage: { ...directoryPreimage(10), entries: [
    { relative_path: "a", kind: "directory", identity: { device: 4, inode: 11 }, mode: 0o755, bytes: 0, digest: null },
    { relative_path: "a/é", kind: "file", identity: { device: 4, inode: 12 }, mode: 0o600, bytes: 5, digest: hash("café") },
    { relative_path: "a-", kind: "symlink", identity: { device: 4, inode: 13 }, mode: 0o777, bytes: 1, digest: hash("a") },
  ] } };
  assert.deepEqual(await publisher.snapshotDirectory(directoryMoves[0].source), response.preimage);
  response.preimage.entries[1].bytes = Number.MAX_SAFE_INTEGER + 1;
  await assert.rejects(publisher.snapshotDirectory(directoryMoves[0].source), /integer/);
  response = { tombstone: directoryMoves[0].destination, publication: directoryReceipts[1], reclaimed: true };
  await assert.rejects(publisher.reclaimDirectory(directoryMoves[0].destination, directoryReceipts[0], directoryMoves[0].expected), /another original/);
});
