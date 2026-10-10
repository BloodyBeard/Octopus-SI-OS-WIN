use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octopus_descriptors::InterfaceId;
use octopus_interfaces::{InterfaceArtifactFile, InterfaceInvocationEnvelope};
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo,
};
use octopus_tool_handlers::{
    ToolCall, ToolCallArtifact, ToolHandler, ToolHandlerContext, ToolHandlerError,
    ToolHandlerResult, ToolResultArtifact,
};
use octopus_tool_registry::ToolDescriptor;
use octopus_worker_env::{
    AbsPath, ExecOutput, ExecSurface, FileSurface, NetworkCeiling, PatchOutcome, ProjectedSandbox,
    WorkerEnvError, WorkerEnvironmentLifecycle,
};

use crate::artifact_bound::ArtifactBoundHandler;
use crate::bound_surface::InvocationArtifactSurface;
const NEUTRAL_TOOL_CALL_LABEL: &str = "tool.call";

struct ArtifactBoundProcess {
    interface: InterfaceId,
    handler: ArtifactBoundHandler,
    run_records_root: Option<PathBuf>,
    artifact_blobs_root: Option<PathBuf>,
}

impl ToolProcessHandler for ArtifactBoundProcess {
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
            let (mut call, correlation_id) = decode_invocation(
                &self.interface,
                &invocation.descriptor,
                &invocation.envelope,
            )
            .map_err(capability)?;
            attach_skill_source_context(
                &mut call,
                &invocation.descriptor,
                &invocation.envelope,
                self.run_records_root.as_ref(),
            )
            .map_err(capability)?;
            let surface = invocation_surface(&invocation.envelope, self.artifact_blobs_root.as_deref()).map_err(capability)?;
            let context =
                handler_context(&invocation.envelope, Arc::clone(&surface)).map_err(capability)?;
            let formation = invocation.worker_environment.as_ref().ok_or_else(|| {
                capability("artifact-bound invocation has no formed worker environment")
            })?;
            let (lifecycle, response) = handler_completion(
                &call.tool_id,
                correlation_id,
                self.handler.execute(&context, &call).await,
            );
            io.record_worker_environment_lifecycle(formation, lifecycle)
                .await
                .map_err(ToolProcessError::Unavailable)?;
            Ok(vec![materialize_response(&invocation.envelope, response).map_err(capability)?])
        })
    }
}

fn handler_completion(
    tool_id: &str,
    correlation_id: Option<String>,
    outcome: Result<ToolHandlerResult, ToolHandlerError>,
) -> (WorkerEnvironmentLifecycle, ToolResultArtifact) {
    match outcome {
        Ok(result) => (
            WorkerEnvironmentLifecycle::Active,
            ToolResultArtifact::success(tool_id, correlation_id, result.body),
        ),
        Err(error) => (
            WorkerEnvironmentLifecycle::Failed,
            ToolResultArtifact::failure(tool_id, correlation_id, error.into_tool_result_error()),
        ),
    }
}

pub async fn run_artifact_bound_tool_process() -> Result<(), String> {
    let env = ToolProcessEnvironment::from_env()?;
    octopus_tool_door_runtime::run_tool_process(Arc::new(ArtifactBoundProcess {
        interface: env.interface,
        handler: ArtifactBoundHandler,
        run_records_root: env.root_slots.get("run_records_root").cloned(),
        artifact_blobs_root: env.root_slots.get("artifact_blobs_root").cloned(),
    }))
    .await
}

