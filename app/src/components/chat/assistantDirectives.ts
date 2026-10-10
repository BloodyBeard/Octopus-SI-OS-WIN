import type { ChatRenderMessage } from "./types";

export type AssistantCodeCommentDirective = {
  body: string;
  confidence?: number;
  directiveIndex: number;
  endLine: number;
  path: string;
  priority?: number;
  startLine: number;
  title: string;
};

export type ProjectedAssistantCodeComment = AssistantCodeCommentDirective & {
  id: string;
  sourceMessageId: string;
};

export type AssistantInlineVisualizationDirective = {
  directiveIndex: number;
  path: string;
};

export type AssistantVisualizationTarget = {
  path: string;
  revision: string;
  surfaceId: string;
  workerEnvironmentId: string;
};

export type AssistantDirectiveProjection = {
  codeComments: AssistantCodeCommentDirective[];
  inlineVisualizations: AssistantInlineVisualizationDirective[];
  markdown: string;
};

type ParsedDirective = {
  attributes: Record<string, string>;
  name: string;
};

export function projectAssistantDirectives(text: string, enabled = true): AssistantDirectiveProjection {
  if (!enabled || !text.includes("::")) return { codeComments: [], inlineVisualizations: [], markdown: text };
  const codeComments: AssistantCodeCommentDirective[] = [];
  const inlineVisualizations: AssistantInlineVisualizationDirective[] = [];
  const visibleLines: string[] = [];
  let fence: MarkdownFence | null = null;
  let directiveIndex = 0;
  for (const line of text.split(/\r?\n/)) {
    if (fence) {
      visibleLines.push(line);
      if (closesMarkdownFence(line, fence)) fence = null;
      continue;
    }
    const openingFence = markdownFence(line);
    if (openingFence) {
      fence = openingFence;
      visibleLines.push(line);
      continue;
    }
    if (/^(?: {4}|\t)/.test(line)) {
      visibleLines.push(line);
      continue;
    }
    const directive = parseDirectiveLine(line);
    if (!directive) {
      visibleLines.push(line);
      continue;
    }
    if (directive.name === "code-comment") {
      const comment = codeCommentDirective(directive.attributes, directiveIndex);
      if (!comment) {
        visibleLines.push(line);
        continue;
      }
      codeComments.push(comment);
      directiveIndex += 1;
      continue;
    }
    if (directive.name === "codex-inline-vis") {
      const path = directive.attributes.file?.trim();
      if (!path) {
        visibleLines.push(line);
        continue;
      }
      inlineVisualizations.push({ directiveIndex, path });
      directiveIndex += 1;
      continue;
    }
    visibleLines.push(line);
  }
  return {
    codeComments,
    inlineVisualizations,
    markdown: normalizeVisibleMarkdown(visibleLines),
  };
}

export function assistantCodeCommentKey(comment: Omit<AssistantCodeCommentDirective, "directiveIndex">): string {
  return [
    normalizeDirectivePath(comment.path),
    comment.startLine,
    comment.endLine,
    comment.priority ?? "",
    comment.title.trim(),
    comment.body.trim(),
  ].join("\u0000");
}

export function projectAssistantCodeComments(
  messages: readonly ChatRenderMessage[],
  runId: string,
): ProjectedAssistantCodeComment[] {
  const seen = new Set<string>();
  const comments: ProjectedAssistantCodeComment[] = [];
  for (const message of messages) {
    if (message.role === "user" || message.error || message.pending || message.terminal === false) continue;
    if (message.runId && message.runId !== runId) continue;
    const kind = message.eventKind ?? "message";
    if (kind !== "message" && kind !== "message_delta") continue;
    const projection = projectAssistantDirectives(message.text);
    for (const comment of projection.codeComments) {
      const key = assistantCodeCommentKey(comment);
      if (seen.has(key)) continue;
      seen.add(key);
      comments.push({
        ...comment,
        id: `${message.id}:code-comment:${comment.directiveIndex}`,
        sourceMessageId: message.id,
      });
    }
  }
  return comments;
}

export function normalizeDirectivePath(value: string): string {
  return value.trim().replaceAll("\\", "/").replace(/^\.\/+/, "").replace(/\/{2,}/g, "/");
}

export function directivePathWithinRoot(value: string, root: string): string | null {
  const rawPath = value.trim().replaceAll("\\", "/");
  if (!rawPath || rawPath.includes("\0")) return null;
  const path = normalizeDirectivePath(value);
  const normalizedRoot = normalizeDirectivePath(root).replace(/\/$/, "");
  if (!path || !normalizedRoot || path === normalizedRoot) return null;
  let relativePath = path;
  if (path.startsWith(`${normalizedRoot}/`)) relativePath = path.slice(normalizedRoot.length + 1);
  const rootName = normalizedRoot.split("/").filter(Boolean).at(-1) ?? "";
  if (relativePath === path && rootName && path.startsWith(`${rootName}/`)) relativePath = path.slice(rootName.length + 1);
  if (relativePath === path && /^(?:\/|[A-Za-z]:\/)/.test(rawPath)) return null;
  if (!relativePath || relativePath.split("/").some((segment) => segment === "..")) return null;
  return relativePath;
}

