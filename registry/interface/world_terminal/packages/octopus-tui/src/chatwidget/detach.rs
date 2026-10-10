use super::*;

impl ChatWidget {
    pub(crate) fn show_daemon_detach_popup(&mut self, run_id: &str) {
        let detached_run_id = run_id.to_string();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Leave while work continues?".to_string()),
            subtitle: Some(format!(
                "{run_id} is attached to a durable daemon. Detaching closes only this terminal client."
            )),
            footer_hint: Some(standard_popup_hint_line()),
            items: vec![
                SelectionItem {
                    name: "Stay connected".to_string(),
                    description: Some("Return to the current run".to_string()),
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Detach and exit".to_string(),
                    description: Some(
                        "Keep daemon work running and print a reconnect command".to_string(),
                    ),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::Exit(ExitMode::Detach {
                            run_id: detached_run_id.clone(),
                        }));
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        self.request_redraw();
    }
}
