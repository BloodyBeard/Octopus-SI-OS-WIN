use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::sync::Arc;

use octopus_descriptors::RunId;
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{
    ArtifactBindingPayload, InterfaceArtifactManifest, InterfaceDescriptor,
    InterfaceInvocationEnvelope,
};
use octopus_ledger::{ContentHash, Endpoint, MovementKind};
use octopus_ledger_store::DurableMovementDecoder;
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession, ProviderOutputHandoff,
    ProviderSourceRecovery, RunAuthoringResultIdentity,
};
use octopus_world_provider_anthropic::{
    current_tool_group_identity, pending_group_matches_replayed_group, pending_group_state,
    AnthropicToolGroupIdentity, CONTEXT_REFERENCE_LABEL, TOOL_RESULTS_PENDING_LABEL,
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;

use crate::driver::{
    materialize_anthropic_failure, GitHubCopilotAnthropicDriver, GitHubCopilotFailure,
};

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(provider_life(ctx)) as InterfaceLifeFuture)
}

struct QueuedTurn {
    envelope: InterfaceInvocationEnvelope,
    seq: u64,
}

async fn provider_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let activation_secrets_home = ctx.root_slot("activation_secrets_home")?.to_path_buf();
    let driver =
        GitHubCopilotAnthropicDriver::from_descriptor(&descriptor, activation_secrets_home)?;
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
            drive_with_retries(
                &ctx,
                &descriptor,
                &driver,
                &handoff_session,
                &run,
                QueuedTurn {
                    envelope,
                    seq: own.movement.seq.0,
                },
            )
            .await?;
            reader.commit_through(&own)?;
        }
    }
}

async fn drive_with_retries(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    driver: &GitHubCopilotAnthropicDriver,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    mut turn: QueuedTurn,
) -> Result<(), String> {
    attach_pending_tool_frontier(ctx, driver, run, &mut turn)?;
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
        ProviderSourceRecovery::Bridged { .. } => return Ok(()),
        ProviderSourceRecovery::Completed => return Ok(()),
        ProviderSourceRecovery::Interrupted {
            last_artifact_label,
        } => {
            return emit_error(
                ctx,
                run,
                &turn,
                &handoff,
                GitHubCopilotFailure::interrupted(format!(
                    "GitHub Copilot Anthropic process restarted after non-terminal artifact '{last_artifact_label}'; the paid external turn was not replayed"
                )),
            )
            .await;
        }
    }
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
            &turn,
            &handoff,
            GitHubCopilotFailure::unavailable(
                "formed interface policy denied GitHub Copilot Anthropic calls",
            ),
        )
        .await;
    }
    if let Err(failure) = driver.enforce_formed_model_policy(&turn.envelope) {
        return emit_error(ctx, run, &turn, &handoff, failure).await;
    }
    if let Err(error) = driver.validate_retry_budget(max_attempts) {
        return emit_error(
            ctx,
            run,
            &turn,
            &handoff,
            GitHubCopilotFailure::contract(error),
        )
        .await;
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
                    return emit_error(
                        ctx,
                        run,
                        &turn,
                        &handoff,
                        GitHubCopilotFailure::contract(
                            "GitHub Copilot Anthropic driver produced no output artifacts",
                        ),
                    )
                    .await;
                }
                handoff.hand_batch(outputs).await?;
                return Ok(());
            }
            Err(failure) if failure.retryable && attempt < max_attempts => {
                let backoff = driver.retry_backoff(attempt)?;
                attempt += 1;
                tokio::select! {
                    _ = ctx.cancel.cancelled() => {
                        return Err("GitHub Copilot Anthropic life cancelled before the movement reached an admitted outcome".to_string());
                    }
                    _ = tokio::time::sleep(backoff) => {}
                }
            }
            Err(failure) if failure.retryable => {
                return emit_error(
                    ctx,
                    run,
                    &turn,
                    &handoff,
                    GitHubCopilotFailure::exhausted(
                        format!(
                            "GitHub Copilot Anthropic turn failed after {attempt} attempts: {}",
                            failure.detail
                        ),
                        failure.evidence,
                    ),
                )
                .await;
            }
            Err(failure) => {
                return emit_error(ctx, run, &turn, &handoff, failure).await;
            }
        }
    }
}

