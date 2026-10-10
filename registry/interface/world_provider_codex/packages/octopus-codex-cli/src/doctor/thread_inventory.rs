use super::CheckStatus;
use super::Config;
use super::DoctorCheck;
use super::DoctorIssue;
use crate::path_utils::normalize_for_path_comparison;
use crate::rollout_reader::open_rollout_line_reader;
use octopus_protocol::protocol::RolloutItem;
use octopus_protocol::protocol::RolloutLine;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;

const MAX_LEDGER_SCAN_FILES: usize = 10_000;
const SAMPLE_LIMIT: usize = 5;
const CHECK_ID: &str = "ledger.thread_inventory";
const CHECK_CATEGORY: &str = "threads";

#[derive(Clone, Debug)]
struct RolloutAuditFile {
    path: PathBuf,
    archived: bool,
    thread_id: String,
}

#[derive(Default)]
struct RolloutScan {
    files: Vec<RolloutAuditFile>,
    scan_errors: Vec<String>,
    malformed_names: Vec<PathBuf>,
    reached_scan_cap: bool,
}

enum RolloutThreadId {
    Id(String),
    MalformedName,
    Unusable(String),
}

impl RolloutScan {
    fn candidate_count(&self) -> usize {
        self.files.len() + self.malformed_names.len() + self.scan_errors.len()
    }

    fn reached_candidate_cap(&self) -> bool {
        self.candidate_count() >= MAX_LEDGER_SCAN_FILES
    }

    fn record_malformed_name(&mut self, path: PathBuf) {
        if self.reached_candidate_cap() {
            self.reached_scan_cap = true;
            return;
        }
        self.malformed_names.push(path);
        self.reached_scan_cap = self.reached_candidate_cap();
    }

    fn record_scan_error(&mut self, message: String) {
        if self.reached_candidate_cap() {
            self.reached_scan_cap = true;
            return;
        }
        self.scan_errors.push(message);
        self.reached_scan_cap = self.reached_candidate_cap();
    }

    fn active_count(&self) -> usize {
        self.files.iter().filter(|file| !file.archived).count()
    }

    fn archived_count(&self) -> usize {
        self.files.iter().filter(|file| file.archived).count()
    }
}

pub(super) async fn thread_inventory_check(config: &Config) -> DoctorCheck {
    thread_inventory_check_for_root(
        config.codex_home.as_path(),
        config.model_provider_id.as_str(),
    )
    .await
}

async fn thread_inventory_check_for_root(
    octopus_cxhome: &Path,
    default_provider: &str,
) -> DoctorCheck {
    let scan = scan_rollout_files(octopus_cxhome).await;
    let duplicate_thread_ids = duplicate_rollout_thread_ids(&scan.files);

    let mut details = vec![
        format!("default model provider: {default_provider}"),
        format!("ledger active rollout files: {}", scan.active_count()),
        format!("ledger archived rollout files: {}", scan.archived_count()),
        format!("ledger scan errors: {}", scan.scan_errors.len()),
        format!(
            "ledger malformed file names: {}",
            scan.malformed_names.len()
        ),
        format!(
            "ledger duplicate thread ids: {}",
            duplicate_thread_ids.len()
        ),
        format!("ledger scan cap reached: {}", scan.reached_scan_cap),
    ];
    push_samples(
        &mut details,
        "ledger scan error sample",
        scan.scan_errors.iter().map(String::as_str),
    );
    push_samples(
        &mut details,
        "ledger malformed file sample",
        scan.malformed_names
            .iter()
            .map(|path| path.display().to_string()),
    );
    push_samples(
        &mut details,
        "ledger duplicate thread id sample",
        duplicate_thread_ids.iter().map(String::as_str),
    );

    let status = if scan.scan_errors.is_empty()
        && scan.malformed_names.is_empty()
        && duplicate_thread_ids.is_empty()
        && !scan.reached_scan_cap
    {
        CheckStatus::Ok
    } else {
        CheckStatus::Warning
    };

    let summary = if status == CheckStatus::Ok {
        "rollout ledger thread inventory is inspectable"
    } else {
        "rollout ledger thread inventory needs attention"
    };
    let mut check = DoctorCheck::new(CHECK_ID, CHECK_CATEGORY, status, summary).details(details);

    if !duplicate_thread_ids.is_empty() {
        check = check.issue(
            DoctorIssue::new(CheckStatus::Warning, "duplicate rollout thread ids found")
                .measured(format!("{} duplicate ids", duplicate_thread_ids.len()))
                .expected("one durable rollout thread id per thread")
                .remedy("Inspect the sampled rollout files and keep the ledger as the single durable inventory."),
        );
    }
    if !scan.scan_errors.is_empty() || !scan.malformed_names.is_empty() || scan.reached_scan_cap {
        check = check.issue(
            DoctorIssue::new(
                CheckStatus::Warning,
                "rollout ledger scan was incomplete or found malformed evidence",
            )
            .measured(format!(
                "{} scan errors, {} malformed names, scan cap reached: {}",
                scan.scan_errors.len(),
                scan.malformed_names.len(),
                scan.reached_scan_cap
            ))
            .expected("rollout ledger directories are fully scannable and parseable")
            .remedy("Check file permissions and unexpected files under CODEX_HOME sessions."),
        );
    }
    check
}

