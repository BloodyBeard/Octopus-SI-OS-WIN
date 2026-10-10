#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ExecCommandItem {
    pub item_id: String,
    pub command: String,
    pub cwd: String,
    pub process_id: Option<String>,
    pub command_actions: Vec<CommandActionItem>,
    pub source: ExecItemSource,
    pub status: ExecItemStatus,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub aggregated_output: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExecItemSource {
    #[default]
    Agent,
    UserShell,
    UnifiedExecStartup,
    UnifiedExecInteraction,
}

impl ExecItemSource {
    fn from_str(source: &str) -> Self {
        match source {
            "userShell" | "user_shell" => Self::UserShell,
            "unifiedExecStartup" | "unified_exec_startup" => Self::UnifiedExecStartup,
            "unifiedExecInteraction" | "unified_exec_interaction" => Self::UnifiedExecInteraction,
            _ => Self::Agent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandActionItem {
    Read {
        cmd: String,
        name: String,
        path: String,
    },
    ListFiles {
        cmd: String,
        path: Option<String>,
    },
    Search {
        cmd: String,
        query: Option<String>,
        path: Option<String>,
    },
    Unknown {
        cmd: String,
    },
}

impl CommandActionItem {
    fn from_value(value: &serde_json::Value) -> Option<Self> {
        let object = value.as_object()?;
        let str_of = |key: &str| {
            object
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        };
        let cmd = str_of("cmd").unwrap_or_default();
        Some(
            match object
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
            {
                "read" => Self::Read {
                    cmd,
                    name: str_of("name").unwrap_or_default(),
                    path: str_of("path").unwrap_or_default(),
                },
                "list_files" => Self::ListFiles {
                    cmd,
                    path: str_of("path"),
                },
                "search" => Self::Search {
                    cmd,
                    query: str_of("query"),
                    path: str_of("path"),
                },
                _ => Self::Unknown { cmd },
            },
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExecItemStatus {
    #[default]
    InProgress,
    Completed,
    Failed,
    Declined,
}

impl ExecItemStatus {
    fn from_str(status: &str) -> Self {
        match status {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "declined" => Self::Declined,
            _ => Self::InProgress,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::InProgress => "running",
            Self::Completed => "ok",
            Self::Failed => "failed",
            Self::Declined => "declined",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ReasoningItem {
    pub item_id: String,
    pub summary: Vec<String>,
    pub content: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TokenUsageItem {
    pub total: TokenBreakdown,
    pub last: TokenBreakdown,
    pub model_context_window: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TokenBreakdown {
    pub total_tokens: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
}

impl TokenBreakdown {
    fn from_value(value: Option<&serde_json::Value>) -> Self {
        let Some(serde_json::Value::Object(map)) = value else {
            return Self::default();
        };
        let field = |key: &str| {
            map.get(key)
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
        };
        Self {
            total_tokens: field("total_tokens"),
            input_tokens: field("input_tokens"),
            cached_input_tokens: field("cached_input_tokens"),
            output_tokens: field("output_tokens"),
            reasoning_output_tokens: field("reasoning_output_tokens"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PlanUpdateItem {
    pub item_id: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ToolCallSurface {
    #[default]
    WorkerEnvironment,
    Dynamic,
    Mcp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ToolCallStatus {
    #[default]
    Dispatched,
    InProgress,
    Completed,
    Failed,
}

impl ToolCallStatus {
    fn from_str(status: &str) -> Self {
        match status {
            "started" | "inProgress" | "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Dispatched,
        }
    }

    pub fn is_complete(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ToolCallItem {
    pub tool: String,
    pub arguments: serde_json::Value,
    pub correlation_id: String,
    pub namespace: Option<String>,
    pub surface: ToolCallSurface,
    pub status: ToolCallStatus,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct UserMessageItem {
    pub item_id: String,
    pub items: Vec<crate::TerminalUserInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct WebSearchItem {
    pub item_id: String,
    pub query: String,
    pub action: Option<WebSearchActionItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebSearchActionItem {
    Search {
        query: Option<String>,
        queries: Vec<String>,
    },
    OpenPage {
        url: Option<String>,
    },
    FindInPage {
        url: Option<String>,
        pattern: Option<String>,
    },
    Other,
}

impl WebSearchItem {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichItemError> {
        let object = parse_object(bytes)?;
        let action = object
            .get("action")
            .and_then(serde_json::Value::as_object)
            .map(|action| {
                let str_of = |key: &str| {
                    action
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(ToOwned::to_owned)
                };
                match action
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                {
                    "search" => WebSearchActionItem::Search {
                        query: str_of("query"),
                        queries: string_array(action, "queries"),
                    },
                    "open_page" | "openPage" => {
                        WebSearchActionItem::OpenPage { url: str_of("url") }
                    }
                    "find_in_page" | "findInPage" => WebSearchActionItem::FindInPage {
                        url: str_of("url"),
                        pattern: str_of("pattern"),
                    },
                    _ => WebSearchActionItem::Other,
                }
            });
        Ok(Self {
            item_id: string_field(&object, "item_id"),
            query: string_field(&object, "query"),
            action,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RichItemError {
    #[error("rich turn-item payload is not a JSON object: {0}")]
    NotAnObject(String),
    #[error("rich turn-item payload has no non-empty '{0}' field")]
    MissingField(&'static str),
}

impl ExecCommandItem {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichItemError> {
        let object = parse_object(bytes)?;
        Ok(Self {
            item_id: string_field(&object, "item_id"),
            command: string_field(&object, "command"),
            cwd: string_field(&object, "cwd"),
            process_id: object
                .get("process_id")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
            command_actions: match object.get("command_actions") {
                Some(serde_json::Value::Array(items)) => items
                    .iter()
                    .filter_map(CommandActionItem::from_value)
                    .collect(),
                _ => Vec::new(),
            },
            source: ExecItemSource::from_str(&string_field(&object, "source")),
            status: ExecItemStatus::from_str(&string_field(&object, "status")),
            exit_code: object.get("exit_code").and_then(serde_json::Value::as_i64),
            duration_ms: object
                .get("duration_ms")
                .and_then(serde_json::Value::as_i64),
            aggregated_output: object
                .get("aggregated_output")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
        })
    }
}

impl ReasoningItem {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichItemError> {
        let object = parse_object(bytes)?;
        Ok(Self {
            item_id: string_field(&object, "item_id"),
            summary: string_array(&object, "summary"),
            content: string_array(&object, "content"),
        })
    }
}

impl TokenUsageItem {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichItemError> {
        let object = parse_object(bytes)?;
        Ok(Self {
            total: TokenBreakdown::from_value(object.get("total")),
            last: TokenBreakdown::from_value(object.get("last")),
            model_context_window: object
                .get("model_context_window")
                .and_then(serde_json::Value::as_i64),
        })
    }
}

impl PlanUpdateItem {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichItemError> {
        let object = parse_object(bytes)?;
        Ok(Self {
            item_id: string_field(&object, "item_id"),
            text: string_field(&object, "text"),
        })
    }
}

impl UserMessageItem {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichItemError> {
        let object = parse_object(bytes)?;
        let items = match object.get("items") {
            Some(serde_json::Value::Array(entries)) => entries
                .iter()
                .filter_map(|entry| {
                    serde_json::from_value::<crate::TerminalUserInput>(entry.clone()).ok()
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(Self {
            item_id: string_field(&object, "item_id"),
            items,
        })
    }
}

impl ToolCallItem {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichItemError> {
        let object = parse_object(bytes)?;
        Ok(Self {
            tool: string_field(&object, "tool"),
            arguments: object
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            correlation_id: string_field(&object, "correlation_id"),
            namespace: optional_string_field(&object, "namespace"),
            surface: ToolCallSurface::WorkerEnvironment,
            status: ToolCallStatus::Dispatched,
            result: None,
            error: None,
            duration_ms: None,
        })
    }

    pub fn from_dynamic_payload(bytes: &[u8]) -> Result<Self, RichItemError> {
        let object = parse_object(bytes)?;
        let status = ToolCallStatus::from_str(&string_field(&object, "status"));
        Ok(Self {
            tool: string_field(&object, "tool"),
            arguments: object
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            correlation_id: first_string_field(&object, &["call_id", "item_id", "id"]),
            namespace: optional_string_field(&object, "namespace"),
            surface: ToolCallSurface::Dynamic,
            status,
            result: object
                .get("body")
                .or_else(|| object.get("result"))
                .or_else(|| object.get("content_items"))
                .filter(|value| !value.is_null())
                .cloned(),
            error: error_text(object.get("error")),
            duration_ms: integer_field(&object, &["duration_ms", "durationMs"]),
        })
    }

    pub fn from_mcp_payload(bytes: &[u8]) -> Result<Self, RichItemError> {
        let object = parse_object(bytes)?;
        Ok(Self {
            tool: string_field(&object, "tool"),
            arguments: object
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            correlation_id: first_string_field(&object, &["id", "call_id", "item_id"]),
            namespace: optional_string_field(&object, "server"),
            surface: ToolCallSurface::Mcp,
            status: ToolCallStatus::from_str(&string_field(&object, "status")),
            result: object
                .get("result")
                .filter(|value| !value.is_null())
                .cloned(),
            error: error_text(object.get("error")),
            duration_ms: integer_field(&object, &["durationMs", "duration_ms"]),
        })
    }
}

fn parse_object(bytes: &[u8]) -> Result<serde_json::Map<String, serde_json::Value>, RichItemError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| RichItemError::NotAnObject(e.to_string()))?;
    match value {
        serde_json::Value::Object(map) => Ok(map),
        other => Err(RichItemError::NotAnObject(format!(
            "expected a JSON object, got {}",
            json_kind(&other)
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
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn first_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> String {
    keys.iter()
        .find_map(|key| optional_string_field(object, key))
        .unwrap_or_default()
}

fn integer_field(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<i64> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(serde_json::Value::as_i64))
}

fn error_text(value: Option<&serde_json::Value>) -> Option<String> {
    let value = value.filter(|value| !value.is_null())?;
    if let Some(text) = value.as_str() {
        return (!text.is_empty()).then(|| text.to_string());
    }
    if let Some(text) = value.get("message").and_then(serde_json::Value::as_str) {
        return (!text.is_empty()).then(|| text.to_string());
    }
    Some(value.to_string())
}

fn string_array(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    match object.get(key) {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_exec_command_item_from_payload() {
        let payload = serde_json::json!({
            "item_id": "exec-1",
            "command": "cargo test -p octopus-world-terminal-client",
            "cwd": "/work/repo",
            "process_id": "proc-7",
            "command_actions": [
                { "kind": "read", "cmd": "cat foo.rs", "name": "foo.rs", "path": "/work/repo/foo.rs" },
                { "kind": "search", "cmd": "rg lane", "query": "lane" },
                { "kind": "list_files", "cmd": "ls src" },
                { "kind": "weird", "cmd": "true" },
            ],
            "source": "agent",
            "status": "completed",
            "exit_code": 0,
            "duration_ms": 1234,
            "aggregated_output": "test result: ok. 12 passed",
        });
        let item = ExecCommandItem::from_payload(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(item.item_id, "exec-1");
        assert_eq!(item.command, "cargo test -p octopus-world-terminal-client");
        assert_eq!(item.cwd, "/work/repo");
        assert_eq!(item.process_id.as_deref(), Some("proc-7"));
        assert_eq!(
            item.command_actions,
            vec![
                CommandActionItem::Read {
                    cmd: "cat foo.rs".to_string(),
                    name: "foo.rs".to_string(),
                    path: "/work/repo/foo.rs".to_string(),
                },
                CommandActionItem::Search {
                    cmd: "rg lane".to_string(),
                    query: Some("lane".to_string()),
                    path: None,
                },
                CommandActionItem::ListFiles {
                    cmd: "ls src".to_string(),
                    path: None,
                },
                CommandActionItem::Unknown {
                    cmd: "true".to_string(),
                },
            ]
        );
        assert_eq!(item.source, ExecItemSource::Agent);
        assert_eq!(item.status, ExecItemStatus::Completed);
        assert_eq!(item.exit_code, Some(0));
        assert_eq!(item.duration_ms, Some(1234));
        assert_eq!(
            item.aggregated_output.as_deref(),
            Some("test result: ok. 12 passed")
        );
    }

    #[test]
    fn exec_item_defaults_unknown_source_and_status() {
        let inprogress = ExecCommandItem::from_payload(
            br#"{"command":"sleep 1","source":"weird","status":"weird"}"#,
        )
        .unwrap();
        assert_eq!(inprogress.source, ExecItemSource::Agent);
        assert_eq!(inprogress.status, ExecItemStatus::InProgress);
        assert!(inprogress.exit_code.is_none());
        assert!(inprogress.aggregated_output.is_none());

        assert_eq!(
            ExecItemSource::from_str("userShell"),
            ExecItemSource::UserShell
        );
        assert_eq!(
            ExecItemStatus::from_str("declined"),
            ExecItemStatus::Declined
        );
    }

    #[test]
    fn exec_item_non_object_fails_closed() {
        assert!(matches!(
            ExecCommandItem::from_payload(b"[1,2,3]"),
            Err(RichItemError::NotAnObject(_))
        ));
        assert!(matches!(
            ExecCommandItem::from_payload(b"not json"),
            Err(RichItemError::NotAnObject(_))
        ));
    }

    #[test]
    fn decodes_reasoning_item_from_payload() {
        let payload = serde_json::json!({
            "item_id": "reason-1",
            "summary": ["Plan the change", "Run the tests"],
            "content": ["First I will read the file…", 42, "then edit it"],
        });
        let item = ReasoningItem::from_payload(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(item.item_id, "reason-1");
        assert_eq!(item.summary, vec!["Plan the change", "Run the tests"]);
        assert_eq!(
            item.content,
            vec!["First I will read the file…", "then edit it"]
        );
    }

    #[test]
    fn decodes_token_usage_item_from_payload() {
        let payload = serde_json::json!({
            "total": {
                "total_tokens": 5000,
                "input_tokens": 4000,
                "cached_input_tokens": 1000,
                "output_tokens": 900,
                "reasoning_output_tokens": 100,
            },
            "last": { "total_tokens": 500, "input_tokens": 400, "output_tokens": 100 },
            "model_context_window": 272000,
        });
        let item = TokenUsageItem::from_payload(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(item.total.total_tokens, 5000);
        assert_eq!(item.total.cached_input_tokens, 1000);
        assert_eq!(item.last.input_tokens, 400);
        assert_eq!(item.last.reasoning_output_tokens, 0);
        assert_eq!(item.model_context_window, Some(272000));

        let empty = TokenUsageItem::from_payload(b"{}").unwrap();
        assert_eq!(empty.total, TokenBreakdown::default());
        assert!(empty.model_context_window.is_none());
    }

    #[test]
    fn decodes_plan_update_item_from_payload() {
        let item = PlanUpdateItem::from_payload(
            br#"{"item_id":"plan-1","text":"1. read\n2. edit\n3. test"}"#,
        )
        .unwrap();
        assert_eq!(item.item_id, "plan-1");
        assert!(item.text.contains("2. edit"));
    }

    #[test]
    fn decodes_tool_call_item_from_payload() {
        let payload = serde_json::json!({
            "tool": "coder.exec",
            "arguments": { "cmd": ["ls", "-la"] },
            "correlation_id": "call-9",
        });
        let item = ToolCallItem::from_payload(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(item.tool, "coder.exec");
        assert_eq!(item.arguments["cmd"][0], "ls");
        assert_eq!(item.correlation_id, "call-9");
        assert_eq!(item.surface, ToolCallSurface::WorkerEnvironment);
        assert_eq!(item.status, ToolCallStatus::Dispatched);
    }

    #[test]
    fn decodes_dynamic_and_mcp_tool_lifecycle_payloads() {
        let dynamic = ToolCallItem::from_dynamic_payload(
            br#"{"call_id":"call-7","namespace":"formed_interfaces","tool":"coder.exec","arguments":{"cmd":"pwd"},"status":"started"}"#,
        )
        .unwrap();
        assert_eq!(dynamic.correlation_id, "call-7");
        assert_eq!(dynamic.surface, ToolCallSurface::Dynamic);
        assert_eq!(dynamic.status, ToolCallStatus::InProgress);

        let mcp = ToolCallItem::from_mcp_payload(
            br#"{"type":"mcpToolCall","id":"mcp-1","server":"calendar","tool":"list","status":"completed","arguments":{},"result":{"content":[{"type":"text","text":"ok"}]},"durationMs":42}"#,
        )
        .unwrap();
        assert_eq!(mcp.namespace.as_deref(), Some("calendar"));
        assert_eq!(mcp.surface, ToolCallSurface::Mcp);
        assert_eq!(mcp.status, ToolCallStatus::Completed);
        assert_eq!(mcp.duration_ms, Some(42));
    }
}
