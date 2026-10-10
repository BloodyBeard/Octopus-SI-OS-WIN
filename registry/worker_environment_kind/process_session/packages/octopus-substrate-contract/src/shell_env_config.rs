use std::collections::HashMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;
use ts_rs::TS;
use wildmatch::WildMatchPattern;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ShellEnvironmentPolicyInherit {
    Core,

    #[default]
    All,

    None,
}

pub type EnvironmentVariablePattern = WildMatchPattern<'*', '?'>;

#[derive(Debug, Clone, PartialEq)]
pub struct ShellEnvironmentPolicy {
    pub inherit: ShellEnvironmentPolicyInherit,

    pub ignore_default_excludes: bool,

    pub exclude: Vec<EnvironmentVariablePattern>,

    pub r#set: HashMap<String, String>,

    pub include_only: Vec<EnvironmentVariablePattern>,

    pub use_profile: bool,
}

impl Default for ShellEnvironmentPolicy {
    fn default() -> Self {
        Self {
            inherit: ShellEnvironmentPolicyInherit::All,
            ignore_default_excludes: true,
            exclude: Vec::new(),
            r#set: HashMap::new(),
            include_only: Vec::new(),
            use_profile: false,
        }
    }
}

#[derive(
    Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Display, JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum WindowsSandboxLevel {
    #[default]
    Disabled,
    RestrictedToken,
    Elevated,
}
