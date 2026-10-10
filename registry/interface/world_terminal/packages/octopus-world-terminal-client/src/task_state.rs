use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use octopus_descriptors::RunId;
use serde::{Deserialize, Serialize};

const TASK_STATE_SCHEMA_VERSION: u32 = 1;
const MAX_TASK_TITLE_CHARS: usize = 160;

#[derive(Debug, thiserror::Error)]
pub enum TaskStateError {
    #[error("task presentation state '{path}' could not be read: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("task presentation state '{path}' is malformed: {reason}")]
    Malformed { path: String, reason: String },
    #[error("task presentation state '{path}' could not be written: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("task title is invalid: {0}")]
    InvalidTitle(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPresentation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_parent: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_from: Option<RunId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskStateFile {
    schema_version: u32,
    #[serde(default)]
    tasks: BTreeMap<String, TaskPresentation>,
}

impl Default for TaskStateFile {
    fn default() -> Self {
        Self {
            schema_version: TASK_STATE_SCHEMA_VERSION,
            tasks: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TaskStateStore {
    path: PathBuf,
    state: TaskStateFile,
}

impl TaskStateStore {
    pub fn open(octopus_home: &Path) -> Result<Self, TaskStateError> {
        let path = octopus_home
            .join("client")
            .join("world_terminal")
            .join("tasks.json");
        let state = match std::fs::read(&path) {
            Ok(bytes) => {
                let state: TaskStateFile =
                    serde_json::from_slice(&bytes).map_err(|error| TaskStateError::Malformed {
                        path: path.display().to_string(),
                        reason: error.to_string(),
                    })?;
                validate_state(&path, &state)?;
                state
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                TaskStateFile::default()
            }
            Err(source) => {
                return Err(TaskStateError::Read {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        Ok(Self { path, state })
    }

    pub fn presentation(&self, run: &RunId) -> TaskPresentation {
        self.state
            .tasks
            .get(run.as_str())
            .cloned()
            .unwrap_or_default()
    }

    pub fn title(&self, run: &RunId) -> Option<&str> {
        self.state
            .tasks
            .get(run.as_str())
            .and_then(|entry| entry.title.as_deref())
    }

    pub fn rename(&mut self, run: &RunId, title: &str) -> Result<(), TaskStateError> {
        let title = validate_title(title)?;
        self.state
            .tasks
            .entry(run.as_str().to_string())
            .or_default()
            .title = Some(title);
        self.save()
    }

    pub fn mark_side(&mut self, run: &RunId, parent: &RunId) -> Result<(), TaskStateError> {
        let entry = self
            .state
            .tasks
            .entry(run.as_str().to_string())
            .or_default();
        entry.hidden = true;
        entry.side_parent = Some(parent.clone());
        self.save()
    }

    pub fn mark_continuation(&mut self, run: &RunId, source: &RunId) -> Result<(), TaskStateError> {
        self.state
            .tasks
            .entry(run.as_str().to_string())
            .or_default()
            .continued_from = Some(source.clone());
        self.save()
    }

    pub fn save(&self) -> Result<(), TaskStateError> {
        validate_state(&self.path, &self.state)?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| TaskStateError::Malformed {
                path: self.path.display().to_string(),
                reason: "task state path has no parent directory".to_string(),
            })?;
        std::fs::create_dir_all(parent).map_err(|source| TaskStateError::Write {
            path: parent.display().to_string(),
            source,
        })?;
        let bytes =
            serde_json::to_vec_pretty(&self.state).map_err(|error| TaskStateError::Malformed {
                path: self.path.display().to_string(),
                reason: error.to_string(),
            })?;
        let staged = parent.join(format!(".tasks.{}.tmp", uuid::Uuid::new_v4().simple()));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)
            .map_err(|source| TaskStateError::Write {
                path: staged.display().to_string(),
                source,
            })?;
        let write_result = (|| {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            std::fs::rename(&staged, &self.path)?;
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
            Ok::<(), std::io::Error>(())
        })();
        if let Err(source) = write_result {
            let _ = std::fs::remove_file(&staged);
            return Err(TaskStateError::Write {
                path: self.path.display().to_string(),
                source,
            });
        }
        Ok(())
    }
}

fn validate_title(title: &str) -> Result<String, TaskStateError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(TaskStateError::InvalidTitle(
            "title must not be empty".to_string(),
        ));
    }
    if title.chars().count() > MAX_TASK_TITLE_CHARS {
        return Err(TaskStateError::InvalidTitle(format!(
            "title exceeds {MAX_TASK_TITLE_CHARS} characters"
        )));
    }
    if title.chars().any(char::is_control) {
        return Err(TaskStateError::InvalidTitle(
            "title contains control characters".to_string(),
        ));
    }
    Ok(title.to_string())
}

fn validate_state(path: &Path, state: &TaskStateFile) -> Result<(), TaskStateError> {
    if state.schema_version != TASK_STATE_SCHEMA_VERSION {
        return Err(TaskStateError::Malformed {
            path: path.display().to_string(),
            reason: format!(
                "schema_version is {}, expected {TASK_STATE_SCHEMA_VERSION}",
                state.schema_version
            ),
        });
    }
    for (run, entry) in &state.tasks {
        if run.trim().is_empty() || run != run.trim() {
            return Err(TaskStateError::Malformed {
                path: path.display().to_string(),
                reason: "task state contains an empty or untrimmed run id".to_string(),
            });
        }
        if let Some(title) = entry.title.as_deref() {
            validate_title(title).map_err(|error| TaskStateError::Malformed {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        }
        if entry
            .side_parent
            .as_ref()
            .is_some_and(|parent| parent.as_str() == run)
        {
            return Err(TaskStateError::Malformed {
                path: path.display().to_string(),
                reason: format!("side run '{run}' names itself as parent"),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_rename_and_side_projection() {
        let root = tempfile::tempdir().unwrap();
        let run = RunId::new("run_side");
        let parent = RunId::new("run_parent");
        let mut state = TaskStateStore::open(root.path()).unwrap();
        state.rename(&run, "Quick question").unwrap();
        state.mark_side(&run, &parent).unwrap();

        let reopened = TaskStateStore::open(root.path()).unwrap();
        let presentation = reopened.presentation(&run);
        assert_eq!(presentation.title.as_deref(), Some("Quick question"));
        assert!(presentation.hidden);
        assert_eq!(presentation.side_parent, Some(parent));
    }
}
