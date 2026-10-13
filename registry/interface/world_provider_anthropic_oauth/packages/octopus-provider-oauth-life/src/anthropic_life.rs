use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::time::Duration;

use octopus_descriptors::RunId;
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{
    ArtifactBindingPayload, InterfaceArtifactFile, InterfaceArtifactManifest,
    InterfaceBoundaryError, InterfaceDescriptor, InterfaceInvocationEnvelope,
};
use octopus_ledger::{ContentHash, Endpoint, MovementKind};
use octopus_ledger_store::DurableMovementDecoder;
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession, ProviderOutputHandoff,
    ProviderSourceRecovery, RunAuthoringResultIdentity,
};
use octopus_world_provider_anthropic::{
    current_tool_group_identity, materialize_provider_turn_error,
    materialize_provider_turn_typed_error, pending_group_matches_replayed_group,
    pending_group_state, AnthropicMessagesDriver, AnthropicToolGroupIdentity,
    CONTEXT_REFERENCE_LABEL, TOOL_RESULTS_PENDING_LABEL,
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;

use crate::auth::{OAuthCredentialManager, OAuthFailure};
use crate::dispatch::OAuthDispatchAdmission;

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(provider_life(ctx)) as InterfaceLifeFuture)
}

struct QueuedTurn {
    envelope: InterfaceInvocationEnvelope,
    seq: u64,
    label: String,
}

async fn provider_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let secrets_home = ctx.root_slot("activation_secrets_home")?.to_path_buf();
    let credentials = OAuthCredentialManager::from_descriptor(&descriptor, &secrets_home)
        .map_err(|error| error.to_string())?;
    let driver = AnthropicMessagesDriver::from_descriptor(&descriptor, secrets_home)?;
    let authoring_result = RunAuthoringResultIdentity::from_descriptor(&descriptor)?;
    let handoff_session = ProviderHandoffSession::new(&ctx)?;
    let mut reader = LedgerTailReader::open(&ctx)?;
    ctx.signal_ready()?;
    loop {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        let batch = reader.read_new()?;
        if batch.is_empty() {
            reader.wait_for_change(&ctx.cancel).await?;
            continue;
        }
        for own in batch {
            let envelope = match reader.envelope(&own) {
                Ok(envelope) => envelope,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "poisoned_outbound",
                        serde_json::json!({"error": error}),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let manifest = match envelope.artifact_file.read_manifest() {
                Ok(manifest) => manifest,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "malformed_artifact_manifest",
                        serde_json::json!({"error": error.to_string()}),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let metadata = match own.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "malformed_runtime_envelope",
                        serde_json::json!({"error": error}),
                    )
                    .await?;
                    reader.commit_through(&own)?;
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
                reader.commit_through(&own)?;
                continue;
            };
            if authoring_result
                .consume_if_matches(&ctx, &own.movement, &envelope, &manifest.label)
                .await?
            {
                reader.commit_through(&own)?;
                continue;
            }
            drive_turn(
                &ctx,
                &descriptor,
                &driver,
                &credentials,
                &handoff_session,
                &run,
                QueuedTurn {
                    envelope,
                    seq: own.movement.seq.0,
                    label: manifest.label,
                },
            )
            .await?;
            reader.commit_through(&own)?;
        }
    }
}

async fn drive_turn(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    driver: &AnthropicMessagesDriver,
    credentials: &OAuthCredentialManager,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    mut turn: QueuedTurn,
) -> Result<(), String> {
    if !OAuthCredentialManager::handles_label(&turn.label) {
        attach_pending_tool_frontier(ctx, driver, run, &mut turn)?;
    }
    let pinned_run_record = ctx.pinned_run_record_bytes(&turn.envelope)?;
    let handoff = handoff_session.from_invocation(
        ctx,
        descriptor,
        run,
        &turn.envelope,
        turn.seq,
        &pinned_run_record,
    )?;
    match handoff.recover_source().await? {
        ProviderSourceRecovery::Process => {}
        ProviderSourceRecovery::Completed => return Ok(()),
        ProviderSourceRecovery::Interrupted {
            last_artifact_label,
        } => {
            return emit_error(
                ctx,
                run,
                &turn,
                &handoff,
                "interrupted",
                &format!(
                    "OAuth Messages process restarted after non-terminal artifact '{last_artifact_label}'; the external call was not replayed"
                ),
            )
            .await;
        }
    }
    if OAuthCredentialManager::handles_label(&turn.label) {
        return drive_auth_artifact(ctx, credentials, &turn.label, run, &turn, &handoff).await;
    }
    drive_provider_artifact(ctx, descriptor, driver, credentials, run, &turn, &handoff).await
}

