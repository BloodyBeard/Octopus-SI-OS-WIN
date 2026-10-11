'use strict';

const fs = require('node:fs/promises');
const path = require('node:path');
const { StringDecoder } = require('node:string_decoder');
const {
  safeText, graphemes, widthOf, clip, wrap, row, composerLayout,
  entryRows, pickerRows, attachmentLabel, isConversationEntry, InlineRenderer,
} = require('./presentation.cjs');

const MAX_DRAFT = 262144;
const COMMANDS = [
  ['new', '[prompt]', 'Start a new task'],
  ['resume', '[task]', 'Continue a task'],
  ['tasks', '', 'Browse your tasks'],
  ['home', '', 'Open your Home task'],
  ['project', '[create]', 'Choose or create a Project'],
  ['agent', '[create]', 'Choose or create an Agent'],
  ['model', '', 'Choose a model and reasoning effort'],
  ['attach', '[path]', 'Attach a file to your next message'],
  ['files', '', 'See attached files'],
  ['remove', '[number|all]', 'Remove an attachment'],
  ['stop', '', 'Stop the active task'],
  ['answer', '', 'Answer a pending request'],
  ['history', '', 'Show this task’s transcript'],
  ['search', '[text]', 'Search tasks or this task’s transcript'],
  ['rename', '[title]', 'Rename this task'],
  ['archive', '', 'Archive this task'],
  ['archived', '', 'Browse archived tasks'],
  ['restore', '', 'Restore this archived task'],
  ['delete', '', 'Delete this settled task; recorded evidence is retained'],
  ['status', '', 'Show Home and current selection'],
  ['help', '', 'Commands and keyboard shortcuts'],
  ['detach', '', 'Leave the terminal; tasks keep running'],
  ['exit', '', 'Leave the terminal'],
].map(([name, args, description]) => ({ name, args, description, label: `/${name}`, search: name }));

function previousBoundary(text, cursor) {
  let previous = 0;
  for (const part of graphemes(text)) {
    if (part.index >= cursor) break;
    previous = part.index;
  }
  return previous;
}

function nextBoundary(text, cursor) {
  for (const part of graphemes(text)) if (part.index >= cursor) return part.index + part.text.length;
  return text.length;
}

class Draft {
  constructor(text = '') {
    this.text = safeText(text, { preserveTabs: true });
    if (Buffer.byteLength(this.text, 'utf8') > MAX_DRAFT) throw new Error('Draft is too large (maximum 256 KiB). Attach a file instead.');
    this.cursor = this.text.length;
    this.literal = false;
    this.limit = MAX_DRAFT;
    this.kill = '';
    this.undo = [];
    this.redo = [];
  }

  snapshot() { return { text: this.text, cursor: this.cursor, literal: this.literal }; }
  restore(value) {
    if (Buffer.byteLength(value.text, 'utf8') > this.limit) throw new Error('Wait for the pending message before restoring this draft.');
    Object.assign(this, value);
  }
  remember() {
    this.undo.push(this.snapshot());
    if (this.undo.length > 40) this.undo.shift();
    this.redo.length = 0;
  }

  set(text, literal = false) {
    text = safeText(text, { preserveTabs: true });
    if (Buffer.byteLength(text, 'utf8') > this.limit) throw new Error('Draft is too large (maximum 256 KiB). Attach a file instead, or wait for the pending message.');
    this.remember();
    this.text = text;
    this.cursor = text.length;
    this.literal = literal;
  }

  insert(text, pasted = false) {
    text = safeText(text, { preserveTabs: true });
    if (!text) return;
    if (Buffer.byteLength(this.text, 'utf8') + Buffer.byteLength(text, 'utf8') > this.limit) throw new Error('Paste is too large (maximum 256 KiB). Your draft is unchanged; attach a file instead, or wait for the pending message.');
    this.remember();
    if (pasted && this.cursor === 0 && /^\s*\//.test(text)) this.literal = true;
    this.text = this.text.slice(0, this.cursor) + text + this.text.slice(this.cursor);
    this.cursor += text.length;
  }

  erase(start, end, kill = false) {
    if (start === end) return;
    this.remember();
    if (kill) this.kill = this.text.slice(start, end);
    this.text = this.text.slice(0, start) + this.text.slice(end);
    this.cursor = start;
    if (!this.text) this.literal = false;
  }

  lineStart() { return this.cursor ? this.text.lastIndexOf('\n', this.cursor - 1) + 1 : 0; }
  lineEnd() {
    const end = this.text.indexOf('\n', this.cursor);
    return end < 0 ? this.text.length : end;
  }

  wordLeft() {
    const parts = graphemes(this.text.slice(0, this.cursor));
    let index = parts.length;
    while (index > 0 && /\s/u.test(parts[index - 1].text)) index -= 1;
    while (index > 0 && !/\s/u.test(parts[index - 1].text)) index -= 1;
    return parts[index]?.index ?? this.cursor;
  }

  wordRight() {
    const parts = graphemes(this.text.slice(this.cursor));
    let index = 0;
    while (index < parts.length && /\s/u.test(parts[index].text)) index += 1;
    while (index < parts.length && !/\s/u.test(parts[index].text)) index += 1;
    return this.cursor + (parts[index]?.index ?? this.text.slice(this.cursor).length);
  }

  vertical(direction, width) {
    const layout = composerLayout(this.text, this.cursor, width, Number.MAX_SAFE_INTEGER);
    const target = layout.positions[layout.cursor.row + direction];
    if (!target) return false;
    const choices = target.filter((position) => position.col <= layout.cursor.col && position.col < width);
    this.cursor = (choices.at(-1) || target[0]).index;
    return true;
  }

  edit(key) {
    switch (key) {
      case 'left': this.cursor = previousBoundary(this.text, this.cursor); break;
      case 'right': this.cursor = nextBoundary(this.text, this.cursor); break;
      case 'word-left': this.cursor = this.wordLeft(); break;
      case 'word-right': this.cursor = this.wordRight(); break;
      case 'home': this.cursor = this.lineStart(); break;
      case 'end': this.cursor = this.lineEnd(); break;
      case 'start': this.cursor = 0; break;
      case 'finish': this.cursor = this.text.length; break;
      case 'backspace': this.erase(previousBoundary(this.text, this.cursor), this.cursor); break;
      case 'delete': this.erase(this.cursor, nextBoundary(this.text, this.cursor)); break;
      case 'kill-word': this.erase(this.wordLeft(), this.cursor, true); break;
      case 'kill-start': this.erase(this.lineStart(), this.cursor, true); break;
      case 'kill-end': this.erase(this.cursor, this.lineEnd(), true); break;
      case 'yank': this.insert(this.kill); break;
      case 'newline': this.insert('\n'); break;
      case 'undo': {
        const previous = this.undo.pop();
        if (previous) { this.redo.push(this.snapshot()); this.restore(previous); }
        break;
      }
      case 'redo': {
        const next = this.redo.pop();
        if (next) { this.undo.push(this.snapshot()); this.restore(next); }
        break;
      }
      default: return false;
    }
    return true;
  }
}

const CONTROL_KEYS = {
  '\x01': 'home', '\x02': 'left', '\x03': 'interrupt', '\x04': 'detach',
  '\x05': 'end', '\x06': 'right', '\x07': 'agents', '\x08': 'backspace',
  '\t': 'tab', '\n': 'newline', '\x0b': 'kill-end', '\x0c': 'redraw',
  '\r': 'enter', '\x0e': 'new-task', '\x0f': 'attach', '\x10': 'projects', '\x11': 'answer', '\x12': 'history-search',
  '\x14': 'tasks', '\x15': 'kill-start', '\x17': 'kill-word', '\x19': 'yank',
  '\x1a': 'undo', '\x1f': 'undo', '\x7f': 'backspace',
};
const ESCAPE_KEYS = {
  '\x1b[A': 'up', '\x1b[B': 'down', '\x1b[C': 'right', '\x1b[D': 'left',
  '\x1b[H': 'home', '\x1b[F': 'end', '\x1bOH': 'home', '\x1bOF': 'end',
  '\x1b[1~': 'home', '\x1b[4~': 'end', '\x1b[7~': 'home', '\x1b[8~': 'end',
  '\x1b[3~': 'delete', '\x1b[5~': 'page-up', '\x1b[6~': 'page-down',
  '\x1b[Z': 'backtab', '\x1b[1;5C': 'word-right', '\x1b[1;5D': 'word-left',
  '\x1b[1;3C': 'word-right', '\x1b[1;3D': 'word-left', '\x1b[1;5H': 'start',
  '\x1b[1;5F': 'finish', '\x1bOP': 'help', '\x1b[11~': 'help',
  '\x1b\r': 'newline', '\x1b\n': 'newline', '\x1b[13;2u': 'newline',
  '\x1b[13;5u': 'enter', '\x1b[27;2;13~': 'newline', '\x1b[27;5;13~': 'enter',
  '\x1bb': 'word-left', '\x1bf': 'word-right', '\x1b\x7f': 'kill-word',
  '\x1bm': 'models', '\x1bz': 'redo',
};

class InputDecoder {
  constructor(onKey, onError) {
    this.onKey = onKey;
    this.onError = onError;
    this.decoder = new StringDecoder('utf8');
    this.buffer = '';
    this.paste = null;
    this.timer = null;
  }

