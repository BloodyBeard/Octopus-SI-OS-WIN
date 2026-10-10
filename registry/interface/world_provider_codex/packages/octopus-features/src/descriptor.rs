use std::path::Path;
use std::path::PathBuf;

use octopus_descriptors::loader;
use octopus_descriptors::Descriptor;
use octopus_descriptors::DescriptorError;
use serde::Deserialize;
use serde::Serialize;

use crate::Stage;

#[cfg(test)]
static FEATURES_ROOT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
std::thread_local! {
    static FEATURES_ROOT_ENV_LOCK_HELD: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) struct FeaturesRootEnvGuard {
    prev: Option<std::ffi::OsString>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl FeaturesRootEnvGuard {
    pub(crate) fn set(root: &Path) -> Self {
        let guard = FEATURES_ROOT_ENV_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        FEATURES_ROOT_ENV_LOCK_HELD.with(|held| held.set(true));
        let prev = std::env::var_os(FEATURES_ROOT_ENV);
        unsafe { std::env::set_var(FEATURES_ROOT_ENV, root) };
        Self {
            prev,
            _guard: guard,
        }
    }
}

#[cfg(test)]
impl Drop for FeaturesRootEnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(value) => unsafe { std::env::set_var(FEATURES_ROOT_ENV, value) },
            None => unsafe { std::env::remove_var(FEATURES_ROOT_ENV) },
        }
        FEATURES_ROOT_ENV_LOCK_HELD.with(|held| held.set(false));
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeatureId(String);

impl FeatureId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for FeatureId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for FeatureId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for FeatureId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StageDescriptor {
    UnderDevelopment,
    Experimental {
        name: String,
        menu_description: String,
        announcement: String,
    },
    Stable,
    Deprecated,
    Removed,
}

impl From<Stage> for StageDescriptor {
    fn from(stage: Stage) -> Self {
        match stage {
            Stage::UnderDevelopment => StageDescriptor::UnderDevelopment,
            Stage::Experimental {
                name,
                menu_description,
                announcement,
            } => StageDescriptor::Experimental {
                name: name.to_string(),
                menu_description: menu_description.to_string(),
                announcement: announcement.to_string(),
            },
            Stage::Stable => StageDescriptor::Stable,
            Stage::Deprecated => StageDescriptor::Deprecated,
            Stage::Removed => StageDescriptor::Removed,
        }
    }
}

