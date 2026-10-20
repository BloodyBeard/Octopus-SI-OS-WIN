use super::*;

impl InterruptedEffectArbiter {
    pub(crate) fn new(ctx: InterfaceLiftContext) -> Self {
        Self {
            ctx,
            cache: Arc::new(tokio::sync::Mutex::new(InterruptedEffectCache::default())),
        }
    }

    pub(crate) async fn journal(&self, run: &RunId) -> Result<Option<InterruptedEffectJournal>, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        Ok(cache.journals.get(run.as_str()).cloned())
    }

    pub(crate) async fn durable_continuity(
        &self,
        run: &RunId,
    ) -> Result<Option<crate::handler::CodexContinuityState>, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let Some(journal) = cache.journals.get(&key).cloned() else {
            return Ok(None);
        };
        match journal.phase {
            InterruptedEffectPhase::AwaitingResult
            | InterruptedEffectPhase::Resolving { .. }
            | InterruptedEffectPhase::Closing { .. } => Ok(Some(journal.continuity)),
            InterruptedEffectPhase::Reconciled { .. } => Ok(None),
        }
    }

    pub(crate) async fn reconciled(&self, run: &RunId) -> Result<bool, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        Ok(cache.journals.get(run.as_str()).is_none_or(|journal| {
            matches!(&journal.phase, InterruptedEffectPhase::Reconciled { .. })
        }))
    }

    pub(crate) async fn reconciled_result(
        &self, run: &RunId,
    ) -> Result<Option<(crate::handler::CodexContinuityState, u64, DurableArtifactAdmission)>, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        Ok(cache.journals.get(run.as_str()).and_then(|journal| match &journal.phase {
            InterruptedEffectPhase::Reconciled { result_source_movement_seq, closure } =>
                Some((journal.continuity.clone(), *result_source_movement_seq, closure.clone())),
            _ => None,
        }))
    }

    pub(crate) async fn record_acknowledgement(
        &self,
        run: &RunId,
        control_source_movement_seq: u64,
        control_id: &str,
        acknowledgement: DurableArtifactAdmission,
        continuity: crate::handler::CodexContinuityState,
    ) -> Result<(), String> {
        validate_durable_interrupted_continuity(&continuity, &self.ctx.interface, run.as_str())?;
        validate_artifact_admission(
            &self.ctx,
            run,
            control_source_movement_seq,
            &acknowledgement,
        )?;
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let next = InterruptedEffectJournal {
            schema_version: 1,
            run_id: key.clone(),
            control_source_movement_seq,
            control_id: control_id.to_string(),
            acknowledgement,
            continuity,
            phase: InterruptedEffectPhase::AwaitingResult,
        };
        if let Some(current) = cache.journals.get(&key) {
            let same_ack = current.control_source_movement_seq == control_source_movement_seq
                && current.control_id == control_id
                && current.acknowledgement == next.acknowledgement
                && current.continuity == next.continuity;
            if control_source_movement_seq > current.control_source_movement_seq
                && control_id != current.control_id
                && next.acknowledgement.movement_seq > current.acknowledgement.movement_seq
                && crate::handler::codex_continuity_retains_interrupted_effect(&current.continuity, &next.continuity)
            {
                return Ok(());
            }
            match &current.phase {
                InterruptedEffectPhase::AwaitingResult if same_ack => return Ok(()),
                InterruptedEffectPhase::Resolving { .. }
                | InterruptedEffectPhase::Closing { .. }
                | InterruptedEffectPhase::Reconciled { .. }
                    if same_ack =>
                {
                    return Ok(());
                }
                InterruptedEffectPhase::AwaitingResult
                | InterruptedEffectPhase::Resolving { .. }
                | InterruptedEffectPhase::Closing { .. } => {
                    return Err(format!(
                        "run '{}' already has a different unresolved interrupted exact effect",
                        run.as_str()
                    ));
                }
                InterruptedEffectPhase::Reconciled { .. } => {}
            }
        }
        self.persist(&next)?;
        cache.journals.insert(key, next);
        Ok(())
    }

    pub(crate) async fn begin_result(
        &self,
        run: &RunId,
        result_source_movement_seq: u64,
        envelope: &InterfaceInvocationEnvelope,
    ) -> Result<bool, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let Some(journal) = cache.journals.get_mut(&key) else {
            return Ok(false);
        };
        if let InterruptedEffectPhase::Reconciled {
            result_source_movement_seq: current_source,
            ..
        } = &journal.phase
        {
            if *current_source != result_source_movement_seq {
                return Ok(false);
            }
            if !exact_interrupted_result_source(envelope, &journal.continuity)? {
                return Err(format!(
                    "run '{}' replayed interrupted exact-result source {} with different evidence",
                    run.as_str(),
                    result_source_movement_seq
                ));
            }
            return Ok(true);
        }
        if !exact_interrupted_result_source(envelope, &journal.continuity)? {
            return Ok(false);
        }
        let source_artifact = envelope.artifact.clone();
        match &journal.phase {
            InterruptedEffectPhase::AwaitingResult => {
                journal.phase = InterruptedEffectPhase::Resolving {
                    result_source_movement_seq,
                    source_artifact,
                };
            }
            InterruptedEffectPhase::Resolving {
                result_source_movement_seq: current_source,
                source_artifact: current_artifact,
            } if *current_source == result_source_movement_seq
                && *current_artifact == source_artifact =>
            {
                return Ok(true);
            }
            InterruptedEffectPhase::Closing {
                result_source_movement_seq: current_source,
                ..
            }
            | InterruptedEffectPhase::Reconciled {
                result_source_movement_seq: current_source,
                ..
            } if *current_source == result_source_movement_seq => return Ok(true),
            _ => {
                return Err(format!(
                    "run '{}' attempted to resolve a different interrupted exact-result source",
                    run.as_str()
                ));
            }
        }
        self.persist(journal)?;
        Ok(true)
    }

    pub(crate) async fn begin_closing(
        &self,
        run: &RunId,
        result_source_movement_seq: u64,
        closure_artifact: ArtifactRef,
    ) -> Result<(), String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let journal = cache.journals.get_mut(&key).ok_or_else(|| {
            format!(
                "run '{}' emitted an interrupted closure without durable ACK arbitration",
                run.as_str()
            )
        })?;
        match &journal.phase {
            InterruptedEffectPhase::Resolving {
                result_source_movement_seq: current_source,
                ..
            } if *current_source == result_source_movement_seq => {
                journal.phase = InterruptedEffectPhase::Closing {
                    result_source_movement_seq,
                    closure_artifact,
                };
            }
            InterruptedEffectPhase::Closing {
                result_source_movement_seq: current_source,
                closure_artifact: current_closure,
            } if *current_source == result_source_movement_seq
                && *current_closure == closure_artifact =>
            {
                return Ok(());
            }
            InterruptedEffectPhase::Reconciled {
                result_source_movement_seq: current_source,
                closure,
            } if *current_source == result_source_movement_seq
                && closure.artifact == closure_artifact =>
            {
                return Ok(());
            }
            _ => {
                return Err(format!(
                    "run '{}' attempted a conflicting interrupted-result closure",
                    run.as_str()
                ));
            }
        }
        self.persist(journal)
    }

    pub(crate) async fn reconciled_source(
        &self,
        run: &RunId,
        result_source_movement_seq: u64,
    ) -> Result<bool, String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        Ok(cache.journals.get(run.as_str()).is_some_and(|journal| {
            matches!(
                journal.phase,
                InterruptedEffectPhase::Reconciled {
                    result_source_movement_seq: current_source,
                    ..
                } if current_source == result_source_movement_seq
            )
        }))
    }

    pub(crate) async fn complete_closing(
        &self,
        run: &RunId,
        result_source_movement_seq: u64,
        source_closure_artifact: &ArtifactRef,
        closure: &DurableArtifactAdmission,
    ) -> Result<(), String> {
        if !artifact_ref_has_label(source_closure_artifact, TURN_INTERRUPTED_RESULT_LABEL)
            || !artifact_ref_has_label(&closure.artifact, TURN_INTERRUPTED_RESULT_LABEL)
        {
            return Err(format!(
                "run '{}' attempted to reconcile a non-interrupted-result closure",
                run.as_str()
            ));
        }
        validate_artifact_admission(&self.ctx, run, result_source_movement_seq, closure)?;
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let journal = cache.journals.get_mut(&key).ok_or_else(|| {
            format!(
                "run '{}' completed an interrupted closure without durable ACK arbitration",
                run.as_str()
            )
        })?;
        match &journal.phase {
            InterruptedEffectPhase::Closing {
                result_source_movement_seq: current_source,
                closure_artifact: current_closure,
            } if *current_source == result_source_movement_seq
                && current_closure == source_closure_artifact =>
            {
                journal.phase = InterruptedEffectPhase::Reconciled {
                    result_source_movement_seq,
                    closure: closure.clone(),
                };
            }
            InterruptedEffectPhase::Reconciled {
                result_source_movement_seq: current_source,
                closure: current_closure,
            } if *current_source == result_source_movement_seq && current_closure == closure => {
                return Ok(());
            }
            _ => {
                return Err(format!(
                    "run '{}' completed a closure outside its durable interrupted phase",
                    run.as_str()
                ));
            }
        }
        self.persist(journal)
    }

    pub(crate) async fn recover_completed_source(
        &self,
        run: &RunId,
        result_source_movement_seq: u64,
        outcomes: &[ProviderHandoffOutcome],
    ) -> Result<(), String> {
        let mut cache = self.cache.lock().await;
        self.load_run(&mut cache, run)?;
        let key = run.as_str().to_string();
        let Some(journal) = cache.journals.get(&key).cloned() else {
            return Ok(());
        };
        match journal.phase {
            InterruptedEffectPhase::Closing {
                result_source_movement_seq: current_source,
                closure_artifact,
            } if current_source == result_source_movement_seq => {
                let closure = interrupted_result_admission(outcomes)?.ok_or_else(|| {
                    format!(
                        "run '{}' recovered exact-result source {} without its admitted interrupted-result output",
                        run.as_str(),
                        result_source_movement_seq
                    )
                })?;
                drop(cache);
                self.complete_closing(run, result_source_movement_seq, &closure_artifact, &closure)
                    .await
            }
            InterruptedEffectPhase::Resolving {
                result_source_movement_seq: current_source,
                ..
            } if current_source == result_source_movement_seq => Err(format!(
                "run '{}' completed interrupted exact-result source {} before its semantic closure was admitted",
                run.as_str(),
                result_source_movement_seq
            )),
            _ => Ok(()),
        }
    }

    pub(crate) fn load_run(&self, cache: &mut InterruptedEffectCache, run: &RunId) -> Result<(), String> {
        let key = run.as_str().to_string();
        if cache.loaded_runs.contains(&key) {
            return Ok(());
        }
        let path = interrupted_effect_journal_path(&self.ctx, run);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                cache.loaded_runs.insert(key);
                return Ok(());
            }
            Err(error) => {
                return Err(format!(
                    "interrupted-effect journal '{}' cannot be read: {error}",
                    path.display()
                ));
            }
        };
        let journal =
            serde_json::from_slice::<InterruptedEffectJournal>(&bytes).map_err(|error| {
                format!(
                    "interrupted-effect journal '{}' is malformed: {error}",
                    path.display()
                )
            })?;
        if journal.schema_version != 1 || journal.run_id != key {
            return Err(format!(
                "interrupted-effect journal '{}' has a mismatched identity",
                path.display()
            ));
        }
        validate_durable_interrupted_continuity(
            &journal.continuity,
            &self.ctx.interface,
            run.as_str(),
        )?;
        validate_artifact_admission(
            &self.ctx,
            run,
            journal.control_source_movement_seq,
            &journal.acknowledgement,
        )?;
        if let InterruptedEffectPhase::Reconciled {
            result_source_movement_seq,
            closure,
        } = &journal.phase
        {
            validate_artifact_admission(&self.ctx, run, *result_source_movement_seq, closure)?;
        }
        cache.journals.insert(key.clone(), journal);
        cache.loaded_runs.insert(key);
        Ok(())
    }

    pub(crate) fn persist(&self, journal: &InterruptedEffectJournal) -> Result<(), String> {
        let path = interrupted_effect_journal_path(&self.ctx, &RunId::new(journal.run_id.clone()));
        persist_json_file(&path, journal, "interrupted-effect journal")
    }
}

