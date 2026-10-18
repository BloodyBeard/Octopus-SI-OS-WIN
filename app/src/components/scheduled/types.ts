import type { OctopusScheduledDraft } from "../../electron";

export type ScheduledInterfaceStatus = "enabled" | "disabled" | "invalid";

export type ScheduledInterfaceProjection = {
  interfaceId: string;
  displayName: string;
  status: ScheduledInterfaceStatus;
  valid: boolean;
  activationDigest: string;
  activationPath: string;
  descriptorPath: string;
  schedule: {
    type: "interval";
    intervalSeconds: number;
  } | {
    type: "cron";
    cron: string;
  } | {
    type: "once";
    dueAt: string;
  } | {
    type: "inactivity";
    seconds: number;
    interfaceMatch: Record<string, string>;
  } | null;
  timezone: string;
  fireOnStart: boolean;
  instructions: string;
  reminderEnv: string;
  runTemplate: {
    runId: string;
    eeId: string;
    workerInterfaceId: string;
    checkpointCount: number;
    outboundInterfaceIds: string[];
    artifactBindingCount: number;
  } | null;
  cloneEligible: boolean;
  lastRunAtMs: number | null;
  lastMovementSeq: number | null;
  nextRunAtMs: number | null;
  nextRunSource: "driver_state" | "schedule" | "";
  error: string;
};

export type ScheduledEditorDraft = OctopusScheduledDraft & {
  interfaceId: string;
  sourceInterfaceId: string;
};

export type ScheduledEditorMode =
  | { kind: "create"; source: ScheduledInterfaceProjection }
  | { kind: "edit"; task: ScheduledInterfaceProjection };

export type ScheduledProjectionResponse = {
  scheduled_interfaces?: unknown[];
};
