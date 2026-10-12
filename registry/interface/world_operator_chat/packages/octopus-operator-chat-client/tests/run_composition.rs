use octopus_operator_chat_client::run_composition::*;
use octopus_runtime::run_record::RunRecord;
use serde_json::{Value, json};

#[path = "support/composition_fixture.rs"]
mod support;
use support::{fixture, write};

#[test]
fn selected_model_and_reasoning_effort_preserve_service_tier() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    for tier in ["priority", "ultrafast", "default"] {
        request.service_tier = Some(tier.into());
        let plan = plan_run_composition(&request).unwrap();
        assert_eq!(plan.model, request.model);
        assert_eq!(plan.reasoning_effort, request.reasoning_effort);
        assert_eq!(plan.service_tier.as_deref(), Some(tier));
        let provider: Value = serde_json::from_str(&plan.descriptor_writes.iter()
            .find(|write| write.tag == format!("interface:{}", plan.provider_interface_id)).unwrap().content).unwrap();
        assert_eq!(provider["interface_driver_config"]["private_config"]["service_tier"], tier);
        let retained: RunCompositionPlan = serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
        assert_eq!(retained.service_tier, plan.service_tier);
    }
    for tier in ["", " priority", "priority\t"] {
        request.service_tier = Some(tier.into());
        assert!(plan_run_composition(&request).unwrap_err().contains("service tier"));
    }
    request.service_tier = None;
    let plan = plan_run_composition(&request).unwrap();
    assert_eq!(plan.service_tier, None);
}

