import { useEffect, useRef, useState } from "react";
import {
  emptyProjection,
  emptySnapshot,
  type Accent,
  type CheckpointRow,
  type DetailRow,
  type EnvironmentNode,
  type EnvironmentStatus,
  type EventLogRow,
  type InterfaceArtifactTransferProjection,
  type InterfaceSummary,
  type ProjectionStatus,
  type RunProjection,
  type StatRow,
  type TopologyItem,
  type TopologySnapshot,
  type WorkerEnvironmentProjection,
} from "./data/topology";
import { hasProjectionBridge, projectionTargetLabel, readProjection } from "./projectionClient";
import { currentProjectionStructureGeneration, useProjectionGeneration } from "./projectionLive";
import type { OctopusRuntimeHostState, OctopusRuntimeTopologyProjection } from "./electron";

type RuntimePayload = {
  generation: number | null;
  snapshotRaw: any;
  eeRaw: any;
  runsRaw: any;
  runCheckpointsRaw: any;
  interfacesRaw: any;
  bindingsRaw: any;
  artifactsRaw: any;
  marksRaw: any;
  workerEnvironmentsRaw: any;
  movementsRaw: any;
  runtimeObservation: ClientRuntimeObservation;
  runDetailsRaw: Record<string, { detail?: any; trace?: any; error?: string }>;
};

type ClientRuntimeObservation = {
  authoritative: false;
  host_error: string;
  observation_source: "client.runtime_status";
  ownership: string;
  phase: string;
  pid: number | null;
  socket_connected: boolean;
};

