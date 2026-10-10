use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::mcp_client::{http_failure_evidence, HttpFailureEvidence};

const DEFAULT_TOKEN_TIMEOUT_MS: u64 = 30_000;
const MAX_TOKEN_TIMEOUT_MS: u64 = 300_000;
const TOKEN_EXPIRY_SKEW_SECS: u64 = 30;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
const DEFAULT_RESPONSE_BYTES: u64 = 1024 * 1024;
const ABSOLUTE_MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OAuthSettings {
    pub(crate) state_path_env: String,
    #[serde(default)]
    pub(crate) client_id_env: Option<String>,
    #[serde(default)]
    pub(crate) client_secret_env: Option<String>,
    pub(crate) redirect_uri: String,
    #[serde(default)]
    pub(crate) scopes: Vec<String>,
    #[serde(default)]
    pub(crate) authorization_server: Option<String>,
    #[serde(default = "default_token_timeout_ms")]
    pub(crate) token_timeout_ms: u64,
    #[serde(default = "default_response_bytes")]
    pub(crate) max_response_bytes: u64,
}

impl OAuthSettings {
    pub(crate) fn validate(&self) -> Result<(), OAuthError> {
        validate_env_name("state_path_env", &self.state_path_env)?;
        if let Some(name) = self.client_id_env.as_deref() {
            validate_env_name("client_id_env", name)?;
        }
        if let Some(name) = self.client_secret_env.as_deref() {
            validate_env_name("client_secret_env", name)?;
        }
        validate_redirect_uri(&self.redirect_uri)?;
        if self.scopes.len() > 64 {
            return Err(OAuthError::Config(
                "oauth scopes contains more than 64 entries".to_string(),
            ));
        }
        for scope in &self.scopes {
            if scope.is_empty()
                || scope.trim() != scope
                || scope.chars().any(char::is_whitespace)
                || scope.contains('\0')
            {
                return Err(OAuthError::Config(format!(
                    "oauth scope '{scope}' is malformed"
                )));
            }
        }
        if let Some(server) = self.authorization_server.as_deref() {
            validate_secure_url("authorization_server", server, true)?;
        }
        if self.token_timeout_ms == 0 || self.token_timeout_ms > MAX_TOKEN_TIMEOUT_MS {
            return Err(OAuthError::Config(format!(
                "oauth token_timeout_ms must be in 1..={MAX_TOKEN_TIMEOUT_MS}"
            )));
        }
        if self.max_response_bytes == 0 || self.max_response_bytes > ABSOLUTE_MAX_RESPONSE_BYTES {
            return Err(OAuthError::Config(format!(
                "oauth max_response_bytes must be in 1..={ABSOLUTE_MAX_RESPONSE_BYTES}"
            )));
        }
        Ok(())
    }

    pub(crate) fn state_handle(&self) -> Result<PathBuf, OAuthError> {
        let raw = std::env::var(&self.state_path_env)
            .map_err(|_| OAuthError::MissingHandle(self.state_path_env.clone()))?;
        let path = PathBuf::from(raw.trim());
        if raw.trim().is_empty() || !path.is_absolute() {
            return Err(OAuthError::Config(format!(
                "oauth state handle '{}' must resolve to an absolute path",
                self.state_path_env
            )));
        }
        Ok(path)
    }

    fn configured_client_id(&self) -> Result<Option<String>, OAuthError> {
        read_optional_secret_handle(self.client_id_env.as_deref())
    }

