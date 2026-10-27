use serde::Deserialize;
use serde::Serialize;

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StepStatus {
    Pending,
    InProgress,
    Completed,
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanItemArg {
    pub step: String,
    pub status: StepStatus,
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdatePlanArgs {
    #[serde(default)]
    pub explanation: Option<String>,
    pub plan: Vec<PlanItemArg>,
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessagePhase {
    Commentary,
    PartialAnswer,
    FinalAnswer,
}

#[cfg(test)]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub(crate) enum AgentMessageContent {
    Text { text: String },
}

#[cfg(test)]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct AgentMessageItem {
    pub id: String,
    pub content: Vec<AgentMessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<MessagePhase>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallToolResult {
    pub content: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_item_wire_shapes_are_byte_compatible() {
        let plan = UpdatePlanArgs {
            explanation: None,
            plan: vec![PlanItemArg {
                step: "seal the edge".to_string(),
                status: StepStatus::InProgress,
            }],
        };
        assert_eq!(
            serde_json::to_value(&plan).expect("serialize plan"),
            serde_json::json!({
                "explanation": null,
                "plan": [{"step": "seal the edge", "status": "in_progress"}],
            })
        );

        let message = AgentMessageItem {
            id: "item-1".to_string(),
            content: vec![AgentMessageContent::Text {
                text: "done".to_string(),
            }],
            phase: Some(MessagePhase::FinalAnswer),
        };
        assert_eq!(
            serde_json::to_value(&message).expect("serialize message"),
            serde_json::json!({
                "id": "item-1",
                "content": [{"type": "Text", "text": "done"}],
                "phase": "final_answer",
            })
        );

        let partial: AgentMessageItem = serde_json::from_value(serde_json::json!({
            "id": "item-2",
            "content": [{"type": "Text", "text": "More work follows"}],
            "phase": "partial_answer",
        }))
        .expect("partial answers decode from the provider wire");
        assert_eq!(partial.phase, Some(MessagePhase::PartialAnswer));
        assert_eq!(
            serde_json::to_value(partial).unwrap()["phase"],
            "partial_answer"
        );

        let result = CallToolResult {
            content: vec![serde_json::json!({"type": "text", "text": "ok"})],
            structured_content: None,
            is_error: Some(false),
            meta: Some(serde_json::json!({"k": 1})),
        };
        assert_eq!(
            serde_json::to_value(&result).expect("serialize result"),
            serde_json::json!({
                "content": [{"type": "text", "text": "ok"}],
                "isError": false,
                "_meta": {"k": 1},
            })
        );
    }
}
