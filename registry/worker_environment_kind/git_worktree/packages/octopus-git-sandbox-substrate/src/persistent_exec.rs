use octopus_descriptors::ProcessSandboxMode;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use octopus_exec::{
    Environment, ExecOutputStream, ExecParams, ExecProcess, ExecProcessEvent,
    ExecProcessEventReceiver, ExecServerRuntimePaths, ProcessId, ProcessSignal, WriteStatus,
};
use octopus_util_pathuri::PathUri;
use octopus_worker_env::{AbsPath, ProjectedSandbox, WorkerEnvironmentId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitWorktreeProcessLimits {
    pub max_processes_per_worker_environment: usize,
    pub max_buffered_output_bytes: usize,
    pub max_buffered_output_events: usize,
    pub max_input_bytes: usize,
    pub process_timeout: Duration,
    pub cleanup_timeout: Duration,
}

impl GitWorktreeProcessLimits {
    fn validate(&self) -> Result<(), String> {
        if self.max_processes_per_worker_environment == 0 {
            return Err("persistent coder process count ceiling must be positive".to_string());
        }
        if self.max_buffered_output_bytes == 0 {
            return Err("persistent coder buffered output ceiling must be positive".to_string());
        }
        if self.max_buffered_output_events == 0 {
            return Err("persistent coder buffered event ceiling must be positive".to_string());
        }
        if self.max_input_bytes == 0 {
            return Err("persistent coder input ceiling must be positive".to_string());
        }
        if self.process_timeout.is_zero() {
            return Err("persistent coder process timeout must be positive".to_string());
        }
        if self.cleanup_timeout.is_zero() {
            return Err("persistent coder cleanup timeout must be positive".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitWorktreeProcessChunk {
    pub seq: u64,
    pub stream: ExecOutputStream,
    pub byte_offset: usize,
    pub end_of_event: bool,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitWorktreeProcessPoll {
    pub process_id: String,
    pub chunks: Vec<GitWorktreeProcessChunk>,
    pub next_seq: u64,
    pub running: bool,
    pub exited: bool,
    pub exit_code: Option<i32>,
    pub exit_seq: Option<u64>,
    pub closed: bool,
    pub close_seq: Option<u64>,
    pub continuation_available: bool,
    pub timed_out: bool,
    pub failure: Option<String>,
    pub tty: bool,
    pub cwd: AbsPath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitWorktreeProcessInfo {
    pub process_id: String,
    pub running: bool,
    pub exited: bool,
    pub exit_code: Option<i32>,
    pub closed: bool,
    pub timed_out: bool,
    pub failure: Option<String>,
    pub continuation_available: bool,
    pub pending_output_bytes: usize,
    pub next_source_seq: u64,
    pub tty: bool,
    pub cwd: AbsPath,
}

pub struct GitWorktreeProcessManager {
    next_process_id: AtomicU64,
    processes: tokio::sync::Mutex<ProcessTable>,
    activity: Arc<tokio::sync::Notify>,
}

#[derive(Default)]
struct ProcessTable {
    by_environment: BTreeMap<WorkerEnvironmentId, ProcessEnvironment>,
}

#[derive(Default)]
struct ProcessEnvironment {
    processes: BTreeMap<String, Arc<ManagedProcess>>,
    starts_in_flight: usize,
    releasing: bool,
    max_processes: Option<usize>,
    cleanup_timeout: Option<Duration>,
}

struct ManagedProcess {
    process: Arc<dyn ExecProcess>,
    state: tokio::sync::Mutex<ManagedProcessState>,
    poll_lock: tokio::sync::Mutex<()>,
    notify: tokio::sync::Notify,
    tty: bool,
    cwd: AbsPath,
    max_input_bytes: usize,
}

struct ManagedProcessState {
    pending: VecDeque<PendingChunk>,
    pending_bytes: usize,
    max_buffered_output_bytes: usize,
    max_buffered_output_events: usize,
    next_expected_seq: u64,
    exited: bool,
    exit_code: Option<i32>,
    exit_seq: Option<u64>,
    closed: bool,
    close_seq: Option<u64>,
    timed_out: bool,
    releasing: bool,
    failure: Option<String>,
}

struct PendingChunk {
    seq: u64,
    stream: ExecOutputStream,
    bytes: Vec<u8>,
    source_offset: usize,
    source_complete: bool,
}

impl Default for GitWorktreeProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GitWorktreeProcessManager {
    pub fn new() -> Self {
        Self {
            next_process_id: AtomicU64::new(1),
            processes: tokio::sync::Mutex::new(ProcessTable::default()),
            activity: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub async fn start(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        sandbox: &ProjectedSandbox,
        sandbox_mode: ProcessSandboxMode,
        mut argv: Vec<String>,
        cwd: PathBuf,
        mut env: BTreeMap<String, String>,
        tty: bool,
        wait_ms: u64,
        max_output_bytes: usize,
        limits: GitWorktreeProcessLimits,
    ) -> Result<GitWorktreeProcessPoll, String> {
        limits.validate()?;
        validate_poll_output_ceiling(max_output_bytes)?;
        if argv.is_empty() || argv.iter().any(String::is_empty) {
            return Err("persistent coder process argv must contain non-empty strings".to_string());
        }
        let cwd = AbsPath::new(&cwd)
            .map_err(|error| format!("persistent coder process cwd is invalid: {error}"))?;
        sandbox
            .authorize_read(&cwd)
            .map_err(|error| format!("persistent coder process cwd is denied: {error}"))?;
        resolve_persistent_process_program(&mut argv, &env, cwd.as_path())?;
        let argv = crate::sandboxed_worktree_process_argv(sandbox, sandbox_mode, &cwd, argv)
            .map_err(|error| format!("persistent coder sandbox floor failed: {error}"))?;
        self.reserve_start(worker_environment_id, &limits).await?;

        env.insert("TMPDIR".to_string(), cwd.as_str().to_string());
        env.insert("TMP".to_string(), cwd.as_str().to_string());
        env.insert("TEMP".to_string(), cwd.as_str().to_string());
        let ordinal = self
            .next_process_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map_err(|_| "persistent coder process id space exhausted".to_string());
        let ordinal = match ordinal {
            Ok(ordinal) => ordinal,
            Err(error) => {
                return match self.cancel_start(worker_environment_id).await {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => Err(format!(
                        "{error}; persistent process reservation cleanup failed: {cleanup_error}"
                    )),
                };
            }
        };
        let process_id = format!("coder-process-{ordinal:016x}");
        let process = match start_exec_process(&process_id, argv, &cwd, env, tty).await {
            Ok(process) => process,
            Err(error) => {
                return match self.cancel_start(worker_environment_id).await {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => Err(format!(
                        "{error}; persistent process reservation cleanup failed: {cleanup_error}"
                    )),
                };
            }
        };
        let events = process.subscribe_events();
        let managed = Arc::new(ManagedProcess {
            process,
            state: tokio::sync::Mutex::new(ManagedProcessState::new(
                limits.max_buffered_output_bytes,
                limits.max_buffered_output_events,
            )),
            poll_lock: tokio::sync::Mutex::new(()),
            notify: tokio::sync::Notify::new(),
            tty,
            cwd: cwd.clone(),
            max_input_bytes: limits.max_input_bytes,
        });
        spawn_event_collector(Arc::clone(&managed), events, Arc::clone(&self.activity));
        spawn_process_timeout(
            Arc::clone(&managed),
            limits.process_timeout,
            Arc::clone(&self.activity),
        );
        let releasing = match self
            .commit_start(worker_environment_id, &process_id, Arc::clone(&managed))
            .await
        {
            Ok(releasing) => releasing,
            Err(error) => {
                let _ = managed.process.terminate().await;
                return Err(error);
            }
        };
        if releasing {
            let _ = managed.process.terminate().await;
            return Err(format!(
                "worker environment '{}' began release while persistent process '{}' was starting",
                worker_environment_id, process_id
            ));
        }
        self.poll_managed(
            worker_environment_id,
            &process_id,
            Arc::clone(&managed),
            wait_ms,
            max_output_bytes,
        )
        .await
    }

    pub async fn write_and_poll(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
        input: &[u8],
        wait_ms: u64,
        max_output_bytes: usize,
    ) -> Result<GitWorktreeProcessPoll, String> {
        validate_poll_output_ceiling(max_output_bytes)?;
        let managed = self
            .managed_process(worker_environment_id, process_id)
            .await?;
        let _poll = managed.poll_lock.lock().await;
        if managed.state.lock().await.releasing {
            return Err(format!(
                "worker environment '{}' is releasing and cannot continue process '{}'",
                worker_environment_id, process_id
            ));
        }
        if input.len() > managed.max_input_bytes {
            return Err(format!(
                "persistent coder stdin contains {} bytes, exceeding the descriptor-owned {} byte ceiling",
                input.len(),
                managed.max_input_bytes
            ));
        }
        if !input.is_empty() {
            let response = managed
                .process
                .write(input.to_vec())
                .await
                .map_err(|error| format!("persistent coder stdin write failed: {error}"))?;
            if response.status != WriteStatus::Accepted {
                return Err(format!(
                    "persistent coder stdin write was not accepted: {:?}",
                    response.status
                ));
            }
        }
        self.poll_managed_locked(
            worker_environment_id,
            process_id,
            Arc::clone(&managed),
            wait_ms,
            max_output_bytes,
        )
        .await
    }

    pub async fn interrupt(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
    ) -> Result<(), String> {
        let managed = self
            .managed_process(worker_environment_id, process_id)
            .await?;
        let response = managed
            .process
            .signal(ProcessSignal::Interrupt)
            .await
            .map_err(|error| format!("persistent coder interrupt failed: {error}"))?;
        if !response.delivered {
            return Err(format!(
                "persistent coder process '{}' is not running",
                process_id
            ));
        }
        Ok(())
    }

    pub async fn interrupt_and_poll(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
        wait_ms: u64,
        max_output_bytes: usize,
    ) -> Result<GitWorktreeProcessPoll, String> {
        validate_poll_output_ceiling(max_output_bytes)?;
        let managed = self
            .managed_process(worker_environment_id, process_id)
            .await?;
        let _poll = managed.poll_lock.lock().await;
        if managed.state.lock().await.releasing {
            return Err(format!(
                "worker environment '{}' is releasing and cannot interrupt process '{}'",
                worker_environment_id, process_id
            ));
        }
        let response = managed
            .process
            .signal(ProcessSignal::Interrupt)
            .await
            .map_err(|error| format!("persistent coder interrupt failed: {error}"))?;
        if !response.delivered {
            return Err(format!(
                "persistent coder process '{}' is not running",
                process_id
            ));
        }
        self.poll_managed_locked(
            worker_environment_id,
            process_id,
            Arc::clone(&managed),
            wait_ms,
            max_output_bytes,
        )
        .await
    }

    pub async fn terminate_and_poll(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
        wait_ms: u64,
        max_output_bytes: usize,
    ) -> Result<GitWorktreeProcessPoll, String> {
        validate_poll_output_ceiling(max_output_bytes)?;
        let managed = self
            .managed_process(worker_environment_id, process_id)
            .await?;
        let _poll = managed.poll_lock.lock().await;
        {
            let state = managed.state.lock().await;
            if state.releasing {
                return Err(format!(
                    "worker environment '{}' is releasing and cannot terminate process '{}'",
                    worker_environment_id, process_id
                ));
            }
            if state.exited || state.closed {
                return Err(format!(
                    "persistent coder process '{}' is not running",
                    process_id
                ));
            }
        }
        managed
            .process
            .terminate()
            .await
            .map_err(|error| format!("persistent coder terminate failed: {error}"))?;
        self.poll_managed_locked(
            worker_environment_id,
            process_id,
            Arc::clone(&managed),
            wait_ms,
            max_output_bytes,
        )
        .await
    }

    pub async fn list(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
    ) -> Vec<GitWorktreeProcessInfo> {
        let processes = {
            let table = self.processes.lock().await;
            table
                .by_environment
                .get(worker_environment_id)
                .map(|environment| {
                    environment
                        .processes
                        .iter()
                        .map(|(process_id, managed)| (process_id.clone(), Arc::clone(managed)))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let mut listed = Vec::with_capacity(processes.len());
        for (process_id, managed) in processes {
            let state = managed.state.lock().await;
            listed.push(GitWorktreeProcessInfo {
                process_id,
                running: !state.exited && !state.closed,
                exited: state.exited,
                exit_code: state.exit_code,
                closed: state.closed,
                timed_out: state.timed_out,
                failure: state.failure.clone(),
                continuation_available: !state.closed || state.pending_bytes > 0,
                pending_output_bytes: state.pending_bytes,
                next_source_seq: state.next_expected_seq,
                tty: managed.tty,
                cwd: managed.cwd.clone(),
            });
        }
        listed
    }

    pub async fn release(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        cleanup_timeout: Option<Duration>,
    ) -> Result<(), String> {
        let cleanup_timeout = {
            let mut table = self.processes.lock().await;
            let Some(environment) = table.by_environment.get_mut(worker_environment_id) else {
                return Ok(());
            };
            if let (Some(declared), Some(owned)) = (cleanup_timeout, environment.cleanup_timeout) {
                if declared != owned {
                    return Err(format!(
                        "worker environment '{}' cleanup timeout differs from its persistent process declaration",
                        worker_environment_id
                    ));
                }
            }
            let resolved = cleanup_timeout.or(environment.cleanup_timeout).ok_or_else(|| {
                format!(
                    "worker environment '{}' has no descriptor-owned persistent process cleanup timeout",
                    worker_environment_id
                )
            })?;
            if resolved.is_zero() {
                return Err("persistent coder cleanup timeout must be positive".to_string());
            }
            environment.releasing = true;
            resolved
        };
        self.activity.notify_one();
        match tokio::time::timeout(
            cleanup_timeout,
            self.release_marked_environment(worker_environment_id),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(format!(
                "worker environment '{}' persistent process closure was not proven within {} ms",
                worker_environment_id,
                cleanup_timeout.as_millis()
            )),
        }
    }

    pub async fn release_all(&self) -> Result<(), String> {
        let worker_environment_ids = self
            .processes
            .lock()
            .await
            .by_environment
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut failures = Vec::new();
        for worker_environment_id in worker_environment_ids {
            if let Err(error) = self.release(&worker_environment_id, None).await {
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "persistent coder process quiesce failed: {}",
                failures.join("; ")
            ))
        }
    }

    async fn reserve_start(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        limits: &GitWorktreeProcessLimits,
    ) -> Result<(), String> {
        let mut table = self.processes.lock().await;
        let environment = table
            .by_environment
            .entry(worker_environment_id.clone())
            .or_default();
        if environment.releasing {
            return Err(format!(
                "worker environment '{}' is releasing and cannot start a persistent process",
                worker_environment_id
            ));
        }
        if let Some(max_processes) = environment.max_processes {
            if max_processes != limits.max_processes_per_worker_environment {
                return Err(format!(
                    "worker environment '{}' received inconsistent persistent process count ceilings",
                    worker_environment_id
                ));
            }
        }
        if let Some(cleanup_timeout) = environment.cleanup_timeout {
            if cleanup_timeout != limits.cleanup_timeout {
                return Err(format!(
                    "worker environment '{}' received inconsistent persistent cleanup timeouts",
                    worker_environment_id
                ));
            }
        }
        let owned_processes = environment
            .processes
            .len()
            .checked_add(environment.starts_in_flight)
            .ok_or_else(|| {
                format!(
                    "worker environment '{}' persistent process count overflowed",
                    worker_environment_id
                )
            })?;
        if owned_processes >= limits.max_processes_per_worker_environment {
            return Err(format!(
                "worker environment '{}' already owns the descriptor maximum of {} persistent processes",
                worker_environment_id, limits.max_processes_per_worker_environment
            ));
        }
        environment.max_processes = Some(limits.max_processes_per_worker_environment);
        environment.cleanup_timeout = Some(limits.cleanup_timeout);
        environment.starts_in_flight += 1;
        Ok(())
    }

    async fn cancel_start(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
    ) -> Result<(), String> {
        let mut table = self.processes.lock().await;
        let environment = table
            .by_environment
            .get_mut(worker_environment_id)
            .ok_or_else(|| {
                format!(
                    "worker environment '{}' lost its persistent process reservation",
                    worker_environment_id
                )
            })?;
        if environment.starts_in_flight == 0 {
            return Err(format!(
                "worker environment '{}' persistent process reservation underflowed",
                worker_environment_id
            ));
        }
        environment.starts_in_flight -= 1;
        if environment.starts_in_flight == 0
            && environment.processes.is_empty()
            && !environment.releasing
        {
            table.by_environment.remove(worker_environment_id);
        }
        drop(table);
        self.activity.notify_one();
        Ok(())
    }

    async fn commit_start(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
        managed: Arc<ManagedProcess>,
    ) -> Result<bool, String> {
        let mut table = self.processes.lock().await;
        let environment = table
            .by_environment
            .get_mut(worker_environment_id)
            .ok_or_else(|| {
                format!(
                    "worker environment '{}' lost its persistent process reservation",
                    worker_environment_id
                )
            })?;
        if environment.starts_in_flight == 0 {
            return Err(format!(
                "worker environment '{}' persistent process reservation underflowed",
                worker_environment_id
            ));
        }
        environment.starts_in_flight -= 1;
        if environment.processes.contains_key(process_id) {
            return Err(format!(
                "persistent coder process id '{}' collided inside worker environment '{}'",
                process_id, worker_environment_id
            ));
        }
        environment
            .processes
            .insert(process_id.to_string(), managed);
        let releasing = environment.releasing;
        drop(table);
        self.activity.notify_one();
        Ok(releasing)
    }

    async fn release_marked_environment(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
    ) -> Result<(), String> {
        let processes = loop {
            let notified = self.activity.notified();
            let state = {
                let table = self.processes.lock().await;
                let Some(environment) = table.by_environment.get(worker_environment_id) else {
                    return Ok(());
                };
                if environment.starts_in_flight == 0 {
                    Some(environment.processes.values().cloned().collect::<Vec<_>>())
                } else {
                    None
                }
            };
            if let Some(processes) = state {
                break processes;
            }
            notified.await;
        };
        for managed in &processes {
            managed.state.lock().await.releasing = true;
            managed.notify.notify_one();
        }
        for managed in &processes {
            let _ = managed.process.terminate().await;
        }
        loop {
            let notified = self.activity.notified();
            let all_closed = {
                let mut all_closed = true;
                for managed in &processes {
                    if !managed.state.lock().await.closed {
                        all_closed = false;
                        break;
                    }
                }
                all_closed
            };
            if all_closed {
                break;
            }
            notified.await;
        }
        self.processes
            .lock()
            .await
            .by_environment
            .remove(worker_environment_id);
        Ok(())
    }

    async fn poll_managed(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
        managed: Arc<ManagedProcess>,
        wait_ms: u64,
        max_output_bytes: usize,
    ) -> Result<GitWorktreeProcessPoll, String> {
        let _poll = managed.poll_lock.lock().await;
        self.poll_managed_locked(
            worker_environment_id,
            process_id,
            Arc::clone(&managed),
            wait_ms,
            max_output_bytes,
        )
        .await
    }

    async fn poll_managed_locked(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
        managed: Arc<ManagedProcess>,
        wait_ms: u64,
        max_output_bytes: usize,
    ) -> Result<GitWorktreeProcessPoll, String> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        loop {
            let terminal = {
                let state = managed.state.lock().await;
                state.closed || state.failure.is_some() || state.timed_out
            };
            if terminal || wait_ms == 0 || tokio::time::Instant::now() >= deadline {
                break;
            }
            if tokio::time::timeout_at(deadline, managed.notify.notified())
                .await
                .is_err()
            {
                break;
            }
        }
        let (poll, remove) = {
            let mut state = managed.state.lock().await;
            let chunks = state.drain(max_output_bytes);
            let continuation_available = !state.closed || state.pending_bytes > 0;
            let poll = GitWorktreeProcessPoll {
                process_id: process_id.to_string(),
                chunks,
                next_seq: state.next_expected_seq,
                running: !state.exited && !state.closed,
                exited: state.exited,
                exit_code: state.exit_code,
                exit_seq: state.exit_seq,
                closed: state.closed,
                close_seq: state.close_seq,
                continuation_available,
                timed_out: state.timed_out,
                failure: state.failure.clone(),
                tty: managed.tty,
                cwd: managed.cwd.clone(),
            };
            (poll, state.closed && state.pending_bytes == 0)
        };
        if remove {
            self.remove_if_same(worker_environment_id, process_id, &managed)
                .await;
        }
        Ok(poll)
    }

    async fn managed_process(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
    ) -> Result<Arc<ManagedProcess>, String> {
        let table = self.processes.lock().await;
        let environment = table
            .by_environment
            .get(worker_environment_id)
            .ok_or_else(|| {
                format!(
                    "worker environment '{}' has no persistent process '{}'",
                    worker_environment_id, process_id
                )
            })?;
        if environment.releasing {
            return Err(format!(
                "worker environment '{}' is releasing and cannot continue process '{}'",
                worker_environment_id, process_id
            ));
        }
        environment
            .processes
            .get(process_id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "worker environment '{}' has no persistent process '{}'",
                    worker_environment_id, process_id
                )
            })
    }

    async fn remove_if_same(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
        managed: &Arc<ManagedProcess>,
    ) {
        let mut table = self.processes.lock().await;
        let mut remove_environment = false;
        if let Some(environment) = table.by_environment.get_mut(worker_environment_id) {
            if environment
                .processes
                .get(process_id)
                .is_some_and(|current| Arc::ptr_eq(current, managed))
            {
                environment.processes.remove(process_id);
            }
            remove_environment = environment.processes.is_empty()
                && environment.starts_in_flight == 0
                && !environment.releasing;
        }
        if remove_environment {
            table.by_environment.remove(worker_environment_id);
        }
    }
}

impl ManagedProcessState {
    fn new(max_buffered_output_bytes: usize, max_buffered_output_events: usize) -> Self {
        Self {
            pending: VecDeque::new(),
            pending_bytes: 0,
            max_buffered_output_bytes,
            max_buffered_output_events,
            next_expected_seq: 1,
            exited: false,
            exit_code: None,
            exit_seq: None,
            closed: false,
            close_seq: None,
            timed_out: false,
            releasing: false,
            failure: None,
        }
    }

    fn record(&mut self, event: ExecProcessEvent) -> bool {
        let Some(seq) = event_seq(&event) else {
            if let ExecProcessEvent::Failed(message) = event {
                self.fail(format!(
                    "persistent coder process event stream failed: {message}"
                ));
            }
            return true;
        };
        if seq != self.next_expected_seq {
            self.fail(format!(
                "persistent coder process event history has a gap: expected seq {}, received {}",
                self.next_expected_seq, seq
            ));
        }
        self.next_expected_seq = match seq.checked_add(1) {
            Some(next) => next,
            None => {
                self.fail("persistent coder process event sequence exhausted".to_string());
                u64::MAX
            }
        };
        let mut terminate = self.failure.is_some();
        match event {
            ExecProcessEvent::Output(chunk) => {
                let event_available = self.pending.len() < self.max_buffered_output_events;
                let available = match self
                    .max_buffered_output_bytes
                    .checked_sub(self.pending_bytes)
                {
                    Some(available) => available,
                    None => {
                        self.fail(
                            "persistent coder unread output accounting exceeded its descriptor ceiling"
                                .to_string(),
                        );
                        terminate = true;
                        0
                    }
                };
                let retained = if event_available {
                    chunk.chunk.0.len().min(available)
                } else {
                    0
                };
                if retained > 0 {
                    self.pending.push_back(PendingChunk {
                        seq,
                        stream: chunk.stream,
                        bytes: chunk.chunk.0[..retained].to_vec(),
                        source_offset: 0,
                        source_complete: retained == chunk.chunk.0.len(),
                    });
                    self.pending_bytes += retained;
                }
                if !event_available && !chunk.chunk.0.is_empty() {
                    self.fail(format!(
                        "persistent coder process output exceeded the descriptor-owned {} unread event ceiling",
                        self.max_buffered_output_events
                    ));
                    terminate = true;
                } else if retained != chunk.chunk.0.len() {
                    self.fail(format!(
                        "persistent coder process output exceeded the descriptor-owned {} byte unread buffer ceiling",
                        self.max_buffered_output_bytes
                    ));
                    terminate = true;
                }
            }
            ExecProcessEvent::Exited { exit_code, .. } => {
                self.exited = true;
                self.exit_code = Some(exit_code);
                self.exit_seq = Some(seq);
            }
            ExecProcessEvent::Closed { .. } => {
                self.closed = true;
                self.close_seq = Some(seq);
            }
            ExecProcessEvent::Failed(_) => unreachable!(),
        }
        terminate
    }

    fn fail(&mut self, message: String) {
        if self.failure.is_none() {
            self.failure = Some(message);
        }
    }

    fn drain(&mut self, max_output_bytes: usize) -> Vec<GitWorktreeProcessChunk> {
        let mut remaining_budget = max_output_bytes;
        let mut drained = Vec::new();
        while remaining_budget > 0 {
            let Some(front) = self.pending.front_mut() else {
                break;
            };
            let take = front.bytes.len().min(remaining_budget);
            let start = front.source_offset;
            let mut bytes = if take == front.bytes.len() {
                std::mem::take(&mut front.bytes)
            } else {
                let unread = front.bytes.split_off(take);
                std::mem::replace(&mut front.bytes, unread)
            };
            bytes.shrink_to_fit();
            front.source_offset += take;
            let event_drained = front.bytes.is_empty();
            drained.push(GitWorktreeProcessChunk {
                seq: front.seq,
                stream: front.stream,
                byte_offset: start,
                end_of_event: event_drained && front.source_complete,
                bytes,
            });
            self.pending_bytes -= take;
            remaining_budget -= take;
            if event_drained {
                self.pending.pop_front();
            }
        }
        drained
    }
}

async fn start_exec_process(
    process_id: &str,
    argv: Vec<String>,
    cwd: &AbsPath,
    env: BTreeMap<String, String>,
    tty: bool,
) -> Result<Arc<dyn ExecProcess>, String> {
    let runtime_paths = ExecServerRuntimePaths::new(
        std::env::current_exe().map_err(|error| {
            format!("persistent coder process executable is unavailable: {error}")
        })?,
        None,
    )
    .map_err(|error| format!("persistent coder process runtime paths are invalid: {error}"))?;
    let environment = Environment::create(None, runtime_paths)
        .map_err(|error| format!("persistent coder process environment failed: {error}"))?;
    environment
        .get_exec_backend()
        .start(ExecParams {
            process_id: ProcessId::new(process_id),
            argv,
            cwd: PathUri::from_path(cwd.as_path())
                .map_err(|error| format!("persistent coder process cwd URI failed: {error}"))?,
            env_policy: None,
            env: env.into_iter().collect(),
            tty,
            pipe_stdin: true,
            arg0: None,
        })
        .await
        .map(|started| started.process)
        .map_err(|error| format!("persistent coder process failed to start: {error}"))
}

fn spawn_event_collector(
    managed: Arc<ManagedProcess>,
    mut events: ExecProcessEventReceiver,
    activity: Arc<tokio::sync::Notify>,
) {
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    managed.state.lock().await.fail(format!(
                        "persistent coder process event collector lagged by {skipped} events"
                    ));
                    managed.notify.notify_one();
                    activity.notify_one();
                    let _ = managed.process.terminate().await;
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    managed.state.lock().await.fail(
                        "persistent coder process event stream closed before closure evidence"
                            .to_string(),
                    );
                    managed.notify.notify_one();
                    activity.notify_one();
                    break;
                }
            };
            let is_closed = matches!(event, ExecProcessEvent::Closed { .. });
            let terminate = managed.state.lock().await.record(event);
            managed.notify.notify_one();
            activity.notify_one();
            if terminate && !is_closed {
                let _ = managed.process.terminate().await;
            }
            if is_closed {
                break;
            }
        }
    });
}

fn spawn_process_timeout(
    managed: Arc<ManagedProcess>,
    process_timeout: Duration,
    activity: Arc<tokio::sync::Notify>,
) {
    let managed = Arc::downgrade(&managed);
    tokio::spawn(async move {
        tokio::time::sleep(process_timeout).await;
        let Some(managed) = managed.upgrade() else {
            return;
        };
        let terminate = {
            let mut state = managed.state.lock().await;
            if state.closed {
                false
            } else {
                state.timed_out = true;
                true
            }
        };
        if terminate {
            managed.notify.notify_one();
            activity.notify_one();
            let _ = managed.process.terminate().await;
        }
    });
}

fn event_seq(event: &ExecProcessEvent) -> Option<u64> {
    match event {
        ExecProcessEvent::Output(chunk) => Some(chunk.seq),
        ExecProcessEvent::Exited { seq, .. } | ExecProcessEvent::Closed { seq } => Some(*seq),
        ExecProcessEvent::Failed(_) => None,
    }
}

fn validate_poll_output_ceiling(max_output_bytes: usize) -> Result<(), String> {
    if max_output_bytes == 0 {
        Err("persistent coder process output ceiling must be positive".to_string())
    } else {
        Ok(())
    }
}

pub fn resolve_persistent_process_program(
    argv: &mut [String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    let Some(program) = argv.first_mut() else {
        return Err("persistent coder process argv must not be empty".to_string());
    };
    if program.contains('/') || program.contains('\\') {
        return Ok(());
    }
    let path = env
        .get("PATH")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("persistent coder executable '{program}' requires PATH"))?;
    let resolved = std::env::split_paths(path)
        .map(|entry| {
            if entry.as_os_str().is_empty() {
                cwd.join(program.as_str())
            } else if entry.is_absolute() {
                entry.join(program.as_str())
            } else {
                cwd.join(entry).join(program.as_str())
            }
        })
        .find(|candidate| executable_file(candidate))
        .ok_or_else(|| format!("persistent coder executable '{program}' was not found in PATH"))?;
    *program = resolved.to_string_lossy().into_owned();
    Ok(())
}

#[cfg(unix)]
fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_exec::{
        ByteChunk, ExecProcessFuture, ProcessOutputChunk, ReadResponse, SignalResponse,
        TerminateResponse, WriteResponse,
    };

    struct NeverClosingProcess {
        process_id: ProcessId,
        wake: tokio::sync::watch::Sender<u64>,
    }

    impl ExecProcess for NeverClosingProcess {
        fn process_id(&self) -> &ProcessId {
            &self.process_id
        }

        fn subscribe_wake(&self) -> tokio::sync::watch::Receiver<u64> {
            self.wake.subscribe()
        }

        fn subscribe_events(&self) -> ExecProcessEventReceiver {
            ExecProcessEventReceiver::empty()
        }

        fn read(
            &self,
            _after_seq: Option<u64>,
            _max_bytes: Option<usize>,
            _wait_ms: Option<u64>,
        ) -> ExecProcessFuture<'_, ReadResponse> {
            Box::pin(async {
                Ok(ReadResponse {
                    chunks: Vec::new(),
                    next_seq: 1,
                    exited: false,
                    exit_code: None,
                    closed: false,
                    failure: None,
                })
            })
        }

        fn write(&self, _chunk: Vec<u8>) -> ExecProcessFuture<'_, WriteResponse> {
            Box::pin(async {
                Ok(WriteResponse {
                    status: WriteStatus::Accepted,
                })
            })
        }

        fn signal(&self, _signal: ProcessSignal) -> ExecProcessFuture<'_, SignalResponse> {
            Box::pin(async { Ok(SignalResponse { delivered: true }) })
        }

        fn terminate(&self) -> ExecProcessFuture<'_, TerminateResponse> {
            Box::pin(async { Ok(TerminateResponse { running: true }) })
        }
    }

    #[test]
    fn collector_orders_output_exit_late_output_and_close_in_one_sequence() {
        let mut state = ManagedProcessState::new(16, 16);
        assert!(!state.record(ExecProcessEvent::Output(ProcessOutputChunk {
            seq: 1,
            stream: ExecOutputStream::Stdout,
            chunk: ByteChunk(b"ab".to_vec()),
        })));
        assert!(!state.record(ExecProcessEvent::Exited {
            seq: 2,
            exit_code: 7,
        }));
        assert!(!state.record(ExecProcessEvent::Output(ProcessOutputChunk {
            seq: 3,
            stream: ExecOutputStream::Stderr,
            chunk: ByteChunk(b"cd".to_vec()),
        })));
        assert!(!state.record(ExecProcessEvent::Closed { seq: 4 }));
        assert_eq!(state.failure, None);
        assert_eq!(state.next_expected_seq, 5);
        assert_eq!(state.exit_code, Some(7));
        assert!(state.closed);
    }

    #[test]
    fn closed_process_keeps_unread_tail_until_bounded_polls_drain_it() {
        let mut state = ManagedProcessState::new(16, 16);
        state.record(ExecProcessEvent::Output(ProcessOutputChunk {
            seq: 1,
            stream: ExecOutputStream::Stdout,
            chunk: ByteChunk(b"abcdef".to_vec()),
        }));
        state.record(ExecProcessEvent::Exited {
            seq: 2,
            exit_code: 0,
        });
        state.record(ExecProcessEvent::Closed { seq: 3 });
        let first = state.drain(2);
        assert_eq!(first[0].bytes, b"ab");
        assert!(!first[0].end_of_event);
        assert_eq!(state.pending_bytes, 4);
        let second = state.drain(4);
        assert_eq!(second[0].byte_offset, 2);
        assert_eq!(second[0].bytes, b"cdef");
        assert!(second[0].end_of_event);
        assert_eq!(state.pending_bytes, 0);
    }

    #[test]
    fn gap_and_unread_buffer_overflow_are_explicit_failures() {
        let mut gap = ManagedProcessState::new(8, 8);
        assert!(gap.record(ExecProcessEvent::Exited {
            seq: 2,
            exit_code: 0,
        }));
        assert!(gap.failure.unwrap().contains("expected seq 1"));

        let mut overflow = ManagedProcessState::new(3, 8);
        assert!(
            overflow.record(ExecProcessEvent::Output(ProcessOutputChunk {
                seq: 1,
                stream: ExecOutputStream::Stdout,
                chunk: ByteChunk(b"abcde".to_vec()),
            }))
        );
        assert_eq!(overflow.pending_bytes, 3);
        assert!(overflow.failure.unwrap().contains("unread buffer ceiling"));

        let mut event_overflow = ManagedProcessState::new(8, 1);
        event_overflow.record(ExecProcessEvent::Output(ProcessOutputChunk {
            seq: 1,
            stream: ExecOutputStream::Stdout,
            chunk: ByteChunk(b"a".to_vec()),
        }));
        assert!(
            event_overflow.record(ExecProcessEvent::Output(ProcessOutputChunk {
                seq: 2,
                stream: ExecOutputStream::Stdout,
                chunk: ByteChunk(b"b".to_vec()),
            }))
        );
        assert!(event_overflow.failure.unwrap().contains("event ceiling"));
    }

    #[tokio::test]
    async fn release_timeout_retains_process_ownership_without_fake_closure() {
        let manager = GitWorktreeProcessManager::new();
        let worker_environment_id = WorkerEnvironmentId::derive(b"release-timeout-test");
        let (wake, _) = tokio::sync::watch::channel(0);
        let managed = Arc::new(ManagedProcess {
            process: Arc::new(NeverClosingProcess {
                process_id: ProcessId::new("never-closes"),
                wake,
            }),
            state: tokio::sync::Mutex::new(ManagedProcessState::new(16, 16)),
            poll_lock: tokio::sync::Mutex::new(()),
            notify: tokio::sync::Notify::new(),
            tty: false,
            cwd: AbsPath::new(Path::new("/")).unwrap(),
            max_input_bytes: 16,
        });
        manager.processes.lock().await.by_environment.insert(
            worker_environment_id.clone(),
            ProcessEnvironment {
                processes: BTreeMap::from([("opaque".to_string(), managed)]),
                starts_in_flight: 0,
                releasing: false,
                max_processes: Some(1),
                cleanup_timeout: Some(Duration::from_millis(10)),
            },
        );
        let error = manager
            .release(&worker_environment_id, None)
            .await
            .expect_err("closure without a Closed event must fail");
        assert!(error.contains("closure was not proven"));
        assert!(
            manager
                .processes
                .lock()
                .await
                .by_environment
                .contains_key(&worker_environment_id)
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn manager_continues_one_sandboxed_process_and_closes_with_ordered_output() {
        for mode in [ProcessSandboxMode::Enforced, ProcessSandboxMode::Disabled] {
            exercise_process_continuation(mode).await;
        }
    }

    #[cfg(target_os = "macos")]
    async fn exercise_process_continuation(sandbox_mode: ProcessSandboxMode) {
        let root = tempfile::tempdir().unwrap();
        let cwd = AbsPath::new(root.path()).unwrap();
        let sandbox = ProjectedSandbox::from_parts(
            vec![cwd.clone()],
            Vec::new(),
            vec![AbsPath::new(Path::new("/")).unwrap()],
            octopus_worker_env::NetworkCeiling::Denied,
        );
        let worker_environment_id = WorkerEnvironmentId::derive(b"persistent-manager-test");
        let manager = GitWorktreeProcessManager::new();
        let limits = GitWorktreeProcessLimits {
            max_processes_per_worker_environment: 2,
            max_buffered_output_bytes: 1024,
            max_buffered_output_events: 128,
            max_input_bytes: 128,
            process_timeout: Duration::from_secs(5),
            cleanup_timeout: Duration::from_secs(2),
        };
        let first = manager
            .start(
                &worker_environment_id,
                &sandbox,
                sandbox_mode,
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "printf ready; IFS= read line; printf %s \"$line\"".to_string(),
                ],
                cwd.as_path().to_path_buf(),
                BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
                false,
                100,
                1024,
                limits,
            )
            .await
            .unwrap();
        assert!(!first.process_id.contains(worker_environment_id.as_str()));
        let process_id = first.process_id.clone();
        let listed = manager.list(&worker_environment_id).await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].process_id, process_id);
        assert!(listed[0].running);
        let second = manager
            .write_and_poll(&worker_environment_id, &process_id, b"done\n", 2000, 1024)
            .await
            .unwrap();
        let bytes = first
            .chunks
            .iter()
            .chain(second.chunks.iter())
            .flat_map(|chunk| chunk.bytes.iter().copied())
            .collect::<Vec<_>>();
        assert_eq!(bytes, b"readydone");
        assert_eq!(second.exit_code, Some(0));
        assert!(second.closed);
        assert!(!second.continuation_available);

        let running = manager
            .start(
                &worker_environment_id,
                &sandbox,
                sandbox_mode,
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "printf running; sleep 30".to_string(),
                ],
                cwd.as_path().to_path_buf(),
                BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
                false,
                100,
                1024,
                GitWorktreeProcessLimits {
                    max_processes_per_worker_environment: 2,
                    max_buffered_output_bytes: 1024,
                    max_buffered_output_events: 128,
                    max_input_bytes: 128,
                    process_timeout: Duration::from_secs(35),
                    cleanup_timeout: Duration::from_secs(2),
                },
            )
            .await
            .unwrap();
        let terminated = manager
            .terminate_and_poll(&worker_environment_id, &running.process_id, 2000, 1024)
            .await
            .unwrap();
        assert!(terminated.exited);
        assert!(terminated.closed);
        assert!(!terminated.continuation_available);
        manager.release(&worker_environment_id, None).await.unwrap();
    }
}
