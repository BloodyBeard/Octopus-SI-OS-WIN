use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use octopus_interfaces::InterfaceDescriptor;
use octopus_secrets::{
    redact_secrets, LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind,
    SecretsManager,
};
use octopus_world_provider_openai_compatible::{BodyEvidence, HttpEvidence};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

const CREDENTIAL_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QwenOAuthConfig {
    schema_version: u32,
    credentials_secret_ref: String,
    access_token_cache_secret_ref: String,
    refresh_journal_secret_ref: String,
    token_endpoint: String,
    client_id: String,
    refresh_skew_seconds: u64,
    request_timeout_ms: u64,
    max_response_bytes: usize,
    qwen_code_version: String,
}

#[derive(Clone, Debug)]
struct SecretHandle {
    scope: SecretScope,
    name: SecretName,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QwenOAuthCredentials {
    pub schema_version: u32,
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_at_unix_ms: u64,
    pub resource_url: String,
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
        credentials: QwenOAuthCredentials,
    },
}

#[derive(Clone, Debug)]
pub struct QwenOAuthHttpEvidence(Arc<Option<HttpEvidence>>);

impl QwenOAuthHttpEvidence {
    fn empty() -> Self {
        Self(Arc::new(None))
    }

    fn from_evidence(evidence: HttpEvidence) -> Self {
        Self(Arc::new(Some(evidence)))
    }
}

impl std::ops::Deref for QwenOAuthHttpEvidence {
    type Target = Option<HttpEvidence>;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

#[derive(Clone, Debug)]
pub struct QwenOAuthFailure {
    pub class: &'static str,
    pub code: &'static str,
    pub detail: String,
    pub retryable: bool,
    pub http: QwenOAuthHttpEvidence,
    pub body: Option<BodyEvidence>,
}

impl QwenOAuthFailure {
    fn protocol(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "protocol",
            code,
            detail: redact_secrets(detail.into()),
            retryable: false,
            http: QwenOAuthHttpEvidence::empty(),
            body: None,
        }
    }

