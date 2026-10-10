use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const STATE_SCHEMA_VERSION: u32 = 1;
const MAX_CREDENTIALS: usize = 128;
const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_REASON_CODE_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialPoolStrategy {
    FillFirst,
    RoundRobin,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialSpec {
    pub id: String,
    pub secret_ref: String,
    #[serde(default)]
    pub priority: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialFailureRule {
    #[serde(default)]
    pub same_credential_retries: u32,
    pub cooldown_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialPoolConfig {
    pub strategy: CredentialPoolStrategy,
    pub credentials: Vec<CredentialSpec>,
    pub failure_rules: BTreeMap<u16, CredentialFailureRule>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialSelection {
    id: String,
    secret_ref: String,
    fingerprint: String,
}

impl CredentialSelection {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn secret_ref(&self) -> &str {
        &self.secret_ref
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialFailure {
    pub status: u16,
    pub reason_code: String,
    pub reset_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialPoolDecision {
    RetrySame(CredentialSelection),
    Rotate(CredentialSelection),
    Exhausted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialPoolAttempt {
    Selected(CredentialSelection),
    Exhausted {
        next_available_at_unix_ms: Option<u64>,
    },
}

pub struct CredentialPool {
    interface: String,
    config: ValidatedConfig,
    state_path: PathBuf,
    state: Mutex<CredentialPoolState>,
}

#[derive(Clone)]
struct ValidatedConfig {
    strategy: CredentialPoolStrategy,
    credentials: Vec<ValidatedCredential>,
    failure_rules: BTreeMap<u16, CredentialFailureRule>,
    digest: String,
}

#[derive(Clone)]
struct ValidatedCredential {
    id: String,
    secret_ref: String,
    fingerprint: String,
    priority: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CredentialPoolState {
    schema_version: u32,
    interface: String,
    config_digest: String,
    last_selected: Option<String>,
    entries: BTreeMap<String, CredentialState>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CredentialState {
    request_count: u64,
    consecutive_status: Option<u16>,
    consecutive_failures: u32,
    cooldown_until_unix_ms: Option<u64>,
    last_status: Option<u16>,
    last_reason_digest: Option<String>,
    last_used_at_unix_ms: Option<u64>,
}

impl CredentialPoolConfig {
    pub fn validate_for_interface(&self, interface: &str) -> Result<(), String> {
        ValidatedConfig::new(interface, self.clone()).map(|_| ())
    }
}

impl CredentialPool {
    pub fn open(
        interface: impl Into<String>,
        config: CredentialPoolConfig,
        state_path: impl Into<PathBuf>,
    ) -> Result<Self, String> {
        let interface = interface.into();
        validate_identifier(&interface, "credential pool interface")?;
        let config = ValidatedConfig::new(&interface, config)?;
        let state_path = state_path.into();
        if state_path.as_os_str().is_empty() {
            return Err("credential pool state path must not be empty".to_string());
        }
        let state = load_state(&state_path, &interface, &config)?;
        let pool = Self {
            interface,
            config,
            state_path,
            state: Mutex::new(state),
        };
        pool.persist()?;
        Ok(pool)
    }

    pub fn start_attempt(
        &self,
        selected: Option<&CredentialSelection>,
        now_unix_ms: u64,
    ) -> Result<CredentialPoolAttempt, String> {
        let mut state = self.lock_state()?;
        clear_expired_cooldowns(&mut state, now_unix_ms);
        let selected = match self.select_locked(&state, selected)? {
            CredentialPoolAttempt::Selected(selected) => selected,
            exhausted @ CredentialPoolAttempt::Exhausted { .. } => return Ok(exhausted),
        };
        record_attempt_locked(&mut state, &selected, now_unix_ms)?;
        persist_state(&self.state_path, &state)?;
        Ok(CredentialPoolAttempt::Selected(selected))
    }

    pub fn select_candidate(
        &self,
        selected: Option<&CredentialSelection>,
        now_unix_ms: u64,
    ) -> Result<CredentialPoolAttempt, String> {
        let mut state = self.lock_state()?;
        clear_expired_cooldowns(&mut state, now_unix_ms);
        self.select_locked(&state, selected)
    }

    pub fn record_success(
        &self,
        selection: &CredentialSelection,
        now_unix_ms: u64,
    ) -> Result<(), String> {
        let credential = self.validate_selection(selection)?;
        let mut state = self.lock_state()?;
        let entry = state
            .entries
            .get_mut(&credential.id)
            .ok_or_else(|| "credential pool state lost a configured credential".to_string())?;
        entry.consecutive_status = None;
        entry.consecutive_failures = 0;
        entry.cooldown_until_unix_ms = None;
        entry.last_status = None;
        entry.last_reason_digest = None;
        entry.last_used_at_unix_ms = Some(now_unix_ms);
        persist_state(&self.state_path, &state)
    }

    pub fn record_failure(
        &self,
        selection: &CredentialSelection,
        failure: CredentialFailure,
        now_unix_ms: u64,
    ) -> Result<CredentialPoolDecision, String> {
        validate_reason_code(&failure.reason_code)?;
        let credential = self.validate_selection(selection)?;
        let rule = self
            .config
            .failure_rules
            .get(&failure.status)
            .ok_or_else(|| {
                format!(
                    "credential pool has no descriptor failure rule for HTTP {}",
                    failure.status
                )
            })?
            .clone();
        let mut state = self.lock_state()?;
        let entry = state
            .entries
            .get_mut(&credential.id)
            .ok_or_else(|| "credential pool state lost a configured credential".to_string())?;
        if entry.consecutive_status == Some(failure.status) {
            entry.consecutive_failures = entry
                .consecutive_failures
                .checked_add(1)
                .ok_or_else(|| "credential pool failure counter overflowed".to_string())?;
        } else {
            entry.consecutive_status = Some(failure.status);
            entry.consecutive_failures = 1;
        }
        entry.last_status = Some(failure.status);
        entry.last_reason_digest = Some(blake3_hex(failure.reason_code.as_bytes()));
        entry.last_used_at_unix_ms = Some(now_unix_ms);
        if entry.consecutive_failures <= rule.same_credential_retries {
            persist_state(&self.state_path, &state)?;
            return Ok(CredentialPoolDecision::RetrySame(selection.clone()));
        }
        let descriptor_cooldown = now_unix_ms
            .checked_add(rule.cooldown_ms)
            .ok_or_else(|| "credential pool cooldown timestamp overflowed".to_string())?;
        entry.cooldown_until_unix_ms = Some(
            failure
                .reset_at_unix_ms
                .map_or(descriptor_cooldown, |reset| reset.max(descriptor_cooldown)),
        );
        clear_expired_cooldowns(&mut state, now_unix_ms);
        let rotated = choose_locked(&self.config, &state, Some(credential.id.as_str()));
        persist_state(&self.state_path, &state)?;
        Ok(match rotated {
            Some(next) => CredentialPoolDecision::Rotate(next),
            None => CredentialPoolDecision::Exhausted,
        })
    }

    pub fn snapshot(&self) -> Result<serde_json::Value, String> {
        let state = self.lock_state()?;
        serde_json::to_value(&*state)
            .map_err(|error| format!("credential pool state cannot be projected: {error}"))
    }

    pub fn interface(&self) -> &str {
        &self.interface
    }

    pub fn manages_status(&self, status: u16) -> bool {
        self.config.failure_rules.contains_key(&status)
    }

    fn validate_selection<'a>(
        &'a self,
        selection: &CredentialSelection,
    ) -> Result<&'a ValidatedCredential, String> {
        let credential = self
            .config
            .credentials
            .iter()
            .find(|candidate| candidate.id == selection.id)
            .ok_or_else(|| "credential selection is outside this exact interface".to_string())?;
        if credential.secret_ref != selection.secret_ref
            || credential.fingerprint != selection.fingerprint
        {
            return Err("credential selection no longer matches descriptor data".to_string());
        }
        Ok(credential)
    }

    fn select_locked(
        &self,
        state: &CredentialPoolState,
        selected: Option<&CredentialSelection>,
    ) -> Result<CredentialPoolAttempt, String> {
        let selected = match selected {
            Some(selected) => {
                let credential = self.validate_selection(selected)?;
                let entry = state.entries.get(&credential.id).ok_or_else(|| {
                    "credential pool state lost a configured credential".to_string()
                })?;
                if entry.cooldown_until_unix_ms.is_some() {
                    return Ok(CredentialPoolAttempt::Exhausted {
                        next_available_at_unix_ms: next_available_at(state),
                    });
                }
                selected.clone()
            }
            None => match choose_locked(&self.config, state, None) {
                Some(selected) => selected,
                None => {
                    return Ok(CredentialPoolAttempt::Exhausted {
                        next_available_at_unix_ms: next_available_at(state),
                    });
                }
            },
        };
        Ok(CredentialPoolAttempt::Selected(selected))
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, CredentialPoolState>, String> {
        self.state
            .lock()
            .map_err(|_| "credential pool state mutex is poisoned".to_string())
    }

    fn persist(&self) -> Result<(), String> {
        let state = self.lock_state()?;
        persist_state(&self.state_path, &state)
    }
}

impl ValidatedConfig {
    fn new(interface: &str, config: CredentialPoolConfig) -> Result<Self, String> {
        if config.credentials.is_empty() {
            return Err("credential pool must declare at least one credential".to_string());
        }
        if config.credentials.len() > MAX_CREDENTIALS {
            return Err(format!(
                "credential pool may declare at most {MAX_CREDENTIALS} credentials"
            ));
        }
        let mut ids = BTreeSet::new();
        let mut handles = BTreeSet::new();
        let mut credentials = Vec::with_capacity(config.credentials.len());
        for spec in &config.credentials {
            validate_identifier(&spec.id, "credential id")?;
            if !ids.insert(spec.id.clone()) {
                return Err(format!("credential id '{}' is duplicated", spec.id));
            }
            validate_exact_secret_ref(interface, &spec.secret_ref)?;
            if !handles.insert(spec.secret_ref.clone()) {
                return Err(format!(
                    "credential secret handle '{}' is duplicated",
                    spec.secret_ref
                ));
            }
            credentials.push(ValidatedCredential {
                id: spec.id.clone(),
                secret_ref: spec.secret_ref.clone(),
                fingerprint: blake3_hex(spec.secret_ref.as_bytes()),
                priority: spec.priority,
            });
        }
        credentials.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.id.cmp(&right.id))
        });
        for (status, rule) in &config.failure_rules {
            if !(100..=599).contains(status) {
                return Err(format!(
                    "credential failure rule status {status} is outside HTTP status space"
                ));
            }
            if rule.cooldown_ms == 0 {
                return Err(format!(
                    "credential failure rule {status} must declare a positive cooldown_ms"
                ));
            }
            if config.strategy == CredentialPoolStrategy::RoundRobin
                && rule.same_credential_retries != 0
            {
                return Err(format!(
                    "round_robin credential failure rule {status} cannot retry the same credential"
                ));
            }
        }
        let digest_bytes = serde_json::to_vec(&config)
            .map_err(|error| format!("credential pool config cannot be encoded: {error}"))?;
        Ok(Self {
            strategy: config.strategy,
            credentials,
            failure_rules: config.failure_rules,
            digest: blake3_hex(&digest_bytes),
        })
    }
}

fn clear_expired_cooldowns(state: &mut CredentialPoolState, now_unix_ms: u64) {
    for entry in state.entries.values_mut() {
        if entry
            .cooldown_until_unix_ms
            .is_some_and(|until| until <= now_unix_ms)
        {
            entry.cooldown_until_unix_ms = None;
            entry.consecutive_status = None;
            entry.consecutive_failures = 0;
            entry.last_status = None;
            entry.last_reason_digest = None;
        }
    }
}

fn next_available_at(state: &CredentialPoolState) -> Option<u64> {
    state
        .entries
        .values()
        .filter_map(|entry| entry.cooldown_until_unix_ms)
        .min()
}

fn choose_locked(
    config: &ValidatedConfig,
    state: &CredentialPoolState,
    exclude_id: Option<&str>,
) -> Option<CredentialSelection> {
    let available = config
        .credentials
        .iter()
        .filter(|credential| exclude_id != Some(credential.id.as_str()))
        .filter(|credential| {
            state
                .entries
                .get(&credential.id)
                .is_some_and(|entry| entry.cooldown_until_unix_ms.is_none())
        })
        .collect::<Vec<_>>();
    let selected = match config.strategy {
        CredentialPoolStrategy::FillFirst => available.first().copied(),
        CredentialPoolStrategy::RoundRobin => {
            let start = state
                .last_selected
                .as_ref()
                .and_then(|last| {
                    config
                        .credentials
                        .iter()
                        .position(|credential| &credential.id == last)
                })
                .map_or(0, |index| (index + 1) % config.credentials.len());
            (0..config.credentials.len())
                .map(|offset| &config.credentials[(start + offset) % config.credentials.len()])
                .find(|credential| {
                    exclude_id != Some(credential.id.as_str())
                        && state
                            .entries
                            .get(&credential.id)
                            .is_some_and(|entry| entry.cooldown_until_unix_ms.is_none())
                })
        }
    }?;
    Some(CredentialSelection {
        id: selected.id.clone(),
        secret_ref: selected.secret_ref.clone(),
        fingerprint: selected.fingerprint.clone(),
    })
}

fn record_attempt_locked(
    state: &mut CredentialPoolState,
    selection: &CredentialSelection,
    now_unix_ms: u64,
) -> Result<(), String> {
    let entry = state
        .entries
        .get_mut(&selection.id)
        .ok_or_else(|| "credential pool state lost a configured credential".to_string())?;
    entry.request_count = entry
        .request_count
        .checked_add(1)
        .ok_or_else(|| "credential pool request counter overflowed".to_string())?;
    entry.last_used_at_unix_ms = Some(now_unix_ms);
    state.last_selected = Some(selection.id.clone());
    Ok(())
}

fn load_state(
    state_path: &Path,
    interface: &str,
    config: &ValidatedConfig,
) -> Result<CredentialPoolState, String> {
    if !state_path.exists() {
        return Ok(fresh_state(interface, config));
    }
    let bytes = std::fs::read(state_path).map_err(|error| {
        format!(
            "credential pool state '{}' cannot be read: {error}",
            state_path.display()
        )
    })?;
    let state: CredentialPoolState = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "credential pool state '{}' is malformed: {error}",
            state_path.display()
        )
    })?;
    if state.schema_version != STATE_SCHEMA_VERSION || state.interface != interface {
        return Err(
            "credential pool state identity is incompatible with this interface".to_string(),
        );
    }
    if state.config_digest != config.digest {
        return Ok(fresh_state(interface, config));
    }
    let expected = config
        .credentials
        .iter()
        .map(|credential| credential.id.clone())
        .collect::<BTreeSet<_>>();
    let actual = state.entries.keys().cloned().collect::<BTreeSet<_>>();
    if expected != actual {
        return Err("credential pool state entries diverge from descriptor data".to_string());
    }
    if state
        .last_selected
        .as_ref()
        .is_some_and(|id| !state.entries.contains_key(id))
    {
        return Err("credential pool state points at an unknown last selection".to_string());
    }
    Ok(state)
}

fn fresh_state(interface: &str, config: &ValidatedConfig) -> CredentialPoolState {
    CredentialPoolState {
        schema_version: STATE_SCHEMA_VERSION,
        interface: interface.to_string(),
        config_digest: config.digest.clone(),
        last_selected: None,
        entries: config
            .credentials
            .iter()
            .map(|credential| (credential.id.clone(), CredentialState::default()))
            .collect(),
    }
}

fn persist_state(path: &Path, state: &CredentialPoolState) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| {
        format!(
            "credential pool state path '{}' has no parent",
            path.display()
        )
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "credential pool state directory '{}' cannot be created: {error}",
            parent.display()
        )
    })?;
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("credential pool state cannot be encoded: {error}"))?;
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| {
            format!(
                "credential pool temporary state '{}' cannot be opened: {error}",
                temporary.display()
            )
        })?;
    file.write_all(&bytes).map_err(|error| {
        format!(
            "credential pool temporary state '{}' cannot be written: {error}",
            temporary.display()
        )
    })?;
    file.sync_all().map_err(|error| {
        format!(
            "credential pool temporary state '{}' cannot be synced: {error}",
            temporary.display()
        )
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        format!(
            "credential pool state '{}' cannot replace its prior generation: {error}",
            path.display()
        )
    })?;
    OpenOptions::new()
        .read(true)
        .open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "credential pool state directory '{}' cannot be synced: {error}",
                parent.display()
            )
        })
}