    fn configured_client_secret(&self) -> Result<Option<String>, OAuthError> {
        read_optional_secret_handle(self.client_secret_env.as_deref())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum OAuthError {
    #[error("mcp oauth config invalid: {0}")]
    Config(String),
    #[error("mcp oauth storage handle environment variable '{0}' is unset")]
    MissingHandle(String),
    #[error("mcp oauth state at {path} is invalid: {reason}")]
    State { path: PathBuf, reason: String },
    #[error("mcp oauth discovery failed: {0}")]
    Discovery(String),
    #[error("mcp oauth endpoint {endpoint} returned HTTP {status}: {evidence}")]
    HttpStatus {
        endpoint: String,
        status: u16,
        evidence: HttpFailureEvidence,
    },
    #[error("mcp oauth transport failed for {endpoint}: {source}")]
    Http {
        endpoint: String,
        source: reqwest::Error,
    },
    #[error("mcp oauth authorization is required")]
    AuthorizationRequired { authorization: Value },
    #[error("mcp oauth authorization state does not match the pending PKCE flow")]
    StateMismatch,
    #[error("mcp oauth token response is malformed: {0}")]
    Token(String),
    #[error("mcp oauth response from {endpoint} exceeded {limit} bytes")]
    ResponseTooLarge { endpoint: String, limit: u64 },
}

impl OAuthError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Config(_) => "oauth_config",
            Self::MissingHandle(_) => "oauth_missing_storage_handle",
            Self::State { .. } => "oauth_state",
            Self::Discovery(_) => "oauth_discovery",
            Self::HttpStatus { status: 401, .. } => "oauth_unauthorized",
            Self::HttpStatus { .. } | Self::Http { .. } => "oauth_transport",
            Self::AuthorizationRequired { .. } => "oauth_authorization_required",
            Self::StateMismatch => "oauth_state_mismatch",
            Self::Token(_) => "oauth_token",
            Self::ResponseTooLarge { .. } => "oauth_response_too_large",
        }
    }

    pub(crate) fn authorization(&self) -> Option<&Value> {
        match self {
            Self::AuthorizationRequired { authorization } => Some(authorization),
            _ => None,
        }
    }

    pub(crate) fn http_failure(&self) -> Option<(&str, u16, &HttpFailureEvidence)> {
        match self {
            Self::HttpStatus {
                endpoint,
                status,
                evidence,
            } => Some((endpoint, *status, evidence)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthState {
    schema_version: u32,
    #[serde(default)]
    client: Option<StoredClient>,
    #[serde(default)]
    metadata: Option<OAuthMetadata>,
    #[serde(default)]
    tokens: Option<StoredTokens>,
    #[serde(default)]
    pending: Option<PendingAuthorization>,
}

impl Default for OAuthState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            client: None,
            metadata: None,
            tokens: None,
            pending: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredClient {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTokens {
    access_token: String,
    token_type: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    expires_at_unix_secs: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingAuthorization {
    state: String,
    code_verifier: String,
    created_at_unix_secs: u64,
}

#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    authorization_servers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AuthorizationServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "default_token_type")]
    token_type: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

pub(crate) struct OAuthClient {
    settings: OAuthSettings,
    resource_url: String,
    state_path: PathBuf,
    state: OAuthState,
    http: reqwest::Client,
}

impl OAuthClient {
    pub(crate) fn open(
        settings: OAuthSettings,
        resource_url: &str,
        http: reqwest::Client,
    ) -> Result<Self, OAuthError> {
        settings.validate()?;
        let state_path = settings.state_handle()?;
        let state = read_state(&state_path)?;
        Ok(Self {
            settings,
            resource_url: resource_url.to_string(),
            state_path,
            state,
            http,
        })
    }

    pub(crate) async fn access_token(&mut self) -> Result<String, OAuthError> {
        self.reload_if_changed()?;
        if self.token_is_current() {
            return self
                .state
                .tokens
                .as_ref()
                .map(|tokens| tokens.access_token.clone())
                .ok_or_else(|| OAuthError::Token("current token disappeared".to_string()));
        }
        if self
            .state
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.refresh_token.as_ref())
            .is_some()
        {
            match self.refresh().await {
                Ok(()) => {
                    return self
                        .state
                        .tokens
                        .as_ref()
                        .map(|tokens| tokens.access_token.clone())
                        .ok_or_else(|| {
                            OAuthError::Token("refresh returned no stored token".to_string())
                        });
                }
                Err(error) => {
                    self.state.tokens = None;
                    write_state(&self.state_path, &self.state)?;
                    if !matches!(
                        error,
                        OAuthError::HttpStatus {
                            status: 400 | 401,
                            ..
                        }
                    ) {
                        return Err(error);
                    }
                }
            }
        }
        let authorization = self.start_authorization(false).await?;
        Err(OAuthError::AuthorizationRequired { authorization })
    }

    pub(crate) async fn refresh_after_unauthorized(&mut self) -> Result<bool, OAuthError> {
        self.reload_if_changed()?;
        if self
            .state
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.refresh_token.as_ref())
            .is_none()
        {
            return Ok(false);
        }
        self.refresh().await?;
        Ok(true)
    }

    pub(crate) async fn start_authorization(
        &mut self,
        force_new: bool,
    ) -> Result<Value, OAuthError> {
        self.reload_if_changed()?;
        self.ensure_metadata().await?;
        self.ensure_client().await?;
        if !force_new {
            if let Some(pending) = self.state.pending.as_ref() {
                if now_unix_secs().saturating_sub(pending.created_at_unix_secs) < 600 {
                    return self.authorization_payload(pending);
                }
            }
        }
        let pending = PendingAuthorization {
            state: random_urlsafe(32)?,
            code_verifier: random_urlsafe(64)?,
            created_at_unix_secs: now_unix_secs(),
        };
        let payload = self.authorization_payload(&pending)?;
        self.state.pending = Some(pending);
        write_state(&self.state_path, &self.state)?;
        Ok(payload)
    }

    pub(crate) async fn complete_authorization(
        &mut self,
        code: &str,
        state: &str,
    ) -> Result<Value, OAuthError> {
        self.reload_if_changed()?;
        validate_single_line("authorization code", code)?;
        validate_single_line("authorization state", state)?;
        let pending = self
            .state
            .pending
            .clone()
            .ok_or(OAuthError::StateMismatch)?;
        if !constant_time_eq(pending.state.as_bytes(), state.as_bytes()) {
            return Err(OAuthError::StateMismatch);
        }
        self.ensure_metadata().await?;
        self.ensure_client().await?;
        let mut form = BTreeMap::new();
        form.insert("grant_type", "authorization_code".to_string());
        form.insert("code", code.to_string());
        form.insert("redirect_uri", self.settings.redirect_uri.clone());
        form.insert("code_verifier", pending.code_verifier);
        let response = self.token_request(form).await?;
        self.store_token_response(response, None)?;
        self.state.pending = None;
        write_state(&self.state_path, &self.state)?;
        Ok(self.status())
    }

    pub(crate) fn status(&self) -> Value {
        let tokens = self.state.tokens.as_ref();
        json!({
            "configured": true,
            "state_handle_env": self.settings.state_path_env,
            "has_client": self.state.client.is_some() || self.settings.client_id_env.is_some(),
            "has_access_token": tokens.is_some(),
            "has_refresh_token": tokens.and_then(|value| value.refresh_token.as_ref()).is_some(),
            "expires_at_unix_secs": tokens.and_then(|value| value.expires_at_unix_secs),
            "authorization_pending": self.state.pending.is_some(),
            "issuer": self.state.metadata.as_ref().map(|value| value.issuer.as_str()),
        })
    }

    fn token_is_current(&self) -> bool {
        self.state.tokens.as_ref().is_some_and(|tokens| {
            !tokens.access_token.trim().is_empty()
                && tokens.token_type.eq_ignore_ascii_case("bearer")
                && tokens.expires_at_unix_secs.is_none_or(|expiry| {
                    expiry > now_unix_secs().saturating_add(TOKEN_EXPIRY_SKEW_SECS)
                })
        })
    }

    fn reload_if_changed(&mut self) -> Result<(), OAuthError> {
        self.state = read_state(&self.state_path)?;
        Ok(())
    }

    async fn refresh(&mut self) -> Result<(), OAuthError> {
        self.ensure_metadata().await?;
        self.ensure_client().await?;
        let previous_refresh = self
            .state
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.refresh_token.clone())
            .ok_or_else(|| OAuthError::Token("refresh token is absent".to_string()))?;
        let mut form = BTreeMap::new();
        form.insert("grant_type", "refresh_token".to_string());
        form.insert("refresh_token", previous_refresh.clone());
        if !self.settings.scopes.is_empty() {
            form.insert("scope", self.settings.scopes.join(" "));
        }
        let response = self.token_request(form).await?;
        self.store_token_response(response, Some(previous_refresh))?;
        write_state(&self.state_path, &self.state)
    }

    async fn token_request(
        &self,
        mut form: BTreeMap<&str, String>,
    ) -> Result<TokenResponse, OAuthError> {
        let metadata = self
            .state
            .metadata
            .as_ref()
            .ok_or_else(|| OAuthError::Discovery("metadata is absent".to_string()))?;
        let client = self
            .resolved_client()?
            .ok_or_else(|| OAuthError::Config("oauth client is absent".to_string()))?;
        form.insert("client_id", client.client_id.clone());
        if let Some(secret) = client.client_secret {
            form.insert("client_secret", secret);
        }
        form.insert("resource", self.resource_url.clone());
        let endpoint = metadata.token_endpoint.clone();
        let response = tokio::time::timeout(
            std::time::Duration::from_millis(self.settings.token_timeout_ms),
            self.http.post(&endpoint).form(&form).send(),
        )
        .await
        .map_err(|_| OAuthError::Discovery("token request timed out".to_string()))?
        .map_err(|source| OAuthError::Http {
            endpoint: endpoint.clone(),
            source,
        })?;
        parse_json_response(response, &endpoint, self.settings.max_response_bytes).await
    }

    fn store_token_response(
        &mut self,
        response: TokenResponse,
        previous_refresh: Option<String>,
    ) -> Result<(), OAuthError> {
        validate_single_line("access_token", &response.access_token)?;
        if !response.token_type.eq_ignore_ascii_case("bearer") {
            return Err(OAuthError::Token(format!(
                "unsupported token_type '{}'",
                response.token_type
            )));
        }
        let expires_at_unix_secs = response
            .expires_in
            .map(|expires| now_unix_secs().saturating_add(expires));
        self.state.tokens = Some(StoredTokens {
            access_token: response.access_token,
            token_type: response.token_type,
            refresh_token: response.refresh_token.or(previous_refresh),
            scope: response.scope,
            expires_at_unix_secs,
        });
        Ok(())
    }

    async fn ensure_metadata(&mut self) -> Result<(), OAuthError> {
        if self.state.metadata.is_some() {
            return Ok(());
        }
        let authorization_server = match self.settings.authorization_server.as_deref() {
            Some(value) => value.to_string(),
            None => self.discover_authorization_server().await?,
        };
        let metadata = self
            .discover_authorization_metadata(&authorization_server)
            .await?;
        self.state.metadata = Some(metadata);
        write_state(&self.state_path, &self.state)
    }

    async fn discover_authorization_server(&self) -> Result<String, OAuthError> {
        if let Ok(response) = self.http.head(&self.resource_url).send().await {
            if response.status().as_u16() == 401 {
                for challenge in response.headers().get_all("www-authenticate") {
                    let Ok(challenge) = challenge.to_str() else {
                        continue;
                    };
                    let Some(metadata_url) = bearer_parameter(challenge, "resource_metadata")
                    else {
                        continue;
                    };
                    validate_secure_url("resource_metadata challenge URL", &metadata_url, true)?;
                    let response = self
                        .http
                        .get(&metadata_url)
                        .send()
                        .await
                        .map_err(|source| OAuthError::Http {
                            endpoint: metadata_url.clone(),
                            source,
                        })?;
                    let metadata: ProtectedResourceMetadata = parse_json_response(
                        response,
                        &metadata_url,
                        self.settings.max_response_bytes,
                    )
                    .await?;
                    if let Some(server) = metadata.authorization_servers.first() {
                        validate_secure_url("discovered authorization server", server, true)?;
                        return Ok(server.clone());
                    }
                }
            }
        }
        for candidate in protected_resource_candidates(&self.resource_url)? {
            let response = match self.http.get(&candidate).send().await {
                Ok(response) if response.status().is_success() => response,
                Ok(_) | Err(_) => continue,
            };
            let metadata: ProtectedResourceMetadata =
                parse_json_response(response, &candidate, self.settings.max_response_bytes).await?;
            if let Some(server) = metadata.authorization_servers.first() {
                validate_secure_url("discovered authorization server", server, true)?;
                return Ok(server.clone());
            }
        }
        Err(OAuthError::Discovery(
            "protected resource metadata did not declare an authorization server; configure authorization_server explicitly"
                .to_string(),
        ))
    }

    async fn discover_authorization_metadata(
        &self,
        authorization_server: &str,
    ) -> Result<OAuthMetadata, OAuthError> {
        for candidate in authorization_metadata_candidates(authorization_server)? {
            let response = match self.http.get(&candidate).send().await {
                Ok(response) if response.status().is_success() => response,
                Ok(_) | Err(_) => continue,
            };
            let metadata: AuthorizationServerMetadata =
                parse_json_response(response, &candidate, self.settings.max_response_bytes).await?;
            validate_secure_url("metadata issuer", &metadata.issuer, true)?;
            validate_secure_url(
                "metadata authorization_endpoint",
                &metadata.authorization_endpoint,
                true,
            )?;
            validate_secure_url("metadata token_endpoint", &metadata.token_endpoint, true)?;
            if let Some(endpoint) = metadata.registration_endpoint.as_deref() {
                validate_secure_url("metadata registration_endpoint", endpoint, true)?;
            }
            if !metadata.code_challenge_methods_supported.is_empty()
                && !metadata
                    .code_challenge_methods_supported
                    .iter()
                    .any(|method| method == "S256")
            {
                return Err(OAuthError::Discovery(
                    "authorization server does not support PKCE S256".to_string(),
                ));
            }
            return Ok(OAuthMetadata {
                issuer: metadata.issuer,
                authorization_endpoint: metadata.authorization_endpoint,
                token_endpoint: metadata.token_endpoint,
                registration_endpoint: metadata.registration_endpoint,
                scopes_supported: metadata.scopes_supported,
            });
        }
        Err(OAuthError::Discovery(format!(
            "no OAuth authorization metadata found for '{authorization_server}'"
        )))
    }

    async fn ensure_client(&mut self) -> Result<(), OAuthError> {
        if self.resolved_client()?.is_some() {
            return Ok(());
        }
        let endpoint = self
            .state
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.registration_endpoint.clone())
            .ok_or_else(|| {
                OAuthError::Config(
                    "oauth client_id handle is absent and server has no registration_endpoint"
                        .to_string(),
                )
            })?;
        let body = json!({
            "client_name": "octopus-mcp-client",
            "redirect_uris": [self.settings.redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        });
        let response = self
            .http
            .post(&endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|source| OAuthError::Http {
                endpoint: endpoint.clone(),
                source,
            })?;
        let registered: RegistrationResponse =
            parse_json_response(response, &endpoint, self.settings.max_response_bytes).await?;
        validate_single_line("registered client_id", &registered.client_id)?;
        if let Some(secret) = registered.client_secret.as_deref() {
            validate_single_line("registered client_secret", secret)?;
        }
        self.state.client = Some(StoredClient {
            client_id: registered.client_id,
            client_secret: registered.client_secret,
        });
        write_state(&self.state_path, &self.state)
    }

    fn resolved_client(&self) -> Result<Option<StoredClient>, OAuthError> {
        if let Some(client_id) = self.settings.configured_client_id()? {
            return Ok(Some(StoredClient {
                client_id,
                client_secret: self.settings.configured_client_secret()?,
            }));
        }
        Ok(self.state.client.clone())
    }

    fn authorization_payload(&self, pending: &PendingAuthorization) -> Result<Value, OAuthError> {
        let metadata = self
            .state
            .metadata
            .as_ref()
            .ok_or_else(|| OAuthError::Discovery("metadata is absent".to_string()))?;
        let client = self
            .resolved_client()?
            .ok_or_else(|| OAuthError::Config("oauth client is absent".to_string()))?;
        let challenge = base64_urlsafe_no_pad(&sha256(pending.code_verifier.as_bytes()));
        let mut url = reqwest::Url::parse(&metadata.authorization_endpoint)
            .map_err(|error| OAuthError::Discovery(error.to_string()))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("response_type", "code");
            query.append_pair("client_id", &client.client_id);
            query.append_pair("redirect_uri", &self.settings.redirect_uri);
            query.append_pair("code_challenge", &challenge);
            query.append_pair("code_challenge_method", "S256");
            query.append_pair("state", &pending.state);
            query.append_pair("resource", &self.resource_url);
            if !self.settings.scopes.is_empty() {
                query.append_pair("scope", &self.settings.scopes.join(" "));
            }
        }
        Ok(json!({
            "authorization_url": url.as_str(),
            "state": pending.state,
            "redirect_uri": self.settings.redirect_uri,
            "state_handle_env": self.settings.state_path_env,
        }))
    }
}

pub(crate) async fn start_authorization(
    settings: OAuthSettings,
    resource_url: &str,
    force_new: bool,
) -> Result<Value, OAuthError> {
    let http = build_http_client(settings.token_timeout_ms)?;
    OAuthClient::open(settings, resource_url, http)?
        .start_authorization(force_new)
        .await
}

pub(crate) async fn complete_authorization(
    settings: OAuthSettings,
    resource_url: &str,
    code: &str,
    state: &str,
) -> Result<Value, OAuthError> {
    let http = build_http_client(settings.token_timeout_ms)?;
    OAuthClient::open(settings, resource_url, http)?
        .complete_authorization(code, state)
        .await
}

pub(crate) fn authorization_status(
    settings: OAuthSettings,
    resource_url: &str,
) -> Result<Value, OAuthError> {
    let http = build_http_client(settings.token_timeout_ms)?;
    Ok(OAuthClient::open(settings, resource_url, http)?.status())
}

fn build_http_client(timeout_ms: u64) -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .redirect(crate::mcp_client::same_origin_redirect_policy(5))
        .build()
        .map_err(|error| OAuthError::Config(format!("HTTP client build failed: {error}")))
}