async fn scan_rollout_files(octopus_cxhome: &Path) -> RolloutScan {
    let mut scan = RolloutScan::default();
    scan_rollout_root(&octopus_cxhome.join("sessions"), false, &mut scan).await;
    scan_rollout_root(&octopus_cxhome.join("archived_sessions"), true, &mut scan).await;
    scan
}

async fn scan_rollout_root(root: &Path, archived: bool, scan: &mut RolloutScan) {
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        if scan.reached_scan_cap {
            return;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                scan.record_scan_error(format!("{} ({err})", dir.display()));
                continue;
            }
        };
        for entry in entries {
            if scan.reached_scan_cap {
                return;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    scan.record_scan_error(format!("{} ({err})", dir.display()));
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(err) => {
                    scan.record_scan_error(format!("{} ({err})", path.display()));
                    continue;
                }
            };
            if file_type.is_dir() {
                dirs.push(path);
                continue;
            }
            if !file_type.is_file() || !is_rollout_file(&path) {
                continue;
            }
            if scan.candidate_count() >= MAX_LEDGER_SCAN_FILES {
                scan.reached_scan_cap = true;
                return;
            }
            let thread_id = match thread_id_from_rollout(&path).await {
                RolloutThreadId::Id(thread_id) => thread_id,
                RolloutThreadId::MalformedName => {
                    scan.record_malformed_name(path.clone());
                    continue;
                }
                RolloutThreadId::Unusable(reason) => {
                    scan.record_scan_error(format!("{} ({reason})", path.display()));
                    continue;
                }
            };
            scan.files.push(RolloutAuditFile {
                path,
                archived,
                thread_id,
            });
        }
    }
}

async fn thread_id_from_rollout(path: &Path) -> RolloutThreadId {
    let mut reader = match open_rollout_line_reader(path).await {
        Ok(reader) => reader,
        Err(err) => return RolloutThreadId::Unusable(err.to_string()),
    };

    while let Some(line) = match reader.next_line().await {
        Ok(line) => line,
        Err(err) => return RolloutThreadId::Unusable(err.to_string()),
    } {
        if line.trim().is_empty() {
            continue;
        }
        let parsed = match serde_json::from_str::<RolloutLine>(&line) {
            Ok(parsed) => parsed,
            Err(err) => return RolloutThreadId::Unusable(err.to_string()),
        };
        if let RolloutItem::SessionMeta(session_meta) = parsed.item {
            return RolloutThreadId::Id(session_meta.meta.id.to_string());
        }
    }

    RolloutThreadId::MalformedName
}

fn is_rollout_file(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("jsonl"))
        && path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("rollout-"))
}

fn path_key(path: &Path) -> PathBuf {
    normalize_for_path_comparison(path).unwrap_or_else(|_| path.to_path_buf())
}

fn duplicate_rollout_thread_ids(files: &[RolloutAuditFile]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut duplicates = HashSet::new();
    for file in files {
        if !seen.insert((file.thread_id.as_str(), path_key(&file.path))) {
            continue;
        }
        if files.iter().any(|other| {
            other.thread_id == file.thread_id && path_key(&other.path) != path_key(&file.path)
        }) {
            duplicates.insert(file.thread_id.clone());
        }
    }
    let mut duplicates = duplicates.into_iter().collect::<Vec<_>>();
    duplicates.sort();
    duplicates
}

