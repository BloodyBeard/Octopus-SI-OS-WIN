use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use uuid::Uuid;

use crate::SupportLogCapture;
use crate::redact_support_text;

const MAX_TAGS: usize = 64;
const MAX_TAG_KEY_BYTES: usize = 48;
const MAX_TAG_VALUE_BYTES: usize = 128;
const MAX_NOTE_BYTES: usize = 16 * 1024;
const MAX_LOG_BYTES: usize = 512 * 1024;
const MAX_TRANSCRIPT_BYTES: usize = 2 * 1024 * 1024;
const MAX_BUNDLE_BYTES: usize = 5 * 1024 * 1024;
const MAX_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCategory {
    Bug,
    BadResult,
    GoodResult,
    SafetyConcern,
    Other,
}

impl FeedbackCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::BadResult => "bad_result",
            Self::GoodResult => "good_result",
            Self::SafetyConcern => "safety_concern",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct AttachmentConsent {
    pub diagnostics: bool,
    pub logs: bool,
    pub transcript: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SupportTag {
    pub key: String,
    pub value: String,
}

impl SupportTag {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SupportContext {
    pub client_version: String,
    pub target: String,
    pub run_id: Option<String>,
    pub interface_id: Option<String>,
    pub active_task: bool,
    pub octopus_home: PathBuf,
    pub durable_root: Option<PathBuf>,
    pub run_record_path: Option<PathBuf>,
    pub log_path: Option<PathBuf>,
    pub control_socket_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct LocalFeedbackRequest {
    pub category: FeedbackCategory,
    pub note: String,
    pub tags: Vec<SupportTag>,
    pub consent: AttachmentConsent,
    pub context: SupportContext,
    pub transcript: Option<String>,
    pub timeout: Duration,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportBundleStatus {
    LocalOnly,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportAttachmentKind {
    Diagnostics,
    Logs,
    Transcript,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SupportAttachmentReceipt {
    pub kind: SupportAttachmentKind,
    pub byte_count: usize,
    pub sha256: String,
    pub truncated: bool,
    pub redaction_count: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SupportBundleReceipt {
    pub schema_version: u32,
    pub receipt_id: Uuid,
    pub bundle_id: Uuid,
    pub bundle_path: PathBuf,
    pub status: SupportBundleStatus,
    pub upload_attempted: bool,
    pub category: FeedbackCategory,
    pub attachments: Vec<SupportAttachmentReceipt>,
    pub created_at_unix_ms: u64,
    pub elapsed_ms: u64,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportPathState {
    Missing,
    File,
    Directory,
    Other,
    Unreadable,
}

#[derive(Debug, thiserror::Error)]
pub enum SupportBundleError {
    #[error("feedback note must not be empty")]
    EmptyNote,
    #[error("feedback note exceeds {MAX_NOTE_BYTES} bytes")]
    NoteTooLarge,
    #[error("feedback has {actual} tags; at most {MAX_TAGS} are allowed")]
    TooManyTags { actual: usize },
    #[error("feedback tag {index} is invalid: {reason}")]
    InvalidTag { index: usize, reason: String },
    #[error("feedback timeout must be between 1 ms and {} ms", MAX_TIMEOUT.as_millis())]
    InvalidTimeout,
    #[error("local feedback bundle timed out after {timeout_ms} ms")]
    Timeout { timeout_ms: u64 },
    #[error("local feedback bundle worker failed: {0}")]
    Worker(String),
    #[error("local feedback bundle I/O failed at '{}': {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("local feedback bundle serialization failed: {0}")]
    Serialize(serde_json::Error),
    #[error("local feedback bundle exceeds {MAX_BUNDLE_BYTES} bytes")]
    BundleTooLarge,
}

#[derive(Serialize)]
struct LocalFeedbackBundle {
    schema_version: u32,
    receipt: SupportBundleReceipt,
    feedback: FeedbackBody,
    attachments: Vec<AttachmentBody>,
}

#[derive(Serialize)]
struct FeedbackBody {
    category: FeedbackCategory,
    note: String,
    tags: Vec<SupportTag>,
    consent: AttachmentConsent,
}

#[derive(Serialize)]
#[serde(tag = "kind", content = "content", rename_all = "snake_case")]
enum AttachmentBody {
    Diagnostics(SupportDiagnostics),
    Logs(String),
    Transcript(String),
}

#[derive(Serialize)]
struct SupportDiagnostics {
    schema_version: u32,
    client_version: String,
    target: String,
    run_id: Option<String>,
    interface_id: Option<String>,
    active_task: bool,
    paths: Vec<SupportPathDiagnostic>,
    environment_presence: BTreeMap<String, bool>,
}

#[derive(Serialize)]
struct SupportPathDiagnostic {
    name: String,
    path: String,
    state: SupportPathState,
    byte_count: Option<u64>,
}

pub async fn create_local_feedback_bundle(
    request: LocalFeedbackRequest,
) -> Result<SupportBundleReceipt, SupportBundleError> {
    validate_request(&request)?;
    let timeout = request.timeout;
    let timeout_ms = duration_ms(timeout);
    let deadline = Instant::now() + timeout;
    let work = tokio::task::spawn_blocking(move || build_bundle(request, deadline));
    match tokio::time::timeout(timeout, work).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(SupportBundleError::Worker(error.to_string())),
        Err(_) => Err(SupportBundleError::Timeout { timeout_ms }),
    }
}

fn validate_request(request: &LocalFeedbackRequest) -> Result<(), SupportBundleError> {
    if request.note.trim().is_empty() {
        return Err(SupportBundleError::EmptyNote);
    }
    if request.note.len() > MAX_NOTE_BYTES {
        return Err(SupportBundleError::NoteTooLarge);
    }
    if request.tags.len() > MAX_TAGS {
        return Err(SupportBundleError::TooManyTags {
            actual: request.tags.len(),
        });
    }
    for (index, tag) in request.tags.iter().enumerate() {
        let reason = if tag.key.trim().is_empty() {
            Some("key is empty".to_string())
        } else if tag.value.trim().is_empty() {
            Some("value is empty".to_string())
        } else if tag.key.len() > MAX_TAG_KEY_BYTES {
            Some(format!("key exceeds {MAX_TAG_KEY_BYTES} bytes"))
        } else if tag.value.len() > MAX_TAG_VALUE_BYTES {
            Some(format!("value exceeds {MAX_TAG_VALUE_BYTES} bytes"))
        } else if !tag
            .key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            Some("key contains unsupported characters".to_string())
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(SupportBundleError::InvalidTag { index, reason });
        }
    }
    if request.timeout.is_zero() || request.timeout > MAX_TIMEOUT {
        return Err(SupportBundleError::InvalidTimeout);
    }
    Ok(())
}

fn build_bundle(
    request: LocalFeedbackRequest,
    deadline: Instant,
) -> Result<SupportBundleReceipt, SupportBundleError> {
    let started = Instant::now();
    ensure_before_deadline(deadline, request.timeout)?;
    let created_at_unix_ms = unix_ms();
    let bundle_id = Uuid::new_v4();
    let receipt_id = Uuid::new_v4();
    let support_root = request.context.octopus_home.join("client").join("support");
    create_private_dir(&support_root)?;
    let filename = format!("feedback-{created_at_unix_ms}-{bundle_id}.json");
    let bundle_path = support_root.join(filename);
    let mut attachment_receipts = Vec::new();
    let mut attachments = Vec::new();

    if request.consent.diagnostics {
        ensure_before_deadline(deadline, request.timeout)?;
        let diagnostics = diagnostics(&request.context);
        let bytes = serde_json::to_vec(&diagnostics).map_err(SupportBundleError::Serialize)?;
        attachment_receipts.push(attachment_receipt(
            SupportAttachmentKind::Diagnostics,
            &bytes,
            false,
            0,
        ));
        attachments.push(AttachmentBody::Diagnostics(diagnostics));
    }

    if request.consent.logs {
        ensure_before_deadline(deadline, request.timeout)?;
        let (logs, truncated) = collect_logs(&request.context)?;
        let redacted = redact_support_text(&logs, request.context.octopus_home.to_str());
        attachment_receipts.push(attachment_receipt(
            SupportAttachmentKind::Logs,
            redacted.text.as_bytes(),
            truncated,
            redacted.redaction_count,
        ));
        attachments.push(AttachmentBody::Logs(redacted.text));
    }

    if request.consent.transcript {
        ensure_before_deadline(deadline, request.timeout)?;
        let source = request.transcript.as_deref().unwrap_or_default();
        let (transcript, truncated) = bounded_text_tail(source, MAX_TRANSCRIPT_BYTES);
        let redacted = redact_support_text(&transcript, request.context.octopus_home.to_str());
        attachment_receipts.push(attachment_receipt(
            SupportAttachmentKind::Transcript,
            redacted.text.as_bytes(),
            truncated,
            redacted.redaction_count,
        ));
        attachments.push(AttachmentBody::Transcript(redacted.text));
    }

    let receipt = SupportBundleReceipt {
        schema_version: 1,
        receipt_id,
        bundle_id,
        bundle_path: bundle_path.clone(),
        status: SupportBundleStatus::LocalOnly,
        upload_attempted: false,
        category: request.category,
        attachments: attachment_receipts,
        created_at_unix_ms,
        elapsed_ms: duration_ms(started.elapsed()),
        timeout_ms: duration_ms(request.timeout),
    };
    let bundle = LocalFeedbackBundle {
        schema_version: 1,
        receipt: receipt.clone(),
        feedback: FeedbackBody {
            category: request.category,
            note: request.note,
            tags: request.tags,
            consent: request.consent,
        },
        attachments,
    };
    let bytes = serde_json::to_vec_pretty(&bundle).map_err(SupportBundleError::Serialize)?;
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(SupportBundleError::BundleTooLarge);
    }
    ensure_before_deadline(deadline, request.timeout)?;
    persist_no_clobber(&support_root, &bundle_path, &bytes)?;
    Ok(receipt)
}

fn diagnostics(context: &SupportContext) -> SupportDiagnostics {
    let mut paths = Vec::new();
    paths.push(inspect_path(
        "octopus_home",
        &context.octopus_home,
        &context.octopus_home,
    ));
    if let Some(path) = &context.durable_root {
        paths.push(inspect_path("durable_root", path, &context.octopus_home));
        paths.push(inspect_path(
            "movement_ledger",
            &path.join("movements.jsonl"),
            &context.octopus_home,
        ));
    }
    if let Some(path) = &context.run_record_path {
        paths.push(inspect_path("run_record", path, &context.octopus_home));
    }
    if let Some(path) = &context.log_path {
        paths.push(inspect_path("tui_log", path, &context.octopus_home));
    }
    if let Some(path) = &context.control_socket_path {
        paths.push(inspect_path(
            "daemon_control_socket",
            path,
            &context.octopus_home,
        ));
    }
    let environment_presence = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "OCTOPUS_HOME",
        "SSH_CONNECTION",
        "TERM",
        "TMUX",
    ]
    .into_iter()
    .map(|name| (name.to_string(), std::env::var_os(name).is_some()))
    .collect();
    SupportDiagnostics {
        schema_version: 1,
        client_version: context.client_version.clone(),
        target: context.target.clone(),
        run_id: context.run_id.clone(),
        interface_id: context.interface_id.clone(),
        active_task: context.active_task,
        paths,
        environment_presence,
    }
}

fn inspect_path(name: &str, path: &Path, octopus_home: &Path) -> SupportPathDiagnostic {
    let display = path_hint(path, octopus_home);
    match std::fs::metadata(path) {
        Ok(metadata) => SupportPathDiagnostic {
            name: name.to_string(),
            path: display,
            state: if metadata.is_file() {
                SupportPathState::File
            } else if metadata.is_dir() {
                SupportPathState::Directory
            } else {
                SupportPathState::Other
            },
            byte_count: metadata.is_file().then_some(metadata.len()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => SupportPathDiagnostic {
            name: name.to_string(),
            path: display,
            state: SupportPathState::Missing,
            byte_count: None,
        },
        Err(_) => SupportPathDiagnostic {
            name: name.to_string(),
            path: display,
            state: SupportPathState::Unreadable,
            byte_count: None,
        },
    }
}

fn path_hint(path: &Path, octopus_home: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(octopus_home) {
        if relative.as_os_str().is_empty() {
            return "$OCTOPUS_HOME".to_string();
        }
        return format!("$OCTOPUS_HOME/{}", relative.display());
    }
    path.file_name()
        .map(|name| format!("$EXTERNAL/{}", name.to_string_lossy()))
        .unwrap_or_else(|| "$EXTERNAL".to_string())
}

fn collect_logs(context: &SupportContext) -> Result<(String, bool), SupportBundleError> {
    let mut bytes = SupportLogCapture::global().snapshot_tail(MAX_LOG_BYTES);
    let mut truncated = false;
    if let Some(path) = context.log_path.as_deref() {
        match read_file_tail(path, MAX_LOG_BYTES) {
            Ok((file_bytes, file_truncated)) => {
                if !file_bytes.is_empty() {
                    if !bytes.is_empty() {
                        bytes.extend_from_slice(b"\n");
                    }
                    bytes.extend_from_slice(&file_bytes);
                }
                truncated |= file_truncated;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(SupportBundleError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
    if bytes.len() > MAX_LOG_BYTES {
        bytes = bytes.split_off(bytes.len() - MAX_LOG_BYTES);
        truncated = true;
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

fn read_file_tail(path: &Path, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(limit as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes)?;
    Ok((bytes, start > 0))
}

fn bounded_text_tail(source: &str, limit: usize) -> (String, bool) {
    if source.len() <= limit {
        return (source.to_string(), false);
    }
    let mut start = source.len() - limit;
    while !source.is_char_boundary(start) {
        start += 1;
    }
    (source[start..].to_string(), true)
}

fn attachment_receipt(
    kind: SupportAttachmentKind,
    bytes: &[u8],
    truncated: bool,
    redaction_count: usize,
) -> SupportAttachmentReceipt {
    SupportAttachmentReceipt {
        kind,
        byte_count: bytes.len(),
        sha256: format!("{:x}", Sha256::digest(bytes)),
        truncated,
        redaction_count,
    }
}

fn create_private_dir(path: &Path) -> Result<(), SupportBundleError> {
    std::fs::create_dir_all(path).map_err(|source| SupportBundleError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(
            |source| SupportBundleError::Io {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(())
}

fn persist_no_clobber(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), SupportBundleError> {
    let mut temp = tempfile::Builder::new()
        .prefix(".feedback-")
        .tempfile_in(root)
        .map_err(|source| SupportBundleError::Io {
            path: root.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| SupportBundleError::Io {
                path: temp.path().to_path_buf(),
                source,
            })?;
    }
    temp.write_all(bytes)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|source| SupportBundleError::Io {
            path: temp.path().to_path_buf(),
            source,
        })?;
    temp.persist_noclobber(path)
        .map_err(|error| SupportBundleError::Io {
            path: path.to_path_buf(),
            source: error.error,
        })?;
    Ok(())
}

fn ensure_before_deadline(deadline: Instant, timeout: Duration) -> Result<(), SupportBundleError> {
    if Instant::now() >= deadline {
        return Err(SupportBundleError::Timeout {
            timeout_ms: duration_ms(timeout),
        });
    }
    Ok(())
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(home: &Path) -> LocalFeedbackRequest {
        LocalFeedbackRequest {
            category: FeedbackCategory::Bug,
            note: "The terminal rendered an unexpected state".to_string(),
            tags: vec![SupportTag::new("client", "world_terminal")],
            consent: AttachmentConsent {
                diagnostics: true,
                logs: false,
                transcript: true,
            },
            context: SupportContext {
                client_version: "1.2.3".to_string(),
                target: "local_daemon".to_string(),
                run_id: Some("run_test".to_string()),
                interface_id: Some("world_terminal_test".to_string()),
                active_task: true,
                octopus_home: home.to_path_buf(),
                durable_root: Some(home.join("ledger")),
                run_record_path: Some(home.join("runs/run_test.json")),
                log_path: None,
                control_socket_path: Some(home.join("daemon/control.sock")),
            },
            transcript: Some("User token=secret\nAssistant response".to_string()),
            timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn writes_local_only_bundle_with_explicit_attachments() {
        let home = tempfile::tempdir().unwrap();
        let receipt = create_local_feedback_bundle(request(home.path()))
            .await
            .unwrap();
        assert_eq!(receipt.status, SupportBundleStatus::LocalOnly);
        assert!(!receipt.upload_attempted);
        assert_eq!(receipt.attachments.len(), 2);
        let body = std::fs::read_to_string(&receipt.bundle_path).unwrap();
        assert!(body.contains("world_terminal"));
        assert!(!body.contains("token=secret"));
        assert!(body.contains("[REDACTED]"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&receipt.bundle_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn rejects_out_of_contract_tags_without_writing() {
        let home = tempfile::tempdir().unwrap();
        let mut many_tags = request(home.path());
        many_tags.tags = (0..65)
            .map(|index| SupportTag::new(format!("tag_{index}"), "value"))
            .collect();
        assert!(matches!(
            create_local_feedback_bundle(many_tags).await,
            Err(SupportBundleError::TooManyTags { actual: 65 })
        ));
        let mut malformed_tag = request(home.path());
        malformed_tag.tags = vec![SupportTag::new(" tag", "value")];
        assert!(matches!(
            create_local_feedback_bundle(malformed_tag).await,
            Err(SupportBundleError::InvalidTag { .. })
        ));
        assert!(!home.path().join("client/support").exists());
    }
}
