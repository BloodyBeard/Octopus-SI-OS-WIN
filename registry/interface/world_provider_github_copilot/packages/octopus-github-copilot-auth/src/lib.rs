use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use octopus_interfaces::{InterfaceDescriptor, InterfaceTransportConfig};
use octopus_secrets::{
    redact_secrets, LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind,
    SecretsManager,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

mod dispatch;

pub use dispatch::GitHubAuthDispatchAdmission;

pub const DEVICE_START_LABEL: &str = "provider.auth.device.start";
pub const DEVICE_POLL_LABEL: &str = "provider.auth.device.poll";
pub const DEVICE_CODE_LABEL: &str = "provider.auth.device.code";
pub const AUTH_STATUS_LABEL: &str = "provider.auth.status";

#[derive(Clone, Debug)]
pub struct AuthArtifact {
    pub label: &'static str,
    pub payload: Value,
}

#[derive(Clone, Debug)]
pub struct AuthFailure {
    pub class: &'static str,
    pub retryable: bool,
    pub code: &'static str,
    pub detail: String,
    pub evidence: Option<Value>,
}

impl AuthFailure {
    fn protocol(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "protocol",
            retryable: false,
            code,
            detail: detail.into(),
            evidence: None,
        }
    }

    fn unavailable(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "unavailable",
            retryable: false,
            code,
            detail: detail.into(),
            evidence: None,
        }
    }

    fn transport(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "transport",
            retryable: true,
            code,
            detail: detail.into(),
            evidence: None,
        }
    }

    fn endpoint(
        code: &'static str,
        retryable: bool,
        detail: impl Into<String>,
        status: u16,
        bytes: &[u8],
    ) -> Self {
        Self {
            class: "endpoint",
            retryable,
            code,
            detail: detail.into(),
            evidence: Some(json!({
                "status": status,
                "response_bytes": bytes.len(),
                "response_blake3": blake3::hash(bytes).to_hex().to_string()
            })),
        }
    }
}

impl std::fmt::Display for AuthFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for AuthFailure {}

