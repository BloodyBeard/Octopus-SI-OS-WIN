use super::*;
use octopus_tool_handlers::ToolResultArtifact;
use sha2::{Digest, Sha256};
use crate::private_fs;
use std::path::{Path, PathBuf};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadImport {
    source_artifact: octopus_interfaces::ArtifactRef,
    source_movement_seq: u64,
    worker_environment_id: String,
    receipt: Value,
    manifest_path: PathBuf,
    manifest_content_hash: String,
}

impl DownloadImport {
    fn read(path: &Path, maximum: u64) -> Result<Option<Self>, String> {
        let file = match private_fs::open_read_no_follow(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("download import record cannot be opened: {error}")),
        };
        if !file.metadata().map_err(|error| error.to_string())?.is_file() {
            return Err("download import record is not a regular file".into());
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::Read::take(file, maximum.saturating_add(1)), &mut bytes)
            .map_err(|error| format!("download import record cannot be read: {error}"))?;
        if bytes.len() as u64 > maximum { return Err("download import record exceeds the declared manifest bound".into()); }
        serde_json::from_slice(&bytes).map(Some).map_err(|error| format!("download import record is malformed: {error}"))
    }

    fn persist(&self, path: &Path, maximum: u64) -> Result<(), String> {
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > maximum { return Err("download import record exceeds the declared manifest bound".into()); }
        let mut file = private_fs::create_private_file(path).map_err(|error| format!("download import record cannot be created: {error}"))?;
        file.write_all(&bytes).and_then(|()| file.sync_all()).map_err(|error| format!("download import record cannot be synced: {error}"))?;
        std::fs::File::open(path.parent().ok_or("download import record has no owner directory")?)
            .and_then(|file| file.sync_all()).map_err(|error| format!("download import directory cannot be synced: {error}"))
    }

    fn restore(&self, root: &Path, decoded: &DecodedRoomInvocation, current: &Value, maximum: u64) -> Result<Value, String> {
        if self.source_movement_seq == 0 || self.source_movement_seq > decoded.source_movement_seq
            || self.worker_environment_id != decoded.formation.worker_environment_id.as_str()
            || self.source_artifact.id.as_str().is_empty()
            || self.receipt["status"] != "completed"
            || !matches!(current["status"].as_str(), Some("completed" | "imported")) {
            return Err("download import does not retain an exact completed source".into());
        }
        for key in ["download_id", "target_id", "browser_context_id", "host_generation", "arm", "bytes", "max_bytes", "sha256"] {
            if current.get(key).is_none() || current.get(key) != self.receipt.get(key) {
                return Err(format!("download import changed its retained native {key}"));
            }
        }
        let id = current["download_id"].as_str().ok_or("download import has no native identity")?;
        let expected_path = root.join(format!("{id}-{}", self.source_movement_seq));
        if self.manifest_path != root.join("artifacts").join(format!("{}.artifact.json", self.manifest_content_hash)) {
            return Err("download import manifest left its original private location".into());
        }
        let manifest = InterfaceArtifactFile::new(self.manifest_path.clone()).map_err(|error| error.to_string())?
            .read_manifest_bounded(maximum).map_err(|error| error.to_string())?;
        if blake3::hash(&manifest.manifest_bytes).to_hex().as_str() != self.manifest_content_hash
            || manifest.manifest.label != "browser.download" || manifest.manifest.payload_path != expected_path {
            return Err("download import changed its original manifest".into());
        }
        let mut file = private_fs::open_read_no_follow(&expected_path)
            .map_err(|error| format!("download imported payload is unavailable: {error}"))?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        let total = current["bytes"].as_u64().ok_or("download import has no byte count")?;
        if !metadata.is_file() || metadata.len() != total || total > current["max_bytes"].as_u64().ok_or("download import has no byte bound")? {
            return Err("download imported payload changed its regular-file byte boundary".into());
        }
        let mut sha = Sha256::new(); let mut content = blake3::Hasher::new(); let mut offset = 0;
        let mut buffer = [0u8; 64 * 1024];
        while offset < total {
            let limit = (total - offset).min(buffer.len() as u64) as usize;
            let count = std::io::Read::read(&mut file, &mut buffer[..limit]).map_err(|error| error.to_string())?;
            if count == 0 { return Err("download imported payload ended before its pinned length".into()); }
            sha.update(&buffer[..count]); content.update(&buffer[..count]); offset += count as u64;
        }
        if std::io::Read::read(&mut file, &mut buffer[..1]).map_err(|error| error.to_string())? != 0
            || format!("{:x}", sha.finalize()) != current["sha256"].as_str().ok_or("download import has no SHA256")?
            || manifest.manifest.payload_content_hash.as_deref() != Some(blake3::Hasher::finalize(&content).to_hex().as_str()) {
            return Err("download imported payload differs from its immutable native and artifact hashes".into());
        }
        let mut result = self.receipt.clone();
        result["artifact"] = serde_json::to_value(manifest.manifest).map_err(|error| error.to_string())?;
        result["path"] = json!(expected_path);
        result["content_hash"] = json!(blake3::Hasher::finalize(&content).to_hex().as_str());
        result["import_source"] = json!({"artifact":self.source_artifact,"source_movement_seq":self.source_movement_seq});
        result["native_copy_released"] = json!(current["status"] == "imported");
        Ok(result)
    }
}

