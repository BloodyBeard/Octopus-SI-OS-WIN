'use strict';

function captureSnapshot(first, second) {
  const element = first instanceof Element ? first : document.body || document.documentElement;
  const injected = first instanceof Element ? second : first;
  if (!element) return { full: '', iframeDepths: {}, iframeRefs: [] };
  const snapshot = injected.incrementalAriaSnapshot(element, { mode: 'ai' });
  let full = snapshot.full;
  const iframeRefs = snapshot.iframeRefs.filter(ref => {
    if (!(ref in snapshot.iframeDepths)) return false;
    try {
      const [frame] = injected.querySelectorAll(injected.parseSelector(`aria-ref=${ref}`), element);
      if (!frame) return false;
      const id = frame.getAttribute('id'), name = frame.getAttribute('name');
      const attributes = [...id ? [`[id=${JSON.stringify(id)}]`] : [], ...name ? [`[name=${JSON.stringify(name)}]`] : []].join(' ');
      if (attributes) full = full.replace(/^([ \t]*- iframe(?: [^\r\n]*)?) \[ref=([^\]\r\n]+)\]/gm,
        (line, prefix, current) => current === ref ? `${prefix} ${attributes} [ref=${ref}]` : line);
      return frame.getAttribute('aria-hidden') !== 'true' && injected.elementState(frame, 'visible').matches === true;
    } catch { return false; }
  });
  return { ...snapshot, full, iframeRefs };
}

function normalizeSnapshot(text) {
  if (!text.startsWith('- ') && !text.includes('\n- ') && !text.includes('\n  - ')) return text;
  const root = { children: [], indent: -1, line: '' }, stack = [root];
  for (const line of text.split('\n')) {
    if (!line.trim()) continue;
    const indent = line.match(/^ */)?.[0].length ?? 0;
    const node = { children: [], indent, line: line.slice(indent) };
    while (stack.length > 1 && indent <= stack.at(-1).indent) stack.pop();
    stack.at(-1).children.push(node); stack.push(node);
  }
  const flatten = node => {
    const children = node.children.flatMap(flatten);
    const line = node.line.replace(/ \[ref=[^\]]+\]/g, '').replace(/ \[cursor=[^\]]+\]/g, '');
    if (/^- img(?: \[[^\]]+\])*:?$/.test(line)) return [];
    return /^- (generic|listitem|group)(?: \[[^\]]+\])*:?$/.test(line) ? children : [{ line, children }];
  };
  const render = (nodes, depth = 0) => nodes.map(node => {
    const children = render(node.children, depth + 1);
    return `${'  '.repeat(depth)}${node.line}${children ? `\n${children}` : ''}`;
  }).join('\n');
  return render(root.children.flatMap(flatten));
}

async function playwrightDomSnapshot(locator, tabId, timeoutMs, validate) {
  const options = { timeoutMs, isolatedWorld: true };
  validate();
  const root = await locator.evaluateOnPlaywrightPage(tabId, captureSnapshot, options);
  validate();
  const deadlineMs = Date.now() + 1000, observations = [];
  const expand = async (snapshot, parent) => {
    const refs = snapshot.iframeRefs.filter(ref => ref in snapshot.iframeDepths);
    if (!refs.length) return snapshot.full;
    const bodies = new Map(await Promise.all(refs.map(async ref => {
      const selector = `${parent ? `${parent} >> internal:control=enter-frame >> ` : ''}aria-ref=${ref}`;
      const observation = { selector, outcome: 'deadline' }; observations.push(observation);
      if (Date.now() >= deadlineMs) return [ref, null];
      try {
        const snapshot = await locator.evaluateOnPlaywrightSelector(tabId,
          `${selector} >> internal:control=enter-frame >> body`, captureSnapshot,
          { ...options, deadlineMs, retry: false, timeoutMs: Math.max(1, Math.min(timeoutMs, 500, deadlineMs - Date.now())) });
        validate(); observation.outcome = 'expanded';
        return [ref, await expand(snapshot, selector)];
      } catch (error) {
        validate(); observation.outcome = 'unavailable';
        observation.error = String(error?.message ?? error).slice(0, 4096);
        return [ref, null];
      }
    })));
    const lines = [];
    for (const line of snapshot.full.split('\n')) {
      const ref = line.trimStart().startsWith('- iframe') ? line.match(/\[ref=([^\]]+)\]/)?.[1] : null;
      const body = ref ? bodies.get(ref) : null;
      if (!body) { lines.push(line); continue; }
      const indent = line.match(/^ */)?.[0] ?? '';
      lines.push(line.endsWith(':') ? line : `${line}:`, ...body.split('\n').map(value => `${indent}  ${value}`));
    }
    return lines.join('\n');
  };
  const value = normalizeSnapshot(await expand(root, null));
  validate();
  return { value, observation: { iframe_budget_ms: 1000, per_frame_timeout_ms: 500, frames: observations } };
}

module.exports = { playwrightDomSnapshot, normalizeSnapshot };
