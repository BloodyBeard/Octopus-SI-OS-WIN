use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use octopus_descriptors::InterfaceId;
use octopus_interfaces::InterfaceDescriptor;
use octopus_secrets::{
    LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind, SecretsManager,
};
use octopus_world_provider_nous_oauth::{NousOAuthCredentialManager, NousOAuthCredentials};
use octopus_world_provider_openai_compatible::WorldProviderInterfaceDriver;

fn descriptor() -> InterfaceDescriptor {
    serde_json::from_str(include_str!("../../../interface.json")).unwrap()
}

fn descriptor_for_exact_interface(id: &str) -> InterfaceDescriptor {
    let mut descriptor = descriptor();
    descriptor.id = InterfaceId::new(id);
    let config = descriptor.interface_driver_config.as_mut().unwrap();
    let oauth = config
        .private_config
        .get_mut("nous_oauth")
        .unwrap()
        .as_object_mut()
        .unwrap();
    let credentials = format!("secret://{id}/NOUS_OAUTH_CREDENTIALS");
    let cache = format!("secret://{id}/NOUS_INVOKE_TOKEN_CACHE");
    let device = format!("secret://{id}/NOUS_DEVICE_STATE");
    let refresh_journal = format!("secret://{id}/NOUS_OAUTH_REFRESH_JOURNAL");
    oauth.insert(
        "credentials_secret_ref".to_string(),
        serde_json::json!(credentials),
    );
    oauth.insert(
        "invoke_token_cache_secret_ref".to_string(),
        serde_json::json!(cache),
    );
    oauth.insert(
        "device_state_secret_ref".to_string(),
        serde_json::json!(device),
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

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn invoke_jwt(scope: &str) -> String {
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::json!({"alg": "RS256", "typ": "JWT"}).to_string());
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::json!({"scope": scope, "exp": now_seconds() + 3600}).to_string());
    format!("{header}.{payload}.signature")
}

fn set_credentials(home: &std::path::Path, scope_value: &str) {
    let scope = SecretScope::named("world_provider_nous_oauth").unwrap();
    let name = SecretName::new("NOUS_OAUTH_CREDENTIALS").unwrap();
    secrets(home)
        .set(
            &scope,
            &name,
            &serde_json::json!({
                "schema_version": 1,
                "access_token": invoke_jwt(scope_value),
                "refresh_token": "rotating-refresh-token",
                "token_type": "Bearer",
                "scope": scope_value,
                "expires_at_unix_ms": (now_seconds() + 3600) * 1000,
                "inference_base_url": "https://inference-api.nousresearch.com/v1"
            })
            .to_string(),
        )
        .unwrap();
}

#[tokio::test]
async fn exact_descriptor_uses_scoped_invoke_jwt_and_generic_wire() {
    let home = tempfile::tempdir().unwrap();
    set_credentials(home.path(), "inference:invoke");
    let descriptor = descriptor();
    let manager = NousOAuthCredentialManager::from_descriptor(&descriptor, home.path()).unwrap();
    manager.ensure_invoke_token(false).await.unwrap();
    let scope = SecretScope::named("world_provider_nous_oauth").unwrap();
    let cache = SecretName::new("NOUS_INVOKE_TOKEN_CACHE").unwrap();
    let token = secrets(home.path()).get(&scope, &cache).unwrap().unwrap();
    assert_eq!(token.split('.').count(), 3);
    assert!(NousOAuthCredentialManager::handles_label(
        "provider.auth.device.start"
    ));
    assert!(NousOAuthCredentialManager::handles_label(
        "provider.auth.device.poll"
    ));
    WorldProviderInterfaceDriver::from_descriptor_with_secret_home(&descriptor, Some(home.path()))
        .unwrap();
}

