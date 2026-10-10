use octopus_terminal_detection::terminal_info;
use octopus_world_terminal_keymap::TuiKeymap;

use super::ChatWidget;
use super::queued_message_edit_hint_binding;
use crate::app_event::KeymapEditIntent;
use crate::keymap::RuntimeKeymap;
use crate::keymap_setup;

impl ChatWidget {
    pub(crate) fn open_keymap_picker(&mut self) {
        match RuntimeKeymap::from_config(&self.config.terminal.keymap) {
            Ok(runtime_keymap) => {
                let params = keymap_setup::build_keymap_picker_params_with_filter(
                    &runtime_keymap,
                    &self.config.terminal.keymap,
                    self.keymap_action_filter(),
                );
                self.bottom_pane.show_selection_view(params);
            }
            Err(err) => {
                self.add_error_message(format!("Invalid `tui.keymap` configuration: {err}"));
            }
        }
    }

    pub(crate) fn open_keymap_action_menu(
        &mut self,
        context: String,
        action: String,
        runtime_keymap: &RuntimeKeymap,
    ) {
        let params = keymap_setup::build_keymap_action_menu_params(
            context,
            action,
            runtime_keymap,
            &self.config.terminal.keymap,
        );
        self.bottom_pane.show_selection_view(params);
    }

    pub(crate) fn open_keymap_capture(
        &mut self,
        context: String,
        action: String,
        intent: KeymapEditIntent,
        runtime_keymap: &RuntimeKeymap,
    ) {
        let view = keymap_setup::build_keymap_capture_view(
            context,
            action,
            intent,
            runtime_keymap,
            self.app_event_tx.clone(),
        );
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    pub(crate) fn open_keymap_debug(&mut self, runtime_keymap: &RuntimeKeymap) {
        let view =
            keymap_setup::build_keymap_debug_view(runtime_keymap, &self.config.terminal.keymap);
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    pub(crate) fn open_keymap_replace_binding_menu(
        &mut self,
        context: String,
        action: String,
        runtime_keymap: &RuntimeKeymap,
    ) {
        let params =
            keymap_setup::build_keymap_replace_binding_menu_params(context, action, runtime_keymap);
        self.bottom_pane.show_selection_view(params);
    }

    pub(crate) fn return_to_keymap_picker(
        &mut self,
        context: &str,
        action: &str,
        runtime_keymap: &RuntimeKeymap,
    ) {
        let params = keymap_setup::build_keymap_picker_params_for_selected_action_with_filter(
            runtime_keymap,
            &self.config.terminal.keymap,
            self.keymap_action_filter(),
            context,
            action,
        );
        let replaced = self.bottom_pane.replace_active_views_with_selection_view(
            &[
                keymap_setup::KEYMAP_PICKER_VIEW_ID,
                keymap_setup::KEYMAP_ACTION_MENU_VIEW_ID,
                keymap_setup::KEYMAP_REPLACE_BINDING_MENU_VIEW_ID,
            ],
            params,
        );
        if !replaced {
            let params = keymap_setup::build_keymap_picker_params_for_selected_action_with_filter(
                runtime_keymap,
                &self.config.terminal.keymap,
                self.keymap_action_filter(),
                context,
                action,
            );
            self.bottom_pane.show_selection_view(params);
        }
        self.request_redraw();
    }

    fn keymap_action_filter(&self) -> keymap_setup::KeymapActionFilter {
        keymap_setup::KeymapActionFilter {
            fast_mode_enabled: self.fast_mode_enabled(),
        }
    }

    pub(crate) fn apply_keymap_update(
        &mut self,
        keymap_config: TuiKeymap,
        runtime_keymap: &RuntimeKeymap,
    ) {
        self.config.terminal.keymap = keymap_config;
        self.copy_last_response_binding = runtime_keymap.app.copy.clone();
        self.chat_keymap = runtime_keymap.chat.clone();
        self.queued_message_edit_hint_binding = queued_message_edit_hint_binding(
            &self.chat_keymap.edit_queued_message,
            terminal_info(),
        );
        self.bottom_pane
            .set_queued_message_edit_binding(self.queued_message_edit_hint_binding);
        self.bottom_pane.set_keymap_bindings(runtime_keymap);
        self.request_redraw();
    }
}
