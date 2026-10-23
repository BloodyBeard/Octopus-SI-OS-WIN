use super::*;
use crate::run_composition::{composition_candidate_writes, plan_run_composition};
use octopus_ledger::{
    Endpoint, GrantProjection, InvocationMetadata, Lamport, Movement, MovementKind,
    RuntimeInvocationMetadata, Seq, encode_durable_movement_row,
};
use std::time::Instant;

mod fixture {
    extern crate self as octopus_operator_chat_client;
    include!("composition_fixture.rs");
}

fn setup(root: &Path, count: usize) -> Vec<NativeRunAdoptionRequest> {
    let mut request = fixture::client_fixture(root);
    request.roots.interface_private_state_root = root.join("interfaces");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../../registry/interface/world_provider_codex/interface.json");
    let declared: Value = serde_json::from_slice(&std::fs::read(source).unwrap()).unwrap();
    let source = root.join("registry/interface/world_provider_fixture/interface.json");
    let mut provider: Value = serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
    provider["interface_driver_config"]["private_config"]["run_affordance_profile"] =
        declared["interface_driver_config"]["private_config"]["run_affordance_profile"].clone();
    std::fs::write(&source, provider.to_string()).unwrap();
    let mut requests = Vec::new();
    for i in 0..count {
        request.identity_seed = format!("adoption-batch-{i}");
        request.communication.receive_interface_id = format!("system_worker_private_batch_{i}");
        let plan = plan_run_composition(&request).unwrap();
        for write in composition_candidate_writes(&plan) {
            std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
            std::fs::write(write.path, write.content).unwrap();
        }
        let provider = request
            .roots
            .registry_root
            .join("interface")
            .join(plan.provider_interface_id)
            .join("interface.json");
        requests.push(NativeRunAdoptionRequest {
            roots: request.roots.clone(),
            ledger_root: root.join("ledger"),
            client_template_interface: "world_operator_chat".into(),
            run_id: plan.run_id,
            expected_run_hash: sha256_hex(&std::fs::read(plan.run_write.path).unwrap()),
            expected_provider_hash: sha256_hex(&std::fs::read(provider).unwrap()),
        });
    }
    provider["interface_driver_config"]["private_config"]["input_messages"] =
        json!({"fixture.request":{"text_fields":["question"],"identity_required":false}});
    std::fs::write(source, provider.to_string()).unwrap();
    std::fs::create_dir_all(root.join("ledger/metadata")).unwrap();
    std::fs::create_dir_all(root.join("ledger/blobs")).unwrap();
    let row = Movement {
        seq: Seq(0),
        lamport: Lamport(0),
        recorded_at_unix_ms: 1,
        trace: octopus_ledger::TraceId("history".into()),
        kind: MovementKind::InterfaceOutbound,
        from: Endpoint::Interface("background".into()),
        to: Endpoint::Interface("background_door".into()),
        artifact: Some(ArtifactRef {
            id: "history".into(),
            content: ContentHash::of_bytes(b"history"),
        }),
        invocation_metadata: Some(octopus_ledger::MovementMetadata::Invocation(
            InvocationMetadata::new(GrantProjection {
                tools: (0..1100).map(|i| format!("tool.{i}")).collect(),
                ..Default::default()
            })
            .with_runtime(RuntimeInvocationMetadata {
                run: "history".into(),
                ee: "ee".into(),
                worker: "worker".into(),
                checkpoint: "checkpoint".into(),
                interface: "background_door".into(),
                epoch: 0,
                run_record_hash: Some("a".repeat(64)),
            }),
        )),
    };
    let row = encode_durable_movement_row(&row, 0, |bytes| {
        let hash = ContentHash::of_bytes(bytes);
        std::fs::write(root.join("ledger/metadata").join(hash.to_hex()), bytes).unwrap();
        Ok(hash)
    })
    .unwrap();
    std::fs::write(root.join("ledger/movements.jsonl"), format!("{row}\n")).unwrap();
    requests
}

