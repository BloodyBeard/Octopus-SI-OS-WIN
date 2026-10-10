use octopus_util_abspath::AbsolutePathBuf;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::config_toml::ToolsToml;
use crate::features::FeaturesToml;
use crate::types::AnalyticsConfigToml;
use crate::types::ApprovalsReviewer;
use crate::types::Personality;
use crate::types::SessionPickerViewMode;
use crate::types::WindowsToml;
use octopus_protocol::config_types::ReasoningSummary;
use octopus_protocol::config_types::SandboxMode;
use octopus_protocol::config_types::Verbosity;
use octopus_protocol::config_types::WebSearchMode;
use octopus_protocol::openai_models::ReasoningEffort;
use octopus_protocol::protocol::AskForApproval;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ConfigProfile {
    pub model: Option<String>,
    pub service_tier: Option<String>,
    pub model_provider: Option<String>,
    pub approval_policy: Option<AskForApproval>,
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    pub sandbox_mode: Option<SandboxMode>,
    pub model_reasoning_effort: Option<ReasoningEffort>,
    pub plan_mode_reasoning_effort: Option<ReasoningEffort>,
    pub model_reasoning_summary: Option<ReasoningSummary>,
    pub model_verbosity: Option<Verbosity>,
    pub model_catalog_json: Option<AbsolutePathBuf>,
    pub personality: Option<Personality>,
    pub chatgpt_base_url: Option<String>,
    pub model_instructions_file: Option<AbsolutePathBuf>,
    #[schemars(skip)]
    pub js_repl_node_path: Option<AbsolutePathBuf>,
    #[schemars(skip)]
    pub js_repl_node_module_dirs: Option<Vec<AbsolutePathBuf>>,
    pub experimental_compact_prompt_file: Option<AbsolutePathBuf>,
    pub include_permissions_instructions: Option<bool>,
    pub include_apps_instructions: Option<bool>,
    pub include_collaboration_mode_instructions: Option<bool>,
    pub include_environment_context: Option<bool>,
    pub experimental_use_unified_exec_tool: Option<bool>,
    pub tools: Option<ToolsToml>,
    pub web_search: Option<WebSearchMode>,
    pub analytics: Option<AnalyticsConfigToml>,
    #[serde(default)]
    pub tui: Option<ProfileTui>,
    #[serde(default)]
    pub windows: Option<WindowsToml>,
    #[serde(default)]
    #[schemars(schema_with = "crate::schema::features_schema")]
    pub features: Option<FeaturesToml>,
    pub oss_provider: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ProfileTui {
    #[serde(default)]
    pub session_picker_view: Option<SessionPickerViewMode>,
}
