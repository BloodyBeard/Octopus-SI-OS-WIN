import type { GlyphName } from "../components/IconGlyphs";
import type { ClientSurfaceReadMethod } from "./sourceContract";

export type ClientSurfacePlacement = "shell" | "home";
export type ClientSurfaceTone = "default" | "muted" | "positive" | "warning" | "danger" | "accent";
export type ClientSurfaceGap = "none" | "xs" | "sm" | "md" | "lg";

export type ClientSurfaceBinding = {
  path: string;
  fallback?: string | number | boolean | null;
  format?: "text" | "number" | "percent" | "date" | "time" | "json";
};

export type ClientSurfaceValue = string | number | boolean | null | ClientSurfaceBinding;
export type ClientSurfaceNumberValue = number | (ClientSurfaceBinding & { fallback?: number; format?: "number" | "percent" });
export type ClientSurfaceNumberListValue = number[] | { path: string };

export type ClientSurfaceCollectionBinding = {
  path: string;
  limit?: number;
};

export type ClientSurfaceTableColumn = {
  key: string;
  label: ClientSurfaceValue;
  value: ClientSurfaceValue;
  align?: "start" | "center" | "end";
  role?: "text" | "number" | "mono";
};

export type ClientSurfaceDonutSegment = {
  label: ClientSurfaceValue;
  value: ClientSurfaceNumberValue;
  tone?: ClientSurfaceTone;
};

export type ClientSurfaceAction = {
  event: string;
  payload?: Record<string, unknown>;
};

export type ClientSurfaceNode =
  | {
      type: "stack";
      gap?: ClientSurfaceGap;
      align?: "start" | "center" | "end" | "stretch";
      children: ClientSurfaceNode[];
    }
  | {
      type: "row";
      gap?: ClientSurfaceGap;
      align?: "start" | "center" | "end" | "stretch";
      justify?: "start" | "center" | "end" | "between";
      wrap?: boolean;
      children: ClientSurfaceNode[];
    }
  | {
      type: "section";
      title: ClientSurfaceValue;
      glyph?: GlyphName;
      children: ClientSurfaceNode[];
    }
  | {
      type: "text";
      value: ClientSurfaceValue;
      role?: "title" | "body" | "caption" | "mono";
      tone?: ClientSurfaceTone;
      truncate?: boolean;
    }
  | {
      type: "key_value";
      label: ClientSurfaceValue;
      value: ClientSurfaceValue;
      tone?: ClientSurfaceTone;
    }
  | {
      type: "status";
      label: ClientSurfaceValue;
      state?: ClientSurfaceValue;
      detail?: ClientSurfaceValue;
      glyph?: GlyphName;
      tone?: ClientSurfaceTone;
    }
  | {
      type: "metric";
      label: ClientSurfaceValue;
      value: ClientSurfaceValue;
      detail?: ClientSurfaceValue;
      tone?: ClientSurfaceTone;
    }
  | {
      type: "progress";
      label?: ClientSurfaceValue;
      value: ClientSurfaceNumberValue;
      detail?: ClientSurfaceValue;
      tone?: ClientSurfaceTone;
    }
  | {
      type: "donut";
      label?: ClientSurfaceValue;
      value: ClientSurfaceNumberValue;
      segments: ClientSurfaceDonutSegment[];
    }
  | {
      type: "sparkline";
      label?: ClientSurfaceValue;
      values: ClientSurfaceNumberListValue;
      tone?: ClientSurfaceTone;
    }
  | {
      type: "list";
      items: ClientSurfaceCollectionBinding;
      children: ClientSurfaceNode[];
      empty?: ClientSurfaceValue;
      divided?: boolean;
    }
  | {
      type: "table";
      rows: ClientSurfaceCollectionBinding;
      columns: ClientSurfaceTableColumn[];
      empty?: ClientSurfaceValue;
    }
  | {
      type: "image";
      source: ClientSurfaceValue;
      alt?: ClientSurfaceValue;
      fit?: "contain" | "cover";
      ratio?: "square" | "portrait" | "landscape" | "wide";
    }
  | {
      type: "input";
      label?: ClientSurfaceValue;
      value?: ClientSurfaceValue;
      placeholder?: ClientSurfaceValue;
      input_kind?: "text" | "search" | "url";
      action?: ClientSurfaceAction;
    }
  | {
      type: "button";
      label: ClientSurfaceValue;
      glyph?: GlyphName;
      variant?: "quiet" | "secondary" | "primary" | "danger" | "footer";
      action: ClientSurfaceAction;
    }
  | { type: "divider" }
  | { type: "spacer"; size?: "xs" | "sm" | "md" | "lg" };