#[test]
fn private_system_composition_is_fresh_exact_and_checked_under_policy() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    let parent_before = request.admitted_parent_record.clone();
    let plan = plan_run_composition(&request).unwrap();
    assert!(
        plan.required_binding_updates[0]
            .required_artifact_bindings
            .iter()
            .any(|binding| binding.id == "role-system_worker_template")
    );
    let retained: RunCompositionPlan =
        serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
    assert_eq!(retained.run_write.content, plan.run_write.content);
    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        serde_json::to_value(plan_run_composition(&request).unwrap()).unwrap()
    );
    assert_eq!(request.admitted_parent_record, parent_before);
    assert!(!request.roots.run_records_root.exists());
    assert!(!request.roots.interface_private_state_root.exists());
    assert_eq!(
        plan.receive_interface_id,
        request.communication.receive_interface_id
    );
    assert_ne!(
        plan.worker_interface_id,
        request.worker_template_interface_id
    );
    assert!(
        plan.descriptor_writes
            .iter()
            .all(|write| write.expected_before_hash.is_none() && !write.path.exists())
    );
    assert!(
        plan.descriptor_writes
            .iter()
            .all(|write| write.path.file_name().unwrap() != "octopus_os.json")
    );
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    assert_eq!(record.interface_state.len(), 1);
    let templates =
        record.interface_state[&plan.provider_interface_id.clone().into()]["run_authoring_binding"]
            ["checkpoints"]
            .as_array()
            .unwrap();
    let call = templates.iter().find(|row| row["role"] == "authoring_call").unwrap();
    assert_eq!(call["required_verified_artifacts"], json!([{
        "mark":request.authoring.tool_call_mark,
        "artifact_label":request.authoring.tool_call_label,
        "verifier":request.authoring.tool_call_verifier,
        "params":{"tools":[plan.authoring.tool_id],"metadata_matches":[{
            "payload":"/arguments/expected_before_hash","metadata":"/runtime/run_record_hash"
        }]}
    }]));
    assert!(record.checkpoints.iter().any(|cp| serde_json::to_value(&cp.required_verified_artifacts).unwrap() == call["required_verified_artifacts"]));
    assert!(templates.iter().filter(|row| row["role"] == "provider_tool_output")
        .all(|row| row["required_verified_artifacts"][0]["params"].get("metadata_matches").is_none()));
    assert!(
        templates
            .iter()
            .any(
                |row| row["interface_binding"]["interface"] == plan.receive_interface_id
                    && row["role"] == "stable"
                    && row["include_context"] == true
            )
    );
    assert_eq!(
        record
            .checkpoints
            .iter()
            .filter(|cp| cp
                .inbound_interface()
                .is_some_and(|id| id.as_str() == plan.receive_interface_id))
            .count(),
        1
    );
    assert!(!plan.run_write.content.contains("MUST NOT COPY"));
    assert!(!plan.run_write.content.contains("private-transcript"));
    assert!(!plan.run_write.content.contains("conversation"));
    assert!(!plan.run_write.content.contains("world_operator_chat"));
    assert!(
        !plan
            .run_write
            .content
            .contains("role-system_worker_unselected")
    );
    let input = record
        .checkpoints
        .iter()
        .find(|cp| {
            cp.inbound_interface()
                .is_some_and(|id| id.as_str() == plan.receive_interface_id)
        })
        .unwrap();
    assert!(
        input
            .artifact_bindings
            .iter()
            .any(
                |binding| binding.id.as_str() == "role-system_worker_template"
                    && binding.access.read
            )
    );
    assert!(
        input
            .artifact_bindings
            .iter()
            .any(
                |binding| binding.id.as_str() == "role-system_worker_parent" && binding.access.read
            )
    );
    assert_eq!(plan.seed.payload, request.seed.payload);
    assert_ne!(
        plan.authoring.capability_tools["file.write"],
        plan.authoring.tool_id
    );
    let writer: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| {
                write
                    .path
                    .ends_with(format!("{}/tool.json", plan.authoring.tool_id))
            })
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(
        writer["process"]["authority"]["write_argument_paths"][0]["within_root_slots"],
        json!(["run_records_root"])
    );
    assert_eq!(
        plan.required_binding_updates[0]
            .outbound_binding
            .interface
            .as_str(),
        plan.receive_interface_id
    );
    assert!(
        plan.required_policy_updates
            .iter()
            .any(|update| update.write.path.ends_with("ee_existence.json"))
    );
    for update in &plan.required_policy_updates {
        assert_eq!(
            update.write.expected_before_hash.as_ref().unwrap(),
            &blake3::hash(&std::fs::read(&update.write.path).unwrap())
                .to_hex()
                .to_string()
        );
    }
    let provider: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| {
                write
                    .path
                    .ends_with(format!("{}/interface.json", plan.provider_interface_id))
            })
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(
        provider["interface_policy"]["values"]["models"],
        json!(["fixture-a"])
    );
    for update in &plan.required_policy_updates {
        std::fs::write(&update.write.path, &update.write.content).unwrap();
    }
    for write in &plan.descriptor_writes {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
        std::fs::write(&write.path, &write.content).unwrap();
    }
    let topology = octopus_world_terminal_client::form_capability_topology(
        &request.roots.registry_root,
        &octopus_world_terminal_client::CapabilityFormationRequest::from_live_chain(
            plan.worker_interface_id.clone(),
            octopus_world_terminal_client::resolve_live_ee_policy_chain(
                &request.roots.ee_instances_root,
                &request.ee_id.clone().into(),
            )
            .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(
        topology.interface_policies[&plan.provider_interface_id.clone().into()].context["models"],
        json!(["fixture-a"])
    );
    let retry = plan_run_composition(&request).unwrap();
    assert_eq!(retry.run_id, plan.run_id);
    assert_eq!(retry.run_write.content, plan.run_write.content);
    request.model = "fixture-b".into();
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("selected model")
    );
    request.model = "fixture-a".into();
    request.identity_seed = "call/second".into();
    request.communication.receive_interface_id = "system_worker_private_second".into();
    let second = plan_run_composition(&request).unwrap();
    assert_ne!(plan.run_id, second.run_id);
    assert_ne!(plan.provider_interface_id, second.provider_interface_id);
    assert_ne!(plan.worker_interface_id, second.worker_interface_id);
    request.communication.return_interface_ids = vec![request.provider_interface_id.clone()];
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("System interface")
    );
}

