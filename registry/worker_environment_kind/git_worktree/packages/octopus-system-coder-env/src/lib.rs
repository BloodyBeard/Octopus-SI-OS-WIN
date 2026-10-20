use octopus_descriptors::ProcessSandboxMode;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use octopus_descriptors::InterfaceId;
use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceBoundaryError as InterfaceDriverError,
    InterfaceInvocationEnvelope,
};
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo, WorkerEnvironmentReleaseContext, WorkerEnvironmentReleaseMode,
};
use octopus_tool_handlers::{
    ToolCall, ToolCallArtifact, ToolHandler, ToolHandlerContext, ToolHandlerError,
    ToolHandlerResult, ToolResultArtifact, ToolResultError,
};
use octopus_tool_registry::ToolDescriptor;
use octopus_worker_env::{
    AbsPath, EnvContext, ProjectedSandbox, SandboxedCommandRunner, WorkerEnvError,
    WorkerEnvKindSpec, WorkerEnvironment, WorkerEnvironmentFormation, WorkerEnvironmentId,
    WorkerEnvironmentLifecycle,
};

mod coder_exec;
mod coder_fs;
#[cfg(test)]
mod tests;
pub use coder_exec::CoderExecHandler;
pub use coder_fs::CoderFsHandler;

const REPO_ROOT_SLOT: &str = "repo_root";
const WORKTREE_ROOT_SLOT: &str = "worktree_root";
const WORKSPACE_ROOT_SLOT: &str = "workspace_root";
const GIT_COMMON_ROOT_SLOT: &str = "git_common_root";
const INTERFACE_LOCAL_IPC_ROOT_SLOT: &str = "interface_local_ipc_root";
const NEUTRAL_TOOL_CALL_LABEL: &str = "tool.call";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CoderOperation {
    WriteFile,
    ApplyPatch,
    ReadFile,
    ListDir,
    Grep,
    RunCommand,
    StartCommand,
    WriteStdin,
    ProcessSignal,
    ProcessTerminate,
    ProcessList,
    WorktreeStatus,
    WorktreeDiff,
    WorktreeSnapshot,
    WorktreeAbandon,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CoderHandlerConfig {
    pub(crate) operation: CoderOperation,
    #[serde(default)]
    max_diff_bytes: Option<usize>,
    #[serde(default)]
    max_changed_paths: Option<usize>,
    #[serde(default)]
    pub(crate) default_timeout_ms: Option<u64>,
    #[serde(default)]
    pub(crate) max_timeout_ms: Option<u64>,
    #[serde(default)]
    pub(crate) default_max_output_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) max_output_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) default_yield_time_ms: Option<u64>,
    #[serde(default)]
    pub(crate) max_yield_time_ms: Option<u64>,
    #[serde(default)]
    pub(crate) max_buffered_output_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) max_buffered_output_events: Option<usize>,
    #[serde(default)]
    pub(crate) max_processes_per_worker_environment: Option<usize>,
    #[serde(default)]
    pub(crate) max_input_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) cleanup_timeout_ms: Option<u64>,
    #[serde(default)]
    pub(crate) default_shell: Option<String>,
    #[serde(default)]
    pub(crate) allowed_shells: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) allow_login_shell: Option<bool>,
    #[serde(default)]
    pub(crate) max_scanned_files: Option<usize>,
    #[serde(default)]
    pub(crate) max_scanned_bytes: Option<u64>,
    #[serde(default)]
    pub(crate) deadline_ms: Option<u64>,
    #[serde(default)]
    pub(crate) max_line_bytes: Option<usize>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct CoderInvocationScope {
    run: String,
    ee: String,
    ee_root_slot_allowance: Option<std::collections::BTreeSet<String>>,
}

#[derive(Clone, Debug)]
struct DecodedCoderRequest {
    scope: CoderInvocationScope,
    call: ToolCall,
    correlation_id: Option<String>,
}

pub struct SystemCoderEnv {
    interface: InterfaceId,
    room: CoderRoom,
    handler: Arc<dyn ToolHandler>,
    process_manager: Arc<octopus_git_sandbox::GitWorktreeProcessManager>,
    floor_runner: Arc<FloorRunner>,
    rooms: tokio::sync::Mutex<BTreeMap<WorkerEnvironmentId, WorkerEnvironment>>,
}

enum CoderRoom {
    Git {
        repo_root: PathBuf,
        worktree_root: PathBuf,
        git_common_root: PathBuf,
        worktree_state_root: PathBuf,
    },
    Direct {
        workspace_root: PathBuf,
    },
}

struct CoderToolFailure(ToolResultError);

impl From<String> for CoderToolFailure {
    fn from(message: String) -> Self {
        Self(ToolResultError::new("tool_execution_failed", message))
    }
}

impl From<ToolHandlerError> for CoderToolFailure {
    fn from(error: ToolHandlerError) -> Self {
        Self(error.into_tool_result_error())
    }
}

impl SystemCoderEnv {
    pub fn load(
        interface: InterfaceId,
        repo_root: PathBuf,
        worktree_root: PathBuf,
        git_common_root: PathBuf,
        worktree_state_root: PathBuf,
        handler: Arc<dyn ToolHandler>,
        process_store: Option<octopus_git_sandbox::GitWorktreeProcessStore>,
    ) -> Result<Self, String> {
        let repo_root = AbsPath::new(&repo_root)
            .map_err(|error| format!("system_coder_env repo root failed closed: {error}"))?
            .as_path()
            .to_path_buf();
        let worktree_root = AbsPath::new(&worktree_root)
            .map_err(|error| format!("system_coder_env worktree root failed closed: {error}"))?
            .as_path()
            .to_path_buf();
        let git_common_root = AbsPath::new(&git_common_root)
            .map_err(|error| format!("system_coder_env git common root failed closed: {error}"))?
            .as_path()
            .to_path_buf();
        let worktree_state_root = AbsPath::new(&worktree_state_root)
            .map_err(|error| {
                format!("system_coder_env worktree state root failed closed: {error}")
            })?
            .as_path()
            .to_path_buf();
        Ok(Self {
            interface,
            room: CoderRoom::Git {
                repo_root,
                worktree_root,
                git_common_root,
                worktree_state_root,
            },
            handler,
            process_manager: Arc::new(octopus_git_sandbox::GitWorktreeProcessManager::new(
                process_store,
            )),
            floor_runner: Arc::new(FloorRunner::default()),
            rooms: tokio::sync::Mutex::new(BTreeMap::new()),
        })
    }

