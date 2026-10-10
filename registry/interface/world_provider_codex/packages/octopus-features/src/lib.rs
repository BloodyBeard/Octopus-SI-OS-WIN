use octopus_protocol::protocol::Event;
use octopus_protocol::protocol::EventMsg;
use octopus_protocol::protocol::WarningEvent;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use toml::Table;

mod descriptor;
mod feature_configs;
mod legacy;
mod telemetry;
pub use descriptor::features_root;
pub use descriptor::load_feature_descriptors_from_disk;
pub use descriptor::FeatureDescriptor;
pub use descriptor::FeatureId;
pub use descriptor::FeatureSet;
pub use descriptor::StageDescriptor;
pub use descriptor::FEATURES_ROOT_ENV;
pub use feature_configs::CodeModeConfigToml;
pub use feature_configs::CurrentTimeReminderConfigToml;
pub use feature_configs::CurrentTimeSource;
pub use feature_configs::MultiAgentV2ConfigToml;
pub use feature_configs::NetworkProxyConfigToml;
pub use feature_configs::NetworkProxyDomainPermissionToml;
pub use feature_configs::NetworkProxyModeToml;
pub use feature_configs::NetworkProxyUnixSocketPermissionToml;
use feature_configs::RemovedAppsMcpPathOverrideConfigToml;
pub use feature_configs::RolloutBudgetConfigToml;
pub use legacy::legacy_feature_keys;
use legacy::LegacyFeatureToggles;
pub use telemetry::SessionTelemetry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    UnderDevelopment,
    Experimental {
        name: &'static str,
        menu_description: &'static str,
        announcement: &'static str,
    },
    Stable,
    Deprecated,
    Removed,
}

impl Stage {
    pub fn experimental_menu_name(self) -> Option<&'static str> {
        match self {
            Stage::Experimental { name, .. } => Some(name),
            Stage::UnderDevelopment | Stage::Stable | Stage::Deprecated | Stage::Removed => None,
        }
    }

    pub fn experimental_menu_description(self) -> Option<&'static str> {
        match self {
            Stage::Experimental {
                menu_description, ..
            } => Some(menu_description),
            Stage::UnderDevelopment | Stage::Stable | Stage::Deprecated | Stage::Removed => None,
        }
    }

    pub fn experimental_announcement(self) -> Option<&'static str> {
        match self {
            Stage::Experimental {
                announcement: "", ..
            } => None,
            Stage::Experimental { announcement, .. } => Some(announcement),
            Stage::UnderDevelopment | Stage::Stable | Stage::Deprecated | Stage::Removed => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    ShellTool,
    CodexHooks,
    SecretAuthStorage,

    CodeMode,
    CodeModeOnly,
    UnifiedExec,
    ShellZshFork,
    UnifiedExecZshFork,
    TerminalResizeReflow,
    TerminalVisualizationInstructions,
    ApplyPatchStreamingEvents,
    ExecPermissionApprovals,
    RequestPermissionsTool,
    WebSearchRequest,
    WebSearchCached,
    StandaloneWebSearch,
    UseLegacyLandlock,
    ShellSnapshot,
    DeferredExecutor,
    RuntimeMetrics,
    MemoryTool,
    LocalThreadStoreCompression,
    Chronicle,
    EnableRequestCompression,
    NetworkProxy,
    RespectSystemProxy,
    Collab,
    MultiAgentV2,
    MultiAgentMode,
    SpawnCsv,
    Apps,
    EnableMcpApps,
    AppsMcpPathOverride,
    ToolSearch,
    ToolSearchAlwaysDeferMcpTools,
    NonPrefixedMcpToolNames,
    ToolSuggest,
    Plugins,
    PluginHooks,
    InAppBrowser,
    BrowserUse,
    BrowserUseExternal,
    ComputerUse,
    RemotePlugin,
    PluginSharing,
    ExternalMigration,
    ImageGeneration,
    ImageGenExt,
    ResizeAllImages,
    ItemIds,
    SkillMcpDependencyInstall,
    SkillEnvVarDependencyPrompt,
    MentionsV2,
    DefaultModeRequestUserInput,
    GuardianApproval,
    Goals,
    TokenBudget,
    RolloutBudget,
    CurrentTimeReminder,
    SleepTool,
    ToolCallMcpElicitation,
    AuthElicitation,
    Personality,
    Artifact,
    FastMode,
    RealtimeConversation,
    PreventIdleSleep,
    RemoteCompactionV2,
    UseAgentIdentity,
    WorkspaceDependencies,

    GhostCommit,
    JsRepl,
    JsReplToolsOnly,
    SearchTool,
    UseLinuxSandboxBwrap,
    RequestRule,
    WindowsSandbox,
    WindowsSandboxElevated,
    RemoteModels,
    CodexGitCommit,
    Sqlite,
    ApplyPatchFreeform,
    UnavailableDummyTools,
    Steer,
    CollaborationModes,
    RemoteControl,
    ImageDetailOriginal,
    TuiAppServer,
    WorkspaceOwnerUsageNudge,
    ResponsesWebsockets,
    ResponsesWebsocketsV2,
}

