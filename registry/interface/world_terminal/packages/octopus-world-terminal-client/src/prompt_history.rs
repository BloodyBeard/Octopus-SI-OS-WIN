use std::path::{Path, PathBuf};

use octopus_ledger::{ArtifactRef, Endpoint, Movement, MovementKind};
use octopus_ledger_store::DurableMovementDecoder;

use crate::emit_input::TERMINAL_INPUT_ARTIFACT_LABEL;
use crate::ledger_transcript::{LedgerScanError, blob_artifact_display_text};
use crate::run_index::movement_invocation_metadata;

#[derive(Debug)]
pub struct PromptHistory {
    blobs_dir: PathBuf,
    entries: Vec<ArtifactRef>,
}

impl PromptHistory {
    pub fn load(durable_root: &Path) -> Result<Self, LedgerScanError> {
        let movements_path = durable_root.join("movements.jsonl");
        let blobs_dir = durable_root.join("blobs");
        let text = match std::fs::read_to_string(&movements_path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    blobs_dir,
                    entries: Vec::new(),
                });
            }
            Err(source) => {
                return Err(LedgerScanError::LedgerRead {
                    path: movements_path.display().to_string(),
                    source,
                });
            }
        };
        let decoder = DurableMovementDecoder::for_log(&movements_path);

        let mut inputs: Vec<Movement> = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let movement = decoder.decode(trimmed.as_bytes(), index).map_err(|err| {
                LedgerScanError::LedgerRow {
                    line: index + 1,
                    reason: err.to_string(),
                }
            })?;
            if movement.kind != MovementKind::InterfaceArrival {
                continue;
            }
            let Some(runtime) =
                movement_invocation_metadata(&movement).and_then(|metadata| metadata.runtime)
            else {
                continue;
            };
            let Endpoint::Interface(source_interface) = &movement.from else {
                continue;
            };
            if source_interface != &runtime.interface {
                continue;
            }
            let Some(artifact) = movement.artifact.as_ref() else {
                continue;
            };
            let label = artifact
                .id
                .as_str()
                .split_once(':')
                .map_or(artifact.id.as_str(), |(label, _)| label);
            if label != TERMINAL_INPUT_ARTIFACT_LABEL {
                continue;
            }
            inputs.push(movement);
        }
        inputs.sort_by_key(|movement| (movement.recorded_at_unix_ms, movement.seq.0));
        Ok(Self {
            blobs_dir,
            entries: inputs
                .into_iter()
                .filter_map(|movement| movement.artifact)
                .collect(),
        })
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn entry_text(&self, offset: usize) -> Option<String> {
        let artifact = self.entries.get(offset)?;
        match blob_artifact_display_text(&self.blobs_dir, artifact) {
            Ok(text) => Some(text),
            Err(reason) => {
                tracing::warn!(offset, %reason, "prompt history entry payload unreadable");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ledger::{rewrite_movements, seed_labeled_movements};

    #[test]
    fn missing_ledger_is_honest_empty_history() {
        let dir = tempfile::tempdir().unwrap();
        let history = PromptHistory::load(&dir.path().join("durable")).unwrap();
        assert_eq!(history.entry_count(), 0);
        assert_eq!(history.entry_text(0), None);
    }

    #[test]
    fn folds_only_terminal_input_arrivals_across_runs_in_recorded_order() {
        let dir = tempfile::tempdir().unwrap();
        let durable = dir.path().join("durable");
        seed_labeled_movements(
            &durable,
            &[
                (
                    "run_b",
                    MovementKind::InterfaceArrival,
                    20,
                    TERMINAL_INPUT_ARTIFACT_LABEL,
                    "second prompt",
                ),
                (
                    "run_a",
                    MovementKind::InterfaceArrival,
                    10,
                    TERMINAL_INPUT_ARTIFACT_LABEL,
                    "first prompt",
                ),
                (
                    "run_a",
                    MovementKind::InterfaceArrival,
                    15,
                    "operator.approval.response",
                    "yes",
                ),
                (
                    "run_a",
                    MovementKind::InterfaceOutbound,
                    30,
                    "interface.artifact.output",
                    "model reply",
                ),
                (
                    "run_a",
                    MovementKind::InterfaceArrival,
                    25,
                    TERMINAL_INPUT_ARTIFACT_LABEL,
                    "untyped prompt",
                ),
            ],
        );
        rewrite_movements(&durable, |movement| {
            if movement.recorded_at_unix_ms == 25 {
                movement.invocation_metadata =
                    Some(octopus_ledger::DeliveryVerdict::Admitted { seq: 25 }.into());
            }
        });
        let history = PromptHistory::load(&durable).unwrap();
        assert_eq!(history.entry_count(), 2);
        assert_eq!(history.entry_text(0).as_deref(), Some("first prompt"));
        assert_eq!(history.entry_text(1).as_deref(), Some("second prompt"));
        assert_eq!(history.entry_text(2), None);
    }

    #[test]
    fn malformed_ledger_row_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let durable = dir.path().join("durable");
        std::fs::create_dir_all(&durable).unwrap();
        std::fs::write(durable.join("movements.jsonl"), "{ not a movement\n").unwrap();
        let err = PromptHistory::load(&durable).unwrap_err();
        assert!(matches!(err, LedgerScanError::LedgerRow { .. }));
    }
}
