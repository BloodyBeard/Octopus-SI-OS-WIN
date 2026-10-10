use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use octopus_descriptors::RootAccess;

use crate::descriptor::{NetworkCeiling, WorkerEnvKindSpec};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum RootGrant {
    Top,
    Only(BTreeSet<String>),
}

impl RootGrant {
    fn intersect(&self, names: &[&str]) -> RootGrant {
        let incoming: BTreeSet<String> = names.iter().map(|s| s.to_string()).collect();
        match self {
            RootGrant::Top => RootGrant::Only(incoming),
            RootGrant::Only(existing) => {
                RootGrant::Only(existing.intersection(&incoming).cloned().collect())
            }
        }
    }
    fn admits(&self, name: &str) -> bool {
        match self {
            RootGrant::Top => true,
            RootGrant::Only(set) => set.contains(name),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerEnvCaps {
    writable: RootGrant,
    readable: RootGrant,
    network: NetworkCeiling,
}

impl WorkerEnvCaps {
    pub fn top() -> Self {
        Self {
            writable: RootGrant::Top,
            readable: RootGrant::Top,
            network: NetworkCeiling::Full,
        }
    }

    pub fn from_kind_boundary(spec: &WorkerEnvKindSpec) -> Self {
        let bd = &spec.boundary_defaults;
        let writable: Vec<&str> = bd
            .write_root_refs
            .iter()
            .filter(|r| r.access.is_empty() || r.access.contains(&RootAccess::Write))
            .map(|r| r.path.as_str())
            .collect();
        let readable: Vec<&str> = bd
            .read_root_refs
            .iter()
            .filter(|r| r.access.is_empty() || r.access.contains(&RootAccess::Read))
            .map(|r| r.path.as_str())
            .collect();
        Self::top()
            .intersect_writable(&writable)
            .intersect_readable(&readable)
            .intersect_network(bd.network_ceiling)
    }

    pub fn from_formed_root_slots(
        readable: BTreeSet<String>,
        writable: BTreeSet<String>,
        network: NetworkCeiling,
    ) -> Self {
        Self {
            readable: RootGrant::Only(readable),
            writable: RootGrant::Only(writable),
            network,
        }
    }

    /// notes2 2026-07-11 #3 — root caps are FORMED, not a constant: they are the
    /// intersection of three legs — the worker_environment_kind boundary defaults,
    /// the host root-slots the door instance actually exposes, and the run's EE
    /// chain allowance. A boundary root the door does not provide, or that the EE
    /// chain does not permit, is stripped (fail-closed, no widening). This is the
    /// door + EE that a bare `from_kind_boundary` seed lacks.
    pub fn form(
        spec: &WorkerEnvKindSpec,
        door_root_bindings: &BTreeMap<String, PathBuf>,
        ee_root_allowance: Option<&BTreeSet<String>>,
    ) -> Self {
        let door: Vec<&str> = door_root_bindings.keys().map(String::as_str).collect();
        let mut caps = Self::from_kind_boundary(spec)
            .intersect_writable(&door)
            .intersect_readable(&door);
        if let Some(allowed) = ee_root_allowance {
            let ee: Vec<&str> = allowed.iter().map(String::as_str).collect();
            caps = caps.intersect_writable(&ee).intersect_readable(&ee);
        }
        caps
    }

    pub fn intersect_writable(self, roots: &[&str]) -> Self {
        Self {
            writable: self.writable.intersect(roots),
            ..self
        }
    }

    pub fn intersect_readable(self, roots: &[&str]) -> Self {
        Self {
            readable: self.readable.intersect(roots),
            ..self
        }
    }

    pub fn intersect_network(self, n: NetworkCeiling) -> Self {
        Self {
            network: self.network.min(n),
            ..self
        }
    }

    pub fn admits_writable(&self, root_name: &str) -> bool {
        self.writable.admits(root_name)
    }
    pub fn admits_readable(&self, root_name: &str) -> bool {
        self.readable.admits(root_name)
    }
    pub fn network(&self) -> NetworkCeiling {
        self.network
    }

    pub fn explicit_writable(&self) -> Option<&BTreeSet<String>> {
        match &self.writable {
            RootGrant::Top => None,
            RootGrant::Only(set) => Some(set),
        }
    }

    pub fn readable_root_names(&self) -> BTreeSet<String> {
        match &self.readable {
            RootGrant::Top => BTreeSet::new(),
            RootGrant::Only(set) => set.clone(),
        }
    }

    pub fn writable_root_names(&self) -> BTreeSet<String> {
        match &self.writable {
            RootGrant::Top => BTreeSet::new(),
            RootGrant::Only(set) => set.clone(),
        }
    }
}
