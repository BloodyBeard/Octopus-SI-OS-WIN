use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PlanType {
    #[default]
    Free,
    Go,
    Plus,
    Pro,
    ProLite,
    Team,
    #[serde(rename = "self_serve_business_usage_based")]
    SelfServeBusinessUsageBased,
    Business,
    #[serde(rename = "enterprise_cbp_usage_based")]
    EnterpriseCbpUsageBased,
    Enterprise,
    Edu,
    #[serde(other)]
    Unknown,
}

impl PlanType {
    pub(crate) fn is_team_like(self) -> bool {
        matches!(self, Self::Team | Self::SelfServeBusinessUsageBased)
    }

    pub(crate) fn is_business_like(self) -> bool {
        matches!(self, Self::Business | Self::EnterpriseCbpUsageBased)
    }

    pub(crate) fn is_workspace_account(self) -> bool {
        matches!(
            self,
            Self::Team
                | Self::SelfServeBusinessUsageBased
                | Self::Business
                | Self::EnterpriseCbpUsageBased
                | Self::Enterprise
                | Self::Edu
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_words_decode_byte_compatible_and_fail_open_to_unknown() {
        for (word, plan) in [
            ("free", PlanType::Free),
            ("prolite", PlanType::ProLite),
            (
                "self_serve_business_usage_based",
                PlanType::SelfServeBusinessUsageBased,
            ),
            (
                "enterprise_cbp_usage_based",
                PlanType::EnterpriseCbpUsageBased,
            ),
        ] {
            assert_eq!(
                serde_json::from_value::<PlanType>(serde_json::json!(word)).expect("decode"),
                plan
            );
            assert_eq!(
                serde_json::to_value(plan).expect("serialize"),
                serde_json::json!(word)
            );
        }
        assert_eq!(
            serde_json::from_value::<PlanType>(serde_json::json!("plan_of_the_future"))
                .expect("unknown word decodes"),
            PlanType::Unknown
        );
    }
}
