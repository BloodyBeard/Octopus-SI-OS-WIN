use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;

use crate::ExecServerError;
use crate::ExecServerRuntimePaths;
use crate::ExecutorFileSystem;
use crate::HttpClient;
use crate::NoiseChannelIdentity;
use crate::NoiseRendezvousConnectProvider;
use crate::client::LazyRemoteExecServerClient;
use crate::client::http_client::ReqwestHttpClient;
use crate::client_api::DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT;
use crate::client_api::ExecServerTransportParams;
use crate::environment_provider::DefaultEnvironmentProvider;
use crate::environment_provider::EnvironmentDefault;
use crate::environment_provider::EnvironmentProvider;
use crate::environment_provider::EnvironmentProviderSnapshot;
use crate::environment_provider::normalize_exec_server_url;
use crate::environment_toml::environment_provider_from_codex_home;
use crate::inlined::shell_detect::DetectedShell;
use crate::local_file_system::LocalFileSystem;
use crate::local_process::LocalProcess;
use crate::process::ExecBackend;
use crate::protocol::EnvironmentInfo;
use crate::protocol::ShellInfo;
use crate::remote::NoiseRendezvousEnvironmentConfig;
use crate::remote_file_system::RemoteFileSystem;
use crate::remote_process::RemoteProcess;
use tokio_util::task::AbortOnDropHandle;

pub const CODEX_EXEC_SERVER_URL_ENV_VAR: &str = "CODEX_EXEC_SERVER_URL";
pub const CODEX_EXEC_SERVER_NOISE_REGISTRY_URL_ENV_VAR: &str =
    "CODEX_EXEC_SERVER_NOISE_REGISTRY_URL";
pub const CODEX_EXEC_SERVER_NOISE_ENVIRONMENT_ID_ENV_VAR: &str =
    "CODEX_EXEC_SERVER_NOISE_ENVIRONMENT_ID";
pub const CODEX_EXEC_SERVER_NOISE_AUTH_TOKEN_ENV_VAR: &str = "CODEX_EXEC_SERVER_NOISE_AUTH_TOKEN";
pub const CODEX_EXEC_SERVER_NOISE_CHATGPT_ACCOUNT_ID_ENV_VAR: &str =
    "CODEX_EXEC_SERVER_NOISE_CHATGPT_ACCOUNT_ID";

#[derive(Debug)]
pub struct EnvironmentManager {
    default_environment: Option<String>,
    environments: RwLock<HashMap<String, Arc<Environment>>>,
    local_environment: Option<Arc<Environment>>,
    local_runtime_paths: Option<ExecServerRuntimePaths>,
}

pub const LOCAL_ENVIRONMENT_ID: &str = "local";
pub const REMOTE_ENVIRONMENT_ID: &str = "remote";

impl EnvironmentManager {
    pub fn default_for_tests() -> Self {
        Self {
            default_environment: Some(LOCAL_ENVIRONMENT_ID.to_string()),
            environments: RwLock::new(HashMap::from([(
                LOCAL_ENVIRONMENT_ID.to_string(),
                Arc::new(Environment::default_for_tests()),
            )])),
            local_environment: Some(Arc::new(Environment::default_for_tests())),
            local_runtime_paths: None,
        }
    }

    pub fn without_environments() -> Self {
        Self {
            default_environment: None,
            environments: RwLock::new(HashMap::new()),
            local_environment: None,
            local_runtime_paths: None,
        }
    }

