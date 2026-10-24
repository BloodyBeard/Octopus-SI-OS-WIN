use super::*;

pub(crate) fn latest_provider_session_carrier(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    session_snapshot_labels: &BTreeSet<String>,
    stop_before_seq: Option<u64>,
) -> Result<ProviderSessionCarrierScan, String> {
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
    let mut latest = None;
    let mut next_seq = source_movement_seq;
    let scan = scan_authoritative_snapshot(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        start_offset,
        expected_seq,
        stop_before_seq,
        |movement| {
            next_seq = movement
                .seq
                .0
                .checked_add(1)
                .ok_or_else(|| "deferred provider ledger sequence overflowed".to_string())?;
            if movement.seq.0 < source_movement_seq {
                return Ok(None::<()>);
            }
            if movement.seq.0 == source_movement_seq {
                if !movement_is_provider_source(ctx, run, &movement) {
                    return Err(format!(
                        "deferred provider source movement {source_movement_seq} differs from interface '{}' run '{}'",
                        ctx.interface.as_str(),
                        run.as_str()
                    ));
                }
                source_verified = true;
                return Ok(None::<()>);
            }
            if !source_verified {
                return Err(format!(
                    "deferred provider scan advanced past causal source {source_movement_seq}"
                ));
            }
            if movement.kind == MovementKind::InterfaceArrival
                && movement.from == Endpoint::Interface(ctx.interface.clone())
                && movement.runtime().is_some_and(|runtime| {
                    movement.to == Endpoint::Interface(runtime.worker.clone())
                        && runtime.interface == ctx.interface
                })
                && movement
                    .invocation_metadata
                    .as_ref()
                    .and_then(octopus_ledger::MovementMetadata::invocation)
                    .and_then(octopus_ledger::InvocationMetadata::runtime)
                    .is_some_and(|runtime| &runtime.run == run)
                && let Some(artifact) = movement.artifact.as_ref()
                && session_snapshot_labels
                    .iter()
                    .any(|label| artifact_ref_has_label(artifact, label))
                && read_pinned_provider_artifact_payload(ctx, artifact, "provider session carrier")
                    .ok()
                    .and_then(|payload| payload_carries_session_context(&payload).ok())
                    .unwrap_or(true)
            {
                latest = Some(DurableArtifactAdmission {
                    artifact: artifact.clone(),
                    movement_seq: movement.seq.0,
                });
            }
            Ok(None::<()>)
        },
    )?;
    ensure_arbitration_snapshot_length(ctx, &file, snapshot_len)?;
    if !source_verified {
        return Err(format!(
            "deferred provider source movement {source_movement_seq} is absent from the authoritative ledger"
        ));
    }
    Ok(ProviderSessionCarrierScan {
        latest: scan.complete_boundary.then_some(latest).flatten(),
        complete_boundary: scan.complete_boundary,
        next_seq,
    })
}

pub(crate) fn terminal_deferred_turn_ownership(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    envelope: &InterfaceInvocationEnvelope,
    source_movement_seq: u64,
) -> Result<Option<(String, String)>, String> {
    let Some(control) =
        decode_admitted_turn_control(ctx, descriptor, envelope).map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    if !matches!(&control, CodexTurnControl::Steer { .. } | CodexTurnControl::Inject { .. }) {
        return Ok(None);
    }
    let control_id = control_id(&control).to_string();
    let action = control_action(&control).to_string();
    let Some(journal) = load_control_journal(ctx, source_movement_seq, run, &control_id, &action)?
    else {
        return Ok(None);
    };
    match journal.phase {
        TurnControlJournalPhase::DeferredTurn {
            result,
            cause: CodexDeferredTurnCause::TerminalTurnMoved,
        } if !result.acknowledged => Ok(Some((control_id, action))),
        _ => Ok(None),
    }
}

pub(crate) fn validate_original_envelope_release(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    run: &RunId,
    turn: &QueuedTurn,
    expected_control_id: &str,
    expected_action: &str,
    observed_ledger_next_seq: u64,
) -> Result<(), String> {
    let ownership = terminal_deferred_turn_ownership(ctx, descriptor, run, &turn.envelope, turn.seq)?;
    if ownership
        .as_ref()
        .map(|(control_id, action)| (control_id.as_str(), action.as_str()))
        != Some((expected_control_id, expected_action))
    {
        return Err(format!(
            "deferred provider turn {} original-envelope release lost terminal control ownership",
            turn.seq
        ));
    }
    if observed_ledger_next_seq <= turn.seq {
        return Err(format!(
            "deferred provider turn {} original-envelope release names an invalid ledger boundary {}",
            turn.seq, observed_ledger_next_seq
        ));
    }
    let labels = codex_session_snapshot_labels(descriptor).map_err(|error| error.to_string())?;
    // The durable release linearized on this exact prefix. A later causal suffix may complete the
    // released source, but cannot retroactively turn its original carrier-free decision invalid.
    let scan = latest_provider_session_carrier(
        ctx,
        run,
        turn.seq,
        &labels,
        Some(observed_ledger_next_seq),
    )?;
    if !scan.complete_boundary || scan.latest.is_some() || scan.next_seq != observed_ledger_next_seq
    {
        return Err(format!(
            "deferred provider turn {} original-envelope release lacks a complete carrier-free terminal ledger boundary",
            turn.seq
        ));
    }
    Ok(())
}

