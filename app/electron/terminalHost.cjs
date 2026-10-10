const { EventEmitter } = require("node:events");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const crypto = require("node:crypto");
const pty = require("node-pty");

const MAX_TRANSCRIPT_BYTES = 256 * 1024;
const MAX_PREPARED_COMMAND_BYTES = 4 * 1024;
const MAX_INPUT_BYTES = 16 * 1024;

function createTerminalHost() {
  return new TerminalHost();
}

class TerminalHost extends EventEmitter {
  constructor() {
    super();
    this.sessions = new Map();
  }

  open(ownerValue, request = {}) {
    const owner = terminalOwner(ownerValue);
    const terminalId = terminalIdValue(request.terminalId);
    const cols = dimension(request.cols, 100);
    const rows = dimension(request.rows, 32);
    const key = terminalSessionKey(owner, terminalId);
    let session = this.sessions.get(key);
    if (!session?.running) {
      session = this.createSession(owner, terminalId, cols, rows, request);
    }
    session.cols = cols;
    session.rows = rows;
    session.process.resize(cols, rows);
    return this.snapshot(session);
  }

  openExisting(ownerValue, request = {}) {
    const owner = terminalOwner(ownerValue);
    const terminalId = terminalIdValue(request.terminalId);
    const session = this.sessions.get(terminalSessionKey(owner, terminalId));
    if (!session?.running) return null;
    session.cols = dimension(request.cols, session.cols);
    session.rows = dimension(request.rows, session.rows);
    session.process.resize(session.cols, session.rows);
    return this.snapshot(session);
  }

  write(ownerValue, terminalId, sessionId, data) {
    const session = this.requireSession(terminalOwner(ownerValue), terminalId, sessionId);
    if (typeof data !== "string" || Buffer.byteLength(data) > MAX_INPUT_BYTES) {
      throw new Error("terminal input must be a string no larger than 16 KiB");
    }
    session.process.write(data);
    if (session.awaitingLaunchSubmission && session.commandTyped && /[\r\n]/.test(data)) {
      this.completeLaunchSubmission(session);
    }
    return this.snapshot(session);
  }

  resize(ownerValue, terminalId, sessionId, request = {}) {
    const session = this.requireSession(terminalOwner(ownerValue), terminalId, sessionId);
    session.cols = dimension(request.cols, session.cols);
    session.rows = dimension(request.rows, session.rows);
    session.process.resize(session.cols, session.rows);
    return this.snapshot(session);
  }

  close(ownerValue, terminalId) {
    const owner = terminalOwner(ownerValue);
    const id = terminalIdValue(terminalId);
    const key = terminalSessionKey(owner, id);
    const session = this.sessions.get(key);
    if (!session) return false;
    this.stopSession(key, session);
    return true;
  }

  closeOwner(ownerToken) {
    if (typeof ownerToken !== "string" || !ownerToken) return 0;
    return this.closeMatching((session) => session.owner.scope === "access" && session.owner.token === ownerToken);
  }

  closeSender(senderId) {
    if (!Number.isSafeInteger(senderId) || senderId <= 0) return 0;
    return this.closeMatching((session) => session.owner.senderId === senderId);
  }

  interruptAll() {
    return this.interruptMatching(() => true);
  }

  interruptLaunch() {
    return this.interruptMatching((session) => session.owner.scope === "launch");
  }

  runningLaunchCount() {
    return [...this.sessions.values()].filter((session) => (
      session.running && session.owner.scope === "launch"
    )).length;
  }

  prepareLaunchCommand() {
    let prepared = 0;
    for (const session of this.sessions.values()) {
      if (!session.running || session.owner.scope !== "launch" || !session.command) continue;
      this.rejectLaunchSubmission(session, new Error("the prepared launch command was replaced"));
      session.awaitingLaunchSubmission = true;
      session.commandTyped = false;
      this.scheduleCommandTyping(session, 160);
      prepared += 1;
    }
    return prepared;
  }

  waitForLaunchSubmission() {
    const sessions = [...this.sessions.values()].filter((session) => (
      session.running && session.owner.scope === "launch"
    ));
    if (sessions.length !== 1 || !sessions[0].awaitingLaunchSubmission) {
      return Promise.reject(new Error("the boot terminal has no prepared command awaiting submission"));
    }
    return new Promise((resolve, reject) => {
      sessions[0].launchSubmissionWaiters.add({ resolve, reject });
    });
  }

  submitPreparedLaunchCommand() {
    const sessions = [...this.sessions.values()].filter((session) => (
      session.running && session.owner.scope === "launch"
    ));
    if (sessions.length !== 1 || !sessions[0].awaitingLaunchSubmission) {
      return Promise.reject(new Error("the boot terminal has no prepared command awaiting submission"));
    }
    const session = sessions[0];
    const submitted = new Promise((resolve, reject) => {
      session.launchSubmissionWaiters.add({ resolve, reject });
    });
    session.submitLaunchWhenTyped = true;
    this.submitLaunchCommandIfReady(session);
    return submitted;
  }

