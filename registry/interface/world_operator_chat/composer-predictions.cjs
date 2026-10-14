const fs = require("node:fs/promises");
const path = require("node:path");
const crypto = require("node:crypto");
const { isDeepStrictEqual: equal } = require("node:util");
const { setTimeout: delay } = require("node:timers/promises");

async function requestComposerPrediction(options, input) {
  const config = JSON.parse(await fs.readFile(path.join(__dirname, "composer-predictions.json"), "utf8"));
  if (!config.enabled) return null;
  const home = options.octopusHome();
  const runId = segment(input?.runId);
  return options.withRunLock(`prediction_${runId}`, async () => {
    const source = await options.readSource(runId);
    if (!source) return null;
    const { chat, terminal } = source;
    const registry = path.join(home, "registry");
    const provider = segment(chat.provider_bound_interface_id);
    const descriptor = await json(path.join(registry, "interface", provider, "interface.json"));
    if (descriptor.id !== provider || descriptor.interface_driver_ref !== config.provider_driver
      || descriptor.interface_driver_config?.private_config?.opencodex) return null;
    const suffix = crypto.createHash("sha256").update(JSON.stringify([runId, provider])).digest("hex").slice(0, 24);
    const service = `${config.client_template}_input_${suffix}`;
    const resultInterface = `${config.client_template}_result_${suffix}`;
    const worker = `${config.worker_template}_${suffix}`;
    const maintenanceRun = `run_client_maintenance_${suffix}`;
    const ee = `ee_client_maintenance_${segment(chat.os_root_ee_id)}`;
    const runPath = path.join(home, "state", "runs", `${maintenanceRun}.json`);
    const previousBytes = await readOptional(runPath);
    const previous = previousBytes ? JSON.parse(previousBytes) : null;
    const prior = previous?.interface_state?.[service]?.composer_prediction;
    const request = { request_id: `prediction-${terminal.movement_seq}`, reply_to: resultInterface,
      source: { id: terminal.artifact_id, content: [...Buffer.from(terminal.content_hash, "hex")] },
      source_movement_seq: terminal.movement_seq };
    if (!/^[0-9a-f]{64}$/.test(terminal.content_hash) || !Number.isSafeInteger(request.source_movement_seq)
      || request.source_movement_seq < 0) throw new Error("Prediction source is malformed");
    const readResult = async (expected) => {
      const rows = await options.readArtifacts({ run_id: maintenanceRun, interface_id: resultInterface, labels: [config.result_label], after_seq: expected.source_movement_seq });
      for (const row of rows) {
        if (row.error) throw new Error(row.error);
        if (row.movement_kind !== "InterfaceOutbound" || row.payload?.request?.request_id !== expected.request_id) continue;
        if (!equal(row.payload.request, expected)) throw new Error("Prediction result source changed");
        const result = row.payload.result;
        if (!["completed", "unavailable", "failed"].includes(result?.status)
          || !(result.suggestion === null || typeof result.suggestion === "string")) throw new Error("Prediction result is malformed");
        return result;
      }
      return undefined;
    };
    const recordFor = (selected, epoch) => {
      const binding = (id, label) => ({ interface: id, config: { artifact_policy: { mode: "allow", names: [label], namespaces: [] } } });
      const checkpoint = (name, from, to, label, artifacts = []) => ({
        id: `cp_${name}`, run: maintenanceRun, trace: `trace:${maintenanceRun}:${name}:${epoch}`, epoch,
        interface_binding: binding(from, label), outbound_interface_bindings: to ? [binding(to, label)] : [],
        artifact_bindings: artifacts, required_verified_artifacts: [],
      });
      const checkpoints = [checkpoint("input", service, provider, config.request_label, [{ id: selected.source.id }]),
        checkpoint("result", resultInterface, null, config.result_label)];
      return { id: maintenanceRun, ee, worker, trace: `trace:${maintenanceRun}`, status: "running",
        interface_state: { [service]: { composer_prediction: { source_run: runId, request: selected, epoch } } },
        checkpoints, active_checkpoints: checkpoints.map((row) => row.id) };
    };
    const present = (result) => {
      if (result.status === "failed") throw new Error(result.error || "Prediction failed");
      return { id: request.request_id, text: result.suggestion, runId: maintenanceRun };
    };
    if (previous && (!prior || !Number.isSafeInteger(prior.epoch) || prior.epoch < 0
      || !equal(previous, recordFor(prior.request, prior.epoch)))) {
      throw new Error("Client maintenance run has another owner");
    }
    if (prior && !equal(prior.request, request) && !await readResult(prior.request)) return null;
    if (prior && equal(prior.request, request)) {
      const result = await readResult(request);
      if (result) return present(result);
    } else {
      await options.withTopologyLock(home, async () => {
      const writes = [];
      const stage = async (file, next) => {
        const existing = await readOptional(file);
        if (existing !== null) {
          if (!equal(JSON.parse(existing), next)) throw new Error(`Client maintenance configuration changed: ${path.basename(file)}`);
        } else writes.push({ file, next, beforeDigest: null });
      };
      await stage(path.join(home, "state", "ee_instances", `${ee}.json`), {
        ee_id: ee, template_name: config.ee_template, parent_ee_id: chat.os_root_ee_id,
        goal: "Maintain the operator's client", config: {},
      });
      for (const id of [service, resultInterface]) for (const filename of ["interface.json", "activation.json", "life.json"]) {
        const value = await json(path.join(registry, "interface", config.client_template, filename));
        if (filename === "interface.json") value.id = id;
        if (filename === "activation.json") value.interface = id;
        if (filename === "life.json") for (const process of value.processes) {
          if (process.bin.startsWith("owner://")) process.bin = `interface://${config.client_template}/${process.bin.slice("owner://".length)}`;
        }
        await stage(path.join(registry, "interface", id, filename), value);
      }
      const pin = await json(path.join(registry, "interface", config.worker_template, "interface.json"));
      pin.id = worker;
      const policy = await json(path.join(registry, "interface", config.worker_template, "policy.json"));
      policy.id = worker;
      policy.ceiling.interfaces = policy.ceiling.interfaces.flatMap((id) => id === config.worker_template ? [worker]
        : id === config.client_template ? [service, resultInterface] : id === config.provider_template ? [provider] : [id]);
      await stage(path.join(registry, "interface", worker, "interface.json"), pin);
      await stage(path.join(registry, "interface", worker, "policy.json"), policy);
      const epoch = (prior?.epoch ?? -1) + 1;
      const record = recordFor(request, epoch);
      writes.push({ file: runPath, next: record, beforeDigest: previousBytes === null ? null : crypto.createHash("sha256").update(previousBytes).digest("hex") });
      await options.commit({ label: "Prepare client suggestion", writes, removals: [], publishBarrierPath: runPath });
      });
    }
    const payloadRoot = path.join(home, "interface-hand-in", service, "client", "predictions");
    await fs.mkdir(payloadRoot, { recursive: true });
    const payloadPath = path.join(payloadRoot, `${request.request_id}.json`);
    const payload = JSON.stringify(request);
    const existingPayload = await readOptional(payloadPath);
    if (existingPayload !== null && existingPayload !== payload) throw new Error("Prediction request changed after preparation");
    if (existingPayload === null) await fs.writeFile(payloadPath, payload, { flag: "wx", mode: 0o600 });
    await options.waitForInterface(home, service, 20_000);
    const verdict = await options.handIn({ interfaceId: service, manifest: { label: config.request_label, payload_path: payloadPath }, timeoutMs: 20_000 });
    if (!Number.isSafeInteger(verdict?.admitted?.seq)) throw new Error("Client suggestion request was refused");
    const deadline = Date.now() + 90_000;
    while (Date.now() < deadline) {
      options.assertCurrent();
      const result = await readResult(request);
      if (result) return present(result);
      await delay(200);
    }
    throw new Error("Client suggestion did not finish in time");
  });
}

function segment(value) {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(value)) throw new Error("Invalid client maintenance identity");
  return value;
}
async function readOptional(file) { try { return await fs.readFile(file, "utf8"); } catch (e) { if (e.code === "ENOENT") return null; throw e; } }
async function json(file) { return JSON.parse(await fs.readFile(file, "utf8")); }
module.exports = { requestComposerPrediction };
