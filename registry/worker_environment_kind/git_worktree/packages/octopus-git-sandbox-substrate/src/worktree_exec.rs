use std::collections::VecDeque;
use std::io::Read;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use octopus_descriptors::ProcessSandboxMode;
use octopus_worker_env::{
    AbsPath, CapturedExecFailure, ExecOutput, ProjectedSandbox, WorkerEnvError,
};

#[cfg(target_os = "linux")]
use octopus_worker_env::NetworkCeiling;

#[cfg(target_os = "linux")]
fn locate_bwrap() -> Option<std::path::PathBuf> {
    crate::find_bwrap_for_sandbox()
}

#[cfg(target_os = "linux")]
fn build_bwrap_argv(sandbox: &ProjectedSandbox, cwd: &Path, argv: &[String]) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    a.push("--new-session".to_string());
    a.push("--die-with-parent".to_string());
    a.push("--ro-bind".to_string());
    a.push("/".to_string());
    a.push("/".to_string());
    a.push("--dev".to_string());
    a.push("/dev".to_string());
    a.push("--proc".to_string());
    a.push("/proc".to_string());
    for root in sandbox.writable_roots() {
        a.push("--bind".to_string());
        a.push(root.as_str().to_string());
        a.push(root.as_str().to_string());
    }
    for sub in sandbox.read_only_subpaths() {
        if Path::new(sub.as_str()).exists() {
            a.push("--ro-bind".to_string());
            a.push(sub.as_str().to_string());
            a.push(sub.as_str().to_string());
        }
    }
    a.push("--unshare-user".to_string());
    a.push("--unshare-pid".to_string());
    if sandbox.network() != NetworkCeiling::Full {
        a.push("--unshare-net".to_string());
    }
    a.push("--chdir".to_string());
    a.push(cwd.to_string_lossy().to_string());
    a.push("--".to_string());
    a.extend(argv.iter().cloned());
    a
}

pub fn sandboxed_worktree_process_argv(
    sandbox: &ProjectedSandbox,
    sandbox_mode: ProcessSandboxMode,
    cwd: &AbsPath,
    argv: Vec<String>,
) -> Result<Vec<String>, WorkerEnvError> {
    if argv.is_empty() {
        return Err(WorkerEnvError::ExecFailed("empty argv".into()));
    }
    sandbox.authorize_read(cwd)?;
    match sandbox_mode {
        ProcessSandboxMode::Disabled => Ok(argv),
        ProcessSandboxMode::Enforced => platform_sandboxed_worktree_process_argv(sandbox, cwd, argv),
    }
}

#[cfg(target_os = "linux")]
fn platform_sandboxed_worktree_process_argv(
    sandbox: &ProjectedSandbox,
    cwd: &AbsPath,
    argv: Vec<String>,
) -> Result<Vec<String>, WorkerEnvError> {
    let bwrap = locate_bwrap().ok_or_else(|| {
        WorkerEnvError::ExecFailed(
            "bubblewrap (bwrap) not found in PATH or packaged sandbox resources; refusing to run the build unsandboxed \
             (fail-closed)"
                .into(),
        )
    })?;
    let mut process_argv = vec![bwrap.to_string_lossy().into_owned()];
    process_argv.extend(build_bwrap_argv(sandbox, cwd.as_path(), &argv));
    Ok(process_argv)
}

