use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octopus_descriptors::{ArtifactId, InterfaceId, RunId};
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{
    InterfaceBoundaryError, InterfaceDescriptor, InterfaceInvocationEnvelope,
};
use octopus_ledger::{ArtifactRef, ContentHash, Endpoint, MovementKind};
use octopus_ledger_store::DurableMovementDecoder;
use octopus_login::AuthManager;
use octopus_model_context_render::model_hidden_tool_names;
use octopus_world_pyecho_life_runtime::{LedgerReplayReader, LedgerTailReader};

use crate::config::{
    CodexInstanceConfig, auth_manager_from_provider_home,
    connect_interface_driver_from_descriptor_with_auth_and_secrets,
};
use crate::handler::{
    CodexAppServerInterfaceDriver, CodexDeferredTurnCause, CodexProviderInvocation,
    CodexRecoveredHandoffArtifact, CodexTurnControl, CodexTurnControlApplied,
    CodexTurnControlBroker, CodexTurnControlDisposition, CodexTurnControlEffectIntent,
    CodexTurnControlResult, ControlOutputError, PROVIDER_TRANSPORT_DISPATCH_LABEL,
    ProviderHandoffBarrier, ProviderOutputStream, ProviderStreamEmission,
    TURN_INTERRUPTED_RESULT_LABEL, codex_continuity_incompatibility,
    codex_continuity_successor_is_settled, codex_continuity_waits_for_result, codex_continuity_has_interrupted_pending,
    codex_session_snapshot_labels, codex_context_output_labels, decode_inflight_turn_control,
    exact_interrupted_control_continuity, exact_interrupted_result_source, exact_interrupted_result_payload,
    interrupted_result_closure_ref, interrupted_result_closure_payload, result_turn_identity, recover_retained_native_outputs, native_terminal_result_closure_ref, recover_provider_result_continuity, is_explicit_turn_control_label,
    is_admitted_peer_interrupt, is_provider_turn_input,
    is_inflight_turn_control_candidate_label, materialize_incompatible_continuity_turn_error,
    materialize_turn_control_result,
    recover_provider_turn_continuity, reconcile_operator_session_continuity, reconcile_delivered_interrupted_continuity, recovered_failed_result_source, terminal_output_continuity,
    codex_projection_output_labels,
    validate_codex_projection_descriptor_contract, validate_durable_interrupted_continuity,
};
use crate::live_stream::{LiveStreamRules, artifact_payload_blake3, recover_sealed_terminal_stream, seal_source_turn_stream};

#[cfg(test)]
#[path = "../tests/support/interrupt_replay.rs"]
mod interrupt_replay_tests;
#[cfg(test)]
#[path = "../tests/support/failure_projection.rs"]
mod failure_projection_tests;
use crate::opencodex::{
    OpenCodexDescriptorConfig, descriptor_config as opencodex_descriptor_config,
};
use octopus_provider_run_authoring::{
    CheckedRunError, FailedSourceHandoffError, ProviderEmission, ProviderEmissionRoute, ProviderHandoffOutcome,
    ProviderHandoffSession, ProviderInvocationSessionSnapshot, ProviderOutputHandoff,
    ProviderSourceRecovery, RunAuthoringResultIdentity, payload_carries_session_context,
    resolve_session_snapshot_chain,
};

const TURN_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);
const PROVIDER_CONTROL_READ_BATCH_LIMIT: usize = 128;
const MACHINE_CODEX_AUTH_FILE_ENV: &str = "OCTOPUS_MACHINE_CODEX_AUTH_FILE";

#[cfg(test)]
#[path = "../tests/support/opencodex_auth.rs"]
mod opencodex_auth_tests;

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| {
        Box::pin(codex_life(ctx, ProviderLifeExpectation::Native)) as InterfaceLifeFuture
    })
}

pub fn opencodex_interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| {
        Box::pin(codex_life(ctx, ProviderLifeExpectation::OpenCodex)) as InterfaceLifeFuture
    })
}

pub fn doctor_requested() -> Result<bool, String> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(false),
        [argument] if argument == "--doctor" => Ok(true),
        _ => Err("provider life binary accepts no arguments or exactly '--doctor'".to_string()),
    }
}

pub fn doctor_native_interface() -> Result<(), String> {
    doctor_interface(ProviderLifeExpectation::Native)
}

pub fn doctor_opencodex_interface() -> Result<(), String> {
    doctor_interface(ProviderLifeExpectation::OpenCodex)
}

#[derive(Clone, Copy)]
enum ProviderLifeExpectation {
    Native,
    OpenCodex,
}

enum ProviderConnectionMode {
    Native,
    OpenCodex(OpenCodexDescriptorConfig),
}

struct ProviderConnectionFailure {
    message: String,
    retryable: bool,
}

impl std::fmt::Display for ProviderConnectionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn doctor_interface(expectation: ProviderLifeExpectation) -> Result<(), String> {
    let interface = std::env::var("OCTOPUS_INTERFACE_ID")
        .map(InterfaceId::new)
        .map_err(|_| "OCTOPUS_INTERFACE_ID is required by the doctor contract".to_string())?;
    let descriptor =
        octopus_world_pyecho_life_runtime::interface_descriptor_from_process_env(&interface)?
            .ok_or_else(|| {
                format!(
                    "{} is required by the doctor contract",
                    octopus_interface_transport::RESOLVED_INTERFACE_DESCRIPTOR_ENV
                )
            })?;
    validate_provider_descriptor_contract(&descriptor, expectation).map(|_| ())
}

fn validate_provider_descriptor_contract(
    descriptor: &InterfaceDescriptor,
    expectation: ProviderLifeExpectation,
) -> Result<ProviderConnectionMode, String> {
    let connection_mode = match (expectation, opencodex_descriptor_config(descriptor)?) {
        (ProviderLifeExpectation::Native, None) => ProviderConnectionMode::Native,
        (ProviderLifeExpectation::Native, Some(_)) => {
            return Err(format!(
                "native Codex life refuses OpenCodex interface '{}'",
                descriptor.id
            ));
        }
        (ProviderLifeExpectation::OpenCodex, Some(config)) => {
            ProviderConnectionMode::OpenCodex(config)
        }
        (ProviderLifeExpectation::OpenCodex, None) => {
            return Err(format!(
                "OpenCodex life requires interface '{}' to declare private_config.opencodex",
                descriptor.id
            ));
        }
    };
    validate_codex_projection_descriptor_contract(descriptor).map_err(|error| {
        format!(
            "codex interface '{}' projection contract is invalid: {error}",
            descriptor.id
        )
    })?;
    CodexInstanceConfig::from_descriptor(descriptor).map_err(|error| error.to_string())?;
    RunAuthoringResultIdentity::from_descriptor(descriptor)?;
    Ok(connection_mode)
}

#[derive(Clone)]
struct QueuedTurn {
    envelope: InterfaceInvocationEnvelope,
    seq: u64,
}

struct CodexRuntimeAuth {
    manager: Arc<AuthManager>,
    provider_home: PathBuf,
    auth_store_home: PathBuf,
    activation_secrets_home: PathBuf,
    connection_mode: ProviderConnectionMode,
}

struct CodexTurnScope<'a> {
    ctx: &'a InterfaceLiftContext,
    descriptor: &'a InterfaceDescriptor,
    handoff_session: &'a ProviderHandoffSession,
    runtime_auth: &'a CodexRuntimeAuth,
    run: &'a RunId,
    control: &'a Arc<CodexTurnControlBroker>,
    interrupted_effects: &'a InterruptedEffectArbiter,
}

#[derive(Default)]
struct ForwardedProviderEmissions {
    outputs: usize,
    dispatches: usize,
    bridged: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum InterruptedResultAdmission {
    Bridged,
    Admitted(usize),
}

type PendingProviderEmission = (
    ProviderEmission,
    Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    Vec<octopus_provider_run_authoring::ProviderOutgoingBinding>,
);

type ActiveTurnControls = Arc<tokio::sync::Mutex<BTreeMap<String, Arc<CodexTurnControlBroker>>>>;
type ControlSourceGates =
    Arc<tokio::sync::Mutex<BTreeMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>>;

#[derive(Clone, Copy)]
struct MainReaderProgress {
    next_seq: u64,
    revision: u64,
}

struct ControlReaderFence {
    run: RunId,
    acknowledged: Option<tokio::sync::oneshot::Sender<u64>>,
}

struct PendingControlReaderFence {
    fence: ControlReaderFence,
    watermark: Option<u64>,
}

struct PendingControlReaderSource {
    envelope: InterfaceInvocationEnvelope,
    label: String,
    run: RunId,
}

struct ControlSource<'a> {
    envelope: &'a InterfaceInvocationEnvelope,
    source_movement_seq: u64,
    broker: &'a Arc<CodexTurnControlBroker>,
    control_gates: &'a ControlSourceGates,
    control: CodexTurnControl,
    terminal_control_fenced: bool,
    independent_reader: bool,
}

struct MalformedControlSource<'a> {
    envelope: &'a InterfaceInvocationEnvelope,
    source_movement_seq: u64,
    broker: Option<&'a Arc<CodexTurnControlBroker>>,
    control_gates: &'a ControlSourceGates,
    error: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlSourceDisposition {
    Handled,
    DeferredTurn,
}

#[derive(Clone)]
struct DeferredProviderTurn {
    run: RunId,
    turn: QueuedTurn,
    release: Option<DeferredProviderTurnRelease>,
}

struct ControlResultDelivery<'a> {
    envelope: &'a InterfaceInvocationEnvelope,
    source_movement_seq: u64,
    output: Option<Arc<ProviderOutputStream>>,
    handoff_barrier: Option<Arc<ProviderHandoffBarrier>>,
    effect_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    result: &'a mut CodexTurnControlResult,
    recovery: bool,
    defer_message_receipt: bool,
}

#[derive(Clone)]
struct InterruptedEffectArbiter {
    ctx: InterfaceLiftContext,
    cache: Arc<tokio::sync::Mutex<InterruptedEffectCache>>,
}

#[derive(Default)]
struct InterruptedEffectCache {
    journals: BTreeMap<String, InterruptedEffectJournal>,
    loaded_runs: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct InterruptedEffectJournal {
    schema_version: u32,
    run_id: String,
    control_source_movement_seq: u64,
    control_id: String,
    acknowledgement: DurableArtifactAdmission,
    continuity: crate::handler::CodexContinuityState,
    phase: InterruptedEffectPhase,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct DurableArtifactAdmission {
    artifact: ArtifactRef,
    movement_seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ArtifactAdmissionScan {
    movement_seq: Option<u64>,
    complete_boundary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum InterruptedEffectPhase {
    AwaitingResult,
    Resolving {
        result_source_movement_seq: u64,
        source_artifact: ArtifactRef,
    },
    Closing {
        result_source_movement_seq: u64,
        closure_artifact: ArtifactRef,
    },
    Reconciled {
        result_source_movement_seq: u64,
        closure: DurableArtifactAdmission,
    },
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct TurnControlJournal {
    schema_version: u32,
    source_movement_seq: u64,
    run_id: String,
    control_id: String,
    action: String,
    phase: TurnControlJournalPhase,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum TurnControlJournalPhase {
    Intent,
    RetargetIntent {
        expected_turn_id: String,
        actual_turn_id: String,
    },
    DeferredTurn {
        result: CodexTurnControlResult,
        cause: CodexDeferredTurnCause,
    },
    Result {
        result: CodexTurnControlResult,
    },
    Delivered {
        result: CodexTurnControlResult,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct DeferredProviderTurnJournal {
    schema_version: u32,
    source_movement_seq: u64,
    run_id: String,
    artifact: ArtifactRef,
    metadata_content_hash: String,
    payload_content_hash: String,
    payload_byte_count: u64,
    artifact_bindings: Vec<DeferredArtifactBindingRef>,
    release: Option<DeferredProviderTurnRelease>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct DeferredProviderTurnJournalV2 {
    schema_version: u32,
    source_movement_seq: u64,
    run_id: String,
    artifact: ArtifactRef,
    manifest_path: PathBuf,
    manifest_bytes: Vec<u8>,
    payload_bytes: Vec<u8>,
    artifact_binding_payloads: Vec<DeferredArtifactBindingPayloadV2>,
    metadata: octopus_ledger::InvocationMetadata,
    release: Option<DeferredProviderTurnRelease>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DeferredProviderTurnRelease {
    SessionRebase {
        source_session_carrier: Option<ArtifactRef>,
        rebased_session_carrier: DurableArtifactAdmission,
    },
    OriginalEnvelope {
        control_id: String,
        action: String,
        observed_ledger_next_seq: u64,
    },
    SettledEnvelope {
        observed_ledger_next_seq: u64,
    },
    RecoveredResult {
        source_session_carrier: Option<ArtifactRef>,
        failed_source_movement_seq: u64,
    },
    InterruptedResult {
        source_session_carrier: Option<ArtifactRef>,
        result_source: DurableArtifactAdmission,
        closure: DurableArtifactAdmission,
    },
}

struct ProviderSessionCarrierScan {
    latest: Option<DurableArtifactAdmission>,
    complete_boundary: bool,
    next_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct DeferredArtifactBindingRef {
    artifact: ArtifactRef,
    bytes_content: ContentHash,
    byte_count: u64,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct DeferredArtifactBindingPayloadV2 {
    artifact: ArtifactRef,
    bytes_content: ContentHash,
    bytes: Vec<u8>,
}

async fn codex_life(
    ctx: InterfaceLiftContext,
    expectation: ProviderLifeExpectation,
) -> Result<(), String> {
    let descriptor = load_own_descriptor(&ctx)?;
    let connection_mode = validate_provider_descriptor_contract(&descriptor, expectation)?;
    let provider_home = ctx.interface_var_dir().join("codex-home");
    let auth_store_home = resolve_provider_auth_store(&ctx, &descriptor, &connection_mode)?;
    let manager = auth_manager_from_provider_home(auth_store_home.clone()).await;
    let activation_secrets_home = ctx.interface_var_dir();
    let runtime_auth = CodexRuntimeAuth {
        manager,
        provider_home,
        auth_store_home,
        activation_secrets_home,
        connection_mode,
    };
    let authoring_result = RunAuthoringResultIdentity::from_descriptor(&descriptor)?;
    let handoff_session = ProviderHandoffSession::new(&ctx)?;
    let mut reader = LedgerTailReader::open(&ctx)?;
    let (main_reader_progress, control_reader_progress) =
        tokio::sync::watch::channel(MainReaderProgress {
            next_seq: reader.next_seq(),
            revision: 0,
        });
    let (control_fence_tx, control_fence_rx) = tokio::sync::mpsc::unbounded_channel();
    let instance_config =
        CodexInstanceConfig::from_descriptor(&descriptor).map_err(|error| error.to_string())?;
    let idle_disconnect = instance_config.idle_disconnect();
    let keep_ready_connected = instance_config.connect_on_ready();
    if keep_ready_connected
        && matches!(
            runtime_auth.connection_mode,
            ProviderConnectionMode::OpenCodex(_)
        )
    {
        return Err(format!(
            "OpenCodex interface '{}' cannot connect_on_ready before a formed interface policy arrives",
            descriptor.id
        ));
    }
    let mut driver = if keep_ready_connected {
        Some(
            connect_declared_interface_driver(
                &ctx,
                &descriptor,
                &runtime_auth,
                None,
            )
            .await
            .map_err(|error| {
                format!(
                    "codex interface '{}' could not connect its explicitly declared ready transport: {error}",
                    ctx.interface.as_str()
                )
            })?,
        )
    } else {
        None
    };
    let mut driver_idle_deadline = None;
    let active_controls = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
    let control_gates = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
    let interrupted_effects = InterruptedEffectArbiter::new(ctx.clone());
    upgrade_deferred_provider_turn_journals(&ctx, &descriptor).await?;
    let mut deferred_turns = load_deferred_provider_turns(&ctx, &descriptor)?;
    ctx.signal_ready()?;
    restore_consumed_provider_inputs(
        &ctx,
        &descriptor,
        &handoff_session,
        &deferred_turns,
        &interrupted_effects,
        reader.next_seq(),
    )
    .await?;
    Box::pin(restore_completed_provider_sources(&ctx, &descriptor, &handoff_session, &runtime_auth, &mut driver,
        &active_controls, &interrupted_effects, reader.next_seq())).await?;
    let mut control_task =
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(Box::pin(control_reader_life(
            ctx.clone(), descriptor.clone(), handoff_session.clone(), active_controls.clone(),
            control_gates.clone(), interrupted_effects.clone(), control_reader_progress, control_fence_rx,
        ))));
    let restored_controls = active_controls.lock().await.iter().map(|(run, broker)|
        (RunId::new(run), broker.source_movement_seq())).collect::<Vec<_>>();
    for (run, source) in restored_controls {
        if let Err(error) = fence_control_reader(&control_fence_tx, &run, source).await {
            return flatten_control_task(control_task.await).and(Err(error));
        }
    }

    loop {
        if ctx.cancel.is_cancelled() {
            finish_active_controls(&active_controls).await;
            control_task.abort();
            let _ = control_task.await;
            shutdown_driver(&ctx, &mut driver).await?;
            return Ok(());
        }
        if control_task.is_finished() {
            let result = flatten_control_task(control_task.await);
            finish_active_controls(&active_controls).await;
            shutdown_driver(&ctx, &mut driver).await?;
            return result;
        }
        let deferred_candidates = deferred_turns
            .iter()
            .filter(|(seq, _)| **seq < reader.next_seq())
            .map(|(seq, deferred)| (*seq, deferred.clone()))
            .collect::<Vec<_>>();
        let mut ready_deferred = None;
        for (seq, deferred) in deferred_candidates {
            if !interrupted_effects.reconciled(&deferred.run).await? {
                continue;
            }
            match prepare_deferred_provider_turn(&ctx, &descriptor, &deferred.run, &deferred, &active_controls).await? {
                DeferredTurnReadiness::AwaitingSessionSettlement => {}
                DeferredTurnReadiness::Ready(turn) => {
                    ready_deferred = Some((seq, turn));
                    break;
                }
            }
        }
        if let Some((seq, turn)) = ready_deferred {
            let DeferredProviderTurn { run, .. } = deferred_turns
                .remove(&seq)
                .ok_or_else(|| format!("deferred provider turn {seq} disappeared"))?;
            retire_controls_before_deferred_turn(
                &active_controls, &main_reader_progress, &control_fence_tx, &run,
            ).await?;
            let broker = CodexTurnControlBroker::new(Some(turn.seq));
            active_controls
                .lock()
                .await
                .insert(run.as_str().to_string(), broker.clone());
            publish_main_reader_progress(&main_reader_progress, reader.next_seq());
            let driven = tokio::select! {
                result = &mut control_task => {
                    broker.finish();
                    remove_control_if_same(&active_controls, &run, &broker).await;
                    let result = flatten_control_task(result);
                    finish_active_controls(&active_controls).await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return result;
                }
                _ = ctx.cancel.cancelled() => {
                    broker.finish();
                    remove_control_if_same(&active_controls, &run, &broker).await;
                    finish_active_controls(&active_controls).await;
                    control_task.abort();
                    let _ = control_task.await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return Ok(());
                }
                result = drive_turn_with_retries(
                    CodexTurnScope {
                        ctx: &ctx,
                        descriptor: &descriptor,
                        handoff_session: &handoff_session,
                        runtime_auth: &runtime_auth,
                        run: &run,
                        control: &broker,
                        interrupted_effects: &interrupted_effects,
                    },
                    &mut driver,
                    turn,
                ) => result,
            };
            if let Err(error) = driven {
                broker.finish();
                remove_control_if_same(&active_controls, &run, &broker).await;
                finish_active_controls(&active_controls).await;
                control_task.abort();
                let _ = control_task.await;
                shutdown_driver(&ctx, &mut driver).await?;
                return Err(error);
            }
            remove_deferred_provider_turn(&ctx, seq)?;
            if broker.is_starting() {
                broker.finish();
            }
            if broker.is_finished() || broker.is_recovered_pending() {
                if let Err(error) =
                    fence_control_reader(&control_fence_tx, &run, broker.source_movement_seq())
                        .await
                {
                    finish_active_controls(&active_controls).await;
                    control_task.abort();
                    let _ = control_task.await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return Err(error);
                }
                if broker.is_finished() { remove_control_if_same(&active_controls, &run, &broker).await; }
            }
            if !deferred_turns.is_empty() {
                continue;
            }
            if keep_ready_connected {
                driver_idle_deadline = None;
            } else if let Some(idle) = idle_disconnect {
                driver_idle_deadline = driver.as_ref().map(|_| tokio::time::Instant::now() + idle);
            } else {
                release_driver_if_quiescent(&ctx, &active_controls, &mut driver).await?;
                driver_idle_deadline = None;
            }
            continue;
        }
        let batch = reader.read_new()?;
        publish_main_reader_progress(&main_reader_progress, reader.next_seq());
        if batch.is_empty() {
            if let Some(deadline) = driver_idle_deadline {
                tokio::select! {
                    result = &mut control_task => {
                        let result = flatten_control_task(result);
                        finish_active_controls(&active_controls).await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return result;
                    }
                    _ = ctx.cancel.cancelled() => {
                        finish_active_controls(&active_controls).await;
                        control_task.abort();
                        let _ = control_task.await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return Ok(());
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        if release_driver_if_quiescent(&ctx, &active_controls, &mut driver).await? {
                            driver_idle_deadline = None;
                        } else {
                            driver_idle_deadline = idle_disconnect
                                .map(|idle| tokio::time::Instant::now() + idle);
                        }
                    }
                    result = reader.wait_for_change(&ctx.cancel) => result?,
                }
            } else {
                tokio::select! {
                    result = &mut control_task => {
                        let result = flatten_control_task(result);
                        finish_active_controls(&active_controls).await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return result;
                    }
                    _ = ctx.cancel.cancelled() => {
                        finish_active_controls(&active_controls).await;
                        control_task.abort();
                        let _ = control_task.await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return Ok(());
                    }
                    result = reader.wait_for_change(&ctx.cancel) => result?,
                }
            }
            continue;
        }
        for own in batch {
            let envelope = match reader.envelope(&own) {
                Ok(envelope) => envelope,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "poisoned_outbound",
                        serde_json::json!({ "error": error }),
                    )
                    .await?;
                    commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                    continue;
                }
            };
            let manifest = envelope.artifact_file.read_manifest().map_err(|error| {
                format!(
                    "provider movement {} has malformed artifact manifest: {error}",
                    own.movement.seq.0
                )
            })?;
            if manifest.label == crate::handler::composer_prediction::REQUEST_LABEL {
                if !matches!(runtime_auth.connection_mode, ProviderConnectionMode::Native) {
                    return Err("composer predictions require a native Codex binding".into());
                }
                if driver.is_none() {
                    driver = Some(connect_declared_interface_driver(&ctx, &descriptor, &runtime_auth, Some(&envelope))
                        .await.map_err(|error| error.message)?);
                }
                if let Err(error) = crate::handler::composer_prediction::run(&ctx, driver.as_ref().unwrap(),
                    &runtime_auth.manager, &envelope, own.movement.seq.0).await {
                    if error.retires_driver() {
                        if error.result_admitted() {
                            commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                        }
                        finish_active_controls(&active_controls).await;
                        control_task.abort();
                        let _ = control_task.await;
                        shutdown_driver(&ctx, &mut driver).await?;
                        return Err(error.to_string());
                    }
                    ctx.hand_admitted_failure(own.movement.seq.0, "composer_prediction_failed", serde_json::json!({"error":error.to_string()})).await?;
                }
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                continue;
            }
            let metadata = match own.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "malformed_runtime_envelope",
                        serde_json::json!({ "error": error }),
                    )
                    .await?;
                    commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                    continue;
                }
            };
            let Some(run) = metadata.runtime.as_ref().map(|runtime| runtime.run.clone()) else {
                ctx.hand_admitted_failure(
                    own.movement.seq.0,
                    "missing_runtime_envelope",
                    serde_json::json!({}),
                )
                .await?;
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                continue;
            };
            if authoring_result
                .consume_if_matches(&ctx, &own.movement, &envelope, &manifest.label)
                .await?
            {
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                continue;
            }
            if deferred_turns.contains_key(&own.movement.seq.0) {
                let replayed = QueuedTurn {
                    envelope,
                    seq: own.movement.seq.0,
                };
                persist_deferred_provider_turn(&ctx, &run, &replayed)?;
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                break;
            }

            let durable_interruption = interrupted_effects.durable_continuity(&run).await?;
            let mut existing_control = active_control(&active_controls, &run).await;
            if existing_control.is_none()
                && let Some(continuity) = durable_interruption.as_ref()
            {
                let broker = CodexTurnControlBroker::new(None);
                broker.publish_recovered_interrupted_pending(continuity)?;
                active_controls
                    .lock()
                    .await
                    .insert(run.as_str().to_string(), broker.clone());
                publish_main_reader_progress(&main_reader_progress, reader.next_seq());
                existing_control = Some(broker);
            }
            let explicit_inactive_resume = existing_control.is_none()
                && is_explicit_turn_control_label(&manifest.label)
                && decode_admitted_turn_control(&ctx, &descriptor, &envelope)
                    .ok()
                    .flatten()
                    .is_some_and(|control| control_action(&control) == "resume");
            if is_inflight_turn_control_candidate_label(&manifest.label)
                && control_journal_exists(&ctx, own.movement.seq.0)?
            {
                match decode_admitted_turn_control(&ctx, &descriptor, &envelope) {
                    Ok(Some(control)) => {
                        let terminal_control_fenced = existing_control.is_some();
                        let broker = existing_control.clone().unwrap_or_else(|| {
                            let broker = CodexTurnControlBroker::new(None);
                            broker.finish();
                            broker
                        });
                        let disposition = handle_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            ControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: &broker,
                                control_gates: &control_gates,
                                control,
                                terminal_control_fenced,
                                independent_reader: false,
                            },
                        )
                        .await?;
                        if disposition == ControlSourceDisposition::DeferredTurn {
                            let turn = QueuedTurn {
                                envelope,
                                seq: own.movement.seq.0,
                            };
                            persist_deferred_provider_turn(&ctx, &run, &turn)?;
                            deferred_turns.entry(turn.seq).or_insert_with(|| {
                                DeferredProviderTurn {
                                    run: run.clone(),
                                    turn,
                                    release: None,
                                }
                            });
                            commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                            break;
                        }
                    }
                    Ok(None) => {
                        handle_malformed_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            MalformedControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: existing_control.as_ref(),
                                control_gates: &control_gates,
                                error: "journaled turn-control source decoded to no control"
                                    .to_string(),
                            },
                        )
                        .await?;
                    }
                    Err(error) => {
                        handle_malformed_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            MalformedControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: existing_control.as_ref(),
                                control_gates: &control_gates,
                                error: error.to_string(),
                            },
                        )
                        .await?;
                    }
                }
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                continue;
            }
            if let Some(broker) = existing_control.as_ref() {
                match decode_admitted_turn_control(&ctx, &descriptor, &envelope) {
                    Ok(Some(control)) => {
                        let disposition = handle_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            ControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker,
                                control_gates: &control_gates,
                                control,
                                terminal_control_fenced: true,
                                independent_reader: false,
                            },
                        )
                        .await?;
                        match disposition {
                            ControlSourceDisposition::Handled => {
                                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                                continue;
                            }
                            ControlSourceDisposition::DeferredTurn => {
                                let turn = QueuedTurn {
                                    envelope,
                                    seq: own.movement.seq.0,
                                };
                                persist_deferred_provider_turn(&ctx, &run, &turn)?;
                                deferred_turns.entry(turn.seq).or_insert_with(|| {
                                    DeferredProviderTurn {
                                        run: run.clone(),
                                        turn,
                                        release: None,
                                    }
                                });
                                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                                break;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) if is_inflight_turn_control_candidate_label(&manifest.label) => {
                        handle_malformed_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            MalformedControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: Some(broker),
                                control_gates: &control_gates,
                                error: error.to_string(),
                            },
                        )
                        .await?;
                        commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                        continue;
                    }
                    Err(_) => {}
                }
            } else if (is_explicit_turn_control_label(&manifest.label) && !explicit_inactive_resume) || is_admitted_peer_interrupt(&envelope, &descriptor) {
                match decode_admitted_turn_control(&ctx, &descriptor, &envelope) {
                    Ok(Some(control)) => {
                        let broker = CodexTurnControlBroker::new(None);
                        broker.finish();
                        handle_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            ControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: &broker,
                                control_gates: &control_gates,
                                control,
                                terminal_control_fenced: true,
                                independent_reader: false,
                            },
                        )
                        .await?;
                    }
                    Ok(None) => {
                        handle_malformed_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            MalformedControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: None,
                                control_gates: &control_gates,
                                error: "explicit turn-control label decoded to no control"
                                    .to_string(),
                            },
                        )
                        .await?;
                    }
                    Err(error) => {
                        handle_malformed_control_source(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &interrupted_effects,
                            &run,
                            MalformedControlSource {
                                envelope: &envelope,
                                source_movement_seq: own.movement.seq.0,
                                broker: None,
                                control_gates: &control_gates,
                                error: error.to_string(),
                            },
                        )
                        .await?;
                    }
                }
                commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                continue;
            }

            let turn = QueuedTurn {
                envelope,
                seq: own.movement.seq.0,
            };
            if matches!(is_provider_turn_input(&descriptor, &turn.envelope), Ok(true)) {
                match operator_turn_requires_deferral_with_context(&ctx, &descriptor, &handoff_session, &run, &turn).await {
                    Ok(true) => {
                        persist_deferred_provider_turn(&ctx, &run, &turn)?;
                        deferred_turns
                            .entry(turn.seq)
                            .or_insert_with(|| DeferredProviderTurn {
                                run: run.clone(),
                                turn,
                                release: None,
                            });
                        commit_main_reader(&mut reader, &own, &main_reader_progress)?;
                        break;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(
                            interface = ctx.interface.as_str(),
                            run = run.as_str(),
                            seq = turn.seq,
                            error = %error,
                            "operator turn deferral preflight delegated validation to the canonical provider path"
                        );
                    }
                }
            }
            let (broker, created_broker) = match existing_control {
                Some(broker) => (broker, false),
                None => {
                    let broker = CodexTurnControlBroker::new(Some(own.movement.seq.0));
                    active_controls
                        .lock()
                        .await
                        .insert(run.as_str().to_string(), broker.clone());
                    publish_main_reader_progress(&main_reader_progress, reader.next_seq());
                    (broker, true)
                }
            };
            let driven = tokio::select! {
                result = &mut control_task => {
                    broker.finish();
                    remove_control_if_same(&active_controls, &run, &broker).await;
                    let result = flatten_control_task(result);
                    finish_active_controls(&active_controls).await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return result;
                }
                _ = ctx.cancel.cancelled() => {
                    broker.finish();
                    remove_control_if_same(&active_controls, &run, &broker).await;
                    finish_active_controls(&active_controls).await;
                    control_task.abort();
                    let _ = control_task.await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return Ok(());
                }
                result = drive_turn_with_retries(
                    CodexTurnScope {
                        ctx: &ctx,
                        descriptor: &descriptor,
                        handoff_session: &handoff_session,
                        runtime_auth: &runtime_auth,
                        run: &run,
                        control: &broker,
                        interrupted_effects: &interrupted_effects,
                    },
                    &mut driver,
                    turn,
                ) => result,
            };
            if let Err(error) = driven {
                broker.finish();
                remove_control_if_same(&active_controls, &run, &broker).await;
                finish_active_controls(&active_controls).await;
                control_task.abort();
                let _ = control_task.await;
                shutdown_driver(&ctx, &mut driver).await?;
                return Err(error);
            }
            commit_main_reader(&mut reader, &own, &main_reader_progress)?;
            if created_broker && broker.is_starting() {
                broker.finish();
            }
            if broker.is_finished() || broker.is_recovered_pending() {
                if let Err(error) =
                    fence_control_reader(&control_fence_tx, &run, broker.source_movement_seq())
                        .await
                {
                    finish_active_controls(&active_controls).await;
                    control_task.abort();
                    let _ = control_task.await;
                    shutdown_driver(&ctx, &mut driver).await?;
                    return Err(error);
                }
                if broker.is_finished() { remove_control_if_same(&active_controls, &run, &broker).await; }
            }
            if !deferred_turns.is_empty() {
                break;
            }
            if keep_ready_connected {
                driver_idle_deadline = None;
            } else if let Some(idle) = idle_disconnect {
                driver_idle_deadline = driver.as_ref().map(|_| tokio::time::Instant::now() + idle);
            } else {
                release_driver_if_quiescent(&ctx, &active_controls, &mut driver).await?;
                driver_idle_deadline = None;
            }
        }
    }
}

fn publish_main_reader_progress(
    progress: &tokio::sync::watch::Sender<MainReaderProgress>,
    next_seq: u64,
) {
    progress.send_modify(|current| {
        current.next_seq = next_seq;
        current.revision = current.revision.saturating_add(1);
    });
}

fn commit_main_reader(
    reader: &mut LedgerTailReader,
    own: &octopus_world_pyecho_life_runtime::OwnOutboundMovement,
    progress: &tokio::sync::watch::Sender<MainReaderProgress>,
) -> Result<(), String> {
    reader.commit_through(own)?;
    publish_main_reader_progress(progress, reader.next_seq());
    Ok(())
}

async fn finish_active_controls(active_controls: &ActiveTurnControls) {
    let mut active = active_controls.lock().await;
    for broker in active.values() {
        broker.finish();
    }
    active.clear();
}

async fn active_controls_are_quiescent(active_controls: &ActiveTurnControls) -> bool {
    let mut active = active_controls.lock().await;
    active.retain(|_, broker| !broker.is_finished());
    active.is_empty()
}

async fn release_driver_if_quiescent(
    ctx: &InterfaceLiftContext,
    active_controls: &ActiveTurnControls,
    driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>,
) -> Result<bool, String> {
    let Some(live) = driver.as_ref() else {
        return Ok(true);
    };
    if !active_controls_are_quiescent(active_controls).await || !live.is_strictly_quiescent().await
    {
        return Ok(false);
    }
    shutdown_driver(ctx, driver).await?;
    Ok(true)
}

async fn shutdown_driver(
    ctx: &InterfaceLiftContext,
    driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>,
) -> Result<(), String> {
    let Some(live) = driver.take() else {
        return Ok(());
    };
    let live = Arc::try_unwrap(live).map_err(|live| {
        *driver = Some(live);
        format!(
            "codex interface '{}' cannot release a driver that is still shared by live work",
            ctx.interface.as_str()
        )
    })?;
    live.shutdown().await.map_err(|error| {
        format!(
            "codex interface '{}' could not shut down its private app-server driver: {error}",
            ctx.interface.as_str()
        )
    })
}

fn current_auth_revision(manager: &AuthManager) -> u64 {
    let receiver = manager.auth_change_receiver();
    *receiver.borrow()
}

async fn adopt_current_machine_auth_generation(
    ctx: &InterfaceLiftContext,
    runtime_auth: &CodexRuntimeAuth,
    driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>,
) -> Result<(), String> {
    runtime_auth.manager.reload().await;
    let current_revision = current_auth_revision(&runtime_auth.manager);
    let Some(live) = driver.as_ref() else {
        return Ok(());
    };
    let connected_revision = live.connected_auth_revision().ok_or_else(|| {
        format!(
            "codex interface '{}' connected a driver without explicit auth-generation evidence",
            ctx.interface.as_str()
        )
    })?;
    if connected_revision == current_revision {
        return Ok(());
    }
    if !live.is_strictly_quiescent().await {
        return Err(format!(
            "codex interface '{}' observed a new machine auth generation while its exact driver was not quiescent",
            ctx.interface.as_str()
        ));
    }
    shutdown_driver(ctx, driver).await
}

async fn connect_declared_interface_driver(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    runtime_auth: &CodexRuntimeAuth,
    envelope: Option<&InterfaceInvocationEnvelope>,
) -> Result<Arc<CodexAppServerInterfaceDriver>, ProviderConnectionFailure> {
    let driver = match &runtime_auth.connection_mode {
        ProviderConnectionMode::Native => {
            connect_interface_driver_from_descriptor_with_auth_and_secrets(
                descriptor,
                &runtime_auth.manager,
                runtime_auth.provider_home.clone(),
                runtime_auth.auth_store_home.clone(),
                runtime_auth.activation_secrets_home.clone(),
            )
            .await
            .map_err(|error| ProviderConnectionFailure {
                retryable: error.is_retryable_transport_failure(),
                message: error.to_string(),
            })
        }
        ProviderConnectionMode::OpenCodex(config) => {
            let envelope = envelope.ok_or_else(|| ProviderConnectionFailure {
                message:
                    "OpenCodex driver connection requires freshly formed interface policy metadata"
                        .to_string(),
                retryable: false,
            })?;
            crate::opencodex::connect_interface_driver(
                ctx,
                descriptor,
                envelope,
                config,
                &runtime_auth.manager,
                runtime_auth.provider_home.clone(),
                runtime_auth.auth_store_home.clone(),
            )
            .await
            .map_err(|message| ProviderConnectionFailure {
                message,
                retryable: true,
            })
        }
    }?;
    driver.attach_life_context(ctx.clone()).map_err(|message| ProviderConnectionFailure { message, retryable: false })?;
    Ok(driver)
}

fn flatten_control_task(
    result: Result<Result<(), String>, tokio::task::JoinError>,
) -> Result<(), String> {
    match result {
        Ok(result) => result,
        Err(error) => Err(format!("codex turn-control reader task failed: {error}")),
    }
}

async fn control_reader_life(
    ctx: InterfaceLiftContext,
    descriptor: InterfaceDescriptor,
    handoff_session: ProviderHandoffSession,
    active_controls: ActiveTurnControls,
    control_gates: ControlSourceGates,
    interrupted_effects: InterruptedEffectArbiter,
    mut main_reader_progress: tokio::sync::watch::Receiver<MainReaderProgress>,
    mut terminal_fences: tokio::sync::mpsc::UnboundedReceiver<ControlReaderFence>,
) -> Result<(), String> {
    use futures::StreamExt;

    let mut reader = LedgerTailReader::open_named(&ctx, "turn-control")?;
    let mut scanned_through = reader.next_seq();
    let mut movements = BTreeMap::new();
    let mut pending = BTreeMap::<u64, PendingControlReaderSource>::new();
    let mut settled = BTreeSet::new();
    let mut running = BTreeMap::<String, u64>::new();
    let mut tasks = futures::stream::FuturesUnordered::new();
    let mut fences = Vec::<PendingControlReaderFence>::new();
    loop {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        while let Ok(fence) = terminal_fences.try_recv() {
            fences.push(PendingControlReaderFence {
                fence,
                watermark: None,
            });
        }
        let batch = reader.read_new_uncommitted_limit(PROVIDER_CONTROL_READ_BATCH_LIMIT)?;
        let caught_up = batch.is_empty();
        for own in batch {
            let seq = own.movement.seq.0;
            scanned_through = scanned_through.max(seq.saturating_add(1));
            match control_reader_source(&reader, &own) {
                Some(source) => {
                    pending.insert(seq, source);
                }
                None => {
                    settled.insert(seq);
                }
            }
            movements.insert(seq, own);
        }
        if caught_up {
            for fence in &mut fences {
                fence.watermark.get_or_insert(scanned_through);
            }
        }
        let mut blocked_runs = running.keys().cloned().collect::<BTreeSet<_>>();
        for seq in pending.keys().copied().collect::<Vec<_>>() {
            let source = pending.get(&seq).expect("pending source selected above");
            let run_key = source.run.as_str().to_string();
            if !blocked_runs.insert(run_key.clone()) {
                continue;
            }
            let broker = active_control(&active_controls, &source.run).await;
            match control_reader_ownership(
                broker.as_deref(), main_reader_progress.borrow().next_seq, seq, &source.label,
            ) {
                ControlReaderOwnership::Main => {
                    pending.remove(&seq);
                    settled.insert(seq);
                    blocked_runs.remove(&run_key);
                    continue;
                }
                ControlReaderOwnership::AwaitMain => continue,
                ControlReaderOwnership::Independent => {}
            }
            let broker = broker.expect("independent control ownership requires a live target");
            let source = pending.remove(&seq).expect("pending source selected above");
            running.insert(run_key.clone(), seq);
            let ctx = ctx.clone();
            let descriptor = descriptor.clone();
            let handoff_session = handoff_session.clone();
            let control_gates = control_gates.clone();
            let interrupted_effects = interrupted_effects.clone();
            tasks.push(async move {
                let result = process_control_reader_source(
                    &ctx,
                    &descriptor,
                    &handoff_session,
                    &interrupted_effects,
                    &control_gates,
                    &broker,
                    seq,
                    source,
                )
                .await;
                (run_key, seq, result)
            });
        }
        let mut committed = None;
        while let Some((&seq, _)) = movements.first_key_value() {
            if !settled.remove(&seq) {
                break;
            }
            committed = movements.pop_first().map(|(_, own)| own);
        }
        if let Some(own) = committed {
            reader.commit_through(&own)?;
        }
        if movements.is_empty() {
            reader.advance_settled_scan()?;
            scanned_through = scanned_through.max(reader.next_seq());
        }
        fences.retain_mut(|pending_fence| {
            let Some(watermark) = pending_fence.watermark else {
                return true;
            };
            let run = &pending_fence.fence.run;
            let waiting = pending
                .range(..watermark)
                .any(|(_, source)| source.run == *run);
            let executing = running
                .get(run.as_str())
                .is_some_and(|seq| *seq < watermark);
            if waiting || executing {
                return true;
            }
            if let Some(acknowledged) = pending_fence.fence.acknowledged.take() {
                let _ = acknowledged.send(watermark);
            }
            false
        });
        tokio::select! {
            completed = tasks.next(), if !tasks.is_empty() => {
                let (run, seq, result) = completed.expect("control task set is not empty");
                result?;
                if running.remove(&run) != Some(seq) {
                    return Err(format!("control reader lost run '{run}' movement {seq} ownership"));
                }
                settled.insert(seq);
            }
            fence = terminal_fences.recv() => {
                let fence = fence.ok_or_else(||
                    "codex main ledger reader dropped the terminal control fence channel".to_string()
                )?;
                fences.push(PendingControlReaderFence { fence, watermark: None });
            }
            changed = main_reader_progress.changed() => {
                changed.map_err(|_| "codex main ledger reader stopped before turn-control intake settled".to_string())?;
            }
            result = reader.wait_for_change(&ctx.cancel) => result?,
            _ = ctx.cancel.cancelled() => return Ok(()),
        }
    }
}

fn decode_admitted_turn_control(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<Option<CodexTurnControl>, InterfaceBoundaryError> {
    let label = envelope.read_artifact_manifest()?.label;
    let pinned = if is_inflight_turn_control_candidate_label(&label)
        && !is_explicit_turn_control_label(&label)
    {
        Some(ctx.pinned_run_record_bytes(envelope)
            .map_err(InterfaceBoundaryError::FailedClosed)?)
    } else {
        None
    };
    decode_inflight_turn_control(descriptor, envelope, pinned.as_deref())
}

fn control_reader_source(
    reader: &LedgerTailReader,
    own: &octopus_world_pyecho_life_runtime::OwnOutboundMovement,
) -> Option<PendingControlReaderSource> {
    let envelope = reader.envelope(own).ok()?;
    let label = envelope.artifact_file.read_manifest().ok()?.label;
    if !is_inflight_turn_control_candidate_label(&label) {
        return None;
    }
    let run = own.metadata().ok()?.runtime.as_ref()?.run.clone();
    Some(PendingControlReaderSource {
        envelope,
        label,
        run,
    })
}

async fn process_control_reader_source(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    interrupted_effects: &InterruptedEffectArbiter,
    control_gates: &ControlSourceGates,
    broker: &Arc<CodexTurnControlBroker>,
    source_movement_seq: u64,
    source: PendingControlReaderSource,
) -> Result<(), String> {
    match decode_admitted_turn_control(ctx, descriptor, &source.envelope) {
        Ok(Some(control)) => {
            handle_control_source(
                ctx,
                descriptor,
                handoff_session,
                interrupted_effects,
                &source.run,
                ControlSource {
                    envelope: &source.envelope,
                    source_movement_seq,
                    broker,
                    control_gates,
                    control,
                    terminal_control_fenced: true,
                    independent_reader: true,
                },
            )
            .await?;
        }
        Ok(None) => {}
        Err(error) => {
            handle_malformed_control_source(
                ctx,
                descriptor,
                handoff_session,
                interrupted_effects,
                &source.run,
                MalformedControlSource {
                    envelope: &source.envelope,
                    source_movement_seq,
                    broker: Some(broker),
                    control_gates,
                    error: error.to_string(),
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn active_control(
    active_controls: &ActiveTurnControls,
    run: &RunId,
) -> Option<Arc<CodexTurnControlBroker>> {
    active_controls.lock().await.get(run.as_str()).cloned()
}

enum ControlReaderOwnership {
    Main,
    AwaitMain,
    Independent,
}

fn control_reader_ownership(
    broker: Option<&CodexTurnControlBroker>,
    main_next_seq: u64,
    source_seq: u64,
    artifact_label: &str,
) -> ControlReaderOwnership {
    let Some(broker) = broker else {
        return if main_next_seq > source_seq {
            ControlReaderOwnership::Main
        } else {
            ControlReaderOwnership::AwaitMain
        };
    };
    if broker.source_movement_seq().is_some_and(|start| source_seq <= start)
        || (broker.is_finished() && !is_explicit_turn_control_label(artifact_label))
    {
        return ControlReaderOwnership::Main;
    }
    if broker.source_movement_seq().is_none() && !broker.is_finished() {
        return if main_next_seq > source_seq {
            ControlReaderOwnership::Main
        } else {
            ControlReaderOwnership::AwaitMain
        };
    }
    ControlReaderOwnership::Independent
}

async fn fence_control_reader(
    terminal_fences: &tokio::sync::mpsc::UnboundedSender<ControlReaderFence>,
    run: &RunId,
    source_movement_seq: Option<u64>,
) -> Result<u64, String> {
    let (acknowledged, acknowledgement) = tokio::sync::oneshot::channel();
    terminal_fences
        .send(ControlReaderFence {
            run: run.clone(),
            acknowledged: Some(acknowledged),
        })
        .map_err(|_| "codex turn-control reader stopped before terminal retirement".to_string())?;
    let watermark = acknowledgement.await.map_err(|_| {
        "codex turn-control reader stopped before acknowledging terminal retirement".to_string()
    })?;
    if let Some(source) = source_movement_seq
        && watermark <= source
    {
        return Err(format!(
            "codex turn-control reader acknowledged terminal watermark {watermark} before provider source {source}"
        ));
    }
    Ok(watermark)
}

async fn retire_controls_before_deferred_turn(
    active_controls: &ActiveTurnControls,
    main_reader_progress: &tokio::sync::watch::Sender<MainReaderProgress>,
    terminal_fences: &tokio::sync::mpsc::UnboundedSender<ControlReaderFence>,
    run: &RunId,
) -> Result<(), String> {
    let broker = match active_control(active_controls, run).await {
        Some(broker) if broker.is_finished() => broker,
        Some(_) => return Err("deferred provider turn is ready before its active control target settled".into()),
        None => {
            let broker = CodexTurnControlBroker::new(None);
            broker.finish();
            active_controls.lock().await.insert(run.to_string(), broker.clone());
            broker
        }
    };
    let next_seq = main_reader_progress.borrow().next_seq;
    publish_main_reader_progress(main_reader_progress, next_seq);
    fence_control_reader(terminal_fences, run, None).await?;
    remove_control_if_same(active_controls, run, &broker).await;
    Ok(())
}

async fn remove_control_if_same(
    active_controls: &ActiveTurnControls,
    run: &RunId,
    broker: &Arc<CodexTurnControlBroker>,
) {
    let mut active = active_controls.lock().await;
    let key = run.as_str();
    if active
        .get(key)
        .is_some_and(|candidate| Arc::ptr_eq(candidate, broker))
    {
        active.remove(key);
    }
}

async fn handle_control_source(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    interrupted_effects: &InterruptedEffectArbiter,
    run: &RunId,
    source: ControlSource<'_>,
) -> Result<ControlSourceDisposition, String> {
    let ControlSource {
        envelope,
        source_movement_seq,
        broker,
        control_gates,
        control,
        terminal_control_fenced,
        independent_reader,
    } = source;
    let gate = control_source_gate(control_gates, run).await;
    let _control_guard = gate.lock().await;
    let pending_transport = broker.pending_intent_transport();
    let control_id = control_id(&control).to_string();
    let action = control_action(&control).to_string();
    let journal = load_control_journal(ctx, source_movement_seq, run, &control_id, &action)?;
    if journal.is_none() && matches!(&control, CodexTurnControl::Steer { .. } | CodexTurnControl::Inject { .. }) {
        let durable_interruption = interrupted_effects.durable_continuity(run).await?;
        let manifest = envelope
            .read_artifact_manifest()
            .map_err(|error| error.to_string())?;
        if !is_explicit_turn_control_label(&manifest.label)
            && (broker.has_interrupted_pending() || durable_interruption.is_some())
        {
            broker.wait_for_durable_interrupt().await?;
            return Ok(ControlSourceDisposition::DeferredTurn);
        }
    }
    let (
        CodexTurnControlApplied {
            mut result,
            disposition: _,
            output,
            handoff_barrier,
            durable_interrupt_handoff,
            effect_guard,
        },
        recovery,
    ) = match journal.map(|journal| journal.phase) {
        Some(TurnControlJournalPhase::DeferredTurn { .. }) => {
            return Ok(ControlSourceDisposition::DeferredTurn);
        }
        Some(TurnControlJournalPhase::Delivered { result })
        | Some(TurnControlJournalPhase::Result { result }) => (
            CodexTurnControlApplied {
                result,
                disposition: CodexTurnControlDisposition::Applied,
                output: broker.output().await,
                handoff_barrier: None,
                durable_interrupt_handoff: None,
                effect_guard: None,
            },
            true,
        ),
        Some(TurnControlJournalPhase::Intent) => {
            let recovered = Box::pin(recover_pending_intent_control(ctx, descriptor, handoff_session, run, broker,
                pending_transport.as_ref(), &control)).await?;
            let result = recovered.unwrap_or_else(|| CodexTurnControlResult {
                control_id: control_id.clone(),
                action: action.clone(),
                acknowledged: false,
                thread_id: None,
                turn_id: None,
                detail: serde_json::json!({
                    "error": "provider process restarted after recording a turn-control intent; the external effect outcome is ambiguous and was not replayed"
                }),
                continuity: None,
            });
            persist_control_journal(
                ctx,
                source_movement_seq,
                run,
                &control_id,
                &action,
                TurnControlJournalPhase::Result {
                    result: result.clone(),
                },
            )?;
            (
                CodexTurnControlApplied {
                    result,
                    disposition: CodexTurnControlDisposition::Applied,
                    output: broker.output().await,
                    handoff_barrier: None,
                    durable_interrupt_handoff: None,
                    effect_guard: None,
                },
                true,
            )
        }
        Some(TurnControlJournalPhase::RetargetIntent {
            expected_turn_id,
            actual_turn_id,
        }) => {
            let result = CodexTurnControlResult {
                control_id: control_id.clone(),
                action: action.clone(),
                acknowledged: false,
                thread_id: None,
                turn_id: Some(actual_turn_id.clone()),
                detail: serde_json::json!({
                    "error": "provider process restarted after recording a one-shot steer retarget intent; the retry outcome is ambiguous and was not replayed",
                    "first_expected_turn_id": expected_turn_id,
                    "retry_expected_turn_id": actual_turn_id,
                }),
                continuity: None,
            };
            persist_control_journal(
                ctx,
                source_movement_seq,
                run,
                &control_id,
                &action,
                TurnControlJournalPhase::Result {
                    result: result.clone(),
                },
            )?;
            (
                CodexTurnControlApplied {
                    result,
                    disposition: CodexTurnControlDisposition::Applied,
                    output: broker.output().await,
                    handoff_barrier: None,
                    durable_interrupt_handoff: None,
                    effect_guard: None,
                },
                true,
            )
        }
        None => {
            let mut applied = if terminal_control_fenced
                && terminal_control_is_satisfied(&action)
                && broker.is_finished()
            {
                terminally_satisfied_control(&control_id, &action)
            } else {
                broker
                    .apply(control, |intent| {
                        let phase = match intent {
                            CodexTurnControlEffectIntent::Initial => {
                                TurnControlJournalPhase::Intent
                            }
                            CodexTurnControlEffectIntent::Retarget {
                                expected_turn_id,
                                actual_turn_id,
                            } => TurnControlJournalPhase::RetargetIntent {
                                expected_turn_id,
                                actual_turn_id,
                            },
                        };
                        persist_control_journal(
                            ctx,
                            source_movement_seq,
                            run,
                            &control_id,
                            &action,
                            phase,
                        )
                    })
                    .await?
            };
            if let CodexTurnControlDisposition::DeferredTurn(cause) = applied.disposition {
                persist_control_journal(
                    ctx,
                    source_movement_seq,
                    run,
                    &control_id,
                    &action,
                    TurnControlJournalPhase::DeferredTurn {
                        result: applied.result,
                        cause,
                    },
                )?;
                return Ok(ControlSourceDisposition::DeferredTurn);
            }
            if terminal_control_fenced
                && terminal_control_is_satisfied(&action)
                && broker.is_finished()
                && control_finished_without_dispatch(&applied.result)
            {
                applied = terminally_satisfied_control(&control_id, &action);
            }
            persist_control_journal(
                ctx,
                source_movement_seq,
                run,
                &control_id,
                &action,
                TurnControlJournalPhase::Result {
                    result: applied.result.clone(),
                },
            )?;
            (applied, false)
        }
    };
    if result.acknowledged && terminal_control_is_satisfied(&result.action)
        && (pending_transport.is_some() || result.detail.get("pending_source_withdrawal").is_some())
    {
        Box::pin(withdraw_pending_source_control(ctx, descriptor, handoff_session, run, broker,
            pending_transport, envelope, source_movement_seq, &mut result)).await?;
    }
    let handoff = hand_control_result(
        ctx,
        descriptor,
        handoff_session,
        interrupted_effects,
        run,
        ControlResultDelivery {
            envelope,
            source_movement_seq,
            output,
            handoff_barrier,
            effect_guard,
            result: &mut result,
            recovery,
            defer_message_receipt: independent_reader,
        },
    )
    .await;
    if let Some(barrier) = durable_interrupt_handoff {
        barrier.finish(handoff.clone().map(|_| ()));
    }
    if !handoff? {
        return Ok(ControlSourceDisposition::Handled);
    }
    persist_control_journal(
        ctx,
        source_movement_seq,
        run,
        &control_id,
        &action,
        TurnControlJournalPhase::Delivered { result },
    )?;
    Ok(ControlSourceDisposition::Handled)
}

fn terminal_control_is_satisfied(action: &str) -> bool {
    matches!(action, "interrupt" | "stop")
}

fn control_finished_without_dispatch(result: &CodexTurnControlResult) -> bool {
    !result.acknowledged && result.thread_id.is_none() && result.turn_id.is_none()
}

fn terminally_satisfied_control(control_id: &str, action: &str) -> CodexTurnControlApplied {
    CodexTurnControlApplied {
        result: CodexTurnControlResult {
            control_id: control_id.to_string(),
            action: action.to_string(),
            acknowledged: true,
            thread_id: None,
            turn_id: None,
            detail: serde_json::json!({
                "already_terminal": true,
                "external_effect_dispatched": false,
                "linearization": "control_reader_terminal_fence"
            }),
            continuity: None,
        },
        disposition: CodexTurnControlDisposition::Applied,
        output: None,
        handoff_barrier: None,
        durable_interrupt_handoff: None,
        effect_guard: None,
    }
}

fn handle_malformed_control_source<'a>(
    ctx: &'a InterfaceLiftContext,
    descriptor: &'a InterfaceDescriptor,
    handoff_session: &'a ProviderHandoffSession,
    interrupted_effects: &'a InterruptedEffectArbiter,
    run: &'a RunId,
    source: MalformedControlSource<'a>,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(), String>> + 'a>> {
    Box::pin(handle_malformed_control_source_inner(
        ctx, descriptor, handoff_session, interrupted_effects, run, source,
    ))
}

async fn handle_malformed_control_source_inner(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    interrupted_effects: &InterruptedEffectArbiter,
    run: &RunId,
    source: MalformedControlSource<'_>,
) -> Result<(), String> {
    let MalformedControlSource {
        envelope,
        source_movement_seq,
        broker,
        control_gates,
        error,
    } = source;
    let gate = control_source_gate(control_gates, run).await;
    let _control_guard = gate.lock().await;
    let control_id = envelope.artifact.id.as_str().to_string();
    let action = "decode".to_string();
    let journal = load_control_journal(ctx, source_movement_seq, run, &control_id, &action)?;
    let (mut result, recovery) = match journal.map(|journal| journal.phase) {
        Some(TurnControlJournalPhase::Delivered { .. }) => return Ok(()),
        Some(TurnControlJournalPhase::DeferredTurn { .. }) => {
            return Err(
                "deferred turn-control journal source no longer decodes as its operator message"
                    .to_string(),
            );
        }
        Some(TurnControlJournalPhase::Result { result }) => (result, true),
        Some(TurnControlJournalPhase::Intent)
        | Some(TurnControlJournalPhase::RetargetIntent { .. }) => (
            CodexTurnControlResult {
                control_id: control_id.clone(),
                action: action.clone(),
                acknowledged: false,
                thread_id: None,
                turn_id: None,
                detail: serde_json::json!({
                    "error": "malformed turn-control source recovered from an unexpected intent state"
                }),
                continuity: None,
            },
            true,
        ),
        None => (
            CodexTurnControlResult {
                control_id: control_id.clone(),
                action: action.clone(),
                acknowledged: false,
                thread_id: None,
                turn_id: None,
                detail: serde_json::json!({ "error": error }),
                continuity: None,
            },
            false,
        ),
    };
    persist_control_journal(
        ctx,
        source_movement_seq,
        run,
        &control_id,
        &action,
        TurnControlJournalPhase::Result {
            result: result.clone(),
        },
    )?;
    let output = match broker {
        Some(broker) => broker.output().await,
        None => None,
    };
    let handoff_barrier = None;
    let effect_guard = None;
    hand_control_result(
        ctx,
        descriptor,
        handoff_session,
        interrupted_effects,
        run,
        ControlResultDelivery {
            envelope,
            source_movement_seq,
            output,
            handoff_barrier,
            effect_guard,
            result: &mut result,
            recovery,
            defer_message_receipt: false,
        },
    )
    .await?;
    persist_control_journal(
        ctx,
        source_movement_seq,
        run,
        &control_id,
        &action,
        TurnControlJournalPhase::Delivered { result },
    )
}

fn control_journal_path(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
) -> std::path::PathBuf {
    ctx.interface_var_dir()
        .join("turn-control-journal")
        .join(format!("{source_movement_seq}.json"))
}

fn control_journal_exists(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
) -> Result<bool, String> {
    let path = control_journal_path(ctx, source_movement_seq);
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "turn-control journal path '{}' is not a file",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "turn-control journal path '{}' cannot be inspected: {error}",
            path.display()
        )),
    }
}

fn load_control_journal(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
    run: &RunId,
    control_id: &str,
    action: &str,
) -> Result<Option<TurnControlJournal>, String> {
    let path = control_journal_path(ctx, source_movement_seq);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "turn-control journal '{}' cannot be read: {error}",
                path.display()
            ));
        }
    };
    let journal = serde_json::from_slice::<TurnControlJournal>(&bytes).map_err(|error| {
        format!(
            "turn-control journal '{}' is malformed: {error}",
            path.display()
        )
    })?;
    if journal.schema_version != 1
        || journal.source_movement_seq != source_movement_seq
        || journal.run_id != run.as_str()
        || journal.control_id != control_id
        || journal.action != action
    {
        return Err(format!(
            "turn-control journal '{}' identity does not match movement {} run '{}' control '{}' action '{}'",
            path.display(),
            source_movement_seq,
            run.as_str(),
            control_id,
            action
        ));
    }
    Ok(Some(journal))
}

fn persist_control_journal(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
    run: &RunId,
    control_id: &str,
    action: &str,
    phase: TurnControlJournalPhase,
) -> Result<(), String> {
    let path = control_journal_path(ctx, source_movement_seq);
    let parent = path.parent().ok_or_else(|| {
        format!(
            "turn-control journal path '{}' has no parent",
            path.display()
        )
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "turn-control journal directory '{}' cannot be created: {error}",
            parent.display()
        )
    })?;
    let journal = TurnControlJournal {
        schema_version: 1,
        source_movement_seq,
        run_id: run.as_str().to_string(),
        control_id: control_id.to_string(),
        action: action.to_string(),
        phase,
    };
    let mut bytes = serde_json::to_vec_pretty(&journal)
        .map_err(|error| format!("turn-control journal could not be encoded: {error}"))?;
    bytes.push(b'\n');
    let temporary = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&temporary).map_err(|error| {
            format!(
                "turn-control journal temporary file '{}' cannot be created: {error}",
                temporary.display()
            )
        })?;
        std::io::Write::write_all(&mut file, &bytes).map_err(|error| {
            format!(
                "turn-control journal temporary file '{}' cannot be written: {error}",
                temporary.display()
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "turn-control journal temporary file '{}' cannot be synced: {error}",
                temporary.display()
            )
        })?;
    }
    std::fs::rename(&temporary, &path).map_err(|error| {
        format!(
            "turn-control journal '{}' cannot be installed: {error}",
            path.display()
        )
    })?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "turn-control journal directory '{}' cannot be synced: {error}",
                parent.display()
            )
        })
}

impl InterruptedEffectArbiter {
    fn new(ctx: InterfaceLiftContext) -> Self {
        Self {
            ctx,
            cache: Arc::new(tokio::sync::Mutex::new(InterruptedEffectCache::default())),
        }
    }

    async fn journal(&self, run: &RunId) -> Result<Option<InterruptedEffectJournal>, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        Ok(cache.journals.get(run.as_str()).cloned())
    }

    async fn durable_continuity(
        &self,
        run: &RunId,
    ) -> Result<Option<crate::handler::CodexContinuityState>, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let Some(journal) = cache.journals.get(&key).cloned() else {
            return Ok(None);
        };
        match journal.phase {
            InterruptedEffectPhase::AwaitingResult
            | InterruptedEffectPhase::Resolving { .. }
            | InterruptedEffectPhase::Closing { .. } => Ok(Some(journal.continuity)),
            InterruptedEffectPhase::Reconciled { .. } => Ok(None),
        }
    }

    async fn reconciled(&self, run: &RunId) -> Result<bool, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        Ok(cache.journals.get(run.as_str()).is_none_or(|journal| {
            matches!(&journal.phase, InterruptedEffectPhase::Reconciled { .. })
        }))
    }

    async fn reconciled_result(
        &self, run: &RunId,
    ) -> Result<Option<(crate::handler::CodexContinuityState, u64, DurableArtifactAdmission)>, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        Ok(cache.journals.get(run.as_str()).and_then(|journal| match &journal.phase {
            InterruptedEffectPhase::Reconciled { result_source_movement_seq, closure } =>
                Some((journal.continuity.clone(), *result_source_movement_seq, closure.clone())),
            _ => None,
        }))
    }

    async fn record_acknowledgement(
        &self,
        run: &RunId,
        control_source_movement_seq: u64,
        control_id: &str,
        acknowledgement: DurableArtifactAdmission,
        continuity: crate::handler::CodexContinuityState,
    ) -> Result<(), String> {
        validate_durable_interrupted_continuity(&continuity, &self.ctx.interface, run.as_str())?;
        validate_artifact_admission(
            &self.ctx,
            run,
            control_source_movement_seq,
            &acknowledgement,
        )?;
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let next = InterruptedEffectJournal {
            schema_version: 1,
            run_id: key.clone(),
            control_source_movement_seq,
            control_id: control_id.to_string(),
            acknowledgement,
            continuity,
            phase: InterruptedEffectPhase::AwaitingResult,
        };
        if let Some(current) = cache.journals.get(&key) {
            let same_ack = current.control_source_movement_seq == control_source_movement_seq
                && current.control_id == control_id
                && current.acknowledgement == next.acknowledgement
                && current.continuity == next.continuity;
            if control_source_movement_seq > current.control_source_movement_seq
                && control_id != current.control_id
                && next.acknowledgement.movement_seq > current.acknowledgement.movement_seq
                && crate::handler::codex_continuity_retains_interrupted_effect(&current.continuity, &next.continuity)
            {
                return Ok(());
            }
            match &current.phase {
                InterruptedEffectPhase::AwaitingResult if same_ack => return Ok(()),
                InterruptedEffectPhase::Resolving { .. }
                | InterruptedEffectPhase::Closing { .. }
                | InterruptedEffectPhase::Reconciled { .. }
                    if same_ack =>
                {
                    return Ok(());
                }
                InterruptedEffectPhase::AwaitingResult
                | InterruptedEffectPhase::Resolving { .. }
                | InterruptedEffectPhase::Closing { .. } => {
                    return Err(format!(
                        "run '{}' already has a different unresolved interrupted exact effect",
                        run.as_str()
                    ));
                }
                InterruptedEffectPhase::Reconciled { .. } => {}
            }
        }
        self.persist(&next)?;
        cache.journals.insert(key, next);
        Ok(())
    }

    async fn begin_result(
        &self,
        run: &RunId,
        result_source_movement_seq: u64,
        envelope: &InterfaceInvocationEnvelope,
    ) -> Result<bool, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let Some(journal) = cache.journals.get_mut(&key) else {
            return Ok(false);
        };
        if let InterruptedEffectPhase::Reconciled {
            result_source_movement_seq: current_source,
            ..
        } = &journal.phase
        {
            if *current_source != result_source_movement_seq {
                return Ok(false);
            }
            if !exact_interrupted_result_source(envelope, &journal.continuity)? {
                return Err(format!(
                    "run '{}' replayed interrupted exact-result source {} with different evidence",
                    run.as_str(),
                    result_source_movement_seq
                ));
            }
            return Ok(true);
        }
        if !exact_interrupted_result_source(envelope, &journal.continuity)? {
            return Ok(false);
        }
        let source_artifact = envelope.artifact.clone();
        match &journal.phase {
            InterruptedEffectPhase::AwaitingResult => {
                journal.phase = InterruptedEffectPhase::Resolving {
                    result_source_movement_seq,
                    source_artifact,
                };
            }
            InterruptedEffectPhase::Resolving {
                result_source_movement_seq: current_source,
                source_artifact: current_artifact,
            } if *current_source == result_source_movement_seq
                && *current_artifact == source_artifact =>
            {
                return Ok(true);
            }
            InterruptedEffectPhase::Closing {
                result_source_movement_seq: current_source,
                ..
            }
            | InterruptedEffectPhase::Reconciled {
                result_source_movement_seq: current_source,
                ..
            } if *current_source == result_source_movement_seq => return Ok(true),
            _ => {
                return Err(format!(
                    "run '{}' attempted to resolve a different interrupted exact-result source",
                    run.as_str()
                ));
            }
        }
        self.persist(journal)?;
        Ok(true)
    }

    async fn begin_closing(
        &self,
        run: &RunId,
        result_source_movement_seq: u64,
        closure_artifact: ArtifactRef,
    ) -> Result<(), String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let journal = cache.journals.get_mut(&key).ok_or_else(|| {
            format!(
                "run '{}' emitted an interrupted closure without durable ACK arbitration",
                run.as_str()
            )
        })?;
        match &journal.phase {
            InterruptedEffectPhase::Resolving {
                result_source_movement_seq: current_source,
                ..
            } if *current_source == result_source_movement_seq => {
                journal.phase = InterruptedEffectPhase::Closing {
                    result_source_movement_seq,
                    closure_artifact,
                };
            }
            InterruptedEffectPhase::Closing {
                result_source_movement_seq: current_source,
                closure_artifact: current_closure,
            } if *current_source == result_source_movement_seq
                && *current_closure == closure_artifact =>
            {
                return Ok(());
            }
            InterruptedEffectPhase::Reconciled {
                result_source_movement_seq: current_source,
                closure,
            } if *current_source == result_source_movement_seq
                && closure.artifact == closure_artifact =>
            {
                return Ok(());
            }
            _ => {
                return Err(format!(
                    "run '{}' attempted a conflicting interrupted-result closure",
                    run.as_str()
                ));
            }
        }
        self.persist(journal)
    }

    async fn reconciled_source(
        &self,
        run: &RunId,
        result_source_movement_seq: u64,
    ) -> Result<bool, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        Ok(cache.journals.get(run.as_str()).is_some_and(|journal| {
            matches!(
                journal.phase,
                InterruptedEffectPhase::Reconciled {
                    result_source_movement_seq: current_source,
                    ..
                } if current_source == result_source_movement_seq
            )
        }))
    }

    async fn complete_closing(
        &self,
        run: &RunId,
        result_source_movement_seq: u64,
        source_closure_artifact: &ArtifactRef,
        closure: &DurableArtifactAdmission,
    ) -> Result<(), String> {
        if !artifact_ref_has_label(source_closure_artifact, TURN_INTERRUPTED_RESULT_LABEL)
            || !artifact_ref_has_label(&closure.artifact, TURN_INTERRUPTED_RESULT_LABEL)
        {
            return Err(format!(
                "run '{}' attempted to reconcile a non-interrupted-result closure",
                run.as_str()
            ));
        }
        validate_artifact_admission(&self.ctx, run, result_source_movement_seq, closure)?;
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let journal = cache.journals.get_mut(&key).ok_or_else(|| {
            format!(
                "run '{}' completed an interrupted closure without durable ACK arbitration",
                run.as_str()
            )
        })?;
        match &journal.phase {
            InterruptedEffectPhase::Closing {
                result_source_movement_seq: current_source,
                closure_artifact: current_closure,
            } if *current_source == result_source_movement_seq
                && current_closure == source_closure_artifact =>
            {
                journal.phase = InterruptedEffectPhase::Reconciled {
                    result_source_movement_seq,
                    closure: closure.clone(),
                };
            }
            InterruptedEffectPhase::Reconciled {
                result_source_movement_seq: current_source,
                closure: current_closure,
            } if *current_source == result_source_movement_seq && current_closure == closure => {
                return Ok(());
            }
            _ => {
                return Err(format!(
                    "run '{}' completed a closure outside its durable interrupted phase",
                    run.as_str()
                ));
            }
        }
        self.persist(journal)
    }

    async fn recover_completed_source(
        &self,
        run: &RunId,
        result_source_movement_seq: u64,
        outcomes: &[ProviderHandoffOutcome],
    ) -> Result<(), String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let Some(journal) = cache.journals.get(&key).cloned() else {
            return Ok(());
        };
        match journal.phase {
            InterruptedEffectPhase::Closing {
                result_source_movement_seq: current_source,
                closure_artifact,
            } if current_source == result_source_movement_seq => {
                let closure = interrupted_result_admission(outcomes)?.ok_or_else(|| {
                    format!(
                        "run '{}' recovered exact-result source {} without its admitted interrupted-result output",
                        run.as_str(),
                        result_source_movement_seq
                    )
                })?;
                drop(cache);
                self.complete_closing(run, result_source_movement_seq, &closure_artifact, &closure)
                    .await
            }
            InterruptedEffectPhase::Resolving {
                result_source_movement_seq: current_source,
                ..
            } if current_source == result_source_movement_seq => Err(format!(
                "run '{}' completed interrupted exact-result source {} before its semantic closure was admitted",
                run.as_str(),
                result_source_movement_seq
            )),
            _ => Ok(()),
        }
    }

    fn load_run(&self, cache: &mut InterruptedEffectCache, run: &RunId) -> Result<(), String> {
        let key = run.as_str().to_string();
        if cache.loaded_runs.contains(&key) {
            return Ok(());
        }
        let path = interrupted_effect_journal_path(&self.ctx, run);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                cache.loaded_runs.insert(key);
                return Ok(());
            }
            Err(error) => {
                return Err(format!(
                    "interrupted-effect journal '{}' cannot be read: {error}",
                    path.display()
                ));
            }
        };
        let journal =
            serde_json::from_slice::<InterruptedEffectJournal>(&bytes).map_err(|error| {
                format!(
                    "interrupted-effect journal '{}' is malformed: {error}",
                    path.display()
                )
            })?;
        if journal.schema_version != 1 || journal.run_id != key {
            return Err(format!(
                "interrupted-effect journal '{}' has a mismatched identity",
                path.display()
            ));
        }
        validate_durable_interrupted_continuity(
            &journal.continuity,
            &self.ctx.interface,
            run.as_str(),
        )?;
        validate_artifact_admission(
            &self.ctx,
            run,
            journal.control_source_movement_seq,
            &journal.acknowledgement,
        )?;
        if let InterruptedEffectPhase::Reconciled {
            result_source_movement_seq,
            closure,
        } = &journal.phase
        {
            validate_artifact_admission(&self.ctx, run, *result_source_movement_seq, closure)?;
        }
        cache.journals.insert(key.clone(), journal);
        cache.loaded_runs.insert(key);
        Ok(())
    }

    fn persist(&self, journal: &InterruptedEffectJournal) -> Result<(), String> {
        let path = interrupted_effect_journal_path(&self.ctx, &RunId::new(journal.run_id.clone()));
        persist_json_file(&path, journal, "interrupted-effect journal")
    }
}

fn interrupted_result_admission(
    outcomes: &[ProviderHandoffOutcome],
) -> Result<Option<DurableArtifactAdmission>, String> {
    let mut matched = None;
    for outcome in outcomes {
        match outcome {
            ProviderHandoffOutcome::Admitted { seq, artifact }
                if artifact_ref_has_label(artifact, TURN_INTERRUPTED_RESULT_LABEL) =>
            {
                let admission = DurableArtifactAdmission {
                    artifact: artifact.clone(),
                    movement_seq: *seq,
                };
                if matched.replace(admission).is_some() {
                    return Err(
                        "recovered exact-result source contains more than one interrupted-result closure"
                            .to_string(),
                    );
                }
            }
            ProviderHandoffOutcome::Admitted { .. } => {}
            ProviderHandoffOutcome::TerminalFailure {
                failure_seq,
                failure_class,
                ..
            } => {
                return Err(format!(
                    "recovered exact-result source ended in run-authoring failure '{failure_class}' at movement {failure_seq}"
                ));
            }
        }
    }
    Ok(matched)
}

fn artifact_ref_has_label(artifact: &ArtifactRef, label: &str) -> bool {
    artifact
        .id
        .as_str()
        .split_once(':')
        .map_or(artifact.id.as_str(), |(artifact_label, _)| artifact_label)
        == label
}

async fn recovered_codex_handoff_artifacts(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    outcomes: &[ProviderHandoffOutcome],
) -> Result<Arc<Vec<CodexRecoveredHandoffArtifact>>, String> {
    for outcome in outcomes {
        if let ProviderHandoffOutcome::TerminalFailure {
            failure_seq,
            failure_class,
            ..
        } = outcome
        {
            return Err(format!(
                "recovered provider source contains run-authoring failure '{failure_class}' at movement {failure_seq}"
            ));
        }
    }
    let mut admissions = None;
    for _ in 0..128 {
        let scan = scan_provider_transport_dispatch_admissions(ctx, run, source_movement_seq)?;
        if scan.complete_boundary {
            admissions = Some(scan.admissions);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    let admissions = admissions.ok_or_else(|| {
        format!(
            "movement ledger '{}' retained an incomplete tail while provider transport dispatch recovery was pending",
            ctx.movements_path.display()
        )
    })?;
    let mut recovered = Vec::new();
    for admission in admissions {
        let payload = read_pinned_provider_artifact_payload(
            ctx,
            &admission.artifact,
            "recovered provider transport dispatch",
        )?;
        let value: serde_json::Value = serde_json::from_slice(&payload).map_err(|error| {
            format!(
                "recovered provider transport dispatch '{}' is malformed: {error}",
                admission.artifact.id.as_str()
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            format!(
                "recovered provider transport dispatch '{}' is not a JSON object",
                admission.artifact.id.as_str()
            )
        })?;
        if object.get("label").and_then(serde_json::Value::as_str)
            != Some(PROVIDER_TRANSPORT_DISPATCH_LABEL)
        {
            return Err(format!(
                "recovered provider transport dispatch '{}' has a conflicting payload label",
                admission.artifact.id.as_str()
            ));
        }
        let provider = object
            .get("provider_interface")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| {
                format!(
                    "recovered provider transport dispatch '{}' has no canonical provider interface",
                    admission.artifact.id.as_str()
                )
            })?;
        if provider != ctx.interface.as_str() {
            return Err(format!(
                "recovered provider transport dispatch '{}' names provider '{}' instead of '{}'",
                admission.artifact.id.as_str(),
                provider,
                ctx.interface.as_str()
            ));
        }
        let dispatch_source = object
            .get("source_movement_seq")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                format!(
                    "recovered provider transport dispatch '{}' has no source movement coordinate",
                    admission.artifact.id.as_str()
                )
            })?;
        if dispatch_source == source_movement_seq {
            recovered.push(CodexRecoveredHandoffArtifact {
                movement_seq: admission.movement_seq,
                artifact: admission.artifact,
                payload,
            });
        }
    }
    Ok(Arc::new(recovered))
}

struct ProviderTransportDispatchAdmissionScan {
    admissions: Vec<DurableArtifactAdmission>,
    complete_boundary: bool,
}

fn scan_provider_transport_dispatch_admissions(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
) -> Result<ProviderTransportDispatchAdmissionScan, String> {
    let (mut file, snapshot_len, max_row_bytes) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (start_offset, expected_seq) = authoritative_source_scan_start(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        source_movement_seq,
    );
    let mut source_verified = false;
    let mut admissions = Vec::new();
    let mut seen = BTreeSet::new();
    let scan = scan_authoritative_snapshot(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        start_offset,
        expected_seq,
        None,
        |movement| {
            if movement.seq.0 < source_movement_seq {
                return Ok(None::<()>);
            }
            if movement.seq.0 == source_movement_seq {
                if !movement_is_provider_source(ctx, run, &movement) {
                    return Err(format!(
                        "provider transport-dispatch scan source movement {} does not match interface '{}' run '{}'",
                        source_movement_seq,
                        ctx.interface.as_str(),
                        run.as_str()
                    ));
                }
                source_verified = true;
                return Ok(None::<()>);
            }
            if !source_verified {
                return Err(format!(
                    "provider transport-dispatch scan advanced past causal source {source_movement_seq}"
                ));
            }
            let Some(artifact) = movement.artifact.as_ref() else {
                return Ok(None::<()>);
            };
            let runtime = movement
                .invocation_metadata
                .as_ref()
                .and_then(octopus_ledger::MovementMetadata::invocation)
                .and_then(octopus_ledger::InvocationMetadata::runtime);
            if movement.kind != MovementKind::InterfaceArrival
                || movement.from != Endpoint::Interface(ctx.interface.clone())
                || !runtime.is_some_and(|runtime| movement.to == Endpoint::Interface(runtime.worker.clone()))
                || !runtime.is_some_and(|runtime| {
                    &runtime.run == run && &runtime.interface == &ctx.interface
                })
                || !artifact_ref_has_label(artifact, PROVIDER_TRANSPORT_DISPATCH_LABEL)
            {
                return Ok(None::<()>);
            }
            if !seen.insert((artifact.id.as_str().to_string(), artifact.content.to_hex())) {
                return Err(format!(
                    "provider transport dispatch artifact '{}' has duplicate authoritative admissions",
                    artifact.id.as_str()
                ));
            }
            admissions.push(DurableArtifactAdmission {
                artifact: artifact.clone(),
                movement_seq: movement.seq.0,
            });
            Ok(None::<()>)
        },
    )?;
    ensure_arbitration_snapshot_length(ctx, &file, snapshot_len)?;
    Ok(ProviderTransportDispatchAdmissionScan {
        admissions,
        complete_boundary: scan.complete_boundary,
    })
}

fn interrupted_effect_journal_path(ctx: &InterfaceLiftContext, run: &RunId) -> PathBuf {
    let key = ContentHash::of_bytes(run.as_str().as_bytes()).to_hex();
    ctx.interface_var_dir()
        .join("interrupted-effect-journal")
        .join(format!("{key}.json"))
}

fn persist_json_file(
    path: &Path,
    value: &impl serde::Serialize,
    label: &str,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{label} path '{}' has no parent", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "{label} directory '{}' cannot be created: {error}",
            parent.display()
        )
    })?;
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("{label} could not be encoded: {error}"))?;
    bytes.push(b'\n');
    let temporary = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&temporary).map_err(|error| {
            format!(
                "{label} temporary file '{}' cannot be created: {error}",
                temporary.display()
            )
        })?;
        std::io::Write::write_all(&mut file, &bytes).map_err(|error| {
            format!(
                "{label} temporary file '{}' cannot be written: {error}",
                temporary.display()
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "{label} temporary file '{}' cannot be synced: {error}",
                temporary.display()
            )
        })?;
    }
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("{label} '{}' cannot be installed: {error}", path.display()))?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "{label} directory '{}' cannot be synced: {error}",
                parent.display()
            )
        })
}

fn deferred_provider_turn_dir(ctx: &InterfaceLiftContext) -> PathBuf {
    ctx.interface_var_dir().join("deferred-provider-turn")
}

fn deferred_provider_turn_path(ctx: &InterfaceLiftContext, source_movement_seq: u64) -> PathBuf {
    deferred_provider_turn_dir(ctx).join(format!("{source_movement_seq}.json"))
}

fn persist_deferred_provider_turn(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
) -> Result<(), String> {
    let mut journal = capture_deferred_provider_turn(ctx, run, turn)?;
    let path = deferred_provider_turn_path(ctx, turn.seq);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let current =
                serde_json::from_slice::<DeferredProviderTurnJournal>(&bytes).map_err(|error| {
                    format!(
                        "deferred provider turn journal '{}' is malformed: {error}",
                        path.display()
                    )
                })?;
            journal.release = current.release.clone();
            if current != journal {
                return Err(format!(
                    "deferred provider turn journal '{}' conflicts with movement {}",
                    path.display(),
                    turn.seq
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            persist_json_file(&path, &journal, "deferred provider turn journal")
        }
        Err(error) => Err(format!(
            "deferred provider turn journal '{}' cannot be read: {error}",
            path.display()
        )),
    }
}

fn capture_deferred_provider_turn(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
) -> Result<DeferredProviderTurnJournal, String> {
    let limits = ctx.required_host_io_limits()?;
    let captured = turn
        .envelope
        .artifact_file
        .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
        .map_err(|error| {
            format!(
                "deferred provider turn {} cannot capture its source artifact: {error}",
                turn.seq
            )
        })?;
    let metadata = turn.envelope.metadata.clone().ok_or_else(|| {
        format!(
            "deferred provider turn {} has no invocation metadata",
            turn.seq
        )
    })?;
    let runtime = metadata.runtime().ok_or_else(|| {
        format!(
            "deferred provider turn {} has no runtime coordinates",
            turn.seq
        )
    })?;
    if &runtime.run != run || &runtime.interface != &ctx.interface {
        return Err(format!(
            "deferred provider turn {} runtime coordinates do not match interface '{}' run '{}'",
            turn.seq,
            ctx.interface.as_str(),
            run.as_str()
        ));
    }
    let artifact_bindings = turn
        .envelope
        .artifact_binding_payloads
        .iter()
        .map(|payload| {
            let bytes = payload.read_bytes().map_err(|error| error.to_string())?;
            Ok(DeferredArtifactBindingRef {
                artifact: payload.artifact.clone(),
                bytes_content: ContentHash::of_bytes(bytes.as_ref()),
                byte_count: bytes.len() as u64,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let journal = DeferredProviderTurnJournal {
        schema_version: 3,
        source_movement_seq: turn.seq,
        run_id: run.as_str().to_string(),
        artifact: turn.envelope.artifact.clone(),
        metadata_content_hash: deferred_metadata_hash(&metadata)?,
        payload_content_hash: ContentHash::of_bytes(&captured.payload_bytes).to_hex(),
        payload_byte_count: captured.payload_bytes.len() as u64,
        artifact_bindings,
        release: None,
    };
    let canonical = restore_deferred_provider_envelope(ctx, &journal)?;
    let restored = canonical.artifact_file
        .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
        .map_err(|error| error.to_string())?;
    if captured.manifest_bytes != restored.manifest_bytes
        || captured.payload_bytes != restored.payload_bytes
        || canonical.metadata.as_ref() != Some(&metadata)
        || canonical.artifact_binding_payloads.len() != turn.envelope.artifact_binding_payloads.len()
    {
        return Err(format!("deferred provider turn {} differs from its canonical source bytes", turn.seq));
    }
    for (pinned, observed) in canonical.artifact_binding_payloads.iter().zip(&turn.envelope.artifact_binding_payloads) {
        if pinned.artifact != observed.artifact
            || pinned.read_bytes().map_err(|error| error.to_string())?.as_ref()
                != observed.read_bytes().map_err(|error| error.to_string())?.as_ref()
        {
            return Err(format!("deferred provider turn {} differs from its canonical source bytes", turn.seq));
        }
    }
    Ok(journal)
}

fn persist_deferred_provider_turn_release(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    release: &DeferredProviderTurnRelease,
) -> Result<(), String> {
    if let DeferredProviderTurnRelease::SessionRebase {
        rebased_session_carrier,
        ..
    } = release
    {
        validate_artifact_admission(ctx, run, turn.seq, rebased_session_carrier)?;
    }
    if let DeferredProviderTurnRelease::OriginalEnvelope {
        control_id,
        action,
        observed_ledger_next_seq,
    } = release
    {
        validate_original_envelope_release(
            ctx,
            descriptor,
            run,
            turn,
            control_id,
            action,
            *observed_ledger_next_seq,
        )?;
    }
    if let DeferredProviderTurnRelease::SettledEnvelope { observed_ledger_next_seq } = release {
        validate_settled_envelope_release(ctx, descriptor, run, turn, *observed_ledger_next_seq)?;
    }
    let path = deferred_provider_turn_path(ctx, turn.seq);
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "deferred provider turn journal '{}' cannot be read before release: {error}",
            path.display()
        )
    })?;
    let mut current =
        serde_json::from_slice::<DeferredProviderTurnJournal>(&bytes).map_err(|error| {
            format!(
                "deferred provider turn journal '{}' is malformed: {error}",
                path.display()
            )
        })?;
    let mut expected = capture_deferred_provider_turn(ctx, run, turn)?;
    expected.release = current.release.clone();
    if current != expected {
        return Err(format!(
            "deferred provider turn journal '{}' changed before release",
            path.display()
        ));
    }
    match current.release.as_ref() {
        Some(existing) if existing == release => return Ok(()),
        Some(_) => {
            return Err(format!(
                "deferred provider turn journal '{}' already names a different release carrier",
                path.display()
            ));
        }
        None => current.release = Some(release.clone()),
    }
    persist_json_file(&path, &current, "deferred provider turn journal")
}

fn deferred_metadata_hash(metadata: &octopus_ledger::InvocationMetadata) -> Result<String, String> {
    serde_json::to_vec(metadata)
        .map(|bytes| ContentHash::of_bytes(&bytes).to_hex())
        .map_err(|error| error.to_string())
}

fn read_deferred_binding_payload(
    ctx: &InterfaceLiftContext,
    binding: &DeferredArtifactBindingRef,
) -> Result<Vec<u8>, String> {
    let limits = ctx.required_host_io_limits()?;
    if binding.byte_count > limits.max_payload_bytes {
        return Err("deferred binding exceeds its captured payload contract".into());
    }
    let carrier = octopus_interfaces::read_artifact_carrier_file_bounded(
        &ctx.blobs_dir.join(binding.artifact.content.to_hex()),
        limits.max_manifest_bytes.max(limits.max_payload_bytes),
    ).map_err(|error| error.to_string())?;
    if ContentHash::of_bytes(&carrier) != binding.artifact.content {
        return Err("deferred binding carrier differs from its admitted reference".into());
    }
    let bytes = match serde_json::from_slice::<octopus_interfaces::InterfaceArtifactManifest>(&carrier) {
        Ok(manifest) if !manifest.payload_path.as_os_str().is_empty() => {
            if carrier.len() as u64 > limits.max_manifest_bytes || manifest.label.trim().is_empty()
                || !manifest.payload_path.is_absolute()
                || manifest.payload_content_hash.as_deref().and_then(ContentHash::from_hex) != Some(binding.bytes_content)
            {
                return Err("deferred binding bytes differ from their admitted manifest payload pin".into());
            }
            octopus_interfaces::read_artifact_carrier_file_bounded(
                &ctx.blobs_dir.join(binding.bytes_content.to_hex()), limits.max_payload_bytes,
            ).map_err(|error| error.to_string())?
        }
        _ => carrier,
    };
    if bytes.len() as u64 != binding.byte_count || ContentHash::of_bytes(&bytes) != binding.bytes_content {
        return Err("deferred binding bytes differ from their admitted payload digest or length".into());
    }
    Ok(bytes)
}

fn restore_deferred_provider_envelope(
    ctx: &InterfaceLiftContext,
    journal: &DeferredProviderTurnJournal,
) -> Result<InterfaceInvocationEnvelope, String> {
    if journal.schema_version != 3 || journal.run_id.trim().is_empty() {
        return Err("deferred provider receipt has an unsupported schema or empty run".into());
    }
    let run = RunId::new(journal.run_id.clone());
    let source = read_provider_source_movement(ctx, &run, journal.source_movement_seq)?;
    let metadata = source.invocation_metadata.as_ref()
        .and_then(octopus_ledger::MovementMetadata::invocation)
        .ok_or("deferred provider source has no original invocation metadata")?;
    if source.artifact.as_ref() != Some(&journal.artifact)
        || deferred_metadata_hash(metadata)? != journal.metadata_content_hash
        || metadata.artifact_binding_refs.len() != journal.artifact_bindings.len()
    {
        return Err("deferred provider receipt differs from its exact admitted source or grant".into());
    }
    let limits = ctx.required_host_io_limits()?;
    let manifest_path = ctx.blobs_dir.join(journal.artifact.content.to_hex());
    let manifest_bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
        &manifest_path, limits.max_manifest_bytes,
    ).map_err(|error| error.to_string())?;
    if ContentHash::of_bytes(&manifest_bytes) != journal.artifact.content {
        return Err("deferred provider manifest differs from its admitted reference".into());
    }
    let manifest: octopus_interfaces::InterfaceArtifactManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("deferred provider manifest is malformed: {error}"))?;
    let payload_hash = manifest.payload_content_hash.as_deref().and_then(ContentHash::from_hex)
        .ok_or("deferred provider manifest has no canonical payload pin")?;
    if !manifest.payload_path.is_absolute() || manifest.label.trim().is_empty()
        || !artifact_ref_has_label(&journal.artifact, &manifest.label)
        || payload_hash.to_hex() != journal.payload_content_hash
        || journal.payload_byte_count > limits.max_payload_bytes
    {
        return Err("deferred provider receipt differs from its admitted manifest payload pin".into());
    }
    let payload_bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
        &ctx.blobs_dir.join(payload_hash.to_hex()), limits.max_payload_bytes,
    ).map_err(|error| error.to_string())?;
    if payload_bytes.len() as u64 != journal.payload_byte_count
        || ContentHash::of_bytes(&payload_bytes) != payload_hash
    {
        return Err("deferred provider payload differs from its admitted digest or length".into());
    }
    let artifact_file = octopus_interfaces::InterfaceArtifactFile::from_captured_snapshot(
        manifest_path, manifest_bytes, payload_bytes,
    ).map_err(|error| error.to_string())?;
    let mut artifact_binding_payloads = Vec::with_capacity(journal.artifact_bindings.len());
    for (expected, binding) in metadata.artifact_binding_refs.iter().zip(&journal.artifact_bindings) {
        if expected != &binding.artifact {
            return Err("deferred provider receipt changed its ordered artifact bindings".into());
        }
        artifact_binding_payloads.push(octopus_interfaces::ArtifactBindingPayload::new(
            binding.artifact.clone(), read_deferred_binding_payload(ctx, binding)?,
        ));
    }
    Ok(InterfaceInvocationEnvelope {
        artifact: journal.artifact.clone(), artifact_file, artifact_binding_payloads,
        metadata: Some(metadata.clone()),
    })
}

fn deferred_provider_journal_entries(ctx: &InterfaceLiftContext) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let directory = deferred_provider_turn_dir(ctx);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("deferred provider turn directory '{}' cannot be read: {error}", directory.display())),
    };
    let mut journals = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("tmp") { continue; }
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if !kind.is_file() || kind.is_symlink()
            || path.extension().and_then(|extension| extension.to_str()) != Some("json")
        {
            return Err(format!("deferred provider turn entry '{}' is not a regular JSON journal", path.display()));
        }
        let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
        journals.push((path, bytes));
    }
    journals.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(journals)
}

fn deferred_turn_from_journal(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    path: &Path,
    journal: &DeferredProviderTurnJournal,
) -> Result<DeferredProviderTurn, String> {
    if path != deferred_provider_turn_path(ctx, journal.source_movement_seq) {
        return Err(format!("deferred provider turn journal '{}' has a mismatched identity", path.display()));
    }
    let run = RunId::new(journal.run_id.clone());
    let envelope = restore_deferred_provider_envelope(ctx, journal)?;
    if let Some(DeferredProviderTurnRelease::SessionRebase { rebased_session_carrier, .. }) = &journal.release {
        validate_artifact_admission(ctx, &run, journal.source_movement_seq, rebased_session_carrier)?;
    }
    if !is_provider_turn_input(descriptor, &envelope).map_err(|error| error.to_string())? {
        return Err(format!("deferred provider turn journal '{}' does not preserve a new provider turn", path.display()));
    }
    Ok(DeferredProviderTurn {
        run, turn: QueuedTurn { envelope, seq: journal.source_movement_seq }, release: journal.release.clone(),
    })
}

fn publish_deferred_provider_journal_upgrade(
    path: &Path,
    original: &[u8],
    upgraded: &DeferredProviderTurnJournal,
) -> Result<(), String> {
    let parent = path.parent().ok_or("deferred provider upgrade journal has no parent")?;
    let mut bytes = serde_json::to_vec_pretty(upgraded).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?.as_nanos();
    let temporary = path.with_extension(format!("upgrade-{}-{nonce}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)
        .map_err(|error| format!("deferred provider upgrade temporary file cannot be created: {error}"))?;
    let mut published = false;
    let result = (|| {
        std::io::Write::write_all(&mut file, &bytes)
            .and_then(|()| file.sync_all()).map_err(|error| error.to_string())?;
        let current = octopus_interfaces::read_artifact_carrier_file_bounded(path, original.len() as u64)
            .map_err(|error| error.to_string())?;
        if current != original {
            return Err("deferred provider v2 journal changed during its checked upgrade".into());
        }
        std::fs::rename(&temporary, path).map_err(|error| error.to_string())?;
        published = true;
        std::fs::File::open(parent).and_then(|directory| directory.sync_all())
            .map_err(|error| format!("deferred provider v3 upgrade was published but directory sync failed: {error}"))
    })();
    drop(file);
    if let Err(error) = result {
        if !published {
            std::fs::remove_file(&temporary).map_err(|cleanup|
                format!("{error}; unpublished deferred upgrade temporary file could not be removed: {cleanup}"))?;
        }
        return Err(error);
    }
    Ok(())
}

async fn upgrade_deferred_provider_turn_journals(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct Version { schema_version: u32 }
    for (path, bytes) in deferred_provider_journal_entries(ctx)? {
        let version: Version = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        match version.schema_version {
            3 => continue,
            2 => {}
            version => return Err(format!("deferred provider journal '{}' has unsupported schema {version}", path.display())),
        }
        let old: DeferredProviderTurnJournalV2 = serde_json::from_slice(&bytes)
            .map_err(|error| format!("deferred provider v2 journal '{}' is malformed: {error}", path.display()))?;
        if old.run_id.trim().is_empty() || path != deferred_provider_turn_path(ctx, old.source_movement_seq)
            || old.metadata.artifact_binding_refs.len() != old.artifact_binding_payloads.len()
        {
            return Err("deferred provider v2 journal has a mismatched source identity".into());
        }
        let run = RunId::new(old.run_id);
        validate_provider_source_artifact(ctx, &run, old.source_movement_seq, &old.artifact, &old.metadata)?;
        let artifact_file = octopus_interfaces::InterfaceArtifactFile::from_captured_snapshot(
            old.manifest_path, old.manifest_bytes, old.payload_bytes,
        ).map_err(|error| error.to_string())?;
        let mut artifact_binding_payloads = Vec::with_capacity(old.artifact_binding_payloads.len());
        for (reference, binding) in old.metadata.artifact_binding_refs.iter().zip(old.artifact_binding_payloads) {
            if reference != &binding.artifact || ContentHash::of_bytes(&binding.bytes) != binding.bytes_content {
                return Err("deferred provider v2 journal changed a captured binding".into());
            }
            artifact_binding_payloads.push(octopus_interfaces::ArtifactBindingPayload::new(binding.artifact, binding.bytes));
        }
        let turn = QueuedTurn { seq: old.source_movement_seq, envelope: InterfaceInvocationEnvelope {
            artifact: old.artifact, artifact_file, artifact_binding_payloads, metadata: Some(old.metadata),
        }};
        let mut upgraded = capture_deferred_provider_turn(ctx, &run, &turn)?;
        upgraded.release = old.release;
        let deferred = deferred_turn_from_journal(ctx, descriptor, &path, &upgraded)?;
        if deferred.release.is_some() && !matches!(
            prepare_deferred_provider_turn(ctx, descriptor, &run, &deferred, &ActiveTurnControls::default()).await?,
            DeferredTurnReadiness::Ready(_)
        ) {
            return Err("deferred provider v2 release has no settled canonical proof".into());
        }
        publish_deferred_provider_journal_upgrade(&path, &bytes, &upgraded)?;
    }
    Ok(())
}

fn load_deferred_provider_turns(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
) -> Result<BTreeMap<u64, DeferredProviderTurn>, String> {
    let mut turns = BTreeMap::new();
    for (path, bytes) in deferred_provider_journal_entries(ctx)? {
        let journal: DeferredProviderTurnJournal = serde_json::from_slice(&bytes)
            .map_err(|error| format!("deferred provider v3 journal '{}' is malformed: {error}", path.display()))?;
        let turn = deferred_turn_from_journal(ctx, descriptor, &path, &journal)?;
        if turns.insert(journal.source_movement_seq, turn).is_some() {
            return Err(format!("deferred provider turn movement {} is journaled more than once", journal.source_movement_seq));
        }
    }
    Ok(turns)
}

fn remove_deferred_provider_turn(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
) -> Result<(), String> {
    let path = deferred_provider_turn_path(ctx, source_movement_seq);
    std::fs::remove_file(&path).map_err(|error| {
        format!(
            "deferred provider turn journal '{}' cannot be removed after admission: {error}",
            path.display()
        )
    })?;
    let parent = path.parent().ok_or_else(|| {
        format!(
            "deferred provider turn journal '{}' has no parent directory",
            path.display()
        )
    })?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "deferred provider turn directory '{}' cannot be synced: {error}",
                parent.display()
            )
        })
}

enum DeferredTurnReadiness {
    AwaitingSessionSettlement,
    Ready(QueuedTurn),
}

fn operator_turn_requires_deferral(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    current: &[octopus_provider_run_authoring::SessionContextArtifact],
) -> Result<bool, String> {
    let source_snapshot = bound_operator_session_snapshot(ctx, descriptor, turn)?;
    let continuity = recover_provider_turn_continuity(
        descriptor,
        &turn.envelope,
        source_snapshot.as_ref().map(|(_, snapshot)| snapshot),
        run,
        &ctx.pinned_run_record_bytes(&turn.envelope)?,
    )
    .map_err(|error| error.to_string())?;
    let continuity = reconcile_operator_session_continuity(
        descriptor, &turn.envelope, continuity, current,
    ).map_err(|error| error.to_string())?;
    if continuity
        .as_ref()
        .is_some_and(codex_continuity_waits_for_result)
    {
        return Ok(true);
    }
    let labels = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
    latest_provider_session_carrier(ctx, run, turn.seq, &labels, None)
        .map(|scan| scan.latest.is_some())
}

async fn wait_for_provider_handoff(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    session: &ProviderHandoffSession,
    run: &RunId,
    input: &InterfaceInvocationEnvelope,
    source_movement_seq: u64,
    pinned: &[u8],
) -> Result<ProviderOutputHandoff, String> {
    loop {
        if ctx.cancel.is_cancelled() { return Err("provider input preparation was cancelled".into()); }
        match session.from_invocation_when_ready(ctx, descriptor, run, input, source_movement_seq, pinned).await {
            Ok(handoff) => return Ok(handoff),
            Err(error @ (CheckedRunError::AwaitingResults { .. } | CheckedRunError::ChangedDuringRead { .. })) => {
                tracing::warn!(interface = ctx.interface.as_str(), run = run.as_str(), seq = source_movement_seq,
                    error = %error, "provider source still awaits checked run evidence");
            }
            Err(CheckedRunError::Invalid(error)) => return Err(error),
        }
    }
}

async fn operator_turn_requires_deferral_with_context(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
) -> Result<bool, String> {
    let independent = independent_provider_input(ctx, turn);
    if matches!(independent, Ok(Some(_))) && input_awaits_open_handoff(ctx, run, turn)? {
        return Ok(true);
    }
    if matches!(independent, Ok(Some(true))) {
        return Ok(false);
    }
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let handoff = wait_for_provider_handoff(
        ctx, descriptor, session, run, &turn.envelope, turn.seq, &pinned,
    ).await.map_err(|error| error.to_string())?;
    operator_turn_requires_deferral(
        ctx, descriptor, run, turn, &handoff.current_session_context_artifacts().await,
    )
}

fn input_awaits_open_handoff(ctx: &InterfaceLiftContext, run: &RunId, turn: &QueuedTurn) -> Result<bool, String> {
    let path = ctx.root_slot("run_records_root")?.join(format!("{}.json", run.as_str()));
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("provider run '{}' current record cannot be read: {error}", run.as_str()))?;
    let record = octopus_runtime::run_record::RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
    octopus_provider_run_authoring::open_handoff_before(&record, &ctx.interface, turn.seq)
}

fn independent_provider_input(ctx: &InterfaceLiftContext, turn: &QueuedTurn) -> Result<Option<bool>, String> {
    let record = octopus_runtime::run_record::RunRecord::from_bytes(&ctx.pinned_run_record_bytes(&turn.envelope)?)
        .map_err(|error| error.to_string())?;
    let metadata = turn.envelope.metadata().map_err(|error| error.to_string())?;
    let runtime = metadata.runtime.as_ref().ok_or("provider input has no runtime identity")?;
    octopus_provider_run_authoring::independent_provider_input(&record, &ctx.interface, &runtime.checkpoint)
}

async fn prepare_deferred_provider_turn(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    deferred: &DeferredProviderTurn,
    active_controls: &ActiveTurnControls,
) -> Result<DeferredTurnReadiness, String> {
    if active_control(active_controls, run).await.is_some_and(|broker| !broker.is_finished()) {
        return Ok(DeferredTurnReadiness::AwaitingSessionSettlement);
    }
    if deferred.release.is_none() && matches!(independent_provider_input(ctx, &deferred.turn), Ok(Some(true))) {
        return Ok(if input_awaits_open_handoff(ctx, run, &deferred.turn)? {
            DeferredTurnReadiness::AwaitingSessionSettlement
        } else {
            DeferredTurnReadiness::Ready(deferred.turn.clone())
        });
    }
    let source_snapshot = bound_operator_session_snapshot(ctx, descriptor, &deferred.turn)?;
    let source_session_carrier = source_snapshot.as_ref().map(|(carrier, _)| carrier.clone());
    let source_continuity = recover_provider_turn_continuity(
        descriptor,
        &deferred.turn.envelope,
        source_snapshot.as_ref().map(|(_, snapshot)| snapshot),
        run,
        &ctx.pinned_run_record_bytes(&deferred.turn.envelope)?,
    )
    .map_err(|error| error.to_string())?;
    if let Some(release) = deferred.release.as_ref() {
        match release {
            DeferredProviderTurnRelease::SettledEnvelope { observed_ledger_next_seq } => {
                validate_settled_envelope_release(ctx, descriptor, run, &deferred.turn, *observed_ledger_next_seq)?;
                return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
            }
            DeferredProviderTurnRelease::InterruptedResult { source_session_carrier: released_source, .. } => {
                if released_source != &source_session_carrier {
                    return Err(format!("deferred provider turn {} changed its interrupted-result source carrier", deferred.turn.seq));
                }
                return Ok(if checked_deferred_interrupted_result(
                    ctx, descriptor, run, &deferred.turn, source_continuity.as_ref(), Some(release), None,
                ).await?.is_some() {
                    DeferredTurnReadiness::Ready(deferred.turn.clone())
                } else { DeferredTurnReadiness::AwaitingSessionSettlement });
            }
            DeferredProviderTurnRelease::RecoveredResult {
                source_session_carrier: released_source,
                failed_source_movement_seq,
            } => {
                if released_source != &source_session_carrier
                    || recover_deferred_failed_result(ctx, descriptor, run, &deferred.turn, source_continuity.as_ref(), Some(*failed_source_movement_seq)).await?
                        != Some(*failed_source_movement_seq)
                {
                    return Err(format!("deferred provider turn {} lost its exact completed-result recovery evidence", deferred.turn.seq));
                }
                return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
            }
            DeferredProviderTurnRelease::SessionRebase {
                source_session_carrier: released_source,
                ..
            } => {
                if released_source != &source_session_carrier {
                    return Err(format!(
                        "deferred provider turn {} release names a different source session carrier",
                        deferred.turn.seq
                    ));
                }
                let (rebased, continuity) =
                    rebase_deferred_provider_turn(ctx, descriptor, run, &deferred.turn, release)?;
                if !codex_continuity_successor_is_settled(
                    source_continuity.as_ref(),
                    continuity.as_ref(),
                )? {
                    return Err(format!(
                        "deferred provider turn {} persisted a release whose continuity is still pending",
                        deferred.turn.seq
                    ));
                }
                return Ok(DeferredTurnReadiness::Ready(rebased));
            }
            DeferredProviderTurnRelease::OriginalEnvelope {
                control_id,
                action,
                observed_ledger_next_seq,
            } => {
                validate_original_envelope_release(
                    ctx,
                    descriptor,
                    run,
                    &deferred.turn,
                    control_id,
                    action,
                    *observed_ledger_next_seq,
                )?;
                if source_continuity
                    .as_ref()
                    .is_some_and(codex_continuity_waits_for_result)
                {
                    return Err(format!(
                        "deferred provider turn {} released its original envelope while continuity is pending",
                        deferred.turn.seq
                    ));
                }
                return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
            }
        }
    }

    if let Some((release, _)) = checked_deferred_interrupted_result(
        ctx, descriptor, run, &deferred.turn, source_continuity.as_ref(), None, None,
    ).await? {
        persist_deferred_provider_turn_release(ctx, descriptor, run, &deferred.turn, &release)?;
        return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
    }

    let labels = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
    let scan = latest_provider_session_carrier(ctx, run, deferred.turn.seq, &labels, None)?;
    let has_carrier = scan.latest.is_some();
    if let Some(rebased_session_carrier) = scan.latest {
        let release = DeferredProviderTurnRelease::SessionRebase {
            source_session_carrier: source_session_carrier.clone(), rebased_session_carrier,
        };
        let (rebased, continuity) = rebase_deferred_provider_turn(ctx, descriptor, run, &deferred.turn, &release)?;
        if codex_continuity_successor_is_settled(source_continuity.as_ref(), continuity.as_ref())? {
            persist_deferred_provider_turn_release(ctx, descriptor, run, &deferred.turn, &release)?;
            return Ok(DeferredTurnReadiness::Ready(rebased));
        }
    }
    if let Some(failed_source_movement_seq) = recover_deferred_failed_result(
        ctx, descriptor, run, &deferred.turn, source_continuity.as_ref(), None,
    ).await? {
        let release = DeferredProviderTurnRelease::RecoveredResult {
            source_session_carrier, failed_source_movement_seq,
        };
        persist_deferred_provider_turn_release(ctx, descriptor, run, &deferred.turn, &release)?;
        return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
    }
    if !has_carrier && scan.complete_boundary
        && !source_continuity.as_ref().is_some_and(codex_continuity_waits_for_result)
        && let Some((control_id, action)) = terminal_deferred_turn_ownership(
            ctx, descriptor, run, &deferred.turn.envelope, deferred.turn.seq,
        )?
    {
        let release = DeferredProviderTurnRelease::OriginalEnvelope {
            control_id, action, observed_ledger_next_seq: scan.next_seq,
        };
        persist_deferred_provider_turn_release(ctx, descriptor, run, &deferred.turn, &release)?;
        return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
    }
    if let Some(observed_ledger_next_seq) = settled_envelope_prefix(ctx, descriptor, run, &deferred.turn, None)? {
        let release = DeferredProviderTurnRelease::SettledEnvelope { observed_ledger_next_seq };
        persist_deferred_provider_turn_release(ctx, descriptor, run, &deferred.turn, &release)?;
        return Ok(DeferredTurnReadiness::Ready(deferred.turn.clone()));
    }
    Ok(DeferredTurnReadiness::AwaitingSessionSettlement)
}

fn settled_envelope_prefix(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    stop_before_seq: Option<u64>,
) -> Result<Option<u64>, String> {
    if stop_before_seq.is_some_and(|stop| stop <= turn.seq) {
        return Err("settled input release has an invalid ledger boundary".into());
    }
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let record = octopus_runtime::run_record::RunRecord::from_bytes(&pinned).map_err(|error| error.to_string())?;
    if record.id != *run || record.status != serde_json::json!("idle") { return Ok(None); }
    let Some(pending) = record.interface_state.get(&ctx.interface).and_then(|state| state.get("pending_handoff")) else {
        return Ok(None);
    };
    let terminal = pending.get("terminal").and_then(serde_json::Value::as_bool).ok_or("settled input has malformed source finality")?;
    let prior = pending.get("source_movement_seq").and_then(serde_json::Value::as_u64).ok_or("settled input has malformed source ownership")?;
    if !terminal || prior >= turn.seq { return Ok(None); }
    let snapshot = bound_operator_session_snapshot(ctx, descriptor, turn)?;
    let continuity = recover_provider_turn_continuity(descriptor, &turn.envelope,
        snapshot.as_ref().map(|(_, snapshot)| snapshot), run, &pinned).map_err(|error| error.to_string())?;
    if continuity.as_ref().is_some_and(codex_continuity_waits_for_result) { return Ok(None); }
    let (mut file, length, limit) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (offset, expected) = authoritative_source_scan_start(ctx, &mut file, length, limit, &decoder, turn.seq);
    let mut source_verified = false;
    let mut advanced = false;
    let mut next_seq = turn.seq;
    let scan = scan_authoritative_snapshot(ctx, &mut file, length, limit, &decoder, offset, expected, stop_before_seq, |movement| {
        next_seq = movement.seq.0.checked_add(1).ok_or("settled input ledger sequence overflowed")?;
        if movement.seq.0 < turn.seq { return Ok(None::<()>); }
        if movement.seq.0 == turn.seq {
            if !movement_is_provider_source(ctx, run, &movement) || movement.artifact.as_ref() != Some(&turn.envelope.artifact) {
                return Err("settled input release changed its admitted source".into());
            }
            source_verified = true;
        } else if movement.from == Endpoint::Interface(ctx.interface.clone())
            && movement.runtime().is_some_and(|runtime| &runtime.run == run)
        {
            advanced = true;
        }
        Ok(None)
    })?;
    ensure_arbitration_snapshot_length(ctx, &file, length)?;
    if !source_verified {
        return Err("settled input release has no complete authoritative source prefix".into());
    }
    if !scan.complete_boundary && stop_before_seq.is_none() { return Ok(None); }
    if !scan.complete_boundary || stop_before_seq.is_some_and(|stop| stop != next_seq) {
        return Err("settled input release has no complete authoritative source prefix".into());
    }
    Ok((!advanced).then_some(next_seq))
}

fn validate_settled_envelope_release(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    observed_ledger_next_seq: u64,
) -> Result<(), String> {
    if settled_envelope_prefix(ctx, descriptor, run, turn, Some(observed_ledger_next_seq))? != Some(observed_ledger_next_seq) {
        return Err("deferred input was not settled at its recorded release boundary".into());
    }
    Ok(())
}

async fn checked_deferred_interrupted_result(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    source: Option<&crate::handler::CodexContinuityState>,
    released: Option<&DeferredProviderTurnRelease>,
    current: Option<&[octopus_provider_run_authoring::SessionContextArtifact]>,
) -> Result<Option<(DeferredProviderTurnRelease, crate::handler::CodexContinuityState)>, String> {
    let Some(source) = source.filter(|state| codex_continuity_has_interrupted_pending(state)) else { return Ok(None); };
    let snapshot = bound_operator_session_snapshot(ctx, descriptor, turn)?;
    let carrier = snapshot.as_ref().map(|(carrier, _)| carrier.clone());
    let (result_source, closure) = match released {
        Some(DeferredProviderTurnRelease::InterruptedResult { source_session_carrier, result_source, closure }) => {
            if source_session_carrier != &carrier { return Err("interrupted release changed its source session carrier".into()); }
            (result_source.clone(), closure.clone())
        }
        Some(_) => return Err("interrupted release has a different release kind".into()),
        None => {
            let Some((interrupted, seq, closure)) = InterruptedEffectArbiter::new(ctx.clone()).reconciled_result(run).await? else { return Ok(None); };
            if &interrupted != source { return Ok(None); }
            let movement = read_provider_source_movement(ctx, run, seq)?;
            (DurableArtifactAdmission { artifact: movement.artifact.ok_or("interrupted result source has no artifact")?, movement_seq: seq }, closure)
        }
    };
    let checked;
    let current = if let Some(current) = current { current } else {
        let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
        let session = ProviderHandoffSession::new(ctx)?;
        let handoff = wait_for_provider_handoff(ctx, descriptor, &session, run, &turn.envelope, turn.seq, &pinned).await?;
        checked = handoff.current_session_context_artifacts().await;
        &checked
    };
    let continuity = checked_interrupted_result_continuity(
        ctx, descriptor, run, &turn.envelope, source, &result_source, &closure,
        current,
    ).await?;
    if !codex_continuity_successor_is_settled(Some(source), Some(&continuity))? { return Ok(None); }
    Ok(Some((DeferredProviderTurnRelease::InterruptedResult {
        source_session_carrier: carrier, result_source, closure,
    }, continuity)))
}

async fn checked_interrupted_result_continuity(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    input: &InterfaceInvocationEnvelope,
    interrupted: &crate::handler::CodexContinuityState,
    result_source: &DurableArtifactAdmission,
    closure: &DurableArtifactAdmission,
    current: &[octopus_provider_run_authoring::SessionContextArtifact],
) -> Result<crate::handler::CodexContinuityState, String> {
    let result = read_provider_source_movement(ctx, run, result_source.movement_seq)?;
    if result.artifact.as_ref() != Some(&result_source.artifact) {
        return Err("interrupted release result ref differs from its exact source movement".into());
    }
    let runtime = input.runtime().map_err(|error| error.to_string())?;
    let result_runtime = result.runtime().ok_or("interrupted release result has no runtime coordinates")?;
    if result_runtime.ee != runtime.ee || result_runtime.worker != runtime.worker {
        return Err("interrupted release result crossed its admitted run scope".into());
    }
    validate_artifact_admission(ctx, run, result_source.movement_seq, closure)?;
    let (result_manifest, result_payload) = read_pinned_provider_artifact(ctx, &result_source.artifact, "interrupted result")?;
    if !exact_interrupted_result_payload(&result_manifest.label, &result_payload, interrupted)? {
        return Err("interrupted release result does not close its exact native pending call".into());
    }
    let (closure_manifest, closure_payload) = read_pinned_provider_artifact(ctx, &closure.artifact, "interrupted closure")?;
    let labels = codex_context_output_labels(descriptor).map_err(|error| error.to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&closure_payload).map_err(|error| error.to_string())?;
    let settlement_source = if let Some(evidence) = value.get("completed_source_settlement") {
        let turn = replay_provider_source(ctx, run, result_source.movement_seq)?
            .ok_or("completed interrupted release lost its admitted result input")?;
        if turn.envelope.artifact != result_source.artifact {
            return Err("completed interrupted release changed its result input".into());
        }
        let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
        let session = ProviderHandoffSession::new(ctx)?;
        let handoff = wait_for_provider_handoff(ctx, descriptor, &session, run, &turn.envelope, turn.seq, &pinned).await?;
        let (record, outcomes) = handoff.completed_source_settlement_predecessor(&closure.artifact).await?;
        let [ProviderHandoffOutcome::Admitted { seq, .. }] = outcomes.as_slice()
            else { return Err("completed interrupted release has no single admitted predecessor".into()); };
        let context = current.iter().filter(|entry| entry.source_movement_seq.is_some_and(|value| value <= *seq)).cloned().collect::<Vec<_>>();
        let acknowledgement: octopus_ledger::ArtifactRef = serde_json::from_value(evidence["stop_artifact"].clone()).map_err(|error| error.to_string())?;
        let acknowledgement_seq = evidence["stop_movement_seq"].as_u64().ok_or("completed interrupted release has no Stop ACK sequence")?;
        let control_source = evidence["stop_source_movement_seq"].as_u64().ok_or("completed interrupted release has no Stop input sequence")?;
        let ack = context.iter().find(|entry| entry.source_artifact == acknowledgement && entry.source_movement_seq == Some(acknowledgement_seq))
            .ok_or("completed interrupted release has no exact checked Stop ACK")?;
        let control_id = ack.payload["control_id"].as_str().ok_or("completed interrupted release has no Stop identity")?;
        let proof = crate::handler::stopped_result::prove_interrupted(descriptor, &turn.envelope, turn.seq,
            &record, &context, &outcomes, &crate::handler::stopped_result::InterruptedResultSource {
                continuity: interrupted, control_source, control_id, acknowledgement: &acknowledgement, acknowledgement_seq,
            }).map_err(|error| error.to_string())?;
        validate_completed_interrupted_result_control(ctx, descriptor, run, interrupted, &proof)?;
        crate::handler::stopped_result::validate_saved_settlement(&proof, &turn.envelope, &value, &labels)?;
        Some(proof.source)
    } else { None };
    let checked = crate::handler::checked_interrupted_context_revision(interrupted, current, &labels, Some(closure.movement_seq))?;
    let closed = interrupted_result_closure_payload(&closure_manifest.label, &closure_payload,
        settlement_source.as_ref().unwrap_or(&checked), &result_source.artifact, &labels)?
        .ok_or("interrupted release lost its exact native closure payload")?;
    let mut evidence = current.to_vec();
    let result_payload: serde_json::Value = serde_json::from_slice(&result_payload).map_err(|error| error.to_string())?;
    if let Some(existing) = evidence.iter().find(|entry| entry.source_artifact == result_source.artifact) {
        if existing.payload != result_payload { return Err("interrupted release result conflicts with checked current evidence".into()); }
    } else {
        evidence.push(octopus_provider_run_authoring::SessionContextArtifact {
            label: result_manifest.label,
            payload: result_payload, source_artifact: result_source.artifact.clone(),
            source_movement_seq: Some(result_source.movement_seq),
        });
    }
    let settled = reconcile_operator_session_continuity(descriptor, input, Some(interrupted.clone()), &evidence)
        .map_err(|error| error.to_string())?;
    if settled.as_ref().is_some_and(codex_continuity_waits_for_result) {
        return settled.ok_or("interrupted release lost its current continuity".into());
    }
    reconcile_delivered_interrupted_continuity(Some(interrupted), settled, Some(closed))
        .map_err(|error| error.to_string())?.ok_or("interrupted release lost its checked closed continuity".into())
}

fn read_provider_source_movement(
    ctx: &InterfaceLiftContext, run: &RunId, seq: u64,
) -> Result<octopus_ledger::Movement, String> {
    let (mut file, snapshot_len, max_row_bytes) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (offset, expected_seq) = authoritative_source_scan_start(ctx, &mut file, snapshot_len, max_row_bytes, &decoder, seq);
    let scan = scan_authoritative_snapshot(ctx, &mut file, snapshot_len, max_row_bytes, &decoder, offset, expected_seq, None,
        |movement| Ok((movement.seq.0 == seq).then_some(movement)))?;
    ensure_arbitration_snapshot_length(ctx, &file, snapshot_len)?;
    let movement = scan.found.ok_or_else(|| format!("interrupted result source {seq} is absent from the authoritative ledger"))?;
    if !movement_is_provider_source(ctx, run, &movement) { return Err("interrupted result source crossed its provider/run coordinates".into()); }
    Ok(movement)
}

async fn recover_deferred_failed_result(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    continuity: Option<&crate::handler::CodexContinuityState>,
    released_source: Option<u64>,
) -> Result<Option<u64>, String> {
    let Some(continuity) = continuity.filter(|state| codex_continuity_waits_for_result(state)) else { return Ok(None); };
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let record = octopus_runtime::run_record::RunRecord::from_bytes(&pinned).map_err(|error| error.to_string())?;
    if !record.interface_state.get(&descriptor.id).is_some_and(|state| state.contains_key("run_authoring_binding")) {
        return Ok(None);
    }
    let session = ProviderHandoffSession::new(ctx)?;
    let handoff = wait_for_provider_handoff(ctx, descriptor, &session, run, &turn.envelope, turn.seq, &pinned).await?;
    let failed_sources = match released_source {
        Some(sequence) if handoff.recovered_failed_context_source(sequence).await? => vec![sequence],
        Some(_) => return Err("released failed result lost its checked recovery proof".into()),
        None => handoff.recovered_failed_context_sources().await?,
    };
    if failed_sources.is_empty() { return Ok(None); }
    let context = handoff.current_session_context_artifacts().await;
    let mut matched = None;
    for failed_source in failed_sources {
        let boundary = turn.seq.max(failed_source.checked_add(1).ok_or("failed result sequence overflowed")?);
        let settled_context = context.iter().filter(|entry| entry.source_artifact != turn.envelope.artifact
            && entry.source_movement_seq.is_some_and(|seq| seq < boundary)).cloned().collect::<Vec<_>>();
        if let Some(source) = recovered_failed_result_source(descriptor, &turn.envelope, continuity, &settled_context, &[failed_source])
            .map_err(|error| error.to_string())?
            && matched.replace(source).is_some()
        {
            return Err("deferred turn has multiple failed sources for one pending result".into());
        }
    }
    Ok(matched)
}

fn bound_operator_session_snapshot(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    turn: &QueuedTurn,
) -> Result<Option<(ArtifactRef, ProviderInvocationSessionSnapshot)>, String> {
    let labels = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
    let mut matched = None;
    let record = octopus_runtime::run_record::RunRecord::from_bytes(&ctx.pinned_run_record_bytes(&turn.envelope)?)
        .map_err(|error| error.to_string())?;
    let payloads = octopus_provider_run_authoring::context_artifact_payloads(
        &record, turn.envelope.metadata().map_err(|error| error.to_string())?, &turn.envelope.artifact_binding_payloads,
    )?;
    let payloads = octopus_model_context_render::resolve_instruction_context(
        turn.envelope.metadata().map_err(|error| error.to_string())?, &payloads,
    )?.remaining_payloads;
    for bound in &payloads {
        let bytes = bound.read_bytes().map_err(|error| error.to_string())?;
        let value: serde_json::Value = serde_json::from_slice(bytes.as_ref())
            .map_err(|error| format!("deferred bound context '{}' is not JSON: {error}", bound.artifact.id))?;
        let Some(session) = value.get("session_context") else {
            continue;
        };
        if session
            .get("provider_interface")
            .and_then(serde_json::Value::as_str)
            != Some(ctx.interface.as_str())
        {
            continue;
        }
        let artifacts = resolve_session_snapshot_chain(
            &ctx.blobs_dir,
            ctx.required_host_io_limits()?,
            &bound.artifact,
            bytes.as_ref(),
            None,
            &labels,
            &ctx.interface,
        )?;
        let snapshot = ProviderInvocationSessionSnapshot {
            carrier: bound.artifact.clone(),
            artifacts: Arc::new(artifacts),
        };
        if matched
            .replace((bound.artifact.clone(), snapshot))
            .is_some()
        {
            return Err(format!(
                "deferred provider turn {} carries multiple current session snapshots",
                turn.seq
            ));
        }
    }
    Ok(matched)
}

fn rebase_deferred_provider_turn(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    release: &DeferredProviderTurnRelease,
) -> Result<(QueuedTurn, Option<crate::handler::CodexContinuityState>), String> {
    let DeferredProviderTurnRelease::SessionRebase {
        source_session_carrier,
        rebased_session_carrier,
    } = release
    else {
        return Err(format!(
            "deferred provider turn {} cannot rebase an original-envelope release",
            turn.seq
        ));
    };
    validate_artifact_admission(ctx, run, turn.seq, rebased_session_carrier)?;
    let labels = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
    let payload = read_deferred_artifact_payload(ctx, &rebased_session_carrier.artifact)?;
    let artifacts = resolve_session_snapshot_chain(
        &ctx.blobs_dir,
        ctx.required_host_io_limits()?,
        &rebased_session_carrier.artifact,
        &payload,
        Some(rebased_session_carrier.movement_seq),
        &labels,
        &ctx.interface,
    )?;
    let snapshot = ProviderInvocationSessionSnapshot {
        carrier: rebased_session_carrier.artifact.clone(),
        artifacts: Arc::new(artifacts),
    };
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let record = octopus_runtime::run_record::RunRecord::from_bytes(&pinned).map_err(|error| error.to_string())?;
    let selected = octopus_provider_run_authoring::payload_artifact_bindings(&record, &ctx.interface)?;
    octopus_provider_run_authoring::context_artifact_payloads(
        &record, turn.envelope.metadata().map_err(|error| error.to_string())?, &turn.envelope.artifact_binding_payloads,
    )?;
    let mut envelope = turn.envelope.clone();
    rebase_deferred_session_bindings(
        turn.seq,
        &mut envelope,
        source_session_carrier.as_ref(),
        &rebased_session_carrier.artifact,
        payload,
        &snapshot,
        &selected,
    )?;
    let continuity = recover_provider_turn_continuity(descriptor, &envelope, Some(&snapshot), run, &ctx.pinned_run_record_bytes(&envelope)?)
        .map_err(|error| error.to_string())?;
    Ok((
        QueuedTurn {
            envelope,
            seq: turn.seq,
        },
        continuity,
    ))
}

fn rebase_deferred_session_bindings(
    turn_seq: u64,
    envelope: &mut InterfaceInvocationEnvelope,
    source_session_carrier: Option<&ArtifactRef>,
    rebased_session_carrier: &ArtifactRef,
    rebased_payload: Vec<u8>,
    snapshot: &ProviderInvocationSessionSnapshot,
    selected: &[ArtifactRef],
) -> Result<(), String> {
    let metadata = envelope
        .metadata
        .as_mut()
        .ok_or_else(|| format!("deferred provider turn {turn_seq} lost its invocation metadata"))?;
    if envelope.artifact_binding_payloads.len() != metadata.artifact_binding_refs.len()
        || envelope
            .artifact_binding_payloads
            .iter()
            .zip(&metadata.artifact_binding_refs)
            .any(|(bound, reference)| &bound.artifact != reference)
    {
        return Err(format!(
            "deferred provider turn {turn_seq} binding payloads disagree with invocation metadata"
        ));
    }

    if selected.contains(rebased_session_carrier)
        || source_session_carrier.is_some_and(|reference| selected.contains(reference))
    {
        return Err(format!("deferred provider turn {turn_seq} selected payload collides with a session carrier"));
    }
    let mut binding_matches = 0;
    if let Some(source_session_carrier) = source_session_carrier {
        for (bound, reference) in envelope
            .artifact_binding_payloads
            .iter_mut()
            .zip(&mut metadata.artifact_binding_refs)
        {
            if &bound.artifact != source_session_carrier {
                continue;
            }
            *bound = octopus_interfaces::ArtifactBindingPayload::new(
                rebased_session_carrier.clone(),
                rebased_payload.clone(),
            );
            *reference = rebased_session_carrier.clone();
            binding_matches += 1;
        }
    } else {
        envelope.artifact_binding_payloads.insert(
            0,
            octopus_interfaces::ArtifactBindingPayload::new(
                rebased_session_carrier.clone(),
                rebased_payload,
            ),
        );
        metadata
            .artifact_binding_refs
            .insert(0, rebased_session_carrier.clone());
        binding_matches = 1;
    }
    if binding_matches != 1 {
        return Err(format!(
            "deferred provider turn {turn_seq} cannot identify one exact source session binding"
        ));
    }

    let subsumed_sources = snapshot
        .artifacts
        .iter()
        .filter(|artifact| &artifact.source_artifact != rebased_session_carrier)
        .map(|artifact| {
            (
                artifact.source_artifact.id.as_str().to_string(),
                artifact.source_artifact.content.to_hex(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut retained_payloads = Vec::with_capacity(envelope.artifact_binding_payloads.len());
    let mut retained_refs = Vec::with_capacity(metadata.artifact_binding_refs.len());
    for (bound, reference) in std::mem::take(&mut envelope.artifact_binding_payloads)
        .into_iter()
        .zip(std::mem::take(&mut metadata.artifact_binding_refs))
    {
        let key = (
            bound.artifact.id.as_str().to_string(),
            bound.artifact.content.to_hex(),
        );
        if bound.artifact != *rebased_session_carrier && !selected.contains(&bound.artifact) && subsumed_sources.contains(&key) {
            continue;
        }
        retained_payloads.push(bound);
        retained_refs.push(reference);
    }
    envelope.artifact_binding_payloads = retained_payloads;
    metadata.artifact_binding_refs = retained_refs;

    let rebased_matches = envelope
        .artifact_binding_payloads
        .iter()
        .filter(|bound| bound.artifact == *rebased_session_carrier)
        .count();
    if rebased_matches != 1
        || envelope.artifact_binding_payloads.len() != metadata.artifact_binding_refs.len()
        || envelope
            .artifact_binding_payloads
            .iter()
            .zip(&metadata.artifact_binding_refs)
            .any(|(bound, reference)| &bound.artifact != reference)
    {
        return Err(format!(
            "deferred provider turn {turn_seq} produced an inconsistent rebased session binding"
        ));
    }
    Ok(())
}

fn latest_provider_session_carrier(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    session_snapshot_labels: &BTreeSet<String>,
    stop_before_seq: Option<u64>,
) -> Result<ProviderSessionCarrierScan, String> {
    let (mut file, snapshot_len, max_row_bytes) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (start_offset, expected_seq) = authoritative_source_scan_start(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        source_movement_seq,
    );
    let mut source_verified = false;
    let mut latest = None;
    let mut next_seq = source_movement_seq;
    let scan = scan_authoritative_snapshot(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        start_offset,
        expected_seq,
        stop_before_seq,
        |movement| {
            next_seq = movement
                .seq
                .0
                .checked_add(1)
                .ok_or_else(|| "deferred provider ledger sequence overflowed".to_string())?;
            if movement.seq.0 < source_movement_seq {
                return Ok(None::<()>);
            }
            if movement.seq.0 == source_movement_seq {
                if !movement_is_provider_source(ctx, run, &movement) {
                    return Err(format!(
                        "deferred provider source movement {source_movement_seq} differs from interface '{}' run '{}'",
                        ctx.interface.as_str(),
                        run.as_str()
                    ));
                }
                source_verified = true;
                return Ok(None::<()>);
            }
            if !source_verified {
                return Err(format!(
                    "deferred provider scan advanced past causal source {source_movement_seq}"
                ));
            }
            if movement.kind == MovementKind::InterfaceArrival
                && movement.from == Endpoint::Interface(ctx.interface.clone())
                && movement.runtime().is_some_and(|runtime| {
                    movement.to == Endpoint::Interface(runtime.worker.clone())
                        && runtime.interface == ctx.interface
                })
                && movement
                    .invocation_metadata
                    .as_ref()
                    .and_then(octopus_ledger::MovementMetadata::invocation)
                    .and_then(octopus_ledger::InvocationMetadata::runtime)
                    .is_some_and(|runtime| &runtime.run == run)
                && let Some(artifact) = movement.artifact.as_ref()
                && session_snapshot_labels
                    .iter()
                    .any(|label| artifact_ref_has_label(artifact, label))
                && read_pinned_provider_artifact_payload(ctx, artifact, "provider session carrier")
                    .ok()
                    .and_then(|payload| payload_carries_session_context(&payload).ok())
                    .unwrap_or(true)
            {
                latest = Some(DurableArtifactAdmission {
                    artifact: artifact.clone(),
                    movement_seq: movement.seq.0,
                });
            }
            Ok(None::<()>)
        },
    )?;
    ensure_arbitration_snapshot_length(ctx, &file, snapshot_len)?;
    if !source_verified {
        return Err(format!(
            "deferred provider source movement {source_movement_seq} is absent from the authoritative ledger"
        ));
    }
    Ok(ProviderSessionCarrierScan {
        latest: scan.complete_boundary.then_some(latest).flatten(),
        complete_boundary: scan.complete_boundary,
        next_seq,
    })
}

fn terminal_deferred_turn_ownership(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    envelope: &InterfaceInvocationEnvelope,
    source_movement_seq: u64,
) -> Result<Option<(String, String)>, String> {
    let Some(control) =
        decode_admitted_turn_control(ctx, descriptor, envelope).map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    if !matches!(&control, CodexTurnControl::Steer { .. } | CodexTurnControl::Inject { .. }) {
        return Ok(None);
    }
    let control_id = control_id(&control).to_string();
    let action = control_action(&control).to_string();
    let Some(journal) = load_control_journal(ctx, source_movement_seq, run, &control_id, &action)?
    else {
        return Ok(None);
    };
    match journal.phase {
        TurnControlJournalPhase::DeferredTurn {
            result,
            cause: CodexDeferredTurnCause::TerminalTurnMoved,
        } if !result.acknowledged => Ok(Some((control_id, action))),
        _ => Ok(None),
    }
}

fn validate_original_envelope_release(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    expected_control_id: &str,
    expected_action: &str,
    observed_ledger_next_seq: u64,
) -> Result<(), String> {
    let ownership = terminal_deferred_turn_ownership(ctx, descriptor, run, &turn.envelope, turn.seq)?;
    if ownership
        .as_ref()
        .map(|(control_id, action)| (control_id.as_str(), action.as_str()))
        != Some((expected_control_id, expected_action))
    {
        return Err(format!(
            "deferred provider turn {} original-envelope release lost terminal control ownership",
            turn.seq
        ));
    }
    if observed_ledger_next_seq <= turn.seq {
        return Err(format!(
            "deferred provider turn {} original-envelope release names an invalid ledger boundary {}",
            turn.seq, observed_ledger_next_seq
        ));
    }
    let labels = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
    // The durable release linearized on this exact prefix. A later causal suffix may complete the
    // released source, but cannot retroactively turn its original carrier-free decision invalid.
    let scan = latest_provider_session_carrier(
        ctx,
        run,
        turn.seq,
        &labels,
        Some(observed_ledger_next_seq),
    )?;
    if !scan.complete_boundary || scan.latest.is_some() || scan.next_seq != observed_ledger_next_seq
    {
        return Err(format!(
            "deferred provider turn {} original-envelope release lacks a complete carrier-free terminal ledger boundary",
            turn.seq
        ));
    }
    Ok(())
}

fn read_deferred_artifact_payload(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
) -> Result<Vec<u8>, String> {
    read_pinned_provider_artifact_payload(ctx, artifact, "deferred session carrier")
}

fn read_pinned_provider_artifact_payload(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
    boundary: &str,
) -> Result<Vec<u8>, String> {
    read_pinned_provider_artifact(ctx, artifact, boundary).map(|(_, payload)| payload)
}

fn read_pinned_provider_artifact(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
    boundary: &str,
) -> Result<(octopus_interfaces::InterfaceArtifactManifest, Vec<u8>), String> {
    let limits = ctx.required_host_io_limits()?;
    let manifest_path = ctx.blobs_dir.join(artifact.content.to_hex());
    let captured = octopus_interfaces::InterfaceArtifactFile::new(&manifest_path)
        .map_err(|error| error.to_string())?
        .read_manifest_bounded(limits.max_manifest_bytes)
    .map_err(|error| {
        format!(
            "{boundary} '{}' cannot be read: {error}",
            manifest_path.display()
        )
    })?;
    if ContentHash::of_bytes(&captured.manifest_bytes) != artifact.content {
        return Err(format!(
            "{boundary} manifest hash differs from its artifact ref"
        ));
    }
    let manifest = captured.manifest;
    if artifact.id.as_str() != format!("{}:{}", manifest.label, artifact.content.to_hex()) {
        return Err(format!("{boundary} id differs from its manifest identity"));
    }
    let payload_hash = manifest
        .payload_content_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or_else(|| format!("{boundary} manifest has no payload hash"))?;
    let payload_path = ctx.blobs_dir.join(payload_hash.to_hex());
    let payload = octopus_interfaces::read_artifact_carrier_file_bounded(
        &payload_path,
        limits.max_payload_bytes,
    )
    .map_err(|error| {
        format!(
            "{boundary} payload '{}' cannot be read: {error}",
            payload_path.display()
        )
    })?;
    if ContentHash::of_bytes(&payload) != payload_hash {
        return Err(format!("{boundary} payload hash differs from its manifest"));
    }
    Ok((manifest, payload))
}

fn validate_provider_source_artifact(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    artifact: &ArtifactRef,
    metadata: &octopus_ledger::InvocationMetadata,
) -> Result<(), String> {
    let (mut file, snapshot_len, max_row_bytes) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (start_offset, expected_seq) = authoritative_source_scan_start(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        source_movement_seq,
    );
    let scan = scan_authoritative_snapshot(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        start_offset,
        expected_seq,
        None,
        |movement| Ok((movement.seq.0 == source_movement_seq).then_some(movement)),
    )?;
    ensure_arbitration_snapshot_length(ctx, &file, snapshot_len)?;
    let Some(source) = scan.found else {
        return Err(format!(
            "deferred provider source movement {source_movement_seq} is absent from the authoritative ledger"
        ));
    };
    if movement_is_provider_source(ctx, run, &source)
        && source.artifact.as_ref() == Some(artifact)
        && source
            .invocation_metadata
            .as_ref()
            .and_then(octopus_ledger::MovementMetadata::invocation)
            == Some(metadata)
    {
        Ok(())
    } else {
        Err(format!(
            "deferred provider source movement {source_movement_seq} does not match interface '{}' run '{}' artifact '{}'",
            ctx.interface.as_str(),
            run.as_str(),
            artifact.id
        ))
    }
}

fn validate_artifact_admission(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    admission: &DurableArtifactAdmission,
) -> Result<(), String> {
    if admission.movement_seq <= source_movement_seq {
        return Err(format!(
            "artifact '{}' admission movement {} does not follow causal source {}",
            admission.artifact.id, admission.movement_seq, source_movement_seq
        ));
    }
    let (mut file, snapshot_len, max_row_bytes) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (start_offset, expected_seq) = authoritative_source_scan_start(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        source_movement_seq,
    );
    let mut source_verified = false;
    let scan = scan_authoritative_snapshot(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        start_offset,
        expected_seq,
        None,
        |movement| {
            if movement.seq.0 < source_movement_seq {
                return Ok(None);
            }
            if movement.seq.0 == source_movement_seq {
                if !movement_is_provider_source(ctx, run, &movement) {
                    return Err(format!(
                        "provider arbitration source movement {} does not match interface '{}' run '{}'",
                        source_movement_seq,
                        ctx.interface.as_str(),
                        run.as_str()
                    ));
                }
                source_verified = true;
                return Ok(None);
            }
            if !source_verified {
                return Err(format!(
                    "provider arbitration admission {} was reached before causal source {}",
                    admission.movement_seq, source_movement_seq
                ));
            }
            Ok((movement.seq.0 == admission.movement_seq).then_some(movement))
        },
    )?;
    ensure_arbitration_snapshot_length(ctx, &file, snapshot_len)?;
    let Some(movement) = scan.found else {
        return Err(format!(
            "artifact '{}' journaled admission movement {} is absent at a {} ledger boundary",
            admission.artifact.id,
            admission.movement_seq,
            if scan.complete_boundary {
                "complete"
            } else {
                "partial"
            }
        ));
    };
    if movement_is_artifact_admission(ctx, run, &admission.artifact, &movement) {
        Ok(())
    } else {
        Err(format!(
            "artifact '{}' journaled admission movement {} does not match the authoritative provider admission",
            admission.artifact.id, admission.movement_seq
        ))
    }
}

fn find_artifact_admission(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    expected: &ArtifactRef,
) -> Result<ArtifactAdmissionScan, String> {
    let (mut file, snapshot_len, max_row_bytes) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (start_offset, expected_seq) = authoritative_source_scan_start(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        source_movement_seq,
    );
    let mut source_verified = false;
    let mut admitted = None;
    let scan = scan_authoritative_snapshot(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        start_offset,
        expected_seq,
        None,
        |movement| {
            if movement.seq.0 < source_movement_seq {
                return Ok(None::<()>);
            }
            if movement.seq.0 == source_movement_seq {
                if !movement_is_provider_source(ctx, run, &movement) {
                    return Err(format!(
                        "provider arbitration source movement {} does not match interface '{}' run '{}'",
                        source_movement_seq,
                        ctx.interface.as_str(),
                        run.as_str()
                    ));
                }
                source_verified = true;
                return Ok(None::<()>);
            }
            if !source_verified {
                return Err(format!(
                    "provider arbitration scan advanced past causal source {source_movement_seq}"
                ));
            }
            if movement_is_artifact_admission(ctx, run, expected, &movement)
                && admitted.replace(movement.seq.0).is_some()
            {
                return Err(format!(
                    "artifact '{}' has duplicate provider admissions",
                    expected.id
                ));
            }
            Ok(None::<()>)
        },
    )?;
    ensure_arbitration_snapshot_length(ctx, &file, snapshot_len)?;
    Ok(ArtifactAdmissionScan {
        movement_seq: admitted,
        complete_boundary: scan.complete_boundary,
    })
}

fn authoritative_source_scan_start(
    ctx: &InterfaceLiftContext,
    file: &mut std::fs::File,
    snapshot_len: u64,
    max_row_bytes: u64,
    decoder: &DurableMovementDecoder,
    source_movement_seq: u64,
) -> (u64, u64) {
    indexed_movement_row_hint(ctx, source_movement_seq, snapshot_len)
        .and_then(|range| {
            read_indexed_movement_candidate(ctx, file, snapshot_len, max_row_bytes, decoder, range)
                .ok()
                .filter(|movement| movement.seq.0 == source_movement_seq)
                .map(|_| (range.start_offset, source_movement_seq))
        })
        .unwrap_or((0, 0))
}

struct AuthoritativeSnapshotScan<T> {
    found: Option<T>,
    complete_boundary: bool,
}

fn open_arbitration_snapshot(
    ctx: &InterfaceLiftContext,
) -> Result<(std::fs::File, u64, u64), String> {
    let file = std::fs::File::open(&ctx.movements_path).map_err(|error| {
        format!(
            "movement ledger '{}' cannot be read during provider arbitration: {error}",
            ctx.movements_path.display()
        )
    })?;
    let snapshot_len = file
        .metadata()
        .map_err(|error| {
            format!(
                "movement ledger '{}' cannot be inspected during provider arbitration: {error}",
                ctx.movements_path.display()
            )
        })?
        .len();
    let max_row_bytes = ctx.required_host_io_limits()?.max_control_frame_bytes;
    Ok((file, snapshot_len, max_row_bytes))
}

fn ensure_arbitration_snapshot_length(
    ctx: &InterfaceLiftContext,
    file: &std::fs::File,
    snapshot_len: u64,
) -> Result<(), String> {
    let current_len = file
        .metadata()
        .map_err(|error| {
            format!(
                "movement ledger '{}' cannot be reinspected during provider arbitration: {error}",
                ctx.movements_path.display()
            )
        })?
        .len();
    if current_len < snapshot_len {
        return Err(format!(
            "movement ledger '{}' shrank from captured snapshot byte {snapshot_len} to {current_len}",
            ctx.movements_path.display()
        ));
    }
    Ok(())
}

fn indexed_movement_row_hint(
    ctx: &InterfaceLiftContext,
    seq: u64,
    snapshot_len: u64,
) -> Option<octopus_ledger_index::IndexedMovementRowRange> {
    let index = octopus_ledger_index::LedgerSqliteReader::open(&ctx.ledger_index_path).ok()?;
    let progress = index.progress().ok()?;
    if seq >= progress.watermark || progress.durable_end_offset > snapshot_len {
        return None;
    }
    let range = index.movement_row_range(seq).ok()??;
    (range.seq == seq
        && range.start_offset < range.end_offset
        && range.end_offset <= progress.durable_end_offset
        && range.end_offset <= snapshot_len)
        .then_some(range)
}

fn read_indexed_movement_candidate(
    ctx: &InterfaceLiftContext,
    file: &mut std::fs::File,
    snapshot_len: u64,
    max_row_bytes: u64,
    decoder: &DurableMovementDecoder,
    range: octopus_ledger_index::IndexedMovementRowRange,
) -> Result<octopus_ledger::Movement, String> {
    let row_len = range
        .end_offset
        .checked_sub(range.start_offset)
        .ok_or_else(|| "indexed provider arbitration row range underflowed".to_string())?;
    if range.end_offset > snapshot_len || row_len == 0 || row_len > max_row_bytes {
        return Err(format!(
            "indexed provider arbitration movement {} has unusable byte range [{}, {})",
            range.seq, range.start_offset, range.end_offset
        ));
    }
    let row_len = usize::try_from(row_len).map_err(|_| {
        format!(
            "indexed provider arbitration movement {} row length exceeds addressable memory",
            range.seq
        )
    })?;
    std::io::Seek::seek(file, std::io::SeekFrom::Start(range.start_offset)).map_err(|error| {
        format!(
            "movement ledger '{}' cannot seek to indexed movement {}: {error}",
            ctx.movements_path.display(),
            range.seq
        )
    })?;
    let mut row = vec![0; row_len];
    std::io::Read::read_exact(file, &mut row).map_err(|error| {
        format!(
            "movement ledger '{}' ended before indexed movement {}: {error}",
            ctx.movements_path.display(),
            range.seq
        )
    })?;
    let row = complete_arbitration_row(&row)?;
    if row.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Err(format!(
            "indexed provider arbitration movement {} points to an empty row",
            range.seq
        ));
    }
    let line = usize::try_from(range.seq).map_err(|_| {
        format!(
            "provider arbitration movement sequence {} exceeds the decoder line range",
            range.seq
        )
    })?;
    let movement = decoder.decode(row, line).map_err(|error| {
        format!(
            "movement ledger '{}' has malformed indexed row {} during provider arbitration: {error}",
            ctx.movements_path.display(),
            range.seq
        )
    })?;
    if movement.seq.0 != range.seq {
        return Err(format!(
            "indexed provider arbitration movement {} resolves to movement {}",
            range.seq, movement.seq.0
        ));
    }
    Ok(movement)
}

fn scan_authoritative_snapshot<T>(
    ctx: &InterfaceLiftContext,
    file: &mut std::fs::File,
    snapshot_len: u64,
    max_row_bytes: u64,
    decoder: &DurableMovementDecoder,
    start_offset: u64,
    mut expected_seq: u64,
    stop_before_seq: Option<u64>,
    mut visit: impl FnMut(octopus_ledger::Movement) -> Result<Option<T>, String>,
) -> Result<AuthoritativeSnapshotScan<T>, String> {
    let remaining = snapshot_len.checked_sub(start_offset).ok_or_else(|| {
        format!(
            "provider arbitration scan offset {start_offset} exceeds captured ledger length {snapshot_len}"
        )
    })?;
    std::io::Seek::seek(file, std::io::SeekFrom::Start(start_offset)).map_err(|error| {
        format!(
            "movement ledger '{}' cannot seek to provider arbitration offset {start_offset}: {error}",
            ctx.movements_path.display()
        )
    })?;
    let snapshot = std::io::Read::take(file, remaining);
    let mut reader = std::io::BufReader::new(snapshot);
    let mut consumed = start_offset;
    loop {
        if let Some(stop_before_seq) = stop_before_seq {
            if expected_seq == stop_before_seq {
                return Ok(AuthoritativeSnapshotScan {
                    found: None,
                    complete_boundary: true,
                });
            }
            if expected_seq > stop_before_seq {
                return Err(format!(
                    "provider arbitration scan started after requested ledger boundary {stop_before_seq}"
                ));
            }
        }
        match read_bounded_arbitration_row(&mut reader, max_row_bytes)? {
            ArbitrationLedgerRow::Complete(row) => {
                consumed = consumed
                    .checked_add(row.len() as u64)
                    .ok_or_else(|| "provider arbitration ledger offset overflowed".to_string())?;
                let row = complete_arbitration_row(&row)?;
                if row.iter().all(|byte| byte.is_ascii_whitespace()) {
                    return Err(format!(
                        "movement ledger '{}' has an empty complete row at movement {} during provider arbitration",
                        ctx.movements_path.display(),
                        expected_seq
                    ));
                }
                let line = usize::try_from(expected_seq).map_err(|_| {
                    format!(
                        "provider arbitration movement sequence {expected_seq} exceeds the decoder line range"
                    )
                })?;
                let movement = decoder.decode(row, line).map_err(|error| {
                    format!(
                        "movement ledger '{}' has malformed row {} during provider arbitration: {error}",
                        ctx.movements_path.display(),
                        expected_seq
                    )
                })?;
                if movement.seq.0 != expected_seq {
                    return Err(format!(
                        "provider arbitration expected movement {}, found {}",
                        expected_seq, movement.seq.0
                    ));
                }
                if let Some(found) = visit(movement)? {
                    return Ok(AuthoritativeSnapshotScan {
                        found: Some(found),
                        complete_boundary: false,
                    });
                }
                expected_seq = expected_seq.checked_add(1).ok_or_else(|| {
                    "provider arbitration movement sequence overflowed".to_string()
                })?;
            }
            ArbitrationLedgerRow::Partial(row) => {
                consumed = consumed
                    .checked_add(row.len() as u64)
                    .ok_or_else(|| "provider arbitration ledger offset overflowed".to_string())?;
                if consumed != snapshot_len {
                    return Err(format!(
                        "movement ledger '{}' ended at byte {consumed} before captured snapshot byte {snapshot_len}",
                        ctx.movements_path.display()
                    ));
                }
                return Ok(AuthoritativeSnapshotScan {
                    found: None,
                    complete_boundary: false,
                });
            }
            ArbitrationLedgerRow::Eof => {
                if consumed != snapshot_len {
                    return Err(format!(
                        "movement ledger '{}' ended at byte {consumed} before captured snapshot byte {snapshot_len}",
                        ctx.movements_path.display()
                    ));
                }
                return Ok(AuthoritativeSnapshotScan {
                    found: None,
                    complete_boundary: stop_before_seq.is_none(),
                });
            }
        }
    }
}

fn complete_arbitration_row(row: &[u8]) -> Result<&[u8], String> {
    row.strip_suffix(b"\n")
        .map(|row| row.strip_suffix(b"\r").unwrap_or(row))
        .ok_or_else(|| "provider arbitration complete row lost its newline".to_string())
}

fn movement_is_artifact_admission(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    expected: &ArtifactRef,
    movement: &octopus_ledger::Movement,
) -> bool {
    movement.kind == MovementKind::InterfaceArrival
        && movement.from == Endpoint::Interface(ctx.interface.clone())
        && movement.runtime().is_some_and(|runtime| {
            movement.to == Endpoint::Interface(runtime.worker.clone())
                && runtime.interface == ctx.interface
        })
        && movement
            .invocation_metadata
            .as_ref()
            .and_then(octopus_ledger::MovementMetadata::invocation)
            .filter(|metadata| metadata.verification_failure.is_none())
            .and_then(octopus_ledger::InvocationMetadata::runtime)
            .is_some_and(|runtime| &runtime.run == run)
        && movement.artifact.as_ref() == Some(expected)
}

fn movement_is_provider_source(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    movement: &octopus_ledger::Movement,
) -> bool {
    movement.kind == MovementKind::InterfaceOutbound
        && matches!(&movement.from, Endpoint::Interface(_))
        && movement.to == Endpoint::Interface(ctx.interface.clone())
        && movement.artifact.is_some()
        && movement
            .invocation_metadata
            .as_ref()
            .and_then(octopus_ledger::MovementMetadata::invocation)
            .and_then(octopus_ledger::InvocationMetadata::runtime)
            .is_some_and(|runtime| &runtime.run == run && &runtime.interface == &ctx.interface)
}

enum ArbitrationLedgerRow {
    Complete(Vec<u8>),
    Partial(Vec<u8>),
    Eof,
}

fn read_bounded_arbitration_row(
    reader: &mut impl std::io::BufRead,
    max_row_bytes: u64,
) -> Result<ArbitrationLedgerRow, String> {
    let mut row = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("provider arbitration ledger read failed: {error}"))?;
        if available.is_empty() {
            return Ok(if row.is_empty() {
                ArbitrationLedgerRow::Eof
            } else {
                ArbitrationLedgerRow::Partial(row)
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if row.len() as u64 + take as u64 > max_row_bytes {
            return Err(format!(
                "provider arbitration ledger row exceeds declared {max_row_bytes} byte limit"
            ));
        }
        row.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            return Ok(ArbitrationLedgerRow::Complete(row));
        }
    }
}

async fn await_artifact_admission_scan(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    expected: &ArtifactRef,
) -> Result<ArtifactAdmissionScan, String> {
    for _ in 0..128 {
        let scan = find_artifact_admission(ctx, run, source_movement_seq, expected)?;
        if scan.movement_seq.is_some() || scan.complete_boundary {
            return Ok(scan);
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    Err(format!(
        "movement ledger '{}' retained an incomplete tail while provider admission '{}' was pending",
        ctx.movements_path.display(),
        expected.id
    ))
}

async fn control_source_gate(
    control_gates: &ControlSourceGates,
    run: &RunId,
) -> Arc<tokio::sync::Mutex<()>> {
    let mut gates = control_gates.lock().await;
    gates.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = gates.get(run.as_str()).and_then(std::sync::Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    gates.insert(run.as_str().to_string(), Arc::downgrade(&gate));
    gate
}

async fn hand_control_result(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    interrupted_effects: &InterruptedEffectArbiter,
    run: &RunId,
    delivery: ControlResultDelivery<'_>,
) -> Result<bool, String> {
    let ControlResultDelivery {
        envelope,
        source_movement_seq,
        output,
        handoff_barrier,
        effect_guard: _effect_guard,
        result,
        recovery,
        defer_message_receipt,
    } = delivery;
    let interrupted_continuity = exact_interrupted_control_continuity(&result)?;
    let control_id = result.control_id.clone();
    let mut artifact = materialize_turn_control_result(
        &ctx.interface_var_dir().join("emitted"),
        source_movement_seq,
        result.clone(),
    )
    .map_err(|error| format!("codex turn-control result failed to materialize: {error}"))?;
    let mut artifact_ref = materialized_artifact_ref(&artifact)?;
    let mut admitted_seq = if recovery {
        await_artifact_admission_scan(ctx, run, source_movement_seq, &artifact_ref)
            .await?
            .movement_seq
    } else {
        None
    };
    if admitted_seq.is_none() {
        if let Some(output) = output {
            match output
                .hand_control(ProviderEmission {
                    artifact: artifact.clone(),
                    route: ProviderEmissionRoute::ClientProjection,
                }, source_movement_seq)
                .await
            {
                Ok(()) => {
                    admitted_seq =
                        await_artifact_admission_scan(ctx, run, source_movement_seq, &artifact_ref)
                            .await?
                            .movement_seq;
                }
                Err(ControlOutputError::Closed) => {}
                Err(ControlOutputError::Failed(error)) => return Err(error),
            }
        }
    }
    if admitted_seq.is_none() && defer_message_receipt
        && matches!(result.action.as_str(), "queue_message" | "steer")
    {
        return Ok(false);
    }
    if admitted_seq.is_none() {
        if let Some(barrier) = handoff_barrier {
            barrier.wait().await.map_err(|error| {
                format!(
                    "prior provider handoff failed before turn control could be authored: {error}"
                )
            })?;
        }
        let pinned_run_record = ctx.pinned_run_record_bytes(envelope)?;
        let handoff = wait_for_provider_handoff(
            ctx,
            descriptor,
            handoff_session,
            run,
            envelope,
            source_movement_seq,
            &pinned_run_record,
        ).await?;
        match handoff.recover_source().await? {
            ProviderSourceRecovery::Bridged { .. } => return Ok(true),
            ProviderSourceRecovery::Completed => {}
            ProviderSourceRecovery::Interrupted {
                last_artifact_label,
            } => {
                return Err(format!(
                    "turn-control result cannot cross while non-terminal provider artifact '{last_artifact_label}' owns the run frontier"
                ));
            }
            ProviderSourceRecovery::Process => {
                if result.continuity.is_none()
                    && result.acknowledged
                    && terminal_control_is_satisfied(&result.action)
                    && result.thread_id.is_none() && result.turn_id.is_none()
                    && result.detail == terminally_satisfied_control(&control_id, &result.action).result.detail
                {
                    let context = handoff.current_session_context_artifacts().await;
                    let continuity = terminal_output_continuity(
                        descriptor, envelope, &context, run, &handoff.current_run_record_bytes().await,
                    ).map_err(|error| error.to_string())?;
                    if let Some(pending) = continuity.as_ref().filter(|state| codex_continuity_waits_for_result(state)) {
                        let original = interrupted_effects.durable_continuity(run).await?
                            .ok_or("terminal turn-control receipt still has an unresolved native effect")?;
                        if !crate::handler::codex_continuity_retains_interrupted_effect(&original, pending) {
                            return Err("terminal turn-control receipt changed its unresolved interrupted effect".into());
                        }
                        let identity = crate::handler::interrupted_turn_identity(&original)?;
                        result.thread_id = Some(identity.thread_id);
                        result.turn_id = Some(identity.turn_id);
                        result.detail["exact_result_pending"] = serde_json::json!(true);
                    }
                    result.continuity = continuity;
                    persist_control_journal(ctx, source_movement_seq, run, &control_id, &result.action,
                        TurnControlJournalPhase::Result { result: result.clone() })?;
                    artifact = materialize_turn_control_result(
                        &ctx.interface_var_dir().join("emitted"), source_movement_seq, result.clone(),
                    ).map_err(|error| format!("codex turn-control result failed to materialize: {error}"))?;
                    artifact_ref = materialized_artifact_ref(&artifact)?;
                }
                let emission = ProviderEmission { artifact, route: ProviderEmissionRoute::ClientProjection };
                if result.continuity.is_none() && (!result.acknowledged || matches!(result.action.as_str(), "queue_message" | "steer")) {
                    handoff.hand_observation(emission, true).await?;
                } else {
                    handoff.hand_final(emission).await?;
                }
            }
        }
        admitted_seq = await_artifact_admission_scan(ctx, run, source_movement_seq, &artifact_ref)
            .await?
            .movement_seq;
    }
    if let Some(continuity) = interrupted_continuity {
        let movement_seq = admitted_seq.ok_or_else(|| {
            "acknowledged interrupt has no admitted control-result movement".to_string()
        })?;
        interrupted_effects
            .record_acknowledgement(
                run,
                source_movement_seq,
                &control_id,
                DurableArtifactAdmission {
                    artifact: artifact_ref,
                    movement_seq,
                },
                continuity,
            )
            .await?;
    }
    Ok(true)
}

fn materialized_artifact_ref(
    artifact: &octopus_interfaces::InterfaceArtifactFile,
) -> Result<ArtifactRef, String> {
    let manifest_bytes = std::fs::read(artifact.manifest_path()).map_err(|error| {
        format!(
            "turn-control result manifest '{}' cannot be read for recovery: {error}",
            artifact.manifest_path().display()
        )
    })?;
    let manifest = artifact.read_manifest().map_err(|error| {
        format!(
            "turn-control result manifest '{}' is invalid during recovery: {error}",
            artifact.manifest_path().display()
        )
    })?;
    let content = ContentHash::of_bytes(&manifest_bytes);
    Ok(ArtifactRef {
        id: ArtifactId::new(format!("{}:{}", manifest.label, content.to_hex())),
        content,
    })
}

fn control_id(control: &CodexTurnControl) -> &str {
    match control {
        CodexTurnControl::Inject { control_id, .. }
        | CodexTurnControl::Steer { control_id, .. }
        | CodexTurnControl::Interrupt { control_id, .. }
        | CodexTurnControl::Unsupported { control_id, .. } => control_id,
    }
}

fn control_action(control: &CodexTurnControl) -> &str {
    match control {
        CodexTurnControl::Inject { .. } => "queue_message",
        CodexTurnControl::Steer { .. } => "steer",
        CodexTurnControl::Interrupt { action, .. }
        | CodexTurnControl::Unsupported { action, .. } => action,
    }
}

async fn hand_stream_emission(
    handoff: &ProviderOutputHandoff,
    pending: PendingProviderEmission,
    terminal: bool,
) -> Result<ProviderHandoffOutcome, String> {
    let (emission, admitted, bindings) = pending;
    let result = handoff
        .hand_batch_with_outgoing_bindings(vec![emission], terminal, &bindings)
        .await
        .and_then(|mut outcomes| {
            if outcomes.len() != 1 {
                return Err(format!("provider emission produced {} outcomes instead of 1", outcomes.len()));
            }
            Ok(outcomes.remove(0))
        })
    .and_then(|outcome| {
        if let ProviderHandoffOutcome::TerminalFailure {
            failure_seq,
            failure_class,
            ..
        } = &outcome
            && !terminal
        {
            return Err(format!(
                "provider stream stopped by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"
            ));
        }
        Ok(outcome)
    });
    match result {
        Ok(outcome) => {
            if let Some(admitted) = admitted {
                let _ = admitted.send(Ok(()));
            }
            Ok(outcome)
        }
        Err(error) => {
            if let Some(admitted) = admitted {
                let _ = admitted.send(Err(error.clone()));
            }
            Err(error)
        }
    }
}

async fn hand_stream_batch(
    handoff: &ProviderOutputHandoff,
    pending: Vec<PendingProviderEmission>,
) -> Result<(), String> {
    if pending.is_empty() {
        return Err("provider stream cannot hand an empty progress batch".to_string());
    }
    let mut pending = pending.into_iter();
    while let Some(emission) = pending.next() {
        if let Err(error) = hand_stream_emission(handoff, emission, false).await {
            for (_, admitted, _) in pending {
                if let Some(admitted) = admitted {
                    let _ = admitted.send(Err(error.clone()));
                }
            }
            return Err(error);
        }
    }
    Ok(())
}

fn flush_stream_batch<'a>(
    handoff: &'a ProviderOutputHandoff,
    pending: &'a mut Vec<PendingProviderEmission>,
    keep_last: bool,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<usize, String>> + 'a>> {
    Box::pin(flush_stream_batch_inner(handoff, pending, keep_last))
}

async fn flush_stream_batch_inner(
    handoff: &ProviderOutputHandoff,
    pending: &mut Vec<PendingProviderEmission>,
    keep_last: bool,
) -> Result<usize, String> {
    let flush_len = pending.len().saturating_sub(usize::from(keep_last));
    if flush_len == 0 {
        return Ok(0);
    }
    let batch = pending.drain(..flush_len).collect::<Vec<_>>();
    hand_stream_batch(handoff, batch).await?;
    Ok(flush_len)
}

async fn hold_newest_stream_emission(
    handoff: &ProviderOutputHandoff,
    pending: &mut Vec<PendingProviderEmission>,
    emission: PendingProviderEmission,
) -> Result<usize, String> {
    pending.push(emission);
    flush_stream_batch(handoff, pending, true).await
}

async fn close_stream_emissions(
    handoff: &ProviderOutputHandoff,
    pending: &mut Vec<PendingProviderEmission>,
    terminal: bool,
) -> Result<(usize, Option<ProviderHandoffOutcome>), String> {
    let flushed = flush_stream_batch(handoff, pending, true).await?;
    let Some(last) = pending.pop() else {
        return Ok((flushed, None));
    };
    Ok((flushed, Some(hand_stream_emission(handoff, last, terminal).await?)))
}

fn drive_turn_with_retries<'a>(
    scope: CodexTurnScope<'a>,
    driver: &'a mut Option<Arc<CodexAppServerInterfaceDriver>>,
    turn: QueuedTurn,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(), String>> + 'a>> {
    Box::pin(drive_turn_with_retries_inner(scope, driver, turn))
}

async fn drive_turn_with_retries_inner(
    scope: CodexTurnScope<'_>,
    driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>,
    turn: QueuedTurn,
) -> Result<(), String> {
    let CodexTurnScope {
        ctx,
        descriptor,
        handoff_session,
        runtime_auth,
        run,
        control,
        interrupted_effects,
    } = scope;
    let current_descriptor = load_current_own_descriptor(ctx)?;
    if current_descriptor != *descriptor {
        return Err(format!(
            "codex interface '{}' descriptor changed after this life was lifted; refusing movement {} until a fresh life adopts the exact descriptor",
            ctx.interface.as_str(),
            turn.seq
        ));
    }
    adopt_current_machine_auth_generation(ctx, runtime_auth, driver).await?;
    let pinned_run_record = match ctx.pinned_run_record_bytes(&turn.envelope) {
        Ok(record) => record,
        Err(error) => {
            record_invalid_provider_context(ctx, descriptor, handoff_session, run, &turn, error)
                .await?;
            control.finish();
            return Ok(());
        }
    };
    let handoff = match wait_for_provider_handoff(
        ctx,
        descriptor,
        handoff_session,
        run,
        &turn.envelope,
        turn.seq,
        &pinned_run_record,
    ).await {
        Ok(handoff) => handoff,
        Err(error) if ctx.cancel.is_cancelled() => return Err(error),
        Err(error) => {
            if recover_interrupted_result_without_context(
                ctx, descriptor, handoff_session, interrupted_effects, control, run, &turn,
            ).await? {
                control.finish();
                return Ok(());
            }
            record_invalid_provider_context(ctx, descriptor, handoff_session, run, &turn, error)
                .await?;
            control.finish();
            return Ok(());
        }
    };
    validate_saved_source_withdrawal(ctx, descriptor, run, &turn, &handoff).await?;
    let (source_recovery, recovered_outcomes) = handoff.recover_source_with_outcomes().await?;
    if matches!(source_recovery, ProviderSourceRecovery::Bridged { .. }) {
        control.finish();
        return Ok(());
    }
    if try_settle_completed_interrupted_result(ctx, descriptor, run, &turn, &handoff, interrupted_effects).await? {
        control.finish();
        return Ok(());
    }
    // Classify exact, completed duplicate results before interruption arbitration:
    // an older result must neither resolve nor cancel its successor's pending effect.
    if crate::handler::correlated_tool_result(&turn.envelope).map_err(|error| error.to_string())?.is_some()
        && let Some(completed_source) = handoff.completed_input_artifact_source(&turn.envelope.artifact).await?
    {
        if source_recovery == ProviderSourceRecovery::Completed { return Ok(()); }
        if source_recovery != ProviderSourceRecovery::Process {
            return Err("duplicate result retains an unfinished source handoff".into());
        }
        handoff.validate_client_output_routes(&codex_projection_output_labels(descriptor).map_err(|error| error.to_string())?)?;
        let receipt = crate::handler::duplicate_tool_result_receipt(
            descriptor, &turn.envelope, turn.seq, completed_source,
            &handoff.current_run_record_bytes().await,
            &handoff.current_session_context_artifacts().await,
            &ctx.interface_var_dir().join("emitted"),
        ).map_err(|error| error.to_string())?;
        match handoff.hand_observation(receipt, true).await? {
            ProviderHandoffOutcome::Admitted { .. } => return Ok(()),
            ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => {
                return Err(format!("duplicate tool result receipt failed: {failure_class} at {failure_seq}"));
            }
        }
    }
    control.prepare_recovered_result().await?;
    let durable_interrupted_continuity = interrupted_effects.durable_continuity(run).await?;
    let resolving_interrupted_result = interrupted_effects
        .begin_result(run, turn.seq, &turn.envelope)
        .await?;
    let durable_interrupted_continuity = if resolving_interrupted_result {
        let original = durable_interrupted_continuity.as_ref().ok_or("interrupted source lost its durable continuity")?;
        Some(crate::handler::checked_interrupted_context_revision(original, &handoff.current_session_context_artifacts().await,
            &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?, None)?)
    } else { durable_interrupted_continuity };
    let (mut interrupted_after_artifact, mut recovered_handoff_artifacts) = match source_recovery {
        ProviderSourceRecovery::Bridged { .. } => {
            control.finish();
            return Ok(());
        }
        ProviderSourceRecovery::Process => {
            if !recovered_outcomes.is_empty() {
                return Err(format!(
                    "provider source movement {} returned recovery outcomes without an interrupted frontier",
                    turn.seq
                ));
            }
            (None, Arc::new(Vec::new()))
        }
        ProviderSourceRecovery::Completed => {
            interrupted_effects
                .recover_completed_source(run, turn.seq, &recovered_outcomes)
                .await?;
            retain_completed_source_control(ctx, descriptor, runtime_auth, driver, control, interrupted_effects, run, &turn, &handoff).await?;
            return Ok(());
        }
        ProviderSourceRecovery::Interrupted {
            last_artifact_label,
        } => (
            Some(last_artifact_label),
            recovered_codex_handoff_artifacts(ctx, run, turn.seq, &recovered_outcomes).await?,
        ),
    };
    let output_labels = codex_projection_output_labels(descriptor).map_err(|error| error.to_string())?;
    if let Err(error) = handoff.validate_client_output_routes(&output_labels) {
        record_invalid_provider_context(
            ctx, descriptor, handoff_session, run, &turn,
            format!("provider output topology is invalid before dispatch: {error}"),
        ).await?;
        control.finish();
        return Ok(());
    }
    if interrupted_after_artifact.is_some() && !resolving_interrupted_result {
        let record = handoff.current_run_record_bytes().await;
        let session = handoff.current_session_context_artifacts().await;
        if let Some(receipt) = crate::handler::recovered_tool_handoff_receipt(
            descriptor, &turn.envelope, turn.seq, &record, &session, &recovered_outcomes,
            &ctx.interface_var_dir().join("emitted"),
        ).map_err(|error| error.to_string())? {
            match handoff.hand_final(receipt).await? {
                ProviderHandoffOutcome::Admitted { .. } => {
                    control.finish();
                    return Ok(());
                }
                ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => {
                    return Err(format!("recovered tool handoff failed to close its source: {failure_class} at {failure_seq}"));
                }
            }
        }
    }
    if interrupted_after_artifact.is_none()
        && !resolving_interrupted_result
        && handoff.consumes_context_free_independent_input().await
    {
        let manifest = turn
            .envelope
            .read_artifact_manifest()
            .map_err(|error| error.to_string())?;
        if !is_inflight_turn_control_candidate_label(&manifest.label) {
            let status = serde_json::from_slice::<serde_json::Value>(
                &handoff.current_run_record_bytes().await,
            )
            .map_err(|error| error.to_string())?;
            let artifact = crate::handler::materialize_independent_input_receipt(
                &ctx.interface_var_dir().join("emitted"),
                turn.seq,
                &manifest.label,
                &turn.envelope.artifact,
            )
            .map_err(|error| {
                format!(
                    "independent input {} receipt failed to materialize: {error}",
                    turn.seq
                )
            })?;
            handoff
                .hand_observation(
                    ProviderEmission {
                        artifact,
                        route: ProviderEmissionRoute::ClientProjection,
                    },
                    status.get("status").and_then(serde_json::Value::as_str) == Some("idle"),
                )
                .await?;
            control.finish();
            return Ok(());
        }
    }
    if resolving_interrupted_result {
        let continuity = durable_interrupted_continuity.as_ref()
            .ok_or("interrupted source lost its durable continuity")?;
        if let Some((emissions, closure)) = recover_sealed_interrupted_result(
            ctx, descriptor, &turn, continuity,
        )? {
            admit_interrupted_result(ctx, &handoff, control, Some(interrupted_effects), run, turn.seq, &closure, emissions).await?;
            control.finish();
            return Ok(());
        }
        if let Some(emissions) = crate::handler::stopped_result::materialize_pending_interrupted(
            descriptor, continuity, &turn.envelope, turn.seq, &ctx.interface_var_dir().join("emitted"), &ctx.interface_var_dir().join("streams"),
            handoff.input_changes_context().await,
        ).map_err(|error| error.to_string())? {
            let reference = materialized_artifact_ref(&emissions.last().ok_or("interrupted result group has no closure")?.artifact)?;
            admit_interrupted_result(ctx, &handoff, control, Some(interrupted_effects), run, turn.seq, &reference, emissions).await?;
            return Ok(());
        }
    }
    let native_terminal_source = if resolving_interrupted_result { None } else {
        let snapshot = handoff.invocation_session_snapshot();
        let current_session = handoff.current_session_context_artifacts().await;
        recover_provider_result_continuity(descriptor, &turn.envelope, snapshot.as_ref(), &current_session, run, &pinned_run_record)
            .map_err(|error| error.to_string())?
    };
    if let Some(source) = native_terminal_source.as_ref() {
        if let Some((emissions, closure)) = recover_sealed_interrupted_result(ctx, descriptor, &turn, source)? {
            admit_interrupted_result(ctx, &handoff, control, None, run, turn.seq, &closure, emissions).await?;
            control.finish();
            return Ok(());
        }
    }
    let private_config = &descriptor
        .interface_driver_config
        .as_ref()
        .ok_or_else(|| {
            format!(
                "provider interface '{}' has no interface_driver_config",
                descriptor.id.as_str()
            )
        })?
        .private_config;
    let callable_tool_names = match handoff.callable_tool_names(&turn.envelope) {
        Ok(names) => names,
        Err(error) => {
            control.finish();
            return emit_terminal_turn_error(ctx, descriptor, handoff_session, run, &turn, &handoff, 0,
                &format!("provider model tool topology is invalid before dispatch: {error}")).await;
        }
    };
    let hidden_tools = model_hidden_tool_names(private_config)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut model_visible_tools = BTreeSet::new();
    for affordance in turn
        .envelope
        .interface_affordance_specs()
        .map_err(|error| error.to_string())?
    {
        let tool = affordance.name.as_str().to_string();
        if !callable_tool_names.contains(&tool) || hidden_tools.contains(&tool) {
            continue;
        }
        if !model_visible_tools.insert(tool.clone()) {
            control.finish();
            return emit_terminal_turn_error(
                ctx,
                descriptor,
                handoff_session,
                run,
                &turn,
                &handoff,
                0,
                &format!("formed model affordances repeat tool '{tool}'"),
            )
            .await;
        }
    }
    if let Err(error) = handoff.validate_model_tool_routes(&model_visible_tools) {
        control.finish();
        return emit_terminal_turn_error(
            ctx,
            descriptor,
            handoff_session,
            run,
            &turn,
            &handoff,
            0,
            &format!("provider model tool topology is invalid before dispatch: {error}"),
        )
        .await;
    }
    let max_attempts = match required_policy_u32(&turn.envelope, "max_attempts") {
        Ok(value) => value,
        Err(error) => {
            ctx.hand_admitted_failure(
                turn.seq,
                "malformed_provider_policy",
                serde_json::json!({ "field": "max_attempts", "error": error }),
            )
            .await?;
            control.finish();
            return emit_terminal_turn_error(
                ctx, descriptor, handoff_session, run, &turn, &handoff, 0,
                &format!("formed provider max_attempts is invalid: {error}"),
            ).await;
        }
    };
    let max_in_flight = match required_policy_u32(&turn.envelope, "max_in_flight") {
        Ok(value) => value,
        Err(error) => {
            ctx.hand_admitted_failure(
                turn.seq,
                "malformed_provider_policy",
                serde_json::json!({ "field": "max_in_flight", "error": error }),
            )
            .await?;
            control.finish();
            return emit_terminal_turn_error(
                ctx, descriptor, handoff_session, run, &turn, &handoff, 0,
                &format!("formed provider max_in_flight is invalid: {error}"),
            ).await;
        }
    };
    if max_attempts == 0 || max_in_flight == 0 {
        control.finish();
        return emit_terminal_turn_error(
            ctx,
            descriptor,
            handoff_session,
            run,
            &turn,
            &handoff,
            0,
            "formed interface policy denied provider calls",
        )
        .await;
    }
    if let Err(error) = enforce_formed_model_policy(descriptor, &turn.envelope) {
        control.finish();
        return emit_terminal_turn_error(
            ctx,
            descriptor,
            handoff_session,
            run,
            &turn,
            &handoff,
            0,
            &format!("provider_model_unavailable: {error}"),
        )
        .await;
    }
    let mut attempt: u32 = 1;
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(
                "codex life cancelled before the movement reached an admitted outcome".to_string(),
            );
        }
        if driver.is_none() {
            match connect_declared_interface_driver(
                ctx,
                descriptor,
                runtime_auth,
                Some(&turn.envelope),
            )
            .await
            {
                Ok(connected) => *driver = Some(connected),
                Err(error) => {
                    tracing::warn!(
                        interface = ctx.interface.as_str(),
                        run = run.as_str(),
                        seq = turn.seq,
                        attempt,
                        error = %error,
                        "codex transport connect failed"
                    );
                    if !error.retryable {
                        emit_terminal_turn_error(
                            ctx,
                            descriptor,
                            handoff_session,
                            run,
                            &turn,
                            &handoff,
                            0,
                            &error.message,
                        )
                        .await?;
                        control.finish();
                        return Ok(());
                    }
                    if !backoff_or_give_up(ctx, max_attempts, &mut attempt).await? {
                        emit_terminal_turn_error(
                            ctx,
                            descriptor,
                            handoff_session,
                            run,
                            &turn,
                            &handoff,
                            max_attempts,
                            &error.message,
                        )
                        .await?;
                        control.finish();
                        return Ok(());
                    }
                    continue;
                }
            }
        }
        let live = driver.as_ref().expect("driver connected above").clone();
        control.publish_starting();

        let (output_stream, mut emit_rx) = ProviderOutputStream::channel();
        let handoff_barrier = ProviderHandoffBarrier::new();
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel::<bool>();
        let forwarder = Box::pin({
            let handoff = handoff.clone();
            let interrupted_effects = interrupted_effects.clone();
            let durable_interrupted_continuity = durable_interrupted_continuity.clone();
            let native_terminal_source = native_terminal_source.clone();
            let source_envelope = turn.envelope.clone();
            let run = run.clone();
            let source_artifact = turn.envelope.artifact.clone();
            let result_source_movement_seq = turn.seq;
            async move {
                let mut pending = Vec::new();
                let mut forwarded = ForwardedProviderEmissions::default();
                let mut interrupted_closure = None;
                let mut terminal_group_handed = false;
                loop {
                    let Some(stream_emission) = emit_rx.recv().await else {
                        break;
                    };
                    if interrupted_closure.is_some() || terminal_group_handed {
                        return Err(
                            "provider emitted another artifact after its Codex-local terminal group"
                                .to_string(),
                        );
                    }
                    match stream_emission {
                        ProviderStreamEmission::CurrentRunRecord { expected_before, reply } => {
                            forwarded.outputs += flush_stream_batch(&handoff, &mut pending, false).await?;
                            let current = handoff.reconciled_run_record_bytes().await?;
                            let current = match expected_before {
                                Some((hash, record)) => handoff.checked_capability_baseline(&hash, &record).await,
                                None => Ok(current),
                            };
                            reply.send(current).map_err(|_| "checked run reader closed before its response".to_string())?;
                        }
                        ProviderStreamEmission::WithCapabilityAmendment { emission, amendment } => {
                            if resolving_interrupted_result {
                                return Err("interrupted result cannot amend its capability contract".into());
                            }
                            forwarded.outputs += flush_stream_batch(&handoff, &mut pending, false).await?;
                            // The bridge ends this input's handoff. Its external result
                            // remains pending in the emitted native continuity.
                            let outcomes = handoff.hand_batch_with_capability_amendment(vec![emission], true, &amendment).await?;
                            if outcomes.len() != 1 || !outcomes.iter().all(|outcome| matches!(outcome, ProviderHandoffOutcome::Admitted { .. })) {
                                return Err("capability amendment has no exact admitted bridge effect".into());
                            }
                            forwarded.outputs += 1;
                            terminal_group_handed = true;
                        }
                        ProviderStreamEmission::WithOutgoingBindings { emission, bindings } => {
                            if resolving_interrupted_result {
                                return Err("interrupted result cannot create outgoing bindings".into());
                            }
                            forwarded.outputs += hold_newest_stream_emission(&handoff, &mut pending, (emission, None, bindings)).await?;
                        }
                        ProviderStreamEmission::Turn(emission) => {
                            if resolving_interrupted_result {
                                return Err(
                                    "interrupted exact-result recovery emitted a non-atomic provider artifact before its closure"
                                        .to_string(),
                                );
                            }
                            forwarded.outputs += hold_newest_stream_emission(&handoff, &mut pending, (emission, None, Vec::new())).await?;
                        }
                        ProviderStreamEmission::TerminalGroup { stream, terminal } => {
                            if resolving_interrupted_result {
                                return Err(
                                    "interrupted exact-result recovery emitted a normal terminal group"
                                        .to_string(),
                                );
                            }
                            if !pending.is_empty() {
                                let flushed =
                                    flush_stream_batch(&handoff, &mut pending, false).await?;
                                forwarded.outputs += flushed;
                            }
                            let outcomes = handoff.hand_batch(vec![stream, terminal]).await?;
                            if outcomes.len() != 2 {
                                return Err(format!(
                                    "Codex terminal group produced {} outcomes instead of 2",
                                    outcomes.len()
                                ));
                            }
                            if let Some((failure_seq, failure_class)) =
                                outcomes.iter().find_map(|outcome| match outcome {
                                    ProviderHandoffOutcome::TerminalFailure {
                                        failure_seq,
                                        failure_class,
                                        ..
                                    } => Some((*failure_seq, failure_class.as_str())),
                                    ProviderHandoffOutcome::Admitted { .. } => None,
                                })
                            {
                                return Err(format!(
                                    "Codex terminal group was replaced by run-authoring failure '{failure_class}' at movement {failure_seq}"
                                ));
                            }
                            forwarded.outputs += outcomes.len();
                            terminal_group_handed = true;
                        }
                        ProviderStreamEmission::InterruptedResult {
                            activity,
                            stream,
                            retained_outputs,
                            closure,
                        } => {
                            if !pending.is_empty() {
                                return Err(
                                    "provider emitted an interrupted-result group outside its exact durable source boundary"
                                        .to_string(),
                                );
                            }
                            let closure_ref = if resolving_interrupted_result {
                                let continuity = durable_interrupted_continuity.as_ref().ok_or("interrupted-result group has no durable interruption continuity")?;
                                interrupted_result_closure_ref(&closure.artifact, continuity, &source_artifact, &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?
                            } else {
                                let continuity = native_terminal_source.as_ref().ok_or("control-free terminal group has no original ordinary pending source")?;
                                native_terminal_result_closure_ref(&closure.artifact, continuity, &source_envelope, &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?
                            }
                            .ok_or_else(|| {
                                "interrupted-result group has no semantic closure artifact"
                                    .to_string()
                            })?;
                            let mut emissions = Vec::new();
                            if let Some(activity) = activity {
                                emissions.push(activity);
                            }
                            if let Some(stream) = stream {
                                emissions.push(stream);
                            }
                            emissions.extend(retained_outputs);
                            emissions.push(closure);
                            match admit_interrupted_result(
                                &ctx, &handoff, control, resolving_interrupted_result.then_some(&interrupted_effects), &run, result_source_movement_seq,
                                &closure_ref, emissions,
                            ).await? {
                                InterruptedResultAdmission::Admitted(count) => forwarded.outputs += count,
                                InterruptedResultAdmission::Bridged => {
                                    forwarded.bridged = true;
                                    return Ok(forwarded);
                                }
                            }
                            interrupted_closure = Some(closure_ref);
                            terminal_group_handed = true;
                        }
                        ProviderStreamEmission::ContextBoundary { emissions, admitted } => {
                            let result = async {
                                if resolving_interrupted_result {
                                    return Err("interrupted exact-result recovery cannot refresh provider context".to_string());
                                }
                                if !pending.is_empty() {
                                    forwarded.outputs += flush_stream_batch(&handoff, &mut pending, false).await?;
                                }
                                let count = emissions.len();
                                if count == 0 || count > 3 {
                                    return Err("provider context boundary has an invalid emission count".to_string());
                                }
                                let outcomes = handoff.hand_batch_with_outgoing_bindings(emissions, false, &[]).await?;
                                if outcomes.len() != count || outcomes.iter().any(|outcome| !matches!(outcome, ProviderHandoffOutcome::Admitted { .. })) {
                                    return Err("provider context boundary was not fully admitted".to_string());
                                }
                                forwarded.outputs += count;
                                    Ok(())
                            }.await;
                            let _ = admitted.send(result.clone());
                            result?;
                        }
                        ProviderStreamEmission::Control { emission, source_movement_seq, admitted } => {
                            if resolving_interrupted_result {
                                let error = "interrupted exact-result recovery cannot emit a control artifact"
                                    .to_string();
                                let _ = admitted.send(Err(error.clone()));
                                return Err(error);
                            }
                            let label = emission.artifact.read_manifest().map_err(|error| error.to_string())?.label;
                            if !pending.is_empty() {
                                forwarded.outputs += flush_stream_batch(&handoff, &mut pending, false).await?;
                            }
                            let consume_context = !matches!(label.as_str(), crate::handler::TURN_INPUT_ACKNOWLEDGED_LABEL | crate::handler::TURN_CONTROL_FAILED_LABEL);
                            let result = handoff.hand_from_input(emission, false, source_movement_seq, consume_context).await.and_then(|outcome| match outcome {
                                ProviderHandoffOutcome::Admitted { .. } => Ok(()),
                                ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => Err(format!(
                                    "provider control response stopped by authoring failure '{failure_class}' at {failure_seq}"
                                )),
                            });
                            let _ = admitted.send(result.clone());
                            result?;
                            forwarded.outputs += 1;
                        }
                        ProviderStreamEmission::TransportDispatch { artifact, admitted } => {
                            if resolving_interrupted_result {
                                let error = "interrupted exact-result recovery cannot dispatch another provider request"
                                    .to_string();
                                let _ = admitted.send(Err(error.clone()));
                                return Err(error);
                            }
                            if !pending.is_empty() {
                                let flushed =
                                    flush_stream_batch(&handoff, &mut pending, false).await?;
                                forwarded.outputs += flushed;
                                }
                            let result = handoff.hand_transport_dispatch(artifact).await.and_then(
                                |outcome| match outcome {
                                    ProviderHandoffOutcome::Admitted { .. } => Ok(()),
                                    ProviderHandoffOutcome::TerminalFailure {
                                        failure_seq,
                                        failure_class,
                                        ..
                                    } => Err(format!(
                                        "provider transport dispatch stopped by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"
                                    )),
                                },
                            );
                            match result {
                                Ok(()) => {
                                    let _ = admitted.send(Ok(()));
                                    forwarded.dispatches += 1;
                                }
                                Err(error) => {
                                    let _ = admitted.send(Err(error.clone()));
                                    return Err(error);
                                }
                            }
                        }
                    }
                }
                let terminal = terminal_rx.await.unwrap_or(false);
                if terminal_group_handed {
                    if !pending.is_empty() {
                        return Err(
                            "Codex-local terminal group left unhanded provider outputs".to_string()
                        );
                    }
                    return Ok::<ForwardedProviderEmissions, String>(forwarded);
                }
                let (flushed, last) = close_stream_emissions(
                    &handoff,
                    &mut pending,
                    terminal || interrupted_closure.is_some(),
                )
                .await?;
                forwarded.outputs += flushed;
                if let Some(outcome) = last {
                    if let Some(closure) = interrupted_closure {
                        match outcome {
                            ProviderHandoffOutcome::Admitted { seq, artifact } => {
                                let admitted = DurableArtifactAdmission {
                                    artifact,
                                    movement_seq: seq,
                                };
                                interrupted_effects
                                    .complete_closing(
                                        &run,
                                        result_source_movement_seq,
                                        &closure,
                                        &admitted,
                                    )
                                    .await?;
                            }
                            ProviderHandoffOutcome::TerminalFailure {
                                failure_seq,
                                failure_class,
                                ..
                            } => {
                                return Err(format!(
                                    "interrupted-result closure was replaced by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"
                                ));
                            }
                        }
                    }
                    forwarded.outputs += 1;
                }
                Ok::<ForwardedProviderEmissions, String>(forwarded)
            }
        });

        let output_root = ctx.interface_var_dir().join("emitted");
        let live_stream_root = ctx.interface_var_dir().join("streams");
        let current_run_record = handoff.current_run_record_bytes().await;
        let current_session_context = handoff.current_session_context_artifacts().await;
        let reconciled_result_continuity = match is_provider_turn_input(descriptor, &turn.envelope) {
            Ok(true) => {
            let snapshot = bound_operator_session_snapshot(ctx, descriptor, &turn)?;
            let bound = recover_provider_turn_continuity(descriptor, &turn.envelope,
                snapshot.as_ref().map(|(_, snapshot)| snapshot), run, &pinned_run_record)
                .map_err(|error| error.to_string())?;
            let deferred = load_deferred_provider_turns(ctx, descriptor)?;
            let release = deferred.get(&turn.seq).and_then(|turn| turn.release.as_ref())
                .filter(|release| matches!(release, DeferredProviderTurnRelease::InterruptedResult { .. }));
            checked_deferred_interrupted_result(ctx, descriptor, run, &turn, bound.as_ref(), release,
                Some(&current_session_context)).await?.map(|(_, continuity)| continuity)
            }
            Ok(false) | Err(_) => None,
        };
        let input_changes_context = handoff.input_changes_context().await;
        let receive = Box::pin(async {
            let result = live
                .receive_artifact_and_stream_outputs(CodexProviderInvocation {
                    callable_tool_names: callable_tool_names.clone(),
                    input: turn.envelope.clone(),
                    emit: output_stream.clone(),
                    resolved_session_snapshot: handoff.invocation_session_snapshot(),
                    current_session_context,
                    output_root: &output_root,
                    live_stream_root: &live_stream_root,
                    source_movement_seq: turn.seq,
                    attempt,
                    admitted_run_record: &pinned_run_record,
                    current_run_record: &current_run_record,
                    input_changes_context,
                    interrupted_after_artifact: interrupted_after_artifact.clone(),
                    recovered_handoff_artifacts: recovered_handoff_artifacts.clone(),
                    durable_interrupted_continuity: durable_interrupted_continuity.clone(),
                    reconciled_result_continuity,
                    control,
                    handoff_barrier: handoff_barrier.clone(),
                })
                .await;
            drop(output_stream);
            let _ = terminal_tx.send(result.is_ok());
            result
        });
        let (result, forwarded) = {
            let mut receive = receive;
            let mut forwarder = forwarder;
            tokio::select! {
                result = &mut receive => (result, forwarder.await),
                forwarded = &mut forwarder => {
                    match forwarded {
                        Ok(forwarded) if forwarded.bridged => (Ok(()), Ok(forwarded)),
                        Ok(forwarded) => (receive.await, Ok(forwarded)),
                        Err(error) => {
                            handoff_barrier.finish(Err(error.clone()));
                            return Err(error);
                        }
                    }
                }
            }
        };
        handoff_barrier.finish(forwarded.as_ref().map(|_| ()).map_err(Clone::clone));
        let forwarded = forwarded?;
        drop(live);
        if forwarded.bridged { return Ok(()); }

        if resolving_interrupted_result {
            if interrupted_effects.reconciled_source(run, turn.seq).await? {
                control.finish();
                return Ok(());
            }
            return Err(format!(
                "interrupted exact-result source {} ended without one ledger-admitted semantic closure{}",
                turn.seq, result.err().map(|error| format!(": {error}")).unwrap_or_default()
            ));
        }

        match result {
            Ok(()) if forwarded.outputs == 0 => {
                control.finish();
                return emit_terminal_turn_error(
                    ctx,
                    descriptor,
                    handoff_session,
                    run,
                    &turn,
                    &handoff,
                    attempt,
                    "codex transport completed a turn without emitting an artifact",
                )
                .await;
            }
            Ok(()) => return Ok(()),
            Err(InterfaceBoundaryError::Protocol(error))
            | Err(InterfaceBoundaryError::FailedClosed(error)) => {
                control.finish();
                return emit_terminal_turn_error(
                    ctx, descriptor, handoff_session, run, &turn, &handoff, attempt, &error,
                )
                .await;
            }
            Err(InterfaceBoundaryError::Transport(error)) => {
                tracing::warn!(
                    interface = ctx.interface.as_str(),
                    run = run.as_str(),
                    seq = turn.seq,
                    attempt,
                    error = %error,
                    "codex turn transport failed"
                );
                if let Err(shutdown_error) = shutdown_driver(ctx, driver).await {
                    control.finish();
                    return emit_terminal_turn_error(
                        ctx, descriptor, handoff_session, run, &turn, &handoff, attempt,
                        &format!("{error}; {shutdown_error}"),
                    ).await;
                }
                let (source_recovery, recovered_outcomes) =
                    handoff.recover_source_with_outcomes().await?;
                (interrupted_after_artifact, recovered_handoff_artifacts) = match source_recovery {
                    ProviderSourceRecovery::Bridged { .. } => {
                        control.finish();
                        return Ok(());
                    }
                    ProviderSourceRecovery::Process => {
                        if !recovered_outcomes.is_empty() {
                            return Err(format!(
                                "provider source movement {} returned recovery outcomes without an interrupted frontier",
                                turn.seq
                            ));
                        }
                        (None, Arc::new(Vec::new()))
                    }
                    ProviderSourceRecovery::Completed => {
                        interrupted_effects
                            .recover_completed_source(run, turn.seq, &recovered_outcomes)
                            .await?;
                        retain_completed_source_control(ctx, descriptor, runtime_auth, driver, control, interrupted_effects, run, &turn, &handoff).await?;
                        return Ok(());
                    }
                    ProviderSourceRecovery::Interrupted {
                        last_artifact_label,
                    } => (
                        Some(last_artifact_label),
                        recovered_codex_handoff_artifacts(ctx, run, turn.seq, &recovered_outcomes)
                            .await?,
                    ),
                };
                if !backoff_or_give_up(ctx, max_attempts, &mut attempt).await? {
                    emit_terminal_turn_error(
                        ctx,
                        descriptor,
                        handoff_session,
                        run,
                        &turn,
                        &handoff,
                        max_attempts,
                        &error,
                    )
                    .await?;
                    control.finish();
                    return Ok(());
                }
            }
        }
    }
}

async fn record_invalid_provider_context(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
    error: String,
) -> Result<(), String> {
    let message = format!(
        "codex turn stopped because its admitted provider context is invalid (movement seq {}): {error}",
        turn.seq
    );
    if author_failed_provider_source(ctx, descriptor, handoff_session, run, turn, &message, false).await? {
        return Ok(());
    }
    Err(message)
}

fn materialize_failed_source_error(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
    message: &str,
) -> Result<octopus_interfaces::InterfaceArtifactFile, String> {
    crate::handler::materialize_terminal_turn_error_payload(
        &ctx.interface_var_dir().join("emitted"), message, None,
        serde_json::json!({"source_movement_seq": source_movement_seq}),
    ).map_err(
        |failure| {
            format!(
                "failed provider source {source_movement_seq} could not be materialized as turn.error: {failure}"
            )
        },
    )
}

async fn author_failed_provider_source(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
    message: &str,
    restore_consumed: bool,
) -> Result<bool, String> {
    let unavailable = match handoff_session.failed_source_handoff(
        ctx,
        descriptor,
        run,
        &turn.envelope,
        turn.seq,
    ) {
        Ok(handoff) => match handoff
            .hand_failed_source(ProviderEmission {
                artifact: materialize_consumed_source_outcome(ctx, turn, message,
                    restore_consumed && handoff.consumes_context_free_independent_input().await)?,
                route: ProviderEmissionRoute::ClientProjection,
            })
            .await
        {
            Ok(_) => return Ok(true),
            Err(FailedSourceHandoffError::Interrupted(error)) => return Err(error),
            Err(FailedSourceHandoffError::Unavailable(error)) => error,
        },
        Err(error) => error,
    };
    tracing::warn!(
        interface = ctx.interface.as_str(),
        run = run.as_str(),
        seq = turn.seq,
        error = %unavailable,
        "codex failed provider source could not be authored into the run"
    );
    Ok(false)
}

fn materialize_consumed_source_outcome(
    ctx: &InterfaceLiftContext, turn: &QueuedTurn, message: &str, context_free_independent: bool,
) -> Result<octopus_interfaces::InterfaceArtifactFile, String> {
    if context_free_independent {
        let manifest = turn.envelope.read_artifact_manifest().map_err(|error| error.to_string())?;
        if !is_inflight_turn_control_candidate_label(&manifest.label) {
            return crate::handler::materialize_independent_input_receipt(
                &ctx.interface_var_dir().join("emitted"), turn.seq, &manifest.label, &turn.envelope.artifact,
            ).map_err(|error| error.to_string());
        }
    }
    materialize_failed_source_error(ctx, turn.seq, message)
}

async fn restore_consumed_provider_inputs(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    deferred_turns: &BTreeMap<u64, DeferredProviderTurn>,
    interrupted_effects: &InterruptedEffectArbiter,
    committed_before_seq: u64,
) -> Result<(), String> {
    let Ok(consumed) = handoff_session.consumed_context_input_sources(ctx, descriptor) else {
        return Ok(());
    };
    for (run, source) in consumed {
        if source >= committed_before_seq
            || deferred_turns.values().any(|deferred| deferred.run == run)
            || !matches!(interrupted_effects.durable_continuity(&run).await, Ok(None))
        {
            continue;
        }
        let Ok(Some(turn)) = replay_provider_source(ctx, &run, source) else {
            continue;
        };
        let message = format!(
            "codex turn ended without a projected outcome (movement seq {source}); its input checkpoint was restored and the message can be sent again"
        );
        author_failed_provider_source(ctx, descriptor, handoff_session, &run, &turn, &message, true)
            .await?;
    }
    Ok(())
}

async fn recover_pending_intent_control(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, session: &ProviderHandoffSession,
    run: &RunId, broker: &CodexTurnControlBroker, transport: Option<&crate::handler::stopped_result::PendingIntentTransport>, control: &CodexTurnControl,
) -> Result<Option<CodexTurnControlResult>, String> {
    let (Some(source), Some(transport)) = (broker.source_movement_seq(), transport) else { return Ok(None); };
    if !matches!(control, CodexTurnControl::Interrupt { .. }) { return Ok(None); }
    let original = replay_provider_source(ctx, run, source)?.ok_or("recorded Stop lost its admitted original source")?;
    let pinned = ctx.pinned_run_record_bytes(&original.envelope)?;
    let handoff = wait_for_provider_handoff(ctx, descriptor, session, run, &original.envelope, source, &pinned).await?;
    let Some(intent) = handoff.pending_source_intent().await? else { return Ok(None); };
    let proof = crate::handler::stopped_result::prove_intent(descriptor, &original.envelope, intent, &handoff.current_session_context_artifacts().await).map_err(|error| error.to_string())?;
    transport.interrupted_control(&proof, control).await.map(Some).map_err(|error| error.to_string())
}

async fn withdraw_pending_source_control(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, handoff_session: &ProviderHandoffSession,
    run: &RunId, broker: &Arc<CodexTurnControlBroker>, pending_transport: Option<crate::handler::stopped_result::PendingIntentTransport>,
    envelope: &InterfaceInvocationEnvelope, source_movement_seq: u64, result: &mut CodexTurnControlResult,
) -> Result<(), String> {
    let control_id = result.control_id.clone();
    let action = result.action.clone();
    let saved_source = result.detail.get("pending_source_withdrawal").map(|saved|
        saved["source_movement_seq"].as_u64().ok_or("saved withdrawal has no exact original source")).transpose()?;
    if saved_source.is_some() {
        let ack = materialize_turn_control_result(&ctx.interface_var_dir().join("emitted"), source_movement_seq, result.clone()).map_err(|error| error.to_string())?;
        if await_artifact_admission_scan(ctx, run, source_movement_seq, &materialized_artifact_ref(&ack)?).await?.movement_seq.is_some() { return Ok(()); }
    }
    if let Some(source) = saved_source.or(broker.source_movement_seq()) {
        if broker.source_movement_seq().is_some_and(|active| active != source) { return Err("saved withdrawal changed its active original source".into()); }
        let original = replay_provider_source(ctx, run, source)?.ok_or("cold Stop lost its exact original result source")?;
        let pinned = ctx.pinned_run_record_bytes(&original.envelope)?;
        let original_handoff = wait_for_provider_handoff(ctx, descriptor, handoff_session, run, &original.envelope, source, &pinned).await?;
        let intent = original_handoff.pending_source_intent().await?;
        let saved = result.detail.get("pending_source_withdrawal").cloned();
        if intent.is_some() || saved.is_some() {
            let (closure, intent) = if let Some(saved) = saved {
                let file = octopus_interfaces::InterfaceArtifactFile::new(std::path::PathBuf::from(saved["manifest_path"].as_str().ok_or("saved withdrawal lost its private artifact")?)).map_err(|error| error.to_string())?;
                let path = file.manifest_path().canonicalize().map_err(|error| error.to_string())?;
                if !path.starts_with(ctx.interface_var_dir().canonicalize().map_err(|error| error.to_string())?)
                    || serde_json::to_value(materialized_artifact_ref(&file)?).map_err(|error| error.to_string())? != saved["artifact"]
                    || saved["source_movement_seq"] != source
                { return Err("saved withdrawal changed its exact private closure".into()); }
                (ProviderEmission { artifact: file, route: ProviderEmissionRoute::ClientProjection }, intent)
            } else {
                let intent = intent.ok_or("withdrawal lost its installed intent")?;
                let proof = crate::handler::stopped_result::prove_intent(descriptor, &original.envelope, intent, &original_handoff.current_session_context_artifacts().await)
                    .map_err(|error| error.to_string())?;
                let closure = pending_transport.ok_or("cold Stop has no exact retained native transport")?.closure(&proof, &original.envelope,
                    source_movement_seq, &envelope.artifact, &result, &ctx.interface_var_dir().join("emitted"),
                    &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?).await.map_err(|error| error.to_string())?;
                result.detail["pending_source_withdrawal"] = serde_json::json!({
                    "source_movement_seq":source,"artifact":materialized_artifact_ref(&closure.artifact)?,
                    "manifest_path":closure.artifact.manifest_path(),
                });
                persist_control_journal(ctx, source_movement_seq, run, &control_id, &action, TurnControlJournalPhase::Result { result: result.clone() })?;
                (closure, Some(proof.intent))
            };
            let value: serde_json::Value = serde_json::from_slice(&closure.artifact.read_payload_bytes().map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
            crate::handler::stopped_result::validate_withdrawal_closure(&value, &original.envelope, source, &envelope.artifact, source_movement_seq, &result)?;
            if let Some(intent) = intent {
                let proof = crate::handler::stopped_result::prove_intent(descriptor, &original.envelope, intent.clone(), &original_handoff.current_session_context_artifacts().await)
                    .map_err(|error| error.to_string())?;
                let control = decode_admitted_turn_control(ctx, descriptor, envelope).map_err(|error| error.to_string())?.ok_or("withdrawal lost its admitted control")?;
                crate::handler::stopped_result::validate_saved_intent_closure(&proof, &original.envelope, &value, &envelope.artifact, source_movement_seq, &control,
                    &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?;
                validate_saved_source_withdrawal(ctx, descriptor, run, &original, &original_handoff).await?;
                match original_handoff.hand_source_withdrawal(&intent, source_movement_seq, &envelope.artifact, closure).await? {
                    ProviderHandoffOutcome::Admitted { .. } => {},
                    ProviderHandoffOutcome::TerminalFailure { failure_class, .. } => return Err(format!("pending intent withdrawal failed: {failure_class}")),
                }
            } else {
                validate_saved_source_withdrawal(ctx, descriptor, run, &original, &original_handoff).await?;
                if original_handoff.pending_source_withdrawal().await?.is_some() {
                    let (recovery, outcomes) = original_handoff.recover_source_with_outcomes().await?;
                    if recovery != ProviderSourceRecovery::Completed || outcomes.len() != 1 {
                        return Err("saved withdrawal has no exact checked completed closure".into());
                    }
                }
                original_handoff.validate_source_withdrawal(source_movement_seq, &envelope.artifact).await?;
            }
            let mut continuity = crate::handler::stopped_result::closure_continuity(&value, &ctx.interface, run.as_str())?;
            continuity.advance_control_context(descriptor)?;
            result.continuity = Some(continuity);
            persist_control_journal(ctx, source_movement_seq, run, &control_id, &action, TurnControlJournalPhase::Result { result: result.clone() })?;
            broker.finish();
        }
    }
    Ok(())
}

async fn retain_completed_source_control(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, auth: &CodexRuntimeAuth,
    driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>, control: &CodexTurnControlBroker,
    interrupted: &InterruptedEffectArbiter, run: &RunId, turn: &QueuedTurn, handoff: &ProviderOutputHandoff,
) -> Result<(), String> {
    if let Some(continuity) = interrupted.durable_continuity(run).await? {
        return control.publish_recovered_interrupted_pending(&continuity);
    }
    let continuity = crate::handler::completed_source_pending_continuity(descriptor, &turn.envelope,
        &handoff.current_run_record_bytes().await, &handoff.current_session_context_artifacts().await)
        .map_err(|error| error.to_string())?;
    if let Some(continuity) = continuity {
        if driver.is_none() {
            *driver = Some(connect_declared_interface_driver(ctx, descriptor, auth, Some(&turn.envelope)).await.map_err(|error| error.message)?);
        }
        driver.as_ref().expect("connected recovery transport").restore_pending_control(control, continuity).await;
    } else { control.finish(); }
    Ok(())
}

async fn restore_completed_provider_sources(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, session: &ProviderHandoffSession,
    auth: &CodexRuntimeAuth, driver: &mut Option<Arc<CodexAppServerInterfaceDriver>>,
    active_controls: &ActiveTurnControls, interrupted: &InterruptedEffectArbiter, committed_before: u64,
) -> Result<(), String> {
    for (run, source) in session.completed_source_candidates(ctx)? {
        let turn = replay_provider_source(ctx, &run, source)?.ok_or("completed result source lost its original admitted input")?;
        let pinned = ctx.pinned_run_record_bytes(&turn.envelope)?;
        let handoff = wait_for_provider_handoff(ctx, descriptor, session, &run, &turn.envelope, source, &pinned).await?;
        if let Some(intent) = handoff.pending_source_intent().await? {
            let continuity = crate::handler::stopped_result::intent_control_continuity(descriptor, &turn.envelope, intent, &handoff.current_session_context_artifacts().await)
                .map_err(|error| error.to_string())?;
            if driver.is_none() {
                *driver = Some(connect_declared_interface_driver(ctx, descriptor, auth, Some(&turn.envelope)).await.map_err(|error| error.message)?);
            }
            let broker = CodexTurnControlBroker::new(Some(source));
            driver.as_ref().expect("connected pending recovery transport").restore_pending_control(&broker, continuity).await;
            active_controls.lock().await.insert(run.to_string(), broker);
            continue;
        }
        validate_saved_source_withdrawal(ctx, descriptor, &run, &turn, &handoff).await?;
        if handoff.pending_source_withdrawal().await?.is_some() {
            let broker = CodexTurnControlBroker::new(Some(source));
            broker.finish();
            active_controls.lock().await.insert(run.to_string(), broker);
            continue;
        }
        if source >= committed_before { continue; }
        let (recovery, outcomes) = handoff.recover_completed_source_with_outcomes().await?;
        if matches!(recovery, ProviderSourceRecovery::Bridged { .. }) { continue; }
        if try_settle_completed_interrupted_result(ctx, descriptor, &run, &turn, &handoff, interrupted).await? { continue; }
        if recovery != ProviderSourceRecovery::Completed { continue; }
        let record = handoff.current_run_record_bytes().await;
        let context = handoff.current_session_context_artifacts().await;
        let Some(proof) = crate::handler::stopped_result::prove(descriptor, &turn.envelope, source, &record, &context, &outcomes)
            .map_err(|error| error.to_string())? else {
                let broker = CodexTurnControlBroker::new(Some(source));
                retain_completed_source_control(ctx, descriptor, auth, driver, &broker, interrupted, &run, &turn, &handoff).await?;
                if !broker.is_finished() { active_controls.lock().await.insert(run.to_string(), broker); }
                continue;
            };
        validate_stopped_result_control(ctx, descriptor, &run, &proof)?;
        if driver.is_none() {
            *driver = Some(connect_declared_interface_driver(ctx, descriptor, auth, Some(&turn.envelope)).await.map_err(|error| error.message)?);
        }
        let closure = driver.as_ref().expect("connected recovery transport").stopped_result_settlement(
            &proof, &turn.envelope, &ctx.interface_var_dir().join("emitted"),
            &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?,
        ).await.map_err(|error| error.to_string())?;
        let expected: serde_json::Value = serde_json::from_slice(&closure.artifact.read_payload_bytes().map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        match handoff.hand_completed_source_settlement(&proof.predecessor, proof.context_generation(), closure).await? {
            ProviderHandoffOutcome::Admitted { seq, artifact } => {
                let (manifest, bytes) = read_pinned_provider_artifact(ctx, &artifact, "stopped result settlement")?;
                let mut actual: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
                actual.as_object_mut().ok_or("stopped result settlement is not an object")?.remove("session_context");
                if manifest.label != TURN_INTERRUPTED_RESULT_LABEL || actual != expected {
                    return Err("stopped result admitted a different semantic closure".into());
                }
                validate_artifact_admission(ctx, &run, source, &DurableArtifactAdmission { artifact, movement_seq: seq })?;
            }
            ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => {
                return Err(format!("stopped result settlement failed: {failure_class} at {failure_seq}"));
            }
        }
    }
    Ok(())
}

async fn try_settle_completed_interrupted_result(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId, turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff, interrupted: &InterruptedEffectArbiter,
) -> Result<bool, String> {
    let record: serde_json::Value = serde_json::from_slice(&handoff.current_run_record_bytes().await).map_err(|error| error.to_string())?;
    let pending = &record["interface_state"][ctx.interface.as_str()]["pending_handoff"];
    if pending["source_movement_seq"] != turn.seq || pending["terminal"] != true
        || (pending["settlement_of"].is_null() && pending["items"][0]["artifact_label"] != "turn.error") { return Ok(false); }
    let Some(journal) = interrupted.journal(run).await? else { return Ok(false); };
    if !matches!(&journal.phase,
        InterruptedEffectPhase::Resolving { result_source_movement_seq, .. }
        | InterruptedEffectPhase::Closing { result_source_movement_seq, .. } if *result_source_movement_seq == turn.seq)
        || !crate::handler::stopped_result::has_native_interrupted_result(&journal.continuity)? { return Ok(false); }
    settle_completed_interrupted_result(ctx, descriptor, run, turn, handoff, interrupted, &journal).await?;
    Ok(true)
}

async fn settle_completed_interrupted_result(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId, turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff, interrupted: &InterruptedEffectArbiter, journal: &InterruptedEffectJournal,
) -> Result<(), String> {
    let saved = handoff.pending_source_settlement().await?
        .ok_or("completed interrupted result has no checked settlement predecessor")?;
    let [ProviderHandoffOutcome::Admitted { seq, artifact }] = saved.predecessor_outcomes.as_slice()
        else { return Err("completed interrupted result requires one admitted predecessor".into()); };
    if artifact != &saved.predecessor { return Err("completed interrupted predecessor ref changed".into()); }
    let context = handoff.current_session_context_artifacts().await.iter()
        .filter(|entry| entry.source_movement_seq.is_some_and(|value| value <= *seq)).cloned().collect::<Vec<_>>();
    let proof = prove_completed_interrupted_result(ctx, descriptor, run, turn, journal,
        &saved.predecessor_record, &context, &saved.predecessor_outcomes)?;
    if let Some(closure) = saved.closure {
        let InterruptedEffectPhase::Closing { closure_artifact, .. } = &journal.phase else {
            return Err("saved interrupted settlement is outside its exact Closing phase".into());
        };
        let value: serde_json::Value = serde_json::from_slice(&closure.read_payload_bytes().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let labels = codex_context_output_labels(descriptor).map_err(|error| error.to_string())?;
        crate::handler::stopped_result::validate_saved_settlement(&proof, &turn.envelope, &value, &labels)?;
        let original = octopus_interfaces::InterfaceArtifactFile::new(ctx.interface_var_dir().join("emitted")
            .join(format!("turn_interrupted_result-{}.artifact.json", closure_artifact.content.to_hex())))
            .map_err(|error| error.to_string())?;
        let limits = ctx.required_host_io_limits()?;
        let original_bytes = original.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes).map_err(|error| error.to_string())?;
        if materialized_artifact_ref(&original)? != *closure_artifact { return Err("interrupted Closing manifest pin changed".into()); }
        let original: serde_json::Value = serde_json::from_slice(&original_bytes.payload_bytes).map_err(|error| error.to_string())?;
        let mut candidate = value.clone();
        candidate.as_object_mut().ok_or("saved interrupted settlement is not an object")?.remove("session_context");
        if candidate != original { return Err("saved interrupted settlement changed its exact Closing closure".into()); }
        let (recovery, outcomes) = handoff.recover_completed_source_settlement(&proof.predecessor, &materialized_artifact_ref(&closure)?).await?;
        if matches!(recovery, ProviderSourceRecovery::Bridged { .. }) { return Ok(()); }
        if recovery != ProviderSourceRecovery::Completed { return Err("saved interrupted settlement did not complete its checked source".into()); }
        let admission = interrupted_result_admission(&outcomes)?.ok_or("saved interrupted settlement has no admitted closure")?;
        let (_, bytes) = read_pinned_provider_artifact(ctx, &admission.artifact, "recovered interrupted settlement")?;
        let admitted: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if admitted != value { return Err("saved interrupted settlement admitted a different snapshot".into()); }
        interrupted.complete_closing(run, turn.seq, closure_artifact, &admission).await
    } else {
        hand_completed_interrupted_result(ctx, descriptor, run, turn, handoff, interrupted, &proof).await
    }
}

fn prove_completed_interrupted_result(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId, turn: &QueuedTurn,
    journal: &InterruptedEffectJournal, record: &[u8],
    context: &[octopus_provider_run_authoring::SessionContextArtifact], outcomes: &[ProviderHandoffOutcome],
) -> Result<crate::handler::stopped_result::StoppedResultSettlement, String> {
    match &journal.phase {
        InterruptedEffectPhase::Resolving { result_source_movement_seq, source_artifact }
            if *result_source_movement_seq == turn.seq && source_artifact == &turn.envelope.artifact => {}
        InterruptedEffectPhase::Closing { result_source_movement_seq, .. }
        | InterruptedEffectPhase::Reconciled { result_source_movement_seq, .. }
            if *result_source_movement_seq == turn.seq => {}
        _ => return Err("completed interrupted settlement changed its durable result coordinate".into()),
    }
    let proof = crate::handler::stopped_result::prove_interrupted(descriptor, &turn.envelope, turn.seq,
        record, context, outcomes, &crate::handler::stopped_result::InterruptedResultSource {
            continuity: &journal.continuity, control_source: journal.control_source_movement_seq,
            control_id: &journal.control_id, acknowledgement: &journal.acknowledgement.artifact,
            acknowledgement_seq: journal.acknowledgement.movement_seq,
        }).map_err(|error| error.to_string())?;
    validate_completed_interrupted_result_control(ctx, descriptor, run, &journal.continuity, &proof)?;
    Ok(proof)
}

fn validate_completed_interrupted_result_control(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId,
    continuity: &crate::handler::CodexContinuityState, proof: &crate::handler::stopped_result::StoppedResultSettlement,
) -> Result<(), String> {
    validate_stopped_result_control(ctx, descriptor, run, &proof)?;
    let control = load_control_journal(ctx, proof.control_source, run, &proof.control_id, &proof.action)?
        .ok_or("completed interrupted settlement lost its durable actual Stop")?;
    let result = match control.phase {
        TurnControlJournalPhase::Result { result } | TurnControlJournalPhase::Delivered { result } => result,
        _ => return Err("completed interrupted settlement has no durable Stop result".into()),
    };
    let (_, bytes) = read_pinned_provider_artifact(ctx, &proof.acknowledgement, "interrupted settlement ACK")?;
    let ack: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if !result.acknowledged || result.control_id != proof.control_id || result.action != proof.action
        || result.continuity.as_ref() != Some(continuity)
        || ack["thread_id"] != serde_json::json!(result.thread_id)
        || ack["turn_id"] != serde_json::json!(result.turn_id) || ack["detail"] != result.detail
    { return Err("completed interrupted settlement ACK conflicts with its durable Stop result".into()); }
    Ok(())
}

async fn hand_completed_interrupted_result(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId, turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff, interrupted: &InterruptedEffectArbiter,
    proof: &crate::handler::stopped_result::StoppedResultSettlement,
) -> Result<(), String> {
    let closure = crate::handler::stopped_result::materialize_interrupted(proof, &turn.envelope,
        &ctx.interface_var_dir().join("emitted"), &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let reference = materialized_artifact_ref(&closure.artifact)?;
    let expected: serde_json::Value = serde_json::from_slice(&closure.artifact.read_payload_bytes().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    interrupted.begin_closing(run, turn.seq, reference.clone()).await?;
    match handoff.hand_completed_source_settlement(&proof.predecessor, proof.context_generation(), closure).await? {
        ProviderHandoffOutcome::Admitted { seq, artifact } => {
            let (manifest, bytes) = read_pinned_provider_artifact(ctx, &artifact, "completed interrupted settlement")?;
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            value.as_object_mut().ok_or("completed interrupted settlement is not an object")?.remove("session_context");
            if manifest.label != TURN_INTERRUPTED_RESULT_LABEL || value != expected {
                return Err("completed interrupted settlement admitted a different semantic closure".into());
            }
            interrupted.complete_closing(run, turn.seq, &reference, &DurableArtifactAdmission { artifact, movement_seq: seq }).await
        }
        ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } =>
            Err(format!("completed interrupted settlement failed: {failure_class} at {failure_seq}")),
    }
}

async fn validate_saved_source_withdrawal(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId,
    original: &QueuedTurn, handoff: &ProviderOutputHandoff,
) -> Result<(), String> {
    let Some(saved) = handoff.pending_source_withdrawal().await? else { return Ok(()); };
    let control = replay_provider_source(ctx, run, saved.control_source)?.ok_or("saved withdrawal lost its admitted control input")?;
    if control.envelope.artifact != saved.control { return Err("saved withdrawal control ArtifactRef changed".into()); }
    let decoded = decode_admitted_turn_control(ctx, descriptor, &control.envelope).map_err(|error| error.to_string())?
        .ok_or("saved withdrawal input is not a declared control")?;
    let context = handoff.current_session_context_artifacts().await;
    let prior = context.iter().filter(|entry| entry.source_movement_seq.is_some_and(|seq| seq < original.seq)).cloned().collect::<Vec<_>>();
    let proof = crate::handler::stopped_result::prove_intent(descriptor, &original.envelope, saved.intent, &prior).map_err(|error| error.to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&saved.closure.read_payload_bytes().map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    crate::handler::stopped_result::validate_saved_intent_closure(&proof, &original.envelope, &value, &saved.control, saved.control_source, &decoded,
        &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?;
    handoff.validate_source_withdrawal_output(&materialized_artifact_ref(&saved.closure)?).await
}

fn validate_stopped_result_control(
    ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor, run: &RunId,
    proof: &crate::handler::stopped_result::StoppedResultSettlement,
) -> Result<(), String> {
    validate_artifact_admission(ctx, run, proof.control_source, &DurableArtifactAdmission {
        artifact: proof.acknowledgement.clone(), movement_seq: proof.acknowledgement_seq,
    })?;
    let control = replay_provider_source(ctx, run, proof.control_source)?.ok_or("stopped result lost its admitted Stop input")?;
    let decoded = decode_admitted_turn_control(ctx, descriptor, &control.envelope).map_err(|error| error.to_string())?
        .ok_or("stopped result source is not a declared control")?;
    if control_id(&decoded) != proof.control_id || control_action(&decoded) != proof.action {
        return Err("stopped result acknowledgement changed its exact admitted control".into());
    }
    Ok(())
}

fn replay_provider_source(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
) -> Result<Option<QueuedTurn>, String> {
    let mut replay = LedgerReplayReader::open(ctx)?;
    while let Some(batch) = replay.read_batch(PROVIDER_CONTROL_READ_BATCH_LIMIT)? {
        for own in batch {
            if own.movement.seq.0 < source_movement_seq {
                continue;
            }
            if own.movement.seq.0 > source_movement_seq
                || own.metadata()?.runtime.as_ref().map(|runtime| &runtime.run) != Some(run)
            {
                return Ok(None);
            }
            return Ok(Some(QueuedTurn {
                envelope: replay.envelope(&own)?,
                seq: source_movement_seq,
            }));
        }
    }
    Ok(None)
}

async fn backoff_or_give_up(
    ctx: &InterfaceLiftContext,
    max_attempts: u32,
    attempt: &mut u32,
) -> Result<bool, String> {
    if *attempt >= max_attempts {
        return Ok(false);
    }
    let backoff = TURN_RETRY_BACKOFF * *attempt;
    *attempt += 1;
    tokio::select! {
        _ = ctx.cancel.cancelled() => Err(
            "codex life cancelled before the movement reached an admitted outcome".to_string()
        ),
        _ = tokio::time::sleep(backoff) => Ok(true),
    }
}

fn emit_terminal_turn_error<'a>(
    ctx: &'a InterfaceLiftContext,
    descriptor: &'a InterfaceDescriptor,
    handoff_session: &'a ProviderHandoffSession,
    run: &'a RunId,
    turn: &'a QueuedTurn,
    handoff: &'a ProviderOutputHandoff,
    attempts: u32,
    error: &'a str,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(), String>> + 'a>> {
    Box::pin(emit_terminal_turn_error_inner(
        ctx, descriptor, handoff_session, run, turn, handoff, attempts, error,
    ))
}

async fn emit_terminal_turn_error_inner(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    attempts: u32,
    error: &str,
) -> Result<(), String> {
    let attempt_summary = match attempts {
        0 => "before any transport attempt".to_string(),
        1 => "after 1 attempt".to_string(),
        count => format!("after {count} attempts"),
    };
    let message = format!(
        "codex turn failed terminally {attempt_summary} (movement seq {}): {error}",
        turn.seq
    );
    emit_terminal_error_message(ctx, descriptor, handoff_session, run, turn, handoff, &message).await
}

async fn emit_terminal_error_message(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    message: &str,
) -> Result<(), String> {
    let current_session_context = handoff.current_session_context_artifacts().await;
    let output_root = ctx.interface_var_dir().join("emitted");
    let artifact = match terminal_output_continuity(
        descriptor,
        &turn.envelope,
        &current_session_context,
        run,
        &handoff.current_run_record_bytes().await,
    ) {
        Ok(continuity) => {
            crate::handler::materialize_terminal_turn_error_payload(
                &output_root, message, continuity.as_ref(),
                serde_json::json!({"source_movement_seq": turn.seq}),
            )
        }
        Err(error) => match codex_continuity_incompatibility(&error) {
            Some(detail) => materialize_incompatible_continuity_turn_error(
                &output_root,
                turn.seq,
                detail,
                message,
            ),
            None => {
                return record_invalid_provider_context(
                    ctx, descriptor, handoff_session, run, turn,
                    format!("{message}; terminal continuity cannot be settled: {error}"),
                ).await;
            }
        },
    }
    .map_err(|error| {
        format!(
            "codex terminal error for run '{}' movement {} failed to materialize: {error}",
            run.as_str(),
            turn.seq
        )
    })?;
    let terminal_label = artifact
        .read_manifest()
        .map_err(|error| format!("codex terminal error manifest is unreadable: {error}"))?
        .label;
    let terminal_artifact_payload_blake3 = artifact_payload_blake3(&artifact)?;
    let private_config = &descriptor
        .interface_driver_config
        .as_ref()
        .ok_or_else(|| {
            format!(
                "provider interface '{}' has no interface_driver_config",
                descriptor.id.as_str()
            )
        })?
        .private_config;
    let rules = LiveStreamRules::from_private_config_or_disabled(private_config)
        .map_err(|error| error.to_string())?;
    let stream = seal_source_turn_stream(
        &ctx.interface_var_dir().join("streams"),
        &rules,
        descriptor.id.as_str(),
        run.as_str(),
        turn.seq,
        &terminal_label,
        &terminal_artifact_payload_blake3,
    )?;
    let terminal = ProviderEmission {
        artifact,
        route: ProviderEmissionRoute::ClientProjection,
    };
    if let Some(stream) = stream {
        match handoff.hand_observation(ProviderEmission {
            artifact: stream,
            route: ProviderEmissionRoute::ClientProjection,
        }, false).await? {
            ProviderHandoffOutcome::Admitted { .. } => {}
            ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => {
                return Err(format!(
                    "terminal stream observation stopped by authoring failure '{failure_class}' at movement {failure_seq}"
                ));
            }
        }
    }
    handoff.hand_final(terminal).await.map(|_| ())
}

async fn recover_interrupted_result_without_context(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    interrupted_effects: &InterruptedEffectArbiter,
    control: &CodexTurnControlBroker,
    run: &RunId,
    turn: &QueuedTurn,
) -> Result<bool, String> {
    let Some(continuity) = interrupted_effects.durable_continuity(run).await? else {
        return Ok(false);
    };
    if !exact_interrupted_result_source(&turn.envelope, &continuity)? {
        return Ok(false);
    }
    let Some((emissions, closure)) = recover_sealed_interrupted_result(
        ctx, descriptor, turn, &continuity,
    )? else { return Ok(false); };
    if !interrupted_effects.begin_result(run, turn.seq, &turn.envelope).await? {
        return Err("sealed interrupted result does not match the durable exact effect".into());
    }
    let handoff = handoff_session.failed_source_handoff(
        ctx, descriptor, run, &turn.envelope, turn.seq,
    )?;
    admit_interrupted_result(
        ctx, &handoff, control, Some(interrupted_effects), run, turn.seq, &closure, emissions,
    ).await?;
    Ok(true)
}

fn recover_sealed_interrupted_result(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    turn: &QueuedTurn,
    continuity: &crate::handler::CodexContinuityState,
) -> Result<Option<(Vec<ProviderEmission>, ArtifactRef)>, String> {
    let private_config = &descriptor.interface_driver_config.as_ref()
        .ok_or("sealed interruption recovery has no declared provider config")?.private_config;
    let rules = LiveStreamRules::from_private_config_or_disabled(private_config)
        .map_err(|error| error.to_string())?;
    let identity = result_turn_identity(continuity)?;
    let Some((stream, payload_hash)) = recover_sealed_terminal_stream(
        &ctx.interface_var_dir().join("streams"), &rules, &identity, turn.seq,
        TURN_INTERRUPTED_RESULT_LABEL,
    )? else { return Ok(None); };
    let output_root = ctx.interface_var_dir().join("emitted").canonicalize()
        .map_err(|error| error.to_string())?;
    let slug = TURN_INTERRUPTED_RESULT_LABEL.replace('.', "_");
    let manifest = octopus_interfaces::InterfaceArtifactManifest {
        label: TURN_INTERRUPTED_RESULT_LABEL.to_string(),
        payload_path: output_root.join(format!("{slug}-{payload_hash}.payload")),
        payload_content_hash: Some(payload_hash),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    let manifest_hash = ContentHash::of_bytes(&manifest_bytes).to_hex();
    let path = output_root.join(format!("{slug}-{manifest_hash}.artifact.json"));
    if std::fs::read(&path).map_err(|error| error.to_string())? != manifest_bytes {
        return Err("sealed interruption recovery changed its terminal manifest".into());
    }
    let terminal = octopus_interfaces::InterfaceArtifactFile::new(path).map_err(|error| error.to_string())?;
    let closure = if crate::handler::codex_continuity_has_interrupted_pending(continuity) {
        interrupted_result_closure_ref(&terminal, continuity, &turn.envelope.artifact, &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?
    } else {
        native_terminal_result_closure_ref(&terminal, continuity, &turn.envelope, &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?
    }
        .ok_or("sealed interruption has no exact result closure")?;
    let mut emissions = vec![
        ProviderEmission { artifact: stream, route: ProviderEmissionRoute::ClientProjection },
    ];
    emissions.extend(recover_retained_native_outputs(&terminal, &output_root, &codex_context_output_labels(descriptor).map_err(|error| error.to_string())?)?);
    emissions.push(ProviderEmission { artifact: terminal, route: ProviderEmissionRoute::ClientProjection });
    Ok(Some((emissions, closure)))
}

fn native_result_admitted_prefix(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_seq: u64,
    emissions: &[ProviderEmission],
    snapshots: &BTreeSet<String>,
    context_labels: &BTreeSet<String>,
) -> Result<Vec<ProviderHandoffOutcome>, String> {
    let expected = emissions.iter().map(|emission| {
        Ok((emission.artifact.read_manifest().map_err(|error| error.to_string())?.label,
            materialized_artifact_ref(&emission.artifact)?,
            emission.artifact.read_payload_bytes().map_err(|error| error.to_string())?))
    }).collect::<Result<Vec<_>, String>>()?;
    if expected.iter().map(|item| &item.0).collect::<BTreeSet<_>>().len() != expected.len() {
        return Err("native result group repeats a recovery label".into());
    }
    let settled = serde_json::from_slice::<serde_json::Value>(&expected.last().ok_or("native result group is empty")?.2)
        .map_err(|error| error.to_string())?["codex_continuity"]["context_generation"].as_u64()
        .ok_or("native result group has no settled generation")?;
    let (mut file, length, limit) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (offset, next) = authoritative_source_scan_start(ctx, &mut file, length, limit, &decoder, source_seq);
    let mut source_verified = false;
    let mut prefix = Vec::new();
    let scan = scan_authoritative_snapshot(ctx, &mut file, length, limit, &decoder, offset, next, None, |movement| {
        if movement.seq.0 < source_seq { return Ok(None::<()>); }
        if movement.seq.0 == source_seq {
            if !movement_is_provider_source(ctx, run, &movement) {
                return Err("native result prefix changed its exact provider source".into());
            }
            source_verified = true;
            return Ok(None);
        }
        if !source_verified { return Err("native result prefix passed its unverified source".into()); }
        let Some(reference) = movement.artifact.as_ref() else { return Ok(None); };
        if !movement_is_artifact_admission(ctx, run, reference, &movement)
            || !expected.iter().any(|item| artifact_ref_has_label(reference, &item.0)) {
            return Ok(None);
        }
        let runtime = movement.runtime().ok_or("native result prefix has no runtime coordinates")?;
        let hash = runtime.run_record_hash.as_deref().and_then(ContentHash::from_hex)
            .ok_or("native result prefix has no pinned run snapshot")?;
        let path = ctx.blobs_dir.join(hash.to_hex());
        if std::fs::metadata(&path).map_err(|error| error.to_string())?.len() > ctx.required_host_io_limits()?.max_payload_bytes {
            return Err("native result prefix run snapshot exceeds its declared payload limit".into());
        }
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        if ContentHash::of_bytes(&bytes) != hash { return Err("native result prefix run snapshot changed its hash".into()); }
        let record = octopus_runtime::run_record::RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
        let state = record.interface_state.get(&ctx.interface).ok_or("native result prefix lost its provider state")?;
        let pending = state.get("pending_handoff").ok_or("native result prefix has no pending handoff")?;
        let owner = pending.get("source_movement_seq").and_then(serde_json::Value::as_u64)
            .ok_or("native result prefix pending source is malformed")?;
        if owner != source_seq { return Ok(None); }
        let (manifest, actual) = read_pinned_provider_artifact(ctx, reference, "native result prefix")?;
        let index = expected.iter().position(|item| item.0 == manifest.label)
            .ok_or("native result prefix manifest conflicts with its artifact label")?;
        if index != prefix.len() { return Err("native result admissions are not an exact contiguous prefix".into()); }
        let remaining = expected[index + 1..].iter().filter(|item| context_labels.contains(&item.0)).count() as u64;
        if state.get("context_generation").and_then(serde_json::Value::as_u64) != settled.checked_sub(remaining) {
            return Err("native result prefix changed its pinned authored context generation".into());
        }
        let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or("native result prefix checkpoint is absent")?;
        if record.id != *run || record.ee != runtime.ee || record.worker != runtime.worker
            || !record.active_checkpoints.contains(&checkpoint.id) || checkpoint.epoch.0 != runtime.epoch
            || checkpoint.inbound_interface() != Some(&ctx.interface)
            || !checkpoint.required_verified_artifacts.is_empty()
        { return Err("native result prefix crossed its pinned checkpoint scope".into()); }
        let pinned_checkpoint = serde_json::json!({
            "id":checkpoint.id,"epoch":checkpoint.epoch.0,"interface_binding":checkpoint.interface_binding,
            "outbound_interface_bindings":checkpoint.outbound_interface_bindings,
            "artifact_bindings":checkpoint.artifact_bindings.iter().map(|binding| &binding.id).collect::<Vec<_>>(),
            "required_verified_artifacts":checkpoint.required_verified_artifacts,
        });
        let items = pending.get("items").and_then(serde_json::Value::as_array)
            .ok_or("native result prefix pending items are malformed")?;
        let matched = items.iter().filter(|item| item.get("artifact_id") == Some(&serde_json::json!(reference.id))
            && item.get("artifact_content_hash").and_then(serde_json::Value::as_str) == Some(reference.content.to_hex().as_str())
            && item.get("artifact_label").and_then(serde_json::Value::as_str) == Some(manifest.label.as_str())
            && item.get("checkpoint") == Some(&pinned_checkpoint)).count();
        if matched != 1 || pending.get("terminal").and_then(serde_json::Value::as_bool) != Some(index + 1 == expected.len()) {
            return Err("native result prefix differs from its exact pinned pending item".into());
        }
        let raw = &expected[index];
        if snapshots.contains(&manifest.label) {
            let mut value: serde_json::Value = serde_json::from_slice(&actual).map_err(|error| error.to_string())?;
            value.as_object_mut().ok_or("native result snapshot is not an object")?.remove("session_context")
                .ok_or("native result snapshot lost its declared session context")?;
            if value != serde_json::from_slice::<serde_json::Value>(&raw.2).map_err(|error| error.to_string())? {
                return Err("native result prefix changed its immutable semantic payload".into());
            }
        } else if *reference != raw.1 || actual != raw.2 {
            return Err("native result prefix changed its immutable artifact".into());
        }
        prefix.push(ProviderHandoffOutcome::Admitted { seq: movement.seq.0, artifact: reference.clone() });
        Ok(None)
    })?;
    ensure_arbitration_snapshot_length(ctx, &file, length)?;
    if !scan.complete_boundary || !source_verified { return Err("native result prefix lacks a complete authoritative ledger boundary".into()); }
    Ok(prefix)
}

async fn admit_interrupted_result(
    ctx: &InterfaceLiftContext,
    handoff: &ProviderOutputHandoff,
    control: &CodexTurnControlBroker,
    interrupted_effects: Option<&InterruptedEffectArbiter>,
    run: &RunId,
    source_movement_seq: u64,
    closure: &ArtifactRef,
    emissions: Vec<ProviderEmission>,
) -> Result<InterruptedResultAdmission, String> {
    let terminal = emissions.last().ok_or("interrupted-result group has no terminal closure")?;
    if materialized_artifact_ref(&terminal.artifact)? != *closure {
        return Err("interrupted-result terminal differs from its exact closure".into());
    }
    let expected_payload: serde_json::Value = serde_json::from_slice(
        &terminal.artifact.read_payload_bytes().map_err(|error| error.to_string())?,
    ).map_err(|error| error.to_string())?;
    let native = expected_payload.get("native_terminal_status").is_some();
    let native_recovery = if native {
        let recovered = handoff.recover_source_with_outcomes().await?;
        if matches!(recovered.0, ProviderSourceRecovery::Bridged { .. }) {
            control.finish();
            return Ok(InterruptedResultAdmission::Bridged);
        }
        Some(recovered)
    } else { None };
    if let Some(interrupted_effects) = interrupted_effects {
        interrupted_effects.begin_closing(run, source_movement_seq, closure.clone()).await?;
    }
    let expected = emissions.len();
    let mut outcomes = Vec::new();
    if native {
        let descriptor = ctx.descriptor_snapshot.as_ref().ok_or("native result admission has no declared provider")?;
        let labels = codex_context_output_labels(descriptor).map_err(|error| error.to_string())?;
        let snapshots = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
        let (recovery, recovered) = native_recovery.ok_or("native result has no source recovery")?;
        outcomes = native_result_admitted_prefix(ctx, run, source_movement_seq, &emissions, &snapshots, &labels)?;
        for outcome in &recovered {
            if !outcomes.iter().any(|admitted| matches!((admitted, outcome),
                (ProviderHandoffOutcome::Admitted {seq:a,artifact:x},ProviderHandoffOutcome::Admitted {seq:b,artifact:y}) if a == b && x == y)) {
                return Err("native result prefix conflicts with checked source recovery".into());
            }
        }
        match recovery {
            ProviderSourceRecovery::Process if !outcomes.is_empty() => return Err("native result prefix has no checked pending frontier".into()),
            ProviderSourceRecovery::Completed if outcomes.len() != expected => return Err("native result source completed before its whole exact group".into()),
            ProviderSourceRecovery::Interrupted { last_artifact_label } if outcomes.is_empty()
                || emissions[outcomes.len()-1].artifact.read_manifest().map_err(|error| error.to_string())?.label != last_artifact_label
                => return Err("native result prefix changed its checked last pending artifact".into()),
            _ => {}
        }
        let input_context = handoff.input_changes_context().await;
        let sealed_input = expected_payload.get("native_terminal_input_context").and_then(serde_json::Value::as_bool)
            .ok_or("native result closure has no exact input consumption")?;
        if (outcomes.is_empty() && input_context != sealed_input) || (!outcomes.is_empty() && input_context) {
            return Err("native result input consumption conflicts with checked source recovery".into());
        }
        let current = octopus_runtime::run_record::RunRecord::from_bytes(&handoff.current_run_record_bytes().await)
            .map_err(|error| error.to_string())?;
        let generation = current.interface_state.get(&ctx.interface).and_then(|state| state.get("context_generation"))
            .and_then(serde_json::Value::as_u64).ok_or("native result handoff has no checked context generation")?;
        let delta = emissions[outcomes.len()..].iter().map(|emission| emission.artifact.read_manifest()
            .map(|manifest| u64::from(labels.contains(&manifest.label))).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?.into_iter().sum::<u64>();
        let settled = expected_payload.get("codex_continuity").and_then(|state| state.get("context_generation"))
            .and_then(serde_json::Value::as_u64).ok_or("native result closure has no exact settled generation")?;
        if generation.checked_add(u64::from(input_context)).and_then(|value| value.checked_add(delta)) != Some(settled) {
            return Err("native result closure conflicts with checked authored context generation".into());
        }
    }
    let suffix = emissions.into_iter().skip(outcomes.len()).collect::<Vec<_>>();
    if !suffix.is_empty() { outcomes.extend(handoff.hand_batch(suffix).await?); }
    if outcomes.len() != expected {
        return Err("exact result group lost its ordered gate outcomes".into());
    }
    if native {
        let current = octopus_runtime::run_record::RunRecord::from_bytes(&handoff.current_run_record_bytes().await)
            .map_err(|error| error.to_string())?;
        if current.interface_state.get(&ctx.interface).and_then(|state| state.get("context_generation"))
            != expected_payload.get("codex_continuity").and_then(|state| state.get("context_generation")) {
            return Err("native result admitted closure differs from its checked authored context".into());
        }
    }
    for outcome in &outcomes {
        if let ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } = outcome {
            return Err(format!("exact result group was replaced by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"));
        }
    }
    match outcomes.last().ok_or("exact result group has no admitted closure")? {
        ProviderHandoffOutcome::Admitted { seq, artifact } => {
            let (manifest, bytes) = read_pinned_provider_artifact(ctx, artifact, "admitted native result closure")?;
            let mut payload: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            payload.as_object_mut().ok_or("admitted native result closure is not an object")?.remove("session_context");
            if manifest.label != TURN_INTERRUPTED_RESULT_LABEL || payload != expected_payload {
                return Err("exact result group admitted a different final closure payload".into());
            }
            validate_artifact_admission(ctx, run, source_movement_seq, &DurableArtifactAdmission {
                artifact: artifact.clone(), movement_seq: *seq,
            })?;
            if let Some(interrupted_effects) = interrupted_effects {
                interrupted_effects.complete_closing(run, source_movement_seq, closure, &DurableArtifactAdmission {
                    artifact: artifact.clone(), movement_seq: *seq,
                }).await?;
            }
        }
        ProviderHandoffOutcome::TerminalFailure { failure_seq, failure_class, .. } => {
            return Err(format!(
                "interrupted-result closure was replaced by terminal run-authoring failure '{failure_class}' at movement {failure_seq}"
            ));
        }
    }
    control.finish();
    Ok(InterruptedResultAdmission::Admitted(outcomes.len()))
}

fn required_policy_u32(envelope: &InterfaceInvocationEnvelope, name: &str) -> Result<u32, String> {
    let values = envelope
        .interface_policy()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "codex invocation carries no formed interface policy".to_string())?;
    let value = values
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("codex formed interface policy carries no unsigned '{name}'"))?;
    u32::try_from(value).map_err(|_| format!("codex formed interface policy '{name}' exceeds u32"))
}

fn enforce_formed_model_policy(
    descriptor: &InterfaceDescriptor,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<(), String> {
    let label = envelope
        .read_artifact_manifest()
        .map_err(|error| error.to_string())?
        .label;
    if label.starts_with("provider.account.")
        || label.starts_with("provider.models.")
        || label.starts_with("provider.capabilities")
    {
        return Ok(());
    }
    let metadata = &descriptor
        .interface_driver_config
        .as_ref()
        .ok_or_else(|| {
            format!(
                "Codex interface '{}' has no interface_driver_config",
                descriptor.id
            )
        })?
        .private_config;
    let model = metadata
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|model| {
            !model.is_empty() && model.trim() == *model && !model.contains(['\r', '\n', '\0'])
        })
        .ok_or_else(|| {
            format!(
                "Codex interface '{}' has no valid provider-local model selection",
                descriptor.id
            )
        })?;
    let policy = envelope
        .interface_policy()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Codex invocation carries no formed interface policy".to_string())?;
    let values = policy
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Codex formed interface policy models must be a string array".to_string())?;
    if values.is_empty() {
        return Err("Codex formed interface policy models is empty".to_string());
    }
    let mut models = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let candidate = value.as_str().filter(|candidate| {
            !candidate.is_empty()
                && candidate.trim() == *candidate
                && !candidate.contains(['\r', '\n', '\0'])
        });
        let Some(candidate) = candidate else {
            return Err(format!(
                "Codex formed interface policy models[{index}] is not a trimmed string"
            ));
        };
        if !models.insert(candidate) {
            return Err(format!(
                "Codex formed interface policy models repeats '{candidate}'"
            ));
        }
    }
    if !models.contains(model) {
        return Err(format!(
            "Codex selected model '{model}' is outside formed interface policy models"
        ));
    }
    Ok(())
}

fn load_own_descriptor(ctx: &InterfaceLiftContext) -> Result<InterfaceDescriptor, String> {
    ctx.resolved_interface_descriptor()
}

fn load_current_own_descriptor(ctx: &InterfaceLiftContext) -> Result<InterfaceDescriptor, String> {
    let interface_dir = ctx.interface_registry_dir();
    let registry_root = interface_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            format!(
                "interface registry directory '{}' has no registry root",
                interface_dir.display()
            )
        })?;
    octopus_interface_transport::resolve_interface_descriptor(registry_root, &ctx.interface)?
        .ok_or_else(|| {
            format!(
                "current descriptor for interface '{}' disappeared from the registry",
                ctx.interface.as_str()
            )
        })
}

fn resolve_provider_auth_store(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    mode: &ProviderConnectionMode,
) -> Result<PathBuf, String> {
    match mode {
        ProviderConnectionMode::Native => resolve_declared_private_auth_store(ctx, descriptor),
        ProviderConnectionMode::OpenCodex(_) => prepare_private_auth_store_home(
            &ctx.interface_var_dir(),
            Path::new("codex-home"),
            ctx.required_host_io_limits()?.max_payload_bytes,
        ),
    }
}

fn resolve_declared_private_auth_store(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
) -> Result<PathBuf, String> {
    let Some(residual) = descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.residual_ambient.as_ref())
        .and_then(serde_json::Value::as_object)
    else {
        return Err(
            "Codex provider descriptor must declare residual_ambient.private_auth_store"
                .to_string(),
        );
    };
    let store = residual.get("private_auth_store").ok_or_else(|| {
        "Codex provider descriptor must declare residual_ambient.private_auth_store".to_string()
    })?;
    let store = store
        .as_object()
        .ok_or_else(|| "private_auth_store must be a JSON object".to_string())?;
    let expected = BTreeSet::from(["auth_file_env"]);
    let observed = store.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if observed != expected {
        return Err("private_auth_store contains undeclared fields".to_string());
    }
    let auth_file_env = store
        .get("auth_file_env")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "private_auth_store.auth_file_env must be a string".to_string())?;
    if auth_file_env != MACHINE_CODEX_AUTH_FILE_ENV {
        return Err(format!(
            "private_auth_store.auth_file_env must be '{MACHINE_CODEX_AUTH_FILE_ENV}'"
        ));
    }
    let declared_auth_file = std::env::var_os(MACHINE_CODEX_AUTH_FILE_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!("private_auth_store requires inherited {MACHINE_CODEX_AUTH_FILE_ENV}")
        })?;
    if !declared_auth_file.is_absolute() {
        return Err(format!(
            "{MACHINE_CODEX_AUTH_FILE_ENV} must name an absolute Codex auth file"
        ));
    }
    let maximum_bytes = ctx.required_host_io_limits()?.max_payload_bytes;
    prepare_machine_codex_auth_store_home(&declared_auth_file, maximum_bytes)
}

fn prepare_machine_codex_auth_store_home(
    declared_auth_file: &Path,
    maximum_bytes: u64,
) -> Result<PathBuf, String> {
    if declared_auth_file.file_name() != Some(std::ffi::OsStr::new("auth.json")) {
        return Err(format!(
            "machine Codex auth file '{}' must end in auth.json",
            declared_auth_file.display()
        ));
    }
    let declared_home = declared_auth_file.parent().ok_or_else(|| {
        format!(
            "machine Codex auth file '{}' has no parent directory",
            declared_auth_file.display()
        )
    })?;
    let parent = declared_home.parent().ok_or_else(|| {
        format!(
            "machine Codex home '{}' has no parent directory",
            declared_home.display()
        )
    })?;
    let name = declared_home.file_name().ok_or_else(|| {
        format!(
            "machine Codex home '{}' has no final path component",
            declared_home.display()
        )
    })?;
    let prepared = prepare_private_auth_store_home(parent, Path::new(name), maximum_bytes)?;
    if prepared != declared_home {
        return Err(format!(
            "machine Codex home resolved to '{}' instead of declared '{}'",
            prepared.display(),
            declared_home.display()
        ));
    }
    Ok(prepared)
}

fn prepare_private_auth_store_home(
    root: &Path,
    relative_home: &Path,
    maximum_bytes: u64,
) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(root).map_err(|error| {
        format!(
            "private auth-store root '{}' cannot be resolved: {error}",
            root.display()
        )
    })?;
    let mut auth_store_home = root.clone();
    for component in relative_home.components() {
        let std::path::Component::Normal(segment) = component else {
            return Err("private auth-store home is not relative".to_string());
        };
        auth_store_home.push(segment);
        match std::fs::symlink_metadata(&auth_store_home) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(format!(
                    "private auth-store path '{}' is not a regular directory",
                    auth_store_home.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&auth_store_home).map_err(|error| {
                    format!(
                        "private auth-store path '{}' cannot be created: {error}",
                        auth_store_home.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "private auth-store path '{}' cannot be inspected: {error}",
                    auth_store_home.display()
                ));
            }
        }
    }
    let auth_store_home = std::fs::canonicalize(&auth_store_home).map_err(|error| {
        format!(
            "private auth-store home '{}' cannot be resolved: {error}",
            auth_store_home.display()
        )
    })?;
    if !auth_store_home.starts_with(&root) {
        return Err(format!(
            "private auth-store home '{}' escapes its declared root",
            auth_store_home.display()
        ));
    }
    let metadata = std::fs::symlink_metadata(&auth_store_home).map_err(|error| {
        format!(
            "private auth-store home '{}' cannot be inspected: {error}",
            auth_store_home.display()
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "private auth-store home '{}' is not a regular directory",
            auth_store_home.display()
        ));
    }
    let auth_file = auth_store_home.join("auth.json");
    match std::fs::symlink_metadata(&auth_file) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            if metadata.len() > maximum_bytes {
                return Err(format!(
                    "private auth-store file '{}' exceeds {} bytes",
                    auth_file.display(),
                    maximum_bytes
                ));
            }
            let bytes = std::fs::read(&auth_file).map_err(|error| {
                format!(
                    "private auth-store file '{}' cannot be read: {error}",
                    auth_file.display()
                )
            })?;
            if u64::try_from(bytes.len()).ok() != Some(metadata.len()) {
                return Err(format!(
                    "private auth-store file '{}' changed while being read",
                    auth_file.display()
                ));
            }
        }
        Ok(_) => {
            return Err(format!(
                "private auth-store file '{}' is not a regular file",
                auth_file.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "private auth-store file '{}' cannot be inspected: {error}",
                auth_file.display()
            ));
        }
    }
    Ok(auth_store_home)
}

#[cfg(test)]
mod tests {
    use super::*;
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/native_terminal_handoff.rs"));
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/turn_deferral.rs"));

    use futures::{SinkExt, StreamExt};
    use octopus_interface_transport::InterfaceHostIoLimits;
    use octopus_interfaces::{
        GrantProjection, InterfaceArtifactFile, InvocationMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::{Lamport, Movement, Seq, TraceId};
    use serde_json::json;

    fn continuity(
        context_generation: u64,
        pending_field: Option<&str>,
    ) -> crate::handler::CodexContinuityState {
        let mut value = json!({
            "schema_version": 1,
            "provider_interface": "world_provider_codex",
            "run": "run-a",
            "thread_id": "thread-a",
            "thread_shape_fingerprint": "a".repeat(64),
            "context_generation": context_generation,
            "static_context_fingerprint": "b".repeat(64)
        });
        if let Some(pending_field) = pending_field {
            value[pending_field] = json!({
                "kind": "dynamic_tool",
                "turn_id": "turn-a",
                "call_id": "call-a",
                "tool": "coder.exec"
            });
        }
        serde_json::from_value(value).unwrap()
    }

    fn descriptor() -> InterfaceDescriptor {
        serde_json::from_str(include_str!("../../../interface.json")).unwrap()
    }

    #[test]
    fn current_descriptor_read_detects_selection_drift_after_lift() {
        let temp = tempfile::tempdir().unwrap();
        let lifted = descriptor();
        let ctx = context(temp.path(), &lifted);
        let mut current = lifted.clone();
        current
            .interface_driver_config
            .as_mut()
            .unwrap()
            .private_config
            .insert("model".to_string(), json!("gpt-5.6-terra"));
        let interface_dir = temp.path().join("registry/interface/world_provider_codex");
        std::fs::create_dir_all(&interface_dir).unwrap();
        std::fs::write(
            interface_dir.join("interface.json"),
            serde_json::to_vec_pretty(&current).unwrap(),
        )
        .unwrap();

        assert_eq!(load_own_descriptor(&ctx).unwrap(), lifted);
        assert_eq!(load_current_own_descriptor(&ctx).unwrap(), current);
    }

    fn bound(id: &str, payload: serde_json::Value) -> octopus_interfaces::ArtifactBindingPayload {
        let bytes = serde_json::to_vec(&payload).unwrap();
        octopus_interfaces::ArtifactBindingPayload::new(
            ArtifactRef {
                id: ArtifactId::new(id),
                content: ContentHash::of_bytes(&bytes),
            },
            bytes,
        )
    }

    fn pinned_test_run_record() -> Vec<u8> {
        let inbound = octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("world_operator"));
        let outbound = octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("world_provider_codex"));
        let bytes = serde_json::to_vec(&json!({"id":"run-a","ee":"ee-a","worker":"system_worker_test",
            "trace":"trace-a","status":"running","checkpoints":[{"id":"checkpoint-a","run":"run-a",
                "trace":"trace-a","epoch":1,"interface_binding":inbound,"outbound_interface_bindings":[outbound]}],
            "active_checkpoints":["checkpoint-a"]})).unwrap();
        octopus_runtime::run_record::RunRecord::from_bytes(&bytes).unwrap();
        bytes
    }

    fn runtime_metadata() -> InvocationMetadata {
        let mut grant = GrantProjection::default();
        grant.interface_policy.insert("world_provider_codex".into(), BTreeMap::from([
            ("web_search_modes".into(), json!(["live"])),
        ]));
        InvocationMetadata::new(grant).with_runtime(
            RuntimeInvocationMetadata {
                run: RunId::new("run-a"),
                ee: octopus_descriptors::EeId::new("ee-a"),
                worker: octopus_descriptors::InterfaceId::new("system_worker_test"),
                checkpoint: octopus_descriptors::CheckpointId::new("checkpoint-a"),
                interface: octopus_descriptors::InterfaceId::new("world_provider_codex"),
                epoch: 1,
                run_record_hash: Some(ContentHash::of_bytes(&pinned_test_run_record()).to_hex()),
            },
        )
    }

    async fn next_test_rpc(
        socket: &mut tokio_tungstenite::WebSocketStream<crate::uds::UnixStream>,
    ) -> octopus_appserver_proto::JSONRPCMessage {
        let message = socket.next().await.unwrap().unwrap();
        serde_json::from_str(&message.into_text().unwrap()).unwrap()
    }

    async fn send_test_result(
        socket: &mut tokio_tungstenite::WebSocketStream<crate::uds::UnixStream>,
        id: octopus_appserver_proto::RequestId,
        result: serde_json::Value,
    ) {
        let response = octopus_appserver_proto::JSONRPCMessage::Response(
            octopus_appserver_proto::JSONRPCResponse { id, result },
        );
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&response).unwrap().into(),
            ))
            .await
            .unwrap();
    }

    async fn send_test_notification(
        socket: &mut tokio_tungstenite::WebSocketStream<crate::uds::UnixStream>,
        notification: octopus_appserver_proto::ServerNotification,
    ) {
        let message: octopus_appserver_proto::JSONRPCMessage =
            serde_json::from_value(serde_json::to_value(notification).unwrap()).unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&message).unwrap().into(),
            ))
            .await
            .unwrap();
    }

    async fn interrupted_drain_driver(
        root: &Path,
        descriptor: &InterfaceDescriptor,
    ) -> (
        Arc<CodexAppServerInterfaceDriver>,
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::mpsc::UnboundedReceiver<String>,
        tokio::task::JoinHandle<()>,
    ) {
        let socket_path = root.join("interrupted-app-server.sock");
        let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (method_tx, method_rx) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let initialize = next_test_rpc(&mut socket).await;
            let octopus_appserver_proto::JSONRPCMessage::Request(initialize) = initialize else {
                panic!("expected initialize request")
            };
            assert_eq!(initialize.method, "initialize");
            send_test_result(
                &mut socket,
                initialize.id,
                json!({"userAgent": "codex/test", "codexHome": "/tmp"}),
            )
            .await;
            assert!(matches!(
                next_test_rpc(&mut socket).await,
                octopus_appserver_proto::JSONRPCMessage::Notification(_)
            ));
            let mut release_rx = release_rx;
            loop {
                tokio::select! {
                    release = &mut release_rx => {
                        release.unwrap();
                        send_test_notification(
                            &mut socket,
                            octopus_appserver_proto::ServerNotification::Warning(
                                octopus_appserver_proto::WarningNotification {
                                    thread_id: Some("thread-a".to_string()),
                                    message: "partial cancelled-turn activity".to_string(),
                                },
                            ),
                        )
                        .await;
                        send_test_notification(
                            &mut socket,
                            octopus_appserver_proto::ServerNotification::Error(
                                octopus_appserver_proto::ErrorNotification {
                                    error: octopus_appserver_proto::TurnError {
                                        message: "cancelled turn transport failed".to_string(),
                                        codex_error_info: None,
                                        additional_details: None,
                                    },
                                    will_retry: false,
                                    thread_id: "thread-a".to_string(),
                                    turn_id: "turn-a".to_string(),
                                },
                            ),
                        )
                        .await;
                        socket.close(None).await.unwrap();
                        break;
                    }
                    message = socket.next() => {
                        let Some(message) = message else {
                            break;
                        };
                        let message = message.unwrap();
                        let message: octopus_appserver_proto::JSONRPCMessage =
                            serde_json::from_str(&message.into_text().unwrap()).unwrap();
                        if let octopus_appserver_proto::JSONRPCMessage::Request(request) = message {
                            method_tx.send(request.method.clone()).unwrap();
                            let result = match request.method.as_str() {
                                "thread/read" => {
                                    assert_eq!(serde_json::from_value::<octopus_appserver_proto::ThreadReadParams>(request.params.clone().unwrap()).unwrap().include_turns,false);
                                    json!({"thread":octopus_appserver_proto::Thread {
                                        id:"thread-a".into(),session_id:"thread-a".into(),forked_from_id:None,parent_thread_id:None,
                                        preview:String::new(),ephemeral:false,model_provider:"fixture".into(),created_at:1,updated_at:1,recency_at:Some(1),
                                        status:octopus_appserver_proto::ThreadStatus::Idle,path:None,cwd:octopus_util_abspath::AbsolutePathBuf::from_absolute_path("/tmp").unwrap(),
                                        cli_version:"test".into(),source:octopus_appserver_proto::SessionSource::Exec,thread_source:None,
                                        agent_nickname:None,agent_role:None,git_info:None,name:None,turns:vec![],
                                    }})
                                }
                                "thread/turns/list" => json!({"data":[{"id":"turn-a","items":[],"itemsView":"notLoaded","status":"interrupted","error":null}],"nextCursor":null}),
                                "thread/items/list" => json!({"data":[],"nextCursor":null}),
                                _ => json!({"turnId":"turn-a"}),
                            };
                            send_test_result(&mut socket,request.id,result).await;
                        }
                    }
                }
            }
        });
        let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
            endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
                socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(
                    &socket_path,
                )
                .unwrap(),
            },
            client_name: "octopus-test".to_string(),
            client_version: "0".to_string(),
            experimental_api: false,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: 8,
        })
        .await
        .unwrap();
        let driver = Arc::new(CodexAppServerInterfaceDriver::new_with_config(
            crate::AppServerClient::Remote(client),
            descriptor.interface_driver_config.as_ref(),
        ));
        (driver, release_tx, method_rx, server)
    }

    fn context(root: &Path, descriptor: &InterfaceDescriptor) -> InterfaceLiftContext {
        let (arrivals, _) = tokio::sync::mpsc::channel(1);
        let record = pinned_test_run_record();
        let blobs = root.join("ledger/blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(blobs.join(ContentHash::of_bytes(&record).to_hex()), record).unwrap();
        InterfaceLiftContext::unprivileged(
            descriptor.id.clone(),
            root.join("registry/interface/world_provider_codex"),
            Some(descriptor.clone()),
            root.join("var/provider"),
            root.join("ledger/movements.jsonl"),
            root.join("ledger/blobs"),
            root.join("ledger/ledger-index.sqlite"),
            BTreeMap::new(),
            arrivals,
            Default::default(),
        )
        .with_host_io_limits(InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 1_048_576,
        })
        .unwrap()
    }

    fn movement(
        seq: u64,
        kind: MovementKind,
        from: Endpoint,
        to: Endpoint,
        artifact: ArtifactRef,
    ) -> Movement {
        let invocation_metadata = matches!(
            &kind,
            MovementKind::InterfaceArrival | MovementKind::InterfaceOutbound
        )
        .then(|| runtime_metadata().into());
        Movement {
            seq: Seq(seq),
            lamport: Lamport(seq + 1),
            recorded_at_unix_ms: seq + 1,
            trace: TraceId(format!("trace-{seq}")),
            kind,
            from,
            to,
            artifact: Some(artifact),
            invocation_metadata,
        }
    }

    fn write_movements(path: &Path, movements: &[Movement]) {
        let mut bytes = Vec::new();
        for movement in movements {
            serde_json::to_writer(&mut bytes, movement).unwrap();
            bytes.push(b'\n');
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        sync_ledger_index(path);
    }

    fn append_movement(path: &Path, movement: &Movement) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        serde_json::to_writer(&mut file, movement).unwrap();
        std::io::Write::write_all(&mut file, b"\n").unwrap();
        file.sync_all().unwrap();
        drop(file);
        sync_ledger_index(path);
    }

    fn sync_ledger_index(path: &Path) {
        let log = octopus_ledger_store::DurableLog::open(path).unwrap();
        octopus_ledger_index::LedgerSqliteIndex::open_healthy(
            &path.parent().unwrap().join("ledger-index.sqlite"),
            &log,
        )
        .unwrap();
    }

    fn artifact(root: &Path, label: &str, value: serde_json::Value) -> InterfaceArtifactFile {
        InterfaceArtifactFile::write(root, label, &serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn cache_artifact(ctx: &InterfaceLiftContext, artifact: &InterfaceArtifactFile) -> ArtifactRef {
        let limits = ctx.required_host_io_limits().unwrap();
        let captured = artifact
            .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
            .unwrap();
        let artifact_ref = materialized_artifact_ref(artifact).unwrap();
        let manifest: octopus_interfaces::InterfaceArtifactManifest =
            serde_json::from_slice(&captured.manifest_bytes).unwrap();
        std::fs::create_dir_all(&ctx.blobs_dir).unwrap();
        std::fs::write(
            ctx.blobs_dir.join(artifact_ref.content.to_hex()),
            captured.manifest_bytes,
        )
        .unwrap();
        std::fs::write(
            ctx.blobs_dir
                .join(manifest.payload_content_hash.as_deref().unwrap()),
            captured.payload_bytes,
        )
        .unwrap();
        artifact_ref
    }

    async fn upgrade_deferred_fixture(
        ctx: &InterfaceLiftContext,
        descriptor: &InterfaceDescriptor,
        run: &RunId,
        turn: &QueuedTurn,
    ) {
        let path = deferred_provider_turn_path(ctx, turn.seq);
        let expected: DeferredProviderTurnJournal = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let captured = turn.envelope.artifact_file.read_bounded(1_048_576, 1_048_576).unwrap();
        let old = DeferredProviderTurnJournalV2 {
            schema_version: 2, source_movement_seq: turn.seq, run_id: run.to_string(),
            artifact: turn.envelope.artifact.clone(),
            manifest_path: turn.envelope.artifact_file.manifest_path().to_path_buf(),
            manifest_bytes: captured.manifest_bytes, payload_bytes: captured.payload_bytes,
            metadata: turn.envelope.metadata.clone().unwrap(), release: expected.release.clone(),
            artifact_binding_payloads: turn.envelope.artifact_binding_payloads.iter().map(|bound| {
                let bytes = bound.read_bytes().unwrap().into_owned();
                DeferredArtifactBindingPayloadV2 { artifact: bound.artifact.clone(),
                    bytes_content: ContentHash::of_bytes(&bytes), bytes }
            }).collect(),
        };
        let bytes = serde_json::to_vec_pretty(&old).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(load_deferred_provider_turns(ctx, descriptor).is_err());
        assert!(publish_deferred_provider_journal_upgrade(&path, b"changed v2 receipt", &expected).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        #[cfg(unix)]
        let old_allocated = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&path).unwrap().blocks() * 512
        };
        let payload = ctx.blobs_dir.join(&expected.payload_content_hash);
        let original = std::fs::read(&payload).unwrap();
        std::fs::write(&payload, b"corrupt canonical input").unwrap();
        assert!(upgrade_deferred_provider_turn_journals(ctx, descriptor).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::write(&payload, &original).unwrap();
        std::fs::remove_file(&old.manifest_path).unwrap();
        std::fs::remove_file(&captured.manifest.payload_path).unwrap();
        upgrade_deferred_provider_turn_journals(ctx, descriptor).await.unwrap();
        let upgraded = std::fs::read(&path).unwrap();
        assert_eq!(serde_json::from_slice::<DeferredProviderTurnJournal>(&upgraded).unwrap(), expected);
        assert!(upgraded.len() < bytes.len());
        let loaded = load_deferred_provider_turns(ctx, descriptor).unwrap();
        assert_eq!(loaded[&turn.seq].turn.envelope.metadata, turn.envelope.metadata);
        assert_eq!(loaded[&turn.seq].turn.envelope.read_artifact_payload_bounded(1_048_576).unwrap(), original);
        for (actual, original) in loaded[&turn.seq].turn.envelope.artifact_binding_payloads.iter()
            .zip(&turn.envelope.artifact_binding_payloads)
        { assert_eq!(actual.artifact, original.artifact); assert_eq!(actual.read_bytes().unwrap().as_ref(), original.read_bytes().unwrap().as_ref()); }
        upgrade_deferred_provider_turn_journals(ctx, descriptor).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), upgraded);
        std::fs::write(&old.manifest_path, &old.manifest_bytes).unwrap();
        std::fs::write(&captured.manifest.payload_path, &old.payload_bytes).unwrap();
        eprintln!("deferred receipt v2={} v3={} bytes, bindings={}", bytes.len(), upgraded.len(), expected.artifact_bindings.len());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            eprintln!("deferred receipt stat-allocated v2={old_allocated} v3={} bytes",
                std::fs::metadata(&path).unwrap().blocks() * 512);
        }
    }

    #[tokio::test]
    async fn control_reader_progresses_other_runs_past_unready_targets() {
        for (ready_count, with_starting) in [(1_u64, false), (10, true), (12, true)] {
            let temp = tempfile::tempdir().unwrap();
            let descriptor = descriptor();
            let ctx = context(temp.path(), &descriptor);
            let controls = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
            let starting = CodexTurnControlBroker::new(None);
            if with_starting {
                starting.publish_recovered_interrupted_pending(&continuity(8, Some("interrupted_pending"))).unwrap();
                controls
                    .lock()
                    .await
                    .insert("starting".into(), starting.clone());
            }
            let source_seq = |index: u64| index + u64::from(with_starting);
            let source_count = ready_count + u64::from(with_starting) + 1;
            let mut movements = Vec::new();
            let waiting = artifact(temp.path(), "operator.message", json!({"prompt": "queued"}));
            let mut waiting = movement(
                0,
                MovementKind::InterfaceOutbound,
                Endpoint::Interface(InterfaceId::new("operator")),
                Endpoint::Interface(ctx.interface.clone()),
                cache_artifact(&ctx, &waiting),
            );
            let mut metadata = runtime_metadata();
            metadata.runtime.as_mut().unwrap().run = RunId::new(if with_starting { "starting" } else { "queued" });
            waiting.invocation_metadata = Some(metadata.into());
            movements.push(waiting);
            let mut admissions = Vec::new();
            for index in u64::from(!with_starting)..=ready_count {
                let run = if index == 0 {
                    "starting".to_string()
                } else {
                    format!("ready-{index}")
                };
                if index != 0 {
                    let broker = CodexTurnControlBroker::new(None);
                    broker.finish();
                    controls.lock().await.insert(run.clone(), broker);
                }
                let seq = source_seq(index);
                let id = format!("stop-{index}");
                let input = artifact(
                    &temp.path().join(&id),
                    "operator.turn.control",
                    json!({"action": "stop", "control_id": id}),
                );
                let mut source = movement(
                    seq,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(InterfaceId::new("operator")),
                    Endpoint::Interface(ctx.interface.clone()),
                    cache_artifact(&ctx, &input),
                );
                let mut metadata = runtime_metadata();
                metadata.runtime.as_mut().unwrap().run = RunId::new(&run);
                source.invocation_metadata = Some(metadata.clone().into());
                movements.push(source);
                let result = terminally_satisfied_control(&id, "stop").result;
                persist_control_journal(
                    &ctx,
                    seq,
                    &RunId::new(&run),
                    &id,
                    "stop",
                    TurnControlJournalPhase::Result {
                        result: result.clone(),
                    },
                )
                .unwrap();
                let output = materialize_turn_control_result(
                    &ctx.interface_var_dir().join("emitted"),
                    seq,
                    result,
                )
                .unwrap();
                let mut admission = movement(
                    source_count + index - u64::from(!with_starting),
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(ctx.interface.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    cache_artifact(&ctx, &output),
                );
                admission.invocation_metadata = Some(metadata.into());
                admissions.push(admission);
            }
            movements.extend(admissions);
            write_movements(&ctx.movements_path, &movements);
            let (progress, progress_rx) = tokio::sync::watch::channel(MainReaderProgress {
                next_seq: 0,
                revision: 0,
            });
            let (fences, fence_rx) = tokio::sync::mpsc::unbounded_channel();
            let gates = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
            let task = tokio::spawn(control_reader_life(
                ctx.clone(),
                descriptor.clone(),
                ProviderHandoffSession::new(&ctx).unwrap(),
                controls.clone(),
                gates.clone(),
                InterruptedEffectArbiter::new(ctx.clone()),
                progress_rx,
                fence_rx,
            ));
            let began = std::time::Instant::now();
            let delivered = tokio::time::timeout(std::time::Duration::from_millis(500), async {
                loop {
                    let count = (1..=ready_count)
                        .filter(|index| {
                            matches!(
                                load_control_journal(
                                    &ctx,
                                    source_seq(*index),
                                    &RunId::new(format!("ready-{index}")),
                                    &format!("stop-{index}"),
                                    "stop"
                                )
                                .unwrap()
                                .unwrap()
                                .phase,
                                TurnControlJournalPhase::Delivered { .. }
                            )
                        })
                        .count();
                    if count == ready_count as usize {
                        break count;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await;
            eprintln!(
                "control reader: ready={ready_count} elapsed_us={} completed={delivered:?}",
                began.elapsed().as_micros()
            );
            let committed = LedgerTailReader::open_named(&ctx, "turn-control")
                .unwrap()
                .next_seq();
            assert_eq!(
                committed, 0,
                "unsettled first movement must remain replayable"
            );
            let watermark = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                fence_control_reader(&fences, &RunId::new("ready-1"), Some(0)),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(watermark >= source_count);
            ctx.cancel.cancel();
            task.await.unwrap().unwrap();
            assert!(
                gates
                    .lock()
                    .await
                    .values()
                    .all(|gate| gate.strong_count() == 0)
            );
            assert_eq!(delivered.unwrap(), ready_count as usize);
            assert_eq!(
                LedgerTailReader::open_named(&ctx, "turn-control")
                    .unwrap()
                    .next_seq(),
                0
            );
            if with_starting {
                assert!(matches!(
                    load_control_journal(&ctx, 1, &RunId::new("starting"), "stop-0", "stop")
                        .unwrap()
                        .unwrap()
                        .phase,
                    TurnControlJournalPhase::Result { .. }
                ));
            }
            publish_main_reader_progress(&progress, 1);
            if with_starting { controls.lock().await.remove("starting"); }
            let mut restarted = ctx.clone();
            restarted.cancel = Default::default();
            let (fences, fence_rx) = tokio::sync::mpsc::unbounded_channel();
            let task = tokio::spawn(control_reader_life(
                restarted.clone(),
                descriptor,
                ProviderHandoffSession::new(&restarted).unwrap(),
                controls.clone(),
                gates,
                InterruptedEffectArbiter::new(restarted.clone()),
                progress.subscribe(),
                fence_rx,
            ));
            if with_starting {
                retire_controls_before_deferred_turn(&controls, &progress, &fences, &RunId::new("starting")).await.unwrap();
                assert!(matches!(load_control_journal(&ctx, 1, &RunId::new("starting"), "stop-0", "stop")
                    .unwrap().unwrap().phase, TurnControlJournalPhase::Delivered { .. }));
            }
            let runs = (1..=ready_count)
                .map(|index| RunId::new(format!("ready-{index}")))
                .chain(with_starting.then(|| RunId::new("starting")))
                .collect::<Vec<_>>();
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                futures::future::try_join_all(
                    runs.iter()
                        .map(|run| fence_control_reader(&fences, run, None)),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                LedgerTailReader::open_named(&restarted, "turn-control")
                    .unwrap()
                    .next_seq(),
                source_count
            );
            assert_eq!(
                std::fs::read(&ctx.movements_path)
                    .unwrap()
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count(),
                movements.len()
            );
            let cursor = restarted.interface_var_dir().join("ledger-cursor.turn-control.json");
            let durable = (std::fs::read(&cursor).unwrap(), std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&cursor).unwrap()));
            let mut foreign = movements.last().unwrap().clone();
            foreign.seq = Seq(movements.len() as u64);
            foreign.lamport = Lamport(movements.len() as u64 + 1);
            append_movement(&ctx.movements_path, &foreign);
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while fence_control_reader(&fences, &runs[0], None).await.unwrap() <= movements.len() as u64 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                (std::fs::read(&cursor).unwrap(), std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&cursor).unwrap())),
                durable,
                "settled foreign ledger progress stays volatile"
            );
            restarted.cancel.cancel();
            task.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn recovered_dispatch_scan_uses_the_full_authoritative_source_suffix() {
        let temp = tempfile::tempdir().unwrap();
        let descriptor = descriptor();
        let ctx = context(temp.path(), &descriptor);
        let run = RunId::new("run-a");
        let source = artifact(temp.path(), "coder.exec.result", json!({"ok": true}));
        let source_ref = cache_artifact(&ctx, &source);
        let dispatch = artifact(
            temp.path(),
            PROVIDER_TRANSPORT_DISPATCH_LABEL,
            json!({
                "label": PROVIDER_TRANSPORT_DISPATCH_LABEL,
                "provider_interface": "world_provider_codex",
                "source_movement_seq": 0,
                "attempt": 1,
                "method": "turn/start",
                "transport": "process",
                "endpoint": {"scheme": "process", "host": "local", "port": null, "path": "/turn/start"},
                "request": {"bytes": 8, "blake3": "a".repeat(64)},
                "credential_handle_fingerprint": null
            }),
        );
        let dispatch_ref = cache_artifact(&ctx, &dispatch);
        let later_source = artifact(temp.path(), "operator.message", json!({"text": "later"}));
        let later_source_ref = cache_artifact(&ctx, &later_source);
        let later_dispatch = artifact(
            temp.path(),
            PROVIDER_TRANSPORT_DISPATCH_LABEL,
            json!({
                "label": PROVIDER_TRANSPORT_DISPATCH_LABEL,
                "provider_interface": "world_provider_codex",
                "source_movement_seq": 2,
                "attempt": 1,
                "method": "turn/start",
                "transport": "process",
                "endpoint": {"scheme": "process", "host": "local", "port": null, "path": "/turn/start"},
                "request": {"bytes": 8, "blake3": "b".repeat(64)},
                "credential_handle_fingerprint": null
            }),
        );
        let later_dispatch_ref = cache_artifact(&ctx, &later_dispatch);
        write_movements(
            &ctx.movements_path,
            &[
                movement(
                    0,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_tool")),
                    Endpoint::Interface(ctx.interface.clone()),
                    source_ref,
                ),
                movement(
                    1,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(ctx.interface.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    dispatch_ref.clone(),
                ),
                movement(
                    2,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_operator")),
                    Endpoint::Interface(ctx.interface.clone()),
                    later_source_ref,
                ),
                movement(
                    3,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(ctx.interface.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    later_dispatch_ref,
                ),
            ],
        );
        let recovered = recovered_codex_handoff_artifacts(&ctx, &run, 0, &[])
            .await
            .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].movement_seq, 1);
        assert_eq!(recovered[0].artifact, dispatch_ref);
    }

    fn session_entry(
        label: &str,
        payload: serde_json::Value,
        source_artifact: ArtifactRef,
        source_movement_seq: u64,
    ) -> serde_json::Value {
        json!({
            "label": label,
            "payload": payload,
            "source_artifact": source_artifact,
            "source_movement_seq": source_movement_seq,
        })
    }

    #[tokio::test]
    async fn operator_message_is_queued_during_interrupted_draining_before_steer_dispatch() {
        let pinned_run_record = pinned_test_run_record();
        for result_before_ack in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let run = RunId::new("run-a");
            let descriptor = descriptor();
            let interrupted = continuity(8, Some("interrupted_pending"));
            let pending = continuity(7, Some("pending"));
            let mut ack_result = CodexTurnControlResult {
                control_id: "control-a".to_string(),
                action: "stop".to_string(),
                acknowledged: true,
                thread_id: Some("thread-a".to_string()),
                turn_id: Some("turn-a".to_string()),
                detail: json!({"interrupted": true, "exact_result_pending": true}),
                continuity: Some(interrupted.clone()),
            };
            let ack = materialize_turn_control_result(
                &root.join("var/provider/emitted"),
                0,
                ack_result.clone(),
            )
            .unwrap();
            let ack_ref = materialized_artifact_ref(&ack).unwrap();
            let stop = artifact(
                &root.join("stop"),
                "operator.turn.control",
                json!({"action": "stop", "control_id": "control-a"}),
            );
            let stop_ref = materialized_artifact_ref(&stop).unwrap();
            let message = artifact(
                &root.join("message"),
                "operator.message",
                json!({"prompt": "continue"}),
            );
            let message_ref = materialized_artifact_ref(&message).unwrap();
            let result = artifact(
                &root.join("result"),
                "coder.exec.result",
                json!({
                    "label": "coder.exec.result",
                    "tool": "coder.exec",
                    "correlation_id": "call-a",
                    "ok": true,
                    "body": {"exit_code": 0}
                }),
            );
            let result_ref = materialized_artifact_ref(&result).unwrap();
            let result_envelope = InterfaceInvocationEnvelope {
                artifact: result_ref.clone(),
                artifact_file: result,
                artifact_binding_payloads: vec![bound(
                    "result-before-ack",
                    json!({
                        "label": "tool.call",
                        "codex_continuity": pending,
                    }),
                )],
                metadata: Some(runtime_metadata()),
            };
            let stop_envelope = InterfaceInvocationEnvelope {
                artifact: stop_ref.clone(),
                artifact_file: stop,
                artifact_binding_payloads: Vec::new(),
                metadata: Some(runtime_metadata()),
            };
            let message_envelope = InterfaceInvocationEnvelope {
                artifact: message_ref.clone(),
                artifact_file: message,
                artifact_binding_payloads: Vec::new(),
                metadata: Some(runtime_metadata()),
            };
            let result_seq = if result_before_ack { 1 } else { 2 };
            let ack_seq = if result_before_ack { 2 } else { 1 };
            let mut movements = vec![
                movement(
                    0,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
                    Endpoint::Interface(descriptor.id.clone()),
                    stop_ref,
                ),
                movement(
                    result_seq,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_coder")),
                    Endpoint::Interface(descriptor.id.clone()),
                    result_ref.clone(),
                ),
                movement(
                    ack_seq,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    ack_ref,
                ),
                movement(
                    3,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
                    Endpoint::Interface(descriptor.id.clone()),
                    message_ref,
                ),
            ];
            movements.sort_by_key(|movement| movement.seq.0);
            let movements_path = root.join("ledger/movements.jsonl");
            write_movements(&movements_path, &movements);
            let ctx = context(root, &descriptor);
            let handoff = ProviderHandoffSession::new(&ctx).unwrap();
            let arbiter = InterruptedEffectArbiter::new(ctx.clone());
            hand_control_result(
                &ctx,
                &descriptor,
                &handoff,
                &arbiter,
                &run,
                ControlResultDelivery {
                    envelope: &stop_envelope,
                    source_movement_seq: 0,
                    output: None,
                    handoff_barrier: None,
                    effect_guard: None,
                    result: &mut ack_result,
                    recovery: true,
                    defer_message_receipt: false,
                },
            )
            .await
            .unwrap();
            let after_ack_restart = InterruptedEffectArbiter::new(ctx.clone());
            let recovered = after_ack_restart
                .durable_continuity(&run)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(recovered, interrupted);
            assert!(
                after_ack_restart
                    .begin_result(&run, result_seq, &result_envelope)
                    .await
                    .unwrap()
            );
            let resolving_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert_eq!(
                resolving_restart.durable_continuity(&run).await.unwrap(),
                Some(interrupted.clone())
            );
            let broker = CodexTurnControlBroker::new(None);
            broker
                .publish_recovered_interrupted_pending(&interrupted)
                .unwrap();
            let (driver, release, mut methods, server) =
                interrupted_drain_driver(root, &descriptor).await;
            driver
                .seed_interrupted_dynamic_tool_request_for_test(&interrupted)
                .await
                .unwrap();
            let (output, mut output_rx) = ProviderOutputStream::channel();
            let first_output_root = ctx.interface_var_dir().join("emitted");
            let live_stream_root = ctx.interface_var_dir().join("streams");
            let receive = driver.receive_artifact_and_stream_outputs(
                crate::handler::CodexProviderInvocation {
                    callable_tool_names: result_envelope.granted_tool_names().unwrap().into_iter().collect(),
                    input: result_envelope.clone(),
                    emit: output,
                    resolved_session_snapshot: None,
                    current_session_context: Arc::new(Vec::new()),
                    output_root: &first_output_root,
                    live_stream_root: &live_stream_root,
                    source_movement_seq: result_seq,
                    attempt: 1,
                    admitted_run_record: &pinned_run_record,
                    current_run_record: &pinned_run_record,
                    input_changes_context: true,
                    interrupted_after_artifact: None,
                    recovered_handoff_artifacts: Arc::new(Vec::new()),
                    durable_interrupted_continuity: Some(interrupted.clone()),
                    reconciled_result_continuity: None,
                    control: &broker,
                    handoff_barrier: ProviderHandoffBarrier::new(),
                },
            );
            let control_gates = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
            let race = async {
                tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    while !broker.is_interrupted_draining() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                let control = decode_admitted_turn_control(&ctx, &descriptor, &message_envelope)
                    .unwrap()
                    .expect("operator message is normally a steer candidate");
                let disposition = handle_control_source(
                    &ctx,
                    &descriptor,
                    &handoff,
                    &resolving_restart,
                    &run,
                    ControlSource {
                        envelope: &message_envelope,
                        source_movement_seq: 3,
                        broker: &broker,
                        control_gates: &control_gates,
                        control,
                        terminal_control_fenced: true,
                        independent_reader: false,
                    },
                )
                .await
                .unwrap();
                assert_eq!(disposition, ControlSourceDisposition::DeferredTurn);
                assert!(!control_journal_exists(&ctx, 3).unwrap());
                assert!(methods.try_recv().is_err());
                release.send(()).unwrap();
            };
            let (receive, ()) = tokio::join!(receive, race);
            receive.unwrap();
            server.await.unwrap();
            assert!(methods.try_recv().is_err());
            let ProviderStreamEmission::InterruptedResult {
                activity: None,
                stream: Some(stream),
                closure,
                ..
            } = output_rx.recv().await.unwrap()
            else {
                panic!("expected one atomic interrupted-result emission")
            };
            assert_eq!(
                stream.artifact.read_manifest().unwrap().label,
                "turn.stream"
            );
            let stream_rows = String::from_utf8(stream.artifact.read_payload_bytes().unwrap())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            let mut activity_payload = stream_rows
                .iter()
                .find(|row| row["type"] == "event" && row["label"] == "turn.dynamic_tool_call")
                .unwrap()["payload"]
                .clone();
            assert_eq!(activity_payload["returned_to_model"], false);
            activity_payload.as_object_mut().unwrap().insert(
                "codex_continuity".to_string(),
                serde_json::to_value(&interrupted).unwrap(),
            );
            let legacy_activity = octopus_interfaces::InterfaceArtifactFile::write(
                root.join("legacy-interrupted-activity"),
                "turn.dynamic_tool_call",
                &serde_json::to_vec(&activity_payload).unwrap(),
            )
            .unwrap();
            assert_eq!(
                closure.artifact.read_manifest().unwrap().label,
                "turn.interrupted.result"
            );
            let first_closure_bytes = closure.artifact.read_payload_bytes().unwrap();
            let closure_payload: serde_json::Value =
                serde_json::from_slice(&first_closure_bytes).unwrap();
            assert_eq!(closure_payload["returned_to_model"], false);
            assert!(closure_payload.get("cancelled_turn_transport").is_none());
            let closed = &closure_payload["codex_continuity"];
            assert_eq!(closed["context_generation"], 10);
            assert!(closed["pending"].is_null() && closed["interrupted_pending"].is_null());
            let mut context: Vec<octopus_provider_run_authoring::SessionContextArtifact> = vec![serde_json::from_value(session_entry(
                TURN_INTERRUPTED_RESULT_LABEL, closure_payload.clone(),
                materialized_artifact_ref(&closure.artifact).unwrap(), 5,
            )).unwrap()];
            let settled = serde_json::to_value(terminal_output_continuity(&descriptor, &message_envelope, &context, &run,
                br#"{"id":"run-a","interface_state":{"world_provider_codex":{"context_generation":10}}}"#,
            ).unwrap().unwrap()).unwrap();
            assert_eq!(settled["thread_id"], "thread-a");
            assert_eq!(settled["context_generation"], 11);
            context[0].payload["codex_continuity"]["context_generation"] = json!(11);
            assert!(terminal_output_continuity(&descriptor, &message_envelope, &context, &run,
                br#"{"id":"run-a","interface_state":{"world_provider_codex":{"context_generation":10}}}"#,
            ).unwrap_err().to_string().contains("terminal Codex continuity is ahead of its authored context"));
            assert!(output_rx.recv().await.is_none());
            let activity_ref = materialized_artifact_ref(&legacy_activity).unwrap();
            let closure_ref = materialized_artifact_ref(&closure.artifact).unwrap();
            let mut replay = QueuedTurn { envelope: result_envelope.clone(), seq: result_seq };
            let (recovered, recovered_ref) = recover_sealed_interrupted_result(
                &ctx, &descriptor, &replay, &interrupted,
            ).unwrap().unwrap();
            assert_eq!(recovered_ref, closure_ref);
            assert_eq!(recovered.len(), 2);
            assert_eq!(recovered[0].artifact.read_payload_bytes().unwrap(), stream.artifact.read_payload_bytes().unwrap());
            assert_eq!(recovered[1].artifact.read_payload_bytes().unwrap(), first_closure_bytes);
            replay.seq += 1;
            assert!(recover_sealed_interrupted_result(&ctx, &descriptor, &replay, &interrupted).is_err());
            let failed_context_session = ProviderHandoffSession::new(&ctx).unwrap();
            let replay_control = CodexTurnControlBroker::new(Some(result_seq));
            assert!(recover_interrupted_result_without_context(
                &ctx, &descriptor, &failed_context_session, &resolving_restart, &replay_control, &run, &replay,
            ).await.is_err());
            assert!(!recover_interrupted_result_without_context(
                &ctx, &descriptor, &failed_context_session, &resolving_restart, &replay_control, &run,
                &QueuedTurn { envelope: message_envelope.clone(), seq: result_seq + 1 },
            ).await.unwrap());
            replay.seq = result_seq;
            replay.envelope.artifact.content = ContentHash::of_bytes(b"another result");
            assert!(recover_sealed_interrupted_result(&ctx, &descriptor, &replay, &interrupted).is_err());
            resolving_restart
                .begin_closing(&run, result_seq, closure_ref.clone())
                .await
                .unwrap();
            let closing_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert_eq!(
                closing_restart.durable_continuity(&run).await.unwrap(),
                Some(interrupted.clone())
            );
            append_movement(
                &movements_path,
                &movement(
                    4,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    activity_ref.clone(),
                ),
            );
            assert_eq!(
                find_artifact_admission(&ctx, &run, result_seq, &activity_ref)
                    .unwrap()
                    .movement_seq,
                Some(4)
            );
            let recovery_broker = CodexTurnControlBroker::new(None);
            recovery_broker
                .publish_recovered_interrupted_pending(&interrupted)
                .unwrap();
            let (recovery_output, mut recovery_rx) = ProviderOutputStream::channel();
            driver
                .receive_artifact_and_stream_outputs(crate::handler::CodexProviderInvocation {
                    callable_tool_names: result_envelope.granted_tool_names().unwrap().into_iter().collect(),
                    input: result_envelope,
                    emit: recovery_output,
                    resolved_session_snapshot: None,
                    current_session_context: Arc::new(Vec::new()),
                    output_root: &first_output_root,
                    live_stream_root: &live_stream_root,
                    source_movement_seq: result_seq,
                    attempt: 2,
                    admitted_run_record: &pinned_run_record,
                    current_run_record: &pinned_run_record,
                    input_changes_context: true,
                    interrupted_after_artifact: Some("turn.dynamic_tool_call".to_string()),
                    recovered_handoff_artifacts: Arc::new(Vec::new()),
                    durable_interrupted_continuity: Some(interrupted.clone()),
                    reconciled_result_continuity: None,
                    control: &recovery_broker,
                    handoff_barrier: ProviderHandoffBarrier::new(),
                })
                .await
                .unwrap();
            let ProviderStreamEmission::InterruptedResult {
                activity: None,
                stream: Some(recovered_stream),
                closure: recovered_closure,
                ..
            } = recovery_rx.recv().await.unwrap()
            else {
                panic!("activity-only recovery must emit only the exact closure")
            };
            assert_eq!(
                recovered_closure.artifact.read_payload_bytes().unwrap(),
                first_closure_bytes
            );
            assert_eq!(
                recovered_stream.artifact.read_payload_bytes().unwrap(),
                stream.artifact.read_payload_bytes().unwrap()
            );
            assert!(recovery_rx.recv().await.is_none());
            let recovered_closure_ref =
                materialized_artifact_ref(&recovered_closure.artifact).unwrap();
            assert_eq!(recovered_closure_ref, closure_ref);
            closing_restart
                .begin_closing(&run, result_seq, recovered_closure_ref.clone())
                .await
                .unwrap();
            append_movement(
                &movements_path,
                &movement(
                    5,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    recovered_closure_ref.clone(),
                ),
            );
            let reconciled_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert!(
                reconciled_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_some()
            );
            reconciled_restart
                .recover_completed_source(
                    &run,
                    result_seq,
                    &[ProviderHandoffOutcome::Admitted {
                        seq: 5,
                        artifact: recovered_closure_ref.clone(),
                    }],
                )
                .await
                .unwrap();
            assert!(
                reconciled_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                find_artifact_admission(&ctx, &run, result_seq, &recovered_closure_ref)
                    .unwrap()
                    .movement_seq,
                Some(5)
            );
            assert!(
                InterruptedEffectArbiter::new(ctx)
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_none()
            );
            broker.finish();
            recovery_broker.finish();
        }
    }

    #[tokio::test]
    async fn result_and_ack_orders_reconcile_once_across_each_restart_boundary() {
        for result_before_ack in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let run = RunId::new("run-a");
            let descriptor = descriptor();
            let pending = continuity(7, Some("pending"));
            let interrupted = continuity(8, Some("interrupted_pending"));
            let closed = continuity(10, None);
            let stop = artifact(
                &root.join("stop"),
                "operator.turn.control",
                json!({"action": "stop", "control_id": "control-a"}),
            );
            let stop_ref = materialized_artifact_ref(&stop).unwrap();
            let result = artifact(
                &root.join("result"),
                "coder.exec.result",
                json!({
                    "label": "coder.exec.result",
                    "tool": "coder.exec",
                    "correlation_id": "call-a",
                    "ok": true,
                    "body": {"exit_code": 0}
                }),
            );
            let result_ref = materialized_artifact_ref(&result).unwrap();
            let mut ack_result = CodexTurnControlResult {
                control_id: "control-a".to_string(),
                action: "stop".to_string(),
                acknowledged: true,
                thread_id: Some("thread-a".to_string()),
                turn_id: Some("turn-a".to_string()),
                detail: json!({"interrupted": true, "exact_result_pending": true}),
                continuity: Some(interrupted.clone()),
            };
            let ack = materialize_turn_control_result(
                &root.join("var/provider/emitted"),
                0,
                ack_result.clone(),
            )
            .unwrap();
            let ack_ref = materialized_artifact_ref(&ack).unwrap();
            let result_seq = if result_before_ack { 1 } else { 2 };
            let ack_seq = if result_before_ack { 2 } else { 1 };
            let mut ordered = vec![movement(
                0,
                MovementKind::InterfaceOutbound,
                Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
                Endpoint::Interface(descriptor.id.clone()),
                stop_ref,
            )];
            ordered.push(movement(
                result_seq,
                MovementKind::InterfaceOutbound,
                Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_coder")),
                Endpoint::Interface(descriptor.id.clone()),
                result_ref.clone(),
            ));
            ordered.push(movement(
                ack_seq,
                MovementKind::InterfaceArrival,
                Endpoint::Interface(descriptor.id.clone()),
                Endpoint::Interface(InterfaceId::new("system_worker_test")),
                ack_ref.clone(),
            ));
            ordered.sort_by_key(|movement| movement.seq.0);
            let movements_path = root.join("ledger/movements.jsonl");
            write_movements(&movements_path, &ordered);
            let ctx = context(root, &descriptor);
            let arbiter = InterruptedEffectArbiter::new(ctx.clone());
            let stop_envelope = InterfaceInvocationEnvelope {
                artifact: materialized_artifact_ref(&stop).unwrap(),
                artifact_file: stop,
                artifact_binding_payloads: Vec::new(),
                metadata: None,
            };
            let handoff = ProviderHandoffSession::new(&ctx).unwrap();
            hand_control_result(
                &ctx,
                &descriptor,
                &handoff,
                &arbiter,
                &run,
                ControlResultDelivery {
                    envelope: &stop_envelope,
                    source_movement_seq: 0,
                    output: None,
                    handoff_barrier: None,
                    effect_guard: None,
                    result: &mut ack_result,
                    recovery: true,
                    defer_message_receipt: false,
                },
            )
            .await
            .unwrap();

            let after_ack_restart = InterruptedEffectArbiter::new(ctx.clone());
            let durable = after_ack_restart
                .durable_continuity(&run)
                .await
                .unwrap()
                .unwrap();
            let (reconciled, override_used) =
                crate::handler::reconcile_durable_interrupted_continuity(
                    Some(pending),
                    Some(durable.clone()),
                )
                .unwrap();
            assert!(override_used);
            assert_eq!(reconciled, Some(interrupted.clone()));
            let result_envelope = InterfaceInvocationEnvelope {
                artifact: result_ref.clone(),
                artifact_file: result,
                artifact_binding_payloads: Vec::new(),
                metadata: None,
            };
            assert!(
                after_ack_restart
                    .begin_result(&run, result_seq, &result_envelope)
                    .await
                    .unwrap()
            );
            let after_result_claim_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert!(
                after_result_claim_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_some()
            );
            let resolving_journal: InterruptedEffectJournal = serde_json::from_slice(
                &std::fs::read(interrupted_effect_journal_path(&ctx, &run)).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                resolving_journal.phase,
                InterruptedEffectPhase::Resolving {
                    result_source_movement_seq: current_source,
                    ref source_artifact,
                } if current_source == result_seq && source_artifact == &result_ref
            ));
            let closure = artifact(
                &root.join("closure"),
                "turn.interrupted.result",
                json!({
                    "label": "turn.interrupted.result",
                    "result_kind": "tool_result",
                    "correlation_id": "call-a",
                    "thread_id": "thread-a",
                    "turn_id": "turn-a",
                    "ok": true,
                    "turn_interrupted": true,
                    "returned_to_model": false,
                    "result_disposition": "durably_admitted_after_interrupt",
                    "source_artifact": result_ref.clone(),
                    "codex_continuity": closed
                }),
            );
            let closure_payload: serde_json::Value =
                serde_json::from_slice(&closure.read_payload_bytes().unwrap()).unwrap();
            assert_eq!(closure_payload["returned_to_model"], false);
            let closure_ref = interrupted_result_closure_ref(&closure, &durable, &result_ref, &codex_context_output_labels(&descriptor).unwrap())
                .unwrap()
                .unwrap();
            after_result_claim_restart
                .begin_closing(&run, result_seq, closure_ref.clone())
                .await
                .unwrap();

            let before_closure_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert!(
                before_closure_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_some()
            );
            let admitted_closure_ref = ArtifactRef {
                id: ArtifactId::new("turn.interrupted.result:post-handoff"),
                content: ContentHash::of_bytes(b"post-handoff-session-snapshot"),
            };
            append_movement(
                &movements_path,
                &movement(
                    3,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    admitted_closure_ref.clone(),
                ),
            );

            let after_closure_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert!(
                after_closure_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_some()
            );
            after_closure_restart
                .recover_completed_source(
                    &run,
                    result_seq,
                    &[ProviderHandoffOutcome::Admitted {
                        seq: 3,
                        artifact: admitted_closure_ref.clone(),
                    }],
                )
                .await
                .unwrap();
            assert!(
                after_closure_restart
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_none()
            );
            let journal: InterruptedEffectJournal = serde_json::from_slice(
                &std::fs::read(interrupted_effect_journal_path(&ctx, &run)).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                journal.phase,
                InterruptedEffectPhase::Reconciled {
                    result_source_movement_seq,
                    closure: DurableArtifactAdmission {
                        ref artifact,
                        movement_seq: 3,
                    }
                } if result_source_movement_seq == result_seq && artifact == &admitted_closure_ref
            ));
            assert_eq!(
                find_artifact_admission(&ctx, &run, result_seq, &admitted_closure_ref)
                    .unwrap()
                    .movement_seq,
                Some(3)
            );
            let settled_restart = InterruptedEffectArbiter::new(ctx.clone());
            assert!(
                !settled_restart
                    .begin_result(&run, result_seq + 1, &result_envelope)
                    .await
                    .unwrap()
            );
            assert!(
                settled_restart
                    .begin_result(&run, result_seq, &result_envelope)
                    .await
                    .unwrap()
            );
            assert!(
                InterruptedEffectArbiter::new(ctx)
                    .durable_continuity(&run)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn admission_scan_waits_for_a_complete_concurrent_jsonl_tail() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let run = RunId::new("run-a");
        let descriptor = descriptor();
        let ctx = context(root, &descriptor);
        let expected = ArtifactRef {
            id: ArtifactId::new("turn.control.acknowledged:exact"),
            content: ContentHash::of_bytes(b"exact-ack"),
        };
        let source = movement(
            0,
            MovementKind::InterfaceOutbound,
            Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
            Endpoint::Interface(descriptor.id.clone()),
            ArtifactRef {
                id: ArtifactId::new("operator.turn.control:source"),
                content: ContentHash::of_bytes(b"source"),
            },
        );
        let admitted = movement(
            1,
            MovementKind::InterfaceArrival,
            Endpoint::Interface(descriptor.id.clone()),
            Endpoint::Interface(InterfaceId::new("system_worker_test")),
            expected.clone(),
        );
        let trailing = movement(
            2,
            MovementKind::InterfaceOutbound,
            Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
            Endpoint::Interface(descriptor.id.clone()),
            ArtifactRef {
                id: ArtifactId::new("operator.message:partial"),
                content: ContentHash::of_bytes(b"partial"),
            },
        );
        let path = root.join("ledger/movements.jsonl");
        write_movements(&path, &[source.clone(), admitted.clone()]);
        let trailing_bytes = serde_json::to_vec(&trailing).unwrap();
        let split = trailing_bytes.len() / 2;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, &trailing_bytes[..split]).unwrap();
        std::io::Write::flush(&mut file).unwrap();
        let scan = find_artifact_admission(&ctx, &run, 0, &expected).unwrap();
        assert_eq!(scan.movement_seq, Some(1));
        assert!(!scan.complete_boundary);

        let admitted_bytes = serde_json::to_vec(&admitted).unwrap();
        let split = admitted_bytes.len() / 2;
        write_movements(&path, std::slice::from_ref(&source));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, &admitted_bytes[..split]).unwrap();
        std::io::Write::flush(&mut file).unwrap();
        drop(file);
        let wait = await_artifact_admission_scan(&ctx, &run, 0, &expected);
        let complete = async {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            std::io::Write::write_all(&mut file, &admitted_bytes[split..]).unwrap();
            std::io::Write::write_all(&mut file, b"\n").unwrap();
            file.sync_all().unwrap();
            drop(file);
            sync_ledger_index(&path);
        };
        let (scan, ()) = tokio::join!(wait, complete);
        let scan = scan.unwrap();
        assert_eq!(scan.movement_seq, Some(1));
        assert!(scan.complete_boundary);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, b"{malformed}\n").unwrap();
        file.sync_all().unwrap();
        let error = find_artifact_admission(&ctx, &run, 0, &expected).unwrap_err();
        assert!(error.contains("malformed row 2"), "{error}");

        let mut failed = serde_json::to_value(&admitted).unwrap();
        failed["invocation_metadata"]["verification_failure"] = json!({"error":"no_requirement_for_label"});
        let failed: Movement = serde_json::from_value(failed).unwrap();
        let mut later = admitted.clone(); later.seq.0 = 2; later.lamport.0 = 3;
        write_movements(&path, &[source.clone(), failed.clone(), later.clone()]);
        assert_eq!(find_artifact_admission(&ctx, &run, 0, &expected).unwrap().movement_seq, Some(2));
        assert!(validate_artifact_admission(&ctx, &run, 0, &DurableArtifactAdmission {
            artifact: expected.clone(), movement_seq: 1,
        }).is_err());
        write_movements(&path, &[source.clone(), failed]);
        assert_eq!(find_artifact_admission(&ctx, &run, 0, &expected).unwrap().movement_seq, None);
        write_movements(&path, &[source, admitted, later]);
        assert!(find_artifact_admission(&ctx, &run, 0, &expected).unwrap_err().contains("duplicate provider admissions"));
    }

    #[tokio::test]
    async fn terminal_carrierless_release_survives_later_carrier_before_journal_delete() {
        for with_settled_continuity in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            let descriptor = descriptor();
            let ctx = context(root, &descriptor);
            let run = RunId::new("run-a");
            let file_bytes = [0xff, 0, 0x81, b'A'].repeat(32 * 1024);
            let file = InterfaceArtifactFile::write(root.join("binary"), "operator.file.binary", &file_bytes).unwrap();
            let file_ref = cache_artifact(&ctx, &file);
            let attachment = json!({"name":"résumé.zip","label":"operator.file.binary","mime_type":"application/zip",
                "size":file_bytes.len(),"payload_path":file.read_manifest().unwrap().payload_path,
                "payload_content_hash":format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&file_bytes)),"last_modified":0});
            let message = artifact(
                &root.join("message"),
                "operator.message",
                json!({"text": "next task", "message_id": "message-a", "attachments":[attachment]}),
            );
            let message_ref = cache_artifact(&ctx, &message);
            let mut artifact_binding_payloads = with_settled_continuity
                .then(|| {
                    bound(
                        "settled-context",
                        json!({
                            "label": "interface.artifact.output",
                            "codex_continuity": continuity(1, None),
                        }),
                    )
                })
                .into_iter()
                .collect::<Vec<_>>();
            for bound in &artifact_binding_payloads {
                std::fs::write(ctx.blobs_dir.join(bound.artifact.content.to_hex()), bound.read_bytes().unwrap().as_ref()).unwrap();
            }
            artifact_binding_payloads.push(octopus_interfaces::ArtifactBindingPayload::new(file_ref.clone(), file_bytes.to_vec()));
            let mut pinned: serde_json::Value = serde_json::from_slice(&pinned_test_run_record()).unwrap();
            let checkpoint = &mut pinned["checkpoints"][0];
            checkpoint["artifact_bindings"] = json!(artifact_binding_payloads.iter().map(|bound| json!({"id":bound.artifact.id})).collect::<Vec<_>>());
            let input_binding = checkpoint["interface_binding"].clone();
            let outputs = checkpoint["outbound_interface_bindings"].clone();
            pinned["interface_state"] = json!({"world_provider_codex":{"run_authoring_binding":{"schema_version":2,
                "run":"run-a","worker":"system_worker_test","provider_interface":"world_provider_codex",
                "authoring_source_interface":"world_provider_codex","payload_artifact_bindings":[file_ref],
                "checkpoints":[{"name":"input","role":"stable","include_context":true,"interface_binding":input_binding,
                    "outbound_interface_bindings":outputs,"required_verified_artifacts":[]}]}}});
            let pinned = serde_json::to_vec(&pinned).unwrap();
            octopus_runtime::run_record::RunRecord::from_bytes(&pinned).unwrap();
            let pin = ContentHash::of_bytes(&pinned);
            std::fs::write(ctx.blobs_dir.join(pin.to_hex()), &pinned).unwrap();
            let mut metadata = runtime_metadata();
            metadata.runtime.as_mut().unwrap().run_record_hash = Some(pin.to_hex());
            metadata.grant.artifact_access.insert(file_ref.id.to_string(), Default::default());
            metadata.artifact_binding_refs = artifact_binding_payloads
                .iter()
                .map(|bound| bound.artifact.clone())
                .collect();
            let turn = QueuedTurn {
                envelope: InterfaceInvocationEnvelope {
                    artifact: message_ref.clone(),
                    artifact_file: message,
                    artifact_binding_payloads,
                    metadata: Some(metadata.clone()),
                },
                seq: 0,
            };
            let decode = |envelope: &InterfaceInvocationEnvelope| {
                let Some(CodexTurnControl::Steer { control_id, input }) = decode_admitted_turn_control(&ctx, &descriptor, envelope).unwrap()
                    else { panic!("exact selected operator source must remain steering") };
                assert_eq!(control_id, "message-a");
                let octopus_appserver_proto::UserInput::Text { text, .. } = &input[0] else { panic!("missing selected-file metadata") };
                serde_json::from_str::<serde_json::Value>(text).unwrap()
            };
            let visible = decode(&turn.envelope);
            let chosen: ArtifactRef = serde_json::from_value(visible["selected_files"][0]["artifact_ref"].clone()).unwrap();
            assert_eq!(chosen, file_ref);
            assert_eq!(visible["selected_files"][0]["filename"], "résumé.zip");
            let mut revoked = turn.envelope.clone();
            revoked.metadata.as_mut().unwrap().grant.artifact_access.clear();
            assert!(decode_admitted_turn_control(&ctx, &descriptor, &revoked).is_err());
            assert!(decode_inflight_turn_control(&descriptor, &turn.envelope, Some(&pinned_test_run_record())).is_err());
            let mut cleared = turn.envelope.clone();
            cleared.metadata.as_mut().unwrap().runtime.as_mut().unwrap().run_record_hash =
                Some(ContentHash::of_bytes(&pinned_test_run_record()).to_hex());
            assert!(decode_admitted_turn_control(&ctx, &descriptor, &cleared).is_err());
            let mut source = movement(
                0,
                MovementKind::InterfaceOutbound,
                Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
                Endpoint::Interface(descriptor.id.clone()),
                message_ref.clone(),
            );
            source.invocation_metadata = Some(metadata.into());
            write_movements(&ctx.movements_path, &[source]);
            persist_control_journal(
                &ctx,
                0,
                &run,
                "message-a",
                "steer",
                TurnControlJournalPhase::DeferredTurn {
                    result: CodexTurnControlResult {
                        control_id: "message-a".to_string(),
                        action: "steer".to_string(),
                        acknowledged: false,
                        thread_id: None,
                        turn_id: None,
                        detail: json!({"error": "no active provider turn accepts this control artifact"}),
                        continuity: None,
                    },
                    cause: CodexDeferredTurnCause::TerminalTurnMoved,
                },
            )
            .unwrap();
            persist_deferred_provider_turn(&ctx, &run, &turn).unwrap();
            let deferred = DeferredProviderTurn {
                run: run.clone(),
                turn: turn.clone(),
                release: None,
            };
            let DeferredTurnReadiness::Ready(released) =
                prepare_deferred_provider_turn(&ctx, &descriptor, &run, &deferred, &ActiveTurnControls::default()).await.unwrap()
            else {
                panic!("terminal carrierless ownership must release the original envelope")
            };
            assert_eq!(released.envelope.artifact, message_ref);
            upgrade_deferred_fixture(&ctx, &descriptor, &run, &turn).await;
            assert_eq!(
                released.envelope.artifact_binding_payloads.len(),
                usize::from(with_settled_continuity) + 1
            );
            let completed = artifact(
                &root.join("completed-session"),
                "interface.artifact.output",
                json!({
                    "label": "interface.artifact.output",
                    "codex_continuity": continuity(2, None),
                    "session_context": {
                        "schema_version": 3,
                        "provider_interface": "world_provider_codex",
                        "artifacts": []
                    }
                }),
            );
            let completed_ref = cache_artifact(&ctx, &completed);
            append_movement(
                &ctx.movements_path,
                &movement(
                    1,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    completed_ref.clone(),
                ),
            );
            let labels = codex_session_snapshot_labels(&descriptor).unwrap();
            let suffix = latest_provider_session_carrier(&ctx, &run, 0, &labels, None).unwrap();
            assert_eq!(
                suffix.latest.as_ref().map(|carrier| &carrier.artifact),
                Some(&completed_ref)
            );

            // Simulate restart after source completion but before journal deletion.
            let loaded = load_deferred_provider_turns(&ctx, &descriptor).unwrap();
            let recovered = loaded.get(&0).unwrap();
            assert_eq!(decode(&recovered.turn.envelope), visible);
            assert_eq!(ctx.pinned_run_record_bytes(&recovered.turn.envelope).unwrap(), pinned);
            let journal_path = deferred_provider_turn_path(&ctx, 0);
            let preserved = std::fs::read(&journal_path).unwrap();
            let actual_manifest = std::fs::read(ctx.blobs_dir.join(chosen.content.to_hex())).unwrap();
            for replacement in [vec![0xfe, 0, 0x81, b'A'], actual_manifest] {
                let mut changed: DeferredProviderTurnJournal = serde_json::from_slice(&preserved).unwrap();
                let bound = changed.artifact_bindings.iter_mut().find(|bound| bound.artifact == chosen).unwrap();
                bound.byte_count = replacement.len() as u64;
                bound.bytes_content = ContentHash::of_bytes(&replacement);
                std::fs::write(&journal_path, serde_json::to_vec(&changed).unwrap()).unwrap();
                assert!(load_deferred_provider_turns(&ctx, &descriptor).err().unwrap().contains("admitted manifest payload pin"));
            }
            std::fs::write(&journal_path, preserved).unwrap();
            assert!(matches!(
                recovered.release,
                Some(DeferredProviderTurnRelease::OriginalEnvelope {
                    observed_ledger_next_seq: 1,
                    ..
                })
            ));
            assert!(matches!(
                prepare_deferred_provider_turn(&ctx, &descriptor, &run, recovered, &ActiveTurnControls::default()).await.unwrap(),
                DeferredTurnReadiness::Ready(_)
            ));
            remove_deferred_provider_turn(&ctx, 0).unwrap();
            assert!(load_deferred_provider_turns(&ctx, &descriptor).unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn completed_turn_race_rebases_the_message_as_the_next_turn() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let descriptor = descriptor();
        let ctx = context(root, &descriptor);
        let run = RunId::new("run-a");
        let message = artifact(
            &root.join("message"),
            "operator.message",
            json!({"prompt": "next task", "message_id": "message-a"}),
        );
        let message_ref = cache_artifact(&ctx, &message);
        let turn = QueuedTurn {
            envelope: InterfaceInvocationEnvelope {
                artifact: message_ref.clone(),
                artifact_file: message,
                artifact_binding_payloads: Vec::new(),
                metadata: Some(runtime_metadata()),
            },
            seq: 0,
        };
        let completed = artifact(
            &root.join("completed-session"),
            "interface.artifact.output",
            json!({
                "label": "interface.artifact.output",
                "codex_continuity": continuity(1, None),
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_codex",
                    "artifacts": [],
                }
            }),
        );
        let completed_ref = cache_artifact(&ctx, &completed);
        write_movements(
            &ctx.movements_path,
            &[
                movement(
                    0,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
                    Endpoint::Interface(descriptor.id.clone()),
                    message_ref.clone(),
                ),
                movement(
                    1,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    completed_ref.clone(),
                ),
            ],
        );

        assert!(operator_turn_requires_deferral(&ctx, &descriptor, &run, &turn, &[]).unwrap());
        persist_deferred_provider_turn(&ctx, &run, &turn).unwrap();
        upgrade_deferred_fixture(&ctx, &descriptor, &run, &turn).await;
        let deferred = DeferredProviderTurn {
            run: run.clone(),
            turn,
            release: None,
        };
        let DeferredTurnReadiness::Ready(rebased) =
            prepare_deferred_provider_turn(&ctx, &descriptor, &run, &deferred, &ActiveTurnControls::default()).await.unwrap()
        else {
            panic!("the completed-turn carrier must release the next ordinary turn")
        };
        assert_eq!(rebased.seq, 0);
        assert_eq!(rebased.envelope.artifact, message_ref);
        assert_eq!(rebased.envelope.artifact_binding_payloads.len(), 1);
        assert_eq!(
            rebased.envelope.artifact_binding_payloads[0].artifact,
            completed_ref
        );
        let journal: DeferredProviderTurnJournal =
            serde_json::from_slice(&std::fs::read(deferred_provider_turn_path(&ctx, 0)).unwrap())
                .unwrap();
        assert!(journal.release.as_ref().is_some_and(|release| matches!(
            release,
            DeferredProviderTurnRelease::SessionRebase {
                source_session_carrier: None,
                ..
            }
        )));
        upgrade_deferred_fixture(&ctx, &descriptor, &run, &deferred.turn).await;
    }

    #[tokio::test]
    async fn pending_result_releases_the_same_deferred_operator_message_once() {
        for (label, payload) in [
            ("operator.message", json!({"prompt": "next task", "message_id": "message-a"})),
            ("client.surface.document", json!({"schema": "octopus.client_surface", "id": "widget-a", "revision": 1})),
        ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let descriptor = descriptor();
        let ctx = context(root, &descriptor);
        let run = RunId::new("run-a");
        let controls = ActiveTurnControls::default();
        let pending = continuity(7, Some("pending"));
        let original_content = json!({
            "label": "interface.artifact.output",
            "codex_continuity": pending,
        });
        let original = artifact(
            &root.join("original-session"),
            "interface.artifact.output",
            json!({
                "label": "interface.artifact.output",
                "codex_continuity": continuity(7, Some("pending")),
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_codex",
                    "artifacts": [],
                }
            }),
        );
        let original_ref = cache_artifact(&ctx, &original);
        let result_payload = json!({
            "label": "coder.exec.result",
            "tool": "coder.exec",
            "correlation_id": "call-a",
            "ok": true,
            "body": {"exit_code": 0},
        });
        let result = artifact(
            &root.join("result"),
            "coder.exec.result",
            result_payload.clone(),
        );
        let result_ref = cache_artifact(&ctx, &result);
        let settled = artifact(
            &root.join("settled-session"),
            "interface.artifact.output",
            json!({
                "label": "interface.artifact.output",
                "codex_continuity": continuity(8, Some("pending")),
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_codex",
                    "parent": original_ref.clone(),
                    "artifacts": [
                        session_entry(
                            "interface.artifact.output",
                            original_content,
                            original_ref.clone(),
                            0,
                        ),
                        session_entry(
                            "coder.exec.result",
                            result_payload,
                            result_ref.clone(),
                            1,
                        ),
                    ],
                }
            }),
        );
        let settled_ref = cache_artifact(&ctx, &settled);
        let message = artifact(
            &root.join("message"),
            label,
            payload.clone(),
        );
        let message_ref = cache_artifact(&ctx, &message);
        let mut metadata = runtime_metadata();
        metadata.artifact_binding_refs = vec![original_ref.clone(), result_ref.clone()];
        let turn = QueuedTurn {
            envelope: InterfaceInvocationEnvelope {
                artifact: message_ref.clone(),
                artifact_file: message,
                artifact_binding_payloads: vec![
                    octopus_interfaces::ArtifactBindingPayload::new(
                        original_ref.clone(),
                        original.read_payload_bytes().unwrap(),
                    ),
                    octopus_interfaces::ArtifactBindingPayload::new(
                        result_ref.clone(),
                        result.read_payload_bytes().unwrap(),
                    ),
                ],
                metadata: Some(metadata.clone()),
            },
            seq: 2,
        };
        let mut source = movement(
            2,
            MovementKind::InterfaceOutbound,
            Endpoint::Interface(octopus_descriptors::InterfaceId::new("world_operator")),
            Endpoint::Interface(descriptor.id.clone()),
            message_ref.clone(),
        );
        source.invocation_metadata = Some(metadata.into());
        write_movements(
            &ctx.movements_path,
            &[
                movement(
                    0,
                    MovementKind::InterfaceArrival,
                    Endpoint::Interface(descriptor.id.clone()),
                    Endpoint::Interface(InterfaceId::new("system_worker_test")),
                    original_ref.clone(),
                ),
                movement(
                    1,
                    MovementKind::InterfaceOutbound,
                    Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_coder")),
                    Endpoint::Interface(descriptor.id.clone()),
                    result_ref,
                ),
                source,
            ],
        );

        assert!(is_provider_turn_input(&descriptor, &turn.envelope).unwrap());
        assert!(operator_turn_requires_deferral(&ctx, &descriptor, &run, &turn, &[]).unwrap());
        persist_deferred_provider_turn(&ctx, &run, &turn).unwrap();
        persist_deferred_provider_turn(&ctx, &run, &turn).unwrap();
        let deferred = DeferredProviderTurn {
            run: run.clone(),
            turn: turn.clone(),
            release: None,
        };
        assert!(matches!(
            prepare_deferred_provider_turn(&ctx, &descriptor, &run, &deferred, &controls).await.unwrap(),
            DeferredTurnReadiness::AwaitingSessionSettlement
        ));

        append_movement(
            &ctx.movements_path,
            &movement(
                3,
                MovementKind::InterfaceArrival,
                Endpoint::Interface(descriptor.id.clone()),
                Endpoint::Interface(InterfaceId::new("system_worker_test")),
                settled_ref.clone(),
            ),
        );
        let broker = CodexTurnControlBroker::new(Some(1));
        controls.lock().await.insert(run.to_string(), broker.clone());
        for interrupted in [false, true] {
            if interrupted { broker.publish_recovered_interrupted_pending(&continuity(8, Some("interrupted_pending"))).unwrap(); }
            assert!(matches!(prepare_deferred_provider_turn(&ctx, &descriptor, &run, &deferred, &controls).await.unwrap(),
                DeferredTurnReadiness::AwaitingSessionSettlement));
            assert!(load_deferred_provider_turns(&ctx, &descriptor).unwrap().get(&2).unwrap().release.is_none());
        }
        broker.finish();
        let DeferredTurnReadiness::Ready(rebased) =
            prepare_deferred_provider_turn(&ctx, &descriptor, &run, &deferred, &controls).await.unwrap()
        else {
            panic!("the exact pending result must release the queued message")
        };
        assert_eq!(rebased.seq, 2);
        assert_eq!(rebased.envelope.artifact_file.read_manifest().unwrap().label, label);
        assert_eq!(rebased.envelope.artifact_file.read_payload_bytes().unwrap(), turn.envelope.artifact_file.read_payload_bytes().unwrap());
        assert_eq!(rebased.envelope.artifact, message_ref);
        assert_eq!(rebased.envelope.artifact_binding_payloads.len(), 1);
        assert_eq!(
            rebased.envelope.artifact_binding_payloads[0].artifact,
            settled_ref
        );

        persist_deferred_provider_turn(&ctx, &run, &turn).unwrap();
        let loaded = load_deferred_provider_turns(&ctx, &descriptor).unwrap();
        assert_eq!(loaded.len(), 1);
        let recovered = loaded.get(&2).unwrap();
        assert!(recovered.release.is_some());
        assert!(matches!(
            prepare_deferred_provider_turn(&ctx, &descriptor, &run, recovered, &controls).await.unwrap(),
            DeferredTurnReadiness::Ready(_)
        ));
        remove_deferred_provider_turn(&ctx, 2).unwrap();
        assert!(load_deferred_provider_turns(&ctx, &descriptor).unwrap().is_empty());

        let ctx = context(&root.join("interrupted"), &descriptor);
        let interrupted = continuity(7, Some("interrupted_pending"));
        let closed = continuity(8, None);
        let partial = artifact(&root.join("interrupted-session"), "interface.artifact.output", json!({
            "label":"interface.artifact.output", "codex_continuity":interrupted,
            "session_context":{"schema_version":3,"provider_interface":"world_provider_codex","artifacts":[]},
        }));
        let partial_ref = cache_artifact(&ctx, &partial);
        let result_ref = cache_artifact(&ctx, &result);
        let closure_file = artifact(&root.join("interrupted-closure"), TURN_INTERRUPTED_RESULT_LABEL, json!({
            "label":TURN_INTERRUPTED_RESULT_LABEL,"codex_continuity":closed,
            "result_kind":"tool_result","correlation_id":"call-a","thread_id":"thread-a","turn_id":"turn-a",
            "ok":true,"turn_interrupted":true,"returned_to_model":false,
            "result_disposition":"durably_admitted_after_interrupt","source_artifact":result_ref,
        }));
        let closure = DurableArtifactAdmission { artifact:cache_artifact(&ctx, &closure_file), movement_seq:3 };
        let result_source = DurableArtifactAdmission { artifact:result_ref, movement_seq:2 };
        std::fs::remove_file(result.read_manifest().unwrap().payload_path).unwrap();
        std::fs::remove_file(closure_file.read_manifest().unwrap().payload_path).unwrap();
        assert!(result.read_payload_bytes().is_err());
        assert!(closure_file.read_payload_bytes().is_err());
        assert!(read_pinned_provider_artifact_payload(&ctx, &result_source.artifact, "interrupted result").is_ok());
        assert!(read_pinned_provider_artifact_payload(&ctx, &closure.artifact, "interrupted closure").is_ok());
        let mut original = turn.clone();
        original.envelope.artifact_binding_payloads = vec![octopus_interfaces::ArtifactBindingPayload::new(
            partial_ref.clone(), partial.read_payload_bytes().unwrap())];
        original.envelope.metadata.as_mut().unwrap().artifact_binding_refs = vec![partial_ref.clone()];
        let mut source = movement(1, MovementKind::InterfaceOutbound,
            Endpoint::Interface(InterfaceId::new("world_operator")), Endpoint::Interface(descriptor.id.clone()), message_ref);
        source.invocation_metadata = Some(original.envelope.metadata.clone().unwrap().into());
        let mut later_source = source.clone();
        later_source.seq = Seq(4);
        later_source.lamport = Lamport(5);
        write_movements(&ctx.movements_path, &[
            movement(0, MovementKind::InterfaceArrival, Endpoint::Interface(descriptor.id.clone()),
                Endpoint::Interface(InterfaceId::new("system_worker_test")), partial_ref.clone()), source,
            movement(2, MovementKind::InterfaceOutbound, Endpoint::Interface(InterfaceId::new("system_coder")),
                Endpoint::Interface(descriptor.id.clone()), result_source.artifact.clone()),
            movement(3, MovementKind::InterfaceArrival, Endpoint::Interface(descriptor.id.clone()),
                Endpoint::Interface(InterfaceId::new("system_worker_test")), closure.artifact.clone()), later_source,
        ]);
        let mut current = vec![octopus_provider_run_authoring::SessionContextArtifact {
            label:"interface.artifact.output".into(), payload:json!({"label":"interface.artifact.output","codex_continuity":interrupted}),
            source_artifact:partial_ref, source_movement_seq:Some(0),
        }];
        for seq in [1, 4] {
            original.seq = seq;
            let pinned = ctx.pinned_run_record_bytes(&original.envelope).unwrap();
            let snapshot = bound_operator_session_snapshot(&ctx, &descriptor, &original).unwrap();
            assert_eq!(recover_provider_turn_continuity(&descriptor, &original.envelope,
                snapshot.as_ref().map(|(_, snapshot)| snapshot), &run, &pinned).unwrap(), Some(interrupted.clone()));
            let private = checked_interrupted_result_continuity(&ctx, &descriptor, &run, &original.envelope,
                &interrupted, &result_source, &closure, &current).await.unwrap();
            assert_eq!(private, closed);
            assert_eq!(original.envelope.artifact_binding_payloads.len(), 1);
            assert_eq!(current.len(), 1);
        }
        let later = continuity(10, None);
        current.push(octopus_provider_run_authoring::SessionContextArtifact {
            label:"turn.agent_message".into(), payload:json!({"label":"turn.agent_message","codex_continuity":later}),
            source_artifact:ArtifactRef { id:ArtifactId::new("later-answer"), content:ContentHash::of_bytes(b"later") },
            source_movement_seq:Some(5),
        });
        assert_eq!(checked_interrupted_result_continuity(&ctx, &descriptor, &run, &original.envelope,
            &interrupted, &result_source, &closure, &current).await.unwrap(), later);
        assert!(reconcile_delivered_interrupted_continuity(Some(&interrupted), Some(continuity(10, Some("pending"))), Some(closed.clone())).is_err());
        for key in ["provider_interface", "run", "thread_id", "thread_shape_fingerprint", "static_context_fingerprint"] {
            let mut wrong = serde_json::to_value(&later).unwrap();
            wrong[key] = json!("different");
            assert!(reconcile_delivered_interrupted_continuity(Some(&interrupted),
                Some(serde_json::from_value(wrong).unwrap()), Some(closed.clone())).is_err());
        }
        let mut wrong = result_source.clone();
        wrong.artifact.content = ContentHash::of_bytes(b"different result");
        assert!(checked_interrupted_result_continuity(&ctx, &descriptor, &run, &original.envelope,
            &interrupted, &wrong, &closure, &current).await.is_err());
        let mut wrong = closure.clone();
        wrong.movement_seq = 4;
        assert!(checked_interrupted_result_continuity(&ctx, &descriptor, &run, &original.envelope,
            &interrupted, &result_source, &wrong, &current).await.is_err());
        }
    }

    #[tokio::test]
    async fn independent_message_receipt_waits_for_main_reader_after_output_closes() {
        let temp = tempfile::tempdir().unwrap();
        let descriptor = descriptor();
        let run = RunId::new("run-a");
        let file = artifact(&temp.path().join("message"), "provider.peer.message",
            json!({"action":"queue", "control_id":"queue-a", "text":"hello"}));
        let envelope = InterfaceInvocationEnvelope {
            artifact: materialized_artifact_ref(&file).unwrap(), artifact_file: file,
            artifact_binding_payloads: Vec::new(), metadata: Some(runtime_metadata()),
        };
        write_movements(&temp.path().join("ledger/movements.jsonl"), &[movement(
            0, MovementKind::InterfaceOutbound,
            Endpoint::Interface(octopus_descriptors::InterfaceId::new("system_peer")),
            Endpoint::Interface(descriptor.id.clone()), envelope.artifact.clone(),
        )]);
        let ctx = context(temp.path(), &descriptor);
        let handoff = ProviderHandoffSession::new(&ctx).unwrap();
        let arbiter = InterruptedEffectArbiter::new(ctx.clone());
        let broker = CodexTurnControlBroker::new(Some(0));
        broker.finish();
        let gates = ControlSourceGates::default();
        persist_control_journal(&ctx, 0, &run, "queue-a", "queue_message",
            TurnControlJournalPhase::Result { result: CodexTurnControlResult {
                control_id: "queue-a".into(), action: "queue_message".into(),
                acknowledged: true, thread_id: Some("thread-a".into()), turn_id: None,
                detail: json!({"queued":true}), continuity: None,
            }}).unwrap();
        let before = std::fs::read(&ctx.movements_path).unwrap();
        for _ in 0..2 {
            assert_eq!(handle_control_source(&ctx, &descriptor, &handoff, &arbiter, &run,
                ControlSource { envelope: &envelope, source_movement_seq: 0,
                    broker: &broker, control_gates: &gates,
                    control: CodexTurnControl::Inject { control_id: "queue-a".into(), items: vec![] },
                    terminal_control_fenced: true, independent_reader: true,
                }).await.unwrap(), ControlSourceDisposition::Handled);
            assert!(matches!(load_control_journal(&ctx, 0, &run, "queue-a", "queue_message")
                .unwrap().unwrap().phase, TurnControlJournalPhase::Result { .. }));
            assert_eq!(std::fs::read(&ctx.movements_path).unwrap(), before);
        }
    }

    #[test]
    fn terminal_broker_leaves_the_next_operator_message_for_the_main_reader() {
        let broker = CodexTurnControlBroker::new(Some(9));
        broker.finish();

        assert!(matches!(control_reader_ownership(Some(&broker), 10, 11, "operator.message"), ControlReaderOwnership::Main));
        assert!(matches!(control_reader_ownership(Some(&broker), 10, 12, "operator.turn.control"), ControlReaderOwnership::Independent));
    }
}
