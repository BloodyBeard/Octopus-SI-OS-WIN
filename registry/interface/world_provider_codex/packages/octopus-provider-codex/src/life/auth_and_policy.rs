use super::*;

pub(crate) fn required_policy_u32(envelope: &InterfaceInvocationEnvelope, name: &str) -> Result<u32, String> {
    let values = envelope
        .interface_policy()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "codex invocation carries no formed interface policy".to_string())?;
    let value = values
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("codex formed interface policy carries no unsigned '{name}'"))?;
    u32::try_from(value).map_err(|_| format!("codex formed interface policy '{name}' exceeds u32"))
}

pub(crate) fn enforce_formed_model_policy(
    descriptor: &InterfaceDescriptor,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<(), String> {
    let label = envelope
        .read_artifact_manifest()
        .map_err(|error| error.to_string())?
        .label;
    if label.starts_with("provider.account.")
        || label.starts_with("provider.models.")
        || label.starts_with("provider.capabilities")
    {
        return Ok(());
    }
    let metadata = &descriptor
        .interface_driver_config
        .as_ref()
        .ok_or_else(|| {
            format!(
                "Codex interface '{}' has no interface_driver_config",
                descriptor.id
            )
        })?
        .private_config;
    let model = metadata
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|model| {
            !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
        })
        .ok_or_else(|| {
            format!(
                "Codex interface '{}' has no valid provider-local model selection",
                descriptor.id
            )
        })?;
    let policy = envelope
        .interface_policy()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Codex invocation carries no formed interface policy".to_string())?;
    let values = policy
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Codex formed interface policy models must be a string array".to_string())?;
    if values.is_empty() {
        return Err("Codex formed interface policy models is empty".to_string());
    }
    let mut models = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let candidate = value.as_str().filter(|candidate| {
            !candidate.is_empty()
                && candidate.trim() == *candidate
                && !candidate.contains(['\r', '\n', '\0'])
        });
        let Some(candidate) = candidate else {
            return Err(format!(
                "Codex formed interface policy models[{index}] is not a trimmed string"
            ));
        };
        if !models.insert(candidate) {
            return Err(format!(
                "Codex formed interface policy models repeats '{candidate}'"
            ));
        }
    }
    if !models.contains(model) {
        return Err(format!(
            "Codex selected model '{model}' is outside formed interface policy models"
        ));
    }
    Ok(())
}

pub(crate) fn load_own_descriptor(ctx: &InterfaceLiftContext) -> Result<InterfaceDescriptor, String> {
    ctx.resolved_interface_descriptor()
}

pub(crate) fn load_current_own_descriptor(ctx: &InterfaceLiftContext) -> Result<InterfaceDescriptor, String> {
    let interface_dir = ctx.interface_registry_dir();
    let registry_root = interface_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            format!(
                "interface registry directory '{}' has no registry root",
                interface_dir.display()
            )
        })?;
    octopus_interface_transport::resolve_interface_descriptor(registry_root, &ctx.interface)?
        .ok_or_else(|| {
            format!(
                "current descriptor for interface '{}' disappeared from the registry",
                ctx.interface.as_str()
            )
        })
}

pub(crate) fn resolve_provider_auth_store(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    mode: &ProviderConnectionMode,
) -> Result<PathBuf, String> {
    match mode {
        ProviderConnectionMode::Native => resolve_declared_private_auth_store(ctx, descriptor),
        ProviderConnectionMode::OpenCodex(_) => prepare_private_auth_store_home(
            &ctx.interface_var_dir(),
            Path::new("codex-home"),
            ctx.required_host_io_limits()?.max_payload_bytes,
        ),
    }
}

pub(crate) fn resolve_declared_private_auth_store(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
) -> Result<PathBuf, String> {
    let Some(residual) = descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.residual_ambient.as_ref())
        .and_then(serde_json::Value::as_object)
    else {
        return Err(
            "Codex provider descriptor must declare residual_ambient.private_auth_store"
                .to_string(),
        );
    };
    let store = residual.get("private_auth_store").ok_or_else(|| {
        "Codex provider descriptor must declare residual_ambient.private_auth_store".to_string()
    })?;
    let store = store
        .as_object()
        .ok_or_else(|| "private_auth_store must be a JSON object".to_string())?;
    let expected = BTreeSet::from(["auth_file_env"]);
    let observed = store.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if observed != expected {
        return Err("private_auth_store contains undeclared fields".to_string());
    }
    let auth_file_env = store
        .get("auth_file_env")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "private_auth_store.auth_file_env must be a string".to_string())?;
    if auth_file_env != MACHINE_CODEX_AUTH_FILE_ENV {
        return Err(format!(
            "private_auth_store.auth_file_env must be '{MACHINE_CODEX_AUTH_FILE_ENV}'"
        ));
    }
    let declared_auth_file = std::env::var_os(MACHINE_CODEX_AUTH_FILE_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!("private_auth_store requires inherited {MACHINE_CODEX_AUTH_FILE_ENV}")
        })?;
    if !declared_auth_file.is_absolute() {
        return Err(format!(
            "{MACHINE_CODEX_AUTH_FILE_ENV} must name an absolute Codex auth file"
        ));
    }
    let maximum_bytes = ctx.required_host_io_limits()?.max_payload_bytes;
    prepare_machine_codex_auth_store_home(&declared_auth_file, maximum_bytes)
}

