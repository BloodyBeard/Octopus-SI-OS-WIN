use std::collections::HashMap;
use std::collections::HashSet;

use crate::connector_ids::AppConnectorId;
use octopus_appserver_proto::AppInfo;
use octopus_core::config::Config;
use octopus_login::AuthManager;
use octopus_login::CodexAuth;

#[derive(Debug, Clone)]
pub struct AccessibleConnectorsStatus {
    pub connectors: Vec<AppInfo>,
    pub codex_apps_ready: bool,
}

async fn apps_enabled(config: &Config) -> bool {
    let auth_manager = AuthManager::shared_from_config(config, false).await;
    let auth = auth_manager.auth().await;
    config
        .features
        .apps_enabled_for_auth(auth.as_ref().is_some_and(CodexAuth::uses_codex_backend))
}

fn connector_runtime_error() -> anyhow::Error {
    anyhow::anyhow!(
        "ChatGPT connector catalog discovery is not runtime truth in Octopus. Capabilities must be \
         concrete interface descriptors plus durable artifact/binding/policy evidence."
    )
}

pub async fn list_connectors(config: &Config) -> anyhow::Result<Vec<AppInfo>> {
    if !apps_enabled(config).await {
        return Ok(Vec::new());
    }
    Err(connector_runtime_error())
}

pub async fn list_accessible_connectors_from_mcp_tools(
    config: &Config,
) -> anyhow::Result<Vec<AppInfo>> {
    Ok(
        list_accessible_connectors_from_mcp_tools_with_options_and_status(config, false)
            .await?
            .connectors,
    )
}

pub async fn list_accessible_connectors_from_mcp_tools_with_options(
    config: &Config,
    force_refetch: bool,
) -> anyhow::Result<Vec<AppInfo>> {
    Ok(
        list_accessible_connectors_from_mcp_tools_with_options_and_status(config, force_refetch)
            .await?
            .connectors,
    )
}

pub async fn list_accessible_connectors_from_mcp_tools_with_options_and_status(
    config: &Config,
    force_refetch: bool,
) -> anyhow::Result<AccessibleConnectorsStatus> {
    let _ = force_refetch;
    if !apps_enabled(config).await {
        return Ok(AccessibleConnectorsStatus {
            connectors: Vec::new(),
            codex_apps_ready: true,
        });
    }
    Err(connector_runtime_error())
}

pub async fn list_cached_accessible_connectors_from_mcp_tools(
    config: &Config,
) -> Option<Vec<AppInfo>> {
    if !apps_enabled(config).await {
        return Some(Vec::new());
    }
    None
}

pub async fn list_all_connectors(config: &Config) -> anyhow::Result<Vec<AppInfo>> {
    list_all_connectors_with_options(config, false, &[]).await
}

pub async fn list_cached_all_connectors(
    config: &Config,
    connector_app_ids: &[AppConnectorId],
) -> Option<Vec<AppInfo>> {
    let _ = connector_app_ids;
    if !apps_enabled(config).await {
        return Some(Vec::new());
    }
    None
}

pub async fn list_all_connectors_with_options(
    config: &Config,
    force_refetch: bool,
    connector_app_ids: &[AppConnectorId],
) -> anyhow::Result<Vec<AppInfo>> {
    let _ = (force_refetch, connector_app_ids);
    if !apps_enabled(config).await {
        return Ok(Vec::new());
    }
    Err(connector_runtime_error())
}

pub fn connectors_for_connector_app_ids(
    connectors: Vec<AppInfo>,
    connector_app_ids: &[AppConnectorId],
) -> Vec<AppInfo> {
    let mut connectors_by_id = connectors
        .into_iter()
        .map(|connector| (connector.id.clone(), connector))
        .collect::<HashMap<_, _>>();

    connector_app_ids
        .iter()
        .filter_map(|connector_id| connectors_by_id.remove(connector_id.0.as_str()))
        .collect()
}

pub fn merge_connectors_with_accessible(
    connectors: Vec<AppInfo>,
    accessible_connectors: Vec<AppInfo>,
    all_connectors_loaded: bool,
) -> Vec<AppInfo> {
    let accessible_connectors = if all_connectors_loaded {
        let connector_ids: HashSet<&str> = connectors
            .iter()
            .map(|connector| connector.id.as_str())
            .collect();
        accessible_connectors
            .into_iter()
            .filter(|connector| connector_ids.contains(connector.id.as_str()))
            .collect()
    } else {
        accessible_connectors
    };

    merge_app_info(connectors, accessible_connectors)
}

pub fn with_app_enabled_state(connectors: Vec<AppInfo>, config: &Config) -> Vec<AppInfo> {
    let _ = config;
    connectors
}

fn merge_app_info(connectors: Vec<AppInfo>, accessible_connectors: Vec<AppInfo>) -> Vec<AppInfo> {
    let mut merged: HashMap<String, AppInfo> = connectors
        .into_iter()
        .map(|mut connector| {
            connector.is_accessible = false;
            (connector.id.clone(), connector)
        })
        .collect();

    for mut connector in accessible_connectors {
        connector.is_accessible = true;
        if let Some(existing) = merged.get_mut(&connector.id) {
            existing.is_accessible = true;
            if existing.name == existing.id && connector.name != connector.id {
                existing.name = connector.name;
            }
            if existing.description.is_none() && connector.description.is_some() {
                existing.description = connector.description;
            }
            if existing.logo_url.is_none() && connector.logo_url.is_some() {
                existing.logo_url = connector.logo_url;
            }
            if existing.logo_url_dark.is_none() && connector.logo_url_dark.is_some() {
                existing.logo_url_dark = connector.logo_url_dark;
            }
            if existing.distribution_channel.is_none() && connector.distribution_channel.is_some() {
                existing.distribution_channel = connector.distribution_channel;
            }
            existing
                .plugin_display_names
                .extend(connector.plugin_display_names);
            existing.plugin_display_names.sort_unstable();
            existing.plugin_display_names.dedup();
        } else {
            merged.insert(connector.id.clone(), connector);
        }
    }

    let mut merged = merged.into_values().collect::<Vec<_>>();
    merged.sort_by(|left, right| {
        right
            .is_accessible
            .cmp(&left.is_accessible)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    merged
}
