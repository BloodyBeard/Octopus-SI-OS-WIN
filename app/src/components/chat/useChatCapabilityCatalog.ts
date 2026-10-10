import { useCallback, useEffect, useRef, useState } from "react";
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
import { useProjectionStructureGeneration } from "../../projectionLive";
import { formatRuntimeError } from "../../runtimeClient";
import { readClientChatProjects, subscribeClientChatProjects } from "../../shell/clientWorkspaces";
import { toolPresentationsFromProjection } from "./chatActivityProjection";
import type { ChatToolPresentationIndex } from "./types";

export function useChatCapabilityCatalog(
  accessProfile: OctopusClientAccessProfile,
  enabled: boolean,
  runtimeReady: boolean,
) {
  const providerReadGenerationRef = useRef(0);
  const capabilityCatalogProfileRef = useRef<OctopusClientAccessProfile | null>(null);
  const [projectPresentationGeneration, setProjectPresentationGeneration] = useState(0);
  const [executionContexts, setExecutionContexts] = useState<ChatExecutionContext[]>([]);
  const [executionContextsLoaded, setExecutionContextsLoaded] = useState(false);
  const [workerEnvironmentRooms, setWorkerEnvironmentRooms] = useState<ChatWorkerEnvironmentRoom[]>([]);
  const [toolPresentations, setToolPresentations] = useState<ChatToolPresentationIndex>({});
  const [executionContextError, setExecutionContextError] = useState("");
  const [providerInterfaces, setProviderInterfaces] = useState<OctopusProviderInterface[]>([]);
  const [providerInterfacesLoaded, setProviderInterfacesLoaded] = useState(false);
  const [providerInterfaceError, setProviderInterfaceError] = useState("");
  const projectionStructureGeneration = useProjectionStructureGeneration();

  const refreshProviderInterfaces = useCallback(async () => {
    const generation = providerReadGenerationRef.current + 1;
    providerReadGenerationRef.current = generation;
    if (!enabled) {
      setProviderInterfaces([]);
      setProviderInterfacesLoaded(false);
      setProviderInterfaceError("");
      return;
    }
    setProviderInterfaceError("");
    try {
      const response = await readRuntimeProviderInterfaces();
      if (providerReadGenerationRef.current !== generation) return;
      const providers = response.providers;
      setProviderInterfaces(providers);
      const declarationError = response.error
        ?? (providers.length === 0 ? "No world/provider interface is declared in the registry." : "");
      setProviderInterfaceError(declarationError);
      setProviderInterfacesLoaded(true);
      if (declarationError) return;
      void readProviderInterfaceRateLimits()
        .then((byProvider) => {
          if (providerReadGenerationRef.current !== generation) return;
          setProviderInterfaces((current) => current.map((provider) => ({
            ...provider,
            rateLimits: byProvider.get(provider.id),
          })));
        })
        .catch(() => undefined);
    } catch (failure) {
      if (providerReadGenerationRef.current === generation) {
        setProviderInterfaces([]);
        setProviderInterfaceError(formatRuntimeError(failure));
        setProviderInterfacesLoaded(true);
      }
    }
  }, [accessProfile, enabled, runtimeReady]);

  useEffect(() => {
    void refreshProviderInterfaces();
  }, [projectionStructureGeneration, refreshProviderInterfaces]);

  useEffect(() => {
    return subscribeClientChatProjects(accessProfile, () => {
      setProjectPresentationGeneration((generation) => generation + 1);
    });
  }, [accessProfile]);

  useEffect(() => {
    if (!enabled || !hasProjectionBridge()) {
      capabilityCatalogProfileRef.current = null;
      setExecutionContexts([]);
      setExecutionContextsLoaded(false);
      setWorkerEnvironmentRooms([]);
      setToolPresentations({});
      setExecutionContextError("");
      return;
    }
    let cancelled = false;
    setExecutionContextsLoaded(false);
    if (capabilityCatalogProfileRef.current !== accessProfile) {
      capabilityCatalogProfileRef.current = accessProfile;
      setExecutionContexts([]);
      setWorkerEnvironmentRooms([]);
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
  }, [accessProfile, enabled, projectPresentationGeneration, projectionStructureGeneration]);

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
  }, [enabled, projectionStructureGeneration]);

  return {
    executionContextError,
    executionContexts,
    executionContextsLoaded,
    providerInterfaceError,
    providerInterfaces,
    providerInterfacesLoaded,
    refreshProviderInterfaces,
    toolPresentations,
    workerEnvironmentRooms,
  };
}
