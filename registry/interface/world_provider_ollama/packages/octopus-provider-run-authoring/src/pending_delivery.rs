use super::*;
use octopus_ledger::ArtifactDelivery;
use octopus_ledger_index::{ArrivalDedupIndex, RetainedArrivalState};

pub(super) struct Ledger {
    interface: InterfaceId,
    path: PathBuf,
    decoder: DurableMovementDecoder,
    max_row_bytes: u64,
    offset: u64,
    observed_length: u64,
    index: ArrivalDedupIndex,
    submitted: HashSet<(String, RuntimeCoordinateKey)>,
    scoped: HashSet<(String, RuntimeCoordinateKey)>,
    unattributed: BTreeSet<String>,
}

impl Ledger {
    pub(super) fn new(interface: InterfaceId, path: PathBuf, max_row_bytes: u64) -> Self {
        Self {
            interface,
            decoder: DurableMovementDecoder::for_log(&path),
            path,
            max_row_bytes,
            offset: 0,
            observed_length: 0,
            index: ArrivalDedupIndex::new(),
            submitted: HashSet::new(),
            scoped: HashSet::new(),
            unattributed: BTreeSet::new(),
        }
    }

    fn refresh(&mut self) -> Result<(), String> {
        let mut file = std::fs::File::open(&self.path).map_err(|error| error.to_string())?;
        let length = file.metadata().map_err(|error| error.to_string())?.len();
        if length < self.observed_length.max(self.offset) {
            return Err("retained provider output ledger shrank behind its observed prefix".into());
        }
        self.observed_length = length;
        file.seek(std::io::SeekFrom::Start(self.offset))
            .map_err(|error| error.to_string())?;
        let mut reader = std::io::BufReader::new(file);
        loop {
            let row = match read_bounded_ledger_row(&mut reader, self.max_row_bytes)? {
                BoundedLedgerRow::Complete(row) => row,
                BoundedLedgerRow::Partial => {
                    return Err("retained provider output requires a complete ledger prefix".into());
                }
                BoundedLedgerRow::Eof => return Ok(()),
            };
            let movement = self
                .decoder
                .decode(&row, self.index.watermark() as usize)
                .map_err(|error| error.to_string())?;
            if movement.seq.0 != self.index.watermark() {
                return Err("retained output ledger is not contiguous".into());
            }
            if let Some(artifact) = movement.artifact.as_ref().filter(|_| {
                movement.from == Endpoint::Interface(self.interface.clone())
                    && matches!(
                        movement.kind,
                        MovementKind::ArtifactMoved
                            | MovementKind::InterfaceArrival
                            | MovementKind::DeliveryRefused
                    )
            }) {
                let artifact = artifact_ref_key(artifact);
                match movement.invocation_metadata.as_ref() {
                    Some(octopus_ledger::MovementMetadata::ArrivalFanout(metadata)) => {
                        for target in &metadata.arrival_fanout.targets {
                            let key =
                                (artifact.clone(), RuntimeCoordinateKey::from_runtime(target));
                            self.submitted.insert(key.clone());
                            self.scoped.insert(key);
                        }
                    }
                    Some(metadata) => {
                        let runtime = match metadata {
                            octopus_ledger::MovementMetadata::Invocation(metadata) => {
                                metadata.runtime.as_ref()
                            }
                            octopus_ledger::MovementMetadata::Delivery(metadata) => {
                                metadata.runtime.as_ref()
                            }
                            _ => None,
                        };
                        if let Some(runtime) = runtime {
                            self.submitted
                                .insert((artifact, RuntimeCoordinateKey::from_runtime(runtime)));
                        } else {
                            self.unattributed.insert(artifact);
                        }
                    }
                    None => {
                        self.unattributed.insert(artifact);
                    }
                }
            }
            self.index
                .catch_up_movements(std::iter::once(movement))
                .map_err(|error| error.to_string())?;
            self.offset = self
                .offset
                .checked_add(row.len() as u64)
                .ok_or("retained output ledger offset overflow")?;
        }
    }

    fn scope(&mut self, identity: &Identity) -> Result<Option<Scope>, String> {
        self.refresh()?;
        let Some(scope) = self
            .index
            .retained_arrival_scope(&identity.provider, &identity.id()?)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        if scope.artifact != Some(identity.artifact()?)
            || scope.seq.0 <= identity.source_movement_seq
        {
            return Err(
                "retained provider output scope changed its original source or artifact".into(),
            );
        }
        let edges = self
            .index
            .retained_arrival_edges(scope.seq.0)
            .map_err(|error| error.to_string())?;
        let has_output = edges.iter().any(|(coordinate, _)| {
            coordinate.run == identity.run
                && coordinate.worker == identity.worker
                && coordinate.interface == identity.provider
                && coordinate.checkpoint == identity.item.checkpoint.id
                && coordinate.epoch == identity.item.checkpoint.epoch
        });
        Ok(Some(Scope {
            seq: scope.seq.0,
            complete: edges
                .iter()
                .all(|(_, state)| matches!(state, RetainedArrivalState::Finished(_))),
            has_output,
        }))
    }
}

struct Scope {
    seq: u64,
    complete: bool,
    has_output: bool,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    schema_version: u32,
    provider: InterfaceId,
    run: RunId,
    worker: InterfaceId,
    source_movement_seq: u64,
    item: PendingHandoffItem,
}

