use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::sync::Arc;

use octopus_descriptors::RunId;
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{
    ArtifactBindingPayload, InterfaceArtifactManifest, InterfaceDescriptor,
    InterfaceInvocationEnvelope,
};
use octopus_ledger::{ArtifactRef, ContentHash, Endpoint, MovementKind};
use octopus_ledger_store::DurableMovementDecoder;
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession, ProviderOutputHandoff,
    ProviderSourceRecovery, RunAuthoringResultIdentity,
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;

use crate::driver::{
    materialize_turn_error, GeminiFailure, GeminiFanInEnvelope, GeminiFanInPlan, GeminiFanInSource,
    GeminiNativeDriver,
};

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(provider_life(ctx)) as InterfaceLifeFuture)
}

struct QueuedTurn {
    envelope: InterfaceInvocationEnvelope,
    seq: u64,
    fan_in_sources: Vec<GeminiFanInSource>,
}

async fn provider_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let activation_secrets_home = ctx.root_slot("activation_secrets_home")?.to_path_buf();
    let driver = GeminiNativeDriver::from_descriptor(&descriptor, activation_secrets_home)?;
    let authoring_result = RunAuthoringResultIdentity::from_descriptor(&descriptor)?;
    let handoff_session = ProviderHandoffSession::new(&ctx)?;
    let mut reader = LedgerTailReader::open(&ctx)?;
    ctx.signal_ready()?;
    'life: loop {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        let batch = reader.read_new()?;
        if batch.is_empty() {
            reader.wait_for_change(&ctx.cancel).await?;
            continue;
        }
        let mut index = 0;
        while index < batch.len() {
            let own = &batch[index];
            let envelope = match reader.envelope(own) {
                Ok(envelope) => envelope,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "poisoned_outbound",
                        serde_json::json!({ "error": error }),
                    )
                    .await?;
                    reader.commit_through(own)?;
                    index += 1;
                    continue;
                }
            };
            let manifest = match envelope.artifact_file.read_manifest() {
                Ok(manifest) => manifest,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "malformed_artifact_manifest",
                        serde_json::json!({ "error": error.to_string() }),
                    )
                    .await?;
                    reader.commit_through(own)?;
                    index += 1;
                    continue;
                }
            };
            let metadata = match own.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "malformed_runtime_envelope",
                        serde_json::json!({ "error": error }),
                    )
                    .await?;
                    reader.commit_through(own)?;
                    index += 1;
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
                reader.commit_through(own)?;
                index += 1;
                continue;
            };
            if authoring_result
                .consume_if_matches(&ctx, &own.movement, &envelope, &manifest.label)
                .await?
            {
                reader.commit_through(own)?;
                index += 1;
                continue;
            }
            let mut envelopes = Vec::with_capacity(batch.len() - index);
            envelopes.push(GeminiFanInEnvelope {
                envelope,
                source_movement_seq: Some(own.movement.seq.0),
            });
            let mut later_envelope_error = None;
            for later in batch.iter().skip(index + 1) {
                match reader.envelope(later) {
                    Ok(envelope) => envelopes.push(GeminiFanInEnvelope {
                        envelope,
                        source_movement_seq: Some(later.movement.seq.0),
                    }),
                    Err(error) => {
                        later_envelope_error = Some(error);
                        break;
                    }
                }
            }
            match driver.prepare_fan_in(&envelopes) {
                Ok(GeminiFanInPlan::Ready {
                    envelope,
                    consumed,
                    source_movements,
                }) => {
                    let consumed_index = index
                        .checked_add(consumed)
                        .ok_or_else(|| "Gemini fan-in consumed index overflowed".to_string())?;
                    if consumed == 0 || consumed_index > batch.len() {
                        return Err(format!(
                            "Gemini fan-in consumed {consumed} movements from batch position {index} of {}",
                            batch.len()
                        ));
                    }
                    let terminal = &batch[consumed_index - 1];
                    let run = envelope
                        .runtime()
                        .map_err(|error| error.to_string())?
                        .run
                        .clone();
                    drive_with_retries(
                        &ctx,
                        &descriptor,
                        &driver,
                        &handoff_session,
                        &run,
                        QueuedTurn {
                            envelope,
                            seq: terminal.movement.seq.0,
                            fan_in_sources: source_movements,
                        },
                    )
                    .await?;
                    reader.commit_through(terminal)?;
                    index = consumed_index;
                }
                Ok(GeminiFanInPlan::Pending {
                    envelope,
                    accumulated,
                    content_group,
                    expected,
                    received,
                    source_movements,
                }) => {
                    let consumed_index = index
                        .checked_add(accumulated)
                        .ok_or_else(|| "Gemini pending fan-in index overflowed".to_string())?;
                    if accumulated == 0 || consumed_index > batch.len() {
                        return Err(format!(
                            "Gemini pending fan-in accumulated {accumulated} movements from batch position {index} of {}",
                            batch.len()
                        ));
                    }
                    let terminal = &batch[consumed_index - 1];
                    let run = envelope
                        .runtime()
                        .map_err(|error| error.to_string())?
                        .run
                        .clone();
                    if let Err(error) = validate_turn_context(
                        &ctx,
                        &descriptor,
                        &handoff_session,
                        &run,
                        &QueuedTurn {
                            envelope: envelope.clone(),
                            seq: terminal.movement.seq.0,
                            fan_in_sources: source_movements.clone(),
                        },
                    ) {
                        ctx.hand_admitted_failure(
                            terminal.movement.seq.0,
                            "invalid_provider_context",
                            serde_json::json!({
                                "error": error,
                                "content_group": content_group,
                                "expected": expected,
                                "received": received,
                            }),
                        )
                        .await?;
                        reader.commit_through(terminal)?;
                        index = consumed_index;
                        continue;
                    }
                    if let Some(error) = later_envelope_error {
                        let failure = GeminiFailure {
                            class: "contract",
                            retryable: false,
                            status: None,
                            code: None,
                            detail: format!(
                                "Gemini response group '{content_group}' received {received} of {expected} function results before a malformed outbound movement: {error}"
                            ),
                            response_bytes: None,
                            response_blake3: None,
                        };
                        drive_preflight_failure(
                            &ctx,
                            &descriptor,
                            &handoff_session,
                            &run,
                            QueuedTurn {
                                envelope,
                                seq: terminal.movement.seq.0,
                                fan_in_sources: source_movements,
                            },
                            &failure,
                        )
                        .await?;
                        reader.commit_through(terminal)?;
                        index = consumed_index;
                    } else {
                        reader.wait_for_change(&ctx.cancel).await?;
                        continue 'life;
                    }
                }
                Ok(GeminiFanInPlan::Rejected {
                    envelope,
                    consumed,
                    failure,
                    source_movements,
                }) => {
                    let consumed_index = index
                        .checked_add(consumed)
                        .ok_or_else(|| "Gemini rejected fan-in index overflowed".to_string())?;
                    if consumed == 0 || consumed_index > batch.len() {
                        return Err(format!(
                            "Gemini rejected fan-in consumed {consumed} movements from batch position {index} of {}",
                            batch.len()
                        ));
                    }
                    let terminal = &batch[consumed_index - 1];
                    let run = envelope
                        .runtime()
                        .map_err(|error| error.to_string())?
                        .run
                        .clone();
                    drive_preflight_failure(
                        &ctx,
                        &descriptor,
                        &handoff_session,
                        &run,
                        QueuedTurn {
                            envelope,
                            seq: terminal.movement.seq.0,
                            fan_in_sources: source_movements,
                        },
                        &failure,
                    )
                    .await?;
                    reader.commit_through(terminal)?;
                    index = consumed_index;
                }
                Err(failure) => {
                    drive_preflight_failure(
                        &ctx,
                        &descriptor,
                        &handoff_session,
                        &run,
                        QueuedTurn {
                            envelope: envelopes.remove(0).envelope,
                            seq: own.movement.seq.0,
                            fan_in_sources: Vec::new(),
                        },
                        &failure,
                    )
                    .await?;
                    reader.commit_through(own)?;
                    index += 1;
                }
            }
        }
    }
}

