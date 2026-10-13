//! Native bound collection payloads, bounded metadata IO and publication plans.
//! Callers establish source binding/read rights and ordinary writer authority.
//! This package never follows external references, forms grants or writes files.

mod safe_io;
mod schema;
pub use schema::*;

use octopus_ledger::{ArtifactRef, ContentHash};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, CollectionError>;

pub const MAX_DESCRIPTOR_BYTES: u64 = 256 * 1024;

pub fn read_descriptor(root: &Path, source: &ArtifactRef) -> Result<Value> {
    schema::safe_id(source.id.as_str())?;
    let root = safe_io::SafeDir::absolute(root)?;
    let directory = root
        .child(source.id.as_str(), false)?
        .ok_or(CollectionError::SourceMismatch)?;
    let read = directory.read("artifact.json", MAX_DESCRIPTOR_BYTES)?;
    let value: Value = serde_json::from_slice(&read.bytes)?;
    let descriptor = parse_descriptor(&value)?;
    if descriptor.id != source.id.as_str()
        || ContentHash::of_canonical_json(&value) != source.content
    {
        return Err(CollectionError::SourceMismatch);
    }
    Ok(value)
}

#[derive(Debug, Error)]
pub enum CollectionError {
    #[error("invalid collection schema: {0}")]
    InvalidSchema(String),
    #[error("invalid collection JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsafe collection path: {0}")]
    UnsafePath(PathBuf),
    #[error("collection IO at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("collection limit exceeded: {0}")]
    LimitExceeded(&'static str),
    #[error("collection source does not match its bound canonical hash")]
    SourceMismatch,
    #[error("collection changed during its bounded scan")]
    ChangedDuringRead,
    #[error("collection source is unavailable because an entry failed integrity checks")]
    SourceUnavailable,
    #[error("collection pagination snapshot is stale")]
    StaleCursor,
    #[error("collection pagination anchor is not visible in this query")]
    InvalidCursor,
    #[error("publication selection source or revision is stale")]
    StaleSelection,
    #[error("collection entry '{0}' is unavailable in this scope")]
    NotVisible(String),
    #[error("collection entry '{0}' already exists; supply its exact revision")]
    AlreadyExists(String),
    #[error("no-follow directory-handle IO is unsupported on this platform")]
    UnsupportedPlatform,
}

pub struct BoundCollection {
    source: ArtifactRef,
    descriptor: CollectionDescriptor,
    directory: safe_io::SafeDir,
}

struct ScannedEntry {
    entry: CollectionEntry,
    value: Value,
    revision: ContentHash,
    bytes: Vec<u8>,
}

struct ScanSnapshot {
    count: usize,
    bytes: u64,
}

impl BoundCollection {
    pub fn open(root: &Path, source: &ArtifactRef, bound_payload: &Value) -> Result<Self> {
        let descriptor = parse_descriptor(bound_payload)?;
        if descriptor.id != source.id.as_str()
            || ContentHash::of_canonical_json(bound_payload) != source.content
        {
            return Err(CollectionError::SourceMismatch);
        }
        let artifact_root = safe_io::SafeDir::absolute(root)?;
        let directory = artifact_root
            .child(&descriptor.id, false)?
            .ok_or(CollectionError::SourceMismatch)?;
        let collection = Self {
            source: source.clone(),
            descriptor,
            directory,
        };
        collection.check_descriptor()?;
        Ok(collection)
    }

    pub fn source(&self) -> &ArtifactRef {
        &self.source
    }
    pub fn descriptor(&self) -> &CollectionDescriptor {
        &self.descriptor
    }

