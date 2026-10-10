use std::ffi::OsString;
use std::fs::File;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;

use octopus_applypatch::CODEX_CORE_APPLY_PATCH_ARG1;
use octopus_exec::CODEX_FS_HELPER_ARG1;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::TempDir;

use home_dir::find_codex_home;
use install_context::InstallContext;

const APPLY_PATCH_ARG0: &str = "apply_patch";
const MISSPELLED_APPLY_PATCH_ARG0: &str = "applypatch";
const CODEX_LINUX_SANDBOX_ARG0: &str = "codex-linux-sandbox";
#[cfg(unix)]
const EXECVE_WRAPPER_ARG0: &str = "codex-execve-wrapper";
const LOCK_FILENAME: &str = ".lock";
const TOKIO_WORKER_STACK_SIZE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Arg0DispatchPaths {
    pub codex_self_exe: Option<PathBuf>,
    pub codex_linux_sandbox_exe: Option<PathBuf>,
    pub main_execve_wrapper_exe: Option<PathBuf>,
}

pub struct Arg0PathEntryGuard {
    _temp_dir: TempDir,
    _lock_file: File,
    paths: Arg0DispatchPaths,
}

impl Arg0PathEntryGuard {
    fn new(temp_dir: TempDir, lock_file: File, paths: Arg0DispatchPaths) -> Self {
        Self {
            _temp_dir: temp_dir,
            _lock_file: lock_file,
            paths,
        }
    }

    pub fn paths(&self) -> &Arg0DispatchPaths {
        &self.paths
    }
}

pub fn arg0_dispatch() -> Option<Arg0PathEntryGuard> {
    let mut args = std::env::args_os();
    let argv0 = args.next().unwrap_or_default();
    let exe_name = Path::new(&argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    #[cfg(unix)]
    if exe_name == EXECVE_WRAPPER_ARG0 {
        match octopus_shell_escalation::main_execve_wrapper() {
            Ok(()) => std::process::exit(0),
            Err(err) => {
                eprintln!("Error: {EXECVE_WRAPPER_ARG0} failed: {err:#}");
                std::process::exit(1);
            }
        }
    }

    if exe_name == CODEX_LINUX_SANDBOX_ARG0 {
        eprintln!(
            "Error: {CODEX_LINUX_SANDBOX_ARG0} dispatch is not yet ported (octopus-sandbox lacks run_main)."
        );
        std::process::exit(1);
    } else if exe_name == APPLY_PATCH_ARG0 || exe_name == MISSPELLED_APPLY_PATCH_ARG0 {
        octopus_applypatch::main();
    }

    let argv1 = args.next().unwrap_or_default();
    if argv1 == CODEX_FS_HELPER_ARG1 {
        octopus_exec::run_fs_helper_main();
    }
    if argv1 == CODEX_CORE_APPLY_PATCH_ARG1 {
        let patch_arg = args.next().and_then(|s| s.to_str().map(str::to_owned));
        let exit_code = match patch_arg {
            Some(patch_arg) => {
                let mut stdout = std::io::stdout();
                let mut stderr = std::io::stderr();
                let cwd = match octopus_util_abspath::AbsolutePathBuf::current_dir() {
                    Ok(cwd) => cwd,
                    Err(_) => std::process::exit(1),
                };
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => std::process::exit(1),
                };
                let cwd = cwd.into();
                match runtime.block_on(octopus_applypatch::apply_patch(
                    &patch_arg,
                    &cwd,
                    &mut stdout,
                    &mut stderr,
                    octopus_file_system::LOCAL_FS.as_ref(),
                    None,
                )) {
                    Ok(_) => 0,
                    Err(_) => 1,
                }
            }
            None => {
                eprintln!("Error: {CODEX_CORE_APPLY_PATCH_ARG1} requires a UTF-8 PATCH argument.");
                1
            }
        };
        std::process::exit(exit_code);
    }

    load_dotenv();

    let (path_entry_guard, updated_path_env_var) = prepare_path_env_var_with_aliases(
        InstallContext::current(),
        std::env::var_os("PATH"),
        prepare_path_entry_for_codex_aliases,
    );
    if let Some(updated_path_env_var) = updated_path_env_var {
        unsafe {
            std::env::set_var("PATH", updated_path_env_var);
        }
    }
    path_entry_guard
}

