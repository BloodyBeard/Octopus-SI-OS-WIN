pub(super) use super::*;
pub(super) use crate::app_command::AppCommand as Op;
pub(super) use crate::app_command::ReviewTargetSpec as ReviewTarget;
pub(super) use crate::app_event::AppEvent;
pub(super) use crate::app_event::ExitMode;
pub(super) use crate::app_event_sender::AppEventSender;
pub(super) use crate::approval_events::ApplyPatchApprovalRequestEvent;
pub(super) use crate::approval_events::ExecApprovalRequestEvent;
pub(super) use crate::approval_events::RequestPermissionProfile;
pub(super) use crate::approval_presets::builtin_approval_presets;
pub(super) use crate::bottom_pane::LocalImageAttachment;
pub(super) use crate::bottom_pane::MentionBinding;
pub(super) use crate::bottom_pane::QueuedInputAction;
pub(super) use crate::diff_model::FileChange;
pub(super) use crate::guardian_events::GuardianAssessmentAction;
pub(super) use crate::guardian_events::GuardianAssessmentDecisionSource;
pub(super) use crate::guardian_events::GuardianAssessmentEvent;
pub(super) use crate::guardian_events::GuardianAssessmentStatus;
pub(super) use crate::guardian_events::GuardianCommandSource;
pub(super) use crate::guardian_events::GuardianRiskLevel;
pub(super) use crate::guardian_events::GuardianUserAuthorization;
pub(super) use crate::history_cell::UserHistoryCell;
pub(super) use crate::model_catalog::ModelCatalog;
pub(super) use crate::plan_type::PlanType;
pub(super) use crate::provider_config_runtime::WindowsSandboxRequirementPosture;
pub(super) use crate::session_posture::ConstrainedValue;
pub(super) use crate::session_posture::SessionPermissionSnapshot;
pub(super) use crate::session_posture::WindowsSandboxMode;
pub(super) use crate::session_telemetry::RuntimeMetricsSummary;
pub(super) use crate::session_telemetry::SessionTelemetry;
pub(super) use crate::session_vocab::config_types::ApprovalsReviewer;
pub(super) use crate::session_vocab::config_types::CollaborationMode;
pub(super) use crate::session_vocab::config_types::ModeKind;
pub(super) use crate::session_vocab::config_types::Personality;
pub(super) use crate::session_vocab::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
pub(super) use crate::session_vocab::config_types::ServiceTier;
pub(super) use crate::session_vocab::openai_models::ModelInfo;
pub(super) use crate::session_vocab::openai_models::ModelPreset;
pub(super) use crate::session_vocab::openai_models::ModelsResponse;
pub(super) use crate::session_vocab::openai_models::ReasoningEffortPreset;
pub(super) use crate::session_vocab::openai_models::default_input_modalities;
pub(super) use crate::test_backend::VT100Backend;
pub(super) use crate::test_support::PathBufExt;
pub(super) use crate::test_support::construct_model_info_offline_for_tests;
pub(super) use crate::test_support::get_model_offline_for_tests;
pub(super) use crate::test_support::test_path_buf;
pub(super) use crate::test_support::test_path_display;
pub(super) use crate::test_support::write_test_model_provider_config;
pub(super) use crate::token_usage::TokenUsage;
pub(super) use crate::token_usage::TokenUsageInfo;
pub(super) use crate::tui::FrameRequester;
pub(super) use crate::turn_items::MessagePhase;
pub(super) use crate::turn_items::PlanItemArg;
pub(super) use crate::turn_items::StepStatus;
pub(super) use crate::turn_items::UpdatePlanArgs;
pub(super) use assert_matches::assert_matches;
pub(super) use crossterm::event::KeyCode;
pub(super) use crossterm::event::KeyEvent;
pub(super) use crossterm::event::KeyModifiers;
pub(super) use insta::assert_snapshot;
pub(super) use octopus_git_utils::CommitLogEntry;
pub(super) use octopus_substrate_contract::parse_command::ParsedCommand;
pub(super) use octopus_substrate_contract::permission_profile::ActivePermissionProfile;
pub(super) use octopus_substrate_contract::permission_profile::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;
pub(super) use octopus_substrate_contract::permission_profile::FileSystemPermissions;
pub(super) use octopus_substrate_contract::permission_profile::NetworkPermissions;
pub(super) use octopus_substrate_contract::permission_profile::PermissionProfile;
pub(super) use octopus_terminal_detection::Multiplexer;
pub(super) use octopus_terminal_detection::TerminalInfo;
pub(super) use octopus_terminal_detection::TerminalName;
pub(super) use octopus_util_abspath::AbsolutePathBuf;
pub(super) use octopus_util_pathuri::LegacyAppPathString;
pub(super) use octopus_world_terminal_client::AccountUsageReport;
pub(super) use octopus_world_terminal_client::CommandActionItem;
pub(super) use octopus_world_terminal_client::CreditsItem;
pub(super) use octopus_world_terminal_client::ExecCommandItem;
pub(super) use octopus_world_terminal_client::ExecItemSource as ExecCommandSource;
pub(super) use octopus_world_terminal_client::ExecItemStatus;
pub(super) use octopus_world_terminal_client::GoalState;
pub(super) use octopus_world_terminal_client::GoalStatusItem;
pub(super) use octopus_world_terminal_client::HookItemStatus;
pub(super) use octopus_world_terminal_client::HookOutputItem;
pub(super) use octopus_world_terminal_client::HookOutputKind;
pub(super) use octopus_world_terminal_client::HookRunItem;
pub(super) use octopus_world_terminal_client::RateLimitReachedKind;
pub(super) use octopus_world_terminal_client::RateLimitSnapshotItem;
pub(super) use octopus_world_terminal_client::RateLimitWindowItem;
pub(super) use octopus_world_terminal_client::TerminalUserInput as UserInput;
pub(super) use octopus_world_terminal_client::TextElement;
pub(super) use octopus_world_terminal_config::Notifications;
pub(super) use serde_json::json;
#[cfg(target_os = "windows")]
pub(super) use serial_test::serial;
pub(super) use std::collections::HashMap;
pub(super) use std::path::PathBuf;
pub(super) use tempfile::NamedTempFile;
pub(super) use tempfile::tempdir;
pub(super) use tokio::sync::mpsc::error::TryRecvError;
pub(super) use tokio::sync::mpsc::unbounded_channel;
pub(super) use toml::Value as TomlValue;

