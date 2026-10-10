use crate::session_vocab::config_types::CollaborationModeMask;
use crate::session_vocab::openai_models::ModelPreset;
use std::convert::Infallible;

#[derive(Debug, Clone)]
pub(crate) struct ModelCatalog {
    models: Vec<ModelPreset>,
    collaboration_modes: Vec<CollaborationModeMask>,
}

impl ModelCatalog {
    pub(crate) fn new(
        models: Vec<ModelPreset>,
        collaboration_modes: Vec<CollaborationModeMask>,
    ) -> Self {
        Self {
            models,
            collaboration_modes,
        }
    }

    pub(crate) fn try_list_models(&self) -> Result<Vec<ModelPreset>, Infallible> {
        Ok(self.models.clone())
    }

    pub(crate) fn collaboration_modes(&self) -> &[CollaborationModeMask] {
        &self.collaboration_modes
    }
}
