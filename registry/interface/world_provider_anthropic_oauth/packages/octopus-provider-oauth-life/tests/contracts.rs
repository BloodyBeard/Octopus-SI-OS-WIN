use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use std::path::Path;

use octopus_interfaces::InterfaceDescriptor;
use octopus_provider_oauth_life::{OAuthCredentialManager, OAuthCredentials};
use octopus_secrets::{
    LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind, SecretsManager,
};
use serde_json::Value;

const DESCRIPTORS: [&str; 3] = [
    "../../world_provider_anthropic_oauth/interface.json",
    "../../world_provider_xai_oauth/interface.json",
    "../../world_provider_minimax_oauth/interface.json",
];

fn descriptor(path: &Path) -> (Value, InterfaceDescriptor) {
    let bytes = std::fs::read(path).expect("exact OAuth descriptor is readable");
    let value: Value = serde_json::from_slice(&bytes).expect("exact OAuth descriptor is JSON");
    let descriptor = serde_json::from_slice(&bytes).expect("exact OAuth descriptor is typed");
    (value, descriptor)
}

fn secrets(home: &Path) -> SecretsManager {
    SecretsManager::new_with_keyring_store(
        home.to_path_buf(),
        SecretsBackendKind::Local,
        Arc::new(LocalFileKeyringStore::new(
            home.join("secrets").join("keyring"),
        )),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn exact_oauth_interfaces_are_scoped_strict_and_logged_out_by_default() {
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in DESCRIPTORS {
        let path = package_root.join(relative);
        let (value, descriptor) = descriptor(&path);
        assert!(value.get("reference").is_none());
        assert_eq!(
            value.get("id").and_then(Value::as_str),
            path.parent()
                .and_then(Path::file_name)
                .and_then(|id| id.to_str())
        );
        let life_bytes = std::fs::read(
            path.parent()
                .expect("exact OAuth interface has a folder")
                .join("life.json"),
        )
        .expect("exact OAuth life descriptor is readable");
        let life: octopus_interface_transport::SpawnLifeConfig =
            serde_json::from_slice(&life_bytes).expect("exact OAuth life descriptor is typed");
        assert_eq!(life.processes.len(), 1);
        assert!(life.processes[0].authority.allow_network);
        assert!(life.processes[0].authority.allow_local_ipc);
        let secrets_home = tempfile::tempdir().expect("isolated secrets home");
        let manager = OAuthCredentialManager::from_descriptor(&descriptor, secrets_home.path())
            .expect("exact OAuth descriptor resolves");
        let status = manager
            .handle_artifact("provider.auth.status.request", b"{}")
            .await
            .expect("empty secret store has a typed status");
        assert_eq!(status.label, "provider.auth.status");
        assert_eq!(status.payload["provider_interface"], descriptor.id.as_str());
        assert_eq!(status.payload["authenticated"], false);
        assert_eq!(status.payload["login_pending"], false);
        assert_eq!(
            status.payload["login_start_artifact"]["label"],
            "provider.auth.login.start"
        );
        assert_eq!(
            status.payload["login_start_artifact"]["payload"],
            Value::Object(Default::default())
        );
        assert_eq!(status.payload["browser_auto_opened"], false);
        if descriptor.id.as_str() == "world_provider_anthropic_oauth" {
            let action = manager
                .handle_artifact("provider.auth.login.start", b"{}")
                .await
                .expect("Anthropic manual PKCE start is local and actionable");
            assert_eq!(action.label, "provider.auth.login.action");
            assert!(action.payload["verification_uri"]
                .as_str()
                .is_some_and(|uri| uri.starts_with("https://claude.ai/oauth/authorize?")));
            assert_eq!(
                action.payload["next_artifact"]["label"],
                "provider.auth.login.complete"
            );
            assert_eq!(action.payload["browser_auto_opened"], false);
            let pending = manager
                .handle_artifact("provider.auth.status.request", b"{}")
                .await
                .expect("pending login remains structurally visible");
            assert_eq!(pending.payload["login_pending"], true);
            manager
                .handle_artifact("provider.auth.logout", b"{}")
                .await
                .expect("logout deletes the isolated test state");
        }
        if descriptor.id.as_str() == "world_provider_minimax_oauth" {
            let error = manager
                .handle_artifact("provider.auth.login.start", b"{}")
                .await
                .expect_err("MiniMax login start cannot send without dispatch admission");
            assert_eq!(error.code, "oauth_dispatch_required");
        }

        let mut malformed = value;
        malformed["interface_driver_config"]["private_config"]["oauth"]
            .as_object_mut()
            .expect("OAuth config is an object")
            .insert(
                "ambient_credentials_home".to_string(),
                Value::String("~/.hermes".to_string()),
            );
        let malformed: InterfaceDescriptor =
            serde_json::from_value(malformed).expect("outer descriptor stays typed");
        let error = match OAuthCredentialManager::from_descriptor(&malformed, secrets_home.path()) {
            Ok(_) => panic!("ambient credential fields must fail closed"),
            Err(error) => error,
        };
        assert_eq!(error.code, "oauth_config_invalid");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn exact_oauth_interface_copy_derives_identity_from_its_rebased_folder_contract() {
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (mut value, _) =
        descriptor(&package_root.join("../../world_provider_anthropic_oauth/interface.json"));
    let clone_id = "world_provider_anthropic_oauth_exact_copy";
    value["id"] = Value::String(clone_id.to_string());
    let secrets_home = tempfile::tempdir().expect("isolated secrets home");
    let unrebased: InterfaceDescriptor =
        serde_json::from_value(value.clone()).expect("copied descriptor stays typed");
    let error = match OAuthCredentialManager::from_descriptor(&unrebased, secrets_home.path()) {
        Ok(_) => panic!("an exact interface copy cannot borrow another interface's secrets"),
        Err(error) => error,
    };
    assert_eq!(error.code, "oauth_secret_scope_mismatch");

    let secret_prefix = format!("secret://{clone_id}");
    value["interface_driver_config"]["private_config"]["oauth"]["credentials_secret_ref"] =
        Value::String(format!("{secret_prefix}/ANTHROPIC_OAUTH_CREDENTIALS_V1"));
    value["interface_driver_config"]["private_config"]["oauth"]["access_token_cache_secret_ref"] =
        Value::String(format!(
            "{secret_prefix}/ANTHROPIC_OAUTH_ACCESS_TOKEN_CACHE_V1"
        ));
    value["interface_driver_config"]["private_config"]["oauth"]["login_state_secret_ref"] =
        Value::String(format!("{secret_prefix}/ANTHROPIC_OAUTH_LOGIN_STATE_V1"));
    value["interface_driver_config"]["private_config"]["oauth"]["refresh_journal_secret_ref"] =
        Value::String(format!(
            "{secret_prefix}/ANTHROPIC_OAUTH_REFRESH_JOURNAL_V1"
        ));
    value["interface_driver_config"]["transport"]["auth_secret_ref"] = Value::String(format!(
        "{secret_prefix}/ANTHROPIC_OAUTH_ACCESS_TOKEN_CACHE_V1"
    ));
    let rebased: InterfaceDescriptor =
        serde_json::from_value(value).expect("rebased exact descriptor stays typed");
    let manager = OAuthCredentialManager::from_descriptor(&rebased, secrets_home.path())
        .expect("rebased exact descriptor resolves without a second identity field");
    let status = manager
        .handle_artifact("provider.auth.status.request", b"{}")
        .await
        .expect("rebased exact descriptor emits typed status");
    assert_eq!(status.payload["provider_interface"], clone_id);
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_refresh_journal_blocks_an_ambiguous_token_replay() {
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (_, descriptor) =
        descriptor(&package_root.join("../../world_provider_anthropic_oauth/interface.json"));
    let home = tempfile::tempdir().expect("isolated secrets home");
    let scope = SecretScope::named("world_provider_anthropic_oauth").unwrap();
    let stored_credentials = OAuthCredentials {
        schema_version: 1,
        provider_interface: "world_provider_anthropic_oauth".to_string(),
        flow: "anthropic_manual_pkce".to_string(),
        access_token: "old-access-token".to_string(),
        refresh_token: "rotating-refresh-token".to_string(),
        token_type: "Bearer".to_string(),
        expires_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 3_600_000,
        scope: Some("org:create_api_key user:profile user:inference".to_string()),
        id_token: None,
        resource_url: None,
    };
    let credential_bytes = serde_json::to_vec(&stored_credentials).unwrap();
    let manager = secrets(home.path());
    manager
        .set(
            &scope,
            &SecretName::new("ANTHROPIC_OAUTH_CREDENTIALS_V1").unwrap(),
            std::str::from_utf8(&credential_bytes).unwrap(),
        )
        .unwrap();
    manager
        .set(
            &scope,
            &SecretName::new("ANTHROPIC_OAUTH_REFRESH_JOURNAL_V1").unwrap(),
            &serde_json::json!({
                "phase": "prepared",
                "schema_version": 1,
                "provider_interface": "world_provider_anthropic_oauth",
                "source_fingerprint": blake3::hash(&credential_bytes).to_hex().to_string()
            })
            .to_string(),
        )
        .unwrap();
    let credential_manager =
        OAuthCredentialManager::from_descriptor(&descriptor, home.path()).unwrap();
    let status_failure = credential_manager
        .handle_artifact("provider.auth.status.request", b"{}")
        .await
        .unwrap_err();
    assert_eq!(status_failure.code, "oauth_refresh_outcome_unknown");
    let failure = credential_manager
        .ensure_access_token(false)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "oauth_refresh_outcome_unknown");
    assert!(manager
        .get(
            &scope,
            &SecretName::new("ANTHROPIC_OAUTH_ACCESS_TOKEN_CACHE_V1").unwrap()
        )
        .unwrap()
        .is_none());
    assert!(manager
        .get(
            &scope,
            &SecretName::new("ANTHROPIC_OAUTH_REFRESH_JOURNAL_V1").unwrap()
        )
        .unwrap()
        .is_some());
    let refreshed = OAuthCredentials {
        access_token: "new-access-token".to_string(),
        refresh_token: "new-refresh-token".to_string(),
        expires_at_unix_ms: stored_credentials.expires_at_unix_ms + 3_600_000,
        ..stored_credentials
    };
    manager
        .set(
            &scope,
            &SecretName::new("ANTHROPIC_OAUTH_REFRESH_JOURNAL_V1").unwrap(),
            &serde_json::json!({
                "phase": "received",
                "schema_version": 1,
                "provider_interface": "world_provider_anthropic_oauth",
                "source_fingerprint": blake3::hash(&credential_bytes).to_hex().to_string(),
                "credentials": refreshed
            })
            .to_string(),
        )
        .unwrap();
    let status = credential_manager
        .handle_artifact("provider.auth.status.request", b"{}")
        .await
        .unwrap();
    assert_eq!(status.payload["authenticated"], true);
    assert_eq!(status.payload["credential_state"], "secret_backed");
    credential_manager.ensure_access_token(false).await.unwrap();
    assert_eq!(
        manager
            .get(
                &scope,
                &SecretName::new("ANTHROPIC_OAUTH_ACCESS_TOKEN_CACHE_V1").unwrap()
            )
            .unwrap(),
        Some("new-access-token".to_string())
    );
    assert!(manager
        .get(
            &scope,
            &SecretName::new("ANTHROPIC_OAUTH_REFRESH_JOURNAL_V1").unwrap()
        )
        .unwrap()
        .is_none());
}
