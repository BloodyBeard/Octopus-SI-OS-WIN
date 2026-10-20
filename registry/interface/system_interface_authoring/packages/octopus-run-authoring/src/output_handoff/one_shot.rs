use super::*;

impl ProviderOutputHandoff {
    pub(crate) async fn hand_one_shot_batch(
        &self,
        emissions: Vec<ProviderHandoffEmission>,
        plan: &OneShotHandoffPlan,
        terminal: bool,
    ) -> Result<Vec<ProviderHandoffOutcome>, String> {
        let mut state = self.state.lock().await;
        let completion_coordinate = RuntimeCoordinateKey::new(
            &self.config.run,
            &self.config.worker,
            &self.config.provider_interface,
            &plan.completion.checkpoint,
            plan.completion.epoch,
        );
        let completion = self.session.index()?.admitted_slot_artifacts(
            &completion_coordinate,
            &plan.completion.required_mark,
            &plan.completion.required_verifier,
        )?;
        if let Some(evidence) = completion.first() {
            self.validate_one_shot_completion_evidence(plan, evidence)?;
        }
        if !terminal
            && (!completion.is_empty() || !self.one_shot_terminal_evidence(plan)?.is_empty())
        {
            return Err("provider one-shot progress follows terminal evidence".into());
        }
        let mut next_session_context = state.session_context.clone();
        let mut next_session_snapshot_head = state.session_snapshot_head.clone();
        let mut next_context = state.context_artifacts.clone();
        let mut prepared = Vec::with_capacity(emissions.len());
        let mut checkpoints = HashSet::new();
        for emission in emissions {
            let ProviderHandoffEmission {
                mut artifact,
                route,
            } = emission;
            if route != ProviderHandoffRoute::ClientProjection {
                return Err(
                    "provider one-shot output batch contains a continuation route".to_string(),
                );
            }
            let manifest = artifact.read_manifest().map_err(|error| {
                format!("provider one-shot output manifest cannot be read: {error}")
            })?;
            let slot = plan.terminal.get(&manifest.label).ok_or_else(|| {
                format!(
                    "provider one-shot topology has no verifier-isolated output slot for '{}'",
                    manifest.label
                )
            })?;
            if !checkpoints.insert(slot.checkpoint.clone()) {
                return Err(format!(
                    "provider one-shot output batch spends checkpoint '{}' more than once",
                    slot.checkpoint.as_str()
                ));
            }
            let context_output = self.context_output_labels.contains(&manifest.label);
            let snapshot_output = self.session_snapshot_labels.contains(&manifest.label);
            artifact = self.prepare_one_shot_output(artifact, terminal)?;
            let payload = if context_output {
                Some(artifact.read_payload_bytes().map_err(|error| {
                    format!("provider one-shot output payload cannot be read: {error}")
                })?)
            } else {
                None
            };
            if snapshot_output {
                artifact = materialize_session_snapshot(
                    &artifact,
                    &next_session_context,
                    next_session_snapshot_head.as_ref(),
                )?;
                next_context = state.static_context_artifacts.clone();
            }
            let identity = artifact_identity(&artifact, &self.var_dir)?;
            if !completion.is_empty() {
                let coordinate = RuntimeCoordinateKey::new(
                    &self.config.run,
                    &self.config.worker,
                    &self.config.provider_interface,
                    &slot.checkpoint,
                    slot.epoch,
                );
                let admitted = self.session.index()?.admitted_slot_artifacts(
                    &coordinate,
                    &slot.required_mark,
                    &slot.required_verifier,
                )?;
                if !admitted
                    .iter()
                    .any(|evidence| evidence.artifact == identity.artifact)
                {
                    return Err(
                        "provider one-shot output would extend completed terminal evidence".into(),
                    );
                }
            }
            if context_output {
                append_session_payload(
                    &mut next_session_context,
                    &manifest.label,
                    payload.as_deref().ok_or_else(|| {
                        "provider one-shot context output lost its payload".to_string()
                    })?,
                    &identity.artifact,
                    None,
                    "provider one-shot output",
                )?;
                append_artifact(&mut next_context, identity.artifact.id.clone());
            }
            if snapshot_output {
                next_session_snapshot_head = Some(session_snapshot_head_after_output(
                    identity.artifact.clone(),
                    &next_session_context,
                )?);
            }
            prepared.push((artifact, slot.clone(), identity.artifact));
        }
        let mut outcomes = Vec::with_capacity(prepared.len());
        let mut previous_mark_seq = None;
        for (artifact, slot, _) in prepared {
            let evidence = self
                .hand_one_shot_artifact_evidence(artifact, &slot)
                .await?;
            if previous_mark_seq.is_some_and(|previous| previous >= evidence.arrival_seq) {
                return Err(
                    "provider one-shot output artifact order differs from ledger order".to_string(),
                );
            }
            previous_mark_seq = Some(evidence.mark_seq);
            outcomes.push(ProviderHandoffOutcome::Admitted {
                seq: evidence.arrival_seq,
                artifact: evidence.artifact.clone(),
            });
        }
        if terminal {
            let terminal_evidence = self.one_shot_terminal_evidence(plan)?;
            let completion_artifact =
                self.materialize_one_shot_completion_artifact(&terminal_evidence)?;
            let completion_evidence = self
                .hand_one_shot_artifact_evidence(completion_artifact, &plan.completion)
                .await?;
            self.validate_one_shot_completion_evidence(plan, &completion_evidence)?;
        }
        state.context_artifacts = next_context;
        state.session_context = next_session_context;
        state.session_snapshot_head = next_session_snapshot_head;
        state.consumed_input = false;
        state.consumed_context_free_independent_input = None;
        Ok(outcomes)
    }

