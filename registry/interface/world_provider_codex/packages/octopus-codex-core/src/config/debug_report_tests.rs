use super::DebugConfigReportLine;
use super::render_debug_config_lines;
use super::sandbox_mode_is_allowed_by_permissions;
use super::session_all_proxy_url;
use crate::config::Permissions;
use octopus_config::ConfigLayerEntry;
use octopus_config::ConfigLayerSource;
use octopus_config::ConfigLayerStack;
use octopus_config::ConfigRequirements;
use octopus_config::ConfigRequirementsToml;
use octopus_config::Constrained;
use octopus_config::ConstrainedWithSource;
use octopus_config::ConstraintError;
use octopus_config::FeatureRequirementsToml;
use octopus_config::FilesystemConstraints;
use octopus_config::McpServerIdentity;
use octopus_config::McpServerRequirement;
use octopus_config::NetworkConstraints;
use octopus_config::NetworkDomainPermissionToml;
use octopus_config::NetworkDomainPermissionsToml;
use octopus_config::NetworkUnixSocketPermissionToml;
use octopus_config::NetworkUnixSocketPermissionsToml;
use octopus_config::RequirementSource;
use octopus_config::ResidencyRequirement;
use octopus_config::SandboxModeRequirement;
use octopus_config::Sourced;
use octopus_config::WebSearchModeRequirement;
use octopus_config::sandbox_mode_requirement_for_permission_profile;
use octopus_protocol::config_types::ApprovalsReviewer;
use octopus_protocol::config_types::WebSearchMode;
use octopus_protocol::models::PermissionProfile;
use octopus_protocol::protocol::AskForApproval;
use octopus_util_abspath::AbsolutePathBuf;
use std::collections::BTreeMap;
use toml::Value as TomlValue;

fn empty_toml_table() -> TomlValue {
    TomlValue::Table(toml::map::Map::new())
}

fn absolute_path(path: &str) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(path).expect("absolute path")
}

