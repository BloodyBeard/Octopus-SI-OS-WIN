use super::App;
use crate::app_server_session::AppServerSession;

impl App {
    pub(super) fn handle_embedded_reply_lane(
        &mut self,
        app_server_client: &AppServerSession,
        result: Result<
            (crate::embedded_spine::ReplyRenderLane, Option<String>),
            octopus_world_terminal_client::TerminalOutboundError,
        >,
    ) {
        use crate::embedded_spine::ReplyRenderLane;
        match result {
            Ok((lane, inline_text)) => {
                tracing::debug!(
                    method = lane.server_notification_method(),
                    "terminal outbound reader lane resolved from the durable ledger"
                );
                let text = match inline_text {
                    Some(text) => Some(text),
                    None => match &lane {
                        ReplyRenderLane::MessageDelta { content, .. }
                        | ReplyRenderLane::ExecOutputDelta { content, .. } => app_server_client
                            .embedded_reply_blob(content)
                            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
                        ReplyRenderLane::TurnStarted
                        | ReplyRenderLane::MarkOutcomeItem { .. }
                        | ReplyRenderLane::Approval { .. }
                        | ReplyRenderLane::Elicitation { .. }
                        | ReplyRenderLane::UserInputRequest { .. }
                        | ReplyRenderLane::OperatorRequestResolved { .. }
                        | ReplyRenderLane::HookResult { .. }
                        | ReplyRenderLane::ToolCall { .. }
                        | ReplyRenderLane::HookCatalog { .. }
                        | ReplyRenderLane::TurnCompleted { .. }
                        | ReplyRenderLane::Reasoning { .. }
                        | ReplyRenderLane::PlanUpdate { .. }
                        | ReplyRenderLane::RichExec { .. }
                        | ReplyRenderLane::TokenUsage { .. }
                        | ReplyRenderLane::UserMessage { .. }
                        | ReplyRenderLane::ProviderError { .. }
                        | ReplyRenderLane::RecordedArtifact { .. }
                        | ReplyRenderLane::WorkerStarted { .. }
                        | ReplyRenderLane::WorkerToolArtifact { .. }
                        | ReplyRenderLane::WorkerMark { .. }
                        | ReplyRenderLane::WorkerCompleted { .. } => None,
                    },
                };
                self.chat_widget
                    .apply_embedded_reply_lane(&lane, text.as_deref());
            }
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("terminal outbound read failed: {err}"));
            }
        }
    }
}