#[test]
fn adoption_batch_matches_fresh_plans_and_commits_sequentially() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut requests = setup(&root, 3);
    let path = requests[2]
        .roots
        .run_records_root
        .join(format!("{}.json", requests[2].run_id));
    let mut busy: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    busy["status"] = json!("running");
    std::fs::write(&path, busy.to_string()).unwrap();
    requests[2].expected_run_hash = sha256_hex(&std::fs::read(path).unwrap());
    let first = &requests[0];
    let command = json!({"operation":"plan","request_id":1,"request":{
        "roots":first.roots,"ledger_root":first.ledger_root,"client_template_interface":first.client_template_interface,
        "run_id":first.run_id,"expected_run_hash":first.expected_run_hash,"expected_provider_hash":first.expected_provider_hash}});
    let frames = format!("{command}\n{{\"operation\":\"finish\",\"request_id\":2}}\n");
    let mut output = Vec::new();
    serve(&mut std::io::Cursor::new(frames), &mut output).unwrap();
    let frames = output
        .split(|b| *b == b'\n')
        .filter(|row| !row.is_empty())
        .map(|row| serde_json::from_slice::<Value>(row).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), 2);
    assert_eq!(
        frames[0]["plan"],
        serde_json::to_value(plan_existing_adoption(first).unwrap()).unwrap()
    );
    assert_eq!(
        frames[1],
        json!({"schema_version":1,"request_id":2,"finished":true})
    );
    let mut batch = NativeAdoptionBatch::default();
    for (i, request) in requests.iter().enumerate() {
        let expected = serde_json::to_value(plan_existing_adoption(request).unwrap()).unwrap();
        let actual = serde_json::to_value(batch.plan(request).unwrap()).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            actual["status"],
            if i == 2 { "deferred" } else { "planned" }
        );
        for write in actual["writes"].as_array().unwrap() {
            let path = Path::new(write["path"].as_str().unwrap());
            assert_eq!(
                sha256_hex(&std::fs::read(path).unwrap()),
                write["expected_before_hash"]
            );
            std::fs::write(path, write["content"].as_str().unwrap()).unwrap();
        }
        if i == 0 {
            let source = root.join("registry/interface/world_provider_fixture/interface.json");
            let mut next: Value = serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
            next["interface_driver_config"]["private_config"]["input_messages"]["fixture.request"]
                ["text_fields"] = json!(["new_question"]);
            std::fs::write(source, next.to_string()).unwrap();
        }
    }
    batch.finish().unwrap();
    assert!(
        batch
            .plan(&requests[0])
            .err()
            .unwrap()
            .contains("repeats a run")
    );
}

