use super::*;
use crate::approval_events::RequestPermissionProfile;
use crate::approval_events::file_changes_from_detail;

impl ChatWidget {
    pub(super) fn on_operator_approval_request(
        &mut self,
        request: octopus_world_terminal_client::ApprovalRequest,
    ) {
        self.record_visible_turn_activity();
        let request2 = request.clone();
        self.defer_or_handle(
            |queue| queue.push_operator_approval(request),
            |chat| chat.handle_operator_approval_now(request2),
        );
    }

    pub(super) fn on_exec_approval_request(&mut self, _id: String, ev: ExecApprovalRequestEvent) {
        self.record_visible_turn_activity();
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_exec_approval(ev),
            |s| s.handle_exec_approval_now(ev2),
        );
    }

    pub(super) fn on_apply_patch_approval_request(
        &mut self,
        _id: String,
        ev: ApplyPatchApprovalRequestEvent,
    ) {
        self.record_visible_turn_activity();
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_apply_patch_approval(ev),
            |s| s.handle_apply_patch_approval_now(ev2),
        );
    }

    pub(super) fn on_guardian_assessment(&mut self, ev: GuardianAssessmentEvent) {
        let permission_request_summary = |subject: &str, reason: &Option<String>| {
            reason
                .as_deref()
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .map(|reason| format!("{subject}: {reason}"))
                .unwrap_or_else(|| subject.to_string())
        };
        let guardian_action_summary = |action: &GuardianAssessmentAction| match action {
            GuardianAssessmentAction::Command { command, .. } => Some(command.clone()),
            GuardianAssessmentAction::Execve { program, argv, .. } => {
                let command = if argv.is_empty() {
                    vec![program.clone()]
                } else {
                    argv.clone()
                };
                shlex::try_join(command.iter().map(String::as_str))
                    .ok()
                    .or_else(|| Some(command.join(" ")))
            }
            GuardianAssessmentAction::ApplyPatch { files, .. } => Some(if files.len() == 1 {
                format!("apply_patch touching {}", files[0].display())
            } else {
                format!("apply_patch touching {} files", files.len())
            }),
            GuardianAssessmentAction::NetworkAccess { target, .. } => {
                Some(format!("network access to {target}"))
            }
            GuardianAssessmentAction::McpToolCall {
                server,
                tool_name,
                connector_name,
                ..
            } => {
                let label = connector_name.as_deref().unwrap_or(server.as_str());
                Some(format!("tool call {tool_name} on {label}"))
            }
            GuardianAssessmentAction::RequestPermissions { reason, .. } => {
                Some(permission_request_summary("permission request", reason))
            }
        };
        let guardian_command = |action: &GuardianAssessmentAction| match action {
            GuardianAssessmentAction::Command { command, .. } => shlex::split(command)
                .filter(|command| !command.is_empty())
                .or_else(|| Some(vec![command.clone()])),
            GuardianAssessmentAction::Execve { program, argv, .. } => Some(if argv.is_empty() {
                vec![program.clone()]
            } else {
                argv.clone()
            })
            .filter(|command| !command.is_empty()),
            GuardianAssessmentAction::ApplyPatch { .. }
            | GuardianAssessmentAction::NetworkAccess { .. }
            | GuardianAssessmentAction::McpToolCall { .. }
            | GuardianAssessmentAction::RequestPermissions { .. } => None,
        };

        if ev.status == GuardianAssessmentStatus::InProgress
            && let Some(detail) = guardian_action_summary(&ev.action)
        {
            self.bottom_pane.ensure_status_indicator();
            self.bottom_pane.set_interrupt_hint_visible(true);
            self.status_state
                .pending_guardian_review_status
                .start_or_update(ev.id.clone(), detail);
            if let Some(status) = self
                .status_state
                .pending_guardian_review_status
                .status_indicator_state()
            {
                self.set_status(
                    status.header,
                    status.details,
                    StatusDetailsCapitalization::Preserve,
                    status.details_max_lines,
                );
            }
            self.request_redraw();
            return;
        }

        if self
            .status_state
            .pending_guardian_review_status
            .finish(&ev.id)
        {
            if let Some(status) = self
                .status_state
                .pending_guardian_review_status
                .status_indicator_state()
            {
                self.set_status(
                    status.header,
                    status.details,
                    StatusDetailsCapitalization::Preserve,
                    status.details_max_lines,
                );
            } else if self.status_state.current_status.is_guardian_review() {
                self.set_status_header(String::from("Working"));
            }
        } else if self.status_state.pending_guardian_review_status.is_empty()
            && self.status_state.current_status.is_guardian_review()
        {
            self.set_status_header(String::from("Working"));
        }

        if ev.status == GuardianAssessmentStatus::Approved {
            let cell = if let Some(command) = guardian_command(&ev.action) {
                history_cell::new_approval_decision_cell(
                    history_cell::ApprovalDecisionSubject::Command(command),
                    crate::history_cell::ReviewDecision::Approved,
                    history_cell::ApprovalDecisionActor::Guardian,
                )
            } else if let Some(summary) = guardian_action_summary(&ev.action) {
                history_cell::new_guardian_approved_action_request(summary)
            } else {
                let summary = serde_json::to_string(&ev.action)
                    .unwrap_or_else(|_| "<unrenderable guardian action>".to_string());
                history_cell::new_guardian_approved_action_request(summary)
            };

            self.add_boxed_history(cell);
            self.request_redraw();
            return;
        }

        if ev.status == GuardianAssessmentStatus::TimedOut {
            let cell = if let Some(command) = guardian_command(&ev.action) {
                history_cell::new_approval_decision_cell(
                    history_cell::ApprovalDecisionSubject::Command(command),
                    crate::history_cell::ReviewDecision::TimedOut,
                    history_cell::ApprovalDecisionActor::Guardian,
                )
            } else {
                match &ev.action {
                    GuardianAssessmentAction::ApplyPatch { files, .. } => {
                        let files = files
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>();
                        history_cell::new_guardian_timed_out_patch_request(files)
                    }
                    GuardianAssessmentAction::McpToolCall {
                        server, tool_name, ..
                    } => history_cell::new_guardian_timed_out_action_request(format!(
                        "codex could call MCP tool {server}.{tool_name}"
                    )),
                    GuardianAssessmentAction::NetworkAccess { target, .. } => {
                        history_cell::new_guardian_timed_out_action_request(format!(
                            "codex could access {target}"
                        ))
                    }
                    GuardianAssessmentAction::RequestPermissions { reason, .. } => {
                        history_cell::new_guardian_timed_out_action_request(
                            permission_request_summary("codex could request permissions", reason),
                        )
                    }
                    GuardianAssessmentAction::Command { .. } => unreachable!(),
                    GuardianAssessmentAction::Execve { .. } => unreachable!(),
                }
            };

            self.add_boxed_history(cell);
            self.request_redraw();
            return;
        }

        if ev.status != GuardianAssessmentStatus::Denied {
            return;
        }
        self.review.recent_auto_review_denials.push(ev.clone());
        let cell = if let Some(command) = guardian_command(&ev.action) {
            history_cell::new_approval_decision_cell(
                history_cell::ApprovalDecisionSubject::Command(command),
                crate::history_cell::ReviewDecision::Denied,
                history_cell::ApprovalDecisionActor::Guardian,
            )
        } else {
            match &ev.action {
                GuardianAssessmentAction::ApplyPatch { files, .. } => {
                    let files = files
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>();
                    history_cell::new_guardian_denied_patch_request(files)
                }
                GuardianAssessmentAction::McpToolCall {
                    server, tool_name, ..
                } => history_cell::new_guardian_denied_action_request(format!(
                    "codex to call MCP tool {server}.{tool_name}"
                )),
                GuardianAssessmentAction::NetworkAccess { target, .. } => {
                    history_cell::new_guardian_denied_action_request(format!(
                        "codex to access {target}"
                    ))
                }
                GuardianAssessmentAction::RequestPermissions { reason, .. } => {
                    history_cell::new_guardian_denied_action_request(permission_request_summary(
                        "codex to request permissions",
                        reason,
                    ))
                }
                GuardianAssessmentAction::Command { .. } => unreachable!(),
                GuardianAssessmentAction::Execve { .. } => unreachable!(),
            }
        };

        self.add_boxed_history(cell);
        self.request_redraw();
    }

    pub(super) fn on_elicitation_request(
        &mut self,
        request: octopus_world_terminal_client::ElicitationRequest,
    ) {
        self.record_visible_turn_activity();
        let request2 = request.clone();
        self.defer_or_handle(
            |q| q.push_elicitation(request),
            |s| s.handle_elicitation_request_now(request2),
        );
    }

    pub(super) fn on_request_user_input(
        &mut self,
        request: octopus_world_terminal_client::UserInputRequest,
    ) {
        self.record_visible_turn_activity();
        let request2 = request.clone();
        self.defer_or_handle(
            |q| q.push_user_input(request),
            |s| s.handle_request_user_input_now(request2),
        );
    }

    pub(super) fn on_request_permissions(&mut self, ev: RequestPermissionsEvent) {
        self.record_visible_turn_activity();
        let ev2 = ev.clone();
        self.defer_or_handle(
            |q| q.push_request_permissions(ev),
            |s| s.handle_request_permissions_now(ev2),
        );
    }

    pub(crate) fn handle_exec_approval_now(&mut self, ev: ExecApprovalRequestEvent) {
        self.flush_answer_stream_with_separator();
        let command = shlex::try_join(ev.command.iter().map(String::as_str))
            .unwrap_or_else(|_| ev.command.join(" "));
        self.notify(Notification::ExecApprovalRequested { command });

        let options = ev.effective_options();
        let request = ApprovalRequest::Exec {
            thread_id: self.thread_id.clone().unwrap_or_default(),
            thread_label: None,
            id: ev.request_id,
            environment_id: ev.environment_id,
            command: ev.command,
            reason: ev.reason,
            options,
            detail: ev.detail,
        };
        self.bottom_pane.push_approval_request(request);
        self.set_ambient_pet_notification(crate::pets::PetNotificationKind::Waiting, None);
        self.request_redraw();
    }

    pub(crate) fn handle_operator_approval_now(
        &mut self,
        request: octopus_world_terminal_client::ApprovalRequest,
    ) {
        self.flush_answer_stream_with_separator();
        let detail = octopus_world_terminal_client::ApprovalDetail::from_value(&request.detail);
        let options = exact_approval_options(&request);
        let approval = match request.kind.as_str() {
            "exec" => {
                let command_text = if detail.command.trim().is_empty() {
                    request.title.clone()
                } else {
                    detail.command.clone()
                };
                let command = shlex::split(&command_text)
                    .filter(|command| !command.is_empty())
                    .unwrap_or_else(|| vec![command_text]);
                ApprovalRequest::Exec {
                    thread_id: self.thread_id.clone().unwrap_or_default(),
                    thread_label: None,
                    id: request.request_id,
                    environment_id: detail.environment_id.clone(),
                    command,
                    reason: detail.reason.clone(),
                    options,
                    detail,
                }
            }
            "patch" => {
                let cwd = if detail.cwd.trim().is_empty() {
                    self.config.cwd.clone()
                } else {
                    match AbsolutePathBuf::try_from(detail.cwd.clone()) {
                        Ok(cwd) => cwd,
                        Err(error) => {
                            self.add_error_message(format!(
                                "Approval request {} has an invalid cwd: {error}",
                                request.request_id
                            ));
                            return;
                        }
                    }
                };
                ApprovalRequest::ApplyPatch {
                    thread_id: self.thread_id.clone().unwrap_or_default(),
                    thread_label: None,
                    id: request.request_id,
                    reason: detail.reason.clone(),
                    changes: file_changes_from_detail(&request.detail),
                    cwd,
                    options,
                }
            }
            "permission" => {
                let Some(permissions) = request.detail.get("permissions") else {
                    self.add_error_message(format!(
                        "Approval request {} has no permissions payload",
                        request.request_id
                    ));
                    return;
                };
                let permissions =
                    match serde_json::from_value::<RequestPermissionProfile>(permissions.clone()) {
                        Ok(permissions) => permissions,
                        Err(error) => {
                            self.add_error_message(format!(
                                "Approval request {} has invalid permissions: {error}",
                                request.request_id
                            ));
                            return;
                        }
                    };
                ApprovalRequest::Permissions {
                    thread_id: self.thread_id.clone().unwrap_or_default(),
                    thread_label: None,
                    call_id: request.request_id,
                    environment_id: detail.environment_id,
                    reason: detail.reason,
                    permissions,
                }
            }
            other => {
                self.add_error_message(format!(
                    "Approval request {} has unsupported kind '{other}'",
                    request.request_id
                ));
                return;
            }
        };
        self.bottom_pane.push_approval_request(approval);
        self.set_ambient_pet_notification(crate::pets::PetNotificationKind::Waiting, None);
        self.request_redraw();
    }

    pub(crate) fn handle_apply_patch_approval_now(&mut self, ev: ApplyPatchApprovalRequestEvent) {
        self.flush_answer_stream_with_separator();

        let request = ApprovalRequest::ApplyPatch {
            thread_id: self.thread_id.clone().unwrap_or_default(),
            thread_label: None,
            id: ev.request_id,
            reason: ev.reason,
            changes: ev.changes.clone(),
            cwd: self.config.cwd.clone(),
            options: default_exact_approval_options(false),
        };
        self.bottom_pane.push_approval_request(request);
        self.set_ambient_pet_notification(crate::pets::PetNotificationKind::Waiting, None);
        self.request_redraw();
        self.notify(Notification::EditApprovalRequested {
            cwd: self.config.cwd.to_path_buf(),
            changes: ev.changes.keys().cloned().collect(),
        });
    }

    pub(crate) fn handle_elicitation_request_now(
        &mut self,
        request: octopus_world_terminal_client::ElicitationRequest,
    ) {
        self.flush_answer_stream_with_separator();

        self.notify(Notification::ElicitationRequested {
            server_name: request.request_id.clone(),
        });

        let Some(form) = McpServerElicitationFormRequest::from_elicitation_request(request) else {
            self.add_error_message("Elicitation request carries an unsupported form schema".into());
            return;
        };
        self.bottom_pane.push_mcp_server_elicitation_request(form);
        self.set_ambient_pet_notification(crate::pets::PetNotificationKind::Waiting, None);
        self.request_redraw();
    }

    pub(crate) fn push_approval_request(&mut self, request: ApprovalRequest) {
        self.bottom_pane.push_approval_request(request);
        self.set_ambient_pet_notification(crate::pets::PetNotificationKind::Waiting, None);
        self.request_redraw();
    }

    pub(crate) fn push_mcp_server_elicitation_request(
        &mut self,
        request: McpServerElicitationFormRequest,
    ) {
        self.bottom_pane
            .push_mcp_server_elicitation_request(request);
        self.set_ambient_pet_notification(crate::pets::PetNotificationKind::Waiting, None);
        self.request_redraw();
    }

    pub(crate) fn handle_request_user_input_now(
        &mut self,
        request: octopus_world_terminal_client::UserInputRequest,
    ) {
        self.flush_answer_stream_with_separator();
        let question_count = request.questions.len();
        let summary = Notification::user_input_request_summary(&request.questions);
        let title = match (question_count, summary.as_deref()) {
            (1, Some(summary)) => summary.to_string(),
            (1, None) => "Question requested".to_string(),
            (count, _) => format!("{count} questions requested"),
        };
        self.notify(Notification::PlanModePrompt { title });
        for question in &request.questions {
            self.add_to_history(history_cell::new_info_event(
                format!("Input requested: {}", question.question),
                None,
            ));
        }
        self.bottom_pane.push_user_input_request(request);
        self.set_ambient_pet_notification(crate::pets::PetNotificationKind::Waiting, None);
        self.request_redraw();
    }

    pub(crate) fn handle_request_permissions_now(&mut self, ev: RequestPermissionsEvent) {
        self.flush_answer_stream_with_separator();
        let request = ApprovalRequest::Permissions {
            thread_id: self.thread_id.clone().unwrap_or_default(),
            thread_label: None,
            call_id: ev.call_id,
            environment_id: ev.environment_id,
            reason: ev.reason,
            permissions: ev.permissions,
        };
        self.bottom_pane.push_approval_request(request);
        self.set_ambient_pet_notification(crate::pets::PetNotificationKind::Waiting, None);
        self.request_redraw();
    }

    pub(super) fn on_operator_request_resolved(&mut self, request_id: &str) {
        let removed_deferred = self.interrupts.remove_operator_request(request_id);
        let removed_visible = self.bottom_pane.dismiss_operator_request(request_id);
        if removed_deferred || removed_visible {
            self.request_redraw();
        }
    }
}

fn exact_approval_options(
    request: &octopus_world_terminal_client::ApprovalRequest,
) -> Vec<octopus_world_terminal_client::ApprovalOptionItem> {
    let options = octopus_world_terminal_client::approval_options_from_detail(&request.detail);
    if options.is_empty() {
        default_exact_approval_options(matches!(
            request.method.as_str(),
            "execCommandApproval" | "applyPatchApproval"
        ))
    } else {
        options
    }
}

fn default_exact_approval_options(
    legacy: bool,
) -> Vec<octopus_world_terminal_client::ApprovalOptionItem> {
    let values = if legacy {
        ["approved", "approved_for_session", "abort"]
    } else {
        ["accept", "acceptForSession", "cancel"]
    };
    values
        .into_iter()
        .map(
            |decision| octopus_world_terminal_client::ApprovalOptionItem {
                kind: match decision {
                    "approved" | "accept" => "accept",
                    "approved_for_session" | "acceptForSession" => "accept_for_session",
                    "abort" | "cancel" => "cancel",
                    other => other,
                }
                .to_string(),
                label: None,
                command_prefix: None,
                decision: serde_json::json!(decision),
            },
        )
        .collect()
}
