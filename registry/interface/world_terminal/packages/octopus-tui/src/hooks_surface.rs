use color_eyre::eyre::Result;
use octopus_world_terminal_client::HookCatalog;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HookTrustUpdate {
    pub(crate) key: String,
    pub(crate) current_hash: String,
}

pub(crate) async fn fetch_hooks_catalog(cwd: PathBuf) -> Result<HookCatalog> {
    let _ = cwd;
    crate::unavailable_interface_action::fail("hooks catalog")
}

pub(crate) async fn write_hook_trusts(trust_updates: Vec<HookTrustUpdate>) -> Result<()> {
    let _ = trust_updates;
    crate::unavailable_interface_action::fail("hook trust update")
}

pub(crate) async fn write_hook_trust(key: String, current_hash: String) -> Result<()> {
    write_hook_trusts(vec![HookTrustUpdate { key, current_hash }]).await
}
