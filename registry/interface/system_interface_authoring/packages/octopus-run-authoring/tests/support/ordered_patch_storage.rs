use super::*;
use octopus_interfaces::GrantProjection;

#[test]
fn ordered_entry_preserves_exact_snapshots_with_a_checked_postimage_patch() {
    let before = bytes(&json!({"generation":0,"context":"unchanged ".repeat(4096)})).unwrap();
    let after = bytes(&json!({"generation":1,"context":"unchanged ".repeat(4096)})).unwrap();
    let entry = OrderedEntry::prepare(
        PathBuf::from("/run.json"),
        &before,
        &after,
        Some(JsonFormat::Compact),
    )
    .unwrap();
    let value = serde_json::to_value(&entry).unwrap();
    assert!(value.get("content").is_none());
    assert_eq!(entry.before(1_000_000).unwrap().as_bytes(), before);
    assert_eq!(entry.content(1_000_000).unwrap().as_bytes(), after);
    let decoded: OrderedEntry = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        decoded.content(1_000_000).unwrap(),
        entry.content(1_000_000).unwrap()
    );
    let mut plaintext = value.clone();
    plaintext.as_object_mut().unwrap().remove("json_format");
    plaintext["expected_before_content"] = json!(std::str::from_utf8(&before).unwrap());
    let pretty_after = JsonFormat::Pretty
        .encode(&serde_json::from_slice(&after).unwrap())
        .unwrap();
    plaintext["expected_after_hash"] = json!(ContentHash::of_bytes(&pretty_after).to_hex());
    let old: OrderedEntry = serde_json::from_value(plaintext.clone()).unwrap();
    assert_eq!(serde_json::to_value(&old).unwrap(), plaintext);
    assert_eq!(old.content(1_000_000).unwrap().as_bytes(), pretty_after);
    for bad in [Value::Null, json!("automatic")] {
        let mut invalid = plaintext.clone();
        invalid["json_format"] = bad;
        assert!(serde_json::from_value::<OrderedEntry>(invalid).is_err());
    }
    let full = json!({"path":entry.path(),"expected_before_content":std::str::from_utf8(&before).unwrap(),
        "expected_before_hash":ContentHash::of_bytes(&before).to_hex(),"content":String::from_utf8(after).unwrap()});
    assert!(
        serde_json::to_vec(&value).unwrap().len() * 100
            < serde_json::to_vec(&full).unwrap().len() * 55
    );
    let content: OrderedEntry = serde_json::from_value(full.clone()).unwrap();
    assert_eq!(serde_json::to_value(&content).unwrap(), full);
    assert_eq!(content.content(1_000_000).unwrap(), full["content"].as_str().unwrap());
    for (field, bad) in [
        (
            "expected_before_hash",
            json!(ContentHash::of_bytes(b"wrong").to_hex()),
        ),
        (
            "expected_after_hash",
            json!(ContentHash::of_bytes(b"wrong").to_hex()),
        ),
        ("patch", json!([])),
        (
            "patch",
            json!([{"op":"replace","path":"/generation","value":2}]),
        ),
    ] {
        let mut corrupted = value.clone();
        corrupted[field] = bad;
        let corrupted: OrderedEntry = serde_json::from_value(corrupted).unwrap();
        assert!(corrupted.content(1_000_000).is_err(), "{field}");
    }
}

