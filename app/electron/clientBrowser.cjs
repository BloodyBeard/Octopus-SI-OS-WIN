const { randomUUID, createHash } = require("node:crypto");

function webUrl(value) {
  if (typeof value !== "string" || value.length > 16384) throw new Error("Invalid web address.");
  const url = new URL(value);
  if (!["https:", "http:"].includes(url.protocol) || url.username || url.password) throw new Error("Only HTTP and HTTPS web addresses can open here.");
  return url.href;
}

function createClientBrowser({ electron, getHome, requireAccess }) {
  const pages = new Map(), owners = new Map();
  function state(page) {
    const contents = page.view.webContents;
    return { id: page.id, url: contents.getURL() || page.url, title: contents.getTitle() || new URL(page.url).hostname,
      loading: contents.isLoading(), canGoBack: contents.navigationHistory.canGoBack(),
      canGoForward: contents.navigationHistory.canGoForward(), error: page.error };
  }
  function close(page) {
    if (!pages.delete(page.id)) return;
    page.owner.pages.delete(page);
    if (!page.owner.window.isDestroyed()) page.owner.window.contentView.removeChildView(page.view);
    if (!page.view.webContents.isDestroyed()) page.view.webContents.close();
  }
  function ownerFor(token, sender) {
    requireAccess(token);
    const home = getHome(), window = electron.BrowserWindow.fromWebContents(sender);
    if (!home || !window || window.isDestroyed() || sender.isDestroyed() || window.webContents !== sender) throw new Error("Browser requires its owning client window.");
    let owner = owners.get(sender);
    if (owner && (owner.home !== home || owner.token !== token)) {
      owner.dispose(); owner = null;
    }
    if (!owner) {
      const session = electron.session.fromPartition("octopus-client-browser:" + createHash("sha256").update(home).digest("hex") + ":" + randomUUID());
      session.setPermissionRequestHandler((_contents, _permission, callback) => callback(false));
      session.setPermissionCheckHandler(() => false);
      owner = { home, token, sender, window, session, pages: new Set() };
      owners.set(sender, owner);
      const dispose = () => {
        if (owners.get(sender) !== owner) return;
        for (const page of [...owner.pages]) close(page);
        owners.delete(sender);
        sender.removeListener("destroyed", dispose);
        sender.removeListener("did-start-navigation", navigating);
      };
      const navigating = (_event, _url, inPlace, mainFrame) => { if (mainFrame && !inPlace) dispose(); };
      owner.dispose = dispose;
      sender.once("destroyed", dispose); sender.on("did-start-navigation", navigating);
    }
    return owner;
  }
  function publish(page) {
    if (!pages.has(page.id) || page.owner.sender.isDestroyed() || page.view.webContents.isDestroyed()) return;
    try { requireAccess(page.owner.token); if (page.owner.home !== getHome()) { close(page); return; } }
    catch { page.view.setVisible(false); return; }
    page.owner.sender.send("octopus:client-browser-state", state(page));
  }
  function open(owner, url, navigateImmediately = true) {
    const view = new electron.WebContentsView({ webPreferences: { session: owner.session, sandbox: true,
      nodeIntegration: false, nodeIntegrationInSubFrames: false, contextIsolation: true, webSecurity: true,
      allowRunningInsecureContent: false, webviewTag: false, navigateOnDragDrop: false } });
    const page = { id: randomUUID(), owner, view, url, error: "" };
    pages.set(page.id, page); owner.pages.add(page);
    owner.window.contentView.addChildView(view); view.setVisible(false);
    const contents = view.webContents;
    const navigate = (event, value) => { try { webUrl(value); } catch { event.preventDefault(); } };
    contents.on("will-navigate", navigate);
    contents.on("will-redirect", navigate);
    contents.on("will-attach-webview", event => event.preventDefault());
    contents.setWindowOpenHandler(({ url: next }) => {
      try {
        requireAccess(owner.token);
        if (owner.home !== getHome() || owner.sender.isDestroyed()) return { action: "deny" };
        const url = webUrl(next);
        return { action: "allow", outlivesOpener: true, createWindow: () => {
          requireAccess(owner.token);
          if (owner.home !== getHome() || owner.sender.isDestroyed()) throw new Error("Browser window changed before opening the tab.");
          const child = open(owner, url, false);
          owner.sender.send("octopus:client-browser-opened", state(child));
          return child.view.webContents;
        } };
      } catch (error) { page.error = error.message; publish(page); }
      return { action: "deny" };
    });
    contents.on("did-start-loading", () => { page.error = ""; publish(page); });
    for (const event of ["did-stop-loading", "did-navigate", "did-navigate-in-page", "page-title-updated"]) contents.on(event, () => publish(page));
    contents.on("did-fail-load", (_event, code, description, _url, mainFrame) => {
      if (mainFrame && code !== -3) { page.error = description; publish(page); }
    });
    contents.on("render-process-gone", () => { page.error = "This page stopped responding. Reload to try again."; view.setVisible(false); publish(page); });
    if (navigateImmediately) void contents.loadURL(url).catch(error => { if (pages.has(page.id)) { page.error = error.message; publish(page); } });
    return page;
  }
  return {
    handle(token, sender, request) {
      if (!request || typeof request !== "object" || Array.isArray(request)) throw new Error("Invalid browser request.");
      if (request.action === "open") {
        if (Object.keys(request).some(key => !["action", "url"].includes(key))) throw new Error("Invalid browser open request.");
        const url = webUrl(request.url);
        return state(open(ownerFor(token, sender), url));
      }
      const page = pages.get(request.id);
      if (!page && request.action === "close") return null;
      if (!page || page.owner.sender !== sender || page.owner.window.isDestroyed()) throw new Error("Browser tab belongs to another window or is closed.");
      const cleanup = request.action === "close" || request.action === "layout" && request.visible === false;
      if (!cleanup) {
        if (page.owner.token !== token) throw new Error("Browser tab belongs to another client access session.");
        requireAccess(token);
        if (page.owner.home !== getHome()) { close(page); throw new Error("Browser tab belongs to another Home."); }
      }
      const contents = page.view.webContents;
      if (request.action === "close") { close(page); return null; }
      if (request.action === "layout") {
        page.view.setVisible(false);
        if (request.visible === false) return state(page);
        const bounds = request.bounds, zoom = sender.getZoomFactor(), window = page.owner.window.getContentBounds();
        if (!bounds || !["x", "y", "width", "height"].every(key => Number.isFinite(bounds[key])) || bounds.width <= 0 || bounds.height <= 0) throw new Error("Invalid browser viewport.");
        const x = Math.round(bounds.x * zoom), y = Math.round(bounds.y * zoom),
          width = Math.round(bounds.width * zoom), height = Math.round(bounds.height * zoom);
        if (x < 0 || y < 0 || x + width > window.width + 1 || y + height > window.height + 1) throw new Error("Browser viewport is outside its client window.");
        page.view.setBounds({ x, y, width, height }); page.view.setVisible(true);
      } else if (request.action === "navigate") {
        page.url = webUrl(request.url); page.error = "";
        void contents.loadURL(page.url).catch(error => { page.error = error.message; publish(page); });
      } else if (request.action === "back") { if (contents.navigationHistory.canGoBack()) contents.navigationHistory.goBack(); }
      else if (request.action === "forward") { if (contents.navigationHistory.canGoForward()) contents.navigationHistory.goForward(); }
      else if (request.action === "reload") contents.reload();
      else if (request.action === "stop") contents.stop();
      else if (request.action !== "state") throw new Error("Unsupported browser action.");
      return state(page);
    },
  };
}
module.exports = { createClientBrowser, webUrl };
