use crate::rich_items::RichItemError;

#[derive(Clone, Debug, PartialEq, Default)]
pub struct RateLimitSnapshotItem {
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    pub primary: Option<RateLimitWindowItem>,
    pub secondary: Option<RateLimitWindowItem>,
    pub credits: Option<CreditsItem>,
    pub individual_limit: Option<SpendLimitItem>,
    pub plan_type: Option<String>,
    pub rate_limit_reached: Option<RateLimitReachedKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RateLimitReachedKind {
    #[default]
    RateLimitReached,
    WorkspaceOwnerCreditsDepleted,
    WorkspaceMemberCreditsDepleted,
    WorkspaceOwnerUsageLimitReached,
    WorkspaceMemberUsageLimitReached,
}

impl RateLimitReachedKind {
    pub fn from_str(kind: &str) -> Self {
        match kind {
            "workspace_owner_credits_depleted" => Self::WorkspaceOwnerCreditsDepleted,
            "workspace_member_credits_depleted" => Self::WorkspaceMemberCreditsDepleted,
            "workspace_owner_usage_limit_reached" => Self::WorkspaceOwnerUsageLimitReached,
            "workspace_member_usage_limit_reached" => Self::WorkspaceMemberUsageLimitReached,
            _ => Self::RateLimitReached,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct RateLimitWindowItem {
    pub used_percent: f64,
    pub window_minutes: Option<i64>,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CreditsItem {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SpendLimitItem {
    pub remaining_percent: f64,
    pub used: String,
    pub limit: String,
    pub resets_at: Option<i64>,
}

pub fn rate_limit_snapshots_from_payload(
    bytes: &[u8],
) -> Result<Vec<RateLimitSnapshotItem>, RichItemError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| RichItemError::NotAnObject(e.to_string()))?;
    let serde_json::Value::Object(object) = value else {
        return Err(RichItemError::NotAnObject(
            "rate-limits payload is not a JSON object".to_string(),
        ));
    };
    match object.get("snapshots") {
        Some(serde_json::Value::Array(entries)) => Ok(entries
            .iter()
            .filter_map(serde_json::Value::as_object)
            .map(snapshot_from_object)
            .collect()),
        _ => Ok(vec![snapshot_from_object(&object)]),
    }
}

fn snapshot_from_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> RateLimitSnapshotItem {
    RateLimitSnapshotItem {
        limit_id: opt_str_field(object, "limit_id"),
        limit_name: opt_str_field(object, "limit_name"),
        primary: object
            .get("primary")
            .and_then(serde_json::Value::as_object)
            .map(window_from_object),
        secondary: object
            .get("secondary")
            .and_then(serde_json::Value::as_object)
            .map(window_from_object),
        credits: object
            .get("credits")
            .and_then(serde_json::Value::as_object)
            .map(|credits| CreditsItem {
                has_credits: bool_field(credits, "has_credits"),
                unlimited: bool_field(credits, "unlimited"),
                balance: credits
                    .get("balance")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
            }),
        individual_limit: object
            .get("individual_limit")
            .and_then(serde_json::Value::as_object)
            .map(|limit| SpendLimitItem {
                remaining_percent: f64_field(limit, "remaining_percent"),
                used: str_field(limit, "used"),
                limit: str_field(limit, "limit"),
                resets_at: limit.get("resets_at").and_then(serde_json::Value::as_i64),
            }),
        plan_type: opt_str_field(object, "plan_type"),
        rate_limit_reached: object
            .get("rate_limit_reached")
            .and_then(serde_json::Value::as_str)
            .map(RateLimitReachedKind::from_str),
    }
}

fn opt_str_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn window_from_object(object: &serde_json::Map<String, serde_json::Value>) -> RateLimitWindowItem {
    RateLimitWindowItem {
        used_percent: f64_field(object, "used_percent"),
        window_minutes: object
            .get("window_minutes")
            .and_then(serde_json::Value::as_i64),
        resets_at: object.get("resets_at").and_then(serde_json::Value::as_i64),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AccountUsageReport {
    pub daily: Vec<AccountUsageBucket>,
    pub summary: Option<AccountUsageSummary>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AccountUsageSummary {
    pub lifetime_tokens: Option<i64>,
    pub peak_daily_tokens: Option<i64>,
    pub current_streak_days: Option<i64>,
    pub longest_streak_days: Option<i64>,
    pub longest_running_turn_sec: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AccountUsageBucket {
    pub bucket_start_seconds: i64,
    pub total_tokens: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
}

impl AccountUsageReport {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, RichItemError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| RichItemError::NotAnObject(e.to_string()))?;
        let serde_json::Value::Object(object) = value else {
            return Err(RichItemError::NotAnObject(
                "account usage payload is not a JSON object".to_string(),
            ));
        };
        let mut daily = Vec::new();
        if let Some(serde_json::Value::Array(entries)) = object.get("daily") {
            for entry in entries {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                let field = |key: &str| {
                    entry
                        .get(key)
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0)
                };
                daily.push(AccountUsageBucket {
                    bucket_start_seconds: field("bucket_start_seconds"),
                    total_tokens: field("total_tokens"),
                    input_tokens: field("input_tokens"),
                    cached_input_tokens: field("cached_input_tokens"),
                    output_tokens: field("output_tokens"),
                    reasoning_output_tokens: field("reasoning_output_tokens"),
                });
            }
        }
        let summary = object
            .get("summary")
            .and_then(serde_json::Value::as_object)
            .map(|summary| {
                let field = |key: &str| summary.get(key).and_then(serde_json::Value::as_i64);
                AccountUsageSummary {
                    lifetime_tokens: field("lifetime_tokens"),
                    peak_daily_tokens: field("peak_daily_tokens"),
                    current_streak_days: field("current_streak_days"),
                    longest_streak_days: field("longest_streak_days"),
                    longest_running_turn_sec: field("longest_running_turn_sec"),
                }
            });
        Ok(Self { daily, summary })
    }
}

fn str_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn bool_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> bool {
    object
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn f64_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> f64 {
    object
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_rate_limit_snapshots() {
        let payload = serde_json::json!({
            "snapshots": [{
                "limit_id": "codex",
                "primary": {"used_percent": 12.5, "window_minutes": 300, "resets_at": 1700000000},
                "credits": {"has_credits": true, "unlimited": false, "balance": "25"},
                "individual_limit": {"remaining_percent": 40.0, "used": "60", "limit": "100", "resets_at": 1700000000},
            }],
        });
        let snapshots =
            rate_limit_snapshots_from_payload(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(snapshots.len(), 1);
        let snapshot = &snapshots[0];
        assert_eq!(snapshot.limit_id.as_deref(), Some("codex"));
        assert_eq!(snapshot.primary.unwrap().used_percent, 12.5);
        assert!(snapshot.secondary.is_none());
        assert_eq!(
            snapshot.credits.as_ref().unwrap().balance.as_deref(),
            Some("25")
        );
        assert_eq!(snapshot.individual_limit.as_ref().unwrap().used, "60");

        let bare = rate_limit_snapshots_from_payload(
            br#"{"limit_id":"codex","primary":{"used_percent":5}}"#,
        )
        .unwrap();
        assert_eq!(bare.len(), 1);
        assert!(rate_limit_snapshots_from_payload(b"[]").is_err());
    }

    #[test]
    fn decodes_account_usage_report() {
        let payload = serde_json::json!({
            "daily": [
                {"bucket_start_seconds": 1700000000, "total_tokens": 1200, "input_tokens": 1000, "output_tokens": 200},
                "malformed entry skipped",
            ],
            "summary": {"lifetime_tokens": 123456, "current_streak_days": 3},
        });
        let report =
            AccountUsageReport::from_payload(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(report.daily.len(), 1);
        assert_eq!(report.daily[0].total_tokens, 1200);
        assert_eq!(report.daily[0].reasoning_output_tokens, 0);
        let summary = report.summary.expect("summary decodes");
        assert_eq!(summary.lifetime_tokens, Some(123456));
        assert_eq!(summary.current_streak_days, Some(3));
        assert_eq!(summary.peak_daily_tokens, None);
        assert!(AccountUsageReport::from_payload(b"null").is_err());

        let bare = AccountUsageReport::from_payload(br#"{"daily": []}"#).unwrap();
        assert!(bare.summary.is_none());
    }
}
