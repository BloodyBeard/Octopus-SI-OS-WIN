use super::*;
use octopus_descriptors::ArtifactId;
use octopus_interfaces::{GrantProjection, InterfaceArtifactFile, InvocationMetadata};
use octopus_ledger::ArtifactRef;
use serde_json::json;
use sha2::{Digest, Sha256};
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
        let envelope = invocation(&root, &kinds, &arguments);
        let directory = effects.join("ordered").join(envelope.artifact.content.to_hex());
        let batch: Batch = serde_json::from_value(arguments.clone()).unwrap();
        let mut budget = DecodeBudget::new(1 << 20).unwrap();
        let entries = batch.writes.iter().map(|entry| entry.prepare(&mut budget).unwrap()).collect::<Vec<_>>();
        std::fs::create_dir_all(&directory).unwrap();
        install_effect_record(&directory.join("intent.json"), &serde_json::to_vec(&json!({
            "schema_version":2,"artifact":envelope.artifact,
            "batch_content_hash":ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&arguments)).to_hex(),
            "writes":entries.iter().map(|entry| json!({"path":entry.path,"before_hash":entry.expected_before_hash,
                "after_hash":ContentHash::of_bytes(entry.content.as_bytes()).to_hex(),"bytes":entry.content.len()})).collect::<Vec<_>>()
        })).unwrap()).unwrap();
        let prepared = Receipt { path:target.clone(), before_hash:Some(ContentHash::of_bytes(b"before").to_hex()),
            after_hash:ContentHash::of_bytes(b"after").to_hex(), bytes:5, outcome:Outcome::NotAttempted,
            observed_hash:None, error:None, publication:None };
        install_effect_record(&directory.join("0.bytes"), b"after").unwrap();
        install_effect_record(&directory.join("0.prepared.json"), &serde_json::to_vec(&prepared).unwrap()).unwrap();
        (envelope, arguments, directory, target, peer)
    };
    let apply = |input: &InterfaceInvocationEnvelope, args: &serde_json::Value| execute(input, args, &kinds, &roots, &effects, 1 << 20);
    for (name, installed, foreign) in [
        ("prepared", false, false),
        ("installed", true, false),
        ("uncertain", true, true),
    ] {
        let (input, args, directory, target, peer) = fixture(name);
        if installed {
            let link = directory.join("0.install");
            std::fs::hard_link(directory.join("0.bytes"), &link).unwrap();
            std::fs::rename(link, &target).unwrap();
            sync_parent(&target).unwrap();
        }
        assert!(!directory.join("0.receipt.json").exists());
        assert!(!directory.join("1.prepared.json").exists());
        let stage = directory.join("0.bytes");
        let inode = std::fs::metadata(&stage).unwrap().ino();
        let intent = std::fs::read(directory.join("intent.json")).unwrap();
        let prepared = std::fs::read(directory.join("0.prepared.json")).unwrap();
        assert_eq!(std::fs::read(&stage).unwrap(), b"after");
        assert_eq!(std::fs::read(&target).unwrap(), if !installed { b"before".as_slice() } else { b"after" });
        if foreign {
            let replacement = root.join("replacement");
            std::fs::write(&replacement, "after").unwrap();
            std::fs::rename(replacement, &target).unwrap();
            let error = apply(&input, &args).unwrap_err();
            let body = &error.details.as_ref().unwrap()["file_write"];
            assert_eq!(body["writes"][0]["outcome"], "uncertain");
            assert_eq!(body["writes"][1]["outcome"], "not_attempted");
            assert!(stage.is_file());
            assert_eq!(std::fs::read(&target).unwrap(), b"after");
            assert!(WriteGuard::acquire(&target, false).unwrap().publish("foreign", Some(&ContentHash::of_bytes(b"after").to_hex()), b"superseded", DigestAlgorithm::Blake3).is_err());
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
    let expected: Receipt = serde_json::from_slice(&std::fs::read(directory.join("0.prepared.json")).unwrap()).unwrap();
    let batch: Batch = serde_json::from_value(args.clone()).unwrap();
    let entry = batch.writes[0].prepare(&mut DecodeBudget::new(1 << 20).unwrap()).unwrap();
    commit_entry(&entry, expected, &directory, 0, &input, 2, &configured_write_roots(std::slice::from_ref(&root)).unwrap()).unwrap();
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

fn invocation(root: &Path, kinds: &[String], args: &serde_json::Value) -> InterfaceInvocationEnvelope {
    let payload = serde_json::to_vec(args).unwrap();
    let content = ContentHash::of_bytes(&payload);
    InterfaceInvocationEnvelope {
        artifact: ArtifactRef { id:ArtifactId::new(format!("tool.call:{}", content.to_hex())), content },
        artifact_file:InterfaceArtifactFile::write(root.join("calls"), "tool.call", &payload).unwrap(),
        artifact_binding_payloads:vec![],
        metadata:Some(InvocationMetadata::new(GrantProjection { worker_environment_kinds:kinds.to_vec(), ..Default::default() })),
    }
}

#[test]
fn shared_publication_retains_native_order_and_original_effect_after_lost_result() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let kinds = vec!["system_room".into()];
    let roots = BTreeMap::from([("run_records_root".into(), root.clone())]);
    let effects = root.join("effects");
    let writer = root.join("writer.json");
    let target = root.join("target.json");
    std::fs::write(&writer, "writer before").unwrap();
    std::fs::write(&target, "target before").unwrap();
    let hash = |bytes: &[u8]| ContentHash::of_bytes(bytes).to_hex();
    let args = json!({"ordered":true,"writes":[
        {"path":writer,"content":"writer advanced","expected_before_content":"writer before","expected_before_hash":hash(b"writer before")},
        {"path":target,"content":"target after","expected_before_content":"target before","expected_before_hash":hash(b"target before")}
    ]});
    let input = invocation(&root, &kinds, &args);
    let directory = effects.join("ordered").join(input.artifact.content.to_hex());
    let apply = || execute(&input, &args, &kinds, &roots, &effects, 1 << 20);
    INTERRUPTION.with(|selected| selected.set(Some(Interruption::Published)));
    assert!(std::panic::catch_unwind(apply).is_err());
    assert!(!directory.join("0.receipt.json").exists());
    assert_eq!(std::fs::read(&writer).unwrap(), b"writer advanced");
    assert_eq!(std::fs::read(&target).unwrap(), b"target before");
    let later = publication::publish(&writer, "client-later", Some(&format!("{:x}", Sha256::digest(b"writer advanced"))), b"later writer").unwrap();
    assert_eq!(later.outcome, PublicationOutcome::Applied);
    assert_eq!(later.digest_algorithm, DigestAlgorithm::Sha256);
    let body = apply().unwrap();
    let first: Receipt = serde_json::from_slice(&std::fs::read(directory.join("0.receipt.json")).unwrap()).unwrap();
    let second: Receipt = serde_json::from_slice(&std::fs::read(directory.join("1.receipt.json")).unwrap()).unwrap();
    assert_eq!(first.publication.as_ref().unwrap().operation_id, publication_operation_id(&input, true, 0).unwrap());
    assert_eq!(first.publication.as_ref().unwrap().digest_algorithm, DigestAlgorithm::Blake3);
    assert_eq!(second.publication.as_ref().unwrap().operation_id, publication_operation_id(&input, true, 1).unwrap());
    assert!(body["writes"][0].get("publication").is_none());
    assert_eq!(std::fs::read(&writer).unwrap(), b"later writer");
    assert_eq!(std::fs::read(&target).unwrap(), b"target after");
    assert_eq!(apply().unwrap(), body);
    assert!(publication::ReadGuard::try_acquire(&[writer, target], &[]).is_ok());
}