async fn drive_preflight_failure(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: QueuedTurn,
    failure: &GeminiFailure,
) -> Result<(), String> {
    let pinned_run_record = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let handoff = match validated_handoff(
        ctx,
        descriptor,
        handoff_session,
        run,
        &turn,
        &pinned_run_record,
    ) {
        Ok(handoff) => handoff,
        Err(error) => {
            ctx.hand_admitted_failure(
                turn.seq,
                "invalid_provider_context",
                serde_json::json!({
                    "error": error,
                    "preflight_failure": failure.detail,
                }),
            )
            .await?;
            return Ok(());
        }
    };
    match handoff.recover_source().await? {
        ProviderSourceRecovery::Process => emit_error(ctx, &turn, &handoff, failure, 0).await,
        ProviderSourceRecovery::Completed => Ok(()),
        ProviderSourceRecovery::Interrupted {
            last_artifact_label,
        } => Err(format!(
            "Gemini preflight replay found non-terminal artifact '{last_artifact_label}'"
        )),
    }
}

async fn drive_with_retries(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    driver: &GeminiNativeDriver,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: QueuedTurn,
) -> Result<(), String> {
    let pinned_run_record = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let handoff = match validated_handoff(
        ctx,
        descriptor,
        handoff_session,
        run,
        &turn,
        &pinned_run_record,
    ) {
        Ok(handoff) => handoff,
        Err(error) => {
            ctx.hand_admitted_failure(
                turn.seq,
                "invalid_provider_context",
                serde_json::json!({ "error": error }),
            )
            .await?;
            return Ok(());
        }
    };
    match handoff.recover_source().await? {
        ProviderSourceRecovery::Process => {}
        ProviderSourceRecovery::Completed => return Ok(()),
        ProviderSourceRecovery::Interrupted {
            last_artifact_label,
        } => {
            let failure = GeminiFailure {
                class: "interrupted",
                retryable: false,
                status: None,
                code: None,
                detail: format!(
                    "Gemini process restarted after non-terminal artifact '{last_artifact_label}'; the external model call was not replayed"
                ),
                response_bytes: None,
                response_blake3: None,
            };
            return emit_error(ctx, &turn, &handoff, &failure, 1).await;
        }
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
            return Ok(());
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
            return Ok(());
        }
    };
    if max_attempts == 0 || max_in_flight == 0 {
        let failure = GeminiFailure {
            class: "policy",
            retryable: false,
            status: None,
            code: None,
            detail: "formed interface policy denied Gemini calls".to_string(),
            response_bytes: None,
            response_blake3: None,
        };
        return emit_error(ctx, &turn, &handoff, &failure, 0).await;
    }
    if let Err(error) = driver.validate_max_attempts(max_attempts) {
        ctx.hand_admitted_failure(
            turn.seq,
            "malformed_provider_policy",
            serde_json::json!({ "field": "max_attempts", "error": error }),
        )
        .await?;
        return Ok(());
    }
    if let Err(failure) = driver.enforce_formed_model_policy(&turn.envelope) {
        return emit_error(ctx, &turn, &handoff, &failure, 0).await;
    }
    let output_root = ctx.interface_var_dir().join("emitted");
    let mut attempt = 1_u32;
    loop {
        match driver
            .receive_artifact_and_materialize_outputs(
                turn.envelope.clone(),
                turn.seq,
                attempt,
                &handoff,
                &output_root,
            )
            .await
        {
            Ok(outputs) => {
                if outputs.is_empty() {
                    return Err("Gemini driver returned no output artifacts".to_string());
                }
                handoff.hand_batch(outputs).await?;
                return Ok(());
            }
            Err(failure) if failure.retryable && attempt < max_attempts => {
                let backoff = driver.retry_delay(attempt)?;
                attempt += 1;
                tokio::select! {
                    _ = ctx.cancel.cancelled() => {
                        return Err("Gemini provider life cancelled before the movement reached an admitted outcome".to_string());
                    }
                    _ = tokio::time::sleep(backoff) => {}
                }
            }
            Err(failure) => {
                return emit_error(ctx, &turn, &handoff, &failure, attempt).await;
            }
        }
    }
}

