use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io::Read;
use std::net::{IpAddr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use octopus_secrets::{
    LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind, SecretsManager,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, HOST};
use reqwest::{Client, Method, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "scheme", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum AuthorizationConfig {
    Sigv4 { credentials: Box<CredentialSource> },
    Bearer { token: TokenSource },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CredentialSource {
    Chain {
        sources: Vec<CredentialSource>,
    },
    Environment {
        access_key_env: String,
        secret_key_env: String,
        #[serde(default)]
        session_token_env: Option<String>,
    },
    SecretHandles {
        access_key_ref: String,
        secret_key_ref: String,
        #[serde(default)]
        session_token_ref: Option<String>,
    },
    Profile {
        profile: String,
        credentials_file_env: String,
        config_file_env: String,
        max_role_depth: usize,
        sts: StsConfig,
        #[serde(default)]
        credential_process: Option<CredentialProcessPolicy>,
        #[serde(default)]
        sso_cache_file_env: Option<String>,
        #[serde(default)]
        sso_portal_domain: Option<String>,
    },
    AssumeRole {
        role_arn: String,
        role_session_name: String,
        #[serde(default)]
        external_id: Option<String>,
        #[serde(default)]
        duration_seconds: Option<u32>,
        source: Box<CredentialSource>,
        sts: StsConfig,
    },
    WebIdentity {
        role_arn_env: String,
        token_file_env: String,
        #[serde(default)]
        role_session_name_env: Option<String>,
        default_role_session_name: String,
        #[serde(default)]
        duration_seconds: Option<u32>,
        sts: StsConfig,
    },
    EcsContainer {
        relative_uri_env: String,
        full_uri_env: String,
        #[serde(default)]
        authorization_token_env: Option<String>,
        #[serde(default)]
        authorization_token_file_env: Option<String>,
        relative_base_url: String,
        allowed_full_uri_hosts: Vec<String>,
        timeout_ms: u64,
        max_response_bytes: usize,
    },
    ImdsV2 {
        endpoint: String,
        #[serde(default)]
        role_name: Option<String>,
        token_ttl_seconds: u32,
        timeout_ms: u64,
        max_response_bytes: usize,
    },
    CredentialProcess {
        executable_env: String,
        args: Vec<String>,
        #[serde(default)]
        inherit_env_names: Vec<String>,
        timeout_ms: u64,
        max_stdout_bytes: usize,
        max_stderr_bytes: usize,
    },
    Sso {
        account_id: String,
        role_name: String,
        region: String,
        portal_base_url: String,
        token: TokenSource,
        timeout_ms: u64,
        max_response_bytes: usize,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StsConfig {
    endpoint: String,
    region: String,
    signing_name: String,
    default_role_session_name: String,
    timeout_ms: u64,
    max_response_bytes: usize,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CredentialProcessPolicy {
    executable_env: String,
    #[serde(default)]
    inherit_env_names: Vec<String>,
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TokenSource {
    Environment { env: String },
    SecretHandle { secret_ref: String },
    CacheFileEnv { path_env: String },
}

#[derive(Clone)]
pub(crate) struct AwsAuthorizer {
    authorization: AuthorizationConfig,
    region: String,
    signing_name: String,
    octopus_home: PathBuf,
    http: Client,
    cached_credentials: Arc<Mutex<Option<AwsCredentials>>>,
}

#[derive(Clone)]
struct AwsCredentials {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    credential_handle_fingerprint: Option<String>,
}

struct SigV4Scope<'a> {
    region: &'a str,
    signing_name: &'a str,
    now: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthFailure {
    pub detail: String,
    pub retryable: bool,
    pub evidence: Value,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuthFailureKind {
    Unavailable,
    Invalid,
    Transport,
    Rejected,
}

#[derive(Clone, Debug, Serialize)]
struct AuthSourceFailure {
    index: usize,
    source: &'static str,
    kind: AuthFailureKind,
    code: String,
    retryable: bool,
}

impl AwsAuthorizer {
    pub(crate) fn new(
        authorization: AuthorizationConfig,
        interface_id: &str,
        region: String,
        signing_name: String,
        octopus_home: PathBuf,
        http: Client,
        process_env_names: &[String],
    ) -> Result<Self, String> {
        validate_region(&region)?;
        validate_signing_name(&signing_name)?;
        validate_authorization(&authorization, process_env_names, interface_id)?;
        Ok(Self {
            authorization,
            region,
            signing_name,
            octopus_home,
            http,
            cached_credentials: Arc::new(Mutex::new(None)),
        })
    }

    pub(crate) async fn authorize(
        &self,
        method: &Method,
        url: &Url,
        body: &[u8],
        mut headers: HeaderMap,
    ) -> Result<HeaderMap, AuthFailure> {
        match &self.authorization {
            AuthorizationConfig::Bearer { token } => {
                let token = self.resolve_token(token).await?;
                let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|error| {
                    AuthFailure::invalid(
                        "bearer_header_invalid",
                        format!("Bedrock bearer token is not a valid header: {error}"),
                    )
                })?;
                headers.insert(AUTHORIZATION, value);
                Ok(headers)
            }
            AuthorizationConfig::Sigv4 { credentials } => {
                let credentials = self.resolve_credentials(credentials).await?;
                sign_headers(
                    method,
                    url,
                    body,
                    headers,
                    &credentials,
                    SigV4Scope {
                        region: &self.region,
                        signing_name: &self.signing_name,
                        now: Utc::now(),
                    },
                )
                .map_err(|error| AuthFailure::invalid("sigv4_signing_failed", error))
            }
        }
    }

    pub(crate) fn uses_cached_credentials(&self) -> bool {
        matches!(&self.authorization, AuthorizationConfig::Sigv4 { .. })
    }

    pub(crate) async fn invalidate_cached_credentials(&self) {
        self.cached_credentials.lock().await.take();
    }

    pub(crate) async fn credential_handle_fingerprint(&self) -> Option<String> {
        match &self.authorization {
            AuthorizationConfig::Bearer {
                token: TokenSource::SecretHandle { secret_ref },
            } => Some(secret_handle_fingerprint(&[secret_ref.as_str()])),
            AuthorizationConfig::Bearer { token } => token_source_fingerprint(token).ok(),
            AuthorizationConfig::Sigv4 { credentials } => {
                credential_source_fingerprint(credentials).ok()
            }
        }
    }

    pub(crate) fn for_endpoint(
        &self,
        region: String,
        signing_name: String,
    ) -> Result<Self, String> {
        validate_region(&region)?;
        validate_signing_name(&signing_name)?;
        Ok(Self {
            authorization: self.authorization.clone(),
            region,
            signing_name,
            octopus_home: self.octopus_home.clone(),
            http: self.http.clone(),
            cached_credentials: self.cached_credentials.clone(),
        })
    }

    async fn resolve_credentials(
        &self,
        source: &CredentialSource,
    ) -> Result<AwsCredentials, AuthFailure> {
        if let Some(cached) = self.cached_credentials().await {
            return Ok(cached);
        }
        let mut profile_stack = Vec::new();
        let mut loaded = self.resolve_source(source, 0, &mut profile_stack).await?;
        loaded.credential_handle_fingerprint = Some(credential_source_fingerprint(source)?);
        *self.cached_credentials.lock().await = Some(loaded.clone());
        Ok(loaded)
    }

    fn resolve_source<'a>(
        &'a self,
        source: &'a CredentialSource,
        depth: usize,
        profile_stack: &'a mut Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<AwsCredentials, AuthFailure>> + Send + 'a>> {
        Box::pin(async move {
            if depth > 32 {
                return Err(AuthFailure::invalid(
                    "credential_recursion_limit",
                    "AWS credential source recursion exceeds 32 levels",
                ));
            }
            match source {
                CredentialSource::Chain { sources } => {
                    let mut failures = Vec::new();
                    for (index, source) in sources.iter().enumerate() {
                        match self.resolve_source(source, depth + 1, profile_stack).await {
                            Ok(credentials) => return Ok(credentials),
                            Err(failure) if failure.fallback_eligible() => {
                                failures.push(failure.source_failure(index, source.kind_name()));
                            }
                            Err(mut failure) => {
                                failure.annotate_chain_source(index, source.kind_name(), failures);
                                return Err(failure);
                            }
                        }
                    }
                    Err(AuthFailure::chain(failures))
                }
                CredentialSource::Environment {
                    access_key_env,
                    secret_key_env,
                    session_token_env,
                } => credentials_from_environment(
                    access_key_env,
                    secret_key_env,
                    session_token_env.as_deref(),
                ),
                CredentialSource::SecretHandles {
                    access_key_ref,
                    secret_key_ref,
                    session_token_ref,
                } => credentials_from_secrets(
                    &self.octopus_home,
                    access_key_ref,
                    secret_key_ref,
                    session_token_ref.as_deref(),
                ),
                CredentialSource::Profile {
                    profile,
                    credentials_file_env,
                    config_file_env,
                    max_role_depth,
                    sts,
                    credential_process,
                    sso_cache_file_env,
                    sso_portal_domain,
                } => {
                    self.resolve_profile(
                        profile.clone(),
                        ProfileResolution {
                            credentials_file_env,
                            config_file_env,
                            max_role_depth: *max_role_depth,
                            sts,
                            process_policy: credential_process.as_ref(),
                            sso_cache_file_env: sso_cache_file_env.as_deref(),
                            sso_portal_domain: sso_portal_domain.as_deref(),
                        },
                        profile_stack,
                    )
                    .await
                }
                CredentialSource::AssumeRole {
                    role_arn,
                    role_session_name,
                    external_id,
                    duration_seconds,
                    source,
                    sts,
                } => {
                    let source_credentials = self
                        .resolve_source(source, depth + 1, profile_stack)
                        .await?;
                    credentials_from_assume_role(
                        &self.http,
                        sts,
                        role_arn,
                        role_session_name,
                        external_id.as_deref(),
                        *duration_seconds,
                        &source_credentials,
                    )
                    .await
                }
                CredentialSource::WebIdentity {
                    role_arn_env,
                    token_file_env,
                    role_session_name_env,
                    default_role_session_name,
                    duration_seconds,
                    sts,
                } => {
                    credentials_from_web_identity(
                        &self.http,
                        sts,
                        role_arn_env,
                        token_file_env,
                        role_session_name_env.as_deref(),
                        default_role_session_name,
                        *duration_seconds,
                    )
                    .await
                }
                CredentialSource::EcsContainer {
                    relative_uri_env,
                    full_uri_env,
                    authorization_token_env,
                    authorization_token_file_env,
                    relative_base_url,
                    allowed_full_uri_hosts,
                    timeout_ms,
                    max_response_bytes,
                } => {
                    credentials_from_ecs(
                        &self.http,
                        EcsCredentialRequest {
                            relative_uri_env,
                            full_uri_env,
                            authorization_token_env: authorization_token_env.as_deref(),
                            authorization_token_file_env: authorization_token_file_env.as_deref(),
                            relative_base_url,
                            allowed_full_uri_hosts,
                            timeout_ms: *timeout_ms,
                            max_response_bytes: *max_response_bytes,
                        },
                    )
                    .await
                }
                CredentialSource::ImdsV2 {
                    endpoint,
                    role_name,
                    token_ttl_seconds,
                    timeout_ms,
                    max_response_bytes,
                } => {
                    credentials_from_imds(
                        &self.http,
                        endpoint,
                        role_name.as_deref(),
                        *token_ttl_seconds,
                        *timeout_ms,
                        *max_response_bytes,
                    )
                    .await
                }
                CredentialSource::CredentialProcess {
                    executable_env,
                    args,
                    inherit_env_names,
                    timeout_ms,
                    max_stdout_bytes,
                    max_stderr_bytes,
                } => {
                    let executable = optional_absolute_path_environment(executable_env)?
                        .ok_or_else(|| {
                            AuthFailure::unavailable(
                                "credential_process_unconfigured",
                                "explicit credential process executable is unavailable",
                            )
                        })?;
                    credentials_from_process(
                        &executable,
                        args,
                        inherit_env_names,
                        *timeout_ms,
                        *max_stdout_bytes,
                        *max_stderr_bytes,
                    )
                    .await
                }
                CredentialSource::Sso {
                    account_id,
                    role_name,
                    region,
                    portal_base_url,
                    token,
                    timeout_ms,
                    max_response_bytes,
                } => {
                    let access_token = self.resolve_token(token).await?;
                    credentials_from_sso(
                        &self.http,
                        SsoCredentialRequest {
                            account_id,
                            role_name,
                            region,
                            portal_base_url,
                            timeout_ms: *timeout_ms,
                            max_response_bytes: *max_response_bytes,
                        },
                        &access_token,
                    )
                    .await
                }
            }
        })
    }

    fn resolve_profile<'a>(
        &'a self,
        profile: String,
        config: ProfileResolution<'a>,
        profile_stack: &'a mut Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<AwsCredentials, AuthFailure>> + Send + 'a>> {
        Box::pin(async move {
            if profile_stack.len() >= config.max_role_depth {
                return Err(AuthFailure::invalid(
                    "profile_depth_exceeded",
                    "AWS source_profile depth exceeds the descriptor limit",
                ));
            }
            if profile_stack.iter().any(|visited| visited == &profile) {
                return Err(AuthFailure::invalid(
                    "profile_cycle",
                    "AWS source_profile graph contains a cycle",
                ));
            }
            let files = load_profile_files(config.credentials_file_env, config.config_file_env)?;
            let values = files.profile(&profile).ok_or_else(|| {
                AuthFailure::unavailable(
                    "profile_absent",
                    format!("AWS profile '{profile}' is absent from declared files"),
                )
            })?;
            profile_stack.push(profile.clone());
            let result = async {
                if values.contains_key("role_arn") {
                if values.contains_key("web_identity_token_file") {
                    Err(AuthFailure::invalid(
                        "profile_web_identity_requires_explicit_source",
                        "AWS profile web identity must use the explicit web_identity source",
                    ))
                } else {
                    let source_profile =
                        required_profile_value(&values, "source_profile", &profile)?;
                    let source = if source_profile == profile {
                        credentials_from_profile_values(&values, &profile)
                    } else {
                        self.resolve_profile(source_profile, config, profile_stack)
                            .await
                    }?;
                    let role_arn = required_profile_value(&values, "role_arn", &profile)?;
                    let role_session_name = values
                        .get("role_session_name")
                        .cloned()
                        .unwrap_or_else(|| config.sts.default_role_session_name.clone());
                    let duration_seconds = values
                        .get("duration_seconds")
                        .map(|value| parse_u32(value, "profile duration_seconds"))
                        .transpose()?;
                    credentials_from_assume_role(
                        &self.http,
                        config.sts,
                        &role_arn,
                        &role_session_name,
                        values.get("external_id").map(String::as_str),
                        duration_seconds,
                        &source,
                    )
                    .await
                }
            } else if has_static_profile_credentials(&values) {
                credentials_from_profile_values(&values, &profile)
            } else if let Some(command) = values.get("credential_process") {
                let policy = config.process_policy.ok_or_else(|| {
                    AuthFailure::invalid(
                        "credential_process_not_authorized",
                        "AWS profile declares credential_process without descriptor process authority",
                    )
                })?;
                let parsed = parse_process_command(command)?;
                let executable = optional_absolute_path_environment(&policy.executable_env)?
                    .ok_or_else(|| {
                        AuthFailure::invalid(
                            "credential_process_executable_unavailable",
                            "AWS profile credential_process executable authority is unavailable",
                        )
                    })?;
                if parsed.first().map(String::as_str) != executable.to_str() {
                    Err(AuthFailure::invalid(
                        "credential_process_executable_mismatch",
                        "AWS profile credential_process executable does not match descriptor authority",
                    ))
                } else {
                    credentials_from_process(
                        &executable,
                        &parsed[1..],
                        &policy.inherit_env_names,
                        policy.timeout_ms,
                        policy.max_stdout_bytes,
                        policy.max_stderr_bytes,
                    )
                    .await
                }
            } else if values.contains_key("sso_account_id")
                || values.contains_key("sso_role_name")
                || values.contains_key("sso_session")
            {
                let cache_env = config.sso_cache_file_env.ok_or_else(|| {
                    AuthFailure::invalid(
                        "sso_cache_not_declared",
                        "AWS SSO profile has no descriptor-declared cache file source",
                    )
                })?;
                let portal_domain = config.sso_portal_domain.ok_or_else(|| {
                    AuthFailure::invalid(
                        "sso_portal_not_declared",
                        "AWS SSO profile has no descriptor-declared portal domain",
                    )
                })?;
                let resolved = files.resolve_sso(&values, &profile, portal_domain)?;
                optional_absolute_path_environment(cache_env)?.ok_or_else(|| {
                    AuthFailure::invalid(
                        "sso_cache_unconfigured",
                        "AWS SSO profile cache file is not configured",
                    )
                })?;
                let token = sso_access_token_from_cache(cache_env)?;
                credentials_from_sso(
                    &self.http,
                    SsoCredentialRequest {
                        account_id: &resolved.account_id,
                        role_name: &resolved.role_name,
                        region: &resolved.region,
                        portal_base_url: &resolved.portal_base_url,
                        timeout_ms: config.sts.timeout_ms,
                        max_response_bytes: config.sts.max_response_bytes,
                    },
                    &token,
                )
                .await
            } else {
                Err(AuthFailure::unavailable(
                    "profile_has_no_credentials",
                    format!("AWS profile '{profile}' has no supported credentials"),
                ))
                }
            }
            .await;
            profile_stack.pop();
            result
        })
    }

    async fn cached_credentials(&self) -> Option<AwsCredentials> {
        let cached = self.cached_credentials.lock().await.clone()?;
        if cached
            .expires_at
            .is_some_and(|expiry| expiry <= Utc::now() + chrono::Duration::minutes(5))
        {
            *self.cached_credentials.lock().await = None;
            return None;
        }
        Some(cached)
    }

    async fn resolve_token(&self, source: &TokenSource) -> Result<String, AuthFailure> {
        match source {
            TokenSource::Environment { env } => required_environment(env, "token"),
            TokenSource::SecretHandle { secret_ref } => {
                secret_value(&self.octopus_home, secret_ref)
            }
            TokenSource::CacheFileEnv { path_env } => sso_access_token_from_cache(path_env),
        }
    }
}

fn credential_source_fingerprint(source: &CredentialSource) -> Result<String, AuthFailure> {
    typed_auth_source_fingerprint(b"octopus.provider.aws.credential_source.v1", source)
}

fn token_source_fingerprint(source: &TokenSource) -> Result<String, AuthFailure> {
    typed_auth_source_fingerprint(b"octopus.provider.aws.token_source.v1", source)
}

fn typed_auth_source_fingerprint<T: Serialize>(
    domain: &[u8],
    source: &T,
) -> Result<String, AuthFailure> {
    let encoded = serde_json::to_vec(source).map_err(|error| {
        AuthFailure::invalid(
            "credential_source_fingerprint_failed",
            format!("AWS credential source could not be encoded: {error}"),
        )
    })?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&(encoded.len() as u64).to_le_bytes());
    hasher.update(&encoded);
    Ok(blake3::Hasher::finalize(&hasher).to_hex().to_string())
}

impl AuthFailure {
    fn unavailable(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            retryable: false,
            evidence: json!({"kind": "unavailable", "code": code}),
        }
    }

    fn unavailable_retryable(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            retryable: true,
            evidence: json!({"kind": "unavailable", "code": code}),
        }
    }

    fn invalid(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            retryable: false,
            evidence: json!({"kind": "invalid", "code": code}),
        }
    }

    fn transport(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            retryable: true,
            evidence: json!({"kind": "transport", "code": code}),
        }
    }

    fn rejected(code: &'static str, detail: impl Into<String>, retryable: bool) -> Self {
        Self {
            detail: detail.into(),
            retryable,
            evidence: json!({"kind": "rejected", "code": code}),
        }
    }

    fn chain(failures: Vec<AuthSourceFailure>) -> Self {
        let retryable = failures.iter().any(|failure| failure.retryable);
        Self {
            detail: format!(
                "all {} explicitly ordered AWS credential sources are unavailable",
                failures.len()
            ),
            retryable,
            evidence: json!({"kind": "aggregate", "code": "credential_chain_exhausted", "sources": failures}),
        }
    }

    fn kind(&self) -> AuthFailureKind {
        match self.evidence.get("kind").and_then(Value::as_str) {
            Some("unavailable") => AuthFailureKind::Unavailable,
            Some("transport") => AuthFailureKind::Transport,
            Some("rejected") => AuthFailureKind::Rejected,
            _ => AuthFailureKind::Invalid,
        }
    }

    fn code(&self) -> String {
        self.evidence
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("credential_source_failed")
            .to_string()
    }

    fn fallback_eligible(&self) -> bool {
        matches!(self.kind(), AuthFailureKind::Unavailable)
    }

    fn source_failure(&self, index: usize, source: &'static str) -> AuthSourceFailure {
        AuthSourceFailure {
            index,
            source,
            kind: self.kind(),
            code: self.code(),
            retryable: self.retryable,
        }
    }

    fn annotate_chain_source(
        &mut self,
        index: usize,
        source: &'static str,
        failures: Vec<AuthSourceFailure>,
    ) {
        self.evidence["source"] =
            serde_json::to_value(self.source_failure(index, source)).unwrap_or(Value::Null);
        if !failures.is_empty() {
            self.evidence["prior_sources"] = serde_json::to_value(failures).unwrap_or(Value::Null);
        }
    }
}

impl CredentialSource {
    fn kind_name(&self) -> &'static str {
        match self {
            Self::Chain { .. } => "chain",
            Self::Environment { .. } => "environment",
            Self::SecretHandles { .. } => "secret_handles",
            Self::Profile { .. } => "profile",
            Self::AssumeRole { .. } => "assume_role",
            Self::WebIdentity { .. } => "web_identity",
            Self::EcsContainer { .. } => "ecs_container",
            Self::ImdsV2 { .. } => "imds_v2",
            Self::CredentialProcess { .. } => "credential_process",
            Self::Sso { .. } => "sso",
        }
    }
}

fn validate_authorization(
    authorization: &AuthorizationConfig,
    process_env_names: &[String],
    interface_id: &str,
) -> Result<(), String> {
    let mut declared = BTreeSet::new();
    for (index, name) in process_env_names.iter().enumerate() {
        validate_env_name(name, &format!("process_env_names[{index}]"))?;
        if !declared.insert(name.clone()) {
            return Err(format!("process_env_names repeats '{name}'"));
        }
    }
    let mut required = BTreeSet::new();
    match authorization {
        AuthorizationConfig::Bearer { token } => {
            collect_token_env(token, &mut required, interface_id)?
        }
        AuthorizationConfig::Sigv4 { credentials } => {
            collect_credential_env(credentials, &mut required, interface_id, 0)?
        }
    }
    if declared != required {
        return Err(format!(
            "process_env_names must exactly match the selected Bedrock auth source; declared={declared:?} required={required:?}"
        ));
    }
    Ok(())
}

fn collect_credential_env(
    source: &CredentialSource,
    target: &mut BTreeSet<String>,
    interface_id: &str,
    depth: usize,
) -> Result<(), String> {
    if depth > 32 {
        return Err("Bedrock credential source nesting exceeds 32 levels".to_string());
    }
    match source {
        CredentialSource::Chain { sources } => {
            if sources.is_empty() || sources.len() > 32 {
                return Err("Bedrock credential chain must contain 1..=32 sources".to_string());
            }
            for source in sources {
                collect_credential_env(source, target, interface_id, depth + 1)?;
            }
        }
        CredentialSource::Environment {
            access_key_env,
            secret_key_env,
            session_token_env,
        } => {
            collect_env(access_key_env, target, "access_key_env")?;
            collect_env(secret_key_env, target, "secret_key_env")?;
            if let Some(name) = session_token_env {
                collect_env(name, target, "session_token_env")?;
            }
        }
        CredentialSource::SecretHandles {
            access_key_ref,
            secret_key_ref,
            session_token_ref,
        } => {
            validate_secret_ref_scope(access_key_ref, interface_id)?;
            validate_secret_ref_scope(secret_key_ref, interface_id)?;
            if let Some(reference) = session_token_ref {
                validate_secret_ref_scope(reference, interface_id)?;
            }
        }
        CredentialSource::Profile {
            profile,
            credentials_file_env,
            config_file_env,
            max_role_depth,
            sts,
            credential_process,
            sso_cache_file_env,
            sso_portal_domain,
        } => {
            non_empty(profile, "profile")?;
            if *max_role_depth == 0 || *max_role_depth > 32 {
                return Err("AWS profile max_role_depth must be within 1..=32".to_string());
            }
            collect_env(credentials_file_env, target, "credentials_file_env")?;
            collect_env(config_file_env, target, "config_file_env")?;
            validate_sts_config(sts)?;
            if let Some(policy) = credential_process {
                validate_process_policy(policy, target)?;
            }
            if let Some(name) = sso_cache_file_env {
                collect_env(name, target, "sso_cache_file_env")?;
            }
            if let Some(domain) = sso_portal_domain {
                validate_sso_portal_domain(domain)?;
            }
        }
        CredentialSource::AssumeRole {
            role_arn,
            role_session_name,
            external_id,
            duration_seconds,
            source,
            sts,
        } => {
            validate_role(
                role_arn,
                role_session_name,
                external_id.as_deref(),
                *duration_seconds,
            )?;
            validate_sts_config(sts)?;
            collect_credential_env(source, target, interface_id, depth + 1)?;
        }
        CredentialSource::WebIdentity {
            role_arn_env,
            token_file_env,
            role_session_name_env,
            default_role_session_name,
            duration_seconds,
            sts,
        } => {
            collect_env(role_arn_env, target, "role_arn_env")?;
            collect_env(token_file_env, target, "token_file_env")?;
            if let Some(name) = role_session_name_env {
                collect_env(name, target, "role_session_name_env")?;
            }
            validate_role_session_name(default_role_session_name)?;
            validate_duration(*duration_seconds)?;
            validate_sts_config(sts)?;
        }
        CredentialSource::EcsContainer {
            relative_uri_env,
            full_uri_env,
            authorization_token_env,
            authorization_token_file_env,
            relative_base_url,
            allowed_full_uri_hosts,
            timeout_ms,
            max_response_bytes,
        } => {
            collect_env(relative_uri_env, target, "relative_uri_env")?;
            collect_env(full_uri_env, target, "full_uri_env")?;
            if let Some(name) = authorization_token_env {
                collect_env(name, target, "authorization_token_env")?;
            }
            if let Some(name) = authorization_token_file_env {
                collect_env(name, target, "authorization_token_file_env")?;
            }
            validate_ecs_base_url(relative_base_url)?;
            validate_allowed_metadata_hosts(allowed_full_uri_hosts)?;
            validate_io_bounds(*timeout_ms, *max_response_bytes, "ECS")?;
        }
        CredentialSource::ImdsV2 {
            endpoint,
            role_name,
            token_ttl_seconds,
            timeout_ms,
            max_response_bytes,
        } => {
            validate_imds_endpoint(endpoint)?;
            if let Some(role_name) = role_name {
                validate_role_name(role_name)?;
            }
            if *token_ttl_seconds == 0 || *token_ttl_seconds > 21_600 {
                return Err("IMDS token_ttl_seconds must be within 1..=21600".to_string());
            }
            validate_io_bounds(*timeout_ms, *max_response_bytes, "IMDS")?;
        }
        CredentialSource::CredentialProcess {
            executable_env,
            args,
            inherit_env_names,
            timeout_ms,
            max_stdout_bytes,
            max_stderr_bytes,
        } => {
            let policy = CredentialProcessPolicy {
                executable_env: executable_env.clone(),
                inherit_env_names: inherit_env_names.clone(),
                timeout_ms: *timeout_ms,
                max_stdout_bytes: *max_stdout_bytes,
                max_stderr_bytes: *max_stderr_bytes,
            };
            validate_process_policy(&policy, target)?;
            validate_process_args(args)?;
        }
        CredentialSource::Sso {
            account_id,
            role_name,
            region,
            portal_base_url,
            token,
            timeout_ms,
            max_response_bytes,
        } => {
            validate_sso(account_id, role_name, region, portal_base_url)?;
            collect_token_env(token, target, interface_id)?;
            validate_io_bounds(*timeout_ms, *max_response_bytes, "SSO")?;
        }
    }
    Ok(())
}

fn collect_token_env(
    source: &TokenSource,
    target: &mut BTreeSet<String>,
    interface_id: &str,
) -> Result<(), String> {
    match source {
        TokenSource::Environment { env } => collect_env(env, target, "token env"),
        TokenSource::SecretHandle { secret_ref } => {
            validate_secret_ref_scope(secret_ref, interface_id)?;
            Ok(())
        }
        TokenSource::CacheFileEnv { path_env } => {
            collect_env(path_env, target, "token cache path_env")
        }
    }
}

fn validate_secret_ref_scope(reference: &str, interface_id: &str) -> Result<(), String> {
    let (scope, _) = parse_secret_ref(reference)?;
    if scope.name() != Some(interface_id) {
        return Err(format!(
            "Bedrock interface '{interface_id}' secret reference must use its exact interface scope"
        ));
    }
    Ok(())
}

fn collect_env(name: &str, target: &mut BTreeSet<String>, surface: &str) -> Result<(), String> {
    validate_env_name(name, surface)?;
    target.insert(name.to_string());
    Ok(())
}

fn validate_env_name(name: &str, surface: &str) -> Result<(), String> {
    if name.is_empty()
        || name.trim() != name
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(format!(
            "Bedrock {surface} must contain only uppercase ASCII letters, digits, or '_'"
        ));
    }
    Ok(())
}

