use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::os::unix::fs::MetadataExt;

use octopus_interface_transport::InterfaceLifeOwnership;
use octopus_ledger::{Endpoint, MovementKind};
use octopus_operator_chat_client::{run_composition::{RunCompositionPlan, plan_run_composition}, task_retirement::TaskRetirementContext};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[allow(dead_code)]
#[path = "support/composition_fixture.rs"]
mod support;

fn digest(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

struct RetirementFixture {
    plan: RunCompositionPlan,
    context: Value,
    removals: Vec<PathBuf>,
    preserved: Vec<PathBuf>,
}

fn start(context: &Value) -> (Child, BufReader<ChildStdout>, Option<Value>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_octopus-operator-chat-topology"))
        .arg("--hold-task-retirement").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    writeln!(child.stdin.as_mut().unwrap(), "{context}").unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let proof = (!line.is_empty()).then(|| serde_json::from_str(&line).unwrap());
    (child, reader, proof)
}

fn close(mut child: Child) {
    writeln!(child.stdin.as_mut().unwrap(), "{{\"operation\":\"release\"}}").unwrap();
    drop(child.stdin.take());
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
}

fn command(child: &mut Child, reader: &mut BufReader<ChildStdout>, command: Value) -> Value {
    writeln!(child.stdin.as_mut().unwrap(), "{command}").unwrap();
    let mut line = String::new();
    assert!(reader.read_line(&mut line).unwrap() > 0, "native command returned no response");
    serde_json::from_str(&line).unwrap()
}

fn fixture(root: &Path) -> RetirementFixture {
    let mut request = support::client_fixture(root);
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::rename(root.join("ee"), root.join("state/ee_instances")).unwrap();
    request.roots.ee_instances_root = root.join("state/ee_instances");
    request.roots.run_records_root = root.join("state/runs");
    request.roots.interface_private_state_root = root.join("interfaces");
    let plan = plan_run_composition(&request).unwrap();
    for write in plan.descriptor_writes.iter().chain(plan.run_writes.iter()).chain([&plan.run_write]) {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
        std::fs::write(&write.path, &write.content).unwrap();
    }
    for relative in ["interfaces", "interface-hand-in", "ledger/blobs"] { std::fs::create_dir_all(root.join(relative)).unwrap(); }
    std::fs::write(root.join("ledger/movements.jsonl"), b"").unwrap();
    let source: Value = serde_json::from_str(&plan.run_write.content).unwrap();
    let mut interfaces = BTreeSet::new();
    let mut records = Vec::new();
    let mut originals = Vec::new();
    for write in plan.run_writes.iter().chain([&plan.run_write]) {
        let record: Value = serde_json::from_str(&write.content).unwrap();
        let states = record["interface_state"].as_object().unwrap();
        let owner = if record["id"] == source["id"] { plan.provider_interface_id.clone() }
            else { states.keys().next().unwrap().clone() };
        interfaces.insert(record["worker"].as_str().unwrap().to_string());
        interfaces.extend(states.keys().cloned());
        let checkpoints = record["checkpoints"].as_array().unwrap().iter().chain(states.values()
            .filter_map(|state| state.pointer("/run_authoring_binding/checkpoints").and_then(Value::as_array)).flatten());
        for checkpoint in checkpoints {
            interfaces.insert(checkpoint["interface_binding"]["interface"].as_str().unwrap().into());
            for binding in checkpoint["outbound_interface_bindings"].as_array().unwrap() {
                interfaces.insert(binding["interface"].as_str().unwrap().into());
            }
        }
        let hash = digest(write.content.as_bytes());
        records.push(json!({"run":record["id"],"file":write.path,"expected_digest":hash,"worker":record["worker"],
            "ee":record["ee"],"role":if record["id"] == source["id"] {"source"} else {"writer"},
            "provider_interface":owner,"target_run":source["id"]}));
        originals.push(json!({"file":write.path,"expected_digest":hash,"content":write.content}));
    }
    records.sort_by_key(|row| row["run"].as_str().unwrap().to_string());
    originals.sort_by_key(|row| row["file"].as_str().unwrap().to_string());
    let selection = json!({"schema_version":1,"source_run":source["id"],"interface_ids":interfaces,"records":records});
    let mut context = json!({"operation_id":"retirement-fixture","roots":{"registry_root":root.join("registry"),
        "run_records_root":request.roots.run_records_root,"ledger_root":root.join("ledger"),"interface_private_state_root":request.roots.interface_private_state_root},
        "selection_digest":digest(&octopus_descriptors::canonical_bytes(&selection)),"selection":selection,
        "retired":originals.iter().map(|row| json!({"file":row["file"],"expected_digest":null})).collect::<Vec<_>>(),
        "originals":originals,"prior_proof":null,
        "directory_inventory":{"schema_version":1,"removals":[],"originals":[],"retired":[]}});
    let mut generated = BTreeSet::new();
    for write in &plan.descriptor_writes {
        for relative in ["registry/interface", "registry/tool"] {
            let base = root.join(relative);
            if let Ok(path) = write.path.strip_prefix(&base) {
                generated.insert(base.join(path.components().next().unwrap().as_os_str()));
            }
        }
    }
    let provider_root = root.join("registry/interface").join(&plan.provider_interface_id);
    assert!(generated.remove(&provider_root));
    let mut removals = vec![provider_root];
    removals.extend(generated);
    let mut evidence = std::collections::BTreeMap::new();
    for directory in removals.iter().filter(|path| path.starts_with(root.join("registry/tool"))) {
        let metadata = std::fs::symlink_metadata(directory).unwrap();
        context["directory_inventory"]["removals"].as_array_mut().unwrap().push(json!({
            "path":directory,"identity":{"device":metadata.dev(),"inode":metadata.ino()}}));
        let file = directory.join("tool.json");
        let descriptor: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        for file in [file, root.join("registry/interface").join(descriptor["interface"].as_str().unwrap()).join("octopus_os.json")] {
            let content = std::fs::read_to_string(&file).unwrap();
            evidence.insert(file.clone(), json!({"file":file,"expected_digest":digest(content.as_bytes()),"content":content}));
        }
    }
    context["directory_inventory"]["originals"] = json!(evidence.into_values().collect::<Vec<_>>());
    for id in context["selection"]["interface_ids"].as_array().unwrap() {
        let id = id.as_str().unwrap();
        if !removals.contains(&root.join("registry/interface").join(id)) { continue; }
        for relative in ["interfaces", "interface-hand-in"] {
            let directory = root.join(relative).join(id);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("fixture-state.json"), b"{\"fixture\":true}").unwrap();
            removals.push(directory);
        }
    }
    support::write(root, "registry/artifact/retained-knowledge/artifact.json",
        json!({"id":"retained-knowledge","label":"context.collection","schema_version":1,"directory":"entries"}));
    support::write(root, "registry/artifact/retained-knowledge/entries/published.json", json!({"text":"Retained published experience"}));
    support::write(root, "registry/artifact/retained-catalogue/artifact.json",
        json!({"id":"retained-catalogue","label":"context.reference","text":"Retained catalogue snapshot"}));
    let snapshot = root.join("ledger/blobs").join(octopus_ledger::ContentHash::of_bytes(plan.run_write.content.as_bytes()).to_hex());
    std::fs::write(&snapshot, &plan.run_write.content).unwrap();
    std::fs::write(root.join("ledger/blobs/retained-payload"), b"retained artifact payload").unwrap();
    let mut preserved: Vec<_> = ["ledger/movements.jsonl", "ledger/blobs/retained-payload",
        "registry/artifact/retained-knowledge/artifact.json", "registry/artifact/retained-knowledge/entries/published.json",
        "registry/artifact/retained-catalogue/artifact.json", "registry/interface/system_worker_template/interface.json",
        "registry/interface/world_operator_chat/interface.json", "registry/tool/file.write/tool.json", "state/ee_instances/ee_existence.json"]
        .into_iter().map(|relative| root.join(relative)).collect();
    preserved.push(snapshot);
    RetirementFixture { plan, context, removals, preserved }
}