    pub fn load_direct(
        interface: InterfaceId,
        workspace_root: PathBuf,
        handler: Arc<dyn ToolHandler>,
        process_store: Option<octopus_git_sandbox::GitWorktreeProcessStore>,
    ) -> Result<Self, String> {
        let workspace_root = AbsPath::new(&workspace_root)
            .map_err(|error| format!("system_workspace_env root failed closed: {error}"))?
            .as_path()
            .to_path_buf();
        Ok(Self {
            interface,
            room: CoderRoom::Direct { workspace_root },
            handler,
            process_manager: Arc::new(octopus_git_sandbox::GitWorktreeProcessManager::new(
                process_store,
            )),
            floor_runner: Arc::new(FloorRunner::default()),
            rooms: tokio::sync::Mutex::new(BTreeMap::new()),
        })
    }

    pub fn load_from_process_environment(
        env: &ToolProcessEnvironment,
        handler: Arc<dyn ToolHandler>,
    ) -> Result<Self, String> {
        let worktree_state_root = env.durable_var_dir.clone().ok_or_else(|| {
            "system_coder_env requires formation-scoped durable process state".to_string()
        })?;
        Self::load(
            env.interface.clone(),
            env.root_slot(REPO_ROOT_SLOT)?.to_path_buf(),
            env.root_slot(WORKTREE_ROOT_SLOT)?.to_path_buf(),
            env.root_slot(GIT_COMMON_ROOT_SLOT)?.to_path_buf(),
            worktree_state_root,
            handler,
            process_store(env)?,
        )
    }

    pub fn load_direct_from_process_environment(
        env: &ToolProcessEnvironment,
        handler: Arc<dyn ToolHandler>,
    ) -> Result<Self, String> {
        Self::load_direct(
            env.interface.clone(),
            env.root_slot(WORKSPACE_ROOT_SLOT)?.to_path_buf(),
            handler,
            process_store(env)?,
        )
    }

    async fn receive(
        &self,
        descriptor: ToolDescriptor,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        formation: WorkerEnvironmentFormation,
        kind: WorkerEnvKindSpec,
        io: &ToolProcessIo,
    ) -> Result<InterfaceArtifactFile, InterfaceDriverError> {
        if descriptor.interface != self.interface {
            return Err(InterfaceDriverError::Protocol(format!(
                "tool descriptor '{}' belongs to interface '{}', not process interface '{}'",
                descriptor.id, descriptor.interface, self.interface
            )));
        }
        let manifest = input.read_artifact_manifest()?;
        let payload = input.read_artifact_payload()?;
        let decoded = decode_coder_request(&manifest.label, &payload, &input)?;
        if descriptor.id.as_str() != decoded.call.tool_id {
            return Err(InterfaceDriverError::Protocol(format!(
                "tool descriptor '{}' does not match tool.call '{}'",
                descriptor.id, decoded.call.tool_id
            )));
        }
        let tool_id = decoded.call.tool_id.clone();
        let correlation_id = decoded.correlation_id;
        let response = match self
            .execute_tool_call(
                decoded.scope,
                decoded.call,
                &format!("movement:{source_movement_seq}"),
                &descriptor,
                &formation,
                &kind,
                io,
            )
            .await
        {
            Ok(result) => ToolResultArtifact::success(&tool_id, correlation_id, result.body),
            Err(error) => ToolResultArtifact::failure(&tool_id, correlation_id, error.0),
        };
        let bytes = serde_json::to_vec(&response).map_err(|error| {
            InterfaceDriverError::Protocol(format!(
                "system_coder_env response artifact did not serialize: {error}"
            ))
        })?;
        materialize_system_coder_emitted_artifact(&input, &response.label, &bytes)
    }

