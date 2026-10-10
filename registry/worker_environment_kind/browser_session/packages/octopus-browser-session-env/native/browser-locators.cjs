'use strict';

const { EventEmitter } = require('node:events');
const { playwrightDomSnapshot } = require('./browser-aria-snapshot.cjs');
const crypto = require('node:crypto');
const { clipboardItems } = require('./browser-rich-text.cjs');
const { waitForPageLoad, waitForLoadState, waitForUrl } = require('./browser-load-wait.cjs');
const { LocatorRuntime, evaluateReadonly, Clipboard, keypress, character, clipboardChord, pastePage, commitCutPage, credentialFieldPolicy, visibleDom, domPoint } = require('./browser-primitives.cjs');

const FAMILY = new Set(['wait', 'locator_query', 'locator_fill', 'locator_press', 'locator_select', 'locator_check', 'locator_wait', 'pointer_move', 'paste', 'clipboard_read', 'clipboard_write', 'evaluate_readonly', 'media_download', 'dom_snapshot', 'playwright_dom_snapshot', 'dom_click', 'dom_scroll']);
const PAGE_EDIT_COMMANDS = new Set(['selectAll', 'deleteBackward', 'deleteForward', 'undo', 'redo']);
function fail(message) { throw Object.assign(new Error(message), { code: 'browser_operation_invalid' }); }
function object(value, allowed, required = []) {
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).some(key => !allowed.includes(key)) || required.some(key => !Object.hasOwn(value, key))) fail('Browser operation fields do not match its declared contract');
  return value;
}
function string(value, name, maximum = 1048576, empty = false) { if (typeof value !== 'string' || (!empty && !value.length) || value.length > maximum) fail(`${name} must be a bounded string`); return value; }
function boolean(value, name, defaultValue = false) { if (value === undefined) return defaultValue; if (typeof value !== 'boolean') fail(`${name} must be boolean`); return value; }
function positive(value, name, maximum) { if (!Number.isSafeInteger(value) || value < 1 || value > maximum) fail(`${name} must be a positive integer at most ${maximum}`); return value; }
function finite(value, name) { if (typeof value !== 'number' || !Number.isFinite(value)) fail(`${name} must be finite`); return value; }
function resultValue(result) { if (result.exceptionDetails) fail(result.exceptionDetails.exception?.description || result.exceptionDetails.text || 'Page evaluation failed'); if (!result.result || !Object.hasOwn(result.result, 'value')) fail('Page operation returned no value'); return result.result.value; }
function timeoutFor(args, maximum = 3000) { if (args.timeout_ms === undefined) return maximum; if (!Number.isSafeInteger(args.timeout_ms) || args.timeout_ms < 0 || args.timeout_ms > 300000) fail('timeout_ms must be a bounded nonnegative integer'); return Math.min(args.timeout_ms, maximum); }
function point(value) { object(value, ['x', 'y'], ['x', 'y']); return { x: finite(value.x, 'x'), y: finite(value.y, 'y') }; }
function modifiers(value) { if (value === undefined) return []; if (!Array.isArray(value) || value.some(v => !['Alt', 'Control', 'ControlOrMeta', 'Meta', 'Shift'].includes(v))) fail('Invalid page modifier'); return [...value]; }
function keyCommands(params) {
  if (Object.hasOwn(params, 'nativeVirtualKeyCode')) return false;
  if (!Object.hasOwn(params, 'commands')) return true;
  return Array.isArray(params.commands) && params.commands.length <= 1 && params.commands.every(command => PAGE_EDIT_COMMANDS.has(command)) && ['keyDown', 'rawKeyDown'].includes(params.type);
}

function roomClipboard(room, maximum) {
  positive(maximum, 'max_clipboard_bytes', 16 * 1024 * 1024);
  if (room.clipboardByteLimit !== undefined && room.clipboardByteLimit !== maximum) fail('Virtual clipboard limit changed within its formed room');
  room.clipboardByteLimit = maximum;
  if (!room.pageClipboard) {
    room.pageClipboard = new Clipboard();
    const write = room.pageClipboard.write.bind(room.pageClipboard);
    room.pageClipboard.write = (items, operation) => {
      if (Buffer.byteLength(JSON.stringify(items)) > maximum) fail('Virtual clipboard exceeds the admitted room byte limit');
      return write(items, operation);
    };
  }
  return room.pageClipboard;
}

