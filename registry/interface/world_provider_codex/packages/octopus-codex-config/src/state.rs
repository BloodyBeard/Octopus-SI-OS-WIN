use crate::config_requirements::ConfigRequirements;
use crate::config_requirements::ConfigRequirementsToml;

use super::fingerprint::record_origins;
use super::fingerprint::version_for_toml;
use super::key_aliases::normalized_with_key_aliases;
use super::merge::merge_toml_values;
use crate::CloudConfigBundleLoader;
use crate::ProfileV2Name;
use octopus_appserver_proto::ConfigLayer;
use octopus_appserver_proto::ConfigLayerMetadata;
use octopus_appserver_proto::ConfigLayerSource;
use octopus_util_abspath::AbsolutePathBuf;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use toml::Value as TomlValue;

#[derive(Debug, Default, Clone)]
pub struct ConfigLoadOptions {
    pub loader_overrides: LoaderOverrides,
    pub strict_config: bool,
    pub cloud_config_bundle: CloudConfigBundleLoader,
}

impl From<LoaderOverrides> for ConfigLoadOptions {
    fn from(loader_overrides: LoaderOverrides) -> Self {
        Self {
            loader_overrides,
            strict_config: false,
            cloud_config_bundle: CloudConfigBundleLoader::default(),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct LoaderOverrides {
    pub user_config_path: Option<AbsolutePathBuf>,
    pub user_config_profile: Option<ProfileV2Name>,
    pub managed_config_path: Option<PathBuf>,
    pub system_config_path: Option<PathBuf>,
    pub system_requirements_path: Option<PathBuf>,
    pub ignore_managed_requirements: bool,
    pub ignore_user_config: bool,
    pub ignore_user_and_project_exec_policy_rules: bool,
    #[cfg(target_os = "macos")]
    pub managed_preferences_base64: Option<String>,
    pub macos_managed_config_requirements_base64: Option<String>,
}

impl LoaderOverrides {
    pub fn without_managed_config_for_tests() -> Self {
        let base = std::env::temp_dir().join("codex-config-tests");
        Self {
            user_config_path: None,
            user_config_profile: None,
            managed_config_path: Some(base.join("managed_config.toml")),
            system_config_path: Some(base.join("config.toml")),
            system_requirements_path: Some(base.join("requirements.toml")),
            ignore_managed_requirements: false,
            ignore_user_config: false,
            ignore_user_and_project_exec_policy_rules: false,
            #[cfg(target_os = "macos")]
            managed_preferences_base64: Some(String::new()),
            macos_managed_config_requirements_base64: Some(String::new()),
        }
    }

    pub fn with_managed_config_path_for_tests(managed_config_path: PathBuf) -> Self {
        let system_requirements_path = managed_config_path.with_file_name("requirements.toml");
        Self {
            user_config_path: None,
            user_config_profile: None,
            managed_config_path: Some(managed_config_path),
            system_requirements_path: Some(system_requirements_path),
            ..Self::without_managed_config_for_tests()
        }
    }

    pub fn user_config_path(&self, codex_home: &Path) -> std::io::Result<AbsolutePathBuf> {
        match self.user_config_path.as_ref() {
            Some(path) => Ok(path.clone()),
            None => Ok(AbsolutePathBuf::resolve_path_against_base(
                crate::CONFIG_TOML_FILE,
                codex_home,
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigLayerEntry {
    pub name: ConfigLayerSource,
    pub config: TomlValue,
    pub version: String,
    pub disabled_reason: Option<String>,
    raw_toml: Option<RawTomlLayer>,
    hooks_config_folder_override: Option<AbsolutePathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
struct RawTomlLayer {
    contents: String,
    base_dir: AbsolutePathBuf,
}

impl ConfigLayerEntry {
    pub fn new(name: ConfigLayerSource, config: TomlValue) -> Self {
        let version = version_for_toml(&config);
        Self {
            name,
            config,
            version,
            disabled_reason: None,
            raw_toml: None,
            hooks_config_folder_override: None,
        }
    }

    pub fn new_with_raw_toml(
        name: ConfigLayerSource,
        config: TomlValue,
        raw_toml: String,
        raw_toml_base_dir: AbsolutePathBuf,
    ) -> Self {
        let version = version_for_toml(&config);
        Self {
            name,
            config,
            version,
            disabled_reason: None,
            raw_toml: Some(RawTomlLayer {
                contents: raw_toml,
                base_dir: raw_toml_base_dir,
            }),
            hooks_config_folder_override: None,
        }
    }

    pub fn new_disabled(
        name: ConfigLayerSource,
        config: TomlValue,
        disabled_reason: impl Into<String>,
    ) -> Self {
        let version = version_for_toml(&config);
        Self {
            name,
            config,
            version,
            disabled_reason: Some(disabled_reason.into()),
            raw_toml: None,
            hooks_config_folder_override: None,
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled_reason.is_some()
    }

    pub fn raw_toml(&self) -> Option<&str> {
        self.raw_toml
            .as_ref()
            .map(|raw_toml| raw_toml.contents.as_str())
    }

    pub fn raw_toml_base_dir(&self) -> Option<&AbsolutePathBuf> {
        self.raw_toml.as_ref().map(|raw_toml| &raw_toml.base_dir)
    }

    pub(crate) fn with_hooks_config_folder_override(
        mut self,
        hooks_config_folder_override: Option<AbsolutePathBuf>,
    ) -> Self {
        self.hooks_config_folder_override = hooks_config_folder_override;
        self
    }

    pub fn metadata(&self) -> ConfigLayerMetadata {
        ConfigLayerMetadata {
            name: self.name.clone(),
            version: self.version.clone(),
        }
    }

    pub fn as_layer(&self) -> ConfigLayer {
        ConfigLayer {
            name: self.name.clone(),
            version: self.version.clone(),
            config: serde_json::to_value(&self.config).unwrap_or(JsonValue::Null),
            disabled_reason: self.disabled_reason.clone(),
        }
    }

    pub fn config_folder(&self) -> Option<AbsolutePathBuf> {
        match &self.name {
            ConfigLayerSource::Mdm { .. } => None,
            ConfigLayerSource::System { file } => file.parent(),
            ConfigLayerSource::EnterpriseManaged { .. } => None,
            ConfigLayerSource::User { file, .. } => file.parent(),
            ConfigLayerSource::Project { dot_codex_folder } => Some(dot_codex_folder.clone()),
            ConfigLayerSource::SessionFlags => None,
            ConfigLayerSource::LegacyManagedConfigTomlFromFile { .. } => None,
            ConfigLayerSource::LegacyManagedConfigTomlFromMdm => None,
        }
    }

    pub fn hooks_config_folder(&self) -> Option<AbsolutePathBuf> {
        self.hooks_config_folder_override
            .clone()
            .or_else(|| self.config_folder())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLayerStackOrdering {
    LowestPrecedenceFirst,
    HighestPrecedenceFirst,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigLayerStack {
    layers: Vec<ConfigLayerEntry>,

    user_layer_index: Option<usize>,

    requirements: ConfigRequirements,

    requirements_toml: ConfigRequirementsToml,

    ignore_user_and_project_exec_policy_rules: bool,

    startup_warnings: Option<Vec<String>>,
}

impl ConfigLayerStack {
    pub fn new(
        layers: Vec<ConfigLayerEntry>,
        requirements: ConfigRequirements,
        requirements_toml: ConfigRequirementsToml,
    ) -> std::io::Result<Self> {
        let user_layer_index = verify_layer_ordering(&layers)?;
        Ok(Self {
            layers,
            user_layer_index,
            requirements,
            requirements_toml,
            ignore_user_and_project_exec_policy_rules: false,
            startup_warnings: None,
        })
    }

    pub fn with_user_and_project_exec_policy_rules_ignored(
        mut self,
        ignore_user_and_project_exec_policy_rules: bool,
    ) -> Self {
        self.ignore_user_and_project_exec_policy_rules = ignore_user_and_project_exec_policy_rules;
        self
    }

    pub fn ignore_user_and_project_exec_policy_rules(&self) -> bool {
        self.ignore_user_and_project_exec_policy_rules
    }

    pub(crate) fn with_startup_warnings(mut self, startup_warnings: Vec<String>) -> Self {
        self.startup_warnings = Some(startup_warnings);
        self
    }

    pub fn startup_warnings(&self) -> Option<&[String]> {
        self.startup_warnings.as_deref()
    }

    pub fn get_active_user_layer(&self) -> Option<&ConfigLayerEntry> {
        self.user_layer_index
            .and_then(|index| self.layers.get(index))
    }

    pub fn get_user_config_file(&self) -> Option<&AbsolutePathBuf> {
        let layer = self.get_active_user_layer()?;
        let ConfigLayerSource::User { file, .. } = &layer.name else {
            return None;
        };
        Some(file)
    }

    pub fn get_user_layers(
        &self,
        ordering: ConfigLayerStackOrdering,
        include_disabled: bool,
    ) -> Vec<&ConfigLayerEntry> {
        self.get_layers(ordering, include_disabled)
            .into_iter()
            .filter(|layer| matches!(layer.name, ConfigLayerSource::User { .. }))
            .collect()
    }

    pub fn effective_user_config(&self) -> Option<TomlValue> {
        let user_layers =
            self.get_user_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, false);
        if user_layers.is_empty() {
            return None;
        }

        let mut merged = TomlValue::Table(toml::map::Map::new());
        for layer in user_layers {
            merge_toml_values(&mut merged, &layer.config);
        }
        Some(merged)
    }

    pub fn requirements(&self) -> &ConfigRequirements {
        &self.requirements
    }

    pub fn requirements_toml(&self) -> &ConfigRequirementsToml {
        &self.requirements_toml
    }

    pub fn with_user_config(&self, config_toml: &AbsolutePathBuf, user_config: TomlValue) -> Self {
        let profile = self.layers.iter().find_map(|layer| match &layer.name {
            ConfigLayerSource::User { file, profile } if file == config_toml => profile
                .as_deref()
                .and_then(|profile| profile.parse::<ProfileV2Name>().ok()),
            _ => None,
        });
        self.with_user_config_profile(config_toml, profile.as_ref(), user_config)
    }

    pub fn with_user_config_profile(
        &self,
        config_toml: &AbsolutePathBuf,
        profile: Option<&ProfileV2Name>,
        user_config: TomlValue,
    ) -> Self {
        let user_layer = ConfigLayerEntry::new(
            ConfigLayerSource::User {
                file: config_toml.clone(),
                profile: profile.map(ToString::to_string),
            },
            user_config,
        );

        let mut layers = self.layers.clone();
        if let Some(index) = layers.iter().position(|layer| {
            matches!(
                &layer.name,
                ConfigLayerSource::User { file, .. } if file == config_toml
            )
        }) {
            layers.remove(index);
        }
        match layers
            .iter()
            .position(|layer| layer.name.precedence() > user_layer.name.precedence())
        {
            Some(index) => layers.insert(index, user_layer),
            None => layers.push(user_layer),
        }
        let user_layer_index = layers.iter().enumerate().rev().find_map(|(index, layer)| {
            if matches!(layer.name, ConfigLayerSource::User { .. }) {
                Some(index)
            } else {
                None
            }
        });
        Self {
            layers,
            user_layer_index,
            requirements: self.requirements.clone(),
            requirements_toml: self.requirements_toml.clone(),
            ignore_user_and_project_exec_policy_rules: self
                .ignore_user_and_project_exec_policy_rules,
            startup_warnings: self.startup_warnings.clone(),
        }
    }

    pub fn with_user_layer_from(&self, other: &Self) -> Self {
        let user_layers = other
            .layers
            .iter()
            .filter(|layer| matches!(layer.name, ConfigLayerSource::User { .. }))
            .cloned()
            .collect::<Vec<_>>();
        let mut layers = self
            .layers
            .iter()
            .filter(|layer| !matches!(layer.name, ConfigLayerSource::User { .. }))
            .cloned()
            .collect::<Vec<_>>();
        for user_layer in user_layers {
            match layers
                .iter()
                .position(|layer| layer.name.precedence() > user_layer.name.precedence())
            {
                Some(index) => layers.insert(index, user_layer),
                None => layers.push(user_layer),
            }
        }
        let user_layer_index = layers.iter().enumerate().rev().find_map(|(index, layer)| {
            if matches!(layer.name, ConfigLayerSource::User { .. }) {
                Some(index)
            } else {
                None
            }
        });
        Self {
            layers,
            user_layer_index,
            requirements: self.requirements.clone(),
            requirements_toml: self.requirements_toml.clone(),
            ignore_user_and_project_exec_policy_rules: self
                .ignore_user_and_project_exec_policy_rules,
            startup_warnings: self.startup_warnings.clone(),
        }
    }

    pub fn effective_config(&self) -> TomlValue {
        let mut merged = TomlValue::Table(toml::map::Map::new());
        for layer in self.get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, false) {
            merge_toml_values(&mut merged, &layer.config);
        }
        merged
    }

    pub fn to_descriptor(
        &self,
        store: &mut octopus_descriptors::DescriptorStore,
    ) -> octopus_descriptors::Result<octopus_descriptors::DescriptorHash> {
        let effective = self.effective_config();
        crate::ConfigShellDescriptor::from_effective_config(&effective)?.put_into_store(store)
    }

    pub fn origins(&self) -> HashMap<String, ConfigLayerMetadata> {
        let mut origins = HashMap::new();
        let mut path = Vec::new();

        for layer in self.get_layers(ConfigLayerStackOrdering::LowestPrecedenceFirst, false) {
            let config = normalized_with_key_aliases(&layer.config, &[]);
            record_origins(&config, &layer.metadata(), &mut path, &mut origins);
        }

        origins
    }

    pub fn layers_high_to_low(&self) -> Vec<&ConfigLayerEntry> {
        self.get_layers(ConfigLayerStackOrdering::HighestPrecedenceFirst, false)
    }

    pub fn get_layers(
        &self,
        ordering: ConfigLayerStackOrdering,
        include_disabled: bool,
    ) -> Vec<&ConfigLayerEntry> {
        let mut layers: Vec<&ConfigLayerEntry> = self
            .layers
            .iter()
            .filter(|layer| include_disabled || !layer.is_disabled())
            .collect();
        if ordering == ConfigLayerStackOrdering::HighestPrecedenceFirst {
            layers.reverse();
        }
        layers
    }
}

fn verify_layer_ordering(layers: &[ConfigLayerEntry]) -> std::io::Result<Option<usize>> {
    if !layers.iter().map(|layer| &layer.name).is_sorted() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "config layers are not in correct precedence order",
        ));
    }

    let mut user_layer_index: Option<usize> = None;
    let mut previous_project_dot_codex_folder: Option<&AbsolutePathBuf> = None;
    for (index, layer) in layers.iter().enumerate() {
        if matches!(layer.name, ConfigLayerSource::User { .. }) {
            user_layer_index = Some(index);
        }

        if let ConfigLayerSource::Project {
            dot_codex_folder: current_project_dot_codex_folder,
        } = &layer.name
        {
            if let Some(previous) = previous_project_dot_codex_folder {
                let Some(parent) = previous.as_path().parent() else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "project layer has no parent directory",
                    ));
                };
                if previous == current_project_dot_codex_folder
                    || !current_project_dot_codex_folder
                        .as_path()
                        .ancestors()
                        .any(|ancestor| ancestor == parent)
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "project layers are not ordered from root to cwd",
                    ));
                }
            }
            previous_project_dot_codex_folder = Some(current_project_dot_codex_folder);
        }
    }

    Ok(user_layer_index)
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
