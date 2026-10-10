use super::resize_reflow::trailing_run_start;
use super::*;
use crate::config_update::format_config_error;
#[cfg(any(target_os = "windows", test))]
use crate::session_posture::WindowsSandboxMode;

const SHUTDOWN_FIRST_EXIT_TIMEOUT: Duration = Duration::from_secs(2);

impl App {
    pub(super) async fn handle_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: AppEvent,
    ) -> Result<AppRunControl> {
        match event {
            AppEvent::NewSession => {
                self.start_fresh_session_with_summary_hint(tui, app_server, None)
                    .await;
            }
            AppEvent::OpenRegistriesTab { focus_kind } => {
                let _ = tui.enter_alt_screen();
                let reads = app_server.registry_tab_reads();
                self.overlay = Some(Overlay::new_registry_focused(reads, focus_kind.as_deref()));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenDashboardTab => {
                let _ = tui.enter_alt_screen();
                let movements = app_server.dashboard_movements_snapshot();
                let spine_wired = app_server.drives_embedded_spine();
                self.overlay = Some(Overlay::new_dashboard(movements, spine_wired));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenMetricsTab => {
                let _ = tui.enter_alt_screen();
                let movements = app_server.dashboard_movements_snapshot();
                let spine_wired = app_server.drives_embedded_spine();
                let tokens = self.chat_widget.token_usage();
                self.overlay = Some(Overlay::new_metrics(movements, tokens, spine_wired));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::ClearUi => {
                self.clear_terminal_ui(tui, false)?;
                self.reset_app_ui_state_after_clear();

                self.start_fresh_session_with_summary_hint(tui, app_server, None)
                    .await;
            }
            AppEvent::RawOutputModeChanged { enabled } => {
                self.apply_raw_output_mode(tui, enabled, false);
            }
            AppEvent::ClearUiAndSubmitUserMessage { text } => {
                self.clear_terminal_ui(tui, false)?;
                self.reset_app_ui_state_after_clear();

                self.start_fresh_session_with_summary_hint(
                    tui,
                    app_server,
                    crate::chatwidget::create_initial_user_message(
                        Some(text),
                        Vec::new(),
                        Vec::new(),
                    ),
                )
                .await;
            }
            AppEvent::OpenResumePicker => {
                match crate::resume_picker::run_resume_picker_from_existing_session(
                    tui,
                    &self.config,
                )
                .await?
                {
                    SessionSelection::Resume(target_session) => {
                        match self
                            .resume_target_session(tui, app_server, target_session)
                            .await?
                        {
                            AppRunControl::Continue => {}
                            AppRunControl::Exit(reason) => {
                                return Ok(AppRunControl::Exit(reason));
                            }
                            detached @ AppRunControl::Detached { .. } => {
                                return Ok(detached);
                            }
                        }
                    }
                    SessionSelection::Exit | SessionSelection::StartFresh => {
                        self.refresh_in_memory_config_from_disk_best_effort(
                            "closing the session picker",
                        )
                        .await;
                    }
                    SessionSelection::Fork(_) => {}
                }

                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenTaskManager => {
                match crate::resume_picker::run_task_manager(tui, &self.config).await? {
                    SessionSelection::Resume(target_session) => {
                        let selected_run = target_session.run.clone();
                        match self
                            .resume_target_session(tui, app_server, target_session)
                            .await?
                        {
                            AppRunControl::Continue => {
                                if app_server.active_run_id().as_ref() == Some(&selected_run) {
                                    self.side_parent_run = None;
                                    self.side_parent_was_running = false;
                                    self.chat_widget.set_side_conversation_active(false);
                                    self.chat_widget.set_side_conversation_context_label(None);
                                }
                            }
                            AppRunControl::Exit(reason) => {
                                return Ok(AppRunControl::Exit(reason));
                            }
                            detached @ AppRunControl::Detached { .. } => {
                                return Ok(detached);
                            }
                        }
                    }
                    SessionSelection::StartFresh => match app_server.author_new_task().await {
                        Ok(run) => {
                            self.reset_for_thread_switch(tui)?;
                            let init = self.chatwidget_init_for_forked_or_resumed_thread(
                                tui,
                                self.config.clone(),
                                None,
                            );
                            self.replace_chat_widget(ChatWidget::new_with_app_event(init));
                            self.sync_chat_to_active_topology(app_server)?;
                            self.side_parent_run = None;
                            self.side_parent_was_running = false;
                            self.chat_widget
                                .add_info_message(format!("Created task {}.", run.as_str()), None);
                        }
                        Err(error) => self
                            .chat_widget
                            .add_error_message(format!("Failed to create task: {error}")),
                    },
                    SessionSelection::Exit => {}
                    SessionSelection::Fork(_) => {}
                }
                tui.frame_requester().schedule_frame();
            }
            AppEvent::ShowWorkingDirectory => match app_server.working_roots() {
                Ok(roots) => {
                    let lines = roots
                        .into_iter()
                        .map(|(slot, root)| format!("{slot}: {}", root.display()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.chat_widget
                        .add_info_message("Working roots for this task".to_string(), Some(lines));
                }
                Err(error) => self
                    .chat_widget
                    .add_error_message(format!("Failed to project working roots: {error}")),
            },
            AppEvent::ChangeWorkingDirectory { path } => {
                if self.app_server_target.uses_remote_workspace() {
                    self.chat_widget.add_error_message(
                        "'/cd' is unavailable for a remote workspace.".to_string(),
                    );
                    return Ok(AppRunControl::Continue);
                }
                if self.chat_widget.side_conversation_active() {
                    self.chat_widget.add_error_message(
                        "'/cd' is unavailable inside a side question. Return to the main task first."
                            .to_string(),
                    );
                    return Ok(AppRunControl::Continue);
                }
                if self.chat_widget.is_task_running() {
                    self.chat_widget.add_error_message(
                        "'/cd' can only create a continuation after the current task is ready."
                            .to_string(),
                    );
                    return Ok(AppRunControl::Continue);
                }
                let Some(active_run) = app_server.active_run_id() else {
                    self.chat_widget.add_error_message(
                        "'/cd' cannot verify an active task to continue.".to_string(),
                    );
                    return Ok(AppRunControl::Continue);
                };
                match app_server.task_projection() {
                    Ok(tasks) => match tasks.iter().find(|task| task.run == active_run) {
                        Some(task)
                            if matches!(
                                task.status,
                                octopus_world_terminal_client::TaskStatus::NeedsInput
                                    | octopus_world_terminal_client::TaskStatus::Working
                            ) =>
                        {
                            self.chat_widget.add_error_message(
                                "'/cd' is blocked while the current task is working or needs operator input."
                                    .to_string(),
                            );
                            return Ok(AppRunControl::Continue);
                        }
                        Some(_) => {}
                        None => {
                            self.chat_widget.add_error_message(format!(
                                "'/cd' could not find the active task {} in the terminal projection.",
                                active_run.as_str()
                            ));
                            return Ok(AppRunControl::Continue);
                        }
                    },
                    Err(error) => {
                        self.chat_widget.add_error_message(format!(
                            "'/cd' could not verify task readiness: {error}"
                        ));
                        return Ok(AppRunControl::Continue);
                    }
                }

                let requested = if path.trim().is_empty() || path.trim() == "~" {
                    match dirs::home_dir() {
                        Some(home) => home,
                        None => {
                            self.chat_widget.add_error_message(
                                "Cannot resolve the home directory for '/cd'.".to_string(),
                            );
                            return Ok(AppRunControl::Continue);
                        }
                    }
                } else if let Some(rest) = path.trim().strip_prefix("~/") {
                    match dirs::home_dir() {
                        Some(home) => home.join(rest),
                        None => {
                            self.chat_widget.add_error_message(
                                "Cannot resolve the home directory for '/cd'.".to_string(),
                            );
                            return Ok(AppRunControl::Continue);
                        }
                    }
                } else {
                    let candidate = PathBuf::from(path.trim());
                    if candidate.is_absolute() {
                        candidate
                    } else {
                        self.config.cwd.join(candidate).to_path_buf()
                    }
                };
                let target = match std::fs::canonicalize(&requested) {
                    Ok(target) if target.is_dir() => target,
                    Ok(_) => {
                        self.chat_widget.add_error_message(format!(
                            "Working root '{}' is not a directory.",
                            requested.display()
                        ));
                        return Ok(AppRunControl::Continue);
                    }
                    Err(error) => {
                        self.chat_widget.add_error_message(format!(
                            "Cannot open working root '{}': {error}",
                            requested.display()
                        ));
                        return Ok(AppRunControl::Continue);
                    }
                };
                let mut granted_roots = vec![self.config.cwd.to_path_buf()];
                granted_roots.extend(
                    self.config
                        .effective_workspace_roots()
                        .into_iter()
                        .map(|root| root.into_path_buf()),
                );
                let granted = granted_roots.into_iter().any(|root| {
                    std::fs::canonicalize(root)
                        .map(|root| root == target)
                        .unwrap_or(false)
                });
                if !granted {
                    self.chat_widget.add_error_message(format!(
                        "Working root '{}' is outside the exact filesystem grants for this task.",
                        target.display()
                    ));
                    return Ok(AppRunControl::Continue);
                }
                if !target.join(".git").exists() {
                    self.chat_widget.add_error_message(format!(
                        "Working root '{}' is not an exact Git repository or worktree root supported by the current coder room.",
                        target.display()
                    ));
                    return Ok(AppRunControl::Continue);
                }

                let source = app_server.active_run_id();
                match app_server.author_root_continuation(target.clone()).await {
                    Ok(run) => {
                        let absolute = match AbsolutePathBuf::from_absolute_path(&target) {
                            Ok(path) => path,
                            Err(error) => {
                                self.chat_widget.add_error_message(format!(
                                    "Working root '{}' is not an absolute path: {error}",
                                    target.display()
                                ));
                                return Ok(AppRunControl::Continue);
                            }
                        };
                        self.reset_for_thread_switch(tui)?;
                        self.config.cwd = absolute;
                        self.file_search.update_search_dir(target.clone());
                        let init = self.chatwidget_init_for_forked_or_resumed_thread(
                            tui,
                            self.config.clone(),
                            None,
                        );
                        self.replace_chat_widget(ChatWidget::new_with_app_event(init));
                        self.sync_chat_to_active_topology(app_server)?;
                        self.chat_widget.add_info_message(
                            format!("Continued as task {}.", run.as_str()),
                            Some(format!(
                                "Working root: {}{}",
                                target.display(),
                                source
                                    .map(|source| format!("\nPrevious run: {}", source.as_str()))
                                    .unwrap_or_default()
                            )),
                        );
                    }
                    Err(error) => self
                        .chat_widget
                        .add_error_message(format!("Failed to change working root: {error}")),
                }
                tui.frame_requester().schedule_frame();
            }
            AppEvent::ResumeSessionByIdOrName(id_or_name) => {
                match crate::lookup_session_target_by_run_id(&self.config, &id_or_name)? {
                    Some(target_session) => {
                        return self
                            .resume_target_session(tui, app_server, target_session)
                            .await;
                    }
                    None => {
                        self.chat_widget.add_error_message(format!(
                            "No saved session found matching '{id_or_name}'."
                        ));
                    }
                }
            }
            AppEvent::ArchiveCurrentThread => {
                return Ok(self.archive_current_thread(app_server).await);
            }
            AppEvent::DeleteCurrentThread => {
                return Ok(self.delete_current_thread(app_server).await);
            }
            AppEvent::ForkCurrentSession => {
                self.session_telemetry.counter(
                    "codex.thread.fork",
                    1,
                    &[("source", "slash_command")],
                );
                self.chat_widget
                    .add_plain_history_lines(vec!["/fork".magenta().into()]);
                self.chat_widget.add_error_message(
                    "'/fork' needs an interface-authored run fork; the retired app-server \
                     thread/fork lane is removed. Use /resume to re-open a run's transcript."
                        .to_string(),
                );
                tui.frame_requester().schedule_frame();
            }
            AppEvent::BeginInitialHistoryReplayBuffer => {
                self.begin_initial_history_replay_buffer();
            }
            AppEvent::BeginThreadSwitchHistoryReplayBuffer => {
                self.begin_thread_switch_history_replay_buffer();
            }
            AppEvent::InsertHistoryCell(cell) => {
                self.insert_history_cell(tui, cell);
            }
            AppEvent::EndInitialHistoryReplayBuffer => {
                self.finish_initial_history_replay_buffer(tui);
            }
            AppEvent::ConsolidateAgentMessage {
                source,
                cwd,
                scrollback_reflow,
                deferred_history_cell,
            } => {
                self.handle_consolidate_agent_message(
                    tui,
                    source,
                    cwd,
                    scrollback_reflow,
                    deferred_history_cell,
                )?;
                self.chat_widget.note_stream_consolidation_completed();
                self.insert_pending_usage_output_after_stream_shutdown(tui);
            }
            AppEvent::ConsolidateProposedPlan(source) => {
                let end = self.transcript_cells.len();
                let start = trailing_run_start::<history_cell::ProposedPlanStreamCell>(
                    &self.transcript_cells,
                );
                let consolidated: Arc<dyn HistoryCell> =
                    Arc::new(history_cell::new_proposed_plan(source, &self.config.cwd));

                if start < end {
                    self.transcript_cells
                        .splice(start..end, std::iter::once(consolidated.clone()));

                    if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                        t.consolidate_cells(start..end, consolidated.clone());
                        tui.frame_requester().schedule_frame();
                    }

                    self.finish_required_stream_reflow(tui)?;
                } else {
                    self.transcript_cells.push(consolidated.clone());
                    if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                        t.insert_cell(consolidated.clone());
                        tui.frame_requester().schedule_frame();
                    }
                    self.insert_history_cell_lines(
                        tui,
                        consolidated.as_ref(),
                        self.chat_widget
                            .history_wrap_width(tui.terminal.last_known_screen_size.width),
                    );

                    self.maybe_finish_stream_reflow(tui)?;
                }
                self.chat_widget.note_stream_consolidation_completed();
                self.insert_pending_usage_output_after_stream_shutdown(tui);
            }
            AppEvent::ApplyThreadRollback { num_turns } => {
                if self.apply_non_pending_thread_rollback(num_turns) {
                    tui.frame_requester().schedule_frame();
                }
            }
            AppEvent::StartCommitAnimation => {
                if self
                    .commit_anim_running
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let tx = self.app_event_tx.clone();
                    let running = self.commit_anim_running.clone();
                    thread::spawn(move || {
                        while running.load(Ordering::Relaxed) {
                            thread::sleep(COMMIT_ANIMATION_TICK);
                            tx.send(AppEvent::CommitTick);
                        }
                    });
                }
            }
            AppEvent::StopCommitAnimation => {
                self.commit_anim_running.store(false, Ordering::Release);
            }
            AppEvent::CommitTick => {
                self.chat_widget.on_commit_tick();
            }
            AppEvent::OpenTranscriptExportFilePrompt => {
                self.chat_widget.show_transcript_export_file_prompt();
            }
            AppEvent::ExportTranscript { destination } => {
                if let Err(error) = self.export_transcript(app_server, destination).await {
                    self.chat_widget
                        .add_error_message(format!("Transcript export failed: {error}"));
                }
            }
            AppEvent::OpenFeedbackPrompt { category } => {
                self.chat_widget.show_feedback_prompt(category);
            }
            AppEvent::OpenFeedbackConsent { category, note } => {
                self.chat_widget.show_feedback_consent_popup(category, note);
            }
            AppEvent::SubmitFeedback {
                category,
                note,
                consent,
            } => {
                self.start_feedback_bundle(app_server, category, note, consent);
            }
            AppEvent::FeedbackBundleFinished { result } => match result {
                Ok(receipt) => {
                    self.chat_widget.add_info_message(
                        format!(
                            "Saved local support bundle to {}. Receipt: {}. Attachments: {}. Nothing was uploaded.",
                            receipt.bundle_path.display(),
                            receipt.receipt_id,
                            receipt.attachments.len(),
                        ),
                        None,
                    );
                }
                Err(error) => self
                    .chat_widget
                    .add_error_message(format!("Local feedback bundle failed: {error}")),
            },
            AppEvent::ShowUpdateDiagnostics => {
                self.show_update_diagnostics();
            }
            AppEvent::Exit(mode) => {
                if mode == ExitMode::ShutdownFirst
                    && self.chat_widget.is_task_running()
                    && !matches!(self.app_server_target, AppServerTarget::Embedded)
                {
                    let Some(run) = app_server.active_run_id() else {
                        self.chat_widget.add_error_message(
                            "Cannot detach: the daemon did not expose an active run identity."
                                .to_string(),
                        );
                        return Ok(AppRunControl::Continue);
                    };
                    self.chat_widget.show_daemon_detach_popup(run.as_str());
                    return Ok(AppRunControl::Continue);
                }
                if mode == ExitMode::ShutdownFirst {
                    self.show_shutdown_feedback(tui)?;
                }
                return Ok(self.handle_exit_mode(mode).await);
            }
            AppEvent::Logout => match app_server.logout_account().await {
                Ok(()) => {
                    self.show_shutdown_feedback(tui)?;
                    return Ok(self.handle_exit_mode(ExitMode::ShutdownFirst).await);
                }
                Err(err) => {
                    tracing::error!("failed to logout: {err}");
                    self.chat_widget
                        .add_error_message(format!("Logout failed: {err}"));
                }
            },
            AppEvent::FatalExitRequest(message) => {
                return Ok(AppRunControl::Exit(ExitReason::Fatal(message)));
            }
            AppEvent::CodexOp(op) => {
                self.chat_widget.prepare_local_op_submission(&op);
                self.submit_op(app_server, op).await?;
            }
            AppEvent::ShellCommandCompleted { item } => {
                self.chat_widget
                    .handle_command_execution_completed_now(item);
                tui.frame_requester().schedule_frame();
            }
            AppEvent::RestoreCancelledTurn(prompt) => {
                self.apply_cancelled_turn_edit(prompt);
            }
            AppEvent::SyncThreadGitBranch { thread_id, branch } => {
                tracing::debug!(%thread_id, branch, "thread git branch noted without mutating run metadata");
            }
            AppEvent::LookupMessageHistoryEntry { offset, log_id } => {
                self.lookup_prompt_history_entry(offset, log_id);
            }
            AppEvent::ApproveRecentAutoReviewDenial { thread_id, id } => {
                self.chat_widget
                    .approve_recent_auto_review_denial(thread_id, id);
            }
            AppEvent::PromptHistoryEntryResponse { event } => {
                self.chat_widget.handle_history_entry_response(event);
            }
            AppEvent::DiffResult(text) => {
                self.chat_widget.on_diff_complete();
                let _ = tui.enter_alt_screen();
                let pager_lines: Vec<ratatui::text::Line<'static>> = if text.trim().is_empty() {
                    vec!["No changes detected.".italic().into()]
                } else {
                    text.lines().map(ansi_escape_line).collect()
                };
                self.overlay = Some(Overlay::new_static_with_lines(
                    pager_lines,
                    "D I F F".to_string(),
                    self.keymap.pager.clone(),
                ));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenUrlInBrowser { url } => {
                self.open_url_in_browser(url);
            }
            AppEvent::OpenDesktopThread { thread_id } => {
                self.open_desktop_thread(thread_id);
            }
            AppEvent::PetSelected { pet_id } => {
                self.handle_pet_selected(tui, pet_id);
            }
            AppEvent::PetDisabled => {
                self.handle_pet_disabled(tui).await;
            }
            AppEvent::PetPreviewRequested { pet_id } => {
                self.chat_widget.start_pet_picker_preview(pet_id);
            }
            AppEvent::PetPreviewLoaded { request_id, result } => {
                self.handle_pet_preview_loaded(tui, request_id, result);
            }
            AppEvent::PetSelectionLoaded {
                request_id,
                pet_id,
                result,
            } => {
                return self
                    .handle_pet_selection_loaded(tui, request_id, pet_id, result)
                    .await;
            }
            AppEvent::ConfiguredPetLoaded { pet_id, result } => {
                self.handle_configured_pet_loaded(tui, pet_id, result);
            }
            AppEvent::FetchHooksList { cwd } => {
                self.fetch_hooks_list(app_server, cwd);
            }
            AppEvent::HooksLoaded { cwd, result } => {
                self.chat_widget.on_hooks_loaded(cwd, result);
            }
            AppEvent::StartFileSearch(query) => {
                if query.is_empty() {
                    self.refresh_task_mentions(app_server);
                }
                self.file_search.on_user_query(query);
            }
            AppEvent::FileSearchResult { query, matches } => {
                self.chat_widget.apply_file_search_result(query, matches);
            }
            AppEvent::RefreshRateLimits { origin } => {
                self.refresh_rate_limits(app_server, origin);
            }
            AppEvent::RefreshTokenActivity { request_id } => {
                self.refresh_token_activity(app_server, request_id);
            }
            AppEvent::OpenThreadGoalMenu { thread_id } => {
                self.open_thread_goal_menu(app_server, thread_id).await;
            }
            AppEvent::OpenThreadGoalEditor { thread_id } => {
                self.open_thread_goal_editor(app_server, thread_id).await;
            }
            AppEvent::SetThreadGoalDraft {
                thread_id,
                draft,
                mode,
            } => {
                self.set_thread_goal_draft(app_server, thread_id, draft, mode)
                    .await;
            }
            AppEvent::SetThreadGoalStatus { thread_id, status } => {
                self.set_thread_goal_status(app_server, thread_id, status)
                    .await;
            }
            AppEvent::ClearThreadGoal { thread_id } => {
                self.clear_thread_goal(app_server, thread_id).await;
            }
            AppEvent::SendAddCreditsNudgeEmail { credit_type } => {
                if self
                    .chat_widget
                    .start_add_credits_nudge_email_request(credit_type.clone())
                {
                    self.send_add_credits_nudge_email(app_server, credit_type);
                }
            }
            AppEvent::AddCreditsNudgeEmailFinished { result } => {
                self.chat_widget
                    .finish_add_credits_nudge_email_request(result);
            }
            AppEvent::RateLimitsLoaded { origin, result } => match result {
                Ok(report) => {
                    let reset_credits = report.reset_credits.clone();
                    for snapshot in report.snapshots {
                        self.chat_widget.on_rate_limit_snapshot(Some(snapshot));
                    }
                    match origin {
                        RateLimitRefreshOrigin::StartupPrefetch {
                            reset_hint_request_id,
                        } => {
                            if self.chat_widget.finish_rate_limit_reset_hint_refresh(
                                reset_hint_request_id,
                                reset_credits.ok_or_else(|| {
                                    "account.rate_limits payload did not include reset credits"
                                        .to_string()
                                }),
                            ) {
                                self.insert_pending_usage_output_if_ready(tui);
                            }
                            tui.frame_requester().schedule_frame();
                        }
                        RateLimitRefreshOrigin::ResetConsume { request_id } => {
                            self.chat_widget.finish_post_consume_reset_credits_refresh(
                                request_id,
                                reset_credits.ok_or_else(|| {
                                    "account.rate_limits payload did not include reset credits"
                                        .to_string()
                                }),
                            );
                            tui.frame_requester().schedule_frame();
                        }
                        RateLimitRefreshOrigin::StatusCommand { request_id } => {
                            self.chat_widget
                                .finish_status_rate_limit_refresh(request_id);
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!("account rate-limits read failed during TUI refresh: {err}");
                    match origin {
                        RateLimitRefreshOrigin::StartupPrefetch {
                            reset_hint_request_id,
                        } => {
                            self.chat_widget.finish_rate_limit_reset_hint_refresh(
                                reset_hint_request_id,
                                Err(err),
                            );
                        }
                        RateLimitRefreshOrigin::ResetConsume { request_id } => {
                            self.chat_widget
                                .finish_post_consume_reset_credits_refresh(request_id, Err(err));
                        }
                        RateLimitRefreshOrigin::StatusCommand { request_id } => {
                            self.chat_widget
                                .finish_status_rate_limit_refresh(request_id);
                        }
                    }
                }
            },
            AppEvent::OpenTokenActivity => {
                self.chat_widget
                    .add_token_activity_output(crate::chatwidget::TokenActivityView::Daily);
            }
            AppEvent::OpenRateLimitResetCredits => {
                let request_id = self.chat_widget.show_rate_limit_reset_loading_popup();
                self.refresh_rate_limit_reset_credits(app_server, request_id);
            }
            AppEvent::RateLimitResetCreditsLoaded { request_id, result } => {
                if let Err(err) = &result {
                    tracing::warn!(
                        "account/rateLimits/read failed during reset-credit refresh: {err}"
                    );
                }
                self.chat_widget
                    .finish_rate_limit_reset_credits_refresh(request_id, result);
            }
            AppEvent::ConsumeRateLimitResetCredit { idempotency_key } => {
                let request_id = self.chat_widget.show_rate_limit_reset_consuming_popup();
                self.consume_rate_limit_reset_credit(app_server, request_id, idempotency_key);
            }
            AppEvent::RateLimitResetCreditConsumed {
                request_id,
                idempotency_key,
                result,
            } => {
                if let Err(err) = &result {
                    tracing::warn!(
                        "account/rateLimitResetCredit/consume failed during TUI request: {err}"
                    );
                }
                if self.chat_widget.finish_rate_limit_reset_consume(
                    request_id,
                    idempotency_key,
                    result,
                ) {
                    self.refresh_rate_limits(
                        app_server,
                        RateLimitRefreshOrigin::ResetConsume { request_id },
                    );
                }
            }
            AppEvent::TokenActivityLoaded { request_id, result } => {
                if let Err(err) = &result {
                    tracing::warn!("account/usage/read failed during TUI refresh: {err}");
                }
                if self
                    .chat_widget
                    .finish_token_activity_refresh(request_id, result)
                {
                    self.insert_pending_usage_output_if_ready(tui);
                }
            }
            AppEvent::CommitPendingUsageOutput => {
                self.insert_pending_usage_output_if_ready(tui);
            }
            AppEvent::CommitPendingUsageOutputAfterStreamShutdown => {
                self.insert_pending_usage_output_after_stream_shutdown(tui);
            }
            AppEvent::UpdateReasoningEffort(effort) => {
                self.on_update_reasoning_effort(effort.clone());
            }
            AppEvent::UpdateModel(model) => {
                self.chat_widget.set_model(&model);
            }
            AppEvent::UpdatePersonality(personality) => {
                self.on_update_personality(personality);
            }
            AppEvent::SettingsSelectionClosed => {
                self.app_event_tx.send(AppEvent::SettingsSelectionSettled);
            }
            AppEvent::SettingsSelectionSettled => {
                if self.chat_widget.no_modal_or_popup_active() {
                    self.chat_widget.set_queue_autosend_suppressed(false);
                    self.chat_widget.maybe_send_next_queued_input();
                }
            }
            AppEvent::OpenReasoningPopup { model } => {
                self.chat_widget.open_reasoning_popup(model);
            }
            AppEvent::OpenPlanReasoningScopePrompt { model, effort } => {
                self.chat_widget
                    .open_plan_reasoning_scope_prompt(model, effort);
            }
            AppEvent::OpenAllModelsPopup { models } => {
                self.chat_widget.open_all_models_popup(models);
            }
            AppEvent::OpenFullAccessConfirmation {
                preset,
                return_to_permissions,
                profile_selection,
            } => {
                self.chat_widget.open_full_access_confirmation(
                    preset,
                    return_to_permissions,
                    profile_selection,
                );
            }
            AppEvent::OpenWorldWritableWarningConfirmation {
                preset,
                profile_selection,
                sample_paths,
                extra_count,
                failed_scan,
            } => {
                self.chat_widget.open_world_writable_warning_confirmation(
                    preset,
                    profile_selection,
                    sample_paths,
                    extra_count,
                    failed_scan,
                );
            }
            AppEvent::LaunchExternalEditor => {
                if self.chat_widget.external_editor_state() == ExternalEditorState::Active {
                    self.launch_external_editor(tui).await;
                }
            }
            AppEvent::OpenWindowsSandboxEnablePrompt {
                preset,
                profile_selection,
            } => {
                self.chat_widget
                    .open_windows_sandbox_enable_prompt(preset, profile_selection);
            }
            AppEvent::OpenWindowsSandboxFallbackPrompt {
                preset,
                profile_selection,
            } => {
                self.session_telemetry.counter(
                    "codex.windows_sandbox.fallback_prompt_shown",
                    1,
                    &[],
                );
                self.chat_widget.clear_windows_sandbox_setup_status();
                if let Some(started_at) = self.windows_sandbox.setup_started_at.take() {
                    self.session_telemetry.record_duration(
                        "codex.windows_sandbox.elevated_setup_duration_ms",
                        started_at.elapsed(),
                        &[("result", "failure")],
                    );
                }
                self.chat_widget
                    .open_windows_sandbox_fallback_prompt(preset, profile_selection);
            }
            AppEvent::BeginWindowsSandboxElevatedSetup {
                preset,
                profile_selection,
            } => {
                #[cfg(any(target_os = "windows", test))]
                if !self
                    .chat_widget
                    .windows_sandbox_mode_allowed(WindowsSandboxMode::Elevated)
                {
                    tracing::warn!(
                        "refusing to set up elevated Windows sandbox mode disallowed by requirements"
                    );
                    self.chat_widget.add_info_message(
                        "That Windows sandbox option is disallowed by requirements.".to_string(),
                        None,
                    );
                    return Ok(AppRunControl::Continue);
                }
                #[cfg(target_os = "windows")]
                {
                    let setup_permissions = match self
                        .windows_setup_permissions(&preset, profile_selection.as_ref())
                        .await
                    {
                        Ok(setup_permissions) => setup_permissions,
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "failed to resolve permission profile for elevated Windows sandbox setup"
                            );
                            self.chat_widget.add_error_message(format!(
                                "Failed to prepare Windows sandbox for the selected permission profile: {err}"
                            ));
                            return Ok(AppRunControl::Continue);
                        }
                    };
                    let permission_profile = setup_permissions.permission_profile;
                    let workspace_roots = setup_permissions.workspace_roots;
                    let command_cwd = self.config.cwd.clone();
                    let env_map: std::collections::HashMap<String, String> =
                        std::env::vars().collect();
                    let codex_home = self.config.codex_home.clone();
                    let tx = self.app_event_tx.clone();

                    if crate::windows_sandbox::sandbox_setup_is_complete(codex_home.as_path()) {
                        tx.send(AppEvent::EnableWindowsSandboxForAgentMode {
                            preset,
                            mode: WindowsSandboxEnableMode::Elevated,
                            profile_selection,
                        });
                        return Ok(AppRunControl::Continue);
                    }

                    self.chat_widget.show_windows_sandbox_setup_status();
                    self.windows_sandbox.setup_started_at = Some(Instant::now());
                    let session_telemetry = self.session_telemetry.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = crate::windows_sandbox::run_elevated_setup(
                            &permission_profile,
                            workspace_roots.as_slice(),
                            command_cwd.as_path(),
                            &env_map,
                            codex_home.as_path(),
                        );
                        let event = match result {
                            Ok(()) => {
                                session_telemetry.counter(
                                    "codex.windows_sandbox.elevated_setup_success",
                                    1,
                                    &[],
                                );
                                AppEvent::EnableWindowsSandboxForAgentMode {
                                    preset: preset.clone(),
                                    mode: WindowsSandboxEnableMode::Elevated,
                                    profile_selection: profile_selection.clone(),
                                }
                            }
                            Err(err) => {
                                let mut code_tag: Option<String> = None;
                                let mut message_tag: Option<String> = None;
                                if let Some((code, message)) =
                                    crate::windows_sandbox::elevated_setup_failure_details(&err)
                                {
                                    code_tag = Some(code);
                                    message_tag = Some(message);
                                }
                                let mut tags: Vec<(&str, &str)> = Vec::new();
                                if let Some(code) = code_tag.as_deref() {
                                    tags.push(("code", code));
                                }
                                if let Some(message) = message_tag.as_deref() {
                                    tags.push(("message", message));
                                }
                                session_telemetry.counter(
                                    crate::windows_sandbox::elevated_setup_failure_metric_name(
                                        &err,
                                    ),
                                    1,
                                    &tags,
                                );
                                tracing::error!(
                                    error = %err,
                                    "failed to run elevated Windows sandbox setup"
                                );
                                AppEvent::OpenWindowsSandboxFallbackPrompt {
                                    preset,
                                    profile_selection,
                                }
                            }
                        };
                        tx.send(event);
                    });
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = (preset, profile_selection);
                }
            }
            AppEvent::BeginWindowsSandboxLegacySetup {
                preset,
                profile_selection,
            } => {
                #[cfg(any(target_os = "windows", test))]
                if !self
                    .chat_widget
                    .windows_sandbox_mode_allowed(WindowsSandboxMode::Unelevated)
                {
                    tracing::warn!(
                        "refusing to set up unelevated Windows sandbox mode disallowed by requirements"
                    );
                    self.chat_widget.add_info_message(
                        "That Windows sandbox option is disallowed by requirements.".to_string(),
                        None,
                    );
                    return Ok(AppRunControl::Continue);
                }
                #[cfg(target_os = "windows")]
                {
                    let setup_permissions = match self
                        .windows_setup_permissions(&preset, profile_selection.as_ref())
                        .await
                    {
                        Ok(setup_permissions) => setup_permissions,
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "failed to resolve permission profile for legacy Windows sandbox setup"
                            );
                            self.chat_widget.add_error_message(format!(
                                "Failed to prepare Windows sandbox for the selected permission profile: {err}"
                            ));
                            return Ok(AppRunControl::Continue);
                        }
                    };
                    let permission_profile = setup_permissions.permission_profile;
                    let workspace_roots = setup_permissions.workspace_roots;
                    let command_cwd = self.config.cwd.clone();
                    let env_map: std::collections::HashMap<String, String> =
                        std::env::vars().collect();
                    let codex_home = self.config.codex_home.clone();
                    let tx = self.app_event_tx.clone();
                    let session_telemetry = self.session_telemetry.clone();

                    self.chat_widget.show_windows_sandbox_setup_status();
                    tokio::task::spawn_blocking(move || {
                        if let Err(err) =
                            octopus_windows_sandbox::run_windows_sandbox_legacy_preflight(
                                &permission_profile,
                                workspace_roots.as_slice(),
                                codex_home.as_path(),
                                command_cwd.as_path(),
                                &env_map,
                            )
                        {
                            session_telemetry.counter(
                                "codex.windows_sandbox.legacy_setup_preflight_failed",
                                1,
                                &[],
                            );
                            tracing::warn!(
                                error = %err,
                                "failed to preflight non-admin Windows sandbox setup"
                            );
                        }
                        tx.send(AppEvent::EnableWindowsSandboxForAgentMode {
                            preset,
                            mode: WindowsSandboxEnableMode::Legacy,
                            profile_selection,
                        });
                    });
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = (preset, profile_selection);
                }
            }
            AppEvent::BeginWindowsSandboxGrantReadRoot { path } => {
                #[cfg(target_os = "windows")]
                {
                    self.chat_widget
                        .add_to_history(history_cell::new_info_event(
                            format!("Granting sandbox read access to {path} ..."),
                            None,
                        ));

                    let permission_profile = self.config.permissions.effective_permission_profile();
                    let workspace_roots = self.config.effective_workspace_roots();
                    let command_cwd = self.config.cwd.clone();
                    let env_map: std::collections::HashMap<String, String> =
                        std::env::vars().collect();
                    let codex_home = self.config.codex_home.clone();
                    let tx = self.app_event_tx.clone();

                    tokio::task::spawn_blocking(move || {
                        let requested_path = PathBuf::from(path);
                        let event = match crate::windows_sandbox::grant_read_root_non_elevated(
                            &permission_profile,
                            workspace_roots.as_slice(),
                            command_cwd.as_path(),
                            &env_map,
                            codex_home.as_path(),
                            requested_path.as_path(),
                        ) {
                            Ok(canonical_path) => AppEvent::WindowsSandboxGrantReadRootCompleted {
                                path: canonical_path,
                                error: None,
                            },
                            Err(err) => AppEvent::WindowsSandboxGrantReadRootCompleted {
                                path: requested_path,
                                error: Some(err.to_string()),
                            },
                        };
                        tx.send(event);
                    });
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = path;
                }
            }
            AppEvent::WindowsSandboxGrantReadRootCompleted { path, error } => match error {
                Some(err) => {
                    self.chat_widget
                        .add_to_history(history_cell::new_error_event(format!("Error: {err}")));
                }
                None => {
                    self.chat_widget
                        .add_to_history(history_cell::new_info_event(
                            format!("Sandbox read access granted for {}", path.display()),
                            None,
                        ));
                }
            },
            AppEvent::EnableWindowsSandboxForAgentMode {
                preset,
                mode,
                profile_selection,
            } => {
                #[cfg(target_os = "windows")]
                {
                    self.chat_widget.clear_windows_sandbox_setup_status();
                    if let Some(started_at) = self.windows_sandbox.setup_started_at.take() {
                        self.session_telemetry.record_duration(
                            "codex.windows_sandbox.elevated_setup_duration_ms",
                            started_at.elapsed(),
                            &[("result", "success")],
                        );
                    }
                    let selected_mode = match mode {
                        WindowsSandboxEnableMode::Elevated => WindowsSandboxMode::Elevated,
                        WindowsSandboxEnableMode::Legacy => WindowsSandboxMode::Unelevated,
                    };
                    let elevated_enabled = selected_mode == WindowsSandboxMode::Elevated;
                    if !self.chat_widget.windows_sandbox_mode_allowed(selected_mode) {
                        tracing::warn!(
                            ?selected_mode,
                            "refusing to persist Windows sandbox mode disallowed by requirements"
                        );
                        self.chat_widget.add_info_message(
                            "That Windows sandbox option is disallowed by requirements."
                                .to_string(),
                            None,
                        );
                        return Ok(AppRunControl::Continue);
                    }
                    let edits =
                        crate::config_update::build_windows_sandbox_mode_edits(elevated_enabled);
                    match crate::config_update::write_config_batch_local(
                        self.config_runtime.as_ref(),
                        edits,
                    )
                    .await
                    {
                        Ok(()) => {
                            if elevated_enabled {
                                self.config.set_windows_sandbox_enabled(false);
                                self.config.set_windows_elevated_sandbox_enabled(true);
                            } else {
                                self.config.set_windows_sandbox_enabled(true);
                                self.config.set_windows_elevated_sandbox_enabled(false);
                            }
                            self.chat_widget.set_windows_sandbox_mode(
                                self.config.permissions.windows_sandbox_mode,
                            );
                            let windows_sandbox_level =
                                crate::windows_sandbox::level_from_config(&self.config);
                            if let Some((sample_paths, extra_count, failed_scan)) =
                                self.chat_widget.world_writable_warning_details()
                            {
                                self.app_event_tx.send(AppEvent::CodexOp(
                                    AppCommand::override_turn_context(
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                        #[cfg(target_os = "windows")]
                                        Some(windows_sandbox_level),
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                    ),
                                ));
                                self.app_event_tx.send(
                                    AppEvent::OpenWorldWritableWarningConfirmation {
                                        preset: Some(preset.clone()),
                                        profile_selection: profile_selection.clone(),
                                        sample_paths,
                                        extra_count,
                                        failed_scan,
                                    },
                                );
                            } else if let Some(selection) = profile_selection {
                                self.app_event_tx.send(AppEvent::CodexOp(
                                    AppCommand::override_turn_context(
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                        #[cfg(target_os = "windows")]
                                        Some(windows_sandbox_level),
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                    ),
                                ));
                                if self.apply_permission_profile_selection(selection).await {
                                    self.chat_widget.submit_initial_user_message_if_pending();
                                }
                                self.chat_widget.add_plain_history_lines(vec![
                                    Line::from(vec!["• ".dim(), "Sandbox ready".into()]),
                                    Line::from(vec![
                                        "  ".into(),
                                        "Octopus can now edit files and execute commands inside this worker environment"
                                            .dark_gray(),
                                    ]),
                                ]);
                            } else {
                                self.app_event_tx.send(AppEvent::CodexOp(
                                    AppCommand::override_turn_context(
                                        None,
                                        Some(AskForApproval::from(preset.approval)),
                                        Some(self.config.approvals_reviewer),
                                        Some(preset.permission_profile.clone()),
                                        Some(preset.active_permission_profile.clone()),
                                        #[cfg(target_os = "windows")]
                                        Some(windows_sandbox_level),
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                    ),
                                ));
                                self.app_event_tx.send(AppEvent::UpdateAskForApprovalPolicy(
                                    AskForApproval::from(preset.approval),
                                ));
                                self.app_event_tx
                                    .send(AppEvent::UpdateActivePermissionProfile(
                                        preset.active_permission_profile.clone(),
                                    ));
                                self.chat_widget.add_plain_history_lines(vec![
                                    Line::from(vec!["• ".dim(), "Sandbox ready".into()]),
                                    Line::from(vec![
                                        "  ".into(),
                                        "Octopus can now edit files and execute commands inside this worker environment"
                                            .dark_gray(),
                                    ]),
                                ]);
                            }
                        }
                        Err(err) => {
                            tracing::error!(
                                error = %err,
                                "failed to enable Windows sandbox feature"
                            );
                            self.chat_widget.add_error_message(format!(
                                "Failed to enable the Windows sandbox feature: {err}"
                            ));
                        }
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = (preset, mode, profile_selection);
                }
            }
            AppEvent::PersistModelSelection { model, effort } => {
                let effort_label = effort
                    .as_ref()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_else(|| "default".to_string());
                let mut message = format!("Model changed to {model}");
                if let Some(label) = Self::reasoning_label_for(&model, effort.as_ref()) {
                    message.push(' ');
                    message.push_str(&label);
                }
                if app_server.drives_embedded_spine() {
                    match app_server
                        .persist_model_interface_selection(model.clone(), effort.clone())
                        .await
                    {
                        Ok(()) => match self.sync_chat_to_active_topology(app_server) {
                            Ok(()) => {
                                tracing::info!(
                                    "Selected model: {model}, Selected effort: {effort_label}; private provider interface config updated through file.write evidence"
                                );
                                self.chat_widget.add_info_message(message, None);
                            }
                            Err(err) => self.chat_widget.add_error_message(format!(
                                "Model interface changed, but the terminal could not attach its updated topology: {err}"
                            )),
                        },
                        Err(err) => {
                            let _ = self.sync_chat_to_active_topology(app_server);
                            tracing::error!(
                                error = %err,
                                "failed to persist private provider interface model selection"
                            );
                            self.chat_widget.add_error_message(format!(
                                "Failed to update this run's model interface: {err}"
                            ));
                        }
                    }
                    return Ok(AppRunControl::Continue);
                }
                match crate::config_update::write_config_batch_local(
                    self.config_runtime.as_ref(),
                    crate::config_update::build_model_selection_edits(
                        model.as_str(),
                        effort.as_ref(),
                    ),
                )
                .await
                {
                    Ok(_) => {
                        tracing::info!("Selected model: {model}, Selected effort: {effort_label}");
                        self.chat_widget.add_info_message(message, None);
                    }
                    Err(err) => {
                        let error = format_config_error(&err);
                        tracing::error!(
                            error = %error,
                            "failed to persist model selection"
                        );
                        self.chat_widget
                            .add_error_message(format!("Failed to save default model: {error}"));
                    }
                }
            }
            AppEvent::PersistPersonalitySelection { personality } => {
                if app_server.drives_embedded_spine() {
                    let label = Self::personality_label(personality);
                    let message = format!("Personality set to {label}");
                    tracing::info!(
                        "Selected personality: {label}; embedded spine carries personality in turn context rather than config persistence"
                    );
                    self.chat_widget.add_info_message(message, None);
                    return Ok(AppRunControl::Continue);
                }
                match crate::config_update::write_config_batch_local(
                    self.config_runtime.as_ref(),
                    vec![crate::config_update::replace_config_value(
                        "personality",
                        serde_json::json!(personality.to_string()),
                    )],
                )
                .await
                {
                    Ok(_) => {
                        let label = Self::personality_label(personality);
                        let message = format!("Personality set to {label}");
                        self.chat_widget.add_info_message(message, None);
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to persist personality selection"
                        );
                        self.chat_widget.add_error_message(format!(
                            "Failed to save default personality: {err}"
                        ));
                    }
                }
            }
            AppEvent::PersistServiceTierSelection { service_tier } => {
                self.refresh_status_line();
                self.config.service_tier = service_tier.clone();
                if app_server.drives_embedded_spine() {
                    let message = if let Some(service_tier) = service_tier {
                        format!("Service tier set to {service_tier}")
                    } else {
                        "Service tier cleared".to_string()
                    };
                    tracing::info!(
                        "Selected service tier; embedded spine carries it in turn context rather than config persistence"
                    );
                    self.chat_widget.add_info_message(message, None);
                    return Ok(AppRunControl::Continue);
                }
                let edits = crate::config_update::build_service_tier_selection_edits(
                    service_tier.as_deref(),
                );
                match crate::config_update::write_config_batch_local(
                    self.config_runtime.as_ref(),
                    edits,
                )
                .await
                {
                    Ok(_) => {
                        let message = if let Some(service_tier) = service_tier {
                            format!("Service tier set to {service_tier}")
                        } else {
                            "Service tier cleared".to_string()
                        };
                        self.chat_widget.add_info_message(message, None);
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "failed to persist service tier selection");
                        self.chat_widget.add_error_message(format!(
                            "Failed to save default service tier: {err}"
                        ));
                    }
                }
            }
            AppEvent::UpdateAskForApprovalPolicy(policy) => {
                let mut config = self.config.clone();
                if !self.try_set_approval_policy_on_config(
                    &mut config,
                    policy,
                    "Failed to set approval policy",
                    "failed to set approval policy on app config",
                ) {
                    return Ok(AppRunControl::Continue);
                }
                self.config = config;
                let approval_policy =
                    AskForApproval::from(self.config.permissions.approval_policy.value());
                self.runtime_approval_policy_override = Some(approval_policy);
                self.chat_widget.set_approval_policy(approval_policy);
            }
            AppEvent::UpdateActivePermissionProfile(active_permission_profile) => {
                let mut config = self.config.clone();
                let Some(permission_profile) = self
                    .try_set_builtin_active_permission_profile_on_config(
                        &mut config,
                        active_permission_profile.clone(),
                        "Failed to set permission profile",
                        "failed to set active permission profile on app config",
                    )
                else {
                    return Ok(AppRunControl::Continue);
                };
                #[cfg(target_os = "windows")]
                let permission_profile_is_managed_restricted =
                    managed_filesystem_sandbox_is_restricted(&permission_profile);
                let permission_profile_for_chat = permission_profile.clone();

                self.config = config;
                if let Err(err) = self
                    .chat_widget
                    .set_permission_profile_from_session_snapshot(
                        SessionPermissionSnapshot::active(
                            permission_profile_for_chat,
                            active_permission_profile,
                        ),
                    )
                {
                    tracing::warn!(%err, "failed to set permission profile on chat config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set permission profile: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                self.runtime_permission_profile_override =
                    Some(RuntimePermissionProfileOverride::from_config(&self.config));
                self.chat_widget.submit_initial_user_message_if_pending();

                #[cfg(target_os = "windows")]
                {
                    if self.windows_sandbox.skip_world_writable_scan_once {
                        self.windows_sandbox.skip_world_writable_scan_once = false;
                        return Ok(AppRunControl::Continue);
                    }

                    let should_check = crate::windows_sandbox::level_from_config(&self.config)
                        != WindowsSandboxLevel::Disabled
                        && permission_profile_is_managed_restricted
                        && !self.chat_widget.world_writable_warning_hidden();
                    if should_check {
                        let cwd = self.config.cwd.clone();
                        let workspace_roots = self.config.effective_workspace_roots();
                        let env_map: std::collections::HashMap<String, String> =
                            std::env::vars().collect();
                        let tx = self.app_event_tx.clone();
                        let logs_base_dir = self.config.codex_home.clone();
                        let permission_profile =
                            self.config.permissions.effective_permission_profile();
                        Self::spawn_world_writable_scan(
                            cwd,
                            workspace_roots,
                            env_map,
                            logs_base_dir,
                            permission_profile,
                            tx,
                        );
                    }
                }
            }
            AppEvent::SelectPermissionProfile(selection) => {
                if self.apply_permission_profile_selection(selection).await {
                    self.chat_widget.submit_initial_user_message_if_pending();
                }
            }
            AppEvent::UpdateApprovalsReviewer(policy) => {
                self.config.approvals_reviewer = policy;
                self.chat_widget.set_approvals_reviewer(policy);
                if let Err(err) = crate::config_update::write_config_batch_local(
                    self.config_runtime.as_ref(),
                    vec![crate::config_update::replace_config_value(
                        "approvals_reviewer",
                        serde_json::json!(policy.to_string()),
                    )],
                )
                .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist approvals reviewer update"
                    );
                    self.chat_widget
                        .add_error_message(format!("Failed to save approvals reviewer: {err}"));
                }
            }
            AppEvent::UpdateFeatureFlags { updates } => {
                self.update_feature_flags(app_server, updates).await;
            }
            AppEvent::UpdateMemorySettings {
                use_memories,
                generate_memories,
            } => {
                self.update_memory_settings_with_app_server(
                    app_server,
                    use_memories,
                    generate_memories,
                )
                .await;
            }
            AppEvent::ResetMemories => {
                self.reset_memories_with_app_server(app_server).await;
            }
            AppEvent::SkipNextWorldWritableScan => {
                self.windows_sandbox.skip_world_writable_scan_once = true;
            }
            AppEvent::UpdateFullAccessWarningAcknowledged(ack) => {
                self.chat_widget.set_full_access_warning_acknowledged(ack);
            }
            AppEvent::UpdateWorldWritableWarningAcknowledged(ack) => {
                self.chat_widget
                    .set_world_writable_warning_acknowledged(ack);
            }
            AppEvent::UpdateRateLimitSwitchPromptHidden(hidden) => {
                self.chat_widget.set_rate_limit_switch_prompt_hidden(hidden);
            }
            AppEvent::UpdatePlanModeReasoningEffort(effort) => {
                self.config.plan_mode_reasoning_effort = effort.clone();
                self.chat_widget.set_plan_mode_reasoning_effort(effort);
            }
            AppEvent::PersistFullAccessWarningAcknowledged => {
                if let Err(err) = crate::config_update::write_config_batch_local(
                    self.config_runtime.as_ref(),
                    vec![crate::config_update::replace_config_value(
                        "notices.hide_full_access_warning",
                        serde_json::json!(true),
                    )],
                )
                .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist full access warning acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save full access confirmation preference: {err}"
                    ));
                }
            }
            AppEvent::PersistWorldWritableWarningAcknowledged => {
                if let Err(err) = crate::config_update::write_config_batch_local(
                    self.config_runtime.as_ref(),
                    vec![crate::config_update::replace_config_value(
                        "notices.hide_world_writable_warning",
                        serde_json::json!(true),
                    )],
                )
                .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist world-writable warning acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save Agent mode warning preference: {err}"
                    ));
                }
            }
            AppEvent::PersistRateLimitSwitchPromptHidden => {
                if let Err(err) = crate::config_update::write_config_batch_local(
                    self.config_runtime.as_ref(),
                    vec![crate::config_update::replace_config_value(
                        "notices.hide_rate_limit_model_nudge",
                        serde_json::json!(true),
                    )],
                )
                .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist rate limit switch prompt preference"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save rate limit reminder preference: {err}"
                    ));
                }
            }
            AppEvent::PersistPlanModeReasoningEffort(effort) => {
                let key_path = "plan_mode_reasoning_effort";
                let edit = if let Some(effort) = effort {
                    crate::config_update::replace_config_value(
                        key_path,
                        serde_json::json!(effort.to_string()),
                    )
                } else {
                    crate::config_update::clear_config_value(key_path)
                };
                if let Err(err) = crate::config_update::write_config_batch_local(
                    self.config_runtime.as_ref(),
                    vec![edit],
                )
                .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist plan mode reasoning effort"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save Plan mode reasoning effort: {err}"
                    ));
                }
            }
            AppEvent::OpenApprovalsPopup => {
                self.chat_widget.open_approvals_popup();
            }
            AppEvent::OpenAgentPicker => {
                self.chat_widget
                    .add_info_message("No agents available yet.".to_string(), None);
            }
            AppEvent::SelectAgentThread(thread_id) => {
                self.chat_widget
                    .add_error_message(format!("Agent thread {thread_id} is no longer available."));
            }
            AppEvent::StartSide {
                parent_thread_id,
                user_message,
            } => {
                if self.chat_widget.side_conversation_active() {
                    self.chat_widget.add_error_message(
                        "A side question is already open. Press Ctrl+C to return first."
                            .to_string(),
                    );
                    return Ok(AppRunControl::Continue);
                }
                let Some(parent) = app_server.active_run_id() else {
                    self.chat_widget
                        .add_error_message("Side question requires an active task.".to_string());
                    return Ok(AppRunControl::Continue);
                };
                if parent.as_str() != parent_thread_id {
                    self.chat_widget.add_error_message(
                        "The active task changed before the side question could start.".to_string(),
                    );
                    return Ok(AppRunControl::Continue);
                }
                let has_history = match app_server.history_pager().and_then(|mut pager| {
                    pager
                        .read_page(1, 50)
                        .map(|page| !page.entries.is_empty())
                        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))
                }) {
                    Ok(has_history) => has_history,
                    Err(error) => {
                        if let Some(user_message) = user_message {
                            self.chat_widget
                                .restore_user_message_to_composer(user_message);
                        }
                        self.chat_widget.add_error_message(format!(
                            "Side question could not verify the parent task history: {error}"
                        ));
                        return Ok(AppRunControl::Continue);
                    }
                };
                if !has_history {
                    if let Some(user_message) = user_message {
                        self.chat_widget
                            .restore_user_message_to_composer(user_message);
                    }
                    self.chat_widget.add_error_message(
                        "Side questions are available after the first task message is recorded."
                            .to_string(),
                    );
                    return Ok(AppRunControl::Continue);
                }

                let parent_was_running = self.chat_widget.is_task_running();
                match app_server.author_side_run().await {
                    Ok(side_run) => {
                        self.side_parent_run = Some(parent);
                        self.side_parent_was_running = parent_was_running;
                        self.reset_for_thread_switch(tui)?;
                        let init = self.chatwidget_init_for_forked_or_resumed_thread(
                            tui,
                            self.config.clone(),
                            None,
                        );
                        self.replace_chat_widget(ChatWidget::new_with_app_event(init));
                        self.sync_chat_to_active_topology(app_server)?;
                        self.chat_widget.set_side_conversation_active(true);
                        self.chat_widget.set_side_conversation_context_label(Some(
                            if parent_was_running {
                                "Main task was working when side opened"
                            } else {
                                "Main task was ready when side opened"
                            }
                            .to_string(),
                        ));
                        self.chat_widget.add_info_message(
                            format!("Side question {}.", side_run.as_str()),
                            Some(
                                "This is an isolated run with explicit permitted artifact references. Press Ctrl+C or Ctrl+D to return."
                                    .to_string(),
                            ),
                        );
                        if let Some(user_message) = user_message {
                            self.chat_widget
                                .submit_user_message_as_plain_user_turn(user_message);
                        }
                        tui.frame_requester().schedule_frame();
                    }
                    Err(error) => {
                        if let Some(user_message) = user_message {
                            self.chat_widget
                                .restore_user_message_to_composer(user_message);
                        }
                        self.chat_widget
                            .add_error_message(format!("Failed to start side question: {error}"));
                    }
                }
                return Ok(AppRunControl::Continue);
            }
            AppEvent::SetHookEnabled { key, enabled } => {
                self.set_hook_enabled(app_server, key, enabled);
            }
            AppEvent::TrustHook { key, current_hash } => {
                self.trust_hook(app_server, key, current_hash);
            }
            AppEvent::TrustHooks { updates } => {
                self.trust_hooks(app_server, updates);
            }
            AppEvent::HookEnabledSet {
                key,
                enabled,
                result,
            } => {
                let queued_enabled = self
                    .pending_hook_enabled_writes
                    .get_mut(&key)
                    .and_then(Option::take);
                let should_apply_result = if let Some(queued_enabled) = queued_enabled
                    && (result.is_err() || queued_enabled != enabled)
                {
                    self.spawn_hook_enabled_write(app_server, key.clone(), queued_enabled);
                    false
                } else {
                    true
                };
                if should_apply_result {
                    self.pending_hook_enabled_writes.remove(&key);
                    if let Err(err) = result {
                        self.chat_widget.add_error_message(err);
                    }
                }
            }
            AppEvent::HookTrusted { result } => {
                if let Err(err) = result {
                    self.chat_widget.add_error_message(err);
                }
            }
            AppEvent::OpenPermissionsPopup => {
                self.chat_widget.open_permissions_popup();
            }
            AppEvent::OpenReviewBranchPicker(cwd) => {
                self.chat_widget.show_review_branch_picker(&cwd).await;
            }
            AppEvent::OpenReviewCommitPicker(cwd) => {
                self.chat_widget.show_review_commit_picker(&cwd).await;
            }
            AppEvent::OpenReviewCustomPrompt => {
                self.chat_widget.show_review_custom_prompt();
            }
            AppEvent::SubmitUserMessageWithMode {
                text,
                collaboration_mode,
            } => {
                self.chat_widget
                    .submit_user_message_with_mode(text, collaboration_mode);
            }
            AppEvent::FullScreenApprovalRequest(request) => match request {
                ApprovalRequest::ApplyPatch { cwd, changes, .. } => {
                    let _ = tui.enter_alt_screen();
                    let diff_summary = DiffSummary::new(changes, cwd);
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![diff_summary.into()],
                        "P A T C H".to_string(),
                        self.keymap.pager.clone(),
                    ));
                }
                ApprovalRequest::Exec { command, .. } => {
                    let _ = tui.enter_alt_screen();
                    let full_cmd = strip_bash_lc_and_escape(&command);
                    let full_cmd_lines = highlight_bash_to_lines(&full_cmd);
                    self.overlay = Some(Overlay::new_static_with_lines(
                        full_cmd_lines,
                        "E X E C".to_string(),
                        self.keymap.pager.clone(),
                    ));
                }
                ApprovalRequest::Permissions {
                    environment_id,
                    permissions,
                    reason,
                    ..
                } => {
                    let _ = tui.enter_alt_screen();
                    let mut lines = Vec::new();
                    if let Some(environment_id) = environment_id {
                        lines.push(Line::from(vec![
                            "Environment: ".into(),
                            environment_id.bold(),
                        ]));
                        lines.push(Line::from(""));
                    }
                    if let Some(reason) = reason {
                        lines.push(Line::from(vec!["Reason: ".into(), reason.italic()]));
                        lines.push(Line::from(""));
                    }
                    if let Some(rule_line) =
                        crate::bottom_pane::format_requested_permissions_rule(&permissions)
                    {
                        lines.push(Line::from(vec![
                            "Permission rule: ".into(),
                            rule_line.cyan(),
                        ]));
                    }
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![Box::new(Paragraph::new(lines).wrap(Wrap { trim: false }))],
                        "P E R M I S S I O N S".to_string(),
                        self.keymap.pager.clone(),
                    ));
                }
            },
            AppEvent::StatusLineSetup {
                items,
                use_theme_colors,
            } => {
                let ids = items.iter().map(ToString::to_string).collect::<Vec<_>>();
                let items_edit = octopus_world_terminal_config::edit::status_line_items_edit(&ids);
                let colors_edit = octopus_world_terminal_config::edit::status_line_use_colors_edit(
                    use_theme_colors,
                );
                let apply_result = self
                    .config
                    .terminal_edits()
                    .with_edits([items_edit, colors_edit])
                    .apply()
                    .await;
                match apply_result {
                    Ok(()) => {
                        self.config.terminal.status_line = Some(ids.clone());
                        self.config.terminal.status_line_use_colors = use_theme_colors;
                        self.chat_widget.setup_status_line(items, use_theme_colors);
                    }
                    Err(err) => {
                        let error = format_config_error(&err);
                        tracing::error!(error = %error, "failed to persist status line settings; keeping previous selection");
                        self.chat_widget.add_error_message(format!(
                            "Failed to save status line settings: {error}"
                        ));
                    }
                }
            }
            AppEvent::StatusLineBranchUpdated { cwd, branch } => {
                self.chat_widget.set_status_line_branch(cwd, branch);
                self.refresh_status_line();
            }
            AppEvent::StatusLineGitSummaryUpdated { cwd, summary } => {
                self.chat_widget.set_status_line_git_summary(cwd, summary);
                self.refresh_status_line();
            }
            AppEvent::StatusLineSetupCancelled => {
                self.chat_widget.cancel_status_line_setup();
            }
            AppEvent::TerminalTitleSetup { items } => {
                let ids = items.iter().map(ToString::to_string).collect::<Vec<_>>();
                let edit = octopus_world_terminal_config::edit::terminal_title_items_edit(&ids);
                let apply_result = self
                    .config
                    .terminal_edits()
                    .with_edits([edit])
                    .apply()
                    .await;
                match apply_result {
                    Ok(()) => {
                        self.config.terminal.terminal_title = Some(ids.clone());
                        self.chat_widget.setup_terminal_title(items);
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "failed to persist terminal title items; keeping previous selection");
                        self.chat_widget.revert_terminal_title_setup_preview();
                        self.chat_widget.add_error_message(format!(
                            "Failed to save terminal title items: {err}"
                        ));
                    }
                }
            }
            AppEvent::TerminalTitleSetupPreview { items } => {
                self.chat_widget.preview_terminal_title(items);
            }
            AppEvent::TerminalTitleSetupCancelled => {
                self.chat_widget.cancel_terminal_title_setup();
            }
            AppEvent::SyntaxThemeSelected { name } => {
                let edit = octopus_world_terminal_config::edit::syntax_theme_edit(&name);
                let apply_result = self
                    .config
                    .terminal_edits()
                    .with_edits([edit])
                    .apply()
                    .await;
                match apply_result {
                    Ok(()) => {
                        if let Some(theme) = crate::render::highlight::resolve_theme_by_name(
                            &name,
                            Some(&self.config.codex_home),
                        ) {
                            crate::render::highlight::set_syntax_theme(theme);
                        }
                        self.sync_tui_theme_selection(name);
                        self.refresh_status_line();
                        tui.frame_requester().schedule_frame();
                    }
                    Err(err) => {
                        self.restore_runtime_theme_from_config();
                        self.refresh_status_line();
                        tracing::error!(error = %err, "failed to persist theme selection");
                        self.chat_widget
                            .add_error_message(format!("Failed to save theme: {err}"));
                    }
                }
            }
            AppEvent::SyntaxThemePreviewed => {
                self.refresh_status_line();
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenKeymapActionMenu { context, action } => {
                self.chat_widget
                    .open_keymap_action_menu(context, action, &self.keymap);
            }
            AppEvent::OpenKeymapReplaceBindingMenu { context, action } => {
                self.chat_widget
                    .open_keymap_replace_binding_menu(context, action, &self.keymap);
            }
            AppEvent::OpenKeymapCapture {
                context,
                action,
                intent,
            } => {
                self.chat_widget
                    .open_keymap_capture(context, action, intent, &self.keymap);
            }
            AppEvent::OpenKeymapDebug => {
                self.chat_widget.open_keymap_debug(&self.keymap);
            }
            AppEvent::KeymapCaptured {
                context,
                action,
                key,
                intent,
            } => {
                self.apply_keymap_capture(context, action, key, intent)
                    .await;
            }
            AppEvent::KeymapCleared { context, action } => {
                self.apply_keymap_clear(context, action).await;
            }
        }
        Ok(AppRunControl::Continue)
    }

    async fn apply_keymap_capture(
        &mut self,
        context: String,
        action: String,
        key: String,
        intent: crate::app_event::KeymapEditIntent,
    ) {
        let outcome = match crate::keymap_setup::keymap_with_edit(
            &self.config.terminal.keymap,
            &self.keymap,
            &context,
            &action,
            &key,
            &intent,
        ) {
            Ok(outcome) => outcome,
            Err(err) => {
                self.chat_widget.add_error_message(err);
                return;
            }
        };
        let (keymap_config, bindings, message) = match outcome {
            crate::keymap_setup::KeymapEditOutcome::Updated {
                keymap_config,
                bindings,
                message,
            } => (*keymap_config, bindings, message),
            crate::keymap_setup::KeymapEditOutcome::Unchanged { message } => {
                self.chat_widget.add_info_message(message, None);
                return;
            }
        };

        let runtime_keymap = match RuntimeKeymap::from_config(&keymap_config) {
            Ok(runtime_keymap) => runtime_keymap,
            Err(err) => {
                let params = crate::keymap_setup::build_keymap_conflict_params(
                    context, action, key, intent, err,
                );
                self.chat_widget.show_selection_view(params);
                return;
            }
        };

        let edit =
            octopus_world_terminal_config::edit::keymap_bindings_edit(&context, &action, &bindings);
        match self
            .config
            .terminal_edits()
            .with_edits([edit])
            .apply()
            .await
        {
            Ok(()) => {
                self.config.terminal.keymap = keymap_config.clone();
                self.keymap = runtime_keymap.clone();
                self.chat_widget
                    .apply_keymap_update(keymap_config, &runtime_keymap);
                self.chat_widget
                    .return_to_keymap_picker(&context, &action, &runtime_keymap);
                self.chat_widget.add_info_message(message, None);
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to persist keymap binding");
                self.chat_widget
                    .add_error_message(format!("Failed to save shortcut: {err}"));
            }
        }
    }

    async fn apply_keymap_clear(&mut self, context: String, action: String) {
        let keymap_config = match crate::keymap_setup::keymap_without_custom_binding(
            &self.config.terminal.keymap,
            &context,
            &action,
        ) {
            Ok(keymap_config) => keymap_config,
            Err(err) => {
                self.chat_widget.add_error_message(err);
                return;
            }
        };

        let runtime_keymap = match RuntimeKeymap::from_config(&keymap_config) {
            Ok(runtime_keymap) => runtime_keymap,
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to refresh shortcuts: {err}"));
                return;
            }
        };

        let edit =
            octopus_world_terminal_config::edit::keymap_binding_clear_edit(&context, &action);
        match self
            .config
            .terminal_edits()
            .with_edits([edit])
            .apply()
            .await
        {
            Ok(()) => {
                self.config.terminal.keymap = keymap_config.clone();
                self.keymap = runtime_keymap.clone();
                self.chat_widget
                    .apply_keymap_update(keymap_config, &runtime_keymap);
                self.chat_widget
                    .return_to_keymap_picker(&context, &action, &runtime_keymap);
                self.chat_widget.add_info_message(
                    format!("Removed custom shortcut for `{context}.{action}`."),
                    None,
                );
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to clear keymap binding");
                self.chat_widget
                    .add_error_message(format!("Failed to remove shortcut: {err}"));
            }
        }
    }

    pub(super) async fn handle_exit_mode(&mut self, mode: ExitMode) -> AppRunControl {
        match mode {
            ExitMode::Detach { run_id } => AppRunControl::Detached { run_id },
            ExitMode::ShutdownFirst | ExitMode::Immediate => {
                AppRunControl::Exit(ExitReason::UserRequested)
            }
        }
    }

    pub(super) async fn archive_current_thread(
        &mut self,
        app_server: &mut AppServerSession,
    ) -> AppRunControl {
        match app_server.thread_archive().await {
            Ok(()) => AppRunControl::Exit(ExitReason::UserRequested),
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to archive current thread: {err}"));
                AppRunControl::Continue
            }
        }
    }

    pub(super) async fn delete_current_thread(
        &mut self,
        app_server: &mut AppServerSession,
    ) -> AppRunControl {
        match app_server.thread_delete().await {
            Ok(()) => AppRunControl::Exit(ExitReason::UserRequested),
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to delete current thread: {err}"));
                AppRunControl::Continue
            }
        }
    }
}
