use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_context_collections::{
    BoundCollection, CollectionEntry, CollectionRecord, EntryContent, EntryScope,
    OrderedFileWritePlan, PublicationState, ScopeContext,
};
use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_ledger::ContentHash;
use octopus_tool_handlers::{ToolCall, ToolHandlerError, ToolHandlerResult};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const MAX_NOTE_BYTES: usize = 8192;
const MAX_TITLE_CHARS: usize = 120;
const MAX_DESCRIPTION_CHARS: usize = 280;
const MAX_SUPERSEDED: usize = 16;
const DEFAULT_SEARCH_RESULTS: usize = 8;
const MAX_SEARCH_RESULTS: usize = 20;
const DEFAULT_PAGE: usize = 20;
const MAX_PAGE: usize = 50;
const MAX_QUERY_TERMS: usize = 32;
const MAX_EE_DEPTH: usize = 64;
const MAX_USAGE_DAYS: usize = 4096;
const PREVIEW_CHARS: usize = 320;
const APPLICABILITY: &str = "Use when the current work relates to this remembered note.";

pub(crate) struct MemoryHost<'a> {
    pub(crate) collection: &'a str,
    pub(crate) artifact_root: &'a Path,
    pub(crate) ee_instances_root: &'a Path,
    pub(crate) effect_root: Option<&'a Path>,
    pub(crate) room_kinds: &'a [String],
    pub(crate) maximum_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveArguments {
    note: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    supersedes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    id: String,
    #[serde(default)]
    revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArguments {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    include_withdrawn: bool,
}

struct Memory<'a> {
    input: &'a InterfaceInvocationEnvelope,
    call: &'a ToolCall,
    host: &'a MemoryHost<'a>,
    collection: BoundCollection,
    caller: ScopeContext,
    lineage: Vec<String>,
}

pub(crate) fn execute(
    input: &InterfaceInvocationEnvelope,
    call: &ToolCall,
    host: &MemoryHost<'_>,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let operation = call
        .handler_config
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let body = match operation {
        "memory_save" => {
            let arguments = parse(call)?;
            Memory::open(input, call, host)?.save(arguments)?
        }
        "memory_search" => {
            let arguments = parse(call)?;
            Memory::open(input, call, host)?.search(arguments)?
        }
        "memory_read" => {
            let arguments = parse(call)?;
            Memory::open(input, call, host)?.read(arguments)?
        }
        "memory_list" => {
            let arguments = parse(call)?;
            Memory::open(input, call, host)?.list(arguments)?
        }
        other => {
            return Err(ToolHandlerError::Execution {
                tool_id: call.tool_id.clone(),
                reason: format!("memory operation '{other}' is not declared"),
            });
        }
    };
    Ok(ToolHandlerResult::new(body))
}

fn parse<T: DeserializeOwned>(call: &ToolCall) -> Result<T, ToolHandlerError> {
    serde_json::from_value(call.arguments.clone()).map_err(|error| {
        ToolHandlerError::InvalidArguments {
            tool_id: call.tool_id.clone(),
            reason: error.to_string(),
        }
    })
}

