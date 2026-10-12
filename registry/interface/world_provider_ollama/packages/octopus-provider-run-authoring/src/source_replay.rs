use super::*;

impl ProviderOutputHandoff {
    pub async fn completed_input_artifact_source(
        &self,
        artifact: &ArtifactRef,
    ) -> Result<Option<u64>, String> {
        let state = self.state.lock().await;
        let key = artifact_ref_key(artifact);
        let inputs = self.session.index()?.session_evidence(&self.config.run, &self.config.provider_interface,
            std::iter::once(key.clone()))?;
        let sequences = inputs.get(&key).into_iter().flat_map(|evidence|
            evidence.provider_inputs.range(..self.source_movement_seq).map(|(sequence, _)| *sequence)
        ).collect::<Vec<_>>();
        if sequences.is_empty() { return Ok(None); }
        if state.session_context.provider_interface != self.config.provider_interface {
            return Err("completed input context belongs to another provider".into());
        }
        let mut matching = state.session_context.artifacts.iter().enumerate()
            .filter(|(_, entry)| entry.source_artifact == *artifact);
        if let Some((position, entry)) = matching.next() {
            if matching.next().is_some() {
                return Err("completed input context repeats an exact artifact".into());
            }
            validate_session_source_payload(entry, &self.ctx.blobs_dir, self.ctx.required_host_io_limits()?,
                &self.session_snapshot_labels, position, &self.config.provider_interface)?;
        }
        let mut completed = None;
        for sequence in sequences {
            if let Some(source) = self.completed_input_source_evidence(&state.run_record_bytes, sequence, artifact)? {
                completed.get_or_insert(source);
            }
        }
        Ok(completed)
    }

    fn completed_input_source_evidence(
        &self, current_bytes: &[u8], sequence: u64, artifact: &ArtifactRef,
    ) -> Result<Option<u64>, String> {
        let limits = self.ctx.required_host_io_limits()?;
        let (before, original) = self.checked_input_artifact_record(sequence, artifact)?;
        let (_, invocation) = self.checked_input_artifact_record(self.source_movement_seq, artifact)?;
        let current = RunRecord::from_bytes(current_bytes).map_err(|error| error.to_string())?;
        if invocation.ee != original.ee || current.ee != original.ee {
            return Err("completed input evidence changed its EE".into());
        }
        let provider_writer = CheckedRunWriter {
            owner: self.config.provider_interface.clone(),
            source: self.config.authoring_source_interface.clone(),
            door: self.config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface().clone(),
            tool: self.config.file_write_tool.clone(),
            call_label: self.config.file_write_label.clone(),
            result_label: self.config.file_write_result_label.clone(),
        };
        let mut writers = self.config.checkpoints.iter()
            .filter_map(|checkpoint| checkpoint.owner_authoring.clone()).collect::<Vec<_>>();
        writers.push(provider_writer.clone());
        let writers = declared_writer_closure(&original, writers)?;
        let evidence = CheckedRunEvidence {
            movements_path: &self.ctx.movements_path,
            ledger_index_path: &self.ctx.ledger_index_path,
            blobs_dir: &self.ctx.blobs_dir,
            max_control_frame_bytes: limits.max_control_frame_bytes,
            max_manifest_bytes: limits.max_manifest_bytes,
            max_payload_bytes: limits.max_payload_bytes,
        };
        stable_run_observation(&self.run_path, current_bytes, limits.max_payload_bytes, || {
            let start = concurrent_writer_evidence_start(&evidence, &original, &before, sequence)?;
            let chain = checked_run_file_chain_state(&evidence, &self.run_path, &before,
                current_bytes, start, &writers)?;
            let mut previous = persisted_pending_handoff(&original, &self.config.provider_interface)?;
            let mut sources = BTreeMap::new();
            if let Some(prior) = &previous {
                if prior.source_movement_seq >= sequence {
                    return Err("completed input pinned record already claims this or a later source".into());
                }
                sources.insert(prior.source_movement_seq, prior.clone());
            }
            for transition in &chain {
                if transition.writer == provider_writer {
                    validate_checked_provider_transition(transition)?;
                } else {
                    validate_checked_owner_transition(transition)?;
                }
                let record = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
                let next = persisted_pending_handoff(&record, &self.config.provider_interface)?;
                if previous == next { continue; }
                let next = next.ok_or("completed input chain lost its pending handoff")?;
                validate_pending_items_are_active(&record, &self.config.provider_interface, &next)?;
                let publication = previous.as_ref().is_some_and(|prior|
                    context_publication::is_publication_completion(prior, &next)
                        && prior.source_without_context == next.source_without_context);
                if let Some(prior) = &previous {
                    if prior.context_publication.is_some() && !publication {
                        return Err("completed input handoff was superseded before context publication".into());
                    }
                    for (item, outcome) in prior.items.iter().zip(self.admitted_replay_handoff(prior)?) {
                        let ProviderHandoffOutcome::Admitted { artifact, .. } = outcome else {
                            return Err("completed input handoff has a refused output".into());
                        };
                        if self.session.index()?.indexed_handoff_seq(PendingHandoffLedgerCoordinate {
                            interface: &self.config.provider_interface, run: &self.config.run, worker: &self.config.worker,
                            artifact: &artifact, required_evidence: pending_required_evidence(item)?,
                            checkpoint: &item.checkpoint.id, epoch: item.checkpoint.epoch,
                        }, transition.call.seq.0)?.is_none() {
                            return Err("completed input handoff was superseded before output admission".into());
                        }
                    }
                }
                if let Some(prior) = sources.get(&next.source_movement_seq) {
                    if prior.terminal && !publication {
                        return Err("completed input chain repeats a terminal source".into());
                    }
                    if prior.items == next.items && !publication {
                        return Err("completed input chain conflicts with a prior pending batch".into());
                    }
                }
                sources.insert(next.source_movement_seq, next.clone());
                previous = Some(next);
            }
            if previous != persisted_pending_handoff(&current, &self.config.provider_interface)? {
                return Err("completed input chain differs from the current pending handoff".into());
            }
            let Some(completed) = sources.get(&sequence) else { return Ok(None); };
            if !completed.terminal || completed.context_publication.is_some() {
                return Err("earlier exact input has no completed terminal handoff".into());
            }
            self.admitted_replay_handoff(completed)?;
            Ok(Some(sequence))
        }).map_err(String::from)
    }

