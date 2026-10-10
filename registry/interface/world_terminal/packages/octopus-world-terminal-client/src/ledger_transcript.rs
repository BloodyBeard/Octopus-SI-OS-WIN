use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};

use octopus_descriptors::{InterfaceId, RunId};
use octopus_interfaces::InterfaceArtifactFile;
use octopus_ledger::{ArtifactRef, Endpoint, Movement, MovementKind};
use octopus_ledger_store::DurableMovementDecoder;

use crate::emit_input::TERMINAL_INPUT_ARTIFACT_LABEL;
use crate::render_outbound::{OutboundRenderKind, classify_outbound_artifact};
use crate::run_index::{movement_invocation_metadata, movement_run_endpoints};

#[derive(Debug, thiserror::Error)]
pub enum LedgerScanError {
    #[error("ledger '{path}' could not be read: {source}")]
    LedgerRead {
        path: String,
        source: std::io::Error,
    },
    #[error("ledger row at line {line} is not a movement (fail-closed): {reason}")]
    LedgerRow { line: usize, reason: String },
    #[error("ledger page size must be between 1 and 4096 rows, got {0}")]
    InvalidPageSize(usize),
    #[error(
        "ledger cursor byte {byte_offset} is beyond the export snapshot of {snapshot_len} bytes"
    )]
    CursorPastSnapshot { byte_offset: u64, snapshot_len: u64 },
    #[error("ledger changed while export snapshot was being hydrated")]
    LedgerChanged,
    #[error("ledger export snapshot ends with an incomplete row at line {line}")]
    PartialLedgerRow { line: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptSide {
    Inbound,
    Outbound,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub side: TranscriptSide,
    pub recorded_at_unix_ms: u64,
    pub label: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LedgerPageCursor {
    pub byte_offset: u64,
    pub line_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerTranscriptPage {
    pub entries: Vec<TranscriptEntry>,
    pub next_cursor: Option<LedgerPageCursor>,
    pub rows_scanned: usize,
    pub snapshot_len: u64,
}

#[derive(Debug)]
pub struct LedgerTranscriptHydrator {
    movements_path: PathBuf,
    blobs_dir: PathBuf,
    snapshot_len: u64,
}

impl LedgerTranscriptHydrator {
    pub fn open(durable_root: &Path) -> Result<Self, LedgerScanError> {
        let movements_path = durable_root.join("movements.jsonl");
        let snapshot_len = match std::fs::metadata(&movements_path) {
            Ok(metadata) => metadata.len(),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => 0,
            Err(source) => {
                return Err(LedgerScanError::LedgerRead {
                    path: movements_path.display().to_string(),
                    source,
                });
            }
        };
        Ok(Self {
            movements_path,
            blobs_dir: durable_root.join("blobs"),
            snapshot_len,
        })
    }

    pub fn read_page(
        &self,
        run: &RunId,
        client_interface: &InterfaceId,
        cursor: LedgerPageCursor,
        page_size: usize,
    ) -> Result<LedgerTranscriptPage, LedgerScanError> {
        if !(1..=4096).contains(&page_size) {
            return Err(LedgerScanError::InvalidPageSize(page_size));
        }
        if cursor.byte_offset > self.snapshot_len {
            return Err(LedgerScanError::CursorPastSnapshot {
                byte_offset: cursor.byte_offset,
                snapshot_len: self.snapshot_len,
            });
        }
        if cursor.byte_offset == self.snapshot_len {
            return Ok(LedgerTranscriptPage {
                entries: Vec::new(),
                next_cursor: None,
                rows_scanned: 0,
                snapshot_len: self.snapshot_len,
            });
        }

        let mut file =
            File::open(&self.movements_path).map_err(|source| LedgerScanError::LedgerRead {
                path: self.movements_path.display().to_string(),
                source,
            })?;
        let current_len = file
            .metadata()
            .map_err(|source| LedgerScanError::LedgerRead {
                path: self.movements_path.display().to_string(),
                source,
            })?
            .len();
        if current_len < self.snapshot_len {
            return Err(LedgerScanError::LedgerChanged);
        }
        file.seek(SeekFrom::Start(cursor.byte_offset))
            .map_err(|source| LedgerScanError::LedgerRead {
                path: self.movements_path.display().to_string(),
                source,
            })?;

        let remaining = self.snapshot_len - cursor.byte_offset;
        let mut reader = BufReader::new(file).take(remaining);
        let decoder = DurableMovementDecoder::for_log(&self.movements_path);
        let mut entries = Vec::new();
        let mut byte_offset = cursor.byte_offset;
        let mut line_index = cursor.line_index;
        let mut rows_scanned = 0usize;

        while rows_scanned < page_size && byte_offset < self.snapshot_len {
            let mut bytes = Vec::new();
            let read = reader.read_until(b'\n', &mut bytes).map_err(|source| {
                LedgerScanError::LedgerRead {
                    path: self.movements_path.display().to_string(),
                    source,
                }
            })?;
            if read == 0 {
                return Err(LedgerScanError::LedgerChanged);
            }
            byte_offset += read as u64;
            if bytes.last() != Some(&b'\n') {
                return Err(LedgerScanError::PartialLedgerRow {
                    line: line_index + 1,
                });
            }
            rows_scanned += 1;
            let current_line = line_index;
            line_index += 1;
            let trimmed = trim_ascii_whitespace(&bytes);
            if trimmed.is_empty() {
                continue;
            }
            let movement = decoder.decode(trimmed, current_line).map_err(|error| {
                LedgerScanError::LedgerRow {
                    line: current_line + 1,
                    reason: error.to_string(),
                }
            })?;
            if let Some(entry) =
                transcript_entry_from_movement(&self.blobs_dir, &movement, run, client_interface)?
            {
                entries.push(entry);
            }
        }

        let next_cursor = (byte_offset < self.snapshot_len).then_some(LedgerPageCursor {
            byte_offset,
            line_index,
        });
        Ok(LedgerTranscriptPage {
            entries,
            next_cursor,
            rows_scanned,
            snapshot_len: self.snapshot_len,
        })
    }

    pub fn hydrate_full(
        &self,
        run: &RunId,
        client_interface: &InterfaceId,
        page_size: usize,
    ) -> Result<Vec<TranscriptEntry>, LedgerScanError> {
        let mut cursor = LedgerPageCursor::default();
        let mut entries = Vec::new();
        loop {
            let page = self.read_page(run, client_interface, cursor, page_size)?;
            entries.extend(page.entries);
            match page.next_cursor {
                Some(next) => cursor = next,
                None => return Ok(entries),
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunActivity {
    pub last_activity_unix_ms: Option<u64>,
    pub preview: Option<String>,
    pub movement_count: usize,
}

#[derive(Debug)]
pub struct LedgerScan {
    blobs_dir: PathBuf,
    by_run: BTreeMap<String, Vec<Movement>>,
}

impl LedgerScan {
    pub fn fold(durable_root: &Path) -> Result<Self, LedgerScanError> {
        let movements_path = durable_root.join("movements.jsonl");
        let blobs_dir = durable_root.join("blobs");
        let text = match std::fs::read_to_string(&movements_path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    blobs_dir,
                    by_run: BTreeMap::new(),
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

        let mut by_run: BTreeMap<String, Vec<Movement>> = BTreeMap::new();
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
            for run in movement_run_endpoints(&movement) {
                by_run
                    .entry(run.as_str().to_string())
                    .or_default()
                    .push(movement.clone());
            }
        }
        Ok(Self { blobs_dir, by_run })
    }

    pub fn run_activity(&self, run: &RunId) -> RunActivity {
        let Some(movements) = self.by_run.get(run.as_str()) else {
            return RunActivity::default();
        };
        let last_activity_unix_ms = movements.iter().map(|m| m.recorded_at_unix_ms).max();
        let preview = {
            let mut with_text: Vec<&Movement> =
                movements.iter().filter(|m| m.artifact.is_some()).collect();
            with_text.sort_by_key(|m| (m.recorded_at_unix_ms, m.seq.0));
            with_text
                .into_iter()
                .rev()
                .find_map(|movement| self.movement_display_text(movement).ok().flatten())
        };
        RunActivity {
            last_activity_unix_ms,
            preview,
            movement_count: movements.len(),
        }
    }

    pub fn run_transcript(
        &self,
        run: &RunId,
        client_interface: &InterfaceId,
    ) -> Result<Vec<TranscriptEntry>, LedgerScanError> {
        let Some(movements) = self.by_run.get(run.as_str()) else {
            return Ok(Vec::new());
        };
        let mut entries = Vec::new();
        for movement in movements {
            if let Some(entry) =
                transcript_entry_from_movement(&self.blobs_dir, movement, run, client_interface)?
            {
                entries.push((movement.seq.0, entry));
            }
        }
        entries.sort_by_key(|(seq, _)| *seq);
        Ok(entries.into_iter().map(|(_, entry)| entry).collect())
    }

    fn movement_display_text(&self, movement: &Movement) -> Result<Option<String>, String> {
        let Some(artifact) = movement.artifact.as_ref() else {
            return Ok(None);
        };
        self.artifact_display_text(artifact).map(Some)
    }

    fn artifact_display_text(&self, artifact: &ArtifactRef) -> Result<String, String> {
        blob_artifact_display_text(&self.blobs_dir, artifact)
    }
}

fn transcript_entry_from_movement(
    blobs_dir: &Path,
    movement: &Movement,
    run: &RunId,
    client_interface: &InterfaceId,
) -> Result<Option<TranscriptEntry>, LedgerScanError> {
    let Some((side, label, artifact)) =
        transcript_artifact_from_movement(movement, run, client_interface)
    else {
        return Ok(None);
    };
    let text = blob_artifact_display_text(blobs_dir, &artifact).map_err(|reason| {
        LedgerScanError::LedgerRow {
            line: movement.seq.0 as usize + 1,
            reason,
        }
    })?;
    Ok(Some(TranscriptEntry {
        side,
        recorded_at_unix_ms: movement.recorded_at_unix_ms,
        label,
        text,
    }))
}

pub(crate) fn transcript_artifact_from_movement(
    movement: &Movement,
    run: &RunId,
    client_interface: &InterfaceId,
) -> Option<(TranscriptSide, String, ArtifactRef)> {
    let Some(runtime) =
        movement_invocation_metadata(movement).and_then(|metadata| metadata.runtime)
    else {
        return None;
    };
    if runtime.run != *run || runtime.interface != *client_interface {
        return None;
    }
    let side = match (&movement.kind, &movement.from, &movement.to) {
        (MovementKind::InterfaceArrival, Endpoint::Interface(interface), _)
            if interface == client_interface =>
        {
            TranscriptSide::Inbound
        }
        (
            MovementKind::InterfaceOutbound,
            Endpoint::Interface(_),
            Endpoint::Interface(interface),
        ) if interface == client_interface => TranscriptSide::Outbound,
        _ => return None,
    };
    let artifact = movement.artifact.as_ref()?;
    let label = artifact
        .id
        .as_str()
        .split_once(':')
        .map_or(artifact.id.as_str(), |(label, _)| label)
        .to_string();
    match side {
        TranscriptSide::Inbound if label != TERMINAL_INPUT_ARTIFACT_LABEL => return None,
        TranscriptSide::Outbound
            if matches!(
                classify_outbound_artifact(artifact.id.as_str()),
                OutboundRenderKind::UserMessage | OutboundRenderKind::HookCatalog
            ) =>
        {
            return None;
        }
        _ => {}
    }
    Some((side, label, artifact.clone()))
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

pub fn render_markdown_transcript(entries: &[TranscriptEntry]) -> Result<String, String> {
    let mut markdown = String::from("# Octopus conversation\n");
    let mut pending_delta = String::new();
    for entry in entries {
        match entry.side {
            TranscriptSide::Inbound => {
                flush_pending_delta(&mut markdown, &mut pending_delta, None);
                push_markdown_section(&mut markdown, "User", &entry.text, false);
            }
            TranscriptSide::Outbound => match classify_outbound_artifact(&entry.label) {
                OutboundRenderKind::MessageDelta => pending_delta.push_str(&entry.text),
                OutboundRenderKind::TerminalReply => {
                    flush_pending_delta(&mut markdown, &mut pending_delta, Some(&entry.text));
                    push_markdown_section(&mut markdown, "Assistant", &entry.text, false);
                }
                OutboundRenderKind::PlanUpdate => {
                    flush_pending_delta(&mut markdown, &mut pending_delta, None);
                    push_markdown_section(&mut markdown, "Plan", &entry.text, false);
                }
                OutboundRenderKind::Reasoning => {
                    flush_pending_delta(&mut markdown, &mut pending_delta, None);
                    push_markdown_section(&mut markdown, "Reasoning", &entry.text, false);
                }
                OutboundRenderKind::UserMessage | OutboundRenderKind::HookCatalog => {}
                _ => {
                    flush_pending_delta(&mut markdown, &mut pending_delta, None);
                    push_markdown_section(&mut markdown, "Activity", &entry.text, true);
                }
            },
        }
    }
    flush_pending_delta(&mut markdown, &mut pending_delta, None);
    if markdown == "# Octopus conversation\n" {
        Err("No conversation content to export.".to_string())
    } else {
        Ok(markdown)
    }
}

fn flush_pending_delta(markdown: &mut String, pending: &mut String, final_reply: Option<&str>) {
    if pending.trim().is_empty() {
        pending.clear();
        return;
    }
    let duplicated_by_final = final_reply.is_some_and(|final_reply| {
        let pending = pending.trim();
        let final_reply = final_reply.trim();
        pending == final_reply || final_reply.contains(pending)
    });
    if !duplicated_by_final {
        push_markdown_section(markdown, "Assistant", pending, false);
    }
    pending.clear();
}

fn push_markdown_section(markdown: &mut String, heading: &str, text: &str, indent: bool) {
    if text.trim().is_empty() {
        return;
    }
    markdown.push_str("\n## ");
    markdown.push_str(heading);
    markdown.push_str("\n\n");
    for line in text.lines() {
        if indent {
            markdown.push_str("    ");
        }
        markdown.push_str(line);
        markdown.push('\n');
    }
}

pub(crate) fn blob_artifact_display_text(
    blobs_dir: &Path,
    artifact: &ArtifactRef,
) -> Result<String, String> {
    let bytes = blob_artifact_payload_bytes(blobs_dir, artifact)?;
    if std::str::from_utf8(&bytes).is_err() || bytes.contains(&0) {
        return Ok("<binary content omitted>".to_string());
    }
    Ok(crate::render_outbound::display_text_from_payload(&bytes))
}

pub(crate) fn blob_artifact_payload_bytes(
    blobs_dir: &Path,
    artifact: &ArtifactRef,
) -> Result<Vec<u8>, String> {
    let blobs_dir = if blobs_dir.is_absolute() {
        blobs_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(blobs_dir))
            .unwrap_or_else(|_| blobs_dir.to_path_buf())
    };
    let manifest_path = blobs_dir.join(artifact.content.to_hex());
    let artifact_file = InterfaceArtifactFile::new(&manifest_path).map_err(|e| {
        format!(
            "artifact manifest blob '{}' cannot be opened: {e}",
            manifest_path.display()
        )
    })?;
    let bytes = artifact_file
        .read_payload_bytes()
        .map_err(|e| format!("artifact payload unreadable: {e}"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ledger::{rewrite_movements, seed_labeled_movements, seed_movements};

    #[test]
    fn missing_ledger_is_honest_empty() {
        let dir = tempfile::tempdir().unwrap();
        let scan = LedgerScan::fold(&dir.path().join("durable")).unwrap();
        assert_eq!(
            scan.run_activity(&RunId::new("run_x")),
            RunActivity::default()
        );
        assert!(
            scan.run_transcript(&RunId::new("run_x"), &InterfaceId::new("world_terminal"),)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn folds_run_transcript_in_recorded_order_with_sides() {
        let dir = tempfile::tempdir().unwrap();
        let durable = dir.path().join("durable");
        seed_labeled_movements(
            &durable,
            &[
                (
                    "run_a",
                    MovementKind::InterfaceArrival,
                    10,
                    TERMINAL_INPUT_ARTIFACT_LABEL,
                    "user asks",
                ),
                (
                    "run_a",
                    MovementKind::InterfaceOutbound,
                    20,
                    "interface.artifact.output",
                    "run replies",
                ),
                (
                    "run_b",
                    MovementKind::InterfaceArrival,
                    15,
                    TERMINAL_INPUT_ARTIFACT_LABEL,
                    "other run",
                ),
            ],
        );
        let scan = LedgerScan::fold(&durable).unwrap();
        let transcript = scan
            .run_transcript(&RunId::new("run_a"), &InterfaceId::new("world_terminal"))
            .unwrap();
        assert_eq!(transcript.len(), 2, "only run_a's two movements");
        assert_eq!(transcript[0].side, TranscriptSide::Inbound);
        assert_eq!(transcript[0].text, "user asks");
        assert_eq!(transcript[1].side, TranscriptSide::Outbound);
        assert_eq!(transcript[1].text, "run replies");
    }

    #[test]
    fn malformed_ledger_row_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let durable = dir.path().join("durable");
        std::fs::create_dir_all(&durable).unwrap();
        std::fs::write(durable.join("movements.jsonl"), "{ not a movement\n").unwrap();
        let err = LedgerScan::fold(&durable).unwrap_err();
        assert!(matches!(err, LedgerScanError::LedgerRow { .. }));
    }

    #[test]
    fn activity_preview_is_most_recent_movement() {
        let dir = tempfile::tempdir().unwrap();
        let durable = dir.path().join("durable");
        seed_movements(
            &durable,
            &[
                ("run_a", MovementKind::InterfaceArrival, 10, "first"),
                ("run_a", MovementKind::InterfaceOutbound, 40, "latest"),
                ("run_a", MovementKind::InterfaceArrival, 25, "middle"),
            ],
        );
        let scan = LedgerScan::fold(&durable).unwrap();
        let activity = scan.run_activity(&RunId::new("run_a"));
        assert_eq!(activity.last_activity_unix_ms, Some(40));
        assert_eq!(activity.preview.as_deref(), Some("latest"));
        assert_eq!(activity.movement_count, 3);
    }

    #[test]
    fn foreign_interface_outbound_is_not_a_transcript_reply() {
        let dir = tempfile::tempdir().unwrap();
        let durable = dir.path().join("durable");
        seed_labeled_movements(
            &durable,
            &[
                (
                    "run_a",
                    MovementKind::InterfaceArrival,
                    10,
                    TERMINAL_INPUT_ARTIFACT_LABEL,
                    "user asks",
                ),
                (
                    "run_a",
                    MovementKind::InterfaceOutbound,
                    20,
                    "interface.artifact.output",
                    "legacy reply",
                ),
            ],
        );
        rewrite_movements(&durable, |movement| {
            if movement.kind == MovementKind::InterfaceOutbound {
                movement.to = Endpoint::Interface(InterfaceId::new("world_foreign"));
            }
        });

        let scan = LedgerScan::fold(&durable).unwrap();
        let transcript = scan
            .run_transcript(&RunId::new("run_a"), &InterfaceId::new("world_terminal"))
            .unwrap();
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript[0].side, TranscriptSide::Inbound);
    }

    #[test]
    fn hydrates_complete_snapshot_across_ledger_pages() {
        let dir = tempfile::tempdir().unwrap();
        let durable = dir.path().join("durable");
        seed_labeled_movements(
            &durable,
            &[
                (
                    "run_a",
                    MovementKind::InterfaceArrival,
                    10,
                    TERMINAL_INPUT_ARTIFACT_LABEL,
                    "first",
                ),
                (
                    "run_b",
                    MovementKind::InterfaceOutbound,
                    15,
                    "interface.artifact.output",
                    "other",
                ),
                (
                    "run_a",
                    MovementKind::InterfaceOutbound,
                    20,
                    "interface.artifact.output",
                    "second",
                ),
            ],
        );
        let hydrator = LedgerTranscriptHydrator::open(&durable).unwrap();
        let transcript = hydrator
            .hydrate_full(&RunId::new("run_a"), &InterfaceId::new("world_terminal"), 1)
            .unwrap();
        assert_eq!(
            transcript
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[test]
    fn markdown_export_filters_synthetic_user_rows_and_collapses_streamed_final() {
        let entries = vec![
            TranscriptEntry {
                side: TranscriptSide::Inbound,
                recorded_at_unix_ms: 1,
                label: TERMINAL_INPUT_ARTIFACT_LABEL.to_string(),
                text: "question".to_string(),
            },
            TranscriptEntry {
                side: TranscriptSide::Outbound,
                recorded_at_unix_ms: 2,
                label: "agent.message.delta".to_string(),
                text: "answer".to_string(),
            },
            TranscriptEntry {
                side: TranscriptSide::Outbound,
                recorded_at_unix_ms: 3,
                label: "interface.artifact.output".to_string(),
                text: "answer".to_string(),
            },
            TranscriptEntry {
                side: TranscriptSide::Outbound,
                recorded_at_unix_ms: 4,
                label: "turn.user_message".to_string(),
                text: "synthetic review prompt".to_string(),
            },
        ];
        let markdown = render_markdown_transcript(&entries).unwrap();
        assert_eq!(markdown.matches("answer").count(), 1);
        assert!(!markdown.contains("synthetic review prompt"));
        assert!(markdown.contains("## User"));
        assert!(markdown.contains("## Assistant"));
    }
}