    async fn execute_tool_call(
        &self,
        scope: CoderInvocationScope,
        mut call: ToolCall,
        call_identity: &str,
        descriptor: &ToolDescriptor,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        io: &ToolProcessIo,
    ) -> Result<ToolHandlerResult, CoderToolFailure> {
        call.handler_config = descriptor.handler_config.clone();
        let handler_config = coder_handler_config(&call)?;
        let room_root = self.room_root(formation)?;
        if handler_config.operation == CoderOperation::WorktreeAbandon {
            let CoderRoom::Git {
                repo_root,
                worktree_state_root,
                ..
            } = &self.room
            else {
                return Err(format!("tool '{}' requires a Git worktree room", call.tool_id).into());
            };
            let cleanup_timeout = handler_config.cleanup_timeout_ms.ok_or_else(|| {
                format!(
                    "tool '{}' worktree abandon operation is missing cleanup_timeout_ms",
                    call.tool_id
                )
            })?;
            self.process_manager
                .release(
                    &formation.worker_environment_id,
                    Some(std::time::Duration::from_millis(cleanup_timeout)),
                )
                .await?;
            let limits = worktree_evidence_limits(&call.tool_id, &handler_config)?;
            let evidence = octopus_git_sandbox::abandon_git_worktree(
                repo_root,
                worktree_state_root,
                &room_root,
                formation.worker_environment_id.as_str(),
                limits,
            )
            .map_err(|error| error.to_string())?;
            self.rooms
                .lock()
                .await
                .remove(&formation.worker_environment_id);
            io.record_worker_environment_lifecycle(formation, WorkerEnvironmentLifecycle::Released)
                .await?;
            return Ok(ToolHandlerResult::new(serde_json::json!({
                "result": "worktree_abandoned",
                "worker_environment_id": formation.worker_environment_id.as_str(),
                "evidence": worktree_evidence_json(evidence),
                "branch_deleted": true,
                "worktree_deleted": true
            })));
        }
        if !self
            .rooms
            .lock()
            .await
            .contains_key(&formation.worker_environment_id)
        {
            let env = match self.lease(&scope, formation, kind).await {
                Ok(env) => env,
                Err(error) => {
                    io.record_worker_environment_lifecycle(
                        formation,
                        WorkerEnvironmentLifecycle::Failed,
                    )
                    .await?;
                    return Err(error.to_string().into());
                }
            };
            self.rooms
                .lock()
                .await
                .insert(formation.worker_environment_id.clone(), env);
        }
        io.record_worker_environment_lifecycle(formation, WorkerEnvironmentLifecycle::Active)
            .await?;
        let (leased_kind, sandbox, fs, exec) = {
            let rooms = self.rooms.lock().await;
            let env = rooms.get(&formation.worker_environment_id).ok_or_else(|| {
                format!(
                    "worker environment '{}' disappeared after lease",
                    formation.worker_environment_id
                )
            })?;
            (
                env.kind_id().clone(),
                env.sandbox().clone(),
                env.fs(),
                env.exec_with(Arc::new(FloorExecution {
                    owner: self.floor_runner.clone(),
                    sandbox_mode: descriptor.process.authority.sandbox_mode,
                })),
            )
        };
        validate_tool_code_world(descriptor, &leased_kind)?;
        let ctx = ToolHandlerContext {
            run: scope.run.clone(),
            room_root: room_root.clone(),
            sandbox: sandbox.clone(),
            fs,
            exec,
            artifacts: None,
        };
        let result = match handler_config.operation {
            CoderOperation::WorktreeStatus
            | CoderOperation::WorktreeDiff
            | CoderOperation::WorktreeSnapshot => {
                if !matches!(&self.room, CoderRoom::Git { .. }) {
                    return Err(
                        format!("tool '{}' requires a Git worktree room", call.tool_id).into(),
                    );
                }
                let limits = worktree_evidence_limits(&call.tool_id, &handler_config)?;
                let evidence =
                    octopus_git_sandbox::capture_git_worktree_evidence(&room_root, limits)
                        .map_err(|error| error.to_string())?;
                match handler_config.operation {
                    CoderOperation::WorktreeStatus => ToolHandlerResult::new(serde_json::json!({
                        "result": "worktree_status",
                        "worker_environment_id": formation.worker_environment_id.as_str(),
                        "branch": evidence.branch,
                        "head_commit": evidence.head_commit,
                        "changed_paths": evidence.changed_paths,
                        "clean": evidence.clean,
                    })),
                    CoderOperation::WorktreeDiff => ToolHandlerResult::new(serde_json::json!({
                        "result": "worktree_diff",
                        "worker_environment_id": formation.worker_environment_id.as_str(),
                        "diff_base": evidence.diff_base,
                        "unified_diff": evidence.unified_diff,
                        "diff_blake3": evidence.diff_blake3,
                        "changed_paths": evidence.changed_paths,
                        "line_count": evidence.line_count,
                        "clean": evidence.clean,
                    })),
                    _ => ToolHandlerResult::new(serde_json::json!({
                        "result": "worktree_snapshot",
                        "worker_environment_id": formation.worker_environment_id.as_str(),
                        "evidence": worktree_evidence_json(evidence)
                    })),
                }
            }
            CoderOperation::StartCommand => coder_exec::start_command(
                &ctx,
                &call,
                call_identity,
                &descriptor.process.inherit_env,
                &handler_config,
                &self.process_manager,
                &formation.worker_environment_id,
                &sandbox,
                descriptor.process.authority.sandbox_mode,
            )
            .await
            .map_err(CoderToolFailure::from)?,
            CoderOperation::ProcessSignal => coder_exec::process_signal(
                &ctx,
                &call,
                call_identity,
                &handler_config,
                &self.process_manager,
                &formation.worker_environment_id,
            )
            .await
            .map_err(CoderToolFailure::from)?,
            CoderOperation::ProcessTerminate => coder_exec::process_terminate(
                &ctx,
                &call,
                call_identity,
                &handler_config,
                &self.process_manager,
                &formation.worker_environment_id,
            )
            .await
            .map_err(CoderToolFailure::from)?,
            CoderOperation::ProcessList => coder_exec::process_list(
                &ctx,
                &call,
                &self.process_manager,
                &formation.worker_environment_id,
            )
            .await
            .map_err(CoderToolFailure::from)?,
            CoderOperation::WriteStdin => coder_exec::write_stdin(
                &ctx,
                &call,
                call_identity,
                &handler_config,
                &self.process_manager,
                &formation.worker_environment_id,
            )
            .await
            .map_err(CoderToolFailure::from)?,
            _ => self
                .handler
                .execute(&ctx, &call)
                .await
                .map_err(CoderToolFailure::from)?,
        };
        Ok(result)
    }

    async fn lease(
        &self,
        scope: &CoderInvocationScope,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
    ) -> Result<octopus_worker_env::WorkerEnvironment, WorkerEnvError> {
        let (mut root_slots, driver) = match &self.room {
            CoderRoom::Git {
                repo_root,
                worktree_root,
                git_common_root,
                worktree_state_root,
            } => (
                BTreeMap::from([
                    (REPO_ROOT_SLOT.to_string(), AbsPath::new(repo_root)?),
                    (WORKTREE_ROOT_SLOT.to_string(), AbsPath::new(worktree_root)?),
                    (
                        GIT_COMMON_ROOT_SLOT.to_string(),
                        AbsPath::new(git_common_root)?,
                    ),
                ]),
                octopus_git_sandbox::process_exec_driver_with_state_root(worktree_state_root)
                    .map_err(|error| WorkerEnvError::OpenFailed {
                        substrate_family: kind.substrate_family.clone(),
                        reason: error.to_string(),
                    })?,
            ),
            CoderRoom::Direct { workspace_root } => (
                BTreeMap::from([(
                    WORKSPACE_ROOT_SLOT.to_string(),
                    AbsPath::new(workspace_root)?,
                )]),
                octopus_git_sandbox::direct_workspace_driver(),
            ),
        };
        for (slot, path) in &formation.door_root_bindings {
            if root_slots.contains_key(slot) {
                continue;
            }
            root_slots.insert(slot.clone(), AbsPath::new(path)?);
        }
        let ctx = EnvContext {
            root_slots,
            env: BTreeMap::new(),
        };
        octopus_worker_env::lease(
            kind,
            formation,
            driver,
            ctx,
            scope.ee_root_slot_allowance.as_ref(),
        )
        .await
    }

