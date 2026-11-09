/* Generated from schemas/mark.schema.json — do not edit.
 * Source of truth: the Rust types (crates/octopus-schemas surfaces()).
 * Regenerate: cargo run -p octopus-schemas --bin octopus-emit-schemas && npm run gen:types */

export type MarkClass = "success";
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
export interface ArtifactRef {
  content: DescriptorHash;
  id: string;
}
export interface VerifierPackagePin {
  descriptor_content: DescriptorHash;
  script_content: DescriptorHash;
}
