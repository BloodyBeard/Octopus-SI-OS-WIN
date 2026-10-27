use crate::config::edit::ConfigEdit;
use crate::config::edit::ConfigEditsBuilder;
use crate::path_utils::normalize_for_native_workdir;
use crate::windows_sandbox::WindowsSandboxLevelExt;
use crate::windows_sandbox::resolve_windows_sandbox_mode;
use crate::windows_sandbox::resolve_windows_sandbox_private_desktop;
use octopus_codex_model_provider_info::LEGACY_OLLAMA_CHAT_PROVIDER_ID;
use octopus_codex_model_provider_info::ModelProviderInfo;
use octopus_codex_model_provider_info::OLLAMA_CHAT_PROVIDER_REMOVED_ERROR;
use octopus_config::CloudConfigBundleLoader;
use octopus_config::ConfigLayerSource;
use octopus_config::ConfigLayerStack;
use octopus_config::ConfigLayerStackOrdering;
use octopus_config::ConfigRequirements;
use octopus_config::ConfigRequirementsToml;
use octopus_config::ConstrainedWithSource;
use octopus_config::FeatureRequirementsToml;
use octopus_config::McpServerIdentity;
use octopus_config::McpServerRequirement;
use octopus_config::ProfileV2Name;
use octopus_config::ResidencyRequirement;
use octopus_config::SandboxModeRequirement;
use octopus_config::Sourced;
use octopus_config::config_toml::ConfigLockfileToml;
use octopus_config::config_toml::ConfigToml;
use octopus_config::config_toml::DEFAULT_PROJECT_DOC_MAX_BYTES;
use octopus_config::config_toml::ProjectConfig;
use octopus_config::config_toml::RealtimeAudioConfig;
use octopus_config::config_toml::RealtimeConfig;
use octopus_config::config_toml::validate_model_providers;
use octopus_config::loader::load_config_layers_state;
use octopus_config::loader::project_trust_key;
use octopus_config::permissions_toml::PermissionsToml;
use octopus_config::sandbox_mode_requirement_for_permission_profile;
use octopus_config::types::ApprovalsReviewer;
use octopus_config::types::AuthCredentialsStoreMode;
use octopus_config::types::AuthKeyringBackendKind;
use octopus_config::types::McpServerConfig;
use octopus_config::types::McpServerDisabledReason;
use octopus_config::types::McpServerTransportConfig;
use octopus_config::types::MemoriesConfig;
use octopus_config::types::Notice;
use octopus_config::types::OAuthCredentialsStoreMode;
use octopus_config::types::ToolSuggestConfig;
use octopus_config::types::ToolSuggestDisabledTool;
use octopus_config::types::ToolSuggestDiscoverable;
use octopus_config::types::UriBasedFileOpener;
use octopus_config::types::WindowsSandboxModeToml;
use octopus_features::CodeModeConfigToml;
use octopus_features::CurrentTimeReminderConfigToml;
use octopus_features::CurrentTimeSource;
use octopus_features::Feature;
use octopus_features::FeatureConfigSource;
use octopus_features::FeatureOverrides;
use octopus_features::FeatureToml;
use octopus_features::Features;
use octopus_features::FeaturesToml;
use octopus_features::MultiAgentV2ConfigToml;
use octopus_features::NetworkProxyConfigToml;
use octopus_file_system::ExecutorFileSystem;
use octopus_file_system::LOCAL_FS;
use octopus_git_utils::resolve_root_git_project_for_trust;
use octopus_install_context::InstallContext;
use octopus_login::AuthManagerConfig;
use octopus_protocol::config_types::AutoCompactTokenLimitScope;
use octopus_protocol::config_types::ForcedLoginMethod;
use octopus_protocol::config_types::Personality;
use octopus_protocol::config_types::ReasoningSummary;
use octopus_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use octopus_protocol::config_types::SandboxMode;
use octopus_protocol::config_types::ServiceTier;
use octopus_protocol::config_types::ShellEnvironmentPolicy;
use octopus_protocol::config_types::TrustLevel;
use octopus_protocol::config_types::Verbosity;
use octopus_protocol::config_types::WebSearchConfig;
use octopus_protocol::config_types::WebSearchMode;
use octopus_protocol::config_types::WindowsSandboxLevel;
use octopus_protocol::models::ActivePermissionProfile;
use octopus_protocol::models::PermissionProfile;
use octopus_protocol::models::SandboxEnforcement;
use octopus_protocol::openai_models::ModelsResponse;
use octopus_protocol::openai_models::ReasoningEffort;
use octopus_protocol::permissions::FileSystemSandboxPolicy;
use octopus_protocol::permissions::NetworkSandboxPolicy;
use octopus_protocol::protocol::AskForApproval;
#[cfg(test)]
use octopus_protocol::protocol::MultiAgentVersion;
use octopus_protocol::protocol::SandboxPolicy;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_util_abspath::AbsolutePathBufGuard;
use octopus_util_pathuri::PathUri;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::permissions::BUILT_IN_READ_ONLY_PROFILE;
use crate::config::permissions::BUILT_IN_WORKSPACE_PROFILE;
use crate::config::permissions::apply_network_proxy_feature_config;
use crate::config::permissions::builtin_permission_profile;
use crate::config::permissions::compile_permission_profile_selection;
use crate::config::permissions::compile_permission_profile_workspace_roots;
use crate::config::permissions::default_builtin_permission_profile_name;
use crate::config::permissions::get_readable_roots_required_for_codex_runtime;
use crate::config::permissions::network_proxy_config_for_profile_selection;
use crate::config::permissions::validate_user_permission_profile_names;
use crate::config_lock::config_without_lock_controls;
use crate::config_lock::lock_layer_from_config;
use crate::config_lock::read_config_lock_from_path;
use octopus_network_proxy::NetworkProxyConfig;
use toml::Value as TomlValue;
use toml_edit::DocumentMut;

pub(crate) mod agent_roles;
mod auth_keyring;
pub mod debug_report;
pub mod edit;
mod managed_features;
mod network_proxy_spec;
mod otel;
mod permissions;
mod resolved_permission_profile;
#[cfg(test)]
mod schema;
pub use auth_keyring::resolve_bootstrap_auth_keyring_backend_kind;
pub use managed_features::ManagedFeatures;
pub use network_proxy_spec::NetworkProxySpec;
pub use network_proxy_spec::StartedNetworkProxy;
pub use octopus_config::ConfigLoadOptions;
pub use octopus_config::Constrained;
pub use octopus_config::ConstraintError;
pub use octopus_config::ConstraintResult;
pub use octopus_config::LoaderOverrides;
pub use octopus_network_proxy::NetworkProxyAuditMetadata;
pub(crate) use permissions::is_builtin_permission_profile_name;
pub(crate) use permissions::reject_unknown_builtin_permission_profile;
pub(crate) use permissions::resolve_permission_profile;
pub use resolved_permission_profile::PermissionProfileSnapshot;
pub(crate) use resolved_permission_profile::PermissionProfileState;

const DEFAULT_IGNORE_LARGE_UNTRACKED_DIRS: i64 = 200;
const DEFAULT_IGNORE_LARGE_UNTRACKED_FILES: i64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostSnapshotConfig {
    pub ignore_large_untracked_files: Option<i64>,
    pub ignore_large_untracked_dirs: Option<i64>,
    pub disable_warnings: bool,
}

impl Default for GhostSnapshotConfig {
    fn default() -> Self {
        Self {
            ignore_large_untracked_files: Some(DEFAULT_IGNORE_LARGE_UNTRACKED_FILES),
            ignore_large_untracked_dirs: Some(DEFAULT_IGNORE_LARGE_UNTRACKED_DIRS),
            disable_warnings: false,
        }
    }
}

pub(crate) const AGENTS_MD_MAX_BYTES: usize = DEFAULT_PROJECT_DOC_MAX_BYTES;
#[cfg(test)]
pub(crate) const DEFAULT_AGENT_MAX_THREADS: Option<usize> = Some(6);
pub(crate) const DEFAULT_MULTI_AGENT_V2_MAX_CONCURRENT_THREADS_PER_SESSION: usize = 4;
pub(crate) const DEFAULT_MULTI_AGENT_V2_MIN_WAIT_TIMEOUT_MS: i64 = 10_000;
pub(crate) const DEFAULT_MULTI_AGENT_V2_MAX_WAIT_TIMEOUT_MS: i64 = 3600 * 1000;
pub(crate) const DEFAULT_MULTI_AGENT_V2_DEFAULT_WAIT_TIMEOUT_MS: i64 = 30_000;
const DEFAULT_MULTI_AGENT_V2_ROOT_AGENT_USAGE_HINT_TEXT: &str = r#"You are `/root`, the primary agent in a team of agents collaborating to fulfill the user's goals.

At the start of your turn, you are the active agent.
You can spawn sub-agents to handle subtasks, and those sub-agents can spawn their own sub-agents.
All agents in the team, including the agents that you can assign tasks to, are equally intelligent and capable, and have access to the same set of tools.

You can use `spawn_agent` to create a new agent, `followup_task` to give an existing agent a new task and trigger a turn, and `send_message` to pass a message to a running agent without triggering a turn.
Child agents can also spawn their own sub-agents.
You can decide how much context you want to propagate to your sub-agents with the `fork_turns` parameter.

You will receive messages in the analysis channel in the form:
```
Message Type: MESSAGE | FINAL_ANSWER
Task name: <recipient>
Sender: <author>
Payload:
<payload text>
```
They may be addressed as to=/root
"#;
const DEFAULT_MULTI_AGENT_V2_SUBAGENT_USAGE_HINT_TEXT: &str = r#"You are an agent in a team of agents collaborating to complete a task.

You can spawn sub-agents to handle subtasks, and those sub-agents can spawn their own sub-agents. All agents in the team, including the agents that you can assign tasks to, are equally intelligent and capable, and have access to the same set of tools.

You can use `spawn_agent` to create a new agent, `followup_task` to give an existing agent a new task and trigger a turn, and `send_message` to pass a message to a running agent.
Child agents can also spawn their own sub-agents.

When you provide a response in the final channel, that content is immediately delivered back to your parent agent.

You will receive messages in the analysis channel in the form:
```
Message Type: NEW_TASK | MESSAGE | FINAL_ANSWER
Task name: <recipient>
Sender: <author>
Payload:
<payload text>
```
You may also see them addressed as to=/root/..., which indicates your identity is /root/...
"#;
const DEFAULT_MULTI_AGENT_V2_SHARED_USAGE_HINT_TEXT: &str = r#"Note that collaboration tools cannot be called from inside `functions.exec`. Call `spawn_agent`, `send_message`, `followup_task`, `wait_agent`, `interrupt_agent`, and `list_agents` only as direct tool calls using the recipient shown in their tool definitions, such as `to=functions.spawn_agent` without a configured namespace or `to=functions.agents.spawn_agent` with `tool_namespace = "agents"`, since they are intentionally absent from the `functions.exec` `tools.*` namespace. Available tools in `functions.exec` are explicitly described with a `tools` namespace in the developer message.

The goal is to correctly solve the problem in as little time as possible. Therefore, if at any point you can parallelize work by delegating tasks to another agent, you should do so to save time.

All agents share the same directory. In detail:
- All agents have access to the same container and filesystem as you.
- All agents use the same current working directory.
- As a result, edits made by one agent are immediately visible to all other agents.
"#;
fn default_multi_agent_v2_usage_hint_text(usage_hint_text: &str, max_concurrency: usize) -> String {
    format!(
        "{usage_hint_text}\n{DEFAULT_MULTI_AGENT_V2_SHARED_USAGE_HINT_TEXT}\nThere are {max_concurrency} available concurrency slots, meaning that up to {max_concurrency} agents can be active at once, including you."
    )
}

pub(crate) const HARD_MIN_MULTI_AGENT_V2_TIMEOUT_MS: i64 = 0;
pub(crate) const HARD_MAX_MULTI_AGENT_V2_TIMEOUT_MS: i64 =
    DEFAULT_MULTI_AGENT_V2_MAX_WAIT_TIMEOUT_MS;
pub(crate) const DEFAULT_AGENT_MAX_DEPTH: i32 = 1;
pub(crate) const DEFAULT_AGENT_JOB_MAX_RUNTIME_SECONDS: Option<u64> = None;
const LOCAL_DEV_BUILD_VERSION: &str = "0.0.0";

pub const CONFIG_TOML_FILE: &str = "config.toml";
const CONFIG_PROFILE_V2_SUFFIX: &str = ".config.toml";

fn resolve_cli_auth_credentials_store_mode(
    configured: AuthCredentialsStoreMode,
    package_version: &str,
) -> AuthCredentialsStoreMode {
    match (package_version, configured) {
        (
            LOCAL_DEV_BUILD_VERSION,
            AuthCredentialsStoreMode::Keyring | AuthCredentialsStoreMode::Auto,
        ) => AuthCredentialsStoreMode::File,
        (_, mode) => mode,
    }
}

fn resolve_mcp_oauth_credentials_store_mode(
    configured: OAuthCredentialsStoreMode,
    package_version: &str,
) -> OAuthCredentialsStoreMode {
    match (package_version, configured) {
        (
            LOCAL_DEV_BUILD_VERSION,
            OAuthCredentialsStoreMode::Keyring | OAuthCredentialsStoreMode::Auto,
        ) => OAuthCredentialsStoreMode::File,
        (_, mode) => mode,
    }
}

#[cfg(test)]
const TEST_MODEL_PROVIDER_CONFIG: &str = r#"model_provider = "local-test"

[model_providers.local-test]
name = "local-test"
base_url = "http://127.0.0.1:8061/api/codex"
wire_api = "responses"
requires_openai_auth = false
"#;

#[cfg(test)]
fn with_test_model_provider_if_absent(mut cfg: ConfigToml) -> ConfigToml {
    if cfg.model_provider.is_some() || !cfg.model_providers.is_empty() {
        return cfg;
    }

    let provider_cfg: ConfigToml =
        toml::from_str(TEST_MODEL_PROVIDER_CONFIG).expect("valid test provider config");
    cfg.model_provider = provider_cfg.model_provider;
    cfg.model_providers = provider_cfg.model_providers;
    cfg
}