  interruptMatching(matches) {
    let interrupted = 0;
    for (const session of this.sessions.values()) {
      if (!session.running || !matches(session)) continue;
      session.process.write("\x03");
      interrupted += 1;
    }
    return interrupted;
  }

  async terminate(timeoutMs = 2_000) {
    const running = [...this.sessions.values()].filter((session) => session.running);
    const exits = running.map((session) => waitForExit(this, session.id));
    for (const session of running) {
      try {
        session.process.kill("SIGTERM");
      } catch {
      }
    }
    await Promise.race([Promise.all(exits), delay(timeoutMs)]);
    this.closeAll();
  }

  closeAll() {
    this.closeMatching(() => true, "SIGKILL");
  }

  closeMatching(matches, signal = "SIGTERM") {
    let closed = 0;
    for (const [key, session] of [...this.sessions]) {
      if (!matches(session)) continue;
      this.stopSession(key, session, signal);
      closed += 1;
    }
    return closed;
  }

  stopSession(key, session, signal = "SIGTERM") {
    if (this.sessions.get(key)?.id !== session.id) return;
    this.sessions.delete(key);
    if (session.typeTimer) clearTimeout(session.typeTimer);
    session.typeTimer = null;
    this.rejectLaunchSubmission(session, new Error("the boot terminal closed before command submission"));
    const wasRunning = session.running;
    session.running = false;
    if (wasRunning) {
      try {
        session.process.kill(signal);
      } catch {
      }
    }
    try {
      session.process.destroy();
    } catch {
    }
  }

  createSession(owner, terminalId, cols, rows, request) {
    const cwd = request.cwd == null || request.cwd === ""
      ? os.homedir()
      : absoluteExistingDirectory(request.cwd, "terminal working directory");
    const env = environmentValue(request.env);
    const preparedCommand = preparedCommandValue(request.preparedCommand);
    const shell = resolveShell();
    const session = {
      id: crypto.randomUUID(),
      key: terminalSessionKey(owner, terminalId),
      owner,
      terminalId,
      cwd,
      command: preparedCommand,
      cols,
      rows,
      process: null,
      running: true,
      pid: null,
      exitCode: null,
      signal: null,
      error: null,
      transcript: "",
      commandTyped: preparedCommand.length === 0,
      awaitingLaunchSubmission: false,
      launchSubmissionWaiters: new Set(),
      submitLaunchWhenTyped: false,
      typeTimer: null,
    };
    const child = pty.spawn(shell.file, shell.args, {
      name: "xterm-256color",
      cols,
      rows,
      cwd,
      env: {
        ...process.env,
        TERM: "xterm-256color",
        COLORTERM: "truecolor",
        ...env,
      },
    });
    this.sessions.set(session.key, session);
    session.process = child;
    session.pid = child.pid;
    child.onData((data) => {
      this.append(session, data);
      this.scheduleCommandTyping(session, 160);
    });
    child.onExit(({ exitCode, signal }) => {
      if (session.typeTimer) clearTimeout(session.typeTimer);
      session.typeTimer = null;
      this.rejectLaunchSubmission(session, new Error("the boot terminal exited before command submission"));
      if (this.sessions.get(session.key)?.id !== session.id) return;
      session.running = false;
      session.pid = null;
      session.exitCode = exitCode;
      session.signal = signal;
      this.emit("exit", {
        owner: session.owner,
        terminalId,
        sessionId: session.id,
        exitCode: session.exitCode,
        signal: session.signal,
        error: session.error,
      });
    });
    this.scheduleCommandTyping(session, 1_500);
    return session;
  }

  resolveLaunchSubmission(session) {
    const waiters = [...session.launchSubmissionWaiters];
    session.launchSubmissionWaiters.clear();
    for (const waiter of waiters) waiter.resolve();
  }

  rejectLaunchSubmission(session, error) {
    session.submitLaunchWhenTyped = false;
    const waiters = [...session.launchSubmissionWaiters];
    session.launchSubmissionWaiters.clear();
    for (const waiter of waiters) waiter.reject(error);
  }

  scheduleCommandTyping(session, delayMs) {
    if (session.commandTyped || !session.running) return;
    if (session.typeTimer) clearTimeout(session.typeTimer);
    session.typeTimer = setTimeout(() => {
      session.typeTimer = null;
      if (session.commandTyped || !session.running) return;
      if (this.sessions.get(session.key)?.id !== session.id) return;
      session.commandTyped = true;
      session.process.write(session.command);
      this.submitLaunchCommandIfReady(session);
    }, delayMs);
  }

