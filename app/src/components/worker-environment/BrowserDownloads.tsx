import { useDeferredValue, useEffect, useRef, useState, type MouseEvent } from "react";
import type { WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import type { OctopusBrowserDownload, OctopusBrowserDownloadHistory, OctopusBrowserDownloadRequest, OctopusBrowserDownloadSnapshot, OctopusClientContextMenuItem } from "../../electron";
import { OcButton, OcMenuSurface } from "../../design-system/components";
import { Glyph } from "../IconGlyphs";
import { formatBytes } from "../chat/format";

const activeStatuses = new Set(["started", "in_progress", "paused"]);
const peekHistory = new Map<string, { sequence: number; availableAt: number }>();
type Props = { active: boolean; environment: WorkerEnvironmentProjection; surface: WorkerEnvironmentSurfaceProjection; standalone?: boolean; openRequest?: number };

export function browserDownloadIndicator(snapshot: OctopusBrowserDownloadSnapshot | null) {
  const records = snapshot?.downloads ?? [], active = records.filter(record => activeStatuses.has(record.status));
  const total = active.reduce((sum, record) => sum + record.totalBytes, 0);
  const received = active.reduce((sum, record) => sum + Math.min(record.totalBytes, Math.max(0, record.receivedBytes)), 0);
  const percent = active.length && active.every(record => record.totalBytes > 0) ? Math.round(received / total * 100) : null;
  const paused = active.length > 0 && active.every(record => record.status === "paused");
  const unseen = records.filter(record => snapshot?.unacknowledgedIds.includes(record.id));
  const failed = unseen.filter(record => record.status === "failed").length, complete = unseen.filter(record => record.status === "complete").length;
  let label = active.length ? `Downloads, ${active.length} ${paused ? "paused" : "active"} download${active.length === 1 ? "" : "s"}${percent === null || paused ? "" : `, ${percent}% complete`}` : "Downloads";
  if (failed) label += `, ${failed} failed download${failed === 1 ? "" : "s"} need attention`;
  else if (complete) label += `, ${complete} completed download${complete === 1 ? "" : "s"}`;
  return { active: active.length, paused, percent, attention: failed || complete, label };
}

function statusText(record: OctopusBrowserDownload) {
  if (record.artifactError) return record.artifactError;
  const bytes = record.totalBytes > 0 && record.status !== "complete" ? `${formatBytes(record.receivedBytes)} of ${formatBytes(record.totalBytes)}` : formatBytes(record.receivedBytes);
  if (record.status === "in_progress") {
    const speed = record.receivedBytes / Math.max(1, (record.updatedAtMs - record.startedAtMs) / 1000);
    const seconds = Math.ceil((record.totalBytes - record.receivedBytes) / speed);
    const remaining = record.receivedBytes <= 0 ? "starting" : record.totalBytes <= record.receivedBytes ? "now" : seconds < 60 ? `${seconds}s` : seconds < 3600 ? `${Math.ceil(seconds / 60)}m` : `${Math.ceil(seconds / 3600)}h`;
    return [`${formatBytes(speed)}/s`, bytes, ...(record.totalBytes > 0 ? [remaining] : [])].join(" · ");
  }
  const text = record.status === "complete" ? record.artifactPending ? "Collecting downloaded file" : record.fileExists ? "Downloaded" : "File moved or deleted" : record.status === "started" ? "Starting" : record.status === "paused" ? "Paused" : record.status === "failed" ? "Download failed" : record.status === "canceled" ? "Stopped" : "Native download no longer available";
  const time = activeStatuses.has(record.status) ? [] : [new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" }).format(record.updatedAtMs)];
  return [text, ...(record.status === "canceled" && !record.receivedBytes ? [] : [bytes]), ...time].join(" · ");
}

export function BrowserDownloads({ active, environment, surface, standalone = false, openRequest = 0 }: Props) {
  const [snapshot, setSnapshot] = useState<OctopusBrowserDownloadSnapshot | null>(null), [open, setOpen] = useState(standalone);
  const [preferencesOpen, setPreferencesOpen] = useState(false);
  const [historyMode, setHistoryMode] = useState(standalone), [history, setHistory] = useState<OctopusBrowserDownloadHistory | null>(null);
  const [query, setQuery] = useState(""), deferredQuery = useDeferredValue(query), [historyPending, setHistoryPending] = useState(false);
  const historyEpoch = useRef(0);
  const [error, setError] = useState(""), [pending, setPending] = useState<string | null>(null), [peek, setPeek] = useState<OctopusBrowserDownload | null>(null), [announcement, setAnnouncement] = useState("");
  const root = useRef<HTMLDivElement>(null), trigger = useRef<HTMLButtonElement>(null), acknowledged = useRef("");
  const peekTimer = useRef<ReturnType<typeof setTimeout> | null>(null), peekDeadline = useRef(0), peekRemaining = useRef(5000), paused = useRef(new Set<string>());
  const initial = useRef(false), current = useRef<OctopusBrowserDownloadSnapshot | null>(null);
  const hostGeneration = String(surface.data.host_generation ?? "");
  const ownerKey = JSON.stringify([environment.id, surface.id, hostGeneration]), owner = useRef(ownerKey);
  owner.current = ownerKey;
  const sourceSequence = (surface.data.downloads as { sequence?: number } | null)?.sequence ?? 0;
  const request = (action: OctopusBrowserDownloadRequest["action"], id?: string, promptForLocation?: boolean, historyQuery?: { text: string; offset: number }) => {
    if (!window.octopusTransport?.nativeBrowserDownloads) return Promise.reject(new Error("Native download history is unavailable."));
    return window.octopusTransport.nativeBrowserDownloads({ workerEnvironmentId: environment.id, surfaceId: surface.id, hostGeneration, action, ...(id === undefined ? {} : { id }), ...(promptForLocation === undefined ? {} : { promptForLocation }), ...(historyQuery === undefined ? {} : { query: historyQuery }) });
  };
  const accept = (next: OctopusBrowserDownloadSnapshot) => {
    if (!Array.isArray(next.downloads) || !Array.isArray(next.unacknowledgedIds) || !Number.isSafeInteger(next.sequence) || !next.preferences || typeof next.preferences.promptForLocation !== "boolean" || (next.preferences.directory !== null && typeof next.preferences.directory !== "string")) throw new Error("Invalid native download history.");
    if (current.current && next.sequence < current.current.sequence) return;
    const changed = next.downloads.filter(record => current.current && current.current.downloads.find(previous => previous.id === record.id)?.status !== record.status).sort((a, b) => b.sequence - a.sequence)[0];
    if (changed) {
      const previous = current.current?.downloads.find(record => record.id === changed.id);
      const text = changed.status === "started" ? `Downloading ${changed.filename}` : changed.status === "in_progress" ? previous?.status === "paused" ? `${changed.filename} download resumed` : ""
        : changed.status === "paused" ? `${changed.filename} download paused` : changed.status === "failed" ? `${changed.filename} failed to download`
          : changed.status === "canceled" ? `${changed.filename} download canceled` : changed.status === "complete" ? `${changed.filename} downloaded` : "";
      if (text) setAnnouncement(text);
    }
    current.current = next; setSnapshot(next);
  };
  const dismissPeek = () => { if (peekTimer.current !== null) clearTimeout(peekTimer.current); peekTimer.current = null; setPeek(null); };
  useEffect(() => {
    current.current = null; initial.current = false; acknowledged.current = ""; setSnapshot(null); setError(""); setAnnouncement(""); dismissPeek();
    ++historyEpoch.current; setHistory(null); setQuery(""); setHistoryMode(standalone); setHistoryPending(false); setPending(null);
  }, [environment.id, surface.id, hostGeneration]);
  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    void request("snapshot").then(next => {
      if (cancelled) return;
      accept(next);
      const latest = [...next.downloads].sort((a, b) => b.sequence - a.sequence)[0];
      const remembered = peekHistory.get(hostGeneration) ?? { sequence: 0, availableAt: 0 };
      if (initial.current && latest && latest.sequence > remembered.sequence && latest.userInitiated && ["complete", "failed"].includes(latest.status)
        && latest.targetId === surface.data.target_id && !standalone && !open && Date.now() >= remembered.availableAt && Date.now() < latest.updatedAtMs + 5000) {
        peekHistory.set(hostGeneration, { sequence: latest.sequence, availableAt: latest.updatedAtMs + 15000 }); paused.current.clear(); peekRemaining.current = latest.updatedAtMs + 5000 - Date.now(); setPeek(latest);
      }
      initial.current = true;
    }).catch(reason => { if (!cancelled) setError(String(reason?.message ?? reason)); });
    return () => { cancelled = true; };
  }, [active, environment.id, surface.id, hostGeneration, sourceSequence]);
  const checkedHistory = (next: OctopusBrowserDownloadSnapshot, text: string, offset: number) => {
    const page = next.history;
    if (!page || page.text !== text || page.offset !== offset || typeof page.hasMore !== "boolean" || !Array.isArray(page.downloads)
      || page.downloads.length > 100 || new Set(page.downloads.map(record => record.id)).size !== page.downloads.length) throw new Error("Invalid native download history page.");
    return page;
  };
  useEffect(() => {
    const epoch = ++historyEpoch.current;
    if (!active || !open || !historyMode) { setHistoryPending(false); return; }
    setHistoryPending(true);
    void request("search", undefined, undefined, { text: deferredQuery, offset: 0 }).then(next => {
      if (historyEpoch.current !== epoch || owner.current !== ownerKey) return;
      const page = checkedHistory(next, deferredQuery, 0); accept(next); setHistory(page);
    }).catch(reason => { if (historyEpoch.current === epoch && owner.current === ownerKey) setError(String(reason?.message ?? reason)); })
      .finally(() => { if (historyEpoch.current === epoch && owner.current === ownerKey) setHistoryPending(false); });
    return () => { if (historyEpoch.current === epoch) ++historyEpoch.current; };
  }, [active, open, historyMode, deferredQuery, ownerKey, sourceSequence, snapshot?.sequence]);
  const loadMore = async () => {
    if (!history || historyPending || !history.hasMore || history.text !== deferredQuery || query !== deferredQuery) return;
    const epoch = ++historyEpoch.current, offset = history.downloads.length;
    setHistoryPending(true);
    try {
      const next = await request("search", undefined, undefined, { text: deferredQuery, offset });
      if (historyEpoch.current !== epoch || owner.current !== ownerKey) return;
      const page = checkedHistory(next, deferredQuery, offset);
      if (current.current && next.sequence !== current.current.sequence) { accept(next); return; }
      const ids = new Set(history.downloads.map(record => record.id));
      if (page.downloads.some(record => ids.has(record.id))) throw new Error("Download history changed while loading this page.");
      accept(next); setHistory({ ...page, offset: 0, downloads: [...history.downloads, ...page.downloads] });
    } catch (reason) { if (historyEpoch.current === epoch && owner.current === ownerKey) setError(String((reason as Error)?.message ?? reason)); }
    finally { if (historyEpoch.current === epoch && owner.current === ownerKey) setHistoryPending(false); }
  };
  useEffect(() => { if (openRequest) setOpen(true); }, [openRequest]);
  useEffect(() => {
    if (!open || !active || !snapshot?.unacknowledgedIds.length) return;
    dismissPeek(); const key = JSON.stringify(snapshot.unacknowledgedIds);
    if (acknowledged.current === key) return;
    acknowledged.current = key;
    let cancelled = false;
    void request("acknowledge").then(next => { if (!cancelled && owner.current === ownerKey) accept(next); })
      .catch(reason => { if (!cancelled && owner.current === ownerKey) setError(String(reason?.message ?? reason)); });
    return () => { cancelled = true; };
  }, [active, open, ownerKey, snapshot?.unacknowledgedIds.join("|")]);
  useEffect(() => {
    if (!peek || open) { if (peekTimer.current !== null) clearTimeout(peekTimer.current); peekTimer.current = null; return; }
    peekDeadline.current = Date.now() + peekRemaining.current;
    peekTimer.current = setTimeout(dismissPeek, peekRemaining.current);
    return () => { if (peekTimer.current !== null) clearTimeout(peekTimer.current); peekTimer.current = null; };
  }, [peek, open]);
  useEffect(() => {
    if (active || !peek) return;
    paused.current.clear();
    if (peekTimer.current === null) { peekDeadline.current = Date.now() + peekRemaining.current; peekTimer.current = setTimeout(dismissPeek, peekRemaining.current); }
  }, [active, peek]);
  useEffect(() => {
    if (!open || standalone) return;
    const close = (event: globalThis.PointerEvent) => { if (!root.current?.contains(event.target as Node)) setOpen(false); };
    document.addEventListener("pointerdown", close); return () => document.removeEventListener("pointerdown", close);
  }, [open, standalone]);
  const holdPeek = (reason: string) => { paused.current.add(reason); if (peekTimer.current !== null) { clearTimeout(peekTimer.current); peekTimer.current = null; peekRemaining.current = Math.max(0, peekDeadline.current - Date.now()); } };
  const resumePeek = (reason: string) => { paused.current.delete(reason); if (!paused.current.size && peek && peekTimer.current === null) { peekDeadline.current = Date.now() + peekRemaining.current; peekTimer.current = setTimeout(dismissPeek, peekRemaining.current); } };
  const perform = async (action: OctopusBrowserDownloadRequest["action"], id?: string, promptForLocation?: boolean) => {
    setPending(id ?? action); setError("");
    try { const next = await request(action, id, promptForLocation); if (owner.current !== ownerKey) return; accept(next); if (action === "open" || action === "show-in-folder") dismissPeek(); }
    catch (reason) { if (owner.current === ownerKey) setError(String((reason as Error)?.message ?? reason)); }
    finally { if (owner.current === ownerKey) setPending(null); }
  };
  const menu = async (event: MouseEvent, record: OctopusBrowserDownload) => {
    event.preventDefault(); event.stopPropagation();
    const actions: OctopusClientContextMenuItem[] = [];
    if (record.status === "complete") actions.push({ id: "open", label: "Open", enabled: record.fileExists });
    actions.push({ id: "show-in-folder", label: "Show in Finder", enabled: record.fileExists });
    if (activeStatuses.has(record.status)) actions.push({ id: "cancel", label: "Stop", enabled: record.canCancel });
    actions.push({ id: "copy-address", label: "Copy address" }, { id: "copy-path", label: "Copy path" });
    if (!activeStatuses.has(record.status)) actions.push({ type: "separator" }, { id: "remove", label: "Remove from list" });
    if (!window.octopusTransport?.showClientContextMenu) { setError("Native download actions are unavailable."); return; }
    try {
      const result = await window.octopusTransport.showClientContextMenu({ requestId: crypto.randomUUID(), items: actions, position: { x: event.clientX, y: event.clientY } });
      if (owner.current === ownerKey && result.id && actions.some(item => "id" in item && item.id === result.id)) await perform(result.id as OctopusBrowserDownloadRequest["action"], record.id);
    } catch (reason) { if (owner.current === ownerKey) setError(String((reason as Error)?.message ?? reason)); }
  };
  const indicator = browserDownloadIndicator(snapshot);
  const records = historyMode ? history?.downloads ?? [] : [...(snapshot?.downloads ?? []).filter(record => activeStatuses.has(record.status)), ...(snapshot?.downloads ?? []).filter(record => !activeStatuses.has(record.status))];
  if (!standalone && !open && !peek && !records.length && !error) return null;
  const content = <>
    <header><span>Downloads</span><button aria-label="Download settings" aria-expanded={preferencesOpen} onClick={() => setPreferencesOpen(value => !value)} type="button"><Glyph name="settings" size={16} /></button><button aria-label="Open downloads folder" disabled={pending === "open-folder"} onClick={() => { void perform("open-folder"); }} type="button"><Glyph name="folder" size={16} /></button></header>
    {historyMode ? <div className="flex flex-col gap-2 border-b border-token-border px-4 py-3">
      <label className="flex flex-col gap-1 text-sm"><span>Search download history</span><input type="search" value={query} placeholder="Filename or address" onChange={event => { ++historyEpoch.current; setHistory(null); setHistoryPending(true); setQuery(event.currentTarget.value); }} className="rounded border border-token-border bg-token-surface px-2 py-1" /></label>
      <div className="flex items-center justify-between gap-2">{!standalone ? <OcButton size="sm" onClick={() => { setHistoryMode(false); setQuery(""); }}>Recent downloads</OcButton> : <span />}
        <OcButton size="sm" disabled={pending !== null || !records.some(record => !activeStatuses.has(record.status))} onClick={() => { void perform("clear-history"); }}>Clear history</OcButton></div>
    </div> : <div className="border-b border-token-border px-4 py-2"><OcButton size="sm" onClick={() => { setHistoryMode(true); }}>View all downloads</OcButton></div>}
    {preferencesOpen && snapshot ? <div className="flex flex-col gap-3 border-b border-token-border px-4 py-3 text-sm">
      <div className="flex items-center gap-2"><span className="min-w-0 flex-1 break-all">{snapshot.preferences.directory ?? "System Downloads folder"}</span><OcButton size="sm" disabled={pending !== null} onClick={() => { void perform("choose-directory"); }}>Change</OcButton>
        {snapshot.preferences.directory !== null ? <OcButton size="sm" disabled={pending !== null} onClick={() => { void perform("reset-directory"); }}>Reset</OcButton> : null}</div>
      <label className="flex items-center gap-2"><input type="checkbox" checked={snapshot.preferences.promptForLocation} disabled={pending !== null} onChange={event => { void perform("set-prompt", undefined, event.currentTarget.checked); }} />Ask where to save each file</label>
    </div> : null}
    {error || snapshot?.error ? <div className="we-download-error" role="alert">{error || snapshot?.error}</div> : null}
    <div className="we-download-list">
      {snapshot === null || historyMode && history === null ? <div className="we-download-empty">{historyPending || snapshot === null ? "Loading downloads" : "Download history is unavailable"}</div> : !records.length ? <div className="we-download-empty"><Glyph name="download" size={24} /><strong>{historyMode && query ? "No matching downloads" : "No downloads yet"}</strong><span>{historyMode && query ? "Try another filename or address" : "Files downloaded from the built-in browser will appear here"}</span></div> : records.map(record => {
        const busy = pending === record.id, progress = record.totalBytes > 0 ? Math.min(1, record.receivedBytes / record.totalBytes) : null;
        const body = <><span className="we-download-file"><Glyph name="file" size={16} /></span><span className="we-download-copy"><span>{record.filename}</span><small className={record.status === "failed" ? "is-failed" : ""}>{statusText(record)}</small>{activeStatuses.has(record.status) ? <span className="we-download-progress"><i className={`${record.status === "paused" ? "is-paused" : ""}${progress === null ? " is-indeterminate" : ""}`} style={progress === null ? undefined : { transform: `scaleX(${Math.max(0.03, progress)})`, opacity: progress === 0 ? 0 : 1 }} /></span> : null}</span></>;
        return <div className="we-download-row" key={record.id} onContextMenu={event => { void menu(event, record); }}>
          {record.status === "complete" && record.fileExists ? <button className="we-download-main" aria-label={`Open ${record.filename}`} disabled={busy} onClick={event => { if (event.detail <= 1) void perform("open", record.id); }} type="button">{body}</button> : <div className="we-download-main">{body}</div>}
          <div className="we-download-row-actions">
            {record.canResume || record.canPause ? <button aria-label={record.canResume ? "Resume download" : "Pause download"} disabled={busy} onClick={() => { void perform(record.canResume ? "resume" : "pause", record.id); }} type="button"><Glyph name={record.canResume ? "play" : "pause"} size={14} /></button> : null}
            {record.canCancel ? <button aria-label="Stop downloading" disabled={busy} onClick={() => { void perform("cancel", record.id); }} type="button"><Glyph name="close" size={14} /></button> : <button aria-label={`Download actions for ${record.filename}`} disabled={busy} onClick={event => { void menu(event, record); }} type="button"><Glyph name="more" size={14} /></button>}
          </div>
        </div>;
      })}
      {historyMode && history?.hasMore ? <div className="px-4 py-3"><OcButton size="sm" disabled={historyPending || query !== deferredQuery} onClick={() => { void loadMore(); }}>{historyPending ? "Loading" : "Load more"}</OcButton></div> : null}
    </div>
  </>;
  return <div className={`we-downloads${standalone ? " is-standalone" : ""}`} ref={root}>
    {!standalone ? <button aria-label={indicator.label} aria-expanded={open} className={indicator.active || indicator.attention ? "has-download-activity" : ""} ref={trigger} onClick={() => { dismissPeek(); setOpen(!open); }} type="button">
      <Glyph name="download" size={18} />{indicator.active ? <svg className={`we-download-ring${indicator.paused ? " is-paused" : ""}${indicator.percent === null && !indicator.paused ? " is-indeterminate" : ""}`} viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10" pathLength="100" strokeDasharray={`${indicator.percent ?? 25} 100`} /></svg> : null}
    </button> : null}
    {standalone ? <section className="we-download-panel" aria-label="Downloads">{content}</section> : open ? <OcMenuSurface className="we-download-panel" aria-label="Downloads" role="dialog" onKeyDown={event => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); setOpen(false); trigger.current?.focus(); } }}>{content}</OcMenuSurface> : null}
    {peek && active && !open ? <OcMenuSurface role="status" className="we-download-peek" aria-label={`Download ${peek.status === "complete" ? "complete" : "failed"}: ${peek.filename}`} onPointerEnter={() => holdPeek("pointer")} onPointerLeave={() => resumePeek("pointer")} onFocus={() => holdPeek("focus")} onBlur={event => { if (!event.currentTarget.contains(event.relatedTarget as Node)) resumePeek("focus"); }}>
      <header><strong>Download {peek.status === "complete" ? "complete" : "failed"}</strong><button aria-label="Dismiss download" onClick={dismissPeek} type="button"><Glyph name="close" size={14} /></button></header><p>{peek.filename}</p>
      <div>{peek.status === "complete" && peek.fileExists ? <><OcButton size="sm" disabled={pending !== null} onClick={() => { void perform("show-in-folder", peek.id); }}>Show in folder</OcButton><OcButton size="sm" disabled={pending !== null} onClick={() => { void perform("open", peek.id); }}>Open</OcButton></> : <OcButton size="sm" onClick={() => { dismissPeek(); setOpen(true); }}>View downloads</OcButton>}</div>
    </OcMenuSurface> : null}
    <span className="we-download-announcement" aria-live="polite" aria-atomic="true">{active ? announcement : ""}</span>
  </div>;
}
