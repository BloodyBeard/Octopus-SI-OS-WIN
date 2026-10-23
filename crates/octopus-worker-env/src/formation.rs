use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use octopus_descriptors::{Descriptor, InterfaceId, RootAccess, WorkerEnvironmentKindId};
use serde::{Deserialize, Serialize};

use crate::{
    AbsPath, NetworkCeiling, WekDescriptorHash, WorkerEnvCaps, WorkerEnvError, WorkerEnvKindSpec,
    WorkerEnvironmentId,
};

const FORMATION_SCHEMA_VERSION: u32 = 5;
static RECORD_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
struct FormationPin<'a> {
    schema_version: u32,
    door_interface: &'a InterfaceId,
    door_descriptor_hash: &'a str,
    worker_environment_kind: &'a WorkerEnvironmentKindId,
    worker_environment_kind_descriptor_hash: &'a str,
    tool_descriptor_hashes: &'a BTreeMap<String, String>,
    instance_config_hashes: &'a BTreeMap<String, String>,
    worker: &'a InterfaceId,
    ee: &'a str,
    run: &'a str,
    granted_tools: &'a [String],
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    door_additional_root_bindings: &'a BTreeMap<String, Vec<PathBuf>>,
    door_root_bindings: &'a BTreeMap<String, PathBuf>,
    readable_root_slots: &'a [String],
    writable_root_slots: &'a [String],
    network_ceiling: NetworkCeiling,
    requires_release: bool,
}

#[derive(Clone, Debug)]
pub struct WorkerEnvironmentFormationRequest<'a> {
    pub door_interface: &'a InterfaceId,
    pub door_descriptor_hash: &'a str,
    pub door_driver_ref: &'a str,
    pub tool: &'a str,
    pub tool_descriptor_hashes: &'a BTreeMap<String, String>,
    pub tool_process_bins: &'a BTreeMap<String, String>,
    pub tool_worker_environment_kinds: &'a [WorkerEnvironmentKindId],
    pub worker: &'a InterfaceId,
    pub ee: &'a str,
    pub run: &'a str,
    pub granted_worker_environment_kinds: &'a [WorkerEnvironmentKindId],
    pub granted_tools: &'a [String],
    pub door_additional_root_bindings: &'a BTreeMap<String, Vec<PathBuf>>,
    pub door_root_bindings: &'a BTreeMap<String, PathBuf>,
    pub door_instance_dir: &'a Path,
    pub ee_root_slot_allowance: Option<&'a BTreeSet<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerEnvironmentFormation {
    pub worker_environment_id: WorkerEnvironmentId,
    pub run: String,
    pub ee: String,
    pub worker: InterfaceId,
    pub door_interface: InterfaceId,
    pub door_descriptor_hash: String,
    pub worker_environment_kind: WorkerEnvironmentKindId,
    pub worker_environment_kind_descriptor_hash: String,
    pub tool_descriptor_hashes: BTreeMap<String, String>,
    pub instance_config_hashes: BTreeMap<String, String>,
    pub formation_pin: String,
    pub granted_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub door_additional_root_bindings: BTreeMap<String, Vec<PathBuf>>,
    pub door_root_bindings: BTreeMap<String, PathBuf>,
    pub readable_root_slots: Vec<String>,
    pub writable_root_slots: Vec<String>,
    pub network_ceiling: NetworkCeiling,
    pub requires_release: bool,
}

impl WorkerEnvironmentFormation {
    pub fn descriptor_hash(&self) -> Result<WekDescriptorHash, WorkerEnvError> {
        let bytes =
            decode_hex_32(&self.worker_environment_kind_descriptor_hash).ok_or_else(|| {
                WorkerEnvError::MalformedFormation(format!(
                    "worker_environment_kind_descriptor_hash '{}' is not 32-byte hex",
                    self.worker_environment_kind_descriptor_hash
                ))
            })?;
        Ok(WekDescriptorHash(bytes))
    }

    pub fn caps(&self) -> WorkerEnvCaps {
        WorkerEnvCaps::from_formed_root_slots(
            self.readable_root_slots.iter().cloned().collect(),
            self.writable_root_slots.iter().cloned().collect(),
            self.network_ceiling,
        )
    }

