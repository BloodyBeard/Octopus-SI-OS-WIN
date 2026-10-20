module.exports = { scheduledInterfacesProjection };

const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");
const { parseScheduledDueAt } = require("../scheduledTime.cjs");
const { activationDriverRef, cronScheduleFingerprint, parseCronActivationDocument, scheduledInterfaceDisplayName } = require("./cronActivation.cjs");
const { readJsonFile, readOptionalRegularJsonDocument, stableJson } = require("./jsonFiles.cjs");
const { movementBoundaryInterface, movementRunId, readChangeJournal } = require("./movementRows.cjs");
const { baseInterfaceRows } = require("./registryRows.cjs");
const { runInterfaceReferences } = require("./taskDeletion.cjs");
const { arrayValue, interfaceBindingId, isAdmittedInterfaceArtifactMovement, ledgerArtifactLabel, numberValue, objectOrEmpty, rowTimeMs, stringValue } = require("./values.cjs");

function scheduledInterfacesProjection(store) {
  function scheduledScopeOptions(projection) {
    const scope = store.operatorOsScope(projection);
    const names = new Map(arrayValue(scope.project_presentations).map((row) => [stringValue(row?.ee_id), stringValue(row?.name)]));
    return arrayValue(scope.projects).map((row) => ({
      ee_id: stringValue(row.ee_id),
      name: names.get(stringValue(row.ee_id)) || stringValue(row.goal) || stringValue(row.ee_id),
    })).filter((row) => row.ee_id);
  }

  async function scheduledInterfaceRows(projection) {
    const rows = [];
    const boundInterfaces = new Set(arrayValue(projection.runRecords).flatMap((record) => (
      [...runInterfaceReferences(record)]
    )).filter(Boolean));
    const descriptors = baseInterfaceRows(projection).filter((descriptor) => (
      arrayValue(arrayValue(descriptor.activation_sidecars)[0]?.activations)
        .some((activation) => activationDriverRef(activation) === "cron_life")
    ));
    const indexedEvidence = descriptors.length > 0
      ? await store.indexedArtifactLabelRows(projection, "cron.current_time", { namespace: true })
      : [];
    if (indexedEvidence === null) await store.refreshLedgerProjection(projection);
    const evidenceRows = (indexedEvidence ?? projection.ledgerRows).filter((movement) => (
      store.artifactLabelMatches(ledgerArtifactLabel(movement), "cron.current_time", true)
      && isAdmittedInterfaceArtifactMovement(movement)
    ));
    for (const descriptor of descriptors) {
      const interfaceId = stringValue(descriptor.rule_id ?? descriptor.id);
      const sidecar = arrayValue(descriptor.activation_sidecars)[0];
      const activationPath = stringValue(sidecar?.descriptor_path);
      let activationDigest = "";
      try {
        activationDigest = crypto.createHash("sha256").update(await fs.readFile(activationPath)).digest("hex");
        const parsed = parseCronActivationDocument({
          schema_version: sidecar.schema_version,
          interface: sidecar.interface,
          activations: sidecar.activations,
        }, interfaceId);
        const schedule = parsed.config.schedule;
        const runTemplate = parsed.config.run_template;
        const trigger = runTemplate.checkpoints.find((checkpoint) => interfaceBindingId(checkpoint.interface_binding) === interfaceId);
        const cloneEligible = parsed.config.target.mode === "new_run"
          || arrayValue(trigger?.outbound_interface_bindings).some((value) => interfaceBindingId(value));
        const template = !boundInterfaces.has(interfaceId);
        if (template) {
          if (parsed.activation.enabled !== false) throw new Error(`scheduled interface '${interfaceId}' is not bound to a run`);
          if (!cloneEligible) continue;
        }
        const evidence = template
          ? { lastRunAtMs: null, lastMovementSeq: null, nextRunAtMs: null, nextRunSource: "", inactivityState: null, deliveries: [] }
          : await scheduledInterfaceEvidence(projection, interfaceId, parsed.config, evidenceRows);
        rows.push({
          interface_id: interfaceId,
          display_name: scheduledInterfaceDisplayName(interfaceId, runTemplate.id),
          template,
          status: template ? "template" : parsed.activation.enabled === false ? "disabled" : "enabled",
          valid: true,
          activation_digest: activationDigest,
          activation_path: activationPath,
          descriptor_path: stringValue(descriptor.descriptor_path),
          schedule: schedule.inactivity != null
            ? { type: "inactivity", ...schedule.inactivity }
            : schedule.due_at != null
            ? { type: "once", due_at: schedule.due_at }
            : schedule.cron == null
              ? { type: "interval", interval_seconds: schedule.interval_seconds }
              : { type: "cron", cron: schedule.cron },
          timezone: schedule.timezone,
          fire_on_start: schedule.fire_on_start,
          instructions: schedule.reminder_default,
          reminder_env: schedule.reminder_env,
          target: parsed.config.target,
          run_template: {
            run_id: runTemplate.id,
            ee_id: runTemplate.ee,
            worker_interface_id: runTemplate.worker,
            checkpoint_count: runTemplate.checkpoints.length,
            outbound_interface_ids: arrayValue(trigger?.outbound_interface_bindings).map(interfaceBindingId).filter(Boolean),
            artifact_binding_count: arrayValue(trigger?.artifact_bindings).length,
          },
          clone_eligible: cloneEligible,
          last_run_at_ms: evidence.lastRunAtMs,
          last_movement_seq: evidence.lastMovementSeq,
          next_run_at_ms: parsed.activation.enabled === false ? null : evidence.nextRunAtMs,
          next_run_source: parsed.activation.enabled === false ? "" : evidence.nextRunSource,
          inactivity_state: evidence.inactivityState,
          deliveries: evidence.deliveries,
        });
      } catch (error) {
        rows.push({
          interface_id: interfaceId,
          display_name: scheduledInterfaceDisplayName(interfaceId, ""),
          status: "invalid",
          valid: false,
          activation_digest: activationDigest,
          activation_path: activationPath,
          descriptor_path: stringValue(descriptor.descriptor_path),
          error: String(error?.message ?? error),
        });
      }
    }
    return rows.sort((left, right) => (
      numberValue(left.next_run_at_ms, Number.MAX_SAFE_INTEGER) - numberValue(right.next_run_at_ms, Number.MAX_SAFE_INTEGER)
      || stringValue(left.display_name).localeCompare(stringValue(right.display_name))
    ));
  }

  async function scheduledInterfaceEvidence(projection, interfaceId, config, evidenceRows) {
    let lastRunAtMs = 0;
    let lastMovementSeq = 0;
    const deliveries = [];
    const deadline = config.schedule.due_at == null ? null : parseScheduledDueAt(config.schedule.due_at);
    for (const movement of evidenceRows) {
      if (
        movementBoundaryInterface(movement, "from") !== interfaceId
      ) continue;
      if (deadline !== null) {
        const payload = objectOrEmpty(await store.resolveLedgerArtifactPayload(movement));
        if (payload.schedule_kind !== "one_shot" || typeof payload.due_at !== "string" || parseScheduledDueAt(payload.due_at) !== deadline) continue;
      }
      const at = rowTimeMs(movement);
      const seq = numberValue(movement.seq, 0);
      if (at > lastRunAtMs || at === lastRunAtMs && seq > lastMovementSeq) {
        lastRunAtMs = at;
        lastMovementSeq = seq;
      }
      deliveries.push({ seq, at_ms: at, run_id: movementRunId(movement) });
    }
    const schedule = config.schedule;
    let nextRunAtMs = null;
    let nextRunSource = "";
    let inactivityState = null;
    if (schedule.inactivity != null) {
      const document = await readOptionalRegularJsonDocument(path.join(projection.octopusHome, "interfaces", interfaceId, "cron-inactivity-state.json"), "inactivity trigger state");
      const state = document?.document;
      if (state && (state.schema_version !== 1 || !["idle", "waiting", "prepared", "admitted"].includes(state.status))) throw new Error("inactivity trigger state has an invalid schema or status");
      if (state && ["waiting", "prepared"].includes(state.status) && (!Number.isSafeInteger(state.next_due_unix_ms) || state.next_due_unix_ms < 0)) throw new Error("inactivity trigger state has an invalid deadline");
      const current = state?.schema_version === 1 && stableJson(state.inactivity ?? null) === stableJson(schedule.inactivity);
      if (current && ["waiting", "prepared"].includes(state.status)
        && Number.isSafeInteger(state.next_due_unix_ms) && state.next_due_unix_ms >= 0) {
        nextRunAtMs = state.next_due_unix_ms;
        nextRunSource = "driver_state";
      }
      inactivityState = {
        status: stringValue(state?.status) || "unstarted",
        current: Boolean(current) || ["idle", "admitted"].includes(stringValue(state?.status)),
        last_activity_at_ms: Number.isSafeInteger(state?.activity?.recorded_at_unix_ms) ? state.activity.recorded_at_unix_ms : null,
        last_activity_run_id: stringValue(state?.activity?.run),
      };
    }
    if (deadline !== null && !lastRunAtMs) {
      nextRunAtMs = Number(deadline / 1_000_000n);
      nextRunSource = "schedule";
    }
    if (schedule.interval_seconds != null) {
      const fingerprint = cronScheduleFingerprint(interfaceId, config);
      const statePath = path.join(projection.octopusHome, "interfaces", interfaceId, "cron-interval-state.json");
      const state = await readJsonFile(statePath);
      if (
        state
        && state.schema_version === 1
        && state.schedule_fingerprint === fingerprint
        && ["waiting", "prepared"].includes(state.status)
        && typeof state.next_due_unix_ns === "number"
        && Number.isFinite(state.next_due_unix_ns)
        && Number.isInteger(state.next_due_unix_ns)
        && state.next_due_unix_ns >= 0
      ) {
        nextRunAtMs = Math.floor(state.next_due_unix_ns / 1_000_000);
        nextRunSource = "driver_state";
      }
    }
    if (!lastRunAtMs && config.schedule.cron != null) {
      const state = await readJsonFile(path.join(projection.octopusHome, "interfaces", interfaceId, "cron-state.json"));
      const parsed = typeof state?.last_cron_minute === "string" ? Date.parse(state.last_cron_minute) : NaN;
      if (Number.isFinite(parsed)) lastRunAtMs = parsed;
    }
    const clientRuns = new Map(arrayValue(store.operatorOsScope(projection).client_runs).map((row) => [stringValue(row.run_id), row]));
    const recent = deliveries.sort((left, right) => right.seq - left.seq).slice(0, 8);
    for (const delivery of recent) {
      delivery.chat_interface_id = stringValue(clientRuns.get(delivery.run_id)?.interface_id);
      delivery.journal = delivery.run_id ? await readChangeJournal(projection, delivery.run_id) : null;
    }
    return {
      lastRunAtMs: lastRunAtMs || null,
      lastMovementSeq: lastMovementSeq || null,
      nextRunAtMs,
      nextRunSource,
      inactivityState,
      deliveries: recent,
    };
  }

  return { scheduledInterfaceEvidence, scheduledInterfaceRows, scheduledScopeOptions };
}
