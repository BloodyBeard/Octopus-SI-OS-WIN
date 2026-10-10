mod lmstudio;

use octopus_codex_model_provider_info::LMSTUDIO_OSS_PROVIDER_ID;
use octopus_codex_model_provider_info::OLLAMA_OSS_PROVIDER_ID;
use octopus_core::config::Config;

pub async fn ensure_oss_provider_ready(
    provider_id: &str,
    config: &Config,
) -> Result<(), std::io::Error> {
    match provider_id {
        LMSTUDIO_OSS_PROVIDER_ID => {
            lmstudio::ensure_oss_ready(config)
                .await
                .map_err(|e| std::io::Error::other(format!("OSS setup failed: {e}")))?;
        }
        OLLAMA_OSS_PROVIDER_ID => {
            octopus_ollama::ensure_responses_supported(&config.model_provider).await?;
            octopus_ollama::ensure_oss_ready(config)
                .await
                .map_err(|e| std::io::Error::other(format!("OSS setup failed: {e}")))?;
        }
        _ => {}
    }
    Ok(())
}
