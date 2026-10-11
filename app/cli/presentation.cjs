'use strict';

const { stripVTControlCharacters } = require('node:util');

const segmenter = new Intl.Segmenter('en', { granularity: 'grapheme' });
const tones = {
  title: '\x1b[1;36m', bold: '\x1b[1m', muted: '\x1b[2m',
  user: '\x1b[1;36m', assistant: '\x1b[1;32m', error: '\x1b[31m',
  warning: '\x1b[33m', selected: '\x1b[7m', success: '\x1b[32m',
};
const NON_CONVERSATION_KINDS = new Set(['event', 'generic_activity', 'usage', 'turn_started', 'turn_completed', 'turn_control', 'interrupted_result', 'input_recorded']);

function isConversationEntry(entry) {
  if (entry.error || entry.request?.actionable === true || entry.activity?.failure) return true;
  if (NON_CONVERSATION_KINDS.has(entry.kind)) return false;
  return entry.role === 'tool' || Boolean(safeText(entry.text).trim()) || Boolean(entry.attachments?.length);
}

function safeText(value, { preserveTabs = false } = {}) {
  const text = stripVTControlCharacters(String(value ?? ''))
    .replace(/\r\n?/g, '\n')
    .replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g, '');
  return preserveTabs ? text : text.replace(/\t/g, '    ');
}

function graphemes(text) {
  return Array.from(segmenter.segment(text), (part) => ({ text: part.segment, index: part.index }));
}

function cellWidth(cluster) {
  if (!cluster || cluster === '\n' || /^\p{Mark}+$/u.test(cluster)) return 0;
  if (cluster === '\t') return 4;
  if (/\p{Extended_Pictographic}|\p{Regional_Indicator}|\u20e3/u.test(cluster)) return 2;
  const cp = cluster.codePointAt(0);
  return (cp >= 0x1100 && (
    cp <= 0x115f || cp === 0x2329 || cp === 0x232a ||
    (cp >= 0x2e80 && cp <= 0xa4cf && cp !== 0x303f) ||
    (cp >= 0xac00 && cp <= 0xd7a3) || (cp >= 0xf900 && cp <= 0xfaff) ||
    (cp >= 0xfe10 && cp <= 0xfe19) || (cp >= 0xfe30 && cp <= 0xfe6f) ||
    (cp >= 0xff00 && cp <= 0xff60) || (cp >= 0xffe0 && cp <= 0xffe6) ||
    (cp >= 0x20000 && cp <= 0x3fffd)
  )) ? 2 : 1;
}

function widthOf(text) {
  let width = 0;
  for (const part of segmenter.segment(safeText(text))) width += cellWidth(part.segment);
  return width;
}

function clip(text, width, ellipsis = '…') {
  text = safeText(text).replace(/\n/g, ' ');
  width = Math.max(0, width);
  if (widthOf(text) <= width) return text;
  const suffix = width >= widthOf(ellipsis) ? ellipsis : '';
  let result = '';
  let used = 0;
  for (const part of segmenter.segment(text)) {
    const next = cellWidth(part.segment);
    if (used + next > width - widthOf(suffix)) break;
    result += part.segment;
    used += next;
  }
  return result + suffix;
}

function wrap(text, width) {
  width = Math.max(1, width);
  const result = [];
  for (const line of safeText(text).split('\n')) {
    let current = '';
    let used = 0;
    for (const part of segmenter.segment(line)) {
      const size = cellWidth(part.segment);
      if (used && used + size > width) {
        result.push(current);
        current = '';
        used = 0;
      }
      if (size > width) {
        current += '�';
        used += 1;
      } else {
        current += part.segment;
        used += size;
      }
    }
    result.push(current);
  }
  return result;
}

function row(text = '', tone) {
  return { text: safeText(text), tone };
}

