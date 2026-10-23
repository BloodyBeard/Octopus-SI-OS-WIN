#![cfg(unix)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_exec::{
    run_main_authenticated, DirectExecServerAuth, Environment, ExecServerRuntimePaths,
};
use octopus_interfaces::{
    GrantProjection, InterfaceArtifactFile, InterfaceInvocationEnvelope, InvocationMetadata,
    RuntimeInvocationMetadata,
};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_remote_exec_room::RemoteExecRoom;
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessInvocation,
    WorkerEnvironmentLifecycleRecorder, WorkerEnvironmentLiveStateRecorder,
};
use octopus_tool_registry::{load_tool_descriptor_from_dir, ToolDescriptor};
use octopus_util_pathuri::PathUri;
use octopus_worker_env::{
    WorkerEnvKindRegistry, WorkerEnvKindSpec, WorkerEnvironmentFormation,
    WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId, WorkerEnvironmentRecordStore,
};
use serde_json::{json, Value};

const DOOR: &str = "system_remote_exec";
const WORKER: &str = "system_worker_coder_max";
const KIND: &str = "remote_exec_server";

struct TestLifecycleRecorder {
    root: PathBuf,
}

impl WorkerEnvironmentLifecycleRecorder for TestLifecycleRecorder {
    fn record<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        lifecycle: octopus_worker_env::WorkerEnvironmentLifecycle,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let store = WorkerEnvironmentRecordStore::new(&self.root);
            let result = match lifecycle {
                octopus_worker_env::WorkerEnvironmentLifecycle::Active => {
                    store.activate(formation, 1)
                }
                octopus_worker_env::WorkerEnvironmentLifecycle::Released => {
                    store.release(formation, 1)
                }
                octopus_worker_env::WorkerEnvironmentLifecycle::Failed => store.fail(formation, 1),
            };
            result.map(|_| ()).map_err(|error| error.to_string())
        })
    }
}

impl WorkerEnvironmentLiveStateRecorder for TestLifecycleRecorder {
    fn record_live_state<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        state: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            WorkerEnvironmentRecordStore::new(&self.root)
                .write_live_state(&formation.worker_environment_id, state)
                .map(|_| ())
                .map_err(|error| error.to_string())
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
        "remote.status",
        "remote.exec",
        "remote.file_read",
        "remote.file_write",
        "remote.file_list",
        "remote.file_stat",
        "remote.file_delete",
        "remote.file_copy",
        "remote.directory_create",
        "remote.process_start",
        "remote.process_read",
        "remote.process_write",
        "remote.process_interrupt",
        "remote.process_terminate",
        "remote.process_list",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn raw_descriptor(id: &str) -> ToolDescriptor {
    load_tool_descriptor_from_dir(&registry_root().join("tool"), id)
        .unwrap()
        .unwrap()
}

fn effective_descriptor(id: &str) -> ToolDescriptor {
    let mut descriptor = raw_descriptor(id);
    descriptor.interface = InterfaceId::new(DOOR);
    descriptor
}

fn write_interface(path: &Path, websocket_url: &str, workspace: &Path, auth_handle: &str) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(
        path.join("interface.json"),
        serde_json::to_vec_pretty(&json!({
            "id": DOOR,
            "interface_type": "system",
            "role": "system",
            "direction": "bidirectional",
            "source_names": ["remote_exec"]
        }))
        .unwrap(),
    )
    .unwrap();
    let workspace_uri = PathUri::from_path(workspace).unwrap().to_string();
    std::fs::write(
        path.join("remote_exec.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "websocket_url": websocket_url,
            "auth_secret_handle": auth_handle,
            "workspace_uri": workspace_uri,
            "request_timeout_ms": 5000,
            "max_output_bytes": 1048576,
            "max_file_bytes": 1048576,
            "max_stdin_bytes": 1048576,
            "max_processes": 8,
            "max_list_entries": 1000,
            "env_inherit": "core",
            "env_override_allowlist": ["TERM"]
        }))
        .unwrap(),
    )
    .unwrap();
}

