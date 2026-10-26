const { EventEmitter } = require("node:events");
const fs = require("node:fs/promises");
const net = require("node:net");
const { localSocketAddress } = require("./localSocket.cjs");
const path = require("node:path");

const RUNTIME_INSTANCE_FIELDS = [
  "launch_owner",
  "phase",
  "pid",
  "schema_version",
  "started_at_unix_ms",
  "updated_at_unix_ms",
];

function createRuntimeHost(options) {
  return new RuntimeHost(options);
}

class RuntimeHost extends EventEmitter {
  constructor(options) {
    super();
    this.options = options;
    this.timer = null;
    this.probePromise = null;
    this.state = {
      phase: "probing",
      ready: false,
      ownership: "none",
      pid: null,
      home: options.home(),
      sourceRegistry: options.sourceRegistry(),
      runtimeBinary: options.runtimeBinary(),
      repoRoot: options.repoRoot?.() ?? "",
      launchCwd: options.launchCwd?.() ?? options.repoRoot?.() ?? "",
      launchCommand: options.launchCommand?.() ?? "",
      launchEnv: stringMap(options.launchEnv?.() ?? {}, "runtime launch environment"),
      endpoint: options.endpoint(),
      error: null,
      updatedAtMs: Date.now(),
    };
  }

  snapshot() {
    return { ...this.state, launchEnv: { ...this.state.launchEnv } };
  }

  observe(intervalMs = 350) {
    if (this.timer) return;
    void this.probe();
    this.timer = setInterval(() => void this.probe(), intervalMs);
  }

  close() {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }

  async waitUntilStopped(timeoutMs = 8_000) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      await this.probe();
      if (
        this.state.phase === "unmanaged"
        && !this.state.ready
        && this.state.ownership === "none"
        && this.state.pid == null
        && !this.state.error
      ) return true;
      await delay(100);
    }
    return false;
  }

  async waitUntilReady(timeoutMs = 300_000) {
    const deadline = Date.now() + timeoutMs;
    let observedRuntimePid = false;
    while (Date.now() < deadline) {
      await this.probe();
      if (this.state.ready) return this.snapshot();
      if (this.state.pid != null) observedRuntimePid = true;
      if (
        observedRuntimePid
        && this.state.phase === "unmanaged"
        && this.state.ownership === "none"
        && this.state.pid == null
        && !this.state.error
      ) {
        throw new Error("Octopus runtime exited before becoming ready");
      }
      await delay(100);
    }
    throw new Error(`Octopus runtime did not become ready within ${Math.round(timeoutMs / 1000)} seconds after terminal submission`);
  }

  probe() {
    if (!this.probePromise) {
      this.probePromise = this.probeInner()
        .catch((error) => {
          this.transition("degraded", {
            ready: false,
            ownership: "none",
            pid: null,
            error: formatError(error),
          });
          return this.snapshot();
        })
        .finally(() => {
          this.probePromise = null;
        });
    }
    return this.probePromise;
  }

  async probeInner() {
    const home = absolutePath(this.options.home(), "runtime home");
    const sourceRegistry = absolutePath(this.options.sourceRegistry(), "source registry");
    const runtimeBinary = absolutePath(this.options.runtimeBinary(), "runtime binary");
    const endpoint = absolutePath(this.options.endpoint(), "runtime endpoint");
    requireUnixSocketPath(endpoint, "runtime endpoint");
    requireUnixSocketPath(
      path.join(home, "app-server-control", `${"0".repeat(24)}.sock`),
      "per-interface artifact socket",
    );
    const base = {
      home,
      sourceRegistry,
      runtimeBinary,
      repoRoot: this.options.repoRoot?.() ?? "",
      launchCwd: absolutePath(
        this.options.launchCwd?.() ?? this.options.repoRoot?.() ?? path.dirname(runtimeBinary),
        "runtime launch directory",
      ),
      launchCommand: requiredString(this.options.launchCommand?.(), "runtime launch command"),
      launchEnv: stringMap(this.options.launchEnv?.() ?? {}, "runtime launch environment"),
      endpoint,
    };

    const instance = await readRuntimeInstance(home);
    if (instance && processIsAlive(instance.pid)) {
      const ready = instance.phase === "ready";
      this.transition(ready ? "ready" : normalizeRuntimePhase(instance.phase), {
        ...base,
        ready,
        ownership: "external",
        pid: instance.pid,
        error: null,
      });
      return this.snapshot();
    }

    if (await socketAccepts(endpoint, 200)) {
      this.transition("degraded", {
        ...base,
        ready: false,
        ownership: "external",
        pid: null,
        error: "runtime endpoint accepts connections without coherent live runtime-instance proof",
      });
      return this.snapshot();
    }

    this.transition("unmanaged", {
      ...base,
      ready: false,
      ownership: "none",
      pid: null,
      error: null,
    });
    return this.snapshot();
  }

  transition(phase, patch = {}) {
    const next = {
      ...this.state,
      ...patch,
      phase,
      updatedAtMs: Date.now(),
    };
    if (sameState(this.state, next)) return;
    this.state = next;
    this.emit("state", this.snapshot());
  }
}