    fn room_root(&self, formation: &WorkerEnvironmentFormation) -> Result<PathBuf, String> {
        match &self.room {
            CoderRoom::Git { worktree_root, .. } => octopus_git_sandbox::instance_worktree_path(
                worktree_root,
                &formation.worker_environment_id,
            )
            .map_err(|error| error.to_string()),
            CoderRoom::Direct { workspace_root } => Ok(workspace_root.clone()),
        }
    }
}

fn validate_tool_code_world(
    descriptor: &ToolDescriptor,
    leased_kind: &octopus_descriptors::WorkerEnvironmentKindId,
) -> Result<(), String> {
    match descriptor.supported_worker_environment_kinds.as_slice() {
        [kind] if kind == leased_kind => Ok(()),
        _ => Err(format!(
            "tool '{}' cannot execute in worker_environment_kind '{}' because its code-world ownership is not exact",
            descriptor.id,
            leased_kind.as_str()
        )),
    }
}

impl ToolProcessHandler for SystemCoderEnv {
    fn execute<'a>(
        &'a self,
        invocation: ToolProcessInvocation,
        io: ToolProcessIo,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<InterfaceArtifactFile>, ToolProcessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let formation = invocation.worker_environment.ok_or_else(|| {
                ToolProcessError::Capability(
                    "system_coder_env invocation has no formed worker environment".to_string(),
                )
            })?;
            let kind = invocation.worker_environment_kind.ok_or_else(|| {
                ToolProcessError::Capability(
                    "system_coder_env invocation has no formed kind descriptor".to_string(),
                )
            })?;
            self.receive(
                invocation.descriptor,
                invocation.envelope,
                invocation.source_movement_seq,
                formation,
                kind,
                &io,
            )
            .await
            .map(|artifact| vec![artifact])
            .map_err(|error| ToolProcessError::Capability(error.to_string()))
        })
    }

    fn release<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        _kind: &'a WorkerEnvKindSpec,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), ToolProcessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.process_manager
                .release(&formation.worker_environment_id, None)
                .await
                .map_err(ToolProcessError::Unavailable)?;
            let env = self
                .rooms
                .lock()
                .await
                .remove(&formation.worker_environment_id);
            match env {
                Some(env) => env
                    .release()
                    .await
                    .map_err(|error| ToolProcessError::Unavailable(error.to_string())),
                None => match &self.room {
                    CoderRoom::Git {
                        repo_root,
                        worktree_root,
                        worktree_state_root,
                        ..
                    } => octopus_git_sandbox::release_git_worktree(
                        repo_root,
                        worktree_state_root,
                        &octopus_git_sandbox::instance_worktree_path(
                            worktree_root,
                            &formation.worker_environment_id,
                        )
                        .map_err(|error| ToolProcessError::Unavailable(error.to_string()))?,
                        formation.worker_environment_id.as_str(),
                    )
                    .map_err(|error| ToolProcessError::Unavailable(error.to_string())),
                    CoderRoom::Direct { .. } => Ok(()),
                },
            }
        })
    }

    fn release_with_context<'a>(
        &'a self,
        context: WorkerEnvironmentReleaseContext<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), ToolProcessError>> + Send + 'a>,
    > {
        match context.mode {
            WorkerEnvironmentReleaseMode::LiveOwner
            | WorkerEnvironmentReleaseMode::ColdRecovery => {
                self.release(context.formation, context.kind)
            }
        }
    }

    fn quiesce<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), ToolProcessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut errors = Vec::new();
            if let Err(error) = self.floor_runner.quiesce().await {
                errors.push(error);
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(ToolProcessError::Unavailable(errors.join("; ")))
            }
        })
    }
}

fn process_store(
    env: &ToolProcessEnvironment,
) -> Result<Option<octopus_git_sandbox::GitWorktreeProcessStore>, String> {
    let (Some(durable), Some(sockets)) = (
        env.durable_var_dir.as_deref(),
        env.root_slots.get(INTERFACE_LOCAL_IPC_ROOT_SLOT),
    ) else {
        return Ok(None);
    };
    let keeper = std::env::current_exe()
        .map_err(|error| format!("persistent process keeper executable is unavailable: {error}"))?;
    octopus_git_sandbox::GitWorktreeProcessStore::new(durable, sockets, keeper).map(Some)
}

async fn run_coder_tool_process(handler: Arc<dyn ToolHandler>) -> Result<(), String> {
    if let Some(spec) = octopus_git_sandbox::persistent_process_keeper_spec(std::env::args_os()) {
        return octopus_git_sandbox::run_persistent_process_keeper(spec).await;
    }
    let env = ToolProcessEnvironment::from_env()?;
    let process = Arc::new(SystemCoderEnv::load_from_process_environment(
        &env, handler,
    )?);
    octopus_tool_door_runtime::run_tool_process(process).await
}

