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

export const CLIENT_SURFACE_DOCUMENT_LIMITS: Readonly<{
  document_bytes: number;
  data_bytes: number;
  action_payload_bytes: number;
  native_nodes: number;
  native_depth: number;
  collection_items: number;
}>;

export function clientSurfaceDocumentSemanticInspection(
  document: unknown,
  options?: { compiled?: boolean; readMethods?: readonly string[]; sourceBytes?: number },
): ClientSurfaceDocumentSemanticInspection;

export function isSafeClientSurfaceVideoSource(value: unknown): boolean;