    pub(crate) fn prepare_one_shot_output(
        &self,
        artifact: InterfaceArtifactFile,
        terminal: bool,
    ) -> Result<InterfaceArtifactFile, String> {
        let payload = artifact
            .read_payload_bytes()
            .map_err(|error| error.to_string())?;
        let value = serde_json::from_slice::<serde_json::Value>(&payload);
        if value
            .as_ref()
            .is_ok_and(|value| value.get(ONE_SHOT_PROGRESS_FIELD).is_some())
        {
            return Err(
                "provider output already contains reserved one-shot progress evidence".into(),
            );
        }
        if terminal {
            return Ok(artifact);
        }
        let mut value = value.map_err(|error| {
            format!("provider one-shot progress must be a JSON object: {error}")
        })?;
        let object = value
            .as_object_mut()
            .ok_or("provider one-shot progress must be a JSON object")?;
        object.insert(
            ONE_SHOT_PROGRESS_FIELD.into(),
            serde_json::to_value(OneShotProgressIdentity {
                provider_interface: self.config.provider_interface.clone(),
                run: self.config.run.clone(),
                source_movement_seq: self.source_movement_seq,
            })
            .map_err(|error| error.to_string())?,
        );
        let label = artifact
            .read_manifest()
            .map_err(|error| error.to_string())?
            .label;
        InterfaceArtifactFile::write(
            self.var_dir.join("one-shot-progress"),
            &label,
            &serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }

    pub(crate) fn is_one_shot_progress(&self, payload: &[u8]) -> Result<bool, String> {
        let value = match serde_json::from_slice::<serde_json::Value>(payload) {
            Ok(value) => value,
            Err(_) => return Ok(false),
        };
        let Some(progress) = value.get(ONE_SHOT_PROGRESS_FIELD) else {
            return Ok(false);
        };
        let progress: OneShotProgressIdentity =
            serde_json::from_value(progress.clone()).map_err(|error| {
                format!("provider one-shot progress evidence is malformed: {error}")
            })?;
        if progress.provider_interface != self.config.provider_interface
            || progress.run != self.config.run
            || progress.source_movement_seq != self.source_movement_seq
        {
            return Err("provider one-shot progress evidence belongs to another invocation".into());
        }
        Ok(true)
    }

    pub(crate) async fn hand_one_shot_artifact(
        &self,
        artifact: InterfaceArtifactFile,
        slot: &OneShotHandoffSlot,
    ) -> Result<ProviderHandoffOutcome, String> {
        let evidence = self.hand_one_shot_artifact_evidence(artifact, slot).await?;
        Ok(ProviderHandoffOutcome::Admitted {
            seq: evidence.arrival_seq,
            artifact: evidence.artifact,
        })
    }

    pub(crate) async fn hand_one_shot_artifact_evidence(
        &self,
        artifact: InterfaceArtifactFile,
        slot: &OneShotHandoffSlot,
    ) -> Result<OneShotAdmittedArtifact, String> {
        let identity = artifact_identity(&artifact, &self.var_dir)?;
        let coordinate = RuntimeCoordinateKey::new(
            &self.config.run,
            &self.config.worker,
            &self.config.provider_interface,
            &slot.checkpoint,
            slot.epoch,
        );
        let existing = self.session.index()?.admitted_slot_artifacts(
            &coordinate,
            &slot.required_mark,
            &slot.required_verifier,
        )?;
        if let Some(admitted) = existing.first() {
            if admitted.artifact == identity.artifact {
                self.validate_one_shot_evidence_source(admitted)?;
                return Ok(admitted.clone());
            }
            return Err(format!(
                "provider one-shot checkpoint '{}' was already spent by artifact '{}'",
                slot.checkpoint.as_str(),
                admitted.artifact.id.as_str()
            ));
        }
        let verdict = self.ctx.hand_arrival(artifact, Vec::new()).await?;
        let admitted = self.session.index()?.admitted_slot_artifacts(
            &coordinate,
            &slot.required_mark,
            &slot.required_verifier,
        )?;
        match (verdict, admitted.first()) {
            (DeliveryVerdict::Admitted { seq }, Some(evidence)) => {
                if evidence.artifact != identity.artifact {
                    return Err(format!(
                        "provider one-shot checkpoint '{}' admitted artifact '{}' instead of '{}'",
                        slot.checkpoint.as_str(),
                        evidence.artifact.id.as_str(),
                        identity.artifact.id.as_str()
                    ));
                }
                if evidence.arrival_seq != seq {
                    return Err(format!(
                        "provider one-shot artifact '{}' verdict seq {} differs from exact arrival seq {}",
                        identity.artifact.id.as_str(),
                        seq,
                        evidence.arrival_seq
                    ));
                }
                self.validate_one_shot_evidence_source(evidence)?;
                Ok(evidence.clone())
            }
            (DeliveryVerdict::Refused { .. }, Some(evidence)) => {
                if evidence.artifact != identity.artifact {
                    return Err(format!(
                        "provider one-shot checkpoint '{}' admitted artifact '{}' instead of '{}'",
                        slot.checkpoint.as_str(),
                        evidence.artifact.id.as_str(),
                        identity.artifact.id.as_str()
                    ));
                }
                self.validate_one_shot_evidence_source(evidence)?;
                Ok(evidence.clone())
            }
            (DeliveryVerdict::Admitted { .. }, None) => Err(format!(
                "provider one-shot artifact '{}' was admitted without exact checkpoint/mark evidence",
                identity.artifact.id.as_str()
            )),
            (DeliveryVerdict::Refused { reason, detail }, None) => Err(format!(
                "provider one-shot artifact '{}' was refused as '{}': {}",
                identity.artifact.id.as_str(),
                reason.as_str(),
                detail
            )),
        }
    }

    pub(crate) fn validate_one_shot_evidence_source(
        &self,
        evidence: &OneShotAdmittedArtifact,
    ) -> Result<(), String> {
        if evidence.arrival_seq <= self.source_movement_seq {
            return Err(format!(
                "provider one-shot artifact '{}' arrival seq {} does not follow source movement {}",
                evidence.artifact.id.as_str(),
                evidence.arrival_seq,
                self.source_movement_seq
            ));
        }
        Ok(())
    }
}

impl ProviderOutputHandoff {
    pub(crate) async fn recover_one_shot(
        &self,
        plan: &OneShotHandoffPlan,
    ) -> Result<ProviderSourceRecovery, String> {
        let completion_coordinate = RuntimeCoordinateKey::new(
            &self.config.run,
            &self.config.worker,
            &self.config.provider_interface,
            &plan.completion.checkpoint,
            plan.completion.epoch,
        );
        let completion = self.session.index()?.admitted_slot_artifacts(
            &completion_coordinate,
            &plan.completion.required_mark,
            &plan.completion.required_verifier,
        )?;
        if let Some(completion_evidence) = completion.first() {
            self.validate_one_shot_completion_evidence(plan, completion_evidence)?;
            return Ok(ProviderSourceRecovery::Completed);
        }

        let terminal = self.one_shot_terminal_evidence(plan)?;
        let dispatched = self.one_shot_dispatch_evidence(plan)?;
        Self::validate_one_shot_dispatch_order(&dispatched, &terminal)?;
        if !terminal.is_empty() {
            let completion_artifact = self.materialize_one_shot_completion_artifact(&terminal)?;
            let completion_evidence = self
                .hand_one_shot_artifact_evidence(completion_artifact, &plan.completion)
                .await?;
            self.validate_one_shot_completion_evidence(plan, &completion_evidence)?;
            return Ok(ProviderSourceRecovery::Completed);
        }
        match dispatched.last() {
            None => Ok(ProviderSourceRecovery::Process),
            Some(_) => Ok(ProviderSourceRecovery::Interrupted {
                last_artifact_label: PROVIDER_TRANSPORT_DISPATCH_LABEL.to_string(),
            }),
        }
    }

