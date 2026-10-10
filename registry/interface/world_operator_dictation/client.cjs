const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");

const RUN_ID = "run_client_dictation";
const EE_ID = "ee_operator_interaction";
const WORKER_ID = "system_worker_dictation";
const MICROPHONE_INTERFACE = "world_microphone";
const SPEECH_INTERFACE = "world_speech_to_text";
const CLIENT_INTERFACE = "world_operator_dictation";
const AUDIO_LABEL = "microphone.audio";
const TRANSCRIPT_LABEL = "speech.transcript";
const FAILURE_LABEL = "speech.transcribe.result";
const MAX_AUDIO_BYTES = 32 * 1024 * 1024;

let authoringTail = Promise.resolve();

async function transcribeOperatorDictation(options, request) {
  const operation = authoringTail.then(() => transcribe(options, request));
  authoringTail = operation.catch(() => {});
  return operation;
}

async function transcribe(options, request) {
  const octopusHome = absolutePath(options.octopusHome(), "octopusHome");
  const bytes = Buffer.isBuffer(request?.bytes) ? request.bytes : null;
  if (!bytes || bytes.length === 0 || bytes.length > MAX_AUDIO_BYTES) {
    throw new Error(`dictation audio must contain 1..=${MAX_AUDIO_BYTES} bytes`);
  }
  const mediaType = requiredString(request?.mediaType, "mediaType");
  const extension = mediaExtension(mediaType);
  const timeoutMs = positiveNumber(request?.timeoutMs, 120_000);
  const movementPath = path.join(octopusHome, "ledger", "movements.jsonl");
  const afterOffset = await fileSize(movementPath);
  const cycle = crypto.randomBytes(12).toString("hex");
  await authorRun(octopusHome, cycle);
  await waitForInterfaceSocket(octopusHome, MICROPHONE_INTERFACE, timeoutMs);

  const payloadRoot = path.join(octopusHome, "interface-hand-in", CLIENT_INTERFACE, "audio");
  await fs.mkdir(payloadRoot, { recursive: true, mode: 0o700 });
  const contentHash = sha256Hex(bytes);
  const payloadPath = path.join(payloadRoot, `${contentHash}.${extension}`);
  await fs.writeFile(payloadPath, bytes, { flag: "wx", mode: 0o600 }).catch((error) => {
    if (error?.code !== "EEXIST") throw error;
  });
  const submittedAtMs = Date.now();
  const verdict = await options.handInArtifact({
    endpoint: interfaceEndpoint(octopusHome, MICROPHONE_INTERFACE),
    interfaceId: MICROPHONE_INTERFACE,
    manifest: {
      label: AUDIO_LABEL,
      payload_path: payloadPath,
    },
    timeoutMs,
  });
  const inputSeq = admittedSeq(verdict, AUDIO_LABEL);
  const result = await waitForTranscript({
    afterOffset,
    inputSeq,
    movementPath,
    octopusHome,
    timeoutMs,
  });
  return {
    runId: RUN_ID,
    text: result.text,
    model: result.model,
    sourceAudioArtifactId: result.sourceAudioArtifactId,
    latency: {
      clientToRuntimeMs: Math.max(0, result.inputRecordedAtMs - submittedAtMs),
      speechInterfaceMs: Math.max(0, result.outputRecordedAtMs - result.inputRecordedAtMs),
      totalMs: Math.max(0, result.outputRecordedAtMs - submittedAtMs),
    },
  };
}

