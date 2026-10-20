use super::*;
use octopus_descriptors::{MarkId, VerifierId};
use octopus_ledger::{Mark, MarkClass, MovementMetadata, VerifierPackagePin};
use octopus_ledger_index::ArrivalDedupIndex;
use octopus_tool_handlers::{ToolResultArtifact, ToolResultError};

fn ordered_before(entry: &Value) -> String {
    let preimage: octopus_run_authoring::Preimage = serde_json::from_value(entry["expected_before_content"].clone()).unwrap();
    preimage.decode(entry["expected_before_hash"].as_str().unwrap(), &mut octopus_run_authoring::DecodeBudget::new(8 * 1024 * 1024).unwrap()).unwrap()
}
fn ordered_postimage(entry: &Value) -> Value {
    let before = ordered_before(entry);
    assert_eq!(entry["expected_before_hash"], ContentHash::of_bytes(before.as_bytes()).to_hex());
    let mut after = serde_json::from_str(&before).unwrap();
    let patch: json_patch::Patch = serde_json::from_value(entry["patch"].clone()).unwrap();
    json_patch::patch(&mut after, &patch).unwrap();
    let format: Option<octopus_run_authoring::JsonFormat> = entry.get("json_format").cloned().map(serde_json::from_value).transpose().unwrap();
    let bytes = format.unwrap_or_default().encode(&after).unwrap();
    assert_eq!(entry["expected_after_hash"], ContentHash::of_bytes(&bytes).to_hex());
    after
}

fn rename(value: &mut Value) {
    match value {
        Value::String(s) => {
            *s = match s.as_str() {
                "voice" => "peer",
                "source" => "peer_source",
                "door" => "peer_door",
                "control" => "peer_control",
                "client" => "peer_client",
                "peer" => "voice",
                "peer_door" => "door",
                other => other,
            }
            .to_owned();
        }
        Value::Array(values) => values.iter_mut().for_each(rename),
        Value::Object(values) => values.values_mut().for_each(rename),
        _ => {}
    }
}