fn required_dispatch<'a>(
    dispatch: Option<&'a GitHubAuthDispatchAdmission<'a>>,
) -> Result<&'a GitHubAuthDispatchAdmission<'a>, AuthFailure> {
    dispatch.ok_or_else(|| {
        AuthFailure::protocol(
            "github_auth_dispatch_required",
            "GitHub auth network mutation requires an admitted provider.transport.dispatch artifact",
        )
    })
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitHubAuthConfig {
    raw_token_secret_ref: String,
    api_token_secret_ref: String,
    api_token_expiry_secret_ref: String,
    device_state_secret_ref: String,
    oauth: OAuthConfig,
    token_exchange: TokenExchangeConfig,
    request_timeout_ms: u64,
    max_response_bytes: usize,
    retryable_statuses: Vec<u16>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthConfig {
    client_id: String,
    scope: String,
    device_code_url: String,
    access_token_url: String,
    user_agent: String,
    poll_safety_margin_seconds: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenExchangeConfig {
    url: String,
    user_agent: String,
    editor_version: String,
    refresh_margin_seconds: u64,
}

#[derive(Clone)]
struct SecretHandle {
    scope: SecretScope,
    name: SecretName,
}

#[derive(Clone)]
pub struct GitHubCredentialManager {
    config: GitHubAuthConfig,
    raw_token: SecretHandle,
    api_token: SecretHandle,
    api_token_expiry: SecretHandle,
    device_state: SecretHandle,
    secrets: SecretsManager,
    client: reqwest::Client,
    retryable_statuses: Vec<u16>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeviceState {
    device_code: String,
    created_at_unix_seconds: u64,
    expires_at_unix_seconds: u64,
    interval_seconds: u64,
}

impl GitHubCredentialManager {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: &Path,
    ) -> Result<Self, AuthFailure> {
        let metadata = descriptor
            .interface_driver_config
            .as_ref()
            .ok_or_else(|| {
                AuthFailure::protocol(
                    "github_auth_config_missing",
                    format!("interface '{}' has no driver config", descriptor.id),
                )
            })?
            .private_config
            .get("github_auth")
            .cloned()
            .ok_or_else(|| {
                AuthFailure::protocol(
                    "github_auth_config_missing",
                    format!(
                        "interface '{}' has no private_config.github_auth",
                        descriptor.id
                    ),
                )
            })?;
        let config: GitHubAuthConfig = serde_json::from_value(metadata).map_err(|error| {
            AuthFailure::protocol(
                "github_auth_config_invalid",
                format!(
                    "interface '{}' GitHub auth config is invalid: {error}",
                    descriptor.id
                ),
            )
        })?;
        validate_config(&config)?;
        let raw_token = parse_secret_ref(&config.raw_token_secret_ref)?;
        let api_token = parse_secret_ref(&config.api_token_secret_ref)?;
        let api_token_expiry = parse_secret_ref(&config.api_token_expiry_secret_ref)?;
        let device_state = parse_secret_ref(&config.device_state_secret_ref)?;
        for handle in [&raw_token, &api_token, &api_token_expiry, &device_state] {
            if handle.scope.name() != Some(descriptor.id.as_str()) {
                return Err(AuthFailure::protocol(
                    "github_auth_scope_mismatch",
                    format!(
                        "interface '{}' credential handle must use its exact interface scope",
                        descriptor.id
                    ),
                ));
            }
        }
        let secrets = SecretsManager::new_with_keyring_store(
            activation_secrets_home.to_path_buf(),
            SecretsBackendKind::Local,
            Arc::new(LocalFileKeyringStore::new(
                activation_secrets_home.join("secrets").join("keyring"),
            )),
        );
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(config.request_timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                AuthFailure::unavailable(
                    "github_auth_client_build_failed",
                    format!("GitHub auth HTTP client could not be built: {error}"),
                )
            })?;
        Ok(Self {
            retryable_statuses: config.retryable_statuses.clone(),
            config,
            raw_token,
            api_token,
            api_token_expiry,
            device_state,
            secrets,
            client,
        })
    }

    pub fn handles_label(label: &str) -> bool {
        matches!(label, DEVICE_START_LABEL | DEVICE_POLL_LABEL)
    }

    pub async fn handle_artifact(
        &self,
        label: &str,
        payload: &[u8],
    ) -> Result<AuthArtifact, AuthFailure> {
        self.handle_artifact_inner(label, payload, None).await
    }

    pub async fn handle_artifact_with_dispatch(
        &self,
        label: &str,
        payload: &[u8],
        dispatch: &GitHubAuthDispatchAdmission<'_>,
    ) -> Result<AuthArtifact, AuthFailure> {
        self.handle_artifact_inner(label, payload, Some(dispatch))
            .await
    }

    async fn handle_artifact_inner(
        &self,
        label: &str,
        payload: &[u8],
        dispatch: Option<&GitHubAuthDispatchAdmission<'_>>,
    ) -> Result<AuthArtifact, AuthFailure> {
        require_empty_json_object(payload)?;
        match label {
            DEVICE_START_LABEL => self.start_device_flow(dispatch).await,
            DEVICE_POLL_LABEL => self.poll_device_flow(dispatch).await,
            _ => Err(AuthFailure::protocol(
                "github_auth_label_unsupported",
                format!("GitHub auth does not accept artifact label '{label}'"),
            )),
        }
    }

    pub fn raw_github_token(&self) -> Result<String, AuthFailure> {
        let token = self.read_required_secret(&self.raw_token, "GitHub token")?;
        validate_raw_token(&token)?;
        Ok(token)
    }

    pub fn raw_token_handle_fingerprint(&self) -> String {
        blake3::hash(self.config.raw_token_secret_ref.as_bytes())
            .to_hex()
            .to_string()
    }

    pub fn interface_id(&self) -> &str {
        self.raw_token.scope.name().unwrap_or("global")
    }

    pub async fn ensure_copilot_api_token(&self) -> Result<String, AuthFailure> {
        let now = unix_seconds()?;
        let margin = self.config.token_exchange.refresh_margin_seconds;
        let cached_token = self.read_optional_secret(&self.api_token)?;
        let cached_expiry = self
            .read_optional_secret(&self.api_token_expiry)?
            .and_then(|value| value.parse::<u64>().ok());
        if let (Some(token), Some(expiry)) = (cached_token, cached_expiry) {
            if !token.trim().is_empty() && now.saturating_add(margin) < expiry {
                return Ok(token);
            }
        }
        let raw_token = self.raw_github_token()?;
        let response = self
            .client
            .get(&self.config.token_exchange.url)
            .header("Authorization", format!("token {raw_token}"))
            .header("User-Agent", &self.config.token_exchange.user_agent)
            .header("Accept", "application/json")
            .header("Editor-Version", &self.config.token_exchange.editor_version)
            .send()
            .await
            .map_err(|error| {
                AuthFailure::transport(
                    "copilot_token_exchange_send_failed",
                    redact_detail(format!("Copilot token exchange could not be sent: {error}")),
                )
            })?;
        let status = response.status().as_u16();
        let bytes = read_bounded(response, self.config.max_response_bytes).await?;
        if !(200..300).contains(&status) {
            return Err(AuthFailure::endpoint(
                "copilot_token_exchange_rejected",
                self.retryable_statuses.contains(&status),
                format!("Copilot token exchange returned HTTP {status}"),
                status,
                &bytes,
            ));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            AuthFailure::protocol(
                "copilot_token_exchange_json_invalid",
                format!("Copilot token exchange returned invalid JSON: {error}"),
            )
        })?;
        let token = required_trimmed_string(&value, "token", "Copilot token exchange")?;
        let expires_at = value
            .get("expires_at")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AuthFailure::protocol(
                    "copilot_token_exchange_expiry_missing",
                    "Copilot token exchange returned no unsigned expires_at",
                )
            })?;
        if expires_at <= now.saturating_add(margin) {
            return Err(AuthFailure::protocol(
                "copilot_token_exchange_expiry_invalid",
                "Copilot token exchange returned an already-expired token",
            ));
        }
        self.write_secret(&self.api_token, &token)?;
        self.write_secret(&self.api_token_expiry, &expires_at.to_string())?;
        Ok(token)
    }

    fn read_optional_secret(&self, handle: &SecretHandle) -> Result<Option<String>, AuthFailure> {
        self.secrets
            .get(&handle.scope, &handle.name)
            .map_err(|error| {
                AuthFailure::unavailable(
                    "github_secret_read_failed",
                    format!(
                        "GitHub credential secret '{}/{}' could not be read: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }

    fn read_required_secret(
        &self,
        handle: &SecretHandle,
        surface: &str,
    ) -> Result<String, AuthFailure> {
        self.read_optional_secret(handle)?
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                AuthFailure::unavailable(
                    "github_secret_missing",
                    format!(
                        "{surface} secret '{}/{}' is not configured",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }

    fn write_secret(&self, handle: &SecretHandle, value: &str) -> Result<(), AuthFailure> {
        self.secrets
            .set(&handle.scope, &handle.name, value)
            .map_err(|error| {
                AuthFailure::unavailable(
                    "github_secret_write_failed",
                    format!(
                        "GitHub credential secret '{}/{}' could not be written: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }

    async fn start_device_flow(
        &self,
        dispatch: Option<&GitHubAuthDispatchAdmission<'_>>,
    ) -> Result<AuthArtifact, AuthFailure> {
        let request = self
            .client
            .post(&self.config.oauth.device_code_url)
            .header("Accept", "application/json")
            .header("User-Agent", &self.config.oauth.user_agent)
            .form(&[
                ("client_id", self.config.oauth.client_id.as_str()),
                ("scope", self.config.oauth.scope.as_str()),
            ])
            .build()
            .map_err(|error| {
                AuthFailure::protocol(
                    "github_device_start_request_invalid",
                    format!("GitHub device authorization request is invalid: {error}"),
                )
            })?;
        required_dispatch(dispatch)?
            .admit_request(&request)
            .await
            .map_err(|error| {
                AuthFailure::protocol(
                    "github_auth_dispatch_admission_failed",
                    format!("GitHub device authorization dispatch was not admitted: {error}"),
                )
            })?;
        let response = self.client.execute(request).await.map_err(|error| {
            AuthFailure::transport(
                "github_device_start_send_failed",
                redact_detail(format!(
                    "GitHub device authorization could not be sent: {error}"
                )),
            )
        })?;
        let status = response.status().as_u16();
        let bytes = read_bounded(response, self.config.max_response_bytes).await?;
        if !(200..300).contains(&status) {
            return Err(AuthFailure::endpoint(
                "github_device_start_rejected",
                self.retryable_statuses.contains(&status),
                format!("GitHub device authorization returned HTTP {status}"),
                status,
                &bytes,
            ));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            AuthFailure::protocol(
                "github_device_start_json_invalid",
                format!("GitHub device authorization returned invalid JSON: {error}"),
            )
        })?;
        let device_code =
            required_trimmed_string(&value, "device_code", "GitHub device authorization")?;
        let user_code =
            required_trimmed_string(&value, "user_code", "GitHub device authorization")?;
        let verification_uri =
            required_trimmed_string(&value, "verification_uri", "GitHub device authorization")?;
        let expires_in =
            required_positive_u64(&value, "expires_in", "GitHub device authorization")?;
        let interval = required_positive_u64(&value, "interval", "GitHub device authorization")?;
        let now = unix_seconds()?;
        let state = DeviceState {
            device_code,
            created_at_unix_seconds: now,
            expires_at_unix_seconds: now.saturating_add(expires_in),
            interval_seconds: interval,
        };
        let encoded = serde_json::to_string(&state).map_err(|error| {
            AuthFailure::protocol(
                "github_device_state_encode_failed",
                format!("GitHub device authorization state could not be encoded: {error}"),
            )
        })?;
        self.write_secret(&self.device_state, &encoded)?;
        Ok(AuthArtifact {
            label: DEVICE_CODE_LABEL,
            payload: json!({
                "label": DEVICE_CODE_LABEL,
                "verification_uri": verification_uri,
                "user_code": user_code,
                "expires_in_seconds": expires_in,
                "poll_after_seconds": interval.saturating_add(self.config.oauth.poll_safety_margin_seconds),
                "credential_state": "secret_backed_pending"
            }),
        })
    }

    async fn poll_device_flow(
        &self,
        dispatch: Option<&GitHubAuthDispatchAdmission<'_>>,
    ) -> Result<AuthArtifact, AuthFailure> {
        let encoded = self.read_required_secret(&self.device_state, "GitHub device state")?;
        let mut state: DeviceState = serde_json::from_str(&encoded).map_err(|error| {
            AuthFailure::protocol(
                "github_device_state_invalid",
                format!("GitHub device authorization state is invalid: {error}"),
            )
        })?;
        let now = unix_seconds()?;
        if now >= state.expires_at_unix_seconds {
            self.delete_secret(&self.device_state)?;
            return Err(AuthFailure::unavailable(
                "github_device_code_expired",
                "GitHub device authorization code expired",
            ));
        }
        let request = self
            .client
            .post(&self.config.oauth.access_token_url)
            .header("Accept", "application/json")
            .header("User-Agent", &self.config.oauth.user_agent)
            .form(&[
                ("client_id", self.config.oauth.client_id.as_str()),
                ("device_code", state.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .build()
            .map_err(|error| {
                AuthFailure::protocol(
                    "github_device_poll_request_invalid",
                    format!("GitHub device authorization poll request is invalid: {error}"),
                )
            })?;
        required_dispatch(dispatch)?
            .admit_request(&request)
            .await
            .map_err(|error| {
                AuthFailure::protocol(
                    "github_auth_dispatch_admission_failed",
                    format!("GitHub device authorization poll dispatch was not admitted: {error}"),
                )
            })?;
        let response = self.client.execute(request).await.map_err(|error| {
            AuthFailure::transport(
                "github_device_poll_send_failed",
                redact_detail(format!("GitHub device authorization poll failed: {error}")),
            )
        })?;
        let status = response.status().as_u16();
        let bytes = read_bounded(response, self.config.max_response_bytes).await?;
        if !(200..300).contains(&status) {
            return Err(AuthFailure::endpoint(
                "github_device_poll_rejected",
                self.retryable_statuses.contains(&status),
                format!("GitHub device authorization poll returned HTTP {status}"),
                status,
                &bytes,
            ));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            AuthFailure::protocol(
                "github_device_poll_json_invalid",
                format!("GitHub device authorization poll returned invalid JSON: {error}"),
            )
        })?;
        if let Some(token) = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            validate_raw_token(token)?;
            self.write_secret(&self.raw_token, token)?;
            self.delete_secret(&self.device_state)?;
            self.delete_secret(&self.api_token)?;
            self.delete_secret(&self.api_token_expiry)?;
            self.ensure_copilot_api_token().await?;
            return Ok(AuthArtifact {
                label: AUTH_STATUS_LABEL,
                payload: json!({
                    "label": AUTH_STATUS_LABEL,
                    "authenticated": true,
                    "credential_state": "secret_backed",
                    "source": "oauth_device",
                    "token_exchanged": true
                }),
            });
        }
        let error = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("missing_access_token");
        match error {
            "authorization_pending" => Ok(AuthArtifact {
                label: AUTH_STATUS_LABEL,
                payload: json!({
                    "label": AUTH_STATUS_LABEL,
                    "authenticated": false,
                    "status": "authorization_pending",
                    "poll_after_seconds": state.interval_seconds.saturating_add(self.config.oauth.poll_safety_margin_seconds)
                }),
            }),
            "slow_down" => {
                state.interval_seconds = state.interval_seconds.saturating_add(5);
                let encoded = serde_json::to_string(&state).map_err(|encode_error| {
                    AuthFailure::protocol(
                        "github_device_state_encode_failed",
                        format!("GitHub device authorization state could not be encoded: {encode_error}"),
                    )
                })?;
                self.write_secret(&self.device_state, &encoded)?;
                Ok(AuthArtifact {
                    label: AUTH_STATUS_LABEL,
                    payload: json!({
                        "label": AUTH_STATUS_LABEL,
                        "authenticated": false,
                        "status": "slow_down",
                        "poll_after_seconds": state.interval_seconds.saturating_add(self.config.oauth.poll_safety_margin_seconds)
                    }),
                })
            }
            "expired_token" => {
                self.delete_secret(&self.device_state)?;
                Err(AuthFailure::unavailable(
                    "github_device_code_expired",
                    "GitHub device authorization code expired",
                ))
            }
            "access_denied" => {
                self.delete_secret(&self.device_state)?;
                Err(AuthFailure::unavailable(
                    "github_device_access_denied",
                    "GitHub device authorization was denied",
                ))
            }
            other => Err(AuthFailure::protocol(
                "github_device_poll_outcome_unknown",
                format!("GitHub device authorization returned unsupported outcome '{other}'"),
            )),
        }
    }

    fn delete_secret(&self, handle: &SecretHandle) -> Result<(), AuthFailure> {
        self.secrets
            .delete(&handle.scope, &handle.name)
            .map(|_| ())
            .map_err(|error| {
                AuthFailure::unavailable(
                    "github_secret_delete_failed",
                    format!(
                        "GitHub credential secret '{}/{}' could not be deleted: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }
}

pub fn install_api_token_for_transport(
    descriptor: &InterfaceDescriptor,
    api_token_secret_ref: &str,
) -> Result<InterfaceDescriptor, AuthFailure> {
    let mut adjusted = descriptor.clone();
    let config = adjusted.interface_driver_config.as_mut().ok_or_else(|| {
        AuthFailure::protocol(
            "github_transport_config_missing",
            format!("interface '{}' has no driver config", descriptor.id),
        )
    })?;
    let transport: &mut InterfaceTransportConfig = config.transport.as_mut().ok_or_else(|| {
        AuthFailure::protocol(
            "github_transport_config_missing",
            format!("interface '{}' has no transport config", descriptor.id),
        )
    })?;
    transport.auth_secret_ref = Some(api_token_secret_ref.to_string());
    transport.auth_env_key = None;
    transport.auth_header = Some("Authorization".to_string());
    transport.auth_value_prefix = Some("Bearer ".to_string());
    Ok(adjusted)
}

pub fn redact_detail(input: String) -> String {
    redact_secrets(input)
}

fn validate_config(config: &GitHubAuthConfig) -> Result<(), AuthFailure> {
    if config.request_timeout_ms == 0 || config.request_timeout_ms > 120_000 {
        return Err(AuthFailure::protocol(
            "github_auth_timeout_invalid",
            "GitHub auth request_timeout_ms must be within 1..=120000",
        ));
    }
    if config.max_response_bytes == 0 || config.max_response_bytes > 1024 * 1024 {
        return Err(AuthFailure::protocol(
            "github_auth_response_limit_invalid",
            "GitHub auth max_response_bytes must be within 1..=1048576",
        ));
    }
    for (surface, url) in [
        (
            "oauth.device_code_url",
            config.oauth.device_code_url.as_str(),
        ),
        (
            "oauth.access_token_url",
            config.oauth.access_token_url.as_str(),
        ),
        ("token_exchange.url", config.token_exchange.url.as_str()),
    ] {
        let parsed = reqwest::Url::parse(url).map_err(|error| {
            AuthFailure::protocol(
                "github_auth_url_invalid",
                format!("GitHub auth {surface} is invalid: {error}"),
            )
        })?;
        if parsed.scheme() != "https" || parsed.host_str().is_none() {
            return Err(AuthFailure::protocol(
                "github_auth_url_invalid",
                format!("GitHub auth {surface} must be an absolute HTTPS URL"),
            ));
        }
    }
    for (surface, value) in [
        ("oauth.client_id", config.oauth.client_id.as_str()),
        ("oauth.scope", config.oauth.scope.as_str()),
        ("oauth.user_agent", config.oauth.user_agent.as_str()),
        (
            "token_exchange.user_agent",
            config.token_exchange.user_agent.as_str(),
        ),
        (
            "token_exchange.editor_version",
            config.token_exchange.editor_version.as_str(),
        ),
    ] {
        if value.is_empty() || value.trim() != value || value.contains('\r') || value.contains('\n')
        {
            return Err(AuthFailure::protocol(
                "github_auth_value_invalid",
                format!("GitHub auth {surface} must be non-empty, trimmed, and single-line"),
            ));
        }
    }
    if config.retryable_statuses.is_empty()
        || config
            .retryable_statuses
            .iter()
            .any(|status| *status < 400 || *status > 599)
    {
        return Err(AuthFailure::protocol(
            "github_auth_retry_statuses_invalid",
            "GitHub auth retryable_statuses must contain HTTP error statuses",
        ));
    }
    Ok(())
}

fn parse_secret_ref(reference: &str) -> Result<SecretHandle, AuthFailure> {
    let path = reference.strip_prefix("secret://").ok_or_else(|| {
        AuthFailure::protocol(
            "github_secret_ref_invalid",
            "GitHub credential references must start with secret://",
        )
    })?;
    let (scope, name) = path.split_once('/').ok_or_else(|| {
        AuthFailure::protocol(
            "github_secret_ref_invalid",
            "GitHub credential references must be secret://<scope>/<name>",
        )
    })?;
    if name.contains('/') {
        return Err(AuthFailure::protocol(
            "github_secret_ref_invalid",
            "GitHub credential references must name one scoped secret",
        ));
    }
    Ok(SecretHandle {
        scope: SecretScope::named(scope.to_string()).map_err(|error| {
            AuthFailure::protocol(
                "github_secret_scope_invalid",
                format!("GitHub credential scope is invalid: {error}"),
            )
        })?,
        name: SecretName::new(name).map_err(|error| {
            AuthFailure::protocol(
                "github_secret_name_invalid",
                format!("GitHub credential name is invalid: {error}"),
            )
        })?,
    })
}

fn validate_raw_token(token: &str) -> Result<(), AuthFailure> {
    if token.is_empty() || token.trim() != token || token.contains('\r') || token.contains('\n') {
        return Err(AuthFailure::unavailable(
            "github_token_invalid",
            "GitHub token must be non-empty, trimmed, and single-line",
        ));
    }
    if token.starts_with("ghp_") {
        return Err(AuthFailure::unavailable(
            "github_classic_pat_unsupported",
            "GitHub classic personal access tokens are not accepted by this Copilot interface",
        ));
    }
    Ok(())
}

fn require_empty_json_object(payload: &[u8]) -> Result<(), AuthFailure> {
    let value: Value = serde_json::from_slice(payload).map_err(|error| {
        AuthFailure::protocol(
            "github_auth_artifact_json_invalid",
            format!("GitHub auth artifact is not valid JSON: {error}"),
        )
    })?;
    if !value.as_object().is_some_and(Map::is_empty) {
        return Err(AuthFailure::protocol(
            "github_auth_artifact_invalid",
            "GitHub auth artifacts must carry an empty JSON object",
        ));
    }
    Ok(())
}

fn required_trimmed_string(
    value: &Value,
    field: &str,
    surface: &str,
) -> Result<String, AuthFailure> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.trim() == *text)
        .map(str::to_string)
        .ok_or_else(|| {
            AuthFailure::protocol(
                "github_auth_response_field_missing",
                format!("{surface} has no non-empty trimmed '{field}'"),
            )
        })
}

fn required_positive_u64(value: &Value, field: &str, surface: &str) -> Result<u64, AuthFailure> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .filter(|number| *number > 0)
        .ok_or_else(|| {
            AuthFailure::protocol(
                "github_auth_response_field_missing",
                format!("{surface} has no positive unsigned '{field}'"),
            )
        })
}

async fn read_bounded(response: reqwest::Response, maximum: usize) -> Result<Vec<u8>, AuthFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(AuthFailure::protocol(
            "github_auth_response_too_large",
            format!("GitHub auth response exceeds descriptor maximum {maximum} bytes"),
        ));
    }
    let bytes = response.bytes().await.map_err(|error| {
        AuthFailure::transport(
            "github_auth_response_read_failed",
            redact_detail(format!("GitHub auth response could not be read: {error}")),
        )
    })?;
    if bytes.len() > maximum {
        return Err(AuthFailure::protocol(
            "github_auth_response_too_large",
            format!("GitHub auth response exceeds descriptor maximum {maximum} bytes"),
        ));
    }
    Ok(bytes.to_vec())
}

fn unix_seconds() -> Result<u64, AuthFailure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| {
            AuthFailure::unavailable(
                "system_clock_invalid",
                format!("system clock is before Unix epoch: {error}"),
            )
        })
}

pub fn api_token_secret_ref(descriptor: &InterfaceDescriptor) -> Result<String, AuthFailure> {
    let value = descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get("github_auth"))
        .and_then(|value| value.get("api_token_secret_ref"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .ok_or_else(|| {
            AuthFailure::protocol(
                "github_api_token_ref_missing",
                format!(
                    "interface '{}' has no GitHub API token secret reference",
                    descriptor.id
                ),
            )
        })?;
    Ok(value.to_string())
}

pub fn auth_headers_from_transport(
    transport: &InterfaceTransportConfig,
) -> Result<HeaderMap, AuthFailure> {
    let mut headers = HeaderMap::new();
    for (name, value) in &transport.headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            AuthFailure::protocol(
                "github_transport_header_invalid",
                format!("GitHub transport header name is invalid: {error}"),
            )
        })?;
        let value = HeaderValue::from_str(value).map_err(|_| {
            AuthFailure::protocol(
                "github_transport_header_invalid",
                "GitHub transport header value is invalid",
            )
        })?;
        headers.insert(name, value);
    }
    Ok(headers)
}

pub fn private_home(root: &Path, interface_id: &str) -> PathBuf {
    root.join("private-home").join(interface_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_personal_access_token_fails_closed() {
        let error = validate_raw_token("ghp_example").expect_err("classic PAT must fail");
        assert_eq!(error.code, "github_classic_pat_unsupported");
    }

    #[tokio::test]
    async fn device_start_cannot_send_without_dispatch_admission() {
        let descriptor_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../world_provider_github_copilot/interface.json");
        let descriptor: InterfaceDescriptor = serde_json::from_slice(
            &std::fs::read(descriptor_path).expect("GitHub Copilot descriptor is readable"),
        )
        .expect("GitHub Copilot descriptor is typed");
        let secrets_home = std::env::temp_dir().join(format!(
            "octopus-github-auth-dispatch-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&secrets_home);
        let manager = GitHubCredentialManager::from_descriptor(&descriptor, &secrets_home)
            .expect("GitHub credential manager resolves");
        let error = manager
            .handle_artifact(DEVICE_START_LABEL, b"{}")
            .await
            .expect_err("device start cannot send without dispatch admission");
        assert_eq!(error.code, "github_auth_dispatch_required");
        let _ = std::fs::remove_dir_all(secrets_home);
    }
}
