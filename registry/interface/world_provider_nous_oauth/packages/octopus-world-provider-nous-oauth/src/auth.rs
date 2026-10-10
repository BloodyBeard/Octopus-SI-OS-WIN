use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use octopus_interfaces::InterfaceDescriptor;
use octopus_secrets::{
    redact_secrets, LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind,
    SecretsManager,
};
use octopus_world_provider_openai_compatible::{BodyEvidence, HttpEvidence};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;

pub const DEVICE_START_LABEL: &str = "provider.auth.device.start";
pub const DEVICE_POLL_LABEL: &str = "provider.auth.device.poll";
pub const DEVICE_CODE_LABEL: &str = "provider.auth.device.code";
pub const AUTH_STATUS_LABEL: &str = "provider.auth.status";

const SCHEMA_VERSION: u32 = 1;
const INFERENCE_SCOPE: &str = "inference:invoke";
const INFERENCE_URL: &str = "https://inference-api.nousresearch.com/v1";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NousOAuthConfig {
    schema_version: u32,
    credentials_secret_ref: String,
    invoke_token_cache_secret_ref: String,
    device_state_secret_ref: String,
    refresh_journal_secret_ref: String,
    device_code_endpoint: String,
    token_endpoint: String,
    client_id: String,
    scope: String,
    refresh_skew_seconds: u64,
    poll_interval_cap_seconds: u64,
    slow_down_increment_seconds: u64,
    slow_down_cap_seconds: u64,
    request_timeout_ms: u64,
    max_response_bytes: usize,
}

#[derive(Clone, Debug)]
struct SecretHandle {
    scope: SecretScope,
    name: SecretName,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NousOAuthCredentials {
    pub schema_version: u32,
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub scope: String,
    pub expires_at_unix_ms: u64,
    pub inference_base_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DeviceState {
    schema_version: u32,
    device_code: String,
    expires_at_unix_ms: u64,
    interval_seconds: u64,
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
        credentials: NousOAuthCredentials,
    },
}

#[derive(Clone, Debug)]
pub struct NousAuthArtifact {
    pub label: &'static str,
    pub payload: Value,
}

#[derive(Clone, Debug)]
pub struct NousOAuthHttpEvidence(Arc<Option<HttpEvidence>>);

impl NousOAuthHttpEvidence {
    fn empty() -> Self {
        Self(Arc::new(None))
    }

    fn from_evidence(evidence: HttpEvidence) -> Self {
        Self(Arc::new(Some(evidence)))
    }
}

impl std::ops::Deref for NousOAuthHttpEvidence {
    type Target = Option<HttpEvidence>;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

#[derive(Clone, Debug)]
pub struct NousOAuthFailure {
    pub class: &'static str,
    pub code: &'static str,
    pub detail: String,
    pub retryable: bool,
    pub http: NousOAuthHttpEvidence,
    pub body: Option<BodyEvidence>,
}

impl NousOAuthFailure {
    fn protocol(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "protocol",
            code,
            detail: redact_secrets(detail.into()),
            retryable: false,
            http: NousOAuthHttpEvidence::empty(),
            body: None,
        }
    }

    fn unavailable(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "failed_closed",
            code,
            detail: redact_secrets(detail.into()),
            retryable: false,
            http: NousOAuthHttpEvidence::empty(),
            body: None,
        }
    }

    fn transport(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "transport",
            code,
            detail: redact_secrets(detail.into()),
            retryable: true,
            http: NousOAuthHttpEvidence::empty(),
            body: None,
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
            http: NousOAuthHttpEvidence::from_evidence(HttpEvidence {
                status,
                content_type: None,
                identifiers: Default::default(),
            }),
            body: Some(BodyEvidence {
                bytes: bytes.len(),
                blake3: blake3::hash(bytes).to_hex().to_string(),
            }),
        }
    }
}

impl std::fmt::Display for NousOAuthFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for NousOAuthFailure {}

fn credential_fingerprint(credentials: &NousOAuthCredentials) -> Result<String, NousOAuthFailure> {
    let encoded = serde_json::to_vec(credentials).map_err(|error| {
        NousOAuthFailure::protocol(
            "nous_oauth_credentials_fingerprint_failed",
            format!("Nous OAuth credentials could not be fingerprinted: {error}"),
        )
    })?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
}

