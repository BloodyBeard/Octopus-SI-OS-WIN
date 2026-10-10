use std::path::Path;

use crate::session_vocab::config_types::CollaborationModeMask;
use crate::session_vocab::config_types::ModeKind;
use crate::session_vocab::config_types::TUI_VISIBLE_COLLABORATION_MODES;
use crate::session_vocab::openai_models::ReasoningEffort;

use crate::model_catalog::ModelCatalog;

fn filtered_presets(model_catalog: &ModelCatalog) -> Vec<CollaborationModeMask> {
    model_catalog
        .collaboration_modes()
        .iter()
        .filter(|mask| mask.mode.is_some_and(ModeKind::is_tui_visible))
        .cloned()
        .collect()
}

pub(crate) fn load_presets(registry_root: &Path) -> Vec<CollaborationModeMask> {
    [plan_preset(registry_root), default_preset(registry_root)]
        .into_iter()
        .flatten()
        .collect()
}

#[derive(serde::Deserialize)]
struct CollabModeArtifact {
    text: String,
}

fn load_collab_mode_text(registry_root: &Path, mode_dir: &str) -> Option<String> {
    let path = registry_root
        .join("artifact")
        .join(mode_dir)
        .join("artifact.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(
                "collaboration mode artifact '{}' is unreadable; omitting the mode preset: {err}",
                path.display()
            );
            return None;
        }
    };
    match serde_json::from_slice::<CollabModeArtifact>(&bytes) {
        Ok(artifact) => Some(artifact.text),
        Err(err) => {
            tracing::error!(
                "collaboration mode artifact '{}' is malformed; omitting the mode preset: {err}",
                path.display()
            );
            None
        }
    }
}

fn plan_preset(registry_root: &Path) -> Option<CollaborationModeMask> {
    let text = load_collab_mode_text(registry_root, "artifact-collab-mode-plan")?;
    Some(CollaborationModeMask {
        name: ModeKind::Plan.display_name().to_string(),
        mode: Some(ModeKind::Plan),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(text)),
    })
}

fn default_preset(registry_root: &Path) -> Option<CollaborationModeMask> {
    let text = load_collab_mode_text(registry_root, "artifact-collab-mode-default")?;
    let known_mode_names = format_mode_names(&TUI_VISIBLE_COLLABORATION_MODES);
    Some(CollaborationModeMask {
        name: ModeKind::Default.display_name().to_string(),
        mode: Some(ModeKind::Default),
        model: None,
        reasoning_effort: None,
        developer_instructions: Some(Some(
            text.replace("{{KNOWN_MODE_NAMES}}", &known_mode_names),
        )),
    })
}

fn format_mode_names(modes: &[ModeKind]) -> String {
    let mode_names: Vec<&str> = modes.iter().map(|mode| mode.display_name()).collect();
    match mode_names.as_slice() {
        [] => "none".to_string(),
        [mode_name] => (*mode_name).to_string(),
        [first, second] => format!("{first} and {second}"),
        [..] => mode_names.join(", "),
    }
}

pub(crate) fn default_mask(model_catalog: &ModelCatalog) -> Option<CollaborationModeMask> {
    let presets = filtered_presets(model_catalog);
    presets
        .iter()
        .find(|mask| mask.mode == Some(ModeKind::Default))
        .cloned()
}

pub(crate) fn mask_for_kind(
    model_catalog: &ModelCatalog,
    kind: ModeKind,
) -> Option<CollaborationModeMask> {
    if !kind.is_tui_visible() {
        return None;
    }
    filtered_presets(model_catalog)
        .into_iter()
        .find(|mask| mask.mode == Some(kind))
}

pub(crate) fn next_mask(
    model_catalog: &ModelCatalog,
    current: Option<&CollaborationModeMask>,
) -> Option<CollaborationModeMask> {
    let presets = filtered_presets(model_catalog);
    if presets.is_empty() {
        return None;
    }
    let current_kind = current.and_then(|mask| mask.mode);
    let next_index = presets
        .iter()
        .position(|mask| mask.mode == current_kind)
        .map_or(0, |idx| (idx + 1) % presets.len());
    presets.get(next_index).cloned()
}

pub(crate) fn default_mode_mask(model_catalog: &ModelCatalog) -> Option<CollaborationModeMask> {
    mask_for_kind(model_catalog, ModeKind::Default)
}

pub(crate) fn plan_mask(model_catalog: &ModelCatalog) -> Option<CollaborationModeMask> {
    mask_for_kind(model_catalog, ModeKind::Plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_isolated_to_the_supplied_registry() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for (root, text) in [(first.path(), "first"), (second.path(), "second")] {
            let folder = root.join("artifact/artifact-collab-mode-plan");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(
                folder.join("artifact.json"),
                serde_json::json!({"text": text}).to_string(),
            )
            .unwrap();
        }
        for (root, expected) in [(first.path(), "first"), (second.path(), "second")] {
            let catalog = ModelCatalog::new(Vec::new(), load_presets(root));
            assert_eq!(
                plan_mask(&catalog).unwrap().developer_instructions,
                Some(Some(expected.to_string()))
            );
            assert!(default_mask(&catalog).is_none());
        }
    }

    #[test]
    fn absent_and_malformed_artifacts_do_not_create_modes() {
        let root = tempfile::tempdir().unwrap();
        assert!(load_presets(root.path()).is_empty());
        let folder = root.path().join("artifact/artifact-collab-mode-plan");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("artifact.json"), r#"{"text":42}"#).unwrap();
        assert!(load_presets(root.path()).is_empty());
    }
}
