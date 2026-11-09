const { resolveClientDictationAuth, dictationFailure } = require("./clientDictation.cjs");
const limits = require("./clientDictationLimits.json");

const STREAM_URL = "wss://chatgpt.com/backend-api/dictation/stream?dictation_surface=work&client_type=desktop";
const MAX_BUFFER_BYTES = 4 * 1024 * 1024;

function createClientDictationStream(request, options = {}) {
  if (!Number.isInteger(request?.sampleRateHz) || request.sampleRateHz < 8_000 || request.sampleRateHz > 96_000) {
    throw new Error("Invalid dictation sample rate.");
  }
  const WebSocket = options.WebSocket || require("ws");
  const utterances = new Map();
  let socket;
  let closed = false;
  let started = false;
  let finishing = false;
  let failed;
  let bytesSent = 0;
  let timer;
  let resolveStart, rejectStart, resolveFinish, rejectFinish;
  const ready = new Promise((resolve, reject) => { resolveStart = resolve; rejectStart = reject; });
  const finished = new Promise((resolve, reject) => { resolveFinish = resolve; rejectFinish = reject; });
  ready.catch(() => {});
  finished.catch(() => {});
  const text = (partial) => [...utterances.values()].map((entry) => entry.final?.text ?? (partial ? entry.partial?.text : "") ?? "").filter(Boolean).join(" ").trim();
  const dispose = () => {
    clearTimeout(timer);
    closed = true;
    socket?.terminate();
  };
  const fail = (error) => {
    if (closed) return;
    failed = error;
    rejectStart(error);
    rejectFinish(error);
    options.onEvent?.({ type: "error", error: dictationFailure(error) });
    dispose();
  };
  const timeout = (milliseconds) => {
    clearTimeout(timer);
    timer = setTimeout(() => fail(new DOMException("Dictation took too long.", "TimeoutError")), milliseconds);
    timer.unref?.();
  };
  const send = (message) => {
    if (socket?.readyState !== WebSocket.OPEN || closed) throw failed ?? new Error("Dictation is not connected.");
    const data = JSON.stringify(message);
    if (socket.bufferedAmount + Buffer.byteLength(data) > MAX_BUFFER_BYTES) throw new Error("Dictation connection is too slow.");
    socket.send(data);
  };

  timeout(10_000);
  void (async () => {
    const auth = await (options.resolveAuth || resolveClientDictationAuth)(options);
    if (closed) return;
    socket = new WebSocket(STREAM_URL, ["chatgpt-dictation", "codex-desktop"], {
      headers: {
        Authorization: `Bearer ${auth.token}`,
        ...(auth.accountId ? { "ChatGPT-Account-Id": auth.accountId } : {}),
        originator: "octopus-desktop",
        "User-Agent": `Octopus Desktop/${options.clientVersion || "0.1.0"} (${process.platform}; ${process.arch})`,
      },
      maxPayload: 8 * 1024 * 1024,
      followRedirects: false,
      handshakeTimeout: 10_000,
    });
    socket.on("open", () => {
      if (closed) return;
      try {
        send({
          type: "session.start", dictation_session_id: request.attemptId, attempt_id: request.attemptId,
          client_capabilities: ["server_stop_requested"],
          config: {
            input_audio_format: "pcm16", sample_rate_hz: request.sampleRateHz, num_channels: 1,
            max_buffer_size_bytes: MAX_BUFFER_BYTES, max_utterance_duration_ms: 30_000, session_ttl_ms: 300_000,
            provider_mode: "streaming_sse", transcript_delivery_mode: "segment",
            vad: { type: "server_vad", threshold: 0.5, prefix_padding_ms: 300, silence_duration_ms: 500 },
          },
        });
      } catch (error) { fail(error); }
    });
    socket.on("message", (data, binary) => {
      if (closed) return;
      try {
        if (binary) throw new Error("Dictation returned unexpected audio.");
        const event = JSON.parse(data.toString("utf8"));
        if (!event || typeof event.type !== "string") throw new Error("Invalid dictation event.");
        if (event.type === "session.started") {
          if (started || event.session?.status !== "active") throw new Error("Invalid dictation session.");
          started = true;
          clearTimeout(timer);
          resolveStart();
        } else if (event.type === "transcript.segment" || event.type === "transcript.final") {
          if (!started || typeof event.utterance_id !== "string" || !Number.isInteger(event.revision) || typeof event.text !== "string" || event.text.length > 1_000_000) throw new Error("Invalid dictation transcript.");
          const entry = utterances.get(event.utterance_id) ?? { partial: null, final: null };
          utterances.set(event.utterance_id, entry);
          if (event.type === "transcript.final" && event.revision >= (entry.final?.revision ?? 0)) {
            entry.final = { revision: event.revision, text: event.text };
            entry.partial = null;
          } else if (event.type === "transcript.segment" && !entry.final && event.revision >= (entry.partial?.revision ?? 0)) {
            entry.partial = { revision: event.revision, text: event.text };
          }
          options.onEvent?.({ type: "transcript", text: text(true) });
        } else if (event.type === "session.stop_requested") {
          options.onEvent?.({ type: "stop-requested" });
        } else if (event.type === "session.updated" && event.session?.status === "closed") {
          if (!finishing || [...utterances.values()].some((entry) => entry.partial && !entry.final)) throw new Error("Dictation ended before the text was finalized.");
          resolveFinish({ provider: "codex", model: "ChatGPT streaming transcription", text: text(false) });
          dispose();
        } else if (event.type === "transcript.failed" || event.type === "session.error" && event.fatal) {
          throw new Error("Couldn't transcribe this recording.");
        } else if (!["session.updated", "session.error", "speech.started", "speech.stopped", "transcript.delta", "audio.frame_processed", "asset.ready", "asset.committed", "asset.failed"].includes(event.type)) {
          throw new Error("Unknown dictation event.");
        }
      } catch (error) { fail(error); }
    });
    socket.on("unexpected-response", (_request, response) => {
      response.resume();
      fail(Object.assign(new Error("Dictation connection was rejected."), { statusCode: response.statusCode, code: [401, 403].includes(response.statusCode) ? "authorization" : response.statusCode === 429 ? "rate-limit" : "transcription" }));
    });
    socket.on("error", (error) => fail(error));
    socket.on("close", () => { if (!closed) fail(new Error("Dictation connection closed before finalizing.")); });
  })().catch(fail);

  return {
    ready,
    append(bytes) {
      if (!started || finishing || closed) return;
      try {
        const audio = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
        bytesSent += audio.byteLength;
        if (!audio.length || audio.length % 2 || audio.length > request.sampleRateHz * 2 || bytesSent > request.sampleRateHz * 2 * limits.maxDurationMs / 1_000) throw new Error("Invalid dictation audio frame.");
        send({ type: "audio.append", audio: audio.toString("base64") });
      } catch (error) { fail(error); }
    },
    async finish() {
      await ready;
      if (failed) throw failed;
      if (!finishing) {
        finishing = true;
        timeout(8_000);
        try { send({ type: "session.close" }); } catch (error) { fail(error); }
      }
      return finished;
    },
    close() {
      if (closed) return;
      const error = new DOMException("Dictation cancelled.", "AbortError");
      rejectStart(error);
      rejectFinish(error);
      dispose();
    },
  };
}

module.exports = { createClientDictationStream };