#[derive(Debug, Clone, PartialEq)]
pub struct Permissions {
    pub approval_policy: Constrained<AskForApproval>,
    permission_profile_state: PermissionProfileState,
    workspace_roots: Vec<AbsolutePathBuf>,
    pub network: Option<NetworkProxySpec>,
    pub allow_login_shell: bool,
    pub shell_environment_policy: ShellEnvironmentPolicy,
    pub windows_sandbox_mode: Option<WindowsSandboxModeToml>,
    pub windows_sandbox_private_desktop: bool,
}

impl Permissions {
    pub fn from_approval_and_profile(
        approval_policy: Constrained<AskForApproval>,
        permission_profile: Constrained<PermissionProfile>,
    ) -> ConstraintResult<Self> {
        Ok(Self {
            approval_policy,
            permission_profile_state: PermissionProfileState::from_constrained_legacy(
                permission_profile,
            )?,
            workspace_roots: Vec::new(),
            network: None,
            allow_login_shell: true,
            shell_environment_policy: ShellEnvironmentPolicy::default(),
            windows_sandbox_mode: None,
            windows_sandbox_private_desktop: true,
        })
    }

    pub fn set_permission_profile_from_session_snapshot(
        &mut self,
        snapshot: PermissionProfileSnapshot,
    ) -> ConstraintResult<()> {
        self.permission_profile_state
            .set_permission_profile_snapshot(snapshot)
    }

    pub fn replace_permission_profile_from_session_snapshot(
        &mut self,
        snapshot: PermissionProfileSnapshot,
    ) -> ConstraintResult<()> {
        let permission_profile = Constrained::allow_only(snapshot.permission_profile().clone());
        self.permission_profile_state = PermissionProfileState::from_constrained_resolved(
            permission_profile,
            snapshot.into_resolved_permission_profile(),
        )?;
        Ok(())
    }

    pub fn permission_profile(&self) -> &PermissionProfile {
        self.permission_profile_state.permission_profile()
    }

    pub fn can_set_permission_profile(
        &self,
        permission_profile: &PermissionProfile,
    ) -> ConstraintResult<()> {
        self.permission_profile_state
            .can_set_legacy_permission_profile(permission_profile)
    }

    pub fn set_workspace_roots(&mut self, workspace_roots: Vec<AbsolutePathBuf>) {
        self.workspace_roots = workspace_roots;
    }

    pub fn workspace_roots(&self) -> &[AbsolutePathBuf] {
        &self.workspace_roots
    }

    pub fn user_visible_workspace_roots(&self) -> &[AbsolutePathBuf] {
        &self.workspace_roots
    }

    pub fn profile_workspace_roots(&self) -> &[AbsolutePathBuf] {
        self.permission_profile_state.profile_workspace_roots()
    }

    fn materialized_permission_profile(&self) -> PermissionProfile {
        self.permission_profile()
            .clone()
            .materialize_project_roots_with_workspace_roots(&self.workspace_roots)
    }

    pub fn effective_permission_profile(&self) -> PermissionProfile {
        self.materialized_permission_profile()
    }

    pub fn active_permission_profile(&self) -> Option<ActivePermissionProfile> {
        self.permission_profile_state.active_permission_profile()
    }

    pub fn file_system_sandbox_policy(&self) -> FileSystemSandboxPolicy {
        self.materialized_permission_profile()
            .file_system_sandbox_policy()
    }

    pub fn network_sandbox_policy(&self) -> NetworkSandboxPolicy {
        self.permission_profile().network_sandbox_policy()
    }

    pub fn can_set_legacy_sandbox_policy(
        &self,
        sandbox_policy: &SandboxPolicy,
        cwd: &Path,
    ) -> ConstraintResult<()> {
        let file_system_sandbox_policy =
            FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(sandbox_policy, cwd);
        let network_sandbox_policy = NetworkSandboxPolicy::from(sandbox_policy);
        let permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
            SandboxEnforcement::from_legacy_sandbox_policy(sandbox_policy),
            &file_system_sandbox_policy,
            network_sandbox_policy,
        );
        self.permission_profile_state
            .can_set_legacy_permission_profile(&permission_profile)
    }

    pub fn set_legacy_sandbox_policy(
        &mut self,
        sandbox_policy: SandboxPolicy,
        cwd: &Path,
    ) -> ConstraintResult<()> {
        self.can_set_legacy_sandbox_policy(&sandbox_policy, cwd)?;
        let file_system_sandbox_policy =
            FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(&sandbox_policy, cwd);
        let network_sandbox_policy = NetworkSandboxPolicy::from(&sandbox_policy);
        let permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
            SandboxEnforcement::from_legacy_sandbox_policy(&sandbox_policy),
            &file_system_sandbox_policy,
            network_sandbox_policy,
        );
        self.workspace_roots = match &sandbox_policy {
            SandboxPolicy::WorkspaceWrite { writable_roots, .. } => {
                let mut workspace_roots = vec![
                    AbsolutePathBuf::from_absolute_path(cwd)
                        .unwrap_or_else(|_| AbsolutePathBuf::resolve_path_against_base(cwd, "/")),
                ];
                for root in writable_roots {
                    if !workspace_roots.iter().any(|existing| existing == root) {
                        workspace_roots.push(root.clone());
                    }
                }
                workspace_roots
            }
            SandboxPolicy::DangerFullAccess
            | SandboxPolicy::ExternalSandbox { .. }
            | SandboxPolicy::ReadOnly { .. } => vec![
                AbsolutePathBuf::from_absolute_path(cwd)
                    .unwrap_or_else(|_| AbsolutePathBuf::resolve_path_against_base(cwd, "/")),
            ],
        };

        self.permission_profile_state
            .set_legacy_permission_profile(permission_profile)?;
        Ok(())
    }

    pub fn set_permission_profile(
        &mut self,
        permission_profile: PermissionProfile,
    ) -> ConstraintResult<()> {
        self.permission_profile_state
            .set_legacy_permission_profile(permission_profile)
    }
}

fn profile_allows_configured_network_proxy(permission_profile: &PermissionProfile) -> bool {
    match permission_profile {
        PermissionProfile::Managed { network, .. } | PermissionProfile::External { network } => {
            network.is_enabled()
        }
        PermissionProfile::Disabled => false,
    }
}