fn prepare_path_env_var_with_aliases(
    install_context: &InstallContext,
    existing_path: Option<OsString>,
    prepare_aliases: impl FnOnce(Option<OsString>) -> std::io::Result<(Arg0PathEntryGuard, OsString)>,
) -> (Option<Arg0PathEntryGuard>, Option<OsString>) {
    let package_path = path_env_with_package_path_dir(install_context, existing_path.clone());
    let path_for_aliases = package_path.clone().or(existing_path);

    match prepare_aliases(path_for_aliases) {
        Ok((path_entry, updated_path_env_var)) => (Some(path_entry), Some(updated_path_env_var)),
        Err(err) => {
            eprintln!("WARNING: proceeding, even though we could not create PATH aliases: {err}");
            (None, package_path)
        }
    }
}

pub fn arg0_dispatch_or_else<F, Fut>(main_fn: F) -> anyhow::Result<()>
where
    F: FnOnce(Arg0DispatchPaths) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let path_entry_guard = arg0_dispatch();
    let current_exe = std::env::current_exe().ok();

    let handle = std::thread::Builder::new()
        .name("codex-main".to_string())
        .stack_size(TOKIO_WORKER_STACK_SIZE_BYTES)
        .spawn(move || {
            let runtime = build_runtime()?;
            runtime.block_on(run_main_with_arg0_guard(
                path_entry_guard,
                current_exe,
                main_fn,
            ))
        })?;
    match handle.join() {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

async fn run_main_with_arg0_guard<F, Fut>(
    path_entry_guard: Option<Arg0PathEntryGuard>,
    current_exe: Option<PathBuf>,
    main_fn: F,
) -> anyhow::Result<()>
where
    F: FnOnce(Arg0DispatchPaths) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let paths = Arg0DispatchPaths {
        codex_self_exe: current_exe.clone(),
        codex_linux_sandbox_exe: if cfg!(target_os = "linux") {
            linux_sandbox_exe_path(path_entry_guard.as_ref(), current_exe)
        } else {
            None
        },
        main_execve_wrapper_exe: path_entry_guard
            .as_ref()
            .and_then(|path_entry| path_entry.paths().main_execve_wrapper_exe.clone()),
    };

    let result = main_fn(paths).await;
    drop(path_entry_guard);
    result
}

fn linux_sandbox_exe_path(
    path_entry_guard: Option<&Arg0PathEntryGuard>,
    current_exe: Option<PathBuf>,
) -> Option<PathBuf> {
    path_entry_guard
        .and_then(|path_entry| path_entry.paths().codex_linux_sandbox_exe.clone())
        .or(current_exe)
}

fn build_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    builder.thread_stack_size(TOKIO_WORKER_STACK_SIZE_BYTES);
    Ok(builder.build()?)
}

const ILLEGAL_ENV_VAR_PREFIX: &str = "CODEX_";

fn load_dotenv() {
    if let Ok(codex_home) = find_codex_home()
        && let Ok(iter) = dotenvy::from_path_iter(codex_home.join(".env"))
    {
        set_filtered(iter);
    }
}

fn set_filtered<I>(iter: I)
where
    I: IntoIterator<Item = Result<(String, String), dotenvy::Error>>,
{
    for (key, value) in iter.into_iter().flatten() {
        if !key.to_ascii_uppercase().starts_with(ILLEGAL_ENV_VAR_PREFIX) {
            unsafe { std::env::set_var(&key, &value) };
        }
    }
}

