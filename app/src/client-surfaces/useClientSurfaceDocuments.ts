import { useEffect, useMemo, useRef, useState } from "react";
import { readClientSurfaceDocuments, subscribeClientSurfaceDocuments } from "./transport";
import { readProjection } from "../projectionClient";
import { useProjectionGeneration } from "../projectionLive";
import type {
  AdmittedClientSurfaceDocument,
  ClientSurfaceAdmissionVerdict,
  ClientSurfaceDocumentSnapshot,
} from "./types";
import { materializeRegistry } from "./documentRegistry";

export type ClientSurfaceDocumentRegistryState = {
  generation: number;
  root: string;
  documents: AdmittedClientSurfaceDocument[];
  voiceSurfaces: AdmittedClientSurfaceDocument[];
  voiceWidgets: AdmittedClientSurfaceDocument[];
  voiceWindows: AdmittedClientSurfaceDocument[];
  workspaceSurfaces: AdmittedClientSurfaceDocument[];
  workerEnvironmentSurfaces: AdmittedClientSurfaceDocument[];
  verdicts: ClientSurfaceAdmissionVerdict[];
  errors: string[];
  loading: boolean;
  hydrated: boolean;
};

const EMPTY_SNAPSHOT: ClientSurfaceDocumentSnapshot = {
  generation: 0,
  root: "",
  documents: [],
  errors: [],
};

export function useClientSurfaceDocuments(accessProfile: "live" | "lab"): ClientSurfaceDocumentRegistryState {
  const projectionGeneration = useProjectionGeneration();
  const [snapshotState, setSnapshotState] = useState({ profile: accessProfile, value: EMPTY_SNAPSHOT });
  const [readError, setReadError] = useState("");
  const [loading, setLoading] = useState(true);
  const refresh = useRef<(() => void) | null>(null);

  useEffect(() => {
    let disposed = false;
    let running = false;
    let requested = false;
    setLoading(true);
    const request = () => {
      if (disposed) return;
      requested = true;
      if (running) return;
      running = true;
      setLoading(true);
      void (async () => {
        while (!disposed && requested) {
          requested = false;
          try {
            const next = await readSurfaceDocumentSnapshot(accessProfile);
            if (disposed) return;
            setSnapshotState({ profile: accessProfile, value: next });
            setReadError("");
          } catch (error) {
            if (disposed) return;
            setSnapshotState({ profile: accessProfile, value: EMPTY_SNAPSHOT });
            setReadError(error instanceof Error ? error.message : String(error));
          }
        }
        running = false;
        if (!disposed) setLoading(false);
      })();
    };
    refresh.current = request;
    const unsubscribe = accessProfile === "lab"
      ? subscribeClientSurfaceDocuments(request)
      : () => {};
    return () => {
      disposed = true;
      unsubscribe();
      if (refresh.current === request) refresh.current = null;
    };
  }, [accessProfile]);

  useEffect(() => {
    refresh.current?.();
  }, [accessProfile, projectionGeneration]);

  return useMemo(() => ({ ...materializeRegistry(
    snapshotState.profile === accessProfile ? snapshotState.value : EMPTY_SNAPSHOT,
    snapshotState.profile === accessProfile ? readError : "",
    loading || snapshotState.profile !== accessProfile,
    accessProfile,
  ), hydrated: snapshotState.profile === accessProfile && snapshotState.value !== EMPTY_SNAPSHOT && !readError }),
  [accessProfile, loading, readError, snapshotState]);
}

async function readSurfaceDocumentSnapshot(accessProfile: "live" | "lab"): Promise<ClientSurfaceDocumentSnapshot> {
  if (accessProfile === "lab") return readClientSurfaceDocuments();
  return readProjection<ClientSurfaceDocumentSnapshot>("projection.client_surfaces.list");
}
