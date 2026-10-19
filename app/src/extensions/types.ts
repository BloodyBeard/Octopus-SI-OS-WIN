import type { GlyphName } from "../components/IconGlyphs";
import type { AdmittedClientSurfaceDocument, ClientSurfaceDocument, ClientSurfaceHostSource } from "../client-surfaces/types";

export type ClientExtensionCategory = "widget" | "tab" | "repair" | "input";

export type ClientExtensionFilesystemSource = {
  id: string;
  kind: "filesystem";
  label: string;
  description: string;
  required: boolean;
  selection: "file" | "directory";
  read: "text" | "json" | "directory";
  refresh_interval_ms?: number;
};

export type ClientExtensionDeviceSource = {
  id: string;
  kind: "device";
  label: string;
  description: string;
  required: boolean;
  device: "camera" | "microphone";
};

export type ClientExtensionCredentialSource = {
  id: string;
  kind: "credential";
  label: string;
  description: string;
  required: boolean;
  origin: string;
  check_path?: string;
};

export type ClientExtensionSource = ClientExtensionFilesystemSource | ClientExtensionDeviceSource | ClientExtensionCredentialSource;

export type ClientExtensionCredentialStatus = {
  origin: string;
  updatedAt: string;
};

export type ClientExtensionInputProvider = {
  credential_source: string;
  endpoint: string;
  model: string;
};

export type ClientExtensionIncidentTrigger = {
  incident_code: string;
  interface_id: string;
};

export type ClientExtensionContribution =
  | { kind: "builtin"; surface_id: string }
  | { kind: "builtin_action"; action_id: string; triggers: ClientExtensionIncidentTrigger[] }
  | { kind: "builtin_input"; input_id: string; provider: ClientExtensionInputProvider }
  | { kind: "surface"; document: ClientSurfaceDocument };

export type ClientExtensionManifest = {
  schema: "octopus.client_extension";
  schema_version: 1;
  id: string;
  version: string;
  name: string;
  description: string;
  developer: string;
  category: ClientExtensionCategory;
  glyph: GlyphName;
  default_installed: boolean;
  contribution: ClientExtensionContribution;
  sources?: ClientExtensionSource[];
};

export type ClientExtensionManifestSource = {
  path: string;
  digest: string;
  bytes: number;
  modifiedAt: string;
  text: string;
};

export type ClientExtensionSourceConfiguration = Record<string, string>;

export type ClientExtensionRevision = {
  digest: string;
  revision: string;
};

export type ClientExtensionStateRecord = {
  sources?: Record<string, ClientExtensionSourceConfiguration>;
  reviewed?: ClientExtensionRevision;
  dismissed?: ClientExtensionRevision;
};

export type ClientExtensionLifecycleReceiptState = "missing" | "valid" | "stale" | "invalid";

export type ClientExtensionLifecycleEvidence = {
  receiptState: ClientExtensionLifecycleReceiptState;
  desiredState: "installed" | "available" | "";
  operationId: string;
  requestDigest: string;
  sourceRevision: ClientExtensionRevision | null;
  completedAt: string;
  receiptDigest: string;
  error: string;
};

export type ClientExtensionLifecycleReceipt = {
  extensionId: string;
  operationId: string;
  requestDigest: string;
  desiredState: "installed" | "available";
  sourceRevision: ClientExtensionRevision;
  completedAt: string;
  receiptDigest: string;
};

export type ClientExtensionContractFiles = {
  root: string;
  files: { manifest: string; document: string; projection: string };
};

export type ClientExtensionStoreSnapshot = {
  generation: number;
  root: string;
  contract?: ClientExtensionContractFiles;
  manifests: ClientExtensionManifestSource[];
  state: Record<string, ClientExtensionStateRecord>;
  stateStatus: "valid" | "invalid" | "missing";
  lifecycle: Record<string, ClientExtensionLifecycleEvidence>;
  credentials: Record<string, Record<string, ClientExtensionCredentialStatus>>;
  actionStatuses: ClientExtensionActionStatus[];
  errors: string[];
};

export type ClientExtensionAdmissionIssue = {
  code: string;
  path: string;
  message: string;
};

