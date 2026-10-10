'use strict';

function check(value, message, code = 'standard_engine_unavailable') { if (!value) throw Object.assign(new Error(message), { code }); }

function createStandardEngine(electron) {
  check(typeof electron.WebContentsView === 'function', 'Standard browser mode requires Electron WebContentsView');
  const views = new WeakMap();
  let backgroundWindow = null;
  function background() {
    if (!backgroundWindow || backgroundWindow.isDestroyed()) backgroundWindow = new electron.BaseWindow({ show: false, width: 1280, height: 720, focusable: false, skipTaskbar: true });
    return backgroundWindow;
  }
  function move(record, window, bounds) {
    record.view.setVisible(false);
    if (record.window !== window) {
      if (record.window && !record.window.isDestroyed()) record.window.contentView.removeChildView(record.view);
      window.contentView.addChildView(record.view);
      record.window = window;
    }
    record.view.setBounds(bounds);
    record.view.setVisible(true);
  }
  function park(record) { move(record, background(), { ...record.bounds, x: 0, y: 0 }); }
  function create(preferences, contents) {
    const view = new electron.WebContentsView({ webPreferences: preferences, ...(contents ? { webContents: contents } : {}) });
    check(!contents || view.webContents === contents, 'Standard popup lost its native WebContents identity');
    check(view.webContents.session === preferences.session, 'Standard browser page has a foreign Session');
    const record = { view, attachment: null, window: null, bounds: { x: 0, y: 0, width: 1280, height: 720 }, viewport: null, scale: 1, metricsKnown: true, chain: Promise.resolve() };
    views.set(view.webContents, record);
    park(record);
    return view.webContents;
  }
  function createPopup(preferences, options, details, register, failed) {
    const contents = create({ ...options.webPreferences, ...preferences }, options.webContents);
    register(contents);
    if (!options.webContents) {
      const post = details.postBody;
      contents.loadURL(details.url, { httpReferrer: details.referrer, ...(post ? { postData: post.data,
        extraHeaders: `content-type: ${post.contentType}${post.boundary ? `; boundary=${post.boundary}` : ''}` } : {}) }).catch(failed);
    }
    return contents;
  }
  function entry(contents) {
    const record = views.get(contents);
    check(record && !contents.isDestroyed(), 'Standard browser page is unavailable');
    return record;
  }
  function ordered(contents, action) {
    const record = entry(contents);
    const task = record.chain.then(() => { entry(contents); return action(record); });
    record.chain = task.then(() => {}, () => {});
    return task;
  }
  async function metrics(contents, record, viewport) {
    const scale = viewport ? Math.min(1, record.bounds.width / viewport.width, record.bounds.height / viewport.height) : 1;
    record.metricsKnown = false;
    await contents.debugger.sendCommand(viewport ? 'Emulation.setDeviceMetricsOverride' : 'Emulation.clearDeviceMetricsOverride',
      viewport ? { ...viewport, deviceScaleFactor: 1, mobile: false, scale } : {});
    entry(contents);
    record.viewport = viewport ? { ...viewport } : null;
    record.scale = scale;
    record.metricsKnown = true;
  }
  function setViewport(contents, viewport, validate = () => {}) { return ordered(contents, record => { validate(); return metrics(contents, record, viewport); }); }
  function command(contents, method, params, sessionId, validate = () => {}) {
    return ordered(contents, record => {
      validate();
      if (method.startsWith('Input.') || method === 'Page.captureScreenshot') check(record.metricsKnown, 'Standard browser viewport is unconfirmed', 'viewport_unconfirmed');
      let input = params;
      if (record.scale !== 1 && ['Input.dispatchMouseEvent', 'Input.synthesizeScrollGesture'].includes(method)) {
        input = { ...params };
        for (const key of ['x', 'y']) if (typeof input[key] === 'number') input[key] *= record.scale;
      }
      return contents.debugger.sendCommand(method, input, sessionId);
    });
  }
  function attached(attachment) { return entry(attachment.target.contents).attachment === attachment; }
  function mount(attachment) {
    const record = entry(attachment.target.contents);
    check(!record.attachment, 'Standard browser page still belongs to another presentation');
    check(attachment.window.contentView && typeof attachment.window.contentView.addChildView === 'function', 'Client window has no native content view');
    record.attachment = attachment;
  }
  function release(attachment) {
    const record = views.get(attachment.target.contents);
    if (!record || record.attachment !== attachment) return;
    record.attachment = null;
    if (!attachment.target.contents.isDestroyed()) park(record);
  }
  function visible(attachment, value) {
    check(attached(attachment), 'Standard browser view belongs to another presentation');
    const record = entry(attachment.target.contents);
    if (value) move(record, attachment.window, record.bounds);
    else park(record);
  }
  async function layout(attachment, packet, validate = () => {}) {
    const record = entry(attachment.target.contents);
    check(record.attachment === attachment, 'Standard browser layout belongs to another presentation');
    park(record);
    check(packet.scale === 1, 'Standard browser mode does not support scaled guest presentation');
    const zoom = attachment.sender.getZoomFactor();
    check(Number.isFinite(zoom) && zoom > 0, 'Client zoom is unavailable');
    const rect = packet.visualBounds, stage = packet.stageBounds;
    check(rect.x >= stage.x && rect.y >= stage.y && rect.x + rect.width <= stage.x + stage.width + 1
      && rect.y + rect.height <= stage.y + stage.height + 1, 'Standard browser view escapes its presentation stage');
    const bounds = { x: Math.round(rect.x * zoom), y: Math.round(rect.y * zoom), width: Math.round(rect.width * zoom), height: Math.round(rect.height * zoom) };
    const content = attachment.window.getContentBounds();
    check(bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= content.width + 1 && bounds.y + bounds.height <= content.height + 1,
      'Standard browser view escapes its client window');
    if (bounds.width <= 0 || bounds.height <= 0) return;
    await ordered(attachment.target.contents, async () => {
      validate();
      check(record.attachment === attachment, 'Standard browser layout belongs to another presentation');
      record.bounds = bounds;
      park(record);
      if (record.viewport) await metrics(attachment.target.contents, record, record.viewport);
    });
  }
  function destroy(contents) {
    const record = views.get(contents);
    if (record?.closing) return record.closing;
    if (record?.window && !record.window.isDestroyed()) record.window.contentView.removeChildView(record.view);
    if (record) record.attachment = null;
    if (contents.isDestroyed()) { views.delete(contents); return Promise.resolve(); }
    const closing = new Promise((resolve, reject) => {
      const done = () => { clearTimeout(timer); views.delete(contents); resolve(); };
      const timer = setTimeout(() => { contents.removeListener('destroyed', done); reject(new Error('Standard browser page destruction is unconfirmed')); }, 10000);
      contents.once('destroyed', done);
      try { contents.close({ waitForBeforeUnload: false }); }
      catch (error) { clearTimeout(timer); contents.removeListener('destroyed', done); reject(error); }
    });
    if (record) record.closing = closing;
    return closing;
  }
  function stop() { if (backgroundWindow && !backgroundWindow.isDestroyed()) backgroundWindow.destroy(); backgroundWindow = null; }
  return { create, createPopup, owns: contents => views.has(contents), attached, mount, release, visible, layout, setViewport, command, destroy, stop };
}

module.exports = { createStandardEngine };
