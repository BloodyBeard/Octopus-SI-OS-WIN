use octopus_descriptors::Descriptor;
use octopus_descriptors::DescriptorHash;
use octopus_descriptors::DescriptorName;
use octopus_descriptors::DescriptorStore;
use serde::Deserialize;
use serde::Serialize;

pub const CONFIG_SHELL_NAME: &str = "effective";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigShellDescriptor {
    pub body: serde_json::Value,
}

impl Descriptor for ConfigShellDescriptor {
    const KIND: &'static str = "config_shell";
    const SCHEMA_VERSION: u32 = 1;
    const CONTRACT_VERSION: u32 = 1;
}

impl ConfigShellDescriptor {
    pub fn from_effective_config(effective: &toml::Value) -> octopus_descriptors::Result<Self> {
        let body = serde_json::to_value(effective)?;
        Ok(Self { body })
    }

    pub fn put_into_store(
        &self,
        store: &mut DescriptorStore,
    ) -> octopus_descriptors::Result<DescriptorHash> {
        let value = serde_json::to_value(self)?;
        let hash = store.put(
            Self::KIND,
            Self::SCHEMA_VERSION,
            Self::CONTRACT_VERSION,
            &value,
        );
        let name = DescriptorName::new(format!("{}/{}", Self::KIND, CONFIG_SHELL_NAME));
        store.set_head(name, hash)?;
        Ok(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_shell_identity_is_key_order_independent_and_bound_fail_closed() {
        let a: toml::Value = toml::from_str("b = 1\na = 2\n[t]\ny = 0\nx = 1\n").unwrap();
        let b: toml::Value = toml::from_str("a = 2\nb = 1\n[t]\nx = 1\ny = 0\n").unwrap();

        let mut store = DescriptorStore::new();
        let ha = ConfigShellDescriptor::from_effective_config(&a)
            .unwrap()
            .put_into_store(&mut store)
            .unwrap();
        let hb = ConfigShellDescriptor::from_effective_config(&b)
            .unwrap()
            .put_into_store(&mut store)
            .unwrap();
        assert_eq!(
            ha, hb,
            "config-shell identity must be key-order independent"
        );
        assert_eq!(store.len(), 1, "idempotent by content");

        let name = DescriptorName::new(format!(
            "{}/{}",
            ConfigShellDescriptor::KIND,
            CONFIG_SHELL_NAME
        ));
        assert_eq!(store.resolve_head(&name).unwrap(), ha);

        let pinned = store.get(&ha).unwrap();
        let parsed: ConfigShellDescriptor =
            octopus_descriptors::parse_strict::<ConfigShellDescriptor>(&pinned.bytes).unwrap();
        assert_eq!(
            parsed.body,
            ConfigShellDescriptor::from_effective_config(&a)
                .unwrap()
                .body
        );
    }
}
