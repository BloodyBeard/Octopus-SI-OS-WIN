use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use octopus_worker_env::{WorkerEnvironmentFormation, WorkerEnvironmentId};
use serde::{Deserialize, Serialize};

const SESSION_FILE: &str = "session.json";
const PROCESSES_DIR: &str = "processes";
const HISTORY_DIR: &str = "history";
const STATE_LOCK_FILE: &str = ".state.lock";
const MAX_RECORD_BYTES: u64 = 512 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteRoomIdentity {
    pub schema_version: u32,
    pub worker_environment_id: WorkerEnvironmentId,
    pub formation_pin: String,
    pub door_descriptor_hash: String,
    pub worker_environment_kind_descriptor_hash: String,
    pub websocket_url: String,
    pub workspace_uri: String,
    pub auth_identity: String,
    pub config_identity: String,
}

impl RemoteRoomIdentity {
    pub(crate) fn new(
        formation: &WorkerEnvironmentFormation,
        websocket_url: String,
        workspace_uri: String,
        auth_identity: String,
        config_identity: String,
    ) -> Self {
        Self {
            schema_version: 1,
            worker_environment_id: formation.worker_environment_id.clone(),
            formation_pin: formation.formation_pin.clone(),
            door_descriptor_hash: formation.door_descriptor_hash.clone(),
            worker_environment_kind_descriptor_hash: formation
                .worker_environment_kind_descriptor_hash
                .clone(),
            websocket_url,
            workspace_uri,
            auth_identity,
            config_identity,
        }
    }

