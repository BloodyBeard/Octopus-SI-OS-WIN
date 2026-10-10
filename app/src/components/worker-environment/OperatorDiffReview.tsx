import type { DiffLineAnnotation, SelectedLineRange } from "@pierre/diffs";
import { useEffect, useMemo, useState } from "react";
import { Glyph } from "../IconGlyphs";
import { OCTOPUS_OPERATOR_DIFF_REVIEW_SEND_EVENT, type OperatorDiffReviewSendDetail } from "../../clientEvents";
import type { ProjectedAssistantCodeComment } from "../chat/assistantDirectives";

export type OperatorDiffReviewScope = {
  projectionRevision: string;
  runId: string;
  surfaceId: string;
  workerEnvironmentId: string;
};

export type OperatorDiffReviewSelection = {
  excerpt: string;
  excerptTruncated: boolean;
  path: string;
  range: SelectedLineRange;
  stage: "branch" | "staged" | "unstaged";
};

export type OperatorDiffReviewComment = {
  body: string;
  createdAt: number;
  endLine: number;
  endSide: "additions" | "deletions";
  excerpt: string;
  excerptTruncated: boolean;
  id: string;
  path: string;
  projectionRevision: string;
  stage: "branch" | "staged" | "unstaged";
  startLine: number;
  startSide: "additions" | "deletions";
  updatedAt: number;
};

export type OperatorDiffReviewAnnotationMetadata = {
  assistantComments: ProjectedAssistantCodeComment[];
  comments: OperatorDiffReviewComment[];
  pending: OperatorDiffReviewSelection | null;
};

type StoredOperatorDiffReview = {
  comments: OperatorDiffReviewComment[];
  runId: string;
  schemaVersion: 1;
  surfaceId: string;
  workerEnvironmentId: string;
};

type StoredDraftState = {
  comments: OperatorDiffReviewComment[];
  revision: number;
  status: "saved" | "saving" | "unavailable";
  storageError: string;
  storageKey: string;
};

type OperatorDiffReviewBarProps = {
  comments: OperatorDiffReviewComment[];
  expanded: boolean;
  onBeginComment: () => void;
  onClear: () => void;
  onDelete: (commentId: string) => void;
  onExpandedChange: (expanded: boolean) => void;
  onSend: () => void;
  onUpdate: (commentId: string, body: string) => void;
  scope: OperatorDiffReviewScope;
  selected: OperatorDiffReviewSelection | null;
  storageError: string;
  storageStatus: StoredDraftState["status"];
};

type OperatorDiffReviewAnnotationProps = {
  metadata: OperatorDiffReviewAnnotationMetadata;
  onCancelPending: () => void;
  onCreate: (body: string) => void;
  onDelete: (commentId: string) => void;
  onUpdate: (commentId: string, body: string) => void;
  projectionRevision: string;
};