async function readRuntimeInstance(home) {
  const file = path.join(home, "state", "runtime-instance", "state.json");
  try {
    const value = JSON.parse(await fs.readFile(file, "utf8"));
    const keys = value && typeof value === "object" && !Array.isArray(value)
      ? Object.keys(value).sort()
      : [];
    if (
      keys.length !== RUNTIME_INSTANCE_FIELDS.length
      || keys.some((key, index) => key !== RUNTIME_INSTANCE_FIELDS[index])
      || value.schema_version !== 3
      || !Number.isSafeInteger(value.pid)
      || value.pid <= 0
      || value.pid > 0xffff_ffff
      || typeof value.phase !== "string"
      || !/^[a-z_]+$/.test(value.phase)
      || !Number.isSafeInteger(value.started_at_unix_ms)
      || value.started_at_unix_ms <= 0
      || !Number.isSafeInteger(value.updated_at_unix_ms)
      || value.updated_at_unix_ms < value.started_at_unix_ms
      || value.launch_owner !== null
    ) {
      throw new Error(`runtime instance file '${file}' is malformed`);
    }
    return value;
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

function processIsAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH") return false;
    if (error?.code === "EPERM") return true;
    throw error;
  }
}

function normalizeRuntimePhase(phase) {
  const value = typeof phase === "string" ? phase : "starting_runtime";
  if (value === "installing_registry") return "registry_install";
  if (value === "registry_ready") return "registry_ready";
  if (value === "state_ready") return "state_ready";
  return "starting_runtime";
}

function socketAccepts(socketPath, timeoutMs) {
  return new Promise((resolve) => {
    let settled = false;
    const socket = net.createConnection(localSocketAddress(socketPath));
    const finish = (value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      resolve(value);
    };
    const timer = setTimeout(() => finish(false), timeoutMs);
    socket.once("connect", () => finish(true));
    socket.once("error", () => finish(false));
  });
}

function absolutePath(value, field) {
  if (typeof value !== "string" || !value.trim() || !path.isAbsolute(value)) {
    throw new Error(`${field} must be an absolute path`);
  }
  return path.normalize(value);
}

function requiredString(value, field) {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${field} must be a non-empty string`);
  return value;
}

function stringMap(value, field) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${field} must be an object`);
  }
  const result = {};
  for (const [key, entry] of Object.entries(value)) {
    if (!key || typeof entry !== "string") throw new Error(`${field} must contain string values`);
    result[key] = entry;
  }
  return result;
}

function requireUnixSocketPath(value, field) {
  if (process.platform === "win32") return;
  const maximum = process.platform === "darwin" ? 103 : 107;
  const bytes = Buffer.byteLength(value);
  if (bytes > maximum) {
    throw new Error(`${field} is ${bytes} bytes but ${process.platform} allows at most ${maximum}; choose a shorter OCTOPUS_HOME`);
  }
}

function formatError(error) {
  return error instanceof Error ? error.message : String(error);
}

function sameState(left, right) {
  return Object.keys(right).every((key) => {
    if (key === "updatedAtMs") return true;
    const previous = left[key];
    const next = right[key];
    if (previous && next && typeof previous === "object" && typeof next === "object") {
      return JSON.stringify(previous) === JSON.stringify(next);
    }
    return previous === next;
  });
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

module.exports = { createRuntimeHost };
