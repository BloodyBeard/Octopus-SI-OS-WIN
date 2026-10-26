use std::collections::VecDeque;
use std::io::Read;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(all(unix, not(target_os = "macos")))]
use std::process::Child;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use octopus_descriptors::ProcessSandboxMode;
use octopus_worker_env::CapturedExecFailure;
use octopus_worker_env::{AbsPath, ExecOutput, ProjectedSandbox, WorkerEnvError};

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

/// No sandbox backend here: run unsandboxed only when the person running
/// Octopus opted in (`OCTOPUS_ALLOW_UNSANDBOXED_PROCESSES=1`).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_sandboxed_worktree_process_argv(
    _sandbox: &ProjectedSandbox,
    _cwd: &AbsPath,
    argv: Vec<String>,
) -> Result<Vec<String>, WorkerEnvError> {
    if octopus_descriptors::unsandboxed_processes_allowed() {
        Ok(argv)
    } else {
        Err(WorkerEnvError::ExecFailed(octopus_descriptors::no_sandbox_backend_refusal(
            "sandboxed worktree exec",
        )))
    }
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

/// Windows has no worktree sandbox backend, so this runs only in
/// `Disabled` mode or with the no-sandbox opt-in (see
/// `platform_sandboxed_worktree_process_argv`).
#[cfg(windows)]
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
        .ok_or_else(|| WorkerEnvError::ExecFailed("worktree exec produced empty argv".into()))?;
    let mut command = Command::new(program);
    command
        .args(process_args)
        .current_dir(cwd.as_path())
        .env("TMPDIR", cwd.as_path())
        .env("TMP", cwd.as_path())
        .env("TEMP", cwd.as_path());
    run_bounded_command_windows(
        command,
        timeout,
        max_output_bytes,
        &format!("command under {program} (sandbox_mode={sandbox_mode:?}, no OS sandbox)"),
        cancelled,
    )
}

#[cfg(not(any(unix, windows)))]
pub fn run_sandboxed_in_worktree(
    _sandbox: &ProjectedSandbox,
    _sandbox_mode: ProcessSandboxMode,
    _cwd: &AbsPath,
    _argv: Vec<String>,
    _timeout: Duration,
    _max_output_bytes: usize,
    _cancelled: &AtomicBool,
) -> Result<ExecOutput, WorkerEnvError> {
    Err(WorkerEnvError::ExecFailed(octopus_descriptors::no_sandbox_backend_refusal(
        "sandboxed worktree exec",
    )))
}

fn cancelled_before_spawn(action: &str) -> WorkerEnvError {
    WorkerEnvError::ExecFailedWithOutput {
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
    }
}