// Accent rotation only — the stage lays zones out itself (CSS grid + measured links).
// There is no fixed number of EEs and no pixel map: every projected zone renders.
const ACCENTS: Accent[] = ["green", "blue", "purple", "amber"];
const TIME_FORMATTER = new Intl.DateTimeFormat([], { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
const DATE_FORMATTER = new Intl.DateTimeFormat([], { month: "short", day: "2-digit", hour: "2-digit", minute: "2-digit" });

export function useRuntimeTopology(
  enabled = true,
  focusedRunIds: readonly string[] = [],
  includeLedger = true,
): TopologySnapshot {
  const [snapshot, setSnapshot] = useState<TopologySnapshot>(() => emptySnapshot());
  // Every projection change on disk re-projects the world in place: no flash back to a
  // loading state, no stale "live" claim. The first read is the only "connecting" moment.
  const generation = useProjectionGeneration(includeLedger, enabled);
  const loadedRef = useRef(false);
  const payloadRef = useRef<RuntimePayload | null>(null);
  const runtimeObservationRef = useRef<ClientRuntimeObservation | null>(null);
  const readerRef = useRef<TopologyReader | null>(null);
  const structureGenerationRef = useRef(0);
  const generationRef = useRef(generation);
  generationRef.current = generation;
  const requestedGenerationRef = useRef<number | null>(null);
  const focusedRunKey = focusedRunIds.filter(Boolean).join("\u0000");

  useEffect(() => {
    if (!enabled) return;
    return window.octopusTransport?.onRuntimeHostState?.((state) => {
      const observation = clientRuntimeHostObservation(state);
      runtimeObservationRef.current = observation;
      const current = payloadRef.current;
      if (!current) return;
      const next = { ...current, runtimeObservation: observation };
      payloadRef.current = next;
      setSnapshot((previous) => ({ ...previous, projection: liveProjectionStatus(next) }));
    });
  }, [enabled]);

  useEffect(() => {
    if (!enabled) {
      // Keep the last projected world so returning to the surface is instant; only the
      // status tells the truth that reads are paused while the surface is closed.
      setSnapshot((prev) => (loadedRef.current
        ? {
          ...prev,
          projection: {
            mode: "offline",
            endpoint: projectionTargetLabel(),
            label: "Projection idle",
            detail: "Reads resume when a topology surface is open",
          },
        }
        : prev));
      readerRef.current = null;
      return;
    }
    if (!hasProjectionBridge()) {
      setSnapshot(emptySnapshot({
        ...emptyProjection,
        endpoint: projectionTargetLabel(),
        label: "Projection bridge unavailable",
        detail: "Open the desktop app so the renderer can read registry and ledger projection files",
      }));
      readerRef.current = null;
      return;
    }

    if (!loadedRef.current) {
      setSnapshot((prev) => ({
        ...prev,
        projection: {
          mode: "connecting",
          endpoint: projectionTargetLabel(),
          label: "Reading projection",
          detail: "Reading registry descriptors, binding manifests, artifacts, marks, and ledger rows",
        },
      }));
    }

    const focusedRuns = new Set(focusedRunKey.split("\u0000").filter(Boolean));
    let appliedGeneration: number | null = null;
    const reader = createTopologyReader(async (isCurrent) => {
      try {
        const payload = await refreshProjection(focusedRuns, includeLedger, runtimeObservationRef.current);
        if (!isCurrent()) return;
        const next = runtimeObservationRef.current
          ? { ...payload, runtimeObservation: runtimeObservationRef.current }
          : payload;
        const unchanged = payload.generation !== null && payload.generation === appliedGeneration && loadedRef.current;
        appliedGeneration = payload.generation;
        loadedRef.current = true;
        payloadRef.current = next;
        if (!unchanged) setSnapshot(projectRuntime(next));
      } catch (error) {
        if (!isCurrent()) return;
        const failure: ProjectionStatus = {
          mode: "error",
          endpoint: projectionTargetLabel(),
          label: "Projection read failed",
          detail: String((error as Error | undefined)?.message ?? error),
        };
        // A failed re-read keeps the last projected world visible and says so; only a
        // failure with nothing loaded yet leaves the world empty.
        appliedGeneration = null;
        setSnapshot((prev) => (loadedRef.current ? { ...prev, projection: failure } : emptySnapshot(failure)));
      }
    });
    readerRef.current = reader;
    requestedGenerationRef.current = generationRef.current;
    structureGenerationRef.current = currentProjectionStructureGeneration();
    reader.request(true);
    return () => {
      if (readerRef.current === reader) readerRef.current = null;
      reader.dispose();
    };
  }, [enabled, focusedRunKey, includeLedger]);

  useEffect(() => {
    const reader = readerRef.current;
    if (!reader || requestedGenerationRef.current === generation) return;
    requestedGenerationRef.current = generation;
    const structureGeneration = currentProjectionStructureGeneration();
    const structural = structureGeneration !== structureGenerationRef.current;
    structureGenerationRef.current = structureGeneration;
    reader.request(structural);
  }, [generation]);

  return snapshot;
}

type TopologyReader = {
  dispose: () => void;
  request: (urgent: boolean) => void;
};

const LEDGER_REFRESH_COST_FACTOR = 3;
const LEDGER_REFRESH_IDLE_TIMEOUT_MS = 1_000;

function createTopologyReader(read: (isCurrent: () => boolean) => Promise<void>): TopologyReader {
  let disposed = false;
  let reading = false;
  let dirty = false;
  let urgent = false;
  let lastCostMs = 0;
  let lastFinishedAtMs = Number.NEGATIVE_INFINITY;
  let timer = 0;
  let idle = 0;
  const cancelScheduled = () => {
    if (timer) window.clearTimeout(timer);
    if (idle) window.cancelIdleCallback?.(idle);
    timer = 0;
    idle = 0;
  };
  const run = async () => {
    cancelScheduled();
    if (disposed || reading || !dirty) return;
    dirty = false;
    urgent = false;
    reading = true;
    const startedAtMs = performance.now();
    try {
      await read(() => !disposed);
    } finally {
      reading = false;
      lastFinishedAtMs = performance.now();
      lastCostMs = lastFinishedAtMs - startedAtMs;
      schedule();
    }
  };
  const schedule = () => {
    if (disposed || reading || !dirty) return;
    if (urgent) {
      cancelScheduled();
      void run();
      return;
    }
    if (timer || idle) return;
    const waitMs = Math.max(0, lastFinishedAtMs + lastCostMs * LEDGER_REFRESH_COST_FACTOR - performance.now());
    timer = window.setTimeout(() => {
      timer = 0;
      if (typeof window.requestIdleCallback === "function") {
        idle = window.requestIdleCallback(() => {
          idle = 0;
          void run();
        }, { timeout: LEDGER_REFRESH_IDLE_TIMEOUT_MS });
      } else {
        void run();
      }
    }, waitMs);
  };
  return {
    dispose: () => {
      disposed = true;
      cancelScheduled();
    },
    request: (isUrgent) => {
      dirty = true;
      urgent ||= isUrgent;
      schedule();
    },
  };
}

async function refreshProjection(
  focusedRunIds: Set<string>,
  includeLedger: boolean,
  runtimeObservation: ClientRuntimeObservation | null,
): Promise<RuntimePayload> {
  const [projection, runtimeStatusRaw] = await Promise.all([
    readProjection<OctopusRuntimeTopologyProjection>("projection.runtime.topology", {
      include_ledger: includeLedger,
      focused_run_ids: [...focusedRunIds],
    }),
    runtimeObservation
      ? Promise.resolve(null)
      : window.octopusTransport?.runtimeStatus?.().catch(() => null) ?? Promise.resolve(null),
  ]);

  return {
    generation: Number.isSafeInteger(projection.generation) ? projection.generation as number : null,
    snapshotRaw: projection.snapshotRaw,
    eeRaw: projection.eeRaw,
    runsRaw: projection.runsRaw,
    runCheckpointsRaw: projection.runCheckpointsRaw,
    interfacesRaw: projection.interfacesRaw,
    bindingsRaw: projection.bindingsRaw,
    artifactsRaw: projection.artifactsRaw,
    marksRaw: projection.marksRaw,
    workerEnvironmentsRaw: projection.workerEnvironmentsRaw,
    movementsRaw: projection.movementsRaw,
    runtimeObservation: runtimeObservation ?? clientRuntimeObservation(runtimeStatusRaw),
    runDetailsRaw: projection.runDetailsRaw,
  };
}

// Truthful live status: when it was projected, and whether any file failed to parse on the
// way (torn writes surface here instead of blanking the world).
function liveProjectionStatus(payload: RuntimePayload): ProjectionStatus {
  const warnings = array(payload.snapshotRaw?.projection?.warnings).map((item) => stringValue(item)).filter(Boolean);
  const projection = record(payload.snapshotRaw?.projection) ?? {};
  const isLab = stringValue(projection.mode) === "lab";
  const observation = payload.runtimeObservation;
  const runtimeUp = observation.socket_connected;
  const runtimePhase = observation.phase;
  const runtimeError = observation.host_error;
  const endpoint = stringValue(projection.octopus_home) || projectionTargetLabel();
  const latestMovementAt = formatTime(Number(projection.latest_movement_recorded_at_unix_ms) || 0) || "none";
  const source = stringValue(projection.octopus_home) || stringValue(projection.durable_root) || "unknown source";
  const ledgerRows = Number.isFinite(Number(projection.ledger_rows)) ? Number(projection.ledger_rows) : 0;
  if (isLab) {
    return {
      mode: "offline",
      endpoint,
      label: "Client Lab projection",
      detail: `${ledgerRows} fixture movements · isolated from live Octopus`,
    };
  }
  if (!runtimeUp && runtimePhase === "degraded") {
    return {
      mode: "error",
      endpoint,
      label: "Octopus could not start",
      detail: runtimeError || "Runtime host entered a degraded state",
    };
  }
  if (!runtimeUp && runtimePhase && !["idle", "stopped", "unmanaged"].includes(runtimePhase)) {
    const phaseLabels: Record<string, string> = {
      preparing_home: "Preparing Octopus Home",
      starting_runtime: "Starting Octopus",
      waiting_runtime: "Starting Octopus",
      registry_install: "Installing capabilities",
      registry_ready: "Registry ready",
      state_ready: "Runtime state ready",
      waiting_interfaces: "Starting interfaces",
    };
    return {
      mode: "connecting",
      endpoint,
      label: phaseLabels[runtimePhase] || "Starting Octopus",
      detail: [
        `Home ${source}`,
        observation.ownership,
        observation.pid === null ? "" : `pid ${observation.pid}`,
        runtimeError,
      ].filter(Boolean).join(" · "),
    };
  }
  return {
    mode: runtimeUp ? "live" : "offline",
    endpoint,
    label: runtimeUp
      ? warnings.length ? `Client observes runtime socket · ${warnings.length} file warning${warnings.length === 1 ? "" : "s"}` : "Client observes runtime socket"
      : "Recorded projection · client socket unavailable",
    detail: [
      `Home ${source}`,
      `${ledgerRows} movements`,
      `last ${latestMovementAt}`,
      runtimeUp ? "non-authoritative client socket observation" : runtimeError || "no client socket observation",
      ...warnings,
    ].join(" · "),
  };
}

function clientRuntimeObservation(value: unknown): ClientRuntimeObservation {
  const row = record(value) ?? {};
  return {
    authoritative: false,
    host_error: stringValue(row.hostError ?? row.error),
    observation_source: "client.runtime_status",
    ownership: stringValue(row.ownership),
    phase: stringValue(row.phase),
    pid: Number.isSafeInteger(row.pid) ? row.pid : null,
    socket_connected: row.up === true,
  };
}

function clientRuntimeHostObservation(value: OctopusRuntimeHostState): ClientRuntimeObservation {
  return {
    authoritative: false,
    host_error: stringValue(value.error),
    observation_source: "client.runtime_status",
    ownership: stringValue(value.ownership),
    phase: stringValue(value.phase),
    pid: Number.isSafeInteger(value.pid) ? value.pid : null,
    socket_connected: value.ready === true,
  };
}

function projectRuntime(payload: RuntimePayload): TopologySnapshot {
  const allEes = array(payload.eeRaw?.ee_instances)
    .filter((ee) => stringValue(ee?.ee_id));

  if (allEes.length === 0) {
    const status = liveProjectionStatus(payload);
    return emptySnapshot({
      ...status,
      detail: `No EE rows were present in registry/ledger projection files · ${status.detail}`,
    });
  }

  const runs = array(payload.runsRaw?.runs);
  const runCheckpointsByRun = new Map<string, any[]>(array(payload.runCheckpointsRaw?.run_checkpoints).flatMap((row) => {
    const runId = stringValue(row?.run_id);
    return runId ? [[runId, array(row?.checkpoints)] as [string, any[]]] : [];
  }));
  const interfaces = array(payload.interfacesRaw?.interfaces);
  const bindings = array(payload.bindingsRaw?.bindings);
  const artifacts = array(payload.artifactsRaw?.artifacts);
  const marks = array(payload.marksRaw?.marks);
  const workerEnvironments = array(payload.workerEnvironmentsRaw?.worker_environments);
  const movements = array(payload.movementsRaw?.movements);
  const bindingEntries: Array<[string, any]> = bindings.flatMap((binding) => {
    const runId = stringValue(binding?.run_id);
    return runId ? [[runId, binding] as [string, any]] : [];
  });
  const bindingByRun = new Map<string, any>(bindingEntries);
  const orderedEes = orderEes(allEes);
  const runsByEe = groupRowsBy(runs, (run) => runEeId(run, bindingByRun));
  const eeIdsByRun = new Map<string, Set<string>>();
  for (const run of runs) {
    const runId = stringValue(run?.run_id);
    if (!runId) continue;
    const eeIds = eeIdsByRun.get(runId) ?? new Set<string>();
    eeIds.add(runEeId(run, bindingByRun));
    eeIdsByRun.set(runId, eeIds);
  }
  const interfacePositions = interfaces.map((_, index) => index);
  const projectionIndex = {
    runsByEe,
    childCounts: groupRowsBy(allEes, (ee) => stringValue(ee?.parent_ee_id ?? ee?.parent_id ?? ee?.parent?.ee_id)),
    bindings: indexScopedRows(bindings, (row) => stringValue(row?.ee_id), (row) => stringValue(row?.run_id), eeIdsByRun),
    artifacts: indexScopedRows(artifacts, artifactEeId, artifactRunId, eeIdsByRun),
    marks: indexScopedRows(marks, artifactEeId, artifactRunId, eeIdsByRun),
    movements: indexScopedRows(movements, (row) => stringValue(row?.raw_ee_id), (row) => stringValue(row?.raw_run_id), eeIdsByRun),
    workerEnvironmentsByEe: groupRowsBy(workerEnvironments, workerEnvironmentEeId),
    workerEnvironmentsByRun: groupRowsBy(workerEnvironments, (row) => stringValue(row?.run_id ?? row?.run ?? row?.meta?.run_id)),
    workerEnvironmentsById: firstRowsBy(workerEnvironments, (row) => stringValue(row?.worker_environment_id)),
    interfaces,
    interfacesByRuleId: firstRowsBy(interfaces, (iface) => stringValue(iface?.rule_id)),
    interfacePositionsById: groupRowsBy(interfacePositions, (position) => stringValue(interfaces[position]?.rule_id ?? interfaces[position]?.template_name)),
    interfacePositionsByTemplate: groupRowsBy(interfacePositions, (position) => stringValue(interfaces[position]?.ee_template_name)),
    workerInterfaceIds: new Set(interfaces.filter((iface) => (
      stringValue(iface?.interface_type).toLowerCase() === "system"
      && stringValue(iface?.role).toLowerCase() === "worker"
    )).map((iface) => stringValue(iface?.rule_id ?? iface?.id)).filter(Boolean)),
  };
  const environments = orderedEes.map((ee, index) => projectEnvironment({
    ee,
    index,
    projectionIndex,
    runDetailsRaw: payload.runDetailsRaw,
    runCheckpointsByRun,
  }));
  const workerRefs = new Set(runs.map((run) => stringValue(run?.worker)).filter(Boolean)).size;
  const interfaceIndex: Record<string, InterfaceSummary> = {};
  for (const iface of interfaces) {
    const id = stringValue(iface?.rule_id ?? iface?.id);
    if (!id) continue;
    const artifactTransfer = interfaceArtifactTransfer(iface);
    interfaceIndex[id] = {
      id,
      type: stringValue(iface?.interface_type).toLowerCase() || "system",
      source: stringValue(iface?.source) || "registry",
      status: stringValue(iface?.mode) || stringValue(iface?.status),
      boundIn: [],
      artifactTransfer,
      descriptor: record(iface) ?? {},
    };
  }
  for (const environment of environments) {
    for (const row of environment.interfaces) {
      const summary = interfaceIndex[row.name];
      if (summary && !summary.boundIn.includes(environment.name)) {
        summary.boundIn.push(environment.name);
      }
    }
  }
  const recentEvents = eventRowsFromMovements(movements, 7);
  const lastArtifact = latestByTime(artifacts);
  const inFlight = artifacts.filter((artifact) => ["open", "pending", "in_flight"].includes(stringValue(artifact?.status).toLowerCase())).length;

  return {
    environments,
    summaryStats: [
      `${allEes.length} EEs`,
      `${runs.length} runs`,
      `${workerRefs || 0} worker refs`,
    ],
    artifactStores: [
      { label: "Last artifact", value: formatTime(timeOf(lastArtifact)) || "none", glyph: "artifact" },
      { label: "In flight", value: String(inFlight), glyph: "binding" },
      { label: "Stored artifacts", value: String(artifacts.length), glyph: "descriptor" },
      { label: "Marks", value: String(marks.length), glyph: "topology" },
    ],
    eventLog: recentEvents,
    interfaceIndex,
    projection: liveProjectionStatus(payload),
  };
}

type TopologyProjectionIndex = {
  runsByEe: Map<string, any[]>;
  childCounts: Map<string, any[]>;
  bindings: ScopedRows;
  artifacts: ScopedRows;
  marks: ScopedRows;
  movements: ScopedRows;
  workerEnvironmentsByEe: Map<string, any[]>;
  workerEnvironmentsByRun: Map<string, any[]>;
  workerEnvironmentsById: Map<string, any>;
  interfaces: any[];
  interfacesByRuleId: Map<string, any>;
  interfacePositionsById: Map<string, number[]>;
  interfacePositionsByTemplate: Map<string, number[]>;
  workerInterfaceIds: Set<string>;
};

type ProjectEnvironmentArgs = {
  ee: any;
  index: number;
  projectionIndex: TopologyProjectionIndex;
  runDetailsRaw: RuntimePayload["runDetailsRaw"];
  runCheckpointsByRun: Map<string, any[]>;
};

function projectEnvironment(args: ProjectEnvironmentArgs): EnvironmentNode {
  const { ee, index, projectionIndex, runDetailsRaw, runCheckpointsByRun } = args;
  const accent = ACCENTS[index % ACCENTS.length];
  const eeId = stringValue(ee?.ee_id);
  const templateName = stringValue(ee?.template_name) || shortId(eeId) || `ee_${index + 1}`;
  const descriptor = descriptorOf(ee);
  const policyProjection = policyOf(ee, descriptor);
  const policy = policyProjection.policy;
  const goal = stringValue(ee?.goal_ref?.title ?? ee?.goal ?? descriptor?.goal);
  const status: EnvironmentStatus = "projected";
  const eeRuns = projectionIndex.runsByEe.get(eeId) ?? [];
  const eeBindings = projectionIndex.bindings.byEe.get(eeId) ?? [];
  const declaredInterfaceIds = refs(policy?.allowed_interfaces ?? policy?.allowed_interface_templates ?? descriptor?.allowed_interfaces ?? descriptor?.allowed_interface_templates);
  // Interface bindings live in the runs' CHECKPOINTS (owner): each checkpoint is an artifact
  // movement through an interface binding. A zone's bound interfaces are only the interfaces
  // materialized by bindings/checkpoints. Declared/allowed interfaces stay in policy JSON.
  const runCheckpointInterfaceNames = eeRuns.flatMap((run) => {
    const runId = stringValue(run?.run_id);
    const runDetail = runDetailsRaw[runId]?.detail;
    const detailedCheckpoints = array(runDetail?.checkpoints);
    const checkpoints = detailedCheckpoints.length > 0 ? detailedCheckpoints : (runCheckpointsByRun.get(runId) ?? []);
    return checkpoints.flatMap((checkpoint: any) => [
      stringValue(checkpoint?.interface),
      stringValue(checkpoint?.out_interface),
      ...array(checkpoint?.out_interfaces).map((name: any) => stringValue(name)),
    ]);
  }).filter(Boolean);
  const boundInterfaceIds = unique([
    ...eeBindings.map((binding) => stringValue(binding?.interface_id)).filter(Boolean),
    ...runCheckpointInterfaceNames,
  ]);
  const interfacePositions = new Set(projectionIndex.interfacePositionsByTemplate.get(templateName));
  for (const id of boundInterfaceIds) {
    for (const position of projectionIndex.interfacePositionsById.get(id) ?? []) interfacePositions.add(position);
  }
  const eeInterfaces = [...interfacePositions].sort((left, right) => left - right).map((position) => projectionIndex.interfaces[position]);
  const eeWorkerEnvironments = projectionIndex.workerEnvironmentsByEe.get(eeId) ?? [];
  const workers = unique([
    ...eeRuns.map((run) => stringValue(run?.worker)).filter(Boolean),
    ...declaredInterfaceIds.filter((interfaceId) => projectionIndex.workerInterfaceIds.has(interfaceId)),
  ]);
  const eeArtifacts = projectionIndex.artifacts.byEe.get(eeId) ?? [];
  const eeMarks = projectionIndex.marks.byEe.get(eeId) ?? [];
  const eeMovements = projectionIndex.movements.byEe.get(eeId) ?? [];
  const runProjections = eeRuns.map((run) => projectRun({
    run,
    eeId,
    projectionIndex,
    detailBundle: runDetailsRaw[stringValue(run?.run_id)],
    checkpointRowsRaw: runCheckpointsByRun.get(stringValue(run?.run_id)),
  }));
  const activeRuns = runProjections.filter((run) => run.status === "active").length;
  const lastArtifactAt = formatTime(timeOf(latestByTime(eeArtifacts)));
  const inFlight = eeArtifacts.filter((artifact) => ["open", "pending", "in_flight"].includes(stringValue(artifact?.status).toLowerCase())).length;
  const policyLayer = policyLayerLabel(policyProjection);
  const runtimeJson = {
    environment: templateName,
    ee_id: eeId,
    status: statusLabel(status),
    active_runs: activeRuns,
    worker_refs: workers.length,
    bindings: eeBindings.length,
    artifacts_in_flight: inFlight,
    artifact_rows: eeArtifacts.length,
    last_artifact_at: lastArtifactAt || null,
    policy_source: policyProjection.source,
  };

  // The world layer: interfaces of interface_type "world" bound in this zone. The stage
  // draws them as the upper layer (a line up to a named blue dot) — past that boundary
  // nothing is visible by design; only artifacts come back.
  const worldInterfaces = unique(
    eeInterfaces
      .filter((iface) => stringValue(iface?.interface_type).toLowerCase() === "world")
      .map((iface) => stringValue(iface?.rule_id ?? iface?.id))
      .filter(Boolean),
  );

  return {
    id: eeId,
    name: `${templateName}.ee`,
    status,
    statusLabel: statusLabel(status),
    accent,
    parentId: stringValue(ee?.parent_ee_id ?? ee?.parent_id ?? ee?.parent?.ee_id) || undefined,
    worldInterfaces,
    sections: [
      { kind: "runs", title: "Runs", items: topRunItems(runProjections) },
      { kind: "workers", title: "Workers", items: topItems(workers, "worker") },
      { kind: "interfaces", title: "Interfaces", items: topItems(eeInterfaces.map((iface) => stringValue(iface?.rule_id)), "interface") },
      { kind: "bindings", title: "Bindings", items: topItems(eeBindings.map((binding) => stringValue(binding?.interface_id)), "binding") },
    ],
    goal: goal || "No goal text projected by registry/ledger files.",
    description: `EE scope ${templateName} contains runs, interface bindings, artifact movement, worker environments, and policy data for this goal boundary.`,
    stats: [
      { label: "Status", value: statusLabel(status), tone: statusTone(status) },
      { label: "Child EEs", value: String(projectionIndex.childCounts.get(eeId)?.length ?? 0) },
      { label: "Active Runs", value: `${activeRuns} / ${eeRuns.length}` },
      { label: "Worker Refs", value: compactList(workers) || "none" },
      { label: "Bound Interfaces", value: compactList(boundInterfaceIds) || "none" },
      { label: "Declared Interfaces", value: compactList(declaredInterfaceIds) || "none" },
      { label: "Bindings", value: String(eeBindings.length) },
      { label: "Policy", value: policyLayer },
      { label: "Artifact Rows", value: `${inFlight} in flight / ${eeArtifacts.length} projected` },
      { label: "Last Artifact", value: lastArtifactAt || "none" },
    ],
    workers: workers.map((worker) => ({
      name: worker,
      description: `${eeRuns.filter((run) => stringValue(run?.worker) === worker).length} run rows`,
      tag: "WORKER",
      status: "declared",
    })),
    workerEnvironments: eeWorkerEnvironments.map(workerEnvironmentProjection),
    runs: runProjections,
    interfaces: eeInterfaces.map((iface) => interfaceRow(iface)),
    bindings: eeBindings.map((binding) => bindingRow(binding)),
    artifacts: eeArtifacts.map((artifact) => artifactRow(artifact)),
    marks: eeMarks.map((mark) => markRow(mark)),
    eventLog: eventRowsFromMovements(eeMovements, 8),
    policyJson: {
      environment: templateName,
      policy: policyLayer,
      policy_source: policyProjection.source,
      workers,
      allowed_interfaces: refs(policy?.allowed_interfaces ?? policy?.allowed_interface_templates ?? descriptor?.allowed_interfaces ?? descriptor?.allowed_interface_templates),
      allowed_tools: refs(policy?.allowed_tools ?? descriptor?.allowed_tools),
      raw_policy: policy && Object.keys(policy).length ? policy : undefined,
    },
    runtimeJson,
    lastArtifactAt: lastArtifactAt || "none",
    artifactFlow: `${inFlight} in flight / ${eeArtifacts.length} projected`,
  };
}

type ProjectRunArgs = {
  run: any;
  eeId: string;
  projectionIndex: TopologyProjectionIndex;
  detailBundle?: { detail?: any; trace?: any; error?: string };
  checkpointRowsRaw?: any[];
};

function projectRun(args: ProjectRunArgs): RunProjection {
  const { run, eeId, projectionIndex, detailBundle, checkpointRowsRaw } = args;
  const detail = detailBundle?.detail ?? {};
  const runFull = detail?.run ?? run;
  const runId = stringValue(runFull?.run_id ?? run?.run_id);
  const meta = record(runFull?.meta) ?? record(run?.meta) ?? {};
  const worker = stringValue(runFull?.worker ?? run?.worker) || "not projected";
  const declaredStatus = normalizeStatus(runFull?.status ?? run?.status);
  const detailBindings = array(detail?.bindings);
  const runBindings = detailBindings.length ? detailBindings : (projectionIndex.bindings.byRun.get(runId) ?? []);
  // Interface bindings live in the CHECKPOINTS (owner), not as run-level rows: each
  // checkpoint is an artifact movement through an interface binding. A run's bound
  // interfaces are the union of its checkpoints' inbound + outbound interface bindings.
  const detailedCheckpointRows = array(detail?.checkpoints);
  const projectedCheckpointRows = detailedCheckpointRows.length > 0 ? detailedCheckpointRows : (checkpointRowsRaw ?? []);
  const checkpointInterfaceNames = unique(projectedCheckpointRows.flatMap((checkpoint: any) => [
    stringValue(checkpoint?.interface),
    stringValue(checkpoint?.out_interface),
    ...array(checkpoint?.out_interfaces).map((name: any) => stringValue(name)),
  ]).filter(Boolean));
  const runInterfaceNames = runBindings.length
    ? unique(runBindings.map((binding) => stringValue(binding?.interface_id)).filter(Boolean))
    : checkpointInterfaceNames;
  const detailArtifacts = array(detail?.artifacts).concat(array(detail?.run_artifacts));
  const runArtifacts = detailArtifacts.length ? detailArtifacts : (projectionIndex.artifacts.byRun.get(runId) ?? []);
  const detailMarks = array(detail?.marks);
  const runMarks = detailMarks.length ? detailMarks : (projectionIndex.marks.byRun.get(runId) ?? []);
  const runMovements = projectionIndex.movements.byRun.get(runId) ?? [];
  const projectedWorkerEnvironments = workerEnvironmentsForRun(runFull, detail, projectionIndex)
    .map(workerEnvironmentProjection)
    .filter((projection) => Boolean(projection.id));
  const checkpoints = checkpointRows({ checkpoints: projectedCheckpointRows }, runBindings);
  const status: EnvironmentStatus = declaredStatus;
  const eventLog = eventRowsFromMovements(runMovements, 9);
  if (detailBundle?.error) {
    eventLog.unshift({
      time: "--:--:--",
      label: "Run detail projection incomplete",
      detail: detailBundle.error,
      tone: "amber",
      kind: "projection.error",
    });
  }
  const createdAt = stringValue(runFull?.created_at ?? run?.created_at);
  const workerEnvProjection = projectedWorkerEnvironments[0] ?? workerEnvironmentProjection({});
  const policy = record(runFull?.policy ?? meta.policy ?? detail?.formed_envelope ?? detail?.policy) ?? {};
  const runtimeJson = {
    run_id: runId,
    ee_id: eeId,
    worker,
    status: statusLabel(status),
    worker_environment: workerEnvProjection.id || null,
    worker_environments: projectedWorkerEnvironments.map((environment) => environment.id),
    bindings: runBindings.length,
    checkpoints: checkpoints.length,
    artifacts: runArtifacts.length,
    marks: runMarks.length,
  };
  const bindingsByInterface = firstRowsBy(runBindings, (binding) => stringValue(binding?.interface_id));

  return {
    id: runId,
    label: shortId(runId) || worker,
    status,
    statusLabel: statusLabel(status),
    worker,
    goal: stringValue(runFull?.goal ?? run?.goal ?? meta.goal) || "No run goal projected by registry/ledger files.",
    eeId,
    createdAt: formatDate(createdAt) || "unknown",
    runningFor: durationSince(createdAt),
    workerEnvironment: workerEnvProjection,
    workerEnvironments: projectedWorkerEnvironments,
    stats: [
      { label: "Status", value: statusLabel(status), tone: statusTone(status) },
      { label: "EE", value: shortId(eeId) || "none" },
      { label: "Worker", value: worker },
      { label: "Worker Environments", value: String(projectedWorkerEnvironments.length) },
      { label: "Bound Interfaces", value: String(runInterfaceNames.length) },
      { label: "Checkpoints", value: String(checkpoints.length) },
      { label: "Artifacts", value: String(runArtifacts.length) },
      { label: "Marks", value: String(runMarks.length) },
      { label: "Created", value: formatDate(createdAt) || "unknown" },
    ],
    // A run's interfaces ARE its checkpoint bindings (interface_binding / outbound_interface
    // _bindings on each checkpoint) — the realized truth, not a declared superset.
    interfaces: runInterfaceNames.map((name) => {
      const binding = bindingsByInterface.get(name);
      const iface = projectionIndex.interfacesByRuleId.get(name);
      return interfaceRow(iface ?? { rule_id: name }, binding);
    }),
    bindings: runBindings.map((binding) => bindingRow(binding)),
    checkpoints,
    eventLog,
    marks: runMarks.map((mark) => markRow(mark)),
    artifacts: runArtifacts.map((artifact) => artifactRow(artifact)),
    policyJson: {
      run_id: runId,
      worker,
      formed_policy: Object.keys(policy).length ? policy : undefined,
      bound_interfaces: runBindings.map((binding) => stringValue(binding?.interface_id)).filter(Boolean),
      worker_environment: workerEnvProjection.id || undefined,
    },
    runtimeJson,
  };
}

function orderEes(ees: any[]): any[] {
  const roots = ees.filter((ee) => !stringValue(ee?.parent_ee_id ?? ee?.parent_id ?? ee?.parent?.ee_id));
  const rootSet = new Set(roots);
  const rest = ees.filter((ee) => !rootSet.has(ee));
  return [...roots, ...rest];
}

function descriptorOf(ee: any): Record<string, unknown> {
  return record(ee?.executional_environment_descriptor)
    ?? record(ee?.ee_template_descriptor)
    ?? record(ee?.descriptor)
    ?? {};
}

function policyOf(ee: any, descriptor: Record<string, unknown>): { policy: Record<string, unknown>; source: string } {
  const runtimePolicy = record(ee?.policy);
  if (runtimePolicy) return { policy: runtimePolicy, source: "runtime" };
  const descriptorPolicy = record(descriptor.policy);
  if (descriptorPolicy) return { policy: descriptorPolicy, source: "descriptor" };
  return { policy: {}, source: "not projected" };
}

function interfaceRow(iface: any, binding?: any): DetailRow {
  const ruleId = stringValue(iface?.rule_id ?? binding?.interface_id) || "interface";
  const ifaceType = stringValue(iface?.interface_type).toLowerCase() || "system";
  const transfer = interfaceArtifactTransfer(iface);
  const transferStatus = interfaceArtifactTransferStatus(transfer);
  const transferDescription = interfaceArtifactTransferDescription(transfer);
  return {
    name: ruleId,
    description: [
      `${ifaceType} interface`,
      transferDescription,
    ].filter(Boolean).join(" / "),
    tag: ifaceType.toUpperCase(),
    status: transferStatus || stringValue(binding?.status) || stringValue(iface?.mode) || stringValue(iface?.status) || "projected",
    tone: interfaceArtifactTransferTone(transfer) ?? (ifaceType === "world" ? "blue" : undefined),
    ref: { kind: "interface", id: ruleId },
  };
}

function bindingRow(binding: any): DetailRow {
  const direction = stringValue(binding?.direction).toLowerCase();
  return {
    name: stringValue(binding?.interface_id) || "interface not projected",
    description: `checkpoint ${shortId(stringValue(binding?.checkpoint_id)) || "unknown"} / run ${shortId(stringValue(binding?.run_id)) || "unbound"}`,
    tag: direction === "inbound" ? "IN" : direction === "outbound" ? "OUT" : "BIND",
    status: stringValue(binding?.status) || "projected",
    tone: "blue",
    ref: stringValue(binding?.run_id) ? { kind: "run", id: stringValue(binding?.run_id) } : undefined,
  };
}

function artifactRow(artifact: any): DetailRow {
  const label = stringValue(artifact?.label ?? artifact?.artifact_label ?? artifact?.route_label);
  return {
    name: shortId(stringValue(artifact?.artifact_id ?? artifact?.id)) || stringValue(artifact?.name) || "artifact",
    description: label || "artifact row",
    tag: stringValue(artifact?.status).toUpperCase() || "ART",
    status: formatTime(timeOf(artifact)) || stringValue(artifact?.status) || "projected",
    tone: "blue",
  };
}

function markRow(mark: any): DetailRow {
  return {
    name: stringValue(mark?.mark_name ?? mark?.name ?? mark?.kind) || "mark",
    description: shortId(stringValue(mark?.run_id ?? mark?.artifact_id ?? mark?.trace_id)) || "mark row",
    tag: "MARK",
    status: formatTime(timeOf(mark)) || stringValue(mark?.status) || "projected",
    tone: "purple",
  };
}

export function workerEnvironmentProjection(value: any): WorkerEnvironmentProjection {
  const item = record(value) ?? {};
  const id = stringValue(item.worker_environment_id ?? item.id);
  const kind = stringValue(item.worker_environment_kind ?? item.kind ?? item.executor_kind ?? item.meta?.worker_environment_kind) || "worker_environment";
  const lifecycle = stringValue(item.lifecycle);
  const status = stringValue(item.status) || (id ? "unknown" : "not projected");
  const explicitLabel = stringValue(item.display_name ?? item.label ?? item.title ?? item.meta?.display_name ?? item.meta?.label);
  const explicitDetail = stringValue(item.description ?? item.detail ?? item.meta?.description ?? item.meta?.detail);
  const root = stringValue(item.root ?? item.worktree_root ?? item.project_root ?? item.path ?? item.meta?.root ?? item.meta?.project_root);
  const session = stringValue(item.session_id ?? item.browser_session_id ?? item.container_id ?? item.meta?.session_id);
  const transport = stringValue(item.transport ?? item.executor_kind ?? item.meta?.transport);
  const substrateFamily = stringValue(item.substrate_family ?? item.meta?.substrate_family);
  const door = stringValue(item.door ?? item.interface ?? item.meta?.door);
  const tools = refs(item.tools ?? item.tool_ids ?? item.meta?.tools);
  const surfaces = workerEnvironmentSurfaceProjections(item);
  const primarySurface = primaryProjectedWorkerEnvironmentSurface(surfaces);
  const cells: StatRow[] = [
    { label: "Kind", value: kind },
    { label: "Lifecycle", value: lifecycle || "unknown" },
    { label: "Status", value: status },
    { label: "Root", value: root || "none" },
    { label: "Session", value: session || "none" },
    { label: "Transport", value: transport || "none" },
  ];
  return {
    id,
    revision: stringValue(item.updated_at)
      || (Number.isFinite(Number(item.last_activity_movement_seq ?? item.generation ?? item.meta?.generation))
        ? String(item.last_activity_movement_seq ?? item.generation ?? item.meta?.generation)
        : ""),
    runId: stringValue(item.run_id ?? item.run ?? item.meta?.run_id),
    eeId: stringValue(item.ee_id ?? item.ee ?? item.meta?.ee_id),
    kind,
    lifecycle,
    status,
    live: item.live_available === true,
    label: explicitLabel || workerEnvironmentLabel(kind, id, status, primarySurface),
    detail: explicitDetail || workerEnvironmentDetail(root, session, transport, primarySurface),
    substrateFamily,
    door,
    root,
    session,
    transport,
    cells,
    capabilities: refs(item.capabilities ?? item.runtime_capabilities ?? item.meta?.capabilities),
    tools,
    surfaces,
  };
}

function workerEnvironmentSurfaceProjections(item: Record<string, any>): WorkerEnvironmentProjection["surfaces"] {
  const values = array(
    item.client_surfaces
    ?? item.surfaces
    ?? item.views
    ?? item.meta?.client_surfaces
    ?? item.meta?.surfaces
    ?? item.meta?.views,
  );
  return values.flatMap((value, index) => {
    const surface = record(value);
    if (!surface) return [];
    const kind = stringValue(surface.kind ?? surface.type);
    if (!kind) return [];
    const modeValue = stringValue(surface.mode).toLowerCase();
    if (modeValue !== "direct" && modeValue !== "fixture" && modeValue !== "evidence") return [];
    const id = stringValue(surface.id);
    const status = stringValue(surface.status);
    if (!id || !status) return [];
    return [{
      id,
      kind,
      label: stringValue(surface.label ?? surface.title) || kind.replace(/[_-]+/g, " "),
      mode: modeValue,
      status,
      viewerUrl: stringValue(surface.viewer_url ?? surface.viewerUrl ?? surface.url ?? surface.endpoint),
      protocol: stringValue(surface.protocol ?? surface.transport),
      root: stringValue(surface.root ?? surface.path),
      readOnly: surface.read_only !== false && surface.readOnly !== false,
      data: record(surface.data) ?? {},
    }];
  });
}

function workerEnvironmentLabel(
  kind: string,
  id: string,
  status: string,
  surface: WorkerEnvironmentProjection["surfaces"][number] | null,
): string {
  const kindLabel = kind.replace(/[_-]+/g, " ");
  if (status === "offline" || status === "declared" || status === "registered") return `${kindLabel} kind`;
  if (!id) return "not projected";
  return surface?.label || kindLabel;
}

function workerEnvironmentDetail(
  root: string,
  session: string,
  transport: string,
  surface: WorkerEnvironmentProjection["surfaces"][number] | null,
): string {
  if (root) return root;
  if (session) return `room ${shortId(session)}`;
  const projectedTransport = surface?.protocol || transport;
  if (projectedTransport) return `${surface?.label || "Room"} · ${projectedTransport}`;
  return "formed room attached to a run";
}

function primaryProjectedWorkerEnvironmentSurface(
  surfaces: WorkerEnvironmentProjection["surfaces"],
): WorkerEnvironmentProjection["surfaces"][number] | null {
  return surfaces.find((surface) => (
    surface.mode === "direct"
    && surface.kind !== "connection"
    && surface.kind !== "files"
    && surface.kind !== "changes"
  ))
    ?? surfaces.find((surface) => surface.kind !== "connection")
    ?? surfaces[0]
    ?? null;
}

function checkpointRows(detail: any, runBindings: any[]): CheckpointRow[] {
  const checkpoints = array(detail?.checkpoints);
  if (checkpoints.length > 0) {
    return checkpoints.map((checkpoint, index) => ({
      id: stringValue(checkpoint?.checkpoint_id ?? checkpoint?.id) || `checkpoint_${index + 1}`,
      label: stringValue(checkpoint?.label ?? checkpoint?.tool_name ?? checkpoint?.kind) || `checkpoint ${index + 1}`,
      status: stringValue(checkpoint?.status) || "projected",
      tool: stringValue(checkpoint?.tool_name ?? checkpoint?.tool),
      interfaceName: stringValue(checkpoint?.template_name ?? checkpoint?.interface ?? checkpoint?.interface_template_name),
      artifactLabel: stringValue(checkpoint?.artifact_label ?? checkpoint?.label),
      artifactBindings: typeof checkpoint?.artifact_bindings === "number" ? checkpoint.artifact_bindings : undefined,
      interfaceBindings: typeof checkpoint?.interface_bindings === "number" ? checkpoint.interface_bindings : undefined,
      requiredVerifiedArtifacts: requiredVerifiedArtifactRows(checkpoint),
      time: formatTime(timeOf(checkpoint)),
    }));
  }
  const byCheckpoint = new Map<string, any[]>();
  for (const binding of runBindings) {
    const checkpointId = stringValue(binding?.checkpoint_id);
    if (!checkpointId) continue;
    const rows = byCheckpoint.get(checkpointId) ?? [];
    rows.push(binding);
    byCheckpoint.set(checkpointId, rows);
  }
  return Array.from(byCheckpoint.entries()).map(([checkpointId, rows]) => {
    const inbound = rows.find((binding) => stringValue(binding?.direction).toLowerCase() === "inbound");
    return {
      id: checkpointId,
      label: checkpointId,
      status: stringValue(rows[0]?.status) || "projected",
      interfaceName: stringValue(inbound?.interface_id ?? rows[0]?.interface_id),
      interfaceBindings: rows.length,
      time: formatTime(timeOf(rows[0])),
    };
  });
}

function requiredVerifiedArtifactRows(checkpoint: any) {
  // The raw rows follow the generated data-surface contract (schemas/required_verified_artifact);
  // the projection only adds the ledger-joined `outcome`.
  type RawRequired = import("./generated/required_verified_artifact").RequiredVerifiedArtifact & { outcome?: string };
  const rows = (array(checkpoint?.required_verified_artifacts) as RawRequired[])
    .map((row) => ({
      mark: stringValue(row?.mark),
      artifactLabel: stringValue(row?.artifact_label),
      verifier: stringValue(row?.verifier),
      outcome: (["closed", "failed"].includes(stringValue(row?.outcome))
        ? stringValue(row?.outcome)
        : "declared") as "declared" | "closed" | "failed",
    }))
    .filter((row) => row.mark);
  return rows.length > 0 ? rows : undefined;
}

function eventRowsFromMovements(movements: any[], limit: number): EventLogRow[] {
  return movements
    .slice(0, limit)
    .map((movement) => {
      const kind = stringValue(movement?.kind) || "movement";
      const artifact = stringValue(movement?.artifact_label ?? movement?.artifact_id) || "artifact";
      const failed = /failed|refused|denied|error/i.test(kind);
      return {
        time: formatTime(Number(movement?.recorded_at_unix_ms) || 0) || "--:--:--",
        label: `${kind} · ${artifact}`,
        detail: `${movementEndpointText(movement?.from)} → ${movementEndpointText(movement?.to)}`,
        tone: failed ? "amber" : "blue",
        kind,
      } as EventLogRow;
    });
}

function movementEndpointText(value: any): string {
  const endpoint = record(value) ?? {};
  const kind = stringValue(endpoint.kind) || "endpoint";
  const id = stringValue(endpoint.id) || "unknown";
  return `${kind}:${shortId(id) || id}`;
}

type ScopedRows = { byRun: Map<string, any[]>; byEe: Map<string, any[]> };

function groupRowsBy<T>(rows: T[], key: (row: T) => string): Map<string, T[]> {
  const groups = new Map<string, T[]>();
  for (const row of rows) {
    const id = key(row);
    const members = groups.get(id);
    if (members) members.push(row);
    else groups.set(id, [row]);
  }
  return groups;
}

function firstRowsBy<T>(rows: T[], key: (row: T) => string): Map<string, T> {
  const index = new Map<string, T>();
  for (const row of rows) {
    const id = key(row);
    if (!index.has(id)) index.set(id, row);
  }
  return index;
}

function indexScopedRows(
  rows: any[],
  eeKey: (row: any) => string,
  runKey: (row: any) => string,
  eeIdsByRun: Map<string, Set<string>>,
): ScopedRows {
  const byRun = groupRowsBy(rows, runKey);
  const byEe = new Map<string, any[]>();
  for (const row of rows) {
    const targets = new Set(eeIdsByRun.get(runKey(row)));
    targets.add(eeKey(row));
    for (const id of targets) {
      const members = byEe.get(id);
      if (members) members.push(row);
      else byEe.set(id, [row]);
    }
  }
  return { byRun, byEe };
}

function artifactRunId(row: any): string {
  return stringValue(row?.run_id ?? row?.meta?.run_id);
}

function artifactEeId(row: any): string {
  return stringValue(row?.ee_id ?? row?.meta?.ee_id);
}

function topRunItems(runs: RunProjection[]): TopologyItem[] {
  if (runs.length === 0) return [{ label: "no run", tone: "gray" }];
  return runs.slice(0, 2).map((run) => ({
    label: run.worker || run.label,
    tone: statusTone(run.status),
    ref: { kind: "run", id: run.id },
  }));
}

function topItems(values: string[], kind: "worker" | "interface" | "binding"): TopologyItem[] {
  const fallback = {
    worker: ["no worker ref"],
    interface: ["no interface"],
    binding: ["no binding"],
  }[kind];
  return (values.length ? values : fallback).slice(0, 2).map((value) => ({
    label: value,
    tone: value.startsWith("no ") ? "gray" : kind === "interface" ? "blue" : kind === "binding" ? "purple" : "gray",
  }));
}

function runEeId(run: any, bindingByRun: Map<string, any>): string {
  const meta = record(run?.meta) ?? {};
  const sourceMeta = record(meta.source_meta) ?? {};
  const trustedArrival = record(meta.trusted_arrival) ?? {};
  const runId = stringValue(run?.run_id);
  const binding = runId ? bindingByRun.get(runId) : undefined;
  return stringValue(run?.ee_id ?? meta.ee_id ?? sourceMeta.ee_id ?? trustedArrival.ee_id ?? binding?.ee_id);
}

function workerEnvironmentEeId(workerEnvironment: any): string {
  const meta = record(workerEnvironment?.meta) ?? {};
  return stringValue(workerEnvironment?.ee_id ?? meta.ee_id);
}

function workerEnvironmentsForRun(run: any, detail: any, projectionIndex: TopologyProjectionIndex): any[] {
  const meta = record(run?.meta) ?? {};
  const inline = record(detail?.worker_environment) ?? record(run?.worker_environment) ?? record(meta.worker_environment) ?? {};
  const runId = stringValue(run?.run_id ?? run?.id ?? meta.run_id);
  const workerEnvironmentId = stringValue(
    inline.worker_environment_id
    ?? inline.id
    ?? meta.worker_environment_id
    ?? run?.worker_environment_id,
  );
  const workerEnvironmentKind = stringValue(
    inline.worker_environment_kind
    ?? inline.kind
    ?? meta.worker_environment_kind
    ?? run?.worker_environment_kind,
  );
  const candidates = [
    ...array(detail?.worker_environments),
    ...(projectionIndex.workerEnvironmentsByRun.get(runId) ?? []),
  ];
  if (workerEnvironmentId && Object.keys(inline).length > 0) candidates.unshift(inline);
  const exact = workerEnvironmentId
    ? projectionIndex.workerEnvironmentsById.get(workerEnvironmentId)
    : undefined;
  if (exact) candidates.unshift(Object.keys(inline).length > 0 ? { ...exact, ...inline } : exact);
  const sameRun = runId
    ? (projectionIndex.workerEnvironmentsByRun.get(runId) ?? [])
    : [];
  if (!workerEnvironmentId && Object.keys(inline).length > 0 && sameRun.length === 1) {
    const formed = sameRun[0];
    const formedKind = stringValue(formed?.worker_environment_kind ?? formed?.kind);
    if (!workerEnvironmentKind || !formedKind || workerEnvironmentKind === formedKind) {
      candidates.unshift({ ...formed, ...inline });
    }
  }
  const byId = new Map<string, any>();
  for (const candidate of candidates) {
    const item = record(candidate);
    if (!item) continue;
    const id = stringValue(item.worker_environment_id ?? item.id);
    if (!id || byId.has(id)) continue;
    byId.set(id, item);
  }
  return [...byId.values()];
}

function normalizeStatus(value: unknown): EnvironmentStatus {
  const text = stringValue(value).toLowerCase();
  if (text === "active" || text === "running" || text === "online") return "active";
  if (text === "idle" || text === "parked") return "idle";
  if (text === "settled") return "settled";
  if (["done", "completed", "finished", "closed"].includes(text)) return "done";
  if (text === "failed" || text === "blocked" || text === "error") return "blocked";
  if (text === "stale") return "stale";
  if (text === "offline") return "offline";
  if (!text) return "unknown";
  return "unknown";
}

function statusLabel(status: EnvironmentStatus): string {
  if (status === "projected") return "Projected";
  if (status === "active") return "Active";
  if (status === "idle") return "Idle";
  if (status === "settled") return "Settled";
  if (status === "done") return "Completed";
  if (status === "blocked") return "Blocked";
  if (status === "stale") return "Stale";
  if (status === "offline") return "Offline";
  return "Unknown";
}

function statusTone(status: EnvironmentStatus): Accent {
  if (status === "projected") return "blue";
  if (status === "active") return "green";
  if (status === "idle") return "yellow";
  if (status === "settled") return "gray";
  if (status === "done") return "green";
  if (status === "blocked") return "red";
  if (status === "stale") return "amber";
  return "gray";
}

function policyLayerLabel(policyProjection: { policy: Record<string, unknown>; source: string }): string {
  const { policy, source } = policyProjection;
  const explicit = stringValue(policy.policy_layer ?? policy.layer ?? policy.id);
  if (explicit) return explicit;
  if (Object.keys(policy).length) return `${source} policy`;
  return "not projected";
}

function compactList(values: string[], max = 3): string {
  const uniqueValues = unique(values.filter(Boolean));
  if (uniqueValues.length <= max) return uniqueValues.join(", ");
  return `${uniqueValues.slice(0, max).join(", ")} +${uniqueValues.length - max}`;
}

function interfaceArtifactTransfer(iface: any): InterfaceArtifactTransferProjection | undefined {
  const raw = record(iface?.artifact_transfer);
  if (!raw) return undefined;
  return {
    kind: stringValue(raw.kind),
    status: stringValue(iface?.artifact_transfer_protocol_status) || stringValue(raw.status) || "unknown",
    path: stringValue(raw.path) || undefined,
    reason: stringValue(iface?.artifact_transfer_target_reason) || stringValue(raw.reason) || undefined,
    targetCount: numberValue(iface?.artifact_transfer_target_count, 0),
    readyTargets: numberValue(iface?.artifact_transfer_ready_targets, 0),
    activationRefs: array(raw.activation_refs).map(stringValue).filter(Boolean),
  };
}

function interfaceArtifactTransferStatus(transfer?: InterfaceArtifactTransferProjection): string {
  if (!transfer) return "";
  if (transfer.status === "ready") return "hand-in ready";
  if (transfer.status === "unavailable") return "hand-in unavailable";
  if (transfer.status === "unbound") return "hand-in unbound";
  if (transfer.status === "policy_denied") return "hand-in denied";
  if (transfer.status === "unknown") return "hand-in unknown";
  return "";
}

function interfaceArtifactTransferDescription(transfer?: InterfaceArtifactTransferProjection): string {
  if (!transfer) return "";
  if (transfer.status === "not_declared") return "";
  if (transfer.status === "ready" && transfer.readyTargets) return `${transfer.readyTargets} active target${transfer.readyTargets === 1 ? "" : "s"}`;
  return transfer.reason || interfaceArtifactTransferStatus(transfer);
}

function interfaceArtifactTransferTone(transfer?: InterfaceArtifactTransferProjection): Accent | undefined {
  if (!transfer) return undefined;
  if (transfer.status === "ready") return "green";
  if (transfer.status === "unavailable") return "red";
  if (transfer.status === "policy_denied") return "red";
  if (transfer.status === "unbound") return "amber";
  if (transfer.status === "unknown") return "amber";
  return undefined;
}

function refs(raw: any): string[] {
  if (!Array.isArray(raw)) return [];
  return raw.map((item) => stringValue(item?.ref ?? item?.name ?? item)).filter(Boolean);
}

function unique(values: string[]): string[] {
  return Array.from(new Set(values));
}

function array(value: any): any[] {
  return Array.isArray(value) ? value : [];
}

function record(value: any): Record<string, any> | undefined {
  return value && typeof value === "object" && !Array.isArray(value) ? value : undefined;
}

function stringValue(value: unknown): string {
  if (typeof value === "string") return value.trim();
  if (typeof value === "number" && Number.isFinite(value)) return String(value);
  return "";
}

function numberValue(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function shortId(value: string): string {
  const text = value.trim();
  if (!text) return "";
  if (text.length <= 18) return text;
  return `${text.slice(0, 9)}...${text.slice(-5)}`;
}

function latestByTime(rows: any[]): any {
  return [...rows].sort((a, b) => timeOf(b) - timeOf(a))[0];
}

function timeOf(row: any): number {
  const value = row?.recorded_at_unix_ms ?? row?.created_at_unix_ms ?? row?.ts_unix_ms ?? row?.created_at ?? row?.ts ?? row?.time;
  if (typeof value === "number") return value > 10_000_000_000 ? value : value * 1000;
  if (typeof value === "string") {
    const parsed = Date.parse(value);
    return Number.isNaN(parsed) ? 0 : parsed;
  }
  return 0;
}

function formatTime(ms: number): string {
  if (!ms) return "";
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return "";
  return TIME_FORMATTER.format(date);
}

function formatDate(value: string): string {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return DATE_FORMATTER.format(date);
}

function durationSince(value: string): string {
  if (!value) return "unknown";
  const started = Date.parse(value);
  if (Number.isNaN(started)) return "unknown";
  const minutes = Math.max(0, Math.round((Date.now() - started) / 60_000));
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest ? `${hours}h ${rest}m` : `${hours}h`;
}