    fn checked_input_artifact_record(
        &self, sequence: u64, artifact: &ArtifactRef,
    ) -> Result<(Vec<u8>, RunRecord), String> {
        let mut index = self.session.index()?;
        let (coordinate, source) = index.exact_provider_source_outbound(
            &self.config.run, &self.config.worker, &self.config.provider_interface, sequence,
        )?;
        if source.artifact != *artifact {
            return Err("completed input source has a different exact artifact".into());
        }
        let key = artifact_ref_key(artifact);
        let inputs = index.session_evidence(&self.config.run, &self.config.provider_interface,
            std::iter::once(key.clone()))?;
        let label = inputs.get(&key).and_then(|evidence| evidence.provider_inputs.get(&sequence))
            .ok_or("completed input source has no admitted provider input evidence")?;
        let (manifest, _) = read_pinned_artifact(&self.ctx, artifact)?;
        if *label != manifest.label {
            return Err("completed input admission differs from its pinned artifact".into());
        }
        drop(index);
        let hash = source.run_record_hash.as_deref().and_then(ContentHash::from_hex)
            .ok_or("completed input source has no pinned run record")?;
        let (bytes, record) = pinned_run_record(&self.ctx, &hash, "completed input source")?;
        let config = config_from_persisted_profile(&record, &self.config.provider_interface, &RunAuthoringProfile {
            file_write_tool: self.config.file_write_tool.clone(), file_write_label: self.config.file_write_label.clone(),
            file_write_result_label: self.config.file_write_result_label.clone(), result_timeout_ms: self.config.result_timeout_ms,
        })?;
        let checkpoint_id = CheckpointId::new(&coordinate.checkpoint);
        let checkpoint = record.checkpoint(&checkpoint_id).ok_or("completed input lost its pinned checkpoint")?;
        let templates = config.checkpoints.iter().filter(|template|
            template.role.is_provider_input() && template.interface() == &source.source
                && checkpoint_record_matches_template(checkpoint, template, &self.config.run)
        ).collect::<Vec<_>>();
        if record.id != self.config.run || record.worker != self.config.worker
            || !record.active_checkpoints.contains(&checkpoint_id) || checkpoint.epoch.0 != coordinate.epoch
            || templates.len() != 1 || !indexed_evidence_matches_checkpoint(&source, templates[0])
            || !artifact_refs_match_bindings(&source.artifact_binding_refs, &checkpoint.artifact_bindings)
        {
            return Err("completed input source differs from its exact pinned input contract".into());
        }
        Ok((bytes, record))
    }