    pub fn list(&self, scope: &ScopeContext, request: &ListRequest) -> Result<CollectionPage> {
        validate_scope_context(scope)?;
        if request.limit == 0 || request.limit > self.descriptor.limits.max_entries {
            return Err(CollectionError::LimitExceeded("page entry count"));
        }
        if let Some(cursor) = &request.cursor {
            schema::safe_id(&cursor.after)?;
        }
        let query = request.query.as_deref().unwrap_or("").to_lowercase();
        if query.len() as u64 > self.descriptor.limits.max_page_bytes {
            return Err(CollectionError::LimitExceeded("query bytes"));
        }
        let placeholder = ContentHash::of_bytes(&[]);
        let mut page = CollectionPage {
            source: self.source.clone(),
            snapshot: placeholder,
            entries: Vec::new(),
            has_more: false,
            cursor: None,
        };
        let mut anchor_found = request.cursor.is_none();
        let mut full = false;
        let mut visible_revisions = Vec::new();
        self.scan(|record| {
            let entry = &record.entry;
            if entry.state != PublicationState::Active
                || !entry.scope.permits(scope)
                || !matches_query(entry, &query)
            {
                return Ok(());
            }
            visible_revisions.push((entry.id.clone(), record.revision.to_hex()));
            if let Some(cursor) = &request.cursor {
                if entry.id == cursor.after {
                    anchor_found = true;
                }
                if entry.id <= cursor.after {
                    return Ok(());
                }
            }
            if full || page.entries.len() >= request.limit as usize {
                page.has_more = true;
                return Ok(());
            }
            page.entries.push(self.summary(record)?);
            page.has_more = true;
            page.cursor = Some(CollectionCursor {
                snapshot: placeholder,
                after: entry.id.clone(),
            });
            if serde_json::to_vec(&page)?.len() as u64 > self.descriptor.limits.max_page_bytes {
                page.entries.pop();
                if page.entries.is_empty() {
                    return Err(CollectionError::LimitExceeded(
                        "one summary exceeds page bytes",
                    ));
                }
                full = true;
            } else {
                page.has_more = false;
            }
            Ok(())
        })?;
        let hash = ContentHash::of_canonical_json(
            &json!({"source":self.source,"scope":scope,"query":query,"entries":visible_revisions}),
        );
        if request
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.snapshot != hash)
        {
            return Err(CollectionError::StaleCursor);
        }
        if !anchor_found {
            return Err(CollectionError::InvalidCursor);
        }
        page.snapshot = hash;
        page.cursor = if page.has_more {
            Some(CollectionCursor {
                snapshot: hash,
                after: page
                    .entries
                    .last()
                    .ok_or(CollectionError::InvalidCursor)?
                    .pin
                    .entry_id
                    .clone(),
            })
        } else {
            None
        };
        if serde_json::to_vec(&page)?.len() as u64 > self.descriptor.limits.max_page_bytes {
            return Err(CollectionError::LimitExceeded("page bytes"));
        }
        Ok(page)
    }

    pub fn read(&self, pin: &CollectionSelection, scope: &ScopeContext) -> Result<EntryRead> {
        let record = self.selected(pin, scope)?;
        Ok(EntryRead {
            pin: pin.clone(),
            entry: record.value,
        })
    }

    pub fn resolve_metadata(
        &self,
        pin: &CollectionSelection,
        target: &ScopeContext,
    ) -> Result<EntrySummary> {
        self.summary(&self.selected(pin, target)?)
    }