impl<'a> Memory<'a> {
    fn open(
        input: &'a InterfaceInvocationEnvelope,
        call: &'a ToolCall,
        host: &'a MemoryHost<'a>,
    ) -> Result<Self, ToolHandlerError> {
        let failure = |reason: String| ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason,
        };
        let runtime = input.runtime().map_err(|error| failure(error.to_string()))?;
        let source = crate::collection_tools::readable_source(input, host.collection)
            .map_err(|error| failure(format!("memory collection '{}': {error}", host.collection)))?;
        let collection = crate::collection_tools::open(host.artifact_root, source).map_err(failure)?;
        let caller = ScopeContext {
            ee_id: runtime.ee.to_string(),
            worker_interface: runtime.worker.to_string(),
        };
        let lineage = ee_lineage(host.ee_instances_root, &caller.ee_id).map_err(failure)?;
        Ok(Self {
            input,
            call,
            host,
            collection,
            caller,
            lineage,
        })
    }

    fn save(&self, arguments: SaveArguments) -> Result<Value, ToolHandlerError> {
        let note = arguments.note.trim();
        if note.is_empty()
            || note.len() > MAX_NOTE_BYTES
            || note.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
        {
            return Err(self.invalid(format!(
                "note must be nonblank text of at most {MAX_NOTE_BYTES} bytes"
            )));
        }
        let title = match arguments.title.as_deref().map(str::trim) {
            Some(title)
                if title.is_empty()
                    || title.chars().count() > MAX_TITLE_CHARS
                    || title.chars().any(char::is_control) =>
            {
                return Err(self.invalid(format!(
                    "title must be one nonblank line of at most {MAX_TITLE_CHARS} characters"
                )));
            }
            Some(title) => title.to_string(),
            None => headline(note, MAX_TITLE_CHARS),
        };
        let mut superseded = BTreeSet::new();
        if arguments.supersedes.len() > MAX_SUPERSEDED
            || !arguments
                .supersedes
                .iter()
                .all(|id| superseded.insert(id.trim()))
        {
            return Err(self.invalid(format!(
                "supersedes lists at most {MAX_SUPERSEDED} distinct memory ids"
            )));
        }
        let grant = self
            .input
            .grant()
            .map_err(|error| self.execution(error.to_string()))?;
        if !grant.may_create_artifact() {
            return Err(self.execution(
                "memory.save is refused: the formed grant holds no create artifact verb",
            ));
        }
        if !superseded.is_empty() && !grant.artifact_verbs.iter().any(|verb| verb == "change") {
            return Err(self.execution(
                "superseding a memory is refused: the formed grant holds no change artifact verb",
            ));
        }
        let effect_root = self
            .host
            .effect_root
            .ok_or_else(|| self.execution("memory.save requires formation-scoped durable state"))?;
        let directory = effect_root
            .join("memory-save")
            .join(self.input.artifact.content.to_hex());
        let _lock = crate::file_write::lock_invocation(&directory).map_err(|error| self.execution(error))?;
        let plan_path = directory.join("plan.json");
        let plan = match self.retained_plan(&plan_path)? {
            Some(plan) => plan,
            None => {
                let records = self.records(false)?;
                let wanted = normalized(note);
                if let Some(existing) = records.iter().find(|record| {
                    note_text(&record.entry).is_some_and(|text| normalized(text) == wanted)
                }) {
                    return Ok(json!({
                        "result": "memory_already_saved",
                        "memory": self.pin(existing),
                    }));
                }
                let plan = self.plan_save(note, title, &superseded, &records)?;
                let mut bytes = serde_json::to_vec(&plan)
                    .map_err(|error| self.execution(error.to_string()))?;
                bytes.push(b'\n');
                crate::file_write::install_effect_record(&plan_path, &bytes)
                    .map_err(|error| self.execution(error))?;
                plan
            }
        };
        self.real_directory(&self.collection.directory().join("entries"), true)?;
        let roots = BTreeMap::from([(
            "registry_artifact_root".to_string(),
            self.host.artifact_root.to_path_buf(),
        )]);
        let receipt = crate::execute_file_write_result(
            self.input,
            &plan["file_write"],
            self.host.room_kinds,
            &roots,
            &effect_root.join("file-write"),
            self.host.maximum_bytes,
        )
        .map_err(|error| ToolHandlerError::ExecutionWithDetails {
            tool_id: self.call.tool_id.clone(),
            reason: error.message,
            details: error.details.unwrap_or_default(),
        })?;
        Ok(json!({
            "result": "memory_saved",
            "memory": plan["memory"],
            "withdrawn": plan["withdrawn"],
            "writes": receipt["writes"],
        }))
    }

    fn plan_save(
        &self,
        note: &str,
        title: String,
        superseded: &BTreeSet<&str>,
        records: &[CollectionRecord],
    ) -> Result<Value, ToolHandlerError> {
        let now = unix_seconds().map_err(|error| self.execution(error))?;
        let hash = self.input.artifact.content.to_hex();
        let id = format!("memory-{}-{}-{}", compact_time(now), slug(&title), &hash[..8]);
        let entry = CollectionEntry {
            schema_version: 1,
            id: id.clone(),
            title: title.clone(),
            description: headline(note, MAX_DESCRIPTION_CHARS),
            applicability: APPLICABILITY.to_string(),
            state: PublicationState::Active,
            scope: EntryScope {
                ee_ids: Some(vec![self.caller.ee_id.clone()]),
                worker_interfaces: None,
            },
            content: Some(EntryContent {
                text: Some(note.to_string()),
                evidence: vec![self.input.artifact.clone()],
            }),
            selection: json!({}),
            requires: Vec::new(),
        };
        let created = self
            .collection
            .prepare_publication(&entry, None, &self.caller)
            .map_err(|error| self.execution(error.to_string()))?;
        let mut writes = created.file_write.writes;
        let mut withdrawn = Vec::new();
        for id in superseded {
            let record = records
                .iter()
                .find(|record| record.entry.id == *id)
                .ok_or_else(|| {
                    self.invalid(format!("memory '{id}' is not an active memory visible here"))
                })?;
            if record.entry.scope.ee_ids.as_deref() != Some(std::slice::from_ref(&self.caller.ee_id))
                || record.entry.scope.worker_interfaces.is_some()
            {
                return Err(self.invalid(format!(
                    "memory '{id}' belongs to a wider or different scope; save a correcting note instead"
                )));
            }
            let mut retired = record.entry.clone();
            retired.state = PublicationState::Withdrawn;
            let plan = self
                .collection
                .prepare_publication(&retired, Some(record.revision), &self.caller)
                .map_err(|error| self.execution(error.to_string()))?;
            withdrawn.push(json!({"id": id, "revision": plan.pin.revision.to_hex()}));
            writes.extend(plan.file_write.writes);
        }
        let batch = serde_json::to_value(OrderedFileWritePlan {
            ordered: true,
            writes,
        })
        .map_err(|error| self.execution(error.to_string()))?;
        Ok(json!({
            "schema_version": 1,
            "artifact": self.input.artifact,
            "memory": {
                "id": id,
                "revision": created.pin.revision.to_hex(),
                "title": title,
                "saved_at": iso_time(now),
                "scope_ee_ids": [self.caller.ee_id],
            },
            "withdrawn": withdrawn,
            "file_write": batch,
        }))
    }

    fn retained_plan(&self, path: &Path) -> Result<Option<Value>, ToolHandlerError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.execution(format!("memory save plan cannot be read: {error}"))),
        };
        let plan: Value = serde_json::from_slice(&bytes)
            .map_err(|error| self.execution(format!("memory save plan is malformed: {error}")))?;
        let artifact = serde_json::to_value(&self.input.artifact)
            .map_err(|error| self.execution(error.to_string()))?;
        if plan.get("schema_version") != Some(&json!(1)) || plan.get("artifact") != Some(&artifact) {
            return Err(self.execution("memory save plan differs from its invocation"));
        }
        Ok(Some(plan))
    }

    fn search(&self, arguments: SearchArguments) -> Result<Value, ToolHandlerError> {
        let terms = terms(&arguments.query);
        if terms.is_empty() {
            return Err(self.invalid("query must contain searchable words"));
        }
        let limit = self.bounded(arguments.max_results, DEFAULT_SEARCH_RESULTS, MAX_SEARCH_RESULTS)?;
        let mut matches = self
            .records(false)?
            .into_iter()
            .filter_map(|record| {
                let score = score(&record.entry, &terms);
                (score > 0).then_some((score, record))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| right.entry.id.cmp(&left.entry.id))
        });
        matches.truncate(limit);
        let rows = matches
            .iter()
            .map(|(score, record)| {
                let mut row = self.summary(record)?;
                row["score"] = json!(score);
                row["preview"] = json!(note_text(&record.entry).map(|text| preview(text, PREVIEW_CHARS)));
                Ok(row)
            })
            .collect::<Result<Vec<_>, ToolHandlerError>>()?;
        Ok(json!({
            "result": "memories_found",
            "query": arguments.query.trim(),
            "matches": rows,
        }))
    }

    fn read(&self, arguments: ReadArguments) -> Result<Value, ToolHandlerError> {
        let id = arguments.id.trim();
        let records = self.records(false)?;
        let record = records
            .iter()
            .find(|record| record.entry.id == id)
            .ok_or_else(|| self.execution(format!("memory '{id}' is not an active memory visible here")))?;
        if let Some(revision) = arguments.revision.as_deref().map(str::trim) {
            if revision != record.revision.to_hex() {
                return Err(self.execution(format!(
                    "memory '{id}' changed; its current revision is {}",
                    record.revision.to_hex()
                )));
            }
        }
        self.record_usage(record)?;
        let mut memory = self.summary(record)?;
        memory["applicability"] = json!(record.entry.applicability);
        memory["text"] = json!(note_text(&record.entry));
        memory["evidence"] = json!(record.entry.content.as_ref().map(|content| &content.evidence));
        memory["selection"] = record.entry.selection.clone();
        memory["requires"] = json!(record.entry.requires);
        Ok(json!({"result": "memory_read", "memory": memory}))
    }

    fn list(&self, arguments: ListArguments) -> Result<Value, ToolHandlerError> {
        let records = self.records(arguments.include_withdrawn)?;
        let rows = records
            .iter()
            .map(|record| json!([record.entry.id, record.revision.to_hex()]))
            .collect::<Vec<_>>();
        let fingerprint =
            ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&Value::Array(rows))).to_hex();
        let start = match arguments.cursor.as_deref() {
            None => 0,
            Some(cursor) => {
                let offset = cursor
                    .strip_prefix(fingerprint.as_str())
                    .and_then(|rest| rest.strip_prefix(':'))
                    .ok_or_else(|| self.execution("memory list cursor is stale; list again without a cursor"))?;
                offset
                    .parse::<usize>()
                    .ok()
                    .filter(|offset| *offset <= records.len())
                    .ok_or_else(|| self.invalid("memory list cursor is malformed"))?
            }
        };
        let limit = self.bounded(arguments.limit, DEFAULT_PAGE, MAX_PAGE)?;
        let end = start.saturating_add(limit).min(records.len());
        let memories = records[start..end]
            .iter()
            .map(|record| self.summary(record))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({
            "result": "memories_listed",
            "memories": memories,
            "total": records.len(),
            "next_cursor": (end < records.len()).then(|| format!("{fingerprint}:{end}")),
        }))
    }

    fn records(&self, include_withdrawn: bool) -> Result<Vec<CollectionRecord>, ToolHandlerError> {
        let mut records = self
            .collection
            .records()
            .map_err(|error| self.execution(error.to_string()))?
            .into_iter()
            .filter(|record| {
                (include_withdrawn || record.entry.state == PublicationState::Active)
                    && self.lineage.iter().any(|ee| {
                        record.entry.scope.permits(&ScopeContext {
                            ee_id: ee.clone(),
                            worker_interface: self.caller.worker_interface.clone(),
                        })
                    })
            })
            .collect::<Vec<_>>();
        records.sort_by(|left, right| left.entry.id.cmp(&right.entry.id));
        Ok(records)
    }

    fn pin(&self, record: &CollectionRecord) -> Value {
        json!({
            "id": record.entry.id,
            "revision": record.revision.to_hex(),
            "title": record.entry.title,
            "saved_at": saved_at(&record.entry.id),
            "scope_ee_ids": record.entry.scope.ee_ids,
        })
    }

    fn summary(&self, record: &CollectionRecord) -> Result<Value, ToolHandlerError> {
        let mut summary = self.pin(record);
        summary["description"] = json!(record.entry.description);
        summary["state"] = json!(record.entry.state);
        summary["usage"] = self.usage(&record.entry.id)?;
        Ok(summary)
    }

    fn usage(&self, id: &str) -> Result<Value, ToolHandlerError> {
        let usage = self.collection.directory().join("usage");
        let directory = usage.join(id);
        if !self.real_directory(&usage, false)? || !self.real_directory(&directory, false)? {
            return Ok(json!({"days_used": 0, "last_used": null}));
        }
        let mut days = Vec::new();
        for entry in std::fs::read_dir(&directory)
            .map_err(|error| self.execution(format!("memory usage of '{id}' cannot be listed: {error}")))?
        {
            let entry = entry.map_err(|error| self.execution(error.to_string()))?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| self.execution(format!("memory usage of '{id}' has a non-UTF-8 name")))?;
            if name.starts_with('.') {
                continue;
            }
            let day = name
                .strip_suffix(".json")
                .filter(|day| is_day(day))
                .ok_or_else(|| self.execution(format!("memory usage of '{id}' holds unexpected '{name}'")))?;
            days.push(day.to_string());
            if days.len() > MAX_USAGE_DAYS {
                return Err(self.execution(format!("memory usage of '{id}' exceeds its bound")));
            }
        }
        days.sort();
        Ok(json!({"days_used": days.len(), "last_used": days.last()}))
    }

    fn record_usage(&self, record: &CollectionRecord) -> Result<(), ToolHandlerError> {
        let now = unix_seconds().map_err(|error| self.execution(error))?;
        let usage = self.collection.directory().join("usage");
        let directory = usage.join(&record.entry.id);
        self.real_directory(&usage, true)?;
        self.real_directory(&directory, true)?;
        let path = directory.join(format!("{}.json", day(now)));
        if std::fs::symlink_metadata(&path).is_ok() {
            return Ok(());
        }
        let runtime = self
            .input
            .runtime()
            .map_err(|error| self.execution(error.to_string()))?;
        let receipt = json!({
            "schema_version": 1,
            "entry_id": record.entry.id,
            "revision": record.revision.to_hex(),
            "day": day(now),
            "run": runtime.run.as_str(),
        });
        let mut bytes = octopus_descriptors::canonical_bytes(&receipt);
        bytes.push(b'\n');
        match crate::file_write::install_effect_record(&path, &bytes) {
            Ok(()) => Ok(()),
            Err(_) if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) => Ok(()),
            Err(error) => Err(self.execution(format!("memory usage cannot be recorded: {error}"))),
        }
    }

    fn real_directory(&self, path: &Path, create: bool) -> Result<bool, ToolHandlerError> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
            Ok(_) => Err(self.execution(format!("memory path '{}' is not a real directory", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                match std::fs::create_dir(path) {
                    Ok(()) => Ok(true),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        self.real_directory(path, false)
                    }
                    Err(error) => Err(self.execution(format!(
                        "memory directory '{}' cannot be created: {error}",
                        path.display()
                    ))),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(self.execution(format!("memory path '{}': {error}", path.display()))),
        }
    }

    fn bounded(&self, value: Option<usize>, default: usize, maximum: usize) -> Result<usize, ToolHandlerError> {
        match value {
            None => Ok(default),
            Some(value) if (1..=maximum).contains(&value) => Ok(value),
            Some(_) => Err(self.invalid(format!("limit must be between 1 and {maximum}"))),
        }
    }

    fn invalid(&self, reason: impl Into<String>) -> ToolHandlerError {
        ToolHandlerError::InvalidArguments {
            tool_id: self.call.tool_id.clone(),
            reason: reason.into(),
        }
    }

    fn execution(&self, reason: impl Into<String>) -> ToolHandlerError {
        ToolHandlerError::Execution {
            tool_id: self.call.tool_id.clone(),
            reason: reason.into(),
        }
    }
}

