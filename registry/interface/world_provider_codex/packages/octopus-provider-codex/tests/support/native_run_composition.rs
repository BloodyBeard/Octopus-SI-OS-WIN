use super::*;

#[path = "../../../../../world_operator_chat/packages/octopus-operator-chat-client/tests/support/composition_fixture.rs"]
mod support;

#[test]
fn creation_uses_admitted_coordinates_after_the_authoring_frontier_advances() {
    let root = tempfile::tempdir().unwrap();
    let request = support::fixture(root.path());
    let admitted = request.admitted_parent_record;
    let mut current = admitted.clone();
    current.checkpoints.clear();
    current.active_checkpoints.clear();
    current.interface_state.insert("world_provider_fixture".into(), BTreeMap::from([("context_generation".into(), json!(7))]));
    let current_before = current.clone();
    let catalog_ref = ArtifactRef { id: "catalog".into(), content: ContentHash::of_bytes(b"catalog") };
    let mut metadata = json!(request.metadata);
    metadata["artifact_binding_refs"] = json!([catalog_ref]);
    metadata["grant"] = json!({
        "artifact_access":{"catalog":{"read":true}},"artifact_verbs":["create"],
        "tools":["file.write","artifact.send"],
        "interface_affordance_specs":[
            {"name":"file.write","description":"Write files","args_schema":{"type":"object"}},
            {"name":"artifact.send","description":"Send artifact","args_schema":{"type":"object"}}
        ]
    });
    let metadata: InvocationMetadata = serde_json::from_value(metadata).unwrap();
    let mut affordances = RunAffordances {
        config: RunAffordanceConfig {
            catalog_artifact:catalog_ref, file_write_tool:Some("file.write".into()),
            artifact_send_tool:Some("artifact.send".into()), roots:Some(request.roots.clone()),
            defaults:Some(RunDefaults {
                client_template_interface_id:None,
                output_selection:None, ee_id:request.ee_id, worker_template_interface_id:request.worker_template_interface_id,
                provider_interface_id:request.provider_interface_id, model:request.model, reasoning_effort:request.reasoning_effort,
                service_tier:None,
                authoring:request.authoring, worker_environment_kinds:request.selected_worker_environment_kinds,
                tools:request.selected_tools, door_host_slots:request.door_host_slots, context_artifacts:vec![],
                return_interface_ids:vec![], return_to_source:true,
            }),
        },
        catalog: RunCatalog { id:"catalog".into(), label:"provider.run.affordances".into(), text:None,
            operations:vec![RunCatalogEntry { name:"create_work".into(), description:"Create work".into(),
                input_schema:json!({"type":"object"}), operation:RunOperation::CreateRun }] },
        enabled:BTreeSet::from(["create_work".into()]),
    };
    let args = json!({"text":"Independent verification"});
    let step = affordances.plan_call(&metadata, &admitted, &current, "call-1", "create_work", &args).unwrap();
    let NativeStep::Effect { effect, pending, .. } = step else { panic!("expected file authoring"); };
    assert_eq!(effect.tool, "file.write");
    assert!(effect.arguments["writes"].as_array().unwrap().iter().all(|write|
        write["path"] != json!(request.roots.run_records_root.join("caller.json"))));
    let NativePhase::Writing { outgoing_bindings, seed, .. } = pending.state else { panic!("expected checked writes before seed"); };
    assert_eq!(seed.tool, "artifact.send");
    assert_eq!(outgoing_bindings.len(), 1);
    assert_eq!(current, current_before);
    assert!(!request.roots.run_records_root.exists());
    assert!(affordances.plan_call(&metadata, &current, &current, "call-2", "create_work", &args).unwrap_err().contains("admitted parent record disagree"));
    let mut stale = metadata.clone();
    stale.runtime.as_mut().unwrap().epoch += 1;
    assert!(affordances.plan_call(&stale, &admitted, &current, "call-3", "create_work", &args).unwrap_err().contains("epoch is stale"));
    affordances.config.defaults.as_mut().unwrap().context_artifacts = vec![ArtifactRef { id: "unbound-creation-document".into(), content: ContentHash::of_bytes(b"document") }];
    let input = InterfaceInvocationEnvelope {
        artifact: affordances.config.catalog_artifact.clone(),
        artifact_file: octopus_interfaces::InterfaceArtifactFile::write(root.path(), "catalog-load", b"{}").unwrap(),
        artifact_binding_payloads: vec![octopus_interfaces::ArtifactBindingPayload::new(
            affordances.config.catalog_artifact.clone(), serde_json::to_vec(&affordances.catalog).unwrap())],
        metadata: Some(metadata.clone()),
    };
    let loaded = RunAffordances::from_invocation(&input, &json!({"run_affordances": affordances.config})).unwrap().unwrap();
    assert!(loaded.contains("create_work"));
    assert!(loaded.plan_call(&metadata, &admitted, &current, "call-unbound", "create_work", &args).unwrap_err().contains("not an exact readable metadata binding"));

}
