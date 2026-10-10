use super::*;
use octopus_world_terminal_client::TranscriptEntry;
use octopus_world_terminal_client::TranscriptSide;
use std::collections::BTreeSet;

impl ChatWidget {
    pub(crate) fn replay_transcript_entries(&mut self, entries: &[TranscriptEntry]) {
        let resolved_requests = entries
            .iter()
            .filter(|entry| entry.side == TranscriptSide::Outbound)
            .filter_map(|entry| {
                let (_, resolved) = octopus_world_terminal_client::resolve_outbound_payload(
                    &entry.label,
                    entry.text.as_bytes(),
                );
                match resolved {
                    octopus_world_terminal_client::OutboundResolved::OperatorRequestResolved(
                        request,
                    ) => Some(request.request_id),
                    _ => None,
                }
            })
            .collect::<BTreeSet<_>>();
        let mut delta_index = 0usize;
        for entry in entries {
            match entry.side {
                TranscriptSide::Inbound => {
                    if !entry.text.trim().is_empty() {
                        let items = [octopus_world_terminal_client::TerminalUserInput::text(
                            entry.text.clone(),
                        )];
                        self.on_committed_user_message(&items, true);
                    }
                }
                TranscriptSide::Outbound => {
                    let (kind, resolved) = octopus_world_terminal_client::resolve_outbound_payload(
                        &entry.label,
                        entry.text.as_bytes(),
                    );
                    let Some((lane, lane_text)) = crate::embedded_spine::terminal_outbound_lane(
                        kind,
                        resolved,
                        entry.label.clone(),
                        None,
                        &mut delta_index,
                    ) else {
                        continue;
                    };
                    let request_id = match &lane {
                        crate::embedded_spine::ReplyRenderLane::Approval { request, .. } => {
                            Some(request.request_id.as_str())
                        }
                        crate::embedded_spine::ReplyRenderLane::Elicitation { request, .. } => {
                            Some(request.request_id.as_str())
                        }
                        crate::embedded_spine::ReplyRenderLane::UserInputRequest {
                            request,
                            ..
                        } => Some(request.request_id.as_str()),
                        _ => None,
                    };
                    if request_id.is_some_and(|id| resolved_requests.contains(id)) {
                        continue;
                    }
                    self.apply_embedded_reply_lane(&lane, lane_text.as_deref());
                }
            }
        }
        self.flush_answer_stream_with_separator();
        self.request_redraw();
    }
}