fn prepare_path_entry_for_codex_aliases(
    existing_path: Option<OsString>,
) -> std::io::Result<(Arg0PathEntryGuard, OsString)> {
    let codex_home = find_codex_home()?;
    #[cfg(not(debug_assertions))]
    {
        let temp_root = std::env::temp_dir();
        if codex_home.starts_with(&temp_root) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Refusing to create helper binaries under temporary dir {temp_root:?} (codex_home: {codex_home:?})"
                ),
            ));
        }
    }

    std::fs::create_dir_all(&codex_home)?;
    let temp_root = codex_home.join("tmp").join("arg0");
    std::fs::create_dir_all(&temp_root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&temp_root, std::fs::Permissions::from_mode(0o700))?;
    }

    if let Err(err) = janitor_cleanup(&temp_root) {
        eprintln!("WARNING: failed to clean up stale arg0 temp dirs: {err}");
    }

    let temp_dir = tempfile::Builder::new()
        .prefix("codex-arg0")
        .tempdir_in(&temp_root)?;
    let path = temp_dir.path();

    let lock_path = path.join(LOCK_FILENAME);
    let lock_file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    lock_file.try_lock()?;

    for filename in &[
        APPLY_PATCH_ARG0,
        MISSPELLED_APPLY_PATCH_ARG0,
        #[cfg(target_os = "linux")]
        CODEX_LINUX_SANDBOX_ARG0,
        #[cfg(unix)]
        EXECVE_WRAPPER_ARG0,
    ] {
        let exe = std::env::current_exe()?;

        #[cfg(unix)]
        {
            let link = path.join(filename);
            symlink(&exe, &link)?;
        }

        #[cfg(windows)]
        {
            let batch_script = path.join(format!("{filename}.bat"));
            let exe = exe.display();
            std::fs::write(
                &batch_script,
                format!(
                    r#"@echo off
"{exe}" {CODEX_CORE_APPLY_PATCH_ARG1} %*
"#,
                ),
            )?;
        }
    }

    let updated_path_env_var = path_env_with_entry(path, existing_path);

    let paths = Arg0DispatchPaths {
        codex_self_exe: std::env::current_exe().ok(),
        codex_linux_sandbox_exe: {
            #[cfg(target_os = "linux")]
            {
                Some(path.join(CODEX_LINUX_SANDBOX_ARG0))
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        },
        main_execve_wrapper_exe: {
            #[cfg(unix)]
            {
                Some(path.join(EXECVE_WRAPPER_ARG0))
            }
            #[cfg(not(unix))]
            {
                None
            }
        },
    };

    Ok((
        Arg0PathEntryGuard::new(temp_dir, lock_file, paths),
        updated_path_env_var,
    ))
}

fn path_env_with_package_path_dir(
    install_context: &InstallContext,
    existing_path: Option<OsString>,
) -> Option<OsString> {
    let path_dir = install_context
        .package_layout
        .as_ref()
        .and_then(|package_layout| package_layout.path_dir.as_ref())?;
    Some(path_env_with_entry(path_dir.as_path(), existing_path))
}

fn path_env_with_entry(path_entry: &Path, existing_path: Option<OsString>) -> OsString {
    #[cfg(unix)]
    const PATH_SEPARATOR: &str = ":";

    #[cfg(windows)]
    const PATH_SEPARATOR: &str = ";";

    let capacity = path_entry.as_os_str().len()
        + existing_path
            .as_ref()
            .map_or(0, |existing_path| 1 + existing_path.len());
    let mut path_env_var = OsString::with_capacity(capacity);
    path_env_var.push(path_entry);
    if let Some(existing_path) = existing_path {
        path_env_var.push(PATH_SEPARATOR);
        path_env_var.push(existing_path);
    }
    path_env_var
}