async function authorRun(octopusHome, cycle) {
  const runRoot = path.join(octopusHome, "state", "runs");
  const runPath = path.join(runRoot, `${RUN_ID}.json`);
  const existing = await readJsonIfPresent(runPath);
  if (existing) {
    const record = requiredObject(existing, "existing dictation run");
    if (record.id !== RUN_ID || record.ee !== EE_ID || record.worker !== WORKER_ID) {
      throw new Error("existing dictation run identity drifted");
    }
    requiredObject(record.interface_state, "existing dictation run.interface_state");
  }
  const inputId = `cp_dictation_input_${cycle}`;
  const outputId = `cp_dictation_output_${cycle}`;
  const checkpoint = (id, input, output) => ({
    id,
    run: RUN_ID,
    trace: `trace:${RUN_ID}:${id}`,
    epoch: 0,
    interface_binding: authoredInterfaceBinding(input),
    outbound_interface_bindings: [authoredInterfaceBinding(output)],
    artifact_bindings: [],
    required_verified_artifacts: [],
  });
  const run = {
    id: RUN_ID,
    ee: EE_ID,
    worker: WORKER_ID,
    trace: `trace:${RUN_ID}`,
    status: "running",
    interface_state: existing?.interface_state ?? {},
    checkpoints: [
      checkpoint(inputId, MICROPHONE_INTERFACE, SPEECH_INTERFACE),
      checkpoint(outputId, SPEECH_INTERFACE, CLIENT_INTERFACE),
    ],
    active_checkpoints: [inputId, outputId],
  };
  await fs.mkdir(runRoot, { recursive: true, mode: 0o700 });
  const temporary = `${runPath}.${process.pid}.${crypto.randomBytes(6).toString("hex")}.tmp`;
  await fs.writeFile(temporary, `${JSON.stringify(run, null, 2)}\n`, { mode: 0o600 });
  await fs.rename(temporary, runPath);
}

function authoredInterfaceBinding(interfaceId) {
  return {
    interface: requiredString(interfaceId, "interface binding identity"),
    config: { artifact_policy: { mode: "all" } },
  };
}

async function waitForTranscript({ afterOffset, inputSeq, movementPath, octopusHome, timeoutMs }) {
  const deadline = Date.now() + timeoutMs;
  let offset = afterOffset;
  let inputRecordedAtMs = 0;
  while (Date.now() < deadline) {
    const tail = await readLedgerTail(movementPath, offset);
    offset = tail.offset;
    for (const movement of tail.movements) {
      if (movement.seq === inputSeq) inputRecordedAtMs = numberValue(movement.recorded_at_unix_ms);
      if (movement.kind !== "InterfaceOutbound") continue;
      if (endpointId(movement.from, "Interface") !== SPEECH_INTERFACE) continue;
      if (endpointId(movement.to, "Interface") !== CLIENT_INTERFACE) continue;
      const runtime = invocationRuntime(movement);
      if (runtime.run !== RUN_ID || runtime.interface !== CLIENT_INTERFACE) continue;
      const artifactId = requiredString(movement?.artifact?.id, "transcript artifact.id");
      if (!artifactId.startsWith(`${TRANSCRIPT_LABEL}:`) && !artifactId.startsWith(`${FAILURE_LABEL}:`)) continue;
      const payload = await readLedgerArtifact(octopusHome, movement.artifact);
      if (artifactId.startsWith(`${FAILURE_LABEL}:`)) {
        throw new Error(requiredString(payload?.error?.detail, "speech transcription error"));
      }
      return {
        text: requiredString(payload?.text, "speech transcript text"),
        model: stringValue(payload?.model),
        sourceAudioArtifactId: stringValue(payload?.source_audio_artifact_id),
        inputRecordedAtMs: inputRecordedAtMs || numberValue(movement.recorded_at_unix_ms),
        outputRecordedAtMs: numberValue(movement.recorded_at_unix_ms),
      };
    }
    await new Promise((resolve) => setTimeout(resolve, 120));
  }
  throw new Error(`speech transcript did not arrive within ${timeoutMs}ms`);
}

async function readLedgerTail(file, offset) {
  let handle;
  try {
    handle = await fs.open(file, "r");
  } catch (error) {
    if (error?.code === "ENOENT") return { offset, movements: [] };
    throw error;
  }
  try {
    const stat = await handle.stat();
    if (stat.size <= offset) return { offset, movements: [] };
    const bytes = Buffer.alloc(stat.size - offset);
    await handle.read(bytes, 0, bytes.length, offset);
    const newline = bytes.lastIndexOf(0x0a);
    if (newline < 0) return { offset, movements: [] };
    const complete = bytes.subarray(0, newline + 1);
    return {
      offset: offset + complete.length,
      movements: complete.toString("utf8").split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line)),
    };
  } finally {
    await handle.close();
  }
}