#[tokio::test]
async fn malformed_credential_schema_fails_closed_without_cache() {
    let home = tempfile::tempdir().unwrap();
    let scope = SecretScope::named("world_provider_nous_oauth").unwrap();
    let name = SecretName::new("NOUS_OAUTH_CREDENTIALS").unwrap();
    secrets(home.path())
        .set(
            &scope,
            &name,
            &serde_json::json!({
                "schema_version": 1,
                "access_token": invoke_jwt("inference:invoke"),
                "refresh_token": "rotating-refresh-token",
                "token_type": "Bearer",
                "scope": "inference:invoke",
                "expires_at_unix_ms": (now_seconds() + 3600) * 1000,
                "inference_base_url": "https://inference-api.nousresearch.com/v1",
                "ambient_auth_path": "~/.hermes/auth.json"
            })
            .to_string(),
        )
        .unwrap();
    let manager = NousOAuthCredentialManager::from_descriptor(&descriptor(), home.path()).unwrap();
    let failure = manager.ensure_invoke_token(false).await.unwrap_err();
    assert_eq!(failure.code, "nous_oauth_credentials_invalid");
    let cache = SecretName::new("NOUS_INVOKE_TOKEN_CACHE").unwrap();
    assert_eq!(secrets(home.path()).get(&scope, &cache).unwrap(), None);
}

#[tokio::test]
async fn exact_run_copy_uses_its_rebased_secret_scope() {
    let home = tempfile::tempdir().unwrap();
    let id = "world_provider_nous_oauth_run_a";
    let scope = SecretScope::named(id).unwrap();
    let name = SecretName::new("NOUS_OAUTH_CREDENTIALS").unwrap();
    secrets(home.path())
        .set(
            &scope,
            &name,
            &serde_json::json!({
                "schema_version": 1,
                "access_token": invoke_jwt("inference:invoke"),
                "refresh_token": "run-refresh-token",
                "token_type": "Bearer",
                "scope": "inference:invoke",
                "expires_at_unix_ms": (now_seconds() + 3600) * 1000,
                "inference_base_url": "https://inference-api.nousresearch.com/v1"
            })
            .to_string(),
        )
        .unwrap();
    let descriptor = descriptor_for_exact_interface(id);
    let manager = NousOAuthCredentialManager::from_descriptor(&descriptor, home.path()).unwrap();
    manager.ensure_invoke_token(false).await.unwrap();
    let cache = SecretName::new("NOUS_INVOKE_TOKEN_CACHE").unwrap();
    assert!(secrets(home.path()).get(&scope, &cache).unwrap().is_some());
}

#[tokio::test]
async fn prepared_refresh_journal_blocks_an_ambiguous_token_replay() {
    let home = tempfile::tempdir().unwrap();
    let scope = SecretScope::named("world_provider_nous_oauth").unwrap();
    let credentials = NousOAuthCredentials {
        schema_version: 1,
        access_token: invoke_jwt("inference:invoke"),
        refresh_token: "rotating-refresh-token".to_string(),
        token_type: "Bearer".to_string(),
        scope: "inference:invoke".to_string(),
        expires_at_unix_ms: (now_seconds() + 3600) * 1000,
        inference_base_url: "https://inference-api.nousresearch.com/v1".to_string(),
    };
    let credential_bytes = serde_json::to_vec(&credentials).unwrap();
    let manager = secrets(home.path());
    manager
        .set(
            &scope,
            &SecretName::new("NOUS_OAUTH_CREDENTIALS").unwrap(),
            std::str::from_utf8(&credential_bytes).unwrap(),
        )
        .unwrap();
    manager
        .set(
            &scope,
            &SecretName::new("NOUS_OAUTH_REFRESH_JOURNAL").unwrap(),
            &serde_json::json!({
                "phase": "prepared",
                "schema_version": 1,
                "provider_interface": "world_provider_nous_oauth",
                "source_fingerprint": blake3::hash(&credential_bytes).to_hex().to_string()
            })
            .to_string(),
        )
        .unwrap();
    let credentials =
        NousOAuthCredentialManager::from_descriptor(&descriptor(), home.path()).unwrap();
    let failure = credentials.ensure_invoke_token(false).await.unwrap_err();
    assert_eq!(failure.code, "nous_oauth_refresh_outcome_unknown");
    assert!(manager
        .get(&scope, &SecretName::new("NOUS_INVOKE_TOKEN_CACHE").unwrap())
        .unwrap()
        .is_none());
    assert!(manager
        .get(
            &scope,
            &SecretName::new("NOUS_OAUTH_REFRESH_JOURNAL").unwrap()
        )
        .unwrap()
        .is_some());
}
