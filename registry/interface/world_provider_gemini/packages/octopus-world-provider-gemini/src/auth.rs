use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use octopus_descriptors::InterfaceId;
use octopus_interfaces::InterfaceTransportConfig;
use octopus_secrets::{
    LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind, SecretsManager,
};
use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::driver::GeminiFailure;

const MAX_AUTH_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_ACCESS_TOKEN_BYTES: usize = 64 * 1024;
const MAX_SECRET_BYTES: usize = 1024 * 1024;
const MAX_TOKEN_LIFETIME_SECONDS: u64 = 24 * 60 * 60;
const JWT_BEARER_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const TOKEN_EXCHANGE_GRANT: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuthSourceSelection {
    Secret,
    Environment,
    GoogleServiceAccountJwt,
    GoogleMetadataAccessToken,
    GoogleWorkloadIdentityFederation,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceAccountJwtConfig {
    pub credential_secret_ref: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub assertion_lifetime_seconds: u64,
    pub refresh_skew_seconds: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MetadataAccessTokenConfig {
    pub token_url: String,
    pub scopes: Vec<String>,
    pub refresh_skew_seconds: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkloadIdentityFederationConfig {
    pub subject_token_secret_ref: String,
    pub token_url: String,
    pub audience: String,
    pub scopes: Vec<String>,
    pub subject_token_type: String,
    pub refresh_skew_seconds: u64,
}

#[derive(Clone)]
struct SecretLocator {
    octopus_home: PathBuf,
    scope: SecretScope,
    name: SecretName,
}

enum GeminiAuthSource {
    Environment(String),
    Secret(SecretLocator),
    Dynamic(Box<DynamicTokenSource>),
}

struct AuthRequestContext<'a> {
    http: &'a reqwest::Client,
    transport_retryable: bool,
    retryable_http_statuses: &'a BTreeSet<u16>,
}

enum DynamicTokenSource {
    ServiceAccount {
        credential: SecretLocator,
        token_url: reqwest::Url,
        scopes: String,
        assertion_lifetime_seconds: u64,
        refresh_skew_seconds: u64,
        cache: Mutex<Option<CachedAccessToken>>,
    },
    Metadata {
        token_url: reqwest::Url,
        scopes: String,
        refresh_skew_seconds: u64,
        cache: Mutex<Option<CachedAccessToken>>,
    },
    WorkloadIdentity {
        subject_token: SecretLocator,
        token_url: reqwest::Url,
        audience: String,
        scopes: String,
        subject_token_type: String,
        refresh_skew_seconds: u64,
        cache: Mutex<Option<CachedAccessToken>>,
    },
}

struct CachedAccessToken {
    value: String,
    expires_at_unix_seconds: u64,
}

struct FetchedAccessToken {
    value: String,
    expires_in_seconds: u64,
}

pub(crate) struct GeminiAuth {
    pub(crate) header: HeaderName,
    value_prefix: String,
    source: GeminiAuthSource,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceAccountCredential {
    #[serde(rename = "type")]
    credential_type: String,
    project_id: Option<String>,
    private_key_id: String,
    private_key: String,
    client_email: String,
    client_id: Option<String>,
    auth_uri: Option<String>,
    token_uri: String,
    auth_provider_x509_cert_url: Option<String>,
    client_x509_cert_url: Option<String>,
    universe_domain: Option<String>,
}

#[derive(Serialize)]
struct ServiceAccountClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthTokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StsTokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
    issued_token_type: String,
    scope: Option<String>,
}

impl GeminiAuth {
    pub(crate) fn from_descriptor(
        transport: &InterfaceTransportConfig,
        selection: AuthSourceSelection,
        activation_secrets_home: PathBuf,
        provider_interface: &InterfaceId,
        service_account: Option<ServiceAccountJwtConfig>,
        metadata: Option<MetadataAccessTokenConfig>,
        workload_identity: Option<WorkloadIdentityFederationConfig>,
    ) -> Result<Self, String> {
        let header = transport
            .auth_header
            .as_deref()
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| "Gemini transport.auth_header is required".to_string())?
            .parse::<HeaderName>()
            .map_err(|error| format!("Gemini auth_header is invalid: {error}"))?;
        let value_prefix = transport
            .auth_value_prefix
            .clone()
            .ok_or_else(|| "Gemini transport.auth_value_prefix is required".to_string())?;
        let source = match selection {
            AuthSourceSelection::Environment => {
                reject_dynamic_configs(&service_account, &metadata, &workload_identity)?;
                if transport.auth_secret_ref.is_some() {
                    return Err(
                        "Gemini environment auth source cannot retain transport.auth_secret_ref"
                            .to_string(),
                    );
                }
                let name = required_trimmed(
                    transport.auth_env_key.as_deref(),
                    "Gemini environment auth source requires transport.auth_env_key",
                )?;
                GeminiAuthSource::Environment(name.to_string())
            }
            AuthSourceSelection::Secret => {
                reject_dynamic_configs(&service_account, &metadata, &workload_identity)?;
                if transport.auth_env_key.is_some() {
                    return Err(
                        "Gemini secret auth source cannot retain transport.auth_env_key"
                            .to_string(),
                    );
                }
                let reference = required_trimmed(
                    transport.auth_secret_ref.as_deref(),
                    "Gemini secret auth source requires transport.auth_secret_ref",
                )?;
                GeminiAuthSource::Secret(secret_locator(
                    reference,
                    activation_secrets_home,
                    provider_interface,
                    "auth_secret_ref",
                )?)
            }
            AuthSourceSelection::GoogleServiceAccountJwt => {
                reject_transport_secret_sources(transport)?;
                require_bearer_header(&header, &value_prefix)?;
                if metadata.is_some() || workload_identity.is_some() {
                    return Err(
                        "Gemini service-account auth cannot retain another dynamic auth config"
                            .to_string(),
                    );
                }
                let config = service_account.ok_or_else(|| {
                    "Gemini google_service_account_jwt auth requires service_account_jwt config"
                        .to_string()
                })?;
                validate_cache_skew(config.refresh_skew_seconds)?;
                if config.assertion_lifetime_seconds == 0
                    || config.assertion_lifetime_seconds > 3600
                {
                    return Err(
                        "Gemini service_account_jwt.assertion_lifetime_seconds must be within 1..=3600"
                            .to_string(),
                    );
                }
                let token_url = validate_oauth_token_url(&config.token_url)?;
                let scopes = validate_scopes(&config.scopes)?;
                let credential = secret_locator(
                    &config.credential_secret_ref,
                    activation_secrets_home,
                    provider_interface,
                    "service_account_jwt.credential_secret_ref",
                )?;
                GeminiAuthSource::Dynamic(Box::new(DynamicTokenSource::ServiceAccount {
                    credential,
                    token_url,
                    scopes,
                    assertion_lifetime_seconds: config.assertion_lifetime_seconds,
                    refresh_skew_seconds: config.refresh_skew_seconds,
                    cache: Mutex::new(None),
                }))
            }
            AuthSourceSelection::GoogleMetadataAccessToken => {
                reject_transport_secret_sources(transport)?;
                require_bearer_header(&header, &value_prefix)?;
                if service_account.is_some() || workload_identity.is_some() {
                    return Err(
                        "Gemini metadata auth cannot retain another dynamic auth config"
                            .to_string(),
                    );
                }
                let config = metadata.ok_or_else(|| {
                    "Gemini google_metadata_access_token auth requires metadata_access_token config"
                        .to_string()
                })?;
                validate_cache_skew(config.refresh_skew_seconds)?;
                let token_url = validate_metadata_token_url(&config.token_url)?;
                let scopes = validate_scopes(&config.scopes)?;
                GeminiAuthSource::Dynamic(Box::new(DynamicTokenSource::Metadata {
                    token_url,
                    scopes,
                    refresh_skew_seconds: config.refresh_skew_seconds,
                    cache: Mutex::new(None),
                }))
            }
            AuthSourceSelection::GoogleWorkloadIdentityFederation => {
                reject_transport_secret_sources(transport)?;
                require_bearer_header(&header, &value_prefix)?;
                if service_account.is_some() || metadata.is_some() {
                    return Err(
                        "Gemini workload-identity auth cannot retain another dynamic auth config"
                            .to_string(),
                    );
                }
                let config = workload_identity.ok_or_else(|| {
                    "Gemini google_workload_identity_federation auth requires workload_identity_federation config"
                        .to_string()
                })?;
                validate_cache_skew(config.refresh_skew_seconds)?;
                let token_url = validate_sts_token_url(&config.token_url)?;
                validate_workload_audience(&config.audience)?;
                validate_subject_token_type(&config.subject_token_type)?;
                let scopes = validate_scopes(&config.scopes)?;
                let subject_token = secret_locator(
                    &config.subject_token_secret_ref,
                    activation_secrets_home,
                    provider_interface,
                    "workload_identity_federation.subject_token_secret_ref",
                )?;
                GeminiAuthSource::Dynamic(Box::new(DynamicTokenSource::WorkloadIdentity {
                    subject_token,
                    token_url,
                    audience: config.audience,
                    scopes,
                    subject_token_type: config.subject_token_type,
                    refresh_skew_seconds: config.refresh_skew_seconds,
                    cache: Mutex::new(None),
                }))
            }
        };
        Ok(Self {
            header,
            value_prefix,
            source,
        })
    }

    pub(crate) fn credential_handle_fingerprint(&self) -> Option<String> {
        Some(credential_source_fingerprint(&self.source))
    }

    pub(crate) async fn header_value(
        &self,
        http: &reqwest::Client,
        transport_retryable: bool,
        retryable_http_statuses: &BTreeSet<u16>,
    ) -> Result<HeaderValue, GeminiFailure> {
        let secret = match &self.source {
            GeminiAuthSource::Environment(name) => std::env::var(name)
                .ok()
                .filter(|value| !value.is_empty() && value.trim() == value)
                .ok_or_else(|| {
                    GeminiFailure::contract(format!(
                        "Gemini auth env var '{name}' is unset, empty, or padded"
                    ))
                })?,
            GeminiAuthSource::Secret(locator) => load_secret(locator, "provider credential")?,
            GeminiAuthSource::Dynamic(source) => {
                source
                    .access_token(&AuthRequestContext {
                        http,
                        transport_retryable,
                        retryable_http_statuses,
                    })
                    .await?
            }
        };
        HeaderValue::from_str(&format!("{}{secret}", self.value_prefix)).map_err(|_| {
            GeminiFailure::contract("Gemini credential cannot form a valid authentication header")
        })
    }

    pub(crate) async fn invalidate_dynamic_token(&self) {
        if let GeminiAuthSource::Dynamic(source) = &self.source {
            source.invalidate().await;
        }
    }

    pub(crate) fn is_dynamic(&self) -> bool {
        matches!(self.source, GeminiAuthSource::Dynamic(_))
    }
}

fn credential_source_fingerprint(source: &GeminiAuthSource) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"octopus.provider.gemini.credential_source.v1");
    match source {
        GeminiAuthSource::Environment(name) => {
            hash_fingerprint_part(&mut hasher, b"environment");
            hash_fingerprint_part(&mut hasher, name.as_bytes());
        }
        GeminiAuthSource::Secret(locator) => {
            hash_fingerprint_part(&mut hasher, b"secret");
            hash_secret_locator(&mut hasher, locator);
        }
        GeminiAuthSource::Dynamic(source) => match source.as_ref() {
            DynamicTokenSource::ServiceAccount {
                credential,
                token_url,
                scopes,
                assertion_lifetime_seconds,
                refresh_skew_seconds,
                ..
            } => {
                hash_fingerprint_part(&mut hasher, b"service_account_jwt");
                hash_secret_locator(&mut hasher, credential);
                hash_url_coordinate(&mut hasher, token_url);
                hash_fingerprint_part(&mut hasher, scopes.as_bytes());
                hash_fingerprint_part(&mut hasher, &assertion_lifetime_seconds.to_le_bytes());
                hash_fingerprint_part(&mut hasher, &refresh_skew_seconds.to_le_bytes());
            }
            DynamicTokenSource::Metadata {
                token_url,
                scopes,
                refresh_skew_seconds,
                ..
            } => {
                hash_fingerprint_part(&mut hasher, b"metadata_access_token");
                hash_url_coordinate(&mut hasher, token_url);
                hash_fingerprint_part(&mut hasher, scopes.as_bytes());
                hash_fingerprint_part(&mut hasher, &refresh_skew_seconds.to_le_bytes());
            }
            DynamicTokenSource::WorkloadIdentity {
                subject_token,
                token_url,
                audience,
                scopes,
                subject_token_type,
                refresh_skew_seconds,
                ..
            } => {
                hash_fingerprint_part(&mut hasher, b"workload_identity_federation");
                hash_secret_locator(&mut hasher, subject_token);
                hash_url_coordinate(&mut hasher, token_url);
                hash_fingerprint_part(&mut hasher, audience.as_bytes());
                hash_fingerprint_part(&mut hasher, scopes.as_bytes());
                hash_fingerprint_part(&mut hasher, subject_token_type.as_bytes());
                hash_fingerprint_part(&mut hasher, &refresh_skew_seconds.to_le_bytes());
            }
        },
    }
    hasher.finalize().to_hex().to_string()
}

