use super::*;

#[test]
fn materialized_checkpoints_preserve_static_role_overlap_access() {
    let fixture = CompletedInputReplayFixture::new();
    let mut template = fixture.handoff.config.checkpoints[0].clone();
    template.static_artifact_bindings = vec![ArtifactId::new("role"), ArtifactId::new("declared-static")];
    for elevated in [false, true] {
        let mut role = ArtifactBinding { id: ArtifactId::new("role"), access: Default::default() };
        role.access.change = elevated; role.access.create = elevated; role.access.delete = elevated;
        let raw = serde_json::to_value(&role).unwrap();
        let value = materialize_checkpoint(&template, "context", &fixture.handoff.config.run, &[raw.clone()]);
        assert_eq!(value["artifact_bindings"], serde_json::json!([raw, {"id":"declared-static"}]));
        let checkpoint = serde_json::from_value(value).unwrap();
        assert!(checkpoint_role_bindings_match(&checkpoint, &[role], &template));
        let duplicates = materialize_checkpoint(&template, "context", &fixture.handoff.config.run, &[raw.clone(), raw]);
        assert_eq!(duplicates["artifact_bindings"].as_array().unwrap().len(), 3, "existing duplicate input must remain invalid, not be silently repaired");
    }
}

struct CompletedInputReplayFixture {
    _root: tempfile::TempDir,
    handoff: ProviderOutputHandoff,
    rows: Vec<Movement>,
    input: ArtifactRef,
    input_file: InterfaceArtifactFile,
}

