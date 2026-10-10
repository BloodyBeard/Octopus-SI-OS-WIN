export const CLIENT_SURFACE_ACTION_EVENT = "octopus:client-surface-action" as const;
export const MCP_RESOURCE_ACTION_EVENT = "octopus:mcp-resource-action" as const;
export const OCTOPUS_OPERATOR_DIFF_REVIEW_SEND_EVENT = "octopus:operator-diff-review-send" as const;

export type McpResourceActionDetail = {
  schemaVersion: 1;
  action: string;
  payload: unknown;
  provenance: {
    artifactId: string;
    interfaceId: string;
    messageId: string;
    movementSeq: number;
    resourceUri: string;
    runId: string;
    server: string;
    tool: string;
    traceId: string;
  };
};

export type OperatorDiffReviewSendDetail = {
  comments: Array<{
    body: string;
    endLine: number;
    endSide: "additions" | "deletions";
    excerpt: string;
    path: string;
    projectionRevision: string;
    stage: "branch" | "staged" | "unstaged";
    startLine: number;
    startSide: "additions" | "deletions";
  }>;
  context: string;
  runId: string;
  schemaVersion: 1;
  surfaceId: string;
  text: string;
  workerEnvironmentId: string;
};