/// The Windows bounded runner: the same contract as the Unix one (timeout,
/// output ceiling, cancellation, the whole process tree killed afterwards),
/// with a job object in place of the process group and reader threads in
/// place of `poll`.
#[cfg(windows)]
fn run_bounded_command_windows(
    mut command: Command,
    timeout: Duration,
    max_output_bytes: usize,
    action: &str,
    cancelled: &AtomicBool,
) -> Result<ExecOutput, WorkerEnvError> {
    use std::os::windows::io::AsRawHandle;
    use std::sync::mpsc;

    if timeout.is_zero() || max_output_bytes == 0 {
        return Err(WorkerEnvError::ExecFailed(format!(
            "{action} requires positive timeout and output ceilings"
        )));
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(cancelled_before_spawn(action));
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| WorkerEnvError::ExecFailed(format!("{action} failed to spawn: {error}")))?;
    let job = match octopus_process_job::Job::containing(child.as_raw_handle()) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WorkerEnvError::ExecFailed(format!(
                "{action} could not be placed in a process job: {error}"
            )));
        }
    };
    let started = Instant::now();
    // Each pipe is read on its own thread; a pipe ends once every process in
    // the tree that inherited it has exited.
    let capture = |reader: Option<Box<dyn Read + Send>>| {
        let (sender, receiver) = mpsc::channel();
        if let Some(mut reader) = reader {
            std::thread::spawn(move || {
                let mut output = RetainedOutput::new(max_output_bytes);
                let mut buffer = [0u8; 8192];
                let error = loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break None,
                        Ok(count) => output.push(&buffer[..count]),
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(error) => break Some(error.to_string()),
                    }
                };
                let _ = sender.send((output, error));
            });
        } else {
            let _ = sender.send((RetainedOutput::new(max_output_bytes), Some("pipe was not opened".into())));
        }
        receiver
    };
    let stdout = capture(child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let stderr = capture(child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let mut reason = None;
    let mut timed_out = false;
    let mut was_cancelled = false;
    let mut status = None;
    loop {
        if cancelled.load(Ordering::Acquire) {
            was_cancelled = true;
            reason = Some(format!("{action} cancelled; effects already performed are not rolled back"));
            break;
        }
        match child.try_wait() {
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
        std::thread::sleep(Duration::from_millis(10));
    }
    // Like the Unix process group, the job is ended even after a normal exit
    // so no descendant keeps running.
    let mut cleanup_errors = Vec::new();
    if let Err(error) = job.terminate() {
        cleanup_errors.push(format!("process-tree termination failed: {error}"));
    }
    let cleanup_deadline = Instant::now() + COMMAND_CLEANUP_TIMEOUT;
    while status.is_none() && Instant::now() < cleanup_deadline {
        match child.try_wait() {
            Ok(observed) => status = observed,
            Err(error) => {
                cleanup_errors.push(format!("child reap failed: {error}"));
                break;
            }
        }
        if status.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if status.is_none() {
        cleanup_errors.push("child exit was not observed before cleanup deadline".to_string());
    }
    let mut collect = |stream: &str, receiver: mpsc::Receiver<(RetainedOutput, Option<String>)>| {
        let remaining = cleanup_deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok((output, error)) => {
                if let Some(error) = &error {
                    cleanup_errors.push(format!("{stream} capture incomplete: {error}"));
                }
                (output.finish(), error.is_none())
            }
            Err(_) => {
                cleanup_errors.push(format!("{stream} did not close before cleanup deadline"));
                ((Vec::new(), 0), false)
            }
        }
    };
    let ((stdout, stdout_total_bytes), stdout_complete) = collect("stdout", stdout);
    let ((stderr, stderr_total_bytes), stderr_complete) = collect("stderr", stderr);
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

// The Unix bounded runner below serves the Linux (bubblewrap) and macOS
// (Seatbelt) backends; Windows uses `run_bounded_command_windows` above.
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
        return Err(cancelled_before_spawn(action));
    }
    configure_process_group(&mut command);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            WorkerEnvError::ExecFailed(format!("{action} failed to spawn: {error}"))
        })?;
    let started = Instant::now();
    let mut stdout = CommandCapture::new(child.stdout.take(), max_output_bytes);
    let mut stderr = CommandCapture::new(child.stderr.take(), max_output_bytes);
    let mut owned = OwnedCommand {
        #[cfg(target_os = "macos")]
        group: octopus_util_pty::owned_process_group::OwnedProcessGroup::from_child(child),
        #[cfg(not(target_os = "macos"))]
        process_group: child.id(),
        #[cfg(not(target_os = "macos"))]
        child,
        cleaned: false,
    };
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
        match owned.observe_exit() {
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
    if let Err(error) = owned.kill_group() {
        cleanup_errors.push(format!("process-group termination failed: {error}"));
    }
    if status.is_none() {
        if let Err(error) = owned.kill_root() {
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
            match owned.observe_exit() {
                Ok(observed) => status = observed,
                Err(error) => {
                    wait_error.get_or_insert_with(|| error.to_string());
                }
            }
        }
        #[cfg(unix)]
        match owned.group_has_exited() {
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

#[cfg(unix)]
struct OwnedCommand {
    #[cfg(target_os = "macos")]
    group: octopus_util_pty::owned_process_group::OwnedProcessGroup,
    #[cfg(not(target_os = "macos"))]
    child: Child,
    #[cfg(not(target_os = "macos"))]
    process_group: u32,
    cleaned: bool,
}

#[cfg(unix)]
impl OwnedCommand {
    fn observe_exit(&mut self) -> std::io::Result<Option<ExitStatus>> {
        #[cfg(target_os = "macos")]
        { self.group.root_exit_status() }
        #[cfg(not(target_os = "macos"))]
        { self.child.try_wait() }
    }

    fn kill_group(&self) -> std::io::Result<()> {
        #[cfg(target_os = "macos")]
        { self.group.signal(libc::SIGKILL).map(|_| ()) }
        #[cfg(not(target_os = "macos"))]
        { kill_command_group(self.process_group) }
    }

    fn kill_root(&mut self) -> std::io::Result<()> {
        #[cfg(target_os = "macos")]
        { self.group.kill_root() }
        #[cfg(not(target_os = "macos"))]
        { self.child.kill() }
    }

    #[cfg(unix)]
    fn group_has_exited(&self) -> std::io::Result<bool> {
        #[cfg(target_os = "macos")]
        { self.group.reap_if_finished() }
        #[cfg(not(target_os = "macos"))]
        { octopus_util_pty::process_group::process_group_has_exited(self.process_group) }
    }
}

#[cfg(unix)]
impl Drop for OwnedCommand {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.kill_group();
            let _ = self.kill_root();
            let _ = self.observe_exit();
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn kill_command_group(process_group: u32) -> std::io::Result<()> {
    octopus_util_pty::process_group::kill_process_group(process_group)
}

#[cfg(unix)]
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
            additional_root_slots: Default::default(),
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
    fn bounded_runner_reaps_root_only_after_its_remaining_group_closes() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "trap '' HUP; /bin/sleep 30 & printf '%s %s\\n' \"$$\" \"$!\"; exit 7"]);
        let output = run_bounded_command(
            command, Duration::from_secs(5), 1024, "root exit cleanup", &AtomicBool::new(false),
        ).unwrap();
        assert_eq!(output.exit_code, 7);
        assert!(!output.timed_out);
        assert!(output.duration < Duration::from_secs(3));
        let ids: Vec<libc::pid_t> = std::str::from_utf8(&output.stdout).unwrap()
            .split_whitespace().map(|pid| pid.parse().unwrap()).collect();
        assert_eq!(ids.len(), 2);
        for pid in ids {
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bounded_runner_cancel_closes_owned_group_and_preserves_neighbor() {
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready");
        let cancelled = AtomicBool::new(false);
        let mut neighbor_command = Command::new("/bin/sleep");
        neighbor_command.arg("30");
        configure_process_group(&mut neighbor_command);
        let mut neighbor = neighbor_command.spawn().unwrap();
        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !ready.exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                cancelled.store(true, Ordering::Release);
            });
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "/bin/sleep 30 & printf '%s %s\\n' \"$$\" \"$!\"; : > \"$1\"; wait", "owned-cancel"])
                .arg(&ready);
            run_bounded_command(command, Duration::from_secs(5), 1024, "cancel cleanup", &cancelled)
        });
        let survived = neighbor.try_wait().unwrap().is_none();
        neighbor.kill().unwrap();
        neighbor.wait().unwrap();
        assert!(survived);
        let WorkerEnvError::ExecFailedWithOutput { captured, .. } = result.unwrap_err() else {
            panic!("cancellation must retain its execution evidence");
        };
        assert!(captured.cancelled && captured.exit_observed && ready.exists());
        assert!(captured.cleanup_errors.is_empty());
        assert!(captured.stdout_complete && captured.stderr_complete);
        let ids = std::str::from_utf8(&captured.output.stdout).unwrap().split_whitespace()
            .map(|pid| pid.parse::<libc::pid_t>().unwrap()).collect::<Vec<_>>();
        assert_eq!(ids.len(), 2);
        for pid in ids {
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
        }
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

        assert!(Command::new("git").args(["init", "--quiet"])
            .arg(&wt).status().unwrap().success());
        let git_context = ctx_for(std::path::Path::new("/"), &wt);
        let git_sandbox = project_sandbox(&git_worktree_spec(), &caps, &git_context).unwrap();
        let status = run_sandboxed_in_worktree(
            &git_sandbox, ProcessSandboxMode::Enforced, &AbsPath::new(&wt).unwrap(),
            vec!["git".into(), "status".into(), "--short".into(), "--branch".into()],
            Duration::from_secs(5), 4096, &AtomicBool::new(false),
        ).unwrap();
        assert_eq!(status.exit_code, 0, "{}", String::from_utf8_lossy(&status.stderr));
        assert!(!status.timed_out && !status.stdout.is_empty());

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

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn run(script: &str, timeout: Duration, max_output_bytes: usize) -> Result<ExecOutput, WorkerEnvError> {
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/c", script]);
        run_bounded_command_windows(command, timeout, max_output_bytes, "test command", &AtomicBool::new(false))
    }

    #[test]
    fn captures_output_and_exit_code() {
        let output = run("echo hello& echo oops 1>&2& exit /b 3", Duration::from_secs(20), 4096).unwrap();
        assert_eq!(output.exit_code, 3);
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
        assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), "oops");
        assert!(!output.timed_out);
    }

    #[test]
    fn caps_output_and_keeps_the_true_total() {
        let output = run("for /l %i in (1,1,400) do @echo 0123456789", Duration::from_secs(20), 256).unwrap();
        assert!(output.stdout.len() <= 256);
        assert!(output.stdout_truncated);
        assert!(output.stdout_total_bytes > 256);
    }

    #[test]
    fn times_out_and_kills_the_whole_tree() {
        // `start /b` leaves a grandchild that holds the pipe; the job must end it.
        let started = Instant::now();
        let output = run("start /b ping -n 30 127.0.0.1 >nul & ping -n 30 127.0.0.1 >nul", Duration::from_secs(1), 4096).unwrap();
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(15), "{:?}", started.elapsed());
    }

    #[test]
    fn a_cancelled_command_never_starts() {
        let command = Command::new("cmd.exe");
        let error = run_bounded_command_windows(command, Duration::from_secs(5), 64, "test command", &AtomicBool::new(true)).unwrap_err();
        assert!(matches!(error, WorkerEnvError::ExecFailedWithOutput { ref captured, .. } if captured.cancelled));
    }
}
