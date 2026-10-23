#[tokio::test]
async fn declared_input_steers_before_closure_and_replays_without_another_effect() {
    use crate::handler::{CodexTurnControlTarget, CodexTurnOutput, PendingServerRequests, TransportStateScope, TurnSink};
    let temp = tempfile::tempdir().unwrap();
    let mut descriptor = descriptor();
    let config = &mut descriptor.interface_driver_config.as_mut().unwrap().private_config;
    let declaration = config["input_messages"]["realtime.delegation"].clone();
    config["input_messages"].as_object_mut().unwrap().insert("custom.user.request".into(), declaration);
    let ctx = context(temp.path(), &descriptor);
    let first = artifact(&temp.path().join("first"), "operator.message", json!({"text":"Begin work"}));
    let first_ref = cache_artifact(&ctx, &first);
    let second_payload = json!({
        "input_text":null, "delegation_id":"request-2", "call_id":"call-1", "correlation_id":"offer-1",
        "transcript":[{"role":"user","text":"Begin work"},{"role":"user","text":"Show the work"}],
        "client_context":[{"name":"client_state","text":"Widget: hidden."}]
    });
    let second = artifact(&temp.path().join("second"), "custom.user.request", second_payload.clone());
    let second_ref = cache_artifact(&ctx, &second);
    write_movements(&ctx.movements_path, &[
        movement(0, MovementKind::InterfaceOutbound, Endpoint::Interface("world_operator".into()), Endpoint::Interface(descriptor.id.clone()), first_ref.clone()),
        movement(1, MovementKind::InterfaceOutbound, Endpoint::Interface("world_request".into()), Endpoint::Interface(descriptor.id.clone()), second_ref.clone()),
    ]);
    let mut reader = LedgerTailReader::open(&ctx).unwrap();
    let first_page = reader.read_new_uncommitted_limit(1).unwrap();
    assert_eq!(first_page[0].movement.seq.0, 0);
    let rows = reader.read_new_uncommitted_limit(1).unwrap();
    let source = control_reader_source(&reader, &rows[0], &descriptor).expect("declared label reaches the inflight reader");
    let envelope = source.envelope.clone();
    assert!(matches!(decode_admitted_turn_control(&ctx, &descriptor, &envelope).unwrap(), Some(CodexTurnControl::Steer { .. })));
    let socket_path = temp.path().join("input.sock");
    let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
    let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = effects.clone();
    let server = tokio::spawn(async move {
        let mut socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap()).await.unwrap();
        let octopus_appserver_proto::JSONRPCMessage::Request(initialize) = next_test_rpc(&mut socket).await else { panic!("initialize") };
        send_test_result(&mut socket, initialize.id, json!({"userAgent":"fixture","codexHome":"/tmp"})).await;
        assert!(matches!(next_test_rpc(&mut socket).await, octopus_appserver_proto::JSONRPCMessage::Notification(_)));
        let octopus_appserver_proto::JSONRPCMessage::Request(request) = next_test_rpc(&mut socket).await else { panic!("steer") };
        assert_eq!(request.method, "turn/steer");
        let params = request.params.unwrap();
        assert_eq!(params["expectedTurnId"], "turn-a");
        assert_eq!(params["clientUserMessageId"], "request-2");
        let input = params["input"].to_string();
        assert!(input.contains("Widget: hidden."));
        assert!(input.contains("Show the work"));
        assert!(input.contains("offer-1"));
        let context = params["input"].as_array().unwrap().iter().find_map(|item| {
            serde_json::from_str::<serde_json::Value>(item.get("text")?.as_str()?).ok()
                .filter(|value| value.get("input_message_context").is_some())
        }).expect("full structured request reaches native steering");
        assert_eq!(context["input_message_context"]["fields"], second_payload);
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        send_test_result(&mut socket, request.id, json!({"turnId":"turn-a"})).await;
        while let Some(frame) = socket.next().await {
            let frame = match frame {
                Ok(frame) => frame,
                Err(tokio_tungstenite::tungstenite::Error::Io(error))
                    if matches!(error.kind(), std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset) => break,
                Err(error) => panic!("fixture socket failed: {error}"),
            };
            if frame.is_text() { panic!("replay made another native request: {}", frame.into_text().unwrap()); }
        }
    });
    let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
        endpoint: crate::RemoteAppServerEndpoint::UnixSocket { socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(&socket_path).unwrap() },
        client_name:"fixture".into(), client_version:"0".into(), experimental_api:false,
        mcp_server_openai_form_elicitation:false, opt_out_notification_methods:vec![], channel_capacity:8,
    }).await.unwrap();
    let (output, mut outputs) = ProviderOutputStream::channel();
    let mut original = continuity(0, None); original.turn_input = Some(first_ref);
    output.continuity.lock().unwrap().continuity = Some(original.clone());
    let broker = CodexTurnControlBroker::new(Some(0));
    broker.publish(CodexTurnControlTarget::Draining {
        handle:crate::AppServerRequestHandle::Remote(client.request_handle()), request_ids:Arc::new(std::sync::atomic::AtomicI64::new(1)),
        pending_requests:PendingServerRequests::default(), scope:TransportStateScope::Run("run-a".into()),
        output:Arc::downgrade(&output), continuity:Some(original), handoff_barrier:ProviderHandoffBarrier::new(),
        effect_gate:Arc::new(tokio::sync::Mutex::new(())), turn_interrupted:Arc::new(std::sync::atomic::AtomicBool::new(false)),
        thread_id:"thread-a".into(), turn_id:"turn-a".into(),
    });
    let handoff = ProviderHandoffSession::new(&ctx).unwrap();
    let arbiter = InterruptedEffectArbiter::new(ctx.clone());
    let gates = ControlSourceGates::default();
    let run = RunId::new("run-a");
    let process = process_control_reader_source(&ctx, &descriptor, &handoff, &arbiter, &gates, &broker, 1, source);
    let admission = async {
        let ProviderStreamEmission::Control { emission, source_movement_seq, admitted } = outputs.recv().await.unwrap() else { panic!("control result") };
        assert_eq!(source_movement_seq, 1);
        assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 1);
        let label = emission.artifact.read_manifest().unwrap().label;
        assert!(codex_context_output_labels(&descriptor).unwrap().contains(&label), "steering changes context and must advance the authored generation");
        let receipt: serde_json::Value = serde_json::from_slice(&emission.artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(receipt["codex_continuity"]["context_generation"], 1);
        append_movement(&ctx.movements_path, &movement(2, MovementKind::InterfaceArrival, Endpoint::Interface(descriptor.id.clone()),
            Endpoint::Interface("system_worker_test".into()), cache_artifact(&ctx, &emission.artifact)));
        admitted.send(Ok(())).unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), async { let ((), ()) = tokio::join!(async { process.await.unwrap(); }, admission); }).await.unwrap();
    let state = output.continuity_snapshot().unwrap();
    assert_eq!(state.turn_input, Some(second_ref.clone()));
    assert_eq!(state.thread_id, "thread-a");
    let labels = BTreeSet::new();
    let rules = LiveStreamRules::from_private_config_or_disabled(&serde_json::Map::new()).unwrap();
    let emitted_root = temp.path().join("final");
    let sink = TurnSink {
        emit:&output, output_root:&emitted_root, live_stream_root:temp.path(), provider_interface:&descriptor.id,
        run:"run-a", source_movement_seq:0, attempt:1, context_output_labels:&labels,
        client_projection_output_labels:&labels, live_stream_rules:&rules,
        live_stream:std::sync::Mutex::new(None), continuity:output.continuity.clone(),
    };
    let mut final_output = CodexTurnOutput::client("turn.completed", json!({"text":"Work shown"}));
    sink.normalize_output_label(&mut final_output).unwrap();
    let emitted = sink.materialize_normalized(final_output, false).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&emitted.artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(body["in_reply_to"], serde_json::to_value(&second_ref).unwrap());
    assert_eq!(body["codex_continuity"]["turn_input"], serde_json::to_value(&second_ref).unwrap());
    let journal = load_control_journal(&ctx, 1, &run, "request-2", "steer").unwrap().unwrap();
    let TurnControlJournalPhase::Delivered { result } = journal.phase else { panic!("delivered request") };
    assert_eq!(result.continuity.unwrap().turn_input, Some(second_ref));
    let before = std::fs::read(&ctx.movements_path).unwrap();
    process_control_reader_source(&ctx, &descriptor, &ProviderHandoffSession::new(&ctx).unwrap(),
        &InterruptedEffectArbiter::new(ctx.clone()), &ControlSourceGates::default(), &broker, 1,
        PendingControlReaderSource { envelope, label:"custom.user.request".into(), run }).await.unwrap();
    assert_eq!(std::fs::read(&ctx.movements_path).unwrap(), before);
    assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 1);
    broker.finish();
    client.shutdown().await.unwrap(); server.await.unwrap();
}
