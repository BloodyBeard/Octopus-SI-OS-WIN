use std::collections::HashMap;
#[cfg(unix)]
use std::fs::File;
#[cfg(not(target_os = "macos"))]
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(unix)]
use std::os::fd::RawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
#[cfg(unix)]
use std::process::Command as StdCommand;
#[cfg(unix)]
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
#[cfg(not(unix))]
use std::time::Duration;

use anyhow::Result;
#[cfg(not(windows))]
use portable_pty::native_pty_system;
use portable_pty::CommandBuilder;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
#[cfg(not(target_os = "macos"))]
use tokio::task::JoinHandle;

#[cfg(all(unix, not(target_os = "macos")))]
use crate::process::exit_code_from_status;
#[cfg(not(target_os = "macos"))]
use crate::process::ChildTerminator;
use crate::process::ProcessHandle;
#[cfg(not(target_os = "macos"))]
use crate::process::ProcessSignal;
use crate::process::PtyHandles;
use crate::process::PtyMasterHandle;
use crate::process::SpawnedProcess;
use crate::process::TerminalSize;

#[cfg(windows)]
pub fn conpty_supported() -> bool {
    crate::win::conpty_supported()
}

#[cfg(not(windows))]
pub fn conpty_supported() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
struct PtyChildTerminator {
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    #[cfg(unix)]
    process_group_id: Option<u32>,
}

#[cfg(not(target_os = "macos"))]
impl ChildTerminator for PtyChildTerminator {
    fn signal(&mut self, signal: ProcessSignal) -> std::io::Result<()> {
        match signal {
            ProcessSignal::Interrupt => {
                #[cfg(unix)]
                if let Some(process_group_id) = self.process_group_id {
                    return crate::process_group::interrupt_process_group(process_group_id);
                }

                Err(crate::process::unsupported_signal(signal))
            }
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id {
            let process_group_kill_result =
                crate::process_group::kill_process_group(process_group_id);
            let child_kill_result = self.killer.kill();
            return match child_kill_result {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == ErrorKind::NotFound => process_group_kill_result,
                Err(err) => process_group_kill_result.or(Err(err)),
            };
        }

        self.killer.kill()
    }

    fn termination_scope_has_exited(&self) -> std::io::Result<bool> {
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id {
            return crate::process_group::process_group_has_exited(process_group_id);
        }

        Ok(true)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
struct RawPidTerminator {
    process_group_id: u32,
}

#[cfg(all(unix, not(target_os = "macos")))]
impl ChildTerminator for RawPidTerminator {
    fn signal(&mut self, signal: ProcessSignal) -> std::io::Result<()> {
        match signal {
            ProcessSignal::Interrupt => {
                crate::process_group::interrupt_process_group(self.process_group_id)
            }
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        crate::process_group::kill_process_group(self.process_group_id)
    }

    fn termination_scope_has_exited(&self) -> std::io::Result<bool> {
        crate::process_group::process_group_has_exited(self.process_group_id)
    }
}

fn platform_native_pty_system() -> Box<dyn portable_pty::PtySystem + Send> {
    #[cfg(windows)]
    {
        Box::new(crate::win::ConPtySystem::default())
    }

    #[cfg(not(windows))]
    {
        native_pty_system()
    }
}

pub async fn spawn_process(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    arg0: &Option<String>,
    size: TerminalSize,
) -> Result<SpawnedProcess> {
    spawn_process_with_inherited_fds(program, args, cwd, env, arg0, size, &[]).await
}

pub async fn spawn_process_with_inherited_fds(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    arg0: &Option<String>,
    size: TerminalSize,
    inherited_fds: &[i32],
) -> Result<SpawnedProcess> {
    if program.is_empty() {
        anyhow::bail!("missing program for PTY spawn");
    }

    #[cfg(not(unix))]
    let _ = inherited_fds;

    #[cfg(unix)]
    if !inherited_fds.is_empty() {
        return spawn_process_preserving_fds(program, args, cwd, env, arg0, size, inherited_fds)
            .await;
    }

    spawn_process_portable(program, args, cwd, env, arg0, size).await
}

async fn spawn_process_portable(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    arg0: &Option<String>,
    size: TerminalSize,
) -> Result<SpawnedProcess> {
    let pty_system = platform_native_pty_system();
    let pair = pty_system.openpty(size.into())?;
    #[cfg(unix)]
    let io = crate::unix_io::PtyIo::new(
        pair.master
            .as_raw_fd()
            .ok_or_else(|| anyhow::anyhow!("PTY master has no file descriptor"))?,
    )?;

    let mut command_builder = CommandBuilder::new(arg0.as_ref().unwrap_or(&program.to_string()));
    command_builder.cwd(cwd);
    command_builder.env_clear();
    for arg in args {
        command_builder.arg(arg);
    }
    for (key, value) in env {
        command_builder.env(key, value);
    }

    let child = pair.slave.spawn_command(command_builder)?;
    #[cfg(not(target_os = "macos"))]
    let mut child = child;
    #[cfg(all(unix, not(target_os = "macos")))]
    let process_group_id = child.process_id();
    #[cfg(not(target_os = "macos"))]
    let killer = child.clone_killer();
    #[cfg(target_os = "macos")]
    let group = Arc::new(crate::owned_process_group::OwnedProcessGroup::from_pty_child(child)?);

    let (writer_tx, writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(128);
    let (_stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>(1);
    #[cfg(unix)]
    let (reader_handle, writer_handle) = io.spawn(
        stdout_tx,
        writer_rx,
        crate::unix_io::StdinCloseBehavior::SendEof,
    );
    #[cfg(not(unix))]
    let (reader_handle, writer_handle) = {
        let mut reader = pair.master.try_clone_reader()?;
        let reader_handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 8_192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = stdout_tx.blocking_send(buf[..n].to_vec());
                    }
                    Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                }
            }
        });

        let mut writer_rx = writer_rx;
        let writer = pair.master.take_writer()?;
        let writer = Arc::new(tokio::sync::Mutex::new(writer));
        let writer_handle: JoinHandle<()> = tokio::spawn({
            let writer = Arc::clone(&writer);
            async move {
                #[cfg(windows)]
                let mut windows_input = crate::WindowsTtyInputNormalizer::default();
                while let Some(bytes) = writer_rx.recv().await {
                    #[cfg(windows)]
                    let bytes = windows_input.normalize(&bytes);
                    let mut guard = writer.lock().await;
                    use std::io::Write;
                    let _ = guard.write_all(&bytes);
                    let _ = guard.flush();
                }
            }
        });
        (reader_handle, writer_handle)
    };

    let (exit_tx, exit_rx) = oneshot::channel::<i32>();
    let exit_status = Arc::new(AtomicBool::new(false));
    #[cfg(not(target_os = "macos"))]
    let wait_exit_status = Arc::clone(&exit_status);
    let exit_code = Arc::new(StdMutex::new(None));
    #[cfg(not(target_os = "macos"))]
    let wait_exit_code = Arc::clone(&exit_code);
    #[cfg(not(target_os = "macos"))]
    let wait_handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
        let code = match child.wait() {
            Ok(status) => status.exit_code() as i32,
            Err(_) => -1,
        };
        wait_exit_status.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut guard) = wait_exit_code.lock() {
            *guard = Some(code);
        }
        let _ = exit_tx.send(code);
    });
    #[cfg(target_os = "macos")]
    let wait_handle = crate::owned_process_group::spawn_waiter(
        Arc::clone(&group), Arc::clone(&exit_status), Arc::clone(&exit_code), exit_tx, true,
    );

    let handles = PtyHandles {
        _slave: if cfg!(windows) {
            Some(pair.slave)
        } else {
            None
        },
        _master: PtyMasterHandle::Resizable(pair.master),
    };

    let handle = ProcessHandle::new(
        writer_tx,
        #[cfg(target_os = "macos")]
        Box::new(group),
        #[cfg(not(target_os = "macos"))]
        Box::new(PtyChildTerminator {
            killer,
            #[cfg(unix)]
            process_group_id,
        }),
        reader_handle,
        Vec::new(),
        writer_handle,
        wait_handle,
        exit_status,
        exit_code,
        Some(handles),
        None,
    );

    Ok(SpawnedProcess {
        session: handle,
        stdout_rx,
        stderr_rx,
        exit_rx,
    })
}

