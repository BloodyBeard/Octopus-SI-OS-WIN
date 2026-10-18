const { spawn } = require("node:child_process");
const { createHash } = require("node:crypto");
const path = require("node:path");
const { isDeepStrictEqual } = require("node:util");

const MAX_FRAME_BYTES = 64 * 1024 * 1024;
const HASH = /^[0-9a-f]{64}$/;
const validHash = (value) => typeof value === "string" && HASH.test(value);
const PROOF_FIELDS = ["schema_version", "phase", "operation_id", "selection_digest", "directory_inventory_digest", "interface_ids", "originals", "retired", "ledger_boundary"];

function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}

function directoryInventory(value) {
  exact(value, ["schema_version", "removals", "originals", "retired"], "directory inventory");
  if (value.schema_version !== 1 || !Array.isArray(value.removals) || value.removals.length > 1024
    || !Array.isArray(value.originals) || !Array.isArray(value.retired)) throw new Error("Task retirement directory inventory has invalid typed fields");
  const paths = new Set(), originals = new Set(), retired = new Set();
  for (const pin of value.removals) {
    exact(pin, ["path", "identity"], "directory inventory removal"); absolutePath(pin.path); identity(pin.identity);
    if (paths.has(pin.path)) throw new Error("Task retirement directory inventory repeats a removal");
    paths.add(pin.path);
  }
  for (const original of value.originals) {
    exact(original, ["file", "expected_digest", "content"], "directory inventory original"); absolutePath(original.file);
    if (!validHash(original.expected_digest) || typeof original.content !== "string" || originals.has(original.file)
      || createHash("sha256").update(original.content).digest("hex") !== original.expected_digest) {
      throw new Error("Task retirement directory inventory changed its original bytes or file set");
    }
    originals.add(original.file);
  }
  for (const pin of value.retired) {
    exact(pin, ["file", "expected_digest"], "directory inventory retired file"); absolutePath(pin.file);
    if (pin.expected_digest !== null || !originals.has(pin.file) || retired.has(pin.file)) {
      throw new Error("Task retirement directory inventory retired files differ from its originals");
    }
    retired.add(pin.file);
  }
  return createHash("sha256").update(canonical(value)).digest("hex");
}

function exact(value, fields, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).length !== fields.length || fields.some((field) => !Object.hasOwn(value, field))) {
    throw new Error(`Task retirement ${label} has a different exact schema`);
  }
  return value;
}

const unsigned = (value) => Number.isSafeInteger(value) && value >= 0;
const nullableError = (value) => value === null || typeof value === "string" && value.length > 0;

function absolutePath(value) {
  if (typeof value !== "string" || !path.isAbsolute(value) || path.normalize(value) !== value
    || value.includes("\\") || /\p{Cc}/u.test(value) || value.split("/").includes(".octopus-publication")) {
    throw new Error("Task retirement directory path must be an ordinary absolute normalized path");
  }
  return value;
}

function identity(value) {
  exact(value, ["device", "inode"], "file identity");
  if (!unsigned(value.device) || !unsigned(value.inode)) throw new Error("Task retirement file identity exceeds the exact integer range");
  return { device: value.device, inode: value.inode };
}

function compareRelativePaths(left, right) {
  const a = left.split("/"), b = right.split("/");
  for (let index = 0; index < Math.min(a.length, b.length); index++) {
    const order = Buffer.compare(Buffer.from(a[index]), Buffer.from(b[index]));
    if (order) return order;
  }
  return a.length - b.length;
}

