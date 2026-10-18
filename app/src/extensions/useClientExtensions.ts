import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { AdmittedClientSurfaceDocument, ClientSurfaceHostSource } from "../client-surfaces/types";
import type { OctopusClientAccessProfile } from "../electron";
import { builtinClientExtensions } from "./catalog";
import {
  applyClientExtensionLifecycle,
  mutateClientExtension,
  readClientExtensions,
  revealClientExtensions,
  subscribeClientExtensions,
} from "./transport";
import type {
  AdmittedClientExtension,
  ClientExtensionRecord,
  ClientExtensionLifecycleEvidence,
  ClientExtensionRevision,
  ClientExtensionRegistryState,
  ClientExtensionSource,
  ClientExtensionSourceConfiguration,
  ClientExtensionStoreSnapshot,
} from "./types";
import { admitClientExtensionSource } from "./validation";

const EMPTY_SNAPSHOT: ClientExtensionStoreSnapshot = {
  generation: 0,
  root: "",
  manifests: [],
  state: {},
  stateStatus: "valid",
  lifecycle: {},
  actionStatuses: [],
  errors: [],
};

export function useClientExtensions(accessProfile: OctopusClientAccessProfile): ClientExtensionRegistryState {
  const [snapshot, setSnapshot] = useState<ClientExtensionStoreSnapshot>(EMPTY_SNAPSHOT);
  const snapshotRef = useRef<ClientExtensionStoreSnapshot>(EMPTY_SNAPSHOT);
  const [hydrated, setHydrated] = useState(accessProfile !== "live");
  const [loading, setLoading] = useState(true);
  const [readError, setReadError] = useState("");
  const [refreshTick, setRefreshTick] = useState(0);

  useEffect(() => {
    if (accessProfile !== "live") {
      snapshotRef.current = EMPTY_SNAPSHOT;
      setSnapshot(EMPTY_SNAPSHOT);
      setReadError("");
      setHydrated(true);
      setLoading(false);
      return;
    }
    if (snapshotRef.current === EMPTY_SNAPSHOT) setHydrated(false);
    let disposed = false;
    let request = 0;
    const refresh = () => {
      const current = ++request;
      void readClientExtensions()
        .then((next) => {
          if (disposed || current !== request) return;
          snapshotRef.current = next;
          setSnapshot(next);
          setReadError("");
          setHydrated(true);
          setLoading(false);
        })
        .catch((error) => {
          if (disposed || current !== request) return;
          setReadError(error instanceof Error ? error.message : String(error));
          setLoading(false);
        });
    };
    refresh();
    const unsubscribe = subscribeClientExtensions(refresh);
    return () => {
      disposed = true;
      unsubscribe();
    };
  }, [accessProfile, refreshTick]);

  const effectiveSnapshot = accessProfile === "live" ? snapshot : EMPTY_SNAPSHOT;
  const materialized = useMemo(
    () => materializeClientExtensions(effectiveSnapshot, accessProfile, accessProfile === "live" ? readError : ""),
    [accessProfile, effectiveSnapshot, readError],
  );

  const mutate = useCallback(async (
    id: string,
    patch: {
      sources?: Record<string, ClientExtensionSourceConfiguration>;
      reviewed?: ClientExtensionRevision;
      dismissed?: ClientExtensionRevision;
    },
  ) => {
    if (accessProfile !== "live") throw new Error("Extensions are read-only in the Test client profile.");
    const next = await mutateClientExtension({
      id,
      expectedGeneration: snapshotRef.current.generation,
      ...patch,
    });
    snapshotRef.current = next;
    setSnapshot(next);
    setReadError("");
    setHydrated(true);
  }, [accessProfile]);

  const applyLifecycle = useCallback(async (
    extension: AdmittedClientExtension,
    action: "install" | "remove",
    operationId: string = crypto.randomUUID(),
  ) => {
    if (accessProfile !== "live") throw new Error("Extensions are read-only in the Test client profile.");
    const result = await applyClientExtensionLifecycle({
      action,
      id: extension.manifest.id,
      operationId,
      expectedGeneration: snapshotRef.current.generation,
      expectedRevision: extensionRevision(extension),
    });
    snapshotRef.current = result.snapshot;
    setSnapshot(result.snapshot);
    setReadError("");
    setHydrated(true);
  }, [accessProfile]);

  return {
    ...materialized,
    generation: effectiveSnapshot.generation,
    root: effectiveSnapshot.root,
    hydrated: accessProfile !== "live" || (hydrated && snapshotRef.current !== EMPTY_SNAPSHOT),
    loading: accessProfile === "live" ? loading : false,
    mutable: accessProfile === "live",
    refresh: () => setRefreshTick((current) => current + 1),
    install: async (id, operationId) => {
      const extension = requireExtensionRecord(materialized.extensions, id);
      if (!extension.configured) throw new Error(`Set up ${extension.manifest.name} before installing it.`);
      if (extension.manifest.contribution.kind === "surface" && extension.admission !== "reviewed") {
        await mutate(id, { reviewed: extensionRevision(extension) });
      }
      await applyLifecycle(extension, "install", operationId);
    },
    dismiss: async (id, operationId) => {
      const extension = requireExtensionRecord(materialized.extensions, id);
      if (extension.manifest.contribution.kind !== "surface") throw new Error("Built-in extensions do not require admission.");
      await mutate(id, { dismissed: extensionRevision(extension) });
      await applyLifecycle(extension, "remove", operationId);
    },
    uninstall: async (id, operationId) => {
      const extension = requireExtensionRecord(materialized.extensions, id);
      await applyLifecycle(extension, "remove", operationId);
    },
    configure: async (id, sources, install = false, operationId) => {
      const extension = requireExtensionRecord(materialized.extensions, id);
      const accepted = validateSourceConfiguration(extension, sources, install);
      await mutate(id, install && extension.manifest.contribution.kind === "surface"
        ? { sources: accepted, reviewed: extensionRevision(extension) }
        : { sources: accepted });
      if (install) await applyLifecycle(extension, "install", operationId);
    },
    revealRoot: async () => {
      if (accessProfile !== "live") throw new Error("Extensions are read-only in the Test client profile.");
      await revealClientExtensions();
    },
  };
}

