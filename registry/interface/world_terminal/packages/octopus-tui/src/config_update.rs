use crate::provider_config_runtime::ProviderConfigRuntime;
use crate::provider_config_runtime::SettingEdit;
use crate::session_vocab::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use color_eyre::eyre::Result;
use serde_json::Value as JsonValue;
use std::fmt::Display;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConfigEdit {
    pub(crate) key_path: String,
    pub(crate) value: JsonValue,
}

pub(crate) fn replace_config_value(key_path: impl Into<String>, value: JsonValue) -> ConfigEdit {
    ConfigEdit {
        key_path: key_path.into(),
        value,
    }
}

pub(crate) fn clear_config_value(key_path: impl Into<String>) -> ConfigEdit {
    replace_config_value(key_path, JsonValue::Null)
}

pub(crate) fn format_config_error(err: &impl Display) -> String {
    format!("{err:#}")
}

pub(crate) fn build_model_selection_edits(
    model: &str,
    effort: Option<impl ToString>,
) -> Vec<ConfigEdit> {
    let effort_edit = effort.map_or_else(
        || clear_config_value("model_reasoning_effort"),
        |effort| {
            replace_config_value(
                "model_reasoning_effort",
                serde_json::json!(effort.to_string()),
            )
        },
    );
    vec![
        replace_config_value("model", serde_json::json!(model)),
        effort_edit,
    ]
}

pub(crate) fn build_service_tier_selection_edits(service_tier: Option<&str>) -> Vec<ConfigEdit> {
    let service_tier_edit = service_tier.map_or_else(
        || clear_config_value("service_tier"),
        |service_tier| {
            let config_value = if service_tier == SERVICE_TIER_DEFAULT_REQUEST_VALUE {
                SERVICE_TIER_DEFAULT_REQUEST_VALUE
            } else {
                match crate::session_vocab::config_types::ServiceTier::from_request_value(
                    service_tier,
                ) {
                    Some(crate::session_vocab::config_types::ServiceTier::Fast) => "fast",
                    Some(crate::session_vocab::config_types::ServiceTier::Flex) => "flex",
                    None => service_tier,
                }
            };
            replace_config_value("service_tier", serde_json::json!(config_value))
        },
    );
    vec![service_tier_edit]
}

#[cfg(target_os = "windows")]
pub(crate) fn build_windows_sandbox_mode_edits(elevated_enabled: bool) -> Vec<ConfigEdit> {
    let feature_key_path = |feature: &str| format!("features.{feature}");
    vec![
        replace_config_value(
            "windows.sandbox",
            serde_json::json!(if elevated_enabled {
                "elevated"
            } else {
                "unelevated"
            }),
        ),
        clear_config_value(feature_key_path("experimental_windows_sandbox")),
        clear_config_value(feature_key_path("elevated_windows_sandbox")),
        clear_config_value(feature_key_path("enable_experimental_windows_sandbox")),
    ]
}

pub(crate) fn build_feature_enabled_edit(
    feature_key: &str,
    enabled: bool,
    default_enabled: Option<bool>,
) -> ConfigEdit {
    let key_path = format!("features.{feature_key}");
    let is_default_false_feature = default_enabled == Some(false);
    if enabled || !is_default_false_feature {
        replace_config_value(key_path, serde_json::json!(enabled))
    } else {
        clear_config_value(key_path)
    }
}

pub(crate) fn build_memory_settings_edits(
    use_memories: bool,
    generate_memories: bool,
) -> Vec<ConfigEdit> {
    vec![
        replace_config_value("memories.use_memories", serde_json::json!(use_memories)),
        replace_config_value(
            "memories.generate_memories",
            serde_json::json!(generate_memories),
        ),
    ]
}

pub(crate) fn build_oss_provider_edit(provider: &str) -> ConfigEdit {
    replace_config_value("oss_provider", serde_json::json!(provider))
}

pub(crate) async fn write_config_batch_local(
    config_runtime: &dyn ProviderConfigRuntime,
    edits: Vec<ConfigEdit>,
) -> Result<()> {
    let edits = edits
        .into_iter()
        .map(|edit| SettingEdit {
            key_path: edit.key_path,
            value: edit.value,
        })
        .collect();
    config_runtime
        .persist_edits(edits)
        .await
        .map_err(|err| color_eyre::eyre::eyre!("failed to persist local TUI config file: {err:#}"))
}

pub(crate) fn write_trusted_project_local(
    config_runtime: &dyn ProviderConfigRuntime,
    codex_home: &Path,
    project_path: &Path,
) -> Result<()> {
    config_runtime
        .set_project_trust(codex_home, project_path)
        .map_err(|err| {
            color_eyre::eyre::eyre!(
                "failed to persist trusted project state under {}: {err:#}",
                codex_home.display()
            )
        })
}

#[cfg(test)]
#[path = "config_update_tests.rs"]
mod tests;