impl Feature {
    pub fn key(self) -> &'static str {
        match self {
            Feature::GhostCommit => "undo",
            Feature::ShellTool => "shell_tool",
            Feature::SecretAuthStorage => "secret_auth_storage",
            Feature::UnifiedExec => "unified_exec",
            Feature::ShellZshFork => "shell_zsh_fork",
            Feature::UnifiedExecZshFork => "unified_exec_zsh_fork",
            Feature::ShellSnapshot => "shell_snapshot",
            Feature::DeferredExecutor => "deferred_executor",
            Feature::JsRepl => "js_repl",
            Feature::CodeMode => "code_mode",
            Feature::CodeModeOnly => "code_mode_only",
            Feature::JsReplToolsOnly => "js_repl_tools_only",
            Feature::TerminalResizeReflow => "terminal_resize_reflow",
            Feature::WebSearchRequest => "web_search_request",
            Feature::WebSearchCached => "web_search_cached",
            Feature::StandaloneWebSearch => "standalone_web_search",
            Feature::SearchTool => "search_tool",
            Feature::CodexGitCommit => "codex_git_commit",
            Feature::RuntimeMetrics => "runtime_metrics",
            Feature::Sqlite => "sqlite",
            Feature::MemoryTool => "memories",
            Feature::LocalThreadStoreCompression => "local_thread_store_compression",
            Feature::Chronicle => "chronicle",
            Feature::ApplyPatchFreeform => "apply_patch_freeform",
            Feature::ApplyPatchStreamingEvents => "apply_patch_streaming_events",
            Feature::ExecPermissionApprovals => "exec_permission_approvals",
            Feature::CodexHooks => "hooks",
            Feature::RequestPermissionsTool => "request_permissions_tool",
            Feature::UseLinuxSandboxBwrap => "use_linux_sandbox_bwrap",
            Feature::UseLegacyLandlock => "use_legacy_landlock",
            Feature::RequestRule => "request_rule",
            Feature::WindowsSandbox => "experimental_windows_sandbox",
            Feature::WindowsSandboxElevated => "elevated_windows_sandbox",
            Feature::RemoteModels => "remote_models",
            Feature::EnableRequestCompression => "enable_request_compression",
            Feature::NetworkProxy => "network_proxy",
            Feature::RespectSystemProxy => "respect_system_proxy",
            Feature::Collab => "multi_agent",
            Feature::MultiAgentV2 => "multi_agent_v2",
            Feature::MultiAgentMode => "multi_agent_mode",
            Feature::SpawnCsv => "enable_fanout",
            Feature::Apps => "apps",
            Feature::EnableMcpApps => "enable_mcp_apps",
            Feature::AppsMcpPathOverride => "apps_mcp_path_override",
            Feature::ToolSearch => "tool_search",
            Feature::ToolSearchAlwaysDeferMcpTools => "tool_search_always_defer_mcp_tools",
            Feature::NonPrefixedMcpToolNames => "non_prefixed_mcp_tool_names",
            Feature::UnavailableDummyTools => "unavailable_dummy_tools",
            Feature::ToolSuggest => "tool_suggest",
            Feature::Plugins => "plugins",
            Feature::PluginHooks => "plugin_hooks",
            Feature::InAppBrowser => "in_app_browser",
            Feature::BrowserUse => "browser_use",
            Feature::BrowserUseExternal => "browser_use_external",
            Feature::ComputerUse => "computer_use",
            Feature::RemotePlugin => "remote_plugin",
            Feature::PluginSharing => "plugin_sharing",
            Feature::ExternalMigration => "external_migration",
            Feature::ImageGeneration => "image_generation",
            Feature::ImageGenExt => "imagegenext",
            Feature::ResizeAllImages => "resize_all_images",
            Feature::ItemIds => "item_ids",
            Feature::SkillMcpDependencyInstall => "skill_mcp_dependency_install",
            Feature::SkillEnvVarDependencyPrompt => "skill_env_var_dependency_prompt",
            Feature::MentionsV2 => "mentions_v2",
            Feature::Steer => "steer",
            Feature::DefaultModeRequestUserInput => "default_mode_request_user_input",
            Feature::TerminalVisualizationInstructions => "terminal_visualization_instructions",
            Feature::GuardianApproval => "guardian_approval",
            Feature::Goals => "goals",
            Feature::TokenBudget => "token_budget",
            Feature::RolloutBudget => "rollout_budget",
            Feature::CurrentTimeReminder => "current_time_reminder",
            Feature::SleepTool => "sleep_tool",
            Feature::CollaborationModes => "collaboration_modes",
            Feature::ToolCallMcpElicitation => "tool_call_mcp_elicitation",
            Feature::AuthElicitation => "auth_elicitation",
            Feature::Personality => "personality",
            Feature::Artifact => "artifact",
            Feature::FastMode => "fast_mode",
            Feature::RealtimeConversation => "realtime_conversation",
            Feature::RemoteControl => "remote_control",
            Feature::ImageDetailOriginal => "image_detail_original",
            Feature::TuiAppServer => "tui_app_server",
            Feature::PreventIdleSleep => "prevent_idle_sleep",
            Feature::WorkspaceOwnerUsageNudge => "workspace_owner_usage_nudge",
            Feature::ResponsesWebsockets => "responses_websockets",
            Feature::ResponsesWebsocketsV2 => "responses_websockets_v2",
            Feature::RemoteCompactionV2 => "remote_compaction_v2",
            Feature::UseAgentIdentity => "use_agent_identity",
            Feature::WorkspaceDependencies => "workspace_dependencies",
        }
    }

    pub fn stage(self) -> Stage {
        self.info().stage
    }

    pub fn default_enabled(self) -> bool {
        self.info().default_enabled
    }

    pub fn id(self) -> FeatureId {
        FeatureId::new(self.key())
    }

    fn info(self) -> &'static FeatureSpec {
        FEATURES
            .iter()
            .find(|spec| spec.id == self)
            .unwrap_or_else(|| {
                panic!(
                    "feature {self:?} (key {:?}) has no backing on-disk feature.json descriptor",
                    self.key()
                )
            })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExperimentalFeatureRow {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LegacyFeatureUsage {
    pub alias: String,
    pub feature: Feature,
    pub summary: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Features {
    enabled: BTreeSet<FeatureId>,
    legacy_usages: BTreeSet<LegacyFeatureUsage>,
}

#[derive(Debug, Clone, Default)]
pub struct FeatureOverrides {
    pub web_search_request: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FeatureConfigSource<'a> {
    pub features: Option<&'a FeaturesToml>,
    pub experimental_use_unified_exec_tool: Option<bool>,
}

impl FeatureOverrides {
    fn apply(self, features: &mut Features) {
        if let Some(enabled) = self.web_search_request {
            if enabled {
                features.enable(Feature::WebSearchRequest);
            } else {
                features.disable(Feature::WebSearchRequest);
            }
            features.record_legacy_usage("web_search_request", Feature::WebSearchRequest);
        }
    }
}

impl Features {
    pub fn try_with_defaults() -> octopus_descriptors::Result<Self> {
        let mut set = BTreeSet::new();
        for (_name, desc) in descriptor::load_feature_descriptors_from_disk()? {
            let effective_default = canonical_feature_for_key(desc.id.as_str())
                .and_then(platform_default_override)
                .unwrap_or(desc.default_enabled);
            if effective_default {
                set.insert(desc.id);
            }
        }
        Ok(Self {
            enabled: set,
            legacy_usages: BTreeSet::new(),
        })
    }

    pub fn with_defaults() -> Self {
        Self::try_with_defaults().unwrap_or_else(|e| {
            panic!("feature registry failed to load from disk (fail-closed): {e}")
        })
    }

    pub fn enabled(&self, f: Feature) -> bool {
        self.enabled.contains(&f.id())
    }

    pub fn apps_enabled_for_auth(&self, has_chatgpt_auth: bool) -> bool {
        self.enabled(Feature::Apps) && has_chatgpt_auth
    }

    pub fn use_legacy_landlock(&self) -> bool {
        self.enabled(Feature::UseLegacyLandlock)
    }

    pub fn enable(&mut self, f: Feature) -> &mut Self {
        self.enabled.insert(f.id());
        self
    }

    pub fn disable(&mut self, f: Feature) -> &mut Self {
        self.enabled.remove(&f.id());
        self
    }

    pub fn set_enabled(&mut self, f: Feature, enabled: bool) -> &mut Self {
        if enabled {
            self.enable(f)
        } else {
            self.disable(f)
        }
    }

    pub fn enabled_key(&self, key: &str) -> bool {
        feature_for_key(key).is_some_and(|feature| self.enabled(feature))
    }

    pub fn set_enabled_key(&mut self, key: &str, enabled: bool) -> bool {
        match feature_for_key(key) {
            Some(feature) => {
                self.set_enabled(feature, enabled);
                true
            }
            None => false,
        }
    }

    pub fn experimental_menu_rows(&self) -> Vec<ExperimentalFeatureRow> {
        FEATURES
            .iter()
            .filter_map(|spec| {
                let name = spec.stage.experimental_menu_name()?;
                let description = spec.stage.experimental_menu_description()?;
                Some(ExperimentalFeatureRow {
                    key: spec.key,
                    name,
                    description,
                    enabled: self.enabled(spec.id),
                })
            })
            .collect()
    }

    pub fn experimental_announcements(&self) -> Vec<&'static str> {
        FEATURES
            .iter()
            .filter_map(|spec| spec.stage.experimental_announcement())
            .collect()
    }

    pub fn key_default_enabled(&self, key: &str) -> Option<bool> {
        feature_for_key(key).map(Feature::default_enabled)
    }

    pub fn record_legacy_usage_force(&mut self, alias: &str, feature: Feature) {
        let (summary, details) = legacy_usage_notice(alias, feature);
        self.legacy_usages.insert(LegacyFeatureUsage {
            alias: alias.to_string(),
            feature,
            summary,
            details,
        });
    }

    pub fn record_legacy_usage(&mut self, alias: &str, feature: Feature) {
        if alias == feature.key() {
            return;
        }
        self.record_legacy_usage_force(alias, feature);
    }

    pub fn legacy_feature_usages(&self) -> impl Iterator<Item = &LegacyFeatureUsage> + '_ {
        self.legacy_usages.iter()
    }

    pub fn emit_metrics(&self, otel: &SessionTelemetry) {
        for feature in FEATURES {
            if matches!(feature.stage, Stage::Removed) {
                continue;
            }
            if self.enabled(feature.id) != feature.default_enabled {
                otel.counter(
                    "codex.feature.state",
                    1,
                    &[
                        ("feature", feature.key),
                        ("value", &self.enabled(feature.id).to_string()),
                    ],
                );
            }
        }
    }

    pub fn apply_map(&mut self, m: &BTreeMap<String, bool>) {
        for (k, v) in m {
            match k.as_str() {
                "web_search_request" => {
                    self.record_legacy_usage_force(
                        "features.web_search_request",
                        Feature::WebSearchRequest,
                    );
                }
                "web_search_cached" => {
                    self.record_legacy_usage_force(
                        "features.web_search_cached",
                        Feature::WebSearchCached,
                    );
                }
                "tui_app_server" => {
                    continue;
                }
                "undo" => {
                    continue;
                }
                "js_repl" => {
                    continue;
                }
                "js_repl_tools_only" => {
                    continue;
                }
                "remote_control" => {
                    continue;
                }
                "apply_patch_freeform" => {
                    continue;
                }
                "tool_search" | "apps_mcp_path_override" => {
                    continue;
                }
                "image_detail_original" => {
                    continue;
                }
                "plugin_hooks" => {
                    continue;
                }
                "skill_env_var_dependency_prompt" => {
                    continue;
                }
                "terminal_resize_reflow" => {
                    continue;
                }
                "use_legacy_landlock" => {
                    self.record_legacy_usage_force(
                        "features.use_legacy_landlock",
                        Feature::UseLegacyLandlock,
                    );
                }
                _ => {}
            }
            match feature_for_key(k) {
                Some(feat) => {
                    if matches!(feat, Feature::TuiAppServer) {
                        continue;
                    }
                    if k != feat.key() {
                        self.record_legacy_usage(k.as_str(), feat);
                    }
                    if *v {
                        self.enable(feat);
                    } else {
                        self.disable(feat);
                    }
                }
                None => {
                    tracing::warn!("unknown feature key in config: {k}");
                }
            }
        }
    }

    pub fn from_sources(
        base: FeatureConfigSource<'_>,
        profile: FeatureConfigSource<'_>,
        overrides: FeatureOverrides,
    ) -> Self {
        let mut features = Features::with_defaults();

        for source in [base, profile] {
            LegacyFeatureToggles {
                experimental_use_unified_exec_tool: source.experimental_use_unified_exec_tool,
            }
            .apply(&mut features);

            if let Some(feature_entries) = source.features {
                features.apply_toml(feature_entries);
            }
        }

        overrides.apply(&mut features);
        features.normalize_dependencies();

        features
    }

    pub fn enabled_features(&self) -> Vec<Feature> {
        self.enabled
            .iter()
            .filter_map(|id| canonical_feature_for_key(id.as_str()))
            .collect()
    }

    pub fn normalize_dependencies(&mut self) {
        if self.enabled(Feature::SpawnCsv) && !self.enabled(Feature::Collab) {
            self.enable(Feature::Collab);
        }
        if self.enabled(Feature::CodeModeOnly) && !self.enabled(Feature::CodeMode) {
            self.enable(Feature::CodeMode);
        }
    }
}