    pub(crate) fn session_claim_id(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("remote room identity cannot encode: {error}"))?;
        Ok(format!("octopus-{}", blake3::hash(&bytes).to_hex()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DurableSessionStatus {
    Claiming,
    Active,
    Lost,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DurableProcessStatus {
    Starting,
    Active,
    Lost,
    Terminal,
    Resolved,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LossTombstone {
    pub code: String,
    pub observed_unix_ms: u64,
    pub detail_blake3: String,
}

impl LossTombstone {
    pub(crate) fn new(code: &str, detail: &str, observed_unix_ms: u64) -> Self {
        Self {
            code: code.to_string(),
            observed_unix_ms,
            detail_blake3: blake3::hash(detail.as_bytes()).to_hex().to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DurableSessionRecord {
    pub schema_version: u32,
    pub identity: RemoteRoomIdentity,
    pub session_id: String,
    pub status: DurableSessionStatus,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub loss: Option<LossTombstone>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DurableProcessRecord {
    pub schema_version: u32,
    pub identity: RemoteRoomIdentity,
    pub session_id: String,
    pub process_id: String,
    pub remote_process_id: String,
    pub generation: u64,
    pub argv: Vec<String>,
    pub cwd: String,
    pub tty: bool,
    pub pipe_stdin: bool,
    pub started_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub status: DurableProcessStatus,
    pub exit_code: Option<i32>,
    pub loss: Option<LossTombstone>,
    pub resolution: Option<String>,
}

pub(crate) struct RecoveryStore {
    root: PathBuf,
    root_dir: std::fs::File,
    processes_dir: std::fs::File,
    history_dir: std::fs::File,
    identity: RemoteRoomIdentity,
    mutation: Mutex<()>,
    state_lock: std::fs::File,
}

struct RecoveryStateGuard<'a> {
    _mutation: MutexGuard<'a, ()>,
    state_lock: &'a std::fs::File,
}

#[cfg(unix)]
impl Drop for RecoveryStateGuard<'_> {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        let _ = unsafe { libc::flock(self.state_lock.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl RecoveryStore {
    pub(crate) fn open(root: &Path, identity: RemoteRoomIdentity) -> Result<Self, String> {
        ensure_private_directory(root)?;
        let durable_root_dir = open_private_directory_fd(root)?;
        let store_root = root.join("remote-exec");
        let root_dir = ensure_private_subdirectory_at(&durable_root_dir, root, "remote-exec")?;
        let processes_dir = ensure_private_subdirectory_at(&root_dir, &store_root, PROCESSES_DIR)?;
        let history_dir = ensure_private_subdirectory_at(&root_dir, &store_root, HISTORY_DIR)?;
        let state_lock = open_state_lock(&root_dir, &store_root)?;
        let store = Self {
            root: store_root,
            root_dir,
            processes_dir,
            history_dir,
            identity,
            mutation: Mutex::new(()),
            state_lock,
        };
        store.validate_all_state()?;
        Ok(store)
    }

    pub(crate) fn identity(&self) -> &RemoteRoomIdentity {
        &self.identity
    }

    pub(crate) fn load_session(&self) -> Result<Option<DurableSessionRecord>, String> {
        let _guard = self.lock_state()?;
        self.load_session_validated_unlocked()
    }

    fn load_session_validated_unlocked(&self) -> Result<Option<DurableSessionRecord>, String> {
        let session = self.load_session_unchecked()?;
        if let Some(session) = &session {
            self.validate_session(session)?;
        }
        Ok(session)
    }

    pub(crate) fn activate_session(
        &self,
        session_id: String,
        now: u64,
    ) -> Result<DurableSessionRecord, String> {
        let _guard = self.lock_state()?;
        if session_id.is_empty() {
            return Err("remote exec durable session id is empty".to_string());
        }
        let mut created_unix_ms = now;
        if let Some(previous) = self.load_session_validated_unlocked()? {
            if previous.status == DurableSessionStatus::Released {
                return Err(
                    "remote exec released durable session cannot be activated again".to_string(),
                );
            }
            self.finish_completed_process_records_unlocked()?;
            if previous.session_id != session_id && !self.load_processes_unlocked()?.is_empty() {
                return Err(
                    "remote exec durable session cannot change while process records remain"
                        .to_string(),
                );
            }
            if previous.status == DurableSessionStatus::Active && previous.session_id != session_id
            {
                return Err(
                    "remote exec server returned a different session for an active durable room"
                        .to_string(),
                );
            }
            if previous.status == DurableSessionStatus::Claiming
                && previous.session_id != session_id
            {
                return Err(
                    "remote exec server returned a different session than the durable claim"
                        .to_string(),
                );
            }
            if previous.session_id == session_id {
                created_unix_ms = previous.created_unix_ms;
            }
            if previous.session_id != session_id || previous.status != DurableSessionStatus::Active
            {
                self.archive_session(&previous)?;
            }
        }
        let record = DurableSessionRecord {
            schema_version: 1,
            identity: self.identity.clone(),
            session_id,
            status: DurableSessionStatus::Active,
            created_unix_ms,
            updated_unix_ms: now,
            loss: None,
        };
        write_json_at(&self.root_dir, &self.root, SESSION_FILE, &record)?;
        Ok(record)
    }

    pub(crate) fn begin_session_claim(
        &self,
        session_id: String,
        now: u64,
    ) -> Result<DurableSessionRecord, String> {
        let _guard = self.lock_state()?;
        if session_id.is_empty() {
            return Err("remote exec durable session claim id is empty".to_string());
        }
        self.finish_completed_process_records_unlocked()?;
        if !self.load_processes_unlocked()?.is_empty() {
            return Err(
                "remote exec session claim cannot begin while process records remain".to_string(),
            );
        }
        if let Some(previous) = self.load_session_validated_unlocked()? {
            if previous.status == DurableSessionStatus::Released {
                return Err(
                    "remote exec released durable session cannot be claimed again".to_string(),
                );
            }
            if previous.status == DurableSessionStatus::Active {
                return Err(
                    "remote exec active durable session cannot begin another claim".to_string(),
                );
            }
            if previous.status == DurableSessionStatus::Claiming
                && previous.session_id == session_id
            {
                return Ok(previous);
            }
            self.archive_session(&previous)?;
        }
        let record = DurableSessionRecord {
            schema_version: 1,
            identity: self.identity.clone(),
            session_id,
            status: DurableSessionStatus::Claiming,
            created_unix_ms: now,
            updated_unix_ms: now,
            loss: None,
        };
        write_json_at(&self.root_dir, &self.root, SESSION_FILE, &record)?;
        Ok(record)
    }

    pub(crate) fn mark_session_lost(
        &self,
        expected_session_id: &str,
        code: &str,
        detail: &str,
        now: u64,
    ) -> Result<Option<DurableSessionRecord>, String> {
        let _guard = self.lock_state()?;
        let Some(mut record) = self.load_session_validated_unlocked()? else {
            return Ok(None);
        };
        if record.session_id != expected_session_id {
            return Err("remote exec session generation changed before loss recording".to_string());
        }
        if record.status == DurableSessionStatus::Released {
            return Err("remote exec released durable session cannot become lost".to_string());
        }
        record.status = DurableSessionStatus::Lost;
        record.updated_unix_ms = now;
        record.loss = Some(LossTombstone::new(code, detail, now));
        write_json_at(&self.root_dir, &self.root, SESSION_FILE, &record)?;
        Ok(Some(record))
    }

    pub(crate) fn release_session(
        &self,
        expected_session_id: &str,
        now: u64,
    ) -> Result<(), String> {
        let _guard = self.lock_state()?;
        self.finish_completed_process_records_unlocked()?;
        if self.load_processes_unlocked()?.iter().any(|record| {
            matches!(
                record.status,
                DurableProcessStatus::Starting
                    | DurableProcessStatus::Active
                    | DurableProcessStatus::Lost
            )
        }) {
            return Err(
                "remote exec durable session cannot release while process state is unresolved"
                    .to_string(),
            );
        }
        let Some(mut record) = self.load_session_validated_unlocked()? else {
            return Ok(());
        };
        if record.session_id != expected_session_id {
            return Err("remote exec session generation changed before release".to_string());
        }
        if record.status == DurableSessionStatus::Released {
            return Ok(());
        }
        record.status = DurableSessionStatus::Released;
        record.updated_unix_ms = now;
        write_json_at(&self.root_dir, &self.root, SESSION_FILE, &record)
    }

    pub(crate) fn begin_process(
        &self,
        process_id: &str,
        remote_process_id: &str,
        generation: u64,
        max_processes: usize,
        argv: Vec<String>,
        cwd: String,
        tty: bool,
        pipe_stdin: bool,
        now: u64,
    ) -> Result<DurableProcessRecord, String> {
        let _guard = self.lock_state()?;
        validate_process_id(process_id)?;
        if remote_process_id.is_empty() {
            return Err("remote process id is empty".to_string());
        }
        if argv.is_empty() {
            return Err("remote process argv is empty".to_string());
        }
        if max_processes == 0 {
            return Err("remote process limit must be positive".to_string());
        }
        let session = self
            .load_session_validated_unlocked()?
            .ok_or_else(|| "remote process cannot start without a durable session".to_string())?;
        if session.status != DurableSessionStatus::Active {
            return Err(
                "remote process cannot start outside an active durable session".to_string(),
            );
        }
        let processes = self.load_processes_unlocked()?;
        let active_processes = processes
            .iter()
            .filter(|record| {
                matches!(
                    record.status,
                    DurableProcessStatus::Starting
                        | DurableProcessStatus::Active
                        | DurableProcessStatus::Lost
                )
            })
            .count();
        if active_processes >= max_processes {
            return Err(format!("remote process limit {max_processes} is exhausted"));
        }
        if let Some(other) = processes.iter().find(|record| {
            record.remote_process_id == remote_process_id && record.process_id != process_id
        }) {
            return Err(format!(
                "remote process id '{remote_process_id}' is already claimed by logical process '{}'",
                other.process_id
            ));
        }
        if let Some(previous) = self.load_process_unlocked(process_id)? {
            match previous.status {
                DurableProcessStatus::Starting
                | DurableProcessStatus::Active
                | DurableProcessStatus::Lost => {
                    return Err(format!(
                        "remote process '{process_id}' has unresolved durable state {:?}; call remote.process_terminate to resolve it before starting that logical process again",
                        previous.status
                    ));
                }
                DurableProcessStatus::Terminal | DurableProcessStatus::Resolved => {
                    self.archive_process(&previous)?;
                    self.remove_process_record(process_id)?;
                }
            }
        }
        let record = DurableProcessRecord {
            schema_version: 2,
            identity: self.identity.clone(),
            session_id: session.session_id,
            process_id: process_id.to_string(),
            remote_process_id: remote_process_id.to_string(),
            generation,
            argv,
            cwd,
            tty,
            pipe_stdin,
            started_unix_ms: now,
            updated_unix_ms: now,
            status: DurableProcessStatus::Starting,
            exit_code: None,
            loss: None,
            resolution: None,
        };
        self.write_process_record(&record)?;
        Ok(record)
    }

    pub(crate) fn mark_process_active(
        &self,
        process_id: &str,
        expected_session_id: &str,
        expected_remote_process_id: &str,
        expected_generation: u64,
        now: u64,
    ) -> Result<DurableProcessRecord, String> {
        let _guard = self.lock_state()?;
        let mut record = self.require_process_generation_unlocked(
            process_id,
            expected_session_id,
            expected_remote_process_id,
            expected_generation,
        )?;
        if !matches!(
            record.status,
            DurableProcessStatus::Starting | DurableProcessStatus::Active
        ) {
            return Err(format!(
                "remote process '{process_id}' cannot become active from {:?}",
                record.status
            ));
        }
        let session = self
            .load_session_validated_unlocked()?
            .ok_or_else(|| "remote process has no durable session".to_string())?;
        if session.status != DurableSessionStatus::Active || session.session_id != record.session_id
        {
            return Err(format!(
                "remote process '{process_id}' cannot become active outside its exact durable session"
            ));
        }
        record.status = DurableProcessStatus::Active;
        record.updated_unix_ms = now;
        record.loss = None;
        self.write_process_record(&record)?;
        Ok(record)
    }

    pub(crate) fn mark_process_lost(
        &self,
        process_id: &str,
        expected_session_id: &str,
        expected_remote_process_id: &str,
        expected_generation: u64,
        code: &str,
        detail: &str,
        now: u64,
    ) -> Result<DurableProcessRecord, String> {
        let _guard = self.lock_state()?;
        let mut record = self.require_process_generation_unlocked(
            process_id,
            expected_session_id,
            expected_remote_process_id,
            expected_generation,
        )?;
        if matches!(
            record.status,
            DurableProcessStatus::Terminal | DurableProcessStatus::Resolved
        ) {
            return Ok(record);
        }
        record.status = DurableProcessStatus::Lost;
        record.updated_unix_ms = now;
        record.loss = Some(LossTombstone::new(code, detail, now));
        self.write_process_record(&record)?;
        Ok(record)
    }

    pub(crate) fn mark_process_terminal(
        &self,
        process_id: &str,
        expected_session_id: &str,
        expected_remote_process_id: &str,
        expected_generation: u64,
        exit_code: Option<i32>,
        now: u64,
    ) -> Result<DurableProcessRecord, String> {
        let _guard = self.lock_state()?;
        let mut record = self.require_process_generation_unlocked(
            process_id,
            expected_session_id,
            expected_remote_process_id,
            expected_generation,
        )?;
        record.status = DurableProcessStatus::Terminal;
        record.updated_unix_ms = now;
        record.exit_code = exit_code;
        record.loss = None;
        self.write_process_record(&record)?;
        self.archive_process(&record)?;
        self.remove_process_record(process_id)?;
        Ok(record)
    }

    pub(crate) fn resolve_lost_process(
        &self,
        process_id: &str,
        expected_session_id: &str,
        expected_remote_process_id: &str,
        expected_generation: u64,
        resolution: &str,
        now: u64,
    ) -> Result<DurableProcessRecord, String> {
        let _guard = self.lock_state()?;
        if resolution.is_empty() {
            return Err("remote lost-process resolution is empty".to_string());
        }
        let mut record = self.require_process_generation_unlocked(
            process_id,
            expected_session_id,
            expected_remote_process_id,
            expected_generation,
        )?;
        if record.status != DurableProcessStatus::Lost {
            return Err(format!(
                "remote process '{process_id}' is {:?}, not a lost process awaiting explicit resolution",
                record.status
            ));
        }
        record.status = DurableProcessStatus::Resolved;
        record.updated_unix_ms = now;
        record.resolution = Some(resolution.to_string());
        self.write_process_record(&record)?;
        self.archive_process(&record)?;
        self.remove_process_record(process_id)?;
        Ok(record)
    }

    pub(crate) fn load_process(
        &self,
        process_id: &str,
    ) -> Result<Option<DurableProcessRecord>, String> {
        let _guard = self.lock_state()?;
        self.load_process_unlocked(process_id)
    }

    fn load_process_unlocked(
        &self,
        process_id: &str,
    ) -> Result<Option<DurableProcessRecord>, String> {
        validate_process_id(process_id)?;
        let leaf = self.process_leaf(process_id);
        let path = self.root.join(PROCESSES_DIR).join(&leaf);
        let Some(record) = read_json_at::<DurableProcessRecord>(
            &self.processes_dir,
            &self.root.join(PROCESSES_DIR),
            &leaf,
        )?
        else {
            return Ok(None);
        };
        self.validate_process(&record)?;
        if record.process_id != process_id {
            return Err(format!(
                "remote process record '{}' claims logical id '{}'",
                path.display(),
                record.process_id
            ));
        }
        Ok(Some(record))
    }

    pub(crate) fn load_processes(&self) -> Result<Vec<DurableProcessRecord>, String> {
        let _guard = self.lock_state()?;
        self.load_processes_unlocked()
    }

    fn load_processes_unlocked(&self) -> Result<Vec<DurableProcessRecord>, String> {
        let directory = self.root.join(PROCESSES_DIR);
        let mut leaves = read_directory_names(&self.processes_dir, &directory)?;
        leaves.sort();
        let mut records = Vec::with_capacity(leaves.len());
        for leaf in leaves {
            let path = directory.join(&leaf);
            if is_stale_temporary_record(&leaf) {
                remove_record_at(&self.processes_dir, &directory, &leaf)?;
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                return Err(format!(
                    "remote process state contains unexpected entry '{}'",
                    path.display()
                ));
            }
            let record =
                read_json_at::<DurableProcessRecord>(&self.processes_dir, &directory, &leaf)?
                    .ok_or_else(|| {
                        format!("remote process record '{}' disappeared", path.display())
                    })?;
            self.validate_process(&record)?;
            let expected = self.process_leaf(&record.process_id);
            if leaf != expected {
                return Err(format!(
                    "remote process record '{}' does not match logical id '{}'",
                    path.display(),
                    record.process_id
                ));
            }
            records.push(record);
        }
        Ok(records)
    }

    pub(crate) fn has_unresolved_processes(&self) -> Result<bool, String> {
        let _guard = self.lock_state()?;
        Ok(self.load_processes_unlocked()?.iter().any(|record| {
            matches!(
                record.status,
                DurableProcessStatus::Starting
                    | DurableProcessStatus::Active
                    | DurableProcessStatus::Lost
            )
        }))
    }

    fn load_session_unchecked(&self) -> Result<Option<DurableSessionRecord>, String> {
        read_json_at(&self.root_dir, &self.root, SESSION_FILE)
    }

    fn validate_session(&self, record: &DurableSessionRecord) -> Result<(), String> {
        if record.schema_version != 1 || record.identity.schema_version != 1 {
            return Err("remote exec durable session schema version is unsupported".to_string());
        }
        if record.identity != self.identity {
            return Err(
                "remote exec connection, authentication, configuration, or formation identity changed while durable state exists"
                    .to_string(),
            );
        }
        if record.session_id.is_empty() {
            return Err("remote exec durable session id is empty".to_string());
        }
        Ok(())
    }

    fn validate_process(&self, record: &DurableProcessRecord) -> Result<(), String> {
        if record.schema_version != 2 || record.identity.schema_version != 1 {
            return Err("remote exec durable process schema version is unsupported".to_string());
        }
        if record.identity != self.identity {
            return Err(
                "remote exec process record does not match the pinned connection, authentication, configuration, and formation identity"
                    .to_string(),
            );
        }
        validate_process_id(&record.process_id)?;
        if record.remote_process_id.is_empty() || record.argv.is_empty() {
            return Err(format!(
                "remote process '{}' has incomplete durable identity",
                record.process_id
            ));
        }
        let session = self.load_session_unchecked()?.ok_or_else(|| {
            format!(
                "remote process '{}' has no durable session",
                record.process_id
            )
        })?;
        self.validate_session(&session)?;
        if record.session_id != session.session_id {
            return Err(format!(
                "remote process '{}' belongs to a different remote session",
                record.process_id
            ));
        }
        Ok(())
    }

    fn require_process_unlocked(&self, process_id: &str) -> Result<DurableProcessRecord, String> {
        self.load_process_unlocked(process_id)?
            .ok_or_else(|| format!("remote process '{process_id}' has no durable record"))
    }

    fn require_process_generation_unlocked(
        &self,
        process_id: &str,
        expected_session_id: &str,
        expected_remote_process_id: &str,
        expected_generation: u64,
    ) -> Result<DurableProcessRecord, String> {
        let record = self.require_process_unlocked(process_id)?;
        if record.session_id != expected_session_id
            || record.remote_process_id != expected_remote_process_id
            || record.generation != expected_generation
        {
            return Err(format!(
                "remote process '{process_id}' generation changed before durable mutation"
            ));
        }
        Ok(record)
    }

    fn validate_all_state(&self) -> Result<(), String> {
        let _guard = self.lock_state()?;
        cleanup_stale_temporary_records_at(&self.root_dir, &self.root)?;
        cleanup_stale_temporary_records_at(&self.processes_dir, &self.root.join(PROCESSES_DIR))?;
        cleanup_stale_temporary_records_at(&self.history_dir, &self.root.join(HISTORY_DIR))?;
        if let Some(session) = self.load_session_unchecked()? {
            self.validate_session(&session)?;
        }
        self.finish_completed_process_records_unlocked()?;
        if self
            .load_session_unchecked()?
            .is_some_and(|session| session.status == DurableSessionStatus::Claiming)
            && !self.load_processes_unlocked()?.is_empty()
        {
            return Err("remote exec claiming session cannot retain process records".to_string());
        }
        Ok(())
    }

    fn finish_completed_process_records_unlocked(&self) -> Result<(), String> {
        for record in self.load_processes_unlocked()? {
            if matches!(
                record.status,
                DurableProcessStatus::Terminal | DurableProcessStatus::Resolved
            ) {
                self.archive_process(&record)?;
                self.remove_process_record(&record.process_id)?;
            }
        }
        Ok(())
    }

    fn lock_state(&self) -> Result<RecoveryStateGuard<'_>, String> {
        let mutation = self
            .mutation
            .lock()
            .map_err(|_| "remote exec durable state lock is poisoned".to_string())?;
        lock_state_file(&self.state_lock)?;
        Ok(RecoveryStateGuard {
            _mutation: mutation,
            state_lock: &self.state_lock,
        })
    }

    fn process_leaf(&self, process_id: &str) -> String {
        format!("{process_id}.json")
    }

    fn write_process_record(&self, record: &DurableProcessRecord) -> Result<(), String> {
        write_json_at(
            &self.processes_dir,
            &self.root.join(PROCESSES_DIR),
            &self.process_leaf(&record.process_id),
            record,
        )
    }

    fn remove_process_record(&self, process_id: &str) -> Result<(), String> {
        remove_record_at(
            &self.processes_dir,
            &self.root.join(PROCESSES_DIR),
            &self.process_leaf(process_id),
        )
    }

    fn archive_session(&self, record: &DurableSessionRecord) -> Result<(), String> {
        let name = format!(
            "session.{}.{}.{}.json",
            safe_fragment(&record.session_id),
            record.updated_unix_ms,
            serialized_fragment(record)?
        );
        write_json_at(
            &self.history_dir,
            &self.root.join(HISTORY_DIR),
            &name,
            record,
        )
    }

    fn archive_process(&self, record: &DurableProcessRecord) -> Result<(), String> {
        let name = format!(
            "process.{}.{}.{}.{}.json",
            safe_fragment(&record.process_id),
            safe_fragment(&record.remote_process_id),
            record.updated_unix_ms,
            serialized_fragment(record)?
        );
        write_json_at(
            &self.history_dir,
            &self.root.join(HISTORY_DIR),
            &name,
            record,
        )
    }
}

pub(crate) fn validate_process_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
    {
        return Err("remote process_id must match [A-Za-z0-9._-]{1,128}".to_string());
    }
    Ok(())
}

fn safe_fragment(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex().to_string()
}

fn is_stale_temporary_record(leaf: &str) -> bool {
    leaf.starts_with('.') && leaf.ends_with(".tmp")
}

fn cleanup_stale_temporary_records_at(
    directory: &std::fs::File,
    display_directory: &Path,
) -> Result<(), String> {
    for leaf in read_directory_names(directory, display_directory)? {
        if is_stale_temporary_record(&leaf) {
            remove_record_at(directory, display_directory, &leaf)?;
        }
    }
    Ok(())
}

fn serialized_fragment<T: Serialize>(value: &T) -> Result<String, String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("remote exec history record did not serialize: {error}"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(format!(
                    "remote exec durable path '{}' is not a real directory",
                    path.display()
                ));
            }
            ensure_owner_only_directory(path, &metadata)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(|error| {
                format!(
                    "remote exec durable directory '{}' cannot be created: {error}",
                    path.display()
                )
            })?;
            let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
            ensure_owner_only_directory(path, &metadata)?;
            let parent = path.parent().ok_or_else(|| {
                format!(
                    "remote exec durable directory '{}' has no parent",
                    path.display()
                )
            })?;
            sync_directory_entry_parent(parent)?;
        }
        Err(error) => {
            return Err(format!(
                "remote exec durable directory '{}' cannot be inspected: {error}",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_private_subdirectory_at(
    parent: &std::fs::File,
    display_parent: &Path,
    leaf: &str,
) -> Result<std::fs::File, String> {
    use std::os::fd::AsRawFd;
    validate_held_directory(parent, display_parent)?;
    let leaf_c = leaf_c_string(leaf)?;
    let display_path = display_parent.join(leaf);
    let open = || open_at_file(parent, &leaf_c, libc::O_RDONLY | libc::O_DIRECTORY, 0);
    let directory = match open() {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if unsafe { libc::mkdirat(parent.as_raw_fd(), leaf_c.as_ptr(), 0o700) } != 0 {
                let create_error = std::io::Error::last_os_error();
                if create_error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(format!(
                        "remote exec durable directory '{}' cannot be created: {create_error}",
                        display_path.display()
                    ));
                }
            }
            parent.sync_all().map_err(|error| {
                format!(
                    "remote exec durable directory '{}' cannot publish its child: {error}",
                    display_parent.display()
                )
            })?;
            open().map_err(|error| {
                format!(
                    "remote exec durable directory '{}' cannot be opened after creation: {error}",
                    display_path.display()
                )
            })?
        }
        Err(error) => {
            return Err(format!(
                "remote exec durable directory '{}' cannot be opened: {error}",
                display_path.display()
            ));
        }
    };
    ensure_owner_only_directory(
        &display_path,
        &directory.metadata().map_err(|error| error.to_string())?,
    )?;
    directory.sync_all().map_err(|error| {
        format!(
            "remote exec durable directory '{}' cannot be synchronized: {error}",
            display_path.display()
        )
    })?;
    validate_held_directory(parent, display_parent)?;
    Ok(directory)
}

#[cfg(not(unix))]
fn ensure_private_subdirectory_at(
    _parent: &std::fs::File,
    _display_parent: &Path,
    _leaf: &str,
) -> Result<std::fs::File, String> {
    Err("remote exec durable records require Unix owner-only filesystem semantics".to_string())
}

fn ensure_owner_only_directory(path: &Path, metadata: &std::fs::Metadata) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o777 != 0o700 {
            return Err(format!(
                "remote exec durable directory '{}' must have mode 0700",
                path.display()
            ));
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "remote exec durable directory '{}' is not owned by the effective user",
                path.display()
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, metadata);
        return Err(
            "remote exec durable records require Unix owner-only filesystem semantics".to_string(),
        );
    }
    Ok(())
}

#[cfg(unix)]
fn write_json_at<T: Serialize>(
    directory: &std::fs::File,
    display_directory: &Path,
    leaf_text: &str,
    value: &T,
) -> Result<(), String> {
    validate_held_directory(directory, display_directory)?;
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("remote exec record did not serialize: {error}"))?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(format!(
            "remote exec record requires {} bytes, above durable limit {MAX_RECORD_BYTES}",
            bytes.len()
        ));
    }
    let sequence = NEXT_TEMP
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| "remote exec temporary record sequence is exhausted".to_string())?;
    let leaf = leaf_c_string(leaf_text)?;
    let temporary_text = format!(
        ".write.{}.{}.{}.tmp",
        safe_fragment(leaf_text),
        std::process::id(),
        sequence
    );
    let temporary = leaf_c_string(&temporary_text)?;
    let temporary_path = display_directory.join(&temporary_text);
    let open_new = || {
        open_at_file(
            directory,
            &temporary,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
    };
    let mut file = match open_new() {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let stale = open_at_file(directory, &temporary, libc::O_RDONLY | libc::O_NONBLOCK, 0)
                .map_err(|error| {
                format!("remote exec stale temporary record cannot be opened: {error}")
            })?;
            validate_private_record_metadata(
                &temporary_path,
                &stale.metadata().map_err(|error| error.to_string())?,
            )?;
            drop(stale);
            unlink_at(directory, &temporary).map_err(|error| {
                format!("remote exec stale temporary record cannot be removed: {error}")
            })?;
            directory.sync_all().map_err(|error| error.to_string())?;
            open_new().map_err(|error| {
                format!("remote exec temporary record cannot be recreated: {error}")
            })?
        }
        Err(error) => {
            return Err(format!(
                "remote exec temporary record cannot be opened: {error}"
            ));
        }
    };
    let result = (|| {
        let created = file.metadata().map_err(|error| error.to_string())?;
        validate_private_record_metadata(&temporary_path, &created)?;
        let identity = record_object_identity(&created);
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        let synced = file.metadata().map_err(|error| error.to_string())?;
        if record_object_identity(&synced) != identity || synced.len() != bytes.len() as u64 {
            return Err(format!(
                "remote exec temporary record '{}' changed while it was written",
                temporary_path.display()
            ));
        }
        let named_temporary =
            open_at_file(directory, &temporary, libc::O_RDONLY | libc::O_NONBLOCK, 0)
                .map_err(|error| error.to_string())?;
        if stable_record_identity(&named_temporary.metadata().map_err(|e| e.to_string())?)
            != stable_record_identity(&synced)
        {
            return Err("remote exec temporary record pathname changed before publish".to_string());
        }
        rename_at(directory, &temporary, &leaf).map_err(|error| error.to_string())?;
        let published = open_at_file(directory, &leaf, libc::O_RDONLY | libc::O_NONBLOCK, 0)
            .map_err(|error| error.to_string())?;
        let published = published.metadata().map_err(|e| e.to_string())?;
        if record_object_identity(&published) != record_object_identity(&synced)
            || published.len() != synced.len()
        {
            return Err("remote exec published record is not the written inode".to_string());
        }
        directory.sync_all().map_err(|error| error.to_string())?;
        validate_held_directory(directory, display_directory)
    })();
    if result.is_err() {
        let _ = unlink_at(directory, &temporary);
        let _ = directory.sync_all();
    }
    result
}

