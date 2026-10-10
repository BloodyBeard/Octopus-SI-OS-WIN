use crate::app_command::AppCommand;
use crate::app_command::ReviewTargetSpec;
use tokio::sync::mpsc::UnboundedSender;

use crate::app_event::AppEvent;
use crate::session_log;

#[derive(Clone, Debug)]
pub(crate) struct AppEventSender {
    pub app_event_tx: UnboundedSender<AppEvent>,
}

impl AppEventSender {
    pub(crate) fn new(app_event_tx: UnboundedSender<AppEvent>) -> Self {
        Self { app_event_tx }
    }

    pub(crate) fn send(&self, event: AppEvent) {
        if !matches!(event, AppEvent::CodexOp(_)) {
            session_log::log_inbound_app_event(&event);
        }
        if let Err(e) = self.app_event_tx.send(event) {
            tracing::error!("failed to send event: {e}");
        }
    }

    pub(crate) fn interrupt(&self) {
        self.send(AppEvent::CodexOp(AppCommand::interrupt()));
    }

    pub(crate) fn interrupt_and_restore_prompt_if_no_output(&self) {
        self.send(AppEvent::CodexOp(
            AppCommand::interrupt_and_restore_prompt_if_no_output(),
        ));
    }

    pub(crate) fn compact(&self) {
        self.send(AppEvent::CodexOp(AppCommand::compact()));
    }

    pub(crate) fn set_thread_name(&self, name: String) {
        self.send(AppEvent::CodexOp(AppCommand::set_thread_name(name)));
    }

    pub(crate) fn review(&self, target: ReviewTargetSpec) {
        self.send(AppEvent::CodexOp(AppCommand::review(target)));
    }

    pub(crate) fn user_input_answer(
        &self,
        request_id: String,
        answers: std::collections::BTreeMap<String, Vec<String>>,
    ) {
        self.send(AppEvent::CodexOp(AppCommand::user_input_answer(
            request_id, answers,
        )));
    }

    pub(crate) fn approval_decision(&self, request_id: String, decision: serde_json::Value) {
        self.send(AppEvent::CodexOp(AppCommand::approval_decision(
            request_id, decision,
        )));
    }

    pub(crate) fn elicitation_answer(&self, request_id: String, answer: serde_json::Value) {
        self.send(AppEvent::CodexOp(AppCommand::elicitation_answer(
            request_id, answer,
        )));
    }
}