    pub fn prepare_publication(
        &self,
        entry: &CollectionEntry,
        expected_revision: Option<ContentHash>,
        caller: &ScopeContext,
    ) -> Result<PublicationPlan> {
        entry.validate()?;
        validate_scope_context(caller)?;
        let value = serde_json::to_value(entry)?;
        let content = String::from_utf8(octopus_descriptors::canonical_bytes(&value))
            .map_err(|_| schema::invalid("entry encoding"))?;
        if content.len() as u64 > self.descriptor.limits.max_entry_bytes {
            return Err(CollectionError::LimitExceeded("entry bytes"));
        }
        let revision = ContentHash::of_canonical_json(&value);
        let summary = self.summary(&ScannedEntry {
            entry: entry.clone(), value: value.clone(), revision, bytes: Vec::new(),
        })?;
        let single_page = CollectionPage {
            source: self.source.clone(), snapshot: revision, entries: vec![summary],
            has_more: true,
            cursor: Some(CollectionCursor { snapshot: revision, after: entry.id.clone() }),
        };
        if serde_json::to_vec(&single_page)?.len() as u64 > self.descriptor.limits.max_page_bytes {
            return Err(CollectionError::LimitExceeded("one summary exceeds page bytes"));
        }
        let mut previous = None;
        let snapshot = self.scan(|record| {
            if record.entry.id == entry.id {
                if !record.entry.scope.permits(caller) {
                    return Err(CollectionError::NotVisible(entry.id.clone()));
                }
                previous = Some((record.revision, record.bytes.clone()));
            }
            Ok(())
        })?;
        match (&previous, expected_revision) {
            (Some(_), None) => return Err(CollectionError::AlreadyExists(entry.id.clone())),
            (Some((revision, _)), Some(expected)) if *revision == expected => {}
            (Some(_), Some(_)) | (None, Some(_)) => return Err(CollectionError::StaleSelection),
            (None, None) if snapshot.count >= self.descriptor.limits.max_entries as usize => {
                return Err(CollectionError::LimitExceeded("entry count"))
            }
            (None, None) => {}
        }
        let previous_bytes = previous.as_ref().map_or(0, |(_, bytes)| bytes.len() as u64);
        let next_bytes = snapshot.bytes.checked_sub(previous_bytes)
            .and_then(|bytes| bytes.checked_add(content.len() as u64))
            .ok_or(CollectionError::LimitExceeded("scan bytes"))?;
        if next_bytes > self.descriptor.limits.max_scan_bytes {
            return Err(CollectionError::LimitExceeded("scan bytes"));
        }
        let expected_before_hash = previous
            .as_ref()
            .map(|(_, bytes)| ContentHash::of_bytes(bytes).to_hex());
        let expected_before_content = previous
            .map(|(_, bytes)| {
                String::from_utf8(bytes).map_err(|_| schema::invalid("entry preimage encoding"))
            })
            .transpose()?;
        Ok(PublicationPlan {
            pin: CollectionSelection {
                source: self.source.clone(),
                entry_id: entry.id.clone(),
                revision,
            },
            file_write: OrderedFileWritePlan {
                ordered: true,
                writes: vec![PlannedFileWrite {
                    path: self
                        .directory
                        .path
                        .join("entries")
                        .join(format!("{}.json", entry.id)),
                    content,
                    expected_before_hash,
                    expected_before_content,
                }],
            },
        })
    }

    fn selected(&self, pin: &CollectionSelection, scope: &ScopeContext) -> Result<ScannedEntry> {
        schema::safe_id(&pin.entry_id)?;
        validate_scope_context(scope)?;
        if pin.source != self.source {
            return Err(CollectionError::StaleSelection);
        }
        let mut selected = None;
        self.scan(|record| {
            if record.entry.id == pin.entry_id {
                selected = Some(ScannedEntry {
                    entry: record.entry.clone(),
                    value: record.value.clone(),
                    revision: record.revision,
                    bytes: Vec::new(),
                });
            }
            Ok(())
        })?;
        let record = selected.ok_or_else(|| CollectionError::NotVisible(pin.entry_id.clone()))?;
        if record.entry.state != PublicationState::Active || !record.entry.scope.permits(scope) {
            return Err(CollectionError::NotVisible(pin.entry_id.clone()));
        }
        if record.revision != pin.revision {
            return Err(CollectionError::StaleSelection);
        }
        Ok(record)
    }

    fn summary(&self, record: &ScannedEntry) -> Result<EntrySummary> {
        Ok(EntrySummary {
            pin: CollectionSelection {
                source: self.source.clone(),
                entry_id: record.entry.id.clone(),
                revision: record.revision,
            },
            title: record.entry.title.clone(),
            description: record.entry.description.clone(),
            applicability: record.entry.applicability.clone(),
            scope: record.entry.scope.clone(),
            selection: normalized_selection(&record.entry.selection)?,
            requires: record.entry.requires.clone(),
        })
    }

