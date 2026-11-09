const fs = require("node:fs/promises");
const path = require("node:path");
const { randomUUID } = require("node:crypto");
const { decodeAudio } = require("./clientDictation.cjs");
const { maxDurationMs } = require("./clientDictationLimits.json");

const MAX_RECORDINGS = 20;
const ID = /^[a-f0-9-]{36}$/;
const MAX_AUDIO_BYTES = 44 + maxDurationMs / 1_000 * 24_000 * 2;

function createClientDictationRecordings(root) {
  let pending = Promise.resolve();
  const serial = (operation) => {
    const next = pending.then(operation);
    pending = next.catch(() => undefined);
    return next;
  };
  const directory = (profile) => {
    if (profile !== "live" && profile !== "lab") throw new Error("Invalid dictation profile.");
    return path.join(root, profile);
  };
  const recordingPath = (profile, id) => {
    if (typeof id !== "string" || !ID.test(id)) throw new Error("Invalid recording id.");
    return path.join(directory(profile), id);
  };
  const readJson = async (file, missing) => {
    try { return JSON.parse(await fs.readFile(file, "utf8")); }
    catch (error) { if (error.code === "ENOENT") return missing; throw error; }
  };
  const writeJson = async (file, value) => {
    const temporary = `${file}.${randomUUID()}.tmp`;
    try {
      await fs.writeFile(temporary, `${JSON.stringify(value)}\n`, { mode: 0o600, flag: "wx" });
      await fs.rename(temporary, file);
    } finally { await fs.rm(temporary, { force: true }); }
  };
  const settings = async (profile) => {
    const value = await readJson(path.join(directory(profile), "settings.json"), { version: 1, enabled: true });
    if (value.version !== 1 || typeof value.enabled !== "boolean") throw new Error("Dictation storage settings are invalid.");
    return value;
  };
  const metadata = async (profile, id) => {
    const value = await readJson(path.join(recordingPath(profile, id), "recording.json"), null);
    if (!value || value.version !== 1 || value.id !== id || !Number.isFinite(value.createdAtMs)
      || !Number.isFinite(value.durationMs) || value.durationMs <= 0 || value.durationMs > maxDurationMs
      || !Number.isFinite(value.byteLength) || value.byteLength < 44 || value.byteLength > MAX_AUDIO_BYTES
      || !["recorded", "transcribed", "failed"].includes(value.status) || typeof value.text !== "string") {
      throw new Error("The saved dictation cannot be read.");
    }
    return value;
  };
  const list = async (profile) => {
    let entries;
    try { entries = await fs.readdir(directory(profile), { withFileTypes: true }); }
    catch (error) { if (error.code === "ENOENT") return []; throw error; }
    const recordings = await Promise.all(entries.filter((entry) => entry.isDirectory() && ID.test(entry.name))
      .map((entry) => metadata(profile, entry.name)));
    return recordings.sort((left, right) => right.createdAtMs - left.createdAtMs);
  };
  return {
    state: (profile) => serial(async () => ({ ...(await settings(profile)), recordings: await list(profile), maxRecordings: MAX_RECORDINGS })),
    configure: (profile, enabled) => serial(async () => {
      if (typeof enabled !== "boolean") throw new Error("Choose whether to save dictation audio.");
      await fs.mkdir(directory(profile), { recursive: true, mode: 0o700 });
      await writeJson(path.join(directory(profile), "settings.json"), { version: 1, enabled });
      return { ...(await settings(profile)), recordings: await list(profile), maxRecordings: MAX_RECORDINGS };
    }),
    save: (profile, request) => serial(async () => {
      if (!(await settings(profile)).enabled) return null;
      const bytes = decodeAudio(request);
      const previous = await list(profile);
      const id = randomUUID();
      const target = recordingPath(profile, id);
      const staging = path.join(directory(profile), `.pending-${id}`);
      const entry = { version: 1, id, createdAtMs: Math.max(Date.now(), (previous[0]?.createdAtMs ?? 0) + 1), durationMs: request.durationMs, byteLength: bytes.length, status: "recorded", text: "" };
      await fs.mkdir(staging, { recursive: true, mode: 0o700 });
      try {
        await fs.writeFile(path.join(staging, "audio.wav"), bytes, { flag: "wx", mode: 0o600 });
        await writeJson(path.join(staging, "recording.json"), entry);
        await fs.rename(staging, target);
      } finally { await fs.rm(staging, { recursive: true, force: true }); }
      for (const old of previous.slice(MAX_RECORDINGS - 1)) {
        await fs.rm(recordingPath(profile, old.id), { recursive: true });
      }
      return entry;
    }),
    update: (profile, request) => serial(async () => {
      const entry = await metadata(profile, request.id);
      if (!["transcribed", "failed"].includes(request.status) || typeof request.text !== "string" || request.text.length > 100_000) {
        throw new Error("Invalid dictation result.");
      }
      const updated = { ...entry, status: request.status, text: request.text };
      await writeJson(path.join(recordingPath(profile, request.id), "recording.json"), updated);
      return updated;
    }),
    read: (profile, id) => serial(async () => {
      const entry = await metadata(profile, id);
      const bytes = await fs.readFile(path.join(recordingPath(profile, id), "audio.wav"));
      if (bytes.length !== entry.byteLength) throw new Error("The saved recording is incomplete.");
      decodeAudio({ bytes, durationMs: entry.durationMs, provider: "codex", mediaType: "audio/wav", sampleRateHz: 24_000 });
      return { entry, bytes };
    }),
    remove: (profile, id) => serial(async () => {
      await fs.rm(recordingPath(profile, id), { recursive: true, force: true });
    }),
    clear: (profile) => serial(async () => {
      for (const entry of await list(profile)) await fs.rm(recordingPath(profile, entry.id), { recursive: true, force: true });
    }),
    directory,
  };
}

module.exports = { createClientDictationRecordings };