fn credentials_from_environment(
    access_key_env: &str,
    secret_key_env: &str,
    session_token_env: Option<&str>,
) -> Result<AwsCredentials, AuthFailure> {
    let access_key = optional_environment(access_key_env, "access key")?;
    let secret_key = optional_environment(secret_key_env, "secret key")?;
    let session_token = session_token_env
        .map(|name| optional_environment(name, "session token"))
        .transpose()?
        .flatten();
    let (access_key, secret_key) = match (access_key, secret_key, session_token.as_ref()) {
        (None, None, None) => {
            return Err(AuthFailure::unavailable(
                "environment_unconfigured",
                "AWS environment credentials are not configured",
            ));
        }
        (Some(access_key), Some(secret_key), _) => (access_key, secret_key),
        _ => {
            return Err(AuthFailure::invalid(
                "environment_partial",
                "AWS environment credentials are only partially configured",
            ));
        }
    };
    AwsCredentials::new(access_key, secret_key, session_token, None, "environment")
}

fn credentials_from_secrets(
    octopus_home: &Path,
    access_key_ref: &str,
    secret_key_ref: &str,
    session_token_ref: Option<&str>,
) -> Result<AwsCredentials, AuthFailure> {
    let manager = secrets_manager(octopus_home);
    let access_key = optional_secret_value(&manager, access_key_ref)?;
    let secret_key = optional_secret_value(&manager, secret_key_ref)?;
    let session_token = session_token_ref
        .map(|reference| optional_secret_value(&manager, reference))
        .transpose()?
        .flatten();
    let mut selected_handles = vec![access_key_ref, secret_key_ref];
    if session_token.is_some() {
        if let Some(reference) = session_token_ref {
            selected_handles.push(reference);
        }
    }
    match (access_key, secret_key, session_token.as_ref()) {
        (None, None, None) => Err(AuthFailure::unavailable(
            "secret_handles_unconfigured",
            "AWS secret-handle credentials are not configured",
        )),
        (Some(access_key), Some(secret_key), _) => AwsCredentials::new(
            access_key,
            secret_key,
            session_token,
            None,
            "secret_handles",
        )
        .map(|credentials| credentials.with_secret_handles(&selected_handles)),
        _ => Err(AuthFailure::invalid(
            "secret_handles_partial",
            "AWS secret-handle credentials are only partially configured",
        )),
    }
}