    pub fn scope_key(&self) -> String {
        format!(
            "{}\0{}\0{}\0{}",
            self.door_interface, self.worker_environment_kind, self.ee, self.run
        )
    }

    pub fn validate(&self) -> Result<(), WorkerEnvError> {
        for (field, value) in [
            ("run", self.run.as_str()),
            ("ee", self.ee.as_str()),
            ("worker", self.worker.as_str()),
            ("door_interface", self.door_interface.as_str()),
            ("door_descriptor_hash", self.door_descriptor_hash.as_str()),
        ] {
            validate_identity(field, value)?;
        }
        self.descriptor_hash()?;
        validate_sorted_unique("granted_tools", &self.granted_tools, true)?;
        validate_sorted_unique("readable_root_slots", &self.readable_root_slots, false)?;
        validate_sorted_unique("writable_root_slots", &self.writable_root_slots, false)?;
        let tool_keys = self
            .tool_descriptor_hashes
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        if tool_keys != self.granted_tools {
            return Err(WorkerEnvError::MalformedFormation(
                "tool_descriptor_hashes keys must exactly equal granted_tools".to_string(),
            ));
        }
        for (tool, hash) in &self.tool_descriptor_hashes {
            validate_identity("tool_descriptor_hashes key", tool)?;
            validate_non_empty_hash("tool_descriptor_hashes value", hash)?;
        }
        for (name, hash) in &self.instance_config_hashes {
            validate_instance_config_name(name)?;
            validate_non_empty_hash("instance_config_hashes value", hash)?;
        }
        for (name, path) in &self.door_root_bindings {
            validate_identity("door_root_bindings key", name)?;
            let normalized = AbsPath::new(path)?;
            if normalized.as_path() != path {
                return Err(WorkerEnvError::MalformedFormation(format!(
                    "door root binding '{name}' is not lexically canonical: {}",
                    path.display()
                )));
            }
        }
        for (slot, paths) in &self.door_additional_root_bindings {
            let primary = self.door_root_bindings.get(slot).ok_or_else(|| WorkerEnvError::MalformedFormation(format!("additional roots have no primary slot '{slot}'")))?;
            let mut seen = BTreeSet::from([primary.clone()]);
            for path in paths {
                if AbsPath::new(path)?.as_path() != path || !seen.insert(path.clone()) {
                    return Err(WorkerEnvError::MalformedFormation(format!("additional roots for '{slot}' are not canonical and unique")));
                }
            }
        }
        for slot in self
            .readable_root_slots
            .iter()
            .chain(self.writable_root_slots.iter())
        {
            if !self.door_root_bindings.contains_key(slot) {
                return Err(WorkerEnvError::MalformedFormation(format!(
                    "formed root slot '{slot}' has no concrete door binding"
                )));
            }
        }
        let expected_pin = formation_pin_digest(&FormationPin {
            schema_version: FORMATION_SCHEMA_VERSION,
            door_interface: &self.door_interface,
            door_descriptor_hash: &self.door_descriptor_hash,
            worker_environment_kind: &self.worker_environment_kind,
            worker_environment_kind_descriptor_hash: &self.worker_environment_kind_descriptor_hash,
            tool_descriptor_hashes: &self.tool_descriptor_hashes,
            instance_config_hashes: &self.instance_config_hashes,
            worker: &self.worker,
            ee: &self.ee,
            run: &self.run,
            granted_tools: &self.granted_tools,
            door_additional_root_bindings: &self.door_additional_root_bindings,
            door_root_bindings: &self.door_root_bindings,
            readable_root_slots: &self.readable_root_slots,
            writable_root_slots: &self.writable_root_slots,
            network_ceiling: self.network_ceiling,
            requires_release: self.requires_release,
        })?;
        if self.formation_pin != expected_pin {
            return Err(WorkerEnvError::MalformedFormation(format!(
                "formation pin mismatch: expected {expected_pin}, observed {}",
                self.formation_pin
            )));
        }
        let expected_id = WorkerEnvironmentId::derive(expected_pin.as_bytes());
        if self.worker_environment_id != expected_id {
            return Err(WorkerEnvError::MalformedFormation(format!(
                "worker_environment_id '{}' does not match formation pin",
                self.worker_environment_id
            )));
        }
        Ok(())
    }

