#![forbid(unsafe_code)]

mod bundle;
mod capture;
mod redaction;
mod update;

pub use bundle::{
    AttachmentConsent, FeedbackCategory, LocalFeedbackRequest, SupportAttachmentKind,
    SupportAttachmentReceipt, SupportBundleError, SupportBundleReceipt, SupportBundleStatus,
    SupportContext, SupportPathState, SupportTag, create_local_feedback_bundle,
};
pub use capture::SupportLogCapture;
pub use redaction::{RedactedText, redact_support_text};
pub use update::{InstallerAuthorityStatus, UpdateDiagnostics, update_diagnostics};