#[test]
fn selected_outputs_keep_recorded_activity_out_of_peer_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    request.communication.output_selection = Some(CompositionOutputSelection {
        return_labels: vec!["interface.artifact.output".into(), "turn.error".into()],
        record_labels: vec!["turn.stream".into(), "turn.completed".into()],
    });
    assert!(plan_run_composition(&request).unwrap_err().contains("must declare client_projection_output_labels"));
    let provider_path = request.roots.registry_root.join("interface/world_provider_fixture/interface.json");
    let mut provider: serde_json::Value = serde_json::from_slice(&std::fs::read(&provider_path).unwrap()).unwrap();
    provider["interface_driver_config"]["private_config"]["client_projection_output_labels"] =
        json!(["interface.artifact.output", "turn.error", "turn.stream", "turn.completed", "turn.input.acknowledged"]);
    std::fs::write(provider_path, serde_json::to_vec(&provider).unwrap()).unwrap();
    assert!(plan_run_composition(&request).unwrap_err().contains("turn.input.acknowledged"));
    request.communication.output_selection.as_mut().unwrap().record_labels.push("turn.input.acknowledged".into());
    let plan = plan_run_composition(&request).unwrap();
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    for (label, destinations) in [("interface.artifact.output", vec!["system_worker_parent"]), ("turn.stream", vec![]), ("turn.input.acknowledged", vec![])] {
        let matches = record.active_checkpoints.iter().filter_map(|id| record.checkpoint(id))
            .filter(|cp| cp.inbound_interface().is_some_and(|id| id.as_str() == plan.provider_interface_id))
            .filter(|cp| cp.required_verified_artifacts.is_empty())
            .filter(|cp| cp.interface_binding.as_ref().unwrap().admits(label)).collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].outbound_interfaces().map(|id| id.as_str()).collect::<Vec<_>>(), destinations);
    }
    request.communication.output_selection.as_mut().unwrap().record_labels.push("turn.error".into());
    assert!(plan_run_composition(&request).unwrap_err().contains("disjoint"));
}

#[test]
fn client_and_return_bindings_are_only_the_callers_explicit_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    request.communication.return_interface_ids.clear();
    let plan = plan_run_composition(&request).unwrap();
    assert!(plan.return_interface_ids.is_empty());
    assert!(!plan.run_write.content.contains("role-system_worker_parent"));
    write(
        dir.path(),
        "registry/interface/world_client/interface.json",
        json!({"id":"world_client","interface_type":"world","role":"admin","direction":"bidirectional","source_names":["client"],"interface_driver_ref":"app_server"}),
    );
    request.communication.client_interface_id = Some("world_client".into());
    let plan = plan_run_composition(&request).unwrap();
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    assert!(record.checkpoints.iter().any(|cp| {
        cp.inbound_interface()
            .is_some_and(|id| id.as_str() == "world_client")
    }));
    assert!(record.checkpoints.iter().any(|cp| {
        cp.outbound_interfaces()
            .any(|id| id.as_str() == "world_client")
    }));
    assert!(plan.descriptor_writes.iter().all(|write| {
        !write
            .path
            .starts_with(request.roots.registry_root.join("interface/world_client"))
    }));
    let state = request
        .roots
        .interface_private_state_root
        .join(&plan.provider_interface_id);
    std::fs::create_dir_all(&state).unwrap();
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("existing private state")
    );
    std::fs::remove_dir(&state).unwrap();
    request.communication.receive_interface_id = request.worker_template_interface_id.clone();
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("private worker pin")
    );
}

#[test]
fn selected_client_template_authors_a_private_client_without_reusing_the_callers_chat() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = support::client_fixture(dir.path());
    let plan = plan_run_composition(&request).unwrap();
    let record: Value = serde_json::from_str(&plan.run_write.content).unwrap();
    let (client, state) = record["interface_state"].as_object().unwrap().iter().find(|(_, state)| state["client_surface_role"] == "task").unwrap();
    assert_ne!(client, "world_operator_chat");
    assert_ne!(client, &plan.receive_interface_id);
    assert_eq!(state["provider_runtime_interface"], plan.provider_interface_id);
    assert!(plan.required_interface_ids.contains(client));
    let sidecar: Value = serde_json::from_str(&plan.descriptor_writes.iter().find(|write| write.path.ends_with(format!("{client}/octopus_os.json"))).unwrap().content).unwrap();
    assert_eq!(sidecar["owner_interface"], *client);
    assert_eq!(sidecar["presentation"], json!({"name":"Context check", "projects":[]}));
    assert_eq!(sidecar["authoring_profile"]["worker_choice_interface"], request.worker_template_interface_id);
    assert!(record["checkpoints"].as_array().unwrap().iter().any(|cp| cp["interface_binding"]["interface"] == *client));
    assert!(!plan.run_write.content.contains("MUST NOT COPY"));
    for invalid in ["", " name", "name\ninside", &"x".repeat(81)] {
        request.communication.client_title = Some(invalid.into());
        assert!(plan_run_composition(&request).unwrap_err().contains("client_title"));
    }
    request.communication.client_title = Some("Context check".into());
    request.communication.client_template_interface_id = None;
    assert!(plan_run_composition(&request).unwrap_err().contains("client_title"));
    request.communication.client_template_interface_id = Some("world_operator_chat".into());
    request.communication.client_interface_id = Some("world_operator_chat".into());
    assert!(plan_run_composition(&request).unwrap_err().contains("not both"));
}

