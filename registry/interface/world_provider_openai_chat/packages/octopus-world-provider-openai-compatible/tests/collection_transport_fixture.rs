use futures::{SinkExt, StreamExt};
use octopus_app_server::{SharedBlobHandle, SpineRootConfig, admit_interface_artifact_arrival, build_embedded_spine};
use octopus_context_collections::CollectionSelection;
use octopus_descriptors::InterfaceId;
use octopus_interface_transport::{InterfaceHostIoLimits, InterfaceLiftContext};
use octopus_interfaces::{DeliveryVerdict, InterfaceArtifactFile, InterfaceInvocationEnvelope};
use octopus_ledger::{ContentHash, LiveMovementLog, SharedBlobs};
use octopus_ledger_store::{DurableBlobs, DurableLog};
use octopus_operator_chat_client::{OperatorChatPlanRequest, plan_operator_chat_topology};
use octopus_provider_run_authoring::{ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession};
use octopus_system_room_env::execute_file_write_result;
use octopus_tool_handlers::{ToolCall, ToolCallArtifact};
use octopus_world_provider_openai_compatible::WorldProviderInterfaceDriver;
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "../../../../../worker_environment_kind/system_room/packages/octopus-system-room-env/src/bound_surface.rs"]
mod bound_surface;
#[path = "../../../../../worker_environment_kind/system_room/packages/octopus-system-room-env/src/collection_tools.rs"]
mod collection_tools;
#[path = "../../../../../worker_environment_kind/system_room/packages/octopus-system-room-env/src/skill_bound.rs"]
mod skill_bound;
mod artifact_bound {
    include!(
        "../../../../../worker_environment_kind/system_room/packages/octopus-system-room-env/src/artifact_bound.rs"
    );

    pub async fn read_call(
        surface: &dyn ArtifactSurface,
        call: &ToolCall,
    ) -> Result<ToolHandlerResult, ToolHandlerError> {
        assert_eq!(descriptor_operation(call)?, "read");
        read(surface, call).await
    }
}
#[path = "../../../../world_operator_chat/packages/octopus-operator-chat-client/tests/support/composition_fixture.rs"]
mod composition;

const PROCEDURE: &str = "Read the exact evidence, prepare an ordinary checked write, and retain its receipt.";
const PRIVATE_BODY: &str = "EXTERNAL PRIVATE EVIDENCE BODY";
const CHATS: [&str; 5] = ["publisher", "separate-discovery", "entry-discovery", "exact-read", "selected-new-run"];

struct ProviderTurn {
    emission: ProviderEmission,
    input: InterfaceInvocationEnvelope,
    tools: BTreeMap<String, String>,
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..")
}