#[test]
fn native_run_edits_export_exact_storage_sequence() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let target = root.join("run_storage.json");
    let effects = root.join("effects");
    let directory = std::env::var_os("OCTOPUS_STORAGE_SEQUENCE_DIR").map(PathBuf::from);
    if let Some(root) = &directory {
        std::fs::create_dir(root).unwrap();
    }
    let mut files = Vec::new();
    let mut export = |turn: usize, kind: &str, bytes: &[u8]| {
        let name = format!("{turn:04}-{kind}.bin");
        if let Some(root) = &directory {
            std::fs::write(root.join(&name), bytes).unwrap();
        }
        files.push(json!({"turn":turn,"kind":kind,"path":name,"bytes":bytes.len(),"hash":ContentHash::of_bytes(bytes).to_hex()}));
    };
    let mut value = json!({"id":"run_storage","ee":"ee_storage","worker":"system_worker_coder",
        "trace":"trace:storage","status":"idle","checkpoints":[],"active_checkpoints":[],
        "interface_state":{"provider":{"input_artifact_bindings":[],"context_generation":0}}});
    let mut before = JsonFormat::Pretty.encode(&value).unwrap();
    std::fs::write(&target, &before).unwrap();
    let mut compact_runs = 0usize;
    let mut pretty_runs = 0usize;
    let mut calls = 0usize;
    let mut plain_calls = 0usize;
    let mut intents = 0usize;
    let mut stages = 0usize;
    let mut milestones = Vec::new();
    let descriptor = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../../registry/tool/file.write/tool.json"),
    )
    .unwrap();
    for turn in 1..=128 {
        let payload = serde_json::to_vec(&json!({"text":format!("Requested revision {turn}: preserve exact input rights and checkpoint evidence."),"turn":turn})).unwrap();
        let hash = ContentHash::of_bytes(&payload).to_hex();
        let bindings = value["interface_state"]["provider"]["input_artifact_bindings"]
            .as_array_mut()
            .unwrap();
        bindings.push(json!({"id":format!("operator.message:{hash}"),"access":{"read":true,"change":false,"create":false,"delete":false}}));
        if turn % 7 == 0 {
            bindings.truncate(bindings.len().saturating_sub(2));
        }
        let id = format!("checkpoint_{turn}_{}", "front".repeat(turn % 5 + 1));
        value["checkpoints"] = if turn % 3 == 0 {
            json!([])
        } else {
            json!([{"id":id,"run":"run_storage","trace":format!("trace:{id}"),"epoch":turn,
                "interface_binding":AuthoredInterfaceBinding::all("provider".into()),"outbound_interface_bindings":[],
                "artifact_bindings":value["interface_state"]["provider"]["input_artifact_bindings"],"required_verified_artifacts":[]}])
        };
        value["active_checkpoints"] = if turn % 3 == 0 {
            json!([])
        } else {
            json!([id])
        };
        value["interface_state"]["provider"]["context_generation"] = json!(turn);
        let edit = CheckedRunEdit::prepare(
            target.clone(),
            &before,
            value.clone(),
            format!("edit:{turn}"),
        )
        .unwrap();
        let entry = OrderedEntry::prepare(
            edit.path.clone(),
            &before,
            edit.content.as_bytes(),
            edit.json_format,
        )
        .unwrap();
        assert_eq!(entry.before(8 << 20).unwrap().as_bytes(), before);
        assert_eq!(entry.content(8 << 20).unwrap(), edit.content);
        let call = serde_json::to_vec(&json!({"tool":"file.write","arguments":{"ordered":true,"writes":[entry]},"correlation_id":edit.correlation_id})).unwrap();
        let call_file =
            InterfaceArtifactFile::write(root.join("calls"), "tool.call", &call).unwrap();
        let identity = artifact_identity(&call_file, &root).unwrap().artifact;
        let envelope = InterfaceInvocationEnvelope {
            artifact: identity.clone(),
            artifact_file: call_file.clone(),
            artifact_binding_payloads: vec![],
            metadata: Some(InvocationMetadata::new(GrantProjection {
                worker_environment_kinds: vec!["system_room".into()],
                ..Default::default()
            })),
        };
        let arguments = serde_json::from_slice::<Value>(&call).unwrap()["arguments"].clone();
        let receipt = octopus_system_room_env::execute_file_write_result(
            &envelope,
            &arguments,
            &["system_room".into()],
            &BTreeMap::from([("run_records_root".into(), root.clone())]),
            &effects,
            8 << 20,
        )
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), edit.content.as_bytes());
        assert_eq!(receipt["writes"][0]["after_hash"], edit.after_hash);
        let effect = effects.join("ordered").join(identity.content.to_hex());
        let intent = std::fs::read(effect.join("intent.json")).unwrap();
        assert!(!effect.join("0.bytes").exists());
        let retained = std::fs::read_dir(&effect).unwrap().map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "bytes"))
            .map(|path| std::fs::symlink_metadata(path).unwrap().len() as usize).sum::<usize>();
        assert_eq!(retained, 0);
        intents += intent.len();
        stages += retained;
        let mut plain: Value = serde_json::from_slice(&call).unwrap();
        plain["arguments"]["writes"][0]["expected_before_content"] =
            json!(std::str::from_utf8(&before).unwrap());
        compact_runs += edit.content.len();
        pretty_runs += JsonFormat::Pretty.encode(&value).unwrap().len();
        calls += call.len();
        plain_calls += serde_json::to_vec(&plain).unwrap().len();
        export(turn, "run-before", &before);
        export(turn, "input-payload", &payload);
        let payload_path = directory
            .as_ref()
            .map_or_else(|| PathBuf::from("/temporary"), Clone::clone)
            .join(format!("{turn:04}-input-payload.bin"));
        let manifest = InterfaceArtifactManifest {
            label: "operator.message".into(),
            payload_path,
            payload_content_hash: Some(hash),
        };
        export(
            turn,
            "input-manifest",
            &serde_json::to_vec(&manifest).unwrap(),
        );
        export(turn, "descriptor", &descriptor);
        export(turn, "call", &call);
        export(
            turn,
            "call-manifest",
            &std::fs::read(call_file.manifest_path()).unwrap(),
        );
        export(turn, "run-after", edit.content.as_bytes());
        export(turn, "intent", &intent);
        before = edit.content.into_bytes();
        if [8, 32, 64, 128].contains(&turn) {
            milestones.push(json!({"turns":turn,"compact_run_bytes":compact_runs,"pretty_run_bytes":pretty_runs,
                "encoded_call_bytes":calls,"plaintext_preimage_call_bytes":plain_calls,"intent_bytes":intents,"staged_postimage_bytes":stages}));
        }
    }
    assert!(calls < plain_calls && compact_runs < pretty_runs);
    if let Some(root) = directory {
        std::fs::write(
            root.join("sequence.json"),
            serde_json::to_vec(&json!({"files":files,"milestones":milestones})).unwrap(),
        )
        .unwrap();
        eprintln!("native storage sequence: {}", root.display());
    }
    eprintln!("native storage terms: {}", json!(milestones));
}
