use octopus_operator_chat_client::run_composition::*;
use octopus_runtime::run_record::RunRecord;
use serde_json::{Value, json};

#[path = "support/composition_fixture.rs"]
mod support;
use support::{fixture, write};

fn enable_native_profile(root: &std::path::Path) {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry/interface/world_provider_codex/interface.json");
    let declared: Value = serde_json::from_slice(&std::fs::read(source).unwrap()).unwrap();
    let path = "registry/interface/world_provider_fixture/interface.json";
    let mut provider: Value = serde_json::from_slice(&std::fs::read(root.join(path)).unwrap()).unwrap();
    provider["interface_driver_config"]["private_config"]["run_affordance_profile"] = declared["interface_driver_config"]["private_config"]["run_affordance_profile"].clone();
    write(root, path, provider);
}

fn adoption_request(root: &std::path::Path, request: &RunCompositionRequest, plan: &RunCompositionPlan) -> Value {
    use sha2::Digest;
    let provider = request.roots.registry_root.join("interface").join(&plan.provider_interface_id).join("interface.json");
    json!({"roots":request.roots,"ledger_root":root.join("ledger"),"client_template_interface":"world_operator_chat",
        "run_id":plan.run_id,"expected_run_hash":format!("{:x}",sha2::Sha256::digest(std::fs::read(&plan.run_write.path).unwrap())),
        "expected_provider_hash":format!("{:x}",sha2::Sha256::digest(std::fs::read(provider).unwrap()))})
}

#[test]
fn current_selection_materializes_defaults_and_preserves_provider_life_while_attaching() {
    use octopus_operator_chat_client::{SelectedToolSeed,run_composition::current_run::*};
    use octopus_provider_run_authoring::capability_amendment::ProviderCapabilityBridge;
    use octopus_ledger::ContentHash;
    let temp=tempfile::tempdir().unwrap();let root=temp.path().canonicalize().unwrap();
    let mut request=support::client_fixture(&root);
    let amend_json=|relative:&str,change:&dyn Fn(&mut Value)| {
        let mut value:Value=serde_json::from_slice(&std::fs::read(root.join(relative)).unwrap()).unwrap();change(&mut value);write(&root,relative,value);
    };
    for (path,pointer) in [("registry/interface/system_worker_template/policy.json","/ceiling/tools"),
        ("registry/ee_template/existence/ee_template.json","/allowed_tools"),("ee/ee_existence.json","/policy_narrowing/allowed_tools")] {
        amend_json(path,&|value|value.pointer_mut(pointer).unwrap().as_array_mut().unwrap().push(json!("file.read")));
    }
    amend_json("registry/interface/world_operator_chat/client.json",&|value| {
        value.as_object_mut().unwrap().remove("home_voice");value["run"]["context_artifact_bindings"]=json!([]);
    });
    let source=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry/interface/world_provider_codex/interface.json");
    let source:Value=serde_json::from_slice(&std::fs::read(source).unwrap()).unwrap();
    amend_json("registry/interface/world_provider_fixture/interface.json",&|value|value["interface_driver_config"]["private_config"]["run_affordance_profile"]=source["interface_driver_config"]["private_config"]["run_affordance_profile"].clone());
    request.selected_tools.clear();request.selected_worker_environment_kinds=vec!["system_room".into()];
    let composition=plan_run_composition(&request).unwrap();
    for write in composition_candidate_writes(&composition) {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();std::fs::write(write.path,write.content).unwrap();
    }
    let current=RunRecord::from_bytes(composition.run_write.content.as_bytes()).unwrap();
    let provider=composition.provider_interface_id.clone().into();
    let mut selection=current_selection(&request.roots,&current,&provider).unwrap().unwrap();
    assert_eq!(selection.selected_tools.len(),3);assert!(selection.selected_tools.iter().any(|tool|tool.tool=="file.read"));
    let source_policy=std::fs::read(root.join("registry/interface/system_worker_template/policy.json")).unwrap();
    for file in ["interface.json","life.json"] {
        let mut value:Value=serde_json::from_slice(&std::fs::read(root.join(format!("registry/interface/system_room/{file}"))).unwrap()).unwrap();
        if file=="interface.json" {value["id"]=json!("system_extra_room");}
        write(&root,&format!("registry/interface/system_extra_room/{file}"),value);
    }
    let mut tool:Value=serde_json::from_slice(&std::fs::read(root.join("registry/tool/file.read/tool.json")).unwrap()).unwrap();tool["id"]=json!("file.peek");tool["interface"]=json!("system_extra_room");
    write(&root,"registry/tool/file.peek/tool.json",tool);
    let worker_policy=format!("registry/interface/{}/policy.json",current.worker);
    for (path,pointer) in [(worker_policy.as_str(),"/ceiling/tools"),
        ("registry/ee_template/existence/ee_template.json","/allowed_tools"),("ee/ee_existence.json","/policy_narrowing/allowed_tools")] {
        amend_json(path,&|value|value.pointer_mut(pointer).unwrap().as_array_mut().unwrap().push(json!("file.peek")));
    }
    for (path,pointer) in [(worker_policy.as_str(),"/ceiling/interfaces"),
        ("registry/ee_template/existence/ee_template.json","/allowed_interfaces"),("ee/ee_existence.json","/policy_narrowing/allowed_interfaces")] {
        amend_json(path,&|value|value.pointer_mut(pointer).unwrap().as_array_mut().unwrap().push(json!("system_extra_room")));
    }
    assert_eq!(current_selection(&request.roots,&current,&provider).unwrap().unwrap().selected_tools,selection.selected_tools);
    selection.selected_tools.push(SelectedToolSeed {tool:"file.peek".into(),interface:"system_extra_room".into(),worker_environment_kind:"system_room".into()});
    let writer=composition.authoring.capability_tools["file.write"].clone();let reader=composition.authoring.capability_tools["file.read"].clone();
    let door:octopus_descriptors::InterfaceId=composition.authoring.capability_doors["system_room"].clone().into();
    let bridge=ProviderCapabilityBridge {tool:reader.clone(),correlation_id:"readback".into(),arguments_hash:ContentHash::of_bytes(b"{}").to_hex(),return_interface:door.clone()};
    let retained=vec![(writer,door.clone()),(reader,door)];
    let allowed=std::fs::read(root.join(&worker_policy)).unwrap();
    for blocked in [false,true] {
        amend_json(&worker_policy,&|value| {
            if blocked {value["ceiling"]["hard_blocked_tools"]=json!(["file.peek"]);}
            else {value["ceiling"]["tools"].as_array_mut().unwrap().retain(|tool|tool!="file.peek");}
        });
        assert!(plan_current_run_amendment(&request.roots,&current,&provider,CurrentRunRequest {amendment_key:"denied".into(),selection:Some(selection.clone()),authored:None},bridge.clone(),&retained).unwrap_err().contains("outside the formed worker/EE grant"));
        std::fs::write(root.join(&worker_policy),&allowed).unwrap();
    }
    let plan=plan_current_run_amendment(&request.roots,&current,&provider,CurrentRunRequest {amendment_key:"attach".into(),selection:Some(selection),authored:None},bridge.clone(),&retained).unwrap();
    assert!(plan.validation.safe_to_apply,"{:?}",plan.validation.regressions);
    let owner=request.roots.registry_root.join("interface").join(provider.as_str());
    assert!(plan.dependency_writes.iter().chain(&plan.activation_writes).all(|write|!write.path.starts_with(&owner)));
    assert!(plan.dependency_writes.iter().any(|write|write.path.to_string_lossy().contains("file.peek__")));
    let amended_policy:Value=serde_json::from_str(&plan.activation_writes.iter().find(|write|write.path==root.join(&worker_policy)).unwrap().content).unwrap();
    for field in ["interfaces","tools","wek","mark_outcomes","hard_blocked_tools"] {
        let old:Value=serde_json::from_slice(&allowed).unwrap();
        assert!(old["ceiling"][field].as_array().unwrap().iter().all(|value|amended_policy["ceiling"][field].as_array().unwrap().contains(value)),"retained {field}");
    }
    let private_peek=format!("file.peek{}",composition.authoring.capability_tools["file.read"].strip_prefix("file.read").unwrap());
    assert!(plan.dependency_writes.iter().any(|write|write.path==root.join("registry/tool").join(&private_peek).join("tool.json")));
    assert_eq!(std::fs::read(root.join("registry/interface/system_worker_template/policy.json")).unwrap(),source_policy);
    let profile_path=owner.join("interface.json");let before=std::fs::read(&profile_path).unwrap();
    let mut value:Value=serde_json::from_slice(&before).unwrap();value["interface_driver_config"]["private_config"]["run_affordances"]["defaults"]["tools"]=json!([]);
    let mut writes=vec![RegistryCandidateWrite {path:profile_path,content:serde_json::to_string(&value).unwrap(),expected_before_hash:Some(ContentHash::of_bytes(&before).to_hex())}];
    for new_file in [false,true] {
        if new_file {writes=vec![RegistryCandidateWrite {path:owner.join("new.json"),content:"{}".into(),expected_before_hash:None}];}
        let authored=AuthoredCurrentRun {expected_before_hash:ContentHash::of_bytes(composition.run_write.content.as_bytes()).to_hex(),expected_before:json!(current),run_record:json!(current),writes:writes.clone()};
        assert!(plan_current_run_amendment(&request.roots,&current,&provider,CurrentRunRequest {amendment_key:"unsafe".into(),selection:None,authored:Some(authored)},bridge.clone(),&retained).unwrap_err().contains("in-flight provider"));
    }
    assert_eq!(std::fs::read(&composition.run_write.path).unwrap(),composition.run_write.content.as_bytes());
    for write in plan.dependency_writes.iter().chain(&plan.activation_writes) {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();std::fs::write(&write.path,&write.content).unwrap();
    }
    let next=RunRecord::from_bytes(&serde_json::to_vec(&plan.amendment.after).unwrap()).unwrap();
    std::fs::write(&composition.run_write.path,serde_json::to_vec(&next).unwrap()).unwrap();
    let next_selection=current_selection(&request.roots,&next,&provider).unwrap().unwrap();
    assert_eq!(next_selection.selected_tools.len(),4);
    assert!(next_selection.selected_tools.iter().any(|tool|tool.tool=="file.peek" && tool.interface=="system_extra_room"));
    let repeated=plan_current_run_amendment(&request.roots,&next,&provider,CurrentRunRequest {amendment_key:"repeat".into(),selection:Some(next_selection.clone()),authored:None},bridge,&retained).unwrap();
    assert!(repeated.validation.safe_to_apply,"{:?}",repeated.validation.regressions);
    assert!(repeated.dependency_writes.is_empty(),"existing private IDs must be reused");
    let owner=&composition.authoring.capability_doors["system_room"];
    let authoring_door=format!("system_room_authoring_{owner}");let authoring_tool=format!("file.write__authoring_{owner}");
    for file in ["interface.json","life.json","octopus_os.json"] {
        let mut value:Value=serde_json::from_slice(&std::fs::read(root.join(format!("registry/interface/{owner}/{file}"))).unwrap()).unwrap();
        if file=="interface.json" {value["id"]=json!(authoring_door);}
        if file=="octopus_os.json" {value["interface"]=json!(authoring_door);}
        write(&root,&format!("registry/interface/{authoring_door}/{file}"),value);
    }
    let mut tool:Value=serde_json::from_slice(&std::fs::read(root.join(format!("registry/tool/{}/tool.json",composition.authoring.tool_id))).unwrap()).unwrap();
    tool["id"]=json!(authoring_tool);tool["interface"]=json!(authoring_door);
    write(&root,&format!("registry/tool/{authoring_tool}/tool.json"),tool);
    for (file,interfaces,tools) in [(worker_policy.as_str(),"/ceiling/interfaces","/ceiling/tools"),
        ("registry/ee_template/existence/ee_template.json","/allowed_interfaces","/allowed_tools"),
        ("ee/ee_existence.json","/policy_narrowing/allowed_interfaces","/policy_narrowing/allowed_tools")] {
        amend_json(file,&|value| {
            value.pointer_mut(interfaces).unwrap().as_array_mut().unwrap().push(json!(authoring_door));
            value.pointer_mut(tools).unwrap().as_array_mut().unwrap().push(json!(authoring_tool));
        });
    }
    let mut authored=json!(next);
    let returned=authored["interface_state"][provider.as_str()]["run_authoring_binding"]["checkpoints"].as_array_mut().unwrap().iter_mut()
        .find(|cp|cp["role"]=="stable" && cp["interface_binding"]["interface"]==*owner).unwrap();
    returned["role"]=json!("owner_input");returned["owner_authoring"]=json!({"owner":owner,"source":owner,"door":authoring_door,"tool":authoring_tool,
        "call_label":"tool.call","result_label":format!("{authoring_tool}.result")});
    let authored=RunRecord::from_bytes(&serde_json::to_vec(&authored).unwrap()).unwrap();
    let inspected=current_selection(&request.roots,&authored,&provider).unwrap().unwrap();
    assert_eq!(inspected.selected_tools.len(),4);
    assert_eq!(inspected.selected_tools,next_selection.selected_tools);
    assert!(!root.join("registry/interface/system_room_authoring_system_room/interface.json").exists());
    assert!(current_selection(&request.roots,&next,&provider).unwrap_err().contains("system_room_authoring_system_room/interface.json"));
}