impl CompletedInputReplayFixture {
    fn new() -> Self {
        use serde_json::json;
        let root = tempfile::tempdir().unwrap();
        let binding = |id: &str| json!(AuthoredInterfaceBinding::all(InterfaceId::new(id)));
        let row = |name: &str, role: &str, source: &str, target: &str, tool: Option<&str>| json!({
            "name":name,"role":role,"interface_binding":binding(source),"outbound_interface_bindings":[binding(target)],
            "include_context":role == "stable","required_verified_artifacts":tool.map(|tool| vec![json!({
                "mark":"tool.call.allowed","artifact_label":"tool.call","verifier":"tool_call_allowed","params":{"tools":[tool]}
            })]).unwrap_or_default()
        });
        let config: RunAuthoringConfig = serde_json::from_value(json!({
            "run":"run","worker":"worker","provider_interface":"provider","authoring_source_interface":"writer",
            "file_write_tool":"write","file_write_label":"tool.call","file_write_result_label":"write.result","result_timeout_ms":1000,
            "checkpoints":[row("input","stable","room","provider",None),row("alternate","stable","alternate_room","provider",None),
                row("control","independent_input","control","provider",None),
                row("client","provider_artifact_output","provider","client",None),
                row("tool","provider_tool_output","provider","room",Some("read")),
                row("writer_call","authoring_call","writer","writer_room",Some("write")),
                row("writer_result","authoring_result","writer_room","provider",None)]
        })).unwrap();
        let topology = BoundRunAuthoringTopology { schema_version:2,run:config.run.clone(),worker:config.worker.clone(),
            provider_interface:config.provider_interface.clone(),authoring_source_interface:config.authoring_source_interface.clone(),
            payload_artifact_bindings:Vec::new(),writer_run:None,checkpoints:config.checkpoints.clone() };
        let initial = serde_json::to_vec(&json!({"id":"run","ee":"ee","worker":"worker","trace":"run","status":"running",
            "checkpoints":config.checkpoints.iter().map(|cp|materialize_checkpoint(cp,&cp.name,&config.run,&[])).collect::<Vec<_>>(),
            "active_checkpoints":config.checkpoints.iter().map(|cp|cp.name.clone()).collect::<Vec<_>>(),
            "interface_state":{"provider":{"context_generation":0,"authoring_generation":0,
                "frontier_checkpoint_ids":["client","tool","writer_call","writer_result"],"run_authoring_binding":topology}}
        })).unwrap();
        let (arrivals, _) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(config.provider_interface.clone(),root.path().join("interface"),None,
            root.path().join("var"),root.path().join("movements.jsonl"),root.path().join("blobs"),root.path().join("index.sqlite"),
            BTreeMap::new(),arrivals,Default::default()).with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits {
                max_control_frame_bytes:1_048_576,max_stderr_bytes:262_144,max_manifest_bytes:1_048_576,max_payload_bytes:16_777_216,
            }).unwrap();
        std::fs::create_dir_all(&ctx.var_dir).unwrap();
        std::fs::create_dir_all(&ctx.blobs_dir).unwrap();
        std::fs::write(&ctx.movements_path, b"").unwrap();
        let run_path = root.path().join("run.json");
        std::fs::write(&run_path,&initial).unwrap();
        let result_reader = LedgerTailReader::open_named(&ctx,"completed-input-test").unwrap();
        let handoff = ProviderOutputHandoff {
            session:ProviderHandoffSession::new(&ctx).unwrap(),var_dir:ctx.var_dir.clone(),ctx,
            config,run_path,hand_in_root:root.path().join("hand-in"),source_movement_seq:0,source_carries_context:true,failed_source:false,
            context_output_labels:BTreeSet::from(["tool.call".into()]),session_snapshot_labels:BTreeSet::new(),
            granted_tools:BTreeSet::from(["read".into(),"write".into()]),granted_interfaces:BTreeSet::new(),granted_mark_outcomes:BTreeSet::new(),
            bound_topology:topology,delivery_mode:HandoffDeliveryMode::Rearm,invocation_session_snapshot:None,
            result_reader:Arc::new(tokio::sync::Mutex::new(result_reader)),
            state:Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
                context_artifacts:Vec::new(),static_context_artifacts:Vec::new(),session_context:ResolvedSessionContext {
                    provider_interface:"provider".into(),artifacts:Vec::new(),
                },session_snapshot_head:None,run_record_bytes:initial.clone(),run_record_evidence_seq:Some(0),
                consumed_input:true,consumed_context_free_independent_input:None,consumed_verified_input:false,
            })),
        };
        let file = InterfaceArtifactFile::write(handoff.var_dir.join("input"),"tool.result",
            br#"{"label":"tool.result","tool":"read","correlation_id":"first-call","ok":true,"body":{"value":1}}"#).unwrap();
        let input = artifact_identity(&file,&handoff.var_dir).unwrap().artifact;
        let mut fixture = Self { _root:root,handoff,rows:Vec::new(),input,input_file:file.clone() };
        fixture.edge(&initial,&"input".into(),&file,"provider".into(),MovementKind::InterfaceOutbound);
        fixture.edge(&initial,&"alternate".into(),&file,"provider".into(),MovementKind::InterfaceOutbound);
        append_session_payload(&mut fixture.handoff.state.try_lock().unwrap().session_context,"tool.result",
            &file.read_payload_bytes().unwrap(),&fixture.input,Some(0),"prior exact input").unwrap();
        fixture
    }

    fn save(&self) {
        std::fs::write(&self.handoff.ctx.movements_path,self.rows.iter()
            .map(|row|format!("{}\n",serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
    }

    fn edge(&mut self, bytes: &[u8], checkpoint: &CheckpointId, file: &InterfaceArtifactFile, target: InterfaceId, kind: MovementKind) {
        use octopus_interfaces::{GrantProjection, RuntimeInvocationMetadata};
        let h = &self.handoff;
        let record = RunRecord::from_bytes(bytes).unwrap();
        let cp = record.checkpoint(checkpoint).unwrap();
        let hash = ContentHash::of_bytes(bytes).to_hex();
        std::fs::write(h.ctx.blobs_dir.join(&hash),bytes).unwrap();
        let artifact = artifact_identity(file,file.manifest_path().parent().unwrap()).unwrap().artifact;
        let manifest = file.read_manifest().unwrap();
        std::fs::write(h.ctx.blobs_dir.join(artifact.content.to_hex()),std::fs::read(file.manifest_path()).unwrap()).unwrap();
        std::fs::write(h.ctx.blobs_dir.join(manifest.payload_content_hash.unwrap()),file.read_payload_bytes().unwrap()).unwrap();
        let mut metadata = InvocationMetadata::new(GrantProjection {
            tools:vec![h.config.file_write_tool.clone()],interfaces:vec![target.to_string()],..Default::default()
        }).with_runtime(RuntimeInvocationMetadata {
            run:record.id.clone(),ee:record.ee.clone(),worker:record.worker.clone(),checkpoint:checkpoint.clone(),
            interface:if kind == MovementKind::InterfaceArrival {h.ctx.interface.clone()} else {target.clone()},
            epoch:cp.epoch.0,run_record_hash:Some(hash),
        }).with_outbound_interface_bindings(cp.outbound_interface_bindings.iter().map(|b|b.interface.clone()).collect())
            .with_checkpoint_binding_policy(CheckpointBindingPolicySnapshot {inbound:cp.interface_binding.clone().unwrap(),outbound:cp.outbound_interface_bindings.clone()})
            .with_required_verified_artifacts(cp.required_verified_artifacts.clone())
            .with_checkpoint_admission(octopus_ledger::CheckpointAdmissionMetadata {artifact_label:manifest.label,
                required_artifact_labels:cp.required_verified_artifacts.iter().map(|r|r.artifact_label.clone()).collect(),complete:true});
        metadata.artifact_binding_refs = cp.artifact_bindings.iter().map(|b|ArtifactRef {id:b.id.clone(),
            content:ContentHash::from_hex(b.id.as_str().rsplit(':').next().unwrap()).unwrap()}).collect();
        let seq = self.rows.len() as u64;
        self.rows.push(Movement {seq:octopus_ledger::Seq(seq),lamport:octopus_ledger::Lamport(seq),recorded_at_unix_ms:seq,
            trace:octopus_ledger::TraceId("run".into()),kind,from:Endpoint::Interface(cp.interface_binding.as_ref().unwrap().interface.clone()),
            to:Endpoint::Interface(target),artifact:Some(artifact),invocation_metadata:Some(metadata.into())});
        self.save();
    }

    fn install(&mut self, authored: &AuthoredRun) {
        let before = std::fs::read(&self.handoff.run_path).unwrap();
        let record = RunRecord::from_bytes(&before).unwrap();
        let cp = active_checkpoint_for_role(&record,&self.handoff.config,RunAuthoringCheckpointRole::AuthoringCall).unwrap().id.clone();
        let call = self.handoff.materialize_file_write_call(authored).unwrap();
        self.edge(&before,&cp,&call,authored.authoring_result_interface.clone(),MovementKind::InterfaceOutbound);
        std::fs::write(&self.handoff.run_path,&authored.content).unwrap();
        let result = InterfaceArtifactFile::write(self.handoff.var_dir.join("results"),"write.result",
            &serde_json::to_vec(&serde_json::json!({"label":"write.result","tool":"write","correlation_id":authored.correlation_id,"ok":true,
                "body":{"result":"files_written","writes":[{"path":self.handoff.run_path,"before_hash":authored.before_hash,
                    "after_hash":authored.after_hash,"bytes":authored.content.len()}]}})).unwrap()).unwrap();
        self.edge(authored.content.as_bytes(),&authored.authoring_result_checkpoint,&result,"provider".into(),MovementKind::InterfaceOutbound);
    }

    fn output(&mut self, terminal: bool, admit: bool, mark: bool) -> AuthoredRun {
        let before = std::fs::read(&self.handoff.run_path).unwrap();
        let record = RunRecord::from_bytes(&before).unwrap();
        let file = InterfaceArtifactFile::write(self.handoff.var_dir.join("outputs"),"tool.call",
            &serde_json::to_vec(&serde_json::json!({"label":"tool.call","tool":"read","arguments":{},"correlation_id":format!("next-{}",self.rows.len())})).unwrap()).unwrap();
        let identity = artifact_identity(&file,&self.handoff.var_dir).unwrap();
        let mut context = current_context_artifacts(&record,&self.handoff.config).unwrap();
        append_artifact(&mut context,self.input.id.clone());
        append_artifact(&mut context,identity.artifact.id.clone());
        let authored = self.handoff.author_run_with_bindings(&before,&context,&[PreparedHandoff {
            identity,output_checkpoint:self.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderToolOutput).unwrap().clone(),verified_inputs:Vec::new(),
        }],true,None,false,terminal,&[]).unwrap();
        self.install(&authored);
        if admit {
            self.edge(authored.content.as_bytes(),&authored.pending_handoff.items[0].checkpoint.id,&file,"worker".into(),MovementKind::InterfaceArrival);
            if mark { self.mark(&authored); }
        }
        authored
    }

    fn mark(&mut self, authored: &AuthoredRun) {
        let item = &authored.pending_handoff.items[0];
        let (_, artifact) = load_pending_artifact(&self.handoff.var_dir,item).unwrap();
        let requirement = &item.checkpoint.required_verified_artifacts[0];
        let seq = self.rows.len() as u64;
        let mark = octopus_interfaces::Mark::passed(requirement.mark.clone(),item.checkpoint.id.clone(),self.handoff.config.run.clone(),
            item.checkpoint.epoch,artifact.clone(),requirement.verifier.clone(),octopus_ledger::VerifierPackagePin {
                descriptor_content:ContentHash::of_bytes(b"descriptor"),script_content:ContentHash::of_bytes(b"script"),
            },serde_json::json!({}));
        self.rows.push(Movement {seq:octopus_ledger::Seq(seq),lamport:octopus_ledger::Lamport(seq),recorded_at_unix_ms:seq,
            trace:octopus_ledger::TraceId("run".into()),kind:MovementKind::MarkOutcomeAdmitted(requirement.mark.clone()),
            from:Endpoint::Interface("provider".into()),to:Endpoint::Interface("worker".into()),artifact:Some(artifact),invocation_metadata:Some(mark.into())});
        self.save();
    }

    fn publish(&mut self, authored: &AuthoredRun) {
        let publication = self.handoff.author_context_publication(authored.content.as_bytes(),&authored.pending_handoff).unwrap();
        self.install(&publication);
    }
}

#[tokio::test]
async fn completed_source_settlement_requires_exact_predecessor_and_replays_once() {
    for mode in ["valid", "wrong_predecessor", "nonterminal", "wrong_source", "effect", "wrong_generation", "unadmitted"] {
        let mut fixture = CompletedInputReplayFixture::new();
        let first = fixture.output(mode != "nonterminal", mode != "unadmitted", true);
        if mode != "unadmitted" { fixture.publish(&first); }
        let before = std::fs::read(&fixture.handoff.run_path).unwrap();
        let record = RunRecord::from_bytes(&before).unwrap();
        let mut predecessor = load_pending_artifact(&fixture.handoff.var_dir, &first.pending_handoff.items[0]).unwrap().1;
        if mode == "wrong_predecessor" { predecessor.content = ContentHash::of_bytes(b"different receipt"); }
        if mode == "wrong_source" { fixture.handoff.source_movement_seq = 1; }
        let file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("settlement"), if mode == "effect" { "tool.call" } else { "context.snapshot" },
            br#"{"label":"context.snapshot","receipt":"settled"}"#).unwrap();
        let identity = artifact_identity(&file, &fixture.handoff.var_dir).unwrap();
        let mut context = current_context_artifacts(&record, &fixture.handoff.config).unwrap();
        context.retain(|id| id != &first.pending_handoff.items[0].artifact_id);
        append_artifact(&mut context, identity.artifact.id.clone());
        let mut authored = fixture.handoff.author_run_with_bindings(&before, &context, &[PreparedHandoff {
            identity, output_checkpoint:fixture.handoff.config.checkpoint_for_role(if mode == "effect" {
                RunAuthoringCheckpointRole::ProviderToolOutput
            } else { RunAuthoringCheckpointRole::ProviderArtifactOutput }).unwrap().clone(), verified_inputs:Vec::new(),
        }], mode != "wrong_generation", None, false, true, &[]).unwrap();
        let result = fixture.handoff.attach_source_settlement(&before, &mut authored, &predecessor,
            optional_u64(&record.interface_state[&fixture.handoff.config.provider_interface], "context_generation").unwrap());
        if mode != "valid" { assert!(result.is_err(), "{mode}"); continue; }
        result.unwrap();
        let mut missing = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
        missing.interface_state.get_mut(&fixture.handoff.config.provider_interface).unwrap().get_mut("pending_handoff").unwrap()["settlement_of"]["content"] = serde_json::json!(ContentHash::of_bytes(b"wrong"));
        assert!(source_settlement::validate_transition(&record, &missing, &fixture.handoff.config).is_err());
        let mut widened = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
        let cp = widened.checkpoints.iter_mut().find(|cp|cp.inbound_interface()==Some(&InterfaceId::new("room"))).unwrap();
        cp.artifact_bindings[0].access.change = true;
        assert!(source_settlement::validate_transition(&record, &widened, &fixture.handoff.config).is_err());
        fixture.install(&authored);
        fixture.edge(authored.content.as_bytes(), &authored.pending_handoff.items[0].checkpoint.id, &file, "worker".into(), MovementKind::InterfaceArrival);
        let publication = fixture.handoff.author_context_publication(authored.content.as_bytes(), &authored.pending_handoff).unwrap();
        let installed = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
        let mut changed = RunRecord::from_bytes(publication.content.as_bytes()).unwrap();
        changed.checkpoints.iter_mut().find(|cp|cp.inbound_interface()==Some(&InterfaceId::new("control"))).unwrap().epoch.0 += 1;
        assert!(source_settlement::validate_transition(&installed, &changed, &fixture.handoff.config).is_err());
        let mut changed = RunRecord::from_bytes(publication.content.as_bytes()).unwrap();
        changed.interface_state.get_mut(&fixture.handoff.config.provider_interface).unwrap().get_mut("pending_handoff").unwrap()["source_without_context"] = serde_json::json!(true);
        assert!(source_settlement::validate_transition(&installed, &changed, &fixture.handoff.config).is_err());
        fixture.publish(&authored);
        fixture.handoff.source_movement_seq = 1;
        fixture.handoff.state.lock().await.run_record_bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
        fixture.handoff.session = ProviderHandoffSession::new(&fixture.handoff.ctx).unwrap();
        assert_eq!(fixture.handoff.completed_input_artifact_source(&fixture.input).await.unwrap(), Some(0));
        let bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
        let ledger = std::fs::read(&fixture.handoff.ctx.movements_path).unwrap();
        assert_eq!(fixture.handoff.completed_input_artifact_source(&fixture.input).await.unwrap(), Some(0));
        assert_eq!(std::fs::read(&fixture.handoff.run_path).unwrap(), bytes);
        assert_eq!(std::fs::read(&fixture.handoff.ctx.movements_path).unwrap(), ledger);
    }
}

#[test]
fn payload_readiness_proves_completed_newer_independent_inputs() {
    for mode in ["completed", "unprocessed", "nonterminal", "unadmitted", "unpublished", "late_admission", "late_mark", "wrong_artifact", "extra_input", "changed_current"] {
        let mut fixture = CompletedInputReplayFixture::new();
        fixture.rows.pop(); fixture.save();
        let initial = std::fs::read(&fixture.handoff.run_path).unwrap();
        let control = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("independent"), "arbitrary.independent.input", br#"{"value":1}"#).unwrap();
        fixture.edge(&initial,&CheckpointId::new("control"),&control,"provider".into(),MovementKind::InterfaceOutbound);
        fixture.handoff.source_movement_seq = 1;
        let independent = if mode == "unprocessed" { None } else {
            let output = fixture.output(mode != "nonterminal", !["unadmitted","late_admission"].contains(&mode), mode != "late_mark");
            if mode != "unpublished" { fixture.publish(&output); }
            Some(output)
        };
        fixture.handoff.source_movement_seq = 0;
        let last = fixture.output(true,true,true); fixture.publish(&last);
        if mode == "late_admission" {
            let output = independent.as_ref().unwrap();
            let file = load_pending_artifact(&fixture.handoff.var_dir,&output.pending_handoff.items[0]).unwrap().0;
            fixture.edge(output.content.as_bytes(),&output.pending_handoff.items[0].checkpoint.id,&file,"worker".into(),MovementKind::InterfaceArrival);
            fixture.mark(output);
        }
        if mode == "late_mark" { fixture.mark(independent.as_ref().unwrap()); }
        if mode == "wrong_artifact" {
            let id = &independent.as_ref().unwrap().pending_handoff.items[0].artifact_id;
            fixture.rows.iter_mut().find(|row|row.kind==MovementKind::InterfaceArrival && row.artifact.as_ref().is_some_and(|reference|&reference.id==id)).unwrap().artifact = Some(fixture.input.clone());
            fixture.save();
        }
        let mut current = std::fs::read(&fixture.handoff.run_path).unwrap();
        if mode == "extra_input" {
            let record = RunRecord::from_bytes(&current).unwrap();
            let cp = record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id) && cp.inbound_interface()==Some(&InterfaceId::new("room"))).unwrap().id.clone();
            fixture.edge(&current,&cp,&fixture.input_file.clone(),"provider".into(),MovementKind::InterfaceOutbound);
        }
        if mode == "changed_current" {
            let mut value:serde_json::Value = serde_json::from_slice(&current).unwrap();
            value["interface_state"]["provider"]["context_generation"] = serde_json::json!(999);
            current = serde_json::to_vec(&value).unwrap();
        }
        let h = &fixture.handoff;
        let writer = CheckedRunWriter {owner:h.config.provider_interface.clone(),source:h.config.authoring_source_interface.clone(),
            door:h.config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult).unwrap().interface().clone(),
            tool:h.config.file_write_tool.clone(),call_label:h.config.file_write_label.clone(),result_label:h.config.file_write_result_label.clone()};
        let evidence = CheckedRunEvidence {movements_path:&h.ctx.movements_path,ledger_index_path:&h.ctx.ledger_index_path,
            blobs_dir:&h.ctx.blobs_dir,max_control_frame_bytes:1_048_576,max_manifest_bytes:1_048_576,max_payload_bytes:16_777_216};
        let cutoff = fixture.rows.len() as u64;
        let file = provider_input_payloads_ready(&evidence,&h.run_path,&current,&writer,&InterfaceId::new("room"));
        let mut reader = FileCheckedRunReader::open(&evidence,None).unwrap();
        let generic = input_revision::reader_payload_readiness(&mut reader,&h.run_path,&current,&writer,&InterfaceId::new("room"),cutoff);
        assert_eq!(file.as_ref().ok(),generic.as_ref().ok(),"{mode}: file={file:?}, generic={generic:?}");
        assert_eq!(file.as_ref().is_ok_and(|ready|*ready),mode=="completed","{mode}: {file:?}");
        let selection = InterfaceArtifactFile::write(h.var_dir.join("selection"),"context.reference",br#"{"value":"selected"}"#).unwrap();
        let selected = prepare_provider_input_payloads(h.run_path.clone(),&current,&writer,InterfaceId::new("room"),
            vec![artifact_identity(&selection,&h.var_dir).unwrap().artifact]).unwrap();
        let mut call = fixture.rows.last().unwrap().clone(); call.seq = octopus_ledger::Seq(cutoff);
        let transition = CheckedRunTransition {writer:writer.clone(),before:current.clone(),after:selected.content.into_bytes(),call:call.clone(),result:call};
        let mut reader = FileCheckedRunReader::open(&evidence,None).unwrap();
        assert_eq!(input_revision::validate_rearm_evidence(&mut reader,&transition,&h.run_path).is_ok(),mode=="completed","historical {mode}");
        if mode == "completed" {
            let mut reader = FileCheckedRunReader::open(&evidence,None).unwrap();
            assert!(!input_revision::reader_payload_readiness(&mut reader,&h.run_path,&current,&writer,&InterfaceId::new("room"),1).unwrap());
        }
    }
}

