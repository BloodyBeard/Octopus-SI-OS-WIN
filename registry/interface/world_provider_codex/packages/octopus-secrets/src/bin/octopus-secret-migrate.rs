use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octopus_secrets::{
    LocalFileKeyringStore, LocalSecretsNamespace, SecretScope, SecretsBackendKind, SecretsManager,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    legacy_octopus_home: PathBuf,
    interface_home: PathBuf,
    interface: String,
    #[serde(default)]
    source_scope: Option<String>,
    #[serde(default)]
    target_scope: Option<String>,
    #[serde(default = "default_namespace")]
    namespace: String,
}

#[derive(Serialize)]
struct Response {
    interface: String,
    migrated: usize,
    legacy_entries_remaining: usize,
}

fn default_namespace() -> String {
    "local".to_string()
}

fn main() {
    if let Err(error) = run() {
        eprintln!("octopus-secret-migrate failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|error| format!("stdin could not be read: {error}"))?;
    let request: Request =
        serde_json::from_slice(&input).map_err(|error| format!("request is malformed: {error}"))?;
    let response = migrate(request)?;
    let output = serde_json::to_vec(&response)
        .map_err(|error| format!("response could not be encoded: {error}"))?;
    std::io::stdout()
        .write_all(&output)
        .map_err(|error| format!("stdout could not be written: {error}"))
}

fn migrate(request: Request) -> Result<Response, String> {
    validate_request(&request)?;

    let namespace =
        LocalSecretsNamespace::new(request.namespace.clone()).map_err(|error| error.to_string())?;
    let source_scope = parse_scope(request.source_scope.as_deref(), &request.interface)?;
    let target_scope = parse_scope(request.target_scope.as_deref(), &request.interface)?;
    require_exact_interface_scope(&request.interface, &target_scope)?;

    let source = manager(&request.legacy_octopus_home, namespace.clone());
    let target = manager(&request.interface_home, namespace);
    let entries = source
        .list(Some(&source_scope))
        .map_err(|error| format!("legacy store could not be listed: {error}"))?;

    for entry in &entries {
        let value = source
            .get(&source_scope, &entry.name)
            .map_err(|error| format!("legacy secret '{}' could not be read: {error}", entry.name))?
            .ok_or_else(|| {
                format!(
                    "legacy secret '{}' disappeared during migration",
                    entry.name
                )
            })?;
        match target.get(&target_scope, &entry.name).map_err(|error| {
            format!("private secret '{}' could not be read: {error}", entry.name)
        })? {
            Some(existing) if existing != value => {
                return Err(format!(
                    "private secret '{}' already exists with different bytes; refusing to overwrite",
                    entry.name
                ));
            }
            Some(_) => {}
            None => target
                .set(&target_scope, &entry.name, &value)
                .map_err(|error| {
                    format!(
                        "private secret '{}' could not be written: {error}",
                        entry.name
                    )
                })?,
        }
        let persisted = target.get(&target_scope, &entry.name).map_err(|error| {
            format!(
                "private secret '{}' could not be verified: {error}",
                entry.name
            )
        })?;
        if persisted.as_deref() != Some(value.as_str()) {
            return Err(format!(
                "private secret '{}' did not verify after migration",
                entry.name
            ));
        }
    }

    for entry in &entries {
        source.delete(&source_scope, &entry.name).map_err(|error| {
            format!(
                "legacy secret '{}' could not be deleted: {error}",
                entry.name
            )
        })?;
    }
    let remaining = source
        .list(Some(&source_scope))
        .map_err(|error| format!("legacy store could not be verified after migration: {error}"))?;
    if !remaining.is_empty() {
        return Err("legacy scoped secrets remain after migration".to_string());
    }

    Ok(Response {
        interface: request.interface,
        migrated: entries.len(),
        legacy_entries_remaining: remaining.len(),
    })
}

fn validate_request(request: &Request) -> Result<(), String> {
    if !request.legacy_octopus_home.is_absolute() || !request.interface_home.is_absolute() {
        return Err("legacy_octopus_home and interface_home must be absolute".to_string());
    }
    let interface = SecretScope::named(request.interface.clone())
        .map_err(|error| format!("interface is invalid: {error}"))?;
    if interface.name() != Some(request.interface.as_str()) {
        return Err("interface identity could not be formed".to_string());
    }
    if request
        .interface_home
        .file_name()
        .and_then(|name| name.to_str())
        != Some(request.interface.as_str())
    {
        return Err("interface_home must end in the exact interface folder id".to_string());
    }
    if request.interface_home
        != request
            .legacy_octopus_home
            .join("interfaces")
            .join(&request.interface)
    {
        return Err(
            "interface_home must be legacy_octopus_home/interfaces/<exact-interface-id>"
                .to_string(),
        );
    }
    if same_path(&request.legacy_octopus_home, &request.interface_home) {
        return Err("legacy and interface-private secret homes must differ".to_string());
    }
    Ok(())
}

fn parse_scope(scope: Option<&str>, interface: &str) -> Result<SecretScope, String> {
    match scope {
        Some(scope) => SecretScope::named(scope.to_string()).map_err(|error| error.to_string()),
        None => SecretScope::named(interface.to_string()).map_err(|error| error.to_string()),
    }
}

fn require_exact_interface_scope(interface: &str, scope: &SecretScope) -> Result<(), String> {
    if scope.name() == Some(interface) {
        Ok(())
    } else {
        Err("target_scope must equal the exact interface id".to_string())
    }
}

fn manager(root: &Path, namespace: LocalSecretsNamespace) -> SecretsManager {
    SecretsManager::new_with_keyring_store_and_namespace(
        root.to_path_buf(),
        SecretsBackendKind::Local,
        Arc::new(LocalFileKeyringStore::new(
            root.join("secrets").join("keyring"),
        )),
        namespace,
    )
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

#[cfg(test)]
mod tests {
    use octopus_secrets::SecretName;

    use super::*;

    #[test]
    fn migration_moves_one_legacy_scope_into_the_exact_interface_store() {
        let root = tempfile::tempdir().unwrap();
        let legacy_home = root.path().join("octopus-home");
        let interface = "world_provider_openrouter";
        let interface_home = legacy_home.join("interfaces").join(interface);
        let namespace = LocalSecretsNamespace::new("migration-test").unwrap();
        let source_scope = SecretScope::named("world_provider_openrouter_old").unwrap();
        let target_scope = SecretScope::named(interface).unwrap();
        let name = SecretName::new("OPENROUTER_API_KEY").unwrap();
        manager(&legacy_home, namespace.clone())
            .set(&source_scope, &name, "migration-test-secret")
            .unwrap();

        let response = migrate(Request {
            legacy_octopus_home: legacy_home.clone(),
            interface_home: interface_home.clone(),
            interface: interface.to_string(),
            source_scope: Some("world_provider_openrouter_old".to_string()),
            target_scope: None,
            namespace: "migration-test".to_string(),
        })
        .unwrap();

        assert_eq!(response.migrated, 1);
        assert_eq!(response.legacy_entries_remaining, 0);
        assert_eq!(
            manager(&interface_home, namespace.clone())
                .get(&target_scope, &name)
                .unwrap()
                .as_deref(),
            Some("migration-test-secret")
        );
        assert!(manager(&legacy_home, namespace)
            .get(&source_scope, &name)
            .unwrap()
            .is_none());
    }
}
