/* Generated from schemas/grant_projection.schema.json — do not edit.
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