fn attach_skill_source_context(
    call: &mut ToolCall,
    descriptor: &ToolDescriptor,
    input: &InterfaceInvocationEnvelope,
    run_records_root: Option<&PathBuf>,
) -> Result<(), String> {
    let operation = descriptor
        .handler_config
        .get("operation")
        .and_then(serde_json::Value::as_str);
    if !matches!(operation, Some("skill_capture" | "skill_upsert")) {
        return Ok(());
    }
    let runtime = input.runtime().map_err(|error| error.to_string())?;
    let expected_hash = runtime.run_record_hash.as_deref().ok_or_else(|| {
        "skill write requires the exact active run-record hash in invocation metadata".to_string()
    })?;
    let root = run_records_root
        .ok_or_else(|| "skill write process has no granted run_records_root".to_string())?;
    let run_name = runtime.run.as_str();
    let source_context =
        attested_skill_source_context(root, run_name, runtime.worker.as_str(), expected_hash)?;
    let arguments = call
        .arguments
        .as_object_mut()
        .ok_or_else(|| "skill write arguments must be a JSON object".to_string())?;
    if arguments.contains_key("_skill_source_context") {
        return Err("skill write arguments contain reserved source context".to_string());
    }
    arguments.insert("_skill_source_context".to_string(), source_context);
    Ok(())
}

fn attested_skill_source_context(
    root: &Path,
    run_name: &str,
    expected_worker: &str,
    expected_hash: &str,
) -> Result<serde_json::Value, String> {
    let mut components = std::path::Path::new(run_name).components();
    let safe = matches!(components.next(), Some(std::path::Component::Normal(name)) if name.to_str() == Some(run_name))
        && components.next().is_none();
    if !safe {
        return Err(format!(
            "skill write run id '{run_name}' is not a record file stem"
        ));
    }
    let canonical_root = root.canonicalize().map_err(|error| {
        format!(
            "skill write run_records_root '{}' cannot be resolved: {error}",
            root.display()
        )
    })?;
    let path = canonical_root.join(format!("{run_name}.json"));
    if path.parent() != Some(canonical_root.as_path()) {
        return Err(format!(
            "skill write run record '{}' escapes run_records_root",
            path.display()
        ));
    }
    let canonical_path = path.canonicalize().map_err(|error| {
        format!(
            "skill write run record '{}' cannot be resolved: {error}",
            path.display()
        )
    })?;
    if canonical_path.parent() != Some(canonical_root.as_path()) {
        return Err(format!(
            "skill write run record '{}' resolves outside run_records_root",
            canonical_path.display()
        ));
    }
    let mut file = std::fs::File::open(&canonical_path).map_err(|error| {
        format!(
            "skill write run record '{}' cannot be opened: {error}",
            canonical_path.display()
        )
    })?;
    let before = file.metadata().map_err(|error| {
        format!(
            "skill write run record '{}' metadata cannot be read: {error}",
            canonical_path.display()
        )
    })?;
    if !before.is_file() || before.len() > 4 * 1024 * 1024 {
        return Err(format!(
            "skill write run record '{}' is not a bounded regular file",
            canonical_path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.read_to_end(&mut bytes).map_err(|error| {
        format!(
            "skill write run record '{}' cannot be read: {error}",
            canonical_path.display()
        )
    })?;
    let after = file.metadata().map_err(|error| {
        format!(
            "skill write run record '{}' final metadata cannot be read: {error}",
            canonical_path.display()
        )
    })?;
    if before.len() != after.len() || bytes.len() as u64 != before.len() {
        return Err(format!(
            "skill write run record '{}' changed while being read",
            canonical_path.display()
        ));
    }
    let observed_hash = octopus_ledger::ContentHash::of_bytes(&bytes).to_hex();
    if observed_hash != expected_hash {
        return Err(format!(
            "skill write run record hash changed: invocation has {expected_hash}, file has {observed_hash}"
        ));
    }
    let record: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("skill write run record is malformed: {error}"))?;
    let object = record
        .as_object()
        .ok_or_else(|| "skill write run record must be a JSON object".to_string())?;
    let exact = |name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("skill write run record has no non-empty string '{name}'"))
    };
    let record_id = exact("id")?;
    let trace = exact("trace")?;
    let worker = exact("worker")?;
    if record_id != run_name || worker != expected_worker {
        return Err(format!(
            "skill write run provenance does not match invocation coordinates: record=({record_id},{worker}) invocation=({run_name},{expected_worker})"
        ));
    }
    Ok(serde_json::json!({
        "run": run_name,
        "trace": trace,
        "worker": expected_worker,
    }))
}

