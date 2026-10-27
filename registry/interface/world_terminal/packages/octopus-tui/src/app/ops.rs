use super::*;

impl App {
    pub(super) async fn submit_op(
        &mut self,
        app_server: &mut AppServerSession,
        op: AppCommand,
    ) -> Result<()> {
        crate::session_log::log_outbound_op(&op);
        match op {
            AppCommand::Interrupt { .. } => {
                if let Err(err) = app_server.turn_interrupt().await {
                    self.chat_widget
                        .add_error_message(format!("turn/interrupt failed in TUI: {err}"));
                }
            }
            AppCommand::UserTurn {
                items,
                cwd,
                approval_policy,
                approvals_reviewer,
                active_permission_profile,
                model: _,
                effort: _,
                summary,
                service_tier,
                final_output_json_schema,
                collaboration_mode,
                personality,
            } => {
                let (permissions_override, approvals_reviewer) = {
                    let config = self.chat_widget.config_ref();
                    (
                        Self::turn_permissions_override_from_config(
                            config,
                            active_permission_profile.as_ref(),
                            self.runtime_permission_profile_override
                                .as_ref()
                                .map(|profile| &profile.permission_profile),
                        ),
                        approvals_reviewer.unwrap_or(config.approvals_reviewer),
                    )
                };
                let workspace_roots = self
                    .chat_widget
                    .config_ref()
                    .permissions
                    .user_visible_workspace_roots();
                if let Err(err) = app_server
                    .turn_start(
                        items,
                        cwd,
                        approval_policy,
                        approvals_reviewer,
                        permissions_override,
                        &workspace_roots,
                        summary,
                        service_tier,
                        collaboration_mode,
                        personality,
                        final_output_json_schema,
                    )
                    .await
                {
                    self.chat_widget
                        .add_error_message(format!("user submit failed in TUI: {err}"));
                }
            }
            AppCommand::ApprovalDecision {
                request_id,
                decision,
            } => {
                if let Err(err) = app_server.respond_approval(&request_id, decision).await {
                    self.chat_widget
                        .add_error_message(format!("approval response failed in TUI: {err}"));
                }
            }
            AppCommand::ElicitationAnswer { request_id, answer } => {
                if let Err(err) = app_server.respond_elicitation(&request_id, answer).await {
                    self.chat_widget
                        .add_error_message(format!("elicitation answer failed in TUI: {err}"));
                }
            }
            AppCommand::UserInputAnswer {
                request_id,
                answers,
            } => {
                if let Err(err) = app_server.respond_user_input(&request_id, answers).await {
                    self.chat_widget
                        .add_error_message(format!("user-input answer failed in TUI: {err}"));
                }
            }
            AppCommand::Compact => match app_server.thread_compact_start().await {
                Ok(()) => {}
                Err(err) => {
                    self.chat_widget.on_task_complete(None, None, false);
                    self.chat_widget
                        .add_error_message(format!("Compaction failed: {err}"));
                }
            },
            AppCommand::RunUserShellCommand { command } => {
                let item_id = format!("terminal-shell-{}", uuid::Uuid::now_v7());
                self.chat_widget.handle_command_execution_started_now(
                    octopus_world_terminal_client::ExecCommandItem {
                        item_id: item_id.clone(),
                        command: command.clone(),
                        cwd: String::new(),
                        process_id: None,
                        command_actions: Vec::new(),
                        source: octopus_world_terminal_client::ExecItemSource::UserShell,
                        status: octopus_world_terminal_client::ExecItemStatus::InProgress,
                        exit_code: None,
                        duration_ms: None,
                        aggregated_output: None,
                    },
                );
                if let Err(err) = app_server.thread_shell_command(
                    item_id.clone(),
                    command.clone(),
                    self.app_event_tx.clone(),
                ) {
                    self.chat_widget.handle_command_execution_completed_now(
                        octopus_world_terminal_client::ExecCommandItem {
                            item_id,
                            command,
                            cwd: String::new(),
                            process_id: None,
                            command_actions: Vec::new(),
                            source: octopus_world_terminal_client::ExecItemSource::UserShell,
                            status: octopus_world_terminal_client::ExecItemStatus::Failed,
                            exit_code: Some(1),
                            duration_ms: Some(0),
                            aggregated_output: Some(err.to_string()),
                        },
                    );
                }
            }
            AppCommand::SetThreadName { name } => {
                let Some(run) = app_server.active_run_id() else {
                    self.chat_widget.add_error_message(
                        "Failed to rename task: no active run is attached.".to_string(),
                    );
                    return Ok(());
                };
                match app_server.rename_task(&run, &name) {
                    Ok(()) => self
                        .chat_widget
                        .on_thread_name_updated(run.as_str().to_string(), Some(name)),
                    Err(err) => self
                        .chat_widget
                        .add_error_message(format!("Failed to rename task: {err}")),
                }
            }
            AppCommand::ThreadRollback { num_turns } => {
                match app_server.thread_rollback(num_turns).await {
                    Ok(()) => self.handle_backtrack_rollback_succeeded(num_turns),
                    Err(err) => {
                        self.handle_backtrack_rollback_failed();
                        self.chat_widget
                            .add_error_message(format!("Rollback failed: {err}"));
                    }
                }
            }
            AppCommand::Review { target } => {
                if let Err(err) = app_server.review_start(target).await {
                    self.chat_widget
                        .add_error_message(format!("Failed to start review: {err}"));
                }
            }
            AppCommand::CleanBackgroundTerminals => {
                if let Err(err) = app_server.thread_background_terminals_clean().await {
                    self.chat_widget
                        .add_error_message(format!("Failed to clean background terminals: {err}"));
                }
            }
            AppCommand::ApproveGuardianDeniedAction { event } => {
                if let Err(err) = app_server
                    .thread_approve_guardian_denied_action(&event)
                    .await
                {
                    self.chat_widget
                        .add_error_message(format!("Failed to approve denied action: {err}"));
                }
            }
            AppCommand::OverrideTurnContext { .. } => {}
        }
        Ok(())
    }

