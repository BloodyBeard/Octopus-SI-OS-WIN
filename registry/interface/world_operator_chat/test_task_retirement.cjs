const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { setTimeout: pause } = require("node:timers/promises");
const { test, before, after } = require("node:test");
const { createTaskRetirementBarrier } = require("./task-retirement.cjs");

function holder() {
  const fs = require("node:fs"), crypto = require("node:crypto");
  const canonical = value => Array.isArray(value) ? `[${value.map(canonical).join(",")}]` : value && typeof value === "object"
    ? `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}` : JSON.stringify(value);
  let context, proof;
  process.on("SIGTERM", () => process.exit(0));
  require("node:readline").createInterface({ input: process.stdin }).on("line", (line) => {
    const value = JSON.parse(line);
    if (!context) {
      context = value;
      fs.writeFileSync(context.fixture.pid_file, String(process.pid));
      proof = { schema_version: 2, phase: "held", operation_id: context.operation_id, selection_digest: context.selection_digest,
        directory_inventory_digest: crypto.createHash("sha256").update(canonical(context.directory_inventory)).digest("hex"),
        interface_ids: context.selection.interface_ids, originals: context.originals.map(({ file, expected_digest }) => ({ file, expected_digest })),
        retired: context.retired, ledger_boundary: { complete_offset: 0, sequence: 0, sha256: crypto.createHash("sha256").update("").digest("hex") } };
      if (context.fixture.malformed === "inventory_proof") proof.directory_inventory_digest = "0".repeat(64);
      if (context.fixture.malformed === "proof_v1") { proof.schema_version = 1; delete proof.directory_inventory_digest; }
    }
    fs.appendFileSync(context.fixture.events_file, `${value.operation || "context"}\n`);
    if (value.operation === "release") process.exit(context.fixture.release_error ? 23 : 0);
    let response = proof;
    const envelope = (phase, fields) => ({ schema_version: 1, phase, operation_id: context.operation_id, proof, ...fields });
    if (value.operation === "snapshot_directory") {
      const stat = fs.lstatSync(value.path);
      response = envelope("directory_snapshot", { path: value.path, preimage: {
        identity: { device: stat.dev, inode: stat.ino }, mode: stat.mode & 0o7777, digest_algorithm: "sha256", entries: context.fixture.entries || [] } });
      if (context.fixture.malformed === "snapshot_path") response.path += "-other";
      if (context.fixture.malformed === "snapshot_schema") response.preimage.extra = true;
      if (context.fixture.malformed === "snapshot_identity") response.preimage.identity.inode = Number.MAX_SAFE_INTEGER + 1;
      if (context.fixture.malformed === "snapshot_algorithm") response.preimage.digest_algorithm = "blake3";
    }
    if (value.operation === "publish_directory_moves") {
      const receipts = value.moves.map((movement, index) => ({ schema_version: 1, operation_id: value.operation_id, index,
        plan_hash: crypto.createHash("sha256").update(JSON.stringify(value.moves)).digest("hex"), movement, outcome: "applied", error: null }));
      const mode = context.fixture.malformed;
      if (mode === "publication_operation") receipts[0].operation_id += "-other";
      if (mode === "publication_plan") receipts[0].plan_hash = "0".repeat(64);
      if (mode === "publication_index") receipts[0].index += 1;
      if (mode === "publication_movement") receipts[0].movement.destination += "-other";
      if (mode === "publication_schema") receipts[0].extra = true;
      if (mode === "publication_partial") receipts[1] = { ...receipts[1], outcome: "uncertain", error: "interrupted" };
      else if (!mode) for (const move of value.moves) fs.renameSync(move.source, move.destination);
      response = envelope("directory_publication", { publication_operation_id: value.operation_id,
        result: { receipts, error: mode === "publication_partial" ? "interrupted" : null } });
    }
    if (value.operation === "reclaim_directory") {
      if (!context.fixture.malformed) fs.rmSync(value.tombstone, { recursive: true });
      response = envelope("directory_reclaimed", { tombstone: value.tombstone, receipt: value.receipt });
      if (context.fixture.malformed === "reclaim_path") response.tombstone += "-other";
    }
    if (value.operation === "assert_held" && context.fixture.malformed === "held_schema") response = { ...proof, extra: true };
    const send = () => process.stdout.write(`${JSON.stringify(response)}\n`);
    if (value.operation === "publish_directory_moves" && context.fixture.delay) setTimeout(send, 20);
    else send();
  });
}

let root, barrier, counter = 0;
before(async () => {
  root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-retirement-lifetime-"));
  const binary = path.join(root, "holder");
  await fs.writeFile(binary, `#!${process.execPath}\n(${holder.toString()})();\n`, { mode: 0o700 });
  barrier = createTaskRetirementBarrier(binary);
});
after(async () => { if (root) await fs.rm(root, { recursive: true, force: true }); });

