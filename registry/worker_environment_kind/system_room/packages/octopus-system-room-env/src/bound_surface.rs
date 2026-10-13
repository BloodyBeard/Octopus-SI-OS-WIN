use std::path::{Path, PathBuf};

use octopus_interfaces::{ArtifactBindingPayload, GrantProjection, InterfaceArtifactFile};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_worker_env::{AbsPath, ArtifactSurface, BoundArtifact, WorkerEnvError};

pub struct InvocationArtifactSurface {
    bound: Vec<ArtifactBindingPayload>,
    emission_root: PathBuf,
    artifact_blobs_root: Option<PathBuf>,
    grant: GrantProjection,
    maximum_bytes: u64,
}

impl InvocationArtifactSurface {
    pub fn new(
        bound: &[ArtifactBindingPayload],
        emission_root: &Path,
        grant: GrantProjection,
        artifact_blobs_root: Option<&Path>,
        maximum_bytes: u64,
    ) -> Result<Self, WorkerEnvError> {
        let emission_root = AbsPath::new(emission_root)
            .map_err(|error| {
                WorkerEnvError::ArtifactSurfaceFailed(format!(
                    "system_context invocation output path failed closed: {error}"
                ))
            })?
            .as_path()
            .to_path_buf();
        Ok(Self {
            bound: bound.to_vec(),
            emission_root,
            artifact_blobs_root: artifact_blobs_root.map(Path::to_path_buf),
            grant,
            maximum_bytes,
        })
    }

    pub fn append_impl(&self, label: &str, payload: &[u8]) -> Result<ArtifactRef, WorkerEnvError> {
        if !self.grant.may_create_artifact() {
            return Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context append refuses to create an artifact: the formed grant carries no create verb (POLICY.md §4 object-level artifact policy)".into(),
            ));
        }
        let label = label.trim();
        if label.is_empty() {
            return Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context append refuses an empty artifact label".into(),
            ));
        }
        let root = self.artifact_blobs_root.as_ref().ok_or_else(|| {
            WorkerEnvError::ArtifactSurfaceFailed("artifact creation requires the declared artifact_blobs_root write grant".into())
        })?;
        let artifact_file = InterfaceArtifactFile::write(&self.emission_root, label, payload)
            .map_err(|error| {
                WorkerEnvError::ArtifactSurfaceFailed(format!(
                    "system_context append could not materialize an artifact: {error}"
                ))
            })?;
        let manifest_bytes = std::fs::read(artifact_file.manifest_path()).map_err(|error| {
            WorkerEnvError::ArtifactSurfaceFailed(format!(
                "system_context append could not read manifest '{}': {error}",
                artifact_file.manifest_path().display()
            ))
        })?;
        let content = ContentHash::of_bytes(&manifest_bytes);
        let artifact = ArtifactRef {
            id: octopus_descriptors::ArtifactId::new(format!("{label}:{}", content.to_hex())),
            content,
        };
        let mut blobs = octopus_ledger_store::DurableBlobs::open(root)
            .map_err(|e| WorkerEnvError::ArtifactSurfaceFailed(e.to_string()))?;
        blobs.try_put(payload).and_then(|_| blobs.try_put(&manifest_bytes))
            .map_err(|e| WorkerEnvError::ArtifactSurfaceFailed(e.to_string()))?;
        Ok(artifact)
    }

    pub fn read_impl(&self, id: &str) -> Result<Option<Vec<u8>>, WorkerEnvError> {
        let mut matches = self.bound.iter().filter(|bound| {
            bound.artifact.id.as_str() == id || bound.artifact.content.to_hex() == id
        });
        let Some(first) = matches.next() else {
            return Ok(None);
        };
        if matches.next().is_some() {
            return Err(WorkerEnvError::ArtifactSurfaceFailed(format!(
                "system_context bound artifact reference '{id}' is ambiguous"
            )));
        }
        if !self.grant.may_read_artifact(first.artifact.id.as_str()) {
            return Err(WorkerEnvError::ArtifactSurfaceFailed(format!(
                "system_context artifact '{}' is not readable under the formed grant (POLICY.md §4 object-level read)",
                first.artifact.id.as_str()
            )));
        }
        first.read_bounded(self.maximum_bytes)
            .map(|bytes| Some(bytes.into_owned()))
            .map_err(|error| WorkerEnvError::ArtifactSurfaceFailed(error.to_string()))
    }

    pub fn list_impl(&self) -> Result<Vec<BoundArtifact>, WorkerEnvError> {
        let mut result = Vec::new();
        let mut retained = 0u64;
        for bound in self.bound.iter().filter(|bound| self.grant.may_read_artifact(bound.artifact.id.as_str())) {
            let bytes = bound.read_bounded(self.maximum_bytes - retained)
                .map_err(|error| WorkerEnvError::ArtifactSurfaceFailed(error.to_string()))?;
            let text = match std::str::from_utf8(&bytes) {
                Ok(_) => {
                    retained = retained.checked_add(bytes.len() as u64).filter(|size| *size <= self.maximum_bytes)
                        .ok_or_else(|| WorkerEnvError::ArtifactSurfaceFailed("bound artifact list exceeds its retained text ceiling".into()))?;
                    Some(String::from_utf8(bytes.into_owned())
                        .map_err(|error| WorkerEnvError::ArtifactSurfaceFailed(error.to_string()))?)
                }
                Err(_) => None,
            };
            result.push(BoundArtifact { artifact: bound.artifact.clone(), text });
        }
        Ok(result)
    }
}

impl ArtifactSurface for InvocationArtifactSurface {
    fn append(
        &self,
        label: &str,
        payload: &[u8],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ArtifactRef, WorkerEnvError>> + Send + '_>,
    > {
        let label = label.to_string();
        let payload = payload.to_vec();
        Box::pin(async move { self.append_impl(&label, &payload) })
    }

    fn read(
        &self,
        id: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<Vec<u8>>, WorkerEnvError>> + Send + '_>,
    > {
        let id = id.to_string();
        Box::pin(async move { self.read_impl(&id) })
    }

    fn list(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<BoundArtifact>, WorkerEnvError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move { self.list_impl() })
    }
}
