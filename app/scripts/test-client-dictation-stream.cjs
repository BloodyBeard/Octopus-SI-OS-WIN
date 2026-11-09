const assert = require("node:assert/strict");
const { EventEmitter } = require("node:events");
const { test } = require("node:test");
const { createClientDictationStream } = require("../electron/clientDictationStream.cjs");

function fixture(resolveAuth = async () => ({ token: "private-token", accountId: "account" })) {
  const events = [], messages = [];
  let socket;
  class WebSocket extends EventEmitter {
    static OPEN = 1;
    readyState = 1;
    bufferedAmount = 0;
    constructor(url, protocols, options) {
      super();
      socket = this;
      this.connection = { url, protocols, options };
      queueMicrotask(() => this.emit("open"));
    }
    send(data) {
      const message = JSON.parse(data);
      messages.push(message);
      if (message.type === "session.start") this.event({ type: "session.started", session: { status: "active" } });
    }
    event(event) { this.emit("message", Buffer.from(JSON.stringify(event)), false); }
    terminate() { this.readyState = 3; this.emit("close"); }
  }
  const stream = createClientDictationStream({ attemptId: "a", sampleRateHz: 24_000 }, { WebSocket, resolveAuth, onEvent: (event) => events.push(event) });
  return { stream, events, messages, socket: () => socket };
}

test("dictation streams PCM and ordered utterance revisions, and finishes only on confirmed session close", async () => {
  const f = fixture();
  await f.stream.ready;
  assert.deepEqual(f.socket().connection.protocols, ["chatgpt-dictation", "codex-desktop"]);
  assert.equal(f.socket().connection.options.headers.Authorization, "Bearer private-token");
  assert.equal(f.messages[0].config.transcript_delivery_mode, "segment");
  assert.equal(f.messages[0].config.sample_rate_hz, 24_000);
  f.stream.append(Buffer.from([1, 0, 2, 0]));
  assert.deepEqual(f.messages.at(-1), { type: "audio.append", audio: "AQACAA==" });
  f.socket().event({ type: "transcript.segment", utterance_id: "first", revision: 2, text: "Hello" });
  f.socket().event({ type: "transcript.segment", utterance_id: "first", revision: 1, text: "Old" });
  f.socket().event({ type: "transcript.segment", utterance_id: "second", revision: 1, text: "world" });
  assert.equal(f.events.at(-1).text, "Hello world");
  const final = f.stream.finish();
  await Promise.resolve();
  assert.equal(f.messages.at(-1).type, "session.close");
  f.socket().event({ type: "transcript.final", utterance_id: "second", revision: 1, text: "world." });
  f.socket().event({ type: "transcript.final", utterance_id: "first", revision: 3, text: "Hello" });
  f.socket().event({ type: "session.updated", session: { status: "closed" } });
  assert.equal((await final).text, "Hello world.");
  assert.equal(JSON.stringify(f.events).includes("private-token"), false);
});

test("interrupted or unfinished streaming speech fails explicitly and never becomes a successful final transcript", async () => {
  const f = fixture();
  await f.stream.ready;
  f.socket().event({ type: "transcript.segment", utterance_id: "first", revision: 1, text: "Unfinished" });
  const final = f.stream.finish();
  await Promise.resolve();
  f.socket().event({ type: "session.updated", session: { status: "closed" } });
  await assert.rejects(final, /before the text was finalized/);
  assert.equal(f.events.at(-1).type, "error");
  const disconnected = fixture();
  await disconnected.stream.ready;
  disconnected.socket().emit("close");
  await assert.rejects(disconnected.stream.finish(), /connection closed/);
});

test("cancel during authorization cannot open a socket or accept a late transcript", async () => {
  let authorize;
  const f = fixture(() => new Promise((resolve) => { authorize = resolve; }));
  f.stream.close();
  authorize({ token: "late-token" });
  await assert.rejects(f.stream.ready, { name: "AbortError" });
  await Promise.resolve();
  assert.equal(f.socket(), undefined);
  assert.deepEqual(f.events, []);
});