export function useOperatorDiffReviewDraft(scope: OperatorDiffReviewScope) {
  const storageKey = useMemo(() => operatorReviewStorageKey(scope), [scope.runId, scope.surfaceId, scope.workerEnvironmentId]);
  const [state, setState] = useState<StoredDraftState>(() => loadStoredReview(scope, storageKey));

  useEffect(() => {
    setState(loadStoredReview(scope, storageKey));
  }, [scope.runId, scope.surfaceId, scope.workerEnvironmentId, storageKey]);

  useEffect(() => {
    if (!state.storageKey || state.storageKey !== storageKey || state.revision === 0) return;
    try {
      const stored: StoredOperatorDiffReview = {
        comments: state.comments,
        runId: scope.runId,
        schemaVersion: 1,
        surfaceId: scope.surfaceId,
        workerEnvironmentId: scope.workerEnvironmentId,
      };
      localStorage.setItem(storageKey, JSON.stringify(stored));
      setState((current) => current.storageKey === storageKey && current.status !== "saved"
        ? { ...current, status: "saved", storageError: "" }
        : current);
    } catch (error) {
      const message = error instanceof Error ? error.message : "Local review storage is unavailable";
      setState((current) => current.storageKey === storageKey
        ? { ...current, status: "unavailable", storageError: message }
        : current);
    }
  }, [scope.runId, scope.surfaceId, scope.workerEnvironmentId, state.comments, state.revision, state.storageKey, storageKey]);

  function mutate(mutator: (comments: OperatorDiffReviewComment[]) => OperatorDiffReviewComment[]) {
    setState((current) => ({
      ...current,
      comments: mutator(current.comments),
      revision: current.revision + 1,
      status: current.storageKey ? "saving" : "unavailable",
      storageError: current.storageKey ? "" : "A real run, Worker Environment, and projected surface are required",
    }));
  }

  function create(selection: OperatorDiffReviewSelection, body: string) {
    const trimmed = body.trim();
    if (!trimmed) return;
    const now = Date.now();
    const startSide = selection.range.side ?? "additions";
    const endSide = selection.range.endSide ?? startSide;
    mutate((comments) => [...comments, {
      body: trimmed,
      createdAt: now,
      endLine: selection.range.end,
      endSide,
      excerpt: selection.excerpt,
      excerptTruncated: selection.excerptTruncated,
      id: crypto.randomUUID(),
      path: selection.path,
      projectionRevision: scope.projectionRevision,
      stage: selection.stage,
      startLine: selection.range.start,
      startSide,
      updatedAt: now,
    }]);
  }

  function update(commentId: string, body: string) {
    const trimmed = body.trim();
    if (!trimmed) return;
    mutate((comments) => comments.map((comment) => comment.id === commentId
      ? { ...comment, body: trimmed, updatedAt: Date.now() }
      : comment));
  }

  function remove(commentId: string) {
    mutate((comments) => comments.filter((comment) => comment.id !== commentId));
  }

  function restore(removed: readonly { comment: OperatorDiffReviewComment; index: number }[]) {
    const ordered = [...removed].sort((left, right) => left.index - right.index);
    mutate((comments) => {
      const next = [...comments];
      const present = new Set(next.map((comment) => comment.id));
      for (const entry of ordered) {
        if (present.has(entry.comment.id)) continue;
        next.splice(Math.max(0, Math.min(entry.index, next.length)), 0, entry.comment);
        present.add(entry.comment.id);
      }
      return next;
    });
  }

  function clear() {
    if (storageKey) {
      try {
        localStorage.removeItem(storageKey);
      } catch (error) {
        const message = error instanceof Error ? error.message : "Local review storage is unavailable";
        setState({ comments: [], revision: 0, status: "unavailable", storageError: message, storageKey });
        return;
      }
    }
    setState({
      comments: [],
      revision: 0,
      status: storageKey ? "saved" : "unavailable",
      storageError: storageKey ? "" : "A real run, Worker Environment, and projected surface are required",
      storageKey,
    });
  }

  return {
    clear,
    comments: state.comments,
    create,
    remove,
    restore,
    storageError: state.storageError,
    storageStatus: state.status,
    update,
  };
}

export function buildOperatorDiffReviewAnnotations(
  comments: OperatorDiffReviewComment[],
  pending: OperatorDiffReviewSelection | null,
  path: string,
  projectionRevision: string,
  assistantComments: ProjectedAssistantCodeComment[] = [],
): DiffLineAnnotation<OperatorDiffReviewAnnotationMetadata>[] {
  const grouped = new Map<string, DiffLineAnnotation<OperatorDiffReviewAnnotationMetadata>>();
  for (const comment of assistantComments) {
    if (comment.path !== path) continue;
    const key = `additions:${comment.endLine}`;
    const existing = grouped.get(key);
    if (existing) {
      existing.metadata.assistantComments.push(comment);
      continue;
    }
    grouped.set(key, {
      lineNumber: comment.endLine,
      side: "additions",
      metadata: { assistantComments: [comment], comments: [], pending: null },
    });
  }
  for (const comment of comments) {
    if (comment.path !== path || comment.projectionRevision !== projectionRevision) continue;
    const key = `${comment.endSide}:${comment.endLine}`;
    const existing = grouped.get(key);
    if (existing) {
      existing.metadata.comments.push(comment);
      continue;
    }
    grouped.set(key, {
      lineNumber: comment.endLine,
      side: comment.endSide,
      metadata: { assistantComments: [], comments: [comment], pending: null },
    });
  }
  if (pending?.path === path) {
    const side = pending.range.endSide ?? pending.range.side ?? "additions";
    const key = `${side}:${pending.range.end}`;
    const existing = grouped.get(key);
    if (existing) existing.metadata.pending = pending;
    else grouped.set(key, {
      lineNumber: pending.range.end,
      side,
      metadata: { assistantComments: [], comments: [], pending },
    });
  }
  return [...grouped.values()].sort((left, right) => left.lineNumber - right.lineNumber);
}

