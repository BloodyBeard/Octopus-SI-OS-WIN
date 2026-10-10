'use strict';

function matchesRoute(event, route) {
  return event.source.tabId === route.tabId && (route.sessionId != null ? event.source.sessionId === route.sessionId : route.targetId != null ? event.source.targetId === route.targetId : event.source.sessionId == null);
}
function navigationBlocked(event, rejectSubframe) {
  return event.method === 'Page.navigationBlocked' && (event.params.resourceType !== 'subFrame' || rejectSubframe);
}
function rejectBlocked(event) {
  if (event.method !== 'Page.navigationBlocked') return;
  let display = 'this page';
  try { const url = new URL(event.params.url); if (['http:', 'https:'].includes(url.protocol)) { url.username = ''; url.password = ''; url.search = ''; url.hash = ''; display = url.toString(); if (url.pathname === '/') display = display.slice(0, -1); } } catch {}
  throw Object.assign(new Error(`Browser navigation is not permitted on ${display}.`), { code: 'browser_navigation_blocked' });
}
function checkObservation(control) {
  if (control?.signal?.aborted) throw control.signal.reason instanceof Error ? control.signal.reason
    : Object.assign(new Error('Browser observation was cancelled'), { code: 'wait_cancelled' });
}
function waitForEvent(events, route, predicate, timeoutMs, control = {}) {
  return new Promise((resolve, reject) => {
    let finished = false, timer;
    const finish = (error, event) => {
      if (finished) return;
      finished = true; events.removeListener('event', listener); clearTimeout(timer);
      control.signal?.removeEventListener('abort', abort);
      if (error) reject(error); else resolve(event);
    };
    const listener = event => { try { checkObservation(control); if (matchesRoute(event, route) && predicate(event)) finish(null, event); } catch (error) { finish(error instanceof Error ? error : new Error(String(error))); } };
    const abort = () => { try { checkObservation(control); } catch (error) { finish(error); } };
    try {
      checkObservation(control);
      events.addListener('event', listener);
      control.signal?.addEventListener('abort', abort, { once: true });
      timer = setTimeout(() => finish(Object.assign(new Error('Browser load observation deadline reached'), { code: 'load_observation_deadline' })), timeoutMs);
      control.onSubscribed?.();
    } catch (error) { finish(error); }
  });
}
async function waitForPageLoad(events, route, { timeoutMs, rejectSubframeNavigationBlocked = false }) {
  const receipt = { scope: route.sessionId != null || route.targetId != null ? 'frame' : 'page', classification_timeout_ms: 250, completion_timeout_ms: timeoutMs, start_event: null, completion_event: null };
  const caught = (error, phase) => ({ ...receipt, outcome: error?.code === 'load_observation_deadline' ? `${phase}_deadline` : 'observation_error', phase, ...(error?.code === 'load_observation_deadline' ? {} : { error: String(error?.message ?? error).slice(0, 4096) }) });
  let start;
  try { start = await waitForEvent(events, route, event => navigationBlocked(event, rejectSubframeNavigationBlocked) || ['Page.frameStartedLoading', 'Page.frameNavigated', 'Page.navigatedWithinDocument'].includes(event.method), 250); }
  catch (error) { return caught(error, 'classification'); }
  receipt.start_event = start.method; rejectBlocked(start);
  let end;
  try { end = await waitForEvent(events, route, event => navigationBlocked(event, rejectSubframeNavigationBlocked) || ['Page.domContentEventFired', 'Page.loadEventFired'].includes(event.method), timeoutMs); }
  catch (error) { return caught(error, 'completion'); }
  receipt.completion_event = end.method; rejectBlocked(end);
  return { ...receipt, outcome: 'load_completed', phase: 'completion' };
}
function urlPattern(pattern) {
  let expression = '^';
  for (let index = 0; index < pattern.length;) {
    const character = pattern[index];
    if (character === '*') { const double = pattern[index + 1] === '*'; expression += double ? '.*' : '[^/]*'; index += double ? 2 : 1; }
    else { expression += /[\\.^$|()[\]{}+?]/.test(character) ? `\\${character}` : character; index++; }
  }
  return new RegExp(`${expression}$`);
}
async function documentState(cdp, tabId, observations, control = {}) {
  try {
    checkObservation(control);
    const result = await cdp.call(tabId, 'Runtime.evaluate', { expression: '({ href: window.location.href, readyState: document.readyState })', returnByValue: true });
    checkObservation(control);
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text || 'Document state evaluation failed');
    return result.result?.value;
  } catch (error) { checkObservation(control); observations.push({ operation: 'document_state', error: String(error?.message ?? error).slice(0, 4096) }); return undefined; }
}
async function waitForLoadState(cdp, tabId, state, timeoutMs, observations = [], control = {}) {
  if (!['load', 'domcontentloaded'].includes(state)) throw new Error('Browser load-state wait does not support this state');
  checkObservation(control);
  await cdp.call(tabId, 'Page.enable', {});
  const current = await documentState(cdp, tabId, observations, control);
  if (current?.readyState === 'complete' || state === 'domcontentloaded' && current?.readyState === 'interactive') return { state, match_event: 'current_document', ready_state: current.readyState };
  const event = await waitForEvent(cdp, { tabId }, event => navigationBlocked(event, false) || event.method === 'Page.loadEventFired' || state === 'domcontentloaded' && event.method === 'Page.domContentEventFired', timeoutMs, control);
  rejectBlocked(event);
  return { state, match_event: event.method };
}
function eventUrl(event, mainFrame, matches) {
  if (event.method === 'Page.navigatedWithinDocument') return typeof event.params?.url === 'string' && matches(event.params.url) ? event.params.url : undefined;
  if (event.method !== 'Page.frameNavigated' || !event.params?.frame) return undefined;
  const frame = event.params.frame;
  if (mainFrame !== undefined ? frame.id !== mainFrame : typeof frame.parentId === 'string') return undefined;
  return typeof frame.url === 'string' && matches(frame.url) ? frame.url : undefined;
}
async function waitForUrl(cdp, tabId, pattern, waitUntil, timeoutMs, control = {}) {
  const expression = urlPattern(pattern), matches = url => expression.test(url), observations = [];
  checkObservation(control);
  await cdp.call(tabId, 'Page.enable', {});
  const current = await documentState(cdp, tabId, observations, control);
  let value;
  if (typeof current?.href === 'string' && current.href.length && matches(current.href)) value = { url: current.href, match_event: 'current_document' };
  else {
    let mainFrame;
    try { checkObservation(control); mainFrame = (await cdp.call(tabId, 'Page.getFrameTree', {})).frameTree?.frame?.id; checkObservation(control); }
    catch (error) { checkObservation(control); observations.push({ operation: 'main_frame', error: String(error?.message ?? error).slice(0, 4096) }); }
    const event = await waitForEvent(cdp, { tabId }, event => navigationBlocked(event, false) || eventUrl(event, mainFrame, matches) !== undefined, timeoutMs, control);
    rejectBlocked(event);
    value = { url: eventUrl(event, mainFrame, matches), match_event: event.method,
      ...(mainFrame === undefined ? {} : { main_frame_id: mainFrame }),
      ...(typeof (event.params?.frame?.id ?? event.params?.frameId) === 'string' ? { event_frame_id: event.params.frame?.id ?? event.params.frameId } : {}) };
  }
  if (waitUntil !== 'commit') value.load_state = await waitForLoadState(cdp, tabId, waitUntil, timeoutMs, observations, control);
  return { ...value, wait_until: waitUntil, observation_errors: observations };
}
function startWaitObservation(cdp, tabId, args, signal) {
  let prepared;
  const ready = new Promise(resolve => { prepared = resolve; });
  const control = { signal, onSubscribed: prepared };
  const duration = Math.min(Math.max(args.timeout_ms ?? 3000, 0), 3000);
  const outcome = (async () => {
    checkObservation(control);
    if (Object.hasOwn(args, 'url_pattern')) return waitForUrl(cdp, tabId, args.url_pattern, args.wait_until ?? 'commit', duration, control);
    const observations = [];
    return { ...(await waitForLoadState(cdp, tabId, args.load_state, duration, observations, control)), observation_errors: observations };
  })();
  outcome.then(prepared, prepared);
  return { ready, outcome, effective_timeout_ms: duration };
}
module.exports = { waitForPageLoad, waitForLoadState, waitForUrl, startWaitObservation };
