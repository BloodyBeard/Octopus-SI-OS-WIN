use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallerAuthorityStatus {
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct UpdateDiagnostics {
    pub schema_version: u32,
    pub client_version: String,
    pub status: InstallerAuthorityStatus,
    pub action_available: bool,
    pub signed_distribution_manifest_present: bool,
    pub installer_receipt_authority_present: bool,
    pub network_check_performed: bool,
    pub background_timer_running: bool,
    pub reason: String,
}

pub fn update_diagnostics(client_version: impl Into<String>) -> UpdateDiagnostics {
    UpdateDiagnostics {
        schema_version: 1,
        client_version: client_version.into(),
        status: InstallerAuthorityStatus::Unavailable,
        action_available: false,
        signed_distribution_manifest_present: false,
        installer_receipt_authority_present: false,
        network_check_performed: false,
        background_timer_running: false,
        reason: "world_terminal has no declared signed client distribution manifest and no installer receipt authority".to_string(),
    }
}
