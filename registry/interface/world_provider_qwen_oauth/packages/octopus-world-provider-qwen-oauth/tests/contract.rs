use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use octopus_descriptors::InterfaceId;
use octopus_interfaces::InterfaceDescriptor;
use octopus_secrets::{
    LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind, SecretsManager,
};
use octopus_world_provider_openai_compatible::WorldProviderInterfaceDriver;
use octopus_world_provider_qwen_oauth::{QwenOAuthCredentialManager, QwenOAuthCredentials};

fn descriptor() -> InterfaceDescriptor {
    serde_json::from_str(include_str!("../../../interface.json")).unwrap()
}

fn descriptor_for_exact_interface(id: &str) -> InterfaceDescriptor {
    let mut descriptor = descriptor();
    descriptor.id = InterfaceId::new(id);
    let config = descriptor.interface_driver_config.as_mut().unwrap();
    let oauth = config
        .private_config
        .get_mut("qwen_oauth")
        .unwrap()
        .as_object_mut()
        .unwrap();
    let credentials = format!("secret://{id}/QWEN_OAUTH_CREDENTIALS");
    let cache = format!("secret://{id}/QWEN_OAUTH_ACCESS_TOKEN_CACHE");
    let refresh_journal = format!("secret://{id}/QWEN_OAUTH_REFRESH_JOURNAL");
    oauth.insert(
        "credentials_secret_ref".to_string(),
        serde_json::json!(credentials),
    );
    oauth.insert(
        "access_token_cache_secret_ref".to_string(),
        serde_json::json!(cache),
    );
    oauth.insert(
        "refresh_journal_secret_ref".to_string(),
        serde_json::json!(refresh_journal),
    );
    config.transport.as_mut().unwrap().auth_secret_ref = Some(cache);
    descriptor
}

fn secrets(home: &std::path::Path) -> SecretsManager {
    SecretsManager::new_with_keyring_store(
        home.to_path_buf(),
        SecretsBackendKind::Local,
        Arc::new(LocalFileKeyringStore::new(
            home.join("secrets").join("keyring"),
        )),
    )
}

fn future_expiry() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    now + 3_600_000
}

#[tokio::test]
async fn exact_descriptor_uses_scoped_credentials_and_generic_wire() {
    let home = tempfile::tempdir().unwrap();
    let scope = SecretScope::named("world_provider_qwen_oauth").unwrap();
    let credential_name = SecretName::new("QWEN_OAUTH_CREDENTIALS").unwrap();
    let cache_name = SecretName::new("QWEN_OAUTH_ACCESS_TOKEN_CACHE").unwrap();
    secrets(home.path())
        .set(
            &scope,
            &credential_name,
            &serde_json::json!({
                "schema_version": 1,
                "access_token": "access-token-value",
                "refresh_token": "refresh-token-value",
                "token_type": "Bearer",
                "expires_at_unix_ms": future_expiry(),
                "resource_url": "portal.qwen.ai"
            })
            .to_string(),
        )
        .unwrap();
    let descriptor = descriptor();
    let manager = QwenOAuthCredentialManager::from_descriptor(&descriptor, home.path()).unwrap();
    manager.ensure_access_token(false).await.unwrap();
    assert_eq!(
        secrets(home.path()).get(&scope, &cache_name).unwrap(),
        Some("access-token-value".to_string())
    );
    let provider_descriptor = manager.provider_descriptor(&descriptor).unwrap();
    let headers = &provider_descriptor
        .interface_driver_config
        .as_ref()
        .unwrap()
        .transport
        .as_ref()
        .unwrap()
        .headers;
    assert_eq!(headers["X-DashScope-AuthType"], "qwen-oauth");
    assert_eq!(headers["X-DashScope-CacheControl"], "enable");
    assert!(headers["User-Agent"].starts_with("QwenCode/0.14.1 ("));
    WorldProviderInterfaceDriver::from_descriptor_with_secret_home(
        &provider_descriptor,
        Some(home.path()),
    )
    .unwrap();
}

