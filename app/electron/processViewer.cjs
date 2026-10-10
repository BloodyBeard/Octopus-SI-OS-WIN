const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const net = require("node:net");
const path = require("node:path");

const CONNECT_TIMEOUT_MS = 7_000;
const COMMAND_TIMEOUT_MS = 7_000;
const MAX_FRAME_BYTES = 1024 * 1024;

function createProcessViewerManager(options) {
  if (typeof options?.resolveSurface !== "function") throw new Error("Process viewer requires a projected surface resolver");
  const sessions = new Map();
  const watchedSenders = new WeakSet();

  async function open(ownerToken, sender, request) {
    const workerEnvironmentId = requiredId(request?.workerEnvironmentId, "workerEnvironmentId");
    const surfaceId = requiredId(request?.surfaceId, "surfaceId");
    const descriptor = await options.resolveSurface(workerEnvironmentId, surfaceId);
    validateProcessSurface(descriptor, workerEnvironmentId, true);
    if (!ownerToken || !sender || typeof sender.id !== "number" || sender.isDestroyed()) {
      throw new Error("Process viewer requires an active client access session");
    }
    if (!watchedSenders.has(sender)) {
      watchedSenders.add(sender);
      const senderId = sender.id;
      sender.once("destroyed", () => closeSender(senderId));
    }
    for (const session of sessions.values()) {
      if (session.ownerToken === ownerToken
        && session.senderId === sender.id
        && session.workerEnvironmentId === workerEnvironmentId
        && session.surfaceId === surfaceId) {
        session.close("replaced");
      }
    }
    const credential = await readViewerCredential(descriptor.data.auth_file);
    const socketMetadata = await fs.lstat(descriptor.viewerUrl);
    if (!socketMetadata.isSocket() || socketMetadata.isSymbolicLink()
      || (socketMetadata.mode & 0o077) !== 0) {
      throw new Error("Process viewer endpoint is not a private local socket");
    }
    const sessionId = crypto.randomBytes(18).toString("base64url");
    const session = new ProcessViewerSession({
      credential,
      descriptor,
      ownerToken,
      sender,
      sessionId,
      surfaceId,
      workerEnvironmentId,
      onClosed: () => sessions.delete(sessionId),
      resolveSurface: options.resolveSurface,
    });
    sessions.set(sessionId, session);
    try {
      await session.connect();
      return session.snapshot();
    } catch (error) {
      session.close("connection failed");
      throw error;
    }
  }

  async function action(ownerToken, sender, request) {
    return requireSession(ownerToken, sender, request?.sessionId).action(request);
  }

  function close(ownerToken, sender, request) {
    const session = requireSession(ownerToken, sender, request?.sessionId);
    session.close("closed by client");
    return { sessionId: session.sessionId, closed: true };
  }

  function requireSession(ownerToken, sender, sessionIdValue) {
    const sessionId = requiredId(sessionIdValue, "sessionId");
    const session = sessions.get(sessionId);
    if (!ownerToken || !sender || sender.isDestroyed()
      || !session || session.ownerToken !== ownerToken || session.senderId !== sender.id) {
      throw new Error("Process viewer session is unavailable");
    }
    return session;
  }

  function closeOwner(ownerToken) {
    for (const session of [...sessions.values()]) {
      if (session.ownerToken === ownerToken) session.close("access session locked");
    }
  }

  function closeSender(senderId) {
    for (const session of [...sessions.values()]) {
      if (session.senderId === senderId) session.close("window closed");
    }
  }

  function closeAll() {
    for (const session of [...sessions.values()]) session.close("client closed");
  }

  async function revalidate() {
    await Promise.all([...sessions.values()].map(async (session) => {
      try {
        const descriptor = await options.resolveSurface(session.workerEnvironmentId, session.surfaceId);
        validateProcessSurface(descriptor, session.workerEnvironmentId, session.state?.processId === "");
        if (descriptor.viewerUrl !== session.descriptor.viewerUrl
          || descriptor.data.auth_file !== session.descriptor.data.auth_file
          || (!session.rebinding && (
            Number(descriptor.data.generation) !== Number(session.state?.generation)
            || stringValue(descriptor.data.process_id) !== stringValue(session.state?.processId)
          ))) {
          session.fail("The projected process room identity changed.");
        } else if (Number(descriptor.data.generation) === Number(session.state?.generation)
          && stringValue(descriptor.data.process_id) === stringValue(session.state?.processId)) {
          session.descriptor = descriptor;
        }
      } catch {
        session.fail("The projected process terminal was withdrawn.");
      }
    }));
  }

  return { action, close, closeAll, closeOwner, open, revalidate };
}