#[test]
#[ignore = "requires an explicitly selected stopped Home after exact source settlement"]
fn selected_home_payload_readiness_proves_consumed_independent_input() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_CLIENT").unwrap());
    let path = home.join("state/runs").join(format!("{run}.json"));
    let bytes = std::fs::read(&path).unwrap();let record = RunRecord::from_bytes(&bytes).unwrap();
    let descriptor:InterfaceDescriptor = serde_json::from_slice(&std::fs::read(home.join("registry/interface").join(provider.as_str()).join("interface.json")).unwrap()).unwrap();
    let writer = declared_provider_run_writer(&record,&provider,&run_authoring_profile(&descriptor).unwrap()).unwrap();
    let movements = home.join("ledger/movements.jsonl");let ledger = std::fs::read(&movements).unwrap();
    let blobs = home.join("ledger/blobs");let scratch = tempfile::tempdir().unwrap();let index = scratch.path().join("ledger-index.sqlite");
    let scratch_ledger = scratch.path().join("movements.jsonl");std::fs::write(&scratch_ledger,&ledger).unwrap();
    std::os::unix::fs::symlink(&blobs,scratch.path().join("blobs")).unwrap();
    std::os::unix::fs::symlink(home.join("ledger/metadata"),scratch.path().join("metadata")).unwrap();
    let log = octopus_ledger_store::DurableLog::open(&scratch_ledger).unwrap();
    let _index = octopus_ledger_index::LedgerSqliteIndex::open_healthy(&index,&log).unwrap();
    let evidence = CheckedRunEvidence {movements_path:&movements,ledger_index_path:&index,blobs_dir:&blobs,
        max_control_frame_bytes:1_048_576,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456};
    assert!(provider_input_payloads_ready(&evidence,&path,&bytes,&writer,&source).unwrap());
    eprintln!("selected Home: direct payload readiness passed against the exact current record");
    let mut reader = FileCheckedRunReader::open(&evidence,None).unwrap();
    let cutoff = ledger.split(|byte|*byte==b'\n').filter(|row|!row.is_empty()).count() as u64;
    assert!(input_revision::reader_payload_readiness(&mut reader,&path,&bytes,&writer,&source,cutoff).unwrap());
    assert_eq!(std::fs::read(path).unwrap(),bytes);assert_eq!(std::fs::read(movements).unwrap(),ledger);
    eprintln!("selected Home: both current and historical readiness prove newer independent completion before {cutoff}");
}