  feed(chunk) {
    clearTimeout(this.timer);
    this.buffer += typeof chunk === 'string' ? chunk : this.decoder.write(chunk);
    this.parse();
  }

  parse() {
    while (this.buffer) {
      if (this.paste !== null) {
        const end = this.buffer.indexOf('\x1b[201~');
        if (end < 0) {
          const keep = Math.min(5, this.buffer.length);
          this.paste += this.buffer.slice(0, -keep);
          this.buffer = this.buffer.slice(-keep);
          if (this.paste.length > MAX_DRAFT) {
            this.paste = '';
            this.discardPaste = true;
          }
          return;
        }
        const text = this.paste + this.buffer.slice(0, end);
        this.buffer = this.buffer.slice(end + 6);
        this.paste = null;
        if (this.discardPaste || text.length > MAX_DRAFT) {
          this.discardPaste = false;
          this.onError(new Error('Paste is too large (maximum 256 KiB). Your draft is unchanged; attach a file instead.'));
        } else this.onKey({ key: 'paste', text });
        continue;
      }
      if (this.buffer.startsWith('\x1b[200~')) {
        this.buffer = this.buffer.slice(6);
        this.paste = '';
        continue;
      }
      if (this.buffer[0] === '\x1b') {
        const csi = this.buffer.match(/^\x1b\[[0-9;:?>]*[ -/]*[@-~]/);
        const ss3 = this.buffer.match(/^\x1bO./s);
        let sequence = csi?.[0] || ss3?.[0];
        if (!sequence && this.buffer.length > 1 && !['[', 'O', ']', 'P', '_', '^'].includes(this.buffer[1])) sequence = this.buffer.slice(0, 1 + Array.from(this.buffer.slice(1))[0].length);
        if (sequence) {
          this.buffer = this.buffer.slice(sequence.length);
          const key = ESCAPE_KEYS[sequence];
          if (key) this.onKey({ key });
          continue;
        }
        if (/^\x1b[\]P_^]/.test(this.buffer)) {
          const end = this.buffer.match(/^(?:\x1b[\]P_^])[\s\S]*?(?:\x07|\x1b\\)/);
          if (end) { this.buffer = this.buffer.slice(end[0].length); continue; }
        }
        this.timer = setTimeout(() => {
          const alone = this.buffer === '\x1b';
          this.buffer = '';
          if (alone) this.onKey({ key: 'escape' });
        }, 40);
        return;
      }
      const char = Array.from(this.buffer)[0];
      this.buffer = this.buffer.slice(char.length);
      const key = CONTROL_KEYS[char];
      if (key) this.onKey({ key });
      else if (!/[\x00-\x1f\x7f-\x9f]/.test(char)) this.onKey({ key: 'text', text: char });
    }
  }

  close() { clearTimeout(this.timer); this.buffer = ''; this.paste = null; }
}

function splitPaths(text) {
  const parts = [];
  let part = '';
  let quote = '';
  let escaped = false;
  for (const char of text.trim()) {
    if (escaped) { part += char; escaped = false; }
    else if (char === '\\' && quote !== "'") escaped = true;
    else if (quote && char === quote) quote = '';
    else if (!quote && (char === '"' || char === "'")) quote = char;
    else if (!quote && /\s/.test(char)) { if (part) parts.push(part); part = ''; }
    else part += char;
  }
  if (quote || escaped) throw new Error('Unfinished quote or backslash in the file path.');
  if (part) parts.push(part);
  return parts;
}

function modelEfforts(model) {
  if (!Array.isArray(model.reasoningEfforts)) throw new Error('The model did not declare its reasoning choices.');
  return model.reasoningEfforts.map((value) => typeof value === 'string' ? value : value.reasoningEffort);
}

class InteractiveTerminal {
  constructor(client, options) {
    this.client = client;
    this.options = options;
    this.input = options.input || process.stdin;
    this.output = options.output || process.stdout;
    this.renderer = new InlineRenderer(this.output, options);
    this.draft = new Draft(options.initialPrompt || '');
    this.files = [];
    this.selection = {};
    this.catalog = { projects: [], providers: [] };
    this.run = null;
    this.state = 'idle';
    this.active = false;
    this.busy = 'Connecting';
    this.history = [];
    this.historyIndex = -1;
    this.historyDraft = null;
    this.drafts = new Map();
    this.entries = new Map();
    this.pendingEntries = new Map();
    this.printed = new Map();
    this.requests = new Map();
    this.followGeneration = 0;
    this.pending = new Set();
    this.closed = false;
    this.menu = null;
    this.form = null;
    this.dismissedCommand = null;
    this.done = new Promise((resolve) => { this.resolveDone = resolve; });
  }

  project() { return this.catalog.projects.find((project) => project.id === this.selection.projectId); }
  agent() { return this.project()?.agents?.find((agent) => agent.id === this.selection.agentId); }
  assistantName() { return this.agent()?.name || this.run?.agentName || 'Octopus'; }
  taskTitle(task = this.run) { return task?.isHome ? 'Home' : task?.title || 'New task'; }
  providerName(providerId = this.selection.providerId) {
    const provider = this.catalog.providers?.find((item) => item.id === providerId);
    return provider?.displayName || (provider?.sourceNames?.length === 1 ? provider.sourceNames[0] : provider?.id) || providerId || 'Choose provider';
  }

  contextLabel() {
    const project = this.project()?.name || this.run?.projectName || (this.selection.projectId ? this.selection.projectId : 'Choose Project');
    const agent = this.agent()?.name || this.run?.agentName || (this.run && !this.run.agentId ? 'Task Agent' : 'Choose Agent');
    return `${project}${this.run?.isHome ? '' : ` · ${agent}`} · ${this.providerName()} / ${this.selection.model || 'Choose model'}${this.selection.effort ? ` · ${this.selection.effort}` : ''}`;
  }

  renderSoon() {
    if (this.closed || this.renderTimer) return;
    this.renderTimer = setTimeout(() => { this.renderTimer = null; this.render(); }, 25);
  }

  render() {
    if (this.closed) return;
    clearTimeout(this.renderTimer);
    this.renderTimer = null;
    const width = this.renderer.width;
    const available = this.renderer.availableRows;
    const rows = [];
    const status = this.busy || (this.connectionLost ? 'Disconnected · /resume to reconnect' : this.run?.archived ? 'Archived · /restore to continue' : this.state === 'stopping' ? 'Stopping · waiting for current operation' : this.state === 'waiting' ? 'Response needed · /answer' : this.active ? 'Working · Esc to stop' : this.run?.writable === false ? 'Read only · /new to chat' : this.state === 'failed' ? 'Task failed' : 'Ready');
    if (available >= 5) rows.push(row(clip(`${this.taskTitle()} · ${status}`, width), this.state === 'failed' ? 'error' : 'muted'));
    if (available >= 4) rows.push(row(clip(this.busy === 'Connecting' ? 'Loading Home…' : this.contextLabel(), width), 'muted'));
    let menuRows = [];
    if (this.menu) {
      const view = pickerRows(this.menu, width, Math.max(1, Math.min(6, available - 7)));
      this.menu.visible = view.items;
      this.menu.index = view.index;
      menuRows = view.rows;
    }
    const editor = this.form?.editor || this.draft;
    const maxComposer = Math.max(1, Math.min(6, available - rows.length - menuRows.length - 2));
    const draft = composerLayout(this.form?.secret ? '•'.repeat(editor.text.length) : editor.text, editor.cursor, width, maxComposer, this.form ? '? ' : '› ');
    const reserved = rows.length + menuRows.length + draft.rows.length + 1;
    const liveBudget = Math.max(0, Math.min(8, available - reserved - (this.files.length ? 1 : 0) - (this.form ? 1 : 0)));
    if (liveBudget && this.pendingEntries.size) {
      const liveRows = [];
      for (const entry of this.pendingEntries.values()) liveRows.push(...entryRows(entry, width, { assistantName: this.assistantName(), outputRows: 3 }));
      if (liveRows.length > liveBudget) rows.push(row('  …', 'muted'));
      rows.push(...liveRows.slice(-(liveBudget - (liveRows.length > liveBudget ? 1 : 0))));
    }
    rows.push(...menuRows);
    if (this.form && rows.length + draft.rows.length + 2 <= available) rows.push(row(clip(this.form.title, width), 'bold'));
    if (!this.menu && this.files.length && rows.length + draft.rows.length + 2 <= available) rows.push(row(clip(`+ ${this.files.length} file${this.files.length === 1 ? '' : 's'} · ${this.files.map((file) => path.basename(file)).join(', ')} · /remove`, width), 'muted'));
    const composerStart = rows.length;
    rows.push(...draft.rows);
    let hint = this.menu ? '↑↓ choose · Enter select · Esc back · Ctrl+D detach' : this.form ? 'Enter confirm · Esc cancel · Ctrl+D detach' : 'Enter send · Alt+Enter / Ctrl+J newline · / commands · Ctrl+D detach';
    if (!editor.text && !this.menu && !this.form) hint = 'Type a message · / commands · Ctrl+P Project · Ctrl+T Tasks';
    if (draft.hiddenAbove || draft.hiddenBelow) hint = `${draft.totalRows} draft lines · ${hint}`;
    rows.push(row(clip(hint, width), 'muted'));
    const excess = Math.max(0, rows.length - available);
    this.renderer.frame(rows.slice(excess), { row: Math.max(0, composerStart + draft.cursor.row - excess), col: draft.cursor.col });
  }