    pub fn validate_against_kind(&self, kind: &WorkerEnvKindSpec) -> Result<(), WorkerEnvError> {
        self.validate()?;
        kind.validate()
            .map_err(WorkerEnvError::MalformedFormation)?;
        if self.worker_environment_kind != kind.worker_environment_kind_id {
            return Err(WorkerEnvError::MalformedFormation(format!(
                "formation kind '{}' does not match descriptor '{}'",
                self.worker_environment_kind, kind.worker_environment_kind_id
            )));
        }
        let kind_value = serde_json::to_value(kind).map_err(|error| {
            WorkerEnvError::MalformedFormation(format!(
                "worker-environment-kind descriptor cannot be encoded: {error}"
            ))
        })?;
        let kind_hash =
            WekDescriptorHash::of_bytes(&octopus_descriptors::canonical_bytes(&kind_value));
        if self.worker_environment_kind_descriptor_hash != kind_hash.to_hex() {
            return Err(WorkerEnvError::UnknownDescriptorHash(kind_hash));
        }
        let expected_configs = kind
            .instance_config_files
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let formed_configs = self
            .instance_config_hashes
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        if expected_configs != formed_configs {
            return Err(WorkerEnvError::MalformedFormation(format!(
                "formed instance configs do not match kind '{}': expected {:?}, observed {:?}",
                kind.worker_environment_kind_id, expected_configs, formed_configs
            )));
        }
        if self.requires_release != kind.lifecycle.requires_release {
            return Err(WorkerEnvError::MalformedFormation(format!(
                "formed release lifecycle does not match kind '{}'",
                kind.worker_environment_kind_id
            )));
        }
        if self
            .network_ceiling
            .min(kind.boundary_defaults.network_ceiling)
            != self.network_ceiling
        {
            return Err(WorkerEnvError::MalformedFormation(format!(
                "formed network ceiling widens kind '{}'",
                kind.worker_environment_kind_id
            )));
        }
        let readable_boundary = root_names(
            &kind.boundary_defaults.read_root_refs,
            Some(RootAccess::Read),
        );
        let writable_boundary = root_names(
            &kind.boundary_defaults.write_root_refs,
            Some(RootAccess::Write),
        );
        let protected_boundary =
            root_names(&kind.boundary_defaults.protected_read_only_subpaths, None);
        if self
            .readable_root_slots
            .iter()
            .any(|slot| !readable_boundary.contains(slot))
            || self
                .writable_root_slots
                .iter()
                .any(|slot| !writable_boundary.contains(slot))
        {
            return Err(WorkerEnvError::MalformedFormation(format!(
                "formed root caps widen kind '{}'",
                kind.worker_environment_kind_id
            )));
        }
        let allowed_bindings = readable_boundary
            .union(&writable_boundary)
            .cloned()
            .collect::<BTreeSet<_>>()
            .union(&protected_boundary)
            .cloned()
            .collect::<BTreeSet<_>>();
        if self
            .door_root_bindings
            .keys()
            .any(|slot| !allowed_bindings.contains(slot))
        {
            return Err(WorkerEnvError::MalformedFormation(format!(
                "formed door roots contain a slot outside kind '{}' boundary",
                kind.worker_environment_kind_id
            )));
        }
        Ok(())
    }

