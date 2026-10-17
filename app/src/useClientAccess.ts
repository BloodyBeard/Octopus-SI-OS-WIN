import { useCallback, useEffect, useRef, useState } from "react";
import { readClientAccessStatus } from "./clientAccess";
import type { OctopusClientAccessStatus } from "./electron";

export function useClientAccess() {
  const [access, updateAccess] = useState<OctopusClientAccessStatus | null>(null);
  const [accessError, setAccessError] = useState("");
  const generation = useRef(0);

  const setAccess = useCallback((status: OctopusClientAccessStatus) => {
    generation.current += 1;
    updateAccess(status);
    setAccessError("");
  }, []);

  const refresh = useCallback(async () => {
    const request = ++generation.current;
    try {
      const status = await readClientAccessStatus();
      if (request === generation.current) setAccess(status);
    } catch (error) {
      if (request !== generation.current) return;
      updateAccess(null);
      setAccessError(error instanceof Error ? error.message : String(error));
    }
  }, [setAccess]);

  useEffect(() => {
    const onFocus = () => { void refresh(); };
    const onVisibility = () => {
      if (document.visibilityState === "visible") void refresh();
    };
    const unsubscribe = window.octopusTransport?.onClientAccessLocked?.(setAccess);
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    void refresh();
    return () => {
      generation.current += 1;
      unsubscribe?.();
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [refresh, setAccess]);

  useEffect(() => {
    if (!access?.authenticated || !access.expiresAt) return;
    const expiresAt = Date.parse(access.expiresAt);
    if (!Number.isFinite(expiresAt)) return;
    const timer = window.setTimeout(() => { void refresh(); }, Math.max(0, expiresAt - Date.now()));
    return () => window.clearTimeout(timer);
  }, [access, refresh]);

  return { access, accessError, setAccess };
}
