use octopus_core::config::ConfigBuilder;
use octopus_core::config::edit::ConfigEditsBuilder;
use octopus_core::config::find_codex_home;
use octopus_features::FEATURES;
use octopus_features::Stage;
use octopus_features::is_known_feature_key;

use crate::cli_args::CliConfigOverrides;

pub fn validate_feature(feature: &str) -> anyhow::Result<()> {
    if is_known_feature_key(feature) {
        Ok(())
    } else {
        anyhow::bail!("Unknown feature flag: {feature}")
    }
}

pub fn feature_toggle_overrides(
    enable: &[String],
    disable: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut v = Vec::new();
    for feature in enable {
        validate_feature(feature)?;
        v.push(format!("features.{feature}=true"));
    }
    for feature in disable {
        validate_feature(feature)?;
        v.push(format!("features.{feature}=false"));
    }
    Ok(v)
}

pub fn stage_str(stage: Stage) -> &'static str {
    match stage {
        Stage::UnderDevelopment => "under development",
        Stage::Experimental { .. } => "experimental",
        Stage::Stable => "stable",
        Stage::Deprecated => "deprecated",
        Stage::Removed => "removed",
    }
}

pub async fn run_features_list(root_config_overrides: CliConfigOverrides) -> anyhow::Result<()> {
    let cli_kv_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;

    let config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .build()
        .await?;
    let mut rows = Vec::with_capacity(FEATURES.len());
    let mut name_width = 0;
    let mut stage_width = 0;
    for def in FEATURES {
        let name = def.key;
        let stage = stage_str(def.stage);
        let enabled = config.features.enabled(def.id);
        name_width = name_width.max(name.len());
        stage_width = stage_width.max(stage.len());
        rows.push((name, stage, enabled));
    }
    rows.sort_unstable_by_key(|(name, _, _)| *name);

    for (name, stage, enabled) in rows {
        println!("{name:<name_width$}  {stage:<stage_width$}  {enabled}");
    }
    Ok(())
}

pub async fn enable_feature_in_config(feature: &str) -> anyhow::Result<()> {
    validate_feature(feature)?;
    let codex_home = find_codex_home()?;
    ConfigEditsBuilder::new(&codex_home)
        .set_feature_enabled(feature, true)
        .apply()
        .await?;
    println!("Enabled feature `{feature}` in config.toml.");
    maybe_print_under_development_feature_warning(&codex_home, feature);
    Ok(())
}

pub async fn disable_feature_in_config(feature: &str) -> anyhow::Result<()> {
    validate_feature(feature)?;
    let codex_home = find_codex_home()?;
    ConfigEditsBuilder::new(&codex_home)
        .set_feature_enabled(feature, false)
        .apply()
        .await?;
    println!("Disabled feature `{feature}` in config.toml.");
    Ok(())
}

fn maybe_print_under_development_feature_warning(codex_home: &std::path::Path, feature: &str) {
    let Some(spec) = FEATURES.iter().find(|spec| spec.key == feature) else {
        return;
    };
    if !matches!(spec.stage, Stage::UnderDevelopment) {
        return;
    }

    let config_path = codex_home.join(octopus_config::CONFIG_TOML_FILE);
    eprintln!(
        "Under-development features enabled: {feature}. Under-development features are incomplete and may behave unpredictably. To suppress this warning, set `suppress_unstable_features_warning = true` in {}.",
        config_path.display()
    );
}