    fn unavailable(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "failed_closed",
            code,
            detail: redact_secrets(detail.into()),
            retryable: false,
            http: QwenOAuthHttpEvidence::empty(),
            body: None,
        }
    }

    fn transport(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class: "transport",
            code,
            detail: redact_secrets(detail.into()),
            retryable: true,
            http: QwenOAuthHttpEvidence::empty(),
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
            http: QwenOAuthHttpEvidence::from_evidence(HttpEvidence {
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

impl std::fmt::Display for QwenOAuthFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for QwenOAuthFailure {}

fn credential_fingerprint(credentials: &QwenOAuthCredentials) -> Result<String, QwenOAuthFailure> {
    let encoded = serde_json::to_vec(credentials).map_err(|error| {
        QwenOAuthFailure::protocol(
            "qwen_oauth_credentials_fingerprint_failed",
            format!("Qwen OAuth credentials could not be fingerprinted: {error}"),
        )
    })?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
}

fn validate_refresh_identity(
    schema_version: u32,
    provider_interface: &str,
    source_fingerprint: &str,
    expected_interface: &str,
) -> Result<(), QwenOAuthFailure> {
    if schema_version != CREDENTIAL_SCHEMA_VERSION
        || provider_interface != expected_interface
        || source_fingerprint.len() != 64
        || !source_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(QwenOAuthFailure::protocol(
            "qwen_oauth_refresh_journal_identity_invalid",
            "Qwen OAuth refresh journal identity does not match the exact interface credential",
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub struct QwenOAuthCredentialManager {
    interface_id: String,
    config: QwenOAuthConfig,
    credentials: SecretHandle,
    access_token_cache: SecretHandle,
    refresh_journal: SecretHandle,
    secrets: SecretsManager,
    client: reqwest::Client,
    lock: Arc<Mutex<()>>,
}

impl QwenOAuthCredentialManager {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: &Path,
    ) -> Result<Self, QwenOAuthFailure> {
        let driver_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
            QwenOAuthFailure::protocol(
                "qwen_oauth_config_missing",
                "Qwen OAuth interface has no driver config",
            )
        })?;
        let raw = driver_config
            .private_config
            .get("qwen_oauth")
            .cloned()
            .ok_or_else(|| {
                QwenOAuthFailure::protocol(
                    "qwen_oauth_config_missing",
                    "Qwen OAuth interface has no private_config.qwen_oauth",
                )
            })?;
        let config: QwenOAuthConfig = serde_json::from_value(raw).map_err(|error| {
            QwenOAuthFailure::protocol(
                "qwen_oauth_config_invalid",
                format!("Qwen OAuth config is invalid: {error}"),
            )
        })?;
        validate_config(&config)?;
        let interface_id = descriptor.id.as_str().to_string();
        let credentials = parse_secret_ref(&config.credentials_secret_ref)?;
        let access_token_cache = parse_secret_ref(&config.access_token_cache_secret_ref)?;
        let refresh_journal = parse_secret_ref(&config.refresh_journal_secret_ref)?;
        for handle in [&credentials, &access_token_cache, &refresh_journal] {
            if handle.scope.name() != Some(interface_id.as_str()) {
                return Err(QwenOAuthFailure::protocol(
                    "qwen_oauth_secret_scope_mismatch",
                    "Qwen OAuth secret handles must use the exact interface scope",
                ));
            }
        }
        if credentials.name == access_token_cache.name
            || credentials.name == refresh_journal.name
            || access_token_cache.name == refresh_journal.name
        {
            return Err(QwenOAuthFailure::protocol(
                "qwen_oauth_secret_handles_collide",
                "Qwen OAuth credential, access-token cache and refresh-journal handles must differ",
            ));
        }
        let transport = driver_config.transport.as_ref().ok_or_else(|| {
            QwenOAuthFailure::protocol(
                "qwen_oauth_transport_missing",
                "Qwen OAuth interface has no transport",
            )
        })?;
        if transport.auth_secret_ref.as_deref()
            != Some(config.access_token_cache_secret_ref.as_str())
        {
            return Err(QwenOAuthFailure::protocol(
                "qwen_oauth_auth_cache_mismatch",
                "Qwen OAuth transport auth handle must equal its access-token cache handle",
            ));
        }
        if transport.base_url != "https://portal.qwen.ai/v1"
            || transport.wire.as_deref() != Some("openai_chat_completions")
        {
            return Err(QwenOAuthFailure::protocol(
                "qwen_oauth_wire_mismatch",
                "Qwen OAuth interface must use the exact Qwen Portal Chat Completions wire",
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
                QwenOAuthFailure::unavailable(
                    "qwen_oauth_client_build_failed",
                    format!("Qwen OAuth refresh client could not be built: {error}"),
                )
            })?;
        Ok(Self {
            interface_id,
            config,
            credentials,
            access_token_cache,
            refresh_journal,
            secrets,
            client,
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn provider_descriptor(
        &self,
        descriptor: &InterfaceDescriptor,
    ) -> Result<InterfaceDescriptor, QwenOAuthFailure> {
        let mut descriptor = descriptor.clone();
        let transport = descriptor
            .interface_driver_config
            .as_mut()
            .and_then(|config| config.transport.as_mut())
            .ok_or_else(|| {
                QwenOAuthFailure::protocol(
                    "qwen_oauth_transport_missing",
                    "Qwen OAuth interface has no transport",
                )
            })?;
        if !transport.headers.is_empty() {
            return Err(QwenOAuthFailure::protocol(
                "qwen_oauth_headers_ambiguous",
                "Qwen OAuth transport headers are authored by its credential life and must be empty in the descriptor",
            ));
        }
        let user_agent = qwen_user_agent(&self.config.qwen_code_version);
        transport
            .headers
            .insert("User-Agent".to_string(), user_agent.clone());
        transport
            .headers
            .insert("X-DashScope-CacheControl".to_string(), "enable".to_string());
        transport
            .headers
            .insert("X-DashScope-UserAgent".to_string(), user_agent);
        transport
            .headers
            .insert("X-DashScope-AuthType".to_string(), "qwen-oauth".to_string());
        Ok(descriptor)
    }

    pub async fn ensure_access_token(&self, force_refresh: bool) -> Result<(), QwenOAuthFailure> {
        let _guard = self.lock.lock().await;
        let credentials = self.read_credentials()?;
        let mut credentials = self.recover_refresh_journal(credentials)?;
        if force_refresh || credentials_expiring(&credentials, self.config.refresh_skew_seconds)? {
            credentials = self.refresh(&credentials).await?;
        }
        self.write_secret(&self.access_token_cache, &credentials.access_token)
    }

    fn read_credentials(&self) -> Result<QwenOAuthCredentials, QwenOAuthFailure> {
        let encoded = self
            .read_secret(&self.credentials)?
            .ok_or_else(|| {
                QwenOAuthFailure::unavailable(
                    "qwen_oauth_credentials_missing",
                    format!(
                        "Qwen OAuth credentials are not configured; set {} to the versioned OAuth credential JSON",
                        self.config.credentials_secret_ref
                    ),
                )
            })?;
        let credentials: QwenOAuthCredentials =
            serde_json::from_str(&encoded).map_err(|error| {
                QwenOAuthFailure::protocol(
                    "qwen_oauth_credentials_invalid",
                    format!("Qwen OAuth credential JSON is invalid: {error}"),
                )
            })?;
        validate_credentials(&credentials)?;
        Ok(credentials)
    }

    async fn refresh(
        &self,
        credentials: &QwenOAuthCredentials,
    ) -> Result<QwenOAuthCredentials, QwenOAuthFailure> {
        self.write_refresh_prepared(credentials)?;
        let result = async {
            let response = self
                .client
                .post(&self.config.token_endpoint)
                .header("Accept", "application/json")
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", credentials.refresh_token.as_str()),
                    ("client_id", self.config.client_id.as_str()),
                ])
                .send()
                .await
                .map_err(|error| {
                    QwenOAuthFailure::transport(
                        "qwen_oauth_refresh_send_failed",
                        format!("Qwen OAuth refresh could not be sent: {error}"),
                    )
                })?;
            let status = response.status().as_u16();
            let bytes = read_bounded(response, self.config.max_response_bytes).await?;
            if !(200..300).contains(&status) {
                return Err(QwenOAuthFailure::endpoint(
                    "qwen_oauth_refresh_rejected",
                    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504),
                    format!(
                        "Qwen OAuth refresh returned HTTP {status}; re-authenticate the exact interface credential"
                    ),
                    status,
                    &bytes,
                ));
            }
            let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                QwenOAuthFailure::protocol(
                    "qwen_oauth_refresh_json_invalid",
                    format!("Qwen OAuth refresh returned invalid JSON: {error}"),
                )
            })?;
            let access_token = required_string(&value, "access_token", "Qwen OAuth refresh")?;
            let refresh_token = optional_string(&value, "refresh_token", "Qwen OAuth refresh")?
                .unwrap_or_else(|| credentials.refresh_token.clone());
            let token_type = optional_string(&value, "token_type", "Qwen OAuth refresh")?
                .unwrap_or_else(|| credentials.token_type.clone());
            let resource_url = optional_string(&value, "resource_url", "Qwen OAuth refresh")?
                .unwrap_or_else(|| credentials.resource_url.clone());
            let expires_in = value
                .get("expires_in")
                .and_then(Value::as_u64)
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    QwenOAuthFailure::protocol(
                        "qwen_oauth_refresh_expiry_invalid",
                        "Qwen OAuth refresh returned no positive unsigned expires_in",
                    )
                })?;
            let now = unix_ms()?;
            let expires_at_unix_ms = now
                .checked_add(expires_in.checked_mul(1000).ok_or_else(|| {
                    QwenOAuthFailure::protocol(
                        "qwen_oauth_refresh_expiry_overflow",
                        "Qwen OAuth refresh expiry exceeds u64",
                    )
                })?)
                .ok_or_else(|| {
                    QwenOAuthFailure::protocol(
                        "qwen_oauth_refresh_expiry_overflow",
                        "Qwen OAuth refresh expiry exceeds u64",
                    )
                })?;
            let refreshed = QwenOAuthCredentials {
                schema_version: CREDENTIAL_SCHEMA_VERSION,
                access_token,
                refresh_token,
                token_type,
                expires_at_unix_ms,
                resource_url,
            };
            validate_credentials(&refreshed)?;
            Ok(refreshed)
        }
        .await;
        self.finish_refresh_attempt(credentials, result)
    }

    fn finish_refresh_attempt(
        &self,
        prior: &QwenOAuthCredentials,
        result: Result<QwenOAuthCredentials, QwenOAuthFailure>,
    ) -> Result<QwenOAuthCredentials, QwenOAuthFailure> {
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
        current: QwenOAuthCredentials,
    ) -> Result<QwenOAuthCredentials, QwenOAuthFailure> {
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
                Err(QwenOAuthFailure::unavailable(
                    "qwen_oauth_refresh_outcome_unknown",
                    "Qwen OAuth refresh may have rotated the credential before its response became durable; replace the exact interface credential",
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
        credentials: &QwenOAuthCredentials,
    ) -> Result<(), QwenOAuthFailure> {
        if self.read_refresh_journal()?.is_some() {
            return Err(QwenOAuthFailure::unavailable(
                "qwen_oauth_refresh_journal_occupied",
                "Qwen OAuth refresh journal already contains an unresolved operation",
            ));
        }
        self.write_refresh_journal(&RefreshJournal::Prepared {
            schema_version: CREDENTIAL_SCHEMA_VERSION,
            provider_interface: self.interface_id.clone(),
            source_fingerprint: credential_fingerprint(credentials)?,
        })
    }

    fn write_refresh_received(
        &self,
        prior: &QwenOAuthCredentials,
        credentials: &QwenOAuthCredentials,
    ) -> Result<(), QwenOAuthFailure> {
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
                    return Err(QwenOAuthFailure::protocol(
                        "qwen_oauth_refresh_journal_source_mismatch",
                        "Qwen OAuth refresh journal does not match the credential used for this refresh",
                    ));
                }
            }
            Some(RefreshJournal::Received { .. }) => {
                return Err(QwenOAuthFailure::protocol(
                    "qwen_oauth_refresh_journal_phase_invalid",
                    "Qwen OAuth refresh journal already contains a received credential",
                ));
            }
            None => {
                return Err(QwenOAuthFailure::protocol(
                    "qwen_oauth_refresh_journal_missing",
                    "Qwen OAuth refresh response arrived without a prepared private journal record",
                ));
            }
        }
        self.write_refresh_journal(&RefreshJournal::Received {
            schema_version: CREDENTIAL_SCHEMA_VERSION,
            provider_interface: self.interface_id.clone(),
            source_fingerprint,
            credentials: credentials.clone(),
        })
    }

    fn read_refresh_journal(&self) -> Result<Option<RefreshJournal>, QwenOAuthFailure> {
        let Some(encoded) = self.read_secret(&self.refresh_journal)? else {
            return Ok(None);
        };
        let journal: RefreshJournal = serde_json::from_str(&encoded).map_err(|error| {
            QwenOAuthFailure::protocol(
                "qwen_oauth_refresh_journal_invalid",
                format!("Qwen OAuth refresh journal JSON is invalid: {error}"),
            )
        })?;
        Ok(Some(journal))
    }

    fn write_refresh_journal(&self, journal: &RefreshJournal) -> Result<(), QwenOAuthFailure> {
        let encoded = serde_json::to_string(journal).map_err(|error| {
            QwenOAuthFailure::protocol(
                "qwen_oauth_refresh_journal_encode_failed",
                format!("Qwen OAuth refresh journal could not be encoded: {error}"),
            )
        })?;
        self.write_secret(&self.refresh_journal, &encoded)
    }

    fn write_credentials(
        &self,
        credentials: &QwenOAuthCredentials,
    ) -> Result<(), QwenOAuthFailure> {
        let encoded = serde_json::to_string(credentials).map_err(|error| {
            QwenOAuthFailure::protocol(
                "qwen_oauth_credentials_encode_failed",
                format!("Qwen OAuth credential JSON could not be encoded: {error}"),
            )
        })?;
        self.write_secret(&self.credentials, &encoded)
    }

    fn read_secret(&self, handle: &SecretHandle) -> Result<Option<String>, QwenOAuthFailure> {
        self.secrets
            .get(&handle.scope, &handle.name)
            .map_err(|error| {
                QwenOAuthFailure::unavailable(
                    "qwen_oauth_secret_read_failed",
                    format!(
                        "Qwen OAuth secret '{}/{}' could not be read: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }

    fn write_secret(&self, handle: &SecretHandle, value: &str) -> Result<(), QwenOAuthFailure> {
        self.secrets
            .set(&handle.scope, &handle.name, value)
            .map_err(|error| {
                QwenOAuthFailure::unavailable(
                    "qwen_oauth_secret_write_failed",
                    format!(
                        "Qwen OAuth secret '{}/{}' could not be written: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }

    fn delete_secret(&self, handle: &SecretHandle) -> Result<(), QwenOAuthFailure> {
        self.secrets
            .delete(&handle.scope, &handle.name)
            .map(|_| ())
            .map_err(|error| {
                QwenOAuthFailure::unavailable(
                    "qwen_oauth_secret_delete_failed",
                    format!(
                        "Qwen OAuth secret '{}/{}' could not be deleted: {error}",
                        handle.scope.name().unwrap_or("global"),
                        handle.name
                    ),
                )
            })
    }
}

fn validate_config(config: &QwenOAuthConfig) -> Result<(), QwenOAuthFailure> {
    if config.schema_version != CREDENTIAL_SCHEMA_VERSION {
        return Err(QwenOAuthFailure::protocol(
            "qwen_oauth_config_version_unsupported",
            format!(
                "Qwen OAuth config schema_version {} is unsupported",
                config.schema_version
            ),
        ));
    }
    for (value, surface) in [
        (&config.client_id, "client_id"),
        (&config.qwen_code_version, "qwen_code_version"),
    ] {
        if value.is_empty() || value.trim() != value {
            return Err(QwenOAuthFailure::protocol(
                "qwen_oauth_config_invalid",
                format!("Qwen OAuth {surface} must be non-empty without surrounding whitespace"),
            ));
        }
    }
    if config.refresh_skew_seconds == 0
        || config.request_timeout_ms == 0
        || config.max_response_bytes == 0
    {
        return Err(QwenOAuthFailure::protocol(
            "qwen_oauth_config_invalid",
            "Qwen OAuth refresh limits and skew must be positive",
        ));
    }
    let endpoint = Url::parse(&config.token_endpoint).map_err(|error| {
        QwenOAuthFailure::protocol(
            "qwen_oauth_token_endpoint_invalid",
            format!("Qwen OAuth token endpoint is invalid: {error}"),
        )
    })?;
    if endpoint.scheme() != "https"
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(QwenOAuthFailure::protocol(
            "qwen_oauth_token_endpoint_invalid",
            "Qwen OAuth token endpoint must be credential-free HTTPS without query or fragment",
        ));
    }
    Ok(())
}

fn validate_credentials(credentials: &QwenOAuthCredentials) -> Result<(), QwenOAuthFailure> {
    if credentials.schema_version != CREDENTIAL_SCHEMA_VERSION {
        return Err(QwenOAuthFailure::protocol(
            "qwen_oauth_credentials_version_unsupported",
            format!(
                "Qwen OAuth credential schema_version {} is unsupported",
                credentials.schema_version
            ),
        ));
    }
    for (value, surface) in [
        (&credentials.access_token, "access_token"),
        (&credentials.refresh_token, "refresh_token"),
    ] {
        if value.is_empty() || value.trim() != value || value.chars().any(char::is_whitespace) {
            return Err(QwenOAuthFailure::protocol(
                "qwen_oauth_credentials_invalid",
                format!("Qwen OAuth credential {surface} is malformed"),
            ));
        }
    }
    if credentials.token_type != "Bearer" || credentials.resource_url != "portal.qwen.ai" {
        return Err(QwenOAuthFailure::protocol(
            "qwen_oauth_credentials_invalid",
            "Qwen OAuth credential must declare Bearer and portal.qwen.ai",
        ));
    }
    if credentials.expires_at_unix_ms == 0 {
        return Err(QwenOAuthFailure::protocol(
            "qwen_oauth_credentials_invalid",
            "Qwen OAuth credential expiry must be positive",
        ));
    }
    Ok(())
}

fn credentials_expiring(
    credentials: &QwenOAuthCredentials,
    skew_seconds: u64,
) -> Result<bool, QwenOAuthFailure> {
    let threshold = unix_ms()?.saturating_add(skew_seconds.saturating_mul(1000));
    Ok(threshold >= credentials.expires_at_unix_ms)
}

fn parse_secret_ref(reference: &str) -> Result<SecretHandle, QwenOAuthFailure> {
    let raw = reference.strip_prefix("secret://").ok_or_else(|| {
        QwenOAuthFailure::protocol(
            "qwen_oauth_secret_ref_invalid",
            "Qwen OAuth secret ref must start with secret://",
        )
    })?;
    let (scope, name) = raw.split_once('/').ok_or_else(|| {
        QwenOAuthFailure::protocol(
            "qwen_oauth_secret_ref_invalid",
            "Qwen OAuth secret ref must be secret://<scope>/<name>",
        )
    })?;
    if name.contains('/') {
        return Err(QwenOAuthFailure::protocol(
            "qwen_oauth_secret_ref_invalid",
            "Qwen OAuth secret ref must name one scoped secret",
        ));
    }
    Ok(SecretHandle {
        scope: SecretScope::named(scope.to_string()).map_err(|error| {
            QwenOAuthFailure::protocol("qwen_oauth_secret_ref_invalid", error.to_string())
        })?,
        name: SecretName::new(name).map_err(|error| {
            QwenOAuthFailure::protocol("qwen_oauth_secret_ref_invalid", error.to_string())
        })?,
    })
}

fn required_string(value: &Value, field: &str, surface: &str) -> Result<String, QwenOAuthFailure> {
    optional_string(value, field, surface)?.ok_or_else(|| {
        QwenOAuthFailure::protocol(
            "qwen_oauth_refresh_field_missing",
            format!("{surface} returned no {field}"),
        )
    })
}

fn optional_string(
    value: &Value,
    field: &str,
    surface: &str,
) -> Result<Option<String>, QwenOAuthFailure> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() && value.trim() == value => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(QwenOAuthFailure::protocol(
            "qwen_oauth_refresh_field_invalid",
            format!("{surface} returned malformed {field}"),
        )),
    }
}

async fn read_bounded(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, QwenOAuthFailure> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        QwenOAuthFailure::transport(
            "qwen_oauth_refresh_read_failed",
            format!("Qwen OAuth refresh response could not be read: {error}"),
        )
    })? {
        let next = bytes.len().checked_add(chunk.len()).ok_or_else(|| {
            QwenOAuthFailure::protocol(
                "qwen_oauth_refresh_response_too_large",
                "Qwen OAuth refresh response length overflowed",
            )
        })?;
        if next > maximum {
            return Err(QwenOAuthFailure::protocol(
                "qwen_oauth_refresh_response_too_large",
                format!("Qwen OAuth refresh response exceeded {maximum} bytes"),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn unix_ms() -> Result<u64, QwenOAuthFailure> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            QwenOAuthFailure::unavailable(
                "qwen_oauth_clock_invalid",
                format!("system clock precedes Unix epoch: {error}"),
            )
        })?
        .as_millis();
    u64::try_from(millis).map_err(|_| {
        QwenOAuthFailure::unavailable(
            "qwen_oauth_clock_invalid",
            "system clock milliseconds exceed u64",
        )
    })
}

fn qwen_user_agent(version: &str) -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" if os == "darwin" => "arm64",
        other => other,
    };
    format!("QwenCode/{version} ({os}; {arch})")
}
