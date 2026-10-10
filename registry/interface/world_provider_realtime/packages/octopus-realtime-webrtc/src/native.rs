use std::fmt::Display;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use libwebrtc::peer_connection::OfferOptions;
use libwebrtc::peer_connection::PeerConnection;
use libwebrtc::peer_connection::PeerConnectionState;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::peer_connection_factory::PeerConnectionFactory;
use libwebrtc::peer_connection_factory::RtcConfiguration;
use libwebrtc::rtp_transceiver::RtpTransceiverDirection;
use libwebrtc::rtp_transceiver::RtpTransceiverInit;
use libwebrtc::session_description::SdpType;
use libwebrtc::session_description::SessionDescription;
use libwebrtc::stats::RtcStats;
use libwebrtc::MediaType;

use crate::RealtimeWebrtcError;
use crate::RealtimeWebrtcEvent;
use crate::Result;

const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(31);

enum Command {
    ApplyAnswer {
        answer_sdp: String,
        reply: mpsc::Sender<Result<()>>,
    },
    Close,
}

pub(crate) struct StartedSession {
    pub(crate) offer_sdp: String,
    pub(crate) handle: SessionHandle,
    pub(crate) events: mpsc::Receiver<RealtimeWebrtcEvent>,
}

pub(crate) struct SessionHandle {
    command_tx: mpsc::Sender<Command>,
    close: Arc<tokio::sync::Notify>,
}

impl SessionHandle {
    pub(crate) fn apply_answer_sdp(&self, answer_sdp: String) -> Result<()> {
        let (reply, reply_rx) = mpsc::channel();
        self.command_tx
            .send(Command::ApplyAnswer { answer_sdp, reply })
            .map_err(|_| RealtimeWebrtcError::Message("realtime WebRTC worker stopped".into()))?;
        match reply_rx.recv_timeout(RESPONSE_TIMEOUT) {
            Ok(result) => result,
            Err(error) => {
                self.close();
                Err(response_error(error))
            }
        }
    }

    pub(crate) fn close(&self) {
        self.close.notify_one();
        let _ = self.command_tx.send(Command::Close);
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.close();
    }
}

struct OwnedPeerConnection(PeerConnection);

impl Drop for OwnedPeerConnection {
    fn drop(&mut self) {
        self.0.close();
    }
}

fn response_error(error: mpsc::RecvTimeoutError) -> RealtimeWebrtcError {
    RealtimeWebrtcError::Message(match error {
        mpsc::RecvTimeoutError::Timeout => "realtime WebRTC worker response timed out".into(),
        mpsc::RecvTimeoutError::Disconnected => "realtime WebRTC worker stopped".into(),
    })
}

pub(crate) fn start() -> Result<StartedSession> {
    let (command_tx, command_rx) = mpsc::channel();
    let (events_tx, events_rx) = mpsc::channel();
    let (offer_tx, offer_rx) = mpsc::channel();
    let close = Arc::new(tokio::sync::Notify::new());
    let worker_close = close.clone();

    thread::Builder::new()
        .name("octopus-realtime-webrtc".to_string())
        .spawn(move || worker_main(command_rx, events_tx, offer_tx, worker_close))
        .map_err(|err| {
            RealtimeWebrtcError::Message(format!("failed to spawn realtime WebRTC worker: {err}"))
        })?;

    let offer_sdp = match offer_rx.recv_timeout(RESPONSE_TIMEOUT) {
        Ok(result) => result?,
        Err(error) => {
            close.notify_one();
            return Err(response_error(error));
        }
    };

    Ok(StartedSession {
        offer_sdp,
        handle: SessionHandle { command_tx, close },
        events: events_rx,
    })
}

fn worker_main(
    command_rx: mpsc::Receiver<Command>,
    events_tx: mpsc::Sender<RealtimeWebrtcEvent>,
    offer_tx: mpsc::Sender<Result<String>>,
    close: Arc<tokio::sync::Notify>,
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("octopus-realtime-webrtc-tokio")
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            let message = format!("failed to start realtime WebRTC runtime: {err}");
            let _ = offer_tx.send(Err(RealtimeWebrtcError::Message(message.clone())));
            let _ = events_tx.send(RealtimeWebrtcEvent::Failed(message));
            return;
        }
    };

    let peer_connection = match runtime.block_on(async {
        tokio::select! {
            biased;
            _ = close.notified() => Err(RealtimeWebrtcError::Message("realtime WebRTC setup cancelled".into())),
            result = tokio::time::timeout(OPERATION_TIMEOUT, create_peer_connection_and_offer(events_tx.clone())) => {
                result.map_err(|_| RealtimeWebrtcError::Message("realtime WebRTC setup timed out".into()))?
            }
        }
    }) {
        Ok((peer_connection, offer_sdp)) => {
            if offer_tx.send(Ok(offer_sdp)).is_err() {
                return;
            }
            peer_connection
        }
        Err(err) => {
            let message = err.to_string();
            let _ = offer_tx.send(Err(err));
            let _ = events_tx.send(RealtimeWebrtcEvent::Failed(message));
            return;
        }
    };

    let mut audio_levels_started = false;
    for command in command_rx {
        match command {
            Command::ApplyAnswer { answer_sdp, reply } => {
                let result = runtime.block_on(async {
                    tokio::select! {
                        biased;
                        _ = close.notified() => Err(RealtimeWebrtcError::Message("realtime WebRTC answer cancelled".into())),
                        result = tokio::time::timeout(OPERATION_TIMEOUT, apply_answer(&peer_connection.0, answer_sdp)) => {
                            result.map_err(|_| RealtimeWebrtcError::Message("realtime WebRTC answer timed out".into()))?
                        }
                    }
                });
                let failed = result.is_err();
                if let Err(error) = &result {
                    let _ = events_tx.send(RealtimeWebrtcEvent::Failed(error.to_string()));
                }
                if result.is_ok() && !audio_levels_started {
                    audio_levels_started = true;
                    start_local_audio_level_task(
                        &runtime,
                        peer_connection.0.clone(),
                        events_tx.clone(),
                    );
                }
                if reply.send(result).is_err() || failed {
                    drop(peer_connection);
                    let _ = events_tx.send(RealtimeWebrtcEvent::Closed);
                    return;
                }
            }
            Command::Close => {
                drop(peer_connection);
                let _ = events_tx.send(RealtimeWebrtcEvent::Closed);
                return;
            }
        }
    }

    drop(peer_connection);
    let _ = events_tx.send(RealtimeWebrtcEvent::Closed);
}

