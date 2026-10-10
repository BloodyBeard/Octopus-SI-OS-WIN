use std::collections::BTreeSet;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use octopus_interfaces::InterfaceDescriptor;
use octopus_secrets::{
    LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind, SecretsManager,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex;

const AZURE_ENTRA_CONFIG_KEY: &str = "azure_entra";
const CLIENT_CREDENTIALS_GRANT: &str = "client_credentials";
const JWT_BEARER_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";
const MAX_ACCESS_TOKEN_BYTES: usize = 64 * 1024;
const MAX_TOKEN_LIFETIME_SECONDS: u64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AzureCredentialSourceKind {
    ClientSecret,
    WorkloadIdentity,
    ManagedIdentityAppService,
    ManagedIdentityImds,
    AzureCli,
}

impl AzureCredentialSourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClientSecret => "client_secret",
            Self::WorkloadIdentity => "workload_identity",
            Self::ManagedIdentityAppService => "managed_identity_app_service",
            Self::ManagedIdentityImds => "managed_identity_imds",
            Self::AzureCli => "azure_cli",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AzureAuthFailureClass {
    Contract,
    Unavailable,
    Transport,
    Endpoint,
}

impl AzureAuthFailureClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Contract => "contract",
            Self::Unavailable => "unavailable",
            Self::Transport => "transport",
            Self::Endpoint => "endpoint",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AzureCredentialSourceFailureEvidence {
    pub index: usize,
    pub source: AzureCredentialSourceKind,
    pub class: AzureAuthFailureClass,
    pub code: &'static str,
    pub retryable: bool,
    pub status: Option<u16>,
    pub process_exit_code: Option<i32>,
    pub response_bytes: Option<usize>,
    pub response_blake3: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AzureAuthFailureEvidence {
    pub class: &'static str,
    pub code: &'static str,
    pub retryable: bool,
    pub status: Option<u16>,
    pub process_exit_code: Option<i32>,
    pub response_bytes: Option<usize>,
    pub response_blake3: Option<String>,
    pub source_failures: Vec<AzureCredentialSourceFailureEvidence>,
}

#[derive(Clone, Debug)]
pub struct AzureAuthFailure {
    pub class: &'static str,
    pub code: &'static str,
    pub retryable: bool,
    pub status: Option<u16>,
    pub process_exit_code: Option<i32>,
    pub detail: String,
    pub response_bytes: Option<usize>,
    pub response_blake3: Option<String>,
    pub source_failures: Box<Vec<AzureCredentialSourceFailureEvidence>>,
}

impl AzureAuthFailure {
    fn new(
        class: AzureAuthFailureClass,
        code: &'static str,
        retryable: bool,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            class: class.as_str(),
            code,
            retryable,
            status: None,
            process_exit_code: None,
            detail: detail.into(),
            response_bytes: None,
            response_blake3: None,
            source_failures: Box::default(),
        }
    }

    fn contract(code: &'static str, detail: impl Into<String>) -> Self {
        Self::new(AzureAuthFailureClass::Contract, code, false, detail)
    }

    fn unavailable(code: &'static str, detail: impl Into<String>) -> Self {
        Self::new(AzureAuthFailureClass::Unavailable, code, false, detail)
    }

    fn transport(code: &'static str, retryable: bool, detail: impl Into<String>) -> Self {
        Self::new(AzureAuthFailureClass::Transport, code, retryable, detail)
    }

    fn response(
        class: AzureAuthFailureClass,
        code: &'static str,
        status: u16,
        retryable: bool,
        response_body: &[u8],
        detail: impl Into<String>,
    ) -> Self {
        Self {
            class: class.as_str(),
            code,
            retryable,
            status: Some(status),
            process_exit_code: None,
            detail: detail.into(),
            response_bytes: Some(response_body.len()),
            response_blake3: Some(blake3::hash(response_body).to_hex().to_string()),
            source_failures: Box::default(),
        }
    }

    fn process_rejected(
        code: &'static str,
        process_exit_code: Option<i32>,
        response_bytes: usize,
        response_blake3: String,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            class: AzureAuthFailureClass::Endpoint.as_str(),
            code,
            retryable: false,
            status: None,
            process_exit_code,
            detail: detail.into(),
            response_bytes: Some(response_bytes),
            response_blake3: Some(response_blake3),
            source_failures: Box::default(),
        }
    }

    fn class_kind(&self) -> AzureAuthFailureClass {
        match self.class {
            "unavailable" => AzureAuthFailureClass::Unavailable,
            "transport" => AzureAuthFailureClass::Transport,
            "endpoint" => AzureAuthFailureClass::Endpoint,
            _ => AzureAuthFailureClass::Contract,
        }
    }

    fn source_evidence(
        &self,
        index: usize,
        source: AzureCredentialSourceKind,
    ) -> AzureCredentialSourceFailureEvidence {
        AzureCredentialSourceFailureEvidence {
            index,
            source,
            class: self.class_kind(),
            code: self.code,
            retryable: self.retryable,
            status: self.status,
            process_exit_code: self.process_exit_code,
            response_bytes: self.response_bytes,
            response_blake3: self.response_blake3.clone(),
        }
    }

    fn with_source(mut self, index: usize, source: AzureCredentialSourceKind) -> Self {
        self.source_failures
            .push(self.source_evidence(index, source));
        self
    }

    fn aggregate(evidence: Vec<AzureCredentialSourceFailureEvidence>) -> Self {
        let summary = evidence
            .iter()
            .map(|failure| {
                format!(
                    "{}:{}:{}",
                    failure.index,
                    failure.source.as_str(),
                    failure.code
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        Self {
            class: "unavailable",
            code: "azure_entra_credential_chain_exhausted",
            retryable: evidence.iter().any(|failure| failure.retryable),
            status: None,
            process_exit_code: None,
            detail: format!("Azure Entra credential chain exhausted [{summary}]"),
            response_bytes: None,
            response_blake3: None,
            source_failures: Box::new(evidence),
        }
    }

    pub fn evidence(&self) -> AzureAuthFailureEvidence {
        AzureAuthFailureEvidence {
            class: self.class,
            code: self.code,
            retryable: self.retryable,
            status: self.status,
            process_exit_code: self.process_exit_code,
            response_bytes: self.response_bytes,
            response_blake3: self.response_blake3.clone(),
            source_failures: self.source_failures.as_ref().clone(),
        }
    }
}

impl std::fmt::Display for AzureAuthFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for AzureAuthFailure {}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AzureEntraConfig {
    authority_host: String,
    scope: String,
    credential_chain: Vec<CredentialSourceConfig>,
    request_timeout_ms: u64,
    max_response_bytes: usize,
    refresh_margin_seconds: u64,
    retryable_statuses: Vec<u16>,
    retry_backoff_ms: Vec<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CredentialSourceConfig {
    ClientSecret {
        tenant_id_secret_ref: String,
        client_id_secret_ref: String,
        client_secret_ref: String,
        continue_on: Vec<AzureAuthFailureClass>,
    },
    WorkloadIdentity {
        tenant_id_secret_ref: String,
        client_id_secret_ref: String,
        federated_token_file_env: String,
        max_assertion_bytes: usize,
        continue_on: Vec<AzureAuthFailureClass>,
    },
    ManagedIdentityAppService {
        protocol: AppServiceProtocol,
        endpoint_env: String,
        header_env: String,
        resource: String,
        allowed_paths: Vec<String>,
        client_id_secret_ref: Option<String>,
        unavailable_statuses: Vec<u16>,
        continue_on: Vec<AzureAuthFailureClass>,
    },
    ManagedIdentityImds {
        endpoint: String,
        resource: String,
        client_id_secret_ref: Option<String>,
        unavailable_statuses: Vec<u16>,
        continue_on: Vec<AzureAuthFailureClass>,
    },
    AzureCli {
        executable: String,
        continue_on: Vec<AzureAuthFailureClass>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AppServiceProtocol {
    IdentityHeader2019,
    MsiSecret2017,
}

#[derive(Clone)]
struct SecretHandle {
    scope: SecretScope,
    name: SecretName,
}

#[derive(Clone)]
enum CredentialSource {
    ClientSecret {
        tenant_id: SecretHandle,
        client_id: SecretHandle,
        client_secret: SecretHandle,
        continue_on: BTreeSet<AzureAuthFailureClass>,
    },
    WorkloadIdentity {
        tenant_id: SecretHandle,
        client_id: SecretHandle,
        federated_token_file_env: String,
        max_assertion_bytes: usize,
        continue_on: BTreeSet<AzureAuthFailureClass>,
    },
    ManagedIdentityAppService {
        protocol: AppServiceProtocol,
        endpoint_env: String,
        header_env: String,
        resource: String,
        allowed_paths: BTreeSet<String>,
        client_id: Option<SecretHandle>,
        unavailable_statuses: BTreeSet<u16>,
        continue_on: BTreeSet<AzureAuthFailureClass>,
    },
    ManagedIdentityImds {
        endpoint: Url,
        resource: String,
        client_id: Option<SecretHandle>,
        unavailable_statuses: BTreeSet<u16>,
        continue_on: BTreeSet<AzureAuthFailureClass>,
    },
    AzureCli {
        executable: String,
        continue_on: BTreeSet<AzureAuthFailureClass>,
    },
}

impl CredentialSource {
    fn kind(&self) -> AzureCredentialSourceKind {
        match self {
            Self::ClientSecret { .. } => AzureCredentialSourceKind::ClientSecret,
            Self::WorkloadIdentity { .. } => AzureCredentialSourceKind::WorkloadIdentity,
            Self::ManagedIdentityAppService { .. } => {
                AzureCredentialSourceKind::ManagedIdentityAppService
            }
            Self::ManagedIdentityImds { .. } => AzureCredentialSourceKind::ManagedIdentityImds,
            Self::AzureCli { .. } => AzureCredentialSourceKind::AzureCli,
        }
    }

    fn continue_on(&self) -> &BTreeSet<AzureAuthFailureClass> {
        match self {
            Self::ClientSecret { continue_on, .. }
            | Self::WorkloadIdentity { continue_on, .. }
            | Self::ManagedIdentityAppService { continue_on, .. }
            | Self::ManagedIdentityImds { continue_on, .. }
            | Self::AzureCli { continue_on, .. } => continue_on,
        }
    }
}

#[derive(Clone)]
struct ResolvedAzureEntraConfig {
    authority_host: String,
    scope: String,
    credential_chain: Vec<CredentialSource>,
    request_timeout_ms: u64,
    max_response_bytes: usize,
    refresh_margin_seconds: u64,
    retryable_statuses: BTreeSet<u16>,
    retry_backoff_ms: Vec<u64>,
}

struct CachedToken {
    access_token: String,
    refresh_at: Instant,
    source: AzureCredentialSourceKind,
}

#[derive(Default)]
struct TokenCache {
    token: Option<CachedToken>,
}

impl TokenCache {
    fn access_token(&self, now: Instant) -> Option<String> {
        self.token
            .as_ref()
            .filter(|token| now < token.refresh_at)
            .map(|token| token.access_token.clone())
    }

    fn replace(&mut self, token: CachedToken) {
        self.token = Some(token);
    }

    fn invalidate(&mut self) {
        self.token = None;
    }
}

#[derive(Clone)]
pub struct AzureEntraCredentialManager {
    config: ResolvedAzureEntraConfig,
    credential_source_fingerprint: String,
    secrets: SecretsManager,
    client: reqwest::Client,
    cache: Arc<Mutex<TokenCache>>,
}

impl AzureEntraCredentialManager {
    pub fn from_descriptor(
        descriptor: &InterfaceDescriptor,
        activation_secrets_home: &Path,
    ) -> Result<Option<Self>, AzureAuthFailure> {
        let metadata = descriptor
            .interface_driver_config
            .as_ref()
            .ok_or_else(|| {
                AzureAuthFailure::contract(
                    "azure_entra_driver_config_missing",
                    format!("interface '{}' has no driver config", descriptor.id),
                )
            })?
            .private_config
            .get(AZURE_ENTRA_CONFIG_KEY)
            .cloned();
        let Some(metadata) = metadata else {
            return Ok(None);
        };
        let config: AzureEntraConfig = serde_json::from_value(metadata).map_err(|error| {
            AzureAuthFailure::contract(
                "azure_entra_config_invalid",
                format!(
                    "interface '{}' Azure Entra config is invalid: {error}",
                    descriptor.id
                ),
            )
        })?;
        let credential_source_fingerprint =
            credential_source_fingerprint(descriptor.id.as_str(), &config)?;
        let config = resolve_config(config, descriptor.id.as_str())?;
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
                AzureAuthFailure::unavailable(
                    "azure_entra_client_build_failed",
                    format!("Azure Entra HTTP client could not be built: {error}"),
                )
            })?;
        Ok(Some(Self {
            config,
            credential_source_fingerprint,
            secrets,
            client,
            cache: Arc::new(Mutex::new(TokenCache::default())),
        }))
    }

    pub async fn access_token(&self) -> Result<String, AzureAuthFailure> {
        let mut cache = self.cache.lock().await;
        if let Some(access_token) = cache.access_token(Instant::now()) {
            return Ok(access_token);
        }
        let token = self.fetch_token().await?;
        let access_token = token.access_token.clone();
        cache.replace(token);
        Ok(access_token)
    }

    pub fn credential_source_fingerprint(&self) -> &str {
        &self.credential_source_fingerprint
    }

    pub async fn invalidate(&self) {
        self.cache.lock().await.invalidate();
    }

    async fn fetch_token(&self) -> Result<CachedToken, AzureAuthFailure> {
        let mut evidence = Vec::new();
        for (index, source) in self.config.credential_chain.iter().enumerate() {
            match self.fetch_from_source(source).await {
                Ok(mut token) => {
                    token.source = source.kind();
                    return Ok(token);
                }
                Err(failure) => {
                    let source_evidence = failure.source_evidence(index, source.kind());
                    if source.continue_on().contains(&failure.class_kind()) {
                        evidence.push(source_evidence);
                        continue;
                    }
                    let mut terminal = failure.with_source(index, source.kind());
                    terminal.source_failures.splice(0..0, evidence);
                    return Err(terminal);
                }
            }
        }
        Err(AzureAuthFailure::aggregate(evidence))
    }

    async fn fetch_from_source(
        &self,
        source: &CredentialSource,
    ) -> Result<CachedToken, AzureAuthFailure> {
        match source {
            CredentialSource::ClientSecret {
                tenant_id,
                client_id,
                client_secret,
                ..
            } => {
                let tenant_id = self.read_required_secret(tenant_id, "tenant id")?;
                validate_tenant_id(&tenant_id)?;
                let client_id = self.read_required_secret(client_id, "client id")?;
                validate_client_id(&client_id)?;
                let client_secret = self.read_required_secret(client_secret, "client secret")?;
                validate_secret_value(&client_secret, "client secret")?;
                let request = client_secret_request(
                    &self.config.authority_host,
                    &self.config.scope,
                    &tenant_id,
                    &client_id,
                    &client_secret,
                )?;
                self.fetch_http_token(request, &BTreeSet::new()).await
            }
            CredentialSource::WorkloadIdentity {
                tenant_id,
                client_id,
                federated_token_file_env,
                max_assertion_bytes,
                ..
            } => {
                let tenant_id = self.read_required_secret(tenant_id, "tenant id")?;
                validate_tenant_id(&tenant_id)?;
                let client_id = self.read_required_secret(client_id, "client id")?;
                validate_client_id(&client_id)?;
                let assertion =
                    read_federated_assertion(federated_token_file_env, *max_assertion_bytes)?;
                let request = workload_identity_request(
                    &self.config.authority_host,
                    &self.config.scope,
                    &tenant_id,
                    &client_id,
                    &assertion,
                )?;
                self.fetch_http_token(request, &BTreeSet::new()).await
            }
            CredentialSource::ManagedIdentityAppService {
                protocol,
                endpoint_env,
                header_env,
                resource,
                allowed_paths,
                client_id,
                unavailable_statuses,
                ..
            } => {
                let endpoint = required_environment(endpoint_env, "managed identity endpoint")?;
                let header = required_environment(header_env, "managed identity header")?;
                validate_secret_value(&header, "managed identity header")?;
                let endpoint = app_service_endpoint(&endpoint, allowed_paths)?;
                let client_id = self.read_optional_client_id(client_id.as_ref())?;
                let request = app_service_request(
                    endpoint,
                    *protocol,
                    header,
                    resource,
                    client_id.as_deref(),
                );
                self.fetch_http_token(request, unavailable_statuses).await
            }
            CredentialSource::ManagedIdentityImds {
                endpoint,
                resource,
                client_id,
                unavailable_statuses,
                ..
            } => {
                let client_id = self.read_optional_client_id(client_id.as_ref())?;
                let request = imds_request(endpoint.clone(), resource, client_id.as_deref());
                self.fetch_http_token(request, unavailable_statuses).await
            }
            CredentialSource::AzureCli { executable, .. } => {
                self.fetch_azure_cli_token(executable).await
            }
        }
    }

    async fn fetch_http_token(
        &self,
        request: TokenHttpRequest,
        unavailable_statuses: &BTreeSet<u16>,
    ) -> Result<CachedToken, AzureAuthFailure> {
        let mut last_failure = None;
        for attempt in 0..=self.config.retry_backoff_ms.len() {
            match request.send(&self.client).await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let bytes =
                        read_bounded_response(response, self.config.max_response_bytes).await?;
                    if (200..300).contains(&status) {
                        return parse_relative_token_response(
                            &bytes,
                            self.config.refresh_margin_seconds,
                        );
                    }
                    if unavailable_statuses.contains(&status) {
                        return Err(AzureAuthFailure::response(
                            AzureAuthFailureClass::Unavailable,
                            "azure_entra_credential_unavailable",
                            status,
                            false,
                            &bytes,
                            format!("Azure credential endpoint returned unavailable HTTP {status}"),
                        ));
                    }
                    let retryable = self.config.retryable_statuses.contains(&status);
                    let failure = AzureAuthFailure::response(
                        AzureAuthFailureClass::Endpoint,
                        "azure_entra_token_rejected",
                        status,
                        retryable,
                        &bytes,
                        format!("Azure credential endpoint returned HTTP {status}"),
                    );
                    if !retryable || attempt == self.config.retry_backoff_ms.len() {
                        return Err(failure);
                    }
                    last_failure = Some(failure);
                }
                Err(error) => {
                    let failure = AzureAuthFailure::transport(
                        "azure_entra_token_send_failed",
                        true,
                        format!(
                            "Azure credential request could not be sent (timeout={}, connect={}, request={})",
                            error.is_timeout(),
                            error.is_connect(),
                            error.is_request()
                        ),
                    );
                    if attempt == self.config.retry_backoff_ms.len() {
                        return Err(failure);
                    }
                    last_failure = Some(failure);
                }
            }
            tokio::time::sleep(Duration::from_millis(self.config.retry_backoff_ms[attempt])).await;
        }
        Err(last_failure.unwrap_or_else(|| {
            AzureAuthFailure::unavailable(
                "azure_entra_token_unavailable",
                "Azure credential request exhausted its bounded attempts",
            )
        }))
    }

    async fn fetch_azure_cli_token(
        &self,
        executable: &str,
    ) -> Result<CachedToken, AzureAuthFailure> {
        let mut command = Command::new(executable);
        command
            .arg("account")
            .arg("get-access-token")
            .arg("--scope")
            .arg(&self.config.scope)
            .arg("--output")
            .arg("json")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AzureAuthFailure::unavailable(
                    "azure_entra_cli_unavailable",
                    "declared Azure CLI executable is unavailable",
                )
            } else {
                AzureAuthFailure::transport(
                    "azure_entra_cli_spawn_failed",
                    false,
                    format!("Azure CLI could not be started: {error}"),
                )
            }
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AzureAuthFailure::contract(
                "azure_entra_cli_pipe_missing",
                "Azure CLI stdout pipe is unavailable",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AzureAuthFailure::contract(
                "azure_entra_cli_pipe_missing",
                "Azure CLI stderr pipe is unavailable",
            )
        })?;
        let execution = async {
            let (stdout, stderr, status) = tokio::join!(
                read_bounded_pipe(stdout, self.config.max_response_bytes),
                read_bounded_pipe(stderr, self.config.max_response_bytes),
                child.wait()
            );
            (stdout, stderr, status)
        };
        let (stdout, stderr, status) = tokio::time::timeout(
            Duration::from_millis(self.config.request_timeout_ms),
            execution,
        )
        .await
        .map_err(|_| {
            AzureAuthFailure::transport(
                "azure_entra_cli_timeout",
                true,
                "Azure CLI token request exceeded its declared timeout",
            )
        })?;
        let stdout = stdout.map_err(|error| cli_io_failure("stdout", error))?;
        let stderr = stderr.map_err(|error| cli_io_failure("stderr", error))?;
        let status = status.map_err(|error| {
            AzureAuthFailure::transport(
                "azure_entra_cli_wait_failed",
                false,
                format!("Azure CLI status could not be read: {error}"),
            )
        })?;
        if !status.success() {
            return Err(AzureAuthFailure::process_rejected(
                "azure_entra_cli_rejected",
                status.code(),
                stderr.len(),
                blake3::hash(&stderr).to_hex().to_string(),
                "Azure CLI rejected the token request",
            ));
        }
        parse_cli_token_response(
            &stdout,
            self.config.refresh_margin_seconds,
            SystemTime::now(),
        )
    }

    fn read_required_secret(
        &self,
        handle: &SecretHandle,
        surface: &str,
    ) -> Result<String, AzureAuthFailure> {
        self.secrets
            .get(&handle.scope, &handle.name)
            .map_err(|_| {
                AzureAuthFailure::unavailable(
                    "azure_entra_secret_read_failed",
                    format!("Azure Entra {surface} scoped secret could not be read"),
                )
            })?
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                AzureAuthFailure::unavailable(
                    "azure_entra_secret_missing",
                    format!("Azure Entra {surface} scoped secret is not configured"),
                )
            })
    }

    fn read_optional_client_id(
        &self,
        handle: Option<&SecretHandle>,
    ) -> Result<Option<String>, AzureAuthFailure> {
        let Some(handle) = handle else {
            return Ok(None);
        };
        let client_id = self.read_required_secret(handle, "managed identity client id")?;
        validate_client_id(&client_id)?;
        Ok(Some(client_id))
    }
}