export function materializeClientExtensions(
  snapshot: ClientExtensionStoreSnapshot,
  accessProfile: OctopusClientAccessProfile,
  readError: string,
): Omit<ClientExtensionRegistryState, "generation" | "root" | "hydrated" | "loading" | "mutable" | "refresh" | "install" | "dismiss" | "uninstall" | "configure" | "revealRoot"> {
  const errors = [...snapshot.errors];
  if (readError) errors.push(readError);
  const admittedCustom = snapshot.manifests.flatMap((source) => {
    const result = admitClientExtensionSource(source);
    if (!result.admitted) {
      errors.push(`${source.path}: ${result.verdict.issues[0]?.message || "extension manifest was refused"}`);
      return [];
    }
    const release = result.admitted.surface?.document.release_channel;
    if (accessProfile === "live" && release && release !== "stable") {
      errors.push(`${source.path}: lab extension surfaces are unavailable in the live client`);
      return [];
    }
    return [result.admitted];
  });
  const customById = new Map<string, AdmittedClientExtension[]>();
  for (const extension of admittedCustom) {
    const rows = customById.get(extension.manifest.id) ?? [];
    rows.push(extension);
    customById.set(extension.manifest.id, rows);
  }
  const builtinIds = new Set(builtinClientExtensions.map((extension) => extension.manifest.id));
  const custom = [...customById.entries()].flatMap(([id, rows]) => {
    if (builtinIds.has(id)) {
      errors.push(`client extension "${id}" conflicts with a built-in extension`);
      return [];
    }
    if (rows.length !== 1) {
      errors.push(`client extension "${id}" is duplicated across ${rows.map((row) => row.sourcePath).join(", ")}`);
      return [];
    }
    return rows;
  });
  const admitted = [...builtinClientExtensions, ...custom];
  const extensions: ClientExtensionRecord[] = admitted.map((extension) => {
    const state = snapshot.state[extension.manifest.id] ?? {};
    const sourceConfiguration = configuredSources(extension.manifest.sources ?? [], state.sources ?? {}, errors, extension.manifest.id);
    const builtin = extension.manifest.contribution.kind !== "surface";
    const revision = extensionRevision(extension);
    const reviewed = sameRevision(state.reviewed, revision);
    const dismissed = sameRevision(state.dismissed, revision);
    const admission: ClientExtensionRecord["admission"] = builtin
      ? "builtin"
      : reviewed
        ? "reviewed"
        : dismissed
          ? "dismissed"
          : "pending";
    const lifecycle = snapshot.lifecycle[extension.manifest.id] ?? missingLifecycleEvidence();
    const receiptInstalled = snapshot.stateStatus === "valid"
      && lifecycle.receiptState === "valid"
      && lifecycle.desiredState === "installed"
      && sameRevision(lifecycle.sourceRevision ?? undefined, revision);
    const installed = receiptInstalled && (builtin || admission === "reviewed");
    const configured = (extension.manifest.sources ?? []).every((source) => (
      !source.required || sourceConfigured(source, sourceConfiguration[source.id])
    ));
    const surface = extension.surface ? {
      ...extension.surface,
      sourceExtensionId: extension.manifest.id,
      hostSources: hostSources(extension.manifest.sources ?? [], sourceConfiguration),
    } : undefined;
    return { ...extension, admission, surface, installed, configured, lifecycle, sourceConfiguration };
  }).sort((left, right) => (
    Number(right.installed) - Number(left.installed)
    || left.manifest.category.localeCompare(right.manifest.category)
    || left.manifest.name.localeCompare(right.manifest.name)
  ));

  const installedBuiltinSurfaceIds: string[] = [];
  const installedBuiltinWidgetIds: string[] = [];
  const builtinWidgetSources: Record<string, ClientSurfaceHostSource[]> = {};
  const installedActions: ClientExtensionRegistryState["installedActions"] = [];
  const workspaceSurfaces: AdmittedClientSurfaceDocument[] = [];
  const voiceSurfaces: AdmittedClientSurfaceDocument[] = [];
  for (const extension of extensions) {
    if (!extension.installed || !extension.configured) continue;
    const contribution = extension.manifest.contribution;
    if (contribution.kind === "builtin_action") {
      installedActions.push({
        extensionId: extension.manifest.id,
        actionId: contribution.action_id,
        triggers: contribution.triggers,
      });
      continue;
    }
    if (contribution.kind === "builtin") {
      if (extension.manifest.category === "tab") installedBuiltinSurfaceIds.push(contribution.surface_id);
      else {
        installedBuiltinWidgetIds.push(contribution.surface_id);
        builtinWidgetSources[contribution.surface_id] = hostSources(
          extension.manifest.sources ?? [],
          extension.sourceConfiguration,
        );
      }
      continue;
    }
    if (!extension.surface) continue;
    const target = extension.surface.document.target;
    if (target.kind === "workspace.surface") workspaceSurfaces.push(extension.surface);
    if (target.kind === "voice.widget" || target.kind === "voice.window") voiceSurfaces.push(extension.surface);
  }

  return {
    extensions,
    errors,
    workspaceSurfaces,
    voiceSurfaces,
    installedBuiltinSurfaceIds,
    installedBuiltinWidgetIds,
    builtinWidgetSources,
    installedActions,
    actionStatuses: snapshot.actionStatuses,
  };
}