export type ClientSurfaceProjectionSource = {
  id: string;
  kind: "projection";
  method: ClientSurfaceReadMethod;
  params?: Record<string, unknown>;
  path?: string;
};

export type VoiceWidgetLayout = {
  min_width: number;
  preferred_width: number;
  max_width: number;
  min_height: number;
  preferred_height: number;
  max_height: number;
  content_fit: "reflow" | "scale";
};

export type VoiceWidgetTarget = {
  kind: "voice.widget";
  slot: "left" | "right";
  order: number;
  layout: VoiceWidgetLayout;
};

export type VoiceWindowTarget = {
  kind: "voice.window";
  slot: "left" | "right";
  order: number;
  size?: "compact" | "standard" | "wide";
};

export type WorkspaceSurfaceTarget = {
  kind: "workspace.surface";
  placements: ClientSurfacePlacement[];
  order: number;
  chrome: {
    topbar: "wide" | "none";
    workspace: "panel" | "immersive";
    titlebar_glyph?: GlyphName;
    search_placeholder?: string;
  };
};

export type WorkerEnvironmentSurfaceTarget = {
  kind: "worker_environment.surface";
  worker_environment_id: string;
  surface_id: string;
  order: number;
};

export type ClientSurfaceRender =
  | {
      kind: "native";
      density?: "compact" | "standard" | "comfortable";
      root: ClientSurfaceNode;
    }
  | {
      kind: "sandbox";
      html: string;
      css?: string;
      javascript?: string;
      initial_height?: number;
      actions?: Record<string, ClientSurfaceAction>;
    };

export type ClientSurfaceDocument = {
  schema: "octopus.client_surface";
  schema_version: 1;
  release_channel: "lab" | "stable";
  id: string;
  revision: number;
  title: string;
  glyph: GlyphName;
  header?: {
    show_glyph?: boolean;
    tone?: ClientSurfaceTone;
  };
  target: VoiceWidgetTarget | VoiceWindowTarget | WorkspaceSurfaceTarget | WorkerEnvironmentSurfaceTarget;
  data?: Record<string, unknown>;
  sources?: ClientSurfaceProjectionSource[];
  render: ClientSurfaceRender;
};

export type ClientSurfaceAdmissionIssue = {
  code: string;
  path: string;
  message: string;
};

export type ClientSurfaceAdmissionVerdict = {
  passed: boolean;
  details: {
    verifier: "client_surface_document_v1";
    contract: "octopus.client_surface@1";
    document_id: string | null;
    revision: number | null;
    source_digest: string;
    node_count: number;
    max_depth: number;
    issues: ClientSurfaceAdmissionIssue[];
  };
};

export type AdmittedClientSurfaceDocument = {
  document: ClientSurfaceDocument;
  sourceDigest: string;
  sourcePath: string;
  sourceExtensionId?: string;
  hostSources?: ClientSurfaceHostSource[];
  sourceArtifactId?: string;
  sourceRunId?: string;
  sourceInterfaceId?: string;
  sourceSequence?: number;
  sourceDeliveredAt?: string;
  verdict: ClientSurfaceAdmissionVerdict & { passed: true };
};

export type ClientSurfaceHostSource =
  | {
      id: string;
      kind: "filesystem";
      path: string;
      read: "text" | "json" | "directory";
      refreshIntervalMs: number;
    }
  | {
      id: string;
      kind: "device";
      device: "camera" | "microphone";
      deviceId: string;
    };

export type ClientSurfaceAdmissionResult = {
  verdict: ClientSurfaceAdmissionVerdict;
  admitted?: AdmittedClientSurfaceDocument;
};

export type ClientSurfaceDocumentSource = {
  path: string;
  digest: string;
  bytes: number;
  modifiedAt: string;
  text: string;
  artifactId?: string;
  runId?: string;
  interfaceId?: string;
  sequence?: number;
};

export type ClientSurfaceDocumentSnapshot = {
  generation: number;
  root: string;
  documents: ClientSurfaceDocumentSource[];
  errors: string[];
};

export type ClientSurfaceDocumentChange = {
  generation: number;
};

export type ClientSurfaceActionEvent = {
  documentId: string;
  documentRevision?: number;
  source?: {
    digest: string;
    artifactId?: string;
    runId?: string;
    interfaceId?: string;
    sequence?: number;
  };
  action: ClientSurfaceAction;
  input?: {
    path: string;
    value: string;
  };
};