  print(rows) {
    if (this.closed) return;
    this.renderer.append(rows.flatMap((item) => wrap(item.text, this.renderer.width).map((line) => row(line, item.tone))));
    this.render();
  }

  notice(text, tone = 'muted') {
    this.print([...wrap(text, this.renderer.width).map((line) => row(line, tone)), row()]);
  }

  fail(error) { this.notice(error?.message || String(error), 'error'); }

  action(label, fn) {
    if (this.busy) {
      this.notice(`${this.busy}… Your draft is kept.`, 'warning');
      return;
    }
    this.busy = label;
    this.render();
    const promise = Promise.resolve().then(fn).catch((error) => { if (!this.closed) this.fail(error); }).finally(() => {
      this.pending.delete(promise);
      if (this.busy === label) this.busy = '';
      if (this.closed) this.maybeDone();
      else this.render();
    });
    this.pending.add(promise);
    return promise;
  }

  invoke(fn, label = 'Selecting', failed) {
    let result;
    try { result = fn(); }
    catch (error) { failed?.(); this.fail(error); this.render(); return; }
    if (!result || typeof result.then !== 'function') { this.render(); return; }
    const ownsBusy = !this.busy;
    if (ownsBusy) this.busy = label;
    const promise = Promise.resolve(result).catch((error) => { if (!this.closed) { failed?.(); this.fail(error); } }).finally(() => {
      this.pending.delete(promise);
      if (ownsBusy && this.busy === label) this.busy = '';
      if (this.closed) this.maybeDone();
      else this.render();
    });
    this.pending.add(promise);
  }

  async start() {
    this.rawBefore = Boolean(this.input.isRaw);
    this.flowingBefore = this.input.readableFlowing === true;
    this.decoder = new InputDecoder((event) => { try { this.key(event); } catch (error) { this.fail(error); } }, (error) => this.fail(error));
    this.onData = (chunk) => this.decoder.feed(chunk);
    this.onEnd = () => this.quit('eof');
    this.onInputError = (error) => { this.failure = error; this.quit('input-error'); };
    this.onOutputError = (error) => { this.failure = error; this.renderer.closed = true; this.quit('output-error'); };
    this.onResize = () => { this.renderer.resize(); this.render(); };
    this.input.setRawMode(true);
    this.input.on('data', this.onData);
    this.input.once('end', this.onEnd);
    this.input.once('error', this.onInputError);
    this.output.on('resize', this.onResize);
    this.output.once('error', this.onOutputError);
    this.input.resume();
    this.output.write('\x1b[?2004h');
    this.renderer.append([row('Octopus', 'title'), row('Talk to an Agent. Your tasks are shared with the desktop.', 'muted'), row()]);
    this.render();
    if (this.options.processSignals !== false && this.input === process.stdin) {
      this.onInterrupt = () => this.key({ key: 'interrupt' });
      this.onTerminate = () => this.quit('signal');
      process.on('SIGINT', this.onInterrupt);
      process.on('SIGTERM', this.onTerminate);
      process.on('SIGHUP', this.onTerminate);
    }
    this.onAbort = () => this.quit('aborted');
    this.options.signal?.addEventListener('abort', this.onAbort, { once: true });
    if (this.options.signal?.aborted) this.quit('aborted');
    try {
      if (!this.closed) {
        const [status, catalog] = await Promise.all([this.client.status(), this.client.contexts()]);
        if (!Array.isArray(catalog?.projects)) throw new Error('The client did not return a Project catalog.');
        this.status = status;
        this.catalog = catalog;
        if (!this.closed) {
          this.notice(`Home: ${status.home || '(not supplied by client)'}${status.state ? ` · ${status.state}` : ''}`);
          if (this.options.runId) await this.resume(this.options.runId);
          else await this.initializeSelection(status.selection || {});
          if (this.options.files?.length) await this.attach(this.options.files.map((file) => `'${String(file).replaceAll("'", "'\\''")}'`).join(' '));
          if (this.options.initialPrompt) this.draft.set(this.options.initialPrompt);
          if (this.run) {
            if (this.options.providerId && this.options.providerId !== this.run.providerId) throw new Error('This task is bound to another provider. Choose a new task to change providers.');
            if (this.options.projectId && this.options.projectId !== this.run.projectId || this.options.agentId && this.options.agentId !== this.run.agentId) throw new Error('The task does not match the explicitly selected Project or Agent.');
            for (const key of ['model', 'effort']) if (this.options[key] !== undefined) this.selection[key] = this.options[key];
          }
          this.busy = '';
          this.render();
          if (this.options.submitInitialPrompt && !this.closed && this.draft.text.trim()) this.submit();
        }
      }
      await this.done;
      if (this.failure) throw this.failure;
      return this.options.returnState ? this.result : 0;
    } finally {
      this.cleanup();
    }
  }

  async initializeSelection(defaultSelection) {
    if (!this.options.projectId && !this.options.agentId) return this.resume('home');
    const requested = { ...defaultSelection };
    for (const key of ['projectId', 'agentId', 'providerId', 'model', 'effort']) if (this.options[key] !== undefined) requested[key] = this.options[key];
    const homes = this.catalog.projects.filter((project) => project.isHome);
    this.requestedSelection = requested;
    const matchingProjects = requested.agentId ? this.catalog.projects.filter((project) => project.agents?.some((agent) => agent.id === requested.agentId)) : [];
    const projectId = requested.projectId || (matchingProjects.length === 1 ? matchingProjects[0].id : homes.length === 1 ? homes[0].id : undefined);
    this.selection = projectId ? { projectId } : {};
    if (projectId && !this.project()) throw new Error('The selected Project is unavailable. Choose a Project explicitly.');
    if (!this.project()) return this.projectPicker();
    if (this.project().isHome && !requested.agentId) return this.resume('home');
    if (!requested.agentId) return this.agentPicker();
    const agent = this.project().agents?.find((candidate) => candidate.id === requested.agentId);
    if (!agent) {
      this.notice('The selected Agent is unavailable. Choose an existing Agent explicitly.', 'warning');
      return this.agentPicker();
    }
    this.setAgent(agent, requested);
    if (!this.selection.providerId || !this.selection.model) await this.modelPicker();
    this.render();
  }

  setAgent(agent, requested = {}) {
    this.selection = {
      projectId: this.selection.projectId, agentId: agent.id,
      providerId: agent.providerId, model: agent.model, effort: agent.effort,
    };
    for (const key of ['providerId', 'model', 'effort']) if (requested[key] !== undefined) this.selection[key] = requested[key];
    this.requestedSelection = null;
  }

  saveDraft() {
    const key = this.run?.id || `new:${this.selection.projectId || ''}`;
    this.drafts.set(key, { ...this.draft.snapshot(), files: [...this.files] });
  }

  flushPending() {
    for (const entry of this.pendingEntries.values()) this.commit(entry);
    this.pendingEntries.clear();
  }

  detachFollow() {
    this.followGeneration += 1;
    this.followController?.abort();
    this.followController = null;
    this.active = false;
  }

