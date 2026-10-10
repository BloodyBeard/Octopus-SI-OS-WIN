const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawn } = require("node:child_process");

const CHATGPT_TRANSCRIPTION_URL = new URL("https://chatgpt.com/backend-api/transcribe");
const MAX_AUDIO_BYTES = 10 * 1024 * 1024;
const MAX_RESPONSE_BYTES = 1024 * 1024;
const MAX_DURATION_MS = 120_000;
const AUTH_TIMEOUT_MS = 10_000;
const AUTH_CACHE_MS = 60_000;
const USER_AGENT = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15";
let authCache = null;

async function transcribeClientDictation(request, options = {}) {
  const audio = decodeAudio(request);
  const executable = options.codexExecutable || resolveCodexExecutable(options.env || process.env);
  const authInput = {
    executable,
    cwd: options.cwd || os.homedir(),
    env: options.env || process.env,
    clientVersion: options.clientVersion || "0.1.0",
  };
  let auth = await resolveCodexAuth(authInput, false);
  let response = await requestTranscription(audio, auth, request.timeoutMs, options.fetch);
  if (response.statusCode === 401 || response.statusCode === 403) {
    auth = await resolveCodexAuth(authInput, true);
    response = await requestTranscription(audio, auth, request.timeoutMs, options.fetch);
  }
  if (response.statusCode < 200 || response.statusCode >= 300) {
    throw new Error(transcriptionError(response.statusCode, response.body));
  }
  const payload = parseJsonObject(response.body);
  const text = nonEmptyString(payload.text) || nonEmptyString(payload.transcript);
  if (!text) throw new Error("The transcription response did not include any text.");
  return { provider: "codex", model: "ChatGPT transcription", text };
}

function decodeAudio(request) {
  if (!request || request.provider !== "codex") {
    throw new Error("Dictation requires the Codex client provider.");
  }
  if (request.mediaType !== "audio/wav" || request.sampleRateHz !== 24_000) {
    throw new Error("Dictation requires 24 kHz mono WAV audio.");
  }
  if (!Number.isFinite(request.durationMs) || request.durationMs <= 0 || request.durationMs > MAX_DURATION_MS) {
    throw new Error("Dictation recordings must be between 1 ms and 120 seconds.");
  }
  const audio = toBuffer(request.bytes);
  if (!audio.length || audio.length > MAX_AUDIO_BYTES) {
    throw new Error("Dictation audio must be non-empty and no larger than 10 MB.");
  }
  if (audio.length < 44 || audio.toString("ascii", 0, 4) !== "RIFF" || audio.toString("ascii", 8, 12) !== "WAVE") {
    throw new Error("The recorded audio is not a valid WAV file.");
  }
  return audio;
}

function toBuffer(value) {
  if (Buffer.isBuffer(value)) return value;
  if (value instanceof ArrayBuffer) return Buffer.from(value);
  if (ArrayBuffer.isView(value)) return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  throw new Error("Dictation audio bytes are missing.");
}

function resolveCodexExecutable(env) {
  const binary = process.platform === "win32" ? "codex.exe" : "codex";
  const candidates = [
    nonEmptyString(env.OCTOPUS_CODEX_PATH),
    ...(nonEmptyString(env.PATH)?.split(path.delimiter).map((entry) => path.join(entry, binary)) ?? []),
    process.platform === "darwin" ? "/Applications/ChatGPT.app/Contents/Resources/codex" : "",
    process.platform === "darwin" ? path.join(os.homedir(), "Applications/ChatGPT.app/Contents/Resources/codex") : "",
  ].filter(Boolean);
  for (const candidate of [...new Set(candidates)]) {
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      return candidate;
    } catch {
      continue;
    }
  }
  throw new Error("Codex CLI was not found. Install Codex, sign in with ChatGPT, and try dictation again.");
}

async function resolveCodexAuth(input, forceRefresh) {
  if (!forceRefresh && authCache && Date.now() - authCache.loadedAt < AUTH_CACHE_MS) return authCache.auth;
  const auth = await discoverCodexAuth(input, true);
  authCache = { auth, loadedAt: Date.now() };
  return auth;
}