fn legacy_usage_notice(alias: &str, feature: Feature) -> (String, Option<String>) {
    let canonical = feature.key();
    match feature {
        Feature::WebSearchRequest | Feature::WebSearchCached => {
            let label = match alias {
                "web_search" => "[features].web_search",
                "features.web_search_request" | "web_search_request" => {
                    "[features].web_search_request"
                }
                "features.web_search_cached" | "web_search_cached" => {
                    "[features].web_search_cached"
                }
                _ => alias,
            };
            let summary =
                format!("`{label}` is deprecated because web search is enabled by default.");
            (summary, Some(web_search_details().to_string()))
        }
        Feature::UseLegacyLandlock => {
            let label = match alias {
                "features.use_legacy_landlock" | "use_legacy_landlock" => {
                    "[features].use_legacy_landlock"
                }
                _ => alias,
            };
            let summary = format!("`{label}` is deprecated and will be removed soon.");
            let details =
                "Remove this setting to stop opting into the legacy Linux sandbox behavior."
                    .to_string();
            (summary, Some(details))
        }
        _ => {
            let label = if alias.contains('.') || alias.starts_with('[') {
                alias.to_string()
            } else {
                format!("[features].{alias}")
            };
            let summary = format!("`{label}` is deprecated. Use `[features].{canonical}` instead.");
            let details = if alias == canonical {
                None
            } else {
                Some(format!(
                    "Enable it with `--enable {canonical}` or `[features].{canonical}` in config.toml. See https://developers.openai.com/codex/config-basic#feature-flags for details."
                ))
            };
            (summary, details)
        }
    }
}