#[test]
fn real_retirement_holds_and_cold_proof_preserve_the_exact_ledger_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let RetirementFixture { plan, mut context, .. } = fixture(&root);
    serde_json::from_value::<TaskRetirementContext>(context.clone()).unwrap().verify_before().unwrap();
    std::fs::write(&plan.run_write.path, b"{}").unwrap();
    assert!(serde_json::from_value::<TaskRetirementContext>(context.clone()).unwrap().verify_before().is_err());
    std::fs::write(&plan.run_write.path, &plan.run_write.content).unwrap();
    for write in plan.run_writes.iter().chain([&plan.run_write]) { std::fs::remove_file(&write.path).unwrap(); }
    let (mut child, mut reader, proof) = start(&context);
    let Some(proof) = proof else { panic!("{}", String::from_utf8_lossy(&child.wait_with_output().unwrap().stderr)); };
    let interface = context["selection"]["interface_ids"][0].as_str().unwrap().into();
    assert!(InterfaceLifeOwnership::try_acquire(&root.join("interfaces"), &interface).unwrap().is_none());
    let source = root.join("registry/interface").join(&plan.provider_interface_id);
    let transaction = root.join("client-state/workspace/transactions/retirement-fixture");
    std::fs::create_dir_all(transaction.join("removed")).unwrap();
    let tombstone = transaction.join("removed/0000");
    let snapshot = command(&mut child, &mut reader, json!({"operation":"snapshot_directory","transaction_id":"retirement-fixture","path":source}));
    assert_eq!(snapshot["proof"], proof);
    assert_eq!(snapshot["preimage"]["digest_algorithm"], "sha256");
    let tool = PathBuf::from(context["directory_inventory"]["removals"][0]["path"].as_str().unwrap());
    let tool_snapshot = command(&mut child, &mut reader, json!({"operation":"snapshot_directory","transaction_id":"retirement-fixture","path":tool}));
    let moves = json!([{"source":source,"destination":tombstone,"expected":snapshot["preimage"]},
        {"source":tool,"destination":transaction.join("removed/0001"),"expected":tool_snapshot["preimage"]}]);
    let publication = command(&mut child, &mut reader, json!({"operation":"publish_directory_moves","transaction_id":"retirement-fixture",
        "operation_id":"retirement-fixture:directory","moves":moves}));
    assert_eq!(publication["result"]["error"], Value::Null);
    assert_eq!(publication["result"]["receipts"][1]["outcome"], "applied");
    assert!(!tool.exists());
    assert!(!source.exists() && tombstone.exists());
    assert!(InterfaceLifeOwnership::try_acquire(&root.join("interfaces"), &interface).unwrap().is_none());
    let blocked = command(&mut child, &mut reader, json!({"operation":"snapshot_directory","transaction_id":"retirement-fixture","path":transaction}));
    writeln!(child.stdin.as_mut().unwrap(), "{}", json!({"operation":"publish_directory_moves","transaction_id":"retirement-fixture",
        "operation_id":"retirement-fixture:premature-cleanup","moves":[{"source":transaction,"destination":transaction.with_file_name(".cleanup-retirement-fixture"),
            "expected":blocked["preimage"]}]})).unwrap();
    let mut refused = String::new(); assert_eq!(reader.read_line(&mut refused).unwrap(), 0);
    drop(child.stdin.take()); let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reclaimed before moving"));
    assert!(transaction.exists() && tombstone.exists());
    context["prior_proof"] = proof.clone();
    let mut changed = context.clone();
    changed["directory_inventory"]["removals"][0]["identity"]["inode"] = json!(0);
    let (child, _, refused) = start(&changed); assert!(refused.is_none());
    assert!(String::from_utf8_lossy(&child.wait_with_output().unwrap().stderr).contains("another exact transaction"));
    let (mut child, mut reader, reacquired) = start(&context); assert_eq!(reacquired.as_ref(), Some(&proof));
    let repeated = command(&mut child, &mut reader, json!({"operation":"publish_directory_moves","transaction_id":"retirement-fixture",
        "operation_id":"retirement-fixture:directory","moves":moves}));
    assert_eq!(repeated["result"], publication["result"]);
    for receipt in publication["result"]["receipts"].as_array().unwrap() {
        command(&mut child, &mut reader, json!({"operation":"reclaim_directory","transaction_id":"retirement-fixture",
            "tombstone":receipt["movement"]["destination"],"receipt":receipt,"expected":receipt["movement"]["expected"]}));
    }
    assert!(!tombstone.exists());
    let snapshot = command(&mut child, &mut reader, json!({"operation":"snapshot_directory","transaction_id":"retirement-fixture","path":transaction}));
    let cleanup = transaction.with_file_name(".cleanup-retirement-fixture");
    let moved_root = command(&mut child, &mut reader, json!({"operation":"publish_directory_moves","transaction_id":"retirement-fixture",
        "operation_id":"retirement-fixture:cleanup","moves":[{"source":transaction,"destination":cleanup,"expected":snapshot["preimage"]}]}));
    assert_eq!(moved_root["result"]["receipts"][0]["outcome"], "applied");
    command(&mut child, &mut reader, json!({"operation":"reclaim_directory","transaction_id":"retirement-fixture","tombstone":cleanup,
        "receipt":moved_root["result"]["receipts"][0],"expected":snapshot["preimage"]}));
    assert!(!transaction.exists() && !cleanup.exists());
    writeln!(child.stdin.as_mut().unwrap(), "{{\"operation\":\"assert_held\"}}").unwrap();
    let mut line = String::new(); reader.read_line(&mut line).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), proof);
    close(child);
    context["prior_proof"] = proof;
    let unrelated = json!({"seq":0,"lamport":1,"recorded_at_unix_ms":1,"trace":"unrelated",
        "kind":MovementKind::InterfaceArrival,"from":Endpoint::Interface("other_source".into()),"to":Endpoint::Interface("other_target".into()),"artifact":null});
    std::fs::write(root.join("ledger/movements.jsonl"), format!("{unrelated}\n")).unwrap();
    let (child, _, proof) = start(&context);
    let Some(proof) = proof else { panic!("{}", String::from_utf8_lossy(&child.wait_with_output().unwrap().stderr)); };
    assert_eq!(proof["ledger_boundary"]["sequence"], 1);
    close(child);
    context["prior_proof"] = proof;
    let mut related = unrelated.clone();
    related["seq"] = json!(1); related["from"] = json!(Endpoint::Interface(plan.provider_interface_id.clone().into()));
    std::fs::write(root.join("ledger/movements.jsonl"), format!("{unrelated}\n{related}\n")).unwrap();
    let (child, _, proof) = start(&context);
    assert!(proof.is_none());
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("new movement evidence"));
    std::fs::write(root.join("ledger/movements.jsonl"), format!("{}\n", unrelated.to_string().replace("unrelated", "corrupted"))).unwrap();
    let (child, _, proof) = start(&context);
    assert!(proof.is_none());
    assert!(String::from_utf8_lossy(&child.wait_with_output().unwrap().stderr).contains("prefix changed"));
}