fn connection_event(state: PeerConnectionState) -> Option<RealtimeWebrtcEvent> {
    match state {
        PeerConnectionState::Connected => Some(RealtimeWebrtcEvent::Connected),
        PeerConnectionState::Failed => Some(RealtimeWebrtcEvent::Failed(
            "realtime WebRTC peer connection failed".into(),
        )),
        _ => None,
    }
}

async fn create_peer_connection_and_offer(
    events_tx: mpsc::Sender<RealtimeWebrtcEvent>,
) -> Result<(OwnedPeerConnection, String)> {
    let factory = PeerConnectionFactory::with_platform_adm();
    let peer_connection = OwnedPeerConnection(
        factory
            .create_peer_connection(RtcConfiguration::default())
            .map_err(|err| message_error("failed to create WebRTC peer connection", err))?,
    );
    peer_connection
        .0
        .on_connection_state_change(Some(Box::new(move |state| {
            if let Some(event) = connection_event(state) {
                let _ = events_tx.send(event);
            }
        })));

    let audio_transceiver = peer_connection
        .0
        .add_transceiver_for_media(
            MediaType::Audio,
            RtpTransceiverInit {
                direction: RtpTransceiverDirection::SendRecv,
                stream_ids: vec!["realtime".to_string()],
                send_encodings: Vec::new(),
            },
        )
        .map_err(|err| message_error("failed to add audio transceiver", err))?;
    let local_audio_source = factory.create_audio_source();
    let local_audio_track = factory.create_audio_track("realtime-mic", local_audio_source);
    audio_transceiver
        .sender()
        .set_track(Some(local_audio_track.into()))
        .map_err(|err| message_error("failed to attach WebRTC audio track", err))?;

    let offer = peer_connection
        .0
        .create_offer(OfferOptions {
            ice_restart: false,
            offer_to_receive_audio: true,
            offer_to_receive_video: false,
        })
        .await
        .map_err(|err| message_error("failed to create WebRTC offer", err))?;

    peer_connection
        .0
        .set_local_description(offer.clone())
        .await
        .map_err(|err| message_error("failed to set local WebRTC description", err))?;

    Ok((peer_connection, offer.to_string()))
}

async fn apply_answer(peer_connection: &PeerConnection, answer_sdp: String) -> Result<()> {
    let answer = SessionDescription::parse(&answer_sdp, SdpType::Answer)
        .map_err(|err| message_error("failed to parse WebRTC answer SDP", err))?;
    peer_connection
        .set_remote_description(answer)
        .await
        .map_err(|err| message_error("failed to set remote WebRTC description", err))?;
    Ok(())
}

fn message_error(prefix: &str, err: impl Display) -> RealtimeWebrtcError {
    RealtimeWebrtcError::Message(format!("{prefix}: {err}"))
}

fn start_local_audio_level_task(
    runtime: &tokio::runtime::Runtime,
    peer_connection: PeerConnection,
    events_tx: mpsc::Sender<RealtimeWebrtcEvent>,
) {
    runtime.spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));

        loop {
            interval.tick().await;
            if matches!(
                peer_connection.connection_state(),
                libwebrtc::peer_connection::PeerConnectionState::Closed
                    | libwebrtc::peer_connection::PeerConnectionState::Failed
            ) {
                return;
            }

            if let Some(peak) = local_audio_level(&peer_connection).await {
                let _ = events_tx.send(RealtimeWebrtcEvent::LocalAudioLevel(peak));
            }
        }
    });
}

async fn local_audio_level(peer_connection: &PeerConnection) -> Option<u16> {
    let stats = peer_connection.get_stats().await.ok()?;
    stats.into_iter().find_map(|stat| match stat {
        RtcStats::MediaSource(stats) if stats.source.kind == "audio" => {
            Some(audio_level_to_peak(stats.audio.audio_level))
        }
        _ => None,
    })
}

fn audio_level_to_peak(audio_level: f64) -> u16 {
    (audio_level.clamp(0.0, 1.0) * i16::MAX as f64).round() as u16
}

#[cfg(test)]
#[path = "../tests/support/native_lifecycle.rs"]
mod tests;
