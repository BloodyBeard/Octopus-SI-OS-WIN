import type { GlyphName } from "../components/IconGlyphs";
import type { AdmittedClientSurfaceDocument, ClientSurfaceAdmissionVerdict, ClientSurfacePlacement } from "../client-surfaces/types";
import type { ClientExtensionRegistryState } from "../extensions/types";
import type { ProjectionReadMethod } from "../generated/projectionReadContract";

export type SurfaceRendererKind =
  | "builtin.dashboard"
  | "builtin.registry"
  | "builtin.event_log"
  | "builtin.chat"
  | "builtin.voice"
  | "builtin.inbox"
  | "builtin.scheduled"
  | "builtin.applications"
  | "builtin.drivers"
  | "builtin.worker_environments"
  | "extension.builtin"
  | "custom.document"
  | "custom.projection"
  | "custom.static";

export type SurfaceTopbarMode = "dashboard" | "wide" | "none";
export type SurfaceWorkspaceMode = "dashboard" | "panel" | "immersive";

export type SurfaceChrome = {
  topbar: SurfaceTopbarMode;
  workspace: SurfaceWorkspaceMode;
  titlebarGlyph?: GlyphName;
  searchPlaceholder?: string;
  usesTopology?: boolean;
};

export type SurfaceProjectionColumn = {
  key: string;
  label: string;
  mono?: boolean;
};

export type SurfaceProjectionDescriptor = {
  method: ProjectionReadMethod;
  params?: Record<string, unknown>;
  path?: string;
  title?: string;
  columns?: SurfaceProjectionColumn[];
};

export type SurfaceStaticContent = {
  title?: string;
  body?: string;
};

export type ClientSurfaceDescriptor = {
  id: string;
  label: string;
  description?: string;
  glyph: GlyphName;
  rendererKind: SurfaceRendererKind;
  order: number;
  source: "builtin" | "client" | "extension";
  placements: ClientSurfacePlacement[];
  chrome: SurfaceChrome;
  hostClassName?: string;
  extension?: {
    extensionId: string;
    surfaceId: string;
  };
  document?: AdmittedClientSurfaceDocument;
  projection?: SurfaceProjectionDescriptor;
  staticContent?: SurfaceStaticContent;
};

export type SurfaceRegistryState = {
  loading: boolean;
  surfaces: ClientSurfaceDescriptor[];
  documents: AdmittedClientSurfaceDocument[];
  voiceSurfaces: AdmittedClientSurfaceDocument[];
  workerEnvironmentSurfaces: AdmittedClientSurfaceDocument[];
  documentRoot: string;
  documentVerdicts: ClientSurfaceAdmissionVerdict[];
  errors: string[];
  extensions: ClientExtensionRegistryState;
};
