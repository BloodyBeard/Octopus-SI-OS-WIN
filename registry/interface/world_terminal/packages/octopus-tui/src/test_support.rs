use std::path::Path;
use std::sync::LazyLock;

const CONFIG_TOML_FILE: &str = "config.toml";
use crate::session_vocab::config_types::ReasoningSummary;
use crate::session_vocab::config_types::ServiceTier;
use crate::session_vocab::openai_models::ConfigShellToolType;
use crate::session_vocab::openai_models::ModelInfo;
use crate::session_vocab::openai_models::ModelInstructionsVariables;
use crate::session_vocab::openai_models::ModelMessages;
use crate::session_vocab::openai_models::ModelPreset;
use crate::session_vocab::openai_models::ModelServiceTier;
use crate::session_vocab::openai_models::ModelVisibility;
use crate::session_vocab::openai_models::ReasoningEffort;
use crate::session_vocab::openai_models::ReasoningEffortPreset;
use crate::session_vocab::openai_models::TruncationPolicyConfig;
use crate::session_vocab::openai_models::WebSearchToolType;
pub(crate) use octopus_util_abspath::test_support::PathBufExt;
pub(crate) use octopus_util_abspath::test_support::test_path_buf;
use serde::Serialize;
use serde::de::DeserializeOwned;
use toml_edit::DocumentMut;

pub(crate) static TEST_MODEL_PRESETS: LazyLock<Vec<ModelPreset>> = LazyLock::new(|| {
    let mut presets: Vec<ModelPreset> = vec![
        construct_model_info_offline_for_tests("gpt-5.2-codex", &()).into(),
        construct_model_info_offline_for_tests("gpt-5.2", &()).into(),
        construct_model_info_offline_for_tests("gpt-5.3-codex", &()).into(),
        construct_model_info_offline_for_tests("gpt-5.4", &()).into(),
        construct_model_info_offline_for_tests("gpt-5.4-mini", &()).into(),
    ];
    ModelPreset::mark_default_by_picker_visibility(&mut presets);
    presets
});

pub(crate) fn test_collaboration_modes()
-> Vec<crate::session_vocab::config_types::CollaborationModeMask> {
    let registry = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let modes = crate::collaboration_modes::load_presets(&registry);
    assert_eq!(
        modes.len(),
        2,
        "native collaboration-mode artifacts must be present"
    );
    modes
}

pub(crate) const TEST_MODEL_PROVIDER_CONFIG: &str = r#"model_provider = "local-test"

[model_providers.local-test]
name = "local-test"
base_url = "http://127.0.0.1:8061/api/codex"
wire_api = "responses"
requires_openai_auth = false
"#;

pub(crate) fn write_test_model_provider_config(codex_home: &Path) {
    write_test_config(codex_home, "");
}

pub(crate) fn write_test_config(codex_home: &Path, body: &str) {
    std::fs::create_dir_all(codex_home).expect("create test codex home");
    let config = test_config_contents(body);
    std::fs::write(codex_home.join(CONFIG_TOML_FILE), config).expect("write test config");
}

pub(crate) fn test_config_contents(body: &str) -> String {
    let mut config = if body.trim().is_empty() {
        DocumentMut::new()
    } else {
        body.parse::<DocumentMut>()
            .expect("test config body should be valid TOML")
    };
    let provider_config = TEST_MODEL_PROVIDER_CONFIG
        .parse::<DocumentMut>()
        .expect("test model provider config should be valid TOML");

    for (key, value) in provider_config.as_table().iter() {
        config[key] = value.clone();
    }

    config.to_string()
}

pub(crate) fn get_model_offline_for_tests(model: Option<&str>) -> String {
    model
        .filter(|model| !model.trim().is_empty())
        .unwrap_or("gpt-5.2-codex")
        .to_string()
}

pub(crate) fn construct_model_info_offline_for_tests<T>(
    model: &str,
    _provider_projection: &T,
) -> ModelInfo {
    let mut service_tiers = Vec::new();
    if model == "gpt-5.4" {
        service_tiers.push(ModelServiceTier {
            id: ServiceTier::Fast.request_value().to_string(),
            name: "fast".to_string(),
            description: "Fastest inference with increased plan usage".to_string(),
        });
    }

    ModelInfo {
        slug: model.to_string(),
        display_name: model.to_string(),
        description: Some("test model".to_string()),
        default_reasoning_level: Some(ReasoningEffort::Medium),
        supported_reasoning_levels: vec![
            ReasoningEffortPreset {
                effort: ReasoningEffort::Low,
                description: "Fast responses with lighter reasoning".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::Medium,
                description: "Balances speed and reasoning depth for everyday tasks".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::High,
                description: "Greater reasoning depth for complex problems".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::XHigh,
                description: "Extra high reasoning depth for complex problems".to_string(),
            },
        ],
        shell_type: ConfigShellToolType::Default,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority: 0,
        additional_speed_tiers: Vec::new(),
        service_tiers,
        default_service_tier: None,
        availability_nux: None,
        upgrade: None,
        base_instructions: String::new(),
        model_messages: Some(ModelMessages {
            instructions_template: Some("base instructions\n{{ personality }}".to_string()),
            instructions_variables: Some(ModelInstructionsVariables {
                personality_default: Some("default personality".to_string()),
                personality_friendly: Some("friendly personality".to_string()),
                personality_pragmatic: Some("pragmatic personality".to_string()),
            }),
        }),
        supports_reasoning_summaries: true,
        default_reasoning_summary: ReasoningSummary::Auto,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        web_search_tool_type: WebSearchToolType::Text,
        truncation_policy: TruncationPolicyConfig::tokens(128_000),
        supports_parallel_tool_calls: true,
        supports_image_detail_original: false,
        context_window: Some(128_000),
        max_context_window: None,
        auto_compact_token_limit: None,
        comp_hash: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
        input_modalities: crate::session_vocab::openai_models::default_input_modalities(),
        used_fallback_model_metadata: false,
        supports_search_tool: false,
        use_responses_lite: false,
        auto_review_model_override: None,
        tool_mode: None,
        multi_agent_version: None,
    }
}