fn read_state(path: &Path) -> Result<OAuthState, OAuthError> {
    let Some(bytes) = read_state_bytes(path)? else {
        return Ok(OAuthState::default());
    };
    let state: OAuthState = serde_json::from_slice(&bytes).map_err(|error| OAuthError::State {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    if state.schema_version != 1 {
        return Err(OAuthError::State {
            path: path.to_path_buf(),
            reason: format!("unsupported schema_version {}", state.schema_version),
        });
    }
    Ok(state)
}

fn write_state(path: &Path, state: &OAuthState) -> Result<(), OAuthError> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| OAuthError::State {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(OAuthError::State {
            path: path.to_path_buf(),
            reason: format!("encoded state exceeds {MAX_STATE_BYTES} bytes"),
        });
    }
    write_state_bytes(path, &bytes)
}

#[cfg(unix)]
fn read_state_bytes(path: &Path) -> Result<Option<Vec<u8>>, OAuthError> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let Some((directory, leaf)) = open_state_directory(path, false)? else {
        return Ok(None);
    };
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )
    };
    if descriptor < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(state_error(path, error));
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(descriptor) };
    let before = validate_state_file(path, &file)?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| state_error(path, error))?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(OAuthError::State {
            path: path.to_path_buf(),
            reason: format!("state exceeds {MAX_STATE_BYTES} bytes"),
        });
    }
    let after = validate_state_file(path, &file)?;
    if before != after {
        return Err(OAuthError::State {
            path: path.to_path_buf(),
            reason: "state file changed while it was read".to_string(),
        });
    }
    Ok(Some(bytes))
}

