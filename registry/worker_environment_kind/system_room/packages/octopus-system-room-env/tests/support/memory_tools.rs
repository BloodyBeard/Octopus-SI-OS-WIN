use super::*;
use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_interfaces::{
    ArtifactBindingPayload, GrantProjection, InterfaceArtifactFile, InvocationMetadata,
    RuntimeInvocationMetadata,
};
use octopus_ledger::ArtifactRef;

struct Fixture {
    root: PathBuf,
    artifacts: PathBuf,
    ees: PathBuf,
    source: ArtifactBindingPayload,
    calls: std::cell::Cell<usize>,
}

impl Fixture {
    fn new(root: &Path) -> Self {
        let artifacts = root.join("artifact");
        let ees = root.join("ee");
        let descriptor = json!({"schema_version":1,"label":"context.collection","id":"artifact-memory-test","description":"Remembered notes",
            "directory":"entries","limits":{"max_entries":64,"max_entry_bytes":65536,"max_page_bytes":65536,"max_scan_bytes":1048576}});
        std::fs::create_dir_all(artifacts.join("artifact-memory-test")).unwrap();
        std::fs::write(artifacts.join("artifact-memory-test/artifact.json"), serde_json::to_vec(&descriptor).unwrap()).unwrap();
        std::fs::create_dir_all(&ees).unwrap();
        for (ee, parent) in [("ee_home", None), ("ee_project", Some("ee_home")), ("ee_other", None)] {
            std::fs::write(ees.join(format!("{ee}.json")), serde_json::to_vec(&json!({"ee_id":ee,"parent_ee_id":parent})).unwrap()).unwrap();
        }
        let source = ArtifactBindingPayload::new(
            ArtifactRef { id: ArtifactId::new("artifact-memory-test"), content: ContentHash::of_canonical_json(&descriptor) },
            octopus_descriptors::canonical_bytes(&descriptor),
        );
        Self { root: root.to_path_buf(), artifacts, ees, source, calls: std::cell::Cell::new(0) }
    }

    fn envelope(&self, tool: &str, arguments: &Value, ee: &str, verbs: &[&str]) -> InterfaceInvocationEnvelope {
        self.calls.set(self.calls.get() + 1);
        let payload = serde_json::to_vec(&json!({"tool":tool,"arguments":arguments})).unwrap();
        let file = InterfaceArtifactFile::write(self.root.join(format!("call-{}", self.calls.get())), "tool.call", &payload).unwrap();
        let content = ContentHash::of_bytes(&std::fs::read(file.manifest_path()).unwrap());
        let grant = GrantProjection {
            tools: vec![tool.to_string()],
            worker_environment_kinds: vec!["system_room".into()],
            artifact_verbs: verbs.iter().map(|verb| verb.to_string()).collect(),
            artifact_access: serde_json::from_value(json!({"artifact-memory-test":{"read":true}})).unwrap(),
            ..Default::default()
        };
        let metadata = InvocationMetadata::new(grant).with_runtime(RuntimeInvocationMetadata {
            run: RunId::new(format!("run_{ee}")), ee: EeId::new(ee), worker: InterfaceId::new("system_worker_memory_test"),
            checkpoint: CheckpointId::new("tool"), interface: InterfaceId::new("system_memory"), epoch: 0, run_record_hash: None,
        });
        InterfaceInvocationEnvelope {
            artifact: ArtifactRef { id: ArtifactId::new(format!("tool.call:{}", content.to_hex())), content },
            artifact_file: file,
            artifact_binding_payloads: vec![self.source.clone()],
            metadata: Some(InvocationMetadata { artifact_binding_refs: vec![self.source.artifact.clone()], ..metadata }),
        }
    }

    fn run(&self, input: &InterfaceInvocationEnvelope, tool: &str, arguments: Value) -> Result<Value, String> {
        self.run_for(input, tool, arguments, None)
    }