async fn drive_auth_artifact(
    ctx: &InterfaceLiftContext,
    credentials: &OAuthCredentialManager,
    label: &str,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
) -> Result<(), String> {
    let payload = match turn.envelope.read_artifact_payload() {
        Ok(payload) => payload,
        Err(error) => {
            return emit_error(
                ctx,
                run,
                turn,
                handoff,
                "protocol",
                &format!("OAuth artifact cannot be read: {error}"),
            )
            .await;
        }
    };
    let output_root = ctx.interface_var_dir().join("emitted");
    let credential_handle_fingerprint = credentials.credential_handle_fingerprint();
    let dispatch = OAuthDispatchAdmission {
        handoff,
        output_root: &output_root,
        provider_interface: credentials.interface_id(),
        source_movement_seq: turn.seq,
        attempt: 1,
        credential_handle_fingerprint: Some(&credential_handle_fingerprint),
    };
    match credentials
        .handle_artifact_with_dispatch(label, &payload, &dispatch)
        .await
    {
        Ok(output) => {
            let bytes = serde_json::to_vec(&output.payload)
                .map_err(|error| format!("OAuth auth output did not encode: {error}"))?;
            let artifact = InterfaceArtifactFile::write(&output_root, output.label, &bytes)
                .map_err(|error| format!("OAuth auth output did not materialize: {error}"))?;
            handoff
                .hand_final(ProviderEmission {
                    artifact,
                    route: ProviderEmissionRoute::ClientProjection,
                })
                .await
                .map(|_| ())
        }
        Err(error) => emit_auth_error(ctx, run, turn, handoff, &error).await,
    }
}

async fn drive_provider_artifact(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    driver: &AnthropicMessagesDriver,
    credentials: &OAuthCredentialManager,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
) -> Result<(), String> {
    let max_attempts = match required_policy_u32(&turn.envelope, "max_attempts") {
        Ok(value) => value,
        Err(error) => {
            ctx.hand_admitted_failure(
                turn.seq,
                "malformed_provider_policy",
                serde_json::json!({"field": "max_attempts", "error": error}),
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
                serde_json::json!({"field": "max_in_flight", "error": error}),
            )
            .await?;
            return Ok(());
        }
    };
    if max_attempts == 0 || max_in_flight == 0 {
        return emit_error(
            ctx,
            run,
            turn,
            handoff,
            "unavailable",
            "formed interface policy denied OAuth provider calls",
        )
        .await;
    }
    if let Err(error) = driver.enforce_formed_model_policy(&turn.envelope) {
        return emit_error(ctx, run, turn, handoff, "unavailable", &error.to_string()).await;
    }
    if let Err(error) = validate_retry_budget(descriptor, max_attempts) {
        return emit_error(ctx, run, turn, handoff, "unavailable", &error).await;
    }
    if let Err(error) = credentials.ensure_access_token(false).await {
        return emit_auth_error(ctx, run, turn, handoff, &error).await;
    }
    let output_root = ctx.interface_var_dir().join("emitted");
    let mut attempt = 1_u32;
    let mut authenticated_401_retried = false;
    loop {
        match driver
            .receive_artifact_and_materialize_outputs(
                turn.envelope.clone(),
                turn.seq,
                attempt,
                handoff,
                &output_root,
            )
            .await
        {
            Ok(outputs) => {
                if outputs.is_empty() {
                    return emit_error(
                        ctx,
                        run,
                        turn,
                        handoff,
                        "invalid_response",
                        "OAuth Messages driver produced no output artifacts",
                    )
                    .await;
                }
                handoff.hand_batch(outputs).await?;
                return Ok(());
            }
            Err(error)
                if !authenticated_401_retried
                    && attempt < max_attempts
                    && unauthorized_messages_error(&error) =>
            {
                authenticated_401_retried = true;
                attempt = attempt.saturating_add(1);
                if let Err(error) = credentials.ensure_access_token(true).await {
                    return emit_auth_error(ctx, run, turn, handoff, &error).await;
                }
            }
            Err(InterfaceBoundaryError::Protocol(error)) => {
                return emit_error(ctx, run, turn, handoff, "invalid_response", &error).await;
            }
            Err(InterfaceBoundaryError::FailedClosed(error)) => {
                return emit_error(ctx, run, turn, handoff, "unavailable", &error).await;
            }
            Err(InterfaceBoundaryError::Transport(_)) if attempt < max_attempts => {
                let backoff = retry_backoff(descriptor, attempt)?;
                attempt += 1;
                tokio::select! {
                    _ = ctx.cancel.cancelled() => {
                        return Err("OAuth Messages life cancelled before the movement reached an admitted outcome".to_string());
                    }
                    _ = tokio::time::sleep(backoff) => {}
                }
            }
            Err(InterfaceBoundaryError::Transport(error)) => {
                return emit_error(
                    ctx,
                    run,
                    turn,
                    handoff,
                    "transport",
                    &format!(
                        "OAuth Messages turn failed after {attempt} transport attempts: {error}"
                    ),
                )
                .await;
            }
        }
    }
}

