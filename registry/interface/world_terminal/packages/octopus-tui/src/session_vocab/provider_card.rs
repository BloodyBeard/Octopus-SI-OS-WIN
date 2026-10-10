use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WireApi {
    #[default]
    Responses,
}

impl std::fmt::Display for WireApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Responses => "responses",
        };
        f.write_str(value)
    }
}

impl<'de> Deserialize<'de> for WireApi {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "responses" => Ok(Self::Responses),
            _ => Err(serde::de::Error::unknown_variant(&value, &["responses"])),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TerminalProviderCard {
    pub name: String,
    pub base_url: Option<String>,
    pub wire_api: WireApi,
    pub requires_openai_auth: bool,
}

pub const DEFAULT_LMSTUDIO_PORT: u16 = 1234;
pub const DEFAULT_OLLAMA_PORT: u16 = 11434;

pub const LMSTUDIO_OSS_PROVIDER_ID: &str = "lmstudio";
pub const OLLAMA_OSS_PROVIDER_ID: &str = "ollama";

pub const LMSTUDIO_DEFAULT_OSS_MODEL: &str = "openai/gpt-oss-20b";
pub const OLLAMA_DEFAULT_OSS_MODEL: &str = "gpt-oss:20b";

pub fn default_model_for_oss_provider(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        LMSTUDIO_OSS_PROVIDER_ID => Some(LMSTUDIO_DEFAULT_OSS_MODEL),
        OLLAMA_OSS_PROVIDER_ID => Some(OLLAMA_DEFAULT_OSS_MODEL),
        _ => None,
    }
}