fn ee_lineage(root: &Path, ee: &str) -> Result<Vec<String>, String> {
    let mut lineage = Vec::new();
    let mut cursor = Some(ee.to_string());
    while let Some(id) = cursor {
        if lineage.contains(&id) {
            return Err(format!("EE lineage of '{ee}' cycles at '{id}'"));
        }
        if lineage.len() >= MAX_EE_DEPTH {
            return Err(format!("EE lineage of '{ee}' exceeds {MAX_EE_DEPTH} layers"));
        }
        if id.is_empty() || id.contains(['/', '\\', '\0']) || matches!(id.as_str(), "." | "..") {
            return Err(format!("EE id '{id}' is not a record name"));
        }
        let path: PathBuf = root.join(format!("{id}.json"));
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("EE instance '{id}' cannot be read: {error}"))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("EE instance '{id}' is malformed: {error}"))?;
        if value.get("ee_id").and_then(Value::as_str) != Some(id.as_str()) {
            return Err(format!("EE instance '{id}' declares a different identity"));
        }
        cursor = match value.get("parent_ee_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(parent)) => Some(parent.clone()),
            Some(_) => return Err(format!("EE instance '{id}' has a malformed parent")),
        };
        lineage.push(id);
    }
    Ok(lineage)
}

fn note_text(entry: &CollectionEntry) -> Option<&str> {
    entry.content.as_ref().and_then(|content| content.text.as_deref())
}

