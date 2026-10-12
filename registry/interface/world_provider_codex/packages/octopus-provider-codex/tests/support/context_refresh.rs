#[tokio::test]
async fn changed_native_tools_continue_from_the_exact_completed_effect() {
    use octopus_appserver_proto::{
        ApprovalsReviewer, AskForApproval, SandboxPolicy, SessionSource, Thread, ThreadStatus,
    };
    for (interface_result, native_effect) in [(false, false), (true, false), (false, true)] {
        let effect_tool = if native_effect { "artifact.send" } else { "file.write" };
        let temp = tempfile::tempdir().unwrap();
        let socket_path = temp.path().join("refresh.sock");
        let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
        let cwd = octopus_util_abspath::AbsolutePathBuf::from_absolute_path(temp.path()).unwrap();
        let source_thread = Thread {
        id:"old-thread".into(), session_id:"old-thread".into(), forked_from_id:None, parent_thread_id:None,
        preview:String::new(), ephemeral:false, model_provider:"openai".into(), created_at:1, updated_at:1,
        recency_at:Some(1), status:ThreadStatus::Idle, path:None, cwd:cwd.clone(), cli_version:"test".into(),
        source:SessionSource::Exec, thread_source:None, agent_nickname:None, agent_role:None, git_info:None, name:None,
        turns:vec![serde_json::from_value(serde_json::json!({"id":"old-turn","items":[],"itemsView":"full","status":"inProgress","error":null})).unwrap()],
    };
        let server = tokio::spawn(async move {
            use futures::{SinkExt, StreamExt};
            let stream = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut retired = false;
            let mut methods = Vec::new();
            loop {
                let rpc = next_test_rpc(&mut socket).await;
                let octopus_appserver_proto::JSONRPCMessage::Request(request) = rpc else {
                    continue;
                };
                methods.push(request.method.clone());
                let response = match request.method.as_str() {
                    "initialize" => serde_json::json!({"userAgent":"codex/test","codexHome":cwd}),
                    "thread/read" => {
                        assert_eq!(request.params.as_ref().unwrap()["threadId"], "old-thread");
                        let mut thread = source_thread.clone();
                        if retired {
                            thread.turns[0].status = TurnStatus::Interrupted;
                        }
                        serde_json::json!({"thread":thread})
                    }
                    "turn/interrupt" => {
                        retired = true;
                        send_test_result(&mut socket, request.id, serde_json::json!({})).await;
                        let mut turn = source_thread.turns[0].clone();
                        turn.status = TurnStatus::Interrupted;
                        socket.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({
                        "method":"turn/completed","params":{"threadId":"old-thread","turn":turn}
                    }).to_string().into())).await.unwrap();
                        continue;
                    }
                    "thread/start" => {
                        let params = request.params.as_ref().unwrap();
                        assert!(params["dynamicTools"].to_string().contains("new-dcall"));
                        assert_eq!(params["developerInstructions"], "updated role");
                        let mut thread = source_thread.clone();
                        thread.id = "new-thread".into();
                        thread.session_id = thread.id.clone();
                        thread.turns.clear();
                        serde_json::to_value(ThreadStartResponse {
                            thread,
                            model: "gpt-5.5".into(),
                            model_provider: "openai".into(),
                            service_tier: None,
                            cwd: cwd.clone(),
                            runtime_workspace_roots: vec![],
                            instruction_sources: vec![],
                            approval_policy: AskForApproval::OnFailure,
                            approvals_reviewer: ApprovalsReviewer::User,
                            sandbox: SandboxPolicy::DangerFullAccess,
                            active_permission_profile: None,
                            reasoning_effort: None,
                            multi_agent_mode: None,
                        })
                        .unwrap()
                    }
                    "turn/start" => {
                        let params = request.params.as_ref().unwrap();
                        assert_eq!(params["threadId"], "new-thread");
                        assert_eq!(params["input"], serde_json::json!([]));
                        let context = params["additionalContext"].to_string();
                        assert!(context.contains("effect-performed-once"));
                        assert!(context.contains("original operator task"));
                        assert!(context.contains("freshly admitted grant"));
                        let mut turn = source_thread.turns[0].clone();
                        turn.id = "new-turn".into();
                        send_test_result(&mut socket, request.id, serde_json::json!({"turn":turn}))
                            .await;
                        socket.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({
                        "id":"new-call-request","method":"item/tool/call","params":{
                            "threadId":"new-thread","turnId":"new-turn","callId":"new-call-id",
                            "namespace":"formed_interfaces","tool":"new-dcall","arguments":{}}
                    }).to_string().into())).await.unwrap();
                        while socket.next().await.is_some() {}
                        return methods;
                    }
                    other => panic!("unexpected provider operation: {other}"),
                };
                send_test_result(&mut socket, request.id, response).await;
            }
        });
        let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
            endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
                socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(
                    &socket_path,
                )
                .unwrap(),
            },
            client_name: "octopus-test".into(),
            client_version: "0".into(),
            experimental_api: false,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: vec![],
            channel_capacity: 16,
        })
        .await
        .unwrap();
        let config = InterfaceDriverConfig { private_config:serde_json::json!({
        "private_thread_cold_start":"resume_compatible", "native_tool_namespace":{"name":"formed_interfaces","description":"tools"},
        "model_result_projection":{"max_tokens":10000}
    }).as_object().unwrap().clone(), ..InterfaceDriverConfig::default() };
        let driver = CodexAppServerInterfaceDriver::new_with_config(
            AppServerClient::Remote(client),
            Some(&config),
        );
        let scope = TransportStateScope::Run("run-a".into());
        let pending = if interface_result {
            CodexPendingContinuity::InterfaceOperation {
                turn_id: "old-turn".into(),
                call_id: "write-call".into(),
                operation: "file.write".into(),
                interface: "system_files".into(),
                output_label: "file.write.result".into(),
            }
        } else {
            CodexPendingContinuity::DynamicTool {
                native_operation: if native_effect { Some(serde_json::json!(NativePending {
                    call_id:"native-create-call".into(), operation_name:"create_run".into(), source_run:"run-a".into(),
                    effect:crate::run_affordances::NativeEffect { tool:effect_tool.into(), correlation_id:"write-call".into(), arguments:serde_json::json!({}) },
                    state:crate::run_affordances::NativePhase::AppliedSeeding {
                        created:serde_json::json!({"run_id":"run-b"}), files:vec![],
                        write_receipt:serde_json::json!({"receipt":"effect-performed-once"}), validation:serde_json::json!({"ok":true}),
                    },
                })) } else { None },
                turn_id: "old-turn".into(),
                call_id: "write-call".into(),
                tool: effect_tool.into(),
            }
        };
        let original = continuity(
            "world_provider_codex",
            "run-a",
            "old-thread",
            7,
            Some(pending),
        );
        let effect_gate = Arc::new(tokio::sync::Mutex::new(()));
        let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        driver.transport_state.lock().await.insert(
            scope.clone(),
            TransportState {
                context_generation: 7,
                hooks_catalog_emitted: true,
                thread_id: Some("old-thread".into()),
                thread_shape_fingerprint: Some(original.thread_shape_fingerprint.clone()),
                static_context_fingerprint: Some(original.static_context_fingerprint.clone()),
                rollout_path: None,
            },
        );
        driver
            .record_drain_outcome(
                &scope,
                TurnDrainOutcome::Suspended(PendingServerRequest {
                    request_id: RequestId::String("write-request".into()),
                    thread_id: "old-thread".into(),
                    turn_id: "old-turn".into(),
                    key: "tool:write-call".into(),
                    kind: if interface_result {
                        PendingServerRequestKind::InterfaceOperation {
                            call_id: "write-call".into(),
                            operation: "file.write".into(),
                            interface: "system_files".into(),
                            output_label: "file.write.result".into(),
                        }
                    } else {
                        PendingServerRequestKind::DynamicTool {
                            call_id: "write-call".into(),
                            tool: effect_tool.into(),
                        }
                    },
                    effect_gate: effect_gate.clone(),
                    turn_interrupted: interrupted.clone(),
                }),
            )
            .await
            .unwrap();
        let (emit, mut rx) = ProviderOutputStream::channel();
        let labels = codex_context_labels();
        let projected = codex_client_projection_labels();
        let rules = live_stream_rules();
        let streams = temp.path().join("streams");
        let interface = InterfaceId::new("world_provider_codex");
        let sink = TurnSink {
            emit: &emit,
            output_root: temp.path(),
            live_stream_root: &streams,
            provider_interface: &interface,
            run: "run-a",
            source_movement_seq: 20,
            attempt: 1,
            context_output_labels: &labels,
            client_projection_output_labels: &projected,
            live_stream_rules: &rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: emit.continuity.clone(),
        };
        sink.set_continuity(original.clone());
        sink.continuity.lock().unwrap().pending_input_context = true;
        let source_artifact = ArtifactRef {
            id: "exact-write-result".into(),
            content: octopus_ledger::ContentHash::of_bytes(b"receipt"),
        };
        let control = CodexTurnControlBroker::new(Some(20));
        let barrier = ProviderHandoffBarrier::new();
        let operations = BTreeMap::new();
        let names = BTreeMap::from([("new-dcall".into(), "new.call".into())]);
        let context = TurnContext {
            run_invocation: None,
            scope: &scope,
            sink: &sink,
            control: &control,
            interface_operations: &operations,
            tool_names: &names,
            source_artifact: &source_artifact,
            handoff_barrier: &barrier,
            effect_gate: &effect_gate,
            turn_interrupted: &interrupted,
            lifecycle: None,
            lifecycle_prompt: None,
        };
        let tools = namespace_native_affordances(
            vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: "new-dcall".into(),
                description: "new capability".into(),
                input_schema: serde_json::json!({"type":"object"}),
                defer_loading: false,
            })],
            &NativeToolNamespace {
                name: "formed_interfaces".into(),
                description: "tools".into(),
            },
        )
        .unwrap();
        let static_hash = "c".repeat(64);
        let current = || CurrentNativeContext {
            model: "gpt-5.5",
            instructions: "updated role",
            tools: &tools,
            static_fingerprint: &static_hash,
            conversation: Some("original operator task"),
            obligations: Some("freshly admitted grant"),
            session_available: true,
            reasoning_effort: None,
            reasoning_summary: ReasoningSummary::None,
            recovered_dispatches: &[],
        };
        let result = |call: &str| {
            if interface_result {
                ProviderInput::InterfaceOperationResult(InterfaceOperationResultArtifact {
                    label: "file.write.result".into(),
                    operation: "file.write".into(),
                    correlation_id: call.into(),
                    ok: true,
                    value: serde_json::json!({"label":"file.write.result","operation":"file.write","correlation_id":call,"ok":true,"result":{"receipt":"effect-performed-once"}}),
                })
            } else {
                ProviderInput::ToolResult(serde_json::from_value(serde_json::json!({
            "label":format!("{effect_tool}.result"),"tool":effect_tool,"correlation_id":call,"ok":true,
            "body":if native_effect { serde_json::json!({"admitted":{"seq":21}}) } else { serde_json::json!({"receipt":"effect-performed-once"}) }
        })).unwrap())
            }
        };
        assert!(
            driver
                .refresh_native_context(
                    context,
                    &result("wrong-call"),
                    Some(&original),
                    7,
                    current()
                )
                .await
                .is_err()
        );
        let admitted_result = result("write-call");
        let receive = async {
            driver
                .refresh_native_context(context, &admitted_result, Some(&original), 7, current())
                .await
                .expect("context refresh")
        };
        let collect = async {
            let mut phases = Vec::new();
            let mut prepared: Option<CodexContinuityState> = None;
            loop {
                match rx.recv().await.unwrap() {
                    ProviderStreamEmission::ContextBoundary {
                        emissions,
                        admitted,
                    } => {
                        let value: serde_json::Value = serde_json::from_slice(
                            &emissions
                                .last()
                                .unwrap()
                                .artifact
                                .read_payload_bytes()
                                .unwrap(),
                        )
                        .unwrap();
                        let marker = parse_codex_continuity(
                            &value[CODEX_CONTINUITY_FIELD],
                            &interface,
                            "run-a",
                        )
                        .unwrap();
                        assert!(
                            !codex_continuity_successor_is_settled(Some(&original), Some(&marker))
                                .unwrap()
                        );
                        let phase = value["context_refresh_phase"].as_str().unwrap().to_string();
                        if phase == "prepared" {
                            assert_eq!(serde_json::to_value(marker.context_refresh.as_ref().unwrap()).unwrap()["completed_output"]["call_id"],
                                if native_effect { "native-create-call" } else { "write-call" });
                            prepared = Some(marker.clone());
                        }
                        if phase == "retired" {
                            assert_eq!(emissions.len(), 2);
                            let previous = prepared.as_ref().unwrap();
                            let stream_manifest_path = emissions[0].artifact.manifest_path();
                            let original_manifest = std::fs::read(stream_manifest_path).unwrap();
                            for missing_manifest_hash in [false, true] {
                                if missing_manifest_hash {
                                    let mut unfinished = emissions[0].artifact.read_manifest().unwrap();
                                    unfinished.payload_content_hash = None;
                                    std::fs::write(stream_manifest_path, serde_json::to_vec_pretty(&unfinished).unwrap()).unwrap();
                                }
                                sink.set_continuity(previous.clone());
                                let (restored, exact) = sink.recover_context_retirement(previous.context_refresh.as_ref().unwrap()).unwrap().unwrap();
                                assert_eq!(restored, marker);
                                assert_eq!(std::fs::read(stream_manifest_path).unwrap(), original_manifest);
                                for (before, after) in emissions.iter().zip(exact.iter()) {
                                    assert_eq!(before.artifact.read_payload_bytes().unwrap(), after.artifact.read_payload_bytes().unwrap());
                                }
                            }
                            sink.set_continuity(marker.clone());
                        }
                        phases.push(phase);
                        admitted.send(Ok(())).unwrap();
                    }
                    ProviderStreamEmission::TransportDispatch { admitted, .. } => {
                        admitted.send(Ok(())).unwrap();
                    }
                    ProviderStreamEmission::Turn(emission) => {
                        let value: serde_json::Value = serde_json::from_slice(
                            &emission.artifact.read_payload_bytes().unwrap(),
                        )
                        .unwrap();
                        if value.get("tool") == Some(&serde_json::json!("new.call")) {
                            assert_eq!(value["correlation_id"], "new-call-id");
                            let state = parse_codex_continuity(
                                &value[CODEX_CONTINUITY_FIELD],
                                &interface,
                                "run-a",
                            )
                            .unwrap();
                            assert!(
                                !codex_continuity_successor_is_settled(
                                    Some(&original),
                                    Some(&state)
                                )
                                .unwrap()
                            );
                            let mut settled = state.clone();
                            settled.pending = None;
                            assert!(
                                codex_continuity_successor_is_settled(
                                    Some(&original),
                                    Some(&settled)
                                )
                                .unwrap()
                            );
                            let mut forged = settled.clone();
                            forged.context_refresh = None;
                            assert!(
                                codex_continuity_successor_is_settled(
                                    Some(&original),
                                    Some(&forged)
                                )
                                .is_err()
                            );
                            assert!(
                                reconcile_current_session_continuity(
                                    Some(original.clone()),
                                    Some(state)
                                )
                                .is_ok()
                            );
                            return phases;
                        }
                    }
                    _ => panic!("refresh must not emit an interrupted task or terminal reply"),
                }
            }
        };
        let (finished, phases) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(receive, collect)
        })
        .await
        .unwrap();
        assert!(finished);
        assert_eq!(phases, ["prepared", "retired", "ready"]);
        assert!(
            !driver
                .pending_server_requests
                .lock()
                .await
                .contains_key(&(scope.clone(), "tool:write-call".into()))
        );
        assert!(
            driver
                .pending_request(&scope, "tool:new-call-id")
                .await
                .unwrap()
                .is_some()
        );
        driver.shutdown().await.unwrap();
        assert_eq!(
            server.await.unwrap(),
            [
                "initialize",
                "thread/read",
                "thread/read",
                "turn/interrupt",
                "thread/read",
                "thread/start",
                "turn/start"
            ]
        );
    }
}
