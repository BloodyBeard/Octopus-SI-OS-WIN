use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

use octopus_substrate_contract::permission_profile::PermissionProfile;
use octopus_substrate_contract::shell_env_config::WindowsSandboxLevel;
use octopus_util_abspath::AbsolutePathBuf;

pub(crate) fn find_codex_home() -> std::io::Result<AbsolutePathBuf> {
    let codex_home_env = std::env::var("CODEX_HOME")
        .ok()
        .filter(|val| !val.is_empty());
    find_codex_home_from_env(codex_home_env.as_deref())
}

fn find_codex_home_from_env(codex_home_env: Option<&str>) -> std::io::Result<AbsolutePathBuf> {
    match codex_home_env {
        Some(val) => {
            let path = PathBuf::from(val);
            let metadata = std::fs::metadata(&path).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("CODEX_HOME points to {val:?}, but that path does not exist"),
                ),
                _ => std::io::Error::new(
                    err.kind(),
                    format!("failed to read CODEX_HOME {val:?}: {err}"),
                ),
            })?;

            if !metadata.is_dir() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("CODEX_HOME points to {val:?}, but that path is not a directory"),
                ))
            } else {
                let canonical = path.canonicalize().map_err(|err| {
                    std::io::Error::new(
                        err.kind(),
                        format!("failed to canonicalize CODEX_HOME {val:?}: {err}"),
                    )
                })?;
                AbsolutePathBuf::from_absolute_path(canonical)
            }
        }
        None => {
            let mut p = dirs::home_dir().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find home directory",
                )
            })?;
            p.push(".codex");
            AbsolutePathBuf::from_absolute_path(p)
        }
    }
}

fn sandbox_dir(codex_home: &Path) -> PathBuf {
    codex_home.join(".sandbox")
}

fn sandbox_bin_dir(codex_home: &Path) -> PathBuf {
    codex_home.join(".sandbox-bin")
}

fn helper_bin_dir(codex_home: &Path) -> PathBuf {
    sandbox_bin_dir(codex_home)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CopyOutcome {
    Reused,
    ReCopied,
}

pub(crate) fn resolve_exe_for_launch(source: &Path, codex_home: &Path) -> PathBuf {
    let Some(file_name) = source.file_name() else {
        return source.to_path_buf();
    };
    let destination = helper_bin_dir(codex_home).join(file_name);
    match copy_from_source_if_needed(source, &destination) {
        Ok(_) => destination,
        Err(err) => {
            let sandbox_log_dir = sandbox_dir(codex_home);
            tracing::warn!(
                helper_source = %source.display(),
                sandbox_log_dir = %sandbox_log_dir.display(),
                error = %err,
                "windows sandbox helper copy failed; falling back to legacy path"
            );
            source.to_path_buf()
        }
    }
}

fn copy_from_source_if_needed(source: &Path, destination: &Path) -> std::io::Result<CopyOutcome> {
    if destination_is_fresh(source, destination)? {
        return Ok(CopyOutcome::Reused);
    }

    let destination_dir = destination.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "helper destination has no parent: {}",
                destination.display()
            ),
        )
    })?;
    fs::create_dir_all(destination_dir)?;

    let temp_path = tempfile::NamedTempFile::new_in(destination_dir)?.into_temp_path();
    let temp_path_buf = temp_path.to_path_buf();

    let mut source_file = fs::File::open(source)?;
    let mut temp_file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&temp_path_buf)?;

    std::io::copy(&mut source_file, &mut temp_file)?;
    temp_file.flush()?;
    drop(temp_file);

    if destination.exists() {
        fs::remove_file(destination)?;
    }

    match fs::rename(&temp_path_buf, destination) {
        Ok(()) => Ok(CopyOutcome::ReCopied),
        Err(rename_err) => {
            if destination_is_fresh(source, destination)? {
                Ok(CopyOutcome::Reused)
            } else {
                Err(rename_err)
            }
        }
    }
}

fn destination_is_fresh(source: &Path, destination: &Path) -> std::io::Result<bool> {
    let source_meta = fs::metadata(source)?;
    let destination_meta = match fs::metadata(destination) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    if source_meta.len() != destination_meta.len() {
        return Ok(false);
    }

    let source_modified = source_meta.modified()?;
    let destination_modified = destination_meta.modified()?;

    Ok(destination_modified >= source_modified)
}

