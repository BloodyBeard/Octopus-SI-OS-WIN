use super::Config;
use super::ConfigTomlLoadResult;
use super::ManagedFeatures;
use octopus_config::types::AuthKeyringBackendKind;
use octopus_features::Feature;
use octopus_features::FeatureConfigSource;
use octopus_features::FeatureOverrides;
use octopus_features::Features;

impl Config {
    pub fn auth_keyring_backend_kind(&self) -> AuthKeyringBackendKind {
        auth_keyring_backend_kind_from_secret_auth_storage(
            self.features.enabled(Feature::SecretAuthStorage),
        )
    }
}

pub fn resolve_bootstrap_auth_keyring_backend_kind(
    bootstrap_config: &ConfigTomlLoadResult,
) -> std::io::Result<AuthKeyringBackendKind> {
    let config_toml = &bootstrap_config.config_toml;
    let features = Features::from_sources(
        FeatureConfigSource {
            features: config_toml.features.as_ref(),
            experimental_use_unified_exec_tool: config_toml.experimental_use_unified_exec_tool,
        },
        FeatureConfigSource::default(),
        FeatureOverrides::default(),
    );
    let managed_features = ManagedFeatures::from_configured(
        features,
        bootstrap_config
            .config_layer_stack
            .requirements()
            .feature_requirements
            .clone(),
    )?;
    Ok(auth_keyring_backend_kind_from_secret_auth_storage(
        managed_features.enabled(Feature::SecretAuthStorage),
    ))
}

fn auth_keyring_backend_kind_from_secret_auth_storage(
    secret_auth_storage_enabled: bool,
) -> AuthKeyringBackendKind {
    if secret_auth_storage_enabled {
        AuthKeyringBackendKind::Secrets
    } else {
        AuthKeyringBackendKind::Direct
    }
}

#[cfg(test)]
#[path = "auth_keyring_tests.rs"]
mod tests;
