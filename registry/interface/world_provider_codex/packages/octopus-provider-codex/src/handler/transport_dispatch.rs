use super::*;

#[derive(serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexTransportDispatch<'a> {
    pub(crate) label: &'static str,
    pub(crate) provider_interface: &'a InterfaceId,
    pub(crate) source_movement_seq: u64,
    pub(crate) attempt: u32,
    pub(crate) method: &'a str,
    pub(crate) transport: &'static str,
    pub(crate) endpoint: CodexTransportEndpoint,
    pub(crate) request: CodexTransportRequest,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) credential_handle_fingerprint: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexTransportEndpoint {
    pub(crate) scheme: &'static str,
    pub(crate) host: &'static str,
    pub(crate) port: Option<u16>,
    pub(crate) path: String,
}

#[derive(serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexTransportRequest {
    pub(crate) bytes: u64,
    pub(crate) blake3: String,
}

#[derive(Clone)]
pub(crate) struct CodexRecoveredHandoffArtifact {
    pub movement_seq: u64,
    pub artifact: ArtifactRef,
    pub payload: Vec<u8>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveredCodexTransportDispatch {
    pub(crate) label: String,
    pub(crate) provider_interface: InterfaceId,
    pub(crate) source_movement_seq: u64,
    pub(crate) attempt: u32,
    pub(crate) method: String,
    pub(crate) transport: String,
    pub(crate) endpoint: RecoveredCodexTransportEndpoint,
    pub(crate) request: RecoveredCodexTransportRequest,
    pub(crate) credential_handle_fingerprint: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveredCodexTransportEndpoint {
    pub(crate) scheme: String,
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) path: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveredCodexTransportRequest {
    pub(crate) bytes: u64,
    pub(crate) blake3: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoveredTurnStartDispatch {
    pub(crate) movement_seq: u64,
    pub(crate) artifact: ArtifactRef,
    pub(crate) attempt: u32,
    pub(crate) request_blake3: String,
}

pub(crate) struct CodexProviderInvocation<'a> {
    pub callable_tool_names: BTreeSet<String>,
    pub input: InterfaceInvocationEnvelope,
    pub emit: Arc<ProviderOutputStream>,
    pub resolved_session_snapshot: Option<ProviderInvocationSessionSnapshot>,
    pub current_session_context: Arc<Vec<SessionContextArtifact>>,
    pub output_root: &'a Path,
    pub live_stream_root: &'a Path,
    pub source_movement_seq: u64,
    pub attempt: u32,
    pub admitted_run_record: &'a [u8],
    pub current_run_record: &'a [u8],
    pub deferred_context_generations: u64,
    pub input_changes_context: bool,
    pub interrupted_after_artifact: Option<String>,
    pub recovered_handoff_artifacts: Arc<Vec<CodexRecoveredHandoffArtifact>>,
    pub durable_interrupted_continuity: Option<CodexContinuityState>,
    pub reconciled_result_continuity: Option<CodexContinuityState>,
    pub control: &'a Arc<CodexTurnControlBroker>,
    pub handoff_barrier: Arc<ProviderHandoffBarrier>,
}

pub(crate) struct CodexAppServerDispatchFrame {
    pub(crate) method: String,
    pub(crate) bytes: Vec<u8>,
}

pub(crate) fn codex_app_server_request_frame(
    request: &ClientRequest,
) -> Result<CodexAppServerDispatchFrame, InterfaceDriverError> {
    let value = serde_json::to_value(request).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "Codex app-server request could not be encoded for dispatch evidence: {error}"
        ))
    })?;
    let request: JSONRPCRequest = serde_json::from_value(value).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "Codex app-server request is not a valid JSON-RPC request: {error}"
        ))
    })?;
    let method = request.method.clone();
    let mut bytes = serde_json::to_vec(&JSONRPCMessage::Request(request)).map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "Codex app-server JSON-RPC frame could not be encoded: {error}"
        ))
    })?;
    bytes.push(b'\n');
    Ok(CodexAppServerDispatchFrame { method, bytes })
}