fn decode_invocation(
    interface: &InterfaceId,
    descriptor: &ToolDescriptor,
    input: &InterfaceInvocationEnvelope,
) -> Result<(ToolCall, Option<String>), String> {
    if &descriptor.interface != interface {
        return Err(format!(
            "tool descriptor '{}' belongs to interface '{}', not process interface '{}'",
            descriptor.id, descriptor.interface, interface
        ));
    }
    let manifest = input
        .read_artifact_manifest()
        .map_err(|error| error.to_string())?;
    if manifest.label != NEUTRAL_TOOL_CALL_LABEL {
        return Err(format!(
            "system_context manifest label '{}' is not '{}'",
            manifest.label, NEUTRAL_TOOL_CALL_LABEL
        ));
    }
    let payload = input
        .read_artifact_payload()
        .map_err(|error| error.to_string())?;
    let artifact: ToolCallArtifact = serde_json::from_slice(&payload)
        .map_err(|error| format!("tool.call artifact is malformed: {error}"))?;
    let tool = artifact.tool.trim();
    if tool.is_empty() {
        return Err("tool.call carries an empty tool id".to_string());
    }
    if descriptor.id.as_str() != tool {
        return Err(format!(
            "tool descriptor '{}' does not match tool.call '{}'",
            descriptor.id, tool
        ));
    }
    let granted = input
        .granted_tool_names()
        .map_err(|error| error.to_string())?;
    if !granted.iter().any(|candidate| candidate == tool) {
        return Err(format!(
            "tool '{}' is absent from the formed tool grant",
            tool
        ));
    }
    let blocked = input
        .hard_blocked_tool_names()
        .map_err(|error| error.to_string())?;
    if blocked.iter().any(|candidate| candidate == tool) {
        return Err(format!("tool '{}' is hard-blocked", tool));
    }
    Ok((
        ToolCall {
            tool_id: tool.to_string(),
            arguments: artifact.arguments,
            handler_config: descriptor.handler_config.clone(),
        },
        artifact.correlation_id,
    ))
}

fn invocation_surface(
    input: &InterfaceInvocationEnvelope,
    artifact_blobs_root: Option<&std::path::Path>,
) -> Result<Arc<InvocationArtifactSurface>, String> {
    let parent = input
        .artifact_file
        .manifest_path()
        .parent()
        .ok_or_else(|| "system_context invocation manifest has no parent".to_string())?;
    let grant = input.grant().map_err(|error| error.to_string())?.clone();
    InvocationArtifactSurface::new(
        &input.artifact_binding_payloads,
        &parent.join("emitted"),
        grant,
        artifact_blobs_root,
    )
    .map(Arc::new)
    .map_err(|error| error.to_string())
}

fn handler_context(
    input: &InterfaceInvocationEnvelope,
    surface: Arc<InvocationArtifactSurface>,
) -> Result<ToolHandlerContext, String> {
    let runtime = input.runtime().map_err(|error| error.to_string())?;
    let room_root = input
        .artifact_file
        .manifest_path()
        .parent()
        .ok_or_else(|| "system_context invocation manifest has no parent".to_string())?
        .join("emitted");
    Ok(ToolHandlerContext {
        run: runtime.run.as_str().to_string(),
        room_root,
        sandbox: ProjectedSandbox::from_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            NetworkCeiling::Denied,
        ),
        fs: Arc::new(SealedFs),
        exec: Arc::new(SealedExec),
        artifacts: Some(surface),
    })
}

fn materialize_response(
    input: &InterfaceInvocationEnvelope,
    response: ToolResultArtifact,
) -> Result<InterfaceArtifactFile, String> {
    let bytes = serde_json::to_vec(&response)
        .map_err(|error| format!("system_context result did not serialize: {error}"))?;
    let parent = input
        .artifact_file
        .manifest_path()
        .parent()
        .ok_or_else(|| "system_context invocation manifest has no parent".to_string())?;
    InterfaceArtifactFile::write(parent.join("emitted"), &response.label, &bytes)
        .map_err(|error| error.to_string())
}