async fn run_workspace_tool_process(handler: Arc<dyn ToolHandler>) -> Result<(), String> {
    if let Some(spec) = octopus_git_sandbox::persistent_process_keeper_spec(std::env::args_os()) {
        return octopus_git_sandbox::run_persistent_process_keeper(spec).await;
    }
    let env = ToolProcessEnvironment::from_env()?;
    let process = Arc::new(SystemCoderEnv::load_direct_from_process_environment(
        &env, handler,
    )?);
    octopus_tool_door_runtime::run_tool_process(process).await
}

pub async fn run_coder_fs_tool_process() -> Result<(), String> {
    run_coder_tool_process(Arc::new(CoderFsHandler)).await
}

pub async fn run_coder_exec_tool_process() -> Result<(), String> {
    run_coder_tool_process(Arc::new(CoderExecHandler)).await
}

pub async fn run_workspace_fs_tool_process() -> Result<(), String> {
    run_workspace_tool_process(Arc::new(CoderFsHandler)).await
}

pub async fn run_workspace_exec_tool_process() -> Result<(), String> {
    run_workspace_tool_process(Arc::new(CoderExecHandler)).await
}

fn invocation_scope_from_input(
    input: &InterfaceInvocationEnvelope,
) -> Result<CoderInvocationScope, InterfaceDriverError> {
    let runtime = input
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.runtime.as_ref())
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "system_coder_env requires runtime metadata beside the artifact".into(),
            )
        })?;
    let ee_root_slot_allowance = input
        .metadata()
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?
        .grant()
        .ee_root_slot_allowance_set();
    Ok(CoderInvocationScope {
        run: runtime.run.as_str().to_string(),
        ee: runtime.ee.as_str().to_string(),
        ee_root_slot_allowance,
    })
}

fn decode_coder_request(
    manifest_label: &str,
    payload: &[u8],
    input: &InterfaceInvocationEnvelope,
) -> Result<DecodedCoderRequest, InterfaceDriverError> {
    if manifest_label != NEUTRAL_TOOL_CALL_LABEL {
        return Err(InterfaceDriverError::Protocol(format!(
            "system_coder_env manifest label '{}' is not '{}'",
            manifest_label, NEUTRAL_TOOL_CALL_LABEL
        )));
    }
    let artifact: ToolCallArtifact = serde_json::from_slice(payload).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "tool.call artifact is not a neutral tool-call JSON object: {error}"
        ))
    })?;
    let tool_id = admitted_tool_call_tool(input, &artifact)?;
    Ok(DecodedCoderRequest {
        scope: invocation_scope_from_input(input)?,
        correlation_id: artifact.correlation_id,
        call: ToolCall {
            tool_id,
            arguments: artifact.arguments,
            handler_config: serde_json::Value::Null,
        },
    })
}

fn admitted_tool_call_tool(
    input: &InterfaceInvocationEnvelope,
    call: &ToolCallArtifact,
) -> Result<String, InterfaceDriverError> {
    let tool = call.tool.trim();
    if tool.is_empty() {
        return Err(InterfaceDriverError::Protocol(
            "system_coder_env tool.call has empty tool name".into(),
        ));
    }
    if !input
        .granted_tool_names()?
        .iter()
        .any(|grant| grant == tool)
    {
        return Err(InterfaceDriverError::Protocol(format!(
            "system_coder_env denied tool.call '{}' because formed metadata did not grant it",
            tool
        )));
    }
    if input
        .hard_blocked_tool_names()?
        .iter()
        .any(|blocked| blocked == tool)
    {
        return Err(InterfaceDriverError::Protocol(format!(
            "system_coder_env denied hard-blocked tool.call '{}'",
            tool
        )));
    }
    Ok(tool.to_string())
}

