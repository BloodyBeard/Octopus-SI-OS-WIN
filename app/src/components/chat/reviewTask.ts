import type { RunProjection, WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import type { OctopusWorkerEnvironmentGitSnapshot } from "../../electron";
import { surfaceHasProjectedChanges } from "../worker-environment/changesProjection";
import { workerEnvironmentPanelKind } from "../worker-environment/surfaceRegistry";
import { MAX_ATTACHMENT_BYTES } from "./constants";

export const REVIEWER_WORKER_INTERFACE_ID = "system_worker_reviewer";

export type ReviewTaskSeed = {
  attachment: File;
  prompt: string;
  sourceRunId: string;
};

type ReviewableSurface = {
  environment: WorkerEnvironmentProjection;
  surface: WorkerEnvironmentSurfaceProjection;
};

type MaterializedReviewSource = {
  changes: Record<string, unknown>;
  source: {
    run_id: string;
    ee_id: string;
    worker: string;
    worker_environment_id: string;
    worker_environment_revision: string;
    surface_id: string;
    surface_protocol: string;
    root: string;
    git_content_revision?: string;
  };
};

export async function createReviewTaskSeed(run: RunProjection): Promise<ReviewTaskSeed> {
  const candidates = findChangesSurfaces(run);
  if (!candidates.length) throw new Error("The active run has no projected Changes surface to review.");
  const materialized = (await Promise.all(candidates.map((candidate) => materializeReviewSource(run, candidate))))
    .filter((candidate): candidate is MaterializedReviewSource => candidate !== null);
  if (!materialized.length) throw new Error("The projected Changes surfaces contain no reviewable file changes.");
  const payload = JSON.stringify({
    schema_version: 2,
    source_run_id: run.id,
    sources: materialized,
  }, null, 2);
  const byteLength = new TextEncoder().encode(payload).byteLength;
  if (byteLength > MAX_ATTACHMENT_BYTES) {
    throw new Error(`The projected review context is ${formatBytes(byteLength)} and exceeds the ${formatBytes(MAX_ATTACHMENT_BYTES)} attachment boundary.`);
  }
  return {
    attachment: new File([payload], `review-context-${safeName(run.id)}.json`, {
      type: "application/json",
      lastModified: Date.now(),
    }),
    prompt: materialized.length === 1
      ? "Review the attached projected changes. Report only discrete actionable findings introduced by this change, each with precise file and line evidence, then give an overall correctness verdict. Do not modify files."
      : "Review every exact Worker Environment change set in the attached context. Report only discrete actionable findings introduced by these changes, each with precise file and line evidence and its source room, then give an overall correctness verdict. Do not modify files.",
    sourceRunId: run.id,
  };
}

async function materializeReviewSource(run: RunProjection, candidate: ReviewableSurface): Promise<MaterializedReviewSource | null> {
  const { environment, surface } = candidate;
  const directGit = surface.mode === "direct" && surface.protocol.trim().toLowerCase() === "git.worktree";
  let changes = surface.data;
  let revision = environment.revision;
  let gitContentRevision = "";
  if (directGit) {
    const inspect = window.octopusTransport?.inspectWorkerEnvironmentGit;
    if (!inspect) throw new Error("Direct Git review requires the Octopus desktop client.");
    const snapshot = await inspect({
      runId: run.id,
      surfaceId: surface.id,
      workerEnvironmentId: environment.id,
    });
    assertGitSnapshotIdentity(snapshot, run.id, environment.id, surface.id);
    if (!snapshot.diffs.length) return null;
    revision = snapshot.projectionRevision;
    gitContentRevision = snapshot.revision;
    changes = {
      base: snapshot.base,
      branch: snapshot.branch,
      diffs: snapshot.diffs,
      staged_files: snapshot.stagedFiles,
      unstaged_files: snapshot.unstagedFiles,
    };
  } else if (!surfaceHasProjectedChanges(surface)) {
    return null;
  }
  return {
    source: {
      run_id: run.id,
      ee_id: run.eeId,
      worker: run.worker,
      worker_environment_id: environment.id,
      worker_environment_revision: revision,
      surface_id: surface.id,
      surface_protocol: surface.protocol,
      root: surface.root || environment.root,
      ...(gitContentRevision ? { git_content_revision: gitContentRevision } : {}),
    },
    changes,
  };
}

function assertGitSnapshotIdentity(
  snapshot: OctopusWorkerEnvironmentGitSnapshot,
  runId: string,
  workerEnvironmentId: string,
  surfaceId: string,
): void {
  if (
    snapshot.runId !== runId
    || snapshot.workerEnvironmentId !== workerEnvironmentId
    || snapshot.surfaceId !== surfaceId
  ) {
    throw new Error("Git review returned state for a different Worker Environment surface.");
  }
}

function findChangesSurfaces(run: RunProjection): ReviewableSurface[] {
  const environments = run.workerEnvironments.length
    ? run.workerEnvironments
    : run.workerEnvironment.id
      ? [run.workerEnvironment]
      : [];
  return environments.flatMap((environment) => (
    environment.surfaces
      .filter((surface) => workerEnvironmentPanelKind(surface) === "changes")
      .map((surface) => ({ environment, surface }))
  ));
}

function safeName(value: string): string {
  return value.replace(/[^a-zA-Z0-9._-]+/g, "-").slice(0, 96) || "run";
}

function formatBytes(value: number): string {
  if (value < 1_000_000) return `${Math.max(1, Math.round(value / 1_000))} KB`;
  return `${(value / 1_000_000).toFixed(1)} MB`;
}