fn credential_source_fingerprint(
    exact_interface_id: &str,
    config: &AzureEntraConfig,
) -> Result<String, AzureAuthFailure> {
    let encoded = serde_json::to_vec(config).map_err(|error| {
        AzureAuthFailure::contract(
            "azure_entra_source_fingerprint_failed",
            format!("Azure Entra credential source config could not be encoded: {error}"),
        )
    })?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"octopus.provider.azure_entra.source.v1");
    hasher.update(&(exact_interface_id.len() as u64).to_le_bytes());
    hasher.update(exact_interface_id.as_bytes());
    hasher.update(&(encoded.len() as u64).to_le_bytes());
    hasher.update(&encoded);
    Ok(hasher.finalize().to_hex().to_string())
}

#[derive(Clone)]
enum TokenHttpRequest {
    Form {
        url: Url,
        fields: Vec<(String, String)>,
    },
    Get {
        url: Url,
        query: Vec<(String, String)>,
        headers: Vec<(&'static str, String)>,
    },
}

impl TokenHttpRequest {
    async fn send(&self, client: &reqwest::Client) -> Result<reqwest::Response, reqwest::Error> {
        match self {
            Self::Form { url, fields } => {
                client
                    .post(url.clone())
                    .header(reqwest::header::ACCEPT, "application/json")
                    .form(fields)
                    .send()
                    .await
            }
            Self::Get {
                url,
                query,
                headers,
            } => {
                let mut request = client
                    .get(url.clone())
                    .header(reqwest::header::ACCEPT, "application/json")
                    .query(query);
                for (name, value) in headers {
                    request = request.header(*name, value);
                }
                request.send().await
            }
        }
    }
}

fn resolve_config(
    config: AzureEntraConfig,
    exact_interface_id: &str,
) -> Result<ResolvedAzureEntraConfig, AzureAuthFailure> {
    validate_authority_host(&config.authority_host)?;
    validate_scope(&config.scope)?;
    if config.credential_chain.is_empty() || config.credential_chain.len() > 8 {
        return Err(AzureAuthFailure::contract(
            "azure_entra_chain_invalid",
            "Azure Entra credential_chain must contain 1..=8 explicit sources",
        ));
    }
    if config.request_timeout_ms == 0 || config.request_timeout_ms > 60_000 {
        return Err(AzureAuthFailure::contract(
            "azure_entra_timeout_invalid",
            "Azure Entra request_timeout_ms must be within 1..=60000",
        ));
    }
    if config.max_response_bytes == 0 || config.max_response_bytes > 1024 * 1024 {
        return Err(AzureAuthFailure::contract(
            "azure_entra_response_limit_invalid",
            "Azure Entra max_response_bytes must be within 1..=1048576",
        ));
    }
    if config.refresh_margin_seconds == 0 || config.refresh_margin_seconds > 3600 {
        return Err(AzureAuthFailure::contract(
            "azure_entra_refresh_margin_invalid",
            "Azure Entra refresh_margin_seconds must be within 1..=3600",
        ));
    }
    if config.retry_backoff_ms.len() > 4
        || config
            .retry_backoff_ms
            .iter()
            .any(|delay| *delay == 0 || *delay > 10_000)
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_retry_backoff_invalid",
            "Azure Entra retry_backoff_ms must contain at most four values in 1..=10000",
        ));
    }
    let retryable_statuses = checked_statuses(
        &config.retryable_statuses,
        "azure_entra_retry_status_invalid",
    )?;
    if retryable_statuses
        .iter()
        .any(|status| !matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504))
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_retry_status_invalid",
            "Azure Entra retryable_statuses contains an unsupported status",
        ));
    }
    let mut credential_chain = Vec::with_capacity(config.credential_chain.len());
    for source in config.credential_chain {
        credential_chain.push(resolve_source(source, exact_interface_id, &config.scope)?);
    }
    Ok(ResolvedAzureEntraConfig {
        authority_host: config.authority_host,
        scope: config.scope,
        credential_chain,
        request_timeout_ms: config.request_timeout_ms,
        max_response_bytes: config.max_response_bytes,
        refresh_margin_seconds: config.refresh_margin_seconds,
        retryable_statuses,
        retry_backoff_ms: config.retry_backoff_ms,
    })
}

