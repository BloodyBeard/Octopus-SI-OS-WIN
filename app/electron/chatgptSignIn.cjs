const { spawn } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const LOGIN_TIMEOUT_MS = 300_000;
const REQUEST_TIMEOUT_MS = 30_000;
const IDLE_STOP_MS = 20_000;

function createChatgptSignIn(options) {
  let appServer = null;
  let idleTimer = null;
  let authBusy = false;
  let authGeneration = 0;
  const logins = new Map();

  function nativeAuthStamp(codexHome) {
    try {
      const stat = fs.statSync(path.join(codexHome, "auth.json"), { bigint: true });
      return `${stat.dev}:${stat.ino}:${stat.size}:${stat.mtimeNs}:${stat.ctimeNs}`;
    } catch (error) {
      if (error.code === "ENOENT") return null;
      throw error;
    }
  }

  function requireCurrentScope(server) {
    if (server !== appServer || server.closed || server.codexHome !== options.codexHome()
      || server.binary !== options.codexBinary()) throw new Error("ChatGPT account scope changed. Refresh and try again.");
  }

  function publishAccountChange() {
    authGeneration += 1;
    options.onAccountChanged?.();
  }

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
    const binary = options.codexBinary();
    const codexHome = options.codexHome();
    const authStamp = nativeAuthStamp(codexHome);
    if (appServer && !appServer.closed) {
      if (appServer.binary === binary && appServer.codexHome === codexHome
        && (appServer.authStamp === authStamp || [...logins.values()].some((login) => login.server === appServer && login.state === "pending"))) return appServer.ready;
      dispose();
    }
    fs.accessSync(binary, fs.constants.X_OK);
    fs.mkdirSync(codexHome, { recursive: true, mode: 0o700 });
    const child = spawn(binary, ["app-server"], {
      cwd: codexHome,
      env: { ...process.env, CODEX_HOME: codexHome },
      stdio: ["pipe", "pipe", "pipe"],
    });
    const server = { child, binary, codexHome, authStamp, closed: false, nextId: 1, pending: new Map(), buffer: "", stderr: "", ready: null };
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
          if (login?.server === server && login.state === "pending" && !login.verifying) {
            if (message.params?.success !== true) {
              clearTimeout(login.timer);
              Object.assign(login, { state: "failed", error: String(message.params?.error || "Sign-in did not complete") });
            } else {
              login.verifying = true;
              const generation = authGeneration;
              void readAccount(server, true).then((value) => {
                if (login.state !== "pending" || generation !== authGeneration) return;
                if (!value.signedIn) throw new Error("ChatGPT sign-in finished without an authorized account.");
                server.authStamp = nativeAuthStamp(server.codexHome);
                clearTimeout(login.timer);
                Object.assign(login, { state: "completed", error: "" });
                publishAccountChange();
              }).catch((error) => {
                if (login.state !== "pending") return;
                clearTimeout(login.timer);
                Object.assign(login, { state: "failed", error: error.message });
                publishAccountChange();
              }).finally(scheduleIdleStop);
            }
          }
        }
        if (message.method === "account/updated" && !authBusy && ![...logins.values()].some((login) => login.state === "pending")) {
          publishAccountChange();
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
    if (idleTimer) clearTimeout(idleTimer);
    idleTimer = null;
    const server = appServer;
    appServer = null;
    if (server && !server.closed) {
      server.closed = true;
      for (const waiter of server.pending.values()) {
        clearTimeout(waiter.timer);
        waiter.reject(new Error("Codex account connection was closed."));
      }
      server.pending.clear();
      for (const login of logins.values()) {
        if (login.server !== server || login.state !== "pending") continue;
        clearTimeout(login.timer);
        Object.assign(login, { state: "failed", error: "ChatGPT account connection was closed." });
      }
      server.child.kill("SIGTERM");
    }
  }

  async function readAccount(server, refreshToken = false) {
    const response = await server.request("account/read", { refreshToken });
    requireCurrentScope(server);
    if (!response || !Object.hasOwn(response, "account") || (response.account !== null
      && !["chatgpt", "apiKey", "amazonBedrock"].includes(response.account?.type))) {
      throw new Error("ChatGPT returned an invalid account response.");
    }
    const native = response.account;
    if (native?.type === "chatgpt" && ((native.email != null && typeof native.email !== "string")
      || (native.planType != null && typeof native.planType !== "string"))) throw new Error("ChatGPT returned invalid account identity.");
    return {
      signedIn: native?.type === "chatgpt", authType: native?.type ?? null,
      email: native?.type === "chatgpt" ? native.email ?? null : null,
      planType: native?.type === "chatgpt" ? native.planType ?? null : null,
      credentialHome: server.codexHome, octopusHome: options.octopusHome?.() ?? null,
    };
  }

  async function account() {
    try { return await readAccount(await ensureAppServer()); }
    finally { scheduleIdleStop(); }
  }

  async function rateLimits() {
    const server = await ensureAppServer();
    const generation = authGeneration;
    const authStamp = nativeAuthStamp(server.codexHome);
    try {
      const before = await readAccount(server);
      if (!before.signedIn) throw new Error("Sign in with ChatGPT to see usage limits.");
      const response = await server.request("account/rateLimits/read");
      const after = await readAccount(server);
      if (generation !== authGeneration || authStamp !== nativeAuthStamp(server.codexHome)
        || JSON.stringify(before) !== JSON.stringify(after)) {
        throw new Error("ChatGPT account changed while loading usage. Refresh and try again.");
      }
      if (!validLimit(response?.rateLimits)
        || (response.rateLimitsByLimitId != null && (typeof response.rateLimitsByLimitId !== "object" || Array.isArray(response.rateLimitsByLimitId)
          || !Object.values(response.rateLimitsByLimitId).every(validLimit)))) {
        throw new Error("ChatGPT returned invalid usage limits.");
      }
      return { rateLimits: response.rateLimits, rateLimitsByLimitId: response.rateLimitsByLimitId ?? null, observedAtMs: Date.now() };
    } finally { scheduleIdleStop(); }
  }

  function validLimit(limit) {
    if (!limit || typeof limit !== "object" || Array.isArray(limit)) return false;
    for (const key of ["limitId", "limitName", "planType", "rateLimitReachedType"]) {
      if (limit[key] != null && typeof limit[key] !== "string") return false;
    }
    for (const key of ["primary", "secondary"]) {
      const window = limit[key];
      if (window != null && (typeof window !== "object" || !Number.isFinite(window.usedPercent)
        || (window.windowDurationMins != null && !Number.isFinite(window.windowDurationMins))
        || (window.resetsAt != null && !Number.isFinite(window.resetsAt)))) return false;
    }
    const credits = limit.credits;
    if (credits != null && (typeof credits.hasCredits !== "boolean" || typeof credits.unlimited !== "boolean"
      || (credits.balance != null && typeof credits.balance !== "string"))) return false;
    const individual = limit.individualLimit;
    if (individual != null && (typeof individual.limit !== "string" || typeof individual.used !== "string"
      || !Number.isFinite(individual.remainingPercent) || !Number.isFinite(individual.resetsAt))) return false;
    return limit.spendControlReached == null || typeof limit.spendControlReached === "boolean";
  }

  async function start() {
    if (authBusy || [...logins.values()].some((login) => login.state === "pending")) {
      throw new Error("A ChatGPT account change is already in progress.");
    }
    authBusy = true;
    authGeneration += 1;
    try {
      const server = await ensureAppServer();
      requireCurrentScope(server);
      const response = await server.request("account/login/start", { type: "chatgpt" });
      requireCurrentScope(server);
      if (response?.type !== "chatgpt" || typeof response.loginId !== "string" || typeof response.authUrl !== "string") {
        throw new Error("ChatGPT sign-in returned an unexpected response");
      }
      const authUrl = new URL(response.authUrl);
      if (authUrl.protocol !== "https:") throw new Error("ChatGPT sign-in returned an unsafe address");
      const login = { state: "pending", error: "", server, verifying: false, timer: null };
      logins.set(response.loginId, login);
      login.timer = setTimeout(() => {
        if (login.state !== "pending") return;
        Object.assign(login, { state: "failed", error: "Sign-in was not finished in the browser in time" });
        void server.request("account/login/cancel", { loginId: response.loginId }).catch((error) => {
          login.error += `. Cancellation failed: ${error.message}`;
        }).finally(() => { publishAccountChange(); scheduleIdleStop(); });
      }, LOGIN_TIMEOUT_MS).unref();
      try { await options.openExternal(authUrl.toString()); }
      catch (error) {
        await cancel(response.loginId);
        throw error;
      }
      return { loginId: response.loginId, authUrl: authUrl.toString() };
    } finally { authBusy = false; scheduleIdleStop(); }
  }

  function status(loginId) {
    const login = logins.get(String(loginId ?? ""));
    if (!login) throw new Error("Unknown ChatGPT sign-in attempt");
    return { state: login.state, error: login.error };
  }

  async function cancel(loginId) {
    const login = logins.get(String(loginId ?? ""));
    if (!login || login.state !== "pending") return { cancelled: false };
    const server = login.server;
    requireCurrentScope(server);
    const response = await server.request("account/login/cancel", { loginId: String(loginId) });
    if (!["canceled", "notFound"].includes(response?.status)) throw new Error("ChatGPT did not confirm sign-in cancellation.");
    clearTimeout(login.timer);
    Object.assign(login, { state: "failed", error: "Sign-in was cancelled" });
    publishAccountChange();
    scheduleIdleStop();
    return { cancelled: response.status === "canceled" };
  }

  async function logout() {
    if (authBusy) throw new Error("A ChatGPT account change is already in progress.");
    authBusy = true;
    authGeneration += 1;
    try {
      for (const [loginId, login] of logins) {
        if (login.state === "pending") await cancel(loginId);
      }
      const server = await ensureAppServer();
      await server.request("account/logout");
      const value = await readAccount(server);
      if (value.signedIn) throw new Error("ChatGPT still reports a signed-in account after logout. Refresh and try again.");
      server.authStamp = nativeAuthStamp(server.codexHome);
      publishAccountChange();
      return value;
    } finally { authBusy = false; scheduleIdleStop(); }
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
            ...require("./providerModelTiers.cjs").modelServiceTiers(model.serviceTiers, model.defaultServiceTier, `Codex model '${model.model}'`),
          });
        }
        if (response.nextCursor === null) {
          if (server !== appServer || server.codexHome !== options.codexHome() || server.binary !== options.codexBinary()) {
            throw new Error("Codex account scope changed during model discovery.");
          }
          return result;
        }
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

  return { account, rateLimits, start, status, cancel, logout, dispose, models };
}

module.exports = { createChatgptSignIn };