pub(crate) fn interrupted_result_admission(
    outcomes: &[ProviderHandoffOutcome],
) -> Result<Option<DurableArtifactAdmission>, String> {
    let mut matched = None;
    for outcome in outcomes {
        match outcome {
            ProviderHandoffOutcome::Admitted { seq, artifact }
                if artifact_ref_has_label(artifact, TURN_INTERRUPTED_RESULT_LABEL) =>
            {
                let admission = DurableArtifactAdmission {
                    artifact: artifact.clone(),
                    movement_seq: *seq,
                };
                if matched.replace(admission).is_some() {
                    return Err(
                        "recovered exact-result source contains more than one interrupted-result closure"
                            .to_string(),
                    );
                }
            }
            ProviderHandoffOutcome::Admitted { .. } => {}
            ProviderHandoffOutcome::TerminalFailure {
                failure_seq,
                failure_class,
                ..
            } => {
                return Err(format!(
                    "recovered exact-result source ended in run-authoring failure '{failure_class}' at movement {failure_seq}"
                ));
            }
        }
    }
    Ok(matched)
}

pub(crate) fn artifact_ref_has_label(artifact: &ArtifactRef, label: &str) -> bool {
    artifact
        .id
        .as_str()
        .split_once(':')
        .map_or(artifact.id.as_str(), |(artifact_label, _)| artifact_label)
        == label
}

