use std::path::PathBuf;

use octopus_descriptors::InterfaceId;
use octopus_interface_transport::{InterfaceHostIoLimits, InterfaceLiftContext};
use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_world_pyecho_life_runtime::{LedgerTailReader, OwnOutboundMovement};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum TerminalOutboundError {
    #[error("terminal outbound reader could not open: {0}")]
    Open(String),
    #[error("terminal outbound read failed: {0}")]
    Read(String),
    #[error("terminal outbound cursor commit failed: {0}")]
    Commit(String),
    #[error("terminal outbound envelope rebuild failed: {0}")]
    Envelope(String),
}

pub struct TerminalOutboundReader {
    reader: LedgerTailReader,
    _ctx: InterfaceLiftContext,
}

impl TerminalOutboundReader {
    pub fn open(
        interface: InterfaceId,
        octopus_home: PathBuf,
        durable_root: PathBuf,
        host_io_limits: InterfaceHostIoLimits,
    ) -> Result<Self, TerminalOutboundError> {
        Self::open_named(
            interface,
            octopus_home,
            durable_root,
            "default",
            host_io_limits,
        )
    }

    pub fn open_named(
        interface: InterfaceId,
        octopus_home: PathBuf,
        durable_root: PathBuf,
        reader: &str,
        host_io_limits: InterfaceHostIoLimits,
    ) -> Result<Self, TerminalOutboundError> {
        let (arrivals, _rx) = tokio::sync::mpsc::channel(1);
        let interface_dir = PathBuf::new();
        let var_dir = octopus_home.join("interfaces").join(interface.as_str());
        let movements_path = durable_root.join("movements.jsonl");
        let blobs_dir = durable_root.join("blobs");
        let ledger_index_path = durable_root.join("ledger-index.sqlite");
        let ctx = InterfaceLiftContext::unprivileged(
            interface,
            interface_dir,
            None,
            var_dir,
            movements_path,
            blobs_dir,
            ledger_index_path,
            Default::default(),
            arrivals,
            CancellationToken::new(),
        );
        let reader = LedgerTailReader::open_named_with_limits(&ctx, reader, host_io_limits)
            .map_err(TerminalOutboundError::Open)?;
        Ok(Self { reader, _ctx: ctx })
    }

    pub fn read_new(&mut self) -> Result<Vec<OwnOutboundMovement>, TerminalOutboundError> {
        self.reader.read_new().map_err(TerminalOutboundError::Read)
    }

    pub async fn wait_for_change(&mut self) -> Result<(), TerminalOutboundError> {
        self.reader
            .wait_for_change(&self._ctx.cancel)
            .await
            .map_err(TerminalOutboundError::Read)
    }

    pub fn commit(&mut self) -> Result<(), TerminalOutboundError> {
        self.reader.commit().map_err(TerminalOutboundError::Commit)
    }

    pub fn envelope(
        &self,
        own: &OwnOutboundMovement,
    ) -> Result<InterfaceInvocationEnvelope, TerminalOutboundError> {
        self.reader
            .envelope(own)
            .map_err(TerminalOutboundError::Envelope)
    }

    pub fn next_seq(&self) -> u64 {
        self.reader.next_seq()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, RunId};
    use octopus_interfaces::{InvocationMetadata, RuntimeInvocationMetadata};
    use octopus_ledger::{
        ArtifactRef, ContentHash, Endpoint, Lamport, Movement, MovementKind, Seq, TraceId,
    };

    fn test_limits() -> InterfaceHostIoLimits {
        InterfaceHostIoLimits {
            max_control_frame_bytes: 4096,
            max_stderr_bytes: 4096,
            max_manifest_bytes: 4096,
            max_payload_bytes: 4096,
        }
    }

