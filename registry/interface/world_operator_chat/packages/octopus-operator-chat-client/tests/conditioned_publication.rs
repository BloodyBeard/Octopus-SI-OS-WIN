#![cfg(unix)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use octopus_interface_transport::runtime_instance::RuntimeInstanceLock;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn hash(text: &str) -> String { format!("{:x}", Sha256::digest(text.as_bytes())) }

fn submit(request: &Value, discard_verdict: bool) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_octopus-operator-chat-topology"))
        .arg("--publish-files").stdin(Stdio::piped())
        .stdout(if discard_verdict { Stdio::null() } else { Stdio::piped() }).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(&serde_json::to_vec(request).unwrap()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    if discard_verdict { Value::Null } else { serde_json::from_slice(&output.stdout).unwrap() }
}

fn write_request(path: &Path, operation: &str) -> Value {
    json!({"path":path,"operation_id":operation,"expected_before_hash":hash("before"),"content":"after"})
}

#[test]
fn conditioned_cli_excludes_startup_and_replays_original_receipts_after_verdict_loss() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    std::fs::create_dir(home.join("state")).unwrap();
    let dependency = home.join("ee.json");
    let targets = [home.join("one.json"), home.join("two.json")];
    std::fs::write(&dependency, "allowed").unwrap();
    for path in &targets { std::fs::write(path, "before").unwrap(); }
    let writes = vec![write_request(&targets[0], "adoption:0"), write_request(&targets[1], "adoption:1")];
    let request = json!({"writes":writes,"condition":{"octopus_home":home,"preimages":[
        {"path":dependency,"sha256":hash("allowed")},
        {"path":home.join("interfaces/new-door/ledger-cursor.tool-door.json"),"sha256":null},
        {"path":targets[0],"sha256":hash("before")},{"path":targets[1],"sha256":hash("before")}]}});
    let held = RuntimeInstanceLock::try_acquire(&home).unwrap();
    let busy = submit(&request, false);
    assert_eq!(busy["receipts"], json!([])); assert!(busy["error"].is_string());
    drop(held);
    std::fs::write(&dependency, "narrowed").unwrap();
    let changed = submit(&request, false);
    assert_eq!(changed["receipts"], json!([])); assert!(changed["error"].as_str().unwrap().contains("dependency changed"));
    for path in &targets { assert_eq!(std::fs::read_to_string(path).unwrap(), "before"); }
    std::fs::write(&dependency, "allowed").unwrap();
    submit(&request, true);
    let receipts = targets.iter().enumerate().map(|(index, path)| {
        serde_json::to_value(octopus_file_storage::publication::read_receipt(path, &format!("adoption:{index}")).unwrap().unwrap()).unwrap()
    }).collect::<Vec<_>>();
    assert!(receipts.iter().all(|receipt| receipt["outcome"] == "applied"));
    let later = json!({"writes":[{"path":targets[0],"operation_id":"later","expected_before_hash":hash("after"),"content":"later"}]});
    assert!(submit(&later, false)["error"].is_null());
    std::fs::write(&dependency, "changed after original publication").unwrap();
    let held = RuntimeInstanceLock::try_acquire(&home).unwrap();
    let replay = submit(&request, false);
    assert_eq!(replay["receipts"], json!(receipts)); assert!(replay["error"].is_null());
    assert_eq!(std::fs::read_to_string(&targets[0]).unwrap(), "later");
    let mut different = request.clone(); different["writes"][0]["content"] = json!("other submitted bytes");
    let refused = submit(&different, false);
    assert_eq!(refused["receipts"], json!([])); assert!(refused["error"].as_str().unwrap().contains("another submitted operation"));
    drop(held);
}

#[test]
fn conditioned_cli_preserves_historical_prefix_and_refuses_inode_aliases_without_effects() {
    let temp = tempfile::tempdir().unwrap(); let home = temp.path().canonicalize().unwrap();
    std::fs::create_dir(home.join("state")).unwrap();
    let (first, tail, dependency) = (home.join("first"), home.join("tail"), home.join("dependency"));
    for path in [&first, &tail] { std::fs::write(path, "before").unwrap(); }
    std::fs::write(&dependency, "new authority").unwrap();
    let original = write_request(&first, "prefix");
    let receipt = submit(&json!({"writes":[original]}), false)["receipts"][0].clone();
    let batch = json!({"writes":[original,write_request(&tail,"tail")],"condition":{"octopus_home":home,
        "preimages":[{"path":dependency,"sha256":hash("old authority")}]}});
    let refused = submit(&batch, false);
    assert_eq!(refused["receipts"], json!([receipt])); assert!(refused["error"].is_string());
    assert_eq!(std::fs::read_to_string(&tail).unwrap(), "before");
    let alias = home.join("alias"); std::fs::hard_link(&tail, &alias).unwrap();
    let aliased = json!({"writes":[write_request(&tail,"alias-refusal")],"condition":{"octopus_home":home,
        "preimages":[{"path":alias,"sha256":hash("before")}]}});
    let start = std::time::Instant::now(); let refused = submit(&aliased, false);
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(refused["receipts"], json!([])); assert!(refused["error"].as_str().unwrap().contains("aliases a write target"));
    assert_eq!(std::fs::read_to_string(&tail).unwrap(), "before");
    let mut refused_then_tail = batch.clone();
    refused_then_tail["writes"][0]["operation_id"] = json!("cas-refused");
    refused_then_tail["condition"]["preimages"] = json!([]);
    let refused = submit(&refused_then_tail, false);
    assert_eq!(refused["receipts"].as_array().unwrap().len(), 1);
    assert_eq!(refused["receipts"][0]["outcome"], "not_applied");
    assert_eq!(std::fs::read_to_string(&tail).unwrap(), "before");
}
