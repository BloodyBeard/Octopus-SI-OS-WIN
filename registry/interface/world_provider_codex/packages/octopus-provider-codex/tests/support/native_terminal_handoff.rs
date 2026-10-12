#[path = "../../../../../world_operator_chat/packages/octopus-operator-chat-client/tests/support/composition_fixture.rs"]
mod terminal_composition;

#[tokio::test]
async fn native_terminal_answer_and_closure_reach_the_real_authoring_gate() {
    use octopus_provider_run_authoring::ProviderSourceRecovery;
    use octopus_runtime::run_record::RunRecord;
    fn edge(ctx: &InterfaceLiftContext, bytes: &[u8], checkpoint: &octopus_runtime::run_record::CheckpointRecord,
        file: &InterfaceArtifactFile, kind: MovementKind, target: InterfaceId) -> Movement {
        let record = RunRecord::from_bytes(bytes).unwrap();
        let tool=&ctx.descriptor_snapshot.as_ref().unwrap().interface_driver_config.as_ref().unwrap().private_config["run_authoring_profile"]["file_write_tool"];
        let interfaces=record.checkpoints.iter().flat_map(|cp|cp.inbound_interface().into_iter().chain(cp.outbound_interfaces())).cloned().collect::<BTreeSet<_>>();
        let artifact = cache_artifact(ctx, file);
        let hash = ContentHash::of_bytes(bytes);
        std::fs::write(ctx.blobs_dir.join(hash.to_hex()), bytes).unwrap();
        let meta: InvocationMetadata = serde_json::from_value(json!({
            "runtime":{"run":record.id,"ee":record.ee,"worker":record.worker,"checkpoint":checkpoint.id,
                "interface":if kind == MovementKind::InterfaceArrival { ctx.interface.clone() } else { target.clone() },"epoch":checkpoint.epoch.0,"run_record_hash":hash.to_hex()},
            "grant":{"tools":[tool],"interfaces":interfaces,"worker_environment_kinds":["system_room"],"mark_outcomes":record.checkpoints.iter().flat_map(|cp|cp.required_verified_artifacts.iter().map(|r|r.mark.clone())).collect::<BTreeSet<_>>(),
                "role_artifacts":checkpoint.artifact_bindings.iter().map(|b|b.id.clone()).collect::<Vec<_>>(),
                "artifact_access":checkpoint.artifact_bindings.iter().map(|b|(b.id.to_string(),b.access.clone())).collect::<BTreeMap<_,_>>()},
            "artifact_binding_refs":checkpoint.artifact_bindings.iter().map(|b| { let content=b.id.as_str().rsplit(':').next().and_then(ContentHash::from_hex).filter(|h|ctx.blobs_dir.join(h.to_hex()).exists()).unwrap_or_else(|| { let payload=std::fs::read(ctx.interface_dir.parent().unwrap().parent().unwrap().join("artifact").join(b.id.as_str()).join("artifact.json")).unwrap(); let hash=ContentHash::of_bytes(&payload); std::fs::write(ctx.blobs_dir.join(hash.to_hex()),payload).unwrap(); hash }); ArtifactRef{id:b.id.clone(),content} }).collect::<Vec<_>>(),
            "checkpoint_binding_policy":{"inbound":checkpoint.interface_binding,"outbound":checkpoint.outbound_interface_bindings},
            "outbound_interface_bindings":checkpoint.outbound_interfaces().cloned().collect::<Vec<_>>(),
            "checkpoint_admission":{"complete":true,"artifact_label":file.read_manifest().unwrap().label,"required_artifact_labels":[]}
        })).unwrap();
        Movement { seq:Seq(0), lamport:Lamport(1), recorded_at_unix_ms:1, trace:record.trace, kind,
            from:Endpoint::Interface(checkpoint.inbound_interface().unwrap().clone()), to:Endpoint::Interface(target),
            artifact:Some(artifact), invocation_metadata:Some(meta.into()) }
    }
    fn append(ctx: &InterfaceLiftContext, mut row: Movement) -> u64 {
        let count=std::fs::read(&ctx.movements_path).unwrap().split(|b|*b==b'\n').filter(|r|!r.is_empty()).count() as u64;
        row.seq=Seq(count); row.lamport=Lamport(count+1); row.recorded_at_unix_ms=count+1;
        append_movement(&ctx.movements_path,&row); count
    }
    async fn pump(gate_ctx: &InterfaceLiftContext, root: &Path, path: &Path, authoring_id: &InterfaceId,
        listener: &tokio::net::UnixListener, receiver: &mut tokio::sync::mpsc::Receiver<octopus_interface_transport::InterfaceActivationArrival>,
        expected_source: &ArtifactRef, delivered_before: usize, count: usize, with_answer: bool, phase: usize, ack_bootstrap: bool,
    ) -> Vec<(String,ArtifactRef,u64)> {
            let mut delivered=Vec::new(); let mut writes=0;
            loop { tokio::select! {
                accepted=listener.accept()=> {
                    let mut ws=tokio_tungstenite::accept_async(accepted.unwrap().0).await.unwrap();
                    let manifest:octopus_interfaces::InterfaceArtifactManifest=serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
                    let file=std::fs::read_dir(root.join("hand-in").join(authoring_id.as_str())).unwrap().map(|e|e.unwrap().path()).filter(|p|p.to_string_lossy().ends_with(".artifact.json")).find_map(|p| { let f=InterfaceArtifactFile::new(p).unwrap(); (serde_json::to_value(f.read_manifest().unwrap()).unwrap()==serde_json::to_value(&manifest).unwrap()).then_some(f) }).unwrap();
                    let before=std::fs::read(&path).unwrap(); let before_record=RunRecord::from_bytes(&before).unwrap();
                    let call_cp=before_record.checkpoints.iter().find(|cp|before_record.active_checkpoints.contains(&cp.id)&&cp.inbound_interface()==Some(authoring_id)).unwrap();
                    let door=call_cp.outbound_interfaces().next().unwrap().clone(); let seq=append(&gate_ctx,edge(&gate_ctx,&before,call_cp,&file,MovementKind::InterfaceOutbound,door.clone()));
                    let call:octopus_tool_handlers::ToolCallArtifact=serde_json::from_slice(&file.read_payload_bytes().unwrap()).unwrap();
                    let call_metadata=edge(gate_ctx,&before,call_cp,&file,MovementKind::InterfaceOutbound,door.clone()).invocation_metadata.unwrap().invocation().unwrap().clone();
                    let call_envelope=InterfaceInvocationEnvelope{artifact:cache_artifact(gate_ctx,&file),artifact_file:file,artifact_binding_payloads:vec![],metadata:Some(call_metadata)};
                    let body=octopus_system_room_env::execute_file_write_result(&call_envelope,&call.arguments,&["system_room".into()],&BTreeMap::from([("run_records_root".into(),root.join("runs"))]),&root.join(format!("effects-{writes}")),1<<24).unwrap();
                    let after=std::fs::read(&path).unwrap(); let after_record=RunRecord::from_bytes(&after).unwrap();
                    let result_cp=after_record.checkpoints.iter().find(|cp|after_record.active_checkpoints.contains(&cp.id)&&cp.inbound_interface()==Some(&door)&&cp.outbound_interfaces().any(|id|id==&gate_ctx.interface)).unwrap();
                    let label=gate_ctx.descriptor_snapshot.as_ref().unwrap().interface_driver_config.as_ref().unwrap().private_config["run_authoring_profile"]["file_write_result_label"].as_str().unwrap();
                    let receipt=artifact(&root.join("writes"),label,json!({"label":label,"tool":call.tool,"correlation_id":call.correlation_id,"ok":true,"body":body}));
                    append(&gate_ctx,edge(&gate_ctx,&after,result_cp,&receipt,MovementKind::InterfaceOutbound,gate_ctx.interface.clone()));
                    ws.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&DeliveryVerdict::Admitted{seq}).unwrap().into())).await.unwrap(); writes+=1;
                    if delivered_before+delivered.len()==count { break; }
                }
                incoming=receiver.recv()=> {
                    let incoming=incoming.unwrap(); let current=std::fs::read(&path).unwrap(); let current_record=RunRecord::from_bytes(&current).unwrap();
                    let label=incoming.artifact.read_manifest().unwrap().label;
                    let pending=&current_record.interface_state[&gate_ctx.interface]["pending_handoff"];
                    let items=pending["items"].as_array().unwrap();
                    assert_eq!(items.len(),if ack_bootstrap || with_answer || label==TURN_INTERRUPTED_RESULT_LABEL {1}else{2});
                    assert_eq!(pending["terminal"],ack_bootstrap || items.last().unwrap()["artifact_label"]==TURN_INTERRUPTED_RESULT_LABEL);
                    let item=items.iter().find(|item|item["artifact_label"]==label).unwrap();
                    if phase==2 && label==TURN_INTERRUPTED_RESULT_LABEL {
                        incoming.verdict_tx.unwrap().send(DeliveryVerdict::Refused{reason:octopus_ledger::RefusedReason::CheckpointSpent,detail:"fixture stop after settlement write".into()}).unwrap(); break;
                    }
                    if !ack_bootstrap && with_answer && phase==0 && label=="turn.completed" {
                        incoming.verdict_tx.unwrap().send(DeliveryVerdict::Refused{reason:octopus_ledger::RefusedReason::CheckpointSpent,detail:"fixture cold boundary after admitted answer".into()}).unwrap(); break;
                    }
                    let cp=current_record.checkpoint(&serde_json::from_value(item["checkpoint"]["id"].clone()).unwrap()).unwrap();
                    let seq=append(&gate_ctx,edge(&gate_ctx,&current,cp,&incoming.artifact,MovementKind::InterfaceArrival,current_record.worker.clone()));
                    let payload:serde_json::Value=serde_json::from_slice(&incoming.artifact.read_payload_bytes().unwrap()).unwrap_or(json!({}));
                    if let Some(continuity)=payload.get("codex_continuity") { assert_eq!(continuity["context_generation"],current_record.interface_state[&gate_ctx.interface]["context_generation"]); }
                    if label==TURN_INTERRUPTED_RESULT_LABEL { assert_eq!(payload["source_artifact"],json!(expected_source)); }
                    delivered.push((label,cache_artifact(&gate_ctx,&incoming.artifact),seq)); incoming.verdict_tx.unwrap().send(DeliveryVerdict::Admitted{seq}).unwrap();
                    if phase==3 { break; }
                }
            }}
            delivered
    }
    fn source_envelope(ctx: &InterfaceLiftContext, root: &Path, row: &Movement, file: InterfaceArtifactFile) -> InterfaceInvocationEnvelope {
        let metadata=row.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
        let payloads=metadata.artifact_binding_refs.iter().map(|reference| {
            let bytes=if reference.id.as_str().rsplit(':').next().and_then(ContentHash::from_hex).is_some() {
                let manifest:octopus_interfaces::InterfaceArtifactManifest=serde_json::from_slice(&std::fs::read(ctx.blobs_dir.join(reference.content.to_hex())).unwrap()).unwrap();
                std::fs::read(ctx.blobs_dir.join(manifest.payload_content_hash.unwrap())).unwrap()
            } else { std::fs::read(root.join("registry/artifact").join(reference.id.as_str()).join("artifact.json")).unwrap() };
            octopus_interfaces::ArtifactBindingPayload::new(reference.clone(),bytes)
        }).collect();
        InterfaceInvocationEnvelope {artifact:row.artifact.clone().unwrap(),artifact_file:file,artifact_binding_payloads:payloads,metadata:Some(metadata)}
    }
    for (with_stop, with_answer, native_interrupted, settlement_phase) in [
        (false,true,false,None),(true,true,false,None),(false,false,false,None),(true,false,false,None),
        (false,false,true,None),(true,false,true,None),(false,false,true,Some(0)),(false,false,true,Some(2)),(false,false,true,Some(3)),(false,false,true,Some(4)),
    ] {
        let temp=tempfile::tempdir().unwrap(); let root=temp.path();
        let request=terminal_composition::client_fixture(root);
        let plan=octopus_operator_chat_client::run_composition::plan_run_composition(&request).unwrap();
        for write in &plan.descriptor_writes { std::fs::create_dir_all(write.path.parent().unwrap()).unwrap(); std::fs::write(&write.path,&write.content).unwrap(); }
        let mut initial:serde_json::Value=serde_json::from_str(&plan.run_write.content).unwrap();
        let templates=&initial["interface_state"][&plan.provider_interface_id]["run_authoring_binding"]["checkpoints"];
        let index=templates.as_array().unwrap().iter().position(|template|template["role"]=="stable" && template["include_context"]==true).unwrap();
        let template=&templates[index];
        let row=initial["checkpoints"].as_array().unwrap().iter().find(|row|row["interface_binding"]==template["interface_binding"]
            && row["outbound_interface_bindings"]==template["outbound_interface_bindings"] && row["required_verified_artifacts"]==template["required_verified_artifacts"]).unwrap();
        let static_ids=row["artifact_bindings"].as_array().unwrap().iter().map(|binding|binding["id"].clone()).collect::<Vec<_>>();
        assert!(!static_ids.is_empty());
        initial["interface_state"][&plan.provider_interface_id]["run_authoring_binding"]["checkpoints"][index]["static_artifact_bindings"]=json!(static_ids);
        let initial=serde_json::to_vec(&initial).unwrap(); let record=RunRecord::from_bytes(&initial).unwrap(); let run=record.id.clone();
        std::fs::create_dir_all(plan.run_write.path.parent().unwrap()).unwrap(); std::fs::write(&plan.run_write.path,&initial).unwrap();
        let mut descriptor:InterfaceDescriptor=serde_json::from_slice(&std::fs::read(root.join("registry/interface").join(&plan.provider_interface_id).join("interface.json")).unwrap()).unwrap();
        let canonical=super::tests::descriptor();
        for key in ["context_output_labels","client_projection_output_labels","session_snapshot_labels","live_stream"] {
            descriptor.interface_driver_config.as_mut().unwrap().private_config.insert(key.into(), canonical.interface_driver_config.as_ref().unwrap().private_config[key].clone());
        }
        let (arrivals,mut receiver)=tokio::sync::mpsc::channel(8);
        let mut ctx=context(root,&descriptor); ctx.interface_dir=root.join("registry/interface").join(&plan.provider_interface_id); ctx.arrivals=arrivals;
        ctx.root_slots=BTreeMap::from([("run_records_root".into(),root.join("runs")),("interface_hand_in_root".into(),root.join("hand-in")),("app_server_control_root".into(),root.join("control"))]);
        std::fs::write(&ctx.movements_path,b"").unwrap();
        let authoring=record.interface_state[&descriptor.id]["run_authoring_binding"]["authoring_source_interface"].as_str().unwrap();
        let authoring_id=InterfaceId::new(authoring);
        let socket=octopus_app_server_interface::interface_control_socket_path_from_root(&root.join("control"),&authoring_id);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap(); let listener=tokio::net::UnixListener::bind(socket).unwrap();
        let native=serde_json::to_value(octopus_appserver_proto::Turn { id:"turn-a".into(),items:if with_answer {vec![octopus_appserver_proto::ThreadItem::AgentMessage{id:"answer-a".into(),text:"Retained native answer".into(),phase:Some(octopus_protocol::models::MessagePhase::FinalAnswer)}]} else {vec![]},items_view:octopus_appserver_proto::TurnItemsView::Full,status:if with_answer {octopus_appserver_proto::TurnStatus::Completed} else if native_interrupted {octopus_appserver_proto::TurnStatus::Interrupted} else {octopus_appserver_proto::TurnStatus::Failed},error:None,started_at:Some(10),completed_at:Some(20),duration_ms:Some(10_000) }).unwrap();
        let before_context=record.interface_state[&descriptor.id]["context_generation"].as_u64().unwrap();
        let mut state=serde_json::to_value(continuity(before_context,Some(if with_stop {"interrupted_pending"}else{"pending"}))).unwrap();
        state["provider_interface"]=json!(descriptor.id); state["run"]=json!(run);
        if let Some(phase) = settlement_phase {
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id)
                && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
            let input=artifact(&root.join("original-input"),"operator.message",json!({"text":"use the tool"}));
            let row=edge(&ctx,&initial,cp,&input,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let input=source_envelope(&ctx,root,&row,input); let source=append(&ctx,row);
            let first=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&input,source,&initial).unwrap();
            state["context_generation"]=json!(before_context+1);
            let original:crate::handler::CodexContinuityState=serde_json::from_value(state.clone()).unwrap();
            let effect=artifact(&ctx.interface_var_dir().join("effect"),"tool.call",json!({"tool":"coder.exec","arguments":{},"correlation_id":"call-a","codex_continuity":original}));
            let admit=first.hand_final(ProviderEmission{artifact:effect,route:ProviderEmissionRoute::ClientProjection});
            let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&input.artifact,0,1,true,0,true);
            let (result,_)=tokio::join!(admit,gate);result.unwrap();
            let pinned=std::fs::read(&plan.run_write.path).unwrap();let record=RunRecord::from_bytes(&pinned).unwrap();
            if phase==4 {
                let (transport,_release,mut methods,server)=interrupted_drain_driver(root,&descriptor).await;
                let mut driver=Some(transport);
                let auth=CodexRuntimeAuth { manager:auth_manager_from_provider_home(root.join("auth")).await,
                    provider_home:root.join("provider"),auth_store_home:root.join("auth"),activation_secrets_home:root.join("secrets"),connection_mode:ProviderConnectionMode::Native };
                let controls:ActiveTurnControls=Default::default();let arbiter=InterruptedEffectArbiter::new(ctx.clone());
                let session=ProviderHandoffSession::new(&ctx).unwrap();
                let committed=std::fs::read_to_string(&ctx.movements_path).unwrap().lines().count() as u64;
                assert!(source<committed);
                Box::pin(restore_completed_provider_sources(&ctx,&descriptor,&session,&auth,&mut driver,&controls,&arbiter,committed)).await.unwrap();
                let broker=active_control(&controls,&run).await.expect("consumed source must restore its pending effect owner");
                assert!(broker.is_recovered_pending());assert!(!broker.is_finished());assert!(methods.try_recv().is_err());
                assert_eq!(std::fs::read(&plan.run_write.path).unwrap(),pinned);
                let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                    && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.turn.control")).unwrap();
                let stop=artifact(&root.join("stop"),"operator.turn.control",json!({"action":"stop","control_id":"stop-a"}));
                let row=edge(&ctx,&pinned,cp,&stop,MovementKind::InterfaceOutbound,descriptor.id.clone());
                let stop=source_envelope(&ctx,root,&row,stop);let stop_seq=append(&ctx,row);
                assert!(matches!(control_reader_ownership(Some(&broker),committed,stop_seq,"operator.turn.control"),ControlReaderOwnership::Independent));
                let gates=Default::default();
                let control=decode_admitted_turn_control(&ctx,&descriptor,&stop).unwrap().unwrap();
                let admit=Box::pin(handle_control_source(&ctx,&descriptor,&session,&arbiter,&run,ControlSource {
                    envelope:&stop,source_movement_seq:stop_seq,broker:&broker,control_gates:&gates,control,
                    terminal_retirement_pending:true,independent_reader:true,
                }));
                let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&stop.artifact,0,1,true,0,true);
                let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(admit,gate)}).await.unwrap();
                assert_eq!(result.unwrap(),ControlSourceDisposition::Handled);assert_eq!(delivered[0].0,"turn.control.acknowledged");
                assert_eq!(methods.try_recv().unwrap(),"turn/interrupt");assert!(methods.try_recv().is_err());
                let interrupted=arbiter.durable_continuity(&run).await.unwrap().unwrap();
                let value=serde_json::to_value(&interrupted).unwrap();
                assert!(value["pending"].is_null());assert_eq!(value["interrupted_pending"],state["pending"]);
                assert_eq!(value["context_generation"],json!(before_context+2));assert!(!broker.is_finished());
                let current=std::fs::read(&plan.run_write.path).unwrap();let record=RunRecord::from_bytes(&current).unwrap();
                let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                    && cp.inbound_interface()==Some(&InterfaceId::new(&plan.worker_interface_id)) && cp.outbound_interfaces().any(|id|id==&descriptor.id)).unwrap();
                let result=artifact(&root.join("result"),"coder.exec.result",json!({"label":"coder.exec.result","tool":"coder.exec","correlation_id":"call-a","ok":true,"body":{"applied":true}}));
                let row=edge(&ctx,&current,cp,&result,MovementKind::InterfaceOutbound,descriptor.id.clone());
                let envelope=source_envelope(&ctx,root,&row,result);let result_seq=append(&ctx,row);
                assert!(arbiter.begin_result(&run,result_seq,&envelope).await.unwrap());
                let handoff=session.from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&current).unwrap();
                let ProviderStreamEmission::InterruptedResult {stream:Some(stream),retained_outputs,closure,..}=crate::handler::native_terminal_group_for_test(
                    &descriptor,&ctx.interface_var_dir().join("emitted"),&ctx.interface_var_dir().join("streams"),
                    &interrupted,native.clone(),&envelope.artifact,result_seq,true,handoff.input_changes_context().await,
                ) else {panic!("exact interrupted result group")};
                let closure_ref=materialized_artifact_ref(&closure.artifact).unwrap();
                let mut emissions=vec![stream];emissions.extend(retained_outputs);emissions.push(closure);let count=emissions.len();
                let admit=admit_interrupted_result(&ctx,&handoff,&broker,Some(&arbiter),&run,result_seq,&closure_ref,emissions);
                let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&envelope.artifact,0,count,false,1,false);
                let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(admit,gate)}).await.unwrap();
                assert_eq!(result.unwrap(),count);assert_eq!(delivered.last().unwrap().0,TURN_INTERRUPTED_RESULT_LABEL);
                assert!(broker.is_finished());assert!(arbiter.reconciled(&run).await.unwrap());assert!(methods.try_recv().is_err());
                server.abort();continue;
            }
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.inbound_interface()==Some(&InterfaceId::new(&plan.worker_interface_id)) && cp.outbound_interfaces().any(|id|id==&descriptor.id)).unwrap();
            let result=artifact(&root.join("result"),"coder.exec.result",json!({"label":"coder.exec.result","tool":"coder.exec","correlation_id":"call-a","ok":true,"body":{"applied":true}}));
            let row=edge(&ctx,&pinned,cp,&result,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let envelope=source_envelope(&ctx,root,&row,result);let result_seq=append(&ctx,row);
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.turn.control")).unwrap();
            let stop=artifact(&root.join("stop"),"operator.turn.control",json!({"action":"stop","control_id":"stop-a"}));
            let row=edge(&ctx,&pinned,cp,&stop,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let stop=source_envelope(&ctx,root,&row,stop);let stop_seq=append(&ctx,row);
            let control=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&stop,stop_seq,&pinned).unwrap();
            let ack=artifact(&ctx.interface_var_dir().join("ack"),"turn.control.acknowledged",json!({
                "label":"turn.control.acknowledged","status":"acknowledged","control_id":"stop-a","action":"stop","source_movement_seq":stop_seq,
                "thread_id":null,"turn_id":null,"detail":{"already_terminal":true,"external_effect_dispatched":false,"linearization":"control_reader_terminal_fence"}
            }));
            let admit=control.hand_final(ProviderEmission{artifact:ack,route:ProviderEmissionRoute::ClientProjection});
            let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&stop.artifact,0,1,true,0,true);
            let (result,_)=tokio::join!(admit,gate);result.unwrap();
            let failed=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&pinned).unwrap();
            assert_eq!(failed.recover_source().await.unwrap(),ProviderSourceRecovery::Process);
            state["context_generation"]=json!(before_context+3);
            let error=artifact(&ctx.interface_var_dir().join("error"),"turn.error",json!({
                "label":"turn.error","message":"error text is not recovery authority","will_retry":false,
                "additional_details":{"source_movement_seq":result_seq},"codex_continuity":state
            }));
            let admit=failed.hand_final(ProviderEmission{artifact:error,route:ProviderEmissionRoute::ClientProjection});
            let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&envelope.artifact,0,1,true,0,true);
            let (result,old)=tokio::join!(admit,gate);result.unwrap();
            let session=ProviderHandoffSession::new(&ctx).unwrap();
            assert!(session.completed_source_candidates(&ctx).unwrap().contains(&(run.clone(),result_seq)));
            let handoff=session.from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&pinned).unwrap();
            let (recovery,outcomes)=handoff.recover_completed_source_with_outcomes().await.unwrap();assert_eq!(recovery,ProviderSourceRecovery::Completed);
            let context=handoff.current_session_context_artifacts().await;
            let proof=crate::handler::stopped_result::prove(&descriptor,&envelope,result_seq,&handoff.current_run_record_bytes().await,&context,&outcomes).unwrap().unwrap();
            validate_stopped_result_control(&ctx,&descriptor,&run,&proof).unwrap();
            for forged in [false,true] {
                let mut changed=proof.clone();
                if forged { changed.acknowledgement.content=ContentHash::of_bytes(b"forged acknowledgement"); } else { changed.acknowledgement_seq+=1; }
                assert!(validate_stopped_result_control(&ctx,&descriptor,&run,&changed).is_err());
            }
            let labels=codex_context_output_labels(&descriptor).unwrap();let output_root=ctx.interface_var_dir().join("emitted");
            for status in ["inProgress","completed"] {
                let mut wrong=native.clone();wrong["status"]=json!(status);
                assert!(crate::handler::stopped_result::materialize_for_test(&proof,&envelope,wrong,&output_root,&labels).is_err());
            }
            let closure=crate::handler::stopped_result::materialize_for_test(&proof,&envelope,native.clone(),&output_root,&labels).unwrap();
            let expected:serde_json::Value=serde_json::from_slice(&closure.artifact.read_payload_bytes().unwrap()).unwrap();
            assert_eq!(expected["ok"],true);assert_eq!(expected["returned_to_model"],false);
            assert_eq!(expected["native_terminal_input_context"],false);assert_eq!(expected["codex_continuity"]["context_generation"],json!(before_context+4));
            assert_eq!(expected["completed_source_settlement"]["error_artifact"],json!(old[0].1));
            let admit=handoff.hand_completed_source_settlement(&proof.predecessor,proof.context_generation(),closure);
            let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&envelope.artifact,0,1,true,phase,true);
            if phase==3 {
                tokio::pin!(admit,gate);
                tokio::select! { biased; delivered=&mut gate=>assert_eq!(delivered.len(),1), result=&mut admit=>panic!("settlement returned before crash boundary: {result:?}") }
            } else {
                let (result,delivered)=tokio::join!(admit,gate);
                if phase==2 { assert!(result.is_err());assert!(delivered.is_empty()); } else { result.unwrap();assert_eq!(delivered.len(),1); }
            }
            drop(handoff);
            let restored=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&envelope,result_seq,&pinned).unwrap();
            if phase!=0 {
                let replay=restored.recover_completed_source_with_outcomes();
                let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&envelope.artifact,usize::from(phase==3),1,true,0,true);
                let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(replay,gate)}).await.unwrap();
                assert_eq!(result.unwrap().0,ProviderSourceRecovery::Completed);assert_eq!(delivered.len(),usize::from(phase==2));
            }
            let before=std::fs::read(&plan.run_write.path).unwrap();let ledger=std::fs::read(&ctx.movements_path).unwrap();
            let (recovery,outcomes)=restored.recover_completed_source_with_outcomes().await.unwrap();assert_eq!(recovery,ProviderSourceRecovery::Completed);assert_eq!(outcomes.len(),1);
            let context=restored.current_session_context_artifacts().await;
            let admitted=context.iter().find(|entry|entry.label==TURN_INTERRUPTED_RESULT_LABEL).unwrap();
            assert_eq!(admitted.payload,expected);
            assert!(context.iter().any(|entry|entry.source_artifact==old[0].1));
            assert!(crate::handler::stopped_result::prove(&descriptor,&envelope,result_seq,&before,&context,&outcomes).unwrap().is_none());
            assert_eq!(std::fs::read(&plan.run_write.path).unwrap(),before);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
            let current=RunRecord::from_bytes(&before).unwrap();
            let cp=current.checkpoints.iter().find(|cp|current.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id) && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
            let message=artifact(&root.join("next-message"),"operator.message",json!({"text":"inspect installed work"}));
            let row=edge(&ctx,&before,cp,&message,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let message=source_envelope(&ctx,root,&row,message);let next_seq=append(&ctx,row);
            let next=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&message,next_seq,&before).unwrap();
            assert_eq!(next.recover_source().await.unwrap(),ProviderSourceRecovery::Process);
            assert!(next.current_session_context_artifacts().await.iter().any(|entry|entry.label==TURN_INTERRUPTED_RESULT_LABEL));
            continue;
        }
        let mut arbiter=InterruptedEffectArbiter::new(ctx.clone());
        if with_stop {
            let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id)
                && cp.interface_binding.as_ref().unwrap().admits("operator.turn.control")).unwrap();
            let control=artifact(&root.join("control-input"),"operator.turn.control",json!({"action":"stop","control_id":"stop-a"}));
            let source=edge(&ctx,&initial,cp,&control,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let control=source_envelope(&ctx,root,&source,control); let control_seq=append(&ctx,source);
            let handoff=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&control,control_seq,&initial).unwrap();
            assert!(!handoff.input_changes_context().await);
            state["context_generation"]=json!(before_context+1); state["native_terminal"]=json!({"thread_id":"thread-a","turn":native});
            let acknowledged:crate::handler::CodexContinuityState=serde_json::from_value(state.clone()).unwrap();
            let ack=artifact(&ctx.interface_var_dir().join("ack"),"turn.control.acknowledged",json!({"control_id":"stop-a","action":"stop","codex_continuity":acknowledged}));
            let admit=async {
                let result=handoff.hand_final(ProviderEmission{artifact:ack,route:ProviderEmissionRoute::ClientProjection}).await;
                if let Err(error)=&result { eprintln!("Stop ACK bootstrap: {error}"); } result
            };
            let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&control.artifact,0,1,true,0,true);
            let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(admit,gate)}).await.unwrap();
            assert_eq!(delivered.len(),1); assert_eq!(delivered[0].0,"turn.control.acknowledged");
            assert!(matches!(result.unwrap(),octopus_provider_run_authoring::ProviderHandoffOutcome::Admitted{seq,artifact}
                if seq==delivered[0].2 && artifact==delivered[0].1));
            arbiter.record_acknowledgement(&run,control_seq,"stop-a",DurableArtifactAdmission{artifact:delivered[0].1.clone(),movement_seq:delivered[0].2},acknowledged).await.unwrap();
        }
        let initial=std::fs::read(&plan.run_write.path).unwrap(); let record=RunRecord::from_bytes(&initial).unwrap();
        let base_context=record.interface_state[&descriptor.id]["context_generation"].as_u64().unwrap();
        assert_eq!(base_context,before_context+u64::from(with_stop));
        assert_eq!(state["context_generation"],json!(base_context));
        let original:crate::handler::CodexContinuityState=serde_json::from_value(state).unwrap();
        let cp=record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
            && cp.inbound_interface()==Some(&InterfaceId::new(&plan.worker_interface_id))
            && cp.outbound_interfaces().any(|id|id==&descriptor.id)).unwrap();
        let result=artifact(&root.join("result"),"coder.exec.result",json!({"label":"coder.exec.result","tool":"coder.exec","correlation_id":"call-a","ok":false,"error":{"code":"tool_effect_indeterminate","message":"uncertain exact effect"}}));
        let source=edge(&ctx,&initial,cp,&result,MovementKind::InterfaceOutbound,descriptor.id.clone());
        let envelope=source_envelope(&ctx,root,&source,result); let result_ref=envelope.artifact.clone(); let source_seq=append(&ctx,source);
        if with_stop { assert!(arbiter.begin_result(&run,source_seq,&envelope).await.unwrap()); }
        let session=ProviderHandoffSession::new(&ctx).unwrap();
        let mut handoff=session.from_invocation(&ctx,&descriptor,&run,&envelope,source_seq,&initial).unwrap();
        let input_context=handoff.input_changes_context().await;
        let count=if with_answer {4}else{3};
        let settled_generation=base_context+u64::from(input_context)+if with_answer {2}else{1};
        let output_root=ctx.interface_var_dir().join("emitted"); let stream_root=ctx.interface_var_dir().join("streams");
        let ProviderStreamEmission::InterruptedResult { stream:Some(stream),retained_outputs,closure,.. }=crate::handler::native_terminal_group_for_test(&descriptor,&output_root,&stream_root,&original,native,&result_ref,source_seq,with_stop,input_context) else {panic!("sealed native group")};
        let source_closure=materialized_artifact_ref(&closure.artifact).unwrap();
        let turn=QueuedTurn{seq:source_seq,envelope:envelope.clone()};
        let (cold,cold_ref)=recover_sealed_interrupted_result(&ctx,&descriptor,&turn,&original).unwrap().unwrap();
        assert_eq!(cold_ref,source_closure); assert_eq!(cold.len(),count);
        let mut emissions=vec![stream]; emissions.extend(retained_outputs); emissions.push(closure);
        for (live,cold) in emissions.iter().zip(&cold) { assert_eq!(materialized_artifact_ref(&live.artifact).unwrap(),materialized_artifact_ref(&cold.artifact).unwrap()); assert_eq!(live.artifact.read_payload_bytes().unwrap(),cold.artifact.read_payload_bytes().unwrap()); }
        let closed:serde_json::Value=serde_json::from_slice(&emissions[count-1].artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(closed["source_artifact"],json!(result_ref)); assert_eq!(closed["codex_continuity"]["context_generation"],json!(settled_generation));
        assert_eq!(closed["native_terminal_input_context"],json!(input_context));
        if with_answer {
            assert_eq!(emissions[1].artifact.read_manifest().unwrap().label,"interface.artifact.output");
            assert_eq!(emissions[2].artifact.read_manifest().unwrap().label,"turn.completed");
            assert!(handoff.hand_observation(ProviderEmission{artifact:emissions[1].artifact.clone(),route:emissions[1].route.clone()},false).await.unwrap_err().contains("cannot change the selected context"));
        }
        let mut all_delivered=Vec::new();
        let control = CodexTurnControlBroker::new(Some(source_seq));
        assert!(!control.is_finished());
        for phase in 0..if with_answer {2}else{1} {
        let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&result_ref,all_delivered.len(),count,with_answer,phase,false);
        let supplied=if phase==0 {emissions.iter().map(|e|ProviderEmission{artifact:e.artifact.clone(),route:e.route.clone()}).collect()} else {recover_sealed_interrupted_result(&ctx,&descriptor,&turn,&original).unwrap().unwrap().0};
        let admit=async {
            let result=admit_interrupted_result(&ctx,&handoff,&control,with_stop.then_some(&arbiter),&run,source_seq,&source_closure,supplied).await;
            if let Err(error)=&result { eprintln!("native terminal handoff stop={with_stop} answer={with_answer} phase={phase}: {error}"); }
            result
        };
        let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async {tokio::join!(admit,gate)}).await.unwrap();
        all_delivered.extend(delivered);
        if with_answer && phase==0 {
            assert!(!control.is_finished(),"a refused closure must retain its control owner");
            assert!(result.unwrap_err().contains("refused")); assert_eq!(all_delivered.len(),2);
            let labels=codex_context_output_labels(&descriptor).unwrap(); let snapshots=codex_session_snapshot_labels(&descriptor).unwrap();
            assert_eq!(native_result_admitted_prefix(&ctx,&run,source_seq,&emissions,&snapshots,&labels).unwrap().len(),2);
            let mut shuffled=emissions.iter().map(|e|ProviderEmission{artifact:e.artifact.clone(),route:e.route.clone()}).collect::<Vec<_>>(); shuffled.swap(1,2);
            assert!(native_result_admitted_prefix(&ctx,&run,source_seq,&shuffled,&snapshots,&labels).unwrap_err().contains("contiguous prefix"));
            let mut changed:serde_json::Value=serde_json::from_slice(&emissions[1].artifact.read_payload_bytes().unwrap()).unwrap(); changed["text"]=json!("changed answer");
            shuffled.swap(1,2); shuffled[1].artifact=artifact(&root.join("conflict"),"interface.artifact.output",changed);
            assert!(native_result_admitted_prefix(&ctx,&run,source_seq,&shuffled,&snapshots,&labels).unwrap_err().contains("semantic payload"));
            drop(handoff); arbiter=InterruptedEffectArbiter::new(ctx.clone());
            handoff=ProviderHandoffSession::new(&ctx).unwrap().from_invocation_when_ready(&ctx,&descriptor,&run,&envelope,source_seq,&initial).await.unwrap();
        } else { assert_eq!(result.unwrap(),count); assert!(control.is_finished(),"an admitted closure must release the next operator input"); }
        }
        assert_eq!(all_delivered.len(),count); assert_eq!(all_delivered[count-1].0,TURN_INTERRUPTED_RESULT_LABEL); assert_ne!(all_delivered[count-1].1,source_closure);
        let current=RunRecord::from_bytes(&std::fs::read(&plan.run_write.path).unwrap()).unwrap();
        assert_eq!(current.interface_state[&descriptor.id]["context_generation"],json!(settled_generation));
        assert_eq!(handoff.recover_source().await.unwrap(),ProviderSourceRecovery::Completed);
        if with_stop { assert!(arbiter.reconciled(&run).await.unwrap()); } else { assert!(!interrupted_effect_journal_path(&ctx,&run).exists()); }
        assert_eq!(recover_sealed_interrupted_result(&ctx,&descriptor,&turn,&original).unwrap().unwrap().1,source_closure);
        if native_interrupted {
            let history=root.join("native-history.jsonl");
            std::fs::write(&history,concat!("{\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-a\"}}\n",
                "{\"type\":\"turn_context\",\"payload\":{\"turn_id\":\"turn-a\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"call_id\":\"outer\",\"name\":\"exec\",\"input\":\"text(await tools.coder_exec({}));\"}}\n")).unwrap();
            let before=std::fs::read(&plan.run_write.path).unwrap();
            let cp=current.checkpoints.iter().find(|cp|current.active_checkpoints.contains(&cp.id)
                && cp.outbound_interfaces().any(|id|id==&descriptor.id)
                && cp.interface_binding.as_ref().unwrap().admits("operator.message")).unwrap();
            let input=artifact(&root.join("next-input"),"operator.message",json!({"text":"continue"}));
            let source=edge(&ctx,&before,cp,&input,MovementKind::InterfaceOutbound,descriptor.id.clone());
            let next_input=source_envelope(&ctx,root,&source,input); let next_seq=append(&ctx,source);
            let next_handoff=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&next_input,next_seq,&before).unwrap();
            let entries=next_handoff.current_session_context_artifacts().await;
            let receipt=crate::handler::native_history_receipt_for_test(&descriptor,&output_root,&history,
                serde_json::from_value(closed["codex_continuity"].clone()).unwrap(),&entries,&next_input.artifact,next_seq).await;
            let admit=next_handoff.hand_batch_with_outgoing_bindings(vec![receipt],false,&[]);
            let gate=pump(&ctx,root,&plan.run_write.path,&authoring_id,&listener,&mut receiver,&next_input.artifact,0,1,true,1,false);
            let (result,delivered)=tokio::time::timeout(std::time::Duration::from_secs(5),async { tokio::join!(admit,gate) }).await.unwrap();
            assert_eq!(delivered.len(),1); assert_eq!(delivered[0].0,"interface.context.updated");
            assert!(matches!(result.unwrap().as_slice(),[octopus_provider_run_authoring::ProviderHandoffOutcome::Admitted{..}]));
            drop(next_handoff);
            let restored=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&next_input,next_seq,&before).unwrap();
            let (_,outcomes)=tokio::time::timeout(std::time::Duration::from_secs(5),restored.recover_source_with_outcomes()).await.unwrap().unwrap();
            assert_eq!(outcomes.len(),1);
            let entries=restored.current_session_context_artifacts().await;
            let receipt=entries.iter().find(|entry|entry.source_artifact==delivered[0].1).unwrap();
            assert_eq!(receipt.payload["codex_continuity"]["thread_id"],"history-replacement");
            assert_eq!(receipt.payload["codex_continuity"]["context_generation"],json!(settled_generation+1));
        }
    }
}