fn formation(door_instance_dir: &Path) -> (WorkerEnvironmentFormation, WorkerEnvKindSpec) {
    let registry =
        WorkerEnvKindRegistry::load_from_dir(&registry_root().join("worker_environment_kind"))
            .unwrap();
    let tools = tool_ids();
    let hashes = tools
        .iter()
        .map(|tool| {
            let value = serde_json::to_value(raw_descriptor(tool)).unwrap();
            let hash = blake3::hash(&octopus_descriptors::canonical_bytes(&value))
                .to_hex()
                .to_string();
            (tool.clone(), hash)
        })
        .collect::<BTreeMap<_, _>>();
    let process_bins = tools
        .iter()
        .map(|tool| (tool.clone(), raw_descriptor(tool).process.bin))
        .collect::<BTreeMap<_, _>>();
    let kind_id = WorkerEnvironmentKindId::new(KIND);
    let kinds = vec![kind_id.clone()];
    let door = InterfaceId::new(DOOR);
    let worker = InterfaceId::new(WORKER);
    let formed = registry
        .form(&WorkerEnvironmentFormationRequest {
            door_additional_root_bindings: &Default::default(),
            door_interface: &door,
            door_descriptor_hash: "remote-exec-test-door-hash",
            door_driver_ref: "system_remote_exec",
            tool: "remote.status",
            tool_descriptor_hashes: &hashes,
            tool_process_bins: &process_bins,
            tool_worker_environment_kinds: &kinds,
            worker: &worker,
            ee: "ee_remote_test",
            run: "run_remote_test",
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
        run: RunId::new("run_remote_test"),
        ee: EeId::new("ee_remote_test"),
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
    room: &RemoteExecRoom,
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
    formation: &WorkerEnvironmentFormation,
    kind: &WorkerEnvKindSpec,
) -> Value {
    let invocation = ToolProcessInvocation {
        request_id: sequence,
        descriptor: effective_descriptor(tool),
        envelope: envelope(root, sequence, tool, arguments),
        source_movement_seq: sequence,
        worker_environment: Some(formation.clone()),
        worker_environment_kind: Some(kind.clone()),
    };
    let outputs = room
        .receive(
            invocation,
            &TestLifecycleRecorder {
                root: root.join("interface-var"),
            },
        )
        .await
        .unwrap_or_else(|error| match error {
            ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                panic!("remote exec room invocation failed: {error}")
            }
        });
    serde_json::from_slice(&outputs[0].read_payload_bytes().unwrap()).unwrap()
}

fn reserve_address() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

#[tokio::test]
async fn formed_remote_room_executes_and_reads_and_writes_through_the_exec_server() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("remote-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let address = reserve_address();
    let websocket_url = format!("ws://{address}");
    let auth_secret = root.path().join("server-auth.key");
    std::fs::write(
        &auth_secret,
        b"remote-exec-room-test-secret-32-bytes-minimum",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&auth_secret).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o600);
    std::fs::set_permissions(&auth_secret, permissions).unwrap();
    let server_auth = DirectExecServerAuth::load_private_file(&auth_secret).unwrap();
    let runtime_paths =
        ExecServerRuntimePaths::new(std::env::current_exe().unwrap(), None).unwrap();
    let server_url = websocket_url.clone();
    let server = tokio::spawn(async move {
        let _ = run_main_authenticated(&server_url, runtime_paths, server_auth).await;
    });
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let unauthenticated = Environment::remote(websocket_url.clone()).unwrap();
    unauthenticated.start_connecting();
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(2),
        unauthenticated.wait_until_ready()
    )
    .await
    .unwrap()
    .is_err());

    let wrong_secret = root.path().join("wrong-auth.key");
    std::fs::write(
        &wrong_secret,
        b"a-different-remote-exec-secret-with-enough-bytes",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&wrong_secret).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o600);
    std::fs::set_permissions(&wrong_secret, permissions).unwrap();
    let wrong_auth = DirectExecServerAuth::load_private_file(&wrong_secret).unwrap();
    let wrong = Environment::remote_authenticated(websocket_url.clone(), wrong_auth).unwrap();
    wrong.start_connecting();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), wrong.wait_until_ready())
            .await
            .unwrap()
            .is_err()
    );

    let interface_dir = root.path().join("interface");
    let interface_var = root.path().join("interface-var");
    let durable_var = interface_var.join("durable-formation");
    std::fs::create_dir_all(&durable_var).unwrap();
    let mut permissions = std::fs::metadata(&durable_var).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
    std::fs::set_permissions(&durable_var, permissions).unwrap();
    std::fs::create_dir_all(interface_var.join("secrets")).unwrap();
    let client_secret = interface_var.join("secrets/remote-exec.auth");
    std::fs::copy(&auth_secret, &client_secret).unwrap();
    let mut permissions = std::fs::metadata(&client_secret).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o600);
    std::fs::set_permissions(&client_secret, permissions).unwrap();
    write_interface(
        &interface_dir,
        &websocket_url,
        &workspace,
        "secrets/remote-exec.auth",
    );
    let (formation, kind) = formation(&interface_dir);
    WorkerEnvironmentRecordStore::new(&interface_var)
        .activate(&formation, 1)
        .unwrap();
    let tool_environment = ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        descriptor_snapshot: None,
        var_dir: interface_var.clone(),
        durable_var_dir: Some(durable_var.clone()),
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::from([("secrets/remote-exec.auth".to_string(), client_secret)]),
        root_slots: BTreeMap::new(),
    };
    let room = RemoteExecRoom::new(tool_environment.clone());

    let status = invoke(
        &room,
        root.path(),
        1,
        "remote.status",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(status["body"]["connected"], true);

    let written = invoke(
        &room,
        root.path(),
        2,
        "remote.file_write",
        json!({
            "path": "nested/proof.txt",
            "content": "remote-file-proof",
            "create_parents": true
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(written["ok"], true, "{written}");

    let read = invoke(
        &room,
        root.path(),
        3,
        "remote.file_read",
        json!({ "path": "nested/proof.txt" }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(read["ok"], true, "{read}");
    assert_eq!(read["body"]["content"], "remote-file-proof");

    let listed = invoke(
        &room,
        root.path(),
        4,
        "remote.file_list",
        json!({ "path": "nested" }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(listed["ok"], true, "{listed}");
    assert_eq!(listed["body"]["entries"][0]["name"], "proof.txt");

    let copied = invoke(
        &room,
        root.path(),
        5,
        "remote.file_copy",
        json!({
            "source": "nested/proof.txt",
            "destination": "copied/proof.txt",
            "create_parents": true
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(copied["ok"], true, "{copied}");

    let stat = invoke(
        &room,
        root.path(),
        6,
        "remote.file_stat",
        json!({ "path": "copied/proof.txt" }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(stat["ok"], true, "{stat}");
    assert_eq!(stat["body"]["is_file"], true);

    std::fs::write(root.path().join("outside.txt"), "outside").unwrap();
    std::os::unix::fs::symlink(root.path(), workspace.join("escape")).unwrap();
    let escaped = invoke(
        &room,
        root.path(),
        7,
        "remote.file_read",
        json!({ "path": "escape/outside.txt" }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(escaped["ok"], false, "{escaped}");
    assert_ne!(escaped["error"]["message"].as_str().unwrap(), "");

    let executed = invoke(
        &room,
        root.path(),
        8,
        "remote.exec",
        json!({ "argv": ["/bin/sh", "-lc", "printf remote-process-proof"] }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(executed["ok"], true, "{executed}");
    assert_eq!(executed["body"]["exit_code"], 0);
    assert_eq!(executed["body"]["stdout"], "remote-process-proof");
    assert_eq!(
        executed["body"]["process_id"],
        format!("{}-8", formation.worker_environment_id.as_str())
    );

    let started = invoke(
        &room,
        root.path(),
        9,
        "remote.process_start",
        json!({
            "process_id": "interactive",
            "argv": ["/bin/sh", "-lc", "printf ready; read line; printf -- '-%s' \"$line\""]
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(started["ok"], true, "{started}");
    assert_eq!(started["body"]["generation"], 9);

    let written_stdin = invoke(
        &room,
        root.path(),
        10,
        "remote.process_write",
        json!({ "process_id": "interactive", "data": "done\n" }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(written_stdin["ok"], true, "{written_stdin}");

    let mut sequence = 11;
    let mut after_seq = None;
    let mut bytes = Vec::new();
    loop {
        let process_output = invoke(
            &room,
            root.path(),
            sequence,
            "remote.process_read",
            json!({
                "process_id": "interactive",
                "after_seq": after_seq,
                "wait_ms": 5000
            }),
            &formation,
            &kind,
        )
        .await;
        assert_eq!(process_output["ok"], true, "{process_output}");
        for chunk in process_output["body"]["chunks"].as_array().unwrap() {
            bytes.extend(
                BASE64
                    .decode(chunk["data_base64"].as_str().unwrap())
                    .unwrap(),
            );
        }
        after_seq = process_output["body"]["next_after_seq"].as_u64();
        sequence += 1;
        if process_output["body"]["closed"] == true {
            assert_eq!(process_output["body"]["state"], "released");
            assert_eq!(process_output["body"]["released"], true);
            break;
        }
    }
    assert_eq!(bytes, b"ready-done");

    let listed = invoke(
        &room,
        root.path(),
        sequence,
        "remote.process_list",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(listed["ok"], true, "{listed}");
    assert!(listed["body"]["processes"].as_array().unwrap().is_empty());
    sequence += 1;

    let started = invoke(
        &room,
        root.path(),
        sequence,
        "remote.process_start",
        json!({
            "process_id": "already-exited",
            "argv": ["/bin/sh", "-lc", "exit 0"]
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(started["ok"], true, "{started}");
    sequence += 1;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let released = invoke(
        &room,
        root.path(),
        sequence,
        "remote.process_terminate",
        json!({ "process_id": "already-exited" }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(released["ok"], true, "{released}");
    assert_eq!(released["body"]["state"], "released");
    assert_eq!(released["body"]["was_running"], false);
    assert_eq!(released["body"]["terminated"], false);
    assert_eq!(released["body"]["released"], true);
    sequence += 1;

    let started = invoke(
        &room,
        root.path(),
        sequence,
        "remote.process_start",
        json!({
            "process_id": "terminable",
            "argv": ["/bin/sh", "-lc", "sleep 30"]
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(started["ok"], true, "{started}");
    sequence += 1;

    let terminated = invoke(
        &room,
        root.path(),
        sequence,
        "remote.process_terminate",
        json!({ "process_id": "terminable" }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(terminated["ok"], true, "{terminated}");
    assert_eq!(terminated["body"]["state"], "terminated");
    assert_eq!(terminated["body"]["was_running"], true);
    assert_eq!(terminated["body"]["released"], true);
    sequence += 1;

    let timed_out = invoke(
        &room,
        root.path(),
        sequence,
        "remote.exec",
        json!({
            "argv": ["/bin/sh", "-lc", "sleep 30"],
            "timeout_ms": 50
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(timed_out["ok"], false, "{timed_out}");
    sequence += 1;

    let after_timeout = invoke(
        &room,
        root.path(),
        sequence,
        "remote.process_list",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(after_timeout["ok"], true, "{after_timeout}");
    assert!(after_timeout["body"]["processes"]
        .as_array()
        .unwrap()
        .is_empty());
    sequence += 1;

    let deleted = invoke(
        &room,
        root.path(),
        sequence,
        "remote.file_delete",
        json!({ "path": "copied", "recursive": true }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(deleted["ok"], true, "{deleted}");

    let live: Value = serde_json::from_slice(
        &std::fs::read(
            WorkerEnvironmentRecordStore::new(&interface_var)
                .live_state_path(&formation.worker_environment_id),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(live["status"], "ready");
    assert_eq!(live["calls"], sequence);
    assert_eq!(live["process_count"], 0);
    assert_eq!(live["viewer"]["protocol"], "remote_exec_server");

    server.abort();
}

#[tokio::test]
async fn persistent_process_resumes_exactly_and_server_loss_requires_resolution() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let address = reserve_address();
    let websocket_url = format!("ws://{address}");
    let auth_secret = root.path().join("server-auth.key");
    std::fs::write(
        &auth_secret,
        b"remote-exec-recovery-test-secret-32-bytes-minimum",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&auth_secret).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o600);
    std::fs::set_permissions(&auth_secret, permissions).unwrap();
    let server_auth = DirectExecServerAuth::load_private_file(&auth_secret).unwrap();
    let runtime_paths =
        ExecServerRuntimePaths::new(std::env::current_exe().unwrap(), None).unwrap();
    let server_url = websocket_url.clone();
    let server = tokio::spawn(async move {
        let _ = run_main_authenticated(&server_url, runtime_paths, server_auth).await;
    });
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let interface_dir = root.path().join("interface");
    let interface_var = root.path().join("interface-var");
    let durable_var = interface_var.join("durable-formation");
    std::fs::create_dir_all(interface_var.join("secrets")).unwrap();
    std::fs::create_dir_all(&durable_var).unwrap();
    let mut permissions = std::fs::metadata(&durable_var).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
    std::fs::set_permissions(&durable_var, permissions).unwrap();
    let client_secret = interface_var.join("secrets/remote-exec.auth");
    std::fs::copy(&auth_secret, &client_secret).unwrap();
    let mut permissions = std::fs::metadata(&client_secret).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o600);
    std::fs::set_permissions(&client_secret, permissions).unwrap();
    write_interface(
        &interface_dir,
        &websocket_url,
        &workspace,
        "secrets/remote-exec.auth",
    );
    let (formation, kind) = formation(&interface_dir);
    WorkerEnvironmentRecordStore::new(&interface_var)
        .activate(&formation, 1)
        .unwrap();
    let tool_environment = ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        descriptor_snapshot: None,
        var_dir: interface_var,
        durable_var_dir: Some(durable_var.clone()),
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::from([("secrets/remote-exec.auth".to_string(), client_secret)]),
        root_slots: BTreeMap::new(),
    };

    let room = RemoteExecRoom::new(tool_environment.clone());
    let started = invoke(
        &room,
        root.path(),
        1,
        "remote.process_start",
        json!({
            "process_id": "durable",
            "argv": ["/bin/sh", "-lc", "printf ready; sleep 30"]
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(started["ok"], true, "{started}");
    assert_eq!(started["body"]["generation"], 1);
    let remote_process_id = started["body"]["remote_process_id"]
        .as_str()
        .unwrap()
        .to_string();
    for record in [
        durable_var.join("remote-exec/session.json"),
        durable_var.join("remote-exec/processes/durable.json"),
    ] {
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(record).unwrap().permissions()
            ) & 0o777,
            0o600
        );
    }

    drop(room);
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let resumed_room = RemoteExecRoom::new(tool_environment.clone());
    let resumed = invoke(
        &resumed_room,
        root.path(),
        2,
        "remote.process_list",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(
        resumed["body"]["processes"][0]["state"], "active",
        "{resumed}"
    );
    assert_eq!(
        resumed["body"]["processes"][0]["remote_process_id"],
        remote_process_id
    );
    assert_eq!(resumed["body"]["processes"][0]["generation"], 1);

    drop(resumed_room);
    server.abort();
    let _ = server.await;
    let replacement_auth = DirectExecServerAuth::load_private_file(&auth_secret).unwrap();
    let replacement_url = websocket_url.clone();
    let replacement_runtime_paths =
        ExecServerRuntimePaths::new(std::env::current_exe().unwrap(), None).unwrap();
    let server = tokio::spawn(async move {
        let _ = run_main_authenticated(
            &replacement_url,
            replacement_runtime_paths,
            replacement_auth,
        )
        .await;
    });
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let lost_room = RemoteExecRoom::new(tool_environment);
    let lost = invoke(
        &lost_room,
        root.path(),
        3,
        "remote.process_list",
        json!({}),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(lost["ok"], true, "{lost}");
    assert_eq!(lost["body"]["processes"][0]["state"], "lost");
    assert_eq!(
        lost["body"]["processes"][0]["loss"]["code"],
        "remote_session_lost"
    );

    let refused = invoke(
        &lost_room,
        root.path(),
        4,
        "remote.process_start",
        json!({
            "process_id": "durable",
            "argv": ["/bin/sh", "-lc", "printf must-not-run"]
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(refused["ok"], false, "{refused}");

    let resolved = invoke(
        &lost_room,
        root.path(),
        5,
        "remote.process_terminate",
        json!({ "process_id": "durable" }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(resolved["ok"], true, "{resolved}");
    assert_eq!(resolved["body"]["state"], "resolved_lost");

    let restarted = invoke(
        &lost_room,
        root.path(),
        6,
        "remote.process_start",
        json!({
            "process_id": "durable",
            "argv": ["/bin/sh", "-lc", "sleep 30"]
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(restarted["ok"], true, "{restarted}");
    assert_eq!(restarted["body"]["generation"], 6);
    assert_ne!(
        restarted["body"]["remote_process_id"].as_str().unwrap(),
        remote_process_id
    );
    let terminated = invoke(
        &lost_room,
        root.path(),
        7,
        "remote.process_terminate",
        json!({ "process_id": "durable" }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(terminated["ok"], true, "{terminated}");
    let history = std::fs::read_dir(durable_var.join("remote-exec/history"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    assert!(history.iter().any(|record| {
        record.contains("\"status\":\"resolved\"")
            && record.contains("\"code\":\"remote_session_lost\"")
    }));
    server.abort();
}
