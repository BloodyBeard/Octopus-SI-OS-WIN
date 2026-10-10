use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_interfaces::{
    GrantProjection, InterfaceArtifactFile, InterfaceInvocationEnvelope, InvocationMetadata,
    RuntimeInvocationMetadata,
};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_mcp_stdio_room::{McpStdioRoom, McpStdioRoomIo};
use octopus_tool_door_runtime::{
    DeliveryVerdict, RefusedReason, ToolProcessEnvironment, ToolProcessError, ToolProcessHandler,
    ToolProcessInvocation, WorkerEnvironmentLifecycleRecorder, WorkerEnvironmentLiveStateRecorder,
};
use octopus_tool_registry::{load_tool_descriptor_from_dir, ToolDescriptor};
use octopus_worker_env::{
    WorkerEnvKindRegistry, WorkerEnvKindSpec, WorkerEnvironmentFormation,
    WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId, WorkerEnvironmentRecordStore,
};
use serde_json::{json, Value};

const DOOR: &str = "system_mcp";
const WORKER: &str = "system_worker_coder_max";
const KIND: &str = "mcp_stdio_session";

struct TestRoomIo {
    next_seq: AtomicU64,
    fail_next_hand_in: AtomicBool,
    refuse_next_hand_in: AtomicBool,
    fail_next_lifecycle: AtomicBool,
    fail_next_live_state: AtomicBool,
    handed: Mutex<Vec<InterfaceArtifactFile>>,
    record_root: Option<PathBuf>,
    live_states: Mutex<Vec<Value>>,
}

impl Default for TestRoomIo {
    fn default() -> Self {
        Self {
            next_seq: AtomicU64::new(1),
            fail_next_hand_in: AtomicBool::new(false),
            refuse_next_hand_in: AtomicBool::new(false),
            fail_next_lifecycle: AtomicBool::new(false),
            fail_next_live_state: AtomicBool::new(false),
            handed: Mutex::new(Vec::new()),
            record_root: None,
            live_states: Mutex::new(Vec::new()),
        }
    }
}

impl TestRoomIo {
    fn failing_once() -> Self {
        Self {
            fail_next_hand_in: AtomicBool::new(true),
            ..Self::default()
        }
    }

    fn host_backed(record_root: PathBuf) -> Self {
        Self {
            record_root: Some(record_root),
            ..Self::default()
        }
    }

    fn take_handed(&self) -> Vec<InterfaceArtifactFile> {
        std::mem::take(&mut *self.handed.lock().unwrap())
    }

    fn fail_next_lifecycle(&self) {
        self.fail_next_lifecycle.store(true, Ordering::Release);
    }

    fn fail_next_live_state(&self) {
        self.fail_next_live_state.store(true, Ordering::Release);
    }
}

impl WorkerEnvironmentLifecycleRecorder for TestRoomIo {
    fn record<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        lifecycle: octopus_worker_env::WorkerEnvironmentLifecycle,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            if self.fail_next_lifecycle.swap(false, Ordering::AcqRel) {
                return Err("injected lifecycle write failure".to_string());
            }
            let Some(root) = self.record_root.as_ref() else {
                return Ok(());
            };
            let store = WorkerEnvironmentRecordStore::new(root);
            match lifecycle {
                octopus_worker_env::WorkerEnvironmentLifecycle::Active => {
                    store.activate(formation, 1)
                }
                octopus_worker_env::WorkerEnvironmentLifecycle::Released => {
                    store.release(formation, 1)
                }
                octopus_worker_env::WorkerEnvironmentLifecycle::Failed => store.fail(formation, 1),
            }
            .map(|_| ())
            .map_err(|error| error.to_string())
        })
    }
}

impl WorkerEnvironmentLiveStateRecorder for TestRoomIo {
    fn record_live_state<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        state: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            if self.fail_next_live_state.swap(false, Ordering::AcqRel) {
                return Err("injected live-state write failure".to_string());
            }
            self.live_states.lock().unwrap().push(state.clone());
            let Some(root) = self.record_root.as_ref() else {
                return Ok(());
            };
            WorkerEnvironmentRecordStore::new(root)
                .write_live_state(&formation.worker_environment_id, state)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }
}