pub(crate) fn coder_handler_config(call: &ToolCall) -> Result<CoderHandlerConfig, String> {
    let config: CoderHandlerConfig =
        serde_json::from_value(call.handler_config.clone()).map_err(|error| {
            format!(
                "tool '{}' has malformed coder handler_config: {error}",
                call.tool_id
            )
        })?;
    let has_diff_bytes = config.max_diff_bytes.is_some();
    let has_changed_paths = config.max_changed_paths.is_some();
    let timeout_limits = [
        config.default_timeout_ms.is_some(),
        config.max_timeout_ms.is_some(),
    ];
    let output_limits = [
        config.default_max_output_bytes.is_some(),
        config.max_output_bytes.is_some(),
    ];
    let yield_limits = [
        config.default_yield_time_ms.is_some(),
        config.max_yield_time_ms.is_some(),
    ];
    let start_limits = [
        config.max_buffered_output_bytes.is_some(),
        config.max_buffered_output_events.is_some(),
        config.max_processes_per_worker_environment.is_some(),
        config.cleanup_timeout_ms.is_some(),
    ];
    let has_input_limit = config.max_input_bytes.is_some();
    let grep_limits = [
        config.max_scanned_files.is_some(),
        config.max_scanned_bytes.is_some(),
        config.deadline_ms.is_some(),
        config.max_line_bytes.is_some(),
    ];
    let shell_config = [
        config.default_shell.is_some(),
        config.allowed_shells.is_some(),
        config.allow_login_shell.is_some(),
    ];
    match config.operation {
        CoderOperation::WorktreeStatus
        | CoderOperation::WorktreeDiff
        | CoderOperation::WorktreeSnapshot => {
            if !has_diff_bytes || !has_changed_paths {
                return Err(format!(
                    "tool '{}' worktree operation requires max_diff_bytes and max_changed_paths",
                    call.tool_id
                ));
            }
            if timeout_limits
                .iter()
                .chain(output_limits.iter())
                .chain(yield_limits.iter())
                .chain(start_limits.iter())
                .any(|present| *present)
                || has_input_limit
            {
                return Err(format!(
                    "tool '{}' worktree snapshot operation cannot declare process limits",
                    call.tool_id
                ));
            }
            if grep_limits.iter().any(|present| *present) {
                return Err(format!(
                    "tool '{}' worktree operation cannot declare grep limits",
                    call.tool_id
                ));
            }
            if shell_config.iter().any(|present| *present) {
                return Err(format!(
                    "tool '{}' worktree operation cannot declare shell config",
                    call.tool_id
                ));
            }
        }
        CoderOperation::WorktreeAbandon => {
            if !has_diff_bytes || !has_changed_paths || config.cleanup_timeout_ms.is_none() {
                return Err(format!(
                    "tool '{}' worktree abandon operation requires evidence and cleanup limits",
                    call.tool_id
                ));
            }
            if timeout_limits
                .iter()
                .chain(output_limits.iter())
                .chain(yield_limits.iter())
                .any(|present| *present)
                || config.max_buffered_output_bytes.is_some()
                || config.max_buffered_output_events.is_some()
                || config.max_processes_per_worker_environment.is_some()
                || has_input_limit
            {
                return Err(format!(
                    "tool '{}' worktree abandon operation cannot declare command limits",
                    call.tool_id
                ));
            }
            validate_cleanup_timeout(&call.tool_id, &config)?;
            if grep_limits.iter().any(|present| *present) {
                return Err(format!(
                    "tool '{}' worktree abandon operation cannot declare grep limits",
                    call.tool_id
                ));
            }
            if shell_config.iter().any(|present| *present) {
                return Err(format!(
                    "tool '{}' worktree abandon operation cannot declare shell config",
                    call.tool_id
                ));
            }
        }
        CoderOperation::RunCommand => {
            if has_diff_bytes || has_changed_paths {
                return Err(format!(
                    "tool '{}' command operation cannot declare worktree evidence limits",
                    call.tool_id
                ));
            }
            if !timeout_limits.iter().all(|present| *present)
                || !output_limits.iter().all(|present| *present)
            {
                return Err(format!(
                    "tool '{}' command operation requires complete timeout and output limits",
                    call.tool_id
                ));
            }
            if grep_limits.iter().any(|present| *present) {
                return Err(format!(
                    "tool '{}' command operation cannot declare grep limits",
                    call.tool_id
                ));
            }
            if yield_limits
                .iter()
                .chain(start_limits.iter())
                .any(|present| *present)
                || has_input_limit
            {
                return Err(format!(
                    "tool '{}' one-shot command cannot declare persistent process limits",
                    call.tool_id
                ));
            }
            validate_timeout_limits(&call.tool_id, &config)?;
            validate_output_limits(&call.tool_id, &config)?;
            validate_shell_config(&call.tool_id, &config)?;
        }
        CoderOperation::StartCommand => {
            if has_diff_bytes || has_changed_paths || grep_limits.iter().any(|present| *present) {
                return Err(format!(
                    "tool '{}' persistent command cannot declare worktree or grep limits",
                    call.tool_id
                ));
            }
            if !timeout_limits.iter().all(|present| *present)
                || !output_limits.iter().all(|present| *present)
                || !yield_limits.iter().all(|present| *present)
                || !start_limits.iter().all(|present| *present)
                || !has_input_limit
            {
                return Err(format!(
                    "tool '{}' persistent command requires complete descriptor-owned limits",
                    call.tool_id
                ));
            }
            validate_timeout_limits(&call.tool_id, &config)?;
            validate_output_limits(&call.tool_id, &config)?;
            validate_yield_limits(&call.tool_id, &config)?;
            validate_persistent_start_limits(&call.tool_id, &config)?;
            validate_shell_config(&call.tool_id, &config)?;
        }
        CoderOperation::WriteStdin => {
            if has_diff_bytes
                || has_changed_paths
                || timeout_limits.iter().any(|present| *present)
                || start_limits.iter().any(|present| *present)
                || grep_limits.iter().any(|present| *present)
                || shell_config.iter().any(|present| *present)
            {
                return Err(format!(
                    "tool '{}' stdin continuation can declare only input, yield, and output limits",
                    call.tool_id
                ));
            }
            if !output_limits.iter().all(|present| *present)
                || !yield_limits.iter().all(|present| *present)
                || !has_input_limit
            {
                return Err(format!(
                    "tool '{}' stdin continuation requires complete input, yield, and output limits",
                    call.tool_id
                ));
            }
            validate_output_limits(&call.tool_id, &config)?;
            validate_yield_limits(&call.tool_id, &config)?;
            let max_input = config.max_input_bytes.unwrap_or(0);
            if max_input == 0 || max_input > 1024 * 1024 {
                return Err(format!(
                    "tool '{}' stdin continuation input limit is invalid",
                    call.tool_id
                ));
            }
        }
        CoderOperation::ProcessSignal | CoderOperation::ProcessTerminate => {
            if has_diff_bytes
                || has_changed_paths
                || timeout_limits.iter().any(|present| *present)
                || start_limits.iter().any(|present| *present)
                || grep_limits.iter().any(|present| *present)
                || has_input_limit
                || shell_config.iter().any(|present| *present)
            {
                return Err(format!(
                    "tool '{}' process control can declare only yield and output limits",
                    call.tool_id
                ));
            }
            if !output_limits.iter().all(|present| *present)
                || !yield_limits.iter().all(|present| *present)
            {
                return Err(format!(
                    "tool '{}' process control requires complete yield and output limits",
                    call.tool_id
                ));
            }
            validate_output_limits(&call.tool_id, &config)?;
            validate_yield_limits(&call.tool_id, &config)?;
        }
        CoderOperation::ProcessList => {
            if has_diff_bytes
                || has_changed_paths
                || timeout_limits
                    .iter()
                    .chain(output_limits.iter())
                    .chain(yield_limits.iter())
                    .chain(start_limits.iter())
                    .chain(grep_limits.iter())
                    .chain(shell_config.iter())
                    .any(|present| *present)
                || has_input_limit
            {
                return Err(format!(
                    "tool '{}' process list cannot declare execution limits",
                    call.tool_id
                ));
            }
        }
        CoderOperation::Grep => {
            if has_diff_bytes
                || has_changed_paths
                || timeout_limits
                    .iter()
                    .chain(output_limits.iter())
                    .chain(yield_limits.iter())
                    .chain(start_limits.iter())
                    .any(|present| *present)
                || has_input_limit
                || shell_config.iter().any(|present| *present)
            {
                return Err(format!(
                    "tool '{}' grep operation cannot declare worktree or command limits",
                    call.tool_id
                ));
            }
            if !grep_limits.iter().all(|present| *present) {
                return Err(format!(
                    "tool '{}' grep operation requires complete scan limits",
                    call.tool_id
                ));
            }
            let max_scanned_files = config.max_scanned_files.unwrap_or(0);
            let max_scanned_bytes = config.max_scanned_bytes.unwrap_or(0);
            let deadline_ms = config.deadline_ms.unwrap_or(0);
            let max_line_bytes = config.max_line_bytes.unwrap_or(0);
            if max_scanned_files == 0
                || max_scanned_files > 1_000_000
                || max_scanned_bytes == 0
                || max_scanned_bytes > 4 * 1024 * 1024 * 1024
                || deadline_ms == 0
                || deadline_ms > 120_000
                || max_line_bytes == 0
                || max_line_bytes > 16 * 1024 * 1024
            {
                return Err(format!(
                    "tool '{}' grep scan limits are invalid",
                    call.tool_id
                ));
            }
        }
        _ if has_diff_bytes || has_changed_paths => {
            return Err(format!(
                "tool '{}' non-worktree operation cannot declare worktree evidence limits",
                call.tool_id
            ));
        }
        _ if timeout_limits
            .iter()
            .chain(output_limits.iter())
            .chain(yield_limits.iter())
            .chain(start_limits.iter())
            .any(|present| *present)
            || has_input_limit =>
        {
            return Err(format!(
                "tool '{}' non-process operation cannot declare process limits",
                call.tool_id
            ));
        }
        _ if grep_limits.iter().any(|present| *present) => {
            return Err(format!(
                "tool '{}' non-grep operation cannot declare grep limits",
                call.tool_id
            ));
        }
        _ if shell_config.iter().any(|present| *present) => {
            return Err(format!(
                "tool '{}' non-command operation cannot declare shell config",
                call.tool_id
            ));
        }
        _ => {}
    }
    Ok(config)
}