async fn emit_auth_error(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    error: &OAuthFailure,
) -> Result<(), String> {
    let artifact = materialize_provider_turn_typed_error(
        &ctx.interface_var_dir().join("emitted"),
        error.class,
        error.code,
        &error.detail,
        error.evidence.clone(),
    )
    .map_err(|materialize_error| {
        format!(
            "OAuth Messages auth error for run '{}' movement {} failed to materialize: {materialize_error}",
            run, turn.seq
        )
    })?;
    handoff
        .hand_final(ProviderEmission {
            artifact,
            route: ProviderEmissionRoute::ClientProjection,
        })
        .await
        .map(|_| ())
}

async fn emit_error(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    class: &str,
    message: &str,
) -> Result<(), String> {
    let artifact =
        materialize_provider_turn_error(&ctx.interface_var_dir().join("emitted"), class, message)
            .map_err(|error| {
            format!(
            "OAuth Messages terminal error for run '{}' movement {} failed to materialize: {error}",
            run, turn.seq
        )
        })?;
    handoff
        .hand_final(ProviderEmission {
            artifact,
            route: ProviderEmissionRoute::ClientProjection,
        })
        .await
        .map(|_| ())
}

fn unauthorized_messages_error(error: &InterfaceBoundaryError) -> bool {
    match error {
        InterfaceBoundaryError::Protocol(message)
        | InterfaceBoundaryError::FailedClosed(message)
        | InterfaceBoundaryError::Transport(message) => message
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair == ["HTTP", "401"]),
    }
}

fn validate_retry_budget(
    descriptor: &InterfaceDescriptor,
    max_attempts: u32,
) -> Result<(), String> {
    let backoffs = retry_backoffs(descriptor)?;
    let retries = max_attempts.saturating_sub(1) as usize;
    if retries > backoffs.len() {
        return Err(format!(
            "OAuth Messages formed max_attempts {max_attempts} requires {retries} backoff entries but the descriptor declares {}",
            backoffs.len()
        ));
    }
    Ok(())
}

fn retry_backoff(
    descriptor: &InterfaceDescriptor,
    failed_attempt: u32,
) -> Result<Duration, String> {
    let backoffs = retry_backoffs(descriptor)?;
    let index = failed_attempt
        .checked_sub(1)
        .ok_or_else(|| "OAuth Messages retry attempt must be positive".to_string())?
        as usize;
    backoffs
        .get(index)
        .copied()
        .map(Duration::from_millis)
        .ok_or_else(|| {
            format!("OAuth Messages descriptor has no backoff for failed attempt {failed_attempt}")
        })
}

