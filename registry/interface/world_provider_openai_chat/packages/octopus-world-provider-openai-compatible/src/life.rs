use std::sync::Arc;

use octopus_descriptors::RunId;
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{InterfaceDescriptor, InterfaceInvocationEnvelope};
use octopus_provider_credential_pool::CredentialPoolDecision;
use octopus_provider_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession, ProviderOutputHandoff,
    ProviderSourceRecovery, RunAuthoringResultIdentity,
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;

use crate::world_provider::PreparedProviderTurn;
use crate::{materialize_provider_turn_error, ProviderErrorEvidence, WorldProviderInterfaceDriver};

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(provider_life(ctx)) as InterfaceLifeFuture)
}

struct QueuedTurn {
    envelope: InterfaceInvocationEnvelope,
    seq: u64,
}

async fn provider_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = load_own_descriptor(&ctx)?;
    let secret_home = ctx.root_slot("activation_secrets_home")?.to_path_buf();
    let driver = WorldProviderInterfaceDriver::from_descriptor_with_roots(
        &descriptor,
        Some(&secret_home),
        Some(&ctx.interface_var_dir()),
    )?;
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
                        serde_json::json!({ "error": error }),
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
                        serde_json::json!({ "error": error.to_string() }),
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
                        serde_json::json!({ "error": error }),
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
    driver: &WorldProviderInterfaceDriver,
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
            return emit_error_message(
                ctx,
                run,
                &turn,
                &handoff,
                &format!(
                    "provider process restarted after non-terminal artifact '{last_artifact_label}'; the interrupted transport turn was closed without replaying the external model call"
                ),
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
        let evidence = ProviderErrorEvidence::new(
            "failed_closed",
            "provider_policy_denied",
            "formed interface policy denied provider calls",
        );
        return emit_turn_error(ctx, run, &turn, &handoff, max_attempts, &evidence).await;
    }
    if let Err(error) = driver.enforce_formed_model_policy(&turn.envelope) {
        return emit_turn_error(ctx, run, &turn, &handoff, 0, error.evidence()).await;
    }
    if let Err(error) = driver.validate_retry_budget(max_attempts) {
        let evidence =
            ProviderErrorEvidence::new("failed_closed", "provider_retry_policy_invalid", error);
        return emit_turn_error(ctx, run, &turn, &handoff, 0, &evidence).await;
    }
    let output_root = ctx.interface_var_dir().join("emitted");
    let callable_tools = match handoff.callable_tool_names(&turn.envelope) {
        Ok(tools) => tools,
        Err(error) => {
            let evidence = ProviderErrorEvidence::new("failed_closed", "provider_tool_topology_invalid", error);
            return emit_turn_error(ctx, run, &turn, &handoff, 0, &evidence).await;
        }
    };
    let prepared = match driver
        .prepare_artifact_and_materialize_local_outputs(turn.envelope.clone(), &output_root, &callable_tools)
    {
        Ok(PreparedProviderTurn::Local(outputs)) => {
            handoff.hand_batch(outputs).await?;
            return Ok(());
        }
        Ok(PreparedProviderTurn::Network(prepared)) => prepared,
        Err(error) => {
            return emit_turn_error(ctx, run, &turn, &handoff, 0, error.evidence()).await;
        }
    };
    let mut attempt = 1_u32;
    let mut authenticated_refresh_replayed = false;
    let mut credential_candidate = None;
    loop {
        let credential = match driver
            .select_credential_candidate(credential_candidate.as_ref(), credential_clock()?)
        {
            Ok(credential) => credential,
            Err(error) => {
                return emit_turn_error(ctx, run, &turn, &handoff, attempt, error.evidence()).await;
            }
        };
        let result = driver
            .execute_prepared_network_turn(
                &prepared,
                turn.seq,
                attempt,
                credential.as_ref(),
                &handoff,
                &output_root,
            )
            .await;
        match result {
            Ok(outputs) => {
                if let Err(error) =
                    driver.record_credential_success(credential.as_ref(), credential_clock()?)
                {
                    return emit_turn_error(ctx, run, &turn, &handoff, attempt, error.evidence())
                        .await;
                }
                handoff.hand_batch(outputs).await?;
                return Ok(());
            }
            Err(error) => {
                if error
                    .evidence()
                    .http
                    .as_ref()
                    .is_some_and(|http| (200..300).contains(&http.status))
                {
                    if let Err(pool_error) =
                        driver.record_credential_success(credential.as_ref(), credential_clock()?)
                    {
                        return emit_turn_error(
                            ctx,
                            run,
                            &turn,
                            &handoff,
                            attempt,
                            pool_error.evidence(),
                        )
                        .await;
                    }
                }
                let pool_decision = match driver.credential_failure_decision(
                    credential.as_ref(),
                    &error,
                    credential_clock()?,
                ) {
                    Ok(decision) => decision,
                    Err(pool_error) => {
                        return emit_turn_error(
                            ctx,
                            run,
                            &turn,
                            &handoff,
                            attempt,
                            pool_error.evidence(),
                        )
                        .await;
                    }
                };
                if let Some(decision) = pool_decision {
                    match decision {
                        CredentialPoolDecision::Exhausted => {
                            let mut evidence = error.evidence().clone();
                            let status = evidence
                                .http
                                .as_ref()
                                .ok_or_else(|| {
                                    "credential pool exhausted without HTTP status evidence"
                                        .to_string()
                                })?
                                .status;
                            evidence.class = "failed_closed";
                            evidence.code = "credential_pool_exhausted".to_string();
                            evidence.message = format!(
                                "exact-interface credential pool exhausted after HTTP {status}"
                            );
                            return emit_turn_error(ctx, run, &turn, &handoff, attempt, &evidence)
                                .await;
                        }
                        CredentialPoolDecision::RetrySame(next)
                        | CredentialPoolDecision::Rotate(next)
                            if attempt < max_attempts =>
                        {
                            let backoff = driver.retry_backoff(attempt)?;
                            wait_for_retry(ctx, backoff).await?;
                            attempt += 1;
                            credential_candidate = Some(next);
                            continue;
                        }
                        CredentialPoolDecision::RetrySame(_)
                        | CredentialPoolDecision::Rotate(_) => {
                            return emit_turn_error(
                                ctx,
                                run,
                                &turn,
                                &handoff,
                                attempt,
                                error.evidence(),
                            )
                            .await;
                        }
                    }
                }
                if error.authenticated_refresh_required()
                    && authenticated_refresh_replay_allowed(
                        attempt,
                        max_attempts,
                        authenticated_refresh_replayed,
                    )
                {
                    authenticated_refresh_replayed = true;
                    if let Err(refresh_error) = driver.refresh_authentication().await {
                        return emit_turn_error(
                            ctx,
                            run,
                            &turn,
                            &handoff,
                            attempt,
                            refresh_error.evidence(),
                        )
                        .await;
                    }
                    attempt += 1;
                    credential_candidate = credential;
                    continue;
                }
                if error.retryable() && attempt < max_attempts {
                    let backoff = driver.retry_backoff(attempt)?;
                    wait_for_retry(ctx, backoff).await?;
                    attempt += 1;
                    credential_candidate = credential;
                    continue;
                }
                return emit_turn_error(ctx, run, &turn, &handoff, attempt, error.evidence()).await;
            }
        }
    }
}