fn normalized(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '!'])
        .to_string()
}

fn headline(text: &str, maximum: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    preview(line, maximum)
}

fn preview(text: &str, maximum: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= maximum {
        return collapsed;
    }
    let mut cut = collapsed.chars().take(maximum - 1).collect::<String>();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

fn slug(text: &str) -> String {
    let mut slug = String::new();
    for word in text
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        if slug.len() + word.len() + 1 > 40 {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(word);
    }
    if slug.is_empty() { "note".to_string() } else { slug }
}

fn terms(query: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| term.chars().count() >= 2)
        .filter(|term| seen.insert(term.to_string()))
        .take(MAX_QUERY_TERMS)
        .map(str::to_string)
        .collect()
}

fn score(entry: &CollectionEntry, terms: &[String]) -> u64 {
    let fields = [
        (entry.title.to_lowercase(), 3),
        (entry.description.to_lowercase(), 2),
        (entry.applicability.to_lowercase(), 1),
        (note_text(entry).unwrap_or_default().to_lowercase(), 2),
    ];
    terms
        .iter()
        .map(|term| {
            fields
                .iter()
                .filter(|(text, _)| text.contains(term.as_str()))
                .map(|(_, weight)| *weight)
                .sum::<u64>()
        })
        .sum()
}

fn unix_seconds() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|error| format!("system clock precedes the Unix epoch: {error}"))
}