pub(crate) fn test_path_display(path: &str) -> String {
    test_path_buf(path).display().to_string()
}

pub(crate) fn session_source_cli<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(serde_json::json!("cli"))
}

pub(crate) fn skill_scope_user<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(serde_json::json!("user"))
}

pub(crate) fn skill_scope_repo<T>() -> T
where
    T: DeserializeOwned,
{
    from_app_server_wire(serde_json::json!("repo"))
}

fn from_app_server_wire<T>(value: impl Serialize) -> T
where
    T: DeserializeOwned,
{
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .unwrap_or_else(|err| {
            panic!("app-server wire value should map to legacy helper type: {err}")
        })
}

type SnapshotTweak =
    Box<dyn Fn(&mut crate::provider_config_runtime::ProviderSessionSnapshot) + Send + Sync>;

pub(crate) struct TestProviderConfigRuntime {
    pub(crate) trust_writes: std::sync::Mutex<Vec<(std::path::PathBuf, std::path::PathBuf)>>,
    pub(crate) persisted_edits: std::sync::Mutex<Vec<crate::provider_config_runtime::SettingEdit>>,
    snapshot_tweak: std::sync::Mutex<Option<std::sync::Arc<SnapshotTweak>>>,
}

impl TestProviderConfigRuntime {
    pub(crate) fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            trust_writes: std::sync::Mutex::new(Vec::new()),
            persisted_edits: std::sync::Mutex::new(Vec::new()),
            snapshot_tweak: std::sync::Mutex::new(None),
        })
    }

    pub(crate) fn set_snapshot_tweak(
        &self,
        tweak: impl Fn(&mut crate::provider_config_runtime::ProviderSessionSnapshot)
        + Send
        + Sync
        + 'static,
    ) {
        *self.snapshot_tweak.lock().expect("test runtime lock") =
            Some(std::sync::Arc::new(Box::new(tweak)));
    }
}

impl crate::provider_config_runtime::ProviderConfigRuntime for TestProviderConfigRuntime {
    fn load(
        &self,
        request: crate::provider_config_runtime::SessionLoadRequest,
    ) -> crate::provider_config_runtime::BoxFuture<
        std::io::Result<crate::provider_config_runtime::ProviderSessionSnapshot>,
    > {
        let tweak = self
            .snapshot_tweak
            .lock()
            .expect("test runtime lock")
            .clone();
        Box::pin(async move {
            let codex_home = match request.codex_home.clone() {
                Some(home) => octopus_util_abspath::AbsolutePathBuf::from_absolute_path(home)?,
                None => octopus_utils_home_dir::find_octopus_home()?,
            };
            let cwd = match request.harness.cwd.clone().or(request.fallback_cwd.clone()) {
                Some(cwd) => octopus_util_abspath::AbsolutePathBuf::from_absolute_path(cwd)?,
                None => octopus_util_abspath::AbsolutePathBuf::current_dir()?,
            };
            let config_path = codex_home.join(CONFIG_TOML_FILE).to_path_buf();
            let effective_config = match std::fs::read_to_string(&config_path) {
                Ok(raw) => toml::from_str::<toml::Value>(&raw).map_err(|err| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("config.toml failed to parse: {err}"),
                    )
                })?,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    toml::Value::Table(toml::map::Map::new())
                }
                Err(err) => return Err(err),
            };
            let mut snapshot =
                crate::provider_config_runtime::ProviderSessionSnapshot::degraded(codex_home, cwd);
            snapshot.effective_config = effective_config;
            snapshot.model = request.harness.model.clone();
            snapshot.bypass_hook_trust = request.harness.bypass_hook_trust.unwrap_or(false);
            if let Some(tweak) = tweak {
                tweak(&mut snapshot);
            }
            Ok(snapshot)
        })
    }

    fn persist_edits(
        &self,
        edits: Vec<crate::provider_config_runtime::SettingEdit>,
    ) -> crate::provider_config_runtime::BoxFuture<std::io::Result<()>> {
        self.persisted_edits
            .lock()
            .expect("test runtime lock")
            .extend(edits);
        Box::pin(async move { Ok(()) })
    }

    fn set_project_trust(
        &self,
        codex_home: &std::path::Path,
        project_path: &std::path::Path,
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(codex_home)?;
        let config_path = codex_home.join(CONFIG_TOML_FILE);
        let mut raw = std::fs::read_to_string(&config_path).unwrap_or_default();
        raw.push_str(&format!(
            "\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            project_path.display()
        ));
        std::fs::write(&config_path, raw)?;
        self.trust_writes
            .lock()
            .expect("test runtime lock")
            .push((codex_home.to_path_buf(), project_path.to_path_buf()));
        Ok(())
    }

    fn debug_report(
        &self,
        _session_proxy: Option<crate::provider_config_runtime::SessionProxyAddrs>,
    ) -> Vec<crate::provider_config_runtime::DebugReportLine> {
        Vec::new()
    }

    fn resolve_oss_provider(
        &self,
        requested: Option<String>,
    ) -> crate::provider_config_runtime::BoxFuture<Option<String>> {
        Box::pin(async move { requested })
    }

    fn execpolicy_warnings(&self) -> crate::provider_config_runtime::BoxFuture<Result<(), String>> {
        Box::pin(async move { Ok(()) })
    }

    fn telemetry_layers(
        &self,
        _originator: &str,
    ) -> crate::provider_config_runtime::TelemetryLayerBundle {
        crate::provider_config_runtime::TelemetryLayerBundle::default()
    }
}