fn validate_exact_secret_ref(interface: &str, reference: &str) -> Result<(), String> {
    if reference.trim() != reference || reference.len() > 512 {
        return Err("credential secret handle must be trimmed and bounded".to_string());
    }
    let raw = reference
        .strip_prefix("secret://")
        .ok_or_else(|| "credential secret handle must start with secret://".to_string())?;
    let (scope, name) = raw.split_once('/').ok_or_else(|| {
        "credential secret handle must be secret://<exact-interface>/<name>".to_string()
    })?;
    if scope != interface {
        return Err(format!(
            "credential secret handle scope '{scope}' must equal exact interface '{interface}'"
        ));
    }
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
    {
        return Err("credential secret handle name must contain only A-Z, 0-9, or _".to_string());
    }
    Ok(())
}

fn validate_identifier(value: &str, surface: &str) -> Result<(), String> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        return Err(format!(
            "{surface} must be a bounded trimmed ASCII identifier"
        ));
    }
    Ok(())
}

fn validate_reason_code(reason: &str) -> Result<(), String> {
    if reason.is_empty()
        || reason.trim() != reason
        || reason.len() > MAX_REASON_CODE_BYTES
        || !reason.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        return Err("credential failure reason_code must be a bounded identifier".to_string());
    }
    Ok(())
}

fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(attempt: CredentialPoolAttempt) -> CredentialSelection {
        match attempt {
            CredentialPoolAttempt::Selected(selected) => selected,
            CredentialPoolAttempt::Exhausted { .. } => {
                panic!("credential pool unexpectedly exhausted")
            }
        }
    }

    fn config(strategy: CredentialPoolStrategy) -> CredentialPoolConfig {
        let same_credential_retries = u32::from(strategy == CredentialPoolStrategy::FillFirst);
        CredentialPoolConfig {
            strategy,
            credentials: vec![
                CredentialSpec {
                    id: "primary".to_string(),
                    secret_ref: "secret://world_provider_test/PRIMARY_KEY".to_string(),
                    priority: 0,
                },
                CredentialSpec {
                    id: "secondary".to_string(),
                    secret_ref: "secret://world_provider_test/SECONDARY_KEY".to_string(),
                    priority: 1,
                },
            ],
            failure_rules: BTreeMap::from([
                (
                    402,
                    CredentialFailureRule {
                        same_credential_retries: 0,
                        cooldown_ms: 86_400_000,
                    },
                ),
                (
                    429,
                    CredentialFailureRule {
                        same_credential_retries,
                        cooldown_ms: 3_600_000,
                    },
                ),
            ]),
        }
    }

    #[test]
    fn secret_handles_are_exact_interface_scoped() {
        let mut config = config(CredentialPoolStrategy::FillFirst);
        config.credentials[0].secret_ref = "secret://another/PRIMARY_KEY".to_string();
        let error = config
            .validate_for_interface("world_provider_test")
            .unwrap_err();
        assert!(error.contains("must equal exact interface"));
    }

    #[test]
    fn candidate_selection_does_not_record_an_attempt() {
        let root = tempfile::tempdir().unwrap();
        let pool = CredentialPool::open(
            "world_provider_test",
            config(CredentialPoolStrategy::FillFirst),
            root.path().join("pool.json"),
        )
        .unwrap();
        let candidate = selected(pool.select_candidate(None, 10).unwrap());
        assert_eq!(candidate.id(), "primary");
        let before = pool.snapshot().unwrap();
        assert_eq!(before["entries"]["primary"]["request_count"], 0);
        let started = selected(pool.start_attempt(Some(&candidate), 10).unwrap());
        assert_eq!(started, candidate);
        let after = pool.snapshot().unwrap();
        assert_eq!(after["entries"]["primary"]["request_count"], 1);
    }

    #[test]
    fn transient_429_retries_once_then_rotates() {
        let root = tempfile::tempdir().unwrap();
        let pool = CredentialPool::open(
            "world_provider_test",
            config(CredentialPoolStrategy::FillFirst),
            root.path().join("pool.json"),
        )
        .unwrap();
        let primary = selected(pool.start_attempt(None, 10).unwrap());
        assert_eq!(primary.id(), "primary");
        let first = pool
            .record_failure(
                &primary,
                CredentialFailure {
                    status: 429,
                    reason_code: "rate_limited".to_string(),
                    reset_at_unix_ms: None,
                },
                20,
            )
            .unwrap();
        assert_eq!(first, CredentialPoolDecision::RetrySame(primary.clone()));
        selected(pool.start_attempt(Some(&primary), 25).unwrap());
        let second = pool
            .record_failure(
                &primary,
                CredentialFailure {
                    status: 429,
                    reason_code: "rate_limited".to_string(),
                    reset_at_unix_ms: None,
                },
                30,
            )
            .unwrap();
        let CredentialPoolDecision::Rotate(secondary) = second else {
            panic!("second 429 must rotate")
        };
        assert_eq!(secondary.id(), "secondary");
        let before_secondary = pool.snapshot().unwrap();
        assert_eq!(before_secondary["entries"]["primary"]["request_count"], 2);
        assert_eq!(before_secondary["entries"]["secondary"]["request_count"], 0);
        selected(pool.start_attempt(Some(&secondary), 35).unwrap());
        let after_secondary = pool.snapshot().unwrap();
        assert_eq!(after_secondary["entries"]["secondary"]["request_count"], 1);
        assert_eq!(
            pool.record_failure(
                &secondary,
                CredentialFailure {
                    status: 429,
                    reason_code: "rate_limited".to_string(),
                    reset_at_unix_ms: None,
                },
                40,
            )
            .unwrap(),
            CredentialPoolDecision::RetrySame(secondary.clone())
        );
        selected(pool.start_attempt(Some(&secondary), 45).unwrap());
        assert_eq!(
            pool.record_failure(
                &secondary,
                CredentialFailure {
                    status: 429,
                    reason_code: "rate_limited".to_string(),
                    reset_at_unix_ms: None,
                },
                50,
            )
            .unwrap(),
            CredentialPoolDecision::Exhausted
        );
        assert!(matches!(
            pool.start_attempt(None, 55).unwrap(),
            CredentialPoolAttempt::Exhausted {
                next_available_at_unix_ms: Some(_)
            }
        ));
    }

    #[test]
    fn round_robin_selection_is_deterministic_and_durable() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("pool.json");
        let pool = CredentialPool::open(
            "world_provider_test",
            config(CredentialPoolStrategy::RoundRobin),
            &state,
        )
        .unwrap();
        assert_eq!(
            selected(pool.start_attempt(None, 1).unwrap()).id(),
            "primary"
        );
        assert_eq!(
            selected(pool.start_attempt(None, 2).unwrap()).id(),
            "secondary"
        );
        drop(pool);
        let reopened = CredentialPool::open(
            "world_provider_test",
            config(CredentialPoolStrategy::RoundRobin),
            state,
        )
        .unwrap();
        assert_eq!(
            selected(reopened.start_attempt(None, 3).unwrap()).id(),
            "primary"
        );
    }

    #[test]
    fn malformed_durable_state_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("pool.json");
        std::fs::write(&state, b"not-json").unwrap();
        let error = CredentialPool::open(
            "world_provider_test",
            config(CredentialPoolStrategy::FillFirst),
            state,
        )
        .err()
        .expect("malformed state must fail");
        assert!(error.contains("malformed"));
    }

    #[test]
    fn descriptor_change_rebuilds_only_derived_pool_state() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("pool.json");
        let pool = CredentialPool::open(
            "world_provider_test",
            config(CredentialPoolStrategy::RoundRobin),
            &state,
        )
        .unwrap();
        assert_eq!(
            selected(pool.start_attempt(None, 1).unwrap()).id(),
            "primary"
        );
        drop(pool);
        let mut changed = config(CredentialPoolStrategy::FillFirst);
        changed.credentials.remove(0);
        let reopened = CredentialPool::open("world_provider_test", changed, state).unwrap();
        assert_eq!(
            selected(reopened.start_attempt(None, 2).unwrap()).id(),
            "secondary"
        );
    }

    #[test]
    fn round_robin_rejects_same_credential_retry_rules() {
        let mut config = config(CredentialPoolStrategy::RoundRobin);
        config
            .failure_rules
            .get_mut(&429)
            .unwrap()
            .same_credential_retries = 1;
        let error = config
            .validate_for_interface("world_provider_test")
            .unwrap_err();
        assert!(error.contains("cannot retry the same credential"));
        config
            .failure_rules
            .get_mut(&429)
            .unwrap()
            .same_credential_retries = 0;
        config
            .validate_for_interface("world_provider_test")
            .unwrap();
    }
}