#[tokio::test]
async fn completed_input_artifact_proves_exact_terminal_history_without_replaying() {
    for mode in ["current", "later_normal", "no_context", "current_context", "multiple_prior", "nonterminal", "unadmitted", "unpublished", "late_mark",
        "repeated_terminal", "wrong_source", "wrong_hash", "wrong_current_source", "wrong_current_hash",
        "wrong_worker", "wrong_run", "wrong_provider", "unadmitted_input", "changed_record"] {
        let mut fixture = CompletedInputReplayFixture::new();
        let first = fixture.output(mode != "nonterminal", mode != "unadmitted", mode != "late_mark");
        if mode != "unpublished" { fixture.publish(&first); }
        if mode == "late_mark" { fixture.mark(&first); }
        if mode == "later_normal" || mode == "repeated_terminal" {
            let bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
            let record = RunRecord::from_bytes(&bytes).unwrap();
            let cp = record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.inbound_interface() == Some(&InterfaceId::new("room"))).unwrap().id.clone();
            let next_input = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("next-input"),"tool.result",
                br#"{"label":"tool.result","tool":"read","correlation_id":"next-call","ok":true,"body":{"value":2}}"#).unwrap();
            fixture.handoff.source_movement_seq = fixture.rows.len() as u64;
            fixture.edge(&bytes,&cp,&next_input,"provider".into(),MovementKind::InterfaceOutbound);
            let later = fixture.output(true,true,true);
            fixture.publish(&later);
            if mode == "repeated_terminal" {
                fixture.handoff.source_movement_seq = 0;
                let repeated = fixture.output(true,true,true);
                fixture.publish(&repeated);
            }
        }
        if ["wrong_worker", "wrong_run", "unadmitted_input"].contains(&mode) {
            let mut metadata = fixture.rows[0].invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
            match mode {
                "wrong_worker" => metadata.runtime.as_mut().unwrap().worker = "other-worker".into(),
                "wrong_run" => metadata.runtime.as_mut().unwrap().run = "other-run".into(),
                _ => metadata.checkpoint_admission = None,
            }
            fixture.rows[0].invocation_metadata = Some(metadata.into()); fixture.save();
        }
        if mode == "wrong_source" {
            fixture.rows[0].artifact = fixture.rows[2].artifact.clone(); fixture.save();
        }
        if mode == "wrong_hash" {
            fixture.rows[0].artifact.as_mut().unwrap().content = ContentHash::of_bytes(b"other source bytes"); fixture.save();
        }
        if mode == "wrong_current_source" {
            fixture.rows[1].artifact = fixture.rows[2].artifact.clone(); fixture.save();
        }
        if mode == "wrong_current_hash" {
            fixture.rows[1].artifact.as_mut().unwrap().content = ContentHash::of_bytes(b"other current bytes"); fixture.save();
        }
        if mode == "wrong_provider" {
            fixture.rows[0].to = Endpoint::Interface("other-provider".into()); fixture.save();
        }
        if mode == "changed_record" {
            let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&fixture.handoff.run_path).unwrap()).unwrap();
            value["status"] = serde_json::json!("unproved");
            std::fs::write(&fixture.handoff.run_path,serde_json::to_vec(&value).unwrap()).unwrap();
        }
        fixture.handoff.source_movement_seq = 1;
        if mode == "multiple_prior" {
            let bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
            let record = RunRecord::from_bytes(&bytes).unwrap();
            let cp = record.checkpoints.iter().find(|cp|record.active_checkpoints.contains(&cp.id)
                && cp.inbound_interface() == Some(&InterfaceId::new("alternate_room"))).unwrap().id.clone();
            fixture.handoff.source_movement_seq = fixture.rows.len() as u64;
            fixture.edge(&bytes,&cp,&fixture.input_file.clone(),"provider".into(),MovementKind::InterfaceOutbound);
        }
        {
            let mut state = fixture.handoff.state.lock().await;
            if mode == "no_context" { state.session_context.artifacts.clear(); }
            if mode == "current_context" || mode == "multiple_prior" {
                state.session_context.artifacts[0].source_movement_seq = Some(fixture.handoff.source_movement_seq);
            }
        }
        fixture.handoff.session = ProviderHandoffSession::new(&fixture.handoff.ctx).unwrap();
        let bytes = std::fs::read(&fixture.handoff.run_path).unwrap();
        fixture.handoff.state.lock().await.run_record_bytes = bytes.clone();
        let ledger = std::fs::read(&fixture.handoff.ctx.movements_path).unwrap();
        let context = serde_json::to_value(fixture.handoff.current_session_context_artifacts().await.as_ref()).unwrap();
        let result = fixture.handoff.completed_input_artifact_source(&fixture.input).await;
        if ["current", "later_normal", "no_context", "current_context", "multiple_prior"].contains(&mode) {
            assert_eq!(result.unwrap(),Some(0),"{mode}");
            let mut changed = fixture.input.clone(); changed.content = ContentHash::of_bytes(b"different artifact");
            assert_eq!(fixture.handoff.completed_input_artifact_source(&changed).await.unwrap(),None);
            let mut current = fixture.handoff.clone(); current.source_movement_seq = 0;
            assert_eq!(current.completed_input_artifact_source(&fixture.input).await.unwrap(),None);
        } else if ["wrong_source", "wrong_hash", "wrong_run", "wrong_provider", "unadmitted_input"].contains(&mode) {
            assert_eq!(result.unwrap(),None,"{mode}");
        } else {
            assert!(result.is_err(),"{mode} accepted {result:?}");
        }
        assert_eq!(std::fs::read(&fixture.handoff.run_path).unwrap(),bytes,"{mode}");
        assert_eq!(std::fs::read(&fixture.handoff.ctx.movements_path).unwrap(),ledger,"{mode}");
        assert_eq!(fixture.handoff.state.lock().await.run_record_bytes,bytes,"{mode}");
        assert_eq!(serde_json::to_value(fixture.handoff.current_session_context_artifacts().await.as_ref()).unwrap(),context,"{mode}");
    }
}