impl StageDescriptor {
    pub fn to_static_stage(&self) -> Stage {
        match self {
            StageDescriptor::UnderDevelopment => Stage::UnderDevelopment,
            StageDescriptor::Experimental {
                name,
                menu_description,
                announcement,
            } => Stage::Experimental {
                name: Box::leak(name.clone().into_boxed_str()),
                menu_description: Box::leak(menu_description.clone().into_boxed_str()),
                announcement: Box::leak(announcement.clone().into_boxed_str()),
            },
            StageDescriptor::Stable => Stage::Stable,
            StageDescriptor::Deprecated => Stage::Deprecated,
            StageDescriptor::Removed => Stage::Removed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureDescriptor {
    pub id: FeatureId,
    pub stage: StageDescriptor,
    pub default_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

impl Descriptor for FeatureDescriptor {
    const KIND: &'static str = "feature";
    const SCHEMA_VERSION: u32 = 1;
    const CONTRACT_VERSION: u32 = 1;
}

pub const FEATURES_ROOT_ENV: &str = "OCTOPUS_FEATURES_ROOT";

pub fn features_root() -> PathBuf {
    #[cfg(test)]
    {
        let already_holding_root_lock = FEATURES_ROOT_ENV_LOCK_HELD.with(|held| held.get());
        if already_holding_root_lock {
            return features_root_unlocked();
        }
        let _guard = FEATURES_ROOT_ENV_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        return features_root_unlocked();
    }

    #[cfg(not(test))]
    {
        features_root_unlocked()
    }
}

fn features_root_unlocked() -> PathBuf {
    if let Some(dir) = std::env::var_os(FEATURES_ROOT_ENV) {
        return PathBuf::from(dir);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("features")
}

pub fn load_feature_descriptors_from_disk(
) -> octopus_descriptors::Result<Vec<(String, FeatureDescriptor)>> {
    let root = features_root();
    let kind_prefix = format!("{}/", FeatureDescriptor::KIND);
    let loaded = loader::load_kind::<FeatureDescriptor>(&root)?;
    let mut out = Vec::with_capacity(loaded.len());
    for (name, desc) in loaded {
        let leaf = name
            .as_str()
            .strip_prefix(&kind_prefix)
            .unwrap_or(name.as_str())
            .to_string();
        if desc.id.as_str() != leaf {
            return Err(DescriptorError::Io(format!(
                "feature descriptor id {:?} does not match its directory {:?} (directory == identity)",
                desc.id.as_str(),
                leaf
            )));
        }
        out.push((leaf, desc));
    }
    out.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureSet {
    enabled: std::collections::BTreeSet<FeatureId>,
    head_hash: octopus_descriptors::DescriptorHash,
}

impl FeatureSet {
    pub fn from_descriptors<'a>(
        descs: impl IntoIterator<Item = (&'a str, &'a FeatureDescriptor)>,
    ) -> Self {
        let mut enabled = std::collections::BTreeSet::new();
        let mut head_map = std::collections::BTreeMap::new();
        for (name, desc) in descs {
            if desc.default_enabled {
                enabled.insert(desc.id.clone());
            }
            let value = serde_json::to_value(desc)
                .expect("FeatureDescriptor always serializes to a JSON value");
            let hash = octopus_descriptors::DescriptorHash::of_canonical_json(&value);
            head_map.insert(name.to_string(), hash.to_hex());
        }
        let head_value = serde_json::to_value(&head_map)
            .expect("BTreeMap<String,String> always serializes to a JSON value");
        let head_hash = octopus_descriptors::DescriptorHash::of_canonical_json(&head_value);
        Self { enabled, head_hash }
    }

    pub fn from_store(
        store: &octopus_descriptors::DescriptorStore,
    ) -> octopus_descriptors::Result<Self> {
        let kind_prefix = format!(
            "{}/",
            <FeatureDescriptor as octopus_descriptors::Descriptor>::KIND
        );
        let mut parsed: Vec<(String, FeatureDescriptor)> = Vec::new();
        for (name, hash) in store.heads_with_prefix(&kind_prefix) {
            let pinned = store.get(&hash)?;
            let desc: FeatureDescriptor =
                octopus_descriptors::parse_strict::<FeatureDescriptor>(&pinned.bytes)?;
            let leaf = name
                .as_str()
                .strip_prefix(&kind_prefix)
                .unwrap_or(name.as_str())
                .to_string();
            parsed.push((leaf, desc));
        }
        Ok(Self::from_descriptors(
            parsed.iter().map(|(n, d)| (n.as_str(), d)),
        ))
    }

    pub fn load_into_store(
        store: &mut octopus_descriptors::DescriptorStore,
    ) -> octopus_descriptors::Result<()> {
        loader::load_into_store::<FeatureDescriptor>(&features_root(), store)?;
        Ok(())
    }

    pub fn enabled(&self, id: &FeatureId) -> bool {
        self.enabled.contains(id)
    }

    pub fn insert(&mut self, id: FeatureId) {
        self.enabled.insert(id);
    }

    pub fn remove(&mut self, id: &FeatureId) {
        self.enabled.remove(id);
    }

    pub fn enabled_ids(&self) -> impl Iterator<Item = &FeatureId> {
        self.enabled.iter()
    }

    pub fn head_hash(&self) -> octopus_descriptors::DescriptorHash {
        self.head_hash
    }
}