    pub async fn create_for_tests(
        exec_server_url: Option<String>,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Self {
        Self::from_default_provider_url(exec_server_url, local_runtime_paths).await
    }

    pub async fn from_codex_home(
        codex_home: impl AsRef<std::path::Path>,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Result<Self, ExecServerError> {
        if let Some(config) = noise_environment_config_from_env()? {
            return Self::from_noise_environment_config(config, local_runtime_paths);
        }
        let provider = environment_provider_from_codex_home(codex_home.as_ref())?;
        Self::from_snapshot(provider.snapshot().await?, local_runtime_paths)
    }

    pub async fn from_env(
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Result<Self, ExecServerError> {
        if let Some(config) = noise_environment_config_from_env()? {
            return Self::from_noise_environment_config(config, local_runtime_paths);
        }
        let provider = DefaultEnvironmentProvider::from_env();
        Self::from_snapshot(provider.snapshot().await?, local_runtime_paths)
    }

    async fn from_default_provider_url(
        exec_server_url: Option<String>,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Self {
        let provider = DefaultEnvironmentProvider::new(exec_server_url);
        match Self::from_snapshot(provider.snapshot_inner(), local_runtime_paths) {
            Ok(manager) => manager,
            Err(err) => panic!("default provider should create valid environments: {err}"),
        }
    }

    fn from_noise_environment_config(
        config: NoiseRendezvousEnvironmentConfig,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Result<Self, ExecServerError> {
        let manager = Self {
            default_environment: Some(REMOTE_ENVIRONMENT_ID.to_string()),
            environments: RwLock::new(HashMap::new()),
            local_environment: None,
            local_runtime_paths,
        };
        manager.upsert_noise_environment(
            REMOTE_ENVIRONMENT_ID.to_string(),
            config.connect_provider(),
        )?;
        Ok(manager)
    }

    pub async fn create_for_tests_with_local(
        exec_server_url: Option<String>,
        local_runtime_paths: ExecServerRuntimePaths,
    ) -> Self {
        let mut snapshot = DefaultEnvironmentProvider::new(exec_server_url).snapshot_inner();
        snapshot.include_local = true;
        match Self::from_snapshot(snapshot, Some(local_runtime_paths)) {
            Ok(manager) => manager,
            Err(err) => panic!("test provider with local should create valid environments: {err}"),
        }
    }

    fn from_snapshot(
        snapshot: EnvironmentProviderSnapshot,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Result<Self, ExecServerError> {
        let EnvironmentProviderSnapshot {
            environments,
            default,
            include_local,
        } = snapshot;
        let mut environment_map =
            HashMap::with_capacity(environments.len() + usize::from(include_local));
        let local_environment = if include_local {
            let local_runtime_paths = local_runtime_paths.clone().ok_or_else(|| {
                ExecServerError::Protocol(
                    "local environment requires configured runtime paths".to_string(),
                )
            })?;
            let local_environment = Arc::new(Environment::local(local_runtime_paths));
            environment_map.insert(
                LOCAL_ENVIRONMENT_ID.to_string(),
                Arc::clone(&local_environment),
            );
            Some(local_environment)
        } else {
            None
        };
        for (id, environment) in environments {
            if id.is_empty() {
                return Err(ExecServerError::Protocol(
                    "environment id cannot be empty".to_string(),
                ));
            }
            if id == LOCAL_ENVIRONMENT_ID {
                return Err(ExecServerError::Protocol(format!(
                    "environment id `{LOCAL_ENVIRONMENT_ID}` is reserved for EnvironmentManager"
                )));
            }
            if environment_map
                .insert(id.clone(), Arc::new(environment))
                .is_some()
            {
                return Err(ExecServerError::Protocol(format!(
                    "environment id `{id}` is duplicated"
                )));
            }
        }
        let default_environment = match default {
            EnvironmentDefault::Disabled => None,
            EnvironmentDefault::EnvironmentId(environment_id) => {
                if !environment_map.contains_key(&environment_id) {
                    return Err(ExecServerError::Protocol(format!(
                        "default environment `{environment_id}` is not configured"
                    )));
                }
                Some(environment_id)
            }
        };
        for environment in environment_map.values() {
            environment.start_connecting();
        }
        Ok(Self {
            default_environment,
            environments: RwLock::new(environment_map),
            local_environment,
            local_runtime_paths,
        })
    }

    pub fn default_environment(&self) -> Option<Arc<Environment>> {
        self.default_environment
            .as_deref()
            .and_then(|environment_id| self.get_environment(environment_id))
    }

    pub fn default_environment_id(&self) -> Option<&str> {
        self.default_environment.as_deref()
    }

    pub fn default_environment_ids(&self) -> Vec<String> {
        let Some(default_environment_id) = self.default_environment.as_ref() else {
            return Vec::new();
        };
        let environments = self
            .environments
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut environment_ids = Vec::with_capacity(environments.len());
        environment_ids.push(default_environment_id.clone());
        environment_ids.extend(
            environments
                .keys()
                .filter(|environment_id| *environment_id != default_environment_id)
                .cloned(),
        );
        environment_ids
    }

    pub fn try_local_environment(&self) -> Option<Arc<Environment>> {
        self.local_environment.as_ref().map(Arc::clone)
    }

    pub fn default_or_local_environment(&self) -> Option<Arc<Environment>> {
        self.default_environment()
            .or_else(|| self.try_local_environment())
    }

    pub fn get_environment(&self, environment_id: &str) -> Option<Arc<Environment>> {
        self.environments
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(environment_id)
            .cloned()
    }

    pub fn upsert_environment(
        &self,
        environment_id: String,
        exec_server_url: String,
        connect_timeout: Option<std::time::Duration>,
    ) -> Result<(), ExecServerError> {
        if environment_id.is_empty() {
            return Err(ExecServerError::Protocol(
                "environment id cannot be empty".to_string(),
            ));
        }
        let (exec_server_url, disabled) = normalize_exec_server_url(Some(exec_server_url));
        if disabled {
            return Err(ExecServerError::Protocol(
                "remote environment cannot use disabled exec-server url".to_string(),
            ));
        }
        let Some(exec_server_url) = exec_server_url else {
            return Err(ExecServerError::Protocol(
                "remote environment requires an exec-server url".to_string(),
            ));
        };
        let environment = Arc::new(Environment::remote_with_transport(
            ExecServerTransportParams::websocket_url(
                exec_server_url,
                connect_timeout.unwrap_or(DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT),
            ),
            self.local_runtime_paths.clone(),
        ));
        environment.start_connecting();
        self.environments
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(environment_id, environment);
        Ok(())
    }

    pub fn upsert_noise_environment(
        &self,
        environment_id: String,
        provider: Arc<dyn NoiseRendezvousConnectProvider>,
    ) -> Result<(), ExecServerError> {
        if environment_id.is_empty() {
            return Err(ExecServerError::Protocol(
                "environment id cannot be empty".to_string(),
            ));
        }
        let identity = NoiseChannelIdentity::generate().map_err(|error| {
            ExecServerError::Protocol(format!(
                "failed to generate Noise harness identity: {error}"
            ))
        })?;
        let environment = Arc::new(Environment::remote_with_transport(
            ExecServerTransportParams::NoiseRendezvous { provider, identity },
            self.local_runtime_paths.clone(),
        ));
        environment.start_connecting();
        self.environments
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(environment_id, environment);
        Ok(())
    }
}

fn noise_environment_config_from_env()
-> Result<Option<NoiseRendezvousEnvironmentConfig>, ExecServerError> {
    noise_environment_config_from_values(
        optional_environment_value(CODEX_EXEC_SERVER_NOISE_REGISTRY_URL_ENV_VAR),
        optional_environment_value(CODEX_EXEC_SERVER_NOISE_ENVIRONMENT_ID_ENV_VAR),
        optional_environment_value(CODEX_EXEC_SERVER_NOISE_AUTH_TOKEN_ENV_VAR),
        optional_environment_value(CODEX_EXEC_SERVER_NOISE_CHATGPT_ACCOUNT_ID_ENV_VAR),
    )
}

fn noise_environment_config_from_values(
    registry_url: Option<String>,
    environment_id: Option<String>,
    auth_token: Option<String>,
    chatgpt_account_id: Option<String>,
) -> Result<Option<NoiseRendezvousEnvironmentConfig>, ExecServerError> {
    let (registry_url, environment_id, auth_token) =
        match (registry_url, environment_id, auth_token) {
            (None, None, None) => return Ok(None),
            (Some(registry_url), Some(environment_id), Some(auth_token)) => {
                (registry_url, environment_id, auth_token)
            }
            _ => {
                return Err(ExecServerError::EnvironmentRegistryConfig(format!(
                    "Noise environment requires {CODEX_EXEC_SERVER_NOISE_REGISTRY_URL_ENV_VAR}, \
{CODEX_EXEC_SERVER_NOISE_ENVIRONMENT_ID_ENV_VAR}, and \
{CODEX_EXEC_SERVER_NOISE_AUTH_TOKEN_ENV_VAR}"
                )));
            }
        };

    let config = NoiseRendezvousEnvironmentConfig::new(
        registry_url,
        environment_id,
        auth_token,
        chatgpt_account_id,
    )?;
    Ok(Some(config))
}

fn optional_environment_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Clone)]
pub struct Environment {
    exec_server_url: Option<String>,
    remote_client: Option<LazyRemoteExecServerClient>,
    startup_task: Arc<Mutex<Option<AbortOnDropHandle<()>>>>,
    exec_backend: Arc<dyn ExecBackend>,
    filesystem: Arc<dyn ExecutorFileSystem>,
    http_client: Arc<dyn HttpClient>,
    local_runtime_paths: Option<ExecServerRuntimePaths>,
}

impl Environment {
    pub fn default_for_tests() -> Self {
        Self {
            exec_server_url: None,
            remote_client: None,
            startup_task: Arc::new(Mutex::new(None)),
            exec_backend: Arc::new(LocalProcess::default()),
            filesystem: Arc::new(LocalFileSystem::unsandboxed()),
            http_client: Arc::new(ReqwestHttpClient),
            local_runtime_paths: None,
        }
    }
}

impl std::fmt::Debug for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Environment")
            .field("exec_server_url", &self.exec_server_url)
            .finish_non_exhaustive()
    }
}

impl Environment {
    pub fn create(
        exec_server_url: Option<String>,
        local_runtime_paths: ExecServerRuntimePaths,
    ) -> Result<Self, ExecServerError> {
        Self::create_inner(exec_server_url, Some(local_runtime_paths))
    }

    pub fn create_for_tests(exec_server_url: Option<String>) -> Result<Self, ExecServerError> {
        Self::create_inner(exec_server_url, None)
    }

    pub fn remote(exec_server_url: String) -> Result<Self, ExecServerError> {
        Self::create_inner(Some(exec_server_url), None)
    }

    pub fn remote_authenticated(
        exec_server_url: String,
        auth: crate::DirectExecServerAuth,
    ) -> Result<Self, ExecServerError> {
        let (exec_server_url, disabled) = normalize_exec_server_url(Some(exec_server_url));
        if disabled {
            return Err(ExecServerError::Protocol(
                "disabled mode does not create an Environment".to_string(),
            ));
        }
        let exec_server_url = exec_server_url.ok_or_else(|| {
            ExecServerError::Protocol(
                "authenticated remote environment requires an exec-server url".to_string(),
            )
        })?;
        Ok(Self::remote_with_transport(
            ExecServerTransportParams::authenticated_websocket_url(
                exec_server_url,
                DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT,
                auth,
            ),
            None,
        ))
    }

    pub fn remote_authenticated_resume(
        exec_server_url: String,
        auth: crate::DirectExecServerAuth,
        resume_session_id: String,
    ) -> Result<Self, ExecServerError> {
        let (exec_server_url, disabled) = normalize_exec_server_url(Some(exec_server_url));
        if disabled {
            return Err(ExecServerError::Protocol(
                "disabled mode does not create an Environment".to_string(),
            ));
        }
        let exec_server_url = exec_server_url.ok_or_else(|| {
            ExecServerError::Protocol(
                "authenticated remote environment requires an exec-server url".to_string(),
            )
        })?;
        if resume_session_id.is_empty() {
            return Err(ExecServerError::Protocol(
                "authenticated remote environment requires a non-empty resume session id"
                    .to_string(),
            ));
        }
        Ok(Self::remote_with_transport(
            ExecServerTransportParams::authenticated_websocket_url_with_resume(
                exec_server_url,
                DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT,
                auth,
                resume_session_id,
            ),
            None,
        ))
    }

    pub fn remote_authenticated_claim(
        exec_server_url: String,
        auth: crate::DirectExecServerAuth,
        claim_session_id: String,
    ) -> Result<Self, ExecServerError> {
        let (exec_server_url, disabled) = normalize_exec_server_url(Some(exec_server_url));
        if disabled {
            return Err(ExecServerError::Protocol(
                "disabled mode does not create an Environment".to_string(),
            ));
        }
        let exec_server_url = exec_server_url.ok_or_else(|| {
            ExecServerError::Protocol(
                "authenticated remote environment requires an exec-server url".to_string(),
            )
        })?;
        crate::protocol::validate_explicit_session_id(&claim_session_id)
            .map_err(|message| ExecServerError::Protocol(message.to_string()))?;
        Ok(Self::remote_with_transport(
            ExecServerTransportParams::authenticated_websocket_url_with_claim(
                exec_server_url,
                DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT,
                auth,
                claim_session_id,
            ),
            None,
        ))
    }

    fn create_inner(
        exec_server_url: Option<String>,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Result<Self, ExecServerError> {
        let (exec_server_url, disabled) = normalize_exec_server_url(exec_server_url);
        if disabled {
            return Err(ExecServerError::Protocol(
                "disabled mode does not create an Environment".to_string(),
            ));
        }

        Ok(match exec_server_url {
            Some(exec_server_url) => Self::remote_inner(exec_server_url, local_runtime_paths),
            None => match local_runtime_paths {
                Some(local_runtime_paths) => Self::local(local_runtime_paths),
                None => Self::default_for_tests(),
            },
        })
    }

    pub(crate) fn local(local_runtime_paths: ExecServerRuntimePaths) -> Self {
        Self {
            exec_server_url: None,
            remote_client: None,
            startup_task: Arc::new(Mutex::new(None)),
            exec_backend: Arc::new(LocalProcess::default()),
            filesystem: Arc::new(LocalFileSystem::with_runtime_paths(
                local_runtime_paths.clone(),
            )),
            http_client: Arc::new(ReqwestHttpClient),
            local_runtime_paths: Some(local_runtime_paths),
        }
    }

    pub(crate) fn remote_inner(
        exec_server_url: String,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Self {
        Self::remote_with_transport(
            ExecServerTransportParams::websocket_url(
                exec_server_url,
                DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT,
            ),
            local_runtime_paths,
        )
    }

    pub(crate) fn remote_with_transport(
        remote_transport: ExecServerTransportParams,
        local_runtime_paths: Option<ExecServerRuntimePaths>,
    ) -> Self {
        let exec_server_url = match &remote_transport {
            ExecServerTransportParams::WebSocketUrl {
                websocket_url: exec_server_url,
                ..
            }
            | ExecServerTransportParams::AuthenticatedWebSocketUrl {
                websocket_url: exec_server_url,
                ..
            } => Some(exec_server_url.clone()),
            ExecServerTransportParams::NoiseRendezvous { .. } => None,
            ExecServerTransportParams::StdioCommand { .. } => None,
        };
        let client = LazyRemoteExecServerClient::new(remote_transport);
        let exec_backend: Arc<dyn ExecBackend> = Arc::new(RemoteProcess::new(client.clone()));
        let filesystem: Arc<dyn ExecutorFileSystem> =
            Arc::new(RemoteFileSystem::new(client.clone()));

        Self {
            exec_server_url,
            remote_client: Some(client.clone()),
            startup_task: Arc::new(Mutex::new(None)),
            exec_backend,
            filesystem,
            http_client: Arc::new(client),
            local_runtime_paths,
        }
    }

    pub fn is_remote(&self) -> bool {
        self.remote_client.is_some()
    }

    pub fn exec_server_url(&self) -> Option<&str> {
        self.exec_server_url.as_deref()
    }

    pub fn local_runtime_paths(&self) -> Option<&ExecServerRuntimePaths> {
        self.local_runtime_paths.as_ref()
    }

    pub async fn info(&self) -> Result<EnvironmentInfo, ExecServerError> {
        match &self.remote_client {
            Some(client) => client.environment_info().await,
            None => Ok(EnvironmentInfo::local()),
        }
    }

    pub fn start_connecting(&self) {
        let Some(client) = &self.remote_client else {
            return;
        };
        let mut startup_task = self
            .startup_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if startup_task.is_none() {
            *startup_task = client.start_connecting();
        }
    }

    pub fn startup_finished(&self) -> bool {
        self.remote_client
            .as_ref()
            .is_none_or(LazyRemoteExecServerClient::startup_finished)
    }

    pub async fn wait_until_ready(&self) -> Result<(), ExecServerError> {
        match &self.remote_client {
            Some(client) => client.wait_until_ready().await,
            None => Ok(()),
        }
    }

    pub async fn remote_session_id(&self) -> Result<String, ExecServerError> {
        let client = self.remote_client.as_ref().ok_or_else(|| {
            ExecServerError::Protocol(
                "session identity requires a remote exec-server environment".to_string(),
            )
        })?;
        let client = client.get().await?;
        client.session_id().ok_or_else(|| {
            ExecServerError::Protocol(
                "remote exec-server did not establish a session identity".to_string(),
            )
        })
    }

    pub async fn release_remote_session(
        &self,
        expected_session_id: &str,
    ) -> Result<String, ExecServerError> {
        crate::protocol::validate_explicit_session_id(expected_session_id)
            .map_err(|message| ExecServerError::Protocol(message.to_string()))?;
        let client = self.remote_client.as_ref().ok_or_else(|| {
            ExecServerError::Protocol(
                "session release requires a remote exec-server environment".to_string(),
            )
        })?;
        client
            .get()
            .await?
            .release_session(expected_session_id)
            .await
    }

    pub async fn attach_exec_process(
        &self,
        process_id: &crate::ProcessId,
    ) -> Result<crate::StartedExecProcess, ExecServerError> {
        let client = self.remote_client.as_ref().ok_or_else(|| {
            ExecServerError::Protocol(
                "process attachment requires a remote exec-server environment".to_string(),
            )
        })?;
        crate::remote_process::RemoteProcess::attach(client, process_id).await
    }

    pub fn get_exec_backend(&self) -> Arc<dyn ExecBackend> {
        Arc::clone(&self.exec_backend)
    }

    pub fn get_http_client(&self) -> Arc<dyn HttpClient> {
        Arc::clone(&self.http_client)
    }

    pub fn get_filesystem(&self) -> Arc<dyn ExecutorFileSystem> {
        Arc::clone(&self.filesystem)
    }

    pub async fn rooted_file_system(
        &self,
        params: crate::FsRootedParams,
    ) -> Result<crate::FsRootedResponse, ExecServerError> {
        let client = self.remote_client.as_ref().ok_or_else(|| {
            ExecServerError::Protocol(
                "rooted filesystem RPC requires a remote exec-server environment".to_string(),
            )
        })?;
        client.get().await?.fs_rooted(params).await
    }
}

impl EnvironmentInfo {
    pub(crate) fn local() -> Self {
        Self {
            shell: crate::inlined::shell_detect::default_user_shell().into(),
        }
    }
}

impl From<DetectedShell> for ShellInfo {
    fn from(shell: DetectedShell) -> Self {
        Self {
            name: shell.name().to_string(),
            path: shell.shell_path.to_string_lossy().into_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use super::Environment;
    use super::EnvironmentManager;
    use super::LOCAL_ENVIRONMENT_ID;
    use super::REMOTE_ENVIRONMENT_ID;
    use super::noise_environment_config_from_values;
    use crate::ExecServerRuntimePaths;
    use crate::ProcessId;
    use crate::client_api::ExecServerTransportParams;
        use crate::environment_provider::EnvironmentDefault;
    use crate::environment_provider::EnvironmentProviderSnapshot;
    use octopus_util_pathuri::PathUri;
    use pretty_assertions::assert_eq;
    use tokio::net::TcpListener;
    use tokio::time::timeout;

    fn test_runtime_paths() -> ExecServerRuntimePaths {
        ExecServerRuntimePaths::new(std::env::current_exe().expect("current exe"), None)
            .expect("runtime paths")
    }

    fn assert_local_environment_unavailable(manager: &EnvironmentManager) {
        assert!(manager.try_local_environment().is_none());
    }

    #[tokio::test]
    async fn noise_environment_config_selects_remote_as_default() {
        let config = noise_environment_config_from_values(
            Some("https://registry.example/api".to_string()),
            Some("environment-requested".to_string()),
            Some("registry-token".to_string()),
            Some("workspace-123".to_string()),
        )
        .expect("parse noise environment configuration")
        .expect("noise environment configuration");

        let manager = EnvironmentManager::from_noise_environment_config(config, None)
            .expect("build environment manager");

        assert_eq!(
            manager.default_environment_id(),
            Some(REMOTE_ENVIRONMENT_ID)
        );
        assert!(
            manager
                .default_environment()
                .expect("remote environment")
                .is_remote()
        );
        assert_local_environment_unavailable(&manager);
    }

    #[tokio::test]
    async fn create_local_environment_does_not_connect() {
        let environment =
            Environment::create(None, test_runtime_paths()).expect("create environment");

        assert_eq!(environment.exec_server_url(), None);
        assert!(!environment.is_remote());
        assert!(environment.info().await.is_ok());
    }

    #[tokio::test]
    async fn environment_manager_normalizes_empty_url() {
        let manager =
            EnvironmentManager::create_for_tests(Some(String::new()), Some(test_runtime_paths()))
                .await;

        let environment = manager.default_environment().expect("default environment");
        assert_eq!(manager.default_environment_id(), Some(LOCAL_ENVIRONMENT_ID));
        assert!(Arc::ptr_eq(
            &environment,
            &manager
                .get_environment(LOCAL_ENVIRONMENT_ID)
                .expect("local environment")
        ));
        assert!(Arc::ptr_eq(
            &environment,
            &manager.try_local_environment().expect("local environment")
        ));
        assert!(manager.try_local_environment().is_some());
        assert!(manager.get_environment(REMOTE_ENVIRONMENT_ID).is_none());
        assert!(!environment.is_remote());
    }

    #[tokio::test]
    async fn disabled_environment_manager_has_no_default_or_local_environment() {
        let manager = EnvironmentManager::without_environments();

        assert!(manager.default_environment().is_none());
        assert_eq!(manager.default_environment_id(), None);
        assert_local_environment_unavailable(&manager);
        assert!(manager.get_environment(LOCAL_ENVIRONMENT_ID).is_none());
        assert!(manager.get_environment(REMOTE_ENVIRONMENT_ID).is_none());
    }

    #[tokio::test]
    async fn environment_manager_reports_remote_url() {
        let manager = EnvironmentManager::create_for_tests(
            Some("ws://127.0.0.1:8765".to_string()),
            Some(test_runtime_paths()),
        )
        .await;

        let environment = manager.default_environment().expect("default environment");
        assert_eq!(
            manager.default_environment_id(),
            Some(REMOTE_ENVIRONMENT_ID)
        );
        assert!(environment.is_remote());
        assert_eq!(environment.exec_server_url(), Some("ws://127.0.0.1:8765"));
        assert!(Arc::ptr_eq(
            &environment,
            &manager
                .get_environment(REMOTE_ENVIRONMENT_ID)
                .expect("remote environment")
        ));
        assert!(manager.get_environment(LOCAL_ENVIRONMENT_ID).is_none());
        assert_local_environment_unavailable(&manager);
    }

    #[tokio::test]
    async fn environment_manager_default_environment_caches_environment() {
        let manager = EnvironmentManager::default_for_tests();

        let first = manager.default_environment().expect("default environment");
        let second = manager.default_environment().expect("default environment");

        assert!(Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(
            &first.get_filesystem(),
            &second.get_filesystem()
        ));
    }

    #[tokio::test]
    async fn environment_manager_builds_from_snapshot() {
        let snapshot = EnvironmentProviderSnapshot {
            environments: vec![(
                REMOTE_ENVIRONMENT_ID.to_string(),
                Environment::create_for_tests(Some("ws://127.0.0.1:8765".to_string()))
                    .expect("remote environment"),
            )],
            default: EnvironmentDefault::EnvironmentId(REMOTE_ENVIRONMENT_ID.to_string()),
            include_local: false,
        };
        let manager = EnvironmentManager::from_snapshot(snapshot, Some(test_runtime_paths()))
            .expect("environment manager");

        assert_eq!(
            manager.default_environment_id(),
            Some(REMOTE_ENVIRONMENT_ID)
        );
        assert!(
            manager
                .get_environment(REMOTE_ENVIRONMENT_ID)
                .expect("remote environment")
                .is_remote()
        );
        assert!(manager.get_environment(LOCAL_ENVIRONMENT_ID).is_none());
        assert_local_environment_unavailable(&manager);
    }

    #[tokio::test]
    async fn environment_manager_rejects_empty_environment_id() {
        let snapshot = EnvironmentProviderSnapshot {
            environments: vec![("".to_string(), Environment::default_for_tests())],
            default: EnvironmentDefault::Disabled,
            include_local: false,
        };
        let err = EnvironmentManager::from_snapshot(snapshot, Some(test_runtime_paths()))
            .expect_err("empty id should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: environment id cannot be empty"
        );
    }

    #[tokio::test]
    async fn environment_manager_rejects_provider_supplied_local_environment() {
        let snapshot = EnvironmentProviderSnapshot {
            environments: vec![(
                LOCAL_ENVIRONMENT_ID.to_string(),
                Environment::default_for_tests(),
            )],
            default: EnvironmentDefault::Disabled,
            include_local: false,
        };
        let err = EnvironmentManager::from_snapshot(snapshot, Some(test_runtime_paths()))
            .expect_err("local id should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: environment id `local` is reserved for EnvironmentManager"
        );
    }

    #[tokio::test]
    async fn environment_manager_uses_explicit_provider_default() {
        let snapshot = EnvironmentProviderSnapshot {
            environments: vec![(
                "devbox".to_string(),
                Environment::create_for_tests(Some("ws://127.0.0.1:8765".to_string()))
                    .expect("remote environment"),
            )],
            default: EnvironmentDefault::EnvironmentId("devbox".to_string()),
            include_local: true,
        };
        let manager = EnvironmentManager::from_snapshot(snapshot, Some(test_runtime_paths()))
            .expect("manager");

        assert_eq!(manager.default_environment_id(), Some("devbox"));
        assert_eq!(
            manager.default_environment_ids(),
            vec!["devbox".to_string(), LOCAL_ENVIRONMENT_ID.to_string()]
        );
        assert!(manager.default_environment().expect("default").is_remote());
    }

    #[tokio::test]
    async fn environment_manager_disables_provider_default() {
        let snapshot = EnvironmentProviderSnapshot {
            environments: vec![(
                "devbox".to_string(),
                Environment::create_for_tests(Some("ws://127.0.0.1:8765".to_string()))
                    .expect("remote environment"),
            )],
            default: EnvironmentDefault::Disabled,
            include_local: true,
        };
        let manager = EnvironmentManager::from_snapshot(snapshot, Some(test_runtime_paths()))
            .expect("manager");

        assert_eq!(manager.default_environment_id(), None);
        assert!(manager.default_environment().is_none());
        assert!(Arc::ptr_eq(
            &manager
                .get_environment(LOCAL_ENVIRONMENT_ID)
                .expect("local environment"),
            &manager.try_local_environment().expect("local environment")
        ));
    }

    #[tokio::test]
    async fn environment_manager_rejects_unknown_provider_default() {
        let snapshot = EnvironmentProviderSnapshot {
            environments: vec![(
                "devbox".to_string(),
                Environment::create_for_tests(Some("ws://127.0.0.1:8765".to_string()))
                    .expect("remote environment"),
            )],
            default: EnvironmentDefault::EnvironmentId("missing".to_string()),
            include_local: true,
        };
        let err = EnvironmentManager::from_snapshot(snapshot, Some(test_runtime_paths()))
            .expect_err("unknown default should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: default environment `missing` is not configured"
        );
    }

    #[tokio::test]
    async fn environment_manager_includes_local_for_default_provider_without_url() {
        let manager = EnvironmentManager::create_for_tests(None, Some(test_runtime_paths())).await;

        let environment = manager.default_environment().expect("default environment");
        assert_eq!(manager.default_environment_id(), Some(LOCAL_ENVIRONMENT_ID));
        assert!(Arc::ptr_eq(
            &environment,
            &manager
                .get_environment(LOCAL_ENVIRONMENT_ID)
                .expect("local environment")
        ));
        assert!(Arc::ptr_eq(
            &environment,
            &manager.try_local_environment().expect("local environment")
        ));
        assert!(!environment.is_remote());
    }

    #[tokio::test]
    async fn environment_manager_carries_local_runtime_paths() {
        let runtime_paths = test_runtime_paths();
        let manager = EnvironmentManager::create_for_tests(None, Some(runtime_paths.clone())).await;

        let environment = manager.try_local_environment().expect("local environment");

        assert_eq!(environment.local_runtime_paths(), Some(&runtime_paths));
        let manager = EnvironmentManager::create_for_tests(
            environment.exec_server_url().map(str::to_owned),
            Some(
                environment
                    .local_runtime_paths()
                    .expect("local runtime paths")
                    .clone(),
            ),
        )
        .await;
        let environment = manager.try_local_environment().expect("local environment");
        assert_eq!(environment.local_runtime_paths(), Some(&runtime_paths));
    }

    #[tokio::test]
    async fn environment_manager_omits_default_provider_local_lookup_when_default_disabled() {
        let manager = EnvironmentManager::create_for_tests(
            Some("none".to_string()),
            Some(test_runtime_paths()),
        )
        .await;

        assert!(manager.default_environment().is_none());
        assert_eq!(manager.default_environment_id(), None);
        assert!(manager.get_environment(LOCAL_ENVIRONMENT_ID).is_none());
        assert!(manager.get_environment(REMOTE_ENVIRONMENT_ID).is_none());
        assert_local_environment_unavailable(&manager);
    }

    #[tokio::test]
    async fn environment_manager_snapshot_without_local_environment_disables_local_default() {
        let mut snapshot = EnvironmentProviderSnapshot {
            environments: Vec::new(),
            default: EnvironmentDefault::EnvironmentId(LOCAL_ENVIRONMENT_ID.to_string()),
            include_local: true,
        };
        snapshot.include_local = false;
        snapshot.default = EnvironmentDefault::Disabled;
        let manager =
            EnvironmentManager::from_snapshot(snapshot, None).expect("environment manager");

        assert!(manager.default_environment().is_none());
        assert_eq!(manager.default_environment_id(), None);
        assert!(manager.get_environment(LOCAL_ENVIRONMENT_ID).is_none());
        assert_local_environment_unavailable(&manager);
    }

    #[tokio::test]
    async fn get_environment_returns_none_for_unknown_id() {
        let manager = EnvironmentManager::default_for_tests();

        assert!(manager.get_environment("does-not-exist").is_none());
    }

    #[tokio::test]
    async fn environment_manager_upserts_named_remote_environment() {
        let manager = EnvironmentManager::without_environments();

        manager
            .upsert_environment(
                "executor-a".to_string(),
                "ws://127.0.0.1:8765".to_string(),
                None,
            )
            .expect("remote environment");
        let first = manager
            .get_environment("executor-a")
            .expect("first remote environment");
        assert!(first.is_remote());
        assert_eq!(first.exec_server_url(), Some("ws://127.0.0.1:8765"));
        assert_eq!(manager.default_environment_id(), None);

        manager
            .upsert_environment(
                "executor-a".to_string(),
                "ws://127.0.0.1:9876".to_string(),
                None,
            )
            .expect("updated remote environment");
        let second = manager
            .get_environment("executor-a")
            .expect("second remote environment");
        assert!(second.is_remote());
        assert_eq!(second.exec_server_url(), Some("ws://127.0.0.1:9876"));
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn environment_manager_starts_remote_environment_when_upserted() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind websocket listener");
        let manager = EnvironmentManager::without_environments();

        manager
            .upsert_environment(
                "executor-a".to_string(),
                format!("ws://{}", listener.local_addr().expect("listener address")),
                None,
            )
            .expect("remote environment");

        timeout(Duration::from_secs(5), listener.accept())
            .await
            .expect("environment should start connecting when registered")
            .expect("accept connection");
    }

    #[tokio::test]
    async fn environment_manager_leaves_stdio_environment_lazy() {
        let environment = Environment::remote_with_transport(
            ExecServerTransportParams::StdioCommand {
                command: crate::client_api::StdioExecServerCommand {
                    program: "codex-missing-exec-server-for-test".to_string(),
                    args: Vec::new(),
                    env: HashMap::new(),
                    cwd: None,
                },
                initialize_timeout: Duration::from_secs(1),
            },
            None,
        );
        let manager = EnvironmentManager::from_snapshot(
            EnvironmentProviderSnapshot {
                environments: vec![("stdio".to_string(), environment)],
                default: EnvironmentDefault::Disabled,
                include_local: false,
            },
            None,
        )
        .expect("environment manager");
        let environment = manager.get_environment("stdio").expect("stdio environment");

        assert!(!environment.startup_finished());
        assert!(environment.wait_until_ready().await.is_err());
        assert!(environment.startup_finished());
    }

    #[tokio::test]
    async fn replacing_environment_stops_its_startup_task() {
        let first_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind first websocket listener");
        let second_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind second websocket listener");
        let manager = EnvironmentManager::without_environments();
        manager
            .upsert_environment(
                "executor-a".to_string(),
                format!(
                    "ws://{}",
                    first_listener.local_addr().expect("first listener address")
                ),
                None,
            )
            .expect("first remote environment");
        let environment = manager
            .get_environment("executor-a")
            .expect("first remote environment");
        let startup_abort = environment
            .startup_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .expect("startup task")
            .abort_handle();
        assert!(!startup_abort.is_finished());
        drop(environment);

        manager
            .upsert_environment(
                "executor-a".to_string(),
                format!(
                    "ws://{}",
                    second_listener
                        .local_addr()
                        .expect("second listener address")
                ),
                None,
            )
            .expect("replacement remote environment");

        timeout(Duration::from_secs(1), async {
            while !startup_abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("replacing the environment should cancel its startup task");
    }

    #[tokio::test]
    async fn environment_manager_rejects_empty_remote_environment_url() {
        let manager = EnvironmentManager::without_environments();

        let err = manager
            .upsert_environment("executor-a".to_string(), String::new(), None)
            .expect_err("empty URL should fail");

        assert_eq!(
            err.to_string(),
            "exec-server protocol error: remote environment requires an exec-server url"
        );
    }

    #[tokio::test]
    async fn default_environment_has_ready_local_executor() {
        let environment = Environment::default_for_tests();

        let response = environment
            .get_exec_backend()
            .start(crate::ExecParams {
                process_id: ProcessId::from("default-env-proc"),
                argv: vec!["true".to_string()],
                cwd: PathUri::from_path(std::env::current_dir().expect("read current dir"))
                    .expect("cwd URI"),
                env_policy: None,
                env: Default::default(),
                tty: false,
                pipe_stdin: false,
                arg0: None,
            })
            .await
            .expect("start process");

        assert_eq!(response.process.process_id().as_str(), "default-env-proc");
    }

    #[tokio::test]
    async fn test_environment_rejects_sandboxed_filesystem_without_runtime_paths() {
        let environment = Environment::default_for_tests();
        let path = octopus_util_abspath::AbsolutePathBuf::from_absolute_path(
            std::env::current_exe().expect("current exe").as_path(),
        )
        .expect("absolute current exe");
        let path = octopus_util_pathuri::PathUri::from_abs_path(&path);
        let sandbox = crate::FileSystemSandboxContext::from_permission_profile(
            octopus_substrate_contract::permission_profile::PermissionProfile::from_runtime_permissions(
                &octopus_substrate_contract::permissions::FileSystemSandboxPolicy::restricted(Vec::new()),
                octopus_substrate_contract::permissions::NetworkSandboxPolicy::Restricted,
            ),
        );

        let err = environment
            .get_filesystem()
            .read_file(&path, Some(&sandbox))
            .await
            .expect_err("sandboxed read should require runtime paths");

        assert_eq!(
            err.to_string(),
            "sandboxed filesystem operations require configured runtime paths"
        );
    }
}