#[tokio::test]
async fn input_revision_cli_preserves_payload_intent_and_requires_checked_writer_results() {
    use futures::{SinkExt, StreamExt};
    use octopus_app_server::{SharedBlobHandle, SpineRootConfig, admit_interface_artifact_arrival, build_embedded_spine};
    use octopus_descriptors::InterfaceId;
    use octopus_interface_transport::{InterfaceActivationArrival, InterfaceHostIoLimits, InterfaceLiftContext};
    use octopus_interfaces::{DeliveryVerdict, InterfaceArtifactFile, InterfaceArtifactManifest, InterfaceInvocationEnvelope};
    use octopus_ledger::{ContentHash, Endpoint, LiveMovementLog, MovementKind, MovementLog, Seq, SharedBlobs};
    use octopus_ledger_store::{DurableBlobs, DurableLog};
    use octopus_operator_chat_client::{OperatorWriterRunPlanRequest, plan_operator_writer_runs};
    use octopus_provider_run_authoring::{ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession};
    use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact};
    use octopus_world_pyecho_life_runtime::LedgerTailReader;
    use std::{collections::BTreeMap, io::Write, path::Path, process::{Command, Stdio}, sync::{Arc, Mutex}};

    fn cli(flag: &str, request: &Value) -> Result<Value, String> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_octopus-operator-chat-topology"))
            .arg(flag).env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(&serde_json::to_vec(request).unwrap()).unwrap();
        let output = child.wait_with_output().unwrap();
        if output.status.success() { Ok(serde_json::from_slice(&output.stdout).unwrap()) }
        else { Err(String::from_utf8(output.stderr).unwrap()) }
    }

    fn transport_call(root: &Path, ready: &Value) -> InterfaceArtifactFile {
        let manifest: InterfaceArtifactManifest = serde_json::from_value(ready["manifest"].clone()).unwrap();
        InterfaceArtifactFile::store_manifest_bounded(root, &serde_json::to_vec(&manifest).unwrap(), 1<<20, 1<<26).unwrap()
    }

    struct Boundary {
        runtime: octopus_app_server::EmbeddedRuntime,
        log: LiveMovementLog<DurableLog>,
        blobs: SharedBlobHandle,
        ctx: InterfaceLiftContext,
        arrivals: tokio::sync::mpsc::Receiver<InterfaceActivationArrival>,
        socket: tokio::net::UnixListener,
        author: InterfaceId,
        door: InterfaceId,
        writes: usize,
    }
    impl Boundary {
        fn file(&self, label: &str, body: &Value) -> InterfaceArtifactFile {
            InterfaceArtifactFile::write(self.ctx.var_dir.join("emitted"), label, &serde_json::to_vec(body).unwrap()).unwrap()
        }
        async fn admit(&mut self, file: &InterfaceArtifactFile, source: &InterfaceId) -> DeliveryVerdict {
            let captured = file.clone().capture_bounded(1<<20, 1<<26).unwrap();
            match admit_interface_artifact_arrival(&self.runtime, &mut self.log, &mut self.blobs, &captured, source).await {
                Ok(outcome) => DeliveryVerdict::Admitted { seq: outcome.admission.records.first().unwrap().arrival_movement.seq.0 },
                Err(error) => error.verdict(),
            }
        }
        fn input(&self, file: &InterfaceArtifactFile, interface: &InterfaceId) -> (u64, InterfaceInvocationEnvelope) {
            let mut ctx = self.ctx.clone(); ctx.interface = interface.clone();
            let mut reader = LedgerTailReader::open_named(&ctx, "revision-test").unwrap();
            let hash = ContentHash::of_bytes(&std::fs::read(file.manifest_path()).unwrap());
            let rows = reader.read_new().unwrap();
            let row = rows.iter().rev().find(|row| row.movement.artifact.as_ref().is_some_and(|a| a.content == hash)).unwrap();
            (row.movement.seq.0, reader.envelope(row).unwrap())
        }
        async fn effect(&mut self, file: &InterfaceArtifactFile) -> (DeliveryVerdict, InterfaceArtifactFile) {
            let author = self.author.clone();
            let verdict = self.admit(file, &author).await;
            assert!(matches!(verdict, DeliveryVerdict::Admitted { .. }), "{verdict:?}");
            let (_, envelope) = self.input(file, &self.door);
            let call: ToolCallArtifact = serde_json::from_slice(&envelope.read_artifact_payload().unwrap()).unwrap();
            let body = octopus_system_room_env::execute_file_write_result(&envelope, &call.arguments, &["system_room".into()],
                &BTreeMap::from([("run_records_root".into(),self.ctx.root_slots["run_records_root"].clone())]), &self.ctx.var_dir.join("effects"), 1<<26).unwrap();
            self.writes += 1;
            let result = ToolResultArtifact::success(&call.tool, call.correlation_id, body);
            (verdict, self.file(&result.label, &json!(result)))
        }
        async fn finish(&mut self, future: impl std::future::Future<Output = Result<(), String>>) {
            let (finished, mut done) = tokio::sync::oneshot::channel();
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                let (result, ()) = tokio::join!(async { let result = future.await; let _ = finished.send(()); result }, async {
                    loop { tokio::select! {
                        _ = &mut done => break,
                        arrival = self.arrivals.recv() => {
                            let arrival = arrival.unwrap(); let provider = self.ctx.interface.clone();
                            let verdict = self.admit(&arrival.artifact, &provider).await;
                            arrival.verdict_tx.unwrap().send(verdict).unwrap();
                        }
                        connection = self.socket.accept() => {
                            let mut ws = tokio_tungstenite::accept_async(connection.unwrap().0).await.unwrap();
                            let bytes = ws.next().await.unwrap().unwrap().into_data();
                            let path = self.ctx.var_dir.join("incoming.json"); std::fs::write(&path, bytes).unwrap();
                            let (verdict,result) = self.effect(&InterfaceArtifactFile::new(path).unwrap()).await;
                            let door = self.door.clone();
                            assert!(matches!(self.admit(&result, &door).await, DeliveryVerdict::Admitted { .. }));
                            ws.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&verdict).unwrap().into())).await.unwrap();
                        }
                    }}
                });
                result.unwrap();
            }).await.unwrap();
        }
    }

    for separate_writer in [false, true] {
        let temp = tempfile::Builder::new().prefix("oct-rev-").tempdir_in("/tmp").unwrap(); let root = temp.path().canonicalize().unwrap();
        let request = support::client_fixture(&root); let plan = plan_run_composition(&request).unwrap();
        for edit in plan.descriptor_writes.iter().chain(plan.required_policy_updates.iter().map(|u| &u.write)).chain(std::iter::once(&plan.run_write)) {
            std::fs::create_dir_all(edit.path.parent().unwrap()).unwrap(); std::fs::write(&edit.path, &edit.content).unwrap();
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry/verifier/tool_call_allowed");
        let target = request.roots.registry_root.join("verifier/tool_call_allowed"); std::fs::create_dir_all(&target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap(); if entry.file_type().unwrap().is_file() { std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap(); }
        }
        if separate_writer {
            use sha2::Digest;
            let current = std::fs::read(&plan.run_write.path).unwrap();
            for edit in plan_operator_writer_runs(&OperatorWriterRunPlanRequest { run_records_root:request.roots.run_records_root.clone(),run_id:plan.run_id.clone(),expected_before_sha256:format!("{:x}",sha2::Sha256::digest(current)) }).unwrap() {
                std::fs::write(edit.path, edit.content).unwrap();
            }
        }
        let current = || std::fs::read_to_string(&plan.run_write.path).unwrap();
        let initial: Value = serde_json::from_str(&current()).unwrap();
        let client: InterfaceId = initial["interface_state"].as_object().unwrap().iter().find(|(_,state)| state["client_surface_role"] == "task").unwrap().0.as_str().into();
        let provider: InterfaceId = plan.provider_interface_id.clone().into();
        let author: InterfaceId = plan.authoring.source_interface_id.clone().into(); let door: InterfaceId = plan.authoring.room_interface_id.clone().into();
        let blobs: SharedBlobs = Arc::new(Mutex::new(DurableBlobs::open(root.join("ledger/blobs")).unwrap()));
        let runtime = build_embedded_spine(SpineRootConfig { registry_root:request.roots.registry_root.clone(),run:plan.run_id.clone().into(),blobs:blobs.clone(),
            interface_artifact_root:root.join("artifacts"),run_records_root:request.roots.run_records_root.clone(),ee_instances_root:request.roots.ee_instances_root.clone() }).await.unwrap();
        assert!(runtime.worker_interface_policies.contains_key(&plan.worker_interface_id.clone().into()), "planned worker {}", std::fs::read_to_string(request.roots.registry_root.join("interface").join(&plan.worker_interface_id).join("policy.json")).unwrap());
        let log = LiveMovementLog::new(DurableLog::open(root.join("ledger/movements.jsonl")).unwrap());
        let (sender, arrivals) = tokio::sync::mpsc::channel(16);
        let descriptor = serde_json::from_slice(&std::fs::read(request.roots.registry_root.join("interface").join(provider.as_str()).join("interface.json")).unwrap()).unwrap();
        let mut ctx = InterfaceLiftContext::unprivileged(provider.clone(), request.roots.registry_root.join("interface").join(provider.as_str()),Some(descriptor),root.join("provider"),
            root.join("ledger/movements.jsonl"),root.join("ledger/blobs"),root.join("ledger/ledger-index.sqlite"),
            BTreeMap::from([("run_records_root".into(), request.roots.run_records_root.clone())]),sender,Default::default());
        std::fs::create_dir(&ctx.var_dir).unwrap();
        ctx.root_slots.insert("interface_hand_in_root".into(), root.join("hand-in")); ctx.root_slots.insert("app_server_control_root".into(), root.join("control"));
        ctx.host_io_limits = Some(InterfaceHostIoLimits {max_control_frame_bytes:1<<20,max_manifest_bytes:1<<20,max_payload_bytes:1<<26,max_stderr_bytes:1024});
        let socket_path = octopus_app_server_interface::interface_control_socket_path_from_root(&root.join("control"), &author);
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap(); let socket = tokio::net::UnixListener::bind(socket_path).unwrap();
        let mut b = Boundary {runtime,log,blobs:SharedBlobHandle::new(blobs.clone()),ctx,arrivals,socket,author,door:door.clone(),writes:0};
        let attachment = b.file("fixture.payload", &json!({"text":"exact selected payload"}));
        let selected = json!([attachment.load_into_blob(&mut *blobs.lock().unwrap()).unwrap().artifact]);
        let mut scope = json!({"registry_root":request.roots.registry_root,"run_path":plan.run_write.path,"ledger_root":root.join("ledger"),
            "artifact_root":root.join("client"),"provider":provider,"source":client,"key":"first-payload","selected_payloads":selected});
        let prepare = |scope: &Value, operation: Value, before: &str| {
            let mut command = scope.clone(); command.as_object_mut().unwrap().remove("selected_payloads");
            command["operation"] = operation; command["expected_before_content"] = json!(before);
            cli("--prepare-input-revision", &command)
        };
        let verify = |scope: &Value| cli("--verify-input-revision", scope);
        let operation = json!({"kind":"payloads","selected":selected});
        let before = current();
        assert_eq!(prepare(&scope,json!({"kind":"owner_inputs","desired_inputs":[]}),&before).unwrap()["status"], "unchanged");
        assert_eq!(prepare(&scope,operation.clone(),&before).unwrap()["status"], "prepared");
        assert_eq!(current(),before);
        let ready = verify(&scope).unwrap(); assert_eq!(ready["status"], "ready");
        let mut wrong_source = scope.clone(); wrong_source["source"] = json!(b.author);
        assert!(verify(&wrong_source).unwrap_err().contains("exact client"));
        let mut wrong_selection = scope.clone(); wrong_selection["selected_payloads"] = json!([]);
        assert!(verify(&wrong_selection).unwrap_err().contains("selected payload"));
        let call = transport_call(&root.join("wire"),&ready);
        let (_,returned) = b.effect(&call).await; assert_eq!(b.writes,1);
        assert_ne!(current(),before);
        assert_eq!(verify(&scope).unwrap()["status"], "awaiting_result");
        assert_eq!(prepare(&scope,operation,&before).unwrap()["status"], "prepared");
        assert_eq!(verify(&scope).unwrap()["status"], "awaiting_result");
        assert!(matches!(b.admit(&returned, &door).await, DeliveryVerdict::Admitted { .. }));
        let installed = verify(&scope).unwrap(); assert_eq!(installed["status"], "installed");
        assert_eq!(verify(&scope).unwrap(), installed); assert_eq!(b.writes,1);
        let original: Value = serde_json::from_str(&before).unwrap(); let changed: Value = serde_json::from_str(&current()).unwrap();
        assert_eq!(changed["interface_state"][client.as_str()],original["interface_state"][client.as_str()]);
        assert_eq!(changed["interface_state"][provider.as_str()]["run_authoring_binding"]["payload_artifact_bindings"],selected);
        let message = b.file("operator.message", &json!({"text":"fixture turn","message_id":"settle"}));
        assert!(matches!(b.admit(&message,&client).await, DeliveryVerdict::Admitted { .. }));
        scope["key"] = json!("admitted-initial-definition");
        assert_eq!(prepare(&scope,json!({"kind":"owner_inputs","desired_inputs":[]}),&current()).unwrap()["status"], "deferred");
        let (seq,input) = b.input(&message,&provider); let pinned = b.ctx.pinned_run_record_bytes(&input).unwrap();
        let handoff = ProviderHandoffSession::new(&b.ctx).unwrap().from_invocation(&b.ctx,b.ctx.descriptor_snapshot.as_ref().unwrap(),&plan.run_id.clone().into(),&input,seq,&pinned).unwrap();
        let output = b.file("interface.artifact.output", &json!({"text":"fixture settled"}));
        b.finish(async { handoff.hand_final(ProviderEmission {artifact:output,route:ProviderEmissionRoute::ClientProjection}).await.map(|_| ()) }).await;
        let settled = current(); let writes = b.writes;
        scope["key"] = json!("ordinary-rearm");
        assert_eq!(prepare(&scope,json!({"kind":"rearm"}),&settled).unwrap()["status"], "prepared");
        let ready = verify(&scope).unwrap(); assert_eq!(ready["status"], "ready");
        let (_,result) = b.effect(&transport_call(&root.join("wire"),&ready)).await;
        assert!(matches!(b.admit(&result,&door).await,DeliveryVerdict::Admitted{..}));
        assert_eq!(verify(&scope).unwrap()["status"], "installed");
        assert_eq!(verify(&scope).unwrap()["status"], "installed"); assert_eq!(b.writes,writes+1);
        let after: Value = serde_json::from_str(&current()).unwrap(); let settled: Value = serde_json::from_str(&settled).unwrap();
        assert_eq!(after["interface_state"][provider.as_str()]["pending_handoff"],settled["interface_state"][provider.as_str()]["pending_handoff"]);
        scope["key"] = json!("wrong-correlation");
        assert_eq!(prepare(&scope,json!({"kind":"rearm"}),&current()).unwrap()["status"], "prepared");
        let ready = verify(&scope).unwrap();
        let (_,result) = b.effect(&transport_call(&root.join("wire"),&ready)).await;
        let mut wrong: Value = serde_json::from_slice(&result.read_payload_bytes().unwrap()).unwrap(); wrong["correlation_id"] = json!("not-the-proposed-write");
        let wrong = b.file(wrong["label"].as_str().unwrap(), &wrong); b.admit(&wrong,&door).await;
        assert_eq!(verify(&scope).unwrap()["status"], "awaiting_result");
        let call_count = b.log.read_from(Seq(0)).iter().filter(|row| row.kind == MovementKind::InterfaceOutbound && row.from == Endpoint::Interface(b.author.clone()) && row.to == Endpoint::Interface(door.clone())).count();
        assert_eq!(call_count,b.writes);
    }
}
