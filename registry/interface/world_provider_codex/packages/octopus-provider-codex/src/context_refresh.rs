use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContextRefresh {
    source_thread: String,
    source_shape: String,
    source_static: String,
    source_generation: u64,
    pending: CodexPendingContinuity,
    result_artifact: ArtifactRef,
    result_movement: u64,
    completed_output: serde_json::Value,
    target_shape: String,
    target_static: String,
    terminal: Option<NativeTerminalEvidence>,
    retired: bool,
    replacement_thread: Option<String>,
    started_turn: Option<String>,
}

impl ContextRefresh {
    pub(super) fn validate(
        &self,
        state: &CodexContinuityState,
    ) -> Result<(), InterfaceDriverError> {
        if self.source_thread.trim().is_empty()
            || self.source_thread.trim() != self.source_thread
            || [
                &self.source_shape,
                &self.source_static,
                &self.target_shape,
                &self.target_static,
            ]
            .iter()
            .any(|hash| !is_sha256_hex(hash))
            || self.source_shape == self.target_shape
            || self.result_movement == 0
            || self.source_generation >= state.context_generation
        {
            return Err(InterfaceDriverError::FailedClosed(
                "provider context refresh has invalid source or target coordinates".into(),
            ));
        }
        validate_pending_continuity_coordinates(Some(&self.pending), "context refresh source")?;
        if !interrupted_pending_has_external_result(&self.pending) {
            return Err(InterfaceDriverError::FailedClosed(
                "provider context refresh requires one completed external effect".into(),
            ));
        }
        if let Some(terminal) = &self.terminal {
            terminal.exact_turn(&self.source_thread, Some(pending_turn_id(&self.pending)))?;
        }
        if (self.retired && self.terminal.is_none())
            || (self.replacement_thread.is_some() && !self.retired)
            || self.started_turn.as_ref().is_some_and(|turn| {
                self.replacement_thread.is_none() || turn.trim().is_empty() || turn.trim() != turn
            })
        {
            return Err(InterfaceDriverError::FailedClosed(
                "provider context refresh has an unproven transition phase".into(),
            ));
        }
        let call_id = match &self.pending {
            CodexPendingContinuity::DynamicTool { call_id, .. }
            | CodexPendingContinuity::InterfaceOperation { call_id, .. } => {
                native_pending_operation(&self.pending)?
                    .map(|operation| operation.call_id)
                    .unwrap_or_else(|| call_id.clone())
            }
            _ => unreachable!("external pending checked above"),
        };
        if self
            .completed_output
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            != Some(call_id.as_str())
            || self
                .completed_output
                .get("type")
                .and_then(serde_json::Value::as_str)
                != Some("function_call_output")
        {
            return Err(InterfaceDriverError::FailedClosed(
                "context refresh output is detached from its exact completed call".into(),
            ));
        }
        serde_json::from_value::<octopus_protocol::models::ResponseItem>(
            self.completed_output.clone(),
        )
        .map_err(|error| {
            InterfaceDriverError::FailedClosed(format!(
                "context refresh output is malformed: {error}"
            ))
        })?;
        match &self.replacement_thread {
            None if state.thread_id == self.source_thread
                && state.thread_shape_fingerprint == self.source_shape
                && state.static_context_fingerprint == self.source_static
                && state.pending.is_none() =>
            {
                Ok(())
            }
            Some(thread)
                if self.terminal.is_some()
                    && thread != &self.source_thread
                    && thread == &state.thread_id
                    && state.thread_shape_fingerprint == self.target_shape
                    && state.static_context_fingerprint == self.target_static =>
            {
                Ok(())
            }
            _ => Err(InterfaceDriverError::FailedClosed(
                "provider context refresh is detached from its current private thread".into(),
            )),
        }
    }

    pub(super) fn connects(
        &self,
        source: &CodexContinuityState,
        target: &CodexContinuityState,
    ) -> bool {
        self.source_thread == source.thread_id
            && self.source_shape == source.thread_shape_fingerprint
            && self.source_static == source.static_context_fingerprint
            && self.source_generation >= source.context_generation
            && source.pending.as_ref() == Some(&self.pending)
            && self.validate(target).is_ok()
    }