fn hash_secret_locator(hasher: &mut blake3::Hasher, locator: &SecretLocator) {
    hash_fingerprint_part(hasher, locator.scope.name().unwrap_or("global").as_bytes());
    let name = locator.name.to_string();
    hash_fingerprint_part(hasher, name.as_bytes());
}

fn hash_url_coordinate(hasher: &mut blake3::Hasher, url: &reqwest::Url) {
    hash_fingerprint_part(hasher, url.scheme().as_bytes());
    hash_fingerprint_part(hasher, url.host_str().unwrap_or_default().as_bytes());
    hash_fingerprint_part(hasher, &url.port().unwrap_or_default().to_le_bytes());
    hash_fingerprint_part(hasher, url.path().as_bytes());
}

fn hash_fingerprint_part(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

impl DynamicTokenSource {
    async fn access_token(
        &self,
        context: &AuthRequestContext<'_>,
    ) -> Result<String, GeminiFailure> {
        let (cache, refresh_skew_seconds) = match self {
            Self::ServiceAccount {
                cache,
                refresh_skew_seconds,
                ..
            }
            | Self::Metadata {
                cache,
                refresh_skew_seconds,
                ..
            }
            | Self::WorkloadIdentity {
                cache,
                refresh_skew_seconds,
                ..
            } => (cache, *refresh_skew_seconds),
        };
        let mut cache = cache.lock().await;
        let now = unix_now()?;
        if let Some(cached) = cache.as_ref() {
            if now.saturating_add(refresh_skew_seconds) < cached.expires_at_unix_seconds {
                return Ok(cached.value.clone());
            }
        }
        let fetched = match self {
            Self::ServiceAccount {
                credential,
                token_url,
                scopes,
                assertion_lifetime_seconds,
                ..
            } => {
                fetch_service_account_token(
                    context,
                    credential,
                    token_url,
                    scopes,
                    *assertion_lifetime_seconds,
                    now,
                )
                .await?
            }
            Self::Metadata {
                token_url, scopes, ..
            } => fetch_metadata_token(context, token_url, scopes).await?,
            Self::WorkloadIdentity {
                subject_token,
                token_url,
                audience,
                scopes,
                subject_token_type,
                ..
            } => {
                fetch_workload_identity_token(
                    context,
                    subject_token,
                    token_url,
                    audience,
                    scopes,
                    subject_token_type,
                )
                .await?
            }
        };
        let expires_at_unix_seconds = now
            .checked_add(fetched.expires_in_seconds)
            .ok_or_else(|| GeminiFailure::contract("Gemini access-token expiry overflowed"))?;
        let value = fetched.value;
        *cache = Some(CachedAccessToken {
            value: value.clone(),
            expires_at_unix_seconds,
        });
        Ok(value)
    }

    async fn invalidate(&self) {
        let cache = match self {
            Self::ServiceAccount { cache, .. }
            | Self::Metadata { cache, .. }
            | Self::WorkloadIdentity { cache, .. } => cache,
        };
        *cache.lock().await = None;
    }
}

async fn fetch_service_account_token(
    context: &AuthRequestContext<'_>,
    credential: &SecretLocator,
    token_url: &reqwest::Url,
    scopes: &str,
    assertion_lifetime_seconds: u64,
    now: u64,
) -> Result<FetchedAccessToken, GeminiFailure> {
    let raw = load_secret(credential, "service-account credential")?;
    if raw.len() > MAX_SECRET_BYTES {
        return Err(GeminiFailure::contract(
            "Gemini service-account credential exceeds the bounded secret size",
        ));
    }
    let credential: ServiceAccountCredential = serde_json::from_str(&raw).map_err(|_| {
        GeminiFailure::contract("Gemini service-account credential is not strict JSON")
    })?;
    validate_service_account_credential(&credential, token_url)?;
    let exp = now
        .checked_add(assertion_lifetime_seconds)
        .ok_or_else(|| GeminiFailure::contract("Gemini JWT assertion expiry overflowed"))?;
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(credential.private_key_id.clone());
    let claims = ServiceAccountClaims {
        iss: &credential.client_email,
        scope: scopes,
        aud: token_url.as_str(),
        iat: now,
        exp,
    };
    let key = EncodingKey::from_rsa_pem(credential.private_key.as_bytes()).map_err(|_| {
        GeminiFailure::contract("Gemini service-account private key is not a valid RSA PEM key")
    })?;
    let assertion = jsonwebtoken::encode(&header, &claims, &key).map_err(|_| {
        GeminiFailure::contract("Gemini service-account JWT assertion signing failed")
    })?;
    let response = context
        .http
        .post(token_url.clone())
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[("grant_type", JWT_BEARER_GRANT), ("assertion", &assertion)])
        .send()
        .await
        .map_err(|_| {
            GeminiFailure::transport(
                "Gemini service-account token exchange transport failed",
                context.transport_retryable,
            )
        })?;
    parse_oauth_response(
        response,
        "service-account token exchange",
        context.retryable_http_statuses,
        scopes,
        context.transport_retryable,
    )
    .await
}