impl Identity {
    fn key(&self) -> Result<(String, RuntimeCoordinateKey), String> {
        Ok((
            artifact_ref_key(&self.artifact()?),
            RuntimeCoordinateKey::new(
                &self.run,
                &self.worker,
                &self.provider,
                &self.item.checkpoint.id,
                self.item.checkpoint.epoch,
            ),
        ))
    }

    fn id(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        Ok(format!(
            "provider-output:{}",
            ContentHash::of_bytes(&bytes).to_hex()
        ))
    }

    fn artifact(&self) -> Result<ArtifactRef, String> {
        Ok(ArtifactRef {
            id: self.item.artifact_id.clone(),
            content: ContentHash::from_hex(&self.item.artifact_content_hash)
                .ok_or("retained output has an invalid manifest hash")?,
        })
    }

    fn path(&self, ctx: &InterfaceLiftContext) -> Result<PathBuf, String> {
        Ok(ctx
            .interface_var_dir()
            .join("handoff-delivery")
            .join(format!(
                "{}.json",
                self.id()?.strip_prefix("provider-output:").unwrap()
            )))
    }

    fn exists(&self, ctx: &InterfaceLiftContext) -> Result<bool, String> {
        let path = self.path(ctx)?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        };
        if bytes.len() as u64 > ctx.required_host_io_limits()?.max_control_frame_bytes {
            return Err("retained provider output identity exceeds the control-frame limit".into());
        }
        let saved: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("retained provider output identity is corrupt: {error}"))?;
        if saved != *self {
            return Err(
                "retained provider output identity changed its exact pending contract".into(),
            );
        }
        Ok(true)
    }

    fn save(&self, ctx: &InterfaceLiftContext) -> Result<(), String> {
        use std::io::Write;
        let path = self.path(ctx)?;
        let parent = path
            .parent()
            .ok_or("retained output identity has no parent")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| error.to_string())?;
        std::fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())
    }
}

impl ProviderOutputHandoff {
    fn pending_delivery_identity(
        &self,
        pending: &PendingHandoff,
        item: &PendingHandoffItem,
    ) -> Identity {
        Identity {
            schema_version: 1,
            provider: self.config.provider_interface.clone(),
            run: self.config.run.clone(),
            worker: self.config.worker.clone(),
            source_movement_seq: pending.source_movement_seq,
            item: item.clone(),
        }
    }

    pub(super) fn pending_delivery_complete(
        &self,
        pending: &PendingHandoff,
        item: &PendingHandoffItem,
    ) -> Result<bool, String> {
        let identity = self.pending_delivery_identity(pending, item);
        if !identity.exists(&self.ctx)? {
            let mut index = self.session.index()?;
            index.retained_delivery.refresh()?;
            if index.retained_delivery.scoped.contains(&identity.key()?) {
                return Err(
                    "retained provider output lost its exact saved dispatch identity".into(),
                );
            }
            return Ok(true);
        }
        let scope = self
            .session
            .index()?
            .retained_delivery
            .scope(&identity)?
            .ok_or("provider output was admitted without its saved retained scope")?;
        if !scope.has_output {
            return Err("retained provider output scope omits its original checkpoint".into());
        }
        Ok(scope.complete)
    }

    fn prepare_pending_delivery(
        &self,
        pending: &PendingHandoff,
        item: &PendingHandoffItem,
    ) -> Result<Identity, String> {
        let identity = self.pending_delivery_identity(pending, item);
        if identity.exists(&self.ctx)? {
            return Ok(identity);
        }
        if !pending.dispatch_fenced {
            return Err("legacy provider pending output has no durable unsubmitted proof; an explicit checked transition is required".into());
        }
        let record = RunRecord::from_bytes(
            &std::fs::read(&self.run_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if source_withdrawal::dispatch_path(&self.ctx, &record, item)
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            return Err("legacy provider pending output was dispatched without a retained identity; its outcome remains unresolved".into());
        }
        {
            let mut index = self.session.index()?;
            index.retained_delivery.refresh()?;
            if index.retained_delivery.submitted.contains(&identity.key()?)
                || index
                    .retained_delivery
                    .unattributed
                    .contains(&artifact_ref_key(&identity.artifact()?))
            {
                return Err("provider pending output already has delivery evidence without its retained identity".into());
            }
        }
        identity.save(&self.ctx)?;
        Ok(identity)
    }

    pub(super) async fn hand_retained_pending(
        &self,
        pending: &PendingHandoff,
        item: &PendingHandoffItem,
        artifact: InterfaceArtifactFile,
    ) -> Result<DeliveryVerdict, String> {
        let identity = self.prepare_pending_delivery(pending, item)?;
        self.record_pending_dispatch(pending, item)?;
        let delivery = self
            .ctx
            .hand_arrival_retained(artifact, Vec::new(), &identity.id()?)
            .await?;
        let scope = self
            .session
            .index()?
            .retained_delivery
            .scope(&identity)?
            .ok_or("retained provider output reply has no durable scope")?;
        match delivery {
            ArtifactDelivery::Pending { scope_seq } if scope.seq == scope_seq => Err(format!(
                "provider pending output '{}' retains unfinished original edges in scope {scope_seq}",
                item.artifact_id.as_str(),
            )),
            ArtifactDelivery::Pending { .. } => {
                Err("retained provider output changed its original scope".into())
            }
            ArtifactDelivery::Complete { verdict }
                if scope.complete && !verdict.awaits_current_source() =>
            {
                Ok(verdict)
            }
            ArtifactDelivery::Complete { .. } => {
                Err("retained provider output completed without all original edge decisions".into())
            }
        }
    }
}
