export type ClientSurfaceRuntimeIssueKind = "source" | "script";

export type ClientSurfaceRuntimeIssue = {
  extension_id: string;
  document_id: string;
  source_digest: string;
  kind: ClientSurfaceRuntimeIssueKind;
  message: string;
  first_observed_at: string;
  last_observed_at: string;
  count: number;
};

type IssueScope = {
  extensionId?: string;
  documentId: string;
  sourceDigest: string;
};

const MAX_ISSUES = 64;
const MAX_MESSAGE_LENGTH = 2_000;
const issues = new Map<string, ClientSurfaceRuntimeIssue>();

function issueKey(scope: IssueScope, kind: ClientSurfaceRuntimeIssueKind): string {
  return JSON.stringify([scope.extensionId ?? "", scope.documentId, scope.sourceDigest, kind]);
}

export function reportClientSurfaceRuntimeIssue(scope: IssueScope, kind: ClientSurfaceRuntimeIssueKind, message: string): void {
  const text = message.trim().slice(0, MAX_MESSAGE_LENGTH);
  if (!text) return;
  const key = issueKey(scope, kind);
  const now = new Date().toISOString();
  const current = issues.get(key);
  issues.delete(key);
  issues.set(key, current && current.message === text
    ? { ...current, last_observed_at: now, count: current.count + 1 }
    : {
      extension_id: scope.extensionId ?? "",
      document_id: scope.documentId,
      source_digest: scope.sourceDigest,
      kind,
      message: text,
      first_observed_at: now,
      last_observed_at: now,
      count: 1,
    });
  while (issues.size > MAX_ISSUES) issues.delete(issues.keys().next().value as string);
}

export function clearClientSurfaceRuntimeIssue(scope: IssueScope, kind: ClientSurfaceRuntimeIssueKind): void {
  issues.delete(issueKey(scope, kind));
}

export function clientSurfaceRuntimeIssues(): ClientSurfaceRuntimeIssue[] {
  return [...issues.values()].map((issue) => ({ ...issue }));
}
