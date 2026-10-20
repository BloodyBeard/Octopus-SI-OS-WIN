use std::io::Write;
use std::path::{Path, PathBuf};

use octopus_interface_transport::runtime_instance::RuntimeInstanceLock;
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInstanceRecord {
    pub schema_version: u32,
    pub pid: u32,
    pub phase: String,
    pub started_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub launch_owner: (),
}

pub struct RuntimeInstance {
    root: PathBuf,
    state_path: PathBuf,
    record: RuntimeInstanceRecord,
    _lock: RuntimeInstanceLock,
}

impl RuntimeInstance {
    pub fn acquire(octopus_home: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let state_root = octopus_home.join("state");
        let lock_path = state_root.join("runtime-instance.lock");
        let lock = match RuntimeInstanceLock::try_acquire(octopus_home) {
            Ok(lock) => lock,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let state_path = state_root.join("runtime-instance").join("state.json");
                let detail = read_record(&state_path)
                    .map(|record| format!("pid {} in phase '{}'", record.pid, record.phase))
                    .unwrap_or_else(|_| "initialization is still in progress".to_string());
                return Err(format!("Octopus runtime is already active: {detail}").into());
            }
            Err(error) => {
                return Err(format!(
                    "Octopus runtime lock '{}' cannot be acquired: {error}",
                    lock_path.display()
                )
                .into());
            }
        };
        lock.verify()?;
        let root = state_root.join("runtime-instance");
        let state_path = root.join("state.json");
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir(&root)?;
        let now = unix_ms()?;
        let record = RuntimeInstanceRecord {
            schema_version: SCHEMA_VERSION,
            pid: std::process::id(),
            phase: "starting".to_string(),
            started_at_unix_ms: now,
            updated_at_unix_ms: now,
            launch_owner: (),
        };
        let mut instance = Self {
            root,
            state_path,
            record,
            _lock: lock,
        };
        instance.persist()?;
        Ok(instance)
    }

    pub fn set_phase(&mut self, phase: &str) -> Result<(), Box<dyn std::error::Error>> {
        if phase.is_empty()
            || !phase
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            return Err(format!("runtime phase '{phase}' is not a lowercase identifier").into());
        }
        self.record.phase = phase.to_string();
        self.record.updated_at_unix_ms = unix_ms()?;
        self.persist()
    }

    fn persist(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self._lock.verify()?;
        let mut bytes = serde_json::to_vec_pretty(&self.record)?;
        bytes.push(b'\n');
        let staged = self.root.join(format!(
            ".state-{}-{}.tmp",
            std::process::id(),
            self.record.updated_at_unix_ms
        ));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&staged, &self.state_path)?;
        std::fs::File::open(&self.root)?.sync_all()?;
        self._lock.verify()?;
        Ok(())
    }
}

impl Drop for RuntimeInstance {
    fn drop(&mut self) {
        let owned = self._lock.verify().is_ok()
            && read_record(&self.state_path).is_ok_and(|record| record.pid == std::process::id());
        if owned {
            let _ = std::fs::remove_dir_all(&self.root);
            if let Some(parent) = self.root.parent() {
                let _ = std::fs::File::open(parent).and_then(|directory| directory.sync_all());
            }
        }
    }
}

