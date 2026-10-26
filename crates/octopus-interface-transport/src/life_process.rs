use std::io;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};
use tokio_util::task::AbortOnDropHandle;

pub(crate) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(unix)]
const OBSERVATION_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) struct LifeProcess {
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
    pid: u32,
    #[cfg(target_os = "macos")]
    group: octopus_util_pty::owned_process_group::OwnedProcessGroup,
    #[cfg(not(target_os = "macos"))]
    child: tokio::process::Child,
    #[cfg(all(unix, not(target_os = "macos")))]
    closed: bool,
    #[cfg(windows)]
    job: octopus_process_job::Job,
}

#[cfg(target_os = "macos")]
impl Drop for LifeProcess {
    fn drop(&mut self) {
        let cleanup = self.group.reap_if_finished().and_then(|closed| {
            if closed {
                Ok(())
            } else {
                self.group.signal(libc::SIGKILL).map(|_| ())
            }
        });
        if let Err(error) = cleanup {
            tracing::error!(pid = self.pid, %error, "life process drop could not terminate its owned group");
        }
    }
}

impl LifeProcess {
    pub(crate) async fn spawn(command: &mut Command) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            let mut child = command.as_std_mut().spawn().map_err(|e| e.to_string())?;
            let pid = child.id();
            let stdin = child.stdin.take();
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let mut process = Self {
                pid,
                stdin: None,
                stdout: None,
                stderr: None,
                group: octopus_util_pty::owned_process_group::OwnedProcessGroup::from_child(child),
            };
            let streams = (|| -> io::Result<()> {
                process.stdin = stdin.map(ChildStdin::from_std).transpose()?;
                process.stdout = stdout.map(ChildStdout::from_std).transpose()?;
                process.stderr = stderr.map(ChildStderr::from_std).transpose()?;
                Ok(())
            })();
            if let Err(error) = streams {
                let cleanup = process.terminate().await;
                return Err(match cleanup {
                    Ok(()) => format!("life pipe setup failed: {error}"),
                    Err(cleanup) => format!("life pipe setup failed: {error}; {cleanup}"),
                });
            }
            Ok(process)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut child = command.spawn().map_err(|e| e.to_string())?;
            #[cfg(windows)]
            let job = match child.raw_handle().map(octopus_process_job::Job::containing) {
                Some(Ok(job)) => job,
                failed => {
                    let _ = child.kill().await;
                    return Err(match failed {
                        Some(Err(error)) => format!("life process job cannot hold the child: {error}"),
                        _ => "life child exited before its job could hold it".to_string(),
                    });
                }
            };
            Ok(Self {
                pid: child.id().ok_or("life child has no owned PID")?,
                stdin: child.stdin.take(),
                stdout: child.stdout.take(),
                stderr: child.stderr.take(),
                child,
                #[cfg(unix)]
                closed: false,
                #[cfg(windows)]
                job,
            })
        }
    }

    pub(crate) fn id(&self) -> u32 {
        self.pid
    }

    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
        #[cfg(unix)]
        loop {
            #[cfg(target_os = "macos")]
            let status = self.group.root_exit_status()?;
            #[cfg(not(target_os = "macos"))]
            let status = self.observe_root()?;
            if let Some(status) = status {
                return Ok(status);
            }
            tokio::time::sleep(OBSERVATION_INTERVAL).await;
        }
        #[cfg(not(unix))]
        self.child.wait().await
    }

    pub(crate) async fn protocol_closed(&mut self) -> Result<(), String> {
        match tokio::time::timeout(CLEANUP_TIMEOUT, self.wait()).await {
            Ok(status) => exit_result(status),
            Err(_) => Err(
                "life process closed its protocol output before exit; exit observation timed out"
                    .into(),
            ),
        }
    }

    pub(crate) async fn terminate(&mut self) -> Result<(), String> {
        tokio::time::timeout(CLEANUP_TIMEOUT, self.terminate_owned())
            .await
            .map_err(|_| "life process group closure timed out".to_string())?
            .map_err(|error| format!("life process group termination failed: {error}"))
    }

    async fn terminate_owned(&mut self) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            if self.group.reap_if_finished()? {
                return Ok(());
            }
            self.group.signal(libc::SIGKILL)?;
            while !self.group.reap_if_finished()? {
                tokio::time::sleep(OBSERVATION_INTERVAL).await;
            }
            Ok(())
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if self.closed {
                return Ok(());
            }
            if self.child.id().is_some() {
                self.observe_root()?;
                if unsafe { libc::killpg(self.pid as libc::pid_t, libc::SIGKILL) } == -1 {
                    return Err(io::Error::last_os_error());
                }
                self.child.wait().await?;
            }
            loop {
                if unsafe { libc::killpg(self.pid as libc::pid_t, 0) } == -1 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error);
                    }
                    self.closed = true;
                    return Ok(());
                }
                tokio::time::sleep(OBSERVATION_INTERVAL).await;
            }
        }
        #[cfg(windows)]
        {
            // The job holds the child and everything it started, like the
            // Unix process group.
            self.job.terminate()?;
            self.child.wait().await?;
            Ok(())
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn observe_root(&self) -> io::Result<Option<ExitStatus>> {
        use std::os::unix::process::ExitStatusExt;
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
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        let status = unsafe { info.si_status() };
        let raw = match info.si_code {
            libc::CLD_EXITED => status << 8,
            libc::CLD_KILLED => status,
            libc::CLD_DUMPED => status | 0x80,
            code => {
                return Err(io::Error::other(format!(
                    "unexpected life exit code {code}"
                )));
            }
        };
        Ok(Some(ExitStatus::from_raw(raw)))
    }
}