fn push_samples<I, V>(details: &mut Vec<String>, label: &str, values: I)
where
    I: Iterator<Item = V>,
    V: ToString,
{
    for value in values.take(SAMPLE_LIMIT) {
        details.push(format!("{label}: {}", value.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_protocol::protocol::RolloutItem;
    use octopus_protocol::protocol::RolloutLine;
    use octopus_protocol::protocol::SessionMeta;
    use octopus_protocol::protocol::SessionMetaLine;
    use octopus_protocol::protocol::SessionSource;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[tokio::test]
    async fn thread_inventory_check_ok_when_rollout_ledger_is_clean() {
        let fixture = Fixture::new();
        fixture.write_rollout(
            false,
            "2025-01-02T10-00-00",
            "00000000-0000-0000-0000-000000000001",
        );
        fixture.write_rollout(
            true,
            "2025-01-02T11-00-00",
            "00000000-0000-0000-0000-000000000002",
        );

        let check =
            thread_inventory_check_for_root(fixture.octopus_cxhome.path(), "test-provider").await;

        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.category, CHECK_CATEGORY);
        assert_detail(&check, "ledger active rollout files", "1");
        assert_detail(&check, "ledger archived rollout files", "1");
        assert_detail(&check, "ledger scan errors", "0");
        assert_detail(&check, "ledger malformed file names", "0");
        assert_detail(&check, "ledger duplicate thread ids", "0");
    }

    #[tokio::test]
    async fn thread_inventory_check_warns_for_duplicate_thread_ids() {
        let fixture = Fixture::new();
        fixture.write_rollout(
            false,
            "2025-01-02T10-00-00",
            "00000000-0000-0000-0000-000000000001",
        );
        fixture.write_rollout(
            false,
            "2025-01-02T11-00-00",
            "00000000-0000-0000-0000-000000000001",
        );

        let check =
            thread_inventory_check_for_root(fixture.octopus_cxhome.path(), "test-provider").await;

        assert_eq!(check.status, CheckStatus::Warning);
        assert_detail(&check, "ledger duplicate thread ids", "1");
        assert!(
            check
                .issues
                .iter()
                .any(|issue| issue.cause == "duplicate rollout thread ids found")
        );
    }

    #[tokio::test]
    async fn thread_inventory_check_warns_for_malformed_rollout_name() {
        let fixture = Fixture::new();
        let root = fixture.octopus_cxhome.path().join("sessions/2025/01/02");
        std::fs::create_dir_all(&root).expect("rollout dir");
        let path = root.join("rollout-not-a-thread.jsonl");
        std::fs::write(&path, "{}\n").expect("rollout file");

        let check =
            thread_inventory_check_for_root(fixture.octopus_cxhome.path(), "test-provider").await;

        assert_eq!(check.status, CheckStatus::Warning);
        assert_detail(&check, "ledger scan errors", "1");
    }

    struct Fixture {
        octopus_cxhome: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                octopus_cxhome: TempDir::new().expect("codex home"),
            }
        }

        fn write_rollout(&self, archived: bool, timestamp: &str, thread_id: &str) -> PathBuf {
            let root = if archived {
                self.octopus_cxhome.path().join("archived_sessions")
            } else {
                self.octopus_cxhome.path().join("sessions/2025/01/02")
            };
            std::fs::create_dir_all(&root).expect("rollout dir");
            let path = root.join(format!("rollout-{timestamp}-{thread_id}.jsonl"));
            let rollout_line = RolloutLine {
                timestamp: timestamp.to_string(),
                item: RolloutItem::SessionMeta(SessionMetaLine {
                    meta: SessionMeta {
                        id: thread_id.to_string(),
                        timestamp: timestamp.to_string(),
                        cwd: self.octopus_cxhome.path().to_path_buf(),
                        originator: "test".to_string(),
                        cli_version: "test".to_string(),
                        source: SessionSource::Cli,
                        model_provider: Some("test-provider".to_string()),
                        ..Default::default()
                    },
                    git: None,
                }),
            };
            let contents = serde_json::to_string(&rollout_line).expect("rollout line");
            std::fs::write(&path, format!("{contents}\n")).expect("rollout file");
            path
        }
    }

    fn assert_detail(check: &DoctorCheck, label: &str, expected: &str) {
        let prefix = format!("{label}: ");
        let actual = check
            .details
            .iter()
            .find_map(|detail| detail.strip_prefix(&prefix))
            .expect("detail should exist");
        assert_eq!(actual, expected);
    }
}
