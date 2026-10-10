use super::*;
use octopus_interface_transport::InterfaceHostIoLimits;

#[test]
fn opencodex_auth_does_not_require_or_import_the_machine_chatgpt_store() {
    let root = tempfile::tempdir().unwrap();
    let descriptor: InterfaceDescriptor = serde_json::from_str(include_str!(
        "../../../../../world_provider_openrouter/interface.json"
    ))
    .unwrap();
    let mode =
        validate_provider_descriptor_contract(&descriptor, ProviderLifeExpectation::OpenCodex)
            .unwrap();
    let (arrivals, _) = tokio::sync::mpsc::channel(1);
    let ctx = InterfaceLiftContext::unprivileged(
        descriptor.id.clone(),
        root.path()
            .join("registry/interface/world_provider_openrouter"),
        Some(descriptor.clone()),
        root.path().join("interfaces"),
        root.path().join("movements.jsonl"),
        root.path().join("blobs"),
        root.path().join("ledger-index.sqlite"),
        BTreeMap::new(),
        arrivals,
        Default::default(),
    )
    .with_host_io_limits(InterfaceHostIoLimits {
        max_control_frame_bytes: 1048576,
        max_stderr_bytes: 1048576,
        max_manifest_bytes: 1048576,
        max_payload_bytes: 1048576,
    })
    .unwrap();
    std::fs::create_dir_all(ctx.interface_var_dir()).unwrap();
    let auth = resolve_provider_auth_store(&ctx, &descriptor, &mode).unwrap();
    assert_eq!(
        auth,
        std::fs::canonicalize(ctx.interface_var_dir())
            .unwrap()
            .join("codex-home")
    );
    assert!(!auth.join("auth.json").exists());
    assert!(
        resolve_provider_auth_store(&ctx, &descriptor, &ProviderConnectionMode::Native)
            .unwrap_err()
            .contains("private_auth_store")
    );
}