async fn emit_error(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    failure: GitHubCopilotFailure,
) -> Result<(), String> {
    let artifact =
        materialize_anthropic_failure(&ctx.interface_var_dir().join("emitted"), &failure)
            .map_err(|error| {
                format!(
                    "GitHub Copilot Anthropic terminal error for run '{}' movement {} failed to materialize: {}",
                    run, turn.seq, error.detail
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

fn attach_pending_tool_frontier(
    ctx: &InterfaceLiftContext,
    driver: &GitHubCopilotAnthropicDriver,
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
                        "GitHub Copilot Anthropic bound artifact '{}' is not JSON while recovering its frontier: {error}",
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
                "GitHub Copilot Anthropic result for recovered group '{}' carries unrelated bound artifact '{}'",
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
    let value: serde_json::Value = serde_json::from_slice(payload).map_err(|error| {
        format!("GitHub Copilot Anthropic pending snapshot is not JSON: {error}")
    })?;
    let artifacts = value
        .get("session_context")
        .and_then(|context| context.get("artifacts"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            "GitHub Copilot Anthropic pending snapshot has no session_context.artifacts".to_string()
        })?;
    artifacts
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let source: octopus_ledger::ArtifactRef = serde_json::from_value(
                item.get("source_artifact").cloned().ok_or_else(|| {
                    format!(
                        "GitHub Copilot Anthropic pending snapshot artifact[{index}] has no source_artifact"
                    )
                })?,
            )
            .map_err(|error| {
                format!(
                    "GitHub Copilot Anthropic pending snapshot artifact[{index}] source is malformed: {error}"
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
            "GitHub Copilot Anthropic frontier recovery cannot open ledger '{}': {error}",
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
        let read = reader.read_line(&mut line).map_err(|error| {
            format!("GitHub Copilot Anthropic frontier ledger read failed: {error}")
        })?;
        if read == 0 || !line.ends_with('\n') {
            break;
        }
        let movement = decoder
            .decode(line.trim_end().as_bytes(), line_number)
            .map_err(|error| {
                format!("GitHub Copilot Anthropic frontier ledger is malformed: {error}")
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
                "GitHub Copilot Anthropic pending frontier artifact '{}' is not JSON: {error}",
                artifact.id.as_str()
            )
        })?;
        if value.get("session_context").is_none() {
            return Err(format!(
                "GitHub Copilot Anthropic pending frontier artifact '{}' has no durable session context",
                artifact.id.as_str()
            ));
        }
        let state = pending_group_state(&value)?;
        if state.provider_interface != ctx.interface || state.run != *run {
            return Err(format!(
                "GitHub Copilot Anthropic pending frontier artifact '{}' has mismatched provider/run identity",
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
            "GitHub Copilot Anthropic pending frontier manifest blob '{}' cannot be read: {error}",
            manifest_path.display()
        )
    })?;
    let observed_manifest = ContentHash::of_bytes(&manifest_bytes);
    if observed_manifest != artifact.content {
        return Err(format!(
            "GitHub Copilot Anthropic pending frontier manifest '{}' hashes to {}, expected {}",
            artifact.id.as_str(),
            observed_manifest.to_hex(),
            artifact.content.to_hex()
        ));
    }
    let manifest: InterfaceArtifactManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| {
            format!(
                "GitHub Copilot Anthropic pending frontier manifest '{}' is malformed: {error}",
                artifact.id.as_str()
            )
        })?;
    if manifest.label != TOOL_RESULTS_PENDING_LABEL {
        return Err(format!(
            "GitHub Copilot Anthropic pending frontier artifact '{}' carries manifest label '{}'",
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
                "GitHub Copilot Anthropic pending frontier artifact '{}' has no valid payload hash",
                artifact.id.as_str()
            )
        })?;
    let payload_path = ctx.blobs_dir.join(payload_hash.to_hex());
    let payload = std::fs::read(&payload_path).map_err(|error| {
        format!(
            "GitHub Copilot Anthropic pending frontier payload blob '{}' cannot be read: {error}",
            payload_path.display()
        )
    })?;
    let observed_payload = ContentHash::of_bytes(&payload);
    if observed_payload != payload_hash {
        return Err(format!(
            "GitHub Copilot Anthropic pending frontier payload '{}' hashes to {}, expected {}",
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
        .ok_or_else(|| "provider invocation carries no formed interface policy".to_string())?;
    let value = values
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("provider formed interface policy carries no unsigned '{name}'"))?;
    u32::try_from(value).map_err(|_| format!("provider policy '{name}' exceeds u32"))
}
