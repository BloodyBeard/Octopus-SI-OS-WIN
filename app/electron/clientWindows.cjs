const { randomUUID } = require("node:crypto");

const CLIENT_WINDOW_SCHEMA = "octopus.client-window";
const CLIENT_WINDOW_SCHEMA_VERSION = 1;
const CLIENT_WINDOW_READY_TIMEOUT_MS = 15_000;
const CLIENT_PANE_TRANSFER_TIMEOUT_MS = 30_000;
const CLIENT_PANE_TRANSFER_SCHEMA = "octopus.client-pane-transfer";
const CLIENT_PANE_TRANSFER_SCHEMA_VERSION = 1;
const CLIENT_WORKSPACE_KEYS = new Set(["dashboard", "octopus-os"]);
const WORKER_ENVIRONMENT_PANEL_KINDS = new Set([
  "browser",
  "connection",
  "container",
  "runtime",
  "worktree",
  "files",
  "file",
  "terminal",
  "changes",
  "desktop",
  "custom",
  "overview",
]);
const CONTROL_CHARACTERS = /[\u0000-\u001f\u007f]/;

function createClientWindowRegistry(options = {}) {
  const idFactory = typeof options.idFactory === "function" ? options.idFactory : randomUUID;
  const readyTimeoutMs = Number.isFinite(options.readyTimeoutMs) && options.readyTimeoutMs > 0
    ? options.readyTimeoutMs
    : CLIENT_WINDOW_READY_TIMEOUT_MS;
  const transferTimeoutMs = Number.isFinite(options.transferTimeoutMs) && options.transferTimeoutMs > 0
    ? options.transferTimeoutMs
    : CLIENT_PANE_TRANSFER_TIMEOUT_MS;
  const byWindowId = new Map();
  const byWebContentsId = new Map();
  const paneTransfers = new Map();
  const paneTransfersBySource = new Map();
  const sourceFinalizations = new Map();
  let focusRevision = 0;

  function register(win, value = {}) {
    const webContentsId = windowWebContentsId(win);
    if (byWebContentsId.has(webContentsId)) throw new Error("Client window webContents is already registered.");
    const primary = value.primary === true;
    if (primary && entries().some((entry) => entry.primary)) throw new Error("The primary Octopus client window already exists.");
    const accessToken = optionalOpaqueString(value.accessToken, "Client window access token", 1_024);
    const request = value.initialTarget === undefined
      ? {}
      : validateClientWindowOpenRequest({ initialTarget: value.initialTarget });
    const windowId = primary ? "main" : allocateWindowId();
    const bootstrap = Object.freeze({
      schema: CLIENT_WINDOW_SCHEMA,
      schemaVersion: CLIENT_WINDOW_SCHEMA_VERSION,
      windowId,
      scope: primary ? "main" : windowId,
      primary,
      ...(request.initialTarget ? { initialTarget: request.initialTarget } : {}),
      ...(value.reattachWindowId ? { reattachWindowId: requiredIdentifier(value.reattachWindowId, "Reattach window id", 160) } : {}),
    });
    const entry = {
      accessToken,
      bootstrap,
      focusRevision: 0,
      primary,
      ready: primary,
      waiters: new Set(),
      webContentsId,
      win,
    };
    const onFocus = () => {
      if (byWindowId.get(windowId) === entry) entry.focusRevision = focusRevision += 1;
    };
    const onClosed = () => unregister(entry);
    entry.onClosed = onClosed;
    entry.onFocus = onFocus;
    byWindowId.set(windowId, entry);
    byWebContentsId.set(webContentsId, entry);
    win.on("focus", onFocus);
    win.once("closed", onClosed);
    if (win.isFocused?.()) onFocus();
    return copyBootstrap(bootstrap);
  }

  function allocateWindowId() {
    for (let attempt = 0; attempt < 16; attempt += 1) {
      const id = requiredIdentifier(idFactory(), "Client window id", 128);
      const windowId = `window-${id}`;
      if (!byWindowId.has(windowId)) return windowId;
    }
    throw new Error("Could not allocate a unique client window id.");
  }

  function unregister(entry) {
    if (byWindowId.get(entry.bootstrap.windowId) !== entry) return;
    byWindowId.delete(entry.bootstrap.windowId);
    byWebContentsId.delete(entry.webContentsId);
    for (const [id, receipt] of sourceFinalizations) if (receipt.sourceWebContentsId === entry.webContentsId) sourceFinalizations.delete(id);
    entry.win.removeListener?.("focus", entry.onFocus);
    for (const transfer of [...paneTransfers.values()]) {
      if (transfer.sourceWebContentsId === entry.webContentsId) {
        if (!transfer.sourceReleased) removePaneTransfer(transfer);
      } else if (transfer.destinationWebContentsId === entry.webContentsId) removePaneTransfer(transfer);
    }
    const error = new Error(`Client window ${entry.bootstrap.windowId} closed before destination initialization.`);
    for (const waiter of entry.waiters) {
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
    entry.waiters.clear();
  }

  function entries() {
    for (const entry of [...byWindowId.values()]) {
      if (entry.win.isDestroyed?.()) unregister(entry);
    }
    return [...byWindowId.values()];
  }

  function entryForWebContents(webContentsId) {
    const entry = byWebContentsId.get(webContentsId) ?? null;
    if (entry?.win.isDestroyed?.()) {
      unregister(entry);
      return null;
    }
    return entry;
  }

  function entryForWindow(win) {
    if (!win || win.isDestroyed?.()) return null;
    return entryForWebContents(win.webContents?.id);
  }

  function entryForWindowId(windowId) {
    const entry = byWindowId.get(windowId) ?? null;
    if (entry?.win.isDestroyed?.()) {
      unregister(entry);
      return null;
    }
    return entry;
  }

  function primaryEntry() {
    return entries().find((entry) => entry.primary) ?? null;
  }

  function select(value = {}) {
    const accessToken = value.accessToken === undefined
      ? undefined
      : optionalOpaqueString(value.accessToken, "Client window access token", 1_024);
    const accepts = (entry) => entry && (accessToken === undefined || entry.accessToken === accessToken);
    const focused = entryForWindow(value.focusedWindow);
    if (accepts(focused)) return focused;
    const lastFocused = entries()
      .filter((entry) => entry.focusRevision > 0)
      .sort((left, right) => right.focusRevision - left.focusRevision)
      .find(accepts) ?? null;
    if (accepts(lastFocused)) return lastFocused;
    const primary = primaryEntry();
    if (accepts(primary)) return primary;
    return entries().find(accepts) ?? null;
  }

  function bootstrapForWebContents(webContentsId) {
    const entry = entryForWebContents(webContentsId);
    return entry ? copyBootstrap(entry.bootstrap) : null;
  }

  function setAccessToken(webContentsId, token) {
    const entry = entryForWebContents(webContentsId);
    if (!entry) throw new Error("Authenticated client window is not registered.");
    entry.accessToken = requiredOpaqueString(token, "Client window access token", 1_024);
  }

  function clearAccessToken(token) {
    for (const [id, receipt] of sourceFinalizations) if (!token || receipt.accessToken === token) sourceFinalizations.delete(id);
    for (const entry of entries()) {
      if (token && entry.accessToken !== token) continue;
      for (const transfer of [...paneTransfers.values()]) {
        if (transfer.sourceWebContentsId === entry.webContentsId || transfer.destinationWebContentsId === entry.webContentsId) {
          removePaneTransfer(transfer);
        }
      }
      entry.accessToken = "";
    }
  }

  function closeAuthenticatedPeers() {
    const peers = entries().filter((entry) => !entry.primary && entry.accessToken);
    for (const entry of peers) {
      if (!entry.win.isDestroyed?.()) entry.win.close();
    }
    return peers.length;
  }

  function markReady(webContentsId, value) {
    const request = exactObject(value, "Client window readiness", ["windowId"]);
    const windowId = requiredIdentifier(request.windowId, "Client window readiness id", 160);
    const entry = entryForWebContents(webContentsId);
    if (!entry || entry.bootstrap.windowId !== windowId) throw new Error("Client window readiness does not match its webContents bootstrap.");
    entry.ready = true;
    const outcome = { opened: true, windowId };
    for (const waiter of entry.waiters) {
      clearTimeout(waiter.timer);
      waiter.resolve(outcome);
    }
    entry.waiters.clear();
    return outcome;
  }

  function waitUntilReady(windowIdValue) {
    const windowId = requiredIdentifier(windowIdValue, "Client window id", 160);
    const entry = entryForWindowId(windowId);
    if (!entry) return Promise.reject(new Error(`Client window ${windowId} is no longer available.`));
    if (entry.ready) return Promise.resolve({ opened: true, windowId });
    return new Promise((resolve, reject) => {
      const waiter = { resolve, reject, timer: null };
      waiter.timer = setTimeout(() => {
        entry.waiters.delete(waiter);
        reject(new Error(`Client window ${windowId} did not become ready within ${readyTimeoutMs}ms.`));
      }, readyTimeoutMs);
      entry.waiters.add(waiter);
    });
  }

  function beginPaneTransfer(webContentsId, value) {
    const source = entryForWebContents(webContentsId);
    if (!source?.accessToken) throw new Error("Client pane transfer requires an authenticated source window.");
    const request = exactObject(value, "Client pane transfer", ["sourcePane", "sourceTabId", "target"]);
    const sourcePane = requiredPane(request.sourcePane, "Client pane transfer source pane");
    const sourceTabId = requiredText(request.sourceTabId, "Client pane transfer source tab id", 512);
    if (!sourceTabId.startsWith("client-window:") && sourcePane !== "secondary") {
      throw new Error("A client pane tab transfer must originate from the secondary pane.");
    }
    const target = validateTransferableWorkspacePaneTarget(request.target);
    const sourceKey = paneTransferSourceKey(source.webContentsId, sourceTabId);
    const existingTransferId = paneTransfersBySource.get(sourceKey);
    if (existingTransferId && paneTransfers.has(existingTransferId)) {
      throw new Error("Client pane transfer is already active for this source tab.");
    }
    paneTransfersBySource.delete(sourceKey);
    let transferId = "";
    for (let attempt = 0; attempt < 16; attempt += 1) {
      const candidate = `transfer-${requiredIdentifier(idFactory(), "Client pane transfer id", 128)}`;
      if (!paneTransfers.has(candidate) && !sourceFinalizations.has(candidate)) {
        transferId = candidate;
        break;
      }
    }
    if (!transferId) throw new Error("Could not allocate a unique client pane transfer id.");
    const transfer = {
      accessToken: source.accessToken,
      committed: false,
      commitOutcome: null,
      destinationWebContentsId: 0,
      rejectCommit: null,
      resolveCommit: null,
      sourceReleased: false,
      sourceTabId,
      sourceKey,
      sourcePane,
      sourceWebContentsId: source.webContentsId,
      sourceWindowId: source.bootstrap.windowId,
      target,
      timer: null,
      transferId,
    };
    transfer.finalized = false;
    transfer.finalOutcome = new Promise((resolve, reject) => {
      transfer.resolveFinal = resolve;
      transfer.rejectFinal = reject;
    });
    transfer.finalOutcome.catch(() => undefined);
    transfer.timer = setTimeout(() => removePaneTransfer(transfer), transferTimeoutMs);
    transfer.timer.unref?.();
    paneTransfers.set(transferId, transfer);
    paneTransfersBySource.set(sourceKey, transferId);
    return paneTransferReference(transfer);
  }

  function acceptPaneTransfer(webContentsId, value) {
    const destination = entryForWebContents(webContentsId);
    if (!destination?.accessToken) throw new Error("Client pane transfer requires an authenticated destination window.");
    const transfer = requirePaneTransfer(value);
    if (transfer.committed) throw new Error("Client pane transfer is already committed for source release.");
    const source = entryForWebContents(transfer.sourceWebContentsId);
    if (
      !source?.accessToken
      || source.accessToken !== transfer.accessToken
      || destination.accessToken !== transfer.accessToken
    ) {
      throw new Error("Client pane transfer windows do not share one authenticated client session.");
    }
    if (source.webContentsId === destination.webContentsId) {
      throw new Error("Client pane transfer destination must be another full client window.");
    }
    if (transfer.destinationWebContentsId && transfer.destinationWebContentsId !== destination.webContentsId) {
      throw new Error("Client pane transfer is already owned by another destination window.");
    }
    const destinationBusy = [...paneTransfers.values()].some((candidate) => (
      candidate !== transfer && candidate.destinationWebContentsId === destination.webContentsId
    ));
    if (destinationBusy) throw new Error("Client pane transfer destination is already accepting another tab.");
    transfer.destinationWebContentsId = destination.webContentsId;
    return {
      ...paneTransferReference(transfer),
      sourcePane: transfer.sourcePane,
      sourceTabId: transfer.sourceTabId,
      target: copyBootstrap(transfer.target),
    };
  }

  function commitPaneTransfer(webContentsId, value) {
    const transfer = requirePaneTransfer(value);
    if (transfer.committed) throw new Error("Client pane transfer is already committed for source release.");
    if (!transfer.destinationWebContentsId || transfer.destinationWebContentsId !== webContentsId) {
      throw new Error("Client pane transfer commit does not match its accepted destination.");
    }
    const destination = entryForWebContents(webContentsId);
    const source = entryForWebContents(transfer.sourceWebContentsId);
    if (
      !destination?.accessToken
      || !source?.accessToken
      || destination.accessToken !== transfer.accessToken
      || source.accessToken !== transfer.accessToken
    ) {
      throw new Error("Client pane transfer session changed before commit.");
    }
    transfer.commitOutcome = new Promise((resolve, reject) => {
      transfer.resolveCommit = resolve;
      transfer.rejectCommit = reject;
    });
    transfer.commitOutcome.catch(() => undefined);
    transfer.committed = true;
    clearTimeout(transfer.timer);
    transfer.timer = setTimeout(() => removePaneTransfer(transfer), transferTimeoutMs);
    transfer.timer.unref?.();
    const completion = paneTransferCompletion(transfer);
    return { completion, outcome: transfer.commitOutcome, sourceWebContentsId: source.webContentsId };
  }

  function acknowledgePaneTransfer(webContentsId, value) {
    const request = exactObject(value, "Client pane transfer acknowledgement", ["released", "transferId"]);
    if (typeof request.released !== "boolean") throw new Error("Client pane transfer acknowledgement requires an explicit release verdict.");
    const transfer = requirePaneTransfer({ transferId: request.transferId });
    if (!transfer.committed || transfer.sourceReleased || transfer.sourceWebContentsId !== webContentsId) {
      throw new Error("Client pane transfer acknowledgement belongs to its committed source window.");
    }
    const source = entryForWebContents(webContentsId);
    if (!source?.accessToken || source.accessToken !== transfer.accessToken) {
      throw new Error("Client pane transfer session changed before source acknowledgement.");
    }
    recordPaneTransferSourceVerdict(transfer, request.released);
    return { released: request.released, transferId: transfer.transferId };
  }

  function resumePaneTransfer(webContentsId, value) {
    const transfer = requirePaneTransfer(value);
    if (!transfer.committed || transfer.destinationWebContentsId !== webContentsId) {
      throw new Error("Client pane transfer resume belongs to its committed destination window.");
    }
    const destination = entryForWebContents(webContentsId);
    if (!destination?.accessToken || destination.accessToken !== transfer.accessToken) {
      throw new Error("Client pane transfer session changed before destination resume.");
    }
    return transfer.sourceReleased
      ? Promise.resolve(paneTransferCompletion(transfer))
      : transfer.commitOutcome;
  }

  function finalizePaneTransfer(webContentsId, value) {
    const transfer = requirePaneTransfer(value);
    if (!transfer.committed || !transfer.sourceReleased || transfer.destinationWebContentsId !== webContentsId) {
      throw new Error("Client pane transfer finalization belongs to its released destination window.");
    }
    const destination = entryForWebContents(webContentsId);
    if (!destination?.accessToken || destination.accessToken !== transfer.accessToken) {
      throw new Error("Client pane transfer session changed before destination finalization.");
    }
    const source = entryForWebContents(transfer.sourceWebContentsId);
    if (source && source.accessToken !== transfer.accessToken) throw new Error("Source session changed before destination finalization.");
    transfer.finalized = true;
    if (entryForWebContents(transfer.sourceWebContentsId)) sourceFinalizations.set(transfer.transferId, {
      accessToken: transfer.accessToken,
      sourceWebContentsId: transfer.sourceWebContentsId,
      completion: paneTransferCompletion(transfer),
    });
    transfer.resolveFinal(paneTransferCompletion(transfer));
    removePaneTransfer(transfer);
    return { finalized: true, transferId: transfer.transferId };
  }

  function abortCommittedPaneTransfer(webContentsId, value, reason) {
    const transfer = requirePaneTransfer(value);
    if (!transfer.committed || transfer.destinationWebContentsId !== webContentsId) {
      throw new Error("Client pane transfer abort belongs to its committed destination window.");
    }
    removePaneTransfer(transfer, reason instanceof Error ? reason : undefined);
    return { aborted: true, transferId: transfer.transferId };
  }

  function rejectDestinationPaneTransfer(webContentsId, value) {
    const transfer = paneTransferForDestination(webContentsId, value);
    removePaneTransfer(transfer, new Error("Destination could not complete pane handoff."));
    return { rejected: true, transferId: transfer.transferId };
  }

  function cancelPaneTransfer(webContentsId, value) {
    const transfer = requirePaneTransfer(value);
    if (transfer.sourceWebContentsId !== webContentsId) {
      throw new Error("Client pane transfer cancellation belongs to its source window.");
    }
    if (transfer.committed) throw new Error("A committed client pane transfer requires source acknowledgement.");
    const source = entryForWebContents(webContentsId);
    if (!source?.accessToken || source.accessToken !== transfer.accessToken) {
      throw new Error("Client pane transfer session changed before cancellation.");
    }
    removePaneTransfer(transfer);
    return { cancelled: true, transferId: transfer.transferId };
  }

  function requirePaneTransfer(value) {
    const request = exactObject(value, "Client pane transfer reference", ["transferId"]);
    const transferId = requiredIdentifier(request.transferId, "Client pane transfer reference id", 160);
    const transfer = paneTransfers.get(transferId);
    if (!transfer) throw new Error("Client pane transfer is unavailable or expired.");
    return transfer;
  }

  function paneTransferForSource(webContentsId, value) {
    const transfer = requirePaneTransfer(value);
    const source = entryForWebContents(webContentsId);
    if (transfer.sourceWebContentsId !== webContentsId || !source?.accessToken || source.accessToken !== transfer.accessToken) {
      throw new Error("Pane transfer belongs to another authenticated source.");
    }
    return transfer;
  }

  function paneTransferForDestination(webContentsId, value) {
    const transfer = requirePaneTransfer(value);
    const destination = entryForWebContents(webContentsId);
    if (transfer.destinationWebContentsId !== webContentsId || !destination?.accessToken || destination.accessToken !== transfer.accessToken) {
      throw new Error("Pane transfer belongs to another authenticated destination.");
    }
    return transfer;
  }

  function pendingSourceFinalizations(webContentsId) {
    const source = entryForWebContents(webContentsId);
    if (!source?.accessToken) throw new Error("Finalization receipts require an authenticated source.");
    return [...sourceFinalizations.values()].filter((receipt) => receipt.sourceWebContentsId === webContentsId && receipt.accessToken === source.accessToken).map((receipt) => copyBootstrap(receipt.completion));
  }

  function acknowledgeSourceFinalization(webContentsId, value) {
    const request = exactObject(value, "Source finalization receipt", ["transferId"]);
    const receipt = sourceFinalizations.get(request.transferId);
    const source = entryForWebContents(webContentsId);
    if (!receipt || receipt.sourceWebContentsId !== webContentsId || !source?.accessToken || receipt.accessToken !== source.accessToken) throw new Error("Source finalization receipt does not match its authenticated owner.");
    sourceFinalizations.delete(request.transferId);
    return { acknowledged: true, transferId: request.transferId };
  }

  function recordPaneTransferSourceVerdict(transfer, released) {
    const completion = paneTransferCompletion(transfer);
    const resolve = transfer.resolveCommit;
    const reject = transfer.rejectCommit;
    transfer.resolveCommit = null;
    transfer.rejectCommit = null;
    if (!released) {
      removePaneTransfer(transfer);
      reject?.(new Error("Client pane transfer source retained ownership before commit."));
      return;
    }
    transfer.sourceReleased = true;
    resolve?.(completion);
  }

  function removePaneTransfer(transfer, reason) {
    if (paneTransfers.get(transfer.transferId) !== transfer) return;
    paneTransfers.delete(transfer.transferId);
    if (paneTransfersBySource.get(transfer.sourceKey) === transfer.transferId) {
      paneTransfersBySource.delete(transfer.sourceKey);
    }
    clearTimeout(transfer.timer);
    const reject = transfer.rejectCommit;
    transfer.resolveCommit = null;
    transfer.rejectCommit = null;
    reject?.(reason ?? new Error("Client pane transfer ended before source release."));
    if (!transfer.finalized) transfer.rejectFinal(reason ?? new Error("Client pane transfer ended before destination finalization."));
    options.onTransferEnd?.({
      completion: paneTransferCompletion(transfer),
      destinationWebContentsId: transfer.destinationWebContentsId,
      finalized: transfer.finalized,
      sourceWebContentsId: transfer.sourceWebContentsId,
    });
  }

  function pendingPaneTransferCompletions(webContentsId) {
    const source = entryForWebContents(webContentsId);
    if (!source?.accessToken) throw new Error("Pending client pane transfers require an authenticated source window.");
    return [...paneTransfers.values()].flatMap((transfer) => (
      transfer.committed
      && !transfer.sourceReleased
      && transfer.sourceWebContentsId === webContentsId
      && transfer.accessToken === source.accessToken
        ? [paneTransferCompletion(transfer)]
        : []
    ));
  }

  function pendingDestinationPaneTransfers(webContentsId) {
    const destination = entryForWebContents(webContentsId);
    if (!destination?.accessToken) throw new Error("Pending destination pane transfers require an authenticated client window.");
    return [...paneTransfers.values()].flatMap((transfer) => (
      transfer.destinationWebContentsId === webContentsId
      && transfer.accessToken === destination.accessToken
        ? [{ ...paneTransferCompletion(transfer), committed: transfer.committed }]
        : []
    ));
  }

  return {
    acknowledgePaneTransfer,
    acknowledgeSourceFinalization,
    acceptPaneTransfer,
    abortCommittedPaneTransfer,
    beginPaneTransfer,
    bootstrapForWebContents,
    cancelPaneTransfer,
    clearAccessToken,
    closeAuthenticatedPeers,
    commitPaneTransfer,
    entries,
    entryForWebContents,
    entryForWindow,
    entryForWindowId,
    finalizePaneTransfer,
    markReady,
    pendingDestinationPaneTransfers,
    pendingPaneTransferCompletions,
    pendingSourceFinalizations,
    paneTransferForSource,
    paneTransferForDestination,
    primaryEntry,
    register,
    resumePaneTransfer,
    rejectDestinationPaneTransfer,
    select,
    setAccessToken,
    waitUntilReady,
  };
}

function validateClientWindowOpenRequest(value) {
  if (value === undefined) return {};
  const request = exactObject(value, "Client window open request", ["initialTarget"]);
  if (!Object.hasOwn(request, "initialTarget")) return {};
  return { initialTarget: validateTransferableWorkspacePaneTarget(request.initialTarget) };
}

function validateTransferableWorkspacePaneTarget(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Client window pane target must be an object.");
  const target = value;
  if (target.kind === "terminal") throw new Error("Terminal panes cannot be transferred to another client window.");
  if (target.kind === "surface") return validateSurfaceTarget(target);
  if (target.kind === "files") return validateFilesTarget(target);
  if (target.kind === "worker_environment") return validateWorkerEnvironmentTarget(target);
  throw new Error("Client window pane target kind is unsupported.");
}

function validateSurfaceTarget(value) {
  exactKeys(value, "Client window surface target", ["kind", "runId", "surfaceId", "workspaceKey"]);
  const surfaceId = requiredText(value.surfaceId, "Client window surface id", 512);
  const runId = requiredText(value.runId, "Client window surface run id", 512, true);
  if (surfaceId !== "chat" && runId) throw new Error("Only a chat surface target can carry a run id.");
  return {
    kind: "surface",
    runId,
    surfaceId,
    workspaceKey: workspaceKey(value.workspaceKey),
  };
}

function validateFilesTarget(value) {
  exactKeys(value, "Client window files target", ["kind", "openTarget", "snapshot", "workspaceKey"]);
  const result = { kind: "files", workspaceKey: workspaceKey(value.workspaceKey) };
  if (Object.hasOwn(value, "openTarget")) result.openTarget = validateFilesOpenTarget(value.openTarget);
  if (Object.hasOwn(value, "snapshot")) result.snapshot = validateFilesPaneSnapshot(value.snapshot);
  return result;
}

function validateFilesPaneSnapshot(value) {
  const textLimit = 2 * 1024 * 1024;
  function normalizeFilesPaneSnapshot(value) {
      if (!record(value, ["version", "id", "revision", "target", "rootId", "rootPath", "file", "draft", "view"]))
          return null;
      if (value.version !== 1 || !token(value.id, 128) || !integer(value.revision) || !pathText(value.rootId, 512) || !pathText(value.rootPath, 4096))
          return null;
      if (Boolean(value.rootId) !== Boolean(value.rootPath) || !text(value.draft))
          return null;
      const view = value.view;
      if (!record(view, ["scrollTop", "scrollLeft"]) || !offset(view.scrollTop) || !offset(view.scrollLeft))
          return null;
      const target = value.target === undefined ? undefined : normalizeTarget(value.target);
      if (value.target !== undefined && !target)
          return null;
      let file = null;
      if (value.file !== null) {
          const row = value.file;
          if (!record(row, ["rootId", "path", "name", "size", "modifiedAt", "kind", "text", "truncated", "digest"]))
              return null;
          if (!token(row.rootId, 512) || row.rootId !== value.rootId || !token(row.path, 4096) || !token(row.name, 512) || !integer(row.size))
              return null;
          if (!token(row.modifiedAt, 128) || !Number.isFinite(Date.parse(row.modifiedAt)) || (row.kind !== "text" && row.kind !== "binary") || !text(row.text) || typeof row.truncated !== "boolean" || typeof row.digest !== "string")
              return null;
          if (row.kind === "text" && !row.truncated ? !/^[a-f0-9]{64}$/.test(row.digest) : row.digest !== "")
              return null;
          if ((row.kind !== "text" || row.truncated) && value.draft !== row.text)
              return null;
          if (target?.kind === "artifact")
              return null;
          file = { rootId: row.rootId, path: row.path, name: row.name, size: row.size, modifiedAt: row.modifiedAt, kind: row.kind, text: row.text, truncated: row.truncated, digest: row.digest };
      }
      else if (value.draft !== "")
          return null;
      return {
          version: 1, id: value.id, revision: value.revision, ...(target ? { target } : {}),
          rootId: value.rootId, rootPath: value.rootPath, file, draft: value.draft,
          view: { scrollTop: view.scrollTop, scrollLeft: view.scrollLeft },
      };
  }
  function normalizeTarget(value) {
      if (!record(value, ["kind", "absolutePath", "path", "rootId", "artifactId", "createdAtMs", "interfaceId", "label", "runId"]))
          return null;
      if (value.kind === "file") {
          if (!record(value, ["kind", "absolutePath", "path", "rootId"]))
              return null;
          if (value.absolutePath !== undefined && !token(value.absolutePath, 4096))
              return null;
          if ((value.path !== undefined) !== (value.rootId !== undefined))
              return null;
          if (value.path !== undefined && (!pathText(value.path, 4096) || !token(value.rootId, 512)))
              return null;
          if (value.absolutePath === undefined && value.path === undefined)
              return null;
          return { kind: "file", ...(typeof value.absolutePath === "string" ? { absolutePath: value.absolutePath } : {}), ...(typeof value.path === "string" && typeof value.rootId === "string" ? { path: value.path, rootId: value.rootId } : {}) };
      }
      if (value.kind !== "artifact" || !record(value, ["kind", "artifactId", "createdAtMs", "interfaceId", "label", "runId"]))
          return null;
      if (!token(value.artifactId, 1024) || !token(value.interfaceId, 512) || !token(value.label, 1024) || !token(value.runId, 512) || value.createdAtMs !== undefined && !integer(value.createdAtMs))
          return null;
      return { kind: "artifact", artifactId: value.artifactId, interfaceId: value.interfaceId, label: value.label, runId: value.runId, ...(typeof value.createdAtMs === "number" ? { createdAtMs: value.createdAtMs } : {}) };
  }
  function record(value, keys) {
      return value !== null && typeof value === "object" && !Array.isArray(value) && Object.keys(value).every((key) => keys.includes(key));
  }
  function integer(value) { return Number.isSafeInteger(value) && value >= 0; }
  function offset(value) { return typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= Number.MAX_SAFE_INTEGER; }
  function pathText(value, maximum) { return typeof value === "string" && value.length <= maximum && !/[\u0000-\u001f\u007f]/.test(value); }
  function token(value, maximum) { return pathText(value, maximum) && value.trim().length > 0; }
  function text(value) { return typeof value === "string" && new TextEncoder().encode(value).byteLength <= textLimit; }

  const snapshot = normalizeFilesPaneSnapshot(value);
  if (!snapshot) throw new Error("Client window Files snapshot is invalid: expected version 1, exact document/root identity, safe revision and scroll offsets, complete CAS baseline, and at most 2 MiB UTF-8 for each text buffer.");
  return snapshot;
}

function validateFilesOpenTarget(value) {
  const target = exactObject(value, "Client window files open target", ["kind", "absolutePath", "path", "rootId", "artifactId", "createdAtMs", "interfaceId", "label", "runId"]);
  if (target.kind === "file") {
    exactKeys(target, "Client window file target", ["kind", "absolutePath", "path", "rootId"]);
    const hasAbsolutePath = Object.hasOwn(target, "absolutePath");
    const hasPath = Object.hasOwn(target, "path");
    const hasRootId = Object.hasOwn(target, "rootId");
    if (!hasAbsolutePath && !hasPath && !hasRootId) throw new Error("Client window file target is empty.");
    if (hasPath !== hasRootId) throw new Error("Client window file target path requires an exact root id.");
    return {
      kind: "file",
      ...(hasAbsolutePath ? { absolutePath: requiredPath(target.absolutePath, "Client window absolute file path", 4_096) } : {}),
      ...(hasPath ? {
        path: requiredPath(target.path, "Client window rooted file path", 4_096, true),
        rootId: requiredText(target.rootId, "Client window file root id", 512),
      } : {}),
    };
  }
  if (target.kind === "artifact") {
    exactKeys(target, "Client window artifact target", ["kind", "artifactId", "createdAtMs", "interfaceId", "label", "runId"]);
    const createdAtMs = Object.hasOwn(target, "createdAtMs")
      ? nonNegativeSafeInteger(target.createdAtMs, "Client window artifact creation time")
      : undefined;
    return {
      kind: "artifact",
      artifactId: requiredText(target.artifactId, "Client window artifact id", 1_024),
      interfaceId: requiredText(target.interfaceId, "Client window artifact interface id", 512),
      label: requiredText(target.label, "Client window artifact label", 1_024),
      runId: requiredText(target.runId, "Client window artifact run id", 512),
      ...(createdAtMs === undefined ? {} : { createdAtMs }),
    };
  }
  throw new Error("Client window files open target kind is unsupported.");
}

function validateWorkerEnvironmentTarget(value) {
  exactKeys(value, "Client window Worker Environment target", ["kind", "target", "workspaceKey"]);
  const target = exactObject(value.target, "Client window Worker Environment pane target", [
    "schemaVersion",
    "workerEnvironmentId",
    "surfaceId",
    "panelKind",
    "filePath",
    "clientSurface",
  ]);
  if (target.schemaVersion !== 1) throw new Error("Client window Worker Environment target schemaVersion must be 1.");
  const result = {
    schemaVersion: 1,
    workerEnvironmentId: requiredText(target.workerEnvironmentId, "Client window Worker Environment id", 512),
  };
  if (Object.hasOwn(target, "surfaceId")) result.surfaceId = requiredText(target.surfaceId, "Client window Worker Environment surface id", 512);
  if (Object.hasOwn(target, "panelKind")) {
    const panelKind = requiredText(target.panelKind, "Client window Worker Environment panel kind", 64);
    if (!WORKER_ENVIRONMENT_PANEL_KINDS.has(panelKind)) throw new Error("Client window Worker Environment panel kind is unsupported.");
    result.panelKind = panelKind;
  }
  if (Object.hasOwn(target, "filePath")) {
    if (!result.surfaceId) throw new Error("Client window Worker Environment file target requires an exact surface id.");
    result.filePath = requiredPath(target.filePath, "Client window Worker Environment file path", 4_096);
  }
  if (Object.hasOwn(target, "clientSurface")) {
    if (!result.surfaceId) throw new Error("Client window Worker Environment client surface requires an exact surface id.");
    const clientSurface = exactObject(target.clientSurface, "Client window Worker Environment client surface", ["documentId", "revision", "sourceDigest"]);
    result.clientSurface = {
      documentId: requiredText(clientSurface.documentId, "Client window client surface document id", 512),
      revision: positiveSafeInteger(clientSurface.revision, "Client window client surface revision"),
      sourceDigest: requiredText(clientSurface.sourceDigest, "Client window client surface source digest", 128),
    };
  }
  return {
    kind: "worker_environment",
    target: result,
    workspaceKey: workspaceKey(value.workspaceKey),
  };
}

function cascadeClientWindowBounds(referenceValue, workAreaValue, fallbackValue, offsetValue = 28) {
  const fallback = finiteBounds(fallbackValue) ?? { width: 1120, height: 720 };
  const reference = finiteBounds(referenceValue);
  const workArea = finiteBounds(workAreaValue);
  if (
    !reference
    || !workArea
    || !Number.isFinite(reference.x)
    || !Number.isFinite(reference.y)
    || !Number.isFinite(workArea.x)
    || !Number.isFinite(workArea.y)
  ) {
    return { width: fallback.width, height: fallback.height };
  }
  const offset = Number.isFinite(offsetValue) && offsetValue > 0 ? offsetValue : 28;
  const width = Math.min(reference.width, workArea.width);
  const height = Math.min(reference.height, workArea.height);
  const maximumX = workArea.x + workArea.width - width;
  const maximumY = workArea.y + workArea.height - height;
  const nextX = reference.x + offset;
  const nextY = reference.y + offset;
  const x = nextX <= maximumX ? Math.max(workArea.x, nextX) : Math.min(maximumX, workArea.x + offset);
  const y = nextY <= maximumY ? Math.max(workArea.y, nextY) : Math.min(maximumY, workArea.y + offset);
  return { x: Math.round(x), y: Math.round(y), width: Math.round(width), height: Math.round(height) };
}

function finiteBounds(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const width = Number(value.width);
  const height = Number(value.height);
  if (!Number.isFinite(width) || width <= 0 || !Number.isFinite(height) || height <= 0) return null;
  const result = { width, height };
  if (Number.isFinite(value.x) && Number.isFinite(value.y)) {
    result.x = value.x;
    result.y = value.y;
  }
  return result;
}

function windowWebContentsId(win) {
  const id = win?.webContents?.id;
  if (!Number.isSafeInteger(id) || id <= 0 || typeof win.on !== "function" || typeof win.once !== "function") {
    throw new Error("Client window registration requires a live BrowserWindow.");
  }
  return id;
}

function workspaceKey(value) {
  if (!CLIENT_WORKSPACE_KEYS.has(value)) throw new Error("Client window workspace key is unsupported.");
  return value;
}

function exactObject(value, label, keys) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object.`);
  exactKeys(value, label, keys);
  return value;
}

function exactKeys(value, label, keys) {
  const allowed = new Set(keys);
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) throw new Error(`${label} contains unsupported field ${key}.`);
  }
}

function requiredIdentifier(value, label, maximum) {
  const text = requiredText(value, label, maximum);
  if (!/^[A-Za-z0-9_-]+$/.test(text)) throw new Error(`${label} is invalid.`);
  return text;
}

function requiredOpaqueString(value, label, maximum) {
  if (typeof value !== "string" || !value || value.length > maximum || CONTROL_CHARACTERS.test(value)) {
    throw new Error(`${label} is invalid.`);
  }
  return value;
}

function requiredPane(value, label) {
  if (value !== "primary" && value !== "secondary") throw new Error(`${label} is invalid.`);
  return value;
}

function optionalOpaqueString(value, label, maximum) {
  if (value === undefined || value === "") return "";
  return requiredOpaqueString(value, label, maximum);
}

function requiredText(value, label, maximum, allowEmpty = false) {
  if (typeof value !== "string" || value.length > maximum || CONTROL_CHARACTERS.test(value) || value.trim() !== value) {
    throw new Error(`${label} is invalid.`);
  }
  if (!allowEmpty && !value) throw new Error(`${label} is required.`);
  return value;
}

function requiredPath(value, label, maximum, allowEmpty = false) {
  if (typeof value !== "string" || value.length > maximum || CONTROL_CHARACTERS.test(value) || (!allowEmpty && !value)) {
    throw new Error(`${label} is invalid.`);
  }
  return value;
}

function nonNegativeSafeInteger(value, label) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${label} is invalid.`);
  return value;
}

