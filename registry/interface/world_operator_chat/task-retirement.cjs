const { spawn } = require("node:child_process");

function createTaskRetirementBarrier(binary, runCommand) {
  return Object.freeze({
    async verifyBefore(context) {
      const result = await runCommand(binary, "--verify-task-retirement", context);
      if (result?.ready !== true || Object.keys(result).length !== 1) {
        throw new Error("Task retirement preflight returned no exact readiness proof");
      }
    },
    async withHeldProof(context, operation) {
      const owner = openHold(binary);
      let result, error, failed = false;
      try {
        await owner.request(context);
        result = await operation(Object.freeze({ verify: () => owner.request({ operation: "assert_held" }) }));
        await owner.request({ operation: "assert_held" });
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

function openHold(binary) {
  const child = spawn(binary, ["--hold-task-retirement"], { stdio: ["pipe", "pipe", "pipe"] });
  let pending = null, failure = null, ended = false, releasing = false, buffer = "", stderr = "";
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
    if (!releasing) fail(new Error("Task retirement owner closed its proof output before release"));
  });
  child.stdout.on("data", (chunk) => {
    buffer += chunk;
    if (Buffer.byteLength(buffer) > 4 * 1024 * 1024) {
      fail(new Error("Task retirement owner exceeded its response bound"));
      child.kill();
      return;
    }
    for (let end; (end = buffer.indexOf("\n")) !== -1;) {
      const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
      if (!pending) { fail(new Error("Task retirement owner emitted an unsolicited proof")); child.kill(); return; }
      const current = pending; pending = null; clearTimeout(current.timer);
      try {
        const proof = JSON.parse(line);
        if (proof?.schema_version !== 1 || proof.phase !== "held") throw new Error("Task retirement owner returned no held proof");
        current.resolve(proof);
      } catch (error) { current.reject(error); fail(error); child.kill(); }
    }
  });
  return {
    async request(value) {
      if (failure) throw failure;
      if (ended || releasing || pending) throw new Error("Task retirement hold is closed or already verifying");
      const proof = await new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          fail(new Error("Task retirement is waiting for its interface processes or evidence"));
          child.kill();
        }, 35000);
        pending = { resolve, reject, timer };
        child.stdin.write(`${JSON.stringify(value)}\n`, (error) => { if (error) fail(error); });
      });
      if (failure) throw failure;
      return proof;
    },
    async close() {
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