export function OperatorDiffReviewAnnotation({ metadata, onCancelPending, onCreate, onDelete, onUpdate, projectionRevision }: OperatorDiffReviewAnnotationProps) {
  return (
    <div className="we-diff-review-annotation-stack">
      {metadata.assistantComments.map((comment) => <AssistantCodeReviewComment comment={comment} key={comment.id} variant="annotation" />)}
      {metadata.comments.map((comment) => (
        <OperatorDiffReviewCommentEditor
          comment={comment}
          key={comment.id}
          onDelete={onDelete}
          onUpdate={onUpdate}
          stale={Boolean(comment.projectionRevision && comment.projectionRevision !== projectionRevision)}
          variant="annotation"
        />
      ))}
      {metadata.pending ? (
        <OperatorDiffReviewComposer onCancel={onCancelPending} onSubmit={onCreate} selection={metadata.pending} />
      ) : null}
    </div>
  );
}

export function AssistantCodeReviewBar({ comments, expanded, onExpandedChange, onReveal }: {
  comments: ProjectedAssistantCodeComment[];
  expanded: boolean;
  onExpandedChange: (expanded: boolean) => void;
  onReveal: (comment: ProjectedAssistantCodeComment) => void;
}) {
  if (!comments.length) return null;
  const fileCount = new Set(comments.map((comment) => comment.path)).size;
  return (
    <section className={`we-diff-review${expanded ? " is-expanded" : ""}`}>
      {expanded ? (
        <div className="we-diff-review-summary">
          <header><div><strong>Assistant review</strong><span>{comments.length} across {fileCount} {fileCount === 1 ? "file" : "files"}</span></div></header>
          <div className="we-diff-review-summary-list">
            {comments.map((comment) => <AssistantCodeReviewComment comment={comment} key={comment.id} onReveal={onReveal} variant="summary" />)}
          </div>
        </div>
      ) : null}
      <div className="we-diff-review-strip">
        <button aria-expanded={expanded} className="we-diff-review-text-action" onClick={() => onExpandedChange(!expanded)} type="button">
          <Glyph name="octopus" size={13} />
          {comments.length} assistant {comments.length === 1 ? "comment" : "comments"}
          <Glyph className={expanded ? "-rotate-180" : ""} name="chevronDown" size={12} />
        </button>
      </div>
    </section>
  );
}