fn build_network_proxy_spec(
    configured_network_proxy_config: NetworkProxyConfig,
    network_requirements: Option<Sourced<octopus_config::NetworkConstraints>>,
    permission_profile: &PermissionProfile,
) -> std::io::Result<Option<NetworkProxySpec>> {
    let (network_requirements, network_requirements_source) = match network_requirements {
        Some(Sourced { value, source }) => (Some(value), Some(source)),
        None => (None, None),
    };
    let has_network_requirements = network_requirements.is_some();
    let network = NetworkProxySpec::from_config_and_constraints(
        configured_network_proxy_config,
        network_requirements,
        permission_profile,
    )
    .map_err(|err| {
        if let Some(source) = network_requirements_source.as_ref() {
            std::io::Error::new(
                err.kind(),
                format!("failed to build managed network proxy from {source}: {err}"),
            )
        } else {
            err
        }
    })?;

    Ok(if has_network_requirements {
        Some(network)
    } else {
        network.enabled().then_some(network)
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub config_layer_stack: ConfigLayerStack,

    pub startup_warnings: Vec<String>,

    pub model: Option<String>,

    pub service_tier: Option<String>,

    pub review_model: Option<String>,

    pub model_context_window: Option<i64>,

    pub model_auto_compact_token_limit: Option<i64>,

    pub model_auto_compact_token_limit_scope: AutoCompactTokenLimitScope,

    pub model_provider_id: String,

    pub model_provider: ModelProviderInfo,

    pub personality: Option<Personality>,

    pub permissions: Permissions,

    pub explicit_permission_profile_mode: bool,

    pub custom_permission_profiles: Vec<CustomPermissionProfileSummary>,

    pub approvals_reviewer: ApprovalsReviewer,

    pub enforce_residency: Constrained<Option<ResidencyRequirement>>,

    pub hide_agent_reasoning: bool,

    pub show_raw_agent_reasoning: bool,

    pub base_instructions: Option<String>,

    pub developer_instructions: Option<String>,

    pub guardian_policy_config: Option<String>,

    pub include_permissions_instructions: bool,

    pub include_apps_instructions: bool,

    pub include_collaboration_mode_instructions: bool,

    pub include_skill_instructions: bool,

    pub orchestrator_skills_enabled: bool,

    pub orchestrator_mcp_enabled: bool,

    pub include_environment_context: bool,

    pub compact_prompt: Option<String>,

    pub notify: Option<Vec<String>>,

    pub cwd: AbsolutePathBuf,

    pub workspace_roots: Vec<AbsolutePathBuf>,
    pub workspace_roots_explicit: bool,

    pub cli_auth_credentials_store_mode: AuthCredentialsStoreMode,

    pub mcp_servers: Constrained<HashMap<String, McpServerConfig>>,

    pub mcp_oauth_credentials_store_mode: OAuthCredentialsStoreMode,

    pub mcp_oauth_callback_port: Option<u16>,

    pub mcp_oauth_callback_url: Option<String>,

    pub model_providers: HashMap<String, ModelProviderInfo>,

    pub project_doc_max_bytes: usize,

    pub project_doc_fallback_filenames: Vec<String>,

    pub tool_output_token_limit: Option<usize>,

    pub agent_max_threads: Option<usize>,
    pub agent_job_max_runtime_seconds: Option<u64>,

    pub agent_interrupt_message_enabled: bool,

    pub agent_max_depth: i32,

    pub agent_roles: BTreeMap<String, AgentRoleConfig>,

    pub memories: MemoriesConfig,

    pub codex_home: AbsolutePathBuf,

    pub log_dir: PathBuf,

    pub config_lock_export_dir: Option<AbsolutePathBuf>,

    pub config_lock_allow_codex_version_mismatch: bool,

    pub config_lock_save_fields_resolved_from_model_catalog: bool,

    pub config_lock_toml: Option<Arc<ConfigLockfileToml>>,

    pub ephemeral: bool,

    pub bypass_hook_trust: bool,

    pub file_opener: UriBasedFileOpener,

    pub codex_self_exe: Option<PathBuf>,

    pub codex_linux_sandbox_exe: Option<PathBuf>,

    pub main_execve_wrapper_exe: Option<PathBuf>,

    pub zsh_path: Option<PathBuf>,

    pub model_reasoning_effort: Option<ReasoningEffort>,
    pub plan_mode_reasoning_effort: Option<ReasoningEffort>,

    pub model_reasoning_summary: Option<ReasoningSummary>,

    pub model_supports_reasoning_summaries: Option<bool>,

    pub model_catalog: Option<ModelsResponse>,

    pub model_verbosity: Option<Verbosity>,

    pub chatgpt_base_url: String,

    pub respect_system_proxy: bool,

    pub apps_mcp_product_sku: Option<String>,

    pub realtime_audio: RealtimeAudioConfig,

    pub experimental_realtime_ws_base_url: Option<String>,
    pub experimental_realtime_webrtc_call_base_url: Option<String>,
    pub experimental_realtime_ws_model: Option<String>,
    pub realtime: RealtimeConfig,
    pub experimental_realtime_ws_backend_prompt: Option<String>,
    pub experimental_realtime_ws_startup_context: Option<String>,
    pub experimental_realtime_start_instructions: Option<String>,

    pub forced_chatgpt_workspace_id: Option<Vec<String>>,

    pub forced_login_method: Option<ForcedLoginMethod>,

    pub web_search_mode: Constrained<WebSearchMode>,

    pub web_search_config: Option<WebSearchConfig>,

    pub experimental_request_user_input_enabled: bool,

    pub code_mode: CodeModeConfig,

    pub use_experimental_unified_exec_tool: bool,

    pub background_terminal_max_timeout: u64,

    pub ghost_snapshot: GhostSnapshotConfig,

    pub multi_agent_v2: MultiAgentV2Config,

    pub rollout_budget: Option<RolloutBudgetConfig>,
    pub current_time_reminder: Option<CurrentTimeReminderConfig>,

    pub features: ManagedFeatures,

    pub suppress_unstable_features_warning: bool,

    pub active_project: ProjectConfig,

    pub notices: Notice,

    pub check_for_update_on_startup: bool,

    pub disable_paste_burst: bool,

    pub analytics_enabled: Option<bool>,

    pub tool_suggest: ToolSuggestConfig,

    pub otel: octopus_config::types::OtelConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CodeModeConfig {
    pub excluded_tool_namespaces: Vec<String>,
    pub direct_only_tool_namespaces: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RolloutBudgetConfig {
    pub limit_tokens: i64,
    pub reminder_interval_tokens: i64,
    pub sampling_token_weight: f64,
    pub prefill_token_weight: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CurrentTimeReminderConfig {
    pub reminder_interval_model_requests: u64,
    pub clock_source: CurrentTimeSource,
}

impl Default for CurrentTimeReminderConfig {
    fn default() -> Self {
        Self {
            reminder_interval_model_requests: 1,
            clock_source: CurrentTimeSource::System,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MultiAgentV2Config {
    pub max_concurrent_threads_per_session: usize,
    pub min_wait_timeout_ms: i64,
    pub max_wait_timeout_ms: i64,
    pub default_wait_timeout_ms: i64,
    pub usage_hint_enabled: bool,
    pub usage_hint_text: Option<String>,
    pub root_agent_usage_hint_text: Option<String>,
    pub subagent_usage_hint_text: Option<String>,
    pub tool_namespace: Option<String>,
    pub hide_spawn_agent_metadata: bool,
    pub non_code_mode_only: bool,
}

impl MultiAgentV2Config {
    fn defaults_for_max_concurrency(max_concurrent_threads_per_session: usize) -> Self {
        Self {
            max_concurrent_threads_per_session,
            min_wait_timeout_ms: DEFAULT_MULTI_AGENT_V2_MIN_WAIT_TIMEOUT_MS,
            max_wait_timeout_ms: DEFAULT_MULTI_AGENT_V2_MAX_WAIT_TIMEOUT_MS,
            default_wait_timeout_ms: DEFAULT_MULTI_AGENT_V2_DEFAULT_WAIT_TIMEOUT_MS,
            usage_hint_enabled: true,
            usage_hint_text: None,
            root_agent_usage_hint_text: Some(default_multi_agent_v2_usage_hint_text(
                DEFAULT_MULTI_AGENT_V2_ROOT_AGENT_USAGE_HINT_TEXT,
                max_concurrent_threads_per_session,
            )),
            subagent_usage_hint_text: Some(default_multi_agent_v2_usage_hint_text(
                DEFAULT_MULTI_AGENT_V2_SUBAGENT_USAGE_HINT_TEXT,
                max_concurrent_threads_per_session,
            )),
            tool_namespace: None,
            hide_spawn_agent_metadata: true,
            non_code_mode_only: true,
        }
    }
}

impl Default for MultiAgentV2Config {
    fn default() -> Self {
        Self::defaults_for_max_concurrency(
            DEFAULT_MULTI_AGENT_V2_MAX_CONCURRENT_THREADS_PER_SESSION,
        )
    }
}

impl AuthManagerConfig for Config {
    fn codex_home(&self) -> PathBuf {
        self.codex_home.to_path_buf()
    }

    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode {
        self.cli_auth_credentials_store_mode
    }

    fn auth_keyring_backend_kind(&self) -> AuthKeyringBackendKind {
        Config::auth_keyring_backend_kind(self)
    }

    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        self.forced_chatgpt_workspace_id.clone()
    }

    fn chatgpt_base_url(&self) -> String {
        self.chatgpt_base_url.clone()
    }
}

#[derive(Clone, Default)]
pub struct ConfigBuilder {
    codex_home: Option<PathBuf>,
    cli_overrides: Option<Vec<(String, TomlValue)>>,
    harness_overrides: Option<ConfigOverrides>,
    loader_overrides: Option<LoaderOverrides>,
    strict_config: bool,
    cloud_config_bundle: CloudConfigBundleLoader,
    fallback_cwd: Option<PathBuf>,
}

impl ConfigBuilder {
    pub fn codex_home(mut self, codex_home: PathBuf) -> Self {
        self.codex_home = Some(codex_home);
        self
    }

    pub fn cli_overrides(mut self, cli_overrides: Vec<(String, TomlValue)>) -> Self {
        self.cli_overrides = Some(cli_overrides);
        self
    }

    pub fn harness_overrides(mut self, harness_overrides: ConfigOverrides) -> Self {
        self.harness_overrides = Some(harness_overrides);
        self
    }

    pub fn loader_overrides(mut self, loader_overrides: LoaderOverrides) -> Self {
        self.loader_overrides = Some(loader_overrides);
        self
    }

    pub fn strict_config(mut self, strict_config: bool) -> Self {
        self.strict_config = strict_config;
        self
    }

    pub fn cloud_config_bundle(mut self, cloud_config_bundle: CloudConfigBundleLoader) -> Self {
        self.cloud_config_bundle = cloud_config_bundle;
        self
    }

    pub fn fallback_cwd(mut self, fallback_cwd: Option<PathBuf>) -> Self {
        self.fallback_cwd = fallback_cwd;
        self
    }

    pub async fn build(self) -> std::io::Result<Config> {
        Box::pin(self.build_inner()).await
    }

    async fn build_inner(self) -> std::io::Result<Config> {
        let Self {
            codex_home,
            cli_overrides,
            harness_overrides,
            loader_overrides,
            strict_config,
            cloud_config_bundle,
            fallback_cwd,
        } = self;
        let codex_home = match codex_home {
            Some(codex_home) => AbsolutePathBuf::from_absolute_path(codex_home)?,
            None => find_codex_home()?,
        };
        let cli_overrides = cli_overrides.unwrap_or_default();
        let mut harness_overrides = harness_overrides.unwrap_or_default();
        let loader_overrides = loader_overrides.unwrap_or_default();
        let cwd_override = harness_overrides.cwd.as_deref().or(fallback_cwd.as_deref());
        let cwd = match cwd_override {
            Some(path) => AbsolutePathBuf::relative_to_current_dir(path)?,
            None => AbsolutePathBuf::current_dir()?,
        };
        harness_overrides.cwd = Some(cwd.to_path_buf());
        let config_layer_stack = load_config_layers_state(
            LOCAL_FS.as_ref(),
            &codex_home,
            Some(cwd),
            &cli_overrides,
            ConfigLoadOptions {
                loader_overrides,
                strict_config,
                cloud_config_bundle,
            },
        )
        .await?;
        let merged_toml = config_layer_stack.effective_config();

        let config_toml: ConfigToml = match merged_toml.try_into() {
            Ok(config_toml) => config_toml,
            Err(err) => {
                if let Some(config_error) = octopus_config::first_layer_config_error::<ConfigToml>(
                    &config_layer_stack,
                    octopus_config::CONFIG_TOML_FILE,
                )
                .await
                {
                    return Err(octopus_config::io_error_from_config_error(
                        std::io::ErrorKind::InvalidData,
                        config_error,
                        Some(err),
                    ));
                }
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, err));
            }
        };
        #[cfg(test)]
        let config_toml = with_test_model_provider_if_absent(config_toml);
        let config_lock_settings = config_toml
            .debug
            .as_ref()
            .and_then(|debug| debug.config_lockfile.as_ref());
        if let Some(config_lock_load_path) =
            config_lock_settings.and_then(|config_lock| config_lock.load_path.as_ref())
        {
            let allow_codex_version_mismatch = config_lock_settings
                .and_then(|config_lock| config_lock.allow_codex_version_mismatch)
                .unwrap_or(false);
            let save_fields_resolved_from_model_catalog = config_lock_settings
                .and_then(|config_lock| config_lock.save_fields_resolved_from_model_catalog)
                .unwrap_or(true);
            let lockfile_toml = read_config_lock_from_path(config_lock_load_path).await?;
            let expected_lock_config = lockfile_toml.clone();
            let lock_layer = lock_layer_from_config(config_lock_load_path, &lockfile_toml)?;
            let lock_config_toml = config_without_lock_controls(&lockfile_toml.config);
            #[cfg(test)]
            let lock_config_toml = with_test_model_provider_if_absent(lock_config_toml);
            let lock_config_layer_stack = ConfigLayerStack::new(
                vec![lock_layer],
                config_layer_stack.requirements().clone(),
                config_layer_stack.requirements_toml().clone(),
            )?;
            let mut config = Config::load_config_with_layer_stack(
                LOCAL_FS.as_ref(),
                lock_config_toml,
                harness_overrides,
                codex_home,
                lock_config_layer_stack,
            )
            .await?;
            config.config_lock_toml = Some(Arc::new(expected_lock_config));
            config.config_lock_allow_codex_version_mismatch = allow_codex_version_mismatch;
            config.config_lock_save_fields_resolved_from_model_catalog =
                save_fields_resolved_from_model_catalog;
            return Ok(config);
        }
        Config::load_config_with_layer_stack(
            LOCAL_FS.as_ref(),
            config_toml,
            harness_overrides,
            codex_home,
            config_layer_stack,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) fn without_managed_config_for_tests() -> Self {
        Self::default().loader_overrides(LoaderOverrides::without_managed_config_for_tests())
    }
}

impl Config {
    #[cfg(test)]
    pub(crate) fn validate_multi_agent_v2_config(&self) -> std::io::Result<()> {
        if self.features.enabled(Feature::MultiAgentV2) && self.agent_max_threads.is_some() {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agents.max_threads cannot be set when features.multi_agent_v2 is enabled",
            ))
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    pub(crate) fn effective_agent_max_threads(
        &self,
        multi_agent_version: MultiAgentVersion,
    ) -> Option<usize> {
        match multi_agent_version {
            MultiAgentVersion::V2 => Some(
                self.multi_agent_v2
                    .max_concurrent_threads_per_session
                    .saturating_sub(1),
            ),
            MultiAgentVersion::Disabled | MultiAgentVersion::V1 => {
                self.agent_max_threads.or(DEFAULT_AGENT_MAX_THREADS)
            }
        }
    }

    pub fn set_legacy_sandbox_policy(
        &mut self,
        sandbox_policy: SandboxPolicy,
    ) -> ConstraintResult<()> {
        self.workspace_roots_explicit = matches!(
            &sandbox_policy,
            SandboxPolicy::WorkspaceWrite { writable_roots, .. } if !writable_roots.is_empty()
        );
        self.permissions
            .set_legacy_sandbox_policy(sandbox_policy, self.cwd.as_path())?;
        self.workspace_roots = self.permissions.workspace_roots().to_vec();
        Ok(())
    }

    pub fn effective_workspace_roots(&self) -> Vec<AbsolutePathBuf> {
        let mut workspace_roots = self.workspace_roots.clone();
        workspace_roots.extend(self.permissions.profile_workspace_roots().iter().cloned());
        dedupe_absolute_paths(&mut workspace_roots);
        workspace_roots
    }

    pub async fn rebuild_preserving_session_layers(
        &self,
        refreshed_config: &Config,
    ) -> std::io::Result<Self> {
        let mut layers = refreshed_config
            .config_layer_stack
            .get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, true)
            .into_iter()
            .filter(|layer| !is_session_layer(&layer.name))
            .cloned()
            .collect::<Vec<_>>();
        layers.extend(
            self.config_layer_stack
                .get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, true)
                .into_iter()
                .filter(|layer| is_session_layer(&layer.name))
                .cloned(),
        );
        layers.sort_by_key(|layer| layer.name.precedence());

        let config_layer_stack = ConfigLayerStack::new(
            layers,
            refreshed_config.config_layer_stack.requirements().clone(),
            refreshed_config
                .config_layer_stack
                .requirements_toml()
                .clone(),
        )?
        .with_user_and_project_exec_policy_rules_ignored(
            refreshed_config
                .config_layer_stack
                .ignore_user_and_project_exec_policy_rules(),
        );
        let cfg: ConfigToml = config_layer_stack
            .effective_config()
            .try_into()
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        let default_zsh_path = refreshed_config
            .zsh_path
            .clone()
            .map(AbsolutePathBuf::try_from)
            .transpose()?;

        Self::load_config_with_layer_stack(
            LOCAL_FS.as_ref(),
            cfg,
            ConfigOverrides {
                cwd: Some(self.cwd.to_path_buf()),
                default_zsh_path,
                ..Default::default()
            },
            refreshed_config.codex_home.clone(),
            config_layer_stack,
        )
        .await
    }

    pub async fn load_with_cli_overrides(
        cli_overrides: Vec<(String, TomlValue)>,
    ) -> std::io::Result<Self> {
        ConfigBuilder::default()
            .cli_overrides(cli_overrides)
            .build()
            .await
    }

    pub async fn load_default_with_cli_overrides(
        cli_overrides: Vec<(String, TomlValue)>,
    ) -> std::io::Result<Self> {
        let codex_home = find_codex_home()?;
        Self::load_default_with_cli_overrides_for_codex_home(
            codex_home.to_path_buf(),
            cli_overrides,
        )
        .await
    }

    pub async fn load_default_with_cli_overrides_for_codex_home(
        codex_home: PathBuf,
        cli_overrides: Vec<(String, TomlValue)>,
    ) -> std::io::Result<Self> {
        let mut merged = toml::Value::try_from(ConfigToml::default()).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to serialize default config: {e}"),
            )
        })?;
        let cli_layer = octopus_config::build_cli_overrides_layer(&cli_overrides);
        octopus_config::merge_toml_values(&mut merged, &cli_layer);
        let codex_home = AbsolutePathBuf::from_absolute_path_checked(codex_home)?;
        let config_toml = deserialize_config_toml_with_base(merged, &codex_home)?;
        Self::load_config_with_layer_stack(
            LOCAL_FS.as_ref(),
            config_toml,
            ConfigOverrides::default(),
            codex_home,
            ConfigLayerStack::default(),
        )
        .await
    }
    pub async fn load_with_cli_overrides_and_harness_overrides(
        cli_overrides: Vec<(String, TomlValue)>,
        harness_overrides: ConfigOverrides,
    ) -> std::io::Result<Self> {
        ConfigBuilder::default()
            .cli_overrides(cli_overrides)
            .harness_overrides(harness_overrides)
            .build()
            .await
    }
}

pub fn resolve_profile_v2_config_path(
    codex_home: &Path,
    profile_name: &ProfileV2Name,
) -> AbsolutePathBuf {
    AbsolutePathBuf::resolve_path_against_base(
        format!("{profile_name}{CONFIG_PROFILE_V2_SUFFIX}"),
        codex_home,
    )
}

pub async fn load_config_as_toml_with_cli_overrides(
    codex_home: &Path,
    cwd: Option<&AbsolutePathBuf>,
    cli_overrides: Vec<(String, TomlValue)>,
    loader_overrides: LoaderOverrides,
) -> std::io::Result<ConfigToml> {
    load_config_as_toml_with_cli_and_loader_overrides(
        codex_home,
        cwd,
        cli_overrides,
        loader_overrides,
    )
    .await
}

pub async fn load_config_as_toml_with_cli_and_loader_overrides(
    codex_home: &Path,
    cwd: Option<&AbsolutePathBuf>,
    cli_overrides: Vec<(String, TomlValue)>,
    loader_overrides: LoaderOverrides,
) -> std::io::Result<ConfigToml> {
    load_config_as_toml_with_cli_and_load_options(codex_home, cwd, cli_overrides, loader_overrides)
        .await
}

pub async fn load_config_as_toml_with_cli_and_load_options(
    codex_home: &Path,
    cwd: Option<&AbsolutePathBuf>,
    cli_overrides: Vec<(String, TomlValue)>,
    options: impl Into<ConfigLoadOptions>,
) -> std::io::Result<ConfigToml> {
    load_config_toml_with_layer_stack(codex_home, cwd, cli_overrides, options)
        .await
        .map(|result| result.config_toml)
}

pub struct ConfigTomlLoadResult {
    pub config_toml: ConfigToml,
    pub config_layer_stack: ConfigLayerStack,
}

pub async fn load_config_toml_with_layer_stack(
    codex_home: &Path,
    cwd: Option<&AbsolutePathBuf>,
    cli_overrides: Vec<(String, TomlValue)>,
    options: impl Into<ConfigLoadOptions>,
) -> std::io::Result<ConfigTomlLoadResult> {
    let config_layer_stack = load_config_layers_state(
        LOCAL_FS.as_ref(),
        codex_home,
        cwd.cloned(),
        &cli_overrides,
        options,
    )
    .await?;

    let merged_toml = config_layer_stack.effective_config();
    let cfg = deserialize_config_toml_with_base(merged_toml, codex_home).map_err(|e| {
        tracing::error!("Failed to deserialize overridden config: {e}");
        e
    })?;

    Ok(ConfigTomlLoadResult {
        config_toml: cfg,
        config_layer_stack,
    })
}

pub fn deserialize_config_toml_with_base(
    root_value: TomlValue,
    config_base_dir: &Path,
) -> std::io::Result<ConfigToml> {
    let _guard = AbsolutePathBufGuard::new(config_base_dir);
    root_value
        .try_into()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn validate_feature_requirements_for_config_toml(
    cfg: &ConfigToml,
    feature_requirements: Option<&Sourced<FeatureRequirementsToml>>,
) -> std::io::Result<()> {
    managed_features::validate_explicit_feature_settings_in_config_toml(cfg, feature_requirements)?;
    managed_features::validate_feature_requirements_in_config_toml(cfg, feature_requirements)
}

fn load_catalog_json(path: &AbsolutePathBuf) -> std::io::Result<ModelsResponse> {
    let file_contents = std::fs::read_to_string(path)?;
    let catalog = serde_json::from_str::<ModelsResponse>(&file_contents).map_err(|err| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "failed to parse model_catalog_json path `{}` as JSON: {err}",
                path.display()
            ),
        )
    })?;
    if catalog.models.is_empty() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "model_catalog_json path `{}` must contain at least one model",
                path.display()
            ),
        ));
    }
    Ok(catalog)
}

