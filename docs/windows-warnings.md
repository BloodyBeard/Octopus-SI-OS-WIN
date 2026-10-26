# Remaining compiler warnings

Last checked: `cargo check --workspace --all-targets`, on Linux and for Windows
(`x86_64-pc-windows-gnu`).

There are no Windows-only and no Linux-only warnings left. The 213 below
appear on every platform. All are `dead_code` (code nothing calls), and none
comes from a platform difference. Nearly all are in the Codex terminal UI and
core, which upstream cut down when it adapted Codex.

The one exception is `octopus-verifiers` `ProcessAuthority`. Its fields are
unread because only macOS has a process-sandbox backend (see
[windows-port.md](windows-port.md#runtime-gaps-on-windows)).

They are listed, not deleted, because deleting about 200 items in files that
upstream still edits would make merging upstream changes harder. Removing
them is a good change to send upstream.

| Crate | Warnings |
| --- | --- |
| `octopus-tui` | 145 |
| `octopus-codex-core` | 54 |
| `octopus-codex-cli` | 6 |
| `octopus-verifiers` | 2 |
| `octopus-operator-chat-client` | 1 |
| `octopus-app-server-transport` | 1 |
| `octopus-appserver-proto` | 1 |
| `octopus-provider-codex` | 1 |
| `octopus-code-mode-tool-runtime` | 1 |
| `octopus-process-session-tool-runtime` | 1 |

## octopus-app-server-transport

- `registry/interface/world_provider_codex/packages/octopus-app-server-transport/src/transport/remote_control/auth.rs`
  - line 83: function `unauthenticated_auth_provider` is never used

## octopus-appserver-proto

- `registry/interface/world_provider_codex/packages/octopus-appserver-proto/src/export.rs`
  - line 2302: function `schema_root` is never used

## octopus-code-mode-tool-runtime

- `registry/worker_environment_kind/code_mode/packages/octopus-code-mode-tool-runtime/src/owner_handoff.rs`
  - line 140: associated function `with_json_format` is never used

## octopus-codex-cli

- `registry/interface/world_provider_codex/packages/octopus-codex-cli/src/path_utils/mod.rs`
  - line 15: function `paths_match_after_normalization` is never used
  - line 25: function `normalize_for_native_workdir` is never used
  - line 29: struct `SymlinkWritePaths` is never constructed
  - line 34: function `resolve_symlink_write_paths` is never used
  - line 107: function `write_atomically` is never used
  - line 125: function `normalize_for_native_workdir_with_flag` is never used

## octopus-codex-core

- `registry/interface/world_provider_codex/packages/octopus-codex-core/src/config/config_loader_tests.rs`
  - line 89: function `write_linked_worktree_pointer` is never used
- `registry/interface/world_provider_codex/packages/octopus-codex-core/src/config/mod.rs`
  - line 166: constant `DEFAULT_AGENT_MAX_THREADS` is never used
  - line 285: function `test_config` is never used
  - line 327: methods `permission_profile_state` and `set_permission_profile_state` are never used
  - line 969: method `multi_agent_version_from_features` is never used
  - line 969: methods `multi_agent_version_from_features`, `validate_multi_agent_v2_config`, and `effective_agent_max_threads` are never used
  - line 1505: function `resolve_tool_suggest_config_from_layer_stack` is never used
  - line 2001: function `resolve_web_search_mode_for_turn` is never used
  - line 3204: method `network_proxy_spec_for_active_permission_profile` is never used
- `registry/interface/world_provider_codex/packages/octopus-codex-core/src/config/network_proxy_spec.rs`
  - line 153: methods `recompute_for_permission_profile`, `with_exec_policy_network_rules`, and `apply_to_started_proxy` are never used
  - line 349: function `apply_exec_policy_network_rules` is never used
  - line 355: function `upsert_network_domains` is never used
- `registry/interface/world_provider_codex/packages/octopus-codex-core/src/config_lock.rs`
  - line 15: struct `ConfigLockReplayOptions` is never constructed
  - line 38: function `config_lockfile` is never used
  - line 46: function `validate_config_lock_replay` is never used
  - line 122: function `config_lock_for_comparison` is never used
  - line 141: function `compact_diff` is never used
  - line 164: function `toml_round_trip` is never used
- `registry/interface/world_provider_codex/packages/octopus-codex-core/src/exec_policy.rs`
  - line 43: enum `ExecApprovalRequirement` is never used
  - line 58: method `proposed_execpolicy_amendment` is never used
  - line 73: constant `PROMPT_CONFLICT_REASON` is never used
  - line 75: constant `REJECT_SANDBOX_APPROVAL_REASON` is never used
  - line 77: constant `REJECT_RULES_APPROVAL_REASON` is never used
  - line 81: constant `DEFAULT_POLICY_FILE` is never used
  - line 82: static `BANNED_PREFIX_SUGGESTIONS` is never used
  - line 132: enum `ExecPolicyCommandOrigin` is never used
  - line 139: struct `UnmatchedCommandContext` is never constructed
  - line 149: struct `ExecPolicyCommands` is never constructed
  - line 155: function `child_uses_parent_exec_policy` is never used
  - line 176: function `is_policy_match` is never used
  - line 183: function `prompt_is_rejected_by_policy` is never used
  - line 230: enum `ExecPolicyUpdateError` is never used
  - line 244: struct `ExecPolicyManager` is never constructed
  - line 249: struct `ExecApprovalRequest` is never constructed
  - line 259: associated items `new`, `load`, `current`, `create_exec_approval_requirement_for_command`, `append_amendment_and_update`, and `append_network_rule_and_update` are never used
  - line 427: method `append_network_rule_and_update` is never used
  - line 619: function `render_decision_for_unmatched_command` is never used
  - line 709: function `profile_has_managed_filesystem_restrictions` is never used
  - line 719: function `default_policy_path` is never used
  - line 723: function `commands_for_exec_policy` is never used
  - line 763: function `try_derive_execpolicy_amendment_for_prompt_rules` is never used
  - line 784: function `try_derive_execpolicy_amendment_for_allow_rules` is never used
  - line 802: function `derive_requested_execpolicy_amendment_from_prefix_rule` is never used
  - line 842: function `prefix_rule_would_approve_all_commands` is never used
  - line 865: function `derive_prompt_reason` is never used
  - line 893: function `render_shlex_command` is never used
  - line 897: function `derive_forbidden_reason` is never used
- `registry/interface/world_provider_codex/packages/octopus-codex-core/src/spawn.rs`
  - line 22: struct `SpawnChildRequest` is never constructed
  - line 33: function `spawn_child_async` is never used
- `registry/interface/world_provider_codex/packages/octopus-codex-core/src/util.rs`
  - line 18: function `error_or_panic` is never used
- `registry/interface/world_provider_codex/packages/octopus-codex-core/src/utils/cache.rs`
  - line 10: struct `BlockingLruCache` is never constructed
  - line 19: associated items `new`, `get_or_insert_with`, and `get_or_try_insert_with` are never used
  - line 60: function `lock_if_runtime` is never used
  - line 68: function `sha1_digest` is never used

## octopus-operator-chat-client

- `registry/interface/world_operator_chat/packages/octopus-operator-chat-client/tests/support/composition_fixture.rs`
  - line 15: function `client_fixture` is never used

## octopus-process-session-tool-runtime

- `registry/worker_environment_kind/process_session/packages/octopus-process-session-tool-runtime/tests/support/execution_loading.rs`
  - line 3: function `stderr` is never used

## octopus-provider-codex

- `registry/interface/world_provider_codex/packages/octopus-provider-codex/../world_operator_chat/packages/octopus-operator-chat-client/tests/support/composition_fixture.rs`
  - line 15: function `client_fixture` is never used

## octopus-tui

- `registry/interface/world_terminal/packages/octopus-tui/src/app/app_server_events.rs`
  - line 5: method `refresh_tool_interface_startup_expected_endpoints_from_config` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/app/event_dispatch.rs`
  - line 7: constant `SHUTDOWN_FIRST_EXIT_TIMEOUT` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/app/tests.rs`
  - line 49: function `test_absolute_path` is never used
  - line 1106: function `next_user_turn_op` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/app_event.rs`
  - line 98: multiple variants are never constructed
  - line 317: variants `BeginInitialHistoryReplayBuffer`, `BeginThreadSwitchHistoryReplayBuffer`, `EndInitialHistoryReplayBuffer`, and `OpenAllModelsPopup` are never constructed
  - line 588: variant `Immediate` is never constructed
- `registry/interface/world_terminal/packages/octopus-tui/src/app_server_session.rs`
  - line 128: field `thread_params_mode` is never read
  - line 694: methods `remote_cwd_override`, `uses_remote_workspace`, `uses_embedded_app_server`, and `thread_set_name` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/auto_review_denials.rs`
  - line 7: constant `MAX_RECENT_DENIALS` is never used
  - line 15: method `push` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/bottom_pane/app_link_view.rs`
  - line 35: enum `AppLinkScreen` is never used
  - line 110: struct `AppLinkView` is never constructed
  - line 134: multiple associated items are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/bottom_pane/chat_composer.rs`
  - line 230: struct `ComposerDraftSnapshot` is never constructed
  - line 1109: methods `draft_snapshot`, `mention_bindings`, and `set_active_agent_label` are never used
  - line 3091: method `set_active_agent_label` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/bottom_pane/mcp_server_elicitation.rs`
  - line 155: method `request_id` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/bottom_pane/mod.rs`
  - line 672: methods `composer_draft_snapshot`, `replace_selection_view_if_active`, `active_tab_id_for_active_view`, `dismiss_view_by_id`, `set_pending_thread_approvals`, and `set_active_agent_label` are never used
  - line 913: methods `replace_selection_view_if_active`, `active_tab_id_for_active_view`, `set_pending_thread_approvals`, `pending_thread_approvals`, and `set_active_agent_label` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/bottom_pane/pending_thread_approvals.rs`
  - line 22: method `set_threads` is never used
  - line 35: method `threads` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget.rs`
  - line 110: constant `MULTI_AGENT_ENABLE_TITLE` is never used
  - line 111: constant `MULTI_AGENT_ENABLE_YES` is never used
  - line 112: constant `MULTI_AGENT_ENABLE_NO` is never used
  - line 113: constant `MULTI_AGENT_ENABLE_NOTICE` is never used
  - line 447: fields `collab_agent_metadata`, `interrupted_turn_notice_mode`, and `last_non_retry_error` are never read
  - line 447: multiple fields are never read
  - line 543: variant `Suppress` is never constructed
  - line 554: enum `ReplayKind` is never used
  - line 555: variant `ResumeInitialMessages` is never constructed
  - line 563: variant `SideConversation` is never constructed
  - line 573: enum `TurnAbortReason` is never used
  - line 601: multiple methods are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/command_lifecycle.rs`
  - line 63: method `on_terminal_interaction` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/exec_state.rs`
  - line 84: associated items `new` and `update_command_display` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/goal_menu.rs`
  - line 69: method `on_thread_goal_cleared` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/goal_status.rs`
  - line 16: associated function `new` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/input_flow.rs`
  - line 83: method `set_queue_submissions_until_session_configured` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/input_queue.rs`
  - line 33: method `clear` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/input_restore.rs`
  - line 27: multiple methods are never used
  - line 39: methods `set_initial_user_message_submit_suppressed` and `enqueue_rejected_steer` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/interrupts.rs`
  - line 16: variants `ExecApproval` and `RequestPermissions` are never constructed
  - line 20: variant `RequestPermissions` is never constructed
  - line 61: methods `push_exec_approval` and `push_request_permissions` are never used
  - line 79: method `push_request_permissions` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/rate_limits.rs`
  - line 131: variant `ServerOverloaded` is never constructed
  - line 131: variants `ServerOverloaded`, `UsageLimit`, and `Generic` are never constructed
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/review.rs`
  - line 8: field `pre_review_token_info` is never read
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/session_flow.rs`
  - line 133: methods `handle_thread_session` and `handle_side_thread_session` are never used
  - line 152: method `handle_side_thread_session` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/settings.rs`
  - line 184: methods `update_account_state` and `on_thread_goal_updated` are never used
  - line 479: method `on_thread_goal_updated` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/status_controls.rs`
  - line 65: method `set_active_agent_label` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/status_state.rs`
  - line 19: method `is_guardian_review` is never used
  - line 27: variant `WaitingForBackgroundTerminal` is never constructed
  - line 44: methods `start_or_update`, `finish`, `is_empty`, and `status_indicator_state` are never used
  - line 99: field `pending_guardian_review_status` is never read
  - line 122: methods `take_retry_status_header` and `remember_retry_status_header` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/streaming.rs`
  - line 84: multiple methods are never used
  - line 210: method `on_reasoning_section_break` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/tests/helpers.rs`
  - line 386: function `handle_error` is never used
  - line 418: function `replay_entered_review_mode` is never used
  - line 716: function `expire_quiet_hook_linger` is never used
  - line 730: function `reveal_running_hooks_after_delayed_redraw` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/tests/status_and_layout.rs`
  - line 1304: function `assert_goal_paused_event` is never used
  - line 2410: function `hook_live_and_history_snapshot` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/tool_interface_startup.rs`
  - line 10: variants `Starting`, `Ready`, `Failed`, and `Cancelled` are never constructed
  - line 17: methods `update_tool_interface_startup_status`, `set_tool_interface_startup_expected_endpoints`, `finish_tool_interface_startup`, `finish_tool_interface_startup_after_lag`, and `on_tool_interface_startup_update` are never used
  - line 149: methods `set_tool_interface_startup_expected_endpoints` and `finish_tool_interface_startup_after_lag` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/tool_lifecycle.rs`
  - line 9: multiple methods are never used
  - line 24: methods `on_image_generation_begin`, `on_web_search_begin`, `on_web_search_item`, and `on_web_search_end` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/tool_requests.rs`
  - line 18: methods `on_exec_approval_request`, `on_guardian_assessment`, `on_request_permissions`, `push_approval_request`, and `push_mcp_server_elicitation_request` are never used
  - line 276: methods `on_request_permissions`, `push_approval_request`, and `push_mcp_server_elicitation_request` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/turn_lifecycle.rs`
  - line 38: methods `restore_running`, `mark_budget_limited`, and `take_budget_limited` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/turn_runtime.rs`
  - line 273: methods `on_server_overloaded_error` and `on_cyber_policy_error` are never used
  - line 364: methods `on_plan_update` and `interrupted_turn_message` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/usage.rs`
  - line 304: method `clear_pending_rate_limit_reset_requests` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/chatwidget/user_messages.rs`
  - line 98: method `has_content` is never used
  - line 109: struct `ThreadInputState` is never constructed
  - line 387: function `remap_colliding_paste_placeholders` is never used
  - line 486: function `merge_user_messages` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/cwd_prompt.rs`
  - line 28: variants `Resume` and `Fork` are never constructed
  - line 55: enum `CwdPromptOutcome` is never used
  - line 61: methods `next` and `prev` are never used
  - line 76: function `run_cwd_selection_prompt` is never used
  - line 121: fields `request_frame`, `selection`, and `should_exit` are never read
  - line 131: associated items `new`, `handle_key`, `set_highlight`, `select`, `is_done`, and `selection` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/embedded_registry_read.rs`
  - line 132: method `with_account` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/embedded_spine.rs`
  - line 25: field `admitted_seq` is never read
  - line 31: variant `Materialize` is never constructed
- `registry/interface/world_terminal/packages/octopus-tui/src/git_action_directives.rs`
  - line 30: method `created_branch_cwd` is never used
  - line 45: method `last_created_branch_cwd` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/history_cell/approvals.rs`
  - line 33: variant `TimedOut` is never constructed
  - line 267: variant `Guardian` is never constructed
  - line 279: function `new_guardian_denied_patch_request` is never used
  - line 301: function `new_guardian_denied_action_request` is never used
  - line 311: function `new_guardian_approved_action_request` is never used
  - line 321: function `new_guardian_timed_out_patch_request` is never used
  - line 343: function `new_guardian_timed_out_action_request` is never used
  - line 353: function `new_review_status_line` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/history_cell/base.rs`
  - line 25: struct `WebHyperlinkHistoryCell` is never constructed
  - line 30: associated function `new` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/history_cell/hook_cell.rs`
  - line 209: methods `expire_quiet_runs_now_for_test` and `reveal_running_runs_after_delayed_redraw_for_test` are never used
  - line 306: methods `expire_quiet_linger_now_for_test` and `reveal_running_after_delayed_redraw_for_test` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/history_cell/mod.rs`
  - line 80: constant `RAW_DIFF_SUMMARY_WIDTH` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/history_cell/notices.rs`
  - line 71: struct `DeprecationNoticeCell` is never constructed
  - line 76: function `new_deprecation_notice` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/history_cell/patches.rs`
  - line 4: struct `PatchHistoryCell` is never constructed
  - line 22: function `new_patch_event` is never used
  - line 32: function `new_patch_apply_failure` is never used
  - line 57: function `new_view_image_tool_call` is never used
  - line 68: function `new_image_generation_call` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/history_cell/plans.rs`
  - line 39: function `new_plan_update` is never used
  - line 134: struct `PlanUpdateCell` is never constructed
- `registry/interface/world_terminal/packages/octopus-tui/src/history_cell/search.rs`
  - line 3: function `web_search_header` is never used
  - line 11: function `web_search_action_detail` is never used
  - line 34: function `web_search_detail` is never used
  - line 44: struct `WebSearchCell` is never constructed
  - line 45: field `call_id` is never read
  - line 54: associated items `new`, `call_id`, `update`, and `complete` are never used
  - line 70: methods `call_id` and `update` are never used
  - line 119: function `new_active_web_search_call` is never used
  - line 127: function `new_web_search_call` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/history_cell/session.rs`
  - line 67: function `padded_emoji` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/lib.rs`
  - line 390: function `config_cwd_for_app_server_target` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/onboarding/auth.rs`
  - line 54: variant `ChatGptSuccessMessage` is never constructed
- `registry/interface/world_terminal/packages/octopus-tui/src/onboarding/onboarding_screen.rs`
  - line 215: method `auth_widget_mut` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/pager_overlay.rs`
  - line 851: method `committed_cell_count` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/resume_picker.rs`
  - line 39: field `record_path` is never read
- `registry/interface/world_terminal/packages/octopus-tui/src/session_state.rs`
  - line 41: method `set_cwd_retargeting_implicit_runtime_workspace_root` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/status/account.rs`
  - line 7: variant `ApiKey` is never constructed
- `registry/interface/world_terminal/packages/octopus-tui/src/status/helpers.rs`
  - line 97: function `plan_type_display_name` is never used
  - line 182: function `title_case` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/streaming/controller.rs`
  - line 426: associated items `new` and `push` are never used
- `registry/interface/world_terminal/packages/octopus-tui/src/test_support.rs`
  - line 175: function `session_source_cli` is never used
  - line 182: function `skill_scope_user` is never used
  - line 189: function `skill_scope_repo` is never used
  - line 196: function `from_app_server_wire` is never used
- `registry/interface/world_terminal/packages/octopus-tui/src/turn_items.rs`
  - line 6: enum `StepStatus` is never used
  - line 14: struct `PlanItemArg` is never constructed
  - line 21: struct `UpdatePlanArgs` is never constructed
  - line 29: enum `MessagePhase` is never used
  - line 37: enum `AgentMessageContent` is never used
  - line 42: struct `AgentMessageItem` is never constructed

## octopus-verifiers

- `crates/octopus-verifiers/src/process_authority.rs`
  - line 20: multiple fields are never read
  - line 23: multiple fields are never read
