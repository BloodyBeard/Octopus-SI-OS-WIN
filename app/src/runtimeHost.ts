import { useCallback, useEffect, useState } from "react";
import type { OctopusRuntimeHostState } from "./electron";

export type RuntimeClientState = OctopusRuntimeHostState & {
  available: boolean;
};

const EMPTY_STATE: RuntimeClientState = {
  phase: "probing",
  ready: false,
  available: true,
  ownership: "none",
  pid: null,
  home: "",
  sourceRegistry: "",
  runtimeBinary: "",
  repoRoot: "",
  launchCwd: "",
  launchCommand: "",
  launchEnv: {},
  endpoint: "",
  error: null,
  updatedAtMs: 0,
};

export function useRuntimeHost() {
  const [state, setState] = useState<RuntimeClientState>(EMPTY_STATE);

  const applyHostState = useCallback((next: OctopusRuntimeHostState) => {
    setState((current) => ({ ...current, ...next, available: true }));
  }, []);

  const refreshObservation = useCallback(async () => {
    const bridge = window.octopusTransport;
    if (!bridge?.bootStatus) {
      setState({
        ...EMPTY_STATE,
        phase: "external_preview",
        ready: true,
        available: false,
      });
      return;
    }
    try {
      applyHostState(await bridge.bootStatus());
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      const accessRequired = /locked or expired|profile lab|primary Octopus window/i.test(message);
      setState((current) => ({
        ...current,
        phase: accessRequired ? "access_required" : "degraded",
        ready: false,
        available: true,
        error: message,
        updatedAtMs: Date.now(),
      }));
    }
  }, [applyHostState]);

  useEffect(() => {
    const bridge = window.octopusTransport;
    if (!bridge?.bootStatus) {
      setState({ ...EMPTY_STATE, phase: "external_preview", ready: true, available: false });
      return;
    }
    const unsubscribe = bridge.onRuntimeHostState?.(applyHostState);
    void refreshObservation();
    return () => unsubscribe?.();
  }, [applyHostState, refreshObservation]);

  return state;
}

export type RuntimeHostView = ReturnType<typeof useRuntimeHost>;