export function OperatorDiffReviewBar({ comments, expanded, onBeginComment, onClear, onDelete, onExpandedChange, onSend, onUpdate, scope, selected, storageError, storageStatus }: OperatorDiffReviewBarProps) {
  const fileCount = new Set(comments.map((comment) => comment.path)).size;
  const canSend = comments.length > 0 && Boolean(scope.runId && scope.workerEnvironmentId && scope.surfaceId);
  if (!selected && comments.length === 0) return null;
  return (
    <section className={`we-diff-review${expanded ? " is-expanded" : ""}`}>
      {expanded && comments.length ? (
        <div className="we-diff-review-summary">
          <header>
            <div>
              <strong>Review comments</strong>
              <span>{comments.length} across {fileCount} {fileCount === 1 ? "file" : "files"}</span>
            </div>
            <button aria-label="Clear review comments" className="we-diff-review-icon-action" onClick={onClear} title="Clear" type="button"><Glyph name="trash" size={13} /></button>
          </header>
          <div className="we-diff-review-summary-list">
            {comments.map((comment) => (
              <OperatorDiffReviewCommentEditor
                comment={comment}
                key={comment.id}
                onDelete={onDelete}
                onUpdate={onUpdate}
                stale={Boolean(comment.projectionRevision && comment.projectionRevision !== scope.projectionRevision)}
                variant="summary"
              />
            ))}
          </div>
        </div>
      ) : null}
      <div className="we-diff-review-strip">
        {selected ? (
          <button className="we-diff-review-text-action" onClick={onBeginComment} title={reviewSelectionLabel(selected)} type="button"><Glyph name="plus" size={12} />Comment</button>
        ) : null}
        {comments.length ? (
          <button aria-expanded={expanded} className="we-diff-review-text-action" onClick={() => onExpandedChange(!expanded)} title={storageError || (storageStatus === "saving" ? "Saving local draft" : "Local review draft")} type="button">
            <Glyph name="changes" size={13} />
            {comments.length} {comments.length === 1 ? "comment" : "comments"}
            <Glyph className={expanded ? "-rotate-180" : ""} name="chevronDown" size={12} />
          </button>
        ) : null}
        {comments.length ? <button className="we-diff-review-send" disabled={!canSend} onClick={onSend} type="button"><Glyph name="send" size={13} />Send</button> : null}
      </div>
    </section>
  );
}

export function dispatchOperatorDiffReview(scope: OperatorDiffReviewScope, comments: OperatorDiffReviewComment[]) {
  if (!scope.runId || !scope.workerEnvironmentId || !scope.surfaceId || comments.length === 0) return;
  const detail = operatorDiffReviewSendDetail(scope, comments);
  window.dispatchEvent(new CustomEvent<OperatorDiffReviewSendDetail>(OCTOPUS_OPERATOR_DIFF_REVIEW_SEND_EVENT, {
    cancelable: true,
    detail,
  }));
}

function AssistantCodeReviewComment({ comment, onReveal, variant }: {
  comment: ProjectedAssistantCodeComment;
  onReveal?: (comment: ProjectedAssistantCodeComment) => void;
  variant: "annotation" | "summary";
}) {
  const priority = comment.priority === undefined || /^\[P\d\]/i.test(comment.title) ? "" : `[P${comment.priority}] `;
  const range = comment.startLine === comment.endLine ? `L${comment.endLine}` : `L${comment.startLine}–${comment.endLine}`;
  return (
    <article className={`we-diff-review-comment is-${variant}`}>
      <header>
        <span className="we-diff-review-avatar"><Glyph name="octopus" size={13} /></span>
        <span><strong>{priority}{comment.title}</strong><small>{comment.path} · {range}</small></span>
      </header>
      <p>{comment.body}</p>
      {onReveal ? <footer><span>Assistant comment</span><button onClick={() => onReveal(comment)} type="button">Show</button></footer> : null}
    </article>
  );
}

function OperatorDiffReviewComposer({ onCancel, onSubmit, selection }: {
  onCancel: () => void;
  onSubmit: (body: string) => void;
  selection: OperatorDiffReviewSelection;
}) {
  const [body, setBody] = useState("");
  const trimmed = body.trim();
  return (
    <div className="we-diff-review-comment composer-surface-chrome is-composer" onPointerDown={(event) => event.stopPropagation()}>
      <header>
        <span className="we-diff-review-avatar"><Glyph name="user" size={13} /></span>
        <strong>Operator</strong>
        <small>{reviewSelectionLabel(selection)}</small>
      </header>
      <textarea
        aria-label="Review comment"
        autoFocus
        onChange={(event) => setBody(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onCancel();
          }
          if ((event.metaKey || event.ctrlKey) && event.key === "Enter" && trimmed) {
            event.preventDefault();
            onSubmit(trimmed);
          }
        }}
        placeholder="Request a change"
        value={body}
      />
      {selection.excerpt ? <ReviewExcerpt excerpt={selection.excerpt} truncated={selection.excerptTruncated} /> : null}
      <footer>
        <button onClick={onCancel} type="button">Cancel</button>
        <button className="is-primary" disabled={!trimmed} onClick={() => onSubmit(trimmed)} type="button">Comment</button>
      </footer>
    </div>
  );
}

