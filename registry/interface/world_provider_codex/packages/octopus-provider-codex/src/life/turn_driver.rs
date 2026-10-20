use super::*;

pub(crate) fn drive_turn_with_retries<'a>(
    scope: CodexTurnScope<'a>,
    driver: &'a mut Option<Arc<CodexAppServerInterfaceDriver>>,
    turn: QueuedTurn,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(), String>> + 'a>> {
    Box::pin(drive_turn_with_retries_inner(scope, driver, turn))
}

pub(crate) async fn drive_turn_with_retries_inner(
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
    if let Err(error) = handoff_session.validate_run_placement(ctx, run, &pinned_run_record, turn.seq) {
        let message = format!("codex turn stopped before it began (movement seq {}): {error}", turn.seq);
        hand_failure_into_standing_route(ctx, run, turn.seq, &message).await
            .map_err(|unreported| format!("{message}; {unreported}"))?;
        control.finish();
        return Ok(());
    }
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
                            let mut outcomes = handoff.hand_progress_batch(vec![stream]).await?;
                            if outcomes.iter().all(|outcome| matches!(outcome, ProviderHandoffOutcome::Admitted { .. })) {
                                outcomes.extend(handoff.hand_batch(vec![terminal]).await?);
                            }
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
        let deferred_context_generations = handoff.deferred_context_generations().await;
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
                    deferred_context_generations,
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
