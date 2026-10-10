use crate::session_vocab::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use crate::session_vocab::openai_models::ModelPreset;
use crate::tui_config::TuiConfig;

pub(crate) fn configured_service_tier(config: &TuiConfig) -> Option<String> {
    config.service_tier.clone().or_else(|| {
        (config.notices.fast_default_opt_out == Some(true))
            .then(|| SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string())
    })
}

pub(crate) fn effective_service_tier(
    config: &TuiConfig,
    model: &str,
    models: &[ModelPreset],
) -> Option<String> {
    if !config.features.enabled_key("fast_mode") {
        return None;
    }

    let configured = configured_service_tier(config);
    let Some(preset) = models.iter().find(|preset| preset.model == model) else {
        return configured;
    };

    match configured.as_deref() {
        Some(service_tier) if service_tier == SERVICE_TIER_DEFAULT_REQUEST_VALUE => configured,
        Some(service_tier) if model_supports_service_tier(preset, service_tier) => configured,
        Some(_) => None,
        None => preset
            .default_service_tier
            .clone()
            .filter(|service_tier| model_supports_service_tier(preset, service_tier)),
    }
}

pub(crate) fn service_tier_update_for_core(
    config: &TuiConfig,
    model: &str,
    models: &[ModelPreset],
) -> Option<Option<String>> {
    if !config.features.enabled_key("fast_mode") {
        return None;
    }

    let effective = effective_service_tier(config, model, models);
    if let Some(service_tier) = effective {
        return Some(Some(service_tier));
    }

    if !models.iter().any(|preset| preset.model == model) {
        return None;
    }

    Some(Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string()))
}

pub(crate) fn model_supports_service_tier(model: &ModelPreset, service_tier: &str) -> bool {
    model
        .service_tiers
        .iter()
        .any(|tier| tier.id == service_tier)
}