pub(super) fn chatwidget_snapshot_dir() -> PathBuf {
    let snapshot_file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/chatwidget/snapshots/codex_tui__chatwidget__tests__chatwidget_tall.snap");
    snapshot_file
        .parent()
        .unwrap_or_else(|| panic!("snapshot file has no parent: {}", snapshot_file.display()))
        .to_path_buf()
}

macro_rules! assert_chatwidget_snapshot {
    ($name:expr, $value:expr $(,)?) => {{
        let mut settings = insta::Settings::clone_current();
        settings.set_prepend_module_to_snapshot(false);
        settings.set_snapshot_path(crate::chatwidget::tests::chatwidget_snapshot_dir());
        settings.bind(|| {
            insta::assert_snapshot!(format!("codex_tui__chatwidget__tests__{}", $name), $value);
        });
    }};
    ($name:expr, $value:expr, @$snapshot:literal $(,)?) => {{
        let mut settings = insta::Settings::clone_current();
        settings.set_prepend_module_to_snapshot(false);
        settings.set_snapshot_path(crate::chatwidget::tests::chatwidget_snapshot_dir());
        settings.bind(|| {
            insta::assert_snapshot!(
                format!("codex_tui__chatwidget__tests__{}", $name),
                &($value),
                @$snapshot
            );
        });
    }};
}

fn next_goal_draft(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    expected_thread_id: String,
) -> crate::goal_files::GoalDraft {
    loop {
        let event = rx.try_recv().expect("expected goal draft event");
        if let AppEvent::SetThreadGoalDraft {
            thread_id, draft, ..
        } = event
        {
            assert_eq!(thread_id, expected_thread_id);
            return draft;
        }
    }
}

mod approval_requests;
mod composer_submission;
#[path = "tests/config_errors_tests.rs"]
mod config_errors;
mod exec_flow;
mod goal_menu;
mod goal_validation;
mod guardian;
pub(crate) mod helpers;
mod history_replay;
mod permissions;
mod plan_mode;
mod popups_and_settings;
mod review_mode;
mod side;
mod slash_commands;
mod status_and_layout;
mod status_command_tests;
mod status_surface_previews;
mod terminal_title;
mod tool_interface_startup;
mod usage;

pub(crate) use helpers::make_chatwidget_manual_with_sender;
pub(crate) use helpers::set_chatgpt_auth;
pub(crate) use helpers::set_fast_mode_test_catalog;
pub(super) use helpers::*;