    pub(super) fn settled_replacement(&self) -> bool {
        self.retired && self.replacement_thread.is_some() && self.started_turn.is_some()
    }

    pub(super) fn matches_thread(&self, thread: &str, shape: &str, static_context: &str) -> bool {
        self.replacement_thread.as_deref() == Some(thread)
            && self.target_shape == shape
            && self.target_static == static_context
    }
}

pub(super) struct CurrentNativeContext<'a> {
    pub model: &'a str,
    pub instructions: &'a str,
    pub tools: &'a [DynamicToolSpec],
    pub static_fingerprint: &'a str,
    pub conversation: Option<&'a str>,
    pub obligations: Option<&'a str>,
    pub session_available: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_summary: ReasoningSummary,
    pub recovered_dispatches: &'a [RecoveredTurnStartDispatch],
}

fn completed_effect(
    input: &ProviderInput,
    pending: &CodexPendingContinuity,
    namespace: &str,
    thread: &str,
    source_artifact: &ArtifactRef,
    projection: ModelResultProjection,
) -> Result<Option<(serde_json::Value, CodexTurnOutput)>, InterfaceDriverError> {
    let (call_id, name, ok, body, error, content) = match (input, pending) {
        (
            ProviderInput::ToolResult(result),
            CodexPendingContinuity::DynamicTool { call_id, tool, .. },
        ) => {
            validate_recovered_tool_result(result, pending)?;
            if let Some(operation) = native_pending_operation(pending)? {
                let receipt = serde_json::to_value(result)
                    .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
                match operation
                    .advance(&receipt)
                    .map_err(InterfaceDriverError::FailedClosed)?
                {
                    NativeStep::Complete { result } => (
                        operation.call_id,
                        operation.operation_name,
                        native_result_success(&result),
                        Some(result.clone()),
                        None,
                        native_result_content(&result, Vec::new())?,
                    ),
                    NativeStep::Effect { .. } | NativeStep::AmendmentEffect { .. } => return Ok(None),
                    NativeStep::Wait { .. } => {
                        return Err(InterfaceDriverError::FailedClosed(
                            "completed result cannot introduce a native wait".into(),
                        ));
                    }
                }
            } else {
                (
                    call_id.clone(),
                    tool.clone(),
                    result.ok,
                    result.body.clone(),
                    result.error.clone(),
                    dynamic_tool_response_content(result, source_artifact, projection)?,
                )
            }
        }
        (
            ProviderInput::InterfaceOperationResult(result),
            CodexPendingContinuity::InterfaceOperation {
                call_id,
                operation,
                output_label,
                ..
            },
        ) => {
            validate_recovered_interface_operation_result(
                &result.label,
                &result.value,
                call_id,
                operation,
                output_label,
            )?;
            (
                call_id.clone(),
                operation.clone(),
                result.ok,
                Some(result.value.clone()),
                None,
                interface_operation_response_content(result, source_artifact, projection)?,
            )
        }
        _ => {
            return Err(InterfaceDriverError::FailedClosed(
                "provider context refresh cannot replace its exact pending result".into(),
            ));
        }
    };
    let output = recovered_function_call_output(&call_id, recovered_result_output(content)?);
    let activity = CodexTurnOutput::dynamic_tool_completed(
        namespace.into(),
        name,
        call_id,
        thread.into(),
        pending_turn_id(pending).into(),
        ok,
        body,
        error,
    );
    Ok(Some((output, activity)))
}