class ProcessViewerSession {
  constructor(options) {
    Object.assign(this, options);
    this.socket = null;
    this.buffer = Buffer.alloc(0);
    this.decoder = new TextDecoder();
    this.pending = new Map();
    this.state = null;
    this.closed = false;
    this.connected = false;
    this.connectWaiter = null;
  }

  connect() {
    if (this.closed || this.socket) return Promise.reject(new Error("Process viewer cannot connect twice"));
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error("Process viewer connection timed out"));
        this.close("connection timed out");
      }, CONNECT_TIMEOUT_MS);
      this.connectWaiter = {
        resolve: () => { clearTimeout(timeout); resolve(); },
        reject: (error) => { clearTimeout(timeout); reject(error); },
      };
      const socket = net.createConnection({ path: this.descriptor.viewerUrl });
      this.socket = socket;
      socket.setNoDelay(true);
      socket.on("connect", () => {
        this.connected = true;
        this.send({
          schema_version: 1,
          worker_environment_id: this.workerEnvironmentId,
          generation: Number(this.descriptor.data.generation),
          process_id: stringValue(this.descriptor.data.process_id) || null,
          token: this.credential,
        });
      });
      socket.on("data", (chunk) => this.receive(chunk));
      socket.on("error", (error) => this.fail(error.message || "Process viewer connection failed"));
      socket.on("close", () => {
        if (!this.closed) this.fail("Process viewer connection closed");
      });
    });
  }

  receive(chunk) {
    if (this.closed) return;
    this.buffer = Buffer.concat([this.buffer, chunk]);
    if (this.buffer.length > MAX_FRAME_BYTES) {
      this.fail("Process viewer frame exceeded its size limit");
      return;
    }
    for (;;) {
      const newline = this.buffer.indexOf(0x0a);
      if (newline < 0) return;
      const line = this.buffer.subarray(0, newline);
      this.buffer = this.buffer.subarray(newline + 1);
      let message;
      try {
        message = JSON.parse(line.toString("utf8"));
      } catch {
        this.fail("Process viewer returned malformed data");
        return;
      }
      try {
        this.handle(message);
      } catch (error) {
        this.fail(error instanceof Error && error.message
          ? error.message
          : "Process viewer returned an invalid message");
        return;
      }
      if (this.closed) return;
    }
  }

  handle(message) {
    if (!message || typeof message !== "object" || Array.isArray(message)) {
      this.fail("Process viewer returned an invalid message");
      return;
    }
    if (message.type === "state") {
      const nextState = normalizedState(message.state);
      this.validateStateIdentity(nextState, false);
      this.state = nextState;
      this.connectWaiter?.resolve();
      this.connectWaiter = null;
      this.publish({ type: "state", state: this.state });
      return;
    }
    if (message.type === "response") {
      if (typeof message.request_id !== "string" || !message.request_id) {
        throw new Error("Process viewer returned an invalid command response");
      }
      const pending = this.pending.get(message.request_id);
      if (!pending) {
        this.fail("Process viewer returned an unknown command response");
        return;
      }
      if (typeof message.ok !== "boolean") {
        throw new Error("Process viewer returned an invalid command response");
      }
      let nextState = null;
      if (message.ok === true) {
        nextState = normalizedState(message.state);
        this.validateStateIdentity(nextState, pending.rebind);
      }
      this.pending.delete(message.request_id);
      clearTimeout(pending.timeout);
      if (message.ok === true) {
        this.state = nextState;
        if (pending.rebind) {
          this.descriptor = {
            ...this.descriptor,
            readOnly: !nextState.tty,
            data: {
              ...this.descriptor.data,
              generation: nextState.generation,
              process_id: nextState.processId,
              tty: nextState.tty,
            },
          };
          this.decoder = new TextDecoder();
          this.publish({ type: "reset" });
        }
        pending.resolve({ state: this.state });
      } else {
        pending.reject(new Error(stringValue(message.error) || "Process viewer command failed"));
      }
      if (pending.rebind) {
        this.rebinding = false;
        if (message.ok !== true) void this.revalidateAfterRejectedRebind();
      }
      return;
    }
    if (message.type === "output") {
      if (!Number.isSafeInteger(message.seq) || message.seq < 0
        || !["stdout", "stderr", "pty"].includes(message.stream)) {
        throw new Error("Process viewer returned an invalid output event");
      }
      const data = this.decoder.decode(decodeBase64(message.data_base64), { stream: true });
      this.publish({ type: "output", data, seq: message.seq });
      return;
    }
    if (message.type === "history_truncated") {
      const retainedFromSeq = message.retained_from_seq;
      if (retainedFromSeq !== null
        && (!Number.isSafeInteger(retainedFromSeq) || retainedFromSeq < 0)) {
        throw new Error("Process viewer returned an invalid retained output boundary");
      }
      this.publish({ type: "history_truncated", retainedFromSeq });
      return;
    }
    if (message.type === "failure") {
      this.fail(stringValue(message.message) || "Process viewer reported an output gap");
      return;
    }
    this.fail("Process viewer returned an unknown message type");
  }

  validateStateIdentity(state, allowRebind) {
    if (state.workerEnvironmentId !== this.workerEnvironmentId) {
      throw new Error("Process viewer returned a different Worker Environment identity");
    }
    if (!this.state && (
      state.generation !== Number(this.descriptor.data.generation)
      || state.processId !== stringValue(this.descriptor.data.process_id)
    )) {
      throw new Error("Process viewer returned a different projected process identity");
    }
    if (!allowRebind && this.state
      && (state.generation !== this.state.generation || state.processId !== this.state.processId)) {
      throw new Error("Process viewer changed process identity without an acknowledged rebind");
    }
    if (!allowRebind && this.state && state.tty !== this.state.tty) {
      throw new Error("Process viewer changed PTY identity without an acknowledged rebind");
    }
    if (allowRebind && this.state && (
      state.generation !== this.state.generation + 1 || !state.processId
    )) {
      throw new Error("Process viewer returned an invalid process rebind");
    }
  }

  async revalidateAfterRejectedRebind() {
    try {
      const descriptor = await this.resolveSurface(this.workerEnvironmentId, this.surfaceId);
      validateProcessSurface(descriptor, this.workerEnvironmentId, this.state?.processId === "");
      if (Number(descriptor.data.generation) !== Number(this.state?.generation)
        || stringValue(descriptor.data.process_id) !== stringValue(this.state?.processId)) {
        this.fail("The process identity changed while a viewer command was rejected.");
        return;
      }
      this.descriptor = descriptor;
    } catch {
      this.fail("The projected process terminal was withdrawn.");
    }
  }

  action(request) {
    if (this.closed || !this.connected) return Promise.reject(new Error("Process viewer is disconnected"));
    const action = requiredAction(request?.action);
    const mayStartFirstShell = action === "start_new_shell"
      && !this.state?.processId
      && this.state?.startNewShellAvailable === true;
    if (!mayStartFirstShell && (this.descriptor.readOnly || this.state?.tty !== true)) {
      throw new Error("This process has no interactive PTY; its projected Output is read-only");
    }
    const rebind = action === "restart" || action === "start_new_shell";
    const requestId = crypto.randomBytes(12).toString("base64url");
    const command = { action, request_id: requestId };
    if (action === "write") {
      const data = String(request?.data ?? "");
      if (Buffer.byteLength(data, "utf8") > 65_536) throw new Error("Terminal input exceeds 65536 bytes");
      command.data_base64 = Buffer.from(data, "utf8").toString("base64");
    }
    if (action === "resize") {
      command.cols = boundedInteger(request?.cols, 2, 500, "cols");
      command.rows = boundedInteger(request?.rows, 1, 300, "rows");
    }
    if (action === "restart" && this.state?.restartAvailable !== true) {
      throw new Error("The projected process has no safe launch to restart");
    }
    if (action === "start_new_shell" && this.state?.startNewShellAvailable !== true) {
      throw new Error("A new shell cannot start while this process is running");
    }
    if (["write", "resize", "interrupt", "terminate"].includes(action) && this.state?.status !== "running") {
      throw new Error(`The projected process is ${this.state?.status || "unavailable"}`);
    }
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(requestId);
        if (rebind) this.rebinding = false;
        reject(new Error("Process viewer command timed out"));
      }, COMMAND_TIMEOUT_MS);
      if (rebind) this.rebinding = true;
      this.pending.set(requestId, { rebind, reject, resolve, timeout });
      try {
        this.send(command);
      } catch (error) {
        this.pending.delete(requestId);
        if (rebind) this.rebinding = false;
        clearTimeout(timeout);
        reject(error);
      }
    });
  }

  send(value) {
    if (!this.socket || this.socket.destroyed) throw new Error("Process viewer is disconnected");
    const frame = Buffer.from(`${JSON.stringify(value)}\n`, "utf8");
    if (frame.length > MAX_FRAME_BYTES) throw new Error("Process viewer command exceeds its size limit");
    this.socket.write(frame);
  }

  snapshot() {
    return { sessionId: this.sessionId, state: this.state };
  }

  publish(event) {
    if (!this.sender.isDestroyed()) {
      this.sender.send("octopus:process-viewer-event", { sessionId: this.sessionId, event });
    }
  }

  fail(message) {
    if (this.closed) return;
    const error = new Error(message);
    this.connectWaiter?.reject(error);
    this.connectWaiter = null;
    this.publish({ type: "failure", message });
    this.close(message);
  }

  close(reason) {
    if (this.closed) return;
    this.closed = true;
    this.socket?.destroy();
    this.socket = null;
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(new Error(reason));
    }
    this.pending.clear();
    this.onClosed();
  }
}