fn retry_backoffs(descriptor: &InterfaceDescriptor) -> Result<Vec<u64>, String> {
    let values = descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get("retry_policy"))
        .and_then(|value| value.get("backoff_ms"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "OAuth Messages descriptor has no retry_policy.backoff_ms".to_string())?;
    let mut output = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let delay = value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
            format!("OAuth Messages retry_policy.backoff_ms[{index}] must be positive")
        })?;
        output.push(delay);
    }
    Ok(output)
}

fn attach_pending_tool_frontier(
    ctx: &InterfaceLiftContext,
    driver: &AnthropicMessagesDriver,
    run: &RunId,
    turn: &mut QueuedTurn,
) -> Result<(), String> {
    let Some(group) = current_tool_group_identity(
        &turn.envelope,
        &ctx.interface,
        run,
        turn.seq,
        driver.tool_name_prefix(),
    )?
    else {
        return Ok(());
    };
    let Some((artifact, payload)) = latest_pending_tool_frontier(ctx, run, &group)? else {
        return Ok(());
    };
    let pending_sources = pending_snapshot_sources(&payload)?;
    let mut static_context = Vec::new();
    for bound in turn.envelope.artifact_binding_payloads.drain(..) {
        let value: serde_json::Value =
            serde_json::from_slice(&bound.read_bytes().map_err(|error| error.to_string())?)
                .map_err(|error| {
                    format!(
                        "OAuth Messages bound artifact '{}' is not JSON while recovering its frontier: {error}",
                        bound.artifact.id.as_str()
                    )
                })?;
        let label = value
            .get("label")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                bound
                    .artifact
                    .id
                    .as_str()
                    .rsplit_once(':')
                    .map(|(label, _)| label)
            });
        if label == Some(CONTEXT_REFERENCE_LABEL) {
            static_context.push(bound);
        } else if value.get("session_context").is_none()
            && !pending_sources.contains(&format!(
                "{}:{}",
                bound.artifact.id.as_str(),
                bound.artifact.content.to_hex()
            ))
        {
            return Err(format!(
                "OAuth Messages result for recovered group '{}' carries unrelated bound artifact '{}'",
                group.response_id,
                bound.artifact.id.as_str()
            ));
        }
    }
    turn.envelope.artifact_binding_payloads = static_context;
    turn.envelope
        .artifact_binding_payloads
        .push(ArtifactBindingPayload::new(artifact, payload));
    Ok(())
}

fn pending_snapshot_sources(payload: &[u8]) -> Result<BTreeSet<String>, String> {
    let value: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|error| format!("OAuth Messages pending snapshot is not JSON: {error}"))?;
    let artifacts = value
        .get("session_context")
        .and_then(|context| context.get("artifacts"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            "OAuth Messages pending snapshot has no session_context.artifacts".to_string()
        })?;
    artifacts
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let source: octopus_ledger::ArtifactRef =
                serde_json::from_value(item.get("source_artifact").cloned().ok_or_else(|| {
                    format!(
                        "OAuth Messages pending snapshot artifact[{index}] has no source_artifact"
                    )
                })?)
                .map_err(|error| {
                    format!(
                    "OAuth Messages pending snapshot artifact[{index}] source is malformed: {error}"
                )
                })?;
            Ok(format!(
                "{}:{}",
                source.id.as_str(),
                source.content.to_hex()
            ))
        })
        .collect()
}

