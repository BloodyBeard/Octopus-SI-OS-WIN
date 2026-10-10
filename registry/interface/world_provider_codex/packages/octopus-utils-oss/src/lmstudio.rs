use octopus_core::config::Config;

pub const DEFAULT_OSS_MODEL: &str = octopus_codex_model_provider_info::LMSTUDIO_DEFAULT_OSS_MODEL;

pub async fn ensure_oss_ready(config: &Config) -> std::io::Result<()> {
    let _model = match config.model.as_ref() {
        Some(model) => model.as_str(),
        None => DEFAULT_OSS_MODEL,
    };

    Ok(())
}