    fn check_descriptor(&self) -> Result<()> {
        let current = safe_io::SafeDir::absolute(&self.directory.path)?;
        if !self.directory.same_identity(&current)? {
            return Err(CollectionError::ChangedDuringRead);
        }
        let read = self.directory.read("artifact.json", MAX_DESCRIPTOR_BYTES)?;
        let value: Value = serde_json::from_slice(&read.bytes)?;
        if ContentHash::of_canonical_json(&value) != self.source.content
            || parse_descriptor(&value)? != self.descriptor
        {
            return Err(CollectionError::SourceMismatch);
        }
        Ok(())
    }

    fn scan(&self, mut visit: impl FnMut(&ScannedEntry) -> Result<()>) -> Result<ScanSnapshot> {
        self.scan_inner(&mut visit).map_err(|error| match error {
            CollectionError::InvalidSchema(_)
            | CollectionError::Json(_)
            | CollectionError::UnsafePath(_)
            | CollectionError::Io { .. } => CollectionError::SourceUnavailable,
            error => error,
        })
    }

    fn scan_inner(
        &self,
        mut visit: impl FnMut(&ScannedEntry) -> Result<()>,
    ) -> Result<ScanSnapshot> {
        self.check_descriptor()?;
        let Some(directory) = self.directory.child("entries", true)? else {
            self.check_descriptor()?;
            if self.directory.child("entries", true)?.is_some() {
                return Err(CollectionError::ChangedDuringRead);
            }
            return Ok(ScanSnapshot { count: 0, bytes: 0 });
        };
        let before = directory.fingerprint()?;
        let names = directory.names(self.descriptor.limits.max_entries)?;
        let mut files = Vec::new();
        for name in &names {
            let id = name
                .strip_suffix(".json")
                .ok_or_else(|| schema::invalid(format!("unexpected collection file '{name}'")))?;
            schema::safe_id(id)?;
            files.push((id.to_string(), name));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let mut stamps = Vec::new();
        let mut scanned_bytes = 0_u64;
        for (id, name) in files {
            let remaining = self.descriptor.limits.max_scan_bytes - scanned_bytes;
            if directory.file_fingerprint(name)?.len() > remaining {
                return Err(CollectionError::LimitExceeded("scan bytes"));
            }
            let read =
                directory.read(name, self.descriptor.limits.max_entry_bytes.min(remaining))?;
            scanned_bytes = scanned_bytes
                .checked_add(read.bytes.len() as u64)
                .ok_or(CollectionError::LimitExceeded("scan bytes"))?;
            let value: Value = serde_json::from_slice(&read.bytes)?;
            let entry = parse_entry(&value)?;
            if entry.id != id {
                return Err(schema::invalid(format!("entry id does not match '{name}'")));
            }
            let revision = ContentHash::of_canonical_json(&value);
            stamps.push((name, read.fingerprint));
            visit(&ScannedEntry {
                entry,
                value,
                revision,
                bytes: read.bytes,
            })?;
        }
        if directory.names(self.descriptor.limits.max_entries)? != names
            || directory.fingerprint()? != before
        {
            return Err(CollectionError::ChangedDuringRead);
        }
        for (name, fingerprint) in stamps {
            if directory.file_fingerprint(name)? != fingerprint {
                return Err(CollectionError::ChangedDuringRead);
            }
        }
        if self
            .directory
            .child("entries", false)?
            .ok_or(CollectionError::ChangedDuringRead)?
            .fingerprint()?
            != before
        {
            return Err(CollectionError::ChangedDuringRead);
        }
        self.check_descriptor()?;
        Ok(ScanSnapshot { count: names.len(), bytes: scanned_bytes })
    }
}

fn validate_scope_context(scope: &ScopeContext) -> Result<()> {
    schema::safe_id(&scope.ee_id)?;
    schema::safe_id(&scope.worker_interface)
}

fn matches_query(entry: &CollectionEntry, query: &str) -> bool {
    query.is_empty()
        || [
            &entry.id,
            &entry.title,
            &entry.description,
            &entry.applicability,
        ]
        .into_iter()
        .any(|text| text.to_lowercase().contains(query))
}
