use super::*;

pub(crate) struct TurnThread {
    pub(crate) thread_id: String,
    pub(crate) cold: bool,
    pub(crate) first_turn_of_run: bool,
    pub(crate) retirement: Option<NativeHistoryRetirement>,
}

pub(crate) struct SettledHistory<'a, 's> {
    pub(crate) session: &'a [SessionContextArtifact],
    pub(crate) sink: &'a TurnSink<'s>,
    pub(crate) source_artifact: &'a ArtifactRef,
    pub(crate) source_movement_seq: u64,
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn acquire_turn_thread(
        &self,
        scope: &TransportStateScope,
        acquisition: ThreadAcquisition<'_>,
        settled_history: Option<SettledHistory<'_, '_>>,
    ) -> Result<TurnThread, InterfaceDriverError> {
        let recovered = acquisition.recovered;
        let (thread_id, cold, first_turn_of_run) = self
            .acquire_transport_thread(scope, acquisition.clone())
            .await?;
        let mut thread = TurnThread {
            thread_id,
            cold,
            first_turn_of_run,
            retirement: None,
        };
        let Some(settled) = settled_history else {
            return Ok(thread);
        };
        let Some(proof) = self
            .settled_native_history_proof(
                scope,
                &thread.thread_id,
                recovered,
                settled.session,
                settled.sink,
            )
            .await?
        else {
            return Ok(thread);
        };
        let source = recovered.ok_or_else(|| {
            InterfaceDriverError::FailedClosed("native history retirement lost its source".into())
        })?;
        let (replacement, cold, first) = self
            .acquire_transport_thread(
                scope,
                ThreadAcquisition {
                    recovered: Some(source),
                    retired_history: Some(&proof),
                    require_exact_resume: false,
                    ..acquisition
                },
            )
            .await?;
        if replacement == source.thread_id || !cold {
            return Err(InterfaceDriverError::FailedClosed(
                "native history retirement did not create an independent replacement thread".into(),
            ));
        }
        thread.retirement = Some(NativeHistoryRetirement {
            source_thread: source.thread_id.clone(),
            source_generation: source.context_generation,
            replacement_thread: replacement.clone(),
            source_artifact: settled.source_artifact.clone(),
            source_movement_seq: settled.source_movement_seq,
            proof,
        });
        thread.thread_id = replacement;
        thread.cold = cold;
        thread.first_turn_of_run |= first;
        Ok(thread)
    }
}