#[cfg(target_os = "macos")]
fn platform_sandboxed_worktree_process_argv(
    sandbox: &ProjectedSandbox,
    cwd: &AbsPath,
    argv: Vec<String>,
) -> Result<Vec<String>, WorkerEnvError> {
    let profile = crate::permission_profile_for_projected_sandbox(sandbox, cwd.as_path()).map_err(
        |error| WorkerEnvError::ExecFailed(format!("sandbox floor translation failed: {error}")),
    )?;
    let (file_system_sandbox_policy, network_sandbox_policy) = profile.to_runtime_permissions();
    let seatbelt_argv = crate::seatbelt::create_seatbelt_command_args(
        crate::seatbelt::CreateSeatbeltCommandArgsParams {
            command: argv,
            file_system_sandbox_policy: &file_system_sandbox_policy,
            network_sandbox_policy,
            sandbox_policy_cwd: cwd.as_path(),
            enforce_managed_network: false,
            environment_id: None,
            network: None,
            extra_allow_unix_sockets: &[],
        },
    )
    .map_err(|error| {
        WorkerEnvError::ExecFailed(format!("seatbelt policy build failed: {error}"))
    })?;
    let mut process_argv = vec![crate::seatbelt::MACOS_PATH_TO_SEATBELT_EXECUTABLE.to_string()];
    process_argv.extend(seatbelt_argv);
    Ok(process_argv)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_sandboxed_worktree_process_argv(
    _sandbox: &ProjectedSandbox,
    _cwd: &AbsPath,
    _argv: Vec<String>,
) -> Result<Vec<String>, WorkerEnvError> {
    Err(WorkerEnvError::ExecFailed(
        "sandboxed worktree exec has no platform floor here; refusing to run unsandboxed \
         (fail-closed)"
            .into(),
    ))
}

#[cfg(target_os = "linux")]
pub fn run_sandboxed_in_worktree(
    sandbox: &ProjectedSandbox,
    sandbox_mode: ProcessSandboxMode,
    cwd: &AbsPath,
    argv: Vec<String>,
    timeout: Duration,
    max_output_bytes: usize,
    cancelled: &AtomicBool,
) -> Result<ExecOutput, WorkerEnvError> {
    let process_argv = sandboxed_worktree_process_argv(sandbox, sandbox_mode, cwd, argv)?;
    let (program, process_args) = process_argv
        .split_first()
        .ok_or_else(|| WorkerEnvError::ExecFailed("sandbox floor produced empty argv".into()))?;
    let mut command = Command::new(program);
    command
        .args(process_args)
        .current_dir(cwd.as_path())
        .env("TMPDIR", cwd.as_path())
        .env("TMP", cwd.as_path())
        .env("TEMP", cwd.as_path());
    let output = run_bounded_command(
        command,
        timeout,
        max_output_bytes,
        &format!("command under {program} (sandbox_mode={sandbox_mode:?})"),
        cancelled,
    )?;

    let stderr_text = String::from_utf8_lossy(&output.stderr);
    if sandbox_mode == ProcessSandboxMode::Enforced
        && output.exit_code == 1
        && is_bwrap_setup_failure(&stderr_text)
    {
        return Err(WorkerEnvError::ExecFailed(format!(
            "bubblewrap could not establish the sandbox (no unsandboxed fallback): {}",
            stderr_text.trim()
        )));
    }

    Ok(output)
}

#[cfg(target_os = "linux")]
fn is_bwrap_setup_failure(stderr: &str) -> bool {
    const MARKERS: &[&str] = &[
        "bwrap:",
        "No permissions to create a new namespace",
        "setting up uid map",
        "loopback: Failed",
        "Creating new namespace failed",
        "Can't mount",
        "Unexpected close of bwrap",
    ];
    MARKERS.iter().any(|m| stderr.contains(m))
}

#[cfg(target_os = "macos")]
pub fn run_sandboxed_in_worktree(
    sandbox: &ProjectedSandbox,
    sandbox_mode: ProcessSandboxMode,
    cwd: &AbsPath,
    argv: Vec<String>,
    timeout: Duration,
    max_output_bytes: usize,
    cancelled: &AtomicBool,
) -> Result<ExecOutput, WorkerEnvError> {
    let process_argv = sandboxed_worktree_process_argv(sandbox, sandbox_mode, cwd, argv)?;
    let (program, process_args) = process_argv
        .split_first()
        .ok_or_else(|| WorkerEnvError::ExecFailed("sandbox floor produced empty argv".into()))?;
    let mut command = Command::new(program);
    command
        .args(process_args)
        .current_dir(cwd.as_path())
        .env("TMPDIR", cwd.as_path())
        .env("TMP", cwd.as_path())
        .env("TEMP", cwd.as_path());
    run_bounded_command(
        command,
        timeout,
        max_output_bytes,
        &format!("command under {program} (sandbox_mode={sandbox_mode:?})"),
        cancelled,
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn run_sandboxed_in_worktree(
    _sandbox: &ProjectedSandbox,
    _sandbox_mode: ProcessSandboxMode,
    _cwd: &AbsPath,
    _argv: Vec<String>,
    _timeout: Duration,
    _max_output_bytes: usize,
    _cancelled: &AtomicBool,
) -> Result<ExecOutput, WorkerEnvError> {
    Err(WorkerEnvError::ExecFailed(
        "sandboxed worktree exec has no platform floor here; refusing to run unsandboxed \
         (fail-closed)"
            .into(),
    ))
}

const COMMAND_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(unix)]
fn run_bounded_command(
    mut command: Command,
    timeout: Duration,
    max_output_bytes: usize,
    action: &str,
    cancelled: &AtomicBool,
) -> Result<ExecOutput, WorkerEnvError> {
    if timeout.is_zero() || max_output_bytes == 0 {
        return Err(WorkerEnvError::ExecFailed(format!(
            "{action} requires positive timeout and output ceilings"
        )));
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(WorkerEnvError::ExecFailedWithOutput {
            reason: format!("{action} cancelled before spawn"),
            captured: Box::new(CapturedExecFailure {
                output: ExecOutput {
                    exit_code: -1,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    stdout_total_bytes: 0,
                    stderr_total_bytes: 0,
                    duration: Duration::ZERO,
                    timed_out: false,
                    stdout_truncated: false,
                    stderr_truncated: false,
                },
                exit_observed: false,
                stdout_complete: true,
                stderr_complete: true,
                cancelled: true,
                cleanup_errors: Vec::new(),
            }),
        });
    }
    configure_process_group(&mut command);
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            WorkerEnvError::ExecFailed(format!("{action} failed to spawn: {error}"))
        })?;
    let started = Instant::now();
    let mut owned = OwnedCommand {
        process_group: child.id(),
        child,
        cleaned: false,
    };
    let mut stdout = CommandCapture::new(owned.child.stdout.take(), max_output_bytes);
    let mut stderr = CommandCapture::new(owned.child.stderr.take(), max_output_bytes);
    let mut reason = None;
    let mut timed_out = false;
    let mut was_cancelled = false;
    let mut status = None;
    loop {
        let progressed = stdout.drain_ready() | stderr.drain_ready();
        if let Some(error) = stdout.error.as_ref() {
            reason = Some(format!("{action} stdout capture failed: {error}"));
            break;
        }
        if let Some(error) = stderr.error.as_ref() {
            reason = Some(format!("{action} stderr capture failed: {error}"));
            break;
        }
        if cancelled.load(Ordering::Acquire) {
            was_cancelled = true;
            reason = Some(format!(
                "{action} cancelled; effects already performed are not rolled back"
            ));
            break;
        }
        match owned.child.try_wait() {
            Ok(Some(observed)) => {
                status = Some(observed);
                break;
            }
            Ok(None) => {}
            Err(error) => {
                reason = Some(format!("{action} status inspection failed: {error}"));
                break;
            }
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            break;
        }
        if !progressed {
            if let Err(error) = wait_for_capture(&stdout, &stderr) {
                reason = Some(format!("{action} pipe readiness failed: {error}"));
                break;
            }
        }
    }
    let mut cleanup_errors = Vec::new();
    if let Err(error) = kill_command_group(owned.process_group) {
        cleanup_errors.push(format!("process-group termination failed: {error}"));
    }
    if status.is_none() {
        if let Err(error) = owned.child.kill() {
            cleanup_errors.push(format!("child termination failed: {error}"));
        }
    }
    let cleanup_started = Instant::now();
    let mut group_exited = false;
    let mut wait_error = None;
    let mut group_error = None;
    loop {
        let progressed = stdout.drain_ready() | stderr.drain_ready();
        if status.is_none() {
            match owned.child.try_wait() {
                Ok(observed) => status = observed,
                Err(error) => {
                    wait_error.get_or_insert_with(|| error.to_string());
                }
            }
        }
        #[cfg(unix)]
        match octopus_util_pty::process_group::process_group_has_exited(owned.process_group) {
            Ok(exited) => group_exited = exited,
            Err(error) => {
                group_error.get_or_insert_with(|| error.to_string());
            }
        }
        if status.is_some() && group_exited && stdout.finished() && stderr.finished() {
            break;
        }
        if cleanup_started.elapsed() >= COMMAND_CLEANUP_TIMEOUT {
            break;
        }
        if !progressed {
            if let Err(error) = wait_for_capture(&stdout, &stderr) {
                cleanup_errors.push(format!("pipe readiness failed during cleanup: {error}"));
                break;
            }
        }
    }
    if let Some(error) = wait_error {
        cleanup_errors.push(format!("child reap failed: {error}"));
    }
    if let Some(error) = group_error {
        cleanup_errors.push(format!("process-group inspection failed: {error}"));
    }
    if status.is_none() {
        cleanup_errors.push("child exit was not observed before cleanup deadline".to_string());
    }
    if !group_exited {
        cleanup_errors
            .push("process-group exit was not observed before cleanup deadline".to_string());
    }
    stdout.finish_capture();
    stderr.finish_capture();
    for (stream, error) in [("stdout", &stdout.error), ("stderr", &stderr.error)] {
        if let Some(error) = error {
            cleanup_errors.push(format!("{stream} capture incomplete: {error}"));
        }
    }
    owned.cleaned = status.is_some() && group_exited;
    let stdout_complete = stdout.eof;
    let stderr_complete = stderr.eof;
    let (stdout, stdout_total_bytes) = stdout.output.finish();
    let (stderr, stderr_total_bytes) = stderr.output.finish();
    let output = ExecOutput {
        exit_code: status.as_ref().and_then(ExitStatus::code).unwrap_or(-1),
        stdout_truncated: stdout_total_bytes > stdout.len(),
        stderr_truncated: stderr_total_bytes > stderr.len(),
        stdout,
        stderr,
        stdout_total_bytes,
        stderr_total_bytes,
        duration: started.elapsed(),
        timed_out,
    };
    if reason.is_some() || !cleanup_errors.is_empty() {
        let mut reason = reason.unwrap_or_else(|| format!("{action} cleanup failed"));
        if !cleanup_errors.is_empty() {
            reason.push_str("; ");
            reason.push_str(&cleanup_errors.join("; "));
        }
        return Err(WorkerEnvError::ExecFailedWithOutput {
            reason,
            captured: Box::new(CapturedExecFailure {
                output,
                exit_observed: status.is_some(),
                stdout_complete,
                stderr_complete,
                cancelled: was_cancelled,
                cleanup_errors,
            }),
        });
    }
    Ok(output)
}

