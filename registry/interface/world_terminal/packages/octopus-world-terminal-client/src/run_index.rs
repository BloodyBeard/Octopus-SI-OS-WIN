use std::path::{Path, PathBuf};

use octopus_descriptors::{EeId, RunId};
use octopus_interfaces::InvocationMetadata;
use octopus_ledger::Movement;

use crate::ledger_transcript::{LedgerScan, LedgerScanError};

#[derive(Debug, thiserror::Error)]
pub enum RunIndexError {
    #[error("terminal runs dir '{path}' could not be read: {source}")]
    RunsDir {
        path: String,
        source: std::io::Error,
    },
    #[error("run record '{path}' could not be read: {source}")]
    RunRead {
        path: String,
        source: std::io::Error,
    },
    #[error("run record '{path}' is malformed: {reason}")]
    RunMalformed { path: String, reason: String },
    #[error(transparent)]
    Ledger(#[from] LedgerScanError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunEntry {
    pub run: RunId,
    pub ee: EeId,
    pub worker: String,
    pub record_path: PathBuf,
    pub last_activity_unix_ms: Option<u64>,
    pub preview: Option<String>,
    pub movement_count: usize,
}

pub struct TerminalRunIndex {
    runs_dir: PathBuf,
    durable_root: PathBuf,
}

impl TerminalRunIndex {
    pub fn new(runs_dir: PathBuf, durable_root: PathBuf) -> Self {
        Self {
            runs_dir,
            durable_root,
        }
    }

    pub fn list_runs(&self) -> Result<Vec<RunEntry>, RunIndexError> {
        let record_paths = self.run_record_paths()?;
        if record_paths.is_empty() {
            return Ok(Vec::new());
        }

        let scan = LedgerScan::fold(&self.durable_root)?;

        let mut entries = Vec::with_capacity(record_paths.len());
        for record_path in record_paths {
            let entry = self.entry_from_record(&record_path, &scan)?;
            entries.push(entry);
        }
        entries.sort_by(|a, b| {
            b.last_activity_unix_ms
                .cmp(&a.last_activity_unix_ms)
                .then_with(|| a.run.as_str().cmp(b.run.as_str()))
        });
        Ok(entries)
    }