fn resolve_source(
    source: CredentialSourceConfig,
    exact_interface_id: &str,
    scope: &str,
) -> Result<CredentialSource, AzureAuthFailure> {
    match source {
        CredentialSourceConfig::ClientSecret {
            tenant_id_secret_ref,
            client_id_secret_ref,
            client_secret_ref,
            continue_on,
        } => {
            let tenant_id = exact_secret_handle(&tenant_id_secret_ref, exact_interface_id)?;
            let client_id = exact_secret_handle(&client_id_secret_ref, exact_interface_id)?;
            let client_secret = exact_secret_handle(&client_secret_ref, exact_interface_id)?;
            require_distinct_handles(&[&tenant_id, &client_id, &client_secret])?;
            Ok(CredentialSource::ClientSecret {
                tenant_id,
                client_id,
                client_secret,
                continue_on: checked_continue_on(continue_on)?,
            })
        }
        CredentialSourceConfig::WorkloadIdentity {
            tenant_id_secret_ref,
            client_id_secret_ref,
            federated_token_file_env,
            max_assertion_bytes,
            continue_on,
        } => {
            let tenant_id = exact_secret_handle(&tenant_id_secret_ref, exact_interface_id)?;
            let client_id = exact_secret_handle(&client_id_secret_ref, exact_interface_id)?;
            require_distinct_handles(&[&tenant_id, &client_id])?;
            validate_env_name(&federated_token_file_env)?;
            if max_assertion_bytes == 0 || max_assertion_bytes > MAX_ACCESS_TOKEN_BYTES {
                return Err(AzureAuthFailure::contract(
                    "azure_entra_assertion_limit_invalid",
                    "Azure workload identity max_assertion_bytes must be within 1..=65536",
                ));
            }
            Ok(CredentialSource::WorkloadIdentity {
                tenant_id,
                client_id,
                federated_token_file_env,
                max_assertion_bytes,
                continue_on: checked_continue_on(continue_on)?,
            })
        }
        CredentialSourceConfig::ManagedIdentityAppService {
            protocol,
            endpoint_env,
            header_env,
            resource,
            allowed_paths,
            client_id_secret_ref,
            unavailable_statuses,
            continue_on,
        } => {
            validate_env_name(&endpoint_env)?;
            validate_env_name(&header_env)?;
            validate_resource_scope(&resource, scope)?;
            let allowed_paths = checked_paths(&allowed_paths)?;
            let client_id = client_id_secret_ref
                .as_deref()
                .map(|reference| exact_secret_handle(reference, exact_interface_id))
                .transpose()?;
            Ok(CredentialSource::ManagedIdentityAppService {
                protocol,
                endpoint_env,
                header_env,
                resource,
                allowed_paths,
                client_id,
                unavailable_statuses: checked_statuses(
                    &unavailable_statuses,
                    "azure_entra_unavailable_status_invalid",
                )?,
                continue_on: checked_continue_on(continue_on)?,
            })
        }
        CredentialSourceConfig::ManagedIdentityImds {
            endpoint,
            resource,
            client_id_secret_ref,
            unavailable_statuses,
            continue_on,
        } => {
            validate_resource_scope(&resource, scope)?;
            let endpoint = imds_endpoint(&endpoint)?;
            let client_id = client_id_secret_ref
                .as_deref()
                .map(|reference| exact_secret_handle(reference, exact_interface_id))
                .transpose()?;
            Ok(CredentialSource::ManagedIdentityImds {
                endpoint,
                resource,
                client_id,
                unavailable_statuses: checked_statuses(
                    &unavailable_statuses,
                    "azure_entra_unavailable_status_invalid",
                )?,
                continue_on: checked_continue_on(continue_on)?,
            })
        }
        CredentialSourceConfig::AzureCli {
            executable,
            continue_on,
        } => {
            if executable != "az" {
                return Err(AzureAuthFailure::contract(
                    "azure_entra_cli_executable_invalid",
                    "Azure CLI source executable must be the authority-declared command 'az'",
                ));
            }
            Ok(CredentialSource::AzureCli {
                executable,
                continue_on: checked_continue_on(continue_on)?,
            })
        }
    }
}