struct OwnedCommand {
    child: Child,
    process_group: u32,
    cleaned: bool,
}

impl Drop for OwnedCommand {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = kill_command_group(self.process_group);
            let _ = self.child.kill();
            let _ = self.child.try_wait();
        }
    }
}

#[cfg(target_os = "macos")]
fn kill_command_group(process_group: u32) -> std::io::Result<()> {
    octopus_util_pty::process_group::kill_process_group_with_member_fallback(process_group)
}

#[cfg(not(target_os = "macos"))]
fn kill_command_group(process_group: u32) -> std::io::Result<()> {
    octopus_util_pty::process_group::kill_process_group(process_group)
}

struct CommandCapture<R> {
    reader: Option<R>,
    output: RetainedOutput,
    eof: bool,
    error: Option<String>,
}

#[cfg(unix)]
fn wait_for_capture(
    stdout: &CommandCapture<impl std::os::fd::AsRawFd>,
    stderr: &CommandCapture<impl std::os::fd::AsRawFd>,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut descriptors = [
        libc::pollfd {
            fd: stdout.reader.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: stderr.reader.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    if unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, 10) } == -1 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_capture_nonblocking(reader: &impl std::os::fd::AsRawFd) -> std::io::Result<()> {
    let fd = reader.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
impl<R: Read + std::os::fd::AsRawFd> CommandCapture<R> {
    fn new(reader: Option<R>, max_output_bytes: usize) -> Self {
        let error = match reader.as_ref() {
            Some(reader) => set_capture_nonblocking(reader)
                .err()
                .map(|error| error.to_string()),
            None => Some("capture pipe is unavailable".to_string()),
        };
        Self {
            reader: if error.is_none() { reader } else { None },
            output: RetainedOutput::new(max_output_bytes),
            eof: false,
            error,
        }
    }

    fn drain_ready(&mut self) -> bool {
        let Some(reader) = self.reader.as_mut() else {
            return false;
        };
        let mut progressed = false;
        let mut buffer = [0u8; 64 * 1024];
        for _ in 0..4 {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    self.eof = true;
                    self.reader.take();
                    break;
                }
                Ok(count) => {
                    self.output.push(&buffer[..count]);
                    progressed = true;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.reader.take();
                    break;
                }
            }
        }
        progressed
    }

    fn finished(&self) -> bool {
        self.reader.is_none()
    }

    fn finish_capture(&mut self) {
        self.reader.take();
        if !self.eof && self.error.is_none() {
            self.error = Some("EOF was not observed before cleanup deadline".to_string());
        }
    }
}