impl CodexAppServerInterfaceDriver {
    pub(super) async fn refresh_native_context(
        &self,
        context: TurnContext<'_, '_>,
        input: &ProviderInput,
        recovered: Option<&CodexContinuityState>,
        generation: u64,
        current: CurrentNativeContext<'_>,
    ) -> Result<bool, InterfaceDriverError> {
        let Some(source) = recovered else {
            return Ok(false);
        };
        let retained = source.context_refresh.as_ref().filter(|refresh| {
            &refresh.result_artifact == context.source_artifact
                && refresh.result_movement == context.sink.source_movement_seq
        });
        let fingerprint = thread_shape_fingerprint(
            Some(current.model),
            current.instructions,
            current.tools,
            current.static_fingerprint,
        )?;
        if retained.is_none()
            && (source.thread_shape_fingerprint == fingerprint
                || source.pending.is_none()
                || source.interrupted_pending.is_some()
                || !matches!(
                    input,
                    ProviderInput::ToolResult(_) | ProviderInput::InterfaceOperationResult(_)
                ))
        {
            return Ok(false);
        }
        if !current.session_available
            || self.private_thread_cold_start(None)? == PrivateThreadColdStart::ResumeExact
        {
            return Err(InterfaceDriverError::FailedClosed("changed provider tools/instructions require a declared session snapshot and a provider context-reseed policy".into()));
        }
        let original = retained
            .map(|refresh| &refresh.pending)
            .or(source.pending.as_ref())
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "context refresh lost its completed effect".into(),
                )
            })?;
        let namespace = self.native_tool_namespace()?.name;
        let source_thread = retained
            .map(|refresh| refresh.source_thread.as_str())
            .unwrap_or(&source.thread_id);
        let Some((completed_output, activity)) =
            completed_effect(input, original, &namespace, source_thread, context.source_artifact, self.model_result_projection()?)?
        else {
            return Ok(false);
        };
        let key = match original {
            CodexPendingContinuity::DynamicTool { call_id, .. } => format!("tool:{call_id}"),
            CodexPendingContinuity::InterfaceOperation { call_id, .. } => format!("tool:{call_id}"),
            _ => unreachable!("completed_effect accepts external results only"),
        };
        let pending = self.pending_request(context.scope, &key).await?;
        let effect_guard = if let Some(pending) = &pending {
            let guard = pending.effect_gate.clone().lock_owned().await;
            self.ensure_pending_request_current(context.scope, pending)
                .await?;
            if pending
                .turn_interrupted
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Ok(false);
            }
            if retained.is_none() {
                self.validate_pending_continuity(context.scope, Some(source), pending, generation)
                    .await?;
            }
            Some(guard)
        } else {
            None
        };
        let handle = self.client.lock().await.request_handle();
        let mut refresh = if let Some(refresh) = retained {
            refresh.validate(source)?;
            if refresh.completed_output != completed_output
                || refresh.target_shape != fingerprint
                || refresh.target_static != current.static_fingerprint
            {
                return Err(InterfaceDriverError::FailedClosed(
                    "admitted provider context refresh changed its result or target contract"
                        .into(),
                ));
            }
            refresh.clone()
        } else {
            if source.context_generation != generation {
                return Err(InterfaceDriverError::FailedClosed(
                    "context refresh result has stale source generation".into(),
                ));
            }
            if read_exact_terminal_turn(
                &handle,
                &self.next_request_id,
                source_thread,
                pending_turn_id(original),
            )
            .await
            .map_err(InterfaceDriverError::FailedClosed)?
            .is_some()
            {
                return Ok(false);
            }
            let refresh = ContextRefresh {
                source_thread: source.thread_id.clone(),
                source_shape: source.thread_shape_fingerprint.clone(),
                source_static: source.static_context_fingerprint.clone(),
                source_generation: source.context_generation,
                pending: original.clone(),
                result_artifact: context.source_artifact.clone(),
                result_movement: context.sink.source_movement_seq,
                completed_output,
                target_shape: fingerprint.clone(),
                target_static: current.static_fingerprint.into(),
                terminal: None,
                retired: false,
                replacement_thread: None,
                started_turn: None,
            };
            let mut prepared = source.clone();
            prepared.pending = None;
            prepared.tool_operator_request = None;
            prepared.context_refresh = Some(refresh.clone());
            context.sink.set_continuity(prepared);
            context
                .sink
                .admit_context_boundary("prepared", false, None)
                .await?;
            refresh
        };
        context
            .control
            .publish(CodexTurnControlTarget::InterruptedDraining {
                output: Arc::downgrade(context.sink.emit),
                durable_handoff: context.handoff_barrier.clone(),
                thread_id: refresh.source_thread.clone(),
                turn_id: pending_turn_id(&refresh.pending).into(),
            });
        if !refresh.retired {
            if let Some((state, emissions)) = context.sink.recover_context_retirement(&refresh)? {
                context.sink.admit_context_emissions(emissions).await?;
                refresh = state
                    .context_refresh
                    .clone()
                    .expect("validated refresh marker");
                context.sink.set_continuity(state);
            }
        }
        if !refresh.retired {
            let turn_id = pending_turn_id(&refresh.pending);
            let mut terminal = read_exact_terminal_turn(
                &handle,
                &self.next_request_id,
                &refresh.source_thread,
                turn_id,
            )
            .await
            .map_err(InterfaceDriverError::FailedClosed)?;
            if terminal.is_none() {
                let interrupt = ClientRequest::TurnInterrupt {
                    request_id: self.mint_request_id(),
                    params: TurnInterruptParams {
                        thread_id: refresh.source_thread.clone(),
                        turn_id: turn_id.into(),
                    },
                };
                context
                    .sink
                    .admit_app_server_dispatch(&interrupt, None)
                    .await?;
                handle
                    .request_typed::<TurnInterruptResponse>(interrupt)
                    .await
                    .map_err(typed_error)?;
                let mut drain =
                    InterruptedTurnDrainState::new(refresh.source_thread.clone(), turn_id.into());
                let mut client = self.client.lock().await;
                loop {
                    if let InterruptedDrainDecision::Complete(evidence) =
                        drain.observe(client.next_event().await)
                    {
                        if evidence.disposition != "turn_completed"
                            || evidence.provider_error.is_some()
                        {
                            return Err(InterfaceDriverError::FailedClosed(format!(
                                "provider context retirement was not confirmed: {}",
                                evidence.disposition
                            )));
                        }
                        break;
                    }
                }
                drop(client);
                terminal = read_exact_terminal_turn(
                    &handle,
                    &self.next_request_id,
                    &refresh.source_thread,
                    turn_id,
                )
                .await
                .map_err(InterfaceDriverError::FailedClosed)?;
            }
            refresh.terminal = Some(terminal.ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "context refresh did not retire its original native turn".into(),
                )
            })?);
            let events = take_native_terminal_events(
                &mut *self.client.lock().await,
                refresh.terminal.as_ref().unwrap(),
                &refresh.source_thread,
                turn_id,
            )?;
            if events
                .iter()
                .any(|event| event.get("quarantined_server_request").is_some())
            {
                return Err(InterfaceDriverError::FailedClosed(
                    "retired provider emitted an unadmitted successor effect".into(),
                ));
            }
            if let Some(pending) = &pending {
                self.remove_pending_request(context.scope, pending).await?;
            }
            let mut state = context.sink.continuity_snapshot().ok_or_else(|| {
                InterfaceDriverError::FailedClosed("refresh lost continuity".into())
            })?;
            state.context_refresh = Some(refresh.clone());
            context.sink.set_continuity(state);
            context
                .sink
                .bind_live_stream(&refresh.source_thread, turn_id)?;
            context
                .sink
                .admit_context_boundary("retired", true, Some(activity))
                .await?;
            refresh = context
                .sink
                .continuity_snapshot()
                .and_then(|state| state.context_refresh)
                .ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(
                        "retirement lost its admitted transition".into(),
                    )
                })?;
        }
        if let Some(pending) = &pending {
            let retained_request = self
                .pending_server_requests
                .lock()
                .await
                .contains_key(&(context.scope.clone(), pending.key.clone()));
            if retained_request {
                self.remove_pending_request(context.scope, pending).await?;
            }
        }
        drop(effect_guard);
        if !current.recovered_dispatches.is_empty() {
            return Err(InterfaceDriverError::FailedClosed("provider context refresh already dispatched a successor turn; an uncertain successor is never replayed".into()));
        }
        let thread_id = if let Some(thread) = &refresh.replacement_thread {
            let response: ThreadResumeResponse = handle
                .request_typed(ClientRequest::ThreadResume {
                    request_id: self.mint_request_id(),
                    params: ThreadResumeParams {
                        thread_id: thread.clone(),
                        exclude_turns: true,
                        model: Some(current.model.into()),
                        service_tier: self.bound_service_tier()?,
                        config: Some(self.native_tool_thread_config()?),
                        developer_instructions: Some(current.instructions.into()),
                        sandbox: Some(self.native_thread_sandbox()?),
                        ..ThreadResumeParams::default()
                    },
                })
                .await
                .map_err(typed_error)?;
            if response.thread.id != *thread {
                return Err(InterfaceDriverError::FailedClosed(
                    "context refresh resumed a different replacement thread".into(),
                ));
            }
            validate_native_service_tier_response(
                self.bound_service_tier()?
                    .as_ref()
                    .and_then(Option::as_deref),
                response.service_tier.as_deref(),
            )?;
            self.transport_state.lock().await.insert(
                context.scope.clone(),
                TransportState {
                    context_generation: context
                        .sink
                        .continuity_snapshot()
                        .unwrap()
                        .context_generation,
                    hooks_catalog_emitted: true,
                    thread_id: Some(thread.clone()),
                    thread_shape_fingerprint: Some(fingerprint.clone()),
                    static_context_fingerprint: Some(current.static_fingerprint.into()),
                    rollout_path: response.thread.path,
                },
            );
            thread.clone()
        } else {
            let previous = context.sink.continuity_snapshot().ok_or_else(|| {
                InterfaceDriverError::FailedClosed("refresh lost retired continuity".into())
            })?;
            let (thread, _, _) = self
                .acquire_transport_thread(
                    context.scope,
                    ThreadAcquisition {
                        recovered: Some(&previous),
                        require_exact_resume: false,
                        context_generation: previous.context_generation.checked_add(1).ok_or_else(
                            || {
                                InterfaceDriverError::FailedClosed(
                                    "context refresh generation overflowed".into(),
                                )
                            },
                        )?,
                        model: Some(current.model.into()),
                        bound_instructions: current.instructions,
                        native_affordances: current.tools,
                        static_context_fingerprint: current.static_fingerprint,
                        session_context_available: true,
                    },
                )
                .await?;
            refresh.replacement_thread = Some(thread.clone());
            let mut state = previous;
            state.thread_id = thread.clone();
            state.thread_shape_fingerprint = fingerprint;
            state.static_context_fingerprint = current.static_fingerprint.into();
            state.context_refresh = Some(refresh.clone());
            context.sink.set_continuity(state);
            context
                .sink
                .admit_context_boundary("ready", false, None)
                .await?;
            thread
        };
        let mut additional = native_turn_context(
            current.conversation.map(str::to_owned),
            current.obligations.map(str::to_owned),
        );
        additional.insert("completed_effect".into(), AdditionalContextEntry { kind: AdditionalContextKind::Untrusted, value: serde_json::to_string(&serde_json::json!({
            "source_artifact": refresh.result_artifact, "source_movement_seq": refresh.result_movement,
            "source_thread": refresh.source_thread, "source_turn": pending_turn_id(&refresh.pending),
            "result": refresh.completed_output,
        })).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))? });
        let request = ClientRequest::TurnStart {
            request_id: self.mint_request_id(),
            params: fresh_turn_start_params(
                thread_id.clone(),
                Vec::new(),
                current.model.into(),
                current.reasoning_effort,
                self.bound_service_tier()?,
                current.reasoning_summary,
                additional,
                "provider_context_refresh",
            ),
        };
        context
            .sink
            .admit_app_server_dispatch(&request, None)
            .await?;
        let response: octopus_appserver_proto::TurnStartResponse =
            handle.request_typed(request).await.map_err(typed_error)?;
        if response.turn.id.trim().is_empty() || response.turn.status != TurnStatus::InProgress {
            return Err(InterfaceDriverError::FailedClosed(
                "context refresh returned no active successor turn".into(),
            ));
        }
        let turn_id = response.turn.id;
        let mut started = context.sink.continuity_snapshot().ok_or_else(|| {
            InterfaceDriverError::FailedClosed("started refresh lost continuity".into())
        })?;
        started
            .context_refresh
            .as_mut()
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed("started refresh lost transition".into())
            })?
            .started_turn = Some(turn_id.clone());
        context.sink.set_continuity(started);
        context.control.publish(CodexTurnControlTarget::Draining {
            handle,
            request_ids: self.next_request_id.clone(),
            pending_requests: self.pending_server_requests.clone(),
            scope: context.scope.clone(),
            output: Arc::downgrade(context.sink.emit),
            continuity: context.sink.continuity_snapshot(),
            handoff_barrier: context.handoff_barrier.clone(),
            effect_gate: context.effect_gate.clone(),
            turn_interrupted: context.turn_interrupted.clone(),
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
        });
        self.drain_turn(context, &thread_id, &turn_id).await?;
        Ok(true)
    }
}

