use octopus_substrate_contract::approvals_substrate::AskForApproval;
use octopus_substrate_contract::permission_profile::ActivePermissionProfile;
use octopus_substrate_contract::permission_profile::PermissionProfile;
use octopus_substrate_contract::shell_env_config::WindowsSandboxLevel;
use octopus_util_abspath::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostureError(pub String);

impl fmt::Display for PostureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PostureError {}

pub type PostureResult<T> = Result<T, PostureError>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstrainedValue<T> {
    value: T,
    allowed: Option<Vec<T>>,
    field_name: String,
    allowed_display: String,
    requirement_source: String,
}

impl<T: Clone + PartialEq + fmt::Debug> ConstrainedValue<T> {
    pub fn unconstrained(value: T) -> Self {
        Self {
            value,
            allowed: None,
            field_name: String::new(),
            allowed_display: String::new(),
            requirement_source: String::new(),
        }
    }

    pub fn constrained(
        value: T,
        allowed: Vec<T>,
        field_name: impl Into<String>,
        allowed_display: impl Into<String>,
        requirement_source: impl Into<String>,
    ) -> Self {
        Self {
            value,
            allowed: Some(allowed),
            field_name: field_name.into(),
            allowed_display: allowed_display.into(),
            requirement_source: requirement_source.into(),
        }
    }

    pub fn allow_only(value: T) -> Self {
        let allowed_display = format!("[{value:?}]");
        Self {
            allowed: Some(vec![value.clone()]),
            value,
            field_name: "<unknown>".to_string(),
            allowed_display,
            requirement_source: "unknown".to_string(),
        }
    }

    pub fn get(&self) -> &T {
        &self.value
    }

    pub fn value(&self) -> T
    where
        T: Copy,
    {
        self.value
    }

    pub fn can_set(&self, candidate: &T) -> PostureResult<()> {
        match &self.allowed {
            None => Ok(()),
            Some(allowed) if allowed.contains(candidate) => Ok(()),
            Some(_) => Err(PostureError(format!(
                "invalid value for `{field}`: `{candidate:?}` is not in the allowed set {allowed} (set by {source})",
                field = self.field_name,
                allowed = self.allowed_display,
                source = self.requirement_source,
            ))),
        }
    }