fn load_model_catalog(
    model_catalog_json: Option<AbsolutePathBuf>,
) -> std::io::Result<Option<ModelsResponse>> {
    model_catalog_json
        .map(|path| load_catalog_json(&path))
        .transpose()
}

fn filter_mcp_servers_by_requirements(
    mcp_servers: &mut HashMap<String, McpServerConfig>,
    mcp_requirements: Option<&Sourced<BTreeMap<String, McpServerRequirement>>>,
) {
    let Some(allowlist) = mcp_requirements else {
        return;
    };

    let source = allowlist.source.clone();
    for (name, server) in mcp_servers.iter_mut() {
        let allowed = allowlist
            .value
            .get(name)
            .is_some_and(|requirement| mcp_server_matches_requirement(requirement, server));
        if allowed {
            server.disabled_reason = None;
        } else {
            server.enabled = false;
            server.disabled_reason = Some(McpServerDisabledReason::Requirements {
                source: source.clone(),
            });
        }
    }
}

fn constrain_mcp_servers(
    mcp_servers: HashMap<String, McpServerConfig>,
    mcp_requirements: Option<&Sourced<BTreeMap<String, McpServerRequirement>>>,
) -> ConstraintResult<Constrained<HashMap<String, McpServerConfig>>> {
    if mcp_requirements.is_none() {
        return Ok(Constrained::allow_any(mcp_servers));
    }

    let mcp_requirements = mcp_requirements.cloned();
    Constrained::normalized(mcp_servers, move |mut servers| {
        filter_mcp_servers_by_requirements(&mut servers, mcp_requirements.as_ref());
        servers
    })
}

fn apply_requirement_constrained_value<T>(
    field_name: &'static str,
    configured_value: T,
    constrained_value: &mut ConstrainedWithSource<T>,
    startup_warnings: &mut Vec<String>,
) -> std::io::Result<bool>
where
    T: Clone + std::fmt::Debug + Send + Sync,
{
    if let Err(err) = constrained_value.set(configured_value) {
        let fallback_value = constrained_value.get().clone();
        tracing::warn!(
            error = %err,
            ?fallback_value,
            requirement_source = ?constrained_value.source,
            "configured value is disallowed by requirements; falling back to required value for {field_name}"
        );
        let message = format!(
            "Configured value for `{field_name}` is disallowed by requirements; falling back to required value {fallback_value:?}. Details: {err}"
        );
        startup_warnings.push(message);

        constrained_value.set(fallback_value).map_err(|fallback_err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "configured value for `{field_name}` is disallowed by requirements ({err}); fallback to a requirement-compliant value also failed ({fallback_err})"
                ),
            )
        })?;
        return Ok(true);
    }

    Ok(false)
}

fn mcp_server_matches_requirement(
    requirement: &McpServerRequirement,
    server: &McpServerConfig,
) -> bool {
    match &requirement.identity {
        McpServerIdentity::Command {
            command: want_command,
        } => matches!(
            &server.transport,
            McpServerTransportConfig::Stdio { command: got_command, .. }
                if got_command == want_command
        ),
        McpServerIdentity::Url { url: want_url } => matches!(
            &server.transport,
            McpServerTransportConfig::StreamableHttp { url: got_url, .. }
                if got_url == want_url
        ),
    }
}

pub async fn load_global_mcp_servers(
    codex_home: &Path,
) -> std::io::Result<BTreeMap<String, McpServerConfig>> {
    let cli_overrides = Vec::<(String, TomlValue)>::new();
    let cwd: Option<AbsolutePathBuf> = None;
    let config_layer_stack = load_config_layers_state(
        LOCAL_FS.as_ref(),
        codex_home,
        cwd,
        &cli_overrides,
        LoaderOverrides::default(),
    )
    .await?;
    let merged_toml = config_layer_stack.effective_config();
    let Some(servers_value) = merged_toml.get("mcp_servers") else {
        return Ok(BTreeMap::new());
    };

    ensure_no_inline_bearer_tokens(servers_value)?;

    servers_value
        .clone()
        .try_into()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn ensure_no_inline_bearer_tokens(value: &TomlValue) -> std::io::Result<()> {
    let Some(servers_table) = value.as_table() else {
        return Ok(());
    };

    for (server_name, server_value) in servers_table {
        if let Some(server_table) = server_value.as_table()
            && server_table.contains_key("bearer_token")
        {
            let message = format!(
                "mcp_servers.{server_name} uses unsupported `bearer_token`; set `bearer_token_env_var`."
            );
            return Err(std::io::Error::new(ErrorKind::InvalidData, message));
        }
    }

    Ok(())
}

pub(crate) fn set_project_trust_level_inner(
    doc: &mut DocumentMut,
    project_path: &Path,
    trust_level: TrustLevel,
) -> anyhow::Result<()> {
    let project_key = project_trust_key(project_path);

    {
        let root = doc.as_table_mut();
        let existing_projects = root.get("projects").cloned();
        if existing_projects.as_ref().is_none_or(|i| !i.is_table()) {
            let mut projects_tbl = toml_edit::Table::new();
            projects_tbl.set_implicit(true);

            if let Some(inline_tbl) = existing_projects.as_ref().and_then(|i| i.as_inline_table()) {
                for (k, v) in inline_tbl.iter() {
                    if let Some(inner_tbl) = v.as_inline_table() {
                        let new_tbl = inner_tbl.clone().into_table();
                        projects_tbl.insert(k, toml_edit::Item::Table(new_tbl));
                    }
                }
            }

            root.insert("projects", toml_edit::Item::Table(projects_tbl));
        }
    }
    let Some(projects_tbl) = doc["projects"].as_table_mut() else {
        return Err(anyhow::anyhow!(
            "projects table missing after initialization"
        ));
    };

    let needs_proj_table = !projects_tbl.contains_key(project_key.as_str())
        || projects_tbl
            .get(project_key.as_str())
            .and_then(|i| i.as_table())
            .is_none();
    if needs_proj_table {
        projects_tbl.insert(project_key.as_str(), toml_edit::table());
    }
    let Some(proj_tbl) = projects_tbl
        .get_mut(project_key.as_str())
        .and_then(|i| i.as_table_mut())
    else {
        return Err(anyhow::anyhow!("project table missing for {project_key}"));
    };
    proj_tbl.set_implicit(false);
    proj_tbl["trust_level"] = toml_edit::value(trust_level.to_string());
    Ok(())
}

pub fn set_project_trust_level(
    codex_home: &Path,
    project_path: &Path,
    trust_level: TrustLevel,
) -> anyhow::Result<()> {
    use crate::config::edit::ConfigEditsBuilder;

    ConfigEditsBuilder::new(codex_home)
        .set_project_trust_level(project_path, trust_level)
        .apply_blocking()
}

pub fn set_default_oss_provider(codex_home: &Path, provider: &str) -> std::io::Result<()> {
    octopus_config::config_toml::validate_oss_provider(provider)?;
    use toml_edit::value;

    let edits = [ConfigEdit::SetPath {
        segments: vec!["oss_provider".to_string()],
        value: value(provider),
    }];

    ConfigEditsBuilder::new(codex_home)
        .with_edits(edits)
        .apply_blocking()
        .map_err(|err| std::io::Error::other(format!("failed to persist config.toml: {err}")))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentRoleConfig {
    pub description: Option<String>,
    pub config_file: Option<PathBuf>,
    pub nickname_candidates: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomPermissionProfileSummary {
    pub id: String,
    pub description: Option<String>,
}

fn resolve_tool_suggest_config(
    config_toml: &ConfigToml,
    config_layer_stack: &ConfigLayerStack,
) -> ToolSuggestConfig {
    resolve_tool_suggest_config_from_config(config_toml.tool_suggest.as_ref(), config_layer_stack)
}

fn resolve_tool_suggest_config_from_config(
    tool_suggest: Option<&ToolSuggestConfig>,
    config_layer_stack: &ConfigLayerStack,
) -> ToolSuggestConfig {
    let discoverables = tool_suggest
        .into_iter()
        .flat_map(|tool_suggest| tool_suggest.discoverables.iter())
        .filter_map(|discoverable| {
            let trimmed = discoverable.id.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(ToolSuggestDiscoverable {
                    kind: discoverable.kind,
                    id: trimmed.to_string(),
                })
            }
        })
        .collect();
    let mut seen_disabled_tools = HashSet::new();
    let mut disabled_tools = Vec::new();
    let mut add_disabled_tool = |disabled_tool: ToolSuggestDisabledTool| {
        if let Some(disabled_tool) = disabled_tool.normalized()
            && seen_disabled_tools.insert(disabled_tool.clone())
        {
            disabled_tools.push(disabled_tool);
        }
    };

    let layers =
        config_layer_stack.get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, false);
    if layers.is_empty() {
        for disabled_tool in tool_suggest
            .into_iter()
            .flat_map(|tool_suggest| tool_suggest.disabled_tools.iter().cloned())
        {
            add_disabled_tool(disabled_tool);
        }
    } else {
        for layer in layers {
            let Some(tool_suggest) = layer
                .config
                .get("tool_suggest")
                .cloned()
                .and_then(|value| value.try_into::<ToolSuggestConfig>().ok())
            else {
                continue;
            };
            for disabled_tool in tool_suggest.disabled_tools {
                add_disabled_tool(disabled_tool);
            }
        }
    }

    ToolSuggestConfig {
        discoverables,
        disabled_tools,
    }
}

fn is_session_layer(source: &ConfigLayerSource) -> bool {
    matches!(source, ConfigLayerSource::SessionFlags)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PermissionConfigSyntax {
    Legacy,
    Profiles,
}

#[derive(Debug, Deserialize, Default)]
struct PermissionSelectionToml {
    default_permissions: Option<String>,
    sandbox_mode: Option<SandboxMode>,
}

#[derive(Debug)]
struct EffectivePermissionSelection<'a> {
    profiles: Option<PermissionsToml>,
    selected_profile_id: Option<&'a str>,
    requirements_force_profile_selection: bool,
}

impl EffectivePermissionSelection<'_> {
    fn has_profiles(&self) -> bool {
        self.profiles
            .as_ref()
            .is_some_and(|profiles| !profiles.is_empty())
    }

    fn profiles_are_active(
        &self,
        default_permissions_override: Option<&str>,
        permission_config_syntax: Option<PermissionConfigSyntax>,
    ) -> bool {
        self.requirements_force_profile_selection
            || default_permissions_override.is_some()
            || matches!(
                permission_config_syntax,
                Some(PermissionConfigSyntax::Profiles)
            )
            || permission_config_syntax.is_none()
    }
}

fn resolve_permission_config_syntax(
    config_layer_stack: &ConfigLayerStack,
    cfg: &ConfigToml,
    sandbox_mode_override: Option<SandboxMode>,
) -> Option<PermissionConfigSyntax> {
    if sandbox_mode_override.is_some() {
        return Some(PermissionConfigSyntax::Legacy);
    }

    let session_flags_select_profiles = config_layer_stack
        .get_layers(ConfigLayerStackOrdering::HighestPrecedenceFirst, false)
        .into_iter()
        .find(|layer| matches!(layer.name, ConfigLayerSource::SessionFlags))
        .and_then(|layer| {
            layer
                .config
                .clone()
                .try_into::<PermissionSelectionToml>()
                .ok()
        })
        .is_some_and(|selection| selection.default_permissions.is_some());
    if session_flags_select_profiles {
        return Some(PermissionConfigSyntax::Profiles);
    }

    let mut selection = None;
    for layer in
        config_layer_stack.get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, false)
    {
        let Ok(layer_selection) = layer.config.clone().try_into::<PermissionSelectionToml>() else {
            continue;
        };

        if layer_selection.sandbox_mode.is_some() {
            selection = Some(PermissionConfigSyntax::Legacy);
        }
        if layer_selection.default_permissions.is_some() {
            selection = Some(PermissionConfigSyntax::Profiles);
        }
    }

    selection.or_else(|| {
        if cfg.default_permissions.is_some() {
            Some(PermissionConfigSyntax::Profiles)
        } else if cfg.sandbox_mode.is_some() {
            Some(PermissionConfigSyntax::Legacy)
        } else {
            None
        }
    })
}