fn validate_turn_context(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
) -> Result<(), String> {
    let pinned_run_record = ctx.pinned_run_record_bytes(&turn.envelope)?;
    validated_handoff(
        ctx,
        descriptor,
        handoff_session,
        run,
        turn,
        &pinned_run_record,
    )
    .map(|_| ())
}

fn validated_handoff(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: &QueuedTurn,
    pinned_run_record: &[u8],
) -> Result<ProviderOutputHandoff, String> {
    validate_fan_in_source_movements(ctx, run, &turn.envelope, &turn.fan_in_sources)?;
    handoff_session.from_invocation(
        ctx,
        descriptor,
        run,
        &turn.envelope,
        turn.seq,
        pinned_run_record,
    )
}

fn validate_fan_in_source_movements(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    envelope: &InterfaceInvocationEnvelope,
    sources: &[GeminiFanInSource],
) -> Result<(), String> {
    if sources.is_empty() {
        return Ok(());
    }
    let mut bindings = BTreeMap::new();
    for binding in &envelope.artifact_binding_payloads {
        validate_fan_in_bound_payload(ctx, binding)?;
        bindings.insert(
            artifact_ref_key(&binding.artifact),
            binding.artifact.clone(),
        );
    }
    let mut expected = BTreeMap::new();
    for source in sources {
        let key = artifact_ref_key(&source.artifact);
        if !bindings.contains_key(&key) {
            return Err(format!(
                "Gemini fan-in source '{}' is absent from the combined artifact bindings",
                source.artifact.id.as_str()
            ));
        }
        if expected.insert(key, source.source_movement_seq).is_some() {
            return Err(format!(
                "Gemini fan-in repeats source provenance for '{}'",
                source.artifact.id.as_str()
            ));
        }
    }
    let mut eligible = expected
        .keys()
        .cloned()
        .map(|key| (key, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let file = std::fs::File::open(&ctx.movements_path).map_err(|error| {
        format!(
            "Gemini fan-in provenance cannot open ledger '{}': {error}",
            ctx.movements_path.display()
        )
    })?;
    let mut reader = std::io::BufReader::new(file);
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let mut line = String::new();
    let mut line_number = 0usize;
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| format!("Gemini fan-in provenance ledger read failed: {error}"))?;
        if read == 0 || !line.ends_with('\n') {
            break;
        }
        let movement = decoder
            .decode(line.trim_end().as_bytes(), line_number)
            .map_err(|error| {
                format!("Gemini fan-in provenance encountered malformed ledger movement: {error}")
            })?;
        line_number += 1;
        if !matches!(movement.kind, MovementKind::InterfaceOutbound)
            || !matches!(movement.from, Endpoint::Interface(_))
            || movement.to != Endpoint::Interface(ctx.interface.clone())
        {
            continue;
        }
        let Some(artifact) = movement.artifact.as_ref() else {
            continue;
        };
        let key = artifact_ref_key(artifact);
        let Some(sequences) = eligible.get_mut(&key) else {
            continue;
        };
        let raw_metadata = movement.invocation_metadata.as_ref().ok_or_else(|| {
            format!(
                "Gemini fan-in source movement {} has no invocation metadata",
                movement.seq.0
            )
        })?;
        let metadata = raw_metadata.invocation().ok_or_else(|| {
            format!(
                "Gemini fan-in source movement {} carries non-invocation metadata",
                movement.seq.0
            )
        })?;
        if metadata
            .runtime
            .as_ref()
            .is_some_and(|runtime| runtime.run == *run && runtime.interface == ctx.interface)
        {
            sequences.insert(movement.seq.0);
        }
    }
    for (key, claimed) in expected {
        let sequences = eligible
            .get(&key)
            .ok_or_else(|| format!("Gemini fan-in source '{key}' has no ledger evidence slot"))?;
        if sequences.len() != 1 {
            return Err(format!(
                "Gemini fan-in source '{key}' has {} eligible movements; exact source provenance is ambiguous",
                sequences.len()
            ));
        }
        if let Some(claimed) = claimed {
            if !sequences.contains(&claimed) {
                return Err(format!(
                    "Gemini fan-in source '{key}' movement {claimed} is not its unique eligible ledger movement"
                ));
            }
        }
    }
    Ok(())
}