function context(options = {}) {
  const id = ++counter;
  return { operation_id: "retirement-fixture", selection_digest: "a".repeat(64), selection: { interface_ids: ["selected"] },
    originals: [], retired: [], prior_proof: null,
    directory_inventory: { schema_version: 1, removals: [], originals: [], retired: [] },
    fixture: { pid_file: path.join(root, `pid-${id}`), events_file: path.join(root, `events-${id}`), ...options } };
}

const pid = async (ctx) => Number(await fs.readFile(ctx.fixture.pid_file, "utf8"));
async function directory() { return fs.mkdtemp(path.join(root, "directory-")); }

async function exited(pid) {
  for (let tries = 0; tries < 200; tries++) {
    try { process.kill(pid, 0); }
    catch (error) { if (error.code === "ESRCH") return; throw error; }
    await pause(10);
  }
  throw new Error("real holder did not exit");
}

test("successful callback requires live final proof and clean release", async () => {
  const result = await barrier.withHeldProof(context(), async (guard) => {
    assert.equal((await guard.verify()).phase, "held");
    return 42;
  });
  assert.equal(result, 42);
  await assert.rejects(barrier.withHeldProof(context({ release_error: true }), async () => 42), /owner exited.*23/);
});

test("holder death fails the next destructive boundary and wrapper completion", async () => {
  let destructive = false, settled = false;
  const ctx = context();
  await assert.rejects(barrier.withHeldProof(ctx, async (guard) => {
    await guard.verify(); const ownerPid = await pid(ctx);
    process.kill(ownerPid, "SIGKILL"); await exited(ownerPid);
    await assert.rejects((async () => { await guard.verify(); destructive = true; })(), /owner|EPIPE/);
    settled = true;
    return 42;
  }), /owner|EPIPE/);
  assert.equal(destructive, false); assert.equal(settled, true);
});

test("holder death after assert waits for callback settlement and rejects its success", async () => {
  let signalDeath, finish, settled = false, wrapperSettled = false;
  const died = new Promise((resolve) => { signalDeath = resolve; });
  const completion = new Promise((resolve) => { finish = resolve; });
  const ctx = context();
  const wrapped = barrier.withHeldProof(ctx, async (guard) => {
    await guard.verify(); const ownerPid = await pid(ctx);
    process.kill(ownerPid, "SIGTERM"); await exited(ownerPid); signalDeath();
    await completion; settled = true;
    return 42;
  });
  const rejected = assert.rejects(wrapped, /owner|EPIPE/);
  wrapped.then(() => { wrapperSettled = true; }, () => { wrapperSettled = true; });
  try {
    await died; await pause(20);
    assert.equal(wrapperSettled, false); assert.equal(settled, false);
  } finally { finish(); await rejected; }
  assert.equal(settled, true);
});

test("callback exception survives holder loss and failed release", async () => {
  for (const kill of [false, true]) {
    const original = new Error("callback failed");
    const ctx = context({ release_error: true });
    await assert.rejects(barrier.withHeldProof(ctx, async (guard) => {
      if (kill) { await guard.verify(); const ownerPid = await pid(ctx); process.kill(ownerPid, "SIGKILL"); await exited(ownerPid); }
      throw original;
    }), (error) => error === original);
  }
});

test("same holder moves and reclaims, and completion drains queued commands", async () => {
  const source = await directory(), tombstone = `${source}-removed`, ctx = context({ delay: true,
    entries: ["a", "a/item", "a.json"].map((relative_path, index) => ({ relative_path, kind: index === 0 ? "directory" : "file",
      identity: { device: 1, inode: index + 1 }, mode: 0o700, bytes: index === 0 ? 0 : 1, digest: index === 0 ? null : "f".repeat(64) })) });
  await barrier.withHeldProof(ctx, async (guard) => {
    const expected = await guard.snapshotDirectory(source);
    const [receipt] = await guard.publishDirectoryMoves(`${ctx.operation_id}:remove`, [{ source, destination: tombstone, expected }]);
    assert.equal(await fs.stat(tombstone).then((stat) => stat.ino), expected.identity.inode);
    await assert.rejects(async () => guard.reclaimDirectory(`${tombstone}-other`, receipt, expected), /exact Applied/);
    await guard.reclaimDirectory(tombstone, receipt, expected);
  });
  await assert.rejects(fs.stat(tombstone), { code: "ENOENT" }); await exited(await pid(ctx));
  const queuedSource = await directory(), queuedContext = context({ delay: true });
  await barrier.withHeldProof(queuedContext, async (guard) => {
    const expected = await guard.snapshotDirectory(queuedSource);
    void guard.publishDirectoryMoves(`${queuedContext.operation_id}:queued`, [{ source: queuedSource, destination: `${queuedSource}-removed`, expected }]);
  });
  assert.ok(await fs.stat(`${queuedSource}-removed`));
  assert.match(await fs.readFile(queuedContext.fixture.events_file, "utf8"), /publish_directory_moves\nassert_held\nrelease\n$/);
});

