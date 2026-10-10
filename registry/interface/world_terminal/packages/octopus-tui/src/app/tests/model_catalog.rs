use super::*;
use crate::session_vocab::openai_models::ModelAvailabilityNux;
use octopus_world_terminal_config::ModelAvailabilityNuxConfig;
use pretty_assertions::assert_eq;

fn all_model_presets() -> Vec<ModelPreset> {
    let models = vec![
        "gpt-5.4".to_string(),
        "gpt-5.4-mini".to_string(),
        crate::test_support::get_model_offline_for_tests(None),
    ];
    let mut presets: Vec<ModelPreset> = models
        .iter()
        .map(|model| crate::test_support::construct_model_info_offline_for_tests(model, &()).into())
        .collect();
    ModelPreset::mark_default_by_picker_visibility(&mut presets);
    presets
}

fn model_availability_nux_config(shown_count: &[(&str, u32)]) -> ModelAvailabilityNuxConfig {
    ModelAvailabilityNuxConfig {
        shown_count: shown_count
            .iter()
            .map(|(model, count)| ((*model).to_string(), *count))
            .collect(),
    }
}

#[test]
fn select_model_availability_nux_picks_only_eligible_model() {
    let mut presets = all_model_presets();
    presets.iter_mut().for_each(|preset| {
        preset.availability_nux = None;
    });
    let target = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.4")
        .expect("target preset present");
    target.availability_nux = Some(ModelAvailabilityNux {
        message: "gpt-5.4 is available".to_string(),
    });

    let selected = select_model_availability_nux(&presets, &model_availability_nux_config(&[]));

    assert_eq!(
        selected,
        Some(StartupTooltipOverride {
            model_slug: "gpt-5.4".to_string(),
            message: "gpt-5.4 is available".to_string(),
        })
    );
}

#[test]
fn select_model_availability_nux_skips_missing_and_exhausted_models() {
    let mut presets = all_model_presets();
    presets.iter_mut().for_each(|preset| {
        preset.availability_nux = None;
    });
    let gpt_5 = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.4")
        .expect("gpt-5.4 preset present");
    gpt_5.availability_nux = Some(ModelAvailabilityNux {
        message: "gpt-5.4 is available".to_string(),
    });
    let gpt_5_2 = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.4-mini")
        .expect("gpt-5.4-mini preset present");
    gpt_5_2.availability_nux = Some(ModelAvailabilityNux {
        message: "gpt-5.4-mini is available".to_string(),
    });

    let selected = select_model_availability_nux(
        &presets,
        &model_availability_nux_config(&[("gpt-5.4", MODEL_AVAILABILITY_NUX_MAX_SHOW_COUNT)]),
    );

    assert_eq!(
        selected,
        Some(StartupTooltipOverride {
            model_slug: "gpt-5.4-mini".to_string(),
            message: "gpt-5.4-mini is available".to_string(),
        })
    );
}

#[test]
fn select_model_availability_nux_uses_existing_model_order_as_priority() {
    let mut presets = all_model_presets();
    presets.iter_mut().for_each(|preset| {
        preset.availability_nux = None;
    });
    let first = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.4-mini")
        .expect("gpt-5.4-mini preset present");
    first.availability_nux = Some(ModelAvailabilityNux {
        message: "first".to_string(),
    });
    let second = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.4")
        .expect("gpt-5.4 preset present");
    second.availability_nux = Some(ModelAvailabilityNux {
        message: "second".to_string(),
    });

    let selected = select_model_availability_nux(&presets, &model_availability_nux_config(&[]));

    assert_eq!(
        selected,
        Some(StartupTooltipOverride {
            model_slug: "gpt-5.4".to_string(),
            message: "second".to_string(),
        })
    );
}

#[test]
fn select_model_availability_nux_returns_none_when_all_models_are_exhausted() {
    let mut presets = all_model_presets();
    presets.iter_mut().for_each(|preset| {
        preset.availability_nux = None;
    });
    let target = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.4")
        .expect("target preset present");
    target.availability_nux = Some(ModelAvailabilityNux {
        message: "gpt-5.4 is available".to_string(),
    });

    let selected = select_model_availability_nux(
        &presets,
        &model_availability_nux_config(&[("gpt-5.4", MODEL_AVAILABILITY_NUX_MAX_SHOW_COUNT)]),
    );

    assert_eq!(selected, None);
}

#[tokio::test]
async fn prepare_startup_tooltip_override_persists_model_availability_nux_count() {
    let codex_home = tempdir().expect("temp codex home");
    write_test_model_provider_config(codex_home.path());
    let mut config = config_from_disk(codex_home.path());
    let mut presets = all_model_presets();
    presets.iter_mut().for_each(|preset| {
        preset.availability_nux = None;
    });
    let target = presets
        .iter_mut()
        .find(|preset| preset.model == "gpt-5.4")
        .expect("target preset present");
    target.availability_nux = Some(ModelAvailabilityNux {
        message: "gpt-5.4 is available".to_string(),
    });

    let tooltip = prepare_startup_tooltip_override(&mut config, &presets, false).await;

    assert_eq!(tooltip.as_deref(), Some("gpt-5.4 is available"));
    assert_eq!(
        config.terminal.model_availability_nux.shown_count,
        HashMap::from([("gpt-5.4".to_string(), 1)])
    );

    let reloaded = config_from_disk(codex_home.path());
    assert_eq!(
        reloaded.terminal.model_availability_nux.shown_count,
        HashMap::from([("gpt-5.4".to_string(), 1)])
    );
}

fn config_from_disk(codex_home: &std::path::Path) -> crate::tui_config::TuiConfig {
    let mut snapshot = crate::test_support::test_session_snapshot(
        octopus_util_abspath::AbsolutePathBuf::from_absolute_path(codex_home)
            .expect("absolute codex home"),
        octopus_util_abspath::AbsolutePathBuf::current_dir().expect("cwd"),
    );
    let raw = std::fs::read_to_string(codex_home.join("config.toml")).expect("read config.toml");
    snapshot.effective_config = toml::from_str::<toml::Value>(&raw).expect("parse config.toml");
    crate::tui_config::TuiConfig::from_snapshot(snapshot).expect("compose tui config")
}
