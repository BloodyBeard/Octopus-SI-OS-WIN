use std::time::Duration;
use std::time::Instant;

use octopus_world_terminal_support::AttachmentConsent;
use octopus_world_terminal_support::FeedbackCategory;
use octopus_world_terminal_support::LocalFeedbackRequest;
use octopus_world_terminal_support::SupportContext;
use octopus_world_terminal_support::SupportTag;

use super::App;
use crate::AppServerTarget;
use crate::app_event::AppEvent;
use crate::app_server_session::AppServerSession;

const FEEDBACK_TIMEOUT: Duration = Duration::from_secs(10);

impl App {
    pub(super) fn start_feedback_bundle(
        &mut self,
        app_server: &AppServerSession,
        category: FeedbackCategory,
        note: String,
        consent: AttachmentConsent,
    ) {
        let started = Instant::now();
        let app_event_tx = self.app_event_tx.clone();
        let active_task = self.chat_widget.is_task_running();
        let run = app_server.active_run_id();
        let interface = app_server.client_interface().ok();
        let durable_root = app_server.prompt_history_durable_root();
        let run_record_path = app_server.active_run_record_path();
        let log_path = self
            .config
            .log_dir_configured
            .then(|| self.config.log_dir.join(crate::TUI_LOG_FILE_NAME));
        let (target, control_socket_path) = match &self.app_server_target {
            AppServerTarget::Embedded => ("embedded".to_string(), None),
            AppServerTarget::LocalDaemon { endpoint } => (
                "local_daemon".to_string(),
                Some(endpoint.control_socket.clone()),
            ),
            AppServerTarget::Remote { endpoint } => (
                "remote_daemon".to_string(),
                Some(endpoint.control_socket.clone()),
            ),
        };
        let context = SupportContext {
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            target: target.clone(),
            run_id: run.as_ref().map(|run| run.as_str().to_string()),
            interface_id: interface
                .as_ref()
                .map(|interface| interface.as_str().to_string()),
            active_task,
            octopus_home: self.config.codex_home.as_path().to_path_buf(),
            durable_root: durable_root.clone(),
            run_record_path,
            log_path,
            control_socket_path,
        };
        let tags = vec![
            SupportTag::new("client", "world_terminal"),
            SupportTag::new("category", category.as_str()),
            SupportTag::new("target", target),
            SupportTag::new("version", env!("CARGO_PKG_VERSION")),
        ];
        self.chat_widget.add_info_message(
            "Preparing a private local support bundle. Nothing will be uploaded.".to_string(),
            None,
        );

        tokio::spawn(async move {
            let operation = async {
                let transcript = if consent.transcript {
                    let durable_root = durable_root.ok_or_else(|| {
                        "transcript attachment requested, but no durable ledger is attached"
                            .to_string()
                    })?;
                    let run = run.ok_or_else(|| {
                        "transcript attachment requested, but no run is active".to_string()
                    })?;
                    let interface = interface.ok_or_else(|| {
                        "transcript attachment requested, but no terminal interface is active"
                            .to_string()
                    })?;
                    Some(
                        tokio::task::spawn_blocking(move || {
                            super::transcript_export::hydrate_markdown_transcript(
                                &durable_root,
                                &run,
                                &interface,
                            )
                        })
                        .await
                        .map_err(|error| {
                            format!("transcript attachment worker failed: {error}")
                        })??,
                    )
                } else {
                    None
                };
                let remaining = FEEDBACK_TIMEOUT
                    .checked_sub(started.elapsed())
                    .ok_or_else(|| "local feedback preparation timed out".to_string())?;
                octopus_world_terminal_support::create_local_feedback_bundle(LocalFeedbackRequest {
                    category,
                    note,
                    tags,
                    consent,
                    context,
                    transcript,
                    timeout: remaining,
                })
                .await
                .map_err(|error| error.to_string())
            };
            let result = match tokio::time::timeout(FEEDBACK_TIMEOUT, operation).await {
                Ok(result) => result,
                Err(_) => Err(format!(
                    "local feedback preparation timed out after {} ms",
                    FEEDBACK_TIMEOUT.as_millis()
                )),
            };
            app_event_tx.send(AppEvent::FeedbackBundleFinished { result });
        });
    }

    pub(super) fn show_update_diagnostics(&mut self) {
        let diagnostics =
            octopus_world_terminal_support::update_diagnostics(env!("CARGO_PKG_VERSION"));
        self.chat_widget.add_info_message(
            format!(
                "Update diagnostics\n\nClient version: {}\nStatus: installer_authority_unavailable\nAction available: {}\nSigned distribution manifest: absent\nInstaller receipt authority: absent\nNetwork check performed: {}\nBackground timer running: {}\n\n{}",
                diagnostics.client_version,
                diagnostics.action_available,
                diagnostics.network_check_performed,
                diagnostics.background_timer_running,
                diagnostics.reason,
            ),
            None,
        );
    }
}