function positiveSafeInteger(value, label) {
  if (!Number.isSafeInteger(value) || value < 1) throw new Error(`${label} is invalid.`);
  return value;
}

function paneTransferReference(transfer) {
  return {
    schema: CLIENT_PANE_TRANSFER_SCHEMA,
    schemaVersion: CLIENT_PANE_TRANSFER_SCHEMA_VERSION,
    sourceWindowId: transfer.sourceWindowId,
    transferId: transfer.transferId,
  };
}

function paneTransferCompletion(transfer) {
  return {
    ...paneTransferReference(transfer),
    sourcePane: transfer.sourcePane,
    sourceTabId: transfer.sourceTabId,
    target: copyBootstrap(transfer.target),
  };
}

function paneTransferSourceKey(webContentsId, sourceTabId) {
  return `${webContentsId}\0${sourceTabId}`;
}

function copyBootstrap(value) {
  return JSON.parse(JSON.stringify(value));
}

module.exports = {
  CLIENT_PANE_TRANSFER_SCHEMA,
  CLIENT_PANE_TRANSFER_SCHEMA_VERSION,
  CLIENT_PANE_TRANSFER_TIMEOUT_MS,
  CLIENT_WINDOW_READY_TIMEOUT_MS,
  CLIENT_WINDOW_SCHEMA,
  CLIENT_WINDOW_SCHEMA_VERSION,
  cascadeClientWindowBounds,
  createClientWindowRegistry,
  validateClientWindowOpenRequest,
  validateTransferableWorkspacePaneTarget,
};
