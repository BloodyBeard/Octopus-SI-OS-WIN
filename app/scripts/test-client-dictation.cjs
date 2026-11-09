const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");
const { createClientDictationRecordings } = require("../electron/clientDictationRecordings.cjs");
const { transcribeClientDictation, dictationFailure } = require("../electron/clientDictation.cjs");

const audio = () => {
  const bytes = Buffer.alloc(44 + 7200 * 2);
  bytes.write("RIFF"); bytes.write("WAVE", 8);
  return { bytes, durationMs: 300, mediaType: "audio/wav", provider: "codex", sampleRateHz: 24_000 };
};

test("dictation audio survives reopening, respects explicit off, stays profile-local and retains twenty recordings", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-dictation-test-"));
  try {
    let store = createClientDictationRecordings(root);
    assert.equal((await store.state("live")).enabled, true);
    const first = await store.save("live", audio());
    await store.update("live", { id: first.id, text: "Saved words", status: "transcribed" });
    store = createClientDictationRecordings(root);
    assert.equal((await store.read("live", first.id)).entry.text, "Saved words");
    assert.deepEqual((await store.read("live", first.id)).bytes, audio().bytes);
    assert.equal((await store.state("lab")).recordings.length, 0);
    await store.configure("live", false);
    store = createClientDictationRecordings(root);
    assert.equal(await store.save("live", audio()), null);
    assert.equal((await store.state("live")).recordings.length, 1);
    await store.configure("live", true);
    for (let i = 0; i < 20; i += 1) await store.save("live", audio());
    const state = await store.state("live");
    assert.equal(state.recordings.length, 20);
    assert.ok(!state.recordings.some((entry) => entry.id === first.id));
    await assert.rejects(store.read("live", "../../outside"), /Invalid recording/);
    await fs.truncate(path.join(root, "live", state.recordings[0].id, "audio.wav"), 10);
    await assert.rejects(store.read("live", state.recordings[0].id), /incomplete/);
    await store.clear("live");
    assert.equal((await store.state("live")).recordings.length, 0);
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});

test("transcription exposes empty speech, authorization, timeout and cancellation as safe typed failures", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-dictation-provider-test-"));
  try {
    const executable = path.join(root, "fixture-provider");
    await fs.writeFile(executable, `#!/usr/bin/env node
require("node:readline").createInterface({ input: process.stdin }).on("line", line => {
  const request = JSON.parse(line);
  if (request.id === 1) process.stdout.write(JSON.stringify({ id: 1, result: {} }) + "\\n");
  if (request.id === 2) process.stdout.write(JSON.stringify({ id: 2, result: { authMethod: "chatgpt", authToken: process.env.FIXTURE_SIGNED_OUT ? "" : "test-only-token" } }) + "\\n");
});
`, { mode: 0o700 });
    const options = { codexExecutable: executable, cwd: root, env: { PATH: process.env.PATH } };
    await assert.rejects(transcribeClientDictation(audio(), { ...options, fetch: async () => new Response('{"text":"  "}', { status: 200 }) }), (error) => {
      assert.deepEqual(dictationFailure(error), { code: "empty-transcript", message: "No speech was recognized. Try again or record a new dictation." });
      return true;
    });
    const result = await transcribeClientDictation(audio(), { ...options, fetch: async () => new Response('{"text":"Actual words"}', { status: 200 }) });
    assert.equal(result.text, "Actual words");
    await assert.rejects(transcribeClientDictation(audio(), { ...options, fetch: async () => new Response('{"error":{"message":"sensitive backend detail"}}', { status: 401 }) }), (error) => {
      assert.equal(dictationFailure(error).code, "authorization");
      assert.ok(!dictationFailure(error).message.includes("sensitive"));
      return true;
    });
    await assert.rejects(transcribeClientDictation({ ...audio(), timeoutMs: 1 }, { ...options, fetch: async (_url, { signal }) => new Promise((_resolve, reject) => {
      const keepAlive = setTimeout(() => reject(new Error("Timeout was not delivered")), 6_000);
      signal.addEventListener("abort", () => { clearTimeout(keepAlive); reject(signal.reason); }, { once: true });
    }) }), (error) => dictationFailure(error).code === "timeout");
    const controller = new AbortController();
    const request = transcribeClientDictation(audio(), { ...options, signal: controller.signal, fetch: async (_url, { signal }) => {
      controller.abort();
      signal.throwIfAborted();
    } });
    await assert.rejects(request, (error) => dictationFailure(error).code === "cancelled");
    delete require.cache[require.resolve("../electron/clientDictation.cjs")];
    const freshProvider = require("../electron/clientDictation.cjs");
    await assert.rejects(freshProvider.transcribeClientDictation(audio(), { ...options, env: { ...options.env, FIXTURE_SIGNED_OUT: "1" }, fetch: async () => { throw new Error("Must not transcribe without authorization"); } }), (error) => freshProvider.dictationFailure(error).code === "authorization");
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});
