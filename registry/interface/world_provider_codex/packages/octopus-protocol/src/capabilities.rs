use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SelectedCapabilityRoot {
    pub id: String,
    pub location: CapabilityRootLocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type")]
#[ts(export_to = "v2/")]
pub enum CapabilityRootLocation {
    Environment {
        #[serde(rename = "environmentId")]
        #[ts(rename = "environmentId")]
        environment_id: String,
        path: String,
    },
}
