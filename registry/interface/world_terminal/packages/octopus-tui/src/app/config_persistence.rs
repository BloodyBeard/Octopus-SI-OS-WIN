use super::*;
#[cfg(target_os = "windows")]
use crate::approval_presets::ApprovalPreset;

#[cfg(target_os = "windows")]
pub(super) struct WindowsSetupPermissions {
    pub(super) permission_profile: PermissionProfile,
    pub(super) workspace_roots: Vec<AbsolutePathBuf>,
}

impl App {
    pub(super) async fn rebuild_config_for_cwd(&self, cwd: PathBuf) -> Result<TuiConfig> {
        let mut request = self.load_request.clone();
        request.codex_home = Some(self.config.codex_home.to_path_buf());
        request.harness.cwd = Some(cwd.clone());
        let cwd_display = cwd.display().to_string();
        let snapshot = self
            .config_runtime
            .load(request)
            .await
            .wrap_err_with(|| format!("Failed to rebuild config for cwd {cwd_display}"))?;
        Ok(TuiConfig::from_snapshot(snapshot)?)
    }

    pub(super) async fn rebuild_config_for_permission_profile(
        &self,
        profile_id: &str,
    ) -> Result<TuiConfig> {
        let mut request = self.load_request.clone();
        request.codex_home = Some(self.config.codex_home.to_path_buf());
        request.harness.cwd = Some(self.chat_widget.config_ref().cwd.to_path_buf());
        request.harness.sandbox_mode = None;
        request.harness.default_permissions = Some(profile_id.to_string());
        let snapshot = self.config_runtime.load(request).await.wrap_err_with(|| {
            format!("Failed to rebuild config for permission profile {profile_id}")
        })?;
        Ok(TuiConfig::from_snapshot(snapshot)?)
    }

    #[cfg(target_os = "windows")]
    pub(super) async fn windows_setup_permissions(
        &self,
        preset: &ApprovalPreset,
        profile_selection: Option<&PermissionProfileSelection>,
    ) -> Result<WindowsSetupPermissions> {
        match profile_selection {
            Some(selection) => {
                let selected_config = self
                    .rebuild_config_for_permission_profile(selection.profile_id.as_str())
                    .await?;
                Ok(WindowsSetupPermissions {
                    permission_profile: selected_config.permissions.permission_profile().clone(),
                    workspace_roots: selected_config.effective_workspace_roots(),
                })
            }
            None => Ok(WindowsSetupPermissions {
                permission_profile: preset.permission_profile.clone(),
                workspace_roots: self.config.effective_workspace_roots(),
            }),
        }
    }