pub(crate) async fn recovered_codex_handoff_artifacts(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    outcomes: &[ProviderHandoffOutcome],
) -> Result<Arc<Vec<CodexRecoveredHandoffArtifact>>, String> {
    for outcome in outcomes {
        if let ProviderHandoffOutcome::TerminalFailure {
            failure_seq,
            failure_class,
            ..
        } = outcome
        {
            return Err(format!(
                "recovered provider source contains run-authoring failure '{failure_class}' at movement {failure_seq}"
            ));
        }
    }
    let mut admissions = None;
    for _ in 0..128 {
        let scan = scan_provider_transport_dispatch_admissions(ctx, run, source_movement_seq)?;
        if scan.complete_boundary {
            admissions = Some(scan.admissions);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    let admissions = admissions.ok_or_else(|| {
        format!(
            "movement ledger '{}' retained an incomplete tail while provider transport dispatch recovery was pending",
            ctx.movements_path.display()
        )
    })?;
    let mut recovered = Vec::new();
    for admission in admissions {
        let payload = read_pinned_provider_artifact_payload(
            ctx,
            &admission.artifact,
            "recovered provider transport dispatch",
        )?;
        let value: serde_json::Value = serde_json::from_slice(&payload).map_err(|error| {
            format!(
                "recovered provider transport dispatch '{}' is malformed: {error}",
                admission.artifact.id.as_str()
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            format!(
                "recovered provider transport dispatch '{}' is not a JSON object",
                admission.artifact.id.as_str()
            )
        })?;
        if object.get("label").and_then(serde_json::Value::as_str)
            != Some(PROVIDER_TRANSPORT_DISPATCH_LABEL)
        {
            return Err(format!(
                "recovered provider transport dispatch '{}' has a conflicting payload label",
                admission.artifact.id.as_str()
            ));
        }
        let provider = object
            .get("provider_interface")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| {
                format!(
                    "recovered provider transport dispatch '{}' has no canonical provider interface",
                    admission.artifact.id.as_str()
                )
            })?;
        if provider != ctx.interface.as_str() {
            return Err(format!(
                "recovered provider transport dispatch '{}' names provider '{}' instead of '{}'",
                admission.artifact.id.as_str(),
                provider,
                ctx.interface.as_str()
            ));
        }
        let dispatch_source = object
            .get("source_movement_seq")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                format!(
                    "recovered provider transport dispatch '{}' has no source movement coordinate",
                    admission.artifact.id.as_str()
                )
            })?;
        if dispatch_source == source_movement_seq {
            recovered.push(CodexRecoveredHandoffArtifact {
                movement_seq: admission.movement_seq,
                artifact: admission.artifact,
                payload,
            });
        }
    }
    Ok(Arc::new(recovered))
}