fn apply_managed_filesystem_constraints(
    file_system_sandbox_policy: &mut FileSystemSandboxPolicy,
    filesystem_constraints: &octopus_config::FilesystemConstraints,
) {
    for deny_read in &filesystem_constraints.deny_read {
        let deny_entry = if deny_read.contains_glob() {
            octopus_protocol::permissions::FileSystemSandboxEntry {
                path: octopus_protocol::permissions::FileSystemPath::GlobPattern {
                    pattern: deny_read.as_str().to_string(),
                },
                access: octopus_protocol::permissions::FileSystemAccessMode::Deny,
            }
        } else {
            let Ok(path) = AbsolutePathBuf::try_from(deny_read.as_str()) else {
                continue;
            };
            octopus_protocol::permissions::FileSystemSandboxEntry {
                path: octopus_protocol::permissions::FileSystemPath::Path { path },
                access: octopus_protocol::permissions::FileSystemAccessMode::Deny,
            }
        };
        if !file_system_sandbox_policy
            .entries
            .iter()
            .any(|existing| existing == &deny_entry)
        {
            file_system_sandbox_policy.entries.push(deny_entry);
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct ConfigOverrides {
    pub model: Option<String>,
    pub review_model: Option<String>,
    pub cwd: Option<PathBuf>,
    pub approval_policy: Option<AskForApproval>,
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    pub sandbox_mode: Option<SandboxMode>,
    pub permission_profile: Option<PermissionProfile>,
    pub default_permissions: Option<String>,
    pub model_provider: Option<String>,
    pub service_tier: Option<Option<String>>,
    pub codex_self_exe: Option<PathBuf>,
    pub codex_linux_sandbox_exe: Option<PathBuf>,
    pub main_execve_wrapper_exe: Option<PathBuf>,
    pub default_zsh_path: Option<AbsolutePathBuf>,
    pub base_instructions: Option<String>,
    pub developer_instructions: Option<String>,
    pub personality: Option<Personality>,
    pub compact_prompt: Option<String>,
    pub show_raw_agent_reasoning: Option<bool>,
    pub tools_web_search_request: Option<bool>,
    pub ephemeral: Option<bool>,
    pub bypass_hook_trust: Option<bool>,
    pub additional_writable_roots: Vec<PathBuf>,
    pub workspace_roots: Option<Vec<AbsolutePathBuf>>,
}

fn dedupe_absolute_paths(paths: &mut Vec<AbsolutePathBuf>) {
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
}

pub fn resolve_oss_provider(
    explicit_provider: Option<&str>,
    config_toml: &ConfigToml,
) -> Option<String> {
    if let Some(provider) = explicit_provider {
        Some(provider.to_string())
    } else {
        config_toml.oss_provider.clone()
    }
}

fn resolve_web_search_mode(config_toml: &ConfigToml, features: &Features) -> Option<WebSearchMode> {
    if let Some(mode) = config_toml.web_search {
        return Some(mode);
    }
    if features.enabled(Feature::WebSearchCached) {
        return Some(WebSearchMode::Cached);
    }
    if features.enabled(Feature::WebSearchRequest) {
        return Some(WebSearchMode::Live);
    }
    None
}

fn resolve_web_search_config(config_toml: &ConfigToml) -> Option<WebSearchConfig> {
    config_toml
        .tools
        .as_ref()
        .and_then(|tools| tools.web_search.as_ref())
        .cloned()
        .map(Into::into)
}

fn resolve_experimental_request_user_input_enabled(config_toml: &ConfigToml) -> bool {
    config_toml
        .tools
        .as_ref()
        .and_then(|tools| tools.experimental_request_user_input.as_ref())
        .is_none_or(|config| config.enabled)
}

fn resolve_orchestrator_feature_enabled(
    feature: Option<&octopus_config::config_toml::OrchestratorFeatureToml>,
) -> bool {
    feature.and_then(|feature| feature.enabled).unwrap_or(true)
}

fn resolve_code_mode_config(config_toml: &ConfigToml) -> CodeModeConfig {
    let base = code_mode_toml_config(config_toml.features.as_ref());

    CodeModeConfig {
        excluded_tool_namespaces: base
            .and_then(|config| config.excluded_tool_namespaces.as_ref())
            .cloned()
            .unwrap_or_default(),
        direct_only_tool_namespaces: base
            .and_then(|config| config.direct_only_tool_namespaces.as_ref())
            .cloned()
            .unwrap_or_default(),
    }
}

fn resolve_multi_agent_v2_config(config_toml: &ConfigToml) -> MultiAgentV2Config {
    let base = multi_agent_v2_toml_config(config_toml.features.as_ref());
    let max_concurrent_threads_per_session = base
        .and_then(|config| config.max_concurrent_threads_per_session)
        .unwrap_or(DEFAULT_MULTI_AGENT_V2_MAX_CONCURRENT_THREADS_PER_SESSION);
    let default =
        MultiAgentV2Config::defaults_for_max_concurrency(max_concurrent_threads_per_session);
    let min_wait_timeout_ms = base
        .and_then(|config| config.min_wait_timeout_ms)
        .unwrap_or(default.min_wait_timeout_ms);
    let max_wait_timeout_ms = base
        .and_then(|config| config.max_wait_timeout_ms)
        .unwrap_or(default.max_wait_timeout_ms);
    let default_wait_timeout_ms = base
        .and_then(|config| config.default_wait_timeout_ms)
        .unwrap_or(default.default_wait_timeout_ms);
    let usage_hint_enabled = base
        .and_then(|config| config.usage_hint_enabled)
        .unwrap_or(default.usage_hint_enabled);
    let usage_hint_text = base
        .and_then(|config| config.usage_hint_text.as_ref())
        .cloned()
        .or(default.usage_hint_text);
    let root_agent_usage_hint_text = resolve_optional_prompt_text(
        base.map(|config| &config.root_agent_usage_hint_text),
        default.root_agent_usage_hint_text,
    );
    let subagent_usage_hint_text = resolve_optional_prompt_text(
        base.map(|config| &config.subagent_usage_hint_text),
        default.subagent_usage_hint_text,
    );
    let tool_namespace = base
        .and_then(|config| config.tool_namespace.as_ref())
        .cloned()
        .or(default.tool_namespace);
    let hide_spawn_agent_metadata = base
        .and_then(|config| config.hide_spawn_agent_metadata)
        .unwrap_or(default.hide_spawn_agent_metadata);
    let non_code_mode_only = base
        .and_then(|config| config.non_code_mode_only)
        .unwrap_or(default.non_code_mode_only);

    MultiAgentV2Config {
        max_concurrent_threads_per_session,
        min_wait_timeout_ms,
        max_wait_timeout_ms,
        default_wait_timeout_ms,
        usage_hint_enabled,
        usage_hint_text,
        root_agent_usage_hint_text,
        subagent_usage_hint_text,
        tool_namespace,
        hide_spawn_agent_metadata,
        non_code_mode_only,
    }
}

fn resolve_rollout_budget_config(
    config_toml: &ConfigToml,
    features: &ManagedFeatures,
) -> std::io::Result<Option<RolloutBudgetConfig>> {
    if !features.enabled(Feature::RolloutBudget) {
        return Ok(None);
    }
    let missing_limit_error = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "features.rollout_budget.limit_tokens is required when rollout_budget is enabled",
        )
    };
    let Some(FeatureToml::Config(config)) = config_toml
        .features
        .as_ref()
        .and_then(|features| features.rollout_budget.as_ref())
    else {
        return Err(missing_limit_error());
    };
    let Some(limit_tokens) = config.limit_tokens else {
        return Err(missing_limit_error());
    };
    if limit_tokens <= 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "features.rollout_budget.limit_tokens must be positive",
        ));
    }
    let reminder_interval_tokens = config
        .reminder_interval_tokens
        .unwrap_or_else(|| (limit_tokens / 10).max(1));
    if reminder_interval_tokens <= 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "features.rollout_budget.reminder_interval_tokens must be positive",
        ));
    }
    let sampling_token_weight = config.sampling_token_weight.unwrap_or(1.0);
    let prefill_token_weight = config.prefill_token_weight.unwrap_or(1.0);
    for (field, weight) in [
        ("sampling_token_weight", sampling_token_weight),
        ("prefill_token_weight", prefill_token_weight),
    ] {
        if !weight.is_finite() || weight < 0.0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("features.rollout_budget.{field} must be finite and non-negative"),
            ));
        }
    }
    Ok(Some(RolloutBudgetConfig {
        limit_tokens,
        reminder_interval_tokens,
        sampling_token_weight,
        prefill_token_weight,
    }))
}

fn resolve_current_time_reminder_config(
    config_toml: &ConfigToml,
    features: &ManagedFeatures,
) -> std::io::Result<Option<CurrentTimeReminderConfig>> {
    if !features.enabled(Feature::CurrentTimeReminder) {
        return Ok(None);
    }

    let base = current_time_reminder_toml_config(config_toml.features.as_ref());
    let default = CurrentTimeReminderConfig::default();
    let reminder_interval_model_requests = base
        .and_then(|config| config.reminder_interval_model_requests)
        .unwrap_or(default.reminder_interval_model_requests);
    if reminder_interval_model_requests == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "features.current_time_reminder.reminder_interval_model_requests must be positive",
        ));
    }

    Ok(Some(CurrentTimeReminderConfig {
        reminder_interval_model_requests,
        clock_source: base
            .and_then(|config| config.clock_source)
            .unwrap_or(default.clock_source),
    }))
}

fn resolve_optional_prompt_text(
    configured: Option<&Option<String>>,
    default: Option<String>,
) -> Option<String> {
    match configured {
        Some(Some(value)) if value.is_empty() => None,
        Some(Some(value)) => Some(value.clone()),
        Some(None) | None => default,
    }
}

fn code_mode_toml_config(features: Option<&FeaturesToml>) -> Option<&CodeModeConfigToml> {
    match features?.code_mode.as_ref()? {
        FeatureToml::Enabled(_) => None,
        FeatureToml::Config(config) => Some(config),
    }
}

fn multi_agent_v2_toml_config(features: Option<&FeaturesToml>) -> Option<&MultiAgentV2ConfigToml> {
    match features?.multi_agent_v2.as_ref()? {
        FeatureToml::Enabled(_) => None,
        FeatureToml::Config(config) => Some(config),
    }
}

fn current_time_reminder_toml_config(
    features: Option<&FeaturesToml>,
) -> Option<&CurrentTimeReminderConfigToml> {
    match features?.current_time_reminder.as_ref()? {
        FeatureToml::Enabled(_) => None,
        FeatureToml::Config(config) => Some(config),
    }
}

fn network_proxy_toml_config(features: Option<&FeaturesToml>) -> Option<&NetworkProxyConfigToml> {
    match features?.network_proxy.as_ref()? {
        FeatureToml::Enabled(_) => None,
        FeatureToml::Config(config) => Some(config),
    }
}

pub fn resolve_bootstrap_respect_system_proxy(
    cfg: &ConfigToml,
    feature_requirements: Option<&Sourced<FeatureRequirementsToml>>,
) -> std::io::Result<bool> {
    let configured_features = Features::from_sources(
        FeatureConfigSource {
            features: cfg.features.as_ref(),
            experimental_use_unified_exec_tool: cfg.experimental_use_unified_exec_tool,
        },
        FeatureConfigSource::default(),
        FeatureOverrides::default(),
    );
    let features =
        ManagedFeatures::from_configured(configured_features, feature_requirements.cloned())?;
    Ok(features.get().enabled(Feature::RespectSystemProxy))
}

#[cfg(test)]
pub(crate) fn resolve_web_search_mode_for_turn(
    web_search_mode: &Constrained<WebSearchMode>,
    permission_profile: &PermissionProfile,
) -> WebSearchMode {
    let preferred = web_search_mode.value();

    if matches!(permission_profile, PermissionProfile::Disabled)
        && !matches!(preferred, WebSearchMode::Disabled | WebSearchMode::Indexed)
    {
        for mode in [
            WebSearchMode::Live,
            WebSearchMode::Cached,
            WebSearchMode::Disabled,
        ] {
            if web_search_mode.can_set(&mode).is_ok() {
                return mode;
            }
        }
    } else {
        if web_search_mode.can_set(&preferred).is_ok() {
            return preferred;
        }
        for mode in [
            WebSearchMode::Cached,
            WebSearchMode::Live,
            WebSearchMode::Disabled,
        ] {
            if web_search_mode.can_set(&mode).is_ok() {
                return mode;
            }
        }
    }

    WebSearchMode::Disabled
}

fn validate_multi_agent_v2_wait_timeout(label: &str, value: i64) -> std::io::Result<()> {
    if value < HARD_MIN_MULTI_AGENT_V2_TIMEOUT_MS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{label} must be at least {HARD_MIN_MULTI_AGENT_V2_TIMEOUT_MS}"),
        ));
    }
    if value > HARD_MAX_MULTI_AGENT_V2_TIMEOUT_MS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{label} must be at most {HARD_MAX_MULTI_AGENT_V2_TIMEOUT_MS}"),
        ));
    }
    Ok(())
}

fn validate_multi_agent_v2_tool_namespace(namespace: Option<&str>) -> std::io::Result<()> {
    const LABEL: &str = "features.multi_agent_v2.tool_namespace";
    const MAX_LEN: usize = 64;
    const RESERVED_RESPONSES_NAMESPACES: &[&str] = &[
        "api_tool",
        "browser",
        "computer",
        "container",
        "file_search",
        "functions",
        "image_gen",
        "multi_tool_use",
        "python",
        "python_user_visible",
        "submodel_delegator",
        "terminal",
        "tool_search",
        "web",
    ];

    let Some(namespace) = namespace else {
        return Ok(());
    };
    if namespace.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{LABEL} must not be empty"),
        ));
    }
    if namespace.trim() != namespace {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{LABEL} must not have leading or trailing whitespace"),
        ));
    }
    if !namespace
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{LABEL} must match ^[a-zA-Z0-9_-]+$"),
        ));
    }
    if namespace.chars().count() > MAX_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{LABEL} must be at most {MAX_LEN} characters"),
        ));
    }
    if namespace == "mcp"
        || namespace.starts_with("mcp__")
        || RESERVED_RESPONSES_NAMESPACES.contains(&namespace)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{LABEL} uses a reserved namespace: {namespace}"),
        ));
    }

    Ok(())
}

impl Config {
    #[cfg(test)]
    async fn load_from_base_config_with_overrides(
        cfg: ConfigToml,
        overrides: ConfigOverrides,
        codex_home: AbsolutePathBuf,
    ) -> std::io::Result<Self> {
        let cfg = with_test_model_provider_if_absent(cfg);
        let config_layer_stack = ConfigLayerStack::default();
        Self::load_config_with_layer_stack(
            LOCAL_FS.as_ref(),
            cfg,
            overrides,
            codex_home,
            config_layer_stack,
        )
        .await
    }