async fn fetch_metadata_token(
    context: &AuthRequestContext<'_>,
    token_url: &reqwest::Url,
    scopes: &str,
) -> Result<FetchedAccessToken, GeminiFailure> {
    let mut url = token_url.clone();
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("enforce_scopes", "true");
        query.append_pair("scopes", &scopes.replace(' ', ","));
    }
    let response = context
        .http
        .get(url)
        .header("Metadata-Flavor", "Google")
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| {
            GeminiFailure::transport(
                "Gemini metadata access-token request transport failed",
                context.transport_retryable,
            )
        })?;
    let metadata_flavor = response
        .headers()
        .get("Metadata-Flavor")
        .and_then(|value| value.to_str().ok());
    if metadata_flavor != Some("Google") {
        return Err(GeminiFailure::contract(
            "Gemini metadata token response did not prove Metadata-Flavor: Google",
        ));
    }
    parse_oauth_response(
        response,
        "metadata token endpoint",
        context.retryable_http_statuses,
        scopes,
        context.transport_retryable,
    )
    .await
}

async fn fetch_workload_identity_token(
    context: &AuthRequestContext<'_>,
    subject_token: &SecretLocator,
    token_url: &reqwest::Url,
    audience: &str,
    scopes: &str,
    subject_token_type: &str,
) -> Result<FetchedAccessToken, GeminiFailure> {
    let subject_token = load_secret(subject_token, "workload identity subject token")?;
    if subject_token.len() > MAX_SECRET_BYTES {
        return Err(GeminiFailure::contract(
            "Gemini workload identity subject token exceeds the bounded secret size",
        ));
    }
    let response = context
        .http
        .post(token_url.clone())
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[
            ("grant_type", TOKEN_EXCHANGE_GRANT),
            ("audience", audience),
            ("scope", scopes),
            ("requested_token_type", ACCESS_TOKEN_TYPE),
            ("subject_token", subject_token.as_str()),
            ("subject_token_type", subject_token_type),
        ])
        .send()
        .await
        .map_err(|_| {
            GeminiFailure::transport(
                "Gemini workload-identity token exchange transport failed",
                context.transport_retryable,
            )
        })?;
    let status = response.status().as_u16();
    let bytes = read_auth_response(response, context.transport_retryable).await?;
    if !(200..300).contains(&status) {
        return Err(auth_endpoint_failure(
            "workload-identity token exchange",
            status,
            &bytes,
            context.retryable_http_statuses.contains(&status),
        ));
    }
    let response: StsTokenResponse = serde_json::from_slice(&bytes).map_err(|_| {
        auth_response_failure(
            "Gemini workload-identity token response is not strict JSON",
            &bytes,
        )
    })?;
    if response.issued_token_type != ACCESS_TOKEN_TYPE {
        return Err(auth_response_failure(
            "Gemini workload-identity token response has an unexpected issued token type",
            &bytes,
        ));
    }
    validate_fetched_token(
        response.access_token,
        response.token_type,
        response.expires_in,
        response.scope.as_deref(),
        scopes,
        &bytes,
    )
}