function composerLayout(text, cursor, width, maxRows = 6, prompt = '› ') {
  width = Math.max(1, width);
  const prefix = clip(prompt, Math.max(0, width - 1), '');
  const inset = widthOf(prefix);
  const continuation = ' '.repeat(inset);
  const contentWidth = Math.max(1, width - inset);
  const lines = [''];
  const positions = [[{ index: 0, col: inset }]];
  let r = 0;
  let col = 0;
  let caret = { row: 0, col: inset };
  for (const part of segmenter.segment(text)) {
    const size = cellWidth(part.segment);
    if (part.segment !== '\n' && col + size > contentWidth) {
      lines.push('');
      r += 1;
      col = 0;
      positions.push([{ index: part.index, col: inset }]);
    }
    if (part.index <= cursor) caret = { row: r, col: col + inset };
    if (part.segment === '\n') {
      lines.push('');
      r += 1;
      col = 0;
      positions.push([{ index: part.index + part.segment.length, col: inset }]);
    } else {
      lines[r] += size > contentWidth ? '�' : part.segment === '\t' ? '    ' : part.segment;
      col += Math.min(size, contentWidth);
      positions[r].push({ index: part.index + part.segment.length, col: col + inset });
    }
    if (part.index + part.segment.length <= cursor) caret = { row: r, col: col + inset };
  }
  if (caret.col >= width) {
    if (caret.row === lines.length - 1) {
      lines.push('');
      positions.push([{ index: cursor, col: inset }]);
    }
    caret = { row: caret.row + 1, col: inset };
  }
  const count = Math.max(1, maxRows);
  const start = Math.min(Math.max(0, caret.row - count + 1), Math.max(0, lines.length - count));
  return {
    rows: lines.slice(start, start + count).map((line, i) => row((start + i === 0 ? prefix : continuation) + line)),
    cursor: { row: caret.row - start, col: Math.min(width - 1, caret.col) },
    hiddenAbove: start, hiddenBelow: Math.max(0, lines.length - start - count), totalRows: lines.length,
    positions: positions.slice(start, start + count),
  };
}

function attachmentLabel(attachment) {
  return safeText(typeof attachment === 'string' ? attachment :
    attachment?.name ?? attachment?.fileName ?? attachment?.path ?? 'Attached file');
}