fn civil(seconds: u64) -> (i64, u32, u32, u64) {
    let days = (seconds / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day, seconds % 86_400)
}

fn day(seconds: u64) -> String {
    let (year, month, day, _) = civil(seconds);
    format!("{year:04}-{month:02}-{day:02}")
}

fn compact_time(seconds: u64) -> String {
    let (year, month, day, time) = civil(seconds);
    format!(
        "{year:04}{month:02}{day:02}t{:02}{:02}{:02}z",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}

fn iso_time(seconds: u64) -> String {
    let (year, month, day, time) = civil(seconds);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}

fn saved_at(id: &str) -> Value {
    let Some(stamp) = id.strip_prefix("memory-").and_then(|rest| rest.get(..16)) else {
        return Value::Null;
    };
    let bytes = stamp.as_bytes();
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if !(digits(0..8) && bytes[8] == b't' && digits(9..15) && bytes[15] == b'z') {
        return Value::Null;
    }
    json!(format!(
        "{}-{}-{}T{}:{}:{}Z",
        &stamp[0..4],
        &stamp[4..6],
        &stamp[6..8],
        &stamp[9..11],
        &stamp[11..13],
        &stamp[13..15]
    ))
}

fn is_day(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

#[cfg(test)]
#[path = "../tests/support/memory_tools.rs"]
mod tests;