#[test]
fn adoption_batch_rejects_changed_prefix_grant_and_per_run_bounds() {
    for mutation in ["append", "prefix", "grant", "row_bound", "roots"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut requests = setup(&root, 2);
        let mut batch = NativeAdoptionBatch::default();
        let first = batch.plan(&requests[0]).unwrap();
        assert!(matches!(first.status, NativeRunAdoptionStatus::Planned));
        let ledger = root.join("ledger/movements.jsonl");
        match mutation {
            "append" => {
                let mut bytes = std::fs::read(&ledger).unwrap();
                bytes.extend_from_slice(b"{}\n");
                std::fs::write(ledger, bytes).unwrap();
            }
            "prefix" => {
                let mut bytes = std::fs::read(&ledger).unwrap();
                bytes[0] = b' ';
                std::fs::write(ledger, bytes).unwrap();
            }
            "grant" => {
                let path = std::fs::read_dir(root.join("ledger/metadata"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                let mut bytes = std::fs::read(&path).unwrap();
                let i = bytes.iter().position(|b| *b == b'.').unwrap();
                bytes[i] = b'_';
                std::fs::write(path, bytes).unwrap();
                let pin = first
                    .preimages
                    .iter()
                    .find(|pin| pin.path.parent() == Some(root.join("ledger/metadata").as_path()))
                    .expect("checked grant must be a commit dependency");
                assert_ne!(
                    Some(sha256_hex(&std::fs::read(&pin.path).unwrap())),
                    pin.sha256
                );
            }
            "row_bound" => {
                let run: Value = serde_json::from_slice(
                    &std::fs::read(
                        requests[1]
                            .roots
                            .run_records_root
                            .join(format!("{}.json", requests[1].run_id)),
                    )
                    .unwrap(),
                )
                .unwrap();
                let provider = run["interface_state"]
                    .as_object()
                    .unwrap()
                    .values()
                    .find_map(|state| state.get("provider_runtime_interface"))
                    .unwrap()
                    .as_str()
                    .unwrap();
                let path = root
                    .join("registry/interface")
                    .join(provider)
                    .join("life.json");
                let mut life: Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                life["processes"][0]["io_limits"]["max_control_frame_bytes"] = json!(128);
                std::fs::write(path, life.to_string()).unwrap();
            }
            "roots" => {
                requests[1].ledger_root = root.join("different-ledger");
            }
            _ => unreachable!(),
        }
        assert!(batch.plan(&requests[1]).is_err(), "{mutation} was accepted");
        assert!(
            batch
                .plan(&requests[0])
                .err()
                .unwrap()
                .contains("already failed")
        );
    }
    for frame in [
        b"{\"operation\":\"finish\",\"request_id\":2}\n".to_vec(),
        b"{\"operation\":\"finish\",\"request_id\":1,\"extra\":true}\n".to_vec(),
        b"{\"operation\":\"finish\",\"request_id\":1}".to_vec(),
        vec![b' '; FRAME_BYTES as usize + 1],
    ] {
        assert!(serve(&mut std::io::Cursor::new(frame), &mut Vec::new()).is_err());
    }
}

#[test]
fn adoption_batch_publication_refuses_grant_tamper_and_late_cancel() {
    use std::process::{Command, Stdio};
    for mode in ["complete", "grant", "cancel"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let requests = setup(&root, 1);
        let plan = plan_existing_adoption(&requests[0]).unwrap();
        let payload =
            json!({"home":root,"plan":plan,"mode":mode,"runHash":requests[0].expected_run_hash});
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/adoption_publication.cjs");
        let mut child = Command::new("node")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&payload).unwrap())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        if mode == "complete" {
            assert_eq!(result, json!({"commits":1,"changed":true}));
        } else {
            assert_eq!(result["commits"], 0);
            assert!(
                result["error"]
                    .as_str()
                    .unwrap()
                    .contains(if mode == "grant" {
                        "preimage changed"
                    } else {
                        "cancelled before publication"
                    }),
                "{result}"
            );
        }
    }
}

