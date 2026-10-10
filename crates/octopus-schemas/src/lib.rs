use schemars::{schema_for, JsonSchema};

pub fn surfaces() -> Vec<(&'static str, schemars::schema::RootSchema)> {
    fn entry<T: JsonSchema>(name: &'static str) -> (&'static str, schemars::schema::RootSchema) {
        (name, schema_for!(T))
    }
    vec![
        entry::<octopus_ledger::Movement>("movement"),
        entry::<octopus_interfaces::DeliveryVerdict>("delivery_verdict"),
        entry::<octopus_runtime::run_record::RunRecord>("run_record"),
        entry::<octopus_interfaces::GrantProjection>("grant_projection"),
        entry::<octopus_marks::RequiredVerifiedArtifact>("required_verified_artifact"),
        entry::<octopus_marks::Mark>("mark"),
        entry::<octopus_marks::MarkDescriptor>("mark_descriptor"),
        entry::<octopus_verifiers::VerifierDescriptor>("verifier_descriptor"),
    ]
}

pub fn schema_text(schema: &schemars::schema::RootSchema) -> String {
    let mut text = serde_json::to_string_pretty(schema).expect("schema serializes");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn schemas_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas")
    }

    #[test]
    fn committed_schemas_match_live_types() {
        let dir = schemas_dir();
        for (name, schema) in surfaces() {
            let path = dir.join(format!("{name}.schema.json"));
            let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!(
                    "schema '{}' is not committed at {} ({e}); run `cargo run -p octopus-schemas --bin octopus-emit-schemas`",
                    name,
                    path.display()
                )
            });
            assert_eq!(
                committed,
                schema_text(&schema),
                "schema '{name}' drifted from the Rust type; regenerate with `cargo run -p octopus-schemas --bin octopus-emit-schemas`"
            );
        }
    }
}