async fn parse_oauth_response(
    response: reqwest::Response,
    surface: &str,
    retryable_http_statuses: &BTreeSet<u16>,
    expected_scopes: &str,
    transport_retryable: bool,
) -> Result<FetchedAccessToken, GeminiFailure> {
    let status = response.status().as_u16();
    let bytes = read_auth_response(response, transport_retryable).await?;
    if !(200..300).contains(&status) {
        return Err(auth_endpoint_failure(
            surface,
            status,
            &bytes,
            retryable_http_statuses.contains(&status),
        ));
    }
    let response: OAuthTokenResponse = serde_json::from_slice(&bytes).map_err(|_| {
        auth_response_failure("Gemini OAuth token response is not strict JSON", &bytes)
    })?;
    validate_fetched_token(
        response.access_token,
        response.token_type,
        response.expires_in,
        response.scope.as_deref(),
        expected_scopes,
        &bytes,
    )
}

fn validate_fetched_token(
    access_token: String,
    token_type: String,
    expires_in: u64,
    scope: Option<&str>,
    expected_scopes: &str,
    response_bytes: &[u8],
) -> Result<FetchedAccessToken, GeminiFailure> {
    if token_type != "Bearer" {
        return Err(auth_response_failure(
            "Gemini token endpoint returned a non-Bearer token",
            response_bytes,
        ));
    }
    if access_token.is_empty()
        || access_token.len() > MAX_ACCESS_TOKEN_BYTES
        || access_token.trim() != access_token
        || access_token.chars().any(char::is_control)
    {
        return Err(auth_response_failure(
            "Gemini token endpoint returned an invalid access token",
            response_bytes,
        ));
    }
    if expires_in == 0 || expires_in > MAX_TOKEN_LIFETIME_SECONDS {
        return Err(auth_response_failure(
            "Gemini token endpoint returned an invalid token lifetime",
            response_bytes,
        ));
    }
    if let Some(scope) = scope {
        let returned = scope.split(' ').collect::<Vec<_>>();
        let expected = expected_scopes.split(' ').collect::<BTreeSet<_>>();
        let returned_set = returned.iter().copied().collect::<BTreeSet<_>>();
        if scope.is_empty()
            || scope.trim() != scope
            || returned.iter().any(|value| value.is_empty())
            || returned.len() != returned_set.len()
            || returned_set != expected
        {
            return Err(auth_response_failure(
                "Gemini token endpoint returned scopes outside the descriptor request",
                response_bytes,
            ));
        }
    }
    Ok(FetchedAccessToken {
        value: access_token,
        expires_in_seconds: expires_in,
    })
}