  newTask(prompt, keepDraft = false) {
    this.saveDraft();
    this.flushPending();
    const previous = this.run;
    this.detachFollow();
    this.run = null;
    this.entries.clear();
    this.printed.clear();
    this.requests.clear();
    this.stopRequested = null;
    this.connectionLost = false;
    this.state = 'idle';
    this.menu = null;
    this.form = null;
    if (!keepDraft) { this.draft.set(prompt || ''); this.files = []; }
    this.notice(`New task${previous && ['working', 'waiting', 'running'].includes(previous.state) ? ' · previous task continues' : ''}`);
    if (previous?.isHome || this.project()?.isHome && !this.selection.agentId) {
      this.selection = {};
      this.projectPicker({ forNewTask: true });
    } else if (!this.project()) this.projectPicker({ forNewTask: true });
    else if (!this.selection.agentId) this.agentPicker();
  }

  openPicker(title, items, choose, extra = {}) {
    this.form = null;
    this.menu = { title, items, choose, query: '', index: 0, ...extra };
    this.render();
  }

  openForm(title, confirm, { initial = '', secret = false, cancel } = {}) {
    this.menu = null;
    this.form = { title, confirm, editor: new Draft(initial), secret, cancel };
    this.render();
  }

  projectPicker({ forNewTask = false, afterSelect } = {}) {
    const items = this.catalog.projects.filter((project) => !forNewTask || !project.isHome).map((project) => ({ label: project.name || project.id, description: project.isHome ? 'Open Home' : '', value: project }));
    items.push({ label: '+ Create Project', value: 'create' });
    this.openPicker('Choose Project · starts a new task', items, async (item) => {
      if (item.value === 'create') return this.createProject();
      if (item.value.isHome) return this.resume('home');
      if (this.run) this.newTask(undefined, true);
      this.selection = { projectId: item.value.id };
      if (afterSelect) return afterSelect();
      this.agentPicker();
    });
  }

  agentPicker() {
    if (!this.project()) return this.projectPicker();
    if (this.project().isHome && !this.project().agents?.length) return this.projectPicker({ forNewTask: true });
    const items = (this.project().agents || []).map((agent) => ({ label: agent.name || agent.id, description: `${agent.model || 'Model not selected'}${agent.effort ? ` · ${agent.effort}` : ''}`, value: agent }));
    items.push({ label: '+ Create Agent', value: 'create' });
    this.openPicker('Choose Agent · starts a new task', items, async (item) => {
      if (item.value === 'create') return this.createAgent();
      if (this.run) this.newTask(undefined, true);
      this.setAgent(item.value, this.requestedSelection || {});
      this.notice(`Agent: ${item.value.name || item.value.id} · ${this.selection.model || 'Choose model'}`);
      if (!this.selection.providerId || !this.selection.model) await this.modelPicker();
    });
  }

  async modelPicker(selection = this.selection, choose) {
    if (!selection.agentId && !selection.sourceWorkerId && !this.run) return this.agentPicker();
    const query = { projectId: selection.projectId, agentId: selection.agentId, workerId: selection.workerId };
    if (this.run && !choose) Object.assign(query, { runId: this.run.id, providerId: this.run.providerId });
    const models = await this.client.models(query);
    if (!Array.isArray(models)) throw new Error('The client did not return a model catalog.');
    const boundProvider = !choose && this.run?.providerId;
    const rows = models.filter((model) => !boundProvider || model.providerId === boundProvider).map((model) => ({
      label: model.displayName || model.model,
      description: `${this.providerName(model.providerId)}${model.providerId === selection.providerId && model.model === selection.model ? ' · selected' : ''}`,
      value: model,
    }));
    if (!rows.length) throw new Error('No available models for this selection. The current model has been kept. Choose another Agent or reconnect the provider.');
    const chosenIndex = rows.findIndex((item) => item.value.providerId === selection.providerId && item.value.model === selection.model);
    this.openPicker('Choose model', rows, async (item) => {
      const model = item.value;
      if (!model.providerId || !model.model) throw new Error('The chosen model has no exact provider/model identity.');
      const finish = (effort) => {
        const value = { ...selection, providerId: model.providerId, model: model.model, effort };
        if (choose) choose(value);
        else { this.selection = value; this.notice(`Model: ${model.displayName || model.model}${effort ? ` · ${effort}` : ''}`); }
      };
      const efforts = modelEfforts(model);
      if (!efforts.length) return finish('');
      if (efforts.some((effort) => typeof effort !== 'string' || !effort)) throw new Error('The model has incomplete reasoning choices.');
      const current = efforts.includes(selection.effort) ? selection.effort : model.defaultReasoningEffort;
      this.openPicker('Reasoning effort', efforts.map((effort) => ({ label: effort, description: effort === model.defaultReasoningEffort ? 'default' : '', value: effort })), (choice) => finish(choice.value), { index: Math.max(0, efforts.indexOf(current)) });
    }, { index: Math.max(0, chosenIndex) });
  }

  async tasksPicker(archived = false, search = '') {
    const tasks = await this.client.tasks({ archived, ...(search ? { search } : {}) });
    if (!Array.isArray(tasks)) throw new Error('The client did not return a task list.');
    const items = tasks.map((task) => ({
      label: this.taskTitle(task),
      description: `${task.state || ''}${task.archived ? ' · archived' : ''} · ${this.catalog.projects.find((project) => project.id === task.projectId)?.name || task.projectId || 'Home'}`,
      search: task.id, value: task,
    }));
    if (!archived) items.push({ label: '+ New task', value: 'new' }, { label: '+ Create Project', value: 'project' }, { label: '+ Create Agent', value: 'agent' });
    this.openPicker(archived ? 'Archived tasks' : 'Your tasks', items, async (item) => {
      if (item.value === 'new') return this.newTask();
      if (item.value === 'project') return this.createProject();
      if (item.value === 'agent') return this.createAgent();
      await this.resume(item.value.id);
    }, { empty: 'No tasks match. /new starts a task.' });
  }

  async resume(id) {
    const [task, history] = await Promise.all([this.client.task(id), this.client.history(id)]);
    if (!task?.id || !Array.isArray(history)) throw new Error('The client returned an incomplete task or transcript.');
    this.saveDraft();
    this.flushPending();
    this.detachFollow();
    this.run = task;
    this.selection = { projectId: task.projectId, agentId: task.agentId, providerId: task.providerId, model: task.model, effort: task.effort };
    this.entries.clear();
    this.printed.clear();
    this.requests.clear();
    this.connectionLost = false;
    this.menu = null;
    this.form = null;
    const saved = this.drafts.get(task.id);
    this.draft.set(saved?.text || '');
    if (saved) this.draft.restore(saved);
    this.files = saved?.files || [];
    this.notice(`${this.taskTitle(task)}${task.archived ? ' · archived · /restore to continue' : ''}`);
    for (const entry of history) {
      this.rememberEntry(entry);
      if (entry.request && entry.status !== 'resolved') this.requests.set(entry.id, { type: 'request', id: entry.id, kind: entry.request.kind, payload: entry.request.payload, status: entry.status, entry });
      this.commit(entry);
    }
    const lastOutcome = history.findLast((entry) => entry.metadata?.runId === task.id && entry.metadata?.interfaceId === task.chatInterfaceId
      && entry.metadata?.fromInterfaceId === task.providerBoundInterfaceId && entry.metadata?.fromInterfaceRole === 'provider'
      && (entry.terminal || require('./transcript.cjs').pendingStop(entry)));
    const stopping = lastOutcome && require('./transcript.cjs').pendingStop(lastOutcome);
    this.state = stopping ? 'stopping' : task.state === 'working' || task.state === 'running' ? 'working' : task.state === 'waiting' ? 'waiting' : task.state === 'failed' ? 'failed' : 'idle';
    this.stopRequested = null;
    if (task.turnOpen === true || (!task.archived && (this.state === 'working' || this.state === 'waiting' || stopping))) {
      this.active = true;
      this.follow(task.id, { cursor: task.cursor, afterSeq: task.afterSeq, messageId: task.messageId });
    }
    if (this.requests.size) this.notice('This task needs a response. Use /answer.', 'warning');
    this.render();
  }

  rememberEntry(entry) {
    if (!entry || typeof entry.id !== 'string' || !['user', 'assistant', 'system', 'tool'].includes(entry.role)) throw new Error('Unsupported transcript entry from the client.');
    this.entries.set(entry.id, entry);
    if (entry.role === 'user' && entry.text && !this.history.includes(entry.text)) this.history.push(entry.text);
  }

