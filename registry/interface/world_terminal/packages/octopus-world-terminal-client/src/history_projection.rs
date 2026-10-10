use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use octopus_descriptors::{InterfaceId, RunId};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_ledger_store::DurableMovementDecoder;

use crate::ledger_transcript::{
    LedgerScanError, TranscriptEntry, blob_artifact_display_text, transcript_artifact_from_movement,
};

pub const MAX_HISTORY_PAGE_ENTRIES: usize = 100;
pub const MAX_HISTORY_SCAN_ROWS: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryCursor {
    snapshot_len: u64,
    before_byte: u64,
    snapshot_tail_hash: ContentHash,
}

impl HistoryCursor {
    pub fn snapshot_len(&self) -> u64 {
        self.snapshot_len
    }

    pub fn before_byte(&self) -> u64 {
        self.before_byte
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptArtifactEntry {
    pub seq: u64,
    pub artifact: ArtifactRef,
    pub entry: TranscriptEntry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPage {
    pub entries: Vec<TranscriptArtifactEntry>,
    pub next_cursor: Option<HistoryCursor>,
    pub rows_scanned: usize,
    pub partial: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryProjectionError {
    #[error(transparent)]
    Ledger(#[from] LedgerScanError),
    #[error("history page size must be between 1 and {MAX_HISTORY_PAGE_ENTRIES}, got {0}")]
    InvalidPageSize(usize),
    #[error("history scan budget must be between 1 and {MAX_HISTORY_SCAN_ROWS}, got {0}")]
    InvalidScanBudget(usize),
    #[error("ledger '{path}' changed before the opaque history cursor could be resumed")]
    StaleCursor { path: String },
}

#[derive(Debug)]
pub struct ReverseHistoryPager {
    movements_path: PathBuf,
    blobs_dir: PathBuf,
    run: RunId,
    client_interface: InterfaceId,
    cursor: Option<HistoryCursor>,
}

impl ReverseHistoryPager {
    pub fn open(
        durable_root: &Path,
        run: RunId,
        client_interface: InterfaceId,
    ) -> Result<Self, HistoryProjectionError> {
        let movements_path = durable_root.join("movements.jsonl");
        let mut file = match File::open(&movements_path) {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    movements_path,
                    blobs_dir: durable_root.join("blobs"),
                    run,
                    client_interface,
                    cursor: None,
                });
            }
            Err(source) => {
                return Err(LedgerScanError::LedgerRead {
                    path: movements_path.display().to_string(),
                    source,
                }
                .into());
            }
        };
        let snapshot_len = complete_snapshot_len(&mut file, &movements_path)?;
        let cursor = if snapshot_len == 0 {
            None
        } else {
            let (_, tail) = previous_row(&mut file, snapshot_len, &movements_path)?
                .ok_or_else(|| LedgerScanError::LedgerChanged)?;
            Some(HistoryCursor {
                snapshot_len,
                before_byte: snapshot_len,
                snapshot_tail_hash: ContentHash::of_bytes(&tail),
            })
        };
        Ok(Self {
            movements_path,
            blobs_dir: durable_root.join("blobs"),
            run,
            client_interface,
            cursor,
        })
    }

    pub fn cursor(&self) -> Option<HistoryCursor> {
        self.cursor.clone()
    }

    pub fn is_complete(&self) -> bool {
        self.cursor.is_none()
    }

    pub fn read_page(
        &mut self,
        page_size: usize,
        scan_budget: usize,
    ) -> Result<HistoryPage, HistoryProjectionError> {
        if !(1..=MAX_HISTORY_PAGE_ENTRIES).contains(&page_size) {
            return Err(HistoryProjectionError::InvalidPageSize(page_size));
        }
        if !(1..=MAX_HISTORY_SCAN_ROWS).contains(&scan_budget) {
            return Err(HistoryProjectionError::InvalidScanBudget(scan_budget));
        }
        let Some(mut cursor) = self.cursor.clone() else {
            return Ok(HistoryPage {
                entries: Vec::new(),
                next_cursor: None,
                rows_scanned: 0,
                partial: false,
            });
        };
        let mut file =
            File::open(&self.movements_path).map_err(|source| LedgerScanError::LedgerRead {
                path: self.movements_path.display().to_string(),
                source,
            })?;
        validate_cursor(&mut file, &self.movements_path, &cursor)?;
        let decoder = DurableMovementDecoder::for_log(&self.movements_path);
        let mut newest_first = Vec::new();
        let mut rows_scanned = 0usize;

        while cursor.before_byte > 0 && rows_scanned < scan_budget && newest_first.len() < page_size
        {
            let Some((before_byte, row)) =
                previous_row(&mut file, cursor.before_byte, &self.movements_path)?
            else {
                cursor.before_byte = 0;
                break;
            };
            cursor.before_byte = before_byte;
            rows_scanned += 1;
            let trimmed = trim_ascii_whitespace(&row);
            if trimmed.is_empty() {
                continue;
            }
            let movement =
                decoder
                    .decode(trimmed, 0)
                    .map_err(|error| LedgerScanError::LedgerRow {
                        line: 0,
                        reason: format!(
                            "row ending at byte {} failed decoding: {error}",
                            cursor.before_byte
                        ),
                    })?;
            let Some((side, label, artifact)) =
                transcript_artifact_from_movement(&movement, &self.run, &self.client_interface)
            else {
                continue;
            };
            let text =
                blob_artifact_display_text(&self.blobs_dir, &artifact).map_err(|reason| {
                    LedgerScanError::LedgerRow {
                        line: movement.seq.0 as usize + 1,
                        reason,
                    }
                })?;
            newest_first.push(TranscriptArtifactEntry {
                seq: movement.seq.0,
                artifact,
                entry: TranscriptEntry {
                    side,
                    recorded_at_unix_ms: movement.recorded_at_unix_ms,
                    label,
                    text,
                },
            });
        }

        newest_first.reverse();
        self.cursor = (cursor.before_byte > 0).then_some(cursor.clone());
        Ok(HistoryPage {
            entries: newest_first,
            next_cursor: self.cursor.clone(),
            rows_scanned,
            partial: self.cursor.is_some() && rows_scanned == scan_budget,
        })
    }
}

fn validate_cursor(
    file: &mut File,
    movements_path: &Path,
    cursor: &HistoryCursor,
) -> Result<(), HistoryProjectionError> {
    let len = file
        .metadata()
        .map_err(|source| LedgerScanError::LedgerRead {
            path: movements_path.display().to_string(),
            source,
        })?
        .len();
    if len < cursor.snapshot_len || cursor.before_byte > cursor.snapshot_len {
        return Err(HistoryProjectionError::StaleCursor {
            path: movements_path.display().to_string(),
        });
    }
    let (_, tail) = previous_row(file, cursor.snapshot_len, movements_path)?.ok_or_else(|| {
        HistoryProjectionError::StaleCursor {
            path: movements_path.display().to_string(),
        }
    })?;
    if ContentHash::of_bytes(&tail) != cursor.snapshot_tail_hash {
        return Err(HistoryProjectionError::StaleCursor {
            path: movements_path.display().to_string(),
        });
    }
    Ok(())
}

fn complete_snapshot_len(file: &mut File, path: &Path) -> Result<u64, LedgerScanError> {
    let len = file
        .metadata()
        .map_err(|source| LedgerScanError::LedgerRead {
            path: path.display().to_string(),
            source,
        })?
        .len();
    if len == 0 {
        return Ok(0);
    }
    file.seek(SeekFrom::Start(len - 1))
        .and_then(|_| {
            let mut byte = [0u8; 1];
            file.read_exact(&mut byte)?;
            Ok(byte[0])
        })
        .map_err(|source| LedgerScanError::LedgerRead {
            path: path.display().to_string(),
            source,
        })
        .and_then(|last| {
            if last == b'\n' {
                Ok(len)
            } else {
                previous_newline(file, len, path).map(|offset| offset.map_or(0, |value| value + 1))
            }
        })
}

fn previous_row(
    file: &mut File,
    before: u64,
    path: &Path,
) -> Result<Option<(u64, Vec<u8>)>, LedgerScanError> {
    if before == 0 {
        return Ok(None);
    }
    let mut line_end = before;
    file.seek(SeekFrom::Start(before - 1))
        .map_err(|source| LedgerScanError::LedgerRead {
            path: path.display().to_string(),
            source,
        })?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)
        .map_err(|source| LedgerScanError::LedgerRead {
            path: path.display().to_string(),
            source,
        })?;
    if last[0] == b'\n' {
        line_end -= 1;
    }
    let line_start = previous_newline(file, line_end, path)?.map_or(0, |offset| offset + 1);
    let len = usize::try_from(line_end.saturating_sub(line_start))
        .map_err(|_| LedgerScanError::LedgerChanged)?;
    let mut row = vec![0u8; len];
    file.seek(SeekFrom::Start(line_start))
        .and_then(|_| file.read_exact(&mut row))
        .map_err(|source| LedgerScanError::LedgerRead {
            path: path.display().to_string(),
            source,
        })?;
    Ok(Some((line_start, row)))
}

fn previous_newline(
    file: &mut File,
    before: u64,
    path: &Path,
) -> Result<Option<u64>, LedgerScanError> {
    let mut end = before;
    let mut buffer = vec![0u8; 8192];
    while end > 0 {
        let start = end.saturating_sub(buffer.len() as u64);
        let len = usize::try_from(end - start).map_err(|_| LedgerScanError::LedgerChanged)?;
        file.seek(SeekFrom::Start(start))
            .and_then(|_| file.read_exact(&mut buffer[..len]))
            .map_err(|source| LedgerScanError::LedgerRead {
                path: path.display().to_string(),
                source,
            })?;
        if let Some(index) = buffer[..len].iter().rposition(|byte| *byte == b'\n') {
            return Ok(Some(start + index as u64));
        }
        end = start;
    }
    Ok(None)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ledger::seed_labeled_movements;
    use octopus_ledger::MovementKind;

    #[test]
    fn reverse_pages_are_chronological_and_cursor_bounded() {
        let root = tempfile::tempdir().unwrap();
        let durable = root.path().join("durable");
        seed_labeled_movements(
            &durable,
            &[
                (
                    "run_a",
                    MovementKind::InterfaceArrival,
                    1,
                    "operator.terminal.input",
                    "one",
                ),
                (
                    "run_a",
                    MovementKind::InterfaceOutbound,
                    2,
                    "interface.artifact.output",
                    "two",
                ),
                (
                    "run_a",
                    MovementKind::InterfaceArrival,
                    3,
                    "operator.terminal.input",
                    "three",
                ),
            ],
        );
        let mut pager = ReverseHistoryPager::open(
            &durable,
            RunId::new("run_a"),
            InterfaceId::new("world_terminal"),
        )
        .unwrap();
        let newest = pager.read_page(2, 10).unwrap();
        assert_eq!(
            newest
                .entries
                .iter()
                .map(|entry| entry.entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["two", "three"]
        );
        assert!(newest.next_cursor.is_some());
        let older = pager.read_page(2, 10).unwrap();
        assert_eq!(older.entries[0].entry.text, "one");
        assert!(older.next_cursor.is_none());
    }
}
