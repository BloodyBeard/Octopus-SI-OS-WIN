export type DesktopPermissionRequest = { workerEnvironmentId: string; surfaceId: string; runId: string };

export type DesktopSurfaceRequest = {
  workerEnvironmentId: string;
  surfaceId: string;
  runId: string;
  viewId: string;
  operation: "pause_view" | "view" | "take_control" | "release_control" | "stop";
  screenId?: string;
  controlGeneration?: number;
};

export type DesktopScreen = {
  id: string; x: number; y: number; width: number; height: number;
  pixel_width: number; pixel_height: number;
};

export type DesktopEvidence = {
  view_image_ref?: { sha256: string };
  status: "ready" | "permission_required" | "stopped";
  permissions: { screen_capture: boolean; accessibility: boolean };
  control: { mode: "idle" | "model" | "human" | "stopped"; owner: string | null; generation: number };
  screens: DesktopScreen[];
  observation: null | {
    id: string; screen: DesktopScreen; input_revision: string; captured_at_unix_ms: number;
    focus: null | { app_id: string; process_id: number; window_id: number };
    image: null | { media_type: "image/png" | "image/jpeg"; data_base64: string; sha256: string; byte_length: number; width: number; height: number };
  };
};

export type DesktopSurfaceResponse = {
  workerEnvironmentId: string; surfaceId: string; runId: string;
} & ({ evidence: DesktopEvidence } | { paused: true });

export function resolveDesktopViewEvidence(previous: DesktopEvidence | null, incoming: DesktopEvidence): DesktopEvidence {
  const { view_image_ref: reference, ...evidence } = incoming;
  if (!reference) return evidence;
  const old = previous?.observation;
  if (!old?.image || !evidence.observation || old.image.sha256 !== reference.sha256 || JSON.stringify(old.screen) !== JSON.stringify(evidence.observation.screen)) {
    throw new Error("Desktop image reference has no matching frame in this viewer attachment");
  }
  return { ...evidence, observation: { ...evidence.observation, image: old.image } };
}

export function desktopEvidenceRevision(evidence: DesktopEvidence): string {
  const observation = evidence.observation;
  const image = observation?.image;
  return JSON.stringify({ ...evidence, observation: observation ? { ...observation, image: image ? { ...image, data_base64: undefined } : null } : null });
}