#[test]
fn selected_model_and_reasoning_effort_preserve_service_tier() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    for tier in ["priority", "ultrafast", "default"] {
        request.service_tier = Some(tier.into());
        let plan = plan_run_composition(&request).unwrap();
        assert_eq!(plan.model, request.model);
        assert_eq!(plan.reasoning_effort, request.reasoning_effort);
        assert_eq!(plan.service_tier.as_deref(), Some(tier));
        let provider: Value = serde_json::from_str(&plan.descriptor_writes.iter()
            .find(|write| write.tag == format!("interface:{}", plan.provider_interface_id)).unwrap().content).unwrap();
        assert_eq!(provider["interface_driver_config"]["private_config"]["service_tier"], tier);
        let retained: RunCompositionPlan = serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
        assert_eq!(retained.service_tier, plan.service_tier);
    }
    for tier in ["", " priority", "priority\t"] {
        request.service_tier = Some(tier.into());
        assert!(plan_run_composition(&request).unwrap_err().contains("service tier"));
    }
    request.service_tier = None;
    let plan = plan_run_composition(&request).unwrap();
    assert_eq!(plan.service_tier, None);
}

#[test]
fn private_system_composition_is_fresh_exact_and_checked_under_policy() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    let parent_before = request.admitted_parent_record.clone();
    let plan = plan_run_composition(&request).unwrap();
    assert!(
        plan.required_binding_updates[0]
            .required_artifact_bindings
            .iter()
            .any(|binding| binding.id == "role-system_worker_template")
    );
    let retained: RunCompositionPlan =
        serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
    assert_eq!(retained.run_write.content, plan.run_write.content);
    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        serde_json::to_value(plan_run_composition(&request).unwrap()).unwrap()
    );
    assert_eq!(request.admitted_parent_record, parent_before);
    assert!(!request.roots.run_records_root.exists());
    assert!(!request.roots.interface_private_state_root.exists());
    assert_eq!(
        plan.receive_interface_id,
        request.communication.receive_interface_id
    );
    assert_ne!(
        plan.worker_interface_id,
        request.worker_template_interface_id
    );
    assert!(
        plan.descriptor_writes
            .iter()
            .all(|write| write.expected_before_hash.is_none() && !write.path.exists())
    );
    assert!(
        plan.descriptor_writes
            .iter()
            .all(|write| write.path.file_name().unwrap() != "octopus_os.json")
    );
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    assert_eq!(record.interface_state.len(), 1);
    let templates =
        record.interface_state[&plan.provider_interface_id.clone().into()]["run_authoring_binding"]
            ["checkpoints"]
            .as_array()
            .unwrap();
    let call = templates.iter().find(|row| row["role"] == "authoring_call").unwrap();
    assert_eq!(call["required_verified_artifacts"], json!([{
        "mark":request.authoring.tool_call_mark,
        "artifact_label":request.authoring.tool_call_label,
        "verifier":request.authoring.tool_call_verifier,
        "params":{"tools":[plan.authoring.tool_id],"metadata_matches":[{
            "payload":"/arguments/expected_before_hash","metadata":"/runtime/run_record_hash"
        }]}
    }]));
    assert!(record.checkpoints.iter().any(|cp| serde_json::to_value(&cp.required_verified_artifacts).unwrap() == call["required_verified_artifacts"]));
    assert!(templates.iter().filter(|row| row["role"] == "provider_tool_output")
        .all(|row| row["required_verified_artifacts"][0]["params"].get("metadata_matches").is_none()));
    assert!(
        templates
            .iter()
            .any(
                |row| row["interface_binding"]["interface"] == plan.receive_interface_id
                    && row["role"] == "stable"
                    && row["include_context"] == true
            )
    );
    assert_eq!(
        record
            .checkpoints
            .iter()
            .filter(|cp| cp
                .inbound_interface()
                .is_some_and(|id| id.as_str() == plan.receive_interface_id))
            .count(),
        1
    );
    assert!(!plan.run_write.content.contains("MUST NOT COPY"));
    assert!(!plan.run_write.content.contains("private-transcript"));
    assert!(!plan.run_write.content.contains("conversation"));
    assert!(!plan.run_write.content.contains("world_operator_chat"));
    assert!(
        !plan
            .run_write
            .content
            .contains("role-system_worker_unselected")
    );
    let input = record
        .checkpoints
        .iter()
        .find(|cp| {
            cp.inbound_interface()
                .is_some_and(|id| id.as_str() == plan.receive_interface_id)
        })
        .unwrap();
    assert!(
        input
            .artifact_bindings
            .iter()
            .any(
                |binding| binding.id.as_str() == "role-system_worker_template"
                    && binding.access.read
            )
    );
    assert!(
        input
            .artifact_bindings
            .iter()
            .any(
                |binding| binding.id.as_str() == "role-system_worker_parent" && binding.access.read
            )
    );
    assert_eq!(plan.seed.payload, request.seed.payload);
    assert_ne!(
        plan.authoring.capability_tools["file.write"],
        plan.authoring.tool_id
    );
    let writer: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| {
                write
                    .path
                    .ends_with(format!("{}/tool.json", plan.authoring.tool_id))
            })
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(
        writer["process"]["authority"]["write_argument_paths"][0]["within_root_slots"],
        json!(["run_records_root"])
    );
    assert_eq!(
        plan.required_binding_updates[0]
            .outbound_binding
            .interface
            .as_str(),
        plan.receive_interface_id
    );
    assert!(
        plan.required_policy_updates
            .iter()
            .any(|update| update.write.path.ends_with("ee_existence.json"))
    );
    for update in &plan.required_policy_updates {
        assert_eq!(
            update.write.expected_before_hash.as_ref().unwrap(),
            &blake3::hash(&std::fs::read(&update.write.path).unwrap())
                .to_hex()
                .to_string()
        );
    }
    let provider: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| {
                write
                    .path
                    .ends_with(format!("{}/interface.json", plan.provider_interface_id))
            })
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(
        provider["interface_policy"]["values"]["models"],
        json!(["fixture-a"])
    );
    for update in &plan.required_policy_updates {
        std::fs::write(&update.write.path, &update.write.content).unwrap();
    }
    for write in &plan.descriptor_writes {
        std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
        std::fs::write(&write.path, &write.content).unwrap();
    }
    let topology = octopus_world_terminal_client::form_capability_topology(
        &request.roots.registry_root,
        &octopus_world_terminal_client::CapabilityFormationRequest::from_live_chain(
            plan.worker_interface_id.clone(),
            octopus_world_terminal_client::resolve_live_ee_policy_chain(
                &request.roots.ee_instances_root,
                &request.ee_id.clone().into(),
            )
            .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(
        topology.interface_policies[&plan.provider_interface_id.clone().into()].context["models"],
        json!(["fixture-a"])
    );
    let retry = plan_run_composition(&request).unwrap();
    assert_eq!(retry.run_id, plan.run_id);
    assert_eq!(retry.run_write.content, plan.run_write.content);
    request.model = "fixture-b".into();
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("selected model")
    );
    request.model = "fixture-a".into();
    request.identity_seed = "call/second".into();
    request.communication.receive_interface_id = "system_worker_private_second".into();
    let second = plan_run_composition(&request).unwrap();
    assert_ne!(plan.run_id, second.run_id);
    assert_ne!(plan.provider_interface_id, second.provider_interface_id);
    assert_ne!(plan.worker_interface_id, second.worker_interface_id);
    request.communication.return_interface_ids = vec![request.provider_interface_id.clone()];
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("System interface")
    );
}