function createBrowserFamily({ target, limits, pageEditingProfile, pageUrl, call, validate, inventory, cursor, clipboardCommand, onClipboardEvent, registerClipboardCleanup, onPageEvent }) {
  const emitter = new EventEmitter();
  const tabId = target.contents.id;
  const frames = new Map();
  const inputTargets = new Map();
  const removePageEvents = onPageEvent(event => emitter.emit(event.type, event.value));
  const cleanupHandlers = [];
  const timing = { startLocatorRetry() { const evidence = { attempts: 0, outcome: null }; return { attemptFailed() { evidence.attempts++; }, finish(outcome) { evidence.outcome = outcome; } }; } };
  const cdp = Object.assign(emitter, {
    platform: 'darwin',
    addTabCleanupHandler(listener) { const remove = registerClipboardCleanup(listener); cleanupHandlers.push(remove); return remove; },
    async call(id, method, params, options) { if (id !== tabId) fail('Browser operation attempted another page'); return this.callTarget({ tabId }, method, params, options); },
    async callTarget(route, method, params = {}, options = {}) {
      validate(); if (route.tabId !== tabId) fail('Browser operation attempted another page');
      if (options.deadlineMs !== undefined && Date.now() >= options.deadlineMs) fail('Browser operation deadline reached before dispatch');
      let sessionId = route.sessionId ?? null;
      if (route.targetId !== undefined) {
        const members = inventory().filter(row => row.target_id === route.targetId);
        if (members.length !== 1 || (sessionId !== null && sessionId !== members[0].session_id)) fail('Selector child target is not an exact owned attachment');
        sessionId = members[0].session_id;
      }
      if (Object.hasOwn(params, '__codexIabExpectedInputTargetToken')) {
        const token = params.__codexIabExpectedInputTargetToken, expected = inputTargets.get(token);
        if (!expected || expected.target.tabId !== tabId || (expected.target.sessionId ?? null) !== sessionId) fail('Page input lost its exact focused frame');
        const checked = await call(sessionId, 'Runtime.evaluate', { contextId: expected.executionContextId, expression: `(() => { const active = root => { const element = root.activeElement; if (!element) return null; if (element.shadowRoot) return active(element.shadowRoot) ?? element; if (['iframe','frame'].includes(element.localName)) { const doc = element.contentDocument; if (doc) return active(doc) ?? element; } return element; }; const element = active(document); if (!document.hasFocus() || element?.__codexIabInputTargetToken !== ${JSON.stringify(token)}) throw new Error('Active element is no longer the expected input target'); return true; })()`, returnByValue: true }, options);
        if (resultValue(checked) !== true) fail('Page input target validation failed');
        params = { ...params }; delete params.__codexIabExpectedInputTargetToken;
      }
      const result = await call(sessionId, method, params, options);
      validate(); return result;
    },
    async enableOopifAutoAttach(id, options = {}) {
      if (id !== tabId) fail('Iframe discovery attempted another page');
      const seen = new Set();
      for (let index = 0; index < 129; index++) {
        const route = [null, ...inventory().map(row => row.session_id)].find(id => !seen.has(id));
        if (route === undefined) break;
        seen.add(route);
        const target = { tabId, ...(route === null ? {} : { sessionId: route }) };
        await this.callTarget(target, 'Page.enable', {}, options);
        await this.callTarget(target, 'Runtime.enable', {}, options);
        await this.callTarget(target, 'Target.setAutoAttach', { autoAttach: true, flatten: true, waitForDebuggerOnStart: false, filter: [{ type: 'iframe', exclude: false }] }, options);
        const { frameTree } = await this.callTarget(target, 'Page.getFrameTree', {}, options);
        const queue = [frameTree];
        for (let i = 0; i < queue.length; i++) {
          const node = queue[i];
          if (i >= 512 || !node?.frame?.id) fail('Invalid owned frame tree');
          const existing = frames.get(node.frame.id);
          if (existing && existing.sessionId !== route && route === null) continue;
          frames.set(node.frame.id, { tabId, ...(route === null ? {} : { sessionId: route }) });
          queue.push(...(node.childFrames ?? []));
        }
      }
    },
    targetForFrame(id, frameId) { if (id !== tabId) fail('Iframe discovery attempted another page'); return frames.get(frameId) ?? null; },
    async targetForFrameOrAttach(id, frameId, options) { await this.enableOopifAutoAttach(id, options); const route = this.targetForFrame(id, frameId); if (!route) fail('Requested iframe has no actual owned debugger attachment'); return route; },
  });
  const loadWaits = [];
  const clickPoint = async ({ point: root, inputPoint, inputTarget, loadTarget, timeoutMs, button = 'left', clickCount = 1, modifiers: mask = 0 }) => {
    point(root); const position = point(inputPoint ?? root), route = inputTarget ?? { tabId };
    if (!['left', 'middle', 'right'].includes(button) || ![1, 2, 3].includes(clickCount)) fail('Invalid pointer click');
    const duration = timeoutFor({ timeout_ms: timeoutMs }), deadlineMs = Date.now() + duration;
    const child = loadTarget != null && (loadTarget.sessionId != null || loadTarget.targetId != null);
    const pending = child ? Promise.all([waitForPageLoad(emitter, loadTarget, { timeoutMs: duration }), waitForPageLoad(emitter, { tabId }, { timeoutMs: duration, rejectSubframeNavigationBlocked: true })]) : waitForPageLoad(emitter, { tabId }, { timeoutMs: duration }).then(result => [result]);
    const observed = pending.catch(() => {});
    try {
      await cursor(root.x, root.y, true);
      await cdp.callTarget(route, 'Input.dispatchMouseEvent', { type: 'mouseMoved', ...position, button: 'none', buttons: 0, modifiers: mask }, { deadlineMs });
      for (let count = 1; count <= clickCount; count++) {
        await cdp.callTarget(route, 'Input.dispatchMouseEvent', { type: 'mousePressed', ...position, button, buttons: { left: 1, right: 2, middle: 4 }[button], clickCount: count, modifiers: mask }, { deadlineMs });
        await cdp.callTarget(route, 'Input.dispatchMouseEvent', { type: 'mouseReleased', ...position, button, buttons: 0, clickCount: count, modifiers: mask }, { deadlineMs });
      }
    } catch (error) { await observed; throw error; }
    loadWaits.push(...await pending);
  };
  const locator = new LocatorRuntime(cdp, { clickPoint }, { currentPlaywrightOperation(value) { return value; } }, timing);
  const room = target.room;
  const clipboard = roomClipboard(room, limits.max_type_chars);
  async function clipboardFor(route) {
    if (!room.clipboardAdapter) {
      const events = new EventEmitter();
      room.clipboardAdapter = Object.assign(events, {
        callTarget: clipboardCommand,
        addTabCleanupHandler: registerClipboardCleanup,
      });
      onClipboardEvent(event => events.emit(event.type, event.value));
    }
    await clipboard.ensurePageClipboard(room.clipboardAdapter, route);
  }
  async function pageClipboard(route, options, contextId) {
    await clipboardFor(route);
    const result = await cdp.callTarget(route, 'Runtime.evaluate', {
      expression: `(${pastePage.toString()})(${JSON.stringify({ ...options, protectedCredentialFieldPolicy: credentialFieldPolicy })})`,
      ...(contextId === undefined ? {} : { contextId }), awaitPromise: true, returnByValue: true,
    });
    return resultValue(result);
  }
  async function chord(keys, route = { tabId }, token, contextId, deadlineMs) {
    if (typeof keys !== 'string' && (!Array.isArray(keys) || keys.length < 1 || keys.length > 16 || keys.some(key => typeof key !== 'string' || key.length > 64))) fail('keys must be a chord string or bounded key list');
    const action = clipboardChord(keys, 'darwin');
    if (action === 'blocked') fail('This chord is not a supported page clipboard operation');
    if (action === null) { await keypress(cdp, route, keys, { iabInputTargetToken: token, deadlineMs }); return; }
    await clipboard.runExclusive(async () => {
      let items = action.startsWith('paste') ? clipboard.read() : [];
      if (action === 'paste-plain-text') items = items.map(item => ({ ...item, entries: item.entries.filter(entry => entry.mime_type === 'text/plain') })).filter(item => item.entries.length);
      const cutToken = action === 'cut' ? crypto.randomUUID() : undefined;
      const data = await pageClipboard(route, { action: action.startsWith('paste') ? 'paste' : action, clipboardItems: items, iabInputTargetToken: token, ...(cutToken ? { cutToken } : {}) }, contextId);
      if (!action.startsWith('paste') && data.items) {
        clipboard.write(data.items, 'browser.key');
        if (action === 'cut' && data.cutToken) resultValue(await cdp.callTarget(route, 'Runtime.evaluate', { expression: `(${commitCutPage.toString()})(${JSON.stringify({ cutToken: data.cutToken })}); true`, ...(contextId === undefined ? {} : { contextId }), returnByValue: true }));
      }
    });
  }
  function richTextForPage() {
    if (!pageEditingProfile.rich_text_paste) return false;
    let url; try { url = new URL(pageUrl()); } catch { return true; }
    const first = url.pathname.split('/').filter(Boolean)[0];
    return !pageEditingProfile.plain_text_documents.some(document => document.host === url.host && document.first_path_segment === first);
  }
  async function paste(text, route = { tabId }, token, contextId, replace = false, richText = false) {
    const items = clipboardItems(text, richText);
    await clipboard.runExclusive(() => pageClipboard(route, { action: 'paste', clipboardItems: items, iabInputTargetToken: token, requireDocumentFocus: contextId !== undefined, replaceInputValue: replace, richTextFallback: richText }, contextId));
  }
  const base = ['target_id', 'selector', 'timeout_ms'];
  async function perform(operation, args, limits) {
    validate();
    const timed = ['wait', 'locator_query', 'locator_fill', 'locator_press', 'locator_select', 'locator_check', 'locator_wait', 'evaluate_readonly', 'media_download', 'playwright_dom_snapshot', 'click', 'dom_click'].includes(operation) || args.timeout_ms !== undefined;
    const timeoutMs = timed ? timeoutFor(args, operation === 'locator_press' && args.mode === 'sequential' ? 5000 : 3000) : null;
    const deadlineMs = timeoutMs === null ? undefined : Date.now() + timeoutMs;
    const options = { ...(timeoutMs === null ? {} : { timeoutMs, deadlineMs }), isolatedWorld: true };
    const selected = () => string(args.selector, 'selector');
    const input = () => ({ tab_id: tabId, selector: selected(), timeout_ms: timeoutMs });
    const evaluate = (fn, arg) => locator.evaluateOnPlaywrightSelector(tabId, selected(), fn, { ...options, arg });
    const evaluateAll = (fn, arg) => locator.evaluateOnPlaywrightSelectorAll(tabId, selected(), fn, { ...options, arg });
    let value, snapshotObservation;
    switch (operation) {
      case 'wait': {
        object(args, ['target_id', 'url_pattern', 'wait_until', 'load_state', 'timeout_ms']);
        if (Object.hasOwn(args, 'url_pattern')) {
          if (Object.hasOwn(args, 'load_state')) fail('Browser wait requires one condition');
          string(args.url_pattern, 'url_pattern', limits.max_eval_chars);
          const until = args.wait_until ?? 'commit';
          if (!['commit', 'load', 'domcontentloaded'].includes(until)) fail('Unsupported URL wait load state');
          value = await waitForUrl(cdp, tabId, args.url_pattern, until, timeoutMs);
        } else {
          if (Object.hasOwn(args, 'wait_until') || !['load', 'domcontentloaded'].includes(args.load_state)) fail('Browser wait requires one supported load state');
          const observations = [];
          value = { ...await waitForLoadState(cdp, tabId, args.load_state, timeoutMs, observations), observation_errors: observations };
        }
        break;
      }
      case 'locator_query': {
        object(args, [...base, 'query', 'attribute', 'relative_selector'], ['selector', 'query']);
        const query = args.query;
        if (args.relative_selector !== undefined) { if (query !== 'read_all') fail('relative_selector belongs only to read_all'); string(args.relative_selector, 'relative_selector'); }
        if (!['count', 'is_visible', 'is_enabled', 'all_text_contents', 'text_content', 'inner_text', 'get_attribute', 'read_all'].includes(query)) fail('Unknown locator query');
        if (query === 'get_attribute') string(args.attribute, 'attribute'); else if (Object.hasOwn(args, 'attribute')) fail('attribute belongs only to get_attribute');
        if (query === 'count') value = await evaluateAll(nodes => nodes.length);
        else if (query === 'all_text_contents') value = await evaluateAll(nodes => nodes.map(node => typeof node.textContent === 'string' ? node.textContent : ''));
        else if (query === 'is_visible' || query === 'is_enabled') value = await locator.readElementState(input(), query === 'is_visible' ? 'visible' : 'enabled');
        else if (query === 'read_all') value = await evaluateAll((nodes, injected, request) => {
          const read = node => ({ attributes: Object.fromEntries(Array.from(node.attributes).filter(attribute => attribute.name.toLowerCase() !== 'value' || !injected.isProtectedCredentialField(node)).map(attribute => [attribute.name, attribute.value])), inner_text: 'innerText' in node ? String(node.innerText) : '', text_content: node.textContent });
          if (!request.relative_selector) return nodes.map(read);
          const parsed = injected.parseSelector(request.relative_selector);
          return nodes.map(node => { const matches = injected.querySelectorAll(parsed, node); return matches[0] ? read(matches[0]) : null; });
        }, { relative_selector: args.relative_selector });
        else value = await evaluate((node, injected, request) => request.query === 'text_content' ? node.textContent : request.query === 'inner_text' ? ('innerText' in node ? String(node.innerText) : '') : request.attribute.toLowerCase() === 'value' && injected.isProtectedCredentialField(node) ? null : node.getAttribute(request.attribute), { query, attribute: args.attribute });
        break;
      }
      case 'locator_fill': {
        object(args, [...base, 'text', 'replace'], ['selector', 'text']); string(args.text, 'text', limits.max_type_chars, true);
        const token = crypto.randomUUID(), replace = boolean(args.replace, 'replace', true);
        if (!replace) {
          const focused = await locator.focusLocator(input(), { requireEditable: true, selectText: false, iabInputTargetToken: token });
          inputTargets.set(token, focused); await paste(args.text, focused.target, token, focused.executionContextId);
        } else {
          const focused = await locator.evaluateOnPlaywrightSelectorWithTarget(tabId, selected(), (node, injected, request) => {
            for (const state of ['visible', 'enabled', 'editable']) { const current = injected.elementState(node, state); if (current.received === 'error:notconnected') throw new Error('Element is not connected'); if (!current.matches) throw new Error('Element is not ' + state); }
            node.scrollIntoView({ block: 'center', inline: 'nearest' }); const result = injected.fill(node, request.text);
            if (result !== 'done' && result !== 'needsinput') throw new Error(String(result));
            if (result === 'needsinput') { const element = injected.retarget(node, 'follow-label'); if (!element) throw new Error('Element is not connected'); Object.defineProperty(element, '__codexIabInputTargetToken', { configurable: true, value: request.token, writable: true }); }
            return result;
          }, { ...options, arg: { text: args.text, token } });
          inputTargets.set(token, focused);
          if (focused.result === 'needsinput') await paste(args.text, focused.target, token, focused.executionContextId, true);
        }
        value = { filled: true }; break;
      }
      case 'locator_press': {
        object(args, [...base, 'mode', 'keys', 'text'], ['selector', 'mode']);
        if (!['chord', 'sequential'].includes(args.mode)) fail('Unknown locator press mode');
        const token = crypto.randomUUID();
        if (args.mode === 'chord') {
          if (Object.hasOwn(args, 'text')) fail('Chord does not accept text');
          const focused = await locator.focusLocator(input(), { requireEditable: false, iabInputTargetToken: token });
          inputTargets.set(token, focused); await chord(args.keys, focused.target, token, focused.executionContextId, deadlineMs);
        } else {
          if (Object.hasOwn(args, 'keys')) fail('Sequential typing does not accept keys'); string(args.text, 'text', limits.max_type_chars, true);
          const targetToken = crypto.randomUUID();
          const focused = await locator.evaluateOnPlaywrightSelectorWithTarget(tabId, selected(), (node, injected, request) => {
            for (const state of ['visible', 'enabled', 'editable']) { const current = injected.elementState(node, state); if (current.received === 'error:notconnected') throw new Error('Element is not connected'); if (!current.matches) throw new Error('Element is not ' + state); }
            node.scrollIntoView({ block: 'center', inline: 'nearest' });
            const element = injected.retarget(node, 'follow-label'); if (!element) throw new Error('Element is not connected');
            if (!element.matches(':focus')) { const result = injected.focusNode(element, false); if (result !== 'done') throw new Error(String(result)); }
            Object.defineProperty(element, '__codexPressSequentiallyTargetToken', { configurable: true, value: request.targetToken });
            Object.defineProperty(element, '__codexIabInputTargetToken', { configurable: true, value: request.inputToken, writable: true });
            return true;
          }, { ...options, arg: { targetToken, inputToken: token } });
          inputTargets.set(token, focused);
          const characters = Array.from(args.text);
          try {
            for (const [index, char] of characters.entries()) {
              validate(); if (Date.now() >= deadlineMs) fail('Sequential typing reached its 5000ms maximum');
              const current = await locator.evaluateOnPlaywrightSelectorWithTarget(tabId, selected(), (node, injected, request, scope) => {
                const element = injected.retarget(node, 'follow-label'); if (!element) return false;
                const focused = element => { for (let current = element; current != null;) { const root = current.getRootNode?.(); if (root == null || root.activeElement !== current) return false; if (!('host' in root)) return true; current = root.host; } return false; };
                return Object.getOwnPropertyDescriptor(element, '__codexPressSequentiallyTargetToken')?.value === request.targetToken && (element.matches(':focus') || focused(element)) && scope.frameChain.every(({ element }) => focused(element));
              }, { ...options, retry: false, arg: { targetToken } });
              if (!current.result || current.target.tabId !== focused.target.tabId || current.target.sessionId !== focused.target.sessionId || current.target.targetId !== focused.target.targetId) fail('pressSequentially target changed or lost focus while typing');
              await character(cdp, focused.target, char, { iabInputTargetToken: token, deadlineMs });
              if (index < characters.length - 1) await new Promise(resolve => setTimeout(resolve, 50));
            }
          } finally {
            await locator.evaluateOnPlaywrightSelector(tabId, selected(), (node, injected, request) => {
              const element = injected.retarget(node, 'follow-label');
              if (element && Object.getOwnPropertyDescriptor(element, '__codexPressSequentiallyTargetToken')?.value === request.targetToken) Reflect.deleteProperty(element, '__codexPressSequentiallyTargetToken');
            }, { arg: { targetToken }, retry: false, timeoutMs: 500 }).catch(() => {});
          }
        }
        value = { pressed: true }; break;
      }
      case 'locator_select':
        object(args, [...base, 'options'], ['selector', 'options']);
        if (!Array.isArray(args.options) || args.options.length > 2000) fail('Select options must be a bounded array');
        for (const option of args.options) { object(option, ['value', 'label', 'index']); if (!Object.keys(option).length) fail('Empty select option'); if (option.value !== undefined) string(option.value, 'value', 1048576, true); if (option.label !== undefined) string(option.label, 'label', 1048576, true); if (option.index !== undefined && (!Number.isSafeInteger(option.index) || option.index < 0)) fail('Invalid option index'); }
        value = await evaluate((node, injected, options) => { const state = injected.elementState(node, 'enabled'); if (state.received === 'error:notconnected') throw new Error('Element is not connected'); if (!state.matches) throw new Error('Element is not enabled'); const result = injected.selectOptions(node, options); if (typeof result === 'string' && result.startsWith('error:')) throw new Error(result); return true; }, args.options); break;
      case 'locator_check': {
        object(args, [...base, 'checked', 'force'], ['selector', 'checked']); boolean(args.checked, 'checked');
        const current = await locator.readCheckedState(input());
        if (current.checked !== args.checked) { if (current.isRadio && !args.checked) fail('Cannot uncheck a radio button'); await locator.clickLocator({ ...input(), force: boolean(args.force, 'force') }, 1); if ((await locator.readCheckedState(input())).checked !== args.checked) fail('Click did not produce the requested checked state'); }
        value = { checked: args.checked }; break;
      }
      case 'locator_wait': {
        object(args, [...base, 'state'], ['selector', 'state']); if (!['attached', 'detached', 'visible', 'hidden'].includes(args.state)) fail('Invalid locator wait state');
        await evaluateAll((nodes, injected, state) => {
          const node = nodes[0] ?? null;
          if (state === 'attached') { if (node) return true; throw new Error('Element is not attached'); }
          if (state === 'detached') { if (!node) return true; throw new Error('Element is still attached'); }
          if (!node) { if (state === 'hidden') return true; throw new Error('Element is not attached'); }
          const current = injected.elementState(node, state); if (current.received === 'error:notconnected') throw new Error('Element is not connected'); if (current.matches) return true; throw new Error('Element is not ' + state);
        }, args.state);
        value = { state: args.state }; break;
      }
      case 'media_download':
        object(args, base, ['selector']);
        await evaluate(element => {
          element.scrollIntoView({ block: 'center', inline: 'nearest' });
          const media = element.closest?.('img, video, source, a[href]') ?? element.querySelector?.('img, video, source, a[href]') ?? element;
          const property = (node, key) => typeof node[key] === 'string' ? node[key] : null;
          const url = property(media, 'currentSrc') ?? property(media, 'src') ?? property(media, 'href') ?? '';
          if (!url) throw new Error('Matched element does not expose a downloadable URL');
          const anchor = document.createElement('a'); anchor.href = url; anchor.download = url.split('/').pop()?.split('?')[0] || 'download'; anchor.rel = 'noopener'; anchor.style.display = 'none';
          document.body.appendChild(anchor); anchor.click(); anchor.remove(); return true;
        }); value = { triggered: true }; break;
      case 'playwright_dom_snapshot': {
        object(args, ['target_id', 'timeout_ms']);
        const result = await playwrightDomSnapshot(locator, tabId, timeoutMs, validate);
        value = result.value; snapshotObservation = result.observation; break;
      }
      case 'dom_snapshot':
        object(args, ['target_id', 'timeout_ms']);
        value = await visibleDom(tabId, cdp, { timeoutMs }); break;
      case 'dom_click': {
        object(args, ['target_id', 'node_id', 'timeout_ms'], ['node_id']); string(args.node_id, 'node_id', 64);
        const resolved = await domPoint(tabId, cdp, args.node_id);
        await clickPoint({ point: resolved.point, inputPoint: resolved.inputPoint, inputTarget: resolved.target, loadTarget: resolved.target, timeoutMs, clickCount: 1 }); value = { clicked: true }; break;
      }
      case 'dom_scroll': {
        object(args, ['target_id', 'node_id', 'scroll_x', 'scroll_y'], ['scroll_x', 'scroll_y']); if (args.node_id !== undefined) string(args.node_id, 'node_id', 64); finite(args.scroll_x, 'scroll_x'); finite(args.scroll_y, 'scroll_y');
        const resolved = args.node_id !== undefined ? await domPoint(tabId, cdp, args.node_id) : await cdp.call(tabId, 'Page.getLayoutMetrics', {}).then(({ cssVisualViewport }) => ({ point: { x: cssVisualViewport.clientWidth / 2, y: cssVisualViewport.clientHeight / 2 } }));
        await cursor(resolved.point.x, resolved.point.y, true);
        await cdp.call(tabId, 'Input.dispatchMouseEvent', { type: 'mouseMoved', ...resolved.point, button: 'none', buttons: 0, modifiers: 0 });
        await cdp.call(tabId, 'Input.synthesizeScrollGesture', { ...resolved.point, xDistance: -args.scroll_x, yDistance: -args.scroll_y, gestureSourceType: 'mouse', preventFling: true, speed: 8000 }); value = { scrolled: true }; break;
      }
      case 'pointer_move':
        object(args, ['target_id', 'x', 'y'], ['x', 'y']); point({ x: args.x, y: args.y });
        await cursor(args.x, args.y, true); await cdp.call(tabId, 'Input.dispatchMouseEvent', { type: 'mouseMoved', x: args.x, y: args.y, button: 'none', buttons: 0 }); value = { moved: true }; break;
      case 'paste': {
        object(args, ['target_id', 'text'], ['text']); string(args.text, 'text', limits.max_type_chars, true);
        const richText = richTextForPage(); await paste(args.text, { tabId }, undefined, undefined, false, richText); value = { pasted: true, rich_text: richText }; break;
      }
      case 'clipboard_read':
        object(args, ['target_id']); value = { items: clipboard.read() }; break;
      case 'clipboard_write':
        object(args, ['target_id', 'items'], ['items']); if (Buffer.byteLength(JSON.stringify(args.items)) > limits.max_type_chars) fail('Virtual clipboard exceeds room byte limit'); clipboard.write(args.items, 'browser.clipboard_write'); value = { written: true }; break;
      case 'evaluate_readonly': {
        object(args, [...base, 'script', 'selector_mode'], ['script']); string(args.script, 'script', limits.max_eval_chars, true);
        if (args.selector !== undefined) selected();
        if (args.selector_mode !== undefined && !['single', 'all'].includes(args.selector_mode)) fail('Unknown readonly selector mode');
        if (args.selector_mode === 'all' && args.selector === undefined) fail('Readonly selector_mode=all requires selector');
        value = (await evaluateReadonly({ tab_id: tabId, script: args.script, ...(args.selector === undefined ? {} : { selector: args.selector }), ...(args.selector_mode === undefined ? {} : { selector_mode: args.selector_mode }), timeout_ms: timeoutMs }, { cdp, playwright: locator, commandTiming: timing })).value;
        break;
      }
      case 'key':
        object(args, ['target_id', 'keys'], ['keys']); await chord(args.keys, { tabId }, undefined, undefined, deadlineMs); value = { pressed: true }; break;
      case 'click': {
        if (args.selector !== undefined) {
          object(args, [...base, 'button', 'click_count', 'modifiers', 'force'], ['selector']); modifiers(args.modifiers);
          await locator.clickLocator({ ...input(), button: args.button ?? 'left', modifiers: args.modifiers, force: boolean(args.force, 'force') }, args.click_count ?? 1);
        } else {
          object(args, ['target_id', 'x', 'y', 'button', 'click_count', 'modifiers', 'snapshot', 'timeout_ms'], ['x', 'y', 'snapshot']);
          if (args.snapshot !== false) fail('Typed viewport click requires explicit snapshot:false');
          const mask = modifiers(args.modifiers).reduce((value, mod) => value | ({ Alt: 1, Control: 2, Meta: 4, ControlOrMeta: 4, Shift: 8 })[mod], 0);
          await clickPoint({ point: { x: args.x, y: args.y }, button: args.button ?? 'left', clickCount: args.click_count ?? 1, modifiers: mask, timeoutMs });
        }
        value = { clicked: true }; break;
      }
      case 'scroll':
        object(args, ['target_id', 'mode', 'x', 'y', 'scroll_x', 'scroll_y'], ['mode', 'x', 'y', 'scroll_x', 'scroll_y']); if (args.mode !== 'gesture') fail('Expected gesture scroll'); point({ x: args.x, y: args.y }); finite(args.scroll_x, 'scroll_x'); finite(args.scroll_y, 'scroll_y'); await cursor(args.x, args.y, true); await cdp.call(tabId, 'Input.dispatchMouseEvent', { type: 'mouseMoved', x: args.x, y: args.y, button: 'none', buttons: 0, modifiers: 0 }); await cdp.call(tabId, 'Input.synthesizeScrollGesture', { x: args.x, y: args.y, xDistance: -args.scroll_x, yDistance: -args.scroll_y, gestureSourceType: 'mouse', preventFling: true, speed: 8000 }); value = { scrolled: true }; break;
      case 'drag': {
        object(args, ['target_id', 'path', 'modifiers'], ['path']); if (!Array.isArray(args.path) || args.path.length < 1 || args.path.length > 10000) fail('Drag requires a bounded point path'); const points = args.path.map(point), mods = modifiers(args.modifiers); const mask = mods.reduce((value, mod) => value | ({ Alt: 1, Control: 2, Meta: 4, ControlOrMeta: 4, Shift: 8 })[mod], 0); let last = points[0], pressed = false;
        await cursor(last.x, last.y, true); await cdp.call(tabId, 'Input.dispatchMouseEvent', { type: 'mouseMoved', ...last, button: 'none', buttons: 0, modifiers: mask });
        try { await cdp.call(tabId, 'Input.dispatchMouseEvent', { type: 'mousePressed', ...last, button: 'left', buttons: 1, clickCount: 1, modifiers: mask }); pressed = true; for (const next of points.slice(1)) { last = next; await cursor(next.x, next.y, false); await cdp.call(tabId, 'Input.dispatchMouseEvent', { type: 'mouseMoved', ...next, button: 'left', buttons: 1, modifiers: mask }); } }
        finally { if (pressed) await cdp.call(tabId, 'Input.dispatchMouseEvent', { type: 'mouseReleased', ...last, button: 'left', buttons: 0, clickCount: 1, modifiers: mask }); }
        value = { dragged: true }; break;
      }
      default: fail('Unknown typed browser operation');
    }
    validate();
    return { value: value === undefined ? null : value, effective_timeout_ms: timeoutMs, load_waits: loadWaits, frame_resolution_may_scroll: typeof args.selector === 'string' || operation === 'playwright_dom_snapshot', ...(snapshotObservation ? { snapshot_observation: snapshotObservation } : {}) };
  }
  return { perform, dispose() { removePageEvents(); for (const remove of cleanupHandlers) remove(); inputTargets.clear(); emitter.removeAllListeners(); } };
}
module.exports = { createBrowserFamily, roomClipboard, FAMILY, keyCommands };
