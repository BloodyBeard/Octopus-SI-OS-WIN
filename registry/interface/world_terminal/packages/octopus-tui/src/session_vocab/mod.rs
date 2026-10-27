
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub mod config_types;
pub mod openai_models;
pub mod provider_card;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MultiAgentVersion {
    Disabled,
    V1,
    V2,
}