function preimage(value) {
  exact(value, ["identity", "mode", "digest_algorithm", "entries"], "directory preimage");
  if (!unsigned(value.mode) || value.mode > 0o7777 || value.digest_algorithm !== "sha256"
    || !Array.isArray(value.entries) || value.entries.length > 16384) {
    throw new Error("Task retirement directory preimage has invalid mode, algorithm or entries");
  }
  const kinds = new Map();
  let previous;
  const entries = value.entries.map((entry) => {
    exact(entry, ["relative_path", "kind", "identity", "mode", "bytes", "digest"], "directory entry");
    if (typeof entry.relative_path !== "string" || !entry.relative_path || entry.relative_path.includes("\\")
      || /\p{Cc}/u.test(entry.relative_path) || entry.relative_path.split("/").some((part) => !part || [".", "..", ".octopus-publication"].includes(part))
      || previous !== undefined && compareRelativePaths(previous, entry.relative_path) >= 0
      || !["directory", "file", "symlink"].includes(entry.kind) || !unsigned(entry.mode) || entry.mode > 0o7777
      || !unsigned(entry.bytes) || (entry.kind === "directory" ? entry.bytes !== 0 || entry.digest !== null : !validHash(entry.digest))) {
      throw new Error("Task retirement directory entry has invalid exact path or content evidence");
    }
    const parent = path.posix.dirname(entry.relative_path);
    if (parent !== "." && kinds.get(parent) !== "directory") throw new Error("Task retirement directory entry has no declared parent");
    kinds.set(entry.relative_path, entry.kind); previous = entry.relative_path;
    return { relative_path: entry.relative_path, kind: entry.kind, identity: identity(entry.identity), mode: entry.mode, bytes: entry.bytes, digest: entry.digest };
  });
  return { identity: identity(value.identity), mode: value.mode, digest_algorithm: value.digest_algorithm, entries };
}

function movement(value) {
  exact(value, ["source", "destination", "expected"], "directory move");
  return { source: absolutePath(value.source), destination: absolutePath(value.destination), expected: preimage(value.expected) };
}

function publicationId(value, context) {
  const prefix = `${context.operation_id}:`;
  if (typeof value !== "string" || !value.startsWith(prefix) || value.length === prefix.length
    || Buffer.byteLength(value) > 256 || value.trim() !== value || /\p{Cc}/u.test(value)) {
    throw new Error("Task retirement directory operation id is outside its held transaction");
  }
  return value;
}

function receipt(value) {
  exact(value, ["schema_version", "operation_id", "index", "plan_hash", "movement", "outcome", "error"], "directory receipt");
  if (value.schema_version !== 1 || typeof value.operation_id !== "string" || !unsigned(value.index) || value.index >= 128 || !validHash(value.plan_hash)
    || !["applied", "not_applied", "uncertain"].includes(value.outcome) || !nullableError(value.error)
    || (value.outcome === "applied") !== (value.error === null)) {
    throw new Error("Task retirement directory receipt has invalid typed evidence");
  }
  return { schema_version: value.schema_version, operation_id: value.operation_id, index: value.index, plan_hash: value.plan_hash,
    movement: movement(value.movement), outcome: value.outcome, error: value.error };
}

function publicationResult(value, command) {
  const receipts = [];
  try {
    exact(value, ["receipts", "error"], "directory publication result");
    if (!Array.isArray(value.receipts) || value.receipts.length > command.moves.length || !nullableError(value.error)) {
      throw new Error("Task retirement directory publication returned an invalid result");
    }
    const planHash = createHash("sha256").update(JSON.stringify(command.moves)).digest("hex");
    for (const [index, item] of value.receipts.entries()) {
      const checked = receipt(item);
      if (checked.operation_id !== command.operation_id || checked.index !== index || checked.plan_hash !== planHash
        || !isDeepStrictEqual(checked.movement, command.moves[index]) || receipts.some((prior) => prior.outcome !== "applied")) {
        throw new Error("Task retirement directory receipt differs from its submitted operation and plan");
      }
      receipts.push(checked);
    }
    if (value.error !== null || receipts.length !== command.moves.length || receipts.some((item) => item.outcome !== "applied")) {
      throw new Error(`Task retirement directory publication failed: ${value.error || "moves were not all applied"}`);
    }
    return receipts;
  } catch (error) {
    error.code = "task_retirement_publication_failed";
    error.publicationReceipts = receipts;
    error.publicationResult = value;
    throw error;
  }
}

