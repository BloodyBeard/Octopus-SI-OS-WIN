#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HookRunItem {
    pub id: String,
    pub event: String,
    pub status: HookItemStatus,
    pub status_message: Option<String>,
    pub outputs: Vec<HookOutputItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HookItemStatus {
    #[default]
    Running,
    Completed,
    Failed,
    Blocked,
    Stopped,
}

impl HookItemStatus {
    pub fn from_str(status: &str) -> Self {
        match status {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "blocked" => Self::Blocked,
            "stopped" => Self::Stopped,
            _ => Self::Running,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookOutputItem {
    pub kind: HookOutputKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HookOutputKind {
    #[default]
    Feedback,
    Warning,
    Stop,
    Context,
    Error,
}

impl HookOutputKind {
    pub fn from_str(kind: &str) -> Self {
        match kind {
            "warning" => Self::Warning,
            "stop" => Self::Stop,
            "context" => Self::Context,
            "error" => Self::Error,
            _ => Self::Feedback,
        }
    }
}

impl HookRunItem {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, crate::rich_items::RichItemError> {
        let object = parse_object(bytes)?;
        let mut outputs: Vec<HookOutputItem> = Vec::new();
        if let Some(serde_json::Value::Array(entries)) = object.get("outputs") {
            for entry in entries {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                let text = entry
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if text.is_empty() {
                    continue;
                }
                let kind = HookOutputKind::from_str(
                    entry
                        .get("kind")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(""),
                );
                outputs.push(HookOutputItem { kind, text });
            }
        }
        if outputs.is_empty()
            && let Some(message) = object.get("message").and_then(serde_json::Value::as_str)
            && !message.is_empty()
        {
            outputs.push(HookOutputItem {
                kind: HookOutputKind::Feedback,
                text: message.to_string(),
            });
        }
        Ok(Self {
            id: string_field(&object, "id"),
            event: string_field(&object, "event"),
            status: HookItemStatus::from_str(&string_field(&object, "status")),
            status_message: optional_string_field(&object, "status_message"),
            outputs,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HookCatalog {
    pub hooks: Vec<HookCatalogItem>,
    pub warnings: Vec<String>,
    pub errors: Vec<HookCatalogIssue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HookCatalogItem {
    pub key: String,
    pub event: String,
    pub matcher: Option<String>,
    pub command: Option<String>,
    pub timeout_sec: i64,
    pub source: String,
    pub source_path: String,
    pub plugin_id: Option<String>,
    pub display_order: i64,
    pub enabled: bool,
    pub is_managed: bool,
    pub current_hash: String,
    pub trust: HookTrustState,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HookCatalogIssue {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HookTrustState {
    Managed,
    Trusted,
    #[default]
    Untrusted,
    Modified,
}

impl HookTrustState {
    pub fn from_str(trust: &str) -> Self {
        match trust {
            "managed" => Self::Managed,
            "trusted" => Self::Trusted,
            "modified" => Self::Modified,
            _ => Self::Untrusted,
        }
    }
}

impl HookCatalogItem {
    pub fn needs_review(&self) -> bool {
        matches!(
            self.trust,
            HookTrustState::Untrusted | HookTrustState::Modified
        )
    }

    pub fn is_active(&self) -> bool {
        self.enabled
            && matches!(
                self.trust,
                HookTrustState::Managed | HookTrustState::Trusted
            )
    }
}

impl HookCatalog {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, crate::rich_items::RichItemError> {
        let object = parse_object(bytes)?;
        let mut hooks = Vec::new();
        if let Some(serde_json::Value::Array(entries)) = object.get("hooks") {
            for entry in entries {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                let key = string_field(entry, "key");
                if key.is_empty() {
                    continue;
                }
                hooks.push(HookCatalogItem {
                    key,
                    event: string_field(entry, "event"),
                    matcher: optional_string_field(entry, "matcher"),
                    command: optional_string_field(entry, "command"),
                    timeout_sec: entry
                        .get("timeout_sec")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0),
                    source: string_field(entry, "source"),
                    source_path: string_field(entry, "source_path"),
                    plugin_id: optional_string_field(entry, "plugin_id"),
                    display_order: entry
                        .get("display_order")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0),
                    enabled: entry
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    is_managed: entry
                        .get("is_managed")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    current_hash: string_field(entry, "current_hash"),
                    trust: HookTrustState::from_str(&string_field(entry, "trust")),
                });
            }
        }
        let warnings = match object.get("warnings") {
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect(),
            _ => Vec::new(),
        };
        let mut errors = Vec::new();
        if let Some(serde_json::Value::Array(items)) = object.get("errors") {
            for item in items {
                let Some(item) = item.as_object() else {
                    continue;
                };
                errors.push(HookCatalogIssue {
                    path: string_field(item, "path"),
                    message: string_field(item, "message"),
                });
            }
        }
        Ok(Self {
            hooks,
            warnings,
            errors,
        })
    }
}

pub const KNOWN_HOOK_EVENTS: &[(&str, &str)] = &[
    ("PreToolUse", "Before a tool executes"),
    ("PermissionRequest", "When permission is requested"),
    ("PostToolUse", "After a tool executes"),
    ("PreCompact", "Before context compaction"),
    ("PostCompact", "After context compaction"),
    ("SessionStart", "When a new session starts"),
    ("UserPromptSubmit", "When the user submits a prompt"),
    ("SubagentStart", "When a subagent is created"),
    ("SubagentStop", "Right before a subagent ends its turn"),
    ("Stop", "Right before the agent ends its turn"),
];

pub fn hook_event_names(catalog: &HookCatalog) -> Vec<String> {
    let mut names: Vec<String> = KNOWN_HOOK_EVENTS
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();
    for hook in &catalog.hooks {
        if !hook.event.is_empty() && !names.iter().any(|name| name == &hook.event) {
            names.push(hook.event.clone());
        }
    }
    names
}

pub fn hook_event_description(event: &str) -> &'static str {
    KNOWN_HOOK_EVENTS
        .iter()
        .find(|(name, _)| *name == event)
        .map(|(_, description)| *description)
        .unwrap_or("")
}

pub fn hook_source_label(source: &str) -> &'static str {
    match source {
        "system"
        | "mdm"
        | "cloudRequirements"
        | "legacyManagedConfigFile"
        | "legacyManagedConfigMdm" => "Admin config",
        "user" => "User config",
        "project" => "Project config",
        "sessionFlags" => "Session flags",
        "cloudManagedConfig" => "Cloud-managed config",
        "plugin" => "Configured hook source",
        _ => "Unknown source",
    }
}

fn parse_object(
    bytes: &[u8],
) -> Result<serde_json::Map<String, serde_json::Value>, crate::rich_items::RichItemError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| crate::rich_items::RichItemError::NotAnObject(e.to_string()))?;
    match value {
        serde_json::Value::Object(map) => Ok(map),
        other => Err(crate::rich_items::RichItemError::NotAnObject(format!(
            "expected a JSON object, got {other}"
        ))),
    }
}

fn string_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn optional_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_hook_run_item_from_payload() {
        let payload = serde_json::json!({
            "id": "hook-1",
            "event": "PostToolUse",
            "status": "blocked",
            "status_message": "checking output policy",
            "outputs": [
                {"kind": "warning", "text": "Heads up"},
                {"kind": "stop", "text": "Denied"},
                {"kind": "weird", "text": "still shown"},
            ],
        });
        let item = HookRunItem::from_payload(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(item.id, "hook-1");
        assert_eq!(item.event, "PostToolUse");
        assert_eq!(item.status, HookItemStatus::Blocked);
        assert_eq!(
            item.status_message.as_deref(),
            Some("checking output policy")
        );
        assert_eq!(item.outputs.len(), 3);
        assert_eq!(item.outputs[0].kind, HookOutputKind::Warning);
        assert_eq!(item.outputs[2].kind, HookOutputKind::Feedback);
    }

    #[test]
    fn thin_hook_result_message_becomes_feedback_output() {
        let item = HookRunItem::from_payload(
            br#"{"event":"PreToolUse","status":"completed","message":"ok"}"#,
        )
        .unwrap();
        assert_eq!(item.status, HookItemStatus::Completed);
        assert_eq!(item.outputs.len(), 1);
        assert_eq!(item.outputs[0].text, "ok");
    }

    #[test]
    fn hook_run_item_decode_edges() {
        assert!(HookRunItem::from_payload(b"[]").is_err());
        let odd = HookRunItem::from_payload(br#"{"event":"X","status":"weird"}"#).unwrap();
        assert_eq!(odd.status, HookItemStatus::Running);
    }

    #[test]
    fn decodes_hook_catalog_from_payload() {
        let payload = serde_json::json!({
            "hooks": [
                {
                    "key": "path:user",
                    "event": "PreToolUse",
                    "command": "~/bin/check.sh",
                    "timeout_sec": 30,
                    "source": "user",
                    "source_path": "/tmp/hooks.json",
                    "display_order": 1,
                    "enabled": true,
                    "current_hash": "sha256:x",
                    "trust": "trusted",
                },
                {"event": "PostToolUse"},
                {"key": "path:odd", "event": "PostToolUse", "trust": "who-knows"},
            ],
            "warnings": ["skipped invalid matcher"],
            "errors": [{"path": "/tmp/hooks.json", "message": "parse error"}],
        });
        let catalog = HookCatalog::from_payload(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(catalog.hooks.len(), 2, "keyless entry skipped");
        assert!(catalog.hooks[0].is_active());
        assert!(!catalog.hooks[0].needs_review());
        assert_eq!(catalog.hooks[1].trust, HookTrustState::Untrusted);
        assert!(catalog.hooks[1].needs_review());
        assert_eq!(catalog.warnings.len(), 1);
        assert_eq!(catalog.errors[0].message, "parse error");
    }

    #[test]
    fn event_names_union_known_and_catalog() {
        let catalog = HookCatalog {
            hooks: vec![HookCatalogItem {
                key: "k".into(),
                event: "MyCustomEvent".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let names = hook_event_names(&catalog);
        assert_eq!(names.len(), KNOWN_HOOK_EVENTS.len() + 1);
        assert_eq!(names.last().map(String::as_str), Some("MyCustomEvent"));
        assert_eq!(
            hook_event_description("PreToolUse"),
            "Before a tool executes"
        );
        assert_eq!(hook_event_description("MyCustomEvent"), "");
        assert_eq!(hook_source_label("user"), "User config");
        assert_eq!(hook_source_label("martian"), "Unknown source");
    }
}
