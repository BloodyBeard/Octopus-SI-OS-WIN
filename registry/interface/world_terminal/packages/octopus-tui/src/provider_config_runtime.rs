use crate::session_posture::SessionFeatures;
use crate::session_posture::SessionPermissions;
use crate::session_vocab::config_types::ApprovalsReviewer;
use crate::session_vocab::config_types::ForcedLoginMethod;
use crate::session_vocab::config_types::Personality;
use crate::session_vocab::config_types::ReasoningSummary;
use crate::session_vocab::config_types::TrustLevel;
use crate::session_vocab::openai_models::ReasoningEffort;
use crate::session_vocab::provider_card::TerminalProviderCard;
use octopus_substrate_contract::approvals_substrate::AskForApproval;
use octopus_substrate_contract::sandbox_policy::SandboxMode;
use octopus_util_abspath::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionHarnessOverrides {
    pub model: Option<String>,
    pub approval_policy: Option<AskForApproval>,
    pub sandbox_mode: Option<SandboxMode>,
    pub cwd: Option<PathBuf>,
    pub model_provider: Option<String>,
    pub show_raw_agent_reasoning: Option<bool>,
    pub bypass_hook_trust: Option<bool>,
    pub additional_writable_roots: Vec<PathBuf>,
    pub default_permissions: Option<String>,
    pub codex_self_exe: Option<PathBuf>,
    pub codex_linux_sandbox_exe: Option<PathBuf>,
    pub main_execve_wrapper_exe: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionLoadRequest {
    pub codex_home: Option<PathBuf>,
    pub fallback_cwd: Option<PathBuf>,
    pub cli_kv_overrides: Vec<(String, toml::Value)>,
    pub profile_v2: Option<String>,
    pub strict_config: bool,
    pub harness: SessionHarnessOverrides,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoticeSettings {
    pub hide_full_access_warning: Option<bool>,
    pub hide_world_writable_warning: Option<bool>,
    pub fast_default_opt_out: Option<bool>,
    pub hide_rate_limit_model_nudge: Option<bool>,
    pub model_migrations: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoriesSettings {
    pub use_memories: bool,
    pub generate_memories: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomPermissionProfileRow {
    pub id: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectConfigLayer {
    pub dot_codex_folder: PathBuf,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowsSandboxRequirementPosture {
    pub unelevated_allowed: bool,
    pub elevated_allowed: bool,
    pub required: bool,
}

impl Default for WindowsSandboxRequirementPosture {
    fn default() -> Self {
        Self {
            unelevated_allowed: true,
            elevated_allowed: true,
            required: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderSessionSnapshot {
    pub model: Option<String>,
    pub cwd: AbsolutePathBuf,
    pub codex_home: AbsolutePathBuf,
    pub log_dir: PathBuf,
    pub log_dir_configured: bool,
    pub notices: NoticeSettings,
    pub startup_warnings: Vec<String>,
    pub service_tier: Option<String>,
    pub model_reasoning_effort: Option<ReasoningEffort>,
    pub plan_mode_reasoning_effort: Option<ReasoningEffort>,
    pub model_context_window: Option<i64>,
    pub model_reasoning_summary: Option<ReasoningSummary>,
    pub personality: Option<Personality>,
    pub memories: MemoriesSettings,
    pub permissions: SessionPermissions,
    pub approvals_reviewer: ApprovalsReviewer,
    pub features: SessionFeatures,
    pub workspace_roots: Vec<AbsolutePathBuf>,
    pub explicit_permission_profile_mode: bool,
    pub custom_permission_profiles: Vec<CustomPermissionProfileRow>,
    pub model_provider_id: String,
    pub model_provider: TerminalProviderCard,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub project_trust_level: Option<TrustLevel>,
    pub disable_paste_burst: bool,
    pub bypass_hook_trust: bool,
    pub enabled_mcp_servers: Vec<String>,
    pub effective_config: toml::Value,
    pub config_edit_path: PathBuf,
    pub project_layers: Vec<ProjectConfigLayer>,
    pub windows_sandbox_requirements: WindowsSandboxRequirementPosture,
}

impl ProviderSessionSnapshot {
    pub fn degraded(codex_home: AbsolutePathBuf, cwd: AbsolutePathBuf) -> Self {
        let config_edit_path = codex_home.join("config.toml").to_path_buf();
        let effective_config = std::fs::read_to_string(&config_edit_path)
            .ok()
            .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
            .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
        let log_dir = codex_home.join("log").to_path_buf();
        Self {
            model: None,
            cwd,
            log_dir,
            log_dir_configured: false,
            notices: NoticeSettings::default(),
            startup_warnings: Vec::new(),
            service_tier: None,
            model_reasoning_effort: None,
            plan_mode_reasoning_effort: None,
            model_context_window: None,
            model_reasoning_summary: None,
            personality: None,
            memories: MemoriesSettings::default(),
            permissions: SessionPermissions::degraded_default(),
            approvals_reviewer: ApprovalsReviewer::User,
            features: SessionFeatures::default(),
            workspace_roots: Vec::new(),
            explicit_permission_profile_mode: false,
            custom_permission_profiles: Vec::new(),
            model_provider_id: "none".to_string(),
            model_provider: TerminalProviderCard::default(),
            forced_login_method: None,
            project_trust_level: Some(TrustLevel::Trusted),
            disable_paste_burst: false,
            bypass_hook_trust: false,
            enabled_mcp_servers: Vec::new(),
            effective_config,
            config_edit_path,
            project_layers: Vec::new(),
            windows_sandbox_requirements: WindowsSandboxRequirementPosture::default(),
            codex_home,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingEdit {
    pub key_path: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugReportLineKind {
    CommandEcho,
    SectionHeader,
    Text,
    Muted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DebugReportLine {
    pub kind: DebugReportLineKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionProxyAddrs {
    pub http_addr: String,
    pub socks_addr: String,
}

#[derive(Default)]
pub struct TelemetryLayerBundle {
    pub logger_layer:
        Option<Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>>,
    pub tracing_layer:
        Option<Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>>,
}

pub trait ProviderConfigRuntime: Send + Sync {
    fn load(
        &self,
        request: SessionLoadRequest,
    ) -> BoxFuture<std::io::Result<ProviderSessionSnapshot>>;

    fn persist_edits(&self, edits: Vec<SettingEdit>) -> BoxFuture<std::io::Result<()>>;

    fn set_project_trust(&self, codex_home: &Path, project_path: &Path) -> std::io::Result<()>;

    fn debug_report(&self, session_proxy: Option<SessionProxyAddrs>) -> Vec<DebugReportLine>;

    fn resolve_oss_provider(&self, requested: Option<String>) -> BoxFuture<Option<String>>;

    fn execpolicy_warnings(&self) -> BoxFuture<Result<(), String>>;

    fn telemetry_layers(&self, originator: &str) -> TelemetryLayerBundle;
}

const NO_PROVIDER_RUNTIME: &str = "no provider config runtime is attached: the bare terminal \
                                   binary has no provider world behind it (run through the codex \
                                   product launcher for provider config affordances)";

struct DegradedProviderConfigRuntime;

impl ProviderConfigRuntime for DegradedProviderConfigRuntime {
    fn load(
        &self,
        request: SessionLoadRequest,
    ) -> BoxFuture<std::io::Result<ProviderSessionSnapshot>> {
        Box::pin(async move {
            let codex_home = match request.codex_home {
                Some(home) => AbsolutePathBuf::from_absolute_path(home)?,
                None => octopus_utils_home_dir::find_octopus_home()?,
            };
            let cwd = match request.harness.cwd.or(request.fallback_cwd) {
                Some(cwd) => AbsolutePathBuf::from_absolute_path(
                    octopus_util_abspath::canonicalize_existing_preserving_symlinks(&cwd)?,
                )?,
                None => AbsolutePathBuf::current_dir()?,
            };
            let mut snapshot = ProviderSessionSnapshot::degraded(codex_home, cwd);
            snapshot.model = request.harness.model;
            snapshot.bypass_hook_trust = request.harness.bypass_hook_trust.unwrap_or(false);
            Ok(snapshot)
        })
    }

    fn persist_edits(&self, _edits: Vec<SettingEdit>) -> BoxFuture<std::io::Result<()>> {
        Box::pin(async move {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                NO_PROVIDER_RUNTIME,
            ))
        })
    }

    fn set_project_trust(&self, _codex_home: &Path, _project_path: &Path) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            NO_PROVIDER_RUNTIME,
        ))
    }

    fn debug_report(&self, _session_proxy: Option<SessionProxyAddrs>) -> Vec<DebugReportLine> {
        vec![
            DebugReportLine {
                kind: DebugReportLineKind::CommandEcho,
                text: "/debug-config".to_string(),
            },
            DebugReportLine {
                kind: DebugReportLineKind::Muted,
                text: format!("  {NO_PROVIDER_RUNTIME}"),
            },
        ]
    }

    fn resolve_oss_provider(&self, _requested: Option<String>) -> BoxFuture<Option<String>> {
        Box::pin(async move { None })
    }

    fn execpolicy_warnings(&self) -> BoxFuture<Result<(), String>> {
        Box::pin(async move { Ok(()) })
    }

    fn telemetry_layers(&self, _originator: &str) -> TelemetryLayerBundle {
        TelemetryLayerBundle::default()
    }
}

pub fn degraded_runtime() -> Arc<dyn ProviderConfigRuntime> {
    Arc::new(DegradedProviderConfigRuntime)
}
