use super::*;
use futures::{SinkExt, StreamExt};
use octopus_app_server::{SharedBlobHandle, SpineRootConfig, admit_interface_artifact_arrival, build_embedded_spine};
use octopus_interface_transport::{InterfaceActivationArrival, InterfaceHostIoLimits, InterfaceLiftContext};
use octopus_interfaces::{DeliveryVerdict, InterfaceArtifactFile, InterfaceInvocationEnvelope};
use octopus_ledger::{Endpoint, LiveMovementLog, MovementKind, MovementLog, Seq, SharedBlobs};
use octopus_ledger_store::{DurableBlobs, DurableLog};
use octopus_provider_run_authoring::{ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession, ProviderSourceRecovery};
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Boundary {
    runtime: octopus_app_server::EmbeddedRuntime,
    log: LiveMovementLog<DurableLog>,
    blobs: SharedBlobHandle,
    ctx: InterfaceLiftContext,
    arrivals: tokio::sync::mpsc::Receiver<InterfaceActivationArrival>,
    socket: tokio::net::UnixListener,
    author: InterfaceId,
    door: InterfaceId,
    roots: BTreeMap<String, PathBuf>,
}

impl Boundary {
    async fn admit(&mut self, file: &InterfaceArtifactFile, interface: &InterfaceId) -> DeliveryVerdict {
        let file = file.clone().capture_bounded(1<<20, 1<<26).unwrap();
        match admit_interface_artifact_arrival(&self.runtime, &mut self.log, &mut self.blobs, &file, interface).await {
            Ok(outcome) => DeliveryVerdict::Admitted { seq: outcome.admission.records.first().unwrap().arrival_movement.seq.0 },
            Err(error) => { eprintln!("admission {}: {error}", interface); error.verdict() },
        }
    }

    fn input(&self, interface: &InterfaceId, artifact: &InterfaceArtifactFile) -> (u64, InterfaceInvocationEnvelope) {
        let mut ctx = self.ctx.clone(); ctx.interface = interface.clone();
        let mut reader = LedgerTailReader::open_named(&ctx, "fixture-read").unwrap();
        let rows = reader.read_new().unwrap();
        let row = rows.iter().rev().find(|row| {
            row.movement.artifact.as_ref().unwrap().id.as_str().starts_with(&format!("{}:", artifact.read_manifest().unwrap().label))
        }).unwrap();
        (row.movement.seq.0, reader.envelope(row).unwrap())
    }

    async fn drive<T>(&mut self, future: impl std::future::Future<Output = Result<T, String>>) -> T {
        let (finished, mut done) = tokio::sync::oneshot::channel();
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let (result, ()) = tokio::join!(async { let result = future.await; let _ = finished.send(()); result }, async {
            loop { tokio::select! {
                _ = &mut done => break,
                arrival = self.arrivals.recv() => {
                    let arrival = arrival.unwrap();
                    let verdict = self.admit(&arrival.artifact, &self.ctx.interface.clone()).await;
                    arrival.verdict_tx.unwrap().send(verdict).unwrap();
                }
                connection = self.socket.accept() => {
                    let mut ws = tokio_tungstenite::accept_async(connection.unwrap().0).await.unwrap();
                    let bytes = ws.next().await.unwrap().unwrap().into_data();
                    let path = self.ctx.var_dir.join("authoring-arrival.json");
                    std::fs::write(&path, &bytes).unwrap();
                    let file = InterfaceArtifactFile::new(&path).unwrap();
                    let verdict = self.admit(&file, &self.author.clone()).await;
                    assert!(matches!(verdict, DeliveryVerdict::Admitted { .. }), "{verdict:?}");
                    let (_, envelope) = self.input(&self.door, &file);
                    let call: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&envelope.read_artifact_payload().unwrap()).unwrap();
                    let body = octopus_system_room_env::execute_file_write_result(&envelope, &call.arguments,
                        &["system_room".into()], &self.roots, &self.ctx.var_dir.join("effects"), self.ctx.host_io_limits.as_ref().unwrap().max_payload_bytes).unwrap();
                    let result = octopus_tool_handlers::ToolResultArtifact::success(&call.tool, call.correlation_id, body);
                    let result = InterfaceArtifactFile::write(self.ctx.var_dir.join("results"), &result.label, &serde_json::to_vec(&result).unwrap()).unwrap();
                    assert!(matches!(self.admit(&result, &self.door.clone()).await, DeliveryVerdict::Admitted { .. }));
                    ws.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&verdict).unwrap().into())).await.unwrap();
                }
            }} });
            result.unwrap()
        }).await.unwrap()
    }
}