function configuredSources(
  definitions: ClientExtensionSource[],
  raw: Record<string, ClientExtensionSourceConfiguration>,
  errors: string[],
  extensionId: string,
): Record<string, ClientExtensionSourceConfiguration> {
  const allowed = new Set(definitions.map((source) => source.id));
  const result: Record<string, ClientExtensionSourceConfiguration> = {};
  for (const [id, configuration] of Object.entries(raw)) {
    if (!allowed.has(id)) {
      errors.push(`client extension "${extensionId}" has configuration for unknown source "${id}"`);
      continue;
    }
    const definition = definitions.find((source) => source.id === id);
    if (!definition) continue;
    const allowedFields = new Set(definition.kind === "filesystem" ? ["path"] : ["device_id"]);
    const accepted = Object.fromEntries(Object.entries(configuration).filter(([field]) => {
      if (allowedFields.has(field)) return true;
      errors.push(`client extension "${extensionId}" source "${id}" has unknown field "${field}"`);
      return false;
    }));
    result[id] = accepted;
  }
  return result;
}

function sourceConfigured(source: ClientExtensionSource, configuration?: ClientExtensionSourceConfiguration): boolean {
  if (source.kind === "filesystem") return isAbsoluteClientPath(configuration?.path ?? "");
  return Boolean(configuration?.device_id?.trim()) || !source.required;
}

