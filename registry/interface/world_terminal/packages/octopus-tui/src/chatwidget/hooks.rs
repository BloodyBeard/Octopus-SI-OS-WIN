use std::path::PathBuf;

use super::ChatWidget;
use crate::app_event::AppEvent;
use crate::bottom_pane::HooksBrowserView;
use octopus_world_terminal_client::HookCatalog;

impl ChatWidget {
    pub(crate) fn add_hooks_output(&mut self) {
        if let Some(catalog) = self.latest_hook_catalog.clone() {
            self.open_hooks_browser(catalog);
            return;
        }
        self.app_event_tx.send(AppEvent::FetchHooksList {
            cwd: self.config.cwd.to_path_buf(),
        });
    }

    pub(super) fn on_hook_catalog(&mut self, catalog: HookCatalog) {
        self.latest_hook_catalog = Some(catalog);
    }

    pub(crate) fn on_hooks_loaded(&mut self, cwd: PathBuf, result: Result<HookCatalog, String>) {
        if self.config.cwd.as_path() != cwd.as_path() {
            return;
        }

        match result {
            Ok(catalog) => self.open_hooks_browser(catalog),
            Err(err) => self.add_error_message(format!("Failed to load hooks: {err}")),
        }
    }

    pub(crate) fn open_hooks_browser(&mut self, catalog: HookCatalog) {
        self.bottom_pane
            .show_view(Box::new(HooksBrowserView::from_catalog(
                catalog,
                self.app_event_tx.clone(),
                self.bottom_pane.list_keymap(),
            )));
        self.request_redraw();
    }
}