function createTaskRetirementBarrier(binary, runCommand) {
  return Object.freeze({
    async verifyBefore(context) {
      directoryInventory(context.directory_inventory);
      const result = await runCommand(binary, "--verify-task-retirement", context);
      if (result?.ready !== true || Object.keys(result).length !== 1) {
        throw new Error("Task retirement preflight returned no exact readiness proof");
      }
    },
    async withHeldProof(context, operation) {
      const encodedContext = JSON.stringify(context);
      if (Buffer.byteLength(encodedContext) + 1 > MAX_FRAME_BYTES) throw new Error("Task retirement context exceeds its frame bound");
      const heldContext = JSON.parse(encodedContext);
      const owner = openHold(binary, heldContext);
      let result, error, failed = false;
      try {
        await owner.request(heldContext, owner.proof);
        result = await operation(Object.freeze({
          verify: () => owner.request({ operation: "assert_held" }, owner.proof),
          snapshotDirectory(directory) {
            return owner.request({ operation: "snapshot_directory", transaction_id: heldContext.operation_id, path: absolutePath(directory) }, (response, command) => {
              owner.envelope(response, "directory_snapshot", ["path", "preimage"]);
              if (response.path !== command.path) throw new Error("Task retirement snapshot returned a different path");
              return preimage(response.preimage);
            });
          },
          publishDirectoryMoves(operationId, moves) {
            if (!Array.isArray(moves) || !moves.length || moves.length > 128) throw new Error("Task retirement directory move count is invalid");
            const plan = moves.map(movement);
            if (plan.reduce((count, item) => count + item.expected.entries.length, 0) > 16384) {
              throw new Error("Task retirement directory plan exceeds its entry bound");
            }
            return owner.request({ operation: "publish_directory_moves", transaction_id: heldContext.operation_id,
              operation_id: publicationId(operationId, heldContext), moves: plan }, (response, command) => {
              owner.envelope(response, "directory_publication", ["publication_operation_id", "result"]);
              if (response.publication_operation_id !== command.operation_id) throw new Error("Task retirement publication returned a different operation id");
              return publicationResult(response.result, command);
            });
          },
          reclaimDirectory(tombstone, proof, expected) {
            const checked = receipt(proof), manifest = preimage(expected);
            publicationId(checked.operation_id, heldContext);
            if (absolutePath(tombstone) !== checked.movement.destination || !isDeepStrictEqual(manifest, checked.movement.expected)
              || checked.outcome !== "applied") throw new Error("Task retirement reclaim requires the exact Applied destination and preimage");
            return owner.request({ operation: "reclaim_directory", transaction_id: heldContext.operation_id, tombstone, receipt: checked, expected: manifest }, (response, command) => {
              owner.envelope(response, "directory_reclaimed", ["tombstone", "receipt"]);
              if (response.tombstone !== command.tombstone || !isDeepStrictEqual(receipt(response.receipt), command.receipt)) {
                throw new Error("Task retirement reclamation returned a different tombstone receipt");
              }
            });
          },
        }));
        await owner.request({ operation: "assert_held" }, owner.proof);
      } catch (cause) {
        failed = true; error = cause;
      }
      try { await owner.close(); }
      catch (cause) { if (!failed) { failed = true; error = cause; } }
      if (failed) throw error;
      return result;
    },
  });
}