#[test]
fn selected_outputs_keep_recorded_activity_out_of_peer_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    request.communication.output_selection = Some(CompositionOutputSelection {
        return_labels: vec!["interface.artifact.output".into(), "turn.error".into()],
        record_labels: vec!["turn.stream".into(), "turn.completed".into()],
    });
    assert!(plan_run_composition(&request).unwrap_err().contains("must declare client_projection_output_labels"));
    let provider_path = request.roots.registry_root.join("interface/world_provider_fixture/interface.json");
    let mut provider: serde_json::Value = serde_json::from_slice(&std::fs::read(&provider_path).unwrap()).unwrap();
    provider["interface_driver_config"]["private_config"]["client_projection_output_labels"] =
        json!(["interface.artifact.output", "turn.error", "turn.stream", "turn.completed", "turn.input.acknowledged"]);
    std::fs::write(provider_path, serde_json::to_vec(&provider).unwrap()).unwrap();
    assert!(plan_run_composition(&request).unwrap_err().contains("turn.input.acknowledged"));
    request.communication.output_selection.as_mut().unwrap().record_labels.push("turn.input.acknowledged".into());
    let plan = plan_run_composition(&request).unwrap();
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    for (label, destinations) in [("interface.artifact.output", vec!["system_worker_parent"]), ("turn.stream", vec![]), ("turn.input.acknowledged", vec![])] {
        let matches = record.active_checkpoints.iter().filter_map(|id| record.checkpoint(id))
            .filter(|cp| cp.inbound_interface().is_some_and(|id| id.as_str() == plan.provider_interface_id))
            .filter(|cp| cp.required_verified_artifacts.is_empty())
            .filter(|cp| cp.interface_binding.as_ref().unwrap().admits(label)).collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].outbound_interfaces().map(|id| id.as_str()).collect::<Vec<_>>(), destinations);
    }
    request.communication.output_selection.as_mut().unwrap().record_labels.push("turn.error".into());
    assert!(plan_run_composition(&request).unwrap_err().contains("disjoint"));
}

#[test]
fn client_and_return_bindings_are_only_the_callers_explicit_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = fixture(dir.path());
    request.communication.return_interface_ids.clear();
    let plan = plan_run_composition(&request).unwrap();
    assert!(plan.return_interface_ids.is_empty());
    assert!(!plan.run_write.content.contains("role-system_worker_parent"));
    write(
        dir.path(),
        "registry/interface/world_client/interface.json",
        json!({"id":"world_client","interface_type":"world","role":"admin","direction":"bidirectional","source_names":["client"],"interface_driver_ref":"app_server"}),
    );
    request.communication.client_interface_id = Some("world_client".into());
    let plan = plan_run_composition(&request).unwrap();
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    assert!(record.checkpoints.iter().any(|cp| {
        cp.inbound_interface()
            .is_some_and(|id| id.as_str() == "world_client")
    }));
    assert!(record.checkpoints.iter().any(|cp| {
        cp.outbound_interfaces()
            .any(|id| id.as_str() == "world_client")
    }));
    assert!(plan.descriptor_writes.iter().all(|write| {
        !write
            .path
            .starts_with(request.roots.registry_root.join("interface/world_client"))
    }));
    let state = request
        .roots
        .interface_private_state_root
        .join(&plan.provider_interface_id);
    std::fs::create_dir_all(&state).unwrap();
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("existing private state")
    );
    std::fs::remove_dir(&state).unwrap();
    request.communication.receive_interface_id = request.worker_template_interface_id.clone();
    assert!(
        plan_run_composition(&request)
            .unwrap_err()
            .contains("private worker pin")
    );
}

#[test]
fn selected_client_template_authors_a_private_client_without_reusing_the_callers_chat() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = support::client_fixture(dir.path());
    let plan = plan_run_composition(&request).unwrap();
    let record: Value = serde_json::from_str(&plan.run_write.content).unwrap();
    let (client, state) = record["interface_state"].as_object().unwrap().iter().find(|(_, state)| state["client_surface_role"] == "task").unwrap();
    assert_ne!(client, "world_operator_chat");
    assert_ne!(client, &plan.receive_interface_id);
    assert_eq!(state["provider_runtime_interface"], plan.provider_interface_id);
    assert!(plan.required_interface_ids.contains(client));
    let sidecar: Value = serde_json::from_str(&plan.descriptor_writes.iter().find(|write| write.path.ends_with(format!("{client}/octopus_os.json"))).unwrap().content).unwrap();
    assert_eq!(sidecar["owner_interface"], *client);
    assert_eq!(sidecar["presentation"], json!({"name":"Context check", "projects":[]}));
    assert_eq!(sidecar["authoring_profile"]["worker_choice_interface"], request.worker_template_interface_id);
    assert!(record["checkpoints"].as_array().unwrap().iter().any(|cp| cp["interface_binding"]["interface"] == *client));
    assert!(!plan.run_write.content.contains("MUST NOT COPY"));
    for invalid in ["", " name", "name\ninside", &"x".repeat(81)] {
        request.communication.client_title = Some(invalid.into());
        assert!(plan_run_composition(&request).unwrap_err().contains("client_title"));
    }
    request.communication.client_title = Some("Context check".into());
    request.communication.client_template_interface_id = None;
    assert!(plan_run_composition(&request).unwrap_err().contains("client_title"));
    request.communication.client_template_interface_id = Some("world_operator_chat".into());
    request.communication.client_interface_id = Some("world_operator_chat".into());
    assert!(plan_run_composition(&request).unwrap_err().contains("not both"));
}

