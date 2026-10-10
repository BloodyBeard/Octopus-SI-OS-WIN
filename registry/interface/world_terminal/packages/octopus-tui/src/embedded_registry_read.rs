use std::sync::Arc;

use octopus_ledger::{Lamport, MovementKind, MovementLog, Seq};
use octopus_runtime::RunId;

pub(crate) struct RegistryRead {
    config: Option<Vec<(String, usize)>>,
    runs: Option<Arc<dyn RunSource>>,
    account: Option<AccountIdentity>,
}

#[derive(Clone, Debug)]
pub(crate) struct AccountIdentity {
    pub(crate) id: String,
    pub(crate) display: Option<String>,
    pub(crate) plan: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunSummary {
    pub(crate) run: RunId,
    pub(crate) first_movement: (Seq, Lamport),
    pub(crate) last_movement: (Seq, Lamport),
    pub(crate) movement_count: u64,
    pub(crate) marks: Vec<String>,
}

pub(crate) trait RunSource: Send + Sync {
    fn list_runs(&self) -> Vec<RunId>;

    fn run_summaries(&self) -> Vec<RunSummary> {
        self.list_runs()
            .into_iter()
            .map(|run| RunSummary {
                run,
                first_movement: (Seq(0), Lamport(0)),
                last_movement: (Seq(0), Lamport(0)),
                movement_count: 0,
                marks: Vec::new(),
            })
            .collect()
    }
}

impl<L: MovementLog + Send + Sync> RunSource for std::sync::Mutex<L> {
    fn list_runs(&self) -> Vec<RunId> {
        let guard = self.lock().expect("ledger mutex is not poisoned");
        let mut seen: Vec<RunId> = Vec::new();
        for movement in guard.read_from(Seq(0)) {
            if let Some(run) = movement.run() && !seen.contains(run) {
                seen.push(run.clone());
            }
        }
        seen
    }

    fn run_summaries(&self) -> Vec<RunSummary> {
        let guard = self.lock().expect("ledger mutex is not poisoned");
        let mut order: Vec<RunId> = Vec::new();
        let mut acc: std::collections::HashMap<
            RunId,
            (Seq, Lamport, Seq, Lamport, u64, Vec<String>),
        > = std::collections::HashMap::new();
        for movement in guard.read_from(Seq(0)) {
            if let Some(run) = movement.run() {
                let entry = acc.entry(run.clone()).or_insert_with(|| {
                    order.push(run.clone());
                    (
                        movement.seq,
                        movement.lamport,
                        movement.seq,
                        movement.lamport,
                        0,
                        Vec::new(),
                    )
                });
                entry.2 = movement.seq;
                entry.3 = movement.lamport;
                entry.4 += 1;
                if let MovementKind::MarkOutcomeAdmitted(mark) = &movement.kind {
                    let name = mark.as_str().to_string();
                    if !entry.5.contains(&name) {
                        entry.5.push(name);
                    }
                }
            }
        }
        order
            .into_iter()
            .map(|run| {
                let (first_seq, first_lamport, last_seq, last_lamport, count, marks) =
                    acc.remove(&run).expect("accumulated run");
                RunSummary {
                    run,
                    first_movement: (first_seq, first_lamport),
                    last_movement: (last_seq, last_lamport),
                    movement_count: count,
                    marks,
                }
            })
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RegistryReadError {
    #[error("registry config projection has no config source")]
    MissingConfig,
    #[error("registry run projection has no ledger source")]
    MissingRuns,
}

impl RegistryRead {
    pub(crate) fn new() -> Self {
        Self {
            config: None,
            runs: None,
            account: None,
        }
    }

    pub(crate) fn with_config(mut self, config: Vec<(String, usize)>) -> Self {
        self.config = Some(config);
        self
    }

    pub(crate) fn with_runs(mut self, runs: Arc<dyn RunSource>) -> Self {
        self.runs = Some(runs);
        self
    }

    pub(crate) fn with_account(mut self, account: AccountIdentity) -> Self {
        self.account = Some(account);
        self
    }

    pub(crate) fn account(&self) -> Option<&AccountIdentity> {
        self.account.as_ref()
    }

    pub(crate) fn config_summary(&self) -> Result<&[(String, usize)], RegistryReadError> {
        self.config
            .as_deref()
            .ok_or(RegistryReadError::MissingConfig)
    }

    pub(crate) fn run_summaries(&self) -> Result<Vec<RunSummary>, RegistryReadError> {
        self.runs
            .as_ref()
            .map(|runs| runs.run_summaries())
            .ok_or(RegistryReadError::MissingRuns)
    }
}
