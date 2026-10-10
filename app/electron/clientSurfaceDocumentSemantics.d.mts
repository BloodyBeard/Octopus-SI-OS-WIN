export type ClientSurfaceDocumentSemanticIssue = {
  code: string;
  path: string;
  message: string;
};

export type ClientSurfaceDocumentSemanticInspection = {
  issues: ClientSurfaceDocumentSemanticIssue[];
  nodeCount: number;
  maxDepth: number;
};

export function clientSurfaceDocumentSemanticInspection(
  document: unknown,
  options?: { compiled?: boolean; readMethods?: readonly string[]; sourceBytes?: number },
): ClientSurfaceDocumentSemanticInspection;