    pub(super) fn turn_permissions_override_from_config(
        config: &TuiConfig,
        active_permission_profile: Option<&ActivePermissionProfile>,
        runtime_permission_profile_override: Option<&PermissionProfile>,
    ) -> crate::app_server_session::TurnPermissionsOverride {
        use crate::app_server_session::TurnPermissionsOverride;
        if let Some(active_permission_profile) = active_permission_profile {
            return TurnPermissionsOverride::ActiveProfile(active_permission_profile.clone());
        }

        let effective_permission_profile = config.permissions.effective_permission_profile();
        let runtime_permission_profile_override =
            runtime_permission_profile_override.map(|profile| {
                profile
                    .clone()
                    .materialize_project_roots_with_workspace_roots(
                        &config.effective_workspace_roots(),
                    )
            });
        if runtime_permission_profile_override
            .as_ref()
            .is_some_and(|profile| profile == &effective_permission_profile)
        {
            return TurnPermissionsOverride::LegacySandbox(effective_permission_profile);
        }

        TurnPermissionsOverride::Preserve
    }

    pub(super) fn lookup_prompt_history_entry(&mut self, offset: usize, log_id: u64) {
        let entry = self
            .prompt_history
            .as_ref()
            .filter(|_| log_id == crate::app::PROMPT_HISTORY_SNAPSHOT_ID)
            .and_then(|history| history.entry_text(offset));
        self.app_event_tx
            .send(AppEvent::PromptHistoryEntryResponse {
                event: crate::app_event::HistoryLookupResponse {
                    offset,
                    log_id,
                    entry,
                },
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server_session::TurnPermissionsOverride;
    use crate::test_support::write_test_model_provider_config;
    use octopus_substrate_contract::permission_profile::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;

    async fn config_with_workspace_profile() -> TuiConfig {
        let temp_dir = tempfile::tempdir().expect("tempdir").keep();
        write_test_model_provider_config(&temp_dir);
        let mut snapshot = crate::test_support::test_session_snapshot(
            octopus_util_abspath::AbsolutePathBuf::from_absolute_path(&temp_dir)
                .expect("absolute codex home"),
            octopus_util_abspath::AbsolutePathBuf::current_dir().expect("cwd"),
        );
        snapshot.permissions = crate::session_posture::SessionPermissions::new(
            crate::session_posture::ConstrainedValue::unconstrained(
                octopus_substrate_contract::approvals_substrate::AskForApproval::OnRequest,
            ),
            crate::session_posture::SessionPermissionSnapshot::active(
                octopus_substrate_contract::permission_profile::PermissionProfile::workspace_write(
                ),
                octopus_substrate_contract::permission_profile::ActivePermissionProfile::new(
                    BUILT_IN_PERMISSION_PROFILE_WORKSPACE,
                ),
            ),
            crate::session_posture::ProfileClassVerdicts::default(),
            vec![snapshot.cwd.clone()],
            None,
        );
        TuiConfig::with_default_terminal(snapshot)
    }

    #[tokio::test]
    async fn turn_permissions_use_active_profile_when_available() {
        let config = config_with_workspace_profile().await;
        let active_permission_profile = config.permissions.active_permission_profile();

        assert_eq!(
            App::turn_permissions_override_from_config(
                &config,
                active_permission_profile.as_ref(),
                None,
            ),
            TurnPermissionsOverride::ActiveProfile(ActivePermissionProfile::new(
                BUILT_IN_PERMISSION_PROFILE_WORKSPACE
            ))
        );
    }

    #[tokio::test]
    async fn turn_permissions_preserve_server_snapshot_without_local_override() {
        let mut config = config_with_workspace_profile().await;
        config
            .permissions
            .set_permission_profile(PermissionProfile::read_only())
            .expect("read-only profile should be allowed");

        assert_eq!(
            App::turn_permissions_override_from_config(&config, None, None,),
            TurnPermissionsOverride::Preserve
        );
    }

    #[tokio::test]
    async fn turn_permissions_send_legacy_sandbox_for_local_override() {
        let mut config = config_with_workspace_profile().await;
        let permission_profile = PermissionProfile::workspace_write();
        config
            .permissions
            .set_permission_profile(permission_profile.clone())
            .expect("workspace profile should be allowed");
        let effective_permission_profile = config.permissions.effective_permission_profile();

        assert_eq!(
            App::turn_permissions_override_from_config(&config, None, Some(&permission_profile),),
            TurnPermissionsOverride::LegacySandbox(effective_permission_profile)
        );
    }
}
