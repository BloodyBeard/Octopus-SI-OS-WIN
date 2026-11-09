const assert = require("node:assert/strict");
const { EventEmitter } = require("node:events");
const test = require("node:test");
const { createClientBrowser } = require("../electron/clientBrowser.cjs");

test("client links own isolated tabs, stay inside their window, and revoke access without granting model control", () => {
  const views = [], sent = [];
  const sender = Object.assign(new EventEmitter(), { isDestroyed: () => false, getZoomFactor: () => 1.5, send: (...args) => sent.push(args) });
  const window = { webContents: sender, isDestroyed: () => false, getContentBounds: () => ({ width: 1200, height: 900 }),
    contentView: { addChildView() {}, removeChildView() {} } };
  let home = "one", unlocked = true;
  class View {
    constructor(options) {
      this.options = options; this.visible = false; views.push(this);
      this.webContents = Object.assign(new EventEmitter(), {
        url: "", destroyed: false, getURL() { return this.url; }, getTitle: () => "Example", isLoading: () => false,
        isDestroyed() { return this.destroyed; }, close() { this.destroyed = true; },
        navigationHistory: { canGoBack: () => false, canGoForward: () => false },
        setWindowOpenHandler(handler) { this.openWindow = handler; }, loadURL(url) { this.url = url; return Promise.resolve(); },
      });
    }
    setVisible(value) { this.visible = value; }
    setBounds(bounds) { this.bounds = bounds; }
  }
  const api = createClientBrowser({ electron: { WebContentsView: View, BrowserWindow: { fromWebContents: () => window },
    session: { fromPartition: () => ({ setPermissionRequestHandler() {}, setPermissionCheckHandler() {} }) } },
    getHome: () => home, requireAccess: token => { if (!unlocked || token !== "access") throw new Error("locked"); } });
  const act = request => api.handle("access", sender, request);
  for (const url of ["javascript:alert(1)", "file:///secret", "https://user:password@example.test"]) {
    assert.throws(() => act({ action: "open", url }));
  }
  const page = act({ action: "open", url: "https://example.test/" });
  assert.equal(views.length, 1); assert.equal(views[0].visible, false);
  assert.equal(views[0].options.webPreferences.nodeIntegration, false);
  assert.equal(views[0].options.webPreferences.sandbox, true);
  assert.throws(() => api.handle("access", {}, { action: "state", id: page.id }), /another window/);
  act({ action: "layout", id: page.id, visible: true, bounds: { x: 20, y: 100, width: 400, height: 300 } });
  assert.deepEqual(views[0].bounds, { x: 30, y: 150, width: 600, height: 450 });
  assert.equal(views[0].visible, true);
  assert.throws(() => act({ action: "layout", id: page.id, visible: true, bounds: { x: -1, y: 0, width: 400, height: 300 } }), /outside/);
  act({ action: "navigate", id: page.id, url: "https://second.test/" });
  assert.equal(views[0].webContents.url, "https://second.test/");
  const popup = views[0].webContents.openWindow({ url: "https://popup.test/" });
  assert.equal(popup.action, "allow");
  const popupContents = popup.createWindow({});
  assert.equal(popupContents, views[1].webContents);
  assert.equal(popupContents.url, "", "Electron must retain the original popup navigation, including POST data");
  assert.equal(sent.at(-1)[0], "octopus:client-browser-opened");
  assert.equal(sent.at(-1)[1].url, "https://popup.test/");
  assert.equal(views[0].webContents.openWindow({ url: "file:///secret" }).action, "deny");
  unlocked = false;
  api.handle(null, sender, { action: "layout", id: page.id, visible: false });
  assert.equal(views[0].visible, false);
  assert.throws(() => act({ action: "navigate", id: page.id, url: "https://third.test/" }), /locked/);
  unlocked = true; home = "two";
  assert.throws(() => act({ action: "state", id: page.id }), /another Home/);
  assert.equal(views[0].webContents.destroyed, true);
  const next = act({ action: "open", url: "https://example.test/" });
  assert.equal(popupContents.destroyed, true);
  assert.equal(sender.listenerCount("did-start-navigation"), 1);
  sender.emit("did-start-navigation", {}, "https://app.test", false, true);
  assert.equal(views[2].webContents.destroyed, true);
  assert.throws(() => act({ action: "state", id: next.id }), /closed/);
  assert.equal(act({ action: "close", id: next.id }), null);
});