#[cfg(unix)]
async fn spawn_process_preserving_fds(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    arg0: &Option<String>,
    size: TerminalSize,
    inherited_fds: &[RawFd],
) -> Result<SpawnedProcess> {
    let (master, slave) = open_unix_pty(size)?;
    let io = crate::unix_io::PtyIo::new(master.as_raw_fd())?;
    let mut command = StdCommand::new(program);
    if let Some(arg0) = arg0 {
        command.arg0(arg0);
    }
    command.current_dir(cwd);
    command.env_clear();
    for arg in args {
        command.arg(arg);
    }
    for (key, value) in env {
        command.env(key, value);
    }

    let stdin = slave.try_clone()?;
    let stdout = slave.try_clone()?;
    let stderr = slave.try_clone()?;
    let inherited_fds = inherited_fds.to_vec();

    unsafe {
        command
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .pre_exec(move || {
                for signo in &[
                    libc::SIGCHLD,
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGALRM,
                ] {
                    libc::signal(*signo, libc::SIG_DFL);
                }

                let empty_set: libc::sigset_t = std::mem::zeroed();
                libc::sigprocmask(libc::SIG_SETMASK, &empty_set, std::ptr::null_mut());

                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }

                #[allow(clippy::cast_lossless)]
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }

                close_inherited_fds_except(&inherited_fds);
                Ok(())
            });
    }

    let child = command.spawn()?;
    #[cfg(not(target_os = "macos"))]
    let mut child = child;
    drop(slave);
    #[cfg(not(target_os = "macos"))]
    let process_group_id = child.id();
    #[cfg(target_os = "macos")]
    let group = Arc::new(crate::owned_process_group::OwnedProcessGroup::from_child(child));

    let (writer_tx, writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>(128);
    let (_stderr_tx, stderr_rx) = mpsc::channel::<Vec<u8>>(1);
    let (reader_handle, writer_handle) = io.spawn(
        stdout_tx,
        writer_rx,
        crate::unix_io::StdinCloseBehavior::NoEof,
    );

    let (exit_tx, exit_rx) = oneshot::channel::<i32>();
    let exit_status = Arc::new(AtomicBool::new(false));
    #[cfg(not(target_os = "macos"))]
    let wait_exit_status = Arc::clone(&exit_status);
    let exit_code = Arc::new(StdMutex::new(None));
    #[cfg(not(target_os = "macos"))]
    let wait_exit_code = Arc::clone(&exit_code);
    #[cfg(not(target_os = "macos"))]
    let wait_handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
        let code = match child.wait() {
            Ok(status) => exit_code_from_status(status),
            Err(_) => -1,
        };
        wait_exit_status.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut guard) = wait_exit_code.lock() {
            *guard = Some(code);
        }
        let _ = exit_tx.send(code);
    });
    #[cfg(target_os = "macos")]
    let wait_handle = crate::owned_process_group::spawn_waiter(
        Arc::clone(&group), Arc::clone(&exit_status), Arc::clone(&exit_code), exit_tx, false,
    );

    let handles = PtyHandles {
        _slave: None,
        _master: PtyMasterHandle::Opaque {
            raw_fd: master.as_raw_fd(),
            _handle: Box::new(master),
        },
    };

    let handle = ProcessHandle::new(
        writer_tx,
        #[cfg(target_os = "macos")]
        Box::new(group),
        #[cfg(not(target_os = "macos"))]
        Box::new(RawPidTerminator { process_group_id }),
        reader_handle,
        Vec::new(),
        writer_handle,
        wait_handle,
        exit_status,
        exit_code,
        Some(handles),
        None,
    );

    Ok(SpawnedProcess {
        session: handle,
        stdout_rx,
        stderr_rx,
        exit_rx,
    })
}

