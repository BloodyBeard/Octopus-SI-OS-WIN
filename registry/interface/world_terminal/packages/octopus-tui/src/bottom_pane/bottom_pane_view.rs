use crate::bottom_pane::ApprovalRequest;
use crate::bottom_pane::McpServerElicitationFormRequest;
use crate::render::renderable::Renderable;
use crossterm::event::KeyEvent;
use octopus_world_terminal_client::UserInputRequest;
use std::time::Instant;

use super::CancellationEvent;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ViewCompletion {
    Accepted,
    Cancelled,
}

pub(crate) trait BottomPaneView: Renderable {
    fn handle_key_event(&mut self, _key_event: KeyEvent) {}

    fn is_complete(&self) -> bool {
        false
    }

    fn completion(&self) -> Option<ViewCompletion> {
        None
    }

    fn dismiss_after_child_accept(&self) -> bool {
        false
    }

    fn clear_dismiss_after_child_accept(&mut self) {}

    fn view_id(&self) -> Option<&'static str> {
        None
    }

    fn selected_index(&self) -> Option<usize> {
        None
    }

    #[allow(dead_code)]
    fn active_tab_id(&self) -> Option<&str> {
        None
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        CancellationEvent::NotHandled
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        false
    }

    fn will_interrupt_turn_on_key_event(&self, _key_event: KeyEvent) -> bool {
        false
    }

    fn handle_paste(&mut self, _pasted: String) -> bool {
        false
    }

    fn flush_paste_burst_if_due(&mut self) -> bool {
        false
    }

    fn is_in_paste_burst(&self) -> bool {
        false
    }

    fn pre_draw_tick(&mut self, _now: Instant) -> bool {
        false
    }

    fn try_consume_approval_request(
        &mut self,
        request: ApprovalRequest,
    ) -> Option<ApprovalRequest> {
        Some(request)
    }

    fn try_consume_user_input_request(
        &mut self,
        request: UserInputRequest,
    ) -> Option<UserInputRequest> {
        Some(request)
    }

    fn try_consume_mcp_server_elicitation_request(
        &mut self,
        request: McpServerElicitationFormRequest,
    ) -> Option<McpServerElicitationFormRequest> {
        Some(request)
    }

    fn dismiss_operator_request(&mut self, _request_id: &str) -> bool {
        false
    }

    fn terminal_title_requires_action(&self) -> bool {
        false
    }

    fn next_frame_delay(&self) -> Option<std::time::Duration> {
        None
    }
}