fn capability(error: impl ToString) -> ToolProcessError {
    ToolProcessError::Capability(error.to_string())
}

struct SealedFs;

impl FileSurface for SealedFs {
    fn read(
        &self,
        _path: &AbsPath,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<u8>, WorkerEnvError>> + Send + '_>,
    > {
        Box::pin(async {
            Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context process holds no file surface".into(),
            ))
        })
    }

    fn write(
        &self,
        _path: &AbsPath,
        _bytes: &[u8],
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<octopus_worker_env::FileWriteOutput, WorkerEnvError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async {
            Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context process holds no file surface".into(),
            ))
        })
    }

    fn list(
        &self,
        _dir: &AbsPath,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<AbsPath>, WorkerEnvError>> + Send + '_>,
    > {
        Box::pin(async {
            Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context process holds no file surface".into(),
            ))
        })
    }

    fn apply_patch(
        &self,
        _patch: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<PatchOutcome, WorkerEnvError>> + Send + '_>,
    > {
        Box::pin(async {
            Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context process holds no file surface".into(),
            ))
        })
    }
}

struct SealedExec;

impl ExecSurface for SealedExec {
    fn run(
        &self,
        _request: octopus_worker_env::ExecRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecOutput, WorkerEnvError>> + Send + '_>,
    > {
        Box::pin(async {
            Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context process holds no exec surface".into(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use octopus_ledger::ContentHash;

    use super::{WorkerEnvironmentLifecycle, attested_skill_source_context, handler_completion};

    #[test]
    fn artifact_bound_handler_outcomes_carry_lifecycle_and_typed_results() {
        let (lifecycle, result) = handler_completion(
            "artifact.append",
            Some("append-1".to_string()),
            Ok(octopus_tool_handlers::ToolHandlerResult::new(
                serde_json::json!({"result": "artifact_appended"}),
            )),
        );
        assert_eq!(lifecycle, WorkerEnvironmentLifecycle::Active);
        assert!(result.ok && result.tool == "artifact.append" && result.error.is_none());
        let (lifecycle, result) = handler_completion(
            "artifact.list",
            Some("list-1".to_string()),
            Err(octopus_tool_handlers::ToolHandlerError::Execution {
                tool_id: "artifact.list".to_string(),
                reason: "bound surface failed closed".to_string(),
            }),
        );
        assert_eq!(lifecycle, WorkerEnvironmentLifecycle::Failed);
        assert!(!result.ok && result.tool == "artifact.list" && result.body.is_none());
        assert_eq!(result.error.unwrap().code, "tool_execution_failed");
    }

    #[test]
    fn skill_source_context_is_bound_to_exact_run_bytes_and_coordinates() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "id": "run_skill",
            "ee": "ee_skill",
            "worker": "system_worker_memory",
            "trace": "trace_skill",
            "status": {},
            "checkpoints": [],
            "active_checkpoints": []
        }))
        .unwrap();
        std::fs::write(temp.path().join("run_skill.json"), &bytes).unwrap();
        let hash = ContentHash::of_bytes(&bytes).to_hex();
        let context =
            attested_skill_source_context(temp.path(), "run_skill", "system_worker_memory", &hash)
                .unwrap();
        assert_eq!(context["run"], "run_skill");
        assert_eq!(context["trace"], "trace_skill");
        assert_eq!(context["worker"], "system_worker_memory");
        assert!(
            attested_skill_source_context(temp.path(), "run_skill", "system_worker_coder", &hash,)
                .is_err()
        );
        assert!(
            attested_skill_source_context(
                temp.path(),
                "run_skill",
                "system_worker_memory",
                &ContentHash::of_bytes(b"other").to_hex(),
            )
            .is_err()
        );
        assert!(
            attested_skill_source_context(
                temp.path(),
                "../run_skill",
                "system_worker_memory",
                &hash,
            )
            .is_err()
        );
    }
}