function isAbsoluteClientPath(value: string): boolean {
  const candidate = value.trim();
  return candidate.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(candidate) || /^\\\\[^\\]/.test(candidate);
}

function requireExtensionRecord(extensions: ClientExtensionRecord[], id: string): ClientExtensionRecord {
  const extension = extensions.find((candidate) => candidate.manifest.id === id);
  if (!extension) throw new Error(`Client extension "${id}" is not admitted.`);
  return extension;
}

function extensionRevision(extension: AdmittedClientExtension): ClientExtensionRevision {
  return { digest: extension.sourceDigest, revision: extension.manifest.version };
}

function missingLifecycleEvidence(): ClientExtensionLifecycleEvidence {
  return {
    receiptState: "missing",
    desiredState: "",
    operationId: "",
    requestDigest: "",
    sourceRevision: null,
    completedAt: "",
    receiptDigest: "",
    error: "",
  };
}

function sameRevision(
  value: ClientExtensionRevision | undefined,
  revision: ClientExtensionRevision,
): boolean {
  return value?.digest === revision.digest && value.revision === revision.revision;
}

function validateSourceConfiguration(
  extension: ClientExtensionRecord,
  raw: Record<string, ClientExtensionSourceConfiguration>,
  requireComplete: boolean,
): Record<string, ClientExtensionSourceConfiguration> {
  const errors: string[] = [];
  const accepted = configuredSources(extension.manifest.sources ?? [], raw, errors, extension.manifest.id);
  if (errors.length) throw new Error(errors[0]);
  if (requireComplete) {
    const missing = (extension.manifest.sources ?? []).find((source) => (
      source.required && !sourceConfigured(source, accepted[source.id])
    ));
    if (missing) throw new Error(`Configure required source "${missing.label}" before installing ${extension.manifest.name}.`);
  }
  return accepted;
}

function hostSources(
  definitions: ClientExtensionSource[],
  configuration: Record<string, ClientExtensionSourceConfiguration>,
): ClientSurfaceHostSource[] {
  return definitions.flatMap((source): ClientSurfaceHostSource[] => {
    if (source.kind === "filesystem") {
      const sourcePath = configuration[source.id]?.path?.trim() ?? "";
      if (!sourcePath && !source.required) return [];
      return [{
        id: source.id,
        kind: "filesystem",
        path: sourcePath,
        read: source.read,
        refreshIntervalMs: source.refresh_interval_ms ?? 30000,
      }];
    }
    const deviceId = configuration[source.id]?.device_id?.trim() ?? "";
    if (!deviceId && !source.required) return [];
    return [{
      id: source.id,
      kind: "device",
      device: source.device,
      deviceId,
    }];
  });
}
