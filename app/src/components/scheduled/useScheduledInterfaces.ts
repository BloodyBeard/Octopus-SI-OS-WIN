import { useCallback, useEffect, useRef, useState } from "react";
import { readProjection } from "../../projectionClient";
import { useProjectionGeneration } from "../../projectionLive";
import { normalizeScheduledInterface, normalizeScopeOptions } from "./scheduleModel";
import type { ScheduledInterfaceProjection, ScheduledProjectionResponse, ScheduledScopeOption } from "./types";

export function useScheduledInterfaces(enabled = true) {
  const projectionGeneration = useProjectionGeneration();
  const [tasks, setTasks] = useState<ScheduledInterfaceProjection[]>([]);
  const [scopeOptions, setScopeOptions] = useState<ScheduledScopeOption[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const requestGeneration = useRef(0);
  const loadedOnce = useRef(false);

  const reload = useCallback(async () => {
    const generation = requestGeneration.current + 1;
    requestGeneration.current = generation;
    if (!loadedOnce.current) setLoading(true);
    try {
      const response = await readProjection<ScheduledProjectionResponse>("projection.scheduled_interfaces.list");
      if (generation !== requestGeneration.current) return;
      const next = (response.scheduled_interfaces ?? [])
        .map(normalizeScheduledInterface)
        .filter((task): task is ScheduledInterfaceProjection => Boolean(task));
      setTasks(next);
      setScopeOptions(normalizeScopeOptions(response.scope_options));
      setError("");
    } catch (failure) {
      if (generation !== requestGeneration.current) return;
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      if (generation === requestGeneration.current) {
        loadedOnce.current = true;
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    if (enabled) void reload();
  }, [enabled, projectionGeneration, reload]);

  return { tasks, scopeOptions, loading, error, reload };
}