test("holder death after a valid proof prevents the next requested move", async () => {
  const source = await directory(), ctx = context();
  await assert.rejects(barrier.withHeldProof(ctx, async (guard) => {
    const expected = await guard.snapshotDirectory(source);
    await guard.verify(); const ownerPid = await pid(ctx); process.kill(ownerPid, "SIGKILL"); await exited(ownerPid);
    await guard.publishDirectoryMoves(`${ctx.operation_id}:remove`, [{ source, destination: `${source}-removed`, expected }]);
  }), /owner|EPIPE/);
  assert.ok(await fs.stat(source)); await assert.rejects(fs.stat(`${source}-removed`), { code: "ENOENT" });
});

test("strict directory responses refuse changed schemas, coordinates, manifests and receipts", async () => {
  for (const malformed of ["held_schema", "inventory_proof", "proof_v1", "snapshot_path", "snapshot_schema", "snapshot_identity", "snapshot_algorithm",
    "publication_operation", "publication_plan", "publication_index", "publication_movement", "publication_schema", "reclaim_path"]) {
    const source = await directory(), ctx = context({ malformed });
    await assert.rejects(barrier.withHeldProof(ctx, async (guard) => {
      if (malformed === "held_schema") return guard.verify();
      const expected = await guard.snapshotDirectory(source);
      if (malformed.startsWith("snapshot")) return;
      const [receipt] = await guard.publishDirectoryMoves(`${ctx.operation_id}:remove`, [{ source, destination: `${source}-removed`, expected }]);
      await guard.reclaimDirectory(`${source}-removed`, receipt, expected);
    }), /Task retirement/);
    await exited(await pid(ctx));
  }
});

test("additional inventory is immutable, content pinned and carried by the held proof", async () => {
  const source = await directory(), ctx = context(), content = '{"id":"owned-tool","interface":"selected-door"}';
  ctx.directory_inventory = { schema_version: 1, removals: [{ path: source, identity: { device: 1, inode: 2 } }],
    originals: [{ file: path.join(source, "tool.json"), content,
      expected_digest: require("node:crypto").createHash("sha256").update(content).digest("hex") }], retired: [] };
  const before = structuredClone(ctx.directory_inventory);
  await barrier.withHeldProof(ctx, async (guard) => {
    const first = await guard.verify();
    ctx.directory_inventory.removals[0].identity.inode++;
    assert.deepEqual(await guard.verify(), first);
    assert.match(first.directory_inventory_digest, /^[0-9a-f]{64}$/);
  });
  for (const mutation of [value => { value.extra = true; }, value => { value.originals[0].content += " "; },
    value => { value.removals[0].identity.inode = Number.MAX_SAFE_INTEGER + 1; },
    value => { value.retired = [{ file: "/unselected/artifact.json", expected_digest: null }]; }]) {
    const invalid = context(); invalid.directory_inventory = structuredClone(before); mutation(invalid.directory_inventory);
    await assert.rejects(barrier.withHeldProof(invalid, async () => assert.fail("invalid inventory reached callback")), /Task retirement/);
    await assert.rejects(fs.stat(invalid.fixture.pid_file), { code: "ENOENT" });
  }
});

test("partial publication preserves validated receipts and callback failure survives a queued operation", async () => {
  const source = await directory(), ctx = context({ malformed: "publication_partial" });
  await assert.rejects(barrier.withHeldProof(ctx, async (guard) => {
    const expected = await guard.snapshotDirectory(source);
    await guard.publishDirectoryMoves(`${ctx.operation_id}:partial`, [source, `${source}-second`]
      .map((source) => ({ source, destination: `${source}-removed`, expected })));
  }), (error) => error.code === "task_retirement_publication_failed" && error.publicationReceipts.length === 2
    && error.publicationReceipts[0].outcome === "applied" && error.publicationReceipts[1].outcome === "uncertain");
  const queued = context({ malformed: "publication_plan", delay: true }), original = new Error("original callback failure");
  await assert.rejects(barrier.withHeldProof(queued, async (guard) => {
    const expected = await guard.snapshotDirectory(source);
    void guard.publishDirectoryMoves(`${queued.operation_id}:queued`, [{ source, destination: `${source}-removed`, expected }]);
    throw original;
  }), (error) => error === original);
  await exited(await pid(queued));
});