pub(crate) fn read_deferred_artifact_payload(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
) -> Result<Vec<u8>, String> {
    read_pinned_provider_artifact_payload(ctx, artifact, "deferred session carrier")
}

pub(crate) fn read_pinned_provider_artifact_payload(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
    boundary: &str,
) -> Result<Vec<u8>, String> {
    read_pinned_provider_artifact(ctx, artifact, boundary).map(|(_, payload)| payload)
}

pub(crate) fn read_pinned_provider_artifact(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
    boundary: &str,
) -> Result<(octopus_interfaces::InterfaceArtifactManifest, Vec<u8>), String> {
    let limits = ctx.required_host_io_limits()?;
    let manifest_path = ctx.blobs_dir.join(artifact.content.to_hex());
    let captured = octopus_interfaces::InterfaceArtifactFile::new(&manifest_path)
        .map_err(|error| error.to_string())?
        .read_manifest_bounded(limits.max_manifest_bytes)
    .map_err(|error| {
        format!(
            "{boundary} '{}' cannot be read: {error}",
            manifest_path.display()
        )
    })?;
    if ContentHash::of_bytes(&captured.manifest_bytes) != artifact.content {
        return Err(format!(
            "{boundary} manifest hash differs from its artifact ref"
        ));
    }
    let manifest = captured.manifest;
    if artifact.id.as_str() != format!("{}:{}", manifest.label, artifact.content.to_hex()) {
        return Err(format!("{boundary} id differs from its manifest identity"));
    }
    let payload_hash = manifest
        .payload_content_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or_else(|| format!("{boundary} manifest has no payload hash"))?;
    let payload_path = ctx.blobs_dir.join(payload_hash.to_hex());
    let payload = octopus_interfaces::read_artifact_carrier_file_bounded(
        &payload_path,
        limits.max_payload_bytes,
    )
    .map_err(|error| {
        format!(
            "{boundary} payload '{}' cannot be read: {error}",
            payload_path.display()
        )
    })?;
    if ContentHash::of_bytes(&payload) != payload_hash {
        return Err(format!("{boundary} payload hash differs from its manifest"));
    }
    Ok((manifest, payload))
}

pub(crate) fn validate_artifact_admission(
    ctx: &InterfaceLiftContext,
    run: &RunId,
    source_movement_seq: u64,
    admission: &DurableArtifactAdmission,
) -> Result<(), String> {
    if admission.movement_seq <= source_movement_seq {
        return Err(format!(
            "artifact '{}' admission movement {} does not follow causal source {}",
            admission.artifact.id, admission.movement_seq, source_movement_seq
        ));
    }
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
                return Ok(None);
            }
            if movement.seq.0 == source_movement_seq {
                if !movement_is_provider_source(ctx, run, &movement) {
                    return Err(format!(
                        "provider arbitration source movement {} does not match interface '{}' run '{}'",
                        source_movement_seq,
                        ctx.interface.as_str(),
                        run.as_str()
                    ));
                }
                source_verified = true;
                return Ok(None);
            }
            if !source_verified {
                return Err(format!(
                    "provider arbitration admission {} was reached before causal source {}",
                    admission.movement_seq, source_movement_seq
                ));
            }
            Ok((movement.seq.0 == admission.movement_seq).then_some(movement))
        },
    )?;
    ensure_arbitration_snapshot_length(ctx, &file, snapshot_len)?;
    let Some(movement) = scan.found else {
        return Err(format!(
            "artifact '{}' journaled admission movement {} is absent at a {} ledger boundary",
            admission.artifact.id,
            admission.movement_seq,
            if scan.complete_boundary {
                "complete"
            } else {
                "partial"
            }
        ));
    };
    if movement_is_artifact_admission(ctx, run, &admission.artifact, &movement) {
        Ok(())
    } else {
        Err(format!(
            "artifact '{}' journaled admission movement {} does not match the authoritative provider admission",
            admission.artifact.id, admission.movement_seq
        ))
    }
}
