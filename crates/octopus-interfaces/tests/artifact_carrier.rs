use octopus_interfaces::{
    read_artifact_carrier_file_bounded, InterfaceArtifactFile, InterfaceArtifactFileError,
    InterfaceArtifactManifest,
};

fn temp_root(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "octopus-interface-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn captured_artifact_snapshot_never_rereads_mutated_paths() {
    let root = temp_root("captured-snapshot");
    let artifact = InterfaceArtifactFile::write(&root, "captured", b"original").unwrap();
    let original = artifact.read_bounded(4096, 4096).unwrap();
    assert!(matches!(
        InterfaceArtifactFile::store_manifest_bounded(
            root.join("rejected"),
            &original.manifest_bytes,
            4096,
            4,
        )
        .unwrap_err(),
        InterfaceArtifactFileError::PayloadTooLarge {
            size: 8,
            maximum: 4,
            ..
        }
    ));
    let captured = InterfaceArtifactFile::store_manifest_bounded(
        root.join("stored"),
        &original.manifest_bytes,
        4096,
        4096,
    )
    .unwrap();

    std::fs::write(artifact.manifest_path(), b"not-json").unwrap();
    std::fs::write(&original.manifest.payload_path, b"replacement").unwrap();
    std::fs::write(captured.manifest_path(), b"also-not-json").unwrap();

    let reread = captured.read_bounded(4096, 4096).unwrap();
    assert_eq!(reread.manifest_bytes, original.manifest_bytes);
    assert_eq!(reread.payload_bytes, b"original");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn manifest_ceiling_refuses_before_opening_its_payload() {
    let root = temp_root("manifest-ceiling");
    std::fs::create_dir_all(&root).unwrap();
    let manifest_path = root.join("oversized.artifact.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&InterfaceArtifactManifest {
            label: "x".repeat(256),
            payload_path: root.join("missing.payload"),
            payload_content_hash: None,
        })
        .unwrap(),
    )
    .unwrap();
    let artifact = InterfaceArtifactFile::new(manifest_path).unwrap();
    assert!(matches!(
        artifact.read_bounded(64, 1).unwrap_err(),
        InterfaceArtifactFileError::ManifestTooLarge { maximum: 64, .. }
    ));
    std::fs::remove_dir_all(root).unwrap();
}

/// Creates a file symlink. Windows allows that only with Developer Mode or
/// the symlink privilege; without it there is nothing to refuse, so the test
/// is skipped.
fn symlink(target: &std::path::Path, link: &std::path::Path) -> bool {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_file(target, link);
    match result {
        Ok(()) => true,
        #[cfg(windows)]
        Err(error) if error.raw_os_error() == Some(1314) => {
            eprintln!("skipping: this Windows account may not create symlinks");
            false
        }
        Err(error) => panic!("symlink cannot be created: {error}"),
    }
}

#[test]
fn bounded_carrier_reads_refuse_symlink_leaves() {
    let root = temp_root("symlink-carrier");
    std::fs::create_dir_all(&root).unwrap();
    let real = InterfaceArtifactFile::write(&root, "real", b"bytes").unwrap();
    let linked_manifest = root.join("linked.artifact.json");
    if !symlink(real.manifest_path(), &linked_manifest) {
        return;
    }
    let linked = InterfaceArtifactFile::new(linked_manifest).unwrap();
    assert!(linked.read_bounded(4096, 4096).is_err());

    let original = real.read_bounded(4096, 4096).unwrap();
    let linked_payload = root.join("linked.payload");
    assert!(symlink(&original.manifest.payload_path, &linked_payload));
    assert!(read_artifact_carrier_file_bounded(&linked_payload, 4096).is_err());
    let manifest_path = root.join("payload-link.artifact.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&InterfaceArtifactManifest {
            label: "linked-payload".to_string(),
            payload_path: linked_payload,
            payload_content_hash: Some(blake3::hash(b"bytes").to_hex().to_string()),
        })
        .unwrap(),
    )
    .unwrap();
    let linked = InterfaceArtifactFile::new(manifest_path).unwrap();
    assert!(linked.read_bounded(4096, 4096).is_err());

    let original = real.read_bounded(4096, 4096).unwrap();
    let store_root = root.join("stored");
    std::fs::create_dir_all(&store_root).unwrap();
    let victim = root.join("victim");
    std::fs::write(&victim, b"unchanged").unwrap();
    let stored_manifest = store_root.join(format!(
        "real-{}.artifact.json",
        blake3::hash(&original.manifest_bytes).to_hex()
    ));
    assert!(symlink(&victim, &stored_manifest));
    let captured = InterfaceArtifactFile::store_manifest_bounded(
        &store_root,
        &original.manifest_bytes,
        4096,
        4096,
    )
    .unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");
    assert!(std::fs::symlink_metadata(captured.manifest_path())
        .unwrap()
        .file_type()
        .is_file());
    std::fs::remove_dir_all(root).unwrap();
}
