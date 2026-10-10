use std::path::PathBuf;

use crate::provider_config_runtime::CustomPermissionProfileRow;
use crate::provider_config_runtime::MemoriesSettings;
use crate::provider_config_runtime::NoticeSettings;
use crate::provider_config_runtime::ProjectConfigLayer;
use crate::provider_config_runtime::ProviderSessionSnapshot;
use crate::provider_config_runtime::WindowsSandboxRequirementPosture;
use crate::session_posture::SessionFeatures;
use crate::session_posture::SessionPermissions;
use crate::session_posture::WindowsSandboxMode;
use crate::session_vocab::config_types::ApprovalsReviewer;
use crate::session_vocab::config_types::ForcedLoginMethod;
use crate::session_vocab::config_types::Personality;
use crate::session_vocab::config_types::ReasoningSummary;
use crate::session_vocab::config_types::TrustLevel;
use crate::session_vocab::openai_models::ReasoningEffort;
use crate::session_vocab::provider_card::TerminalProviderCard;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_world_terminal_config::TerminalConfig;
use octopus_world_terminal_config::edit::TerminalConfigEditsBuilder;

#[derive(Clone, Debug, PartialEq)]
pub struct TuiConfig {
    pub terminal: TerminalConfig,

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

impl TuiConfig {
    pub fn from_snapshot(snapshot: ProviderSessionSnapshot) -> std::io::Result<Self> {
        let terminal = TerminalConfig::from_effective_config_value(&snapshot.effective_config)?;
        Ok(Self::from_halves(snapshot, terminal))
    }

    pub fn with_default_terminal(snapshot: ProviderSessionSnapshot) -> Self {
        Self::from_halves(snapshot, TerminalConfig::default())
    }

    fn from_halves(snapshot: ProviderSessionSnapshot, terminal: TerminalConfig) -> Self {
        let ProviderSessionSnapshot {
            model,
            cwd,
            codex_home,
            log_dir,
            log_dir_configured,
            notices,
            startup_warnings,
            service_tier,
            model_reasoning_effort,
            plan_mode_reasoning_effort,
            model_context_window,
            model_reasoning_summary,
            personality,
            memories,
            permissions,
            approvals_reviewer,
            features,
            workspace_roots,
            explicit_permission_profile_mode,
            custom_permission_profiles,
            model_provider_id,
            model_provider,
            forced_login_method,
            project_trust_level,
            disable_paste_burst,
            bypass_hook_trust,
            enabled_mcp_servers,
            effective_config,
            config_edit_path,
            project_layers,
            windows_sandbox_requirements,
        } = snapshot;
        Self {
            terminal,
            model,
            cwd,
            codex_home,
            log_dir,
            log_dir_configured,
            notices,
            startup_warnings,
            service_tier,
            model_reasoning_effort,
            plan_mode_reasoning_effort,
            model_context_window,
            model_reasoning_summary,
            personality,
            memories,
            permissions,
            approvals_reviewer,
            features,
            workspace_roots,
            explicit_permission_profile_mode,
            custom_permission_profiles,
            model_provider_id,
            model_provider,
            forced_login_method,
            project_trust_level,
            disable_paste_burst,
            bypass_hook_trust,
            enabled_mcp_servers,
            effective_config,
            config_edit_path,
            project_layers,
            windows_sandbox_requirements,
        }
    }

    pub fn terminal_config_edit_path(&self) -> PathBuf {
        self.config_edit_path.clone()
    }

    pub fn terminal_edits(&self) -> TerminalConfigEditsBuilder {
        TerminalConfigEditsBuilder::for_config_path(&self.terminal_config_edit_path())
    }

    pub fn effective_workspace_roots(&self) -> Vec<AbsolutePathBuf> {
        let mut workspace_roots = self.workspace_roots.clone();
        workspace_roots.extend(self.permissions.profile_workspace_roots().iter().cloned());
        let mut seen = std::collections::HashSet::new();
        workspace_roots.retain(|path| seen.insert(path.clone()));
        workspace_roots
    }

    pub fn set_windows_sandbox_enabled(&mut self, value: bool) {
        self.permissions.windows_sandbox_mode = if value {
            Some(WindowsSandboxMode::Unelevated)
        } else if matches!(
            self.permissions.windows_sandbox_mode,
            Some(WindowsSandboxMode::Unelevated)
        ) {
            None
        } else {
            self.permissions.windows_sandbox_mode
        };
    }

    pub fn set_windows_elevated_sandbox_enabled(&mut self, value: bool) {
        self.permissions.windows_sandbox_mode = if value {
            Some(WindowsSandboxMode::Elevated)
        } else if matches!(
            self.permissions.windows_sandbox_mode,
            Some(WindowsSandboxMode::Elevated)
        ) {
            None
        } else {
            self.permissions.windows_sandbox_mode
        };
    }

    #[cfg(test)]
    pub(crate) fn set_codex_home_for_tests(&mut self, home: AbsolutePathBuf) {
        self.log_dir = home.join("log").to_path_buf();
        self.config_edit_path = home.join("config.toml").to_path_buf();
        self.codex_home = home;
    }
}