#[cfg(unix)]
#[test]
fn native_retirement_refuses_unselected_aliases_and_changed_commands() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let RetirementFixture { plan, mut context, .. } = fixture(&root);
    for write in plan.run_writes.iter().chain([&plan.run_write]) { std::fs::remove_file(&write.path).unwrap(); }
    let (child, _, proof) = start(&context);
    context["prior_proof"] = proof.unwrap(); close(child);
    let private = root.join("interfaces").join(&plan.provider_interface_id);
    let outside = root.join("registry/artifact/retained-knowledge");
    let transaction = root.join("client-state/workspace/transactions/retirement-fixture");
    std::fs::create_dir_all(&transaction).unwrap();
    std::os::unix::fs::symlink(&outside, transaction.join("removed")).unwrap();
    for value in [
        json!({"operation":"snapshot_directory","transaction_id":"retirement-fixture","path":outside}),
        json!({"operation":"snapshot_directory","transaction_id":"different-operation","path":private}),
        json!({"operation":"snapshot_directory","transaction_id":"retirement-fixture","path":transaction.join("removed/0000")}),
        json!({"operation":"publish_directory_moves","transaction_id":"retirement-fixture","operation_id":"other:step","moves":[]}),
        json!({"operation":"assert_held","extra":true}),
    ] {
        let (mut child, mut reader, proof) = start(&context); assert!(proof.is_some());
        writeln!(child.stdin.as_mut().unwrap(), "{value}").unwrap();
        let mut response = String::new(); assert_eq!(reader.read_line(&mut response).unwrap(), 0);
        drop(child.stdin.take()); let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(outside.join("artifact.json").exists() && private.exists());
    }
    let (mut child, mut reader, proof) = start(&context); assert!(proof.is_some());
    let snapshot = command(&mut child, &mut reader, json!({"operation":"snapshot_directory","transaction_id":"retirement-fixture","path":private}));
    child.kill().unwrap(); let mut input = child.stdin.take().unwrap(); child.wait().unwrap();
    let tombstone = transaction.join("removed/0000");
    assert!(writeln!(&mut input, "{}", json!({"operation":"publish_directory_moves","transaction_id":"retirement-fixture",
        "operation_id":"retirement-fixture:after-death","moves":[{"source":private,"destination":tombstone,"expected":snapshot["preimage"]}]})).is_err());
    assert!(private.exists() && outside.join("artifact.json").exists());
}