struct ProfileFiles {
    credentials: BTreeMap<String, BTreeMap<String, String>>,
    config: BTreeMap<String, BTreeMap<String, String>>,
}

struct ResolvedSsoProfile {
    account_id: String,
    role_name: String,
    region: String,
    portal_base_url: String,
}

#[derive(Clone, Copy)]
struct ProfileResolution<'a> {
    credentials_file_env: &'a str,
    config_file_env: &'a str,
    max_role_depth: usize,
    sts: &'a StsConfig,
    process_policy: Option<&'a CredentialProcessPolicy>,
    sso_cache_file_env: Option<&'a str>,
    sso_portal_domain: Option<&'a str>,
}

impl ProfileFiles {
    fn profile(&self, profile: &str) -> Option<BTreeMap<String, String>> {
        let section = if profile == "default" {
            "default".to_string()
        } else {
            format!("profile {profile}")
        };
        let mut values = self.config.get(&section).cloned().unwrap_or_default();
        if let Some(credentials) = self.credentials.get(profile) {
            values.extend(credentials.clone());
        }
        (!values.is_empty()).then_some(values)
    }

    fn resolve_sso(
        &self,
        values: &BTreeMap<String, String>,
        profile: &str,
        portal_domain: &str,
    ) -> Result<ResolvedSsoProfile, AuthFailure> {
        let mut values = values.clone();
        if let Some(session_name) = values.get("sso_session").cloned() {
            let session = self
                .config
                .get(&format!("sso-session {session_name}"))
                .ok_or_else(|| {
                    AuthFailure::invalid(
                        "sso_session_absent",
                        format!("AWS profile '{profile}' references an absent sso-session"),
                    )
                })?;
            for (key, value) in session {
                if values.get(key).is_some_and(|existing| existing != value) {
                    return Err(AuthFailure::invalid(
                        "sso_session_conflict",
                        format!("AWS profile '{profile}' conflicts with its sso-session"),
                    ));
                }
                values.insert(key.clone(), value.clone());
            }
        }
        let account_id = required_profile_value(&values, "sso_account_id", profile)?;
        let role_name = required_profile_value(&values, "sso_role_name", profile)?;
        let region = required_profile_value(&values, "sso_region", profile)?;
        let portal_base_url = sso_portal_base_url(&region, portal_domain)
            .map_err(|error| AuthFailure::invalid("sso_profile_invalid", error))?;
        validate_sso(&account_id, &role_name, &region, &portal_base_url)
            .map_err(|error| AuthFailure::invalid("sso_profile_invalid", error))?;
        Ok(ResolvedSsoProfile {
            account_id,
            role_name,
            region,
            portal_base_url,
        })
    }
}

fn load_profile_files(
    credentials_file_env: &str,
    config_file_env: &str,
) -> Result<ProfileFiles, AuthFailure> {
    let credentials_path = optional_absolute_path_environment(credentials_file_env)?;
    let config_path = optional_absolute_path_environment(config_file_env)?;
    if credentials_path.is_none() && config_path.is_none() {
        return Err(AuthFailure::unavailable(
            "profile_path_unconfigured",
            "AWS shared credential and config paths are not configured",
        ));
    }
    let credentials = credentials_path
        .as_deref()
        .map(parse_ini_file)
        .transpose()?
        .unwrap_or_default();
    let config = config_path
        .as_deref()
        .map(parse_ini_file)
        .transpose()?
        .unwrap_or_default();
    Ok(ProfileFiles {
        credentials,
        config,
    })
}

fn has_static_profile_credentials(values: &BTreeMap<String, String>) -> bool {
    values.contains_key("aws_access_key_id") || values.contains_key("aws_secret_access_key")
}

fn credentials_from_profile_values(
    values: &BTreeMap<String, String>,
    profile: &str,
) -> Result<AwsCredentials, AuthFailure> {
    let access_key = required_profile_value(values, "aws_access_key_id", profile)?;
    let secret_key = required_profile_value(values, "aws_secret_access_key", profile)?;
    let session_token = values
        .get("aws_session_token")
        .or_else(|| values.get("aws_security_token"))
        .cloned()
        .filter(|value| !value.is_empty());
    AwsCredentials::new(access_key, secret_key, session_token, None, "profile")
}

async fn credentials_from_assume_role(
    http: &Client,
    sts: &StsConfig,
    role_arn: &str,
    role_session_name: &str,
    external_id: Option<&str>,
    duration_seconds: Option<u32>,
    source: &AwsCredentials,
) -> Result<AwsCredentials, AuthFailure> {
    let body = assume_role_body(role_arn, role_session_name, external_id, duration_seconds)?;
    let endpoint = validate_sts_config(sts)
        .map_err(|error| AuthFailure::invalid("sts_endpoint_invalid", error))?;
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    let headers = sign_headers(
        &Method::POST,
        &endpoint,
        body.as_bytes(),
        headers,
        source,
        SigV4Scope {
            region: &sts.region,
            signing_name: &sts.signing_name,
            now: Utc::now(),
        },
    )
    .map_err(|error| AuthFailure::invalid("sts_signing_failed", error))?;
    let (status, bytes) = send_bounded(
        http.request(Method::POST, endpoint)
            .headers(headers)
            .body(body),
        sts.timeout_ms,
        sts.max_response_bytes,
        "STS AssumeRole",
    )
    .await?;
    parse_sts_credentials(status, &bytes, "assume_role").map(|mut credentials| {
        credentials.credential_handle_fingerprint = source.credential_handle_fingerprint.clone();
        credentials
    })
}

async fn credentials_from_web_identity(
    http: &Client,
    sts: &StsConfig,
    role_arn_env: &str,
    token_file_env: &str,
    role_session_name_env: Option<&str>,
    default_role_session_name: &str,
    duration_seconds: Option<u32>,
) -> Result<AwsCredentials, AuthFailure> {
    let role_arn = optional_environment(role_arn_env, "web identity role ARN")?;
    let token_path = optional_absolute_path_environment(token_file_env)?;
    let configured_session_name = role_session_name_env
        .map(|name| optional_environment(name, "web identity role session name"))
        .transpose()?
        .flatten();
    let (role_arn, token_path) = match (role_arn, token_path, configured_session_name.as_ref()) {
        (None, None, None) => {
            return Err(AuthFailure::unavailable(
                "web_identity_unconfigured",
                "AWS web identity is not configured",
            ));
        }
        (Some(role_arn), Some(token_path), _) => (role_arn, token_path),
        _ => {
            return Err(AuthFailure::invalid(
                "web_identity_partial",
                "AWS web identity is only partially configured",
            ));
        }
    };
    let role_session_name =
        configured_session_name.unwrap_or_else(|| default_role_session_name.to_string());
    validate_role(&role_arn, &role_session_name, None, duration_seconds)
        .map_err(|error| AuthFailure::invalid("web_identity_invalid", error))?;
    let token = read_file_bounded(
        &token_path,
        131_072,
        "web_identity_token_unreadable",
        "AWS web identity token file cannot be read",
    )?;
    let token = String::from_utf8(token).map_err(|_| {
        AuthFailure::invalid(
            "web_identity_token_malformed",
            "AWS web identity token is not UTF-8",
        )
    })?;
    let token = required_value(token, "AWS web identity token")?;
    let endpoint = validate_sts_config(sts)
        .map_err(|error| AuthFailure::invalid("sts_endpoint_invalid", error))?;
    let body = web_identity_body(&role_arn, &role_session_name, &token, duration_seconds)?;
    let (status, bytes) = send_bounded(
        http.request(Method::POST, endpoint)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body),
        sts.timeout_ms,
        sts.max_response_bytes,
        "STS AssumeRoleWithWebIdentity",
    )
    .await?;
    parse_sts_credentials(status, &bytes, "web_identity")
}

struct EcsCredentialRequest<'a> {
    relative_uri_env: &'a str,
    full_uri_env: &'a str,
    authorization_token_env: Option<&'a str>,
    authorization_token_file_env: Option<&'a str>,
    relative_base_url: &'a str,
    allowed_full_uri_hosts: &'a [String],
    timeout_ms: u64,
    max_response_bytes: usize,
}