fn pair() -> (Fixture, Fixture) {
    let mut owner = Fixture::new();
    let mut peer = Fixture::new();
    let mut run: Value = serde_json::from_slice(&owner.pinned).unwrap();
    let required = json!([{"artifact_label":"tool.call","verifier":"tool_call_allowed","mark":"tool_allowed","params":{"tools":["file.write"],"metadata_matches":[{"payload":"/arguments/expected_before_hash","metadata":"/runtime/run_record_hash"}]}}]);
    run["interface_state"]["voice"]["run_authoring_binding"]["checkpoints"][3]["required_verified_artifacts"] =
        required.clone();
    run["checkpoints"][3]["required_verified_artifacts"] = required;
    run["interface_state"]["voice"]["run_authoring_binding"]["checkpoints"][2]["outbound_interface_bindings"] =
        json!([binding("peer", Some("realtime.delegation"))]);
    run["checkpoints"][2]["outbound_interface_bindings"] =
        json!([binding("peer", Some("realtime.delegation"))]);
    let mut peer_state = run["interface_state"]["voice"].clone();
    rename(&mut peer_state);
    peer_state["run_authoring_binding"]["checkpoints"][2]["interface_binding"] =
        json!(binding("peer", Some("realtime.session.update")));
    peer_state["run_authoring_binding"]["checkpoints"][2]["outbound_interface_bindings"] =
        json!([binding("voice", Some("realtime.session.update"))]);
    peer_state["run_authoring_binding"]["checkpoints"][5]["interface_binding"] =
        json!(binding("voice", Some("realtime.delegation")));
    peer_state["run_authoring_binding"]["checkpoints"][5]["outbound_interface_bindings"] =
        json!([binding("peer", Some("realtime.delegation"))]);
    let templates = peer_state["run_authoring_binding"]["checkpoints"]
        .as_array_mut()
        .unwrap();
    for template in templates.iter_mut() {
        template["name"] = json!(format!("second_{}", template["name"].as_str().unwrap()));
        if template["role"] == "owner_input" {
            template["owner_authoring"]["source"] = json!("source");
        }
    }
    let peer_checkpoints = templates.iter().filter(|t| t["role"] != "owner_input").map(|t| json!({"id":t["name"],"run":"run","trace":"trace:run","epoch":0,
        "interface_binding":t["interface_binding"],"outbound_interface_bindings":t["outbound_interface_bindings"],
        "artifact_bindings":[],"required_verified_artifacts":t["required_verified_artifacts"]})).collect::<Vec<_>>();
    peer_state["frontier_checkpoint_ids"] = json!([
        "second_output",
        "second_delegation",
        "second_write",
        "second_result"
    ]);
    run["interface_state"]["voice"]["run_authoring_binding"]["checkpoints"][5]["owner_authoring"]
        ["source"] = json!("peer_source");
    run["interface_state"]["peer"] = peer_state;
    run["checkpoints"]
        .as_array_mut()
        .unwrap()
        .retain(|c| c["id"] != "peer_input");
    run["active_checkpoints"]
        .as_array_mut()
        .unwrap()
        .retain(|id| id != "peer_input");
    run["active_checkpoints"]
        .as_array_mut()
        .unwrap()
        .extend(peer_checkpoints.iter().map(|c| c["id"].clone()));
    run["checkpoints"]
        .as_array_mut()
        .unwrap()
        .extend(peer_checkpoints);
    for provider in ["voice", "peer"] {
        let id = RunId::new(format!("writer_{provider}"));
        let writer = octopus_run_authoring::separate_writer_run(
            &mut run,
            &InterfaceId::new(provider),
            &id,
        )
        .unwrap();
        let content = octopus_run_authoring::JsonFormat::Compact.encode(&writer).unwrap();
        std::fs::write(
            owner.ctx.root_slots["run_records_root"].join(format!("{id}.json")),
            content,
        )
        .unwrap();
    }
    let mut pinned = serde_json::to_vec_pretty(&run).unwrap();
    pinned.push(b'\n');
    std::fs::write(&owner.run_path, &pinned).unwrap();
    std::fs::write(
        owner
            .ctx
            .blobs_dir
            .join(ContentHash::of_bytes(&pinned).to_hex()),
        &pinned,
    )
    .unwrap();
    std::fs::write(&owner.ctx.movements_path, b"").unwrap();
    peer.source = "peer_source".into();
    peer.door = "peer_door".into();
    peer.source_seq = 1;
    let mut descriptor = serde_json::to_value(&peer.descriptor).unwrap();
    descriptor["id"] = json!("peer");
    peer.descriptor = serde_json::from_value(descriptor).unwrap();
    peer.ctx.interface = InterfaceId::new("peer");
    peer.ctx.descriptor_snapshot = Some(peer.descriptor.clone());
    peer.ctx.movements_path = owner.ctx.movements_path.clone();
    peer.ctx.blobs_dir = owner.ctx.blobs_dir.clone();
    peer.ctx.ledger_index_path = owner.ctx.ledger_index_path.clone();
    peer.ctx.root_slots = owner.ctx.root_slots.clone();
    peer.run_path = owner.run_path.clone();
    let socket = octopus_app_server_interface::interface_control_socket_path_from_root(
        &peer.ctx.root_slots["app_server_control_root"],
        &InterfaceId::new(&peer.source),
    );
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    peer.listener = UnixListener::bind(socket).unwrap();
    for (fixture, control, input_id) in [
        (&mut owner, "control", "input"),
        (&mut peer, "peer_control", "second_input"),
    ] {
        fixture.pinned = pinned.clone();
        let checkpoint = run["checkpoints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == input_id)
            .unwrap();
        fixture.input.metadata = Some(metadata(
            checkpoint,
            &pinned,
            fixture.ctx.interface.as_str(),
        ));
        let grant = &mut fixture.input.metadata.as_mut().unwrap().grant;
        grant.tools.clear();
        grant.worker_environment_kinds.clear();
        grant.interfaces.retain(|id| id != &fixture.source && id != &fixture.door);
        fixture.input.artifact = pin(&fixture.input.artifact_file, &fixture.ctx.blobs_dir);
        let seq = fixture.append(
            MovementKind::InterfaceOutbound,
            control,
            fixture.ctx.interface.as_str(),
            fixture.input.artifact.clone(),
            fixture.input.metadata.clone(),
        );
        assert_eq!(seq, fixture.source_seq);
    }
    (owner, peer)
}

fn emission(fixture: &Fixture, n: u64) -> ProviderEmission {
    ProviderEmission {
        artifact: InterfaceArtifactFile::write(
            fixture.ctx.var_dir.join("output"),
            "realtime.transcript",
            &serde_json::to_vec(&json!({"sequence":n})).unwrap(),
        )
        .unwrap(),
        route: ProviderEmissionRoute::ClientProjection,
    }
}

fn coordinate(cp: &Value, bytes: &[u8], interface: &str, label: &str) -> InvocationMetadata {
    let mut m = metadata(cp, bytes, interface);
    m.required_verified_artifacts =
        serde_json::from_value(cp["required_verified_artifacts"].clone()).unwrap();
    m.checkpoint_admission = Some(octopus_ledger::CheckpointAdmissionMetadata {
        artifact_label: label.into(),
        required_artifact_labels: m
            .required_verified_artifacts
            .iter()
            .map(|r| r.artifact_label.clone())
            .collect(),
        complete: true,
    });
    m
}
fn admit_call(
    f: &Fixture,
    call: &Value,
    file: &InterfaceArtifactFile,
) -> InterfaceInvocationEnvelope {
    let before_content = ordered_before(&call["arguments"]["writes"][0]);
    let before = before_content.as_bytes();
    let run: Value = serde_json::from_slice(before).unwrap();
    assert_eq!(
        std::fs::read(call["arguments"]["writes"][0]["path"].as_str().unwrap()).unwrap(),
        before
    );
    std::fs::write(
        f.ctx.blobs_dir.join(ContentHash::of_bytes(before).to_hex()),
        before,
    )
    .unwrap();
    let cp = run["checkpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["interface_binding"]["interface"] == f.source)
        .unwrap();
    let reference = pin(file, &f.ctx.blobs_dir);
    f.append(
        MovementKind::InterfaceArrival,
        &f.source,
        "worker",
        reference.clone(),
        Some(coordinate(cp, before, &f.source, "tool.call")),
    );
    for required in cp["required_verified_artifacts"].as_array().unwrap() {
        let verifier = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../verifier/tool_call_allowed/verify.py");
        let mut child = std::process::Command::new("python3")
            .arg(verifier)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let request = json!({"params":required["params"],"payload_utf8":serde_json::to_string(call).unwrap(),"metadata":coordinate(cp,before,&f.door,"tool.call")});
        child
            .stdin
            .take()
            .unwrap()
            .write_all(serde_json::to_string(&request).unwrap().as_bytes())
            .unwrap();
        let result = child.wait_with_output().unwrap();
        let verdict: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(
            verdict["passed"], true,
            "real tool-call verifier: {verdict}"
        );
        let seq = std::fs::read_to_string(&f.ctx.movements_path)
            .unwrap()
            .lines()
            .count() as u64;
        let mark: Mark = Mark {
            name: MarkId::new(required["mark"].as_str().unwrap()),
            subject: CheckpointId::new(cp["id"].as_str().unwrap()),
            run: RunId::new(run["id"].as_str().unwrap()),
            run_epoch: 0,
            class: MarkClass::Success,
            evidence: Some(reference.clone()),
            arrival_seq: None,
            verifier: VerifierId::new(required["verifier"].as_str().unwrap()),
            verifier_package: Some(VerifierPackagePin {
                descriptor_content: ContentHash::of_bytes(b"fixture"),
                script_content: ContentHash::of_bytes(b"fixture"),
            }),
            details: json!({}),
        };
        let row = Movement {
            seq: Seq(seq),
            lamport: Lamport(seq + 1),
            recorded_at_unix_ms: seq + 1,
            trace: TraceId("trace:run".into()),
            kind: MovementKind::MarkOutcomeAdmitted(mark.name.clone()),
            from: Endpoint::Interface(InterfaceId::new(&f.source)),
            to: Endpoint::Interface(InterfaceId::new("worker")),
            artifact: Some(reference.clone()),
            invocation_metadata: Some(MovementMetadata::MarkOutcome(mark)),
        };
        let mut ledger = std::fs::OpenOptions::new()
            .append(true)
            .open(&f.ctx.movements_path)
            .unwrap();
        writeln!(ledger, "{}", serde_json::to_string(&row).unwrap()).unwrap();
        ledger.sync_all().unwrap();
    }
    f.append(
        MovementKind::InterfaceOutbound,
        &f.source,
        &f.door,
        reference.clone(),
        Some(coordinate(cp, before, &f.door, "tool.call")),
    );
    InterfaceInvocationEnvelope {
        artifact: reference,
        artifact_file: file.clone(),
        artifact_binding_payloads: vec![],
        metadata: Some(coordinate(cp, before, &f.door, "tool.call")),
    }
}
fn effect(
    f: &Fixture,
    call: &Value,
    input: &InterfaceInvocationEnvelope,
) -> Result<Value, ToolResultError> {
    octopus_system_room_env::execute_file_write_result(
        input,
        &call["arguments"],
        &["system_room".into()],
        &BTreeMap::from([(
            "run_records_root".into(),
            f.ctx.root_slots["run_records_root"].clone(),
        )]),
        &f.ctx.var_dir.join("effects"),
        f.ctx.host_io_limits.as_ref().unwrap().max_payload_bytes,
    )
}
fn admit_result(f: &Fixture, call: &Value, outcome: Result<Value, ToolResultError>) {
    let writer = std::fs::read(call["arguments"]["writes"][0]["path"].as_str().unwrap()).unwrap();
    let target = std::fs::read(&f.run_path).unwrap();
    for bytes in [&writer, &target] {
        std::fs::write(
            f.ctx.blobs_dir.join(ContentHash::of_bytes(bytes).to_hex()),
            bytes,
        )
        .unwrap();
    }
    let run: Value = serde_json::from_slice(&writer).unwrap();
    let cp = run["checkpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["interface_binding"]["interface"] == f.door)
        .unwrap();
    let correlation = Some(call["correlation_id"].as_str().unwrap().to_string());
    let result = match outcome {
        Ok(body) => ToolResultArtifact::success("file.write", correlation, body),
        Err(error) => ToolResultArtifact::failure("file.write", correlation, error),
    };
    let file = InterfaceArtifactFile::write(
        f.ctx.var_dir.join("results"),
        "file.write.result",
        &serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
    let mut index = ArrivalDedupIndex::new();
    index
        .catch_up(&octopus_ledger_store::DurableLog::open(&f.ctx.movements_path).unwrap())
        .unwrap();
    assert!(
        !index
            .checkpoint_admitted(
                &RunId::new(run["id"].as_str().unwrap()),
                &CheckpointId::new(cp["id"].as_str().unwrap()),
                0
            )
            .unwrap(),
        "result must have an unspent declared edge"
    );
    f.append(
        MovementKind::InterfaceArrival,
        &f.door,
        "worker",
        pin(&file, &f.ctx.blobs_dir),
        Some(coordinate(cp, &writer, &f.door, "file.write.result")),
    );
    f.append(
        MovementKind::InterfaceOutbound,
        &f.door,
        f.ctx.interface.as_str(),
        pin(&file, &f.ctx.blobs_dir),
        Some(coordinate(
            cp,
            &writer,
            f.ctx.interface.as_str(),
            "file.write.result",
        )),
    );
}
async fn install(
    f: &Fixture,
    mut ws: WebSocketStream<tokio::net::UnixStream>,
    call: Value,
    file: InterfaceArtifactFile,
) {
    let input = admit_call(f, &call, &file);
    let result = effect(f, &call, &input);
    admit_result(f, &call, result);
    ws.send(Message::Text(
        serde_json::to_string(&DeliveryVerdict::Admitted { seq: 1 })
            .unwrap()
            .into(),
    ))
    .await
    .unwrap();
}
async fn advance(f: &mut Fixture, handoff: &ProviderOutputHandoff, n: u64) {
    let output = emission(f, n);
    let drive = async {
        let (ws, call, file) = f.accept_write().await;
        install(f, ws, call, file).await;
        f.admit().await;
    };
    let (result, _) = tokio::join!(handoff.hand_observation(output, false), drive);
    result.unwrap();
}

#[tokio::test]
async fn two_declared_writers_recover_after_admitted_cas_failure() {
    tokio::time::timeout(Duration::from_secs(12),async {
        let (mut owner,mut peer)=pair(); let handoff=owner.handoff(); let peer_handoff=peer.handoff();
        advance(&mut owner,&handoff,0).await; advance(&mut peer,&peer_handoff,0).await;
        let output=emission(&owner,1); let mut attempt=Box::pin(handoff.hand_observation(output,false));
        let (mut ws,call,file)=tokio::select! {receipt=owner.accept_write()=>receipt,result=&mut attempt=>panic!("before conflict: {result:?}")};
        let input=admit_call(&owner,&call,&file);
        ws.send(Message::Text(serde_json::to_string(&DeliveryVerdict::Admitted {seq:1}).unwrap().into())).await.unwrap();
        advance(&mut peer,&peer_handoff,1).await;
        let peer_bytes=std::fs::read(&owner.run_path).unwrap();
        let failure=effect(&owner,&call,&input).unwrap_err();
        assert_eq!(failure.code,"file_write_incomplete");
        let evidence=&failure.details.as_ref().unwrap()["file_write"]["writes"];
        assert_eq!(evidence[0]["outcome"],"applied"); assert_eq!(evidence[1]["outcome"],"refused");
        assert_eq!(std::fs::read(&owner.run_path).unwrap(),peer_bytes);
        assert_eq!(effect(&owner,&call,&input).unwrap_err(),failure,"exact replay preserves the failure and never rewrites its prefix");
        admit_result(&owner,&call,Err(failure));
        drop(attempt);
        let restarted = owner.handoff();
        let attempt = restarted.recover_source_with_outcomes();
        let finish=async { let (ws,retry,file)=owner.accept_write().await; assert_ne!(retry["correlation_id"],call["correlation_id"]); install(&owner,ws,retry,file).await; owner.admit().await; };
        let (outcome,_)=tokio::join!(attempt,finish); let (_, outputs) = outcome.unwrap(); assert_eq!(outputs.len(),1);
        let final_record:Value=serde_json::from_slice(&std::fs::read(&owner.run_path).unwrap()).unwrap();
        let peer_record:Value=serde_json::from_slice(&peer_bytes).unwrap();
        assert_eq!(final_record["interface_state"]["peer"],peer_record["interface_state"]["peer"]);
        assert_eq!(final_record["interface_state"]["voice"]["authoring_generation"],2);
        let rebuilt=owner.handoff(); assert!(rebuilt.recover_source().await.is_ok());
        assert!(owner.arrivals.try_recv().is_err());
    }).await.expect("two writer recovery exceeded deadline");
}

#[tokio::test]
async fn outgoing_connection_survives_publication_conflict_and_cold_recovery() {
    tokio::time::timeout(Duration::from_secs(12), async {
        let (mut owner, _peer) = pair();
        let mut descriptor = serde_json::to_value(&owner.descriptor).unwrap();
        descriptor["interface_driver_config"]["private_config"]["context_output_labels"] = json!(["realtime.transcript"]);
        descriptor["interface_driver_config"]["private_config"]["session_snapshot_labels"] = json!(["realtime.transcript"]);
        owner.descriptor = serde_json::from_value(descriptor).unwrap();
        owner.ctx.descriptor_snapshot = Some(owner.descriptor.clone());
        let handoff = owner.handoff();
        let mut attempt = Box::pin(handoff.hand_final(emission(&owner, 1)));
        let (ws, staged, staged_file) = tokio::select! {
            receipt = owner.accept_write() => receipt, result = &mut attempt => panic!("before staging: {result:?}")
        };
        install(&owner, ws, staged, staged_file).await;
        let output = tokio::select! {
            output = owner.admit() => output, result = &mut attempt => panic!("before output admission: {result:?}")
        };
        let (mut ws, call, file) = tokio::select! {
            receipt = owner.accept_write() => receipt, result = &mut attempt => panic!("before publication: {result:?}")
        };
        let private_bytes = std::fs::read(file.manifest_path()).unwrap(); let private_payload = file.read_payload_bytes().unwrap();
        let before: Value = serde_json::from_str(&ordered_before(&call["arguments"]["writes"][1])).unwrap();
        let planned = ordered_postimage(&call["arguments"]["writes"][1]);
        let pending = &before["interface_state"]["voice"]["pending_handoff"];
        assert!(pending["context_publication"].is_array());
        assert_eq!(pending["source_movement_seq"], json!(owner.source_seq)); assert_eq!(pending["terminal"], true);
        assert_eq!(pending["items"], planned["interface_state"]["voice"]["pending_handoff"]["items"]);
        let input = admit_call(&owner, &call, &file);
        ws.send(Message::Text(serde_json::to_string(&DeliveryVerdict::Admitted { seq: 1 }).unwrap().into())).await.unwrap();
        let mut connected = before.clone();
        let route = json!({"name":"connected_output","role":"provider_interface_output","include_context":false,
            "interface_binding":binding("voice", Some("future.route")),"outbound_interface_bindings":[binding("peer_client", Some("future.route"))],"required_verified_artifacts":[]});
        let checkpoint = json!({"id":"connected_output","run":"run","trace":"trace:run:connected_output","epoch":0,
            "interface_binding":route["interface_binding"],"outbound_interface_bindings":route["outbound_interface_bindings"],"artifact_bindings":[],"required_verified_artifacts":[]});
        connected["checkpoints"].as_array_mut().unwrap().push(checkpoint.clone());
        connected["active_checkpoints"].as_array_mut().unwrap().push(json!("connected_output"));
        connected["interface_state"]["voice"]["frontier_checkpoint_ids"].as_array_mut().unwrap().push(json!("connected_output"));
        connected["interface_state"]["voice"]["run_authoring_binding"]["checkpoints"].as_array_mut().unwrap().push(route.clone());
        let mut connected_bytes = serde_json::to_vec_pretty(&connected).unwrap(); connected_bytes.push(b'\n');
        std::fs::write(&owner.run_path, &connected_bytes).unwrap();
        let failure = effect(&owner, &call, &input).unwrap_err();
        assert_eq!(failure.code, "file_write_incomplete");
        let receipts = &failure.details.as_ref().unwrap()["file_write"]["writes"];
        assert_eq!(receipts[0]["outcome"], "applied"); assert_eq!(receipts[1]["outcome"], "refused");
        assert_eq!(std::fs::read(&owner.run_path).unwrap(), connected_bytes);
        admit_result(&owner, &call, Err(failure)); drop(attempt);
        let restarted = owner.handoff();
        let finish = async {
            let (ws, retry, file) = owner.accept_write().await;
            assert_eq!(retry["correlation_id"], "provider-writer-run:run:2:3"); assert_eq!(ordered_before(&retry["arguments"]["writes"][1]).as_bytes(), connected_bytes);
            let rebased = ordered_postimage(&retry["arguments"]["writes"][1]);
            assert_eq!(rebased["interface_state"]["voice"]["pending_handoff"], planned["interface_state"]["voice"]["pending_handoff"]);
            install(&owner, ws, retry, file).await;
        };
        let (result, ()) = tokio::join!(restarted.recover_source_with_outcomes(), finish);
        let (recovery, outputs) = result.unwrap(); assert_eq!(recovery, octopus_run_authoring::ProviderSourceRecovery::Completed);
        assert_eq!(outputs.len(), 1); assert!(matches!(&outputs[0], ProviderHandoffOutcome::Admitted { artifact, .. } if artifact == &output));
        let final_bytes = std::fs::read(&owner.run_path).unwrap();
        let final_record: Value = serde_json::from_slice(&final_bytes).unwrap();
        assert!(final_record["checkpoints"].as_array().unwrap().contains(&checkpoint));
        assert!(final_record["active_checkpoints"].as_array().unwrap().contains(&json!("connected_output")));
        assert!(final_record["interface_state"]["voice"]["frontier_checkpoint_ids"].as_array().unwrap().contains(&json!("connected_output")));
        assert!(final_record["interface_state"]["voice"]["run_authoring_binding"]["checkpoints"].as_array().unwrap().contains(&route));
        assert_eq!(final_record["interface_state"]["peer"], before["interface_state"]["peer"]);
        assert_eq!(std::fs::read(file.manifest_path()).unwrap(), private_bytes); assert_eq!(file.read_payload_bytes().unwrap(), private_payload);
        let limits = owner.ctx.required_host_io_limits().unwrap();
        let evidence = octopus_run_authoring::CheckedRunEvidence { movements_path: &owner.ctx.movements_path, ledger_index_path: &owner.ctx.ledger_index_path,
            blobs_dir: &owner.ctx.blobs_dir, max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes };
        let writer = octopus_run_authoring::CheckedRunWriter { owner: InterfaceId::new("voice"), source: InterfaceId::new("source"), door: InterfaceId::new("door"),
            tool: "file.write".into(), call_label: "tool.call".into(), result_label: "file.write.result".into() };
        let error = octopus_run_authoring::checked_run_file_chain(&evidence, &owner.run_path, &owner.pinned, &final_bytes, owner.source_seq, &[writer]).unwrap_err();
        assert!(error.contains("complete admitted checked file.write chain"), "{error}");
        let ledger = std::fs::read(&owner.ctx.movements_path).unwrap(); let cold = owner.handoff();
        assert_eq!(cold.recover_source().await.unwrap(), octopus_run_authoring::ProviderSourceRecovery::Completed);
        assert!(cold.current_session_context_artifacts().await.iter().any(|entry| entry.source_artifact == output));
        assert_eq!(std::fs::read(&owner.ctx.movements_path).unwrap(), ledger); assert!(owner.arrivals.try_recv().is_err());
    }).await.expect("outgoing publication recovery exceeded deadline");
}