impl TurnSink<'_> {
    pub(super) fn recover_context_retirement(
        &self,
        expected: &ContextRefresh,
    ) -> Result<Option<(CodexContinuityState, Vec<ProviderEmission>)>, InterfaceDriverError> {
        let identity = LiveTurnIdentity {
            provider_interface: self.provider_interface.as_str().into(),
            run: self.run.into(),
            thread_id: expected.source_thread.clone(),
            turn_id: pending_turn_id(&expected.pending).into(),
        };
        let Some((stream, payload_hash)) = crate::live_stream::recover_sealed_terminal_stream(
            self.live_stream_root,
            self.live_stream_rules,
            &identity,
            self.source_movement_seq,
            INTERFACE_CONTEXT_UPDATED_LABEL,
        )
        .map_err(InterfaceDriverError::FailedClosed)?
        else {
            return Ok(None);
        };
        let root = self
            .output_root
            .canonicalize()
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
        let slug = INTERFACE_CONTEXT_UPDATED_LABEL.replace('.', "_");
        let manifest = InterfaceArtifactManifest {
            label: INTERFACE_CONTEXT_UPDATED_LABEL.into(),
            payload_path: root.join(format!("{slug}-{payload_hash}.payload")),
            payload_content_hash: Some(payload_hash),
        };
        let bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        let hash = octopus_ledger::ContentHash::of_bytes(&bytes).to_hex();
        let path = root.join(format!("{slug}-{hash}.artifact.json"));
        if std::fs::read(&path)
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?
            != bytes
        {
            return Err(InterfaceDriverError::FailedClosed(
                "sealed context retirement changed its exact manifest".into(),
            ));
        }
        let boundary = InterfaceArtifactFile::new(path)
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
        let payload: serde_json::Value = serde_json::from_slice(
            &boundary
                .read_payload_bytes()
                .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?,
        )
        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        let state = parse_codex_continuity(
            &payload[CODEX_CONTINUITY_FIELD],
            self.provider_interface,
            self.run,
        )?;
        let refresh = state.context_refresh.as_ref().ok_or_else(|| {
            InterfaceDriverError::FailedClosed("sealed retirement has no exact transition".into())
        })?;
        let mut comparable = refresh.clone();
        comparable.terminal = expected.terminal.clone();
        comparable.retired = expected.retired;
        if payload["context_refresh_phase"] != "retired"
            || payload["source_movement_seq"] != self.source_movement_seq
            || !refresh.retired
            || refresh.replacement_thread.is_some()
            || &comparable != expected
            || self
                .continuity_snapshot()
                .is_some_and(|current| state.context_generation < current.context_generation)
        {
            return Err(InterfaceDriverError::FailedClosed(
                "sealed context retirement crossed its exact source, result or target".into(),
            ));
        }
        Ok(Some((
            state,
            vec![
                ProviderEmission {
                    artifact: stream,
                    route: ProviderEmissionRoute::ClientProjection,
                },
                ProviderEmission {
                    artifact: boundary,
                    route: ProviderEmissionRoute::ClientProjection,
                },
            ],
        )))
    }

    async fn admit_context_boundary(
        &self,
        phase: &str,
        seal: bool,
        activity: Option<CodexTurnOutput>,
    ) -> Result<(), InterfaceDriverError> {
        if !self
            .context_output_labels
            .contains(INTERFACE_CONTEXT_UPDATED_LABEL)
            || !self
                .client_projection_output_labels
                .contains(INTERFACE_CONTEXT_UPDATED_LABEL)
        {
            return Err(InterfaceDriverError::FailedClosed(
                "provider context refresh requires its declared context output binding".into(),
            ));
        }
        let mut emissions = Vec::new();
        if let Some(mut activity) = activity {
            self.normalize_output_label(&mut activity)?;
            if self.live_stream_rules.disposition(
                &activity.label,
                &activity.value,
                self.has_live_stream(),
            )? == LiveStreamDisposition::Stream
            {
                self.send(activity)?;
            } else {
                emissions.push(self.materialize_normalized(activity, true)?);
            }
        }
        if seal {
            let mut state = self.continuity_snapshot().ok_or_else(|| {
                InterfaceDriverError::FailedClosed("retirement lost continuity".into())
            })?;
            state
                .context_refresh
                .as_mut()
                .ok_or_else(|| {
                    InterfaceDriverError::FailedClosed("retirement lost transition".into())
                })?
                .retired = true;
            self.set_continuity(state);
        }
        if seal && self.has_live_stream() {
            self.advance_continuity(false)?;
        }
        let mut output = CodexTurnOutput::client(
            INTERFACE_CONTEXT_UPDATED_LABEL,
            serde_json::json!({"context_refresh_phase": phase, "source_movement_seq": self.source_movement_seq}),
        );
        self.normalize_output_label(&mut output)?;
        let boundary = self.materialize_normalized(output, false)?;
        if seal {
            let hash = artifact_payload_blake3(&boundary.artifact)
                .map_err(InterfaceDriverError::FailedClosed)?;
            if let Some(stream) = self.seal_live_stream(INTERFACE_CONTEXT_UPDATED_LABEL, &hash)? {
                emissions.push(stream);
            }
        }
        emissions.push(boundary);
        self.admit_context_emissions(emissions).await?;
        if seal {
            *self.live_stream.lock().expect("live stream lock") = None;
        }
        Ok(())
    }

    async fn admit_context_emissions(
        &self,
        emissions: Vec<ProviderEmission>,
    ) -> Result<(), InterfaceDriverError> {
        let (admitted, receipt) = tokio::sync::oneshot::channel();
        self.emit
            .sender
            .send(ProviderStreamEmission::ContextBoundary {
                emissions,
                admitted,
            })
            .map_err(|_| {
                InterfaceDriverError::Transport("context refresh output stream closed".into())
            })?;
        receipt
            .await
            .map_err(|_| {
                InterfaceDriverError::Transport("context refresh admission receipt lost".into())
            })?
            .map_err(InterfaceDriverError::FailedClosed)?;
        Ok(())
    }
}

