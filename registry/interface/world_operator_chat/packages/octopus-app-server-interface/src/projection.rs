use octopus_interfaces::{
    InterfaceBoundaryError as InterfaceDriverError, InterfaceInvocationEnvelope,
};

pub const APP_SERVER_INTERFACE_DRIVER_REF: &str = "app_server";

pub const INTERFACE_ARTIFACT_OUTPUT_LABEL: &str = "interface.artifact.output";

pub const COMMAND_EXECUTION_STARTED_LABEL: &str = "command_execution.started";

pub const COMMAND_EXECUTION_COMPLETED_LABEL: &str = "command_execution.completed";

pub const AGENT_MESSAGE_DELTA_LABEL: &str = "agent.message.delta";

pub const OPERATOR_APPROVAL_REQUEST_LABEL: &str = "operator.approval.request";

pub const OPERATOR_APPROVAL_RESPONSE_LABEL: &str = "operator.approval.response";

pub const OPERATOR_ELICITATION_REQUEST_LABEL: &str = "operator.elicitation.request";

pub const OPERATOR_ELICITATION_RESPONSE_LABEL: &str = "operator.elicitation.response";

pub const HOOK_RESULT_LABEL: &str = "hook.result";

pub const TOOL_CALL_LABEL: &str = "tool.call";

pub const TURN_EXEC_COMMAND_LABEL: &str = "turn.exec.command";

pub const TURN_REASONING_LABEL: &str = "turn.reasoning";

pub const TURN_TOKEN_USAGE_LABEL: &str = "turn.token_usage";

pub const TURN_PLAN_UPDATE_LABEL: &str = "turn.plan.update";

pub const TURN_WEB_SEARCH_LABEL: &str = "turn.web_search";

pub const TURN_USER_MESSAGE_LABEL: &str = "turn.user_message";

pub const OPERATOR_USER_INPUT_REQUEST_LABEL: &str = "operator.user_input.request";

pub const OPERATOR_USER_INPUT_RESPONSE_LABEL: &str = "operator.user_input.response";

pub const HOOK_CATALOG_LABEL: &str = "hook.catalog";

pub const GOAL_STATUS_LABEL: &str = "goal.status";

pub const ACCOUNT_RATE_LIMITS_LABEL: &str = "account.rate_limits";

pub const ACCOUNT_TOKEN_USAGE_LABEL: &str = "account.token_usage";

