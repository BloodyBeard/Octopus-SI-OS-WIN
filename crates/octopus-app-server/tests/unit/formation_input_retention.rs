use super::*;
use octopus_descriptors::CheckpointId;
use octopus_ledger::InMemoryBlobs;
use octopus_policy::ArtifactAccess;
use serde_json::json;

fn write(path: &Path, value: serde_json::Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn blob(blobs: &SharedBlobs, hash: &DescriptorHash) -> Vec<u8> {
    blobs
        .lock()
        .unwrap()
        .get(&ContentHash::from_hex(&hash.to_hex()).unwrap())
        .unwrap()
}

fn resolve(
    snapshot: LoadedRegistrySnapshot,
    blobs: SharedBlobs,
) -> Result<RunGrantResolution, String> {
    RegistryRunGrantResolver {
        live_registry: Arc::new(RwLock::new(snapshot)),
        blobs,
    }
    .resolve_run_grant(RunGrantQuery {
        run: &RunId::new("run"),
        checkpoint: &CheckpointId::new("checkpoint"),
        ee: &EeId::new("ee_root"),
        worker: &InterfaceId::new("system_worker_test"),
        epoch: GrantEpoch(0),
        authored_interfaces: &[InterfaceId::new("system_worker_test")],
        artifact_bindings: &[ArtifactId::new("role")],
        artifact_access: &BTreeMap::from([(ArtifactId::new("role"), ArtifactAccess::read_only())]),
        required_verified_artifacts: &[],
    })
}

#[test]
fn normalized_formation_inputs_rebuild_the_historical_grant_after_source_changes() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry");
    let instances = dir.path().join("instances");
    for kind in "interface artifact tool worker_environment_kind verifier mark ee_template"
        .split_whitespace()
    {
        std::fs::create_dir_all(registry.join(kind)).unwrap();
    }
    let worker = registry.join("interface/system_worker_test");
    write(
        &worker.join("interface.json"),
        json!({"id":"system_worker_test", "interface_type":"system",
        "role":"worker", "direction":"bidirectional", "source_names":["test"]}),
    );
    write(
        &worker.join("policy.json"),
        json!({"id":"system_worker_test",
        "ceiling":{"interfaces":["system_worker_test"]}, "role_artifacts":["role"]}),
    );
    write(
        &registry.join("artifact/role/artifact.json"),
        json!({"id":"role", "content":"original role"}),
    );
    write(
        &registry.join("ee_template/root/ee_template.json"),
        json!({"id":"root", "goal_class":"original goal",
        "bootstrap_root":true, "allowed_interfaces":["system_worker_test"]}),
    );
    write(
        &instances.join("ee_root.json"),
        json!({"ee_id":"ee_root", "template_name":"root",
        "parent_ee_id":null, "goal":"original goal"}),
    );
    let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));
    let first = resolve(
        LoadedRegistrySnapshot::load(&registry, &instances).unwrap(),
        blobs.clone(),
    )
    .unwrap();
    let policy_name = crate::worker_policy::worker_interface_descriptor_name(&InterfaceId::new(
        "system_worker_test",
    ));
    assert_ne!(
        blob(&blobs, &first.grant().pin.inputs[&policy_name]),
        std::fs::read(worker.join("policy.json")).unwrap()
    );
    write(
        &worker.join("policy.json"),
        json!({"id":"system_worker_test", "ceiling":{
        "interfaces":["system_worker_test"], "artifact_verbs":["read","change"]}, "role_artifacts":["role"]}),
    );
    write(
        &registry.join("artifact/role/artifact.json"),
        json!({"id":"role", "content":"changed role"}),
    );
    write(
        &instances.join("ee_root.json"),
        json!({"ee_id":"ee_root", "template_name":"root",
        "parent_ee_id":null, "goal":"original goal", "config":{"changed":true}}),
    );
    write(
        &registry.join("ee_template/root/ee_template.json"),
        json!({"id":"root", "goal_class":"changed goal",
        "bootstrap_root":true, "allowed_interfaces":["system_worker_test"]}),
    );
    let current = resolve(
        LoadedRegistrySnapshot::load(&registry, &instances).unwrap(),
        blobs.clone(),
    )
    .unwrap();
    assert_ne!(current.grant().pin, first.grant().pin);
    let roots = BTreeMap::from([
        ("registry_interface_root", registry.join("interface")),
        ("registry_artifact_root", registry.join("artifact")),
        ("registry_ee_template_root", registry.join("ee_template")),
        ("ee_instances_root", instances.clone()),
    ]);
    for (name, hash) in &first.grant().pin.inputs {
        let bytes = blob(&blobs, hash);
        assert_eq!(DescriptorHash::of_bytes(&bytes), *hash);
        let (slot, relative) = name.as_str().split_once('/').unwrap();
        let path = roots[slot].join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let mut historical = LoadedRegistrySnapshot::load(&registry, &instances).unwrap();
    let replayed = resolve(historical.clone(), blobs.clone()).unwrap();
    assert_eq!(replayed.grant(), first.grant());
    assert_eq!(replayed.metadata_projection(), first.metadata_projection());
    historical
        .formation_source_bytes
        .get_mut(&policy_name)
        .unwrap()
        .push(b' ');
    assert!(
        resolve(historical, blobs)
            .unwrap_err()
            .contains("differs from its pinned hash")
    );
}

#[test]
fn registry_observation_requires_stability_and_propagates_read_failures() {
    for (reads, expected, loads) in [
        (vec![1, 2, 2, 2], "ok", 2),
        (vec![1, 1], "malformed", 1),
        ((1..=8).collect(), "after 4 attempts", 4),
        (vec![0], "before load failed", 0),
        (vec![1, 0], "after load failed", 1),
        (vec![u128::MAX, 1, 2, 2, 2], "ok", 2),
        (vec![1, u128::MAX, 2, 2], "ok", 2),
        (vec![u128::MAX, 1, 1], "malformed", 1),
        (vec![u128::MAX; 4], "after 4 attempts", 0),
    ] {
        let mut reads = reads.into_iter().map(|digest| match digest {
            0 => Err(std::io::Error::other("unreadable")),
            u128::MAX => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "enumerated descriptor was removed")),
            digest => Ok(RegistryDescriptorFingerprint {
                file_count: 1,
                digest,
            }),
        });
        let mut count = 0;
        let result = observe_stable_registry(
            None,
            || reads.next().unwrap(),
            || {
                count += 1;
                (count != 1).then_some(count).ok_or("malformed".into())
            },
        );
        assert_eq!(count, loads);
        assert!(reads.next().is_none());
        assert!(match result {
            Ok(Some((f, candidate))) => expected == "ok" && (f.digest, candidate) == (2, 2),
            Err(error) => {
                assert_eq!(matches!(error, RegistrySnapshotError::Unstable { .. }), expected == "after 4 attempts");
                if let RegistrySnapshotError::Unstable { attempts } = &error {
                    let verdict = crate::InterfaceArtifactArrivalAdmissionError::RegistryUnstable { attempts: *attempts }.verdict();
                    assert!(matches!(verdict, octopus_ledger::DeliveryVerdict::Refused {
                        reason: octopus_ledger::RefusedReason::RegistryUnstable, ..
                    }));
                    assert_eq!(serde_json::to_value(verdict).unwrap()["refused"]["reason"], "registry_unstable");
                }
                error.to_string().contains(expected)
            },
            Ok(None) => false,
        });
    }
}
