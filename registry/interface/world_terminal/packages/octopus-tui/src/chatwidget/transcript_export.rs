use super::*;
use crate::app_event::TranscriptExportDestination;

impl ChatWidget {
    pub(crate) fn copy_transcript_to_clipboard(&mut self, markdown: &str) {
        match crate::clipboard_copy::copy_to_clipboard(markdown) {
            Ok(lease) => {
                self.clipboard_lease = lease;
                self.add_info_message(
                    "Copied complete conversation to clipboard.".to_string(),
                    None,
                );
            }
            Err(error) => self.add_error_message(format!("Transcript copy failed: {error}")),
        }
    }

    pub(crate) fn show_transcript_export_popup(&mut self) {
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Export conversation".to_string()),
            subtitle: Some("Hydrate the complete run ledger and export Markdown".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items: vec![
                SelectionItem {
                    name: "Copy to clipboard".to_string(),
                    description: Some("Copy the complete Markdown transcript".to_string()),
                    is_disabled: cfg!(target_os = "android"),
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::ExportTranscript {
                            destination: TranscriptExportDestination::Clipboard,
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Save to file".to_string(),
                    description: Some("Choose a new Markdown filename".to_string()),
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::OpenTranscriptExportFilePrompt);
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        self.defer_input_until_settings_applied();
        self.request_redraw();
    }

    pub(crate) fn show_transcript_export_file_prompt(&mut self) {
        let tx = self.app_event_tx.clone();
        let filename = self.thread_id().map_or_else(
            || "octopus-conversation.md".to_string(),
            |run_id| format!("octopus-conversation-{run_id}.md"),
        );
        let view = CustomPromptView::new(
            "Save conversation".to_string(),
            "Type a new filename and press Enter".to_string(),
            filename,
            None,
            Box::new(move |filename| {
                tx.send(AppEvent::ExportTranscript {
                    destination: TranscriptExportDestination::File(PathBuf::from(filename)),
                });
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }
}
