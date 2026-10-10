/* Generated from schemas/required_verified_artifact.schema.json — do not edit.
 * Source of truth: the Rust types (crates/octopus-schemas surfaces()).
 * Regenerate: cargo run -p octopus-schemas --bin octopus-emit-schemas && npm run gen:types */

export interface RequiredVerifiedArtifact {
  artifact_label: string;
  mark: string;
  params?: {
    [k: string]: unknown;
  };
  verifier: string;
}