async fn wait_for_retry(
    ctx: &InterfaceLiftContext,
    backoff: std::time::Duration,
) -> Result<(), String> {
    tokio::select! {
        _ = ctx.cancel.cancelled() => {
            Err("provider life cancelled before the movement reached an admitted outcome".to_string())
        }
        _ = tokio::time::sleep(backoff) => Ok(()),
    }
}

fn credential_clock() -> Result<u64, String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("provider credential clock is before Unix epoch: {error}"))?
        .as_millis();
    u64::try_from(millis).map_err(|_| "provider credential clock exceeds u64".to_string())
}

async fn emit_turn_error(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    attempts: u32,
    error: &ProviderErrorEvidence,
) -> Result<(), String> {
    emit_error_evidence(ctx, run, turn, handoff, attempts, error).await
}

async fn emit_error_message(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    message: &str,
) -> Result<(), String> {
    let evidence =
        ProviderErrorEvidence::new("failed_closed", "provider_turn_interrupted", message);
    emit_error_evidence(ctx, run, turn, handoff, 1, &evidence).await
}

async fn emit_error_evidence(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    turn: &QueuedTurn,
    handoff: &ProviderOutputHandoff,
    attempts: u32,
    evidence: &ProviderErrorEvidence,
) -> Result<(), String> {
    let artifact = materialize_provider_turn_error(
        &ctx.interface_var_dir().join("emitted"),
        evidence,
        attempts,
        turn.seq,
    )
    .map_err(|error| {
        format!(
            "provider terminal error for run '{}' movement {} failed to materialize: {error}",
            run.as_str(),
            turn.seq
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
        .ok_or_else(|| "provider invocation carries no formed interface policy".to_string())?;
    let value = values
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("provider formed interface policy carries no unsigned '{name}'"))?;
    u32::try_from(value).map_err(|_| format!("provider policy '{name}' exceeds u32"))
}

fn authenticated_refresh_replay_allowed(
    attempt: u32,
    max_attempts: u32,
    already_replayed: bool,
) -> bool {
    !already_replayed && attempt < max_attempts
}

fn load_own_descriptor(ctx: &InterfaceLiftContext) -> Result<InterfaceDescriptor, String> {
    ctx.resolved_interface_descriptor()
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