    pub(super) fn recover_overtaken_source(
        &self,
        current: &[u8],
        pending: &PendingHandoff,
    ) -> Result<(ProviderSourceRecovery, Vec<ProviderHandoffOutcome>, Vec<CheckedRunTransition>), String> {
        self.validate_independent_handoff_source(pending)?;
        let (_, source) = self.session.index()?.exact_provider_source_outbound(
            &self.config.run, &self.config.worker, &self.config.provider_interface,
            self.source_movement_seq,
        )?;
        let hash = source.run_record_hash.as_deref().and_then(ContentHash::from_hex)
            .ok_or("replayed provider source has no pinned run record")?;
        let (before, original) = pinned_run_record(&self.ctx, &hash, "replayed provider source")?;
        let prior = persisted_pending_handoff(&original, &self.config.provider_interface)?;
        let provider_writer = CheckedRunWriter {
            owner: self.config.provider_interface.clone(),
            source: self.config.authoring_source_interface.clone(),
            door: self.config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface().clone(),
            tool: self.config.file_write_tool.clone(),
            call_label: self.config.file_write_label.clone(),
            result_label: self.config.file_write_result_label.clone(),
        };
        let mut writers = self.config.checkpoints.iter()
            .filter_map(|checkpoint| checkpoint.owner_authoring.clone()).collect::<Vec<_>>();
        writers.push(provider_writer.clone());
        let writers = declared_writer_closure(&original, writers)?;
        let limits = self.ctx.required_host_io_limits()?;
        let evidence = CheckedRunEvidence {
            movements_path: &self.ctx.movements_path,
            ledger_index_path: &self.ctx.ledger_index_path,
            blobs_dir: &self.ctx.blobs_dir,
            max_control_frame_bytes: limits.max_control_frame_bytes,
            max_manifest_bytes: limits.max_manifest_bytes,
            max_payload_bytes: limits.max_payload_bytes,
        };
        let chain = stable_run_observation(&self.run_path, current, limits.max_payload_bytes, || {
            let start = concurrent_writer_evidence_start(&evidence, &original, &before, self.source_movement_seq)?;
            checked_run_file_chain_state(&evidence, &self.run_path, &before, current, start, &writers)
        }).map_err(String::from)?;
        if chain.is_empty() {
            return Err("overtaken provider source has no checked authoring chain".into());
        }
        let mut previous = prior.clone();
        let mut recovered = None;
        let mut independent_sources = BTreeMap::new();
        let mut earlier_sources = BTreeMap::new();
        for transition in &chain {
            if transition.writer == provider_writer {
                validate_checked_provider_transition(transition)?;
            } else {
                validate_checked_owner_transition(transition)?;
            }
            let record = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
            let next = persisted_pending_handoff(&record, &self.config.provider_interface)?
                .ok_or("checked source replay lost its pending handoff")?;
            if let Some(previous) = previous.as_ref() {
                if previous.items != next.items {
                    if previous.context_publication.is_some() {
                        return Err("provider handoff was superseded before its context was published".into());
                    }
                    let outcomes = self.admitted_replay_handoff(previous)?;
                    if outcomes.iter().any(|outcome| matches!(outcome,
                        ProviderHandoffOutcome::Admitted { seq, .. } if *seq >= transition.call.seq.0)) {
                        return Err("provider handoff was superseded before its artifacts were admitted".into());
                    }
                }
            }
            if next.source_movement_seq == self.source_movement_seq {
                recovered = Some(next.clone());
            } else if prior.as_ref().is_none_or(|prior| prior.source_movement_seq != next.source_movement_seq) {
                if next.source_movement_seq > self.source_movement_seq {
                    independent_sources.insert(next.source_movement_seq, next.clone());
                } else {
                    earlier_sources.insert(next.source_movement_seq, next.clone());
                }
            }
            previous = Some(next);
        }
        if previous.as_ref() != Some(pending) {
            return Err("checked source replay differs from the current pending handoff".into());
        }
        for independent in independent_sources.values() {
            self.validate_independent_handoff_source(independent)?;
            self.admitted_replay_handoff(independent)?;
        }
        for earlier in earlier_sources.values() {
            self.validate_replay_handoff_source(earlier, false)?;
            self.admitted_replay_handoff(earlier)?;
        }
        self.admitted_replay_handoff(pending)?;
        let (recovery, outcomes) = match recovered {
            None => (ProviderSourceRecovery::Process, Vec::new()),
            Some(recovered) => {
                let outcomes = self.admitted_replay_handoff(&recovered)?;
                let recovery = if recovered.terminal {
                    ProviderSourceRecovery::Completed
                } else {
                    ProviderSourceRecovery::Interrupted {
                        last_artifact_label: recovered.items.last()
                            .ok_or("replayed source has no output artifacts")?.artifact_label.clone(),
                    }
                };
                (recovery, outcomes)
            }
        };
        Ok((recovery, outcomes, chain))
    }