pub fn observe(
    octopus_home: &Path,
) -> Result<Option<RuntimeInstanceRecord>, Box<dyn std::error::Error>> {
    let state_root = octopus_home.join("state");
    let lock_path = state_root.join("runtime-instance.lock");
    match RuntimeInstanceLock::try_acquire(octopus_home) {
        Ok(lock) => {
            lock.verify()?;
            return Ok(None);
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(error) => {
            return Err(format!(
                "Octopus runtime lock '{}' cannot be observed: {error}",
                lock_path.display()
            )
            .into());
        }
    }
    let state_path = state_root.join("runtime-instance").join("state.json");
    match read_record(&state_path) {
        Ok(record) => Ok(Some(record)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub fn read_current(
    octopus_home: &Path,
) -> Result<RuntimeInstanceRecord, Box<dyn std::error::Error>> {
    read_record(
        &octopus_home
            .join("state")
            .join("runtime-instance")
            .join("state.json"),
    )
}

fn read_record(path: &Path) -> Result<RuntimeInstanceRecord, Box<dyn std::error::Error>> {
    let record: RuntimeInstanceRecord = serde_json::from_slice(&std::fs::read(path)?)?;
    if !record_is_valid(&record) {
        return Err(format!("runtime instance record '{}' is invalid", path.display()).into());
    }
    Ok(record)
}

fn record_is_valid(record: &RuntimeInstanceRecord) -> bool {
    if record.schema_version != SCHEMA_VERSION
        || record.pid == 0
        || record.phase.is_empty()
        || !record
            .phase
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        || record.started_at_unix_ms == 0
        || record.updated_at_unix_ms < record.started_at_unix_ms
    {
        return false;
    }
    true
}

fn unix_ms() -> Result<u128, std::time::SystemTimeError> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> RuntimeInstanceRecord {
        RuntimeInstanceRecord {
            schema_version: SCHEMA_VERSION,
            pid: 42,
            phase: "ready".to_string(),
            started_at_unix_ms: 1,
            updated_at_unix_ms: 2,
            launch_owner: (),
        }
    }

    #[test]
    fn manual_runtime_record_has_explicit_null_launch_owner() {
        let value = record();
        assert!(record_is_valid(&value));
        let encoded = serde_json::to_value(value).unwrap();
        assert_eq!(encoded["schema_version"], SCHEMA_VERSION);
        assert!(encoded["launch_owner"].is_null());
    }

    #[test]
    fn non_null_launch_owner_is_rejected() {
        for launch_owner in [
            serde_json::json!({ "protocol": "client_boot_handoff_v1" }),
            serde_json::json!("external"),
        ] {
            let mut encoded = serde_json::to_value(record()).unwrap();
            encoded["launch_owner"] = launch_owner;
            assert!(serde_json::from_value::<RuntimeInstanceRecord>(encoded).is_err());
        }
    }

    #[test]
    fn unknown_runtime_instance_fields_are_rejected() {
        let mut encoded = serde_json::to_value(record()).unwrap();
        encoded["foreign"] = serde_json::json!(true);
        assert!(serde_json::from_value::<RuntimeInstanceRecord>(encoded).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn shared_runtime_lock_excludes_startup_without_touching_state_records() {
        let temporary = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(temporary.path()).unwrap();
        let state = home.join("state/runtime-instance/state.json");
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::write(&state, b"retained state").unwrap();
        let lock_path = home.join("state/runtime-instance.lock");
        std::fs::write(&lock_path, b"retained lock").unwrap();
        let maintenance = RuntimeInstanceLock::try_acquire(&home).unwrap();
        assert_eq!(std::fs::read(&state).unwrap(), b"retained state");
        assert!(RuntimeInstance::acquire(&home).is_err());
        maintenance.verify().unwrap();
        drop(maintenance);
        let runtime = RuntimeInstance::acquire(&home).unwrap();
        assert_eq!(RuntimeInstanceLock::try_acquire(&home).err().unwrap().kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(observe(&home).unwrap().unwrap().phase, "starting");
        drop(runtime);
        assert!(observe(&home).unwrap().is_none());
        assert_eq!(std::fs::read(&lock_path).unwrap(), b"retained lock");
    }

    #[cfg(unix)]
    #[test]
    fn runtime_lock_rejects_replaced_files_and_parents_without_following_aliases() {
        for parent in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let home = std::fs::canonicalize(temporary.path()).unwrap();
            let held = RuntimeInstanceLock::try_acquire(&home).unwrap();
            let path = home.join(if parent { "state" } else { "state/runtime-instance.lock" });
            let retained = path.with_extension("retained");
            std::fs::rename(&path, &retained).unwrap();
            if parent { std::fs::create_dir(&path).unwrap(); } else { std::fs::write(&path, b"foreign").unwrap(); }
            assert!(held.verify().is_err());
            if parent { std::fs::remove_dir(&path).unwrap(); } else { std::fs::remove_file(&path).unwrap(); }
            std::os::unix::fs::symlink(&retained, &path).unwrap();
            assert!(held.verify().is_err());
            assert!(RuntimeInstanceLock::try_acquire(&home).is_err());
        }
    }
}
