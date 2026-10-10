use crate::approval_events::RequestPermissionProfile;
use octopus_substrate_contract::approvals_substrate::NetworkApprovalProtocol;
use octopus_util_abspath::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GuardianRiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GuardianUserAuthorization {
    Unknown,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GuardianAssessmentStatus {
    InProgress,
    Approved,
    Denied,
    TimedOut,
    Aborted,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GuardianAssessmentDecisionSource {
    Agent,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GuardianCommandSource {
    Shell,
    UnifiedExec,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum GuardianAssessmentAction {
    Command {
        source: GuardianCommandSource,
        command: String,
        cwd: AbsolutePathBuf,
    },
    Execve {
        source: GuardianCommandSource,
        program: String,
        argv: Vec<String>,
        cwd: AbsolutePathBuf,
    },
    ApplyPatch {
        cwd: AbsolutePathBuf,
        files: Vec<AbsolutePathBuf>,
    },
    NetworkAccess {
        target: String,
        host: String,
        protocol: NetworkApprovalProtocol,
        port: u16,
    },
    McpToolCall {
        server: String,
        tool_name: String,
        connector_id: Option<String>,
        connector_name: Option<String>,
        tool_title: Option<String>,
    },
    RequestPermissions {
        reason: Option<String>,
        permissions: RequestPermissionProfile,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub(crate) struct GuardianAssessmentEvent {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_item_id: Option<String>,
    #[serde(default)]
    pub turn_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    pub status: GuardianAssessmentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<GuardianRiskLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_authorization: Option<GuardianUserAuthorization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_source: Option<GuardianAssessmentDecisionSource>,
    pub action: GuardianAssessmentAction,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guardian_event_wire_shape_is_byte_compatible() {
        let event = GuardianAssessmentEvent {
            id: "g-1".to_string(),
            target_item_id: None,
            turn_id: "t-1".to_string(),
            started_at_ms: 5,
            completed_at_ms: None,
            status: GuardianAssessmentStatus::Denied,
            risk_level: Some(GuardianRiskLevel::High),
            user_authorization: None,
            rationale: Some("unsafe".to_string()),
            decision_source: Some(GuardianAssessmentDecisionSource::Agent),
            action: GuardianAssessmentAction::Command {
                source: GuardianCommandSource::Shell,
                command: "rm -rf /".to_string(),
                cwd: octopus_util_abspath::AbsolutePathBuf::from_absolute_path("/tmp")
                    .expect("absolute"),
            },
        };
        let value = serde_json::to_value(&event).expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({
                "id": "g-1",
                "turn_id": "t-1",
                "started_at_ms": 5,
                "status": "denied",
                "risk_level": "high",
                "rationale": "unsafe",
                "decision_source": "agent",
                "action": {
                    "type": "command",
                    "source": "shell",
                    "command": "rm -rf /",
                    "cwd": "/tmp",
                },
            })
        );
        let decoded: GuardianAssessmentEvent =
            serde_json::from_value(value).expect("round-trip decode");
        assert_eq!(decoded, event);
    }
}