pub(crate) fn exit_result(status: io::Result<ExitStatus>) -> Result<(), String> {
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("life process exited with {status}")),
        Err(error) => Err(format!("life process wait failed: {error}")),
    }
}

pub(crate) async fn finish_stderr(
    mut task: AbortOnDropHandle<Result<(), String>>,
) -> Result<(), String> {
    match tokio::time::timeout(CLEANUP_TIMEOUT, &mut task).await {
        Ok(result) => result.map_err(|error| format!("life stderr task failed: {error}"))?,
        Err(_) => {
            task.abort();
            Err("life stderr closure timed out; capture task aborted".into())
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn life_exit_eof_and_cleanup_preserve_owned_identity_and_bound_stderr() {
        for script in ["exit 0", "exit 7", "exec 1>&-; /bin/sleep 30"] {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", script])
                .process_group(0)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let mut child = LifeProcess::spawn(&mut command).await.unwrap();
            let mut bytes = Vec::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_end(&mut bytes)
                .await
                .unwrap();
            let result = child.protocol_closed().await;
            match script {
                "exit 0" => assert!(result.is_ok(), "{result:?}"),
                "exit 7" => assert!(result.unwrap_err().contains("exit status: 7")),
                _ => assert!(result.unwrap_err().contains("exit observation timed out")),
            }
            #[cfg(target_os = "macos")]
            if script == "exit 0" {
                assert_eq!(
                    unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) },
                    -1
                );
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
            }
            child.terminate().await.unwrap();
            let mut neighbor = Command::new("/bin/sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .unwrap();
            child.pid = neighbor.id().unwrap();
            child.terminate().await.unwrap();
            assert!(neighbor.try_wait().unwrap().is_none());
            neighbor.kill().await.unwrap();
        }
        let stuck =
            AbortOnDropHandle::new(tokio::spawn(std::future::pending::<Result<(), String>>()));
        let abort = stuck.abort_handle();
        assert!(
            finish_stderr(stuck)
                .await
                .unwrap_err()
                .contains("stderr closure timed out")
        );
        tokio::task::yield_now().await;
        assert!(abort.is_finished());
    }
}
