use super::*;
use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_provider_codex::intake_state::{IntakeEdit, reprepare_input};
use octopus_provider_run_authoring::{OwnerWriterEvidence, separate_owner_writer_run};
use octopus_tool_handlers::ToolResultArtifact;

fn begin(f: &mut Fixture, edit: &IntakeEdit) -> (Value, InterfaceInvocationEnvelope) {
    let file = edit
        .materialize(&f.ctx.var_dir.join("ordered-input"), &f.config)
        .unwrap();
    let captured = file.read_bounded(1 << 20, 1 << 24).unwrap();
    let call: Value = serde_json::from_slice(&captured.payload_bytes).unwrap();
    let writer = f.config.writer.clone();
    let entry = &call["arguments"]["writes"][0];
    let preimage: octopus_provider_run_authoring::Preimage = serde_json::from_value(entry["expected_before_content"].clone()).unwrap();
    let snapshot_text = preimage.decode(entry["expected_before_hash"].as_str().unwrap(), &mut octopus_provider_run_authoring::DecodeBudget::new(f.ctx.host_io_limits.as_ref().unwrap().max_payload_bytes).unwrap()).unwrap();
    let snapshot = snapshot_text.as_bytes();
    let mut draft = f.movement_draft(&writer, &edit.edit, snapshot, None);
    draft.artifact = Some(f.artifact(&writer.call_label, call.clone()));
    let mut metadata = draft
        .invocation_metadata
        .take()
        .unwrap()
        .invocation()
        .unwrap()
        .clone();
    metadata.grant.worker_environment_kinds = vec!["system_room".into()];
    draft.invocation_metadata = Some(metadata.clone().into());
    let input = InterfaceInvocationEnvelope {
        artifact: draft.artifact.clone().unwrap(),
        artifact_file: file,
        artifact_binding_payloads: vec![],
        metadata: Some(metadata),
    };
    f.log.append(draft);
    (call, input)
}

fn finish(
    f: &mut Fixture,
    edit: &IntakeEdit,
    call: &Value,
    input: &InterfaceInvocationEnvelope,
) -> bool {
    let roots = std::collections::BTreeMap::from([(
        "run_records_root".into(),
        f.path.parent().unwrap().to_path_buf(),
    )]);
    let effect = octopus_system_room_env::execute_file_write_result(
        input,
        &call["arguments"],
        &["system_room".into()],
        &roots,
        &f.ctx.var_dir.join("effects"),
        f.ctx.host_io_limits.as_ref().unwrap().max_payload_bytes,
    );
    let ok = effect.is_ok();
    let result = match effect {
        Ok(body) => ToolResultArtifact::success(
            &f.config.writer.tool,
            Some(edit.edit.correlation_id.clone()),
            body,
        ),
        Err(error) => {
            assert_eq!(error.code, "file_write_incomplete");
            ToolResultArtifact::failure(
                &f.config.writer.tool,
                Some(edit.edit.correlation_id.clone()),
                error,
            )
        }
    };
    let snapshot = std::fs::read(call["arguments"]["writes"][0]["path"].as_str().unwrap()).unwrap();
    let writer = f.config.writer.clone();
    let mut draft = f.movement_draft(&writer, &edit.edit, &snapshot, Some(ok));
    draft.artifact = Some(f.artifact(&writer.result_label, json!(result)));
    f.log.append(draft);
    ok
}

