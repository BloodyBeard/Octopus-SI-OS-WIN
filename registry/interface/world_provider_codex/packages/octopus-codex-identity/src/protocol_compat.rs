pub mod auth {
    use serde::Deserialize;
    use serde::Serialize;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum PlanType {
        Known(KnownPlan),
        Unknown(String),
    }

    impl PlanType {
        pub fn from_raw_value(raw: &str) -> Self {
            match raw.to_ascii_lowercase().as_str() {
                "free" => Self::Known(KnownPlan::Free),
                "go" => Self::Known(KnownPlan::Go),
                "plus" => Self::Known(KnownPlan::Plus),
                "pro" => Self::Known(KnownPlan::Pro),
                "prolite" => Self::Known(KnownPlan::ProLite),
                "team" => Self::Known(KnownPlan::Team),
                "self_serve_business_usage_based" => {
                    Self::Known(KnownPlan::SelfServeBusinessUsageBased)
                }
                "business" => Self::Known(KnownPlan::Business),
                "enterprise_cbp_usage_based" => Self::Known(KnownPlan::EnterpriseCbpUsageBased),
                "enterprise" | "hc" => Self::Known(KnownPlan::Enterprise),
                "education" | "edu" => Self::Known(KnownPlan::Edu),
                _ => Self::Unknown(raw.to_string()),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum KnownPlan {
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
        #[serde(alias = "hc")]
        Enterprise,
        #[serde(alias = "education")]
        Edu,
    }

    impl KnownPlan {
        pub fn display_name(self) -> &'static str {
            match self {
                Self::Free => "Free",
                Self::Go => "Go",
                Self::Plus => "Plus",
                Self::Pro => "Pro",
                Self::ProLite => "Pro Lite",
                Self::Team => "Team",
                Self::SelfServeBusinessUsageBased => "Self Serve Business Usage Based",
                Self::Business => "Business",
                Self::EnterpriseCbpUsageBased => "Enterprise CBP Usage Based",
                Self::Enterprise => "Enterprise",
                Self::Edu => "Edu",
            }
        }

        pub fn raw_value(self) -> &'static str {
            match self {
                Self::Free => "free",
                Self::Go => "go",
                Self::Plus => "plus",
                Self::Pro => "pro",
                Self::ProLite => "prolite",
                Self::Team => "team",
                Self::SelfServeBusinessUsageBased => "self_serve_business_usage_based",
                Self::Business => "business",
                Self::EnterpriseCbpUsageBased => "enterprise_cbp_usage_based",
                Self::Enterprise => "enterprise",
                Self::Edu => "edu",
            }
        }

        pub fn is_workspace_account(self) -> bool {
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
}

pub mod protocol {
    use std::fmt;

    use serde::Deserialize;
    use serde::Serialize;

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
    #[serde(rename_all = "lowercase")]
    pub enum SessionSource {
        Cli,
        #[default]
        VSCode,
        Exec,
        Mcp,
        Custom(String),
        Internal(InternalSessionSource),
        SubAgent(SubAgentSource),
        #[serde(other)]
        Unknown,
    }

    impl fmt::Display for SessionSource {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                SessionSource::Cli => f.write_str("cli"),
                SessionSource::VSCode => f.write_str("vscode"),
                SessionSource::Exec => f.write_str("exec"),
                SessionSource::Mcp => f.write_str("mcp"),
                SessionSource::Custom(source) => f.write_str(source),
                SessionSource::Internal(source) => write!(f, "internal_{source}"),
                SessionSource::SubAgent(sub_source) => write!(f, "subagent_{sub_source}"),
                SessionSource::Unknown => f.write_str("unknown"),
            }
        }
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum InternalSessionSource {
        MemoryConsolidation,
    }

    impl fmt::Display for InternalSessionSource {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                InternalSessionSource::MemoryConsolidation => f.write_str("memory_consolidation"),
            }
        }
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum SubAgentSource {
        Review,
        Compact,
        MemoryConsolidation,
        Other(String),
    }

    impl fmt::Display for SubAgentSource {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                SubAgentSource::Review => f.write_str("review"),
                SubAgentSource::Compact => f.write_str("compact"),
                SubAgentSource::MemoryConsolidation => f.write_str("memory_consolidation"),
                SubAgentSource::Other(other) => f.write_str(other),
            }
        }
    }
}
