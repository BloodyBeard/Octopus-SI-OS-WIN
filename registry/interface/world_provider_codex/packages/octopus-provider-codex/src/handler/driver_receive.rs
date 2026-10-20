use super::*;

impl CodexAppServerInterfaceDriver {
    pub(crate) fn resolve_bound_session_contexts(
        artifact_binding_payloads: &[octopus_interfaces::ArtifactBindingPayload],
        provider_interface: &InterfaceId,
        session_snapshot_labels: &BTreeSet<String>,
        resolved_session_snapshot: Option<&ProviderInvocationSessionSnapshot>,
    ) -> Result<BoundSessionContexts, InterfaceDriverError> {
        let mut resolved = BTreeMap::new();
        let mut current_seen = false;
        let mut grounded_snapshot_used = false;
        for bound in artifact_binding_payloads {
            let bytes = bound.read_bytes()
                .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
            let value: serde_json::Value =
                serde_json::from_slice(bytes.as_ref()).map_err(|error| {
                    InterfaceDriverError::Protocol(format!(
                        "Codex session carrier '{}' is not JSON: {error}",
                        bound.artifact.id
                    ))
                })?;
            let Some(raw) = value.get(SESSION_CONTEXT_FIELD) else {
                continue;
            };
            let label = continuity_carrier_label(&value, bound.artifact.id.as_str())?;
            if !session_snapshot_labels.contains(label) {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "Codex session carrier '{}' is not a descriptor-declared snapshot output",
                    bound.artifact.id
                )));
            }
            let snapshot: SessionContextSnapshot =
                serde_json::from_value(raw.clone()).map_err(|error| {
                    InterfaceDriverError::Protocol(format!(
                        "Codex session carrier '{}' is malformed: {error}",
                        bound.artifact.id
                    ))
                })?;
            if snapshot.schema_version != 3 {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "Codex session carrier '{}' has unsupported schema_version {}",
                    bound.artifact.id, snapshot.schema_version
                )));
            }
            let key = continuity_source_key(&bound.artifact);
            if snapshot.provider_interface != *provider_interface {
                if resolved.insert(key, BoundSessionContext::Foreign).is_some() {
                    return Err(InterfaceDriverError::FailedClosed(
                        "Codex invocation repeats one foreign session carrier".to_string(),
                    ));
                }
                continue;
            }
            if current_seen {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex invocation carries multiple current session snapshots".to_string(),
                ));
            }
            current_seen = true;
            let snapshot = resolved_session_snapshot.ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "Codex current session carrier has no handoff-grounded ledger view".to_string(),
                )
            })?;
            if snapshot.carrier != bound.artifact {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex handoff-grounded session carrier differs from the invocation binding"
                        .to_string(),
                ));
            }
            grounded_snapshot_used = true;
            if resolved
                .insert(
                    key,
                    BoundSessionContext::Current(snapshot.artifacts.clone()),
                )
                .is_some()
            {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex invocation repeats one current session carrier".to_string(),
                ));
            }
        }
        if resolved_session_snapshot.is_some() && !grounded_snapshot_used {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex handoff grounded a session carrier absent from the invocation bindings"
                    .to_string(),
            ));
        }
        Ok(resolved)
    }

    pub(crate) fn receive_artifact_and_stream_outputs<'a>(
        &'a self,
        invocation: CodexProviderInvocation<'a>,
    ) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<(), InterfaceDriverError>> + 'a>> {
        Box::pin(self.receive_artifact_and_stream_outputs_inner(invocation))
    }

    pub(crate) async fn receive_artifact_and_stream_outputs_inner(
        &self,
        invocation: CodexProviderInvocation<'_>,
    ) -> Result<(), InterfaceDriverError> {
        let CodexProviderInvocation {
            callable_tool_names,
            input,
            emit,
            resolved_session_snapshot,
            current_session_context,
            output_root,
            live_stream_root,
            source_movement_seq,
            attempt,
            admitted_run_record,
            current_run_record,
            deferred_context_generations,
            input_changes_context,
            interrupted_after_artifact,
            recovered_handoff_artifacts,
            durable_interrupted_continuity,
            reconciled_result_continuity,
            control,
            handoff_barrier,
        } = invocation;
        let request_bytes = input.read_artifact_payload()?;

        let runtime = invocation_runtime_metadata(&input)?;
        if interrupted_after_artifact.is_none() && !recovered_handoff_artifacts.is_empty() {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex invocation carries recovered handoff artifacts without an interrupted source frontier"
                    .to_string(),
            ));
        }
        let recovered_turn_start_dispatches = recovered_turn_start_dispatches(
            &recovered_handoff_artifacts,
            &runtime.interface,
            source_movement_seq,
        )?;
        let run_record = parse_run_record_json(current_run_record, runtime.run.as_str())?;
        let granted_interfaces = input
            .allowed_interfaces()
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?
            .iter()
            .map(|interface| InterfaceId::new(interface.clone()))
            .collect::<BTreeSet<_>>();
        let grant = input
            .grant()
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        let granted_tools = grant.tools.iter().cloned().collect::<BTreeSet<_>>();
        let granted_marks = grant.mark_outcomes.iter().cloned().collect::<BTreeSet<_>>();
        let scope = TransportStateScope::from_run(&runtime.run);
        let context_output_labels = self.context_output_labels()?;
        let session_snapshot_labels = self.session_snapshot_labels()?;
        let client_projection_output_labels =
            self.client_projection_output_labels(&context_output_labels, &session_snapshot_labels)?;
        let live_stream_rules =
            LiveStreamRules::from_private_config_or_disabled(&self.interface_private_config)?;
        live_stream_rules.validate_projection_labels(
            &context_output_labels,
            &session_snapshot_labels,
            &client_projection_output_labels,
        )?;
        let sink = TurnSink {
            emit: &emit,
            output_root,
            live_stream_root,
            provider_interface: &runtime.interface,
            run: runtime.run.as_str(),
            source_movement_seq,
            attempt,
            context_output_labels: &context_output_labels,
            client_projection_output_labels: &client_projection_output_labels,
            live_stream_rules: &live_stream_rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: emit.continuity.clone(),
        };
        sink.continuity.lock().expect("continuity lock").pending_input_context = input_changes_context;
        let manifest = input.read_artifact_manifest()?;
        let peer_message = admitted_peer_message(&input, self.interface_private_config.get("input_intake"))?;
        let lifecycle_inbound = decode_lifecycle_inbound(&manifest.label, &request_bytes)
            .map_err(InterfaceDriverError::Protocol)?;
        let thread_compact_requested = peer_message.is_none() && self.configured_thread_compact_request(&manifest.label)?;
        if !thread_compact_requested
            && peer_message.is_none()
            && let Some(operation) = self.configured_provider_operation(&manifest.label)?
        {
            return self
                .handle_provider_operation(operation, &runtime.interface, &request_bytes, &sink)
                .await;
        }
        if thread_compact_requested
            && serde_json::from_slice::<EmptyProviderOperationRequest>(&request_bytes).is_err()
        {
            return sink.send(provider_operation_error(
                PROVIDER_THREAD_COMPACT_START_LABEL,
                "operation_payload_malformed",
                "protocol",
                false,
                "thread compact payload must be an empty object",
            ));
        }
        if peer_message.is_none() && is_undeclared_provider_operation(&manifest.label, thread_compact_requested) {
            return sink.send(provider_operation_error(
                &manifest.label,
                "operation_not_declared",
                "unsupported",
                false,
                "the exact provider interface descriptor does not declare this operation artifact",
            ));
        }
        let admitted_record = octopus_runtime::run_record::RunRecord::from_bytes(admitted_run_record)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        let (context_payloads, payload_selection) = admitted_payload_context(&admitted_record, &input)?;
        let complete_context_payloads = context_payloads;
        let instructions = resolve_instruction_context(input.metadata()?, &complete_context_payloads)
            .map_err(InterfaceDriverError::FailedClosed)?;
        let context_payloads = &instructions.remaining_payloads;
        let bound_session_contexts = Self::resolve_bound_session_contexts(
            &context_payloads,
            &runtime.interface,
            &session_snapshot_labels,
            resolved_session_snapshot.as_ref(),
        )?;
        let lifecycle_payloads = lifecycle_context_payloads(
            &context_payloads,
            resolved_session_snapshot.as_ref(),
        )?;
        let lifecycle = LifecycleBinding::resolve(
            &lifecycle_payloads,
            &run_record,
            runtime,
            &granted_interfaces,
            &granted_marks,
        )
        .map_err(InterfaceDriverError::FailedClosed)?;
        if lifecycle_inbound.is_some() && lifecycle.is_none() {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "run '{}' received lifecycle artifact '{}' without an exact connected lifecycle.connection",
                runtime.run.as_str(),
                manifest.label
            )));
        }
        let recovered = recover_codex_continuity_resolved(
            &context_payloads,
            &bound_session_contexts,
            &runtime.interface,
            runtime.run.as_str(),
            &context_output_labels,
            &session_snapshot_labels,
        )?;
        let bound_continuity = recovered.clone();
        let recovered = reconcile_current_session_continuity(
            recovered,
            recover_codex_continuity_from_current_session(
                &current_session_context,
                &input.artifact,
                &runtime.interface,
                runtime.run.as_str(),
                &context_output_labels,
            )?,
        )?;
        let recovered = reconcile_delivered_interrupted_continuity(
            bound_continuity.as_ref(), recovered, reconciled_result_continuity,
        )?;
        let (recovered, durable_interrupt_override) =
            reconcile_durable_interrupted_continuity(recovered, durable_interrupted_continuity)?;
        if manifest.label == PROVIDER_MODELS_LIST_LABEL {
            if self.has_pending_request(&scope).await
                || recovered.as_ref().is_some_and(|state| {
                    state.pending.is_some() || state.interrupted_pending.is_some()
                })
            {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex model catalog request cannot replace an unresolved app-server request"
                        .to_string(),
                ));
            }
            return self
                .emit_model_catalog(&request_bytes, &sink, &required_formed_models(&input)?)
                .await;
        }
        if let Some(state) = recovered.clone() {
            sink.set_continuity(state);
        }
        self.enforce_native_web_search_policy(&input)?;
        self.native_mcp_resource_discovery()?;
        let hidden_tools = model_hidden_tool_names(&self.interface_private_config)
            .map_err(InterfaceDriverError::Protocol)?;
        let hidden_tools = model_tool_exclusions(input.metadata()?, &callable_tool_names, &hidden_tools);
        let native_namespace = self.native_tool_namespace()?;
        let run_affordances = RunAffordances::from_invocation(&input, &serde_json::Value::Object(self.interface_private_config.clone()))
            .map_err(InterfaceDriverError::FailedClosed)?;
        let native_catalog =
            extract_native_affordance_catalog(&input, &hidden_tools, &native_namespace, run_affordances.as_ref(), &self.interface_private_config)?;
        let source_interface = if native_catalog.interface_operations.is_empty() {
            None
        } else {
            Some(
                input
                    .metadata()?
                    .checkpoint_binding_policy
                    .as_ref()
                    .ok_or_else(|| {
                        InterfaceDriverError::FailedClosed(
                            "provider input has no admitted checkpoint binding snapshot"
                                .to_string(),
                        )
                    })?
                    .inbound
                    .interface
                    .clone(),
            )
        };
        let mut effective_request_bytes = request_bytes.clone();
        let mut effective_manifest_label = manifest.label.clone();
        let mut effective_source_artifact = input.artifact.clone();
        let mut resumed_start_turn = None;
        let mut resumed_additional_context = None;
        let mut lifecycle_resumed_provider_input = false;
        let mut lifecycle_resume_boundary = false;
        let mut lifecycle_prompt_event = None;
        if let Some(inbound) = lifecycle_inbound {
            let binding = lifecycle
                .as_ref()
                .expect("lifecycle inbound has a connection");
            match inbound {
                LifecycleInbound::Event(event) => {
                    restore_lifecycle_provider_continuity(&sink, runtime, &event)?;
                    let dispatch = binding
                        .dispatch_handler(
                            runtime.run.as_str(),
                            &event,
                            &granted_tools,
                            &granted_interfaces,
                        )
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    if let Some(dispatch) = dispatch {
                        sink.send_lifecycle(LifecyclePlannedOutput {
                            label: dispatch.label,
                            value: dispatch.value,
                            route: dispatch.route,
                        })?;
                    }
                    return Ok(());
                }
                LifecycleInbound::Decision(decision) => {
                    let event = binding
                        .validate_bound_event(&decision.event_artifact)
                        .map(|_| decision.event_artifact.clone())
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    restore_lifecycle_provider_continuity(&sink, runtime, &event)?;
                    let continuation = binding
                        .validate_decision(runtime.run.as_str(), &decision, input.metadata()?)
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    match continuation {
                        LifecycleContinuation::HeldEmission { .. } => {
                            sink.send_held_lifecycle(&continuation)?;
                            return Ok(());
                        }
                        LifecycleContinuation::HeldEmissionAndRecoverBoundary {
                            ref source_artifact,
                            ..
                        } => {
                            sink.send_held_lifecycle(&continuation)?;
                            lifecycle_resume_boundary = true;
                            effective_source_artifact = source_artifact.clone();
                        }
                        LifecycleContinuation::StartTurn {
                            turn_input,
                            additional_context,
                            source_artifact,
                        } => {
                            resumed_start_turn = Some(
                                serde_json::from_value::<Vec<UserInput>>(turn_input).map_err(
                                    |error| {
                                        InterfaceDriverError::Protocol(format!(
                                            "lifecycle start-turn continuation is malformed: {error}"
                                        ))
                                    },
                                )?,
                            );
                            resumed_additional_context = Some(additional_context);
                            effective_source_artifact = source_artifact;
                        }
                        LifecycleContinuation::ProviderInput {
                            manifest_label,
                            payload,
                            source_artifact,
                        } => {
                            lifecycle_resumed_provider_input = true;
                            effective_request_bytes = serde_json::to_vec(&payload).map_err(
                                |error| {
                                    InterfaceDriverError::Protocol(format!(
                                        "lifecycle provider-input continuation did not encode: {error}"
                                    ))
                                },
                            )?;
                            effective_manifest_label = manifest_label;
                            effective_source_artifact = source_artifact;
                        }
                        LifecycleContinuation::RecoverBoundary { source_artifact } => {
                            lifecycle_resume_boundary = true;
                            effective_source_artifact = source_artifact;
                        }
                    }
                }
                LifecycleInbound::HandlerPrompt(value) => {
                    let event: LifecycleEventArtifact =
                        serde_json::from_value(value.get("event").cloned().ok_or_else(|| {
                            InterfaceDriverError::Protocol(
                                "lifecycle handler prompt has no event".to_string(),
                            )
                        })?)
                        .map_err(|error| {
                            InterfaceDriverError::Protocol(format!(
                                "lifecycle handler prompt event is malformed: {error}"
                            ))
                        })?;
                    binding
                        .validate_bound_event(&event)
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    binding
                        .validate_handler_invocation(&event, &value, LIFECYCLE_HANDLER_PROMPT_LABEL)
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    restore_lifecycle_provider_continuity(&sink, runtime, &event)?;
                    let prompt = value
                        .get("prompt")
                        .and_then(serde_json::Value::as_str)
                        .filter(|prompt| !prompt.trim().is_empty() && prompt.trim() == *prompt)
                        .ok_or_else(|| {
                            InterfaceDriverError::Protocol(
                                "lifecycle handler prompt has no non-empty prompt".to_string(),
                            )
                        })?;
                    resumed_start_turn = Some(vec![UserInput::Text {
                        text: lifecycle_handler_model_prompt(prompt, &event)?,
                        text_elements: Vec::new(),
                    }]);
                    effective_source_artifact = event.subject_artifact.clone();
                    lifecycle_prompt_event = Some(event);
                }
                LifecycleInbound::HandlerTask(value) => {
                    let event: LifecycleEventArtifact =
                        serde_json::from_value(value.get("event").cloned().ok_or_else(|| {
                            InterfaceDriverError::Protocol(
                                "lifecycle handler task has no event".to_string(),
                            )
                        })?)
                        .map_err(|error| {
                            InterfaceDriverError::Protocol(format!(
                                "lifecycle handler task event is malformed: {error}"
                            ))
                        })?;
                    binding
                        .validate_bound_event(&event)
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    binding
                        .validate_handler_invocation(&event, &value, LIFECYCLE_HANDLER_TASK_LABEL)
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    restore_lifecycle_provider_continuity(&sink, runtime, &event)?;
                    let task = value
                        .get("task")
                        .and_then(serde_json::Value::as_str)
                        .filter(|task| !task.trim().is_empty() && task.trim() == *task)
                        .ok_or_else(|| {
                            InterfaceDriverError::Protocol(
                                "lifecycle handler task has no non-empty task".to_string(),
                            )
                        })?;
                    resumed_start_turn = Some(vec![UserInput::Text {
                        text: lifecycle_handler_model_prompt(task, &event)?,
                        text_elements: Vec::new(),
                    }]);
                    effective_source_artifact = event.subject_artifact.clone();
                    lifecycle_prompt_event = Some(event);
                }
                LifecycleInbound::HandlerOutput(value) => {
                    binding
                        .validate_handler_output(&value)
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    return Ok(());
                }
            }
        }
        let lifecycle_start_resumed = resumed_start_turn.is_some();
        let selected_files = selected_file_projection(
            &admitted_record,
            &input,
            &effective_source_artifact,
            &effective_manifest_label,
            &effective_request_bytes,
            &bound_session_contexts,
            &current_session_context,
            self.model_result_projection()?,
        )?;
        let mut provider_input = match resumed_start_turn {
            Some(turn_input) => ProviderInput::Turn(turn_input),
            None if lifecycle_resume_boundary => ProviderInput::Turn(Vec::new()),
            None if thread_compact_requested => ProviderInput::Turn(Vec::new()),
            None if peer_message.is_some() => {
                let peer = peer_message.as_ref().expect("peer message selected");
                if peer.action == PeerAction::Interrupt {
                    return Err(InterfaceDriverError::FailedClosed("peer interrupt must enter the admitted turn-control reader".into()));
                }
                ProviderInput::Turn(vec![UserInput::Text { text: peer_input_text(peer, &input.artifact)?, text_elements: Vec::new() }])
            }
            None => decode_provider_input(
                &effective_request_bytes,
                &effective_manifest_label,
                source_interface.as_ref(),
                &native_catalog.interface_operations,
                Some(&selected_files),
            )?,
        };
        let mut assembled_context = assemble_current_context(
            &context_payloads,
            &bound_session_contexts,
            &runtime.interface,
            &session_snapshot_labels,
            &current_session_context,
            &effective_source_artifact,
            self.model_result_projection()?,
        )?;
        let recovered_result_context = match (&provider_input, recovered.as_ref()) {
            (ProviderInput::Turn(_) | ProviderInput::Resume { .. }, Some(state)) if !codex_continuity_waits_for_result(state) => {
                resumed_model_context(&current_session_context, &effective_source_artifact, state, self.model_result_projection()?)?
            }
            _ => None,
        };
        let instruction_artifacts = instruction_artifact_selection(&self.interface_private_config)?;
        let instruction_text = instructions.render(&instruction_artifacts)
            .map_err(InterfaceDriverError::FailedClosed)?;
        assembled_context.role = instruction_text;
        assembled_context.static_context_bytes.clear();
        for bound in &complete_context_payloads {
            if instructions.source_artifacts.contains(&bound.artifact) {
                append_static_context_source(&mut assembled_context.static_context_bytes, bound)?;
            }
        }
        assembled_context.static_context_bytes.extend_from_slice(
            &serde_json::to_vec(&native_catalog.tool_names).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?,
        );
        let authored_context_generation = read_codex_context_generation(&run_record, &runtime.interface)?
            .checked_add(deferred_context_generations)
            .ok_or_else(|| InterfaceDriverError::FailedClosed("Codex authored context generation overflowed".to_string()))?;
        let context_generation = if durable_interrupt_override {
            recovered
                .as_ref()
                .map(|state| state.context_generation)
                .unwrap_or(authored_context_generation)
        } else {
            authored_context_generation
        };
        let invocation_context_generation = if input_changes_context {
            context_generation.checked_add(1).ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "Codex invocation context generation overflowed".to_string(),
                )
            })?
        } else {
            context_generation
        };
        if let ProviderInput::ToolResult(result) = &provider_input
            && let Some(request) = ToolOperatorRequest::from_result(result)?
        {
            let mut state = recovered.clone().ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "application request has no durable native DynamicTool".to_string(),
                )
            })?;
            request.validate_pending(&state)?;
            request.validate_original(
                &input,
                &run_record,
                &current_session_context,
                &hidden_tools,
            )?;
            if state.interrupted_pending.is_some() || state.tool_operator_request.is_some() {
                return Err(InterfaceDriverError::FailedClosed(
                    "application request repeats or follows an interrupted native call".to_string(),
                ));
            }
            let config = self.operator_continuity()?;
            let operator_request = ToolOperatorContinuity {
                request: request.clone(),
                payload_artifact_bindings: payload_selection.clone(),
                request_artifact: input.artifact.clone(),
                operator_interface: octopus_run_authoring::bound_client_interface(
                    &run_record,
                    &runtime.interface,
                )
                .map_err(InterfaceDriverError::FailedClosed)?,
                phase: ToolOperatorPhase::AwaitingOperator,
            };
            let output = request.projection(&config);
            state.tool_operator_request = Some(operator_request);
            sink.set_continuity(state);
            self.retain_tool_operator_wait(
                &scope,
                &sink,
                control,
                &handoff_barrier,
                output,
            )
            .await?;
            return Ok(());
        }
        if let Some(mut state) = recovered.clone()
            && let Some(mut request) = state.tool_operator_request.clone()
        {
            request.request.validate_pending(&state)?;
            if request.payload_artifact_bindings != payload_selection {
                return Err(InterfaceDriverError::FailedClosed("pending operator continuation changed its exact payload selection".into()));
            }
            match &provider_input {
                ProviderInput::OperatorResponse(_) => {
                    if state.interrupted_pending.is_some() {
                        return Err(InterfaceDriverError::FailedClosed(
                            "application approval cannot resume an interrupted native call"
                                .to_string(),
                        ));
                    }
                    request.admit(&input, &self.operator_continuity()?)?;
                    state.tool_operator_request = Some(request);
                    sink.set_continuity(state);
                    self.retain_tool_operator_wait(
                        &scope,
                        &sink,
                        control,
                        &handoff_barrier,
                        CodexTurnOutput::client(
                            INTERFACE_CONTEXT_UPDATED_LABEL,
                            serde_json::json!({
                                "operator_response_artifact": input.artifact,
                            }),
                        ),
                    )
                    .await?;
                    return Ok(());
                }
                ProviderInput::ToolResult(result) => {
                    let result = request.resume_result(result, &input)?;
                    effective_request_bytes = serde_json::to_vec(&result)
                        .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
                    effective_manifest_label = result.label.clone();
                    provider_input = ProviderInput::ToolResult(result);
                }
                _ => {
                    return Err(InterfaceDriverError::FailedClosed(
                        "input cannot replace a native application approval continuation"
                            .to_string(),
                    ));
                }
            }
        }
        let early_model_context = if recovered.as_ref().is_some_and(|state| state.interrupted_pending.is_none() && (state.pending.is_some() || state.context_refresh.is_some()))
            && matches!(&provider_input, ProviderInput::ToolResult(_) | ProviderInput::InterfaceOperationResult(_)) {
            Some(self.resolve_model_context(&input, &assembled_context).await?)
        } else { None };
        let effect_gate = Arc::new(tokio::sync::Mutex::new(()));
        let turn_interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let native_parent_record = octopus_runtime::run_record::RunRecord::from_bytes(current_run_record)
            .map_err(|error| InterfaceDriverError::FailedClosed(error.to_string()))?;
        let turn_context = TurnContext {
            run_invocation: run_affordances.as_ref().map(|affordances| NativeRunInvocation {
                affordances,
                metadata: input.metadata().expect("validated invocation metadata"),
                admitted_record: &admitted_record,
                current_record: &native_parent_record,
            }),
            scope: &scope,
            sink: &sink,
            control,
            interface_operations: &native_catalog.interface_operations,
            tool_names: &native_catalog.tool_names,
            source_artifact: &effective_source_artifact,
            handoff_barrier: &handoff_barrier,
            effect_gate: &effect_gate,
            turn_interrupted: &turn_interrupted,
            lifecycle: lifecycle.as_ref(),
            lifecycle_prompt: lifecycle_prompt_event.as_ref(),
        };
        if let Some(model_context) = &early_model_context
            && self.refresh_native_context(
                turn_context, &provider_input, recovered.as_ref(), context_generation,
                CurrentNativeContext {
                    model: &model_context.model, instructions: &model_context.bound_instructions, tools: &native_catalog.specs,
                    static_fingerprint: &model_context.static_context_fingerprint,
                    conversation: model_context.conversation_block.as_deref(), obligations: model_context.obligations_grant_block.as_deref(),
                    session_available: assembled_context.session_context_available,
                    reasoning_effort: model_context.reasoning_effort.clone(), reasoning_summary: model_context.reasoning_summary.clone(),
                    recovered_dispatches: &recovered_turn_start_dispatches,
                },
            ).await? { return Ok(()); }
        if durable_interrupt_override {
            emit_lifecycle_trigger(
                &sink,
                lifecycle.as_ref(),
                runtime.run.as_str(),
                LifecycleTrigger {
                    event: LifecycleEventName::Interrupt,
                    trigger: "durable_interrupt_recovery".to_string(),
                    match_values: Vec::new(),
                    subject_artifact: effective_source_artifact.clone(),
                    data: serde_json::json!({
                        "interrupted_after_artifact": interrupted_after_artifact.clone(),
                    }),
                    continuation: None,
                },
            )?;
        }
        let mut recovered_tool_continuation = None;
        let mut cut_turn_continuation = None;
        let mut recovered_operator_continuation = None;
        let interrupted_result_recovery = matches!(
            (
                &provider_input,
                recovered
                    .as_ref()
                    .and_then(|state| state.interrupted_pending.as_ref())
            ),
            (
                ProviderInput::ToolResult(_),
                Some(CodexPendingContinuity::DynamicTool { .. })
            ) | (
                ProviderInput::InterfaceOperationResult(_),
                Some(CodexPendingContinuity::InterfaceOperation { .. })
            )
        );
        let pending_result_recovery = matches!(
            (
                &provider_input,
                recovered.as_ref().and_then(|state| state.pending.as_ref())
            ),
            (
                ProviderInput::ToolResult(_),
                Some(CodexPendingContinuity::DynamicTool { .. })
            ) | (
                ProviderInput::InterfaceOperationResult(_),
                Some(CodexPendingContinuity::InterfaceOperation { .. })
            )
        );
        if recovered
            .as_ref()
            .is_some_and(|state| state.interrupted_pending.is_some())
            && !interrupted_result_recovery
        {
            control.finish();
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex input '{}' cannot replace a durably interrupted exact effect before its correlated result is admitted",
                manifest.label
            )));
        }
        if let Some(last_artifact_label) = interrupted_after_artifact.as_deref() {
            let Some(recovered) = recovered.as_ref() else {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex source recovery has no durable continuity evidence".to_string(),
                ));
            };
            if !interrupted_result_recovery && !pending_result_recovery {
                control.finish();
                return sink.send(interrupted_transport_output(
                    last_artifact_label,
                    recovered.pending.as_ref(),
                ));
            }
        }
        match &provider_input {
            ProviderInput::ToolResult(result) => {
                if let Some(binding) = lifecycle.as_ref()
                    && let Some(correlation_id) = result.correlation_id.as_deref()
                    && let Some(event) = binding
                        .bound_event_for_correlation(&lifecycle_payloads, correlation_id)
                        .map_err(InterfaceDriverError::FailedClosed)?
                {
                    binding
                        .validate_command_result(&event, &result.tool)
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    let output = binding
                        .normalize_handler_result(
                            &event,
                            "command_result",
                            result.ok,
                            &serde_json::json!({
                                "ok": result.ok,
                                "body": result.body,
                                "error": result.error,
                            }),
                        )
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    sink.send_lifecycle(output)?;
                    return Ok(());
                }
                if !lifecycle_resumed_provider_input && let Some(binding) = lifecycle.as_ref() {
                    let post = LifecycleTrigger {
                        event: LifecycleEventName::PostToolUse,
                        trigger: "tool_result_arrival".to_string(),
                        match_values: vec![result.tool.clone()],
                        subject_artifact: effective_source_artifact.clone(),
                        data: serde_json::json!({
                            "tool": result.tool,
                            "correlation_id": result.correlation_id,
                            "ok": result.ok,
                            "body": result.body,
                            "error": result.error,
                        }),
                        continuation: Some(LifecycleContinuation::ProviderInput {
                            manifest_label: effective_manifest_label.clone(),
                            payload: serde_json::from_slice(&effective_request_bytes).map_err(
                                |error| {
                                    InterfaceDriverError::Protocol(format!(
                                        "post_tool_use continuation input is not JSON: {error}"
                                    ))
                                },
                            )?,
                            source_artifact: effective_source_artifact.clone(),
                        }),
                    };
                    if emit_lifecycle_trigger(&sink, Some(binding), runtime.run.as_str(), post)? {
                        return Ok(());
                    }
                }
                if self
                    .resume_tool_result(
                        turn_context,
                        result.clone(),
                        recovered.as_ref(),
                        context_generation,
                        interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
                    )
                    .await?
                {
                    return Ok(());
                }
                if let Some(interrupted) = recovered
                    .as_ref()
                    .and_then(|state| state.interrupted_pending.as_ref())
                {
                    validate_recovered_tool_result(result, interrupted)?;
                    sink.set_pending(None);
                    let CodexPendingContinuity::DynamicTool {
                        native_operation: _,
                        turn_id,
                        call_id,
                        tool,
                    } = interrupted
                    else {
                        unreachable!("validated interrupted tool result has dynamic-tool state")
                    };
                    let native_namespace = self.native_tool_namespace()?.name;
                    let activity = (interrupted_after_artifact.as_deref()
                        != Some(TURN_DYNAMIC_TOOL_CALL_LABEL))
                    .then(|| {
                        CodexTurnOutput::dynamic_tool_completed_after_interrupt(
                            native_namespace,
                            tool.clone(),
                            call_id.clone(),
                            recovered
                                .as_ref()
                                .expect("interrupted continuity exists")
                                .thread_id
                                .clone(),
                            turn_id.clone(),
                            result.ok,
                            result.body.clone(),
                            result.error.clone(),
                        )
                    });
                    sink.bind_live_stream(
                        &recovered
                            .as_ref()
                            .expect("interrupted continuity exists")
                            .thread_id,
                        turn_id,
                    )?;
                    return sink.send_interrupted_result(
                        activity,
                        interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
                        CodexTurnOutput::interrupted_result_closed(
                            "tool_result",
                            call_id.clone(),
                            recovered
                                .as_ref()
                                .expect("interrupted continuity exists")
                                .thread_id
                                .clone(),
                            turn_id.clone(),
                            result.ok,
                            &effective_source_artifact,
                        ),
                    );
                }
                if let Some(pending) = recovered.as_ref().and_then(|state| state.pending.as_ref()) {
                    validate_recovered_tool_result(result, pending)?;
                    let state = recovered.as_ref().ok_or_else(|| InterfaceDriverError::FailedClosed("cold tool result has no source continuity".into()))?;
                    if state.context_generation != context_generation {
                        return Err(InterfaceDriverError::FailedClosed("cold tool result has stale source context generation".into()));
                    }
                    match self.complete_native_terminal_result(
                        turn_context, state, None,
                        CodexTurnOutput::dynamic_tool_completed_after_interrupt(
                            self.native_tool_namespace()?.name, result.tool.clone(), result.correlation_id.clone().ok_or_else(|| InterfaceDriverError::FailedClosed("cold result lost its correlation".into()))?,
                            state.thread_id.clone(), pending_turn_id(pending).to_string(), result.ok, result.body.clone(), result.error.clone(),
                        ), interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
                        CodexTurnOutput::interrupted_result_closed("tool_result", result.correlation_id.clone().ok_or_else(|| InterfaceDriverError::FailedClosed("cold result lost its correlation".into()))?, state.thread_id.clone(), pending_turn_id(pending).to_string(), result.ok, &effective_source_artifact),
                        native_pending_operation(pending)?.is_none(),
                    ).await? {
                        NativeTerminalCheck::Closed => return Ok(()),
                        NativeTerminalCheck::CutByProcessDeath(activity) => {
                            cut_turn_continuation = Some(CutTurnContinuation {
                                context: cut_turn_context(
                                    &current_session_context, &effective_source_artifact, state, &result.tool,
                                    &effective_manifest_label, &effective_request_bytes, self.model_result_projection()?,
                                )?,
                                activity,
                                activity_already_admitted: interrupted_after_artifact
                                    .as_deref()
                                    .is_some_and(|label| label != PROVIDER_TRANSPORT_DISPATCH_LABEL),
                            });
                        }
                        NativeTerminalCheck::Open => {}
                    }
                    let CodexPendingContinuity::DynamicTool {
                        native_operation: _,
                        turn_id,
                        call_id,
                        tool,
                    } = pending
                    else {
                        unreachable!("validated recovered tool result has dynamic-tool state")
                    };
                    if let Some(operation) = native_pending_operation(pending)? {
                        let receipt = serde_json::to_value(result).map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
                        match operation.advance(&receipt).map_err(InterfaceDriverError::FailedClosed)? {
                            NativeStep::Effect { pending: next, outgoing_bindings, .. } => {
                                emit_native_effect(&sink, turn_id, next, outgoing_bindings, None)?;
                                return Ok(());
                            }
                            NativeStep::AmendmentEffect { pending: next, amendment, .. } => {
                                emit_native_effect(&sink, turn_id, next, Vec::new(), Some(amendment))?;
                                return Ok(());
                            }
                            NativeStep::Complete { result: value } => {
                                recovered_tool_continuation = Some(RecoveredToolContinuation {
                                    item: recovered_function_call_output(&operation.call_id, recovered_result_output(native_result_content(&value, Vec::new())?)?),
                                    activity: CodexTurnOutput::dynamic_tool_completed(
                                        self.native_tool_namespace()?.name, operation.operation_name, operation.call_id,
                                        recovered.as_ref().expect("recovered continuity").thread_id.clone(), turn_id.clone(),
                                        native_result_success(&value), Some(value), None,
                                    ),
                                    activity_already_admitted: interrupted_after_artifact.as_deref().is_some_and(|label| label != PROVIDER_TRANSPORT_DISPATCH_LABEL),
                                });
                            }
                            NativeStep::Wait { .. } => return Err(InterfaceDriverError::FailedClosed("recovered effect cannot become a new wait".into())),
                        }
                    } else if cut_turn_continuation.is_none() {
                    let namespace = self.native_tool_namespace()?.name;
                    recovered_tool_continuation = Some(RecoveredToolContinuation {
                        item: recovered_function_call_output(
                            call_id,
                            recovered_result_output(dynamic_tool_response_content(
                                result, &effective_source_artifact, self.model_result_projection()?,
                            )?)?,
                        ),
                        activity: CodexTurnOutput::dynamic_tool_completed(
                            namespace,
                            tool.clone(),
                            call_id.clone(),
                            recovered
                                .as_ref()
                                .expect("recovered tool continuity exists")
                                .thread_id
                                .clone(),
                            turn_id.clone(),
                            result.ok,
                            result.body.clone(),
                            result.error.clone(),
                        ),
                        activity_already_admitted: interrupted_after_artifact
                            .as_deref()
                            .is_some_and(|label| label != PROVIDER_TRANSPORT_DISPATCH_LABEL),
                    });
                    }
                }
            }
            ProviderInput::InterfaceOperationResult(result) => {
                if let Some(binding) = lifecycle.as_ref()
                    && let Some(event) = binding
                        .bound_event_for_correlation(&lifecycle_payloads, &result.correlation_id)
                        .map_err(InterfaceDriverError::FailedClosed)?
                {
                    binding
                        .validate_mcp_result(&event, source_interface.as_ref(), &result.operation)
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    let output = binding
                        .normalize_handler_result(&event, "mcp_result", result.ok, &result.value)
                        .map_err(InterfaceDriverError::FailedClosed)?;
                    sink.send_lifecycle(output)?;
                    return Ok(());
                }
                if !lifecycle_resumed_provider_input && let Some(binding) = lifecycle.as_ref() {
                    let pending_is_subagent = recovered
                        .as_ref()
                        .and_then(|state| state.pending.as_ref())
                        .is_some_and(|pending| match pending {
                            CodexPendingContinuity::InterfaceOperation { interface, .. } => {
                                binding.is_subagent_interface(interface)
                            }
                            _ => false,
                        });
                    let continuation = LifecycleContinuation::ProviderInput {
                        manifest_label: effective_manifest_label.clone(),
                        payload: serde_json::from_slice(&effective_request_bytes).map_err(
                            |error| {
                                InterfaceDriverError::Protocol(format!(
                                    "interface-result lifecycle continuation is not JSON: {error}"
                                ))
                            },
                        )?,
                        source_artifact: effective_source_artifact.clone(),
                    };
                    let mut triggers = vec![LifecycleTrigger {
                        event: LifecycleEventName::PostToolUse,
                        trigger: "interface_operation_result_arrival".to_string(),
                        match_values: vec![result.operation.clone()],
                        subject_artifact: effective_source_artifact.clone(),
                        data: serde_json::json!({
                            "operation": result.operation,
                            "correlation_id": result.correlation_id,
                            "ok": result.ok,
                            "result": result.value,
                        }),
                        continuation: Some(continuation.clone()),
                    }];
                    if pending_is_subagent {
                        triggers.push(LifecycleTrigger {
                            event: LifecycleEventName::SubagentStop,
                            trigger: "system_worker_result_arrival".to_string(),
                            match_values: vec![result.operation.clone()],
                            subject_artifact: effective_source_artifact.clone(),
                            data: serde_json::json!({
                                "operation": result.operation,
                                "correlation_id": result.correlation_id,
                                "ok": result.ok,
                                "result": result.value,
                            }),
                            continuation: Some(continuation),
                        });
                    }
                    if emit_lifecycle_trigger_set(
                        &sink,
                        Some(binding),
                        runtime.run.as_str(),
                        triggers,
                    )? {
                        return Ok(());
                    }
                }
                if self
                    .resume_interface_operation_result(
                        turn_context,
                        result,
                        recovered.as_ref(),
                        context_generation,
                        interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
                    )
                    .await?
                {
                    return Ok(());
                }
                if let Some(interrupted) = recovered
                    .as_ref()
                    .and_then(|state| state.interrupted_pending.as_ref())
                {
                    validate_recovered_interface_operation_input(result, interrupted)?;
                    sink.set_pending(None);
                    let CodexPendingContinuity::InterfaceOperation {
                        turn_id,
                        call_id,
                        operation,
                        ..
                    } = interrupted
                    else {
                        unreachable!(
                            "validated interrupted interface result has interface-operation state"
                        )
                    };
                    let activity = (interrupted_after_artifact.as_deref()
                        != Some(TURN_DYNAMIC_TOOL_CALL_LABEL))
                    .then(|| {
                        CodexTurnOutput::interface_operation_completed_after_interrupt(
                            operation.clone(),
                            call_id.clone(),
                            recovered
                                .as_ref()
                                .expect("interrupted continuity exists")
                                .thread_id
                                .clone(),
                            turn_id.clone(),
                            result.ok,
                            result.value.clone(),
                        )
                    });
                    sink.bind_live_stream(
                        &recovered
                            .as_ref()
                            .expect("interrupted continuity exists")
                            .thread_id,
                        turn_id,
                    )?;
                    return sink.send_interrupted_result(
                        activity,
                        interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
                        CodexTurnOutput::interrupted_result_closed(
                            "interface_operation_result",
                            call_id.clone(),
                            recovered
                                .as_ref()
                                .expect("interrupted continuity exists")
                                .thread_id
                                .clone(),
                            turn_id.clone(),
                            result.ok,
                            &effective_source_artifact,
                        ),
                    );
                }
                if let Some(pending) = recovered.as_ref().and_then(|state| state.pending.as_ref()) {
                    validate_recovered_interface_operation_input(result, pending)?;
                    let state = recovered.as_ref().ok_or_else(|| InterfaceDriverError::FailedClosed("cold interface result has no source continuity".into()))?;
                    if state.context_generation != context_generation {
                        return Err(InterfaceDriverError::FailedClosed("cold interface result has stale source context generation".into()));
                    }
                    match self.complete_native_terminal_result(
                        turn_context, state, None,
                        CodexTurnOutput::interface_operation_completed_after_interrupt(
                            result.operation.clone(), result.correlation_id.clone(), state.thread_id.clone(), pending_turn_id(pending).to_string(), result.ok, result.value.clone(),
                        ), interrupted_after_artifact.as_deref() == Some(TURN_DYNAMIC_TOOL_CALL_LABEL),
                        CodexTurnOutput::interrupted_result_closed("interface_operation_result", result.correlation_id.clone(), state.thread_id.clone(), pending_turn_id(pending).to_string(), result.ok, &effective_source_artifact),
                        true,
                    ).await? {
                        NativeTerminalCheck::Closed => return Ok(()),
                        NativeTerminalCheck::CutByProcessDeath(activity) => {
                            cut_turn_continuation = Some(CutTurnContinuation {
                                context: cut_turn_context(
                                    &current_session_context, &effective_source_artifact, state, &result.operation,
                                    &effective_manifest_label, &effective_request_bytes, self.model_result_projection()?,
                                )?,
                                activity,
                                activity_already_admitted: interrupted_after_artifact
                                    .as_deref()
                                    .is_some_and(|label| label != PROVIDER_TRANSPORT_DISPATCH_LABEL),
                            });
                        }
                        NativeTerminalCheck::Open => {}
                    }
                    let CodexPendingContinuity::InterfaceOperation {
                        turn_id,
                        call_id,
                        operation,
                        ..
                    } = pending
                    else {
                        unreachable!(
                            "validated recovered interface result has interface-operation state"
                        )
                    };
                    if cut_turn_continuation.is_none() {
                        recovered_tool_continuation = Some(RecoveredToolContinuation {
                            item: recovered_function_call_output(
                                call_id,
                                recovered_result_output(interface_operation_response_content(
                                    result, &effective_source_artifact, self.model_result_projection()?,
                                )?)?,
                            ),
                            activity: CodexTurnOutput::interface_operation_completed(
                                operation.clone(),
                                call_id.clone(),
                                recovered
                                    .as_ref()
                                    .expect("recovered interface continuity exists")
                                    .thread_id
                                    .clone(),
                                turn_id.clone(),
                                result.ok,
                                result.value.clone(),
                            ),
                            activity_already_admitted: interrupted_after_artifact
                                .as_deref()
                                .is_some_and(|label| label != PROVIDER_TRANSPORT_DISPATCH_LABEL),
                        });
                    }
                }
                if recovered_tool_continuation.is_none() && cut_turn_continuation.is_none() {
                    return Err(InterfaceDriverError::FailedClosed(
                        "interface operation result has no live structured pending request"
                            .to_string(),
                    ));
                }
            }
            ProviderInput::InterfaceContext(context) => {
                if self.has_pending_request(&scope).await
                    || recovered
                        .as_ref()
                        .is_some_and(|state| state.pending.is_some())
                {
                    return Err(InterfaceDriverError::FailedClosed(
                        "interface context artifact cannot replace an unresolved app-server request"
                            .to_string(),
                    ));
                }
                return sink.send(CodexTurnOutput::client(
                    INTERFACE_CONTEXT_UPDATED_LABEL,
                    serde_json::json!({
                        "interface": context.interface,
                        "source_label": context.label,
                        "source_artifact": effective_source_artifact.clone(),
                        "content": context.content,
                    }),
                ));
            }
            ProviderInput::OperatorResponse(response) => {
                validate_operator_response_evidence(
                    &self.operator_continuity()?,
                    &effective_manifest_label,
                    &response,
                    input.metadata().map_err(|error| {
                        InterfaceDriverError::Protocol(format!(
                            "operator response has no invocation metadata: {error}"
                        ))
                    })?,
                )?;
                if self
                    .resume_operator_response(
                        turn_context,
                        response.clone(),
                        recovered.as_ref(),
                        context_generation,
                    )
                    .await?
                {
                    return Ok(());
                }
                if let Some(pending) = recovered.as_ref().and_then(|state| state.pending.as_ref()) {
                    validate_recovered_operator_response(response, pending)?;
                    recovered_operator_continuation = Some((response.clone(), pending.clone()));
                }
            }
            ProviderInput::Turn(_) | ProviderInput::Resume { .. } => {
                if self.has_pending_request(&scope).await {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "run '{}' sent a new provider turn while an app-server request still awaits its artifact result",
                        runtime.run.as_str()
                    )));
                }
                if recovered
                    .as_ref()
                    .is_some_and(|state| state.pending.is_some())
                {
                    return Err(InterfaceDriverError::FailedClosed(
                        "provider life dispatched an operator turn before its durable pending result reconciled"
                            .to_string(),
                    ));
                }
            }
        }

        let context_refresh::ResolvedModelContext { model, reasoning_effort, reasoning_summary,
            static_context_fingerprint, bound_instructions, conversation_block, obligations_grant_block } = match early_model_context {
                Some(context) => context,
                None => self.resolve_model_context(&input, &assembled_context).await?,
            };
        let native_affordances = &native_catalog.specs;

        if let Some((response, pending)) = recovered_operator_continuation {
            let recovered = recovered.as_ref().ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "operator response recovery lost its durable continuity".to_string(),
                )
            })?;
            self.resume_replayed_operator_response(
                turn_context,
                response,
                recovered,
                &pending,
                context_generation,
                &model,
                &bound_instructions,
                native_affordances,
                &static_context_fingerprint,
            )
            .await?;
            return Ok(());
        }

        let resume_control_id = match &provider_input {
            ProviderInput::Resume { control_id } => Some(control_id.clone()),
            _ => None,
        };
        let inspect_settled_history = matches!(&provider_input, ProviderInput::Turn(_))
            && !thread_compact_requested
            && peer_message.as_ref().is_none_or(|peer| peer.action != PeerAction::Queue)
            && recovered_turn_start_dispatches.is_empty();
        let turn_input = if recovered_tool_continuation.is_some() || cut_turn_continuation.is_some() {
            Vec::new()
        } else {
            provider_input.into_turn_input()?
        };

        let (mut thread_id, mut cold_thread, mut first_turn_of_run) = self
            .acquire_transport_thread(
                &scope,
                ThreadAcquisition {
                    recovered: recovered.as_ref(),
                    retired_history: None,
                    require_exact_resume: thread_compact_requested
                        || resume_control_id.is_some()
                        || cut_turn_continuation.is_some(),
                    context_generation: invocation_context_generation,
                    input_changes_context,
                    model: Some(model.clone()),
                    bound_instructions: &bound_instructions,
                    native_affordances,
                    static_context_fingerprint: &static_context_fingerprint,
                    session_context_available: assembled_context.session_context_available,
                },
            )
            .await?;

        let retirement = if inspect_settled_history {
            self.settled_native_history_proof(&scope, &thread_id, recovered.as_ref(), &current_session_context, &sink).await?
        } else { None };
        let retirement = if let Some(proof) = retirement {
            let source = recovered.as_ref().ok_or_else(|| InterfaceDriverError::FailedClosed("native history retirement lost its source".into()))?;
            let (replacement, cold, first) = self.acquire_transport_thread(&scope, ThreadAcquisition {
                recovered: Some(source), retired_history: Some(&proof), require_exact_resume: false,
                context_generation: invocation_context_generation, input_changes_context, model: Some(model.clone()),
                bound_instructions: &bound_instructions, native_affordances,
                static_context_fingerprint: &static_context_fingerprint,
                session_context_available: assembled_context.session_context_available,
            }).await?;
            if replacement == source.thread_id || !cold {
                return Err(InterfaceDriverError::FailedClosed("native history retirement did not create an independent replacement thread".into()));
            }
            let retirement = NativeHistoryRetirement {
                source_thread: source.thread_id.clone(), source_generation: source.context_generation,
                replacement_thread: replacement.clone(), source_artifact: effective_source_artifact.clone(),
                source_movement_seq, proof,
            };
            thread_id = replacement;
            cold_thread = cold;
            first_turn_of_run |= first;
            Some(retirement)
        } else { None };

        let thread_shape_fingerprint = thread_shape_fingerprint(
            Some(&model),
            &bound_instructions,
            native_affordances,
            &static_context_fingerprint,
        )?;
        let retained_refresh = recovered.as_ref().and_then(|state| state.context_refresh.as_ref())
            .filter(|refresh| refresh.matches_thread(&thread_id, &thread_shape_fingerprint, &static_context_fingerprint)).cloned();
        let retiring_history = retirement.is_some();
        let native_history_retirement = retirement.or_else(|| recovered.as_ref()
            .and_then(|state| state.native_history_retirement.as_ref())
            .filter(|retirement| retirement.replacement_thread == thread_id).cloned());
        sink.set_continuity(CodexContinuityState {
            schema_version: 1,
            provider_interface: runtime.interface.clone(),
            run: runtime.run.as_str().to_string(),
            thread_id: thread_id.clone(),
            thread_shape_fingerprint,
            context_generation,
            static_context_fingerprint,
            pending: recovered_tool_continuation
                .as_ref()
                .and_then(|_| recovered.as_ref().and_then(|state| state.pending.clone())),
            interrupted_pending: None,
            native_terminal: None,
            context_refresh: retained_refresh,
            native_history_retirement,
            tool_operator_request: recovered_tool_continuation.as_ref().and_then(|_| {
                recovered
                    .as_ref()
                    .and_then(|state| state.tool_operator_request.clone())
            }),
        });
        if retiring_history {
            sink.admit_native_history_retirement().await?;
        }

        if let Some(peer) = peer_message.as_ref().filter(|peer| peer.action == PeerAction::Queue) {
            let mut items = Vec::new();
            if cold_thread && let Some(context) = conversation_block.as_ref() { items.push(queued_peer_item(context.clone())); }
            items.push(queued_peer_item(peer_input_text(peer, &input.artifact)?));
            let request = ClientRequest::ThreadInjectItems {
                request_id: self.mint_request_id(), params: ThreadInjectItemsParams { thread_id: thread_id.clone(), items },
            };
            sink.admit_app_server_dispatch(&request, None).await?;
            self.client.lock().await.request_typed::<ThreadInjectItemsResponse>(request).await.map_err(typed_error)?;
            return sink.send(CodexTurnOutput::client(INTERFACE_CONTEXT_UPDATED_LABEL, serde_json::json!({
                "source_artifact": input.artifact, "peer_control_id": peer.control_id,
                "queued": true, "turn_triggered": false,
            })));
        }

        if let Some(control_id) = resume_control_id.as_ref() {
            sink.send(CodexTurnOutput::client(
                TURN_CONTROL_ACKNOWLEDGED_LABEL,
                serde_json::json!({
                    "control_id": control_id,
                    "action": "resume",
                    "status": "acknowledged",
                    "acknowledged": true,
                    "source_movement_seq": source_movement_seq,
                    "thread_id": thread_id,
                    "turn_id": serde_json::Value::Null,
                    "detail": {
                        "turn_trigger": TURN_TRIGGER_RESUME_INTERRUPTED_TASK,
                        "prompt_replayed": false,
                    },
                }),
            ))?;
        }

        if thread_compact_requested {
            return self.run_manual_compaction(turn_context, &thread_id).await;
        }

        let recovered_rollout_evidence = match (
            recovered_tool_continuation.as_ref(),
            interrupted_after_artifact.as_deref(),
        ) {
            (Some(continuation), last_artifact_label) => {
                if last_artifact_label == Some(PROVIDER_TRANSPORT_DISPATCH_LABEL)
                    && recovered_handoff_artifacts.is_empty()
                {
                    return Err(InterfaceDriverError::FailedClosed(
                        "Codex interrupted result recovery lost its durable transport-dispatch artifact"
                            .to_string(),
                    ));
                }
                let rollout_path = self
                    .transport_state
                    .lock()
                    .await
                    .get(&scope)
                    .and_then(|state| state.rollout_path.clone())
                    .ok_or_else(|| {
                        InterfaceDriverError::FailedClosed(
                            "Codex interrupted result recovery has no exact rollout path"
                                .to_string(),
                        )
                    })?;
                let mut native_activity = continuation.activity.value.clone();
                if let Some(tool) = native_activity.get("tool").and_then(serde_json::Value::as_str) {
                    let name = native_catalog.tool_names.iter().find_map(|(name, exact)| (exact == tool).then_some(name))
                        .ok_or_else(|| InterfaceDriverError::FailedClosed(format!("recovered tool '{tool}' has no admitted model name")))?;
                    native_activity["tool"] = serde_json::json!(decode_tool_name(name)?);
                }
                let evidence = recovered_tool_result_rollout_evidence(
                    &rollout_path,
                    &continuation.item,
                    Some(&native_activity),
                )?;
                if let Some(last_artifact_label) = last_artifact_label
                    && !evidence.output_persisted
                    && last_artifact_label != PROVIDER_TRANSPORT_DISPATCH_LABEL
                {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "Codex interrupted result recovery handed '{}' after dispatch without persisting the exact native tool result",
                        last_artifact_label
                    )));
                }
                evidence
            }
            _ => RecoveredToolResultRolloutEvidence::default(),
        };
        if recovered_tool_continuation.is_some() {
            reconcile_recovered_successor_dispatch(
                &recovered_rollout_evidence,
                &recovered_turn_start_dispatches,
            )?;
        } else if !recovered_turn_start_dispatches.is_empty() {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex recovered turn/start dispatch evidence is detached from a tool-result continuation"
                    .to_string(),
            ));
        }
        let recovered_item_already_injected = recovered_rollout_evidence.output_persisted;

        let additional_context = if let Some(context) = resumed_additional_context {
            context
        } else if let Some(cut) = cut_turn_continuation.as_ref() {
            native_turn_context(Some(cut.context.clone()), obligations_grant_block)
        } else if recovered_tool_continuation.is_some() || resume_control_id.is_some() {
            BTreeMap::new()
        } else {
            native_turn_context(if cold_thread { conversation_block } else { recovered_result_context }, obligations_grant_block)
        };
        if !thread_compact_requested
            && recovered_tool_continuation.is_none()
            && cut_turn_continuation.is_none()
            && resume_control_id.is_none()
            && lifecycle_prompt_event.is_none()
            && !lifecycle_start_resumed
        {
            let serialized_turn_input = serde_json::to_value(&turn_input).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "lifecycle start-turn input did not encode: {error}"
                ))
            })?;
            let continuation = LifecycleContinuation::StartTurn {
                turn_input: serialized_turn_input.clone(),
                additional_context: additional_context.clone(),
                source_artifact: effective_source_artifact.clone(),
            };
            let mut triggers = Vec::new();
            if first_turn_of_run {
                let source = if cold_thread { "startup" } else { "resume" };
                triggers.push(LifecycleTrigger {
                    event: LifecycleEventName::SessionStart,
                    trigger: "provider_thread_acquired".to_string(),
                    match_values: vec![source.to_string()],
                    subject_artifact: effective_source_artifact.clone(),
                    data: serde_json::json!({
                        "source": source,
                        "cold_thread": cold_thread,
                        "thread_id": thread_id.clone(),
                    }),
                    continuation: Some(continuation.clone()),
                });
            }
            triggers.push(LifecycleTrigger {
                event: LifecycleEventName::UserPromptSubmit,
                trigger: "provider_turn_before_dispatch".to_string(),
                match_values: Vec::new(),
                subject_artifact: effective_source_artifact.clone(),
                data: serde_json::json!({
                    "turn_input": serialized_turn_input,
                    "additional_context": additional_context,
                    "thread_id": thread_id.clone(),
                }),
                continuation: Some(continuation),
            });
            if emit_lifecycle_trigger_set(
                &sink,
                lifecycle.as_ref(),
                runtime.run.as_str(),
                triggers,
            )? {
                return Ok(());
            }
        }
        if first_turn_of_run && recovered_tool_continuation.is_none() && cut_turn_continuation.is_none() {
            self.emit_hooks_catalog(&sink).await?;
        }
        if let Some(continuation) = recovered_tool_continuation.as_ref()
            && !recovered_item_already_injected
        {
            recovered_rollout_evidence.validate_injection(&continuation.item)?;
            let inject = ClientRequest::ThreadInjectItems {
                request_id: self.mint_request_id(),
                params: ThreadInjectItemsParams {
                    thread_id: thread_id.clone(),
                    items: vec![continuation.item.clone()],
                },
            };
            {
                let client = self.client.lock().await;
                if !matches!(&*client, AppServerClient::Spawn(_)) {
                    return Err(InterfaceDriverError::FailedClosed(
                        "Codex result recovery dispatch evidence only supports the exact local process transport"
                            .to_string(),
                    ));
                }
            }
            sink.admit_app_server_dispatch(&inject, None).await?;
            {
                let client = self.client.lock().await;
                client
                    .request_typed::<ThreadInjectItemsResponse>(inject)
                    .await
                    .map_err(typed_error)?;
            }
        }
        let turn = ClientRequest::TurnStart {
            request_id: self.mint_request_id(),
            params: fresh_turn_start_params(
                thread_id.clone(),
                turn_input,
                model.clone(),
                reasoning_effort.clone(),
                self.bound_service_tier()?,
                reasoning_summary,
                additional_context,
                if resume_control_id.is_some() || cut_turn_continuation.is_some() {
                    TURN_TRIGGER_RESUME_INTERRUPTED_TASK
                } else if recovered_tool_continuation.is_some() {
                    TURN_TRIGGER_TOOL_RESULT_RECOVERY
                } else {
                    TURN_TRIGGER_INPUT
                },
            ),
        };
        {
            let client = self.client.lock().await;
            if !matches!(&*client, AppServerClient::Spawn(_)) {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex turn dispatch evidence only supports the exact local process transport"
                    .to_string(),
                ));
            }
        }
        sink.admit_app_server_dispatch(&turn, None).await?;
        let (turn_response, request_handle) = {
            let client = self.client.lock().await;
            let request_handle = client.request_handle();
            let response = client
                .request_typed::<octopus_appserver_proto::TurnStartResponse>(turn)
                .await
                .map_err(typed_error)?;
            (response, request_handle)
        };
        if turn_response.turn.id.trim().is_empty()
            || turn_response.turn.status != TurnStatus::InProgress
        {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "turn/start returned an invalid active turn: id='{}' status={:?}",
                turn_response.turn.id, turn_response.turn.status
            )));
        }
        let turn_id = turn_response.turn.id;
        if let Some((activity, activity_already_admitted)) = recovered_tool_continuation
            .map(|continuation| (continuation.activity, continuation.activity_already_admitted))
            .or(cut_turn_continuation.map(|cut| (cut.activity, cut.activity_already_admitted)))
        {
            if !activity_already_admitted && !live_stream_rules.enabled() {
                sink.send(activity)?;
            }
            if first_turn_of_run {
                self.emit_hooks_catalog(&sink).await?;
            }
        }
        control.publish(CodexTurnControlTarget::Draining {
            handle: request_handle,
            request_ids: self.next_request_id.clone(),
            pending_requests: self.pending_server_requests.clone(),
            scope: scope.clone(),
            output: Arc::downgrade(&emit),
            continuity: sink.continuity_snapshot(),
            handoff_barrier: handoff_barrier.clone(),
            effect_gate: effect_gate.clone(),
            turn_interrupted: turn_interrupted.clone(),
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
        });

        let outcome = self.drain_turn(turn_context, &thread_id, &turn_id).await?;
        match outcome {
            TurnDrainOutcome::Suspended(_) | TurnDrainOutcome::Complete => Ok(()),
        }
    }
}
