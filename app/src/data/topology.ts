import type { GlyphName } from "../components/IconGlyphs";

export type SectionKind = "runs" | "workers" | "interfaces" | "bindings" | "policy";
export type EnvironmentStatus = "active" | "idle" | "settled" | "done" | "offline" | "stale" | "blocked" | "projected" | "unknown";
export type Accent = "green" | "blue" | "purple" | "amber" | "gray" | "yellow" | "red";

export type SelectionRef =
  | { kind: "environment"; id: string }
  | { kind: "run"; id: string }
  | { kind: "interface"; id: string };

export type InterfaceArtifactTransferProjection = {
  kind: string;
  status: string;
  path?: string;
  reason?: string;
  targetCount?: number;
  readyTargets?: number;
  activationRefs?: string[];
};

export type InterfaceSummary = {
  id: string;
  type: string;
  source: string;
  status: string;
  boundIn: string[];
  artifactTransfer?: InterfaceArtifactTransferProjection;
  descriptor: Record<string, unknown>;
};

export type TopologyItem = {
  label: string;
  tone?: Accent;
  ref?: SelectionRef;
};

export type TopologySection = {
  kind: SectionKind;
  title: string;
  items: TopologyItem[];
};

export type DetailRow = {
  name: string;
  description: string;
  tag: string;
  status: string;
  tone?: Accent;
  ref?: SelectionRef;
};

export type EventLogRow = {
  time: string;
  label: string;
  detail?: string;
  tone?: Accent;
  kind?: string;
};

export type StatRow = {
  label: string;
  value: string;
  tone?: Accent;
};

export type CheckpointRow = {
  id: string;
  label: string;
  status: string;
  tool?: string;
  interfaceName?: string;
  artifactLabel?: string;
  time?: string;
  artifactBindings?: number;
  interfaceBindings?: number;
  requiredVerifiedArtifacts?: RequiredVerifiedArtifactRow[];
};

export type RequiredVerifiedArtifactRow = {
  mark: string;
  artifactLabel: string;
  verifier: string;
  outcome: "declared" | "closed" | "failed";
};

export type WorkerEnvironmentProjection = {
  id: string;
  revision: string;
  runId: string;
  eeId: string;
  kind: string;
  lifecycle: string;
  status: string;
  live: boolean;
  label: string;
  detail: string;
  substrateFamily: string;
  door: string;
  root: string;
  session: string;
  transport: string;
  cells: StatRow[];
  capabilities: string[];
  tools: string[];
  surfaces: WorkerEnvironmentSurfaceProjection[];
};

export type WorkerEnvironmentSurfaceProjection = {
  id: string;
  kind: string;
  label: string;
  mode: "direct" | "evidence" | "fixture";
  status: string;
  viewerUrl: string;
  protocol: string;
  root: string;
  readOnly: boolean;
  data: Record<string, unknown>;
};

export type RunProjection = {
  id: string;
  label: string;
  status: EnvironmentStatus;
  statusLabel: string;
  worker: string;
  goal: string;
  eeId: string;
  createdAt: string;
  runningFor: string;
  workerEnvironment: WorkerEnvironmentProjection;
  workerEnvironments: WorkerEnvironmentProjection[];
  stats: StatRow[];
  interfaces: DetailRow[];
  bindings: DetailRow[];
  checkpoints: CheckpointRow[];
  eventLog: EventLogRow[];
  marks: DetailRow[];
  artifacts: DetailRow[];
  policyJson: Record<string, unknown>;
  runtimeJson: Record<string, unknown>;
};

export type EnvironmentNode = {
  id: string;
  name: string;
  status: EnvironmentStatus;
  statusLabel: string;
  accent: Accent;
  parentId?: string;
  worldInterfaces: string[];
  sections: TopologySection[];
  goal: string;
  description: string;
  stats: StatRow[];
  workers: DetailRow[];
  workerEnvironments: WorkerEnvironmentProjection[];
  runs: RunProjection[];
  interfaces: DetailRow[];
  bindings: DetailRow[];
  artifacts: DetailRow[];
  marks: DetailRow[];
  eventLog: EventLogRow[];
  policyJson: Record<string, unknown>;
  runtimeJson: Record<string, unknown>;
  lastArtifactAt: string;
  artifactFlow: string;
};

export type ProjectionMode = "live" | "connecting" | "offline" | "error";

export type ProjectionStatus = {
  mode: ProjectionMode;
  endpoint: string;
  label: string;
  detail: string;
};

export type TopologySnapshot = {
  environments: EnvironmentNode[];
  summaryStats: string[];
  artifactStores: Array<{ label: string; value: string; glyph: GlyphName }>;
  eventLog: EventLogRow[];
  interfaceIndex: Record<string, InterfaceSummary>;
  projection: ProjectionStatus;
};

export const inspectorTabs = ["Overview", "Runs", "Environments", "Interfaces", "Policy", "Event Log"] as const;

export const emptyProjection: ProjectionStatus = {
  mode: "offline",
  endpoint: "registry/ledger files",
  label: "Projection offline",
  detail: "No file projection is available",
};

export function emptySnapshot(projection: ProjectionStatus = emptyProjection): TopologySnapshot {
  return {
    environments: [],
    summaryStats: ["0 EEs", "0 runs", "0 artifacts"],
    artifactStores: [],
    eventLog: [],
    interfaceIndex: {},
    projection,
  };
}