#[test]
#[ignore = "requires explicitly selected read-only installation evidence"]
fn selected_home_exact_checked_install() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let call_seq = std::env::var("OCTOPUS_REPLAY_CALL").unwrap().parse::<u64>().unwrap();
    let result_seq = std::env::var("OCTOPUS_REPLAY_RESULT").unwrap().parse::<u64>().unwrap();
    let movements = home.join("ledger/movements.jsonl");
    let blobs = home.join("ledger/blobs");
    let scratch = tempfile::tempdir().unwrap();
    let index = scratch.path().join("index.sqlite");
    let evidence = CheckedRunEvidence { movements_path: &movements, ledger_index_path: &index,
        blobs_dir: &blobs, max_control_frame_bytes: 1_048_576,
        max_manifest_bytes: 1_048_576, max_payload_bytes: 268_435_456 };
    let mut reader = FileCheckedRunReader::open(&evidence, Some(call_seq - 1)).unwrap();
    let call = reader.admitted_movement(call_seq).unwrap().unwrap();
    let result = reader.admitted_movement(result_seq).unwrap().unwrap();
    let (manifest, bytes) = reader.artifact(call.artifact.as_ref().unwrap()).unwrap();
    let (result_manifest, _) = reader.artifact(result.artifact.as_ref().unwrap()).unwrap();
    let payload: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&bytes).unwrap();
    let runtime = call.runtime().unwrap();
    let declaration = reader.snapshot(&ContentHash::from_hex(runtime.run_record_hash.as_ref().unwrap()).unwrap()).unwrap();
    let Endpoint::Interface(source) = &call.from;
    let Endpoint::Interface(door) = &call.to;
    let Endpoint::Interface(owner) = &result.to;
    let writer = CheckedRunWriter { owner:owner.clone(), source:source.clone(), door:door.clone(),
        tool:payload.tool, call_label:manifest.label, result_label:result_manifest.label };
    let path = home.join("state/runs").join(format!("{}.json", runtime.run));
    let proof = CheckedRunInstall { call: ConsentEvidenceRef { movement_seq:call_seq, artifact:call.artifact.clone().unwrap() },
        result: ConsentEvidenceRef { movement_seq:result_seq, artifact:result.artifact.clone().unwrap() } };
    let checked = checked_declared_run_install(&evidence, &path, &declaration, &proof, vec![writer]).unwrap();
    assert_eq!(checked.call, call);
    assert_eq!(checked.result, result);
    assert_eq!(ContentHash::of_bytes(&checked.before).to_hex(), payload.arguments["expected_before_hash"]);
    assert_eq!(ContentHash::of_bytes(&checked.after).to_hex(), payload.arguments["expected_after_hash"]);
}