    fn run_record_paths(&self) -> Result<Vec<PathBuf>, RunIndexError> {
        let entries = match std::fs::read_dir(&self.runs_dir) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(RunIndexError::RunsDir {
                    path: self.runs_dir.display().to_string(),
                    source,
                });
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| RunIndexError::RunsDir {
                path: self.runs_dir.display().to_string(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    fn entry_from_record(
        &self,
        record_path: &Path,
        scan: &LedgerScan,
    ) -> Result<RunEntry, RunIndexError> {
        let bytes = std::fs::read(record_path).map_err(|source| RunIndexError::RunRead {
            path: record_path.display().to_string(),
            source,
        })?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|err| RunIndexError::RunMalformed {
                path: record_path.display().to_string(),
                reason: err.to_string(),
            })?;
        let run = string_field(&value, "id", record_path)?;
        let ee = string_field(&value, "ee", record_path)?;
        let worker = string_field(&value, "worker", record_path)?;
        let run = RunId::new(run);
        let activity = scan.run_activity(&run);
        Ok(RunEntry {
            ee: EeId::new(ee),
            worker,
            record_path: record_path.to_path_buf(),
            last_activity_unix_ms: activity.last_activity_unix_ms,
            preview: activity.preview,
            movement_count: activity.movement_count,
            run,
        })
    }
}

fn string_field(
    value: &serde_json::Value,
    field: &str,
    record_path: &Path,
) -> Result<String, RunIndexError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RunIndexError::RunMalformed {
            path: record_path.display().to_string(),
            reason: format!("missing string field '{field}'"),
        })
}

pub(crate) fn movement_run_endpoints(movement: &Movement) -> Vec<RunId> {
    movement.run().cloned().into_iter().collect()
}

pub(crate) fn movement_invocation_metadata(movement: &Movement) -> Option<InvocationMetadata> {
    movement.invocation_metadata.as_ref()?.invocation().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_ledger::MovementKind;
    use crate::test_ledger::{rewrite_movements, seed_movements, write_run_record};

    #[test]
    fn missing_runs_dir_is_honest_empty() {
        let dir = tempfile::tempdir().unwrap();
        let index =
            TerminalRunIndex::new(dir.path().join("no-such-runs"), dir.path().join("durable"));
        assert!(index.list_runs().unwrap().is_empty());
    }

    #[test]
    fn lists_runs_from_records_and_folds_activity_from_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let runs_dir = dir.path().join("runs");
        let durable = dir.path().join("durable");
        std::fs::create_dir_all(&runs_dir).unwrap();

        write_run_record(&runs_dir, "run_a", "ee_operator", "flash");
        write_run_record(&runs_dir, "run_b", "ee_operator", "flash");

        seed_movements(
            &durable,
            &[
                ("run_b", MovementKind::InterfaceArrival, 100, "hi from b"),
                ("run_a", MovementKind::InterfaceArrival, 200, "hi from a"),
                ("run_a", MovementKind::InterfaceOutbound, 300, "reply to a"),
            ],
        );

        let index = TerminalRunIndex::new(runs_dir, durable);
        let runs = index.list_runs().unwrap();
        assert_eq!(runs.len(), 2, "both authored runs are listed");
        assert_eq!(runs[0].run.as_str(), "run_a");
        assert_eq!(runs[0].last_activity_unix_ms, Some(300));
        assert_eq!(runs[0].movement_count, 2);
        assert_eq!(runs[0].preview.as_deref(), Some("reply to a"));
        assert_eq!(runs[1].run.as_str(), "run_b");
        assert_eq!(runs[1].last_activity_unix_ms, Some(100));
    }

    #[test]
    fn fails_closed_on_malformed_run_record() {
        let dir = tempfile::tempdir().unwrap();
        let runs_dir = dir.path().join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::write(runs_dir.join("run_bad.json"), b"{ not json").unwrap();

        let index = TerminalRunIndex::new(runs_dir, dir.path().join("durable"));
        let err = index.list_runs().unwrap_err();
        assert!(matches!(err, RunIndexError::RunMalformed { .. }));
    }

    #[test]
    fn run_with_no_movements_lists_with_no_activity() {
        let dir = tempfile::tempdir().unwrap();
        let runs_dir = dir.path().join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        write_run_record(&runs_dir, "run_fresh", "ee_operator", "flash");
        let index = TerminalRunIndex::new(runs_dir, dir.path().join("durable"));
        let runs = index.list_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].last_activity_unix_ms, None);
        assert_eq!(runs[0].preview, None);
        assert_eq!(runs[0].movement_count, 0);
    }

    #[test]
    fn excludes_movements_without_typed_run_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let runs_dir = dir.path().join("runs");
        let durable = dir.path().join("durable");
        std::fs::create_dir_all(&runs_dir).unwrap();
        write_run_record(&runs_dir, "run_legacy", "ee_operator", "flash");
        write_run_record(&runs_dir, "run_untyped", "ee_operator", "flash");
        seed_movements(
            &durable,
            &[
                ("run_legacy", MovementKind::InterfaceOutbound, 10, "legacy"),
                (
                    "run_untyped",
                    MovementKind::InterfaceOutbound,
                    20,
                    "untyped",
                ),
            ],
        );
        rewrite_movements(&durable, |movement| match movement.seq.0 {
            0 => movement.invocation_metadata = None,
            1 => {
                movement.invocation_metadata =
                    Some(octopus_ledger::DeliveryVerdict::Admitted { seq: 20 }.into());
            }
            _ => unreachable!(),
        });

        let runs = TerminalRunIndex::new(runs_dir, durable)
            .list_runs()
            .unwrap();
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|run| run.movement_count == 0));
        assert!(runs.iter().all(|run| run.last_activity_unix_ms.is_none()));
    }
}