#[cfg(not(unix))]
fn write_json_at<T: Serialize>(
    _directory: &std::fs::File,
    _display_directory: &Path,
    _leaf_text: &str,
    _value: &T,
) -> Result<(), String> {
    Err("remote exec durable records require Unix owner-only filesystem semantics".to_string())
}

#[cfg(unix)]
fn read_json_at<T: for<'de> Deserialize<'de>>(
    directory: &std::fs::File,
    display_directory: &Path,
    leaf_text: &str,
) -> Result<Option<T>, String> {
    validate_held_directory(directory, display_directory)?;
    let path = display_directory.join(leaf_text);
    let leaf = leaf_c_string(leaf_text)?;
    let mut file = match open_at_file(directory, &leaf, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "remote exec record '{}' cannot be opened: {error}",
                path.display()
            ));
        }
    };
    let opened = file.metadata().map_err(|error| error.to_string())?;
    validate_private_record_metadata(&path, &opened)?;
    if opened.len() == 0 || opened.len() > MAX_RECORD_BYTES {
        return Err(format!(
            "remote exec record '{}' has invalid length {}",
            path.display(),
            opened.len()
        ));
    }
    let opened_identity = stable_record_identity(&opened);
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 != opened.len() {
        return Err(format!(
            "remote exec record '{}' changed during read",
            path.display()
        ));
    }
    let after = file.metadata().map_err(|error| error.to_string())?;
    let named =
        open_at_file(directory, &leaf, libc::O_RDONLY | libc::O_NONBLOCK, 0).map_err(|error| {
            format!(
                "remote exec record '{}' cannot be re-opened: {error}",
                path.display()
            )
        })?;
    let named = named.metadata().map_err(|error| error.to_string())?;
    if stable_record_identity(&after) != opened_identity
        || stable_record_identity(&named) != opened_identity
    {
        return Err(format!(
            "remote exec record '{}' changed during read",
            path.display()
        ));
    }
    validate_held_directory(directory, display_directory)?;
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        format!(
            "remote exec record '{}' is malformed: {error}",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn read_json_at<T: for<'de> Deserialize<'de>>(
    _directory: &std::fs::File,
    _display_directory: &Path,
    _leaf_text: &str,
) -> Result<Option<T>, String> {
    Err("remote exec durable records require Unix owner-only filesystem semantics".to_string())
}

#[cfg(unix)]
fn validate_private_record_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "remote exec record '{}' is not a regular file",
            path.display()
        ));
    }
    if metadata.mode() & 0o777 != 0o600 {
        return Err(format!(
            "remote exec record '{}' must have mode 0600",
            path.display()
        ));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(format!(
            "remote exec record '{}' is not owned by the effective user",
            path.display()
        ));
    }
    if metadata.nlink() != 1 {
        return Err(format!(
            "remote exec record '{}' must have exactly one filesystem link",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_record_metadata(
    _path: &Path,
    _metadata: &std::fs::Metadata,
) -> Result<(), String> {
    Err("remote exec durable records require Unix owner-only filesystem semantics".to_string())
}

#[cfg(unix)]
fn record_object_identity(metadata: &std::fs::Metadata) -> (u64, u64, u32, u64, u32, u32) {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.mode(),
        metadata.nlink(),
        metadata.uid(),
        metadata.gid(),
    )
}

#[cfg(not(unix))]
fn record_object_identity(_metadata: &std::fs::Metadata) -> () {}

#[cfg(unix)]
fn stable_record_identity(
    metadata: &std::fs::Metadata,
) -> (u64, u64, u32, u64, u32, u32, u64, i64, i64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.mode(),
        metadata.nlink(),
        metadata.uid(),
        metadata.gid(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[cfg(not(unix))]
fn stable_record_identity(_metadata: &std::fs::Metadata) -> () {}

#[cfg(unix)]
fn remove_record_at(
    directory: &std::fs::File,
    display_directory: &Path,
    leaf_text: &str,
) -> Result<(), String> {
    validate_held_directory(directory, display_directory)?;
    let path = display_directory.join(leaf_text);
    let leaf = leaf_c_string(leaf_text)?;
    let existing = match open_at_file(directory, &leaf, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "remote exec record '{}' cannot be opened for removal: {error}",
                path.display()
            ));
        }
    };
    let opened = existing.metadata().map_err(|error| error.to_string())?;
    validate_private_record_metadata(&path, &opened)?;
    let named =
        open_at_file(directory, &leaf, libc::O_RDONLY | libc::O_NONBLOCK, 0).map_err(|error| {
            format!(
                "remote exec record '{}' cannot be re-opened for removal: {error}",
                path.display()
            )
        })?;
    if stable_record_identity(&named.metadata().map_err(|error| error.to_string())?)
        != stable_record_identity(&opened)
    {
        return Err(format!(
            "remote exec record '{}' changed before removal",
            path.display()
        ));
    }
    unlink_at(directory, &leaf).map_err(|error| {
        format!(
            "remote exec record '{}' cannot be removed: {error}",
            path.display()
        )
    })?;
    directory.sync_all().map_err(|error| {
        format!(
            "remote exec durable directory '{}' cannot be synchronized: {error}",
            display_directory.display()
        )
    })?;
    validate_held_directory(directory, display_directory)
}

#[cfg(not(unix))]
fn remove_record_at(
    _directory: &std::fs::File,
    _display_directory: &Path,
    _leaf_text: &str,
) -> Result<(), String> {
    Err("remote exec durable records require Unix owner-only filesystem semantics".to_string())
}

#[cfg(unix)]
fn sync_directory_entry_parent(path: &Path) -> Result<(), String> {
    open_directory_fd(path)
        .map_err(|error| {
            format!(
                "remote exec durable directory parent '{}' cannot be opened: {error}",
                path.display()
            )
        })?
        .sync_all()
        .map_err(|error| {
            format!(
                "remote exec durable directory parent '{}' cannot be synchronized: {error}",
                path.display()
            )
        })
}

#[cfg(not(unix))]
fn sync_directory_entry_parent(_path: &Path) -> Result<(), String> {
    Err("remote exec durable records require Unix owner-only filesystem semantics".to_string())
}

#[cfg(unix)]
fn open_private_directory_fd(path: &Path) -> Result<std::fs::File, String> {
    let directory = open_directory_fd(path).map_err(|error| {
        format!(
            "remote exec durable directory '{}' cannot be opened: {error}",
            path.display()
        )
    })?;
    let metadata = directory.metadata().map_err(|error| error.to_string())?;
    ensure_owner_only_directory(path, &metadata)?;
    Ok(directory)
}

#[cfg(unix)]
fn validate_held_directory(directory: &std::fs::File, display: &Path) -> Result<(), String> {
    ensure_owner_only_directory(
        display,
        &directory.metadata().map_err(|error| error.to_string())?,
    )
}

#[cfg(not(unix))]
fn validate_held_directory(_directory: &std::fs::File, _display: &Path) -> Result<(), String> {
    Err("remote exec durable records require Unix owner-only filesystem semantics".to_string())
}

#[cfg(unix)]
fn open_state_lock(
    directory: &std::fs::File,
    display_root: &Path,
) -> Result<std::fs::File, String> {
    validate_held_directory(directory, display_root)?;
    let leaf = leaf_c_string(STATE_LOCK_FILE)?;
    let file = open_at_file(directory, &leaf, libc::O_RDWR | libc::O_CREAT, 0o600)
        .map_err(|error| format!("remote exec durable state lock cannot be opened: {error}"))?;
    validate_private_record_metadata(
        &display_root.join(STATE_LOCK_FILE),
        &file.metadata().map_err(|error| error.to_string())?,
    )?;
    file.sync_all().map_err(|error| error.to_string())?;
    directory.sync_all().map_err(|error| error.to_string())?;
    Ok(file)
}

#[cfg(not(unix))]
fn open_state_lock(
    _directory: &std::fs::File,
    _display_root: &Path,
) -> Result<std::fs::File, String> {
    Err("remote exec durable records require Unix owner-only filesystem semantics".to_string())
}

#[cfg(unix)]
fn lock_state_file(file: &std::fs::File) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
        Ok(())
    } else {
        Err(format!(
            "remote exec durable state lock cannot be acquired: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(unix))]
fn lock_state_file(_file: &std::fs::File) -> Result<(), String> {
    Err("remote exec durable records require Unix owner-only filesystem semantics".to_string())
}

#[cfg(unix)]
fn open_directory_fd(path: &Path) -> Result<std::fs::File, std::io::Error> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "remote exec durable directory path contains NUL",
        )
    })?;
    let raw = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { std::fs::File::from_raw_fd(raw) })
    }
}