    fn run_for(&self, input: &InterfaceInvocationEnvelope, tool: &str, arguments: Value, observed_run: Option<&str>) -> Result<Value, String> {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
        let descriptor = octopus_tool_registry::load_tool_descriptor_from_dir(&repository.join("registry/tool"), tool).unwrap().unwrap();
        octopus_tool_registry::ToolArgumentValidator::compile(&descriptor.args_schema).unwrap()
            .validate(&arguments).map_err(|error| error.to_string())?;
        let effects = self.root.join("effects");
        let host = MemoryHost { collection: "artifact-memory-test", artifact_root: &self.artifacts, ee_instances_root: &self.ees,
            effect_root: Some(&effects), maximum_bytes: 1024 * 1024, observed_run };
        let call = ToolCall { tool_id: tool.into(), arguments, handler_config: descriptor.handler_config.clone() };
        execute(input, &call, &host).map(|result| result.body).map_err(|error| error.to_string())
    }

    fn call(&self, tool: &str, arguments: Value, ee: &str, verbs: &[&str]) -> Result<Value, String> {
        let input = self.envelope(tool, &arguments, ee, verbs);
        self.run(&input, tool, arguments)
    }
}

#[test]
fn one_save_is_found_by_later_runs_within_its_scope_lineage() {
    let temporary = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(&temporary.path().canonicalize().unwrap());
    let writer = ["read", "create", "change"];
    let remember = json!({"note":"The user prefers pnpm over npm for JavaScript projects."});
    let save = fixture.envelope("memory.save", &remember, "ee_home", &writer);
    let saved = fixture.run(&save, "memory.save", remember.clone()).unwrap();
    assert_eq!(saved["result"], "memory_saved");
    assert_eq!(fixture.run(&save, "memory.save", remember.clone()).unwrap(), saved);
    let id = saved["memory"]["id"].as_str().unwrap().to_string();
    let stored: Value = serde_json::from_slice(&std::fs::read(fixture.artifacts.join(format!("artifact-memory-test/entries/{id}.json"))).unwrap()).unwrap();
    assert_eq!(stored["scope"], json!({"ee_ids":["ee_home"]}));
    assert_eq!(stored["content"]["evidence"], json!([save.artifact]));
    let repeated = fixture.call("memory.save", json!({"note":"the user prefers pnpm over npm for JavaScript projects"}), "ee_home", &writer).unwrap();
    assert_eq!((repeated["result"].as_str(), repeated["memory"]["id"].as_str()), (Some("memory_already_saved"), Some(id.as_str())));

    let found = fixture.call("memory.search", json!({"query":"which package manager, pnpm or npm?"}), "ee_project", &["read"]).unwrap();
    assert_eq!(found["matches"][0]["id"], id);
    let read = fixture.call("memory.read", json!({"id":id}), "ee_project", &["read"]).unwrap();
    assert_eq!(read["memory"]["text"], remember["note"]);
    assert_eq!(read["memory"]["usage"]["days_used"], 1);
    assert_eq!(fixture.call("memory.search", json!({"query":"pnpm"}), "ee_other", &["read"]).unwrap()["matches"], json!([]));
    assert!(fixture.call("memory.read", json!({"id":id}), "ee_other", &["read"]).is_err());
    assert!(fixture.call("memory.save", json!({"note":"Unauthorized note."}), "ee_home", &["read"]).unwrap_err().contains("create artifact verb"));
    assert!(fixture.call("memory.save", json!({"note":"Use yarn.","supersedes":[id]}), "ee_project", &writer).unwrap_err().contains("wider or different scope"));

    let correction = fixture.call("memory.save", json!({"note":"The user now uses yarn instead of pnpm.","title":"Package manager: yarn","supersedes":[id]}), "ee_home", &writer).unwrap();
    assert_eq!(correction["withdrawn"][0]["id"], id);
    let current = fixture.call("memory.search", json!({"query":"pnpm yarn"}), "ee_project", &["read"]).unwrap();
    assert_eq!(current["matches"].as_array().unwrap().len(), 1);
    assert_eq!(current["matches"][0]["title"], "Package manager: yarn");
    let history = fixture.call("memory.list", json!({"include_withdrawn":true}), "ee_home", &["read"]).unwrap();
    let states = history["memories"].as_array().unwrap().iter().map(|row| row["state"].as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!((history["total"].as_u64(), states.contains(&"withdrawn")), (Some(2), true));
    let names = std::fs::read_dir(fixture.artifacts.join("artifact-memory-test/entries")).unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect::<Vec<_>>();
    assert!(names.len() == 2 && names.iter().all(|name| name.ends_with(".json") && !name.starts_with('.')), "{names:?}");
}

#[test]
fn observation_keeps_its_admitted_scope_across_moves_and_retries() {
    let temporary = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(&temporary.path().canonicalize().unwrap());
    let writer = ["read", "create", "change"];
    let persist = |label: &str, value: &Value| {
        let bytes = serde_json::to_vec(value).unwrap();
        let file = InterfaceArtifactFile::write(fixture.root.join("carriers"), label, &bytes).unwrap();
        let manifest = std::fs::read(file.manifest_path()).unwrap();
        let content = ContentHash::of_bytes(&manifest);
        ArtifactBindingPayload::new(ArtifactRef { id: ArtifactId::new(format!("{label}:{}", content.to_hex())), content }, bytes)
    };
    let bind = |input: &mut InterfaceInvocationEnvelope, source: &ArtifactBindingPayload| {
        let metadata = input.metadata.as_mut().unwrap();
        metadata.grant.artifact_access.insert(source.artifact.id.to_string(), serde_json::from_value(json!({"read":true})).unwrap());
        metadata.artifact_binding_refs.push(source.artifact.clone());
        input.artifact_binding_payloads.push(source.clone());
    };
    let mut experience = json!({"schema_version":1,"label":"memory.experience.input","collection_source":"artifact-memory-test",
        "source":{"run":"observed","ee":"ee_project","terminal_source_movement_seq":10,"projection_source_movement_seq":12,"terminal_artifact":fixture.source.artifact}});
    let source = persist("memory.experience.input", &experience);
    let note = json!({"note":"Private project deployment procedure."});
    let mut input = fixture.envelope("memory.save", &note, "ee_home", &writer);
    assert!(fixture.run_for(&input, "memory.save", note.clone(), Some("observed")).unwrap_err().contains("no bound experience"));
    bind(&mut input, &source);
    let saved = fixture.run_for(&input, "memory.save", note.clone(), Some("observed")).unwrap();
    assert_eq!(saved["memory"]["scope_ee_ids"], json!(["ee_project"]));
    assert_eq!(fixture.call("memory.search", json!({"query":"private project"}), "ee_home", &["read"]).unwrap()["matches"], json!([]));
    assert!(fixture.run_for(&input, "memory.save", note.clone(), Some("another_run")).is_err());
    assert_eq!(fixture.run_for(&input, "memory.save", note.clone(), Some("observed")).unwrap(), saved);
    experience["source"]["ee"] = json!("ee_home");
    experience["source"]["projection_source_movement_seq"] = json!(30);
    let later = persist("memory.experience.input", &experience);
    let future_note = json!({"note":"New Home experience after the move."});
    let mut future = fixture.envelope("memory.save", &future_note, "ee_other", &writer);
    bind(&mut future, &source);
    bind(&mut future, &later);
    assert_eq!(fixture.run_for(&future, "memory.save", future_note, Some("observed")).unwrap()["memory"]["scope_ee_ids"], json!(["ee_home"]));
    let mut denied = input.clone();
    denied.metadata.as_mut().unwrap().grant.artifact_access.remove(source.artifact.id.as_str());
    assert!(fixture.run_for(&denied, "memory.save", note.clone(), Some("observed")).is_err());
    let plan = fixture.root.join("effects/memory-save").join(input.artifact.content.to_hex()).join("plan.json");
    let mut poisoned: Value = serde_json::from_slice(&std::fs::read(&plan).unwrap()).unwrap();
    poisoned["memory"]["scope_ee_ids"] = json!(["ee_home"]);
    std::fs::write(plan, serde_json::to_vec(&poisoned).unwrap()).unwrap();
    assert!(fixture.run_for(&input, "memory.save", note, Some("observed")).unwrap_err().contains("differs from its admitted experience scope"));
}