fn web_search_details() -> &'static str {
    "Set `web_search` to `\"live\"`, `\"indexed\"`, `\"cached\"`, or `\"disabled\"` at the top level (or under a profile) in config.toml if you want to override it."
}

pub fn feature_for_key(key: &str) -> Option<Feature> {
    canonical_feature_for_key(key).or_else(|| legacy::feature_for_key(key))
}

pub fn canonical_feature_for_key(key: &str) -> Option<Feature> {
    feature_from_canonical_key(key)
}

pub fn is_known_feature_key(key: &str) -> bool {
    feature_for_key(key).is_some()
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
pub struct FeaturesToml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_mode: Option<FeatureToml<CodeModeConfigToml>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_agent_v2: Option<FeatureToml<MultiAgentV2ConfigToml>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_budget: Option<FeatureToml<RolloutBudgetConfigToml>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_time_reminder: Option<FeatureToml<CurrentTimeReminderConfigToml>>,
    #[serde(default, rename = "apps_mcp_path_override", skip_serializing)]
    #[schemars(skip)]
    removed_apps_mcp_path_override: Option<FeatureToml<RemovedAppsMcpPathOverrideConfigToml>>,
    pub network_proxy: Option<FeatureToml<NetworkProxyConfigToml>>,
    #[serde(flatten)]
    entries: BTreeMap<String, bool>,
}

