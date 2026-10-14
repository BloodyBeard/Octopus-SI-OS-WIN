const fs = require("node:fs/promises");
const { constants } = require("node:fs");
const path = require("node:path");
const { setTimeout: delay } = require("node:timers/promises");

const tails = new Map();

async function withClientFileLock(options, operation) {
  const { directory, filename, shared = false, waitMs = 0, busyCode, busyMessage } = options;
  if (!path.isAbsolute(directory) || path.basename(filename) !== filename || !filename
    || !Number.isFinite(waitMs) || waitMs < 0 || typeof operation !== "function") throw new Error("Invalid client file lock");
  const deadline = waitMs > 0 ? performance.now() + waitMs : null;
  const busy = cause => Object.assign(new Error(busyMessage, { cause }), { code: busyCode });
  await fs.mkdir(directory, { recursive: true, mode: 0o700 });
  const root = await fs.lstat(directory);
  if (!root.isDirectory() || root.isSymbolicLink()) throw new Error("Client lock root must be a plain directory");
  const lockPath = path.join(await fs.realpath(directory), filename);
  const previous = tails.get(lockPath) ?? Promise.resolve();
  let release;
  const held = new Promise(resolve => { release = resolve; });
  const tail = previous.then(() => held);
  tails.set(lockPath, tail);
  return (async () => {
    if (deadline === null) await previous;
    else {
      let timer;
      try {
        await Promise.race([previous, new Promise((_, reject) => {
          timer = setTimeout(() => reject(busy()), Math.max(0, deadline - performance.now()));
        })]);
      } finally { clearTimeout(timer); }
      if (performance.now() >= deadline) throw busy();
    }
    const handle = await fs.open(lockPath, constants.O_CREAT | constants.O_RDWR | constants.O_NOFOLLOW, 0o600);
    let database, readOnly = shared;
    try {
      const identity = await handle.stat();
      if (!identity.isFile() || identity.nlink !== 1) throw new Error("Client lock must be a private regular file");
      const { DatabaseSync } = require("node:sqlite");
      database = new DatabaseSync(lockPath);
      const acquire = async (read) => {
        for (;;) {
          if (deadline !== null && performance.now() >= deadline) throw busy();
          try {
            database.exec(read ? "PRAGMA busy_timeout=0; BEGIN; SELECT name FROM sqlite_schema LIMIT 1" : "PRAGMA busy_timeout=0; BEGIN EXCLUSIVE");
            break;
          } catch (error) {
            if (database.isTransaction) database.exec("ROLLBACK");
            if (![5, 6].includes(error.errcode & 255)) throw error;
            if (deadline === null || performance.now() >= deadline) throw busy(error);
            await delay(Math.min(25, Math.max(1, deadline - performance.now())));
          }
        }
        if (database.prepare("PRAGMA journal_mode").get().journal_mode !== "delete") throw new Error("Client lock requires rollback-journal locking");
        const current = await fs.lstat(lockPath);
        if (!current.isFile() || current.nlink !== 1 || current.dev !== identity.dev || current.ino !== identity.ino) throw new Error("Client lock changed while acquiring ownership");
      };
      await acquire(readOnly);
      return await operation(async () => {
        if (!readOnly) return;
        database.exec("ROLLBACK");
        await acquire(false);
        readOnly = false;
      });
    } finally {
      try { database?.close(); }
      finally { await handle.close(); }
    }
  })().finally(() => {
    release();
    void tail.then(() => { if (tails.get(lockPath) === tail) tails.delete(lockPath); });
  });
}

module.exports = { withClientFileLock };