impl BrowserRoom {
    pub(crate) fn prepare_download_output(
        &self,
        response: &InterfaceArtifactFile,
        source: &octopus_interfaces::ArtifactRef,
        source_movement_seq: u64,
    ) -> Result<Option<(InterfaceArtifactFile, ToolResultArtifact, Value)>, String> {
        let bytes = response.read_bounded(self.environment.host_io_limits.max_manifest_bytes, self.environment.host_io_limits.max_payload_bytes).map_err(|error| error.to_string())?.payload_bytes;
        let mut result: ToolResultArtifact = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        result.validate()?;
        if !result.ok { return Ok(None); }
        let body = result.body.as_mut().ok_or("download output has no result body")?;
        if body["status"] != "completed" { return Ok(None); }
        let correlation = result.correlation_id.as_deref().filter(|value| !value.is_empty() && value.trim() == *value).ok_or("download publication requires its original tool correlation")?;
        let manifest: octopus_interfaces::InterfaceArtifactManifest = serde_json::from_value(body["artifact"].clone()).map_err(|error| format!("download output has no retained manifest: {error}"))?;
        let total = body["bytes"].as_u64().ok_or("download output has no byte count")?;
        if total > self.environment.host_io_limits.max_payload_bytes || body["source_movement_seq"].as_u64() != Some(source_movement_seq) {
            return Err("download output exceeds its admitted carrier bound or changed its invocation".into());
        }
        let label = format!("tool.output.{}", blake3::hash(correlation.as_bytes()).to_hex());
        let root = self.environment.var_dir.join("download-outputs");
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let payload = octopus_interfaces::read_artifact_carrier_file_bounded(&manifest.payload_path, total).map_err(|error| error.to_string())?;
        let hash = blake3::hash(&payload).to_hex().to_string();
        if payload.len() as u64 != total || manifest.payload_content_hash.as_deref() != Some(&hash)
            || body["sha256"].as_str() != Some(format!("{:x}", Sha256::digest(&payload)).as_str()) {
            return Err("download publication differs from the retained completed bytes".into());
        }
        let renamed = octopus_interfaces::InterfaceArtifactManifest {
            label: label.clone(), payload_path: manifest.payload_path,
            payload_content_hash: Some(hash.clone()),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&renamed).map_err(|error| error.to_string())?;
        let artifact = InterfaceArtifactFile::store_manifest_bounded(
            &root, &manifest_bytes, self.environment.host_io_limits.max_manifest_bytes,
            self.environment.host_io_limits.max_payload_bytes,
        ).map_err(|error| error.to_string())?;
        if artifact.read_payload_bytes().map_err(|error| error.to_string())? != payload {
            return Err("download payload changed while its output manifest was published".into());
        }
        std::fs::File::open(&root).and_then(|directory| directory.sync_all())
            .map_err(|error| format!("download output directory cannot be synced: {error}"))?;
        let pinned = artifact.read_manifest_bounded(self.environment.host_io_limits.max_manifest_bytes).map_err(|error| error.to_string())?;
        let content = octopus_descriptors::DescriptorHash::of_bytes(&pinned.manifest_bytes);
        let reference = octopus_interfaces::ArtifactRef {
            id: octopus_descriptors::ArtifactId::new(format!("{label}:{}",content.to_hex())),
            content,
        };
        body.as_object_mut().ok_or("download body is not an object")?.remove("path");
        body["artifact"] = serde_json::to_value(&reference).map_err(|error|error.to_string())?;
        let receipt = json!({"artifact":reference,"source_artifact":source,"source_movement_seq":source_movement_seq,"payload_content_hash":hash,"byte_count":total});
        Ok(Some((artifact,result,receipt)))
    }