async function readViewerCredential(file) {
  if (!path.isAbsolute(file)) throw new Error("Process viewer credential path is unavailable");
  const metadata = await fs.lstat(file);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size < 16 || metadata.size > 256
    || (metadata.mode & 0o077) !== 0) {
    throw new Error("Process viewer credential is invalid");
  }
  const value = (await fs.readFile(file, "utf8")).trim();
  if (!/^[A-Za-z0-9+/]{43}=$/.test(value)) throw new Error("Process viewer credential is malformed");
  return value;
}

function validateProcessSurface(descriptor, workerEnvironmentId, allowInitialShell) {
  const tty = descriptor?.data?.tty === true;
  const processId = stringValue(descriptor?.data?.process_id);
  const initialShellAvailable = allowInitialShell === true
    && !processId
    && descriptor?.data?.start_new_shell_available === true;
  if (!descriptor
    || descriptor.kind !== "terminal"
    || descriptor.mode !== "direct"
    || descriptor.protocol !== "process_session.direct.v1"
    || descriptor.readOnly !== (!tty && !initialShellAvailable)
    || descriptor.environmentKind !== "process_session"
    || !path.isAbsolute(descriptor.viewerUrl)
    || descriptor.data?.worker_environment_id !== workerEnvironmentId
    || !Number.isSafeInteger(descriptor.data?.generation)
    || typeof descriptor.data?.tty !== "boolean"
    || !path.isAbsolute(descriptor.data?.auth_file)) {
    throw new Error("Projected surface is not an exact process_session viewer");
  }
}