async fn credentials_from_ecs(
    http: &Client,
    request: EcsCredentialRequest<'_>,
) -> Result<AwsCredentials, AuthFailure> {
    let EcsCredentialRequest {
        relative_uri_env,
        full_uri_env,
        authorization_token_env,
        authorization_token_file_env,
        relative_base_url,
        allowed_full_uri_hosts,
        timeout_ms,
        max_response_bytes,
    } = request;
    let relative = optional_environment(relative_uri_env, "ECS relative URI")?;
    let full = optional_environment(full_uri_env, "ECS full URI")?;
    let token =
        resolve_ecs_authorization_token(authorization_token_env, authorization_token_file_env)?;
    let url = match (relative, full, token.as_ref()) {
        (None, None, None) => {
            return Err(AuthFailure::unavailable(
                "ecs_unconfigured",
                "ECS container credentials are not configured",
            ));
        }
        (Some(relative), None, _) => {
            validate_relative_metadata_uri(&relative)?;
            validate_ecs_base_url(relative_base_url)
                .map_err(|error| AuthFailure::invalid("ecs_base_invalid", error))?
                .join(&relative)
                .map_err(|_| {
                    AuthFailure::invalid("ecs_relative_uri_invalid", "ECS relative URI is invalid")
                })?
        }
        (None, Some(full), _) => validate_ecs_full_url(&full, allowed_full_uri_hosts)
            .map_err(|error| AuthFailure::invalid("ecs_full_uri_invalid", error))?,
        (Some(_), Some(_), _) => {
            return Err(AuthFailure::invalid(
                "ecs_uri_ambiguous",
                "ECS relative and full credential URIs are both configured",
            ));
        }
        (None, None, Some(_)) => {
            return Err(AuthFailure::invalid(
                "ecs_uri_partial",
                "ECS authorization is configured without a credential URI",
            ));
        }
    };
    let mut request = http
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(token) = token {
        request = request.header(AUTHORIZATION, token);
    }
    let (status, bytes) =
        send_bounded(request, timeout_ms, max_response_bytes, "ECS credentials").await?;
    parse_json_credentials(status, &bytes, "ecs_container")
}

async fn credentials_from_imds(
    http: &Client,
    endpoint: &str,
    role_name: Option<&str>,
    token_ttl_seconds: u32,
    timeout_ms: u64,
    max_response_bytes: usize,
) -> Result<AwsCredentials, AuthFailure> {
    let base = validate_imds_endpoint(endpoint)
        .map_err(|error| AuthFailure::invalid("imds_endpoint_invalid", error))?;
    let token_url = base
        .join("/latest/api/token")
        .map_err(|_| AuthFailure::invalid("imds_token_url_invalid", "IMDS token URL is invalid"))?;
    let (status, token_bytes) = send_bounded(
        http.put(token_url).header(
            "x-aws-ec2-metadata-token-ttl-seconds",
            token_ttl_seconds.to_string(),
        ),
        timeout_ms,
        max_response_bytes.min(16_384),
        "IMDS token",
    )
    .await
    .map_err(|_| {
        AuthFailure::unavailable_retryable(
            "imds_unavailable",
            "IMDSv2 token endpoint is unavailable",
        )
    })?;
    if !status.is_success() {
        return Err(AuthFailure::unavailable(
            "imds_unavailable",
            format!("IMDSv2 token endpoint returned HTTP {}", status.as_u16()),
        ));
    }
    let token = String::from_utf8(token_bytes)
        .map_err(|_| AuthFailure::invalid("imds_token_malformed", "IMDSv2 token is not UTF-8"))?;
    let token = required_value(token, "IMDSv2 token")?;
    let role_name = match role_name {
        Some(role_name) => role_name.to_string(),
        None => {
            let role_url = base
                .join("/latest/meta-data/iam/security-credentials/")
                .map_err(|_| {
                    AuthFailure::invalid("imds_role_url_invalid", "IMDS role URL is invalid")
                })?;
            let (status, bytes) = send_bounded(
                http.get(role_url)
                    .header("x-aws-ec2-metadata-token", &token),
                timeout_ms,
                4096,
                "IMDS role",
            )
            .await
            .map_err(|_| {
                AuthFailure::unavailable_retryable(
                    "imds_unavailable",
                    "IMDS role endpoint is unavailable",
                )
            })?;
            if !status.is_success() {
                return Err(AuthFailure::unavailable(
                    "imds_unavailable",
                    format!("IMDS role endpoint returned HTTP {}", status.as_u16()),
                ));
            }
            let role = String::from_utf8(bytes).map_err(|_| {
                AuthFailure::invalid("imds_role_malformed", "IMDS role name is not UTF-8")
            })?;
            let role = role.trim().to_string();
            validate_role_name(&role)
                .map_err(|error| AuthFailure::invalid("imds_role_malformed", error))?;
            role
        }
    };
    let encoded_role = aws_percent_encode(role_name.as_bytes());
    let credential_url = base
        .join(&format!(
            "/latest/meta-data/iam/security-credentials/{encoded_role}"
        ))
        .map_err(|_| {
            AuthFailure::invalid(
                "imds_credentials_url_invalid",
                "IMDS credential URL is invalid",
            )
        })?;
    let (status, bytes) = send_bounded(
        http.get(credential_url)
            .header("x-aws-ec2-metadata-token", token),
        timeout_ms,
        max_response_bytes,
        "IMDS credentials",
    )
    .await
    .map_err(|_| {
        AuthFailure::unavailable_retryable(
            "imds_unavailable",
            "IMDS credential endpoint is unavailable",
        )
    })?;
    if !status.is_success() {
        return Err(AuthFailure::unavailable(
            "imds_unavailable",
            format!("IMDS credential endpoint returned HTTP {}", status.as_u16()),
        ));
    }
    parse_imds_credentials(&bytes)
}

fn parse_imds_credentials(bytes: &[u8]) -> Result<AwsCredentials, AuthFailure> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        AuthFailure::invalid(
            "imds_credentials_malformed",
            "IMDSv2 credential response is malformed",
        )
    })?;
    let code = value
        .get("Code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if code != "Success" {
        return Err(AuthFailure::rejected(
            "imds_credentials_rejected",
            "IMDSv2 credential response code is not Success",
            false,
        ));
    }
    let access_key = json_string(&value, "AccessKeyId", "IMDSv2 credentials")?;
    let secret_key = json_string(&value, "SecretAccessKey", "IMDSv2 credentials")?;
    let session_token = Some(json_string(&value, "Token", "IMDSv2 credentials")?);
    let expires_at = parse_rfc3339(
        &json_string(&value, "Expiration", "IMDSv2 credentials")?,
        "IMDSv2 Expiration",
    )?;
    AwsCredentials::new(
        access_key,
        secret_key,
        session_token,
        Some(expires_at),
        "imds_v2",
    )
}

struct SsoCredentialRequest<'a> {
    account_id: &'a str,
    role_name: &'a str,
    region: &'a str,
    portal_base_url: &'a str,
    timeout_ms: u64,
    max_response_bytes: usize,
}

async fn credentials_from_sso(
    http: &Client,
    request: SsoCredentialRequest<'_>,
    access_token: &str,
) -> Result<AwsCredentials, AuthFailure> {
    let SsoCredentialRequest {
        account_id,
        role_name,
        region,
        portal_base_url,
        timeout_ms,
        max_response_bytes,
    } = request;
    validate_sso(account_id, role_name, region, portal_base_url)
        .map_err(|error| AuthFailure::invalid("sso_config_invalid", error))?;
    let mut url = validate_https_base_url(portal_base_url, "SSO portal_base_url")
        .map_err(|error| AuthFailure::invalid("sso_endpoint_invalid", error))?;
    url.set_path("/federation/credentials");
    url.query_pairs_mut()
        .clear()
        .append_pair("account_id", account_id)
        .append_pair("role_name", role_name);
    let (status, bytes) = send_bounded(
        http.get(url).header("x-amz-sso_bearer_token", access_token),
        timeout_ms,
        max_response_bytes,
        "SSO role",
    )
    .await?;
    if !status.is_success() {
        let retryable = status.as_u16() == 429 || status.as_u16() >= 500;
        return Err(AuthFailure::rejected(
            "sso_role_rejected",
            format!("SSO role endpoint returned HTTP {}", status.as_u16()),
            retryable,
        ));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        AuthFailure::invalid("sso_response_malformed", "SSO role response is malformed")
    })?;
    let role = value.get("roleCredentials").ok_or_else(|| {
        AuthFailure::invalid(
            "sso_response_malformed",
            "SSO response has no roleCredentials",
        )
    })?;
    let access_key = json_string(role, "accessKeyId", "SSO roleCredentials")?;
    let secret_key = json_string(role, "secretAccessKey", "SSO roleCredentials")?;
    let session_token = Some(json_string(role, "sessionToken", "SSO roleCredentials")?);
    let expiration_ms = role
        .get("expiration")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            AuthFailure::invalid(
                "sso_response_malformed",
                "SSO roleCredentials.expiration is malformed",
            )
        })?;
    let expires_at = DateTime::<Utc>::from_timestamp_millis(expiration_ms).ok_or_else(|| {
        AuthFailure::invalid(
            "sso_response_malformed",
            "SSO roleCredentials.expiration is out of range",
        )
    })?;
    AwsCredentials::new(
        access_key,
        secret_key,
        session_token,
        Some(expires_at),
        "sso",
    )
}

async fn credentials_from_process(
    executable: &Path,
    args: &[String],
    inherit_env_names: &[String],
    timeout_ms: u64,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
) -> Result<AwsCredentials, AuthFailure> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in inherit_env_names {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = command.spawn().map_err(|_| {
        AuthFailure::invalid(
            "credential_process_spawn_failed",
            "credential_process could not be started",
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        AuthFailure::invalid(
            "credential_process_pipe_failed",
            "credential_process stdout pipe is unavailable",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        AuthFailure::invalid(
            "credential_process_pipe_failed",
            "credential_process stderr pipe is unavailable",
        )
    })?;
    let stdout_task = tokio::spawn(read_limited(stdout, max_stdout_bytes));
    let stderr_task = tokio::spawn(read_limited(stderr, max_stderr_bytes));
    let status = match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
        Ok(result) => result.map_err(|_| {
            AuthFailure::transport(
                "credential_process_wait_failed",
                "credential_process status could not be read",
            )
        })?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(AuthFailure::invalid(
                "credential_process_timeout",
                "credential_process exceeded its descriptor timeout",
            ));
        }
    };
    let stdout = stdout_task.await.map_err(|_| {
        AuthFailure::invalid(
            "credential_process_stdout_failed",
            "credential_process stdout reader failed",
        )
    })??;
    let stderr = stderr_task.await.map_err(|_| {
        AuthFailure::invalid(
            "credential_process_stderr_failed",
            "credential_process stderr reader failed",
        )
    })??;
    if !status.success() {
        return Err(AuthFailure {
            detail: "credential_process returned a non-zero status".to_string(),
            retryable: false,
            evidence: json!({
                "kind": "rejected",
                "code": "credential_process_nonzero",
                "exit_code": status.code(),
                "stderr_bytes": stderr.len(),
                "stderr_blake3": blake3::hash(&stderr).to_hex().to_string()
            }),
        });
    }
    parse_process_credentials(&stdout)
}

async fn read_limited<R>(mut reader: R, maximum: usize) -> Result<Vec<u8>, AuthFailure>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await.map_err(|_| {
            AuthFailure::transport(
                "credential_process_read_failed",
                "credential_process output could not be read",
            )
        })?;
        if read == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(read) > maximum {
            return Err(AuthFailure::invalid(
                "credential_process_output_too_large",
                "credential_process output exceeds its descriptor bound",
            ));
        }
        output.extend_from_slice(&chunk[..read]);
    }
}

fn parse_process_credentials(bytes: &[u8]) -> Result<AwsCredentials, AuthFailure> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        AuthFailure::invalid(
            "credential_process_malformed",
            "credential_process stdout is not valid JSON",
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        AuthFailure::invalid(
            "credential_process_malformed",
            "credential_process stdout is not a JSON object",
        )
    })?;
    if object.get("Version").and_then(Value::as_u64) != Some(1) {
        return Err(AuthFailure::invalid(
            "credential_process_version",
            "credential_process Version must equal 1",
        ));
    }
    let access_key = json_string(&value, "AccessKeyId", "credential_process")?;
    let secret_key = json_string(&value, "SecretAccessKey", "credential_process")?;
    let session_token = value
        .get("SessionToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    let expires_at = value
        .get("Expiration")
        .and_then(Value::as_str)
        .map(|value| parse_rfc3339(value, "credential_process Expiration"))
        .transpose()?;
    AwsCredentials::new(
        access_key,
        secret_key,
        session_token,
        expires_at,
        "credential_process",
    )
}

fn validate_sts_config(config: &StsConfig) -> Result<Url, String> {
    validate_region(&config.region)?;
    validate_signing_name(&config.signing_name)?;
    validate_role_session_name(&config.default_role_session_name)?;
    validate_io_bounds(config.timeout_ms, config.max_response_bytes, "STS")?;
    let url = validate_https_base_url(&config.endpoint, "STS endpoint")?;
    if url.path() != "/" || url.port().is_some() {
        return Err("STS endpoint must use the default HTTPS port and root path".to_string());
    }
    let host = url.host_str().unwrap_or_default();
    let expected = format!("sts.{}.amazonaws.com", config.region);
    let expected_cn = format!("sts.{}.amazonaws.com.cn", config.region);
    if host != expected && host != expected_cn {
        return Err("STS endpoint host must exactly match its declared region".to_string());
    }
    Ok(url)
}