fn checked_continue_on(
    values: Vec<AzureAuthFailureClass>,
) -> Result<BTreeSet<AzureAuthFailureClass>, AzureAuthFailure> {
    let values = values.into_iter().collect::<BTreeSet<_>>();
    if values.contains(&AzureAuthFailureClass::Contract) {
        return Err(AzureAuthFailure::contract(
            "azure_entra_continuation_invalid",
            "Azure credential sources cannot continue after contract failures",
        ));
    }
    Ok(values)
}

fn checked_statuses(values: &[u16], code: &'static str) -> Result<BTreeSet<u16>, AzureAuthFailure> {
    let statuses = values.iter().copied().collect::<BTreeSet<_>>();
    if statuses.len() != values.len() || statuses.iter().any(|status| !(400..=599).contains(status))
    {
        return Err(AzureAuthFailure::contract(
            code,
            "Azure credential status list contains an invalid or duplicate status",
        ));
    }
    Ok(statuses)
}

fn checked_paths(values: &[String]) -> Result<BTreeSet<String>, AzureAuthFailure> {
    let paths = values.iter().cloned().collect::<BTreeSet<_>>();
    if paths.is_empty()
        || paths.len() != values.len()
        || paths.iter().any(|path| {
            !path.starts_with('/')
                || path.len() > 512
                || path.contains(['?', '#', '\r', '\n'])
                || path.trim() != path
        })
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_managed_paths_invalid",
            "Azure App Service managed identity allowed_paths must be distinct absolute URL paths",
        ));
    }
    Ok(paths)
}

