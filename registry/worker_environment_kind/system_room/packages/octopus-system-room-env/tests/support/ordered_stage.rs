use super::*;
use octopus_descriptors::ArtifactId;
use octopus_interfaces::{GrantProjection, InterfaceArtifactFile, InvocationMetadata};
use octopus_ledger::ArtifactRef;
use serde_json::json;
use std::os::unix::fs::MetadataExt;

#[test]
fn applied_stages_retire_only_after_durable_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let kinds = vec!["system_room".into()];
    let roots = BTreeMap::from([("run_records_root".into(), root.clone())]);
    let effects = root.join("effects");
    let fixture = |name: &str| {
        let target = root.join(format!("{name}.json"));
        let peer = root.join(format!("{name}-peer.json"));
        std::fs::write(&target, "before").unwrap();
        std::fs::write(&peer, "before peer").unwrap();
        let arguments = json!({"ordered":true,"writes":[
            {"path":target,"content":"after","expected_before_content":"before","expected_before_hash":ContentHash::of_bytes(b"before").to_hex()},
            {"path":peer,"content":"after peer","expected_before_content":"before peer","expected_before_hash":ContentHash::of_bytes(b"before peer").to_hex()}
        ]});
        let payload = serde_json::to_vec(&arguments).unwrap();
        let content = ContentHash::of_bytes(&payload);
        let envelope = InterfaceInvocationEnvelope {
            artifact: ArtifactRef { id: ArtifactId::new(format!("tool.call:{}", content.to_hex())), content },
            artifact_file: InterfaceArtifactFile::write(root.join("calls"), "tool.call", &payload).unwrap(),
            artifact_binding_payloads: vec![],
            metadata: Some(InvocationMetadata::new(GrantProjection { worker_environment_kinds:kinds.clone(), ..Default::default() })),
        };
        let directory = effects.join("ordered").join(content.to_hex());
        (envelope, arguments, directory, target, peer)
    };
    let apply = |input: &InterfaceInvocationEnvelope, args: &serde_json::Value| execute(input, args, &kinds, &roots, &effects, 1 << 20);
    for (name, boundary, foreign) in [
        ("prepared", Interruption::Prepared, false),
        ("installed", Interruption::DurableTarget, false),
        ("uncertain", Interruption::DurableTarget, true),
    ] {
        let (input, args, directory, target, peer) = fixture(name);
        INTERRUPTION.with(|selected| selected.set(Some(boundary)));
        assert!(std::panic::catch_unwind(|| apply(&input, &args)).is_err());
        assert!(!directory.join("0.receipt.json").exists());
        assert!(!directory.join("1.prepared.json").exists());
        let stage = directory.join("0.bytes");
        let inode = std::fs::metadata(&stage).unwrap().ino();
        let intent = std::fs::read(directory.join("intent.json")).unwrap();
        let prepared = std::fs::read(directory.join("0.prepared.json")).unwrap();
        assert_eq!(std::fs::read(&stage).unwrap(), b"after");
        assert_eq!(std::fs::read(&target).unwrap(), if boundary == Interruption::Prepared { b"before".as_slice() } else { b"after" });
        if foreign {
            let replacement = root.join("replacement");
            std::fs::write(&replacement, "independent").unwrap();
            std::fs::rename(replacement, &target).unwrap();
            let error = apply(&input, &args).unwrap_err();
            let body = &error.details.as_ref().unwrap()["file_write"];
            assert_eq!(body["writes"][0]["outcome"], "uncertain");
            assert_eq!(body["writes"][1]["outcome"], "not_attempted");
            assert!(stage.is_file());
            assert_eq!(std::fs::read(&target).unwrap(), b"independent");
            assert_eq!(std::fs::read(&peer).unwrap(), b"before peer");
        } else {
            let receipt = apply(&input, &args).unwrap();
            assert!(!stage.exists() && !directory.join("1.bytes").exists());
            assert_eq!(std::fs::metadata(&target).unwrap().ino(), inode);
            assert_eq!(std::fs::read(&target).unwrap(), b"after");
            assert_eq!(std::fs::read(&peer).unwrap(), b"after peer");
            assert_eq!(apply(&input, &args).unwrap(), receipt);
        }
        assert_eq!(std::fs::read(directory.join("intent.json")).unwrap(), intent);
        assert_eq!(std::fs::read(directory.join("0.prepared.json")).unwrap(), prepared);
    }

    let (input, args, directory, target, peer) = fixture("retirement-error");
    INTERRUPTION.with(|selected| selected.set(Some(Interruption::Prepared)));
    assert!(std::panic::catch_unwind(|| apply(&input, &args)).is_err());
    let expected: Receipt = serde_json::from_slice(&std::fs::read(directory.join("0.prepared.json")).unwrap()).unwrap();
    let batch: Batch = serde_json::from_value(args.clone()).unwrap();
    let entry = batch.writes[0].prepare(&mut DecodeBudget::new(1 << 20).unwrap()).unwrap();
    commit_entry(&entry, expected, &directory, 0).unwrap();
    let receipt_path = directory.join("0.receipt.json");
    let saved = std::fs::read(&receipt_path).unwrap();
    let stage = directory.join("0.bytes");
    std::fs::remove_file(&stage).unwrap();
    std::fs::create_dir(&stage).unwrap();
    let error = apply(&input, &args).unwrap_err();
    assert_eq!(error.code, "file_write_evidence_failed");
    assert!(error.message.contains("stage retirement failed"));
    let body = error.details.unwrap()["file_write"].clone();
    assert!(body["writes"].as_array().unwrap().iter().all(|receipt| receipt["outcome"] == "applied"));
    assert_eq!(std::fs::read(&peer).unwrap(), b"after peer", "retirement must follow the complete ordered batch");
    assert_eq!(std::fs::read(&receipt_path).unwrap(), saved);
    std::fs::write(&target, "later target").unwrap();
    std::fs::write(&peer, "later peer").unwrap();
    assert_eq!(apply(&input, &args).unwrap_err().details.unwrap()["file_write"], body);
    std::fs::remove_dir(&stage).unwrap();
    std::os::unix::fs::symlink(&target, &stage).unwrap();
    assert_eq!(apply(&input, &args).unwrap_err().details.unwrap()["file_write"], body);
    assert!(stage.symlink_metadata().unwrap().file_type().is_symlink());
    std::fs::remove_file(&stage).unwrap();
    std::fs::hard_link(&target, &stage).unwrap();
    let inode = std::fs::metadata(&target).unwrap().ino();
    assert_eq!(apply(&input, &args).unwrap(), body);
    assert!(!stage.exists() && !directory.join("1.bytes").exists());
    assert_eq!(std::fs::metadata(&target).unwrap().ino(), inode);
    assert_eq!(std::fs::metadata(&target).unwrap().nlink(), 1);
    assert_eq!(std::fs::read(&target).unwrap(), b"later target");
    assert_eq!(std::fs::read(&peer).unwrap(), b"later peer");
    assert_eq!(std::fs::read(&receipt_path).unwrap(), saved);
    assert_eq!(apply(&input, &args).unwrap(), body);
}