#[cfg(unix)]
fn read_directory_names(
    directory: &std::fs::File,
    display_directory: &Path,
) -> Result<Vec<String>, String> {
    use std::os::fd::IntoRawFd;
    validate_held_directory(directory, display_directory)?;
    let current = std::ffi::CString::new(".").map_err(|error| error.to_string())?;
    let listing = open_at_file(directory, &current, libc::O_RDONLY | libc::O_DIRECTORY, 0)
        .map_err(|error| {
            format!(
                "remote exec durable directory '{}' cannot open a listing descriptor: {error}",
                display_directory.display()
            )
        })?;
    let listing = listing.into_raw_fd();
    let stream = unsafe { libc::fdopendir(listing) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(listing);
        }
        return Err(format!(
            "remote exec durable directory '{}' cannot start listing: {error}",
            display_directory.display()
        ));
    }
    let result = (|| {
        let mut names = Vec::new();
        loop {
            set_errno_zero();
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                let errno = current_errno();
                if errno != 0 {
                    return Err(format!(
                        "remote exec durable directory '{}' cannot be listed: {}",
                        display_directory.display(),
                        std::io::Error::from_raw_os_error(errno)
                    ));
                }
                break;
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }
                .to_str()
                .map_err(|_| {
                    format!(
                        "remote exec durable directory '{}' contains a non-UTF-8 entry",
                        display_directory.display()
                    )
                })?;
            if name != "." && name != ".." {
                leaf_c_string(name)?;
                names.push(name.to_string());
            }
        }
        Ok(names)
    })();
    let close_result = unsafe { libc::closedir(stream) };
    if close_result != 0 {
        return Err(format!(
            "remote exec durable directory '{}' cannot finish listing: {}",
            display_directory.display(),
            std::io::Error::last_os_error()
        ));
    }
    validate_held_directory(directory, display_directory)?;
    result
}