function requiredAction(value) {
  if (!["write", "resize", "interrupt", "terminate", "restart", "start_new_shell"].includes(value)) {
    throw new Error("Process viewer action is unsupported");
  }
  return value;
}

function requiredId(value, label) {
  const id = stringValue(value);
  if (!id || id.length > 256 || /[\0\r\n]/.test(id)) throw new Error(`${label} is invalid`);
  return id;
}

function boundedInteger(value, min, max, label) {
  if (!Number.isSafeInteger(value) || value < min || value > max) throw new Error(`${label} is outside the supported range`);
  return value;
}

function decodeBase64(value) {
  const encoded = typeof value === "string" ? value : "";
  const bytes = Buffer.from(encoded, "base64");
  if (bytes.toString("base64") !== encoded) throw new Error("Process viewer output is not valid base64");
  return bytes;
}

function normalizedState(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Process viewer returned an invalid state");
  }
  const state = value;
  const workerEnvironmentId = stringValue(state.worker_environment_id ?? state.workerEnvironmentId);
  const status = stringValue(state.status);
  const processIdValue = state.process_id ?? state.processId;
  const exitCodeValue = state.exit_code ?? state.exitCode;
  if (!workerEnvironmentId
    || !Number.isSafeInteger(state.generation)
    || state.generation < 0
    || (processIdValue !== null && typeof processIdValue !== "string")
    || !status
    || typeof state.tty !== "boolean"
    || (exitCodeValue !== null && !Number.isSafeInteger(exitCodeValue))
    || typeof state.closed !== "boolean"
    || (state.failure !== null && typeof state.failure !== "string")
    || typeof (state.restart_available ?? state.restartAvailable) !== "boolean"
    || typeof (state.start_new_shell_available ?? state.startNewShellAvailable) !== "boolean"
    || (state.launch_label ?? state.launchLabel) !== null
      && typeof (state.launch_label ?? state.launchLabel) !== "string") {
    throw new Error("Process viewer returned an invalid state");
  }
  return {
    workerEnvironmentId,
    generation: state.generation,
    processId: stringValue(processIdValue),
    status,
    tty: state.tty,
    exitCode: exitCodeValue,
    closed: state.closed,
    failure: stringValue(state.failure),
    restartAvailable: state.restart_available === true || state.restartAvailable === true,
    startNewShellAvailable: state.start_new_shell_available === true || state.startNewShellAvailable === true,
    launchLabel: stringValue(state.launch_label ?? state.launchLabel),
  };
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

module.exports = { createProcessViewerManager };
