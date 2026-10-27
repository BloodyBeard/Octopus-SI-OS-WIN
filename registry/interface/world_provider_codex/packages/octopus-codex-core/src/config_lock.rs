use std::io;

use octopus_config::ConfigLayerEntry;
use octopus_config::ConfigLayerSource;
use octopus_config::config_toml::ConfigLockfileToml;
use octopus_config::config_toml::ConfigToml;
use octopus_util_abspath::AbsolutePathBuf;
use serde::Serialize;

pub(crate) const CONFIG_LOCK_VERSION: u32 = 1;

pub(crate) async fn read_config_lock_from_path(
    path: &AbsolutePathBuf,
) -> io::Result<ConfigLockfileToml> {
    let contents = tokio::fs::read_to_string(path).await.map_err(|err| {
        config_lock_error(format!(
            "failed to read config lock file {}: {err}",
            path.display()
        ))
    })?;
    let lockfile: ConfigLockfileToml = toml::from_str(&contents).map_err(|err| {
        config_lock_error(format!(
            "failed to parse config lock file {}: {err}",
            path.display()
        ))
    })?;
    validate_config_lock_metadata_shape(&lockfile)?;
    Ok(lockfile)
}

pub(crate) fn lock_layer_from_config(
    lock_path: &AbsolutePathBuf,
    lockfile: &ConfigLockfileToml,
) -> io::Result<ConfigLayerEntry> {
    let value = toml_value(
        &config_without_lock_controls(&lockfile.config),
        "config lock",
    )?;
    Ok(ConfigLayerEntry::new(
        ConfigLayerSource::User {
            file: lock_path.clone(),
            profile: None,
        },
        value,
    ))
}

pub(crate) fn config_without_lock_controls(config: &ConfigToml) -> ConfigToml {
    let mut config = config.clone();
    clear_config_lock_debug_controls(&mut config);
    config
}

pub(crate) fn clear_config_lock_debug_controls(config: &mut ConfigToml) {
    if let Some(debug) = config.debug.as_mut() {
        debug.config_lockfile = None;
    }
    if config
        .debug
        .as_ref()
        .is_some_and(|debug| debug.config_lockfile.is_none())
    {
        config.debug = None;
    }
}

fn validate_config_lock_metadata_shape(lock: &ConfigLockfileToml) -> io::Result<()> {
    if lock.version != CONFIG_LOCK_VERSION {
        return Err(config_lock_error(format!(
            "unsupported config lock version {}; expected {CONFIG_LOCK_VERSION}",
            lock.version
        )));
    }
    Ok(())
}

fn config_lock_error(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn toml_value<T: Serialize>(value: &T, label: &str) -> io::Result<toml::Value> {
    toml::Value::try_from(value)
        .map_err(|err| config_lock_error(format!("failed to serialize {label}: {err}")))
}

