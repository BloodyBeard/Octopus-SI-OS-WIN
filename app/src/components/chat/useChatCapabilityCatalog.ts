import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  formatProjectionError,
  loadChatExecutionContexts,
  loadChatWorkerEnvironmentRooms,
  type ChatExecutionContext,
  type ChatWorkerEnvironmentRoom,
} from "../../chatRuns";
import type { OctopusClientAccessProfile, OctopusProviderInterface } from "../../electron";
import {
  hasProjectionBridge,
  readRuntimeProviderInterfaces,
  readProjection,
  readProviderInterfaceRateLimits,
} from "../../projectionClient";
import { useProjectionRegistryGeneration, useProjectionStructureGeneration } from "../../projectionLive";
import { formatRuntimeError } from "../../runtimeClient";
import { readClientChatProjects, subscribeClientChatProjects } from "../../shell/clientWorkspaces";
import { toolPresentationsFromProjection } from "./chatActivityProjection";
import type { ChatToolPresentationIndex } from "./types";

type ProviderRead = { key: string; generation: number; promise: Promise<void> };

export function useChatCapabilityCatalog(
  accessProfile: OctopusClientAccessProfile,
  enabled: boolean,
  runtimeReady: boolean,
  runtimeHome: string,
) {
  const providerReadGenerationRef = useRef(0);
  const providerReadScopeRef = useRef("");
  const providerReadRef = useRef<ProviderRead | null>(null);
  const providerUsageReadRef = useRef<ProviderRead | null>(null);
  const capabilityCatalogScopeRef = useRef("");
  const [projectPresentationGeneration, setProjectPresentationGeneration] = useState(0);
  const [executionContexts, setExecutionContexts] = useState<ChatExecutionContext[]>([]);
  const [executionContextsLoaded, setExecutionContextsLoaded] = useState(false);
  const [completedCapabilityReadKey, setCompletedCapabilityReadKey] = useState("");
  const [workerEnvironmentRooms, setWorkerEnvironmentRooms] = useState<ChatWorkerEnvironmentRoom[]>([]);
  const [toolPresentations, setToolPresentations] = useState<ChatToolPresentationIndex>({});
  const [executionContextError, setExecutionContextError] = useState("");
  const [providerInterfaces, setProviderInterfaces] = useState<OctopusProviderInterface[]>([]);
  const [providerInterfacesLoaded, setProviderInterfacesLoaded] = useState(false);
  const [providerReadPending, setProviderReadPending] = useState(false);
  const [completedProviderReadKey, setCompletedProviderReadKey] = useState("");
  const [providerInterfaceError, setProviderInterfaceError] = useState("");
  const [providerUsageError, setProviderUsageError] = useState("");
  const projectionStructureGeneration = useProjectionStructureGeneration();
  const projectionRegistryGeneration = useProjectionRegistryGeneration();
  const scope = JSON.stringify([accessProfile, enabled, runtimeReady, runtimeHome]);
  const providerReadKey = JSON.stringify([scope, projectionRegistryGeneration]);
  const providerContextRef = useRef({ scope, key: providerReadKey });
  const providerInterfacesRefreshing = enabled && (providerReadPending || completedProviderReadKey !== providerReadKey);
  useLayoutEffect(() => { providerContextRef.current = { scope, key: providerReadKey }; }, [providerReadKey, scope]);

  const refreshProviderUsage = useCallback((): Promise<void> => {
    if (!enabled || providerContextRef.current.scope !== scope) return Promise.resolve();
    const key = providerContextRef.current.key;
    const generation = providerReadGenerationRef.current;
    const pending = providerUsageReadRef.current;
    if (pending?.key === key && pending.generation === generation) return pending.promise;
    const read: ProviderRead = { key, generation, promise: Promise.resolve() };
    providerUsageReadRef.current = read;
    const isCurrent = () => providerContextRef.current.key === key && providerReadGenerationRef.current === generation;
    setProviderUsageError("");
    read.promise = (async () => {
      try {
        const byProvider = await readProviderInterfaceRateLimits();
        if (!isCurrent()) return;
        setProviderInterfaces((current) => current.map((provider) => ({ ...provider, rateLimits: byProvider.get(provider.id) })));
      } catch (failure) {
        if (isCurrent()) {
          setProviderInterfaces((current) => current.map((provider) => ({ ...provider, rateLimits: undefined })));
          setProviderUsageError(`Provider usage is unavailable. ${formatRuntimeError(failure)}`);
        }
      } finally {
        if (providerUsageReadRef.current === read) providerUsageReadRef.current = null;
      }
    })();
    return read.promise;
  }, [enabled, scope]);

  const refreshProviderInterfaces = useCallback((invalidate = false): Promise<void> => {
    if (providerContextRef.current.key !== providerReadKey) return Promise.resolve();
    const pending = providerReadRef.current;
    if (!invalidate && pending?.key === providerReadKey && pending.generation === providerReadGenerationRef.current) return pending.promise;
    const generation = ++providerReadGenerationRef.current;
    if (providerReadScopeRef.current !== scope) {
      providerReadScopeRef.current = scope;
      setProviderInterfaces([]);
      setProviderInterfacesLoaded(false);
    }
    if (!enabled) {
      setProviderInterfaces([]);
      setProviderInterfacesLoaded(false);
      setProviderInterfaceError("");
      setProviderUsageError("");
      setProviderReadPending(false);
      return Promise.resolve();
    }
    setProviderInterfaceError("");
    setProviderReadPending(true);
    setProviderUsageError("");
    const read: ProviderRead = { key: providerReadKey, generation, promise: Promise.resolve() };
    providerReadRef.current = read;
    const isCurrent = () => providerContextRef.current.key === providerReadKey && providerReadGenerationRef.current === generation;
    read.promise = (async () => {
      try {
        const response = await readRuntimeProviderInterfaces();
        if (!isCurrent()) return;
        const providers = response.providers;
        setProviderInterfaces(providers);
        const declarationError = response.error
          ?? (providers.length === 0 ? "No world/provider interface is declared in the registry."
            : providers.every((provider) => !provider.models.length)
              ? providers.map((provider) => provider.modelCatalogError).filter(Boolean).join("\n") || "No models are available from the declared provider interfaces." : "");
        setProviderInterfaceError(declarationError);
        setProviderInterfacesLoaded(true);
        if (!declarationError) void refreshProviderUsage();
      } catch (failure) {
        if (isCurrent()) {
          setProviderInterfaces([]);
          setProviderInterfaceError(formatRuntimeError(failure));
          setProviderInterfacesLoaded(true);
        }
      } finally {
        if (isCurrent()) {
          setCompletedProviderReadKey(providerReadKey);
          setProviderReadPending(false);
        }
        if (providerReadRef.current === read) providerReadRef.current = null;
      }
    })();
    return read.promise;
  }, [enabled, providerReadKey, refreshProviderUsage, scope]);

  useEffect(() => {
    void refreshProviderInterfaces(true);
    return () => { providerReadGenerationRef.current += 1; };
  }, [refreshProviderInterfaces]);

  useEffect(() => {
    if (accessProfile !== "live" || !enabled) return;
    return window.octopusTransport?.onChatgptAccountChanged?.(() => { void refreshProviderInterfaces(true); });
  }, [accessProfile, enabled, refreshProviderInterfaces]);

  useEffect(() => {
    return subscribeClientChatProjects(accessProfile, () => {
      setProjectPresentationGeneration((generation) => generation + 1);
    });
  }, [accessProfile]);

  useEffect(() => {
    if (!enabled || !hasProjectionBridge()) {
      capabilityCatalogScopeRef.current = "";
      setExecutionContexts([]);
      setExecutionContextsLoaded(false);
      setWorkerEnvironmentRooms([]);
      setToolPresentations({});
      setExecutionContextError("");
      return;
    }
    let cancelled = false;
    if (capabilityCatalogScopeRef.current !== scope) {
      capabilityCatalogScopeRef.current = scope;
      setExecutionContexts([]);
      setExecutionContextsLoaded(false);
      setWorkerEnvironmentRooms([]);
      setExecutionContextError("");
    }
    const projectPresentations = readClientChatProjects(accessProfile);
    const projectDisplayNames = new Map(projectPresentations.map((project) => [project.id, project.name]));
    const projectColors = new Map(projectPresentations.map((project) => [project.id, project.color]));
    Promise.all([
      loadChatExecutionContexts(projectDisplayNames, projectColors),
      loadChatWorkerEnvironmentRooms(),
    ])
      .then(([contexts, rooms]) => {
        if (cancelled) return;
        setExecutionContexts(contexts);
        setCompletedCapabilityReadKey(providerReadKey);
        setExecutionContextsLoaded(true);
        setWorkerEnvironmentRooms(rooms);
        setExecutionContextError("");
      })
      .catch((failure) => {
        if (!cancelled) {
          setExecutionContextsLoaded(false);
          setExecutionContextError(formatProjectionError(failure));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [accessProfile, enabled, projectPresentationGeneration, projectionStructureGeneration, providerReadKey, scope]);

  useEffect(() => {
    if (!enabled || !hasProjectionBridge()) {
      setToolPresentations({});
      return;
    }
    let cancelled = false;
    readProjection("projection.tools.list", { limit: 5_000 })
      .then((tools) => {
        if (!cancelled) setToolPresentations(toolPresentationsFromProjection(tools));
      })
      .catch(() => {
        if (!cancelled) setToolPresentations({});
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, projectionRegistryGeneration]);

  return {
    executionContextError: capabilityCatalogScopeRef.current === scope ? executionContextError : "",
    executionContexts: capabilityCatalogScopeRef.current === scope ? executionContexts : [],
    executionContextsLoaded: capabilityCatalogScopeRef.current === scope && executionContextsLoaded && completedCapabilityReadKey === providerReadKey,
    providerInterfaceError: providerReadScopeRef.current === scope ? providerInterfaceError : "",
    providerInterfaces: providerReadScopeRef.current === scope ? providerInterfaces : [],
    providerInterfacesLoaded: providerReadScopeRef.current === scope && providerInterfacesLoaded,
    providerInterfacesRefreshing,
    providerUsageError: providerReadScopeRef.current === scope ? providerUsageError : "",
    refreshProviderInterfaces,
    refreshProviderUsage,
    toolPresentations,
    workerEnvironmentRooms: capabilityCatalogScopeRef.current === scope ? workerEnvironmentRooms : [],
  };
}