fn validate_role(
    role_arn: &str,
    role_session_name: &str,
    external_id: Option<&str>,
    duration_seconds: Option<u32>,
) -> Result<(), String> {
    if !role_arn.starts_with("arn:") || role_arn.len() > 2048 || role_arn.trim() != role_arn {
        return Err("AWS role ARN is malformed".to_string());
    }
    validate_role_session_name(role_session_name)?;
    if external_id.is_some_and(|value| value.is_empty() || value.len() > 1224) {
        return Err("AWS external_id must contain 1..=1224 bytes".to_string());
    }
    validate_duration(duration_seconds)
}

fn validate_role_session_name(value: &str) -> Result<(), String> {
    if value.len() < 2
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'+' | b'=' | b',' | b'.' | b'@' | b'-')
        })
    {
        return Err("AWS role session name is malformed".to_string());
    }
    Ok(())
}

fn validate_role_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value.trim() != value
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'+' | b'=' | b',' | b'.' | b'@' | b'-')
        })
    {
        return Err("AWS metadata role name is malformed".to_string());
    }
    Ok(())
}

fn validate_duration(value: Option<u32>) -> Result<(), String> {
    if value.is_some_and(|value| !(900..=43_200).contains(&value)) {
        return Err("AWS role duration_seconds must be within 900..=43200".to_string());
    }
    Ok(())
}

fn validate_process_policy(
    policy: &CredentialProcessPolicy,
    target: &mut BTreeSet<String>,
) -> Result<(), String> {
    collect_env(
        &policy.executable_env,
        target,
        "credential process executable_env",
    )?;
    for name in &policy.inherit_env_names {
        collect_env(name, target, "credential process inherit_env_names")?;
    }
    if policy.timeout_ms == 0 || policy.timeout_ms > 60_000 {
        return Err("credential_process timeout_ms must be within 1..=60000".to_string());
    }
    if policy.max_stdout_bytes == 0
        || policy.max_stdout_bytes > 1_048_576
        || policy.max_stderr_bytes == 0
        || policy.max_stderr_bytes > 1_048_576
    {
        return Err("credential_process output bounds must be within 1..=1048576".to_string());
    }
    Ok(())
}

fn validate_process_args(args: &[String]) -> Result<(), String> {
    if args.len() > 128
        || args
            .iter()
            .any(|arg| arg.is_empty() || arg.len() > 16_384 || arg.as_bytes().contains(&0))
    {
        return Err("credential_process args violate descriptor bounds".to_string());
    }
    Ok(())
}

fn validate_sso(
    account_id: &str,
    role_name: &str,
    region: &str,
    portal_base_url: &str,
) -> Result<(), String> {
    if account_id.len() != 12 || !account_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("SSO account_id must contain exactly 12 digits".to_string());
    }
    non_empty(role_name, "SSO role_name")?;
    validate_region(region)?;
    let url = validate_https_base_url(portal_base_url, "SSO portal_base_url")?;
    if url.path() != "/" || url.port().is_some() {
        return Err(
            "SSO portal_base_url must use the default HTTPS port and root path".to_string(),
        );
    }
    let host = url.host_str().unwrap_or_default();
    let expected = format!("portal.sso.{region}.amazonaws.com");
    let expected_cn = format!("portal.sso.{region}.amazonaws.com.cn");
    if host != expected && host != expected_cn {
        return Err("SSO portal host must exactly match its declared region".to_string());
    }
    Ok(())
}

fn validate_sso_portal_domain(domain: &str) -> Result<(), String> {
    if !matches!(domain, "amazonaws.com" | "amazonaws.com.cn") {
        return Err("SSO portal domain must be amazonaws.com or amazonaws.com.cn".to_string());
    }
    Ok(())
}

fn sso_portal_base_url(region: &str, domain: &str) -> Result<String, String> {
    validate_region(region)?;
    validate_sso_portal_domain(domain)?;
    Ok(format!("https://portal.sso.{region}.{domain}"))
}

fn validate_io_bounds(
    timeout_ms: u64,
    max_response_bytes: usize,
    surface: &str,
) -> Result<(), String> {
    if timeout_ms == 0 || timeout_ms > 60_000 {
        return Err(format!("{surface} timeout_ms must be within 1..=60000"));
    }
    if max_response_bytes == 0 || max_response_bytes > 1_048_576 {
        return Err(format!(
            "{surface} max_response_bytes must be within 1..=1048576"
        ));
    }
    Ok(())
}

fn validate_ecs_base_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "ECS relative base URL is invalid".to_string())?;
    if url.scheme() != "http"
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.port().is_some()
        || !matches!(
            url.host_str(),
            Some("169.254.170.2" | "169.254.170.23" | "fd00:ec2::23")
        )
    {
        return Err("ECS relative base URL is not an allowed metadata endpoint".to_string());
    }
    Ok(url)
}

fn validate_allowed_metadata_hosts(hosts: &[String]) -> Result<(), String> {
    if hosts.is_empty() || hosts.len() > 16 {
        return Err("ECS allowed_full_uri_hosts must contain 1..=16 hosts".to_string());
    }
    let mut seen = BTreeSet::new();
    for host in hosts {
        if host.is_empty()
            || host.trim() != host
            || host.len() > 255
            || !seen.insert(host.to_ascii_lowercase())
        {
            return Err("ECS allowed_full_uri_hosts contains an invalid host".to_string());
        }
    }
    Ok(())
}

fn validate_ecs_full_url(value: &str, allowed_hosts: &[String]) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "ECS full URI is invalid".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("ECS full URI has a forbidden URL component".to_string());
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if !allowed_hosts
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&host))
    {
        return Err("ECS full URI host is not descriptor-allowlisted".to_string());
    }
    if url.scheme() == "http" && !metadata_http_host_allowed(&host) {
        return Err("ECS full HTTP URI host is not link-local or loopback".to_string());
    }
    Ok(url)
}

fn metadata_http_host_allowed(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|address| {
        address.is_loopback()
            || matches!(
                address,
                IpAddr::V4(value)
                    if value == std::net::Ipv4Addr::new(169, 254, 170, 2)
                        || value == std::net::Ipv4Addr::new(169, 254, 170, 23)
            )
            || matches!(
                address,
                IpAddr::V6(value)
                    if value == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0023)
            )
    })
}

fn validate_relative_metadata_uri(value: &str) -> Result<(), AuthFailure> {
    if !value.starts_with('/')
        || value.starts_with("//")
        || value.contains('#')
        || value.contains("\r")
        || value.contains("\n")
        || value.len() > 4096
    {
        return Err(AuthFailure::invalid(
            "ecs_relative_uri_invalid",
            "ECS relative credential URI is malformed",
        ));
    }
    Ok(())
}

fn resolve_ecs_authorization_token(
    token_env: Option<&str>,
    token_file_env: Option<&str>,
) -> Result<Option<String>, AuthFailure> {
    let direct = token_env
        .map(|name| optional_environment(name, "ECS authorization token"))
        .transpose()?
        .flatten();
    let file = token_file_env
        .map(optional_absolute_path_environment)
        .transpose()?
        .flatten();
    if direct.is_some() && file.is_some() {
        return Err(AuthFailure::invalid(
            "ecs_authorization_ambiguous",
            "ECS authorization token and token file are both configured",
        ));
    }
    let token = match (direct, file) {
        (Some(token), None) => Some(token),
        (None, Some(path)) => {
            let bytes = read_file_bounded(
                &path,
                16_384,
                "ecs_authorization_unreadable",
                "ECS authorization token file cannot be read",
            )?;
            Some(String::from_utf8(bytes).map_err(|_| {
                AuthFailure::invalid(
                    "ecs_authorization_malformed",
                    "ECS authorization token is not UTF-8",
                )
            })?)
        }
        _ => None,
    };
    if token
        .as_ref()
        .is_some_and(|token| token.contains('\r') || token.contains('\n'))
    {
        return Err(AuthFailure::invalid(
            "ecs_authorization_malformed",
            "ECS authorization token contains a line break",
        ));
    }
    Ok(token)
}

fn form_encode(fields: &[(&str, String)]) -> String {
    fields
        .iter()
        .map(|(name, value)| {
            format!(
                "{}={}",
                aws_percent_encode(name.as_bytes()),
                aws_percent_encode(value.as_bytes())
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn assume_role_body(
    role_arn: &str,
    role_session_name: &str,
    external_id: Option<&str>,
    duration_seconds: Option<u32>,
) -> Result<String, AuthFailure> {
    validate_role(role_arn, role_session_name, external_id, duration_seconds)
        .map_err(|error| AuthFailure::invalid("assume_role_invalid", error))?;
    let mut fields = vec![
        ("Action", "AssumeRole".to_string()),
        ("Version", "2011-06-15".to_string()),
        ("RoleArn", role_arn.to_string()),
        ("RoleSessionName", role_session_name.to_string()),
    ];
    if let Some(external_id) = external_id {
        fields.push(("ExternalId", external_id.to_string()));
    }
    if let Some(duration_seconds) = duration_seconds {
        fields.push(("DurationSeconds", duration_seconds.to_string()));
    }
    Ok(form_encode(&fields))
}

fn web_identity_body(
    role_arn: &str,
    role_session_name: &str,
    token: &str,
    duration_seconds: Option<u32>,
) -> Result<String, AuthFailure> {
    validate_role(role_arn, role_session_name, None, duration_seconds)
        .map_err(|error| AuthFailure::invalid("web_identity_invalid", error))?;
    let mut fields = vec![
        ("Action", "AssumeRoleWithWebIdentity".to_string()),
        ("Version", "2011-06-15".to_string()),
        ("RoleArn", role_arn.to_string()),
        ("RoleSessionName", role_session_name.to_string()),
        ("WebIdentityToken", token.to_string()),
    ];
    if let Some(duration_seconds) = duration_seconds {
        fields.push(("DurationSeconds", duration_seconds.to_string()));
    }
    Ok(form_encode(&fields))
}

async fn send_bounded(
    request: reqwest::RequestBuilder,
    timeout_ms: u64,
    maximum: usize,
    surface: &str,
) -> Result<(StatusCode, Vec<u8>), AuthFailure> {
    tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        let mut response = request.send().await.map_err(|_| {
            AuthFailure::transport(
                "credential_http_transport",
                format!("{surface} request failed"),
            )
        })?;
        if response
            .content_length()
            .is_some_and(|length| length > maximum as u64)
        {
            return Err(AuthFailure::invalid(
                "credential_http_body_too_large",
                format!("{surface} response exceeds its descriptor bound"),
            ));
        }
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            AuthFailure::transport(
                "credential_http_body_transport",
                format!("{surface} response body failed"),
            )
        })? {
            if bytes.len().saturating_add(chunk.len()) > maximum {
                return Err(AuthFailure::invalid(
                    "credential_http_body_too_large",
                    format!("{surface} response exceeds its descriptor bound"),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((status, bytes))
    })
    .await
    .map_err(|_| {
        AuthFailure::transport(
            "credential_http_timeout",
            format!("{surface} request exceeded its descriptor timeout"),
        )
    })?
}

fn parse_json_credentials(
    status: StatusCode,
    bytes: &[u8],
    source: &'static str,
) -> Result<AwsCredentials, AuthFailure> {
    if !status.is_success() {
        return Err(AuthFailure {
            detail: format!(
                "{source} credential endpoint returned HTTP {}",
                status.as_u16()
            ),
            retryable: status.as_u16() == 429 || status.is_server_error(),
            evidence: json!({
                "kind": "rejected",
                "code": "credential_endpoint_rejected",
                "status": status.as_u16(),
                "body_bytes": bytes.len(),
                "body_blake3": blake3::hash(bytes).to_hex().to_string()
            }),
        });
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        AuthFailure::invalid(
            "credential_response_malformed",
            format!("{source} credential response is malformed"),
        )
    })?;
    let access_key = json_string(&value, "AccessKeyId", source)?;
    let secret_key = json_string(&value, "SecretAccessKey", source)?;
    let session_token = Some(json_string(&value, "Token", source)?);
    let expires_at = parse_rfc3339(&json_string(&value, "Expiration", source)?, source)?;
    AwsCredentials::new(
        access_key,
        secret_key,
        session_token,
        Some(expires_at),
        source,
    )
}

fn parse_sts_credentials(
    status: StatusCode,
    bytes: &[u8],
    source: &'static str,
) -> Result<AwsCredentials, AuthFailure> {
    if !status.is_success() {
        return Err(AuthFailure {
            detail: format!("STS credential request returned HTTP {}", status.as_u16()),
            retryable: status.as_u16() == 429 || status.is_server_error(),
            evidence: json!({
                "kind": "rejected",
                "code": "sts_rejected",
                "status": status.as_u16(),
                "body_bytes": bytes.len(),
                "body_blake3": blake3::hash(bytes).to_hex().to_string()
            }),
        });
    }
    let access_key = xml_tag(bytes, "AccessKeyId")?;
    let secret_key = xml_tag(bytes, "SecretAccessKey")?;
    let session_token = Some(xml_tag(bytes, "SessionToken")?);
    let expires_at = parse_rfc3339(&xml_tag(bytes, "Expiration")?, "STS Expiration")?;
    AwsCredentials::new(
        access_key,
        secret_key,
        session_token,
        Some(expires_at),
        source,
    )
}

fn xml_tag(bytes: &[u8], tag: &str) -> Result<String, AuthFailure> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| AuthFailure::invalid("sts_response_malformed", "STS response is not UTF-8"))?;
    let start = format!("<{tag}>");
    let end = format!("</{tag}>");
    let start_index = text.find(&start).ok_or_else(|| {
        AuthFailure::invalid(
            "sts_response_malformed",
            "STS response is missing credentials",
        )
    })? + start.len();
    let tail = &text[start_index..];
    let end_index = tail.find(&end).ok_or_else(|| {
        AuthFailure::invalid(
            "sts_response_malformed",
            "STS response is missing credentials",
        )
    })?;
    decode_xml_entities(&tail[..end_index])
}

fn decode_xml_entities(value: &str) -> Result<String, AuthFailure> {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        rest = &rest[index..];
        let (decoded, consumed) = if rest.starts_with("&amp;") {
            ('&', 5)
        } else if rest.starts_with("&lt;") {
            ('<', 4)
        } else if rest.starts_with("&gt;") {
            ('>', 4)
        } else if rest.starts_with("&quot;") {
            ('"', 6)
        } else if rest.starts_with("&apos;") {
            ('\'', 6)
        } else {
            return Err(AuthFailure::invalid(
                "sts_response_malformed",
                "STS response contains an unsupported XML entity",
            ));
        };
        output.push(decoded);
        rest = &rest[consumed..];
    }
    output.push_str(rest);
    required_value(output, "STS XML credential")
}