function OperatorDiffReviewCommentEditor({ comment, onDelete, onUpdate, stale, variant }: {
  comment: OperatorDiffReviewComment;
  onDelete: (commentId: string) => void;
  onUpdate: (commentId: string, body: string) => void;
  stale: boolean;
  variant: "annotation" | "summary";
}) {
  const [editing, setEditing] = useState(false);
  const [body, setBody] = useState(comment.body);
  useEffect(() => {
    if (!editing) setBody(comment.body);
  }, [comment.body, editing]);
  const trimmed = body.trim();
  const label = reviewCommentLocation(comment);
  return (
    <article className={`we-diff-review-comment composer-surface-chrome is-${variant}${stale ? " is-stale" : ""}`} onPointerDown={(event) => event.stopPropagation()}>
      <header>
        <span className="we-diff-review-avatar"><Glyph name="user" size={13} /></span>
        <strong>Operator</strong>
        <small>{label}</small>
        {stale ? <em>Projection changed</em> : null}
      </header>
      {editing ? (
        <textarea
          aria-label={`Edit review comment on ${label}`}
          autoFocus
          onChange={(event) => setBody(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              setBody(comment.body);
              setEditing(false);
            }
            if ((event.metaKey || event.ctrlKey) && event.key === "Enter" && trimmed) {
              event.preventDefault();
              onUpdate(comment.id, trimmed);
              setEditing(false);
            }
          }}
          value={body}
        />
      ) : <p>{comment.body}</p>}
      {comment.excerpt ? <ReviewExcerpt excerpt={comment.excerpt} truncated={comment.excerptTruncated} /> : null}
      <footer>
        <button className="is-danger" onClick={() => onDelete(comment.id)} type="button">Delete</button>
        {editing ? (
          <span>
            <button onClick={() => { setBody(comment.body); setEditing(false); }} type="button">Cancel</button>
            <button className="is-primary" disabled={!trimmed || trimmed === comment.body} onClick={() => { onUpdate(comment.id, trimmed); setEditing(false); }} type="button">Save</button>
          </span>
        ) : (
          <button onClick={() => setEditing(true)} type="button"><Glyph name="edit" size={12} />Edit</button>
        )}
      </footer>
    </article>
  );
}

function ReviewExcerpt({ excerpt, truncated }: { excerpt: string; truncated: boolean }) {
  return (
    <details className="we-diff-review-excerpt">
      <summary>Selected lines{truncated ? " · excerpt" : ""}</summary>
      <pre>{excerpt}</pre>
    </details>
  );
}

function operatorReviewStorageKey(scope: OperatorDiffReviewScope): string {
  if (!scope.runId || !scope.workerEnvironmentId || !scope.surfaceId) return "";
  const identity = [scope.runId, scope.workerEnvironmentId, scope.surfaceId]
    .map((value) => encodeURIComponent(value))
    .join(":");
  return `octopus.operator-diff-review.v1:${identity}`;
}

function loadStoredReview(scope: OperatorDiffReviewScope, storageKey: string): StoredDraftState {
  if (!storageKey) return {
    comments: [],
    revision: 0,
    status: "unavailable",
    storageError: "A real run, Worker Environment, and projected surface are required",
    storageKey,
  };
  try {
    const raw = localStorage.getItem(storageKey);
    if (!raw) return { comments: [], revision: 0, status: "saved", storageError: "", storageKey };
    const parsed: unknown = JSON.parse(raw);
    if (!isStoredReview(parsed, scope)) throw new Error("The saved operator review draft does not match this projected surface");
    return { comments: parsed.comments, revision: 0, status: "saved", storageError: "", storageKey };
  } catch (error) {
    return {
      comments: [],
      revision: 0,
      status: "unavailable",
      storageError: error instanceof Error ? error.message : "The saved operator review draft is unreadable",
      storageKey,
    };
  }
}

function isStoredReview(value: unknown, scope: OperatorDiffReviewScope): value is StoredOperatorDiffReview {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  return record.schemaVersion === 1
    && record.runId === scope.runId
    && record.workerEnvironmentId === scope.workerEnvironmentId
    && record.surfaceId === scope.surfaceId
    && Array.isArray(record.comments)
    && record.comments.every(isOperatorDiffReviewComment);
}

