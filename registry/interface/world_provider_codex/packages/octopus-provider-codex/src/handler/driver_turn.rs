use super::*;

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn drain_turn(
        &self,
        context: TurnContext<'_, '_>,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<TurnDrainOutcome, InterfaceDriverError> {
        let sink = context.sink;
        sink.bind_live_stream(thread_id, turn_id)?;
        let control = context.control;
        let mut drain = TurnDrain::default();
        let provider_account_events_enabled = self.provider_operations()?.is_some();
        let mut client = self.client.lock().await;
        loop {
            let event = match drain.flush_deadline {
                Some(deadline) => tokio::select! {
                    event = client.next_event() => Some(event),
                    _ = tokio::time::sleep_until(deadline) => None,
                },
                None => Some(client.next_event().await),
            };
            let Some(event) = event else {
                flush_pending_delta(context.sink, &mut drain.pending_delta, &mut drain.delta_seq)?;
                drain.flush_deadline = None;
                continue;
            };
            let step = match event {
                Some(AppServerEvent::ServerNotification(notification)) => {
                    self.drain_notification(context, thread_id, turn_id, provider_account_events_enabled, &mut drain, notification).await?
                }
                Some(AppServerEvent::ServerRequest(request)) => {
                    self.drain_server_request(context, thread_id, turn_id, &mut drain, &mut client, request).await?
                }
                Some(AppServerEvent::UnknownServerNotification(notification)) => {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "Codex emitted unknown app-server notification '{}'; provider protocol drift cannot be dropped silently",
                        notification.method
                    )));
                }
                Some(AppServerEvent::Disconnected { message }) => {
                    if let Some(error) = drain.terminal_error.take() {
                        sink.set_pending(None);
                        control.finish();
                        sink.send_terminal(error)?;
                        return Ok(TurnDrainOutcome::Complete);
                    }
                    return Err(InterfaceDriverError::Transport(format!(
                        "app-server disconnected mid-turn: {message}"
                    )));
                }
                Some(AppServerEvent::Lagged { skipped }) => {
                    return Err(InterfaceDriverError::FailedClosed(format!(
                        "app-server event stream lagged ({skipped} skipped); turn integrity lost"
                    )));
                }
                None => {
                    if let Some(error) = drain.terminal_error.take() {
                        sink.set_pending(None);
                        control.finish();
                        sink.send_terminal(error)?;
                        return Ok(TurnDrainOutcome::Complete);
                    }
                    return Err(InterfaceDriverError::Transport(
                        "app-server event stream closed before TurnCompleted".into(),
                    ));
                }
            };
            if let DrainStep::Done(outcome) = step {
                return Ok(outcome);
            }
        }
    }
}