fn fixture(root: &Path, endpoint: &str) -> OperatorChatPlanRequest {
    let composed = composition::client_fixture(root);
    let client_path = "registry/interface/world_operator_chat/client.json";
    let mut client: Value = serde_json::from_slice(&std::fs::read(root.join(client_path)).unwrap()).unwrap();
    client["run"]["context_artifact_bindings"] = json!([]);
    composition::write(root, client_path, client);
    let mut provider: Value = serde_json::from_slice(
        &std::fs::read(repository().join("registry/interface/world_provider_nous_oauth/interface.json")).unwrap(),
    )
    .unwrap();
    for id in ["world_provider_azure_foundry_chat_entra", "world_provider_nous_oauth"] {
        let value: Value = serde_json::from_slice(
            &std::fs::read(repository().join(format!("registry/interface/{id}/interface.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(value["interface_driver_ref"], "openai_compatible");
    }
    let ollama: Value = serde_json::from_slice(
        &std::fs::read(repository().join("registry/interface/world_provider_ollama/interface.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ollama["interface_driver_ref"], "codex_app_server");
    provider["id"] = json!("world_provider_fixture");
    let private = provider["interface_driver_config"]["private_config"].as_object_mut().unwrap();
    private.remove("nous_oauth");
    private.insert("model".into(), json!("fixture-a"));
    private.insert("model_catalog".into(), json!([{"model":"fixture-a"}]));
    provider["interface_driver_config"]["transport"] = json!({
        "base_url":endpoint,"wire":"openai_chat_completions",
        "request_timeout_ms":5000,"max_response_bytes":1048576});
    provider["interface_policy"]["variables"]["models"]["default"] = json!(["fixture-a"]);
    composition::write(root, "registry/interface/world_provider_fixture/interface.json", provider);
    let life = serde_json::from_slice(
        &std::fs::read(repository().join("registry/interface/world_provider_nous_oauth/life.json")).unwrap(),
    )
    .unwrap();
    composition::write(root, "registry/interface/world_provider_fixture/life.json", life);
    for relative in [
        "interface/system_context/interface.json",
        "interface/system_context/life.json",
        "tool/context.discover/tool.json",
        "tool/context.read/tool.json",
        "tool/context.prepare_publication/tool.json",
    ] {
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(repository().join("registry").join(relative)).unwrap()).unwrap();
        if relative == "interface/system_context/interface.json" {
            value["interface_driver_config"]["private_config"]["context_artifact_bindings"] = json!([{"id":"library"}]);
        }
        composition::write(&root.join("registry"), relative, value);
    }
    for (file, interfaces, tools) in [
        ("registry/interface/system_worker_template/policy.json", "/ceiling/interfaces", "/ceiling/tools"),
        ("registry/ee_template/existence/ee_template.json", "/allowed_interfaces", "/allowed_tools"),
        ("ee/ee_existence.json", "/policy_narrowing/allowed_interfaces", "/policy_narrowing/allowed_tools"),
    ] {
        let mut value: Value = serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap();
        value.pointer_mut(interfaces).unwrap().as_array_mut().unwrap().push(json!("system_context"));
        value.pointer_mut(tools).unwrap().as_array_mut().unwrap().extend([
            json!("context.discover"),
            json!("context.read"),
            json!("context.prepare_publication"),
            json!("artifact.read"),
        ]);
        if file != "registry/interface/system_worker_template/policy.json" {
            for chat in CHATS {
                value.pointer_mut(interfaces).unwrap().as_array_mut().unwrap().extend([
                    json!(format!("system_interface_authoring_{chat}")),
                    json!(format!("system_room_authoring_{chat}")),
                    json!(format!("system_room_{chat}")),
                    json!(format!("system_context_{chat}")),
                    json!(format!("system_worker_template_{chat}")),
                    json!(format!("world_operator_chat_{chat}")),
                    json!(format!("world_provider_fixture_{chat}")),
                ]);
                value.pointer_mut(tools).unwrap().as_array_mut().unwrap().extend([
                    json!(format!("file.write__authoring_{chat}")),
                    json!(format!("file.write__{chat}")),
                    json!(format!("artifact.send__{chat}")),
                    json!(format!("context.discover__{chat}")),
                    json!(format!("context.read__{chat}")),
                    json!(format!("context.prepare_publication__{chat}")),
                    json!(format!("artifact.read__{chat}")),
                ]);
            }
        }
        composition::write(root, file, value);
    }
    let mut tools = composed.selected_tools;
    tools.extend(["context.discover", "context.read", "context.prepare_publication"].map(|tool| {
        octopus_operator_chat_client::SelectedToolSeed {
            tool: tool.into(),
            interface: "system_context".into(),
            worker_environment_kind: "system_room".into(),
        }
    }));
    serde_json::from_value(json!({"registry_root":composed.roots.registry_root,"ee_instances_root":composed.roots.ee_instances_root,
        "run_records_root":composed.roots.run_records_root,"interface_private_state_root":composed.roots.interface_private_state_root,
        "write_hash_algorithm":"blake3","chat_id":"selected-new-run","provider_interface_id":"world_provider_fixture","model":"fixture-a",
        "ee_id":"ee_existence","worker_interface_id":"system_worker_template","client_surface_role":"task","cycle":"first",
        "selected_tools":tools,"context_artifact_bindings":[{"id":"library"}],
        "door_host_slots":{"system_room":{"registry_artifact_root":root.join("registry/artifact")},
            "system_context":{"registry_artifact_root":root.join("registry/artifact")}},
        "conversation":{"ledger_offset":0}})).unwrap()
}

async fn wire_fixture()
-> (String, tokio::sync::mpsc::Sender<Value>, tokio::sync::mpsc::Receiver<Value>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (responses, mut response_rx) = tokio::sync::mpsc::channel::<Value>(1);
    let (requests, request_rx) = tokio::sync::mpsc::channel(1);
    let server = tokio::spawn(async move {
        while let Some(message) = response_rx.recv().await {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let (header_end, length) = loop {
                let mut part = [0_u8; 4096];
                let count = stream.read(&mut part).await.unwrap();
                assert!(count > 0 && bytes.len() + count <= 1048576);
                bytes.extend_from_slice(&part[..count]);
                if let Some(index) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..index]).unwrap();
                    let mut request_line = headers.lines().next().unwrap().split_whitespace();
                    assert_eq!(request_line.next(), Some("POST"));
                    let target = request_line.next().unwrap();
                    let (path, query) = target.split_once('?').unwrap_or((target, ""));
                    assert_eq!(path, "/v1/chat/completions");
                    assert!(query.is_empty());
                    assert_eq!(request_line.next(), Some("HTTP/1.1"));
                    assert!(request_line.next().is_none());
                    assert!(!headers.to_lowercase().contains("authorization:"));
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    assert!(length <= 1048576);
                    break (index + 4, length);
                }
            };
            while bytes.len() < header_end + length {
                let mut part = [0_u8; 4096];
                let count = stream.read(&mut part).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&part[..count]);
            }
            requests.send(serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()).await.unwrap();
            let chunk = json!({"id":"transport-fixture","model":"fixture-a","choices":[{"index":0,"delta":message,
                "finish_reason":if message.get("tool_calls").is_some(){"tool_calls"}else{"stop"}}]});
            let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    (endpoint, responses, request_rx, server)
}

async fn provider_turn(
    root: &Path,
    request: &OperatorChatPlanRequest,
    chat: &str,
    tool: &str,
    arguments: Value,
    responses: &tokio::sync::mpsc::Sender<Value>,
) -> ProviderTurn {
    let mut request = request.clone();
    request.chat_id = chat.into();
    let plan = plan_operator_chat_topology(&request).unwrap();
    let tools = plan.capability_tools.clone();
    let response = tool_response(tools.get(tool).unwrap(), arguments);
    for write in plan.interface_writes.iter().chain(&plan.run_writes) {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
        std::fs::write(&write.path, &write.content).unwrap();
    }
    let runs = request.run_records_root.as_ref().unwrap();
    let run_path = runs.join(format!("{}.json", plan.run_id));
    std::fs::create_dir_all(runs).unwrap();
    std::fs::write(run_path, &plan.run_record_content).unwrap();
    if chat == "publisher" {
        let candidate = "candidate_unselected__publisher";
        let mut descriptor: Value = serde_json::from_slice(&std::fs::read(request.registry_root.join("tool").join(&tools["context.discover"]).join("tool.json")).unwrap()).unwrap();
        descriptor["id"] = json!(candidate);
        composition::write(&request.registry_root, &format!("tool/{candidate}/tool.json"), descriptor);
        for (path, pointer) in [
            (format!("registry/interface/{}/policy.json", plan.worker_interface_id), "/ceiling/tools"),
            ("registry/ee_template/existence/ee_template.json".into(), "/allowed_tools"),
            ("ee/ee_existence.json".into(), "/policy_narrowing/allowed_tools"),
        ] {
            let mut policy: Value = serde_json::from_slice(&std::fs::read(root.join(&path)).unwrap()).unwrap();
            policy.pointer_mut(pointer).unwrap().as_array_mut().unwrap().push(json!(candidate));
            composition::write(root, &path, policy);
        }
    }
    let run: Value = serde_json::from_str(&plan.run_record_content).unwrap();
    let provider: InterfaceId = plan.model_interface_id.clone().into();
    let author: InterfaceId =
        run["interface_state"][provider.as_str()]["run_authoring_binding"]["authoring_source_interface"]
            .as_str()
            .unwrap()
            .into();
    let descriptor = serde_json::from_slice(
        &std::fs::read(request.registry_root.join("interface").join(provider.as_str()).join("interface.json")).unwrap(),
    )
    .unwrap();
    let ledger = root.join(chat).join("ledger");
    let blobs: SharedBlobs = Arc::new(Mutex::new(DurableBlobs::open(ledger.join("blobs")).unwrap()));
    let runtime = build_embedded_spine(SpineRootConfig {
        registry_root: request.registry_root.clone(),
        run: plan.run_id.clone().into(),
        blobs: blobs.clone(),
        interface_artifact_root: root.join(chat).join("artifacts"),
        run_records_root: runs.clone(),
        ee_instances_root: request.ee_instances_root.clone(),
    })
    .await
    .unwrap();
    let mut log = LiveMovementLog::new(DurableLog::open(ledger.join("movements.jsonl")).unwrap());
    let mut shared = SharedBlobHandle::new(blobs);
    let (sender, mut arrivals) = tokio::sync::mpsc::channel(16);
    let control = root.join(chat).join("control");
    let roots = BTreeMap::from([
        ("run_records_root".into(), runs.clone()),
        ("registry_artifact_root".into(), request.registry_root.join("artifact")),
    ]);
    let mut ctx = InterfaceLiftContext::unprivileged(
        provider.clone(),
        request.registry_root.join("interface").join(provider.as_str()),
        Some(descriptor),
        root.join(chat).join("provider"),
        ledger.join("movements.jsonl"),
        ledger.join("blobs"),
        ledger.join("index.sqlite"),
        roots.clone(),
        sender,
        Default::default(),
    );
    std::fs::create_dir_all(&ctx.var_dir).unwrap();
    ctx.root_slots.insert("interface_hand_in_root".into(), root.join(chat).join("hand-in"));
    ctx.root_slots.insert("app_server_control_root".into(), control.clone());
    ctx.host_io_limits = Some(InterfaceHostIoLimits {
        max_control_frame_bytes: 1 << 20,
        max_manifest_bytes: 1 << 20,
        max_payload_bytes: 1 << 20,
        max_stderr_bytes: 1024,
    });
    let socket_path = octopus_app_server_interface::interface_control_socket_path_from_root(&control, &author);
    std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
    let socket = tokio::net::UnixListener::bind(socket_path).unwrap();
    let message = InterfaceArtifactFile::write(
        root.join(chat).join("input"),
        "operator.message",
        &serde_json::to_vec(&json!({"text":"Use the declared collection contract","message_id":chat})).unwrap(),
    )
    .unwrap();
    admit_interface_artifact_arrival(
        &runtime,
        &mut log,
        &mut shared,
        &message.clone().capture_bounded(1 << 20, 1 << 20).unwrap(),
        &plan.chat_interface_id.clone().into(),
    )
    .await
    .unwrap();
    let mut reader = LedgerTailReader::open_named(&ctx, "transport-fixture").unwrap();
    let rows = reader.read_new().unwrap();
    let source = rows.last().unwrap();
    let input = reader.envelope(source).unwrap();
    let pinned = ctx.pinned_run_record_bytes(&input).unwrap();
    let handoff = ProviderHandoffSession::new(&ctx)
        .unwrap()
        .from_invocation(
            &ctx,
            ctx.descriptor_snapshot.as_ref().unwrap(),
            &plan.run_id.into(),
            &input,
            source.movement.seq.0,
            &pinned,
        )
        .unwrap();
    if chat == "publisher" {
        let before = input.metadata.clone();
        let selected = handoff.callable_tool_names(&input).unwrap();
        assert!(selected.contains(&tools["context.discover"]));
        assert!(!selected.contains("candidate_unselected__publisher"));
        assert!(input.grant().unwrap().tools.contains(&"candidate_unselected__publisher".into()));
        assert_eq!(input.metadata, before);
        let mut forged = input.clone();
        forged.metadata.as_mut().unwrap().grant.tools.push("unaccepted".into());
        assert!(handoff.callable_tool_names(&forged).unwrap_err().contains("exact accepted invocation metadata"));
    }
    let driver = WorldProviderInterfaceDriver::from_descriptor(ctx.descriptor_snapshot.as_ref().unwrap()).unwrap();
    responses.send(response).await.unwrap();
    let (finished, mut done) = tokio::sync::oneshot::channel();
    let (emissions, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(async {
            let result = driver.receive_artifact_and_materialize_outputs(input.clone(),source.movement.seq.0,1,None,&handoff,&ctx.var_dir.join("output")).await;
            let _ = finished.send(());
            result.unwrap()
        }, async {
            loop { tokio::select! {
                _ = &mut done => break,
                arrival = arrivals.recv() => {
                    let arrival = arrival.unwrap();
                    if let Some(retained) = arrival.retained {
                        let delivery = octopus_app_server::admit_retained_interface_artifact_arrival(&runtime,&mut log,&mut shared,&arrival.artifact,&provider,&retained.id).await.unwrap();
                        retained.reply.send(Ok(delivery)).unwrap();
                    } else {
                        let outcome = admit_interface_artifact_arrival(&runtime,&mut log,&mut shared,&arrival.artifact.clone().capture_bounded(1<<20,1<<20).unwrap(),&provider).await.unwrap();
                        arrival.verdict_tx.unwrap().send(DeliveryVerdict::Admitted {seq:outcome.admission.records.first().unwrap().arrival_movement.seq.0}).unwrap();
                    }
                }
                accepted = socket.accept() => {
                    let mut wire = tokio_tungstenite::accept_async(accepted.unwrap().0).await.unwrap();
                    let bytes = wire.next().await.unwrap().unwrap().into_data();
                    let path = ctx.var_dir.join("authoring-arrival.json");
                    std::fs::write(&path,bytes).unwrap();
                    let file = InterfaceArtifactFile::new(path).unwrap();
                    let admitted = admit_interface_artifact_arrival(&runtime,&mut log,&mut shared,&file.clone().capture_bounded(1<<20,1<<20).unwrap(),&author).await.unwrap();
                    let verdict = DeliveryVerdict::Admitted {seq:admitted.admission.records.first().unwrap().arrival_movement.seq.0};
                    let mut door_ctx = ctx.clone(); door_ctx.interface = plan.authoring_environment_interface_id.clone().into();
                    let mut door_reader = LedgerTailReader::open_named(&door_ctx,"writer-fixture").unwrap();
                    let rows = door_reader.read_new().unwrap();
                    let envelope = door_reader.envelope(rows.last().unwrap()).unwrap();
                    let call: ToolCallArtifact = serde_json::from_slice(&envelope.read_artifact_payload().unwrap()).unwrap();
                    let receipt = execute_file_write_result(&envelope,&call.arguments,&["system_room".into()],&roots,&ctx.var_dir.join("effects"),1<<20).unwrap();
                    let result = octopus_tool_handlers::ToolResultArtifact::success(&call.tool,call.correlation_id,receipt);
                    let result = InterfaceArtifactFile::write(ctx.var_dir.join("results"),&result.label,&serde_json::to_vec(&result).unwrap()).unwrap();
                    admit_interface_artifact_arrival(&runtime,&mut log,&mut shared,&result.capture_bounded(1<<20,1<<20).unwrap(),&door_ctx.interface).await.unwrap();
                    wire.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&verdict).unwrap().into())).await.unwrap();
                }
            }}
        })
    }).await.unwrap();
    assert_eq!(emissions.len(), 1);
    ProviderTurn { emission: emissions.into_iter().next().unwrap(), input, tools }
}

fn tool_response(tool: &str, arguments: Value) -> Value {
    let encoded = tool.replace('-', "-h").replace('.', "-d");
    json!({"role":"assistant","tool_calls":[{"index":0,"id":"fixture-call","type":"function","function":{
        "name":encoded,"arguments":serde_json::to_string(&arguments).unwrap()}}]})
}

fn room_call(root: &Path, turn: &ProviderTurn) -> (Value, InterfaceInvocationEnvelope) {
    let mut input = turn.input.clone();
    let body: Value = serde_json::from_slice(&turn.emission.artifact.read_payload_bytes().unwrap()).unwrap();
    let tool = body["tool"].as_str().unwrap();
    assert!(matches!(turn.emission.route,ProviderEmissionRoute::Tool {tool:ref routed} if routed == tool));
    assert!(input.grant().unwrap().tools.iter().any(|granted| granted == tool));
    assert!(!turn.tools.contains_key("artifact.read"));
    assert!(input.grant().unwrap().tools.iter().all(|tool| !tool.starts_with("artifact.read__")));
    let descriptor =
        octopus_tool_registry::load_tool_descriptor_from_dir(&root.join("registry/tool"), tool).unwrap().unwrap();
    octopus_tool_registry::ToolArgumentValidator::compile(&descriptor.args_schema)
        .unwrap()
        .validate(&body["arguments"])
        .unwrap();
    input.artifact_file = turn.emission.artifact.clone();
    input.artifact.content = ContentHash::of_bytes(&std::fs::read(input.artifact_file.manifest_path()).unwrap());
    input.metadata.as_mut().unwrap().runtime.as_mut().unwrap().interface = descriptor.interface.clone();
    let result = collection_tools::execute(
        &input,
        &ToolCall {
            tool_id: tool.into(),
            arguments: body["arguments"].clone(),
            handler_config: descriptor.handler_config,
        },
        Some(&root.join("registry/artifact")),
        1 << 20,
    )
    .unwrap();
    (result.body, input)
}

#[tokio::test]
async fn compatible_collection_transport_fixture_publishes_discovers_reads_and_selects_for_new_run() {
    let temporary = tempfile::Builder::new().prefix("oct-col-wire-").tempdir_in("/tmp").unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let (endpoint, responses, mut requests, server) = wire_fixture().await;
    let request = fixture(&root, &endpoint);
    let descriptor = json!({"id":"library","label":"context.collection","description":"Reusable library","schema_version":1,
        "directory":"entries","limits":{"max_entries":8,"max_entry_bytes":16384,"max_page_bytes":32768,"max_scan_bytes":131072}});
    composition::write(&root, "registry/artifact/library/artifact.json", descriptor);
    let external = json!({"id":"unbound-proof","label":"context.reference","text":PRIVATE_BODY});
    composition::write(&root, "registry/artifact/unbound-proof/artifact.json", external.clone());
    let entry = json!({"schema_version":1,"id":"checked-write","title":"Checked write","description":"A reusable learned procedure",
        "applicability":"When publishing a reusable result","state":"active","scope":{"ee_ids":["ee_existence"]},
        "content":{"text":PROCEDURE,"evidence":[{"id":"unbound-proof","content":ContentHash::of_canonical_json(&external)}]},
        "selection":{"selected_tools":[{"tool":"artifact.read","interface":"system_context","worker_environment_kind":"system_room"}]}});
    let publisher = provider_turn(
        &root,
        &request,
        "publisher",
        "context.prepare_publication",
        json!({"source":"library","entry":entry,"expected_revision":null}),
        &responses,
    )
    .await;
    let (prepared, mut effect) = room_call(&root, &publisher);
    assert!(!root.join("registry/artifact/library/entries/checked-write.json").exists());
    let wire = requests.recv().await.unwrap();
    assert!(!wire.to_string().contains("candidate_unselected__publisher"));
    assert!(wire["messages"].to_string().contains("callable tools:"));
    assert!(wire["messages"].to_string().contains(&publisher.tools["context.discover"]));
    assert!(wire["tools"].as_array().unwrap().iter().any(|tool| tool["function"]["name"] == publisher.tools["context.discover"].replace('.', "-d")));
    let writer_id = &publisher.tools["file.write"];
    assert!(effect.grant().unwrap().tools.contains(writer_id));
    let writer =
        octopus_tool_registry::load_tool_descriptor_from_dir(&root.join("registry/tool"), writer_id).unwrap().unwrap();
    octopus_tool_registry::ToolArgumentValidator::compile(&writer.args_schema)
        .unwrap()
        .validate(&prepared["file_write"])
        .unwrap();
    effect.artifact_file = InterfaceArtifactFile::write(
        root.join("publish"),
        "tool.call",
        &serde_json::to_vec(&json!({"tool":writer_id,"arguments":prepared["file_write"]})).unwrap(),
    )
    .unwrap();
    effect.artifact.content = ContentHash::of_bytes(&std::fs::read(effect.artifact_file.manifest_path()).unwrap());
    effect.metadata.as_mut().unwrap().runtime.as_mut().unwrap().interface = writer.interface;
    let roots = BTreeMap::from([("registry_artifact_root".into(), root.join("registry/artifact"))]);
    let receipt = execute_file_write_result(
        &effect,
        &prepared["file_write"],
        &["system_room".into()],
        &roots,
        &root.join("publication-effects"),
        1 << 20,
    )
    .unwrap();
    assert_eq!(
        execute_file_write_result(
            &effect,
            &prepared["file_write"],
            &["system_room".into()],
            &roots,
            &root.join("publication-effects"),
            1 << 20
        )
        .unwrap(),
        receipt
    );
    let discovery =
        provider_turn(&root, &request, "separate-discovery", "context.discover", json!({}), &responses).await;
    let (sources, _) = room_call(&root, &discovery);
    assert_eq!(sources["sources"][0]["source"]["id"], "library");
    let discovered_source = sources["sources"][0]["source"]["id"].as_str().unwrap();
    requests.recv().await.unwrap();
    let discovery = provider_turn(
        &root,
        &request,
        "entry-discovery",
        "context.discover",
        json!({"source":discovered_source}),
        &responses,
    )
    .await;
    let (discovered, _) = room_call(&root, &discovery);
    assert_eq!(discovered["entries"].as_array().unwrap().len(), 1);
    assert!(discovered["entries"][0].get("content").is_none());
    let pin: CollectionSelection = serde_json::from_value(discovered["entries"][0]["pin"].clone()).unwrap();
    requests.recv().await.unwrap();
    let read_turn = provider_turn(
        &root,
        &request,
        "exact-read",
        "context.read",
        json!({"source":discovered_source,"entry_id":pin.entry_id,"revision":pin.revision.to_hex()}),
        &responses,
    )
    .await;
    let (read, _) = room_call(&root, &read_turn);
    assert_eq!(read["entry"]["content"]["text"], PROCEDURE);
    assert_eq!(read["pin"], serde_json::to_value(&pin).unwrap());
    requests.recv().await.unwrap();
    let mut selected = request.clone();
    selected.publications = vec![pin];
    let selected_plan = plan_operator_chat_topology(&selected).unwrap();
    let snapshot = selected_plan.publication_evidence[0].snapshot.as_ref().unwrap();
    let turn =
        provider_turn(&root, &selected, "selected-new-run", "artifact.read", json!({"id":snapshot.id}), &responses)
            .await;
    let tool_id = &turn.tools["artifact.read"];
    let input = &turn.input;
    assert!(matches!(turn.emission.route, ProviderEmissionRoute::Tool { ref tool } if tool == tool_id));
    assert!(input.grant().unwrap().tools.contains(tool_id));
    assert!(!input.grant().unwrap().may_read_artifact("unbound-proof"));
    assert!(input.metadata().unwrap().artifact_binding_refs.contains(snapshot));
    let descriptor =
        octopus_tool_registry::load_tool_descriptor_from_dir(&root.join("registry/tool"), tool_id).unwrap().unwrap();
    let call: ToolCallArtifact = serde_json::from_slice(&turn.emission.artifact.read_payload_bytes().unwrap()).unwrap();
    let validator = octopus_tool_registry::ToolArgumentValidator::compile(&descriptor.args_schema).unwrap();
    validator.validate(&call.arguments).unwrap();
    let surface = bound_surface::InvocationArtifactSurface::new(
        &input.artifact_binding_payloads,
        &root.join("selected-read"),
        input.grant().unwrap().clone(),
        None,
        1 << 20,
    )
    .unwrap();
    let mut call =
        ToolCall { tool_id: call.tool, arguments: call.arguments, handler_config: descriptor.handler_config };
    let output = artifact_bound::read_call(&surface, &call).await.unwrap().body;
    assert_eq!(output["result"], "artifact_read");
    assert_eq!(output["id"], snapshot.id.as_str());
    assert_eq!(output["truncated"], false);
    let bound = input.artifact_binding_payloads.iter().find(|payload| payload.artifact == *snapshot).unwrap();
    assert_eq!(output["text"].as_str().unwrap().as_bytes(), bound.read_bytes().unwrap().as_ref());
    let read: Value = serde_json::from_str(output["text"].as_str().unwrap()).unwrap();
    assert_eq!(read["text"], PROCEDURE);
    assert_eq!(read["publication_selection"]["pin"], serde_json::to_value(&selected.publications[0]).unwrap());
    call.arguments = json!({"id":"unbound-proof"});
    validator.validate(&call.arguments).unwrap();
    assert!(artifact_bound::read_call(&surface, &call).await.is_err());
    let wire = requests.recv().await.unwrap();
    assert!(wire["messages"][0]["content"].as_str().unwrap().contains(PROCEDURE));
    assert!(!wire.to_string().contains(PRIVATE_BODY));
    assert!(
        wire["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"]
                == tool_response(tool_id, json!({}))["tool_calls"][0]["function"]["name"])
    );
    assert!(!wire.to_string().contains("RunOperation"));
    drop(responses);
    server.await.unwrap();
}