#[cfg(not(unix))]
fn read_directory_names(
    _directory: &std::fs::File,
    _display_directory: &Path,
) -> Result<Vec<String>, String> {
    Err("remote exec durable records require Unix owner-only filesystem semantics".to_string())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn set_errno_zero() {
    unsafe {
        *libc::__errno_location() = 0;
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn current_errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn set_errno_zero() {
    unsafe {
        *libc::__error() = 0;
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn current_errno() -> i32 {
    unsafe { *libc::__error() }
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))
))]
fn set_errno_zero() {}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))
))]
fn current_errno() -> i32 {
    0
}

#[cfg(unix)]
fn leaf_c_string(leaf: &str) -> Result<std::ffi::CString, String> {
    if leaf.is_empty() || leaf == "." || leaf == ".." || leaf.contains('/') {
        return Err("remote exec durable record has an invalid leaf name".to_string());
    }
    std::ffi::CString::new(leaf.as_bytes())
        .map_err(|_| "remote exec durable record leaf contains NUL".to_string())
}

#[cfg(unix)]
fn open_at_file(
    directory: &std::fs::File,
    leaf: &std::ffi::CStr,
    flags: i32,
    mode: libc::mode_t,
) -> Result<std::fs::File, std::io::Error> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let raw = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            leaf.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            mode as libc::c_uint,
        )
    };
    if raw < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { std::fs::File::from_raw_fd(raw) })
    }
}

