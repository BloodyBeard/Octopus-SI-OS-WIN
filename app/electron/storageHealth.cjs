const fs = require("node:fs/promises");
const path = require("node:path");
const { randomUUID } = require("node:crypto");

const MINIMUM_WRITABLE_BYTES = 16 * 1024 * 1024;
const STORAGE_BLOCKED_MESSAGE = "Disk full or storage quota reached for Octopus Home. Free disk space to continue. Sending is paused; your draft is retained.";

function nativeStorageFailure(error) {
  return error?.code === "ENOSPC" || error?.code === "EDQUOT";
}

function blockedStorageHealth(code, availableBytes = null) {
  return { status: "blocked", availableBytes, minimumBytes: MINIMUM_WRITABLE_BYTES, code,
    message: code === "insufficient_space"
      ? "Octopus Home has less than 16 MiB of writable disk space. Free disk space to continue. Sending is paused; your draft is retained."
      : STORAGE_BLOCKED_MESSAGE };
}

async function readHomeStorageHealth(home) {
  let availableBytes = null;
  let handle;
  let probePath;
  let health;
  try {
    const stat = await fs.statfs(home, { bigint: true });
    availableBytes = Number(stat.bavail * stat.bsize);
    if (availableBytes < MINIMUM_WRITABLE_BYTES) return blockedStorageHealth("insufficient_space", availableBytes);
    probePath = path.join(home, `.octopus-client-storage-${randomUUID()}`);
    handle = await fs.open(probePath, "wx", 0o600);
    await handle.writeFile(Buffer.alloc(4096));
    await handle.sync();
    health = { status: "writable", availableBytes, minimumBytes: MINIMUM_WRITABLE_BYTES, code: null, message: null };
  } catch (error) {
    health = nativeStorageFailure(error)
      ? blockedStorageHealth(error.code, availableBytes)
      : { status: "unavailable", availableBytes, minimumBytes: MINIMUM_WRITABLE_BYTES,
        code: typeof error?.code === "string" ? error.code : null,
        message: `Octopus could not verify writable storage for this Home: ${error instanceof Error ? error.message : String(error)}. Sending is paused; your draft is retained.` };
  } finally {
    try {
      if (handle) {
        try { await handle.close(); }
        finally { await fs.unlink(probePath); }
      }
    } catch (error) {
      health = nativeStorageFailure(error)
        ? blockedStorageHealth(error.code, availableBytes)
        : { status: "unavailable", availableBytes, minimumBytes: MINIMUM_WRITABLE_BYTES,
          code: typeof error?.code === "string" ? error.code : null,
          message: `Octopus could not finish checking Home storage: ${error instanceof Error ? error.message : String(error)}. Sending is paused; your draft is retained.` };
    }
  }
  return health;
}

module.exports = { MINIMUM_WRITABLE_BYTES, STORAGE_BLOCKED_MESSAGE, blockedStorageHealth, nativeStorageFailure, readHomeStorageHealth };
