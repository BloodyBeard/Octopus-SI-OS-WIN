/* Generated from schemas/run_record.schema.json — do not edit.
 * Source of truth: the Rust types (crates/octopus-schemas surfaces()).
 * Regenerate: cargo run -p octopus-schemas --bin octopus-emit-schemas && npm run gen:types */

export type GrantEpoch = number;
export type BindingArtifactPolicy =
  | {
      mode: "all";
    }
  | {
      mode: "allow";
      names: string[];
      namespaces: string[];
    };

export interface RunRecord {
  active_checkpoints: string[];
  checkpoints: CheckpointRecord[];
  ee: string;
  id: string;
  interface_state?: {
    [k: string]: {
      [k: string]: unknown;
    };
  };
  status: "running" | "idle";
  trace: string;
  worker: string;
}
export interface CheckpointRecord {
  artifact_bindings?: ArtifactBinding[];
  epoch: GrantEpoch;
  id: string;
  interface_binding?: AuthoredInterfaceBinding | null;
  outbound_interface_bindings?: AuthoredInterfaceBinding[];
  required_verified_artifacts?: RequiredVerifiedArtifact[];
  run: string;
  trace: string;
}
/**
 * POLICY.md §4 — an artifact binding is the run's authored declaration of its object-level rights on a specific artifact. The `id` names the artifact; the `access` states which of read/change/create/delete the run claims on it (default: read only — a bound artifact is readable context). Formation masks this down to the worker-interface + EE verb ceiling; the interface-binding (the default flow) is a separate axis on the checkpoint.
 */
export interface ArtifactBinding {
  access?: ArtifactAccess;
  id: string;
}
export interface ArtifactAccess {
  change?: boolean;
  create?: boolean;
  delete?: boolean;
  read?: boolean;
}
export interface AuthoredInterfaceBinding {
  config: InterfaceBindingConfig;
  interface: string;
}
export interface InterfaceBindingConfig {
  artifact_policy: BindingArtifactPolicy;
}
export interface RequiredVerifiedArtifact {
  artifact_label: string;
  mark: string;
  params?: {
    [k: string]: unknown;
  };
  verifier: string;
}