async fn read_auth_response(
    mut response: reqwest::Response,
    transport_retryable: bool,
) -> Result<Vec<u8>, GeminiFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_AUTH_RESPONSE_BYTES as u64)
    {
        return Err(GeminiFailure::response_limit(
            "Gemini auth endpoint response exceeds the bounded response size",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        GeminiFailure::transport(
            "Gemini auth endpoint response read failed",
            transport_retryable,
        )
    })? {
        if bytes.len().saturating_add(chunk.len()) > MAX_AUTH_RESPONSE_BYTES {
            return Err(GeminiFailure::response_limit(
                "Gemini auth endpoint response exceeds the bounded response size",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn auth_endpoint_failure(
    surface: &str,
    status: u16,
    bytes: &[u8],
    retryable: bool,
) -> GeminiFailure {
    let code = serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .and_then(|value| match value {
            Value::String(code) if !code.is_empty() && code.len() <= 128 => Some(code),
            Value::Object(object) => object
                .get("status")
                .and_then(Value::as_str)
                .filter(|code| !code.is_empty() && code.len() <= 128)
                .map(str::to_string),
            _ => None,
        });
    GeminiFailure {
        class: "auth_endpoint",
        retryable,
        status: Some(status),
        code,
        detail: format!("Gemini {surface} returned HTTP {status}"),
        response_bytes: Some(bytes.len()),
        response_blake3: Some(blake3::hash(bytes).to_hex().to_string()),
    }
}

fn auth_response_failure(detail: &'static str, bytes: &[u8]) -> GeminiFailure {
    GeminiFailure {
        class: "auth_response_contract",
        retryable: false,
        status: Some(200),
        code: None,
        detail: detail.to_string(),
        response_bytes: Some(bytes.len()),
        response_blake3: Some(blake3::hash(bytes).to_hex().to_string()),
    }
}

fn validate_service_account_credential(
    credential: &ServiceAccountCredential,
    token_url: &reqwest::Url,
) -> Result<(), GeminiFailure> {
    if credential.credential_type != "service_account" {
        return Err(GeminiFailure::contract(
            "Gemini service-account credential has an unexpected type",
        ));
    }
    if credential.token_uri != token_url.as_str() {
        return Err(GeminiFailure::contract(
            "Gemini service-account credential token_uri does not match the descriptor token URL",
        ));
    }
    if !valid_service_account_email(&credential.client_email) {
        return Err(GeminiFailure::contract(
            "Gemini service-account credential has an invalid client_email",
        ));
    }
    if credential.private_key_id.is_empty()
        || credential.private_key_id.len() > 128
        || !credential
            .private_key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(GeminiFailure::contract(
            "Gemini service-account credential has an invalid private_key_id",
        ));
    }
    if credential.private_key.len() > 64 * 1024
        || !credential
            .private_key
            .starts_with("-----BEGIN PRIVATE KEY-----\n")
        || !credential
            .private_key
            .ends_with("-----END PRIVATE KEY-----\n")
    {
        return Err(GeminiFailure::contract(
            "Gemini service-account credential has an invalid private_key carrier",
        ));
    }
    let _ = (
        &credential.project_id,
        &credential.client_id,
        &credential.auth_uri,
        &credential.auth_provider_x509_cert_url,
        &credential.client_x509_cert_url,
        &credential.universe_domain,
    );
    Ok(())
}

fn valid_service_account_email(value: &str) -> bool {
    value.len() <= 320
        && value.ends_with(".gserviceaccount.com")
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && !domain.is_empty()
                && local
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                && domain
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        })
}

fn reject_dynamic_configs(
    service_account: &Option<ServiceAccountJwtConfig>,
    metadata: &Option<MetadataAccessTokenConfig>,
    workload_identity: &Option<WorkloadIdentityFederationConfig>,
) -> Result<(), String> {
    if service_account.is_some() || metadata.is_some() || workload_identity.is_some() {
        return Err("Gemini static auth source cannot retain a dynamic auth config".to_string());
    }
    Ok(())
}

fn reject_transport_secret_sources(transport: &InterfaceTransportConfig) -> Result<(), String> {
    if transport.auth_env_key.is_some() || transport.auth_secret_ref.is_some() {
        return Err(
            "Gemini dynamic Google auth cannot retain transport auth_env_key or auth_secret_ref"
                .to_string(),
        );
    }
    Ok(())
}

fn require_bearer_header(header: &HeaderName, value_prefix: &str) -> Result<(), String> {
    if header != reqwest::header::AUTHORIZATION || value_prefix != "Bearer " {
        return Err(
            "Gemini dynamic Google auth requires Authorization with exact 'Bearer ' prefix"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_cache_skew(value: u64) -> Result<(), String> {
    if !(30..=900).contains(&value) {
        return Err("Gemini auth refresh_skew_seconds must be within 30..=900".to_string());
    }
    Ok(())
}

fn validate_oauth_token_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = strict_url(raw, "service-account token URL")?;
    if url.as_str() != "https://oauth2.googleapis.com/token" {
        return Err(
            "Gemini service-account token URL must be exactly https://oauth2.googleapis.com/token"
                .to_string(),
        );
    }
    Ok(url)
}

fn validate_metadata_token_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = strict_url(raw, "metadata token URL")?;
    let allowed_host = matches!(
        url.host_str(),
        Some("metadata.google.internal") | Some("169.254.169.254")
    );
    if url.scheme() != "http"
        || !allowed_host
        || url.path() != "/computeMetadata/v1/instance/service-accounts/default/token"
    {
        return Err(
            "Gemini metadata token URL must name the exact Google metadata access-token path"
                .to_string(),
        );
    }
    Ok(url)
}

fn validate_sts_token_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = strict_url(raw, "workload-identity token URL")?;
    let host = url
        .host_str()
        .ok_or_else(|| "Gemini workload-identity token URL has no host".to_string())?;
    let allowed_host = host == "sts.googleapis.com"
        || host
            .strip_prefix("sts.")
            .and_then(|value| value.strip_suffix(".rep.googleapis.com"))
            .is_some_and(valid_google_region);
    if url.scheme() != "https" || !allowed_host || url.path() != "/v1/token" {
        return Err(
            "Gemini workload-identity token URL must name a Google global or regional STS v1 endpoint"
                .to_string(),
        );
    }
    Ok(url)
}

fn strict_url(raw: &str, surface: &str) -> Result<reqwest::Url, String> {
    if raw.is_empty() || raw.trim() != raw {
        return Err(format!("Gemini {surface} must be a non-empty trimmed URL"));
    }
    let url = reqwest::Url::parse(raw)
        .map_err(|error| format!("Gemini {surface} is invalid: {error}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "Gemini {surface} cannot contain credentials, a port, query, or fragment"
        ));
    }
    Ok(url)
}

fn validate_scopes(values: &[String]) -> Result<String, String> {
    if values.is_empty() || values.len() > 32 {
        return Err("Gemini Google auth scopes must contain 1..=32 entries".to_string());
    }
    let mut unique = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let url = strict_url(value, &format!("Google auth scope[{index}]"))?;
        if url.scheme() != "https"
            || url.host_str() != Some("www.googleapis.com")
            || !url.path().starts_with("/auth/")
            || url.path().len() <= "/auth/".len()
        {
            return Err(format!(
                "Gemini Google auth scope[{index}] must be an explicit www.googleapis.com/auth URL"
            ));
        }
        if !unique.insert(value.clone()) {
            return Err(format!("Gemini Google auth scopes repeat '{value}'"));
        }
    }
    Ok(values.join(" "))
}