impl McpStdioRoomIo for TestRoomIo {
    fn hand_output_artifact<'a>(
        &'a self,
        artifact: &'a InterfaceArtifactFile,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<DeliveryVerdict, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.handed.lock().unwrap().push(artifact.clone());
            if self.refuse_next_hand_in.swap(false, Ordering::AcqRel) {
                return Ok(DeliveryVerdict::Refused {
                    reason: RefusedReason::CheckpointSpent,
                    detail: json!("injected spent checkpoint"),
                });
            }
            if self.fail_next_hand_in.swap(false, Ordering::AcqRel) {
                return Err("injected delivery acknowledgement loss".to_string());
            }
            let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
            Ok(DeliveryVerdict::Admitted { seq })
        })
    }
}

fn registry_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}

fn tool_ids() -> Vec<String> {
    [
        "mcp.call_tool",
        "mcp.list_tools",
        "mcp.list_resources",
        "mcp.list_resource_templates",
        "mcp.read_resource",
        "mcp.list_prompts",
        "mcp.get_prompt",
        "mcp.cancel_request",
        "mcp.send_progress",
        "mcp.poll_events",
        "mcp.status",
        "mcp.close",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn descriptor(id: &str) -> ToolDescriptor {
    load_tool_descriptor_from_dir(&registry_root().join("tool"), id)
        .unwrap()
        .unwrap()
}

fn formation(door_instance_dir: &Path) -> (WorkerEnvironmentFormation, WorkerEnvKindSpec) {
    let registry =
        WorkerEnvKindRegistry::load_from_dir(&registry_root().join("worker_environment_kind"))
            .unwrap();
    let tools = tool_ids();
    let hashes = tools
        .iter()
        .map(|tool| {
            let value = serde_json::to_value(descriptor(tool)).unwrap();
            let hash = blake3::hash(&octopus_descriptors::canonical_bytes(&value))
                .to_hex()
                .to_string();
            (tool.clone(), hash)
        })
        .collect::<BTreeMap<_, _>>();
    let process_bins = tools
        .iter()
        .map(|tool| (tool.clone(), descriptor(tool).process.bin))
        .collect::<BTreeMap<_, _>>();
    let kind_id = WorkerEnvironmentKindId::new(KIND);
    let kinds = vec![kind_id.clone()];
    let door = InterfaceId::new(DOOR);
    let worker = InterfaceId::new(WORKER);
    let formed = registry
        .form(&WorkerEnvironmentFormationRequest {
            door_interface: &door,
            door_descriptor_hash: "mcp-stdio-test-door-hash",
            door_driver_ref: "system_mcp",
            tool: "mcp.list_tools",
            tool_descriptor_hashes: &hashes,
            tool_process_bins: &process_bins,
            tool_worker_environment_kinds: &kinds,
            worker: &worker,
            ee: "ee_mcp_test",
            run: "run_mcp_test",
            granted_worker_environment_kinds: &kinds,
            granted_tools: &tools,
            door_root_bindings: &BTreeMap::new(),
            door_instance_dir,
            ee_root_slot_allowance: None,
        })
        .unwrap();
    let (kind, _) = registry.get(&kind_id).unwrap();
    (formed, (*kind).clone())
}

fn write_interface(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(
        path.join("interface.json"),
        serde_json::to_vec_pretty(&json!({
            "id": DOOR,
            "interface_type": "system",
            "role": "system",
            "direction": "bidirectional",
            "source_names": ["mcp"],
            "interface_driver_ref": "system_mcp",
            "interface_driver_config": {
                "private_config": { "process_env_keys": [] }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        path.join("mcp_stdio.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "command": env!("CARGO_BIN_EXE_octopus-mcp-stdio-room-tool"),
            "args": ["--example-server"],
            "forward_env": [],
            "env": {},
            "protocol_versions": ["2025-06-18"],
            "client_name": "octopus-mcp-stdio-room-test",
            "client_version": "1",
            "request_timeout_ms": 5000,
            "max_message_bytes": 1048576,
            "max_stderr_segment_bytes": 65536,
            "stderr_retained_segments": 4,
            "event_journal_segment_bytes": 1048576,
            "event_journal_retained_segments": 8
        }))
        .unwrap(),
    )
    .unwrap();
}

fn envelope(
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
) -> InterfaceInvocationEnvelope {
    let payload = serde_json::to_vec(&json!({
        "tool": tool,
        "arguments": arguments,
        "correlation_id": format!("call-{sequence}")
    }))
    .unwrap();
    let artifact_file = InterfaceArtifactFile::write(
        root.join(format!("inbound-{sequence}")),
        "tool.call",
        &payload,
    )
    .unwrap();
    let content = ContentHash::of_bytes(&payload);
    let metadata = InvocationMetadata::new(GrantProjection {
        tools: tool_ids(),
        worker_environment_kinds: vec![KIND.to_string()],
        ..GrantProjection::default()
    })
    .with_runtime(RuntimeInvocationMetadata {
        run: RunId::new("run_mcp_test"),
        ee: EeId::new("ee_mcp_test"),
        worker: InterfaceId::new(WORKER),
        checkpoint: CheckpointId::new(format!("cp-{sequence}")),
        interface: InterfaceId::new(DOOR),
        epoch: 0,
        run_record_hash: None,
    });
    InterfaceInvocationEnvelope {
        artifact: ArtifactRef {
            id: ArtifactId::new(format!("tool.call:{}", content.to_hex())),
            content,
        },
        artifact_file,
        artifact_binding_payloads: Vec::new(),
        metadata: Some(metadata),
    }
}

async fn invoke(
    room: &McpStdioRoom,
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
    formation: &WorkerEnvironmentFormation,
    kind: &WorkerEnvKindSpec,
) -> Vec<(String, Value)> {
    let io = TestRoomIo::host_backed(root.join("interface-var"));
    invoke_with_io(room, root, sequence, tool, arguments, formation, kind, &io)
        .await
        .unwrap_or_else(|error| match error {
            ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                panic!("MCP stdio room invocation failed: {error}")
            }
        })
}

async fn invoke_with_io(
    room: &McpStdioRoom,
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
    formation: &WorkerEnvironmentFormation,
    kind: &WorkerEnvKindSpec,
    io: &TestRoomIo,
) -> Result<Vec<(String, Value)>, ToolProcessError> {
    let invocation = ToolProcessInvocation {
        request_id: sequence,
        descriptor: descriptor(tool),
        envelope: envelope(root, sequence, tool, arguments),
        source_movement_seq: sequence,
        worker_environment: Some(formation.clone()),
        worker_environment_kind: Some(kind.clone()),
    };
    assert!(room.receive(invocation, io).await?.is_empty());
    let artifacts = io.take_handed();
    assert_eq!(artifacts.len(), 1);
    Ok(artifacts
        .into_iter()
        .map(|artifact| {
            let label = artifact.read_manifest().unwrap().label;
            let body = serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
            (label, body)
        })
        .collect())
}

#[tokio::test]
async fn one_formed_room_keeps_the_stdio_server_across_catalog_tool_resource_and_prompt_calls() {
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    let interface_var = root.path().join("interface-var");
    write_interface(&interface_dir);
    let room = McpStdioRoom::new(ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir: interface_dir.clone(),
        descriptor_snapshot: None,
        var_dir: interface_var.clone(),
        durable_var_dir: Some(interface_var.join("durable-formation")),
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    });
    let (formation, kind) = formation(&interface_dir);
    WorkerEnvironmentRecordStore::new(&interface_var)
        .activate(&formation, 1)
        .unwrap();

    let listed = invoke(
        &room,
        root.path(),
        1,
        "mcp.list_tools",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(listed[0].0, "mcp.list_tools.result");
    assert_eq!(listed[0].1["ok"], true, "{:?}", listed[0].1);
    assert_eq!(listed[0].1["body"]["session_disposition"], "connected");
    assert_eq!(listed[0].1["body"]["result"]["tools"][0]["name"], "echo");
    assert_eq!(listed[0].1["correlation_id"], "call-1");
    assert_eq!(
        listed[0].1["body"]["server_events"]["worker_environment_id"],
        formation.worker_environment_id.as_str()
    );
    assert!(listed[0].1["body"]["server_events"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["body"]["method"] == "notifications/tools/list_changed"));

    let called = invoke(
        &room,
        root.path(),
        2,
        "mcp.call_tool",
        json!({ "name": "echo", "arguments": { "text": "artifact-native" } }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(called[0].1["body"]["session_disposition"], "attached");
    assert_eq!(
        called[0].1["body"]["result"]["structuredContent"]["echo"],
        "artifact-native"
    );

    let resource = invoke(
        &room,
        root.path(),
        3,
        "mcp.read_resource",
        json!({ "uri": "octopus://room/readme" }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(
        resource[0].1["body"]["result"]["contents"][0]["text"],
        "persistent MCP stdio room"
    );

    let prompt = invoke(
        &room,
        root.path(),
        4,
        "mcp.get_prompt",
        json!({ "name": "echo_prompt", "arguments": { "text": "hello" } }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(
        prompt[0].1["body"]["result"]["messages"][0]["content"]["text"],
        "hello"
    );

    let live_path = WorkerEnvironmentRecordStore::new(&interface_var)
        .live_state_path(&formation.worker_environment_id);
    let live: Value = serde_json::from_slice(&std::fs::read(&live_path).unwrap()).unwrap();
    assert_eq!(live["status"], "running");
    assert_eq!(live["generation"], 1);
    assert_eq!(live["viewer"]["protocol"], "mcp_stdio_room");
    let stderr_path = PathBuf::from(live["viewer"]["stderr_log"].as_str().unwrap());
    for _ in 0..50 {
        if std::fs::read_to_string(&stderr_path)
            .unwrap_or_default()
            .contains("example server ready")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(std::fs::read_to_string(stderr_path)
        .unwrap()
        .contains("example server ready"));

    let closed = invoke(
        &room,
        root.path(),
        5,
        "mcp.close",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(closed[0].1["ok"], true);
    assert_eq!(closed[0].1["body"]["session_disposition"], "closed");
    let closed_live: Value = serde_json::from_slice(&std::fs::read(&live_path).unwrap()).unwrap();
    assert_eq!(closed_live["status"], "closed");
    assert!(closed_live["server"].is_null());
    assert!(closed_live["viewer"].is_null());
    assert_eq!(
        WorkerEnvironmentRecordStore::new(&interface_var)
            .read(&formation.worker_environment_id)
            .unwrap()
            .lifecycle,
        octopus_worker_env::WorkerEnvironmentLifecycle::Active
    );
    let closed_status = invoke(
        &room,
        root.path(),
        6,
        "mcp.status",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(closed_status[0].1["ok"], true);
    assert_eq!(closed_status[0].1["body"]["status"], "closed");
    assert_eq!(closed_status[0].1["body"]["session_disposition"], "closed");
    let closed_again = invoke(
        &room,
        root.path(),
        7,
        "mcp.close",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(closed_again[0].1["ok"], true);
    assert_eq!(closed_again[0].1["body"]["session_disposition"], "closed");
}

#[tokio::test]
async fn completed_mcp_effects_return_artifacts_when_evidence_writes_fail() {
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    write_interface(&interface_dir);
    let room = McpStdioRoom::new(ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir: interface_dir.clone(),
        descriptor_snapshot: None,
        var_dir: root.path().join("interface-var"),
        durable_var_dir: Some(root.path().join("durable-formation")),
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    });
    let (formation, kind) = formation(&interface_dir);

    let ready = invoke(
        &room,
        root.path(),
        20,
        "mcp.list_tools",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(ready[0].1["ok"], true, "{:?}", ready[0].1);

    let io = TestRoomIo::default();
    io.fail_next_live_state();
    let called = invoke_with_io(
        &room,
        root.path(),
        21,
        "mcp.call_tool",
        json!({"name": "echo", "arguments": {"text": "once"}}),
        &formation,
        &kind,
        &io,
    )
    .await
    .unwrap_or_else(|error| match error {
        ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
            panic!("MCP effect invocation failed: {error}")
        }
    });
    assert_eq!(called[0].1["ok"], false, "{:?}", called[0].1);
    assert_eq!(
        called[0].1["error"]["details"]["operation_body"]["result"]["structuredContent"]["echo"],
        "once"
    );
    assert_eq!(
        called[0].1["error"]["details"]["evidence_write_errors"][0]["stage"],
        "live_state"
    );

    io.fail_next_lifecycle();
    let closed = invoke_with_io(
        &room,
        root.path(),
        22,
        "mcp.close",
        json!({}),
        &formation,
        &kind,
        &io,
    )
    .await
    .unwrap_or_else(|error| match error {
        ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
            panic!("MCP close invocation failed: {error}")
        }
    });
    assert_eq!(closed[0].1["ok"], false, "{:?}", closed[0].1);
    assert_eq!(
        closed[0].1["error"]["details"]["operation_body"]["closed"],
        true
    );
    assert_eq!(
        closed[0].1["error"]["details"]["evidence_write_errors"][0]["stage"],
        "lifecycle"
    );
}

#[tokio::test]
async fn projected_server_event_cursor_survives_room_restart_without_replaying_history() {
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    let interface_var = root.path().join("interface-var");
    write_interface(&interface_dir);
    let (formation, kind) = formation(&interface_dir);
    WorkerEnvironmentRecordStore::new(&interface_var)
        .activate(&formation, 1)
        .unwrap();
    let make_room = || {
        McpStdioRoom::new(ToolProcessEnvironment {
            interface: InterfaceId::new(DOOR),
            interface_dir: interface_dir.clone(),
            descriptor_snapshot: None,
            var_dir: interface_var.clone(),
            durable_var_dir: Some(interface_var.join("durable-formation")),
            host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
                max_control_frame_bytes: 1024 * 1024,
                max_stderr_bytes: 256 * 1024,
                max_manifest_bytes: 1024 * 1024,
                max_payload_bytes: 64 * 1024 * 1024,
            },
            private_reads: BTreeMap::new(),
            root_slots: BTreeMap::new(),
        })
    };

    let room = make_room();
    let first = invoke(
        &room,
        root.path(),
        40,
        "mcp.list_tools",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    let first_events = first[0].1["body"]["server_events"]["events"]
        .as_array()
        .unwrap();
    assert_eq!(first_events.len(), 1, "{first_events:?}");
    let first_sequence = first_events[0]["seq"].as_u64().unwrap();
    let cursor_path = interface_var
        .join("durable-formation")
        .join("mcp-stdio")
        .join("projected_server_events.json");
    let cursor: Value = serde_json::from_slice(&std::fs::read(&cursor_path).unwrap()).unwrap();
    assert_eq!(cursor["schema_version"], 1);
    assert_eq!(cursor["cursor"], first_sequence);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&cursor_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let closed = invoke(
        &room,
        root.path(),
        41,
        "mcp.close",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(closed[0].1["ok"], true);
    drop(room);

    let restarted = make_room();
    let second = invoke(
        &restarted,
        root.path(),
        42,
        "mcp.list_tools",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    let second_events = second[0].1["body"]["server_events"]["events"]
        .as_array()
        .unwrap();
    assert_eq!(second_events.len(), 1, "{second_events:?}");
    assert_eq!(second_events[0]["seq"].as_u64(), Some(first_sequence + 1));
}

#[tokio::test]
async fn lost_result_ack_preserves_cursor_and_events_replay_after_room_restart() {
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    let interface_var = root.path().join("interface-var");
    write_interface(&interface_dir);
    let (formation, kind) = formation(&interface_dir);
    WorkerEnvironmentRecordStore::new(&interface_var)
        .activate(&formation, 1)
        .unwrap();
    let make_room = || {
        McpStdioRoom::new(ToolProcessEnvironment {
            interface: InterfaceId::new(DOOR),
            interface_dir: interface_dir.clone(),
            descriptor_snapshot: None,
            var_dir: interface_var.clone(),
            durable_var_dir: Some(interface_var.join("durable-formation")),
            host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
                max_control_frame_bytes: 1024 * 1024,
                max_stderr_bytes: 256 * 1024,
                max_manifest_bytes: 1024 * 1024,
                max_payload_bytes: 64 * 1024 * 1024,
            },
            private_reads: BTreeMap::new(),
            root_slots: BTreeMap::new(),
        })
    };
    let cursor_path = interface_var
        .join("durable-formation")
        .join("mcp-stdio")
        .join("projected_server_events.json");

    let room = make_room();
    let failed_io = TestRoomIo::failing_once();
    let failed_invocation = || ToolProcessInvocation {
        request_id: 70,
        descriptor: descriptor("mcp.list_tools"),
        envelope: envelope(root.path(), 70, "mcp.list_tools", json!({})),
        source_movement_seq: 70,
        worker_environment: Some(formation.clone()),
        worker_environment_kind: Some(kind.clone()),
    };
    failed_io.refuse_next_hand_in.store(true, Ordering::Release);
    let refused = room
        .receive(failed_invocation(), &failed_io)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, ToolProcessError::Unavailable(message) if message.contains("checkpoint_spent"))
    );
    let refused_result = failed_io.take_handed();
    assert_eq!(refused_result.len(), 1);
    assert!(!cursor_path.exists());
    let error = room
        .receive(failed_invocation(), &failed_io)
        .await
        .unwrap_err();
    assert!(matches!(error, ToolProcessError::Unavailable(message)
        if message.contains("acknowledgement unavailable") && message.contains("execution will not be repeated")));
    let first_handed = failed_io.take_handed();
    assert_eq!(first_handed.len(), 1);
    assert_eq!(
        first_handed[0].manifest_path(),
        refused_result[0].manifest_path()
    );
    let first_manifest = first_handed[0].manifest_path().to_path_buf();
    let first_payload = first_handed[0].read_payload_bytes().unwrap();
    let failed_result: Value = serde_json::from_slice(&first_payload).unwrap();
    assert_eq!(failed_result["ok"], true);
    assert!(failed_result["body"]["result"]["tools"].is_array());
    assert_eq!(failed_result["body"]["server_events"]["after_cursor"], 0);
    assert!(first_manifest.starts_with(interface_var.join("durable-formation")));
    assert!(!cursor_path.exists());
    if let Err(error) = room.release(&formation, &kind).await {
        match error {
            ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                panic!("MCP stdio room release failed: {error}")
            }
        }
    }
    drop(room);

    let restarted = make_room();
    let admitted_io = TestRoomIo::default();
    let invocation = ToolProcessInvocation {
        request_id: 71,
        descriptor: descriptor("mcp.list_resources"),
        envelope: envelope(root.path(), 71, "mcp.list_resources", json!({})),
        source_movement_seq: 71,
        worker_environment: Some(formation.clone()),
        worker_environment_kind: Some(kind.clone()),
    };
    restarted
        .receive(invocation, &admitted_io)
        .await
        .unwrap_or_else(|error| match error {
            ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                panic!("MCP stdio room restart invocation failed: {error}")
            }
        });
    let replayed = admitted_io.take_handed();
    assert_eq!(replayed.len(), 1);
    let replayed_body: Value =
        serde_json::from_slice(&replayed[0].read_payload_bytes().unwrap()).unwrap();
    let events = replayed_body["body"]["server_events"]["events"]
        .as_array()
        .unwrap();
    assert!(events.len() >= 2, "{events:?}");
    assert_eq!(
        events[0],
        failed_result["body"]["server_events"]["events"][0]
    );
    let last_sequence = events.last().unwrap()["seq"].as_u64().unwrap();
    let cursor: Value = serde_json::from_slice(&std::fs::read(&cursor_path).unwrap()).unwrap();
    assert_eq!(cursor["cursor"].as_u64(), Some(last_sequence));
    assert!(restarted
        .receive(failed_invocation(), &admitted_io)
        .await
        .unwrap_or_else(|_| panic!("retained result retry failed"))
        .is_empty());
    let retry = admitted_io.take_handed();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].manifest_path(), first_manifest);
    assert_eq!(retry[0].read_payload_bytes().unwrap(), first_payload);
    let after_retry: Value = serde_json::from_slice(&std::fs::read(cursor_path).unwrap()).unwrap();
    assert_eq!(after_retry, cursor);
    assert_eq!(admitted_io.live_states.lock().unwrap().len(), 1);
    let original_manifest = std::fs::read(&first_manifest).unwrap();
    for pointer in [
        "/tool",
        "/correlation_id",
        "/body/server_events/invocation/source_movement_seq",
        "/body/server_events/invocation/formation/formation_pin",
    ] {
        let mut mismatched = failed_result.clone();
        *mismatched.pointer_mut(pointer).unwrap() = json!("mismatched");
        let artifact = InterfaceArtifactFile::write(
            first_manifest.parent().unwrap(),
            "mcp.list_tools.result",
            &serde_json::to_vec(&mismatched).unwrap(),
        )
        .unwrap();
        std::fs::copy(artifact.manifest_path(), &first_manifest).unwrap();
        assert!(restarted
            .receive(failed_invocation(), &admitted_io)
            .await
            .is_err());
        assert!(admitted_io.take_handed().is_empty());
    }
    std::fs::write(&first_manifest, original_manifest).unwrap();
    assert_eq!(admitted_io.live_states.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn one_room_correlates_out_of_order_responses_without_blocking_a_fast_request() {
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    let interface_var = root.path().join("interface-var");
    write_interface(&interface_dir);
    let room = McpStdioRoom::new(ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir: interface_dir.clone(),
        descriptor_snapshot: None,
        var_dir: interface_var.clone(),
        durable_var_dir: Some(interface_var.join("durable-formation")),
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    });
    let (formation, kind) = formation(&interface_dir);
    WorkerEnvironmentRecordStore::new(&interface_var)
        .activate(&formation, 1)
        .unwrap();
    let connected = invoke(
        &room,
        root.path(),
        19,
        "mcp.list_tools",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(connected[0].1["ok"], true);

    let slow = invoke(
        &room,
        root.path(),
        20,
        "mcp.call_tool",
        json!({
            "name": "echo",
            "arguments": { "text": "slow", "delay_ms": 400 }
        }),
        &formation,
        &kind,
    );
    let fast = async {
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        let started = std::time::Instant::now();
        let result = invoke(
            &room,
            root.path(),
            21,
            "mcp.call_tool",
            json!({ "name": "echo", "arguments": { "text": "fast" } }),
            &formation,
            &kind,
        )
        .await;
        (result, started.elapsed())
    };
    let (slow, (fast, fast_elapsed)) = tokio::join!(slow, fast);
    assert_eq!(
        fast[0].1["body"]["result"]["structuredContent"]["echo"],
        "fast"
    );
    assert_eq!(
        slow[0].1["body"]["result"]["structuredContent"]["echo"],
        "slow"
    );
    assert!(
        fast_elapsed < std::time::Duration::from_millis(250),
        "fast request was serialized for {fast_elapsed:?}"
    );
    let projected_events = slow[0].1["body"]["server_events"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .chain(
            fast[0].1["body"]["server_events"]["events"]
                .as_array()
                .unwrap()
                .iter(),
        )
        .collect::<Vec<_>>();
    assert_eq!(projected_events.len(), 2, "{projected_events:?}");
    assert!(projected_events
        .iter()
        .all(|event| event["body"]["method"] == "notifications/progress"));
    let sequences = projected_events
        .iter()
        .map(|event| event["seq"].as_u64().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(sequences.len(), projected_events.len());
}

#[tokio::test]
async fn status_and_cancel_reach_one_room_while_a_call_is_in_flight() {
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    let interface_var = root.path().join("interface-var");
    write_interface(&interface_dir);
    let room = McpStdioRoom::new(ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir: interface_dir.clone(),
        descriptor_snapshot: None,
        var_dir: interface_var.clone(),
        durable_var_dir: Some(interface_var.join("durable-formation")),
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    });
    let (formation, kind) = formation(&interface_dir);
    WorkerEnvironmentRecordStore::new(&interface_var)
        .activate(&formation, 1)
        .unwrap();
    let connected = invoke(
        &room,
        root.path(),
        29,
        "mcp.list_tools",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(connected[0].1["ok"], true);

    let slow = invoke(
        &room,
        root.path(),
        30,
        "mcp.call_tool",
        json!({
            "name": "echo",
            "arguments": { "text": "cancel-me", "delay_ms": 2000 },
            "timeout_ms": 5000
        }),
        &formation,
        &kind,
    );
    let control = async {
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let started = std::time::Instant::now();
        let status = invoke(
            &room,
            root.path(),
            31,
            "mcp.status",
            json!({}),
            &formation,
            &kind,
        )
        .await;
        let request_id = status[0].1["body"]["in_flight_request_ids"][0]
            .as_i64()
            .unwrap();
        let cancelled = invoke(
            &room,
            root.path(),
            32,
            "mcp.cancel_request",
            json!({ "request_id": request_id, "reason": "test cancellation" }),
            &formation,
            &kind,
        )
        .await;
        (status, cancelled, started.elapsed())
    };
    let (slow, (status, cancelled, elapsed)) = tokio::join!(slow, control);
    assert_eq!(status[0].1["ok"], true);
    assert_eq!(cancelled[0].1["ok"], true);
    assert_eq!(slow[0].1["ok"], false);
    assert!(elapsed < std::time::Duration::from_millis(400));
}

#[tokio::test]
async fn room_release_terminates_an_in_flight_call_without_waiting_for_its_timeout() {
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    let interface_var = root.path().join("interface-var");
    write_interface(&interface_dir);
    let room = McpStdioRoom::new(ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir: interface_dir.clone(),
        descriptor_snapshot: None,
        var_dir: interface_var.clone(),
        durable_var_dir: Some(interface_var.join("durable-formation")),
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    });
    let (formation, kind) = formation(&interface_dir);
    WorkerEnvironmentRecordStore::new(&interface_var)
        .activate(&formation, 1)
        .unwrap();
    let connected = invoke(
        &room,
        root.path(),
        59,
        "mcp.list_tools",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(connected[0].1["ok"], true);

    let slow = invoke(
        &room,
        root.path(),
        60,
        "mcp.call_tool",
        json!({
            "name": "echo",
            "arguments": { "text": "release-me", "delay_ms": 2000 },
            "timeout_ms": 5000
        }),
        &formation,
        &kind,
    );
    let release = async {
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let started = std::time::Instant::now();
        if let Err(error) = room.release(&formation, &kind).await {
            match error {
                ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                    panic!("MCP stdio room release failed: {error}")
                }
            }
        }
        started.elapsed()
    };
    let (slow, elapsed) = tokio::join!(slow, release);
    assert_eq!(slow[0].1["ok"], false);
    assert!(
        elapsed < std::time::Duration::from_millis(400),
        "room release waited {elapsed:?}"
    );
}