async function readLedgerArtifact(octopusHome, artifact) {
  const content = Array.isArray(artifact?.content) ? Buffer.from(artifact.content).toString("hex") : "";
  if (!/^[0-9a-f]{64}$/.test(content)) throw new Error("speech artifact content hash is malformed");
  const blobs = path.join(octopusHome, "ledger", "blobs");
  const manifest = JSON.parse(await fs.readFile(path.join(blobs, content), "utf8"));
  const payloadHash = stringValue(manifest?.payload_content_hash);
  const payload = payloadHash
    ? await fs.readFile(path.join(blobs, payloadHash))
    : await fs.readFile(requiredString(manifest?.payload_path, "speech artifact payload_path"));
  return JSON.parse(payload.toString("utf8"));
}

async function waitForInterfaceSocket(octopusHome, interfaceId, timeoutMs) {
  const socketPath = interfaceEndpoint(octopusHome, interfaceId).slice("unix://".length);
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await fs.access(socketPath);
      return;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 120));
    }
  }
  throw new Error(`interface '${interfaceId}' did not expose its hand-in socket within ${timeoutMs}ms`);
}

function interfaceEndpoint(octopusHome, interfaceId) {
  const hash = crypto.createHash("sha256").update(interfaceId).digest("hex").slice(0, 24);
  return `unix://${path.join(octopusHome, "app-server-control", `${hash}.sock`)}`;
}

async function readJsonIfPresent(file) {
  try {
    return JSON.parse(await fs.readFile(file, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

async function fileSize(file) {
  try {
    return (await fs.stat(file)).size;
  } catch (error) {
    if (error?.code === "ENOENT") return 0;
    throw error;
  }
}

function admittedSeq(verdict, label) {
  if (Number.isSafeInteger(verdict?.admitted?.seq)) return verdict.admitted.seq;
  if (verdict?.refused) throw new Error(`${label} refused: ${JSON.stringify(verdict.refused)}`);
  throw new Error(`${label} returned a malformed delivery verdict`);
}

function endpointId(endpoint, kind) {
  return stringValue(endpoint?.[kind] ?? endpoint?.[kind.toLowerCase()]);
}

function invocationRuntime(movement) {
  const metadata = requiredObject(movement?.invocation_metadata, "movement invocation_metadata");
  const runtime = requiredObject(metadata.runtime, "movement invocation_metadata.runtime");
  requiredString(runtime.run, "movement runtime.run");
  requiredString(runtime.ee, "movement runtime.ee");
  requiredString(runtime.worker, "movement runtime.worker");
  requiredString(runtime.checkpoint, "movement runtime.checkpoint");
  requiredString(runtime.interface, "movement runtime.interface");
  if (!Number.isSafeInteger(runtime.epoch) || runtime.epoch < 0) {
    throw new Error("movement runtime.epoch must be a non-negative integer");
  }
  return runtime;
}

function mediaExtension(mediaType) {
  const type = mediaType.split(";", 1)[0].trim().toLowerCase();
  if (type === "audio/webm") return "webm";
  if (type === "audio/wav" || type === "audio/x-wav") return "wav";
  if (type === "audio/mpeg") return "mp3";
  if (type === "audio/mp4") return "m4a";
  if (type === "audio/ogg") return "ogg";
  if (type === "audio/flac") return "flac";
  throw new Error(`unsupported dictation media type '${mediaType}'`);
}

function sha256Hex(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function absolutePath(value, field) {
  const output = requiredString(value, field);
  if (!path.isAbsolute(output)) throw new Error(`${field} must be absolute`);
  return path.normalize(output);
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function requiredString(value, field) {
  const output = stringValue(value);
  if (!output) throw new Error(`${field} is required`);
  return output;
}

function requiredObject(value, field) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${field} must be an object`);
  }
  return value;
}

function numberValue(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

function positiveNumber(value, fallback) {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : fallback;
}

module.exports = { transcribeOperatorDictation };
