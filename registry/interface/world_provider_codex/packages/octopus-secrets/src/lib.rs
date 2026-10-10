use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

mod keyring_store;
mod local;
mod sanitizer;

pub use keyring_store::CredentialStoreError;
pub use keyring_store::DefaultKeyringStore;
pub use keyring_store::KeyringStore;
pub use keyring_store::LocalFileKeyringStore;
pub use local::LocalSecretsBackend;
pub use local::LocalSecretsNamespace;
pub use sanitizer::redact_secrets;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    pub use crate::keyring_store::tests::MockKeyringStore;
}

const KEYRING_SERVICE: &str = "octopus";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretName(String);

impl SecretName {
    pub fn new(raw: &str) -> Result<Self> {
        anyhow::ensure!(!raw.is_empty(), "secret name must not be empty");
        anyhow::ensure!(
            raw.trim() == raw,
            "secret name must not contain surrounding whitespace"
        );
        anyhow::ensure!(
            raw.chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_'),
            "secret name must contain only A-Z, 0-9, or _"
        );
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for SecretName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct SecretScope(Option<String>);

impl SecretScope {
    pub fn global() -> Self {
        Self::default()
    }

    pub fn named(scope: impl Into<String>) -> Result<Self> {
        let scope = scope.into();
        anyhow::ensure!(!scope.is_empty(), "secret scope must not be empty");
        anyhow::ensure!(
            scope.trim() == scope,
            "secret scope must not contain surrounding whitespace"
        );
        anyhow::ensure!(
            scope.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            }),
            "secret scope must contain only ASCII letters, digits, _, -, or ."
        );
        Ok(Self(Some(scope)))
    }

    pub fn name(&self) -> Option<&str> {
        self.0.as_deref()
    }

    pub(crate) fn canonical_key(&self, name: &SecretName) -> String {
        match self.name() {
            None => format!("global/{}", name.as_str()),
            Some(scope) => format!("scope/{scope}/{}", name.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretListEntry {
    pub scope: SecretScope,
    pub name: SecretName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum SecretsBackendKind {
    #[default]
    Local,
}

pub trait SecretsBackend: Send + Sync {
    fn set(&self, scope: &SecretScope, name: &SecretName, value: &str) -> Result<()>;
    fn get(&self, scope: &SecretScope, name: &SecretName) -> Result<Option<String>>;
    fn delete(&self, scope: &SecretScope, name: &SecretName) -> Result<bool>;
    fn list(&self, scope_filter: Option<&SecretScope>) -> Result<Vec<SecretListEntry>>;
}

#[derive(Clone)]
pub struct SecretsManager {
    backend: Arc<dyn SecretsBackend>,
}

impl SecretsManager {
    pub fn new(octopus_home: PathBuf, backend_kind: SecretsBackendKind) -> Self {
        let backend: Arc<dyn SecretsBackend> = match backend_kind {
            SecretsBackendKind::Local => {
                let keyring_store: Arc<dyn KeyringStore> = Arc::new(DefaultKeyringStore);
                Arc::new(LocalSecretsBackend::new(octopus_home, keyring_store))
            }
        };
        Self { backend }
    }

    pub fn new_with_keyring_store(
        octopus_home: PathBuf,
        backend_kind: SecretsBackendKind,
        keyring_store: Arc<dyn KeyringStore>,
    ) -> Self {
        let backend: Arc<dyn SecretsBackend> = match backend_kind {
            SecretsBackendKind::Local => {
                Arc::new(LocalSecretsBackend::new(octopus_home, keyring_store))
            }
        };
        Self { backend }
    }

    pub fn new_with_keyring_store_and_namespace(
        octopus_home: PathBuf,
        backend_kind: SecretsBackendKind,
        keyring_store: Arc<dyn KeyringStore>,
        namespace: LocalSecretsNamespace,
    ) -> Self {
        let backend: Arc<dyn SecretsBackend> = match backend_kind {
            SecretsBackendKind::Local => Arc::new(LocalSecretsBackend::new_with_namespace(
                octopus_home,
                keyring_store,
                namespace,
            )),
        };
        Self { backend }
    }

    pub fn set(&self, scope: &SecretScope, name: &SecretName, value: &str) -> Result<()> {
        self.backend.set(scope, name, value)
    }

    pub fn get(&self, scope: &SecretScope, name: &SecretName) -> Result<Option<String>> {
        self.backend.get(scope, name)
    }

    pub fn delete(&self, scope: &SecretScope, name: &SecretName) -> Result<bool> {
        self.backend.delete(scope, name)
    }

    pub fn list(&self, scope_filter: Option<&SecretScope>) -> Result<Vec<SecretListEntry>> {
        self.backend.list(scope_filter)
    }
}

pub fn compute_keyring_account(octopus_home: &std::path::Path) -> String {
    let canonical = octopus_home
        .canonicalize()
        .unwrap_or_else(|_| octopus_home.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let short = hex.get(..16).unwrap_or(hex.as_str());
    format!("secrets|{short}")
}

pub(crate) fn keyring_service() -> &'static str {
    KEYRING_SERVICE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyring_store::tests::MockKeyringStore;
    use pretty_assertions::assert_eq;

    #[test]
    fn manager_round_trips_local_backend() -> Result<()> {
        let octopus_home = tempfile::tempdir().expect("tempdir");
        let keyring = Arc::new(MockKeyringStore::default());
        let manager = SecretsManager::new_with_keyring_store(
            octopus_home.path().to_path_buf(),
            SecretsBackendKind::Local,
            keyring,
        );
        let scope = SecretScope::global();
        let name = SecretName::new("GITHUB_TOKEN")?;

        manager.set(&scope, &name, "token-1")?;
        assert_eq!(manager.get(&scope, &name)?, Some("token-1".to_string()));

        let interface_scope = SecretScope::named("world_telegram")?;
        manager.set(&interface_scope, &name, "token-2")?;
        assert_eq!(
            manager.get(&interface_scope, &name)?,
            Some("token-2".to_string())
        );
        assert_eq!(manager.list(Some(&interface_scope))?.len(), 1);
        assert!(SecretScope::named("../escape").is_err());

        let listed = manager.list(None)?;
        assert_eq!(listed.len(), 2);

        assert!(manager.delete(&scope, &name)?);
        assert_eq!(manager.get(&scope, &name)?, None);
        Ok(())
    }
}
