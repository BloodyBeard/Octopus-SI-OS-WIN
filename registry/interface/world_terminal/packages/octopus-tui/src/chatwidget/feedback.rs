use super::*;
use octopus_world_terminal_support::AttachmentConsent;
use octopus_world_terminal_support::FeedbackCategory;

impl ChatWidget {
    pub(crate) fn show_feedback_category_popup(&mut self) {
        let categories = [
            (
                FeedbackCategory::Bug,
                "Bug",
                "Something behaved incorrectly",
            ),
            (
                FeedbackCategory::BadResult,
                "Bad result",
                "The result was not useful or correct",
            ),
            (
                FeedbackCategory::GoodResult,
                "Good result",
                "A result or interaction worked especially well",
            ),
            (
                FeedbackCategory::SafetyConcern,
                "Safety concern",
                "A permission, privacy, or safety concern",
            ),
            (
                FeedbackCategory::Other,
                "Other",
                "Anything else about the terminal client",
            ),
        ];
        let items = categories
            .into_iter()
            .map(|(category, name, description)| SelectionItem {
                name: name.to_string(),
                description: Some(description.to_string()),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenFeedbackPrompt { category });
                })],
                dismiss_on_select: true,
                ..Default::default()
            })
            .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Local feedback".to_string()),
            subtitle: Some("Choose a category. Nothing will be uploaded.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
        self.defer_input_until_settings_applied();
        self.request_redraw();
    }

    pub(crate) fn show_feedback_prompt(&mut self, category: FeedbackCategory) {
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            format!("Feedback · {}", category.as_str()),
            "Describe what happened and press Enter".to_string(),
            String::new(),
            Some("Your note is stored only in the local bundle".to_string()),
            Box::new(move |note| {
                tx.send(AppEvent::OpenFeedbackConsent { category, note });
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    pub(crate) fn show_feedback_consent_popup(&mut self, category: FeedbackCategory, note: String) {
        let choices = [
            (
                "Note only",
                "Attach nothing else",
                AttachmentConsent::default(),
            ),
            (
                "Attach diagnostics",
                "Attach bounded path states and environment-variable presence",
                AttachmentConsent {
                    diagnostics: true,
                    logs: false,
                    transcript: false,
                },
            ),
            (
                "Attach diagnostics and logs",
                "Also attach a bounded, redacted terminal log tail",
                AttachmentConsent {
                    diagnostics: true,
                    logs: true,
                    transcript: false,
                },
            ),
            (
                "Attach diagnostics, logs, and transcript",
                "Also attach a bounded, redacted transcript tail",
                AttachmentConsent {
                    diagnostics: true,
                    logs: true,
                    transcript: true,
                },
            ),
        ];
        let items = choices
            .into_iter()
            .map(|(name, description, consent)| {
                let note = note.clone();
                SelectionItem {
                    name: name.to_string(),
                    description: Some(description.to_string()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::SubmitFeedback {
                            category,
                            note: note.clone(),
                            consent: consent.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Choose support attachments".to_string()),
            subtitle: Some(
                "Attachments are bounded and redacted. The bundle stays local; no upload is attempted."
                    .to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
        self.request_redraw();
    }
}
