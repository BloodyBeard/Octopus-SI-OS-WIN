use std::path::PathBuf;

use octopus_descriptors::InterfaceId;

use octopus_interface_transport::{
    ActivationStart, ActivationStartFuture, InterfaceActivationArrival, InterfaceHostIoLimits,
    send_arrival,
};

pub const TRANSPORT_REF: &str = "stdin_json_lines";

pub fn activation_start(start: ActivationStart) -> ActivationStartFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Config {
            #[serde(default)]
            enabled_when_env_absent: Option<String>,
        }

        let config: Config = serde_json::from_value(start.config).map_err(|e| {
            format!(
                "interface {} stdin_json_lines activation config is malformed: {e}",
                start.interface_id
            )
        })?;
        if let Some(env_name) = config.enabled_when_env_absent {
            if std::env::var_os(&env_name).is_some() {
                return Ok(Vec::new());
            }
        }
        tracing::info!(
            interface = %start.interface_id,
            "activated interface via stdin_json_lines"
        );
        Ok(vec![spawn_stdin_arrivals(
            start.tx,
            start.interface_id,
            start.artifact_root,
            start.host_io_limits,
        )])
    })
}

fn spawn_stdin_arrivals(
    tx: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    interface_id: InterfaceId,
    artifact_root: PathBuf,
    host_io_limits: InterfaceHostIoLimits,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let mut input = tokio::io::BufReader::new(tokio::io::stdin());
        let maximum_frame_bytes = host_io_limits
            .max_control_frame_bytes
            .min(host_io_limits.max_manifest_bytes);
        loop {
            match read_bounded_line(&mut input, maximum_frame_bytes).await {
                Ok(Some(line)) => {
                    let Ok(line) = std::str::from_utf8(&line) else {
                        tracing::warn!("stdin interface manifest frame is not UTF-8");
                        continue;
                    };
                    let trimmed = line.trim().as_bytes();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let artifact =
                        match capture_stdin_artifact(&artifact_root, trimmed, host_io_limits) {
                            Ok(artifact) => artifact,
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "stdin interface artifact materialization failed closed"
                                );
                                continue;
                            }
                        };
                    if send_arrival(&tx, &interface_id, artifact, Vec::new(), None)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "stdin interface arrival intake read error; ending intake"
                    );
                    break;
                }
            }
        }
    })
}

fn capture_stdin_artifact(
    artifact_root: &std::path::Path,
    manifest_bytes: &[u8],
    host_io_limits: InterfaceHostIoLimits,
) -> Result<octopus_interfaces::InterfaceArtifactFile, String> {
    octopus_interfaces::InterfaceArtifactFile::store_manifest_bounded(
        artifact_root,
        manifest_bytes,
        host_io_limits.max_manifest_bytes,
        host_io_limits.max_payload_bytes,
    )
    .map_err(|error| error.to_string())
}

async fn read_bounded_line<R>(reader: &mut R, maximum_bytes: u64) -> Result<Option<Vec<u8>>, String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;

    let maximum_bytes = usize::try_from(maximum_bytes)
        .map_err(|_| "stdin manifest ceiling exceeds host address space".to_string())?;
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| format!("stdin interface arrival intake read error: {error}"))?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_bytes = newline.unwrap_or(available.len());
        if content_bytes > maximum_bytes.saturating_sub(line.len()) {
            return Err(format!(
                "stdin interface manifest frame exceeds the declared ceiling of {maximum_bytes} bytes"
            ));
        }
        line.extend_from_slice(&available[..content_bytes]);
        reader.consume(newline.map_or(content_bytes, |index| index + 1));
        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_limits() -> InterfaceHostIoLimits {
        InterfaceHostIoLimits {
            max_control_frame_bytes: 1024,
            max_stderr_bytes: 1024,
            max_manifest_bytes: 1024,
            max_payload_bytes: 6,
        }
    }

    #[tokio::test]
    async fn line_and_artifact_capture_stay_finite_and_stable() {
        let mut oversized = tokio::io::BufReader::new(&b"12345\n"[..]);
        assert!(read_bounded_line(&mut oversized, 4).await.is_err());

        let root = std::env::temp_dir().join(format!(
            "octopus-stdin-capture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("source.payload");
        std::fs::write(&payload, b"before").unwrap();
        let manifest = serde_json::to_vec(&serde_json::json!({
            "label": "stdin.input",
            "payload_path": payload,
        }))
        .unwrap();
        let captured = capture_stdin_artifact(&root, &manifest, test_limits()).unwrap();
        std::fs::write(&payload, b"after!").unwrap();
        assert_eq!(captured.read_payload_bytes().unwrap(), b"before");
        std::fs::remove_dir_all(root).unwrap();
    }
}
