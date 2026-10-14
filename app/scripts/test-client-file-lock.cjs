const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { withClientFileLock } = require("../electron/clientFileLock.cjs");

test("expired local waiters never run later or release the active file owner", { timeout: 5000 }, async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "oct-client-file-lock-"));
  const options = { directory, filename: "owner.sqlite", waitMs: 1000, busyCode: "busy", busyMessage: "Owner busy" };
  const entered = Promise.withResolvers(), release = Promise.withResolvers();
  let expiredRan = false, nextRan = false;
  const owner = withClientFileLock(options, async () => { entered.resolve(); await release.promise; });
  try {
    await entered.promise;
    await assert.rejects(withClientFileLock({ ...options, waitMs: 15 }, () => { expiredRan = true; }), error => error.code === "busy");
    const next = withClientFileLock(options, () => { nextRan = true; });
    await new Promise(resolve => setTimeout(resolve, 30));
    assert.equal(expiredRan, false); assert.equal(nextRan, false);
    release.resolve(); await owner; await next;
    assert.equal(expiredRan, false); assert.equal(nextRan, true);
    const failure = new Error("operation refused");
    await assert.rejects(withClientFileLock(options, () => { throw failure; }), error => error === failure);
    assert.equal(await withClientFileLock(options, () => "released"), "released");
  } finally { release.resolve(); await owner; await fs.rm(directory, { recursive: true, force: true }); }
});