  commit(entry) {
    if (!isConversationEntry(entry)) return;
    const displayKey = JSON.stringify([entry.role, entry.text, entry.status, entry.activity, entry.attachments, entry.error]);
    if (this.printed.get(entry.id) === displayKey) return;
    if (this.printed.has(entry.id)) this.renderer.append([row('Updated task entry', 'muted')]);
    this.renderer.append(entryRows(entry, this.renderer.width, { assistantName: this.assistantName() }));
    this.printed.set(entry.id, displayKey);
    this.pendingEntries.delete(entry.id);
  }

  receive(event) {
    if (!event || typeof event.type !== 'string') throw new Error('Invalid live event from the client.');
    if (this.run && event.cursor !== undefined) {
      this.run.cursor = event.cursor;
      if (this.run.lastReceipt) this.run.lastReceipt = { ...this.run.lastReceipt, cursor: event.cursor };
    }
    switch (event.type) {
      case 'message': {
        this.rememberEntry(event.entry);
        if (!isConversationEntry(event.entry)) break;
        if (event.entry.role === 'user' || event.entry.role === 'system') this.commit(event.entry);
        else this.pendingEntries.set(event.entry.id, event.entry);
        break;
      }
      case 'delta': {
        if (!event.text) break;
        if (typeof event.id !== 'string' || !['assistant', 'user', 'system'].includes(event.role)) throw new Error('Invalid reply delta from the client.');
        const entry = { ...(this.entries.get(event.id) || { id: event.id, role: event.role, kind: 'message', text: '' }), text: (this.entries.get(event.id)?.text || '') + event.text };
        this.rememberEntry(entry);
        this.pendingEntries.set(entry.id, entry);
        break;
      }
      case 'activity': {
        const entry = event.entry;
        this.rememberEntry(entry);
        if (!isConversationEntry(entry)) break;
        const activity = { ...(entry.activity || {}), title: event.title, status: event.status };
        for (const field of ['command', 'script', 'output', 'stdout', 'stderr']) if (event[field] !== undefined) activity[field] = event[field];
        const presentation = { ...entry, activity, status: event.status || entry.status };
        this.entries.set(entry.id, presentation);
        if (['completed', 'exited', 'failed', 'error', 'cancelled'].includes(presentation.status)) this.commit(presentation);
        else this.pendingEntries.set(entry.id, presentation);
        break;
      }
      case 'request':
        this.rememberEntry(event.entry);
        this.commit(event.entry);
        if (event.status === 'resolved' || event.payload?.resolved === true) this.requests.delete(event.id);
        else { this.requests.set(event.id, event); this.state = 'waiting'; this.notice('Response needed · /answer', 'warning'); }
        break;
      case 'error':
        this.rememberEntry(event.entry);
        this.commit({ ...event.entry, text: event.text || event.entry.text, error: event.entry.error || true });
        break;
      case 'status':
        if (!['working', 'waiting', 'stopping', 'idle', 'failed'].includes(event.state)) throw new Error('Unsupported task status from the client.');
        if (event.correlated === true) {
          if (event.state === 'stopping' && this.state !== 'stopping') this.notice('Stop acknowledged. Waiting for the current operation to report its result.');
          this.state = event.state;
          if (this.run) this.run.state = event.state;
          if (event.state === 'stopping') this.active = true;
        }
        if (event.entry) { this.rememberEntry(event.entry); this.commit(event.entry); }
        if (event.terminal === true && event.correlated === true) {
          this.flushPending();
          this.active = false;
          this.connectionLost = false;
          this.stopRequested = null;
          if (event.interrupted) this.notice(event.entry?.kind === 'interrupted_result' ? 'Stopped.' : 'Stop acknowledged.');
          if (event.state === 'failed') this.notice('Task failed. Your next draft is kept.', 'error');
          return true;
        }
        break;
      default: throw new Error(`Unsupported terminal event: ${safeText(event.type)}.`);
    }
    this.renderSoon();
    return false;
  }

  follow(runId, receipt = {}) {
    this.followController?.abort();
    const controller = new AbortController();
    this.followController = controller;
    const generation = ++this.followGeneration;
    this.followPromise = (async () => {
      let confirmed = false;
      try {
        const events = await this.client.follow(runId, { ...receipt, signal: controller.signal });
        for await (const event of events) {
          if (controller.signal.aborted || this.closed || generation !== this.followGeneration) return;
          if (this.receive(event)) { confirmed = true; controller.abort(); break; }
        }
        if (!confirmed && !controller.signal.aborted && !this.closed && generation === this.followGeneration) {
          this.flushPending();
          this.active = false;
          this.connectionLost = true;
          this.notice('Live connection ended; task completion was not confirmed. /resume reconnects to the task.', 'warning');
        }
      } catch (error) {
        if (!controller.signal.aborted && !this.closed && generation === this.followGeneration) {
          this.flushPending();
          this.active = false;
          this.connectionLost = true;
          this.fail(error);
          this.notice('Live connection lost. /resume reconnects; the shared task may still be running.', 'warning');
        }
      } finally {
        if (generation === this.followGeneration && !this.closed) this.render();
      }
    })();
  }

  submit() {
    if (this.busy) return this.notice(`${this.busy}… Your draft is kept.`, 'warning');
    if (this.active) return this.notice('This task is working. Your draft is kept. Esc stops it; /new opens another task.', 'warning');
    if (this.connectionLost) return this.notice('Reconnect with /resume before sending another message. Task completion was not confirmed; your draft is kept.', 'warning');
    if (this.run?.archived) return this.notice('This task is archived. /restore it, or start /new.', 'warning');
    if (this.run?.writable === false) return this.notice('This task is read only. /new starts a writable task.', 'warning');
    if (!this.run && !this.selection.agentId) return this.agentPicker();
    if (!this.selection.projectId || !this.selection.providerId || !this.selection.model) return this.notice('Choose a Project, Agent, and model before sending. Your draft is kept.', 'warning');
    const text = this.draft.text;
    if (!text.trim() && !this.files.length) return;
    const files = [...this.files];
    const snapshot = this.draft.snapshot();
    const selection = { ...this.selection };
    const runId = this.run?.id;
    this.draft.set('');
    this.draft.limit = Math.max(0, MAX_DRAFT - Buffer.byteLength(text, 'utf8') - 2);
    this.files = [];
    this.action('Sending', async () => {
      let receipt;
      try {
        receipt = await this.client.send({ ...selection, ...(runId ? { runId } : {}), text, ...(files.length ? { files } : {}) });
        if (!receipt?.runId) throw new Error('The message was not acknowledged with a task ID. Check /tasks before retrying.');
      } catch (error) {
        const nextDraft = this.draft.text;
        this.draft.limit = MAX_DRAFT;
        this.draft.set(snapshot.text + (nextDraft ? `\n\n${nextDraft}` : ''), snapshot.literal);
        this.files = [...new Set([...files, ...this.files])];
        this.stopAfterSend = false;
        if (this.closed) this.failure = error;
        else if (nextDraft) this.notice('Your unsent message was restored. New typing is kept below it.', 'warning');
        throw error;
      }
      this.draft.limit = MAX_DRAFT;
      this.run = { ...(this.run || { title: clip(text.trim().split('\n')[0] || 'Attached files', 72), writable: true }), ...selection, id: receipt.runId, state: 'working' };
      this.run.lastReceipt = receipt;
      this.stopRequested = null;
      this.history.push(text);
      this.historyIndex = -1;
      this.historyDraft = null;
      this.state = 'working';
      this.active = true;
      this.connectionLost = false;
      if (!this.closed) this.follow(receipt.runId, receipt);
      if (this.stopAfterSend) {
        this.stopAfterSend = false;
        this.stopRequested = receipt.runId;
        try { await this.client.stop(receipt.runId); }
        catch (error) { this.stopRequested = null; throw error; }
        if (!this.closed) this.notice('Stop requested. Waiting for task status.');
      }
    });
  }

  stop() {
    if (this.busy === 'Sending') {
      this.stopAfterSend = true;
      this.notice('The task will be stopped as soon as its submission is acknowledged.');
      return;
    }
    if (!this.run?.id) {
      this.notice('No task is selected.');
      return;
    }
    const runId = this.run.id;
    if (this.stopRequested === runId) return this.notice('Stop has already been requested. Waiting for task status.');
    this.stopRequested = runId;
    const promise = Promise.resolve().then(() => this.client.stop(runId)).then(() => {
      if (!this.closed) this.notice('Stop requested. Waiting for task status.');
    }).catch((error) => { this.stopRequested = null; if (!this.closed) this.fail(error); }).finally(() => { this.pending.delete(promise); if (this.closed) this.maybeDone(); });
    this.pending.add(promise);
  }