  submitLaunchCommandIfReady(session) {
    if (!session.running || !session.awaitingLaunchSubmission || !session.commandTyped || !session.submitLaunchWhenTyped) return;
    session.process.write("\r");
    this.completeLaunchSubmission(session);
  }

  completeLaunchSubmission(session) {
    session.awaitingLaunchSubmission = false;
    session.submitLaunchWhenTyped = false;
    this.resolveLaunchSubmission(session);
  }

  append(session, data) {
    if (this.sessions.get(session.key)?.id !== session.id || !data) return;
    session.transcript = trimTranscript(`${session.transcript}${data}`);
    this.emit("data", { owner: session.owner, terminalId: session.terminalId, sessionId: session.id, data });
  }

  requireSession(owner, terminalId, sessionId) {
    const id = terminalIdValue(terminalId);
    const session = this.sessions.get(terminalSessionKey(owner, id));
    if (!session || session.id !== sessionId) {
      throw new Error("terminal session is missing or stale");
    }
    if (!session.running) throw new Error("terminal shell has exited");
    return session;
  }

  snapshot(session) {
    return {
      terminalId: session.terminalId,
      sessionId: session.id,
      command: session.command,
      cwd: session.cwd,
      transcript: session.transcript,
      running: session.running,
      pid: session.pid,
      exitCode: session.exitCode,
      signal: session.signal,
      error: session.error,
    };
  }
}

function terminalOwner(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("terminal owner is required");
  }
  const scope = value.scope === "launch" ? "launch" : value.scope === "access" ? "access" : "";
  const senderId = Number(value.senderId);
  const token = typeof value.token === "string" ? value.token : "";
  if (!scope || !Number.isSafeInteger(senderId) || senderId <= 0 || (scope === "access" && !token)) {
    throw new Error("terminal owner is invalid");
  }
  return { scope, senderId, token };
}

function terminalSessionKey(owner, terminalId) {
  return `${owner.scope}\0${owner.senderId}\0${owner.token}\0${terminalId}`;
}

function resolveShell() {
  if (process.platform === "win32") {
    return { file: process.env.COMSPEC || "powershell.exe", args: [] };
  }
  const file = process.env.SHELL || "/bin/zsh";
  return { file: absoluteExistingFile(file, "user shell"), args: [] };
}

function waitForExit(host, sessionId) {
  return new Promise((resolve) => {
    const onExit = (event) => {
      if (event.sessionId !== sessionId) return;
      host.off("exit", onExit);
      resolve(true);
    };
    host.on("exit", onExit);
  });
}

function terminalIdValue(value) {
  if (typeof value !== "string" || !/^[a-z0-9][a-z0-9_.-]{0,63}$/i.test(value)) {
    throw new Error("terminal id must be a short alphanumeric identifier");
  }
  return value;
}

function environmentValue(value) {
  if (value == null) return {};
  if (typeof value !== "object" || Array.isArray(value)) {
    throw new Error("terminal env must be an object of string values");
  }
  const env = {};
  for (const [name, entry] of Object.entries(value)) {
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
      throw new Error(`terminal env name '${name}' is not a valid environment variable name`);
    }
    if (typeof entry !== "string") {
      throw new Error(`terminal env '${name}' must be a string`);
    }
    env[name] = entry;
  }
  return env;
}

function preparedCommandValue(value) {
  if (value == null) return "";
  if (typeof value !== "string" || Buffer.byteLength(value) > MAX_PREPARED_COMMAND_BYTES) {
    throw new Error("prepared command must be a string no larger than 4 KiB");
  }
  for (const character of value) {
    const code = character.codePointAt(0);
    if (code < 0x20 || code === 0x7f) {
      throw new Error("prepared command must not contain control characters; it is typed, never executed");
    }
  }
  return value;
}

function absolutePath(value, label) {
  if (typeof value !== "string" || !value.trim() || !path.isAbsolute(value)) {
    throw new Error(`${label} must be an absolute path`);
  }
  return path.normalize(value);
}

function absoluteExistingFile(value, label) {
  const candidate = absolutePath(value, label);
  const stat = fs.statSync(candidate);
  if (!stat.isFile()) throw new Error(`${label} '${candidate}' is not a file`);
  return candidate;
}

function absoluteExistingDirectory(value, label) {
  const candidate = absolutePath(value, label);
  const stat = fs.statSync(candidate);
  if (!stat.isDirectory()) throw new Error(`${label} '${candidate}' is not a directory`);
  return candidate;
}

function dimension(value, fallback) {
  return Number.isSafeInteger(value) && value > 0 && value <= 1000 ? value : fallback;
}

function trimTranscript(value) {
  const bytes = Buffer.byteLength(value);
  if (bytes <= MAX_TRANSCRIPT_BYTES) return value;
  const buffer = Buffer.from(value);
  return buffer.subarray(bytes - MAX_TRANSCRIPT_BYTES).toString("utf8");
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

module.exports = { createTerminalHost };