fn parse_u32(value: &str, surface: &str) -> Result<u32, AuthFailure> {
    value.parse::<u32>().map_err(|_| {
        AuthFailure::invalid(
            "profile_number_malformed",
            format!("{surface} is not an unsigned integer"),
        )
    })
}

fn parse_process_command(value: &str) -> Result<Vec<String>, AuthFailure> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            } else {
                current.push(character);
            }
            continue;
        }
        if character.is_ascii_whitespace() && quote.is_none() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped || quote.is_some() {
        return Err(AuthFailure::invalid(
            "credential_process_command_malformed",
            "credential_process command has an unfinished escape or quote",
        ));
    }
    if !current.is_empty() {
        args.push(current);
    }
    validate_process_args(&args)
        .map_err(|error| AuthFailure::invalid("credential_process_command_malformed", error))?;
    if args.is_empty() {
        return Err(AuthFailure::invalid(
            "credential_process_command_malformed",
            "credential_process command is empty",
        ));
    }
    Ok(args)
}

impl AwsCredentials {
    fn new(
        access_key: String,
        secret_key: String,
        session_token: Option<String>,
        expires_at: Option<DateTime<Utc>>,
        source: &'static str,
    ) -> Result<Self, AuthFailure> {
        let access_key = required_value(access_key, "AWS access key")?;
        let secret_key = required_value(secret_key, "AWS secret key")?;
        let session_token = session_token
            .map(|value| required_value(value, "AWS session token"))
            .transpose()?;
        if expires_at.is_some_and(|expiry| expiry <= Utc::now()) {
            return Err(AuthFailure::invalid(
                "credentials_expired",
                format!("AWS credentials from '{source}' are expired"),
            ));
        }
        Ok(Self {
            access_key,
            secret_key,
            session_token,
            expires_at,
            credential_handle_fingerprint: None,
        })
    }

    fn with_secret_handles(mut self, handles: &[&str]) -> Self {
        self.credential_handle_fingerprint = Some(secret_handle_fingerprint(handles));
        self
    }
}

fn secret_handle_fingerprint(handles: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"octopus.provider.credential_handles.v1");
    for handle in handles {
        hasher.update(&(handle.len() as u64).to_le_bytes());
        hasher.update(handle.as_bytes());
    }
    blake3::Hasher::finalize(&hasher).to_hex().to_string()
}

fn sign_headers(
    method: &Method,
    url: &Url,
    body: &[u8],
    mut headers: HeaderMap,
    credentials: &AwsCredentials,
    scope: SigV4Scope<'_>,
) -> Result<HeaderMap, String> {
    if headers.contains_key(AUTHORIZATION) {
        return Err("SigV4 input headers already contain authorization".to_string());
    }
    let host = canonical_host(url)?;
    headers.insert(
        HOST,
        HeaderValue::from_str(&host).map_err(|error| format!("host header is invalid: {error}"))?,
    );
    let amz_date = scope.now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = scope.now.format("%Y%m%d").to_string();
    let payload_hash = hex_sha256(body);
    headers.insert(
        HeaderName::from_static("x-amz-date"),
        HeaderValue::from_str(&amz_date)
            .map_err(|error| format!("x-amz-date is invalid: {error}"))?,
    );
    headers.insert(
        HeaderName::from_static("x-amz-content-sha256"),
        HeaderValue::from_str(&payload_hash)
            .map_err(|error| format!("payload hash header is invalid: {error}"))?,
    );
    if let Some(token) = &credentials.session_token {
        headers.insert(
            HeaderName::from_static("x-amz-security-token"),
            HeaderValue::from_str(token)
                .map_err(|error| format!("AWS session token header is invalid: {error}"))?,
        );
    }
    let (canonical_headers, signed_headers) = canonical_headers(&headers)?;
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.as_str(),
        canonical_uri(url),
        canonical_query(url),
        canonical_headers,
        signed_headers,
        payload_hash
    );
    let credential_scope = format!(
        "{date}/{}/{}/aws4_request",
        scope.region, scope.signing_name
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );
    let date_key = hmac_sha256(
        format!("AWS4{}", credentials.secret_key).as_bytes(),
        date.as_bytes(),
    )?;
    let region_key = hmac_sha256(&date_key, scope.region.as_bytes())?;
    let service_key = hmac_sha256(&region_key, scope.signing_name.as_bytes())?;
    let signing_key = hmac_sha256(&service_key, b"aws4_request")?;
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{credential_scope},SignedHeaders={signed_headers},Signature={signature}",
        credentials.access_key
    );
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&authorization)
            .map_err(|error| format!("authorization header is invalid: {error}"))?,
    );
    Ok(headers)
}

fn canonical_headers(headers: &HeaderMap) -> Result<(String, String), String> {
    let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in headers.keys() {
        if name == AUTHORIZATION {
            continue;
        }
        let normalized_name = name.as_str().to_ascii_lowercase();
        for value in headers.get_all(name).iter() {
            let value = value.to_str().map_err(|error| {
                format!("SigV4 header '{normalized_name}' is not UTF-8: {error}")
            })?;
            values
                .entry(normalized_name.clone())
                .or_default()
                .push(normalize_header_value(value));
        }
    }
    let signed_headers = values.keys().cloned().collect::<Vec<_>>().join(";");
    let mut canonical = String::new();
    for (name, parts) in values {
        canonical.push_str(&name);
        canonical.push(':');
        canonical.push_str(&parts.join(","));
        canonical.push('\n');
    }
    Ok((canonical, signed_headers))
}

fn canonical_uri(url: &Url) -> String {
    let path = url.path();
    if path.is_empty() {
        "/".to_string()
    } else {
        path.replace('%', "%25")
    }
}

fn canonical_query(url: &Url) -> String {
    let mut pairs = url
        .query_pairs()
        .map(|(name, value)| {
            (
                aws_percent_encode(name.as_bytes()),
                aws_percent_encode(value.as_bytes()),
            )
        })
        .collect::<Vec<_>>();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) fn aws_percent_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len());
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(*byte as char);
        } else {
            output.push('%');
            output.push_str(&format!("{byte:02X}"));
        }
    }
    output
}

fn canonical_host(url: &Url) -> Result<String, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "SigV4 URL has no host".to_string())?;
    let mut value = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    if let Some(port) = url.port() {
        let default =
            (url.scheme() == "https" && port == 443) || (url.scheme() == "http" && port == 80);
        if !default {
            value.push(':');
            value.push_str(&port.to_string());
        }
    }
    Ok(value)
}

fn normalize_header_value(value: &str) -> String {
    value.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

fn hmac_sha256(key: &[u8], value: &[u8]) -> Result<Vec<u8>, String> {
    let mut hmac = HmacSha256::new_from_slice(key)
        .map_err(|error| format!("HMAC key construction failed: {error}"))?;
    hmac.update(value);
    Ok(hmac.finalize().into_bytes().to_vec())
}

fn hex_sha256(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn parse_ini_file(path: &Path) -> Result<BTreeMap<String, BTreeMap<String, String>>, AuthFailure> {
    let bytes = read_file_bounded(
        path,
        4 * 1024 * 1024,
        "profile_file_unreadable",
        "explicit AWS profile file cannot be read",
    )?;
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        AuthFailure::invalid(
            "profile_file_malformed",
            "explicit AWS profile file is not UTF-8",
        )
    })?;
    let mut sections = BTreeMap::<String, BTreeMap<String, String>>::new();
    let mut section: Option<String> = None;
    for (line_index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = &line[1..line.len() - 1];
            non_empty(name, "AWS profile section")
                .map_err(|error| AuthFailure::invalid("profile_file_malformed", error))?;
            if sections.contains_key(name) {
                return Err(AuthFailure::invalid(
                    "profile_file_malformed",
                    "AWS profile file repeats a section",
                ));
            }
            sections.insert(name.to_string(), BTreeMap::new());
            section = Some(name.to_string());
            continue;
        }
        let active = section.as_ref().ok_or_else(|| {
            AuthFailure::invalid(
                "profile_file_malformed",
                format!(
                    "AWS profile file line {} is outside a section",
                    line_index + 1
                ),
            )
        })?;
        let (key, value) = line.split_once('=').ok_or_else(|| {
            AuthFailure::invalid(
                "profile_file_malformed",
                format!("AWS profile file line {} is not key=value", line_index + 1),
            )
        })?;
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        non_empty(&key, "AWS profile key")
            .map_err(|error| AuthFailure::invalid("profile_file_malformed", error))?;
        non_empty(&value, "AWS profile value")
            .map_err(|error| AuthFailure::invalid("profile_file_malformed", error))?;
        let values = sections.get_mut(active).ok_or_else(|| {
            AuthFailure::invalid(
                "profile_file_malformed",
                "AWS profile parser lost its active section",
            )
        })?;
        if values.insert(key, value).is_some() {
            return Err(AuthFailure::invalid(
                "profile_file_malformed",
                "AWS profile file repeats a key in one section",
            ));
        }
    }
    Ok(sections)
}

fn sso_access_token_from_cache(path_env: &str) -> Result<String, AuthFailure> {
    let path = path_from_environment(path_env)?;
    let bytes = read_file_bounded(
        &path,
        1_048_576,
        "sso_cache_unreadable",
        "explicit SSO cache file cannot be read",
    )?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        AuthFailure::invalid(
            "sso_cache_malformed",
            "explicit SSO cache file is malformed",
        )
    })?;
    let token = json_string(&value, "accessToken", "SSO cache")?;
    let expires = parse_rfc3339(
        &json_string(&value, "expiresAt", "SSO cache")?,
        "SSO cache expiresAt",
    )?;
    if expires <= Utc::now() {
        return Err(AuthFailure::invalid(
            "sso_cache_expired",
            "explicit SSO cache token is expired",
        ));
    }
    Ok(token)
}