pub(crate) fn materialize_codex_transport_dispatch(
    output_root: &Path,
    provider_interface: &InterfaceId,
    source_movement_seq: u64,
    attempt: u32,
    method: &str,
    request_frame: &[u8],
    credential_handle_fingerprint: Option<String>,
) -> Result<InterfaceArtifactFile, InterfaceDriverError> {
    if attempt == 0 {
        return Err(InterfaceDriverError::Protocol(
            "Codex transport dispatch attempt must be positive".to_string(),
        ));
    }
    let payload = serde_json::to_vec(&CodexTransportDispatch {
        label: PROVIDER_TRANSPORT_DISPATCH_LABEL,
        provider_interface,
        source_movement_seq,
        attempt,
        method,
        transport: "process",
        endpoint: CodexTransportEndpoint {
            scheme: "process",
            host: "local",
            port: None,
            path: format!("/{method}"),
        },
        request: CodexTransportRequest {
            bytes: u64::try_from(request_frame.len()).map_err(|_| {
                InterfaceDriverError::Protocol(
                    "Codex transport dispatch request byte count exceeds u64".to_string(),
                )
            })?,
            blake3: octopus_ledger::ContentHash::of_bytes(request_frame).to_hex(),
        },
        credential_handle_fingerprint,
    })
    .map_err(|error| {
        InterfaceDriverError::Protocol(format!(
            "Codex transport dispatch evidence could not be encoded: {error}"
        ))
    })?;
    InterfaceArtifactFile::write(output_root, PROVIDER_TRANSPORT_DISPATCH_LABEL, &payload).map_err(
        |error| {
            InterfaceDriverError::Protocol(format!(
                "Codex transport dispatch evidence could not be materialized: {error}"
            ))
        },
    )
}

pub(crate) fn recovered_turn_start_dispatches(
    artifacts: &[CodexRecoveredHandoffArtifact],
    provider_interface: &InterfaceId,
    source_movement_seq: u64,
) -> Result<Vec<RecoveredTurnStartDispatch>, InterfaceDriverError> {
    let mut seen_movements = BTreeSet::new();
    let mut seen_artifacts = BTreeSet::new();
    let mut turn_starts = Vec::new();
    for recovered in artifacts {
        if recovered.movement_seq <= source_movement_seq {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "recovered Codex transport dispatch movement {} does not follow source movement {}",
                recovered.movement_seq, source_movement_seq
            )));
        }
        if !seen_movements.insert(recovered.movement_seq) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "recovered Codex transport dispatch repeats movement {}",
                recovered.movement_seq
            )));
        }
        if !seen_artifacts.insert((
            recovered.artifact.id.as_str().to_string(),
            recovered.artifact.content.to_hex(),
        )) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "recovered Codex transport dispatch repeats artifact '{}'",
                recovered.artifact.id.as_str()
            )));
        }
        let (label, content) = recovered
            .artifact
            .id
            .as_str()
            .split_once(':')
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(format!(
                    "recovered Codex transport dispatch artifact '{}' has no content-addressed identity",
                    recovered.artifact.id.as_str()
                ))
            })?;
        if label != PROVIDER_TRANSPORT_DISPATCH_LABEL
            || content != recovered.artifact.content.to_hex()
        {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "recovered Codex transport dispatch artifact '{}' conflicts with its content address",
                recovered.artifact.id.as_str()
            )));
        }
        let dispatch: RecoveredCodexTransportDispatch = serde_json::from_slice(&recovered.payload)
            .map_err(|error| {
                InterfaceDriverError::FailedClosed(format!(
                    "recovered Codex transport dispatch artifact '{}' is malformed: {error}",
                    recovered.artifact.id.as_str()
                ))
            })?;
        if dispatch.label != PROVIDER_TRANSPORT_DISPATCH_LABEL
            || dispatch.provider_interface != *provider_interface
            || dispatch.source_movement_seq != source_movement_seq
        {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "recovered Codex transport dispatch at movement {} crossed provider/source coordinates",
                recovered.movement_seq
            )));
        }
        if dispatch.attempt == 0
            || dispatch.method.is_empty()
            || dispatch.method.trim() != dispatch.method
            || dispatch.transport != "process"
            || dispatch.endpoint.scheme != "process"
            || dispatch.endpoint.host != "local"
            || dispatch.endpoint.port.is_some()
            || dispatch.endpoint.path != format!("/{}", dispatch.method)
            || dispatch.request.bytes == 0
        {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "recovered Codex transport dispatch at movement {} violates the local-process evidence contract",
                recovered.movement_seq
            )));
        }
        let request_hash = octopus_ledger::ContentHash::from_hex(&dispatch.request.blake3)
            .filter(|hash| hash.to_hex() == dispatch.request.blake3)
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(format!(
                    "recovered Codex transport dispatch at movement {} has a non-canonical request hash",
                    recovered.movement_seq
                ))
            })?;
        if dispatch.method == "turn/start" {
            if dispatch.credential_handle_fingerprint.is_some() {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "recovered Codex turn/start dispatch at movement {} unexpectedly carries credential evidence",
                    recovered.movement_seq
                )));
            }
            turn_starts.push(RecoveredTurnStartDispatch {
                movement_seq: recovered.movement_seq,
                artifact: recovered.artifact.clone(),
                attempt: dispatch.attempt,
                request_blake3: request_hash.to_hex(),
            });
        }
    }
    turn_starts.sort_by_key(|dispatch| dispatch.movement_seq);
    Ok(turn_starts)
}