fn validate_workload_audience(value: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value || value.len() > 1024 {
        return Err("Gemini workload identity audience is invalid".to_string());
    }
    let segments = value.split('/').collect::<Vec<_>>();
    let valid = segments.len() == 11
        && segments[0].is_empty()
        && segments[1].is_empty()
        && segments[2] == "iam.googleapis.com"
        && segments[3] == "projects"
        && !segments[4].is_empty()
        && segments[4].bytes().all(|byte| byte.is_ascii_digit())
        && segments[5] == "locations"
        && segments[6] == "global"
        && segments[7] == "workloadIdentityPools"
        && valid_resource_id(segments[8])
        && segments[9] == "providers"
        && valid_resource_id(segments[10]);
    if !valid {
        return Err(
            "Gemini workload identity audience must be one explicit workload identity pool provider resource"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_subject_token_type(value: &str) -> Result<(), String> {
    if !matches!(
        value,
        "urn:ietf:params:oauth:token-type:jwt"
            | "urn:ietf:params:oauth:token-type:id_token"
            | "urn:ietf:params:oauth:token-type:saml2"
            | "urn:ietf:params:aws:token-type:aws4_request"
    ) {
        return Err("Gemini workload identity subject token type is unsupported".to_string());
    }
    Ok(())
}

fn valid_resource_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn valid_google_region(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn secret_locator(
    reference: &str,
    octopus_home: PathBuf,
    provider_interface: &InterfaceId,
    surface: &str,
) -> Result<SecretLocator, String> {
    let (scope, name) = parse_secret_ref(reference, surface)?;
    if scope.name() != Some(provider_interface.as_str()) {
        return Err(format!(
            "Gemini {surface} scope '{}' must exactly match concrete provider interface '{}'",
            scope.name().unwrap_or("global"),
            provider_interface.as_str()
        ));
    }
    Ok(SecretLocator {
        octopus_home,
        scope,
        name,
    })
}

fn parse_secret_ref(reference: &str, surface: &str) -> Result<(SecretScope, SecretName), String> {
    if reference.is_empty() || reference.trim() != reference || reference.len() > 512 {
        return Err(format!("Gemini {surface} must be a trimmed secret handle"));
    }
    let path = reference
        .strip_prefix("secret://")
        .ok_or_else(|| format!("Gemini {surface} must start with secret://"))?;
    let (scope, name) = path
        .split_once('/')
        .ok_or_else(|| format!("Gemini {surface} must be secret://<scope>/<name>"))?;
    if name.contains('/') {
        return Err(format!("Gemini {surface} must name one scoped secret"));
    }
    Ok((
        SecretScope::named(scope.to_string()).map_err(|error| error.to_string())?,
        SecretName::new(name).map_err(|error| error.to_string())?,
    ))
}

fn load_secret(locator: &SecretLocator, surface: &str) -> Result<String, GeminiFailure> {
    let value = SecretsManager::new_with_keyring_store(
        locator.octopus_home.clone(),
        SecretsBackendKind::Local,
        Arc::new(LocalFileKeyringStore::new(
            locator.octopus_home.join("secrets").join("keyring"),
        )),
    )
    .get(&locator.scope, &locator.name)
    .map_err(|_| GeminiFailure::contract(format!("Gemini {surface} could not be loaded")))?
    .ok_or_else(|| GeminiFailure::contract(format!("Gemini {surface} is not configured")))?;
    if value.is_empty() || value.trim() != value {
        return Err(GeminiFailure::contract(format!(
            "Gemini {surface} is empty or padded"
        )));
    }
    Ok(value)
}

fn required_trimmed<'a>(value: Option<&'a str>, error: &str) -> Result<&'a str, String> {
    value
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .ok_or_else(|| error.to_string())
}

fn unix_now() -> Result<u64, GeminiFailure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| GeminiFailure::contract("Gemini system clock is before the Unix epoch"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_auth_urls_scopes_and_audiences_are_strict() {
        assert!(validate_oauth_token_url("https://oauth2.googleapis.com/token").is_ok());
        assert!(validate_oauth_token_url("https://example.com/token").is_err());
        assert!(validate_metadata_token_url("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token").is_ok());
        assert!(validate_metadata_token_url(
            "http://127.0.0.1/computeMetadata/v1/instance/service-accounts/default/token"
        )
        .is_err());
        assert!(validate_sts_token_url("https://sts.googleapis.com/v1/token").is_ok());
        assert!(
            validate_sts_token_url("https://sts.us-central1.rep.googleapis.com/v1/token").is_ok()
        );
        assert!(validate_sts_token_url("https://sts.googleapis.com.evil.test/v1/token").is_err());
        assert!(
            validate_scopes(&["https://www.googleapis.com/auth/cloud-platform".to_string()])
                .is_ok()
        );
        assert!(validate_scopes(&["https://example.com/auth/cloud-platform".to_string()]).is_err());
        assert!(validate_workload_audience("//iam.googleapis.com/projects/123456789/locations/global/workloadIdentityPools/pool/providers/provider").is_ok());
        assert!(validate_workload_audience("//iam.googleapis.com/projects/project-id/locations/global/workloadIdentityPools/pool/providers/provider").is_err());
    }

    #[tokio::test]
    async fn cached_access_token_obeys_refresh_skew_and_invalidation() {
        let source = DynamicTokenSource::Metadata {
            token_url: validate_metadata_token_url("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token").unwrap(),
            scopes: "https://www.googleapis.com/auth/cloud-platform".to_string(),
            refresh_skew_seconds: 300,
            cache: Mutex::new(Some(CachedAccessToken {
                value: "cached-token".to_string(),
                expires_at_unix_seconds: unix_now().unwrap() + 3600,
            })),
        };
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let retryable_http_statuses = BTreeSet::new();
        let token = source
            .access_token(&AuthRequestContext {
                http: &http,
                transport_retryable: false,
                retryable_http_statuses: &retryable_http_statuses,
            })
            .await
            .unwrap();
        assert_eq!(token, "cached-token");
        source.invalidate().await;
        let cache = match &source {
            DynamicTokenSource::Metadata { cache, .. } => cache,
            _ => unreachable!(),
        };
        assert!(cache.lock().await.is_none());
    }

    #[test]
    fn auth_failures_never_expose_response_bodies() {
        let body = br#"{"error":"invalid_grant","error_description":"private-key-material"}"#;
        let failure = auth_endpoint_failure("token exchange", 400, body, false);
        assert_eq!(failure.code.as_deref(), Some("invalid_grant"));
        assert!(!failure.detail.contains("private-key-material"));
        assert_eq!(failure.response_bytes, Some(body.len()));
        assert!(failure.response_blake3.is_some());
    }

    #[test]
    fn dynamic_auth_dispatch_fingerprint_is_stable_and_source_specific() {
        let source = GeminiAuthSource::Dynamic(Box::new(DynamicTokenSource::Metadata {
            token_url: validate_metadata_token_url("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token").unwrap(),
            scopes: "https://www.googleapis.com/auth/cloud-platform".to_string(),
            refresh_skew_seconds: 300,
            cache: Mutex::new(None),
        }));
        let first = credential_source_fingerprint(&source);
        let second = credential_source_fingerprint(&source);
        let environment =
            credential_source_fingerprint(&GeminiAuthSource::Environment("GEMINI_KEY".into()));
        assert_eq!(first.len(), 64);
        assert_eq!(first, second);
        assert_ne!(first, environment);
    }
}