    pub fn attest_door_root_bindings(
        &self,
        provided: &BTreeMap<String, PathBuf>,
    ) -> Result<(), WorkerEnvError> {
        for (slot, expected) in &self.door_root_bindings {
            let observed = provided.get(slot).ok_or_else(|| {
                WorkerEnvError::MalformedFormation(format!(
                    "formed door root slot '{slot}' is no longer provided"
                ))
            })?;
            if observed != expected {
                return Err(WorkerEnvError::MalformedFormation(format!(
                    "formed door root slot '{slot}' changed from '{}' to '{}'",
                    expected.display(),
                    observed.display()
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerEnvironmentLifecycle {
    Active,
    Released,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerEnvironmentRecord {
    pub schema_version: u32,
    pub formation: WorkerEnvironmentFormation,
    pub lifecycle: WorkerEnvironmentLifecycle,
    pub opened_at_movement_seq: u64,
    pub last_activity_movement_seq: u64,
}

impl WorkerEnvironmentRecord {
    pub fn validate(&self) -> Result<(), WorkerEnvError> {
        if self.schema_version != FORMATION_SCHEMA_VERSION {
            return Err(WorkerEnvError::RecordFailed {
                path: "<worker-environment-record>".to_string(),
                reason: format!(
                    "unsupported schema_version {}, expected {}",
                    self.schema_version, FORMATION_SCHEMA_VERSION
                ),
            });
        }
        self.formation.validate()?;
        if self.last_activity_movement_seq < self.opened_at_movement_seq {
            return Err(WorkerEnvError::RecordFailed {
                path: "<worker-environment-record>".to_string(),
                reason: "last activity movement precedes the opening movement".to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerEnvironmentSnapshot {
    pub record: WorkerEnvironmentRecord,
    pub live_state: Option<serde_json::Value>,
}

#[derive(Clone, Debug)]
pub struct WorkerEnvironmentRecordStore {
    root: PathBuf,
}

impl WorkerEnvironmentRecordStore {
    pub fn new(interface_var_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: interface_var_dir.into().join("worker-environments"),
        }
    }

    pub fn activate(
        &self,
        formation: &WorkerEnvironmentFormation,
        source_movement_seq: u64,
    ) -> Result<PathBuf, WorkerEnvError> {
        self.write_formation_lifecycle(
            formation,
            source_movement_seq,
            WorkerEnvironmentLifecycle::Active,
        )
    }

    pub fn fail(
        &self,
        formation: &WorkerEnvironmentFormation,
        source_movement_seq: u64,
    ) -> Result<PathBuf, WorkerEnvError> {
        self.write_formation_lifecycle(
            formation,
            source_movement_seq,
            WorkerEnvironmentLifecycle::Failed,
        )
    }

    pub fn release(
        &self,
        formation: &WorkerEnvironmentFormation,
        source_movement_seq: u64,
    ) -> Result<PathBuf, WorkerEnvError> {
        let path = self.record_path(&formation.worker_environment_id);
        if path.exists() {
            let existing = self.read(&formation.worker_environment_id)?;
            if existing.formation != *formation {
                return Err(WorkerEnvError::RecordFailed {
                    path: path.display().to_string(),
                    reason: "record formation does not match its worker_environment_id".to_string(),
                });
            }
            if !matches!(
                existing.lifecycle,
                WorkerEnvironmentLifecycle::Active
                    | WorkerEnvironmentLifecycle::Released
                    | WorkerEnvironmentLifecycle::Failed
            ) {
                return Err(WorkerEnvError::RecordFailed {
                    path: path.display().to_string(),
                    reason: format!(
                        "invalid lifecycle transition {:?} -> {:?}",
                        existing.lifecycle,
                        WorkerEnvironmentLifecycle::Released
                    ),
                });
            }
        }
        let path = self.write_formation_lifecycle(
            formation,
            source_movement_seq,
            WorkerEnvironmentLifecycle::Released,
        )?;
        self.remove_live_state(&formation.worker_environment_id)?;
        Ok(path)
    }

    pub fn set_lifecycle(
        &self,
        id: &WorkerEnvironmentId,
        lifecycle: WorkerEnvironmentLifecycle,
    ) -> Result<(), WorkerEnvError> {
        let path = self.record_path(id);
        let mut record = self.read(id)?;
        let resumable_release = record.lifecycle == WorkerEnvironmentLifecycle::Released
            && lifecycle == WorkerEnvironmentLifecycle::Active
            && !record.formation.requires_release;
        let allowed = record.lifecycle == lifecycle
            || resumable_release
            || matches!(
                (record.lifecycle, lifecycle),
                (
                    WorkerEnvironmentLifecycle::Active,
                    WorkerEnvironmentLifecycle::Released | WorkerEnvironmentLifecycle::Failed
                )
            );
        if !allowed {
            return Err(WorkerEnvError::RecordFailed {
                path: path.display().to_string(),
                reason: format!(
                    "invalid lifecycle transition {:?} -> {:?}",
                    record.lifecycle, lifecycle
                ),
            });
        }
        record.lifecycle = lifecycle;
        record.validate()?;
        self.write_record(&path, &record)?;
        if matches!(
            lifecycle,
            WorkerEnvironmentLifecycle::Released | WorkerEnvironmentLifecycle::Failed
        ) {
            self.remove_live_state(id)?;
        }
        Ok(())
    }

    pub fn write_live_state(
        &self,
        id: &WorkerEnvironmentId,
        state: &serde_json::Value,
    ) -> Result<PathBuf, WorkerEnvError> {
        if !state.is_object() {
            return Err(WorkerEnvError::RecordFailed {
                path: self.live_state_path(id).display().to_string(),
                reason: "live state must be a JSON object".to_string(),
            });
        }
        let record = self.read(id)?;
        if !matches!(
            record.lifecycle,
            WorkerEnvironmentLifecycle::Active | WorkerEnvironmentLifecycle::Failed
        ) {
            return Err(WorkerEnvError::RecordFailed {
                path: self.live_state_path(id).display().to_string(),
                reason: format!(
                    "cannot publish live state for {:?} worker environment",
                    record.lifecycle
                ),
            });
        }
        let path = self.live_state_path(id);
        write_json_atomic(&path, state)?;
        Ok(path)
    }

    pub fn read(
        &self,
        id: &WorkerEnvironmentId,
    ) -> Result<WorkerEnvironmentRecord, WorkerEnvError> {
        let path = self.record_path(id);
        let record = self.read_record(&path)?;
        if record.formation.worker_environment_id != *id {
            return Err(WorkerEnvError::RecordFailed {
                path: path.display().to_string(),
                reason: format!(
                    "record identity '{}' does not match directory '{}'",
                    record.formation.worker_environment_id, id
                ),
            });
        }
        Ok(record)
    }

    pub fn read_live_state(
        &self,
        id: &WorkerEnvironmentId,
    ) -> Result<Option<serde_json::Value>, WorkerEnvError> {
        let path = self.live_state_path(id);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(WorkerEnvError::RecordFailed {
                    path: path.display().to_string(),
                    reason: error.to_string(),
                });
            }
        };
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|error| WorkerEnvError::RecordFailed {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        if !value.is_object() {
            return Err(WorkerEnvError::RecordFailed {
                path: path.display().to_string(),
                reason: "live state must be a JSON object".to_string(),
            });
        }
        Ok(Some(value))
    }

    pub fn snapshots(&self) -> Result<Vec<WorkerEnvironmentSnapshot>, WorkerEnvError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(WorkerEnvError::RecordFailed {
                    path: self.root.display().to_string(),
                    reason: error.to_string(),
                });
            }
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| WorkerEnvError::RecordFailed {
                path: self.root.display().to_string(),
                reason: error.to_string(),
            })?;
            let file_type = entry
                .file_type()
                .map_err(|error| WorkerEnvError::RecordFailed {
                    path: entry.path().display().to_string(),
                    reason: error.to_string(),
                })?;
            if !file_type.is_dir() {
                return Err(WorkerEnvError::RecordFailed {
                    path: entry.path().display().to_string(),
                    reason: "worker-environment store contains a non-directory entry".to_string(),
                });
            }
            let name =
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| WorkerEnvError::RecordFailed {
                        path: entry.path().display().to_string(),
                        reason: "worker-environment directory name is not valid UTF-8".to_string(),
                    })?;
            ids.push(WorkerEnvironmentId::parse(name)?);
        }
        ids.sort();
        ids.into_iter()
            .map(|id| {
                Ok(WorkerEnvironmentSnapshot {
                    record: self.read(&id)?,
                    live_state: self.read_live_state(&id)?,
                })
            })
            .collect()
    }

    pub fn record_path(&self, id: &WorkerEnvironmentId) -> PathBuf {
        self.root.join(id.as_str()).join("worker_environment.json")
    }

    pub fn live_state_path(&self, id: &WorkerEnvironmentId) -> PathBuf {
        self.root.join(id.as_str()).join("live_state.json")
    }

    fn read_record(&self, path: &Path) -> Result<WorkerEnvironmentRecord, WorkerEnvError> {
        let bytes = std::fs::read(path).map_err(|error| WorkerEnvError::RecordFailed {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
        let record: WorkerEnvironmentRecord =
            serde_json::from_slice(&bytes).map_err(|error| WorkerEnvError::RecordFailed {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        record
            .validate()
            .map_err(|error| WorkerEnvError::RecordFailed {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        Ok(record)
    }

    fn write_record(
        &self,
        path: &Path,
        record: &WorkerEnvironmentRecord,
    ) -> Result<(), WorkerEnvError> {
        let value = serde_json::to_value(record).map_err(|error| WorkerEnvError::RecordFailed {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
        write_json_atomic(path, &value)
    }

    fn write_formation_lifecycle(
        &self,
        formation: &WorkerEnvironmentFormation,
        source_movement_seq: u64,
        lifecycle: WorkerEnvironmentLifecycle,
    ) -> Result<PathBuf, WorkerEnvError> {
        formation.validate()?;
        let path = self.record_path(&formation.worker_environment_id);
        let existing = if path.exists() {
            Some(self.read(&formation.worker_environment_id)?)
        } else {
            None
        };
        if existing
            .as_ref()
            .is_some_and(|record| record.formation != *formation)
        {
            return Err(WorkerEnvError::RecordFailed {
                path: path.display().to_string(),
                reason: "record formation does not match its worker_environment_id".to_string(),
            });
        }
        let resumable_release =
            lifecycle == WorkerEnvironmentLifecycle::Active && !formation.requires_release;
        if existing.as_ref().is_some_and(|record| {
            record.lifecycle == WorkerEnvironmentLifecycle::Released
                && lifecycle != WorkerEnvironmentLifecycle::Released
                && !resumable_release
        }) {
            return Err(WorkerEnvError::RecordFailed {
                path: path.display().to_string(),
                reason: format!(
                    "invalid lifecycle transition {:?} -> {:?}",
                    WorkerEnvironmentLifecycle::Released,
                    lifecycle
                ),
            });
        }
        if lifecycle == WorkerEnvironmentLifecycle::Failed
            || existing.as_ref().is_some_and(|record| {
                record.lifecycle != lifecycle && lifecycle == WorkerEnvironmentLifecycle::Active
            })
        {
            self.remove_live_state(&formation.worker_environment_id)?;
        }
        let record = WorkerEnvironmentRecord {
            schema_version: FORMATION_SCHEMA_VERSION,
            formation: formation.clone(),
            lifecycle,
            opened_at_movement_seq: existing
                .as_ref()
                .map_or(source_movement_seq, |record| record.opened_at_movement_seq),
            last_activity_movement_seq: existing.as_ref().map_or(source_movement_seq, |record| {
                record.last_activity_movement_seq.max(source_movement_seq)
            }),
        };
        record.validate()?;
        if existing.as_ref() == Some(&record) {
            return Ok(path);
        }
        self.write_record(&path, &record)?;
        Ok(path)
    }

    fn remove_live_state(&self, id: &WorkerEnvironmentId) -> Result<(), WorkerEnvError> {
        let path = self.live_state_path(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(WorkerEnvError::RecordFailed {
                path: path.display().to_string(),
                reason: error.to_string(),
            }),
        }
    }
}

pub(crate) fn formation_pin(
    request: &WorkerEnvironmentFormationRequest<'_>,
    kind: &WorkerEnvironmentKindId,
    kind_hash: &WekDescriptorHash,
    granted_tools: &[String],
    tool_descriptor_hashes: &BTreeMap<String, String>,
    instance_config_hashes: &BTreeMap<String, String>,
    door_root_bindings: &BTreeMap<String, PathBuf>,
    door_additional_root_bindings: &BTreeMap<String, Vec<PathBuf>>,
    caps: &WorkerEnvCaps,
    requires_release: bool,
) -> Result<(String, WorkerEnvironmentId), WorkerEnvError> {
    let kind_hash = kind_hash.to_hex();
    let readable_root_slots = caps.readable_root_names().into_iter().collect::<Vec<_>>();
    let writable_root_slots = caps.writable_root_names().into_iter().collect::<Vec<_>>();
    let pin = FormationPin {
        schema_version: FORMATION_SCHEMA_VERSION,
        door_interface: request.door_interface,
        door_descriptor_hash: request.door_descriptor_hash,
        worker_environment_kind: kind,
        worker_environment_kind_descriptor_hash: &kind_hash,
        tool_descriptor_hashes,
        instance_config_hashes,
        worker: request.worker,
        ee: request.ee,
        run: request.run,
        granted_tools,
        door_root_bindings,
        door_additional_root_bindings,
        readable_root_slots: &readable_root_slots,
        writable_root_slots: &writable_root_slots,
        network_ceiling: caps.network(),
        requires_release,
    };
    let digest = formation_pin_digest(&pin)?;
    let id = WorkerEnvironmentId::derive(digest.as_bytes());
    Ok((digest, id))
}

fn formation_pin_digest(pin: &FormationPin<'_>) -> Result<String, WorkerEnvError> {
    let value = serde_json::to_value(pin).map_err(|error| WorkerEnvError::RecordFailed {
        path: "<formation-pin>".to_string(),
        reason: error.to_string(),
    })?;
    let bytes = octopus_descriptors::canonical_bytes(&value);
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn validate_identity(field: &str, value: &str) -> Result<(), WorkerEnvError> {
    if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(WorkerEnvError::MalformedFormation(format!(
            "{field} is empty or malformed"
        )));
    }
    Ok(())
}

fn validate_sorted_unique(
    field: &str,
    values: &[String],
    require_non_empty: bool,
) -> Result<(), WorkerEnvError> {
    if require_non_empty && values.is_empty() {
        return Err(WorkerEnvError::MalformedFormation(format!(
            "{field} must not be empty"
        )));
    }
    for value in values {
        validate_identity(field, value)?;
    }
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(WorkerEnvError::MalformedFormation(format!(
            "{field} must be sorted and unique"
        )));
    }
    Ok(())
}

fn validate_non_empty_hash(field: &str, hash: &str) -> Result<(), WorkerEnvError> {
    if hash.trim().is_empty() {
        return Err(WorkerEnvError::MalformedFormation(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

fn validate_instance_config_name(name: &str) -> Result<(), WorkerEnvError> {
    if name.is_empty()
        || name.trim() != name
        || !name.ends_with(".json")
        || name.contains('/')
        || name.contains('\\')
    {
        return Err(WorkerEnvError::MalformedFormation(format!(
            "instance config name is malformed: {name:?}"
        )));
    }
    Ok(())
}

fn root_names(
    roots: &[octopus_descriptors::RootRef],
    required: Option<RootAccess>,
) -> BTreeSet<String> {
    roots
        .iter()
        .filter(|root| {
            required.is_none_or(|access| root.access.is_empty() || root.access.contains(&access))
        })
        .map(|root| root.path.clone())
        .collect()
}

fn write_json_atomic(path: &Path, value: &serde_json::Value) -> Result<(), WorkerEnvError> {
    let parent = path.parent().ok_or_else(|| WorkerEnvError::RecordFailed {
        path: path.display().to_string(),
        reason: "record path has no parent".to_string(),
    })?;
    std::fs::create_dir_all(parent).map_err(|error| WorkerEnvError::RecordFailed {
        path: parent.display().to_string(),
        reason: error.to_string(),
    })?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| WorkerEnvError::RecordFailed {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let sequence = RECORD_TEMP_SEQUENCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| WorkerEnvError::RecordFailed {
            path: path.display().to_string(),
            reason: "worker-environment temporary-file sequence overflowed".to_string(),
        })?;
    let name = path
        .file_name()
        .ok_or_else(|| WorkerEnvError::RecordFailed {
            path: path.display().to_string(),
            reason: "record path has no file name".to_string(),
        })?;
    let mut temporary_name = std::ffi::OsString::from(".");
    temporary_name.push(name);
    temporary_name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
    let temporary = parent.join(temporary_name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| WorkerEnvError::RecordFailed {
            path: temporary.display().to_string(),
            reason: error.to_string(),
        })?;
    let result = (|| {
        use std::io::Write;

        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| WorkerEnvError::RecordFailed {
                path: temporary.display().to_string(),
                reason: error.to_string(),
            })?;
        std::fs::rename(&temporary, path).map_err(|error| WorkerEnvError::RecordFailed {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| WorkerEnvError::RecordFailed {
                path: parent.display().to_string(),
                reason: error.to_string(),
            })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn decode_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}
