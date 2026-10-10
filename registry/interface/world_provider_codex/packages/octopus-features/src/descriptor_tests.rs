use crate::descriptor::features_root;
use crate::descriptor::load_feature_descriptors_from_disk;
use crate::descriptor::FeaturesRootEnvGuard;
use crate::FeatureDescriptor;
use crate::FeatureId;
use crate::FeatureSet;
use crate::Features;
use crate::FEATURES;
use octopus_descriptors::loader;
use octopus_descriptors::parse_strict;
use octopus_descriptors::Descriptor;
use octopus_descriptors::DescriptorName;
use octopus_descriptors::DescriptorStore;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

fn default_features_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("features")
}

fn write_feature(root: &Path, key: &str, default_enabled: bool, stage_kind: &str) {
    let dir = root.join(key);
    std::fs::create_dir_all(&dir).unwrap();
    let body = serde_json::json!({
        "id": key,
        "stage": {"kind": stage_kind},
        "default_enabled": default_enabled,
    });
    std::fs::write(
        dir.join("feature.json"),
        serde_json::to_vec_pretty(&body).unwrap(),
    )
    .unwrap();
}

#[test]
fn with_defaults_reads_on_disk_feature_json_at_runtime() {
    let base = std::env::temp_dir().join(format!("octo_feat_live_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    write_feature(&base, "code_mode", true, "stable");

    let _guard = FeaturesRootEnvGuard::set(&base);
    let folded = Features::try_with_defaults().expect("load on-disk features fail-closed");
    assert!(
        folded.enabled(crate::Feature::CodeMode),
        "with_defaults must reflect the on-disk feature.json (default flipped to true), proving \
         the file is the runtime source of truth — not an embedded seed"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn checked_in_feature_files_load_fail_closed_and_are_identity_consistent() {
    let loaded = load_feature_descriptors_from_disk()
        .expect("checked-in feature.json files must load fail-closed via the spine loader");
    assert!(!loaded.is_empty(), "the feature registry must not be empty");
    for (dir, desc) in &loaded {
        assert_eq!(
            dir,
            desc.id.as_str(),
            "directory == identity: dir {dir:?} must equal descriptor id {:?}",
            desc.id.as_str()
        );
    }
    assert_eq!(features_root(), default_features_root());
}

#[test]
fn compiled_features_projection_matches_on_disk_descriptors() {
    let from_disk: BTreeMap<String, FeatureDescriptor> = load_feature_descriptors_from_disk()
        .unwrap()
        .into_iter()
        .collect();
    for spec in FEATURES {
        let desc = from_disk
            .get(spec.key)
            .unwrap_or_else(|| panic!("FEATURES row {} has no on-disk descriptor", spec.key));
        assert_eq!(desc.id, FeatureId::new(spec.key));
        let platform_overridden = matches!(
            spec.key,
            "secret_auth_storage" | "unified_exec" | "prevent_idle_sleep"
        );
        if !platform_overridden {
            assert_eq!(
                desc.default_enabled, spec.default_enabled,
                "default_enabled drift for {}",
                spec.key
            );
            assert_eq!(
                desc.stage.to_static_stage(),
                spec.stage,
                "stage drift for {}",
                spec.key
            );
        }
    }
}

#[test]
fn unknown_on_disk_key_fails_registry_build_closed() {
    let base = std::env::temp_dir().join(format!("octo_feat_orphan_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    write_feature(&base, "shell_tool", true, "stable");
    write_feature(&base, "totally_unknown_feature", true, "stable");

    let _guard = FeaturesRootEnvGuard::set(&base);
    let err = crate::try_feature_registry()
        .expect_err("an on-disk key with no Feature variant must fail the registry build closed");
    let msg = err.to_string();
    assert!(
        msg.contains("totally_unknown_feature") && msg.contains("no compiled Feature variant"),
        "expected a typed drift error naming the orphan key, got: {msg}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn missing_descriptor_for_compiled_variant_fails_registry_build_closed() {
    let base = std::env::temp_dir().join(format!("octo_feat_missing_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    write_feature(&base, "shell_tool", true, "stable");

    let _guard = FeaturesRootEnvGuard::set(&base);
    let err = crate::try_feature_registry()
        .expect_err("a compiled variant with no on-disk descriptor must fail the build closed");
    let msg = err.to_string();
    assert!(
        msg.contains("has no on-disk feature.json descriptor"),
        "expected a typed drift error for the unbacked variant, got: {msg}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn unknown_field_in_feature_descriptor_fails_closed() {
    let bad = br#"{"id":"shell_tool","stage":{"kind":"stable"},"default_enabled":true,"rogue":1}"#;
    assert!(
        parse_strict::<FeatureDescriptor>(bad).is_err(),
        "deny_unknown_fields must fail closed on an edited feature descriptor"
    );
}

#[test]
fn feature_set_from_store_folds_on_disk_defaults_deterministically() {
    let mut store = DescriptorStore::new();
    FeatureSet::load_into_store(&mut store).expect("load on-disk feature descriptors into store");
    let set = FeatureSet::from_store(&store).expect("fold features from store");

    let from_disk: BTreeMap<String, FeatureDescriptor> = load_feature_descriptors_from_disk()
        .unwrap()
        .into_iter()
        .collect();
    for (key, desc) in &from_disk {
        if matches!(
            key.as_str(),
            "secret_auth_storage" | "unified_exec" | "prevent_idle_sleep"
        ) {
            continue;
        }
        assert_eq!(
            set.enabled(&FeatureId::new(key.clone())),
            desc.default_enabled,
            "store fold disagreed with on-disk default for `{key}`"
        );
    }

    let mut store2 = DescriptorStore::new();
    FeatureSet::load_into_store(&mut store2).unwrap();
    let set2 = FeatureSet::from_store(&store2).unwrap();
    assert_eq!(set, set2, "fold is not deterministic");
    assert_eq!(set.head_hash(), set2.head_hash(), "head_hash not stable");
}

#[test]
fn feature_set_from_store_fails_closed_on_malformed_bound_descriptor() {
    let mut store = DescriptorStore::new();
    let bad = serde_json::json!({ "id": "x", "stage": {"kind":"stable"}, "default_enabled": true, "rogue": 1 });
    let hash = store.put(
        FeatureDescriptor::KIND,
        FeatureDescriptor::SCHEMA_VERSION,
        FeatureDescriptor::CONTRACT_VERSION,
        &bad,
    );
    store
        .set_head(DescriptorName::new("feature/x"), hash)
        .unwrap();
    assert!(
        FeatureSet::from_store(&store).is_err(),
        "a malformed bound feature descriptor must fail the fold closed"
    );
}

#[test]
fn crate_loader_matches_raw_spine_loader_over_checked_in_root() {
    let via_crate: BTreeMap<String, FeatureDescriptor> = load_feature_descriptors_from_disk()
        .unwrap()
        .into_iter()
        .collect();
    let via_spine: BTreeMap<String, FeatureDescriptor> =
        loader::load_kind::<FeatureDescriptor>(&default_features_root())
            .unwrap()
            .into_iter()
            .map(|(name, desc)| {
                (
                    name.as_str().strip_prefix("feature/").unwrap().to_string(),
                    desc,
                )
            })
            .collect();
    assert_eq!(
        via_crate, via_spine,
        "crate loader diverged from the spine loader"
    );
}

#[test]
fn live_with_defaults_agrees_with_store_fold_over_real_root_and_replays() {
    let live = Features::try_with_defaults().expect("live with_defaults loads from disk");

    let mut store = DescriptorStore::new();
    FeatureSet::load_into_store(&mut store).expect("load real on-disk descriptors into store");
    let folded = FeatureSet::from_store(&store).expect("fold");

    for spec in FEATURES {
        if matches!(
            spec.key,
            "secret_auth_storage" | "unified_exec" | "prevent_idle_sleep"
        ) {
            continue;
        }
        assert_eq!(
            live.enabled(spec.id),
            folded.enabled(&FeatureId::new(spec.key)),
            "live accessor and store fold disagree on `{}` — the store is not the authoritative \
             enabled-state source",
            spec.key
        );
    }

    let mut store2 = DescriptorStore::new();
    FeatureSet::load_into_store(&mut store2).unwrap();
    let folded2 = FeatureSet::from_store(&store2).unwrap();
    assert_eq!(
        folded.head_hash(),
        folded2.head_hash(),
        "restart-replay over the real on-disk root must reproduce the same content identity"
    );
}
