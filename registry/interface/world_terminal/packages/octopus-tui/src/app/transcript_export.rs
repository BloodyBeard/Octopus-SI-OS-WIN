use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use octopus_descriptors::InterfaceId;
use octopus_descriptors::RunId;

use super::App;
use crate::app_event::TranscriptExportDestination;
use crate::app_server_session::AppServerSession;

impl App {
    pub(super) async fn export_transcript(
        &mut self,
        app_server: &AppServerSession,
        destination: TranscriptExportDestination,
    ) -> Result<(), String> {
        let run = app_server
            .active_run_id()
            .ok_or_else(|| "No active Octopus run to export.".to_string())?;
        let interface = app_server
            .client_interface()
            .map_err(|error| error.to_string())?;
        let durable_root = app_server
            .prompt_history_durable_root()
            .ok_or_else(|| "The active terminal has no durable movement ledger.".to_string())?;
        self.chat_widget.add_info_message(
            "Hydrating the complete run ledger for export…".to_string(),
            None,
        );
        let markdown = tokio::task::spawn_blocking(move || {
            hydrate_markdown_transcript(&durable_root, &run, &interface)
        })
        .await
        .map_err(|error| format!("transcript export worker failed: {error}"))??;

        match destination {
            TranscriptExportDestination::Clipboard => {
                self.chat_widget.copy_transcript_to_clipboard(&markdown);
            }
            TranscriptExportDestination::File(requested_path) => {
                let cwd = std::env::current_dir().map_err(|error| {
                    format!("could not determine local export directory: {error}")
                })?;
                let path = write_transcript(&cwd, &requested_path, &markdown)?;
                self.chat_widget.add_info_message(
                    format!("Saved complete conversation to {}.", path.display()),
                    None,
                );
            }
        }
        Ok(())
    }
}

pub(super) fn hydrate_markdown_transcript(
    durable_root: &Path,
    run: &RunId,
    interface: &InterfaceId,
) -> Result<String, String> {
    let hydrator = octopus_world_terminal_client::LedgerTranscriptHydrator::open(durable_root)
        .map_err(|error| format!("could not open movement ledger: {error}"))?;
    let entries = hydrator
        .hydrate_full(run, interface, 1024)
        .map_err(|error| format!("could not hydrate complete movement ledger: {error}"))?;
    octopus_world_terminal_client::render_markdown_transcript(&entries)
}

fn write_transcript(cwd: &Path, requested_path: &Path, markdown: &str) -> Result<PathBuf, String> {
    let path = if let Ok(relative) = requested_path.strip_prefix("~") {
        dirs::home_dir()
            .ok_or_else(|| "could not determine the home directory".to_string())?
            .join(relative)
    } else if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        cwd.join(requested_path)
    };
    let parent = path.parent().unwrap_or(cwd);
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    file.write_all(markdown.as_bytes())
        .and_then(|()| file.as_file().sync_all())
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    file.persist_noclobber(&path)
        .map_err(|error| format!("could not create {}: {}", path.display(), error.error))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_file_write_is_no_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let requested = PathBuf::from("conversation.md");
        let path = write_transcript(dir.path(), &requested, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        assert!(write_transcript(dir.path(), &requested, "second").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
    }
}
