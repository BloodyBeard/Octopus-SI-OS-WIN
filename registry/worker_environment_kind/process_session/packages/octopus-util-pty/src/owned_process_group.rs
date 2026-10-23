use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Keep the root waitable until its group closes, pinning the PID/PGID while
/// signals remain authorized. Observation, signalling and reaping share a lock.
pub struct OwnedProcessGroup {
    state: Mutex<State>,
}

struct State {
    pid: libc::pid_t,
    status: Option<ExitStatus>,
    reap: Option<Box<dyn FnMut() -> io::Result<()> + Send>>,
    failure: Option<String>,
    unreported_exit: Option<tokio::sync::oneshot::Sender<i32>>,
}

impl OwnedProcessGroup {
    pub fn from_child(mut child: Child) -> Self {
        Self::new(child.id(), Box::new(move || child.wait().map(|_| ())))
    }

    pub(crate) fn from_pty_child(
        mut child: Box<dyn portable_pty::Child + Send + Sync>,
    ) -> io::Result<Self> {
        let pid = child
            .process_id()
            .ok_or_else(|| io::Error::other("PTY child has no owned PID"))?;
        Ok(Self::new(pid, Box::new(move || child.wait().map(|_| ()))))
    }

    fn new(pid: u32, reap: Box<dyn FnMut() -> io::Result<()> + Send>) -> Self {
        Self {
            state: Mutex::new(State {
                pid: pid as libc::pid_t,
                status: None,
                reap: Some(reap),
                failure: None,
                unreported_exit: None,
            }),
        }
    }

    pub fn root_exit_status(&self) -> io::Result<Option<ExitStatus>> {
        self.lock()?.observe_exit()
    }

    pub fn signal(&self, signal: libc::c_int) -> io::Result<bool> {
        let mut state = self.lock()?;
        if state.reap_if_finished()? {
            return Ok(false);
        }
        if unsafe { libc::killpg(state.pid, signal) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match state.reap_if_finished() {
            Ok(true) => Ok(false),
            Ok(false) if error.raw_os_error() == Some(libc::ESRCH) => Ok(false),
            Ok(false) => Err(error),
            Err(closure_error) => Err(io::Error::other(format!(
                "process-group signal failed: {error}; closure inspection failed: {closure_error}"
            ))),
        }
    }

    pub fn kill_root(&self) -> io::Result<()> {
        let mut state = self.lock()?;
        if state.observe_exit()?.is_some() || state.reap.is_none() {
            return Ok(());
        }
        if unsafe { libc::kill(state.pid, libc::SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn reap_if_finished(&self) -> io::Result<bool> {
        self.lock()?.reap_if_finished()
    }

    fn lock(&self) -> io::Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| io::Error::other("owned process group lock is poisoned"))
    }

    fn retain_wait_failure(
        &self,
        error: io::Error,
        exit_tx: Option<tokio::sync::oneshot::Sender<i32>>,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .failure
            .get_or_insert_with(|| format!("owned process group observation failed: {error}"));
        state.unreported_exit = exit_tx;
    }
}

impl Drop for OwnedProcessGroup {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(mut reap) = state.reap.take() {
            let pid = state.pid;
            std::thread::spawn(move || {
                if let Err(error) = reap() {
                    eprintln!("owned process group {pid} root reap failed: {error}");
                }
            });
        }
    }
}

impl State {
    fn reap_if_finished(&mut self) -> io::Result<bool> {
        if self.observe_exit()?.is_none() {
            return Ok(false);
        }
        if self.reap.is_none() {
            return Ok(true);
        }
        if group_members(self.pid)?
            .into_iter()
            .any(|pid| pid != self.pid)
        {
            return Ok(false);
        }
        let mut reap = self.reap.take().expect("owned child reaper");
        if let Err(error) = reap() {
            self.failure = Some(format!(
                "owned process group {} could not reap its root: {error}",
                self.pid
            ));
            return Err(io::Error::other(
                self.failure.clone().expect("reap failure"),
            ));
        }
        Ok(true)
    }

    fn observe_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(error) = &self.failure {
            return Err(io::Error::other(error.clone()));
        }
        if self.reap.is_none() {
            return Ok(self.status);
        }
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        loop {
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } == 0
            {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                // An external reaper has released this PID. Dropping a std
                // Child does not wait; neither we nor a runtime orphan reaper
                // may use that numeric PID again.
                self.reap = None;
            }
            self.failure = Some(format!(
                "owned process group {} lost root wait ownership: {error}",
                self.pid
            ));
            return Err(io::Error::other(
                self.failure.clone().expect("wait ownership failure"),
            ));
        }
        if info.si_pid == self.pid {
            let raw = match info.si_code {
                libc::CLD_EXITED => info.si_status << 8,
                libc::CLD_KILLED => info.si_status,
                libc::CLD_DUMPED => info.si_status | 0x80,
                code => {
                    return Err(io::Error::other(format!(
                        "unexpected child exit code {code}"
                    )));
                }
            };
            self.status = Some(ExitStatus::from_raw(raw));
        }
        Ok(self.status)
    }
}