#[test]
#[ignore = "requires an explicitly selected stopped Home and its actual prepared settlement receipt"]
fn selected_home_materializes_exact_settlement_checkpoint_contract() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
    let life: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("life.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1); drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(), dir, Some(descriptor.clone()),
        home.join("interfaces").join(provider.as_str()), home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        scratch.path().join("unused-index.sqlite"), BTreeMap::from([
            ("run_records_root".into(), home.join("state/runs")), ("interface_hand_in_root".into(), home.join("interface-hand-in")),
        ]), arrivals, Default::default()).with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
    let path = home.join("state/runs").join(format!("{run}.json"));
    let before = std::fs::read(&path).unwrap(); let ledger = std::fs::read(&ctx.movements_path).unwrap();
    let cursors = ["ledger-cursor.json", "ledger-cursor.turn-control.json"].map(|name| {
        let path = ctx.interface_var_dir().join(name); let bytes = std::fs::read(&path).unwrap(); (path, bytes)
    });
    let mut replay = octopus_world_pyecho_life_runtime::LedgerReplayReader::open(&ctx).unwrap();
    let mut input = None;
    while let Some(batch) = replay.read_batch(256).unwrap() {
        if let Some(own) = batch.into_iter().find(|own|own.movement.seq.0 == source) { input = Some(replay.envelope(&own).unwrap()); break; }
    }
    let input = input.unwrap(); let pinned = ctx.pinned_run_record_bytes(&input).unwrap();
    let handoff = ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&input,source,&pinned).unwrap();
    let record = RunRecord::from_bytes(&before).unwrap();
    let prior = persisted_pending_handoff(&record,&provider).unwrap().unwrap();
    assert_eq!(prior.source_movement_seq,source); assert!(prior.terminal);
    let predecessor = load_pending_artifact(&handoff.var_dir,&prior.items[0]).unwrap().1;
    let receipt = InterfaceArtifactFile::new(PathBuf::from(std::env::var("OCTOPUS_REPLAY_RECEIPT").unwrap())).unwrap();
    let payload_bytes = receipt.read_payload_bytes().unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).unwrap();
    assert_eq!(payload["session_context"]["parent"],serde_json::json!(predecessor));
    assert_eq!(payload["completed_source_settlement"]["error_artifact"],serde_json::json!(predecessor));
    assert_eq!(payload["completed_source_settlement"]["result_artifact"],serde_json::json!(input.artifact));
    assert_eq!(payload["completed_source_settlement"]["result_movement_seq"],source);
    let generation = optional_u64(&record.interface_state[&provider],"context_generation").unwrap();
    assert_eq!(payload["codex_continuity"]["context_generation"],generation+1);
    let identity = artifact_identity(&receipt,&handoff.var_dir).unwrap();
    assert!(handoff.session_snapshot_labels.contains(&identity.label));
    let old = provider_context_bindings(&record,&provider).unwrap();
    let mut expected = old.iter().filter(|binding|binding.id != predecessor.id).cloned().collect::<Vec<_>>();
    expected.push(ArtifactBinding{id:identity.artifact.id.clone(),access:Default::default()});
    let target = expected.iter().map(|binding|binding.id.clone()).collect::<Vec<_>>();
    let output_checkpoint = handoff.config.output_checkpoint(&ProviderHandoffRoute::ClientProjection,&handoff.granted_tools,&handoff.granted_interfaces,&identity.label).unwrap();
    let mut authored = handoff.author_run_with_bindings(&before,&target,&[PreparedHandoff{identity,output_checkpoint,verified_inputs:Vec::new()}],true,None,false,true,&[]).unwrap();
    handoff.attach_source_settlement(&before,&mut authored,&predecessor,generation).unwrap();
    let pending = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
    assert_eq!(provider_context_bindings(&pending,&provider).unwrap(),old);
    let publication = handoff.author_context_publication(authored.content.as_bytes(),&authored.pending_handoff).unwrap();
    let published = RunRecord::from_bytes(publication.content.as_bytes()).unwrap();
    source_settlement::validate_transition(&pending,&published,&handoff.config).unwrap();
    validate_bound_run_authoring_topology(&published,&handoff.config,None).unwrap();
    assert_eq!(provider_context_bindings(&published,&provider).unwrap(),expected);
    assert!(handoff.config.checkpoints.iter().any(|template|template.include_context && !template.static_artifact_bindings.is_empty()));
    std::fs::write(scratch.path().join("settlement.json"),&authored.content).unwrap();
    std::fs::write(scratch.path().join("published.json"),&publication.content).unwrap();
    assert_eq!(std::fs::read(path).unwrap(),before);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
    for (path,bytes) in cursors { assert_eq!(std::fs::read(path).unwrap(),bytes); }
    assert_eq!(receipt.read_payload_bytes().unwrap(),payload_bytes);
    eprintln!("actual source {source}: settlement and published active checkpoints preserve {} unique context bindings and exact rights",expected.len());
}

#[test]
#[ignore = "requires explicitly selected read-only outgoing-binding installation evidence"]
fn selected_home_outgoing_static_binding_recovers_exact_pending_output() {
    let home=PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider=InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run=RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source:u64=std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let call_seq:u64=std::env::var("OCTOPUS_REPLAY_CALL").unwrap().parse().unwrap();
    let result_seq:u64=std::env::var("OCTOPUS_REPLAY_RESULT").unwrap().parse().unwrap();
    let dir=home.join("registry/interface").join(provider.as_str());
    let descriptor:InterfaceDescriptor=serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
    let life:serde_json::Value=serde_json::from_slice(&std::fs::read(dir.join("life.json")).unwrap()).unwrap();
    let scratch=tempfile::tempdir().unwrap();let (arrivals,receiver)=tokio::sync::mpsc::channel(1);drop(receiver);
    let ctx=InterfaceLiftContext::unprivileged(provider.clone(),dir,Some(descriptor.clone()),scratch.path().join("provider"),
        home.join("ledger/movements.jsonl"),home.join("ledger/blobs"),home.join("ledger/ledger-index.sqlite"),
        BTreeMap::from([("run_records_root".into(),home.join("state/runs")),("interface_hand_in_root".into(),home.join("interface-hand-in"))]),arrivals,Default::default())
        .with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
    let path=home.join("state/runs").join(format!("{run}.json"));let current=std::fs::read(&path).unwrap();
    let ledger=std::fs::read(&ctx.movements_path).unwrap();let limits=ctx.required_host_io_limits().unwrap();
    let cursors=["ledger-cursor.json","ledger-cursor.turn-control.json"].map(|name| {
        let path=home.join("interfaces").join(provider.as_str()).join(name);let bytes=std::fs::read(&path).unwrap();(path,bytes)
    });
    let evidence=CheckedRunEvidence {movements_path:&ctx.movements_path,ledger_index_path:&ctx.ledger_index_path,blobs_dir:&ctx.blobs_dir,
        max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes};
    let call=generated_artifacts::read_movement(&evidence,call_seq).unwrap();let result=generated_artifacts::read_movement(&evidence,result_seq).unwrap();
    let mut replay=octopus_world_pyecho_life_runtime::LedgerReplayReader::open(&ctx).unwrap();let mut input=None;
    while let Some(batch)=replay.read_batch(256).unwrap() {
        if let Some(row)=batch.into_iter().find(|row|row.movement.seq.0==source) {input=Some(replay.envelope(&row).unwrap());break;}
    }
    let input=input.unwrap();let pinned=ctx.pinned_run_record_bytes(&input).unwrap();let original=RunRecord::from_bytes(&pinned).unwrap();
    let writer=declared_provider_run_writer(&original,&provider,&run_authoring_profile(&descriptor).unwrap()).unwrap();
    let proof=CheckedRunInstall {call:ConsentEvidenceRef {movement_seq:call_seq,artifact:call.artifact.clone().unwrap()},
        result:ConsentEvidenceRef {movement_seq:result_seq,artifact:result.artifact.clone().unwrap()}};
    let checked=checked_declared_run_install(&evidence,&path,&pinned,&proof,vec![writer]).unwrap();
    assert_eq!(checked.before,pinned);assert_eq!(checked.after,current);
    let record=RunRecord::from_bytes(&current).unwrap();
    let prior=writer_run::binding(&original,&provider).unwrap().unwrap();let next=writer_run::binding(&record,&provider).unwrap().unwrap();
    assert_eq!(prior.role_artifact_bindings,next.role_artifact_bindings);
    for record in [&original,&record] {
        for item in persisted_pending_handoff(record,&provider).unwrap().unwrap().items {
            let target=ctx.interface_var_dir().join(&item.manifest_relative_path);std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(home.join("interfaces").join(provider.as_str()).join(&item.manifest_relative_path),target).unwrap();
        }
    }
    let handoff=ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&input,source,&pinned).unwrap();
    let pending=persisted_pending_handoff(&record,&provider).unwrap().unwrap();assert_eq!(pending.source_movement_seq,source);
    validate_pending_items_are_active(&record,&provider,&pending).unwrap();
    assert!(handoff.recover_authoring_failure(&record,&current).unwrap().is_none());
    assert!(matches!(handoff.recover_authoring_attempt(&record,&current).unwrap(),AuthoringAttemptRecovery::None));
    assert_eq!(handoff.pending_handoff_evidence(&pending).unwrap(),vec![None]);
    let (_,artifact)=load_pending_artifact(&handoff.var_dir,&pending.items[0]).unwrap();
    assert_eq!(artifact.id,pending.items[0].artifact_id);assert_eq!(artifact.content.to_hex(),pending.items[0].artifact_content_hash);
    assert_eq!(std::fs::read(path).unwrap(),current);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
    for (path,bytes) in cursors {assert_eq!(std::fs::read(path).unwrap(),bytes);}
    eprintln!("exact checked write {call_seq}/{result_seq}: {} unchanged detached roles; source {source} reconstructs with no authoring retry and one exact unadmitted pending artifact {}",next.role_artifact_bindings.len(),artifact.id);
}