fn render_to_text(lines: &[DebugConfigReportLine]) -> String {
    lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_stack_to_text(stack: &ConfigLayerStack) -> String {
    render_stack_to_text_with_sandbox_mode_filter(stack, |_| true)
}

fn render_stack_to_text_with_sandbox_mode_filter(
    stack: &ConfigLayerStack,
    sandbox_mode_is_effectively_allowed: impl Fn(SandboxModeRequirement) -> bool,
) -> String {
    render_to_text(&render_debug_config_lines(
        stack,
        sandbox_mode_is_effectively_allowed,
    ))
}

#[test]
fn debug_config_output_lists_all_layers_including_disabled() {
    let system_file = if cfg!(windows) {
        absolute_path("C:\\etc\\codex\\config.toml")
    } else {
        absolute_path("/etc/codex/config.toml")
    };
    let project_folder = if cfg!(windows) {
        absolute_path("C:\\repo\\.codex")
    } else {
        absolute_path("/repo/.codex")
    };

    let layers = vec![
        ConfigLayerEntry::new(
            ConfigLayerSource::System { file: system_file },
            empty_toml_table(),
        ),
        ConfigLayerEntry::new_disabled(
            ConfigLayerSource::Project {
                dot_codex_folder: project_folder,
            },
            empty_toml_table(),
            "project is untrusted",
        ),
    ];
    let stack = ConfigLayerStack::new(
        layers,
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("config layer stack");

    let rendered = render_stack_to_text(&stack);
    assert!(rendered.contains("(enabled)"));
    assert!(rendered.contains("(disabled)"));
    assert!(rendered.contains("reason: project is untrusted"));
    assert!(rendered.contains("Requirements:"));
    assert!(rendered.contains("  <none>"));
}

#[test]
fn debug_config_output_lists_requirement_sources() {
    let requirements_file = if cfg!(windows) {
        absolute_path("C:\\ProgramData\\OpenAI\\Codex\\requirements.toml")
    } else {
        absolute_path("/etc/codex/requirements.toml")
    };
    let denied_path = if cfg!(windows) {
        absolute_path("C:\\Users\\alice\\.gitconfig")
    } else {
        absolute_path("/home/alice/.gitconfig")
    };

    let requirements = ConfigRequirements {
        approval_policy: ConstrainedWithSource::new(
            Constrained::allow_any(AskForApproval::OnRequest),
            Some(RequirementSource::LegacyManagedConfigTomlFromMdm),
        ),
        approvals_reviewer: ConstrainedWithSource::new(
            Constrained::allow_any(ApprovalsReviewer::AutoReview),
            Some(RequirementSource::LegacyManagedConfigTomlFromMdm),
        ),
        permission_profile: ConstrainedWithSource::new(
            Constrained::allow_any(PermissionProfile::read_only()),
            Some(RequirementSource::SystemRequirementsToml {
                file: requirements_file.clone(),
            }),
        ),
        mcp_servers: Some(Sourced::new(
            BTreeMap::from([(
                "docs".to_string(),
                McpServerRequirement {
                    identity: McpServerIdentity::Command {
                        command: "codex-mcp".to_string(),
                    },
                },
            )]),
            RequirementSource::LegacyManagedConfigTomlFromMdm,
        )),
        enforce_residency: ConstrainedWithSource::new(
            Constrained::allow_any(Some(ResidencyRequirement::Us)),
            Some(RequirementSource::LegacyManagedConfigTomlFromMdm),
        ),
        web_search_mode: ConstrainedWithSource::new(
            Constrained::allow_any(WebSearchMode::Cached),
            Some(RequirementSource::LegacyManagedConfigTomlFromMdm),
        ),
        allow_appshots: Some(Sourced::new(
            false,
            RequirementSource::LegacyManagedConfigTomlFromMdm,
        )),
        allow_remote_control: Some(Sourced::new(
            false,
            RequirementSource::LegacyManagedConfigTomlFromMdm,
        )),
        feature_requirements: Some(Sourced::new(
            FeatureRequirementsToml {
                entries: BTreeMap::from([("guardian_approval".to_string(), true)]),
            },
            RequirementSource::LegacyManagedConfigTomlFromMdm,
        )),
        network: Some(Sourced::new(
            NetworkConstraints {
                enabled: Some(true),
                domains: Some(NetworkDomainPermissionsToml {
                    entries: BTreeMap::from([(
                        "example.com".to_string(),
                        NetworkDomainPermissionToml::Allow,
                    )]),
                }),
                ..Default::default()
            },
            RequirementSource::LegacyManagedConfigTomlFromMdm,
        )),
        filesystem: Some(Sourced::new(
            FilesystemConstraints {
                deny_read: vec![denied_path.clone().into()],
            },
            RequirementSource::SystemRequirementsToml {
                file: requirements_file.clone(),
            },
        )),
        guardian_policy_config_source: Some(RequirementSource::LegacyManagedConfigTomlFromMdm),
        ..ConfigRequirements::default()
    };

    let requirements_toml = ConfigRequirementsToml {
        allowed_approval_policies: Some(vec![AskForApproval::OnRequest]),
        allowed_approvals_reviewers: Some(vec![ApprovalsReviewer::AutoReview]),
        allowed_sandbox_modes: Some(vec![SandboxModeRequirement::ReadOnly]),
        allowed_permission_profiles: None,
        default_permissions: None,
        remote_sandbox_config: None,
        allowed_web_search_modes: Some(vec![WebSearchModeRequirement::Cached]),
        allow_appshots: Some(false),
        allow_remote_control: Some(false),
        computer_use: None,
        windows: None,
        guardian_policy_config: Some("Use the managed guardian policy.".to_string()),
        feature_requirements: Some(FeatureRequirementsToml {
            entries: BTreeMap::from([("guardian_approval".to_string(), true)]),
        }),
        mcp_servers: Some(BTreeMap::from([(
            "docs".to_string(),
            McpServerRequirement {
                identity: McpServerIdentity::Command {
                    command: "codex-mcp".to_string(),
                },
            },
        )])),
        apps: None,
        rules: None,
        enforce_residency: Some(ResidencyRequirement::Us),
        network: None,
        permissions: None,
    };

    let user_file = if cfg!(windows) {
        absolute_path("C:\\users\\alice\\.codex\\config.toml")
    } else {
        absolute_path("/home/alice/.codex/config.toml")
    };
    let stack = ConfigLayerStack::new(
        vec![ConfigLayerEntry::new(
            ConfigLayerSource::User {
                file: user_file,
                profile: None,
            },
            empty_toml_table(),
        )],
        requirements,
        requirements_toml,
    )
    .expect("config layer stack");

    let rendered = render_stack_to_text(&stack);
    #[cfg(not(windows))]
    insta::assert_snapshot!("debug_config_requirement_sources", rendered.as_str());

    let requirements_source = (RequirementSource::LegacyManagedConfigTomlFromMdm).to_string();
    assert!(rendered.contains(&format!(
        "allowed_approval_policies: on-request (source: {requirements_source})"
    )));
    assert!(rendered.contains(
        "allowed_approvals_reviewers: auto_review (source: MDM managed_config.toml (legacy))"
    ));
    assert!(
        rendered.contains(
            format!(
                "allowed_sandbox_modes: read-only (source: {})",
                requirements_file.as_path().display()
            )
            .as_str(),
        )
    );
    assert!(rendered.contains(&format!(
        "allowed_web_search_modes: cached, disabled (source: {requirements_source})"
    )));
    assert!(rendered.contains(&format!(
        "allow_appshots: false (source: {requirements_source})"
    )));
    assert!(rendered.contains(&format!(
        "allow_remote_control: false (source: {requirements_source})"
    )));
    assert!(rendered.contains(&format!(
        "guardian_policy_config: configured (source: {requirements_source})"
    )));
    assert!(rendered.contains(&format!(
        "features: guardian_approval=true (source: {requirements_source})"
    )));
    assert!(rendered.contains("mcp_servers: docs (source: MDM managed_config.toml (legacy))"));
    assert!(rendered.contains(&format!(
        "enforce_residency: us (source: {requirements_source})"
    )));
    assert!(rendered.contains(&format!(
        "experimental_network: enabled=true, domains={{example.com=allow}} (source: {requirements_source})"
    )));
    assert!(
        rendered.contains(
            format!(
                "permissions.filesystem.deny_read: {}",
                denied_path.as_path().display()
            )
            .as_str()
        )
    );
    assert!(!rendered.contains("  - rules:"));
}

#[test]
fn debug_config_output_filters_sandbox_modes_blocked_by_deny_read_requirements() {
    let requirements_file = if cfg!(windows) {
        absolute_path("C:\\ProgramData\\OpenAI\\Codex\\requirements.toml")
    } else {
        absolute_path("/etc/codex/requirements.toml")
    };
    let denied_path = if cfg!(windows) {
        absolute_path("C:\\Users\\alice\\.gitconfig")
    } else {
        absolute_path("/home/alice/.gitconfig")
    };

    let requirements = ConfigRequirements {
        permission_profile: ConstrainedWithSource::new(
            Constrained::allow_any(PermissionProfile::read_only()),
            Some(RequirementSource::SystemRequirementsToml {
                file: requirements_file.clone(),
            }),
        ),
        filesystem: Some(Sourced::new(
            FilesystemConstraints {
                deny_read: vec![denied_path.into()],
            },
            RequirementSource::SystemRequirementsToml {
                file: requirements_file.clone(),
            },
        )),
        ..ConfigRequirements::default()
    };
    let requirements_toml = ConfigRequirementsToml {
        allowed_sandbox_modes: Some(vec![
            SandboxModeRequirement::ReadOnly,
            SandboxModeRequirement::WorkspaceWrite,
            SandboxModeRequirement::DangerFullAccess,
            SandboxModeRequirement::ExternalSandbox,
        ]),
        ..ConfigRequirementsToml::default()
    };
    let stack = ConfigLayerStack::new(Vec::new(), requirements, requirements_toml)
        .expect("config layer stack");
    let constrained_permission_profile =
        Constrained::new(PermissionProfile::read_only(), |candidate| {
            let mode = sandbox_mode_requirement_for_permission_profile(candidate);
            match mode {
                SandboxModeRequirement::ReadOnly | SandboxModeRequirement::WorkspaceWrite => Ok(()),
                SandboxModeRequirement::DangerFullAccess
                | SandboxModeRequirement::ExternalSandbox => Err(ConstraintError::InvalidValue {
                    field_name: "sandbox_mode",
                    candidate: format!("{mode:?}"),
                    allowed: "[read-only, workspace-write]".to_string(),
                    requirement_source: RequirementSource::Unknown,
                }),
            }
        })
        .expect("constrained permission profile");
    let permissions = Permissions::from_approval_and_profile(
        Constrained::allow_any(AskForApproval::OnRequest),
        constrained_permission_profile,
    )
    .expect("permissions");

    let rendered = render_stack_to_text_with_sandbox_mode_filter(&stack, |mode| {
        sandbox_mode_is_allowed_by_permissions(&permissions, mode)
    });
    #[cfg(not(windows))]
    insta::assert_snapshot!(
        "debug_config_effective_sandbox_modes_with_deny_read",
        rendered.as_str()
    );
    assert!(
        rendered.contains(
            format!(
                "allowed_sandbox_modes: read-only, workspace-write (source: {})",
                requirements_file.as_path().display()
            )
            .as_str()
        )
    );
    assert!(!rendered.contains("danger-full-access"));
    assert!(!rendered.contains("external-sandbox"));
}

#[test]
fn debug_config_output_lists_approvals_reviewer_as_requirement() {
    let requirements = ConfigRequirements {
        approvals_reviewer: ConstrainedWithSource::new(
            Constrained::allow_any(ApprovalsReviewer::AutoReview),
            Some(RequirementSource::LegacyManagedConfigTomlFromMdm),
        ),
        ..ConfigRequirements::default()
    };
    let requirements_toml = ConfigRequirementsToml {
        allowed_approvals_reviewers: Some(vec![ApprovalsReviewer::AutoReview]),
        ..ConfigRequirementsToml::default()
    };
    let stack = ConfigLayerStack::new(Vec::new(), requirements, requirements_toml)
        .expect("config layer stack");

    let rendered = render_stack_to_text(&stack);
    assert!(rendered.contains(
        "allowed_approvals_reviewers: auto_review (source: MDM managed_config.toml (legacy))"
    ));
    assert!(!rendered.contains("Requirements:\n  <none>"));
}

#[test]
fn debug_config_output_formats_unix_socket_permissions() {
    let requirements = ConfigRequirements {
        network: Some(Sourced::new(
            NetworkConstraints {
                unix_sockets: Some(NetworkUnixSocketPermissionsToml {
                    entries: BTreeMap::from([
                        (
                            "/tmp/codex.sock".to_string(),
                            NetworkUnixSocketPermissionToml::Allow,
                        ),
                        (
                            "/tmp/blocked.sock".to_string(),
                            NetworkUnixSocketPermissionToml::Deny,
                        ),
                    ]),
                }),
                ..Default::default()
            },
            RequirementSource::LegacyManagedConfigTomlFromMdm,
        )),
        ..ConfigRequirements::default()
    };

    let stack = ConfigLayerStack::new(Vec::new(), requirements, ConfigRequirementsToml::default())
        .expect("config layer stack");

    let rendered = render_stack_to_text(&stack);
    let requirements_source = (RequirementSource::LegacyManagedConfigTomlFromMdm).to_string();
    assert!(rendered.contains(&format!(
        "experimental_network: unix_sockets={{/tmp/blocked.sock=deny, /tmp/codex.sock=allow}} (source: {requirements_source})"
    )));
}

#[test]
fn debug_config_output_lists_session_flag_key_value_pairs() {
    let session_flags = toml::from_str::<TomlValue>(
        r#"
model = "gpt-5"
[sandbox_workspace_write]
network_access = true
writable_roots = ["/tmp"]
"#,
    )
    .expect("session flags");

    let stack = ConfigLayerStack::new(
        vec![ConfigLayerEntry::new(
            ConfigLayerSource::SessionFlags,
            session_flags,
        )],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("config layer stack");

    let rendered = render_stack_to_text(&stack);
    assert!(rendered.contains("session-flags (enabled)"));
    assert!(rendered.contains("     - model = \"gpt-5\""));
    assert!(rendered.contains("     - sandbox_workspace_write.network_access = true"));
    assert!(rendered.contains("sandbox_workspace_write.writable_roots"));
    assert!(rendered.contains("/tmp"));
}

#[test]
fn debug_config_output_shows_legacy_mdm_layer_value() {
    let raw_mdm_toml = r#"
# managed by MDM
model = "managed_model"
approval_policy = "never"
"#;
    let mdm_value = toml::from_str::<TomlValue>(raw_mdm_toml).expect("MDM value");
    let mdm_base_dir = if cfg!(windows) {
        absolute_path("C:\\codex")
    } else {
        absolute_path("/var/lib/codex")
    };

    let stack = ConfigLayerStack::new(
        vec![ConfigLayerEntry::new_with_raw_toml(
            ConfigLayerSource::LegacyManagedConfigTomlFromMdm,
            mdm_value,
            raw_mdm_toml.to_string(),
            mdm_base_dir,
        )],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("config layer stack");

    let rendered = render_stack_to_text(&stack);
    assert!(rendered.contains("legacy managed_config.toml (MDM) (enabled)"));
    assert!(rendered.contains("MDM value:"));
    assert!(rendered.contains("# managed by MDM"));
    assert!(rendered.contains("model = \"managed_model\""));
    assert!(rendered.contains("approval_policy = \"never\""));
}

#[test]
fn debug_config_output_shows_enterprise_managed_layer_value() {
    let raw_cloud_toml = r#"
# managed by cloud
model = "enterprise_model"
approval_policy = "never"
"#;
    let cloud_value = toml::from_str::<TomlValue>(raw_cloud_toml).expect("cloud value");
    let cloud_base_dir = if cfg!(windows) {
        absolute_path("C:\\codex")
    } else {
        absolute_path("/var/lib/codex")
    };

    let stack = ConfigLayerStack::new(
        vec![ConfigLayerEntry::new_with_raw_toml(
            ConfigLayerSource::EnterpriseManaged {
                id: "cfg_123".to_string(),
                name: "Base policy".to_string(),
            },
            cloud_value,
            raw_cloud_toml.to_string(),
            cloud_base_dir,
        )],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("config layer stack");

    let rendered = render_stack_to_text(&stack);
    assert!(rendered.contains("enterprise-managed (Base policy, cfg_123) (enabled)"));
    assert!(rendered.contains("Enterprise-managed config value:"));
    assert!(!rendered.contains("MDM value:"));
    assert!(rendered.contains("# managed by cloud"));
    assert!(rendered.contains("model = \"enterprise_model\""));
    assert!(rendered.contains("approval_policy = \"never\""));
}

#[test]
fn debug_config_output_normalizes_empty_web_search_mode_list() {
    let requirements = ConfigRequirements {
        web_search_mode: ConstrainedWithSource::new(
            Constrained::allow_any(WebSearchMode::Disabled),
            Some(RequirementSource::LegacyManagedConfigTomlFromMdm),
        ),
        ..ConfigRequirements::default()
    };

    let requirements_toml = ConfigRequirementsToml {
        allowed_approval_policies: None,
        allowed_approvals_reviewers: None,
        allowed_sandbox_modes: None,
        allowed_permission_profiles: None,
        default_permissions: None,
        remote_sandbox_config: None,
        allowed_web_search_modes: Some(Vec::new()),
        allow_appshots: None,
        allow_remote_control: None,
        computer_use: None,
        windows: None,
        guardian_policy_config: None,
        feature_requirements: None,
        mcp_servers: None,
        apps: None,
        rules: None,
        enforce_residency: None,
        network: None,
        permissions: None,
    };

    let stack = ConfigLayerStack::new(Vec::new(), requirements, requirements_toml)
        .expect("config layer stack");

    let rendered = render_stack_to_text(&stack);
    let requirements_source = (RequirementSource::LegacyManagedConfigTomlFromMdm).to_string();
    assert!(rendered.contains(&format!(
        "allowed_web_search_modes: disabled (source: {requirements_source})"
    )));
}

#[test]
fn session_all_proxy_url_uses_socks_when_enabled() {
    assert_eq!(
        session_all_proxy_url("127.0.0.1:3128", "127.0.0.1:8081", true),
        "socks5h://127.0.0.1:8081".to_string()
    );
}

#[test]
fn session_all_proxy_url_uses_http_when_socks_disabled() {
    assert_eq!(
        session_all_proxy_url("127.0.0.1:3128", "127.0.0.1:8081", false),
        "http://127.0.0.1:3128".to_string()
    );
}