    pub fn set(&mut self, candidate: T) -> PostureResult<()> {
        self.can_set(&candidate)?;
        self.value = candidate;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxModeClass {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
    ExternalSandbox,
}

pub fn classify_permission_profile(profile: &PermissionProfile) -> SandboxModeClass {
    match profile {
        PermissionProfile::Disabled => SandboxModeClass::DangerFullAccess,
        PermissionProfile::External { .. } => SandboxModeClass::ExternalSandbox,
        PermissionProfile::Managed { .. } => {
            let file_system_policy = profile.file_system_sandbox_policy();
            if file_system_policy.has_full_disk_write_access() {
                SandboxModeClass::DangerFullAccess
            } else if file_system_policy
                .entries
                .iter()
                .any(|entry| entry.access.can_write())
            {
                SandboxModeClass::WorkspaceWrite
            } else {
                SandboxModeClass::ReadOnly
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileClassVerdicts {
    pub read_only: Option<String>,
    pub workspace_write: Option<String>,
    pub danger_full_access: Option<String>,
    pub external: Option<String>,
}

impl ProfileClassVerdicts {
    fn verdict_for(&self, class: SandboxModeClass) -> Option<&String> {
        match class {
            SandboxModeClass::ReadOnly => self.read_only.as_ref(),
            SandboxModeClass::WorkspaceWrite => self.workspace_write.as_ref(),
            SandboxModeClass::DangerFullAccess => self.danger_full_access.as_ref(),
            SandboxModeClass::ExternalSandbox => self.external.as_ref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionPermissionSnapshot {
    permission_profile: PermissionProfile,
    active_permission_profile: Option<ActivePermissionProfile>,
    profile_workspace_roots: Vec<AbsolutePathBuf>,
}

impl SessionPermissionSnapshot {
    pub fn legacy(permission_profile: PermissionProfile) -> Self {
        Self {
            permission_profile,
            active_permission_profile: None,
            profile_workspace_roots: Vec::new(),
        }
    }

    pub fn active(
        permission_profile: PermissionProfile,
        active_permission_profile: ActivePermissionProfile,
    ) -> Self {
        Self {
            permission_profile,
            active_permission_profile: Some(active_permission_profile),
            profile_workspace_roots: Vec::new(),
        }
    }

    pub fn active_with_profile_workspace_roots(
        permission_profile: PermissionProfile,
        active_permission_profile: ActivePermissionProfile,
        profile_workspace_roots: Vec<AbsolutePathBuf>,
    ) -> Self {
        Self {
            permission_profile,
            active_permission_profile: Some(active_permission_profile),
            profile_workspace_roots,
        }
    }

    pub fn from_session_snapshot(
        permission_profile: PermissionProfile,
        active_permission_profile: Option<ActivePermissionProfile>,
    ) -> Self {
        Self {
            permission_profile,
            active_permission_profile,
            profile_workspace_roots: Vec::new(),
        }
    }

    pub fn permission_profile(&self) -> &PermissionProfile {
        &self.permission_profile
    }

    pub fn active_permission_profile(&self) -> Option<ActivePermissionProfile> {
        self.active_permission_profile.clone()
    }

    pub fn profile_workspace_roots(&self) -> &[AbsolutePathBuf] {
        &self.profile_workspace_roots
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowsSandboxMode {
    Elevated,
    Unelevated,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionPermissions {
    pub approval_policy: ConstrainedValue<AskForApproval>,
    permission_profile: PermissionProfile,
    active_permission_profile: Option<ActivePermissionProfile>,
    profile_workspace_roots: Vec<AbsolutePathBuf>,
    profile_verdicts: ProfileClassVerdicts,
    workspace_roots: Vec<AbsolutePathBuf>,
    pub windows_sandbox_mode: Option<WindowsSandboxMode>,
}

impl SessionPermissions {
    pub fn new(
        approval_policy: ConstrainedValue<AskForApproval>,
        snapshot: SessionPermissionSnapshot,
        profile_verdicts: ProfileClassVerdicts,
        workspace_roots: Vec<AbsolutePathBuf>,
        windows_sandbox_mode: Option<WindowsSandboxMode>,
    ) -> Self {
        Self {
            approval_policy,
            permission_profile: snapshot.permission_profile,
            active_permission_profile: snapshot.active_permission_profile,
            profile_workspace_roots: snapshot.profile_workspace_roots,
            profile_verdicts,
            workspace_roots,
            windows_sandbox_mode,
        }
    }

    pub fn degraded_default() -> Self {
        Self {
            approval_policy: ConstrainedValue::unconstrained(AskForApproval::default()),
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            profile_workspace_roots: Vec::new(),
            profile_verdicts: ProfileClassVerdicts::default(),
            workspace_roots: Vec::new(),
            windows_sandbox_mode: None,
        }
    }

    pub fn permission_profile(&self) -> &PermissionProfile {
        &self.permission_profile
    }

    pub fn active_permission_profile(&self) -> Option<ActivePermissionProfile> {
        self.active_permission_profile.clone()
    }

    pub fn profile_workspace_roots(&self) -> &[AbsolutePathBuf] {
        &self.profile_workspace_roots
    }

    pub fn workspace_roots(&self) -> &[AbsolutePathBuf] {
        &self.workspace_roots
    }

    pub fn user_visible_workspace_roots(&self) -> &[AbsolutePathBuf] {
        &self.workspace_roots
    }

    pub fn set_workspace_roots(&mut self, workspace_roots: Vec<AbsolutePathBuf>) {
        self.workspace_roots = workspace_roots;
    }

    pub fn effective_permission_profile(&self) -> PermissionProfile {
        self.permission_profile
            .clone()
            .materialize_project_roots_with_workspace_roots(&self.workspace_roots)
    }

    pub fn can_set_permission_profile(&self, candidate: &PermissionProfile) -> PostureResult<()> {
        let class = classify_permission_profile(candidate);
        match self.profile_verdicts.verdict_for(class) {
            None => Ok(()),
            Some(message) => Err(PostureError(message.clone())),
        }
    }

    pub fn set_permission_profile(&mut self, candidate: PermissionProfile) -> PostureResult<()> {
        self.set_permission_profile_from_session_snapshot(SessionPermissionSnapshot::legacy(
            candidate,
        ))
    }

    pub fn set_permission_profile_from_session_snapshot(
        &mut self,
        snapshot: SessionPermissionSnapshot,
    ) -> PostureResult<()> {
        self.can_set_permission_profile(snapshot.permission_profile())?;
        self.install_snapshot(snapshot);
        Ok(())
    }

    pub fn replace_permission_profile_from_session_snapshot(
        &mut self,
        snapshot: SessionPermissionSnapshot,
    ) -> PostureResult<()> {
        let class = classify_permission_profile(snapshot.permission_profile());
        let refusal = "pinned by the live session (a differently-classed profile was rejected by \
                       the local requirements)"
            .to_string();
        self.profile_verdicts = ProfileClassVerdicts {
            read_only: (class != SandboxModeClass::ReadOnly).then(|| refusal.clone()),
            workspace_write: (class != SandboxModeClass::WorkspaceWrite).then(|| refusal.clone()),
            danger_full_access: (class != SandboxModeClass::DangerFullAccess)
                .then(|| refusal.clone()),
            external: (class != SandboxModeClass::ExternalSandbox).then(|| refusal),
        };
        self.install_snapshot(snapshot);
        Ok(())
    }

    fn install_snapshot(&mut self, snapshot: SessionPermissionSnapshot) {
        self.permission_profile = snapshot.permission_profile;
        self.active_permission_profile = snapshot.active_permission_profile;
        self.profile_workspace_roots = snapshot.profile_workspace_roots;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionFeatureRow {
    pub key: String,
    pub enabled: bool,
    pub default_enabled: bool,
    pub menu_name: Option<String>,
    pub menu_description: Option<String>,
    pub announcement: Option<String>,
    pub enable_verdict: Option<String>,
    pub disable_verdict: Option<String>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionFeatures {
    rows: Vec<SessionFeatureRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeatureMenuRow {
    pub key: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
}

impl SessionFeatures {
    pub fn from_rows(rows: Vec<SessionFeatureRow>) -> Self {
        Self { rows }
    }

    fn row(&self, key: &str) -> Option<&SessionFeatureRow> {
        self.rows
            .iter()
            .find(|row| row.key == key || row.aliases.iter().any(|alias| alias == key))
    }

    pub fn enabled_key(&self, key: &str) -> bool {
        self.row(key).is_some_and(|row| row.enabled)
    }

    pub fn key_default_enabled(&self, key: &str) -> Option<bool> {
        self.row(key).map(|row| row.default_enabled)
    }

    pub fn set_enabled_by_key(&mut self, key: &str, enabled: bool) -> std::io::Result<()> {
        let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| row.key == key || row.aliases.iter().any(|alias| alias == key))
        else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unknown feature key `{key}`"),
            ));
        };
        let verdict = if enabled {
            row.enable_verdict.as_ref()
        } else {
            row.disable_verdict.as_ref()
        };
        if let Some(message) = verdict {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message.clone(),
            ));
        }
        row.enabled = enabled;
        Ok(())
    }

    pub fn toggle_verdict(&self, key: &str, enabled: bool) -> Option<String> {
        let row = self.row(key)?;
        if enabled {
            row.enable_verdict.clone()
        } else {
            row.disable_verdict.clone()
        }
    }

    pub fn experimental_menu_rows(&self) -> Vec<FeatureMenuRow> {
        self.rows
            .iter()
            .filter_map(|row| {
                Some(FeatureMenuRow {
                    key: row.key.clone(),
                    name: row.menu_name.clone()?,
                    description: row.menu_description.clone()?,
                    enabled: row.enabled,
                })
            })
            .collect()
    }

    pub fn experimental_announcements(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter_map(|row| row.announcement.clone())
            .collect()
    }
}

pub fn windows_sandbox_level(
    permissions: &SessionPermissions,
    features: &SessionFeatures,
) -> WindowsSandboxLevel {
    match permissions.windows_sandbox_mode {
        Some(WindowsSandboxMode::Elevated) => WindowsSandboxLevel::Elevated,
        Some(WindowsSandboxMode::Unelevated) => WindowsSandboxLevel::RestrictedToken,
        None => {
            if features.enabled_key("elevated_windows_sandbox") {
                WindowsSandboxLevel::Elevated
            } else if features.enabled_key("experimental_windows_sandbox") {
                WindowsSandboxLevel::RestrictedToken
            } else {
                WindowsSandboxLevel::Disabled
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn constrained_value_refuses_outside_the_allowed_set_with_provider_shaped_text() {
        let mut policy = ConstrainedValue::constrained(
            AskForApproval::Never,
            vec![AskForApproval::Never],
            "approval_policy",
            "[Never]",
            "requirements.toml",
        );
        assert_eq!(policy.set(AskForApproval::Never), Ok(()));
        let err = policy.set(AskForApproval::OnRequest).unwrap_err();
        assert_eq!(
            err.0,
            "invalid value for `approval_policy`: `OnRequest` is not in the allowed set [Never] (set by requirements.toml)"
        );
        assert_eq!(policy.value(), AskForApproval::Never);
    }

    #[test]
    fn profile_classification_follows_the_substrate_write_surface() {
        assert_eq!(
            classify_permission_profile(&PermissionProfile::read_only()),
            SandboxModeClass::ReadOnly
        );
        assert_eq!(
            classify_permission_profile(&PermissionProfile::workspace_write()),
            SandboxModeClass::WorkspaceWrite
        );
        assert_eq!(
            classify_permission_profile(&PermissionProfile::Disabled),
            SandboxModeClass::DangerFullAccess
        );
    }

    #[test]
    fn class_verdicts_gate_profile_installation_fail_closed() {
        let mut permissions = SessionPermissions::new(
            ConstrainedValue::unconstrained(AskForApproval::OnRequest),
            SessionPermissionSnapshot::legacy(PermissionProfile::read_only()),
            ProfileClassVerdicts {
                danger_full_access: Some("no full access (set by requirements.toml)".to_string()),
                ..Default::default()
            },
            Vec::new(),
            None,
        );
        assert!(
            permissions
                .set_permission_profile(PermissionProfile::workspace_write())
                .is_ok()
        );
        let err = permissions
            .set_permission_profile(PermissionProfile::Disabled)
            .unwrap_err();
        assert_eq!(err.0, "no full access (set by requirements.toml)");
        permissions
            .replace_permission_profile_from_session_snapshot(SessionPermissionSnapshot::legacy(
                PermissionProfile::Disabled,
            ))
            .unwrap();
        assert_eq!(
            permissions.permission_profile(),
            &PermissionProfile::Disabled
        );
        assert!(
            permissions
                .set_permission_profile(PermissionProfile::read_only())
                .is_err()
        );
    }

    #[test]
    fn feature_rows_read_and_toggle_by_key_fail_closed() {
        let mut features = SessionFeatures::from_rows(vec![
            SessionFeatureRow {
                key: "goals".to_string(),
                enabled: true,
                default_enabled: true,
                ..Default::default()
            },
            SessionFeatureRow {
                key: "guardian_approval".to_string(),
                enabled: false,
                default_enabled: false,
                enable_verdict: Some("pinned off (set by requirements.toml)".to_string()),
                ..Default::default()
            },
        ]);
        assert!(features.enabled_key("goals"));
        assert!(!features.enabled_key("nonexistent"));
        assert_eq!(features.key_default_enabled("nonexistent"), None);
        assert!(features.set_enabled_by_key("goals", false).is_ok());
        assert!(!features.enabled_key("goals"));
        let err = features
            .set_enabled_by_key("guardian_approval", true)
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(features.set_enabled_by_key("nonexistent", true).is_err());
        assert_eq!(
            features.toggle_verdict("guardian_approval", true),
            Some("pinned off (set by requirements.toml)".to_string())
        );
        assert_eq!(features.toggle_verdict("guardian_approval", false), None);
    }

    #[test]
    fn windows_level_resolves_mode_override_then_feature_fallback() {
        let mut permissions = SessionPermissions::degraded_default();
        let features = SessionFeatures::from_rows(vec![SessionFeatureRow {
            key: "experimental_windows_sandbox".to_string(),
            enabled: true,
            ..Default::default()
        }]);
        assert_eq!(
            windows_sandbox_level(&permissions, &features),
            WindowsSandboxLevel::RestrictedToken
        );
        permissions.windows_sandbox_mode = Some(WindowsSandboxMode::Elevated);
        assert_eq!(
            windows_sandbox_level(&permissions, &features),
            WindowsSandboxLevel::Elevated
        );
    }
}
