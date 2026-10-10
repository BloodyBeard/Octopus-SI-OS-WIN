use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use octopus_interfaces::InterfaceDescriptor;
use octopus_secrets::{
    redact_secrets, LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind,
    SecretsManager,
};
use rand::rngs::OsRng;
use rand::TryRngCore;
use reqwest::{Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::dispatch::OAuthDispatchAdmission;

pub const LOGIN_START_LABEL: &str = "provider.auth.login.start";
pub const LOGIN_COMPLETE_LABEL: &str = "provider.auth.login.complete";
pub const LOGIN_POLL_LABEL: &str = "provider.auth.login.poll";
pub const STATUS_REQUEST_LABEL: &str = "provider.auth.status.request";
pub const LOGOUT_LABEL: &str = "provider.auth.logout";
pub const LOGIN_ACTION_LABEL: &str = "provider.auth.login.action";
pub const STATUS_LABEL: &str = "provider.auth.status";
pub const LOGOUT_COMPLETED_LABEL: &str = "provider.auth.logout.completed";

const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthConfig {
    schema_version: u32,
    credentials_secret_ref: String,
    access_token_cache_secret_ref: String,
    login_state_secret_ref: String,
    refresh_journal_secret_ref: String,
    refresh_skew_seconds: u64,
    login_state_ttl_seconds: u64,
    request_timeout_ms: u64,
    max_response_bytes: usize,
    flow: OAuthFlowConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OAuthFlowConfig {
    #[serde(rename = "anthropic_manual_pkce")]
    AnthropicManual {
        authorization_endpoint: String,
        token_endpoint: String,
        client_id: String,
        redirect_uri: String,
        scope: String,
        user_agent: String,
    },
    #[serde(rename = "xai_oidc_pkce")]
    XaiOidc {
        discovery_endpoint: String,
        client_id: String,
        redirect_uri: String,
        scope: String,
        authorization_parameters: BTreeMap<String, String>,
    },
    #[serde(rename = "minimax_user_code_pkce")]
    MinimaxUserCode {
        authorization_endpoint: String,
        token_endpoint: String,
        client_id: String,
        scope: String,
        grant_type: String,
    },
}

#[derive(Clone, Debug)]
struct SecretHandle {
    scope: SecretScope,
    name: SecretName,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OAuthCredentials {
    pub schema_version: u32,
    pub provider_interface: String,
    pub flow: String,
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_at_unix_ms: u64,
    pub scope: Option<String>,
    pub id_token: Option<String>,
    pub resource_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum RefreshJournal {
    Prepared {
        schema_version: u32,
        provider_interface: String,
        source_fingerprint: String,
    },
    Received {
        schema_version: u32,
        provider_interface: String,
        source_fingerprint: String,
        credentials: Box<OAuthCredentials>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum LoginState {
    #[serde(rename = "anthropic_manual_pkce")]
    AnthropicManual {
        schema_version: u32,
        provider_interface: String,
        code_verifier: String,
        state: String,
        expires_at_unix_ms: u64,
    },
    #[serde(rename = "xai_oidc_pkce")]
    XaiOidc {
        schema_version: u32,
        provider_interface: String,
        authorization_endpoint: String,
        token_endpoint: String,
        redirect_uri: String,
        code_verifier: String,
        code_challenge: String,
        state: String,
        nonce: String,
        expires_at_unix_ms: u64,
    },
    #[serde(rename = "minimax_user_code_pkce")]
    MinimaxUserCode {
        schema_version: u32,
        provider_interface: String,
        user_code: String,
        code_verifier: String,
        state: String,
        expires_at_unix_ms: u64,
        poll_after_ms: u64,
    },
}

#[derive(Clone, Debug)]
pub struct OAuthArtifact {
    pub label: &'static str,
    pub payload: Value,
}

#[derive(Clone, Debug)]
pub struct OAuthFailure {
    pub class: &'static str,
    pub code: &'static str,
    pub detail: String,
    pub retryable: bool,
    pub status: Option<u16>,
    pub evidence: Option<Value>,
}

impl OAuthFailure {
    fn protocol(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "protocol",
            code,
            detail: redact_secrets(detail.into()),
            retryable: false,
            status: None,
            evidence: None,
        }
    }

    fn unavailable(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "failed_closed",
            code,
            detail: redact_secrets(detail.into()),
            retryable: false,
            status: None,
            evidence: None,
        }
    }

    fn transport(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "transport",
            code,
            detail: redact_secrets(detail.into()),
            retryable: true,
            status: None,
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
            code,
            detail: redact_secrets(detail.into()),
            retryable,
            status: Some(status),
            evidence: Some(json!({
                "status": status,
                "response_bytes": bytes.len(),
                "response_blake3": blake3::hash(bytes).to_hex().to_string()
            })),
        }
    }
}

impl std::fmt::Display for OAuthFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for OAuthFailure {}

fn credential_fingerprint(credentials: &OAuthCredentials) -> Result<String, OAuthFailure> {
    let encoded = serde_json::to_vec(credentials).map_err(|error| {
        OAuthFailure::protocol(
            "oauth_credentials_fingerprint_failed",
            format!("OAuth credentials could not be fingerprinted: {error}"),
        )
    })?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
}

fn validate_refresh_identity(
    schema_version: u32,
    provider_interface: &str,
    source_fingerprint: &str,
    expected_interface: &str,
) -> Result<(), OAuthFailure> {
    if schema_version != SCHEMA_VERSION
        || provider_interface != expected_interface
        || source_fingerprint.len() != 64
        || !source_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(OAuthFailure::protocol(
            "oauth_refresh_journal_identity_invalid",
            "OAuth refresh journal identity does not match the exact interface credential",
        ));
    }
    Ok(())
}

fn required_dispatch<'a>(
    dispatch: Option<&'a OAuthDispatchAdmission<'a>>,
) -> Result<&'a OAuthDispatchAdmission<'a>, OAuthFailure> {
    dispatch.ok_or_else(|| {
        OAuthFailure::protocol(
            "oauth_dispatch_required",
            "OAuth network mutation requires an admitted provider.transport.dispatch artifact",
        )
    })
}

#[derive(Clone)]
pub struct OAuthCredentialManager {
    interface_id: String,
    config: OAuthConfig,
    credentials: SecretHandle,
    access_token_cache: SecretHandle,
    login_state: SecretHandle,
    refresh_journal: SecretHandle,
    secrets: SecretsManager,
    client: reqwest::Client,
    lock: Arc<Mutex<()>>,
}

impl OAuthCredentialManager {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: &Path,
    ) -> Result<Self, OAuthFailure> {
        let driver_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
            OAuthFailure::protocol(
                "oauth_config_missing",
                "OAuth interface has no driver config",
            )
        })?;
        let raw = driver_config
            .private_config
            .get("oauth")
            .cloned()
            .ok_or_else(|| {
                OAuthFailure::protocol(
                    "oauth_config_missing",
                    "OAuth interface has no private_config.oauth",
                )
            })?;
        let config: OAuthConfig = serde_json::from_value(raw).map_err(|error| {
            OAuthFailure::protocol(
                "oauth_config_invalid",
                format!("OAuth interface config is invalid: {error}"),
            )
        })?;
        validate_config(&config)?;
        let interface_id = descriptor.id.as_str().to_string();
        let credentials = parse_secret_ref(&config.credentials_secret_ref)?;
        let access_token_cache = parse_secret_ref(&config.access_token_cache_secret_ref)?;
        let login_state = parse_secret_ref(&config.login_state_secret_ref)?;
        let refresh_journal = parse_secret_ref(&config.refresh_journal_secret_ref)?;
        for handle in [
            &credentials,
            &access_token_cache,
            &login_state,
            &refresh_journal,
        ] {
            if handle.scope.name() != Some(interface_id.as_str()) {
                return Err(OAuthFailure::protocol(
                    "oauth_secret_scope_mismatch",
                    "OAuth secret handles must use the exact interface scope",
                ));
            }
        }
        let names = [
            credentials.name.as_str(),
            access_token_cache.name.as_str(),
            login_state.name.as_str(),
            refresh_journal.name.as_str(),
        ];
        if names
            .iter()
            .enumerate()
            .any(|(index, name)| names[index + 1..].contains(name))
        {
            return Err(OAuthFailure::protocol(
                "oauth_secret_handles_collide",
                "OAuth credential, access-token cache, login-state and refresh-journal handles must differ",
            ));
        }
        let transport = driver_config.transport.as_ref().ok_or_else(|| {
            OAuthFailure::protocol(
                "oauth_transport_missing",
                "OAuth interface has no transport",
            )
        })?;
        if transport.auth_secret_ref.as_deref()
            != Some(config.access_token_cache_secret_ref.as_str())
        {
            return Err(OAuthFailure::protocol(
                "oauth_auth_cache_mismatch",
                "OAuth transport auth handle must equal its access-token cache handle",
            ));
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
                OAuthFailure::unavailable(
                    "oauth_client_build_failed",
                    format!("OAuth HTTP client could not be built: {error}"),
                )
            })?;
        Ok(Self {
            interface_id,
            config,
            credentials,
            access_token_cache,
            login_state,
            refresh_journal,
            secrets,
            client,
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn handles_label(label: &str) -> bool {
        matches!(
            label,
            LOGIN_START_LABEL
                | LOGIN_COMPLETE_LABEL
                | LOGIN_POLL_LABEL
                | STATUS_REQUEST_LABEL
                | LOGOUT_LABEL
        )
    }

    pub async fn handle_artifact(
        &self,
        label: &str,
        payload: &[u8],
    ) -> Result<OAuthArtifact, OAuthFailure> {
        self.handle_artifact_inner(label, payload, None).await
    }

    pub(crate) async fn handle_artifact_with_dispatch(
        &self,
        label: &str,
        payload: &[u8],
        dispatch: &OAuthDispatchAdmission<'_>,
    ) -> Result<OAuthArtifact, OAuthFailure> {
        self.handle_artifact_inner(label, payload, Some(dispatch))
            .await
    }

    async fn handle_artifact_inner(
        &self,
        label: &str,
        payload: &[u8],
        dispatch: Option<&OAuthDispatchAdmission<'_>>,
    ) -> Result<OAuthArtifact, OAuthFailure> {
        let _guard = self.lock.lock().await;
        match label {
            LOGIN_START_LABEL => {
                require_empty_object(payload)?;
                self.start_login(dispatch).await
            }
            LOGIN_COMPLETE_LABEL => self.complete_login(payload, dispatch).await,
            LOGIN_POLL_LABEL => {
                require_empty_object(payload)?;
                self.poll_login(dispatch).await
            }
            STATUS_REQUEST_LABEL => {
                require_empty_object(payload)?;
                self.status()
            }
            LOGOUT_LABEL => {
                require_empty_object(payload)?;
                self.logout()
            }
            _ => Err(OAuthFailure::protocol(
                "oauth_artifact_unsupported",
                format!("OAuth life does not accept artifact label '{label}'"),
            )),
        }
    }

    pub async fn ensure_access_token(&self, force_refresh: bool) -> Result<(), OAuthFailure> {
        let _guard = self.lock.lock().await;
        let credentials = self.read_credentials()?.ok_or_else(|| {
            OAuthFailure::unavailable(
                "oauth_credentials_missing",
                format!(
                    "OAuth credentials are not configured for exact interface '{}'; send {}",
                    self.interface_id, LOGIN_START_LABEL
                ),
            )
        })?;
        let mut credentials = self.recover_refresh_journal(credentials)?;
        let threshold =
            unix_ms()?.saturating_add(self.config.refresh_skew_seconds.saturating_mul(1000));
        if force_refresh || credentials.expires_at_unix_ms <= threshold {
            match self.refresh(&credentials).await {
                Ok(refreshed) => credentials = refreshed,
                Err(error) => {
                    if terminal_refresh_failure(&error) {
                        self.delete_secret(&self.credentials)?;
                        self.delete_secret(&self.access_token_cache)?;
                        self.delete_secret(&self.refresh_journal)?;
                    }
                    return Err(error);
                }
            }
        }
        self.write_secret(&self.access_token_cache, &credentials.access_token)
    }

    pub fn credential_handle_fingerprint(&self) -> String {
        blake3::hash(self.config.credentials_secret_ref.as_bytes())
            .to_hex()
            .to_string()
    }

    pub fn interface_id(&self) -> &str {
        &self.interface_id
    }

    pub fn invalidate(&self) -> Result<(), OAuthFailure> {
        self.delete_secret(&self.credentials)?;
        self.delete_secret(&self.access_token_cache)?;
        self.delete_secret(&self.refresh_journal)
    }

    fn status(&self) -> Result<OAuthArtifact, OAuthFailure> {
        let credentials = match self.read_credentials()? {
            Some(credentials) => Some(self.recover_refresh_journal(credentials)?),
            None if self.read_refresh_journal()?.is_some() => {
                return Err(OAuthFailure::unavailable(
                    "oauth_refresh_journal_without_credentials",
                    "OAuth refresh journal exists without its exact interface credential; complete login again",
                ));
            }
            None => None,
        };
        let pending = self.read_login_state()?.is_some();
        let now = unix_ms()?;
        let authenticated = credentials
            .as_ref()
            .is_some_and(|value| value.expires_at_unix_ms > now);
        Ok(OAuthArtifact {
            label: STATUS_LABEL,
            payload: json!({
                "label": STATUS_LABEL,
                "provider_interface": self.interface_id,
                "authenticated": authenticated,
                "credential_state": if authenticated { "secret_backed" } else { "absent_or_expired" },
                "expires_at_unix_ms": credentials.as_ref().map(|value| value.expires_at_unix_ms),
                "login_pending": pending,
                "login_start_artifact": {"label": LOGIN_START_LABEL, "payload": {}},
                "logout_artifact": {"label": LOGOUT_LABEL, "payload": {}},
                "browser_auto_opened": false
            }),
        })
    }

    fn logout(&self) -> Result<OAuthArtifact, OAuthFailure> {
        self.delete_secret(&self.credentials)?;
        self.delete_secret(&self.access_token_cache)?;
        self.delete_secret(&self.login_state)?;
        self.delete_secret(&self.refresh_journal)?;
        Ok(OAuthArtifact {
            label: LOGOUT_COMPLETED_LABEL,
            payload: json!({
                "label": LOGOUT_COMPLETED_LABEL,
                "provider_interface": self.interface_id,
                "authenticated": false,
                "credential_state": "deleted",
                "login_start_artifact": {"label": LOGIN_START_LABEL, "payload": {}},
                "browser_auto_opened": false
            }),
        })
    }

    async fn start_login(
        &self,
        dispatch: Option<&OAuthDispatchAdmission<'_>>,
    ) -> Result<OAuthArtifact, OAuthFailure> {
        let (verifier, challenge) = pkce_pair()?;
        let state = random_urlsafe(32)?;
        let expires_at_unix_ms = checked_expiry(unix_ms()?, self.config.login_state_ttl_seconds)?;
        match &self.config.flow {
            OAuthFlowConfig::AnthropicManual {
                authorization_endpoint,
                client_id,
                redirect_uri,
                scope,
                ..
            } => {
                let mut url =
                    exact_url(authorization_endpoint, "Anthropic authorization endpoint")?;
                url.query_pairs_mut()
                    .append_pair("code", "true")
                    .append_pair("client_id", client_id)
                    .append_pair("response_type", "code")
                    .append_pair("redirect_uri", redirect_uri)
                    .append_pair("scope", scope)
                    .append_pair("code_challenge", &challenge)
                    .append_pair("code_challenge_method", "S256")
                    .append_pair("state", &state);
                self.write_login_state(&LoginState::AnthropicManual {
                    schema_version: SCHEMA_VERSION,
                    provider_interface: self.interface_id.clone(),
                    code_verifier: verifier,
                    state,
                    expires_at_unix_ms,
                })?;
                Ok(login_action(
                    &self.interface_id,
                    "manual_authorization_code",
                    url.as_str(),
                    None,
                    expires_at_unix_ms,
                    json!({
                        "label": LOGIN_COMPLETE_LABEL,
                        "payload": {"authorization_response": "<code>#<state>"}
                    }),
                    None,
                ))
            }
            OAuthFlowConfig::XaiOidc {
                discovery_endpoint,
                client_id,
                redirect_uri,
                scope,
                authorization_parameters,
            } => {
                let discovery = self.discover_xai(discovery_endpoint).await?;
                let authorization_endpoint =
                    required_string(&discovery, "authorization_endpoint", "xAI OIDC discovery")?;
                let token_endpoint =
                    required_string(&discovery, "token_endpoint", "xAI OIDC discovery")?;
                validate_xai_endpoint(&authorization_endpoint, "authorization endpoint")?;
                validate_xai_endpoint(&token_endpoint, "token endpoint")?;
                let nonce = random_urlsafe(32)?;
                let mut url = exact_url(&authorization_endpoint, "xAI authorization endpoint")?;
                {
                    let mut query = url.query_pairs_mut();
                    query
                        .append_pair("response_type", "code")
                        .append_pair("client_id", client_id)
                        .append_pair("redirect_uri", redirect_uri)
                        .append_pair("scope", scope)
                        .append_pair("code_challenge", &challenge)
                        .append_pair("code_challenge_method", "S256")
                        .append_pair("state", &state)
                        .append_pair("nonce", &nonce);
                    for (name, value) in authorization_parameters {
                        query.append_pair(name, value);
                    }
                }
                self.write_login_state(&LoginState::XaiOidc {
                    schema_version: SCHEMA_VERSION,
                    provider_interface: self.interface_id.clone(),
                    authorization_endpoint,
                    token_endpoint,
                    redirect_uri: redirect_uri.clone(),
                    code_verifier: verifier,
                    code_challenge: challenge,
                    state,
                    nonce,
                    expires_at_unix_ms,
                })?;
                Ok(login_action(
                    &self.interface_id,
                    "manual_loopback_callback",
                    url.as_str(),
                    None,
                    expires_at_unix_ms,
                    json!({
                        "label": LOGIN_COMPLETE_LABEL,
                        "payload": {"callback_url": "<full loopback callback URL>"}
                    }),
                    None,
                ))
            }
            OAuthFlowConfig::MinimaxUserCode {
                authorization_endpoint,
                client_id,
                scope,
                ..
            } => {
                let request_id = random_urlsafe(18)?;
                let request = self
                    .client
                    .post(authorization_endpoint)
                    .header("Accept", "application/json")
                    .header("x-request-id", request_id)
                    .form(&[
                        ("response_type", "code"),
                        ("client_id", client_id.as_str()),
                        ("scope", scope.as_str()),
                        ("code_challenge", challenge.as_str()),
                        ("code_challenge_method", "S256"),
                        ("state", state.as_str()),
                    ])
                    .build()
                    .map_err(|error| {
                        OAuthFailure::protocol(
                            "minimax_oauth_start_request_invalid",
                            format!("MiniMax OAuth authorization request is invalid: {error}"),
                        )
                    })?;
                required_dispatch(dispatch)?
                    .admit_request(&request)
                    .await
                    .map_err(|error| {
                        OAuthFailure::protocol(
                            "oauth_dispatch_admission_failed",
                            format!(
                                "MiniMax OAuth authorization dispatch was not admitted: {error}"
                            ),
                        )
                    })?;
                let response = self.client.execute(request).await.map_err(|error| {
                    OAuthFailure::transport(
                        "minimax_oauth_start_send_failed",
                        format!("MiniMax OAuth authorization could not be sent: {error}"),
                    )
                })?;
                let (status, bytes) = self.read_response(response).await?;
                if !(200..300).contains(&status) {
                    return Err(OAuthFailure::endpoint(
                        "minimax_oauth_start_rejected",
                        retryable_status(status),
                        format!("MiniMax OAuth authorization returned HTTP {status}"),
                        status,
                        &bytes,
                    ));
                }
                let value = parse_json_object(&bytes, "MiniMax OAuth authorization")?;
                if required_string(&value, "state", "MiniMax OAuth authorization")? != state {
                    return Err(OAuthFailure::protocol(
                        "minimax_oauth_state_mismatch",
                        "MiniMax OAuth authorization returned a mismatched state",
                    ));
                }
                let user_code =
                    required_string(&value, "user_code", "MiniMax OAuth authorization")?;
                let verification_uri =
                    required_string(&value, "verification_uri", "MiniMax OAuth authorization")?;
                validate_minimax_verification_url(&verification_uri)?;
                let raw_expiry =
                    required_positive_u64(&value, "expired_in", "MiniMax OAuth authorization")?;
                let resolved_expiry = minimax_expiry(raw_expiry, unix_ms()?)?;
                let interval_ms = value
                    .get("interval")
                    .and_then(Value::as_u64)
                    .unwrap_or(2000)
                    .max(2000);
                self.write_login_state(&LoginState::MinimaxUserCode {
                    schema_version: SCHEMA_VERSION,
                    provider_interface: self.interface_id.clone(),
                    user_code: user_code.clone(),
                    code_verifier: verifier,
                    state,
                    expires_at_unix_ms: resolved_expiry.min(expires_at_unix_ms),
                    poll_after_ms: interval_ms,
                })?;
                Ok(login_action(
                    &self.interface_id,
                    "user_code_poll",
                    &verification_uri,
                    Some(&user_code),
                    resolved_expiry.min(expires_at_unix_ms),
                    json!({"label": LOGIN_POLL_LABEL, "payload": {}}),
                    Some(interval_ms),
                ))
            }
        }
    }

    async fn complete_login(
        &self,
        payload: &[u8],
        dispatch: Option<&OAuthDispatchAdmission<'_>>,
    ) -> Result<OAuthArtifact, OAuthFailure> {
        let state = self.read_required_login_state()?;
        ensure_login_state_live(&state, &self.interface_id)?;
        match (&self.config.flow, state) {
            (
                OAuthFlowConfig::AnthropicManual {
                    token_endpoint,
                    client_id,
                    redirect_uri,
                    scope,
                    user_agent,
                    ..
                },
                LoginState::AnthropicManual {
                    code_verifier,
                    state,
                    ..
                },
            ) => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Completion {
                    authorization_response: String,
                }
                let completion: Completion = parse_payload(payload, "Anthropic OAuth completion")?;
                let (code, received_state) = completion
                    .authorization_response
                    .split_once('#')
                    .ok_or_else(|| {
                        OAuthFailure::protocol(
                            "anthropic_oauth_completion_invalid",
                            "Anthropic OAuth completion must contain code#state",
                        )
                    })?;
                if code.is_empty() || received_state != state {
                    return Err(OAuthFailure::protocol(
                        "anthropic_oauth_state_mismatch",
                        "Anthropic OAuth completion carries an empty code or mismatched state",
                    ));
                }
                let request = self
                    .client
                    .post(token_endpoint)
                    .header("Accept", "application/json")
                    .header("User-Agent", user_agent)
                    .json(&json!({
                        "grant_type": "authorization_code",
                        "client_id": client_id,
                        "code": code,
                        "state": received_state,
                        "redirect_uri": redirect_uri,
                        "code_verifier": code_verifier
                    }))
                    .build()
                    .map_err(|error| {
                        OAuthFailure::protocol(
                            "anthropic_oauth_exchange_request_invalid",
                            format!("Anthropic OAuth token exchange request is invalid: {error}"),
                        )
                    })?;
                required_dispatch(dispatch)?
                    .admit_request(&request)
                    .await
                    .map_err(|error| {
                        OAuthFailure::protocol(
                            "oauth_dispatch_admission_failed",
                            format!(
                                "Anthropic OAuth token exchange dispatch was not admitted: {error}"
                            ),
                        )
                    })?;
                let response = self.client.execute(request).await.map_err(|error| {
                    OAuthFailure::transport(
                        "anthropic_oauth_exchange_send_failed",
                        format!("Anthropic OAuth token exchange could not be sent: {error}"),
                    )
                })?;
                let credentials = self
                    .credentials_from_standard_response(
                        response,
                        "anthropic_manual_pkce",
                        Some(scope.clone()),
                        None,
                    )
                    .await?;
                self.finish_login(credentials)
            }
            (
                OAuthFlowConfig::XaiOidc {
                    client_id, scope, ..
                },
                LoginState::XaiOidc {
                    token_endpoint,
                    redirect_uri,
                    code_verifier,
                    code_challenge,
                    state,
                    ..
                },
            ) => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Completion {
                    callback_url: String,
                }
                let completion: Completion = parse_payload(payload, "xAI OAuth completion")?;
                let callback = Url::parse(&completion.callback_url).map_err(|error| {
                    OAuthFailure::protocol(
                        "xai_oauth_callback_invalid",
                        format!("xAI OAuth callback URL is invalid: {error}"),
                    )
                })?;
                if callback.scheme() != "http"
                    || callback.host_str() != Some("127.0.0.1")
                    || callback.port() != Some(56121)
                    || callback.path() != "/callback"
                    || !callback.username().is_empty()
                    || callback.password().is_some()
                    || callback.fragment().is_some()
                    || callback.as_str().split('?').next() != Some(redirect_uri.as_str())
                {
                    return Err(OAuthFailure::protocol(
                        "xai_oauth_callback_invalid",
                        "xAI OAuth callback must use the exact loopback redirect URI",
                    ));
                }
                let query = callback.query_pairs().collect::<BTreeMap<_, _>>();
                let code = query
                    .get("code")
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        OAuthFailure::protocol(
                            "xai_oauth_code_missing",
                            "xAI OAuth callback carries no authorization code",
                        )
                    })?;
                if query.get("state").map(|value| value.as_ref()) != Some(state.as_str()) {
                    return Err(OAuthFailure::protocol(
                        "xai_oauth_state_mismatch",
                        "xAI OAuth callback carries a mismatched state",
                    ));
                }
                let request = self
                    .client
                    .post(&token_endpoint)
                    .header("Accept", "application/json")
                    .form(&[
                        ("grant_type", "authorization_code"),
                        ("code", code.as_ref()),
                        ("redirect_uri", redirect_uri.as_str()),
                        ("client_id", client_id.as_str()),
                        ("code_verifier", code_verifier.as_str()),
                        ("code_challenge", code_challenge.as_str()),
                        ("code_challenge_method", "S256"),
                    ])
                    .build()
                    .map_err(|error| {
                        OAuthFailure::protocol(
                            "xai_oauth_exchange_request_invalid",
                            format!("xAI OAuth token exchange request is invalid: {error}"),
                        )
                    })?;
                required_dispatch(dispatch)?
                    .admit_request(&request)
                    .await
                    .map_err(|error| {
                        OAuthFailure::protocol(
                            "oauth_dispatch_admission_failed",
                            format!("xAI OAuth token exchange dispatch was not admitted: {error}"),
                        )
                    })?;
                let response = self.client.execute(request).await.map_err(|error| {
                    OAuthFailure::transport(
                        "xai_oauth_exchange_send_failed",
                        format!("xAI OAuth token exchange could not be sent: {error}"),
                    )
                })?;
                let credentials = self
                    .credentials_from_standard_response(
                        response,
                        "xai_oidc_pkce",
                        Some(scope.clone()),
                        None,
                    )
                    .await?;
                self.finish_login(credentials)
            }
            (OAuthFlowConfig::MinimaxUserCode { .. }, _) => Err(OAuthFailure::protocol(
                "minimax_oauth_completion_unsupported",
                format!("MiniMax OAuth completes through {LOGIN_POLL_LABEL}"),
            )),
            _ => Err(OAuthFailure::protocol(
                "oauth_login_state_flow_mismatch",
                "OAuth login state does not match the exact interface flow",
            )),
        }
    }

    async fn poll_login(
        &self,
        dispatch: Option<&OAuthDispatchAdmission<'_>>,
    ) -> Result<OAuthArtifact, OAuthFailure> {
        let state = self.read_required_login_state()?;
        ensure_login_state_live(&state, &self.interface_id)?;
        let (
            OAuthFlowConfig::MinimaxUserCode {
                token_endpoint,
                client_id,
                scope,
                grant_type,
                ..
            },
            LoginState::MinimaxUserCode {
                user_code,
                code_verifier,
                poll_after_ms,
                ..
            },
        ) = (&self.config.flow, state)
        else {
            return Err(OAuthFailure::protocol(
                "oauth_poll_unsupported",
                "This OAuth interface does not use user-code polling",
            ));
        };
        let request = self
            .client
            .post(token_endpoint)
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", grant_type.as_str()),
                ("client_id", client_id.as_str()),
                ("user_code", user_code.as_str()),
                ("code_verifier", code_verifier.as_str()),
            ])
            .build()
            .map_err(|error| {
                OAuthFailure::protocol(
                    "minimax_oauth_poll_request_invalid",
                    format!("MiniMax OAuth poll request is invalid: {error}"),
                )
            })?;
        required_dispatch(dispatch)?
            .admit_request(&request)
            .await
            .map_err(|error| {
                OAuthFailure::protocol(
                    "oauth_dispatch_admission_failed",
                    format!("MiniMax OAuth poll dispatch was not admitted: {error}"),
                )
            })?;
        let response = self.client.execute(request).await.map_err(|error| {
            OAuthFailure::transport(
                "minimax_oauth_poll_send_failed",
                format!("MiniMax OAuth poll could not be sent: {error}"),
            )
        })?;
        let (status, bytes) = self.read_response(response).await?;
        if !(200..300).contains(&status) {
            return Err(OAuthFailure::endpoint(
                "minimax_oauth_poll_rejected",
                retryable_status(status),
                format!("MiniMax OAuth token poll returned HTTP {status}"),
                status,
                &bytes,
            ));
        }
        let value = parse_json_object(&bytes, "MiniMax OAuth token poll")?;
        match value.get("status").and_then(Value::as_str) {
            Some("pending") | None => Ok(OAuthArtifact {
                label: STATUS_LABEL,
                payload: json!({
                    "label": STATUS_LABEL,
                    "provider_interface": self.interface_id,
                    "authenticated": false,
                    "status": "authorization_pending",
                    "poll_after_ms": poll_after_ms,
                    "next_artifact": {"label": LOGIN_POLL_LABEL, "payload": {}},
                    "browser_auto_opened": false
                }),
            }),
            Some("error") => {
                self.delete_secret(&self.login_state)?;
                Err(OAuthFailure::endpoint(
                    "minimax_oauth_authorization_denied",
                    false,
                    "MiniMax OAuth authorization was denied",
                    status,
                    &bytes,
                ))
            }
            Some("success") => {
                let access_token =
                    required_string(&value, "access_token", "MiniMax OAuth token poll")?;
                let refresh_token =
                    required_string(&value, "refresh_token", "MiniMax OAuth token poll")?;
                let raw_expiry =
                    required_positive_u64(&value, "expired_in", "MiniMax OAuth token poll")?;
                let credentials = OAuthCredentials {
                    schema_version: SCHEMA_VERSION,
                    provider_interface: self.interface_id.clone(),
                    flow: "minimax_user_code_pkce".to_string(),
                    access_token,
                    refresh_token,
                    token_type: optional_string(&value, "token_type", "MiniMax OAuth token poll")?
                        .unwrap_or_else(|| "Bearer".to_string()),
                    expires_at_unix_ms: minimax_expiry(raw_expiry, unix_ms()?)?,
                    scope: Some(scope.clone()),
                    id_token: None,
                    resource_url: optional_string(
                        &value,
                        "resource_url",
                        "MiniMax OAuth token poll",
                    )?,
                };
                self.finish_login(credentials)
            }
            Some(other) => Err(OAuthFailure::protocol(
                "minimax_oauth_poll_status_invalid",
                format!("MiniMax OAuth poll returned unsupported status '{other}'"),
            )),
        }
    }

    async fn refresh(&self, prior: &OAuthCredentials) -> Result<OAuthCredentials, OAuthFailure> {
        match &self.config.flow {
            OAuthFlowConfig::AnthropicManual {
                token_endpoint,
                client_id,
                scope,
                user_agent,
                ..
            } => {
                self.write_refresh_prepared(prior)?;
                let result = async {
                    let response = self
                        .client
                        .post(token_endpoint)
                        .header("Accept", "application/json")
                        .header("User-Agent", user_agent)
                        .form(&[
                            ("grant_type", "refresh_token"),
                            ("refresh_token", prior.refresh_token.as_str()),
                            ("client_id", client_id.as_str()),
                        ])
                        .send()
                        .await
                        .map_err(|error| {
                            OAuthFailure::transport(
                                "anthropic_oauth_refresh_send_failed",
                                format!("Anthropic OAuth refresh could not be sent: {error}"),
                            )
                        })?;
                    self.credentials_from_standard_response(
                        response,
                        "anthropic_manual_pkce",
                        Some(scope.clone()),
                        Some(prior),
                    )
                    .await
                }
                .await;
                self.finish_refresh_attempt(prior, result)
            }
            OAuthFlowConfig::XaiOidc {
                discovery_endpoint,
                client_id,
                scope,
                ..
            } => {
                let discovery = self.discover_xai(discovery_endpoint).await?;
                let token_endpoint =
                    required_string(&discovery, "token_endpoint", "xAI OIDC discovery")?;
                validate_xai_endpoint(&token_endpoint, "token endpoint")?;
                self.write_refresh_prepared(prior)?;
                let result = async {
                    let response = self
                        .client
                        .post(token_endpoint)
                        .header("Accept", "application/json")
                        .form(&[
                            ("grant_type", "refresh_token"),
                            ("client_id", client_id.as_str()),
                            ("refresh_token", prior.refresh_token.as_str()),
                        ])
                        .send()
                        .await
                        .map_err(|error| {
                            OAuthFailure::transport(
                                "xai_oauth_refresh_send_failed",
                                format!("xAI OAuth refresh could not be sent: {error}"),
                            )
                        })?;
                    self.credentials_from_standard_response(
                        response,
                        "xai_oidc_pkce",
                        Some(scope.clone()),
                        Some(prior),
                    )
                    .await
                }
                .await;
                self.finish_refresh_attempt(prior, result)
            }
            OAuthFlowConfig::MinimaxUserCode {
                token_endpoint,
                client_id,
                scope,
                ..
            } => {
                self.write_refresh_prepared(prior)?;
                let result = async {
                    let response = self
                        .client
                        .post(token_endpoint)
                        .header("Accept", "application/json")
                        .form(&[
                            ("grant_type", "refresh_token"),
                            ("client_id", client_id.as_str()),
                            ("refresh_token", prior.refresh_token.as_str()),
                        ])
                        .send()
                        .await
                        .map_err(|error| {
                            OAuthFailure::transport(
                                "minimax_oauth_refresh_send_failed",
                                format!("MiniMax OAuth refresh could not be sent: {error}"),
                            )
                        })?;
                    let (status, bytes) = self.read_response(response).await?;
                    if !(200..300).contains(&status) {
                        return Err(OAuthFailure::endpoint(
                            "minimax_oauth_refresh_rejected",
                            retryable_status(status),
                            format!("MiniMax OAuth refresh returned HTTP {status}"),
                            status,
                            &bytes,
                        ));
                    }
                    let value = parse_json_object(&bytes, "MiniMax OAuth refresh")?;
                    if value.get("status").and_then(Value::as_str) != Some("success") {
                        return Err(OAuthFailure::endpoint(
                            "minimax_oauth_refresh_rejected",
                            false,
                            "MiniMax OAuth refresh did not return success",
                            status,
                            &bytes,
                        ));
                    }
                    let access_token =
                        required_string(&value, "access_token", "MiniMax OAuth refresh")?;
                    let refresh_token =
                        optional_string(&value, "refresh_token", "MiniMax OAuth refresh")?
                            .unwrap_or_else(|| prior.refresh_token.clone());
                    let raw_expiry =
                        required_positive_u64(&value, "expired_in", "MiniMax OAuth refresh")?;
                    let credentials = OAuthCredentials {
                        schema_version: SCHEMA_VERSION,
                        provider_interface: self.interface_id.clone(),
                        flow: "minimax_user_code_pkce".to_string(),
                        access_token,
                        refresh_token,
                        token_type: optional_string(&value, "token_type", "MiniMax OAuth refresh")?
                            .unwrap_or_else(|| prior.token_type.clone()),
                        expires_at_unix_ms: minimax_expiry(raw_expiry, unix_ms()?)?,
                        scope: Some(scope.clone()),
                        id_token: prior.id_token.clone(),
                        resource_url: optional_string(
                            &value,
                            "resource_url",
                            "MiniMax OAuth refresh",
                        )?
                        .or_else(|| prior.resource_url.clone()),
                    };
                    validate_credentials(&credentials, &self.interface_id, &self.config)?;
                    Ok(credentials)
                }
                .await;
                self.finish_refresh_attempt(prior, result)
            }
        }
    }

    async fn credentials_from_standard_response(
        &self,
        response: Response,
        flow: &str,
        scope: Option<String>,
        prior: Option<&OAuthCredentials>,
    ) -> Result<OAuthCredentials, OAuthFailure> {
        let (status, bytes) = self.read_response(response).await?;
        if !(200..300).contains(&status) {
            if status == 403 && matches!(&self.config.flow, OAuthFlowConfig::XaiOidc { .. }) {
                return Err(OAuthFailure::endpoint(
                    "xai_oauth_entitlement_denied",
                    false,
                    "xAI OAuth account is not authorized for API access; the provider plan or entitlement may not include API use",
                    status,
                    &bytes,
                ));
            }
            return Err(OAuthFailure::endpoint(
                "oauth_token_exchange_rejected",
                retryable_status(status),
                format!("OAuth token endpoint returned HTTP {status}"),
                status,
                &bytes,
            ));
        }
        let value = parse_json_object(&bytes, "OAuth token exchange")?;
        let access_token = required_string(&value, "access_token", "OAuth token exchange")?;
        let refresh_token = optional_string(&value, "refresh_token", "OAuth token exchange")?
            .or_else(|| prior.map(|value| value.refresh_token.clone()))
            .ok_or_else(|| {
                OAuthFailure::protocol(
                    "oauth_refresh_token_missing",
                    "OAuth token response carries no refresh_token",
                )
            })?;
        let expires_at_unix_ms = response_expiry(&value, &access_token, unix_ms()?)?;
        let credentials = OAuthCredentials {
            schema_version: SCHEMA_VERSION,
            provider_interface: self.interface_id.clone(),
            flow: flow.to_string(),
            access_token,
            refresh_token,
            token_type: optional_string(&value, "token_type", "OAuth token exchange")?
                .or_else(|| prior.map(|value| value.token_type.clone()))
                .unwrap_or_else(|| "Bearer".to_string()),
            expires_at_unix_ms,
            scope,
            id_token: optional_string(&value, "id_token", "OAuth token exchange")?
                .or_else(|| prior.and_then(|value| value.id_token.clone())),
            resource_url: optional_string(&value, "resource_url", "OAuth token exchange")?
                .or_else(|| prior.and_then(|value| value.resource_url.clone())),
        };
        validate_credentials(&credentials, &self.interface_id, &self.config)?;
        Ok(credentials)
    }

    fn finish_login(&self, credentials: OAuthCredentials) -> Result<OAuthArtifact, OAuthFailure> {
        validate_credentials(&credentials, &self.interface_id, &self.config)?;
        self.write_credentials(&credentials)?;
        self.write_secret(&self.access_token_cache, &credentials.access_token)?;
        self.delete_secret(&self.login_state)?;
        self.delete_secret(&self.refresh_journal)?;
        Ok(OAuthArtifact {
            label: STATUS_LABEL,
            payload: json!({
                "label": STATUS_LABEL,
                "provider_interface": self.interface_id,
                "authenticated": true,
                "credential_state": "secret_backed",
                "expires_at_unix_ms": credentials.expires_at_unix_ms,
                "logout_artifact": {"label": LOGOUT_LABEL, "payload": {}},
                "browser_auto_opened": false
            }),
        })
    }

    async fn discover_xai(&self, endpoint: &str) -> Result<Map<String, Value>, OAuthFailure> {
        let response = self
            .client
            .get(endpoint)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|error| {
                OAuthFailure::transport(
                    "xai_oauth_discovery_send_failed",
                    format!("xAI OIDC discovery could not be sent: {error}"),
                )
            })?;
        let (status, bytes) = self.read_response(response).await?;
        if !(200..300).contains(&status) {
            return Err(OAuthFailure::endpoint(
                "xai_oauth_discovery_rejected",
                retryable_status(status),
                format!("xAI OIDC discovery returned HTTP {status}"),
                status,
                &bytes,
            ));
        }
        parse_json_object(&bytes, "xAI OIDC discovery")
    }

    async fn read_response(&self, response: Response) -> Result<(u16, Vec<u8>), OAuthFailure> {
        let status = response.status().as_u16();
        let bytes = read_bounded(response, self.config.max_response_bytes).await?;
        Ok((status, bytes))
    }

    fn finish_refresh_attempt(
        &self,
        prior: &OAuthCredentials,
        result: Result<OAuthCredentials, OAuthFailure>,
    ) -> Result<OAuthCredentials, OAuthFailure> {
        match result {
            Ok(credentials) => {
                self.write_refresh_received(prior, &credentials)?;
                self.write_credentials(&credentials)?;
                self.delete_secret(&self.refresh_journal)?;
                Ok(credentials)
            }
            Err(error) => {
                if error.status.is_some() {
                    self.delete_secret(&self.refresh_journal)?;
                }
                Err(error)
            }
        }
    }

    fn recover_refresh_journal(
        &self,
        current: OAuthCredentials,
    ) -> Result<OAuthCredentials, OAuthFailure> {
        let Some(journal) = self.read_refresh_journal()? else {
            return Ok(current);
        };
        let current_fingerprint = credential_fingerprint(&current)?;
        match journal {
            RefreshJournal::Prepared {
                schema_version,
                provider_interface,
                source_fingerprint,
            } => {
                validate_refresh_identity(
                    schema_version,
                    &provider_interface,
                    &source_fingerprint,
                    &self.interface_id,
                )?;
                if current_fingerprint != source_fingerprint {
                    self.delete_secret(&self.refresh_journal)?;
                    return Ok(current);
                }
                self.delete_secret(&self.access_token_cache)?;
                Err(OAuthFailure::unavailable(
                    "oauth_refresh_outcome_unknown",
                    "OAuth refresh may have rotated the credential before its response became durable; complete login again for this exact interface",
                ))
            }
            RefreshJournal::Received {
                schema_version,
                provider_interface,
                source_fingerprint,
                credentials,
            } => {
                validate_refresh_identity(
                    schema_version,
                    &provider_interface,
                    &source_fingerprint,
                    &self.interface_id,
                )?;
                validate_credentials(&credentials, &self.interface_id, &self.config)?;
                let recovered = if current_fingerprint == source_fingerprint {
                    self.write_credentials(&credentials)?;
                    *credentials
                } else {
                    current
                };
                self.delete_secret(&self.refresh_journal)?;
                Ok(recovered)
            }
        }
    }

    fn write_refresh_prepared(&self, prior: &OAuthCredentials) -> Result<(), OAuthFailure> {
        if self.read_refresh_journal()?.is_some() {
            return Err(OAuthFailure::unavailable(
                "oauth_refresh_journal_occupied",
                "OAuth refresh journal already contains an unresolved operation",
            ));
        }
        self.write_refresh_journal(&RefreshJournal::Prepared {
            schema_version: SCHEMA_VERSION,
            provider_interface: self.interface_id.clone(),
            source_fingerprint: credential_fingerprint(prior)?,
        })
    }

    fn write_refresh_received(
        &self,
        prior: &OAuthCredentials,
        credentials: &OAuthCredentials,
    ) -> Result<(), OAuthFailure> {
        validate_credentials(credentials, &self.interface_id, &self.config)?;
        let source_fingerprint = credential_fingerprint(prior)?;
        match self.read_refresh_journal()? {
            Some(RefreshJournal::Prepared {
                schema_version,
                provider_interface,
                source_fingerprint: recorded,
            }) => {
                validate_refresh_identity(
                    schema_version,
                    &provider_interface,
                    &recorded,
                    &self.interface_id,
                )?;
                if recorded != source_fingerprint {
                    return Err(OAuthFailure::protocol(
                        "oauth_refresh_journal_source_mismatch",
                        "OAuth refresh journal does not match the credential used for this refresh",
                    ));
                }
            }
            Some(RefreshJournal::Received { .. }) => {
                return Err(OAuthFailure::protocol(
                    "oauth_refresh_journal_phase_invalid",
                    "OAuth refresh journal already contains a received credential",
                ));
            }
            None => {
                return Err(OAuthFailure::protocol(
                    "oauth_refresh_journal_missing",
                    "OAuth refresh response arrived without a prepared private journal record",
                ));
            }
        }
        self.write_refresh_journal(&RefreshJournal::Received {
            schema_version: SCHEMA_VERSION,
            provider_interface: self.interface_id.clone(),
            source_fingerprint,
            credentials: Box::new(credentials.clone()),
        })
    }

    fn read_refresh_journal(&self) -> Result<Option<RefreshJournal>, OAuthFailure> {
        let Some(encoded) = self.read_secret(&self.refresh_journal)? else {
            return Ok(None);
        };
        let journal: RefreshJournal = serde_json::from_str(&encoded).map_err(|error| {
            OAuthFailure::protocol(
                "oauth_refresh_journal_invalid",
                format!("OAuth refresh journal JSON is invalid: {error}"),
            )
        })?;
        Ok(Some(journal))
    }

    fn write_refresh_journal(&self, journal: &RefreshJournal) -> Result<(), OAuthFailure> {
        let encoded = serde_json::to_string(journal).map_err(|error| {
            OAuthFailure::protocol(
                "oauth_refresh_journal_encode_failed",
                format!("OAuth refresh journal could not be encoded: {error}"),
            )
        })?;
        self.write_secret(&self.refresh_journal, &encoded)
    }

    fn read_credentials(&self) -> Result<Option<OAuthCredentials>, OAuthFailure> {
        let Some(encoded) = self.read_secret(&self.credentials)? else {
            return Ok(None);
        };
        let credentials: OAuthCredentials = serde_json::from_str(&encoded).map_err(|error| {
            OAuthFailure::protocol(
                "oauth_credentials_invalid",
                format!("OAuth credential JSON is invalid: {error}"),
            )
        })?;
        validate_credentials(&credentials, &self.interface_id, &self.config)?;
        Ok(Some(credentials))
    }

    fn write_credentials(&self, credentials: &OAuthCredentials) -> Result<(), OAuthFailure> {
        validate_credentials(credentials, &self.interface_id, &self.config)?;
        let encoded = serde_json::to_string(credentials).map_err(|error| {
            OAuthFailure::protocol(
                "oauth_credentials_encode_failed",
                format!("OAuth credential JSON could not be encoded: {error}"),
            )
        })?;
        self.write_secret(&self.credentials, &encoded)
    }

    fn read_login_state(&self) -> Result<Option<LoginState>, OAuthFailure> {
        let Some(encoded) = self.read_secret(&self.login_state)? else {
            return Ok(None);
        };
        let state: LoginState = serde_json::from_str(&encoded).map_err(|error| {
            OAuthFailure::protocol(
                "oauth_login_state_invalid",
                format!("OAuth login-state JSON is invalid: {error}"),
            )
        })?;
        validate_login_state(&state)?;
        Ok(Some(state))
    }

    fn read_required_login_state(&self) -> Result<LoginState, OAuthFailure> {
        self.read_login_state()?.ok_or_else(|| {
            OAuthFailure::unavailable(
                "oauth_login_state_missing",
                format!("OAuth login has not been started with {LOGIN_START_LABEL}"),
            )
        })
    }

    fn write_login_state(&self, state: &LoginState) -> Result<(), OAuthFailure> {
        validate_login_state(state)?;
        let encoded = serde_json::to_string(state).map_err(|error| {
            OAuthFailure::protocol(
                "oauth_login_state_encode_failed",
                format!("OAuth login state could not be encoded: {error}"),
            )
        })?;
        self.write_secret(&self.login_state, &encoded)
    }

    fn read_secret(&self, handle: &SecretHandle) -> Result<Option<String>, OAuthFailure> {
        self.secrets
            .get(&handle.scope, &handle.name)
            .map_err(|error| {
                OAuthFailure::unavailable(
                    "oauth_secret_read_failed",
                    format!(
                        "OAuth secret '{}/{}' could not be read: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }

    fn write_secret(&self, handle: &SecretHandle, value: &str) -> Result<(), OAuthFailure> {
        self.secrets
            .set(&handle.scope, &handle.name, value)
            .map_err(|error| {
                OAuthFailure::unavailable(
                    "oauth_secret_write_failed",
                    format!(
                        "OAuth secret '{}/{}' could not be written: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }

    fn delete_secret(&self, handle: &SecretHandle) -> Result<(), OAuthFailure> {
        self.secrets
            .delete(&handle.scope, &handle.name)
            .map(|_| ())
            .map_err(|error| {
                OAuthFailure::unavailable(
                    "oauth_secret_delete_failed",
                    format!(
                        "OAuth secret '{}/{}' could not be deleted: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }
}

fn login_action(
    interface: &str,
    flow: &str,
    verification_uri: &str,
    user_code: Option<&str>,
    expires_at_unix_ms: u64,
    next_artifact: Value,
    poll_after_ms: Option<u64>,
) -> OAuthArtifact {
    OAuthArtifact {
        label: LOGIN_ACTION_LABEL,
        payload: json!({
            "label": LOGIN_ACTION_LABEL,
            "provider_interface": interface,
            "flow": flow,
            "verification_uri": verification_uri,
            "user_code": user_code,
            "expires_at_unix_ms": expires_at_unix_ms,
            "next_artifact": next_artifact,
            "poll_after_ms": poll_after_ms,
            "browser_auto_opened": false
        }),
    }
}

fn validate_config(config: &OAuthConfig) -> Result<(), OAuthFailure> {
    if config.schema_version != SCHEMA_VERSION
        || config.refresh_skew_seconds == 0
        || config.login_state_ttl_seconds == 0
        || config.request_timeout_ms == 0
        || config.max_response_bytes == 0
    {
        return Err(OAuthFailure::protocol(
            "oauth_config_invalid",
            "OAuth config does not satisfy its versioned limits and identity contract",
        ));
    }
    match &config.flow {
        OAuthFlowConfig::AnthropicManual {
            authorization_endpoint,
            token_endpoint,
            client_id,
            redirect_uri,
            scope,
            user_agent,
        } => {
            exact_endpoint(
                authorization_endpoint,
                "https://claude.ai/oauth/authorize",
                "Anthropic authorization endpoint",
            )?;
            exact_endpoint(
                token_endpoint,
                "https://platform.claude.com/v1/oauth/token",
                "Anthropic token endpoint",
            )?;
            exact_endpoint(
                redirect_uri,
                "https://console.anthropic.com/oauth/code/callback",
                "Anthropic redirect URI",
            )?;
            if client_id != "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
                || scope != "org:create_api_key user:profile user:inference"
                || user_agent.is_empty()
                || user_agent.trim() != user_agent
            {
                return Err(OAuthFailure::protocol(
                    "anthropic_oauth_contract_invalid",
                    "Anthropic OAuth client, scope or user-agent is not the exact declared contract",
                ));
            }
        }
        OAuthFlowConfig::XaiOidc {
            discovery_endpoint,
            client_id,
            redirect_uri,
            scope,
            authorization_parameters,
        } => {
            exact_endpoint(
                discovery_endpoint,
                "https://auth.x.ai/.well-known/openid-configuration",
                "xAI discovery endpoint",
            )?;
            exact_endpoint(
                redirect_uri,
                "http://127.0.0.1:56121/callback",
                "xAI redirect URI",
            )?;
            if client_id != "b1a00492-073a-47ea-816f-4c329264a828"
                || scope != "openid profile email offline_access grok-cli:access api:access"
                || authorization_parameters.get("plan").map(String::as_str) != Some("generic")
                || authorization_parameters.get("referrer").map(String::as_str)
                    != Some("hermes-agent")
                || authorization_parameters.len() != 2
            {
                return Err(OAuthFailure::protocol(
                    "xai_oauth_contract_invalid",
                    "xAI OAuth client, scope or authorization parameters are not exact",
                ));
            }
        }
        OAuthFlowConfig::MinimaxUserCode {
            authorization_endpoint,
            token_endpoint,
            client_id,
            scope,
            grant_type,
        } => {
            exact_endpoint(
                authorization_endpoint,
                "https://api.minimax.io/oauth/code",
                "MiniMax authorization endpoint",
            )?;
            exact_endpoint(
                token_endpoint,
                "https://api.minimax.io/oauth/token",
                "MiniMax token endpoint",
            )?;
            if client_id != "78257093-7e40-4613-99e0-527b14b39113"
                || scope != "group_id profile model.completion"
                || grant_type != "urn:ietf:params:oauth:grant-type:user_code"
            {
                return Err(OAuthFailure::protocol(
                    "minimax_oauth_contract_invalid",
                    "MiniMax OAuth client, scope or grant type is not exact",
                ));
            }
        }
    }
    Ok(())
}

fn validate_credentials(
    credentials: &OAuthCredentials,
    interface_id: &str,
    config: &OAuthConfig,
) -> Result<(), OAuthFailure> {
    let expected_flow = match config.flow {
        OAuthFlowConfig::AnthropicManual { .. } => "anthropic_manual_pkce",
        OAuthFlowConfig::XaiOidc { .. } => "xai_oidc_pkce",
        OAuthFlowConfig::MinimaxUserCode { .. } => "minimax_user_code_pkce",
    };
    if credentials.schema_version != SCHEMA_VERSION
        || credentials.provider_interface != interface_id
        || credentials.flow != expected_flow
        || credentials.token_type != "Bearer"
        || credentials.expires_at_unix_ms == 0
    {
        return Err(OAuthFailure::protocol(
            "oauth_credentials_invalid",
            "OAuth credentials do not match their exact interface, flow, version, token type or expiry",
        ));
    }
    for value in [&credentials.access_token, &credentials.refresh_token] {
        if value.is_empty() || value.trim() != value || value.chars().any(char::is_whitespace) {
            return Err(OAuthFailure::protocol(
                "oauth_credentials_invalid",
                "OAuth access or refresh token is malformed",
            ));
        }
    }
    Ok(())
}

fn validate_login_state(state: &LoginState) -> Result<(), OAuthFailure> {
    let valid = match state {
        LoginState::AnthropicManual {
            schema_version,
            provider_interface,
            code_verifier,
            state,
            expires_at_unix_ms,
        } => {
            *schema_version == SCHEMA_VERSION
                && valid_identifier(provider_interface)
                && valid_secret_component(code_verifier)
                && valid_secret_component(state)
                && *expires_at_unix_ms > 0
        }
        LoginState::XaiOidc {
            schema_version,
            provider_interface,
            authorization_endpoint,
            token_endpoint,
            redirect_uri,
            code_verifier,
            code_challenge,
            state,
            nonce,
            expires_at_unix_ms,
        } => {
            *schema_version == SCHEMA_VERSION
                && valid_identifier(provider_interface)
                && validate_xai_endpoint(authorization_endpoint, "authorization endpoint").is_ok()
                && validate_xai_endpoint(token_endpoint, "token endpoint").is_ok()
                && redirect_uri == "http://127.0.0.1:56121/callback"
                && valid_secret_component(code_verifier)
                && valid_secret_component(code_challenge)
                && valid_secret_component(state)
                && valid_secret_component(nonce)
                && *expires_at_unix_ms > 0
        }
        LoginState::MinimaxUserCode {
            schema_version,
            provider_interface,
            user_code,
            code_verifier,
            state,
            expires_at_unix_ms,
            poll_after_ms,
        } => {
            *schema_version == SCHEMA_VERSION
                && valid_identifier(provider_interface)
                && valid_secret_component(user_code)
                && valid_secret_component(code_verifier)
                && valid_secret_component(state)
                && *expires_at_unix_ms > 0
                && *poll_after_ms >= 2000
        }
    };
    if !valid {
        return Err(OAuthFailure::protocol(
            "oauth_login_state_invalid",
            "OAuth login state does not satisfy its versioned schema",
        ));
    }
    Ok(())
}

fn ensure_login_state_live(state: &LoginState, interface: &str) -> Result<(), OAuthFailure> {
    validate_login_state(state)?;
    let (state_interface, expires_at) = match state {
        LoginState::AnthropicManual {
            provider_interface,
            expires_at_unix_ms,
            ..
        }
        | LoginState::XaiOidc {
            provider_interface,
            expires_at_unix_ms,
            ..
        }
        | LoginState::MinimaxUserCode {
            provider_interface,
            expires_at_unix_ms,
            ..
        } => (provider_interface, expires_at_unix_ms),
    };
    if state_interface != interface || unix_ms()? >= *expires_at {
        return Err(OAuthFailure::unavailable(
            "oauth_login_state_expired",
            "OAuth login state is expired or belongs to another exact interface",
        ));
    }
    Ok(())
}

fn parse_secret_ref(reference: &str) -> Result<SecretHandle, OAuthFailure> {
    let path = reference.strip_prefix("secret://").ok_or_else(|| {
        OAuthFailure::protocol(
            "oauth_secret_ref_invalid",
            "OAuth secret ref must start with secret://",
        )
    })?;
    let (scope, name) = path.split_once('/').ok_or_else(|| {
        OAuthFailure::protocol(
            "oauth_secret_ref_invalid",
            "OAuth secret ref must be secret://<scope>/<name>",
        )
    })?;
    if name.contains('/') {
        return Err(OAuthFailure::protocol(
            "oauth_secret_ref_invalid",
            "OAuth secret ref must name one scoped secret",
        ));
    }
    Ok(SecretHandle {
        scope: SecretScope::named(scope.to_string()).map_err(|error| {
            OAuthFailure::protocol(
                "oauth_secret_ref_invalid",
                format!("OAuth secret scope is invalid: {error}"),
            )
        })?,
        name: SecretName::new(name).map_err(|error| {
            OAuthFailure::protocol(
                "oauth_secret_ref_invalid",
                format!("OAuth secret name is invalid: {error}"),
            )
        })?,
    })
}

fn pkce_pair() -> Result<(String, String), OAuthFailure> {
    let verifier = random_urlsafe(64)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Ok((verifier, challenge))
}

fn random_urlsafe(length: usize) -> Result<String, OAuthFailure> {
    let mut bytes = vec![0_u8; length];
    OsRng.try_fill_bytes(&mut bytes).map_err(|error| {
        OAuthFailure::unavailable(
            "oauth_random_failed",
            format!("OAuth secure randomness is unavailable: {error}"),
        )
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn response_expiry(
    value: &Map<String, Value>,
    access_token: &str,
    now: u64,
) -> Result<u64, OAuthFailure> {
    if let Some(expires_in) = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|v| *v > 0)
    {
        return checked_expiry(now, expires_in);
    }
    let mut parts = access_token.split('.');
    let _header = parts.next();
    let payload = parts.next().ok_or_else(|| {
        OAuthFailure::protocol(
            "oauth_token_expiry_missing",
            "OAuth token response carries neither positive expires_in nor a JWT expiry",
        )
    })?;
    if parts.next().is_none() || parts.next().is_some() {
        return Err(OAuthFailure::protocol(
            "oauth_token_expiry_missing",
            "OAuth access token is not a three-part JWT",
        ));
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).map_err(|_| {
        OAuthFailure::protocol(
            "oauth_token_expiry_invalid",
            "OAuth access token JWT payload is not base64url",
        )
    })?;
    let claims: Map<String, Value> = serde_json::from_slice(&bytes).map_err(|_| {
        OAuthFailure::protocol(
            "oauth_token_expiry_invalid",
            "OAuth access token JWT payload is not a JSON object",
        )
    })?;
    claims
        .get("exp")
        .and_then(Value::as_u64)
        .and_then(|seconds| seconds.checked_mul(1000))
        .filter(|expiry| *expiry > now)
        .ok_or_else(|| {
            OAuthFailure::protocol(
                "oauth_token_expiry_invalid",
                "OAuth access token JWT carries no future unsigned exp",
            )
        })
}

fn minimax_expiry(raw: u64, now: u64) -> Result<u64, OAuthFailure> {
    if raw > now / 2 {
        return Ok(raw);
    }
    checked_expiry(now, raw.max(1))
}

fn checked_expiry(now_ms: u64, seconds: u64) -> Result<u64, OAuthFailure> {
    now_ms
        .checked_add(seconds.checked_mul(1000).ok_or_else(|| {
            OAuthFailure::protocol("oauth_expiry_overflow", "OAuth expiry exceeds u64")
        })?)
        .ok_or_else(|| OAuthFailure::protocol("oauth_expiry_overflow", "OAuth expiry exceeds u64"))
}

fn terminal_refresh_failure(error: &OAuthFailure) -> bool {
    matches!(error.status, Some(400 | 401 | 403))
        || (error.code == "minimax_oauth_refresh_rejected" && error.status == Some(200))
        || error.detail.to_ascii_lowercase().contains("invalid_grant")
        || error
            .detail
            .to_ascii_lowercase()
            .contains("refresh_token_reused")
        || error
            .detail
            .to_ascii_lowercase()
            .contains("invalid_refresh_token")
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504)
}

fn exact_endpoint(raw: &str, expected: &str, surface: &str) -> Result<(), OAuthFailure> {
    let url = exact_url(raw, surface)?;
    if url.as_str() != expected {
        return Err(OAuthFailure::protocol(
            "oauth_endpoint_invalid",
            format!("{surface} must be the exact credential-free endpoint"),
        ));
    }
    Ok(())
}

fn exact_url(raw: &str, surface: &str) -> Result<Url, OAuthFailure> {
    let url = Url::parse(raw).map_err(|error| {
        OAuthFailure::protocol(
            "oauth_endpoint_invalid",
            format!("{surface} is invalid: {error}"),
        )
    })?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(OAuthFailure::protocol(
            "oauth_endpoint_invalid",
            format!("{surface} cannot contain credentials or a fragment"),
        ));
    }
    Ok(url)
}

fn validate_xai_endpoint(raw: &str, surface: &str) -> Result<(), OAuthFailure> {
    let url = exact_url(raw, surface)?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if url.scheme() != "https"
        || (host != "x.ai" && !host.ends_with(".x.ai"))
        || url.query().is_some()
    {
        return Err(OAuthFailure::protocol(
            "xai_oauth_endpoint_invalid",
            format!("xAI {surface} must remain on the credential-free xAI HTTPS origin"),
        ));
    }
    Ok(())
}

fn validate_minimax_verification_url(raw: &str) -> Result<(), OAuthFailure> {
    let url = exact_url(raw, "MiniMax verification URI")?;
    if url.scheme() != "https"
        || url.host_str() != Some("api.minimax.io")
        || url.fragment().is_some()
    {
        return Err(OAuthFailure::protocol(
            "minimax_oauth_verification_uri_invalid",
            "MiniMax verification URI must remain on the credential-free API HTTPS origin",
        ));
    }
    Ok(())
}

fn require_empty_object(payload: &[u8]) -> Result<(), OAuthFailure> {
    let value: Value = serde_json::from_slice(payload).map_err(|error| {
        OAuthFailure::protocol(
            "oauth_artifact_json_invalid",
            format!("OAuth control artifact is not valid JSON: {error}"),
        )
    })?;
    if !value.as_object().is_some_and(Map::is_empty) {
        return Err(OAuthFailure::protocol(
            "oauth_artifact_invalid",
            "OAuth control artifact payload must be an empty JSON object",
        ));
    }
    Ok(())
}

fn parse_payload<T: for<'de> Deserialize<'de>>(
    payload: &[u8],
    surface: &str,
) -> Result<T, OAuthFailure> {
    serde_json::from_slice(payload).map_err(|error| {
        OAuthFailure::protocol(
            "oauth_artifact_invalid",
            format!("{surface} artifact payload is invalid: {error}"),
        )
    })
}

fn parse_json_object(bytes: &[u8], surface: &str) -> Result<Map<String, Value>, OAuthFailure> {
    serde_json::from_slice(bytes).map_err(|error| {
        OAuthFailure::protocol(
            "oauth_response_json_invalid",
            format!("{surface} returned invalid JSON: {error}"),
        )
    })
}

fn required_string(
    value: &Map<String, Value>,
    field: &str,
    surface: &str,
) -> Result<String, OAuthFailure> {
    optional_string(value, field, surface)?.ok_or_else(|| {
        OAuthFailure::protocol(
            "oauth_response_field_missing",
            format!("{surface} returned no {field}"),
        )
    })
}

fn optional_string(
    value: &Map<String, Value>,
    field: &str,
    surface: &str,
) -> Result<Option<String>, OAuthFailure> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() && value.trim() == value => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(OAuthFailure::protocol(
            "oauth_response_field_invalid",
            format!("{surface} returned malformed {field}"),
        )),
    }
}

fn required_positive_u64(
    value: &Map<String, Value>,
    field: &str,
    surface: &str,
) -> Result<u64, OAuthFailure> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            OAuthFailure::protocol(
                "oauth_response_field_invalid",
                format!("{surface} returned no positive unsigned {field}"),
            )
        })
}

async fn read_bounded(response: Response, maximum: usize) -> Result<Vec<u8>, OAuthFailure> {
    let mut bytes = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        OAuthFailure::transport(
            "oauth_response_read_failed",
            format!("OAuth response body could not be read: {error}"),
        )
    })? {
        let next = bytes.len().checked_add(chunk.len()).ok_or_else(|| {
            OAuthFailure::transport(
                "oauth_response_size_overflow",
                "OAuth response size overflowed",
            )
        })?;
        if next > maximum {
            return Err(OAuthFailure::transport(
                "oauth_response_too_large",
                format!("OAuth response exceeds {maximum} bytes"),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.trim() == value
}

fn valid_secret_component(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_whitespace)
}

fn unix_ms() -> Result<u64, OAuthFailure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            OAuthFailure::unavailable(
                "oauth_clock_invalid",
                format!("System clock precedes Unix epoch: {error}"),
            )
        })?
        .as_millis()
        .try_into()
        .map_err(|_| OAuthFailure::unavailable("oauth_clock_overflow", "System clock exceeds u64"))
}