#[cfg(unix)]
fn write_state_bytes(path: &Path, bytes: &[u8]) -> Result<(), OAuthError> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let (directory, leaf) = open_state_directory(path, true)?.ok_or_else(|| OAuthError::State {
        path: path.to_path_buf(),
        reason: "state parent could not be materialized".to_string(),
    })?;
    let mut temp_name = b".".to_vec();
    temp_name.extend_from_slice(leaf.to_bytes());
    temp_name.extend_from_slice(b".tmp-");
    temp_name.extend_from_slice(random_urlsafe(16)?.as_bytes());
    let temp = std::ffi::CString::new(temp_name).map_err(|error| state_error(path, error))?;
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(state_error(path, std::io::Error::last_os_error()));
    }
    let result = (|| {
        let mut file = unsafe { std::fs::File::from_raw_fd(descriptor) };
        file.write_all(bytes)?;
        file.sync_all()?;
        let renamed = unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temp.as_ptr(),
                directory.as_raw_fd(),
                leaf.as_ptr(),
            )
        };
        if renamed != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let synced = unsafe { libc::fsync(directory.as_raw_fd()) };
        if synced != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok::<(), std::io::Error>(())
    })();
    if let Err(error) = result {
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), temp.as_ptr(), 0);
        }
        return Err(state_error(path, error));
    }
    Ok(())
}