pub(crate) struct ProviderTransportDispatchAdmissionScan {
    pub(crate) admissions: Vec<DurableArtifactAdmission>,
    pub(crate) complete_boundary: bool,
}

pub(crate) fn scan_provider_transport_dispatch_admissions(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
) -> Result<ProviderTransportDispatchAdmissionScan, String> {
    let (mut file, snapshot_len, max_row_bytes) = open_arbitration_snapshot(ctx)?;
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
    let (start_offset, expected_seq) = authoritative_source_scan_start(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        source_movement_seq,
    );
    let mut source_verified = false;
    let mut admissions = Vec::new();
    let mut seen = BTreeSet::new();
    let scan = scan_authoritative_snapshot(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        start_offset,
        expected_seq,
        None,
        |movement| {
            if movement.seq.0 < source_movement_seq {
                return Ok(None::<()>);
            }
            if movement.seq.0 == source_movement_seq {
                if !movement_is_provider_source(ctx, run, &movement) {
                    return Err(format!(
                        "provider transport-dispatch scan source movement {} does not match interface '{}' run '{}'",
                        source_movement_seq,
                        ctx.interface.as_str(),
                        run.as_str()
                    ));
                }
                source_verified = true;
                return Ok(None::<()>);
            }
            if !source_verified {
                return Err(format!(
                    "provider transport-dispatch scan advanced past causal source {source_movement_seq}"
                ));
            }
            let Some(artifact) = movement.artifact.as_ref() else {
                return Ok(None::<()>);
            };
            let runtime = movement
                .invocation_metadata
                .as_ref()
                .and_then(octopus_ledger::MovementMetadata::invocation)
                .and_then(octopus_ledger::InvocationMetadata::runtime);
            if movement.kind != MovementKind::InterfaceArrival
                || movement.from != Endpoint::Interface(ctx.interface.clone())
                || !runtime.is_some_and(|runtime| movement.to == Endpoint::Interface(runtime.worker.clone()))
                || !runtime.is_some_and(|runtime| {
                    &runtime.run == run && &runtime.interface == &ctx.interface
                })
                || !artifact_ref_has_label(artifact, PROVIDER_TRANSPORT_DISPATCH_LABEL)
            {
                return Ok(None::<()>);
            }
            if !seen.insert((artifact.id.as_str().to_string(), artifact.content.to_hex())) {
                return Err(format!(
                    "provider transport dispatch artifact '{}' has duplicate authoritative admissions",
                    artifact.id.as_str()
                ));
            }
            admissions.push(DurableArtifactAdmission {
                artifact: artifact.clone(),
                movement_seq: movement.seq.0,
            });
            Ok(None::<()>)
        },
    )?;
    ensure_arbitration_snapshot_length(ctx, &file, snapshot_len)?;
    Ok(ProviderTransportDispatchAdmissionScan {
        admissions,
        complete_boundary: scan.complete_boundary,
    })
}

pub(crate) fn interrupted_effect_journal_path(ctx: &InterfaceLiftContext, run: &RunId) -> PathBuf {
    let key = ContentHash::of_bytes(run.as_str().as_bytes()).to_hex();
    ctx.interface_var_dir()
        .join("interrupted-effect-journal")
        .join(format!("{key}.json"))
}

pub(crate) fn persist_json_file(
    path: &Path,
    value: &impl serde::Serialize,
    label: &str,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{label} path '{}' has no parent", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "{label} directory '{}' cannot be created: {error}",
            parent.display()
        )
    })?;
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("{label} could not be encoded: {error}"))?;
    bytes.push(b'\n');
    let temporary = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&temporary).map_err(|error| {
            format!(
                "{label} temporary file '{}' cannot be created: {error}",
                temporary.display()
            )
        })?;
        std::io::Write::write_all(&mut file, &bytes).map_err(|error| {
            format!(
                "{label} temporary file '{}' cannot be written: {error}",
                temporary.display()
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "{label} temporary file '{}' cannot be synced: {error}",
                temporary.display()
            )
        })?;
    }
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("{label} '{}' cannot be installed: {error}", path.display()))?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "{label} directory '{}' cannot be synced: {error}",
                parent.display()
            )
        })
}
