use thiserror::Error;

pub trait RegistryProjection: Send + Sync {
    fn read(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<Vec<u8>, InterfaceRegistryProjectionError>;
}

#[derive(Debug, Error)]
pub enum InterfaceRegistryProjectionError {
    #[error("interface registry projection protocol error: {0}")]
    Protocol(String),
}
