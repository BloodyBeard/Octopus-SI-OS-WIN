use super::*;
use octopus_file_storage::publication::{DigestAlgorithm, PublicationOutcome, WriteGuard};

fn publish_record(path: &std::path::Path, operation: &str, bytes: &[u8]) {
    let before = octopus_ledger::ContentHash::of_bytes(&std::fs::read(path).unwrap()).to_hex();
    let receipt = WriteGuard::acquire(path, false).unwrap()
        .publish(operation, Some(&before), bytes, DigestAlgorithm::Blake3).unwrap();
    assert_eq!(receipt.outcome, PublicationOutcome::Applied);
}

#[tokio::test]
async fn changed_sources_after_verification_do_not_admit_or_spend_the_checkpoint() {
    for change in ["record", "authority", "withdraw"] {
        let runtime = runtime_with_interface_catalog();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        runtime.set_verified_artifact_admission(Arc::new(BlockingVerifiedArtifactAdmission {
            started: started.clone(), release: release.clone(),
        }));
        let root = temp_test_dir("publication-admission");
        runtime.set_run_records_dir(root.clone());
        let grant = grant_with_interfaces(&["world_test"], &["coder.build.ok"]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let path = literal_boundary_run_record_fixture(&root, &RunId::new("r1"), &InterfaceId::new("world_test"), &grant);
        set_required_verified_artifacts(&path, serde_json::json!([{
            "mark":"coder.build.ok", "artifact_label":"coder.build.result", "verifier":"build_success", "params":{}
        }]));
        let original = std::fs::read(&path).unwrap();
        let arrival = InterfaceArtifactAdmission {
            run: RunId::new("r1"), checkpoint: None, epoch: GrantEpoch(0), interface: InterfaceId::new("world_test"),
            artifact: test_artifact_ref("coder.build.result", b"result"), artifact_label:"coder.build.result".into(), run_record_hash:None,
        };
        let pending = {
            let runtime = runtime.clone();
            let arrival = arrival.clone();
            tokio::spawn(async move { runtime.admit_bound_interface_checkpoint(arrival).await })
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), started.notified()).await.unwrap();
        if change == "authority" {
            runtime.set_run_authority(RunId::new("r1"), grant_with_interfaces(&["world_test"], &[]), GrantEpoch(0));
        } else {
            let mut edited: serde_json::Value = serde_json::from_slice(&original).unwrap();
            if change == "withdraw" { edited["active_checkpoints"] = serde_json::json!([]); }
            else { edited["trace"] = serde_json::json!("changed while verification ran"); }
            publish_record(&path, "concurrent-edit", &serde_json::to_vec(&edited).unwrap());
        }
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), pending).await.unwrap().unwrap();
        assert!(result.is_err(), "{change} must invalidate the prepared admission");
        assert!(runtime.admitted_mark_outcomes().is_empty());
        runtime.set_run_authority(RunId::new("r1"), grant, GrantEpoch(0));
        if change != "authority" { publish_record(&path, "restore", &original); }
        runtime.set_verified_artifact_admission(Arc::new(TestVerifiedArtifactAdmission));
        runtime.admit_bound_interface_checkpoint(arrival).await.unwrap();
        assert_eq!(runtime.admitted_mark_outcomes().len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