    pub(crate) async fn load_config_with_layer_stack(
        fs: &dyn ExecutorFileSystem,
        cfg: ConfigToml,
        overrides: ConfigOverrides,
        codex_home: AbsolutePathBuf,
        config_layer_stack: ConfigLayerStack,
    ) -> std::io::Result<Self> {
        Box::pin(async move {
        #[cfg(test)]
        let cfg = with_test_model_provider_if_absent(cfg);
        if cfg.sqlite_home.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`sqlite_home` is no longer supported; durable runtime truth is the rollout ledger",
            ));
        }
        if cfg.experimental_thread_store_endpoint.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`experimental_thread_store_endpoint` is no longer supported; remove it from config.toml",
            ));
        }
        if cfg.experimental_thread_store.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`experimental_thread_store` is no longer supported; thread persistence is rollout-ledger only",
            ));
        }

        validate_model_providers(&cfg.model_providers)
            .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
        let orchestrator = cfg.orchestrator.as_ref();
        let orchestrator_skills_enabled =
            resolve_orchestrator_feature_enabled(orchestrator.and_then(|value| value.skills.as_ref()));
        let orchestrator_mcp_enabled =
            resolve_orchestrator_feature_enabled(orchestrator.and_then(|value| value.mcp.as_ref()));
        let ConfigRequirements {
            approval_policy: mut constrained_approval_policy,
            approvals_reviewer: mut constrained_approvals_reviewer,
            permission_profile: mut constrained_permission_profile,
            windows_sandbox_mode: mut constrained_windows_sandbox_mode,
            web_search_mode: mut constrained_web_search_mode,
            allow_appshots: _,
            allow_remote_control: _,
            computer_use: _,
            feature_requirements,
            mcp_servers,
            exec_policy: _,
            enforce_residency,
            network: network_requirements,
            filesystem: filesystem_requirements,
            guardian_policy_config_source: _,
        } = config_layer_stack.requirements().clone();

        let mut startup_warnings = config_layer_stack
            .startup_warnings()
            .unwrap_or_default()
            .to_vec();

        let ConfigOverrides {
            model,
            review_model: override_review_model,
            cwd,
            approval_policy: approval_policy_override,
            approvals_reviewer: approvals_reviewer_override,
            sandbox_mode,
            permission_profile,
            default_permissions: default_permissions_override,
            model_provider,
            service_tier: service_tier_override,
            codex_self_exe,
            codex_linux_sandbox_exe,
            main_execve_wrapper_exe,
            default_zsh_path,
            base_instructions,
            developer_instructions,
            personality,
            compact_prompt,
            show_raw_agent_reasoning,
            tools_web_search_request: override_tools_web_search_request,
            ephemeral,
            bypass_hook_trust,
            additional_writable_roots,
            workspace_roots: workspace_roots_override,
        } = overrides;
        let bypass_hook_trust = bypass_hook_trust.unwrap_or_default();

        if bypass_hook_trust {
            startup_warnings.push(
                "`--dangerously-bypass-hook-trust` is enabled. Enabled hooks may run without review for this invocation."
                    .to_string(),
            );
        }

        if sandbox_mode.is_some() && permission_profile.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`sandbox_mode` and `permission_profile` overrides cannot both be set",
            ));
        }
        if sandbox_mode.is_some() && default_permissions_override.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`sandbox_mode` and `default_permissions` overrides cannot both be set",
            ));
        }
        if permission_profile.is_some() && default_permissions_override.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`permission_profile` and `default_permissions` overrides cannot both be set",
            ));
        }
        if let Some(profile) = cfg.profile.as_deref() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "legacy `profile = \"{profile}\"` config is no longer supported; use `--profile {profile}` with `{profile}.config.toml` instead"
                ),
            ));
        }

        let tool_suggest = resolve_tool_suggest_config(&cfg, &config_layer_stack);
        let feature_overrides = FeatureOverrides {
            web_search_request: override_tools_web_search_request,
        };

        let configured_features = Features::from_sources(
            FeatureConfigSource {
                features: cfg.features.as_ref(),
                experimental_use_unified_exec_tool: cfg.experimental_use_unified_exec_tool,
            },
            FeatureConfigSource {
                ..Default::default()
            },
            feature_overrides,
        );
        let features = ManagedFeatures::from_configured_with_warnings(
            configured_features,
            feature_requirements,
            &mut startup_warnings,
        )?;
        let respect_system_proxy = features.enabled(Feature::RespectSystemProxy);
        let enable_network_proxy = features.enabled(Feature::NetworkProxy);
        let configured_windows_sandbox_mode = resolve_windows_sandbox_mode(&cfg);
        let selected_windows_sandbox_mode = configured_windows_sandbox_mode.or_else(|| {
            match WindowsSandboxLevel::from_features(&features) {
                WindowsSandboxLevel::Elevated => Some(WindowsSandboxModeToml::Elevated),
                WindowsSandboxLevel::RestrictedToken => Some(WindowsSandboxModeToml::Unelevated),
                WindowsSandboxLevel::Disabled => None,
            }
        });
        apply_requirement_constrained_value(
            "windows.sandbox",
            selected_windows_sandbox_mode,
            &mut constrained_windows_sandbox_mode,
            &mut startup_warnings,
        )?;
        let effective_windows_sandbox_mode = *constrained_windows_sandbox_mode.get();
        let windows_sandbox_mode = if constrained_windows_sandbox_mode.source.is_some() {
            effective_windows_sandbox_mode
        } else {
            configured_windows_sandbox_mode
        };
        let windows_sandbox_private_desktop = resolve_windows_sandbox_private_desktop(&cfg);
        let resolved_cwd = AbsolutePathBuf::try_from(normalize_for_native_workdir({
            use std::env;

            match cwd {
                None => {
                    tracing::info!("cwd not set, using current dir");
                    env::current_dir()?
                }
                Some(p) if p.is_absolute() => p,
                Some(p) => {
                    tracing::info!("cwd is relative, resolving against current dir");
                    let mut current = env::current_dir()?;
                    current.push(p);
                    current
                }
            }
        }))?;
        let requested_additional_writable_roots: Vec<AbsolutePathBuf> = additional_writable_roots
            .into_iter()
            .map(|path| AbsolutePathBuf::resolve_path_against_base(path, resolved_cwd.as_path()))
            .collect();
        let repo_root = resolve_root_git_project_for_trust(fs, &resolved_cwd).await;
        let active_project = cfg
            .get_active_project(
                resolved_cwd.as_path(),
                repo_root.as_ref().map(AbsolutePathBuf::as_path),
            )
            .unwrap_or(ProjectConfig { trust_level: None });
        let permission_config_syntax = resolve_permission_config_syntax(
            &config_layer_stack,
            &cfg,
            sandbox_mode,
        );
        let requirements_toml = config_layer_stack.requirements_toml();
        let effective_permission_selection = resolve_effective_permission_selection(
            cfg.permissions.as_ref(),
            default_permissions_override.as_deref(),
            cfg.default_permissions.as_deref(),
            requirements_toml,
            &mut startup_warnings,
        )?;
        if effective_permission_selection.has_profiles()
            && !matches!(
                permission_config_syntax,
                Some(PermissionConfigSyntax::Legacy)
            )
            && effective_permission_selection.selected_profile_id.is_none()
            && !effective_permission_selection.requirements_force_profile_selection
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "config defines `[permissions]` profiles but does not set `default_permissions`",
            ));
        }

        let windows_sandbox_level = match effective_windows_sandbox_mode {
            Some(WindowsSandboxModeToml::Elevated) => WindowsSandboxLevel::Elevated,
            Some(WindowsSandboxModeToml::Unelevated) => WindowsSandboxLevel::RestrictedToken,
            None => WindowsSandboxLevel::Disabled,
        };
        let memories_config: MemoriesConfig = cfg.memories.clone().unwrap_or_default().into();
        let memories_root = codex_home.join("memories");

        let profiles_are_active = effective_permission_selection.profiles_are_active(
            default_permissions_override.as_deref(),
            permission_config_syntax,
        );
        let explicit_permission_profile_mode = default_permissions_override.is_some()
            || matches!(
                permission_config_syntax,
                Some(PermissionConfigSyntax::Profiles)
            );
        let custom_permission_profiles = cfg
            .permissions
            .as_ref()
            .map_or_else(Vec::new, |permissions| {
                permissions
                    .entries
                    .iter()
                    .map(|(id, profile)| CustomPermissionProfileSummary {
                        id: id.clone(),
                        description: profile.description.clone(),
                    })
                    .collect()
            });
        let using_implicit_builtin_profile = permission_config_syntax.is_none()
            && effective_permission_selection.selected_profile_id.is_none();
        let should_seed_legacy_workspace_roots = effective_permission_selection
            .selected_profile_id
            .is_none()
            && matches!(
                permission_config_syntax,
                None | Some(PermissionConfigSyntax::Legacy)
            );
        let legacy_workspace_roots_explicit = should_seed_legacy_workspace_roots
            && cfg
                .sandbox_workspace_write
                .as_ref()
                .is_some_and(|sandbox_workspace_write| {
                    !sandbox_workspace_write.writable_roots.is_empty()
                });
        let workspace_roots_explicit = workspace_roots_override.is_some()
            || !requested_additional_writable_roots.is_empty()
            || legacy_workspace_roots_explicit;
        let mut workspace_roots = match workspace_roots_override {
            Some(workspace_roots) => workspace_roots,
            None => {
                let mut workspace_roots = vec![resolved_cwd.clone()];
                workspace_roots.extend(requested_additional_writable_roots.clone());
                if should_seed_legacy_workspace_roots
                    && let Some(sandbox_workspace_write) = cfg.sandbox_workspace_write.as_ref()
                {
                    workspace_roots.extend(sandbox_workspace_write.writable_roots.clone());
                }
                workspace_roots
            }
        };
        dedupe_absolute_paths(&mut workspace_roots);
        let (
            mut configured_network_proxy_config,
            permission_profile,
            file_system_sandbox_policy,
            mut active_permission_profile,
            mut profile_workspace_roots,
        ) = if let Some(permission_profile) = permission_profile {
            let (file_system_sandbox_policy, _network_sandbox_policy) =
                permission_profile.to_runtime_permissions();
            let configured_network_proxy_config =
                if profile_allows_configured_network_proxy(&permission_profile)
                    && profiles_are_active
                {
                    let default_permissions = effective_permission_selection
                        .selected_profile_id
                        .unwrap_or_else(|| {
                            default_builtin_permission_profile_name(
                                &active_project,
                                windows_sandbox_level,
                            )
                        });
                    network_proxy_config_for_profile_selection(
                        effective_permission_selection.profiles.as_ref(),
                        default_permissions,
                    )?
                } else {
                    NetworkProxyConfig::default()
                };
            (
                configured_network_proxy_config,
                permission_profile,
                file_system_sandbox_policy,
                None,
                Vec::new(),
            )
        } else if profiles_are_active {
            let default_permissions = effective_permission_selection
                .selected_profile_id
                .unwrap_or_else(|| {
                    default_builtin_permission_profile_name(&active_project, windows_sandbox_level)
                });
            let builtin_workspace_write_settings = if using_implicit_builtin_profile {
                cfg.sandbox_workspace_write.as_ref()
            } else {
                None
            };
            let configured_network_proxy_config = network_proxy_config_for_profile_selection(
                effective_permission_selection.profiles.as_ref(),
                default_permissions,
            )?;
            let (mut file_system_sandbox_policy, network_sandbox_policy) =
                compile_permission_profile_selection(
                    effective_permission_selection.profiles.as_ref(),
                    default_permissions,
                    builtin_workspace_write_settings,
                    resolved_cwd.as_path(),
                    &mut startup_warnings,
                )?;
            let mut configured_workspace_roots = compile_permission_profile_workspace_roots(
                effective_permission_selection.profiles.as_ref(),
                default_permissions,
                resolved_cwd.as_path(),
            )?;
            if using_implicit_builtin_profile
                && default_permissions == BUILT_IN_WORKSPACE_PROFILE
                && let Some(sandbox_workspace_write) = cfg.sandbox_workspace_write.as_ref()
            {
                configured_workspace_roots.extend(sandbox_workspace_write.writable_roots.clone());
            }
            dedupe_absolute_paths(&mut configured_workspace_roots);
            file_system_sandbox_policy = file_system_sandbox_policy
                .with_materialized_project_roots_for_workspace_roots(&configured_workspace_roots);
            let permission_profile = if let Some(permission_profile) =
                builtin_permission_profile(default_permissions, builtin_workspace_write_settings)
            {
                permission_profile
            } else {
                PermissionProfile::from_runtime_permissions(
                    &file_system_sandbox_policy,
                    network_sandbox_policy,
                )
            };
            let active_permission_profile = if using_implicit_builtin_profile
                && default_permissions == BUILT_IN_WORKSPACE_PROFILE
                && cfg.sandbox_workspace_write.is_some()
            {
                None
            } else {
                let selected_profile_extends = cfg
                    .permissions
                    .as_ref()
                    .and_then(|permissions| permissions.entries.get(default_permissions))
                    .and_then(|profile| profile.extends.clone());
                Some(ActivePermissionProfile {
                    id: default_permissions.to_string(),
                    extends: selected_profile_extends,
                })
            };
            (
                configured_network_proxy_config,
                permission_profile,
                file_system_sandbox_policy,
                active_permission_profile,
                configured_workspace_roots,
            )
        } else {
            let configured_network_proxy_config = NetworkProxyConfig::default();
            let mut permission_profile = cfg
                .derive_permission_profile(
                    sandbox_mode,
                    windows_sandbox_level,
                    Some(&active_project),
                    Some(&constrained_permission_profile),
                )
                .await;
            if let Err(err) = permission_profile.to_legacy_sandbox_policy(resolved_cwd.as_path()) {
                tracing::warn!(
                    error = %err,
                    "derived permission profile cannot be represented as a legacy sandbox policy; falling back to read-only"
                );
                permission_profile = PermissionProfile::read_only();
            }
            let (file_system_sandbox_policy, _network_sandbox_policy) =
                permission_profile.to_runtime_permissions();
            (
                configured_network_proxy_config,
                permission_profile,
                file_system_sandbox_policy,
                None,
                Vec::new(),
            )
        };
        if enable_network_proxy && permission_profile.network_sandbox_policy().is_enabled() {
            if let Some(network_proxy) = network_proxy_toml_config(cfg.features.as_ref()) {
                apply_network_proxy_feature_config(
                    &mut configured_network_proxy_config,
                    network_proxy,
                );
            }
            configured_network_proxy_config.network.enabled = true;
        }
        let approval_policy_was_explicit =
            approval_policy_override.is_some() || cfg.approval_policy.is_some();
        let mut approval_policy = approval_policy_override
            .or(cfg.approval_policy)
            .unwrap_or_else(|| {
                if active_project.is_trusted() {
                    AskForApproval::OnRequest
                } else if active_project.is_untrusted() {
                    AskForApproval::UnlessTrusted
                } else {
                    AskForApproval::default()
                }
            });
        if !approval_policy_was_explicit
            && let Err(err) = constrained_approval_policy.can_set(&approval_policy)
        {
            tracing::warn!(
                error = %err,
                "default approval policy is disallowed by requirements; falling back to required default"
            );
            approval_policy = constrained_approval_policy.value();
        }
        let approvals_reviewer_was_explicit =
            approvals_reviewer_override.is_some() || cfg.approvals_reviewer.is_some();
        let mut approvals_reviewer = approvals_reviewer_override
            .or(cfg.approvals_reviewer)
            .unwrap_or(ApprovalsReviewer::User);
        if !approvals_reviewer_was_explicit
            && let Err(err) = constrained_approvals_reviewer.can_set(&approvals_reviewer)
        {
            tracing::warn!(
                error = %err,
                "default approvals reviewer is disallowed by requirements; falling back to required default"
            );
            approvals_reviewer = constrained_approvals_reviewer.value();
        }
        let web_search_mode =
            resolve_web_search_mode(&cfg, &features).unwrap_or(WebSearchMode::Cached);
        let web_search_config = resolve_web_search_config(&cfg);
        let experimental_request_user_input_enabled =
            resolve_experimental_request_user_input_enabled(&cfg);
        let code_mode = resolve_code_mode_config(&cfg);
        let multi_agent_v2 = resolve_multi_agent_v2_config(&cfg);
        let rollout_budget = resolve_rollout_budget_config(&cfg, &features)?;
        let current_time_reminder = resolve_current_time_reminder_config(&cfg, &features)?;

        let agent_roles =
            agent_roles::load_agent_roles(fs, &cfg, &config_layer_stack, &mut startup_warnings)
                .await?;

        let model_providers = cfg.model_providers;

        let model_provider_id = model_provider.or(cfg.model_provider).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "no model provider selected: set `model_provider` to a declared \
                 `model_providers.<id>` (no default provider)",
            )
        })?;
        if model_provider_id == LEGACY_OLLAMA_CHAT_PROVIDER_ID {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                OLLAMA_CHAT_PROVIDER_REMOVED_ERROR,
            ));
        }
        let model_provider = model_providers
            .get(&model_provider_id)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Model provider `{model_provider_id}` not found"),
                )
            })?
            .clone();

        let shell_environment_policy = cfg.shell_environment_policy.into();
        let allow_login_shell = cfg.allow_login_shell.unwrap_or(true);

        if multi_agent_v2.max_concurrent_threads_per_session == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.multi_agent_v2.max_concurrent_threads_per_session must be at least 1",
            ));
        }
        validate_multi_agent_v2_wait_timeout(
            "features.multi_agent_v2.min_wait_timeout_ms",
            multi_agent_v2.min_wait_timeout_ms,
        )?;
        validate_multi_agent_v2_wait_timeout(
            "features.multi_agent_v2.max_wait_timeout_ms",
            multi_agent_v2.max_wait_timeout_ms,
        )?;
        validate_multi_agent_v2_wait_timeout(
            "features.multi_agent_v2.default_wait_timeout_ms",
            multi_agent_v2.default_wait_timeout_ms,
        )?;
        if multi_agent_v2.min_wait_timeout_ms > multi_agent_v2.max_wait_timeout_ms {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.multi_agent_v2.min_wait_timeout_ms must be at most features.multi_agent_v2.max_wait_timeout_ms",
            ));
        }
        if multi_agent_v2.default_wait_timeout_ms < multi_agent_v2.min_wait_timeout_ms {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.multi_agent_v2.default_wait_timeout_ms must be at least features.multi_agent_v2.min_wait_timeout_ms",
            ));
        }
        if multi_agent_v2.default_wait_timeout_ms > multi_agent_v2.max_wait_timeout_ms {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.multi_agent_v2.default_wait_timeout_ms must be at most features.multi_agent_v2.max_wait_timeout_ms",
            ));
        }
        validate_multi_agent_v2_tool_namespace(multi_agent_v2.tool_namespace.as_deref())?;
        let agent_max_threads = cfg.agents.as_ref().and_then(|agents| agents.max_threads);
        if agent_max_threads == Some(0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agents.max_threads must be at least 1",
            ));
        }
        let agent_max_depth = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.max_depth)
            .unwrap_or(DEFAULT_AGENT_MAX_DEPTH);
        if agent_max_depth < 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agents.max_depth must be at least 1",
            ));
        }
        let agent_job_max_runtime_seconds = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.job_max_runtime_seconds)
            .or(DEFAULT_AGENT_JOB_MAX_RUNTIME_SECONDS);
        if agent_job_max_runtime_seconds == Some(0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agents.job_max_runtime_seconds must be at least 1",
            ));
        }
        if let Some(max_runtime_seconds) = agent_job_max_runtime_seconds
            && max_runtime_seconds > i64::MAX as u64
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agents.job_max_runtime_seconds must fit within a 64-bit signed integer",
            ));
        }
        let agent_interrupt_message_enabled = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.interrupt_message)
            .unwrap_or(true);
        const MIN_EMPTY_YIELD_TIME_MS: u64 = 5_000;
        const DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS: u64 = 300_000;
        let background_terminal_max_timeout = cfg
            .background_terminal_max_timeout
            .unwrap_or(DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS)
            .max(MIN_EMPTY_YIELD_TIME_MS);

        let ghost_snapshot = {
            let mut config = GhostSnapshotConfig::default();
            if let Some(ghost_snapshot) = cfg.ghost_snapshot.as_ref()
                && let Some(ignore_over_bytes) = ghost_snapshot.ignore_large_untracked_files
            {
                config.ignore_large_untracked_files = if ignore_over_bytes > 0 {
                    Some(ignore_over_bytes)
                } else {
                    None
                };
            }
            if let Some(ghost_snapshot) = cfg.ghost_snapshot.as_ref()
                && let Some(threshold) = ghost_snapshot.ignore_large_untracked_dirs
            {
                config.ignore_large_untracked_dirs =
                    if threshold > 0 { Some(threshold) } else { None };
            }
            if let Some(ghost_snapshot) = cfg.ghost_snapshot.as_ref()
                && let Some(disable_warnings) = ghost_snapshot.disable_warnings
            {
                config.disable_warnings = disable_warnings;
            }
            config
        };

        let use_experimental_unified_exec_tool = features.enabled(Feature::UnifiedExec);

        let forced_chatgpt_workspace_id = cfg
            .forced_chatgpt_workspace_id
            .clone()
            .map(octopus_config::config_toml::ForcedChatgptWorkspaceIds::into_vec)
            .map(|values| {
                values
                    .into_iter()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|values| !values.is_empty());

        let forced_login_method = cfg.forced_login_method;

        let model = model.or(cfg.model);
        let notices = cfg.notice.unwrap_or_default();
        let service_tier = match service_tier_override {
            Some(Some(service_tier)) => Some(service_tier),
            Some(None) => Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string()),
            None => cfg.service_tier,
        };
        let service_tier = service_tier.and_then(|service_tier| {
            match ServiceTier::from_request_value(&service_tier) {
                Some(ServiceTier::Fast) => features
                    .enabled(Feature::FastMode)
                    .then(|| ServiceTier::Fast.request_value().to_string()),
                Some(ServiceTier::Flex) => Some(ServiceTier::Flex.request_value().to_string()),
                None => Some(service_tier),
            }
        });

        let compact_prompt = compact_prompt.or(cfg.compact_prompt).and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let model_instructions_path = cfg.model_instructions_file.as_ref();
        let file_base_instructions = Self::try_read_non_empty_file(
            fs,
            model_instructions_path,
            "model instructions file",
        )
        .await?;
        let base_instructions = base_instructions
            .or(file_base_instructions)
            .or(cfg.instructions.clone());
        let developer_instructions = developer_instructions.or(cfg.developer_instructions);
        let include_permissions_instructions = cfg.include_permissions_instructions.unwrap_or(true);
        let include_apps_instructions = cfg.include_apps_instructions.unwrap_or(true);
        let include_collaboration_mode_instructions =
            cfg.include_collaboration_mode_instructions.unwrap_or(true);
        let include_skill_instructions = true;
        let include_environment_context = cfg.include_environment_context.unwrap_or(true);
        let guardian_policy_config =
            guardian_policy_config_from_requirements(config_layer_stack.requirements_toml())
                .or_else(|| {
                    cfg.auto_review
                        .as_ref()
                        .and_then(|auto_review| normalize_guardian_policy_config(
                            auto_review.policy.as_deref(),
                        ))
                });
        let personality = personality
            .or(cfg.personality)
            .or_else(|| {
                features
                    .enabled(Feature::Personality)
                    .then_some(Personality::Pragmatic)
            });

        let experimental_compact_prompt_path = cfg.experimental_compact_prompt_file.as_ref();
        let file_compact_prompt = Self::try_read_non_empty_file(
            fs,
            experimental_compact_prompt_path,
            "experimental compact prompt file",
        )
        .await?;
        let compact_prompt = compact_prompt.or(file_compact_prompt);
        let zsh_path = default_zsh_path
            .or_else(|| InstallContext::current().bundled_zsh_path())
            .map(AbsolutePathBuf::into_path_buf);

        let review_model = override_review_model.or(cfg.review_model);

        let check_for_update_on_startup = cfg.check_for_update_on_startup.unwrap_or(true);
        let model_catalog = load_model_catalog(cfg.model_catalog_json.clone())?;

        let log_dir = cfg
            .log_dir
            .as_ref()
            .map(AbsolutePathBuf::to_path_buf)
            .unwrap_or_else(|| codex_home.join("log").to_path_buf());
        let original_permission_profile = permission_profile.clone();
        apply_requirement_constrained_value(
            "approval_policy",
            approval_policy,
            &mut constrained_approval_policy,
            &mut startup_warnings,
        )?;
        if let Some(Sourced {
            value: filesystem_requirements,
            source: filesystem_requirements_source,
        }) = filesystem_requirements.as_ref()
            && !filesystem_requirements.deny_read.is_empty()
        {
            let requirement_source = filesystem_requirements_source.clone();
            constrained_permission_profile
                .value
                .add_validator(move |permission_profile| {
                    let mode = sandbox_mode_requirement_for_permission_profile(permission_profile);
                    match mode {
                        SandboxModeRequirement::ReadOnly
                        | SandboxModeRequirement::WorkspaceWrite => Ok(()),
                        SandboxModeRequirement::DangerFullAccess
                        | SandboxModeRequirement::ExternalSandbox => {
                            Err(ConstraintError::InvalidValue {
                                field_name: "sandbox_mode",
                                candidate: format!("{mode:?}"),
                                allowed: "[read-only, workspace-write]".to_string(),
                                requirement_source: requirement_source.clone(),
                            })
                        }
                    }
                })
                .map_err(std::io::Error::from)?;
        }
        apply_requirement_constrained_value(
            "approvals_reviewer",
            approvals_reviewer,
            &mut constrained_approvals_reviewer,
            &mut startup_warnings,
        )?;
        let permission_profile_was_constrained = apply_requirement_constrained_value(
            "permission_profile",
            permission_profile,
            &mut constrained_permission_profile,
            &mut startup_warnings,
        )?;
        if permission_profile_was_constrained
            && sandbox_mode_requirement_for_permission_profile(&original_permission_profile)
                == SandboxModeRequirement::DangerFullAccess
            && constrained_permission_profile.get() == &PermissionProfile::read_only()
            && constrained_approval_policy.value() == AskForApproval::Never
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`approval_policy = \"never\"` cannot be used because requirements do not allow `sandbox_mode = \"danger-full-access\"`; Codex would fall back to read-only permissions with approvals disabled. Choose an `approval_policy` based on what you need, such as `on-request`, or choose an allowed sandbox mode.",
            ));
        }
        if permission_profile_was_constrained {
            active_permission_profile = None;
            profile_workspace_roots.clear();
        }
        apply_requirement_constrained_value(
            "web_search_mode",
            web_search_mode,
            &mut constrained_web_search_mode,
            &mut startup_warnings,
        )?;

        let mcp_servers = constrain_mcp_servers(cfg.mcp_servers.clone(), mcp_servers.as_ref())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{e}")))?;

        let network_permission_profile = constrained_permission_profile.get().clone();
        let network = build_network_proxy_spec(
            configured_network_proxy_config,
            network_requirements,
            &network_permission_profile,
        )?;
        let mut helper_readable_roots = get_readable_roots_required_for_codex_runtime(
            &codex_home,
            zsh_path.as_ref(),
            main_execve_wrapper_exe.as_ref(),
        );
        if features.enabled(Feature::MemoryTool) && memories_config.use_memories {
            helper_readable_roots.push(memories_root);
        }
        let effective_permission_profile = constrained_permission_profile.value.get().clone();
        let (mut effective_file_system_sandbox_policy, effective_network_sandbox_policy) =
            effective_permission_profile.to_runtime_permissions();
        if effective_permission_profile != original_permission_profile {
            effective_file_system_sandbox_policy
                .preserve_deny_read_restrictions_from(&file_system_sandbox_policy);
        }
        if let Some(Sourced {
            value: filesystem_requirements,
            ..
        }) = filesystem_requirements.as_ref()
        {
            apply_managed_filesystem_constraints(
                &mut effective_file_system_sandbox_policy,
                filesystem_requirements,
            );
        }
        let effective_file_system_sandbox_policy = effective_file_system_sandbox_policy
            .with_additional_readable_roots(resolved_cwd.as_path(), &helper_readable_roots);
        let effective_permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
            effective_permission_profile.enforcement(),
            &effective_file_system_sandbox_policy,
            effective_network_sandbox_policy,
        );
        constrained_permission_profile
            .value
            .set(effective_permission_profile)
            .map_err(std::io::Error::from)?;
        let permission_profile_state = PermissionProfileState::from_constrained_active_profile(
            constrained_permission_profile.value,
            active_permission_profile,
            profile_workspace_roots,
        )
        .map_err(std::io::Error::from)?;
        let otel = otel::resolve_config(cfg.otel.unwrap_or_default(), &mut startup_warnings);
        let config = Self {
            model,
            service_tier,
            review_model,
            model_context_window: cfg.model_context_window,
            model_auto_compact_token_limit: cfg.model_auto_compact_token_limit,
            model_auto_compact_token_limit_scope: cfg
                .model_auto_compact_token_limit_scope
                .unwrap_or_default(),
            model_provider_id,
            model_provider,
            cwd: resolved_cwd,
            workspace_roots: workspace_roots.clone(),
            workspace_roots_explicit,
            startup_warnings,
            permissions: Permissions {
                approval_policy: constrained_approval_policy.value,
                permission_profile_state,
                workspace_roots,
                network,
                allow_login_shell,
                shell_environment_policy,
                windows_sandbox_mode,
                windows_sandbox_private_desktop,
            },
            explicit_permission_profile_mode,
            custom_permission_profiles,
            approvals_reviewer: constrained_approvals_reviewer.value(),
            enforce_residency: enforce_residency.value,
            notify: cfg.notify,
            base_instructions,
            personality,
            developer_instructions,
            compact_prompt,
            include_permissions_instructions,
            include_apps_instructions,
            include_collaboration_mode_instructions,
            include_skill_instructions,
            orchestrator_skills_enabled,
            orchestrator_mcp_enabled,
            include_environment_context,
            cli_auth_credentials_store_mode: resolve_cli_auth_credentials_store_mode(
                cfg.cli_auth_credentials_store.unwrap_or_default(),
                env!("CARGO_PKG_VERSION"),
            ),
            mcp_servers,
            mcp_oauth_credentials_store_mode: resolve_mcp_oauth_credentials_store_mode(
                cfg.mcp_oauth_credentials_store.unwrap_or_default(),
                env!("CARGO_PKG_VERSION"),
            ),
            mcp_oauth_callback_port: cfg.mcp_oauth_callback_port,
            mcp_oauth_callback_url: cfg.mcp_oauth_callback_url.clone(),
            model_providers,
            project_doc_max_bytes: cfg.project_doc_max_bytes.unwrap_or(AGENTS_MD_MAX_BYTES),
            project_doc_fallback_filenames: cfg
                .project_doc_fallback_filenames
                .unwrap_or_default()
                .into_iter()
                .filter_map(|name| {
                    let trimmed = name.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                })
                .collect(),
            tool_output_token_limit: cfg.tool_output_token_limit,
            agent_max_threads,
            agent_max_depth,
            agent_roles,
            memories: memories_config,
            agent_job_max_runtime_seconds,
            agent_interrupt_message_enabled,
            codex_home,
            log_dir,
            config_lock_export_dir: cfg
                .debug
                .as_ref()
                .and_then(|debug| debug.config_lockfile.as_ref())
                .and_then(|config_lock| config_lock.export_dir.clone()),
            config_lock_allow_codex_version_mismatch: cfg
                .debug
                .as_ref()
                .and_then(|debug| debug.config_lockfile.as_ref())
                .and_then(|config_lock| config_lock.allow_codex_version_mismatch)
                .unwrap_or(false),
            config_lock_save_fields_resolved_from_model_catalog: cfg
                .debug
                .as_ref()
                .and_then(|debug| debug.config_lockfile.as_ref())
                .and_then(|config_lock| config_lock.save_fields_resolved_from_model_catalog)
                .unwrap_or(true),
            config_lock_toml: None,
            config_layer_stack,
            ephemeral: ephemeral.unwrap_or_default(),
            bypass_hook_trust,
            file_opener: cfg.file_opener.unwrap_or(UriBasedFileOpener::VsCode),
            codex_self_exe,
            codex_linux_sandbox_exe,
            main_execve_wrapper_exe,
            zsh_path,

            hide_agent_reasoning: cfg.hide_agent_reasoning.unwrap_or(false),
            show_raw_agent_reasoning: cfg
                .show_raw_agent_reasoning
                .or(show_raw_agent_reasoning)
                .unwrap_or(false),
            guardian_policy_config,
            model_reasoning_effort: cfg.model_reasoning_effort,
            plan_mode_reasoning_effort: cfg.plan_mode_reasoning_effort,
            model_reasoning_summary: cfg.model_reasoning_summary,
            model_supports_reasoning_summaries: cfg.model_supports_reasoning_summaries,
            model_catalog,
            model_verbosity: cfg.model_verbosity,
            chatgpt_base_url: cfg
                .chatgpt_base_url
                .unwrap_or("https://chatgpt.com/backend-api/".to_string()),
            respect_system_proxy,
            apps_mcp_product_sku: cfg.apps_mcp_product_sku.clone(),
            realtime_audio: cfg
                .audio
                .map_or_else(RealtimeAudioConfig::default, |audio| RealtimeAudioConfig {
                    microphone: audio.microphone,
                    speaker: audio.speaker,
                }),
            experimental_realtime_ws_base_url: cfg.experimental_realtime_ws_base_url,
            experimental_realtime_webrtc_call_base_url: cfg
                .experimental_realtime_webrtc_call_base_url,
            experimental_realtime_ws_model: cfg.experimental_realtime_ws_model,
            realtime: cfg
                .realtime
                .map_or_else(RealtimeConfig::default, |realtime| {
                    let defaults = RealtimeConfig::default();
                    RealtimeConfig {
                        version: realtime.version.unwrap_or(defaults.version),
                        session_type: realtime.session_type.unwrap_or(defaults.session_type),
                        transport: realtime.transport.unwrap_or(defaults.transport),
                        voice: realtime.voice,
                    }
                }),
            experimental_realtime_ws_backend_prompt: cfg.experimental_realtime_ws_backend_prompt,
            experimental_realtime_ws_startup_context: cfg.experimental_realtime_ws_startup_context,
            experimental_realtime_start_instructions: cfg.experimental_realtime_start_instructions,
            forced_chatgpt_workspace_id,
            forced_login_method,
            web_search_mode: constrained_web_search_mode.value,
            web_search_config,
            experimental_request_user_input_enabled,
            code_mode,
            use_experimental_unified_exec_tool,
            background_terminal_max_timeout,
            ghost_snapshot,
            multi_agent_v2,
            rollout_budget,
            current_time_reminder,
            features,
            suppress_unstable_features_warning: cfg
                .suppress_unstable_features_warning
                .unwrap_or(false),
            active_project,
            notices,
            check_for_update_on_startup,
            disable_paste_burst: cfg.disable_paste_burst.unwrap_or(false),
            analytics_enabled: cfg.analytics.as_ref().and_then(|a| a.enabled),
            tool_suggest,
            otel,
        };
        Ok(config)
        })
        .await
    }

    async fn try_read_non_empty_file(
        fs: &dyn ExecutorFileSystem,
        path: Option<&AbsolutePathBuf>,
        context: &str,
    ) -> std::io::Result<Option<String>> {
        let Some(path) = path else {
            return Ok(None);
        };

        let path_uri = PathUri::from_abs_path(path);
        let contents = fs.read_file_text(&path_uri, None).await.map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("failed to read {context} {}: {e}", path.display()),
            )
        })?;

        let s = contents.trim().to_string();
        if s.is_empty() {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{context} is empty: {}", path.display()),
            ))
        } else {
            Ok(Some(s))
        }
    }

    pub fn set_windows_sandbox_enabled(&mut self, value: bool) {
        self.permissions.windows_sandbox_mode = if value {
            Some(WindowsSandboxModeToml::Unelevated)
        } else if matches!(
            self.permissions.windows_sandbox_mode,
            Some(WindowsSandboxModeToml::Unelevated)
        ) {
            None
        } else {
            self.permissions.windows_sandbox_mode
        };
    }

    pub fn set_windows_elevated_sandbox_enabled(&mut self, value: bool) {
        self.permissions.windows_sandbox_mode = if value {
            Some(WindowsSandboxModeToml::Elevated)
        } else if matches!(
            self.permissions.windows_sandbox_mode,
            Some(WindowsSandboxModeToml::Elevated)
        ) {
            None
        } else {
            self.permissions.windows_sandbox_mode
        };
    }

    pub fn managed_network_requirements_enabled(&self) -> bool {
        !matches!(
            self.permissions.permission_profile(),
            PermissionProfile::Disabled
        ) && self
            .config_layer_stack
            .requirements_toml()
            .network
            .is_some()
    }
}