#[cfg(unix)]
fn open_unix_pty(size: TerminalSize) -> Result<(File, File)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let mut size = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let winp = std::ptr::addr_of_mut!(size);

    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            winp,
        )
    };
    if result != 0 {
        anyhow::bail!("failed to openpty: {:?}", std::io::Error::last_os_error());
    }

    set_cloexec(master)?;
    set_cloexec(slave)?;

    Ok(unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) })
}

#[cfg(unix)]
fn set_cloexec(fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn close_inherited_fds_except(preserved_fds: &[RawFd]) {
    let mut descriptors = [libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    }; 1024];
    let bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            descriptors.as_mut_ptr().cast(),
            std::mem::size_of_val(&descriptors) as libc::c_int,
        )
    };
    let close_inheritable = |fd| {
        if fd <= libc::STDERR_FILENO || preserved_fds.contains(&fd) {
            return;
        }
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::close(fd);
            }
        }
    };
    if bytes > 0 && (bytes as usize) < std::mem::size_of_val(&descriptors) {
        let count = bytes as usize / std::mem::size_of::<libc::proc_fdinfo>();
        for descriptor in descriptors.iter().take(count) {
            close_inheritable(descriptor.proc_fd);
        }
        return;
    }

    let descriptor_table_bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    if descriptor_table_bytes > 0 {
        let upper_bound =
            descriptor_table_bytes as usize / std::mem::size_of::<libc::proc_fdinfo>();
        for fd in libc::STDERR_FILENO + 1..upper_bound as RawFd {
            close_inheritable(fd);
        }
        return;
    }

    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } == 0 {
        let upper_bound = limit.rlim_cur.min(RawFd::MAX as _) as RawFd;
        for fd in libc::STDERR_FILENO + 1..upper_bound {
            close_inheritable(fd);
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn close_inherited_fds_except(preserved_fds: &[RawFd]) {
    if let Ok(dir) = std::fs::read_dir("/dev/fd") {
        let mut fds = Vec::new();
        for entry in dir {
            let num = entry
                .ok()
                .map(|entry| entry.file_name())
                .and_then(|name| name.into_string().ok())
                .and_then(|name| name.parse::<RawFd>().ok());
            if let Some(num) = num {
                if num <= 2 || preserved_fds.contains(&num) {
                    continue;
                }
                let flags = unsafe { libc::fcntl(num, libc::F_GETFD) };
                if flags == -1 || flags & libc::FD_CLOEXEC != 0 {
                    continue;
                }
                fds.push(num);
            }
        }
        for fd in fds {
            unsafe {
                libc::close(fd);
            }
        }
    }
}