pub const TURN_ERROR_LABEL: &str = "turn.error";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorPermissionDecision {
    Accept,
    Decline,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorPermissionPersistence {
    Once,
    Session,
    Always,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorPermissionChoice {
    pub decision: OperatorPermissionDecision,
    pub persistence: OperatorPermissionPersistence,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorPermissionOffer {
    pub client_interface: octopus_descriptors::InterfaceId,
    pub offered_choices: Vec<OperatorPermissionChoice>,
}

impl OperatorPermissionOffer {
    pub fn validate(&self) -> Result<(), String> {
        let client = self.client_interface.as_str();
        if client.is_empty() || client.trim() != client || client.len() > 4096
            || client.contains('\0') || self.offered_choices.is_empty()
            || self.offered_choices.len() > 6
            || self.offered_choices.iter().enumerate().any(|(index, choice)| {
                choice.decision == OperatorPermissionDecision::Cancel
                    || self.offered_choices[..index].contains(choice)
            }) {
            return Err("operator permission offer has invalid client or duplicate/invalid choices".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorPermissionResponse {
    pub request_id: String,
    pub call_id: String,
    pub decision: OperatorPermissionDecision,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present_permission_persistence")]
    pub selected_persistence: Option<OperatorPermissionPersistence>,
}

fn present_permission_persistence<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<OperatorPermissionPersistence>, D::Error> {
    use serde::Deserialize;
    OperatorPermissionPersistence::deserialize(deserializer).map(Some)
}

impl OperatorPermissionResponse {
    pub fn validate(
        &self,
        request_id: &str,
        call_id: &str,
        offer: Option<&OperatorPermissionOffer>,
    ) -> Result<(), String> {
        if self.request_id != request_id || self.call_id != call_id {
            return Err("operator response differs from its exact original request/call".into());
        }
        if let Some(offer) = offer {
            offer.validate()?;
            if self.decision == OperatorPermissionDecision::Cancel {
                if self.selected_persistence.is_some() {
                    return Err("operator cancellation cannot select persistence".into());
                }
                return Ok(());
            }
            let persistence = self.selected_persistence.ok_or("operator response requires its offered persistence choice")?;
            if !offer.offered_choices.contains(&OperatorPermissionChoice { decision: self.decision, persistence }) {
                return Err("operator response selected a choice absent from the immutable offer".into());
            }
        } else if self.selected_persistence.is_some() {
            return Err("operator response selected persistence without an immutable offer".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceArtifactEnvelope {
    pub method: String,
    pub params: serde_json::Value,
}

impl InterfaceArtifactEnvelope {
    pub fn parse(bytes: &[u8]) -> Result<Self, InterfaceArtifactEnvelopeError> {
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
            InterfaceArtifactEnvelopeError::new(format!(
                "interface artifact payload is not valid JSON: {e}"
            ))
        })?;
        if value.get("projection").is_some() {
            return Err(InterfaceArtifactEnvelopeError::new(
                "interface artifact payload must not carry 'projection'; the descriptor-admitted artifact label is not a runtime intent classifier"
                    .to_string(),
            ));
        }
        let method = value
            .get("method")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                InterfaceArtifactEnvelopeError::new(
                    "interface artifact payload has no non-empty 'method' field".to_string(),
                )
            })?;
        let params = value
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        Ok(Self { method, params })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceArtifactEnvelopeError {
    message: String,
}

impl InterfaceArtifactEnvelopeError {
    fn new(message: String) -> Self {
        Self { message }
    }
}

impl std::fmt::Display for InterfaceArtifactEnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for InterfaceArtifactEnvelopeError {}

#[derive(Default)]
pub struct AppServerInterfaceDriver;

impl AppServerInterfaceDriver {
    pub fn new() -> Self {
        Self
    }
}

impl AppServerInterfaceDriver {
    pub fn interface_driver_ref(&self) -> &str {
        APP_SERVER_INTERFACE_DRIVER_REF
    }

    pub async fn receive_artifact(
        &self,
        input: InterfaceInvocationEnvelope,
    ) -> Result<(), InterfaceDriverError> {
        let input_artifact_bytes = input.read_artifact_payload()?;
        let envelope = parse_interface_envelope(&input_artifact_bytes)?;

        Err(InterfaceDriverError::FailedClosed(format!(
            "app_server single-shot receive_artifact has no run/gate/ledger: a '{}' input artifact \
             '{}' runs through the LIVE interface artifact arrival path \
             (binding/checkpoint → interface-owned artifact path; reply → a Movement), \
             driven by the runtime arrival boundary. Failing closed on the bare trait call so no \
             mark outcome is admitted on a non-answer.",
            envelope.method,
            input.artifact.id.as_str()
        )))
    }
}

pub fn method_is_command_execution(method: &str) -> bool {
    let noun = method.split('/').next().unwrap_or("");
    let verb = method.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    matches!(noun, "command" | "process") || (noun == "thread" && verb == "shellcommand")
}

fn parse_interface_envelope(
    input_artifact_bytes: &[u8],
) -> Result<InterfaceArtifactEnvelope, InterfaceDriverError> {
    InterfaceArtifactEnvelope::parse(input_artifact_bytes)
        .map_err(|e| InterfaceDriverError::Protocol(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::ArtifactId;
    use octopus_ledger::{ArtifactRef, ContentHash};
    use pretty_assertions::assert_eq;

    #[test]
    fn interface_driver_ref_is_the_app_server_family() {
        assert_eq!(
            AppServerInterfaceDriver::new().interface_driver_ref(),
            "app_server"
        );
        assert_eq!(APP_SERVER_INTERFACE_DRIVER_REF, "app_server");
    }

    #[test]
    fn parse_interface_envelope_rejects_payload_projection() {
        let parsed = parse_interface_envelope(
            br#"{"method":"thread/injectItems","params":{"items":[{"kind":"text","text":"hello"}]}}"#,
        )
        .unwrap();
        assert_eq!(parsed.method, "thread/injectItems");
        assert!(matches!(
            parse_interface_envelope(br#"{"params":{}}"#),
            Err(InterfaceDriverError::Protocol(_))
        ));
        assert!(matches!(
            parse_interface_envelope(br#"{"method":""}"#),
            Err(InterfaceDriverError::Protocol(_))
        ));
        assert!(matches!(
            parse_interface_envelope(br#"{"method":"thread/injectItems","projection":"maybe"}"#),
            Err(InterfaceDriverError::Protocol(_))
        ));
        assert!(matches!(
            parse_interface_envelope(b"not json"),
            Err(InterfaceDriverError::Protocol(_))
        ));
    }

    #[test]
    fn interface_payload_roundtrips_without_runtime_projection_field() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "method": "interface/familyMethod",
            "params": { "opaque": true }
        }))
        .unwrap();
        let parsed = InterfaceArtifactEnvelope::parse(&bytes).unwrap();
        assert_eq!(parsed.method, "interface/familyMethod");
        assert_eq!(parsed.params["opaque"], true);
    }

    #[test]
    fn exec_method_predicate_is_structural_over_command_surfaces() {
        assert!(method_is_command_execution("command/exec"));
        assert!(method_is_command_execution("process/spawn"));
        assert!(method_is_command_execution("thread/shellCommand"));
        assert!(!method_is_command_execution("thread/injectItems"));
        assert!(!method_is_command_execution("thread/steer"));
        assert!(!method_is_command_execution("review/start"));
        assert!(!method_is_command_execution("interface/list"));
    }

    fn driver_input(bytes: &[u8]) -> InterfaceInvocationEnvelope {
        let artifact = ArtifactRef {
            id: ArtifactId::new("a1"),
            content: ContentHash::of_bytes(bytes),
        };
        let artifact_file = octopus_interfaces::InterfaceArtifactFile::write(
            std::env::temp_dir().join("octopus-app-server-projection-test-artifacts"),
            "app-server.projection.request",
            bytes,
        )
        .expect("test interface artifact file writes");
        InterfaceInvocationEnvelope {
            artifact,
            artifact_file,
            artifact_binding_payloads: Vec::new(),
            metadata: None,
        }
    }

    #[tokio::test]
    async fn receive_artifact_fails_closed_until_arrival_path_is_wired() {
        let driver = AppServerInterfaceDriver::new();
        let res = driver
            .receive_artifact(driver_input(
                br#"{"method":"thread/injectItems","params":{}}"#,
            ))
            .await;
        assert!(
            matches!(res, Err(InterfaceDriverError::FailedClosed(_))),
            "unwired live path must fail closed, got {res:?}"
        );

        let bad = driver.receive_artifact(driver_input(b"not json")).await;
        assert!(matches!(bad, Err(InterfaceDriverError::Protocol(_))));
    }
}