    pub(super) async fn apply_permission_profile_selection(
        &mut self,
        selection: PermissionProfileSelection,
    ) -> bool {
        let PermissionProfileSelection {
            profile_id,
            approval_policy,
            approvals_reviewer,
            display_label,
        } = selection;
        let selected_config = match self
            .rebuild_config_for_permission_profile(profile_id.as_str())
            .await
        {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    profile_id,
                    "failed to resolve selected permission profile"
                );
                self.chat_widget.add_error_message(format!(
                    "Failed to set permission profile `{profile_id}`: {err}"
                ));
                return false;
            }
        };
        let permission_profile = selected_config.permissions.permission_profile();
        let active_permission_profile = selected_config.permissions.active_permission_profile();

        let mut config = self.config.clone();
        if let Some(policy) = approval_policy
            && !self.try_set_approval_policy_on_config(
                &mut config,
                policy,
                "Failed to set approval policy",
                "failed to set selected permission profile approval policy on app config",
            )
        {
            return false;
        }
        if let Err(err) = config
            .permissions
            .set_permission_profile_from_session_snapshot(
                SessionPermissionSnapshot::from_session_snapshot(
                    permission_profile.clone(),
                    active_permission_profile.clone(),
                ),
            )
        {
            tracing::warn!(
                error = %err,
                profile_id,
                "failed to set selected permission profile on app config"
            );
            self.chat_widget.add_error_message(format!(
                "Failed to set permission profile `{profile_id}`: {err}"
            ));
            return false;
        }
        if let Some(reviewer) = approvals_reviewer {
            config.approvals_reviewer = reviewer;
        }
        self.config = config;

        if let Some(policy) = approval_policy {
            self.runtime_approval_policy_override = Some(policy);
            self.chat_widget.set_approval_policy(policy);
        }
        if let Err(err) = self.chat_widget.set_permission_profile_with_active_profile(
            permission_profile.clone(),
            active_permission_profile.clone(),
        ) {
            tracing::warn!(
                error = %err,
                profile_id,
                "failed to set selected permission profile on chat config"
            );
            self.chat_widget.add_error_message(format!(
                "Failed to set permission profile `{profile_id}`: {err}"
            ));
            return false;
        }
        if let Some(reviewer) = approvals_reviewer {
            self.chat_widget.set_approvals_reviewer(reviewer);
        }
        self.runtime_permission_profile_override =
            Some(RuntimePermissionProfileOverride::from_config(&self.config));
        self.app_event_tx
            .send(AppEvent::CodexOp(AppCommand::override_turn_context(
                None,
                approval_policy,
                approvals_reviewer,
                Some(permission_profile.clone()),
                active_permission_profile,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )));
        self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::new_info_event(format!("Permissions updated to {display_label}"), None),
        )));
        true
    }

    pub(super) async fn refresh_in_memory_config_from_disk(&mut self) -> Result<()> {
        let mut config = self
            .rebuild_config_for_cwd(self.chat_widget.config_ref().cwd.to_path_buf())
            .await?;
        self.apply_runtime_policy_overrides(&mut config);
        self.config = config;
        self.chat_widget
            .sync_runtime_projection_config(&self.config);
        Ok(())
    }

    pub(super) async fn refresh_in_memory_config_from_disk_best_effort(&mut self, action: &str) {
        if let Err(err) = self.refresh_in_memory_config_from_disk().await {
            tracing::warn!(
                error = %err,
                action,
                "failed to refresh config before thread transition; continuing with current in-memory config"
            );
        }
    }

    pub(super) async fn rebuild_config_for_resume_or_fallback(
        &mut self,
        current_cwd: &Path,
        resume_cwd: PathBuf,
    ) -> Result<TuiConfig> {
        match self.rebuild_config_for_cwd(resume_cwd.clone()).await {
            Ok(config) => Ok(config),
            Err(err) => {
                if !octopus_utils_path::paths_match_after_normalization(current_cwd, &resume_cwd) {
                    Err(err)
                } else {
                    let resume_cwd_display = resume_cwd.display().to_string();
                    tracing::warn!(
                        error = %err,
                        cwd = %resume_cwd_display,
                        "failed to rebuild config for same-cwd resume; using current in-memory config"
                    );
                    Ok(self.config.clone())
                }
            }
        }
    }

    pub(super) fn apply_runtime_policy_overrides(&mut self, config: &mut TuiConfig) {
        if let Some(policy) = self.runtime_approval_policy_override.as_ref()
            && let Err(err) = config.permissions.approval_policy.set(*policy)
        {
            tracing::warn!(%err, "failed to carry forward approval policy override");
            self.chat_widget.add_error_message(format!(
                "Failed to carry forward approval policy override: {err}"
            ));
        }
        if let Some(profile_override) = self.runtime_permission_profile_override.as_ref() {
            match config
                .permissions
                .set_permission_profile_from_session_snapshot(
                    SessionPermissionSnapshot::from_session_snapshot(
                        profile_override.permission_profile.clone(),
                        profile_override.active_permission_profile.clone(),
                    ),
                ) {
                Ok(()) => {}
                Err(err) => {
                    tracing::warn!(%err, "failed to carry forward permission profile override");
                    self.chat_widget.add_error_message(format!(
                        "Failed to carry forward permission profile override: {err}"
                    ));
                }
            }
        }
    }

    pub(super) fn set_approvals_reviewer_in_app_and_widget(&mut self, reviewer: ApprovalsReviewer) {
        self.config.approvals_reviewer = reviewer;
        self.chat_widget.set_approvals_reviewer(reviewer);
    }

    pub(super) fn try_set_approval_policy_on_config(
        &mut self,
        config: &mut TuiConfig,
        policy: AskForApproval,
        user_message_prefix: &str,
        log_message: &str,
    ) -> bool {
        if let Err(err) = config.permissions.approval_policy.set(policy) {
            tracing::warn!(error = %err, "{log_message}");
            self.chat_widget
                .add_error_message(format!("{user_message_prefix}: {err}"));
            return false;
        }

        true
    }

    pub(super) fn try_set_builtin_active_permission_profile_on_config(
        &mut self,
        config: &mut TuiConfig,
        active_permission_profile: ActivePermissionProfile,
        user_message_prefix: &str,
        log_message: &str,
    ) -> Option<PermissionProfile> {
        let Some(permission_profile) =
            crate::approval_presets::builtin_permission_profile_for_active_permission_profile(
                &active_permission_profile,
            )
        else {
            tracing::warn!(
                id = %active_permission_profile.id,
                "{log_message}: unsupported active permission profile"
            );
            self.chat_widget.add_error_message(format!(
                "{user_message_prefix}: unsupported active permission profile `{}`",
                active_permission_profile.id
            ));
            return None;
        };

        if let Err(err) = config
            .permissions
            .set_permission_profile_from_session_snapshot(SessionPermissionSnapshot::active(
                permission_profile.clone(),
                active_permission_profile,
            ))
        {
            tracing::warn!(error = %err, "{log_message}");
            self.chat_widget
                .add_error_message(format!("{user_message_prefix}: {err}"));
            return None;
        }

        Some(permission_profile)
    }

    pub(super) async fn update_feature_flags(
        &mut self,
        app_server: &mut AppServerSession,
        updates: Vec<(String, bool)>,
    ) {
        if updates.is_empty() {
            return;
        }

        let auto_review_preset = auto_review_mode();
        let mut next_config = self.config.clone();
        let windows_sandbox_changed = updates.iter().any(|(feature_key, _)| {
            matches!(
                feature_key.as_str(),
                "experimental_windows_sandbox" | "elevated_windows_sandbox"
            )
        });
        let mut approval_policy_override = None;
        let mut approvals_reviewer_override = None;
        let mut permission_profile_override = None;
        let mut active_permission_profile_override = None;
        let mut feature_updates_to_apply = Vec::with_capacity(updates.len());
        let mut permissions_history_label: Option<&'static str> = None;
        let mut config_edits = Vec::new();

        for (feature_key, enabled) in updates {
            let feature_key = feature_key.as_str();
            let mut feature_edits = Vec::new();
            let mut feature_config = next_config.clone();
            if let Err(err) = feature_config
                .features
                .set_enabled_by_key(feature_key, enabled)
            {
                tracing::error!(
                    error = %err,
                    feature = feature_key,
                    "failed to update constrained feature flags"
                );
                self.chat_widget.add_error_message(format!(
                    "Failed to update experimental feature `{feature_key}`: {err}"
                ));
                continue;
            }
            let effective_enabled = feature_config.features.enabled_key(feature_key);
            if feature_key == "guardian_approval" {
                let previous_approvals_reviewer = feature_config.approvals_reviewer;
                if effective_enabled {
                    feature_config.approvals_reviewer = auto_review_preset.approvals_reviewer;
                    feature_edits.push(crate::config_update::replace_config_value(
                        "approvals_reviewer",
                        serde_json::json!(auto_review_preset.approvals_reviewer.to_string()),
                    ));
                    if previous_approvals_reviewer != auto_review_preset.approvals_reviewer {
                        permissions_history_label = Some("Approve for me");
                    }
                } else if !effective_enabled {
                    feature_edits.push(crate::config_update::clear_config_value(
                        "approvals_reviewer",
                    ));
                    feature_config.approvals_reviewer = ApprovalsReviewer::User;
                    if previous_approvals_reviewer != ApprovalsReviewer::User {
                        permissions_history_label = Some("Ask for approval");
                    }
                }
                approvals_reviewer_override = Some(feature_config.approvals_reviewer);
            }
            if feature_key == "guardian_approval" && effective_enabled {
                if !self.try_set_approval_policy_on_config(
                    &mut feature_config,
                    auto_review_preset.approval_policy,
                    "Failed to enable Approve for me",
                    "failed to set auto-review approval policy on staged config",
                ) {
                    continue;
                }
                let Some(permission_profile) = self
                    .try_set_builtin_active_permission_profile_on_config(
                        &mut feature_config,
                        auto_review_preset.active_permission_profile.clone(),
                        "Failed to enable Approve for me",
                        "failed to set auto-review permission profile on staged config",
                    )
                else {
                    continue;
                };
                feature_edits.extend([
                    crate::config_update::replace_config_value(
                        "approval_policy",
                        serde_json::json!("on-request"),
                    ),
                    crate::config_update::replace_config_value(
                        "sandbox_mode",
                        serde_json::json!("workspace-write"),
                    ),
                ]);
                approval_policy_override = Some(auto_review_preset.approval_policy);
                permission_profile_override = Some(permission_profile);
                active_permission_profile_override =
                    Some(auto_review_preset.active_permission_profile.clone());
            }
            let feature_default_enabled = feature_config.features.key_default_enabled(feature_key);
            next_config = feature_config;
            feature_updates_to_apply.push((feature_key.to_string(), effective_enabled));
            config_edits.extend(feature_edits);
            config_edits.push(crate::config_update::build_feature_enabled_edit(
                feature_key,
                effective_enabled,
                feature_default_enabled,
            ));
        }

        let _ = app_server;
        if let Err(err) = crate::config_update::write_config_batch_local(
            self.config_runtime.as_ref(),
            config_edits,
        )
        .await
        {
            let error = crate::config_update::format_config_error(&err);
            tracing::error!(error = %error, "failed to persist feature flags");
            self.chat_widget
                .add_error_message(format!("Failed to update experimental features: {error}"));
            return;
        }

        let memory_tool_was_enabled = self.config.features.enabled_key("memories");
        self.config = next_config;
        let show_memory_enable_notice =
            feature_updates_to_apply
                .iter()
                .any(|(feature_key, enabled)| {
                    feature_key == "memories" && *enabled && !memory_tool_was_enabled
                });
        for (feature_key, effective_enabled) in feature_updates_to_apply {
            self.chat_widget
                .set_feature_enabled(&feature_key, effective_enabled);
        }
        if show_memory_enable_notice {
            self.chat_widget.add_memories_enable_notice();
        }
        if approvals_reviewer_override.is_some() {
            self.set_approvals_reviewer_in_app_and_widget(self.config.approvals_reviewer);
        }
        if approval_policy_override.is_some() {
            self.chat_widget.set_approval_policy(AskForApproval::from(
                self.config.permissions.approval_policy.value(),
            ));
        }
        let permission_profile_override_value = permission_profile_override
            .is_some()
            .then(|| self.config.permissions.permission_profile().clone());
        if let Some(permission_profile) = permission_profile_override_value.as_ref()
            && let Err(err) = self
                .chat_widget
                .set_permission_profile_from_session_snapshot(
                    SessionPermissionSnapshot::from_session_snapshot(
                        permission_profile.clone(),
                        active_permission_profile_override.clone(),
                    ),
                )
        {
            tracing::error!(
                error = %err,
                "failed to set auto-review permission profile on chat config"
            );
            self.chat_widget
                .add_error_message(format!("Failed to enable Approve for me: {err}"));
        }
        if permission_profile_override.is_some() {
            self.runtime_permission_profile_override =
                Some(RuntimePermissionProfileOverride::from_config(&self.config));
        }

        if approval_policy_override.is_some()
            || approvals_reviewer_override.is_some()
            || permission_profile_override.is_some()
        {
            let op = AppCommand::override_turn_context(
                None,
                approval_policy_override,
                approvals_reviewer_override,
                permission_profile_override,
                active_permission_profile_override,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            );
            self.chat_widget.submit_op(op);
        }

        if windows_sandbox_changed {
            self.propagate_windows_sandbox_turn_context();
        }

        if let Some(label) = permissions_history_label {
            self.chat_widget
                .add_info_message(format!("Permissions updated to {label}"), None);
        }
    }

    pub(super) async fn update_memory_settings(
        &mut self,
        app_server: &mut AppServerSession,
        use_memories: bool,
        generate_memories: bool,
    ) -> bool {
        let edits =
            crate::config_update::build_memory_settings_edits(use_memories, generate_memories);

        let _ = app_server;
        if let Err(err) =
            crate::config_update::write_config_batch_local(self.config_runtime.as_ref(), edits)
                .await
        {
            tracing::error!(error = %err, "failed to persist memory settings");
            self.chat_widget
                .add_error_message(format!("Failed to save memory settings: {err}"));
            return false;
        }

        self.config.memories.use_memories = use_memories;
        self.config.memories.generate_memories = generate_memories;
        self.chat_widget
            .set_memory_settings(use_memories, generate_memories);
        true
    }

    pub(super) async fn update_memory_settings_with_app_server(
        &mut self,
        app_server: &mut AppServerSession,
        use_memories: bool,
        generate_memories: bool,
    ) {
        if !self
            .update_memory_settings(app_server, use_memories, generate_memories)
            .await
        {
            return;
        }
    }

    pub(super) async fn reset_memories_with_app_server(
        &mut self,
        _app_server: &mut AppServerSession,
    ) {
        let memory_root = self.config.codex_home.join("memories");
        if let Err(err) = reset_local_memories_dir(&memory_root) {
            tracing::error!(error = %err, "failed to reset memories");
            self.chat_widget
                .add_error_message(format!("Failed to reset memories: {err}"));
            return;
        }

        self.chat_widget
            .add_info_message("Reset local memories.".to_string(), None);
    }

    pub(super) fn reasoning_label(reasoning_effort: Option<&ReasoningEffortConfig>) -> String {
        match reasoning_effort {
            None | Some(ReasoningEffortConfig::None) => "default".to_string(),
            Some(reasoning_effort) => reasoning_effort.as_str().to_string(),
        }
    }

    pub(super) fn reasoning_label_for(
        model: &str,
        reasoning_effort: Option<&ReasoningEffortConfig>,
    ) -> Option<String> {
        (!model.starts_with("codex-auto-")).then(|| Self::reasoning_label(reasoning_effort))
    }

    pub(crate) fn token_usage(&self) -> crate::token_usage::TokenUsage {
        self.chat_widget.token_usage()
    }

    pub(super) fn on_update_reasoning_effort(&mut self, effort: Option<ReasoningEffortConfig>) {
        self.config.model_reasoning_effort = effort.clone();
        self.chat_widget.set_reasoning_effort(effort);
    }

    pub(super) fn on_update_personality(&mut self, personality: Personality) {
        self.config.personality = Some(personality);
        self.chat_widget.set_personality(personality);
    }

    pub(super) fn sync_tui_theme_selection(&mut self, name: String) {
        self.config.terminal.theme = Some(name.clone());
        self.chat_widget.set_tui_theme(Some(name));
    }

    #[cfg(test)]
    pub(super) fn sync_tui_pet_selection(&mut self, pet: String) {
        self.config.terminal.pet = Some(pet.clone());
        self.chat_widget.set_tui_pet(Some(pet));
    }

    pub(super) fn sync_tui_pet_disabled(&mut self) {
        let pet = crate::pets::DISABLED_PET_ID.to_string();
        self.config.terminal.pet = Some(pet.clone());
        self.chat_widget.set_tui_pet(Some(pet));
    }

    pub(super) fn restore_runtime_theme_from_config(&self) {
        if let Some(name) = self.config.terminal.theme.as_deref()
            && let Some(theme) =
                crate::render::highlight::resolve_theme_by_name(name, Some(&self.config.codex_home))
        {
            crate::render::highlight::set_syntax_theme(theme);
            return;
        }

        let auto_theme_name = crate::render::highlight::adaptive_default_theme_name();
        if let Some(theme) = crate::render::highlight::resolve_theme_by_name(
            auto_theme_name,
            Some(&self.config.codex_home),
        ) {
            crate::render::highlight::set_syntax_theme(theme);
        }
    }

    pub(super) fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "None",
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    fn propagate_windows_sandbox_turn_context(&self) {
        #[cfg(target_os = "windows")]
        {
            let windows_sandbox_level = crate::windows_sandbox::level_from_config(&self.config);
            self.app_event_tx
                .send(AppEvent::CodexOp(AppCommand::override_turn_context(
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(windows_sandbox_level),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )));
        }
    }
}