  async attach(text) {
    const paths = splitPaths(text);
    if (!paths.length) throw new Error('Enter a file path to attach. Quote paths containing spaces.');
    const files = [];
    for (const value of paths) {
      if (/[\x00-\x1f\x7f-\x9f]/.test(value)) throw new Error('The file path contains terminal control characters.');
      const absolute = path.resolve(this.options.cwd || process.cwd(), value.startsWith('~/') ? path.join(this.options.userHome || process.env.HOME || '', value.slice(2)) : value);
      const stat = await fs.stat(absolute);
      if (!stat.isFile() && !stat.isDirectory()) throw new Error(`${safeText(value)} is not a file or folder.`);
      await fs.access(absolute, require('node:fs').constants.R_OK);
      if (!this.files.includes(absolute) && !files.includes(absolute)) files.push(absolute);
    }
    this.files.push(...files);
    this.notice(`${files.length} file${files.length === 1 ? '' : 's'} attached to your next message.`);
  }

  async createProject() {
    this.openForm('Project name', (name) => {
      if (!name.trim()) throw new Error('Enter a Project name.');
      const parentId = this.selection.projectId;
      const create = (policy) => this.action('Creating Project', async () => {
        const created = await this.client.createProject({ name: name.trim(), ...(parentId ? { parentId } : {}), policy });
        if (!created?.eeId) throw new Error('The new Project was not acknowledged with its EE ID.');
        this.catalog = await this.client.contexts();
        if (!this.catalog.projects?.some((project) => project.id === created.eeId)) throw new Error('The created Project is not visible in the shared catalog. Refresh /project before retrying.');
        if (this.run) this.newTask(undefined, true);
        this.selection = { projectId: created.eeId };
        this.notice(`Project created: ${name.trim()}`);
        this.agentPicker();
      });
      return this.action('Loading Project choices', async () => {
        const policies = await this.client.projectPolicies(parentId);
        if (!Array.isArray(policies) || !policies.length) throw new Error('This Project has no permitted child Project policies.');
        this.openPicker('Project access', policies.map((policy) => ({ label: policy.label, description: policy.description, value: policy.id })), (item) => create(item.value), {
          index: Math.max(0, policies.findIndex((policy) => policy.inheritsParentPolicy)),
        });
      });
    });
  }

  async createAgent() {
    if (!this.project()) return this.projectPicker();
    const sourceWorkerId = this.project().sourceWorkerId;
    if (this.project().isHome && !sourceWorkerId) return this.projectPicker({ forNewTask: true, afterSelect: () => this.createAgent() });
    if (!sourceWorkerId) throw new Error('This Project has no Agent template source. Connect or install a template before creating an Agent.');
    const projectId = this.selection.projectId;
    this.openForm('Agent name', async (name) => {
        if (!name.trim()) throw new Error('Enter an Agent name.');
        await this.modelPicker({ projectId, sourceWorkerId, workerId: sourceWorkerId }, (selection) => this.action('Creating Agent', async () => {
          const created = await this.client.createAgent({ ...selection, name: name.trim() });
          if (!created?.agentId) throw new Error('The new Agent was not acknowledged with an Agent ID.');
          this.catalog = await this.client.contexts();
          const agent = this.catalog.projects?.find((project) => project.id === projectId)?.agents?.find((candidate) => candidate.id === created.agentId);
          if (!agent) throw new Error('The created Agent is not visible in the shared catalog. Refresh /agent before retrying.');
          if (this.run) this.newTask(undefined, true);
          this.selection.projectId = projectId;
          this.setAgent(agent);
          this.notice(`Agent created: ${agent.name || name.trim()}`);
        }));
    });
  }

  async search(query) {
    if (!query.trim()) throw new Error('Enter search text.');
    const results = await this.client.search(query, this.run?.id);
    if (!Array.isArray(results?.matches)) throw new Error('The client did not return task search matches.');
    if (!results.matches.length) return this.notice(`No results for “${query}”.`);
    this.openPicker(`Search: ${query}${results.has_older ? ' · more older matches available' : ''}`, results.matches.map((match) => ({
      label: match.snippet, description: match.message?.role || '', value: match,
    })), (item) => this.resume(item.value.task_run_id));
  }