pub(super) struct ResolvedModelContext {
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_summary: ReasoningSummary,
    pub static_context_fingerprint: String,
    pub bound_instructions: String,
    pub conversation_block: Option<String>,
    pub obligations_grant_block: Option<String>,
}

impl CodexAppServerInterfaceDriver {
    pub(super) async fn resolve_model_context(
        &self,
        input: &InterfaceInvocationEnvelope,
        assembled_context: &AssembledContext,
    ) -> Result<ResolvedModelContext, InterfaceDriverError> {
        let runtime = invocation_runtime_metadata(input)?;
        let model = self
            .resolve_private_config_string("model")?
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(format!(
                    "no model bound to interface '{}'; its interface_driver_config.private_config \
                     carries no model, and the app-server may not pick a hidden default — bind the run \
                     to an interface descriptor whose folder config declares a model (constitution §3, \
                     fail-closed)",
                    runtime.interface.as_str()
                ))
            })?;
        let formed_models = required_formed_models(input)?;
        if !formed_models.contains(&model) {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex interface '{}' selected model '{}' outside its freshly formed interface policy models [{}]",
                runtime.interface.as_str(),
                model,
                formed_models.iter().cloned().collect::<Vec<_>>().join(", ")
            )));
        }
        let reasoning_effort = parse_reasoning_effort(
            self.resolve_private_config_string("reasoning_effort")?
                .as_deref(),
        )?;
        self.validate_native_service_tier(&model).await?;
        let reasoning_summary = parse_reasoning_summary(
            self.resolve_private_config_string("reasoning_summary")?
                .as_deref(),
        )?;

        let static_context_fingerprint = sha256_hex(&assembled_context.static_context_bytes);
        let assembled_role =
            (!assembled_context.role.is_empty()).then(|| assembled_context.role.join("\n\n"));
        let bound_instructions = resolve_bound_instructions(assembled_role.as_deref())?;
        let conversation_block = (!assembled_context.conversation.is_empty())
            .then(|| assembled_context.conversation.join("\n\n"));

        let obligations_grant_block = input.metadata.as_ref().and_then(render_model_context_block);

        Ok(ResolvedModelContext {
            model,
            reasoning_effort,
            reasoning_summary,
            static_context_fingerprint,
            bound_instructions,
            conversation_block,
            obligations_grant_block,
        })
    }
}