pub(crate) fn test_model_provider_info() -> crate::session_vocab::provider_card::TerminalProviderCard
{
    crate::session_vocab::provider_card::TerminalProviderCard {
        name: "local-test".to_string(),
        base_url: Some("http://127.0.0.1:8061/api/codex".to_string()),
        requires_openai_auth: false,
        ..Default::default()
    }
}

pub(crate) fn test_session_snapshot(
    codex_home: octopus_util_abspath::AbsolutePathBuf,
    cwd: octopus_util_abspath::AbsolutePathBuf,
) -> crate::provider_config_runtime::ProviderSessionSnapshot {
    let mut snapshot =
        crate::provider_config_runtime::ProviderSessionSnapshot::degraded(codex_home, cwd.clone());
    snapshot.model_provider_id = "local-test".to_string();
    snapshot.model_provider = test_model_provider_info();
    snapshot.features = test_session_features();
    snapshot.personality = Some(crate::session_vocab::config_types::Personality::Pragmatic);
    snapshot.project_trust_level = None;
    snapshot.workspace_roots = vec![cwd.clone()];
    snapshot.permissions = crate::session_posture::SessionPermissions::new(
        crate::session_posture::ConstrainedValue::unconstrained(
            octopus_substrate_contract::approvals_substrate::AskForApproval::OnRequest,
        ),
        crate::session_posture::SessionPermissionSnapshot::active(
            octopus_substrate_contract::permission_profile::PermissionProfile::read_only(),
            octopus_substrate_contract::permission_profile::ActivePermissionProfile::read_only(),
        ),
        crate::session_posture::ProfileClassVerdicts::default(),
        vec![cwd],
        None,
    );
    snapshot.startup_warnings.clear();
    snapshot
}

pub(crate) fn test_session_features() -> crate::session_posture::SessionFeatures {
    use crate::session_posture::SessionFeatureRow;
    let row = |key: &str, default_enabled: bool| SessionFeatureRow {
        key: key.to_string(),
        enabled: default_enabled,
        default_enabled,
        ..Default::default()
    };
    crate::session_posture::SessionFeatures::from_rows(vec![
        row("guardian_approval", true),
        row("collaboration_modes", true),
        row("goals", true),
        row("fast_mode", true),
        SessionFeatureRow {
            key: "memories".to_string(),
            enabled: false,
            default_enabled: false,
            menu_name: Some("Memories".to_string()),
            menu_description: Some(
                "Allow Codex to create new memories from conversations and bring relevant \
                 memories into new conversations."
                    .to_string(),
            ),
            announcement: Some(
                "NEW: Codex can now generate and use memories. Try it now with `/memories`"
                    .to_string(),
            ),
            ..Default::default()
        },
        row("experimental_windows_sandbox", false),
        row("elevated_windows_sandbox", false),
        row("personality", true),
        SessionFeatureRow {
            key: "prevent_idle_sleep".to_string(),
            enabled: false,
            default_enabled: false,
            menu_name: Some("Prevent sleep while running".to_string()),
            menu_description: Some(
                "Keep your computer awake while Codex is running a thread.".to_string(),
            ),
            announcement: Some(
                "NEW: Prevent sleep while running is now available in /experimental.".to_string(),
            ),
            ..Default::default()
        },
        row("runtime_metrics", false),
        row("mentions_v2", true),
        row("steer", true),
    ])
}