function discoverCodexAuth(input, refresh) {
  return new Promise((resolve, reject) => {
    const child = spawn(input.executable, ["app-server"], {
      cwd: input.cwd,
      env: input.env,
      shell: false,
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    let settled = false;
    let stdout = "";
    const timeout = setTimeout(() => finish(new Error("Timed out while reading ChatGPT authorization from Codex.")), AUTH_TIMEOUT_MS);
    timeout.unref?.();

    const finish = (error, token) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      child.kill();
      if (error) reject(error);
      else resolve(token);
    };
    const send = (message) => {
      if (!child.stdin.destroyed) child.stdin.write(`${JSON.stringify(message)}\n`);
    };

    child.once("error", (error) => finish(new Error(`Could not start Codex authorization discovery: ${error.message}`)));
    child.once("close", (code) => {
      if (!settled) finish(new Error(`Codex authorization discovery exited before responding (${code ?? "unknown"}).`));
    });
    child.stderr.on("data", () => undefined);
    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString("utf8");
      if (stdout.length > 1024 * 1024) {
        finish(new Error("Codex authorization discovery exceeded its response limit."));
        return;
      }
      const lines = stdout.split("\n");
      stdout = lines.pop() || "";
      for (const line of lines) {
        let message;
        try {
          message = JSON.parse(line);
        } catch {
          continue;
        }
        if (message.id === 1) {
          if (message.error) {
            finish(new Error("Codex rejected client authorization initialization."));
            return;
          }
          send({ jsonrpc: "2.0", method: "initialized", params: {} });
          send({ jsonrpc: "2.0", id: 2, method: "getAuthStatus", params: { includeToken: true, refreshToken: refresh } });
          continue;
        }
        if (message.id !== 2) continue;
        if (message.error) {
          finish(new Error("Codex could not read the local ChatGPT authorization."));
          return;
        }
        const result = object(message.result);
        const authMethod = nonEmptyString(result.authMethod);
        const token = nonEmptyString(result.authToken);
        if (!token) {
          finish(new Error("No ChatGPT session token is available. Sign in to ChatGPT in Codex."));
          return;
        }
        if (authMethod !== "chatgpt" && authMethod !== "chatgptAuthTokens") {
          finish(new Error("Dictation requires a ChatGPT-authenticated Codex session."));
          return;
        }
        finish(null, {
          accountId: chatGptAccountIdFromToken(token),
          token,
        });
      }
    });
    send({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        clientInfo: { name: "octopus-desktop", title: "Octopus", version: input.clientVersion },
        capabilities: { experimentalApi: true },
      },
    });
  });
}

async function requestTranscription(audio, auth, timeoutMs, fetchOverride) {
  if (typeof fetchOverride !== "function") {
    throw new Error("Electron networking is unavailable for client dictation.");
  }
  const body = new FormData();
  body.append("file", new Blob([audio], { type: "audio/wav" }), "voice.wav");
  const abortController = new AbortController();
  const timeout = setTimeout(
    () => abortController.abort(new Error("Voice transcription timed out.")),
    Math.min(120_000, Math.max(5_000, Number(timeoutMs) || 30_000)),
  );
  timeout.unref?.();
  try {
    const response = await fetchOverride(CHATGPT_TRANSCRIPTION_URL.toString(), {
      method: "POST",
      redirect: "error",
      signal: abortController.signal,
      headers: {
        Authorization: `Bearer ${auth.token}`,
        ...(auth.accountId ? { "ChatGPT-Account-Id": auth.accountId } : {}),
        "User-Agent": USER_AGENT,
      },
      body,
    });
    return {
      body: await readBoundedResponse(response),
      statusCode: response.status,
    };
  } finally {
    clearTimeout(timeout);
  }
}

async function readBoundedResponse(response) {
  const contentLength = Number(response.headers.get("content-length"));
  if (Number.isFinite(contentLength) && contentLength > MAX_RESPONSE_BYTES) {
    await response.body?.cancel();
    throw new Error("The transcription response exceeded 1 MB.");
  }
  if (!response.body) return "";
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > MAX_RESPONSE_BYTES) {
      await reader.cancel();
      throw new Error("The transcription response exceeded 1 MB.");
    }
    chunks.push(Buffer.from(value));
  }
  return Buffer.concat(chunks).toString("utf8");
}

function chatGptAccountIdFromToken(token) {
  const parts = token.split(".");
  if (parts.length !== 3 || !parts[1]) return "";
  try {
    const payload = object(JSON.parse(Buffer.from(parts[1], "base64url").toString("utf8")));
    const authClaims = object(payload["https://api.openai.com/auth"]);
    return safeHeaderValue(authClaims.chatgpt_account_id);
  } catch {
    return "";
  }
}

function safeHeaderValue(value) {
  const normalized = nonEmptyString(value);
  return normalized && !/[\r\n]/u.test(normalized) ? normalized : "";
}

function transcriptionError(statusCode, body) {
  const payload = parseJsonObject(body);
  const providerMessage = nonEmptyString(object(payload.error).message) || nonEmptyString(payload.message);
  if (providerMessage) return providerMessage;
  if (statusCode === 401) return "Your ChatGPT login has expired. Sign in again in Codex.";
  if (statusCode === 403) return "ChatGPT rejected the transcription request for this Codex login.";
  return `Transcription failed with status ${statusCode}.`;
}

function parseJsonObject(value) {
  try {
    return object(JSON.parse(value));
  } catch {
    return {};
  }
}

function object(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

function nonEmptyString(value) {
  return typeof value === "string" && value.trim() ? value.trim() : "";
}

module.exports = {
  CHATGPT_TRANSCRIPTION_URL: CHATGPT_TRANSCRIPTION_URL.toString(),
  decodeAudio,
  resolveCodexExecutable,
  transcribeClientDictation,
};