fn validate_refresh_identity(
    schema_version: u32,
    provider_interface: &str,
    source_fingerprint: &str,
    expected_interface: &str,
) -> Result<(), NousOAuthFailure> {
    if schema_version != SCHEMA_VERSION
        || provider_interface != expected_interface
        || source_fingerprint.len() != 64
        || !source_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(NousOAuthFailure::protocol(
            "nous_oauth_refresh_journal_identity_invalid",
            "Nous OAuth refresh journal identity does not match the exact interface credential",
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub struct NousOAuthCredentialManager {
    interface_id: String,
    config: NousOAuthConfig,
    credentials: SecretHandle,
    invoke_token_cache: SecretHandle,
    device_state: SecretHandle,
    refresh_journal: SecretHandle,
    secrets: SecretsManager,
    client: reqwest::Client,
    lock: Arc<Mutex<()>>,
}

impl NousOAuthCredentialManager {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: &Path,
    ) -> Result<Self, NousOAuthFailure> {
        let driver_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
            NousOAuthFailure::protocol(
                "nous_oauth_config_missing",
                "Nous OAuth interface has no driver config",
            )
        })?;
        let raw = driver_config
            .private_config
            .get("nous_oauth")
            .cloned()
            .ok_or_else(|| {
                NousOAuthFailure::protocol(
                    "nous_oauth_config_missing",
                    "Nous OAuth interface has no private_config.nous_oauth",
                )
            })?;
        let config: NousOAuthConfig = serde_json::from_value(raw).map_err(|error| {
            NousOAuthFailure::protocol(
                "nous_oauth_config_invalid",
                format!("Nous OAuth config is invalid: {error}"),
            )
        })?;
        validate_config(&config)?;
        let interface_id = descriptor.id.as_str().to_string();
        let credentials = parse_secret_ref(&config.credentials_secret_ref)?;
        let invoke_token_cache = parse_secret_ref(&config.invoke_token_cache_secret_ref)?;
        let device_state = parse_secret_ref(&config.device_state_secret_ref)?;
        let refresh_journal = parse_secret_ref(&config.refresh_journal_secret_ref)?;
        for handle in [
            &credentials,
            &invoke_token_cache,
            &device_state,
            &refresh_journal,
        ] {
            if handle.scope.name() != Some(interface_id.as_str()) {
                return Err(NousOAuthFailure::protocol(
                    "nous_oauth_secret_scope_mismatch",
                    "Nous OAuth secret handles must use the exact interface scope",
                ));
            }
        }
        let mut names = BTreeSet::new();
        for handle in [
            &credentials,
            &invoke_token_cache,
            &device_state,
            &refresh_journal,
        ] {
            if !names.insert(handle.name.as_str()) {
                return Err(NousOAuthFailure::protocol(
                    "nous_oauth_secret_handles_collide",
                    "Nous OAuth secret handles must be distinct",
                ));
            }
        }
        let transport = driver_config.transport.as_ref().ok_or_else(|| {
            NousOAuthFailure::protocol(
                "nous_oauth_transport_missing",
                "Nous OAuth interface has no transport",
            )
        })?;
        if transport.auth_secret_ref.as_deref()
            != Some(config.invoke_token_cache_secret_ref.as_str())
        {
            return Err(NousOAuthFailure::protocol(
                "nous_oauth_auth_cache_mismatch",
                "Nous OAuth transport auth handle must equal its invoke-token cache handle",
            ));
        }
        if transport.base_url != INFERENCE_URL
            || transport.wire.as_deref() != Some("openai_chat_completions")
        {
            return Err(NousOAuthFailure::protocol(
                "nous_oauth_wire_mismatch",
                "Nous OAuth interface must use the exact Nous inference Chat Completions wire",
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
                NousOAuthFailure::unavailable(
                    "nous_oauth_client_build_failed",
                    format!("Nous OAuth HTTP client could not be built: {error}"),
                )
            })?;
        Ok(Self {
            interface_id,
            config,
            credentials,
            invoke_token_cache,
            device_state,
            refresh_journal,
            secrets,
            client,
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn handles_label(label: &str) -> bool {
        matches!(label, DEVICE_START_LABEL | DEVICE_POLL_LABEL)
    }

    pub async fn handle_artifact(
        &self,
        label: &str,
        payload: &[u8],
    ) -> Result<NousAuthArtifact, NousOAuthFailure> {
        require_empty_object(payload)?;
        let _guard = self.lock.lock().await;
        match label {
            DEVICE_START_LABEL => self.start_device_flow().await,
            DEVICE_POLL_LABEL => self.poll_device_flow().await,
            _ => Err(NousOAuthFailure::protocol(
                "nous_oauth_artifact_unsupported",
                format!("Nous OAuth does not accept artifact label '{label}'"),
            )),
        }
    }

    pub async fn ensure_invoke_token(&self, force_refresh: bool) -> Result<(), NousOAuthFailure> {
        let _guard = self.lock.lock().await;
        let credentials = self.read_credentials()?;
        let mut credentials = self.recover_refresh_journal(credentials)?;
        let status = invoke_jwt_status(&credentials, self.config.refresh_skew_seconds)?;
        if force_refresh || status.is_some() {
            credentials = self.refresh(&credentials).await?;
        }
        if let Some(reason) = invoke_jwt_status(&credentials, self.config.refresh_skew_seconds)? {
            return Err(NousOAuthFailure::unavailable(
                "nous_invoke_jwt_unusable",
                format!(
                    "Nous Portal access token is not a usable inference JWT ({reason}); start a new device authorization"
                ),
            ));
        }
        self.write_secret(&self.invoke_token_cache, &credentials.access_token)
    }

    async fn start_device_flow(&self) -> Result<NousAuthArtifact, NousOAuthFailure> {
        let response = self
            .client
            .post(&self.config.device_code_endpoint)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("scope", self.config.scope.as_str()),
            ])
            .send()
            .await
            .map_err(|error| {
                NousOAuthFailure::transport(
                    "nous_device_start_send_failed",
                    format!("Nous device authorization could not be sent: {error}"),
                )
            })?;
        let status = response.status().as_u16();
        let bytes = read_bounded(response, self.config.max_response_bytes).await?;
        if !(200..300).contains(&status) {
            return Err(NousOAuthFailure::endpoint(
                "nous_device_start_rejected",
                retryable_status(status),
                format!("Nous device authorization returned HTTP {status}"),
                status,
                &bytes,
            ));
        }
        let value = parse_json_object(&bytes, "Nous device authorization")?;
        let device_code = required_string(&value, "device_code", "Nous device authorization")?;
        let user_code = required_string(&value, "user_code", "Nous device authorization")?;
        let verification_uri =
            required_string(&value, "verification_uri", "Nous device authorization")?;
        let verification_uri_complete = required_string(
            &value,
            "verification_uri_complete",
            "Nous device authorization",
        )?;
        validate_portal_verification_url(&verification_uri)?;
        validate_portal_verification_url(&verification_uri_complete)?;
        let expires_in = required_positive_u64(&value, "expires_in", "Nous device authorization")?;
        let interval = required_positive_u64(&value, "interval", "Nous device authorization")?;
        let now = unix_ms()?;
        let state = DeviceState {
            schema_version: SCHEMA_VERSION,
            device_code,
            expires_at_unix_ms: checked_expiry(now, expires_in)?,
            interval_seconds: interval.min(self.config.poll_interval_cap_seconds).max(1),
        };
        self.write_device_state(&state)?;
        Ok(NousAuthArtifact {
            label: DEVICE_CODE_LABEL,
            payload: json!({
                "label": DEVICE_CODE_LABEL,
                "verification_uri": verification_uri,
                "verification_uri_complete": verification_uri_complete,
                "user_code": user_code,
                "expires_in_seconds": expires_in,
                "poll_after_seconds": state.interval_seconds,
                "credential_state": "secret_backed_pending"
            }),
        })
    }

    async fn poll_device_flow(&self) -> Result<NousAuthArtifact, NousOAuthFailure> {
        let mut state = self.read_device_state()?;
        if unix_ms()? >= state.expires_at_unix_ms {
            self.delete_secret(&self.device_state)?;
            return Err(NousOAuthFailure::unavailable(
                "nous_device_code_expired",
                "Nous device authorization expired; start a new device authorization",
            ));
        }
        let response = self
            .client
            .post(&self.config.token_endpoint)
            .header("Accept", "application/json")
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", self.config.client_id.as_str()),
                ("device_code", state.device_code.as_str()),
            ])
            .send()
            .await
            .map_err(|error| {
                NousOAuthFailure::transport(
                    "nous_device_poll_send_failed",
                    format!("Nous device authorization poll failed: {error}"),
                )
            })?;
        let status = response.status().as_u16();
        let bytes = read_bounded(response, self.config.max_response_bytes).await?;
        if (200..300).contains(&status) {
            let value = parse_json_object(&bytes, "Nous device token exchange")?;
            let credentials = credentials_from_token_response(&value, None, &self.config)?;
            self.write_credentials(&credentials)?;
            self.delete_secret(&self.refresh_journal)?;
            self.delete_secret(&self.device_state)?;
            if let Some(reason) = invoke_jwt_status(&credentials, self.config.refresh_skew_seconds)?
            {
                return Err(NousOAuthFailure::unavailable(
                    "nous_device_invoke_jwt_unusable",
                    format!(
                        "Nous device authorization returned an unusable inference JWT ({reason})"
                    ),
                ));
            }
            self.write_secret(&self.invoke_token_cache, &credentials.access_token)?;
            return Ok(NousAuthArtifact {
                label: AUTH_STATUS_LABEL,
                payload: json!({
                    "label": AUTH_STATUS_LABEL,
                    "authenticated": true,
                    "credential_state": "secret_backed",
                    "source": "device_code",
                    "auth_path": "invoke_jwt",
                    "scope": credentials.scope,
                    "expires_at_unix_ms": credentials.expires_at_unix_ms
                }),
            });
        }
        let value = match parse_json_object(&bytes, "Nous device token exchange") {
            Ok(value) => value,
            Err(_) => {
                return Err(NousOAuthFailure::endpoint(
                    "nous_device_poll_rejected",
                    retryable_status(status),
                    format!("Nous device token exchange returned non-JSON HTTP {status}"),
                    status,
                    &bytes,
                ));
            }
        };
        let code = required_string(&value, "error", "Nous device token exchange")?;
        match code.as_str() {
            "authorization_pending" => Ok(NousAuthArtifact {
                label: AUTH_STATUS_LABEL,
                payload: json!({
                    "label": AUTH_STATUS_LABEL,
                    "authenticated": false,
                    "status": "authorization_pending",
                    "poll_after_seconds": state.interval_seconds
                }),
            }),
            "slow_down" => {
                state.interval_seconds = state
                    .interval_seconds
                    .saturating_add(self.config.slow_down_increment_seconds)
                    .min(self.config.slow_down_cap_seconds);
                self.write_device_state(&state)?;
                Ok(NousAuthArtifact {
                    label: AUTH_STATUS_LABEL,
                    payload: json!({
                        "label": AUTH_STATUS_LABEL,
                        "authenticated": false,
                        "status": "slow_down",
                        "poll_after_seconds": state.interval_seconds
                    }),
                })
            }
            "expired_token" | "access_denied" => {
                self.delete_secret(&self.device_state)?;
                Err(NousOAuthFailure::endpoint(
                    "nous_device_authorization_terminal",
                    false,
                    format!("Nous device authorization ended with {code}"),
                    status,
                    &bytes,
                ))
            }
            _ => Err(NousOAuthFailure::endpoint(
                "nous_device_poll_rejected",
                retryable_status(status),
                format!("Nous device token exchange returned HTTP {status} with {code}"),
                status,
                &bytes,
            )),
        }
    }

    async fn refresh(
        &self,
        credentials: &NousOAuthCredentials,
    ) -> Result<NousOAuthCredentials, NousOAuthFailure> {
        self.write_refresh_prepared(credentials)?;
        let result = async {
            let response = self
                .client
                .post(&self.config.token_endpoint)
                .header("Accept", "application/json")
                .header("x-nous-refresh-token", &credentials.refresh_token)
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", self.config.client_id.as_str()),
                ])
                .send()
                .await
                .map_err(|error| {
                    NousOAuthFailure::transport(
                        "nous_oauth_refresh_send_failed",
                        format!("Nous OAuth refresh could not be sent: {error}"),
                    )
                })?;
            let status = response.status().as_u16();
            let bytes = read_bounded(response, self.config.max_response_bytes).await?;
            if !(200..300).contains(&status) {
                let value = match parse_json_object(&bytes, "Nous OAuth refresh") {
                    Ok(value) => value,
                    Err(_) => {
                        self.delete_secret(&self.credentials)?;
                        self.delete_secret(&self.invoke_token_cache)?;
                        return Err(NousOAuthFailure::endpoint(
                            "nous_oauth_refresh_rejected",
                            false,
                            format!("Nous OAuth refresh returned non-JSON HTTP {status}"),
                            status,
                            &bytes,
                        ));
                    }
                };
                let code = optional_string(&value, "error", "Nous OAuth refresh")?
                    .unwrap_or_else(|| "refresh_rejected".to_string());
                let description =
                    optional_string(&value, "error_description", "Nous OAuth refresh")?
                        .unwrap_or_else(|| "refresh token exchange failed".to_string());
                let terminal = matches!(
                    code.as_str(),
                    "invalid_grant" | "invalid_token" | "refresh_token_reused"
                ) || description.to_ascii_lowercase().contains("reuse");
                if terminal {
                    self.delete_secret(&self.credentials)?;
                    self.delete_secret(&self.invoke_token_cache)?;
                }
                return Err(NousOAuthFailure::endpoint(
                    "nous_oauth_refresh_rejected",
                    !terminal && retryable_status(status),
                    if terminal {
                        format!(
                            "Nous OAuth refresh session is no longer usable ({code}); start a new device authorization"
                        )
                    } else {
                        format!(
                            "Nous OAuth refresh returned HTTP {status} with {code}: {description}"
                        )
                    },
                    status,
                    &bytes,
                ));
            }
            let value = parse_json_object(&bytes, "Nous OAuth refresh")?;
            credentials_from_token_response(&value, Some(credentials), &self.config)
        }
        .await;
        self.finish_refresh_attempt(credentials, result)
    }

    fn finish_refresh_attempt(
        &self,
        prior: &NousOAuthCredentials,
        result: Result<NousOAuthCredentials, NousOAuthFailure>,
    ) -> Result<NousOAuthCredentials, NousOAuthFailure> {
        match result {
            Ok(credentials) => {
                self.write_refresh_received(prior, &credentials)?;
                self.write_credentials(&credentials)?;
                self.delete_secret(&self.refresh_journal)?;
                Ok(credentials)
            }
            Err(error) => {
                if error.http.is_some() {
                    self.delete_secret(&self.refresh_journal)?;
                }
                Err(error)
            }
        }
    }

    fn recover_refresh_journal(
        &self,
        current: NousOAuthCredentials,
    ) -> Result<NousOAuthCredentials, NousOAuthFailure> {
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
                self.delete_secret(&self.invoke_token_cache)?;
                Err(NousOAuthFailure::unavailable(
                    "nous_oauth_refresh_outcome_unknown",
                    "Nous OAuth refresh may have rotated the credential before its response became durable; start a new device authorization for this exact interface",
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
                validate_credentials(&credentials)?;
                let recovered = if current_fingerprint == source_fingerprint {
                    self.write_credentials(&credentials)?;
                    credentials
                } else {
                    current
                };
                self.delete_secret(&self.refresh_journal)?;
                Ok(recovered)
            }
        }
    }

    fn write_refresh_prepared(
        &self,
        credentials: &NousOAuthCredentials,
    ) -> Result<(), NousOAuthFailure> {
        if self.read_refresh_journal()?.is_some() {
            return Err(NousOAuthFailure::unavailable(
                "nous_oauth_refresh_journal_occupied",
                "Nous OAuth refresh journal already contains an unresolved operation",
            ));
        }
        self.write_refresh_journal(&RefreshJournal::Prepared {
            schema_version: SCHEMA_VERSION,
            provider_interface: self.interface_id.clone(),
            source_fingerprint: credential_fingerprint(credentials)?,
        })
    }

    fn write_refresh_received(
        &self,
        prior: &NousOAuthCredentials,
        credentials: &NousOAuthCredentials,
    ) -> Result<(), NousOAuthFailure> {
        validate_credentials(credentials)?;
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
                    return Err(NousOAuthFailure::protocol(
                        "nous_oauth_refresh_journal_source_mismatch",
                        "Nous OAuth refresh journal does not match the credential used for this refresh",
                    ));
                }
            }
            Some(RefreshJournal::Received { .. }) => {
                return Err(NousOAuthFailure::protocol(
                    "nous_oauth_refresh_journal_phase_invalid",
                    "Nous OAuth refresh journal already contains a received credential",
                ));
            }
            None => {
                return Err(NousOAuthFailure::protocol(
                    "nous_oauth_refresh_journal_missing",
                    "Nous OAuth refresh response arrived without a prepared private journal record",
                ));
            }
        }
        self.write_refresh_journal(&RefreshJournal::Received {
            schema_version: SCHEMA_VERSION,
            provider_interface: self.interface_id.clone(),
            source_fingerprint,
            credentials: credentials.clone(),
        })
    }

    fn read_refresh_journal(&self) -> Result<Option<RefreshJournal>, NousOAuthFailure> {
        let Some(encoded) = self.read_secret(&self.refresh_journal)? else {
            return Ok(None);
        };
        let journal: RefreshJournal = serde_json::from_str(&encoded).map_err(|error| {
            NousOAuthFailure::protocol(
                "nous_oauth_refresh_journal_invalid",
                format!("Nous OAuth refresh journal JSON is invalid: {error}"),
            )
        })?;
        Ok(Some(journal))
    }

    fn write_refresh_journal(&self, journal: &RefreshJournal) -> Result<(), NousOAuthFailure> {
        let encoded = serde_json::to_string(journal).map_err(|error| {
            NousOAuthFailure::protocol(
                "nous_oauth_refresh_journal_encode_failed",
                format!("Nous OAuth refresh journal could not be encoded: {error}"),
            )
        })?;
        self.write_secret(&self.refresh_journal, &encoded)
    }

    fn read_credentials(&self) -> Result<NousOAuthCredentials, NousOAuthFailure> {
        let encoded = self.read_secret(&self.credentials)?.ok_or_else(|| {
            NousOAuthFailure::unavailable(
                "nous_oauth_credentials_missing",
                "Nous OAuth credentials are not configured; send provider.auth.device.start to this exact interface",
            )
        })?;
        let credentials: NousOAuthCredentials =
            serde_json::from_str(&encoded).map_err(|error| {
                NousOAuthFailure::protocol(
                    "nous_oauth_credentials_invalid",
                    format!("Nous OAuth credential JSON is invalid: {error}"),
                )
            })?;
        validate_credentials(&credentials)?;
        Ok(credentials)
    }

    fn read_device_state(&self) -> Result<DeviceState, NousOAuthFailure> {
        let encoded = self.read_secret(&self.device_state)?.ok_or_else(|| {
            NousOAuthFailure::unavailable(
                "nous_device_state_missing",
                "Nous device authorization has not been started",
            )
        })?;
        let state: DeviceState = serde_json::from_str(&encoded).map_err(|error| {
            NousOAuthFailure::protocol(
                "nous_device_state_invalid",
                format!("Nous device state JSON is invalid: {error}"),
            )
        })?;
        validate_device_state(&state)?;
        Ok(state)
    }

    fn write_credentials(
        &self,
        credentials: &NousOAuthCredentials,
    ) -> Result<(), NousOAuthFailure> {
        validate_credentials(credentials)?;
        let encoded = serde_json::to_string(credentials).map_err(|error| {
            NousOAuthFailure::protocol(
                "nous_oauth_credentials_encode_failed",
                format!("Nous OAuth credential JSON could not be encoded: {error}"),
            )
        })?;
        self.write_secret(&self.credentials, &encoded)
    }

    fn write_device_state(&self, state: &DeviceState) -> Result<(), NousOAuthFailure> {
        validate_device_state(state)?;
        let encoded = serde_json::to_string(state).map_err(|error| {
            NousOAuthFailure::protocol(
                "nous_device_state_encode_failed",
                format!("Nous device state JSON could not be encoded: {error}"),
            )
        })?;
        self.write_secret(&self.device_state, &encoded)
    }

    fn read_secret(&self, handle: &SecretHandle) -> Result<Option<String>, NousOAuthFailure> {
        self.secrets
            .get(&handle.scope, &handle.name)
            .map_err(|error| {
                NousOAuthFailure::unavailable(
                    "nous_oauth_secret_read_failed",
                    format!(
                        "Nous OAuth secret '{}/{}' could not be read: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }

    fn write_secret(&self, handle: &SecretHandle, value: &str) -> Result<(), NousOAuthFailure> {
        self.secrets
            .set(&handle.scope, &handle.name, value)
            .map_err(|error| {
                NousOAuthFailure::unavailable(
                    "nous_oauth_secret_write_failed",
                    format!(
                        "Nous OAuth secret '{}/{}' could not be written: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }

    fn delete_secret(&self, handle: &SecretHandle) -> Result<(), NousOAuthFailure> {
        self.secrets
            .delete(&handle.scope, &handle.name)
            .map(|_| ())
            .map_err(|error| {
                NousOAuthFailure::unavailable(
                    "nous_oauth_secret_delete_failed",
                    format!(
                        "Nous OAuth secret '{}/{}' could not be deleted: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }
}

fn credentials_from_token_response(
    value: &Map<String, Value>,
    prior: Option<&NousOAuthCredentials>,
    config: &NousOAuthConfig,
) -> Result<NousOAuthCredentials, NousOAuthFailure> {
    let access_token = required_string(value, "access_token", "Nous OAuth token response")?;
    let refresh_token = optional_string(value, "refresh_token", "Nous OAuth token response")?
        .or_else(|| prior.map(|credential| credential.refresh_token.clone()))
        .ok_or_else(|| {
            NousOAuthFailure::protocol(
                "nous_oauth_refresh_token_missing",
                "Nous OAuth token response has no refresh_token",
            )
        })?;
    let token_type = optional_string(value, "token_type", "Nous OAuth token response")?
        .or_else(|| prior.map(|credential| credential.token_type.clone()))
        .unwrap_or_else(|| "Bearer".to_string());
    let scope = optional_string(value, "scope", "Nous OAuth token response")?
        .or_else(|| prior.map(|credential| credential.scope.clone()))
        .unwrap_or_else(|| config.scope.clone());
    let expires_in = required_positive_u64(value, "expires_in", "Nous OAuth token response")?;
    if let Some(network_url) =
        optional_string(value, "inference_base_url", "Nous OAuth token response")?
    {
        if network_url.trim_end_matches('/') != INFERENCE_URL {
            return Err(NousOAuthFailure::protocol(
                "nous_oauth_inference_url_rejected",
                "Nous Portal returned an inference URL outside the exact interface endpoint",
            ));
        }
    }
    let credentials = NousOAuthCredentials {
        schema_version: SCHEMA_VERSION,
        access_token,
        refresh_token,
        token_type,
        scope,
        expires_at_unix_ms: checked_expiry(unix_ms()?, expires_in)?,
        inference_base_url: INFERENCE_URL.to_string(),
    };
    validate_credentials(&credentials)?;
    Ok(credentials)
}

fn validate_config(config: &NousOAuthConfig) -> Result<(), NousOAuthFailure> {
    if config.schema_version != SCHEMA_VERSION {
        return Err(NousOAuthFailure::protocol(
            "nous_oauth_config_version_unsupported",
            format!(
                "Nous OAuth config schema_version {} is unsupported",
                config.schema_version
            ),
        ));
    }
    if config.client_id != "hermes-cli" || config.scope != INFERENCE_SCOPE {
        return Err(NousOAuthFailure::protocol(
            "nous_oauth_client_contract_invalid",
            "Nous OAuth exact interface requires client_id hermes-cli and scope inference:invoke",
        ));
    }
    if config.refresh_skew_seconds == 0
        || config.poll_interval_cap_seconds == 0
        || config.slow_down_increment_seconds == 0
        || config.slow_down_cap_seconds < config.poll_interval_cap_seconds
        || config.request_timeout_ms == 0
        || config.max_response_bytes == 0
    {
        return Err(NousOAuthFailure::protocol(
            "nous_oauth_limits_invalid",
            "Nous OAuth limits, polling policy, and refresh skew are invalid",
        ));
    }
    validate_endpoint(
        &config.device_code_endpoint,
        "/api/oauth/device/code",
        "device-code endpoint",
    )?;
    validate_endpoint(&config.token_endpoint, "/api/oauth/token", "token endpoint")
}

fn validate_endpoint(raw: &str, path: &str, surface: &str) -> Result<(), NousOAuthFailure> {
    let url = Url::parse(raw).map_err(|error| {
        NousOAuthFailure::protocol(
            "nous_oauth_endpoint_invalid",
            format!("Nous OAuth {surface} is invalid: {error}"),
        )
    })?;
    if url.scheme() != "https"
        || url.host_str() != Some("portal.nousresearch.com")
        || url.path() != path
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(NousOAuthFailure::protocol(
            "nous_oauth_endpoint_invalid",
            format!("Nous OAuth {surface} must be the exact credential-free Portal HTTPS endpoint"),
        ));
    }
    Ok(())
}

fn validate_portal_verification_url(raw: &str) -> Result<(), NousOAuthFailure> {
    let url = Url::parse(raw).map_err(|error| {
        NousOAuthFailure::protocol(
            "nous_device_verification_url_invalid",
            format!("Nous device verification URL is invalid: {error}"),
        )
    })?;
    if url.scheme() != "https"
        || url.host_str() != Some("portal.nousresearch.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(NousOAuthFailure::protocol(
            "nous_device_verification_url_invalid",
            "Nous device verification URL must remain on the credential-free Portal HTTPS host",
        ));
    }
    Ok(())
}

fn validate_credentials(credentials: &NousOAuthCredentials) -> Result<(), NousOAuthFailure> {
    if credentials.schema_version != SCHEMA_VERSION {
        return Err(NousOAuthFailure::protocol(
            "nous_oauth_credentials_version_unsupported",
            format!(
                "Nous OAuth credential schema_version {} is unsupported",
                credentials.schema_version
            ),
        ));
    }
    for (value, surface) in [
        (&credentials.access_token, "access_token"),
        (&credentials.refresh_token, "refresh_token"),
    ] {
        if value.is_empty() || value.trim() != value || value.chars().any(char::is_whitespace) {
            return Err(NousOAuthFailure::protocol(
                "nous_oauth_credentials_invalid",
                format!("Nous OAuth credential {surface} is malformed"),
            ));
        }
    }
    if credentials.token_type != "Bearer"
        || credentials.scope.trim() != credentials.scope
        || credentials.scope.is_empty()
        || credentials.expires_at_unix_ms == 0
        || credentials.inference_base_url != INFERENCE_URL
    {
        return Err(NousOAuthFailure::protocol(
            "nous_oauth_credentials_invalid",
            "Nous OAuth credential has invalid token type, scope, expiry, or inference endpoint",
        ));
    }
    Ok(())
}

fn validate_device_state(state: &DeviceState) -> Result<(), NousOAuthFailure> {
    if state.schema_version != SCHEMA_VERSION
        || state.device_code.is_empty()
        || state.device_code.trim() != state.device_code
        || state.device_code.chars().any(char::is_whitespace)
        || state.expires_at_unix_ms == 0
        || state.interval_seconds == 0
        || state.interval_seconds > 30
    {
        return Err(NousOAuthFailure::protocol(
            "nous_device_state_invalid",
            "Nous device state does not satisfy its versioned schema",
        ));
    }
    Ok(())
}

fn invoke_jwt_status(
    credentials: &NousOAuthCredentials,
    skew_seconds: u64,
) -> Result<Option<&'static str>, NousOAuthFailure> {
    let claims = match decode_jwt_claims(&credentials.access_token) {
        Ok(claims) => claims,
        Err(_) => return Ok(Some("access_token_not_jwt")),
    };
    let mut scopes = scope_values(Some(&credentials.scope));
    extend_claim_scopes(&mut scopes, claims.get("scope"));
    extend_claim_scopes(&mut scopes, claims.get("scp"));
    if !scopes.contains(INFERENCE_SCOPE) {
        return Ok(Some("missing_inference_invoke_scope"));
    }
    let threshold = unix_ms()?.saturating_add(skew_seconds.saturating_mul(1000));
    if let Some(exp) = claims.get("exp").and_then(Value::as_u64) {
        if exp.saturating_mul(1000) <= threshold {
            return Ok(Some("invoke_jwt_expiring"));
        }
        return Ok(None);
    }
    if credentials.expires_at_unix_ms <= threshold {
        return Ok(Some("invoke_jwt_expiry_unknown_or_expiring"));
    }
    Ok(None)
}

fn decode_jwt_claims(token: &str) -> Result<Map<String, Value>, NousOAuthFailure> {
    let mut parts = token.split('.');
    let Some(_) = parts.next() else {
        return Err(NousOAuthFailure::protocol(
            "nous_invoke_jwt_invalid",
            "Nous invoke token is not a JWT",
        ));
    };
    let payload = parts.next().ok_or_else(|| {
        NousOAuthFailure::protocol("nous_invoke_jwt_invalid", "Nous invoke token is not a JWT")
    })?;
    if parts.next().is_none() || parts.next().is_some() {
        return Err(NousOAuthFailure::protocol(
            "nous_invoke_jwt_invalid",
            "Nous invoke token is not a three-part JWT",
        ));
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| {
            NousOAuthFailure::protocol(
                "nous_invoke_jwt_invalid",
                "Nous invoke JWT payload is not valid base64url",
            )
        })?;
    serde_json::from_slice::<Map<String, Value>>(&decoded).map_err(|_| {
        NousOAuthFailure::protocol(
            "nous_invoke_jwt_invalid",
            "Nous invoke JWT payload is not a JSON object",
        )
    })
}

fn scope_values(scope: Option<&str>) -> BTreeSet<String> {
    scope
        .into_iter()
        .flat_map(|value| {
            value
                .replace(',', " ")
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn extend_claim_scopes(scopes: &mut BTreeSet<String>, value: Option<&Value>) {
    match value {
        Some(Value::String(value)) => scopes.extend(scope_values(Some(value))),
        Some(Value::Array(values)) => {
            for value in values.iter().filter_map(Value::as_str) {
                scopes.extend(scope_values(Some(value)));
            }
        }
        _ => {}
    }
}

fn require_empty_object(payload: &[u8]) -> Result<(), NousOAuthFailure> {
    let value: Value = serde_json::from_slice(payload).map_err(|error| {
        NousOAuthFailure::protocol(
            "nous_oauth_artifact_json_invalid",
            format!("Nous OAuth artifact is not valid JSON: {error}"),
        )
    })?;
    if !value.as_object().is_some_and(Map::is_empty) {
        return Err(NousOAuthFailure::protocol(
            "nous_oauth_artifact_invalid",
            "Nous OAuth control artifact payload must be an empty JSON object",
        ));
    }
    Ok(())
}

fn parse_json_object(bytes: &[u8], surface: &str) -> Result<Map<String, Value>, NousOAuthFailure> {
    serde_json::from_slice(bytes).map_err(|error| {
        NousOAuthFailure::protocol(
            "nous_oauth_response_json_invalid",
            format!("{surface} returned invalid JSON: {error}"),
        )
    })
}

fn required_string(
    value: &Map<String, Value>,
    field: &str,
    surface: &str,
) -> Result<String, NousOAuthFailure> {
    optional_string(value, field, surface)?.ok_or_else(|| {
        NousOAuthFailure::protocol(
            "nous_oauth_response_field_missing",
            format!("{surface} returned no {field}"),
        )
    })
}

fn optional_string(
    value: &Map<String, Value>,
    field: &str,
    surface: &str,
) -> Result<Option<String>, NousOAuthFailure> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() && value.trim() == value => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(NousOAuthFailure::protocol(
            "nous_oauth_response_field_invalid",
            format!("{surface} returned malformed {field}"),
        )),
    }
}

fn required_positive_u64(
    value: &Map<String, Value>,
    field: &str,
    surface: &str,
) -> Result<u64, NousOAuthFailure> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            NousOAuthFailure::protocol(
                "nous_oauth_response_field_invalid",
                format!("{surface} returned no positive unsigned {field}"),
            )
        })
}

async fn read_bounded(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, NousOAuthFailure> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        NousOAuthFailure::transport(
            "nous_oauth_response_read_failed",
            format!("Nous OAuth response could not be read: {error}"),
        )
    })? {
        let next = bytes.len().checked_add(chunk.len()).ok_or_else(|| {
            NousOAuthFailure::protocol(
                "nous_oauth_response_too_large",
                "Nous OAuth response length overflowed",
            )
        })?;
        if next > maximum {
            return Err(NousOAuthFailure::protocol(
                "nous_oauth_response_too_large",
                format!("Nous OAuth response exceeded {maximum} bytes"),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn parse_secret_ref(reference: &str) -> Result<SecretHandle, NousOAuthFailure> {
    let raw = reference.strip_prefix("secret://").ok_or_else(|| {
        NousOAuthFailure::protocol(
            "nous_oauth_secret_ref_invalid",
            "Nous OAuth secret ref must start with secret://",
        )
    })?;
    let (scope, name) = raw.split_once('/').ok_or_else(|| {
        NousOAuthFailure::protocol(
            "nous_oauth_secret_ref_invalid",
            "Nous OAuth secret ref must be secret://<scope>/<name>",
        )
    })?;
    if name.contains('/') {
        return Err(NousOAuthFailure::protocol(
            "nous_oauth_secret_ref_invalid",
            "Nous OAuth secret ref must name one scoped secret",
        ));
    }
    Ok(SecretHandle {
        scope: SecretScope::named(scope.to_string()).map_err(|error| {
            NousOAuthFailure::protocol("nous_oauth_secret_ref_invalid", error.to_string())
        })?,
        name: SecretName::new(name).map_err(|error| {
            NousOAuthFailure::protocol("nous_oauth_secret_ref_invalid", error.to_string())
        })?,
    })
}

fn checked_expiry(now_ms: u64, expires_in_seconds: u64) -> Result<u64, NousOAuthFailure> {
    now_ms
        .checked_add(expires_in_seconds.checked_mul(1000).ok_or_else(|| {
            NousOAuthFailure::protocol(
                "nous_oauth_expiry_overflow",
                "Nous OAuth expiry exceeds u64",
            )
        })?)
        .ok_or_else(|| {
            NousOAuthFailure::protocol(
                "nous_oauth_expiry_overflow",
                "Nous OAuth expiry exceeds u64",
            )
        })
}

fn unix_ms() -> Result<u64, NousOAuthFailure> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            NousOAuthFailure::unavailable(
                "nous_oauth_clock_invalid",
                format!("system clock precedes Unix epoch: {error}"),
            )
        })?
        .as_millis();
    u64::try_from(millis).map_err(|_| {
        NousOAuthFailure::unavailable(
            "nous_oauth_clock_invalid",
            "system clock milliseconds exceed u64",
        )
    })
}

fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
}