    fn seed_outbound(root: &std::path::Path, interface: &str, payload: &[u8]) {
        let blobs = root.join("blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        let payload_path = root.join("out.payload");
        std::fs::write(&payload_path, payload).unwrap();
        let manifest = serde_json::json!({
            "label": "interface.artifact.output",
            "payload_path": payload_path,
        });
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let hash = ContentHash::of_bytes(&manifest_bytes);
        std::fs::write(blobs.join(hash.to_hex()), &manifest_bytes).unwrap();

        let movement = Movement {
            seq: Seq(0),
            lamport: Lamport(1),
            recorded_at_unix_ms: 1,
            trace: TraceId("t".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
            to: Endpoint::Interface(InterfaceId::new(interface)),
            artifact: Some(ArtifactRef {
                id: ArtifactId::new(format!("interface.artifact.output:{}", hash.to_hex())),
                content: hash,
            }),
            invocation_metadata: Some(
                InvocationMetadata::new(octopus_interfaces::GrantProjection::default())
                    .with_runtime(RuntimeInvocationMetadata {
                        run: RunId::new("run_r"),
                        ee: EeId::new("ee_root"),
                        worker: octopus_descriptors::InterfaceId::new("system_worker_flash"),
                        checkpoint: CheckpointId::new("c1"),
                        interface: InterfaceId::new(interface),
                        epoch: 0,
                        run_record_hash: None,
                    })
                    .into(),
            ),
        };
        let line = serde_json::to_string(&movement).unwrap();
        std::fs::write(root.join("movements.jsonl"), format!("{line}\n")).unwrap();
    }

    #[test]
    fn reads_own_outbound_and_resolves_payload_then_commits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let durable = root.join("durable");
        std::fs::create_dir_all(&durable).unwrap();
        seed_outbound(
            &durable,
            "world_terminal",
            b"{\"text\":\"hi from the run\"}",
        );

        let mut reader = TerminalOutboundReader::open(
            InterfaceId::new("world_terminal"),
            root.join("home"),
            durable.clone(),
            test_limits(),
        )
        .unwrap();
        let own = reader.read_new().unwrap();
        assert_eq!(own.len(), 1, "exactly the terminal's one outbound row");
        let envelope = reader.envelope(&own[0]).unwrap();
        assert_eq!(
            envelope.read_artifact_payload().unwrap(),
            b"{\"text\":\"hi from the run\"}"
        );
        reader.commit().unwrap();

        let mut resumed = TerminalOutboundReader::open(
            InterfaceId::new("world_terminal"),
            root.join("home"),
            durable,
            test_limits(),
        )
        .unwrap();
        assert_eq!(resumed.next_seq(), 1, "cursor survived");
        assert!(
            resumed.read_new().unwrap().is_empty(),
            "nothing new to render"
        );
    }

    #[test]
    fn does_not_read_another_interfaces_outbound() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let durable = root.join("durable");
        std::fs::create_dir_all(&durable).unwrap();
        seed_outbound(
            &durable,
            "world_provider_codex",
            b"{\"text\":\"model side\"}",
        );

        let mut reader = TerminalOutboundReader::open(
            InterfaceId::new("world_terminal"),
            root.join("home"),
            durable,
            test_limits(),
        )
        .unwrap();
        assert!(
            reader.read_new().unwrap().is_empty(),
            "the terminal does not read another interface's outbound rows"
        );
    }

    #[test]
    fn rejects_legacy_run_to_interface_outbound() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let durable = root.join("durable");
        std::fs::create_dir_all(&durable).unwrap();
        seed_outbound(&durable, "world_terminal", b"{\"text\":\"legacy\"}");
        let path = durable.join("movements.jsonl");
        let mut movement: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&path).unwrap().trim()).unwrap();
        movement["from"] = serde_json::json!({"Run":"run_r"});
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string(&movement).unwrap()),
        )
        .unwrap();

        let mut reader = TerminalOutboundReader::open(
            InterfaceId::new("world_terminal"),
            root.join("home"),
            durable,
            test_limits(),
        )
        .unwrap();
        assert!(reader.read_new().is_err());
    }
}