fn validate_fan_in_bound_payload(
    ctx: &InterfaceLiftContext,
    binding: &ArtifactBindingPayload,
) -> Result<(), String> {
    let limits = ctx.required_host_io_limits()?;
    let manifest_path = ctx.blobs_dir.join(binding.artifact.content.to_hex());
    let manifest_bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
        &manifest_path,
        limits.max_manifest_bytes,
    )
    .map_err(|error| {
        format!(
            "Gemini fan-in manifest blob '{}' cannot be read: {error}",
            manifest_path.display()
        )
    })?;
    if ContentHash::of_bytes(&manifest_bytes) != binding.artifact.content {
        return Err(format!(
            "Gemini fan-in manifest for '{}' does not match its ArtifactRef",
            binding.artifact.id.as_str()
        ));
    }
    let manifest: InterfaceArtifactManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| {
            format!(
                "Gemini fan-in manifest for '{}' is malformed: {error}",
                binding.artifact.id.as_str()
            )
        })?;
    let expected_id = format!("{}:{}", manifest.label, binding.artifact.content.to_hex());
    if binding.artifact.id.as_str() != expected_id {
        return Err(format!(
            "Gemini fan-in artifact '{}' is not its pinned manifest identity",
            binding.artifact.id.as_str()
        ));
    }
    let payload_hash = manifest
        .payload_content_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or_else(|| {
            format!(
                "Gemini fan-in artifact '{}' has no valid payload hash",
                binding.artifact.id.as_str()
            )
        })?;
    if ContentHash::of_bytes(binding.as_bytes()) != payload_hash {
        return Err(format!(
            "Gemini fan-in payload for '{}' does not match its pinned manifest",
            binding.artifact.id.as_str()
        ));
    }
    let payload_path = ctx.blobs_dir.join(payload_hash.to_hex());
    let payload_bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
        &payload_path,
        limits.max_payload_bytes,
    )
    .map_err(|error| {
        format!(
            "Gemini fan-in payload blob '{}' cannot be read: {error}",
            payload_path.display()
        )
    })?;
    if payload_bytes != binding.as_bytes() {
        return Err(format!(
            "Gemini fan-in payload for '{}' differs from its pinned blob",
            binding.artifact.id.as_str()
        ));
    }
    Ok(())
}