#[test]
fn intake_recovers_an_admitted_conflict_with_its_own_writer_after_restart() {
    let mut f = Fixture::new();
    f.path = f.path.canonicalize().unwrap();
    f.ctx.root_slots.insert(
        "run_records_root".into(),
        f.path.parent().unwrap().to_path_buf(),
    );
    let mut value: Value = serde_json::from_slice(&f.before).unwrap();
    let initial =
        separate_owner_writer_run(&mut value, &f.config.writer, &"writer_intake".into()).unwrap();
    value["interface_state"][f.config.writer.owner.as_str()]["writer_run"].as_object_mut().unwrap().remove("initial_record_format");
    let encode = |v: &Value| octopus_provider_run_authoring::JsonFormat::Pretty.encode(v).unwrap();
    std::fs::write(
        f.path.parent().unwrap().join("writer_intake.json"),
        encode(&initial),
    )
    .unwrap();
    f.before = encode(&value);
    std::fs::write(&f.path, &f.before).unwrap();
    let mut first = prepare_input(&f.config, f.path.clone(), &f.before, 0, f.log.read_from(Seq(0))[0].artifact.as_ref().unwrap(), 0, 8 * 1024 * 1024).unwrap();
    first.edit = CheckedRunEdit::prepare_with_format(f.path.clone(), &f.before,
        serde_json::from_str(&first.edit.content).unwrap(), first.edit.correlation_id.clone(), None).unwrap();
    let mut retained = serde_json::to_value(&first).unwrap();
    for entry in retained["ordered"]["arguments"]["writes"].as_array_mut().unwrap() {
        let preimage: octopus_provider_run_authoring::Preimage = serde_json::from_value(entry["expected_before_content"].clone()).unwrap();
        let before = preimage.decode(entry["expected_before_hash"].as_str().unwrap(),
            &mut octopus_provider_run_authoring::DecodeBudget::new(8 * 1024 * 1024).unwrap()).unwrap();
        let mut after: Value = serde_json::from_str(&before).unwrap();
        let patch: json_patch::Patch = serde_json::from_value(entry["patch"].clone()).unwrap();
        json_patch::patch(&mut after, &patch).unwrap();
        *entry = json!({"path":entry["path"],"expected_before_hash":entry["expected_before_hash"],
            "expected_before_content":before,"content":String::from_utf8(encode(&after)).unwrap()});
    }
    first = serde_json::from_value(retained).unwrap();
    let (call, input) = begin(&mut f, &first);
    assert!(finish(&mut f, &first, &call, &input));
    assert_eq!(
        OwnerWriterEvidence::default()
            .result(&f.ctx, first.ordered.as_ref().unwrap())
            .unwrap(),
        Some(true)
    );
    f.before = first.edit.content.as_bytes().to_vec();
    adopt_writer_schema(&f, &call["arguments"]);
    let pending = retained_input(&f);
    let attempt = pending.edit.as_ref().unwrap();
    let (call, input) = begin(&mut f, attempt);
    let descriptor: Value = serde_json::from_slice(&std::fs::read(f._root.path().join("registry/tool")
        .join(&f.config.writer.tool).join("tool.json")).unwrap()).unwrap();
    octopus_tool_registry::ToolArgumentValidator::compile(&descriptor["args_schema"]).unwrap().validate(&call["arguments"]).unwrap();
    assert_eq!(call["arguments"]["writes"][0]["expected_before_content"]["encoding"], "zlib_base64");
    let before = f.before.clone();
    let peer = f.provider_edit(&before);
    let writer = f.provider.clone();
    f.movement(&writer, &peer, &before, None);
    f.effect(&peer);
    f.movement(&writer, &peer, peer.content.as_bytes(), Some(true));
    assert!(!finish(&mut f, attempt, &call, &input));
    assert_eq!(std::fs::read(&f.path).unwrap(), peer.content.as_bytes());
    let recovered = OwnerWriterEvidence::default();
    assert_eq!(
        recovered
            .result(&f.ctx, attempt.ordered.as_ref().unwrap())
            .unwrap(),
        Some(false)
    );
    let chain = f.advance(&before, peer.content.as_bytes(), 0).unwrap();
    let next = reprepare_input(&f.config, &pending, peer.content.as_bytes(), 0, &chain, 8 * 1024 * 1024).unwrap();
    let (call, input) = begin(&mut f, &next);
    assert!(finish(&mut f, &next, &call, &input));
    assert_eq!(
        OwnerWriterEvidence::default()
            .result(&f.ctx, next.ordered.as_ref().unwrap())
            .unwrap(),
        Some(true)
    );
    let final_run = RunRecord::from_bytes(next.edit.content.as_bytes()).unwrap();
    let peer_run = RunRecord::from_bytes(peer.content.as_bytes()).unwrap();
    assert_eq!(
        final_run.interface_state[&f.provider.owner],
        peer_run.interface_state[&f.provider.owner]
    );
}

