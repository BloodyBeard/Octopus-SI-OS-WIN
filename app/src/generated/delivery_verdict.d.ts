/* Generated from schemas/delivery_verdict.schema.json — do not edit.
 * Source of truth: the Rust types (crates/octopus-schemas surfaces()).
 * Regenerate: cargo run -p octopus-schemas --bin octopus-emit-schemas && npm run gen:types */

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