fn validate_timeout_limits(tool_id: &str, config: &CoderHandlerConfig) -> Result<(), String> {
    let default_timeout = config.default_timeout_ms.unwrap_or(0);
    let max_timeout = config.max_timeout_ms.unwrap_or(0);
    if default_timeout == 0
        || max_timeout == 0
        || default_timeout > max_timeout
        || max_timeout > 1_800_000
    {
        return Err(format!(
            "tool '{tool_id}' command timeout limits are invalid"
        ));
    }
    Ok(())
}

fn validate_output_limits(tool_id: &str, config: &CoderHandlerConfig) -> Result<(), String> {
    let default_output = config.default_max_output_bytes.unwrap_or(0);
    let max_output = config.max_output_bytes.unwrap_or(0);
    if default_output == 0
        || max_output == 0
        || default_output > max_output
        || max_output > 16 * 1024 * 1024
    {
        return Err(format!(
            "tool '{tool_id}' command output limits are invalid"
        ));
    }
    Ok(())
}

fn validate_yield_limits(tool_id: &str, config: &CoderHandlerConfig) -> Result<(), String> {
    let default_yield = config.default_yield_time_ms.unwrap_or(u64::MAX);
    let max_yield = config.max_yield_time_ms.unwrap_or(0);
    if default_yield > max_yield || max_yield > 60_000 {
        return Err(format!("tool '{tool_id}' command yield limits are invalid"));
    }
    Ok(())
}

fn validate_cleanup_timeout(tool_id: &str, config: &CoderHandlerConfig) -> Result<(), String> {
    let cleanup_timeout = config.cleanup_timeout_ms.unwrap_or(0);
    if cleanup_timeout == 0 || cleanup_timeout > 60_000 {
        return Err(format!(
            "tool '{tool_id}' persistent cleanup timeout is invalid"
        ));
    }
    Ok(())
}

fn validate_persistent_start_limits(
    tool_id: &str,
    config: &CoderHandlerConfig,
) -> Result<(), String> {
    let max_buffered_output = config.max_buffered_output_bytes.unwrap_or(0);
    let max_buffered_events = config.max_buffered_output_events.unwrap_or(0);
    let max_processes = config.max_processes_per_worker_environment.unwrap_or(0);
    let max_input = config.max_input_bytes.unwrap_or(0);
    if max_buffered_output == 0 || max_buffered_output > 64 * 1024 * 1024 {
        return Err(format!(
            "tool '{tool_id}' persistent unread output limit is invalid"
        ));
    }
    if max_buffered_events == 0 || max_buffered_events > 1_000_000 {
        return Err(format!(
            "tool '{tool_id}' persistent unread event limit is invalid"
        ));
    }
    if max_processes == 0 || max_processes > 1024 {
        return Err(format!(
            "tool '{tool_id}' persistent process count limit is invalid"
        ));
    }
    if max_input == 0 || max_input > 1024 * 1024 {
        return Err(format!(
            "tool '{tool_id}' persistent input limit is invalid"
        ));
    }
    validate_cleanup_timeout(tool_id, config)
}

fn validate_shell_config(tool_id: &str, config: &CoderHandlerConfig) -> Result<(), String> {
    let default_shell = config
        .default_shell
        .as_deref()
        .filter(|shell| !shell.trim().is_empty())
        .ok_or_else(|| format!("tool '{tool_id}' command requires a non-empty default_shell"))?;
    let allowed_shells = config
        .allowed_shells
        .as_ref()
        .filter(|shells| !shells.is_empty())
        .ok_or_else(|| format!("tool '{tool_id}' command requires allowed_shells"))?;
    if allowed_shells.iter().any(|shell| shell.trim().is_empty()) {
        return Err(format!(
            "tool '{tool_id}' command allowed_shells contains an empty shell"
        ));
    }
    let unique = allowed_shells
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if unique.len() != allowed_shells.len() {
        return Err(format!(
            "tool '{tool_id}' command allowed_shells contains duplicates"
        ));
    }
    if !allowed_shells.iter().any(|shell| shell == default_shell) {
        return Err(format!(
            "tool '{tool_id}' command default_shell is absent from allowed_shells"
        ));
    }
    if config.allow_login_shell.is_none() {
        return Err(format!(
            "tool '{tool_id}' command requires allow_login_shell"
        ));
    }
    Ok(())
}