#[test]
#[ignore = "bounded read-only source-prefix benchmark; requires OCTOPUS_ADOPTION_BENCH_LEDGER"]
fn benchmark_native_adoption_batch_fixed_prefix() {
    use std::io::Read;
    let started = Instant::now();
    let source = PathBuf::from(
        std::env::var_os("OCTOPUS_ADOPTION_BENCH_LEDGER").expect("explicit source ledger"),
    );
    let file = std::fs::File::open(&source).unwrap();
    let length = std::env::var_os("OCTOPUS_ADOPTION_BENCH_PREFIX_BYTES")
        .map(|value| value.to_str().unwrap().parse::<u64>().unwrap())
        .unwrap_or(file.metadata().unwrap().len());
    assert!(
        length <= 512 * 1024 * 1024,
        "benchmark prefix exceeds 512 MiB bound"
    );
    let mut bytes = Vec::new();
    file.take(length).read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes.len() as u64, length);
    bytes.truncate(bytes.iter().rposition(|b| *b == b'\n').unwrap() + 1);
    let prefix_hash = ContentHash::of_bytes(&bytes);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let requests = setup(&root, 3);
    let mut grants = BTreeSet::new();
    let mut rows = 0;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        assert!(line.len() <= 1 << 20);
        let row: Value = serde_json::from_slice(line).unwrap();
        if let Some(hash) = row
            .pointer("/invocation_metadata/grant_content")
            .and_then(Value::as_str)
        {
            grants.insert(ContentHash::from_hex(hash).unwrap());
        }
        rows += 1;
    }
    for hash in &grants {
        let bytes = std::fs::read(
            source
                .parent()
                .unwrap()
                .join("metadata")
                .join(hash.to_hex()),
        )
        .unwrap();
        assert_eq!(ContentHash::of_bytes(&bytes), *hash);
        std::fs::write(root.join("ledger/metadata").join(hash.to_hex()), bytes).unwrap();
    }
    std::fs::write(root.join("ledger/movements.jsonl"), &bytes).unwrap();
    let spotlight = std::env::var_os("OCTOPUS_ADOPTION_BENCH_SPOTLIGHT").is_some();
    if spotlight {
        let log =
            octopus_ledger_store::DurableLog::open(root.join("ledger/movements.jsonl")).unwrap();
        let rows = log.try_boot_movements_from(Seq(0)).unwrap();
        octopus_ledger_index::LedgerSqliteIndex::open_healthy_from_movements(
            &root.join("ledger/ledger-index.sqlite"),
            &log,
            &rows,
        )
        .unwrap();
        assert_eq!(
            ContentHash::of_bytes(&std::fs::read(root.join("ledger/movements.jsonl")).unwrap()),
            prefix_hash
        );
    }
    let mut batch = NativeAdoptionBatch::default();
    let mut independent_ms = 0.0;
    let mut batch_ms = 0.0;
    for (i, request) in requests.iter().enumerate() {
        assert!(
            started.elapsed().as_secs() < 300,
            "benchmark exhausted its five-minute budget"
        );
        let mut independent = NativeAdoptionBatch::default();
        let start = Instant::now();
        let expected = independent.plan(request).unwrap();
        let before_ms = start.elapsed().as_secs_f64() * 1000.0;
        independent_ms += before_ms;
        let start = Instant::now();
        let actual = batch.plan(request).unwrap();
        let after_ms = start.elapsed().as_secs_f64() * 1000.0;
        batch_ms += after_ms;
        assert!(matches!(actual.status, NativeRunAdoptionStatus::Planned));
        let expected = ContentHash::of_bytes(&serde_json::to_vec(&expected).unwrap());
        assert_eq!(
            ContentHash::of_bytes(&serde_json::to_vec(&actual).unwrap()),
            expected
        );
        for pin in &actual.preimages {
            let hash = match std::fs::read(&pin.path) {
                Ok(bytes) => Some(sha256_hex(&bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("{error}"),
            };
            assert_eq!(hash, pin.sha256);
        }
        for write in actual.writes {
            std::fs::write(write.path, write.content).unwrap();
        }
        let phases = |metrics: &PlanningMetrics| {
            metrics
                .phases
                .iter()
                .map(|(name, value)| (*name, value.as_secs_f64() * 1000.0))
                .collect::<BTreeMap<_, _>>()
        };
        println!(
            "{}",
            json!({"item":i,"independent_ms":before_ms,"batch_ms":after_ms,
            "independent_phases_ms":phases(&independent.metrics),"batch_cumulative_phases_ms":phases(&batch.metrics)})
        );
    }
    let start = Instant::now();
    batch.finish().unwrap();
    let finish_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut reread = Vec::new();
    std::fs::File::open(&source)
        .unwrap()
        .take(bytes.len() as u64)
        .read_to_end(&mut reread)
        .unwrap();
    assert_eq!(ContentHash::of_bytes(&reread), prefix_hash);
    println!(
        "{}",
        json!({"prefix_bytes":bytes.len(),"prefix_hash":prefix_hash.to_hex(),"rows":rows,"grants":grants.len(),"spotlight":spotlight,
        "runs":requests.len(),"independent_total_ms":independent_ms,"batch_total_ms":batch_ms+finish_ms,"finish_ms":finish_ms,
        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"all_plans_equal":true})
    );
}