function isOperatorDiffReviewComment(value: unknown): value is OperatorDiffReviewComment {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  return typeof record.id === "string" && Boolean(record.id)
    && typeof record.path === "string" && Boolean(record.path)
    && typeof record.body === "string" && Boolean(record.body.trim())
    && (record.stage === "branch" || record.stage === "staged" || record.stage === "unstaged")
    && (record.startSide === "additions" || record.startSide === "deletions")
    && (record.endSide === "additions" || record.endSide === "deletions")
    && isPositiveInteger(record.startLine)
    && isPositiveInteger(record.endLine)
    && typeof record.excerpt === "string"
    && typeof record.excerptTruncated === "boolean"
    && typeof record.projectionRevision === "string"
    && typeof record.createdAt === "number" && Number.isFinite(record.createdAt)
    && typeof record.updatedAt === "number" && Number.isFinite(record.updatedAt);
}

function isPositiveInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value > 0;
}

function reviewSelectionLabel(selection: OperatorDiffReviewSelection): string {
  const startSide = selection.range.side ?? "additions";
  const endSide = selection.range.endSide ?? startSide;
  return `${selection.path} · ${lineRangeLabel(selection.range.start, startSide, selection.range.end, endSide)}`;
}

function reviewCommentLocation(comment: OperatorDiffReviewComment): string {
  return `${comment.path} · ${lineRangeLabel(comment.startLine, comment.startSide, comment.endLine, comment.endSide)}`;
}

function lineRangeLabel(startLine: number, startSide: "additions" | "deletions", endLine: number, endSide: "additions" | "deletions"): string {
  const start = `${startSide === "deletions" ? "L" : "R"}${startLine}`;
  const end = `${endSide === "deletions" ? "L" : "R"}${endLine}`;
  return start === end ? `line ${start}` : `lines ${start}–${end}`;
}

function operatorDiffReviewSendDetail(scope: OperatorDiffReviewScope, comments: OperatorDiffReviewComment[]): OperatorDiffReviewSendDetail {
  const sorted = [...comments].sort((left, right) => left.path.localeCompare(right.path) || left.startLine - right.startLine || left.createdAt - right.createdAt);
  const context = [
    "Operator review context",
    `Run: ${scope.runId}`,
    `Worker Environment: ${scope.workerEnvironmentId}`,
    `Projected surface: ${scope.surfaceId}`,
    scope.projectionRevision ? `Projection revision: ${scope.projectionRevision}` : "",
    "The review was authored against a read-only projected diff. It requests changes but performs no Git or filesystem action.",
  ].filter(Boolean).join("\n");
  const text = [
    "Please address this operator review of the projected changes:",
    ...sorted.map((comment, index) => {
      const excerpt = comment.excerpt
        ? `\n\nProjected excerpt${comment.excerptTruncated ? " (truncated)" : ""}:\n${indentBlock(comment.excerpt)}`
        : "";
      const revision = comment.projectionRevision
        ? `; projection revision ${comment.projectionRevision}${comment.projectionRevision !== scope.projectionRevision ? "; superseded" : ""}`
        : "";
      return `${index + 1}. ${reviewCommentLocation(comment)} [${comment.stage}${revision}]\n${indentBlock(comment.body)}${excerpt}`;
    }),
  ].join("\n\n");
  return {
    comments: sorted.map((comment) => ({
      body: comment.body,
      endLine: comment.endLine,
      endSide: comment.endSide,
      excerpt: comment.excerpt,
      path: comment.path,
      projectionRevision: comment.projectionRevision,
      stage: comment.stage,
      startLine: comment.startLine,
      startSide: comment.startSide,
    })),
    context,
    runId: scope.runId,
    schemaVersion: 1,
    surfaceId: scope.surfaceId,
    text,
    workerEnvironmentId: scope.workerEnvironmentId,
  };
}

function indentBlock(value: string): string {
  return value.split("\n").map((line) => `   ${line}`).join("\n");
}