fn guardian_policy_config_from_requirements(
    requirements_toml: &ConfigRequirementsToml,
) -> Option<String> {
    normalize_guardian_policy_config(requirements_toml.guardian_policy_config.as_deref())
}

fn merge_managed_permission_profiles(
    configured_permissions: Option<&PermissionsToml>,
    requirements_toml: &ConfigRequirementsToml,
) -> std::io::Result<Option<PermissionsToml>> {
    let managed_profiles = requirements_toml
        .permissions
        .as_ref()
        .map(|permissions| &permissions.profiles)
        .filter(|profiles| !profiles.is_empty());
    let Some(managed_profiles) = managed_profiles else {
        return Ok(configured_permissions.cloned());
    };

    let mut merged_permissions = configured_permissions.cloned().unwrap_or_default();
    for (profile_id, managed_profile) in managed_profiles {
        if merged_permissions.entries.contains_key(profile_id) {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "requirements.toml permissions profile `{profile_id}` conflicts with a config-defined profile of the same name"
                ),
            ));
        }
        merged_permissions
            .entries
            .insert(profile_id.clone(), managed_profile.clone());
    }

    Ok(Some(merged_permissions))
}

fn resolve_effective_permission_selection<'a>(
    configured_permissions: Option<&PermissionsToml>,
    default_permissions_override: Option<&'a str>,
    configured_default_permissions: Option<&'a str>,
    requirements_toml: &'a ConfigRequirementsToml,
    startup_warnings: &mut Vec<String>,
) -> std::io::Result<EffectivePermissionSelection<'a>> {
    let profiles = merge_managed_permission_profiles(configured_permissions, requirements_toml)?;
    validate_user_permission_profile_names(profiles.as_ref())?;
    validate_required_permission_profile_catalog(requirements_toml, profiles.as_ref())?;
    let selected_profile_id = resolve_default_permissions(
        default_permissions_override,
        configured_default_permissions,
        requirements_toml,
        startup_warnings,
    )?;

    Ok(EffectivePermissionSelection {
        profiles,
        selected_profile_id,
        requirements_force_profile_selection: requirements_toml
            .allowed_permission_profiles
            .is_some(),
    })
}

