import { useCallback, useEffect, useMemo, useState } from "react";
import type { AdmittedClientSurfaceDocument } from "./types";
import { homeSurfaceIdentity, selectHomeSurfaceDocuments, type HomeSurfaceScope } from "./homePresentation";

const MAX_DISMISSALS = 512;
const EMPTY_DISMISSALS = new Set<string>();

export function useHomeSurfaceDocuments(
  surfaces: AdmittedClientSurfaceDocument[],
  scope: HomeSurfaceScope & { accessProfile: "live" | "lab" },
) {
  const storageKey = `octopus.home.surface-dismissals.v1:${JSON.stringify([scope.accessProfile, scope.runId, scope.interfaceId])}`;
  const [stored, setStored] = useState<{ key: string; ids: Set<string>; error: string }>(() => readDismissals(storageKey));
  const [now, setNow] = useState(Date.now);
  useEffect(() => { setStored(readDismissals(storageKey)); }, [storageKey]);
  const ids = stored.key === storageKey ? stored.ids : EMPTY_DISMISSALS;
  const selection = useMemo(() => selectHomeSurfaceDocuments(surfaces, scope, Math.max(now, Date.now()), ids), [surfaces, scope.runId, scope.interfaceId, now, ids]);
  useEffect(() => { setNow(Date.now()); }, [surfaces, storageKey]);

  useEffect(() => {
    const refresh = () => setNow(Date.now());
    const timer = selection.nextExpiry === null ? null : window.setTimeout(refresh, Math.min(2_147_483_647, Math.max(1, selection.nextExpiry - Date.now())));
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      if (timer !== null) window.clearTimeout(timer);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [selection.nextExpiry, now]);

  useEffect(() => {
    const refresh = (event: StorageEvent) => {
      if (event.key === storageKey || event.key === null) setStored(readDismissals(storageKey));
    };
    window.addEventListener("storage", refresh);
    return () => window.removeEventListener("storage", refresh);
  }, [storageKey]);

  const setVisible = useCallback((item: AdmittedClientSurfaceDocument, visible: boolean) => {
    const identity = homeSurfaceIdentity(item);
    if (!selection.documents.some((candidate) => homeSurfaceIdentity(candidate) === identity)) return;
    const next = new Set(ids);
    if (visible) next.delete(identity);
    else next.add(identity);
    let error = "";
    try {
      if (next.size > MAX_DISMISSALS) throw new Error("Too many saved widget dismissals; restore widgets before saving more");
      window.localStorage.setItem(storageKey, JSON.stringify([...next]));
    } catch (reason) {
      error = `Widget dismissal could not be saved: ${reason instanceof Error ? reason.message : String(reason)}`;
    }
    setStored({ key: storageKey, ids: next, error });
  }, [ids, selection.documents, storageKey]);

  const restoreAll = useCallback(() => {
    try {
      window.localStorage.removeItem(storageKey);
      setStored({ key: storageKey, ids: new Set(), error: "" });
    } catch (reason) {
      setStored({ key: storageKey, ids, error: `Widget dismissals could not be cleared: ${String(reason)}` });
    }
  }, [storageKey, ids]);

  return {
    ...selection,
    visibleDocuments: stored.key === storageKey && !stored.error ? selection.visibleDocuments : [],
    dismissed: ids,
    errors: [...selection.errors, ...(stored.key === storageKey && stored.error ? [stored.error] : [])],
    dismiss: (item: AdmittedClientSurfaceDocument) => setVisible(item, false),
    restore: (item: AdmittedClientSurfaceDocument) => setVisible(item, true),
    restoreAll,
  };
}

function readDismissals(key: string) {
  try {
    const raw = window.localStorage.getItem(key);
    const value: unknown = raw === null ? [] : JSON.parse(raw);
    if (!Array.isArray(value) || value.length > MAX_DISMISSALS || value.some((id) => typeof id !== "string" || id.length > 4096)) {
      throw new Error("Saved widget dismissals are malformed; use Restore all to clear them");
    }
    return { key, ids: new Set<string>(value), error: "" };
  } catch (reason) {
    return { key, ids: new Set<string>(), error: reason instanceof Error ? reason.message : String(reason) };
  }
}
