const { spawn } = require("node:child_process");
const fs = require("node:fs");

const LOGIN_TIMEOUT_MS = 300_000;
const REQUEST_TIMEOUT_MS = 30_000;
const IDLE_STOP_MS = 20_000;

function createChatgptSignIn(options) {
  let appServer = null;
  let idleTimer = null;
  const logins = new Map();

  function scheduleIdleStop() {
    if (idleTimer) clearTimeout(idleTimer);
    idleTimer = setTimeout(() => {
      idleTimer = null;
      const server = appServer;
      if (!server || server.closed) return;
      if (server.pending.size > 0 || [...logins.values()].some((login) => login.state === "pending")) {
        scheduleIdleStop();
        return;
      }
      dispose();
    }, IDLE_STOP_MS);
    idleTimer.unref();
  }

  function ensureAppServer() {
    if (appServer && !appServer.closed) return appServer.ready;
    const binary = options.codexBinary();
    fs.accessSync(binary, fs.constants.X_OK);
    const codexHome = options.codexHome();
    fs.mkdirSync(codexHome, { recursive: true, mode: 0o700 });
    const child = spawn(binary, ["app-server"], {
      cwd: codexHome,
      env: { ...process.env, CODEX_HOME: codexHome },
      stdio: ["pipe", "pipe", "pipe"],
    });
    const server = { child, closed: false, nextId: 1, pending: new Map(), buffer: "", stderr: "", ready: null };
    appServer = server;
    const fail = (error) => {
      server.closed = true;
      for (const waiter of server.pending.values()) {
        clearTimeout(waiter.timer);
        waiter.reject(error);
      }
      server.pending.clear();
      for (const login of logins.values()) {
        if (login.state === "pending") Object.assign(login, { state: "failed", error: error.message });
      }
    };
    child.on("error", (error) => fail(new Error(`ChatGPT sign-in could not start: ${error.message}`)));
    child.on("exit", () => {
      if (!server.closed) fail(new Error("ChatGPT sign-in stopped before it finished"));
    });
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => { server.stderr = (server.stderr + chunk).slice(-4000); });
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      server.buffer += chunk;
      for (;;) {
        const newline = server.buffer.indexOf("\n");
        if (newline < 0) break;
        const line = server.buffer.slice(0, newline).trim();
        server.buffer = server.buffer.slice(newline + 1);
        if (!line) continue;
        let message;
        try { message = JSON.parse(line); } catch { continue; }
        if (message.id !== undefined && (message.result !== undefined || message.error !== undefined)) {
          const waiter = server.pending.get(message.id);
          if (!waiter) continue;
          server.pending.delete(message.id);
          clearTimeout(waiter.timer);
          if (message.error) waiter.reject(new Error(String(message.error.message || "ChatGPT sign-in was refused")));
          else waiter.resolve(message.result);
          continue;
        }
        if (message.method === "account/login/completed") {
          const login = logins.get(String(message.params?.loginId ?? ""));
          if (login) {
            Object.assign(login, message.params?.success === true
              ? { state: "completed", error: "" }
              : { state: "failed", error: String(message.params?.error || "Sign-in did not complete") });
          }
        }
      }
    });
    server.request = (method, params) => new Promise((resolve, reject) => {
      if (server.closed) return reject(new Error("ChatGPT sign-in is not running"));
      const id = server.nextId++;
      const timer = setTimeout(() => {
        server.pending.delete(id);
        reject(new Error("ChatGPT sign-in did not respond"));
      }, REQUEST_TIMEOUT_MS);
      server.pending.set(id, { resolve, reject, timer });
      child.stdin.write(`${JSON.stringify({ id, method, ...(params === undefined ? {} : { params }) })}\n`);
    });
    server.ready = server.request("initialize", {
      clientInfo: { name: "octopus_desktop", title: "Octopus", version: options.version() },
    }).then(() => {
      child.stdin.write(`${JSON.stringify({ method: "initialized" })}\n`);
      return server;
    });
    return server.ready;
  }

  function dispose() {
    const server = appServer;
    appServer = null;
    if (server && !server.closed) {
      server.closed = true;
      server.child.kill("SIGTERM");
    }
  }

  async function account() {
    const server = await ensureAppServer();
    const response = await server.request("account/read", { refreshToken: false });
    scheduleIdleStop();
    return { signedIn: response?.account?.type === "chatgpt" };
  }

  async function start() {
    const server = await ensureAppServer();
    const response = await server.request("account/login/start", { type: "chatgpt" });
    if (response?.type !== "chatgpt" || typeof response.loginId !== "string" || typeof response.authUrl !== "string") {
      throw new Error("ChatGPT sign-in returned an unexpected response");
    }
    const authUrl = new URL(response.authUrl);
    if (authUrl.protocol !== "https:") throw new Error("ChatGPT sign-in returned an unsafe address");
    const login = { state: "pending", error: "" };
    logins.set(response.loginId, login);
    setTimeout(() => {
      if (login.state !== "pending") return;
      Object.assign(login, { state: "failed", error: "Sign-in was not finished in the browser in time" });
      void server.request("account/login/cancel", { loginId: response.loginId }).catch(() => {});
    }, LOGIN_TIMEOUT_MS).unref();
    await options.openExternal(authUrl.toString());
    return { loginId: response.loginId };
  }

  function status(loginId) {
    const login = logins.get(String(loginId ?? ""));
    if (!login) throw new Error("Unknown ChatGPT sign-in attempt");
    return { state: login.state, error: login.error };
  }

  async function cancel(loginId) {
    const login = logins.get(String(loginId ?? ""));
    if (!login || login.state !== "pending") return { cancelled: false };
    Object.assign(login, { state: "failed", error: "Sign-in was cancelled" });
    const server = await ensureAppServer();
    await server.request("account/login/cancel", { loginId: String(loginId) }).catch(() => {});
    return { cancelled: true };
  }

  async function models() {
    const server = await ensureAppServer();
    const result = [];
    const modelIds = new Set();
    const cursors = new Set();
    let cursor = null;
    try {
      for (let page = 0; page < 100; page += 1) {
        const response = await server.request("model/list", { cursor, includeHidden: true });
        if (!Array.isArray(response?.data)) throw new Error("Codex returned an invalid model list.");
        for (const model of response.data) {
          const efforts = model?.supportedReasoningEfforts?.map((entry) => entry?.reasoningEffort);
          if (typeof model?.model !== "string" || !model.model || modelIds.has(model.model)
            || typeof model.displayName !== "string" || !Array.isArray(efforts)
            || !efforts.every((effort) => typeof effort === "string" && effort)
            || new Set(efforts).size !== efforts.length || !efforts.includes(model.defaultReasoningEffort)) {
            throw new Error("Codex returned an invalid or repeated model.");
          }
          modelIds.add(model.model);
          result.push({
            model: model.model, displayName: model.displayName,
            description: typeof model.description === "string" ? model.description : "",
            defaultReasoningEffort: model.defaultReasoningEffort, supportedReasoningEfforts: efforts,
            showInPicker: model.hidden !== true, isDefault: model.isDefault === true,
          });
        }
        if (response.nextCursor === null) return result;
        if (typeof response.nextCursor !== "string" || !response.nextCursor || cursors.has(response.nextCursor)) {
          throw new Error("Codex returned an invalid or repeated model-list cursor.");
        }
        cursor = response.nextCursor;
        cursors.add(cursor);
      }
      throw new Error("Codex model list exceeded its pagination limit.");
    } finally {
      scheduleIdleStop();
    }
  }

  return { account, start, status, cancel, dispose, models };
}

module.exports = { createChatgptSignIn };