    pub(super) fn restore_replayed_context(
        &self, state: &mut HandoffState, current: &RunRecord, chain: &[CheckedRunTransition],
    ) -> Result<(), String> {
        let publications = self.checked_context_publications(chain, &state.run_record_bytes)?;
        for transition in chain {
            let record = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
            self.restore_published_context(state, current, &record, &publications)?;
        }
        Ok(())
    }

    pub(super) fn admitted_replay_handoff(&self, pending: &PendingHandoff) -> Result<Vec<ProviderHandoffOutcome>, String> {
        pending.items.iter().zip(self.pending_handoff_evidence(pending)?)
            .map(|(item, admitted)| {
                let seq = admitted.ok_or_else(|| format!(
                    "provider source {} has no admitted ledger evidence for '{}'",
                    pending.source_movement_seq, item.artifact_id,
                ))?;
                if seq <= pending.source_movement_seq {
                    return Err("provider handoff admission does not follow its exact source".into());
                }
                let (_, artifact) = load_pending_artifact(&self.var_dir, item)?;
                Ok(ProviderHandoffOutcome::Admitted { seq, artifact })
            }).collect()
    }

    fn validate_independent_handoff_source(&self, pending: &PendingHandoff) -> Result<(), String> {
        self.validate_replay_handoff_source(pending, true)
    }

    fn validate_replay_handoff_source(&self, pending: &PendingHandoff, independent: bool) -> Result<(), String> {
        let (coordinate, source) = self.session.index()?.exact_provider_source_outbound(
            &self.config.run, &self.config.worker, &self.config.provider_interface,
            pending.source_movement_seq,
        )?;
        let pinned = indexed_source_run_record(&self.ctx, &source, pending.source_movement_seq)?;
        let checkpoint_id = CheckpointId::new(&coordinate.checkpoint);
        let checkpoint = pinned.checkpoint(&checkpoint_id)
            .ok_or("replayed handoff source lost its pinned checkpoint")?;
        let context_free = !independent || self.source_is_context_free(pending.source_movement_seq)?;
        let templates = self.config.checkpoints.iter().filter(|template| {
            (!independent || (matches!(template.role,
                RunAuthoringCheckpointRole::IndependentInput | RunAuthoringCheckpointRole::OwnerInput)
                && !template.include_context))
                && template.interface() == &source.source
                && checkpoint_record_matches_template(checkpoint, template, &self.config.run)
        }).collect::<Vec<_>>();
        if !pending.terminal || !context_free || pinned.id != self.config.run || pinned.worker != self.config.worker
            || !pinned.active_checkpoints.contains(&checkpoint_id)
            || checkpoint.epoch.0 != coordinate.epoch || templates.len() != 1
            || !indexed_evidence_matches_checkpoint(&source, templates[0])
            || !artifact_refs_match_bindings(&source.artifact_binding_refs, &checkpoint.artifact_bindings)
        {
            let authority = if independent { "independent-input" } else { "earlier-source" };
            return Err(format!("provider source {} has no exact terminal {authority} handoff", pending.source_movement_seq));
        }
        Ok(())
    }
}
