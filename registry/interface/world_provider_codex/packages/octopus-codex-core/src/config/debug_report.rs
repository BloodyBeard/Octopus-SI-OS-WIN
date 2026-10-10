use octopus_config::CONFIG_TOML_FILE;
use octopus_config::ConfigLayerEntry;
use octopus_config::ConfigLayerSource;
use octopus_config::ConfigLayerStack;
use octopus_config::ConfigLayerStackOrdering;
use octopus_config::NetworkConstraints;
use octopus_config::NetworkDomainPermissionToml;
use octopus_config::NetworkUnixSocketPermissionToml;
use octopus_config::RequirementSource;
use octopus_config::ResidencyRequirement;
use octopus_config::SandboxModeRequirement;
use octopus_config::WebSearchModeRequirement;
use octopus_config::format_config_layer_source;
use octopus_protocol::models::PermissionProfile;
use octopus_protocol::permissions::NetworkSandboxPolicy;
use toml::Value as TomlValue;

use super::Config;
use super::NetworkProxySpec;
use super::Permissions;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugConfigReportLineKind {
    CommandEcho,
    SectionHeader,
    Text,
    Muted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugConfigReportLine {
    pub kind: DebugConfigReportLineKind,
    pub text: String,
}

impl DebugConfigReportLine {
    fn command_echo(text: impl Into<String>) -> Self {
        Self {
            kind: DebugConfigReportLineKind::CommandEcho,
            text: text.into(),
        }
    }

    fn section_header(text: impl Into<String>) -> Self {
        Self {
            kind: DebugConfigReportLineKind::SectionHeader,
            text: text.into(),
        }
    }

    fn text(text: impl Into<String>) -> Self {
        Self {
            kind: DebugConfigReportLineKind::Text,
            text: text.into(),
        }
    }

    fn muted(text: impl Into<String>) -> Self {
        Self {
            kind: DebugConfigReportLineKind::Muted,
            text: text.into(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SessionNetworkProxyAddrs<'a> {
    pub http_addr: &'a str,
    pub socks_addr: &'a str,
}

pub fn debug_config_report(
    config: &Config,
    session_network_proxy: Option<SessionNetworkProxyAddrs<'_>>,
) -> Vec<DebugConfigReportLine> {
    let mut lines = render_debug_config_lines(&config.config_layer_stack, |mode| {
        sandbox_mode_is_allowed_by_permissions(&config.permissions, mode)
    });

    if let Some(proxy) = session_network_proxy {
        lines.push(DebugConfigReportLine::text(""));
        lines.push(DebugConfigReportLine::section_header("Session runtime:"));
        lines.push(DebugConfigReportLine::text("  - network_proxy"));
        let SessionNetworkProxyAddrs {
            http_addr,
            socks_addr,
        } = proxy;
        let all_proxy = session_all_proxy_url(
            http_addr,
            socks_addr,
            config
                .permissions
                .network
                .as_ref()
                .is_some_and(NetworkProxySpec::socks_enabled),
        );
        lines.push(DebugConfigReportLine::text(format!(
            "    - HTTP_PROXY  = http://{http_addr}"
        )));
        lines.push(DebugConfigReportLine::text(format!(
            "    - ALL_PROXY   = {all_proxy}"
        )));
    }

    lines
}

fn sandbox_mode_is_allowed_by_permissions(
    permissions: &Permissions,
    mode: SandboxModeRequirement,
) -> bool {
    let permission_profile = match mode {
        SandboxModeRequirement::ReadOnly => PermissionProfile::read_only(),
        SandboxModeRequirement::WorkspaceWrite => PermissionProfile::workspace_write(),
        SandboxModeRequirement::DangerFullAccess => PermissionProfile::Disabled,
        SandboxModeRequirement::ExternalSandbox => PermissionProfile::External {
            network: NetworkSandboxPolicy::Restricted,
        },
    };

    permissions
        .can_set_permission_profile(&permission_profile)
        .is_ok()
}

fn session_all_proxy_url(http_addr: &str, socks_addr: &str, socks_enabled: bool) -> String {
    if socks_enabled {
        format!("socks5h://{socks_addr}")
    } else {
        format!("http://{http_addr}")
    }
}

fn render_debug_config_lines(
    stack: &ConfigLayerStack,
    sandbox_mode_is_effectively_allowed: impl Fn(SandboxModeRequirement) -> bool,
) -> Vec<DebugConfigReportLine> {
    let mut lines = vec![
        DebugConfigReportLine::command_echo("/debug-config"),
        DebugConfigReportLine::text(""),
    ];

    lines.push(DebugConfigReportLine::section_header(
        "Config layer stack (lowest precedence first):",
    ));
    let layers = stack.get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, true);
    if layers.is_empty() {
        lines.push(DebugConfigReportLine::muted("  <none>"));
    } else {
        for (index, layer) in layers.iter().enumerate() {
            let source = format_config_layer_source(&layer.name, CONFIG_TOML_FILE);
            let status = if layer.is_disabled() {
                "disabled"
            } else {
                "enabled"
            };
            lines.push(DebugConfigReportLine::text(format!(
                "  {}. {source} ({status})",
                index + 1
            )));
            lines.extend(render_non_file_layer_details(layer));
            if let Some(reason) = &layer.disabled_reason {
                lines.push(DebugConfigReportLine::muted(format!(
                    "     reason: {reason}"
                )));
            }
        }
    }

    let requirements = stack.requirements();
    let requirements_toml = stack.requirements_toml();

    lines.push(DebugConfigReportLine::text(""));
    lines.push(DebugConfigReportLine::section_header("Requirements:"));
    let mut requirement_lines = Vec::new();

    if let Some(policies) = requirements_toml.allowed_approval_policies.as_ref() {
        let value = join_or_empty(policies.iter().map(ToString::to_string).collect::<Vec<_>>());
        requirement_lines.push(requirement_line(
            "allowed_approval_policies",
            value,
            requirements.approval_policy.source.as_ref(),
        ));
    }

    if let Some(reviewers) = requirements_toml.allowed_approvals_reviewers.as_ref() {
        let value = join_or_empty(
            reviewers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        );
        requirement_lines.push(requirement_line(
            "allowed_approvals_reviewers",
            value,
            requirements.approvals_reviewer.source.as_ref(),
        ));
    }

    if let Some(modes) = requirements_toml.allowed_sandbox_modes.as_ref() {
        let value = join_or_empty(
            modes
                .iter()
                .copied()
                .filter(|mode| sandbox_mode_is_effectively_allowed(*mode))
                .map(format_sandbox_mode_requirement)
                .collect::<Vec<_>>(),
        );
        requirement_lines.push(requirement_line(
            "allowed_sandbox_modes",
            value,
            requirements.permission_profile.source.as_ref(),
        ));
    }

    if let Some(modes) = requirements_toml.allowed_web_search_modes.as_ref() {
        let normalized = normalize_allowed_web_search_modes(modes);
        let value = join_or_empty(
            normalized
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        );
        requirement_lines.push(requirement_line(
            "allowed_web_search_modes",
            value,
            requirements.web_search_mode.source.as_ref(),
        ));
    }

    if let Some(allow_appshots) = requirements_toml.allow_appshots {
        requirement_lines.push(requirement_line(
            "allow_appshots",
            allow_appshots.to_string(),
            requirements
                .allow_appshots
                .as_ref()
                .map(|sourced| &sourced.source),
        ));
    }

    if let Some(allow_remote_control) = requirements_toml.allow_remote_control {
        requirement_lines.push(requirement_line(
            "allow_remote_control",
            allow_remote_control.to_string(),
            requirements
                .allow_remote_control
                .as_ref()
                .map(|sourced| &sourced.source),
        ));
    }

    if requirements_toml.guardian_policy_config.is_some() {
        requirement_lines.push(requirement_line(
            "guardian_policy_config",
            "configured".to_string(),
            requirements.guardian_policy_config_source.as_ref(),
        ));
    }

    if let Some(feature_requirements) = requirements.feature_requirements.as_ref() {
        let value = join_or_empty(
            feature_requirements
                .value
                .entries
                .iter()
                .map(|(feature, enabled)| format!("{feature}={enabled}"))
                .collect::<Vec<_>>(),
        );
        requirement_lines.push(requirement_line(
            "features",
            value,
            Some(&feature_requirements.source),
        ));
    }

    if let Some(servers) = requirements_toml.mcp_servers.as_ref() {
        let value = join_or_empty(servers.keys().cloned().collect::<Vec<_>>());
        requirement_lines.push(requirement_line(
            "mcp_servers",
            value,
            requirements
                .mcp_servers
                .as_ref()
                .map(|sourced| &sourced.source),
        ));
    }

    if requirements_toml.rules.is_some() {
        requirement_lines.push(requirement_line(
            "rules",
            "configured".to_string(),
            requirements.exec_policy_source(),
        ));
    }

    if let Some(residency) = requirements_toml.enforce_residency {
        requirement_lines.push(requirement_line(
            "enforce_residency",
            format_residency_requirement(residency),
            requirements.enforce_residency.source.as_ref(),
        ));
    }

    if let Some(network) = requirements.network.as_ref() {
        requirement_lines.push(requirement_line(
            "experimental_network",
            format_network_constraints(&network.value),
            Some(&network.source),
        ));
    }

    if let Some(filesystem) = requirements.filesystem.as_ref() {
        let deny_read = join_or_empty(
            filesystem
                .value
                .deny_read
                .iter()
                .map(|pattern| pattern.as_str().to_string())
                .collect::<Vec<_>>(),
        );
        requirement_lines.push(requirement_line(
            "permissions.filesystem.deny_read",
            deny_read,
            Some(&filesystem.source),
        ));
    }

    if requirement_lines.is_empty() {
        lines.push(DebugConfigReportLine::muted("  <none>"));
    } else {
        lines.extend(requirement_lines);
    }

    lines
}

fn render_non_file_layer_details(layer: &ConfigLayerEntry) -> Vec<DebugConfigReportLine> {
    match &layer.name {
        ConfigLayerSource::SessionFlags => render_session_flag_details(&layer.config),
        ConfigLayerSource::Mdm { .. }
        | ConfigLayerSource::EnterpriseManaged { .. }
        | ConfigLayerSource::LegacyManagedConfigTomlFromMdm => render_non_file_layer_value(layer),
        ConfigLayerSource::System { .. }
        | ConfigLayerSource::User { .. }
        | ConfigLayerSource::Project { .. }
        | ConfigLayerSource::LegacyManagedConfigTomlFromFile { .. } => Vec::new(),
    }
}

fn render_session_flag_details(config: &TomlValue) -> Vec<DebugConfigReportLine> {
    let mut pairs = Vec::new();
    flatten_toml_key_values(config, None, &mut pairs);

    if pairs.is_empty() {
        return vec![DebugConfigReportLine::muted("     - <none>")];
    }

    pairs
        .into_iter()
        .map(|(key, value)| DebugConfigReportLine::text(format!("     - {key} = {value}")))
        .collect()
}

fn render_non_file_layer_value(layer: &ConfigLayerEntry) -> Vec<DebugConfigReportLine> {
    let label = non_file_layer_value_label(&layer.name);
    let value = layer
        .raw_toml()
        .map(ToString::to_string)
        .unwrap_or_else(|| format_toml_value(&layer.config));
    if value.is_empty() {
        return vec![DebugConfigReportLine::muted(format!(
            "     {label}: <empty>"
        ))];
    }

    if value.contains('\n') {
        let mut lines = vec![DebugConfigReportLine::text(format!("     {label}:"))];
        lines.extend(
            value
                .lines()
                .map(|line| DebugConfigReportLine::text(format!("       {line}"))),
        );
        lines
    } else {
        vec![DebugConfigReportLine::text(format!(
            "     {label}: {value}"
        ))]
    }
}

fn non_file_layer_value_label(source: &ConfigLayerSource) -> &'static str {
    match source {
        ConfigLayerSource::Mdm { .. } | ConfigLayerSource::LegacyManagedConfigTomlFromMdm => {
            "MDM value"
        }
        ConfigLayerSource::EnterpriseManaged { .. } => "Enterprise-managed config value",
        ConfigLayerSource::SessionFlags
        | ConfigLayerSource::System { .. }
        | ConfigLayerSource::User { .. }
        | ConfigLayerSource::Project { .. }
        | ConfigLayerSource::LegacyManagedConfigTomlFromFile { .. } => "Layer value",
    }
}

fn flatten_toml_key_values(
    value: &TomlValue,
    prefix: Option<&str>,
    out: &mut Vec<(String, String)>,
) {
    match value {
        TomlValue::Table(table) => {
            let mut entries = table.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| key.as_str());
            for (key, child) in entries {
                let next_prefix = if let Some(prefix) = prefix {
                    format!("{prefix}.{key}")
                } else {
                    key.to_string()
                };
                flatten_toml_key_values(child, Some(&next_prefix), out);
            }
        }
        _ => {
            let key = prefix.unwrap_or("<value>").to_string();
            out.push((key, format_toml_value(value)));
        }
    }
}

fn format_toml_value(value: &TomlValue) -> String {
    value.to_string()
}

fn requirement_line(
    name: &str,
    value: String,
    source: Option<&RequirementSource>,
) -> DebugConfigReportLine {
    let source = source
        .map(ToString::to_string)
        .unwrap_or_else(|| "<unspecified>".to_string());
    DebugConfigReportLine::text(format!("  - {name}: {value} (source: {source})"))
}

fn join_or_empty(values: Vec<String>) -> String {
    if values.is_empty() {
        "<empty>".to_string()
    } else {
        values.join(", ")
    }
}

fn normalize_allowed_web_search_modes(
    modes: &[WebSearchModeRequirement],
) -> Vec<WebSearchModeRequirement> {
    if modes.is_empty() {
        return vec![WebSearchModeRequirement::Disabled];
    }

    let mut normalized = modes.to_vec();
    if !normalized.contains(&WebSearchModeRequirement::Disabled) {
        normalized.push(WebSearchModeRequirement::Disabled);
    }
    normalized
}

fn format_sandbox_mode_requirement(mode: SandboxModeRequirement) -> String {
    match mode {
        SandboxModeRequirement::ReadOnly => "read-only".to_string(),
        SandboxModeRequirement::WorkspaceWrite => "workspace-write".to_string(),
        SandboxModeRequirement::DangerFullAccess => "danger-full-access".to_string(),
        SandboxModeRequirement::ExternalSandbox => "external-sandbox".to_string(),
    }
}

fn format_residency_requirement(requirement: ResidencyRequirement) -> String {
    match requirement {
        ResidencyRequirement::Us => "us".to_string(),
    }
}

fn format_network_constraints(network: &NetworkConstraints) -> String {
    let mut parts = Vec::new();

    let NetworkConstraints {
        enabled,
        http_port,
        socks_port,
        allow_upstream_proxy,
        dangerously_allow_non_loopback_proxy,
        dangerously_allow_all_unix_sockets,
        domains,
        managed_allowed_domains_only,
        unix_sockets,
        allow_local_binding,
    } = network;

    if let Some(enabled) = enabled {
        parts.push(format!("enabled={enabled}"));
    }
    if let Some(http_port) = http_port {
        parts.push(format!("http_port={http_port}"));
    }
    if let Some(socks_port) = socks_port {
        parts.push(format!("socks_port={socks_port}"));
    }
    if let Some(allow_upstream_proxy) = allow_upstream_proxy {
        parts.push(format!("allow_upstream_proxy={allow_upstream_proxy}"));
    }
    if let Some(dangerously_allow_non_loopback_proxy) = dangerously_allow_non_loopback_proxy {
        parts.push(format!(
            "dangerously_allow_non_loopback_proxy={dangerously_allow_non_loopback_proxy}"
        ));
    }
    if let Some(dangerously_allow_all_unix_sockets) = dangerously_allow_all_unix_sockets {
        parts.push(format!(
            "dangerously_allow_all_unix_sockets={dangerously_allow_all_unix_sockets}"
        ));
    }
    if let Some(domains) = domains {
        parts.push(format!(
            "domains={}",
            format_network_permission_entries(&domains.entries, format_network_domain_permission)
        ));
    }
    if let Some(managed_allowed_domains_only) = managed_allowed_domains_only {
        parts.push(format!(
            "managed_allowed_domains_only={managed_allowed_domains_only}"
        ));
    }
    if let Some(unix_sockets) = unix_sockets {
        parts.push(format!(
            "unix_sockets={}",
            format_network_permission_entries(
                &unix_sockets.entries,
                format_network_unix_socket_permission,
            )
        ));
    }
    if let Some(allow_local_binding) = allow_local_binding {
        parts.push(format!("allow_local_binding={allow_local_binding}"));
    }

    join_or_empty(parts)
}

fn format_network_permission_entries<T: Copy>(
    entries: &std::collections::BTreeMap<String, T>,
    format_value: impl Fn(T) -> &'static str,
) -> String {
    let parts = entries
        .iter()
        .map(|(key, value)| format!("{key}={}", format_value(*value)))
        .collect::<Vec<_>>();
    format!("{{{}}}", parts.join(", "))
}

fn format_network_domain_permission(permission: NetworkDomainPermissionToml) -> &'static str {
    match permission {
        NetworkDomainPermissionToml::Allow => "allow",
        NetworkDomainPermissionToml::Deny => "deny",
    }
}

fn format_network_unix_socket_permission(
    permission: NetworkUnixSocketPermissionToml,
) -> &'static str {
    match permission {
        NetworkUnixSocketPermissionToml::Allow => "allow",
        NetworkUnixSocketPermissionToml::Deny => "deny",
    }
}

#[cfg(test)]
#[path = "debug_report_tests.rs"]
mod tests;