fn group_members(group: libc::pid_t) -> io::Result<Vec<libc::pid_t>> {
    let mut pids = vec![0; 16];
    loop {
        let bytes = libc::c_int::try_from(std::mem::size_of_val(pids.as_slice()))
            .map_err(|_| io::Error::other("owned process group is too large"))?;
        unsafe {
            *libc::__error() = 0;
        }
        let count = unsafe { libc::proc_listpgrppids(group, pids.as_mut_ptr().cast(), bytes) };
        if count < 0 || (count == 0 && io::Error::last_os_error().raw_os_error() != Some(0)) {
            return Err(io::Error::last_os_error());
        }
        if (count as usize) < pids.len() {
            pids.truncate(count as usize);
            pids.retain(|pid| *pid > 0);
            return Ok(pids);
        }
        let capacity = pids
            .len()
            .checked_mul(2)
            .ok_or_else(|| io::Error::other("owned process group is too large"))?;
        pids.resize(capacity, 0);
    }
}

pub(crate) fn spawn_waiter(
    group: Arc<OwnedProcessGroup>,
    exit_status: Arc<AtomicBool>,
    exit_code: Arc<Mutex<Option<i32>>>,
    exit_tx: tokio::sync::oneshot::Sender<i32>,
    portable_status: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut exit_tx = Some(exit_tx);
        loop {
            let status = match group.root_exit_status() {
                Ok(status) => status,
                Err(error) => {
                    group.retain_wait_failure(error, exit_tx.take());
                    return;
                }
            };
            if let Some(status) = status {
                if let Some(exit_tx) = exit_tx.take() {
                    let code = if portable_status {
                        portable_pty::ExitStatus::from(status).exit_code() as i32
                    } else {
                        crate::process::exit_code_from_status(status)
                    };
                    if let Ok(mut value) = exit_code.lock() {
                        *value = Some(code);
                    }
                    exit_status.store(true, Ordering::SeqCst);
                    let _ = exit_tx.send(code);
                }
                match group.reap_if_finished() {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(error) => {
                        group.retain_wait_failure(error, exit_tx.take());
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}

impl crate::process::ChildTerminator for Arc<OwnedProcessGroup> {
    fn signal(&mut self, signal: crate::process::ProcessSignal) -> io::Result<()> {
        match signal {
            crate::process::ProcessSignal::Interrupt => {
                self.as_ref().signal(libc::SIGINT).map(|_| ())
            }
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        self.as_ref().signal(libc::SIGKILL).map(|_| ())
    }

    fn termination_scope_has_exited(&self) -> io::Result<bool> {
        self.reap_if_finished()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    #[tokio::test]
    async fn root_wait_ownership_survives_until_pipe_and_pty_descendants_close()
    -> anyhow::Result<()> {
        let cwd = std::env::current_dir()?;
        let preserved = std::fs::File::open("/dev/null")?;
        for mode in 0..3 {
            let args = vec![
                "-c".into(),
                "trap '' HUP; /bin/sleep 30 & printf '%s %s\\n' \"$$\" \"$!\"; exit 7".into(),
            ];
            let mut spawned = match mode {
                0 => {
                    crate::spawn_pipe_process("/bin/sh", &args, &cwd, &HashMap::new(), &None)
                        .await?
                }
                1 => {
                    crate::spawn_pty_process(
                        "/bin/sh",
                        &args,
                        &cwd,
                        &HashMap::new(),
                        &None,
                        crate::TerminalSize::default(),
                    )
                    .await?
                }
                _ => {
                    crate::pty::spawn_process_with_inherited_fds(
                        "/bin/sh",
                        &args,
                        &cwd,
                        &HashMap::new(),
                        &None,
                        crate::TerminalSize::default(),
                        &[preserved.as_raw_fd()],
                    )
                    .await?
                }
            };
            let output = tokio::time::timeout(Duration::from_secs(3), async {
                let mut bytes = Vec::new();
                while !bytes.contains(&b'\n') {
                    bytes.extend(spawned.stdout_rx.recv().await.expect("process PID output"));
                }
                bytes
            })
            .await?;
            let text = String::from_utf8(output)?;
            let ids: Vec<libc::pid_t> = text
                .split_whitespace()
                .map(str::parse)
                .collect::<Result<_, _>>()?;
            assert_eq!(ids.len(), 2);
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(3), &mut spawned.exit_rx).await??,
                7
            );
            assert!(spawned.session.has_exited());
            assert!(!spawned.session.termination_scope_has_exited()?);
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            assert_eq!(
                unsafe {
                    libc::waitid(
                        libc::P_PID,
                        ids[0] as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                    )
                },
                0
            );
            assert_eq!(
                info.si_pid, ids[0],
                "mode {mode}: root must remain our waitable child"
            );
            assert_eq!(unsafe { libc::kill(ids[1], 0) }, 0);
            spawned
                .session
                .terminate_and_wait(Duration::from_secs(3))
                .await?;
            assert!(spawned.session.termination_scope_has_exited()?);
            assert_eq!(
                unsafe {
                    libc::waitid(
                        libc::P_PID,
                        ids[0] as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                    )
                },
                -1
            );
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
            assert_eq!(unsafe { libc::kill(ids[1], 0) }, -1);
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
            spawned.session.request_terminate()?;
            spawned.session.signal(crate::ProcessSignal::Interrupt)?;
        }
        Ok(())
    }

    #[test]
    fn closed_owner_cannot_signal_a_reused_group_identity() -> io::Result<()> {
        let mut command = std::process::Command::new("/usr/bin/true");
        command.process_group(0);
        let group = OwnedProcessGroup::from_child(command.spawn()?);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while group.root_exit_status()?.is_none() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!group.signal(libc::SIGKILL)?);
        assert_eq!(
            group.root_exit_status()?.and_then(|status| status.code()),
            Some(0)
        );
        assert!(group.reap_if_finished()?);
        let mut neighbor_command = std::process::Command::new("/bin/sleep");
        neighbor_command.arg("30").process_group(0);
        let mut neighbor = neighbor_command.spawn()?;
        group.state.lock().unwrap().pid = neighbor.id() as libc::pid_t;
        let signalled = group.signal(libc::SIGKILL)?;
        let survived = neighbor.try_wait()?.is_none();
        neighbor.kill()?;
        neighbor.wait()?;
        assert!(!signalled && survived);
        assert!(group.reap_if_finished()?);
        Ok(())
    }

    #[tokio::test]
    async fn reap_failure_stops_waiter_and_remains_visible_after_root_exit() -> anyhow::Result<()> {
        let mut command = std::process::Command::new("/usr/bin/true");
        command.process_group(0);
        let mut child = command.spawn()?;
        let group = Arc::new(OwnedProcessGroup::new(
            child.id(),
            Box::new(move || {
                child.wait()?;
                Err(io::Error::other("injected root reap failure"))
            }),
        ));
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        let exited = Arc::new(AtomicBool::new(false));
        let code = Arc::new(Mutex::new(None));
        let waiter = spawn_waiter(
            Arc::clone(&group),
            Arc::clone(&exited),
            Arc::clone(&code),
            exit_tx,
            false,
        );
        let (writer_tx, _) = tokio::sync::mpsc::channel(1);
        let session = crate::ProcessHandle::new(
            writer_tx,
            Box::new(Arc::clone(&group)),
            tokio::spawn(async {}),
            Vec::new(),
            tokio::spawn(async {}),
            tokio::spawn(async {}),
            Arc::clone(&exited),
            code,
            None,
            None,
        );
        session.terminate()?;
        tokio::time::timeout(Duration::from_secs(1), waiter).await??;
        assert_eq!(Some(exit_rx.await?), session.exit_code());
        assert!(exited.load(Ordering::SeqCst));
        let error = group.reap_if_finished().unwrap_err().to_string();
        assert!(error.contains("injected root reap failure"), "{error}");
        assert_eq!(group.root_exit_status().unwrap_err().to_string(), error);
        assert_eq!(group.signal(libc::SIGKILL).unwrap_err().to_string(), error);
        assert_eq!(
            session
                .termination_scope_has_exited()
                .unwrap_err()
                .to_string(),
            error
        );
        let cleanup_error = tokio::time::timeout(
            Duration::from_secs(1),
            session.terminate_and_wait(Duration::from_secs(10)),
        )
        .await?
        .unwrap_err();
        assert_eq!(cleanup_error.to_string(), error);
        Ok(())
    }

    #[tokio::test]
    async fn lost_wait_ownership_stops_waiter_and_is_reported_by_cleanup() -> anyhow::Result<()> {
        let mut command = std::process::Command::new("/usr/bin/true");
        command.process_group(0);
        let child = command.spawn()?;
        let pid = child.id() as libc::pid_t;
        let group = Arc::new(OwnedProcessGroup::from_child(child));
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        let (exit_tx, mut exit_rx) = tokio::sync::oneshot::channel();
        let exited = Arc::new(AtomicBool::new(false));
        let code = Arc::new(Mutex::new(None));
        let waiter = spawn_waiter(
            Arc::clone(&group),
            Arc::clone(&exited),
            Arc::clone(&code),
            exit_tx,
            false,
        );
        tokio::time::timeout(Duration::from_secs(1), waiter).await??;
        assert!(group.state.lock().unwrap().reap.is_none());
        assert!(!exited.load(Ordering::SeqCst));
        assert_eq!(
            exit_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        let (writer_tx, _) = tokio::sync::mpsc::channel(1);
        let session = crate::ProcessHandle::new(
            writer_tx,
            Box::new(group),
            tokio::spawn(async {}),
            Vec::new(),
            tokio::spawn(async {}),
            tokio::spawn(async {}),
            exited,
            code,
            None,
            None,
        );
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            session.terminate_and_wait(Duration::from_secs(10)),
        )
        .await?
        .unwrap_err();
        assert!(
            error.to_string().contains("lost root wait ownership"),
            "{error}"
        );
        assert!(session.termination_scope_has_exited().is_err());
        assert!(session.request_terminate().is_err());
        assert_eq!(
            exit_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        Ok(())
    }
}