fn validate_authority_host(raw: &str) -> Result<(), AzureAuthFailure> {
    let url = Url::parse(raw).map_err(|error| {
        AzureAuthFailure::contract(
            "azure_entra_authority_invalid",
            format!("Azure Entra authority_host is invalid: {error}"),
        )
    })?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || !matches!(url.path(), "" | "/")
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_authority_invalid",
            "Azure Entra authority_host must be an HTTPS origin without credentials, path, query or fragment",
        ));
    }
    Ok(())
}

fn validate_scope(raw: &str) -> Result<(), AzureAuthFailure> {
    let url = Url::parse(raw).map_err(|error| {
        AzureAuthFailure::contract(
            "azure_entra_scope_invalid",
            format!("Azure Entra scope is invalid: {error}"),
        )
    })?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || !url.path().ends_with("/.default")
        || raw.trim() != raw
        || raw.contains(char::is_whitespace)
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_scope_invalid",
            "Azure Entra scope must be one trimmed HTTPS /.default resource scope",
        ));
    }
    Ok(())
}

fn validate_resource_scope(resource: &str, scope: &str) -> Result<(), AzureAuthFailure> {
    let url = Url::parse(resource).map_err(|error| {
        AzureAuthFailure::contract(
            "azure_entra_resource_invalid",
            format!("Azure managed identity resource is invalid: {error}"),
        )
    })?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || !matches!(url.path(), "" | "/")
        || resource.trim() != resource
        || format!("{}/.default", resource.trim_end_matches('/')) != scope
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_resource_scope_mismatch",
            "Azure managed identity resource must exactly correspond to the declared scope",
        ));
    }
    Ok(())
}

fn validate_env_name(name: &str) -> Result<(), AzureAuthFailure> {
    if name.is_empty()
        || name.len() > 128
        || name.trim() != name
        || !name
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit())
        || name.as_bytes()[0].is_ascii_digit()
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_env_name_invalid",
            "Azure credential environment names must be explicit uppercase identifiers",
        ));
    }
    Ok(())
}

fn exact_secret_handle(
    reference: &str,
    exact_interface_id: &str,
) -> Result<SecretHandle, AzureAuthFailure> {
    let handle = parse_secret_ref(reference)?;
    if handle.scope.name() != Some(exact_interface_id) {
        return Err(AzureAuthFailure::contract(
            "azure_entra_secret_scope_mismatch",
            format!(
                "interface '{exact_interface_id}' Azure credential handles must use its exact interface scope"
            ),
        ));
    }
    Ok(handle)
}

fn parse_secret_ref(reference: &str) -> Result<SecretHandle, AzureAuthFailure> {
    if reference.trim() != reference {
        return Err(AzureAuthFailure::contract(
            "azure_entra_secret_ref_invalid",
            "Azure Entra secret reference must be trimmed",
        ));
    }
    let path = reference.strip_prefix("secret://").ok_or_else(|| {
        AzureAuthFailure::contract(
            "azure_entra_secret_ref_invalid",
            "Azure Entra secret reference must start with secret://",
        )
    })?;
    let (scope, name) = path.split_once('/').ok_or_else(|| {
        AzureAuthFailure::contract(
            "azure_entra_secret_ref_invalid",
            "Azure Entra secret reference must be secret://<exact-interface>/<name>",
        )
    })?;
    if name.contains('/') {
        return Err(AzureAuthFailure::contract(
            "azure_entra_secret_ref_invalid",
            "Azure Entra secret reference must name one scoped secret",
        ));
    }
    Ok(SecretHandle {
        scope: SecretScope::named(scope.to_string()).map_err(|error| {
            AzureAuthFailure::contract("azure_entra_secret_ref_invalid", error.to_string())
        })?,
        name: SecretName::new(name).map_err(|error| {
            AzureAuthFailure::contract("azure_entra_secret_ref_invalid", error.to_string())
        })?,
    })
}

fn require_distinct_handles(handles: &[&SecretHandle]) -> Result<(), AzureAuthFailure> {
    let values = handles
        .iter()
        .map(|handle| {
            format!(
                "{}/{}",
                handle.scope.name().unwrap_or("global"),
                handle.name
            )
        })
        .collect::<BTreeSet<_>>();
    if values.len() != handles.len() {
        return Err(AzureAuthFailure::contract(
            "azure_entra_secret_handles_overlap",
            "Azure Entra credential handles within one source must be distinct",
        ));
    }
    Ok(())
}

fn required_environment(name: &str, surface: &str) -> Result<String, AzureAuthFailure> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AzureAuthFailure::unavailable(
                "azure_entra_environment_unavailable",
                format!("declared Azure {surface} environment value is unavailable"),
            )
        })
}

