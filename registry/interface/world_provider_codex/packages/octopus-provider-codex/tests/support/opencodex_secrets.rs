use super::*;

#[test]
fn opencodex_configuration_selects_the_bridge_at_the_toml_root() {
    let root = tempfile::tempdir().unwrap();
    let descriptor: InterfaceDescriptor = serde_json::from_str(include_str!(
        "../../../../../world_provider_openrouter/interface.json"
    ))
    .unwrap();
    let declared = descriptor_config(&descriptor).unwrap().unwrap();
    let home = build_curated_opencodex_home(
        root.path(),
        "http://127.0.0.1:12345/v1",
        declared.model_catalog.as_ref(),
    )
    .unwrap();
    let encoded = std::fs::read_to_string(home.join("config.toml")).unwrap();
    let config: toml::Value = toml::from_str(&encoded).unwrap();
    assert_eq!(config["model_provider"].as_str(), Some("octopus_opencodex"));
    assert!(
        !config["agents"]
            .as_table()
            .unwrap()
            .contains_key("model_provider")
    );
    let provider = &config["model_providers"]["octopus_opencodex"];
    assert_eq!(
        provider["base_url"].as_str(),
        Some("http://127.0.0.1:12345/v1")
    );
    assert_eq!(provider["requires_openai_auth"].as_bool(), Some(false));
    assert_eq!(config["features"]["shell_tool"].as_bool(), Some(false));
    let catalog: ModelsResponse = serde_json::from_slice(
        &std::fs::read(config["model_catalog_json"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(catalog, declared.model_catalog.unwrap());
    let sonnet = catalog
        .models
        .iter()
        .find(|model| model.slug == "anthropic/claude-sonnet-5")
        .unwrap();
    assert_eq!(sonnet.context_window, Some(1000000));
    assert!(!sonnet.used_fallback_model_metadata);
}

#[test]
fn opencodex_reads_the_interface_local_store_for_owner_and_task_interfaces() {
    let root = tempfile::tempdir().unwrap();
    let owner = root.path().join("world_provider_openrouter");
    let scope = SecretScope::named("world_provider_openrouter").unwrap();
    let name = SecretName::new("OPENROUTER_API_KEY").unwrap();
    let store = SecretsManager::new_with_keyring_store(
        owner.clone(),
        SecretsBackendKind::Local,
        Arc::new(LocalFileKeyringStore::new(owner.join("secrets/keyring"))),
    );
    store
        .set(&scope, &name, "test-interface-private-key")
        .unwrap();
    let mut descriptor: InterfaceDescriptor = serde_json::from_str(include_str!(
        "../../../../../world_provider_openrouter/interface.json"
    ))
    .unwrap();
    let config = descriptor_config(&descriptor).unwrap().unwrap();
    let mut transport = descriptor
        .interface_driver_config
        .as_ref()
        .unwrap()
        .transport
        .clone()
        .unwrap();
    transport.auth_env_key = None;
    transport.auth_secret_ref =
        Some("secret://world_provider_openrouter/OPENROUTER_API_KEY".into());
    let (arrivals, _) = tokio::sync::mpsc::channel(1);
    let mut ctx = InterfaceLiftContext::unprivileged(
        descriptor.id.clone(),
        owner.clone(),
        None,
        owner.clone(),
        root.path().join("movements.jsonl"),
        root.path().join("blobs"),
        root.path().join("ledger-index.sqlite"),
        BTreeMap::from([("activation_secrets_home".into(), owner.clone())]),
        arrivals,
        Default::default(),
    );
    assert_eq!(
        resolve_api_key(&ctx, &descriptor, &transport, &config)
            .unwrap()
            .as_deref(),
        Some("test-interface-private-key")
    );
    descriptor.id = octopus_descriptors::InterfaceId::new("world_provider_openrouter_task");
    ctx.root_slots.clear();
    ctx.root_slots
        .insert(PROVIDER_SECRET_OWNER_ROOT_SLOT.into(), owner.clone());
    assert_eq!(
        resolve_api_key(&ctx, &descriptor, &transport, &config)
            .unwrap()
            .as_deref(),
        Some("test-interface-private-key")
    );
    let mut wrong_scope = transport.clone();
    wrong_scope.auth_secret_ref = Some("secret://world_provider_other/OPENROUTER_API_KEY".into());
    assert!(
        resolve_api_key(&ctx, &descriptor, &wrong_scope, &config)
            .unwrap_err()
            .contains("scope must equal")
    );
    store.delete(&scope, &name).unwrap();
    assert!(
        resolve_api_key(&ctx, &descriptor, &transport, &config)
            .unwrap_err()
            .contains("credential is missing")
    );
}
