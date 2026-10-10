import { useEffect, useMemo, useState } from "react";
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

  useEffect(() => {
    let disposed = false;
    let request = 0;
    setLoading(true);
    const refresh = () => {
      const current = ++request;
      void readSurfaceDocumentSnapshot(accessProfile)
        .then((next) => {
          if (disposed || current !== request) return;
          setSnapshotState({ profile: accessProfile, value: next });
          setReadError("");
          setLoading(false);
        })
        .catch((error) => {
          if (disposed || current !== request) return;
          setSnapshotState({ profile: accessProfile, value: EMPTY_SNAPSHOT });
          setReadError(error instanceof Error ? error.message : String(error));
          setLoading(false);
        });
    };
    refresh();
    const unsubscribe = accessProfile === "lab"
      ? subscribeClientSurfaceDocuments(refresh)
      : () => {};
    return () => {
      disposed = true;
      unsubscribe();
    };
  }, [accessProfile, projectionGeneration]);

  return useMemo(() => materializeRegistry(
    snapshotState.profile === accessProfile ? snapshotState.value : EMPTY_SNAPSHOT,
    snapshotState.profile === accessProfile ? readError : "",
    loading || snapshotState.profile !== accessProfile,
    accessProfile,
  ), [accessProfile, loading, readError, snapshotState]);
}

async function readSurfaceDocumentSnapshot(accessProfile: "live" | "lab"): Promise<ClientSurfaceDocumentSnapshot> {
  if (accessProfile === "lab") return readClientSurfaceDocuments();
  return readProjection<ClientSurfaceDocumentSnapshot>("projection.client_surfaces.list");
}
