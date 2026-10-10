use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_interfaces::{
    GrantProjection, InterfaceArtifactFile, InterfaceDescriptor, InvocationMetadata,
    RuntimeInvocationMetadata,
};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_tool_registry::{load_tool_descriptor_from_dir, ToolDescriptor};
use octopus_worker_env::{
    WorkerEnvKindRegistry, WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId,
};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../..")
        .canonicalize()
        .unwrap()
}

fn canonical_hash(value: &impl Serialize) -> String {
    let value = serde_json::to_value(value).unwrap();
    ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&value)).to_hex()
}

async fn write_message(stdin: &mut tokio::process::ChildStdin, value: &Value) {
    let mut bytes = serde_json::to_vec(value).unwrap();
    bytes.push(b'\n');
    stdin.write_all(&bytes).await.unwrap();
    stdin.flush().await.unwrap();
}

async fn read_message(
    stdout: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
) -> Value {
    let line = tokio::time::timeout(std::time::Duration::from_secs(10), stdout.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn file_write_process_reports_active_before_completion_and_accepts_release() {
    file_write_process_case(false).await;
}

#[tokio::test]
async fn ordered_file_write_process_reports_durable_partial_effects() {
    file_write_process_case(true).await;
}

async fn file_write_process_case(ordered: bool) {
    let repository_root = repository_root();
    let interface_dir = repository_root.join("registry/interface/system_room");
    let interface: InterfaceDescriptor = octopus_descriptors::parse_strict(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    )
    .unwrap();
    let life: Value =
        serde_json::from_slice(&std::fs::read(interface_dir.join("life.json")).unwrap()).unwrap();
    let host_io_limits_json = serde_json::to_string(&life["processes"][0]["io_limits"]).unwrap();
    let tool_root = repository_root.join("registry/tool");
    let descriptor: ToolDescriptor = load_tool_descriptor_from_dir(&tool_root, "file.write")
        .unwrap()
        .unwrap();
    let kind_root = repository_root.join("registry/worker_environment_kind");
    let registry = WorkerEnvKindRegistry::load_from_dir(&kind_root).unwrap();
    let kind_id = WorkerEnvironmentKindId::new("system_room");
    let (kind, _) = registry.get(&kind_id).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let run_records_root = temp.path().join("runs");
    let ee_instances_root = temp.path().join("ees");
    let process_var_dir = temp.path().join("process-var");
    let durable_var_dir = temp.path().join("durable-var");
    std::fs::create_dir_all(&run_records_root).unwrap();
    std::fs::create_dir_all(&ee_instances_root).unwrap();
    std::fs::create_dir_all(&process_var_dir).unwrap();
    std::fs::create_dir_all(&durable_var_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&durable_var_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let durable_var_dir = std::fs::canonicalize(durable_var_dir).unwrap();
    let root_slots = BTreeMap::from([
        ("run_records_root".to_string(), run_records_root.clone()),
        (
            "registry_interface_root".to_string(),
            repository_root.join("registry/interface"),
        ),
        ("ee_instances_root".to_string(), ee_instances_root),
    ]);
    let tool_hashes = BTreeMap::from([("file.write".to_string(), canonical_hash(&descriptor))]);
    let tool_process_bins =
        BTreeMap::from([("file.write".to_string(), descriptor.process.bin.clone())]);
    let granted_tools = vec!["file.write".to_string()];
    let granted_kinds = vec![kind_id.clone()];
    let allowance = root_slots.keys().cloned().collect::<BTreeSet<_>>();
    let door = InterfaceId::new("system_room");
    let worker = InterfaceId::new("system_worker_flash");
    let formation = registry
        .form(&WorkerEnvironmentFormationRequest {
            door_interface: &door,
            door_descriptor_hash: &canonical_hash(&interface),
            door_driver_ref: &interface.interface_driver_ref,
            tool: "file.write",
            tool_descriptor_hashes: &tool_hashes,
            tool_process_bins: &tool_process_bins,
            tool_worker_environment_kinds: &descriptor.supported_worker_environment_kinds,
            worker: &worker,
            ee: "ee_test",
            run: "run_test",
            granted_worker_environment_kinds: &granted_kinds,
            granted_tools: &granted_tools,
            door_root_bindings: &root_slots,
            door_instance_dir: &interface_dir,
            ee_root_slot_allowance: Some(&allowance),
        })
        .unwrap();
    let target = run_records_root.join("created.json");
    let peer = run_records_root.join("peer.json");
    std::fs::write(&peer, "peer generation 2").unwrap();
    if ordered { std::fs::write(&target, "before\n").unwrap(); }
    let arguments = if ordered { json!({"ordered":true,"writes":[
        {"path":target,"content":"{\"created\":true}\n","expected_before_content":"before\n","expected_before_hash":ContentHash::of_bytes(b"before\n").to_hex()},
        {"path":peer,"content":"stale edit","expected_before_content":"peer generation 1","expected_before_hash":ContentHash::of_bytes(b"peer generation 1").to_hex()}
    ]}) } else { json!({"path":target,"content":"{\"created\":true}\n","expected_before_hash":null}) };
    let payload = serde_json::to_vec(&json!({
        "tool": "file.write",
        "arguments": arguments,
        "correlation_id": "write-1"
    }))
    .unwrap();
    let content = ContentHash::of_bytes(&payload);
    let artifact = ArtifactRef {
        id: ArtifactId::new(format!("tool.call:{}", content.to_hex())),
        content,
    };
    let input_artifact = InterfaceArtifactFile::write(
        process_var_dir.join("invocations/1/artifact"),
        "tool.call",
        &payload,
    )
    .unwrap();
    let metadata = InvocationMetadata::new(GrantProjection {
        tools: granted_tools,
        worker_environment_kinds: vec![kind_id.as_str().to_string()],
        ee_root_slot_allowance: Some(allowance.into_iter().collect()),
        ..GrantProjection::default()
    })
    .with_runtime(RuntimeInvocationMetadata {
        run: RunId::new("run_test"),
        ee: EeId::new("ee_test"),
        worker,
        checkpoint: CheckpointId::new("checkpoint_test"),
        interface: door.clone(),
        epoch: 0,
        run_record_hash: None,
    });
    let descriptor_json = serde_json::to_string(&interface).unwrap();
    let root_slots_json = serde_json::to_string(&root_slots).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_octopus-file-write-tool"))
        .env("OCTOPUS_INTERFACE_ID", door.as_str())
        .env("OCTOPUS_INTERFACE_DIR", &interface_dir)
        .env("OCTOPUS_INTERFACE_DESCRIPTOR_JSON", descriptor_json)
        .env("OCTOPUS_INTERFACE_HOST_IO_LIMITS", host_io_limits_json)
        .env("OCTOPUS_VAR_DIR", &process_var_dir)
        .env("OCTOPUS_TOOL_PROCESS_DURABLE_VAR_DIR", &durable_var_dir)
        .env("OCTOPUS_PRIVATE_READS_JSON", "{}")
        .env("OCTOPUS_ROOT_SLOTS_JSON", root_slots_json)
        .env("TMPDIR", &process_var_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut stdout = tokio::io::BufReader::new(stdout).lines();
    write_message(
        &mut stdin,
        &json!({
            "kind": "invoke",
            "request_id": 1,
            "descriptor": descriptor,
            "envelope": {
                "artifact": artifact,
                "label": "tool.call",
                "manifest_path": input_artifact.manifest_path(),
                "artifact_bindings": [],
                "metadata": metadata,
                "source_movement_seq": 41,
                "worker_environment": formation,
                "worker_environment_kind": &*kind
            }
        }),
    )
    .await;
    let lifecycle = read_message(&mut stdout).await;
    assert_eq!(lifecycle["kind"], "worker_environment_lifecycle");
    assert_eq!(lifecycle["lifecycle"], if ordered { "failed" } else { "active" }, "{lifecycle}");
    assert_eq!(
        lifecycle["worker_environment_id"],
        formation.worker_environment_id.as_str()
    );
    write_message(
        &mut stdin,
        &json!({
            "kind": "worker_environment_lifecycle_recorded",
            "request_id": 1,
            "event_id": lifecycle["event_id"],
            "error": null
        }),
    )
    .await;
    let completed = read_message(&mut stdout).await;
    assert_eq!(completed["kind"], "complete");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "{\"created\":true}\n"
    );
    if ordered {
        assert_eq!(std::fs::read_to_string(&peer).unwrap(), "peer generation 2");
        let effects = durable_var_dir.join("file-write-effects/ordered").join(content.to_hex());
        let applied: Value = serde_json::from_slice(&std::fs::read(effects.join("0.receipt.json")).unwrap()).unwrap();
        let refused: Value = serde_json::from_slice(&std::fs::read(effects.join("1.receipt.json")).unwrap()).unwrap();
        assert_eq!(applied["outcome"], "applied");
        assert_eq!(refused["outcome"], "refused");
        assert_eq!(refused["observed_hash"], ContentHash::of_bytes(b"peer generation 2").to_hex());
        let returned = InterfaceArtifactFile::new(completed["artifact_manifest_paths"][0].as_str().unwrap()).unwrap()
            .read_bounded(1024 * 1024, 8 * 1024 * 1024).unwrap();
        let result: Value = serde_json::from_slice(&returned.payload_bytes).unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], "file_write_incomplete");
        assert_eq!(result["error"]["details"]["file_write"]["writes"], json!([applied,refused]));
    } else { assert_eq!(
        std::fs::read_dir(durable_var_dir.join("file-write-effects"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(
                |entry| entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            )
            .count(),
        1
    ); }
    assert!(!process_var_dir.join("file-write-effects").exists());
    write_message(
        &mut stdin,
        &json!({
            "kind": "release_worker_environment",
            "request_id": 2,
            "formation": formation,
            "worker_environment_kind": &*kind,
            "mode": "live_owner"
        }),
    )
    .await;
    let released = read_message(&mut stdout).await;
    assert_eq!(released["kind"], "worker_environment_released");
    drop(stdin);
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
}
