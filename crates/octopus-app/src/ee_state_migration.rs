use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use octopus_descriptors::{EeId, EeTemplateId};
use octopus_ee::EeInstance;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostManifest {
    host_manifest_version: u32,
    root_refs: std::collections::BTreeMap<String, PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyEeInstance {
    ee_id: String,
    template: String,
    parent_ee: Option<String>,
    goal: String,
    #[serde(default)]
    status: Option<String>,
}

struct PlannedWrite {
    path: PathBuf,
    original: Vec<u8>,
    replacement: Vec<u8>,
    permissions: fs::Permissions,
}

pub struct MigrationOutcome {
    pub migrated: usize,
    pub current: usize,
}

pub fn migrate(octopus_home: &Path) -> Result<MigrationOutcome, String> {
    let ee_root = resolve_ee_root(octopus_home)?;
    let mut entries = fs::read_dir(&ee_root)
        .map_err(|error| {
            format!(
                "cannot read EE instance root '{}': {error}",
                ee_root.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            format!(
                "cannot enumerate EE instance root '{}': {error}",
                ee_root.display()
            )
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut current = 0;
    let mut writes = Vec::new();
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect EE instance '{}': {error}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "EE instance '{}' must be a regular file",
                path.display()
            ));
        }
        let original = fs::read(&path)
            .map_err(|error| format!("cannot read EE instance '{}': {error}", path.display()))?;
        if let Ok(instance) = serde_json::from_slice::<EeInstance>(&original) {
            require_path_identity(&path, &instance)?;
            current += 1;
            continue;
        }
        let legacy: LegacyEeInstance = serde_json::from_slice(&original).map_err(|error| {
            format!(
                "EE instance '{}' matches neither schema: {error}",
                path.display()
            )
        })?;
        if let Some(status) = legacy.status.as_deref() {
            if status != "active" && status != "archived" {
                return Err(format!(
                    "EE instance '{}' has unknown retired status '{status}'",
                    path.display()
                ));
            }
        }
        let instance = EeInstance {
            ee_id: EeId::new(legacy.ee_id),
            template: EeTemplateId::new(legacy.template),
            parent_ee: legacy.parent_ee.map(EeId::new),
            goal: legacy.goal,
            config: Default::default(),
            policy_narrowing: Default::default(),
        };
        require_path_identity(&path, &instance)?;
        let mut replacement = serde_json::to_vec_pretty(&instance).map_err(|error| {
            format!("cannot serialize EE instance '{}': {error}", path.display())
        })?;
        replacement.push(b'\n');
        writes.push(PlannedWrite {
            path,
            original,
            replacement,
            permissions: metadata.permissions(),
        });
    }

    for (index, write) in writes.iter().enumerate() {
        let observed = fs::read(&write.path).map_err(|error| {
            format!(
                "cannot re-read EE instance '{}': {error}",
                write.path.display()
            )
        })?;
        if observed != write.original {
            return Err(format!(
                "EE instance '{}' changed after migration preflight",
                write.path.display()
            ));
        }
        let temporary =
            write
                .path
                .with_extension(format!("json.migrate-{}-{}", std::process::id(), index));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| {
                format!(
                    "cannot create migration file '{}': {error}",
                    temporary.display()
                )
            })?;
        fs::set_permissions(&temporary, write.permissions.clone()).map_err(|error| {
            format!(
                "cannot preserve permissions on '{}': {error}",
                temporary.display()
            )
        })?;
        file.write_all(&write.replacement)
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                format!(
                    "cannot persist migration file '{}': {error}",
                    temporary.display()
                )
            })?;
        fs::rename(&temporary, &write.path).map_err(|error| {
            format!(
                "cannot commit EE instance migration '{}': {error}",
                write.path.display()
            )
        })?;
    }
    fs::File::open(&ee_root)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "cannot sync EE instance root '{}': {error}",
                ee_root.display()
            )
        })?;

    Ok(MigrationOutcome {
        migrated: writes.len(),
        current,
    })
}

fn resolve_ee_root(octopus_home: &Path) -> Result<PathBuf, String> {
    let host_path = octopus_home.join("host.json");
    let root = if host_path.exists() {
        let manifest: HostManifest =
            serde_json::from_slice(&fs::read(&host_path).map_err(|error| {
                format!(
                    "cannot read host manifest '{}': {error}",
                    host_path.display()
                )
            })?)
            .map_err(|error| {
                format!(
                    "host manifest '{}' is invalid: {error}",
                    host_path.display()
                )
            })?;
        if manifest.host_manifest_version != 1 {
            return Err(format!(
                "host manifest '{}' has unsupported version {}",
                host_path.display(),
                manifest.host_manifest_version
            ));
        }
        manifest
            .root_refs
            .get("ee_instances_root")
            .cloned()
            .ok_or_else(|| {
                format!(
                    "host manifest '{}' has no ee_instances_root",
                    host_path.display()
                )
            })?
    } else {
        octopus_home.join("state/ee_instances")
    };
    if !root.is_absolute() {
        return Err(format!(
            "EE instance root '{}' is not absolute",
            root.display()
        ));
    }
    fs::canonicalize(&root).map_err(|error| {
        format!(
            "cannot canonicalize EE instance root '{}': {error}",
            root.display()
        )
    })
}

fn require_path_identity(path: &Path, instance: &EeInstance) -> Result<(), String> {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if stem != instance.ee_id.as_str() {
        return Err(format!(
            "EE instance path '{}' does not match id '{}'",
            path.display(),
            instance.ee_id.as_str()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_is_explicit_strict_and_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let ee_root = home.path().join("state/ee_instances");
        fs::create_dir_all(&ee_root).unwrap();
        fs::write(
            ee_root.join("ee_root.json"),
            br#"{"ee_id":"ee_root","template":"existence","parent_ee":null,"goal":"existence","status":"active"}"#,
        )
        .unwrap();

        let first = migrate(home.path()).unwrap();
        assert_eq!(first.migrated, 1);
        assert_eq!(first.current, 0);
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(ee_root.join("ee_root.json")).unwrap()).unwrap();
        assert_eq!(value["template_name"], "existence");
        assert!(value.get("parent_ee_id").is_some());
        assert!(value.get("template").is_none());
        assert!(value.get("parent_ee").is_none());
        assert!(value.get("status").is_none());

        let second = migrate(home.path()).unwrap();
        assert_eq!(second.migrated, 0);
        assert_eq!(second.current, 1);
    }
}