fn artifact_ref_key(artifact: &ArtifactRef) -> String {
    format!("{}:{}", artifact.id.as_str(), artifact.content.to_hex())
}

async fn emit_error(
    ctx: &InterfaceLiftContext,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    failure: &GeminiFailure,
    attempts: u32,
) -> Result<(), String> {
    let artifact = materialize_turn_error(
        &ctx.interface_var_dir().join("emitted"),
        failure,
        attempts,
        turn.seq,
    )?;
    handoff
        .hand_final(ProviderEmission {
            artifact,
            route: ProviderEmissionRoute::ClientProjection,
        })
        .await
        .map(|_| ())
}

fn required_policy_u32(envelope: &InterfaceInvocationEnvelope, name: &str) -> Result<u32, String> {
    let values = envelope
        .interface_policy()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Gemini invocation carries no formed interface policy".to_string())?;
    let value = values
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("Gemini formed interface policy has no unsigned '{name}'"))?;
    u32::try_from(value).map_err(|_| format!("Gemini policy '{name}' exceeds u32"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId};
    use octopus_interface_transport::InterfaceHostIoLimits;
    use octopus_interfaces::{
        GrantProjection, InterfaceArtifactFile, InvocationMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::{Lamport, Movement, Seq, TraceId};
    use serde_json::json;

    fn host_io_limits() -> InterfaceHostIoLimits {
        InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 1_048_576,
        }
    }

    fn pin_artifact(
        root: &Path,
        blobs_dir: &Path,
        label: &str,
        payload: &[u8],
    ) -> (InterfaceArtifactFile, ArtifactRef, Vec<u8>) {
        let artifact = InterfaceArtifactFile::write(root, label, payload).unwrap();
        let manifest_bytes = std::fs::read(artifact.manifest_path()).unwrap();
        let manifest_hash = ContentHash::of_bytes(&manifest_bytes);
        let manifest: InterfaceArtifactManifest = serde_json::from_slice(&manifest_bytes).unwrap();
        let payload_bytes = artifact.read_payload_bytes().unwrap();
        let payload_hash = manifest
            .payload_content_hash
            .as_deref()
            .and_then(ContentHash::from_hex)
            .unwrap();
        assert_eq!(ContentHash::of_bytes(&payload_bytes), payload_hash);
        std::fs::create_dir_all(blobs_dir).unwrap();
        std::fs::write(blobs_dir.join(manifest_hash.to_hex()), manifest_bytes).unwrap();
        std::fs::write(blobs_dir.join(payload_hash.to_hex()), &payload_bytes).unwrap();
        (
            artifact,
            ArtifactRef {
                id: ArtifactId::new(format!("{label}:{}", manifest_hash.to_hex())),
                content: manifest_hash,
            },
            payload_bytes,
        )
    }

    fn movement_metadata(
        run: &RunId,
        checkpoint: &str,
        interface: &InterfaceId,
    ) -> InvocationMetadata {
        InvocationMetadata::new(GrantProjection::default()).with_runtime(
            RuntimeInvocationMetadata {
                run: run.clone(),
                ee: EeId::new("ee_test"),
                worker: InterfaceId::new("system_worker_test"),
                checkpoint: CheckpointId::new(checkpoint),
                interface: interface.clone(),
                epoch: 0,
                run_record_hash: None,
            },
        )
    }

    fn write_movements(path: &Path, movements: &[Movement]) {
        let mut bytes = Vec::new();
        for movement in movements {
            serde_json::to_writer(&mut bytes, movement).unwrap();
            bytes.push(b'\n');
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn provider_descriptor() -> InterfaceDescriptor {
        serde_json::from_value(json!({
            "id": "world_provider_gemini",
            "interface_type": "world",
            "role": "provider",
            "direction": "bidirectional",
            "source_names": ["gemini"],
            "interface_driver_ref": "gemini_native",
            "interface_driver_config": {
                "private_config": {
                    "context_output_labels": [
                        "interface.artifact.output",
                        "tool.call",
                        "turn.error"
                    ],
                    "session_snapshot_labels": [
                        "interface.artifact.output",
                        "turn.error"
                    ],
                    "run_authoring_profile": {
                        "file_write_tool": "file.write",
                        "file_write_label": "tool.call",
                        "file_write_result_label": "file.write.result",
                        "result_timeout_ms": 1000
                    }
                }
            }
        }))
        .unwrap()
    }

    fn lift_context(
        root: &Path,
        descriptor: &InterfaceDescriptor,
        movements_path: PathBuf,
        blobs_dir: PathBuf,
    ) -> InterfaceLiftContext {
        let mut root_slots = BTreeMap::new();
        root_slots.insert("run_records_root".to_string(), root.join("runs"));
        root_slots.insert("interface_hand_in_root".to_string(), root.join("hand-in"));
        let (arrivals, _) = tokio::sync::mpsc::channel(1);
        InterfaceLiftContext::unprivileged(
            descriptor.id.clone(),
            root.join("registry/interface/world_provider_gemini"),
            Some(descriptor.clone()),
            root.join("var/provider"),
            movements_path,
            blobs_dir,
            root.join("ledger/ledger-index.sqlite"),
            root_slots,
            arrivals,
            Default::default(),
        )
        .with_host_io_limits(host_io_limits())
        .unwrap()
    }

    #[test]
    fn embedded_session_source_requires_exact_ledger_and_blob_proof() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let blobs_dir = root.join("ledger/blobs");
        let movements_path = root.join("ledger/movements.jsonl");
        let descriptor = provider_descriptor();
        let run = RunId::new("run_test");
        let provider = descriptor.id.clone();
        let previous_value = json!({
            "label": "interface.artifact.output",
            "text": "proved",
            "session_context": {
                "schema_version": 2,
                "provider_interface": "world_provider_gemini",
                "artifacts": []
            }
        });
        let (_, previous_ref, _) = pin_artifact(
            &root.join("previous"),
            &blobs_dir,
            "interface.artifact.output",
            &serde_json::to_vec(&previous_value).unwrap(),
        );
        let snapshot_value = json!({
            "label": "interface.artifact.output",
            "text": "snapshot",
            "session_context": {
                "schema_version": 3,
                "provider_interface": "world_provider_gemini",
                "artifacts": [{
                    "label": "interface.artifact.output",
                    "payload": {
                        "label": "interface.artifact.output",
                        "text": "forged"
                    },
                    "source_artifact": previous_ref,
                    "source_movement_seq": 0
                }]
            }
        });
        let (_, snapshot_ref, snapshot_payload) = pin_artifact(
            &root.join("snapshot"),
            &blobs_dir,
            "interface.artifact.output",
            &serde_json::to_vec(&snapshot_value).unwrap(),
        );
        let current_value = json!({
            "label": "interface.input",
            "prompt": "continue"
        });
        let (current_file, current_ref, _) = pin_artifact(
            &root.join("current"),
            &blobs_dir,
            "interface.input",
            &serde_json::to_vec(&current_value).unwrap(),
        );
        write_movements(
            &movements_path,
            &[
                Movement {
                    seq: Seq(0),
                    lamport: Lamport(1),
                    recorded_at_unix_ms: 1,
                    trace: TraceId("trace-previous".to_string()),
                    kind: MovementKind::InterfaceArrival,
                    from: Endpoint::Interface(provider.clone()),
                    to: Endpoint::Interface(movement_metadata(&run, "output", &provider).runtime().unwrap().worker.clone()),
                    artifact: Some(previous_ref),
                    invocation_metadata: Some(movement_metadata(&run, "output", &provider).into()),
                },
                Movement {
                    seq: Seq(1),
                    lamport: Lamport(2),
                    recorded_at_unix_ms: 2,
                    trace: TraceId("trace-snapshot".to_string()),
                    kind: MovementKind::InterfaceArrival,
                    from: Endpoint::Interface(provider.clone()),
                    to: Endpoint::Interface(movement_metadata(&run, "output", &provider).runtime().unwrap().worker.clone()),
                    artifact: Some(snapshot_ref.clone()),
                    invocation_metadata: Some(movement_metadata(&run, "output", &provider).into()),
                },
                Movement {
                    seq: Seq(2),
                    lamport: Lamport(3),
                    recorded_at_unix_ms: 3,
                    trace: TraceId("trace-current".to_string()),
                    kind: MovementKind::InterfaceOutbound,
                    from: Endpoint::Interface(InterfaceId::new("system_context")),
                    to: Endpoint::Interface(provider.clone()),
                    artifact: Some(current_ref.clone()),
                    invocation_metadata: Some(movement_metadata(&run, "input", &provider).into()),
                },
            ],
        );
        let run_value = json!({
            "id": "run_test",
            "ee": "ee_test",
            "worker": "system_worker_test",
            "trace": "trace:run_test",
            "status": "running",
            "checkpoints": [
                {
                    "id": "input",
                    "run": "run_test",
                    "trace": "trace:run_test:input",
                    "epoch": 0,
                    "interface_binding": octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("world_input_test")),
                    "outbound_interface_bindings": [octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("world_provider_gemini"))],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "output",
                    "run": "run_test",
                    "trace": "trace:run_test:output",
                    "epoch": 0,
                    "interface_binding": octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("world_provider_gemini")),
                    "outbound_interface_bindings": [octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("world_client_test"))],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_call",
                    "run": "run_test",
                    "trace": "trace:run_test:authoring_call",
                    "epoch": 0,
                    "interface_binding": octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("system_interface_authoring_test")),
                    "outbound_interface_bindings": [octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("system_context_authoring_test"))],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                },
                {
                    "id": "authoring_result",
                    "run": "run_test",
                    "trace": "trace:run_test:authoring_result",
                    "epoch": 0,
                    "interface_binding": octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("system_context_authoring_test")),
                    "outbound_interface_bindings": [octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("world_provider_gemini"))],
                    "artifact_bindings": [],
                    "required_verified_artifacts": []
                }
            ],
            "active_checkpoints": ["input", "output", "authoring_call", "authoring_result"],
            "interface_state": {
                "world_provider_gemini": {
                    "context_generation": 0,
                    "authoring_generation": 0,
                    "frontier_checkpoint_ids": ["output", "authoring_call", "authoring_result"]
                }
            }
        });
        let run_bytes = serde_json::to_vec(&run_value).unwrap();
        std::fs::create_dir_all(root.join("runs")).unwrap();
        std::fs::write(root.join("runs/run_test.json"), &run_bytes).unwrap();
        let metadata = movement_metadata(&run, "input", &provider)
            .with_artifact_binding_refs(vec![snapshot_ref.clone()]);
        let envelope = InterfaceInvocationEnvelope {
            artifact: current_ref,
            artifact_file: current_file,
            artifact_binding_payloads: vec![ArtifactBindingPayload::new(
                snapshot_ref,
                snapshot_payload,
            )],
            metadata: Some(metadata),
        };
        let ctx = lift_context(root, &descriptor, movements_path, blobs_dir);
        let handoff_session = ProviderHandoffSession::new(&ctx).unwrap();
        let turn = QueuedTurn {
            envelope,
            seq: 2,
            fan_in_sources: Vec::new(),
        };
        let error =
            match validated_handoff(&ctx, &descriptor, &handoff_session, &run, &turn, &run_bytes) {
                Ok(_) => panic!("forged embedded session source was admitted"),
                Err(error) => error,
            };
        assert!(
            error.contains("payload differs from its pinned source artifact"),
            "{error}"
        );
    }

    #[test]
    fn fan_in_source_seq_is_preserved_without_inventing_an_optional_value() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let blobs_dir = root.join("ledger/blobs");
        let movements_path = root.join("ledger/movements.jsonl");
        let descriptor = provider_descriptor();
        let run = RunId::new("run_test");
        let provider = descriptor.id.clone();
        let payload = serde_json::to_vec(&json!({
            "label": "artifact.read.result",
            "tool": "artifact.read",
            "correlation_id": "call-1",
            "ok": true,
            "body": {"text": "first"}
        }))
        .unwrap();
        let (artifact_file, artifact, payload) = pin_artifact(
            &root.join("source"),
            &blobs_dir,
            "artifact.read.result",
            &payload,
        );
        write_movements(
            &movements_path,
            &[Movement {
                seq: Seq(41),
                lamport: Lamport(42),
                recorded_at_unix_ms: 1,
                trace: TraceId("trace-fan-in".to_string()),
                kind: MovementKind::InterfaceOutbound,
                from: Endpoint::Interface(InterfaceId::new("system_context")),
                to: Endpoint::Interface(provider.clone()),
                artifact: Some(artifact.clone()),
                invocation_metadata: Some(movement_metadata(&run, "input", &provider).into()),
            }],
        );
        let envelope = InterfaceInvocationEnvelope {
            artifact: artifact.clone(),
            artifact_file,
            artifact_binding_payloads: vec![ArtifactBindingPayload::new(artifact.clone(), payload)],
            metadata: Some(movement_metadata(&run, "input", &provider)),
        };
        let ctx = lift_context(root, &descriptor, movements_path, blobs_dir);
        let preserved = vec![GeminiFanInSource {
            artifact: artifact.clone(),
            source_movement_seq: Some(41),
        }];
        validate_fan_in_source_movements(&ctx, &run, &envelope, &preserved).unwrap();
        assert_eq!(preserved[0].source_movement_seq, Some(41));
        let unavailable = vec![GeminiFanInSource {
            artifact: artifact.clone(),
            source_movement_seq: None,
        }];
        validate_fan_in_source_movements(&ctx, &run, &envelope, &unavailable).unwrap();
        assert_eq!(unavailable[0].source_movement_seq, None);
        let forged = vec![GeminiFanInSource {
            artifact,
            source_movement_seq: Some(42),
        }];
        let error = validate_fan_in_source_movements(&ctx, &run, &envelope, &forged).unwrap_err();
        assert!(error.contains("movement 42 is not its unique eligible ledger movement"));
    }
}
