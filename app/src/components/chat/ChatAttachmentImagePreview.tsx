import { useEffect, useLayoutEffect, useRef, useState, type PointerEvent } from "react";
import { createPortal, flushSync } from "react-dom";
import { containModalFocus, isolateModalLayer, isTopmostModal, restoreModalFocus } from "../../design-system/modalFocus";
import { appToast } from "../../toasts";
import { safeArtifactFilename } from "../artifacts/artifactFileOperations";
import { AttachmentIcon } from "./AttachmentIcon";

type Point = { clientX: number; clientY: number };
type Size = { width: number; height: number };
const zoomSteps = [25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500];

export function ChatAttachmentImagePreview({ blob, filename, src, onClose, onError, onPreviousImage, onNextImage }: {
  blob: Blob; filename: string; src: string; onClose: () => void; onError: () => void;
  onPreviousImage?: () => void; onNextImage?: () => void;
}) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const viewportRef = useRef<HTMLDivElement>(null);
  const imageRef = useRef<HTMLImageElement>(null);
  const live = useRef(true);
  const pointers = useRef(new Map<number, Point>());
  const pointer = useRef<Point | null>(null);
  const pan = useRef<(Point & { id: number; left: number; top: number }) | null>(null);
  const pinch = useRef<{ distance: number; zoom: number } | null>(null);
  const [viewportSize, setViewportSize] = useState<Size | null>(null);
  const [naturalSize, setNaturalSize] = useState<Size | null>(null);
  const [zoom, setZoom] = useState<number | null>(null);
  const fit = viewportSize && naturalSize ? Math.min(1, viewportSize.width / naturalSize.width, viewportSize.height / naturalSize.height) * 100 : null;
  const currentZoom = zoom ?? (fit && Number.isFinite(fit) ? fit : null);
  const ramp = [...zoomSteps, ...(fit && !zoomSteps.includes(fit) ? [fit] : [])].sort((left, right) => left - right);
  const minimum = ramp[0], maximum = ramp.at(-1)!;
  const clamp = (value: number) => Math.min(maximum, Math.max(minimum, value));

  function measure() {
    const rect = viewportRef.current?.getBoundingClientRect();
    if (rect) setViewportSize((previous) => previous?.width === rect.width && previous.height === rect.height ? previous : { width: rect.width, height: rect.height });
    const image = imageRef.current;
    if (image?.naturalWidth && image.naturalHeight) setNaturalSize((previous) => previous?.width === image.naturalWidth && previous.height === image.naturalHeight
      ? previous : { width: image.naturalWidth, height: image.naturalHeight });
  }

  useLayoutEffect(() => {
    const dialog = dialogRef.current!;
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const release = isolateModalLayer(dialog);
    live.current = true;
    dialog.focus({ preventScroll: true });
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(viewportRef.current!);
    return () => {
      live.current = false;
      observer.disconnect();
      release();
      restoreModalFocus(previousFocus);
    };
  }, []);

  function zoomAt(value: number, point?: Point) {
    const viewport = viewportRef.current, image = imageRef.current;
    if (!viewport || !image || !Number.isFinite(value)) return;
    const viewportRect = viewport.getBoundingClientRect();
    const anchor = point ?? { clientX: viewportRect.left + viewportRect.width / 2, clientY: viewportRect.top + viewportRect.height / 2 };
    const before = image.getBoundingClientRect();
    const x = before.width > 0 ? Math.min(1, Math.max(0, (anchor.clientX - before.left) / before.width)) : 0.5;
    const y = before.height > 0 ? Math.min(1, Math.max(0, (anchor.clientY - before.top) / before.height)) : 0.5;
    flushSync(() => setZoom(clamp(value)));
    if (!viewport.isConnected || !image.isConnected) return;
    const after = image.getBoundingClientRect();
    viewport.scrollLeft += after.left + after.width * x - anchor.clientX;
    viewport.scrollTop += after.top + after.height * y - anchor.clientY;
  }

  function step(direction: "in" | "out") {
    if (currentZoom == null) return;
    const next = direction === "in" ? ramp.find((value) => value > currentZoom) : [...ramp].reverse().find((value) => value < currentZoom);
    if (next != null) zoomAt(next);
  }

  useEffect(() => {
    const viewport = viewportRef.current!;
    const wheel = (event: WheelEvent) => {
      if (!event.ctrlKey || currentZoom == null) return;
      event.preventDefault();
      event.stopPropagation();
      const delta = event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? 800 : 1);
      const rect = viewport.getBoundingClientRect();
      const inside = (point: Point) => point.clientX >= rect.left && point.clientX <= rect.right && point.clientY >= rect.top && point.clientY <= rect.bottom;
      const point = (event.clientX || event.clientY) && inside(event) ? event : pointer.current && inside(pointer.current) ? pointer.current : undefined;
      zoomAt(currentZoom * Math.exp(-delta / 200), point);
    };
    viewport.addEventListener("wheel", wheel, { passive: false });
    return () => viewport.removeEventListener("wheel", wheel);
  });

  function pair() {
    const [first, second] = pointers.current.values();
    if (!first || !second) return null;
    const distance = Math.hypot(first.clientX - second.clientX, first.clientY - second.clientY);
    return distance > 0 ? { distance, clientX: (first.clientX + second.clientX) / 2, clientY: (first.clientY + second.clientY) / 2 } : null;
  }

  function pointerDown(event: PointerEvent<HTMLDivElement>) {
    pointer.current = { clientX: event.clientX, clientY: event.clientY };
    if (event.pointerType !== "touch") return;
    event.currentTarget.setPointerCapture(event.pointerId);
    pointers.current.set(event.pointerId, pointer.current);
    if (pointers.current.size === 1) {
      pan.current = { ...pointer.current, id: event.pointerId, left: event.currentTarget.scrollLeft, top: event.currentTarget.scrollTop };
      return;
    }
    pan.current = null;
    const points = pair();
    pinch.current = points && currentZoom ? { distance: points.distance, zoom: currentZoom } : null;
  }

  function pointerMove(event: PointerEvent<HTMLDivElement>) {
    pointer.current = { clientX: event.clientX, clientY: event.clientY };
    if (event.pointerType !== "touch" || !pointers.current.has(event.pointerId)) return;
    pointers.current.set(event.pointerId, pointer.current);
    if (pointers.current.size > 1) {
      event.preventDefault(); event.stopPropagation(); pan.current = null;
      const points = pair();
      if (pinch.current && points) zoomAt(pinch.current.zoom * points.distance / pinch.current.distance, points);
    } else if (pan.current?.id === event.pointerId) {
      event.preventDefault(); event.stopPropagation();
      event.currentTarget.scrollLeft = pan.current.left - (event.clientX - pan.current.clientX);
      event.currentTarget.scrollTop = pan.current.top - (event.clientY - pan.current.clientY);
    }
  }

  function pointerUp(event: PointerEvent<HTMLDivElement>) {
    if (event.pointerType !== "touch") return;
    pointers.current.delete(event.pointerId);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
    const remaining = pointers.current.entries().next().value;
    const points = pair();
    pan.current = pointers.current.size === 1 && remaining ? { ...remaining[1], id: remaining[0], left: event.currentTarget.scrollLeft, top: event.currentTarget.scrollTop } : null;
    pinch.current = points && currentZoom ? { distance: points.distance, zoom: currentZoom } : null;
  }

  async function download() {
    try {
      const transport = window.octopusTransport;
      if (!transport?.saveClientFile) throw new Error("Native file saving is unavailable.");
      const bytes = new Uint8Array(await blob.arrayBuffer());
      if (live.current) await transport.saveClientFile({ bytes, filename: safeArtifactFilename(filename) });
    } catch (error: unknown) {
      if (live.current) appToast.danger(error instanceof Error ? error.message : "The image could not be saved.");
    }
  }

  return createPortal(<div aria-label="Image preview" aria-modal="true" className="chat-attachment-lightbox" data-oc-dialog ref={dialogRef} role="dialog" tabIndex={-1}
    onKeyDown={(event) => {
      containModalFocus(event, dialogRef.current);
      if (event.defaultPrevented || !isTopmostModal(dialogRef.current)) return;
      if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); onClose(); }
      else if (event.key === "ArrowLeft" && onPreviousImage) { event.preventDefault(); event.stopPropagation(); onPreviousImage(); }
      else if (event.key === "ArrowRight" && onNextImage) { event.preventDefault(); event.stopPropagation(); onNextImage(); }
      else if (event.metaKey || event.ctrlKey) {
        if (event.key === "+" || event.key === "=") { event.preventDefault(); event.stopPropagation(); step("in"); }
        else if (event.key === "-") { event.preventDefault(); event.stopPropagation(); step("out"); }
        else if (event.key === "0") {
          event.preventDefault(); event.stopPropagation(); setZoom(null);
          if (viewportRef.current) { viewportRef.current.scrollLeft = 0; viewportRef.current.scrollTop = 0; }
        }
      }
    }}>
    <div className={`chat-attachment-lightbox-content${onPreviousImage || onNextImage ? " has-gallery" : ""}`}
      onPointerDown={(event) => { if (event.currentTarget === event.target) onClose(); }}>
      <div className="chat-attachment-lightbox-toolbar">
        <button aria-label="Download image" className="chat-attachment-lightbox-action" onClick={() => { void download(); }} type="button"><AttachmentIcon name="download" size={16} /></button>
        <button aria-label="Close image preview" className="chat-attachment-lightbox-action" onClick={onClose} type="button"><AttachmentIcon name="close" size={18} /></button>
      </div>
      {onPreviousImage ? <button aria-label="Previous image" className="chat-attachment-lightbox-action is-previous" onClick={onPreviousImage} type="button"><AttachmentIcon name="previous" size={18} /></button> : null}
      {onNextImage ? <button aria-label="Next image" className="chat-attachment-lightbox-action is-next" onClick={onNextImage} type="button"><AttachmentIcon name="previous" size={18} /></button> : null}
      <div className="chat-attachment-lightbox-viewport" onPointerCancelCapture={pointerUp} onPointerDownCapture={pointerDown} onPointerMoveCapture={pointerMove} onPointerUpCapture={pointerUp} ref={viewportRef}
        onPointerDown={(event) => { if (event.currentTarget === event.target) onClose(); }}>
        <h2 className="sr-only">Image preview</h2>
        <img alt={filename} className={currentZoom && naturalSize ? "is-sized" : undefined} draggable={false} onError={onError} onLoad={measure}
          onPointerDown={(event) => event.stopPropagation()} ref={imageRef} referrerPolicy="no-referrer" src={src}
          style={currentZoom && naturalSize ? { width: naturalSize.width * currentZoom / 100, height: naturalSize.height * currentZoom / 100 } : undefined} />
      </div>
      <div className="chat-attachment-lightbox-bottom">
        <div className="chat-attachment-lightbox-zoom">
          <button aria-label="Zoom out image" disabled={currentZoom == null || currentZoom <= minimum} onClick={() => step("out")} type="button"><AttachmentIcon name="minus" size={16} /></button>
          <span>{currentZoom == null ? "" : `${Math.round(currentZoom)}%`}</span>
          <button aria-label="Zoom in image" disabled={currentZoom == null || currentZoom >= maximum} onClick={() => step("in")} type="button"><AttachmentIcon name="plus" size={16} /></button>
        </div>
      </div>
    </div>
  </div>, document.body);
}