#[cfg(unix)]
fn open_state_directory(
    path: &Path,
    create: bool,
) -> Result<Option<(std::fs::File, std::ffi::CString)>, OAuthError> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

    let parent = path.parent().ok_or_else(|| OAuthError::State {
        path: path.to_path_buf(),
        reason: "state path has no parent".to_string(),
    })?;
    if create {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(parent)
            .map_err(|error| state_error(path, error))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let directory = match options.open(parent) {
        Ok(directory) => directory,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(state_error(path, error)),
    };
    let metadata = directory
        .metadata()
        .map_err(|error| state_error(path, error))?;
    if !metadata.is_dir()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(OAuthError::State {
            path: path.to_path_buf(),
            reason: "state parent must be an owner-only directory owned by the current user"
                .to_string(),
        });
    }
    let leaf = path.file_name().ok_or_else(|| OAuthError::State {
        path: path.to_path_buf(),
        reason: "state path has no file name".to_string(),
    })?;
    let leaf = std::ffi::CString::new(leaf.as_bytes()).map_err(|error| state_error(path, error))?;
    Ok(Some((directory, leaf)))
}

#[cfg(unix)]
fn validate_state_file(
    path: &Path,
    file: &std::fs::File,
) -> Result<(u64, u64, u64, u32, u32, i64, i64, i64, i64), OAuthError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata().map_err(|error| state_error(path, error))?;
    if !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(OAuthError::State {
            path: path.to_path_buf(),
            reason: "state handle must be an owner-only regular file owned by the current user"
                .to_string(),
        });
    }
    if metadata.len() > MAX_STATE_BYTES {
        return Err(OAuthError::State {
            path: path.to_path_buf(),
            reason: format!("state exceeds {MAX_STATE_BYTES} bytes"),
        });
    }
    Ok((
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mode(),
        metadata.uid(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    ))
}