fn worktree_evidence_limits(
    tool_id: &str,
    config: &CoderHandlerConfig,
) -> Result<octopus_git_sandbox::GitWorktreeEvidenceLimits, String> {
    let max_diff_bytes = config
        .max_diff_bytes
        .ok_or_else(|| format!("tool '{tool_id}' worktree operation is missing max_diff_bytes"))?;
    let max_changed_paths = config.max_changed_paths.ok_or_else(|| {
        format!("tool '{tool_id}' worktree operation is missing max_changed_paths")
    })?;
    octopus_git_sandbox::GitWorktreeEvidenceLimits::new(max_diff_bytes, max_changed_paths)
        .map_err(|error| error.to_string())
}

fn worktree_evidence_json(evidence: octopus_git_sandbox::GitWorktreeEvidence) -> serde_json::Value {
    serde_json::json!({
        "branch": evidence.branch,
        "head_commit": evidence.head_commit,
        "diff_base": evidence.diff_base,
        "unified_diff": evidence.unified_diff,
        "diff_blake3": evidence.diff_blake3,
        "changed_paths": evidence.changed_paths,
        "line_count": evidence.line_count,
        "clean": evidence.clean
    })
}

fn materialize_system_coder_emitted_artifact(
    input: &InterfaceInvocationEnvelope,
    label: &str,
    payload_bytes: &[u8],
) -> Result<InterfaceArtifactFile, InterfaceDriverError> {
    let parent = input
        .artifact_file
        .manifest_path()
        .parent()
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "system_coder_env inbound artifact manifest '{}' has no parent directory",
                input.artifact_file.manifest_path().display()
            ))
        })?;
    InterfaceArtifactFile::write(parent.join("emitted"), label, payload_bytes)
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))
}

#[derive(Default)]
struct FloorRunner {
    jobs: tokio::sync::Mutex<Vec<FloorJob>>,
    closed: std::sync::atomic::AtomicBool,
}

struct CancelCommandOnDrop(Arc<std::sync::atomic::AtomicBool>);

impl Drop for CancelCommandOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

struct FloorJob {
    cancel: CancelCommandOnDrop,
    task: Option<tokio::task::JoinHandle<Result<(), String>>>,
}

impl FloorJob {
    async fn join(mut self) -> Result<(), String> {
        self.task
            .take()
            .expect("owned command job exists")
            .await
            .map_err(|error| format!("sandboxed exec task failed: {error}"))?
    }
}

impl FloorRunner {
    async fn quiesce(&self) -> Result<(), String> {
        let jobs = {
            let mut jobs = self.jobs.lock().await;
            self.closed
                .store(true, std::sync::atomic::Ordering::Release);
            for job in jobs.iter() {
                job.cancel
                    .0
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            std::mem::take(&mut *jobs)
        };
        let mut errors = Vec::new();
        for job in jobs {
            if let Err(error) = job.join().await {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

struct FloorExecution {
    owner: Arc<FloorRunner>,
    sandbox_mode: ProcessSandboxMode,
}

impl SandboxedCommandRunner for FloorExecution {
    fn run_sandboxed(
        &self,
        sandbox: &ProjectedSandbox,
        cwd: &AbsPath,
        argv: Vec<String>,
        timeout: std::time::Duration,
        max_output_bytes: usize,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<octopus_worker_env::ExecOutput, WorkerEnvError>>
                + Send
                + '_,
        >,
    > {
        let sandbox = sandbox.clone();
        let cwd = cwd.clone();
        let sandbox_mode = self.sandbox_mode;
        Box::pin(async move {
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let _cancel_on_drop = CancelCommandOnDrop(Arc::clone(&cancel));
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            let mut jobs = self.owner.jobs.lock().await;
            if self.owner.closed.load(std::sync::atomic::Ordering::Acquire) {
                return Err(WorkerEnvError::ExecFailed(
                    "command owner is quiesced".to_string(),
                ));
            }
            let mut errors = Vec::new();
            for index in (0..jobs.len()).rev() {
                if jobs[index]
                    .task
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
                {
                    if let Err(error) = jobs.swap_remove(index).join().await {
                        errors.push(error);
                    }
                }
            }
            if !errors.is_empty() {
                return Err(WorkerEnvError::ExecFailed(errors.join("; ")));
            }
            let job_cancel = Arc::clone(&cancel);
            let task = tokio::task::spawn_blocking(move || {
                let result = octopus_git_sandbox::run_sandboxed_in_worktree(
                    &sandbox,
                    sandbox_mode,
                    &cwd,
                    argv,
                    timeout,
                    max_output_bytes,
                    &job_cancel,
                );
                match result_tx.send(result) {
                    Err(Err(WorkerEnvError::ExecFailedWithOutput { reason, captured }))
                        if captured.cancelled && captured.cleanup_errors.is_empty() =>
                    {
                        tracing::debug!(%reason, exit_observed = captured.exit_observed, "cancelled command owner job settled");
                        Ok(())
                    }
                    Err(Err(error)) => Err(format!("command result was not delivered: {error}")),
                    _ => Ok(()),
                }
            });
            jobs.push(FloorJob {
                cancel: CancelCommandOnDrop(cancel),
                task: Some(task),
            });
            drop(jobs);
            result_rx.await.map_err(|error| {
                WorkerEnvError::ExecFailed(format!("sandboxed exec result unavailable: {error}"))
            })?
        })
    }
}