function codeCommentDirective(attributes: Record<string, string>, directiveIndex: number): AssistantCodeCommentDirective | null {
  const title = attributes.title?.trim();
  const body = attributes.body?.trim();
  const path = attributes.file?.trim();
  if (!title || !body || !path) return null;
  const authoredStart = optionalInteger(attributes.start);
  const authoredEnd = optionalInteger(attributes.end);
  if (attributes.start !== undefined && authoredStart === undefined) return null;
  if (attributes.end !== undefined && authoredEnd === undefined) return null;
  const startLine = Math.max(1, authoredStart ?? 1);
  const endLine = Math.max(startLine, authoredEnd ?? startLine);
  const priority = optionalPriority(attributes.priority);
  if (attributes.priority !== undefined && priority === undefined) return null;
  if (priority !== undefined && (priority < 0 || priority > 3)) return null;
  const confidence = optionalNumber(attributes.confidence);
  if (attributes.confidence !== undefined && confidence === undefined) return null;
  return {
    body,
    ...(confidence === undefined ? {} : { confidence }),
    directiveIndex,
    endLine,
    path: normalizeDirectivePath(path),
    ...(priority === undefined ? {} : { priority }),
    startLine,
    title,
  };
}

function parseDirectiveLine(line: string): ParsedDirective | null {
  const trimmed = line.trim();
  const match = /^(:{1,3})([a-z][a-z0-9-]*)\{([\s\S]*)\}$/.exec(trimmed);
  if (!match) return null;
  if (match[2] !== "code-comment" && match[1] !== "::") return null;
  const attributes = parseDirectiveAttributes(match[3]);
  return attributes ? { attributes, name: match[2] } : null;
}

function parseDirectiveAttributes(source: string): Record<string, string> | null {
  const attributes: Record<string, string> = {};
  let cursor = 0;
  while (cursor < source.length) {
    while (/\s/.test(source[cursor] ?? "")) cursor += 1;
    if (cursor >= source.length) break;
    const nameMatch = /^[A-Za-z][A-Za-z0-9_-]*/.exec(source.slice(cursor));
    if (!nameMatch) return null;
    const name = nameMatch[0];
    if (Object.prototype.hasOwnProperty.call(attributes, name)) return null;
    cursor += name.length;
    while (/\s/.test(source[cursor] ?? "")) cursor += 1;
    if (source[cursor] !== "=") return null;
    cursor += 1;
    while (/\s/.test(source[cursor] ?? "")) cursor += 1;
    if (source[cursor] === '"') {
      const parsed = parseQuotedAttribute(source, cursor + 1);
      if (!parsed) return null;
      attributes[name] = parsed.value;
      cursor = parsed.cursor;
      continue;
    }
    const valueMatch = /^[^\s}]+/.exec(source.slice(cursor));
    if (!valueMatch) return null;
    attributes[name] = valueMatch[0];
    cursor += valueMatch[0].length;
  }
  return attributes;
}

function parseQuotedAttribute(source: string, start: number): { cursor: number; value: string } | null {
  let cursor = start;
  let value = "";
  while (cursor < source.length) {
    const character = source[cursor];
    if (character === '"') return { cursor: cursor + 1, value };
    if (character !== "\\") {
      value += character;
      cursor += 1;
      continue;
    }
    cursor += 1;
    if (cursor >= source.length) return null;
    const escaped = source[cursor];
    value += escaped === "n" ? "\n" : escaped === "r" ? "\r" : escaped === "t" ? "\t" : escaped;
    cursor += 1;
  }
  return null;
}

function optionalInteger(value: string | undefined): number | undefined {
  if (value === undefined || !/^-?\d+$/.test(value.trim())) return undefined;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : undefined;
}

function optionalPriority(value: string | undefined): number | undefined {
  if (value === undefined) return undefined;
  return optionalInteger(value.trim().replace(/^[Pp]/, ""));
}

function optionalNumber(value: string | undefined): number | undefined {
  if (value === undefined || !value.trim()) return undefined;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : undefined;
}

function normalizeVisibleMarkdown(lines: string[]): string {
  const visible = [...lines];
  while (visible[0]?.trim() === "") visible.shift();
  while (visible.at(-1)?.trim() === "") visible.pop();
  return visible.join("\n").replace(/\n{3,}/g, "\n\n");
}

type MarkdownFence = {
  length: number;
  marker: "`" | "~";
};

function markdownFence(line: string): MarkdownFence | null {
  const match = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
  if (!match) return null;
  const marker = match[1][0] as MarkdownFence["marker"];
  if (marker === "`" && match[2].includes("`")) return null;
  return { length: match[1].length, marker };
}

function closesMarkdownFence(line: string, fence: MarkdownFence): boolean {
  const match = /^ {0,3}(`+|~+)[ \t]*$/.exec(line);
  return Boolean(match && match[1][0] === fence.marker && match[1].length >= fence.length);
}