fn read_federated_assertion(
    env_name: &str,
    maximum_bytes: usize,
) -> Result<String, AzureAuthFailure> {
    let path = std::env::var_os(env_name).ok_or_else(|| {
        AzureAuthFailure::unavailable(
            "azure_entra_workload_file_unavailable",
            "declared Azure workload identity token-file environment value is unavailable",
        )
    })?;
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(AzureAuthFailure::contract(
            "azure_entra_workload_file_invalid",
            "Azure workload identity token file must be an absolute authority-granted path",
        ));
    }
    let file = std::fs::File::open(&path).map_err(|_| {
        AzureAuthFailure::unavailable(
            "azure_entra_workload_file_unavailable",
            "declared Azure workload identity token file cannot be opened",
        )
    })?;
    let mut bytes = Vec::new();
    file.take(maximum_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            AzureAuthFailure::unavailable(
                "azure_entra_workload_file_unavailable",
                "declared Azure workload identity token file cannot be read",
            )
        })?;
    if bytes.len() > maximum_bytes {
        return Err(AzureAuthFailure::contract(
            "azure_entra_workload_file_too_large",
            "Azure workload identity token file exceeds its declared byte limit",
        ));
    }
    let assertion = std::str::from_utf8(&bytes).map_err(|_| {
        AzureAuthFailure::contract(
            "azure_entra_workload_assertion_invalid",
            "Azure workload identity assertion must be UTF-8",
        )
    })?;
    let assertion = assertion.trim().to_string();
    validate_secret_value(&assertion, "workload identity assertion")?;
    if assertion.contains(char::is_whitespace) {
        return Err(AzureAuthFailure::contract(
            "azure_entra_workload_assertion_invalid",
            "Azure workload identity assertion cannot contain internal whitespace",
        ));
    }
    Ok(assertion)
}

fn validate_tenant_id(value: &str) -> Result<(), AzureAuthFailure> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > 255
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_tenant_id_invalid",
            "Azure Entra tenant id must be a trimmed GUID or verified tenant domain",
        ));
    }
    Ok(())
}

fn validate_client_id(value: &str) -> Result<(), AzureAuthFailure> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > 255
        || value.contains(['\r', '\n'])
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_client_id_invalid",
            "Azure Entra client id must be non-empty, trimmed and bounded",
        ));
    }
    Ok(())
}

fn validate_secret_value(value: &str, surface: &str) -> Result<(), AzureAuthFailure> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > MAX_ACCESS_TOKEN_BYTES
        || value.contains(['\r', '\n'])
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_secret_value_invalid",
            format!("Azure Entra {surface} must be non-empty, trimmed and bounded"),
        ));
    }
    Ok(())
}

fn token_endpoint(authority_host: &str, tenant_id: &str) -> Result<Url, AzureAuthFailure> {
    let mut url = Url::parse(authority_host).map_err(|error| {
        AzureAuthFailure::contract(
            "azure_entra_authority_invalid",
            format!("Azure Entra authority_host is invalid: {error}"),
        )
    })?;
    url.path_segments_mut()
        .map_err(|_| {
            AzureAuthFailure::contract(
                "azure_entra_authority_invalid",
                "Azure Entra authority_host cannot accept path segments",
            )
        })?
        .pop_if_empty()
        .push(tenant_id)
        .push("oauth2")
        .push("v2.0")
        .push("token");
    Ok(url)
}

fn client_secret_request(
    authority_host: &str,
    scope: &str,
    tenant_id: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<TokenHttpRequest, AzureAuthFailure> {
    Ok(TokenHttpRequest::Form {
        url: token_endpoint(authority_host, tenant_id)?,
        fields: vec![
            ("client_id".to_string(), client_id.to_string()),
            ("client_secret".to_string(), client_secret.to_string()),
            ("scope".to_string(), scope.to_string()),
            (
                "grant_type".to_string(),
                CLIENT_CREDENTIALS_GRANT.to_string(),
            ),
        ],
    })
}

fn workload_identity_request(
    authority_host: &str,
    scope: &str,
    tenant_id: &str,
    client_id: &str,
    assertion: &str,
) -> Result<TokenHttpRequest, AzureAuthFailure> {
    Ok(TokenHttpRequest::Form {
        url: token_endpoint(authority_host, tenant_id)?,
        fields: vec![
            ("client_id".to_string(), client_id.to_string()),
            ("scope".to_string(), scope.to_string()),
            (
                "grant_type".to_string(),
                CLIENT_CREDENTIALS_GRANT.to_string(),
            ),
            (
                "client_assertion_type".to_string(),
                JWT_BEARER_ASSERTION_TYPE.to_string(),
            ),
            ("client_assertion".to_string(), assertion.to_string()),
        ],
    })
}

fn imds_endpoint(raw: &str) -> Result<Url, AzureAuthFailure> {
    let url = Url::parse(raw).map_err(|error| {
        AzureAuthFailure::contract(
            "azure_entra_imds_endpoint_invalid",
            format!("Azure IMDS endpoint is invalid: {error}"),
        )
    })?;
    if url.scheme() != "http"
        || url.host_str() != Some("169.254.169.254")
        || !matches!(url.port(), None | Some(80))
        || url.path() != "/metadata/identity/oauth2/token"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_imds_endpoint_invalid",
            "Azure IMDS endpoint must be the exact link-local metadata token endpoint",
        ));
    }
    Ok(url)
}

fn app_service_endpoint(
    raw: &str,
    allowed_paths: &BTreeSet<String>,
) -> Result<Url, AzureAuthFailure> {
    let url = Url::parse(raw).map_err(|error| {
        AzureAuthFailure::contract(
            "azure_entra_app_service_endpoint_invalid",
            format!("Azure App Service managed identity endpoint is invalid: {error}"),
        )
    })?;
    let host = url.host_str().ok_or_else(|| {
        AzureAuthFailure::contract(
            "azure_entra_app_service_endpoint_invalid",
            "Azure App Service managed identity endpoint has no host",
        )
    })?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if url.scheme() != "http"
        || !loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !allowed_paths.contains(url.path())
    {
        return Err(AzureAuthFailure::contract(
            "azure_entra_app_service_endpoint_invalid",
            "Azure App Service managed identity endpoint must be an allowed loopback HTTP path without credentials, query or fragment",
        ));
    }
    Ok(url)
}

fn imds_request(endpoint: Url, resource: &str, client_id: Option<&str>) -> TokenHttpRequest {
    let mut query = vec![
        ("api-version".to_string(), "2018-02-01".to_string()),
        ("resource".to_string(), resource.to_string()),
    ];
    if let Some(client_id) = client_id {
        query.push(("client_id".to_string(), client_id.to_string()));
    }
    TokenHttpRequest::Get {
        url: endpoint,
        query,
        headers: vec![("Metadata", "true".to_string())],
    }
}

fn app_service_request(
    endpoint: Url,
    protocol: AppServiceProtocol,
    header_value: String,
    resource: &str,
    client_id: Option<&str>,
) -> TokenHttpRequest {
    let (api_version, header_name) = match protocol {
        AppServiceProtocol::IdentityHeader2019 => ("2019-08-01", "X-IDENTITY-HEADER"),
        AppServiceProtocol::MsiSecret2017 => ("2017-09-01", "Secret"),
    };
    let mut query = vec![
        ("api-version".to_string(), api_version.to_string()),
        ("resource".to_string(), resource.to_string()),
    ];
    if let Some(client_id) = client_id {
        query.push(("client_id".to_string(), client_id.to_string()));
    }
    TokenHttpRequest::Get {
        url: endpoint,
        query,
        headers: vec![(header_name, header_value)],
    }
}