impl Features {
    fn apply_toml(&mut self, features: &FeaturesToml) {
        let entries = features.entries();
        self.apply_map(&entries);
    }
}

impl FeaturesToml {
    pub fn clear_removed_compatibility_entries(&mut self) {
        self.removed_apps_mcp_path_override = None;
        self.entries.remove("apps_mcp_path_override");
    }

    pub fn entries(&self) -> BTreeMap<String, bool> {
        let mut entries = self.entries.clone();
        if let Some(enabled) = self.code_mode.as_ref().and_then(FeatureToml::enabled) {
            entries.insert(Feature::CodeMode.key().to_string(), enabled);
        }
        if let Some(enabled) = self.multi_agent_v2.as_ref().and_then(FeatureToml::enabled) {
            entries.insert(Feature::MultiAgentV2.key().to_string(), enabled);
        }
        if let Some(enabled) = self.rollout_budget.as_ref().and_then(FeatureToml::enabled) {
            entries.insert(Feature::RolloutBudget.key().to_string(), enabled);
        }
        if let Some(enabled) = self
            .current_time_reminder
            .as_ref()
            .and_then(FeatureToml::enabled)
        {
            entries.insert(Feature::CurrentTimeReminder.key().to_string(), enabled);
        }
        if let Some(enabled) = self.network_proxy.as_ref().and_then(FeatureToml::enabled) {
            entries.insert(Feature::NetworkProxy.key().to_string(), enabled);
        }
        entries
    }

    pub fn materialize_resolved_enabled(&mut self, features: &Features) {
        self.clear_removed_compatibility_entries();
        let Self {
            code_mode,
            multi_agent_v2,
            rollout_budget,
            current_time_reminder,
            removed_apps_mcp_path_override: _,
            network_proxy,
            entries,
        } = self;
        for key in legacy::legacy_feature_keys() {
            entries.remove(key);
        }
        for spec in FEATURES {
            let enabled = features.enabled(spec.id);
            if spec.id == Feature::CodeMode {
                materialize_resolved_feature_enabled(code_mode, enabled);
            } else if spec.id == Feature::MultiAgentV2 {
                materialize_resolved_feature_enabled(multi_agent_v2, enabled);
            } else if spec.id == Feature::RolloutBudget {
                materialize_resolved_feature_enabled(rollout_budget, enabled);
            } else if spec.id == Feature::CurrentTimeReminder {
                materialize_resolved_feature_enabled(current_time_reminder, enabled);
            } else if spec.id == Feature::NetworkProxy {
                materialize_resolved_feature_enabled(network_proxy, enabled);
            } else {
                entries.insert(spec.key.to_string(), enabled);
            }
        }
    }
}

fn materialize_resolved_feature_enabled<T: FeatureConfig>(
    feature: &mut Option<FeatureToml<T>>,
    enabled: bool,
) {
    match feature {
        Some(feature) => feature.set_enabled(enabled),
        None => *feature = Some(FeatureToml::Enabled(enabled)),
    }
}