function entryRows(entry, width, { assistantName = 'Octopus', outputRows = 24 } = {}) {
  if (!entry || typeof entry !== 'object') throw new TypeError('Invalid transcript entry.');
  if (!isConversationEntry(entry)) return [];
  const rows = [];
  if (entry.role === 'tool') {
    const activity = entry.activity || {};
    const title = activity.title || entry.text || 'Tool activity';
    const status = entry.status ?? activity.status;
    const failed = status === 'failed' || status === 'error' || Number.isInteger(activity.exitCode) && activity.exitCode !== 0;
    rows.push(row(`${failed ? '×' : '•'} ${title}${status ? ` · ${status}` : ''}${Number.isInteger(activity.exitCode) ? ` · exit ${activity.exitCode}` : ''}`, failed ? 'error' : 'muted'));
    const command = activity.command ?? activity.script;
    if (command) rows.push(...wrap(command, Math.max(1, width - 2)).map((line) => row(`  ${line}`, 'muted')));
    const output = activity.output || [activity.stdout, activity.stderr].filter(Boolean).join('\n');
    if (output) {
      const lines = wrap(output, Math.max(1, width - 2));
      if (lines.length > outputRows) rows.push(row(`  … ${lines.length - outputRows} earlier output lines; /history for the task`, 'muted'));
      rows.push(...lines.slice(-outputRows).map((line) => row(`  ${line}`, failed ? 'error' : undefined)));
    }
    if (activity.failure && !activity.sections?.some((section) => section.stream === 'error')) rows.push(...wrap(activity.failure, Math.max(1, width - 2)).map((line) => row(`  ${line}`, 'error')));
    for (const notice of activity.notices || []) rows.push(...wrap(notice, Math.max(1, width - 2)).map((line) => row(`  ${line}`, 'muted')));
  } else {
    const label = entry.role === 'user' ? 'You' : entry.role === 'assistant' ? assistantName : entry.error ? 'Error' : 'Notice';
    rows.push(row(label, entry.error ? 'error' : entry.role === 'user' ? 'user' : entry.role === 'assistant' ? 'assistant' : 'muted'));
    let code = false;
    for (const line of safeText(entry.text).split('\n')) {
      if (/^\s*```/.test(line)) {
        code = !code;
        rows.push(row(`  ${line}`, 'muted'));
        continue;
      }
      for (const part of wrap(line, Math.max(1, width - 2))) rows.push(row(`  ${part}`, code ? 'muted' : entry.error ? 'error' : undefined));
    }
    for (const attachment of entry.attachments || []) {
      rows.push(row(`  + ${attachmentLabel(attachment)}`, 'muted'));
    }
    if (entry.metadata?.truncated) rows.push(row('  Partial transcript content; see the task result for the retained artifact.', 'warning'));
  }
  rows.push(row());
  return rows;
}

function pickerRows(picker, width, maxItems = 6) {
  const query = safeText(picker.query || '');
  const needle = query.toLocaleLowerCase();
  const items = picker.items.filter((item) => !needle || safeText(`${item.label} ${item.description || ''} ${item.search || ''}`).toLocaleLowerCase().includes(needle));
  const index = Math.max(0, Math.min(picker.index || 0, items.length - 1));
  const start = Math.max(0, Math.min(index - Math.floor(maxItems / 2), items.length - maxItems));
  const rows = [row(clip(picker.title, width), 'bold')];
  if (picker.searchable !== false) rows.push(row(clip(`  Search: ${query || 'type to filter'}`, width), 'muted'));
  if (picker.loading) rows.push(row('  Loading…', 'muted'));
  else if (!items.length) rows.push(row(clip(picker.empty || '  No matches', width), 'muted'));
  else items.slice(start, start + maxItems).forEach((item, offset) => {
    const selected = start + offset === index;
    rows.push(row(clip(`${selected ? '›' : ' '} ${item.checked ? '[✓] ' : ''}${item.label}${item.description ? `  ${item.description}` : ''}`, width), selected ? 'selected' : item.disabled ? 'muted' : undefined));
  });
  if (items.length > maxItems) rows.push(row(clip(`  ${index + 1}/${items.length} · ↑↓ move · PgUp/PgDn`, width), 'muted'));
  return { rows, items, index };
}

class InlineRenderer {
  constructor(output, { color = process.env.NO_COLOR === undefined } = {}) {
    this.output = output;
    this.color = color;
    this.cursorRow = 0;
    this.height = 0;
    this.columns = output.columns || 80;
    this.closed = false;
  }

  get width() { return Math.max(1, (this.output.columns || 80) - 1); }
  get availableRows() { return Math.max(2, (this.output.rows || 24) - 1); }

  paint(item) {
    const text = clip(item.text ?? item, this.width, '');
    const style = this.color && tones[item.tone];
    return style ? `${style}${text}\x1b[0m` : text;
  }

  clear() {
    if (!this.height) return;
    this.output.write(`\x1b[?25l\r${this.cursorRow ? `\x1b[${this.cursorRow}A` : ''}\x1b[J`);
    this.height = 0;
    this.cursorRow = 0;
  }

  frame(rows, cursor) {
    if (this.closed) return;
    this.clear();
    if (!rows.length) return;
    this.output.write('\x1b[?25l');
    this.output.write(rows.map((item) => this.paint(item)).join('\n\x1b[1G'));
    const target = cursor || { row: rows.length - 1, col: 0 };
    const up = rows.length - 1 - target.row;
    this.output.write(`\r${up > 0 ? `\x1b[${up}A` : ''}${target.col > 0 ? `\x1b[${target.col}C` : ''}\x1b[?25h`);
    this.height = rows.length;
    this.cursorRow = target.row;
    this.columns = this.output.columns || 80;
  }

  append(rows) {
    if (this.closed) return;
    this.clear();
    if (rows.length) this.output.write(`${rows.map((item) => this.paint(item)).join('\n\x1b[1G')}\n\x1b[1G`);
  }

  resize() {
    if (this.height) {
      this.clear();
      this.output.write('\r');
    }
  }

  close(rows = []) {
    if (this.closed) return;
    this.append(rows);
    this.output.write('\x1b[0m\x1b[?25h\x1b[?2004l');
    this.closed = true;
  }
}

module.exports = { safeText, graphemes, cellWidth, widthOf, clip, wrap, row, composerLayout, attachmentLabel, isConversationEntry, entryRows, pickerRows, InlineRenderer };