fn read_file_bounded(
    path: &Path,
    maximum: usize,
    code: &'static str,
    detail: &'static str,
) -> Result<Vec<u8>, AuthFailure> {
    let file = std::fs::File::open(path).map_err(|_| AuthFailure::invalid(code, detail))?;
    let length = file
        .metadata()
        .map_err(|_| AuthFailure::invalid(code, detail))?
        .len();
    if length > maximum as u64 {
        return Err(AuthFailure::invalid(
            "credential_file_too_large",
            "credential file exceeds its provider bound",
        ));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take((maximum as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| AuthFailure::invalid(code, detail))?;
    if bytes.len() > maximum {
        return Err(AuthFailure::invalid(
            "credential_file_too_large",
            "credential file exceeds its provider bound",
        ));
    }
    Ok(bytes)
}

fn secret_value(octopus_home: &Path, reference: &str) -> Result<String, AuthFailure> {
    let manager = secrets_manager(octopus_home);
    optional_secret_value(&manager, reference)?.ok_or_else(|| {
        AuthFailure::invalid(
            "secret_handle_unavailable",
            "secret handle has no stored value",
        )
    })
}

fn secrets_manager(octopus_home: &Path) -> SecretsManager {
    SecretsManager::new_with_keyring_store(
        octopus_home.to_path_buf(),
        SecretsBackendKind::Local,
        Arc::new(LocalFileKeyringStore::new(
            octopus_home.join("secrets").join("keyring"),
        )),
    )
}

fn optional_secret_value(
    manager: &SecretsManager,
    reference: &str,
) -> Result<Option<String>, AuthFailure> {
    let (scope, name) = parse_secret_ref(reference)
        .map_err(|error| AuthFailure::invalid("secret_ref_invalid", error))?;
    let value = manager.get(&scope, &name).map_err(|_| {
        AuthFailure::invalid(
            "secret_handle_unavailable",
            "secret handle cannot be resolved",
        )
    })?;
    value
        .map(|value| required_value(value, "secret handle value"))
        .transpose()
}

fn parse_secret_ref(reference: &str) -> Result<(SecretScope, SecretName), String> {
    let remainder = reference
        .strip_prefix("secret://")
        .ok_or_else(|| format!("secret reference '{reference}' must start with secret://"))?;
    let (scope, name) = remainder
        .split_once('/')
        .ok_or_else(|| format!("secret reference '{reference}' must be secret://<scope>/<name>"))?;
    if name.contains('/') {
        return Err(format!(
            "secret reference '{reference}' must contain one scope/name pair"
        ));
    }
    Ok((
        SecretScope::named(scope.to_string()).map_err(|error| error.to_string())?,
        SecretName::new(name).map_err(|error| error.to_string())?,
    ))
}

fn required_environment(name: &str, surface: &str) -> Result<String, AuthFailure> {
    let value = std::env::var(name).map_err(|_| {
        AuthFailure::unavailable(
            "token_unconfigured",
            format!("explicit Bedrock {surface} environment variable '{name}' is absent"),
        )
    })?;
    required_value(value, &format!("environment variable '{name}'"))
}

fn optional_environment(name: &str, surface: &str) -> Result<Option<String>, AuthFailure> {
    match std::env::var(name) {
        Ok(value) => {
            required_value(value, &format!("{surface} environment variable '{name}'")).map(Some)
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(AuthFailure::invalid(
            "environment_invalid",
            format!("{surface} environment variable '{name}' is not valid Unicode"),
        )),
    }
}

fn path_from_environment(name: &str) -> Result<PathBuf, AuthFailure> {
    let raw = required_environment(name, "path")?;
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(AuthFailure::invalid(
            "path_environment_invalid",
            format!("explicit AWS path environment variable '{name}' is not absolute"),
        ));
    }
    Ok(path)
}

fn optional_absolute_path_environment(name: &str) -> Result<Option<PathBuf>, AuthFailure> {
    let Some(raw) = optional_environment(name, "path")? else {
        return Ok(None);
    };
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(AuthFailure::invalid(
            "path_environment_invalid",
            format!("explicit AWS path environment variable '{name}' is not absolute"),
        ));
    }
    Ok(Some(path))
}

fn required_profile_value(
    values: &BTreeMap<String, String>,
    key: &str,
    profile: &str,
) -> Result<String, AuthFailure> {
    values
        .get(key)
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AuthFailure::invalid(
                "profile_value_missing",
                format!("AWS profile '{profile}' has no '{key}'"),
            )
        })
}

fn json_string(value: &Value, key: &str, surface: &str) -> Result<String, AuthFailure> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty() && value.trim() == value)
        .ok_or_else(|| {
            AuthFailure::invalid(
                "credential_value_missing",
                format!("{surface}.{key} is missing or malformed"),
            )
        })
}

fn required_value(value: String, surface: &str) -> Result<String, AuthFailure> {
    if value.is_empty() || value.trim() != value {
        return Err(AuthFailure::invalid(
            "credential_value_invalid",
            format!("{surface} must be non-empty and contain no surrounding whitespace"),
        ));
    }
    Ok(value)
}

fn parse_rfc3339(value: &str, surface: &str) -> Result<DateTime<Utc>, AuthFailure> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            AuthFailure::invalid(
                "credential_expiration_malformed",
                format!("{surface} is malformed: {error}"),
            )
        })
}

fn validate_region(region: &str) -> Result<(), String> {
    if region.is_empty()
        || region.trim() != region
        || !region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("Bedrock region must be a non-empty lowercase ASCII region id".to_string());
    }
    Ok(())
}

fn validate_signing_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.trim() != name
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("AWS signing_name must be non-empty lowercase ASCII".to_string());
    }
    Ok(())
}

fn validate_imds_endpoint(endpoint: &str) -> Result<Url, String> {
    let url = Url::parse(endpoint).map_err(|error| format!("IMDS endpoint is invalid: {error}"))?;
    if url.scheme() != "http"
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.port().is_some()
    {
        return Err("IMDS endpoint must be an exact default-port HTTP metadata base".to_string());
    }
    let allowed = match url.host_str().and_then(|host| host.parse::<IpAddr>().ok()) {
        Some(IpAddr::V4(address)) => address == std::net::Ipv4Addr::new(169, 254, 169, 254),
        Some(IpAddr::V6(address)) => {
            address == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254)
        }
        _ => false,
    };
    if !allowed {
        return Err("IMDS endpoint host must be 169.254.169.254 or fd00:ec2::254".to_string());
    }
    Ok(url)
}

fn validate_https_base_url(value: &str, surface: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|error| format!("{surface} is invalid: {error}"))?;
    if url.scheme() != "https"
        || url.host().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "{surface} must be an HTTPS base URL without credentials or query"
        ));
    }
    Ok(url)
}

