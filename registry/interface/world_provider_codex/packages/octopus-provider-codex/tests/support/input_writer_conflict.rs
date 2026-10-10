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
    let snapshot = call["arguments"]["writes"][0]["expected_before_content"]
        .as_str()
        .unwrap()
        .as_bytes();
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
    let encode = |v: &Value| {
        let mut bytes = serde_json::to_vec_pretty(v).unwrap();
        bytes.push(b'\n');
        bytes
    };
    std::fs::write(
        f.path.parent().unwrap().join("writer_intake.json"),
        encode(&initial),
    )
    .unwrap();
    f.before = encode(&value);
    std::fs::write(&f.path, &f.before).unwrap();
    let first = prepare_input(&f.config, f.path.clone(), &f.before, 0, f.log.read_from(Seq(0))[0].artifact.as_ref().unwrap(), 0).unwrap();
    let (call, input) = begin(&mut f, &first);
    assert!(finish(&mut f, &first, &call, &input));
    assert_eq!(
        OwnerWriterEvidence::default()
            .result(&f.ctx, first.ordered.as_ref().unwrap())
            .unwrap(),
        Some(true)
    );
    f.before = first.edit.content.as_bytes().to_vec();
    let pending = retained_input(&f);
    let attempt = pending.edit.as_ref().unwrap();
    let (call, input) = begin(&mut f, attempt);
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
    let next = reprepare_input(&f.config, &pending, peer.content.as_bytes(), 0, &chain).unwrap();
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
