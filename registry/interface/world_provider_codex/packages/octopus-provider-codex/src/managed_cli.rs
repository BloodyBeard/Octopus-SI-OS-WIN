use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MANAGED_CLI_RECEIPT: &str = "codex-cli-receipt.json";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedCliImport {
    pub executable: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedCliReceipt {
    pub schema_version: u32,
    pub version: String,
    pub release_url: String,
    pub archive_sha256: String,
    pub executable: String,
    pub executable_sha256: String,
    pub executable_bytes: u64,
    pub schema_baseline: String,
    pub schemas: std::collections::BTreeMap<String, String>,
    pub legal: std::collections::BTreeMap<String, String>,
    pub signing: Option<ManagedCliSigning>,
    pub companions: std::collections::BTreeMap<String, ManagedCliCompanion>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedCliCompanion {
    pub archive_sha256: String,
    pub executable: String,
    pub executable_sha256: String,
    pub executable_bytes: u64,
    pub signing: Option<ManagedCliSigning>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedCliSigning {
    pub kind: String,
    pub before_sha256: String,
}

fn sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn verify_managed_cli(binary: &Path) -> Result<ManagedCliReceipt, String> {
    let fail = |error: &str| format!("managed Codex CLI '{}': {error}", binary.display());
    let directory = binary
        .parent()
        .ok_or_else(|| fail("binary has no directory"))?;
    let receipt_path = directory.join(MANAGED_CLI_RECEIPT);
    let metadata = std::fs::symlink_metadata(&receipt_path).map_err(|_| {
        fail("distribution receipt is missing; install the Octopus provider package instead of a global CLI")
    })?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(fail("distribution receipt must be a bounded regular file"));
    }
    let receipt: ManagedCliReceipt = serde_json::from_slice(
        &std::fs::read(&receipt_path).map_err(|error| fail(&error.to_string()))?,
    )
    .map_err(|error| fail(&error.to_string()))?;
    if receipt.schema_version != 2
        || receipt.executable != "codex"
        || receipt.schema_baseline != format!("rust-v{}", receipt.version)
        || receipt.release_url
            != format!(
                "https://github.com/openai/codex/releases/tag/rust-v{}",
                receipt.version
            )
        || !sha256_hex(&receipt.archive_sha256)
        || !sha256_hex(&receipt.executable_sha256)
        || receipt.executable_bytes == 0
        || receipt.executable_bytes > 512 * 1024 * 1024
        || crate::compatibility::release_version(&receipt.version).is_err()
        || receipt.schemas.is_empty()
        || receipt
            .schemas
            .iter()
            .any(|(name, hash)| name.is_empty() || !sha256_hex(hash))
        || !receipt.legal.contains_key("LICENSE")
        || receipt
            .legal
            .iter()
            .any(|(name, hash)| !matches!(name.as_str(), "LICENSE" | "NOTICE") || !sha256_hex(hash))
        || receipt
            .signing
            .as_ref()
            .is_some_and(|signing| signing.kind != "ad-hoc" || !sha256_hex(&signing.before_sha256))
        || receipt.companions.len() != 1
        || !receipt.companions.contains_key("codex-code-mode-host")
    {
        return Err(fail(
            "distribution receipt has invalid release or integrity evidence",
        ));
    }
    for (name, expected) in &receipt.legal {
        let path = directory.join(name);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| fail(&format!("upstream {name} is missing: {error}")))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 {
            return Err(fail("upstream attribution must be a bounded regular file"));
        }
        let bytes = std::fs::read(path).map_err(|error| fail(&error.to_string()))?;
        if format!("{:x}", Sha256::digest(bytes)) != *expected {
            return Err(fail(
                "upstream attribution differs from its attested SHA-256",
            ));
        }
    }
    verify_executable(binary, receipt.executable_bytes, &receipt.executable_sha256)?;
    for (name, companion) in &receipt.companions {
        if companion.executable != *name
            || !sha256_hex(&companion.archive_sha256)
            || !sha256_hex(&companion.executable_sha256)
            || companion.executable_bytes == 0
            || companion.executable_bytes > 512 * 1024 * 1024
            || companion.signing.as_ref().is_some_and(|signing| {
                signing.kind != "ad-hoc" || !sha256_hex(&signing.before_sha256)
            })
        {
            return Err(fail("companion receipt has invalid integrity evidence"));
        }
        verify_executable(
            &directory.join(name),
            companion.executable_bytes,
            &companion.executable_sha256,
        )?;
    }
    Ok(receipt)
}

fn verify_executable(
    binary: &Path,
    expected_bytes: u64,
    expected_hash: &str,
) -> Result<(), String> {
    let fail = |error: &str| format!("managed Codex executable '{}': {error}", binary.display());
    let before = std::fs::symlink_metadata(binary).map_err(|error| fail(&error.to_string()))?;
    if !before.is_file() || before.len() != expected_bytes {
        return Err(fail("binary must be a regular file matching its receipt"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if before.permissions().mode() & 0o111 == 0 {
            return Err(fail("binary is not executable"));
        }
    }
    let mut file = std::fs::File::open(binary).map_err(|error| fail(&error.to_string()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| fail(&error.to_string()))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let after = std::fs::symlink_metadata(binary).map_err(|error| fail(&error.to_string()))?;
    if before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || !after.is_file()
        || format!("{:x}", hash.finalize()) != expected_hash
    {
        return Err(fail("binary changed or differs from its attested SHA-256"));
    }
    Ok(())
}

impl ManagedCliImport {
    fn relative_path(&self) -> Result<PathBuf, String> {
        let relative = self
            .executable
            .strip_prefix("interface://")
            .ok_or_else(|| {
                "managed Codex CLI requires an explicit interface:// executable import".to_string()
            })?;
        let components = relative.split('/').collect::<Vec<_>>();
        if components.len() < 4
            || components.last() != Some(&"codex")
            || components[1] != "packages"
            || components.iter().any(|component| {
                component.is_empty()
                    || *component == "."
                    || *component == ".."
                    || !component.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
                    })
            })
        {
            return Err("managed Codex CLI import has invalid owner or path components".into());
        }
        Ok(PathBuf::from(relative))
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        self.relative_path().map(|_| ())
    }

    pub(crate) fn resolve(&self) -> Result<PathBuf, String> {
        let raw = std::env::var("OCTOPUS_ROOT_SLOTS_JSON").map_err(|_| {
            "managed Codex CLI requires the declared registry_interface_root slot".to_string()
        })?;
        let slots: std::collections::BTreeMap<String, PathBuf> = serde_json::from_str(&raw)
            .map_err(|error| format!("managed Codex CLI root slots are malformed: {error}"))?;
        let root = slots
            .get("registry_interface_root")
            .filter(|root| root.is_absolute())
            .ok_or_else(|| {
                "managed Codex CLI has no absolute registry_interface_root".to_string()
            })?;
        self.resolve_in(root)
    }

    pub(crate) fn resolve_in(&self, root: &Path) -> Result<PathBuf, String> {
        let relative = self.relative_path()?;
        let canonical_root = std::fs::canonicalize(root)
            .map_err(|error| format!("managed Codex registry root is unavailable: {error}"))?;
        let binary = root.join(relative);
        let canonical_binary = std::fs::canonicalize(&binary)
            .map_err(|error| format!("managed Codex import is unavailable: {error}"))?;
        if !canonical_binary.starts_with(canonical_root) {
            return Err(
                "managed Codex executable import escapes its declared registry root".into(),
            );
        }
        verify_managed_cli(&binary)?;
        Ok(canonical_binary)
    }
}

#[cfg(test)]
#[path = "../tests/support/managed_cli_import.rs"]
mod tests;