const CODEX_WINDOWS_SANDBOX_ARG1: &str = "--run-as-windows-sandbox";
const COMMAND_CWD_FLAG: &str = "--command-cwd";
const CODEX_HOME_FLAG: &str = "--codex-home";
const DENY_READ_PATHS_JSON_FLAG: &str = "--deny-read-paths-json";
const DENY_WRITE_PATHS_JSON_FLAG: &str = "--deny-write-paths-json";
const ENV_JSON_FLAG: &str = "--env-json";
const PERMISSION_PROFILE_FLAG: &str = "--permission-profile";
const PRIVATE_DESKTOP_FLAG: &str = "--windows-sandbox-private-desktop";
const PROXY_ENFORCED_FLAG: &str = "--proxy-enforced";
const READ_ROOTS_INCLUDE_PLATFORM_DEFAULTS_FLAG: &str = "--read-roots-include-platform-defaults";
const READ_ROOTS_JSON_FLAG: &str = "--read-roots-json";
const SANDBOX_LEVEL_FLAG: &str = "--windows-sandbox-level";
const WRITE_ROOTS_JSON_FLAG: &str = "--write-roots-json";
const WORKSPACE_ROOT_FLAG: &str = "--workspace-root";

#[allow(clippy::too_many_arguments)]
pub(crate) fn create_windows_sandbox_command_args_for_permission_profile(
    command: Vec<String>,
    command_cwd: &AbsolutePathBuf,
    workspace_roots: &[AbsolutePathBuf],
    env_map: &HashMap<String, String>,
    permission_profile: &PermissionProfile,
    windows_sandbox_level: WindowsSandboxLevel,
    windows_sandbox_private_desktop: bool,
    proxy_enforced: bool,
    read_roots_override: Option<&[PathBuf]>,
    read_roots_include_platform_defaults: bool,
    write_roots_override: Option<&[PathBuf]>,
    deny_read_paths_override: &[AbsolutePathBuf],
    deny_write_paths_override: &[AbsolutePathBuf],
    codex_home: &Path,
) -> Vec<String> {
    let permission_profile_json = serde_json::to_string(permission_profile)
        .unwrap_or_else(|err| panic!("failed to serialize permission profile: {err}"));
    let env_json = serde_json::to_string(env_map)
        .unwrap_or_else(|err| panic!("failed to serialize env: {err}"));
    let mut args = vec![
        CODEX_WINDOWS_SANDBOX_ARG1.to_string(),
        CODEX_HOME_FLAG.to_string(),
        codex_home.to_string_lossy().into_owned(),
        COMMAND_CWD_FLAG.to_string(),
        command_cwd.as_path().to_string_lossy().into_owned(),
        PERMISSION_PROFILE_FLAG.to_string(),
        permission_profile_json,
        ENV_JSON_FLAG.to_string(),
        env_json,
        SANDBOX_LEVEL_FLAG.to_string(),
        windows_sandbox_level.to_string(),
    ];
    let workspace_roots = if workspace_roots.is_empty() {
        std::slice::from_ref(command_cwd)
    } else {
        workspace_roots
    };
    for root in workspace_roots {
        args.push(WORKSPACE_ROOT_FLAG.to_string());
        args.push(root.as_path().to_string_lossy().into_owned());
    }
    if windows_sandbox_private_desktop {
        args.push(PRIVATE_DESKTOP_FLAG.to_string());
    }
    if proxy_enforced {
        args.push(PROXY_ENFORCED_FLAG.to_string());
    }
    if let Some(read_roots_override) = read_roots_override {
        push_json_arg(&mut args, READ_ROOTS_JSON_FLAG, &read_roots_override);
    }
    if read_roots_include_platform_defaults {
        args.push(READ_ROOTS_INCLUDE_PLATFORM_DEFAULTS_FLAG.to_string());
    }
    if let Some(write_roots_override) = write_roots_override {
        push_json_arg(&mut args, WRITE_ROOTS_JSON_FLAG, &write_roots_override);
    }
    if !deny_read_paths_override.is_empty() {
        push_json_arg(
            &mut args,
            DENY_READ_PATHS_JSON_FLAG,
            &deny_read_paths_override,
        );
    }
    if !deny_write_paths_override.is_empty() {
        push_json_arg(
            &mut args,
            DENY_WRITE_PATHS_JSON_FLAG,
            &deny_write_paths_override,
        );
    }
    args.push("--".to_string());
    args.extend(command);
    args
}

fn push_json_arg<T: serde::Serialize>(args: &mut Vec<String>, flag: &str, value: &T) {
    args.push(flag.to_string());
    args.push(
        serde_json::to_string(value)
            .unwrap_or_else(|err| panic!("failed to serialize {flag}: {err}")),
    );
}
