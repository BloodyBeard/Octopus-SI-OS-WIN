use std::collections::VecDeque;

use crate::approval_events::ApplyPatchApprovalRequestEvent;
use crate::approval_events::ExecApprovalRequestEvent;
use crate::approval_events::RequestPermissionsEvent;
use octopus_world_terminal_client::ApprovalRequest as OperatorApprovalRequest;
use octopus_world_terminal_client::ElicitationRequest;
use octopus_world_terminal_client::ExecCommandItem;
use octopus_world_terminal_client::ToolCallItem;
use octopus_world_terminal_client::UserInputRequest;

use super::ChatWidget;

#[derive(Debug)]
pub(crate) enum QueuedInterrupt {
    ExecApproval(ExecApprovalRequestEvent),
    OperatorApproval(OperatorApprovalRequest),
    ApplyPatchApproval(ApplyPatchApprovalRequestEvent),
    Elicitation(ElicitationRequest),
    RequestPermissions(RequestPermissionsEvent),
    RequestUserInput(UserInputRequest),
    ExecStarted(ExecCommandItem),
    ExecCompleted(ExecCommandItem),
    ToolActivity {
        artifact_label: String,
        item: ToolCallItem,
    },
}

#[derive(Default)]
pub(crate) struct InterruptManager {
    queue: VecDeque<QueuedInterrupt>,
}

impl InterruptManager {
    pub(crate) fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub(crate) fn has_pending_prompt(&self) -> bool {
        self.queue.iter().any(|queued| {
            matches!(
                queued,
                QueuedInterrupt::ExecApproval(_)
                    | QueuedInterrupt::OperatorApproval(_)
                    | QueuedInterrupt::ApplyPatchApproval(_)
                    | QueuedInterrupt::Elicitation(_)
                    | QueuedInterrupt::RequestPermissions(_)
                    | QueuedInterrupt::RequestUserInput(_)
            )
        })
    }

    pub(crate) fn push_exec_approval(&mut self, ev: ExecApprovalRequestEvent) {
        self.queue.push_back(QueuedInterrupt::ExecApproval(ev));
    }

    pub(crate) fn push_operator_approval(&mut self, request: OperatorApprovalRequest) {
        self.queue
            .push_back(QueuedInterrupt::OperatorApproval(request));
    }

    pub(crate) fn push_apply_patch_approval(&mut self, ev: ApplyPatchApprovalRequestEvent) {
        self.queue
            .push_back(QueuedInterrupt::ApplyPatchApproval(ev));
    }

    pub(crate) fn push_elicitation(&mut self, request: ElicitationRequest) {
        self.queue.push_back(QueuedInterrupt::Elicitation(request));
    }

    pub(crate) fn push_request_permissions(&mut self, ev: RequestPermissionsEvent) {
        self.queue
            .push_back(QueuedInterrupt::RequestPermissions(ev));
    }

    pub(crate) fn push_user_input(&mut self, request: UserInputRequest) {
        self.queue
            .push_back(QueuedInterrupt::RequestUserInput(request));
    }

    pub(crate) fn push_exec_started(&mut self, item: ExecCommandItem) {
        self.queue.push_back(QueuedInterrupt::ExecStarted(item));
    }

    pub(crate) fn push_exec_completed(&mut self, item: ExecCommandItem) {
        self.queue.push_back(QueuedInterrupt::ExecCompleted(item));
    }

    pub(crate) fn push_tool_activity(&mut self, artifact_label: String, item: ToolCallItem) {
        self.queue.push_back(QueuedInterrupt::ToolActivity {
            artifact_label,
            item,
        });
    }

    pub(crate) fn remove_operator_request(&mut self, request_id: &str) -> bool {
        if !self.has_pending_prompt() {
            return false;
        }
        let len = self.queue.len();
        self.queue.retain(|queued| match queued {
            QueuedInterrupt::ExecApproval(event) => event.request_id != request_id,
            QueuedInterrupt::OperatorApproval(request) => request.request_id != request_id,
            QueuedInterrupt::ApplyPatchApproval(event) => event.request_id != request_id,
            QueuedInterrupt::Elicitation(request) => request.request_id != request_id,
            QueuedInterrupt::RequestPermissions(event) => event.call_id != request_id,
            QueuedInterrupt::RequestUserInput(request) => request.request_id != request_id,
            QueuedInterrupt::ExecStarted(_)
            | QueuedInterrupt::ExecCompleted(_)
            | QueuedInterrupt::ToolActivity { .. } => true,
        });
        self.queue.len() != len
    }

    pub(crate) fn flush_all(&mut self, chat: &mut ChatWidget) {
        while let Some(q) = self.queue.pop_front() {
            match q {
                QueuedInterrupt::ExecApproval(ev) => chat.handle_exec_approval_now(ev),
                QueuedInterrupt::OperatorApproval(request) => {
                    chat.handle_operator_approval_now(request);
                }
                QueuedInterrupt::ApplyPatchApproval(ev) => chat.handle_apply_patch_approval_now(ev),
                QueuedInterrupt::Elicitation(request) => {
                    chat.handle_elicitation_request_now(request);
                }
                QueuedInterrupt::RequestPermissions(ev) => chat.handle_request_permissions_now(ev),
                QueuedInterrupt::RequestUserInput(request) => {
                    chat.handle_request_user_input_now(request);
                }
                QueuedInterrupt::ExecStarted(item) => {
                    chat.handle_command_execution_started_now(item);
                }
                QueuedInterrupt::ExecCompleted(item) => {
                    chat.handle_command_execution_completed_now(item);
                }
                QueuedInterrupt::ToolActivity {
                    artifact_label,
                    item,
                } => chat.handle_tool_call_artifact_now(artifact_label, item),
            }
        }
    }
}
