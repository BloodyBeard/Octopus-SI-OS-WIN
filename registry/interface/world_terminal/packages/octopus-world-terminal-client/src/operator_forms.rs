use crate::rich_items::RichItemError;

pub const APPROVAL_META_KIND_KEY: &str = "codex_approval_kind";
pub const APPROVAL_META_KIND_MCP_TOOL_CALL: &str = "mcp_tool_call";
pub const APPROVAL_META_PERSIST_KEY: &str = "persist";
pub const APPROVAL_META_PERSIST_SESSION: &str = "session";
pub const APPROVAL_META_PERSIST_ALWAYS: &str = "always";
pub const APPROVAL_META_TOOL_PARAMS_KEY: &str = "tool_params";
pub const APPROVAL_META_TOOL_PARAMS_DISPLAY_KEY: &str = "tool_params_display";

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ApprovalDetail {
    pub command: String,
    pub cwd: String,
    pub reason: Option<String>,
    pub environment_id: Option<String>,
    pub grant_root: Option<String>,
    pub network: Option<NetworkApprovalScope>,
    pub permissions: Option<PermissionScope>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct NetworkApprovalScope {
    pub host: String,
    pub protocol: String,
    pub port: Option<i64>,
}

impl NetworkApprovalScope {
    pub fn target(&self) -> String {
        if self.protocol.is_empty() {
            self.host.clone()
        } else {
            format!("{}://{}", self.protocol, self.host)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PermissionScope {
    pub network: bool,
    pub read: Vec<String>,
    pub write: Vec<String>,
    pub deny: Vec<String>,
}

impl PermissionScope {
    pub fn rule_line(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.network {
            parts.push("network".to_string());
        }
        let join = |paths: &[String]| {
            paths
                .iter()
                .map(|p| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if !self.read.is_empty() {
            parts.push(format!("read {}", join(&self.read)));
        }
        if !self.write.is_empty() {
            parts.push(format!("write {}", join(&self.write)));
        }
        if !self.deny.is_empty() {
            parts.push(format!("deny read {}", join(&self.deny)));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    pub fn is_empty(&self) -> bool {
        !self.network && self.read.is_empty() && self.write.is_empty() && self.deny.is_empty()
    }
}

impl ApprovalDetail {
    pub fn from_value(detail: &serde_json::Value) -> Self {
        let Some(object) = detail.as_object() else {
            return Self::default();
        };
        Self {
            command: command_field(object, "command"),
            cwd: str_field(object, "cwd"),
            reason: opt_str_field(object, "reason"),
            environment_id: opt_str_field_alias(object, &["environment_id", "environmentId"]),
            grant_root: opt_str_field_alias(object, &["grant_root", "grantRoot"]),
            network: object
                .get("network")
                .or_else(|| object.get("networkApprovalContext"))
                .and_then(|network| {
                    let network = network.as_object()?;
                    Some(NetworkApprovalScope {
                        host: str_field(network, "host"),
                        protocol: str_field(network, "protocol"),
                        port: network.get("port").and_then(serde_json::Value::as_i64),
                    })
                }),
            permissions: object
                .get("permissions")
                .or_else(|| object.get("additionalPermissions"))
                .and_then(|permissions| {
                    let permissions = permissions.as_object()?;
                    let scope = PermissionScope {
                        network: permissions
                            .get("network")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        read: str_array(permissions, "read"),
                        write: str_array(permissions, "write"),
                        deny: str_array(permissions, "deny"),
                    };
                    (!scope.is_empty()).then_some(scope)
                }),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalOptionItem {
    pub kind: String,
    pub label: Option<String>,
    pub command_prefix: Option<String>,
    pub decision: serde_json::Value,
}

pub fn approval_options_from_payload(bytes: &[u8]) -> Vec<ApprovalOptionItem> {
    let Ok(serde_json::Value::Object(object)) = serde_json::from_slice(bytes) else {
        return Vec::new();
    };
    approval_options_from_object(&object)
}

pub fn approval_options_from_detail(detail: &serde_json::Value) -> Vec<ApprovalOptionItem> {
    let Some(object) = detail.as_object() else {
        return Vec::new();
    };
    approval_decisions_from_object(object)
}

fn approval_options_from_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Vec<ApprovalOptionItem> {
    let Some(serde_json::Value::Array(options)) = object.get("options") else {
        return object
            .get("detail")
            .map(approval_options_from_detail)
            .unwrap_or_default();
    };
    options
        .iter()
        .filter_map(|option| {
            let option = option.as_object()?;
            let kind = str_field(option, "kind");
            let label = opt_str_field(option, "label");
            if kind.is_empty() && label.is_none() {
                return None;
            }
            Some(ApprovalOptionItem {
                kind,
                label,
                command_prefix: opt_str_field(option, "command_prefix"),
                decision: option
                    .get("decision")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            })
        })
        .collect()
}

fn approval_decisions_from_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Vec<ApprovalOptionItem> {
    let Some(serde_json::Value::Array(decisions)) = object
        .get("availableDecisions")
        .or_else(|| object.get("available_decisions"))
    else {
        return Vec::new();
    };
    decisions
        .iter()
        .filter_map(|decision| {
            let raw_kind = match decision {
                serde_json::Value::String(kind) => kind.as_str(),
                serde_json::Value::Object(object) => object.keys().next()?.as_str(),
                _ => return None,
            };
            let kind = match raw_kind {
                "acceptForSession" | "approved_for_session" => "accept_for_session",
                "acceptWithExecpolicyAmendment" => "accept_for_prefix",
                "applyNetworkPolicyAmendment" => "allow_host",
                "approved" => "accept",
                "denied" => "decline",
                "abort" => "cancel",
                other => other,
            };
            Some(ApprovalOptionItem {
                kind: kind.to_string(),
                label: None,
                command_prefix: decision
                    .pointer("/acceptWithExecpolicyAmendment/execpolicyAmendment/command")
                    .or_else(|| {
                        decision
                            .pointer("/acceptWithExecpolicyAmendment/execpolicy_amendment/command")
                    })
                    .and_then(command_value),
                decision: decision.clone(),
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct ElicitationFormField {
    pub id: String,
    pub label: String,
    pub prompt: String,
    pub required: bool,
    pub input: ElicitationFieldInput,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ElicitationFieldInput {
    Select {
        options: Vec<ElicitationOption>,
        default_idx: Option<usize>,
    },
    Text {
        secret: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ElicitationOption {
    pub label: String,
    pub description: Option<String>,
    pub value: serde_json::Value,
}

pub fn parse_elicitation_fields(
    requested_schema: &serde_json::Value,
) -> Option<Vec<ElicitationFormField>> {
    let schema = requested_schema.as_object()?;
    if schema.get("type").and_then(serde_json::Value::as_str) != Some("object") {
        return None;
    }
    let required: Vec<String> = str_array(schema, "required");
    let properties = schema.get("properties")?.as_object()?;
    let mut fields = Vec::new();
    for (id, property) in properties {
        fields.push(parse_elicitation_field(
            id,
            property,
            required.iter().any(|r| r == id),
        )?);
    }
    (!fields.is_empty()).then_some(fields)
}

fn parse_elicitation_field(
    id: &str,
    property: &serde_json::Value,
    required: bool,
) -> Option<ElicitationFormField> {
    let property = property.as_object()?;
    let label = opt_str_field(property, "title").unwrap_or_else(|| id.to_string());
    let prompt = opt_str_field(property, "description").unwrap_or_else(|| label.clone());
    let field_type = property
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    if field_type == "string"
        && let Some(serde_json::Value::Array(one_of)) = property.get("oneOf")
    {
        let default = opt_str_field(property, "default");
        let mut options = Vec::new();
        for entry in one_of {
            let entry = entry.as_object()?;
            let value = entry.get("const").and_then(serde_json::Value::as_str)?;
            let title = opt_str_field(entry, "title").unwrap_or_else(|| value.to_string());
            options.push(ElicitationOption {
                label: title,
                description: None,
                value: serde_json::Value::String(value.to_string()),
            });
        }
        let default_idx = default.as_deref().and_then(|default| {
            options
                .iter()
                .position(|option| option.value.as_str() == Some(default))
        });
        return Some(ElicitationFormField {
            id: id.to_string(),
            label,
            prompt,
            required,
            input: ElicitationFieldInput::Select {
                options,
                default_idx,
            },
        });
    }

    if field_type == "string"
        && let Some(serde_json::Value::Array(values)) = property.get("enum")
    {
        let names: Vec<String> = str_array(property, "enumNames");
        let default = opt_str_field(property, "default");
        let mut options = Vec::new();
        for (idx, value) in values.iter().enumerate() {
            let value = value.as_str()?;
            options.push(ElicitationOption {
                label: names.get(idx).cloned().unwrap_or_else(|| value.to_string()),
                description: None,
                value: serde_json::Value::String(value.to_string()),
            });
        }
        let default_idx = default.as_deref().and_then(|default| {
            options
                .iter()
                .position(|option| option.value.as_str() == Some(default))
        });
        return Some(ElicitationFormField {
            id: id.to_string(),
            label,
            prompt,
            required,
            input: ElicitationFieldInput::Select {
                options,
                default_idx,
            },
        });
    }

    match field_type {
        "string" => Some(ElicitationFormField {
            id: id.to_string(),
            label,
            prompt,
            required,
            input: ElicitationFieldInput::Text { secret: false },
        }),
        "boolean" => {
            let default_idx = property
                .get("default")
                .and_then(serde_json::Value::as_bool)
                .map(|value| if value { 0 } else { 1 });
            let options = [true, false]
                .into_iter()
                .map(|value| ElicitationOption {
                    label: if value { "True" } else { "False" }.to_string(),
                    description: None,
                    value: serde_json::Value::Bool(value),
                })
                .collect();
            Some(ElicitationFormField {
                id: id.to_string(),
                label,
                prompt,
                required,
                input: ElicitationFieldInput::Select {
                    options,
                    default_idx,
                },
            })
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UserInputRequest {
    pub request_id: String,
    pub questions: Vec<UserInputQuestion>,
    pub is_blocking: bool,
    pub auto_resolution_ms: Option<u64>,
}

impl Default for UserInputRequest {
    fn default() -> Self {
        Self {
            request_id: String::new(),
            questions: Vec::new(),
            is_blocking: true,
            auto_resolution_ms: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub is_other: bool,
    pub is_secret: bool,
    pub options: Vec<UserInputOption>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct UserInputOption {
    pub label: String,
    pub description: String,
}

impl UserInputRequest {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichItemError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| RichItemError::NotAnObject(e.to_string()))?;
        let serde_json::Value::Object(object) = value else {
            return Err(RichItemError::NotAnObject(
                "user-input request payload is not a JSON object".to_string(),
            ));
        };
        let request_id = object
            .get("request_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
            .ok_or(RichItemError::MissingField("request_id"))?;
        let mut questions = Vec::new();
        if let Some(serde_json::Value::Array(entries)) = object.get("questions") {
            for entry in entries {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                let id = str_field(entry, "id");
                if id.is_empty() {
                    continue;
                }
                let options = match entry.get("options") {
                    Some(serde_json::Value::Array(options)) => options
                        .iter()
                        .filter_map(|option| {
                            let option = option.as_object()?;
                            Some(UserInputOption {
                                label: str_field(option, "label"),
                                description: str_field(option, "description"),
                            })
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                questions.push(UserInputQuestion {
                    id,
                    header: str_field(entry, "header"),
                    question: str_field(entry, "question"),
                    is_other: entry
                        .get("is_other")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    is_secret: entry
                        .get("is_secret")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    options,
                });
            }
        }
        Ok(Self {
            request_id,
            questions,
            is_blocking: object
                .get("is_blocking")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            auto_resolution_ms: object
                .get("auto_resolution_ms")
                .and_then(serde_json::Value::as_u64),
        })
    }
}

pub fn emit_user_input_response(
    artifact_root: impl AsRef<std::path::Path>,
    request_id: &str,
    answers: std::collections::BTreeMap<String, Vec<String>>,
) -> Result<crate::emit_input::TerminalInputArtifact, crate::rich_requests::RichRequestError> {
    let response = serde_json::json!({
        "request_id": request_id,
        "answers": answers,
    });
    crate::rich_requests::emit_operator_response(
        artifact_root.as_ref(),
        octopus_app_server_interface::OPERATOR_USER_INPUT_RESPONSE_LABEL,
        "operator_user_input_response",
        &format!("operator answers for user-input request '{request_id}'"),
        response,
    )
}

#[derive(Clone)]
pub struct OperatorResponder {
    artifact_root: std::path::PathBuf,
    emitter: std::sync::Arc<crate::daemon_arrival::TerminalDaemonEmitter>,
}

impl OperatorResponder {
    pub fn new(
        artifact_root: std::path::PathBuf,
        emitter: crate::daemon_arrival::TerminalDaemonEmitter,
    ) -> Self {
        Self {
            artifact_root,
            emitter: std::sync::Arc::new(emitter),
        }
    }

    pub async fn respond_approval(
        &self,
        request_id: &str,
        decision: serde_json::Value,
    ) -> Result<u64, crate::rich_requests::RichRequestError> {
        let built = crate::rich_requests::emit_approval_response(
            &self.artifact_root,
            request_id,
            decision,
        )?;
        self.hand_in(built).await
    }

    pub async fn respond_elicitation(
        &self,
        request_id: &str,
        answer: serde_json::Value,
    ) -> Result<u64, crate::rich_requests::RichRequestError> {
        let built = crate::rich_requests::emit_elicitation_response(
            &self.artifact_root,
            request_id,
            answer,
        )?;
        self.hand_in(built).await
    }

    pub async fn respond_user_input(
        &self,
        request_id: &str,
        answers: std::collections::BTreeMap<String, Vec<String>>,
    ) -> Result<u64, crate::rich_requests::RichRequestError> {
        let built = emit_user_input_response(&self.artifact_root, request_id, answers)?;
        self.hand_in(built).await
    }

    async fn hand_in(
        &self,
        built: crate::emit_input::TerminalInputArtifact,
    ) -> Result<u64, crate::rich_requests::RichRequestError> {
        let verdict = self
            .emitter
            .emit_input_manifest(&built.manifest_bytes)
            .await?;
        let octopus_interfaces::DeliveryVerdict::Admitted { seq } = verdict else {
            unreachable!("the terminal daemon emitter converts refused verdicts into errors")
        };
        Ok(seq)
    }
}

fn str_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn opt_str_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn opt_str_field_alias(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| opt_str_field(object, key))
}

fn command_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    object.get(key).and_then(command_value).unwrap_or_default()
}

fn command_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(command) if !command.is_empty() => Some(command.clone()),
        serde_json::Value::Array(argv) => {
            let command = argv
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            (!command.is_empty()).then_some(command)
        }
        _ => None,
    }
}

fn str_array(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    match object.get(key) {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_approval_detail_and_rule_line() {
        let detail = serde_json::json!({
            "command": "rm -rf /tmp/x",
            "cwd": "/work",
            "reason": "cleanup",
            "environment_id": "env-1",
            "network": {"host": "example.com", "protocol": "https"},
            "permissions": {"network": true, "read": ["/a"], "write": ["/b"], "deny": ["/c"]},
        });
        let decoded = ApprovalDetail::from_value(&detail);
        assert_eq!(decoded.command, "rm -rf /tmp/x");
        assert_eq!(decoded.cwd, "/work");
        assert_eq!(decoded.reason.as_deref(), Some("cleanup"));
        assert_eq!(
            decoded.network.as_ref().map(NetworkApprovalScope::target),
            Some("https://example.com".to_string())
        );
        assert_eq!(
            decoded.permissions.unwrap().rule_line().as_deref(),
            Some("network; read `/a`; write `/b`; deny read `/c`")
        );
        assert_eq!(
            ApprovalDetail::from_value(&serde_json::Value::Null),
            ApprovalDetail::default()
        );
    }

    #[test]
    fn decodes_approval_options() {
        let payload = serde_json::json!({
            "request_id": "r1",
            "options": [
                {"kind": "accept", "decision": "accept"},
                {"kind": "accept_for_prefix", "command_prefix": "cargo test",
                 "decision": {"accept_with_execpolicy_amendment": {"command": ["cargo", "test"]}}},
                {"label": "Custom", "decision": "odd"},
                {"decision": "skipped"},
            ],
        });
        let options = approval_options_from_payload(&serde_json::to_vec(&payload).unwrap());
        assert_eq!(options.len(), 3);
        assert_eq!(options[0].kind, "accept");
        assert_eq!(options[1].command_prefix.as_deref(), Some("cargo test"));
        assert!(options[1].decision["accept_with_execpolicy_amendment"].is_object());
        assert_eq!(options[2].label.as_deref(), Some("Custom"));
        assert!(approval_options_from_payload(br#"{"request_id":"r1"}"#).is_empty());
    }

    #[test]
    fn parses_elicitation_fields_from_schema_json() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["env"],
            "properties": {
                "env": {"type": "string", "enum": ["dev", "prod"], "enumNames": ["Development", "Production"], "default": "prod"},
                "confirm": {"type": "boolean", "title": "Confirm?", "default": false},
                "note": {"type": "string", "description": "Anything else"},
                "pick": {"type": "string", "oneOf": [{"const": "a", "title": "Alpha"}, {"const": "b", "title": "Beta"}]},
            },
        });
        let fields = parse_elicitation_fields(&schema).expect("form parses");
        assert_eq!(fields.len(), 4);
        let env = fields.iter().find(|f| f.id == "env").unwrap();
        assert!(env.required);
        let ElicitationFieldInput::Select {
            options,
            default_idx,
        } = &env.input
        else {
            panic!("enum is a select");
        };
        assert_eq!(options[0].label, "Development");
        assert_eq!(*default_idx, Some(1));
        let confirm = fields.iter().find(|f| f.id == "confirm").unwrap();
        let ElicitationFieldInput::Select { default_idx, .. } = &confirm.input else {
            panic!("boolean is a select");
        };
        assert_eq!(*default_idx, Some(1), "default false selects False");
        let note = fields.iter().find(|f| f.id == "note").unwrap();
        assert!(matches!(
            note.input,
            ElicitationFieldInput::Text { secret: false }
        ));
        assert_eq!(note.prompt, "Anything else");
        let pick = fields.iter().find(|f| f.id == "pick").unwrap();
        let ElicitationFieldInput::Select { options, .. } = &pick.input else {
            panic!("oneOf is a select");
        };
        assert_eq!(options[1].label, "Beta");
        assert_eq!(options[1].value, serde_json::json!("b"));

        let unsupported = serde_json::json!({
            "type": "object",
            "properties": {"n": {"type": "number"}},
        });
        assert!(parse_elicitation_fields(&unsupported).is_none());
        assert!(parse_elicitation_fields(&serde_json::Value::Null).is_none());
    }

    #[test]
    fn decodes_user_input_request_and_emits_response() {
        let payload = serde_json::json!({
            "request_id": "ui-1",
            "questions": [
                {"id": "q1", "header": "Choose", "question": "Which one?", "is_other": true,
                 "options": [{"label": "A", "description": "first"}, {"label": "B", "description": "second"}]},
                {"header": "no id — skipped"},
            ],
            "is_blocking": false,
            "auto_resolution_ms": 5000,
        });
        let request =
            UserInputRequest::from_payload(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(request.request_id, "ui-1");
        assert_eq!(request.questions.len(), 1);
        assert!(request.questions[0].is_other);
        assert_eq!(request.questions[0].options[1].label, "B");
        assert!(!request.is_blocking);
        assert_eq!(request.auto_resolution_ms, Some(5000));
        let legacy = UserInputRequest::from_payload(br#"{"request_id":"legacy"}"#).unwrap();
        assert!(legacy.is_blocking);
        assert!(UserInputRequest::from_payload(b"[]").is_err());

        let dir = tempfile::tempdir().unwrap();
        let mut answers = std::collections::BTreeMap::new();
        answers.insert("q1".to_string(), vec!["A".to_string()]);
        let built = emit_user_input_response(dir.path(), "ui-1", answers).unwrap();
        let manifest = built.artifact_file.read_manifest().unwrap();
        assert_eq!(
            manifest.label,
            octopus_app_server_interface::OPERATOR_USER_INPUT_RESPONSE_LABEL
        );
        let body: serde_json::Value =
            serde_json::from_slice(&built.artifact_file.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(body["operator_user_input_response"]["request_id"], "ui-1");
        assert_eq!(
            body["operator_user_input_response"]["answers"]["q1"][0],
            "A"
        );
    }
}