impl From<BTreeMap<String, bool>> for FeaturesToml {
    fn from(entries: BTreeMap<String, bool>) -> Self {
        Self {
            entries,
            ..Default::default()
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(untagged)]
pub enum FeatureToml<T> {
    Enabled(bool),
    Config(T),
}

impl<T: FeatureConfig> FeatureToml<T> {
    pub fn enabled(&self) -> Option<bool> {
        match self {
            Self::Enabled(enabled) => Some(*enabled),
            Self::Config(config) => config.enabled(),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        match self {
            Self::Enabled(value) => *value = enabled,
            Self::Config(config) => config.set_enabled(enabled),
        }
    }
}

pub trait FeatureConfig {
    fn enabled(&self) -> Option<bool>;
    fn set_enabled(&mut self, enabled: bool);
}

#[derive(Debug, Clone, Copy)]
pub struct FeatureSpec {
    pub id: Feature,
    pub key: &'static str,
    pub stage: Stage,
    pub default_enabled: bool,
}

fn feature_from_canonical_key(key: &str) -> Option<Feature> {
    match key {
        "undo" => Some(Feature::GhostCommit),
        "shell_tool" => Some(Feature::ShellTool),
        "secret_auth_storage" => Some(Feature::SecretAuthStorage),
        "unified_exec" => Some(Feature::UnifiedExec),
        "shell_zsh_fork" => Some(Feature::ShellZshFork),
        "unified_exec_zsh_fork" => Some(Feature::UnifiedExecZshFork),
        "shell_snapshot" => Some(Feature::ShellSnapshot),
        "deferred_executor" => Some(Feature::DeferredExecutor),
        "js_repl" => Some(Feature::JsRepl),
        "code_mode" => Some(Feature::CodeMode),
        "code_mode_only" => Some(Feature::CodeModeOnly),
        "js_repl_tools_only" => Some(Feature::JsReplToolsOnly),
        "terminal_resize_reflow" => Some(Feature::TerminalResizeReflow),
        "web_search_request" => Some(Feature::WebSearchRequest),
        "web_search_cached" => Some(Feature::WebSearchCached),
        "standalone_web_search" => Some(Feature::StandaloneWebSearch),
        "search_tool" => Some(Feature::SearchTool),
        "codex_git_commit" => Some(Feature::CodexGitCommit),
        "runtime_metrics" => Some(Feature::RuntimeMetrics),
        "sqlite" => Some(Feature::Sqlite),
        "memories" => Some(Feature::MemoryTool),
        "local_thread_store_compression" => Some(Feature::LocalThreadStoreCompression),
        "chronicle" => Some(Feature::Chronicle),
        "apply_patch_freeform" => Some(Feature::ApplyPatchFreeform),
        "apply_patch_streaming_events" => Some(Feature::ApplyPatchStreamingEvents),
        "exec_permission_approvals" => Some(Feature::ExecPermissionApprovals),
        "hooks" => Some(Feature::CodexHooks),
        "request_permissions_tool" => Some(Feature::RequestPermissionsTool),
        "use_linux_sandbox_bwrap" => Some(Feature::UseLinuxSandboxBwrap),
        "use_legacy_landlock" => Some(Feature::UseLegacyLandlock),
        "request_rule" => Some(Feature::RequestRule),
        "experimental_windows_sandbox" => Some(Feature::WindowsSandbox),
        "elevated_windows_sandbox" => Some(Feature::WindowsSandboxElevated),
        "remote_models" => Some(Feature::RemoteModels),
        "enable_request_compression" => Some(Feature::EnableRequestCompression),
        "network_proxy" => Some(Feature::NetworkProxy),
        "respect_system_proxy" => Some(Feature::RespectSystemProxy),
        "multi_agent" => Some(Feature::Collab),
        "multi_agent_v2" => Some(Feature::MultiAgentV2),
        "multi_agent_mode" => Some(Feature::MultiAgentMode),
        "enable_fanout" => Some(Feature::SpawnCsv),
        "apps" => Some(Feature::Apps),
        "enable_mcp_apps" => Some(Feature::EnableMcpApps),
        "apps_mcp_path_override" => Some(Feature::AppsMcpPathOverride),
        "tool_search" => Some(Feature::ToolSearch),
        "tool_search_always_defer_mcp_tools" => Some(Feature::ToolSearchAlwaysDeferMcpTools),
        "non_prefixed_mcp_tool_names" => Some(Feature::NonPrefixedMcpToolNames),
        "unavailable_dummy_tools" => Some(Feature::UnavailableDummyTools),
        "tool_suggest" => Some(Feature::ToolSuggest),
        "plugins" => Some(Feature::Plugins),
        "plugin_hooks" => Some(Feature::PluginHooks),
        "in_app_browser" => Some(Feature::InAppBrowser),
        "browser_use" => Some(Feature::BrowserUse),
        "browser_use_external" => Some(Feature::BrowserUseExternal),
        "computer_use" => Some(Feature::ComputerUse),
        "remote_plugin" => Some(Feature::RemotePlugin),
        "plugin_sharing" => Some(Feature::PluginSharing),
        "external_migration" => Some(Feature::ExternalMigration),
        "image_generation" => Some(Feature::ImageGeneration),
        "imagegenext" => Some(Feature::ImageGenExt),
        "resize_all_images" => Some(Feature::ResizeAllImages),
        "item_ids" => Some(Feature::ItemIds),
        "skill_mcp_dependency_install" => Some(Feature::SkillMcpDependencyInstall),
        "skill_env_var_dependency_prompt" => Some(Feature::SkillEnvVarDependencyPrompt),
        "mentions_v2" => Some(Feature::MentionsV2),
        "steer" => Some(Feature::Steer),
        "default_mode_request_user_input" => Some(Feature::DefaultModeRequestUserInput),
        "terminal_visualization_instructions" => Some(Feature::TerminalVisualizationInstructions),
        "guardian_approval" => Some(Feature::GuardianApproval),
        "goals" => Some(Feature::Goals),
        "token_budget" => Some(Feature::TokenBudget),
        "rollout_budget" => Some(Feature::RolloutBudget),
        "current_time_reminder" => Some(Feature::CurrentTimeReminder),
        "sleep_tool" => Some(Feature::SleepTool),
        "collaboration_modes" => Some(Feature::CollaborationModes),
        "tool_call_mcp_elicitation" => Some(Feature::ToolCallMcpElicitation),
        "auth_elicitation" => Some(Feature::AuthElicitation),
        "personality" => Some(Feature::Personality),
        "artifact" => Some(Feature::Artifact),
        "fast_mode" => Some(Feature::FastMode),
        "realtime_conversation" => Some(Feature::RealtimeConversation),
        "remote_control" => Some(Feature::RemoteControl),
        "image_detail_original" => Some(Feature::ImageDetailOriginal),
        "tui_app_server" => Some(Feature::TuiAppServer),
        "prevent_idle_sleep" => Some(Feature::PreventIdleSleep),
        "workspace_owner_usage_nudge" => Some(Feature::WorkspaceOwnerUsageNudge),
        "responses_websockets" => Some(Feature::ResponsesWebsockets),
        "responses_websockets_v2" => Some(Feature::ResponsesWebsocketsV2),
        "remote_compaction_v2" => Some(Feature::RemoteCompactionV2),
        "use_agent_identity" => Some(Feature::UseAgentIdentity),
        "workspace_dependencies" => Some(Feature::WorkspaceDependencies),
        _ => None,
    }
}

fn platform_default_override(feature: Feature) -> Option<bool> {
    match feature {
        Feature::SecretAuthStorage => Some(cfg!(windows)),
        Feature::UnifiedExec => Some(!cfg!(windows)),
        _ => None,
    }
}

fn platform_stage_override(feature: Feature, seed_stage: Stage) -> Stage {
    match feature {
        Feature::PreventIdleSleep => {
            if cfg!(any(
                target_os = "macos",
                target_os = "linux",
                target_os = "windows"
            )) {
                seed_stage
            } else {
                Stage::UnderDevelopment
            }
        }
        _ => seed_stage,
    }
}

const ALL_FEATURE_VARIANTS: &[Feature] = &[
    Feature::ShellTool,
    Feature::CodexHooks,
    Feature::SecretAuthStorage,
    Feature::CodeMode,
    Feature::CodeModeOnly,
    Feature::UnifiedExec,
    Feature::ShellZshFork,
    Feature::UnifiedExecZshFork,
    Feature::TerminalResizeReflow,
    Feature::TerminalVisualizationInstructions,
    Feature::ApplyPatchStreamingEvents,
    Feature::ExecPermissionApprovals,
    Feature::RequestPermissionsTool,
    Feature::WebSearchRequest,
    Feature::WebSearchCached,
    Feature::StandaloneWebSearch,
    Feature::UseLegacyLandlock,
    Feature::ShellSnapshot,
    Feature::DeferredExecutor,
    Feature::RuntimeMetrics,
    Feature::MemoryTool,
    Feature::LocalThreadStoreCompression,
    Feature::Chronicle,
    Feature::EnableRequestCompression,
    Feature::NetworkProxy,
    Feature::RespectSystemProxy,
    Feature::Collab,
    Feature::MultiAgentV2,
    Feature::MultiAgentMode,
    Feature::SpawnCsv,
    Feature::Apps,
    Feature::EnableMcpApps,
    Feature::AppsMcpPathOverride,
    Feature::ToolSearch,
    Feature::ToolSearchAlwaysDeferMcpTools,
    Feature::NonPrefixedMcpToolNames,
    Feature::ToolSuggest,
    Feature::Plugins,
    Feature::PluginHooks,
    Feature::InAppBrowser,
    Feature::BrowserUse,
    Feature::BrowserUseExternal,
    Feature::ComputerUse,
    Feature::RemotePlugin,
    Feature::PluginSharing,
    Feature::ExternalMigration,
    Feature::ImageGeneration,
    Feature::ImageGenExt,
    Feature::ResizeAllImages,
    Feature::ItemIds,
    Feature::SkillMcpDependencyInstall,
    Feature::SkillEnvVarDependencyPrompt,
    Feature::MentionsV2,
    Feature::DefaultModeRequestUserInput,
    Feature::GuardianApproval,
    Feature::Goals,
    Feature::TokenBudget,
    Feature::RolloutBudget,
    Feature::CurrentTimeReminder,
    Feature::SleepTool,
    Feature::ToolCallMcpElicitation,
    Feature::AuthElicitation,
    Feature::Personality,
    Feature::Artifact,
    Feature::FastMode,
    Feature::RealtimeConversation,
    Feature::PreventIdleSleep,
    Feature::RemoteCompactionV2,
    Feature::UseAgentIdentity,
    Feature::WorkspaceDependencies,
    Feature::GhostCommit,
    Feature::JsRepl,
    Feature::JsReplToolsOnly,
    Feature::SearchTool,
    Feature::UseLinuxSandboxBwrap,
    Feature::RequestRule,
    Feature::WindowsSandbox,
    Feature::WindowsSandboxElevated,
    Feature::RemoteModels,
    Feature::CodexGitCommit,
    Feature::Sqlite,
    Feature::ApplyPatchFreeform,
    Feature::UnavailableDummyTools,
    Feature::Steer,
    Feature::CollaborationModes,
    Feature::RemoteControl,
    Feature::ImageDetailOriginal,
    Feature::TuiAppServer,
    Feature::WorkspaceOwnerUsageNudge,
    Feature::ResponsesWebsockets,
    Feature::ResponsesWebsocketsV2,
];

fn try_build_features() -> octopus_descriptors::Result<Vec<FeatureSpec>> {
    let loaded = descriptor::load_feature_descriptors_from_disk()?;

    let mut rows = Vec::with_capacity(loaded.len());
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    for (_name, desc) in &loaded {
        let id = feature_from_canonical_key(desc.id.as_str()).ok_or_else(|| {
            octopus_descriptors::DescriptorError::Io(format!(
                "on-disk feature.json key {:?} has no compiled Feature variant — add the variant and its key arms (drift, fail-closed)",
                desc.id.as_str()
            ))
        })?;
        seen.insert(id.key());
        let disk_stage = desc.stage.to_static_stage();
        rows.push(FeatureSpec {
            id,
            key: id.key(),
            stage: platform_stage_override(id, disk_stage),
            default_enabled: platform_default_override(id).unwrap_or(desc.default_enabled),
        });
    }

    for variant in ALL_FEATURE_VARIANTS {
        if !seen.contains(variant.key()) {
            return Err(octopus_descriptors::DescriptorError::Io(format!(
                "compiled Feature::{variant:?} (key {:?}) has no on-disk feature.json descriptor — \
                 add features/{}/feature.json (drift, fail-closed)",
                variant.key(),
                variant.key()
            )));
        }
    }

    Ok(rows)
}

static FEATURES_STORAGE: std::sync::LazyLock<Vec<FeatureSpec>> = std::sync::LazyLock::new(|| {
    try_build_features()
        .unwrap_or_else(|e| panic!("feature registry failed to load from disk (fail-closed): {e}"))
});

pub fn try_feature_registry() -> octopus_descriptors::Result<Vec<FeatureSpec>> {
    try_build_features()
}

#[derive(Clone, Copy, Debug)]
pub struct FeatureRegistry;

impl std::ops::Deref for FeatureRegistry {
    type Target = [FeatureSpec];
    fn deref(&self) -> &'static [FeatureSpec] {
        FEATURES_STORAGE.as_slice()
    }
}

impl IntoIterator for FeatureRegistry {
    type Item = &'static FeatureSpec;
    type IntoIter = std::slice::Iter<'static, FeatureSpec>;
    fn into_iter(self) -> Self::IntoIter {
        FEATURES_STORAGE.as_slice().iter()
    }
}

impl<'a> IntoIterator for &'a FeatureRegistry {
    type Item = &'static FeatureSpec;
    type IntoIter = std::slice::Iter<'static, FeatureSpec>;
    fn into_iter(self) -> Self::IntoIter {
        FEATURES_STORAGE.as_slice().iter()
    }
}

pub const FEATURES: FeatureRegistry = FeatureRegistry;

pub fn unstable_features_warning_event(
    effective_features: Option<&Table>,
    suppress_unstable_features_warning: bool,
    features: &Features,
    config_path: &str,
) -> Option<Event> {
    if suppress_unstable_features_warning {
        return None;
    }

    let mut under_development_feature_keys = Vec::new();
    if let Some(table) = effective_features {
        for (key, value) in table {
            let is_enabled = value.as_bool() == Some(true)
                || value
                    .as_table()
                    .and_then(|table| table.get("enabled"))
                    .and_then(toml::Value::as_bool)
                    == Some(true);
            if !is_enabled {
                continue;
            }
            let Some(spec) = FEATURES.iter().find(|spec| spec.key == key.as_str()) else {
                continue;
            };
            if !features.enabled(spec.id) {
                continue;
            }
            if matches!(spec.stage, Stage::UnderDevelopment) {
                under_development_feature_keys.push(spec.key.to_string());
            }
        }
    }

    if under_development_feature_keys.is_empty() {
        return None;
    }

    under_development_feature_keys.sort();
    let under_development_feature_keys = under_development_feature_keys.join(", ");
    let message = format!(
        "Under-development features enabled: {under_development_feature_keys}. Under-development features are incomplete and may behave unpredictably. To suppress this warning, set `suppress_unstable_features_warning = true` in {config_path}."
    );
    Some(Event {
        id: String::new(),
        msg: EventMsg::Warning(WarningEvent { message }),
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "descriptor_tests.rs"]
mod descriptor_tests;