#[test]
fn owner_input_revision_retry_keeps_the_original_exact_request() {
    use octopus_operator_chat_client::run_revision::{RunRevisionRequest, prepare_run_revision};

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let request = support::client_fixture(&root);
    let plan = plan_run_composition(&request).unwrap();
    for edit in plan
        .descriptor_writes
        .iter()
        .chain(
            plan.required_policy_updates
                .iter()
                .map(|update| &update.write),
        )
        .chain(std::iter::once(&plan.run_write))
    {
        std::fs::create_dir_all(edit.path.parent().unwrap()).unwrap();
        std::fs::write(&edit.path, &edit.content).unwrap();
    }
    let mut record: Value = serde_json::from_str(&plan.run_write.content).unwrap();
    let client = record["interface_state"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, state)| state["client_surface_role"] == "task")
        .unwrap()
        .0
        .clone();
    let mut owner_input = json!({
        "name":"owner_result", "role":"owner_input", "include_context":false, "required_verified_artifacts":[],
        "interface_binding":octopus_runtime::AuthoredInterfaceBinding::all("system_worker_parent".into()),
        "outbound_interface_bindings":[octopus_runtime::AuthoredInterfaceBinding::all(plan.provider_interface_id.clone().into())],
        "owner_authoring":{"owner":"system_worker_parent", "source":"system_worker_parent",
            "door":"system_room", "tool":"file.write", "call_label":"tool.call", "result_label":"file.write.result"}
    });
    record["interface_state"][&plan.provider_interface_id]["run_authoring_binding"]["checkpoints"]
        .as_array_mut()
        .unwrap()
        .push(owner_input.clone());
    let before = serde_json::to_string_pretty(&record).unwrap();
    std::fs::write(&plan.run_write.path, &before).unwrap();
    std::fs::create_dir_all(root.join("ledger/blobs")).unwrap();
    std::fs::write(root.join("ledger/movements.jsonl"), b"").unwrap();
    owner_input["name"] = json!("revised_owner_result");
    let prepare = |desired: Value| {
        let request: RunRevisionRequest = serde_json::from_value(json!({
            "registry_root":request.roots.registry_root, "run_path":plan.run_write.path,
            "ledger_root":root.join("ledger"), "artifact_root":root.join("client"),
            "provider":plan.provider_interface_id, "source":client, "key":"owner-input-retry",
            "operation":{"kind":"owner_inputs", "desired_inputs":[desired]}, "expected_before_content":before
        })).unwrap();
        prepare_run_revision(request)
    };
    assert_eq!(prepare(owner_input.clone()).unwrap()["status"], "prepared");
    assert_eq!(prepare(owner_input.clone()).unwrap()["status"], "prepared");
    owner_input["name"] = json!("different_requested_result");
    assert!(
        prepare(owner_input)
            .unwrap_err()
            .contains("retry changed its original requested operation")
    );
    assert_eq!(
        std::fs::read_to_string(&plan.run_write.path).unwrap(),
        before
    );
}

