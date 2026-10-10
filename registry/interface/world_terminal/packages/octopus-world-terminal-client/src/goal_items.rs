pub const MAX_THREAD_GOAL_OBJECTIVE_CHARS: usize = 4_000;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GoalStatusItem {
    pub objective: String,
    pub status: GoalState,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GoalState {
    #[default]
    Unknown,
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

impl GoalState {
    pub fn from_str(status: &str) -> Self {
        match status {
            "active" => Self::Active,
            "paused" => Self::Paused,
            "blocked" => Self::Blocked,
            "usage_limited" | "usageLimited" => Self::UsageLimited,
            "budget_limited" | "budgetLimited" => Self::BudgetLimited,
            "complete" | "completed" => Self::Complete,
            _ => Self::Unknown,
        }
    }
}

impl GoalStatusItem {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, crate::rich_items::RichItemError> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| crate::rich_items::RichItemError::NotAnObject(e.to_string()))?;
        let serde_json::Value::Object(object) = value else {
            return Err(crate::rich_items::RichItemError::NotAnObject(
                "goal status payload is not a JSON object".to_string(),
            ));
        };
        let i64_field = |key: &str| {
            object
                .get(key)
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
        };
        Ok(Self {
            objective: object
                .get("objective")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            status: GoalState::from_str(
                object
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            ),
            token_budget: object
                .get("token_budget")
                .and_then(serde_json::Value::as_i64),
            tokens_used: i64_field("tokens_used"),
            time_used_seconds: i64_field("time_used_seconds"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_goal_status_from_payload() {
        let item = GoalStatusItem::from_payload(
            br#"{"objective":"ship it","status":"active","token_budget":50000,"tokens_used":12500,"time_used_seconds":90}"#,
        )
        .unwrap();
        assert_eq!(item.objective, "ship it");
        assert_eq!(item.status, GoalState::Active);
        assert_eq!(item.token_budget, Some(50000));
        assert_eq!(item.tokens_used, 12500);
    }

    #[test]
    fn unknown_status_claims_nothing() {
        let item = GoalStatusItem::from_payload(br#"{"status":"mystery"}"#).unwrap();
        assert_eq!(item.status, GoalState::Unknown);
        assert!(GoalStatusItem::from_payload(b"7").is_err());
    }
}