  async answerRequest() {
    const request = this.requests.values().next().value;
    if (!request) return this.notice('No pending request.');
    if (this.run?.archived || this.run?.writable === false) throw new Error('Restore or select a writable task before answering.');
    const data = request.payload;
    if (!data || typeof data !== 'object') throw new Error('This request has no complete response contract.');
    if (request.entry.request?.actionable === false) throw new Error('This request has no complete response provenance. A fresh request is required.');
    const finish = (answer) => this.action('Sending response', async () => {
      await this.client.respond(this.run.id, request, answer);
      this.requests.delete(request.id);
      this.notice('Response sent.');
      this.state = 'working';
      this.active = true;
      if (!this.followController || this.followController.signal.aborted) this.follow(this.run.id, this.run.lastReceipt || this.run);
    });
    const cancel = () => this.notice('Response picker closed. No response was sent. Use /answer to reopen.');
    this.notice(request.entry.text || 'The task needs your response.', 'warning');
    const detail = request.entry.request?.detail;
    if (detail && Object.keys(detail).length) this.notice(JSON.stringify(detail, null, 2));
    if (request.kind === 'approval') {
      const offered = data.detail?.permission_offer?.offered_choices;
      let choices;
      if (Array.isArray(offered)) {
        if (offered.some((choice) => !['accept', 'decline'].includes(choice.decision) || !['once', 'session', 'always'].includes(choice.persistence))) throw new Error('This request contains an unsupported permission choice. A fresh request is required.');
        choices = offered.map((choice) => ({
          label: `${choice.decision === 'accept' ? 'Allow' : 'Decline'} · ${choice.persistence}`,
          value: { decision: choice.decision, selected_persistence: choice.persistence },
        }));
        choices.push({ label: 'Cancel request', value: { decision: 'cancel' } });
      } else if (data.kind === 'permission' || data.method === 'item/permissions/requestApproval') {
        const permissions = data.detail?.permissions;
        if (!permissions || typeof permissions !== 'object' || Array.isArray(permissions)) throw new Error('The permission request has no exact permissions to select.');
        const granted = Object.fromEntries(['network', 'fileSystem'].filter((key) => permissions[key] != null).map((key) => [key, permissions[key]]));
        choices = ['turn', 'session'].map((scope) => ({ label: `Allow for this ${scope}`, value: { permissions: granted, scope } }));
        choices.push({ label: 'Decline', value: { permissions: {}, scope: 'turn' } });
      } else if (['application', 'browser_file_upload', 'browser_history_read'].includes(data.kind)) {
        choices = [{ label: 'Allow once', value: 'accept' }, { label: 'Decline', value: 'decline' }, { label: 'Cancel request', value: 'cancel' }];
      } else choices = [{ label: 'Allow once', value: 'accept' }, { label: 'Allow for this task', value: 'acceptForSession' }, { label: 'Decline', value: 'decline' }];
      this.openPicker('Permission request · choose explicitly', choices, (item) => finish({ kind: 'approval', decision: item.value }), { searchable: false, cancel });
    } else if (request.kind === 'user_input') {
      const questions = request.entry.request?.questions;
      const answers = {};
      const list = Array.isArray(questions) && questions.length ? questions : [{ id: 'answer', question: request.entry.text }];
      const ask = (index) => {
        if (index >= list.length) return finish({ kind: 'user_input', answers });
        const question = list[index];
        if (typeof question.id !== 'string' || !question.id) throw new Error('The question has no exact answer identity.');
        const take = (value) => { answers[question.id] = [value]; ask(index + 1); };
        const freeform = () => this.openForm(question.question || question.header || 'Your answer', (value) => {
          if (!value.trim()) throw new Error('Enter an answer.');
          take(value);
        }, { secret: question.isSecret === true, cancel });
        const options = question.options;
        if (Array.isArray(options) && options.length) {
          const rows = options.map((option) => ({ label: option.label, description: option.description, value: option.value }));
          if (question.isOther) rows.push({ label: 'Write another answer', value: null });
          this.openPicker(question.question || question.header || 'Choose your answer', rows, (item) => item.value === null ? freeform() : take(item.value), { cancel });
        } else freeform();
      };
      ask(0);
    } else if (request.kind === 'elicitation') {
      const schema = data.requested_schema;
      const properties = schema?.properties;
      const fields = properties && typeof properties === 'object' ? Object.entries(properties) : [];
      const required = new Set(schema?.required || []);
      const content = {};
      const submit = (value) => finish({ kind: 'elicitation', answer: { action: 'accept', content: value, _meta: null } });
      const ask = (index) => {
        if (index >= fields.length) return submit(content);
        const [name, field] = fields[index];
        const title = `${field.title || name}${required.has(name) ? '' : ' (optional)'}${field.description ? ` · ${field.description}` : ''}`;
        const take = (value) => { if (value !== undefined) content[name] = value; ask(index + 1); };
        if (Array.isArray(field.enum)) {
          const items = field.enum.map((value) => ({ label: typeof value === 'string' ? value : JSON.stringify(value), value }));
          if (!required.has(name)) items.push({ label: 'Skip', value: undefined });
          return this.openPicker(title, items, (item) => take(item.value), { cancel });
        }
        if (field.type === 'boolean') {
          const items = [{ label: 'Yes', value: true }, { label: 'No', value: false }];
          if (!required.has(name)) items.push({ label: 'Skip', value: undefined });
          return this.openPicker(title, items, (item) => take(item.value), { cancel });
        }
        this.openForm(title, (value) => {
          if (!value.trim()) { if (required.has(name)) throw new Error('This answer is required.'); return take(undefined); }
          if (field.type === 'number' || field.type === 'integer') {
            const number = Number(value);
            if (!Number.isFinite(number) || field.type === 'integer' && !Number.isInteger(number)) throw new Error('Enter a valid number.');
            if (field.minimum !== undefined && number < field.minimum || field.maximum !== undefined && number > field.maximum) throw new Error('The number is outside the requested range.');
            return take(number);
          }
          if (field.type === 'array' || field.type === 'object') {
            let parsed;
            try { parsed = JSON.parse(value); } catch { throw new Error('Enter valid JSON for this field.'); }
            if (field.type === 'array' ? !Array.isArray(parsed) : !parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error(`Enter a JSON ${field.type}.`);
            return take(parsed);
          }
          take(value);
        }, { initial: field.default === undefined ? '' : typeof field.default === 'string' ? field.default : JSON.stringify(field.default), secret: field.format === 'password', cancel });
      };
      this.openPicker('Respond to request', [{ label: 'Provide requested values', value: 'accept' },
        { label: 'Decline request', value: 'decline' }, { label: 'Cancel request', value: 'cancel' }], (item) => {
        if (item.value !== 'accept') return finish({ kind: 'elicitation', answer: { action: item.value, _meta: null } });
        if (fields.length) ask(0);
        else this.openForm('Your answer', (value) => {
          if (!value.trim()) throw new Error('Enter an answer.');
          submit(value);
        }, { cancel });
      }, { searchable: false, cancel });
    } else throw new Error(`Unsupported response form: ${safeText(request.kind)}. The request remains unanswered.`);
  }

  help() {
    const rows = [row('Talk to Octopus', 'bold'), row('Enter sends. Alt+Enter, Shift+Enter (supported terminals), or Ctrl+J adds a line.'), row('Pasted text stays editable. ↑/↓ recalls prompts at the start/end of a draft.'), row('Ctrl+R searches prompt history; selecting a prompt does not send it.'), row('Ctrl+P Projects · Ctrl+G Agents · Alt+M Models · Ctrl+T Tasks · Ctrl+N New'), row('Ctrl+O attaches a file or folder · Ctrl+Q answers a pending request.'), row('Arrows / Home / End edit. Ctrl+W/U/K deletes; Ctrl+Y restores. Ctrl+Z undoes.'), row('Esc closes a picker first, then stops the active task. Ctrl+C stops an active task.'), row('Ctrl+D, /exit, and /detach leave the terminal; shared tasks keep running.'), row('Quoted file paths are supported. Prefix a message with // to send a literal slash.'), row()];
    for (const command of COMMANDS) rows.push(...wrap(`${command.label}${command.args ? ` ${command.args}` : ''}  ${command.description}`, this.renderer.width).map((line) => row(line)));
    rows.push(row());
    this.print(rows);
  }

  command(name, args = '') {
    this.menu = null;
    switch (name) {
      case 'exit': case 'detach': return this.quit('detached');
      case 'stop': return this.stop();
      case 'help': return this.help();
      case 'new': return this.newTask(args);
      case 'project': return args === 'create' ? this.createProject() : this.action('Loading Projects', async () => { this.catalog = await this.client.contexts(); this.projectPicker(); });
      case 'agent': return args === 'create' ? this.createAgent() : this.action('Loading Agents', async () => { this.catalog = await this.client.contexts(); this.agentPicker(); });
      case 'model': return this.action('Loading models', () => this.modelPicker());
      case 'resume': return args ? this.action('Opening task', () => this.resume(args)) : this.action('Loading tasks', () => this.tasksPicker());
      case 'tasks': return this.action('Loading tasks', () => this.tasksPicker());
      case 'archived': return this.action('Loading archived tasks', () => this.tasksPicker(true));
      case 'home': return this.action('Opening Home', () => this.resume('home'));
      case 'attach':
        return args ? this.action('Attaching files', () => this.attach(args)) : this.openForm('File path · quote paths containing spaces', (value) => this.action('Attaching files', () => this.attach(value)));
      case 'files': return this.notice(this.files.length ? this.files.map((file, i) => `${i + 1}. ${file}`).join('\n') : 'No files attached. /attach adds a file.');
      case 'remove': {
        if (!args) return this.openPicker('Remove attachment', this.files.map((file, index) => ({ label: attachmentLabel(file), value: index })), (item) => { this.files.splice(item.value, 1); });
        if (args === 'all') this.files = [];
        else {
          const index = Number(args) - 1;
          if (!Number.isInteger(index) || index < 0 || index >= this.files.length) throw new Error('Choose an attachment number from /files, or /remove all.');
          this.files.splice(index, 1);
        }
        return this.render();
      }
      case 'history': return this.action('Loading transcript', async () => {
        if (!this.run) throw new Error('Select a task with /resume first.');
        const history = await this.client.history(this.run.id);
        if (!Array.isArray(history)) throw new Error('The client did not return a transcript.');
        for (const entry of history) this.renderer.append(entryRows(entry, this.renderer.width, { assistantName: this.assistantName(), outputRows: 200 }));
      });
      case 'answer': return this.action('Loading response', () => this.answerRequest());
      case 'search': return args ? this.action('Searching', () => this.search(args)) : this.openForm('Search text', (value) => this.action('Searching', () => this.search(value)));
      case 'rename': {
        const rename = (title) => this.action('Renaming task', async () => {
          if (!this.run) throw new Error('Select a task to rename.');
          if (!title.trim()) throw new Error('Enter a task title.');
          await this.client.rename(this.run.id, title.trim());
          this.run = await this.client.task(this.run.id);
          this.notice(`Task renamed: ${this.run.title}`);
        });
        return args ? rename(args) : this.openForm('Task title', rename, { initial: this.run?.title || '' });
      }
      case 'archive': case 'restore': return this.action(name === 'archive' ? 'Archiving task' : 'Restoring task', async () => {
        if (!this.run) throw new Error('Select a task first.');
        await this.client.archive(this.run.id, name === 'archive');
        this.run = await this.client.task(this.run.id);
        this.notice(this.run.archived ? 'Task archived. /restore makes it available again.' : 'Task restored.');
      });
      case 'delete': {
        if (!this.run || this.run.isHome) throw new Error('Select a task to delete. Home cannot be deleted.');
        if (this.active) throw new Error('Stop the task and wait for it to settle before deleting it.');
        const id = this.run.id;
        return this.openPicker(`Delete “${this.taskTitle()}”? Recorded evidence is retained.`,
          [{ label: 'Cancel', value: false }, { label: 'Delete task', value: true }],
          (item) => item.value && this.action('Deleting task', async () => {
            await this.client.deleteTask(id);
            this.notice('Task deleted.');
            await this.resume('home');
          }), { searchable: false });
      }
      case 'status': return this.action('Refreshing status', async () => {
        this.status = await this.client.status();
        this.notice(`Home: ${this.status.home || '(not supplied by client)'}\n${this.contextLabel()}\n${this.run ? `Task: ${this.taskTitle()} · ${this.run.state}` : 'New task'}`);
      });
      default: throw new Error(`Unknown command /${name}. Use /help or // to send text beginning with a slash.`);
    }
  }

  commandMenu() {
    if (this.form || (this.menu && this.menu.kind !== 'commands') || this.draft.literal) return;
    const match = this.draft.text.match(/^\/([a-z-]*)$/);
    if (!match || this.dismissedCommand === this.draft.text) {
      if (this.menu?.kind === 'commands') this.menu = null;
      return;
    }
    if (this.menu?.query !== match[1]) this.menu = { title: 'Commands', items: COMMANDS, query: match[1], index: 0, kind: 'commands', searchable: false };
  }

  recall(direction) {
    if (!this.history.length) return;
    if (this.historyIndex < 0) {
      if (direction > 0) return;
      this.historyDraft = this.draft.snapshot();
      this.historyIndex = this.history.length;
    }
    this.historyIndex = Math.max(0, Math.min(this.history.length, this.historyIndex + direction));
    if (this.historyIndex === this.history.length) {
      this.draft.restore(this.historyDraft);
      this.historyIndex = -1;
      this.historyDraft = null;
    } else this.draft.set(this.history[this.historyIndex], true);
  }

  key(event) {
    if (this.closed) return;
    const { key, text } = event;
    if (key === 'detach') return this.quit('detached');
    if (key === 'interrupt') {
      if (this.active || this.connectionLost || this.busy === 'Sending') { this.menu = null; this.form = null; return this.stop(); }
      if (this.menu || this.form) { this.menu = null; this.form = null; return this.render(); }
      if (this.draft.text || this.files.length) { this.draft.set(''); this.files = []; return this.render(); }
      return this.quit('detached');
    }
    if (key === 'escape') {
      if (this.menu) { const cancel = this.menu.cancel; this.dismissedCommand = this.draft.text; this.menu = null; cancel?.(); return this.render(); }
      if (this.form) { const cancel = this.form.cancel; this.form = null; cancel?.(); return this.render(); }
      if (this.active || this.connectionLost || this.busy === 'Sending') return this.stop();
      return;
    }
    if (key === 'redraw') { this.renderer.resize(); return this.render(); }
    if (this.menu && this.menu.kind !== 'commands') {
      const menu = this.menu;
      const visible = pickerRows(menu, this.renderer.width).items;
      if (key === 'up' || key === 'backtab') menu.index = Math.max(0, menu.index - 1);
      else if (key === 'down' || key === 'tab') menu.index = Math.min(visible.length - 1, menu.index + 1);
      else if (key === 'page-up') menu.index = Math.max(0, menu.index - 6);
      else if (key === 'page-down') menu.index = Math.min(visible.length - 1, menu.index + 6);
      else if (key === 'enter') {
        const item = visible[menu.index];
        if (!item || item.disabled) return;
        if (this.busy) return this.notice(`${this.busy}… The selection is kept.`, 'warning');
        this.menu = null;
        this.invoke(() => menu.choose(item));
      } else if (menu.searchable !== false && (key === 'text' || key === 'paste')) { menu.query += safeText(text).replace(/\n/g, ' '); menu.index = 0; }
      else if (key === 'backspace') { menu.query = menu.query.slice(0, previousBoundary(menu.query, menu.query.length)); menu.index = 0; }
      else if (key === 'kill-start') { menu.query = ''; menu.index = 0; }
      return this.render();
    }
    if (this.form) {
      const form = this.form;
      if (key === 'enter') {
        if (this.busy) return this.notice(`${this.busy}… Your answer is kept.`, 'warning');
        this.form = null;
        this.invoke(() => form.confirm(form.editor.text), 'Preparing', () => { if (!this.menu && !this.form) this.form = form; });
      } else if (key === 'text' || key === 'paste') form.editor.insert(text, key === 'paste');
      else if (key === 'up' || key === 'down') form.editor.vertical(key === 'up' ? -1 : 1, this.renderer.width);
      else form.editor.edit(key);
      return this.render();
    }
    if (this.menu?.kind === 'commands') {
      const items = pickerRows(this.menu, this.renderer.width).items;
      if (key === 'up') { this.menu.index = Math.max(0, this.menu.index - 1); return this.render(); }
      if (key === 'down') { this.menu.index = Math.min(items.length - 1, this.menu.index + 1); return this.render(); }
      if (key === 'tab' || key === 'enter') {
        const item = items[this.menu.index];
        if (!item) return;
        if (key === 'tab') { this.draft.set(`/${item.name} `); this.menu = null; return this.render(); }
        if (this.busy && !['stop', 'exit', 'detach', 'help'].includes(item.name)) return this.notice(`${this.busy}… Your command is kept.`, 'warning');
        this.draft.set('');
        this.menu = null;
        this.command(item.name);
        return this.render();
      }
    }
    if (key === 'enter') {
      const command = !this.draft.literal && this.draft.text.match(/^\/([a-z-]+)(?:\s+([\s\S]*))?$/);
      const attachedPath = !this.draft.literal && this.draft.text.startsWith('/attach/') ? this.draft.text.slice(7) : null;
      if (command || attachedPath) {
        const name = attachedPath ? 'attach' : command[1];
        if (this.busy && !['stop', 'exit', 'detach', 'help'].includes(name)) return this.notice(`${this.busy}… Your command is kept.`, 'warning');
        const snapshot = this.draft.snapshot();
        this.draft.set('');
        try { this.command(name, attachedPath || command[2]?.trim() || ''); }
        catch (error) { this.draft.restore(snapshot); throw error; }
      } else {
        if (!this.draft.literal && this.draft.text.startsWith('//')) this.draft.set(this.draft.text.slice(1), true);
        this.submit();
      }
    } else if (key === 'text' || key === 'paste') {
      this.historyIndex = -1;
      this.draft.insert(text, key === 'paste');
    } else if (key === 'up' || key === 'down') {
      if (!this.draft.vertical(key === 'up' ? -1 : 1, this.renderer.width)) this.recall(key === 'up' ? -1 : 1);
    } else if (key === 'history-search') {
      this.openPicker('Prompt history · Enter edits, it does not send', [...this.history].reverse().map((prompt) => ({ label: clip(prompt, 90), search: prompt, value: prompt })), (item) => this.draft.set(item.value, true));
    } else if (key === 'projects') this.command('project');
    else if (key === 'attach') this.command('attach');
    else if (key === 'answer') this.command('answer');
    else if (key === 'agents') this.command('agent');
    else if (key === 'models') this.command('model');
    else if (key === 'tasks') this.command('tasks');
    else if (key === 'new-task') { if (!this.busy) this.newTask(undefined, true); }
    else if (key === 'help') this.help();
    else this.draft.edit(key);
    this.commandMenu();
    this.renderSoon();
  }

  quit(reason) {
    if (this.closed) return;
    this.flushPending();
    this.closed = true;
    this.exitReason = reason;
    this.detachFollow();
    clearTimeout(this.renderTimer);
    this.renderTimer = null;
    this.renderer.close([row('Detached. Your tasks remain available in Octopus.', 'muted')]);
    this.maybeDone();
  }

  maybeDone() {
    if (!this.closed || this.pending.size) return;
    this.result = { reason: this.exitReason, runId: this.run?.id, selection: { ...this.selection }, draft: this.draft.text, files: [...this.files] };
    this.resolveDone(this.result);
  }

  cleanup() {
    this.closed = true;
    this.detachFollow();
    clearTimeout(this.renderTimer);
    this.decoder?.close();
    this.input.removeListener('data', this.onData);
    this.input.removeListener('end', this.onEnd);
    this.input.removeListener('error', this.onInputError);
    this.output.removeListener('resize', this.onResize);
    this.output.removeListener('error', this.onOutputError);
    if (this.onInterrupt) process.removeListener('SIGINT', this.onInterrupt);
    if (this.onTerminate) { process.removeListener('SIGTERM', this.onTerminate); process.removeListener('SIGHUP', this.onTerminate); }
    this.options.signal?.removeEventListener('abort', this.onAbort);
    this.input.setRawMode(this.rawBefore);
    if (!this.flowingBefore) this.input.pause();
    this.renderer.close();
  }
}

async function runInteractive(client, options = {}) {
  const input = options.input || process.stdin;
  const output = options.output || process.stdout;
  if (!input.isTTY || !output.isTTY || typeof input.setRawMode !== 'function') {
    const error = new Error('Interactive chat needs a terminal on both input and output. Open Octopus in a terminal, or use the noninteractive chat command.');
    error.code = 'OCTOPUS_TTY_REQUIRED';
    throw error;
  }
  if (options.term === 'dumb' || (!options.term && process.env.TERM === 'dumb')) {
    const error = new Error('This terminal cannot display interactive chat. Use a terminal with cursor support or the noninteractive chat command.');
    error.code = 'OCTOPUS_TERMINAL_UNSUPPORTED';
    throw error;
  }
  for (const method of ['status', 'contexts', 'send', 'follow']) if (typeof client?.[method] !== 'function') throw new TypeError(`The terminal client needs ${method}().`);
  return new InteractiveTerminal(client, options).start();
}

module.exports = { runInteractive, Draft, InputDecoder, splitPaths, COMMANDS };
