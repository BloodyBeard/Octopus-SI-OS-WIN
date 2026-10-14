const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { setTimeout: pause } = require("node:timers/promises");
const { test, before, after } = require("node:test");
const { createTaskRetirementBarrier } = require("./task-retirement.cjs");

function holder() {
  let context;
  process.on("SIGTERM", () => process.exit(0));
  require("node:readline").createInterface({ input: process.stdin }).on("line", (line) => {
    const value = JSON.parse(line);
    if (value.operation === "release") process.exit(context.release_error ? 23 : 0);
    context ??= value;
    process.stdout.write(`${JSON.stringify({ schema_version: 1, phase: "held", pid: process.pid })}\n`);
  });
}

let root, barrier;
before(async () => {
  root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-retirement-lifetime-"));
  const binary = path.join(root, "holder");
  await fs.writeFile(binary, `#!${process.execPath}\n(${holder.toString()})();\n`, { mode: 0o700 });
  barrier = createTaskRetirementBarrier(binary);
});
after(async () => { if (root) await fs.rm(root, { recursive: true, force: true }); });

async function exited(pid) {
  for (let tries = 0; tries < 200; tries++) {
    try { process.kill(pid, 0); }
    catch (error) { if (error.code === "ESRCH") return; throw error; }
    await pause(10);
  }
  throw new Error("real holder did not exit");
}

test("successful callback requires live final proof and clean release", async () => {
  const result = await barrier.withHeldProof({}, async (guard) => {
    assert.equal((await guard.verify()).phase, "held");
    return 42;
  });
  assert.equal(result, 42);
  await assert.rejects(barrier.withHeldProof({ release_error: true }, async () => 42), /owner exited.*23/);
});

test("holder death fails the next destructive boundary and wrapper completion", async () => {
  let destructive = false, settled = false;
  await assert.rejects(barrier.withHeldProof({}, async (guard) => {
    const { pid } = await guard.verify();
    process.kill(pid, "SIGKILL"); await exited(pid);
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
  const wrapped = barrier.withHeldProof({}, async (guard) => {
    const { pid } = await guard.verify();
    process.kill(pid, "SIGTERM"); await exited(pid); signalDeath();
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
    await assert.rejects(barrier.withHeldProof({ release_error: true }, async (guard) => {
      if (kill) { const { pid } = await guard.verify(); process.kill(pid, "SIGKILL"); await exited(pid); }
      throw original;
    }), (error) => error === original);
  }
});
