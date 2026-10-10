use octopus_interfaces::{InterfaceArtifactFile, InterfaceArtifactFileError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InterfaceArrivalPayloadError {
    #[error("interface artifact file error: {0}")]
    ArtifactFile(#[from] InterfaceArtifactFileError),
}

pub fn resolve_interface_arrival_payload(
    artifact_file: &InterfaceArtifactFile,
) -> Result<InterfaceArtifactFile, InterfaceArrivalPayloadError> {
    Ok(artifact_file.require_captured()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrival_requires_an_interface_captured_carrier() {
        let root = std::env::temp_dir().join(format!(
            "octopus-arrival-capture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let artifact = InterfaceArtifactFile::write(&root, "arrival", b"before").unwrap();
        let manifest = artifact.read_manifest().unwrap();
        assert!(resolve_interface_arrival_payload(&artifact).is_err());
        assert!(artifact.capture_bounded(1024, 5).is_err());
        let captured = artifact.capture_bounded(1024, 6).unwrap();
        let captured = resolve_interface_arrival_payload(&captured).unwrap();

        std::fs::write(&manifest.payload_path, b"after!").unwrap();
        std::fs::write(artifact.manifest_path(), b"replaced").unwrap();

        assert_eq!(captured.read_payload_bytes().unwrap(), b"before");
        std::fs::remove_dir_all(root).unwrap();
    }
}
