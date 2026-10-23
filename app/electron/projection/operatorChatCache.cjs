const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");
const { verifiedArtifactFilesUnchanged } = require("../contentAddressedArtifact.cjs");
const { operatorChatExecutionScope, operatorChatRuntimeProviderId, operatorChatTurnText } = require("./operatorChatEvents.cjs");
const { movementRunId, readStableArtifactBytes } = require("./movementRows.cjs");
const { rowTimeMs } = require("./values.cjs");

const CACHE_VERSION = 2;
const CACHE_MAX_BYTES = 16 * 1024 * 1024;

function digest(value) {
  return crypto.createHash("sha256").update(JSON.stringify(value)).digest("hex");
}

function rowDigest(rows, count) {
  const hash = crypto.createHash("sha256");
  for (let index = 0; index < count; index++) {
    const row = rows[index];
    hash.update(JSON.stringify([row.seq, rowTimeMs(row), row.kind, row.from, row.to, row.artifact,
      row.source_file, movementRunId(row), row.runtime?.checkpoint ?? row.invocation_metadata?.runtime?.checkpoint]));
    hash.update("\n");
  }
  return hash.digest("hex");
}

async function readCache(file, warn) {
  try {
    const document = JSON.parse((await readStableArtifactBytes(file, CACHE_MAX_BYTES, "client projection cache")).toString("utf8"));
    if (document.version !== CACHE_VERSION) return null;
    if (document.digest !== digest(document.value)) throw new Error("invalid cache digest; rebuilding from ledger");
    return document.value;
  } catch (error) {
    if (error?.code !== "ENOENT") warn(error);
    return null;
  }
}

async function writeCache(file, value, warn) {
  const encoded = JSON.stringify({ version: CACHE_VERSION, digest: digest(value), value });
  if (Buffer.byteLength(encoded) > CACHE_MAX_BYTES) return;
  const temporary = `${file}.${crypto.randomUUID()}.tmp`;
  try {
    await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.writeFile(temporary, encoded, { flag: "wx", mode: 0o600 });
    await fs.rename(temporary, file);
  } catch (error) {
    warn(error);
  } finally {
    await fs.rm(temporary, { force: true }).catch(warn);
  }
}

async function cachedOperatorChatArtifactText({ home, entry, row, protocol, direction, limit, resolve, warnings }) {
  const textOf = (payload) => operatorChatTurnText(payload, direction).replace(/\s+/g, " ").trim().slice(0, limit);
  if (row.source_file !== path.join(home, "ledger", "movements.jsonl")) return textOf(await resolve(row));
  const basis = digest([row.source_file, row.artifact, protocol.artifactReadProjection, direction, limit]);
  const file = path.join(home, "client", "projection-cache", "previews", `${digest([entry.runId, entry.chatInterfaceId, direction, limit])}.json`);
  const warn = (error) => warnings?.push(`Task preview cache: ${String(error?.message ?? error)}`);
  const cached = await readCache(file, warn);
  if (cached?.basis === basis && typeof cached.text === "string") {
    try {
      if (await verifiedArtifactFilesUnchanged(cached.files, path.join(home, "ledger", "blobs"))) return cached.text;
    } catch (error) { warn(error); }
  }
  let files;
  const text = textOf(await resolve(row, (verified) => { files = verified; }));
  if (files?.length) await writeCache(file, { basis, files, text }, warn);
  return text;
}

async function cachedOperatorChatTurnState({ projection, entry, protocol, reduce, resolve }) {
  const initial = { open: false, state: "idle", lastMovementSeq: -1, updatedAtMs: 0 };
  const rows = [...entry.activity].sort((left, right) => left.seq - right.seq);
  const ledgerPath = path.join(projection.octopusHome, "ledger", "movements.jsonl");
  if (!rows.length || rows.some(row => row.source_file !== ledgerPath)) return reduce(initial, rows, resolve);
  const ledgerStat = await fs.stat(ledgerPath);
  const scope = operatorChatExecutionScope(entry);
  const client = entry.record?.interface_state?.[entry.chatInterfaceId];
  const basis = digest([CACHE_VERSION, ledgerPath, ledgerStat.dev, ledgerStat.ino,
    entry.runId, entry.chatInterfaceId, operatorChatRuntimeProviderId(entry), entry.externalInputs,
    scope && [[...scope.checkpoints].sort(), scope.sourceMovementSeq],
    client?.client_surface_role, client?.voice_context?.delegation_interface_id, protocol]);
  const directory = path.join(projection.octopusHome, "client", "projection-cache", "turns");
  const file = path.join(directory, `${digest([entry.runId, entry.chatInterfaceId])}.json`);
  const warn = (error) => projection.warnings?.push(`Task status cache ${entry.runId}: ${String(error?.message ?? error)}`);
  let checkpoint;
  const value = await readCache(file, warn);
  try {
    if (value?.basis === basis
      && Number.isSafeInteger(value.count) && value.count > 0 && value.count <= rows.length
      && value.rowsDigest === rowDigest(rows, value.count)
      && value.state.lastMovementSeq === rows[value.count - 1].seq
      && typeof value.state.open === "boolean" && ["idle", "waiting", "working", "failed"].includes(value.state.state)
      && await verifiedArtifactFilesUnchanged(value.files, path.join(projection.octopusHome, "ledger", "blobs"))) {
      const state = { ...value.state, nativeTurns: new Map(value.state.nativeTurns),
        recordedInputs: new Map(value.state.recordedInputs.map(([key, ids]) => [key, new Set(ids)])) };
      checkpoint = { ...value, state };
    }
  } catch (error) {
    warn(error);
  }
  if (checkpoint?.count === rows.length) return checkpoint.state;
  const files = new Map((checkpoint?.files ?? []).map(file => [file.path, file]));
  let cacheable = true;
  const state = await reduce(checkpoint?.state ?? initial, rows.slice(checkpoint?.count ?? 0), async (row) => {
    let observed = false;
    const payload = await resolve(row, (verified) => {
      observed = true;
      if (!verified?.length) { cacheable = false; return; }
      for (const file of verified) {
        const previous = files.get(file.path);
        if (previous && JSON.stringify(previous.identity) !== JSON.stringify(file.identity)) cacheable = false;
        files.set(file.path, file);
      }
    });
    if (!observed) cacheable = false;
    return payload;
  });
  if (!cacheable) return state;
  const next = { basis, count: rows.length, rowsDigest: rowDigest(rows, rows.length), files: [...files.values()],
    state: { ...state, nativeTurns: [...state.nativeTurns], recordedInputs: [...state.recordedInputs].map(([key, ids]) => [key, [...ids]]) } };
  await writeCache(file, next, warn);
  return state;
}

module.exports = { cachedOperatorChatTurnState, cachedOperatorChatArtifactText };
