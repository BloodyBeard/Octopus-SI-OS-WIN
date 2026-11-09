import { useEffect, useRef, useState } from "react";
import type { ClientBrowserRequest, ClientBrowserState } from "../../clientBrowser";
import { OcIconButton } from "../../design-system/components";
import { appToast } from "../../toasts";

export function ClientBrowserTab({ id, active }: { id: string; active: boolean }) {
  const [state, setState] = useState<ClientBrowserState | null>(null);
  const [address, setAddress] = useState("");
  const viewport = useRef<HTMLDivElement>(null);
  const action = async (request: ClientBrowserRequest) => {
    try {
      if (!window.octopusTransport?.clientBrowser) throw new Error("The in-app browser is unavailable.");
      const next = await window.octopusTransport.clientBrowser(request);
      if (next) setState(next);
    } catch (error) { appToast.danger(error instanceof Error ? error.message : String(error)); }
  };
  useEffect(() => {
    let disposed = false;
    const update = (next: ClientBrowserState) => { if (!disposed && next.id === id) setState(next); };
    const stop = window.octopusTransport?.onClientBrowserState(update);
    void window.octopusTransport?.clientBrowser({ action: "state", id }).then(next => { if (next) update(next); }).catch(error => appToast.danger(String(error)));
    return () => { disposed = true; stop?.(); };
  }, [id]);
  useEffect(() => { if (state) setAddress(state.url); }, [state?.url]);
  useEffect(() => {
    const element = viewport.current, transport = window.octopusTransport;
    if (!element || !transport) return;
    let stopped = false, frame = 0, last = "";
    const sync = () => {
      frame = 0;
      if (stopped) return;
      const rect = element.getBoundingClientRect();
      const covered = Array.from(document.querySelectorAll<HTMLElement>('[role="dialog"], [role="menu"], [role="listbox"], [data-radix-popper-content-wrapper]')).some(overlay => {
        if (overlay.contains(element) || overlay.closest('[hidden], [inert], [aria-hidden="true"]')) return false;
        const box = overlay.getBoundingClientRect(), style = getComputedStyle(overlay);
        return style.display !== "none" && style.visibility !== "hidden" && style.opacity !== "0" && box.width > 0 && box.height > 0
          && box.left < rect.right && box.right > rect.left && box.top < rect.bottom && box.bottom > rect.top;
      });
      const request: ClientBrowserRequest = { action: "layout", id,
        visible: active && !document.hidden && rect.width > 0 && rect.height > 0 && !covered && !state?.error,
        bounds: { x: rect.x, y: rect.y, width: rect.width, height: rect.height } };
      const key = JSON.stringify(request);
      if (key === last) return;
      last = key;
      void transport.clientBrowser(request).catch(error => { if (!stopped) appToast.danger(String(error)); });
    };
    const schedule = () => { if (!frame) frame = requestAnimationFrame(sync); };
    const resize = new ResizeObserver(schedule), changes = new MutationObserver(schedule);
    for (let parent: HTMLElement | null = element; parent; parent = parent.parentElement) resize.observe(parent);
    changes.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["style", "class", "hidden", "inert", "aria-hidden"] });
    window.addEventListener("resize", schedule); window.addEventListener("scroll", schedule, true); document.addEventListener("visibilitychange", schedule);
    schedule();
    return () => {
      stopped = true; cancelAnimationFrame(frame); resize.disconnect(); changes.disconnect();
      window.removeEventListener("resize", schedule); window.removeEventListener("scroll", schedule, true); document.removeEventListener("visibilitychange", schedule);
      void transport.clientBrowser({ action: "layout", id, visible: false }).catch(() => {});
    };
  }, [active, id, state?.error]);
  return <div className="flex h-full min-h-0 flex-col" style={active ? undefined : { display: "none" }}>
    <form className="flex shrink-0 items-center gap-1 border-b border-token-border p-2" onSubmit={event => {
      event.preventDefault(); void action({ action: "navigate", id, url: address });
    }}>
      <OcIconButton glyph="arrowLeft" label="Back" disabled={!state?.canGoBack} onClick={() => void action({ action: "back", id })} />
      <OcIconButton className="rotate-180" glyph="arrowLeft" label="Forward" disabled={!state?.canGoForward} onClick={() => void action({ action: "forward", id })} />
      <OcIconButton glyph={state?.loading ? "close" : "refresh"} label={state?.loading ? "Stop loading" : "Reload"} onClick={() => void action({ action: state?.loading ? "stop" : "reload", id })} />
      <input aria-label="Web address" className="min-w-0 flex-1 rounded-lg border border-token-border bg-transparent px-3 py-1.5 text-sm" value={address} onChange={event => setAddress(event.target.value)} />
      <OcIconButton glyph="openExternal" label="Open in default browser" onClick={() => { if (state?.url) window.open(state.url, "_blank", "noopener,noreferrer"); }} />
    </form>
    <div className="relative min-h-0 flex-1" ref={viewport} aria-label="Browser page">
      {state?.error ? <p role="alert" className="p-4 text-sm">{state.error}</p> : null}
    </div>
  </div>;
}
