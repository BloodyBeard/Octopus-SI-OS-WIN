import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useClientPreferences } from "../../clientPreferences";
import { notifyClientEvent } from "../../clientNotifications";
import { readProjection, formatProjectionError } from "../../projectionClient";
import { useProjectionGeneration } from "../../projectionLive";
import { decodeActivityReadState, decodeMarkActivity, decodeMarkKinds, decodeOperatorActivity, latestActivityMessages, selectActivity, type MarkActivityGroup, type MarkActivityItem } from "./markActivity";

export function useMarkActivity(profile: string, home: string, primary: boolean, open: boolean, visibleChatRunIds: readonly string[] = []) {
  const preferences = useClientPreferences();
  const generation = useProjectionGeneration();
  const key = `octopus.mark-activity.${encodeURIComponent(profile)}.${encodeURIComponent(home)}.read.v1`;
  const dismissedKey = `octopus.mark-activity.${encodeURIComponent(profile)}.${encodeURIComponent(home)}.dismissed.v1`;
  const [snapshot, setSnapshot] = useState<{ key: string; items: MarkActivityItem[]; kinds: string[] } | null>(null);
  const [failure, setFailure] = useState("");
  const [readFailure, setReadFailure] = useState("");
  const [readState, setReadState] = useState<{ key: string; ids: Set<string>; dismissed: Set<string> } | null>(null);
  const [windowFocused, setWindowFocused] = useState(() => typeof document !== "undefined" && document.hasFocus() && document.visibilityState !== "hidden");
  const [arrival, setArrival] = useState<{ key: string; id: string } | null>(null);
  const visibleRunKey = JSON.stringify([...new Set(visibleChatRunIds.filter(Boolean))].sort());
  const visibleRuns = useMemo<ReadonlySet<string>>(() => new Set(windowFocused ? JSON.parse(visibleRunKey) as string[] : []), [visibleRunKey, windowFocused]);
  const storedRead = useRef<{ key: string; ids: Set<string>; dismissed: Set<string> } | null>(null);
  const arrivals = useRef<{ key: string; seen: Set<string>; pending: Set<string> } | null>(null);
  const sourceItems = useMemo(() => snapshot?.key === key ? snapshot.items : [], [key, snapshot]);
  const read = useMemo(() => readState?.key === key ? readState.ids : new Set<string>(), [key, readState]);
  const dismissed = useMemo(() => readState?.key === key ? readState.dismissed : new Set<string>(), [key, readState]);
  const projectedItems = useMemo(() => latestActivityMessages(sourceItems).filter((item) => !dismissed.has(item.id)), [sourceItems, dismissed]);
  const items = useMemo(() => projectedItems.filter((item) => !read.has(item.id)
    && !(item.navigation.kind === "run" && visibleRuns.has(item.navigation.runId))), [projectedItems, read, visibleRuns]);
  const filtered = useMemo(() => selectActivity(items, preferences.markActivityMode, preferences.markActivityTypes), [items, preferences.markActivityMode, preferences.markActivityTypes]);
  const listed = useMemo(() => selectActivity(projectedItems, preferences.markActivityMode, preferences.markActivityTypes), [projectedItems, preferences.markActivityMode, preferences.markActivityTypes]);

  const mergeRead = useCallback((added: ReadonlySet<string> = new Set(), removed: ReadonlySet<string> = new Set()) => {
    const persisted = decodeActivityReadState(window.localStorage.getItem(key));
    const persistedDismissed = decodeActivityReadState(window.localStorage.getItem(dismissedKey));
    const nextDismissed = new Set([...persistedDismissed, ...(storedRead.current?.key === key ? storedRead.current.dismissed : []), ...removed]);
    const next = new Set([...persisted, ...(storedRead.current?.key === key ? storedRead.current.ids : []), ...added, ...nextDismissed]);
    if (next.size !== persisted.size) window.localStorage.setItem(key, JSON.stringify([...next]));
    if (nextDismissed.size !== persistedDismissed.size) window.localStorage.setItem(dismissedKey, JSON.stringify([...nextDismissed]));
    storedRead.current = { key, ids: next, dismissed: nextDismissed };
    setReadState(storedRead.current);
    setReadFailure("");
  }, [key, dismissedKey]);

  useEffect(() => {
    if (typeof document === "undefined") return;
    const observe = () => setWindowFocused(document.hasFocus() && document.visibilityState !== "hidden");
    observe();
    window.addEventListener("focus", observe);
    window.addEventListener("blur", observe);
    document.addEventListener("visibilitychange", observe);
    return () => {
      window.removeEventListener("focus", observe);
      window.removeEventListener("blur", observe);
      document.removeEventListener("visibilitychange", observe);
    };
  }, []);

  useEffect(() => {
    if (snapshot?.key !== key || readState?.key !== key || !visibleRuns.size) return;
    const ids = sourceItems.filter((item) => item.navigation.kind === "run"
      && visibleRuns.has(item.navigation.runId) && !read.has(item.id)).map((item) => item.id);
    if (!ids.length) return;
    try { mergeRead(new Set(ids)); }
    catch (error) { setReadFailure(formatProjectionError(error)); }
  }, [key, mergeRead, read, readState?.key, snapshot?.key, sourceItems, visibleRuns]);

  useEffect(() => {
    try { mergeRead(); }
    catch (error) { setReadFailure(formatProjectionError(error)); }
  }, [generation, mergeRead, open]);

  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key !== key && event.key !== dismissedKey && event.key !== null) return;
      try {
        if (event.storageArea && event.storageArea !== window.localStorage) return;
        const incoming = decodeActivityReadState(event.newValue);
        if (event.key === dismissedKey) mergeRead(new Set(), incoming);
        else mergeRead(incoming);
      }
      catch (error) { setReadFailure(formatProjectionError(error)); }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [key, dismissedKey, mergeRead]);

  useEffect(() => {
    let cancelled = false;
    void Promise.all([
      readProjection("projection.marks.list"),
      readProjection("projection.descriptors.mark_kinds.list"),
      readProjection("projection.operator.chats.list", { include_activity: true }),
    ]).then(([marks, kinds, attention]) => {
      const next = { key, items: decodeOperatorActivity(attention, decodeMarkActivity(marks)), kinds: decodeMarkKinds(kinds) };
      if (!cancelled) { setSnapshot(next); setFailure(""); }
    }).catch((error) => { if (!cancelled) setFailure(formatProjectionError(error)); });
    return () => { cancelled = true; };
  }, [generation, key]);

  useEffect(() => {
    if (snapshot?.key !== key) return;
    if (arrivals.current?.key !== key) {
      arrivals.current = { key, seen: new Set(sourceItems.map((item) => item.id)), pending: new Set() };
    } else {
      for (const item of sourceItems) {
        if (!arrivals.current.seen.has(item.id)) arrivals.current.pending.add(item.id);
        arrivals.current.seen.add(item.id);
      }
    }
    const pending = arrivals.current.pending;
    if (!primary) { pending.clear(); return; }
    const selected = new Set(filtered.map((item) => item.id));
    const present = new Set(sourceItems.map((item) => item.id));
    for (const id of pending) {
      if ((present.has(id) && !selected.has(id)) || read.has(id)) pending.delete(id);
    }
    if (readState?.key !== key || failure || readFailure) return;
    const newItems = filtered.filter((item) => pending.has(item.id));
    if (newItems.length) setArrival({ key, id: newItems[0].id });
    if (preferences.markNotifications === "never") { pending.clear(); return; }
    for (const item of [...filtered].reverse()) {
      if (!pending.delete(item.id)) continue;
      notifyClientEvent({
        title: item.title,
        body: item.agentName || item.worker || "",
        key: `activity:${key}:${item.id}`,
        sound: preferences.notificationSound,
        target: { kind: "mark", movementId: item.id },
        when: preferences.markNotifications,
      });
    }
  }, [failure, filtered, sourceItems, key, preferences.markNotifications, preferences.notificationSound, primary, read, readFailure, readState?.key, snapshot?.key]);

  const markRead = useCallback((ids: readonly string[]) => {
    try {
      const available = new Set(items.map((item) => item.id));
      if (snapshot?.key !== key || !ids.length || ids.some((id) => !available.has(id))) {
        throw new Error("The selected Activity items are unavailable in the current projection.");
      }
      mergeRead(new Set(ids));
      return true;
    } catch (error) { setReadFailure(formatProjectionError(error)); return false; }
  }, [items, key, mergeRead, snapshot?.key]);

  const groups = useMemo<MarkActivityGroup[]>(() => [{ key: "priority", label: "Priority", items: listed }], [listed]);
  const clearNotifications = useCallback(() => {
    try {
      if (snapshot?.key !== key || readState?.key !== key || failure || readFailure) throw new Error("Activity read state is unavailable.");
      const ids = listed.map((item) => item.id);
      if (!ids.length) throw new Error("There are no Activity notifications to clear.");
      mergeRead(new Set(), new Set(ids));
      return true;
    } catch (error) { setReadFailure(formatProjectionError(error)); return false; }
  }, [failure, listed, key, mergeRead, readFailure, readState?.key, snapshot?.key]);
  const kinds = useMemo(() => [...new Set([...(snapshot?.key === key ? snapshot.kinds : []), ...sourceItems.filter((item) => item.kind === "mark").map((item) => item.name), ...(preferences.markActivityTypes ?? [])])].sort(), [sourceItems, key, preferences.markActivityTypes, snapshot]);

  return {
    items, projectedItems, groups, kinds, read, dismissed, markRead, clearNotifications,
    error: failure || readFailure,
    loaded: snapshot?.key === key && readState?.key === key && !failure && !readFailure,
    unreadCount: filtered.length,
    listedCount: listed.length,
    arrivalId: arrival?.key === key ? `${key}:${arrival.id}` : "",
  };
}

export type MarkActivity = ReturnType<typeof useMarkActivity>;