function openHold(binary, context) {
  const inventoryDigest = directoryInventory(context.directory_inventory);
  const child = spawn(binary, ["--hold-task-retirement"], { stdio: ["pipe", "pipe", "pipe"] });
  let pending = null, failure = null, ended = false, closing = false, releasing = false, buffer = "", stderr = "";
  let requests = Promise.resolve(), boundary = context.prior_proof?.ledger_boundary;
  const fail = (error) => {
    failure ??= error;
    if (pending) { const current = pending; pending = null; clearTimeout(current.timer); current.reject(failure); }
  };
  const closed = new Promise((resolve) => {
    child.on("error", (error) => { fail(error); resolve(); });
    const exited = (code, signal) => {
      ended = true;
      if (!releasing || code !== 0 || signal !== null) {
        fail(new Error(stderr.trim() || `Task retirement owner exited with code ${code}, signal ${signal}`));
      }
    };
    child.on("exit", exited);
    child.on("close", (code, signal) => {
      exited(code, signal);
      resolve();
    });
  });
  child.stdin.on("error", fail);
  child.stderr.setEncoding("utf8");
  child.stderr.on("error", fail);
  child.stderr.on("data", (chunk) => { stderr = (stderr + chunk).slice(-262144); });
  child.stdout.setEncoding("utf8");
  child.stdout.on("error", fail);
  child.stdout.on("end", () => {
    if (buffer || pending || !releasing) fail(new Error("Task retirement owner closed its proof output before completing release"));
  });
  child.stdout.on("data", (chunk) => {
    buffer += chunk;
    if (Buffer.byteLength(buffer) > MAX_FRAME_BYTES) {
      fail(new Error("Task retirement owner exceeded its response bound"));
      child.kill();
      return;
    }
    for (let end; (end = buffer.indexOf("\n")) !== -1;) {
      const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
      if (!pending) { fail(new Error("Task retirement owner emitted an unsolicited proof")); child.kill(); return; }
      const current = pending; pending = null; clearTimeout(current.timer);
      try {
        current.resolve(current.check(JSON.parse(line), current.command));
      } catch (error) { current.reject(error); fail(error); child.kill(); }
    }
  });
  const proof = (value) => {
    exact(value, PROOF_FIELDS, "held proof");
    exact(value.ledger_boundary, ["complete_offset", "sequence", "sha256"], "ledger boundary");
    const next = value.ledger_boundary;
    if (value.schema_version !== 2 || value.phase !== "held" || value.operation_id !== context.operation_id
      || value.selection_digest !== context.selection_digest || !validHash(value.selection_digest)
      || value.directory_inventory_digest !== inventoryDigest
      || !isDeepStrictEqual(value.interface_ids, context.selection?.interface_ids)
      || !isDeepStrictEqual(value.originals, context.originals?.map((item) => ({ file: item.file, expected_digest: item.expected_digest })))
      || !isDeepStrictEqual(value.retired, context.retired) || !unsigned(next.complete_offset) || !unsigned(next.sequence) || !validHash(next.sha256)
      || boundary && (next.complete_offset < boundary.complete_offset || next.sequence < boundary.sequence
        || next.complete_offset === boundary.complete_offset && !isDeepStrictEqual(next, boundary)
        || next.complete_offset > boundary.complete_offset && next.sequence <= boundary.sequence)) {
      throw new Error("Task retirement owner returned a different held proof or regressed ledger boundary");
    }
    boundary = { ...next };
    return value;
  };
  return {
    proof,
    envelope(value, phase, fields) {
      exact(value, ["schema_version", "phase", "operation_id", "proof", ...fields], "directory response");
      if (value.schema_version !== 1 || value.phase !== phase || value.operation_id !== context.operation_id) {
        throw new Error("Task retirement owner returned a different directory command response");
      }
      proof(value.proof);
    },
    request(value, check) {
      let encoded, command, encodingFailure;
      try {
        encoded = `${JSON.stringify(value)}\n`;
        if (Buffer.byteLength(encoded) > MAX_FRAME_BYTES) throw new Error("Task retirement command exceeds its frame bound");
        command = JSON.parse(encoded);
      } catch (error) { encodingFailure = error; }
      if (closing) return Promise.reject(new Error("Task retirement hold is closed"));
      const requested = requests.then(async () => {
        if (encodingFailure) throw encodingFailure;
        if (failure) throw failure;
        if (ended || releasing) throw new Error("Task retirement hold is closed");
        const response = await new Promise((resolve, reject) => {
          const timer = setTimeout(() => {
            fail(new Error("Task retirement is waiting for its interface processes or evidence"));
            child.kill();
          }, 35000);
          pending = { resolve, reject, timer, check, command };
          child.stdin.write(encoded, (error) => { if (error) fail(error); });
        });
        if (failure) throw failure;
        return response;
      });
      requests = requested.catch((error) => { fail(error); child.kill(); });
      return requested;
    },
    async close() {
      closing = true;
      await requests;
      const timer = setTimeout(() => {
        fail(new Error("Task retirement owner did not release its hold"));
        child.kill("SIGKILL");
      }, 3000);
      try {
        if (!ended) {
          releasing = true;
          child.stdin.end(`${JSON.stringify({ operation: "release" })}\n`);
        }
        await closed;
      } finally { clearTimeout(timer); }
      if (failure) throw failure;
    },
  };
}

module.exports = { createTaskRetirementBarrier };