    pub(crate) fn materialize_one_shot_completion_artifact(
        &self,
        terminal: &[(String, OneShotAdmittedArtifact)],
    ) -> Result<InterfaceArtifactFile, String> {
        if terminal.is_empty() {
            return Err(
                "provider one-shot completion contains no terminal ledger evidence".to_string(),
            );
        }
        let completion = ProviderHandoffCompletion {
            label: PROVIDER_HANDOFF_COMPLETE_LABEL.to_string(),
            provider_interface: self.config.provider_interface.clone(),
            source_movement_seq: self.source_movement_seq,
            artifacts: terminal
                .iter()
                .map(|(_, evidence)| evidence.artifact.clone())
                .collect(),
        };
        let payload = serde_json::to_vec(&completion)
            .map_err(|error| format!("provider handoff completion did not encode: {error}"))?;
        InterfaceArtifactFile::write(
            self.var_dir.join("handoff-completion"),
            PROVIDER_HANDOFF_COMPLETE_LABEL,
            &payload,
        )
        .map_err(|error| {
            format!("provider handoff completion artifact could not be written: {error}")
        })
    }

    pub(crate) fn validate_one_shot_completion_evidence(
        &self,
        plan: &OneShotHandoffPlan,
        completion_evidence: &OneShotAdmittedArtifact,
    ) -> Result<(), String> {
        self.validate_one_shot_evidence_source(completion_evidence)?;
        let payload = read_pinned_artifact_payload(
            &self.ctx,
            &completion_evidence.artifact,
            PROVIDER_HANDOFF_COMPLETE_LABEL,
        )?;
        let completion = validate_handoff_completion_payload(
            &payload,
            &self.config.provider_interface,
            self.source_movement_seq,
        )?;
        let terminal = self.one_shot_terminal_evidence(plan)?;
        let admitted = terminal
            .iter()
            .map(|(_, evidence)| evidence.artifact.clone())
            .collect::<Vec<_>>();
        if completion.artifacts != admitted {
            let completion_set = completion
                .artifacts
                .iter()
                .map(artifact_ref_key)
                .collect::<BTreeSet<_>>();
            let admitted_set = admitted
                .iter()
                .map(artifact_ref_key)
                .collect::<BTreeSet<_>>();
            if completion_set != admitted_set {
                return Err(format!(
                    "provider one-shot completion artifact set differs from admitted terminal evidence: completion={completion_set:?}, admitted={admitted_set:?}"
                ));
            }
            return Err(
                "provider one-shot completion artifact order differs from ledger order".to_string(),
            );
        }
        let last_terminal_mark = terminal
            .last()
            .map(|(_, evidence)| evidence.mark_seq)
            .ok_or_else(|| {
                "provider one-shot completion contains no terminal ledger evidence".to_string()
            })?;
        if last_terminal_mark >= completion_evidence.arrival_seq {
            return Err(
                "provider one-shot completion evidence does not follow every terminal mark"
                    .to_string(),
            );
        }
        let dispatched = self.one_shot_dispatch_evidence(plan)?;
        Self::validate_one_shot_dispatch_order(&dispatched, &terminal)
    }