fn janitor_cleanup(temp_root: &Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(temp_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(_lock_file) = try_lock_dir(&path)? else {
            continue;
        };

        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

fn try_lock_dir(dir: &Path) -> std::io::Result<Option<File>> {
    let lock_path = dir.join(LOCK_FILENAME);
    let lock_file = match File::options().read(true).write(true).open(&lock_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };

    match lock_file.try_lock() {
        Ok(()) => Ok(Some(lock_file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

mod home_dir {
    use std::path::PathBuf;

    use dirs::home_dir;
    use octopus_util_abspath::AbsolutePathBuf;

    pub fn find_codex_home() -> std::io::Result<AbsolutePathBuf> {
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
                let mut p = home_dir().ok_or_else(|| {
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
}

mod install_context {
    use std::sync::OnceLock;

    use octopus_util_abspath::AbsolutePathBuf;

    static INSTALL_CONTEXT: OnceLock<InstallContext> = OnceLock::new();

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct CodexPackageLayout {
        pub package_dir: AbsolutePathBuf,
        pub bin_dir: AbsolutePathBuf,
        pub resources_dir: Option<AbsolutePathBuf>,
        pub path_dir: Option<AbsolutePathBuf>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct InstallContext {
        pub method: InstallMethod,
        pub package_layout: Option<CodexPackageLayout>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum InstallMethod {
        Other,
    }

    impl InstallContext {
        pub fn current() -> &'static Self {
            INSTALL_CONTEXT.get_or_init(|| Self {
                method: InstallMethod::Other,
                package_layout: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Arg0DispatchPaths;
    use super::Arg0PathEntryGuard;
    use super::LOCK_FILENAME;
    use super::install_context::CodexPackageLayout;
    use super::install_context::InstallContext;
    use super::install_context::InstallMethod;
    use super::janitor_cleanup;
    use super::linux_sandbox_exe_path;
    #[cfg(unix)]
    use super::run_main_with_arg0_guard;
    #[cfg(unix)]
    use anyhow::ensure;
    use octopus_util_abspath::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::fs::File;
    use std::path::Path;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct PackagePathTestFixture {
        _temp_dir: TempDir,
        arg0_dir: PathBuf,
        existing_dir: PathBuf,
        install_context: InstallContext,
        path_dir: AbsolutePathBuf,
    }

    fn create_lock(dir: &Path) -> std::io::Result<File> {
        let lock_path = dir.join(LOCK_FILENAME);
        File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
    }

    fn package_path_test_fixture() -> anyhow::Result<PackagePathTestFixture> {
        let temp_dir = TempDir::new()?;
        let arg0_dir = temp_dir.path().join("arg0");
        let package_dir = temp_dir.path().join("package");
        let bin_dir = package_dir.join("bin");
        let path_dir = package_dir.join("codex-path");
        let existing_dir = temp_dir.path().join("existing-bin");
        fs::create_dir_all(&arg0_dir)?;
        fs::create_dir_all(&bin_dir)?;
        fs::create_dir_all(&path_dir)?;
        fs::create_dir_all(&existing_dir)?;
        let path_dir = AbsolutePathBuf::from_absolute_path(path_dir.canonicalize()?)?;
        let install_context = InstallContext {
            method: InstallMethod::Other,
            package_layout: Some(CodexPackageLayout {
                package_dir: AbsolutePathBuf::from_absolute_path(package_dir.canonicalize()?)?,
                bin_dir: AbsolutePathBuf::from_absolute_path(bin_dir.canonicalize()?)?,
                resources_dir: None,
                path_dir: Some(path_dir.clone()),
            }),
        };

        Ok(PackagePathTestFixture {
            _temp_dir: temp_dir,
            arg0_dir,
            existing_dir,
            install_context,
            path_dir,
        })
    }

    #[test]
    fn linux_sandbox_exe_path_prefers_codex_linux_sandbox_alias() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let lock_file = create_lock(temp_dir.path())?;
        let alias_path = temp_dir.path().join("codex-linux-sandbox");
        let path_entry = Arg0PathEntryGuard::new(
            temp_dir,
            lock_file,
            Arg0DispatchPaths {
                codex_self_exe: Some(PathBuf::from("/usr/bin/codex")),
                codex_linux_sandbox_exe: Some(alias_path.clone()),
                main_execve_wrapper_exe: None,
            },
        );

        assert_eq!(
            linux_sandbox_exe_path(Some(&path_entry), Some(PathBuf::from("/usr/bin/codex"))),
            Some(alias_path),
        );
        Ok(())
    }

    #[test]
    fn path_env_can_prepend_package_path_before_arg0_alias_dir() -> anyhow::Result<()> {
        let fixture = package_path_test_fixture()?;

        let package_path = super::path_env_with_package_path_dir(
            &fixture.install_context,
            Some(fixture.existing_dir.as_os_str().to_owned()),
        )
        .expect("package path dir should update PATH");
        let updated_path = super::path_env_with_entry(&fixture.arg0_dir, Some(package_path));

        assert_eq!(
            std::env::split_paths(&updated_path).collect::<Vec<_>>(),
            vec![
                fixture.arg0_dir,
                fixture.path_dir.as_path().to_path_buf(),
                fixture.existing_dir
            ],
        );
        Ok(())
    }

    #[test]
    fn package_path_survives_arg0_alias_setup_failure() -> anyhow::Result<()> {
        let fixture = package_path_test_fixture()?;

        let (path_entry_guard, updated_path_env_var) = super::prepare_path_env_var_with_aliases(
            &fixture.install_context,
            Some(fixture.existing_dir.as_os_str().to_owned()),
            |path_for_aliases| {
                assert_eq!(
                    std::env::split_paths(
                        &path_for_aliases.expect("package PATH should be passed to alias setup")
                    )
                    .collect::<Vec<_>>(),
                    vec![
                        fixture.path_dir.as_path().to_path_buf(),
                        fixture.existing_dir.clone()
                    ],
                );
                Err(std::io::Error::other("alias setup failed"))
            },
        );

        assert!(path_entry_guard.is_none());
        let updated_path_env_var =
            updated_path_env_var.expect("package PATH should survive alias setup failure");
        assert_eq!(
            std::env::split_paths(&updated_path_env_var).collect::<Vec<_>>(),
            vec![
                fixture.path_dir.as_path().to_path_buf(),
                fixture.existing_dir
            ],
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_main_with_arg0_guard_keeps_aliases_alive_until_main_returns() -> anyhow::Result<()> {
        let temp_dir = TempDir::new()?;
        let alias_path = temp_dir.path().join("codex-helper-alias");
        fs::write(&alias_path, b"")?;
        let lock_file = create_lock(temp_dir.path())?;
        let path_entry = Arg0PathEntryGuard::new(
            temp_dir,
            lock_file,
            Arg0DispatchPaths {
                codex_self_exe: Some(PathBuf::from("/usr/bin/codex")),
                codex_linux_sandbox_exe: Some(alias_path.clone()),
                main_execve_wrapper_exe: Some(alias_path),
            },
        );

        super::build_runtime()?.block_on(run_main_with_arg0_guard(
            Some(path_entry),
            Some(PathBuf::from("/usr/bin/codex")),
            |paths| async move {
                let alias_path = paths
                    .codex_linux_sandbox_exe
                    .or(paths.main_execve_wrapper_exe)
                    .expect("unix dispatch should create at least one alias path");
                ensure!(
                    alias_path.exists(),
                    "alias path disappeared before main future was polled: {}",
                    alias_path.display()
                );

                tokio::task::yield_now().await;

                ensure!(
                    alias_path.exists(),
                    "alias path disappeared while main future was running: {}",
                    alias_path.display()
                );
                Ok(())
            },
        ))
    }

    #[test]
    fn janitor_skips_dirs_without_lock_file() -> std::io::Result<()> {
        let root = tempfile::tempdir()?;
        let dir = root.path().join("no-lock");
        fs::create_dir(&dir)?;

        janitor_cleanup(root.path())?;

        assert!(dir.exists());
        Ok(())
    }

    #[test]
    fn janitor_skips_dirs_with_held_lock() -> std::io::Result<()> {
        let root = tempfile::tempdir()?;
        let dir = root.path().join("locked");
        fs::create_dir(&dir)?;
        let lock_file = create_lock(&dir)?;
        lock_file.try_lock()?;

        janitor_cleanup(root.path())?;

        assert!(dir.exists());
        Ok(())
    }

    #[test]
    fn janitor_removes_dirs_with_unlocked_lock() -> std::io::Result<()> {
        let root = tempfile::tempdir()?;
        let dir = root.path().join("stale");
        fs::create_dir(&dir)?;
        create_lock(&dir)?;

        janitor_cleanup(root.path())?;

        assert!(!dir.exists());
        Ok(())
    }
}
