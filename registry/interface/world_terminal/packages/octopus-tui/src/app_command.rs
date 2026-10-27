use std::path::PathBuf;

use crate::guardian_events::GuardianAssessmentEvent;
use crate::session_vocab::config_types::ApprovalsReviewer;
use crate::session_vocab::config_types::CollaborationMode;
use crate::session_vocab::config_types::Personality;
use crate::session_vocab::config_types::ReasoningSummary as ReasoningSummaryConfig;
use crate::session_vocab::openai_models::ReasoningEffort as ReasoningEffortConfig;
use octopus_substrate_contract::approvals_substrate::AskForApproval;
use octopus_substrate_contract::permission_profile::ActivePermissionProfile;
use octopus_substrate_contract::permission_profile::PermissionProfile;
use octopus_substrate_contract::shell_env_config::WindowsSandboxLevel;
use octopus_world_terminal_client::TerminalUserInput;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum ReviewTargetSpec {
    UncommittedChanges,
    #[serde(rename_all = "camelCase")]
    BaseBranch {
        branch: String,
    },
    #[serde(rename_all = "camelCase")]
    Commit {
        sha: String,
        title: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Custom {
        instructions: String,
    },
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) enum AppCommand {
    Interrupt {
        behavior: InterruptBehavior,
    },
    CleanBackgroundTerminals,
    RunUserShellCommand {
        command: String,
    },
    UserTurn {
        items: Vec<TerminalUserInput>,
        cwd: PathBuf,
        approval_policy: AskForApproval,
        approvals_reviewer: Option<ApprovalsReviewer>,
        active_permission_profile: Option<ActivePermissionProfile>,
        model: String,
        effort: Option<ReasoningEffortConfig>,
        summary: Option<ReasoningSummaryConfig>,
        service_tier: Option<Option<String>>,
        final_output_json_schema: Option<Value>,
        collaboration_mode: Option<CollaborationMode>,
        personality: Option<Personality>,
    },
    OverrideTurnContext {
        cwd: Option<PathBuf>,
        approval_policy: Option<AskForApproval>,
        approvals_reviewer: Option<ApprovalsReviewer>,
        permission_profile: Option<PermissionProfile>,
        active_permission_profile: Option<ActivePermissionProfile>,
        windows_sandbox_level: Option<WindowsSandboxLevel>,
        model: Option<String>,
        effort: Option<Option<ReasoningEffortConfig>>,
        summary: Option<ReasoningSummaryConfig>,
        service_tier: Option<Option<String>>,
        collaboration_mode: Option<CollaborationMode>,
        personality: Option<Personality>,
    },
    ApprovalDecision {
        request_id: String,
        decision: Value,
    },
    ElicitationAnswer {
        request_id: String,
        answer: Value,
    },
    UserInputAnswer {
        request_id: String,
        answers: std::collections::BTreeMap<String, Vec<String>>,
    },
    Compact,
    SetThreadName {
        name: String,
    },
    ThreadRollback {
        num_turns: u32,
    },
    Review {
        target: ReviewTargetSpec,
    },
    ApproveGuardianDeniedAction {
        event: GuardianAssessmentEvent,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum InterruptBehavior {
    Default,
    RestorePromptIfNoOutput,
}

impl AppCommand {
    pub(crate) fn interrupt() -> Self {
        Self::Interrupt {
            behavior: InterruptBehavior::Default,
        }
    }

    pub(crate) fn interrupt_and_restore_prompt_if_no_output() -> Self {
        Self::Interrupt {
            behavior: InterruptBehavior::RestorePromptIfNoOutput,
        }
    }

    pub(crate) fn clean_background_terminals() -> Self {
        Self::CleanBackgroundTerminals
    }

    pub(crate) fn run_user_shell_command(command: String) -> Self {
        Self::RunUserShellCommand { command }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn user_turn(
        items: Vec<TerminalUserInput>,
        cwd: PathBuf,
        approval_policy: AskForApproval,
        active_permission_profile: Option<ActivePermissionProfile>,
        model: String,
        effort: Option<ReasoningEffortConfig>,
        summary: Option<ReasoningSummaryConfig>,
        service_tier: Option<Option<String>>,
        final_output_json_schema: Option<Value>,
        collaboration_mode: Option<CollaborationMode>,
        personality: Option<Personality>,
    ) -> Self {
        Self::UserTurn {
            items,
            cwd,
            approval_policy,
            approvals_reviewer: None,
            active_permission_profile,
            model,
            effort,
            summary,
            service_tier,
            final_output_json_schema,
            collaboration_mode,
            personality,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn override_turn_context(
        cwd: Option<PathBuf>,
        approval_policy: Option<AskForApproval>,
        approvals_reviewer: Option<ApprovalsReviewer>,
        permission_profile: Option<PermissionProfile>,
        active_permission_profile: Option<ActivePermissionProfile>,
        windows_sandbox_level: Option<WindowsSandboxLevel>,
        model: Option<String>,
        effort: Option<Option<ReasoningEffortConfig>>,
        summary: Option<ReasoningSummaryConfig>,
        service_tier: Option<Option<String>>,
        collaboration_mode: Option<CollaborationMode>,
        personality: Option<Personality>,
    ) -> Self {
        Self::OverrideTurnContext {
            cwd,
            approval_policy,
            approvals_reviewer,
            permission_profile,
            active_permission_profile,
            windows_sandbox_level,
            model,
            effort,
            summary,
            service_tier,
            collaboration_mode,
            personality,
        }
    }

    pub(crate) fn approval_decision(request_id: String, decision: Value) -> Self {
        Self::ApprovalDecision {
            request_id,
            decision,
        }
    }

    pub(crate) fn elicitation_answer(request_id: String, answer: Value) -> Self {
        Self::ElicitationAnswer { request_id, answer }
    }

    pub(crate) fn user_input_answer(
        request_id: String,
        answers: std::collections::BTreeMap<String, Vec<String>>,
    ) -> Self {
        Self::UserInputAnswer {
            request_id,
            answers,
        }
    }

    pub(crate) fn compact() -> Self {
        Self::Compact
    }

    pub(crate) fn set_thread_name(name: String) -> Self {
        Self::SetThreadName { name }
    }

    pub(crate) fn thread_rollback(num_turns: u32) -> Self {
        Self::ThreadRollback { num_turns }
    }

    pub(crate) fn review(target: ReviewTargetSpec) -> Self {
        Self::Review { target }
    }

    pub(crate) fn approve_guardian_denied_action(event: GuardianAssessmentEvent) -> Self {
        Self::ApproveGuardianDeniedAction { event }
    }

    pub(crate) fn is_review(&self) -> bool {
        matches!(self, Self::Review { .. })
    }
}

impl From<&AppCommand> for AppCommand {
    fn from(value: &AppCommand) -> Self {
        value.clone()
    }
}