    pub(super) async fn publish_download_output(
        &self,
        response: InterfaceArtifactFile,
        source: &octopus_interfaces::ArtifactRef,
        source_movement_seq: u64,
        io: &ToolProcessIo,
    ) -> Result<InterfaceArtifactFile, ToolProcessError> {
        let Some((artifact, mut result, mut receipt)) = self.prepare_download_output(&response,source,source_movement_seq).map_err(crate::capability)? else { return Ok(response); };
        match io.hand_output_artifact(&artifact).await.map_err(crate::capability)? {
            octopus_tool_door_runtime::DeliveryVerdict::Admitted { seq } => receipt["admission_seq"] = json!(seq),
            verdict => return Err(crate::capability(format!("download artifact admission refused: {verdict:?}"))),
        }
        result.body.as_mut().ok_or_else(|| crate::capability("download output lost its body"))?["generated_artifacts"] = json!([receipt]);
        let bytes = serde_json::to_vec(&result).map_err(|error| crate::capability(error.to_string()))?;
        InterfaceArtifactFile::write(self.environment.var_dir.join("emitted"), &result.label, &bytes).map_err(|error| crate::capability(error.to_string()))
    }

    pub(super) async fn download(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        if !matches!(self.transport, BrowserTransport::Native(_)) {
            return Err("browser downloads require the owned native browser transport".into());
        }
        let tool = decoded.call.tool_id.as_str();
        let arm = decoded.operation == BrowserOperation::DownloadArm;
        let cancel = decoded.operation == BrowserOperation::DownloadCancel;
        let object = if arm { page_object(tool, &decoded.call.arguments, &["max_bytes", "timeout_ms"])? }
            else { strict_object(tool, &decoded.call.arguments, if cancel { &["download_id"] } else { &["download_id", "timeout_ms"] })? };
        let params = if arm {
            let bytes = bounded_optional_u64(object, "max_bytes", 1, 9_007_199_254_740_991)?.ok_or("download_arm requires max_bytes")?;
            let timeout = bounded_optional_u64(object, "timeout_ms", 1, 300_000)?.ok_or("download_arm requires timeout_ms")?;
            json!({"max_bytes":bytes,"timeout_ms":timeout})
        } else {
            let id = required_string(object, "download_id")?;
            if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) { return Err("download_id must be the exact native download identity".into()); }
            if cancel { json!({"download_id":id}) }
            else { json!({"download_id":id,"timeout_ms":bounded_optional_u64(object,"timeout_ms",1,30_000)?.unwrap_or(1000)}) }
        };
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let state = &mut *room;
        let expected_target = if arm { Some(self.ensure_session(state, false, decoded, recorder).await?.1.target_id.clone()) }
            else { self.ensure_browser_context(state, false).await?; None };
        let expected_context = state.browser_context_id.clone().ok_or("browser download has no owned context")?;
        let expected_generation = state.native_identity.as_ref().ok_or("browser download has no native identity")?.host_generation.clone();
        let session = if let Some(id) = &expected_target { state.pages.get_mut(id).and_then(|page| page.session.as_mut()) } else { state.browser.as_mut() }
            .ok_or("browser download has no owned control connection")?;
        let mut params = params;
        if !arm && !cancel {
            let timeout = session.request_timeout.as_millis().saturating_sub(1).min(params["timeout_ms"].as_u64().unwrap_or(1000) as u128);
            if timeout == 0 { return Err("browser transport timeout cannot contain a download observation".into()); }
            params["timeout_ms"] = json!(timeout as u64);
        }
        let method = if arm { "OctopusBrowser.armDownload" } else if cancel { "OctopusBrowser.cancelDownload" } else { "OctopusBrowser.waitDownload" };
        let mut result = session.command(tool, method, params).await?;
        let target_id = result["target_id"].as_str().ok_or("download receipt has no original target")?.to_owned();
        validate_target_id(&target_id)?;
        if expected_target.as_ref().is_some_and(|target| target != &target_id)
            || result["browser_context_id"].as_str() != Some(expected_context.as_str())
            || result["host_generation"].as_str() != Some(expected_generation.as_str())
            || result["arm"]["formation_pin"] != json!(decoded.formation.formation_pin)
            || result["arm"]["source_movement_seq"].as_u64().is_none_or(|seq| seq == 0 || seq > decoded.source_movement_seq)
            || (!arm && result["download_id"] != decoded.call.arguments["download_id"]) {
            return Err("download response differs from its original target, room, or admitted invocation".into());
        }
        let import_root = self.state_dir.join("worker-environments").join(decoded.formation.worker_environment_id.as_str()).join("downloads");
        let import_record = if !arm && !cancel {
            let id = required_string(result.as_object().ok_or("download receipt is not an object")?, "download_id")?;
            Some(import_root.join(format!("{id}.import.json")))
        } else { None };
        let retained = if let Some(path) = &import_record {
            DownloadImport::read(path, self.environment.host_io_limits.max_manifest_bytes)?
        } else { None };
        if let Some(retained) = retained {
            result = retained.restore(&import_root.canonicalize().map_err(|error| error.to_string())?, decoded, &result, self.environment.host_io_limits.max_manifest_bytes)?;
        } else if result["status"] == "imported" && !arm && !cancel {
            return Err("native download was imported but its exact durable import record is unavailable; refusing another import or a fabricated artifact".into());
        } else if result["status"] == "completed" && !arm && !cancel {
            if let Some(policy) = self.upload_approval.as_ref().and_then(|config| config.permission_policy.as_ref()) {
                policy.require_download(&result)?;
            }
            let download_id = required_string(result.as_object().ok_or("download receipt is not an object")?, "download_id")?.to_owned();
            let total = result["bytes"].as_u64().ok_or("download receipt has no native byte count")?;
            let max_bytes = result["max_bytes"].as_u64().ok_or("download receipt has no admitted byte bound")?;
            let expected = result["sha256"].as_str().filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())).ok_or("download receipt has no terminal SHA256")?.to_owned();
            if total > max_bytes || result["native_event"]["state"] != "completed" { return Err("download receipt does not prove native completion within its bound".into()); }
            let root = import_root;
            std::fs::create_dir_all(&root).map_err(|error| format!("download import directory: {error}"))?;
            let root = root.canonicalize().map_err(|error| format!("download import path: {error}"))?;
            let filename = root.join(format!("{}-{}", download_id, decoded.source_movement_seq));
            let mut output = private_fs::create_private_file(&filename).map_err(|error| format!("download import cannot create its file: {error}"))?;
            let mut sha256 = Sha256::new(); let mut content = blake3::Hasher::new(); let mut offset = 0u64;
            while offset < total {
                let length = (total - offset).min(8 * 1024 * 1024);
                let chunk = session.command(tool, "OctopusBrowser.readDownload", json!({"download_id":download_id,"offset":offset,"length":length})).await?;
                if chunk["download_id"] != download_id || chunk["target_id"] != target_id || chunk["browser_context_id"] != expected_context || chunk["offset"].as_u64() != Some(offset) || chunk["bytes"].as_u64() != Some(length) || chunk["sha256"] != expected { return Err("download chunk changed its terminal identity or byte range".into()); }
                let payload = chunk["payload_base64"].as_str().ok_or("download chunk has no bytes")?;
                if payload.len() as u64 > length.div_ceil(3) * 4 { return Err("download chunk exceeds its requested bound".into()); }
                let bytes = base64::engine::general_purpose::STANDARD.decode(payload).map_err(|error| format!("download chunk is not base64: {error}"))?;
                if bytes.len() as u64 != length || base64::engine::general_purpose::STANDARD.encode(&bytes) != payload { return Err("download chunk byte count is inconsistent".into()); }
                output.write_all(&bytes).map_err(|error| format!("download import write failed: {error}"))?;
                sha256.update(&bytes); content.update(&bytes); offset += length;
            }
            output.sync_all().map_err(|error| format!("download import cannot sync its file: {error}"))?; drop(output);
            let actual = format!("{:x}", sha256.finalize());
            if actual != expected { return Err("download imported bytes differ from native terminal SHA256".into()); }
            let artifact = octopus_interfaces::InterfaceArtifactManifest {
                label: "browser.download".to_string(), payload_path: filename.clone(),
                payload_content_hash: Some(blake3::Hasher::finalize(&content).to_hex().to_string()),
            };
            let manifest = serde_json::to_vec(&artifact).map_err(|error| error.to_string())?;
            let artifact_root = root.join("artifacts");
            std::fs::create_dir_all(&artifact_root).map_err(|error| format!("download artifact directory: {error}"))?;
            let manifest_path = artifact_root.join(format!("{}.artifact.json", blake3::hash(&manifest).to_hex()));
            let mut manifest_file = private_fs::create_private_file(&manifest_path).map_err(|error| format!("download artifact cannot create its manifest: {error}"))?;
            manifest_file.write_all(&manifest).and_then(|()| manifest_file.sync_all())
                .map_err(|error| format!("download artifact manifest cannot be persisted: {error}"))?;
            drop(manifest_file);
            let imported = DownloadImport {
                source_artifact: decoded.envelope.artifact.clone(),
                source_movement_seq: decoded.source_movement_seq,
                worker_environment_id: decoded.formation.worker_environment_id.to_string(),
                receipt: result.clone(),
                manifest_path: manifest_path.clone(),
                manifest_content_hash: blake3::hash(&manifest).to_hex().to_string(),
            };
            let artifact = InterfaceArtifactFile::new(manifest_path).map_err(|error| error.to_string())?
                .read_manifest_bounded(16_384).map_err(|error| format!("download artifact import failed: {error}"))?.manifest;
            for directory in [artifact_root.as_path(), root.as_path(), root.parent().ok_or("download import directory has no owner")?] {
                std::fs::File::open(directory).and_then(|file| file.sync_all())
                    .map_err(|error| format!("download artifact directory cannot be synced: {error}"))?;
            }
            imported.persist(import_record.as_deref().ok_or("completed download lost its import record location")?, self.environment.host_io_limits.max_manifest_bytes)?;
            result["artifact"] = serde_json::to_value(artifact).map_err(|error| error.to_string())?;
            result["path"] = json!(filename);
            result["content_hash"] = json!(blake3::Hasher::finalize(&content).to_hex().as_str());
            result["import_source"] = json!({"artifact":imported.source_artifact,"source_movement_seq":imported.source_movement_seq});
            let finished = session.command(tool, "OctopusBrowser.finishDownload", json!({"download_id":download_id,"sha256":expected})).await;
            result["native_copy_released"] = json!(finished.as_ref().is_ok_and(|value| value["status"] == "imported" && value["download_id"] == download_id && value["target_id"] == target_id && value["browser_context_id"] == expected_context));
            if let Err(error) = finished { result["native_release_error"] = json!(error); }
        }
        result["result"] = json!(tool);
        result["worker_environment_id"] = json!(decoded.formation.worker_environment_id);
        result["source_movement_seq"] = json!(decoded.source_movement_seq);
        Ok(result)
    }
}
