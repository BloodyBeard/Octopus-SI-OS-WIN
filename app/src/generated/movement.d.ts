/* Generated from schemas/movement.schema.json — do not edit.
 * Source of truth: the Rust types (crates/octopus-schemas surfaces()).
 * Regenerate: cargo run -p octopus-schemas --bin octopus-emit-schemas && npm run gen:types */

/**
 * @minItems 32
 * @maxItems 32
 */
export type DescriptorHash = [
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number
];
export type Endpoint = {
  Interface: string;
};
export type MovementMetadata = ArrivalFanoutMetadata | DeliveryMetadata | Mark | InvocationMetadata;
export type DeliveryVerdict =
  | {
      admitted: {
        seq: number;
      };
    }
  | {
      refused: {
        detail: unknown;
        reason: RefusedReason;
      };
    };
export type RefusedReason =
  | "no_binding"
  | "policy"
  | "verification_failed"
  | "unknown_interface"
  | "unknown_run"
  | "stale_epoch"
  | "checkpoint_spent"
  | "settled"
  | "registry_unstable"
  | "malformed";
export type MarkClass = "success";
export type BindingArtifactPolicy =
  | {
      mode: "all";
    }
  | {
      mode: "allow";
      names: string[];
      namespaces: string[];
    };
export type MovementKind =
  | ("ArtifactMoved" | "VerificationFailed" | "DeliveryRefused" | "InterfaceArrival" | "InterfaceOutbound")
  | {
      MarkOutcomeAdmitted: string;
    };
export type Lamport = number;
export type Seq = number;

export interface Movement {
  artifact?: ArtifactRef | null;
  from: Endpoint;
  invocation_metadata?: MovementMetadata | null;
  kind: MovementKind;
  lamport: Lamport;
  recorded_at_unix_ms: number;
  seq: Seq;
  to: Endpoint;
  trace: string;
}
export interface ArtifactRef {
  content: DescriptorHash;
  id: string;
}
export interface ArrivalFanoutMetadata {
  arrival_fanout: ArrivalFanoutScope;
}
export interface ArrivalFanoutScope {
  identity: string;
  targets: RuntimeInvocationMetadata[];
}
export interface RuntimeInvocationMetadata {
  checkpoint: string;
  ee: string;
  epoch: number;
  interface: string;
  run: string;
  run_record_hash?: string | null;
  worker: string;
}
export interface DeliveryMetadata {
  delivery_scope_seq?: number | null;
  delivery_verdict: DeliveryVerdict;
  runtime?: RuntimeInvocationMetadata | null;
  source_movement_seq?: number | null;
}
export interface Mark {
  arrival_seq?: number | null;
  class: MarkClass;
  details?: {
    [k: string]: unknown;
  };
  evidence?: ArtifactRef | null;
  name: string;
  run: string;
  run_epoch: number;
  subject: string;
  verifier: string;
  verifier_package?: VerifierPackagePin | null;
}
export interface VerifierPackagePin {
  descriptor_content: DescriptorHash;
  script_content: DescriptorHash;
}
export interface InvocationMetadata {
  artifact_binding_refs?: ArtifactRef[];
  checkpoint_admission?: CheckpointAdmissionMetadata | null;
  checkpoint_binding_policy?: CheckpointBindingPolicySnapshot | null;
  delivery_scope_seq?: number | null;
  grant?: GrantProjection;
  outbound_interface_bindings?: string[];
  replaces_arrival_seq?: number | null;
  required_verified_artifacts?: RequiredVerifiedArtifact[];
  runtime?: RuntimeInvocationMetadata | null;
  source_movement_seq?: number | null;
  verification_failure?: VerificationFailureMetadata | null;
}
export interface CheckpointAdmissionMetadata {
  artifact_label: string;
  complete: boolean;
  required_artifact_labels?: string[];
}
export interface CheckpointBindingPolicySnapshot {
  inbound: AuthoredInterfaceBinding;
  outbound?: AuthoredInterfaceBinding[];
}
export interface AuthoredInterfaceBinding {
  config: InterfaceBindingConfig;
  interface: string;
}
export interface InterfaceBindingConfig {
  artifact_policy: BindingArtifactPolicy;
}
export interface GrantProjection {
  artifact_access?: {
    [k: string]: ArtifactAccess;
  };
  artifact_verbs?: string[];
  attached_interface_bindings?: AttachedInterfaceBinding[];
  ee_root_slot_allowance?: string[] | null;
  formation_pin?: FormationPin;
  goal?: string | null;
  hard_blocked_tools?: string[];
  interface_affordance_names?: string[];
  interface_affordance_specs?: InterfaceToolAffordance[];
  interface_policy?: {
    [k: string]: {
      [k: string]: unknown;
    };
  };
  interfaces?: string[];
  mark_outcomes?: string[];
  role_artifacts?: string[];
  tools?: string[];
  worker_environment_kinds?: string[];
}
export interface ArtifactAccess {
  change?: boolean;
  create?: boolean;
  delete?: boolean;
  read?: boolean;
}
export interface AttachedInterfaceBinding {
  direction: string;
  interface: string;
  interface_type: string;
  role: string;
}
export interface FormationPin {
  inputs: {
    [k: string]: DescriptorHash;
  };
}
export interface InterfaceToolAffordance {
  args_schema: unknown;
  description: string;
  name: string;
}
export interface RequiredVerifiedArtifact {
  artifact_label: string;
  mark: string;
  params?: {
    [k: string]: unknown;
  };
  verifier: string;
}
export interface VerificationFailureMetadata {
  artifact_label?: string | null;
  checkpoint?: string | null;
  details?: unknown;
  error: string;
  mark?: string | null;
  run?: string | null;
  verifier?: string | null;
  verifier_package?: VerifierPackagePin | null;
}
