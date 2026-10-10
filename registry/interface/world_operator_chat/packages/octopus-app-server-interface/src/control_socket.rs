use std::path::PathBuf;

use octopus_descriptors::InterfaceId;
use sha2::{Digest, Sha256};

pub fn interface_control_socket_path_from_root(
    control_root: &std::path::Path,
    interface: &InterfaceId,
) -> PathBuf {
    let digest = format!("{:x}", Sha256::digest(interface.as_str().as_bytes()));
    control_root.join(format!("{}.sock", &digest[..24]))
}