#[test]
#[ignore = "requires explicitly selected read-only historical ledger evidence"]
fn selected_ledger_checked_authoring_history() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let (arrivals, _) = tokio::sync::mpsc::channel(1);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(), home.join("registry/interface").join(provider.as_str()),
        None, home.join("interfaces").join(provider.as_str()), home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        home.join("ledger/ledger-index.sqlite"), BTreeMap::new(), arrivals, Default::default())
        .with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits { max_control_frame_bytes:1_048_576,
            max_stderr_bytes:262_144,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456 }).unwrap();
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let mut selected = BTreeMap::new();
    for (line, row) in std::io::BufReader::new(std::fs::File::open(&ctx.movements_path).unwrap()).lines().enumerate() {
        let row = row.unwrap(); let value: serde_json::Value = serde_json::from_str(&row).unwrap();
        let seq = value["seq"].as_u64().unwrap();
        if [1452,1456,1471,1473,1477,1478,1481].contains(&seq) {
            selected.insert(seq, decoder.decode(row.as_bytes(), line + 1).unwrap());
        }
    }
    let runtime = |seq| selected[&seq].runtime().unwrap();
    for seq in [1452,1456,1477,1481] {
        assert_eq!(runtime(seq).run, run);
        assert_eq!(selected[&seq].to, Endpoint::Interface(provider.clone()));
    }
    let snapshot = |seq| pinned_run_record(&ctx, &ContentHash::from_hex(
        runtime(seq).run_record_hash.as_ref().unwrap()).unwrap(), "historical ledger").unwrap();
    let (before, _) = snapshot(1452); let (after, record) = snapshot(1481);
    let (call_manifest, call_bytes) = read_pinned_artifact(&ctx, selected[&1471].artifact.as_ref().unwrap()).unwrap();
    let call: FileWriteCallEnvelope = serde_json::from_slice(&call_bytes).unwrap();
    let (result, _) = read_pinned_artifact(&ctx, selected[&1473].artifact.as_ref().unwrap()).unwrap();
    let profile = RunAuthoringProfile { file_write_tool:call.tool, file_write_label:call_manifest.label,
        file_write_result_label:result.label, result_timeout_ms:1000 };
    let config = config_from_persisted_profile(&record, &provider, &profile).unwrap();
    let evidence = CheckedRunEvidence { movements_path:&ctx.movements_path, ledger_index_path:&ctx.ledger_index_path,
        blobs_dir:&ctx.blobs_dir,max_control_frame_bytes:1_048_576,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456 };
    let writer = CheckedRunWriter { owner:provider.clone(),source:config.authoring_source_interface.clone(),
        door:config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult).unwrap().interface().clone(),
        tool:profile.file_write_tool,call_label:profile.file_write_label,result_label:profile.file_write_result_label };
    let start = concurrent_writer_evidence_start(&evidence, &RunRecord::from_bytes(&before).unwrap(), &before, 1452).unwrap();
    let chain = checked_run_file_chain_state(&evidence, &call.arguments.path, &before, &after, start, &[writer]).unwrap();
    for transition in &chain { validate_checked_provider_transition(transition).unwrap(); }
    assert_eq!(chain.len(), 3);
    assert_eq!(record.interface_state[&provider]["authoring_generation"], serde_json::json!(68));
    assert_eq!(persisted_pending_handoff(&record, &provider).unwrap().unwrap().source_movement_seq, 1456);
    assert_eq!(runtime(1477).checkpoint, runtime(1478).checkpoint);
    assert_ne!(runtime(1477).checkpoint, runtime(1481).checkpoint);
    eprintln!("historical ledger: 3 checked writes through generation 68; source 1452 precedes Stop 1456; message 1477 and Stop 1481 use distinct checkpoints");
}

#[tokio::test]
#[ignore = "requires explicitly selected read-only replay evidence"]
async fn selected_home_independent_handoff_replays_unconsumed_source() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source_movement_seq = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let run_path = home.join("state/runs").join(format!("{run}.json"));
    let current = std::fs::read(&run_path).unwrap();
    let record = RunRecord::from_bytes(&current).unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    ).unwrap();
    let profile = run_authoring_profile(&descriptor).unwrap();
    let (config, bound_topology) = resolve_profile_invocation(&profile, &provider, &run, &record, None).unwrap();
    let (context_output_labels, session_snapshot_labels) = provider_context_label_sets(&descriptor).unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let mut ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), interface_dir, Some(descriptor), home.join("interfaces").join(provider.as_str()),
        home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        home.join("ledger/ledger-index.sqlite"), BTreeMap::new(), arrivals,
        tokio_util::sync::CancellationToken::new(),
    ).with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits {
        max_control_frame_bytes: 1_048_576, max_stderr_bytes: 262_144,
        max_manifest_bytes: 1_048_576, max_payload_bytes: 268_435_456,
    }).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    ctx.ledger_index_path = scratch.path().join("unused-index.sqlite");
    let mut reader_ctx = ctx.clone();
    reader_ctx.var_dir = scratch.path().to_path_buf();
    let result_reader = LedgerTailReader::open_named(&reader_ctx, "replay-audit").unwrap();
    let handoff = ProviderOutputHandoff {
        session: ProviderHandoffSession::new(&reader_ctx).unwrap(),
        hand_in_root: home.join("interface-hand-in").join(config.authoring_source_interface.as_str()),
        var_dir: ctx.var_dir.clone(), ctx, config, run_path, source_movement_seq,
        source_carries_context: true, failed_source: false,
        context_output_labels, session_snapshot_labels,
        granted_tools: BTreeSet::new(), granted_interfaces: BTreeSet::new(), granted_mark_outcomes: BTreeSet::new(),
        bound_topology, delivery_mode: HandoffDeliveryMode::Rearm, invocation_session_snapshot: None,
        result_reader: Arc::new(tokio::sync::Mutex::new(result_reader)),
        state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
            context_artifacts: Vec::new(), static_context_artifacts: Vec::new(),
            session_context: ResolvedSessionContext { provider_interface: provider.clone(), artifacts: Vec::new() },
            session_snapshot_head: None, run_record_bytes: current.clone(), run_record_evidence_seq: None,
            consumed_input: true, consumed_context_free_independent_input: None, consumed_verified_input: false,
        })),
    };
    let pending = persisted_pending_handoff(&record, &provider).unwrap().unwrap();
    assert!(pending.source_movement_seq > source_movement_seq);
    let recovered = handoff.recover_overtaken_source(&current, &pending).unwrap();
    assert_eq!((&recovered.0, &recovered.1), (&ProviderSourceRecovery::Process, &Vec::new()));
    assert!(pending.context_publication.is_none());
    assert!(handoff.recover_authoring_failure(&record, &current).unwrap().is_none());
    assert!(matches!(handoff.recover_authoring_attempt(&record, &current).unwrap(), AuthoringAttemptRecovery::None));
    let (_, source) = handoff.session.index().unwrap().exact_provider_source_outbound(
        &handoff.config.run, &handoff.config.worker, &provider, source_movement_seq,
    ).unwrap();
    let (manifest, payload) = read_pinned_artifact(&handoff.ctx, &source.artifact).unwrap();
    append_session_payload(&mut handoff.state.lock().await.session_context,
        &manifest.label, &payload, &source.artifact, Some(source_movement_seq), "replayed input").unwrap();
    assert_eq!(handoff.recover_source_with_outcomes().await.unwrap(), (recovered.0, recovered.1));
    let context = handoff.current_session_context_artifacts().await;
    assert_eq!(context.last().unwrap().source_artifact, source.artifact);
    assert_eq!(context.last().unwrap().source_movement_seq, Some(source_movement_seq));
    let mut unadmitted = pending.clone();
    unadmitted.items[0].checkpoint.epoch += 1;
    assert!(handoff.recover_overtaken_source(&current, &unadmitted).is_err());
    let mut unrelated = pending.clone();
    unrelated.source_movement_seq = source_movement_seq;
    assert!(handoff.recover_overtaken_source(&current, &unrelated).unwrap_err().contains("independent-input"));
    let mut incomplete = pending.clone();
    incomplete.terminal = false;
    assert!(handoff.recover_overtaken_source(&current, &incomplete).unwrap_err().contains("independent-input"));
    let mut changed: serde_json::Value = serde_json::from_slice(&current).unwrap();
    changed["status"] = serde_json::json!("running");
    assert!(handoff.recover_overtaken_source(&serde_json::to_vec(&changed).unwrap(), &pending).is_err());
    assert_eq!(std::fs::read(&handoff.run_path).unwrap(), current);
    eprintln!("verified source {source_movement_seq} remains Process behind admitted independent source {}", pending.source_movement_seq);
}

