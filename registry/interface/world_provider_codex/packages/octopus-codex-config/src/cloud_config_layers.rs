use crate::ConfigLayerEntry;
use crate::ConfigLayerSource;
use crate::TomlValue;
use crate::config_toml::ConfigToml;
use crate::loader::resolve_relative_paths_in_config_toml;
use crate::strict_config::config_error_from_ignored_toml_value_fields_for_source_name;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_util_abspath::AbsolutePathBufGuard;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use std::io;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CloudConfigFragment {
    pub id: String,
    pub name: String,
    pub contents: String,
}

impl CloudConfigFragment {
    fn source_ref(&self) -> CloudConfigFragmentSource {
        CloudConfigFragmentSource {
            id: self.id.clone(),
            name: self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudConfigFragmentSource {
    pub id: String,
    pub name: String,
}

impl fmt::Display for CloudConfigFragmentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name, self.id)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CloudConfigLayerError {
    #[error("failed to parse cloud config fragment {fragment}: {message}")]
    Parse {
        fragment: CloudConfigFragmentSource,
        message: String,
    },
    #[error("invalid cloud config fragment {fragment}: {message}")]
    Invalid {
        fragment: CloudConfigFragmentSource,
        message: String,
    },
}

pub fn cloud_config_layers_from_fragments(
    fragments: impl IntoIterator<Item = CloudConfigFragment>,
    base_dir: &AbsolutePathBuf,
) -> Result<Vec<ConfigLayerEntry>, CloudConfigLayerError> {
    cloud_config_layers_from_fragments_impl(fragments, base_dir, false)
}

pub(crate) fn cloud_config_layers_from_fragments_strict(
    fragments: impl IntoIterator<Item = CloudConfigFragment>,
    base_dir: &AbsolutePathBuf,
) -> Result<Vec<ConfigLayerEntry>, CloudConfigLayerError> {
    cloud_config_layers_from_fragments_impl(fragments, base_dir, true)
}

fn cloud_config_layers_from_fragments_impl(
    fragments: impl IntoIterator<Item = CloudConfigFragment>,
    base_dir: &AbsolutePathBuf,
    strict_config: bool,
) -> Result<Vec<ConfigLayerEntry>, CloudConfigLayerError> {
    let mut layers = Vec::new();
    for fragment in fragments {
        let source_ref = fragment.source_ref();
        let raw_toml = fragment.contents;
        let value: TomlValue =
            toml::from_str(&raw_toml).map_err(|err| CloudConfigLayerError::Parse {
                fragment: source_ref.clone(),
                message: err.to_string(),
            })?;
        if strict_config {
            validate_fragment_strictly(&source_ref, &raw_toml, &value, base_dir)?;
        }
        let resolved =
            resolve_relative_paths_in_config_toml(value, base_dir.as_path()).map_err(|err| {
                CloudConfigLayerError::Invalid {
                    fragment: source_ref.clone(),
                    message: err.to_string(),
                }
            })?;
        layers.push(ConfigLayerEntry::new_with_raw_toml(
            ConfigLayerSource::EnterpriseManaged {
                id: fragment.id,
                name: fragment.name,
            },
            resolved,
            raw_toml,
            base_dir.clone(),
        ));
    }

    layers.reverse();
    Ok(layers)
}

fn validate_fragment_strictly(
    source_ref: &CloudConfigFragmentSource,
    raw_toml: &str,
    value: &TomlValue,
    base_dir: &AbsolutePathBuf,
) -> Result<(), CloudConfigLayerError> {
    let _guard = AbsolutePathBufGuard::new(base_dir.as_path());
    if let Some(config_error) = config_error_from_ignored_toml_value_fields_for_source_name::<
        ConfigToml,
    >(&source_ref.to_string(), raw_toml, value.clone())
    {
        return Err(CloudConfigLayerError::Invalid {
            fragment: source_ref.clone(),
            message: config_error.message,
        });
    }

    Ok(())
}

impl From<CloudConfigLayerError> for io::Error {
    fn from(error: CloudConfigLayerError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, error)
    }
}

#[cfg(test)]
#[path = "cloud_config_layers_tests.rs"]
mod tests;