fn adopt_writer_schema(f: &Fixture, old_call: &Value) {
    use octopus_operator_chat_client::{OperatorWriterSchemaPlanRequest, plan_operator_writer_schema};
    use sha2::{Digest, Sha256};
    let hash = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    let registry = f._root.path().join("registry").canonicalize().unwrap();
    let tool_path = registry.join("tool").join(&f.config.writer.tool).join("tool.json");
    let mut tool: Value = serde_json::from_slice(&std::fs::read(&tool_path).unwrap()).unwrap();
    let schema = tool["args_schema"].clone();
    let mut old = schema.clone();
    old["properties"].as_object_mut().unwrap().remove("json_format");
    old["properties"]["writes"]["items"]["properties"].as_object_mut().unwrap().remove("json_format");
    old["properties"]["writes"]["items"]["properties"]["expected_before_content"] = json!({"type":["string","null"]});
    old["allOf"][0]["then"]["properties"]["writes"]["items"]["then"]["properties"]["expected_before_content"] = json!({"type":"string"});
    octopus_tool_registry::ToolArgumentValidator::compile(&old).unwrap().validate(old_call).unwrap();
    tool["args_schema"] = old.clone();
    let old_bytes = serde_json::to_vec_pretty(&tool).unwrap();
    std::fs::write(&tool_path, &old_bytes).unwrap();
    let ledger = std::fs::read(&f.ctx.movements_path).unwrap();
    let run = std::fs::read(&f.path).unwrap();
    let writer = std::fs::read(f.path.parent().unwrap().join("writer_intake.json")).unwrap();
    let mut request = OperatorWriterSchemaPlanRequest { registry_root:registry.clone(), run_records_root:f.path.parent().unwrap().to_path_buf(),
        run_id:f.config.run.clone(), expected_run_sha256:hash(&run), writer:f.config.writer.clone(),
        expected_owner_sha256:hash(&std::fs::read(registry.join("interface").join(f.config.writer.owner.as_str()).join("interface.json")).unwrap()),
        expected_tool_sha256:hash(&old_bytes), expected_args_schema:old.clone(), args_schema:schema.clone() };
    request.expected_args_schema = json!({});
    let rejected = plan_operator_writer_schema(&request).unwrap_err();
    assert!(rejected.contains("approved previous schema"), "{rejected}");
    request.expected_args_schema = old;
    let expected = request.expected_run_sha256.clone(); request.expected_run_sha256 = "0".repeat(64);
    assert!(plan_operator_writer_schema(&request).unwrap_err().contains("changed before planning"));
    request.expected_run_sha256 = expected;
    let topology = json!({"id":"schema-adoption-topology","owner_interface":"nested-owner","routes":[{"tool":f.config.writer.tool}]});
    let reference = ArtifactRef { id:"schema-adoption-topology".into(), content:ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&topology)) };
    support::write(&registry, "artifact/schema-adoption-topology/artifact.json", topology);
    support::write(&registry, "interface/nested-owner/code_mode.json", json!({"handoff":{"topology_artifact":reference}}));
    assert!(plan_operator_writer_schema(&request).unwrap_err().contains("pinned in a nested CodeMode route"));
    std::fs::remove_file(registry.join("interface/nested-owner/code_mode.json")).unwrap();
    let writes = plan_operator_writer_schema(&request).unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, tool_path);
    assert_eq!(writes[0].expected_before_hash.as_deref(), Some(hash(&old_bytes).as_str()));
    assert_eq!(std::fs::read(&tool_path).unwrap(), old_bytes);
    let mut next: Value = serde_json::from_str(&writes[0].content).unwrap();
    assert_eq!(next.as_object_mut().unwrap().remove("args_schema"), Some(schema.clone()));
    tool.as_object_mut().unwrap().remove("args_schema"); assert_eq!(next, tool);
    assert_eq!(hash(&std::fs::read(&tool_path).unwrap()), request.expected_tool_sha256);
    std::fs::write(&tool_path, &writes[0].content).unwrap();
    octopus_tool_registry::ToolArgumentValidator::compile(&schema).unwrap().validate(old_call).unwrap();
    assert_eq!(std::fs::read(&f.ctx.movements_path).unwrap(), ledger);
    assert_eq!(std::fs::read(&f.path).unwrap(), run);
    assert_eq!(std::fs::read(f.path.parent().unwrap().join("writer_intake.json")).unwrap(), writer);
}
