'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const root = path.resolve(__dirname, '../../../../../../../');
if (!process.versions.electron) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'octopus-standard-native-'));
  const env = { ...process.env }; delete env.ELECTRON_RUN_AS_NODE;
  try {
    const executable = require(path.join(root, 'app/node_modules/electron'));
    const result = require('node:child_process').spawnSync(executable, [__filename, directory], { env, stdio: 'inherit', timeout: 45000 });
    assert.ifError(result.error); assert.equal(result.status, 0, 'Stock Electron lifecycle fixture failed');
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
} else {
  const electron = require('electron'), { app } = electron;
  const { createStandardEngine } = require('../standard-engine.cjs');
  app.setPath('userData', path.join(process.argv[2], 'profile'));
  app.on('window-all-closed', () => {});
  const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
  async function until(read, expected) { const end = Date.now() + 5000; do { if (await read() === expected) return; await delay(20); } while (Date.now() < end); assert.equal(await read(), expected); }
  const fatal = error => { console.error(error); app.exit(1); };
  process.on('uncaughtException', fatal); process.on('unhandledRejection', fatal);
  setTimeout(() => fatal(Error('Native lifecycle fixture timed out')), 35000).unref();
  app.whenReady().then(async () => {
    app.dock?.hide(); assert.equal(electron.webContents.createOwnedGuest, undefined);
    const requests = [], pages = [], errors = [], engine = createStandardEngine(electron);
    const server = require('node:http').createServer((request, response) => {
      const data = []; request.on('data', bytes => data.push(bytes)); request.on('end', () => {
        requests.push({ url: request.url, method: request.method, referrer: request.headers.referer, type: request.headers['content-type'], cookie: request.headers.cookie, body: Buffer.concat(data).toString() });
        response.setHeader('content-type', 'text/html');
        response.end(`<body style="margin:0;background:rgb(16,32,48);height:3000px"><a href="/new" style="position:absolute;left:20px;top:20px;width:80px;height:80px">new</a><button style="position:absolute;left:800px;top:500px;width:100px;height:100px;border:0;background:rgb(255,0,0)" onclick="clicked++"></button><form action="/post" method="post" target="_blank"><input name="proof" value="kept"></form><script>window.clicked=0;window.reply=null;onmessage=e=>reply=e.data</script>`);
      });
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve)); const origin = `http://127.0.0.1:${server.address().port}`;
    const session = electron.session.fromPartition('stock-native-fixture');
    const preferences = { session, sandbox: true, contextIsolation: true, nodeIntegration: false, backgroundThrottling: false };
    await session.cookies.set({ url: origin, name: 'fixture', value: 'isolated' });
    const page = engine.create(preferences); pages.push(page); page.debugger.attach('1.3'); await page.loadURL(origin);
    const capture = async (width, height) => { const result = await engine.command(page, 'Page.captureScreenshot', { format: 'png', fromSurface: true }); const bytes = Buffer.from(result.data, 'base64'); assert.equal(bytes.readUInt32BE(16), width); assert.equal(bytes.readUInt32BE(20), height); return electron.nativeImage.createFromBuffer(bytes); };
    await capture(1280, 720);
    await engine.setViewport(page, { width: 1000, height: 800 }); await capture(1000, 800);
    const window = new electron.BrowserWindow({ show: false, width: 900, height: 700 });
    const attachment = { target: { contents: page }, window, sender: window.webContents };
    const rect = { x: 40, y: 30, width: 640, height: 480 }, packet = { scale: 1, visualBounds: rect, stageBounds: rect };
    engine.mount(attachment); await engine.layout(attachment, packet); engine.visible(attachment, true);
    assert.deepEqual(await page.executeJavaScript('({width:innerWidth,height:innerHeight})'), { width: 1000, height: 800 });
    const pixel = image => [...image.toBitmap().subarray(0, 3)];
    await page.executeJavaScript('new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))');
    assert.deepEqual(pixel((await capture(1000, 800)).crop({ x: 850, y: 550, width: 1, height: 1 })), [0, 0, 255]);
    assert.deepEqual(pixel((await page.capturePage()).crop({ x: 510, y: 330, width: 1, height: 1 })), [0, 0, 255]);
    await engine.command(page, 'Emulation.setFocusEmulationEnabled', { enabled: true });
    for (const type of ['mousePressed', 'mouseReleased']) await engine.command(page, 'Input.dispatchMouseEvent', { type, x: 850, y: 550, button: 'left', clickCount: 1 });
    await until(() => page.executeJavaScript('clicked'), 1);
    for (const type of ['mouseDown', 'mouseUp']) page.sendInputEvent({ type, x: 510, y: 330, button: 'left', clickCount: 1 });
    await until(() => page.executeJavaScript('clicked'), 2);
    const held = engine.command(page, 'Runtime.evaluate', { expression: 'new Promise(resolve=>setTimeout(resolve,50))', awaitPromise: true });
    let admitted = true;
    const cancelled = engine.command(page, 'Input.dispatchMouseEvent', { type: 'mousePressed', x: 850, y: 550, button: 'left' }, undefined, () => assert(admitted, 'Invocation cancelled'));
    admitted = false; await assert.rejects(cancelled, /Invocation cancelled/); await held;
    await engine.command(page, 'Input.dispatchMouseEvent', { type: 'mouseMoved', x: 200, y: 200 });
    await engine.command(page, 'Input.dispatchMouseEvent', { type: 'mouseWheel', x: 200, y: 200, deltaX: 0, deltaY: 100 });
    await until(() => page.executeJavaScript('scrollY'), 100); await page.executeJavaScript('scrollTo(0,0)');
    const backgroundPixel = async () => pixel((await capture(1000, 800)).crop({ x: 400, y: 300, width: 1, height: 1 }));
    const beforeHide = await backgroundPixel(); engine.visible(attachment, false); await page.executeJavaScript("document.body.style.background='rgb(80,96,112)'");
    const hiddenPixel = await backgroundPixel(); assert.notDeepEqual(hiddenPixel, beforeHide);
    engine.release(attachment); window.destroy(); await page.executeJavaScript("document.body.style.background='rgb(128,144,160)'");
    assert.notDeepEqual(await backgroundPixel(), hiddenPixel);
    await engine.setViewport(page, null); await capture(640, 480);
    const children = [];
    page.setWindowOpenHandler(details => ({ action: 'allow', outlivesOpener: true, overrideBrowserWindowOptions: { webPreferences: preferences }, createWindow: options =>
      engine.createPopup(preferences, options, details, contents => { pages.push(contents); children.push({ contents, supplied: !!options.webContents, url: details.url }); assert.equal(contents.session, session); if (options.webContents) assert.equal(contents, options.webContents); }, error => errors.push(error)) }));
    await page.executeJavaScript(`window.child=window.open('${origin}/child','fixture-child');true`, true);
    await until(() => requests.some(row => row.url === '/child'), true); assert.equal(children[0].supplied, true);
    await children[0].contents.executeJavaScript(`opener.postMessage('retained-opener','${origin}')`); await until(() => page.executeJavaScript('reply'), 'retained-opener');
    await page.executeJavaScript('document.forms[0].submit();true', true); await until(() => requests.some(row => row.url === '/post'), true);
    const post = requests.find(row => row.url === '/post'); assert.equal(post.method, 'POST'); assert.equal(post.body, 'proof=kept'); assert.equal(post.referrer, origin + '/'); assert.equal(post.cookie, 'fixture=isolated');
    for (const type of ['mouseDown', 'mouseUp']) page.sendInputEvent({ type, x: 50, y: 50, button: 'middle', clickCount: 1 });
    await until(() => requests.some(row => row.url === '/new'), true); assert.equal(children.find(row => row.url === origin + '/new').supplied, false);
    const linked = requests.find(row => row.url === '/new'); assert.equal(linked.referrer, origin + '/'); assert.equal(linked.cookie, 'fixture=isolated');
    await children[0].contents.executeJavaScript('window.close()').catch(() => {}); await until(() => children[0].contents.isDestroyed(), true);
    assert.deepEqual(errors, []); await Promise.all(pages.map(contents => engine.destroy(contents))); engine.stop();
    await new Promise(resolve => server.close(resolve)); assert.equal(electron.webContents.getAllWebContents().length, 0);
    console.log(JSON.stringify({ passed: true, electron: process.versions.electron, chromium: process.versions.chrome, freshHiddenAndDetachedPixels: true, modelViewport: '1000x800 in 640x480', nativeAndModelInput: true, cancelledQueuedInput: true, retainedPopup: true, postAndReferrer: true, newTabWithoutChild: true })); app.exit(0);
  }).catch(fatal);
}
