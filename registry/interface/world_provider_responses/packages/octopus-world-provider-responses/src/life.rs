use std::sync::Arc;

use octopus_descriptors::RunId;
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{InterfaceDescriptor, InterfaceInvocationEnvelope};
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession, ProviderOutputHandoff,
    ProviderSourceRecovery, RunAuthoringResultIdentity,
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use serde_json::Value;

use crate::driver::ResponsesDriverFailure;
use crate::{materialize_provider_turn_error, OpenAiResponsesDriver, ResponsesFailure};

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
    let driver = OpenAiResponsesDriver::from_descriptor(&descriptor, activation_secrets_home)?;
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
    driver: &OpenAiResponsesDriver,
    handoff_session: &ProviderHandoffSession,
    run: &RunId,
    turn: QueuedTurn,
) -> Result<(), String> {
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
                ResponsesFailure::interrupted(format!(
                    "provider process restarted after non-terminal artifact '{last_artifact_label}'; the external Responses call was not replayed"
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
            ResponsesFailure::unavailable("formed interface policy denied provider calls"),
        )
        .await;
    }
    if let Err(failure) = driver.enforce_formed_model_policy(&turn.envelope) {
        return emit_error(ctx, run, &turn, &handoff, failure).await;
    }
    let output_root = ctx.interface_var_dir().join("emitted");
    let mut attempt = 1_u32;
    let mut authenticated_refresh_replayed = false;
    loop {
        match driver
            .receive_artifact_and_materialize_outputs_with_auth_refresh(
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
                        ResponsesFailure::contract("Responses driver produced no output artifacts"),
                    )
                    .await;
                }
                handoff.hand_batch(outputs).await?;
                return Ok(());
            }
            Err(ResponsesDriverFailure::AuthenticatedRefreshRequired(_))
                if authenticated_refresh_replay_allowed(
                    attempt,
                    max_attempts,
                    authenticated_refresh_replayed,
                ) =>
            {
                authenticated_refresh_replayed = true;
                if let Err(refresh_failure) = driver.refresh_authentication().await {
                    return emit_error(ctx, run, &turn, &handoff, refresh_failure).await;
                }
                attempt = attempt.saturating_add(1);
            }
            Err(ResponsesDriverFailure::AuthenticatedRefreshRequired(failure)) => {
                return emit_error(ctx, run, &turn, &handoff, failure).await;
            }
            Err(ResponsesDriverFailure::Failure(failure))
                if failure.retryable && attempt < max_attempts =>
            {
                let backoff = driver.retry_backoff(attempt);
                attempt += 1;
                tokio::select! {
                    _ = ctx.cancel.cancelled() => {
                        return Err("Responses provider life cancelled before the movement reached an admitted outcome".to_string());
                    }
                    _ = tokio::time::sleep(backoff) => {}
                }
            }
            Err(ResponsesDriverFailure::Failure(failure)) if failure.retryable => {
                return emit_error(
                    ctx,
                    run,
                    &turn,
                    &handoff,
                    ResponsesFailure::exhausted(
                        format!(
                            "Responses turn failed after {attempt} attempts: {}",
                            failure.detail
                        ),
                        failure.evidence,
                    ),
                )
                .await;
            }
            Err(ResponsesDriverFailure::Failure(failure)) => {
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
    failure: ResponsesFailure,
) -> Result<(), String> {
    let artifact =
        materialize_provider_turn_error(&ctx.interface_var_dir().join("emitted"), &failure)
            .map_err(|error| {
                format!(
            "Responses provider terminal error for run '{}' movement {} failed to materialize: {}",
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

fn required_policy_u32(envelope: &InterfaceInvocationEnvelope, name: &str) -> Result<u32, String> {
    let values = envelope
        .interface_policy()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Responses invocation carries no formed interface policy".to_string())?;
    let value = values
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("Responses formed interface policy carries no unsigned '{name}'"))?;
    u32::try_from(value).map_err(|_| format!("Responses policy '{name}' exceeds u32"))
}

fn authenticated_refresh_replay_allowed(
    attempt: u32,
    max_attempts: u32,
    already_replayed: bool,
) -> bool {
    !already_replayed && attempt < max_attempts
}

#[cfg(test)]
mod tests {
    use super::authenticated_refresh_replay_allowed;

    #[test]
    fn authenticated_refresh_replay_requires_remaining_attempt_budget() {
        assert!(!authenticated_refresh_replay_allowed(1, 1, false));
        assert!(authenticated_refresh_replay_allowed(1, 2, false));
        assert!(authenticated_refresh_replay_allowed(2, 3, false));
        assert!(!authenticated_refresh_replay_allowed(2, 2, false));
        assert!(!authenticated_refresh_replay_allowed(1, 2, true));
    }
}