#[cfg(unix)]
fn rename_at(
    directory: &std::fs::File,
    from: &std::ffi::CStr,
    to: &std::ffi::CStr,
) -> Result<(), std::io::Error> {
    use std::os::fd::AsRawFd;
    if unsafe {
        libc::renameat(
            directory.as_raw_fd(),
            from.as_ptr(),
            directory.as_raw_fd(),
            to.as_ptr(),
        )
    } == 0
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn unlink_at(directory: &std::fs::File, leaf: &std::ffi::CStr) -> Result<(), std::io::Error> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::unlinkat(directory.as_raw_fd(), leaf.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn private_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let mut permissions = std::fs::metadata(root.path()).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(root.path(), permissions).unwrap();
        root
    }

    fn identity() -> RemoteRoomIdentity {
        RemoteRoomIdentity {
            schema_version: 1,
            worker_environment_id: WorkerEnvironmentId::derive(b"remote-recovery-test"),
            formation_pin: "formation-pin".to_string(),
            door_descriptor_hash: "door-hash".to_string(),
            worker_environment_kind_descriptor_hash: "kind-hash".to_string(),
            websocket_url: "wss://remote.invalid/exec".to_string(),
            workspace_uri: "file:///workspace".to_string(),
            auth_identity: "auth-hash".to_string(),
            config_identity: "config-hash".to_string(),
        }
    }

    #[test]
    fn store_keeps_the_opened_directory_identity_after_path_replacement() {
        let root = private_root();
        let store = RecoveryStore::open(root.path(), identity()).unwrap();
        store.activate_session("session-a".to_string(), 10).unwrap();
        let moved = root.path().join("remote-exec-opened");
        std::fs::rename(root.path().join("remote-exec"), &moved).unwrap();
        std::fs::create_dir(root.path().join("remote-exec")).unwrap();
        let mut permissions = std::fs::metadata(root.path().join("remote-exec"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(root.path().join("remote-exec"), permissions).unwrap();
        store
            .mark_session_lost("session-a", "transport_lost", "closed", 20)
            .unwrap();
        assert!(!root.path().join("remote-exec/session.json").exists());
        let record: DurableSessionRecord =
            serde_json::from_slice(&std::fs::read(moved.join(SESSION_FILE)).unwrap()).unwrap();
        assert_eq!(record.status, DurableSessionStatus::Lost);
        assert_eq!(
            store.load_session().unwrap().unwrap().status,
            DurableSessionStatus::Lost
        );
    }

    #[test]
    fn session_claim_is_durable_and_exact_before_remote_activation() {
        let root = private_root();
        let identity = identity();
        let claim_id = identity.session_claim_id().unwrap();
        let store = RecoveryStore::open(root.path(), identity.clone()).unwrap();
        let claiming = store.begin_session_claim(claim_id.clone(), 10).unwrap();
        assert_eq!(claiming.status, DurableSessionStatus::Claiming);
        assert_eq!(claiming.session_id, claim_id);
        drop(store);

        let reopened = RecoveryStore::open(root.path(), identity).unwrap();
        assert_eq!(
            reopened.load_session().unwrap().unwrap().status,
            DurableSessionStatus::Claiming
        );
        let repeated = reopened
            .begin_session_claim(claiming.session_id.clone(), 20)
            .unwrap();
        assert_eq!(repeated.created_unix_ms, 10);
        let active = reopened
            .activate_session(claiming.session_id.clone(), 30)
            .unwrap();
        assert_eq!(active.status, DurableSessionStatus::Active);
        assert_eq!(active.created_unix_ms, 10);
    }

    #[test]
    fn stale_process_generations_and_completed_crash_records_cannot_cross_sessions() {
        let root = private_root();
        let store = RecoveryStore::open(root.path(), identity()).unwrap();
        let first_session = store.activate_session("session-a".to_string(), 10).unwrap();
        let logical_id = "p".repeat(128);
        let first = store
            .begin_process(
                &logical_id,
                "remote-a",
                100,
                8,
                vec!["/bin/true".to_string()],
                "/workspace".to_string(),
                false,
                true,
                11,
            )
            .unwrap();
        store
            .mark_process_active(
                &logical_id,
                &first.session_id,
                &first.remote_process_id,
                first.generation,
                12,
            )
            .unwrap();
        store
            .mark_process_terminal(
                &logical_id,
                &first.session_id,
                &first.remote_process_id,
                first.generation,
                Some(0),
                13,
            )
            .unwrap();
        let mut second = store
            .begin_process(
                &logical_id,
                "remote-a",
                200,
                8,
                vec!["/bin/sleep".to_string(), "30".to_string()],
                "/workspace".to_string(),
                false,
                true,
                14,
            )
            .unwrap();
        assert!(store
            .mark_process_lost(
                &logical_id,
                &first.session_id,
                &first.remote_process_id,
                first.generation,
                "late_event",
                "stale",
                15,
            )
            .is_err());
        assert_eq!(
            store.load_process(&logical_id).unwrap().unwrap().generation,
            200
        );
        second.status = DurableProcessStatus::Terminal;
        second.updated_unix_ms = 16;
        second.exit_code = Some(0);
        store.write_process_record(&second).unwrap();
        store
            .mark_session_lost("session-a", "transport_lost", "closed", 20)
            .unwrap();
        let recovered = store.activate_session("session-a".to_string(), 30).unwrap();
        assert_eq!(recovered.created_unix_ms, first_session.created_unix_ms);
        assert!(store.load_processes().unwrap().is_empty());
        store
            .mark_session_lost("session-a", "transport_lost", "closed", 40)
            .unwrap();
        let replacement = store.activate_session("session-b".to_string(), 50).unwrap();
        assert_eq!(replacement.created_unix_ms, 50);
        for entry in std::fs::read_dir(root.path().join("remote-exec/history")).unwrap() {
            assert!(entry.unwrap().file_name().to_string_lossy().len() <= 255);
        }
    }
}