#[tokio::test]
#[ignore = "requires explicitly selected read-only replay evidence"]
async fn selected_home_failed_source_rearms_its_consumed_input() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source_movement_seq: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    ).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), interface_dir, Some(descriptor.clone()), scratch.path().join("var"),
        home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        scratch.path().join("unused-index.sqlite"),
        BTreeMap::from([
            ("run_records_root".to_string(), home.join("state/runs")),
            ("interface_hand_in_root".to_string(), scratch.path().join("hand-in")),
        ]),
        arrivals, tokio_util::sync::CancellationToken::new(),
    ).with_host_io_limits(octopus_interface_transport::InterfaceHostIoLimits {
        max_control_frame_bytes: 1_048_576, max_stderr_bytes: 262_144,
        max_manifest_bytes: 1_048_576, max_payload_bytes: 268_435_456,
    }).unwrap();
    std::fs::create_dir_all(&ctx.var_dir).unwrap();
    let mut replay = octopus_world_pyecho_life_runtime::LedgerReplayReader::open(&ctx).unwrap();
    let mut input = None;
    while input.is_none() {
        let Some(batch) = replay.read_batch(256).unwrap() else { break; };
        for own in batch {
            if own.movement.seq.0 == source_movement_seq {
                input = Some(replay.envelope(&own).unwrap());
            }
        }
    }
    let input = input.expect("selected source is not an outbound movement of this provider");
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    eprintln!(
        "consumed context input sources: {:?}",
        session.consumed_context_input_sources(&ctx, &descriptor).unwrap()
    );
    let pinned = ctx.pinned_run_record_bytes(&input).unwrap();
    let canonical = session
        .from_invocation_state(&ctx, &descriptor, &run, &input, source_movement_seq, &pinned)
        .err();
    eprintln!("canonical handoff for source {source_movement_seq}: {}", canonical.as_ref().map(ToString::to_string).unwrap_or("formed".into()));
    if let Some(CheckedRunError::AwaitingResults { call_sequences }) = &canonical {
        let waiting = await_checked_evidence(&ctx, 0, || {
            session.from_invocation_state(&ctx, &descriptor, &run, &input, source_movement_seq, &pinned)
        }).await.err().expect("missing result must remain pending");
        assert!(matches!(waiting, CheckedRunError::AwaitingResults { call_sequences: actual } if actual == *call_sequences));
    }
    let failed = session
        .failed_source_handoff(&ctx, &descriptor, &run, &input, source_movement_seq)
        .unwrap();
    let error = InterfaceArtifactFile::write(
        ctx.var_dir.join("emitted"), "turn.error",
        br#"{"label":"turn.error","message":"replayed failed source"}"#,
    ).unwrap();
    let state = failed.state.lock().await;
    let before = RunRecord::from_bytes(&state.run_record_bytes).unwrap();
    let identity = artifact_identity(&error, &failed.var_dir).unwrap();
    let output_checkpoint = failed.config.output_checkpoint(
        &ProviderHandoffRoute::ClientProjection, &failed.granted_tools, &failed.granted_interfaces, &identity.label,
    ).unwrap();
    let authored = failed.author_run_with_bindings(
        &state.run_record_bytes, &state.context_artifacts,
        &[PreparedHandoff { identity, output_checkpoint, verified_inputs: Vec::new() }],
        state.consumed_input, state.consumed_context_free_independent_input.as_ref(),
        state.consumed_verified_input, true, &[],
    ).unwrap();
    let after = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
    let checkpoint = &input.runtime().unwrap().checkpoint;
    eprintln!(
        "failed source {source_movement_seq}: consumed_input={} checkpoint '{}' active before={} after={}",
        state.consumed_input, checkpoint.as_str(),
        before.active_checkpoints.contains(checkpoint), after.active_checkpoints.contains(checkpoint),
    );
    assert!(authored.pending_handoff.terminal && authored.pending_handoff.source_without_context);
    if let Some(template) = &state.consumed_context_free_independent_input {
        assert!(canonical.is_none(), "{}", canonical.unwrap());
        assert!(!after.active_checkpoints.contains(checkpoint));
        let rearmed = after.checkpoints.iter().filter(|candidate| {
            after.active_checkpoints.contains(&candidate.id)
                && checkpoint_record_matches_template(candidate, template, &run)
        }).collect::<Vec<_>>();
        assert_eq!(rearmed.len(), 1);
        assert_eq!(rearmed[0].artifact_bindings, before.checkpoint(checkpoint).unwrap().artifact_bindings);
        assert_ne!(rearmed[0].id, *checkpoint);
    } else {
        assert_eq!(after.active_checkpoints.contains(checkpoint), !state.consumed_input && before.active_checkpoints.contains(checkpoint));
    }
    assert_eq!(current_context_artifacts(&after, &failed.config).unwrap(), state.context_artifacts);
    assert_eq!(std::fs::read(home.join("state/runs").join(format!("{run}.json"))).unwrap(), state.run_record_bytes);
}