#[cfg(not(unix))]
fn read_state_bytes(path: &Path) -> Result<Option<Vec<u8>>, OAuthError> {
    if !path.exists() {
        return Ok(None);
    }
    let mut file = std::fs::File::open(path).map_err(|error| state_error(path, error))?;
    let metadata = file.metadata().map_err(|error| state_error(path, error))?;
    if !metadata.is_file() || metadata.len() > MAX_STATE_BYTES {
        return Err(OAuthError::State {
            path: path.to_path_buf(),
            reason: "state handle must be a bounded regular file".to_string(),
        });
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| state_error(path, error))?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(OAuthError::State {
            path: path.to_path_buf(),
            reason: format!("state exceeds {MAX_STATE_BYTES} bytes"),
        });
    }
    Ok(Some(bytes))
}

#[cfg(not(unix))]
fn write_state_bytes(path: &Path, bytes: &[u8]) -> Result<(), OAuthError> {
    let parent = path.parent().ok_or_else(|| OAuthError::State {
        path: path.to_path_buf(),
        reason: "state path has no parent".to_string(),
    })?;
    std::fs::create_dir_all(parent).map_err(|error| state_error(path, error))?;
    let temp = path.with_extension(format!("tmp-{}", random_urlsafe(16)?));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(state_error(path, error));
    }
    Ok(())
}

fn state_error(path: &Path, error: impl std::fmt::Display) -> OAuthError {
    OAuthError::State {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}

async fn parse_json_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    endpoint: &str,
    max_response_bytes: u64,
) -> Result<T, OAuthError> {
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes)
    {
        return Err(OAuthError::ResponseTooLarge {
            endpoint: endpoint.to_string(),
            limit: max_response_bytes,
        });
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|source| OAuthError::Http {
        endpoint: endpoint.to_string(),
        source,
    })? {
        let next =
            bytes
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| OAuthError::ResponseTooLarge {
                    endpoint: endpoint.to_string(),
                    limit: max_response_bytes,
                })?;
        if next as u64 > max_response_bytes {
            return Err(OAuthError::ResponseTooLarge {
                endpoint: endpoint.to_string(),
                limit: max_response_bytes,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(OAuthError::HttpStatus {
            endpoint: endpoint.to_string(),
            status: status.as_u16(),
            evidence: http_failure_evidence(&bytes),
        });
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| OAuthError::Token(format!("{endpoint}: {error}")))
}

fn protected_resource_candidates(resource_url: &str) -> Result<Vec<String>, OAuthError> {
    let resource = reqwest::Url::parse(resource_url)
        .map_err(|error| OAuthError::Discovery(error.to_string()))?;
    let origin = format!(
        "{}://{}{}",
        resource.scheme(),
        resource.host_str().unwrap_or_default(),
        resource
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default()
    );
    let path = resource.path().trim_start_matches('/');
    let mut candidates = Vec::new();
    if !path.is_empty() {
        candidates.push(format!(
            "{origin}/.well-known/oauth-protected-resource/{path}"
        ));
    }
    candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));
    Ok(candidates)
}