fn resolve_default_permissions<'a>(
    default_permissions_override: Option<&'a str>,
    configured_default_permissions: Option<&'a str>,
    requirements_toml: &'a ConfigRequirementsToml,
    startup_warnings: &mut Vec<String>,
) -> std::io::Result<Option<&'a str>> {
    let selected_permissions = default_permissions_override.or(configured_default_permissions);
    let Some(allowed_permission_profiles) = requirements_toml.allowed_permission_profiles.as_ref()
    else {
        return Ok(selected_permissions);
    };
    let Some(fallback_permissions) = requirements_toml
        .default_permissions
        .as_deref()
        .or_else(|| implicit_default_permissions(allowed_permission_profiles))
    else {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "requirements.toml default_permissions must be set unless allowed_permission_profiles allows both `:workspace` and `:read-only`",
        ));
    };

    match selected_permissions {
        None => Ok(Some(fallback_permissions)),
        Some(selected_permissions)
            if is_permission_allowed(allowed_permission_profiles, selected_permissions) =>
        {
            Ok(Some(selected_permissions))
        }
        Some(selected_permissions) => {
            startup_warnings.push(format!(
                "Configured value for `permission_profile` is disallowed by requirements; falling back from `{selected_permissions}` to required value `{fallback_permissions}`."
            ));
            Ok(Some(fallback_permissions))
        }
    }
}

fn validate_required_permission_profile_catalog(
    requirements_toml: &ConfigRequirementsToml,
    available_permissions: Option<&PermissionsToml>,
) -> std::io::Result<()> {
    let is_known_profile = |profile_id: &str| {
        is_builtin_permission_profile_name(profile_id)
            || available_permissions
                .as_ref()
                .is_some_and(|permissions| permissions.entries.contains_key(profile_id))
    };

    let Some(allowed_permission_profiles) = requirements_toml.allowed_permission_profiles.as_ref()
    else {
        if requirements_toml.default_permissions.is_some() {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                "requirements.toml default_permissions requires allowed_permission_profiles",
            ));
        }
        return Ok(());
    };
    for profile_id in allowed_permission_profiles.keys() {
        if !is_known_profile(profile_id) {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "requirements.toml allowed_permission_profiles refers to undefined profile `{profile_id}`"
                ),
            ));
        }
    }

    let Some(default_permissions) = requirements_toml
        .default_permissions
        .as_deref()
        .or_else(|| implicit_default_permissions(allowed_permission_profiles))
    else {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "requirements.toml default_permissions must be set unless allowed_permission_profiles allows both `:workspace` and `:read-only`",
        ));
    };
    if !is_permission_allowed(allowed_permission_profiles, default_permissions) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            format!(
                "requirements.toml default_permissions `{default_permissions}` must be allowed by allowed_permission_profiles"
            ),
        ));
    }

    Ok(())
}

fn implicit_default_permissions(
    allowed_permission_profiles: &BTreeMap<String, bool>,
) -> Option<&'static str> {
    (is_permission_allowed(allowed_permission_profiles, BUILT_IN_WORKSPACE_PROFILE)
        && is_permission_allowed(allowed_permission_profiles, BUILT_IN_READ_ONLY_PROFILE))
    .then_some(BUILT_IN_WORKSPACE_PROFILE)
}

fn is_permission_allowed(
    allowed_permission_profiles: &BTreeMap<String, bool>,
    profile_id: &str,
) -> bool {
    allowed_permission_profiles
        .get(profile_id)
        .copied()
        .unwrap_or(false)
}

fn normalize_guardian_policy_config(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub fn find_codex_home() -> std::io::Result<AbsolutePathBuf> {
    octopus_utils_home_dir::find_octopus_home()
}

pub fn log_dir(cfg: &Config) -> std::io::Result<PathBuf> {
    Ok(cfg.log_dir.clone())
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "config_loader_tests.rs"]
mod config_loader_tests;
