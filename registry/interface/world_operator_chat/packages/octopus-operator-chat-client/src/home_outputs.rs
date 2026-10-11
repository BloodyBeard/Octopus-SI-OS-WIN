use super::*;
use serde_json::{Value, json};

pub(super) fn interfaces(identity: &PlannedIdentity) -> Vec<String> {
    if identity.surface_role != "home" {
        return Vec::new();
    }
    vec![
        format!("system_worker_home_presentation_{}", identity.chat_id),
        format!("system_worker_home_voice_update_{}", identity.chat_id),
        format!("system_worker_home_voice_delegation_{}", identity.chat_id),
    ]
}

fn context_id(identity: &PlannedIdentity) -> String {
    format!("artifact-home-delivery-{}", identity.chat_id)
}

pub(super) fn template(
    identity: &PlannedIdentity,
    voice_update: bool,
) -> Result<BoundCheckpointTemplate, String> {
    let voice = identity
        .voice
        .as_ref()
        .ok_or("Home delivery requires its declared voice topology")?;
    let source = &interfaces(identity)[usize::from(voice_update)];
    let (label, recipient) = if voice_update {
        (
            "realtime.session.update",
            voice.context.provider_id.as_str(),
        )
    } else {
        (
            "client.surface.document",
            identity.chat_interface_id.as_str(),
        )
    };
    let binding = |id: &str| AuthoredInterfaceBinding {
        interface: InterfaceId::new(id),
        config: InterfaceBindingConfig {
            artifact_policy: BindingArtifactPolicy::Allow {
                names: if voice_update { vec![label.into()] } else { vec![label.into(), "client.action".into()] },
                namespaces: Vec::new(),
            },
        },
    };
    let mut result = bound_checkpoint_template(
        BoundCheckpointRole::IndependentInput,
        binding(source),
        if voice_update {
            vec![binding(&identity.model_interface_id)]
        } else {
            vec![binding(&identity.model_interface_id), binding(recipient)]
        },
        Vec::new(),
    )?;
    result.static_artifact_bindings =
        vec![octopus_descriptors::ArtifactId::new(context_id(identity))];
    Ok(result)
}