#[test]
fn composition_binds_planned_native_catalog_and_task_coordinates_before_validation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut request = support::client_fixture(&root);
    request.communication.client_template_interface_id = None;
    request.communication.client_title = None;
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    for relative in ["interface/system_browser/interface.json", "interface/system_browser/life.json",
        "worker_environment_kind/browser_session/worker_environment_kind.json", "tool/browser.navigate/tool.json",
        "tool/browser.filechooser_set_files/tool.json", "tool/browser.filechooser_resume/tool.json"] {
        let value=serde_json::from_slice(&std::fs::read(source.join("registry").join(relative)).unwrap()).unwrap();
        write(&root.join("registry"),relative,value);
    }
    for (file, interfaces, tools) in [("registry/interface/system_worker_template/policy.json","/ceiling/interfaces","/ceiling/tools"),
        ("registry/ee_template/existence/ee_template.json","/allowed_interfaces","/allowed_tools"),
        ("ee/ee_existence.json","/policy_narrowing/allowed_interfaces","/policy_narrowing/allowed_tools")] {
        let mut value:Value=serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap();
        value.pointer_mut(interfaces).unwrap().as_array_mut().unwrap().push(json!("system_browser"));
        value.pointer_mut(tools).unwrap().as_array_mut().unwrap().extend([json!("browser.navigate"),json!("browser.filechooser_set_files"),json!("browser.filechooser_resume")]);
        if let Some(kinds)=value.pointer_mut("/ceiling/wek") {kinds.as_array_mut().unwrap().push(json!("browser_session"));}
        write(&root,file,value);
    }
    request.selected_tools.push(octopus_operator_chat_client::SelectedToolSeed {tool:"browser.navigate".into(),interface:"system_browser".into(),worker_environment_kind:"browser_session".into()});
    request.selected_worker_environment_kinds=vec!["system_room".into(),"browser_session".into()];
    enable_native_profile(&root);
    let plan = plan_run_composition(&request).unwrap();
    assert_eq!(plan.authoring.capability_tools.len(),3);
    assert!(plan.authoring.capability_tools.contains_key("browser.navigate"));
    let browser:Value=serde_json::from_str(&plan.descriptor_writes.iter().find(|write|write.tag==format!("interface:{}",plan.authoring.capability_doors["system_browser"])).unwrap().content).unwrap();
    assert!(browser["interface_driver_config"]["private_config"]["browser_room"].get("upload_approval").is_none());
    let provider: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| write.tag == format!("interface:{}", plan.provider_interface_id))
            .unwrap()
            .content,
    )
    .unwrap();
    let config = &provider["interface_driver_config"]["private_config"]["run_affordances"];
    assert_eq!(config["defaults"]["client_template_interface_id"],Value::Null);
    assert_eq!(
        config["file_write_tool"],
        plan.authoring.capability_tools["file.write"]
    );
    assert_eq!(
        config["artifact_send_tool"],
        plan.authoring.capability_tools["artifact.send"]
    );
    let catalog_id = config["catalog_artifact"]["id"].as_str().unwrap();
    let catalog: Value = serde_json::from_str(
        &plan
            .descriptor_writes
            .iter()
            .find(|write| write.tag == format!("artifact:{catalog_id}"))
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(
        config["catalog_artifact"]["content"],
        json!(octopus_ledger::ContentHash::of_bytes(
            &octopus_descriptors::canonical_bytes(&catalog)
        ))
    );
    let record = RunRecord::from_bytes(plan.run_write.content.as_bytes()).unwrap();
    let input = record
        .checkpoints
        .iter()
        .find(|checkpoint| {
            checkpoint
                .inbound_interface()
                .is_some_and(|interface| interface.as_str() == plan.receive_interface_id)
        })
        .unwrap();
    assert!(
        input
            .artifact_bindings
            .iter()
            .any(|binding| binding.id.as_str() == catalog_id && binding.access.read)
    );
    let coordinates = plan
        .descriptor_writes
        .iter()
        .find(|write| {
            write
                .tag
                .starts_with("artifact:artifact-run-authoring-coordinates-")
        })
        .unwrap();
    let coordinates: Value = serde_json::from_str(&coordinates.content).unwrap();
    assert!(
        input
            .artifact_bindings
            .iter()
            .any(|binding| json!(binding.id) == coordinates["id"] && binding.access.read)
    );
    let validation = validate_registry_candidate(
        &validation_roots(&request.roots),
        &composition_candidate_writes(&plan),
    )
    .unwrap();
    assert!(validation.safe_to_apply, "{:?}", validation.regressions);
    assert!(!request.roots.run_records_root.exists());
    let selected=request.selected_tools.clone();
    request.selected_tools.clear();
    assert!(plan_run_composition(&request).unwrap_err().contains("upload approval names another source client"));
    request.selected_tools=selected;
    request.selected_tools.retain(|tool|tool.tool!="file.write");
    let without_writer=plan_run_composition(&request).unwrap();
    assert_eq!(without_writer.authoring.capability_tools["file.write"],without_writer.authoring.tool_id);
    request.communication.client_template_interface_id=Some("world_operator_chat".into());
    request.communication.client_title=Some("Background task".into());
    request.selected_tools.push(octopus_operator_chat_client::SelectedToolSeed {tool:"file.write".into(),interface:"system_room".into(),worker_environment_kind:"system_room".into()});
    let child=plan_run_composition(&request).unwrap();
    let provider:Value=serde_json::from_str(&child.descriptor_writes.iter().find(|write|write.tag==format!("interface:{}",child.provider_interface_id)).unwrap().content).unwrap();
    assert_eq!(provider["interface_driver_config"]["private_config"]["run_affordances"]["defaults"]["client_template_interface_id"],"world_operator_chat");
    for edit in composition_candidate_writes(&child) {
        std::fs::create_dir_all(edit.path.parent().unwrap()).unwrap(); std::fs::write(edit.path,edit.content).unwrap();
    }
    std::fs::create_dir_all(root.join("ledger/blobs")).unwrap();
    std::fs::write(root.join("ledger/movements.jsonl"),"").unwrap();
    let provider_path=request.roots.registry_root.join("interface").join(&child.provider_interface_id).join("interface.json");
    let mut old=provider.clone();
    let defaults=old["interface_driver_config"]["private_config"]["run_affordances"]["defaults"].as_object_mut().unwrap();
    defaults.remove("client_template_interface_id"); defaults.insert("return_interface_ids".into(),json!(["chosen.destination"]));
    std::fs::write(&provider_path,serde_json::to_vec_pretty(&old).unwrap()).unwrap();
    let source_path=root.join("registry/interface/world_provider_fixture/interface.json");
    let mut source:Value=serde_json::from_slice(&std::fs::read(&source_path).unwrap()).unwrap();
    source["interface_driver_config"]["private_config"]["run_affordance_profile"]["catalog"]["operations"][0]["description"]=json!("Updated release description");
    std::fs::write(&source_path,serde_json::to_vec_pretty(&source).unwrap()).unwrap();
    let original_run=std::fs::read(&child.run_write.path).unwrap();
    let plan_adoption=|| {
        octopus_operator_chat_client::native_run_authoring::plan_existing_adoption(&serde_json::from_value(adoption_request(&root,&request,&child)).unwrap())
            .map(|plan|serde_json::to_value(plan).unwrap())
    };
    let adopted=plan_adoption().unwrap(); assert_eq!(adopted["status"],"planned"); assert_eq!(adopted["writes"].as_array().unwrap().len(),2);
    let updated:Value=serde_json::from_str(adopted["writes"][1]["content"].as_str().unwrap()).unwrap();
    let mut expected=old.clone();
    let private=&mut expected["interface_driver_config"]["private_config"];
    private["run_affordances"]["defaults"]["client_template_interface_id"]=json!("world_operator_chat");
    private["run_affordances"]["catalog_artifact"]=updated["interface_driver_config"]["private_config"]["run_affordances"]["catalog_artifact"].clone();
    private["run_affordance_profile"]=source["interface_driver_config"]["private_config"]["run_affordance_profile"].clone();
    assert_eq!(updated,expected);
    let catalog_path=adopted["writes"][0]["path"].as_str().unwrap();
    let catalog_bytes=std::fs::read(catalog_path).unwrap();
    let mut local:Value=serde_json::from_slice(&catalog_bytes).unwrap(); local["text"]=json!("Locally authored catalog");
    std::fs::write(catalog_path,serde_json::to_vec(&local).unwrap()).unwrap();
    assert!(plan_adoption().unwrap_err().contains("prior owner-generated"));
    std::fs::write(catalog_path,catalog_bytes).unwrap();
    let mut explicit=old.clone();
    explicit["interface_driver_config"]["private_config"]["run_affordances"]["defaults"]["client_template_interface_id"]=Value::Null;
    std::fs::write(&provider_path,serde_json::to_vec(&explicit).unwrap()).unwrap();
    let explicit=plan_adoption().unwrap();
    let explicit:Value=serde_json::from_str(explicit["writes"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(explicit["interface_driver_config"]["private_config"]["run_affordances"]["defaults"]["client_template_interface_id"],Value::Null);
    std::fs::write(&provider_path,serde_json::to_vec_pretty(&old).unwrap()).unwrap();
    for edit in adopted["writes"].as_array().unwrap() {std::fs::write(edit["path"].as_str().unwrap(),edit["content"].as_str().unwrap()).unwrap();}
    assert_eq!(plan_adoption().unwrap()["status"],"unchanged");
    assert_eq!(std::fs::read(&child.run_write.path).unwrap(),original_run);
}

#[test]
fn package_tool_contract_adoption_requires_exact_predecessor_owner_and_settlement() {
    use octopus_operator_chat_client::{SelectedToolSeed, native_run_authoring::plan_existing_adoption};
    let temp = tempfile::tempdir().unwrap(); let root = temp.path().canonicalize().unwrap();
    let mut request = support::client_fixture(&root); enable_native_profile(&root);
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    for name in ["interface.json", "life.json"] {
        write(&root, &format!("registry/interface/system_context/{name}"),
            serde_json::from_slice(&std::fs::read(source.join("registry/interface/system_context").join(name)).unwrap()).unwrap());
    }
    for (file, pointer, item) in [
        ("registry/interface/system_worker_template/policy.json", "/ceiling/tools", "client.mcp"),
        ("registry/interface/system_worker_template/policy.json", "/ceiling/interfaces", "system_context"),
        ("registry/ee_template/existence/ee_template.json", "/allowed_tools", "client.mcp"),
        ("registry/ee_template/existence/ee_template.json", "/allowed_interfaces", "system_context"),
        ("ee/ee_existence.json", "/policy_narrowing/allowed_tools", "client.mcp"),
        ("ee/ee_existence.json", "/policy_narrowing/allowed_interfaces", "system_context"),
    ] {
        let mut value: Value = serde_json::from_slice(&std::fs::read(root.join(file)).unwrap()).unwrap();
        value.pointer_mut(pointer).unwrap().as_array_mut().unwrap().push(json!(item)); write(&root,file,value);
    }
    let canonical: Value = serde_json::from_slice(&std::fs::read(source.join("registry/tool/client.mcp/tool.json")).unwrap()).unwrap();
    let config: Value = serde_json::from_slice(&std::fs::read(root.join("registry/interface/world_operator_chat/client.json")).unwrap()).unwrap();
    let mut previous = canonical.clone();
    for (pointer, value) in config["tool_contract_updates"][0]["predecessors"][0].as_object().unwrap() {
        *previous.pointer_mut(pointer).unwrap() = value.clone();
    }
    write(&root,"registry/tool/client.mcp/tool.json",previous);
    request.selected_tools.push(SelectedToolSeed {tool:"client.mcp".into(),interface:"system_context".into(),worker_environment_kind:"system_room".into()});
    let child = plan_run_composition(&request).unwrap();
    for edit in composition_candidate_writes(&child) {
        std::fs::create_dir_all(edit.path.parent().unwrap()).unwrap(); std::fs::write(edit.path,edit.content).unwrap();
    }
    std::fs::create_dir_all(root.join("ledger/blobs")).unwrap(); std::fs::write(root.join("ledger/movements.jsonl"),"").unwrap();
    write(&root,"registry/tool/client.mcp/tool.json",canonical.clone());
    let tool = &child.authoring.capability_tools["client.mcp"];
    let tool_path = root.join("registry/tool").join(tool).join("tool.json");
    let before = std::fs::read(&tool_path).unwrap(); let run_before = std::fs::read(&child.run_write.path).unwrap();
    let plan = || plan_existing_adoption(&serde_json::from_value(adoption_request(&root,&request,&child)).unwrap())
        .map(|plan|serde_json::to_value(plan).unwrap());
    let adopted = plan().unwrap(); assert_eq!(adopted["status"],"planned");
    assert_eq!(adopted["writes"].as_array().unwrap().len(),1); assert_eq!(adopted["writes"][0]["path"],json!(tool_path));
    let next: Value = serde_json::from_str(adopted["writes"][0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(next["handler_config"],canonical["handler_config"]);
    let current = RunRecord::from_bytes(&run_before).unwrap();
    let call = octopus_interfaces::InterfaceArtifactFile::write(root.join("accepted-call"), "tool.call",
        &serde_json::to_vec(&json!({"tool":tool,"arguments":{},"correlation_id":"pending-contract-call"})).unwrap()).unwrap();
    let accepted = call.load_into_blob(&mut octopus_ledger_store::DurableBlobs::open(root.join("ledger/blobs")).unwrap()).unwrap().artifact;
    let door = child.authoring.capability_doors["system_context"].clone();
    let metadata = octopus_interfaces::InvocationMetadata::new(octopus_interfaces::GrantProjection {
        tools:vec![tool.clone()], ..Default::default()
    }).with_runtime(octopus_interfaces::RuntimeInvocationMetadata {
        run:current.id.clone(), ee:current.ee.clone(), worker:current.worker.clone(),
        checkpoint:current.checkpoints[0].id.clone(), interface:door.clone().into(), epoch:0, run_record_hash:None,
    });
    let accepted = octopus_ledger::Movement { seq:octopus_ledger::Seq(0), lamport:octopus_ledger::Lamport(1), recorded_at_unix_ms:1,
        trace:current.trace.clone(), kind:octopus_ledger::MovementKind::InterfaceOutbound,
        from:octopus_ledger::Endpoint::Interface(child.provider_interface_id.clone().into()),
        to:octopus_ledger::Endpoint::Interface(door.into()), artifact:Some(accepted), invocation_metadata:Some(metadata.into()) };
    std::fs::write(root.join("ledger/movements.jsonl"),format!("{}\n",serde_json::to_string(&accepted).unwrap())).unwrap();
    let deferred=plan().unwrap();assert_eq!(deferred["status"],"deferred");assert_eq!(deferred["writes"],json!([]));
    assert!(deferred["reason"].as_str().unwrap().contains("accepted tool work"));assert_eq!(std::fs::read(&tool_path).unwrap(),before);
    std::fs::write(root.join("ledger/movements.jsonl"),"").unwrap();
    let mut pending: Value = serde_json::from_slice(&run_before).unwrap(); pending["interface_state"][&child.provider_interface_id]["pending_handoff"]=json!({"terminal":false,"source_movement_seq":0});
    std::fs::write(&child.run_write.path,serde_json::to_vec(&pending).unwrap()).unwrap();
    let deferred=plan().unwrap();assert_eq!(deferred["status"],"deferred");assert_eq!(deferred["writes"],json!([]));
    assert_eq!(std::fs::read(&tool_path).unwrap(),before);std::fs::write(&child.run_write.path,&run_before).unwrap();
    let sidecar=root.join("registry/interface").join(&child.authoring.capability_doors["system_context"]).join("octopus_os.json");
    let ownership=std::fs::read(&sidecar).unwrap(); let mut wrong:Value=serde_json::from_slice(&ownership).unwrap();wrong["owner_interface"]=json!("foreign");
    std::fs::write(&sidecar,serde_json::to_vec(&wrong).unwrap()).unwrap();assert!(plan().unwrap_err().contains("exact client"));std::fs::write(&sidecar,ownership).unwrap();
    let provider_path=root.join("registry/interface").join(&child.provider_interface_id).join("interface.json");
    let provider:Value=serde_json::from_slice(&std::fs::read(&provider_path).unwrap()).unwrap();
    let catalog_path=root.join("registry/artifact").join(provider["interface_driver_config"]["private_config"]["run_affordances"]["catalog_artifact"]["id"].as_str().unwrap()).join("artifact.json");
    let source_path=root.join("registry/interface/world_provider_fixture/interface.json");
    let prior_source=std::fs::read(&source_path).unwrap();let mut updated_source:Value=serde_json::from_slice(&prior_source).unwrap();
    updated_source["interface_driver_config"]["private_config"]["run_affordance_profile"]["catalog"]["text"]=json!("Updated package catalog");
    std::fs::write(&source_path,serde_json::to_vec(&updated_source).unwrap()).unwrap();
    assert_eq!(plan().unwrap()["writes"].as_array().unwrap().len(),3);
    let protected=[&child.run_write.path,&provider_path,&catalog_path,&source_path,&sidecar].map(|path|(path,std::fs::read(path).unwrap()));
    for pointer in ["/handler_config/mcp/tools/3/input_schema", "/handler_config/mcp/tools/0/description", "/description"] {
        let mut local:Value=serde_json::from_slice(&before).unwrap(); *local.pointer_mut(pointer).unwrap()=if pointer.ends_with("input_schema") {json!({"type":"object","required":["locally_authored"]})} else {json!("Local tool description")};
        std::fs::write(&tool_path,serde_json::to_vec(&local).unwrap()).unwrap();let retained=std::fs::read(&tool_path).unwrap();
        let deferred=plan().unwrap();assert_eq!(deferred["status"],"deferred");assert_eq!(deferred["writes"],json!([]));
        assert!(deferred["reason"].as_str().unwrap().contains("locally authored tools"));assert_eq!(std::fs::read(&tool_path).unwrap(),retained);
        for (path,bytes) in &protected {assert_eq!(&std::fs::read(path).unwrap(),bytes);}
    }
    let worker_path=root.join("registry/interface").join(&child.worker_interface_id).join("policy.json");
    let worker_before=std::fs::read(&worker_path).unwrap();let mut blocked:Value=serde_json::from_slice(&worker_before).unwrap();
    blocked["ceiling"]["hard_blocked_tools"]=json!([tool]);std::fs::write(&worker_path,serde_json::to_vec(&blocked).unwrap()).unwrap();
    assert!(plan().unwrap_err().contains("outside the actual formed grant"));std::fs::write(&worker_path,worker_before).unwrap();
    for (pointer,value) in [("/id",json!("foreign.tool")),("/interface",json!("foreign_door")),("/process",Value::Null),
        ("/process/bin",json!(""))] {
        let mut invalid:Value=serde_json::from_slice(&before).unwrap();*invalid.pointer_mut(pointer).unwrap()=value;
        std::fs::write(&tool_path,serde_json::to_vec(&invalid).unwrap()).unwrap();let retained=std::fs::read(&tool_path).unwrap();
        let error=plan().unwrap_err();if matches!(pointer,"/id"|"/interface") {assert!(error.contains("another exact identity or door"));}
        assert_eq!(std::fs::read(&tool_path).unwrap(),retained);
    }
    std::fs::write(&source_path,prior_source).unwrap();
    std::fs::write(&tool_path,&before).unwrap();
    std::fs::write(&tool_path,adopted["writes"][0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(plan().unwrap()["status"],"unchanged");assert_eq!(std::fs::read(&child.run_write.path).unwrap(),run_before);
    let provider:Value=serde_json::from_slice(&std::fs::read(provider_path).unwrap()).unwrap();
    request.selected_tools=serde_json::from_value(provider["interface_driver_config"]["private_config"]["run_affordances"]["defaults"]["tools"].clone()).unwrap();
    request.identity_seed="future-child-with-inherited-selection".into();
    request.communication.receive_interface_id="system_worker_future_child".into();
    let future=plan_run_composition(&request).unwrap();
    let future_tool=future.descriptor_writes.iter().find(|write|write.tag==format!("tool:{}",future.authoring.capability_tools["client.mcp"])).unwrap();
    let future_tool:Value=serde_json::from_str(&future_tool.content).unwrap();
    assert_eq!(future_tool["handler_config"],canonical["handler_config"]);
}

#[tokio::test]
async fn input_revision_cli_preserves_payload_intent_and_requires_checked_writer_results() {
    use futures::{SinkExt, StreamExt};
    use octopus_app_server::{SharedBlobHandle, SpineRootConfig, admit_interface_artifact_arrival, build_embedded_spine};
    use octopus_descriptors::InterfaceId;
    use octopus_interface_transport::{InterfaceActivationArrival, InterfaceHostIoLimits, InterfaceLiftContext};
    use octopus_interfaces::{DeliveryVerdict, InterfaceArtifactFile, InterfaceArtifactManifest, InterfaceInvocationEnvelope};
    use octopus_ledger::{ContentHash, Endpoint, LiveMovementLog, MovementKind, MovementLog, Seq, SharedBlobs};
    use octopus_ledger_store::{DurableBlobs, DurableLog};
    use octopus_operator_chat_client::{OperatorWriterRunPlanRequest, plan_operator_writer_runs};
    use octopus_provider_run_authoring::{ProviderEmission, ProviderEmissionRoute, ProviderHandoffSession};
    use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact};
    use octopus_world_pyecho_life_runtime::LedgerTailReader;
    use std::{collections::BTreeMap, io::Write, path::Path, process::{Command, Stdio}, sync::{Arc, Mutex}};

    fn cli(flag: &str, request: &Value) -> Result<Value, String> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_octopus-operator-chat-topology"))
            .arg(flag).env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(&serde_json::to_vec(request).unwrap()).unwrap();
        let output = child.wait_with_output().unwrap();
        if output.status.success() { Ok(serde_json::from_slice(&output.stdout).unwrap()) }
        else { Err(String::from_utf8(output.stderr).unwrap()) }
    }

    fn transport_call(root: &Path, ready: &Value) -> InterfaceArtifactFile {
        let manifest: InterfaceArtifactManifest = serde_json::from_value(ready["manifest"].clone()).unwrap();
        InterfaceArtifactFile::store_manifest_bounded(root, &serde_json::to_vec(&manifest).unwrap(), 1<<20, 1<<26).unwrap()
    }

    struct Boundary {
        runtime: octopus_app_server::EmbeddedRuntime,
        log: LiveMovementLog<DurableLog>,
        blobs: SharedBlobHandle,
        ctx: InterfaceLiftContext,
        arrivals: tokio::sync::mpsc::Receiver<InterfaceActivationArrival>,
        socket: tokio::net::UnixListener,
        author: InterfaceId,
        door: InterfaceId,
        writes: usize,
    }
    impl Boundary {
        fn file(&self, label: &str, body: &Value) -> InterfaceArtifactFile {
            InterfaceArtifactFile::write(self.ctx.var_dir.join("emitted"), label, &serde_json::to_vec(body).unwrap()).unwrap()
        }
        async fn admit(&mut self, file: &InterfaceArtifactFile, source: &InterfaceId) -> DeliveryVerdict {
            let captured = file.clone().capture_bounded(1<<20, 1<<26).unwrap();
            match admit_interface_artifact_arrival(&self.runtime, &mut self.log, &mut self.blobs, &captured, source).await {
                Ok(outcome) => DeliveryVerdict::Admitted { seq: outcome.admission.records.first().unwrap().arrival_movement.seq.0 },
                Err(error) => error.verdict(),
            }
        }
        fn input(&self, file: &InterfaceArtifactFile, interface: &InterfaceId) -> (u64, InterfaceInvocationEnvelope) {
            let mut ctx = self.ctx.clone(); ctx.interface = interface.clone();
            let mut reader = LedgerTailReader::open_named(&ctx, "revision-test").unwrap();
            let hash = ContentHash::of_bytes(&std::fs::read(file.manifest_path()).unwrap());
            let rows = reader.read_new().unwrap();
            let row = rows.iter().rev().find(|row| row.movement.artifact.as_ref().is_some_and(|a| a.content == hash)).unwrap();
            (row.movement.seq.0, reader.envelope(row).unwrap())
        }
        async fn effect(&mut self, file: &InterfaceArtifactFile) -> (DeliveryVerdict, InterfaceArtifactFile) {
            let author = self.author.clone();
            let verdict = self.admit(file, &author).await;
            assert!(matches!(verdict, DeliveryVerdict::Admitted { .. }), "{verdict:?}");
            let (_, envelope) = self.input(file, &self.door);
            let call: ToolCallArtifact = serde_json::from_slice(&envelope.read_artifact_payload().unwrap()).unwrap();
            let body = octopus_system_room_env::execute_file_write_result(&envelope, &call.arguments, &["system_room".into()],
                &BTreeMap::from([("run_records_root".into(),self.ctx.root_slots["run_records_root"].clone())]), &self.ctx.var_dir.join("effects"), 1<<26).unwrap();
            self.writes += 1;
            let result = ToolResultArtifact::success(&call.tool, call.correlation_id, body);
            (verdict, self.file(&result.label, &json!(result)))
        }
        async fn finish(&mut self, future: impl std::future::Future<Output = Result<(), String>>) {
            let (finished, mut done) = tokio::sync::oneshot::channel();
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                let (result, ()) = tokio::join!(async { let result = future.await; let _ = finished.send(()); result }, async {
                    loop { tokio::select! {
                        _ = &mut done => break,
                        arrival = self.arrivals.recv() => {
                            let arrival = arrival.unwrap(); let provider = self.ctx.interface.clone();
                            let verdict = self.admit(&arrival.artifact, &provider).await;
                            arrival.verdict_tx.unwrap().send(verdict).unwrap();
                        }
                        connection = self.socket.accept() => {
                            let mut ws = tokio_tungstenite::accept_async(connection.unwrap().0).await.unwrap();
                            let bytes = ws.next().await.unwrap().unwrap().into_data();
                            let path = self.ctx.var_dir.join("incoming.json"); std::fs::write(&path, bytes).unwrap();
                            let (verdict,result) = self.effect(&InterfaceArtifactFile::new(path).unwrap()).await;
                            let door = self.door.clone();
                            assert!(matches!(self.admit(&result, &door).await, DeliveryVerdict::Admitted { .. }));
                            ws.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&verdict).unwrap().into())).await.unwrap();
                        }
                    }}
                });
                result.unwrap();
            }).await.unwrap();
        }
    }

    for separate_writer in [false, true] {
        let temp = tempfile::Builder::new().prefix("oct-rev-").tempdir_in("/tmp").unwrap(); let root = temp.path().canonicalize().unwrap();
        let request = support::client_fixture(&root); enable_native_profile(&root); let plan = plan_run_composition(&request).unwrap();
        for edit in plan.descriptor_writes.iter().chain(plan.required_policy_updates.iter().map(|u| &u.write)).chain(std::iter::once(&plan.run_write)) {
            std::fs::create_dir_all(edit.path.parent().unwrap()).unwrap(); std::fs::write(&edit.path, &edit.content).unwrap();
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry/verifier/tool_call_allowed");
        let target = request.roots.registry_root.join("verifier/tool_call_allowed"); std::fs::create_dir_all(&target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap(); if entry.file_type().unwrap().is_file() { std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap(); }
        }
        if separate_writer {
            use sha2::Digest;
            let current = std::fs::read(&plan.run_write.path).unwrap();
            for edit in plan_operator_writer_runs(&OperatorWriterRunPlanRequest { run_records_root:request.roots.run_records_root.clone(),run_id:plan.run_id.clone(),expected_before_sha256:format!("{:x}",sha2::Sha256::digest(current)) }).unwrap() {
                std::fs::write(edit.path, edit.content).unwrap();
            }
        }
        let current = || std::fs::read_to_string(&plan.run_write.path).unwrap();
        let initial: Value = serde_json::from_str(&current()).unwrap();
        let client: InterfaceId = initial["interface_state"].as_object().unwrap().iter().find(|(_,state)| state["client_surface_role"] == "task").unwrap().0.as_str().into();
        let provider: InterfaceId = plan.provider_interface_id.clone().into();
        let author: InterfaceId = plan.authoring.source_interface_id.clone().into(); let door: InterfaceId = plan.authoring.room_interface_id.clone().into();
        let blobs: SharedBlobs = Arc::new(Mutex::new(DurableBlobs::open(root.join("ledger/blobs")).unwrap()));
        let runtime = build_embedded_spine(SpineRootConfig { registry_root:request.roots.registry_root.clone(),run:plan.run_id.clone().into(),blobs:blobs.clone(),
            interface_artifact_root:root.join("artifacts"),run_records_root:request.roots.run_records_root.clone(),ee_instances_root:request.roots.ee_instances_root.clone() }).await.unwrap();
        assert!(runtime.worker_interface_policies.contains_key(&plan.worker_interface_id.clone().into()), "planned worker {}", std::fs::read_to_string(request.roots.registry_root.join("interface").join(&plan.worker_interface_id).join("policy.json")).unwrap());
        let log = LiveMovementLog::new(DurableLog::open(root.join("ledger/movements.jsonl")).unwrap());
        let (sender, arrivals) = tokio::sync::mpsc::channel(16);
        let descriptor = serde_json::from_slice(&std::fs::read(request.roots.registry_root.join("interface").join(provider.as_str()).join("interface.json")).unwrap()).unwrap();
        let mut ctx = InterfaceLiftContext::unprivileged(provider.clone(), request.roots.registry_root.join("interface").join(provider.as_str()),Some(descriptor),root.join("provider"),
            root.join("ledger/movements.jsonl"),root.join("ledger/blobs"),root.join("ledger/ledger-index.sqlite"),
            BTreeMap::from([("run_records_root".into(), request.roots.run_records_root.clone())]),sender,Default::default());
        std::fs::create_dir(&ctx.var_dir).unwrap();
        ctx.root_slots.insert("interface_hand_in_root".into(), root.join("hand-in")); ctx.root_slots.insert("app_server_control_root".into(), root.join("control"));
        ctx.host_io_limits = Some(InterfaceHostIoLimits {max_control_frame_bytes:1<<20,max_manifest_bytes:1<<20,max_payload_bytes:1<<26,max_stderr_bytes:1024});
        let socket_path = octopus_app_server_interface::interface_control_socket_path_from_root(&root.join("control"), &author);
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap(); let socket = tokio::net::UnixListener::bind(socket_path).unwrap();
        let mut b = Boundary {runtime,log,blobs:SharedBlobHandle::new(blobs.clone()),ctx,arrivals,socket,author,door:door.clone(),writes:0};
        let source_path=root.join("registry/interface/world_provider_fixture/interface.json");
        let mut updated_source:Value=serde_json::from_slice(&std::fs::read(&source_path).unwrap()).unwrap();
        updated_source["interface_driver_config"]["private_config"]["run_affordance_profile"]["catalog"]["text"]=json!("New native catalog release");
        std::fs::write(source_path,serde_json::to_vec(&updated_source).unwrap()).unwrap();
        let attachment = b.file("fixture.payload", &json!({"text":"exact selected payload"}));
        let selected = json!([attachment.load_into_blob(&mut *blobs.lock().unwrap()).unwrap().artifact]);
        let mut scope = json!({"registry_root":request.roots.registry_root,"run_path":plan.run_write.path,"ledger_root":root.join("ledger"),
            "artifact_root":root.join("client"),"provider":provider,"source":client,"key":"first-payload","selected_payloads":selected});
        let prepare = |scope: &Value, operation: Value, before: &str| {
            let mut command = scope.clone(); command.as_object_mut().unwrap().remove("selected_payloads");
            command["operation"] = operation; command["expected_before_content"] = json!(before);
            cli("--prepare-input-revision", &command)
        };
        let verify = |scope: &Value| cli("--verify-input-revision", scope);
        let operation = json!({"kind":"payloads","selected":selected});
        let before = current();
        assert_eq!(prepare(&scope,json!({"kind":"owner_inputs","desired_inputs":[]}),&before).unwrap()["status"], "unchanged");
        assert_eq!(prepare(&scope,operation.clone(),&before).unwrap()["status"], "prepared");
        assert_eq!(current(),before);
        let ready = verify(&scope).unwrap(); assert_eq!(ready["status"], "ready");
        let mut wrong_source = scope.clone(); wrong_source["source"] = json!(b.author);
        assert!(verify(&wrong_source).unwrap_err().contains("exact client"));
        let mut wrong_selection = scope.clone(); wrong_selection["selected_payloads"] = json!([]);
        assert!(verify(&wrong_selection).unwrap_err().contains("selected payload"));
        let call = transport_call(&root.join("wire"),&ready);
        let (_,returned) = b.effect(&call).await; assert_eq!(b.writes,1);
        assert_ne!(current(),before);
        assert_eq!(verify(&scope).unwrap()["status"], "awaiting_result");
        assert_eq!(prepare(&scope,operation,&before).unwrap()["status"], "prepared");
        assert_eq!(verify(&scope).unwrap()["status"], "awaiting_result");
        assert!(matches!(b.admit(&returned, &door).await, DeliveryVerdict::Admitted { .. }));
        let installed = verify(&scope).unwrap(); assert_eq!(installed["status"], "installed");
        assert_eq!(verify(&scope).unwrap(), installed); assert_eq!(b.writes,1);
        let original: Value = serde_json::from_str(&before).unwrap(); let changed: Value = serde_json::from_str(&current()).unwrap();
        assert_eq!(changed["interface_state"][client.as_str()],original["interface_state"][client.as_str()]);
        assert_eq!(changed["interface_state"][provider.as_str()]["run_authoring_binding"]["payload_artifact_bindings"],selected);
        let message = b.file("operator.message", &json!({"text":"fixture turn","message_id":"settle"}));
        assert!(matches!(b.admit(&message,&client).await, DeliveryVerdict::Admitted { .. }));
        scope["key"] = json!("admitted-initial-definition");
        assert_eq!(prepare(&scope,json!({"kind":"owner_inputs","desired_inputs":[]}),&current()).unwrap()["status"], "deferred");
        let (seq,input) = b.input(&message,&provider); let pinned = b.ctx.pinned_run_record_bytes(&input).unwrap();
        let handoff = ProviderHandoffSession::new(&b.ctx).unwrap().from_invocation(&b.ctx,b.ctx.descriptor_snapshot.as_ref().unwrap(),&plan.run_id.clone().into(),&input,seq,&pinned).unwrap();
        assert_eq!(cli("--plan-native-adoption",&adoption_request(&root,&request,&plan)).unwrap()["status"],"deferred");
        let unfinished_ledger=std::fs::read(root.join("ledger/movements.jsonl")).unwrap();
        let output = b.file("interface.artifact.output", &json!({"text":"fixture settled"}));
        b.finish(async { handoff.hand_final(ProviderEmission {artifact:output,route:ProviderEmissionRoute::ClientProjection}).await.map(|_| ()) }).await;
        let adoption=adoption_request(&root,&request,&plan);
        assert_eq!(cli("--plan-native-adoption",&adoption).unwrap()["status"],"planned");
        let mut missing=adoption.clone(); missing["ledger_root"]=json!(root.join("unadmitted"));
        std::fs::create_dir(root.join("unadmitted")).unwrap(); std::fs::write(root.join("unadmitted/movements.jsonl"),unfinished_ledger).unwrap();
        std::os::unix::fs::symlink(root.join("ledger/blobs"),root.join("unadmitted/blobs")).unwrap();
        std::os::unix::fs::symlink(root.join("ledger/metadata"),root.join("unadmitted/metadata")).unwrap();
        assert_eq!(cli("--plan-native-adoption",&missing).unwrap()["status"],"deferred");
        let stop=b.file("operator.turn.control",&json!({"action":"stop","control_id":"adoption-stop"}));
        assert!(matches!(b.admit(&stop,&client).await,DeliveryVerdict::Admitted{..}));
        let stopped_plan=cli("--plan-native-adoption",&adoption_request(&root,&request,&plan)).unwrap();
        assert_eq!(stopped_plan["status"],"deferred"); assert_eq!(stopped_plan["writes"],json!([]));
        let (stop_seq,stop_input)=b.input(&stop,&provider); let stop_pin=b.ctx.pinned_run_record_bytes(&stop_input).unwrap();
        let stopped=ProviderHandoffSession::new(&b.ctx).unwrap().from_invocation(&b.ctx,b.ctx.descriptor_snapshot.as_ref().unwrap(),&plan.run_id.clone().into(),&stop_input,stop_seq,&stop_pin).unwrap();
        let acknowledged=b.file("turn.control.acknowledged",&json!({"action":"stop","source_movement_seq":stop_seq}));
        b.finish(async {stopped.hand_final(ProviderEmission {artifact:acknowledged,route:ProviderEmissionRoute::ClientProjection}).await.map(|_|())}).await;
        assert_eq!(cli("--plan-native-adoption",&adoption_request(&root,&request,&plan)).unwrap()["status"],"planned");
        let settled = current(); let writes = b.writes;
        scope["key"] = json!("ordinary-rearm");
        assert_eq!(prepare(&scope,json!({"kind":"rearm"}),&settled).unwrap()["status"], "prepared");
        let ready = verify(&scope).unwrap(); assert_eq!(ready["status"], "ready");
        let (_,result) = b.effect(&transport_call(&root.join("wire"),&ready)).await;
        assert!(matches!(b.admit(&result,&door).await,DeliveryVerdict::Admitted{..}));
        assert_eq!(verify(&scope).unwrap()["status"], "installed");
        assert_eq!(verify(&scope).unwrap()["status"], "installed"); assert_eq!(b.writes,writes+1);
        let after: Value = serde_json::from_str(&current()).unwrap(); let settled: Value = serde_json::from_str(&settled).unwrap();
        assert_eq!(after["interface_state"][provider.as_str()]["pending_handoff"],settled["interface_state"][provider.as_str()]["pending_handoff"]);
        scope["key"] = json!("wrong-correlation");
        assert_eq!(prepare(&scope,json!({"kind":"rearm"}),&current()).unwrap()["status"], "prepared");
        let ready = verify(&scope).unwrap();
        let (_,result) = b.effect(&transport_call(&root.join("wire"),&ready)).await;
        let mut wrong: Value = serde_json::from_slice(&result.read_payload_bytes().unwrap()).unwrap(); wrong["correlation_id"] = json!("not-the-proposed-write");
        let wrong = b.file(wrong["label"].as_str().unwrap(), &wrong); b.admit(&wrong,&door).await;
        assert_eq!(verify(&scope).unwrap()["status"], "awaiting_result");
        let call_count = b.log.read_from(Seq(0)).iter().filter(|row| row.kind == MovementKind::InterfaceOutbound && row.from == Endpoint::Interface(b.author.clone()) && row.to == Endpoint::Interface(door.clone())).count();
        assert_eq!(call_count,b.writes);
    }
}