    pub(crate) fn validate_one_shot_dispatch_order(
        dispatched: &[OneShotAdmittedArtifact],
        terminal: &[(String, OneShotAdmittedArtifact)],
    ) -> Result<(), String> {
        if let Some(first_terminal_arrival) =
            terminal.first().map(|(_, evidence)| evidence.arrival_seq)
        {
            if dispatched
                .iter()
                .any(|evidence| evidence.mark_seq >= first_terminal_arrival)
            {
                return Err(
                    "provider one-shot transport dispatch evidence does not precede every terminal arrival"
                        .to_string(),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn one_shot_terminal_evidence(
        &self,
        plan: &OneShotHandoffPlan,
    ) -> Result<Vec<(String, OneShotAdmittedArtifact)>, String> {
        let mut raw = Vec::new();
        let mut index = self.session.index()?;
        for (label, slot) in &plan.terminal {
            let coordinate = RuntimeCoordinateKey::new(
                &self.config.run,
                &self.config.worker,
                &self.config.provider_interface,
                &slot.checkpoint,
                slot.epoch,
            );
            for evidence in index.admitted_slot_artifacts(
                &coordinate,
                &slot.required_mark,
                &slot.required_verifier,
            )? {
                raw.push((label.clone(), slot.checkpoint.clone(), evidence));
            }
        }
        drop(index);
        let mut terminal = Vec::with_capacity(raw.len());
        let mut artifacts = HashSet::new();
        for (label, checkpoint, evidence) in raw {
            self.validate_one_shot_evidence_source(&evidence)?;
            let expected_id = format!("{label}:{}", evidence.artifact.content.to_hex());
            if evidence.artifact.id.as_str() != expected_id {
                return Err(format!(
                    "provider one-shot terminal checkpoint '{}' admitted artifact '{}' outside label '{}'",
                    checkpoint.as_str(),
                    evidence.artifact.id.as_str(),
                    label
                ));
            }
            let payload = read_pinned_artifact_payload(&self.ctx, &evidence.artifact, &label)?;
            if self.is_one_shot_progress(&payload)? {
                continue;
            }
            let key = artifact_ref_key(&evidence.artifact);
            if !artifacts.insert(key.clone()) {
                return Err(format!(
                    "provider one-shot terminal evidence repeats artifact '{key}'"
                ));
            }
            terminal.push((label, evidence));
        }
        terminal.sort_by_key(|(_, evidence)| evidence.arrival_seq);
        let mut previous_mark_seq = None;
        for (_, evidence) in &terminal {
            if previous_mark_seq.is_some_and(|previous| previous >= evidence.arrival_seq) {
                return Err(
                    "provider one-shot terminal evidence overlaps in ledger order".to_string(),
                );
            }
            previous_mark_seq = Some(evidence.mark_seq);
        }
        Ok(terminal)
    }

    pub(crate) fn one_shot_dispatch_evidence(
        &self,
        plan: &OneShotHandoffPlan,
    ) -> Result<Vec<OneShotAdmittedArtifact>, String> {
        let mut dispatched = Vec::new();
        let mut index = self.session.index()?;
        for (attempt, slot) in &plan.dispatch {
            let coordinate = RuntimeCoordinateKey::new(
                &self.config.run,
                &self.config.worker,
                &self.config.provider_interface,
                &slot.checkpoint,
                slot.epoch,
            );
            for evidence in index.admitted_slot_artifacts(
                &coordinate,
                &slot.required_mark,
                &slot.required_verifier,
            )? {
                self.validate_one_shot_evidence_source(&evidence)?;
                let payload = read_pinned_artifact_payload(
                    &self.ctx,
                    &evidence.artifact,
                    PROVIDER_TRANSPORT_DISPATCH_LABEL,
                )?;
                let dispatch = validate_transport_dispatch_payload(
                    &payload,
                    &self.ctx.interface,
                    self.source_movement_seq,
                )?;
                if dispatch.attempt != *attempt {
                    return Err(format!(
                        "provider one-shot dispatch checkpoint '{}' proves attempt {} instead of {}",
                        slot.checkpoint.as_str(),
                        dispatch.attempt,
                        attempt
                    ));
                }
                dispatched.push(evidence);
            }
        }
        drop(index);
        dispatched.sort_by_key(|evidence| evidence.arrival_seq);
        let mut previous_mark_seq = None;
        for evidence in &dispatched {
            if previous_mark_seq.is_some_and(|previous| previous >= evidence.arrival_seq) {
                return Err(
                    "provider one-shot transport dispatch evidence overlaps in ledger order"
                        .to_string(),
                );
            }
            previous_mark_seq = Some(evidence.mark_seq);
        }
        Ok(dispatched)
    }
}