pub(super) fn plan(
    request: &OperatorChatPlanRequest,
    identity: &PlannedIdentity,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<Option<ArtifactBindingSeed>, String> {
    if identity.surface_role != "home" {
        return Ok(None);
    }
    let registry = &request.registry_root;
    let runs = request
        .run_records_root
        .as_ref()
        .filter(|path| path.is_absolute())
        .ok_or("Home context requires its explicit absolute run_records_root")?;
    let root = registry.join("interface");
    let source = load_interface_descriptor(&root, &identity.worker_choice_interface)?;
    let worker = writes
        .iter()
        .find(|w| w.tag == format!("policy:{}", identity.worker_interface_id))
        .ok_or("Home delivery has no concrete worker ceiling")?;
    let worker: Value = serde_json::from_str(&worker.content).map_err(|e| e.to_string())?;
    for id in interfaces(identity) {
        push_json_write(
            root.join(&id).join("interface.json"),
            sibling_descriptor_bytes(&source, &id, None, None)?,
            format!("interface:{id}"),
            writes,
        )?;
        push_json_write(
            root.join(&id).join("policy.json"),
            pretty_json(
                &json!({"id":id,"ceiling":{"interfaces":[],"tools":[],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[]},"role_artifacts":worker["role_artifacts"]}),
            )?,
            format!("policy:{id}"),
            writes,
        )?;
        plan_octopus_os_sidecar(
            &root,
            &id,
            identity,
            "artifact_return",
            false,
            None,
            false,
            writes,
        )?;
    }
    let voice = identity
        .voice
        .as_ref()
        .ok_or("Home delivery lost its voice declaration")?;
    let ids = interfaces(identity);
    let contract = json!({
        "widget":{"interface":ids[0],"label":"client.surface.document","recipient_interface":identity.chat_interface_id,"document_contract":"artifact-home-widget-authoring"},
        "client_action":{"interface":ids[0],"label":"client.action","recipient_interface":identity.chat_interface_id,"schema":"octopus.client_action","schema_version":1,"actions":["open_surface"]},
        "voice_update":{"run":voice.context.run_id,"interface":ids[1],"label":"realtime.session.update","recipient_interface":voice.context.provider_id,"payload":{"call_id":"copy from the admitted realtime.delegation artifact","correlation_id":"same admitted call correlation","update_id":"new stable identity for this one semantic update","delegation_id":"same admitted delegation or null","channel":"commentary","text":"Model-selected public progress or result, grounded in Task evidence"}},
        "run":identity.run_id,
        "authoring":{"registry_root":registry,"ee_instances_root":request.ee_instances_root,"run_records_root":runs,
            "run_file":runs.join(format!("{}.json",identity.run_id)),"worker_policy":registry.join("interface").join(&identity.worker_interface_id).join("policy.json"),
            "tools":identity.capability_tools}
    });
    let text = format!(
        "Exact Home artifact delivery addresses for this run:\n{}\n\nFor widgets and client navigation use the actual granted client.mcp tool and its publish_widget/open_client_surface catalogue operations with the stated System presentation interface. The MCP adapter uses artifact.send under the same grant. For voice updates use the actual granted artifact.send tool with the stated System interface, label, and payload. The runtime admits this artifact once and delivers it both to the listed recipient and to Home's exact text provider. Home re-arms its widget receipt. Voice updates reach separate Home and Voice checkpoints; each Run re-arms only its own input through its declared checked writer. A widget receipt is presentation evidence; it does not ask for or force an outward text reply. Decide whether any response is useful. Task workers can use the same return pins only after authoring those exact bindings, policy and readable role closure in their own runs. Never resend an admitted artifact to make it visible. Voice updates must copy call_id/correlation_id/delegation_id from actual admitted voice semantics, not private transport state or guessed values. A sent update is not speech-playback proof; inspect its realtime.session.updated evidence. Stopping voice does not cancel Tasks. Use artifact-home-widget-authoring for native/sandbox and persistent/transient document payloads.",
        pretty_json(&contract)?
    );
    let id = context_id(identity);
    push_json_write(
        registry.join("artifact").join(&id).join("artifact.json"),
        pretty_json(
            &json!({"id":id,"label":"context.reference","description":"Exact Home semantic artifact delivery addresses","text":text,"delivery":contract}),
        )?,
        format!("artifact:{id}"),
        writes,
    )?;
    Ok(Some(ArtifactBindingSeed { id }))
}

pub(super) fn wire(
    identity: &PlannedIdentity,
    roles: &[String],
    context: &[ArtifactBindingSeed],
    content: String,
) -> Result<String, String> {
    if identity.surface_role != "home" {
        return Ok(content);
    }
    let mut record: Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    for is_voice in [false, true] {
        let mut template = template(identity, is_voice)?;
        for binding in context {
            let id = octopus_descriptors::ArtifactId::new(&binding.id);
            if !roles.contains(&binding.id) && !template.static_artifact_bindings.contains(&id) {
                template.static_artifact_bindings.push(id);
            }
        }
        let mut artifacts = roles.iter().map(|id| json!({"id":id})).collect::<Vec<_>>();
        for id in &template.static_artifact_bindings {
            if !artifacts.iter().any(|binding| binding["id"] == id.as_str()) {
                artifacts.push(json!({"id":id}));
            }
        }
        let rows = record["checkpoints"]
            .as_array_mut()
            .ok_or("Home has no checkpoints")?;
        let matching = rows
            .iter()
            .filter(|cp| {
                cp["interface_binding"] == json!(template.interface_binding)
                    && cp["outbound_interface_bindings"]
                        == json!(template.outbound_interface_bindings)
            })
            .map(|cp| cp["id"].clone())
            .collect::<Vec<_>>();
        if matching.len() > 1 {
            return Err("Home delivery has duplicate exact receiving checkpoints".into());
        }
        if let Some(id) = matching.first() {
            let row = rows
                .iter_mut()
                .find(|cp| cp["id"] == *id)
                .ok_or("Home delivery row disappeared")?;
            row["artifact_bindings"] = json!(artifacts);
        }
        if matching.is_empty() {
            let id = format!("cp_home_delivery_{}", template.name);
            rows.push(json!({"id":id,"run":identity.run_id,"trace":format!("trace:{}:{id}",identity.run_id),"epoch":0,"interface_binding":template.interface_binding,"outbound_interface_bindings":template.outbound_interface_bindings,"artifact_bindings":artifacts,"required_verified_artifacts":[]}));
            record["active_checkpoints"]
                .as_array_mut()
                .ok_or("Home has no active checkpoints")?
                .push(json!(id));
        }
        let templates=record["interface_state"][&identity.model_interface_id]["run_authoring_binding"]["checkpoints"].as_array_mut().ok_or("Home text provider has no authoring topology")?;
        if templates.iter().any(|cp| cp["name"] == template.name) {
            return Err(
                "Home delivery template already exists in the newly authored provider topology"
                    .into(),
            );
        }
        templates.push(json!(template));
    }
    record["interface_state"][&identity.chat_interface_id]["delivery_artifact"] =
        json!(context_id(identity));
    pretty_json(&record)
}