async fn read_bounded_response(
    mut response: reqwest::Response,
    maximum_bytes: usize,
) -> Result<Vec<u8>, AzureAuthFailure> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        AzureAuthFailure::transport(
            "azure_entra_token_read_failed",
            true,
            format!(
                "Azure credential response could not be read (timeout={}, connect={}, body={})",
                error.is_timeout(),
                error.is_connect(),
                error.is_body()
            ),
        )
    })? {
        if bytes.len().saturating_add(chunk.len()) > maximum_bytes {
            return Err(AzureAuthFailure::contract(
                "azure_entra_token_response_too_large",
                format!("Azure credential response exceeded {maximum_bytes} bytes"),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn read_bounded_pipe<R: AsyncRead + Unpin>(
    mut reader: R,
    maximum_bytes: usize,
) -> Result<Vec<u8>, std::io::Error> {
    let mut bytes = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        if !exceeded && bytes.len().saturating_add(read) <= maximum_bytes {
            bytes.extend_from_slice(&buffer[..read]);
        } else {
            exceeded = true;
        }
    }
    if exceeded {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "output exceeded declared byte limit",
        ));
    }
    Ok(bytes)
}

fn cli_io_failure(surface: &str, error: std::io::Error) -> AzureAuthFailure {
    let code = if error.kind() == std::io::ErrorKind::InvalidData {
        "azure_entra_cli_output_too_large"
    } else {
        "azure_entra_cli_output_failed"
    };
    AzureAuthFailure::transport(
        code,
        false,
        format!("Azure CLI {surface} could not be read: {error}"),
    )
}

fn parse_relative_token_response(
    bytes: &[u8],
    refresh_margin_seconds: u64,
) -> Result<CachedToken, AzureAuthFailure> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        AzureAuthFailure::contract(
            "azure_entra_token_json_invalid",
            format!("Azure credential response is invalid JSON: {error}"),
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        AzureAuthFailure::contract(
            "azure_entra_token_shape_invalid",
            "Azure credential response must be an object",
        )
    })?;
    validate_token_type(object.get("token_type"))?;
    let access_token = required_access_token(object.get("access_token"))?;
    let expires_in = unsigned_json_number(object.get("expires_in"))
        .filter(|value| *value > refresh_margin_seconds && *value <= MAX_TOKEN_LIFETIME_SECONDS)
        .ok_or_else(|| {
            AzureAuthFailure::contract(
                "azure_entra_token_expiry_invalid",
                "Azure expires_in must exceed the refresh margin and be at most 86400 seconds",
            )
        })?;
    cached_token_from_lifetime(access_token, expires_in, refresh_margin_seconds)
}

fn parse_cli_token_response(
    bytes: &[u8],
    refresh_margin_seconds: u64,
    now: SystemTime,
) -> Result<CachedToken, AzureAuthFailure> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        AzureAuthFailure::contract(
            "azure_entra_cli_json_invalid",
            format!("Azure CLI token output is invalid JSON: {error}"),
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        AzureAuthFailure::contract(
            "azure_entra_cli_shape_invalid",
            "Azure CLI token output must be an object",
        )
    })?;
    if let Some(token_type) = object.get("tokenType") {
        validate_token_type(Some(token_type))?;
    }
    let access_token = required_access_token(object.get("accessToken"))?;
    let expires_on = unsigned_json_number(object.get("expires_on"))
        .or_else(|| unsigned_json_number(object.get("expiresOn")))
        .ok_or_else(|| {
            AzureAuthFailure::contract(
                "azure_entra_cli_expiry_invalid",
                "Azure CLI token output requires numeric expires_on epoch evidence",
            )
        })?;
    let now = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            AzureAuthFailure::contract(
                "azure_entra_cli_expiry_invalid",
                "local clock precedes the Unix epoch",
            )
        })?
        .as_secs();
    let lifetime = expires_on
        .checked_sub(now)
        .filter(|value| *value > refresh_margin_seconds && *value <= MAX_TOKEN_LIFETIME_SECONDS);
    cached_token_from_lifetime(
        access_token,
        lifetime.ok_or_else(|| {
            AzureAuthFailure::contract(
                "azure_entra_cli_expiry_invalid",
                "Azure CLI token expiry must be future, bounded, and exceed the refresh margin",
            )
        })?,
        refresh_margin_seconds,
    )
}

fn validate_token_type(value: Option<&Value>) -> Result<(), AzureAuthFailure> {
    let token_type = value
        .and_then(Value::as_str)
        .filter(|value| value.eq_ignore_ascii_case("bearer"))
        .ok_or_else(|| {
            AzureAuthFailure::contract(
                "azure_entra_token_type_invalid",
                "Azure credential token_type must be Bearer",
            )
        })?;
    if token_type.len() != "bearer".len() {
        return Err(AzureAuthFailure::contract(
            "azure_entra_token_type_invalid",
            "Azure credential token_type must be Bearer",
        ));
    }
    Ok(())
}

fn required_access_token(value: Option<&Value>) -> Result<String, AzureAuthFailure> {
    let access_token = value.and_then(Value::as_str).ok_or_else(|| {
        AzureAuthFailure::contract(
            "azure_entra_access_token_missing",
            "Azure credential response has no access token",
        )
    })?;
    validate_secret_value(access_token, "access token")?;
    Ok(access_token.to_string())
}

fn unsigned_json_number(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::Number(value)) => value.as_u64(),
        Some(Value::String(value)) => value.parse::<u64>().ok(),
        _ => None,
    }
}