fn authorization_metadata_candidates(server: &str) -> Result<Vec<String>, OAuthError> {
    let url =
        reqwest::Url::parse(server).map_err(|error| OAuthError::Discovery(error.to_string()))?;
    let origin = format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default()
    );
    let path = url.path().trim_matches('/');
    let mut candidates = Vec::new();
    if !path.is_empty() {
        candidates.push(format!(
            "{origin}/.well-known/oauth-authorization-server/{path}"
        ));
    }
    candidates.push(format!(
        "{}/.well-known/oauth-authorization-server",
        server.trim_end_matches('/')
    ));
    candidates.push(format!(
        "{}/.well-known/openid-configuration",
        server.trim_end_matches('/')
    ));
    let mut seen = std::collections::BTreeSet::new();
    candidates.retain(|candidate| seen.insert(candidate.clone()));
    Ok(candidates)
}

fn validate_redirect_uri(raw: &str) -> Result<(), OAuthError> {
    let url = reqwest::Url::parse(raw)
        .map_err(|error| OAuthError::Config(format!("redirect_uri is invalid: {error}")))?;
    if url.scheme() != "http" || !matches!(url.host_str(), Some("127.0.0.1" | "localhost")) {
        return Err(OAuthError::Config(
            "redirect_uri must be an http loopback URL".to_string(),
        ));
    }
    if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return Err(OAuthError::Config(
            "redirect_uri must not contain credentials or a fragment".to_string(),
        ));
    }
    Ok(())
}

fn bearer_parameter(challenge: &str, name: &str) -> Option<String> {
    let (scheme, parameters) = challenge.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    for parameter in parameters.split(',') {
        let Some((key, value)) = parameter.trim().split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case(name) {
            let value = value.trim();
            if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                return Some(value[1..value.len() - 1].replace("\\\"", "\""));
            }
            return Some(value.to_string());
        }
    }
    None
}

fn validate_secure_url(
    field: &str,
    raw: &str,
    allow_loopback_http: bool,
) -> Result<(), OAuthError> {
    let url = reqwest::Url::parse(raw)
        .map_err(|error| OAuthError::Config(format!("{field} is invalid: {error}")))?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"));
    if url.scheme() != "https" && !(allow_loopback_http && url.scheme() == "http" && loopback) {
        return Err(OAuthError::Config(format!(
            "{field} must use https except for loopback development endpoints"
        )));
    }
    if url.host_str().is_none()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(OAuthError::Config(format!("{field} is malformed")));
    }
    Ok(())
}

fn read_optional_secret_handle(name: Option<&str>) -> Result<Option<String>, OAuthError> {
    let Some(name) = name else {
        return Ok(None);
    };
    let value = std::env::var(name).map_err(|_| OAuthError::MissingHandle(name.to_string()))?;
    validate_single_line(name, &value)?;
    Ok(Some(value))
}

fn validate_env_name(field: &str, value: &str) -> Result<(), OAuthError> {
    if value.is_empty()
        || value.trim() != value
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
        })
    {
        return Err(OAuthError::Config(format!(
            "oauth {field} '{value}' is not a valid environment variable name"
        )));
    }
    Ok(())
}

fn validate_single_line(field: &str, value: &str) -> Result<(), OAuthError> {
    if value.is_empty()
        || value.trim() != value
        || value.contains('\0')
        || value.contains('\r')
        || value.contains('\n')
    {
        return Err(OAuthError::Config(format!("{field} is malformed")));
    }
    Ok(())
}

fn random_urlsafe(bytes: usize) -> Result<String, OAuthError> {
    let mut value = vec![0u8; bytes];
    fill_secure_random(&mut value)?;
    Ok(base64_urlsafe_no_pad(&value))
}

fn fill_secure_random(value: &mut [u8]) -> Result<(), OAuthError> {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut source = std::fs::File::open("/dev/urandom")
            .map_err(|error| OAuthError::Config(format!("secure randomness failed: {error}")))?;
        source
            .read_exact(value)
            .map_err(|error| OAuthError::Config(format!("secure randomness failed: {error}")))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = value;
        Err(OAuthError::Config(
            "secure OS randomness is unavailable on this target".to_string(),
        ))
    }
}

fn base64_urlsafe_no_pad(value: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity((value.len() * 4).div_ceil(3));
    let mut chunks = value.chunks_exact(3);
    for chunk in &mut chunks {
        encoded.push(TABLE[(chunk[0] >> 2) as usize] as char);
        encoded.push(TABLE[(((chunk[0] & 0x03) << 4) | (chunk[1] >> 4)) as usize] as char);
        encoded.push(TABLE[(((chunk[1] & 0x0f) << 2) | (chunk[2] >> 6)) as usize] as char);
        encoded.push(TABLE[(chunk[2] & 0x3f) as usize] as char);
    }
    match chunks.remainder() {
        [first] => {
            encoded.push(TABLE[(first >> 2) as usize] as char);
            encoded.push(TABLE[((first & 0x03) << 4) as usize] as char);
        }
        [first, second] => {
            encoded.push(TABLE[(first >> 2) as usize] as char);
            encoded.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
            encoded.push(TABLE[((second & 0x0f) << 2) as usize] as char);
        }
        _ => {}
    }
    encoded
}

