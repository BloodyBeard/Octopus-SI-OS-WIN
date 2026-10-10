const DETACH_DISTANCE = 15;
const CURSOR_SAMPLE_MS = 8;
const PREVIEW_COLOR = /^(#[0-9a-f]{3,8}|(rgb|rgba|hsl|hsla|oklch|oklab|lab|lch|color)\([0-9a-z.,%/ +-]{1,120}\))$/i;
function rect(value, label, maximum = 20000) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).some((key) => !["x", "y", "width", "height"].includes(key))
    || ![value.x, value.y, value.width, value.height].every(Number.isFinite)
    || value.width <= 0 || value.height <= 0 || value.width > maximum || value.height > maximum) {
    throw new Error(`${label} is invalid.`);
  }
  return { x: value.x, y: value.y, width: value.width, height: value.height };
}
function contains(bounds, point) {
  return point.x >= bounds.x && point.y >= bounds.y && point.x < bounds.x + bounds.width && point.y < bounds.y + bounds.height;
}
function expand(bounds, margin) {
  return { x: bounds.x - margin, y: bounds.y - margin, width: bounds.width + margin * 2, height: bounds.height + margin * 2 };
}
function appearance(value) {
  if (value === undefined) return { radius: 0, background: "" };
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).some((key) => !["radius", "background"].includes(key))) {
    throw new Error("Pane preview appearance is invalid.");
  }
  return {
    radius: Number.isFinite(value.radius) ? Math.max(0, Math.min(64, value.radius)) : 0,
    background: typeof value.background === "string" && PREVIEW_COLOR.test(value.background) ? value.background : "",
  };
}
function nativePaneTransferBlockReason(target) {
  if (target.kind === "worker_environment" && (!target.target.panelKind || target.target.panelKind === "browser")) {
    return "Live browser adoption is unavailable for this viewer. The source browser remains in its window.";
  }
  return "";
}
function transferReference(value) {
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).length !== 1 || typeof value.transferId !== "string") throw new Error("Pane drag reference is invalid.");
  return value.transferId;
}
function createPaneWindowDragHost({ registry, cursor, capturePreview, openPreview, createDestination, positionDestination, discardDestination, notifyDestination, notifySource, reportError }) {
  const drags = new Map();
  const dropZones = new Map();
  function sourceEntry(ownerId, transferId) {
    const transfer = registry.paneTransferForSource(ownerId, { transferId });
    return { transfer, source: registry.entryForWebContents(ownerId) };
  }
  function registerDropZone(ownerId, value) {
    const entry = registry.entryForWebContents(ownerId);
    if (!entry?.accessToken) throw new Error("Pane drop zone requires an authenticated client window.");
    if (value === null) dropZones.delete(ownerId);
    else dropZones.set(ownerId, rect(value, "Pane drop zone"));
  }
  function screenRect(entry, bounds) {
    const content = entry.win.getContentBounds();
    const zoom = entry.win.webContents.getZoomFactor();
    return { x: content.x + bounds.x * zoom, y: content.y + bounds.y * zoom, width: bounds.width * zoom, height: bounds.height * zoom };
  }
  function destinationAt(source, point) {
    if (contains(source.win.getBounds(), point)) return null;
    return registry.entries().filter((entry) => entry !== source && !entry.win.isMinimized() && entry.win.isVisible())
      .sort((a, b) => b.focusRevision - a.focusRevision)
      .find((entry) => contains(entry.win.getBounds(), point)) ?? null;
  }
  function live(drag) {
    return drags.get(drag.transferId) === drag;
  }
  function grabOrigin(drag, point) {
    return { x: point.x - drag.offset.x * drag.zoom, y: point.y - drag.offset.y * drag.zoom };
  }
  function presentPreview(drag) {
    const preview = drag.preview;
    if (!preview) return;
    if (drag.phase !== "floating") {
      if (drag.previewShown) { preview.hide(); drag.previewShown = false; }
      return;
    }
    const origin = grabOrigin(drag, drag.point);
    const x = Math.round(origin.x), y = Math.round(origin.y);
    if (!drag.previewShown || drag.previewAt?.x !== x || drag.previewAt?.y !== y) {
      preview.setPosition(x, y);
      drag.previewAt = { x, y };
    }
    if (!drag.previewShown) { preview.showInactive(); drag.previewShown = true; }
  }
  function report(drag) {
    const visible = drag.phase === "floating" && drag.previewShown ? "floating" : "prepared";
    if (drag.reported === visible) return;
    drag.reported = visible;
    notifySource(drag.ownerId, { transferId: drag.transferId, phase: visible });
  }
  function placeDestination(drag, entry, contentOrigin) {
    if (!positionDestination || entry.win.isDestroyed()) return;
    positionDestination(entry, contentOrigin, drag.point);
  }
  function estimatedOrigin(drag) {
    const grab = grabOrigin(drag, drag.point);
    return { x: grab.x - drag.captureRect.x * drag.zoom, y: grab.y - drag.captureRect.y * drag.zoom };
  }
  function schedulePrewarm(drag) {
    if (drag.prewarm || drag.prewarmTimer || drag.prewarmTried) return;
    drag.prewarmTimer = setTimeout(() => {
      drag.prewarmTimer = null;
      if (!live(drag) || drag.phase !== "floating" || drag.prewarm) return;
      drag.prewarmTried = true;
      try {
        drag.prewarm = createDestination(drag.source, drag.point);
        placeDestination(drag, drag.prewarm, estimatedOrigin(drag));
      } catch (error) { reportError?.(error); }
    }, 0);
    drag.prewarmTimer.unref?.();
  }
  function takePrewarm(drag) {
    clearTimeout(drag.prewarmTimer);
    drag.prewarmTimer = null;
    const prewarm = drag.prewarm;
    drag.prewarm = null;
    return prewarm && !prewarm.win.isDestroyed() ? prewarm : null;
  }
  async function begin(ownerId, value) {
    if (!value || Object.keys(value).some((key) => !["transferId", "captureRect", "rowRect", "offset", "appearance"].includes(key))) throw new Error("Pane preview request is invalid.");
    const { transfer, source } = sourceEntry(ownerId, value.transferId);
    const blocked = nativePaneTransferBlockReason(transfer.target);
    if (blocked) throw new Error(blocked);
    if ([...drags.values()].some((drag) => drag.ownerId === ownerId)) throw new Error("A pane window drag is already active.");
    const captureRect = rect(value.captureRect, "Pane preview capture", 2048);
    const rowRect = rect(value.rowRect, "Pane tab row");
    const contentBounds = source.win.getContentBounds();
    const zoom = source.win.webContents.getZoomFactor();
    if (captureRect.x < 0 || captureRect.y < 0 || (captureRect.x + captureRect.width) * zoom > contentBounds.width + 1
      || (captureRect.y + captureRect.height) * zoom > contentBounds.height + 1 || captureRect.height > 100) throw new Error("Pane preview capture must remain inside source chrome.");
    const offset = value.offset;
    if (!offset || Object.keys(offset).some((key) => !["x", "y"].includes(key)) || ![offset.x, offset.y].every(Number.isFinite)
      || offset.x < 0 || offset.y < 0 || offset.x > captureRect.width || offset.y > captureRect.height) throw new Error("Pane preview pointer offset is invalid.");
    const style = appearance(value.appearance);
    const drag = {
      ownerId, transferId: transfer.transferId, source, captureRect, rowRect, offset, zoom, phase: "preparing",
      point: cursor(), preview: null, previewShown: false, previewAt: null, reported: "prepared", timer: null, destination: null,
      prewarm: null, prewarmTimer: null, prewarmTried: false, created: null,
    };
    drags.set(transfer.transferId, drag);
    transfer.finalOutcome.catch(() => {}).finally(() => dispose(drag));
    try {
      const capture = await capturePreview(source, captureRect);
      if (!live(drag) || drag.phase !== "preparing") return { phase: "cancelled" };
      sourceEntry(ownerId, transfer.transferId);
      drag.phase = "prepared";
      void Promise.resolve().then(() => openPreview(capture, style)).then((preview) => {
        if (!live(drag) || drag.phase === "closed") { preview.close(); return; }
        drag.preview = preview;
        presentPreview(drag);
        report(drag);
      }, (error) => { if (live(drag)) reportError?.(error); });
      drag.timer = setInterval(() => {
        if (drag.phase !== "prepared" && drag.phase !== "floating") return;
        try { move(ownerId, { transferId: drag.transferId }); }
        catch (error) { cancelInternal(drag); reportError?.(error); }
      }, CURSOR_SAMPLE_MS);
      drag.timer.unref?.();
      return { phase: "prepared" };
    } catch (error) { cancelInternal(drag); throw error; }
  }
  function move(ownerId, value) {
    const transferId = transferReference(value);
    const { source } = sourceEntry(ownerId, transferId);
    const drag = drags.get(transferId);
    if (!drag || drag.ownerId !== ownerId) throw new Error("Pane preview is unavailable.");
    if (!["prepared", "floating"].includes(drag.phase)) return { phase: drag.phase, destinationWindowId: null };
    if (source.win.webContents.getZoomFactor() !== drag.zoom) { cancelInternal(drag); throw new Error("Window zoom changed during pane drag."); }
    const point = cursor();
    drag.point = point;
    const row = screenRect(source, drag.rowRect);
    const floating = !contains(expand(row, drag.phase === "floating" ? 0 : DETACH_DISTANCE * drag.zoom), point);
    drag.destination = floating ? destinationAt(source, point) : null;
    drag.phase = floating ? "floating" : "prepared";
    presentPreview(drag);
    report(drag);
    const candidate = drag.destination;
    const zone = candidate && dropZones.get(candidate.webContentsId);
    const accepted = candidate?.accessToken === source.accessToken && zone && contains(screenRect(candidate, zone), point);
    if (floating && !candidate) schedulePrewarm(drag);
    return { phase: drag.phase, destinationWindowId: accepted ? candidate.bootstrap.windowId : null };
  }
  async function finish(ownerId, value) {
    const transferId = transferReference(value);
    const { transfer, source } = sourceEntry(ownerId, transferId);
    const drag = drags.get(transferId);
    if (!drag) { registry.cancelPaneTransfer(ownerId, { transferId }); return { status: "cancelled" }; }
    if (drag.ownerId !== ownerId) throw new Error("Pane preview belongs to another source.");
    if (drag.phase !== "prepared" && drag.phase !== "floating") { cancelInternal(drag); return { status: "cancelled" }; }
    const hit = move(ownerId, { transferId });
    if (hit.phase !== "floating") { cancelInternal(drag); return { status: "cancelled" }; }
    let destination = drag.destination;
    if (destination && hit.destinationWindowId == null) { cancelInternal(drag); return { status: "cancelled" }; }
    clearInterval(drag.timer);
    drag.phase = "transferring";
    let created = false;
    try {
      if (!destination) {
        destination = takePrewarm(drag) ?? createDestination(source, drag.point);
        created = true;
        drag.created = destination;
        placeDestination(drag, destination, estimatedOrigin(drag));
      }
      sourceEntry(ownerId, transferId);
      const accepted = registry.acceptPaneTransfer(destination.webContentsId, { transferId });
      notifyDestination(destination.webContentsId, accepted);
      const completion = await transfer.finalOutcome;
      return { status: "transferred", windowId: destination.bootstrap.windowId, transferId: completion.transferId };
    } catch (error) {
      if (created && destination && !destination.win.isDestroyed()) destination.win.close();
      cancelInternal(drag);
      throw error;
    } finally { dispose(drag); }
  }
  function anchor(ownerId, value) {
    if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).some((key) => !["transferId", "rect"].includes(key))
      || typeof value.transferId !== "string") throw new Error("Pane drop anchor is invalid.");
    const tab = rect(value.rect, "Pane drop anchor", 4096);
    const drag = drags.get(value.transferId);
    const entry = drag?.created;
    if (!drag || drag.phase !== "transferring" || entry?.webContentsId !== ownerId || entry.win.isDestroyed() || entry.win.isVisible()) return { anchored: false };
    const zoom = entry.win.webContents.getZoomFactor();
    const grab = { x: Math.min(drag.offset.x * drag.zoom, tab.width * zoom), y: Math.min(drag.offset.y * drag.zoom, tab.height * zoom) };
    placeDestination(drag, entry, { x: drag.point.x - tab.x * zoom - grab.x, y: drag.point.y - tab.y * zoom - grab.y });
    return { anchored: true };
  }
  async function reattach(ownerId, value) {
    const transferId = transferReference(value);
    const { transfer, source } = sourceEntry(ownerId, transferId);
    const blocked = nativePaneTransferBlockReason(transfer.target);
    if (blocked) throw new Error(blocked);
    const destination = registry.entryForWindowId(source.bootstrap.reattachWindowId);
    if (!destination || destination === source || destination.accessToken !== source.accessToken) throw new Error("Original authenticated window is unavailable.");
    const accepted = registry.acceptPaneTransfer(destination.webContentsId, { transferId });
    destination.win.show();
    destination.win.focus();
    notifyDestination(destination.webContentsId, accepted);
    await transfer.finalOutcome;
    return { status: "transferred", windowId: destination.bootstrap.windowId, transferId };
  }
  async function detach(ownerId, value) {
    const transferId = transferReference(value);
    const { transfer, source } = sourceEntry(ownerId, transferId);
    const blocked = nativePaneTransferBlockReason(transfer.target);
    if (blocked) throw new Error(blocked);
    let destination;
    try {
      destination = createDestination(source, cursor());
      registry.acceptPaneTransfer(destination.webContentsId, { transferId });
      await transfer.finalOutcome;
      return { status: "transferred", windowId: destination.bootstrap.windowId, transferId };
    } catch (error) {
      if (destination && !destination.win.isDestroyed()) destination.win.close();
      throw error;
    }
  }
  function cancelInternal(drag) {
    if (!live(drag)) return;
    dispose(drag);
    try { registry.cancelPaneTransfer(drag.ownerId, { transferId: drag.transferId }); }
    catch (error) {
      try { const transfer = registry.paneTransferForSource(drag.ownerId, { transferId: drag.transferId }); registry.abortCommittedPaneTransfer(transfer.destinationWebContentsId, { transferId: drag.transferId }, error); }
      catch {}
    }
  }
  function dispose(drag) {
    if (!live(drag)) return;
    drags.delete(drag.transferId);
    clearInterval(drag.timer);
    const prewarm = takePrewarm(drag);
    drag.phase = "closed";
    drag.preview?.close();
    if (prewarm) {
      try { (discardDestination ?? ((entry) => entry.win.close()))(prewarm); }
      catch (error) { reportError?.(error); }
    }
    notifySource(drag.ownerId, { transferId: drag.transferId, phase: "closed" });
  }
  function cancel(ownerId, value) {
    const transferId = transferReference(value);
    sourceEntry(ownerId, transferId);
    const drag = drags.get(transferId);
    if (drag) cancelInternal(drag);
    else registry.cancelPaneTransfer(ownerId, { transferId });
    return { cancelled: true, transferId };
  }
  function removeOwner(ownerId) {
    dropZones.delete(ownerId);
    for (const drag of drags.values()) {
      if (drag.ownerId === ownerId) cancelInternal(drag);
      else if (drag.prewarm?.webContentsId === ownerId) drag.prewarm = null;
    }
  }
  return { begin, move, finish, cancel, anchor, reattach, detach, registerDropZone, removeOwner };
}
module.exports = { createPaneWindowDragHost, nativePaneTransferBlockReason, contains, rect };