#[tokio::test]
async fn malformed_credential_schema_fails_closed_without_cache() {
    let home = tempfile::tempdir().unwrap();
    let scope = SecretScope::named("world_provider_qwen_oauth").unwrap();
    let credential_name = SecretName::new("QWEN_OAUTH_CREDENTIALS").unwrap();
    let cache_name = SecretName::new("QWEN_OAUTH_ACCESS_TOKEN_CACHE").unwrap();
    secrets(home.path())
        .set(
            &scope,
            &credential_name,
            &serde_json::json!({
                "schema_version": 1,
                "access_token": "access-token-value",
                "refresh_token": "refresh-token-value",
                "token_type": "Bearer",
                "expires_at_unix_ms": future_expiry(),
                "resource_url": "portal.qwen.ai",
                "ambient_path": "~/.qwen/oauth_creds.json"
            })
            .to_string(),
        )
        .unwrap();
    let manager = QwenOAuthCredentialManager::from_descriptor(&descriptor(), home.path()).unwrap();
    let failure = manager.ensure_access_token(false).await.unwrap_err();
    assert_eq!(failure.code, "qwen_oauth_credentials_invalid");
    assert_eq!(secrets(home.path()).get(&scope, &cache_name).unwrap(), None);
}

#[tokio::test]
async fn exact_run_copy_uses_its_rebased_secret_scope() {
    let home = tempfile::tempdir().unwrap();
    let id = "world_provider_qwen_oauth_run_a";
    let scope = SecretScope::named(id).unwrap();
    let credential_name = SecretName::new("QWEN_OAUTH_CREDENTIALS").unwrap();
    let cache_name = SecretName::new("QWEN_OAUTH_ACCESS_TOKEN_CACHE").unwrap();
    secrets(home.path())
        .set(
            &scope,
            &credential_name,
            &serde_json::json!({
                "schema_version": 1,
                "access_token": "run-access-token",
                "refresh_token": "run-refresh-token",
                "token_type": "Bearer",
                "expires_at_unix_ms": future_expiry(),
                "resource_url": "portal.qwen.ai"
            })
            .to_string(),
        )
        .unwrap();
    let descriptor = descriptor_for_exact_interface(id);
    let manager = QwenOAuthCredentialManager::from_descriptor(&descriptor, home.path()).unwrap();
    manager.ensure_access_token(false).await.unwrap();
    assert_eq!(
        secrets(home.path()).get(&scope, &cache_name).unwrap(),
        Some("run-access-token".to_string())
    );
}

#[tokio::test]
async fn prepared_refresh_journal_blocks_an_ambiguous_token_replay() {
    let home = tempfile::tempdir().unwrap();
    let scope = SecretScope::named("world_provider_qwen_oauth").unwrap();
    let credentials = QwenOAuthCredentials {
        schema_version: 1,
        access_token: "old-access-token".to_string(),
        refresh_token: "rotating-refresh-token".to_string(),
        token_type: "Bearer".to_string(),
        expires_at_unix_ms: future_expiry(),
        resource_url: "portal.qwen.ai".to_string(),
    };
    let credential_bytes = serde_json::to_vec(&credentials).unwrap();
    let manager = secrets(home.path());
    manager
        .set(
            &scope,
            &SecretName::new("QWEN_OAUTH_CREDENTIALS").unwrap(),
            std::str::from_utf8(&credential_bytes).unwrap(),
        )
        .unwrap();
    manager
        .set(
            &scope,
            &SecretName::new("QWEN_OAUTH_REFRESH_JOURNAL").unwrap(),
            &serde_json::json!({
                "phase": "prepared",
                "schema_version": 1,
                "provider_interface": "world_provider_qwen_oauth",
                "source_fingerprint": blake3::hash(&credential_bytes).to_hex().to_string()
            })
            .to_string(),
        )
        .unwrap();
    let credentials =
        QwenOAuthCredentialManager::from_descriptor(&descriptor(), home.path()).unwrap();
    let failure = credentials.ensure_access_token(false).await.unwrap_err();
    assert_eq!(failure.code, "qwen_oauth_refresh_outcome_unknown");
    assert!(manager
        .get(
            &scope,
            &SecretName::new("QWEN_OAUTH_ACCESS_TOKEN_CACHE").unwrap()
        )
        .unwrap()
        .is_none());
    assert!(manager
        .get(
            &scope,
            &SecretName::new("QWEN_OAUTH_REFRESH_JOURNAL").unwrap()
        )
        .unwrap()
        .is_some());
}
