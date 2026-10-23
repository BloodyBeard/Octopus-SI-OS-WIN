const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const fsSync = require("node:fs");
const path = require("node:path");
const { createHash } = require("node:crypto");
const { execFileSync } = require("node:child_process");

const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const present = async (file) => fs.lstat(file).then(() => true, (error) => {
  if (error.code === "ENOENT") return false;
  throw error;
});
const transactions = async directory => {
  const entries = await fs.readdir(directory);
  if (entries.includes(".octopus-publication")) {
    const stat = await fs.lstat(path.join(directory, ".octopus-publication"));
    assert.ok(stat.isDirectory() && !stat.isSymbolicLink() && !(stat.mode & 0o077));
  }
  return entries.filter(name => name !== ".octopus-publication");
};

async function runFixture(input) {
  const { mode, home, repository, binary, selection, removals, preserved, provider_directory: providerDirectory } = input;
  assert.ok(["commit", "interrupt", "recover", "settle-private", "change-public"].includes(mode));
  const registry = path.join(home, "registry");
  const transactionRoot = path.join(home, "client-state/workspace/transactions");
  const { commitProjectionClientAuthoring } = require(path.join(repository, "app/electron/clientWorkspaceAuthoring.cjs"));
  const { recoverWorkspaceTransactions } = require(path.join(repository, "app/electron/workspaceSetup.cjs"));
  const { createTaskRetirementBarrier } = require(path.join(repository, "registry/interface/world_operator_chat/task-retirement.cjs"));
  const { createStoragePublisher } = require(path.join(repository, "registry/interface/world_operator_chat/publication.cjs"));
  const publication = createStoragePublisher(binary, (executable, flag, request, maxBuffer) => JSON.parse(execFileSync(executable, [flag], {
    input: JSON.stringify(request), encoding: "utf8", timeout: 30000, maxBuffer,
  })));
  const before = new Map(await Promise.all(preserved.map(async (file) => [file, await fs.readFile(file)])));
  const ledger = before.get(path.join(home, "ledger/movements.jsonl"));
  assert.ok(ledger);
  let preflights = 0, acquisitions = 0, heldVerifications = 0, usedPriorProof = false;
  const native = createTaskRetirementBarrier(binary, async (executable, flag, context) => {
    assert.equal(executable, binary);
    assert.equal(flag, "--verify-task-retirement");
    const output = execFileSync(executable, [flag], {
      input: JSON.stringify(context), encoding: "utf8", timeout: 15000, maxBuffer: 4 * 1024 * 1024,
    });
    return JSON.parse(output);
  });
  const retirementBarrier = {
    async verifyBefore(context) {
      preflights += 1;
      assert.equal(context.roots.run_records_root, path.join(home, "state/runs"));
      assert.equal(context.roots.interface_private_state_root, path.join(home, "interfaces"));
      assert.deepEqual(context.selection, selection);
      await native.verifyBefore(context);
      if (mode === "settle-private") {
        await fs.writeFile(path.join(home, "interfaces", path.basename(providerDirectory), "lifecycle-finalized"), "settled\n");
      }
      if (mode === "change-public") await fs.writeFile(path.join(providerDirectory, "unexpected-public-edit"), "changed\n");
    },
    async withHeldProof(context, operation) {
      acquisitions += 1;
      usedPriorProof ||= context.prior_proof !== null;
      assert.deepEqual(context.selection, selection);
      if (mode === "recover") {
        assert.ok(context.prior_proof, "cold recovery must use the actual durable native proof");
        assert.equal(await present(providerDirectory), false, "provider was tombstoned before the caller died");
      }
      return native.withHeldProof(context, async (guard) => operation({
        ...guard,
        async verify() {
          const proof = await guard.verify();
          heldVerifications += 1;
          assert.equal(proof.phase, "held");
          assert.deepEqual(proof.interface_ids, selection.interface_ids);
          assert.equal(proof.originals.length, selection.records.length);
          assert.equal(proof.ledger_boundary.complete_offset, ledger.length);
          assert.equal(proof.ledger_boundary.sha256, digest(ledger));
          for (const row of selection.records) assert.equal(await present(row.file), false);
          if (mode === "interrupt" && !await present(providerDirectory)) {
            const entries = await transactions(transactionRoot);
            assert.equal(entries.length, 1);
            const journal = JSON.parse(await fs.readFile(path.join(transactionRoot, entries[0], "journal.json"), "utf8"));
            assert.equal(journal.phase, "published");
            assert.ok(journal.retirement.proof, "interruption must follow a durable native proof");
            const provider = journal.removals.find((entry) => entry.destination === providerDirectory);
            assert.equal(provider.state, "moved");
            assert.equal(await present(path.join(transactionRoot, entries[0], provider.tombstone)), true);
            for (const row of journal.writes) assert.equal(await present(path.join(transactionRoot, entries[0], row.before_file)), true);
            fsSync.writeSync(1, JSON.stringify({ phase: journal.phase, provider_tombstoned: true, has_held_proof: true }));
            process.kill(process.pid, "SIGKILL");
            throw new Error("SIGKILL did not interrupt the caller");
          }
          return proof;
        },
      }));
    },
  };
  if (mode === "recover") {
    const result = await recoverWorkspaceTransactions({ octopusHomeRoot: home, registryRoot: registry, publication, retirementBarrier });
    assert.equal(result.recovered, 1);
  } else {
    const plannedRemovals = await Promise.all(removals.map(async (directory) => {
      const stat = await fs.lstat(directory);
      assert.ok(stat.isDirectory() && !stat.isSymbolicLink());
      return { path: directory, type: "directory", dev: stat.dev, ino: stat.ino, label: directory };
    }));
    await commitProjectionClientAuthoring(home, registry, {
      label: "native fixture task retirement",
      writes: selection.records.map((row) => ({ file: row.file, beforeDigest: row.expected_digest, next: null })),
      removals: plannedRemovals,
      publishBarrierPath: selection.records.find((row) => row.run === selection.source_run).file,
      retirement: selection,
    }, { publication, retirementBarrier });
  }
  for (const row of selection.records) assert.equal(await present(row.file), false);
  for (const directory of removals) assert.equal(await present(directory), false);
  for (const [file, bytes] of before) assert.deepEqual(await fs.readFile(file), bytes);
  assert.ok(heldVerifications > 1, "commit/recovery must recheck the real holder during cleanup");
  const remainingTransactions = (await transactions(transactionRoot)).length;
  assert.equal(remainingTransactions, 0);
  const receiptRoot = path.join(home, "client-state/workspace/client-authoring-receipts");
  const receipts = await fs.readdir(receiptRoot);
  assert.equal(receipts.length, 1);
  const receipt = JSON.parse(await fs.readFile(path.join(receiptRoot, receipts[0]), "utf8"));
  assert.equal(receipt.writes.length, selection.records.length);
  return { preflights, acquisitions, held_verifications: heldVerifications, used_prior_proof: usedPriorProof,
    removed_records: selection.records.length, remaining_transactions: remainingTransactions };
}

if (require.main === module) {
  let input = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => { input += chunk; });
  process.stdin.on("end", async () => {
    try { process.stdout.write(JSON.stringify(await runFixture(JSON.parse(input)))); }
    catch (error) { process.stderr.write(`${error.stack}\n`); process.exitCode = 1; }
  });
}

module.exports = { runFixture };
