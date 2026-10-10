export type ClientExtensionManifestSemanticIssue = {
  code: string;
  path: string;
  message: string;
};

export function clientExtensionManifestSemanticIssues(
  manifest: unknown,
  options?: { compiled?: boolean },
): ClientExtensionManifestSemanticIssue[];
