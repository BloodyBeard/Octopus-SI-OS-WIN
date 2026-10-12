use super::{
    InterfaceLiftContext, ToolProcessDeclaration, WorkerEnvKindSpec, registry_file_fingerprint,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

#[derive(Default)]
pub(super) struct ExecutionInputCache {
    files: BTreeMap<PathBuf, (super::RegistryFileFingerprint, String)>,
    recent: VecDeque<PathBuf>,
}

pub(super) struct ExecutionInputs {
    pub program: String,
    pub base: PathBuf,
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub readable_files: Vec<PathBuf>,
    pub hash: String,
}

pub(super) fn kind_owner_root(
    ctx: &InterfaceLiftContext,
    kind: &WorkerEnvKindSpec,
) -> Result<PathBuf, String> {
    kind_owner_root_from_interface(&ctx.interface_registry_dir(), kind)
}

pub(super) fn kind_owner_root_from_interface(
    interface_dir: &Path,
    kind: &WorkerEnvKindSpec,
) -> Result<PathBuf, String> {
    let registry = interface_dir
        .parent()
        .and_then(Path::parent)
        .ok_or("tool door has no registry root")?;
    let kinds = std::fs::canonicalize(registry.join("worker_environment_kind"))
        .map_err(|error| format!("worker-environment registry is unavailable: {error}"))?;
    let owner = std::fs::canonicalize(kinds.join(kind.worker_environment_kind_id.as_str()))
        .map_err(|error| format!("tool executable owner is unavailable: {error}"))?;
    if owner != kinds.join(kind.worker_environment_kind_id.as_str()) {
        return Err("tool executable owner escapes its exact registry folder".into());
    }
    Ok(owner)
}

pub(super) fn owner_file(declared: &str, owner: &Path) -> Result<PathBuf, String> {
    let relative = declared
        .strip_prefix("owner://")
        .ok_or("owner file has no owner:// prefix")?;
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || !path.starts_with("packages")
    {
        return Err(format!("invalid owner package file '{declared}'"));
    }
    let candidate = owner.join(path);
    let metadata = std::fs::symlink_metadata(&candidate).map_err(|error| {
        format!(
            "owner package file '{}' is unavailable: {error}",
            candidate.display()
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "owner package file '{}' is not a regular file",
            candidate.display()
        ));
    }
    let resolved = std::fs::canonicalize(&candidate).map_err(|error| error.to_string())?;
    if !resolved.starts_with(owner) || resolved != candidate {
        return Err(format!(
            "owner package file '{}' escapes its exact owner",
            candidate.display()
        ));
    }
    Ok(resolved)
}

impl ExecutionInputCache {
    pub fn resolve(
        &mut self,
        ctx: &InterfaceLiftContext,
        declaration: &ToolProcessDeclaration,
        kind: Option<&WorkerEnvKindSpec>,
        environment: &BTreeMap<String, OsString>,
    ) -> Result<ExecutionInputs, String> {
        let installation = ctx.installation_root()?;
        let has_owner_files = declaration.bin.starts_with("owner://")
            || declaration
                .args
                .iter()
                .chain(&declaration.required_files)
                .any(|value| value.starts_with("owner://"));
        let owner = has_owner_files
            .then(|| {
                kind_owner_root(
                    ctx,
                    kind.ok_or("owner file has no formed worker-environment kind")?,
                )
            })
            .transpose()?;
        let (program, base) = match declaration.bin.strip_prefix("owner://") {
            Some(_) => {
                let owner = owner.as_ref().ok_or("tool executable has no exact owner")?;
                (
                    owner_file(&declaration.bin, owner)?
                        .to_string_lossy()
                        .into_owned(),
                    owner.clone(),
                )
            }
            None => (declaration.bin.clone(), installation.clone()),
        };
        let executable = super::process_authority::resolve_executable(
            &program,
            &base,
            environment.get("PATH").map(OsString::as_os_str),
        )?;
        if declaration.bin.starts_with("owner://")
            && !executable.starts_with(owner.as_ref().ok_or("tool executable has no owner")?)
        {
            return Err("tool executable escapes its exact kind owner".into());
        }
        let mut readable_files = BTreeSet::new();
        let mut arguments = Vec::new();
        for argument in &declaration.args {
            if argument.starts_with("owner://") {
                let file = owner_file(
                    argument,
                    owner.as_ref().ok_or("tool argument has no owner")?,
                )?;
                readable_files.insert(file.clone());
                arguments.push(file.into_os_string());
            } else {
                arguments.push(OsString::from(argument));
            }
        }
        for declared in &declaration.required_files {
            readable_files.insert(if declared.starts_with("owner://") {
                owner_file(
                    declared,
                    owner.as_ref().ok_or("required file has no owner")?,
                )?
            } else {
                super::process_authority::resolve_required_repository_file(declared, &installation)?
            });
        }
        let mut paths = readable_files.clone();
        paths.insert(executable.clone());
        let mut hasher = blake3::Hasher::new();
        for path in paths {
            let before = registry_file_fingerprint(&path)?
                .ok_or_else(|| format!("execution input '{}' disappeared", path.display()))?;
            let digest = match self.files.get(&path) {
                Some((stamp, digest)) if stamp == &before => digest.clone(),
                _ => {
                    let mut options = std::fs::OpenOptions::new();
                    options.read(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
                    }
                    let mut file = options.open(&path).map_err(|error| error.to_string())?;
                    let metadata = file.metadata().map_err(|error| error.to_string())?;
                    if !metadata.is_file() || metadata.len() != before.len {
                        return Err("execution input changed before being read".into());
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        if (metadata.dev(), metadata.ino(), metadata.ctime(), metadata.ctime_nsec()) != before.identity_and_change {
                            return Err("execution input changed before being read".into());
                        }
                    }
                    let mut content = blake3::Hasher::new();
                    let mut buffer = [0u8; 65536];
                    let mut count = 0u64;
                    loop {
                        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
                        if read == 0 {
                            break;
                        }
                        count = count
                            .checked_add(read as u64)
                            .ok_or("execution input length overflowed")?;
                        if count > before.len {
                            return Err("execution input changed while being read".into());
                        }
                        content.update(&buffer[..read]);
                    }
                    if count != before.len
                        || registry_file_fingerprint(&path)?.as_ref() != Some(&before)
                    {
                        return Err("execution input changed while being read".into());
                    }
                    let digest = content.finalize().to_hex().to_string();
                    self.files.insert(path.clone(), (before, digest.clone()));
                    digest
                }
            };
            self.recent.retain(|cached| cached != &path);
            self.recent.push_back(path.clone());
            if self.recent.len() > 256 {
                if let Some(expired) = self.recent.pop_front() {
                    self.files.remove(&expired);
                }
            }
            super::append_tool_process_identity_part(
                &mut hasher,
                b"path",
                path.as_os_str().as_encoded_bytes(),
            );
            super::append_tool_process_identity_part(&mut hasher, b"content", digest.as_bytes());
        }
        Ok(ExecutionInputs {
            program,
            base,
            executable,
            arguments,
            readable_files: readable_files.into_iter().collect(),
            hash: hasher.finalize().to_hex().to_string(),
        })
    }
}