fn latest_pending_tool_frontier(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    group: &AnthropicToolGroupIdentity,
) -> Result<Option<(octopus_ledger::ArtifactRef, Vec<u8>)>, String> {
    let file = std::fs::File::open(&ctx.movements_path).map_err(|error| {
        format!(
            "OAuth Messages frontier recovery cannot open ledger '{}': {error}",
            ctx.movements_path.display()
        )
    })?;
    let mut reader = BufReader::new(file);
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let mut line = String::new();
    let mut line_number = 0usize;
    let mut latest = None;
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| format!("OAuth Messages frontier ledger read failed: {error}"))?;
        if read == 0 || !line.ends_with('\n') {
            break;
        }
        let movement = decoder
            .decode(line.trim_end().as_bytes(), line_number)
            .map_err(|error| {
                format!("OAuth Messages frontier ledger contains malformed movement: {error}")
            })?;
        line_number += 1;
        if movement.kind != MovementKind::InterfaceArrival
            || movement.from != Endpoint::Interface(ctx.interface.clone())
            || !movement.runtime().is_some_and(|runtime| {
                &runtime.run == run
                    && runtime.interface == ctx.interface
                    && movement.to == Endpoint::Interface(runtime.worker.clone())
            })
        {
            continue;
        }
        let Some(artifact) = movement.artifact.as_ref() else {
            continue;
        };
        if artifact
            .id
            .as_str()
            .rsplit_once(':')
            .map(|(label, _)| label)
            != Some(TOOL_RESULTS_PENDING_LABEL)
        {
            continue;
        }
        let payload = read_pinned_artifact_payload(ctx, artifact)?;
        let value: serde_json::Value = serde_json::from_slice(&payload).map_err(|error| {
            format!(
                "OAuth Messages pending frontier artifact '{}' is not JSON: {error}",
                artifact.id.as_str()
            )
        })?;
        if value.get("session_context").is_none() {
            return Err(format!(
                "OAuth Messages pending frontier artifact '{}' has no durable session context",
                artifact.id.as_str()
            ));
        }
        let state = pending_group_state(&value)?;
        if state.provider_interface != ctx.interface || state.run != *run {
            return Err(format!(
                "OAuth Messages pending frontier artifact '{}' has mismatched provider/run identity",
                artifact.id.as_str()
            ));
        }
        if !pending_group_matches_replayed_group(&state.group, group) {
            continue;
        }
        latest = Some((movement.seq.0, artifact.clone(), payload));
    }
    Ok(latest.map(|(_, artifact, payload)| (artifact, payload)))
}

fn read_pinned_artifact_payload(
    ctx: &InterfaceLiftContext,
    artifact: &octopus_ledger::ArtifactRef,
) -> Result<Vec<u8>, String> {
    let manifest_path = ctx.blobs_dir.join(artifact.content.to_hex());
    let manifest_bytes = std::fs::read(&manifest_path).map_err(|error| {
        format!(
            "OAuth Messages pending frontier manifest blob '{}' cannot be read: {error}",
            manifest_path.display()
        )
    })?;
    let observed_manifest = ContentHash::of_bytes(&manifest_bytes);
    if observed_manifest != artifact.content {
        return Err(format!(
            "OAuth Messages pending frontier manifest '{}' hashes to {}, expected {}",
            artifact.id.as_str(),
            observed_manifest.to_hex(),
            artifact.content.to_hex()
        ));
    }
    let manifest: InterfaceArtifactManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| {
            format!(
                "OAuth Messages pending frontier manifest '{}' is malformed: {error}",
                artifact.id.as_str()
            )
        })?;
    if manifest.label != TOOL_RESULTS_PENDING_LABEL {
        return Err(format!(
            "OAuth Messages pending frontier artifact '{}' carries manifest label '{}'",
            artifact.id.as_str(),
            manifest.label
        ));
    }
    let payload_hash = manifest
        .payload_content_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or_else(|| {
            format!(
                "OAuth Messages pending frontier artifact '{}' has no valid payload hash",
                artifact.id.as_str()
            )
        })?;
    let payload_path = ctx.blobs_dir.join(payload_hash.to_hex());
    let payload = std::fs::read(&payload_path).map_err(|error| {
        format!(
            "OAuth Messages pending frontier payload blob '{}' cannot be read: {error}",
            payload_path.display()
        )
    })?;
    let observed_payload = ContentHash::of_bytes(&payload);
    if observed_payload != payload_hash {
        return Err(format!(
            "OAuth Messages pending frontier payload '{}' hashes to {}, expected {}",
            artifact.id.as_str(),
            observed_payload.to_hex(),
            payload_hash.to_hex()
        ));
    }
    Ok(payload)
}

fn required_policy_u32(envelope: &InterfaceInvocationEnvelope, name: &str) -> Result<u32, String> {
    let values = envelope
        .interface_policy()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "OAuth Messages invocation carries no formed interface policy".to_string()
        })?;
    let value = values
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            format!("OAuth Messages formed interface policy carries no unsigned '{name}'")
        })?;
    u32::try_from(value).map_err(|_| format!("OAuth Messages policy '{name}' exceeds u32"))
}