fn cjs_fixture_command(root: &Path, fixture: &RetirementFixture, mode: &str) -> std::process::Output {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..").canonicalize().unwrap();
    let mut child = Command::new(std::env::var_os("OCTOPUS_NODE_COMMAND").unwrap_or_else(|| "node".into()))
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/task_retirement.cjs"))
        .current_dir(&repository).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    serde_json::to_writer(child.stdin.take().unwrap(), &json!({"mode":mode,"home":root,"repository":repository,
        "binary":env!("CARGO_BIN_EXE_octopus-operator-chat-topology"),"selection":fixture.context["selection"],
        "removals":fixture.removals,"preserved":fixture.preserved,
        "provider_directory":root.join("registry/interface").join(&fixture.plan.provider_interface_id)})).unwrap();
    child.wait_with_output().unwrap()
}

fn cjs_result(output: std::process::Output) -> Value {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn native_retirement_barrier_commits_the_real_cjs_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fixture = fixture(&root);
    let result = cjs_result(cjs_fixture_command(&root, &fixture, "commit"));
    assert_eq!(result["preflights"], 1);
    assert_eq!(result["acquisitions"], 1);
    assert_eq!(result["removed_records"], fixture.context["selection"]["records"].as_array().unwrap().len());
    assert!(result["held_verifications"].as_u64().unwrap() > 1);
    assert_eq!(result["remaining_transactions"], 0);
    assert!(InterfaceLifeOwnership::try_acquire(&root.join("interfaces"),
        &fixture.context["selection"]["interface_ids"][0].as_str().unwrap().into()).unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn native_retirement_barrier_recovers_a_killed_cjs_caller_after_published_cleanup() {
    use std::os::unix::process::ExitStatusExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fixture = fixture(&root);
    let preserved = fixture.preserved.iter().map(|path| (path, std::fs::read(path).unwrap())).collect::<Vec<_>>();
    let output = cjs_fixture_command(&root, &fixture, "interrupt");
    assert_eq!(output.status.signal(), Some(libc::SIGKILL), "{}", String::from_utf8_lossy(&output.stderr));
    let interrupted: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(interrupted["phase"], "published");
    assert_eq!(interrupted["provider_tombstoned"], true);
    assert_eq!(interrupted["has_held_proof"], true);
    assert!(!root.join("registry/interface").join(&fixture.plan.provider_interface_id).exists());
    let result = cjs_result(cjs_fixture_command(&root, &fixture, "recover"));
    assert_eq!(result["preflights"], 0);
    assert_eq!(result["acquisitions"], 1);
    assert_eq!(result["used_prior_proof"], true);
    assert_eq!(result["remaining_transactions"], 0);
    for (path, bytes) in preserved { assert_eq!(std::fs::read(path).unwrap(), bytes); }
}
