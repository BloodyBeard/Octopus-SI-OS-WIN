const { spawn } = require("node:child_process");

const FRAME_BYTES = 4 * 1024 * 1024;
const STDERR_BYTES = 256 * 1024;

function openNativeAdoptionBatch(binary, { timeoutMs, signal } = {}) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0) throw new Error("Native adoption requires a positive bounded request timeout");
  signal?.throwIfAborted();
  const child = spawn(binary, ["--plan-native-adoption-batch"], { stdio: ["pipe", "pipe", "pipe"] });
  let pending = null, failure = null, ended = false, outputEnded = false, finishing = false, acknowledged = false, endTimer;
  let buffer = Buffer.alloc(0), stderr = Buffer.alloc(0), sequence = 0;
  let signalError = null;
  let resolveExit;
  const exited = new Promise(resolve => { resolveExit = resolve; });
  const stop = name => {
    if (ended) return;
    try { if (!child.kill(name)) signalError = new Error(`Native adoption ${name} was not delivered`); }
    catch (error) { signalError = error; }
  };
  const fail = error => {
    clearTimeout(endTimer);
    failure ??= error;
    if (pending) { const current = pending; pending = null; clearTimeout(current.timer); current.reject(failure); }
    stop("SIGTERM");
  };
  const aborted = () => fail(signal.reason instanceof Error ? signal.reason : new Error("Native adoption cancelled"));
  signal?.addEventListener("abort", aborted, { once: true });
  child.on("error", fail);
  child.stdin.on("error", fail);
  child.stdout.on("error", fail);
  child.stderr.on("error", fail);
  child.stdout.on("end", () => {
    outputEnded = true;
    if (!failure && !ended && (!acknowledged || pending || buffer.length)) {
      endTimer = setTimeout(() => fail(new Error(stderr.toString("utf8").trim() || "Native adoption closed its output before completion")), 3000);
    }
  });
  child.stderr.on("data", chunk => { stderr = Buffer.concat([stderr, chunk]).subarray(-STDERR_BYTES); });
  child.on("close", (code, exitSignal) => {
    clearTimeout(endTimer);
    ended = true;
    if (code !== 0 || !acknowledged || pending || buffer.length) {
      fail(new Error(stderr.toString("utf8").trim() || `Native adoption ended without completion (${code ?? exitSignal})`));
    }
    resolveExit();
  });
  child.stdout.on("data", chunk => {
    buffer = Buffer.concat([buffer, chunk]);
    if (buffer.length > FRAME_BYTES) return fail(new Error("Native adoption response exceeds its frame bound"));
    const end = buffer.indexOf(10);
    if (end === -1) return;
    if (!pending || end !== buffer.length - 1) return fail(new Error("Native adoption emitted an unsolicited response"));
    const current = pending;
    pending = null;
    clearTimeout(current.timer);
    try {
      const value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(buffer.subarray(0, end)));
      buffer = Buffer.alloc(0);
      const field = current.operation === "finish" ? "finished" : "plan";
      if (!value || Array.isArray(value) || Object.keys(value).length !== 3
        || value.schema_version !== 1 || value.request_id !== current.id || !Object.hasOwn(value, field)
        || (field === "finished" ? value.finished !== true : !value.plan || typeof value.plan !== "object" || Array.isArray(value.plan)
          || value.plan.run_id !== current.runId)) throw new Error("Native adoption response changed its exact request identity or schema");
      if (field === "finished") acknowledged = true;
      current.resolve(value[field]);
    } catch (error) { current.reject(error); fail(error); }
  });
  function request(operation, value) {
    if (failure) return Promise.reject(failure);
    if (ended || outputEnded || finishing || pending) return Promise.reject(new Error("Native adoption requires one live sequential request"));
    const id = ++sequence;
    const frame = Buffer.from(`${JSON.stringify({ operation, request_id: id, ...value })}\n`);
    if (frame.length > FRAME_BYTES) {
      fail(new Error("Native adoption request exceeds its frame bound"));
      return Promise.reject(failure);
    }
    if (operation === "finish") finishing = true;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => fail(new Error("Native adoption request timed out")), timeoutMs);
      pending = { id, operation, runId: value.request?.run_id, resolve, reject, timer };
      child.stdin.write(frame, error => { if (error) fail(error); });
    });
  }
  const assertActive = () => {
    signal?.throwIfAborted();
    if (failure) throw failure;
    if (ended || outputEnded || finishing) throw new Error("Native adoption batch is no longer active");
  };
  async function waitForExit(milliseconds) {
    let timer;
    try { await Promise.race([exited, new Promise(resolve => { timer = setTimeout(resolve, milliseconds); })]); }
    finally { clearTimeout(timer); }
  }
  return {
    plan: value => request("plan", { request: value }),
    abort: fail,
    assertActive,
    async finish() {
      await request("finish", {});
      child.stdin.end();
    },
    async close() {
      signal?.removeEventListener("abort", aborted);
      if (!ended && !acknowledged) fail(new Error("Native adoption closed before completion"));
      await waitForExit(3000);
      if (!ended) {
        fail(new Error("Native adoption process did not close"));
        stop("SIGKILL");
        await waitForExit(3000);
      }
      if (!ended) {
        child.stdin.destroy(); child.stdout.destroy(); child.stderr.destroy(); child.unref();
        throw new Error(`Native adoption process closure unconfirmed${signalError ? `: ${signalError.message}` : ""}`, { cause: failure });
      }
      if (failure) throw failure;
    },
  };
}

module.exports = { openNativeAdoptionBatch };