fn cached_token_from_lifetime(
    access_token: String,
    lifetime_seconds: u64,
    refresh_margin_seconds: u64,
) -> Result<CachedToken, AzureAuthFailure> {
    let refresh_at = Instant::now()
        .checked_add(Duration::from_secs(
            lifetime_seconds.saturating_sub(refresh_margin_seconds),
        ))
        .ok_or_else(|| {
            AzureAuthFailure::contract(
                "azure_entra_token_expiry_invalid",
                "Azure token lifetime exceeds the local clock range",
            )
        })?;
    Ok(CachedToken {
        access_token,
        refresh_at,
        source: AzureCredentialSourceKind::ClientSecret,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn query_value<'a>(query: &'a [(String, String)], name: &str) -> Option<&'a str> {
        query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn form_value<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
        query_value(fields, name)
    }

    #[test]
    fn descriptor_chain_is_exact_ordered_and_life_authorized() {
        let registry = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("packages directory")
            .parent()
            .expect("interface registry")
            .to_path_buf();
        let mut fingerprints = BTreeSet::new();
        for interface in [
            "world_provider_azure_foundry_chat_entra",
            "world_provider_azure_foundry_responses_entra",
            "world_provider_azure_foundry_anthropic_entra",
        ] {
            let folder = registry.join(interface);
            let bytes = fs::read(folder.join("interface.json")).expect("interface descriptor");
            let descriptor: Value = serde_json::from_slice(&bytes).expect("valid interface JSON");
            let typed: InterfaceDescriptor =
                serde_json::from_slice(&bytes).expect("valid interface descriptor");
            assert_eq!(
                descriptor.get("id").and_then(Value::as_str),
                Some(interface)
            );
            assert!(descriptor.get("reference").is_none());
            let chain = descriptor
                .pointer("/interface_driver_config/private_config/azure_entra/credential_chain")
                .and_then(Value::as_array)
                .expect("credential chain");
            let kinds = chain
                .iter()
                .map(|source| source.get("kind").and_then(Value::as_str).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                kinds,
                [
                    "client_secret",
                    "workload_identity",
                    "managed_identity_app_service",
                    "managed_identity_imds"
                ]
            );
            let encoded = serde_json::to_string(&descriptor).expect("encoded descriptor");
            for handle in ["AZURE_TENANT_ID", "AZURE_CLIENT_ID", "AZURE_CLIENT_SECRET"] {
                assert!(encoded.contains(&format!("secret://{interface}/{handle}")));
            }
            let manager = AzureEntraCredentialManager::from_descriptor(
                &typed,
                Path::new("/tmp/octopus-azure-auth-test"),
            )
            .expect("valid Entra config")
            .expect("Azure Entra manager");
            assert_eq!(manager.credential_source_fingerprint().len(), 64);
            assert!(fingerprints.insert(manager.credential_source_fingerprint().to_string()));
            let life: Value = serde_json::from_slice(
                &fs::read(folder.join("life.json")).expect("life descriptor"),
            )
            .expect("valid life JSON");
            assert!(life
                .pointer("/processes/0/authority/optional_read_env_paths")
                .and_then(Value::as_array)
                .is_some_and(|values| values
                    .iter()
                    .any(|value| { value.as_str() == Some("AZURE_FEDERATED_TOKEN_FILE") })));
            assert!(life
                .pointer("/processes/0/authority/executable_commands")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty));
        }
        assert_eq!(fingerprints.len(), 3);
    }

    #[test]
    fn explicit_precedence_continues_only_on_declared_classes() {
        let unavailable = AzureAuthFailure::unavailable("missing", "missing");
        let endpoint = AzureAuthFailure::response(
            AzureAuthFailureClass::Endpoint,
            "rejected",
            401,
            false,
            b"",
            "rejected",
        );
        let continue_on = BTreeSet::from([AzureAuthFailureClass::Unavailable]);
        assert!(continue_on.contains(&unavailable.class_kind()));
        assert!(!continue_on.contains(&endpoint.class_kind()));
        let aggregate = AzureAuthFailure::aggregate(vec![
            unavailable.source_evidence(0, AzureCredentialSourceKind::ClientSecret),
            AzureAuthFailure::unavailable("file_missing", "missing")
                .source_evidence(1, AzureCredentialSourceKind::WorkloadIdentity),
        ]);
        assert_eq!(aggregate.code, "azure_entra_credential_chain_exhausted");
        assert_eq!(aggregate.source_failures.len(), 2);
        assert!(!aggregate.to_string().contains("secret-value"));
    }

    #[test]
    fn typed_failure_evidence_keeps_only_bounded_response_proof() {
        let failure = AzureAuthFailure::response(
            AzureAuthFailureClass::Endpoint,
            "rejected",
            401,
            false,
            b"Bearer secret-value",
            "credential endpoint rejected the request",
        )
        .with_source(0, AzureCredentialSourceKind::ClientSecret);
        let encoded = serde_json::to_string(&failure.evidence()).unwrap();
        assert!(encoded.contains("\"status\":401"));
        assert!(encoded.contains("\"source\":\"client_secret\""));
        assert!(encoded.contains("response_blake3"));
        assert!(!encoded.contains("secret-value"));
        assert!(!encoded.contains("Bearer"));
    }

    #[test]
    fn cache_respects_expiry_and_invalidation() {
        let now = Instant::now();
        let mut cache = TokenCache {
            token: Some(CachedToken {
                access_token: "cached".to_string(),
                refresh_at: now + Duration::from_secs(30),
                source: AzureCredentialSourceKind::WorkloadIdentity,
            }),
        };
        assert_eq!(cache.access_token(now).as_deref(), Some("cached"));
        assert!(cache.access_token(now + Duration::from_secs(31)).is_none());
        cache.invalidate();
        assert!(cache.access_token(now).is_none());
    }

    #[test]
    fn workload_and_managed_identity_request_shapes_are_strict() {
        let workload = workload_identity_request(
            "https://login.microsoftonline.com",
            "https://ai.azure.com/.default",
            "tenant",
            "client",
            "assertion",
        )
        .expect("workload request");
        let TokenHttpRequest::Form { url, fields } = workload else {
            panic!("workload request must be form POST")
        };
        assert_eq!(url.path(), "/tenant/oauth2/v2.0/token");
        assert_eq!(
            form_value(&fields, "scope"),
            Some("https://ai.azure.com/.default")
        );
        assert_eq!(form_value(&fields, "client_assertion"), Some("assertion"));
        assert_eq!(
            form_value(&fields, "client_assertion_type"),
            Some(JWT_BEARER_ASSERTION_TYPE)
        );

        let imds = imds_request(
            imds_endpoint("http://169.254.169.254/metadata/identity/oauth2/token")
                .expect("IMDS endpoint"),
            "https://ai.azure.com",
            None,
        );
        let TokenHttpRequest::Get { query, headers, .. } = imds else {
            panic!("IMDS request must be GET")
        };
        assert_eq!(query_value(&query, "api-version"), Some("2018-02-01"));
        assert_eq!(
            query_value(&query, "resource"),
            Some("https://ai.azure.com")
        );
        assert_eq!(headers, vec![("Metadata", "true".to_string())]);

        let app = app_service_request(
            app_service_endpoint(
                "http://127.0.0.1:41741/MSI/token",
                &BTreeSet::from(["/MSI/token".to_string()]),
            )
            .expect("App Service endpoint"),
            AppServiceProtocol::IdentityHeader2019,
            "header-secret".to_string(),
            "https://ai.azure.com",
            None,
        );
        let TokenHttpRequest::Get { query, headers, .. } = app else {
            panic!("App Service request must be GET")
        };
        assert_eq!(query_value(&query, "api-version"), Some("2019-08-01"));
        assert_eq!(
            headers,
            vec![("X-IDENTITY-HEADER", "header-secret".to_string())]
        );
        assert!(app_service_endpoint(
            "http://example.com/MSI/token",
            &BTreeSet::from(["/MSI/token".to_string()])
        )
        .is_err());
    }

    #[test]
    fn scope_resource_and_token_expiry_are_exact() {
        assert!(
            validate_resource_scope("https://ai.azure.com", "https://ai.azure.com/.default")
                .is_ok()
        );
        assert!(validate_resource_scope(
            "https://cognitiveservices.azure.com",
            "https://ai.azure.com/.default"
        )
        .is_err());
        let token = parse_relative_token_response(
            br#"{"token_type":"Bearer","access_token":"signed-token","expires_in":3600}"#,
            120,
        )
        .expect("valid token response");
        assert_eq!(token.access_token, "signed-token");
        assert!(parse_relative_token_response(
            br#"{"token_type":"Bearer","access_token":"signed-token","expires_in":60}"#,
            120,
        )
        .is_err());
    }

    #[test]
    fn cli_requires_numeric_expiry_and_never_exposes_output_on_failure() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let token = parse_cli_token_response(
            br#"{"tokenType":"Bearer","accessToken":"cli-token","expires_on":1003600}"#,
            120,
            now,
        )
        .expect("valid CLI token");
        assert_eq!(token.access_token, "cli-token");
        assert!(parse_cli_token_response(
            br#"{"tokenType":"Bearer","accessToken":"cli-token","expiresOn":"2026-07-22 10:00:00"}"#,
            120,
            now,
        )
        .is_err());
    }
}