fn non_empty(value: &str, surface: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!("{surface} must be non-empty and trimmed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn authorizer(source: CredentialSource) -> AwsAuthorizer {
        AwsAuthorizer {
            authorization: AuthorizationConfig::Sigv4 {
                credentials: Box::new(source),
            },
            region: "us-east-1".to_string(),
            signing_name: "bedrock".to_string(),
            octopus_home: PathBuf::new(),
            http: Client::builder().no_proxy().build().unwrap(),
            cached_credentials: Arc::new(Mutex::new(None)),
        }
    }

    fn sts() -> StsConfig {
        StsConfig {
            endpoint: "https://sts.us-east-1.amazonaws.com".to_string(),
            region: "us-east-1".to_string(),
            signing_name: "sts".to_string(),
            default_role_session_name: "octopus-bedrock".to_string(),
            timeout_ms: 1000,
            max_response_bytes: 262_144,
        }
    }

    #[test]
    fn sigv4_matches_the_aws_single_chunk_lifecycle_example() {
        let credentials = AwsCredentials {
            access_key: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: None,
            expires_at: None,
            credential_handle_fingerprint: None,
        };
        let url = Url::parse("https://examplebucket.s3.amazonaws.com/?lifecycle").unwrap();
        let headers = sign_headers(
            &Method::GET,
            &url,
            b"",
            HeaderMap::new(),
            &credentials,
            SigV4Scope {
                region: "us-east-1",
                signing_name: "s3",
                now: Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap(),
            },
        )
        .unwrap();
        let authorization = headers.get(AUTHORIZATION).unwrap().to_str().unwrap();
        assert!(authorization.ends_with(
            "Signature=fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
        ));
    }

    #[test]
    fn sigv4_canonical_uri_matches_bedrock_wire_paths() {
        for (wire, expected) in [
            (
                "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-3-haiku-20240307-v1%3A0/converse",
                "/model/anthropic.claude-3-haiku-20240307-v1%253A0/converse",
            ),
            (
                "https://bedrock-runtime.us-east-1.amazonaws.com/model/publisher%2Fmodel-v1%3A0/converse-stream",
                "/model/publisher%252Fmodel-v1%253A0/converse-stream",
            ),
            (
                "https://bedrock-runtime.us-east-1.amazonaws.com/model/arn%3Aaws%3Abedrock%3Aus-east-1%3A111122223333%3Ainference-profile%2Fus.anthropic.claude-3-haiku-20240307-v1%3A0/converse",
                "/model/arn%253Aaws%253Abedrock%253Aus-east-1%253A111122223333%253Ainference-profile%252Fus.anthropic.claude-3-haiku-20240307-v1%253A0/converse",
            ),
        ] {
            assert_eq!(canonical_uri(&Url::parse(wire).unwrap()), expected);
        }
    }

    #[test]
    fn selected_auth_source_must_exactly_own_the_process_environment() {
        let auth = AuthorizationConfig::Sigv4 {
            credentials: Box::new(CredentialSource::Environment {
                access_key_env: "AWS_ACCESS_KEY_ID".to_string(),
                secret_key_env: "AWS_SECRET_ACCESS_KEY".to_string(),
                session_token_env: Some("AWS_SESSION_TOKEN".to_string()),
            }),
        };
        assert!(validate_authorization(
            &auth,
            &[
                "AWS_ACCESS_KEY_ID".to_string(),
                "AWS_SECRET_ACCESS_KEY".to_string(),
                "AWS_SESSION_TOKEN".to_string(),
            ],
            "world_provider_bedrock",
        )
        .is_ok());
        assert!(validate_authorization(
            &auth,
            &["AWS_PROFILE".to_string()],
            "world_provider_bedrock",
        )
        .is_err());
    }

    #[tokio::test]
    async fn explicit_chain_uses_the_first_available_source() {
        let names = [
            "OCTOPUS_BEDROCK_TEST_CHAIN_AK1",
            "OCTOPUS_BEDROCK_TEST_CHAIN_SK1",
            "OCTOPUS_BEDROCK_TEST_CHAIN_AK2",
            "OCTOPUS_BEDROCK_TEST_CHAIN_SK2",
        ];
        for name in names {
            std::env::remove_var(name);
        }
        std::env::set_var(names[0], "FIRST_ACCESS");
        std::env::set_var(names[1], "FIRST_SECRET");
        std::env::set_var(names[2], "SECOND_ACCESS");
        std::env::set_var(names[3], "SECOND_SECRET");
        let auth = authorizer(CredentialSource::Chain {
            sources: vec![
                CredentialSource::Environment {
                    access_key_env: names[0].to_string(),
                    secret_key_env: names[1].to_string(),
                    session_token_env: None,
                },
                CredentialSource::Environment {
                    access_key_env: names[2].to_string(),
                    secret_key_env: names[3].to_string(),
                    session_token_env: None,
                },
            ],
        });
        let AuthorizationConfig::Sigv4 {
            credentials: source,
        } = &auth.authorization
        else {
            unreachable!();
        };
        let credentials = auth.resolve_credentials(source).await.unwrap();
        assert_eq!(credentials.access_key, "FIRST_ACCESS");
        for name in names {
            std::env::remove_var(name);
        }
    }

    #[tokio::test]
    async fn explicit_chain_returns_redacted_aggregate_failure() {
        let names = [
            "OCTOPUS_BEDROCK_TEST_MISSING_AK1",
            "OCTOPUS_BEDROCK_TEST_MISSING_SK1",
            "OCTOPUS_BEDROCK_TEST_MISSING_AK2",
            "OCTOPUS_BEDROCK_TEST_MISSING_SK2",
        ];
        for name in names {
            std::env::remove_var(name);
        }
        let auth = authorizer(CredentialSource::Chain {
            sources: vec![
                CredentialSource::Environment {
                    access_key_env: names[0].to_string(),
                    secret_key_env: names[1].to_string(),
                    session_token_env: None,
                },
                CredentialSource::Environment {
                    access_key_env: names[2].to_string(),
                    secret_key_env: names[3].to_string(),
                    session_token_env: None,
                },
            ],
        });
        let AuthorizationConfig::Sigv4 {
            credentials: source,
        } = &auth.authorization
        else {
            unreachable!();
        };
        let failure = match auth.resolve_credentials(source).await {
            Ok(_) => panic!("missing sources must not produce credentials"),
            Err(failure) => failure,
        };
        assert_eq!(failure.evidence["kind"], "aggregate");
        assert_eq!(failure.evidence["sources"].as_array().unwrap().len(), 2);
        let rendered = failure.evidence.to_string();
        assert!(!rendered.contains(names[0]));
        assert!(!rendered.contains("FIRST_SECRET"));
    }

    #[tokio::test]
    async fn explicit_chain_does_not_hide_partial_primary_credentials() {
        let names = [
            "OCTOPUS_BEDROCK_TEST_PARTIAL_AK",
            "OCTOPUS_BEDROCK_TEST_PARTIAL_SK",
            "OCTOPUS_BEDROCK_TEST_PARTIAL_TOKEN",
            "OCTOPUS_BEDROCK_TEST_FALLBACK_AK",
            "OCTOPUS_BEDROCK_TEST_FALLBACK_SK",
        ];
        for name in names {
            std::env::remove_var(name);
        }
        std::env::set_var(names[2], "PARTIAL_TOKEN");
        std::env::set_var(names[3], "FALLBACK_ACCESS");
        std::env::set_var(names[4], "FALLBACK_SECRET");
        let auth = authorizer(CredentialSource::Chain {
            sources: vec![
                CredentialSource::Environment {
                    access_key_env: names[0].to_string(),
                    secret_key_env: names[1].to_string(),
                    session_token_env: Some(names[2].to_string()),
                },
                CredentialSource::Environment {
                    access_key_env: names[3].to_string(),
                    secret_key_env: names[4].to_string(),
                    session_token_env: None,
                },
            ],
        });
        let AuthorizationConfig::Sigv4 {
            credentials: source,
        } = &auth.authorization
        else {
            unreachable!();
        };
        let failure = match auth.resolve_credentials(source).await {
            Ok(_) => panic!("partial primary credentials must stop the chain"),
            Err(failure) => failure,
        };
        assert_eq!(failure.evidence["code"], "environment_partial");
        assert_eq!(failure.evidence["source"]["index"], 0);
        assert_eq!(failure.evidence["source"]["source"], "environment");
        assert_eq!(failure.evidence["source"]["code"], "environment_partial");
        for name in names {
            std::env::remove_var(name);
        }
    }

    #[tokio::test]
    async fn source_profile_cycles_fail_before_assume_role_transport() {
        let directory = tempfile::tempdir().unwrap();
        let credentials_path = directory.path().join("credentials");
        let config_path = directory.path().join("config");
        std::fs::write(
            &credentials_path,
            "[unused]\naws_access_key_id=x\naws_secret_access_key=y\n",
        )
        .unwrap();
        std::fs::write(
            &config_path,
            "[profile role-a]\nrole_arn=arn:aws:iam::123456789012:role/a\nsource_profile=role-b\n[profile role-b]\nrole_arn=arn:aws:iam::123456789012:role/b\nsource_profile=role-a\n",
        )
        .unwrap();
        let credentials_env = "OCTOPUS_BEDROCK_TEST_CREDENTIALS_FILE";
        let config_env = "OCTOPUS_BEDROCK_TEST_CONFIG_FILE";
        std::env::set_var(credentials_env, &credentials_path);
        std::env::set_var(config_env, &config_path);
        let auth = authorizer(CredentialSource::Profile {
            profile: "role-a".to_string(),
            credentials_file_env: credentials_env.to_string(),
            config_file_env: config_env.to_string(),
            max_role_depth: 8,
            sts: sts(),
            credential_process: None,
            sso_cache_file_env: None,
            sso_portal_domain: None,
        });
        let AuthorizationConfig::Sigv4 {
            credentials: source,
        } = &auth.authorization
        else {
            unreachable!();
        };
        let failure = match auth.resolve_credentials(source).await {
            Ok(_) => panic!("cyclic source profiles must not produce credentials"),
            Err(failure) => failure,
        };
        assert_eq!(failure.evidence["code"], "profile_cycle");
        std::env::remove_var(credentials_env);
        std::env::remove_var(config_env);
    }

    #[tokio::test]
    async fn shared_credentials_profile_resolves_without_ambient_paths() {
        let directory = tempfile::tempdir().unwrap();
        let credentials_path = directory.path().join("credentials");
        let config_path = directory.path().join("config");
        std::fs::write(
            &credentials_path,
            "[source]\naws_access_key_id=PROFILE_ACCESS\naws_secret_access_key=PROFILE_SECRET\naws_session_token=PROFILE_TOKEN\n",
        )
        .unwrap();
        std::fs::write(&config_path, "[profile source]\nregion=us-east-1\n").unwrap();
        let credentials_env = "OCTOPUS_BEDROCK_TEST_STATIC_CREDENTIALS_FILE";
        let config_env = "OCTOPUS_BEDROCK_TEST_STATIC_CONFIG_FILE";
        std::env::set_var(credentials_env, &credentials_path);
        std::env::set_var(config_env, &config_path);
        let auth = authorizer(CredentialSource::Profile {
            profile: "source".to_string(),
            credentials_file_env: credentials_env.to_string(),
            config_file_env: config_env.to_string(),
            max_role_depth: 8,
            sts: sts(),
            credential_process: None,
            sso_cache_file_env: None,
            sso_portal_domain: None,
        });
        let AuthorizationConfig::Sigv4 {
            credentials: source,
        } = &auth.authorization
        else {
            unreachable!();
        };
        let credentials = auth.resolve_credentials(source).await.unwrap();
        assert_eq!(credentials.access_key, "PROFILE_ACCESS");
        assert_eq!(credentials.session_token.as_deref(), Some("PROFILE_TOKEN"));
        std::env::remove_var(credentials_env);
        std::env::remove_var(config_env);
    }

    #[test]
    fn assume_role_and_web_identity_requests_are_wire_native() {
        let role = "arn:aws:iam::123456789012:role/bedrock";
        let assume = assume_role_body(role, "octopus-role", Some("external"), Some(3600)).unwrap();
        assert!(assume.contains("Action=AssumeRole"));
        assert!(assume.contains("RoleArn=arn%3Aaws%3Aiam%3A%3A123456789012%3Arole%2Fbedrock"));
        assert!(assume.contains("ExternalId=external"));
        let web =
            web_identity_body(role, "octopus-web", "header.payload.signature", Some(3600)).unwrap();
        assert!(web.contains("Action=AssumeRoleWithWebIdentity"));
        assert!(web.contains("WebIdentityToken=header.payload.signature"));
        let response = br#"<AssumeRoleResponse><AssumeRoleResult><Credentials><AccessKeyId>AKIA</AccessKeyId><SecretAccessKey>secret</SecretAccessKey><SessionToken>token</SessionToken><Expiration>2099-01-01T00:00:00Z</Expiration></Credentials></AssumeRoleResult></AssumeRoleResponse>"#;
        let credentials = parse_sts_credentials(StatusCode::OK, response, "assume_role").unwrap();
        assert_eq!(credentials.access_key, "AKIA");
    }

    #[test]
    fn ecs_and_imds_metadata_hosts_are_bounded() {
        let allowed = vec!["127.0.0.1".to_string(), "169.254.170.2".to_string()];
        assert!(validate_ecs_full_url("http://127.0.0.1:8123/credentials", &allowed).is_ok());
        assert!(validate_ecs_full_url("http://10.0.0.1/credentials", &allowed).is_err());
        assert!(validate_imds_endpoint("http://169.254.169.254").is_ok());
        assert!(validate_imds_endpoint("http://169.254.169.254:8080").is_err());
        assert!(validate_imds_endpoint("http://127.0.0.1").is_err());
        let ecs = parse_json_credentials(
            StatusCode::OK,
            br#"{"AccessKeyId":"ECS_ACCESS","SecretAccessKey":"ECS_SECRET","Token":"ECS_TOKEN","Expiration":"2099-01-01T00:00:00Z"}"#,
            "ecs_container",
        )
        .unwrap();
        assert_eq!(ecs.access_key, "ECS_ACCESS");
        let imds = parse_imds_credentials(
            br#"{"Code":"Success","AccessKeyId":"IMDS_ACCESS","SecretAccessKey":"IMDS_SECRET","Token":"IMDS_TOKEN","Expiration":"2099-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(imds.access_key, "IMDS_ACCESS");
    }

    #[tokio::test]
    async fn credential_process_output_is_strict_and_bounded() {
        let parsed = parse_process_credentials(
            br#"{"Version":1,"AccessKeyId":"AKIA","SecretAccessKey":"secret","SessionToken":"token","Expiration":"2099-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(parsed.access_key, "AKIA");
        let failure = match credentials_from_process(
            Path::new("/bin/sh"),
            &["-c".to_string(), "printf 123456789".to_string()],
            &[],
            1000,
            8,
            8,
        )
        .await
        {
            Ok(_) => panic!("oversized process output must not produce credentials"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.evidence["code"],
            "credential_process_output_too_large"
        );
    }

    #[test]
    fn exact_interface_secret_scope_is_required() {
        assert!(validate_secret_ref_scope(
            "secret://world_provider_bedrock/AWS_ACCESS_KEY_ID",
            "world_provider_bedrock"
        )
        .is_ok());
        assert!(validate_secret_ref_scope(
            "secret://world_provider_other/AWS_ACCESS_KEY_ID",
            "world_provider_bedrock"
        )
        .is_err());
    }

    #[tokio::test]
    async fn exact_bearer_secret_handle_has_only_a_canonical_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        let secret_ref = "secret://world_provider_bedrock/BEDROCK_BEARER_TOKEN";
        let authorizer = AwsAuthorizer::new(
            AuthorizationConfig::Bearer {
                token: TokenSource::SecretHandle {
                    secret_ref: secret_ref.to_string(),
                },
            },
            "world_provider_bedrock",
            "us-east-1".to_string(),
            "bedrock".to_string(),
            directory.path().to_path_buf(),
            Client::builder().no_proxy().build().unwrap(),
            &[],
        )
        .unwrap();
        let fingerprint = authorizer.credential_handle_fingerprint().await.unwrap();
        assert_eq!(fingerprint, secret_handle_fingerprint(&[secret_ref]));
        assert_eq!(fingerprint.len(), 64);
        assert!(!fingerprint.contains("secret://"));
    }

    #[tokio::test]
    async fn dynamic_auth_sources_have_stable_source_specific_fingerprints() {
        let first = authorizer(CredentialSource::Environment {
            access_key_env: "OCTOPUS_BEDROCK_AK_ONE".to_string(),
            secret_key_env: "OCTOPUS_BEDROCK_SK_ONE".to_string(),
            session_token_env: None,
        });
        let same = authorizer(CredentialSource::Environment {
            access_key_env: "OCTOPUS_BEDROCK_AK_ONE".to_string(),
            secret_key_env: "OCTOPUS_BEDROCK_SK_ONE".to_string(),
            session_token_env: None,
        });
        let different = authorizer(CredentialSource::Environment {
            access_key_env: "OCTOPUS_BEDROCK_AK_TWO".to_string(),
            secret_key_env: "OCTOPUS_BEDROCK_SK_TWO".to_string(),
            session_token_env: None,
        });

        let first = first.credential_handle_fingerprint().await.unwrap();
        assert_eq!(first, same.credential_handle_fingerprint().await.unwrap());
        assert_ne!(
            first,
            different.credential_handle_fingerprint().await.unwrap()
        );
        assert_eq!(first.len(), 64);

        let bearer = AwsAuthorizer::new(
            AuthorizationConfig::Bearer {
                token: TokenSource::Environment {
                    env: "OCTOPUS_BEDROCK_BEARER".to_string(),
                },
            },
            "world_provider_bedrock",
            "us-east-1".to_string(),
            "bedrock".to_string(),
            PathBuf::new(),
            Client::builder().no_proxy().build().unwrap(),
            &["OCTOPUS_BEDROCK_BEARER".to_string()],
        )
        .unwrap()
        .credential_handle_fingerprint()
        .await
        .unwrap();
        assert_eq!(bearer.len(), 64);
        assert_ne!(first, bearer);
    }

    #[test]
    fn exact_secret_handles_distinguish_absent_partial_and_complete_sets() {
        let directory = tempfile::tempdir().unwrap();
        let access_ref = "secret://world_provider_bedrock/AWS_ACCESS_KEY_ID";
        let secret_ref = "secret://world_provider_bedrock/AWS_SECRET_ACCESS_KEY";
        let missing = match credentials_from_secrets(directory.path(), access_ref, secret_ref, None)
        {
            Ok(_) => panic!("absent secret handles must not produce credentials"),
            Err(failure) => failure,
        };
        assert_eq!(missing.evidence["kind"], "unavailable");
        let manager = SecretsManager::new_with_keyring_store(
            directory.path().to_path_buf(),
            SecretsBackendKind::Local,
            Arc::new(LocalFileKeyringStore::new(
                directory.path().join("secrets").join("keyring"),
            )),
        );
        let scope = SecretScope::named("world_provider_bedrock").unwrap();
        manager
            .set(
                &scope,
                &SecretName::new("AWS_ACCESS_KEY_ID").unwrap(),
                "AKIA",
            )
            .unwrap();
        let partial = match credentials_from_secrets(directory.path(), access_ref, secret_ref, None)
        {
            Ok(_) => panic!("partial secret handles must not produce credentials"),
            Err(failure) => failure,
        };
        assert_eq!(partial.evidence["code"], "secret_handles_partial");
        manager
            .set(
                &scope,
                &SecretName::new("AWS_SECRET_ACCESS_KEY").unwrap(),
                "secret",
            )
            .unwrap();
        let credentials =
            credentials_from_secrets(directory.path(), access_ref, secret_ref, None).unwrap();
        assert_eq!(credentials.access_key, "AKIA");
        let fingerprint = credentials.credential_handle_fingerprint.unwrap();
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        assert_eq!(
            fingerprint,
            secret_handle_fingerprint(&[access_ref, secret_ref])
        );
    }

    #[tokio::test]
    async fn refreshable_cache_expires_and_invalidates() {
        let auth = authorizer(CredentialSource::Environment {
            access_key_env: "OCTOPUS_BEDROCK_UNUSED_AK".to_string(),
            secret_key_env: "OCTOPUS_BEDROCK_UNUSED_SK".to_string(),
            session_token_env: None,
        });
        *auth.cached_credentials.lock().await = Some(AwsCredentials {
            access_key: "AKIA".to_string(),
            secret_key: "secret".to_string(),
            session_token: Some("token".to_string()),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            credential_handle_fingerprint: None,
        });
        assert!(auth.cached_credentials().await.is_some());
        auth.invalidate_cached_credentials().await;
        assert!(auth.cached_credentials().await.is_none());
        *auth.cached_credentials.lock().await = Some(AwsCredentials {
            access_key: "AKIA".to_string(),
            secret_key: "secret".to_string(),
            session_token: Some("token".to_string()),
            expires_at: Some(Utc::now() + chrono::Duration::minutes(1)),
            credential_handle_fingerprint: None,
        });
        assert!(auth.cached_credentials().await.is_none());
    }

    #[tokio::test]
    async fn static_credentials_are_cached_until_explicit_invalidation() {
        let access_env = "OCTOPUS_BEDROCK_TEST_CACHE_AK";
        let secret_env = "OCTOPUS_BEDROCK_TEST_CACHE_SK";
        std::env::set_var(access_env, "CACHED_ACCESS");
        std::env::set_var(secret_env, "CACHED_SECRET");
        let auth = authorizer(CredentialSource::Environment {
            access_key_env: access_env.to_string(),
            secret_key_env: secret_env.to_string(),
            session_token_env: None,
        });
        let AuthorizationConfig::Sigv4 {
            credentials: source,
        } = &auth.authorization
        else {
            unreachable!();
        };
        assert_eq!(
            auth.resolve_credentials(source).await.unwrap().access_key,
            "CACHED_ACCESS"
        );
        std::env::remove_var(access_env);
        std::env::remove_var(secret_env);
        assert!(auth.resolve_credentials(source).await.is_ok());
        auth.invalidate_cached_credentials().await;
        assert!(auth.resolve_credentials(source).await.is_err());
    }
}
