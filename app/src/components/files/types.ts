export type FilesOpenTarget =
  | {
      kind: "file";
      absolutePath?: string;
      path?: string;
      rootId?: string;
    }
  | {
      kind: "artifact";
      artifactId: string;
      createdAtMs?: number;
      interfaceId: string;
      label: string;
      runId: string;
    };
