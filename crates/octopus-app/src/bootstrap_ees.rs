use std::path::Path;

use octopus_descriptors::{loader, Descriptor, EeId};
use octopus_ee::{bootstrap_ee_instance_id, EeInstance, EeTemplateDescriptor};
use octopus_file_storage::publication::{self, DigestAlgorithm, DirectoryMove, PublicationOutcome};

pub fn ensure_bootstrap_ee_instances(
    registry_root: &Path,
    ee_instances_root: &Path,
) -> Result<usize, String> {
    let templates =
        loader::load_kind::<EeTemplateDescriptor>(&registry_root.join(EeTemplateDescriptor::KIND))
            .map_err(|error| format!("EE template registry failed to load: {error}"))?;
    let mut roots = templates
        .iter()
        .filter(|(_, template)| template.bootstrap_root);
    let Some((_, root_template)) = roots.next() else {
        return Err("EE template registry declares no bootstrap root".to_string());
    };
    if let Some((_, other)) = roots.next() {
        return Err(format!(
            "EE template registry declares multiple bootstrap roots: '{}' and '{}'",
            root_template.id.as_str(),
            other.id.as_str()
        ));
    }

    let root_id = bootstrap_ee_instance_id(root_template);
    let root = EeInstance {
        ee_id: root_id.clone(),
        template: root_template.id.clone(),
        parent_ee: None,
        goal: root_template.goal_class.clone(),
        config: Default::default(),
        policy_narrowing: Default::default(),
    };
    let root_path = ee_instances_root.join(format!("{}.json", root.ee_id.as_str()));
    if ee_instances_root.exists() {
        let bytes = std::fs::read(&root_path).map_err(|error| {
            format!(
                "existing EE instance root '{}' has no readable bootstrap root '{}': {error}; normal boot never recreates deleted EE records",
                ee_instances_root.display(),
                root_path.display()
            )
        })?;
        validate_existing_root(&root_path, &root, &bytes)?;
        return Ok(0);
    }
    let parent = ee_instances_root.parent().ok_or_else(|| {
        format!(
            "EE instance root '{}' has no parent",
            ee_instances_root.display()
        )
    })?;
    publication::create_dir_all(parent).map_err(|error| {
        format!(
            "EE state parent '{}' cannot be created: {error}",
            parent.display()
        )
    })?;
    let attempt = uuid::Uuid::new_v4();
    let staging = parent.join(format!(".octopus-ee-bootstrap-{attempt}"));
    std::fs::create_dir(&staging).map_err(|error| {
        format!(
            "EE bootstrap staging directory '{}' cannot be created: {error}",
            staging.display()
        )
    })?;
    let staged_root = staging.join(format!("{}.json", root.ee_id.as_str()));
    let staged = (|| -> Result<(), String> {
        let mut bytes = serde_json::to_vec_pretty(&root)
            .map_err(|error| format!("bootstrap EE '{}' failed to encode: {error}", root.ee_id))?;
        bytes.push(b'\n');
        std::fs::write(&staged_root, &bytes).map_err(|error| {
            format!(
                "bootstrap EE root '{}' cannot be staged: {error}",
                staged_root.display()
            )
        })?;
        octopus_private_fs::sync_directory(&staged_root)
            .map_err(|error| format!("bootstrap EE root did not sync: {error}"))?;
        octopus_private_fs::sync_directory(&staging)
            .map_err(|error| format!("bootstrap EE staging directory did not sync: {error}"))
    })();
    if let Err(error) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    let expected = publication::snapshot_directory(&staging, DigestAlgorithm::Sha256)
        .map_err(|error| format!("cannot prepare EE root publication: {error}"))?;
    let result = publication::publish_directory_moves(&format!("ee-bootstrap:{attempt}"), &[DirectoryMove {
        source: staging, destination: ee_instances_root.to_path_buf(), expected,
    }]).map_err(|error| format!("cannot publish EE instance root '{}': {error}", ee_instances_root.display()))?;
    if result.error.is_some() || !matches!(result.receipts.as_slice(), [receipt] if receipt.outcome == PublicationOutcome::Applied) {
        return Err(format!("EE instance root '{}' was not published: {}", ee_instances_root.display(),
            result.error.as_deref().unwrap_or("publication has no exact applied receipt")));
    }
    Ok(1)
}

fn validate_existing_root(path: &Path, expected: &EeInstance, bytes: &[u8]) -> Result<(), String> {
    let actual: EeInstance = serde_json::from_slice(bytes).map_err(|error| {
        format!(
            "bootstrap EE instance '{}' is invalid: {error}",
            path.display()
        )
    })?;
    if actual.ee_id != expected.ee_id
        || actual.template != expected.template
        || actual.parent_ee.is_some()
        || actual.goal.trim().is_empty()
    {
        return Err(format!(
            "bootstrap EE instance '{}' does not declare root identity/template/goal for '{}'",
            path.display(),
            expected.ee_id.as_str()
        ));
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    if EeId::new(stem) != expected.ee_id {
        return Err(format!(
            "boot EE instance path '{}' does not match id '{}'",
            path.display(),
            expected.ee_id.as_str()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry")
    }

    #[test]
    fn boot_ees_are_durable_and_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let instances = root.path().canonicalize().unwrap().join("ee_instances");
        assert_eq!(
            ensure_bootstrap_ee_instances(&registry_root(), &instances).unwrap(),
            1
        );
        assert_eq!(
            ensure_bootstrap_ee_instances(&registry_root(), &instances).unwrap(),
            0
        );
        let ids = std::fs::read_dir(&instances)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["ee_existence.json"]);
    }

    #[test]
    fn normal_boot_never_resurrects_a_deleted_root() {
        let root = tempfile::tempdir().unwrap();
        let instances = root.path().canonicalize().unwrap().join("ee_instances");
        ensure_bootstrap_ee_instances(&registry_root(), &instances).unwrap();
        let path = instances.join("ee_existence.json");
        std::fs::remove_file(&path).unwrap();
        assert!(ensure_bootstrap_ee_instances(&registry_root(), &instances).is_err());
        assert!(!path.exists());
    }
}