fn sha256(value: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let bit_len = (value.len() as u64).wrapping_mul(8);
    let mut padded = value.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    let mut state = INITIAL;
    for block in padded.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(ROUND[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (target, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *target = target.wrapping_add(value);
        }
    }
    let mut output = [0u8; 32];
    for (index, word) in state.into_iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    output
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn default_token_timeout_ms() -> u64 {
    DEFAULT_TOKEN_TIMEOUT_MS
}

fn default_response_bytes() -> u64 {
    DEFAULT_RESPONSE_BYTES
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn oauth_json_response_is_bounded_before_deserialization() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).await;
            let body = vec![b'x'; 1024];
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        });
        let endpoint = format!("http://{address}/metadata");
        let response = reqwest::Client::new().get(&endpoint).send().await.unwrap();
        assert!(matches!(
            parse_json_response::<Value>(response, &endpoint, 64).await,
            Err(OAuthError::ResponseTooLarge { limit: 64, .. })
        ));
    }

    #[tokio::test]
    async fn oauth_non_success_never_retains_echoed_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let secret = "client-secret-echo";
        let body = format!("{{\"error\":\"invalid_client\",\"echo\":\"{secret}\"}}");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).await;
            let head = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(body.as_bytes()).await.unwrap();
        });
        let endpoint = format!("http://{address}/token");
        let response = build_http_client(1000)
            .unwrap()
            .get(&endpoint)
            .send()
            .await
            .unwrap();
        let error = parse_json_response::<Value>(response, &endpoint, 1024)
            .await
            .unwrap_err();
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains(secret));
        let OAuthError::HttpStatus { evidence, .. } = error else {
            panic!("expected HTTP status evidence");
        };
        assert_eq!(evidence.safe_error_code.as_deref(), Some("invalid_client"));
        assert!(evidence.body_bytes > 0);
    }

    #[test]
    fn pkce_payload_never_exposes_the_verifier() {
        let settings = OAuthSettings {
            state_path_env: "TEST_MCP_OAUTH_STATE".to_string(),
            client_id_env: None,
            client_secret_env: None,
            redirect_uri: "http://127.0.0.1:9876/callback".to_string(),
            scopes: vec!["mcp:tools".to_string()],
            authorization_server: None,
            token_timeout_ms: 1000,
            max_response_bytes: DEFAULT_RESPONSE_BYTES,
        };
        let pending = PendingAuthorization {
            state: "state-value".to_string(),
            code_verifier: "verifier-value".to_string(),
            created_at_unix_secs: now_unix_secs(),
        };
        let client = OAuthClient {
            settings,
            resource_url: "https://mcp.example/mcp".to_string(),
            state_path: PathBuf::from("/tmp/unused"),
            state: OAuthState {
                client: Some(StoredClient {
                    client_id: "client".to_string(),
                    client_secret: None,
                }),
                metadata: Some(OAuthMetadata {
                    issuer: "https://auth.example".to_string(),
                    authorization_endpoint: "https://auth.example/authorize".to_string(),
                    token_endpoint: "https://auth.example/token".to_string(),
                    registration_endpoint: None,
                    scopes_supported: Vec::new(),
                }),
                ..OAuthState::default()
            },
            http: reqwest::Client::new(),
        };
        let value = client.authorization_payload(&pending).unwrap();
        let url = value["authorization_url"].as_str().unwrap();
        assert!(url.contains("code_challenge="));
        assert!(!url.contains("verifier-value"));
        assert_eq!(value["state"], "state-value");
    }

    #[cfg(unix)]
    #[test]
    fn oauth_state_refuses_a_symlink_leaf() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = root.path().join("target.json");
        write_state(&target, &OAuthState::default()).unwrap();
        let link = root.path().join("state.json");
        symlink(&target, &link).unwrap();
        assert!(matches!(read_state(&link), Err(OAuthError::State { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn oauth_state_roundtrip_is_owner_only_and_bounded() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().join("state.json");
        write_state(&path, &OAuthState::default()).unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        let state = read_state(&path).unwrap();
        assert_eq!(state.schema_version, 1);
        assert!(state.client.is_none());
        assert!(state.metadata.is_none());
        assert!(state.tokens.is_none());
        assert!(state.pending.is_none());
    }

    #[test]
    fn pkce_primitives_match_sha256_and_base64url_vectors() {
        assert_eq!(
            sha256(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
        assert_eq!(
            base64_urlsafe_no_pad(b"any carnal pleasure."),
            "YW55IGNhcm5hbCBwbGVhc3VyZS4"
        );
        assert_eq!(
            bearer_parameter(
                "Bearer realm=\"mcp\", resource_metadata=\"https://mcp.example/.well-known/oauth-protected-resource\"",
                "resource_metadata"
            )
            .as_deref(),
            Some("https://mcp.example/.well-known/oauth-protected-resource")
        );
    }
}
