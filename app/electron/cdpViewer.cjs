const { WebSocket } = require("ws");

const CONNECT_TIMEOUT_MS = 7_000;
const COMMAND_TIMEOUT_MS = 7_000;
const DISCOVERY_MAX_BYTES = 1024 * 1024;
const MAX_FRAME_DATA_LENGTH = 24 * 1024 * 1024;
const MIN_FRAME_INTERVAL_MS = 28;

function createCdpViewerManager(options) {
  if (typeof options?.resolveSurface !== "function") throw new Error("CDP viewer requires a projected surface resolver");
  const sessions = new Map();
  const manualOwners = new Map();
  const watchedSenders = new WeakSet();

  async function open(ownerToken, sender, request) {
    const workerEnvironmentId = requiredId(request?.workerEnvironmentId, "workerEnvironmentId");
    const surfaceId = requiredId(request?.surfaceId, "surfaceId");
    const sessionId = requiredId(request?.attachmentId, "attachmentId");
    if (sessions.has(sessionId)) throw new Error("CDP viewer attachment is already open");
    if (!watchedSenders.has(sender)) {
      watchedSenders.add(sender);
      const senderId = sender.id;
      sender.once("destroyed", () => closeSender(senderId));
    }
    const session = new CdpViewerSession({
      ownerToken,
      sender,
      sessionId,
      surfaceId,
      workerEnvironmentId,
      visible: request.visible === true,
      onClosed: () => {
        releaseControl(session);
        sessions.delete(sessionId);
      },
    });
    sessions.set(sessionId, session);
    try {
      const descriptor = await options.resolveSurface(workerEnvironmentId, surfaceId);
      validateProjectedCdpSurface(descriptor);
      if (session.closed || sender.isDestroyed()) throw new Error("CDP viewer attachment was released");
      session.descriptor = { ...descriptor, data: { ...descriptor.data } };
      await session.connect();
      publishControl(targetKey(session));
      return session.snapshot();
    } catch (error) {
      session.close("connection failed");
      throw error;
    }
  }

  async function action(ownerToken, sender, request) {
    const session = requireSession(ownerToken, sender, request?.sessionId);
    if (request.action === "releaseControl") {
      releaseControl(session);
      return session.snapshot();
    }
    if (request.action === "setVisible") {
      if (request.visible !== true) releaseControl(session);
      return session.setVisible(request.visible === true);
    }
    const controlRevision = session.controlRevision;
    return session.enqueue(async () => {
      try {
        if (request.action === "claimControl" || request.action === "refreshState") {
          await validateSession(session);
        } else {
          await session.validationWork;
        }
      } catch (error) {
        session.fail(formatError(error));
        throw error;
      }
      if (session.closed) throw new Error("CDP viewer attachment was released");
      if (request.action !== "refreshState" && controlRevision !== session.controlRevision) {
        throw new Error("Manual browser input was released before this action");
      }
      if (request.action === "claimControl") {
        if (session.descriptor.readOnly) throw new Error("This browser surface is read-only");
        if (!session.visible) throw new Error("Show the browser before enabling manual input");
        const key = targetKey(session);
        const owner = manualOwners.get(key);
        if (owner && owner !== session) throw new Error("Manual browser input is enabled in another viewer. Release it there first.");
        manualOwners.set(key, session);
        publishControl(key);
        return session.snapshot();
      }
      if (request.action !== "refreshState" && (!session.manualControl || session.descriptor.readOnly || !session.visible)) {
        throw new Error("Enable manual browser control before sending input");
      }
      return session.action(request);
    });
  }

  function targetKey(session) {
    return JSON.stringify([session.descriptor?.viewerUrl, session.descriptor?.data?.target_id]);
  }

  function publishControl(key) {
    const owner = manualOwners.get(key);
    for (const session of sessions.values()) {
      if (targetKey(session) !== key) continue;
      session.manualControl = owner === session;
      session.manualControllerPresent = Boolean(owner);
      session.emitState();
    }
  }

  function releaseControl(session) {
    session.controlRevision += 1;
    const key = targetKey(session);
    if (manualOwners.get(key) === session) manualOwners.delete(key);
    session.manualControl = false;
    publishControl(key);
  }

  function close(ownerToken, sender, request) {
    const session = requireSession(ownerToken, sender, request?.sessionId);
    session.close("closed by client");
    return { sessionId: session.sessionId, closed: true };
  }

  function requireSession(ownerToken, sender, sessionIdValue) {
    const sessionId = requiredId(sessionIdValue, "sessionId");
    const session = sessions.get(sessionId);
    if (!session || session.ownerToken !== ownerToken || session.senderId !== sender.id) {
      throw new Error("CDP viewer session is unavailable");
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

  async function revalidate() {
    await Promise.all([...sessions.values()].filter((session) => session.descriptor).map(async (session) => {
      try {
        await validateSession(session);
      } catch {
        session.fail("The projected browser surface was withdrawn.");
      }
    }));
  }

  function validateSession(session) {
    session.validationWork = session.validationWork.then(() => revalidateSession(session));
    return session.validationWork;
  }

  async function revalidateSession(session) {
    const descriptor = await options.resolveSurface(session.workerEnvironmentId, session.surfaceId);
    validateProjectedCdpSurface(descriptor);
    if (descriptor.viewerUrl !== session.descriptor?.viewerUrl
      || descriptor.runId !== session.descriptor?.runId
      || descriptor.formationRevision !== session.descriptor?.formationRevision
      || stringValue(descriptor.data?.target_id) !== stringValue(session.descriptor?.data?.target_id)) {
      session.fail("The projected CDP room identity changed.");
      throw new Error("The projected CDP room identity changed.");
    }
    session.descriptor = { ...descriptor, data: { ...descriptor.data } };
    if (descriptor.readOnly) releaseControl(session);
  }

  function closeAll() {
    for (const session of [...sessions.values()]) session.close("application closing");
  }

  return { action, close, closeAll, closeOwner, closeSender, open, revalidate };
}

class CdpViewerSession {
  constructor(input) {
    this.descriptor = null;
    this.ownerToken = input.ownerToken;
    this.sender = input.sender;
    this.senderId = input.sender.id;
    this.sessionId = input.sessionId;
    this.surfaceId = input.surfaceId;
    this.workerEnvironmentId = input.workerEnvironmentId;
    this.onClosed = input.onClosed;
    this.socket = null;
    this.closed = false;
    this.nextCommandId = 1;
    this.pending = new Map();
    this.history = { currentIndex: -1, entries: [] };
    this.zoom = 1;
    this.url = "";
    this.title = "";
    this.visible = input.visible;
    this.manualControl = false;
    this.controlRevision = 0;
    this.inputWork = Promise.resolve();
    this.validationWork = Promise.resolve();
    this.manualControllerPresent = false;
    this.navigationError = "";
    this.lastFrame = null;
    this.screencasting = false;
    this.visibilityRevision = 0;
    this.visibilityWork = Promise.resolve();
    this.loading = true;
    this.lastFrameAt = 0;
    this.frameTimer = null;
    this.historyTimer = null;
    this.target = null;
  }

  enqueue(work) {
    const pending = this.inputWork.then(work, work);
    this.inputWork = pending;
    return pending;
  }

  async connect() {
    this.target = await resolveCdpTarget(
      this.descriptor.viewerUrl,
      stringValue(this.descriptor.data?.target_id),
    );
    this.url = this.target.url || this.url;
    this.title = this.target.title || this.title;
    await this.openSocket(this.target.webSocketDebuggerUrl);
    await this.command("Page.enable");
    await this.command("Page.setLifecycleEventsEnabled", { enabled: true });
    await this.setVisible(this.visible);
    await this.refreshHistory();
    this.loading = false;
    this.emitState();
  }

  setVisible(visible) {
    this.visible = visible;
    if (!visible && this.frameTimer) clearTimeout(this.frameTimer);
    if (!visible) this.frameTimer = null;
    const revision = ++this.visibilityRevision;
    this.visibilityWork = this.visibilityWork.then(async () => {
      if (this.closed || !this.socket || revision !== this.visibilityRevision) return this.snapshot();
      const visible = this.visible;
      if (this.screencasting !== visible) {
        await this.command(visible ? "Page.startScreencast" : "Page.stopScreencast", visible ? {
          format: "jpeg", quality: 78, maxWidth: 1920, maxHeight: 1200, everyNthFrame: 1,
        } : {});
        this.screencasting = visible;
      }
      if (this.visible && this.lastFrame) this.publishFrame();
      this.emitState();
      return this.snapshot();
    }).catch((error) => {
      this.fail(formatError(error));
      throw error;
    });
    return this.visibilityWork;
  }

  openSocket(url) {
    return new Promise((resolve, reject) => {
      if (this.closed) return reject(new Error("CDP viewer session is closed"));
      const socket = new WebSocket(url, { maxPayload: 32 * 1024 * 1024, perMessageDeflate: false });
      this.socket = socket;
      let settled = false;
      const timer = setTimeout(() => finish(new Error("Timed out while connecting to the projected CDP endpoint")), CONNECT_TIMEOUT_MS);
      const finish = (error) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        if (error) reject(error);
        else resolve();
      };
      socket.once("open", () => finish());
      socket.once("error", (error) => finish(new Error(`Projected CDP endpoint failed: ${formatError(error)}`)));
      socket.on("message", (data) => this.onMessage(data));
      socket.on("close", (_code, reason) => {
        if (!settled) finish(new Error(`Projected CDP endpoint closed before initialization: ${String(reason || "closed")}`));
        if (!this.closed) {
          this.fail("The projected browser session disconnected.");
        }
      });
    });
  }

  onMessage(raw) {
    if (this.closed) return;
    let message;
    try {
      message = JSON.parse(Buffer.isBuffer(raw) ? raw.toString("utf8") : String(raw));
    } catch {
      this.fail("The browser sent malformed CDP data.");
      return;
    }
    if (Number.isSafeInteger(message?.id)) {
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      clearTimeout(pending.timer);
      if (message.error) pending.reject(new Error(stringValue(message.error.message) || "CDP command failed"));
      else pending.resolve(message.result ?? {});
      return;
    }
    const method = stringValue(message?.method);
    const params = objectOrEmpty(message?.params);
    if (method === "Page.screencastFrame") {
      try {
        const frameSessionId = params.sessionId;
        if (!Number.isSafeInteger(frameSessionId) || frameSessionId < 0) throw new Error("The browser sent an invalid screencast frame id.");
        void this.command("Page.screencastFrameAck", { sessionId: frameSessionId }).catch((error) => this.fail(formatError(error)));
        const data = stringValue(params.data);
        if (!data || data.length > MAX_FRAME_DATA_LENGTH) throw new Error("The browser sent an invalid screencast frame.");
        const metadata = sanitizeFrameMetadata(params.metadata);
        if (metadata.deviceWidth <= 0 || metadata.deviceHeight <= 0) throw new Error("The browser sent invalid screencast dimensions.");
        this.lastFrame = {
          type: "frame",
          data,
          mimeType: "image/jpeg",
          metadata,
        };
        if (this.visible) {
          const delay = Math.max(0, MIN_FRAME_INTERVAL_MS - (Date.now() - this.lastFrameAt));
          if (delay === 0) this.publishFrame();
          else if (!this.frameTimer) this.frameTimer = setTimeout(() => this.publishFrame(), delay);
        }
      } catch (error) {
        this.fail(formatError(error));
      }
      return;
    }
    if (method === "Page.frameStartedLoading") {
      this.loading = true;
      this.navigationError = "";
      this.emitState();
      return;
    }
    if (method === "Page.frameStoppedLoading" || method === "Page.loadEventFired") {
      this.loading = false;
      this.emitState();
      this.scheduleHistoryRefresh();
      return;
    }
    if (method === "Page.frameNavigated" && !params.frame?.parentId) {
      this.url = stringValue(params.frame?.url) || this.url;
      this.navigationError = stringValue(params.frame?.unreachableUrl) ? `Unable to load ${params.frame.unreachableUrl}` : "";
      this.emitState();
      this.scheduleHistoryRefresh();
      return;
    }
    if (method === "Page.navigatedWithinDocument") {
      this.scheduleHistoryRefresh();
      return;
    }
    if (method === "Inspector.detached") {
      this.fail(stringValue(params.reason) || "The browser inspector detached.");
    }
  }

  publishFrame() {
    if (this.frameTimer) clearTimeout(this.frameTimer);
    this.frameTimer = null;
    if (this.closed || !this.visible || !this.lastFrame) return;
    this.lastFrameAt = Date.now();
    this.emit(this.lastFrame);
  }

  command(method, params = {}) {
    if (this.closed || !this.socket || this.socket.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error("Projected CDP endpoint is not connected"));
    }
    const id = this.nextCommandId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CDP command timed out: ${method}`));
      }, COMMAND_TIMEOUT_MS);
      this.pending.set(id, { reject, resolve, timer });
      try {
        this.socket.send(JSON.stringify({ id, method, params }));
      } catch (error) {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(error);
      }
    });
  }

  async action(request) {
    const action = stringValue(request?.action);
    if (action === "navigate") {
      const url = safeNavigationUrl(request?.url);
      this.loading = true;
      this.emitState();
      this.navigationError = "";
      const result = await this.command("Page.navigate", { url });
      if (stringValue(result.errorText)) {
        this.loading = false;
        this.navigationError = result.errorText;
        this.emitState();
        throw new Error(`Browser navigation failed: ${result.errorText}`);
      }
      return this.snapshot();
    }
    if (action === "stop") {
      await this.command("Page.stopLoading");
      this.loading = false;
      this.emitState();
      return this.snapshot();
    }
    if (action === "reload") {
      this.loading = true;
      this.navigationError = "";
      this.emitState();
      await this.command("Page.reload");
      return this.snapshot();
    }
    if (action === "find") {
      const query = boundedString(request?.query, 2_048);
      if (!query) throw new Error("Browser find query is required");
      const backwards = request?.backwards === true;
      const matchCase = request?.matchCase === true;
      const expression = `(() => typeof window.find === "function" && window.find(${JSON.stringify(query)}, ${matchCase}, ${backwards}, true, false, true, false))()`;
      const result = await this.command("Runtime.evaluate", { expression, returnByValue: true, userGesture: true });
      if (result.exceptionDetails) throw new Error("Browser find could not execute in this document");
      return { sessionId: this.sessionId, accepted: true, found: result?.result?.value === true };
    }
    if (action === "zoom") {
      const factor = finiteNumber(request?.factor, 0.5, 2.5);
      await this.command("Emulation.setPageScaleFactor", { pageScaleFactor: factor });
      this.zoom = factor;
      this.emitState();
      return this.snapshot();
    }
    if (action === "back" || action === "forward") {
      await this.refreshHistory();
      const nextIndex = this.history.currentIndex + (action === "back" ? -1 : 1);
      const entry = this.history.entries[nextIndex];
      if (!entry || !Number.isSafeInteger(entry.id)) throw new Error(`Browser cannot navigate ${action}`);
      this.loading = true;
      this.emitState();
      await this.command("Page.navigateToHistoryEntry", { entryId: entry.id });
      return this.snapshot();
    }
    if (action === "mouse") {
      await this.command("Input.dispatchMouseEvent", sanitizeMouseEvent(request));
      return { sessionId: this.sessionId, accepted: true };
    }
    if (action === "key") {
      await this.command("Input.dispatchKeyEvent", sanitizeKeyEvent(request));
      return { sessionId: this.sessionId, accepted: true };
    }
    if (action === "refreshState") {
      await this.refreshHistory();
      if (this.visible && this.lastFrame) this.publishFrame();
      return this.snapshot();
    }
    throw new Error(`Unsupported CDP viewer action: ${action || "missing"}`);
  }

  async refreshHistory() {
    const result = await this.command("Page.getNavigationHistory");
    const entries = Array.isArray(result.entries)
      ? result.entries.slice(0, 500).flatMap((entry) => {
          if (!Number.isSafeInteger(entry?.id)) return [];
          return [{ id: entry.id, url: stringValue(entry.url), title: stringValue(entry.title) }];
        })
      : [];
    this.history = {
      currentIndex: Number.isSafeInteger(result.currentIndex) ? result.currentIndex : -1,
      entries,
    };
    const current = entries[this.history.currentIndex];
    if (current) {
      this.url = current.url || this.url;
      this.title = current.title || this.title;
    }
    this.emitState();
  }

  scheduleHistoryRefresh() {
    if (this.historyTimer || this.closed) return;
    this.historyTimer = setTimeout(() => {
      this.historyTimer = null;
      void this.refreshHistory().catch((error) => this.fail(formatError(error)));
    }, 120);
  }

  snapshot() {
    return {
      sessionId: this.sessionId,
      workerEnvironmentId: this.workerEnvironmentId,
      surfaceId: this.surfaceId,
      runId: stringValue(this.descriptor?.runId),
      targetId: stringValue(this.descriptor?.data?.target_id),
      visible: this.visible,
      readOnly: this.descriptor?.readOnly === true,
      manualControl: this.manualControl,
      manualControllerPresent: this.manualControllerPresent,
      navigationError: this.navigationError,
      connected: !this.closed && this.socket?.readyState === WebSocket.OPEN,
      loading: this.loading,
      url: this.url,
      title: this.title,
      canGoBack: this.history.currentIndex > 0,
      canGoForward: this.history.currentIndex >= 0 && this.history.currentIndex < this.history.entries.length - 1,
      zoom: this.zoom,
    };
  }

  emitState() {
    this.emit({ type: "state", state: this.snapshot() });
  }

  emit(payload) {
    if (this.closed || this.sender.isDestroyed()) return;
    this.sender.send("octopus:cdp-viewer-event", { sessionId: this.sessionId, ...payload });
  }

  fail(message) {
    if (this.closed) return;
    this.emit({ type: "error", error: message });
    this.close(message);
  }

  close(reason) {
    if (this.closed) return;
    this.closed = true;
    if (this.historyTimer) clearTimeout(this.historyTimer);
    this.historyTimer = null;
    if (this.frameTimer) clearTimeout(this.frameTimer);
    this.frameTimer = null;
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(new Error(`CDP viewer closed: ${reason}`));
    }
    this.pending.clear();
    this.lastFrame = null;
    try { this.socket?.close(1000, "client closed"); } catch {}
    this.socket = null;
    this.onClosed();
  }
}

async function resolveCdpTarget(addressValue, targetId) {
  requiredId(targetId, "projected target_id");
  const address = new URL(requiredString(addressValue, "projected CDP address"));
  if (address.protocol === "ws:" || address.protocol === "wss:") {
    if (!address.pathname.endsWith(`/page/${encodeURIComponent(targetId)}`)) {
      throw new Error("Projected CDP WebSocket must name the exact page target");
    }
    return { id: targetId, title: "", url: "", webSocketDebuggerUrl: address.toString() };
  }
  if (address.protocol !== "http:" && address.protocol !== "https:") {
    throw new Error(`Unsupported projected CDP address protocol: ${address.protocol}`);
  }
  const payload = await readDiscoveryJson(address.pathname.startsWith("/json/") ? address : new URL("json/list", address));
  if (!Array.isArray(payload)) throw new Error("CDP page discovery must return a target list");
  const candidates = payload.filter((entry) => entry?.type === "page" && entry.id === targetId);
  if (candidates.length !== 1 || !validWebSocketUrl(candidates[0].webSocketDebuggerUrl)) {
    throw new Error(`CDP discovery returned no unique attachable page target ${targetId}`);
  }
  const candidate = candidates[0];
  const socket = new URL(candidate.webSocketDebuggerUrl);
  if (socket.hostname !== address.hostname) throw new Error("CDP page target is outside its projected host");
  return { id: targetId, title: stringValue(candidate.title), url: stringValue(candidate.url), webSocketDebuggerUrl: socket.toString() };
}

async function readDiscoveryJson(url) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), CONNECT_TIMEOUT_MS);
  try {
    const response = await fetch(url, {
      headers: { accept: "application/json" },
      redirect: "error",
      signal: controller.signal,
    });
    if (!response.ok) throw new Error(`CDP discovery failed with HTTP ${response.status}`);
    const declaredLength = Number(response.headers.get("content-length"));
    if (Number.isFinite(declaredLength) && declaredLength > DISCOVERY_MAX_BYTES) throw new Error("CDP discovery response is too large");
    const text = await response.text();
    if (Buffer.byteLength(text) > DISCOVERY_MAX_BYTES) throw new Error("CDP discovery response is too large");
    return JSON.parse(text);
  } finally {
    clearTimeout(timer);
  }
}

function validateProjectedCdpSurface(descriptor) {
  if (!descriptor || descriptor.kind !== "browser" || descriptor.mode !== "direct") {
    throw new Error("Worker Environment did not project a direct browser surface");
  }
  if (stringValue(descriptor.protocol).toLowerCase() !== "cdp") {
    throw new Error("Worker Environment browser surface is not a CDP transport");
  }
  requiredId(descriptor.data?.target_id, "projected target_id");
  const address = requiredString(descriptor.viewerUrl, "projected CDP address");
  const url = new URL(address);
  if (!["http:", "https:", "ws:", "wss:"].includes(url.protocol)) {
    throw new Error(`Unsupported projected CDP address protocol: ${url.protocol}`);
  }
}

function sanitizeFrameMetadata(value) {
  const metadata = objectOrEmpty(value);
  return {
    deviceWidth: finiteNumber(metadata.deviceWidth, 0, 16_384),
    deviceHeight: finiteNumber(metadata.deviceHeight, 0, 16_384),
    pageScaleFactor: finiteNumber(metadata.pageScaleFactor, 0, 100),
    offsetTop: finiteNumber(metadata.offsetTop, -100_000, 100_000),
    scrollOffsetX: finiteNumber(metadata.scrollOffsetX, -100_000, 100_000),
    scrollOffsetY: finiteNumber(metadata.scrollOffsetY, -100_000, 100_000),
    timestamp: finiteNumber(metadata.timestamp, 0, Number.MAX_SAFE_INTEGER),
  };
}

function sanitizeMouseEvent(request) {
  const type = stringValue(request?.eventType);
  if (!["mousePressed", "mouseReleased", "mouseMoved", "mouseWheel"].includes(type)) {
    throw new Error(`Unsupported CDP mouse event: ${type || "missing"}`);
  }
  const button = stringValue(request?.button) || "none";
  if (!["none", "left", "middle", "right", "back", "forward"].includes(button)) throw new Error("Invalid CDP mouse button");
  return {
    type,
    x: finiteNumber(request?.x, -100_000, 100_000),
    y: finiteNumber(request?.y, -100_000, 100_000),
    button,
    buttons: Number.isSafeInteger(request?.buttons) ? request.buttons : 0,
    clickCount: Number.isSafeInteger(request?.clickCount) ? Math.min(3, Math.max(0, request.clickCount)) : 0,
    deltaX: finiteNumber(request?.deltaX, -100_000, 100_000),
    deltaY: finiteNumber(request?.deltaY, -100_000, 100_000),
    modifiers: Number.isSafeInteger(request?.modifiers) ? request.modifiers : 0,
  };
}

function sanitizeKeyEvent(request) {
  const type = stringValue(request?.eventType);
  if (!["keyDown", "keyUp", "rawKeyDown", "char"].includes(type)) throw new Error(`Unsupported CDP key event: ${type || "missing"}`);
  return {
    type,
    key: boundedString(request?.key, 128),
    code: boundedString(request?.code, 128),
    text: boundedString(request?.text, 4096),
    unmodifiedText: boundedString(request?.unmodifiedText, 4096),
    windowsVirtualKeyCode: Number.isSafeInteger(request?.windowsVirtualKeyCode) ? request.windowsVirtualKeyCode : 0,
    nativeVirtualKeyCode: Number.isSafeInteger(request?.nativeVirtualKeyCode) ? request.nativeVirtualKeyCode : 0,
    modifiers: Number.isSafeInteger(request?.modifiers) ? request.modifiers : 0,
    autoRepeat: request?.autoRepeat === true,
  };
}

function safeNavigationUrl(value) {
  const url = new URL(requiredString(value, "url"));
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("Browser navigation accepts only HTTP(S) URLs");
  return url.toString();
}

function validWebSocketUrl(value) {
  try {
    const url = new URL(String(value ?? ""));
    return url.protocol === "ws:" || url.protocol === "wss:";
  } catch {
    return false;
  }
}

function requiredId(value, label) {
  const text = requiredString(value, label);
  if (text.length > 512 || /[\u0000-\u001f]/.test(text)) throw new Error(`${label} is invalid`);
  return text;
}

function requiredString(value, label) {
  const text = stringValue(value);
  if (!text) throw new Error(`${label} is required`);
  return text;
}

function boundedString(value, maxLength) {
  const text = typeof value === "string" ? value : "";
  if (text.length > maxLength) throw new Error("CDP input field is too large");
  return text;
}

function finiteNumber(value, minimum, maximum) {
  const number = Number(value ?? 0);
  if (!Number.isFinite(number) || number < minimum || number > maximum) throw new Error("CDP numeric input is out of range");
  return number;
}

function objectOrEmpty(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function formatError(error) {
  return error instanceof Error ? error.message : String(error);
}

module.exports = { createCdpViewerManager };