struct RetainedOutput {
    max_bytes: usize,
    head: Vec<u8>,
    tail: VecDeque<Vec<u8>>,
    tail_bytes: usize,
    total_bytes: usize,
}

impl RetainedOutput {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            head: Vec::with_capacity(max_bytes / 2),
            tail: VecDeque::new(),
            tail_bytes: 0,
            total_bytes: 0,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(chunk.len());
        let head_budget = self.max_bytes / 2;
        let head_take = head_budget.saturating_sub(self.head.len()).min(chunk.len());
        self.head.extend_from_slice(&chunk[..head_take]);
        self.push_tail(&chunk[head_take..]);
    }

    fn push_tail(&mut self, chunk: &[u8]) {
        let tail_budget = self.max_bytes.saturating_sub(self.max_bytes / 2);
        if chunk.is_empty() || tail_budget == 0 {
            return;
        }
        if chunk.len() >= tail_budget {
            self.tail.clear();
            self.tail
                .push_back(chunk[chunk.len().saturating_sub(tail_budget)..].to_vec());
            self.tail_bytes = tail_budget;
            return;
        }
        self.tail.push_back(chunk.to_vec());
        self.tail_bytes = self.tail_bytes.saturating_add(chunk.len());
        while self.tail_bytes > tail_budget {
            let excess = self.tail_bytes - tail_budget;
            let front_len = self.tail.front().map(Vec::len).unwrap_or(0);
            if front_len <= excess {
                self.tail.pop_front();
                self.tail_bytes = self.tail_bytes.saturating_sub(front_len);
            } else {
                self.tail
                    .front_mut()
                    .expect("tail front exists")
                    .drain(..excess);
                self.tail_bytes -= excess;
            }
        }
    }

    fn finish(self) -> (Vec<u8>, usize) {
        let mut retained = Vec::with_capacity(self.head.len().saturating_add(self.tail_bytes));
        retained.extend_from_slice(&self.head);
        for chunk in self.tail {
            retained.extend_from_slice(&chunk);
        }
        (retained, self.total_bytes)
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use crate::boundary::project_sandbox;
    use octopus_worker_env::{AbsPath, EnvContext, WorkerEnvCaps, WorkerEnvKindSpec};
    use std::collections::BTreeMap;

    fn git_worktree_spec() -> WorkerEnvKindSpec {
        serde_json::from_value(serde_json::json!({
            "worker_environment_kind_id": "wek_test_exec",
            "substrate_family": "process_exec",
            "door_driver_refs": ["system_coder_env"],
            "tool_process_bins": [
                "target/debug/octopus-coder-exec-tool",
                "target/debug/octopus-coder-fs-tool"
            ],
            "worker_consent": "explicit_ceiling",
            "supported_executor_kinds": ["exec"],
            "capabilities": ["filesystem", "process_exec", "git"],
            "boundary_defaults": {
                "read_root_refs": [{ "path": "repo_root", "access": ["read"] }],
                "write_root_refs": [{ "path": "worktree_root", "access": ["read", "write"] }],
                "protected_read_only_subpaths": [{ "path": "protected_branch_ref", "access": ["read"] }],
                "network_ceiling": "denied"
            },
            "lifecycle": { "requires_release": true }
        }))
        .expect("test wek spec parses")
    }

    fn ctx_for(repo: &std::path::Path, wt: &std::path::Path) -> EnvContext {
        EnvContext {
            root_slots: BTreeMap::from([
                ("repo_root".to_string(), AbsPath::new(repo).unwrap()),
                ("worktree_root".to_string(), AbsPath::new(wt).unwrap()),
                (
                    "protected_branch_ref".to_string(),
                    AbsPath::new(&wt.join(".git")).unwrap(),
                ),
            ]),
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn empty_argv_is_fail_closed() {
        let tmp = std::env::temp_dir().join(format!("octo_floor_exec_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let ctx = ctx_for(&tmp, &tmp);
        let caps = WorkerEnvCaps::from_kind_boundary(&git_worktree_spec());
        let projected = project_sandbox(&git_worktree_spec(), &caps, &ctx).unwrap();
        let wt = AbsPath::new(&tmp).unwrap();
        let err = run_sandboxed_in_worktree(
            &projected,
            ProcessSandboxMode::Enforced,
            &wt,
            vec![],
            Duration::from_secs(1),
            1024,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(matches!(err, WorkerEnvError::ExecFailed(_)));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn bounded_runner_truncates_output_and_terminates_the_process_group_on_deadline() {
        let mut command = Command::new("/bin/sh");
        command.args(["-lc", "printf 123456; sleep 2"]);
        let output = run_bounded_command(
            command,
            Duration::from_millis(500),
            4,
            "bounded runner probe",
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(output.stdout, b"1256");
        assert_eq!(output.stdout_total_bytes, 6);
        assert!(output.stdout_truncated);
        assert!(output.timed_out);
        assert!(output.duration < Duration::from_secs(2));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_floor_runs_cwd_relative_file_probe_in_worktree() {
        let root = std::env::temp_dir().join(format!(
            "octo_floor_macos_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = root.join("repo");
        let wt = root.join("wt");
        std::fs::create_dir_all(wt.join("registry/generated")).unwrap();
        std::fs::create_dir_all(repo.join("registry")).unwrap();
        std::fs::write(wt.join("registry/generated/task.json"), b"{}\n").unwrap();

        let ctx = ctx_for(&repo, &wt);
        let caps = WorkerEnvCaps::from_kind_boundary(&git_worktree_spec());
        let projected = project_sandbox(&git_worktree_spec(), &caps, &ctx)
            .expect("worker-env sandbox projects");

        let out = run_sandboxed_in_worktree(
            &projected,
            ProcessSandboxMode::Enforced,
            &AbsPath::new(&wt).unwrap(),
            vec![
                "/bin/test".to_string(),
                "-f".to_string(),
                "registry/generated/task.json".to_string(),
            ],
            Duration::from_secs(5),
            64 * 1024,
            &AtomicBool::new(false),
        )
        .unwrap_or_else(|e| panic!("macOS worktree floor failed closed: {e}"));
        assert_eq!(
            out.exit_code,
            0,
            "cwd-relative file probe must succeed in the leased worktree; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );

        let outside = root.join("outside");
        let target = outside.to_string_lossy().into_owned();
        for mode in [ProcessSandboxMode::Enforced, ProcessSandboxMode::Disabled] {
            let out = run_sandboxed_in_worktree(&projected, mode, &AbsPath::new(&wt).unwrap(),
                vec!["/bin/mkdir".into(), target.clone()], Duration::from_secs(5),
                1024, &AtomicBool::new(false)).unwrap();
            assert_eq!(out.exit_code == 0, mode == ProcessSandboxMode::Disabled);
        }
        assert!(sandboxed_worktree_process_argv(&projected, ProcessSandboxMode::Disabled,
            &AbsPath::new(&root).unwrap(), vec!["/usr/bin/true".into()]).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