pub(crate) fn prepare_machine_codex_auth_store_home(
    declared_auth_file: &Path,
    maximum_bytes: u64,
) -> Result<PathBuf, String> {
    if declared_auth_file.file_name() != Some(std::ffi::OsStr::new("auth.json")) {
        return Err(format!(
            "machine Codex auth file '{}' must end in auth.json",
            declared_auth_file.display()
        ));
    }
    let declared_home = declared_auth_file.parent().ok_or_else(|| {
        format!(
            "machine Codex auth file '{}' has no parent directory",
            declared_auth_file.display()
        )
    })?;
    let parent = declared_home.parent().ok_or_else(|| {
        format!(
            "machine Codex home '{}' has no parent directory",
            declared_home.display()
        )
    })?;
    let name = declared_home.file_name().ok_or_else(|| {
        format!(
            "machine Codex home '{}' has no final path component",
            declared_home.display()
        )
    })?;
    let prepared = prepare_private_auth_store_home(parent, Path::new(name), maximum_bytes)?;
    if prepared != declared_home {
        return Err(format!(
            "machine Codex home resolved to '{}' instead of declared '{}'",
            prepared.display(),
            declared_home.display()
        ));
    }
    Ok(prepared)
}

pub(crate) fn prepare_private_auth_store_home(
    root: &Path,
    relative_home: &Path,
    maximum_bytes: u64,
) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(root).map_err(|error| {
        format!(
            "private auth-store root '{}' cannot be resolved: {error}",
            root.display()
        )
    })?;
    let mut auth_store_home = root.clone();
    for component in relative_home.components() {
        let std::path::Component::Normal(segment) = component else {
            return Err("private auth-store home is not relative".to_string());
        };
        auth_store_home.push(segment);
        match std::fs::symlink_metadata(&auth_store_home) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(format!(
                    "private auth-store path '{}' is not a regular directory",
                    auth_store_home.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&auth_store_home).map_err(|error| {
                    format!(
                        "private auth-store path '{}' cannot be created: {error}",
                        auth_store_home.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "private auth-store path '{}' cannot be inspected: {error}",
                    auth_store_home.display()
                ));
            }
        }
    }
    let auth_store_home = std::fs::canonicalize(&auth_store_home).map_err(|error| {
        format!(
            "private auth-store home '{}' cannot be resolved: {error}",
            auth_store_home.display()
        )
    })?;
    if !auth_store_home.starts_with(&root) {
        return Err(format!(
            "private auth-store home '{}' escapes its declared root",
            auth_store_home.display()
        ));
    }
    let metadata = std::fs::symlink_metadata(&auth_store_home).map_err(|error| {
        format!(
            "private auth-store home '{}' cannot be inspected: {error}",
            auth_store_home.display()
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "private auth-store home '{}' is not a regular directory",
            auth_store_home.display()
        ));
    }
    let auth_file = auth_store_home.join("auth.json");
    match std::fs::symlink_metadata(&auth_file) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            if metadata.len() > maximum_bytes {
                return Err(format!(
                    "private auth-store file '{}' exceeds {} bytes",
                    auth_file.display(),
                    maximum_bytes
                ));
            }
            let bytes = std::fs::read(&auth_file).map_err(|error| {
                format!(
                    "private auth-store file '{}' cannot be read: {error}",
                    auth_file.display()
                )
            })?;
            if u64::try_from(bytes.len()).ok() != Some(metadata.len()) {
                return Err(format!(
                    "private auth-store file '{}' changed while being read",
                    auth_file.display()
                ));
            }
        }
        Ok(_) => {
            return Err(format!(
                "private auth-store file '{}' is not a regular file",
                auth_file.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "private auth-store file '{}' cannot be inspected: {error}",
                auth_file.display()
            ));
        }
    }
    Ok(auth_store_home)
}