fn reset_local_memories_dir(memory_root: &std::path::Path) -> Result<()> {
    if memory_root.exists() {
        std::fs::remove_dir_all(memory_root).wrap_err_with(|| {
            format!("removing local memory directory {}", memory_root.display())
        })?;
    }
    std::fs::create_dir_all(memory_root).wrap_err_with(|| {
        format!(
            "recreating local memory directory {}",
            memory_root.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::app_enabled_in_effective_config;
    use crate::app::test_support::make_test_app;
    use crate::app::test_support::make_test_app_with_runtime;
    use crate::test_support::PathBufExt;
    use crate::test_support::write_test_config;
    use crate::test_support::write_test_model_provider_config;
    use octopus_substrate_contract::permission_profile::PermissionProfile;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[tokio::test]
    async fn update_reasoning_effort_updates_collaboration_mode() {
        let mut app = make_test_app().await;
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

        app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_loads_latest_apps_state() -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        write_test_model_provider_config(codex_home.path());
        app.config
            .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
        let app_id = "unit_test_refresh_in_memory_config_connector".to_string();

        assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

        write_test_config(
            codex_home.path(),
            format!(
                r#"
[apps.{app_id}]
enabled = false
disabled_reason = "user"
"#
            )
            .as_str(),
        );

        assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

        app.refresh_in_memory_config_from_disk().await?;

        assert_eq!(
            app_enabled_in_effective_config(&app.config, &app_id),
            Some(false)
        );
        Ok(())
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_keeps_requirement_pinned_approval_policy()
    -> Result<()> {
        let (mut app, runtime) = make_test_app_with_runtime().await;
        let codex_home = tempdir()?;
        write_test_model_provider_config(codex_home.path());
        app.config
            .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
        let required_policy =
            octopus_substrate_contract::approvals_substrate::AskForApproval::Never;
        runtime.set_snapshot_tweak(move |snapshot| {
            snapshot.permissions.approval_policy =
                crate::session_posture::ConstrainedValue::constrained(
                    required_policy,
                    vec![required_policy],
                    "approval_policy",
                    "[never]",
                    "enterprise requirements",
                );
        });

        let assert_pinned_policy = |app: &App| {
            let config = app.fresh_session_config();
            assert_eq!(config.permissions.approval_policy.value(), required_policy);
            assert!(
                config
                    .permissions
                    .approval_policy
                    .can_set(
                        &octopus_substrate_contract::approvals_substrate::AskForApproval::OnRequest
                    )
                    .is_err()
            );
        };

        app.refresh_in_memory_config_from_disk().await?;
        assert_pinned_policy(&app);

        app.refresh_in_memory_config_from_disk().await?;
        assert_pinned_policy(&app);
        Ok(())
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_best_effort_keeps_current_config_on_error()
    -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config
            .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
        std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
        let original_config = app.config.clone();

        app.refresh_in_memory_config_from_disk_best_effort("starting a new thread")
            .await;

        assert_eq!(app.config, original_config);
        Ok(())
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_uses_active_chat_widget_cwd() -> Result<()> {
        let mut app = make_test_app().await;
        let original_cwd = app.config.cwd.clone();
        let next_cwd_tmp = tempdir()?;
        let next_cwd = next_cwd_tmp.path().to_path_buf();

        app.chat_widget
            .handle_thread_session(crate::session_state::ThreadSessionState {
                thread_id: uuid::Uuid::now_v7().to_string(),
                forked_from_id: None,
                fork_parent_title: None,
                thread_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                permission_profile: PermissionProfile::read_only(),
                active_permission_profile: None,
                cwd: next_cwd.clone().abs(),
                runtime_workspace_roots: Vec::new(),
                instruction_source_paths: Vec::new(),
                reasoning_effort: None,
                collaboration_mode: None,
                personality: None,
                network_proxy: None,
                rollout_path: Some(PathBuf::new()),
            });

        assert_eq!(app.chat_widget.config_ref().cwd.to_path_buf(), next_cwd);
        assert_eq!(app.config.cwd, original_cwd);

        app.refresh_in_memory_config_from_disk().await?;

        assert_eq!(app.config.cwd, app.chat_widget.config_ref().cwd);
        Ok(())
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_updates_resize_reflow_config() -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config
            .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
        write_test_config(
            codex_home.path(),
            r#"
[tui]
terminal_resize_reflow_max_rows = 9000
"#,
        );

        app.refresh_in_memory_config_from_disk().await?;

        assert_eq!(
            app.config.terminal.resize_reflow.max_rows,
            octopus_world_terminal_config::TerminalResizeReflowMaxRows::Limit(9000)
        );
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_config_for_resume_or_fallback_uses_current_config_on_same_cwd_error()
    -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config
            .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
        std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
        let current_config = app.config.clone();
        let current_cwd = current_config.cwd.clone();

        let resume_config = app
            .rebuild_config_for_resume_or_fallback(&current_cwd, current_cwd.to_path_buf())
            .await?;

        assert_eq!(resume_config, current_config);
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_config_for_resume_or_fallback_errors_when_cwd_changes() -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config
            .set_codex_home_for_tests(codex_home.path().to_path_buf().abs());
        std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
        let current_cwd = app.config.cwd.clone();
        let next_cwd_tmp = tempdir()?;
        let next_cwd = next_cwd_tmp.path().to_path_buf();

        let result = app
            .rebuild_config_for_resume_or_fallback(&current_cwd, next_cwd)
            .await;

        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn sync_tui_theme_selection_updates_chat_widget_config_copy() {
        let mut app = make_test_app().await;

        app.sync_tui_theme_selection("dracula".to_string());

        assert_eq!(app.config.terminal.theme.as_deref(), Some("dracula"));
        assert_eq!(
            app.chat_widget.config_ref().terminal.theme.as_deref(),
            Some("dracula")
        );
    }

    #[tokio::test]
    async fn sync_tui_pet_selection_updates_chat_widget_config_copy() {
        let mut app = make_test_app().await;

        app.sync_tui_pet_selection("chefito".to_string());

        assert_eq!(app.config.terminal.pet.as_deref(), Some("chefito"));
        assert_eq!(
            app.chat_widget.config_ref().terminal.pet.as_deref(),
            Some("chefito")
        );
    }

    #[tokio::test]
    async fn sync_tui_pet_disabled_updates_chat_widget_config_copy() {
        let mut app = make_test_app().await;

        app.sync_tui_pet_disabled();

        assert_eq!(
            app.config.terminal.pet.as_deref(),
            Some(crate::pets::DISABLED_PET_ID)
        );
        assert_eq!(
            app.chat_widget.config_ref().terminal.pet.as_deref(),
            Some(crate::pets::DISABLED_PET_ID)
        );
    }
}