export type ClientExtensionAdmissionVerdict = {
  passed: boolean;
  extensionId: string | null;
  sourceDigest: string;
  issues: ClientExtensionAdmissionIssue[];
};

export type AdmittedClientExtension = {
  manifest: ClientExtensionManifest;
  sourceDigest: string;
  sourcePath: string;
  verdict: ClientExtensionAdmissionVerdict & { passed: true };
  surface?: AdmittedClientSurfaceDocument;
};

export type ClientExtensionRecord = AdmittedClientExtension & {
  admission: "builtin" | "dismissed" | "pending" | "reviewed";
  installed: boolean;
  configured: boolean;
  lifecycle: ClientExtensionLifecycleEvidence;
  sourceConfiguration: Record<string, ClientExtensionSourceConfiguration>;
  credentials: Record<string, ClientExtensionCredentialStatus>;
};

export type ClientExtensionMutation = {
  id: string;
  expectedGeneration: number;
  sources?: Record<string, ClientExtensionSourceConfiguration>;
  reviewed?: ClientExtensionRevision;
  dismissed?: ClientExtensionRevision;
};

export type ClientExtensionCredentialUpdate = {
  id: string;
  sourceId: string;
  secret: string | null;
  expectedGeneration: number;
};

export type ClientExtensionCredentialCheck = {
  checked: boolean;
  ok: boolean;
  status: number;
};

export type ClientExtensionLifecycleRequest = {
  action: "install" | "remove";
  id: string;
  operationId: string;
  expectedGeneration: number;
  expectedRevision: ClientExtensionRevision;
};

export type ClientExtensionLifecycleOutcome = {
  changed: boolean;
  receipt: ClientExtensionLifecycleReceipt;
  snapshot: ClientExtensionStoreSnapshot;
};

export type ClientExtensionIncident = {
  id: string;
  code: string;
  interfaceId: string;
  message: string;
  observedAtMs: number;
};

export type ClientExtensionActionRegistration = {
  extensionId: string;
  actionId: string;
  triggers: ClientExtensionIncidentTrigger[];
};

export type ClientExtensionActionRequest = {
  extensionId: string;
  actionId: string;
  incident: Pick<ClientExtensionIncident, "id" | "code" | "interfaceId">;
};

export type ClientExtensionActionOutcome = {
  extensionId: string;
  actionId: string;
  incidentId: string;
  interfaceId: string;
  credentialUpdated: boolean;
  runtimeRestarted: boolean;
  sharedCredential: boolean;
};

export type ClientExtensionActionStatus = {
  contract: "octopus.client-extension.action-status.v1";
  extensionId: string;
  actionId: string;
  interfaceId: string;
  installed: boolean;
  actionAvailable: boolean;
  incidentCode: string;
  state: "configured" | "repair_available" | "sign_in_required" | "waiting_for_interface" | "unavailable";
  code: string;
  message: string;
};

export type ClientExtensionRegistryState = {
  generation: number;
  root: string;
  hydrated: boolean;
  loading: boolean;
  mutable: boolean;
  extensions: ClientExtensionRecord[];
  errors: string[];
  workspaceSurfaces: AdmittedClientSurfaceDocument[];
  voiceSurfaces: AdmittedClientSurfaceDocument[];
  installedBuiltinSurfaceIds: string[];
  installedBuiltinWidgetIds: string[];
  builtinWidgetSources: Record<string, ClientSurfaceHostSource[]>;
  installedActions: ClientExtensionActionRegistration[];
  actionStatuses: ClientExtensionActionStatus[];
  refresh: () => void;
  install: (id: string, operationId?: string) => Promise<void>;
  dismiss: (id: string, operationId?: string) => Promise<void>;
  uninstall: (id: string, operationId?: string) => Promise<void>;
  configure: (
    id: string,
    sources: Record<string, ClientExtensionSourceConfiguration>,
    install?: boolean,
    operationId?: string,
    credentials?: Record<string, string>,
  ) => Promise<void>;
  removeCredential: (id: string, sourceId: string) => Promise<void>;
  checkCredential: (id: string, sourceId: string) => Promise<ClientExtensionCredentialCheck>;
  revealRoot: () => Promise<void>;
};