#[tokio::test]
async fn home_stop_rearms_through_real_admission_and_file_effect_across_restart() {
    for surface in ["home", "task"] {
    let temp = tempfile::tempdir().unwrap(); let root = temp.path();
    let registry = root.join("registry"); let ees = root.join("ees"); let runs = root.join("runs");
    tests::copy_planner_descriptors(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry"), &registry);
    std::fs::create_dir(&ees).unwrap(); std::fs::create_dir(&runs).unwrap(); tests::boot_operator_space_ee_instances(&ees);
    let request: OperatorChatPlanRequest = serde_json::from_value(json!({
        "registry_root":registry,"ee_instances_root":ees,"run_records_root":runs,"interface_private_state_root":root.join("interface-private-state"),"write_hash_algorithm":"sha256",
        "chat_id":"home_controls","provider_interface_id":"world_provider_codex","ee_id":"ee_existence",
        "worker_interface_id":"system_worker_operator_home","client_surface_role":surface,"cycle":"initial",
        "selected_worker_environment_kinds":["system_room"],
        "conversation":{"ledger_offset":0,"artifact_bindings":[],"pinned_artifact_bindings":[],"pinned_context_items":[],"continued_from":null}
    })).unwrap();
    let plan = plan_operator_chat_topology(&request).unwrap();
    for write in plan.interface_writes.iter().chain(&plan.run_writes) {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap(); std::fs::write(&write.path, &write.content).unwrap();
    }
    let path = runs.join(format!("{}.json", plan.run_id)); std::fs::write(&path, &plan.run_record_content).unwrap();
    let record: Value = serde_json::from_str(&plan.run_record_content).unwrap();
    let voice_path = record["interface_state"][&plan.chat_interface_id]["voice_context"]["run_id"].as_str().map(|id| runs.join(format!("{id}.json")));
    let voice = voice_path.as_ref().map(|path| std::fs::read(path).unwrap());
    assert_eq!(voice.is_some(), surface == "home");
    assert!(record["interface_state"][&plan.model_interface_id]["run_authoring_binding"]["writer_run"].is_object());
    let blobs: SharedBlobs = Arc::new(Mutex::new(DurableBlobs::open(root.join("ledger/blobs")).unwrap()));
    let runtime = build_embedded_spine(SpineRootConfig { registry_root:registry.clone(), run:plan.run_id.clone().into(), blobs:blobs.clone(),
        interface_artifact_root:root.join("artifacts"), run_records_root:runs.clone(), ee_instances_root:ees }).await.unwrap();
    let log = LiveMovementLog::new(DurableLog::open(root.join("ledger/movements.jsonl")).unwrap());
    let (sender, arrivals) = tokio::sync::mpsc::channel(16);
    let provider: InterfaceId = plan.model_interface_id.clone().into(); let client: InterfaceId = plan.chat_interface_id.clone().into();
    let descriptor = serde_json::from_slice(&std::fs::read(registry.join("interface").join(provider.as_str()).join("interface.json")).unwrap()).unwrap();
    let roots = BTreeMap::from([("run_records_root".into(), runs.clone())]);
    let mut ctx = InterfaceLiftContext::unprivileged(provider.clone(), registry.join("interface").join(provider.as_str()), Some(descriptor),
        root.join("provider"), root.join("ledger/movements.jsonl"), root.join("ledger/blobs"), root.join("ledger/index.sqlite"), roots.clone(), sender, Default::default());
    std::fs::create_dir(&ctx.var_dir).unwrap();
    ctx.root_slots.insert("interface_hand_in_root".into(), root.join("hand-in"));
    ctx.root_slots.insert("app_server_control_root".into(), root.join("control"));
    ctx.host_io_limits = Some(InterfaceHostIoLimits {max_control_frame_bytes:1<<20,max_manifest_bytes:1<<20,max_payload_bytes:1<<26,max_stderr_bytes:1024});
    let author = record["interface_state"][provider.as_str()]["run_authoring_binding"]["authoring_source_interface"].as_str().unwrap().into();
    let socket_path = octopus_app_server_interface::interface_control_socket_path_from_root(&root.join("control"), &author);
    std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap(); let socket = tokio::net::UnixListener::bind(socket_path).unwrap();
    let mut b = Boundary {runtime,log,blobs:SharedBlobHandle::new(blobs),ctx,arrivals,socket,author,door:plan.authoring_environment_interface_id.into(),roots};
    let artifact = |label: &str, payload: Value| InterfaceArtifactFile::write(root.join("provider/emitted"), label, &serde_json::to_vec(&payload).unwrap()).unwrap();
    let emission = |file| ProviderEmission { artifact:file, route:ProviderEmissionRoute::ClientProjection };
    let mut foreground: Option<octopus_provider_run_authoring::ProviderOutputHandoff> = None;
    for attempt in 0..3 {
        let stop = artifact("operator.turn.control", json!({"action":"stop","control_id":format!("stop-{attempt}")}));
        assert!(matches!(b.admit(&stop, &client).await, DeliveryVerdict::Admitted { .. }));
        assert!(matches!(b.admit(&stop, &client).await, DeliveryVerdict::Refused { .. }));
        let (seq, input) = b.input(&provider, &stop);
        let pinned = b.ctx.pinned_run_record_bytes(&input).unwrap();
        let session = ProviderHandoffSession::new(&b.ctx).unwrap();
        let handoff = session.from_invocation(&b.ctx, b.ctx.descriptor_snapshot.as_ref().unwrap(), &plan.run_id.clone().into(), &input, seq, &pinned).unwrap();
        let label = if attempt == 2 { "turn.control.acknowledged" } else { "turn.control.failed" };
        let receipt = artifact(label, json!({"action":"stop","status":if attempt == 2 {"acknowledged"} else {"failed"},"source_movement_seq":seq}));
        if let Some(active) = &foreground {
            b.drive(active.hand_from_input(emission(receipt), false, seq, attempt == 2)).await;
        } else {
            b.drive(handoff.hand_observation(emission(receipt), true)).await;
            let restarted = ProviderHandoffSession::new(&b.ctx).unwrap().from_invocation(&b.ctx,b.ctx.descriptor_snapshot.as_ref().unwrap(),&plan.run_id.clone().into(),&input,seq,&pinned).unwrap();
            assert_eq!(b.drive(restarted.recover_source()).await, ProviderSourceRecovery::Completed);
        }
        let message = artifact("operator.message", json!({"text":"typed input","message_id":format!("message-{attempt}")}));
        if attempt == 0 {
            assert!(matches!(b.admit(&message, &client).await, DeliveryVerdict::Admitted { .. }));
            let (seq, input) = b.input(&provider, &message); let pinned = b.ctx.pinned_run_record_bytes(&input).unwrap();
            foreground = Some(ProviderHandoffSession::new(&b.ctx).unwrap().from_invocation(&b.ctx,b.ctx.descriptor_snapshot.as_ref().unwrap(),&plan.run_id.clone().into(),&input,seq,&pinned).unwrap());
        }
        assert_eq!(voice_path.as_ref().map(|path| std::fs::read(path).unwrap()), voice);
    }
    let active = foreground.unwrap();
    b.drive(active.hand_final(emission(artifact("interface.artifact.output", json!({"text":"typed response"}))))).await;
    let retry = artifact("operator.message", json!({"text":"next typed input","message_id":"retry"}));
    assert!(matches!(b.admit(&retry, &client).await, DeliveryVerdict::Admitted { .. }));
    assert!(b.log.read_from(Seq(0)).iter().any(|row| row.kind == MovementKind::InterfaceOutbound && row.to == Endpoint::Interface(client.clone()) && row.artifact.as_ref().is_some_and(|a| a.id.as_str().starts_with("turn.control.failed:"))));
    assert_eq!(voice_path.as_ref().map(|path| std::fs::read(path).unwrap()), voice);
    }
}
